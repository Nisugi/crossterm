#[cfg(all(unix, feature = "event-stream"))]
pub(crate) use unix::waker::Waker;
#[cfg(all(windows, feature = "event-stream"))]
pub(crate) use windows::waker::Waker;

/// OS-independent ANSI/VT byte-stream parsing (incl. the kitty keyboard
/// protocol). Unix sources always read this format; the Windows source
/// reads it too when the console is in VT-input mode (ConPTY under a VT
/// terminal such as Alacritty).
#[cfg(feature = "events")]
pub(crate) mod ansi;

#[cfg(unix)]
pub(crate) mod unix;
#[cfg(windows)]
pub(crate) mod windows;
