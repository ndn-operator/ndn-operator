use operator::{
    crd::{Router, RouterStatus, ROUTER_MANAGER_NAME}, telemetry, Error
};
use futures::{TryStreamExt, pin_mut};
use kube::{api::{Patch, PatchParams}, runtime::{watcher, WatchStreamExt}, Api, Client, ResourceExt};
use serde_json::json;
use std::{collections::BTreeSet, env};
use std::process::Command;
use tracing::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    telemetry::init().await;
    let network_namespace = env::var("NDN_NETWORK_NAMESPACE")?;
    let my_router_name = env::var("NDN_ROUTER_NAME")?;
    let client = Client::try_default().await?; 
    let api_router = Api::<Router>::namespaced(client, &network_namespace);
    // Set my_router.status.online to true
    let my_router = api_router.get(&my_router_name).await?;
    let current_status = match my_router.status {
        Some(ref status) => status.clone(),
        None => RouterStatus {
            online: false,
            neighbors: BTreeSet::new(),
        },
    };
    let status = json!({
        "status": RouterStatus{
            online: true,
            neighbors: current_status.neighbors,
        }
    });
    let serverside = PatchParams::apply(ROUTER_MANAGER_NAME);
    let _ = api_router.patch_status(&my_router.name_any(), &serverside, &Patch::Merge(&status)).await
        .map_err(Error::KubeError);
    info!("Set my router status to online");
    // Watch the neighbors in my_router's status and run `/ndnd dv link-create <URL>` or `/ndnd dv link-destroy <URL>` when it changes
    let wc = watcher::Config::default()
        .fields(format!("metadata.name={}", my_router_name).as_str());
    let mut neighbors = BTreeSet::<String>::new();
    let watcher = watcher(api_router, wc).applied_objects();
    pin_mut!(watcher);
    while let Some(router) = watcher.try_next().await? {
        let new_neighbors = match router.status {
            Some(ref status) => status.neighbors.clone(),
            None => BTreeSet::<String>::new(),
        };
        let added_neighbors: BTreeSet<String> = new_neighbors.difference(&neighbors).cloned().collect();
        let removed_neighbors: BTreeSet<String> = neighbors.difference(&new_neighbors).cloned().collect();
        for neighbor in added_neighbors {
            info!("Creating link to {}", neighbor);
            Command::new("/ndnd")
                .arg("dv")
                .arg("link-create")
                .arg(neighbor)
                .output()
                .expect("Failed to create link");
        }
        for neighbor in removed_neighbors {
            info!("Destroying link to {}", neighbor);
            Command::new("/ndnd")
                .arg("dv")
                .arg("link-destroy")
                .arg(neighbor)
                .output()
                .expect("Failed to destroy link");
        }
        neighbors = new_neighbors;
        info!("Updated neighbors: {:?}", neighbors);
        };
    Ok(())
}