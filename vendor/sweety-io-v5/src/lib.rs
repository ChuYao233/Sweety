//! Async traits and types used for Io operations.

#![forbid(unsafe_code)]

pub mod bytes;
#[cfg(feature = "runtime")]
pub mod io;
#[cfg(feature = "runtime")]
pub mod net;
