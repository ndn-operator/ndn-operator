use crate::{Error, Result};
use chrono::{DateTime, Utc};
use futures::StreamExt;
use std::sync::Arc;
use kube::{
    api::{Api, ListParams, Patch, PatchParams, ResourceExt},
    client::Client,
    runtime::{
        controller::{Action, Controller},
        events::{Event, EventType, Recorder, Reporter},
        finalizer::{finalizer, Event as Finalizer},
        watcher::Config,
    },
    CustomResource, Resource,
};
use serde::{Deserialize, Serialize};
use schemars::JsonSchema;
use serde_json::json;
use tokio::{sync::RwLock, time::Duration};
use tracing::*;

pub static NETWORK_FINALIZER: &str = "networks.named-data.net/finalizer";

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[kube(group = "named-data.net", version = "v1alpha1", kind = "Network", namespaced)]
#[kube(status = "NetworkStatus")]
pub struct NetworkSpec {
    prefix: String,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct NetworkStatus {
    nodes: Vec<String>,
}

// Context for our reconciler
#[derive(Clone)]
pub struct Context {
    /// Kubernetes client
    pub client: Client,
    /// Event recorder
    pub recorder: Recorder,
    /// Diagnostics read by the web server
    pub diagnostics: Arc<RwLock<Diagnostics>>,
}

async fn reconcile(network: Arc<Network>, ctx: Arc<Context>) -> Result<Action> {
    let ns = network.namespace().unwrap(); // doc is namespace scoped
    let networks: Api<Network> = Api::namespaced(ctx.client.clone(), &ns);

    info!("Reconciling Network \"{}\" in {}", network.name_any(), ns);
    finalizer(&networks, NETWORK_FINALIZER, network, |event| async {
        match event {
            Finalizer::Apply(network) => network.reconcile(ctx.clone()).await,
            Finalizer::Cleanup(network) => network.cleanup(ctx.clone()).await,
        }
    })
    .await
    .map_err(|e| Error::FinalizerError(Box::new(e)))
}

impl Network {
    async fn reconcile(&self, ctx: Arc<Context>) -> Result<Action> {
        let client = ctx.client.clone();
        let ns = self.namespace().unwrap();
        let name = self.name_any();
        let networks: Api<Network> = Api::namespaced(client, &ns);
        let new_status = Patch::Apply(json!({
            "apiVersion": "named-data.net/v1alpha1",
            "kind": "Network",
            "status": NetworkStatus {
                nodes: vec!["node1".to_string(), "node2".to_string()],
            }
        }));
        let ps = PatchParams::apply("cntrlr").force();
        let _o = networks
            .patch_status(&name, &ps, &new_status)
            .await
            .map_err(Error::KubeError)?;
        Ok(Action::requeue(Duration::from_secs(5 * 60)))
    }

    async fn cleanup(&self, ctx: Arc<Context>) -> Result<Action> {
        let oref = self.object_ref(&());
        ctx.recorder
            .publish(
                &Event {
                    type_: EventType::Normal,
                    reason: "DeleteRequested".into(),
                    note: Some(format!("Delete `{}`", self.name_any())),
                    action: "Deleting".into(),
                    secondary: None,
                },
                &oref,
            )
            .await
            .map_err(Error::KubeError)?;
        Ok(Action::await_change())
    }
}

/// Diagnostics to be exposed by the web server
#[derive(Clone, Serialize)]
pub struct Diagnostics {
    #[serde(deserialize_with = "from_ts")]
    pub last_event: DateTime<Utc>,
    #[serde(skip)]
    pub reporter: Reporter,
}
impl Default for Diagnostics {
    fn default() -> Self {
        Self {
            last_event: Utc::now(),
            reporter: "network-controller".into(),
        }
    }
}
impl Diagnostics {
    fn recorder(&self, client: Client) -> Recorder {
        Recorder::new(client, self.reporter.clone())
    }
}

/// State shared between the controller and the web server
#[derive(Clone, Default)]
pub struct State {
    /// Diagnostics populated by the reconciler
    diagnostics: Arc<RwLock<Diagnostics>>,
}

impl State {
    /// State getter
    pub async fn diagnostics(&self) -> Diagnostics {
        self.diagnostics.read().await.clone()
    }

    // Create a Controller Context that can update State
    pub async fn to_context(&self, client: Client) -> Arc<Context> {
        Arc::new(Context {
            client: client.clone(),
            recorder: self.diagnostics.read().await.recorder(client),
            diagnostics: self.diagnostics.clone(),
        })
    }
}

fn error_policy(_: Arc<Network>, error: &Error, _: Arc<Context>) -> Action {
    warn!("reconcile failed: {:?}", error);
    Action::requeue(Duration::from_secs(5 * 60))
}

pub async fn run(state: State) {
    let client = Client::try_default().await.expect("Expected a valid KUBECONFIG environment variable");
    let networks = Api::<Network>::all(client.clone());
    if let Err(e) = networks.list(&ListParams::default().limit(1)).await {
        error!("CRD is not queryable; {e:?}. Is the CRD installed?");
        info!("Installation: cargo run --bin crdgen | kubectl apply -f -");
        std::process::exit(1);
    }
    Controller::new(networks, Config::default().any_semantic())
        .shutdown_on_signal()
        .run(reconcile, error_policy, state.to_context(client).await)
        .filter_map(async |x| { std::result::Result::ok(x) })
        .for_each(|_| futures::future::ready(()))
        .await;
}