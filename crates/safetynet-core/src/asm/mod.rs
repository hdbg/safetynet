//! The disassembler: a graph, or a run of instructions, as assembly text.
//!
//! The other direction — text into a graph — is the assembler macro's job, and a
//! graph into bytecode is [`resolve`](crate::ir::resolve)'s. What is left here is
//! the printing: turning the IR back into the language it reads as.

mod print;

#[cfg(test)]
mod tests;

pub use print::{print, print_listing};
