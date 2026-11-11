---
title: Modules, Testing & FFI
permalink: /reference/modules/
---

# Modules, Testing & FFI

Splitting a program across files, writing tests, and calling C through `extern`.
{: .lead}

## Modules & Imports {#imports}

`import("name")` returns a *module value*, a compile-time record of the symbols another
file exports. Bind it through `const` either whole or destructured.

```jam
// Whole-module binding.
const std = import("std");

fn show() {
    std.fmt.println("Hello!");
}

// Destructured binding, pulls specific names into the current scope.
// Re-exports under `std` can be reached via chained access on the
// import expression: pick the names you need without binding the
// whole module.
const { Vec }    = import("std").collections;
const { Option } = import("std").option;
const { assert } = import("test");
```

The path is resolved relative to the file (`./collections.jam` or `std/string.jam`) or
to a built-in name (`std`, `test`). Modules can re-export by binding to a local `pub`
name.

---

## Testing {#testing}

`tfn` declares a test. `jam test <file-or-dir>` compiles every `tfn`,
synthesizes a `main` that runs them, and reports each by name.
`import("test")` exports `assert(actual, expected)`.

```jam
const { assert } = import("test");

fn double(x: u64) u64 { return x * 2; }

tfn doubling() {
    assert(double(21) as i32, 42);
}
```

---

## extern / FFI {#extern}

`extern` declarations link to symbols defined in C (or any system providing the C ABI).
The compiler does not generate a body; it just emits a call. Most stdlib allocators use
this.

```jam
pub extern fn malloc(size: u64)  *mut[] u8;
pub extern fn free(ptr: *mut[] u8);
pub extern fn realloc(ptr: *mut[] u8, size: u64) *mut[] u8;

fn allocOne() *mut[] u32 {
    var raw: *mut[] u8 = malloc(@sizeOf(u32));
    return raw as *mut[] u32;
}
```

Param types must match the C ABI. Slice arguments are passed as a `{ptr, len}` pair, and
struct arguments larger than two words use sret-style return ABI, these match the
platform's C compiler.

### Calling raw function addresses: `@callC`

When the callee is only known as an address (`dlsym` results,
`objc_msgSend`-style dispatchers), `@callC(R, addr, args...)` performs an
indirect call with the C calling convention. `R` is the return type (`void`
is accepted here), `addr` is a `u64`, and each argument's *static* type
becomes the corresponding parameter type of the synthesized signature, with no
coercion happens at the boundary, so spell scalar types explicitly
(`0 as i64`, `x as u64`).

```jam
pub extern fn fabs(x: f64) f64;

fn callThroughAddress() f64 {
    const addr: u64 = fabs as u64;
    return @callC(f64, addr, -2.5);        // fn(f64) f64, by address
}
```

By-value aggregates are allowed when they are homogeneous float aggregates
of at most 4 same-width floats (`{f64,f64,f64,f64}`, FP registers) or at
most two full 64-bit words (`{u64,u64}`, GP registers); other shapes are
rejected at compile time.

### Variadic `cfn`: forwarding argument packs

A `cfn` method may declare one trailing argument pack, `args: ...`. The
method is instantiated per call-site shape (each pack argument's static
type becomes a real parameter), and the pack's only use is forwarding:
`args...` at the end of a call-argument list, most usefully into `@callC`
or an extern C-variadic like `printf`:

```jam
const Caller = struct {
    addr: u64,
    cfn callThrough(self: Caller, R: type, args: ...) R {
        return @callC(R, self.addr, args...);
    }
};

const n: i32 = c.callThrough(i32, -7 as i32);   // fn(i32) i32
const d: f64 = c.callThrough(f64, 1.5, 2.5);    // fn(f64, f64) f64
```

The pack cannot be read, indexed, or stored, only spread. Packs are not
available on plain `fn` or on top-level (comptime) `cfn`s.
