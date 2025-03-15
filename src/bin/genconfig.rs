extern crate controller;
use controller::NdndConfig;

fn main() {
    let config = NdndConfig::new("ndn".to_string(), "router".to_string());
    println!("{}", serde_yaml::to_string(&config).unwrap());
}