use crate::{Error, Result};
use crate::daemonset::*;
use chrono::{DateTime, Utc};
use futures::StreamExt;
use k8s_openapi::api::apps::v1::DaemonSet;
use k8s_openapi::api::core::v1::Pod;
use std::sync::Arc;
use kube::{
    api::{Api, ListParams, Patch, PatchParams, ResourceExt}, client::Client, runtime::{
        controller::{Action, Controller},
        events::{Event, EventType, Recorder, Reporter},
        finalizer::{finalizer, Event as Finalizer},
        watcher::Config,
    }, CustomResource, Resource
};
use serde::{Deserialize, Serialize};
use schemars::JsonSchema;
use serde_json::json;
use tokio::{sync::RwLock, time::Duration};
use tracing::*;

pub static NETWORK_FINALIZER: &str = "networks.named-data.net/finalizer";
pub static MANAGER_NAME: &str = "ndnd-controller";

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[kube(group = "named-data.net", version = "v1alpha1", kind = "Network", namespaced, shortname = "ndn")]
#[kube(status = "NetworkStatus")]
pub struct NetworkSpec {
    prefix: String,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct NetworkStatus {
    ds_created: Option<bool>,
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
        let api_nw: Api<Network> = Api::namespaced(ctx.client.clone(), &self.namespace().unwrap());
        let api_ds: Api<DaemonSet> = Api::namespaced(ctx.client.clone(), &self.namespace().unwrap());
        let serverside = PatchParams::apply(MANAGER_NAME);
        let image = get_my_image(ctx.client.clone()).await;
        let ds_data = create_owned_daemonset(&self, &image);
        let ds = api_ds.patch(&self.name_any(), &serverside, &Patch::Apply(ds_data)).await.map_err(Error::KubeError)?;
        // Publish event
        ctx.recorder
            .publish(
                &Event {
                    type_: EventType::Normal,
                    reason: "DaemonSetCreated".into(),
                    note: Some(format!("Created `{}` DaemonSet for `{}` Network", ds.name_any(), self.name_any())),
                    action: "Created".into(),
                    secondary: None,
                },
                &self.object_ref(&()),
            )
            .await
            .map_err(Error::KubeError)?;
        // Update the status of the Network
        let status = json!({
            "status": NetworkStatus {
                ds_created: Some(true),
            }
        });
        let _o = api_nw
            .patch_status(&self.name_any(), &serverside, &Patch::Merge(&status))
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

fn get_my_namespace() -> String {
    std::fs::read_to_string("/var/run/secrets/kubernetes.io/serviceaccount/namespace").unwrap()
}

fn get_my_pod_name() -> String {
    std::fs::read_to_string("/etc/hostname").unwrap().trim_end_matches('\n').to_string()
}

async fn get_my_image(client: Client) -> String {
    let pods = Api::<Pod>::namespaced(client.clone(), &get_my_namespace());
    let pod = pods.get(&get_my_pod_name()).await.expect("Failed to get my pod");
    let pod_spec = pod.spec.expect("Pod has no spec");
    let container = pod_spec.containers.first().expect("Pod has no containers");
    container.image.clone().expect("Container has no image")
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