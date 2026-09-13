//! The frame: where a program's locals live.

use crate::{FrameSize, Width};

/// A local's home: a byte offset inside the frame and a width.
///
/// A local *is* a cell, never a slot index. The frame being byte-granular is
/// what lets frame layout and aggregate layout use one packing discipline
/// instead of two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Cell {
    off: u16,
    width: Width,
}

impl Cell {
    /// Byte offset of the cell from the base of the frame.
    pub const fn off(self) -> u16 {
        self.off
    }

    /// How many of its bytes the cell owns.
    pub const fn width(self) -> Width {
        self.width
    }
}

/// Identifies a cell within one [`Frame`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CellId(u16);

impl CellId {
    /// Position of the cell in its frame.
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// The frame layout: one cell per local, and the size to reserve for them.
///
/// Offsets are `u16` because every displacement computed from them has to fit
/// the `u16` operand of `LDS`/`STS`; a frame that could not be addressed would
/// be a layout that fails much later, at materialization, instead of here.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frame {
    cells: Vec<Cell>,
    used: u16,
    size: FrameSize,
}

impl Frame {
    /// An empty frame.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a cell of `width`, naturally aligned, and returns its id.
    ///
    /// Returns `None` if the frame would grow past what a displacement can
    /// address. Cells are never reused between locals: overlapping the cells of
    /// locals whose lifetimes do not overlap is a later optimization, and the
    /// symbolic form leaves room for it.
    ///
    /// # Examples
    ///
    /// ```
    /// use safetynet_core::Width;
    /// use safetynet_core::ir::Frame;
    ///
    /// let mut frame = Frame::new();
    /// let flag = frame.add(Width::U8).expect("room");
    /// let count = frame.add(Width::U64).expect("room");
    ///
    /// // The word cell is aligned, so the byte cell leaves a seven-byte gap.
    /// assert_eq!(frame.cell(flag).map(|c| c.off()), Some(0));
    /// assert_eq!(frame.cell(count).map(|c| c.off()), Some(8));
    /// assert_eq!(frame.size().bytes(), 16);
    /// ```
    pub fn add(&mut self, width: Width) -> Option<CellId> {
        let off = self.used.checked_next_multiple_of(width.bytes())?;
        let used = off.checked_add(width.bytes())?;

        // Reject here rather than at materialization: a frame whose rounded size
        // has no `ALLOC` operand is not a frame this machine can reserve.
        let size = FrameSize::round_up(used)?;
        let id = CellId(u16::try_from(self.cells.len()).ok()?);

        self.cells.push(Cell { off, width });
        self.used = used;
        self.size = size;

        Some(id)
    }

    /// The cell `id` names.
    pub fn cell(&self, id: CellId) -> Option<Cell> {
        self.cells.get(id.index()).copied()
    }

    /// Every cell, in the order they were added.
    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    /// Bytes to reserve in the prologue: the cells, rounded up to a whole word
    /// so that `SP` stays aligned.
    pub const fn size(&self) -> FrameSize {
        self.size
    }
}
