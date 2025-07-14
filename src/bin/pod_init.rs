use clap::Parser;
use k8s_openapi::api::core::v1::Secret;
use kube::{
  api::{Api, Patch, PatchParams},
  runtime::wait::await_condition,
  Client,
};
use futures::future::try_join_all;
use operator::{
  cert_controller::{is_cert_valid, Certificate}, dv::RouterConfig, fw::{FacesConfig, ForwarderConfig, UdpConfig, UnixConfig}, helper::{decode_secret, Decoded}, network_controller::{
	is_router_created, Network, Router, RouterFaces, RouterStatus
  }, telemetry, Error, NdndConfig
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

fn gen_config(
  network_name: String,
  router_name: String,
  udp_unicast_port: i32,
  keychain: String,
  socket_path: Option<String>,
  trust_anchors: Option<Vec<String>>,
) -> NdndConfig {
  NdndConfig {
	dv: RouterConfig {
		network: format!("/{network_name}" ),
		router: format!("/{network_name}/{router_name}"),
		keychain: keychain,
		trust_anchors: trust_anchors,
		..RouterConfig::default()
	},
	fw: ForwarderConfig {
	  faces: FacesConfig {
		udp: Some(UdpConfig {
		  enabled_unicast: true,
		  port_unicast: Some(udp_unicast_port),
		  ..UdpConfig::default()
		}),
		unix: Some(UnixConfig {
		  enabled: true,
		  socket_path: socket_path.unwrap_or("/run/nfd/nfd.sock".to_string()),
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
    let udp_unicast_port = env::var("NDN_UDP_UNICAST_PORT")?.parse::<i32>()?;
    let socket_path = env::var("NDN_SOCKET_PATH").ok();
    let keychain_dir = env::var("NDN_KEYS_DIR")?;
    let insecure = env::var("NDN_INSECURE")?.parse::<bool>()?;
    let keychain = match insecure {
        true => format!("dir://{keychain_dir}"),
        false => "insecure".to_string(),
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

  // Generate Ndnd config
  let config = gen_config(
	network_name.clone(),
	router_name.clone(),
	udp_unicast_port,
	keychain,
	socket_path,
	trust_anchors,
  );
  let config_str = serde_yaml::to_string(&config)?;
  std::fs::write(args.output, config_str.clone())?;
  info!("{}", config_str);


  info!("Waiting for the router {}...", router_name);
  let created = await_condition(
	api_rt.clone(),
	&router_name,
	is_router_created()
  );
  let _ = tokio::time::timeout(std::time::Duration::from_secs(3), created).await?;

  // If not insecure, wait for certificate to be valid
  if !insecure {
	info!("Waiting for the router certificate to be valid...");
	let cert_valid = await_condition(
	  api_cert.clone(),
	  &certificate_name,
	  is_cert_valid()
	);
	let _ = tokio::time::timeout(std::time::Duration::from_secs(3), cert_valid).await?;
	// Copy cert and keys from the secrets to the keychain directory
	let cert = api_cert.get_status(&certificate_name).await.map_err(Error::KubeError)?;
	let key_secret_name = cert.status.clone()
	  .and_then(|status| status.key.secret)
	  .ok_or(Error::OtherError("Key secret not found".to_string()))?;
	let cert_secret_name = cert.status
	  .and_then(|status| status.cert.secret)
	  .ok_or(Error::OtherError("Certificate secret not found".to_string()))?;
	let key_secret = decode_secret(&api_secret.get(&key_secret_name).await.map_err(Error::KubeError)?);
	let cert_secret = decode_secret(&api_secret.get(&cert_secret_name).await.map_err(Error::KubeError)?);
	let key_data = key_secret.get(Certificate::SECRET_KEY)
	  .ok_or(Error::OtherError("Key not found in secret".to_string()))?;
	let cert_data = cert_secret.get(Certificate::CERT_KEY)
	  .ok_or(Error::OtherError("Certificate not found in secret".to_string()))?;
	let signer_data = cert_secret.get(Certificate::SIGNER_CERT_KEY)
	  .ok_or(Error::OtherError("Signer certificate not found in secret".to_string()))?;
	let signer_cert_text = match signer_data {
	  Decoded::Utf8(text) => text,
	  Decoded::Bytes(_) => return Err(Error::OtherError("Signer certificate is not a valid UTF-8 string".to_string()).into()),
	};
	let key_text = match key_data {
	  Decoded::Utf8(text) => text,
	  Decoded::Bytes(_) => return Err(Error::OtherError("Key is not a valid UTF-8 string".to_string()).into()),
	};
	let cert_text = match cert_data {
	  Decoded::Utf8(text) => text,
	  Decoded::Bytes(_) => return Err(Error::OtherError("Certificate is not a valid UTF-8 string".to_string()).into()),
	};
	// Write key and cert to the keychain directory
	let key_path = format!("{}/ndn.key", keychain_dir);
	let cert_path = format!("{}/ndn.cert", keychain_dir);
	let signer_cert_path = format!("{}/signer.cert", keychain_dir);
	std::fs::write(&key_path, key_text)?;
	std::fs::write(&cert_path, cert_text)?;
	std::fs::write(&signer_cert_path, signer_cert_text)?;
	info!("Key and certificate written to {} and {}", key_path, cert_path);
  }

  // Patch the status of the existing router
  let faces = RouterFaces {
	udp4: {
		ip4.map(|ip4| format!("udp://{ip4}:{udp_unicast_port}"))
	},
	tcp4: None,
	udp6: {
		ip6.map(|ip6| format!("udp://[{ip6}]:{udp_unicast_port}"))
	},
	tcp6: None,
  };
  let patch_status = json!({
	"status": RouterStatus {
	  faces,
	  initialized: true,
	  ..RouterStatus::default()
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
