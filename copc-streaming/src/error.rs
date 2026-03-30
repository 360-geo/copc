use thiserror::Error;

/// Errors that can occur when reading a COPC file.
#[derive(Debug, Error)]
pub enum CopcError {
    /// I/O error from the underlying byte source.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Error parsing LAS header or VLR data.
    #[error("LAS error: {0}")]
    Las(#[from] las::Error),

    /// Error during LAZ decompression.
    #[error("LAZ error: {0}")]
    Laz(#[from] laz::LasZipError),

    /// The required COPC info VLR was not found.
    #[error("COPC info VLR not found")]
    CopcInfoNotFound,

    /// The required LAZ VLR was not found.
    #[error("LAZ VLR not found")]
    LazVlrNotFound,

    /// A hierarchy page was shorter than expected.
    #[error("hierarchy page at offset {offset} truncated")]
    TruncatedHierarchyPage {
        /// File offset where the truncated page starts.
        offset: u64,
    },

    /// The requested node is not in the loaded hierarchy.
    #[error("node not found in hierarchy: {0:?}")]
    NodeNotFound(crate::types::VoxelKey),

    /// Custom error from a [`ByteSource`](crate::ByteSource) implementation.
    #[error("byte source error: {0}")]
    ByteSource(Box<dyn std::error::Error + Send + Sync>),
}
