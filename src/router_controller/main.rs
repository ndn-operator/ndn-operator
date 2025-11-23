use kube::{
    api::{Api, ResourceExt},
    runtime::{
        controller::Action,
        finalizer::{Event as Finalizer, finalizer},
    },
};
use std::sync::Arc;
use tracing::*;

use super::Router;
use crate::{Error, Result};

crate::controller_scaffold! {
    controller_ty: super::Router,
    reporter: "router-controller",
    run_fn: run_router,
    reconcile_fn: reconcile_router,
    error_policy_fn: router_error_policy,
    error_requeue_secs: 5 * 60,
    api_builder: |client: kube::Client| kube::Api::<Router>::all(client),
    watcher_config: kube::runtime::watcher::Config::default().any_semantic(),
    preflight: |api: kube::Api<Router>| async move {
        if let Err(e) = api.list(&kube::api::ListParams::default().limit(1)).await {
            error!("Router CRD is not queryable; {e:?}. Is the CRD installed?");
            info!("Installation: cargo run --bin crdgen | kubectl apply -f -");
            std::process::exit(1);
        }
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
