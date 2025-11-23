use chrono::{DateTime, Utc};
use futures::StreamExt;
use kube::{
    api::{Api, ListParams},
    client::Client,
    runtime::{
        controller::{Action, Controller},
        events::{Recorder, Reporter},
        watcher,
    },
};
use serde::Serialize;
use std::sync::Arc;
use tokio::{sync::RwLock, time::Duration};
use tracing::*;

use super::NeighborLink;
use crate::Error;

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
            reporter: "neighbor-link-controller".into(),
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

fn neighbor_link_error_policy(_: Arc<NeighborLink>, error: &Error, _: Arc<Context>) -> Action {
    warn!("reconcile failed: {:?}", error);
    Action::requeue(Duration::from_secs(5 * 60))
}

pub async fn run_neighbor_link(state: State) {
    let client = Client::try_default()
        .await
        .expect("Expected a valid KUBECONFIG environment variable");
    let api_nl = Api::<NeighborLink>::all(client.clone());
    if let Err(e) = api_nl.list(&ListParams::default().limit(1)).await {
        error!("NeighborLink CRD is not queryable; {e:?}. Is the CRD installed?");
        info!("Installation: cargo run --bin crdgen | kubectl apply -f -");
        std::process::exit(1);
    }
    Controller::new(api_nl, watcher::Config::default().any_semantic())
        .shutdown_on_signal()
        .run(
            super::neighbor_link::reconcile_neighbor_link,
            neighbor_link_error_policy,
            state.to_context(client.clone()).await,
        )
        .filter_map(async |x| std::result::Result::ok(x))
        .for_each(async |_| ())
        .await;
}
