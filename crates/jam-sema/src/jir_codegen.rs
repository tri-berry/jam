/*
 * Copyright (c) 2026-present Raphael Amorim
 *
 * This file is part of jam.
 * Licensed under the Apache License, Version 2.0 with LLVM Exceptions.
 */

//! JIR codegen — walks a fully-typed [`JirFunction`] and emits LLVM IR. Purely
//! mechanical: no inference here, each `JirInst` maps to a few LLVM
//! instructions. [`jir_declare_prototype`] emits signatures first (so forward
//! references resolve), then [`jir_define_body`] lowers each body; both consult
//! the same [`crate::abi`] classifier, so caller and callee can't disagree on
//! by-value/by-pointer or direct/sret.

use std::collections::HashMap;

use jam_core::index::{StringIdx, TypeIdx};
use jam_core::param_mode::ParamMode;
use jam_llvm::{
    BasicBlock, CallConv, Function, IntPredicate, Linkage, RealPredicate, Type, Value,
    append_to_used,
};
use jam_syntax::ast_flat::{TypeKind, builtin};

use crate::abi::{
    CAbiClass, CParam, CSig, CWord, ParamAbi, ParamAbiKind, ReturnAbi, ReturnAbiKind,
    c_abi_float_leaves, classify_c_abi, classify_param, classify_return, is_by_ref, sysv_demote,
};
use crate::codegen_context::CodegenContext;
use crate::jir::{JirBlockRef, JirFunction, JirInst, JirRef, JirTag, NO_JIR_REF};

/// Classify a JIR function's return. `extern`/`tfn` preserve the user-written
/// type verbatim (no sret); a test returns void; `kNoType` is void.
fn jir_classify_return<'ctx>(
    jfn: &JirFunction,
    ctx: &CodegenContext<'ctx>,
) -> Result<ReturnAbi<'ctx>, String> {
    if jfn.is_extern || jfn.is_test {
        let direct_type = if jfn.is_test || jfn.return_type.is_none() {
            Some(ctx.context().void_type())
        } else {
            Some(ctx.get_llvm_type(jfn.return_type)?)
        };
        return Ok(ReturnAbi {
            kind: ReturnAbiKind::Direct,
            direct_type,
            sret_align: 0,
        });
    }
    if jfn.return_type.is_none() {
        return Ok(ReturnAbi {
            kind: ReturnAbiKind::Direct,
            direct_type: Some(ctx.context().void_type()),
            sret_align: 0,
        });
    }
    classify_return(jfn.return_type, ctx)
}

/// Does the LLVM signature have a leading `ptr sret(%T)` argument?
pub fn jir_return_is_sret(jfn: &JirFunction, ctx: &CodegenContext) -> bool {
    matches!(
        jir_classify_return(jfn, ctx).map(|r| r.kind),
        Ok(ReturnAbiKind::Indirect)
    )
}

/// Classify param `i`. `extern` preserves the user-written type verbatim (the
/// FFI boundary is what the user wrote, e.g. `*const T` for an out-ptr).
fn jir_classify_param<'ctx>(
    jfn: &JirFunction,
    i: usize,
    ctx: &CodegenContext<'ctx>,
) -> Result<ParamAbi<'ctx>, String> {
    let t = jfn.param_types[i];
    let mode = jfn.param_modes.get(i).copied().unwrap_or(ParamMode::Let);
    if jfn.is_extern {
        // `mut` params and arrays are what call sites pass them as: a
        // pointer (C's `T*`, and array decay). By-value aggregates never get
        // here: they take the C-ABI prototype (`declare_c_abi_fn`).
        let llvm_type = if mode == ParamMode::Mut || is_by_ref(t, ctx) {
            ctx.context().pointer_type(0)
        } else {
            ctx.get_llvm_type(t)?
        };
        return Ok(ParamAbi {
            kind: ParamAbiKind::ByValue,
            llvm_type: Some(llvm_type),
            pointer_align: 0,
        });
    }
    classify_param(mode, t, ctx)
}

// ---- C-ABI signatures (extern / export with by-value aggregates) ----
//
// Jam's own ABI passes every byref aggregate as a pointer to its storage and
// returns it through an sret slot. C does not: AAPCS64 and SysV split
// aggregates into Direct / Hfa / Coerce / Memory classes
// (`abi::classify_c_abi`). Signatures that carry an aggregate by value across
// the C boundary are therefore lowered twice-over:
//
//   extern fn   the prototype is declared with C classes and tagged
//               `C_ABI_ATTR`; each direct call converts Jam values to C
//               arguments and lifts the C result back into a Jam slot.
//   export fn   the body keeps Jam's ABI under `<name>` + `EXPORT_BODY_SUFFIX`
//               (internal linkage, what Jam callers use); a C-ABI wrapper owns
//               the exported symbol `<name>` (what C callers and `fn as u64`
//               get) and forwards to the body.
//
// Scalar-only signatures are identical under both ABIs and keep the plain
// lowering.

/// Marker on prototypes declared with C-ABI classification.
const C_ABI_ATTR: &str = "jam-c-abi";
/// LLVM name of an `export fn` body whose exported symbol is a C-ABI wrapper.
/// `$` never occurs in Jam's mangling (`mod.Type.fn`), so this can't collide.
const EXPORT_BODY_SUFFIX: &str = "$jam.body";

/// Does `ty` resolve to an array? Arrays are byref in Jam, but C has no
/// array parameters or returns: Jam's pointer lowering already is C's array
/// decay, so arrays never take the by-value C-ABI path.
fn is_array_shaped(ty: TypeIdx, ctx: &CodegenContext) -> bool {
    if ty.is_none() {
        return false;
    }
    let ty = ctx.requalify_type(ty, &ctx.current_body_module());
    let k = ctx.type_pool.get(ty);
    let next = match k.kind {
        TypeKind::Array => return true,
        TypeKind::ArrayExpr => ctx.resolve_array_expr(ty),
        TypeKind::GenericCall => ctx.resolve_generic_call(ty),
        TypeKind::Named => {
            let name =
                String::from_utf8_lossy(&ctx.string_pool.get(StringIdx::new(k.a))).into_owned();
            // A registered type is no array. Not chasing it also stops the
            // loop through swapped import handles (`const a = import("b");
            // const b = import("a")`), whose aliases map `a.S` and `b.S`
            // onto each other.
            if ctx.is_struct_name_registered(&name)
                || ctx.is_union_name_registered(&name)
                || ctx.is_enum_name_registered(&name)
            {
                return false;
            }
            ctx.lookup_type_alias(&name)
        }
        _ => return false,
    };
    !next.is_none() && next != ty && is_array_shaped(next, ctx)
}

/// A struct, union or payload enum: what C passes and returns by value.
fn is_c_aggregate(ty: TypeIdx, ctx: &CodegenContext) -> bool {
    is_by_ref(ty, ctx) && !is_array_shaped(ty, ctx)
}

/// Does a C caller of `jfn` expect an integer extension Jam's own ABI doesn't
/// do (a narrow int / bool param or return; see [`c_int_ext`])?
fn needs_c_int_ext(jfn: &JirFunction, ctx: &CodegenContext) -> bool {
    let ext_param = |i: usize| {
        jfn.param_modes.get(i).copied().unwrap_or(ParamMode::Let) != ParamMode::Mut
            && c_int_ext(jfn.param_types[i], ctx).is_some()
    };
    c_int_ext(jfn.return_type, ctx).is_some() || (0..jfn.param_types.len()).any(ext_param)
}

/// Does `jfn`'s signature pass or return an aggregate by value? Only then do
/// its Jam and C lowerings differ.
fn passes_aggregates(jfn: &JirFunction, ctx: &CodegenContext) -> bool {
    let by_value = |i: usize| {
        jfn.param_modes.get(i).copied().unwrap_or(ParamMode::Let) != ParamMode::Mut
            && is_c_aggregate(jfn.param_types[i], ctx)
    };
    is_c_aggregate(jfn.return_type, ctx) || (0..jfn.param_types.len()).any(by_value)
}

/// Does `jfn` pass or return an aggregate by value across the C boundary?
fn needs_c_abi(jfn: &JirFunction, ctx: &CodegenContext) -> bool {
    (jfn.is_extern || jfn.is_export) && passes_aggregates(jfn, ctx)
}

/// `(GPRs, SSE regs)` a pass-through param takes on x86-64 SysV.
fn sysv_scalar_regs(ty: TypeIdx, mode: ParamMode, ctx: &CodegenContext) -> (u32, u32) {
    if mode == ParamMode::Mut || is_by_ref(ty, ctx) {
        return (1, 0); // a pointer
    }
    let ty = ctx.requalify_type(ty, &ctx.current_body_module());
    match ctx.type_pool.get(ty).kind {
        TypeKind::Float => (0, 1),
        TypeKind::Slice => (2, 0), // {ptr, len}
        _ => (1, 0),
    }
}

/// The C ABI's integer-extension attribute for a scalar `ty` passed or
/// returned in a register, as clang emits it per target: on x86-64 SysV and
/// Apple (arm64 and x86-64), `bool` and unsigned integers narrower than 32
/// bits are zero-extended and signed ones sign-extended; Win64 extends only
/// `bool`; AAPCS64 elsewhere (Linux) extends nothing, the upper bits are
/// unspecified and neither side may rely on them.
fn c_int_ext(ty: TypeIdx, ctx: &CodegenContext) -> Option<&'static str> {
    use crate::target::{Arch, Os};
    let (arch, os) = (ctx.target_arch(), ctx.target_os());
    if arch == Arch::AArch64 && os != Os::MacOs {
        return None;
    }
    if ty == builtin::BOOL {
        return Some("zeroext");
    }
    if arch == Arch::X86_64 && os == Os::Windows {
        return None;
    }
    let k = ctx.type_pool.get(ty);
    if k.kind == TypeKind::Int && k.a < 32 {
        return Some(if k.b != 0 { "signext" } else { "zeroext" });
    }
    // A unit-only enum crosses as its `u8` tag: extended like a `u8`.
    if let Some(en) = ctx.enum_name_of(ty)
        && !ctx.enum_has_payload_by_name(&en).unwrap_or(true)
    {
        return Some("zeroext");
    }
    None
}

/// One by-value aggregate for the C side, resolved in the declaring module.
/// A zero-sized one has no argument under SysV and AAPCS64 (C drops empty
/// records); MSVC gives an empty C struct a nonzero size, so Win64 passes it
/// as one integer register (contents unread).
fn c_value(ty: TypeIdx, ctx: &CodegenContext) -> Result<CParam, String> {
    let ty = crate::abi::abi_concrete(ty, ctx);
    if ctx.type_size(ty)? == 0 {
        if ctx.target_os() == crate::target::Os::Windows {
            return Ok(CParam::Value(
                ty,
                CAbiClass::Coerce {
                    w0: CWord::Int,
                    w1: None,
                },
            ));
        }
        return Ok(CParam::Skip);
    }
    Ok(CParam::Value(ty, classify_c_abi(ty, ctx)?))
}

/// The C lowering of `jfn`'s signature, final classes included (x86-64 SysV
/// register demotion runs here, once, for every user of the prototype).
fn c_signature(jfn: &JirFunction, ctx: &CodegenContext) -> Result<CSig, String> {
    let rt = jfn.return_type;
    let ret = if rt.is_none() || rt == builtin::NORETURN || !is_by_ref(rt, ctx) {
        CParam::Pass
    } else if is_array_shaped(rt, ctx) {
        // C has no array returns: keep Jam's sret shape.
        CParam::Value(
            ctx.requalify_type(rt, &ctx.current_body_module()),
            CAbiClass::Memory { byval: false },
        )
    } else {
        c_value(rt, ctx)?
    };
    let mut params = Vec::with_capacity(jfn.param_types.len());
    let mut scalars = Vec::with_capacity(jfn.param_types.len());
    for (i, &t) in jfn.param_types.iter().enumerate() {
        let mode = jfn.param_modes.get(i).copied().unwrap_or(ParamMode::Let);
        if mode == ParamMode::Mut || !is_c_aggregate(t, ctx) {
            params.push(CParam::Pass);
            scalars.push(sysv_scalar_regs(t, mode, ctx));
        } else {
            params.push(c_value(t, ctx)?);
            scalars.push((0, 0));
        }
    }
    if is_sysv_x86_64(ctx) {
        let sret = matches!(ret, CParam::Value(_, CAbiClass::Memory { .. }));
        sysv_demote(&mut params, &scalars, sret);
    }
    Ok(CSig { ret, params })
}

fn is_sysv_x86_64(ctx: &CodegenContext) -> bool {
    ctx.target_arch() == crate::target::Arch::X86_64
        && ctx.target_os() != crate::target::Os::Windows
}

/// The LLVM function holding `jfn`'s body, as declared: what Jam callers
/// target.
fn llvm_body_name(jfn: &JirFunction, ctx: &CodegenContext) -> String {
    if jfn.is_export && needs_c_abi(jfn, ctx) {
        format!("{}{EXPORT_BODY_SUFFIX}", jfn.name)
    } else {
        jfn.name.clone()
    }
}

/// The function holding the body of `name`: the wrapped export body when one
/// was declared, else `name` itself. Decided by what exists in the module,
/// so it can't drift from the declaration with the current module scope.
fn body_fn<'ctx>(ctx: &CodegenContext<'ctx>, name: &str) -> Option<Function<'ctx>> {
    let module = ctx.module();
    module
        .get_function(&format!("{name}{EXPORT_BODY_SUFFIX}"))
        .or_else(|| module.get_function(name))
}

/// Alignment of a coerced-word temp holding a `ty`: words need 8, and the
/// struct's own alignment must not be lowered.
fn coerce_tmp_align(ty: TypeIdx, ctx: &CodegenContext) -> Result<u64, String> {
    Ok(ctx.type_align(ty)?.max(8))
}

/// One or two 64-bit words (`i64` / `double` per eightbyte class).
///
/// AArch64 spells two words `[2 x i64]`, as clang and Zig do: LLVM gives
/// arrays AAPCS64's consecutive-register rule, so a 16-byte struct that no
/// longer fits in x0..x7 goes wholly to the stack. A `{i64, i64}` struct
/// would be split between x7 and the stack. x86-64 keeps the eightbyte
/// struct (INTEGER/SSE words may mix there; `abi::sysv_demote` handles
/// register exhaustion).
fn c_coerce_ty<'ctx>(ctx: &CodegenContext<'ctx>, w0: CWord, w1: Option<CWord>) -> Type<'ctx> {
    let word = |w: CWord| {
        if w == CWord::Fp {
            ctx.context().f64_type()
        } else {
            ctx.context().i64_type()
        }
    };
    match w1 {
        None => word(w0),
        Some(_) if ctx.target_arch() != crate::target::Arch::X86_64 => {
            ctx.context().i64_type().array_type(2)
        }
        Some(w1) => ctx.context().struct_type(&[word(w0), word(w1)], false),
    }
}

/// An HFA as `[N x float]` / `[N x double]` (AAPCS64, same reason as the
/// `[2 x i64]` above: v0..v7 are consecutive or the whole value is on the
/// stack). Layout-identical to the aggregate, so loads and stores through
/// its storage are sound.
fn c_hfa_ty<'ctx>(ty: TypeIdx, ctx: &CodegenContext<'ctx>) -> Result<Type<'ctx>, String> {
    let (bits, count) = c_abi_float_leaves(ty, ctx).ok_or("HFA class without float leaves")?;
    let elem = match bits {
        32 => ctx.context().f32_type(),
        64 => ctx.context().f64_type(),
        _ => return ctx.get_llvm_type(ty),
    };
    Ok(elem.array_type(count as u64))
}

/// How a C argument of class `class` travels in LLVM.
fn c_param_llvm_ty<'ctx>(
    ty: TypeIdx,
    class: CAbiClass,
    ctx: &CodegenContext<'ctx>,
) -> Result<Type<'ctx>, String> {
    Ok(match class {
        CAbiClass::Direct => ctx.get_llvm_type(ty)?,
        CAbiClass::Hfa => c_hfa_ty(ty, ctx)?,
        CAbiClass::Coerce { w0, w1 } => c_coerce_ty(ctx, w0, w1),
        CAbiClass::Memory { .. } => ctx.context().pointer_type(0),
    })
}

/// LLVM return type of a C prototype whose Jam return type is `rt`.
fn c_ret_llvm_ty<'ctx>(
    rt: TypeIdx,
    ret: CParam,
    ctx: &CodegenContext<'ctx>,
) -> Result<Type<'ctx>, String> {
    Ok(match ret {
        CParam::Pass if rt.is_none() => ctx.context().void_type(),
        CParam::Pass => ctx.get_llvm_type(rt)?,
        CParam::Skip | CParam::Value(_, CAbiClass::Memory { .. }) => ctx.context().void_type(),
        CParam::Value(t, class) => c_param_llvm_ty(t, class, ctx)?,
    })
}

/// Declare `name` with `jfn`'s signature under C-ABI classification, and
/// record the lowering for its call sites (and an export's wrapper).
fn declare_c_abi_fn<'ctx>(
    jfn: &JirFunction,
    ctx: &CodegenContext<'ctx>,
    name: &str,
) -> Result<Function<'ctx>, String> {
    if let Some(f) = ctx.module().get_function(name)
        && f.has_marker_attr(C_ABI_ATTR)
    {
        return Ok(f);
    }
    let sig = c_signature(jfn, ctx)?;
    let sret = matches!(sig.ret, CParam::Value(_, CAbiClass::Memory { .. }));
    let mut arg_types = Vec::with_capacity(jfn.param_types.len() + sret as usize);
    if sret {
        arg_types.push(ctx.context().pointer_type(0));
    }
    let mut byval: Vec<(u32, TypeIdx)> = Vec::new();
    let mut exts: Vec<(u32, &'static str)> = Vec::new();
    for (i, (&t, &p)) in jfn.param_types.iter().zip(&sig.params).enumerate() {
        let mode = jfn.param_modes.get(i).copied().unwrap_or(ParamMode::Let);
        match p {
            // What Jam's own ABI hands over: a pointer for `mut` params and
            // arrays, the scalar otherwise.
            CParam::Pass => {
                if mode == ParamMode::Mut || is_by_ref(t, ctx) {
                    arg_types.push(ctx.context().pointer_type(0));
                } else {
                    if let Some(ext) = c_int_ext(t, ctx) {
                        exts.push((arg_types.len() as u32, ext));
                    }
                    arg_types.push(ctx.get_llvm_type(t)?);
                }
            }
            CParam::Skip => {}
            CParam::Value(pt, class) => {
                if matches!(class, CAbiClass::Memory { byval: true }) {
                    byval.push((arg_types.len() as u32, pt));
                }
                arg_types.push(c_param_llvm_ty(pt, class, ctx)?);
            }
        }
    }
    let ret_type = c_ret_llvm_ty(jfn.return_type, sig.ret, ctx)?;
    // Every type the attributes need, BEFORE the function exists: one that
    // can't lower yet (an imported payload enum ahead of the type bodies)
    // must fail the whole declaration, not leave a half-declared function
    // with no `sret`/`byval` that call sites then bind to.
    let sret_ty = match sig.ret {
        CParam::Value(rt, CAbiClass::Memory { .. }) => {
            Some((ctx.get_llvm_type(rt)?, ctx.type_align(rt)? as u32))
        }
        _ => None,
    };
    let byval: Vec<(u32, Type<'ctx>, u32)> = byval
        .into_iter()
        .map(|(idx, pt)| Ok((idx, ctx.get_llvm_type(pt)?, ctx.type_align(pt)? as u32)))
        .collect::<Result<_, String>>()?;
    let f = ctx
        .module()
        .add_function(name, ret_type.fn_type(&arg_types, jfn.is_var_args));
    f.apply_default_attrs(jfn.is_extern);
    if jfn.return_type == builtin::NORETURN {
        f.set_no_return();
    }
    if let Some((ty, align)) = sret_ty {
        f.add_param_attr_sret(0, ty, align);
    }
    for (idx, ty, align) in byval {
        f.add_param_attr_byval(idx, ty, align);
    }
    for (idx, ext) in exts {
        f.add_param_attr_ext(idx, ext);
    }
    if matches!(sig.ret, CParam::Pass)
        && let Some(ext) = c_int_ext(jfn.return_type, ctx)
    {
        f.add_ret_attr_ext(ext);
    }
    f.set_linkage(Linkage::External);
    f.set_call_conv(CallConv::C);
    f.add_marker_attr(C_ABI_ATTR);
    ctx.record_c_sig(name, sig);
    if jfn.is_export {
        append_to_used(ctx.module(), f);
    }
    Ok(f)
}

/// A Jam value (aggregates: a pointer to storage) as a C argument of `class`.
/// Never hands C the caller's storage where C expects to own a copy.
fn lower_c_arg<'ctx>(
    lctx: &mut JirCodegenCtx<'_, 'ctx>,
    av: Value<'ctx>,
    pt: TypeIdx,
    class: CAbiClass,
) -> Result<Value<'ctx>, String> {
    Ok(match class {
        CAbiClass::Direct => av,
        CAbiClass::Hfa => {
            let hfa_ty = c_hfa_ty(pt, lctx.ctx)?;
            lctx.ctx.builder().load(hfa_ty, av, "arg.hfa")
        }
        CAbiClass::Coerce { w0, w1 } => {
            // memcpy into a word-aligned temp so a 12-byte struct never
            // over-reads, then load the coerced value.
            let ct = c_coerce_ty(lctx.ctx, w0, w1);
            let size = lctx.ctx.type_size(pt)?;
            let align = lctx.ctx.type_align(pt)?;
            let tmp_align = coerce_tmp_align(pt, lctx.ctx)?;
            let tmp = lctx.ctx.builder().alloca(ct, tmp_align, "arg.coerce");
            lctx.ctx.builder().memcpy(tmp, tmp_align, av, align, size);
            lctx.ctx.builder().load(ct, tmp, "arg.coerce.v")
        }
        CAbiClass::Memory { .. } => {
            let agg_ty = lctx.ctx.get_llvm_type(pt)?;
            let size = lctx.ctx.type_size(pt)?;
            let align = lctx.ctx.type_align(pt)?;
            let copy = lctx.ctx.builder().alloca(agg_ty, align, "arg.mem.copy");
            lctx.ctx.builder().memcpy(copy, align, av, align, size);
            copy
        }
    })
}

/// A variadic argument past the fixed params: C passes it by its own type
/// (default argument promotions don't touch aggregates). `None` when C has
/// no argument for it (a zero-sized aggregate).
fn lower_c_vararg<'ctx>(
    lctx: &mut JirCodegenCtx<'_, 'ctx>,
    av: Value<'ctx>,
    t: TypeIdx,
    byval: &mut Vec<(u32, TypeIdx)>,
    index: u32,
) -> Result<Option<Value<'ctx>>, String> {
    if !is_c_aggregate(t, lctx.ctx) {
        // C's default argument promotions for a `...` argument: `float`
        // becomes `double`, and `bool` and integers narrower than `int`
        // widen to `int` by their signedness (`printf("%f", x_f32)` printed
        // 0.0, `%d` of an `i8` printed its byte).
        let k = lctx.ctx.type_pool.get(t);
        let b = lctx.ctx.builder();
        if k.kind == TypeKind::Float && k.a == 32 {
            return Ok(Some(b.fp_cast(
                av,
                lctx.ctx.context().f64_type(),
                "vararg.fpext",
            )));
        }
        if t == builtin::BOOL || (k.kind == TypeKind::Int && k.a < 32) {
            let signed = t != builtin::BOOL && k.b != 0;
            return Ok(Some(b.int_cast(
                av,
                lctx.ctx.context().i32_type(),
                signed,
                "vararg.ext",
            )));
        }
        // A unit-only enum is its `u8` tag: promoted like an unsigned char.
        if let Some(en) = lctx.ctx.enum_name_of(t)
            && !lctx.ctx.enum_has_payload_by_name(&en).unwrap_or(true)
        {
            return Ok(Some(b.int_cast(
                av,
                lctx.ctx.context().i32_type(),
                false,
                "vararg.ext",
            )));
        }
        return Ok(Some(av));
    }
    if lctx.ctx.type_size(t)? == 0 {
        return Ok(None);
    }
    let class = classify_c_abi(t, lctx.ctx)?;
    if matches!(class, CAbiClass::Memory { byval: true }) {
        byval.push((index, t));
    }
    Ok(Some(lower_c_arg(lctx, av, t, class)?))
}

/// Direct call to a C-ABI prototype, lowered by its recorded [`CSig`].
/// `args[0]` is Jam's sret slot when the caller sees a byref return (astgen
/// adds it from Jam's own classification); the C result is lifted into it.
fn emit_c_abi_call<'ctx>(
    lctx: &mut JirCodegenCtx<'_, 'ctx>,
    f: Function<'ctx>,
    symbol: &str,
    ret_ty: TypeIdx,
    arg_refs: &[JirRef],
    args: &[Value<'ctx>],
) -> Result<Option<Value<'ctx>>, String> {
    let sig = lctx
        .ctx
        .c_sig(symbol)
        .ok_or_else(|| format!("C-ABI call: no signature recorded for `{symbol}`"))?;
    let has_slot = is_by_ref(ret_ty, lctx.ctx);
    let (slot, jam_refs, jam_args) = if has_slot {
        (Some(args[0]), &arg_refs[1..], &args[1..])
    } else {
        (None, arg_refs, args)
    };
    let ret_is_memory = matches!(sig.ret, CParam::Value(_, CAbiClass::Memory { .. }));

    let mut c_args = Vec::with_capacity(args.len());
    if ret_is_memory {
        c_args.push(slot.ok_or("C-ABI call: Memory return without a slot")?);
    }
    let mut byval: Vec<(u32, TypeIdx)> = Vec::new();
    for (i, (&r, &av)) in jam_refs.iter().zip(jam_args).enumerate() {
        match sig.params.get(i) {
            Some(CParam::Pass) => c_args.push(av),
            Some(CParam::Skip) => {}
            Some(&CParam::Value(pt, class)) => {
                if matches!(class, CAbiClass::Memory { byval: true }) {
                    byval.push((c_args.len() as u32, pt));
                }
                let lowered = lower_c_arg(lctx, av, pt, class)?;
                c_args.push(lowered);
            }
            // Variadic extras lower by the argument's own type (Zig's
            // `ParamTypeIterator.nextCall` does the same).
            None => {
                let t = lctx.jfn.get_inst(r).ty;
                let idx = c_args.len() as u32;
                if let Some(v) = lower_c_vararg(lctx, av, t, &mut byval, idx)? {
                    c_args.push(v);
                }
            }
        }
    }
    let returns_value = match sig.ret {
        CParam::Pass => !ret_ty.is_none() && ret_ty != builtin::NORETURN,
        CParam::Skip | CParam::Value(_, CAbiClass::Memory { .. }) => false,
        CParam::Value(..) => true,
    };
    let result = lctx
        .ctx
        .builder()
        .call(f, &c_args, if returns_value { "call.c" } else { "" });
    for (idx, pt) in byval {
        let agg_ty = lctx.ctx.get_llvm_type(pt)?;
        let align = lctx.ctx.type_align(pt)?;
        result.add_call_site_byval(idx, agg_ty, align as u32);
    }
    let Some(slot) = slot else {
        return Ok(Some(result));
    };
    // Never write past the caller's slot, whatever the callee's type says.
    let slot_size = lctx.ctx.type_size(ret_ty)?;
    match sig.ret {
        CParam::Value(rt, CAbiClass::Memory { .. }) => {
            let agg_ty = lctx.ctx.get_llvm_type(rt)?;
            let align = lctx.ctx.type_align(rt)?;
            result.add_call_site_sret(0, agg_ty, align as u32);
        }
        CParam::Value(rt, CAbiClass::Coerce { w0, w1 }) => {
            // Spill the words to a word-sized temp, then copy only the
            // struct's bytes into the Jam slot.
            let ct = c_coerce_ty(lctx.ctx, w0, w1);
            let tmp_align = coerce_tmp_align(rt, lctx.ctx)?;
            let tmp = lctx.ctx.builder().alloca(ct, tmp_align, "ret.coerce");
            lctx.ctx.builder().store(result, tmp);
            let size = lctx.ctx.type_size(rt)?.min(slot_size);
            let align = lctx.ctx.type_align(ret_ty)?;
            lctx.ctx.builder().memcpy(slot, align, tmp, tmp_align, size);
        }
        CParam::Value(rt, CAbiClass::Hfa | CAbiClass::Direct) => {
            if lctx.ctx.type_size(rt)? <= slot_size {
                lctx.ctx.builder().store(result, slot);
            }
        }
        CParam::Skip => {}
        CParam::Pass => {
            if returns_value {
                lctx.ctx.builder().store(result, slot);
            }
        }
    }
    Ok(Some(slot))
}

/// Body of an `export fn`'s C-ABI wrapper: C arguments in, Jam-ABI body call,
/// C return out, all by the wrapper's recorded [`CSig`].
///
/// Ownership at this boundary is C's: a by-value aggregate is the callee's
/// own copy, so a `move` param's drop runs on that copy. A Jam caller that
/// reaches the wrapper by address (`@callC(.., f as u64, ..)`) keeps its
/// original too, so a drop-bearing `move` arg is dropped twice; call the
/// export directly (which targets the Jam-ABI body) to move it.
fn emit_export_wrapper<'ctx>(jfn: &JirFunction, ctx: &CodegenContext<'ctx>) -> Result<(), String> {
    let w = ctx
        .module()
        .get_function(&jfn.name)
        .ok_or("export wrapper: prototype missing")?;
    let body = body_fn(ctx, &jfn.name).ok_or("export wrapper: body missing")?;
    emit_c_wrapper(jfn, ctx, &jfn.name, w, body)
}

/// Fill `w`, declared by `declare_c_abi_fn` under `w_name`, with a C-ABI to
/// Jam-ABI adapter that calls `body` (`jfn`'s Jam-ABI function).
fn emit_c_wrapper<'ctx>(
    jfn: &JirFunction,
    ctx: &CodegenContext<'ctx>,
    w_name: &str,
    w: Function<'ctx>,
    body: Function<'ctx>,
) -> Result<(), String> {
    let sig = ctx
        .c_sig(w_name)
        .ok_or("C wrapper: no signature recorded")?;
    let b = ctx.builder();
    b.position_at_end(w.append_basic_block("entry"));

    let ret_ty = jfn.return_type;
    let mut ci = 0u32;
    let c_sret = if matches!(sig.ret, CParam::Value(_, CAbiClass::Memory { .. })) {
        ci = 1;
        Some(w.param(0))
    } else {
        None
    };
    let mut jam_args = Vec::with_capacity(jfn.param_types.len() + 1);
    // Jam's body returns byref aggregates through a leading slot: reuse C's
    // sret buffer when there is one, else a local the result is read from.
    let jam_slot = if is_by_ref(ret_ty, ctx) {
        let s = match c_sret {
            Some(p) => p,
            None => b.alloca(
                ctx.get_llvm_type(ret_ty)?,
                ctx.type_align(ret_ty)?,
                "ret.slot",
            ),
        };
        jam_args.push(s);
        Some(s)
    } else {
        None
    };
    for (i, &p) in sig.params.iter().enumerate() {
        let jam = match p {
            // Scalars, and `mut` params / arrays that C hands over as
            // pointers: already what the Jam body takes.
            CParam::Pass => {
                ci += 1;
                w.param(ci - 1)
            }
            // C passed nothing; the body still wants storage to point at.
            CParam::Skip => {
                let t = jfn.param_types[i];
                b.alloca(ctx.get_llvm_type(t)?, ctx.type_align(t)?, "arg.empty")
            }
            CParam::Value(pt, class) => {
                let v = w.param(ci);
                ci += 1;
                match class {
                    // C already passed a pointer to its own copy.
                    CAbiClass::Memory { .. } => v,
                    CAbiClass::Coerce { w0, w1 } => {
                        let tmp = b.alloca(
                            c_coerce_ty(ctx, w0, w1),
                            coerce_tmp_align(pt, ctx)?,
                            "arg.coerce",
                        );
                        b.store(v, tmp);
                        tmp
                    }
                    // An HFA arrives as `[N x float]`, layout-identical to the
                    // aggregate, so storing it into its storage is sound.
                    CAbiClass::Hfa | CAbiClass::Direct => {
                        let tmp =
                            b.alloca(ctx.get_llvm_type(pt)?, ctx.type_align(pt)?, "arg.spill");
                        b.store(v, tmp);
                        tmp
                    }
                }
            }
        };
        jam_args.push(jam);
    }
    let returns_scalar = !ret_ty.is_none() && ret_ty != builtin::NORETURN && jam_slot.is_none();
    let r = b.call(body, &jam_args, if returns_scalar { "body" } else { "" });
    if ret_ty == builtin::NORETURN {
        b.unreachable();
        return Ok(());
    }
    match (jam_slot, sig.ret) {
        (None, _) if ret_ty.is_none() => {
            b.ret_void();
        }
        (None, _) => {
            b.ret(r);
        }
        (Some(_), CParam::Skip | CParam::Value(_, CAbiClass::Memory { .. })) => {
            b.ret_void();
        }
        (Some(slot), CParam::Value(rt, CAbiClass::Coerce { w0, w1 })) => {
            let ct = c_coerce_ty(ctx, w0, w1);
            let tmp_align = coerce_tmp_align(rt, ctx)?;
            let tmp = b.alloca(ct, tmp_align, "ret.coerce");
            let size = ctx.type_size(rt)?;
            let align = ctx.type_align(rt)?;
            b.memcpy(tmp, tmp_align, slot, align, size);
            let v = b.load(ct, tmp, "ret.coerce.v");
            b.ret(v);
        }
        (Some(slot), CParam::Value(rt, class)) => {
            let ty = c_param_llvm_ty(rt, class, ctx)?;
            let v = b.load(ty, slot, "ret.v");
            b.ret(v);
        }
        (Some(slot), CParam::Pass) => {
            let v = b.load(ctx.get_llvm_type(ret_ty)?, slot, "ret.v");
            b.ret(v);
        }
    }
    Ok(())
}

/// Suffix of the internal C-ABI entry of a plain Jam fn whose address is
/// taken.
const C_THUNK_SUFFIX: &str = "$jam.cabi";

/// The address a fn-ref to `name` yields. Calls through fn values use the C
/// ABI (see `CallIndirect`), which externs and exports already have. A plain
/// Jam fn that passes or returns an aggregate by value takes it by pointer
/// instead, so its address is an internal C-ABI wrapper over it, built on
/// first use.
fn fn_ref_target<'ctx>(ctx: &CodegenContext<'ctx>, name: &str) -> Result<Function<'ctx>, String> {
    let f = ctx
        .module()
        .get_function(name)
        .ok_or_else(|| format!("unknown fn-ref `{name}`"))?;
    if f.has_marker_attr(C_ABI_ATTR) {
        return Ok(f);
    }
    let Some(ast) = ctx.get_function_ast(name) else {
        return Ok(f);
    };
    // An extern is C already; so is an export, whose symbol carries the C
    // attributes itself (one address, whichever side takes it).
    if ast.is_extern || ast.is_export {
        return Ok(f);
    }
    let mut sig = JirFunction::new();
    sig.name = name.to_string();
    sig.param_types = ast.args.iter().map(|p| p.ty).collect();
    sig.param_modes = ast.args.iter().map(|p| p.mode).collect();
    sig.return_type = ast.return_type;
    sig.module_path = ast.module_path.clone();
    sig.is_var_args = ast.is_var_args;
    ctx.push_body_module(sig.module_path.clone());
    let r = (|| {
        // A C caller (a callback) also expects narrow ints extended, which
        // the internal fn doesn't do: route those through the thunk too.
        if !passes_aggregates(&sig, ctx) && !needs_c_int_ext(&sig, ctx) {
            return Ok(f);
        }
        let thunk = format!("{name}{C_THUNK_SUFFIX}");
        if let Some(t) = ctx.module().get_function(&thunk) {
            return Ok(t);
        }
        let saved = ctx.builder().insert_block();
        let w = declare_c_abi_fn(&sig, ctx, &thunk)?;
        w.set_linkage(Linkage::Internal);
        let r = emit_c_wrapper(&sig, ctx, &thunk, w, f);
        if let Some(bb) = saved {
            ctx.builder().position_at_end(bb);
        }
        r.map(|_| w)
    })();
    ctx.pop_body_module();
    r
}

/// Emit the LLVM function declaration (signature + linkage + ABI attributes).
/// Run before any body so cross-function forward references resolve.
pub fn jir_declare_prototype<'ctx>(
    jfn: &JirFunction,
    ctx: &CodegenContext<'ctx>,
) -> Result<Function<'ctx>, String> {
    let c_abi = needs_c_abi(jfn, ctx);
    if c_abi {
        let f = declare_c_abi_fn(jfn, ctx, &jfn.name)?;
        if jfn.is_extern {
            return Ok(f);
        }
        // export: fall through to declare the Jam-ABI body under its own name.
    }
    let llvm_name = llvm_body_name(jfn, ctx);
    if c_abi && let Some(f) = ctx.module().get_function(&llvm_name) {
        return Ok(f);
    }

    let rabi = jir_classify_return(jfn, ctx)?;
    let sret = rabi.kind == ReturnAbiKind::Indirect;

    // Already declared (an early pass ran before every type's LLVM body
    // existed, so a payload enum's `sret` attribute couldn't be built): add
    // what's missing instead of declaring a renamed duplicate.
    if !jfn.is_extern
        && let Some(f) = ctx.module().get_function(&llvm_name)
    {
        if sret && !f.uses_sret() {
            f.add_param_attr_sret(0, ctx.get_llvm_type(jfn.return_type)?, rabi.sret_align);
        }
        return Ok(f);
    }

    // The `sret` pointee, built BEFORE declaring: when its LLVM type doesn't
    // exist yet (a payload enum before the type bodies are filled), fail
    // without leaving a half-declared function (no `sret`, default linkage)
    // for the post-fill pass to find.
    let sret_ty = if sret {
        Some(ctx.get_llvm_type(jfn.return_type)?)
    } else {
        None
    };
    let mut arg_types = Vec::with_capacity(jfn.param_types.len() + sret as usize);
    if sret {
        // Leading `ptr` carries the caller-owned return slot (sret attr below).
        arg_types.push(ctx.context().pointer_type(0));
    }
    for i in 0..jfn.param_types.len() {
        let pabi = jir_classify_param(jfn, i, ctx)?;
        match pabi.kind {
            ParamAbiKind::ByPointer => arg_types.push(ctx.context().pointer_type(0)),
            ParamAbiKind::ByValue => {
                arg_types.push(pabi.llvm_type.ok_or("ByValue param missing LLVM type")?)
            }
        }
    }

    let ret_type = if sret {
        ctx.context().void_type()
    } else {
        rabi.direct_type.ok_or("Direct return missing LLVM type")?
    };
    let ft = ret_type.fn_type(&arg_types, jfn.is_var_args);
    let f = ctx.module().add_function(&llvm_name, ft);
    f.apply_default_attrs(jfn.is_extern);
    if jfn.return_type == builtin::NORETURN {
        f.set_no_return();
    }
    if let Some(t) = sret_ty {
        f.add_param_attr_sret(0, t, rabi.sret_align);
    }

    // A wrapped export's body is internal: the C-ABI wrapper owns the symbol.
    let external_linkage = !c_abi && (jfn.is_extern || jfn.is_export || jfn.name == "main");
    if external_linkage {
        f.set_linkage(Linkage::External);
        // `export` symbols exist for C callers the optimizer can't see — pin
        // them in llvm.used so the internalize/global-DCE pass spares them.
        if jfn.is_export {
            append_to_used(ctx.module(), f);
        }
        f.set_call_conv(CallConv::C);
        // C ABI: bool and narrow integer args/returns are extended to the
        // register width. Internal-linkage callers use our own ABI.
        let arg_offset = if sret { 1u32 } else { 0 };
        for i in 0..jfn.param_types.len() {
            let mode = jfn.param_modes.get(i).copied().unwrap_or(ParamMode::Let);
            if mode != ParamMode::Mut
                && let Some(ext) = c_int_ext(jfn.param_types[i], ctx)
            {
                f.add_param_attr_ext(i as u32 + arg_offset, ext);
            }
        }
        if !sret && let Some(ext) = c_int_ext(jfn.return_type, ctx) {
            f.add_ret_attr_ext(ext);
        }
    } else {
        f.set_linkage(Linkage::Internal);
    }
    Ok(f)
}

// ---- body lowering ----

/// Per-function lowering state: `JirRef -> LLVM Value` (dataflow) and
/// `JirBlockRef -> LLVM BasicBlock` (terminators).
struct JirCodegenCtx<'a, 'ctx> {
    jfn: &'a JirFunction,
    ctx: &'a CodegenContext<'ctx>,
    value_map: HashMap<JirRef, Value<'ctx>>,
    block_map: HashMap<JirBlockRef, BasicBlock<'ctx>>,
}

/// Look up the cached LLVM value for `r`, or emit it (recursing for
/// subexpressions) and cache. The caching is load-bearing: a JirRef referenced
/// across out-of-order block walks must lower to exactly one LLVM value, or
/// reads split from writes.
fn emit_inst<'ctx>(
    lctx: &mut JirCodegenCtx<'_, 'ctx>,
    r: JirRef,
) -> Result<Option<Value<'ctx>>, String> {
    if r == NO_JIR_REF {
        return Ok(None);
    }
    if let Some(&v) = lctx.value_map.get(&r) {
        return Ok(Some(v));
    }
    let v = emit_inst_impl(lctx, r)?;
    if let Some(val) = v {
        lctx.value_map.insert(r, val);
    }
    Ok(v)
}

/// Emit both operands of a binary instruction (each must produce a value).
fn binop_operands<'ctx>(
    lctx: &mut JirCodegenCtx<'_, 'ctx>,
    inst: &JirInst,
) -> Result<(Value<'ctx>, Value<'ctx>), String> {
    let a = emit_inst(lctx, inst.a)?.ok_or("binop lhs produced no value")?;
    let b = emit_inst(lctx, inst.b)?.ok_or("binop rhs produced no value")?;
    Ok((a, b))
}

fn emit_inst_impl<'ctx>(
    lctx: &mut JirCodegenCtx<'_, 'ctx>,
    r: JirRef,
) -> Result<Option<Value<'ctx>>, String> {
    let inst = *lctx.jfn.get_inst(r);
    match inst.tag {
        JirTag::Poison => {
            let ty = if inst.ty.is_none() {
                lctx.ctx.context().void_type()
            } else {
                lctx.ctx.get_llvm_type(inst.ty)?
            };
            Ok(Some(ty.undef()))
        }
        JirTag::Int => {
            let val = (inst.a as u64) | ((inst.b as u64) << 32);
            let is_neg = inst.flags & 1 != 0;
            let ty = lctx.ctx.get_llvm_type(inst.ty)?;
            let materialized = if is_neg {
                (val as i64).wrapping_neg() as u64
            } else {
                val
            };
            Ok(Some(ty.const_int(materialized, is_neg)))
        }
        JirTag::Float => {
            let bits = (inst.a as u64) | ((inst.b as u64) << 32);
            let mut d = f64::from_bits(bits);
            if inst.flags & 1 != 0 {
                d = -d;
            }
            Ok(Some(lctx.ctx.get_llvm_type(inst.ty)?.const_real(d)))
        }
        JirTag::Bool => Ok(Some(
            lctx.ctx
                .context()
                .i1_type()
                .const_int(if inst.a != 0 { 1 } else { 0 }, false),
        )),

        // Integer arithmetic.
        JirTag::Add => {
            let (a, b) = binop_operands(lctx, &inst)?;
            Ok(Some(lctx.ctx.builder().add(a, b, "add")))
        }
        JirTag::Sub => {
            let (a, b) = binop_operands(lctx, &inst)?;
            Ok(Some(lctx.ctx.builder().sub(a, b, "sub")))
        }
        JirTag::Mul => {
            let (a, b) = binop_operands(lctx, &inst)?;
            Ok(Some(lctx.ctx.builder().mul(a, b, "mul")))
        }
        JirTag::SDiv => {
            let (a, b) = binop_operands(lctx, &inst)?;
            Ok(Some(lctx.ctx.builder().sdiv(a, b, "sdiv")))
        }
        JirTag::UDiv => {
            let (a, b) = binop_operands(lctx, &inst)?;
            Ok(Some(lctx.ctx.builder().udiv(a, b, "udiv")))
        }
        JirTag::SRem => {
            let (a, b) = binop_operands(lctx, &inst)?;
            Ok(Some(lctx.ctx.builder().srem(a, b, "srem")))
        }
        JirTag::URem => {
            let (a, b) = binop_operands(lctx, &inst)?;
            Ok(Some(lctx.ctx.builder().urem(a, b, "urem")))
        }

        // Float arithmetic.
        JirTag::FAdd => {
            let (a, b) = binop_operands(lctx, &inst)?;
            Ok(Some(lctx.ctx.builder().fadd(a, b, "fadd")))
        }
        JirTag::FSub => {
            let (a, b) = binop_operands(lctx, &inst)?;
            Ok(Some(lctx.ctx.builder().fsub(a, b, "fsub")))
        }
        JirTag::FMul => {
            let (a, b) = binop_operands(lctx, &inst)?;
            Ok(Some(lctx.ctx.builder().fmul(a, b, "fmul")))
        }
        JirTag::FDiv => {
            let (a, b) = binop_operands(lctx, &inst)?;
            Ok(Some(lctx.ctx.builder().fdiv(a, b, "fdiv")))
        }
        JirTag::FRem => {
            let (a, b) = binop_operands(lctx, &inst)?;
            Ok(Some(lctx.ctx.builder().frem(a, b, "frem")))
        }
        JirTag::FNeg => {
            let a = emit_inst(lctx, inst.a)?.ok_or("FNeg operand")?;
            Ok(Some(lctx.ctx.builder().fneg(a, "fneg")))
        }

        // Integer comparison.
        JirTag::ICmpEq => icmp(lctx, &inst, IntPredicate::Eq, "eq"),
        JirTag::ICmpNe => icmp(lctx, &inst, IntPredicate::Ne, "ne"),
        JirTag::ICmpSlt => icmp(lctx, &inst, IntPredicate::Slt, "slt"),
        JirTag::ICmpSle => icmp(lctx, &inst, IntPredicate::Sle, "sle"),
        JirTag::ICmpSgt => icmp(lctx, &inst, IntPredicate::Sgt, "sgt"),
        JirTag::ICmpSge => icmp(lctx, &inst, IntPredicate::Sge, "sge"),
        JirTag::ICmpUlt => icmp(lctx, &inst, IntPredicate::Ult, "ult"),
        JirTag::ICmpUle => icmp(lctx, &inst, IntPredicate::Ule, "ule"),
        JirTag::ICmpUgt => icmp(lctx, &inst, IntPredicate::Ugt, "ugt"),
        JirTag::ICmpUge => icmp(lctx, &inst, IntPredicate::Uge, "uge"),

        // Float comparison (ordered; note `FCmpOne` maps to LLVM `une`).
        JirTag::FCmpOeq => fcmp(lctx, &inst, RealPredicate::Oeq, "oeq"),
        JirTag::FCmpOne => fcmp(lctx, &inst, RealPredicate::Une, "one"),
        JirTag::FCmpOlt => fcmp(lctx, &inst, RealPredicate::Olt, "olt"),
        JirTag::FCmpOle => fcmp(lctx, &inst, RealPredicate::Ole, "ole"),
        JirTag::FCmpOgt => fcmp(lctx, &inst, RealPredicate::Ogt, "ogt"),
        JirTag::FCmpOge => fcmp(lctx, &inst, RealPredicate::Oge, "oge"),

        // Bitwise / shift.
        JirTag::BitAnd => {
            let (a, b) = binop_operands(lctx, &inst)?;
            Ok(Some(lctx.ctx.builder().and(a, b, "and")))
        }
        JirTag::BitOr => {
            let (a, b) = binop_operands(lctx, &inst)?;
            Ok(Some(lctx.ctx.builder().or(a, b, "or")))
        }
        JirTag::BitXor => {
            let (a, b) = binop_operands(lctx, &inst)?;
            Ok(Some(lctx.ctx.builder().xor(a, b, "xor")))
        }
        JirTag::Shl => {
            let (a, b) = binop_operands(lctx, &inst)?;
            Ok(Some(lctx.ctx.builder().shl(a, b, "shl")))
        }
        JirTag::AShr => {
            let (a, b) = binop_operands(lctx, &inst)?;
            Ok(Some(lctx.ctx.builder().ashr(a, b, "ashr")))
        }
        JirTag::LShr => {
            let (a, b) = binop_operands(lctx, &inst)?;
            Ok(Some(lctx.ctx.builder().lshr(a, b, "lshr")))
        }
        JirTag::BitNot => {
            // LLVM has no NOT; xor with all-ones of the operand's type.
            let v = emit_inst(lctx, inst.a)?.ok_or("BitNot operand")?;
            let ty = lctx.ctx.get_llvm_type(inst.ty)?;
            let ones = ty.const_int(!0u64, true);
            Ok(Some(lctx.ctx.builder().xor(v, ones, "not")))
        }
        JirTag::LogNot => {
            // Boolean inversion: xor with i1 1 (operand is already i1).
            let v = emit_inst(lctx, inst.a)?.ok_or("LogNot operand")?;
            let one = lctx.ctx.context().i1_type().const_int(1, false);
            Ok(Some(lctx.ctx.builder().xor(v, one, "lnot")))
        }

        // Type conversions.
        JirTag::ZExt => cast(lctx, &inst, "zext", |b, v, t| b.zext(v, t, "zext")),
        JirTag::SExt => cast(lctx, &inst, "sext", |b, v, t| b.sext(v, t, "sext")),
        JirTag::Trunc => cast(lctx, &inst, "trunc", |b, v, t| b.trunc(v, t, "trunc")),
        JirTag::SIToFP => cast(lctx, &inst, "si2fp", |b, v, t| b.si_to_fp(v, t, "si2fp")),
        JirTag::UIToFP => cast(lctx, &inst, "ui2fp", |b, v, t| b.ui_to_fp(v, t, "ui2fp")),
        JirTag::FPToSI => cast(lctx, &inst, "fp2si", |b, v, t| b.fp_to_si(v, t, "fp2si")),
        JirTag::FPToUI => cast(lctx, &inst, "fp2ui", |b, v, t| b.fp_to_ui(v, t, "fp2ui")),
        JirTag::FPExt | JirTag::FPTrunc => {
            cast(lctx, &inst, "fpcast", |b, v, t| b.fp_cast(v, t, "fpcast"))
        }
        JirTag::BitCast => cast(lctx, &inst, "bitcast", |b, v, t| b.bitcast(v, t, "bitcast")),
        JirTag::PtrToInt => cast(lctx, &inst, "p2i", |b, v, t| b.ptr_to_int(v, t, "p2i")),
        JirTag::IntToPtr => cast(lctx, &inst, "i2p", |b, v, t| b.int_to_ptr(v, t, "i2p")),

        // Control flow.
        JirTag::Br => {
            let bb = *lctx
                .block_map
                .get(&inst.a)
                .ok_or("Br: missing target block")?;
            lctx.ctx.builder().br(bb);
            Ok(None)
        }
        JirTag::CondBr => {
            let cond = emit_inst(lctx, inst.a)?.ok_or("CondBr cond")?;
            let then_b = lctx.jfn.get_extra(inst.b);
            let else_b = lctx.jfn.get_extra(inst.b + 1);
            let then_bb = *lctx.block_map.get(&then_b).ok_or("CondBr then block")?;
            let else_bb = *lctx.block_map.get(&else_b).ok_or("CondBr else block")?;
            lctx.ctx.builder().cond_br(cond, then_bb, else_bb);
            Ok(None)
        }
        JirTag::Switch => emit_switch(lctx, &inst),
        JirTag::Ret => emit_ret(lctx, &inst),
        JirTag::Unreachable => {
            lctx.ctx.builder().unreachable();
            Ok(None)
        }

        // ---- function parameters ----
        JirTag::Param => {
            // When the function uses sret, source param `i` is LLVM arg `i+1`.
            let f = body_fn(lctx.ctx, &lctx.jfn.name).ok_or("Param: fn missing")?;
            let arg_offset = if jir_return_is_sret(lctx.jfn, lctx.ctx) {
                1
            } else {
                0
            };
            Ok(Some(f.param(inst.a + arg_offset)))
        }
        JirTag::SretArg => {
            let f = body_fn(lctx.ctx, &lctx.jfn.name).ok_or("SretArg: fn missing")?;
            Ok(Some(f.param(0)))
        }

        // ---- storage ----
        JirTag::Alloca => {
            let ty = lctx.ctx.get_llvm_type(inst.ty)?;
            let align = lctx.ctx.alloca_align(inst.ty)?;
            Ok(Some(lctx.ctx.builder().alloca(ty, align, &format!("v{r}"))))
        }
        JirTag::Load => {
            let ptr = emit_inst(lctx, inst.a)?.ok_or("Load ptr")?;
            // Byref aggregate: the value IS the storage pointer (no SSA load).
            if is_by_ref(inst.ty, lctx.ctx) {
                return Ok(Some(ptr));
            }
            let ty = lctx.ctx.get_llvm_type(inst.ty)?;
            Ok(Some(lctx.ctx.builder().load(ty, ptr, "load")))
        }
        JirTag::Store => {
            let ptr = emit_inst(lctx, inst.a)?.ok_or("Store ptr")?;
            let val = emit_inst(lctx, inst.b)?.ok_or("Store val")?;
            let val_ty = lctx.jfn.get_inst(inst.b).ty;
            if is_by_ref(val_ty, lctx.ctx) {
                // `val` is a pointer to a byref aggregate — memcpy, don't store.
                let size = lctx.ctx.type_size(val_ty)?;
                let align = lctx.ctx.alloca_align(val_ty)?;
                lctx.ctx.builder().memcpy(ptr, align, val, align, size);
            } else {
                lctx.ctx.builder().store(val, ptr);
            }
            Ok(None)
        }
        JirTag::AddrOf => emit_inst(lctx, inst.a),
        JirTag::Deref => {
            let ptr = emit_inst(lctx, inst.a)?.ok_or("Deref ptr")?;
            // Aggregate pointees keep the pointer — that's the byref model's
            // value form (consumers copy at their boundary: Ret memcpys into
            // sret, stores go field-wise). Loading here handed Ret a struct
            // VALUE where it expected a pointer and it memcpy'd from garbage.
            if is_by_ref(inst.ty, lctx.ctx) {
                return Ok(Some(ptr));
            }
            let ty = lctx.ctx.get_llvm_type(inst.ty)?;
            Ok(Some(lctx.ctx.builder().load(ty, ptr, "deref")))
        }

        // ---- aggregates ----
        JirTag::FieldAccess | JirTag::ExtractValue => {
            let base = emit_inst(lctx, inst.a)?.ok_or("FieldAccess base")?;
            let base_ty = lctx.jfn.get_inst(inst.a).ty;
            if is_by_ref(base_ty, lctx.ctx) {
                let struct_ty = lctx.ctx.get_llvm_type(base_ty)?;
                let fp = lctx.ctx.builder().struct_gep(struct_ty, base, inst.b, "fa");
                if is_by_ref(inst.ty, lctx.ctx) {
                    return Ok(Some(fp));
                }
                let field_ty = lctx.ctx.get_llvm_type(inst.ty)?;
                return Ok(Some(lctx.ctx.builder().load(field_ty, fp, "field")));
            }
            Ok(Some(
                lctx.ctx.builder().extract_value(base, inst.b, "field"),
            ))
        }
        JirTag::StructLit => {
            let ty = lctx.ctx.get_llvm_type(inst.ty)?;
            let extra = inst.b;
            let count = lctx.jfn.get_extra(extra);
            if is_by_ref(inst.ty, lctx.ctx) {
                let align = lctx.ctx.type_align(inst.ty)?;
                let slot = lctx.ctx.builder().alloca(ty, align, "lit");
                for i in 0..count {
                    let fr = lctx.jfn.get_extra(extra + 1 + i);
                    let fv = emit_inst(lctx, fr)?.ok_or("StructLit field")?;
                    let fp = lctx.ctx.builder().struct_gep(ty, slot, i, "lit.f");
                    let ft = lctx.jfn.get_inst(fr).ty;
                    if is_by_ref(ft, lctx.ctx) {
                        let (fsz, fal) = (lctx.ctx.type_size(ft)?, lctx.ctx.type_align(ft)?);
                        lctx.ctx.builder().memcpy(fp, fal, fv, fal, fsz);
                    } else {
                        lctx.ctx.builder().store(fv, fp);
                    }
                }
                return Ok(Some(slot));
            }
            let mut agg = ty.undef();
            for i in 0..count {
                let fr = lctx.jfn.get_extra(extra + 1 + i);
                let fv = emit_inst(lctx, fr)?.ok_or("StructLit field")?;
                agg = lctx.ctx.builder().insert_value(agg, fv, i, "field");
            }
            Ok(Some(agg))
        }
        JirTag::ArrayLit => {
            let ty = lctx.ctx.get_llvm_type(inst.ty)?;
            let extra = inst.b;
            let count = lctx.jfn.get_extra(extra);
            if is_by_ref(inst.ty, lctx.ctx) {
                let align = lctx.ctx.type_align(inst.ty)?;
                let slot = lctx.ctx.builder().alloca(ty, align, "alit");
                let i64ty = lctx.ctx.context().i64_type();
                for i in 0..count {
                    let er = lctx.jfn.get_extra(extra + 1 + i);
                    let ev = emit_inst(lctx, er)?.ok_or("ArrayLit elem")?;
                    let idx_val = i64ty.const_int(i as u64, false);
                    let ep = lctx.ctx.builder().array_gep(ty, slot, idx_val, "alit.e");
                    let et = lctx.jfn.get_inst(er).ty;
                    if is_by_ref(et, lctx.ctx) {
                        let (esz, eal) = (lctx.ctx.type_size(et)?, lctx.ctx.type_align(et)?);
                        lctx.ctx.builder().memcpy(ep, eal, ev, eal, esz);
                    } else {
                        lctx.ctx.builder().store(ev, ep);
                    }
                }
                return Ok(Some(slot));
            }
            let mut agg = ty.undef();
            for i in 0..count {
                let er = lctx.jfn.get_extra(extra + 1 + i);
                let ev = emit_inst(lctx, er)?.ok_or("ArrayLit elem")?;
                agg = lctx.ctx.builder().insert_value(agg, ev, i, "elem");
            }
            Ok(Some(agg))
        }
        JirTag::Index => {
            let base_ty = lctx.jfn.get_inst(inst.a).ty;
            let basek = lctx.ctx.type_pool.get(base_ty).kind;
            let mut idx_val = emit_inst(lctx, inst.b)?.ok_or("Index idx")?;
            let i64ty = lctx.ctx.context().i64_type();
            if idx_val.type_of() != i64ty {
                idx_val = lctx
                    .ctx
                    .builder()
                    .int_cast(idx_val, i64ty, false, "idx.cast");
            }
            let elem_llvm = lctx.ctx.get_llvm_type(inst.ty)?;
            let elem_byref = is_by_ref(inst.ty, lctx.ctx);
            match basek {
                TypeKind::Slice => {
                    let base = emit_inst(lctx, inst.a)?.ok_or("Index base")?;
                    let ptr = lctx.ctx.builder().extract_value(base, 0, "slice.ptr");
                    let gep = lctx
                        .ctx
                        .builder()
                        .ptr_gep(elem_llvm, ptr, idx_val, "idx.gep");
                    if elem_byref {
                        return Ok(Some(gep));
                    }
                    Ok(Some(lctx.ctx.builder().load(elem_llvm, gep, "idx")))
                }
                TypeKind::PtrMany => {
                    let base = emit_inst(lctx, inst.a)?.ok_or("Index base")?;
                    let gep = lctx
                        .ctx
                        .builder()
                        .ptr_gep(elem_llvm, base, idx_val, "idx.gep");
                    if elem_byref {
                        return Ok(Some(gep));
                    }
                    Ok(Some(lctx.ctx.builder().load(elem_llvm, gep, "idx")))
                }
                _ => {
                    // Array: byref base is already a pointer; a by-value
                    // aggregate base must be spilled to a temp alloca first.
                    let base_llvm = lctx.ctx.get_llvm_type(base_ty)?;
                    let base = emit_inst(lctx, inst.a)?.ok_or("Index base")?;
                    let storage = if !is_by_ref(base_ty, lctx.ctx) {
                        let align = lctx.ctx.type_align(base_ty)?;
                        let st = lctx.ctx.builder().alloca(base_llvm, align, "arr.tmp");
                        lctx.ctx.builder().store(base, st);
                        st
                    } else {
                        base
                    };
                    let gep = lctx
                        .ctx
                        .builder()
                        .array_gep(base_llvm, storage, idx_val, "idx.gep");
                    if elem_byref {
                        return Ok(Some(gep));
                    }
                    Ok(Some(lctx.ctx.builder().load(elem_llvm, gep, "idx")))
                }
            }
        }
        JirTag::FieldAddr => {
            let base_ptr = emit_inst(lctx, inst.a)?.ok_or("FieldAddr base")?;
            let base_inst = *lctx.jfn.get_inst(inst.a);
            let alloca_like = base_inst.tag == JirTag::Alloca
                || (base_inst.tag == JirTag::Param && base_inst.flags & 1 != 0);
            let pointee_ty = if alloca_like {
                base_inst.ty
            } else {
                let k = lctx.ctx.type_pool.get(base_inst.ty);
                if k.kind != TypeKind::PtrSingle && k.kind != TypeKind::PtrMany {
                    return Err("FieldAddr base must be a pointer".into());
                }
                TypeIdx::new(k.a)
            };
            let struct_ty = lctx.ctx.get_llvm_type(pointee_ty)?;
            Ok(Some(
                lctx.ctx
                    .builder()
                    .struct_gep(struct_ty, base_ptr, inst.b, "fieldp"),
            ))
        }
        JirTag::IndexAddr => {
            let base_ptr = emit_inst(lctx, inst.a)?.ok_or("IndexAddr base")?;
            let base_inst = *lctx.jfn.get_inst(inst.a);
            let alloca_like = base_inst.tag == JirTag::Alloca
                || (base_inst.tag == JirTag::Param && base_inst.flags & 1 != 0);
            let pointee_ty = if alloca_like {
                base_inst.ty
            } else {
                let k = lctx.ctx.type_pool.get(base_inst.ty);
                if k.kind == TypeKind::PtrSingle || k.kind == TypeKind::PtrMany {
                    TypeIdx::new(k.a)
                } else {
                    base_inst.ty
                }
            };
            let pk = lctx.ctx.type_pool.get(pointee_ty).kind;
            let mut idx_val = emit_inst(lctx, inst.b)?.ok_or("IndexAddr idx")?;
            let i64ty = lctx.ctx.context().i64_type();
            if idx_val.type_of() != i64ty {
                idx_val = lctx
                    .ctx
                    .builder()
                    .int_cast(idx_val, i64ty, false, "idx.cast");
            }
            let elem_ty = TypeIdx::new(lctx.ctx.type_pool.get(inst.ty).a);
            let elem_llvm = lctx.ctx.get_llvm_type(elem_ty)?;
            if pk == TypeKind::Array {
                let arr_llvm = lctx.ctx.get_llvm_type(pointee_ty)?;
                return Ok(Some(
                    lctx.ctx
                        .builder()
                        .array_gep(arr_llvm, base_ptr, idx_val, "idx.addr"),
                ));
            }
            Ok(Some(
                lctx.ctx
                    .builder()
                    .ptr_gep(elem_llvm, base_ptr, idx_val, "idx.addr"),
            ))
        }
        JirTag::EnumPayload => {
            let enum_ptr = emit_inst(lctx, inst.a)?.ok_or("EnumPayload base")?;
            let base_ty = lctx.jfn.get_inst(inst.a).ty;
            let enum_ty = lctx.ctx.get_llvm_type(base_ty)?;
            let payload_area = lctx
                .ctx
                .builder()
                .struct_gep(enum_ty, enum_ptr, 1, "enum.payload");
            let field_ptr = if inst.b != 0 {
                let off = lctx
                    .ctx
                    .context()
                    .i64_type()
                    .const_int(inst.b as u64, false);
                let i8 = lctx.ctx.context().i8_type();
                lctx.ctx
                    .builder()
                    .ptr_gep(i8, payload_area, off, "enum.payload.off")
            } else {
                payload_area
            };
            if is_by_ref(inst.ty, lctx.ctx) {
                return Ok(Some(field_ptr));
            }
            let field_ty = lctx.ctx.get_llvm_type(inst.ty)?;
            Ok(Some(lctx.ctx.builder().load(
                field_ty,
                field_ptr,
                "enum.payload.val",
            )))
        }

        // ---- slices / strings / fills ----
        JirTag::MakeSlice => {
            let ptr_v = emit_inst(lctx, inst.a)?.ok_or("MakeSlice ptr")?;
            let len_v = emit_inst(lctx, inst.b)?.ok_or("MakeSlice len")?;
            let slice_ty = lctx.ctx.get_llvm_type(inst.ty)?;
            let mut slice = slice_ty.undef();
            slice = lctx
                .ctx
                .builder()
                .insert_value(slice, ptr_v, 0, "slice.ptr");
            slice = lctx
                .ctx
                .builder()
                .insert_value(slice, len_v, 1, "slice.len");
            Ok(Some(slice))
        }
        JirTag::MemSet => {
            let ptr = emit_inst(lctx, inst.a)?.ok_or("MemSet dst")?;
            let byte_val = lctx
                .ctx
                .context()
                .i8_type()
                .const_int(inst.flags as u64, false);
            lctx.ctx.builder().memset(ptr, byte_val, inst.b as u64, 1);
            Ok(None)
        }
        JirTag::Str => {
            let bytes = lctx.ctx.string_pool.get(StringIdx::new(inst.a)).to_vec();
            let str_const = lctx.ctx.context().const_string(&bytes, true);
            let arr_ty = lctx
                .ctx
                .context()
                .i8_type()
                .array_type(bytes.len() as u64 + 1);
            let str_global = lctx.ctx.module().add_global(arr_ty, "str");
            str_global.set_global_constant(true);
            str_global.set_initializer(str_const);
            let i8ptr = lctx.ctx.context().pointer_type(0);
            let str_ptr = lctx.ctx.builder().bitcast(str_global, i8ptr, "str_ptr");
            // Pointer decay: a pointer-typed Str yields the bare address.
            let strk = lctx.ctx.type_pool.get(inst.ty).kind;
            if strk == TypeKind::PtrMany || strk == TypeKind::PtrSingle {
                return Ok(Some(str_ptr));
            }
            let slice_ty = lctx.ctx.get_llvm_type(inst.ty)?;
            let mut slice = slice_ty.undef();
            slice = lctx
                .ctx
                .builder()
                .insert_value(slice, str_ptr, 0, "slice_ptr");
            let len = lctx
                .ctx
                .context()
                .i64_type()
                .const_int(bytes.len() as u64, false);
            slice = lctx.ctx.builder().insert_value(slice, len, 1, "slice_len");
            Ok(Some(slice))
        }

        // ---- function references / calls / drops ----
        JirTag::FnRef => {
            let name = String::from_utf8_lossy(&lctx.ctx.string_pool.get(StringIdx::new(inst.a)))
                .into_owned();
            let fn_val = fn_ref_target(lctx.ctx, &name)?.as_value();
            if lctx.ctx.type_pool.get(inst.ty).kind == TypeKind::Fn {
                return Ok(Some(fn_val)); // typed fn-pointer: already a ptr value
            }
            let ty = lctx.ctx.get_llvm_type(inst.ty)?;
            Ok(Some(lctx.ctx.builder().ptr_to_int(fn_val, ty, "fnref.u64")))
        }
        JirTag::DropBinding => {
            let binding_ptr = emit_inst(lctx, inst.a)?.ok_or("DropBinding ptr")?;
            let symbol = String::from_utf8_lossy(&lctx.ctx.string_pool.get(StringIdx::new(inst.b)))
                .into_owned();
            let f = lctx
                .ctx
                .module()
                .get_function(&symbol)
                .ok_or_else(|| format!("drop callee `{symbol}` not declared"))?;
            lctx.ctx.builder().call(f, &[binding_ptr], "");
            Ok(None)
        }
        JirTag::Call => {
            let symbol = String::from_utf8_lossy(&lctx.ctx.string_pool.get(StringIdx::new(inst.a)))
                .into_owned();
            // A wrapped `export fn` is called through its Jam-ABI body; the
            // exported symbol is the C-ABI wrapper for C callers.
            let module = lctx.ctx.module();
            let f = module
                .get_function(&format!("{symbol}{EXPORT_BODY_SUFFIX}"))
                .or_else(|| module.get_function(&symbol))
                .ok_or_else(|| format!("unknown callee `{symbol}`"))?;
            let extra = inst.b;
            let arg_count = lctx.jfn.get_extra(extra);
            let callee_uses_sret = f.uses_sret();
            let mut arg_refs = Vec::with_capacity(arg_count as usize);
            let mut args = Vec::with_capacity(arg_count as usize);
            for i in 0..arg_count {
                let ar = lctx.jfn.get_extra(extra + 1 + i);
                arg_refs.push(ar);
                args.push(emit_inst(lctx, ar)?.ok_or("call arg")?);
            }
            if f.has_marker_attr(C_ABI_ATTR) {
                return emit_c_abi_call(lctx, f, &symbol, inst.ty, &arg_refs, &args);
            }
            if f.is_var_arg() && !callee_uses_sret {
                // A variadic extern with scalar-only fixed params keeps the
                // plain prototype, but aggregate extras still cross by value
                // under C rules.
                let fixed = f.count_params() as usize;
                let mut c_args = Vec::with_capacity(args.len());
                let mut byval: Vec<(u32, TypeIdx)> = Vec::new();
                for (k, (&r, &av)) in arg_refs.iter().zip(&args).enumerate() {
                    if k < fixed {
                        c_args.push(av);
                        continue;
                    }
                    let t = lctx.jfn.get_inst(r).ty;
                    let idx = c_args.len() as u32;
                    if let Some(v) = lower_c_vararg(lctx, av, t, &mut byval, idx)? {
                        c_args.push(v);
                    }
                }
                let name = if inst.ty.is_none() { "" } else { "call" };
                let result = lctx.ctx.builder().call(f, &c_args, name);
                for (idx, pt) in byval {
                    let agg_ty = lctx.ctx.get_llvm_type(pt)?;
                    let align = lctx.ctx.type_align(pt)?;
                    result.add_call_site_byval(idx, agg_ty, align as u32);
                }
                return Ok(Some(result));
            }
            if callee_uses_sret {
                // LLVM call returns void; the result is in args[0] (the sret
                // slot) — hand that pointer back for byref consumers.
                lctx.ctx.builder().call(f, &args, "");
                return Ok(Some(args[0]));
            }
            let name = if inst.ty.is_none() { "" } else { "call" };
            Ok(Some(lctx.ctx.builder().call(f, &args, name)))
        }
        JirTag::CallIndirect => {
            // Indirect calls target the C ABI (that's what fn-typed values
            // exist for: objc_msgSend puns, dlsym'd addresses, @callC, and a
            // Jam fn's address, which is its C-ABI entry; see
            // `fn_ref_target`). The fn type is lowered by `c_signature`, the
            // same as an extern's prototype, so caller and callee agree on
            // every shape: zero-sized args are dropped, arrays pass as
            // pointers, x86-64 SysV demotes on register exhaustion.
            let callee_val = emit_inst(lctx, inst.a)?.ok_or("CallIndirect callee")?;
            let callee_ty = lctx.jfn.get_inst(inst.a).ty;
            let k = lctx.ctx.type_pool.get(callee_ty);
            if k.kind != TypeKind::Fn {
                return Err("CallIndirect callee is not of Fn type".into());
            }
            let mut fsig = JirFunction::new();
            fsig.return_type = TypeIdx::new(k.a);
            fsig.param_types = lctx.ctx.type_pool.fn_params_at(k.b).to_vec();
            fsig.param_modes = vec![ParamMode::Let; fsig.param_types.len()];
            let ret_ty = fsig.return_type;
            let sig = c_signature(&fsig, lctx.ctx)?;
            let ptr_t = lctx.ctx.context().pointer_type(0);

            let ret_is_memory = matches!(sig.ret, CParam::Value(_, CAbiClass::Memory { .. }));
            let mut llvm_params = Vec::with_capacity(sig.params.len() + 1);
            let mut sret_slot = None;
            if ret_is_memory {
                let agg_ty = lctx.ctx.get_llvm_type(ret_ty)?;
                let align = lctx.ctx.type_align(ret_ty)?;
                sret_slot = Some(lctx.ctx.builder().alloca(agg_ty, align, "ret.sret"));
                llvm_params.push(ptr_t);
            }
            for (&pt, &p) in fsig.param_types.iter().zip(&sig.params) {
                match p {
                    CParam::Pass if is_by_ref(pt, lctx.ctx) => llvm_params.push(ptr_t),
                    CParam::Pass => llvm_params.push(lctx.ctx.get_llvm_type(pt)?),
                    CParam::Skip => {}
                    CParam::Value(t, class) => {
                        llvm_params.push(c_param_llvm_ty(t, class, lctx.ctx)?)
                    }
                }
            }
            let llvm_ret = c_ret_llvm_ty(ret_ty, sig.ret, lctx.ctx)?;
            let llvm_fn_ty = llvm_ret.fn_type(&llvm_params, false);

            let extra = inst.b;
            let arg_count = lctx.jfn.get_extra(extra);
            let mut args = Vec::with_capacity(arg_count as usize + 1);
            // (LLVM arg index, jam type) of byval args, and (LLVM index,
            // attribute) of integer-extension args: attributed after the call
            // is built.
            let mut byval_args: Vec<(u32, TypeIdx)> = Vec::new();
            let mut ext_args: Vec<(u32, &'static str)> = Vec::new();
            if let Some(slot) = sret_slot {
                args.push(slot);
            }
            for i in 0..arg_count {
                let ar = lctx.jfn.get_extra(extra + 1 + i);
                let av = emit_inst(lctx, ar)?.ok_or("CallIndirect arg")?;
                let pt = fsig.param_types.get(i as usize).copied();
                match (pt, sig.params.get(i as usize)) {
                    (Some(pt), Some(CParam::Pass)) => {
                        if let Some(ext) = c_int_ext(pt, lctx.ctx) {
                            ext_args.push((args.len() as u32, ext));
                        }
                        args.push(av);
                    }
                    (_, Some(CParam::Skip)) => {}
                    (_, Some(&CParam::Value(t, class))) => {
                        if matches!(class, CAbiClass::Memory { byval: true }) {
                            byval_args.push((args.len() as u32, t));
                        }
                        let lowered = lower_c_arg(lctx, av, t, class)?;
                        args.push(lowered);
                    }
                    _ => args.push(av),
                }
            }
            let returns_value = matches!(sig.ret, CParam::Value(_, c) if !matches!(c, CAbiClass::Memory { .. }))
                || (matches!(sig.ret, CParam::Pass) && !ret_ty.is_none());
            let name = if returns_value { "call.indirect" } else { "" };
            let result = lctx
                .ctx
                .builder()
                .indirect_call(llvm_fn_ty, callee_val, &args, name);
            for (idx, pt) in byval_args {
                let agg_ty = lctx.ctx.get_llvm_type(pt)?;
                let align = lctx.ctx.type_align(pt)?;
                result.add_call_site_byval(idx, agg_ty, align as u32);
            }
            for (idx, ext) in ext_args {
                result.add_call_site_ext(idx, ext);
            }
            // A byref return (aggregate or array) hands its consumer a
            // pointer to storage holding the value.
            let agg_slot =
                |lctx: &mut JirCodegenCtx<'_, 'ctx>, name: &str| -> Result<Value<'ctx>, String> {
                    let agg_ty = lctx.ctx.get_llvm_type(ret_ty)?;
                    let align = lctx.ctx.type_align(ret_ty)?;
                    Ok(lctx.ctx.builder().alloca(agg_ty, align, name))
                };
            match sig.ret {
                CParam::Value(rt, CAbiClass::Memory { .. }) => {
                    let agg_ty = lctx.ctx.get_llvm_type(rt)?;
                    let align = lctx.ctx.type_align(rt)?;
                    result.add_call_site_sret(0, agg_ty, align as u32);
                    Ok(Some(
                        sret_slot.expect("sret slot exists for Memory returns"),
                    ))
                }
                CParam::Value(rt, CAbiClass::Coerce { w0, w1 }) => {
                    // Spill the words to a word-sized temp, then copy only
                    // the value's bytes into its storage.
                    let ct = c_coerce_ty(lctx.ctx, w0, w1);
                    let tmp_align = coerce_tmp_align(rt, lctx.ctx)?;
                    let tmp = lctx.ctx.builder().alloca(ct, tmp_align, "ret.coerce");
                    lctx.ctx.builder().store(result, tmp);
                    let slot = agg_slot(lctx, "ret.val")?;
                    let size = lctx.ctx.type_size(rt)?;
                    let align = lctx.ctx.type_align(rt)?;
                    lctx.ctx.builder().memcpy(slot, align, tmp, tmp_align, size);
                    Ok(Some(slot))
                }
                CParam::Value(_, CAbiClass::Hfa | CAbiClass::Direct) => {
                    let slot = agg_slot(lctx, "ret.byval")?;
                    lctx.ctx.builder().store(result, slot);
                    Ok(Some(slot))
                }
                // Zero-sized: nothing came back; the consumer still wants
                // storage to point at.
                CParam::Skip => Ok(Some(agg_slot(lctx, "ret.empty")?)),
                CParam::Pass => Ok(Some(result)),
            }
        }
        JirTag::Invalid => Err("jir_codegen: Invalid tag in instruction stream".into()),
    }
}

fn icmp<'ctx>(
    lctx: &mut JirCodegenCtx<'_, 'ctx>,
    inst: &JirInst,
    pred: IntPredicate,
    name: &str,
) -> Result<Option<Value<'ctx>>, String> {
    let (a, b) = binop_operands(lctx, inst)?;
    Ok(Some(lctx.ctx.builder().icmp(pred, a, b, name)))
}

fn fcmp<'ctx>(
    lctx: &mut JirCodegenCtx<'_, 'ctx>,
    inst: &JirInst,
    pred: RealPredicate,
    name: &str,
) -> Result<Option<Value<'ctx>>, String> {
    let (a, b) = binop_operands(lctx, inst)?;
    Ok(Some(lctx.ctx.builder().fcmp(pred, a, b, name)))
}

fn cast<'ctx, F>(
    lctx: &mut JirCodegenCtx<'_, 'ctx>,
    inst: &JirInst,
    what: &str,
    build: F,
) -> Result<Option<Value<'ctx>>, String>
where
    F: FnOnce(&jam_llvm::Builder<'ctx>, Value<'ctx>, jam_llvm::Type<'ctx>) -> Value<'ctx>,
{
    let v = emit_inst(lctx, inst.a)?.ok_or_else(|| format!("{what} operand"))?;
    let ty = lctx.ctx.get_llvm_type(inst.ty)?;
    Ok(Some(build(lctx.ctx.builder(), v, ty)))
}

fn emit_switch<'ctx>(
    lctx: &mut JirCodegenCtx<'_, 'ctx>,
    inst: &JirInst,
) -> Result<Option<Value<'ctx>>, String> {
    let scrut = emit_inst(lctx, inst.a)?.ok_or("Switch scrut")?;
    let scrut_ty = lctx.jfn.get_inst(inst.a).ty;
    let scrut_llvm = lctx.ctx.get_llvm_type(scrut_ty)?;
    let extra = inst.b;
    let default_b = lctx.jfn.get_extra(extra);
    let case_count = lctx.jfn.get_extra(extra + 1);
    let default_bb = *lctx
        .block_map
        .get(&default_b)
        .ok_or("Switch default block")?;

    let read_case = |i: u32| -> Result<(Value<'ctx>, BasicBlock<'ctx>), String> {
        let base = extra + 2 + i * 4;
        let lo = lctx.jfn.get_extra(base) as u64;
        let hi = lctx.jfn.get_extra(base + 1) as u64;
        let signed = lctx.jfn.get_extra(base + 2) != 0;
        let case_b = lctx.jfn.get_extra(base + 3);
        let bits = lo | (hi << 32);
        let case_val = scrut_llvm.const_int(bits, signed);
        let case_bb = *lctx.block_map.get(&case_b).ok_or("Switch case block")?;
        Ok((case_val, case_bb))
    };

    // 1 case + default collapses to icmp + cond_br.
    if case_count == 1 {
        let (case_val, case_bb) = read_case(0)?;
        let cmp = lctx
            .ctx
            .builder()
            .icmp(IntPredicate::Eq, scrut, case_val, "match.eq");
        lctx.ctx.builder().cond_br(cmp, case_bb, default_bb);
        return Ok(None);
    }
    let sw = lctx.ctx.builder().switch(scrut, default_bb, case_count);
    for i in 0..case_count {
        let (case_val, case_bb) = read_case(i)?;
        sw.add_case(case_val, case_bb);
    }
    Ok(None)
}

fn emit_ret<'ctx>(
    lctx: &mut JirCodegenCtx<'_, 'ctx>,
    inst: &JirInst,
) -> Result<Option<Value<'ctx>>, String> {
    if jir_return_is_sret(lctx.jfn, lctx.ctx) {
        let f = body_fn(lctx.ctx, &lctx.jfn.name).ok_or("Ret: function missing")?;
        let sret_slot = f.param(0);
        if inst.a != NO_JIR_REF {
            let vty = lctx.jfn.get_inst(inst.a).ty;
            if is_by_ref(vty, lctx.ctx) {
                let src = emit_inst(lctx, inst.a)?.ok_or("Ret src")?;
                let size = lctx.ctx.type_size(vty)?;
                let align = lctx.ctx.type_align(vty)?;
                lctx.ctx
                    .builder()
                    .memcpy(sret_slot, align, src, align, size);
            } else {
                let v = emit_inst(lctx, inst.a)?.ok_or("Ret value")?;
                lctx.ctx.builder().store(v, sret_slot);
            }
        }
        lctx.ctx.builder().ret_void();
        return Ok(None);
    }
    if inst.a == NO_JIR_REF {
        lctx.ctx.builder().ret_void();
    } else {
        let v = emit_inst(lctx, inst.a)?.ok_or("Ret value")?;
        lctx.ctx.builder().ret(v);
    }
    Ok(None)
}

/// Mark blocks reachable from entry (block 1) by walking terminator successors.
/// Unreachable blocks (orphaned by constant-folded branches) are skipped in
/// codegen — frontend DCE that keeps their instructions (incl. extern calls)
/// out of the LLVM module.
fn compute_reachable_blocks(jfn: &JirFunction) -> Vec<bool> {
    let mut reachable = vec![false; jfn.blocks.len()];
    if jfn.blocks.len() <= 1 {
        return reachable;
    }
    reachable[1] = true;
    let mut stack = vec![1u32];
    while let Some(b) = stack.pop() {
        let blk = jfn.get_block(b);
        if blk.insts.is_empty() {
            continue;
        }
        let last = *jfn.get_inst(*blk.insts.last().unwrap());
        let mut visit = |t: u32| {
            if t >= 1 && (t as usize) < jfn.blocks.len() && !reachable[t as usize] {
                reachable[t as usize] = true;
                stack.push(t);
            }
        };
        match last.tag {
            JirTag::Br => visit(last.a),
            JirTag::CondBr => {
                let ex = last.b;
                if ex as usize + 2 <= jfn.extra.len() {
                    visit(jfn.get_extra(ex));
                    visit(jfn.get_extra(ex + 1));
                }
            }
            JirTag::Switch => {
                let ex = last.b;
                if ex as usize + 2 <= jfn.extra.len() {
                    visit(jfn.get_extra(ex)); // default
                    let case_count = jfn.get_extra(ex + 1);
                    for i in 0..case_count {
                        let slot = ex + 2 + i * 4 + 3;
                        if (slot as usize) < jfn.extra.len() {
                            visit(jfn.get_extra(slot));
                        }
                    }
                }
            }
            _ => {} // Ret / Unreachable: no successors
        }
    }
    reachable
}

/// Lower a function body to LLVM IR. The prototype must already exist
/// ([`jir_declare_prototype`]). `extern` functions have no body.
pub fn jir_define_body<'ctx>(jfn: &JirFunction, ctx: &CodegenContext<'ctx>) -> Result<(), String> {
    if jfn.is_extern {
        return Ok(());
    }
    let f = body_fn(ctx, &jfn.name)
        .ok_or_else(|| format!("jir_define_body: prototype missing for `{}`", jfn.name))?;

    let mut lctx = JirCodegenCtx {
        jfn,
        ctx,
        value_map: HashMap::new(),
        block_map: HashMap::new(),
    };

    let reachable = compute_reachable_blocks(jfn);

    // Create LLVM blocks first so terminators resolve forward references; skip
    // the sentinel (0) and dead blocks.
    for (b, &live) in reachable.iter().enumerate().skip(1) {
        if !live {
            continue;
        }
        let bb = f.append_basic_block(&jfn.get_block(b as JirBlockRef).name);
        lctx.block_map.insert(b as JirBlockRef, bb);
    }

    // Emit instructions block-by-block.
    for (b, &live) in reachable.iter().enumerate().skip(1) {
        if !live {
            continue;
        }
        let bb = lctx.block_map[&(b as JirBlockRef)];
        ctx.builder().position_at_end(bb);
        let insts = jfn.get_block(b as JirBlockRef).insts.clone();
        for r in insts {
            let v = emit_inst(&mut lctx, r)?;
            if let Some(val) = v {
                lctx.value_map.insert(r, val);
            }
        }
    }
    let wrapped = format!("{}{EXPORT_BODY_SUFFIX}", jfn.name);
    if jfn.is_export && ctx.module().get_function(&wrapped).is_some() {
        emit_export_wrapper(jfn, ctx)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jir::{JirFunction, JirInst};
    use jam_core::index::TypeIdx;
    use jam_llvm::Context;

    #[test]
    fn declare_simple_function_prototype() {
        let ctx = Context::new();
        let cg = CodegenContext::new(&ctx, "m");
        let mut jfn = JirFunction::new();
        jfn.name = "add".into();
        jfn.param_types = vec![builtin::I32, builtin::I32];
        jfn.param_modes = vec![ParamMode::Let, ParamMode::Let];
        jfn.return_type = builtin::I32;
        let f = jir_declare_prototype(&jfn, &cg).unwrap();
        assert_eq!(f.count_params(), 2);
        assert!(f.return_type().is_integer());
        // NB: a bodyless *internal*-linkage function does NOT verify yet — LLVM
        // requires internal functions to have a definition; the body is added
        // by jir_define_body. (An `extern` decl with external linkage would.)
    }

    #[test]
    fn mut_param_lowers_to_pointer() {
        let ctx = Context::new();
        let cg = CodegenContext::new(&ctx, "m");
        let mut jfn = JirFunction::new();
        jfn.name = "bump".into();
        jfn.param_types = vec![builtin::I32];
        jfn.param_modes = vec![ParamMode::Mut];
        jfn.return_type = TypeIdx::NONE; // void
        let f = jir_declare_prototype(&jfn, &cg).unwrap();
        assert_eq!(f.count_params(), 1);
        assert!(f.param(0).type_of().is_pointer()); // mut -> ByPointer
        assert!(f.return_type().is_void());
    }

    #[test]
    fn extern_declaration_verifies() {
        let ctx = Context::new();
        let cg = CodegenContext::new(&ctx, "m");
        let mut jfn = JirFunction::new();
        jfn.name = "putchar".into();
        jfn.param_types = vec![builtin::I32];
        jfn.return_type = builtin::I32;
        jfn.is_extern = true;
        let f = jir_declare_prototype(&jfn, &cg).unwrap();
        // extern -> external linkage; a bodyless external declaration verifies.
        assert!(f.verify());
        assert_eq!(f.count_params(), 1);
    }

    /// Register a struct of `n` i64 fields named `name`; returns its type.
    fn i64_struct<'c>(ctx: &'c Context, cg: &CodegenContext<'c>, name: &str, n: usize) -> TypeIdx {
        let nid = cg.string_pool.intern(name.as_bytes());
        let ty = cg.type_pool.intern_struct(nid);
        let fields = (0..n).map(|i| (format!("f{i}"), builtin::I64)).collect();
        cg.register_struct(name, ctx.named_struct(name), fields);
        ty
    }

    #[test]
    fn scalar_extern_keeps_plain_lowering() {
        let ctx = Context::new();
        let cg = CodegenContext::new(&ctx, "m");
        let mut jfn = JirFunction::new();
        jfn.name = "putchar".into();
        jfn.param_types = vec![builtin::I32];
        jfn.return_type = builtin::I32;
        jfn.is_extern = true;
        let f = jir_declare_prototype(&jfn, &cg).unwrap();
        assert!(!f.has_marker_attr(C_ABI_ATTR));
    }

    #[test]
    fn extern_memory_class_return_is_sret() {
        // extern fn mk() Big  (32 bytes): C returns it through an sret slot.
        let ctx = Context::new();
        let cg = CodegenContext::new(&ctx, "m");
        let big = i64_struct(&ctx, &cg, "Big", 4);
        let mut jfn = JirFunction::new();
        jfn.name = "mk".into();
        jfn.return_type = big;
        jfn.is_extern = true;
        let f = jir_declare_prototype(&jfn, &cg).unwrap();
        assert!(f.has_marker_attr(C_ABI_ATTR));
        assert!(f.uses_sret(), "sret attribute routes the slot to x8 / rdi");
        assert_eq!(f.count_params(), 1);
        assert!(f.return_type().is_void());
        assert!(f.verify());
    }

    #[test]
    fn extern_two_word_struct_arg_matches_clang() {
        // extern fn sum(l: L2) i64: clang passes L2 as `[2 x i64]` on
        // AArch64 (consecutive x registers or all on the stack) and as an
        // eightbyte pair on x86-64.
        let ctx = Context::new();
        let cg = CodegenContext::new(&ctx, "m");
        let l2 = i64_struct(&ctx, &cg, "L2", 2);
        let mut jfn = JirFunction::new();
        jfn.name = "sum".into();
        jfn.param_types = vec![l2];
        jfn.return_type = builtin::I64;
        jfn.is_extern = true;
        let f = jir_declare_prototype(&jfn, &cg).unwrap();
        assert_eq!(f.count_params(), 1);
        let pt = f.param(0).type_of();
        if cg.target_arch() == crate::target::Arch::X86_64 {
            assert!(pt.is_struct());
        } else {
            assert!(
                pt.is_array(),
                "AArch64 two-word aggregate must be [2 x i64]"
            );
        }
        let sig = cg.c_sig("sum").expect("signature recorded");
        assert!(matches!(sig.params[0], CParam::Value(t, _) if t == l2));
    }

    #[test]
    fn export_with_struct_gets_c_wrapper_over_jam_body() {
        // export fn mkPair(a: i64) Pair: the exported symbol is the C-ABI
        // wrapper; the Jam-ABI body lives under the suffixed name.
        let ctx = Context::new();
        let cg = CodegenContext::new(&ctx, "m");
        let pair = i64_struct(&ctx, &cg, "Pair", 2);
        let mut jfn = JirFunction::new();
        jfn.name = "mkPair".into();
        jfn.param_types = vec![builtin::I64];
        jfn.return_type = pair;
        jfn.is_export = true;
        let body = jir_declare_prototype(&jfn, &cg).unwrap();
        let wrapper = cg.module().get_function("mkPair").expect("wrapper");
        assert!(wrapper.has_marker_attr(C_ABI_ATTR));
        assert!(!wrapper.uses_sret(), "16 bytes return in registers under C");
        let body_name = format!("mkPair{EXPORT_BODY_SUFFIX}");
        assert!(cg.module().get_function(&body_name).is_some());
        assert!(!body.has_marker_attr(C_ABI_ATTR));
        assert!(body.uses_sret(), "the body keeps Jam's sret return");
        assert_eq!(llvm_body_name(&jfn, &cg), body_name);
    }

    #[test]
    fn lower_arithmetic_body_emits_real_ir() {
        // fn five() i32 { return 2 + 3 }
        let ctx = Context::new();
        let cg = CodegenContext::new(&ctx, "m");
        let mut jfn = JirFunction::new();
        jfn.name = "five".into();
        jfn.return_type = builtin::I32;
        let entry = jfn.push_block("entry");
        let i2 = jfn.push_inst(JirInst {
            tag: JirTag::Int,
            a: 2,
            ty: builtin::I32,
            ..Default::default()
        });
        let i3 = jfn.push_inst(JirInst {
            tag: JirTag::Int,
            a: 3,
            ty: builtin::I32,
            ..Default::default()
        });
        let add = jfn.push_inst(JirInst {
            tag: JirTag::Add,
            a: i2,
            b: i3,
            ty: builtin::I32,
            ..Default::default()
        });
        let ret = jfn.push_inst(JirInst {
            tag: JirTag::Ret,
            a: add,
            ..Default::default()
        });
        jfn.get_block_mut(entry).insts.extend([i2, i3, add, ret]);

        jir_declare_prototype(&jfn, &cg).unwrap();
        jir_define_body(&jfn, &cg).unwrap();
        let f = cg.module().get_function("five").unwrap();
        // An internal-linkage function WITH a body now verifies.
        assert!(f.verify());
    }

    #[test]
    fn lower_local_variable_body() {
        // fn f() i32 { var x: i32; x = 7; return x }  (Alloca, Store, Load, Ret)
        let ctx = Context::new();
        let cg = CodegenContext::new(&ctx, "m");
        let mut jfn = JirFunction::new();
        jfn.name = "f".into();
        jfn.return_type = builtin::I32;
        let entry = jfn.push_block("entry");
        let slot = jfn.push_inst(JirInst {
            tag: JirTag::Alloca,
            ty: builtin::I32,
            ..Default::default()
        });
        let seven = jfn.push_inst(JirInst {
            tag: JirTag::Int,
            a: 7,
            ty: builtin::I32,
            ..Default::default()
        });
        let store = jfn.push_inst(JirInst {
            tag: JirTag::Store,
            a: slot,
            b: seven,
            ..Default::default()
        });
        let load = jfn.push_inst(JirInst {
            tag: JirTag::Load,
            a: slot,
            ty: builtin::I32,
            ..Default::default()
        });
        let ret = jfn.push_inst(JirInst {
            tag: JirTag::Ret,
            a: load,
            ..Default::default()
        });
        jfn.get_block_mut(entry)
            .insts
            .extend([slot, seven, store, load, ret]);

        jir_declare_prototype(&jfn, &cg).unwrap();
        jir_define_body(&jfn, &cg).unwrap();
        assert!(cg.module().get_function("f").unwrap().verify());
    }

    #[test]
    fn lower_comparison_body() {
        // fn lt() bool { return 2 < 3 }
        let ctx = Context::new();
        let cg = CodegenContext::new(&ctx, "m");
        let mut jfn = JirFunction::new();
        jfn.name = "lt".into();
        jfn.return_type = builtin::BOOL;
        let entry = jfn.push_block("entry");
        let i2 = jfn.push_inst(JirInst {
            tag: JirTag::Int,
            a: 2,
            ty: builtin::I32,
            ..Default::default()
        });
        let i3 = jfn.push_inst(JirInst {
            tag: JirTag::Int,
            a: 3,
            ty: builtin::I32,
            ..Default::default()
        });
        let lt = jfn.push_inst(JirInst {
            tag: JirTag::ICmpSlt,
            a: i2,
            b: i3,
            ty: builtin::BOOL,
            ..Default::default()
        });
        let ret = jfn.push_inst(JirInst {
            tag: JirTag::Ret,
            a: lt,
            ..Default::default()
        });
        jfn.get_block_mut(entry).insts.extend([i2, i3, lt, ret]);

        jir_declare_prototype(&jfn, &cg).unwrap();
        jir_define_body(&jfn, &cg).unwrap();
        let f = cg.module().get_function("lt").unwrap();
        assert!(f.verify());
    }

    #[test]
    fn struct_return_uses_sret() {
        let ctx = Context::new();
        let cg = CodegenContext::new(&ctx, "m");
        let nid = cg.string_pool.intern(b"Pair");
        let pair = cg.type_pool.intern_struct(nid);
        let named = ctx.named_struct("Pair");
        cg.register_struct(
            "Pair",
            named,
            vec![("a".into(), builtin::I64), ("b".into(), builtin::I64)],
        );
        let mut jfn = JirFunction::new();
        jfn.name = "makePair".into();
        jfn.return_type = pair;
        let f = jir_declare_prototype(&jfn, &cg).unwrap();
        // sret: a leading ptr param + a void return.
        assert_eq!(f.count_params(), 1);
        assert!(f.param(0).type_of().is_pointer());
        assert!(f.return_type().is_void());
    }
}
