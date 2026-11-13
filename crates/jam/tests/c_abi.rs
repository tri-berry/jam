/*
 * Copyright (c) 2026-present Raphael Amorim
 *
 * This file is part of jam.
 * Licensed under the Apache License, Version 2.0 with LLVM Exceptions.
 */

//! C ABI conformance, after Zig's `test/c_abi`: `c_abi/cfuncs.c` (built
//! with the system C compiler, so the C side is ground truth) and
//! `c_abi/main.jam` call each other with structs by value in both
//! directions. Each check prints `ok` / `FAIL`; the binary's exit code is
//! the failure count.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Scratch directory removed on drop, so a failed assertion doesn't leak it.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Number of checks a source declares: `check(t, "` in Jam, `CHECK("` in C.
fn count_checks(path: &Path, needle: &str) -> usize {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter(|l| l.trim_start().starts_with(needle))
        .count()
}

#[test]
fn structs_by_value_cross_the_c_boundary_both_ways() {
    if Command::new("cc").arg("--version").output().is_err() {
        eprintln!("skipping: no C compiler (`cc`) on PATH");
        return;
    }
    let jam = env!("CARGO_BIN_EXE_jam");
    let manifest = env!("CARGO_MANIFEST_DIR");
    let repo = format!("{manifest}/../..");
    let src = Path::new(manifest).join("tests/c_abi");
    let dir = TempDir(std::env::temp_dir().join(format!("jam_c_abi_{}", std::process::id())));
    std::fs::create_dir_all(&dir.0).unwrap();

    let obj = dir.0.join("cfuncs.o");
    let cc = Command::new("cc")
        // No fortified libc calls (`__printf_chk`): on Linux jam links
        // against musl, which does not provide glibc's checked variants.
        .args(["-O1", "-c", "-U_FORTIFY_SOURCE"])
        .arg(src.join("cfuncs.c"))
        .arg("-o")
        .arg(&obj)
        .output()
        .expect("spawn cc");
    assert!(
        cc.status.success(),
        "cc failed:\n{}",
        String::from_utf8_lossy(&cc.stderr)
    );
    let ar = Command::new("ar")
        .arg("rcs")
        .arg(dir.0.join("libcfuncs.a"))
        .arg(&obj)
        .status()
        .expect("spawn ar");
    assert!(ar.success(), "ar failed");

    let bin = dir.0.join("c_abi_bin");
    // `-L` points jam's linker at the archive.
    let compile = Command::new(jam)
        .arg("build")
        .arg(src.join("main.jam"))
        .arg("-o")
        .arg(&bin)
        .arg("-L")
        .arg(&dir.0)
        .arg("-lcfuncs")
        .env("JAM_STD_PATH", format!("{repo}/std"))
        .output()
        .expect("spawn jam");
    assert!(
        compile.status.success(),
        "compile failed:\n{}{}",
        String::from_utf8_lossy(&compile.stdout),
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&bin).output().expect("run c_abi binary");
    let out = String::from_utf8_lossy(&run.stdout);
    let failed: Vec<&str> = out.lines().filter(|l| l.starts_with("FAIL")).collect();
    assert!(
        run.status.success() && failed.is_empty(),
        "C ABI checks failed (exit {:?}):\n{out}",
        run.status.code()
    );
    // Every declared check ran (a crash midway would print fewer).
    let expected = count_checks(&src.join("main.jam"), "check(t, \"")
        + count_checks(&src.join("cfuncs.c"), "CHECK(\"");
    let ran = out.lines().filter(|l| l.starts_with("ok")).count();
    assert_eq!(ran, expected, "{out}");
}
