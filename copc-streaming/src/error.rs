use thiserror::Error;

#[derive(Debug, Error)]
pub enum CopcError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("LAS error: {0}")]
    Las(#[from] las::Error),

    #[error("LAZ error: {0}")]
    Laz(#[from] laz::LasZipError),

    #[error("COPC info VLR not found")]
    CopcInfoNotFound,

    #[error("LAZ VLR not found")]
    LazVlrNotFound,

    #[error("hierarchy page at offset {offset} truncated")]
    TruncatedHierarchyPage { offset: u64 },

    #[error("byte source error: {0}")]
    ByteSource(Box<dyn std::error::Error + Send + Sync>),
}
