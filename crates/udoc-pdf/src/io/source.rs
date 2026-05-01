use crate::{Error, Result};

/// Trait for random-access byte sources.
///
/// PDF parsing is inherently random-access (jumping to xref, then to
/// objects scattered throughout the file). This trait abstracts over
/// the underlying data source.
#[allow(dead_code)]
pub(crate) trait RandomAccessSource {
    /// Read bytes starting at `offset` into `buf`.
    /// Returns the number of bytes actually read.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize>;

    /// Total length of the source in bytes.
    fn length(&self) -> u64;

    /// Read a range of bytes, returning an owned copy.
    fn read_range(&self, start: u64, end: u64) -> Result<Vec<u8>> {
        if end < start {
            return Err(Error::parse(
                start,
                "valid range",
                format!("end ({end}) before start ({start})"),
            ));
        }

        // Clamp end to source length to prevent over-reads
        let end = end.min(self.length());

        // If start is past the clamped end, return empty result
        if start >= end {
            return Ok(Vec::new());
        }

        // Check for overflow when computing length
        let len = (end - start)
            .try_into()
            .map_err(|_| Error::structure(format!("range too large: {start}..{end}")))?;

        let mut buf = vec![0u8; len];
        let bytes_read = self.read_at(start, &mut buf)?;
        buf.truncate(bytes_read);
        Ok(buf)
    }
}

/// An in-memory byte buffer as a random-access source.
///
/// This is the simplest source. Useful for testing, small files,
/// and WASM targets where we can't mmap.
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct BufferSource {
    data: Vec<u8>,
}

impl BufferSource {
    /// Create a new buffer source from owned bytes.
    #[allow(dead_code)]
    pub(crate) fn new(data: Vec<u8>) -> Self {
        Self { data }
    }

    /// Borrow the underlying data.
    #[allow(dead_code)]
    pub(crate) fn data(&self) -> &[u8] {
        &self.data
    }
}

impl From<Vec<u8>> for BufferSource {
    fn from(data: Vec<u8>) -> Self {
        Self::new(data)
    }
}

impl From<&[u8]> for BufferSource {
    fn from(data: &[u8]) -> Self {
        Self::new(data.to_vec())
    }
}

impl RandomAccessSource for BufferSource {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let offset = match usize::try_from(offset) {
            Ok(o) => o,
            Err(_) => return Ok(0), // offset beyond addressable range
        };
        // Return 0 bytes if offset is at or past end (lenient behavior)
        if offset >= self.data.len() {
            return Ok(0);
        }
        let available = &self.data[offset..];
        let to_copy = buf.len().min(available.len());
        buf[..to_copy].copy_from_slice(&available[..to_copy]);
        Ok(to_copy)
    }

    fn length(&self) -> u64 {
        self.data.len() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_buffer_source_read_at() {
        let src = BufferSource::new(b"Hello, World!".to_vec());
        let mut buf = [0u8; 5];
        let n = src.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf, b"Hello");
    }

    #[test]
    fn test_buffer_source_read_at_offset() {
        let src = BufferSource::new(b"Hello, World!".to_vec());
        let mut buf = [0u8; 6];
        let n = src.read_at(7, &mut buf).unwrap();
        assert_eq!(n, 6);
        assert_eq!(&buf, b"World!");
    }

    #[test]
    fn test_buffer_source_read_past_end() {
        let src = BufferSource::new(b"Hi".to_vec());
        let mut buf = [0u8; 10];
        let n = src.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn test_buffer_source_read_at_end() {
        let src = BufferSource::new(b"Hi".to_vec());
        let mut buf = [0u8; 5];
        let n = src.read_at(2, &mut buf).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn test_buffer_source_read_range() {
        let src = BufferSource::new(b"Hello, World!".to_vec());
        let data = src.read_range(7, 12).unwrap();
        assert_eq!(&data, b"World");
    }

    #[test]
    fn test_buffer_source_length() {
        let src = BufferSource::new(b"Test".to_vec());
        assert_eq!(src.length(), 4);
    }

    #[test]
    fn test_buffer_source_empty() {
        let src = BufferSource::new(Vec::new());
        assert_eq!(src.length(), 0);
        let mut buf = [0u8; 5];
        let n = src.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn test_buffer_source_from_slice() {
        let src = BufferSource::from(b"data".as_slice());
        assert_eq!(src.length(), 4);
        assert_eq!(src.data(), b"data");
    }

    #[test]
    fn test_read_range_invalid() {
        let src = BufferSource::new(b"data".to_vec());
        assert!(src.read_range(5, 3).is_err());
    }

    #[test]
    fn test_read_range_past_end_clamped() {
        let src = BufferSource::new(b"hello".to_vec());
        let data = src.read_range(0, 100).unwrap();
        assert_eq!(&data, b"hello");
    }

    #[test]
    fn test_read_range_start_past_end() {
        let src = BufferSource::new(b"hello".to_vec());
        let data = src.read_range(10, 20).unwrap();
        assert_eq!(data.len(), 0);
    }

    #[test]
    fn test_read_at_exactly_at_end() {
        let src = BufferSource::new(b"hi".to_vec());
        let mut buf = [0u8; 5];
        let n = src.read_at(2, &mut buf).unwrap();
        assert_eq!(n, 0);
    }
}
