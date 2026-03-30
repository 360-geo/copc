use thiserror::Error;

/// Errors that can occur when reading the temporal index.
#[derive(Debug, Error)]
pub enum TemporalError {
    /// The temporal EVLR header is shorter than 32 bytes.
    #[error("temporal index EVLR header is truncated")]
    TruncatedHeader,

    /// The temporal index version is not supported by this reader.
    #[error("unsupported temporal index version: {0}")]
    UnsupportedVersion(u32),

    /// The stride value in the header is invalid.
    #[error("invalid stride: {0} (must be >= 1)")]
    InvalidStride(u32),

    /// I/O error from the underlying byte source.
    #[error("I/O error reading temporal index: {0}")]
    Io(#[from] std::io::Error),

    /// Error propagated from the COPC streaming reader.
    #[error("COPC streaming error: {0}")]
    Copc(#[from] copc_streaming::CopcError),
}
