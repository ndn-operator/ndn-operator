use std::{
    collections::BTreeSet, sync::Arc
};

// use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::{
    api::{ListParams, Patch, PatchParams},
    core::Expression,
    runtime::{
        controller::Action,
        events::{Event, EventType}, wait::Condition,
    },
    Api, CustomResource, Resource, ResourceExt,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;
use json_patch::{jsonptr::PointerBuf, Patch as JsonPatch, PatchOperation, ReplaceOperation};
use tracing::*;

use super::{Context, NETWORK_LABEL_KEY};
use crate::{Error, Result};

pub static ROUTER_FINALIZER: &str = "router.named-data.net/finalizer";
pub static ROUTER_MANAGER_NAME: &str = "router-controller";

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[kube(
    group = "named-data.net",
    version = "v1alpha1",
    kind = "Router",
    derive="Default",
    namespaced,
    shortname = "rt",
    doc = "Router represents a Named Data Networking (NDN) router in Kubernetes",
    printcolumn = r#"{"name":"Online","jsonPath":".status.online","type":"boolean"}"#,
    printcolumn = r#"{"name":"Initialized","jsonPath":".status.initialized","type":"boolean"}"#,
    printcolumn = r#"{"name":"Prefix","jsonPath":".spec.prefix","type":"string"}"#,
    printcolumn = r#"{"name":"Node","jsonPath":".spec.nodeName","type":"string"}"#,
    printcolumn = r#"{"name":"Certificate","jsonPath":".spec.cert.name","type":"string"}"#,
    status = "RouterStatus",
)]
pub struct RouterSpec {
    /// The prefix for the router, used for routing and naming conventions.
    /// This should be a valid NDN prefix, e.g., "/example/router"
    pub prefix: String,
    /// The name of the node where the router is running
    pub node_name: String,
    /// The certificate for the router.
    /// If not specified, the router will be insecure (no certificates)
    pub cert: Option<CertificateRef>,
}

#[skip_serializing_none]
#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct RouterStatus {
    /// Router init container is complete
    pub initialized: bool,
    /// Router is online and ready to serve
    pub online: bool,
    /// The status of the router faces
    pub faces: RouterFaces,
    /// List of the neighbor routers' faces
    pub neighbors: BTreeSet<String>,
}

#[skip_serializing_none]
#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CertificateRef {
    /// The name of the certificate, e.g., "router-cert"
    pub name: String,
    /// Namespace of the certificate.
    /// If not specified, the router's namespace will be used
    pub namespace: Option<String>,
}

#[skip_serializing_none]
#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RouterFaces {
    pub udp4: Option<String>,
    pub tcp4: Option<String>,
    pub udp6: Option<String>,
    pub tcp6: Option<String>,
}


impl RouterFaces {
    pub fn to_btree_set(&self) -> BTreeSet<String> {
        let mut faces = BTreeSet::new();
        if let Some(ref udp4) = self.udp4 {
            faces.insert(udp4.clone());
        }
        if let Some(ref tcp4) = self.tcp4 {
            faces.insert(tcp4.clone());
        }
        if let Some(ref udp6) = self.udp6 {
            faces.insert(udp6.clone());
        }
        if let Some(ref tcp6) = self.tcp6 {
            faces.insert(tcp6.clone());
        }
        faces
    }
}

impl Router {
    pub async fn reconcile(&self, ctx: Arc<Context>) -> Result<Action> {

        debug!("Reconciling router: {:?}", self);
        let my_status = self.status.clone().unwrap_or_default();
        // Proceed only if status.online is true
        match &my_status.online{
            true => {
                debug!("Router {} is online, proceeding with reconciliation", self.name_any());
            }
            false => {
                debug!("Router {} is offline, skipping reconciliation", self.name_any());
                return Ok(Action::await_change());
            }
        }

        // Update status.neighbors of all other routers in the network
        let api_router = Api::<Router>::namespaced(ctx.client.clone(), &self.namespace().unwrap());
        let my_network_name = self.labels().get(NETWORK_LABEL_KEY).ok_or(Error::OtherError("Network label not found".to_owned()))?;
        let my_faces = my_status.faces.to_btree_set();
        let lp = ListParams::default()
            .labels_from(&Expression::Equal(NETWORK_LABEL_KEY.into(), my_network_name.into()).into());

        // List all routers in the network, excluding self
        for router in api_router
            .list(&lp)
            .await
            .map_err(Error::KubeError)?
            .iter()
            .filter(|router| router.name_any() != self.name_any())
        {
            // Adding self to the set of neighbors
            let router_neighbors = match &router.status {
                Some(status) => status.neighbors.clone(),
                None => BTreeSet::new(),
            };

            // add self.faces to the neighbors
            let mut new_neighbors = router_neighbors.clone();
            for face in &my_faces {
                new_neighbors.insert(face.to_string());
            }
            debug!("Router {} neighbors: {:?}", router.name_any(), new_neighbors);
            let patches = vec![
                PatchOperation::Replace(
                    ReplaceOperation{
                        path: PointerBuf::from_tokens(vec!["status", "neighbors"]),
                        value: serde_json::to_value(new_neighbors).unwrap_or(serde_json::Value::Null),
                    }
                )
            ];
            let patch = Patch::Json::<()>(JsonPatch(patches));
            info!("Updating neigbors of router {}...", router.name_any());
            debug!("Status patch: {:?}", patch);
            let serverside = PatchParams::apply(ROUTER_MANAGER_NAME);
            let _ = api_router.patch_status(&router.name_any(), &serverside, &patch).await
                .map_err(Error::KubeError)?;

            ctx.recorder
                .publish(
                    &Event {
                        type_: EventType::Normal,
                        reason: "NeighborsInserted".into(),
                        note: Some(format!("From `{}` Router", self.name_any())),
                        action: "Updated".into(),
                        secondary: None,
                    },
                    &router.object_ref(&()),
                )
                .await
                .map_err(Error::KubeError)?;
        }
        // Publish event
        ctx.recorder
            .publish(
                &Event {
                    type_: EventType::Normal,
                    reason: "RouterUpdated".into(),
                    note: Some("Propagated my faces to all routers in the network".to_string()),
                    action: "Updated".into(),
                    secondary: None,
                },
                &self.object_ref(&()),
            )
            .await
            .map_err(Error::KubeError)?;
        Ok(Action::await_change())
    }

    pub async fn cleanup(&self, ctx: Arc<Context>) -> Result<Action> {

        // Update status.neighbors of all other routers in the network
        let api_router = Api::<Router>::namespaced(ctx.client.clone(), &self.namespace().unwrap());
        let my_network_name = self.labels().get(NETWORK_LABEL_KEY).ok_or(Error::OtherError("Network label not found".to_owned()))?;
        let lp = ListParams::default()
            .labels(&format!("{NETWORK_LABEL_KEY}={my_network_name}"));
        let my_status = self.status.clone().unwrap_or_default();
        let my_faces = my_status.faces.to_btree_set();
        for router in api_router
            .list(&lp)
            .await
            .map_err(Error::KubeError)?
            .iter()
            .filter(|router| router.name_any() != self.name_any())
        {
            let current_neighbors = match &router.status {
                Some(status) => status.neighbors.clone(),
                None => BTreeSet::new(),
            };
            // remove self.faces from the neighbors
            let mut new_neighbors = current_neighbors.clone();
            for face in &my_faces {
                new_neighbors.remove(&face.to_string());
            }
            debug!("Router {} neighbors: {:?}", router.name_any(), new_neighbors);
            let patches = vec![
                PatchOperation::Replace(
                    ReplaceOperation{
                        path: PointerBuf::from_tokens(vec!["status", "neighbors"]),
                        value: serde_json::to_value(new_neighbors).unwrap_or(serde_json::Value::Null),
                    }
                )
            ];
            let patch = Patch::Json::<()>(JsonPatch(patches));
            info!("Updating neigbors of router {}...", router.name_any());
            debug!("Status patch: {:?}", patch);
            let serverside = PatchParams::apply(ROUTER_MANAGER_NAME);
            let _ = api_router.patch_status(&router.name_any(), &serverside, &patch).await
                .map_err(Error::KubeError)?;
            ctx.recorder
                .publish(
                    &Event {
                        type_: EventType::Normal,
                        reason: "NeighborsRemoved".into(),
                        note: Some(format!("From `{}` Router", self.name_any())),
                        action: "Updated".into(),
                        secondary: None,
                    },
                    &router.object_ref(&()),
                )
                .await
                .map_err(Error::KubeError)?;
        }

        // Publish event
        ctx.recorder
            .publish(
                &Event {
                    type_: EventType::Normal,
                    reason: "RouterDeleted".into(),
                    note: Some(format!("Deleted `{}` Router", self.name_any())),
                    action: "Deleted".into(),
                    secondary: None,
                },
                &self.object_ref(&()),
            )
            .await
            .map_err(Error::KubeError)?;
        Ok(Action::await_change())
    }
}

pub fn is_router_created() -> impl Condition<Router> {
    |obj: Option<&Router>| {
        obj.is_some()
    }
}

pub fn is_router_online() -> impl Condition<Router> {
    |obj: Option<&Router>| {
        if let Some(router) = obj {
            if let Some(status) = &router.status {
                return status.online
            }
        }
        false
    }
}

pub fn is_router_initialized() -> impl Condition<Router> {
    |obj: Option<&Router>| {
        if let Some(router) = obj {
            if let Some(status) = &router.status {
                return status.initialized
            }
        }
        false
    }
}
