//! Pass one: tokens become blocks, and every statement keeps its span.
//!
//! Nothing is resolved here. A label is still a name, a block still ends
//! wherever the next label begins, and whether any of it makes sense is the
//! next pass's question. What this pass does decide is where one statement
//! stops and the next starts — see [`mnemonic`](super::mnemonic) for the
//! lookahead that settles it.

use proc_macro2::Span;
use safetynet_core::Width;
use safetynet_core::ir::Item;
use syn::ext::IdentExt as _;
use syn::parse::{Parse, ParseStream};
use syn::{Ident, Path, Token, braced};

use super::mnemonic::{Stmt, statement};

/// A program as written: its cells, where it starts, and its blocks in source
/// order.
#[derive(Debug)]
pub(crate) struct Program {
    pub(crate) cells: Vec<CellDecl>,
    pub(crate) entry: Option<Ident>,
    pub(crate) blocks: Vec<RawBlock>,
}

/// One cell of the frame, as declared.
#[derive(Debug)]
pub(crate) struct CellDecl {
    pub(crate) name: Ident,
    pub(crate) width: Width,
}

/// A block-body step: a resolved instruction, or a field reference still
/// carrying the type and path only the call site can resolve.
#[derive(Debug)]
pub(crate) enum RawItem {
    Core(Item),
    Field { ty: Path, path: Vec<Ident> },
}

impl RawItem {
    /// Net change to `SP`, in bytes.
    pub(crate) fn sp_delta(&self) -> i32 {
        match self {
            Self::Core(item) => item.sp_delta(),
            Self::Field { .. } => Item::Field(0).sp_delta(),
        }
    }
}

/// One block, with its terminator still naming labels.
#[derive(Debug)]
pub(crate) struct RawBlock {
    pub(crate) label: Ident,
    pub(crate) items: Vec<(RawItem, Span)>,
    /// Absent when the block runs into the next label: falling through is an
    /// edge the next pass draws, because only it knows what comes next.
    pub(crate) term: Option<(RawTerm, Span)>,
}

/// How a block ends, before its labels are edges.
#[derive(Debug)]
pub(crate) enum RawTerm {
    Jmp(Ident),
    /// Jump when the popped word is zero; the other arm falls through.
    Jz(Ident),
    /// Jump when it is not zero; the other arm falls through.
    Jnz(Ident),
    Br {
        then: Ident,
        els: Ident,
    },
    Switch {
        arms: Vec<Ident>,
        default: Ident,
    },
    Halt,
}

/// Position of the cell `name` refers to, if the frame declares it.
pub(crate) fn cell_index(cells: &[CellDecl], name: &Ident) -> Option<u16> {
    let position = cells.iter().position(|cell| cell.name == *name)?;
    u16::try_from(position).ok()
}

impl Parse for Program {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut cells = Vec::new();
        let mut framed = false;
        let mut entry = None;

        // Directives come first and nowhere else: a `.` once a block has been
        // opened is a statement, and there is no statement that starts with one.
        while input.peek(Token![.]) {
            input.parse::<Token![.]>()?;
            let directive = Ident::parse_any(input)?;

            match directive.unraw().to_string().as_str() {
                "frame" => {
                    if framed {
                        return Err(syn::Error::new(
                            directive.span(),
                            "the frame is declared once",
                        ));
                    }
                    framed = true;
                    parse_frame(input, &mut cells)?;
                }
                "entry" => {
                    if entry.is_some() {
                        return Err(syn::Error::new(
                            directive.span(),
                            "the entry block is named once",
                        ));
                    }
                    entry = Some(Ident::parse_any(input)?.unraw());
                }
                other => {
                    return Err(syn::Error::new(
                        directive.span(),
                        format!("unknown directive `.{other}`; there are `.frame` and `.entry`"),
                    ));
                }
            }
        }

        let mut blocks = Vec::new();
        while !input.is_empty() {
            blocks.push(parse_block(input, &cells)?);
        }

        Ok(Self {
            cells,
            entry,
            blocks,
        })
    }
}

/// Parses the body of `.frame { .. }` into cells.
fn parse_frame(input: ParseStream, cells: &mut Vec<CellDecl>) -> syn::Result<()> {
    let body;
    braced!(body in input);

    while !body.is_empty() {
        let name = Ident::parse_any(&body)?.unraw();
        if cell_index(cells, &name).is_some() {
            return Err(syn::Error::new(
                name.span(),
                format!("the frame already has a cell named `{name}`"),
            ));
        }

        body.parse::<Token![:]>()?;
        let width = Ident::parse_any(&body)?;
        let width = match width.unraw().to_string().as_str() {
            "u8" => Width::U8,
            "u32" => Width::U32,
            "u64" => Width::U64,
            other => {
                return Err(syn::Error::new(
                    width.span(),
                    format!("unknown cell width `{other}`; there are `u8`, `u32` and `u64`"),
                ));
            }
        };

        cells.push(CellDecl { name, width });

        if body.is_empty() {
            break;
        }
        body.parse::<Token![,]>()?;
    }

    Ok(())
}

/// Parses one labelled block, up to the next label or the end of the program.
fn parse_block(input: ParseStream, cells: &[CellDecl]) -> syn::Result<RawBlock> {
    if !opens_block(input) {
        return Err(input.error("expected a block label, as in `head:`"));
    }

    let label = Ident::parse_any(input)?.unraw();
    input.parse::<Token![:]>()?;

    let mut items = Vec::new();
    let mut term = None;

    while !input.is_empty() && !opens_block(input) {
        let stmt = statement(input, cells)?;

        if term.is_some() {
            return Err(syn::Error::new(
                stmt.span(),
                "this comes after the block's terminator, which is where the block ends",
            ));
        }

        match stmt {
            Stmt::Item(item, span) => items.push((item, span)),
            Stmt::Term(raw, span) => term = Some((raw, span)),
        }
    }

    Ok(RawBlock { label, items, term })
}

/// Whether the next two tokens open a block.
///
/// The one piece of lookahead the whole grammar rests on: a name followed by a
/// colon is a label, and a name anywhere else is a mnemonic or an operand.
pub(crate) fn opens_block(input: ParseStream) -> bool {
    input.peek(Ident::peek_any) && input.peek2(Token![:])
}
