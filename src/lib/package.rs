//! G package manager — installs libg and generates language bindings.
//!
//! On first run, `g install` (or any g command) will:
//!   1. Create ~/.g/{bin,lib,include,registry}
//!   2. Copy the g binary and libg.so/libg.a to ~/.g/
//!   3. Copy g.h to ~/.g/include/
//!
//! `g init` in a project directory:
//!   1. Detects the project language
//!   2. Writes a .gc config file
//!   3. Generates real, working language bindings that use ~/.g/lib/libg.so

use std::path::{Path, PathBuf};
use std::fs;
use std::io::{self, Write};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct GConfig {
    pub project: String,
    pub g_install: String,
    pub language: String,
    pub default_n: usize,
    pub version: String,
}

impl GConfig {
    pub fn load(path: &Path) -> Result<Self, String> {
        let s = fs::read_to_string(path).map_err(|e| e.to_string())?;
        serde_json::from_str(&s).map_err(|e| e.to_string())
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let s = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        fs::write(path, s).map_err(|e| e.to_string())
    }
}

pub fn g_home() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".g")
}

pub fn is_installed() -> bool {
    g_home().join("installed").exists()
}

/// First-run installation — copies binaries into ~/.g/
pub fn install() -> Result<(), String> {
    let home = g_home();
    println!();
    println!("  \x1b[1;36mG\x1b[0m — First run detected. Installing...");
    println!();

    for dir in &["bin", "lib", "include", "registry"] {
        fs::create_dir_all(home.join(dir))
            .map_err(|e| format!("Failed to create ~/.g/{}: {}", dir, e))?;
    }

    // Copy the running g binary to ~/.g/bin/g
    let current_exe = std::env::current_exe()
        .map_err(|e| format!("Cannot find current executable: {}", e))?;
    let bin_dest = home.join("bin").join("g");
    fs::copy(&current_exe, &bin_dest)
        .map_err(|e| format!("Cannot copy binary to {}: {}", bin_dest.display(), e))?;

    // Copy libg.so if it exists next to the binary (optional)
    let exe_dir = current_exe.parent().unwrap_or(Path::new("."));
    let lib_dest = home.join("lib");
    let mut found_lib = false;

    for libname in &["libg.so", "libg.a", "g.dll", "g.lib"] {
        let src = exe_dir.join(libname);
        if src.exists() {
            let dst = lib_dest.join(libname);
            fs::copy(&src, &dst)
                .map_err(|e| format!("Cannot copy {} to {}: {}", libname, dst.display(), e))?;
            println!("  \x1b[1;32m✓\x1b[0m Installed \x1b[33m{}\x1b[0m", libname);
            found_lib = true;
        }
    }

    if !found_lib {
        println!("  \x1b[33m!\x1b[0m libg not found — run \x1b[1mg build-lib\x1b[0m to compile it (requires cargo).");
    }

    // Write g.h to ~/.g/include/
    let header_dest = home.join("include").join("g.h");
    fs::write(&header_dest, G_HEADER)
        .map_err(|e| format!("Cannot write g.h: {}", e))?;
    println!("  \x1b[1;32m✓\x1b[0m Installed \x1b[33mg.h\x1b[0m");

    // Ask about project callable setup
    println!();
    println!("  \x1b[1mEnable project integration?\x1b[0m");
    println!("  This creates a \x1b[33m.gc\x1b[0m config in your project directories.");
    print!("  [y/N] > ");
    io::stdout().flush().ok();

    let mut input = String::new();
    io::stdin().read_line(&mut input).map_err(|e| e.to_string())?;
    let callable = input.trim().to_lowercase() == "y";

    // Write install marker and config
    fs::write(home.join("installed"), env!("CARGO_PKG_VERSION"))
        .map_err(|e| e.to_string())?;
    let cfg = serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "callable": callable,
        "default_n": 28,
    });
    fs::write(home.join("config.json"), serde_json::to_string_pretty(&cfg).unwrap())
        .map_err(|e| e.to_string())?;

    println!();
    println!("  \x1b[1;32m✓\x1b[0m G installed to \x1b[33m{}\x1b[0m", home.display());
    println!("  Run \x1b[1mg init\x1b[0m in any project to generate bindings.");
    println!();
    Ok(())
}

/// Initialize G in a project directory — generates real working bindings
pub fn init_project(project_dir: &Path) -> Result<(), String> {
    let home = g_home();
    let lib_path = home.join("lib");
    let include_path = home.join("include");

    println!();
    println!("  \x1b[1;36mG\x1b[0m — Initializing in \x1b[33m{}\x1b[0m", project_dir.display());
    println!();

    // Check libg is installed
    let libg_so = lib_path.join("libg.so");
    let libg_dll = lib_path.join("g.dll");
    if !libg_so.exists() && !libg_dll.exists() {
        println!("  \x1b[33m!\x1b[0m libg not found in ~/.g/lib/");
        println!("  \x1b[33m!\x1b[0m Copy libg.so and g.h next to the g binary, then re-run g install.");
        println!("  Generating bindings anyway — update the library path manually if needed.");
    }

    let language = detect_language(project_dir);
    println!("  Detected language: \x1b[1m{}\x1b[0m", language);
    print!("  Correct? [Y/n] > ");
    io::stdout().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).map_err(|e| e.to_string())?;
    let language = if input.trim().to_lowercase() == "n" {
        print!("  Language (rust/python/cpp): ");
        io::stdout().flush().ok();
        let mut lang = String::new();
        io::stdin().read_line(&mut lang).map_err(|e| e.to_string())?;
        lang.trim().to_string()
    } else {
        language
    };

    print!("  Default Gn instance [28]: ");
    io::stdout().flush().ok();
    let mut n_input = String::new();
    io::stdin().read_line(&mut n_input).map_err(|e| e.to_string())?;
    let default_n: usize = n_input.trim().parse().unwrap_or(28);

    let cfg = GConfig {
        project: project_dir.to_string_lossy().to_string(),
        g_install: home.to_string_lossy().to_string(),
        language: language.clone(),
        default_n,
        version: env!("CARGO_PKG_VERSION").to_string(),
    };
    cfg.save(&project_dir.join(".gc"))?;
    println!("  \x1b[1;32m✓\x1b[0m Created \x1b[33m.gc\x1b[0m");

    match language.as_str() {
        "rust"       => generate_rust_bindings(project_dir, default_n, &lib_path)?,
        "python"     => generate_python_bindings(project_dir, default_n, &lib_path)?,
        "cpp"|"c++"  => generate_cpp_bindings(project_dir, default_n, &lib_path, &include_path)?,
        other        => println!("  \x1b[33m!\x1b[0m Unknown language '{}' — skipping bindings", other),
    }

    // Register in ~/.g/registry
    let proj_name = project_dir.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let _ = fs::write(
        home.join("registry").join(format!("{}.json", proj_name)),
        serde_json::to_string_pretty(&serde_json::json!({
            "project": project_dir.to_string_lossy(),
            "language": language,
            "default_n": default_n,
        })).unwrap()
    );

    println!("  \x1b[1;32m✓\x1b[0m Registered project");
    println!();
    println!("  G is ready. Use \x1b[1mg encode\x1b[0m / \x1b[1mg decode\x1b[0m, or import G in your code.");
    println!();
    Ok(())
}

fn detect_language(dir: &Path) -> String {
    if dir.join("Cargo.toml").exists()          { return "rust".to_string(); }
    if dir.join("setup.py").exists()
    || dir.join("pyproject.toml").exists()      { return "python".to_string(); }
    if dir.join("CMakeLists.txt").exists()
    || dir.join("Makefile").exists()            { return "cpp".to_string(); }
    "rust".to_string()
}

// ── Rust bindings ────────────────────────────────────────────────────────────

fn generate_rust_bindings(dir: &Path, n: usize, lib_path: &Path) -> Result<(), String> {
    let content = format!(r#"//! G bindings — auto-generated by `g init`
//! Add to Cargo.toml:
//!   [build-dependencies]
//!   # (none needed — we link directly)
//!
//! Add to your Cargo.toml [dependencies]:
//!   # nothing — use FFI directly via this file
//!
//! Link flags (in build.rs):
//!   println!("cargo:rustc-link-search={lib}");
//!   println!("cargo:rustc-link-lib=g");

#[allow(non_camel_case_types)]
mod g_sys {{
    pub type c_int  = i32;
    pub type c_uint = u32;
    pub type c_char = u8;

    #[link(name = "g", kind = "dylib")]
    extern "C" {{
        pub fn g_version(major: *mut c_int, minor: *mut c_int, patch: *mut c_int);
        pub fn g_last_error() -> *const c_char;
        pub fn g_encode_max_size(in_len: usize, n: c_int) -> usize;
        pub fn g_decode_max_size(in_len: usize) -> usize;
        pub fn g_encode_text_max_size(in_len: usize, n: c_int) -> usize;
        pub fn g_encode(in_: *const u8, in_len: usize, n: c_int,
                        out: *mut u8, out_cap: usize, out_len: *mut usize) -> c_int;
        pub fn g_decode(in_: *const u8, in_len: usize,
                        out: *mut u8, out_cap: usize, out_len: *mut usize) -> c_int;
        pub fn g_encode_text(in_: *const u8, in_len: usize, n: c_int,
                             out: *mut u8, out_cap: usize, out_len: *mut usize) -> c_int;
        pub fn g_decode_text(in_: *const u8, in_len: usize,
                             out: *mut u8, out_cap: usize, out_len: *mut usize) -> c_int;
        pub fn g_validate(in_: *const u8, in_len: usize) -> c_int;
    }}
}}

pub const DEFAULT_N: i32 = {n};

pub fn encode(data: &[u8]) -> Result<Vec<u8>, String> {{
    unsafe {{
        let cap = g_sys::g_encode_max_size(data.len(), DEFAULT_N);
        let mut out = vec![0u8; cap];
        let mut out_len = 0usize;
        let rc = g_sys::g_encode(data.as_ptr(), data.len(), DEFAULT_N,
                                  out.as_mut_ptr(), cap, &mut out_len);
        if rc == 0 {{ out.truncate(out_len); Ok(out) }}
        else {{ Err(last_error()) }}
    }}
}}

pub fn decode(data: &[u8]) -> Result<Vec<u8>, String> {{
    unsafe {{
        let cap = g_sys::g_decode_max_size(data.len());
        let mut out = vec![0u8; cap];
        let mut out_len = 0usize;
        let rc = g_sys::g_decode(data.as_ptr(), data.len(),
                                  out.as_mut_ptr(), cap, &mut out_len);
        if rc == 0 {{ out.truncate(out_len); Ok(out) }}
        else {{ Err(last_error()) }}
    }}
}}

pub fn encode_text(data: &[u8]) -> Result<String, String> {{
    unsafe {{
        let cap = g_sys::g_encode_text_max_size(data.len(), DEFAULT_N);
        let mut out = vec![0u8; cap];
        let mut out_len = 0usize;
        let rc = g_sys::g_encode_text(data.as_ptr(), data.len(), DEFAULT_N,
                                       out.as_mut_ptr(), cap, &mut out_len);
        if rc == 0 {{
            out.truncate(out_len);
            String::from_utf8(out).map_err(|e| e.to_string())
        }} else {{ Err(last_error()) }}
    }}
}}

fn last_error() -> String {{
    unsafe {{
        let ptr = g_sys::g_last_error();
        if ptr.is_null() {{ return "unknown error".to_string(); }}
        std::ffi::CStr::from_ptr(ptr as *const std::ffi::c_char)
            .to_string_lossy().to_string()
    }}
}}
"#, n = n, lib = lib_path.display());

    fs::write(dir.join("g_bindings.rs"), &content).map_err(|e| e.to_string())?;

    // Generate build.rs
    let build_rs = format!(r#"fn main() {{
    println!("cargo:rustc-link-search={lib}");
    println!("cargo:rustc-link-lib=dylib=g");
    println!("cargo:rerun-if-changed=g_bindings.rs");
}}
"#, lib = lib_path.display());
    fs::write(dir.join("build.rs"), &build_rs).map_err(|e| e.to_string())?;

    println!("  \x1b[1;32m✓\x1b[0m Generated \x1b[33mg_bindings.rs\x1b[0m + \x1b[33mbuild.rs\x1b[0m");
    println!("    Include in your code: \x1b[36minclude!(\"g_bindings.rs\");\x1b[0m");
    Ok(())
}

// ── Python bindings ───────────────────────────────────────────────────────────

fn generate_python_bindings(dir: &Path, n: usize, lib_path: &Path) -> Result<(), String> {
    // Detect the library file that actually exists
    let lib_file = if lib_path.join("libg.so").exists() {
        lib_path.join("libg.so").to_string_lossy().to_string()
    } else if lib_path.join("g.dll").exists() {
        lib_path.join("g.dll").to_string_lossy().to_string()
    } else {
        lib_path.join("libg.so").to_string_lossy().to_string() // placeholder
    };

    let content = format!(r#""""
G encoding system Python bindings
Auto-generated by `g init` — do not edit manually.
Uses ctypes to call libg directly — no compilation needed.
"""
import ctypes
import os
import sys

# ── Load library ──────────────────────────────────────────────────────────────

_LIB_PATH = r"{lib}"

def _load_lib():
    if os.path.exists(_LIB_PATH):
        return ctypes.CDLL(_LIB_PATH)
    # Try to find it on LD_LIBRARY_PATH or standard locations
    for candidate in [
        os.path.join(os.path.expanduser("~"), ".g", "lib", "libg.so"),
        "/usr/local/lib/libg.so",
        "/usr/lib/libg.so",
    ]:
        if os.path.exists(candidate):
            return ctypes.CDLL(candidate)
    raise RuntimeError(
        f"Cannot find libg.so. Run `g install` first, or set _LIB_PATH in g.py.\n"
        f"Tried: {{_LIB_PATH}}"
    )

_lib = _load_lib()

# ── Type setup ────────────────────────────────────────────────────────────────

_lib.g_version.argtypes = [ctypes.POINTER(ctypes.c_int)] * 3
_lib.g_version.restype  = None

_lib.g_last_error.argtypes = []
_lib.g_last_error.restype  = ctypes.c_char_p

_lib.g_encode_max_size.argtypes = [ctypes.c_size_t, ctypes.c_int]
_lib.g_encode_max_size.restype  = ctypes.c_size_t

_lib.g_decode_max_size.argtypes = [ctypes.c_size_t]
_lib.g_decode_max_size.restype  = ctypes.c_size_t

_lib.g_encode_text_max_size.argtypes = [ctypes.c_size_t, ctypes.c_int]
_lib.g_encode_text_max_size.restype  = ctypes.c_size_t

_lib.g_encode.argtypes = [
    ctypes.c_char_p, ctypes.c_size_t, ctypes.c_int,
    ctypes.c_char_p, ctypes.c_size_t, ctypes.POINTER(ctypes.c_size_t)
]
_lib.g_encode.restype = ctypes.c_int

_lib.g_decode.argtypes = [
    ctypes.c_char_p, ctypes.c_size_t,
    ctypes.c_char_p, ctypes.c_size_t, ctypes.POINTER(ctypes.c_size_t)
]
_lib.g_decode.restype = ctypes.c_int

_lib.g_encode_text.argtypes = [
    ctypes.c_char_p, ctypes.c_size_t, ctypes.c_int,
    ctypes.c_char_p, ctypes.c_size_t, ctypes.POINTER(ctypes.c_size_t)
]
_lib.g_encode_text.restype = ctypes.c_int

_lib.g_decode_text.argtypes = [
    ctypes.c_char_p, ctypes.c_size_t,
    ctypes.c_char_p, ctypes.c_size_t, ctypes.POINTER(ctypes.c_size_t)
]
_lib.g_decode_text.restype = ctypes.c_int

_lib.g_validate.argtypes = [ctypes.c_char_p, ctypes.c_size_t]
_lib.g_validate.restype  = ctypes.c_int

# ── Constants ─────────────────────────────────────────────────────────────────

DEFAULT_N = {n}
G_OK          =  0
G_ERR_BUFFER  = -1
G_ERR_INVALID = -2
G_ERR_VERSION = -3
G_ERR_NULL    = -4
G_ERR_ENCODE  = -5

# ── Public API ────────────────────────────────────────────────────────────────

def version() -> tuple:
    """Return (major, minor, patch) version tuple."""
    maj, min_, pat = ctypes.c_int(), ctypes.c_int(), ctypes.c_int()
    _lib.g_version(ctypes.byref(maj), ctypes.byref(min_), ctypes.byref(pat))
    return (maj.value, min_.value, pat.value)

def last_error() -> str:
    """Return the last error message."""
    err = _lib.g_last_error()
    return err.decode() if err else ""

def encode(data: bytes, n: int = DEFAULT_N) -> bytes:
    """Encode bytes using G binary format."""
    if not isinstance(data, (bytes, bytearray)):
        raise TypeError("data must be bytes")
    cap = _lib.g_encode_max_size(len(data), n)
    buf = ctypes.create_string_buffer(cap)
    out_len = ctypes.c_size_t(0)
    rc = _lib.g_encode(data, len(data), n, buf, cap, ctypes.byref(out_len))
    if rc != G_OK:
        raise RuntimeError(f"g_encode failed: {{last_error()}}")
    return bytes(buf.raw[:out_len.value])

def decode(data: bytes) -> bytes:
    """Decode a G binary stream back to bytes."""
    if not isinstance(data, (bytes, bytearray)):
        raise TypeError("data must be bytes")
    cap = _lib.g_decode_max_size(len(data))
    buf = ctypes.create_string_buffer(cap)
    out_len = ctypes.c_size_t(0)
    rc = _lib.g_decode(data, len(data), buf, cap, ctypes.byref(out_len))
    if rc != G_OK:
        raise RuntimeError(f"g_decode failed: {{last_error()}}")
    return bytes(buf.raw[:out_len.value])

def encode_text(data: bytes, n: int = DEFAULT_N) -> str:
    """Encode bytes to G canonical text format (.gt)."""
    if not isinstance(data, (bytes, bytearray)):
        raise TypeError("data must be bytes")
    cap = _lib.g_encode_text_max_size(len(data), n)
    buf = ctypes.create_string_buffer(cap)
    out_len = ctypes.c_size_t(0)
    rc = _lib.g_encode_text(data, len(data), n, buf, cap, ctypes.byref(out_len))
    if rc != G_OK:
        raise RuntimeError(f"g_encode_text failed: {{last_error()}}")
    return buf.raw[:out_len.value].decode('utf-8')

def decode_text(gt_text: str) -> bytes:
    """Decode G canonical text format (.gt) back to bytes."""
    raw = gt_text.encode('utf-8')
    cap = _lib.g_decode_max_size(len(raw))
    buf = ctypes.create_string_buffer(cap)
    out_len = ctypes.c_size_t(0)
    rc = _lib.g_decode_text(raw, len(raw), buf, cap, ctypes.byref(out_len))
    if rc == G_ERR_VERSION:
        raise RuntimeError("Unsupported G text version — update your G installation")
    if rc != G_OK:
        raise RuntimeError(f"g_decode_text failed: {{last_error()}}")
    return bytes(buf.raw[:out_len.value])

def validate(gt_text: str) -> bool:
    """Validate a .gt text string. Returns True if valid."""
    raw = gt_text.encode('utf-8')
    return bool(_lib.g_validate(raw, len(raw)))

# ── Self-test ─────────────────────────────────────────────────────────────────

if __name__ == "__main__":
    print(f"G version: {{'.'.join(str(v) for v in version())}}")
    data = b"hello from Python via libg"
    enc = encode(data)
    dec = decode(enc)
    assert dec == data, f"roundtrip failed: {{dec!r}} != {{data!r}}"
    print(f"encode/decode: OK ({{len(data)}} -> {{len(enc)}} -> {{len(dec)}} bytes)")
    gt = encode_text(data)
    dec2 = decode_text(gt)
    assert dec2 == data
    print(f"encode_text/decode_text: OK")
    assert validate(gt)
    print(f"validate: OK")
    print("All tests passed.")
"#, n = n, lib = lib_file);

    fs::write(dir.join("g.py"), &content).map_err(|e| e.to_string())?;
    println!("  \x1b[1;32m✓\x1b[0m Generated \x1b[33mg.py\x1b[0m");
    println!("    Test with: \x1b[36mpython3 g.py\x1b[0m");
    println!("    Import:    \x1b[36mimport g; g.encode(data)\x1b[0m");
    Ok(())
}

// ── C++ bindings ──────────────────────────────────────────────────────────────

fn generate_cpp_bindings(dir: &Path, n: usize, lib_path: &Path, include_path: &Path) -> Result<(), String> {
    // Copy g.h if available
    let g_h_src = include_path.join("g.h");
    let g_h_dst = dir.join("g.h");
    if g_h_src.exists() {
        fs::copy(&g_h_src, &g_h_dst).map_err(|e| e.to_string())?;
        println!("  \x1b[1;32m✓\x1b[0m Copied \x1b[33mg.h\x1b[0m");
    } else {
        fs::write(&g_h_dst, G_HEADER).map_err(|e| e.to_string())?;
        println!("  \x1b[1;32m✓\x1b[0m Generated \x1b[33mg.h\x1b[0m");
    }

    let wrapper = format!(r#"/**
 * g.hpp — C++ wrapper for libg
 * Auto-generated by `g init`
 *
 * Compile: g++ your_file.cpp -o out -L{lib} -lg -Wl,-rpath,{lib}
 */
#pragma once
#include "g.h"
#include <vector>
#include <string>
#include <stdexcept>

namespace g {{

constexpr int DEFAULT_N = {n};

inline std::tuple<int,int,int> version() {{
    int maj, min, pat;
    g_version(&maj, &min, &pat);
    return {{maj, min, pat}};
}}

inline std::vector<uint8_t> encode(const uint8_t* data, size_t len, int n = DEFAULT_N) {{
    size_t cap = g_encode_max_size(len, n);
    std::vector<uint8_t> out(cap);
    size_t out_len = 0;
    int rc = g_encode(data, len, n, out.data(), cap, &out_len);
    if (rc != G_OK) throw std::runtime_error(g_last_error());
    out.resize(out_len);
    return out;
}}

inline std::vector<uint8_t> encode(const std::vector<uint8_t>& data, int n = DEFAULT_N) {{
    return encode(data.data(), data.size(), n);
}}

inline std::vector<uint8_t> decode(const uint8_t* data, size_t len) {{
    size_t cap = g_decode_max_size(len);
    std::vector<uint8_t> out(cap);
    size_t out_len = 0;
    int rc = g_decode(data, len, out.data(), cap, &out_len);
    if (rc != G_OK) throw std::runtime_error(g_last_error());
    out.resize(out_len);
    return out;
}}

inline std::vector<uint8_t> decode(const std::vector<uint8_t>& data) {{
    return decode(data.data(), data.size());
}}

inline std::string encode_text(const uint8_t* data, size_t len, int n = DEFAULT_N) {{
    size_t cap = g_encode_text_max_size(len, n);
    std::string out(cap, '\0');
    size_t out_len = 0;
    int rc = g_encode_text(data, len, n, (uint8_t*)out.data(), cap, &out_len);
    if (rc != G_OK) throw std::runtime_error(g_last_error());
    out.resize(out_len);
    return out;
}}

inline std::vector<uint8_t> decode_text(const std::string& gt) {{
    size_t cap = g_decode_max_size(gt.size());
    std::vector<uint8_t> out(cap);
    size_t out_len = 0;
    int rc = g_decode_text((const uint8_t*)gt.data(), gt.size(), out.data(), cap, &out_len);
    if (rc == G_ERR_VERSION) throw std::runtime_error("Unsupported G version");
    if (rc != G_OK) throw std::runtime_error(g_last_error());
    out.resize(out_len);
    return out;
}}

inline bool validate(const std::string& gt) {{
    return g_validate((const uint8_t*)gt.data(), gt.size()) == 1;
}}

}} // namespace g
"#, n = n, lib = lib_path.display());

    fs::write(dir.join("g.hpp"), &wrapper).map_err(|e| e.to_string())?;
    println!("  \x1b[1;32m✓\x1b[0m Generated \x1b[33mg.hpp\x1b[0m");
    println!("    Compile: \x1b[36mg++ your_file.cpp -L{} -lg -Wl,-rpath,{}\x1b[0m",
        lib_path.display(), lib_path.display());
    Ok(())
}

// ── Embedded C header ─────────────────────────────────────────────────────────

/// The g.h header file, embedded in the binary so g init works without internet.
const G_HEADER: &str = r#"/**
 * g.h — G encoding system C API  (v0.1.0)
 * Embedded in g binary — no download needed.
 */
#ifndef G_H
#define G_H
#ifdef __cplusplus
extern "C" {
#endif
#include <stdint.h>
#include <stddef.h>

#define G_OK          0
#define G_ERR_BUFFER  -1
#define G_ERR_INVALID -2
#define G_ERR_VERSION -3
#define G_ERR_NULL    -4
#define G_ERR_ENCODE  -5
#define G_DEFAULT_N   28

void        g_version(int *major, int *minor, int *patch);
const char *g_last_error(void);
size_t      g_encode_max_size(size_t in_len, int n);
size_t      g_decode_max_size(size_t in_len);
size_t      g_encode_text_max_size(size_t in_len, int n);
int         g_info(int n, uint64_t *valid_lo, uint64_t *valid_hi, uint32_t *bits, size_t *charset_len);
int         g_charset(int n, uint8_t *out, size_t out_cap, size_t *out_len);
int         g_encode(const uint8_t *in, size_t in_len, int n, uint8_t *out, size_t out_cap, size_t *out_len);
int         g_decode(const uint8_t *in, size_t in_len, uint8_t *out, size_t out_cap, size_t *out_len);
int         g_encode_text(const uint8_t *in, size_t in_len, int n, uint8_t *out, size_t out_cap, size_t *out_len);
int         g_decode_text(const uint8_t *in, size_t in_len, uint8_t *out, size_t out_cap, size_t *out_len);
int         g_validate(const uint8_t *in, size_t in_len);

#define G_CHECK(rc, label) do { if ((rc) != G_OK) goto label; } while(0)

#ifdef __cplusplus
}
#endif
#endif /* G_H */
"#;

/// Build libg from source and install to ~/.g/lib/
/// Requires cargo on PATH.
pub fn build_lib() -> Result<(), String> {
    let home = g_home();
    let lib_dest = home.join("lib");
    let inc_dest = home.join("include");
    fs::create_dir_all(&lib_dest).map_err(|e| e.to_string())?;
    fs::create_dir_all(&inc_dest).map_err(|e| e.to_string())?;

    println!();
    println!("  \x1b[1;36mG\x1b[0m \u{2014} Building libg from source...");

    // Check cargo exists
    let ok = std::process::Command::new("cargo").arg("--version").output()
        .map(|o| o.status.success()).unwrap_or(false);
    if !ok {
        return Err("cargo not found. Install Rust: https://rustup.rs".to_string());
    }

    let build_dir = std::env::temp_dir().join("gencode-libg-build");
    let src_dir = build_dir.join("src");
    fs::create_dir_all(&src_dir).map_err(|e| e.to_string())?;

    let version = env!("CARGO_PKG_VERSION");
    fs::write(build_dir.join("Cargo.toml"), format!(
        "[package]\nname = \"gencode-lib\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n\
         [lib]\nname = \"g\"\ncrate-type = [\"cdylib\", \"staticlib\"]\n\n\
         [dependencies]\ngencode = \"{}\"", version
    )).map_err(|e| e.to_string())?;

    fs::write(src_dir.join("lib.rs"),
        "pub use gencode::ffi::*;\n"
    ).map_err(|e| e.to_string())?;

    println!("  Building gencode v{} (this may take a minute)...", version);

    let status = std::process::Command::new("cargo")
        .args(["build", "--release"])
        .current_dir(&build_dir)
        .status()
        .map_err(|e| format!("Failed to run cargo: {}", e))?;

    if !status.success() {
        let _ = fs::remove_dir_all(&build_dir);
        return Err("Build failed. Check output above.".to_string());
    }

    let release_dir = build_dir.join("target").join("release");
    let mut installed = false;

    for name in &["libg.so", "libg.a", "g.dll", "g.lib"] {
        let src = release_dir.join(name);
        if src.exists() {
            let dst = lib_dest.join(name);
            fs::copy(&src, &dst).map_err(|e| e.to_string())?;
            println!("  \x1b[1;32m\u{2713}\x1b[0m {}", dst.display());
            installed = true;
        }
    }

    let header_dst = inc_dest.join("g.h");
    fs::write(&header_dst, G_HEADER).map_err(|e| e.to_string())?;
    println!("  \x1b[1;32m\u{2713}\x1b[0m {}", header_dst.display());
    let _ = fs::remove_dir_all(&build_dir);

    if !installed {
        return Err("No library files produced. This is a bug.".to_string());
    }

    println!();
    println!("  \x1b[1;32mDone!\x1b[0m Run \x1b[1mg init\x1b[0m to generate language bindings.");
    println!();
    Ok(())
}
