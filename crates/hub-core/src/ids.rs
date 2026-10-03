use std::fmt::Write;

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
        // Writing into a String cannot fail.
        let _ = write!(out, "{byte:02x}");
        out
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn hex_is_lowercase_and_padded() {
        assert_eq!(super::hex(&[0x0a, 0xff]), "0aff");
    }
}
