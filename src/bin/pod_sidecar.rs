
use std::env;

use controller::{crd::{Router, RouterStatus, NETWORK_LABEL_KEY}, telemetry};
use futures::TryStreamExt;
use kube::{api::{ListParams, Patch, PatchParams}, runtime::{watcher, WatchStreamExt}, Api, Client, ResourceExt};
use controller::Error;
use serde_json::json;
use tracing::*;

pub static MANAGER_NAME: &str = "ndnd-watcher";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    telemetry::init().await;
    let network_name = env::var("NDN_NETWORK_NAME")?;
    let network_namespace = env::var("NDN_NETWORK_NAMESPACE")?;
    let my_router_name = env::var("NDN_ROUTER_NAME")?;
    let client = Client::try_default().await?; 
    let api_router = Api::<Router>::namespaced(client, &network_namespace);
    let lp = ListParams::default()
        .labels(&format!("{}={network_name}", NETWORK_LABEL_KEY));
    // Find spec.faces of every router in the network except the one we are running
    let neighbors_faces = api_router
        .list(&lp)
        .await
        .map_err(Error::KubeError)?
        .iter()
        .filter(|router| router.name_any() != my_router_name)
        .flat_map(|router| {
            let faces = &router.spec.faces;
            info!("Router {} faces: {:?}", router.name_any(), faces);
            faces.to_btree_set()
        })
        .collect();

    // Update the status of the router we are running
    let my_router = api_router.get(&my_router_name).await.map_err(Error::KubeError)?;
    let status = json!({
        "status": RouterStatus{
            online: true,
            neighbors: neighbors_faces,
        }
    });
    let serverside = PatchParams::apply(MANAGER_NAME);
    let _o = api_router
            .patch_status(&my_router.name_any(), &serverside, &Patch::Merge(&status))
            .await
            .map_err(Error::KubeError)?;
    // Watch my_router and print the changes
    let wc = watcher::Config::default()
        .fields(format!("metadata.name={}", my_router_name).as_str());
    watcher(api_router, wc)
        .default_backoff()
        .applied_objects()
        .try_for_each(async |router| {
            info!("Router {} changed: {:?}", router.name_any(), router);
            if let Some(ref status) = router.status {
                info!("Router {} status: {:?}", router.name_any(), status);
            }
            Ok(())
        }).await?;
    Ok(())
}