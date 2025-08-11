use chrono::{DateTime, Utc};
use futures::StreamExt;
use k8s_openapi::api::core::v1::Pod;
use kube::{
    api::{Api, ListParams, ResourceExt},
    client::Client,
    core::Expression,
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

use super::{
    DS_LABEL_KEY, NETWORK_FINALIZER, Network, ROUTER_FINALIZER, Router, pod_apply, pod_cleanup,
};
use crate::{Error, Result, network_controller::POD_FINALIZER};

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

async fn reconcile_network(network: Arc<Network>, ctx: Arc<Context>) -> Result<Action> {
    let ns = network.namespace().unwrap();
    let api_nw: Api<Network> = Api::namespaced(ctx.client.clone(), &ns);

    info!("Reconciling Network \"{}\" in {}", network.name_any(), ns);
    finalizer(
        &api_nw,
        NETWORK_FINALIZER,
        network,
        async |event| match event {
            Finalizer::Apply(network) => network.reconcile(ctx.clone()).await,
            Finalizer::Cleanup(network) => network.cleanup(ctx.clone()).await,
        },
    )
    .await
    .map_err(|e| Error::FinalizerError(Box::new(e)))
}

async fn reconcile_router(router: Arc<Router>, ctx: Arc<Context>) -> Result<Action> {
    let ns = router.namespace().unwrap();
    let api_router: Api<Router> = Api::namespaced(ctx.client.clone(), &ns);

    info!("Reconciling Router \"{}\" in {}", router.name_any(), ns);
    finalizer(
        &api_router,
        ROUTER_FINALIZER,
        router,
        async |event| match event {
            Finalizer::Apply(router) => router.reconcile(ctx.clone()).await,
            Finalizer::Cleanup(router) => router.cleanup(ctx.clone()).await,
        },
    )
    .await
    .map_err(|e| Error::FinalizerError(Box::new(e)))
}

async fn reconcile_pod(pod: Arc<Pod>, ctx: Arc<Context>) -> Result<Action> {
    let ns = pod.namespace().unwrap();
    let api_pod: Api<Pod> = Api::namespaced(ctx.client.clone(), &ns);
    info!("Reconciling Pod \"{}\" in {}", pod.name_any(), ns);
    finalizer(&api_pod, POD_FINALIZER, pod, async |event| match event {
        Finalizer::Apply(pod) => pod_apply(pod, (*ctx).clone()).await,
        Finalizer::Cleanup(pod) => pod_cleanup(pod, (*ctx).clone()).await,
    })
    .await
    .map_err(|e| Error::FinalizerError(Box::new(e)))
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

fn network_error_policy(_: Arc<Network>, error: &Error, _: Arc<Context>) -> Action {
    warn!("reconcile failed: {:?}", error);
    Action::requeue(Duration::from_secs(5 * 60))
}

fn router_error_policy(_: Arc<Router>, error: &Error, _: Arc<Context>) -> Action {
    warn!("reconcile failed: {:?}", error);
    Action::requeue(Duration::from_secs(5 * 60))
}

fn pod_error_policy(_: Arc<Pod>, error: &Error, _: Arc<Context>) -> Action {
    warn!("reconcile failed: {:?}", error);
    Action::requeue(Duration::from_secs(60))
}

pub async fn run_nw(state: State) {
    let client = Client::try_default()
        .await
        .expect("Expected a valid KUBECONFIG environment variable");
    let api_nw = Api::<Network>::all(client.clone());
    if let Err(e) = api_nw.list(&ListParams::default().limit(1)).await {
        error!("Network CRD is not queryable; {e:?}. Is the CRD installed?");
        info!("Installation: cargo run --bin crdgen | kubectl apply -f -");
        std::process::exit(1);
    }
    Controller::new(api_nw, watcher::Config::default().any_semantic())
        .shutdown_on_signal()
        .run(
            reconcile_network,
            network_error_policy,
            state.to_context(client.clone()).await,
        )
        .filter_map(async |x| std::result::Result::ok(x))
        .for_each(async |_| ())
        .await;
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

pub async fn run_pod_sync(state: State) {
    let client = Client::try_default()
        .await
        .expect("Expected a valid KUBECONFIG environment variable");
    let api_pod = Api::<Pod>::all(client.clone());
    Controller::new(
        api_pod,
        watcher::Config::default().labels_from(&Expression::Exists(DS_LABEL_KEY.into()).into()),
    )
    .shutdown_on_signal()
    .run(
        reconcile_pod,
        pod_error_policy,
        state.to_context(client.clone()).await,
    )
    .filter_map(async |x| std::result::Result::ok(x))
    .for_each(async |_| ())
    .await;
}
