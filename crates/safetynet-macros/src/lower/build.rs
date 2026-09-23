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

use super::intrinsics::{self, Handler, Place, Receiver, Source, Table};
use super::ty::{self, Scalar};
use crate::backend::{FieldRef, Rodata};

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
    /// The constant tables the body declared, laid out in `.rodata`.
    pub(crate) rodata: Rodata,
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
    /// A `const` or `static` item of the body, folded to the word it rests as.
    Const { value: u64, ty: Scalar },
    /// A `const` or `static` table of the body, placed in `.rodata`.
    Table(Table),
}

/// A byte region located in the frame: where its content starts, absolutely,
/// and how many bytes it holds once checked against the input.
#[derive(Clone, Copy)]
pub(super) struct RegionCells {
    base: CellId,
    pub(super) len: CellId,
}

/// Why a `for` iterable was refused.
const FOR_ITERABLE: &str = "a `for` loop must iterate a range `a..b` or the `.iter()` of a byte region or a constant table";

/// Why a path with more than one segment was refused: the macro sees the
/// function's tokens and nothing else, so `Self::X` or `m::X` names something
/// it cannot read.
const OUTSIDE_PATH: &str = "a path to an item outside the function is not lowered; \
                            declare the const inside the body";

/// Why a const's type was refused.
const CONST_TYPE: &str =
    "a const must be a scalar the machine can hold, an array of them, or a `&str`";

/// Why a const's initializer was refused.
const CONST_INIT: &str = "a const's initializer must be a literal, an array of literals, or \
                          `[literal; N]`; the machine cannot evaluate it";

/// Why a method receiver was refused.
const NOT_A_RECEIVER: &str = "only the aggregate parameter, one of its fields, or a constant table \
                              has methods the machine lowers";

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
        rodata: Rodata::default(),
        field_types: HashMap::new(),
        aggregate: aggregate.clone(),
        cur: entry,
        ret,
    };
    lowerer.lower_fn_body(&func.block)?;
    let field_refs = lowerer.field_refs;
    let rodata = lowerer.rodata;
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
        rodata,
        param_offsets,
        input_size,
        aggregate: aggregate.map(|(_, ty)| ty),
        ret: ret_ty.clone(),
    })
}

/// The state threaded through lowering a body.
pub(super) struct Lowerer {
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
    /// The constant tables, appended as their items are lowered.
    rodata: Rodata,
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
            syn::Stmt::Item(item) => {
                self.lower_item(item)?;
                Ok(Flow::Open)
            }
            other => Err(err(other, "this statement is not supported yet")),
        }
    }

    /// Lowers an item declared inside the body. Only a constant can be: the
    /// machine has no globals, so a `static mut` would not keep its value
    /// between runs the way the reference copy's does.
    fn lower_item(&mut self, item: &syn::Item) -> syn::Result<()> {
        match item {
            syn::Item::Const(item) => self.lower_const(&item.ident, &item.ty, &item.expr),
            syn::Item::Static(item) => match item.mutability {
                syn::StaticMutability::None => self.lower_const(&item.ident, &item.ty, &item.expr),
                _ => Err(err(
                    &item.mutability,
                    "the machine has no globals: a `static mut` could not keep its value between runs",
                )),
            },
            other => Err(err(
                other,
                "only `const` and `static` items are lowered inside a body",
            )),
        }
    }

    /// Lowers `const NAME: T = init;`, folding a scalar to the word it rests
    /// as, so every use is an immediate.
    ///
    /// The initializer has to be a literal: the macro will not evaluate an
    /// expression the compiler would, and a value another const names is a
    /// reference the reference copy would have to have resolved first.
    fn lower_const(
        &mut self,
        name: &syn::Ident,
        ty: &syn::Type,
        init: &syn::Expr,
    ) -> syn::Result<()> {
        if let Some(scalar) = Scalar::held(ty) {
            let value = const_scalar(init)?.ok_or_else(|| err(init, CONST_INIT))?;
            return self.bind(
                name.to_string(),
                Binding::Const {
                    value: at_rest(value, scalar),
                    ty: scalar,
                },
            );
        }
        let elem = table_type(ty).ok_or_else(|| err(ty, CONST_TYPE))?;
        let values = const_elements(init)?
            .ok_or_else(|| err(init, CONST_INIT))?
            .into_iter()
            .map(|value| at_rest(value, elem))
            .collect::<Vec<_>>();
        let len = u32::try_from(values.len())
            .map_err(|_| err(init, "the constants do not fit in .rodata"))?;
        let off = self
            .rodata
            .push(elem.width, values)
            .ok_or_else(|| err(init, "the constants do not fit in .rodata"))?;
        self.bind(name.to_string(), Binding::Table(Table { off, len, elem }))
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
            Some(Binding::Const { .. } | Binding::Table(_)) => {
                Err(err(name, "a constant cannot be assigned to"))
            }
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

    /// Lowers `for pat in iterable { body }`: a counted range, or a walk over
    /// the bytes of a region.
    fn lower_for(&mut self, for_expr: &syn::ExprForLoop) -> syn::Result<Flow> {
        match &*for_expr.expr {
            syn::Expr::Range(range) => self.lower_for_range(for_expr, range),
            syn::Expr::MethodCall(_) => match self.classify(&for_expr.expr)? {
                Receiver::Walk(source) => self.lower_for_walk(for_expr, &source),
                Receiver::Place(_) | Receiver::Table(_) => Err(err(&for_expr.expr, FOR_ITERABLE)),
            },
            other => Err(err(other, FOR_ITERABLE)),
        }
    }

    /// Lowers `for i in a..b { body }` as a counted loop.
    ///
    /// The end bound is snapshotted so mutating it in the body cannot change the
    /// count.
    fn lower_for_range(
        &mut self,
        for_expr: &syn::ExprForLoop,
        range: &syn::ExprRange,
    ) -> syn::Result<Flow> {
        let (start, end) = range_bounds(range)?;
        let var = for_var(for_expr)?;
        let scalar = self.range_scalar(start, end)?;

        let i_cell = self.cell(scalar.width, &for_expr.pat)?;
        let end_cell = self.cell(scalar.width, &for_expr.pat)?;

        // i = start; end = b. Both bounds are evaluated outside the variable's
        // scope, so `for i in i..n` reads the outer `i`.
        self.lower_expr(start, scalar, 0)?;
        self.store(i_cell)?;
        self.lower_expr(end, scalar, 0)?;
        self.store(end_cell)?;

        self.counted_loop(for_expr, i_cell, end_cell, scalar, |this| {
            if let Some(name) = var {
                this.bind(
                    name,
                    Binding::Local {
                        cell: i_cell,
                        ty: scalar,
                    },
                )?;
            }
            Ok(())
        })
    }

    /// Lowers `for x in source.iter() { body }`, walking a byte region's
    /// content from its header or a constant table from where it was placed.
    ///
    /// Each element is copied into a cell the binding reads, so the body sees
    /// a `T` where the reference copy sees a `&T`.
    fn lower_for_walk(
        &mut self,
        for_expr: &syn::ExprForLoop,
        source: &Source,
    ) -> syn::Result<Flow> {
        let var = for_var(for_expr)?;
        let (region, elem) = match source {
            Source::Place(place) => (self.load_region(place, 0, &for_expr.expr)?, Scalar::U8),
            Source::Table(table) => (self.load_table(table, &for_expr.expr)?, table.elem),
        };
        let i_cell = self.cell(Width::U32, &for_expr.pat)?;
        let elem_cell = self.cell(elem.width, &for_expr.pat)?;

        self.push_word(0)?;
        self.store(i_cell)?;

        self.counted_loop(for_expr, i_cell, region.len, Scalar::U32, |this| {
            this.load(region.base)?;
            this.load(i_cell)?;
            this.stride(elem)?;
            this.push_instr(Add)?;
            this.push_instr(load(elem.width))?;
            this.store(elem_cell)?;
            if let Some(name) = var {
                this.bind(
                    name,
                    Binding::Local {
                        cell: elem_cell,
                        ty: elem,
                    },
                )?;
            }
            Ok(())
        })
    }

    /// Scales the index on top of the stack to a byte offset: nothing for a
    /// byte, a shift for the wider elements.
    fn stride(&mut self, elem: Scalar) -> syn::Result<()> {
        let shift = elem.size().trailing_zeros();
        if shift > 0 {
            self.push_word(u64::from(shift))?;
            self.push_instr(Shl)?;
        }
        Ok(())
    }

    /// Locates a constant table: its base is `.rodata` plus where it was
    /// placed, and its length was fixed when it was declared. Both go into
    /// cells so a walk over it runs the same loop a region's does.
    fn load_table(
        &mut self,
        table: &Table,
        node: impl ToTokens + Copy,
    ) -> syn::Result<RegionCells> {
        let base = self.cell(Width::U64, node)?;
        let len = self.cell(Width::U32, node)?;

        self.push_table(table)?;
        self.store(base)?;
        self.push_word(u64::from(table.len))?;
        self.store(len)?;

        Ok(RegionCells { base, len })
    }

    /// Pushes the address a constant table starts at.
    fn push_table(&mut self, table: &Table) -> syn::Result<()> {
        self.push_base(Region::Rodata)?;
        if table.off != 0 {
            self.push_word(u64::from(table.off))?;
            self.push_instr(Add)?;
        }
        Ok(())
    }

    /// The loop every `for` becomes: while `i < end`, run `prologue` then the
    /// body, then `i += 1`. `continue` targets the increment, so the counter
    /// still advances.
    fn counted_loop(
        &mut self,
        for_expr: &syn::ExprForLoop,
        i_cell: CellId,
        end_cell: CellId,
        scalar: Scalar,
        prologue: impl FnOnce(&mut Self) -> syn::Result<()>,
    ) -> syn::Result<Flow> {
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
            prologue(this)?;
            this.lower_stmts(&for_expr.body.stmts)
        })?;
        self.loops.pop();
        if flow == Flow::Open {
            self.seal(self.cur, Terminator::Jmp(incr))?;
        }

        self.cur = incr;
        self.load(i_cell)?;
        self.push_word(1)?;
        self.push_instr(Add)?;
        self.store(i_cell)?;
        self.seal(self.cur, Terminator::Jmp(head))?;

        self.cur = exit;
        Ok(Flow::Open)
    }

    /// Locates a region's content: the absolute address it starts at and its
    /// length, both read from the header the host wrote.
    ///
    /// The header is data, so its length is checked against where the input
    /// really ends before anything is read through it; a header that points
    /// past the input describes an empty region. The blocks this opens sit at
    /// `depth`, so it can run inside an expression.
    pub(super) fn load_region(
        &mut self,
        place: &Place,
        depth: u32,
        node: impl ToTokens + Copy,
    ) -> syn::Result<RegionCells> {
        let base = self.cell(Width::U64, node)?;
        let len = self.cell(Width::U32, node)?;

        let off_hole = self.region_hole(place, "off", node)?;
        self.read_hole(off_hole)?;
        self.push_base(Region::Input)?;
        self.push_instr(Add)?;
        self.store(base)?;

        let len_hole = self.region_hole(place, "len", node)?;
        self.read_hole(len_hole)?;
        self.store(len)?;

        // base + len <= end of .input, or the region is empty.
        self.load(base)?;
        self.load(len)?;
        self.push_instr(Add)?;
        self.push_base(Region::Input)?;
        self.body()?.len(Region::Input);
        self.push_instr(Add)?;
        self.push_instr(CmpLe)?;
        let clamp = self.builder.block(depth);
        let join = self.builder.block(depth);
        self.seal(
            self.cur,
            Terminator::Br {
                then: join,
                els: clamp,
            },
        )?;
        self.cur = clamp;
        self.push_word(0)?;
        self.store(len)?;
        self.seal(self.cur, Terminator::Jmp(join))?;
        self.cur = join;

        Ok(RegionCells { base, len })
    }

    /// What a method receiver is: a place in the aggregate parameter, or a
    /// walk a method chain started over one.
    ///
    /// Each link of a chain is looked up in the method table by the shape of
    /// what it is called on, so `.as_bytes().iter().copied()` resolves link by
    /// link and an unknown method is refused at the link that names it.
    fn classify(&self, expr: &syn::Expr) -> syn::Result<Receiver> {
        match unparen(expr) {
            syn::Expr::Field(field) => Ok(Receiver::Place(self.place_of(field)?)),
            syn::Expr::Path(path) => match (&self.aggregate, path.path.get_ident()) {
                (Some((name, ty)), Some(root)) if *root == name => Ok(Receiver::Place(Place {
                    ty: ty.clone(),
                    path: Vec::new(),
                })),
                (_, Some(root)) => match self.lookup(&root.to_string()) {
                    Some(Binding::Table(table)) => Ok(Receiver::Table(table)),
                    Some(_) => Err(err(path, NOT_A_RECEIVER)),
                    None => Err(undeclared(root)),
                },
                _ => Err(err(path, OUTSIDE_PATH)),
            },
            syn::Expr::MethodCall(call) => {
                let receiver = self.classify(&call.receiver)?;
                match self.method(&receiver, call)?.handler {
                    Handler::Walk => match receiver {
                        Receiver::Place(place) => Ok(Receiver::Walk(Source::Place(place))),
                        Receiver::Table(table) => Ok(Receiver::Walk(Source::Table(table))),
                        Receiver::Walk(_) => Err(internal(call, "a walk started on a walk")),
                    },
                    Handler::Same => Ok(receiver),
                    Handler::Value { .. } => Err(err(
                        call,
                        &format!("`.{}()` is a value, which has no methods", call.method),
                    )),
                }
            }
            other => Err(err(other, NOT_A_RECEIVER)),
        }
    }

    /// The table entry for `call` on `receiver`, with the call's shape checked
    /// against it: no arguments ever, and a turbofish exactly when the entry
    /// asks for one.
    fn method(
        &self,
        receiver: &Receiver,
        call: &syn::ExprMethodCall,
    ) -> syn::Result<&'static intrinsics::Method> {
        let name = call.method.to_string();
        let method = intrinsics::find(receiver.kind(), &name).ok_or_else(|| {
            err(
                &call.method,
                &format!(
                    "`.{name}()` is not lowered on {}",
                    receiver.kind().describe()
                ),
            )
        })?;
        if !call.args.is_empty() {
            return Err(err(&call.args, &format!("`.{name}()` takes no arguments")));
        }
        match (method.turbofish, intrinsics::typed_argument(call)) {
            (true, None) => Err(err(
                call,
                &format!("`.{name}()` needs the type: `.{name}::<u32>()`"),
            )),
            (false, _) if call.turbofish.is_some() => {
                Err(err(call, &format!("`.{name}()` takes no type argument")))
            }
            _ => Ok(method),
        }
    }

    /// Lowers a method call in value position through the table.
    fn lower_method(&mut self, call: &syn::ExprMethodCall, depth: u32) -> syn::Result<Scalar> {
        let receiver = self.classify(&call.receiver)?;
        match self.method(&receiver, call)?.handler {
            Handler::Value { lower, .. } => lower(self, receiver, call, depth),
            Handler::Walk | Handler::Same => Err(err(
                call,
                &format!(
                    "`.{}()` starts a walk, which only a `for` can consume",
                    call.method
                ),
            )),
        }
    }

    /// The place a field access names, which must be in the aggregate parameter.
    fn place_of(&self, field: &syn::ExprField) -> syn::Result<Place> {
        let (root, path) = field_path(field)?;
        match &self.aggregate {
            Some((name, ty)) if *name == root => Ok(Place {
                ty: ty.clone(),
                path,
            }),
            _ => Err(err(field, "only the aggregate parameter has fields")),
        }
    }

    /// A hole for the header field `leaf` of the region at `place`.
    fn region_hole(&mut self, place: &Place, leaf: &str, node: impl ToTokens) -> syn::Result<u32> {
        let mut path = place.path.clone();
        path.push(syn::Ident::new(leaf, Span::call_site()));
        self.field_hole(place.ty.clone(), path, node)
    }

    /// Adds a frame cell, or refuses when the frame cannot hold it.
    fn cell(&mut self, width: Width, node: impl ToTokens) -> syn::Result<CellId> {
        self.builder
            .cell(width)
            .ok_or_else(|| err(node, "the frame is too large"))
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
            syn::Expr::MethodCall(call) => self.lower_method(call, depth),
            syn::Expr::Cast(cast) => self.lower_cast(cast, depth),
            syn::Expr::Field(field) => self.lower_field_known(field),
            syn::Expr::Index(index) => self.lower_index(index, depth),
            other => Err(err(other, "this expression is not supported yet")),
        }
    }

    /// Lowers `x[i]` on a constant table or a byte region.
    fn lower_index(&mut self, index: &syn::ExprIndex, depth: u32) -> syn::Result<Scalar> {
        match self.classify(&index.expr)? {
            Receiver::Table(table) => self.lower_table_index(&table, index, depth),
            Receiver::Place(place) => self.lower_region_index(&place, index, depth),
            Receiver::Walk(_) => Err(err(&index.expr, "a walk cannot be indexed")),
        }
    }

    /// Lowers `TABLE[i]`: an index known at expansion folds to the element's
    /// address; one computed at run time is checked against the table's
    /// length first and aborts past it, where the reference copy panics.
    fn lower_table_index(
        &mut self,
        table: &Table,
        index: &syn::ExprIndex,
        depth: u32,
    ) -> syn::Result<Scalar> {
        let elem = table.elem;

        if let Some(at) = self.const_index(&index.index)? {
            if at >= u64::from(table.len) {
                return Err(err(
                    &index.index,
                    &format!(
                        "index {at} is out of bounds for a constant of {} elements",
                        table.len
                    ),
                ));
            }
            let off = u64::from(table.off) + at * u64::from(elem.size());
            self.push_base(Region::Rodata)?;
            if off != 0 {
                self.push_word(off)?;
                self.push_instr(Add)?;
            }
            self.push_instr(load(elem.width))?;
            self.normalize_load(elem)?;
            return Ok(elem);
        }

        let len = table.len;
        let at = self.checked_index(&index.index, depth, |this| this.push_word(u64::from(len)))?;
        self.push_table(table)?;
        self.load(at)?;
        self.stride(elem)?;
        self.push_instr(Add)?;
        self.push_instr(load(elem.width))?;
        self.normalize_load(elem)?;
        Ok(elem)
    }

    /// Lowers `region[i]`: the header is read and clamped as for a walk, then
    /// the index is checked against that length. Nothing folds here, since
    /// the length is the host's: a literal past the end is a panic in the
    /// reference copy and an abort in the machine.
    fn lower_region_index(
        &mut self,
        place: &Place,
        index: &syn::ExprIndex,
        depth: u32,
    ) -> syn::Result<Scalar> {
        let region = self.load_region(place, depth, &index.expr)?;
        let at = self.checked_index(&index.index, depth, |this| this.load(region.len))?;
        self.load(region.base)?;
        self.load(at)?;
        self.push_instr(Add)?;
        self.push_instr(Ld8)?;
        Ok(Scalar::U8)
    }

    /// Lowers a run-time index into a cell and guards it: `at < len`, with
    /// `len` pushed by `push_len`, or the block aborts. Returns the cell, with
    /// the current block the one where the index is known to be in bounds.
    fn checked_index(
        &mut self,
        index: &syn::Expr,
        depth: u32,
        push_len: impl FnOnce(&mut Self) -> syn::Result<()>,
    ) -> syn::Result<CellId> {
        // Rust indexes with a `usize`, which the machine holds as a word; a
        // narrower index is a type error in the reference copy too.
        let ty = self.lower_expr(index, Scalar::U64, depth)?;
        if ty != Scalar::U64 {
            return Err(err(
                index,
                "an index must be a `usize`: cast it with `as usize`",
            ));
        }
        // The index is needed twice, for the check and the address, and there
        // is no `dup`: a cell holds it.
        let at = self.cell(Width::U64, index)?;
        self.store(at)?;
        self.load(at)?;
        push_len(self)?;
        self.push_instr(CmpLt)?;
        let ok = self.builder.block(depth);
        let abort = self.builder.block(depth);
        self.seal(
            self.cur,
            Terminator::Br {
                then: ok,
                els: abort,
            },
        )?;
        self.seal(abort, Terminator::Abort)?;
        self.cur = ok;
        Ok(at)
    }

    /// The index an expression fixes at expansion: a literal, or a const of
    /// the body.
    fn const_index(&self, expr: &syn::Expr) -> syn::Result<Option<u64>> {
        Ok(match unparen(expr) {
            syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Int(int),
                ..
            }) => Some(int.base10_parse()?),
            syn::Expr::Path(path) => match path.path.get_ident() {
                Some(name) => match self.lookup(&name.to_string()) {
                    Some(Binding::Const { value, .. }) => Some(value),
                    _ => None,
                },
                None => None,
            },
            _ => None,
        })
    }

    /// Records the type a field's first use named, refusing a second name
    /// that disagrees with it.
    pub(super) fn name_field_type(
        &mut self,
        place: &Place,
        scalar: Scalar,
        spelled: String,
        node: impl ToTokens,
    ) -> syn::Result<()> {
        let key = field_key(&place.path);
        match self.field_types.get(&key) {
            Some((_, prior)) if *prior != spelled => Err(err(
                node,
                &format!("this field is already typed as `{prior}`"),
            )),
            Some(_) => Ok(()),
            None => {
                self.field_types.insert(key, (scalar, spelled));
                Ok(())
            }
        }
    }

    /// Lowers a bare field read, legal once its type has been named.
    fn lower_field_known(&mut self, field: &syn::ExprField) -> syn::Result<Scalar> {
        let place = self.place_of(field)?;
        let scalar = self
            .field_types
            .get(&field_key(&place.path))
            .map(|(scalar, _)| *scalar)
            .ok_or_else(|| {
                let access = format!("{}.{}", self.aggregate_name(), field_key(&place.path));
                err(
                    field,
                    &format!(
                        "the field's type cannot be resolved\n\
                         help: name it at the field's first use: `{access}.typed::<u32>()` \
                         (with the field's own type), importing `safetynet::Typed`"
                    ),
                )
            })?;
        self.read_place(&place, scalar, field)
    }

    /// The aggregate parameter's name, for a message that spells out a fix.
    fn aggregate_name(&self) -> &str {
        self.aggregate
            .as_ref()
            .map_or("p", |(name, _)| name.as_str())
    }

    /// Reads a field of the aggregate parameter as the asserted `scalar`.
    ///
    /// The offset and load width are resolved from the aggregate's layout at
    /// link time.
    pub(super) fn read_place(
        &mut self,
        place: &Place,
        scalar: Scalar,
        node: impl ToTokens,
    ) -> syn::Result<Scalar> {
        let hole = self.field_hole(place.ty.clone(), place.path.clone(), node)?;
        self.read_hole(hole)?;
        self.normalize_load(scalar)?;
        Ok(scalar)
    }

    /// Records a field reference for the linker, returning the hole naming it.
    fn field_hole(
        &mut self,
        ty: syn::Type,
        path: Vec<syn::Ident>,
        node: impl ToTokens,
    ) -> syn::Result<u32> {
        let hole = u32::try_from(self.field_refs.len())
            .map_err(|_| err(node, "too many field references"))?;
        self.field_refs.push(FieldRef { ty, path });
        Ok(hole)
    }

    /// Reads the field a hole names: base + offset is its address, and the
    /// load reads it at the width the layout gives it.
    fn read_hole(&mut self, hole: u32) -> syn::Result<()> {
        self.push_base(Region::Input)?;
        self.body()?.field(hole);
        self.push_instr(Add)?;
        self.body()?.load_field(hole);
        Ok(())
    }

    /// Lowers `e as T` between machine scalars.
    ///
    /// A value rests sign- or zero-extended to the word, so a cast is the
    /// target's own normalization: masking narrows, shifting re-signs, and a
    /// widening from unsigned or between signed types is already at rest.
    fn lower_cast(&mut self, cast: &syn::ExprCast, depth: u32) -> syn::Result<Scalar> {
        let target = Scalar::held(&cast.ty)
            .ok_or_else(|| err(&cast.ty, "a cast must target a scalar the machine can hold"))?;
        let source = self.lower_expr(&cast.expr, target, depth)?;
        let at_rest =
            source == target || (source.width < target.width && (!source.signed || target.signed));
        if !at_rest {
            self.normalize(target)?;
        }
        Ok(target)
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
            syn::Lit::Byte(byte) => {
                self.push_word(byte.value().into())?;
                Ok(Scalar::U8)
            }
            other => Err(err(other, "only integer and bool literals are supported")),
        }
    }

    /// Reads a parameter from `.input` or a local from its cell.
    fn lower_path(&mut self, path: &syn::ExprPath) -> syn::Result<Scalar> {
        let name = path
            .path
            .get_ident()
            .ok_or_else(|| err(path, OUTSIDE_PATH))?;
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
            Some(Binding::Const { value, ty }) => {
                self.push_word(value)?;
                Ok(ty)
            }
            Some(Binding::Table(_)) => Err(err(
                name,
                "a constant table is a place: walk it with `.iter()`, index it, or take its `.len()`",
            )),
            None => Err(undeclared(name)),
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
            // The only reference the subset makes is a region loop's `&u8`
            // binding, whose cell already holds the byte. The reference copy
            // refuses `*` on anything else.
            syn::UnOp::Deref(_) => match unparen(&unary.expr) {
                syn::Expr::Path(path) => self.lower_path(path),
                other => Err(err(other, "only a loop's byte binding can be dereferenced")),
            },
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
                match self.lookup(&name.to_string())? {
                    Binding::Param { ty, .. }
                    | Binding::Local { ty, .. }
                    | Binding::Const { ty, .. } => Some(ty),
                    Binding::Table(_) => None,
                }
            }
            syn::Expr::Paren(paren) => self.peek(&paren.expr),
            syn::Expr::Unary(unary) => self.peek(&unary.expr),
            syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Bool(_),
                ..
            }) => Some(Scalar::BOOL),
            syn::Expr::MethodCall(call) => {
                let receiver = self.classify(&call.receiver).ok()?;
                let method = intrinsics::find(receiver.kind(), &call.method.to_string())?;
                match method.handler {
                    Handler::Value {
                        result: Some(scalar),
                        ..
                    } => Some(scalar),
                    Handler::Value { result: None, .. } => {
                        Scalar::of(intrinsics::typed_argument(call)?)
                    }
                    _ => None,
                }
            }
            syn::Expr::Cast(cast) => Scalar::held(&cast.ty),
            syn::Expr::Index(index) => match self.classify(&index.expr).ok()? {
                Receiver::Table(table) => Some(table.elem),
                Receiver::Place(_) => Some(Scalar::U8),
                Receiver::Walk(_) => None,
            },
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
    pub(super) fn push_word(&mut self, value: u64) -> syn::Result<()> {
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
    pub(super) fn load(&mut self, cell: CellId) -> syn::Result<()> {
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
            | syn::Expr::Cast(_)
            | syn::Expr::Index(_)
            | syn::Expr::Block(_)
    )
}

/// The expression under any parentheses.
fn unparen(expr: &syn::Expr) -> &syn::Expr {
    let mut inner = expr;
    while let syn::Expr::Paren(paren) = inner {
        inner = &paren.expr;
    }
    inner
}

/// Picks the signed or unsigned opcode by the operand type.
fn signed(ty: Scalar, when_signed: Instr, when_unsigned: Instr) -> Instr {
    if ty.signed {
        when_signed
    } else {
        when_unsigned
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
///
/// `&b` is accepted too: it is how the reference copy binds a region's byte by
/// value, and the cell holds the byte either way.
fn for_var(for_expr: &syn::ExprForLoop) -> syn::Result<Option<String>> {
    let pat = match &*for_expr.pat {
        syn::Pat::Reference(reference) => &*reference.pat,
        other => other,
    };
    match pat {
        syn::Pat::Wild(_) => Ok(None),
        syn::Pat::Ident(pat) if pat.subpat.is_none() => Ok(Some(pat.ident.to_string())),
        other => Err(err(other, "a `for` binding must be a name or `_`")),
    }
}

/// The start and end of a `for` loop's exclusive range.
fn range_bounds(range: &syn::ExprRange) -> syn::Result<(&syn::Expr, &syn::Expr)> {
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

/// The word a literal initializer spells, or `None` when the expression is
/// not one: an integer, `true`/`false`, a byte, or the negation of an
/// integer. A suffix is not read; the reference copy checks it against the
/// declared type.
fn const_scalar(expr: &syn::Expr) -> syn::Result<Option<u64>> {
    Ok(match expr {
        syn::Expr::Paren(paren) => const_scalar(&paren.expr)?,
        syn::Expr::Lit(lit) => match &lit.lit {
            syn::Lit::Int(int) => Some(int.base10_parse()?),
            syn::Lit::Bool(boolean) => Some(boolean.value.into()),
            syn::Lit::Byte(byte) => Some(byte.value().into()),
            _ => None,
        },
        syn::Expr::Unary(syn::ExprUnary {
            op: syn::UnOp::Neg(_),
            expr,
            ..
        }) => match &**expr {
            syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Int(int),
                ..
            }) => Some(0u64.wrapping_sub(int.base10_parse()?)),
            _ => None,
        },
        _ => None,
    })
}

/// The element of a table type: `[T; N]`, `&[T; N]`, `&[T]` or `&str`, whose
/// bytes are what `.bytes()` yields.
fn table_type(ty: &syn::Type) -> Option<Scalar> {
    match ty {
        syn::Type::Reference(reference) => match &*reference.elem {
            syn::Type::Path(path) if path.qself.is_none() && path.path.is_ident("str") => {
                Some(Scalar::U8)
            }
            inner => table_type(inner),
        },
        syn::Type::Array(array) => Scalar::of(&array.elem),
        syn::Type::Slice(slice) => Scalar::of(&slice.elem),
        _ => None,
    }
}

/// The elements a table initializer spells, or `None` when it is not one:
/// `[a, b, c]` of literals, `[a; N]` with a literal count, a string or a
/// byte string, any of them behind `&`. Which of these the declared type
/// admits is the reference copy's to check.
fn const_elements(expr: &syn::Expr) -> syn::Result<Option<Vec<u64>>> {
    Ok(match expr {
        syn::Expr::Paren(paren) => const_elements(&paren.expr)?,
        syn::Expr::Reference(reference) => const_elements(&reference.expr)?,
        syn::Expr::Array(array) => {
            let mut values = Vec::with_capacity(array.elems.len());
            for elem in &array.elems {
                match const_scalar(elem)? {
                    Some(value) => values.push(value),
                    None => return Ok(None),
                }
            }
            Some(values)
        }
        syn::Expr::Repeat(repeat) => {
            let count = match &*repeat.len {
                syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Int(int),
                    ..
                }) => int.base10_parse::<usize>()?,
                _ => return Ok(None),
            };
            const_scalar(&repeat.expr)?.map(|value| vec![value; count])
        }
        syn::Expr::Lit(lit) => match &lit.lit {
            syn::Lit::ByteStr(bytes) => {
                Some(bytes.value().iter().map(|byte| u64::from(*byte)).collect())
            }
            syn::Lit::Str(string) => Some(string.value().bytes().map(u64::from).collect()),
            _ => None,
        },
        _ => None,
    })
}

/// A word in the resting form `normalize` would leave it in at run time:
/// masked to the width when unsigned, sign-extended from it when signed.
fn at_rest(value: u64, ty: Scalar) -> u64 {
    let bits = u32::from(ty.width.bits());
    if bits == u64::BITS {
        value
    } else if ty.signed {
        let shift = u64::BITS - bits;
        // Two's-complement sign extension: shift the sign bit up, then back down.
        ((value << shift) as i64 >> shift) as u64
    } else {
        value & (u64::MAX >> (u64::BITS - bits))
    }
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

/// The refusal for a name nothing in scope binds. A name spelled like a
/// constant most likely is one, declared where the macro cannot see it, so
/// that case says where to put it.
fn undeclared(name: &syn::Ident) -> syn::Error {
    let spelled = name.to_string();
    let constant_case = spelled.chars().any(|c| c.is_ascii_uppercase())
        && !spelled.chars().any(|c| c.is_ascii_lowercase());
    if constant_case {
        err(
            name,
            &format!(
                "`{spelled}` is not declared in this function: the macro only sees the body, \
                 so a const it reads must be declared inside it"
            ),
        )
    } else {
        err(name, "no such parameter or local")
    }
}

/// A refusal pointed at the offending syntax.
pub(super) fn err<T: ToTokens>(node: T, message: &str) -> syn::Error {
    syn::Error::new_spanned(node, message)
}

/// A builder failure, which means the lowerer built something impossible.
pub(super) fn internal<T: ToTokens>(node: T, error: impl core::fmt::Display) -> syn::Error {
    syn::Error::new_spanned(node, format!("internal lowering error: {error}"))
}

/// A builder failure with nothing smaller than the invocation to blame.
fn internal_span(error: impl core::fmt::Display) -> syn::Error {
    syn::Error::new(
        Span::call_site(),
        format!("internal lowering error: {error}"),
    )
}
