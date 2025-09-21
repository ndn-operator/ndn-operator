use clap::Parser;
use futures::future::try_join_all;
use k8s_openapi::api::core::v1::Secret;
use kube::{
    Client,
    api::{Api, Patch, PatchParams},
    runtime::wait::await_condition,
};
use operator::{
    Error, NdndConfig,
    cert_controller::{Certificate, is_cert_valid},
    dv::RouterConfig,
    fw::{FacesConfig, ForwarderConfig, TcpConfig, UdpConfig, UnixConfig, WebSocketConfig},
    helper::{Decoded, decode_secret},
    network_controller::{IpFamily, Network, Router, is_router_created},
    telemetry,
};
use serde_json::json;
use std::env;
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
                tcp: tcp_port.map(|p| TcpConfig {
                    enabled: true,
                    port_unicast: p,
                    ..TcpConfig::default()
                }),
                unix: Some(UnixConfig {
                    enabled: true,
                    socket_path: socket_path.unwrap_or("/run/nfd/nfd.sock".to_string()),
                }),
                websocket: ws_port.map(|p| WebSocketConfig {
                    enabled: true,
                    bind: "0.0.0.0".to_string(),
                    port: p,
                    tls_enabled: false,
                    tls_cert: None,
                    tls_key: None,
                }),
                ..FacesConfig::default()
            },
            ..ForwarderConfig::default()
        },
    }
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
    let api_nw = Api::<Network>::namespaced(client.clone(), &network_namespace);
    let api_secret = Api::<Secret>::namespaced(client.clone(), &network_namespace);

    let my_network = api_nw.get(&network_name).await.map_err(Error::KubeError)?;
    // TODO: refactor
    let trust_anchors = if let Some(anchors) = my_network.spec.trust_anchors.clone() {
        Some(
            try_join_all(anchors.into_iter().map(async |ta| {
                let client = client.clone();
                let network_namespace = network_namespace.clone();
                ta.get_cert_name(&client, &network_namespace).await
            }))
            .await?,
        )
    } else {
        None
    };

    // Determine tcp/websocket ports from Network faces
    let tcp_port: Option<u16> = my_network
        .spec
        .faces
        .as_ref()
        .and_then(|f| f.tcp.as_ref().map(|t| t.port));
    let ws_port: Option<u16> = my_network
        .spec
        .faces
        .as_ref()
        .and_then(|f| f.websocket.as_ref().map(|w| w.port));

    // Generate Ndnd config
    let config = gen_config(GenConfigParams {
        network_name: network_name.clone(),
        router_name: router_name.clone(),
        udp_unicast_port,
        keychain,
        socket_path,
        trust_anchors,
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
        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), cert_valid).await?;
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
        let key_secret = decode_secret(
            &api_secret
                .get(&key_secret_name)
                .await
                .map_err(Error::KubeError)?,
        );
        let cert_secret = decode_secret(
            &api_secret
                .get(&cert_secret_name)
                .await
                .map_err(Error::KubeError)?,
        );
        let key_data = key_secret
            .get(Certificate::SECRET_KEY)
            .ok_or(Error::OtherError("Key not found in secret".to_string()))?;
        let cert_data = cert_secret
            .get(Certificate::CERT_KEY)
            .ok_or(Error::OtherError(
                "Certificate not found in secret".to_string(),
            ))?;
        let signer_data =
            cert_secret
                .get(Certificate::SIGNER_CERT_KEY)
                .ok_or(Error::OtherError(
                    "Signer certificate not found in secret".to_string(),
                ))?;
        let signer_cert_text = match signer_data {
            Decoded::Utf8(text) => text,
            Decoded::Bytes(_) => {
                return Err(Error::OtherError(
                    "Signer certificate is not a valid UTF-8 string".to_string(),
                )
                .into());
            }
        };
        let key_text = match key_data {
            Decoded::Utf8(text) => text,
            Decoded::Bytes(_) => {
                return Err(
                    Error::OtherError("Key is not a valid UTF-8 string".to_string()).into(),
                );
            }
        };
        let cert_text = match cert_data {
            Decoded::Utf8(text) => text,
            Decoded::Bytes(_) => {
                return Err(Error::OtherError(
                    "Certificate is not a valid UTF-8 string".to_string(),
                )
                .into());
            }
        };
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
    }

    // Patch the status of the existing router
    // Decide which UDP face to publish based on Network ipFamily.
    // We only publish ONE UDP face per router to prevent duplicate inter-router links.
    // Preference is driven by Network.spec.ipFamily with a graceful fallback to the other family if the preferred IP is absent on the node.
    let (udp4_face, udp6_face) = match my_network.spec.ip_family.unwrap_or(IpFamily::IPv4) {
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
