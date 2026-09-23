//! How a generated wrapper fails: with the reason under `debug`, and with a
//! bare panic otherwise.
//!
//! The wrapper cannot know at expansion time whether the runtime was built
//! with the feature, so it hands the reason here and this decides what to say.

use crate::ir::NotFinal;
use crate::vm::Trap;

/// Why a wrapper could not run its program.
#[cfg_attr(feature = "debug", derive(Debug, thiserror::Error))]
pub enum Failure {
    /// The regions do not fit an image.
    #[cfg_attr(feature = "debug", error("the image does not fit"))]
    ImageDoesNotFit,
    /// The marshalled input is larger than its region.
    #[cfg_attr(feature = "debug", error("the input does not fit"))]
    InputDoesNotFit,
    /// The marshalled input is larger than a region can be.
    #[cfg_attr(feature = "debug", error("the input is too large"))]
    InputTooLarge,
    /// A byte region's content is longer than its header can record.
    #[cfg_attr(feature = "debug", error("a byte region is too long for its header"))]
    RegionTooLong,
    /// The artifact did not finalize against the layout.
    #[cfg_attr(feature = "debug", error("{0}"))]
    NotFinal(NotFinal),
    /// The program trapped.
    #[cfg_attr(feature = "debug", error("{0}"))]
    Trap(Trap),
}

crate::opaque_error!(Failure);

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
