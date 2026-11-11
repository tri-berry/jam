---
title: Installation
permalink: /installation/
---

# Installation

Jam is a statically typed systems language with mutable value semantics and an LLVM
backend. There is no tagged release yet, but you can build the compiler from `main`
today.
{: .lead}

Expect things to move. Syntax and the standard library will keep changing until v0.1,
and issues and pull requests on the repository stay closed until then.

## macOS <span class="badge alpha">HEAD only</span>

<div class="card">
    <img src="{{ '/assets/images/logo.png' | relative_url }}" alt="">
    <div class="card-text">
        <div class="card-title">Homebrew formula</div>
        <div class="card-sub">Builds the compiler from <code>main</code></div>
    </div>
    <a class="button" href="https://github.com/raphamorim/jam">GitHub</a>
</div>

The formula builds the latest commit on `main` and pulls in LLVM 22 and Rust for you.

```bash
brew tap raphamorim/jam https://github.com/raphamorim/jam
brew install --HEAD jam
```

## From source <span class="badge">Rust + LLVM 22</span>

The compiler is written in Rust. You need the toolchain from `rust-toolchain.toml`
(picked up automatically by `rustup`) and LLVM 22, either `brew install llvm` or
`LLVM_CONFIG` pointing at your `llvm-config`.

```bash
git clone https://github.com/raphamorim/jam
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

Next, read the [Language Reference]({{ '/reference/' | relative_url }}).
