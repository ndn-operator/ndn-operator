use json_patch::jsonptr::PointerBuf;
use k8s_openapi::api::core::v1::{EnvVar, HostPathVolumeSource, Pod, Volume, VolumeMount};
use kube::{core::{
    admission::{AdmissionRequest, AdmissionResponse, AdmissionReview, Operation},
    DynamicObject, ResourceExt,
}, Client};
use operator::crd::Network;
use std::{convert::Infallible, error::Error};
use tracing::*;
use std::env;
use warp::{reply, Filter, Reply};


static ANNOTATION_NAME: &str = "networks.named-data.net/name";
static ANNOTATION_NAMESPACE: &str = "networks.named-data.net/namespace";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    
    let listen_port = env::var("NDN_INJECT_PORT").unwrap_or("8443".to_string()).parse::<u16>()?;
    let listen_ip = env::var("NDN_INJECT_IP").unwrap_or("0.0.0.0".to_string()).parse::<std::net::IpAddr>()?;
    let cert_path = env::var("NDN_INJECT_TLS_CERT_FILE").unwrap_or("tls.crt".to_string());
    let key_path = env::var("NDN_INJECT_TLS_KEY_FILE").unwrap_or("tls.key".to_string());

    let routes = warp::path::end()
        .and(warp::body::json())
        .and_then(mutate_handler)
        .with(warp::trace::request());


    warp::serve(warp::post().and(routes))
        .tls()
        .cert_path(cert_path)
        .key_path(key_path)
        .run((listen_ip, listen_port))
        .await;
    Ok(())
}

async fn mutate_handler(body: AdmissionReview<DynamicObject>) -> Result<impl Reply, Infallible> {
    let req: AdmissionRequest<_> = match body.try_into() {
        Ok(req) => req,
        Err(err) => {
            error!("invalid request: {}", err.to_string());
            return Ok(reply::json(
                &AdmissionResponse::invalid(err.to_string()).into_review(),
            ));
        }
    };

    let mut res = AdmissionResponse::from(&req);
    if let Some(obj) = req.object {
        if let Some(pod) = obj.try_parse::<Pod>().ok() {
            let name = pod.name_any();
            if let Operation::Create = req.operation {
                if let Some(network_name) = pod.annotations().get(ANNOTATION_NAME) {
                    let network_namespace = match pod.annotations().get(ANNOTATION_NAMESPACE) {
                        Some(ns) => ns,
                        None => &pod.namespace().unwrap(),
                    };
                    res = match mutate(res.clone(), &pod, network_name, network_namespace).await {
                        Ok(res) => {
                            info!("accepted: {:?} on {}", req.operation, name);
                            res
                        }
                        Err(err) => {
                            warn!("denied: {:?} on {} ({})", req.operation, name, err);
                            res.deny(err.to_string())
                        }
                    };
                };
            }
        }
    }
    // Wrap the AdmissionResponse wrapped in an AdmissionReview
    Ok(reply::json(&res.into_review()))
}


async fn mutate(res: AdmissionResponse, pod: &Pod, network_name: &String, network_namespace: &String) -> Result<AdmissionResponse, Box<dyn Error>> {

    let client = Client::try_default().await.expect("Expected a valid KUBECONFIG environment variable");

    // If network doesn't exist, deny the request
    let api_network = kube::Api::<Network>::namespaced(client.clone(), network_namespace);
    let network = match api_network
        .get(network_name)
        .await
    {
        Ok(network) => network,
        Err(err) => {
            error!("failed to get network {network_name} in {network_namespace}: {err}");
            return Ok(res.deny(format!("failed to get network {network_name} in {network_namespace}: {err}")));
        }
    };

    // Mount ndnd socket to each container in the pod
    let containers_len = match pod.spec {
        Some(ref spec) => spec.containers.len(),
        None => 0,
    };
    let mut patches = Vec::new();
    patches.push(json_patch::PatchOperation::Add(json_patch::AddOperation {
        path: PointerBuf::from_tokens(["spec", "volumes"]),
        value: serde_json::json!([]),
    }));
    patches.push(json_patch::PatchOperation::Add(json_patch::AddOperation {
        path: PointerBuf::from_tokens(["spec", "volumes", "-"]),
        value: serde_json::json!(create_volume(&network)),
    }));
    // patch each container
    (0..containers_len).for_each(|i| {
        patches.push(json_patch::PatchOperation::Add(json_patch::AddOperation {
            path: PointerBuf::from_tokens(["spec", "containers", &i.to_string(), "volumeMounts"]),
            value: serde_json::json!([]),
        }));
        patches.push(json_patch::PatchOperation::Add(json_patch::AddOperation {
            path: PointerBuf::from_tokens(["spec", "containers", &i.to_string(), "volumeMounts", "-"]),
            value: serde_json::json!(create_volume_mount()),
        }));
        patches.push(json_patch::PatchOperation::Add(json_patch::AddOperation {
            path: PointerBuf::from_tokens(["spec", "containers", &i.to_string(), "env"]),
            value: serde_json::json!([]),
        }));
        patches.push(json_patch::PatchOperation::Add(json_patch::AddOperation {
            path: PointerBuf::from_tokens(["spec", "containers", &i.to_string(), "env", "-"]),
            value: serde_json::json!(create_env_var_transport()),
        }));
        patches.push(json_patch::PatchOperation::Add(json_patch::AddOperation {
            path: PointerBuf::from_tokens(["spec", "containers", &i.to_string(), "env", "-"]),
            value: serde_json::json!(create_env_var_prefix(&network)),
        }));
    });
    Ok(res.with_patch(json_patch::Patch(patches))?)
}

static VOLUME_NAME: &str = "run-ndnd";
static MOUNT_PATH: &str = "/run/ndnd.sock";

fn create_volume(network: &Network) -> Volume {
    Volume {
        name: VOLUME_NAME.to_string(),
        host_path: Some(HostPathVolumeSource {
            path: network.host_socket_path(),
            type_: Some("Socket".to_string()),
            ..HostPathVolumeSource::default()
        }),
        ..Volume::default()
    }
}

fn create_volume_mount() -> VolumeMount {
    VolumeMount {
        name: VOLUME_NAME.to_string(),
        mount_path: MOUNT_PATH.to_string(),
        ..VolumeMount::default()
    }
}

fn create_env_var_transport() -> EnvVar {
    EnvVar {
        name: "NDN_CLIENT_TRANSPORT".to_string(),
        value: Some(format!("unix://{}", MOUNT_PATH)),
        ..EnvVar::default()
    }
}

fn create_env_var_prefix(network: &Network) -> EnvVar {
    EnvVar {
        name: "NDN_PREFIX".to_string(),
        value: Some(network.spec.prefix.to_string()),
        ..EnvVar::default()
    }
}