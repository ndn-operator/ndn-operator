use kube::CustomResourceExt;
use controller::crd::Network;
fn main() {
    print!("{}", serde_yaml::to_string(&Network::crd()).unwrap())
}