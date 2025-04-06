
use std::env;

use futures::pin_mut;
use controller::{crd::{Router, RouterStatus, NETWORK_LABEL_KEY}, telemetry};
use futures::StreamExt;
use kube::{api::{DeleteParams, ListParams, Patch, PatchParams}, runtime::{watcher, WatchStreamExt}, Api, Client, ResourceExt};
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

    info!("Starting watcher...");
    // Watch for new routers in the network
    let wc = watcher::Config::default().streaming_lists();
    let watch_stream = watcher(api_router.clone(), wc).applied_objects();
    pin_mut!(watch_stream);
    loop {
        tokio::select! {
            Some(event) = watch_stream.next() => {
                match event {
                    Ok(router) => {
                        if router.name_any() != my_router_name {
                            info!("Found new router: {} in network: {}", router.name_any(), network_name);
                        }
                    },
                    Err(e) => error!("Error watching routers: {}", e),
                }
            }
            // Handle shutdown signal
            _ = tokio::signal::ctrl_c() => {
                info!("Received shutdown signal");
                delete_router(&api_router, &my_router_name).await?;
                info!("Deleted router: {}", my_router_name);
                break;
            }
        }
    };
    Ok(())
}

async fn delete_router(api_router: &Api<Router>, name: &str) -> anyhow::Result<()> {
    let dp = DeleteParams::default();
    let _o = api_router
        .delete(name, &dp).await
        .map_err(Error::KubeError)?;
    Ok(())
}