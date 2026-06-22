//! libg — C-compatible FFI for the G encoding system.
//!
//! All functions follow these conventions:
//!   - Returns 0 on success, negative error code on failure
//!   - Output buffer is caller-allocated (use g_encode_max_size etc. to size it)
//!   - Thread-local error detail via g_last_error()
//!   - All functions are thread-safe (stateless except thread-local error)
//!
//! Error codes:
//!   G_OK            =  0  success
//!   G_ERR_BUFFER    = -1  output buffer too small
//!   G_ERR_INVALID   = -2  invalid input (bad header, charset violation, etc.)
//!   G_ERR_VERSION   = -3  unsupported .gt version
//!   G_ERR_NULL      = -4  null pointer argument
//!   G_ERR_ENCODE    = -5  encoding failed internally

use std::cell::RefCell;
use std::slice;
use std::ffi::CStr;
use super::codec::{encode_bytes, decode_to_bytes, EncodeOptions, DecodeOptions};
use super::text::{encode_to_text, decode_from_text, validate_text, VERSION_MAJOR, VERSION_MINOR, VERSION_PATCH};
use super::index::IndexTable;

// ── Error codes ─────────────────────────────────────────────────────────────

pub const G_OK:          i32 = 0;
pub const G_ERR_BUFFER:  i32 = -1;
pub const G_ERR_INVALID: i32 = -2;
pub const G_ERR_VERSION: i32 = -3;
pub const G_ERR_NULL:    i32 = -4;
pub const G_ERR_ENCODE:  i32 = -5;

// ── Thread-local error storage ──────────────────────────────────────────────

thread_local! {
    static LAST_ERROR: RefCell<String> = RefCell::new(String::new());
}

fn set_error(msg: impl Into<String>) {
    LAST_ERROR.with(|e| *e.borrow_mut() = msg.into());
}

fn clear_error() {
    LAST_ERROR.with(|e| e.borrow_mut().clear());
}

// ── Helper macros ───────────────────────────────────────────────────────────

macro_rules! null_check {
    ($ptr:expr) => {
        if $ptr.is_null() {
            set_error("null pointer argument");
            return G_ERR_NULL;
        }
    };
}

macro_rules! check_buf {
    ($needed:expr, $cap:expr) => {
        if $needed > $cap {
            set_error(format!("output buffer too small: need {} bytes, have {}", $needed, $cap));
            return G_ERR_BUFFER;
        }
    };
}

// ── Version ─────────────────────────────────────────────────────────────────

/// Get the G library version.
/// All output pointers may be null (ignored if so).
#[unsafe(no_mangle)]
pub extern "C" fn g_version(major: *mut i32, minor: *mut i32, patch: *mut i32) {
    if !major.is_null() { unsafe { *major = VERSION_MAJOR as i32; } }
    if !minor.is_null() { unsafe { *minor = VERSION_MINOR as i32; } }
    if !patch.is_null() { unsafe { *patch = VERSION_PATCH as i32; } }
}

// ── Last error ───────────────────────────────────────────────────────────────

/// Return a pointer to the last error message for this thread.
/// The pointer is valid until the next G call on this thread.
/// Never returns null — returns empty string if no error.
#[unsafe(no_mangle)]
pub extern "C" fn g_last_error() -> *const std::os::raw::c_char {
    LAST_ERROR.with(|e| {
        let s = e.borrow();
        // Leak a CString for the duration of the call
        // (caller should not free this pointer)
        let cstr = std::ffi::CString::new(s.as_str()).unwrap_or_default();
        cstr.into_raw() as *const _
    })
}

// ── Size queries ─────────────────────────────────────────────────────────────

/// Maximum output size for g_encode (binary).
/// Returns the upper bound on encoded output bytes for `in_len` input bytes with instance `n`.
#[unsafe(no_mangle)]
pub extern "C" fn g_encode_max_size(in_len: usize, n: i32) -> usize {
    if n <= 0 { return 0; }
    let table = IndexTable::new(n as usize);
    let bits = table.bits as usize;
    let total_bits = in_len * 8;
    let blocks = (total_bits + bits - 1) / bits;
    // Header (frame) overhead: ~40 bytes. Index stream: ceil(blocks * bits / 8).
    // RNCE compression may reduce this but we give the uncompressed upper bound.
    let index_bytes = (blocks * bits + 7) / 8;
    40 + index_bytes + 64  // conservative overhead for RNCE header
}

/// Maximum output size for g_encode_text (.gt format).
/// Returns upper bound on text output bytes.
#[unsafe(no_mangle)]
pub extern "C" fn g_encode_text_max_size(in_len: usize, n: i32) -> usize {
    if n <= 0 { return 0; }
    // Header line: ~40 bytes. Each block: n chars + newline.
    let bits = IndexTable::new(n as usize).bits as usize;
    let total_bits = in_len * 8;
    let blocks = (total_bits + bits - 1).max(1) / bits + 1;
    40 + blocks * (n as usize + 1)
}

/// Maximum output size for g_decode (binary → bytes).
/// The exact size is stored in the .g header; this returns a safe upper bound.
#[unsafe(no_mangle)]
pub extern "C" fn g_decode_max_size(in_len: usize) -> usize {
    // Decoded output is always <= input (compression gains space),
    // but for safety return 2x input as upper bound
    in_len * 2
}

// ── Info ─────────────────────────────────────────────────────────────────────

/// Get information about a Gn instance.
/// All output pointers may be null (ignored if so).
/// Returns G_OK on success, G_ERR_INVALID if n is invalid.
#[unsafe(no_mangle)]
pub extern "C" fn g_info(
    n: i32,
    out_valid_count_lo: *mut u64,  // lower 64 bits of valid_count
    out_valid_count_hi: *mut u64,  // upper 64 bits of valid_count (usually 0)
    out_bits: *mut u32,
    out_charset_len: *mut usize,
) -> i32 {
    if n <= 0 {
        set_error(format!("invalid n value: {}", n));
        return G_ERR_INVALID;
    }
    clear_error();
    let table = IndexTable::new(n as usize);
    if !out_valid_count_lo.is_null() {
        unsafe { *out_valid_count_lo = table.valid_count as u64; }
    }
    if !out_valid_count_hi.is_null() {
        unsafe { *out_valid_count_hi = (table.valid_count >> 64) as u64; }
    }
    if !out_bits.is_null() {
        unsafe { *out_bits = table.bits; }
    }
    if !out_charset_len.is_null() {
        unsafe { *out_charset_len = table.charset.len(); }
    }
    G_OK
}

/// Fill `out` with the Gn charset as a null-terminated string.
/// `out_cap` must be at least `g_info(..., charset_len) + 1`.
#[unsafe(no_mangle)]
pub extern "C" fn g_charset(n: i32, out: *mut u8, out_cap: usize, out_len: *mut usize) -> i32 {
    null_check!(out);
    if n <= 0 { set_error("invalid n"); return G_ERR_INVALID; }
    clear_error();

    let table = IndexTable::new(n as usize);
    let s: String = table.charset.iter().map(|g| g.ch).collect();
    let bytes = s.as_bytes();
    check_buf!(bytes.len() + 1, out_cap);

    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len());
        *out.add(bytes.len()) = 0; // null terminator
        if !out_len.is_null() { *out_len = bytes.len(); }
    }
    G_OK
}

// ── Binary encode/decode ─────────────────────────────────────────────────────

/// Encode raw bytes into a binary G stream (.g format).
///
/// `in`      — input bytes
/// `in_len`  — number of input bytes
/// `n`       — Gn instance (recommended: 28)
/// `out`     — output buffer (caller-allocated, use g_encode_max_size to size)
/// `out_cap` — capacity of output buffer
/// `out_len` — set to actual bytes written on success
///
/// Returns G_OK on success, negative error code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn g_encode(
    in_ptr: *const u8,
    in_len: usize,
    n: i32,
    out: *mut u8,
    out_cap: usize,
    out_len: *mut usize,
) -> i32 {
    null_check!(in_ptr);
    null_check!(out);
    null_check!(out_len);
    if n <= 0 { set_error(format!("invalid n: {}", n)); return G_ERR_INVALID; }
    clear_error();

    let data = unsafe { slice::from_raw_parts(in_ptr, in_len) };
    let opts = EncodeOptions {
        n: n as usize,
        show_progress: false,
        repair_invalid: true,
        entropy: true,
        context: false,
        adaptive: false,
        lz77: true,
    };

    match encode_bytes(data, &opts) {
        Ok(result) => {
            check_buf!(result.output_bytes, out_cap);
            unsafe {
                std::ptr::copy_nonoverlapping(result.data.as_ptr(), out, result.output_bytes);
                *out_len = result.output_bytes;
            }
            G_OK
        }
        Err(e) => {
            set_error(e);
            G_ERR_ENCODE
        }
    }
}

/// Decode a binary G stream (.g format) back to raw bytes.
///
/// Returns G_OK on success, negative error code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn g_decode(
    in_ptr: *const u8,
    in_len: usize,
    out: *mut u8,
    out_cap: usize,
    out_len: *mut usize,
) -> i32 {
    null_check!(in_ptr);
    null_check!(out);
    null_check!(out_len);
    clear_error();

    let data = unsafe { slice::from_raw_parts(in_ptr, in_len) };
    let opts = DecodeOptions { n: 0, show_progress: false, skip_nulls: true };

    match decode_to_bytes(data, &opts) {
        Ok(result) => {
            check_buf!(result.data.len(), out_cap);
            unsafe {
                std::ptr::copy_nonoverlapping(result.data.as_ptr(), out, result.data.len());
                *out_len = result.data.len();
            }
            G_OK
        }
        Err(e) => {
            set_error(e);
            G_ERR_INVALID
        }
    }
}

// ── Text encode/decode ────────────────────────────────────────────────────────

/// Encode raw bytes into G canonical text format (.gt).
///
/// `out` will be a null-terminated UTF-8 string.
/// Use g_encode_text_max_size to compute out_cap.
#[unsafe(no_mangle)]
pub extern "C" fn g_encode_text(
    in_ptr: *const u8,
    in_len: usize,
    n: i32,
    out: *mut u8,
    out_cap: usize,
    out_len: *mut usize,
) -> i32 {
    null_check!(in_ptr);
    null_check!(out);
    null_check!(out_len);
    if n <= 0 { set_error(format!("invalid n: {}", n)); return G_ERR_INVALID; }
    clear_error();

    let data = unsafe { slice::from_raw_parts(in_ptr, in_len) };

    match encode_to_text(data, n as usize) {
        Ok(text) => {
            let bytes = text.as_bytes();
            check_buf!(bytes.len() + 1, out_cap);
            unsafe {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len());
                *out.add(bytes.len()) = 0; // null terminator
                *out_len = bytes.len();
            }
            G_OK
        }
        Err(e) => {
            set_error(e);
            G_ERR_ENCODE
        }
    }
}

/// Decode G canonical text format (.gt) back to raw bytes.
///
/// `in_ptr` — null-terminated or length-bounded UTF-8 .gt text
/// Use g_decode_max_size(in_len) for out_cap upper bound.
#[unsafe(no_mangle)]
pub extern "C" fn g_decode_text(
    in_ptr: *const u8,
    in_len: usize,
    out: *mut u8,
    out_cap: usize,
    out_len: *mut usize,
) -> i32 {
    null_check!(in_ptr);
    null_check!(out);
    null_check!(out_len);
    clear_error();

    let bytes = unsafe { slice::from_raw_parts(in_ptr, in_len) };
    let text = match std::str::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => {
            set_error(format!("invalid UTF-8: {}", e));
            return G_ERR_INVALID;
        }
    };

    match decode_from_text(text) {
        Ok(data) => {
            check_buf!(data.len(), out_cap);
            unsafe {
                std::ptr::copy_nonoverlapping(data.as_ptr(), out, data.len());
                *out_len = data.len();
            }
            G_OK
        }
        Err(e) => {
            if e.contains("Unsupported") {
                set_error(e);
                G_ERR_VERSION
            } else {
                set_error(e);
                G_ERR_INVALID
            }
        }
    }
}

// ── Validate ──────────────────────────────────────────────────────────────────

/// Validate a .gt text string.
///
/// Returns 1 if valid, 0 if invalid.
/// Call g_last_error() for details on validation failure.
#[unsafe(no_mangle)]
pub extern "C" fn g_validate(in_ptr: *const u8, in_len: usize) -> i32 {
    if in_ptr.is_null() {
        set_error("null pointer");
        return 0;
    }
    clear_error();

    let bytes = unsafe { slice::from_raw_parts(in_ptr, in_len) };
    let text = match std::str::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => {
            set_error(format!("invalid UTF-8: {}", e));
            return 0;
        }
    };

    let result = validate_text(text);
    if !result.valid {
        set_error(result.errors.join("; "));
        0
    } else {
        1
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version() {
        let mut maj = -1i32; let mut min = -1i32; let mut pat = -1i32;
        g_version(&mut maj, &mut min, &mut pat);
        assert_eq!(maj, 0); assert_eq!(min, 1); assert_eq!(pat, 0);
    }

    #[test]
    fn info_g28() {
        let mut bits = 0u32;
        let mut cs_len = 0usize;
        let rc = g_info(28, std::ptr::null_mut(), std::ptr::null_mut(), &mut bits, &mut cs_len);
        assert_eq!(rc, G_OK);
        assert_eq!(bits, 128);
        assert!(cs_len > 0);
    }

    #[test]
    fn encode_decode_roundtrip() {
        let data = b"hello G library world";
        let n = 28i32;

        let enc_cap = g_encode_max_size(data.len(), n);
        let mut enc_buf = vec![0u8; enc_cap];
        let mut enc_len = 0usize;

        let rc = g_encode(data.as_ptr(), data.len(), n,
                          enc_buf.as_mut_ptr(), enc_cap, &mut enc_len);
        assert_eq!(rc, G_OK, "encode failed");
        assert!(enc_len > 0);

        let dec_cap = g_decode_max_size(enc_len);
        let mut dec_buf = vec![0u8; dec_cap];
        let mut dec_len = 0usize;

        let rc = g_decode(enc_buf.as_ptr(), enc_len,
                          dec_buf.as_mut_ptr(), dec_cap, &mut dec_len);
        assert_eq!(rc, G_OK, "decode failed");
        assert_eq!(&dec_buf[..dec_len], data);
    }

    #[test]
    fn text_encode_decode_roundtrip() {
        let data = b"hello G text library";
        let n = 28i32;

        let enc_cap = g_encode_text_max_size(data.len(), n);
        let mut enc_buf = vec![0u8; enc_cap];
        let mut enc_len = 0usize;

        let rc = g_encode_text(data.as_ptr(), data.len(), n,
                               enc_buf.as_mut_ptr(), enc_cap, &mut enc_len);
        assert_eq!(rc, G_OK, "text encode failed");
        assert!(enc_len > 0);

        let dec_cap = g_decode_max_size(enc_len);
        let mut dec_buf = vec![0u8; dec_cap];
        let mut dec_len = 0usize;

        let rc = g_decode_text(enc_buf.as_ptr(), enc_len,
                               dec_buf.as_mut_ptr(), dec_cap, &mut dec_len);
        assert_eq!(rc, G_OK, "text decode failed");
        assert_eq!(&dec_buf[..dec_len], data);
    }

    #[test]
    fn validate_valid() {
        let data = b"test validate";
        let n = 28i32;
        let cap = g_encode_text_max_size(data.len(), n);
        let mut buf = vec![0u8; cap];
        let mut len = 0usize;
        g_encode_text(data.as_ptr(), data.len(), n, buf.as_mut_ptr(), cap, &mut len);
        assert_eq!(g_validate(buf.as_ptr(), len), 1);
    }

    #[test]
    fn validate_invalid() {
        let bad = b"not a gt file at all\n";
        assert_eq!(g_validate(bad.as_ptr(), bad.len()), 0);
    }

    #[test]
    fn null_pointer_returns_err() {
        let mut out_len = 0usize;
        let mut buf = vec![0u8; 1024];
        let rc = g_encode(std::ptr::null(), 10, 28,
                          buf.as_mut_ptr(), 1024, &mut out_len);
        assert_eq!(rc, G_ERR_NULL);
    }

    #[test]
    fn buffer_too_small_returns_err() {
        let data = b"hello";
        let mut buf = vec![0u8; 1]; // way too small
        let mut out_len = 0usize;
        let rc = g_encode(data.as_ptr(), data.len(), 28,
                          buf.as_mut_ptr(), 1, &mut out_len);
        assert_eq!(rc, G_ERR_BUFFER);
    }

    #[test]
    fn charset_g4() {
        let mut buf = vec![0u8; 64];
        let mut len = 0usize;
        let rc = g_charset(4, buf.as_mut_ptr(), 64, &mut len);
        assert_eq!(rc, G_OK);
        assert!(len > 0);
        let s = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(s.contains('0'));
        assert!(s.contains('1'));
        assert!(s.contains('a'));
        assert!(s.contains('A'));
    }
}
