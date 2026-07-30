#[cfg(feature = "event-stream")]
pub(crate) mod waker;

// The byte parser is OS-independent and shared with the Windows VT-input
// path; the old `sys::unix::parse` path stays valid via this re-export.
#[cfg(feature = "events")]
pub(crate) use super::ansi as parse;
