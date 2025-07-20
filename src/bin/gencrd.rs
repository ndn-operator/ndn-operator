use kube::CustomResourceExt;
use operator::{
  network_controller::{Network, Router},
  cert_controller::Certificate,
};

use clap::Parser;
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
  // Output directory
  #[arg(short, long, default_value = ".")]
    output: String
}
fn main() {
    let args = Args::parse();
    // Create directory if it does not exist
    std::fs::create_dir_all(&args.output).unwrap();
    std::fs::write(format!("{}/network.yaml", args.output), serde_yaml::to_string(&Network::crd()).unwrap()).unwrap();
    std::fs::write(format!("{}/router.yaml", args.output), serde_yaml::to_string(&Router::crd()).unwrap()).unwrap();
    std::fs::write(format!("{}/certificate.yaml", args.output), serde_yaml::to_string(&Certificate::crd()).unwrap()).unwrap();
}
