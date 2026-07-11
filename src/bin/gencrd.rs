use kube::CustomResourceExt;
use operator::{
    cert_controller::{Certificate, ExternalCertificate},
    neighbor_controller::Neighbor,
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
    std::fs::create_dir_all(&args.output).unwrap();
    for (file, crd) in [
        ("network.yaml", Network::crd()),
        ("router.yaml", Router::crd()),
        ("certificate.yaml", Certificate::crd()),
        ("external_certificate.yaml", ExternalCertificate::crd()),
        ("neighbor.yaml", Neighbor::crd()),
    ] {
        std::fs::write(
            format!("{}/{file}", args.output),
            serde_yaml::to_string(&crd).unwrap(),
        )
        .unwrap();
    }
}
