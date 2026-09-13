//! The expansion: a resolved graph encoded to the bytes and relocations that
//! rebuild an [`Artifact`] at the call site.
//!
//! The block-resolving work — validation, layout, branch backpatching — is the
//! same whatever order or image the program later runs against, so it happens
//! here, at expansion, and what the macro emits is finished bytecode. The one
//! address a graph cannot settle on its own, a region's base, is left to a
//! relocation the host runs when it chooses a layout.

use proc_macro2::{Literal, TokenStream};
use quote::quote;
use safetynet_core::encoding::encode;
use safetynet_core::ir::resolve;
use safetynet_core::{Be, Le, Region};
use syn::Path;
use syn::spanned::Spanned;

use super::Lowered;

/// The two byte orders the macro can encode against at expansion.
enum Order {
    Le,
    Be,
}

/// Encodes a resolved graph and writes the call that rebuilds it as an
/// `Artifact` in the given order.
pub(crate) fn emit(order: &Path, lowered: &Lowered) -> syn::Result<TokenStream> {
    let which = concrete_order(order)?;

    // The layout- and order-independent work, run host-side. Anything the
    // resolver rejects surfaces here, where the invocation still has a span.
    let resolved = resolve(&lowered.cfg)
        .map_err(|error| syn::Error::new(order.span(), format!("cannot assemble: {error}")))?;

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
        relocs.push(quote!(::safetynet::Reloc::region_base::<#order>(#at, #region)));
    }

    let mut patches = Vec::new();
    for reloc in resolved.field_relocs() {
        let at = byte_at(reloc.index);
        let field = lowered.field_refs.get(reloc.hole as usize).ok_or_else(|| {
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
            ::safetynet::FieldPatch {
                at: #at,
                layout: <#ty as ::safetynet::VmLayout>::LAYOUT,
                path: &[#(#names),*],
            }
        });
    }

    Ok(quote! {
        {
            const CODE: &[u8] = &::safetynet::link_fields::<#order, #len>(
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
            "`asm!` needs a concrete byte order, `Le` or `Be`",
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
