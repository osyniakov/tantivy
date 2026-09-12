use std::io::{self, BufWriter, Write};
use std::ops::Range;

use common::{CountingWriter, OwnedBytes};
#[cfg(feature = "zstd-compression")]
use zstd::bulk::Compressor;

use super::value::ValueWriter;
use super::{BlockReader, value, vint};

const FOUR_BIT_LIMITS: usize = 1 << 4;
const VINT_MODE: u8 = 1u8;
const BLOCK_LEN: usize = 4_000;

pub struct DeltaWriter<W, TValueWriter>
where W: io::Write
{
    block: Vec<u8>,
    write: CountingWriter<BufWriter<W>>,
    value_writer: TValueWriter,
    // Only here to avoid allocations.
    stateless_buffer: Vec<u8>,
    block_len: usize,
}

impl<W, TValueWriter> DeltaWriter<W, TValueWriter>
where
    W: io::Write,
    TValueWriter: ValueWriter,
{
    pub fn new(wrt: W) -> Self {
        DeltaWriter {
            block: Vec::with_capacity(BLOCK_LEN * 2),
            write: CountingWriter::wrap(BufWriter::new(wrt)),
            value_writer: TValueWriter::default(),
            stateless_buffer: Vec::new(),
            block_len: BLOCK_LEN,
        }
    }

    pub fn set_block_len(&mut self, block_len: usize) {
        self.block_len = block_len
    }

    pub fn flush_block(&mut self) -> io::Result<Option<Range<usize>>> {
        if self.block.is_empty() {
            return Ok(None);
        }
        let start_offset = self.write.written_bytes() as usize;

        let buffer: &mut Vec<u8> = &mut self.stateless_buffer;
        self.value_writer.serialize_block(buffer);
        self.value_writer.clear();

        let block_len = buffer.len() + self.block.len();

        if cfg!(feature = "zstd-compression") && block_len > 2048 {
            #[cfg(feature = "zstd-compression")]
            {
                buffer.extend_from_slice(&self.block);
                self.block.clear();

                let max_len = zstd::zstd_safe::compress_bound(buffer.len());
                self.block.reserve(max_len);
                Compressor::new(3)?.compress_to_buffer(buffer, &mut self.block)?;

                // verify compression had a positive impact
                if self.block.len() < buffer.len() {
                    self.write
                        .write_all(&(self.block.len() as u32 + 1).to_le_bytes())?;
                    self.write.write_all(&[1])?;
                    self.write.write_all(&self.block[..])?;
                } else {
                    self.write
                        .write_all(&(block_len as u32 + 1).to_le_bytes())?;
                    self.write.write_all(&[0])?;
                    self.write.write_all(&buffer[..])?;
                }
            }
        } else {
            self.write
                .write_all(&(block_len as u32 + 1).to_le_bytes())?;
            self.write.write_all(&[0])?;
            self.write.write_all(&buffer[..])?;
            self.write.write_all(&self.block[..])?;
        }

        let end_offset = self.write.written_bytes() as usize;
        self.block.clear();
        buffer.clear();
        Ok(Some(start_offset..end_offset))
    }

    fn encode_keep_add(&mut self, keep_len: usize, add_len: usize) {
        if keep_len < FOUR_BIT_LIMITS && add_len < FOUR_BIT_LIMITS {
            let b = (keep_len | (add_len << 4)) as u8;
            self.block.extend_from_slice(&[b])
        } else {
            let mut buf = [VINT_MODE; 20];
            let mut len = 1 + vint::serialize(keep_len as u64, &mut buf[1..]);
            len += vint::serialize(add_len as u64, &mut buf[len..]);
            self.block.extend_from_slice(&buf[..len])
        }
    }

    pub(crate) fn write_suffix(&mut self, common_prefix_len: usize, suffix: &[u8]) {
        let keep_len = common_prefix_len;
        let add_len = suffix.len();
        self.encode_keep_add(keep_len, add_len);
        self.block.extend_from_slice(suffix);
    }

    pub(crate) fn write_value(&mut self, value: &TValueWriter::Value) {
        self.value_writer.write(value);
    }

    pub fn flush_block_if_required(&mut self) -> io::Result<Option<Range<usize>>> {
        if self.block.len() > self.block_len {
            return self.flush_block();
        }
        Ok(None)
    }

    pub fn finish(self) -> CountingWriter<BufWriter<W>> {
        self.write
    }
}

pub struct DeltaReader<TValueReader> {
    common_prefix_len: usize,
    suffix_range: Range<usize>,
    value_reader: TValueReader,
    block_reader: BlockReader,
    idx: usize,
}

impl<TValueReader> DeltaReader<TValueReader>
where TValueReader: value::ValueReader
{
    pub fn new(reader: OwnedBytes) -> Self {
        DeltaReader {
            idx: 0,
            common_prefix_len: 0,
            suffix_range: 0..0,
            value_reader: TValueReader::default(),
            block_reader: BlockReader::new(reader),
        }
    }

    /// Build a reader over slices that may not be contiguous, each labelled with the term
    /// ordinal of its first term. See [`DeltaReader::take_first_ordinal`].
    pub fn from_multiple_blocks(reader: Vec<(OwnedBytes, u64)>) -> Self {
        DeltaReader {
            idx: 0,
            common_prefix_len: 0,
            suffix_range: 0..0,
            value_reader: TValueReader::default(),
            block_reader: BlockReader::from_multiple_blocks(reader),
        }
    }

    /// The first term ordinal of the slice just moved to, returned once per slice.
    ///
    /// A caller tracking term ordinals must consult this after every [`DeltaReader::advance`]:
    /// when an automaton has pruned blocks the ordinal jumps, and only the slice knows where to.
    pub fn take_first_ordinal(&mut self) -> Option<u64> {
        self.block_reader.take_first_ordinal()
    }

    pub fn empty() -> Self {
        DeltaReader::new(OwnedBytes::empty())
    }

    fn deserialize_vint(&mut self) -> u64 {
        self.block_reader.deserialize_u64()
    }

    fn read_keep_add(&mut self) -> Option<(usize, usize)> {
        let b = {
            let buf = &self.block_reader.buffer();
            if buf.is_empty() {
                return None;
            }
            buf[0]
        };
        self.block_reader.advance(1);
        match b {
            VINT_MODE => {
                let keep = self.deserialize_vint() as usize;
                let add = self.deserialize_vint() as usize;
                Some((keep, add))
            }
            b => {
                let keep = (b & 0b1111) as usize;
                let add = (b >> 4) as usize;
                Some((keep, add))
            }
        }
    }

    fn read_delta_key(&mut self) -> io::Result<bool> {
        let Some((keep, add)) = self.read_keep_add() else {
            return Ok(false);
        };
        // `add` is read from the block. A corrupt block can claim a suffix
        // longer than what is left, and advancing past the end would make the
        // next `buffer()` slice out of bounds.
        if add > self.block_reader.buffer().len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "sstable block claims a key suffix longer than the block",
            ));
        }
        // `keep` is the length of the prefix shared with the previous key, so
        // it can never exceed that key's length. A corrupt value here made the
        // caller resize its key buffer to `keep + add` bytes -- an allocation
        // of up to 2^64 bytes.
        let prev_key_len = self.common_prefix_len + self.suffix_range.len();
        if keep > prev_key_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "sstable block claims a key prefix longer than the previous key",
            ));
        }
        self.common_prefix_len = keep;
        let suffix_start = self.block_reader.offset();
        self.suffix_range = suffix_start..(suffix_start + add);
        self.block_reader.advance(add);
        Ok(true)
    }

    pub fn advance(&mut self) -> io::Result<bool> {
        if self.block_reader.buffer().is_empty() {
            if !self.block_reader.read_block()? {
                return Ok(false);
            }
            let consumed_len = self.value_reader.load(self.block_reader.buffer())?;
            self.block_reader.advance(consumed_len);
            self.idx = 0;
        } else {
            self.idx += 1;
        }
        if !self.read_delta_key()? {
            return Ok(false);
        }
        // Every key in a block has a value in the same block. A corrupt block
        // whose key section outruns its value section used to make `value()`
        // index past the end.
        if self
            .value_reader
            .num_values()
            .is_some_and(|num_values| self.idx >= num_values)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "sstable block holds more keys than values",
            ));
        }
        Ok(true)
    }

    #[inline(always)]
    pub fn common_prefix_len(&self) -> usize {
        self.common_prefix_len
    }

    #[inline(always)]
    pub fn suffix(&self) -> &[u8] {
        self.block_reader.buffer_from_to(self.suffix_range.clone())
    }

    #[inline(always)]
    pub fn value(&self) -> &TValueReader::Value {
        self.value_reader.value(self.idx)
    }
}

#[cfg(test)]
mod tests {
    use super::DeltaReader;
    use crate::value::U64MonotonicValueReader;

    #[test]
    fn test_empty() {
        let mut delta_reader: DeltaReader<U64MonotonicValueReader> = DeltaReader::empty();
        assert!(!delta_reader.advance().unwrap());
    }
}

#[cfg(test)]
mod corrupt_block_tests {
    use common::OwnedBytes;

    use super::DeltaReader;
    use crate::value::U64MonotonicValueReader;

    // A single uncompressed block. The layout is `[len: u32 LE][compressed: u8]`
    // followed by the value section, then the key section, then the 4-byte zero
    // length that ends the stream.
    fn block(content: &[u8]) -> OwnedBytes {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(content.len() as u32 + 1).to_le_bytes());
        bytes.push(0);
        bytes.extend_from_slice(content);
        bytes.extend_from_slice(&0u32.to_le_bytes());
        OwnedBytes::new(bytes)
    }

    // Regression test for a fuzzing finding: the block below declares one value
    // but carries two keys, and reading the second key used to index past the
    // end of the value reader's `Vec` and panic.
    #[test]
    fn more_keys_than_values_is_an_error() {
        // values: count=1, delta=5   keys: (keep=0, add=1) "a", (keep=0, add=1) "b"
        let mut reader = DeltaReader::<U64MonotonicValueReader>::new(block(&[
            0x01, 0x05, 0x10, b'a', 0x10, b'b',
        ]));
        assert!(reader.advance().unwrap());
        assert_eq!(*reader.value(), 5);
        assert!(reader.advance().is_err());
    }

    #[test]
    fn suffix_longer_than_block_is_an_error() {
        // values: count=1, delta=5   key: (keep=0, add=15) but only one byte follows
        let mut reader =
            DeltaReader::<U64MonotonicValueReader>::new(block(&[0x01, 0x05, 0xf0, b'a']));
        assert!(reader.advance().is_err());
    }

    #[test]
    fn truncated_value_section_is_an_error() {
        // values: count=3, but a single (continued) byte and then nothing
        let mut reader = DeltaReader::<U64MonotonicValueReader>::new(block(&[0x03, 0x85]));
        assert!(reader.advance().is_err());
    }

    #[test]
    fn prefix_longer_than_previous_key_is_an_error() {
        // values: count=1, delta=5   first key: (keep=5, add=1) "a" -- but there
        // is no previous key to share five bytes with.
        let mut reader =
            DeltaReader::<U64MonotonicValueReader>::new(block(&[0x01, 0x05, 0x15, b'a']));
        assert!(reader.advance().is_err());
    }
}
