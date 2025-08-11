pub mod dv;
use dv::RouterConfig;
pub mod fw;
use fw::ForwarderConfig;

use serde::Serialize;

#[derive(Serialize)]
pub struct NdndConfig {
    pub dv: RouterConfig,
    pub fw: ForwarderConfig,
}
