
use operator::{
  Result,
  Error,
  controller::{create_owned_router, Network, Router, ROUTER_MANAGER_NAME},
  telemetry, NdndConfig,
  fw::{ForwarderConfig, FacesConfig, UdpConfig, UnixConfig},
  dv::RouterConfig,
};
use clap::Parser;
use kube::api::{Api, Patch, PatchParams};
use kube::Client;
use std::env;

/// Generate config file for ndnd
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    // Output file
    #[arg(short, long)]
    output: String,
}

struct CreateRouterParams {
  router_name: String,
  namespace: String,
  node_name: String,
  ip4: Option<String>,
  ip6: Option<String>,
}

async fn create_router(parent: &Network, client: Client, params: CreateRouterParams) -> Result<Router> {
  let router_data = create_owned_router(
    parent,
    params.router_name.clone(),
    params.node_name,
    params.ip4,
    params.ip6,
    parent.spec.udp_unicast_port);
  let api_router = Api::<Router>::namespaced(client, &params.namespace);
  let serverside = PatchParams::apply(ROUTER_MANAGER_NAME);
  api_router
      .patch(&params.router_name, &serverside, &Patch::Apply(router_data))
      .await
      .map_err(Error::KubeError)
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
  let node_name = env::var("NDN_NODE_NAME")?;
  let udp_unicast_port = env::var("NDN_UDP_UNICAST_PORT")?.parse::<i32>()?;
  let socket_path = env::var("NDN_SOCKET_PATH").ok();

  let local_ip = local_ip_address::local_ip();
  println!("local ip: {:?}", local_ip);
  let ip4 = match local_ip.ok() {
    Some(ip) => {Some(ip.to_string())},
    None => None,
  };

  let local_ipv6 = local_ip_address::local_ipv6();
  println!("local ip6: {:?}", local_ipv6);
  let ip6 = match local_ip_address::local_ipv6().ok() {
    Some(ip) => {Some(ip.to_string())},
    None => None,
  };
  println!("local ip4: {:?}", ip4);
  println!("local ip6: {:?}", ip6);
  // Generate Ndnd config
  let config = gen_config(network_name.clone(), router_name.clone(), udp_unicast_port, socket_path);
  let config_str = serde_yaml::to_string(&config)?;
  std::fs::write(args.output, config_str.clone())?;
  println!("{}", config_str);

  // Create router in the same namespace as the parent network
  let client = Client::try_default().await?;
  let api_nw: Api<Network> = Api::namespaced(client.clone(), &network_namespace);
  let nw = api_nw.get(&network_name).await?;
  let _ = create_router(
    &nw,
    client,
    CreateRouterParams {
      router_name: router_name,
      namespace: network_namespace,
      node_name: node_name,
      ip4,
      ip6,
    },
  ).await?;
  Ok(())
}