use k8s_openapi::api::core::v1::Pod;
use kube::{
    api::{Api, ResourceExt},
    core::Expression,
    runtime::{
        controller::Action,
        finalizer::{Event as Finalizer, finalizer},
    },
};
use std::sync::Arc;
use tracing::*;

use super::{POD_FINALIZER, pod_apply, pod_cleanup};
use crate::{Error, Result, network_controller::DS_LABEL_KEY};

crate::controller_scaffold! {
    controller_ty: Pod,
    reporter: "pod-controller",
    run_fn: run_pod_sync,
    reconcile_fn: reconcile_pod,
    error_policy_fn: pod_error_policy,
    error_requeue_secs: 60,
    api_builder: |client: kube::Client| kube::Api::<Pod>::all(client),
    watcher_config: {
        kube::runtime::watcher::Config::default()
            .labels_from(&Expression::Exists(DS_LABEL_KEY.into()).into())
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
