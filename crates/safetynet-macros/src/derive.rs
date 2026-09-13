//! `#[derive(VmLayout)]`: the canonical flat layout of a struct, computed from
//! its fields' own layouts at const time.

use proc_macro2::{Literal, TokenStream};
use quote::{format_ident, quote};
use syn::{Data, DeriveInput, Fields, Ident, LitStr, Type};

/// One field to lay out: its name and type.
struct FieldDef {
    name: Ident,
    ty: Type,
}

pub(crate) fn vm_layout(input: DeriveInput) -> syn::Result<TokenStream> {
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.generics,
            "`VmLayout` cannot be derived for a generic type yet",
        ));
    }

    let name = &input.ident;
    let fields = fields(&input)?;

    // Per field: its alignment, size, and byte offset, each a const the compiler
    // resolves from the field's own `VmLayout`. Offsets chain, so a field starts
    // at the next multiple of its alignment past the one before it.
    let mut layout_consts = Vec::new();
    let (mut aligns, mut sizes, mut offsets) = (Vec::new(), Vec::new(), Vec::new());
    let mut entries = Vec::new();
    for (i, field) in fields.iter().enumerate() {
        let ty = &field.ty;
        let (a, s, o) = (
            format_ident!("__sn_a{i}"),
            format_ident!("__sn_s{i}"),
            format_ident!("__sn_o{i}"),
        );
        let prev_end = if i == 0 {
            quote!(0usize)
        } else {
            let (po, ps) = (
                format_ident!("__sn_o{}", i - 1),
                format_ident!("__sn_s{}", i - 1),
            );
            quote!(#po + #ps)
        };
        layout_consts.push(quote! {
            const #a: usize = <#ty as ::safetynet::VmLayout>::ALIGN;
            const #s: usize = <#ty as ::safetynet::VmLayout>::SIZE;
            const #o: usize = (#prev_end).next_multiple_of(#a);
        });

        let name_str = LitStr::new(&field.name.to_string(), field.name.span());
        entries.push(quote! {
            ::safetynet::Field::new(
                #name_str,
                #o as u32,
                #s as u32,
                if <#ty as ::safetynet::VmLayout>::LAYOUT.fields().is_empty() {
                    ::core::option::Option::None
                } else {
                    ::core::option::Option::Some(<#ty as ::safetynet::VmLayout>::LAYOUT)
                },
            )
        });
        aligns.push(a);
        sizes.push(s);
        offsets.push(o);
    }

    let struct_end = match (offsets.last(), sizes.last()) {
        (Some(o), Some(s)) => quote!(#o + #s),
        _ => quote!(0usize),
    };

    // One inherent const holds the whole computation, so `LAYOUT`, `SIZE` and
    // `ALIGN` share it rather than each recomputing the offsets.
    let sn = format_ident!("__SN_VMLAYOUT");
    let computed = quote! {
        #[doc(hidden)]
        #[allow(non_upper_case_globals)]
        impl #name {
            const #sn: (&'static [::safetynet::Field], usize, usize) = {
                #(#layout_consts)*
                const __sn_align: usize = {
                    let mut __m = 1usize;
                    #( if #aligns > __m { __m = #aligns; } )*
                    __m
                };
                const __sn_size: usize = (#struct_end).next_multiple_of(__sn_align);
                (&[#(#entries),*], __sn_size, __sn_align)
            };
        }
    };

    let indices = (0..fields.len()).map(Literal::usize_unsuffixed);
    let marshal = fields.iter().zip(indices.clone()).map(|(field, i)| {
        let f = &field.name;
        quote! {
            if let ::core::option::Option::Some(__fd) = __f.get(#i) {
                let __o = __fd.offset() as usize;
                if let ::core::option::Option::Some(__slot) = __mem.get_mut(__o..__o + __fd.size() as usize) {
                    ::safetynet::VmLayout::marshal::<B>(&self.#f, __slot);
                }
            }
        }
    });
    let unmarshal = fields.iter().zip(indices).map(|(field, i)| {
        let (f, ty) = (&field.name, &field.ty);
        quote! {
            #f: {
                let __sub: &[u8] = match __f.get(#i) {
                    ::core::option::Option::Some(__fd) => {
                        let __o = __fd.offset() as usize;
                        __mem.get(__o..__o + __fd.size() as usize).unwrap_or(&[])
                    }
                    ::core::option::Option::None => &[],
                };
                <#ty as ::safetynet::VmLayout>::unmarshal::<B>(__sub)
            },
        }
    });

    Ok(quote! {
        #computed

        #[automatically_derived]
        impl ::safetynet::VmLayout for #name {
            const LAYOUT: &'static ::safetynet::TypeLayout = {
                const __L: ::safetynet::TypeLayout = ::safetynet::TypeLayout::new(#name::#sn.0);
                &__L
            };
            const SIZE: usize = #name::#sn.1;
            const ALIGN: usize = #name::#sn.2;

            fn marshal<B: ::safetynet::ByteOrder>(&self, __mem: &mut [u8]) {
                let __f = <Self as ::safetynet::VmLayout>::LAYOUT.fields();
                #(#marshal)*
            }

            fn unmarshal<B: ::safetynet::ByteOrder>(__mem: &[u8]) -> Self {
                let __f = <Self as ::safetynet::VmLayout>::LAYOUT.fields();
                Self { #(#unmarshal)* }
            }
        }
    })
}

/// The named fields to lay out, in order. A unit struct has none; anything that
/// is not a named-field struct is refused.
fn fields(input: &DeriveInput) -> syn::Result<Vec<FieldDef>> {
    let named = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(named) => &named.named,
            Fields::Unit => return Ok(Vec::new()),
            Fields::Unnamed(_) => {
                return Err(syn::Error::new_spanned(
                    input,
                    "`VmLayout` needs named fields; a tuple struct has no names to marshal by",
                ));
            }
        },
        Data::Enum(_) => {
            return Err(syn::Error::new_spanned(
                input,
                "`VmLayout` is for structs; a field-less enum derives `VmValue` instead",
            ));
        }
        Data::Union(_) => {
            return Err(syn::Error::new_spanned(
                input,
                "a union has no defined layout to marshal",
            ));
        }
    };

    named
        .iter()
        .map(|field| match &field.ident {
            Some(name) => Ok(FieldDef {
                name: name.clone(),
                ty: field.ty.clone(),
            }),
            None => Err(syn::Error::new_spanned(field, "a named field has no name")),
        })
        .collect()
}
