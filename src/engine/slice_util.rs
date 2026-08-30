//! Bounds-safe byte slice helpers for wire parsers (corrupt CDN payloads must not panic).

/// Returns `len` bytes at `start`, or `None` on overflow / OOB.
#[inline]
pub(crate) fn subslice_len(data: &[u8], start: usize, len: usize) -> Option<&[u8]> {
    let end = start.checked_add(len)?;
    data.get(start..end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subslice_rejects_oob() {
        assert!(subslice_len(b"abcd", 2, 3).is_none());
        assert_eq!(subslice_len(b"abcd", 0, 2), Some(b"ab".as_ref()));
    }
}
