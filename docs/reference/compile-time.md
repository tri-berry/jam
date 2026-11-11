---
title: Compile Time
permalink: /reference/compile-time/
---

# Compile Time

Code that runs inside the compiler, and the intrinsics available to it.
{: .lead}

## Compile-Time Execution {#comptime}

`comp` marks bindings and parameters the compiler evaluates during
compilation. A `comp const`/`comp var` folds to a constant; `comp if` selects
a branch at compile time and the dead arm is never lowered; a `comp`
parameter specializes the function per call-site value (each distinct value
gets its own clone).

```jam
comp const WORDS: u32 = 4;

fn scale(comp k: u32, x: u32) u32 {   // one clone per distinct k
    return x * k;
}

fn size() u32 {
    comp if (WORDS > 2) {
        return scale(8, WORDS);
    } else {
        return scale(2, WORDS);
    }
}
```

`cfn` is the second compile-time form. A top-level `cfn` runs its body at
compile time and emits code into the caller (`std.fmt.print` is one; the
format string is parsed during compilation). A `cfn` **method** is an
ordinary runtime function that opts into compiler-synthesized call sites:
`cfn drop` registers with automatic drop, `cfn clone` with `.clone()`,
`cfn at`/`setAt`/`len` with `v[i]` indexing (on plain bindings and field
chains alike: `h.buf[i]` dispatches the same way), and a `cfn` method may
declare a variadic pack (see extern / FFI).

---

## Comptime Intrinsics {#intrinsics}

Intrinsics are compiler builtins prefixed with `@`. They run at compile time and their
results are substituted as constants before LLVM sees the code.

| Intrinsic                    | Description                                             |
|------------------------------|---------------------------------------------------------|
| `@sizeOf(T)`                 | Size of `T` in bytes (u64); `@sizeOf(void)` is 0        |
| `@alignOf(T)`                | Alignment of `T` in bytes (u64)                         |
| `@os()`                      | Target OS name as a `str` (`"macos"`, `"linux"`, ...)   |
| `@isDarwin()` `@isLinux()` `@isWindows()` `@isUnix()` | Target-OS predicates; fold to a `bool` literal and dead arms drop |
| `@isX86_64()` `@isAarch64()` | Target-arch predicates (honour `-C target`); same folding |
| `@dropInPlace(ptr)`          | Run the pointee type's drop glue at `ptr`               |
| `@callC(R, addr, args...)`   | Indirect C call through a raw `u64` address; the fixed C signature is synthesized from `R` and the args' static types (see extern / FFI) |

```jam
fn bytesFor(n: u64) u64 {
    return n * @sizeOf(u32);          // n * 4, fully constant-folded
}

fn pointerSize() u64 {
    return @sizeOf(*const u8);        // 8 on a 64-bit target
}

fn sliceSize() u64 {
    return @sizeOf([]u8);             // 16, {ptr, len}
}
```

Intrinsics compose freely with runtime arithmetic. They produce a `u64` and require an
explicit cast to narrow to other widths.
