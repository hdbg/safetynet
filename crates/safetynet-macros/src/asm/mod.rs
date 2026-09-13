//! The assembler: assembly text in, the tokens that rebuild a graph out.
//!
//! The text this reads is the text
//! [`print`](safetynet_core::asm::print) writes, and the two are a fixed point
//! rather than two formats that resemble each other — which is what makes a
//! printed graph something a person can edit and hand back.
//!
//! It runs in two passes. The first turns tokens into blocks and keeps a span
//! for every statement; the second resolves labels into edges, works out the
//! stack depth each block is entered at, and hands the graph to the same
//! validator the rest of the compiler uses. Everything a check can reject is
//! rejected here, where there is still a span to point at.

mod ast;
mod emit;
mod lower;
mod mnemonic;

#[cfg(test)]
mod tests;

use proc_macro2::TokenStream;
use syn::parse::{Parse, ParseStream};
use syn::{Path, braced};

pub(crate) use lower::Lowered;

/// Expands one invocation into the expression it evaluates to.
pub(crate) fn expand(tokens: TokenStream) -> syn::Result<TokenStream> {
    let invocation: Invocation = syn::parse2(tokens)?;
    let lowered = parse_cfg(invocation.program)?;

    emit::emit(&invocation.order, &lowered)
}

/// Assembles a program and checks it.
pub(crate) fn parse_cfg(tokens: TokenStream) -> syn::Result<Lowered> {
    let lowered = parse_cfg_raw(tokens)?;
    lowered.check()?;
    Ok(lowered)
}

/// Assembles a program without checking it.
///
/// For text that is a listing rather than a program: decoded bytecode carries
/// the prologue that reserves the frame, and a graph is not allowed to say that
/// in a block.
pub(crate) fn parse_cfg_raw(tokens: TokenStream) -> syn::Result<Lowered> {
    lower::lower(syn::parse2(tokens)?)
}

/// One `asm!` invocation: the byte order, then the program.
#[derive(Debug)]
struct Invocation {
    /// The byte order to lower in. A path so an alias resolves at the call site,
    /// though the expansion still reads a concrete `Le`/`Be` off its last segment.
    order: Path,
    program: TokenStream,
}

impl Parse for Invocation {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let order = input.parse()?;

        let program;
        braced!(program in input);

        Ok(Self {
            order,
            program: program.parse()?,
        })
    }
}
