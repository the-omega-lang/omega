const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
const RADIX: u64 = DIGITS.len() as u64;

pub fn encode(value: u64) -> String {
    if value == 0 {
        return "_".to_string();
    }

    let mut value = value - 1;
    let mut digits = Vec::new();
    loop {
        let digit = usize::try_from(value % RADIX).expect("base62 digit index fits in usize");
        digits.push(DIGITS[digit]);
        value /= RADIX;
        if value == 0 {
            break;
        }
    }

    digits.reverse();
    let mut encoded = String::from_utf8(digits).expect("base62 digits are ASCII");
    encoded.push('_');
    encoded
}

pub fn decode(bytes: &[u8], pos: &mut usize) -> Option<u64> {
    let start = *pos;
    let decoded = decode_inner(bytes, pos);
    if decoded.is_none() {
        *pos = start;
    }
    decoded
}

fn decode_inner(bytes: &[u8], pos: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    let mut has_digits = false;

    while let Some(&byte) = bytes.get(*pos) {
        if byte == b'_' {
            *pos += 1;
            return if has_digits {
                value.checked_add(1)
            } else {
                Some(0)
            };
        }

        let digit = decode_digit(byte)?;
        value = value.checked_mul(RADIX)?.checked_add(digit)?;
        has_digits = true;
        *pos += 1;
    }

    None
}

fn decode_digit(byte: u8) -> Option<u64> {
    Some(match byte {
        b'0'..=b'9' => u64::from(byte - b'0'),
        b'a'..=b'z' => u64::from(byte - b'a' + 10),
        b'A'..=b'Z' => u64::from(byte - b'A' + 36),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for value in [0, 1, 2, 61, 62, 63, 64, 1000, 999_999, u64::MAX] {
            let encoded = encode(value);
            let mut pos = 0;
            assert_eq!(decode(encoded.as_bytes(), &mut pos), Some(value));
            assert_eq!(pos, encoded.len());
        }
    }

    #[test]
    fn matches_grammar_examples() {
        assert_eq!(encode(0), "_");
        assert_eq!(encode(1), "0_");
        assert_eq!(encode(62), "Z_");
        assert_eq!(encode(63), "10_");
    }

    #[test]
    fn failed_decode_does_not_consume_input() {
        for input in ["!", "10", "ZZZZZZZZZZZZZZZZZZZZ_"] {
            let mut pos = 0;
            assert_eq!(decode(input.as_bytes(), &mut pos), None, "input={input}");
            assert_eq!(pos, 0, "input={input}");
        }
    }
}
