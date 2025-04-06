
use std::env;

use controller::crd::{Router, RouterStatus, NETWORK_LABEL_KEY};
use futures::TryStreamExt;
use kube::{api::{ListParams, Patch, PatchParams}, runtime::{watcher, WatchStreamExt}, Api, Client, ResourceExt};
use controller::Error;
use serde_json::json;
use tracing::*;

pub static MANAGER_NAME: &str = "ndnd-watcher";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let network_name = env::var("NDN_NETWORK_NAME")?;
    let network_namespace = env::var("NDN_NETWORK_NAMESPACE")?;
    let my_router_name = env::var("NDN_ROUTER_NAME")?;
    let client = Client::try_default().await?; 
    let api_router = Api::<Router>::namespaced(client, &network_namespace);
    let lp = ListParams::default()
        .labels(&format!("{}={network_name}", NETWORK_LABEL_KEY));
    // Print every router in the network except the one we are running
    api_router.list(&lp)
        .await
        .map_err(Error::KubeError)?
        .iter()
        .filter(|router| router.name_any() != my_router_name)
        .for_each(|router| {
            info!("Found router: {} in network: {}", router.name_any(), network_name);
        });

    // Update the status of the router we are running
    let my_router = api_router.get(&my_router_name).await.map_err(Error::KubeError)?;
    let status = json!({
        "status": RouterStatus{
            online: true,
        }
    });
    let serverside = PatchParams::apply(MANAGER_NAME);
    let _o = api_router
            .patch_status(&my_router.name_any(), &serverside, &Patch::Merge(&status))
            .await
            .map_err(Error::KubeError)?;

    // Watch for new routers in the network
    let wc = watcher::Config::default().streaming_lists();
    watcher(api_router, wc)
        .applied_objects()
        .default_backoff()
        .try_for_each(async |router| {
            info!("Saw router: {} in network: {}", router.name_any(), network_name);
            Ok(())
        })
        .await?;
    Ok(())
}