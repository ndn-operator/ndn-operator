use chrono::{DateTime, Utc};
use futures::StreamExt;
use kube::{
    api::{Api, ListParams, ResourceExt},
    client::Client,
    runtime::{
        controller::{Action, Controller},
        events::{Recorder, Reporter},
        finalizer::{Event as Finalizer, finalizer},
        watcher,
    },
};
use serde::Serialize;
use std::sync::Arc;
use tokio::{sync::RwLock, time::Duration};
use tracing::*;

use super::Router;
use crate::{Error, Result};

/// Context shared between the router reconciler and helper routines
#[derive(Clone)]
pub struct Context {
    /// Kubernetes client
    pub client: Client,
    /// Event recorder
    pub recorder: Recorder,
    /// Diagnostics shared with the HTTP server
    pub diagnostics: Arc<RwLock<Diagnostics>>,
}

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
            reporter: "router-controller".into(),
        }
    }
}
impl Diagnostics {
    fn recorder(&self, client: Client) -> Recorder {
        Recorder::new(client, self.reporter.clone())
    }
}

#[derive(Clone, Default)]
pub struct State {
    diagnostics: Arc<RwLock<Diagnostics>>,
}

impl State {
    pub async fn diagnostics(&self) -> Diagnostics {
        self.diagnostics.read().await.clone()
    }

    pub async fn to_context(&self, client: Client) -> Arc<Context> {
        Arc::new(Context {
            client: client.clone(),
            recorder: self.diagnostics.read().await.recorder(client),
            diagnostics: self.diagnostics.clone(),
        })
    }
}

async fn reconcile_router(router: Arc<Router>, ctx: Arc<Context>) -> Result<Action> {
    let ns = router.namespace().unwrap();
    let api_router: Api<Router> = Api::namespaced(ctx.client.clone(), &ns);

    info!("Reconciling Router \"{}\" in {}", router.name_any(), ns);
    finalizer(
        &api_router,
        crate::router_controller::ROUTER_FINALIZER,
        router,
        async |event| match event {
            Finalizer::Apply(router) => router.reconcile(ctx.clone()).await,
            Finalizer::Cleanup(router) => router.cleanup(ctx.clone()).await,
        },
    )
    .await
    .map_err(|e| Error::FinalizerError(Box::new(e)))
}

fn router_error_policy(_: Arc<Router>, error: &Error, _: Arc<Context>) -> Action {
    warn!("reconcile failed: {:?}", error);
    Action::requeue(Duration::from_secs(5 * 60))
}

pub async fn run_router(state: State) {
    let client = Client::try_default()
        .await
        .expect("Expected a valid KUBECONFIG environment variable");
    let api_router = Api::<Router>::all(client.clone());
    if let Err(e) = api_router.list(&ListParams::default().limit(1)).await {
        error!("Router CRD is not queryable; {e:?}. Is the CRD installed?");
        info!("Installation: cargo run --bin crdgen | kubectl apply -f -");
        std::process::exit(1);
    }
    Controller::new(api_router, watcher::Config::default().any_semantic())
        .shutdown_on_signal()
        .run(
            reconcile_router,
            router_error_policy,
            state.to_context(client.clone()).await,
        )
        .filter_map(async |x| std::result::Result::ok(x))
        .for_each(async |_| ())
        .await;
}
