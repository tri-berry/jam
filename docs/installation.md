---
title: Installation
permalink: /installation/
---

# Installation

Jam is a statically typed systems language with mutable value semantics and an LLVM
backend. The current release is v{{ site.version }}, with prebuilt compilers for macOS
and Linux.
{: .lead}

Expect things to move. Syntax and the standard library will keep changing until v0.1,
and issues and pull requests on the repository stay closed until then.

## Download <span class="badge alpha">v{{ site.version }}</span>
{: #download}

<div class="card">
    <img src="{{ '/assets/images/logo.png' | relative_url }}" alt="">
    <div class="card-text">
        <div class="card-title">Jam v{{ site.version }}</div>
        <div class="card-sub">Prebuilt compiler, standard library included</div>
    </div>
    <a class="button" href="https://github.com/tri-berry/jam/releases/tag/v{{ site.version }}">Download</a>
</div>

Each archive is self-contained: the compiler, the standard library and everything it
needs to link programs. Nothing else has to be installed, not even a C compiler.

| Platform | Archive |
|---|---|
| macOS, Apple Silicon | `jam-v{{ site.version }}-aarch64-apple-darwin.tar.gz` |
| macOS, Intel | `jam-v{{ site.version }}-x86_64-apple-darwin.tar.gz` |
| Linux, x86_64 | `jam-v{{ site.version }}-x86_64-unknown-linux-gnu.tar.gz` |
| Linux, ARM64 | `jam-v{{ site.version }}-aarch64-unknown-linux-gnu.tar.gz` |

Download the archive for your platform, unpack it anywhere, and put its `bin` directory
on your `PATH`:

```bash
curl -LO https://github.com/tri-berry/jam/releases/download/v{{ site.version }}/jam-v{{ site.version }}-aarch64-apple-darwin.tar.gz
tar xzf jam-v{{ site.version }}-aarch64-apple-darwin.tar.gz
export PATH="$PWD/jam-v{{ site.version }}-aarch64-apple-darwin/bin:$PATH"
jam version
```

The macOS compilers need macOS 15 or newer, and the Linux ones glibc 2.35 or newer
(Ubuntu 22.04, Debian 12). The release page also has a `SHA256SUMS` file to check a
download against.

## Homebrew <span class="badge">macOS</span>
{: #homebrew}

The formula builds the release from source and pulls in LLVM 22, LLD 22 and Rust for
you. Add `--HEAD` to build the latest commit on `main` instead.

```bash
brew tap tri-berry/jam https://github.com/tri-berry/jam
brew install jam
```

## From source <span class="badge">Rust + LLVM 22</span>

The compiler is written in Rust. You need the toolchain from `rust-toolchain.toml`
(picked up automatically by `rustup`) plus LLVM 22 and LLD 22, either
`brew install llvm@22 lld@22` or `LLVM_CONFIG` pointing at your `llvm-config` and
`JAM_LLD_PREFIX` at your LLD install.

```bash
git clone https://github.com/tri-berry/jam
cd jam
make build     # debug build
make test      # Rust unit tests + the .jam corpus under tests/
make install   # release build, installs jam + std to ~/.local
```

## Hello, world

```jam
const std = import("std");

fn main() {
    std.fmt.println("Hello, World!");
}
```

```bash
jam run hello.jam
```

The installed `jam` finds the standard library relative to its own binary
(`<prefix>/lib/jam/std`). Set `JAM_STD_PATH` or pass `--std-path` to point it
somewhere else.

## Linking

Jam links programs itself, with the LLD linker built into the compiler and the C
library files it ships in `<prefix>/lib/jam/libc`. You do not need clang, gcc or the
Xcode command-line tools to build a Jam program.

- **macOS**: programs link against a bundled stub of the system library.
- **Linux**: programs link statically against a bundled musl, so they run on any
  distribution.

Both sets of files ship with every install, so cross-compiling needs nothing extra:
`jam -C target=x86_64-unknown-linux-musl hello.jam` on a Mac produces a Linux
executable.

Link other libraries with `-l<name>`, and add search directories with `-L<dir>`:

- **macOS**: `-l` also searches the macOS SDK when the Xcode command-line tools are
  installed, so `jam run -lncurses tetris.jam` works. Without an SDK only the `-L`
  directories and `/usr/local/lib` are searched.
- **Linux**: because programs are static, `-l` can only use static libraries
  (`lib<name>.a`) from the `-L` directories.

On macOS a program runs on the OS version it was built on and newer. Set
`MACOSX_DEPLOYMENT_TARGET` (for example `12.0`) to target an older release.
`JAM_LIBC_PATH` points Jam at a different copy of the bundled files.

Next, read the [Language Reference]({{ '/reference/' | relative_url }}).
