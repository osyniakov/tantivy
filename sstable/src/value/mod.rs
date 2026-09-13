pub(crate) mod index;
mod range;
mod u64_monotonic;
mod vec_u32;
mod void;

use std::io;

pub use range::{RangeValueReader, RangeValueWriter};
pub use u64_monotonic::{U64MonotonicValueReader, U64MonotonicValueWriter};
pub use vec_u32::{VecU32ValueReader, VecU32ValueWriter};
pub use void::{VoidValueReader, VoidValueWriter};

/// `ValueReader` is a trait describing the contract of something
/// reading blocks of value, and offering random access within this values.
pub trait ValueReader: Default {
    /// Type of the value being read.
    type Value;

    /// Access the value at index `idx`, in the last block that was read
    /// via a call to `ValueReader::read`.
    fn value(&self, idx: usize) -> &Self::Value;

    /// Loads a block.
    ///
    /// Returns the number of bytes that were read.
    fn load(&mut self, data: &[u8]) -> io::Result<usize>;

    /// Number of values in the block loaded by the last call to `load`, when
    /// the reader knows it.
    ///
    /// The block readers use it to reject a corrupt block whose key section
    /// holds more entries than its value section declared; without it, `value`
    /// would be asked for an index that does not exist. Returns `None` by
    /// default, so existing implementors are unaffected and simply keep the
    /// old behaviour.
    fn num_values(&self) -> Option<usize> {
        None
    }
}

/// `ValueWriter` is a trait to make it possible to write blocks
/// of value.
pub trait ValueWriter: Default {
    /// Type of the value being written.
    type Value;

    /// Records a new value.
    /// This method usually just accumulates data in a `Vec`,
    /// only to be serialized on the call to `ValueWriter::serialize_block`.
    fn write(&mut self, val: &Self::Value);

    /// Serializes the accumulated values into the output buffer.
    fn serialize_block(&self, output: &mut Vec<u8>);

    /// Clears the `ValueWriter`. After a call to clear, the `ValueWriter`
    /// should behave like a fresh `ValueWriter::default()`.
    fn clear(&mut self);
}

pub(crate) fn invalid_data(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

/// Reads one vint, rejecting input that ends before the value does and
/// encodings longer than a `u64` can need.
///
/// `deserialize_read` alone reports neither: on an exhausted buffer it returns
/// `(0, 0)`, so a truncated block used to decode as a run of zeros, and an
/// over-long encoding used to overflow its shift.
fn deserialize_vint_u64(data: &mut &[u8]) -> io::Result<u64> {
    let (num_bytes, val) = super::vint::deserialize_read(data);
    if num_bytes == 0 || data[num_bytes - 1] >= super::vint::CONTINUE_BIT {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "sstable value block ends in the middle of a value",
        ));
    }
    if num_bytes > super::vint::MAX_U64_VINT_LEN {
        return Err(invalid_data(
            "sstable value block holds a vint longer than a u64",
        ));
    }
    *data = &data[num_bytes..];
    Ok(val)
}

/// Reads one little-endian `u32`, or fails if fewer than 4 bytes remain.
pub(crate) fn read_u32(data: &mut &[u8]) -> io::Result<u32> {
    let Some((bytes, rest)) = data.split_first_chunk::<4>() else {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "sstable value block ends in the middle of a u32",
        ));
    };
    *data = rest;
    Ok(u32::from_le_bytes(*bytes))
}

#[cfg(test)]
pub(crate) mod tests {
    use std::fmt;

    use super::{ValueReader, ValueWriter};

    pub(crate) fn test_value_reader_writer<
        V: Eq + fmt::Debug,
        TReader: ValueReader<Value = V>,
        TWriter: ValueWriter<Value = V>,
    >(
        value_block: &[V],
    ) {
        let mut buffer = Vec::new();
        {
            let mut writer = TWriter::default();
            for value in value_block {
                writer.write(value);
            }
            writer.serialize_block(&mut buffer);
            writer.clear();
        }
        let data_len = buffer.len();
        buffer.extend_from_slice(&b"extradata"[..]);
        let mut reader = TReader::default();
        assert_eq!(reader.load(&buffer[..]).unwrap(), data_len);
        for (i, val) in value_block.iter().enumerate() {
            assert_eq!(reader.value(i), val);
        }
    }
}
