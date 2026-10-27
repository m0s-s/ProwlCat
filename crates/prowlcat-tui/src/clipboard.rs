//! Copy to the clipboard through the terminal itself (OSC 52), so it also works over SSH and
//! needs no platform clipboard library. Terminals that do not support it simply ignore it.

use std::io::Write;

/// Longest text sent; some terminals drop larger sequences.
const MAX_BYTES: usize = 32 * 1024;

pub fn copy(text: &str) {
    let mut out = std::io::stdout();
    let _ = out.write_all(osc52(text).as_bytes());
    let _ = out.flush();
}

pub fn osc52(text: &str) -> String {
    let bytes = text.as_bytes();
    let bytes = &bytes[..bytes.len().min(MAX_BYTES)];
    format!("\x1b]52;c;{}\x07", base64(bytes))
}

fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |acc, (i, b)| acc | (*b as u32) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_rfc_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn osc52_wraps_the_payload() {
        assert_eq!(osc52("hi"), "\x1b]52;c;aGk=\x07");
    }

    #[test]
    fn huge_text_is_truncated() {
        let seq = osc52(&"a".repeat(MAX_BYTES * 2));
        assert!(seq.len() < MAX_BYTES * 2);
    }
}
