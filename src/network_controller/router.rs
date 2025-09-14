use std::{collections::BTreeSet, sync::Arc};

// use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use crate::conditions::Conditions;
use crate::events_helper::emit_info;
use json_patch::{Patch as JsonPatch, PatchOperation, ReplaceOperation, jsonptr::PointerBuf};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition as K8sCondition;
use kube::{
    Api, CustomResource, Resource, ResourceExt,
    api::{ListParams, Patch, PatchParams},
    core::Expression,
    runtime::{
        controller::Action,
        events::{Event, EventType},
        wait::Condition,
    },
};
use operator_derive::Conditions;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;
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
    derive = "Default",
    namespaced,
    shortname = "rt",
    doc = "Router represents a Named Data Networking (NDN) router in Kubernetes",
    printcolumn = r#"{"name":"Online","jsonPath":".status.online","type":"boolean"}"#,
    printcolumn = r#"{"name":"Initialized","jsonPath":".status.initialized","type":"boolean"}"#,
    printcolumn = r#"{"name":"Prefix","jsonPath":".spec.prefix","type":"string"}"#,
    printcolumn = r#"{"name":"Node","jsonPath":".spec.nodeName","type":"string"}"#,
    printcolumn = r#"{"name":"Certificate","jsonPath":".spec.cert.name","type":"string"}"#,
    status = "RouterStatus"
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
#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema, Conditions)]
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
    /// Standard Kubernetes-style conditions for this router
    /// - Ready: Initialized && Online && FacesReady
    /// - Initialized: init container is complete
    /// - Online: router is online
    /// - FacesReady: at least one face is present
    /// - NeighborsSynced: last neighbor propagation succeeded (informative)
    pub conditions: Option<Vec<K8sCondition>>,
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
        let observed_gen = self.metadata.generation.unwrap_or(0);
        let faces_ready = !my_status.faces.to_btree_set().is_empty();
        // Base conditions from current status
        let api_router = Api::<Router>::namespaced(ctx.client.clone(), &self.namespace().unwrap());
        let serverside = PatchParams::apply(ROUTER_MANAGER_NAME);
        let mut conds: Option<Vec<K8sCondition>> = None;
        // Initialized
        let mut buf = my_status.clone();
        buf.conditions = conds;
        buf.upsert_bool(
            "Initialized",
            my_status.initialized,
            if my_status.initialized {
                "InitComplete"
            } else {
                "InitPending"
            },
            None,
            observed_gen,
        );
        conds = buf.conditions;
        // Online
        let mut buf = my_status.clone();
        buf.conditions = conds;
        buf.upsert_bool(
            "Online",
            my_status.online,
            if my_status.online {
                "Online"
            } else {
                "Offline"
            },
            None,
            observed_gen,
        );
        conds = buf.conditions;
        // FacesReady
        let mut buf = my_status.clone();
        buf.conditions = conds;
        buf.upsert_bool(
            "FacesReady",
            faces_ready,
            if faces_ready {
                "FacesAvailable"
            } else {
                "FacesMissing"
            },
            None,
            observed_gen,
        );
        conds = buf.conditions;
        // Default NeighborsSynced to NotStarted until we run propagation below
        let mut buf = my_status.clone();
        buf.conditions = conds;
        buf.upsert_bool(
            "NeighborsSynced",
            false,
            "NotStarted",
            Some("Neighbor propagation not performed"),
            observed_gen,
        );
        conds = buf.conditions;
        let ready = my_status.initialized && my_status.online && faces_ready;
        let not_ready_msg = if ready {
            None
        } else {
            let mut missing: Vec<&str> = Vec::new();
            if !my_status.initialized {
                missing.push("Initialized");
            }
            if !my_status.online {
                missing.push("Online");
            }
            if !faces_ready {
                missing.push("FacesReady");
            }
            Some(format!("Missing prerequisites: {}", missing.join(", ")))
        };
        let mut buf = my_status.clone();
        buf.conditions = conds;
        buf.upsert_bool(
            "Ready",
            ready,
            if ready {
                "Ready"
            } else {
                "PrerequisitesNotReady"
            },
            not_ready_msg.as_deref(),
            observed_gen,
        );
        conds = buf.conditions;
        // If offline, patch conditions and return early
        if !my_status.online {
            let patch = Patch::Merge(serde_json::json!({ "status": { "conditions": conds } }));
            let _ = api_router
                .patch_status(&self.name_any(), &serverside, &patch)
                .await
                .map_err(Error::KubeError)?;
            debug!(
                "Router {} is offline, conditions updated, skipping reconciliation",
                self.name_any()
            );
            return Ok(Action::await_change());
        }

        // Update status.neighbors of all other routers in the network
        let api_router = Api::<Router>::namespaced(ctx.client.clone(), &self.namespace().unwrap());
        let my_network_name = self
            .labels()
            .get(NETWORK_LABEL_KEY)
            .ok_or(Error::OtherError("Network label not found".to_owned()))?;
        let my_faces = my_status.faces.to_btree_set();
        let lp = ListParams::default().labels_from(
            &Expression::Equal(NETWORK_LABEL_KEY.into(), my_network_name.into()).into(),
        );

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
            debug!(
                "Router {} neighbors: {:?}",
                router.name_any(),
                new_neighbors
            );
            let patches = vec![PatchOperation::Replace(ReplaceOperation {
                path: PointerBuf::from_tokens(vec!["status", "neighbors"]),
                value: serde_json::to_value(new_neighbors).unwrap_or(serde_json::Value::Null),
            })];
            let patch = Patch::Json::<()>(JsonPatch(patches));
            info!("Updating neigbors of router {}...", router.name_any());
            debug!("Status patch: {:?}", patch);
            let serverside = PatchParams::apply(ROUTER_MANAGER_NAME);
            let _ = api_router
                .patch_status(&router.name_any(), &serverside, &patch)
                .await
                .map_err(Error::KubeError)?;

            emit_info(
                &ctx.recorder,
                router,
                "NeighborsInserted",
                "Updated",
                Some(format!("From `{}` Router", self.name_any())),
            )
            .await;
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

        // Neighbors propagation completed; mark synced
        let mut buf = my_status.clone();
        buf.conditions = conds;
        buf.upsert_bool(
            "NeighborsSynced",
            true,
            "SyncOK",
            Some("Last neighbor propagation completed"),
            observed_gen,
        );
        conds = buf.conditions;
        let patch = Patch::Merge(serde_json::json!({
            "status": { "conditions": conds }
        }));
        let _ = api_router
            .patch_status(&self.name_any(), &serverside, &patch)
            .await
            .map_err(Error::KubeError)?;
        Ok(Action::await_change())
    }

    pub async fn cleanup(&self, ctx: Arc<Context>) -> Result<Action> {
        // Update status.neighbors of all other routers in the network
        let api_router = Api::<Router>::namespaced(ctx.client.clone(), &self.namespace().unwrap());
        let my_network_name = self
            .labels()
            .get(NETWORK_LABEL_KEY)
            .ok_or(Error::OtherError("Network label not found".to_owned()))?;
        let lp = ListParams::default().labels(&format!("{NETWORK_LABEL_KEY}={my_network_name}"));
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
            debug!(
                "Router {} neighbors: {:?}",
                router.name_any(),
                new_neighbors
            );
            let patches = vec![PatchOperation::Replace(ReplaceOperation {
                path: PointerBuf::from_tokens(vec!["status", "neighbors"]),
                value: serde_json::to_value(new_neighbors).unwrap_or(serde_json::Value::Null),
            })];
            let patch = Patch::Json::<()>(JsonPatch(patches));
            info!("Updating neigbors of router {}...", router.name_any());
            debug!("Status patch: {:?}", patch);
            let serverside = PatchParams::apply(ROUTER_MANAGER_NAME);
            let _ = api_router
                .patch_status(&router.name_any(), &serverside, &patch)
                .await
                .map_err(Error::KubeError)?;
            emit_info(
                &ctx.recorder,
                router,
                "NeighborsRemoved",
                "Updated",
                Some(format!("From `{}` Router", self.name_any())),
            )
            .await;
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

// local helpers removed; using crate::conditions

pub fn is_router_created() -> impl Condition<Router> {
    |obj: Option<&Router>| obj.is_some()
}

pub fn is_router_online() -> impl Condition<Router> {
    |obj: Option<&Router>| {
        if let Some(router) = obj
            && let Some(status) = &router.status
        {
            return status.online;
        }
        false
    }
}

pub fn is_router_initialized() -> impl Condition<Router> {
    |obj: Option<&Router>| {
        if let Some(router) = obj
            && let Some(status) = &router.status
        {
            return status.initialized;
        }
        false
    }
}
