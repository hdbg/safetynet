//! Proc-macros for safetynet.

mod asm;
mod derive;

use proc_macro::TokenStream;

/// Derives [`VmLayout`] for a struct: its canonical flat layout, `SIZE`,
/// `ALIGN`, and `marshal`/`unmarshal`, computed from the fields' own layouts.
#[proc_macro_derive(VmLayout)]
pub fn derive_vm_layout(input: TokenStream) -> TokenStream {
    match syn::parse(input).and_then(derive::vm_layout) {
        Ok(expansion) => expansion.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

/// Derives [`VmValue`] for a field-less enum: its discriminant to and from a
/// word.
#[proc_macro_derive(VmValue)]
pub fn derive_vm_value(input: TokenStream) -> TokenStream {
    match syn::parse(input).and_then(derive::vm_value) {
        Ok(expansion) => expansion.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

/// Assembles a program at compile time.
///
/// Takes the byte order to lower in, then a program, and evaluates to an
/// `Artifact<Order>` — bytecode that has been checked, block-resolved and
/// encoded, with only the region-base addresses left for the host. Calling
/// `finalize` against a layout fills those in and hands back the runnable
/// `Program`.
///
/// The order has to be a concrete `Le` or `Be`: the bytes are baked at expansion
/// and their immediates are stored in one order or the other, so a generic type
/// parameter is refused. An alias whose last path segment reads `Le`/`Be` is
/// fine — that is the ident the expansion picks its encoder from.
///
/// ```text
/// safetynet::asm!(Le {
///     .frame { cursor: u64, sum: u64 }
/// head:
///     $push .input
///     $store cursor
/// loop:
///     $load cursor
///     $load sum
///     eq
///     jnz done            // the other arm falls through
/// body:
///     $load cursor
///     push8 1
///     add
///     $store cursor
///     jmp loop
/// done:
///     $load sum
///     halt
/// })
/// ```
///
/// A label opens a block and a terminator closes it; a block that reaches the
/// next label without one falls through to it. Cells are named in `.frame` and
/// reached by name, so nothing here spells a displacement or an address.
///
/// The expansion names `::safetynet::` paths, so the calling crate has to
/// depend on the façade rather than on the core crate alone.
#[proc_macro]
pub fn asm(input: TokenStream) -> TokenStream {
    match asm::expand(input.into()) {
        Ok(expansion) => expansion.into(),
        Err(error) => error.to_compile_error().into(),
    }
}
