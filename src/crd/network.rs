use std::sync::Arc;

use crate::{daemonset::*, Context, Error, Result};
use k8s_openapi::{api::{apps::v1::DaemonSet, core::v1::{Node, Pod}}, apimachinery::pkg::api};
use kube::{
    api::{Api, Patch, PatchParams, ResourceExt},
    client::Client,
    runtime::{
        controller::Action,
        events::{Event, EventType},
        watcher,
        WatchStreamExt,
    }, CustomResource, Resource
};
use serde::{Deserialize, Serialize};
use schemars::JsonSchema;
use serde_json::json;
use tokio::time::Duration;
use futures::{Stream, StreamExt, TryStreamExt};

pub static NETWORK_FINALIZER: &str = "networks.named-data.net/finalizer";
pub static MANAGER_NAME: &str = "ndnd-controller";

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[kube(group = "named-data.net", version = "v1alpha1", kind = "Network", namespaced, shortname = "ndn")]
#[kube(status = "NetworkStatus")]
pub struct NetworkSpec {
    prefix: String,
    nodeSelector: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct NetworkStatus {
    ds_created: Option<bool>,
}

impl Network {
    pub async fn reconcile(&self, ctx: Arc<Context>) -> Result<Action> {
        let api_nw: Api<Network> = Api::namespaced(ctx.client.clone(), &self.namespace().unwrap());
        let api_ds: Api<DaemonSet> = Api::namespaced(ctx.client.clone(), &self.namespace().unwrap());
        let serverside = PatchParams::apply(MANAGER_NAME);
        let image = get_my_image(ctx.client.clone()).await;
        let ds_data = create_owned_daemonset(&self, &image);
        let ds = api_ds.patch(&self.name_any(), &serverside, &Patch::Apply(ds_data)).await.map_err(Error::KubeError)?;
        // Publish event
        ctx.recorder
            .publish(
                &Event {
                    type_: EventType::Normal,
                    reason: "DaemonSetCreated".into(),
                    note: Some(format!("Created `{}` DaemonSet for `{}` Network", ds.name_any(), self.name_any())),
                    action: "Created".into(),
                    secondary: None,
                },
                &self.object_ref(&()),
            )
            .await
            .map_err(Error::KubeError)?;
        // Update the status of the Network
        let status = json!({
            "status": NetworkStatus {
                ds_created: Some(true),
            }
        });
        let _o = api_nw
            .patch_status(&self.name_any(), &serverside, &Patch::Merge(&status))
            .await
            .map_err(Error::KubeError)?;
        Ok(Action::requeue(Duration::from_secs(5 * 60)))
    }

    pub async fn cleanup(&self, ctx: Arc<Context>) -> Result<Action> {
        let oref = self.object_ref(&());
        ctx.recorder
            .publish(
                &Event {
                    type_: EventType::Normal,
                    reason: "DeleteRequested".into(),
                    note: Some(format!("Delete `{}`", self.name_any())),
                    action: "Deleting".into(),
                    secondary: None,
                },
                &oref,
            )
            .await
            .map_err(Error::KubeError)?;
        Ok(Action::await_change())
    }
}

fn get_my_namespace() -> String {
    std::fs::read_to_string("/var/run/secrets/kubernetes.io/serviceaccount/namespace").unwrap()
}

fn get_my_pod_name() -> String {
    std::fs::read_to_string("/etc/hostname").unwrap().trim_end_matches('\n').to_string()
}

async fn get_my_image(client: Client) -> String {
    let pods = Api::<Pod>::namespaced(client.clone(), &get_my_namespace());
    let pod = pods.get(&get_my_pod_name()).await.expect("Failed to get my pod");
    let pod_spec = pod.spec.expect("Pod has no spec");
    let container = pod_spec.containers.first().expect("Pod has no containers");
    container.image.clone().expect("Container has no image")
}
