extern crate controller;
use controller::NdndConfig;
use clap::Parser;

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
    let config = NdndConfig::new("ndn".to_string(), "router".to_string());
    let config_str = serde_yaml::to_string(&config).unwrap();
    std::fs::write(args.output, config_str).unwrap();
}