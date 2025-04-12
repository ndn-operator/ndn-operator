use json_patch::jsonptr::PointerBuf;
use k8s_openapi::api::core::v1::{EnvVar, HostPathVolumeSource, Pod, Volume, VolumeMount};
use kube::{api::ListParams, core::{
    admission::{AdmissionRequest, AdmissionResponse, AdmissionReview, Operation},
    DynamicObject, ResourceExt,
}, Client};
use operator::crd::{Network, Router, NETWORK_LABEL_KEY};
use std::{convert::Infallible, error::Error};
use tracing::*;
use warp::{reply, Filter, Reply};


static ANNOTATION_NAME: &str = "networks.named-data.net/name";
static ANNOTATION_NAMESPACE: &str = "networks.named-data.net/namespace";

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    
    let routes = warp::path("mutate")
        .and(warp::body::json())
        .and_then(mutate_handler)
        .with(warp::trace::request());

    // You must generate a certificate for the service / url,
    // encode the CA in the MutatingWebhookConfiguration, and terminate TLS here.
    // See admission_setup.sh + admission_controller.yaml.tpl for how to do this.
    let addr = format!("{}:8443", std::env::var("ADMISSION_PRIVATE_IP").unwrap());
    warp::serve(warp::post().and(routes))
        .tls()
        .cert_path("admission-controller-tls.crt")
        .key_path("admission-controller-tls.key")
        //.run(([0, 0, 0, 0], 8443)) // in-cluster
        .run(addr.parse::<std::net::SocketAddr>().unwrap()) // local-dev
        .await;
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
                            info!("accepted: {:?} on Foo {}", req.operation, name);
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

    // If network's router doesn't exist on the pod's node, we deny the request
    let api_router = kube::Api::<Router>::namespaced(client.clone(), network_namespace);
    let lp = ListParams::default()
        .labels(&format!("{}={}", NETWORK_LABEL_KEY, network_name));
    let routers = match api_router
        .list(&lp)
        .await {
            Ok(routers) => routers,
            Err(err) => {
                error!("failed to list routers: {}", err);
                return Ok(res.deny(format!("failed to list routers: {}", err)));
            }
        };
    // One of the routers must have spec.node_name == pod.spec.node_name
    let pod_node_name = &pod.spec.clone().unwrap_or_default().node_name.unwrap_or_default();
    let router = match routers
        .iter()
        .find(|router| {
                 &router.spec.node_name == pod_node_name
        }) {
            Some(router) => router,
            None => {
                let msg = format!("There are no routers on node {pod_node_name} for network {network_name} in namespace {network_namespace}");
                warn!("{}", msg);
                return Ok(res.deny(msg));
            }
        };

    let network = match TryInto::<Network>::try_into(router.owner_references()[0].clone()) {
        Ok(network) => network,
        Err(err) => {
            error!("failed to get parent network: {}", err);
            return Ok(res.deny(format!("failed to get network: {}", err)));
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
            value: serde_json::json!(create_env_var()),
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

fn create_env_var() -> EnvVar {
    EnvVar {
        name: "NDN_CLIENT_TRANSPORT".to_string(),
        value: Some(format!("unix://{}", MOUNT_PATH)),
        ..EnvVar::default()
    }
}