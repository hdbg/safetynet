//! The expansion: a resolved graph encoded to the tokens that rebuild an
//! `Artifact` at the call site.
//!
//! The block-resolving work — validation, layout, branch backpatching — is the
//! same whatever order or image the program later runs against, so it happens
//! here, at expansion. Resolving is all this front-end adds; encoding the result
//! and baking its holes is the shared backend's job.

use proc_macro2::TokenStream;
use safetynet_core::ir::resolve;
use syn::Path;
use syn::spanned::Spanned;

use super::Lowered;
use crate::backend;

/// Resolves a graph and hands it to the backend to encode in the given order.
pub(crate) fn emit(order: &Path, lowered: &Lowered) -> syn::Result<TokenStream> {
    // The layout- and order-independent work. Anything the resolver rejects
    // surfaces here, where the invocation still has a span.
    let resolved = resolve(&lowered.cfg)
        .map_err(|error| syn::Error::new(order.span(), format!("cannot assemble: {error}")))?;

    backend::emit_artifact(order, &resolved, &lowered.field_refs, &lowered.tag_refs)
}
