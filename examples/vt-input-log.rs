//! Live validation for the Windows VT-input path: run inside the terminal
//! under test (e.g. `alacritty -e ...\vt-input-log.exe`), press keys, and
//! every decoded event is appended to `vt-input-log.txt` in the working
//! directory. Auto-exits after 25 seconds or on `q`.
//!
//! What to look for under a kitty-capable terminal:
//! - `supports_keyboard_enhancement = true`
//! - Numpad-8 (NumLock on) -> `Char('8')` with `state: KeyEventState(KEYPAD)`
//! - top-row 8 -> `Char('8')` with empty state
//! - typing, shifted letters, arrows, Esc all arrive normally

use std::fs::OpenOptions;
use std::io::Write;
use std::time::{Duration, Instant};

use crossterm::event::{
    poll, read, Event, KeyCode, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, supports_keyboard_enhancement};

fn main() -> std::io::Result<()> {
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open("vt-input-log.txt")?;
    writeln!(log, "==== vt-input-log run ====")?;

    // A panic in the event path would otherwise vanish with the window.
    let panic_log = std::env::current_dir()?.join("vt-input-log.txt");
    std::panic::set_hook(Box::new(move |info| {
        if let Ok(mut f) = OpenOptions::new().append(true).open(&panic_log) {
            let _ = writeln!(f, "PANIC: {info}");
        }
    }));

    enable_raw_mode()?;
    let supported = supports_keyboard_enhancement()?;
    writeln!(log, "supports_keyboard_enhancement = {supported}")?;
    log.flush()?;
    if supported {
        writeln!(log, "pushing flags...")?;
        log.flush()?;
        let result = execute!(
            std::io::stdout(),
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
            )
        );
        writeln!(log, "push result: {result:?}")?;
        log.flush()?;
    }

    writeln!(log, "flags pushed; entering loop")?;
    log.flush()?;
    print!("vt-input-log: press Numpad-8, top-row 8, arrows, type ab, Shift+A; q to quit (45s max)\r\n");

    let deadline = Instant::now() + Duration::from_secs(45);
    while Instant::now() < deadline {
        if poll(Duration::from_millis(250))? {
            let event = read()?;
            writeln!(log, "{event:?}")?;
            log.flush()?;
            if let Event::Key(key) = &event {
                if key.code == KeyCode::Char('q') {
                    break;
                }
            }
        }
    }

    if supported {
        execute!(std::io::stdout(), PopKeyboardEnhancementFlags)?;
    }
    disable_raw_mode()?;
    writeln!(log, "==== end ====")?;
    Ok(())
}
