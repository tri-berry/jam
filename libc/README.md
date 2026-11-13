# Bundled C library files

Jam links programs with the LLD it embeds, against the files in this directory
(installed to `<prefix>/lib/jam/libc`). They are what lets a Jam install build
programs without a system compiler or SDK.

## `darwin/libSystem.tbd`

A text stub listing the symbols of macOS's system library (`libSystem` and the
libraries it re-exports), for x86_64 and arm64. Programs load the real library
from the OS at run time; the stub only tells the linker what exists.

Taken unmodified from the Zig project (`lib/libc/darwin/libSystem.tbd`), which
generates it from Apple's macOS SDK (version 26.1).

## `musl/<arch>-linux-musl/`

What a static Linux executable is linked from, for `x86_64` and `aarch64`:

- `crt1.o`, `crti.o`, `crtn.o`, `libc.a`: musl 1.2.6's startup objects and
  static C library. MIT licensed; see `musl/COPYRIGHT`.
- `libcompiler_rt.a`: the compiler-rt builtins from LLVM 22.1.3 (128-bit
  integer and float helpers). Apache-2.0 with LLVM exceptions, the same
  licence as Jam.

They are prebuilt binaries, since Jam has no C compiler to build them with.
They come unmodified, apart from `llvm-strip --strip-debug`, from Alpine Linux
3.24 packages (`https://dl-cdn.alpinelinux.org/alpine/v3.24/main/<arch>/`):

| Package | Architecture | SHA-256 of the `.apk` |
|---|---|---|
| `musl-dev-1.2.6-r3` | x86_64 | `310b51cff12e1d5ce5f135c558a82e2550427da5465bbd16c9ff9b05e63ee3d9` |
| `musl-dev-1.2.6-r3` | aarch64 | `45afe0a9f78de3402b6b5d11f5153ed955bf59d59b45344d9c3fed9f4bbf2f66` |
| `compiler-rt-22.1.3-r0` | x86_64 | `1775eb9ba0aa9d465820ee7eb6918390973389f1f32b15378a9cc9c554ce5ee2` |
| `compiler-rt-22.1.3-r0` | aarch64 | `4339f4d0ea4e3c0825ce164e9b26289af894c852005f1f08dcd17709d1d9def5` |

To update them, extract `usr/lib/{crt1.o,crti.o,crtn.o,libc.a}` from `musl-dev`
and `libclang_rt.builtins-<arch>.a` from `compiler-rt`, strip debug info, and
replace the files here.
