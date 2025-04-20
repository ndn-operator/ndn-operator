use std::{
    collections::{BTreeMap, BTreeSet}, sync::Arc
};

use kube::{
    api::{ListParams, ObjectMeta, Patch, PatchParams},
    core::Expression,
    runtime::{
        controller::Action,
        events::{Event, EventType}, wait::Condition,
    },
    Api, CustomResource, Resource, ResourceExt,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use serde_with::skip_serializing_none;
use tracing::*;

use super::{Context, Network, NETWORK_LABEL_KEY};
use crate::{Error, Result};

pub static ROUTER_FINALIZER: &str = "router.named-data.net/finalizer";
pub static ROUTER_MANAGER_NAME: &str = "router-controller";

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[kube(group = "named-data.net", version = "v1alpha1", kind = "Router", namespaced, shortname = "rt")]
#[kube(status = "RouterStatus")]
pub struct RouterSpec {
    pub prefix: String,
    pub node_name: String,
}

#[skip_serializing_none]
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RouterStatus {
    pub initialized: Option<bool>,
    pub online: Option<bool>,
    pub faces: Option<RouterFaces>,
    pub neighbors: Option<BTreeSet<String>>,
}

impl Default for RouterStatus {
    fn default() -> Self {
        RouterStatus {
            initialized: None,
            online: None,
            faces: None,
            neighbors: None,
        }
    }
}

#[skip_serializing_none]
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RouterFaces {
    pub udp4: Option<String>,
    pub tcp4: Option<String>,
    pub udp6: Option<String>,
    pub tcp6: Option<String>,
}

impl Default for RouterFaces {
    fn default() -> Self {
        RouterFaces {
            udp4: None,
            tcp4: None,
            udp6: None,
            tcp6: None,
        }
    }
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
        match &my_status.online.unwrap_or_default() {
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
        let my_faces = my_status.faces.unwrap_or_default().to_btree_set();
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
                Some(status) => status.neighbors.clone().unwrap_or_default().clone(),
                None => BTreeSet::new(),
            };
            // add self.faces to the neighbors
            let mut new_neighbors = router_neighbors.clone();
            for face in &my_faces {
                new_neighbors.insert(face.to_string());
            }
            // Patch only neighbors
            let patch_status = json!({
                "status": RouterStatus{
                    initialized: None,
                    online: None,
                    faces: None,
                    neighbors: Some(new_neighbors),
                }
            });
            info!("Updating neigbors of router {}...", router.name_any());
            debug!("Status patch: {:?}", patch_status);
            let serverside = PatchParams::apply(ROUTER_MANAGER_NAME);
            let _ = api_router.patch_status(&router.name_any(), &serverside, &Patch::Strategic(&patch_status)).await
                .map_err(Error::KubeError);

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
                    note: Some(format!("Propagated my faces to all routers in the network")),
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
        let my_faces = my_status.faces.unwrap_or_default().to_btree_set();
        for router in api_router
            .list(&lp)
            .await
            .map_err(Error::KubeError)?
            .iter()
            .filter(|router| router.name_any() != self.name_any())
        {
            let current_neighbors = match &router.status {
                Some(status) => status.neighbors.clone().unwrap_or_default().clone(),
                None => BTreeSet::new(),
            };
            // remove self.faces from the neighbors
            let mut new_neighbors = current_neighbors.clone();
            for face in &my_faces {
                new_neighbors.remove(&face.to_string());
            }
            // Patch only neighbors
            let patch_status = json!({
                "status": RouterStatus{
                    initialized: None,
                    online: None,
                    faces: None,
                    neighbors: Some(new_neighbors),
                }
            });
            info!("Updating status of router {}...", router.name_any());
            debug!("Status: {:?}", patch_status);
            let serverside = PatchParams::apply(ROUTER_MANAGER_NAME);
            let _ = api_router.patch_status(&router.name_any(), &serverside, &Patch::Strategic(&patch_status)).await
                .map_err(Error::KubeError);
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

pub fn create_owned_router(source: &Network, name: &String, node_name: &String) -> Router {
    let oref = source.controller_owner_ref(&()).unwrap();
    Router {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            namespace: source.namespace(),
            owner_references: Some(vec![oref]),
            labels: {
                let mut labels = source.labels().clone();
                labels.extend(BTreeMap::from([(NETWORK_LABEL_KEY.to_string(), source.name_any())]));
                Some(labels)
            },
            annotations: Some(source.annotations().clone()),
            ..ObjectMeta::default()
        },
        spec: RouterSpec {
            prefix: source.spec.prefix.clone(),
            node_name: node_name.to_string(),
        },
        status: Some(RouterStatus::default())
    }
}

pub fn is_router_created() -> impl Condition<Router> {
    |obj: Option<&Router>| {
        if let Some(router) = &obj {
            debug!("Router created: {:?}", router);
            return true;
        }
        false
    }
}

pub fn is_router_online() -> impl Condition<Router> {
    |obj: Option<&Router>| {
        if let Some(router) = &obj {
            if let Some(status) = &router.status {
                if let Some(online) = &status.online {
                    return online == &true;
                }
            }
        }
        false
    }
}
