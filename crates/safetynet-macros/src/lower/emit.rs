//! The `#[safetynet]` expansion.
//!
//! Four items come out: the original body, renamed and kept so the Rust
//! compiler still type-checks it; the public function, which marshals its
//! arguments into an image, runs the bytecode and reads the result back; the
//! lowered bytecode itself; and dead-code assertions that each binding is a
//! `VmValue` (scalars) or `VmLayout` (aggregate parameters).

use proc_macro2::{Literal, TokenStream};
use quote::{format_ident, quote};
use safetynet_core::ir::resolve;

use super::build::{self, Lowered};
use crate::backend;

/// Bytes of operand stack to reserve beyond the frame. Straight-line scalar
/// code needs only a few words; the margin is for the expressions on top.
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
        layouts,
        field_refs,
        rodata,
        param_offsets,
        input_size,
        aggregate,
        ret,
    } = build::lower(&func)?;

    // A debugging window on the compiler: `SN_DUMP_IR=1` prints the graph each
    // function lowered to. The listing is a `debug` form, so it is empty
    // without the feature.
    if std::env::var_os("SN_DUMP_IR").is_some() {
        eprintln!("// {}\n{cfg:?}", func.sig.ident);
    }

    let resolved = resolve(&cfg).map_err(|error| {
        syn::Error::new_spanned(&func.sig.ident, format!("cannot assemble: {error}"))
    })?;
    let stack = u32::from(resolved.frame().bytes()) + STACK_MARGIN;
    // The bytes are baked in one order; `Le` is the default until an argument
    // selects otherwise.
    let order: syn::Path = syn::parse_quote!(::safetynet::Le);
    let program = backend::emit_artifact(&order, &resolved, &field_refs, &rodata)?;
    let rodata_len = Literal::u32_suffixed(rodata.size);
    // The constants are the artifact's own; a function without any has
    // nothing to write.
    let place_rodata = (rodata.size > 0).then(|| {
        quote! {
            if __sn_image.write(::safetynet::Region::Rodata, __sn_artifact.rodata()).is_none() {
                ::safetynet::__private::fail(::safetynet::__private::Failure::ImageDoesNotFit);
            }
        }
    });

    let name = &func.sig.ident;
    let hidden = format_ident!("__sn_ref_{name}");
    let program_fn = format_ident!("__sn_program_{name}");

    // The original body, renamed and made private: the compiler type-checks
    let mut reference = func.clone();
    reference.sig.ident = hidden.clone();
    reference.vis = syn::Visibility::Inherited;

    // The public function: marshal, run, read back. The original's attributes
    // ride along — docs, `#[must_use]`, lints — since this is the function
    // callers actually see.
    let attrs = &func.attrs;
    let vis = &func.vis;
    let sig = &func.sig;
    let args = param_names(&func)?;
    let fuel = Literal::u64_suffixed(FUEL);
    let (fixed_len, marshal) = input_shape(aggregate.as_ref(), &args, &param_offsets, input_size);
    let public = quote! {
        #(#attrs)*
        #vis #sig {
            // The fixed part is sized at compile time; a variable-length field
            // appends its content to the tail, so `.input` is sized at run time.
            let mut __sn_fixed = [0u8; #fixed_len];
            let mut __sn_tail = ::safetynet::Tail::new(#fixed_len);
            #marshal
            let mut __sn_input = __sn_fixed.to_vec();
            __sn_input.extend_from_slice(__sn_tail.as_slice());
            // Every failure goes through one runtime function, which knows
            // whether the build may say why.
            let __sn_input_len = match u32::try_from(__sn_input.len()) {
                ::core::result::Result::Ok(__sn_len) => __sn_len,
                ::core::result::Result::Err(_) => ::safetynet::__private::fail(
                    ::safetynet::__private::Failure::InputTooLarge,
                ),
            };
            let __sn_artifact = #program_fn();
            let __sn_layout = match ::safetynet::Layout::new(::safetynet::image::Sizes {
                input: __sn_input_len,
                rodata: #rodata_len,
                scratch: 0,
                stack: #stack,
            }) {
                ::core::option::Option::Some(__sn_layout) => __sn_layout,
                ::core::option::Option::None => ::safetynet::__private::fail(
                    ::safetynet::__private::Failure::ImageDoesNotFit,
                ),
            };
            let mut __sn_image = ::safetynet::Image::new(__sn_layout);
            if __sn_image.write(::safetynet::Region::Input, &__sn_input).is_none() {
                ::safetynet::__private::fail(::safetynet::__private::Failure::InputDoesNotFit);
            }
            #place_rodata
            let __sn_program = match __sn_artifact.finalize(&__sn_layout) {
                ::core::result::Result::Ok(__sn_program) => __sn_program,
                ::core::result::Result::Err(__sn_error) => ::safetynet::__private::fail(
                    ::safetynet::__private::Failure::NotFinal(__sn_error),
                ),
            };
            let mut __sn_vm = match ::safetynet::Vm::<::safetynet::Le>::new(__sn_image)
                .run(&__sn_program, #fuel)
            {
                ::core::result::Result::Ok(__sn_vm) => __sn_vm,
                ::core::result::Result::Err(__sn_trap) => ::safetynet::__private::fail(
                    ::safetynet::__private::Failure::Trap(__sn_trap),
                ),
            };
            let __sn_result = match __sn_vm.pop() {
                ::core::result::Result::Ok(__sn_word) => __sn_word,
                ::core::result::Result::Err(__sn_trap) => ::safetynet::__private::fail(
                    ::safetynet::__private::Failure::Trap(__sn_trap),
                ),
            };
            <#ret as ::safetynet::VmValue>::from_word(__sn_result)
        }
    };

    // A scalar that is not `VmValue`, or an aggregate that is not `VmLayout`,
    // fails to compile, pointed at its own type.
    let assertions = quote! {
        const _: fn() = || {
            fn __sn_assert_vm_value<__T: ::safetynet::VmValue>() {}
            fn __sn_assert_vm_layout<__T: ::safetynet::VmLayout>() {}
            #( __sn_assert_vm_value::<#bindings>(); )*
            #( __sn_assert_vm_layout::<#layouts>(); )*
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

/// The fixed part's size and the marshalling that fills it.
///
/// Scalars pack at their offsets; a single aggregate fills the fixed part
/// through its own `marshal`, sized by its `VmLayout::SIZE`.
fn input_shape(
    aggregate: Option<&syn::Type>,
    args: &[syn::Ident],
    param_offsets: &[u32],
    input_size: u32,
) -> (TokenStream, TokenStream) {
    if let (Some(ty), Some(arg)) = (aggregate, args.first()) {
        return (
            quote! { <#ty as ::safetynet::VmLayout>::SIZE },
            quote! {
                ::safetynet::VmLayout::marshal::<::safetynet::Le>(
                    &#arg,
                    &mut __sn_fixed,
                    &mut __sn_tail,
                );
            },
        );
    }

    let len = Literal::usize_unsuffixed(input_size as usize);
    let offsets = param_offsets
        .iter()
        .map(|offset| Literal::usize_unsuffixed(*offset as usize));
    (
        quote! { #len },
        quote! {
            #(
                if let ::core::option::Option::Some(__sn_slot) = __sn_fixed.get_mut(#offsets..) {
                    ::safetynet::VmLayout::marshal::<::safetynet::Le>(&#args, __sn_slot, &mut __sn_tail);
                }
            )*
        },
    )
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
