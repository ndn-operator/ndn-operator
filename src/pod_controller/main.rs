use chrono::{DateTime, Utc};
use futures::StreamExt;
use k8s_openapi::api::core::v1::Pod;
use kube::{
    api::{Api, ResourceExt},
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

use super::{POD_FINALIZER, pod_apply, pod_cleanup};
use crate::{Error, Result, network_controller::DS_LABEL_KEY};

#[derive(Clone)]
pub struct Context {
    pub client: Client,
    pub recorder: Recorder,
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
            reporter: "pod-controller".into(),
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

fn pod_error_policy(_: Arc<Pod>, error: &Error, _: Arc<Context>) -> Action {
    warn!("reconcile failed: {:?}", error);
    Action::requeue(Duration::from_secs(60))
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
