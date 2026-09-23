//! The lowerer: a Rust function, into a graph and the items around it.
//!
//! Where the assembler reads text, this reads a `syn::ItemFn` — but both build
//! the same `core` graph and hand it to the same backend, so the two front-ends
//! never go through each other.

mod build;
mod emit;
mod intrinsics;
mod ty;

#[cfg(test)]
mod tests;

pub(crate) use emit::expand;
