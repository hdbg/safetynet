//! What a human-readable form is when the `debug` feature is off: nothing.
//!
//! Every public type must implement `Debug`, and every error `Display` and
//! `Error`, for the traits and bounds to keep working. Without the feature the
//! implementations write nothing, so no type, variant, field or message name
//! is in the binary for a host that formats one.

/// Implements `Debug` as silence.
macro_rules! opaque_debug {
    ($($ty:ident $(<$($g:ident : $b:path),*>)?),* $(,)?) => {$(
        #[cfg(not(feature = "debug"))]
        impl$(<$($g: $b),*>)? core::fmt::Debug for $ty$(<$($g),*>)? {
            fn fmt(&self, _: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                Ok(())
            }
        }
    )*};
}

/// Implements `Debug`, `Display` and `Error` as silence.
macro_rules! opaque_error {
    ($($ty:ident),* $(,)?) => {$(
        crate::opaque_debug!($ty);

        #[cfg(not(feature = "debug"))]
        impl core::fmt::Display for $ty {
            fn fmt(&self, _: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                Ok(())
            }
        }

        #[cfg(not(feature = "debug"))]
        impl core::error::Error for $ty {}
    )*};
}

pub(crate) use {opaque_debug, opaque_error};
