use std::sync::Arc;

use crate::{controller::RouterStatus, Error, Result};
use k8s_openapi::api::core::v1::Pod;
use kube::{
    api::{DeleteParams, Patch, PatchParams},
    runtime::controller::Action,
    ResourceExt,
};
use serde_json::json;
use tracing::*;

use super::{create_owned_router, Context, DS_LABEL_KEY, Network, Router};

pub static POD_FINALIZER: &str = "pod.named-data.net/finalizer";
pub static POD_SYNC_MANAGER_NAME: &str = "pod-sync";

pub async fn pod_apply(pod: Arc<Pod>, ctx: Context) -> Result<Action> {
    // Create a router for the pod
    let client = ctx.client.clone();
    let ns = pod.namespace().unwrap();
    let nw_name = pod
        .labels()
        .get(DS_LABEL_KEY)
        .ok_or(Error::MissingLabel(DS_LABEL_KEY.to_string()))?;
    let api_nw = kube::Api::<Network>::namespaced(client.clone(), &ns);
    let api_rt = kube::Api::<Router>::namespaced(client.clone(), &ns);
    let nw = api_nw
        .get(nw_name)
        .await
        .map_err(|e| Error::KubeError(e))?;
    let node_name = pod
        .as_ref()
        .spec
        .as_ref()
        .and_then(|spec| spec.node_name.clone())
        .ok_or(Error::MissingAnnotation("node_name".to_string()))?;
    let router_name = pod.name_any().clone();
    info!("Creating router for pod {} on node {}", pod.name_any(), node_name);
    let router_data = create_owned_router(&nw, &router_name, &node_name);
    let pp = PatchParams::apply(POD_SYNC_MANAGER_NAME);
    let _ = api_rt
      .patch(&router_name, &pp, &Patch::Apply(router_data))
      .await
      .map_err(Error::KubeError)?;

    // Add status.created to the router
    let patch_status = json!({
        "status": RouterStatus {
            created: Some(true),
            ..RouterStatus::default()
        }
    });
    debug!("Patch status: {:?}", patch_status);
    let _ = api_rt
      .patch_status(&router_name, &pp, &Patch::Strategic(patch_status))
      .await
      .map_err(Error::KubeError)?;

    Ok(Action::await_change())
}

pub async fn pod_cleanup(pod: Arc<Pod>, ctx: Context) -> Result<Action> {
    // Delete the router for the pod
    let client = ctx.client.clone();
    let ns = pod.namespace().unwrap();
    let api_rt = kube::Api::<Router>::namespaced(client.clone(), &ns);
    let pod_name = pod.name_any();
    let router_name = pod_name.clone();
    let dp = DeleteParams::default();
    info!("Deleting router for pod {}", pod_name);
    let _ = api_rt
      .delete(&router_name, &dp)
      .await
      .map_err(Error::KubeError);

    Ok(Action::await_change())
}