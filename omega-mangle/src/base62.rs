//! `<base62-number> = {0-9a-zA-Z} "_"` -- v0's own encoding, reused
//! verbatim (RFC 2603's reference-level explanation): `"_"` alone is 0,
//! any other value is offset by 1 (`"0_"` is 1, `"Z_"` is 62, `"10_"` is
//! 63, ...). Used for backref byte-offsets, sized-array lengths, and
//! enum-variant indices -- anywhere the grammar needs a compact integer.

const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";

pub fn encode(n: u64) -> String {
    if n == 0 {
        return "_".to_string();
    }
    let mut m = n - 1;
    let mut digits = Vec::new();
    loop {
        digits.push(DIGITS[(m % 62) as usize]);
        m /= 62;
        if m == 0 {
            break;
        }
    }
    digits.reverse();
    let mut s = String::from_utf8(digits).expect("DIGITS is pure ASCII");
    s.push('_');
    s
}

/// Parses a `<base62-number>` starting at `bytes[*pos]`, advancing `*pos`
/// past the trailing `"_"` on success.
pub fn decode(bytes: &[u8], pos: &mut usize) -> Option<u64> {
    let start = *pos;
    let mut value: u64 = 0;
    let mut len = 0u32;
    while let Some(&b) = bytes.get(*pos) {
        if b == b'_' {
            *pos += 1;
            if len == 0 {
                return Some(0);
            }
            return Some(value + 1);
        }
        let digit = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'z' => b - b'a' + 10,
            b'A'..=b'Z' => b - b'A' + 36,
            _ => {
                *pos = start;
                return None;
            }
        } as u64;
        value = value.checked_mul(62)?.checked_add(digit)?;
        len += 1;
        *pos += 1;
    }
    *pos = start;
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for n in [0, 1, 2, 61, 62, 63, 64, 1000, 999_999] {
            let s = encode(n);
            let mut pos = 0;
            let bytes = s.as_bytes();
            assert_eq!(decode(bytes, &mut pos), Some(n), "n={n} s={s}");
            assert_eq!(pos, bytes.len());
        }
    }

    #[test]
    fn matches_rfc_examples() {
        assert_eq!(encode(0), "_");
        assert_eq!(encode(1), "0_");
        assert_eq!(encode(62), "Z_");
        assert_eq!(encode(63), "10_");
    }
}
