//! Bounds-checked little-endian reads over a byte slice.
//!
//! Rust panics on an out-of-range index, which for a parser fed a truncated
//! download or a malformed executable means the application dies instead of
//! reporting a bad file. Every accessor here returns `None` instead, and the
//! callers turn that into their own error code.

pub trait Le {
    fn u16_at(&self, offset: usize) -> Option<u16>;
    fn u32_at(&self, offset: usize) -> Option<u32>;
    fn u64_at(&self, offset: usize) -> Option<u64>;
    /// A fixed-size window, or `None` if it would run past the end.
    fn window(&self, offset: usize, length: usize) -> Option<&[u8]>;
}

impl Le for [u8] {
    fn u16_at(&self, offset: usize) -> Option<u16> {
        let bytes = self.window(offset, 2)?;
        Some(u16::from_le_bytes([*bytes.first()?, *bytes.get(1)?]))
    }

    fn u32_at(&self, offset: usize) -> Option<u32> {
        let bytes = self.window(offset, 4)?;
        Some(u32::from_le_bytes([
            *bytes.first()?,
            *bytes.get(1)?,
            *bytes.get(2)?,
            *bytes.get(3)?,
        ]))
    }

    fn u64_at(&self, offset: usize) -> Option<u64> {
        let bytes = self.window(offset, 8)?;
        let mut buffer = [0u8; 8];
        buffer.copy_from_slice(bytes);
        Some(u64::from_le_bytes(buffer))
    }

    fn window(&self, offset: usize, length: usize) -> Option<&[u8]> {
        let end = offset.checked_add(length)?;
        self.get(offset..end)
    }
}

#[cfg(test)]
mod tests {
    use super::Le;

    #[test]
    fn reads_within_bounds_and_refuses_past_the_end() {
        let data: [u8; 8] = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        assert_eq!(data.u16_at(0), Some(0x0201));
        assert_eq!(data.u32_at(0), Some(0x0403_0201));
        assert_eq!(data.u64_at(0), Some(0x0807_0605_0403_0201));
        assert_eq!(data.u16_at(6), Some(0x0807));

        // One byte short in every width.
        assert_eq!(data.u16_at(7), None);
        assert_eq!(data.u32_at(5), None);
        assert_eq!(data.u64_at(1), None);
        assert_eq!(data.u32_at(8), None);
    }

    #[test]
    fn an_offset_near_the_maximum_cannot_wrap() {
        let data: [u8; 4] = [1, 2, 3, 4];
        // `offset + length` would overflow and wrap to a small in-range value
        // without the checked add.
        assert_eq!(data.u32_at(usize::MAX), None);
        assert_eq!(data.window(usize::MAX - 1, 4), None);
    }

    #[test]
    fn windows_are_exact() {
        let data: [u8; 4] = [1, 2, 3, 4];
        assert_eq!(data.window(1, 2), Some(&[2u8, 3][..]));
        assert_eq!(data.window(0, 4), Some(&data[..]));
        assert_eq!(data.window(0, 5), None);
        assert_eq!(data.window(4, 0), Some(&[][..]));
    }
}
