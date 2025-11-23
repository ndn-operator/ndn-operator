use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("SerializationError: {0}")]
    SerializationError(#[source] serde_json::Error),

    #[error("Kube Error: {0}")]
    KubeError(#[source] kube::Error),

    #[error("IO Error: {0}")]
    IoError(std::io::Error),

    #[error("UTF-8 Error: {0}")]
    Utf8Error(#[source] std::string::FromUtf8Error),

    #[error("Parse Error: {0}")]
    ParseError(#[source] chrono::ParseError),

    #[error("Finalizer Error: {0}")]
    // NB: awkward type because finalizer::Error embeds the reconciler error (which is this)
    // so boxing this error to break cycles
    FinalizerError(#[source] Box<kube::runtime::finalizer::Error<Error>>),

    #[error("Missing Label: {0}")]
    MissingLabel(String),

    #[error("Missing Annotation: {0}")]
    MissingAnnotation(String),

    #[error("Validation Error: {0}")]
    ValidationError(String),

    /// NB: this is a catch-all for any other errors
    #[error("Other Error: {0}")]
    OtherError(String),
}
pub type Result<T, E = Error> = std::result::Result<T, E>;

pub mod cert_controller;
pub mod conditions;
mod events_helper;
pub mod ext_cert_controller;
pub mod helper;
pub mod macros;
mod ndnd;
pub mod neighbor_controller;
pub mod network_controller;
pub mod pod_controller;
pub mod router_controller;
pub use crate::ndnd::*;
pub use events_helper::*;

/// Log and trace integrations
pub mod telemetry;
