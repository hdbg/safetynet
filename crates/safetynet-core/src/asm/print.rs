//! A graph as assembly text.
//!
//! The form printed here is the one the assembler parses, so it is a fixed
//! point rather than a debug dump: printing a graph, parsing the text and
//! printing again has to give back the same characters.

use core::fmt::Write as _;

use crate::ir::{Block, BlockId, Cfg, Item, Terminator};
use crate::{Instr, Region, Width};

/// Prints a graph as assembly text.
///
/// # Examples
///
/// ```
/// use safetynet_core::asm::print;
/// use safetynet_core::ir::{Cfg, Frame, Terminator};
/// use safetynet_core::isa::Push8;
///
/// let mut builder = Cfg::builder(Frame::new());
/// let entry = builder.block(0);
/// builder.at(entry).expect("open").instr(Push8 { imm: 7 });
/// builder.seal(entry, Terminator::Halt).expect("seals");
/// let cfg = builder.build(entry).expect("builds");
///
/// assert_eq!(print(&cfg), "b0:\n    push8 7\n    halt\n");
/// ```
pub fn print(cfg: &Cfg) -> String {
    let mut out = String::new();
    // Writing into a `String` cannot fail; the `Result` belongs to the trait.
    let _ = write_cfg(&mut out, cfg);
    out
}

/// Prints instructions one per line, with nothing around them.
///
/// The linear half of the same text: what decoded bytecode looks like, once
/// there are no blocks or cells left to name.
pub fn print_listing(code: &[Instr]) -> String {
    let mut out = String::new();
    for instr in code {
        let _ = writeln!(out, "{instr}");
    }
    out
}

/// Indent for everything inside a block.
const INDENT: &str = "    ";

fn write_cfg(out: &mut String, cfg: &Cfg) -> core::fmt::Result {
    let cells = cfg.frame().cells();
    if !cells.is_empty() {
        out.push_str(".frame {");
        for (index, cell) in cells.iter().enumerate() {
            let separator = if index == 0 { " " } else { ", " };
            write!(out, "{separator}c{index}: {}", width(cell.width()))?;
        }
        out.push_str(" }\n");
    }

    // Block zero is where execution starts unless something says otherwise, so
    // the common case spells nothing.
    if cfg.entry().index() != 0 {
        writeln!(out, ".entry b{}", cfg.entry().index())?;
    }

    for (position, block) in cfg.blocks().iter().enumerate() {
        let next = cfg.blocks().get(position + 1).map(Block::id);

        writeln!(out, "b{}:", block.id().index())?;
        for item in block.code() {
            out.push_str(INDENT);
            write_item(out, *item)?;
            out.push('\n');
        }

        out.push_str(INDENT);
        write_term(out, block.term(), next)?;
        out.push('\n');
    }

    Ok(())
}

fn write_item(out: &mut String, item: Item) -> core::fmt::Result {
    match item {
        Item::Instr(instr) => write!(out, "{instr}"),
        Item::Load(cell) => write!(out, "load c{}", cell.index()),
        Item::Store(cell) => write!(out, "store c{}", cell.index()),
        Item::Base(region) => write!(out, "push .{}", region_name(region)),
    }
}

fn write_term(out: &mut String, term: &Terminator, next: Option<BlockId>) -> core::fmt::Result {
    match term {
        Terminator::Halt => out.push_str("halt"),
        Terminator::Jmp(target) => write!(out, "jmp b{}", target.index())?,

        // A conditional spells the arm that is not reached by falling through.
        // `then` is the non-zero arm, so falling into `els` leaves `jnz`.
        Terminator::Br { then, els } if next == Some(*els) => {
            write!(out, "jnz b{}", then.index())?;
        }
        Terminator::Br { then, els } if next == Some(*then) => {
            write!(out, "jz b{}", els.index())?;
        }
        Terminator::Br { then, els } => {
            write!(out, "br b{}, b{}", then.index(), els.index())?;
        }

        Terminator::Switch { arms, default } => {
            out.push_str("switch [");
            for (index, arm) in arms.iter().enumerate() {
                let separator = if index == 0 { "" } else { ", " };
                write!(out, "{separator}b{}", arm.index())?;
            }
            write!(out, "] default b{}", default.index())?;
        }
    }

    Ok(())
}

/// How a width is spelled in a frame declaration.
const fn width(width: Width) -> &'static str {
    match width {
        Width::U8 => "u8",
        Width::U32 => "u32",
        Width::U64 => "u64",
    }
}

/// How a region is spelled after `push .`.
const fn region_name(region: Region) -> &'static str {
    match region {
        Region::Input => "input",
        Region::Scratch => "scratch",
        Region::Stack => "stack",
    }
}
