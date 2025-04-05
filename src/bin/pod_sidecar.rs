
use std::env;

use controller::crd::{Router, NETWORK_LABEL_KEY};
use futures::TryStreamExt;
use kube::{api::ListParams, Api, Client, ResourceExt, runtime::{watcher, WatchStreamExt}};
use controller::Error;
use tracing::*;


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