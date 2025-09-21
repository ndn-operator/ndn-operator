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
use std::{collections::BTreeSet, env};
use tracing::*;
// Pure helpers for testing and reuse
fn merge_neighbor_uris(
    inner: &std::collections::BTreeMap<String, String>,
    outer: &std::collections::BTreeMap<String, String>,
) -> BTreeSet<String> {
    let mut set = BTreeSet::<String>::new();
    set.extend(inner.values().cloned());
    set.extend(outer.values().cloned());
    set
}

fn diff_uris(
    old_set: &BTreeSet<String>,
    new_set: &BTreeSet<String>,
) -> (BTreeSet<String>, BTreeSet<String>) {
    let added: BTreeSet<String> = new_set.difference(old_set).cloned().collect();
    let removed: BTreeSet<String> = old_set.difference(new_set).cloned().collect();
    (added, removed)
}

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
    // Watch the neighbors in my_router's status and run `/ndnd dv link-create <URL>` or `/ndnd dv link-destroy <URL>` when it changes
    let wc = watcher::Config::default().fields(format!("metadata.name={my_router_name}").as_str());
    // Track URIs only (inner + outer neighbors) as a set
    let mut neighbors = BTreeSet::<String>::new();
    let watcher = watcher(api_router, wc).applied_objects();
    pin_mut!(watcher);
    while let Some(router) = watcher.try_next().await? {
        let new_neighbors = match router.status {
            Some(ref status) => {
                merge_neighbor_uris(&status.inner_neighbors, &status.outer_neighbors)
            }
            None => BTreeSet::<String>::new(),
        };
        // Determine added and removed by URIs
        let (added_neighbors, removed_neighbors) = diff_uris(&neighbors, &new_neighbors);
        for uri in added_neighbors {
            info!("Creating link to {}", uri);
            Command::new("/ndnd")
                .arg("dv")
                .arg("link-create")
                .arg(&uri)
                .output()
                .expect("Failed to create link");
        }
        for uri in removed_neighbors {
            info!("Destroying link to {}", uri);
            Command::new("/ndnd")
                .arg("dv")
                .arg("link-destroy")
                .arg(&uri)
                .output()
                .expect("Failed to destroy link");
        }
        neighbors = new_neighbors;
        info!("Updated neighbors: {:?}", neighbors);
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::collections::{BTreeMap, BTreeSet};

        #[test]
        fn test_merge_neighbor_uris_dedup() {
            let mut inner = BTreeMap::new();
            inner.insert("r1".to_string(), "udp://10.0.0.1:6363".to_string());
            inner.insert("r2".to_string(), "udp://10.0.0.2:6363".to_string());
            let mut outer = BTreeMap::new();
            // duplicate URI should be deduped by set
            outer.insert("nl-a".to_string(), "udp://10.0.0.2:6363".to_string());
            outer.insert("nl-b".to_string(), "udp://203.0.113.1:6363".to_string());

            let merged = merge_neighbor_uris(&inner, &outer);
            assert_eq!(merged.len(), 3);
            assert!(merged.contains("udp://10.0.0.1:6363"));
            assert!(merged.contains("udp://10.0.0.2:6363"));
            assert!(merged.contains("udp://203.0.113.1:6363"));
        }

        #[test]
        fn test_diff_uris_added_removed() {
            let old: BTreeSet<String> = ["udp://10.0.0.1:6363", "udp://10.0.0.2:6363"]
                .into_iter()
                .map(|s| s.to_string())
                .collect();
            let new: BTreeSet<String> = [
                "udp://10.0.0.2:6363", // kept
                "udp://10.0.0.3:6363", // added
            ]
            .into_iter()
            .map(|s| s.to_string())
            .collect();
            let (added, removed) = diff_uris(&old, &new);
            assert_eq!(added.len(), 1);
            assert!(added.contains("udp://10.0.0.3:6363"));
            assert_eq!(removed.len(), 1);
            assert!(removed.contains("udp://10.0.0.1:6363"));
        }
    }
    Ok(())
}
