use tracing::*;

crate::controller_scaffold! {
    controller_ty: super::NeighborLink,
    reporter: "neighbor-link-controller",
    run_fn: run_neighbor_link,
    reconcile_fn: super::neighbor_link::reconcile_neighbor_link,
    error_policy_fn: neighbor_link_error_policy,
    error_requeue_secs: 5 * 60,
    api_builder: |client: kube::Client| kube::Api::<super::NeighborLink>::all(client),
    watcher_config: kube::runtime::watcher::Config::default().any_semantic(),
    preflight: |api: kube::Api<super::NeighborLink>| async move {
        if let Err(e) = api.list(&kube::api::ListParams::default().limit(1)).await {
            error!("NeighborLink CRD is not queryable; {e:?}. Is the CRD installed?");
            info!("Installation: cargo run --bin crdgen | kubectl apply -f -");
            std::process::exit(1);
        }
    }
}
