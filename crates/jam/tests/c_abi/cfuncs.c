/*
 * Copyright (c) 2026-present Raphael Amorim
 *
 * This file is part of jam.
 * Licensed under the Apache License, Version 2.0 with LLVM Exceptions.
 */

/* C side of the C-ABI suite (crates/jam/tests/c_abi.rs), after Zig's
 * test/c_abi/cfuncs.c: functions main.jam calls with structs by value,
 * and c_run_tests(), which calls main.jam's `export fn`s the way any C
 * caller would. Every shape class is covered: one and two GP words,
 * a 12-byte struct (tail padding), HFAs, a mixed INT/SSE pair (x86-64),
 * a memory-class struct, and register exhaustion, where a 16-byte struct
 * no longer fits in the remaining registers and must go wholly to the
 * stack. Fixed-width types keep the C side identical on LP64 and LLP64
 * (Win64 `long` is 32-bit). */

#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>

typedef struct { int a, b; } P;           /* 8B   one GP word */
typedef struct { int a, b, c; } T12;      /* 12B  two words, tail padding */
typedef struct { int64_t a, b; } L2;         /* 16B  two GP words */
typedef struct { double x, y; } D2;       /* HFA (aarch64) / SSE pair (x86-64) */
typedef struct { float x, y, z; } F3;     /* HFA of 3 floats */
typedef struct { int64_t a; double d; } Mix; /* INT + SSE on x86-64 */
typedef struct { int64_t a, b, c, d; } Big;  /* 32B  memory class */
typedef struct { bool on; int n; } Flag;  /* bool member */
typedef union { float a; float b; } UF;   /* union of floats: an HFA */
typedef struct { unsigned char tag; int v; } E8; /* = Jam enum { Num(i32), Nothing } */
typedef struct {} Z;                      /* empty record: no argument at all */

/* ---- called from Jam ---- */

P c_mk_p(int a, int b) { P p = {a, b}; return p; }
int c_sum_p(P p) { return p.a * 10 + p.b; }
T12 c_mk_t12(int a) { T12 t = {a, a + 1, a + 2}; return t; }
int c_sum_t12(T12 t) { return t.a + t.b + t.c; }
L2 c_mk_l2(int64_t a) { L2 l = {a, a * 2}; return l; }
int64_t c_sum_l2(L2 l) { return l.a + l.b; }
D2 c_mk_d2(double x) { D2 d = {x, x / 2}; return d; }
double c_sum_d2(D2 d) { return d.x + d.y; }
F3 c_mk_f3(float x) { F3 f = {x, x + 1, x + 2}; return f; }
float c_sum_f3(F3 f) { return f.x + f.y + f.z; }
Mix c_mk_mix(int64_t a) { Mix m = {a, a + 0.5}; return m; }
double c_sum_mix(Mix m) { return m.a + m.d; }
Big c_mk_big(int64_t x) { Big g = {x, x + 1, x + 2, x + 3}; return g; }
int64_t c_sum_big(Big g) { return g.a + g.b + g.c + g.d; }
Flag c_mk_flag(bool on, int n) { Flag f = {on, n}; return f; }
int c_flag_val(Flag f) { return f.on ? f.n : -f.n; }

int64_t c_mixed(int a, Big g, P p, D2 d, int64_t z) {
    return a + g.a + g.d + p.a + p.b + (int64_t)d.x + z;
}

/* Pass-through params: Jam `mut` params and arrays are pointers in C. */
void c_bump(P *p) { p->a += 10; }
void c_bump_by(int *x, P p) { *x += p.b; }
int c_first_diff(const unsigned char *a, const unsigned char *b, int n) {
    for (int i = 0; i < n; i++) if (a[i] != b[i]) return i;
    return -1;
}

void c_bump_with(P *p, P by) { p->a += by.a; p->b += by.b; }

UF c_mk_uf(float x) { UF u; u.a = x; return u; }
float c_uf(UF u) { return u.b * 2; }
E8 c_id_e(E8 e) { return e; }
int c_z(Z z, int k) { (void)z; return k * 2; }
int c_va(int n, ...) {
    va_list ap;
    va_start(ap, n);
    P p = va_arg(ap, P);
    va_end(ap);
    return n * 100 + p.a * 10 + p.b;
}

/* Called through c_abi_mod.jam, whose `Pair` is 8 bytes while main.jam has a
 * 24-byte `Pair` of its own. */
P c_mod_pair(int a, int b) { P p = {a, b}; return p; }
int c_mod_sum(P p) { return p.a * 10 + p.b; }

/* Seven GP args fill x0..x6 (rdi..r9 + one stack slot on x86-64); the L2
 * cannot split across x7 and the stack. */
int64_t c_gp_exhaust(int64_t a0, int64_t a1, int64_t a2, int64_t a3, int64_t a4, int64_t a5,
                  int64_t a6, L2 s, int64_t tail) {
    return a6 + s.a * 10 + s.b * 100 + tail * 1000;
}

/* Same for FP registers v0..v7 with a two-double HFA. */
double c_fp_exhaust(double d0, double d1, double d2, double d3, double d4,
                    double d5, double d6, D2 s, double tail) {
    return d6 + s.x * 10 + s.y * 100 + tail * 1000;
}

/* A generic Jam struct `Pt(f64)` is a D2 at the boundary (HFA / SSE pair). */
double c_sum_pt(D2 d) { return d.x * 10 + d.y; }
D2 c_mk_pt(double x) { D2 d = {x, x + 1}; return d; }
double c_cb_pt(double (*f)(D2)) { D2 d = {3, 4}; return f(d); }

/* Narrow integer args: Apple arm64 has the CALLER extend them, so a Jam
 * caller that doesn't leaves garbage in the upper bits. */
int c_s16(short x) { return x; }
int c_u8(unsigned char x) { return x; }
short c_ret_s16(int x) { return (short)x; }

/* Default argument promotions: a Jam f32 / i8 passed through `...` must
 * arrive as double / int. */
double c_va_promote(int n, ...) {
    va_list ap;
    va_start(ap, n);
    double f = va_arg(ap, double);
    int i = va_arg(ap, int);
    va_end(ap);
    return f * 100 + i;
}

double c_mod_q(D2 q) { return q.x * 10 + q.y; }

/* Jam fns as C callbacks returning narrow types: the caller relies on the
 * callee having extended the result (Apple arm64, x86-64). */
int c_cb_i8(signed char (*f)(int)) { return f(300); }
int c_cb_bool(_Bool (*f)(int)) { return f(257) ? 1 : 0; }
int c_cb_u16(unsigned short (*f)(int)) { return f(65536 + 9); }

/* The address of a Jam `export fn` taken by C: Jam's `jam_id8 as u64` must
 * be the same symbol (a narrow-int export has no separate thunk). */
signed char jam_id8(signed char x);
uint64_t c_addr_id8(void) { return (uint64_t)&jam_id8; }

/* Declared by main.jam and c_abi_mod.jam, each with its own unit-only
 * enum and its own element struct for the slice: same crossing, so both
 * declarations agree. A Jam slice is {ptr, len}. */
int64_t c_enum_tag(unsigned char e) { return e; }
typedef struct { const void *ptr; int64_t len; } JSlice;
int64_t c_slice_len(JSlice s) { return s.len; }

/* = c_abi_mod.jam's BigE: { N, B(i64, i64, i64) }, 32 bytes (memory class). */
typedef struct { unsigned char tag; int64_t a, b, c; } BigE;
BigE c_big_enum(int64_t x) { BigE e = {1, x, x + 1, x + 2}; return e; }
int64_t c_big_enum_sum(BigE e) { return e.tag == 1 ? e.a + e.b + e.c : -1; }

/* ---- calling Jam's exports ---- */

P jam_mk_p(int a, int b);
int jam_sum_p(P p, int k);
T12 jam_mk_t12(int a);
L2 jam_mk_l2(int64_t a);
D2 jam_mk_d2(double x);
double jam_sum_d2(D2 d);
F3 jam_mk_f3(float x);
Mix jam_mk_mix(int64_t a);
Big jam_mk_big(int64_t x);
int64_t jam_sum_big(Big g, int k);
Flag jam_mk_flag(bool on, int n);
int64_t jam_gp_exhaust(int64_t a0, int64_t a1, int64_t a2, int64_t a3, int64_t a4, int64_t a5,
                    int64_t a6, L2 s, int64_t tail);
double jam_fp_exhaust(double d0, double d1, double d2, double d3, double d4,
                      double d5, double d6, D2 s, double tail);
void jam_bump(P *p, P by);
int64_t jam_big_enum_sum(BigE e);
int64_t jam_mod_local(int64_t x);
float jam_uf(UF u);
int jam_z(Z z, int k);

static int fails;
#define CHECK(name, cond)                                   \
    do {                                                    \
        if (cond) {                                         \
            printf("ok   c->jam %s\n", name);               \
        } else {                                            \
            printf("FAIL c->jam %s\n", name);               \
            fails++;                                        \
        }                                                   \
    } while (0)

int c_run_tests(void) {
    P p = jam_mk_p(2, 3);
    CHECK("8B return", p.a == 2 && p.b == 3);
    CHECK("8B arg", jam_sum_p(p, 4) == 234);
    T12 t = jam_mk_t12(7);
    CHECK("12B return", t.a == 7 && t.b == 8 && t.c == 9);
    L2 l = jam_mk_l2(21);
    CHECK("16B return", l.a == 21 && l.b == 42);
    D2 d = jam_mk_d2(3.0);
    CHECK("HFA2 return", d.x == 3.0 && d.y == 1.5);
    CHECK("HFA2 arg", jam_sum_d2(d) == 4.5);
    F3 f = jam_mk_f3(1.0f);
    CHECK("HFA3 return", f.x == 1.0f && f.y == 2.0f && f.z == 3.0f);
    Mix m = jam_mk_mix(4);
    CHECK("mixed INT/SSE return", m.a == 4 && m.d == 4.5);
    Big g = jam_mk_big(100);
    CHECK("memory-class return", g.a == 100 && g.d == 103);
    CHECK("memory-class arg", jam_sum_big(g, 1000) == 1406);
    Flag fl = jam_mk_flag(true, 9);
    CHECK("bool member return", fl.on && fl.n == 9);
    L2 s = {2, 3};
    CHECK("GP exhaustion arg", jam_gp_exhaust(0, 0, 0, 0, 0, 0, 7, s, 4) == 4327);
    D2 sd = {2.0, 3.0};
    CHECK("FP exhaustion arg",
          jam_fp_exhaust(0, 0, 0, 0, 0, 0, 7.0, sd, 4.0) == 4327.0);
    P q = {1, 2};
    P by = {5, 6};
    jam_bump(&q, by);
    CHECK("mut param is a pointer", q.a == 6 && q.b == 8);
    UF u;
    u.a = 1.25f;
    CHECK("float union (HFA) arg", jam_uf(u) == 2.5f);
    Z z;
    CHECK("empty struct arg", jam_z(z, 21) == 42);
    BigE be = {1, 1, 10, 100};
    CHECK("imported export takes a big payload enum", jam_big_enum_sum(be) == 111);
    CHECK("imported export uses its module's own types", jam_mod_local(1) == 321);
    return fails;
}
