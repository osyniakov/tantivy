use std::io;

use super::{ValueReader, ValueWriter, invalid_data, read_u32};

#[derive(Default)]
pub struct VecU32ValueReader {
    vals: Vec<Vec<u32>>,
}

impl ValueReader for VecU32ValueReader {
    type Value = Vec<u32>;

    #[inline(always)]
    fn value(&self, idx: usize) -> &Self::Value {
        &self.vals[idx]
    }

    fn num_values(&self) -> Option<usize> {
        Some(self.vals.len())
    }

    fn load(&mut self, mut data: &[u8]) -> io::Result<usize> {
        let original_num_bytes = data.len();
        self.vals.clear();

        // The first 4 bytes are the number of blocks
        let num_blocks = read_u32(&mut data)? as usize;

        for _ in 0..num_blocks {
            // Each block starts with a 4-byte length
            let segment_len = read_u32(&mut data)? as usize;
            // `segment_len` comes out of the block, so make sure the ids it
            // announces are actually there before reserving room for them.
            if data.len() / 4 < segment_len {
                return Err(invalid_data(
                    "sstable value block announces more segment ids than it holds",
                ));
            }

            // Read the segment IDs for this block
            let mut segment_ids = Vec::with_capacity(segment_len);
            for _ in 0..segment_len {
                let segment_id = read_u32(&mut data)?;
                segment_ids.push(segment_id);
            }
            self.vals.push(segment_ids);
        }

        // Return the number of bytes consumed
        Ok(original_num_bytes - data.len())
    }
}

#[derive(Default)]
pub struct VecU32ValueWriter {
    vals: Vec<Vec<u32>>,
}

impl ValueWriter for VecU32ValueWriter {
    type Value = Vec<u32>;

    fn write(&mut self, val: &Self::Value) {
        self.vals.push(val.to_vec());
    }

    fn serialize_block(&self, output: &mut Vec<u8>) {
        let num_blocks = self.vals.len() as u32;
        output.extend_from_slice(&num_blocks.to_le_bytes());
        for vals in &self.vals {
            let len = vals.len() as u32;
            output.extend_from_slice(&len.to_le_bytes());
            for &segment_id in vals.iter() {
                output.extend_from_slice(&segment_id.to_le_bytes());
            }
        }
    }

    fn clear(&mut self) {
        self.vals.clear();
    }
}
