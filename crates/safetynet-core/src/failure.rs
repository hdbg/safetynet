//! How a generated wrapper fails: with the reason under `debug`, and with a
//! bare panic otherwise.
//!
//! The wrapper cannot know at expansion time whether the runtime was built
//! with the feature, so it hands the reason here and this decides what to say.

use crate::ir::NotFinal;
use crate::vm::Trap;

/// Why a wrapper could not run its program.
#[cfg_attr(feature = "debug", derive(Debug))]
pub enum Failure {
    /// The regions do not fit an image.
    ImageDoesNotFit,
    /// The marshalled input is larger than its region.
    InputDoesNotFit,
    /// The marshalled input is larger than a region can be.
    InputTooLarge,
    /// A byte region's content is longer than its header can record.
    RegionTooLong,
    /// The artifact did not finalize against the layout.
    NotFinal(NotFinal),
    /// The program trapped.
    Trap(Trap),
}

#[cfg(feature = "debug")]
impl core::fmt::Display for Failure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::ImageDoesNotFit => f.write_str("the image does not fit"),
            Self::InputDoesNotFit => f.write_str("the input does not fit"),
            Self::InputTooLarge => f.write_str("the input is too large"),
            Self::RegionTooLong => f.write_str("a byte region is too long for its header"),
            Self::NotFinal(error) => error.fmt(f),
            Self::Trap(trap) => trap.fmt(f),
        }
    }
}

crate::opaque_debug!(Failure);

/// Stops with `failure` as the reason.
///
/// # Panics
///
/// Always. With `debug` the message names the reason; without it the message
/// is one fixed word, and the reason goes nowhere.
#[cold]
#[inline(never)]
pub fn fail(failure: Failure) -> ! {
    #[cfg(feature = "debug")]
    {
        panic!("safetynet: {failure}");
    }
    #[cfg(not(feature = "debug"))]
    {
        let _ = failure;
        panic!("safetynet");
    }
}
