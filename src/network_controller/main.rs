use kube::{
    api::{Api, ResourceExt},
    runtime::{
        controller::Action,
        finalizer::{Event as Finalizer, finalizer},
    },
};
use std::sync::Arc;
use tracing::*;

use super::{NETWORK_FINALIZER, Network};
use crate::{Error, Result};

crate::controller_scaffold! {
    controller_ty: super::Network,
    reporter: "network-controller",
    run_fn: run_nw,
    reconcile_fn: reconcile_network,
    error_policy_fn: network_error_policy,
    error_requeue_secs: 5 * 60,
    api_builder: |client: kube::Client| kube::Api::<Network>::all(client),
    watcher_config: kube::runtime::watcher::Config::default().any_semantic(),
    preflight: |api: kube::Api<Network>| async move {
        if let Err(e) = api.list(&kube::api::ListParams::default().limit(1)).await {
            error!("Network CRD is not queryable; {e:?}. Is the CRD installed?");
            info!("Installation: cargo run --bin crdgen | kubectl apply -f -");
            std::process::exit(1);
        }
    }
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
