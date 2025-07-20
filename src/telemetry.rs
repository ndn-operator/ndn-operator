use tracing_subscriber::{prelude::*, EnvFilter, Registry};

/// Initialize tracing
pub async fn init() {

    let logger = tracing_subscriber::fmt::layer().json();
    let env_filter = EnvFilter::try_from_env("LOG")
        .or(EnvFilter::try_new("info"))
        .unwrap();

    // Decide on layers
    let reg = Registry::default();
    reg.with(env_filter).with(logger).init();
}
