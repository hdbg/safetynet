//! Proc-macros for safetynet.

mod backend;
mod derive;
mod lower;

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

/// Compiles a function to VM bytecode.
#[proc_macro_attribute]
pub fn safetynet(attr: TokenStream, item: TokenStream) -> TokenStream {
    match lower::expand(attr.into(), item.into()) {
        Ok(expansion) => expansion.into(),
        Err(error) => error.to_compile_error().into(),
    }
}
