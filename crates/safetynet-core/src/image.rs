//! The memory image: what regions a program's address space has, and where.
//!
//! The *order* of the regions is fixed; their addresses are not. Bases are
//! computed per program from the sizes that build actually needs, and both the
//! host and the bytecode read them from the same layout — so an address is never
//! a constant two sides have to keep agreeing on.
use crate::WORD_SIZE;

/// Every region starts on a word boundary. Not decoration: `.stack` has to, or
/// `SP` is unaligned for the whole run, and a region that began mid-word would
/// make every multi-byte access in it straddle one.
const ALIGN: u32 = WORD_SIZE as u32;

/// A region of the address space.
///
/// Listed in the order they are laid out. The stack is last so that it is the
/// only thing that can grow into the end of the image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Region {
    /// What the host put there before the run, sized by the caller rather than
    /// by the program.
    ///
    /// **Untrusted**: every byte of it is someone else's, so a length, an index
    /// or a discriminant read out of it is a claim to check, not a fact.
    Input,
    /// The program's own workspace, sized by what the program needs.
    ///
    /// Also the only place a program can keep something it addresses
    /// absolutely, since nothing can take the address of a frame local yet.
    Scratch,
    /// The frame and the operand stack, growing upward. `SP` is bounded to this
    /// region in both directions.
    Stack,
}

/// Where a region ended up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    base: u32,
    len: u32,
}

impl Span {
    /// Address of the region's first byte.
    pub const fn base(self) -> u32 {
        self.base
    }

    /// How many bytes it holds.
    pub const fn len(self) -> u32 {
        self.len
    }

    /// Whether it holds nothing.
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    /// One past its last byte.
    pub const fn end(self) -> u32 {
        self.base + self.len
    }

    /// Its bytes, as a range into the address space.
    pub fn range(self) -> core::ops::Range<usize> {
        self.base as usize..self.end() as usize
    }
}

/// How big each region needs to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Sizes {
    /// Bytes the host will write.
    pub input: u32,
    /// Bytes of workspace.
    pub scratch: u32,
    /// Bytes for the frame and the operand stack together.
    pub stack: u32,
}

/// Where every region of one program's address space lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Layout {
    input: Span,
    scratch: Span,
    stack: Span,
    size: u32,
}

impl Layout {
    /// Lays out the regions in order, each starting on a word boundary.
    ///
    /// Returns `None` if the image would not fit in a `u32` address space.
    ///
    /// # Examples
    ///
    /// ```
    /// use safetynet_core::image::{Layout, Region, Sizes};
    ///
    /// let layout = Layout::new(Sizes {
    ///     input: 12,
    ///     stack: 1024,
    ///     ..Sizes::default()
    /// })
    /// .expect("fits");
    ///
    /// assert_eq!(layout.span(Region::Input).base(), 0);
    /// // Rounded up from 12: the next region starts on a word boundary.
    /// assert_eq!(layout.span(Region::Scratch).base(), 16);
    /// assert_eq!(layout.span(Region::Stack).len(), 1024);
    /// ```
    pub fn new(sizes: Sizes) -> Option<Self> {
        let mut at = 0;

        Some(Self {
            input: place(&mut at, sizes.input)?,
            scratch: place(&mut at, sizes.scratch)?,
            stack: place(&mut at, sizes.stack)?,
            size: at,
        })
    }

    /// Where `region` ended up.
    ///
    /// Total: every region exists in every image, empty if the program had no
    /// use for it.
    pub const fn span(&self, region: Region) -> Span {
        match region {
            Region::Input => self.input,
            Region::Scratch => self.scratch,
            Region::Stack => self.stack,
        }
    }

    /// The whole address space, in bytes.
    pub const fn size(&self) -> u32 {
        self.size
    }
}

/// Places a region at `at` and moves it past, to the next word boundary.
fn place(at: &mut u32, len: u32) -> Option<Span> {
    let span = Span { base: *at, len };
    *at = at.checked_add(len)?.checked_next_multiple_of(ALIGN)?;
    Some(span)
}

/// A program's address space, with its regions in known places.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    memory: Vec<u8>,
    layout: Layout,
}

impl Image {
    /// A zeroed image with this layout.
    pub fn new(layout: Layout) -> Self {
        Self {
            memory: vec![0; layout.size() as usize],
            layout,
        }
    }

    /// Where everything is.
    pub const fn layout(&self) -> &Layout {
        &self.layout
    }

    /// The whole address space.
    pub fn memory(&self) -> &[u8] {
        &self.memory
    }

    /// The bytes of one region.
    pub fn region(&self, region: Region) -> &[u8] {
        self.memory
            .get(self.layout.span(region).range())
            .unwrap_or_default()
    }

    /// Fills the start of a region, as a host does before a run.
    ///
    /// Returns `None` if the bytes do not fit, rather than writing past the
    /// region into whatever follows it.
    pub fn write(&mut self, region: Region, bytes: &[u8]) -> Option<()> {
        let span = self.layout.span(region);
        let end = span.base as usize + bytes.len();

        if bytes.len() > span.len() as usize {
            return None;
        }

        self.memory
            .get_mut(span.base as usize..end)?
            .copy_from_slice(bytes);
        Some(())
    }

    /// Takes the address space, leaving the layout behind.
    pub fn into_memory(self) -> Vec<u8> {
        self.memory
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regions sit in a fixed order, each on a word boundary, and an empty one
    /// still has an address — it just has nowhere to put anything.
    #[test]
    fn regions_are_laid_out_in_order() {
        let layout = Layout::new(Sizes {
            input: 12,
            scratch: 1,
            stack: 256,
        })
        .expect("fits");

        let span = |region| layout.span(region);
        assert_eq!(span(Region::Input).base(), 0);
        assert_eq!(span(Region::Scratch).base(), 16, "12 rounded up");
        assert_eq!(span(Region::Stack).base(), 24, "1 byte still costs a word");
        assert_eq!(layout.size(), 280);

        for region in [Region::Input, Region::Scratch, Region::Stack] {
            assert_eq!(span(region).base() % 8, 0, "{region:?}");
        }
    }

    /// `SP` starts at the base of the stack, so an unaligned base would leave it
    /// unaligned for the whole run.
    #[test]
    fn the_stack_starts_aligned_whatever_precedes_it() {
        for input in 0..24 {
            let layout = Layout::new(Sizes {
                input,
                stack: 64,
                ..Sizes::default()
            })
            .expect("fits");

            assert_eq!(layout.span(Region::Stack).base() % 8, 0, "input {input}");
        }
    }

    #[test]
    fn an_image_that_cannot_be_addressed_is_refused() {
        assert!(
            Layout::new(Sizes {
                input: u32::MAX,
                stack: 64,
                ..Sizes::default()
            })
            .is_none()
        );
    }

    /// Writing a region is bounded by that region, not by the image: spilling
    /// into whatever follows is how a marshalling bug becomes a corrupt program
    /// rather than an error.
    #[test]
    fn a_write_stays_inside_its_region() {
        let layout = Layout::new(Sizes {
            input: 4,
            stack: 64,
            ..Sizes::default()
        })
        .expect("fits");
        let mut image = Image::new(layout);

        assert_eq!(image.write(Region::Input, b"abcd"), Some(()));
        assert_eq!(image.region(Region::Input), b"abcd");

        assert_eq!(image.write(Region::Input, b"abcde"), None, "one too many");
        assert_eq!(image.region(Region::Input), b"abcd", "and nothing moved");

        // The padding after the region is not part of it.
        assert_eq!(image.region(Region::Scratch), b"");
        assert_eq!(image.memory().len(), layout.size() as usize);
    }
}
