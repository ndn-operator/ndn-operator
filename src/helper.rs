use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::{Pod, Secret};
use kube::{Api, Client};
use crate::{Result, Error};

pub fn get_my_namespace() -> Result<String> {
    std::fs::read_to_string("/var/run/secrets/kubernetes.io/serviceaccount/namespace")
        .map_err(Error::IoError)
}

pub fn get_my_pod_name() -> Result<String> {
    std::fs::read_to_string("/etc/hostname").map_err(Error::IoError)
}

pub async fn get_my_pod(client: Client) -> Result<Pod> {
    let namespace_raw = get_my_namespace()?;
    let namespace = namespace_raw.trim_end_matches('\n');
    let api_pods = Api::<Pod>::namespaced(client, namespace);
    let pod_name_raw = get_my_pod_name()?;
    let pod_name = pod_name_raw.trim_end_matches('\n');
    api_pods.get(pod_name).await.map_err(Error::KubeError)
}

pub async fn get_my_image(client: Client) -> Result<String> {
    let pod = get_my_pod(client).await?;
    pod.spec
        .ok_or(Error::OtherError("Pod spec not found".to_string()))?
        .containers.first()
        .ok_or(Error::OtherError("Container not found".to_string()))?
        .image.clone().ok_or(Error::OtherError("Image not found".to_string()))
}

pub enum Decoded {
    /// Usually secrets are just short utf8 encoded strings
    Utf8(String),
    /// But it's allowed to just base64 encode binary in the values
    Bytes(Vec<u8>),
}

pub fn decode_secret(secret: &Secret) -> BTreeMap<String, Decoded> {
    let mut res = BTreeMap::new();
    // Ignoring binary data for now
    if let Some(data) = secret.data.clone() {
        for (k, v) in data {
            if let Ok(b) = std::str::from_utf8(&v.0) {
                res.insert(k, Decoded::Utf8(b.to_string()));
            } else {
                res.insert(k, Decoded::Bytes(v.0));
            }
        }
    }
    res
}
