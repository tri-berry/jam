---
title: About
permalink: /
---

# About Jam

Jam is a statically typed systems language with mutable value semantics, generics
over types, tagged unions, and an LLVM backend. It aims for the memory safety of Rust
without lifetime annotations, and for code that reads plainly at the call site.
{: .lead}

```jam
const std = import("std");

const Shape = enum { Circle(f64), Rect(f64, f64) };

fn area(s: Shape) f64 {
    match (s) {
        Shape.Circle(r) { return 3.14159 * r * r; }
        Shape.Rect(w, h) { return w * h; }
    }
    return 0.0;
}

// `mut` borrows the caller's value exclusively, for the call only.
fn scale(s: mut Shape, k: f64) {
    match (s) {
        Shape.Circle(r) { s = Shape.Circle(r * k); }
        Shape.Rect(w, h) { s = Shape.Rect(w * k, h * k); }
    }
}

fn main() {
    var s = Shape.Rect(2.0, 3.0);
    scale(s, 2.0);
    const a = area(s) as i64;
    std.fmt.println("area = {a}");
}
```

## Values, not references

Every binding owns its value. Passing a value to a function never silently shares
storage with the caller: a parameter is a read-only borrow by default, `mut` makes it
an exclusive read-write borrow, and `move` hands ownership over. The mode lives in the
signature, so call sites carry no sigils.

The compiler enforces definite initialization, exclusivity and linear drops. That
rules out use-after-free, double-free, data races and reads of uninitialized memory,
with no lifetime annotations to write. See
[Mutable Value Semantics]({{ '/reference/#mvs-overview' | relative_url }}).

## What's in the language

- **Structs, enums and unions.** Enums carry payloads and are taken apart with
  [`match`]({{ '/reference/#match' | relative_url }}).
- **Generics as functions.** A generic type is a function from types to a type,
  evaluated at compile time. See [Generics]({{ '/reference/#generics' | relative_url }}).
- **Compile-time execution.** Code can run in the compiler, at compile time. See
  [Compile-Time Execution]({{ '/reference/#comptime' | relative_url }}).
- **Tests built in.** `tfn` declares a test and `jam test` runs every one of them.
  See [Testing]({{ '/reference/#testing' | relative_url }}).
- **C interop.** `extern fn` calls C and `export fn` is callable from C, with structs
  passed by value under the platform's C ABI. See
  [extern / FFI]({{ '/reference/#extern' | relative_url }}).

## The compiler

The compiler is written in Rust and generates code through LLVM. It builds native
executables, runs a program in one step with `jam run`, and can print the LLVM IR it
generates. Jam is distributed under the Apache License 2.0 with LLVM Exceptions.

## Status

Jam has no tagged release yet. Syntax and the standard library will keep changing until
v0.1, and issues and pull requests on the repository stay closed until then. You can
still build the compiler from `main` today.

Next, see [Installation]({{ '/installation/' | relative_url }}), then read the
[Language Reference]({{ '/reference/' | relative_url }}).
