use actix_web::{App, HttpResponse, HttpServer, web};
use anyhow::{Context, anyhow, bail};
use json_patch::jsonptr::PointerBuf;
use k8s_openapi::api::core::v1::{
    EnvVar, HostPathVolumeSource, KeyToPath, Pod, ProjectedVolumeSource, SecretProjection, Volume,
    VolumeMount, VolumeProjection,
};
use kube::{
    Client,
    core::{
        DynamicObject, ResourceExt,
        admission::{AdmissionRequest, AdmissionResponse, AdmissionReview, Operation},
    },
};
use operator::{
    cert_controller::{Certificate, ExternalCertificate},
    network_controller::Network,
    telemetry,
};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::{env, error::Error};
use tracing::*;

static ANNOTATION_NAME: &str = "networks.named-data.net/name";
static ANNOTATION_NAMESPACE: &str = "networks.named-data.net/namespace";
static ANNOTATION_CREDENTIAL_CONTAINER: &str = "credentials.named-data.net/container";
static ANNOTATION_SIGNING: &str = "credentials.named-data.net/signing";
static ANNOTATION_TRUST_ANCHORS: &str = "credentials.named-data.net/trust-anchors";
static ANNOTATION_CERTIFICATE_CHAIN: &str = "credentials.named-data.net/certificate-chain";

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

    let credentials = resolve_credentials(&client, pod).await?;
    Ok(res.with_patch(build_patch(pod, &network, credentials.as_ref())?)?)
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CredentialKind {
    Certificate,
    ExternalCertificate,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CredentialRef {
    kind: CredentialKind,
    name: String,
}

impl CredentialRef {
    fn parse(raw: &str) -> anyhow::Result<Self> {
        let (kind, name) = raw
            .trim()
            .split_once('/')
            .ok_or_else(|| anyhow!("credential reference `{raw}` must be Kind/name"))?;
        if name.is_empty() || name.contains('/') {
            bail!("credential reference `{raw}` has an invalid resource name");
        }
        let kind = match kind {
            "Certificate" => CredentialKind::Certificate,
            "ExternalCertificate" => CredentialKind::ExternalCertificate,
            _ => bail!("unsupported credential kind `{kind}` in `{raw}`"),
        };
        Ok(Self {
            kind,
            name: name.to_string(),
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct CredentialRequest {
    container: String,
    signing: Option<CredentialRef>,
    trust_anchors: Vec<CredentialRef>,
    certificate_chain: Vec<CredentialRef>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ResolvedCertificate {
    key_secret: Option<String>,
    cert_secret: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct CredentialMounts {
    container: String,
    signing: Option<ResolvedCertificate>,
    trust_anchors: Vec<String>,
    certificate_chain: Vec<String>,
}

fn parse_credential_request(pod: &Pod) -> anyhow::Result<Option<CredentialRequest>> {
    let annotations = pod.annotations();
    let signing = annotations.get(ANNOTATION_SIGNING);
    let anchors = annotations.get(ANNOTATION_TRUST_ANCHORS);
    let chain = annotations.get(ANNOTATION_CERTIFICATE_CHAIN);
    if signing.is_none() && anchors.is_none() && chain.is_none() {
        return Ok(None);
    }

    let container = annotations
        .get(ANNOTATION_CREDENTIAL_CONTAINER)
        .ok_or_else(|| {
            anyhow!(
                "annotation `{ANNOTATION_CREDENTIAL_CONTAINER}` is required when application credentials are requested"
            )
        })?
        .trim();
    if container.is_empty() {
        bail!("annotation `{ANNOTATION_CREDENTIAL_CONTAINER}` cannot be empty");
    }

    let parse_list = |value: Option<&String>| -> anyhow::Result<Vec<CredentialRef>> {
        match value {
            None => Ok(Vec::new()),
            Some(value) => value
                .split(',')
                .map(|raw| {
                    if raw.trim().is_empty() {
                        bail!("credential reference list contains an empty item");
                    }
                    CredentialRef::parse(raw)
                })
                .collect(),
        }
    };

    Ok(Some(CredentialRequest {
        container: container.to_string(),
        signing: signing
            .map(|value| CredentialRef::parse(value))
            .transpose()?,
        trust_anchors: parse_list(anchors)?,
        certificate_chain: parse_list(chain)?,
    }))
}

async fn resolve_credentials(
    client: &Client,
    pod: &Pod,
) -> anyhow::Result<Option<CredentialMounts>> {
    let Some(request) = parse_credential_request(pod)? else {
        return Ok(None);
    };
    let namespace = pod
        .namespace()
        .ok_or_else(|| anyhow!("application credential injection requires a Pod namespace"))?;

    let signing = match request.signing {
        Some(reference) => Some(resolve_certificate(client, &namespace, &reference, true).await?),
        None => None,
    };
    let mut trust_anchors = Vec::new();
    for reference in request.trust_anchors {
        trust_anchors.push(
            resolve_certificate(client, &namespace, &reference, false)
                .await?
                .cert_secret,
        );
    }
    let mut certificate_chain = Vec::new();
    for reference in request.certificate_chain {
        certificate_chain.push(
            resolve_certificate(client, &namespace, &reference, false)
                .await?
                .cert_secret,
        );
    }

    Ok(Some(CredentialMounts {
        container: request.container,
        signing,
        trust_anchors,
        certificate_chain,
    }))
}

async fn resolve_certificate(
    client: &Client,
    namespace: &str,
    reference: &CredentialRef,
    require_key: bool,
) -> anyhow::Result<ResolvedCertificate> {
    let (key_exists, key_secret, cert_exists, cert_valid, cert_secret) = match reference.kind {
        CredentialKind::Certificate => {
            let cert = kube::Api::<Certificate>::namespaced(client.clone(), namespace)
                .get(&reference.name)
                .await
                .with_context(|| format!("failed to get Certificate/{}", reference.name))?;
            let status = cert
                .status
                .ok_or_else(|| anyhow!("Certificate/{} has no status", reference.name))?;
            (
                status.key_exists,
                status.key.secret,
                status.cert_exists,
                status.cert.valid,
                status.cert.secret,
            )
        }
        CredentialKind::ExternalCertificate => {
            let cert = kube::Api::<ExternalCertificate>::namespaced(client.clone(), namespace)
                .get(&reference.name)
                .await
                .with_context(|| format!("failed to get ExternalCertificate/{}", reference.name))?;
            let status = cert
                .status
                .ok_or_else(|| anyhow!("ExternalCertificate/{} has no status", reference.name))?;
            (
                status.key_exists,
                status.key.secret,
                status.cert_exists,
                status.cert.valid,
                status.cert.secret,
            )
        }
    };

    validate_resolved_certificate(
        reference,
        require_key,
        key_exists,
        key_secret,
        cert_exists,
        cert_valid,
        cert_secret,
    )
}

fn validate_resolved_certificate(
    reference: &CredentialRef,
    require_key: bool,
    key_exists: bool,
    key_secret: Option<String>,
    cert_exists: bool,
    cert_valid: bool,
    cert_secret: Option<String>,
) -> anyhow::Result<ResolvedCertificate> {
    if !cert_exists || !cert_valid {
        bail!(
            "credential {}/{} does not have a ready valid certificate",
            kind_name(reference),
            reference.name
        );
    }
    let cert_secret = cert_secret.ok_or_else(|| {
        anyhow!(
            "credential {}/{} has no certificate Secret",
            kind_name(reference),
            reference.name
        )
    })?;
    if require_key && (!key_exists || key_secret.is_none()) {
        bail!(
            "signing credential {}/{} does not have a private key Secret",
            kind_name(reference),
            reference.name
        );
    }
    Ok(ResolvedCertificate {
        key_secret,
        cert_secret,
    })
}

fn kind_name(reference: &CredentialRef) -> &'static str {
    match reference.kind {
        CredentialKind::Certificate => "Certificate",
        CredentialKind::ExternalCertificate => "ExternalCertificate",
    }
}

fn build_patch(
    pod: &Pod,
    network: &Network,
    credentials: Option<&CredentialMounts>,
) -> anyhow::Result<json_patch::Patch> {
    let spec = pod
        .spec
        .as_ref()
        .ok_or_else(|| anyhow!("Pod has no spec to inject"))?;
    let target_index = match credentials {
        Some(credentials) => Some(
            spec.containers
                .iter()
                .position(|container| container.name == credentials.container)
                .ok_or_else(|| {
                    anyhow!(
                        "credential target container `{}` does not exist in Pod",
                        credentials.container
                    )
                })?,
        ),
        None => None,
    };

    let mut volumes = spec.volumes.clone().unwrap_or_default();
    upsert_volume(&mut volumes, create_volume(network));
    if let Some(credentials) = credentials {
        if let Some(signing) = &credentials.signing {
            upsert_volume(&mut volumes, create_signing_volume(signing)?);
        }
        if !credentials.trust_anchors.is_empty() {
            upsert_volume(
                &mut volumes,
                create_cert_bundle_volume(
                    TRUST_ANCHORS_VOLUME_NAME,
                    "anchor",
                    &credentials.trust_anchors,
                ),
            );
        }
        if !credentials.certificate_chain.is_empty() {
            upsert_volume(
                &mut volumes,
                create_cert_bundle_volume(
                    CERTIFICATE_CHAIN_VOLUME_NAME,
                    "chain",
                    &credentials.certificate_chain,
                ),
            );
        }
    }

    let mut patches = Vec::new();
    patches.push(json_patch::PatchOperation::Add(json_patch::AddOperation {
        path: PointerBuf::from_tokens(["spec", "volumes"]),
        value: serde_json::json!(volumes),
    }));

    for (i, container) in spec.containers.iter().enumerate() {
        let mut volume_mounts = container.volume_mounts.clone().unwrap_or_default();
        upsert_volume_mount(&mut volume_mounts, create_volume_mount());
        let mut env = container.env.clone().unwrap_or_default();
        upsert_env(&mut env, create_env_var_transport());
        upsert_env(&mut env, create_env_var_prefix(network));

        if Some(i) == target_index {
            let credentials = credentials.expect("target index only exists with credentials");
            if credentials.signing.is_some() {
                upsert_volume_mount(&mut volume_mounts, create_signing_volume_mount());
                upsert_env(
                    &mut env,
                    env_var("NDN_APP_SIGNING_KEY_FILE", SIGNING_KEY_FILE),
                );
                upsert_env(
                    &mut env,
                    env_var("NDN_APP_SIGNING_CERT_FILE", SIGNING_CERT_FILE),
                );
            }
            if !credentials.trust_anchors.is_empty() {
                upsert_volume_mount(&mut volume_mounts, create_trust_anchor_volume_mount());
                upsert_env(
                    &mut env,
                    env_var("NDN_APP_TRUST_ANCHOR_DIR", TRUST_ANCHORS_MOUNT_PATH),
                );
            }
            if !credentials.certificate_chain.is_empty() {
                upsert_volume_mount(&mut volume_mounts, create_certificate_chain_volume_mount());
                upsert_env(
                    &mut env,
                    env_var(
                        "NDN_APP_CERTIFICATE_CHAIN_DIR",
                        CERTIFICATE_CHAIN_MOUNT_PATH,
                    ),
                );
            }
        }
        patches.push(json_patch::PatchOperation::Add(json_patch::AddOperation {
            path: PointerBuf::from_tokens(["spec", "containers", &i.to_string(), "volumeMounts"]),
            value: serde_json::json!(volume_mounts),
        }));
        patches.push(json_patch::PatchOperation::Add(json_patch::AddOperation {
            path: PointerBuf::from_tokens(["spec", "containers", &i.to_string(), "env"]),
            value: serde_json::json!(env),
        }));
    }
    Ok(json_patch::Patch(patches))
}

static VOLUME_NAME: &str = "run-ndnd";
static MOUNT_PATH: &str = "/run/ndnd.sock";
static SIGNING_VOLUME_NAME: &str = "ndn-app-signing";
static TRUST_ANCHORS_VOLUME_NAME: &str = "ndn-app-trust-anchors";
static CERTIFICATE_CHAIN_VOLUME_NAME: &str = "ndn-app-certificate-chain";
static SIGNING_MOUNT_PATH: &str = "/etc/ndn/app/signing";
static TRUST_ANCHORS_MOUNT_PATH: &str = "/etc/ndn/app/trust-anchors";
static CERTIFICATE_CHAIN_MOUNT_PATH: &str = "/etc/ndn/app/certificate-chain";
static SIGNING_KEY_FILE: &str = "/etc/ndn/app/signing/ndn.key";
static SIGNING_CERT_FILE: &str = "/etc/ndn/app/signing/ndn.cert";

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

fn create_signing_volume(signing: &ResolvedCertificate) -> anyhow::Result<Volume> {
    let key_secret = signing
        .key_secret
        .as_ref()
        .ok_or_else(|| anyhow!("resolved signing certificate has no private key Secret"))?;
    Ok(projected_volume(
        SIGNING_VOLUME_NAME,
        vec![
            secret_projection(key_secret, "ndn.key", "ndn.key"),
            secret_projection(&signing.cert_secret, "ndn.cert", "ndn.cert"),
        ],
    ))
}

fn create_cert_bundle_volume(volume_name: &str, file_prefix: &str, secrets: &[String]) -> Volume {
    projected_volume(
        volume_name,
        secrets
            .iter()
            .enumerate()
            .map(|(index, secret)| {
                secret_projection(secret, "ndn.cert", &format!("{file_prefix}-{index}.cert"))
            })
            .collect(),
    )
}

fn projected_volume(name: &str, sources: Vec<VolumeProjection>) -> Volume {
    Volume {
        name: name.to_string(),
        projected: Some(ProjectedVolumeSource {
            default_mode: Some(0o444),
            sources: Some(sources),
        }),
        ..Volume::default()
    }
}

fn secret_projection(secret_name: &str, key: &str, path: &str) -> VolumeProjection {
    VolumeProjection {
        secret: Some(SecretProjection {
            name: secret_name.to_string(),
            items: Some(vec![KeyToPath {
                key: key.to_string(),
                path: path.to_string(),
                ..KeyToPath::default()
            }]),
            optional: Some(false),
        }),
        ..VolumeProjection::default()
    }
}

fn credential_volume_mount(name: &str, mount_path: &str) -> VolumeMount {
    VolumeMount {
        name: name.to_string(),
        mount_path: mount_path.to_string(),
        read_only: Some(true),
        ..VolumeMount::default()
    }
}

fn create_signing_volume_mount() -> VolumeMount {
    credential_volume_mount(SIGNING_VOLUME_NAME, SIGNING_MOUNT_PATH)
}

fn create_trust_anchor_volume_mount() -> VolumeMount {
    credential_volume_mount(TRUST_ANCHORS_VOLUME_NAME, TRUST_ANCHORS_MOUNT_PATH)
}

fn create_certificate_chain_volume_mount() -> VolumeMount {
    credential_volume_mount(CERTIFICATE_CHAIN_VOLUME_NAME, CERTIFICATE_CHAIN_MOUNT_PATH)
}

fn upsert_volume(volumes: &mut Vec<Volume>, volume: Volume) {
    volumes.retain(|existing| existing.name != volume.name);
    volumes.push(volume);
}

fn upsert_volume_mount(volume_mounts: &mut Vec<VolumeMount>, volume_mount: VolumeMount) {
    volume_mounts.retain(|existing| existing.name != volume_mount.name);
    volume_mounts.push(volume_mount);
}

fn upsert_env(env: &mut Vec<EnvVar>, value: EnvVar) {
    env.retain(|existing| existing.name != value.name);
    env.push(value);
}

fn env_var(name: &str, value: &str) -> EnvVar {
    EnvVar {
        name: name.to_string(),
        value: Some(value.to_string()),
        ..EnvVar::default()
    }
}

fn create_env_var_transport() -> EnvVar {
    env_var("NDN_CLIENT_TRANSPORT", &format!("unix://{MOUNT_PATH}"))
}

fn create_env_var_prefix(network: &Network) -> EnvVar {
    env_var("NDN_PREFIX", &network.spec.prefix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use operator::network_controller::{IpFamily, NdndSpec, NetworkSpec};

    fn sample_network() -> Network {
        let mut network = Network::new(
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
        network.metadata.namespace = Some("mynetwork".into());
        network
    }

    #[test]
    fn injected_prefix_remains_the_application_prefix() {
        let network = sample_network();
        assert_eq!(
            create_env_var_prefix(&network).value.as_deref(),
            Some("/root-network/subnetwork1")
        );
    }

    #[test]
    fn parses_typed_credential_annotations() {
        let pod: Pod = serde_json::from_value(serde_json::json!({
            "metadata": {
                "annotations": {
                    "credentials.named-data.net/container": "producer",
                    "credentials.named-data.net/signing": "Certificate/app-leaf",
                    "credentials.named-data.net/trust-anchors": "Certificate/app-root,ExternalCertificate/imported-root",
                    "credentials.named-data.net/certificate-chain": "Certificate/app-ca"
                }
            }
        }))
        .expect("pod");

        let request = parse_credential_request(&pod)
            .expect("valid credential annotations")
            .expect("credentials requested");
        assert_eq!(request.container, "producer");
        assert_eq!(
            request.signing,
            Some(CredentialRef {
                kind: CredentialKind::Certificate,
                name: "app-leaf".into(),
            })
        );
        assert_eq!(request.trust_anchors.len(), 2);
        assert_eq!(request.certificate_chain.len(), 1);
    }

    #[test]
    fn patch_preserves_fields_and_mounts_credentials_only_in_target_container() {
        let pod: Pod = serde_json::from_value(serde_json::json!({
            "metadata": {"name": "app", "namespace": "mynetwork"},
            "spec": {
                "volumes": [{"name": "existing", "emptyDir": {}}],
                "containers": [
                    {
                        "name": "producer",
                        "image": "app",
                        "volumeMounts": [{"name": "existing", "mountPath": "/existing"}],
                        "env": [{"name": "KEEP_ME", "value": "yes"}]
                    },
                    {"name": "sidecar", "image": "sidecar"}
                ]
            }
        }))
        .expect("pod");
        let credentials = CredentialMounts {
            container: "producer".into(),
            signing: Some(ResolvedCertificate {
                key_secret: Some("leaf-key".into()),
                cert_secret: "leaf-cert".into(),
            }),
            trust_anchors: vec!["root-cert".into()],
            certificate_chain: vec!["ca-cert".into(), "leaf-cert".into()],
        };

        let patch = build_patch(&pod, &sample_network(), Some(&credentials)).expect("patch");
        let mut value = serde_json::to_value(&pod).expect("serialize pod");
        json_patch::patch(&mut value, &patch).expect("apply patch");
        let injected: Pod = serde_json::from_value(value).expect("patched pod");
        let spec = injected.spec.expect("pod spec");

        assert!(
            spec.volumes
                .unwrap()
                .iter()
                .any(|volume| volume.name == "existing")
        );
        let producer = &spec.containers[0];
        let producer_mounts = producer.volume_mounts.as_ref().expect("producer mounts");
        assert!(producer_mounts.iter().any(|mount| mount.name == "existing"));
        assert!(
            producer_mounts
                .iter()
                .any(|mount| mount.name == SIGNING_VOLUME_NAME)
        );
        let producer_env = producer.env.as_ref().expect("producer env");
        assert!(producer_env.iter().any(|value| value.name == "KEEP_ME"));
        assert!(
            producer_env
                .iter()
                .any(|value| value.name == "NDN_APP_SIGNING_KEY_FILE")
        );

        let sidecar = &spec.containers[1];
        let sidecar_mounts = sidecar.volume_mounts.as_ref().expect("sidecar mounts");
        assert!(
            sidecar_mounts
                .iter()
                .all(|mount| mount.name != SIGNING_VOLUME_NAME)
        );
        assert!(
            sidecar
                .env
                .as_ref()
                .expect("sidecar env")
                .iter()
                .all(|value| !value.name.starts_with("NDN_APP_"))
        );
    }

    #[test]
    fn rejects_unknown_credential_kind() {
        assert!(CredentialRef::parse("Secret/app-root").is_err());
    }

    #[test]
    fn validates_ready_certificate_kinds_and_requires_a_signing_key() {
        let certificate = CredentialRef {
            kind: CredentialKind::Certificate,
            name: "leaf".into(),
        };
        assert_eq!(
            validate_resolved_certificate(
                &certificate,
                true,
                true,
                Some("leaf-key".into()),
                true,
                true,
                Some("leaf-cert".into()),
            )
            .expect("ready Certificate"),
            ResolvedCertificate {
                key_secret: Some("leaf-key".into()),
                cert_secret: "leaf-cert".into(),
            }
        );

        let external = CredentialRef {
            kind: CredentialKind::ExternalCertificate,
            name: "root".into(),
        };
        assert!(
            validate_resolved_certificate(
                &external,
                false,
                false,
                None,
                true,
                true,
                Some("root-cert".into()),
            )
            .is_ok()
        );
        assert!(
            validate_resolved_certificate(
                &external,
                true,
                false,
                None,
                true,
                true,
                Some("root-cert".into()),
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_missing_credential_target_container() {
        let pod: Pod = serde_json::from_value(serde_json::json!({
            "metadata": {"name": "app", "namespace": "mynetwork"},
            "spec": {"containers": [{"name": "consumer", "image": "app"}]}
        }))
        .expect("pod");
        let credentials = CredentialMounts {
            container: "missing".into(),
            ..CredentialMounts::default()
        };

        assert!(build_patch(&pod, &sample_network(), Some(&credentials)).is_err());
    }
}
