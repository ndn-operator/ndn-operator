use std::sync::Arc;

use crate::{Error, Result, events_helper::emit_info};
use json_patch::{
    AddOperation, Patch as JsonPatch, PatchOperation, RemoveOperation, jsonptr::PointerBuf,
};
use kube::{
    Api, CustomResource, ResourceExt,
    api::{ListParams, Patch, PatchParams},
    core::Expression,
    runtime::{
        controller::Action,
        finalizer::{Event as Finalizer, finalizer},
    },
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::*;

use super::{Context, NETWORK_LABEL_KEY, ROUTER_MANAGER_NAME, Router};

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[kube(
    group = "named-data.net",
    version = "v1alpha1",
    kind = "NeighborLink",
    derive = "Default",
    namespaced,
    shortname = "nl",
    doc = "NeighborLink references a public face of an external NDN network"
)]
pub struct NeighborLinkSpec {
    /// Name of the local Network CR this link applies to
    pub network: String,
    /// Public URI of the neighbor network (e.g., tcp://host:port)
    pub uri: String,
}

impl super::main::Context {
    fn router_api_in_ns(&self, ns: &str) -> Api<Router> {
        Api::<Router>::namespaced(self.client.clone(), ns)
    }
}

impl NeighborLink {
    pub async fn reconcile(self: Arc<Self>, ctx: Arc<Context>) -> Result<Action> {
        let ns = self.namespace().unwrap();
        let api_router = ctx.router_api_in_ns(&ns);
        let serverside = PatchParams::apply(ROUTER_MANAGER_NAME);
        let network_name = &self.spec.network;
        // List all routers belonging to this network in the same namespace
        let lp = ListParams::default()
            .labels_from(&Expression::Equal(NETWORK_LABEL_KEY.into(), network_name.clone()).into());
        for router in api_router.list(&lp).await.map_err(Error::KubeError)?.iter() {
            let patches = vec![PatchOperation::Add(AddOperation {
                path: PointerBuf::from_tokens(vec![
                    "status",
                    "outerNeighbors",
                    self.name_any().as_str(),
                ]),
                value: serde_json::to_value(&self.spec.uri).unwrap_or(serde_json::Value::Null),
            })];
            let patch = Patch::Json::<()>(JsonPatch(patches));
            debug!(
                "NeighborLink status patch to {}: {:?}",
                router.name_any(),
                patch
            );
            let _ = api_router
                .patch_status(&router.name_any(), &serverside, &patch)
                .await
                .map_err(Error::KubeError)?;
            emit_info(
                &ctx.recorder,
                router,
                "OuterNeighborInserted",
                "Updated",
                Some(format!("From NeighborLink `{}`", self.name_any())),
            )
            .await;
        }
        Ok(Action::await_change())
    }

    pub async fn cleanup(self: Arc<Self>, ctx: Arc<Context>) -> Result<Action> {
        let ns = self.namespace().unwrap();
        let api_router = ctx.router_api_in_ns(&ns);
        let network_name = &self.spec.network;
        let lp = ListParams::default()
            .labels_from(&Expression::Equal(NETWORK_LABEL_KEY.into(), network_name.clone()).into());
        for router in api_router.list(&lp).await.map_err(Error::KubeError)?.iter() {
            // Remove only if present
            let patches = vec![PatchOperation::Remove(RemoveOperation {
                path: PointerBuf::from_tokens(vec![
                    "status",
                    "outerNeighbors",
                    self.name_any().as_str(),
                ]),
            })];
            let patch = Patch::Json::<()>(JsonPatch(patches));
            debug!(
                "NeighborLink cleanup patch to {}: {:?}",
                router.name_any(),
                patch
            );
            let serverside = PatchParams::apply(ROUTER_MANAGER_NAME);
            let _ = api_router
                .patch_status(&router.name_any(), &serverside, &patch)
                .await
                .map_err(Error::KubeError)?;
            emit_info(
                &ctx.recorder,
                router,
                "OuterNeighborRemoved",
                "Updated",
                Some(format!("From NeighborLink `{}`", self.name_any())),
            )
            .await;
        }
        Ok(Action::await_change())
    }
}

pub async fn reconcile_neighbor_link(nl: Arc<NeighborLink>, ctx: Arc<Context>) -> Result<Action> {
    let ns = nl.namespace().unwrap();
    let api_nl: Api<NeighborLink> = Api::namespaced(ctx.client.clone(), &ns);
    info!("Reconciling NeighborLink \"{}\" in {}", nl.name_any(), ns);
    finalizer(
        &api_nl,
        "neighborlink.named-data.net/finalizer",
        nl,
        move |event| async move {
            match event {
                Finalizer::Apply(nl) => nl.reconcile(ctx.clone()).await,
                Finalizer::Cleanup(nl) => nl.cleanup(ctx.clone()).await,
            }
        },
    )
    .await
    .map_err(|e| Error::FinalizerError(Box::new(e)))
}
