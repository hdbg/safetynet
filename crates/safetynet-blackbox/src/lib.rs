//! [`blackboxify!`]: wrap a block's expressions in [`core::hint::black_box`], so
//! the optimizer cannot fold or drop them.

use proc_macro::TokenStream;
use quote::quote;
use syn::visit_mut::{self, VisitMut};
use syn::{Block, Expr, Local, parse_quote};

/// Wraps every value-producing expression in a block — function and method
/// calls, arithmetic, `if`/`match`, casts, assignments, and each `let`
/// initializer — in `::core::hint::black_box`.
///
/// The input is treated as a block body (a bare expression is a one-line block),
/// and the result is a block expression. Place expressions — the target of an
/// assignment, a method's receiver, a path — are left alone, so what comes out
/// still borrows and assigns exactly what went in.
///
/// ```
/// let x = safetynet_blackbox::blackboxify! { 2 + 40 };
/// assert_eq!(x, 42);
/// ```
#[proc_macro]
pub fn blackboxify(input: TokenStream) -> TokenStream {
    let body: proc_macro2::TokenStream = input.into();
    let mut block: Block = match syn::parse2(quote!({ #body })) {
        Ok(block) => block,
        Err(error) => return error.to_compile_error().into(),
    };

    Boxer.visit_block_mut(&mut block);
    quote!(#block).into()
}

/// Walks the syntax tree, wrapping each wrappable expression on the way back up.
struct Boxer;

impl VisitMut for Boxer {
    fn visit_expr_mut(&mut self, expr: &mut Expr) {
        // Children first, so an outer call wraps around already-wrapped inner
        // ones rather than re-visiting them.
        visit_mut::visit_expr_mut(self, expr);
        if wrappable(expr) {
            wrap(expr);
        }
    }

    fn visit_local_mut(&mut self, local: &mut Local) {
        visit_mut::visit_local_mut(self, local);
        // A `let` initializer is always a value, whatever its kind.
        if let Some(init) = &mut local.init {
            wrap(&mut init.expr);
        }
    }
}

/// Whether an expression produces a value that is safe to pass through
/// `black_box`. Place expressions — paths, field and index access, references —
/// are excluded so a receiver or an assignment target is never moved.
fn wrappable(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::Call(_)
            | Expr::MethodCall(_)
            | Expr::Binary(_)
            | Expr::Unary(_)
            | Expr::If(_)
            | Expr::Match(_)
            | Expr::Cast(_)
            | Expr::Assign(_)
    )
}

/// Wraps `expr` in `black_box`, unless it already is one.
fn wrap(expr: &mut Expr) {
    if is_black_box(expr) {
        return;
    }
    let inner = expr.clone();
    *expr = parse_quote!(::core::hint::black_box(#inner));
}

/// Whether `expr` is already a `black_box(..)` call.
fn is_black_box(expr: &Expr) -> bool {
    let Expr::Call(call) = expr else {
        return false;
    };
    let Expr::Path(path) = call.func.as_ref() else {
        return false;
    };
    path.path
        .segments
        .last()
        .is_some_and(|segment| segment.ident == "black_box")
}
