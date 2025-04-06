use std::{collections::BTreeMap, sync::Arc};

use kube::{api::ObjectMeta, runtime::{controller::Action, events::{Event, EventType}}, CustomResource, Resource, ResourceExt};
use serde::{Deserialize, Serialize};
use schemars::JsonSchema;
use super::Network;
use crate::{Context, Error, Result};
use tokio::time::Duration;

pub static NETWORK_LABEL_KEY: &str = "network.named-data.net/name";
pub static ROUTER_FINALIZER: &str = "routers.named-data.net/finalizer";
pub static UDP_UNICAST_PORT: i32 = 6363;

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[kube(group = "named-data.net", version = "v1alpha1", kind = "Router", namespaced)]
#[kube(status = "RouterStatus")]
pub struct RouterSpec {
    prefix: String,
    node: String,
    udp4: Option<String>,
    tcp4: Option<String>,
    udp6: Option<String>,
    tcp6: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct RouterStatus {
    pub online: bool,
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
        status: Some(RouterStatus {
            online: false,
        }),
    }
}
