use std::io;

use common::OwnedBytes;

/// Splits `bytes`, keeping `left_len` bytes on the left.
///
/// `OwnedBytes::split` panics when asked for more bytes than it has, which is
/// the right contract for a length the writer produced. These lengths are read
/// back out of the file, so they are as untrusted as the rest of it: `what`
/// names the field so a corrupt column says which one disagreed.
pub(crate) fn split_checked(
    bytes: OwnedBytes,
    left_len: usize,
    what: &str,
) -> io::Result<(OwnedBytes, OwnedBytes)> {
    if left_len > bytes.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{what} declares {left_len} bytes, but only {} remain",
                bytes.len()
            ),
        ));
    }
    Ok(bytes.split(left_len))
}

/// Splits `bytes`, keeping `right_len` bytes on the right.
///
/// See [`split_checked`].
pub(crate) fn rsplit_checked(
    bytes: OwnedBytes,
    right_len: usize,
    what: &str,
) -> io::Result<(OwnedBytes, OwnedBytes)> {
    if right_len > bytes.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{what} declares {right_len} bytes, but only {} remain",
                bytes.len()
            ),
        ));
    }
    Ok(bytes.rsplit(right_len))
}

const fn compute_mask(num_bits: u8) -> u8 {
    if num_bits == 8 {
        u8::MAX
    } else {
        (1u8 << num_bits) - 1
    }
}

#[inline(always)]
#[must_use]
pub(crate) fn select_bits<const START: u8, const END: u8>(code: u8) -> u8 {
    assert!(START <= END);
    assert!(END <= 8);
    let num_bits: u8 = END - START;
    let mask: u8 = compute_mask(num_bits);
    (code >> START) & mask
}

#[inline(always)]
#[must_use]
pub(crate) fn place_bits<const START: u8, const END: u8>(code: u8) -> u8 {
    assert!(START <= END);
    assert!(END <= 8);
    let num_bits: u8 = END - START;
    let mask: u8 = compute_mask(num_bits);
    assert!(code <= mask);
    code << START
}

/// Pop-front one bytes from a slice of bytes.
#[inline(always)]
pub fn pop_first_byte(bytes: &mut &[u8]) -> Option<u8> {
    if bytes.is_empty() {
        return None;
    }
    let first_byte = bytes[0];
    *bytes = &bytes[1..];
    Some(first_byte)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_select_bits() {
        assert_eq!(255u8, select_bits::<0, 8>(255u8));
        assert_eq!(0u8, select_bits::<0, 0>(255u8));
        assert_eq!(8u8, select_bits::<0, 4>(8u8));
        assert_eq!(4u8, select_bits::<1, 4>(8u8));
        assert_eq!(0u8, select_bits::<1, 3>(8u8));
    }

    #[test]
    fn test_place_bits() {
        assert_eq!(255u8, place_bits::<0, 8>(255u8));
        assert_eq!(4u8, place_bits::<2, 3>(1u8));
        assert_eq!(0u8, place_bits::<2, 2>(0u8));
    }

    #[test]
    #[should_panic]
    fn test_place_bits_overflows() {
        let _ = place_bits::<1, 4>(8u8);
    }

    #[test]
    fn test_pop_first_byte() {
        let mut cursor: &[u8] = &b"abcd"[..];
        assert_eq!(pop_first_byte(&mut cursor), Some(b'a'));
        assert_eq!(pop_first_byte(&mut cursor), Some(b'b'));
        assert_eq!(pop_first_byte(&mut cursor), Some(b'c'));
        assert_eq!(pop_first_byte(&mut cursor), Some(b'd'));
        assert_eq!(pop_first_byte(&mut cursor), None);
    }
}
