use std::{collections::BTreeMap, sync::Arc};

use kube::{api::ObjectMeta, runtime::{controller::Action, events::{Event, EventType}}, CustomResource, Resource, ResourceExt};
use serde::{Deserialize, Serialize};
use schemars::JsonSchema;
use serde_with::skip_serializing_none;
use super::Network;
use crate::{Context, Error, Result};

pub static NETWORK_LABEL_KEY: &str = "network.named-data.net/name";
pub static ROUTER_FINALIZER: &str = "routers.named-data.net/finalizer";
pub static UDP_UNICAST_PORT: i32 = 6363;

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[kube(group = "named-data.net", version = "v1alpha1", kind = "Router", namespaced)]
#[kube(status = "RouterStatus")]
pub struct RouterSpec {
    prefix: String,
    node: String,
    pub faces: RouterFaces,
}

#[skip_serializing_none]
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct RouterFaces {
    udp4: Option<String>,
    tcp4: Option<String>,
    udp6: Option<String>,
    tcp6: Option<String>,
}

impl RouterFaces {
    pub fn to_vec(&self) -> Vec<String> {
        let mut faces = vec![];
        if let Some(udp4) = &self.udp4 {
            faces.push(udp4.clone());
        }
        if let Some(tcp4) = &self.tcp4 {
            faces.push(tcp4.clone());
        }
        if let Some(udp6) = &self.udp6 {
            faces.push(udp6.clone());
        }
        if let Some(tcp6) = &self.tcp6 {
            faces.push(tcp6.clone());
        }
        faces
    }
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct RouterStatus {
    pub online: bool,
    pub neighbors: Vec<String>,
}

impl Router {
    pub async fn reconcile(&self, ctx: Arc<Context>) -> Result<Action> {
        // Publish event
        ctx.recorder
            .publish(
                &Event {
                    type_: EventType::Normal,
                    reason: "RouterCreated".into(),
                    note: Some(format!("Created `{}` Router", self.name_any())),
                    action: "Created".into(),
                    secondary: None,
                },
                &self.object_ref(&()),
            )
            .await
            .map_err(Error::KubeError)?;
        Ok(Action::await_change())
    }
    pub async fn cleanup(&self, ctx: Arc<Context>) -> Result<Action> {
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

pub fn create_owned_router(source: &Network, name: String, node_name: String, ip4: Option<String>, ip6: Option<String>, udp_unicast_port: i32) -> Router {
    let oref = source.controller_owner_ref(&()).unwrap();
    Router {
        metadata: ObjectMeta {
            name: Some(name),
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
            node: node_name,
            faces: RouterFaces {
                udp4: {
                    if let Some(ip4) = ip4 {
                        Some(format!("udp://{ip4}:{udp_unicast_port}"))
                    } else {
                        None
                    }
                },
                tcp4: None,
                udp6: {
                    if let Some(ip6) = ip6 {
                        Some(format!("udp://[{ip6}]:{udp_unicast_port}"))
                    } else {
                        None
                    }
                },
                tcp6: None,
            },
        },
        status: Some(RouterStatus {
            online: false,
            neighbors: vec![],
        }),
    }
}
