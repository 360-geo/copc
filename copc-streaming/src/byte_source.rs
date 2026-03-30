use std::future::Future;

use crate::error::CopcError;

/// Async random-access byte source.
///
/// Implementations can back this with HTTP range requests, local file I/O,
/// in-memory buffers, or any other random-access mechanism.
///
/// All methods return non-Send futures for WASM compatibility.
pub trait ByteSource {
    /// Read `length` bytes starting at `offset`.
    fn read_range(
        &self,
        offset: u64,
        length: u64,
    ) -> impl Future<Output = Result<Vec<u8>, CopcError>>;

    /// Total size of the source in bytes, if known.
    fn size(&self) -> impl Future<Output = Result<Option<u64>, CopcError>>;

    /// Read multiple ranges in one logical operation.
    ///
    /// The default implementation issues sequential reads.
    /// HTTP implementations should override to issue parallel requests.
    fn read_ranges(
        &self,
        ranges: &[(u64, u64)],
    ) -> impl Future<Output = Result<Vec<Vec<u8>>, CopcError>> {
        async move {
            let mut results = Vec::with_capacity(ranges.len());
            for &(offset, length) in ranges {
                results.push(self.read_range(offset, length).await?);
            }
            Ok(results)
        }
    }
}

/// In-memory byte source, useful for testing and when data is already loaded.
impl ByteSource for Vec<u8> {
    async fn read_range(&self, offset: u64, length: u64) -> Result<Vec<u8>, CopcError> {
        let start = offset as usize;
        let end = start + length as usize;
        if end > self.len() {
            return Err(CopcError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!(
                    "read_range({offset}, {length}) out of bounds (size {})",
                    self.len()
                ),
            )));
        }
        Ok(self[start..end].to_vec())
    }

    async fn size(&self) -> Result<Option<u64>, CopcError> {
        Ok(Some(self.len() as u64))
    }
}

/// Byte source over a shared slice reference.
impl ByteSource for &[u8] {
    async fn read_range(&self, offset: u64, length: u64) -> Result<Vec<u8>, CopcError> {
        let start = offset as usize;
        let end = start + length as usize;
        if end > self.len() {
            return Err(CopcError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!(
                    "read_range({offset}, {length}) out of bounds (size {})",
                    self.len()
                ),
            )));
        }
        Ok(self[start..end].to_vec())
    }

    async fn size(&self) -> Result<Option<u64>, CopcError> {
        Ok(Some(self.len() as u64))
    }
}
