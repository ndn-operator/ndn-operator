use kube::{
    api::{Api, ResourceExt},
    runtime::{
        controller::Action,
        finalizer::{Event as Finalizer, finalizer},
    },
};
use std::sync::Arc;
use tracing::*;

use crate::{
    Error, Result,
    cert_controller::{EXTERNAL_CERTIFICATE_FINALIZER, ExternalCertificate},
};

crate::controller_scaffold! {
    controller_ty: crate::cert_controller::ExternalCertificate,
    reporter: "external-cert-controller",
    run_fn: run_ext_cert,
    reconcile_fn: reconcile_external_cert,
    error_policy_fn: external_certificate_error_policy,
    error_requeue_secs: 5 * 60,
    api_builder: |client: kube::Client| kube::Api::<ExternalCertificate>::all(client),
    watcher_config: kube::runtime::watcher::Config::default().any_semantic(),
    preflight: |api: kube::Api<ExternalCertificate>| async move {
        if let Err(e) = api.list(&kube::api::ListParams::default().limit(1)).await {
            error!("ExternalCertificate CRD is not queryable; {e:?}. Is the CRD installed?");
            info!("Installation: cargo run --bin crdgen | kubectl apply -f -");
            std::process::exit(1);
        }
    }
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
        EXTERNAL_CERTIFICATE_FINALIZER,
        cert,
        async |event| match event {
            Finalizer::Apply(cert) => cert.reconcile(ctx.clone()).await,
            Finalizer::Cleanup(cert) => cert.cleanup(ctx.clone()).await,
        },
    )
    .await
    .map_err(|e| Error::FinalizerError(Box::new(e)))
}
