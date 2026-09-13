//! Straight-line lowering: a function's body becomes one block of a graph.
//!
//! Parameters are read from `.input` at offsets packed the way the marshaller
//! writes them; locals live in frame cells; the value the function evaluates to
//! is left on the stack, which the caller reads once the machine halts.

use std::collections::HashMap;

use quote::ToTokens;
use safetynet_core::ir::{BlockBody, CellId, Cfg, Frame, Terminator};
use safetynet_core::isa::{
    Add, And, CmpEq, CmpLe, CmpLt, CmpSLe, CmpSLt, Div, Ld8, Ld32, Ld64, Mul, Or, Push8, Push32,
    Push64, Rem, SDiv, SRem, Sar, Shl, Shr, Sub, Xor,
};
use safetynet_core::{Instr, Region, Width};

use super::ty::Scalar;

/// A function lowered to its graph, with what the wrapper needs to marshal for
/// it and the binding types the call site must still prove are `VmValue`.
#[derive(Debug)]
pub(crate) struct Lowered {
    pub(crate) cfg: Cfg,
    pub(crate) bindings: Vec<syn::Type>,
    /// Each parameter's byte offset in `.input`, in signature order.
    pub(crate) param_offsets: Vec<u32>,
    /// Bytes the parameters occupy: the size of the `.input` region.
    pub(crate) input_size: u32,
    /// The return type, read back from the top of the stack.
    pub(crate) ret: syn::Type,
}

/// Where a name in scope reads from.
#[derive(Clone, Copy)]
enum Binding {
    /// A parameter, at a byte offset in `.input`.
    Param { offset: u32, ty: Scalar },
    /// A local, in a frame cell.
    Local { cell: CellId, ty: Scalar },
}

/// Lowers one function to its graph.
pub(crate) fn lower(func: &syn::ItemFn) -> syn::Result<Lowered> {
    reject_odd_signature(func)?;
    let ret_ty = return_type(func)
        .ok_or_else(|| err(&func.sig.ident, "the function must return a scalar"))?;
    let ret = Scalar::of(ret_ty).ok_or_else(|| {
        err(
            ret_ty,
            "the return type must be a scalar the machine can hold",
        )
    })?;

    // Parameters first: their offsets in `.input`, and the scope they seed.
    let mut bindings = Vec::new();
    let mut param_offsets = Vec::new();
    let mut scope: HashMap<String, Binding> = HashMap::new();
    let mut offset = 0u32;
    for arg in &func.sig.inputs {
        let (name, ty_node) = param(arg)?;
        let scalar = Scalar::of(ty_node)
            .ok_or_else(|| err(ty_node, "a parameter must be a scalar the machine can hold"))?;
        offset = offset.next_multiple_of(scalar.align());
        param_offsets.push(offset);
        scope.insert(name, Binding::Param { offset, ty: scalar });
        offset += scalar.size();
        bindings.push(ty_node.clone());
    }
    let input_size = offset;
    bindings.push(ret_ty.clone());

    // One cell per local, in declaration order, before anything is lowered.
    let mut frame = Frame::new();
    let mut cells = Vec::new();
    for stmt in &func.block.stmts {
        if let syn::Stmt::Local(local) = stmt {
            let (name, scalar, ty_node) = declared_local(local)?;
            let cell = frame
                .add(scalar.width)
                .ok_or_else(|| err(&local.pat, "the frame is too large"))?;
            cells.push((name, cell, scalar));
            bindings.push(ty_node);
        }
    }

    // The body: append into the single block, resolving names as they go live.
    let mut builder = Cfg::builder(frame);
    let entry = builder.block(0);
    let body = builder
        .at(entry)
        .map_err(|error| err(&func.sig.ident, &format!("cannot open the block: {error}")))?;

    let stmts = &func.block.stmts;
    let mut cells = cells.into_iter();
    for (index, stmt) in stmts.iter().enumerate() {
        let last = index + 1 == stmts.len();
        match stmt {
            syn::Stmt::Local(local) => {
                let (name, cell, scalar) = cells
                    .next()
                    .ok_or_else(|| err(&local.pat, "a local without a cell"))?;
                let init = &local
                    .init
                    .as_ref()
                    .ok_or_else(|| err(&local.pat, "a local needs an initializer"))?
                    .expr;
                lower_expr(body, &scope, init, Some(scalar))?;
                body.store(cell);
                scope.insert(name, Binding::Local { cell, ty: scalar });
            }
            syn::Stmt::Expr(syn::Expr::Return(ret_expr), _) if last => {
                let value = ret_expr
                    .expr
                    .as_ref()
                    .ok_or_else(|| err(ret_expr, "the function must return a value"))?;
                lower_expr(body, &scope, value, Some(ret))?;
            }
            syn::Stmt::Expr(expr, None) if last => {
                lower_expr(body, &scope, expr, Some(ret))?;
            }
            other => {
                return Err(err(
                    other,
                    "only `let` bindings and a final expression are supported yet",
                ));
            }
        }
    }

    builder
        .seal(entry, Terminator::Halt)
        .map_err(|error| err(&func.sig.ident, &format!("cannot seal the block: {error}")))?;
    let cfg = builder
        .build(entry)
        .map_err(|error| err(&func.sig.ident, &format!("cannot build the graph: {error}")))?;
    Ok(Lowered {
        cfg,
        bindings,
        param_offsets,
        input_size,
        ret: ret_ty.clone(),
    })
}

/// Lowers an expression, leaving its value on top of the stack.
///
/// `expected` is the type a bare literal takes, so `1` in `x + 1` is the width
/// of `x` rather than a guess.
fn lower_expr(
    body: &mut BlockBody,
    scope: &HashMap<String, Binding>,
    expr: &syn::Expr,
    expected: Option<Scalar>,
) -> syn::Result<Scalar> {
    match expr {
        syn::Expr::Lit(lit) => lower_lit(body, lit, expected),
        syn::Expr::Path(path) => lower_path(body, scope, path),
        syn::Expr::Paren(paren) => lower_expr(body, scope, &paren.expr, expected),
        syn::Expr::Binary(binary) => lower_binary(body, scope, binary, expected),
        other => Err(err(other, "this expression is not supported yet")),
    }
}

/// Pushes a literal; its type is the context's, or `i32` with nothing to go on.
fn lower_lit(
    body: &mut BlockBody,
    lit: &syn::ExprLit,
    expected: Option<Scalar>,
) -> syn::Result<Scalar> {
    match &lit.lit {
        syn::Lit::Int(int) => {
            push_word(body, int.base10_parse()?);
            Ok(expected.unwrap_or(Scalar {
                width: Width::U32,
                signed: true,
            }))
        }
        syn::Lit::Bool(boolean) => {
            push_word(body, boolean.value.into());
            Ok(Scalar::BOOL)
        }
        other => Err(err(other, "only integer and bool literals are supported")),
    }
}

/// Reads a parameter from `.input` or a local from its cell.
fn lower_path(
    body: &mut BlockBody,
    scope: &HashMap<String, Binding>,
    path: &syn::ExprPath,
) -> syn::Result<Scalar> {
    let name = path
        .path
        .get_ident()
        .ok_or_else(|| err(path, "expected a parameter or local"))?;
    match scope.get(&name.to_string()) {
        Some(Binding::Param { offset, ty }) => {
            body.base(Region::Input);
            if *offset != 0 {
                push_word(body, u64::from(*offset));
                body.instr(Add);
            }
            body.instr(load(ty.width));
            Ok(*ty)
        }
        Some(Binding::Local { cell, ty }) => {
            body.load(*cell);
            Ok(*ty)
        }
        None => Err(err(name, "no such parameter or local")),
    }
}

/// Lowers a binary operation and the arithmetic on top of it.
fn lower_binary(
    body: &mut BlockBody,
    scope: &HashMap<String, Binding>,
    binary: &syn::ExprBinary,
    expected: Option<Scalar>,
) -> syn::Result<Scalar> {
    // `>` and `>=` are the mirror of `<` and `<=`, so they lower their operands
    // in the other order and reuse the same opcode.
    let swap = matches!(binary.op, syn::BinOp::Gt(_) | syn::BinOp::Ge(_));
    let (first, second) = if swap {
        (&*binary.right, &*binary.left)
    } else {
        (&*binary.left, &*binary.right)
    };
    let left = lower_expr(body, scope, first, expected)?;
    let right = lower_expr(body, scope, second, Some(left))?;
    if left.width != right.width {
        return Err(err(binary, "the operands must be the same width"));
    }
    emit_op(body, binary, left)
}

/// Emits the opcode a binary operator lowers to, and reports its result type.
fn emit_op(body: &mut BlockBody, binary: &syn::ExprBinary, ty: Scalar) -> syn::Result<Scalar> {
    use syn::BinOp;
    match binary.op {
        BinOp::Add(_) => body.instr(Add),
        BinOp::Sub(_) => body.instr(Sub),
        BinOp::Mul(_) => body.instr(Mul),
        BinOp::Div(_) => body.instr(if ty.signed {
            Instr::from(SDiv)
        } else {
            Div.into()
        }),
        BinOp::Rem(_) => body.instr(if ty.signed {
            Instr::from(SRem)
        } else {
            Rem.into()
        }),
        BinOp::BitAnd(_) => body.instr(And),
        BinOp::BitOr(_) => body.instr(Or),
        BinOp::BitXor(_) => body.instr(Xor),
        BinOp::Shl(_) => body.instr(Shl),
        BinOp::Shr(_) => body.instr(if ty.signed {
            Instr::from(Sar)
        } else {
            Shr.into()
        }),
        BinOp::Eq(_) => return cmp(body, CmpEq.into()),
        BinOp::Lt(_) | BinOp::Gt(_) => {
            return cmp(
                body,
                if ty.signed {
                    CmpSLt.into()
                } else {
                    CmpLt.into()
                },
            );
        }
        BinOp::Le(_) | BinOp::Ge(_) => {
            return cmp(
                body,
                if ty.signed {
                    CmpSLe.into()
                } else {
                    CmpLe.into()
                },
            );
        }
        BinOp::Ne(_) => {
            body.instr(CmpEq);
            // Negate the flag: it equals zero exactly when the values differed.
            push_word(body, 0);
            body.instr(CmpEq);
            return Ok(Scalar::BOOL);
        }
        _ => return Err(err(binary, "this operator is not supported yet")),
    };
    Ok(ty)
}

/// Emits a comparison, which always yields a boolean.
fn cmp(body: &mut BlockBody, op: Instr) -> syn::Result<Scalar> {
    body.instr(op);
    Ok(Scalar::BOOL)
}

/// Pushes a word with the narrowest immediate that holds it.
fn push_word(body: &mut BlockBody, value: u64) {
    if let Ok(byte) = u8::try_from(value) {
        body.instr(Push8 { imm: byte });
    } else if let Ok(word) = u32::try_from(value) {
        body.instr(Push32 { imm: word });
    } else {
        body.instr(Push64 { imm: value });
    }
}

/// The load opcode for a width.
fn load(width: Width) -> Instr {
    match width {
        Width::U8 => Ld8.into(),
        Width::U32 => Ld32.into(),
        Width::U64 => Ld64.into(),
    }
}

/// The name and scalar of a parameter, refusing `self` and pattern arguments.
fn param(arg: &syn::FnArg) -> syn::Result<(String, &syn::Type)> {
    let typed = match arg {
        syn::FnArg::Typed(typed) => typed,
        syn::FnArg::Receiver(receiver) => {
            return Err(err(receiver, "a method receiver is not supported"));
        }
    };
    match &*typed.pat {
        syn::Pat::Ident(pat) if pat.subpat.is_none() => Ok((pat.ident.to_string(), &typed.ty)),
        other => Err(err(other, "a parameter must be a plain name")),
    }
}

/// The name, scalar and type node of a `let`, requiring an explicit type.
fn declared_local(local: &syn::Local) -> syn::Result<(String, Scalar, syn::Type)> {
    let typed = match &local.pat {
        syn::Pat::Type(typed) => typed,
        other => return Err(err(other, "a local needs a type: `let x: u32 = ...`")),
    };
    let name = match &*typed.pat {
        syn::Pat::Ident(pat) if pat.subpat.is_none() => pat.ident.to_string(),
        other => return Err(err(other, "a local must be a plain name")),
    };
    let scalar = Scalar::of(&typed.ty)
        .ok_or_else(|| err(&typed.ty, "a local must be a scalar the machine can hold"))?;
    Ok((name, scalar, (*typed.ty).clone()))
}

/// The function's return type, or `None` when it returns nothing.
fn return_type(func: &syn::ItemFn) -> Option<&syn::Type> {
    match &func.sig.output {
        syn::ReturnType::Type(_, ty) => Some(ty),
        syn::ReturnType::Default => None,
    }
}

/// Refuses the shapes the lowerer does not model.
fn reject_odd_signature(func: &syn::ItemFn) -> syn::Result<()> {
    let sig = &func.sig;
    if let Some(token) = &sig.asyncness {
        return Err(err(token, "an async function is not supported"));
    }
    if let Some(token) = &sig.variadic {
        return Err(err(token, "a variadic function is not supported"));
    }
    if !sig.generics.params.is_empty() || sig.generics.where_clause.is_some() {
        return Err(err(&sig.generics, "generics are not supported"));
    }
    Ok(())
}

/// A refusal pointed at the offending syntax.
fn err<T: ToTokens>(node: T, message: &str) -> syn::Error {
    syn::Error::new_spanned(node, message)
}
