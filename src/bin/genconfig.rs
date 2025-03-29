
use controller::NdndConfig;
use controller::dv::RouterConfig;
use controller::fw::{ForwarderConfig, FacesConfig, UdpConfig, UnixConfig};
use clap::Parser;
use std::env;

/// Generate config file for ndnd
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    // Output file
    #[arg(short, long)]
    output: String,
}

fn main() {
    let args = Args::parse();
    let network_name = env::var("NDN_NETWORK_NAME").unwrap_or("ndn".to_string());
    let router_name = env::var("NDN_ROUTER_NAME").unwrap_or("router".to_string());
    let socket_path = env::var("NDN_SOCKET_PATH").unwrap_or("/run/nfd/nfd.sock".to_string());
    let config = NdndConfig {
        dv: RouterConfig {
            network: format!("/{network_name}" ),
            router: format!("/{network_name}/{router_name}"),
            ..RouterConfig::default()
        },
        fw: ForwarderConfig {
          faces: FacesConfig {
            udp: Some(UdpConfig {
              enabled_unicast: true,
              port_unicast: Some(6363),
              ..UdpConfig::default()
            }),
            unix: Some(UnixConfig {
              enabled: true,
              socket_path: socket_path,
            }),
            ..FacesConfig::default()
          },
          ..ForwarderConfig::default()
        },
    };
    let config_str = serde_yaml::to_string(&config).unwrap();
    std::fs::write(args.output, config_str.clone()).unwrap();
    println!("{}", config_str);
}