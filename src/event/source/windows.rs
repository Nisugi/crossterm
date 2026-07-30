use std::collections::VecDeque;
use std::time::Duration;

use crossterm_winapi::{Console, ConsoleMode, Handle, InputRecord, KeyEventRecord};

use crate::event::{
    sys::windows::{parse::MouseButtonsPressed, poll::WinApiPoll},
    Event,
};

#[cfg(feature = "event-stream")]
use crate::event::sys::Waker;
use crate::event::{
    internal::InternalEvent,
    source::EventSource,
    sys::ansi::parse_event,
    sys::windows::parse::{handle_key_event, handle_mouse_event},
    timeout::PollTimeout,
};

/// Byte-stream input state for consoles in VT-input mode (ConPTY under a
/// kitty-capable VT terminal such as Alacritty). Key records then carry the
/// terminal's raw escape bytes one UTF-16 unit at a time; they are decoded
/// to UTF-8 and run through the shared ANSI parser — the same one every
/// unix source uses — so kitty keyboard sequences (and everything else)
/// parse identically on Windows.
struct VtInput {
    /// Accumulates bytes until they form a complete sequence.
    buffer: Vec<u8>,
    /// Parsed events not yet handed out (one sequence can yield several).
    events: VecDeque<InternalEvent>,
    /// A pending high surrogate awaiting its low half.
    pending_surrogate: Option<u16>,
}

impl VtInput {
    fn new() -> Self {
        Self {
            buffer: Vec::new(),
            events: VecDeque::new(),
            pending_surrogate: None,
        }
    }

    /// Feed one key record's character unit. `more` means further input is
    /// already queued behind this record (drives Esc disambiguation).
    fn feed(&mut self, record: &KeyEventRecord, more: bool) {
        // In VT-input mode the byte stream arrives as key-down records;
        // synthesized key-ups (if any) carry no new bytes.
        if !record.key_down || record.u_char == 0 {
            return;
        }
        let ch = match (self.pending_surrogate.take(), record.u_char) {
            (Some(high), low @ 0xDC00..=0xDFFF) => {
                let combined =
                    0x10000 + ((high as u32 - 0xD800) << 10) + (low as u32 - 0xDC00);
                char::from_u32(combined)
            }
            (_, high @ 0xD800..=0xDBFF) => {
                self.pending_surrogate = Some(high);
                return;
            }
            (_, unit) => char::from_u32(unit as u32),
        };
        let Some(ch) = ch else { return };

        let mut utf8 = [0u8; 4];
        let encoded = ch.encode_utf8(&mut utf8).as_bytes().to_vec();
        let last = encoded.len() - 1;
        for (idx, byte) in encoded.iter().enumerate() {
            self.buffer.push(*byte);
            match parse_event(&self.buffer, idx < last || more) {
                Ok(Some(event)) => {
                    self.events.push_back(event);
                    self.buffer.clear();
                }
                Ok(None) => {
                    // Incomplete sequence - keep buffering.
                }
                Err(_) => {
                    // Unparseable - drop the sequence and resync.
                    self.buffer.clear();
                }
            }
        }
    }

    /// Re-examine a buffered lone Esc once we know no more input is queued.
    fn flush_pending(&mut self) {
        if !self.buffer.is_empty() {
            if let Ok(Some(event)) = parse_event(&self.buffer, false) {
                self.events.push_back(event);
                self.buffer.clear();
            }
        }
    }

    fn next(&mut self) -> Option<InternalEvent> {
        self.events.pop_front()
    }
}

pub(crate) struct WindowsEventSource {
    console: Console,
    poll: WinApiPoll,
    surrogate_buffer: Option<u16>,
    mouse_buttons_pressed: MouseButtonsPressed,
    /// Some(_) when the console is in VT-input mode and key records are a
    /// byte stream to parse rather than virtual-key events to map.
    vt: Option<VtInput>,
}

impl WindowsEventSource {
    pub(crate) fn new() -> std::io::Result<WindowsEventSource> {
        let console = Console::from(Handle::current_in_handle()?);

        // Under a kitty-capable VT terminal, read the raw escape byte
        // stream instead of ConPTY's re-synthesized key records (which
        // erase keypad identity). enable_raw_mode sets the console flag
        // when it runs after us; set it here too so the source works for
        // apps that create it first.
        let vt = if crate::terminal::sys::env_declares_kitty_terminal() {
            let mode = ConsoleMode::from(Handle::current_in_handle()?);
            if let Ok(current) = mode.mode() {
                // ENABLE_VIRTUAL_TERMINAL_INPUT
                let _ = mode.set_mode(current | 0x0200);
            }
            Some(VtInput::new())
        } else {
            None
        };

        Ok(WindowsEventSource {
            console,

            #[cfg(not(feature = "event-stream"))]
            poll: WinApiPoll::new(),
            #[cfg(feature = "event-stream")]
            poll: WinApiPoll::new()?,

            surrogate_buffer: None,
            mouse_buttons_pressed: MouseButtonsPressed::default(),
            vt,
        })
    }
}

impl EventSource for WindowsEventSource {
    fn try_read(&mut self, timeout: Option<Duration>) -> std::io::Result<Option<InternalEvent>> {
        let poll_timeout = PollTimeout::new(timeout);

        loop {
            // Hand out anything the VT parser already produced.
            if let Some(vt) = self.vt.as_mut() {
                if let Some(event) = vt.next() {
                    return Ok(Some(event));
                }
            }

            if let Some(event_ready) = self.poll.poll(poll_timeout.leftover())? {
                let number = self.console.number_of_console_input_events()?;
                if event_ready && number != 0 {
                    let event = match self.console.read_single_input_event()? {
                        InputRecord::KeyEvent(record) => {
                            if let Some(vt) = self.vt.as_mut() {
                                let more =
                                    self.console.number_of_console_input_events()? != 0;
                                vt.feed(&record, more);
                                if !more {
                                    // Nothing else queued: a buffered lone
                                    // Esc is a real Esc key, not a prefix.
                                    vt.flush_pending();
                                }
                                if let Some(event) = vt.next() {
                                    return Ok(Some(event));
                                }
                                None
                            } else {
                                handle_key_event(record, &mut self.surrogate_buffer)
                                    .map(InternalEvent::Event)
                            }
                        }
                        InputRecord::MouseEvent(record) => {
                            let mouse_event =
                                handle_mouse_event(record, &self.mouse_buttons_pressed);
                            self.mouse_buttons_pressed = MouseButtonsPressed {
                                left: record.button_state.left_button(),
                                right: record.button_state.right_button(),
                                middle: record.button_state.middle_button(),
                            };

                            mouse_event.map(InternalEvent::Event)
                        }
                        InputRecord::WindowBufferSizeEvent(record) => {
                            // windows starts counting at 0, unix at 1, add one to replicate unix behaviour.
                            Some(InternalEvent::Event(Event::Resize(
                                (record.size.x as i32 + 1) as u16,
                                (record.size.y as i32 + 1) as u16,
                            )))
                        }
                        InputRecord::FocusEvent(record) => {
                            let event = if record.set_focus {
                                Event::FocusGained
                            } else {
                                Event::FocusLost
                            };
                            Some(InternalEvent::Event(event))
                        }
                        _ => None,
                    };

                    if let Some(event) = event {
                        return Ok(Some(event));
                    }
                }
            }

            if poll_timeout.elapsed() {
                return Ok(None);
            }
        }
    }

    #[cfg(feature = "event-stream")]
    fn waker(&self) -> Waker {
        self.poll.waker()
    }
}
