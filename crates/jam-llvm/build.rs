/*
 * Copyright (c) 2026-present Raphael Amorim
 *
 * This file is part of jam.
 * Licensed under the Apache License, Version 2.0 with LLVM Exceptions.
 */

//! Build script: locate the system LLVM via `llvm-config` and emit the link
//! directives for libLLVM. We link the C API only (the symbols our hand-rolled
//! `extern "C"` FFI in `src/raw.rs` declares) — no C++ toolchain participates
//! in our build, just the prebuilt library. LLD (the embedded linker, see
//! `shim/jam_lld.cpp`) is linked the same way.
//!
//! The conventions here deliberately mirror the established Rust↔LLVM crates
//! (`llvm-sys`, which inkwell and most others link through, and rustc's own
//! `rustc_llvm`):
//!   * locate `llvm-config` via `$LLVM_CONFIG`, then the versioned
//!     `$LLVM_SYS_221_PREFIX/bin` convention, then `PATH`, then Homebrew;
//!   * locate LLD via `$JAM_LLD_PREFIX`, then the LLVM prefix, then Homebrew;
//!   * pass an explicit `--link-shared` / `--link-static` to `llvm-config` so
//!     `--libs`/`--libdir`/`--system-libs` all answer for the same mode;
//!   * choose the C++ runtime by platform (only needed for a static link);
//!   * handle `--system-libs` entries that are absolute paths, not just `-lx`.
//!
//! Release archives are built with `JAM_LLVM_LINK_SHARED=0` (static libLLVM)
//! and `JAM_LLVM_STATIC_SYSLIBS` (see below), so the shipped binary does not
//! need an LLVM install; see `.github/workflows/release.yaml`.
//!

// note: targets LLVM 22.1

#![allow(clippy::collapsible_if)]

use std::path::PathBuf;
use std::process::Command;

const LLVM_SYS_PREFIX: &str = "LLVM_SYS_221_PREFIX";

fn run(bin: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(bin).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn locate_llvm_config() -> String {
    if let Ok(explicit) = std::env::var("LLVM_CONFIG") {
        if run(&explicit, &["--version"]).is_some() {
            return explicit;
        }
    }
    if let Ok(prefix) = std::env::var(LLVM_SYS_PREFIX) {
        let cand = PathBuf::from(prefix).join("bin").join("llvm-config");
        let cand = cand.to_string_lossy().into_owned();
        if run(&cand, &["--version"]).is_some() {
            return cand;
        }
    }
    for cand in [
        "llvm-config",
        // The versioned keg first: Homebrew's lld@22 links against it, and
        // jam must load the same libLLVM as the lld it embeds.
        "/opt/homebrew/opt/llvm@22/bin/llvm-config",
        "/usr/local/opt/llvm@22/bin/llvm-config",
        "/opt/homebrew/opt/llvm/bin/llvm-config",
        "/usr/local/opt/llvm/bin/llvm-config",
    ] {
        if run(cand, &["--version"]).is_some() {
            return cand.to_string();
        }
    }
    panic!(
        "llvm-config not found. Install LLVM 22 (e.g. `brew install llvm@22`) and put it \
         on PATH, or set LLVM_CONFIG / {LLVM_SYS_PREFIX}."
    );
}

/// Find the LLD install (the prefix holding `include/lld` and `lib/liblld*`):
/// `$JAM_LLD_PREFIX`, then the LLVM prefix itself (apt and source builds put
/// lld there), then Homebrew's separate keg.
fn locate_lld(llvm_config: &str) -> PathBuf {
    let mut cands: Vec<PathBuf> = Vec::new();
    if let Ok(p) = std::env::var("JAM_LLD_PREFIX") {
        cands.push(PathBuf::from(p));
    }
    if let Some(p) = run(llvm_config, &["--prefix"]) {
        cands.push(PathBuf::from(p));
    }
    for p in [
        "/opt/homebrew/opt/lld@22",
        "/usr/local/opt/lld@22",
        "/opt/homebrew/opt/lld",
        "/usr/local/opt/lld",
    ] {
        cands.push(PathBuf::from(p));
    }
    for cand in cands {
        if cand.join("include/lld/Common/Driver.h").exists() {
            return cand;
        }
    }
    panic!(
        "LLD development files not found. Install LLD 22 (e.g. `brew install lld@22` or \
         `apt install liblld-22-dev`), or set JAM_LLD_PREFIX to its install prefix."
    );
}

/// Emit a link directive for one token from `--libs` / `--system-libs`, which
/// may be `-lfoo` or an absolute path to a `.a`/`.so`/`.dylib`/`.tbd`.
fn link_token(tok: &str, default_kind: &str) {
    if let Some(name) = tok.strip_prefix("-l") {
        println!("cargo:rustc-link-lib={default_kind}={name}");
        return;
    }
    let path = PathBuf::from(tok);
    if path.is_absolute() {
        if let Some(dir) = path.parent() {
            println!("cargo:rustc-link-search=native={}", dir.display());
        }
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            let name = stem.strip_prefix("lib").unwrap_or(stem);
            let kind = if tok.ends_with(".a") {
                "static"
            } else {
                "dylib"
            };
            println!("cargo:rustc-link-lib={kind}={name}");
        }
    }
}

fn main() {
    for var in [
        "LLVM_CONFIG",
        LLVM_SYS_PREFIX,
        "CXXSTDLIB",
        "JAM_LLVM_LINK_SHARED",
        "JAM_LLVM_STATIC_SYSLIBS",
        "JAM_LLD_PREFIX",
    ] {
        println!("cargo:rerun-if-env-changed={var}");
    }
    println!("cargo:rerun-if-changed=build.rs");

    let lc = locate_llvm_config();

    let version = run(&lc, &["--version"]).unwrap_or_default();
    if !version.starts_with("22.") {
        println!("cargo:warning=jam_llvm targets LLVM 22.x; llvm-config reports {version}");
    }

    // Compile the C++ shims (the TargetOptions/pipeline gaps in the C API, and
    // the embedded LLD entry point). cc(true) also emits the C++-stdlib link
    // directive. We pass llvm-config's own --cxxflags so the shims are built
    // with the same include path, language std, and defines LLVM itself uses.
    //
    // This runs before any LLVM link directive on purpose: with static
    // archives a GNU linker resolves left to right, so the shim (which calls
    // into lld and LLVM) has to come first, then lld, then LLVM.
    let lld = locate_lld(&lc);
    let cxxflags = run(&lc, &["--cxxflags"]).unwrap_or_default();
    let mut shim = cc::Build::new();
    // Don't add -Wall/-Wextra: we'd only be warned about LLVM's own headers.
    shim.cpp(true)
        .warnings(false)
        .file("shim/jam_shim.cpp")
        .file("shim/jam_lld.cpp")
        .include(lld.join("include"));
    for tok in cxxflags.split_whitespace() {
        shim.flag(tok);
    }
    shim.compile("jam_shim");
    println!("cargo:rerun-if-changed=shim/jam_shim.cpp");
    println!("cargo:rerun-if-changed=shim/jam_lld.cpp");

    // LLD's Mach-O and ELF drivers. Shared when the install ships dylibs
    // (Homebrew), static otherwise (apt, a source build).
    let lld_lib = lld.join("lib");
    println!("cargo:rustc-link-search=native={}", lld_lib.display());
    for name in ["lldMachO", "lldELF", "lldCommon"] {
        let shared = ["dylib", "so"]
            .iter()
            .any(|ext| lld_lib.join(format!("lib{name}.{ext}")).exists());
        let kind = if shared { "dylib" } else { "static" };
        println!("cargo:rustc-link-lib={kind}={name}");
    }

    // Prefer the monolithic shared libLLVM (what the jam toolchain links). Fall
    // back to a static link if this LLVM was built static-only. Override with
    // JAM_LLVM_LINK_SHARED=0.
    let want_shared = std::env::var("JAM_LLVM_LINK_SHARED")
        .map(|v| v != "0")
        .unwrap_or(true);
    let shared_ok = want_shared
        && run(&lc, &["--link-shared", "--libs"])
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
    let (link_flag, kind) = if shared_ok {
        ("--link-shared", "dylib")
    } else {
        ("--link-static", "static")
    };

    // All three queries pinned to the chosen mode so their answers agree.
    let libdir = run(&lc, &[link_flag, "--libdir"])
        .or_else(|| run(&lc, &["--libdir"]))
        .expect("llvm-config --libdir failed");
    println!("cargo:rustc-link-search=native={libdir}");

    let mut linked_any = false;
    if let Some(libs) = run(&lc, &[link_flag, "--libs"]) {
        for tok in libs.split_whitespace() {
            link_token(tok, kind);
            linked_any = true;
        }
    }
    if !linked_any {
        println!("cargo:rustc-link-lib=dylib=LLVM");
    }

    // System libraries libLLVM depends on (zlib, zstd, curses, libxml2, …).
    // Empty for a stock shared build; populated for static / Linux.
    //
    // They link dynamically unless named in JAM_LLVM_STATIC_SYSLIBS (a comma
    // list, e.g. `zstd,z`). Release builds use that to fold in the libraries
    // a user's machine may not have, so the binary is self-contained; the
    // `.a` files are found through the usual `-L` search path.
    let static_syslibs = std::env::var("JAM_LLVM_STATIC_SYSLIBS").unwrap_or_default();
    let static_syslibs: Vec<&str> = static_syslibs
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if let Some(sys) = run(&lc, &[link_flag, "--system-libs"]) {
        for tok in sys.split_whitespace() {
            let is_static = tok
                .strip_prefix("-l")
                .is_some_and(|name| static_syslibs.contains(&name));
            link_token(tok, if is_static { "static" } else { "dylib" });
        }
    }

    // The C++ runtime. A shared libLLVM carries this in its own load commands,
    // so it is only our responsibility for a static link. Choice is by platform
    // (matching llvm-sys/rustc), overridable via CXXSTDLIB.
    if kind == "static" {
        if let Some(cxx) = cxx_stdlib() {
            println!("cargo:rustc-link-lib=dylib={cxx}");
        }
    }

    // Make a shared libLLVM discoverable at runtime without
    // DYLD_LIBRARY_PATH / LD_LIBRARY_PATH. Harmless for a static link.
    println!("cargo:rustc-link-arg=-Wl,-rpath,{libdir}");
}

/// The C++ standard library to link against a static libLLVM. `None` on MSVC,
/// where it is implicit.
fn cxx_stdlib() -> Option<String> {
    if let Ok(over) = std::env::var("CXXSTDLIB") {
        return if over.is_empty() { None } else { Some(over) };
    }
    let target = std::env::var("TARGET").unwrap_or_default();
    if target.contains("msvc") {
        None
    } else if target.contains("apple")
        || target.contains("darwin")
        || target.contains("freebsd")
        || target.contains("openbsd")
    {
        Some("c++".to_string())
    } else {
        Some("stdc++".to_string())
    }
}
