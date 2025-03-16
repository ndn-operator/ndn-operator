mod dv;
use dv::RouterConfig;
mod fw;
use fw::ForwarderConfig;

use serde::Serialize;

#[derive(Serialize)]
pub struct NdndConfig {
    pub dv: RouterConfig,
    pub fw: ForwarderConfig,
}

impl NdndConfig {
    pub fn new(network: String, router: String) -> Self {
        Self {
            dv: RouterConfig::new(network, router),
            fw: ForwarderConfig::new(),
        }
    }
}