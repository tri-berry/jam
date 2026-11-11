---
title: Control Flow & Functions
permalink: /reference/control-flow-functions/
---

# Control Flow & Functions

Branches and loops, function declarations, function pointers, and explicit casts between types.
{: .lead}

## Control Flow {#control-flow}

Jam has `if`/`else`, `while`, `for`, and `return`. Conditions require parentheses.

```jam
fn classify(n: i32) i32 {
    if (n < 0) {
        return -1;
    } else if (n == 0) {
        return 0;
    } else {
        return 1;
    }
}

fn sumTo(n: u32) u32 {
    var total: u32 = 0;
    var i: u32 = 0;
    while (i < n) {
        total = total + i;
        i = i + 1;
    }
    return total;
}

fn fillIndices() [16]u8 {
    var arr: [16]u8 = [0; 16];
    for i in 0:16 {
        arr[i] = i as u8;
    }
    return arr;
}
```

The `for` loop iterates a half-open integer range, `for i in 0:N` binds `i = 0, 1, …, N-1`.
With a literal end bound the index is a `u64`; with a variable bound it takes that
variable's type.

Without a `:`, `for` walks a slice or array element-wise. The loop var binds each
element *in place*, like `s[i]` as an lvalue, so assigning through it writes to the
underlying storage.

```jam
fn sumBytes(s: []u8) u32 {
    var total: u32 = 0;
    for b in s {
        total = total + (b as u32);
    }
    return total;
}

fn firstFourSquares() [4]u16 {
    var arr: [4]u16 = [1, 2, 3, 4];
    for v in arr {
        v = v * v;
    }
    return arr;                    // [1, 4, 9, 16]
}
```

`loop { … }` runs forever (sugar for `while (true)`); `break` exits the innermost
loop and `continue` jumps to its next iteration.

```jam
fn firstPowerAbove(n: u64) u64 {
    var p: u64 = 1;
    loop {
        p = p * 2;
        if (p > n) { break; }
    }
    return p;
}
```

---

## Functions {#fn-declaration}

Functions are declared with `fn`. Parameters are strictly typed; the return type goes
directly after the parameter list. Jam code uses *camelCase* by convention for functions
and variables.

```jam
// Two i32 inputs, returns i32.
fn mixIngredients(sugar: i32, fruit: i32) i32 {
    return sugar + fruit;
}

// No return value, omit the return type.
fn greet() {
    std.fmt.println("Hello!");
}
```

Functions can be prefixed with `pub` (visible outside the defining module) and `extern`
(declared but defined elsewhere, typically libc).

```jam
pub fn add(a: i32, b: i32) i32 { return a + b; }

pub extern fn malloc(size: u64) *mut[] u8;
pub extern fn free(ptr: *mut[] u8);
```

<div class="callout">
    <h4>Void Functions</h4>
    <p>
        A function with no declared return type returns nothing. Don't write
        <code>void</code>, just omit the return-type slot.
    </p>
</div>

---

## Function Pointers {#fn-pointers}

`fn(params) Ret` is a first-class type: store one in a local or a struct field
and call it through the binding. `someFn as u64` takes a function's raw
address (for FFI hand-off and `@callC`).

```jam
fn add(a: i32, b: i32) i32 { return a + b; }
fn mul(a: i32, b: i32) i32 { return a * b; }

const Op = struct { name: str, apply: fn(i32, i32) i32 };

fn run() i32 {
    var f: fn(i32, i32) i32 = add;
    const r: i32 = f(2, 3);          // 5
    const op: Op = Op { name: "mul", apply: mul };
    return r + op.apply(2, 3);       // 5 + 6
}
```

There are no closures: a callback that needs state takes an explicit context
argument (see `std/box` and jam-objc's block support for the pattern).

---

## Type Casts {#casts}

The `as` operator performs explicit conversions. Integer ↔ integer casts truncate or
extend; integer → float converts numerically; an enum tag can be extracted to its
underlying integer type.

```jam
const big: u32 = 0x1234_5678;
const low: u8  = big as u8;            // truncate → 0x78

const small: u8  = 200;
const wide:  u32 = small as u32;       // zero-extend → 200

const negative: i32 = 250;
const narrow:   i8  = negative as i8;  // wrap → -6
```

Casts are explicit at every narrowing or sign-changing step. There are no implicit
numeric conversions.
