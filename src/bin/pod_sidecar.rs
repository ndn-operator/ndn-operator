use futures::{TryStreamExt, pin_mut};
use json_patch::{Patch as JsonPatch, PatchOperation, ReplaceOperation, jsonptr::PointerBuf};
use kube::{
    Api, Client,
    api::{Patch, PatchParams},
    runtime::{WatchStreamExt, watcher},
};
use operator::{
    Error,
    network_controller::{ROUTER_MANAGER_NAME, Router},
    telemetry,
};
use std::process::Command;
use std::{collections::BTreeMap, env};
use tracing::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    telemetry::init().await;
    let network_namespace = env::var("NDN_NETWORK_NAMESPACE")?;
    let my_router_name = env::var("NDN_ROUTER_NAME")?;
    let client = Client::try_default().await?;
    let api_router = Api::<Router>::namespaced(client, &network_namespace);
    // Set my status.online to true
    info!("Set my router status to online");
    let patches = vec![PatchOperation::Replace(ReplaceOperation {
        path: PointerBuf::from_tokens(vec!["status", "online"]),
        value: serde_json::to_value(true).unwrap(),
    })];
    let patch = Patch::Json::<()>(JsonPatch(patches));
    debug!("Patch status: {:?}", patch);
    let serverside = PatchParams::apply(ROUTER_MANAGER_NAME);
    let patched = api_router
        .patch_status(&my_router_name, &serverside, &patch)
        .await
        .map_err(Error::KubeError)?;
    info!("Patched router status: {:?}", patched.status);
    // Watch the inner neighbors in my_router's status and run `/ndnd dv link-create <URL>` or `/ndnd dv link-destroy <URL>` when it changes
    let wc = watcher::Config::default().fields(format!("metadata.name={my_router_name}").as_str());
    let mut neighbors = BTreeMap::<String, String>::new();
    let watcher = watcher(api_router, wc).applied_objects();
    pin_mut!(watcher);
    while let Some(router) = watcher.try_next().await? {
        let new_neighbors = match router.status {
            Some(ref status) => {
                let mut merged = BTreeMap::<String, String>::new();
                merged.extend(status.inner_neighbors.clone());
                merged.extend(status.outer_neighbors.clone());
                merged
            }
            None => BTreeMap::<String, String>::new(),
        };
        // Determine added and removed by keys
        let added_keys: Vec<String> = new_neighbors
            .keys()
            .filter(|k| !neighbors.contains_key(*k))
            .cloned()
            .collect();
        let removed_keys: Vec<String> = neighbors
            .keys()
            .filter(|k| !new_neighbors.contains_key(*k))
            .cloned()
            .collect();
        for key in added_keys {
            let uri = new_neighbors.get(&key).cloned().unwrap_or_default();
            info!("Creating link to {} -> {}", key, uri);
            Command::new("/ndnd")
                .arg("dv")
                .arg("link-create")
                .arg(uri)
                .output()
                .expect("Failed to create link");
        }
        for key in removed_keys {
            let uri = neighbors.get(&key).cloned().unwrap_or_default();
            info!("Destroying link to {} -> {}", key, uri);
            Command::new("/ndnd")
                .arg("dv")
                .arg("link-destroy")
                .arg(uri)
                .output()
                .expect("Failed to destroy link");
        }
        neighbors = new_neighbors;
        info!("Updated inner neighbors: {:?}", neighbors);
    }
    Ok(())
}
