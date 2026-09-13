use std::io;
use std::io::Write;
use std::num::NonZeroU64;

use common::{BinarySerializable, VInt};

use crate::RowId;

/// Column statistics.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ColumnStats {
    /// GCD of the elements `el - min(column)`.
    pub gcd: NonZeroU64,
    /// Minimum value of the column.
    pub min_value: u64,
    /// Maximum value of the column.
    pub max_value: u64,
    /// Number of rows in the column.
    pub num_rows: RowId,
}

impl ColumnStats {
    /// Amplitude of value.
    /// Difference between the maximum and the minimum value.
    pub fn amplitude(&self) -> u64 {
        self.max_value - self.min_value
    }
}

impl BinarySerializable for ColumnStats {
    fn serialize<W: Write + ?Sized>(&self, writer: &mut W) -> io::Result<()> {
        VInt(self.min_value).serialize(writer)?;
        VInt(self.gcd.get()).serialize(writer)?;
        VInt(self.amplitude() / self.gcd).serialize(writer)?;
        VInt(self.num_rows as u64).serialize(writer)?;
        Ok(())
    }

    fn deserialize<R: io::Read>(reader: &mut R) -> io::Result<Self> {
        let min_value = VInt::deserialize(reader)?.0;
        let gcd = VInt::deserialize(reader)?.0;
        let gcd = NonZeroU64::new(gcd)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "GCD of 0 is forbidden"))?;
        // The amplitude and the minimum are both read from the file, so their
        // product and sum are not bounded by anything the writer did: a column
        // claiming an amplitude that overflows a u64 is corrupt.
        let amplitude_over_gcd = VInt::deserialize(reader)?.0;
        let max_value = amplitude_over_gcd
            .checked_mul(gcd.get())
            .and_then(|amplitude| min_value.checked_add(amplitude))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "column stats overflow: a minimum of {min_value} plus \
                         {amplitude_over_gcd} times a gcd of {gcd} does not fit in a u64"
                    ),
                )
            })?;
        let num_rows = VInt::deserialize(reader)?.0 as RowId;
        Ok(ColumnStats {
            min_value,
            max_value,
            num_rows,
            gcd,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use common::BinarySerializable;

    use crate::column_values::ColumnStats;

    // Regression test for a fuzzing finding: the amplitude and the minimum both
    // come from the file, and multiplying the amplitude by the gcd or adding the
    // minimum to it used to overflow rather than report the column as corrupt.
    #[test]
    fn test_deserialize_overflowing_stats_is_error() {
        use common::VInt;

        fn stats_bytes(min_value: u64, gcd: u64, amplitude_over_gcd: u64) -> Vec<u8> {
            let mut buffer = Vec::new();
            VInt(min_value).serialize(&mut buffer).unwrap();
            VInt(gcd).serialize(&mut buffer).unwrap();
            VInt(amplitude_over_gcd).serialize(&mut buffer).unwrap();
            VInt(1u64).serialize(&mut buffer).unwrap();
            buffer
        }

        // The amplitude alone overflows when scaled back up by the gcd.
        let bytes = stats_bytes(0, 1 << 32, 1 << 32);
        assert!(ColumnStats::deserialize(&mut &bytes[..]).is_err());

        // The amplitude fits, but adding the minimum to it does not.
        let bytes = stats_bytes(u64::MAX, 1, 1);
        assert!(ColumnStats::deserialize(&mut &bytes[..]).is_err());

        // A column that does add up still deserializes.
        let bytes = stats_bytes(10, 2, 3);
        let stats = ColumnStats::deserialize(&mut &bytes[..]).unwrap();
        assert_eq!(stats.min_value, 10);
        assert_eq!(stats.max_value, 16);
    }

    #[track_caller]
    fn test_stats_ser_deser_aux(stats: &ColumnStats, num_bytes: usize) {
        let mut buffer: Vec<u8> = Vec::new();
        stats.serialize(&mut buffer).unwrap();
        assert_eq!(buffer.len(), num_bytes);
        let deser_stats = ColumnStats::deserialize(&mut &buffer[..]).unwrap();
        assert_eq!(stats, &deser_stats);
    }

    #[test]
    fn test_stats_serialization() {
        test_stats_ser_deser_aux(
            &(ColumnStats {
                gcd: NonZeroU64::new(3).unwrap(),
                min_value: 1,
                max_value: 3001,
                num_rows: 10,
            }),
            5,
        );
        test_stats_ser_deser_aux(
            &(ColumnStats {
                gcd: NonZeroU64::new(1_000).unwrap(),
                min_value: 1,
                max_value: 3001,
                num_rows: 10,
            }),
            5,
        );
        test_stats_ser_deser_aux(
            &(ColumnStats {
                gcd: NonZeroU64::new(1).unwrap(),
                min_value: 0,
                max_value: 0,
                num_rows: 0,
            }),
            4,
        );
    }
}
