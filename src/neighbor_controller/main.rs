use tracing::*;

crate::controller_scaffold! {
    controller_ty: super::Neighbor,
    reporter: "neighbor-controller",
    run_fn: run_neighbor,
    reconcile_fn: super::neighbor::reconcile_neighbor,
    error_policy_fn: neighbor_error_policy,
    error_requeue_secs: 5 * 60,
    api_builder: |client: kube::Client| kube::Api::<super::Neighbor>::all(client),
    watcher_config: kube::runtime::watcher::Config::default().any_semantic(),
    preflight: |api: kube::Api<super::Neighbor>| async move {
        if let Err(e) = api.list(&kube::api::ListParams::default().limit(1)).await {
            error!("Neighbor CRD is not queryable; {e:?}. Is the CRD installed?");
            info!("Installation: cargo run --bin crdgen | kubectl apply -f -");
            std::process::exit(1);
        }
    }
}
