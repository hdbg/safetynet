//! Lowering a function's body to a graph.
//!
//! Parameters are read from `.input` at offsets packed the way the marshaller
//! writes them; locals live in frame cells; the value the function evaluates to
//! is left on the stack, which the caller reads once the machine halts.
//!
//! Structured control flow becomes blocks and edges. Each construct is lowered
//! into a fresh block, and the lowerer tracks whether a path *falls through* to
//! the next block or *diverges* by returning — so a branch whose arms both
//! return leaves no join behind, which the reachability check would reject.

use std::collections::HashMap;

use proc_macro2::Span;
use quote::ToTokens;
use safetynet_core::ir::{BlockId, Cfg, Frame, Terminator};
use safetynet_core::isa::{
    Add, And, CmpEq, CmpLe, CmpLt, CmpSLe, CmpSLt, Div, Ld8, Ld32, Ld64, Mul, Or, Push8, Push32,
    Push64, Rem, SDiv, SRem, Sar, Shl, Shr, Sub, Xor,
};
use safetynet_core::{Instr, Region, Width};

use super::ty::Scalar;

/// One word on the operand stack, in bytes: the depth a produced value adds.
const WORD: u32 = 8;

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
    Local {
        cell: safetynet_core::ir::CellId,
        ty: Scalar,
    },
}

/// Whether a statement path continues or has already left the block.
#[derive(Clone, Copy, PartialEq)]
enum Flow {
    /// Execution reaches the end of the current block; it still needs a
    /// terminator.
    Open,
    /// The path returned; the current block is already sealed.
    Diverged,
}

/// What a value-producing block left behind.
enum Value {
    /// A value of this type is on top of the stack.
    Produced(Scalar),
    /// Every path returned; there is no value and no fall-through.
    Diverged,
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

    // One cell per top-level local, before anything is lowered.
    let mut frame = Frame::new();
    let mut cells = Vec::new();
    for stmt in &func.block.stmts {
        if let syn::Stmt::Local(local) = stmt {
            let (_, scalar, ty_node) = declared_local(local)?;
            let cell = frame
                .add(scalar.width)
                .ok_or_else(|| err(&local.pat, "the frame is too large"))?;
            cells.push(cell);
            bindings.push(ty_node);
        }
    }

    let mut builder = Cfg::builder(frame);
    let entry = builder.block(0);
    let mut lowerer = Lowerer {
        builder,
        scope,
        cells: cells.into_iter(),
        cur: entry,
        ret,
    };
    lowerer.lower_fn_body(&func.block)?;
    let cfg = lowerer
        .builder
        .build(entry)
        .map_err(|error| internal(&func.sig.ident, error))?;

    Ok(Lowered {
        cfg,
        bindings,
        param_offsets,
        input_size,
        ret: ret_ty.clone(),
    })
}

/// The state threaded through lowering a body.
struct Lowerer {
    builder: safetynet_core::ir::Builder,
    scope: HashMap<String, Binding>,
    cells: std::vec::IntoIter<safetynet_core::ir::CellId>,
    /// The block instructions are being appended to.
    cur: BlockId,
    ret: Scalar,
}

impl Lowerer {
    /// Lowers the function body: statements, then a value or a return on every
    /// path.
    fn lower_fn_body(&mut self, block: &syn::Block) -> syn::Result<()> {
        let stmts = &block.stmts;
        for (index, stmt) in stmts.iter().enumerate() {
            let last = index + 1 == stmts.len();
            let flow = match stmt {
                syn::Stmt::Local(local) => {
                    self.lower_let(local)?;
                    Flow::Open
                }
                syn::Stmt::Expr(syn::Expr::Return(ret), _) => {
                    self.lower_return(ret)?;
                    Flow::Diverged
                }
                syn::Stmt::Expr(syn::Expr::If(if_expr), semi) if last && semi.is_none() => {
                    match self.lower_if_value(if_expr, self.ret)? {
                        Value::Produced(_) => {
                            self.seal(self.cur, Terminator::Halt)?;
                            Flow::Diverged
                        }
                        Value::Diverged => Flow::Diverged,
                    }
                }
                syn::Stmt::Expr(syn::Expr::If(if_expr), _) => self.lower_if_stmt(if_expr)?,
                syn::Stmt::Expr(expr, None) if last => {
                    self.lower_value(expr, self.ret)?;
                    self.seal(self.cur, Terminator::Halt)?;
                    Flow::Diverged
                }
                other => {
                    return Err(err(
                        other,
                        "only `let`, `if`, `return`, and a final expression are supported yet",
                    ));
                }
            };
            if flow == Flow::Diverged {
                return Ok(());
            }
        }
        Err(err(block, "the function must end by returning a value"))
    }

    /// Lowers a `let`, storing the initializer into the local's cell.
    fn lower_let(&mut self, local: &syn::Local) -> syn::Result<()> {
        let (name, scalar, _) = declared_local(local)?;
        let cell = self
            .cells
            .next()
            .ok_or_else(|| err(&local.pat, "a local without a cell"))?;
        let init = &local
            .init
            .as_ref()
            .ok_or_else(|| err(&local.pat, "a local needs an initializer"))?
            .expr;
        self.lower_value(init, scalar)?;
        self.store(cell)?;
        self.scope.insert(name, Binding::Local { cell, ty: scalar });
        Ok(())
    }

    /// Lowers `return e`: leave the value on the stack, then halt.
    fn lower_return(&mut self, ret: &syn::ExprReturn) -> syn::Result<()> {
        let value = ret
            .expr
            .as_ref()
            .ok_or_else(|| err(ret, "the function must return a value"))?;
        self.lower_value(value, self.ret)?;
        self.seal(self.cur, Terminator::Halt)
    }

    /// Lowers an `if` used as a statement, tracking whether it falls through.
    fn lower_if_stmt(&mut self, if_expr: &syn::ExprIf) -> syn::Result<Flow> {
        self.lower_expr(&if_expr.cond, Scalar::BOOL)?;
        let then_id = self.builder.block(0);

        let Some((_, else_expr)) = &if_expr.else_branch else {
            // No `else`: the false path is the fall-through.
            let join = self.builder.block(0);
            self.seal(
                self.cur,
                Terminator::Br {
                    then: then_id,
                    els: join,
                },
            )?;
            self.cur = then_id;
            if self.lower_branch(&if_expr.then_branch)? == Flow::Open {
                self.seal(self.cur, Terminator::Jmp(join))?;
            }
            self.cur = join;
            return Ok(Flow::Open);
        };

        let else_id = self.builder.block(0);
        self.seal(
            self.cur,
            Terminator::Br {
                then: then_id,
                els: else_id,
            },
        )?;

        self.cur = then_id;
        let then_exit =
            (self.lower_branch(&if_expr.then_branch)? == Flow::Open).then_some(self.cur);

        self.cur = else_id;
        let else_exit = (self.lower_else_stmt(else_expr)? == Flow::Open).then_some(self.cur);

        match (then_exit, else_exit) {
            (None, None) => Ok(Flow::Diverged),
            _ => {
                let join = self.builder.block(0);
                if let Some(block) = then_exit {
                    self.seal(block, Terminator::Jmp(join))?;
                }
                if let Some(block) = else_exit {
                    self.seal(block, Terminator::Jmp(join))?;
                }
                self.cur = join;
                Ok(Flow::Open)
            }
        }
    }

    /// Lowers the `else` of a statement `if`: another block, or an `else if`.
    fn lower_else_stmt(&mut self, else_expr: &syn::Expr) -> syn::Result<Flow> {
        match else_expr {
            syn::Expr::Block(block) => self.lower_branch(&block.block),
            syn::Expr::If(if_expr) => self.lower_if_stmt(if_expr),
            other => Err(err(other, "an `else` must be a block or another `if`")),
        }
    }

    /// Lowers a branch body: statements only, no value and no new locals.
    fn lower_branch(&mut self, block: &syn::Block) -> syn::Result<Flow> {
        for stmt in &block.stmts {
            match stmt {
                syn::Stmt::Expr(syn::Expr::Return(ret), _) => {
                    self.lower_return(ret)?;
                    return Ok(Flow::Diverged);
                }
                syn::Stmt::Expr(syn::Expr::If(if_expr), _) => {
                    if self.lower_if_stmt(if_expr)? == Flow::Diverged {
                        return Ok(Flow::Diverged);
                    }
                }
                syn::Stmt::Local(local) => {
                    return Err(err(
                        &local.pat,
                        "a `let` inside a branch is not supported yet",
                    ));
                }
                other => {
                    return Err(err(
                        other,
                        "only `if` and `return` are supported in a branch",
                    ));
                }
            }
        }
        Ok(Flow::Open)
    }

    /// Lowers an `if` used as a value: both reached arms leave a value, and the
    /// arms that return leave none.
    fn lower_if_value(&mut self, if_expr: &syn::ExprIf, expected: Scalar) -> syn::Result<Value> {
        let (_, else_expr) = if_expr
            .else_branch
            .as_ref()
            .ok_or_else(|| err(if_expr, "an `if` used as a value needs an `else`"))?;

        self.lower_expr(&if_expr.cond, Scalar::BOOL)?;
        let then_id = self.builder.block(0);
        let else_id = self.builder.block(0);
        self.seal(
            self.cur,
            Terminator::Br {
                then: then_id,
                els: else_id,
            },
        )?;

        self.cur = then_id;
        let then = self.lower_value_block(&if_expr.then_branch, expected)?;
        let then_exit = value_exit(then, self.cur);

        self.cur = else_id;
        let els = self.lower_else_value(else_expr, expected)?;
        let else_exit = value_exit(els, self.cur);

        match (then_exit, else_exit) {
            (None, None) => Ok(Value::Diverged),
            _ => {
                let join = self.builder.block(WORD);
                let mut ty = None;
                for (block, scalar) in [then_exit, else_exit].into_iter().flatten() {
                    self.seal(block, Terminator::Jmp(join))?;
                    ty = Some(scalar);
                }
                self.cur = join;
                Ok(Value::Produced(ty.unwrap_or(expected)))
            }
        }
    }

    /// Lowers the `else` of a value `if`: a block, or an `else if`.
    fn lower_else_value(&mut self, else_expr: &syn::Expr, expected: Scalar) -> syn::Result<Value> {
        match else_expr {
            syn::Expr::Block(block) => self.lower_value_block(&block.block, expected),
            syn::Expr::If(if_expr) => self.lower_if_value(if_expr, expected),
            other => Err(err(other, "an `else` must be a block or another `if`")),
        }
    }

    /// Lowers a block that produces a value: statements, then a value or a
    /// return.
    fn lower_value_block(&mut self, block: &syn::Block, expected: Scalar) -> syn::Result<Value> {
        let stmts = &block.stmts;
        let (last, rest) = match stmts.split_last() {
            Some(split) => split,
            None => return Err(err(block, "a block used as a value must produce one")),
        };
        for stmt in rest {
            match stmt {
                syn::Stmt::Expr(syn::Expr::Return(ret), _) => {
                    self.lower_return(ret)?;
                    return Ok(Value::Diverged);
                }
                syn::Stmt::Expr(syn::Expr::If(if_expr), _) => {
                    if self.lower_if_stmt(if_expr)? == Flow::Diverged {
                        return Ok(Value::Diverged);
                    }
                }
                syn::Stmt::Local(local) => {
                    return Err(err(
                        &local.pat,
                        "a `let` inside a branch is not supported yet",
                    ));
                }
                other => return Err(err(other, "only `if` and `return` are supported here")),
            }
        }
        match last {
            syn::Stmt::Expr(syn::Expr::Return(ret), _) => {
                self.lower_return(ret)?;
                Ok(Value::Diverged)
            }
            syn::Stmt::Expr(expr, None) => Ok(Value::Produced(self.lower_value(expr, expected)?)),
            other => Err(err(
                other,
                "a block used as a value must end with an expression",
            )),
        }
    }

    /// Lowers an expression in a value position, where an `if` may itself be the
    /// value.
    fn lower_value(&mut self, expr: &syn::Expr, expected: Scalar) -> syn::Result<Scalar> {
        match expr {
            syn::Expr::If(if_expr) => match self.lower_if_value(if_expr, expected)? {
                Value::Produced(scalar) => Ok(scalar),
                Value::Diverged => Err(err(if_expr, "this `if` never produces a value")),
            },
            syn::Expr::Block(block) => match self.lower_value_block(&block.block, expected)? {
                Value::Produced(scalar) => Ok(scalar),
                Value::Diverged => Err(err(block, "this block never produces a value")),
            },
            other => self.lower_expr(other, expected),
        }
    }

    /// Lowers a plain expression, leaving its value on top of the stack.
    fn lower_expr(&mut self, expr: &syn::Expr, expected: Scalar) -> syn::Result<Scalar> {
        match expr {
            syn::Expr::Lit(lit) => self.lower_lit(lit, expected),
            syn::Expr::Path(path) => self.lower_path(path),
            syn::Expr::Paren(paren) => self.lower_expr(&paren.expr, expected),
            syn::Expr::Unary(unary) => self.lower_unary(unary, expected),
            syn::Expr::Binary(binary) => self.lower_binary(binary, expected),
            other => Err(err(other, "this expression is not supported yet")),
        }
    }

    /// Pushes a literal; its type is the context's, or `i32` with nothing to go
    /// on.
    fn lower_lit(&mut self, lit: &syn::ExprLit, expected: Scalar) -> syn::Result<Scalar> {
        match &lit.lit {
            syn::Lit::Int(int) => {
                self.push_word(int.base10_parse()?)?;
                Ok(expected)
            }
            syn::Lit::Bool(boolean) => {
                self.push_word(boolean.value.into())?;
                Ok(Scalar::BOOL)
            }
            other => Err(err(other, "only integer and bool literals are supported")),
        }
    }

    /// Reads a parameter from `.input` or a local from its cell.
    fn lower_path(&mut self, path: &syn::ExprPath) -> syn::Result<Scalar> {
        let name = path
            .path
            .get_ident()
            .ok_or_else(|| err(path, "expected a parameter or local"))?;
        match self.scope.get(&name.to_string()).copied() {
            Some(Binding::Param { offset, ty }) => {
                self.push_base(Region::Input)?;
                if offset != 0 {
                    self.push_word(u64::from(offset))?;
                    self.push_instr(Add)?;
                }
                self.push_instr(load(ty.width))?;
                Ok(ty)
            }
            Some(Binding::Local { cell, ty }) => {
                self.load(cell)?;
                Ok(ty)
            }
            None => Err(err(name, "no such parameter or local")),
        }
    }

    /// Lowers a unary operator.
    fn lower_unary(&mut self, unary: &syn::ExprUnary, expected: Scalar) -> syn::Result<Scalar> {
        match unary.op {
            // Negation is `0 - x`, which the wrapping subtraction handles.
            syn::UnOp::Neg(_) => {
                self.push_word(0)?;
                let ty = self.lower_expr(&unary.expr, expected)?;
                self.push_instr(Sub)?;
                Ok(ty)
            }
            syn::UnOp::Not(_) => {
                let ty = self.lower_expr(&unary.expr, expected)?;
                if ty == Scalar::BOOL {
                    // Logical not: the flag is one exactly when the value was zero.
                    self.push_word(0)?;
                    self.push_instr(CmpEq)?;
                    Ok(Scalar::BOOL)
                } else {
                    self.push_instr(safetynet_core::isa::BitNot)?;
                    Ok(ty)
                }
            }
            other => Err(err(other, "this operator is not supported yet")),
        }
    }

    /// Lowers a binary operation and the operator on top of it.
    fn lower_binary(&mut self, binary: &syn::ExprBinary, expected: Scalar) -> syn::Result<Scalar> {
        // The operands share a type of their own; a comparison's `bool` result is
        // not it. Infer that type from whichever operand names one, falling back
        // to the result type for arithmetic and to `i32` for a bare comparison.
        let fallback = if is_comparison(&binary.op) {
            Scalar::I32
        } else {
            expected
        };
        let operand = self
            .peek(&binary.left)
            .or_else(|| self.peek(&binary.right))
            .unwrap_or(fallback);

        // `>` and `>=` are the mirror of `<` and `<=`, so they lower their
        // operands in the other order and reuse the same opcode.
        let swap = matches!(binary.op, syn::BinOp::Gt(_) | syn::BinOp::Ge(_));
        let (first, second) = if swap {
            (&*binary.right, &*binary.left)
        } else {
            (&*binary.left, &*binary.right)
        };
        let left = self.lower_expr(first, operand)?;
        let right = self.lower_expr(second, operand)?;
        if left.width != right.width {
            return Err(err(binary, "the operands must be the same width"));
        }
        self.emit_op(binary, operand)
    }

    /// The type an expression will have, when that is known without lowering it.
    ///
    /// A literal gives nothing back, so a sibling operand's type drives the two.
    fn peek(&self, expr: &syn::Expr) -> Option<Scalar> {
        match expr {
            syn::Expr::Path(path) => {
                let name = path.path.get_ident()?;
                Some(match self.scope.get(&name.to_string())? {
                    Binding::Param { ty, .. } | Binding::Local { ty, .. } => *ty,
                })
            }
            syn::Expr::Paren(paren) => self.peek(&paren.expr),
            syn::Expr::Unary(unary) => self.peek(&unary.expr),
            syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Bool(_),
                ..
            }) => Some(Scalar::BOOL),
            syn::Expr::Binary(binary) if is_comparison(&binary.op) => Some(Scalar::BOOL),
            syn::Expr::Binary(binary) => {
                self.peek(&binary.left).or_else(|| self.peek(&binary.right))
            }
            _ => None,
        }
    }

    /// Emits the opcode a binary operator lowers to, and reports its result
    /// type.
    fn emit_op(&mut self, binary: &syn::ExprBinary, ty: Scalar) -> syn::Result<Scalar> {
        use syn::BinOp;
        match binary.op {
            BinOp::Add(_) => self.push_instr(Add)?,
            BinOp::Sub(_) => self.push_instr(Sub)?,
            BinOp::Mul(_) => self.push_instr(Mul)?,
            BinOp::Div(_) => self.push_instr(signed(ty, Instr::from(SDiv), Div.into()))?,
            BinOp::Rem(_) => self.push_instr(signed(ty, SRem.into(), Rem.into()))?,
            BinOp::BitAnd(_) => self.push_instr(And)?,
            BinOp::BitOr(_) => self.push_instr(Or)?,
            BinOp::BitXor(_) => self.push_instr(Xor)?,
            BinOp::Shl(_) => self.push_instr(Shl)?,
            BinOp::Shr(_) => self.push_instr(signed(ty, Sar.into(), Shr.into()))?,
            BinOp::Eq(_) => return self.cmp(CmpEq.into()),
            BinOp::Lt(_) | BinOp::Gt(_) => {
                return self.cmp(signed(ty, CmpSLt.into(), CmpLt.into()));
            }
            BinOp::Le(_) | BinOp::Ge(_) => {
                return self.cmp(signed(ty, CmpSLe.into(), CmpLe.into()));
            }
            BinOp::Ne(_) => {
                self.push_instr(CmpEq)?;
                // Negate the flag: it equals zero exactly when the values differed.
                self.push_word(0)?;
                self.push_instr(CmpEq)?;
                return Ok(Scalar::BOOL);
            }
            other => return Err(err(other, "this operator is not supported yet")),
        }
        Ok(ty)
    }

    /// Emits a comparison, which always yields a boolean.
    fn cmp(&mut self, op: Instr) -> syn::Result<Scalar> {
        self.push_instr(op)?;
        Ok(Scalar::BOOL)
    }

    /// Pushes a word with the narrowest immediate that holds it.
    fn push_word(&mut self, value: u64) -> syn::Result<()> {
        if let Ok(byte) = u8::try_from(value) {
            self.push_instr(Push8 { imm: byte })
        } else if let Ok(word) = u32::try_from(value) {
            self.push_instr(Push32 { imm: word })
        } else {
            self.push_instr(Push64 { imm: value })
        }
    }

    /// Appends an instruction to the current block.
    fn push_instr(&mut self, instr: impl Into<Instr>) -> syn::Result<()> {
        self.body()?.instr(instr);
        Ok(())
    }

    /// Appends the base address of a region.
    fn push_base(&mut self, region: Region) -> syn::Result<()> {
        self.body()?.base(region);
        Ok(())
    }

    /// Appends a load from a frame cell.
    fn load(&mut self, cell: safetynet_core::ir::CellId) -> syn::Result<()> {
        self.body()?.load(cell);
        Ok(())
    }

    /// Appends a store into a frame cell.
    fn store(&mut self, cell: safetynet_core::ir::CellId) -> syn::Result<()> {
        self.body()?.store(cell);
        Ok(())
    }

    /// The current block's body.
    fn body(&mut self) -> syn::Result<&mut safetynet_core::ir::BlockBody> {
        let cur = self.cur;
        self.builder.at(cur).map_err(internal_span)
    }

    /// Seals a block with its terminator.
    fn seal(&mut self, block: BlockId, term: Terminator) -> syn::Result<()> {
        self.builder.seal(block, term).map_err(internal_span)
    }
}

/// The block a value arm exits from, if it produced a value.
fn value_exit(value: Value, exit: BlockId) -> Option<(BlockId, Scalar)> {
    match value {
        Value::Produced(scalar) => Some((exit, scalar)),
        Value::Diverged => None,
    }
}

/// Picks the signed or unsigned opcode by the operand type.
fn signed(ty: Scalar, when_signed: Instr, when_unsigned: Instr) -> Instr {
    if ty.signed {
        when_signed
    } else {
        when_unsigned
    }
}

/// Whether an operator compares (yielding a boolean) rather than computes.
fn is_comparison(op: &syn::BinOp) -> bool {
    use syn::BinOp;
    matches!(
        op,
        BinOp::Eq(_) | BinOp::Ne(_) | BinOp::Lt(_) | BinOp::Le(_) | BinOp::Gt(_) | BinOp::Ge(_)
    )
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

/// A builder failure, which means the lowerer built something impossible.
fn internal<T: ToTokens>(node: T, error: impl core::fmt::Display) -> syn::Error {
    syn::Error::new_spanned(node, format!("internal lowering error: {error}"))
}

/// A builder failure with nothing smaller than the invocation to blame.
fn internal_span(error: impl core::fmt::Display) -> syn::Error {
    syn::Error::new(
        Span::call_site(),
        format!("internal lowering error: {error}"),
    )
}
