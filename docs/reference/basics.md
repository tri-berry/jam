---
title: Basics
permalink: /reference/basics/
---

# Basics

Bindings and the built-in types: integers, booleans, strings, pointers, slices and arrays.
{: .lead}

## Variables & Constants {#variables}

Jam has two binding forms inside functions: `var` for mutable storage and `const` for
single-assignment values. Every binding must be initialized at its declaration, Jam has
no `undefined` placeholder.

```jam
fn example() {
    var counter: i32 = 0;
    counter = counter + 1;

    const limit: i32 = 100;
    // limit = 101;  // error: cannot reassign const binding
}
```

At module scope, `const` declarations bind a name to a compile-time value. They are
*inlined* at every use site, referring to one costs the same as a literal. The type may
be inferred when an initializer alone determines it.

```jam
const FLAG_Z: u8 = 0x80;
const PAGE:   u32 = 4096;
const SHIFT       = 4;            // type inferred

fn isZSet(f: u8) bool {
    return (f & FLAG_Z) != 0;     // FLAG_Z inlined here
}
```

Module-scope `const` is also how top-level types, imports, and function pointers are
named, Jam funnels every top-level declaration through the same form.

---

## Integer Types {#types}

An integer is a number without a fractional component. Jam provides both signed (`i`) and
unsigned (`u`) integers of various sizes.

| Length | Signed  | Unsigned | Range (Signed)                                            | Range (Unsigned)              |
|--------|---------|----------|-----------------------------------------------------------|-------------------------------|
| 8-bit  | `i8`    | `u8`     | -128 to 127                                               | 0 to 255                      |
| 16-bit | `i16`   | `u16`    | -32,768 to 32,767                                         | 0 to 65,535                   |
| 32-bit | `i32`   | `u32`    | -2,147,483,648 to 2,147,483,647                           | 0 to 4,294,967,295            |
| 64-bit | `i64`   | `u64`    | -9,223,372,036,854,775,808 to 9,223,372,036,854,775,807   | 0 to 18,446,744,073,709,551,615 |

```jam
fn main() u8 {
    const smallNum: u8  = 255;
    const bigNum:   u64 = 18446744073709551615;

    const temperature: i32 = -40;
    const altitude:    i64 = -11034;

    var counter: i8 = -128;
    return 0;
}
```

Numeric literals can use underscores for readability (`0x1234_5678`, `1_000_000`) and
the standard `0x`, `0o`, `0b` prefixes.

<div class="callout">
    <h4>Choosing Integer Types</h4>
    <p>
        Use unsigned types (<code>u8</code>, <code>u16</code>, etc.) for values that are never negative, indices, counts, byte sizes.
        Use signed types (<code>i8</code>, <code>i16</code>, etc.) when you need to represent negative values.
    </p>
</div>

---

## Boolean Type {#bool}

The `bool` type represents a value that can be either `true` or `false`. Booleans are one
byte in size.

```jam
const isSweet: bool = true;
const isSour:  bool = false;

if (isSweet) {
    std.fmt.println("Delicious!");
}
```

### Logical Operators

Jam provides three logical operators for working with boolean values:

| Operator | Name | Description                                                            |
|----------|------|------------------------------------------------------------------------|
| `!`      | NOT  | Negates a boolean value                                                |
| `&&`     | AND  | Returns true if both operands are true (short-circuits if first is false) |
| `\|\|`     | OR   | Returns true if either operand is true (short-circuits if first is true)  |

The `&&` and `\|\|` operators use **short-circuit evaluation**: if the result can
be determined from the first operand alone, the second operand is not
evaluated; a call on the right-hand side does not run.

```jam
fn checkAccess(isAdmin: bool, isOwner: bool) bool {
    return isAdmin || isOwner;
}

fn validate(hasEmail: bool, hasPassword: bool) bool {
    return hasEmail && hasPassword;
}
```

---

## Strings {#strings}

Strings in Jam are represented as slices of bytes (`[]u8`). The `str` type is an alias for
`[]u8`. Literal bytes live in read-only data: a slice bound to a literal can be read and
rebound but not written through, and the compiler rejects such writes no matter how many
bindings or calls the literal passes through. To edit the bytes, copy them into owned
storage first (`buf.appendSlice("hi")`, or an array).

```jam
const greeting: str   = "Hello, World!";
const message:  []u8  = "Same as str";

// Strings are UTF-8.
const chinese: str = "世界";
const emoji:   str = "🌍";
```

### Memory Layout

A slice is a two-word value: a pointer to the data and a length.

| Field | Type          | Description                                                       |
|-------|---------------|-------------------------------------------------------------------|
| `ptr` | `*const[] u8` | Many-item pointer to the first byte                                |
| `len` | `u64`         | Number of bytes (excludes any trailing null in literals)           |

### Slicing & Comparison

`base[start..end]` produces a sub-slice. The base may be a slice, a fixed array, or a
many-item pointer; bounds are the caller's responsibility, like indexing. `==` and `!=`
compare two slices by length and contents (float element types are rejected; NaN makes
byte equality a lie; compare those with a loop).

```jam
const s: str = "hello world";
const hello: []u8 = s[0..5];
const world: []u8 = s[6..11];

if (hello == "hello") { /* taken */ }
if (hello != world)   { /* taken */ }

var arr: [6]u16 = [1, 2, 3, 4, 5, 6];
const mid: []u16 = arr[2..5];          // [3, 4, 5]
```

`std.string` layers call-style helpers on top: `eq`, `startsWith`, `endsWith`, `find`,
`contains`, `slice`, `trim`/`trimLeft`/`trimRight`, `concat`, `fromInt`, `parseInt`.

### Escape Sequences

| Escape       | Description                                       |
|--------------|---------------------------------------------------|
| `\n`         | Newline                                           |
| `\r`         | Carriage return                                   |
| `\t`         | Tab                                               |
| `\\`         | Backslash                                         |
| `\"`         | Double quote                                      |
| `\'`         | Single quote                                      |
| `\0`         | Null byte                                         |
| `\xHH`       | Hex byte (2 hex digits)                           |
| `\u{HHHHHH}` | Unicode codepoint (1-6 hex digits, encoded as UTF-8) |

```jam
const newline: str = "Line1\nLine2";
const quote:   str = "She said \"Hello\"";
const hello:   str = "\x48\x65\x6C\x6C\x6F";   // "Hello"
const earth:   str = "\u{1F30D}";              // 🌍
```

---

## Pointers & Slices {#pointers}

Jam distinguishes three reference families. Pointer types take a *required* `const` or
`mut` qualifier marking whether the pointee may be written. Slices and fixed arrays carry
no qualifier, their element mutability follows the binding.

| Type          | Description                                                                      |
|---------------|----------------------------------------------------------------------------------|
| `*const T`    | Single-item pointer, read-only pointee                                           |
| `*mut T`      | Single-item pointer, writable pointee                                            |
| `*const[] T`  | Many-item pointer (indexable), read-only pointee                                 |
| `*mut[] T`    | Many-item pointer (indexable), writable pointee                                  |
| `[]T`         | Slice (pointer + length); element mutability follows the binding                 |
| `[N]T`        | Fixed-size array of N elements; element mutability follows the binding           |

The `&` prefix takes the address of a binding or array element; `p.*` dereferences a
pointer. Indexing through a many-item pointer is `p[i]`.

```jam
fn pointerExample() u8 {
    var x: u8 = 42;
    var p: *mut u8 = &x;
    p.* = 100;
    return x;                       // 100
}

fn indexThroughMany() u8 {
    var arr: [4]u8 = [10, 20, 30, 40];
    var p: *mut[] u8 = &arr[0];
    p[2] = 99;
    return arr[2];                  // 99
}
```

<div class="callout">
    <h4>No bare <code>*T</code></h4>
    <p>
        The mutability qualifier is part of the pointer type, there is no shorthand
        <code>*T</code>. Writing <code>*const u8</code>
        vs <code>*mut u8</code> documents intent at every signature.
    </p>
</div>

---

## Arrays {#arrays}

Fixed-size arrays have a length known at compile time. Array literals come in three forms:
comma-separated, fill-with-count, and empty (which produces a zero-length slice).

```jam
var a: [4]u8  = [10, 20, 30, 40];     // [a, b, c, d]
var b: [16]u8 = [0; 16];              // [expr; N], fill 16 slots with 0
const empty: []u8 = [];               // empty slice

// Index with []. Out-of-bounds is undefined at runtime;
// bounds checks are not yet inserted.
var x: u8 = a[2];                     // 30
a[0] = 99;
```

A fixed array implicitly coerces to a slice when bound to a `[]T` location, producing a
two-word `{ptr, len}` view over the same storage.
