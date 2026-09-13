//! The `#[safetynet]` expansion.
//!
//! Four items come out: the original body, renamed and kept so the Rust
//! compiler still type-checks it; the public function, which marshals its
//! arguments into an image, runs the bytecode and reads the result back; the
//! lowered bytecode itself; and a dead-code assertion per binding that its type
//! is a `VmValue`.

use proc_macro2::{Literal, TokenStream};
use quote::{format_ident, quote};
use safetynet_core::ir::resolve;

use super::build::{self, Lowered};
use crate::backend;

/// Words of operand stack to reserve beyond the frame. Straight-line scalar
/// code needs only a handful; the margin is for the expressions on top of it.
const STACK_MARGIN: u32 = 4096;

/// Instructions a run may take before it is called a runaway.
const FUEL: u64 = 1_000_000;

/// Expands `#[safetynet]` on a function.
pub(crate) fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    if !attr.is_empty() {
        return Err(syn::Error::new_spanned(
            attr,
            "`#[safetynet]` takes no arguments yet",
        ));
    }

    let func: syn::ItemFn = syn::parse2(item)?;
    let Lowered {
        cfg,
        bindings,
        param_offsets,
        input_size,
        ret,
    } = build::lower(&func)?;

    let resolved = resolve(&cfg).map_err(|error| {
        syn::Error::new_spanned(&func.sig.ident, format!("cannot assemble: {error}"))
    })?;
    let stack = u32::from(resolved.frame().bytes()) + STACK_MARGIN;
    // The bytes are baked in one order; `Le` is the default until an argument
    // selects otherwise.
    let order: syn::Path = syn::parse_quote!(::safetynet::Le);
    let program = backend::emit_artifact(&order, &resolved, &[], &[])?;

    let name = &func.sig.ident;
    let hidden = format_ident!("__sn_ref_{name}");
    let program_fn = format_ident!("__sn_program_{name}");

    // The original body, renamed and made private: the compiler type-checks it,
    // and it is the oracle a later differential harness compares against.
    let mut reference = func.clone();
    reference.sig.ident = hidden.clone();
    reference.vis = syn::Visibility::Inherited;

    // The public function: marshal, run, read back.
    let vis = &func.vis;
    let sig = &func.sig;
    let args = param_names(&func)?;
    let offsets = param_offsets
        .iter()
        .map(|offset| Literal::usize_unsuffixed(*offset as usize));
    let input_len = Literal::usize_unsuffixed(input_size as usize);
    let fuel = Literal::u64_suffixed(FUEL);
    let public = quote! {
        #vis #sig {
            let mut __sn_input = [0u8; #input_len];
            #(
                if let ::core::option::Option::Some(__sn_slot) = __sn_input.get_mut(#offsets..) {
                    ::safetynet::VmLayout::marshal::<::safetynet::Le>(&#args, __sn_slot);
                }
            )*
            let __sn_layout = match ::safetynet::Layout::new(::safetynet::image::Sizes {
                input: #input_size,
                scratch: 0,
                stack: #stack,
            }) {
                ::core::option::Option::Some(__sn_layout) => __sn_layout,
                ::core::option::Option::None => ::core::panic!("safetynet: the image does not fit"),
            };
            let mut __sn_image = ::safetynet::Image::new(__sn_layout);
            if __sn_image.write(::safetynet::Region::Input, &__sn_input).is_none() {
                ::core::panic!("safetynet: the input does not fit");
            }
            let __sn_program = match #program_fn().finalize(&__sn_layout) {
                ::core::result::Result::Ok(__sn_program) => __sn_program,
                ::core::result::Result::Err(__sn_error) => {
                    ::core::panic!("safetynet: {__sn_error}")
                }
            };
            let mut __sn_vm = match ::safetynet::Vm::<::safetynet::Le>::new(__sn_image)
                .run(&__sn_program, #fuel)
            {
                ::core::result::Result::Ok(__sn_vm) => __sn_vm,
                ::core::result::Result::Err(__sn_trap) => ::core::panic!("safetynet: {__sn_trap}"),
            };
            let __sn_result = match __sn_vm.pop() {
                ::core::result::Result::Ok(__sn_word) => __sn_word,
                ::core::result::Result::Err(__sn_trap) => ::core::panic!("safetynet: {__sn_trap}"),
            };
            <#ret as ::safetynet::VmValue>::from_word(__sn_result)
        }
    };

    // A non-`VmValue` binding fails to compile, pointed at its own type.
    let assertions = quote! {
        const _: fn() = || {
            fn __sn_assert_vm_value<__T: ::safetynet::VmValue>() {}
            #( __sn_assert_vm_value::<#bindings>(); )*
        };
    };

    let embedded = quote! {
        fn #program_fn() -> ::safetynet::Artifact<::safetynet::Le> {
            #program
        }
    };

    Ok(quote! {
        #[allow(dead_code)]
        #reference
        #public
        #embedded
        #assertions
    })
}

/// The parameter names, to marshal in signature order.
fn param_names(func: &syn::ItemFn) -> syn::Result<Vec<syn::Ident>> {
    func.sig
        .inputs
        .iter()
        .map(|arg| match arg {
            syn::FnArg::Typed(typed) => match &*typed.pat {
                syn::Pat::Ident(pat) => Ok(pat.ident.clone()),
                other => Err(syn::Error::new_spanned(other, "a parameter must be a name")),
            },
            syn::FnArg::Receiver(receiver) => Err(syn::Error::new_spanned(
                receiver,
                "no receiver is supported",
            )),
        })
        .collect()
}
