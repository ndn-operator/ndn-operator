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

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            network: "ndn".to_string(),
            router: "ndnd".to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn router_config_defaults_match_expected_values() {
        let cfg = RouterConfig::default();
        assert_eq!(cfg.network, "ndn");
        assert_eq!(cfg.router, "ndnd");
        assert_eq!(cfg.keychain, "insecure");
        assert!(cfg.trust_anchors.is_none());
        assert!(cfg.neighbors.is_none());
    }

    #[test]
    fn neighbor_allows_optional_mtu() {
        let neighbor = Neighbor {
            uri: "udp://router".into(),
            mtu: Some(9000),
        };
        assert_eq!(neighbor.mtu, Some(9000));
    }
}
