use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Debug)]
pub struct ForwarderConfig {
    pub core: Option<CoreConfig>,
    pub faces: FacesConfig,
    pub fw: FwConfig,
    pub mgmt: Option<MgmtConfig>,
    pub tables: Option<TablesConfig>,
}

impl Default for ForwarderConfig {
    fn default() -> Self {
        Self {
            core: Some(CoreConfig::default()),
            faces: FacesConfig::default(),
            fw: FwConfig::default(),
            mgmt: Some(MgmtConfig::default()),
            tables: None,
        }
    }
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Debug)]
pub struct CoreConfig {
    pub log_level: String,
    pub log_file: Option<String>,
}

impl Default for CoreConfig {
    fn default() -> Self {
        Self {
            log_level: "INFO".to_string(),
            log_file: None,
        }
    }
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Debug)]
pub struct FacesConfig {
    pub queue_size: Option<i32>,
    pub congestion_marking: Option<bool>,
    pub lock_threads_to_cores: Option<bool>,
    pub udp: Option<UdpConfig>,
    pub tcp: Option<TcpConfig>,
    pub unix: Option<UnixConfig>,
    pub websocket: Option<WebSocketConfig>,
}

impl Default for FacesConfig {
    fn default() -> Self {
        Self {
            queue_size: Some(1024),
            congestion_marking: Some(true),
            lock_threads_to_cores: Some(false),
            udp: Some(UdpConfig::default()),
            tcp: Some(TcpConfig::default()),
            unix: Some(UnixConfig::default()),
            websocket: Some(WebSocketConfig::default()),
        }
    }
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Debug)]
pub struct UdpConfig {
    pub enabled_unicast: bool,
    pub enabled_multicast: bool,
    pub port_unicast: Option<u16>,
    pub port_multicast: Option<u16>,
    pub multicast_address_ipv4: Option<String>,
    pub multicast_address_ipv6: Option<String>,
    pub lifetime: Option<u64>,
    pub default_mtu: Option<u16>,
}

impl Default for UdpConfig {
    fn default() -> Self {
        Self {
            enabled_unicast: false,
            enabled_multicast: false,
            port_unicast: Some(6363),
            port_multicast: None,
            multicast_address_ipv4: None,
            multicast_address_ipv6: None,
            lifetime: Some(600),
            default_mtu: Some(1420),
        }
    }
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Debug)]
pub struct TcpConfig {
    pub enabled: bool,
    pub port_unicast: u16,
    pub lifetime: Option<u64>,
    pub reconnect_interval: Option<u64>,
}

impl Default for TcpConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port_unicast: 6363,
            lifetime: Some(600),
            reconnect_interval: Some(10),
        }
    }
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Debug)]
pub struct UnixConfig {
    pub enabled: bool,
    pub socket_path: String,
}

impl Default for UnixConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            socket_path: "/run/nfd/nfd.sock".to_string(),
        }
    }
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Debug)]
pub struct WebSocketConfig {
    pub enabled: bool,
    pub bind: String,
    pub port: u16,
    pub tls_enabled: bool,
    pub tls_cert: Option<String>,
    pub tls_key: Option<String>,
}

impl Default for WebSocketConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: "".to_string(),
            port: 9696,
            tls_enabled: false,
            tls_cert: None,
            tls_key: None,
        }
    }
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Debug)]
pub struct FwConfig {
    pub threads: Option<i32>,
    pub queue_size: Option<i32>,
    pub lock_threads_to_cores: bool,
}

impl Default for FwConfig {
    fn default() -> Self {
        Self {
            threads: Some(8),
            queue_size: Some(1024),
            lock_threads_to_cores: false,
        }
    }
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Default)]
pub struct MgmtConfig {
    pub allow_localhop: bool,
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Debug)]
pub struct TablesConfig {
    pub content_store: ContentStoreConfig,
    pub dead_nonce_list: Option<DeadNonceListConfig>,
    pub network_region: Option<NetworkRegionConfig>,
    pub rib: RibConfig,
    pub fib: FibConfig,
}

impl Default for TablesConfig {
    fn default() -> Self {
        Self {
            content_store: ContentStoreConfig::default(),
            dead_nonce_list: Some(DeadNonceListConfig::default()),
            network_region: Some(NetworkRegionConfig::default()),
            rib: RibConfig::default(),
            fib: FibConfig::default(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ContentStoreConfig {
    pub capacity: u16,
    pub admit: bool,
    pub serve: bool,
    pub replacement_policy: String,
}

impl Default for ContentStoreConfig {
    fn default() -> Self {
        Self {
            capacity: 1024,
            admit: true,
            serve: true,
            replacement_policy: "lru".to_string(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct DeadNonceListConfig {
    pub lifetime: i32,
}

impl Default for DeadNonceListConfig {
    fn default() -> Self {
        Self { lifetime: 6000 }
    }
}

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct NetworkRegionConfig {
    pub regions: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct RibConfig {
    pub readvertise_nlsr: bool,
}

impl Default for RibConfig {
    fn default() -> Self {
        Self {
            readvertise_nlsr: true,
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct FibConfig {
    pub algorithm: String,
    pub hashtable: HashtableConfig,
}

impl Default for FibConfig {
    fn default() -> Self {
        Self {
            algorithm: "nametree".to_string(),
            hashtable: HashtableConfig::default(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct HashtableConfig {
    pub m: u16,
}

impl Default for HashtableConfig {
    fn default() -> Self {
        Self { m: 5 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwarder_config_default_sections_enabled() {
        let cfg = ForwarderConfig::default();
        assert!(cfg.core.is_some());
        assert!(cfg.mgmt.is_some());
        assert!(cfg.faces.tcp.is_some());
        assert!(cfg.faces.udp.is_some());
        assert_eq!(cfg.fw.queue_size, Some(1024));
    }

    #[test]
    fn udp_and_websocket_defaults_match_expected_values() {
        let faces = FacesConfig::default();
        let udp = faces.udp.as_ref().unwrap();
        assert_eq!(udp.port_unicast, Some(6363));
        assert_eq!(udp.enabled_unicast, false);
        let ws = faces.websocket.as_ref().unwrap();
        assert_eq!(ws.port, 9696);
        assert!(!ws.tls_enabled);
    }
}
