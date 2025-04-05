
use controller::crd::{create_owned_router, Network, Router, UDP_UNICAST_PORT};
use controller::NdndConfig;
use controller::dv::RouterConfig;
use controller::fw::{ForwarderConfig, FacesConfig, UdpConfig, UnixConfig};
use controller::helper::*;
use clap::Parser;
use kube::api::{Api, Patch, PatchParams};
use kube::Client;
use std::env;
use controller::{Result, Error};

/// Generate config file for ndnd
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    // Output file
    #[arg(short, long)]
    output: String,
}

pub static MANAGER_NAME: &str = "ndnd-init";

async fn create_router(parent: &Network, router_name: String, namespace: &str, client: Client, ip4: Option<String>, ip6: Option<String>) -> Result<Router> {
  let my_pod = get_my_pod(client.clone()).await?;
  let my_node_name = my_pod.spec.expect("Failed to get pod spec").node_name.expect("Failed to get node name");
  let router_data = create_owned_router(parent, router_name.clone(), my_node_name, ip4, ip6, UDP_UNICAST_PORT);
  let api_router = Api::<Router>::namespaced(client, namespace);
  let serverside = PatchParams::apply(MANAGER_NAME);
  api_router
      .patch(&router_name, &serverside, &Patch::Apply(router_data))
      .await
      .map_err(Error::KubeError)
}

fn gen_config(network_name: String, router_name: String, socket_path: Option<String> ) -> NdndConfig {

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
          port_unicast: Some(UDP_UNICAST_PORT),
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
  let args = Args::parse();
  let network_name = env::var("NDN_NETWORK_NAME")?;
  let network_namespace = env::var("NDN_NETWORK_NAMESPACE")?;
  let router_name = env::var("NDN_ROUTER_NAME")?;
  let socket_path = env::var("NDN_SOCKET_PATH").ok();

  let ip4 = match local_ip_address::local_ip().ok() {
    Some(ip) => {Some(ip.to_string())},
    None => None,
  };
  let ip6 = match local_ip_address::local_ipv6().ok() {
    Some(ip) => {Some(ip.to_string())},
    None => None,
  };
    
  // Generate Ndnd config
  let config = gen_config(network_name.clone(), router_name.clone(), socket_path);
  let config_str = serde_yaml::to_string(&config)?;
  std::fs::write(args.output, config_str.clone())?;
  println!("{}", config_str);

  // Create router in the same namespace as the parent network
  let client = Client::try_default().await?;
  let api_nw: Api<Network> = Api::namespaced(client.clone(), &network_namespace);
  let nw = api_nw.get(&network_name).await?;
  let _ = create_router(&nw, router_name, &network_namespace, client, ip4, ip6).await?;
  Ok(())
}