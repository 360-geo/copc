use thiserror::Error;

#[derive(Debug, Error)]
pub enum TemporalError {
    #[error("temporal index EVLR header is truncated")]
    TruncatedHeader,

    #[error("unsupported temporal index version: {0}")]
    UnsupportedVersion(u32),

    #[error("invalid stride: {0} (must be >= 1)")]
    InvalidStride(u32),

    #[error("I/O error reading temporal index: {0}")]
    Io(#[from] std::io::Error),

    #[error("COPC streaming error: {0}")]
    Copc(#[from] copc_streaming::CopcError),
}
