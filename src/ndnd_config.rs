mod dv;
use dv::RouterConfig;
mod fw;
use fw::ForwarderConfig;

use serde::Serialize;
use serde_with::skip_serializing_none;

#[skip_serializing_none]
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