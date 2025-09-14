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

use super::{Certificate, ExternalCertificate};
use crate::{Error, Result, cert_controller::CERTIFICATE_FINALIZER};

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

async fn reconcile_cert(cert: Arc<Certificate>, ctx: Arc<Context>) -> Result<Action> {
    let ns = cert.namespace().unwrap();
    let api_cert: Api<Certificate> = Api::namespaced(ctx.client.clone(), &ns);

    info!("Reconciling Certificate \"{}\" in {}", cert.name_any(), ns);
    finalizer(
        &api_cert,
        CERTIFICATE_FINALIZER,
        cert,
        async |event| match event {
            Finalizer::Apply(cert) => cert.reconcile(ctx.clone()).await,
            Finalizer::Cleanup(cert) => cert.cleanup(ctx.clone()).await,
        },
    )
    .await
    .map_err(|e| Error::FinalizerError(Box::new(e)))
}

async fn reconcile_external_cert(
    cert: Arc<ExternalCertificate>,
    ctx: Arc<Context>,
) -> Result<Action> {
    let ns = cert.namespace().unwrap();
    let api_cert: Api<ExternalCertificate> = Api::namespaced(ctx.client.clone(), &ns);

    info!(
        "Reconciling ExternalCertificate \"{}\" in {}",
        cert.name_any(),
        ns
    );
    finalizer(
        &api_cert,
        crate::cert_controller::EXTERNAL_CERTIFICATE_FINALIZER,
        cert,
        async |event| match event {
            Finalizer::Apply(cert) => cert.reconcile(ctx.clone()).await,
            Finalizer::Cleanup(cert) => cert.cleanup(ctx.clone()).await,
        },
    )
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
            reporter: "cert-controller".into(),
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

fn certificate_error_policy(_: Arc<Certificate>, error: &Error, _: Arc<Context>) -> Action {
    warn!("reconcile failed: {:?}", error);
    Action::requeue(Duration::from_secs(5 * 60))
}

fn external_certificate_error_policy(
    _: Arc<ExternalCertificate>,
    error: &Error,
    _: Arc<Context>,
) -> Action {
    warn!("reconcile failed: {:?}", error);
    Action::requeue(Duration::from_secs(5 * 60))
}

pub async fn run_cert(state: State) {
    let client = Client::try_default()
        .await
        .expect("Expected a valid KUBECONFIG environment variable");
    let api_cert = Api::<Certificate>::all(client.clone());
    let api_ecert = Api::<ExternalCertificate>::all(client.clone());
    if let Err(e) = api_cert.list(&ListParams::default().limit(1)).await {
        error!("Certificate CRD is not queryable; {e:?}. Is the CRD installed?");
        info!("Installation: cargo run --bin crdgen | kubectl apply -f -");
        std::process::exit(1);
    }
    if let Err(e) = api_ecert.list(&ListParams::default().limit(1)).await {
        error!("ExternalCertificate CRD is not queryable; {e:?}. Is the CRD installed?");
        info!("Installation: cargo run --bin crdgen | kubectl apply -f -");
        std::process::exit(1);
    }
    let cert_ctrl = Controller::new(api_cert, watcher::Config::default().any_semantic())
        .shutdown_on_signal()
        .run(
            reconcile_cert,
            certificate_error_policy,
            state.to_context(client.clone()).await,
        )
        .filter_map(async |x| std::result::Result::ok(x))
        .for_each(async |_| ());

    let ecert_ctrl = Controller::new(api_ecert, watcher::Config::default().any_semantic())
        .shutdown_on_signal()
        .run(
            reconcile_external_cert,
            external_certificate_error_policy,
            state.to_context(client.clone()).await,
        )
        .filter_map(async |x| std::result::Result::ok(x))
        .for_each(async |_| ());

    tokio::join!(cert_ctrl, ecert_ctrl).0;
}
