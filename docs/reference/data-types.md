---
title: Data Types
permalink: /reference/data-types/
---

# Data Types

The types you define yourself: structs with methods, enums with payloads, unions, pattern matching with `match`, and generics.
{: .lead}

## Structs {#structs}

A struct groups fields under a single name. Top-level structs are declared via
`const Name = struct { … };`. Field names are `field: Type`; instances are built with
`TypeName { field: value, … }`: literals always name their type.

```jam
const Vec3   = struct { x: f32, y: f32, z: f32 };
const Pixel  = struct { r: u8, g: u8, b: u8 };
const Player = struct { hp: u32, level: u8, alive: bool };

fn main() {
    const v: Vec3 = Vec3 { x: 0.0, y: 100.0, z: 50.0 };
    var px:  Pixel = Pixel { r: 10, g: 20, b: 30 };
    px.r = 100;

    // Nested literals work the same way.
    const Outer = struct { inner: Pixel, c: u8 };
    const x: Outer = Outer {
        inner: Pixel { r: 1, g: 2, b: 3 },
        c: 4,
    };
}
```

### Methods

Functions declared *inside* a struct body are methods. The first parameter is
conventionally `self`; the parameter mode chooses borrow semantics.

```jam
const Counter = struct {
    value: u32,
    sink:  *mut u32,

    fn drop(self: mut Counter) {
        var p: *mut u32 = self.sink;
        p.* = p.* + 1;
    }
};

fn observe() u32 {
    var hits: u32 = 0;
    var c: Counter = Counter { value: 5, sink: &hits };
    return c.value;
    // c.drop() fires automatically here, see Drop, below.
}
```

A method can also be invoked by-name on the type: `Counter.drop(&c)` calls it
explicitly while `c` is still in scope (and the automatic drop will still fire at scope
exit, calling drop manually is currently a footgun).

---

## Enums {#enums}

Enums describe a closed set of named variants. The simplest form is *payload-less*, each
variant is a u8 discriminant.

```jam
const Color = enum { Red, Green, Blue };

fn classifyColor(c: Color) u8 {
    match (c) {
        Color.Red   { return 100; }
        Color.Green { return 200; }
        Color.Blue  { return 50; }
        _           { return 0; }
    }
    return 99;
}
```

Discriminant values are assigned in declaration order starting at 0. You can also pin them
explicitly:

```jam
const Phase = enum { Idle = 0, Running = 5, Stopping = 9 };

fn phaseAsByte(p: Phase) u8 {
    return p as u8;
}
```

### Variants with Payloads (Tagged Unions)

Variants can carry positional fields. Pattern matching destructures them by name.

```jam
const Op = enum {
    Nop,
    LdRR(u8, u8),
    Imm(u8),
    Wide(u16),
};

fn srcReg(op: Op) u8 {
    match (op) {
        Op.Nop          { return 0xFF; }
        Op.LdRR(d, s)   { return s; }
        Op.Imm(v)       { return v; }
        Op.Wide(w)      { return 0xEE; }
        _               { return 0xCC; }
    }
    return 0;
}
```

Payload-carrying enums are how `Option(T)` and `Result(T, E)` are built, see
[Generics](#generics).

---

## Unions {#unions}

A `union` is *untagged*, every field shares the same storage. Use it for type punning
(read float bits as `u32`, etc.) and for matching C-side `union { … }` types at FFI
boundaries.

```jam
const FloatBits = union {
    i: u32,
    f: f32,
};

fn floatBits(x: f32) u32 {
    var b: FloatBits = FloatBits { f: x };
    return b.i;
}
```

Unions size to their largest field; alignment is the max of all fields'. Unlike enums,
there is no discriminant, the compiler trusts the program to know which field is live.

---

## Pattern Matching {#match}

The `match` expression dispatches on a scrutinee. Patterns include integer literals,
enum variants (with or without payloads), and `_` as a wildcard.

```jam
fn dispatch(x: u8) u8 {
    match (x) {
        0           { return 10; }
        1 | 2 | 3   { return 20; }       // `|`-joined patterns
        4..=9       { return 30; }       // inclusive range
        _           { return 255; }
    }
    return 99;
}
```

Range bounds are full 64-bit and may be negative (`-100..=-1`); an inverted
range is a compile error. When the scrutinee's enum type is unambiguous the
variant may be written bare: `Some(x)` instead of `Option(i32).Some(x)`.

A `match` can also be used as an *expression*, each arm produces a value, and the result
is the value of the matched arm.

```jam
fn unwrap(o: Option(i32), fallback: i32) i32 {
    return match (o) {
        Option(i32).Some(x) { x }
        Option(i32).None    { fallback }
    };
}
```

Arms that bind payload fields introduce those names into the arm's scope.

A match **expression** must be exhaustive: cover every variant of the enum or
add a `_` arm; anything else is a compile error (a fallthrough would leave the
result undefined). Statement-position matches keep switch-style fallthrough.

---

## Generics {#generics}

A generic function takes one or more type parameters declared `T: type` and returns a
type. Calling it with concrete type arguments at compile time produces a concrete type;
each distinct argument list yields a fresh struct (or enum).

```jam
fn Box(T: type) type {
    return struct {
        value: T,
    };
}

fn Pair(A: type, B: type) type {
    return struct {
        first:  A,
        second: B,
    };
}

fn main() {
    var b: Box(i32)      = Box(i32) { value: 17 };
    var p: Pair(i32, u8) = Pair(i32, u8) { first: 7, second: 35 };
}
```

A type alias `const Name = Generic(arg);` registers `Name` as a synonym, subsequent
uses resolve to the same instantiated struct.

```jam
const BoxI32 = Box(i32);
var b: BoxI32 = BoxI32 { value: 17 };
```

Type arguments are not limited to names: structural types work too, in aliases and in
calls with `T: type` parameters.

```jam
const Bytes = Box([]u8);
const Ptrs  = Pair(*const u8, [4]u16);

fn sizeOfIt(T: type) u64 { return @sizeOf(T); }
const SLICE_SIZE = sizeOfIt([]u8);    // 16: {ptr, len}
```

### Generic Methods

Methods on generic types are cloned per instantiation. Each instantiation gets its own
LLVM symbol with substituted parameter and return types.

```jam
fn Holder(T: type) type {
    return struct {
        value: T,
        fn unwrap(self: move Self) T {
            return self.value;
        }
    };
}
```

`Self` inside a struct body refers to the enclosing struct type, for a generic, that's
the specific instantiation in play.

### Generic Enums

The same mechanism produces tagged unions parameterized by type. The standard library's
`Option(T)` is built this way:

```jam
pub fn Option(T: type) type {
    return enum {
        Some(T),
        None,
    };
}
```

Construct variants by qualifying with the instantiated type: `Option(i32).Some(42)`,
`Option(i32).None`.

`Result(T, E)` ships the same way (`import("std").result`). An instantiation can be
bound to a module-scope alias and used like any named type:

```jam
const { Result } = import("std").result;

const ParseResult = Result(u64, str);

fn parse(s: []u8) ParseResult {
    if (s.len == 0) {
        return ParseResult.Err("empty input");
    }
    return ParseResult.Ok(s.len);
}
```
