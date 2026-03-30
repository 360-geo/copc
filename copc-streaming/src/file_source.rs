//! File-based ByteSource for local file access.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Mutex;

use crate::byte_source::ByteSource;
use crate::error::CopcError;

/// A ByteSource backed by a local file.
///
/// Uses a Mutex for interior mutability since Read+Seek requires &mut self
/// but ByteSource's read_range takes &self.
pub struct FileSource {
    file: Mutex<std::fs::File>,
    size: u64,
}

impl FileSource {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, CopcError> {
        let file = std::fs::File::open(path)?;
        let size = file.metadata()?.len();
        Ok(Self {
            file: Mutex::new(file),
            size,
        })
    }
}

impl ByteSource for FileSource {
    async fn read_range(&self, offset: u64, length: u64) -> Result<Vec<u8>, CopcError> {
        let mut file = self.file.lock().unwrap();
        file.seek(SeekFrom::Start(offset))?;
        let mut buf = vec![0u8; length as usize];
        file.read_exact(&mut buf)?;
        Ok(buf)
    }

    async fn size(&self) -> Result<Option<u64>, CopcError> {
        Ok(Some(self.size))
    }
}
