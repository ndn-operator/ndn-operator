use actix_web::{App, HttpResponse, HttpServer, web};
use anyhow::{Context, anyhow};
use json_patch::jsonptr::PointerBuf;
use k8s_openapi::api::core::v1::{EnvVar, HostPathVolumeSource, Pod, Volume, VolumeMount};
use kube::{
    Client,
    core::{
        DynamicObject, ResourceExt,
        admission::{AdmissionRequest, AdmissionResponse, AdmissionReview, Operation},
    },
};
use operator::{network_controller::Network, telemetry};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::{env, error::Error};
use tracing::*;

static ANNOTATION_NAME: &str = "networks.named-data.net/name";
static ANNOTATION_NAMESPACE: &str = "networks.named-data.net/namespace";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    telemetry::init().await;

    let listen_port = env::var("NDN_INJECTOR_PORT")
        .unwrap_or("8443".to_string())
        .parse::<u16>()?;
    let listen_ip = env::var("NDN_INJECTOR_IP")
        .unwrap_or("0.0.0.0".to_string())
        .parse::<std::net::IpAddr>()?;
    let cert_path = env::var("NDN_INJECTOR_TLS_CERT_FILE").unwrap_or("tls.crt".to_string());
    let key_path = env::var("NDN_INJECTOR_TLS_KEY_FILE").unwrap_or("tls.key".to_string());

    let tls_config = load_tls_config(&cert_path, &key_path)?;
    info!("starting injector webhook on {listen_ip}:{listen_port}");

    HttpServer::new(|| App::new().route("/", web::post().to(mutate_handler)))
        .bind_rustls_0_23((listen_ip, listen_port), tls_config)?
        .run()
        .await?;

    Ok(())
}

fn load_tls_config(cert_path: &str, key_path: &str) -> anyhow::Result<rustls::ServerConfig> {
    let cert_chain = CertificateDer::pem_file_iter(cert_path)
        .with_context(|| format!("failed to open TLS certificate file `{cert_path}`"))?
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("failed to read TLS certificates from `{cert_path}`"))?;
    if cert_chain.is_empty() {
        return Err(anyhow!(
            "TLS certificate file `{cert_path}` contains no certificates"
        ));
    }

    let private_key = PrivateKeyDer::from_pem_file(key_path)
        .with_context(|| format!("failed to read TLS private key from `{key_path}`"))?;

    rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, private_key)
        .with_context(|| format!("failed to build TLS config from `{cert_path}` and `{key_path}`"))
}

async fn mutate_handler(body: web::Json<AdmissionReview<DynamicObject>>) -> HttpResponse {
    let req: AdmissionRequest<_> = match body.into_inner().try_into() {
        Ok(req) => req,
        Err(err) => {
            error!("invalid request: {}", err.to_string());
            return HttpResponse::Ok()
                .json(AdmissionResponse::invalid(err.to_string()).into_review());
        }
    };

    let mut res = AdmissionResponse::from(&req);
    match (req.object, &req.operation) {
        (Some(obj), Operation::Create) => {
            let obj_name = obj.name_any();
            match obj.try_parse::<Pod>() {
                Ok(pod) => {
                    let (network_name, network_namespace) = match (
                        pod.annotations().get(ANNOTATION_NAME),
                        pod.annotations().get(ANNOTATION_NAMESPACE),
                    ) {
                        (Some(name), Some(ns)) => (name.clone(), ns.clone()),
                        (Some(name), None) => (name.clone(), pod.namespace().unwrap_or_default()),
                        _ => {
                            debug!("skipped: {:?} on {}", req.operation, obj_name);
                            return HttpResponse::Ok().json(res.into_review());
                        }
                    };
                    res = match mutate(res.clone(), &pod, &network_name, &network_namespace).await {
                        Ok(res) => {
                            info!("accepted: {:?} on {}", req.operation, obj_name);
                            res
                        }
                        Err(err) => {
                            warn!("denied: {:?} on {} ({})", req.operation, obj_name, err);
                            res.deny(err.to_string())
                        }
                    };
                }
                Err(err) => {
                    warn!("denied: {:?} on {} ({})", req.operation, obj_name, err);
                    res = res.deny(err.to_string());
                }
            }
        }
        _ => {
            debug!("skipped: {:?}", req.operation);
        }
    }
    // Wrap the AdmissionResponse wrapped in an AdmissionReview
    HttpResponse::Ok().json(res.into_review())
}

async fn mutate(
    res: AdmissionResponse,
    pod: &Pod,
    network_name: &String,
    network_namespace: &String,
) -> Result<AdmissionResponse, Box<dyn Error>> {
    let client = Client::try_default()
        .await
        .expect("Expected a valid KUBECONFIG environment variable");

    // If network doesn't exist, deny the request
    let api_network = kube::Api::<Network>::namespaced(client.clone(), network_namespace);
    let network = match api_network.get(network_name).await {
        Ok(network) => network,
        Err(err) => {
            error!("failed to get network {network_name} in {network_namespace}: {err}");
            return Ok(res.deny(format!(
                "failed to get network {network_name} in {network_namespace}: {err}"
            )));
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
            path: PointerBuf::from_tokens([
                "spec",
                "containers",
                &i.to_string(),
                "volumeMounts",
                "-",
            ]),
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
        value: Some(format!("unix://{MOUNT_PATH}")),
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

#[cfg(test)]
mod tests {
    use super::*;
    use operator::network_controller::{IpFamily, NdndSpec, NetworkSpec};

    #[test]
    fn injected_prefix_remains_the_application_prefix() {
        let network = Network::new(
            "subnetwork1",
            NetworkSpec {
                prefix: "/root-network/subnetwork1".into(),
                dv_network: Some("/root-network".into()),
                udp_unicast_port: 6363,
                ip_family: IpFamily::IPv4,
                node_selector: None,
                template: None,
                ndnd: NdndSpec::default(),
                operator: None,
                router_cert_issuer: None,
                trust_anchors: None,
                faces: None,
            },
        );

        assert_eq!(
            create_env_var_prefix(&network).value.as_deref(),
            Some("/root-network/subnetwork1")
        );
    }
}
