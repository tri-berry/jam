---
title: Mutable Value Semantics
permalink: /reference/value-semantics/
---

# Mutable Value Semantics {#mvs-overview}

Jam is a *mutable value semantics* language. Every binding owns its value; passing a
value to a function does not silently share storage with the caller. Functions opt into
a specific borrowing or transfer behavior via a per-parameter **mode** keyword.

The result is the same memory safety as Rust, no use-after-free, no double-free, no
data races, no reads of uninitialized memory, with no lifetime annotations.

The design is inspired by Hylo's parameter-mode system and Rust's drop semantics. The
compiler enforces three rules: *definite initialization* (every binding is written
before it is read), *exclusivity* (no overlapping mutable borrows), and *linear drops*
(every owned value gets exactly one destructor call).

---

## Parameter Modes {#mvs-modes}

Every function parameter is declared in one of three modes. The default mode is
read-only; the other two are introduced by an explicit keyword between the colon and
the type.

| Mode                  | Keyword     | Pointee Mutability | Caller After Call        |
|-----------------------|-------------|--------------------|--------------------------|
| Read-only borrow      | `(default)` | read-only          | unchanged                |
| Exclusive read-write  | `mut`       | read-write         | unchanged                |
| Consume ownership     | `move`      | read-write         | becomes uninitialized    |

```jam
// Default read-only borrow, caller's value is unchanged.
fn distance(a: Point, b: Point) f64 {
    return sqrt((a.x - b.x) * (a.x - b.x) + (a.y - b.y) * (a.y - b.y));
}

// Exclusive read-write, declared at the signature only.
fn scale(p: mut Point, factor: f64) {
    p.x = p.x * factor;
    p.y = p.y * factor;
}

// Consume, caller's binding is uninitialized after the call.
fn storeIn(buf: move []u8, db: mut Database) {
    db.append(buf);
}
```

Call sites are sigil-free for every mode: the signature's mode declaration does all
the work, and the argument is always a plain expression:

```jam
var p: Point = Point { x: 3.0, y: 4.0 };
scale(p, 2.0);                   // mut access, no sigil
distance(p, otherPoint);         // read-only, no sigil
```

The default mode is enforced: writing into a default-mode parameter (the binding
itself, a field, an array or slice element, or a `for`-each element view over it)
is a compile error. Writes through a pointer-typed *value* (`p.*`, `self.ptr[i]`)
are governed by the pointer's own `mut`/`const` qualifier, not the parameter's mode.

```jam
fn broken(arr: [4]u16) {
    arr[0] = 99;    // error: parameters are read-only borrows by default
}
```

The borrow follows the value through calls: a returned slice, whether bare, carried
in a struct's fields (`Pair { data: v.asSlice(), n: 0 }`), or in an enum payload
(`Option([]u8).Some(v.asSlice())`), is tracked back to the arguments it derives
from, so views of a read-only parameter reject writes no matter how many bindings,
calls, or `match` arms they pass through, including locals assembled inside the
callee and then returned (`var p = ...; p.data = v.asSlice(); return p;`). Scalar
payloads and fields are plain copies and stay freely mutable. View state is per
field: `p.data = owned;` releases just that field for writing, a later
`p.data = v.asSlice();` re-views it, and a whole rebind of `p` recomputes
everything. A view-carrying struct still owns its
inline fields: `p.n = 5` and whole rebinds stay legal; only writes reaching the
borrowed elements are rejected. Passing read-only storage as a `mut`
argument or calling a `mut self` method on it, directly (`v.push(1)`) or through a
field chain (`h.buf.push(1)`), is rejected the same way; the fix is always to take
the parameter as `mut` in the signature.

There is no reference type in jam, so `&` is not a borrow operator: it is address-of,
producing a `*mut T` / `*const T` pointer value, and is only meaningful where the
parameter's *type* is a pointer (FFI, sink out-params). Writing `&x` for a mode
parameter is a compile error.

After a `move` call the binding is dead in the caller; reading it is a compile error.

---

## Exclusivity {#mvs-exclusivity}

At any call boundary, at most one argument may be a `mut` borrow of a given binding,
and a `mut` borrow cannot coexist with any other borrow of the same binding. The
compiler rejects programs that violate this rule, the same "law of exclusivity"
Rust's borrow checker enforces, applied locally at each call rather than across
whole-program lifetimes.

```jam
fn modify(x: mut u32, y: u32) u32 {
    x = x + y;
    return x;
}

fn caller() u32 {
    var n: u32 = 5;
    return modify(n, n);   // error: conflicting borrows of `n`
}
```

---

## Drop {#mvs-drop}

A struct may define a `drop` method that runs automatically when an owned instance goes
out of scope. The drop method takes `self: mut Self` and runs exactly once per owned
value, even on early returns, in match arms, and through nested control flow. There is
no manual `defer` ceremony.

```jam
const File = struct {
    fd: i32,

    fn drop(self: mut File) {
        close(self.fd);
    }
};

fn readFile(path: []u8) i32 {
    var f: File = openFile(path);
    return f.fd;
    // f.drop() runs here automatically
}
```

A value *moved* into another function (via the `move` mode) becomes uninitialized in
the caller, so the drop fires at the new owner's scope exit, never twice.

A `move` parameter is owned by the callee: it drops when the function exits, unless
the body moves it onward: into a container slot, a struct-literal field, another
`move` call, or a `return`. Binding a bare drop-bearing value to a new name
(`var owned = c;`) or storing it (`arr[i] = c;`) is likewise a move, never a copy.
Assigning over a live drop-bearing value (`c = newCounter();`, `h.field = x;`,
`v[i] = x;`) drops the previous occupant before the store, so overwriting never
leaks.
Moving a drop-bearing value out of a `let`/`mut` parameter is rejected; those are
borrowed, not owned; declare the parameter `move` to take ownership.

Matching a drop-bearing enum by value consumes the scrutinee: arm bindings take
ownership of the payload (they drop at the arm's end unless moved onward), arms
that don't bind drop the residual payload on entry, and a temporary scrutinee
(`match (v.pop())`) is owned by the match itself. Matching a `let`/`mut`
parameter's enum is rejected (borrowed values can't be consumed); clone the
scrutinee or take it by `move`.

When both values are genuinely needed, clone explicitly: `x.clone()` is a deep
copy. Plain data clones for free (it is the value), structs without their own
`drop` clone field-wise, arrays clone element-wise, and a type that owns
resources (has `cfn drop`) must define `cfn clone(self: Self) Self` to say how
its resource duplicates: the compiler can clone structure, never resources.
Container clones are conditional: `Vec(T).clone()` exists exactly when `T` is
cloneable. There are no implicit clones.

Drops stay static: jam never inserts runtime drop flags. Two restrictions on
drop-bearing bindings keep that possible: a move must be unconditional relative to the
binding's declaration (moving inside an `if` arm, loop body, or `match` arm that does
not also contain the declaration is rejected with "move it on all control-flow paths
or none"), and a moved binding cannot be re-assigned; bind a new name instead. Where
Rust would insert a runtime drop flag for a conditionally-moved value, jam rejects the
program, the same static resolution Swift's noncopyable types use.
