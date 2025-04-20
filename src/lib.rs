use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("SerializationError: {0}")]
    SerializationError(#[source] serde_json::Error),

    #[error("Kube Error: {0}")]
    KubeError(#[source] kube::Error),

    #[error("IO Error: {0}")]
    IoError(std::io::Error),

    #[error("Finalizer Error: {0}")]
    // NB: awkward type because finalizer::Error embeds the reconciler error (which is this)
    // so boxing this error to break cycles
    FinalizerError(#[source] Box<kube::runtime::finalizer::Error<Error>>),

    #[error("Missing Label: {0}")]
    MissingLabel(String),
    
    #[error("Missing Annotation: {0}")]
    MissingAnnotation(String),

    /// NB: this is a catch-all for any other errors
    #[error("Other Error: {0}")]
    OtherError(String),
}
pub type Result<T, E = Error> = std::result::Result<T, E>;

mod ndnd;
pub mod controller;
pub use crate::ndnd::*;

/// Log and trace integrations
pub mod telemetry;