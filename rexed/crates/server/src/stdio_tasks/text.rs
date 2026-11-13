//! Platform compatibility for the text projection of observed stdio bytes.
//! Byte accounting and the underlying pipes remain byte-oriented.

#[cfg(not(windows))]
pub(super) fn decode(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(windows)]
pub(super) fn decode(bytes: &[u8]) -> String {
    decode_windows(bytes, windows_sys::Win32::Globalization::CP_OEMCP)
}

#[cfg(windows)]
fn decode_windows(bytes: &[u8], code_page: u32) -> String {
    use std::borrow::Cow;
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => Cow::Borrowed(text),
        Err(_) => Cow::Owned(
            decode_code_page(bytes, code_page)
                .unwrap_or_else(|| String::from_utf8_lossy(bytes).into_owned()),
        ),
    };
    // Markdown consumers must not mistake the CR in Windows CRLF for a control.
    text.replace("\r\n", "\n")
}

#[cfg(windows)]
fn decode_code_page(bytes: &[u8], code_page: u32) -> Option<String> {
    use windows_sys::Win32::Globalization::MultiByteToWideChar;
    let len = i32::try_from(bytes.len()).ok()?;
    // SAFETY: The input has `len` bytes; a null output with size zero requests
    // the UTF-16 buffer size without writing any output.
    let needed =
        unsafe { MultiByteToWideChar(code_page, 0, bytes.as_ptr(), len, std::ptr::null_mut(), 0) };
    if needed <= 0 {
        return None;
    }
    let mut wide = vec![0u16; needed as usize];
    // SAFETY: `wide` contains the number of UTF-16 elements requested above.
    // The input, code page and conversion flags are identical to the size query.
    let written = unsafe {
        MultiByteToWideChar(code_page, 0, bytes.as_ptr(), len, wide.as_mut_ptr(), needed)
    };
    if written <= 0 {
        return None;
    }
    String::from_utf16(&wide[..written as usize]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_utf8_and_empty_output() {
        assert_eq!(decode(b""), "");
        assert_eq!(decode("中文\nemoji: 🦀\n".as_bytes()), "中文\nemoji: 🦀\n");
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_keeps_its_original_byte_projection() {
        assert_eq!(
            decode(b"line\r\nprogress\rnext\n\xff"),
            "line\r\nprogress\rnext\n�"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_normalizes_crlf_without_losing_lines() {
        assert_eq!(decode(b"one\r\n\r\ntwo\n"), "one\n\ntwo\n");
    }

    #[cfg(windows)]
    #[test]
    fn windows_prefers_utf8_over_the_legacy_code_page() {
        assert_eq!(decode_windows("中文 🦀\r\n".as_bytes(), 936), "中文 🦀\n");
    }

    #[cfg(windows)]
    #[test]
    fn windows_decodes_gbk_and_normalizes_crlf() {
        assert_eq!(
            decode_windows(b"\xd6\xd0\xce\xc4\xb2\xe2\xca\xd4\r\n", 936),
            "中文测试\n"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_uses_the_selected_code_page_instead_of_hardcoding_gbk() {
        assert_eq!(decode_windows(b"caf\xe9\r\n", 1252), "café\n");
    }
}
