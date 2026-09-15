//! Lowering a function's body to a graph.
//!
//! Parameters are read from `.input` at offsets packed the way the marshaller
//! writes them; locals live in frame cells; the value the function evaluates to
//! is left on the stack, which the caller reads once the machine halts.
//!
//! Structured control flow becomes blocks and edges. Each construct is lowered
//! into fresh blocks, and the lowerer tracks whether a path *falls through* to
//! the next block or *diverges* — by returning, breaking or continuing — so a
//! branch or loop that never falls through leaves no unreachable block behind,
//! which the graph check would reject.

use std::collections::HashMap;

use proc_macro2::Span;
use quote::ToTokens;
use safetynet_core::ir::{BlockId, Builder, CellId, Cfg, Frame, Terminator};
use safetynet_core::isa::{
    Add, And, BitNot, CmpEq, CmpLe, CmpLt, CmpSLe, CmpSLt, Div, Ld8, Ld32, Ld64, Mul, Or, Push8,
    Push32, Push64, Rem, SDiv, SRem, Sar, Shl, Shr, Sub, Xor,
};
use safetynet_core::{Instr, Region, WORD_SIZE, Width, Word};

use super::ty::{self, Scalar};
use crate::backend::FieldRef;

/// A function lowered to its graph, with what the wrapper needs to marshal for
/// it and the binding types the call site must still prove are `VmValue`.
#[derive(Debug)]
pub(crate) struct Lowered {
    pub(crate) cfg: Cfg,
    /// Types the call site must prove are `VmValue` (scalars).
    pub(crate) bindings: Vec<syn::Type>,
    /// Types the call site must prove are `VmLayout` (aggregate parameters).
    pub(crate) layouts: Vec<syn::Type>,
    /// Field references, indexed by the hole id in [`Item::Field`] and
    /// [`Item::LoadField`].
    pub(crate) field_refs: Vec<FieldRef>,
    /// Each scalar parameter's byte offset in `.input`, in signature order.
    pub(crate) param_offsets: Vec<u32>,
    /// Bytes the scalar parameters occupy, when there is no aggregate.
    pub(crate) input_size: u32,
    /// The single aggregate parameter, if any: it fills `.input` on its own.
    pub(crate) aggregate: Option<syn::Type>,
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

/// The blocks a `break` and a `continue` jump to for one enclosing loop.
struct Loop {
    /// The loop's label, without its tick.
    label: Option<String>,
    /// Where `continue` goes: a `while`'s head, a `loop`'s body.
    continue_to: BlockId,
    /// Where `break` goes, created on the first one a `loop` needs.
    break_to: Option<BlockId>,
}

/// Whether a statement path continues or has already left the block.
#[derive(Clone, Copy, PartialEq)]
enum Flow {
    /// Execution reaches the end of the current block; it still needs a
    /// terminator.
    Open,
    /// The path left the block, which is already sealed.
    Diverged,
}

/// What a value-producing block left behind.
enum Value {
    /// A value of this type is on top of the stack.
    Produced(Scalar),
    /// Every path left; there is no value and no fall-through.
    Diverged,
}

/// The plain binary operators, once assignment and mirroring are stripped off.
#[derive(Clone, Copy)]
enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    And,
    Or,
    Xor,
    Shl,
    Shr,
    Eq,
    Ne,
    /// Less-than, after any operand swap for `>`.
    Lt,
    /// Less-or-equal, after any operand swap for `>=`.
    Le,
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

    // Parameters first: scalars pack into `.input` at their offsets; a single
    // aggregate fills `.input` on its own and is read by field.
    let mut bindings = Vec::new();
    let mut layouts = Vec::new();
    let mut param_offsets = Vec::new();
    let mut scope: HashMap<String, Binding> = HashMap::new();
    let mut aggregate: Option<(String, syn::Type)> = None;
    let mut offset = 0u32;
    for arg in &func.sig.inputs {
        let (name, ty_node) = param(arg)?;
        match Scalar::of(ty_node) {
            Some(scalar) => {
                offset = offset.next_multiple_of(scalar.align());
                param_offsets.push(offset);
                scope.insert(name, Binding::Param { offset, ty: scalar });
                offset += scalar.size();
                bindings.push(ty_node.clone());
            }
            None if ty::unsupported_primitive(ty_node) => {
                return Err(err(
                    ty_node,
                    "a parameter must be a scalar the machine can hold",
                ));
            }
            None => {
                aggregate = Some((name, ty_node.clone()));
                layouts.push(ty_node.clone());
            }
        }
    }
    // The aggregate reads from `.input` base, so nothing else may share it.
    if aggregate.is_some() && func.sig.inputs.len() > 1 {
        return Err(err(
            &func.sig.inputs,
            "an aggregate parameter must be the only parameter for now",
        ));
    }
    let input_size = offset;
    bindings.push(ret_ty.clone());

    // Locals get their cells as lowering reaches each `let`: the builder grows
    // the frame in place
    let mut builder = Cfg::builder(Frame::new());
    let entry = builder.block(0);
    let mut lowerer = Lowerer {
        builder,
        scope: vec![scope],
        bindings,
        loops: Vec::new(),
        field_refs: Vec::new(),
        field_types: HashMap::new(),
        aggregate: aggregate.clone(),
        cur: entry,
        ret,
    };
    lowerer.lower_fn_body(&func.block)?;
    let field_refs = lowerer.field_refs;
    let bindings = lowerer.bindings;
    let cfg = lowerer
        .builder
        .build(entry)
        .map_err(|error| internal(&func.sig.ident, error))?;

    Ok(Lowered {
        cfg,
        bindings,
        layouts,
        field_refs,
        param_offsets,
        input_size,
        aggregate: aggregate.map(|(_, ty)| ty),
        ret: ret_ty.clone(),
    })
}

/// The state threaded through lowering a body.
struct Lowerer {
    builder: Builder,
    /// Name resolution, innermost block last: every syntactic block pushes a
    /// frame and pops it on exit, so a binding lives exactly as long as its
    /// block. The outermost frame holds the parameters.
    scope: Vec<HashMap<String, Binding>>,
    /// Types the call site must prove are `VmValue`, appended as locals are
    /// lowered.
    bindings: Vec<syn::Type>,
    loops: Vec<Loop>,
    /// Field references, appended as field accesses are lowered.
    field_refs: Vec<FieldRef>,
    /// Each field's scalar and named type spelling, keyed by its dotted path.
    field_types: HashMap<String, (Scalar, String)>,
    /// The single aggregate parameter's name and type, if the function has one.
    aggregate: Option<(String, syn::Type)>,
    /// The block instructions are being appended to.
    cur: BlockId,
    ret: Scalar,
}

impl Lowerer {
    /// Lowers the function body: statements, then a value or a return on every
    /// path.
    fn lower_fn_body(&mut self, block: &syn::Block) -> syn::Result<()> {
        self.scoped(|this| {
            let stmts = &block.stmts;
            for (index, stmt) in stmts.iter().enumerate() {
                let last = index + 1 == stmts.len();
                let flow = if last {
                    this.lower_tail(stmt)?
                } else {
                    this.lower_stmt(stmt)?
                };
                if flow == Flow::Diverged {
                    return Ok(());
                }
            }
            Err(err(block, "the function must end by returning a value"))
        })
    }

    /// Runs `f` inside a fresh scope frame that dies when it returns.
    fn scoped<T>(&mut self, f: impl FnOnce(&mut Self) -> syn::Result<T>) -> syn::Result<T> {
        self.scope.push(HashMap::new());
        let result = f(self);
        self.scope.pop();
        result
    }

    /// The binding `name` resolves to, innermost frame first.
    fn lookup(&self, name: &str) -> Option<Binding> {
        self.scope
            .iter()
            .rev()
            .find_map(|frame| frame.get(name))
            .copied()
    }

    /// Binds `name` in the innermost frame.
    fn bind(&mut self, name: String, binding: Binding) -> syn::Result<()> {
        self.scope
            .last_mut()
            .ok_or_else(|| internal_span("no scope frame to bind into"))?
            .insert(name, binding);
        Ok(())
    }

    /// Lowers the last statement of the function body, where an expression is the
    /// returned value.
    fn lower_tail(&mut self, stmt: &syn::Stmt) -> syn::Result<Flow> {
        match stmt {
            syn::Stmt::Expr(syn::Expr::Return(ret), _) => {
                self.lower_return(ret)?;
                Ok(Flow::Diverged)
            }
            syn::Stmt::Expr(syn::Expr::If(if_expr), None) => {
                match self.lower_if_value(if_expr, self.ret)? {
                    Value::Produced(_) => {
                        self.seal(self.cur, Terminator::Halt)?;
                        Ok(Flow::Diverged)
                    }
                    Value::Diverged => Ok(Flow::Diverged),
                }
            }
            syn::Stmt::Expr(expr, None) if is_value_expr(expr) => {
                self.lower_value(expr, self.ret)?;
                self.seal(self.cur, Terminator::Halt)?;
                Ok(Flow::Diverged)
            }
            other => self.lower_stmt(other),
        }
    }

    /// Lowers a statement in statement position.
    fn lower_stmt(&mut self, stmt: &syn::Stmt) -> syn::Result<Flow> {
        match stmt {
            syn::Stmt::Local(local) => {
                self.lower_let(local)?;
                Ok(Flow::Open)
            }
            syn::Stmt::Expr(expr, _) => match expr {
                syn::Expr::Return(ret) => {
                    self.lower_return(ret)?;
                    Ok(Flow::Diverged)
                }
                syn::Expr::If(if_expr) => self.lower_if_stmt(if_expr),
                syn::Expr::While(while_expr) => self.lower_while(while_expr),
                syn::Expr::Loop(loop_expr) => self.lower_loop(loop_expr),
                syn::Expr::ForLoop(for_expr) => self.lower_for(for_expr),
                syn::Expr::Break(brk) => self.lower_break(brk),
                syn::Expr::Continue(cont) => self.lower_continue(cont),
                syn::Expr::Assign(assign) => {
                    self.lower_assign(&assign.left, &assign.right)?;
                    Ok(Flow::Open)
                }
                syn::Expr::Binary(binary) if compound(&binary.op).is_some() => {
                    self.lower_compound(binary)?;
                    Ok(Flow::Open)
                }
                other => Err(err(other, "this statement is not supported yet")),
            },
            other => Err(err(other, "this statement is not supported yet")),
        }
    }

    /// Lowers a run of statements, stopping once a path has diverged.
    fn lower_stmts(&mut self, stmts: &[syn::Stmt]) -> syn::Result<Flow> {
        let mut flow = Flow::Open;
        for stmt in stmts {
            if flow == Flow::Diverged {
                break;
            }
            flow = self.lower_stmt(stmt)?;
        }
        Ok(flow)
    }

    /// Lowers a `let`, storing the initializer into the local's cell.
    fn lower_let(&mut self, local: &syn::Local) -> syn::Result<()> {
        let (name, scalar, ty_node) = declared_local(local)?;
        let cell = self
            .builder
            .cell(scalar.width)
            .ok_or_else(|| err(&local.pat, "the frame is too large"))?;
        self.bindings.push(ty_node);
        let init = &local
            .init
            .as_ref()
            .ok_or_else(|| err(&local.pat, "a local needs an initializer"))?
            .expr;
        self.lower_value(init, scalar)?;
        self.store(cell)?;
        self.bind(name, Binding::Local { cell, ty: scalar })
    }

    /// Lowers `x = e`: evaluate `e`, store it into the local `x`.
    fn lower_assign(&mut self, target: &syn::Expr, value: &syn::Expr) -> syn::Result<()> {
        let (cell, ty) = self.assign_target(target)?;
        self.lower_value(value, ty)?;
        self.store(cell)
    }

    /// Lowers `x op= e`: load `x`, apply `op` with `e`, store it back.
    fn lower_compound(&mut self, binary: &syn::ExprBinary) -> syn::Result<()> {
        let op = compound(&binary.op).ok_or_else(|| err(binary, "not a compound assignment"))?;
        let (cell, ty) = self.assign_target(&binary.left)?;
        self.load(cell)?;
        self.normalize_load(ty)?;
        let right = self.lower_expr(&binary.right, ty, WORD_SIZE as u32)?;
        if right.width != ty.width {
            return Err(err(binary, "the operands must be the same width"));
        }
        self.emit_op(op, ty)?;
        self.store(cell)
    }

    /// Resolves an assignment target to the local cell it writes.
    fn assign_target(&self, target: &syn::Expr) -> syn::Result<(CellId, Scalar)> {
        let path = match target {
            syn::Expr::Path(path) => path,
            other => return Err(err(other, "only a local can be assigned to")),
        };
        let name = path
            .path
            .get_ident()
            .ok_or_else(|| err(path, "only a local can be assigned to"))?;
        match self.lookup(&name.to_string()) {
            Some(Binding::Local { cell, ty }) => Ok((cell, ty)),
            Some(Binding::Param { .. }) => Err(err(name, "a parameter cannot be assigned to")),
            None => Err(err(name, "no such local")),
        }
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

    /// Lowers `break`, jumping to the enclosing loop's exit.
    fn lower_break(&mut self, brk: &syn::ExprBreak) -> syn::Result<Flow> {
        if brk.expr.is_some() {
            return Err(err(brk, "`break` with a value is not supported yet"));
        }
        let index = self.target_loop(brk.label.as_ref(), brk, "`break` outside a loop")?;
        let existing = self.loops.get(index).and_then(|target| target.break_to);
        let exit = match existing {
            Some(exit) => exit,
            None => {
                let exit = self.builder.block(0);
                if let Some(target) = self.loops.get_mut(index) {
                    target.break_to = Some(exit);
                }
                exit
            }
        };
        self.seal(self.cur, Terminator::Jmp(exit))?;
        Ok(Flow::Diverged)
    }

    /// Lowers `continue`, jumping to its loop's head.
    fn lower_continue(&mut self, cont: &syn::ExprContinue) -> syn::Result<Flow> {
        let index = self.target_loop(cont.label.as_ref(), cont, "`continue` outside a loop")?;
        let head = self
            .loops
            .get(index)
            .ok_or_else(|| err(cont, "`continue` outside a loop"))?
            .continue_to;
        self.seal(self.cur, Terminator::Jmp(head))?;
        Ok(Flow::Diverged)
    }

    /// The loop a `break` or `continue` targets: the innermost one, or the
    /// innermost carrying the named label.
    fn target_loop<T: ToTokens>(
        &self,
        label: Option<&syn::Lifetime>,
        node: T,
        outside: &str,
    ) -> syn::Result<usize> {
        match label {
            None => self
                .loops
                .len()
                .checked_sub(1)
                .ok_or_else(|| err(node, outside)),
            Some(lifetime) => {
                let name = lifetime.ident.to_string();
                self.loops
                    .iter()
                    .rposition(|target| target.label.as_deref() == Some(&name))
                    .ok_or_else(|| err(lifetime, "no enclosing loop has this label"))
            }
        }
    }

    /// Lowers `while cond { body }`.
    fn lower_while(&mut self, while_expr: &syn::ExprWhile) -> syn::Result<Flow> {
        let head = self.builder.block(0);
        self.seal(self.cur, Terminator::Jmp(head))?;
        self.cur = head;
        self.lower_expr(&while_expr.cond, Scalar::BOOL, 0)?;

        let body = self.builder.block(0);
        let exit = self.builder.block(0);
        self.seal(
            self.cur,
            Terminator::Br {
                then: body,
                els: exit,
            },
        )?;

        self.cur = body;
        self.loops.push(Loop {
            label: loop_label(&while_expr.label),
            continue_to: head,
            break_to: Some(exit),
        });
        let flow = self.scoped(|this| this.lower_stmts(&while_expr.body.stmts))?;
        self.loops.pop();
        if flow == Flow::Open {
            self.seal(self.cur, Terminator::Jmp(head))?;
        }

        // The condition being false always reaches the exit.
        self.cur = exit;
        Ok(Flow::Open)
    }

    /// The scalar a `for` range runs over: a typed bound wins, then a literal
    /// suffix, then the `i32` a bare literal defaults to — the same default the
    /// reference copy's inference lands on, whose `overflowing_literals` check
    /// keeps honest.
    fn range_scalar(&self, start: &syn::Expr, end: &syn::Expr) -> syn::Result<Scalar> {
        if let Some(scalar) = self.peek(start).or_else(|| self.peek(end)) {
            return Ok(scalar);
        }
        match suffix_scalar(start).or_else(|| suffix_scalar(end)) {
            Some(result) => result,
            None => Ok(Scalar::I32),
        }
    }

    /// Lowers `for i in a..b { body }` as a counted loop.
    ///
    /// The end bound is snapshotted so mutating it in the body cannot change the
    /// count. `continue` targets the increment, so the variable still advances.
    fn lower_for(&mut self, for_expr: &syn::ExprForLoop) -> syn::Result<Flow> {
        let (start, end) = range_bounds(for_expr)?;
        let var = for_var(for_expr)?;
        let scalar = self.range_scalar(start, end)?;

        let i_cell = self
            .builder
            .cell(scalar.width)
            .ok_or_else(|| err(&for_expr.pat, "the frame is too large"))?;
        let end_cell = self
            .builder
            .cell(scalar.width)
            .ok_or_else(|| err(&for_expr.pat, "the frame is too large"))?;

        // i = start; end = b. Both bounds are evaluated outside the variable's
        // scope, so `for i in i..n` reads the outer `i`.
        self.lower_expr(start, scalar, 0)?;
        self.store(i_cell)?;
        self.lower_expr(end, scalar, 0)?;
        self.store(end_cell)?;

        let head = self.builder.block(0);
        self.seal(self.cur, Terminator::Jmp(head))?;
        self.cur = head;
        self.load(i_cell)?;
        self.normalize_load(scalar)?;
        self.load(end_cell)?;
        self.normalize_load(scalar)?;
        self.push_instr(signed(scalar, CmpSLt.into(), CmpLt.into()))?;

        let body = self.builder.block(0);
        let incr = self.builder.block(0);
        let exit = self.builder.block(0);
        self.seal(
            self.cur,
            Terminator::Br {
                then: body,
                els: exit,
            },
        )?;

        self.cur = body;
        self.loops.push(Loop {
            label: loop_label(&for_expr.label),
            continue_to: incr,
            break_to: Some(exit),
        });
        let flow = self.scoped(|this| {
            if let Some(name) = var {
                this.bind(
                    name,
                    Binding::Local {
                        cell: i_cell,
                        ty: scalar,
                    },
                )?;
            }
            this.lower_stmts(&for_expr.body.stmts)
        })?;
        self.loops.pop();
        if flow == Flow::Open {
            self.seal(self.cur, Terminator::Jmp(incr))?;
        }

        // Increment: i = i + 1, then back to the head.
        self.cur = incr;
        self.load(i_cell)?;
        self.push_word(1)?;
        self.push_instr(Add)?;
        self.store(i_cell)?;
        self.seal(self.cur, Terminator::Jmp(head))?;

        self.cur = exit;
        Ok(Flow::Open)
    }

    /// Lowers `loop { body }`, which falls through only where it breaks.
    fn lower_loop(&mut self, loop_expr: &syn::ExprLoop) -> syn::Result<Flow> {
        let head = self.builder.block(0);
        self.seal(self.cur, Terminator::Jmp(head))?;
        self.cur = head;

        self.loops.push(Loop {
            label: loop_label(&loop_expr.label),
            continue_to: head,
            break_to: None,
        });
        let flow = self.scoped(|this| this.lower_stmts(&loop_expr.body.stmts))?;
        let broke = self.loops.pop().and_then(|ctx| ctx.break_to);
        if flow == Flow::Open {
            self.seal(self.cur, Terminator::Jmp(head))?;
        }

        match broke {
            Some(exit) => {
                self.cur = exit;
                Ok(Flow::Open)
            }
            None => Ok(Flow::Diverged),
        }
    }

    /// Lowers an `if` used as a statement, tracking whether it falls through.
    fn lower_if_stmt(&mut self, if_expr: &syn::ExprIf) -> syn::Result<Flow> {
        self.lower_expr(&if_expr.cond, Scalar::BOOL, 0)?;
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
            if self.scoped(|this| this.lower_stmts(&if_expr.then_branch.stmts))? == Flow::Open {
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
        let then_exit = (self.scoped(|this| this.lower_stmts(&if_expr.then_branch.stmts))?
            == Flow::Open)
            .then_some(self.cur);

        self.cur = else_id;
        let else_exit = (self.lower_else_stmt(else_expr)? == Flow::Open).then_some(self.cur);

        match (then_exit, else_exit) {
            (None, None) => Ok(Flow::Diverged),
            _ => {
                let join = self.builder.block(0);
                for block in [then_exit, else_exit].into_iter().flatten() {
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
            syn::Expr::Block(block) => self.scoped(|this| this.lower_stmts(&block.block.stmts)),
            syn::Expr::If(if_expr) => self.lower_if_stmt(if_expr),
            other => Err(err(other, "an `else` must be a block or another `if`")),
        }
    }

    /// Lowers an `if` used as a value: reached arms leave a value, arms that
    /// leave do not.
    fn lower_if_value(&mut self, if_expr: &syn::ExprIf, expected: Scalar) -> syn::Result<Value> {
        let (_, else_expr) = if_expr
            .else_branch
            .as_ref()
            .ok_or_else(|| err(if_expr, "an `if` used as a value needs an `else`"))?;

        self.lower_expr(&if_expr.cond, Scalar::BOOL, 0)?;
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
                let join = self.builder.block(WORD_SIZE as u32);
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
        let Some((last, rest)) = block.stmts.split_last() else {
            return Err(err(block, "a block used as a value must produce one"));
        };
        self.scoped(|this| {
            if this.lower_stmts(rest)? == Flow::Diverged {
                return Ok(Value::Diverged);
            }
            match last {
                syn::Stmt::Expr(syn::Expr::Return(ret), _) => {
                    this.lower_return(ret)?;
                    Ok(Value::Diverged)
                }
                syn::Stmt::Expr(expr, None) => {
                    Ok(Value::Produced(this.lower_value(expr, expected)?))
                }
                other => Err(err(
                    other,
                    "a block used as a value must end with an expression",
                )),
            }
        })
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
            other => self.lower_expr(other, expected, 0),
        }
    }

    /// Lowers a plain expression, leaving its value on top of the stack.
    ///
    /// `depth` is the operand-stack depth in bytes on entry: `&&`/`||` split
    /// into blocks whose entry depth this pins.
    fn lower_expr(
        &mut self,
        expr: &syn::Expr,
        expected: Scalar,
        depth: u32,
    ) -> syn::Result<Scalar> {
        match expr {
            syn::Expr::Lit(lit) => self.lower_lit(lit, expected),
            syn::Expr::Path(path) => self.lower_path(path),
            syn::Expr::Paren(paren) => self.lower_expr(&paren.expr, expected, depth),
            syn::Expr::Unary(unary) => self.lower_unary(unary, expected, depth),
            syn::Expr::Binary(binary) => self.lower_binary(binary, expected, depth),
            syn::Expr::MethodCall(call) if call.method == "typed" => self.lower_field_typed(call),
            syn::Expr::Field(field) => self.lower_field_known(field),
            other => Err(err(other, "this expression is not supported yet")),
        }
    }

    /// Lowers `p.x.typed::<u64>()`: a field read whose first use names the
    /// field's type.
    ///
    /// The field's own type is not visible here, so its first use names one;
    /// later uses may go bare. The reference copy calls the real
    /// `Typed::typed`, which compiles only when the named type is exactly the
    /// field's own.
    fn lower_field_typed(&mut self, call: &syn::ExprMethodCall) -> syn::Result<Scalar> {
        let mut inner: &syn::Expr = &call.receiver;
        while let syn::Expr::Paren(paren) = inner {
            inner = &paren.expr;
        }
        let syn::Expr::Field(field) = inner else {
            return Err(err(call, "only a field names its type with `.typed()`"));
        };
        if !call.args.is_empty() {
            return Err(err(&call.args, "`.typed()` takes no arguments"));
        }
        let ty = typed_argument(call)
            .ok_or_else(|| err(call, "`.typed()` needs the type: `.typed::<u32>()`"))?;
        let scalar = Scalar::of(ty)
            .ok_or_else(|| err(ty, "a field must be typed as a scalar the machine can hold"))?;

        let (_, path) = field_path(field)?;
        let key = field_key(&path);
        let spelled = ty.to_token_stream().to_string();
        match self.field_types.get(&key) {
            Some((_, prior)) if *prior != spelled => {
                return Err(err(
                    call,
                    &format!("this field is already typed as `{prior}`"),
                ));
            }
            Some(_) => {}
            None => {
                self.field_types.insert(key, (scalar, spelled));
            }
        }
        self.lower_field_read(field, scalar)
    }

    /// Lowers a bare field read, legal once its type has been named.
    fn lower_field_known(&mut self, field: &syn::ExprField) -> syn::Result<Scalar> {
        let (root, path) = field_path(field)?;
        let scalar = self
            .field_types
            .get(&field_key(&path))
            .map(|(scalar, _)| *scalar)
            .ok_or_else(|| {
                let access = format!("{root}.{}", field_key(&path));
                err(
                    field,
                    &format!(
                        "the field's type cannot be resolved\n\
                         help: name it at the field's first use: `{access}.typed::<u32>()` \
                         (with the field's own type), importing `safetynet::Typed`"
                    ),
                )
            })?;
        self.lower_field_read(field, scalar)
    }

    /// Reads a field of the aggregate parameter as the asserted `scalar`.
    ///
    /// The offset and load width are resolved from the aggregate's layout at
    /// link time.
    fn lower_field_read(&mut self, field: &syn::ExprField, scalar: Scalar) -> syn::Result<Scalar> {
        let (root, path) = field_path(field)?;
        let ty = match &self.aggregate {
            Some((name, ty)) if *name == root => ty.clone(),
            _ => return Err(err(field, "only the aggregate parameter has fields")),
        };
        let hole = u32::try_from(self.field_refs.len())
            .map_err(|_| err(field, "too many field references"))?;
        self.field_refs.push(FieldRef { ty, path });

        // base + offset is the field's address; the load reads it at its width.
        self.push_base(Region::Input)?;
        self.body()?.field(hole);
        self.push_instr(Add)?;
        self.body()?.load_field(hole);
        self.normalize_load(scalar)?;
        Ok(scalar)
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
        match self.lookup(&name.to_string()) {
            Some(Binding::Param { offset, ty }) => {
                self.push_base(Region::Input)?;
                if offset != 0 {
                    self.push_word(u64::from(offset))?;
                    self.push_instr(Add)?;
                }
                self.push_instr(load(ty.width))?;
                self.normalize_load(ty)?;
                Ok(ty)
            }
            Some(Binding::Local { cell, ty }) => {
                self.load(cell)?;
                self.normalize_load(ty)?;
                Ok(ty)
            }
            None => Err(err(name, "no such parameter or local")),
        }
    }

    /// Lowers a unary operator.
    fn lower_unary(
        &mut self,
        unary: &syn::ExprUnary,
        expected: Scalar,
        depth: u32,
    ) -> syn::Result<Scalar> {
        match unary.op {
            // Negation is `0 - x`, which the wrapping subtraction handles.
            syn::UnOp::Neg(_) => {
                self.push_word(0)?;
                let ty = self.lower_expr(&unary.expr, expected, depth + WORD_SIZE as u32)?;
                self.push_instr(Sub)?;
                self.normalize(ty)?;
                Ok(ty)
            }
            syn::UnOp::Not(_) => {
                let ty = self.lower_expr(&unary.expr, expected, depth)?;
                if ty == Scalar::BOOL {
                    // Logical not: the flag is one exactly when the value was zero.
                    self.push_word(0)?;
                    self.push_instr(CmpEq)?;
                    Ok(Scalar::BOOL)
                } else {
                    self.push_instr(BitNot)?;
                    self.normalize(ty)?;
                    Ok(ty)
                }
            }
            other => Err(err(other, "this operator is not supported yet")),
        }
    }

    /// Lowers a binary operation and the operator on top of it.
    fn lower_binary(
        &mut self,
        binary: &syn::ExprBinary,
        expected: Scalar,
        depth: u32,
    ) -> syn::Result<Scalar> {
        if let Some(and) = logical(&binary.op) {
            return self.lower_and_or(binary, and, depth);
        }

        let op =
            plain(&binary.op).ok_or_else(|| err(binary, "this operator is not supported yet"))?;

        // The operands share a type of their own; a comparison's `bool` result is
        // not it. Infer it from whichever operand names one, falling back to the
        // result type for arithmetic and to `i32` for a bare comparison.
        let fallback = if op.is_comparison() {
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
        let (first, second) = if swaps(&binary.op) {
            (&*binary.right, &*binary.left)
        } else {
            (&*binary.left, &*binary.right)
        };
        let left = self.lower_expr(first, operand, depth)?;
        let right = self.lower_expr(second, operand, depth + WORD_SIZE as u32)?;
        if left.width != right.width {
            return Err(err(binary, "the operands must be the same width"));
        }
        self.emit_op(op, operand)
    }

    /// Lowers `&&` or `||`, evaluating the right side only when it is reached.
    ///
    /// The left value is branched on and popped; each path then leaves one
    /// boolean, so the join is entered one word deeper than the operator began.
    fn lower_and_or(
        &mut self,
        binary: &syn::ExprBinary,
        is_and: bool,
        depth: u32,
    ) -> syn::Result<Scalar> {
        self.lower_expr(&binary.left, Scalar::BOOL, depth)?;
        let rhs = self.builder.block(depth);
        let shortcut = self.builder.block(depth);
        let join = self.builder.block(depth + WORD_SIZE as u32);

        // `&&` runs the right side when the left is true; `||`, when it is false.
        let branch = if is_and {
            Terminator::Br {
                then: rhs,
                els: shortcut,
            }
        } else {
            Terminator::Br {
                then: shortcut,
                els: rhs,
            }
        };
        self.seal(self.cur, branch)?;

        self.cur = rhs;
        self.lower_expr(&binary.right, Scalar::BOOL, depth)?;
        self.seal(self.cur, Terminator::Jmp(join))?;

        // The short-circuit result: `&&` yields false, `||` yields true.
        self.cur = shortcut;
        self.push_word(u64::from(!is_and))?;
        self.seal(self.cur, Terminator::Jmp(join))?;

        self.cur = join;
        Ok(Scalar::BOOL)
    }

    /// The type an expression will have, when that is known without lowering it.
    ///
    /// A literal gives nothing back, so a sibling operand's type drives the two.
    fn peek(&self, expr: &syn::Expr) -> Option<Scalar> {
        match expr {
            syn::Expr::Path(path) => {
                let name = path.path.get_ident()?;
                Some(match self.lookup(&name.to_string())? {
                    Binding::Param { ty, .. } | Binding::Local { ty, .. } => ty,
                })
            }
            syn::Expr::Paren(paren) => self.peek(&paren.expr),
            syn::Expr::Unary(unary) => self.peek(&unary.expr),
            syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Bool(_),
                ..
            }) => Some(Scalar::BOOL),
            syn::Expr::MethodCall(call) if call.method == "typed" => {
                Scalar::of(typed_argument(call)?)
            }
            syn::Expr::Field(field) => {
                let (_, path) = field_path(field).ok()?;
                self.field_types
                    .get(&field_key(&path))
                    .map(|(scalar, _)| *scalar)
            }
            syn::Expr::Binary(binary) if logical(&binary.op).is_some() => Some(Scalar::BOOL),
            syn::Expr::Binary(binary) => match plain(&binary.op) {
                Some(op) if op.is_comparison() => Some(Scalar::BOOL),
                _ => self.peek(&binary.left).or_else(|| self.peek(&binary.right)),
            },
            _ => None,
        }
    }

    /// Emits the opcode an operator lowers to, and reports its result type.
    fn emit_op(&mut self, op: Op, ty: Scalar) -> syn::Result<Scalar> {
        match op {
            Op::Add => self.push_instr(Add)?,
            Op::Sub => self.push_instr(Sub)?,
            Op::Mul => self.push_instr(Mul)?,
            Op::Div => self.push_instr(signed(ty, Instr::from(SDiv), Div.into()))?,
            Op::Rem => self.push_instr(signed(ty, SRem.into(), Rem.into()))?,
            Op::And => self.push_instr(And)?,
            Op::Or => self.push_instr(Or)?,
            Op::Xor => self.push_instr(Xor)?,
            Op::Shl => self.push_instr(Shl)?,
            Op::Shr => self.push_instr(signed(ty, Sar.into(), Shr.into()))?,
            Op::Eq => return self.cmp(CmpEq.into()),
            Op::Lt => return self.cmp(signed(ty, CmpSLt.into(), CmpLt.into())),
            Op::Le => return self.cmp(signed(ty, CmpSLe.into(), CmpLe.into())),
            Op::Ne => {
                self.push_instr(CmpEq)?;
                // Negate the flag: it equals zero exactly when the values differed.
                self.push_word(0)?;
                self.push_instr(CmpEq)?;
                return Ok(Scalar::BOOL);
            }
        }
        self.normalize(ty)?;
        Ok(ty)
    }

    /// Puts a freshly loaded value at rest: loads zero-extend, which is a
    /// narrow signed type's resting form only after sign-extension.
    fn normalize_load(&mut self, ty: Scalar) -> syn::Result<()> {
        if ty.signed {
            self.normalize(ty)
        } else {
            Ok(())
        }
    }

    /// Restores a value's resting form: the machine computes at the word
    /// width, so a narrow result is masked back down when unsigned and
    /// sign-extended when signed. Word-wide values already rest as they are.
    fn normalize(&mut self, ty: Scalar) -> syn::Result<()> {
        if ty.width == Width::U64 {
            return Ok(());
        }
        let bits = Word::from(ty.width.bits());
        if ty.signed {
            let shift = (WORD_SIZE as u64) * 8 - bits;
            self.push_word(shift)?;
            self.push_instr(Shl)?;
            self.push_word(shift)?;
            self.push_instr(Sar)?;
        } else {
            self.push_word(Word::MAX >> (64 - bits))?;
            self.push_instr(And)?;
        }
        Ok(())
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
    fn load(&mut self, cell: CellId) -> syn::Result<()> {
        self.body()?.load(cell);
        Ok(())
    }

    /// Appends a store into a frame cell.
    fn store(&mut self, cell: CellId) -> syn::Result<()> {
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

impl Op {
    /// Whether the operator compares, yielding a boolean.
    fn is_comparison(self) -> bool {
        matches!(self, Op::Eq | Op::Ne | Op::Lt | Op::Le)
    }
}

/// The plain operator a binary expression uses, if it is one that is supported.
fn plain(op: &syn::BinOp) -> Option<Op> {
    use syn::BinOp;
    Some(match op {
        BinOp::Add(_) => Op::Add,
        BinOp::Sub(_) => Op::Sub,
        BinOp::Mul(_) => Op::Mul,
        BinOp::Div(_) => Op::Div,
        BinOp::Rem(_) => Op::Rem,
        BinOp::BitAnd(_) => Op::And,
        BinOp::BitOr(_) => Op::Or,
        BinOp::BitXor(_) => Op::Xor,
        BinOp::Shl(_) => Op::Shl,
        BinOp::Shr(_) => Op::Shr,
        BinOp::Eq(_) => Op::Eq,
        BinOp::Ne(_) => Op::Ne,
        BinOp::Lt(_) => Op::Lt,
        BinOp::Le(_) => Op::Le,
        BinOp::Gt(_) => Op::Lt,
        BinOp::Ge(_) => Op::Le,
        _ => return None,
    })
}

/// The plain operator a compound assignment applies, if it is a supported one.
fn compound(op: &syn::BinOp) -> Option<Op> {
    use syn::BinOp;
    Some(match op {
        BinOp::AddAssign(_) => Op::Add,
        BinOp::SubAssign(_) => Op::Sub,
        BinOp::MulAssign(_) => Op::Mul,
        BinOp::DivAssign(_) => Op::Div,
        BinOp::RemAssign(_) => Op::Rem,
        BinOp::BitAndAssign(_) => Op::And,
        BinOp::BitOrAssign(_) => Op::Or,
        BinOp::BitXorAssign(_) => Op::Xor,
        BinOp::ShlAssign(_) => Op::Shl,
        BinOp::ShrAssign(_) => Op::Shr,
        _ => return None,
    })
}

/// Whether a binary operator swaps its operands (`>` and `>=`).
fn swaps(op: &syn::BinOp) -> bool {
    matches!(op, syn::BinOp::Gt(_) | syn::BinOp::Ge(_))
}

/// The short-circuit operators: `Some(true)` for `&&`, `Some(false)` for `||`.
fn logical(op: &syn::BinOp) -> Option<bool> {
    match op {
        syn::BinOp::And(_) => Some(true),
        syn::BinOp::Or(_) => Some(false),
        _ => None,
    }
}

/// The block a value arm exits from, if it produced a value.
fn value_exit(value: Value, exit: BlockId) -> Option<(BlockId, Scalar)> {
    match value {
        Value::Produced(scalar) => Some((exit, scalar)),
        Value::Diverged => None,
    }
}

/// Whether an expression can stand as a block's produced value.
fn is_value_expr(expr: &syn::Expr) -> bool {
    matches!(
        expr,
        syn::Expr::Lit(_)
            | syn::Expr::Path(_)
            | syn::Expr::Paren(_)
            | syn::Expr::Unary(_)
            | syn::Expr::Binary(_)
            | syn::Expr::Field(_)
            | syn::Expr::MethodCall(_)
            | syn::Expr::Block(_)
    )
}

/// Picks the signed or unsigned opcode by the operand type.
fn signed(ty: Scalar, when_signed: Instr, when_unsigned: Instr) -> Instr {
    if ty.signed {
        when_signed
    } else {
        when_unsigned
    }
}

/// The `T` of `.typed::<T>()`, when the turbofish names exactly one type.
fn typed_argument(call: &syn::ExprMethodCall) -> Option<&syn::Type> {
    let turbofish = call.turbofish.as_ref()?;
    match turbofish.args.first() {
        Some(syn::GenericArgument::Type(ty)) if turbofish.args.len() == 1 => Some(ty),
        _ => None,
    }
}

/// The name of a loop's label, without its tick.
fn loop_label(label: &Option<syn::Label>) -> Option<String> {
    label.as_ref().map(|label| label.name.ident.to_string())
}

/// A field path's map key: its dotted spelling.
fn field_key(path: &[syn::Ident]) -> String {
    path.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(".")
}

/// The root name and dotted path of a field access, `p.a.b` → `(p, [a, b])`.
fn field_path(field: &syn::ExprField) -> syn::Result<(String, Vec<syn::Ident>)> {
    let member = match &field.member {
        syn::Member::Named(ident) => ident.clone(),
        syn::Member::Unnamed(_) => {
            return Err(err(&field.member, "tuple fields are not supported"));
        }
    };
    match &*field.base {
        syn::Expr::Field(inner) => {
            let (root, mut path) = field_path(inner)?;
            path.push(member);
            Ok((root, path))
        }
        syn::Expr::Path(path) => {
            let root = path
                .path
                .get_ident()
                .ok_or_else(|| err(path, "a field access must start from a parameter"))?;
            Ok((root.to_string(), vec![member]))
        }
        other => Err(err(other, "a field access must start from a parameter")),
    }
}

/// The element type of a `for` range: whichever bound names a type, else `i32`.
/// The scalar a literal bound's suffix spells, if it has one.
fn suffix_scalar(expr: &syn::Expr) -> Option<syn::Result<Scalar>> {
    match expr {
        syn::Expr::Paren(paren) => suffix_scalar(&paren.expr),
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Int(int),
            ..
        }) if !int.suffix().is_empty() => Some(
            Scalar::of_name(int.suffix())
                .ok_or_else(|| err(int, "the range's type is not a scalar the machine can hold")),
        ),
        _ => None,
    }
}

/// The name a `for` loop binds, or `None` for `_`.
fn for_var(for_expr: &syn::ExprForLoop) -> syn::Result<Option<String>> {
    match &*for_expr.pat {
        syn::Pat::Wild(_) => Ok(None),
        syn::Pat::Ident(pat) if pat.subpat.is_none() => Ok(Some(pat.ident.to_string())),
        other => Err(err(other, "a `for` binding must be a name or `_`")),
    }
}

/// The start and end of a `for` loop's exclusive range.
fn range_bounds(for_expr: &syn::ExprForLoop) -> syn::Result<(&syn::Expr, &syn::Expr)> {
    let range = match &*for_expr.expr {
        syn::Expr::Range(range) => range,
        other => return Err(err(other, "a `for` loop must iterate a range `a..b`")),
    };
    if !matches!(range.limits, syn::RangeLimits::HalfOpen(_)) {
        return Err(err(range, "an inclusive `..=` range is not supported yet"));
    }
    let start = range
        .start
        .as_ref()
        .ok_or_else(|| err(range, "the range needs a start"))?;
    let end = range
        .end
        .as_ref()
        .ok_or_else(|| err(range, "the range needs an end"))?;
    Ok((start, end))
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
