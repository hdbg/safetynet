//! One statement: a mnemonic, its operands, and the lookahead that tells an
//! item from a terminator.
//!
//! There are no separators in the language, so a statement ends where its
//! operands run out. Four mnemonics need more than that, and each is decided by
//! the token after it rather than by a mode the parser is in:
//!
//! - `jmp`/`jz`/`jnz` take a label as an edge and an integer as the raw
//!   instruction, which stays spellable so that a decoded listing reads back;
//! - `switch` is a jump table when a bracket follows and the reserved
//!   instruction otherwise;
//! - `halt` ends a block when nothing but a label follows it, and is an
//!   instruction in the middle of one;
//! - `push` is a region address, and any other use of it is the mistake the
//!   error message names.

use core::fmt::Display;
use core::str::FromStr;

use proc_macro2::Span;
use safetynet_core::ir::{CellId, Item};
use safetynet_core::isa::{
    Add, Alloc, And, BitNot, CmpEq, CmpLe, CmpLt, CmpSLe, CmpSLt, Div, Drop, Free, Halt, Host, Jmp,
    Jnz, Jz, Ld8, Ld32, Ld64, Lds8, Lds32, Lds64, Mul, Or, Push8, Push32, Push64, Rem, SDiv, SRem,
    Sar, Shl, Shr, St8, St32, St64, Sts8, Sts32, Sts64, Sub, Switch, Xor,
};
use safetynet_core::{FrameSize, Instr, Region};
use syn::ext::IdentExt as _;
use syn::parse::ParseStream;
use syn::{Ident, LitInt, Path, Token, bracketed, token};

use super::ast::{CellDecl, RawItem, RawTerm, cell_index, opens_block};

/// One statement: either a step of a block's body, or the end of the block.
#[derive(Debug)]
pub(crate) enum Stmt {
    Item(RawItem, Span),
    Term(RawTerm, Span),
}

impl Stmt {
    /// Where the statement was written; the mnemonic's own span.
    pub(crate) fn span(&self) -> Span {
        match self {
            Self::Item(_, span) | Self::Term(_, span) => *span,
        }
    }
}

/// Parses one statement.
pub(crate) fn statement(input: ParseStream, cells: &[CellDecl]) -> syn::Result<Stmt> {
    if input.peek(Token![.]) {
        return Err(input.error("a directive belongs before the first block"));
    }
    if !input.peek(Ident::peek_any) {
        return Err(input.error("expected an instruction"));
    }

    let word = Ident::parse_any(input)?;
    let at = word.span();
    let name = word.unraw().to_string();

    match name.as_str() {
        // The only instruction that is also a way to end a block.
        "halt" if ends_block(input) => Ok(Stmt::Term(RawTerm::Halt, at)),
        "halt" => Ok(instr(Halt, at)),

        "drop" => Ok(instr(Drop, at)),
        "ld8" => Ok(instr(Ld8, at)),
        "ld32" => Ok(instr(Ld32, at)),
        "ld64" => Ok(instr(Ld64, at)),
        "st8" => Ok(instr(St8, at)),
        "st32" => Ok(instr(St32, at)),
        "st64" => Ok(instr(St64, at)),
        "add" => Ok(instr(Add, at)),
        "sub" => Ok(instr(Sub, at)),
        "mul" => Ok(instr(Mul, at)),
        "div" => Ok(instr(Div, at)),
        "rem" => Ok(instr(Rem, at)),
        "sdiv" => Ok(instr(SDiv, at)),
        "srem" => Ok(instr(SRem, at)),
        "and" => Ok(instr(And, at)),
        "or" => Ok(instr(Or, at)),
        "xor" => Ok(instr(Xor, at)),
        "not" => Ok(instr(BitNot, at)),
        "shl" => Ok(instr(Shl, at)),
        "shr" => Ok(instr(Shr, at)),
        "sar" => Ok(instr(Sar, at)),
        "eq" => Ok(instr(CmpEq, at)),
        "lt" => Ok(instr(CmpLt, at)),
        "le" => Ok(instr(CmpLe, at)),
        "slt" => Ok(instr(CmpSLt, at)),
        "sle" => Ok(instr(CmpSLe, at)),

        "push8" => Ok(instr(Push8 { imm: imm(input)? }, at)),
        "push32" => Ok(instr(Push32 { imm: imm(input)? }, at)),
        "push64" => Ok(instr(Push64 { imm: imm(input)? }, at)),
        "host" => Ok(instr(Host { index: imm(input)? }, at)),

        "alloc" => Ok(instr(Alloc { n: size(input)? }, at)),
        "free" => Ok(instr(Free { n: size(input)? }, at)),

        "lds8" => Ok(instr(Lds8 { disp: imm(input)? }, at)),
        "lds32" => Ok(instr(Lds32 { disp: imm(input)? }, at)),
        "lds64" => Ok(instr(Lds64 { disp: imm(input)? }, at)),
        "sts8" => Ok(instr(Sts8 { disp: imm(input)? }, at)),
        "sts32" => Ok(instr(Sts32 { disp: imm(input)? }, at)),
        "sts64" => Ok(instr(Sts64 { disp: imm(input)? }, at)),

        "jmp" | "jz" | "jnz" => branch(input, &name, at),

        "br" => {
            let then = label(input)?;
            input.parse::<Token![,]>()?;
            let els = label(input)?;
            Ok(Stmt::Term(RawTerm::Br { then, els }, at))
        }

        "switch" if input.peek(token::Bracket) => table(input, at),
        "switch" => Ok(instr(Switch, at)),

        "field" => field_ref(input, at),

        "load" | "store" => {
            let cell = cell(input, cells)?;
            let item = if name == "load" {
                Item::Load(cell)
            } else {
                Item::Store(cell)
            };
            Ok(Stmt::Item(RawItem::Core(item), at))
        }

        "push" => base(input, at),

        other => Err(syn::Error::new(
            at,
            format!("unknown instruction `{other}`"),
        )),
    }
}

/// Wraps an instruction as a statement.
fn instr(instr: impl Into<Instr>, at: Span) -> Stmt {
    Stmt::Item(RawItem::Core(Item::Instr(instr.into())), at)
}

/// Whether nothing is left of the current block.
fn ends_block(input: ParseStream) -> bool {
    input.is_empty() || opens_block(input)
}

/// `jmp`, `jz` or `jnz`: an edge if a label follows, the raw instruction if a
/// number does.
fn branch(input: ParseStream, name: &str, at: Span) -> syn::Result<Stmt> {
    if input.peek(Ident::peek_any) {
        let target = label(input)?;
        let term = match name {
            "jmp" => RawTerm::Jmp(target),
            "jz" => RawTerm::Jz(target),
            _ => RawTerm::Jnz(target),
        };
        return Ok(Stmt::Term(term, at));
    }

    let offset = offset(input)?;
    Ok(match name {
        "jmp" => instr(Jmp { offset }, at),
        "jz" => instr(Jz { offset }, at),
        _ => instr(Jnz { offset }, at),
    })
}

/// The jump table: `switch [b1, b2] default b0`.
fn table(input: ParseStream, at: Span) -> syn::Result<Stmt> {
    let body;
    bracketed!(body in input);

    let mut arms = Vec::new();
    while !body.is_empty() {
        arms.push(label(&body)?);
        if body.is_empty() {
            break;
        }
        body.parse::<Token![,]>()?;
    }

    let keyword = Ident::parse_any(input)?;
    if keyword.unraw() != "default" {
        return Err(syn::Error::new(
            keyword.span(),
            "a table needs a `default` arm: an index that matches nothing has to go somewhere",
        ));
    }

    let default = label(input)?;
    Ok(Stmt::Term(RawTerm::Switch { arms, default }, at))
}

/// `push .input` and friends: the address a region begins at.
fn base(input: ParseStream, at: Span) -> syn::Result<Stmt> {
    if !input.peek(Token![.]) {
        return Err(syn::Error::new(
            at,
            "`push` needs a width — `push8`, `push32`, `push64` — or a region, as in `push .input`",
        ));
    }
    input.parse::<Token![.]>()?;

    let name = Ident::parse_any(input)?;
    let region = match name.unraw().to_string().as_str() {
        "input" => Region::Input,
        "scratch" => Region::Scratch,
        "stack" => Region::Stack,
        other => {
            return Err(syn::Error::new(
                name.span(),
                format!("unknown region `.{other}`; there are `.input`, `.scratch` and `.stack`"),
            ));
        }
    };

    Ok(Stmt::Item(RawItem::Core(Item::Base(region)), at))
}

/// `field Packet::header.seq`: a type, then a dotted field path. The last `::`
/// segment starts the path; `.` continues it.
fn field_ref(input: ParseStream, at: Span) -> syn::Result<Stmt> {
    let full: Path = input.parse()?;
    if full.segments.len() < 2 {
        return Err(syn::Error::new(
            at,
            "`field` needs a type and a field, as in `field Packet::header.seq`",
        ));
    }

    let leading_colon = full.leading_colon;
    let mut segments: Vec<_> = full.segments.into_iter().collect();
    let Some(first) = segments.pop() else {
        return Err(syn::Error::new(at, "`field` needs a type and a field"));
    };

    let ty = Path {
        leading_colon,
        segments: segments.into_iter().collect(),
    };

    let mut path = vec![first.ident];
    while input.peek(Token![.]) {
        input.parse::<Token![.]>()?;
        path.push(Ident::parse_any(input)?.unraw());
    }

    Ok(Stmt::Item(RawItem::Field { ty, path }, at))
}

/// A label, keywords included: `loop:` is a perfectly good name for a block.
fn label(input: ParseStream) -> syn::Result<Ident> {
    Ok(Ident::parse_any(input)?.unraw())
}

/// The cell an access names.
fn cell(input: ParseStream, cells: &[CellDecl]) -> syn::Result<CellId> {
    let name = Ident::parse_any(input)?.unraw();
    let index = cell_index(cells, &name).ok_or_else(|| {
        syn::Error::new(
            name.span(),
            format!("unknown cell `{name}`; cells are declared in `.frame`"),
        )
    })?;

    Ok(CellId::from_index(index))
}

/// An integer operand, at whatever width the instruction takes it.
///
/// The suffix is refused rather than ignored: `push8 1u8` looks like it says
/// something about the operand's width, and it does not.
fn imm<T>(input: ParseStream) -> syn::Result<T>
where
    T: FromStr,
    T::Err: Display,
{
    number(input)?.base10_parse()
}

/// A frame size, which has to be word-aligned to be an operand at all.
fn size(input: ParseStream) -> syn::Result<FrameSize> {
    let literal = number(input)?;
    let bytes: u16 = literal.base10_parse()?;

    FrameSize::new(bytes).ok_or_else(|| {
        syn::Error::new(
            literal.span(),
            "a frame size must be a multiple of 8, the size of a word",
        )
    })
}

/// A branch displacement: the one operand that can be negative.
fn offset(input: ParseStream) -> syn::Result<i32> {
    let minus = input.parse::<Option<Token![-]>>()?;
    let literal = number(input)?;

    // Read into a wider type so the sign is applied before the range is
    // checked: `-2147483648` is a displacement this operand holds, and its
    // magnitude on its own is not.
    let magnitude: i64 = literal.base10_parse()?;
    let value = if minus.is_some() {
        -magnitude
    } else {
        magnitude
    };

    i32::try_from(value)
        .map_err(|_| syn::Error::new(literal.span(), "a branch displacement must fit in 32 bits"))
}

/// An unsuffixed integer literal, in any radix.
fn number(input: ParseStream) -> syn::Result<LitInt> {
    let literal: LitInt = input.parse()?;

    let suffix = literal.suffix();
    if !suffix.is_empty() {
        return Err(syn::Error::new(
            literal.span(),
            format!("an operand is a plain number; drop the `{suffix}` suffix"),
        ));
    }

    Ok(literal)
}
