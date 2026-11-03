use crate::{Error, MAX_BYTES, Result};
use serde::Serialize;
use serde_json::ser::{CompactFormatter, Formatter, PrettyFormatter, Serializer};
use sha2::{Digest, Sha256};
use std::io::{self, Write};

#[derive(Default)]
struct BoundedBuffer {
    bytes: Vec<u8>,
    exceeded: bool,
}

impl Write for BoundedBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_BYTES as usize - self.bytes.len() {
            self.exceeded = true;
            return Err(io::Error::other("JSON exceeds the 4 MiB limit"));
        }
        let required = self.bytes.len() + bytes.len();
        if required > self.bytes.capacity() {
            let capacity = required
                .max(self.bytes.capacity().saturating_mul(2))
                .min(MAX_BYTES as usize);
            self.bytes
                .try_reserve_exact(capacity - self.bytes.len())
                .map_err(io::Error::other)?;
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn encode(value: &impl Serialize, formatter: impl Formatter) -> Result<Vec<u8>> {
    let mut buffer = BoundedBuffer::default();
    let result = value.serialize(&mut Serializer::with_formatter(&mut buffer, formatter));
    if buffer.exceeded {
        return Err(Error::refused("Encoded JSON exceeds the 4 MiB limit."));
    }
    result.map_err(|_| Error::new("io", "Could not encode JSON.", 1))?;
    Ok(buffer.bytes)
}

/// Encodes pretty JSON while retaining at most 4 MiB of output.
pub fn json_bytes(value: &impl Serialize) -> Result<Vec<u8>> {
    encode(value, PrettyFormatter::new())
}

pub(crate) fn compact_json_bytes(value: &impl Serialize) -> Result<Vec<u8>> {
    encode(value, CompactFormatter)
}

struct HashWriter(Sha256);

impl Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) fn json_digest(value: &impl Serialize) -> Result<String> {
    // Preserve the existing pretty-JSON digest without allocating its expanded bytes.
    let mut writer = HashWriter(Sha256::new());
    serde_json::to_writer_pretty(&mut writer, value)
        .map_err(|_| Error::new("io", "Could not hash JSON.", 1))?;
    Ok(writer
        .0
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_growth_never_reserves_past_the_artifact_limit() {
        let mut buffer = BoundedBuffer::default();
        buffer.write_all(b"\"").unwrap();
        buffer
            .write_all(&vec![b'x'; MAX_BYTES as usize - 2])
            .unwrap();
        buffer.write_all(b"\"").unwrap();
        assert_eq!(buffer.bytes.len(), MAX_BYTES as usize);
        assert!(buffer.bytes.capacity() <= MAX_BYTES as usize);
        assert!(buffer.write_all(b"x").is_err());
        assert_eq!(buffer.bytes.len(), MAX_BYTES as usize);
    }
}
