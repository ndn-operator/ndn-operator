use std::sync::Arc;

use crate::{cert_controller::Certificate, Error, Result};
use k8s_openapi::api::core::v1::Pod;
use kube::{
    api::{DeleteParams, Patch, PatchParams},
    runtime::controller::Action,
    ResourceExt,
};
use tracing::*;

use super::{Context, DS_LABEL_KEY, Network, Router};

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
        .map_err(Error::KubeError)?;
    let node_name = pod
        .as_ref()
        .spec
        .as_ref()
        .and_then(|spec| spec.node_name.clone())
        .ok_or(Error::MissingAnnotation("node_name".to_string()))?;
    let router_name = pod.name_any().clone();
    let pp = PatchParams::apply(POD_SYNC_MANAGER_NAME);
    let router_cert = match &nw.spec.router_cert_issuer {
        Some(cert_issuer) => {
            let certificate_name = router_name.clone();
            info!("Creating certificate {} for router {} on node {}", certificate_name, router_name, node_name);
            let certificate_data = nw.create_owned_certificate(&certificate_name, &router_name, cert_issuer)?;
            let api_cert = kube::Api::<Certificate>::namespaced(client.clone(), &ns);
            let certificate = api_cert
                .patch(&certificate_name, &pp, &Patch::Apply(certificate_data))
                .await
                .map_err(Error::KubeError)?;
            Some(certificate)
            }
        None => None,
    };
    info!("Creating router for pod {} on node {}", pod.name_any(), node_name);
    let router_data = nw.create_owned_router(&router_name, &node_name, router_cert)?;
    let _ = api_rt
        .patch(&router_name, &pp, &Patch::Apply(router_data))
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
