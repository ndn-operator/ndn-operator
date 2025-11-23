use clap::Parser;
use futures::future::try_join_all;
use k8s_openapi::api::core::v1::Secret;
use kube::{
    Client, Resource,
    api::{Api, Patch, PatchParams},
    runtime::wait::await_condition,
};
use operator::{
    Error, NdndConfig,
    cert_controller::{
        Certificate, CertificateStatus, ExternalCertificate, ExternalCertificateStatus,
        is_cert_valid, is_external_cert_valid,
    },
    dv::RouterConfig,
    fw::{FacesConfig, ForwarderConfig, TcpConfig, UdpConfig, UnixConfig, WebSocketConfig},
    helper::{Decoded, decode_secret},
    network_controller::{IpFamily, TrustAnchorRef},
    router_controller::{Router, is_router_created},
    telemetry,
};
use serde_json::json;
use std::{collections::BTreeMap, env, time::Duration};
use tracing::*;

/// Generate config file for ndnd
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    // Output file
    #[arg(short, long)]
    output: String,
}

#[derive(Debug, Clone)]
struct GenConfigParams {
    network_name: String,
    router_name: String,
    udp_unicast_port: u16,
    keychain: String,
    socket_path: Option<String>,
    trust_anchors: Option<Vec<String>>,
    tcp_port: Option<u16>,
    ws_port: Option<u16>,
}

#[derive(Debug, Clone)]
struct ResolvedTrustAnchor {
    resource_name: String,
    namespace: String,
    cert_name: String,
    cert_secret_name: String,
}

fn gen_config(params: GenConfigParams) -> NdndConfig {
    let GenConfigParams {
        network_name,
        router_name,
        udp_unicast_port,
        keychain,
        socket_path,
        trust_anchors,
        tcp_port,
        ws_port,
    } = params;
    NdndConfig {
        dv: RouterConfig {
            network: format!("/{network_name}"),
            router: format!("/{network_name}/{router_name}"),
            keychain,
            trust_anchors,
            ..RouterConfig::default()
        },
        fw: ForwarderConfig {
            faces: FacesConfig {
                udp: Some(UdpConfig {
                    enabled_unicast: true,
                    port_unicast: Some(udp_unicast_port),
                    ..UdpConfig::default()
                }),
                tcp: tcp_port
                    .map(|p| TcpConfig {
                        enabled: true,
                        port_unicast: p,
                        ..TcpConfig::default()
                    })
                    .or(Some(TcpConfig::default())), // When tcp config is absent, ndnd enables it on default port 6363, but we want to disable it explicitly
                unix: Some(UnixConfig {
                    enabled: true,
                    socket_path: socket_path.unwrap_or("/run/nfd/nfd.sock".to_string()),
                }),
                websocket: ws_port
                    .map(|p| WebSocketConfig {
                        enabled: true,
                        bind: "0.0.0.0".to_string(),
                        port: p,
                        tls_enabled: false,
                        tls_cert: None,
                        tls_key: None,
                    })
                    .or(Some(WebSocketConfig::default())), // When websocket config is absent, ndnd enables it on default port 9696, but we want to disable it explicitly
                ..FacesConfig::default()
            },
            ..ForwarderConfig::default()
        },
    }
}

async fn load_secret_data(
    api_secret: &Api<Secret>,
    secret_name: &str,
) -> Result<BTreeMap<String, Decoded>, Error> {
    let secret = api_secret
        .get(secret_name)
        .await
        .map_err(Error::KubeError)?;
    Ok(decode_secret(&secret))
}

fn secret_value_utf8(
    data: &BTreeMap<String, Decoded>,
    secret_name: &str,
    key: &str,
) -> Result<String, Error> {
    match data.get(key) {
        Some(Decoded::Utf8(text)) => Ok(text.clone()),
        Some(Decoded::Bytes(_)) => Err(Error::OtherError(format!(
            "{key} in secret {secret_name} is not a valid UTF-8 string"
        ))),
        None => Err(Error::OtherError(format!(
            "{key} not found in secret {secret_name}"
        ))),
    }
}

trait CertSecret {
    fn cert_fields(&self) -> (Option<String>, Option<String>);
}

impl CertSecret for CertificateStatus {
    fn cert_fields(&self) -> (Option<String>, Option<String>) {
        (self.cert.name.clone(), self.cert.secret.clone())
    }
}

impl CertSecret for ExternalCertificateStatus {
    fn cert_fields(&self) -> (Option<String>, Option<String>) {
        (self.cert.name.clone(), self.cert.secret.clone())
    }
}

async fn resolve_cert_from_api<T, F, C, S>(
    api: Api<T>,
    name: &str,
    wait_condition: F,
    kind_label: &str,
) -> Result<(String, String), Error>
where
    T: Resource<DynamicType = ()>
        + Clone
        + serde::de::DeserializeOwned
        + std::fmt::Debug
        + Send
        + Sync
        + 'static,
    F: Fn() -> C,
    C: kube::runtime::wait::Condition<T>,
    T: kube::core::object::HasStatus<Status = S>,
    S: CertSecret,
{
    tokio::time::timeout(
        Duration::from_secs(3),
        await_condition(api.clone(), name, wait_condition()),
    )
    .await
    .map_err(|e| {
        Error::OtherError(format!(
            "Timeout while waiting for {kind_label} `{name}` to be valid: {e}"
        ))
    })?
    .map_err(|e| {
        Error::OtherError(format!(
            "Failed while waiting for {kind_label} `{name}` to become ready: {e}"
        ))
    })?;

    let resource = api.get_status(name).await.map_err(Error::KubeError)?;
    let status = resource
        .status()
        .ok_or_else(|| Error::OtherError(format!("Status not found for {kind_label} `{name}`")))?;
    let (cert_name_opt, cert_secret_opt) = status.cert_fields();

    let cert_name = cert_name_opt.ok_or_else(|| {
        Error::OtherError(format!("Certificate name not found in status for `{name}`"))
    })?;
    let cert_secret_name = cert_secret_opt.ok_or_else(|| {
        Error::OtherError(format!(
            "Certificate secret not found in status for `{name}`"
        ))
    })?;

    Ok((cert_name, cert_secret_name))
}

async fn resolve_trust_anchor(
    trust_anchor: TrustAnchorRef,
    client: Client,
    default_namespace: String,
) -> Result<ResolvedTrustAnchor, Error> {
    let TrustAnchorRef {
        name,
        kind,
        namespace,
    } = trust_anchor;
    let namespace = namespace.unwrap_or(default_namespace);

    let (cert_name, cert_secret_name) = match kind.as_str() {
        "Certificate" => {
            let api = Api::<Certificate>::namespaced(client.clone(), &namespace);
            resolve_cert_from_api(api, &name, is_cert_valid, "Certificate").await?
        }
        "ExternalCertificate" => {
            let api = Api::<ExternalCertificate>::namespaced(client.clone(), &namespace);
            resolve_cert_from_api(api, &name, is_external_cert_valid, "ExternalCertificate").await?
        }
        _ => {
            return Err(Error::OtherError(format!(
                "Unsupported trust anchor kind: {}",
                kind
            )));
        }
    };

    Ok(ResolvedTrustAnchor {
        resource_name: name,
        namespace,
        cert_name,
        cert_secret_name,
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    telemetry::init().await;
    let args = Args::parse();
    let network_name = env::var("NDN_NETWORK_NAME")?;
    let network_namespace = env::var("NDN_NETWORK_NAMESPACE")?;
    let router_name = env::var("NDN_ROUTER_NAME")?;
    let certificate_name = env::var("NDN_ROUTER_CERT_NAME").unwrap_or(router_name.clone());
    let udp_unicast_port = env::var("NDN_UDP_UNICAST_PORT")?.parse::<u16>()?;
    let socket_path = env::var("NDN_SOCKET_PATH").ok();
    let keychain_dir = env::var("NDN_KEYS_DIR")?;
    let insecure = env::var("NDN_INSECURE")?.parse::<bool>()?;
    let keychain = match insecure {
        true => "insecure".to_string(),
        false => format!("dir://{keychain_dir}"),
    };

    let local_ip = local_ip_address::local_ip();
    debug!("local ip: {:?}", local_ip);
    let ip4 = local_ip.ok().map(|ip| ip.to_string());

    let local_ipv6 = local_ip_address::local_ipv6();
    debug!("local ip6: {:?}", local_ipv6);
    let ip6 = local_ip_address::local_ipv6().ok().map(|ip| ip.to_string());
    info!("local ip4: {:?}", ip4);
    info!("local ip6: {:?}", ip6);

    let client = Client::try_default().await?;
    let api_rt = Api::<Router>::namespaced(client.clone(), &network_namespace);
    let api_cert = Api::<Certificate>::namespaced(client.clone(), &network_namespace);
    let api_secret = Api::<Secret>::namespaced(client.clone(), &network_namespace);

    let trust_anchor_refs: Option<Vec<TrustAnchorRef>> = env::var("NDN_TRUST_ANCHORS")
        .ok()
        .map(|raw| {
            serde_json::from_str(&raw)
                .map_err(|e| Error::OtherError(format!("Failed to parse NDN_TRUST_ANCHORS: {e}")))
        })
        .transpose()?;
    let resolved_trust_anchors = if let Some(anchors) = trust_anchor_refs {
        Some(
            try_join_all(anchors.into_iter().map(|ta| {
                let client = client.clone();
                let ns = network_namespace.clone();
                async move { resolve_trust_anchor(ta, client, ns).await }
            }))
            .await?,
        )
    } else {
        None
    };

    let tcp_port = env::var("NDN_TCP_PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok());
    let ws_port = env::var("NDN_WS_PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok());
    let preferred_ip_family = match env::var("NDN_IP_FAMILY").ok().as_deref() {
        Some("IPv6") => IpFamily::IPv6,
        _ => IpFamily::IPv4,
    };

    // Generate Ndnd config
    let config = gen_config(GenConfigParams {
        network_name: network_name.clone(),
        router_name: router_name.clone(),
        udp_unicast_port,
        keychain,
        socket_path,
        trust_anchors: resolved_trust_anchors
            .as_ref()
            .map(|anchors| anchors.iter().map(|a| a.cert_name.clone()).collect()),
        tcp_port,
        ws_port,
    });
    let config_str = serde_yaml::to_string(&config)?;
    std::fs::write(args.output, config_str.clone())?;
    info!("{}", config_str);

    info!("Waiting for the router {}...", router_name);
    let created = await_condition(api_rt.clone(), &router_name, is_router_created());
    let _ = tokio::time::timeout(std::time::Duration::from_secs(3), created).await?;

    // If not insecure, wait for certificate to be valid
    if !insecure {
        info!("Waiting for the router certificate to be valid...");
        let cert_valid = await_condition(api_cert.clone(), &certificate_name, is_cert_valid());
        let _ = tokio::time::timeout(std::time::Duration::from_secs(12), cert_valid).await?;
        // Copy cert and keys from the secrets to the keychain directory
        let cert = api_cert
            .get_status(&certificate_name)
            .await
            .map_err(Error::KubeError)?;
        let key_secret_name = cert
            .status
            .clone()
            .and_then(|status| status.key.secret)
            .ok_or(Error::OtherError("Key secret not found".to_string()))?;
        let cert_secret_name =
            cert.status
                .and_then(|status| status.cert.secret)
                .ok_or(Error::OtherError(
                    "Certificate secret not found".to_string(),
                ))?;
        let key_secret_data = load_secret_data(&api_secret, &key_secret_name).await?;
        let cert_secret_data = load_secret_data(&api_secret, &cert_secret_name).await?;
        let key_text =
            secret_value_utf8(&key_secret_data, &key_secret_name, Certificate::SECRET_KEY)?;
        let cert_text =
            secret_value_utf8(&cert_secret_data, &cert_secret_name, Certificate::CERT_KEY)?;
        let signer_cert_text = secret_value_utf8(
            &cert_secret_data,
            &cert_secret_name,
            Certificate::SIGNER_CERT_KEY,
        )?;
        // Write key and cert to the keychain directory
        let key_path = format!("{keychain_dir}/ndn.key");
        let cert_path = format!("{keychain_dir}/ndn.cert");
        let signer_cert_path = format!("{keychain_dir}/signer.cert");
        std::fs::write(&key_path, key_text)?;
        std::fs::write(&cert_path, cert_text)?;
        std::fs::write(&signer_cert_path, signer_cert_text)?;
        info!(
            "Key and certificate written to {} and {}",
            key_path, cert_path
        );

        if let Some(trust_anchors) = &resolved_trust_anchors {
            for anchor in trust_anchors {
                let secret_api = Api::<Secret>::namespaced(client.clone(), &anchor.namespace);
                let trust_anchor_secret =
                    load_secret_data(&secret_api, &anchor.cert_secret_name).await?;
                let trust_anchor_cert = secret_value_utf8(
                    &trust_anchor_secret,
                    &anchor.cert_secret_name,
                    Certificate::CERT_KEY,
                )?;
                let anchor_path = format!(
                    "{keychain_dir}/trust-anchor-{}-{}.cert",
                    anchor.namespace, anchor.resource_name
                );
                std::fs::write(&anchor_path, trust_anchor_cert)?;
                info!(
                    "Trust anchor certificate `{}` written to {}",
                    anchor.cert_name, anchor_path
                );
            }
        }
    }

    // Patch the status of the existing router
    // Decide which UDP face to publish based on ip family preference.
    // We only publish ONE UDP face per router to prevent duplicate inter-router links.
    // Preference is driven by Network.spec.ipFamily with a graceful fallback to the other family if the preferred IP is absent on the node.
    let (udp4_face, udp6_face) = match preferred_ip_family {
        IpFamily::IPv4 => {
            // Prefer IPv4; if not available, fallback to IPv6
            if let Some(ip) = ip4.clone() {
                (Some(format!("udp://{ip}:{udp_unicast_port}")), None)
            } else {
                (
                    None,
                    ip6.clone()
                        .map(|ip| format!("udp://[{ip}]:{udp_unicast_port}")),
                )
            }
        }
        IpFamily::IPv6 => {
            // Prefer IPv6; if not available, fallback to IPv4
            if let Some(ip) = ip6.clone() {
                (None, Some(format!("udp://[{ip}]:{udp_unicast_port}")))
            } else {
                (
                    ip4.clone()
                        .map(|ip| format!("udp://{ip}:{udp_unicast_port}")),
                    None,
                )
            }
        }
    };
    // Compute the single inner_face value to publish
    let inner_face: Option<String> = match (udp4_face, udp6_face) {
        (Some(f), None) => Some(f),
        (None, Some(f)) => Some(f),
        _ => None,
    };
    // Patch only intended fields to avoid resetting other status fields (e.g., online, conditions)
    let patch_status = json!({
        "status": {
            "innerFace": inner_face,
            "initialized": true
        }
    });
    debug!("Patch status: {:?}", patch_status);
    let pp = PatchParams::default();
    let router = api_rt
        .patch_status(&router_name, &pp, &Patch::Merge(patch_status))
        .await
        .map_err(Error::KubeError)?;
    info!("Patched router status: {:?}", router.status);

    Ok(())
}
