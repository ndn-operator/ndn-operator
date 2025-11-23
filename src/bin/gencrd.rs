use kube::CustomResourceExt;
use operator::{
    cert_controller::{Certificate, ExternalCertificate},
    neighbor_link_controller::NeighborLink,
    network_controller::Network,
    router_controller::Router,
};

use clap::Parser;
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    // Output directory
    #[arg(short, long, default_value = ".")]
    output: String,
}
fn main() {
    let args = Args::parse();
    // Create directory if it does not exist
    std::fs::create_dir_all(&args.output).unwrap();
    std::fs::write(
        format!("{}/network.yaml", args.output),
        serde_yaml::to_string(&Network::crd()).unwrap(),
    )
    .unwrap();
    std::fs::write(
        format!("{}/router.yaml", args.output),
        serde_yaml::to_string(&Router::crd()).unwrap(),
    )
    .unwrap();
    std::fs::write(
        format!("{}/certificate.yaml", args.output),
        serde_yaml::to_string(&Certificate::crd()).unwrap(),
    )
    .unwrap();
    std::fs::write(
        format!("{}/external_certificate.yaml", args.output),
        serde_yaml::to_string(&ExternalCertificate::crd()).unwrap(),
    )
    .unwrap();
    std::fs::write(
        format!("{}/neighbor_link.yaml", args.output),
        serde_yaml::to_string(&NeighborLink::crd()).unwrap(),
    )
    .unwrap();
}
