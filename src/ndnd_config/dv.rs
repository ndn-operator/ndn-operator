use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Debug)]
pub struct RouterConfig {
    pub network: String,
    pub router: String,
    pub advertise_interval: Option<u64>,
    pub router_dead_interval: Option<u64>,
    pub keychain: String,
    pub trust_anchors: Option<Vec<String>>,
    pub neighbors: Option<Vec<Neighbor>>,
}

impl RouterConfig {
    pub fn new(network: String, router: String) -> Self {
        Self {
            network: network,
            router: router,
            advertise_interval: None,
            router_dead_interval: None,
            keychain: "insecure".to_string(),
            trust_anchors: None,
            neighbors: None,
        }
    }
    
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Debug)]
pub struct Neighbor {
    pub uri: String,
    pub mtu: Option<u64>,
}

