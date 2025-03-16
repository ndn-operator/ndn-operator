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

impl ForwarderConfig {
    pub fn new() -> Self {
        Self {
            core: Some(CoreConfig::new()),
            faces: FacesConfig::new(),
            fw: FwConfig::new(),
            mgmt: Some(MgmtConfig::new()),
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

impl CoreConfig {
    pub fn new() -> Self {
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

impl FacesConfig {
    pub fn new() -> Self {
        Self {
            queue_size: Some(1024),
            congestion_marking: Some(true),
            lock_threads_to_cores: Some(false),
            udp: Some(UdpConfig::new()),
            tcp: None,
            unix: Some(UnixConfig::new()),
            websocket: None,
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

impl UdpConfig {
    pub fn new() -> Self {
        Self {
            enabled_unicast: true,
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

impl TcpConfig {
    pub fn new() -> Self {
        Self {
            enabled: true,
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

impl UnixConfig {
    pub fn new() -> Self {
        Self {
            enabled: true,
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

impl WebSocketConfig {
    pub fn new() -> Self {
        Self {
            enabled: true,
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

impl FwConfig {
    pub fn new() -> Self {
        Self {
            threads: Some(8),
            queue_size: Some(1024),
            lock_threads_to_cores: false,
        }
    }
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Debug)]
pub struct MgmtConfig {
    pub allow_localhop: bool,
}

impl MgmtConfig {
    pub fn new() -> Self {
        Self {
            allow_localhop: false,
        }
    }
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

impl TablesConfig {
    pub fn new() -> Self {
        Self {
            content_store: ContentStoreConfig::new(),
            dead_nonce_list: Some(DeadNonceListConfig::new()),
            network_region: Some(NetworkRegionConfig::new()),
            rib: RibConfig::new(),
            fib: FibConfig::new(),
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

impl ContentStoreConfig {
    pub fn new() -> Self {
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

impl DeadNonceListConfig {
    pub fn new() -> Self {
        Self {
            lifetime: 6000,
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct NetworkRegionConfig {
    pub regions: Vec<String>,
}

impl NetworkRegionConfig {
    pub fn new() -> Self {
        Self {
            regions: Vec::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct RibConfig {
    pub readvertise_nlsr: bool,
}

impl RibConfig {
    pub fn new() -> Self {
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

impl FibConfig {
    pub fn new() -> Self {
        Self {
            algorithm: "nametree".to_string(),
            hashtable: HashtableConfig::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct HashtableConfig {
    pub m: u16,
}

impl HashtableConfig {
    pub fn new() -> Self {
        Self {
            m: 5,
        }
    }
}