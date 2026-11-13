/*
 * Copyright (c) 2026-present Raphael Amorim
 *
 * This file is part of jam.
 * Licensed under the Apache License, Version 2.0 with LLVM Exceptions.
 */

//! The link step. jam links with the LLD it embeds (`jam_llvm::lld_link`)
//! against the C library pieces it ships under `lib/jam/libc`, so building a
//! jam program needs no system compiler or SDK, and any host can build for
//! any of these targets:
//!
//!   * macOS: `libc/darwin/libSystem.tbd`, a text stub of the system library
//!     every Mach-O executable loads from the OS at run time;
//!   * Linux: `libc/musl/<arch>-linux-musl/`, musl's startup objects, a static
//!     `libc.a` and the compiler-rt builtins. Executables are fully static.
//!
//! No system compiler driver is ever spawned. `-l<name>` libraries are
//! resolved by LLD itself, through the `-L` directories and, on macOS, the
//! installed SDK when there is one (only libSystem is bundled).

use std::path::{Path, PathBuf};

use jam_llvm::{Lto, OptLevel, default_target_triple, target_triple_is_overridden};
use jam_sema::target::{Arch, Os, Target};

use crate::cli::emit::Strip;

/// Deployment target for cross builds to macOS, per architecture.
const MACOS_MIN_ARM64: &str = "11.0";
const MACOS_MIN_X86_64: &str = "10.13";
/// The SDK version the bundled `libSystem.tbd` was generated from.
const MACOS_SDK: &str = "26.1";
/// `-L` directories, `:`-separated. The CLI appends each `-L` here, the same
/// way `--std-path` sets `JAM_STD_PATH`, so test workers inherit them.
pub const LIBRARY_PATH_ENV: &str = "JAM_LIBRARY_PATH";

/// The triple objects are emitted for. On macOS it spells out the deployment
/// target (the oldest OS the program runs on) as `macosx<version>`, since
/// LLVM's host triple only carries the kernel version (`arm64-apple-darwin25`).
/// In order of precedence:
///
///   1. a `macosx<version>` the user wrote in `-C target`;
///   2. `MACOSX_DEPLOYMENT_TARGET`, the convention clang and rustc follow;
///   3. for a native build, the OS version of this machine (what clang and
///      Zig default to), so libraries installed here link without a
///      "newer than target minimum" warning;
///   4. for a cross build, a fixed per-architecture minimum.
pub fn deployment_triple(triple: &str) -> String {
    let host = Target::from_triple_str(triple);
    if host.os != Os::MacOs || macos_version_in(triple).is_some() {
        return triple.to_string();
    }
    let from_env = deployment_target_env();
    let native = if target_triple_is_overridden() {
        None
    } else {
        host_macos_version()
    };
    let Some(min) = from_env
        .or(native)
        .or(macos_default_min(host.arch).map(str::to_string))
    else {
        return triple.to_string();
    };
    let mut parts: Vec<String> = triple.split('-').map(str::to_string).collect();
    for part in parts.iter_mut().skip(1) {
        if part.starts_with("darwin") || part.starts_with("macosx") {
            *part = format!("macosx{min}");
        }
    }
    parts.join("-")
}

/// Whether the user asked for a specific macOS deployment target (cases 1
/// and 2 above). If not, jam chose it, and a library built for a newer OS is
/// not a mismatch worth warning about: the linker's "newer than target
/// minimum" warnings are silenced. Typical trigger: a library compiled on
/// this machine targets its SDK version (27.2), a point release ahead of the
/// OS version (27.0) jam targets by default.
fn macos_target_is_explicit() -> bool {
    macos_version_in(&default_target_triple()).is_some() || deployment_target_env().is_some()
}

fn deployment_target_env() -> Option<String> {
    std::env::var("MACOSX_DEPLOYMENT_TARGET")
        .ok()
        .filter(|v| v.starts_with(|c: char| c.is_ascii_digit()))
}

/// This machine's macOS version as `major.minor` (`kern.osproductversion`).
#[cfg(target_os = "macos")]
fn host_macos_version() -> Option<String> {
    unsafe extern "C" {
        fn sysctlbyname(
            name: *const std::ffi::c_char,
            oldp: *mut std::ffi::c_void,
            oldlenp: *mut usize,
            newp: *mut std::ffi::c_void,
            newlen: usize,
        ) -> std::ffi::c_int;
    }
    let mut buf = [0u8; 32];
    let mut len = buf.len();
    let rc = unsafe {
        sysctlbyname(
            c"kern.osproductversion".as_ptr(),
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    let version = std::ffi::CStr::from_bytes_until_nul(&buf)
        .ok()?
        .to_str()
        .ok()?;
    let mut fields = version.split('.');
    let major = fields.next().filter(|f| !f.is_empty())?;
    let minor = fields.next().unwrap_or("0");
    if !major
        .bytes()
        .chain(minor.bytes())
        .all(|b| b.is_ascii_digit())
    {
        return None;
    }
    Some(format!("{major}.{minor}"))
}

#[cfg(not(target_os = "macos"))]
fn host_macos_version() -> Option<String> {
    None
}

/// The `<version>` of a `macosx<version>` triple component, if spelled out.
fn macos_version_in(triple: &str) -> Option<&str> {
    triple
        .split('-')
        .filter_map(|p| p.strip_prefix("macosx"))
        .find(|v| v.starts_with(|c: char| c.is_ascii_digit()))
}

fn macos_default_min(arch: Arch) -> Option<&'static str> {
    match arch {
        Arch::AArch64 => Some(MACOS_MIN_ARM64),
        Arch::X86_64 => Some(MACOS_MIN_X86_64),
        _ => None,
    }
}

/// How one executable gets linked. Built once per compile; `key` feeds the
/// test-binary cache so a change of flags relinks.
pub struct LinkPlan {
    kind: Kind,
}

enum Kind {
    /// In-process LLD: `<pre> -o <out> <before_obj> <obj> <after_obj>`.
    /// `hint` is printed when the link fails. `quiet_min_version` silences
    /// "newer than target minimum" warnings (see `macos_target_is_explicit`).
    Embedded {
        pre: Vec<String>,
        before_obj: Vec<String>,
        after_obj: Vec<String>,
        hint: Option<String>,
        quiet_min_version: bool,
    },
    /// jam cannot link for this target; the message says why.
    Unavailable(String),
}

impl LinkPlan {
    /// `triple` is the one the object was emitted for.
    pub fn new(triple: &str, libs: &[String], opt: OptLevel, lto: Lto, strip: Strip) -> LinkPlan {
        let host = Target::from_triple_str(triple);
        let kind = match libc_root() {
            Some(root) => match host.os {
                Os::MacOs => macho_plan(&host, triple, &root, libs, opt, lto, strip),
                Os::Linux => elf_musl_plan(&host, triple, &root, libs, opt, lto, strip),
                _ => Kind::Unavailable(format!("jam cannot link programs for `{triple}` yet")),
            },
            None => Kind::Unavailable(
                "jam's bundled C library files were not found; expected `lib/jam/libc` next \
                 to the jam binary (set JAM_LIBC_PATH to point at them)"
                    .to_string(),
            ),
        };
        LinkPlan { kind }
    }

    /// Every argument that shapes the output, for cache keys.
    pub fn key(&self) -> Vec<&str> {
        match &self.kind {
            Kind::Embedded {
                pre,
                before_obj,
                after_obj,
                ..
            } => pre
                .iter()
                .chain(before_obj)
                .chain(after_obj)
                .map(String::as_str)
                .collect(),
            Kind::Unavailable(why) => vec![why.as_str()],
        }
    }

    /// Link `obj` into the executable `out`. Does NOT remove the object. On
    /// failure prints `Linking failed` and returns 1.
    pub fn link(&self, obj: &str, out: &str) -> i32 {
        let ok = match &self.kind {
            Kind::Embedded {
                pre,
                before_obj,
                after_obj,
                hint,
                quiet_min_version,
            } => {
                let mut args = pre.clone();
                args.push("-o".to_string());
                args.push(out.to_string());
                args.extend(before_obj.iter().cloned());
                args.push(obj.to_string());
                args.extend(after_obj.iter().cloned());
                let ok = jam_llvm::lld_link(&args, *quiet_min_version) == 0;
                if !ok && let Some(hint) = hint {
                    eprintln!("note: {hint}");
                }
                ok
            }
            Kind::Unavailable(why) => {
                eprintln!("error: {why}");
                false
            }
        };
        if ok {
            0
        } else {
            eprintln!("Linking failed");
            1
        }
    }
}

/// The bundled libc root: `JAM_LIBC_PATH`, else the first ancestor of the
/// running binary holding `lib/jam/libc` (an install or a release archive) or
/// `libc/darwin` (a build inside the source tree).
fn libc_root() -> Option<PathBuf> {
    static ROOT: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| {
        if let Ok(p) = std::env::var("JAM_LIBC_PATH")
            && !p.is_empty()
        {
            return Some(PathBuf::from(p));
        }
        let exe = std::env::current_exe().ok()?;
        let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
        let mut cur = exe.as_path();
        while let Some(parent) = cur.parent() {
            let installed = parent.join("lib").join("jam").join("libc");
            if installed.is_dir() {
                return Some(installed);
            }
            let in_tree = parent.join("libc");
            if in_tree.join("darwin").join("libSystem.tbd").is_file() {
                return Some(in_tree);
            }
            cur = parent;
        }
        None
    })
    .clone()
}

fn path_arg(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// The `-L` directories given on the command line, in order.
fn library_dirs() -> Vec<String> {
    std::env::var(LIBRARY_PATH_ENV)
        .unwrap_or_default()
        .split(':')
        .filter(|d| !d.is_empty())
        .map(str::to_string)
        .collect()
}

/// The installed macOS SDK, if any: `$SDKROOT`, the command-line tools, then
/// Xcode. Only consulted when the program links `-l` libraries.
fn macos_sdk() -> Option<PathBuf> {
    let mut cands: Vec<PathBuf> = Vec::new();
    if let Ok(p) = std::env::var("SDKROOT")
        && !p.is_empty()
    {
        cands.push(PathBuf::from(p));
    }
    cands.push(PathBuf::from(
        "/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk",
    ));
    cands.push(PathBuf::from(
        "/Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk",
    ));
    cands
        .into_iter()
        .find(|p| p.join("usr/lib/libSystem.tbd").is_file())
}

fn macho_plan(
    host: &Target,
    triple: &str,
    root: &Path,
    libs: &[String],
    opt: OptLevel,
    lto: Lto,
    strip: Strip,
) -> Kind {
    let darwin = root.join("darwin");
    if !darwin.join("libSystem.tbd").is_file() {
        return Kind::Unavailable(format!(
            "jam's bundled `libSystem.tbd` is missing from `{}`",
            darwin.display()
        ));
    }
    let arch = match host.arch {
        Arch::AArch64 => "arm64",
        Arch::X86_64 => "x86_64",
        _ => {
            return Kind::Unavailable(format!("jam cannot link programs for `{triple}` yet"));
        }
    };
    let min = macos_version_in(triple)
        .or(macos_default_min(host.arch))
        .unwrap_or(MACOS_MIN_ARM64);
    let mut pre: Vec<String> = ["ld64.lld", "-arch", arch, "-platform_version", "macos", min]
        .iter()
        .map(|s| s.to_string())
        .collect();
    pre.push(MACOS_SDK.to_string());

    // A program with no `-l` libraries links against the bundled stub alone.
    // One that names libraries needs more than jam ships, so it links against
    // the installed SDK when there is one. Either way the directory is also
    // the `-syslibroot`: libSystem re-exports `/usr/lib/system/*.dylib`, and
    // rooting those paths keeps LLD from reading the few that exist on the
    // build machine's disk instead of the stubs.
    let sdk = if libs.is_empty() { None } else { macos_sdk() };
    let mut after_obj: Vec<String> = Vec::new();
    for dir in library_dirs() {
        after_obj.push(format!("-L{dir}"));
    }
    pre.push("-syslibroot".to_string());
    match &sdk {
        Some(sdk) => pre.push(path_arg(sdk)),
        None => {
            pre.push(path_arg(&darwin));
            after_obj.push(format!("-L{}", path_arg(&darwin)));
        }
    }
    if !libs.is_empty() && Path::new("/usr/local/lib").is_dir() {
        after_obj.push("-L/usr/local/lib".to_string());
    }
    for lib in libs {
        after_obj.push(format!("-l{lib}"));
    }
    after_obj.push("-lSystem".to_string());

    if lto != Lto::Off {
        pre.push(format!("--lto-O{}", lto_level(opt)));
    }
    // Strip unreferenced functions/data. Pairs with FunctionSections /
    // DataSections on the TargetMachine. Skipped in debug to keep link fast.
    if !matches!(opt, OptLevel::None) {
        pre.push("-dead_strip".to_string());
    }
    // `-C strip`: `-S` drops debug info, `-x` local symbols too.
    if strip != Strip::None {
        pre.push("-S".to_string());
        if strip == Strip::Symbols {
            pre.push("-x".to_string());
        }
    }
    let hint = (!libs.is_empty() && sdk.is_none()).then(|| {
        "no macOS SDK was found, so `-l` libraries are only searched in the `-L` \
         directories and /usr/local/lib; install the command-line tools \
         (`xcode-select --install`) or set SDKROOT to link system libraries"
            .to_string()
    });
    Kind::Embedded {
        pre,
        before_obj: Vec::new(),
        after_obj,
        hint,
        quiet_min_version: !macos_target_is_explicit(),
    }
}

/// A static musl link, the argument order Zig and `musl-gcc -static` use:
/// `crt1.o crti.o <obj> <libs> libc.a libcompiler_rt.a crtn.o`.
fn elf_musl_plan(
    host: &Target,
    triple: &str,
    root: &Path,
    libs: &[String],
    opt: OptLevel,
    lto: Lto,
    strip: Strip,
) -> Kind {
    let (dir, emulation) = match host.arch {
        Arch::X86_64 => ("x86_64-linux-musl", "elf_x86_64"),
        Arch::AArch64 => ("aarch64-linux-musl", "aarch64linux"),
        _ => {
            return Kind::Unavailable(format!("jam cannot link programs for `{triple}` yet"));
        }
    };
    let sysroot = root.join("musl").join(dir);
    let file = |name: &str| path_arg(&sysroot.join(name));
    if !["crt1.o", "crti.o", "crtn.o", "libc.a"]
        .iter()
        .all(|f| sysroot.join(f).is_file())
    {
        return Kind::Unavailable(format!(
            "jam has no C library files for `{triple}`: expected crt1.o, crti.o, crtn.o and \
             libc.a in `{}`",
            sysroot.display()
        ));
    }
    let mut pre: Vec<String> = ["ld.lld", "-m", emulation, "-static", "--eh-frame-hdr"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    if lto != Lto::Off {
        pre.push(format!("--lto-O{}", lto_level(opt)));
    }
    if !matches!(opt, OptLevel::None) {
        pre.push("--gc-sections".to_string());
    }
    match strip {
        Strip::None => {}
        Strip::Symbols => pre.push("-s".to_string()),
        Strip::DebugInfo => pre.push("--strip-debug".to_string()),
    }
    let mut after_obj: Vec<String> = Vec::new();
    for dir in library_dirs() {
        after_obj.push(format!("-L{dir}"));
    }
    for lib in libs {
        after_obj.push(format!("-l{lib}"));
    }
    // libc and the compiler-rt builtins (128-bit integer and float helpers)
    // refer to each other, hence the group. musl itself needs the builtins on
    // aarch64, where `long double` is a soft-float 128-bit type.
    after_obj.push("--start-group".to_string());
    after_obj.push(file("libc.a"));
    if sysroot.join("libcompiler_rt.a").is_file() {
        after_obj.push(file("libcompiler_rt.a"));
    }
    after_obj.push("--end-group".to_string());
    after_obj.push(file("crtn.o"));
    let hint = (!libs.is_empty()).then(|| {
        "Linux programs are linked statically against musl, so `-l` can only use static \
         libraries (`lib<name>.a`) found in the `-L` directories"
            .to_string()
    });
    Kind::Embedded {
        pre,
        before_obj: vec![file("crt1.o"), file("crti.o")],
        after_obj,
        hint,
        quiet_min_version: false,
    }
}

/// LLD's `--lto-O<n>` takes 0..=3; size levels optimize like `-O2`.
fn lto_level(opt: OptLevel) -> u8 {
    match opt {
        OptLevel::None => 0,
        OptLevel::Less => 1,
        OptLevel::Aggressive => 3,
        _ => 2,
    }
}
