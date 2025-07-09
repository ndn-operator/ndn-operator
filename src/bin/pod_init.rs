use clap::Parser;
use kube::{
  api::{Api, Patch, PatchParams},
  runtime::wait::await_condition,
  Client,
};
use operator::{
  network_controller::{
    is_router_created, Router, RouterFaces, RouterStatus,
  },
  dv::RouterConfig,
  fw::{FacesConfig, ForwarderConfig, UdpConfig, UnixConfig},
  telemetry, Error, NdndConfig,
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

fn gen_config(network_name: String, router_name: String, udp_unicast_port: i32, socket_path: Option<String> ) -> NdndConfig {

  NdndConfig {
    dv: RouterConfig {
        network: format!("/{network_name}" ),
        router: format!("/{network_name}/{router_name}"),
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
  let udp_unicast_port = env::var("NDN_UDP_UNICAST_PORT")?.parse::<i32>()?;
  let socket_path = env::var("NDN_SOCKET_PATH").ok();

  let local_ip = local_ip_address::local_ip();
  debug!("local ip: {:?}", local_ip);
  let ip4 = local_ip.ok().map(|ip| ip.to_string());

  let local_ipv6 = local_ip_address::local_ipv6();
  debug!("local ip6: {:?}", local_ipv6);
  let ip6 = local_ip_address::local_ipv6().ok().map(|ip| ip.to_string());
  info!("local ip4: {:?}", ip4);
  info!("local ip6: {:?}", ip6);
  // Generate Ndnd config
  let config = gen_config(network_name.clone(), router_name.clone(), udp_unicast_port, socket_path);
  let config_str = serde_yaml::to_string(&config)?;
  std::fs::write(args.output, config_str.clone())?;
  info!("{}", config_str);

  // Wait for the router to be created
  info!("Waiting for the router {}...", router_name);
  let client = Client::try_default().await?;
  let api_rt = Api::<Router>::namespaced(client.clone(), &network_namespace);
  let created = await_condition(
    api_rt.clone(),
    &router_name,
    is_router_created()
  );
  let _ = tokio::time::timeout(std::time::Duration::from_secs(10), created).await?;

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
