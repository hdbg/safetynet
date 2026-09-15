//! The backend: a resolved graph becomes the tokens that rebuild it.
//!
//! The lowerer resolves to a graph; what comes out the other side is the
//! encoded instructions, the region-base relocations the host fills in, and
//! the field holes the linker bakes at compile time.

use proc_macro2::{Literal, TokenStream};
use quote::quote;
use safetynet_core::encoding::{Packed, encode, push32_immediate};
use safetynet_core::ir::Resolved;
use safetynet_core::isa::{Ld8, Ld32, Ld64};
use safetynet_core::{Be, Instr, Le, Region};
use syn::Path;
use syn::spanned::Spanned;

/// A field reference the call site resolves: its type and the path into it.
#[derive(Debug)]
pub(crate) struct FieldRef {
    pub(crate) ty: syn::Type,
    pub(crate) path: Vec<syn::Ident>,
}

/// The two byte orders the backend can encode against at expansion.
#[derive(Clone, Copy)]
enum Order {
    Le,
    Be,
}

/// Encodes a resolved graph and writes the expression that rebuilds it as an
/// `Artifact` in the given order.
///
/// The block-resolving work is already done; what is left is order-dependent —
/// the encoded bytes, and where each push's immediate lands so a hole can be
/// patched into it.
pub(crate) fn emit_artifact(
    order: &Path,
    resolved: &Resolved,
    field_refs: &[FieldRef],
) -> syn::Result<TokenStream> {
    let which = concrete_order(order)?;

    // The IR can hold a tag reference, but nothing here produces one.
    if !resolved.tag_relocs().is_empty() {
        return Err(syn::Error::new(
            order.span(),
            "a tag reference has nothing to resolve it",
        ));
    }

    // Encode in the chosen order, recording where each instruction begins so a
    // relocation can name the byte its push starts at. A region base resolves to
    // a placeholder `Push32 { imm: 0 }`, which encodes as the five zero-immediate
    // bytes the relocation later overwrites.
    let mut bytes = Vec::new();
    let mut offsets = Vec::with_capacity(resolved.code().len());
    for instr in resolved.code() {
        offsets.push(bytes.len());
        let written = match which {
            Order::Le => encode::<Le>(*instr, &mut bytes),
            Order::Be => encode::<Be>(*instr, &mut bytes),
        };
        written
            .map_err(|error| syn::Error::new(order.span(), format!("cannot assemble: {error}")))?;
    }

    let raw = bytes.iter().map(|byte| Literal::u8_suffixed(*byte));
    let len = Literal::usize_unsuffixed(bytes.len());
    let frame_bytes = Literal::u16_suffixed(resolved.frame().bytes());

    let byte_at =
        |index: usize| Literal::usize_suffixed(offsets.get(index).copied().unwrap_or_default());

    // Region bases wait for a layout at finalize; field offsets are known now,
    // so they are baked at compile time by the linker below.
    let mut relocs = Vec::new();
    for reloc in resolved.relocs() {
        let at = byte_at(reloc.index);
        let region = region_tokens(reloc.region);
        relocs.push(quote! {
            ::safetynet::Reloc::region_base::<::safetynet::encoding::Packed<#order>>(#at, #region)
        });
    }

    // Where each push's immediate lands and how it is ordered, probed from the
    // very encoder that wrote the bytes above — the linker patches into that
    // rather than assuming a shape. Field offsets ride a push32.
    let immediate = || match which {
        Order::Le => push32_immediate(&Packed::<Le>::new()),
        Order::Be => push32_immediate(&Packed::<Be>::new()),
    };
    let missing = || syn::Error::new(order.span(), "the encoder has no push32 immediate to patch");
    let at_of = |imm: &safetynet_core::Immediate, index: usize| {
        Literal::usize_suffixed(offsets.get(index).copied().unwrap_or_default() + imm.at)
    };

    let mut patches = Vec::new();

    if !resolved.field_relocs().is_empty() {
        let imm = immediate().ok_or_else(missing)?;
        let (width, big_endian) = (Literal::usize_suffixed(imm.width), imm.big_endian);
        for reloc in resolved.field_relocs() {
            let at = at_of(&imm, reloc.index);
            let field = field_refs.get(reloc.hole as usize).ok_or_else(|| {
                syn::Error::new(
                    order.span(),
                    "a field reference went missing while assembling",
                )
            })?;
            let ty = &field.ty;
            let names = field
                .path
                .iter()
                .map(|name| Literal::string(&name.to_string()));
            patches.push(quote! {
                ::safetynet::Patch {
                    at: #at,
                    width: #width,
                    big_endian: #big_endian,
                    hole: ::safetynet::Hole::Field {
                        layout: <#ty as ::safetynet::VmLayout>::LAYOUT,
                        path: &[#(#names),*],
                    },
                }
            });
        }
    }

    // A field load is one opcode byte at the instruction's own offset; the
    // linker picks `ld8`/`ld32`/`ld64` from the field's size, so the three
    // opcode bytes are probed from the encoder and ride along.
    if !resolved.load_relocs().is_empty() {
        let [ld8, ld32, ld64] = load_opcodes(which);
        for reloc in resolved.load_relocs() {
            let at = Literal::usize_suffixed(offsets.get(reloc.index).copied().unwrap_or_default());
            let field = field_refs.get(reloc.hole as usize).ok_or_else(|| {
                syn::Error::new(
                    order.span(),
                    "a field load reference went missing while assembling",
                )
            })?;
            let ty = &field.ty;
            let names = field
                .path
                .iter()
                .map(|name| Literal::string(&name.to_string()));
            patches.push(quote! {
                ::safetynet::Patch {
                    at: #at,
                    width: 1,
                    big_endian: false,
                    hole: ::safetynet::Hole::Load {
                        layout: <#ty as ::safetynet::VmLayout>::LAYOUT,
                        path: &[#(#names),*],
                        opcodes: [#ld8, #ld32, #ld64],
                    },
                }
            });
        }
    }

    Ok(quote! {
        {
            const CODE: &[u8] = &::safetynet::link::<#len>(
                [#(#raw),*],
                &[#(#patches),*],
            );
            ::safetynet::Artifact::<#order>::new(
                CODE,
                ::safetynet::FrameSize::new(#frame_bytes).unwrap_or_default(),
                ::std::vec![#(#relocs),*],
            )
        }
    })
}

/// The `ld8`, `ld32` and `ld64` opcode bytes, so the linker can write the one a
/// field's size calls for without a second copy of the wire format.
fn load_opcodes(which: Order) -> [u8; 3] {
    let byte = |instr: Instr| {
        let mut bytes = Vec::new();
        let _ = match which {
            Order::Le => encode::<Le>(instr, &mut bytes),
            Order::Be => encode::<Be>(instr, &mut bytes),
        };
        bytes.first().copied().unwrap_or_default()
    };
    [byte(Ld8.into()), byte(Ld32.into()), byte(Ld64.into())]
}

/// Reads the concrete order off the path's last segment.
///
/// The order is emitted verbatim into the `Artifact<_>` type, so an alias that
/// resolves to `Le` or `Be` still type-checks at the call site — but the
/// expansion has to pick an encoder to run *now*, and the ident is all it has to
/// go on. Anything else is refused rather than guessed.
fn concrete_order(order: &Path) -> syn::Result<Order> {
    match order.segments.last() {
        Some(segment) if segment.ident == "Le" => Ok(Order::Le),
        Some(segment) if segment.ident == "Be" => Ok(Order::Be),
        _ => Err(syn::Error::new(
            order.span(),
            "a concrete byte order, `Le` or `Be`, is needed to encode",
        )),
    }
}

/// A region as the path that names it through the façade.
fn region_tokens(region: Region) -> TokenStream {
    match region {
        Region::Input => quote!(::safetynet::Region::Input),
        Region::Scratch => quote!(::safetynet::Region::Scratch),
        Region::Stack => quote!(::safetynet::Region::Stack),
    }
}
