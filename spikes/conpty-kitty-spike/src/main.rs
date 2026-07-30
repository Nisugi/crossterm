//! ConPTY / kitty-keyboard passthrough spike (crossterm phase-2 gate).
//!
//! Run inside the terminal under test (e.g. `alacritty -e conpty-kitty-spike.exe`).
//! Phase A (automated): put the console input handle into VT-input mode, send
//! the kitty enhancement query + primary DA fence, and record whether the
//! responses come back through ConPTY as raw bytes.
//! Phase B (one keypress): push kitty flags (disambiguate | report-all-keys),
//! prompt for Numpad-8, and check whether it arrives as CSI 57407 u.
//!
//! Everything is appended to spike-log.txt next to the exe; the parent session
//! reads that after the run. Exit code: 0 = both phases positive, 2 = phase A
//! positive only, 3 = phase A negative (ConPTY mangles the response path).

use std::io::Write as _;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows_sys::Win32::System::Console::{
    GetConsoleMode, GetStdHandle, ReadConsoleInputW, SetConsoleMode, WriteConsoleW,
    ENABLE_PROCESSED_OUTPUT, ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
    INPUT_RECORD, KEY_EVENT, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::Threading::WaitForSingleObject;

struct Log(std::fs::File);

impl Log {
    fn line(&mut self, s: &str) {
        let _ = writeln!(self.0, "{s}");
        let _ = self.0.flush();
    }
}

fn write_out(handle: HANDLE, s: &str) {
    let utf16: Vec<u16> = s.encode_utf16().collect();
    let mut written = 0u32;
    unsafe {
        WriteConsoleW(
            handle,
            utf16.as_ptr() as *const _,
            utf16.len() as u32,
            &mut written,
            std::ptr::null(),
        );
    }
}

/// Drain key-event bytes for up to `window`, appending the UnicodeChar of
/// every key-down record to `bytes` and logging each record raw.
fn collect(
    input: HANDLE,
    log: &mut Log,
    bytes: &mut String,
    window: Duration,
    stop_when: Option<&str>,
) {
    let deadline = Instant::now() + window;
    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let wait_ms = (deadline - now).as_millis() as u32;
        let signaled = unsafe { WaitForSingleObject(input, wait_ms.min(250)) };
        if signaled != WAIT_OBJECT_0 {
            if let Some(needle) = stop_when {
                if bytes.contains(needle) {
                    break;
                }
            }
            continue;
        }
        let mut records: [INPUT_RECORD; 32] = unsafe { std::mem::zeroed() };
        let mut read = 0u32;
        let ok = unsafe { ReadConsoleInputW(input, records.as_mut_ptr(), 32, &mut read) };
        if ok == 0 {
            log.line("ReadConsoleInputW FAILED");
            break;
        }
        for record in records.iter().take(read as usize) {
            if record.EventType as u32 == KEY_EVENT as u32 {
                let key = unsafe { record.Event.KeyEvent };
                let ch = unsafe { key.uChar.UnicodeChar };
                log.line(&format!(
                    "  KEY down={} vk=0x{:02x} scan=0x{:02x} ch=0x{:04x} ({})",
                    key.bKeyDown,
                    key.wVirtualKeyCode,
                    key.wVirtualScanCode,
                    ch,
                    printable(ch),
                ));
                if key.bKeyDown != 0 && ch != 0 {
                    if let Some(c) = char::from_u32(ch as u32) {
                        bytes.push(c);
                    }
                }
            } else {
                log.line(&format!("  (event type {})", record.EventType));
            }
        }
        if let Some(needle) = stop_when {
            if bytes.contains(needle) {
                break;
            }
        }
    }
}

fn printable(ch: u16) -> String {
    match ch {
        0x1b => "ESC".to_string(),
        0x20..=0x7e => char::from_u32(ch as u32).unwrap().to_string(),
        _ => format!("#{ch}"),
    }
}

fn escaped(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c == '\u{1b}' {
                "\\e".to_string()
            } else if c.is_ascii_graphic() || c == ' ' {
                c.to_string()
            } else {
                format!("\\u{:04x}", c as u32)
            }
        })
        .collect()
}

fn main() {
    let exe = std::env::current_exe().unwrap();
    let log_path = exe.parent().unwrap().join("spike-log.txt");
    let mut log = Log(std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .unwrap());
    log.line("==== conpty-kitty-spike run ====");

    let (input, output) = unsafe { (GetStdHandle(STD_INPUT_HANDLE), GetStdHandle(STD_OUTPUT_HANDLE)) };

    let mut in_mode = 0u32;
    let mut out_mode = 0u32;
    unsafe {
        GetConsoleMode(input, &mut in_mode);
        GetConsoleMode(output, &mut out_mode);
    }
    log.line(&format!("original modes: in=0x{in_mode:04x} out=0x{out_mode:04x}"));

    // Output: VT processing on. Input: VT input ONLY (no line/echo/processed).
    let set_out = unsafe {
        SetConsoleMode(
            output,
            ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING,
        )
    };
    let set_in = unsafe { SetConsoleMode(input, ENABLE_VIRTUAL_TERMINAL_INPUT) };
    log.line(&format!("set modes: out_ok={set_out} in_ok={set_in} (in=VT_INPUT only)"));

    write_out(output, "\x1b[2J\x1b[H\r\n  ConPTY kitty spike\r\n\r\n");

    if std::env::args().any(|a| a == "--probe") {
        // ---- Detection probe: XTVERSION (CSI > 0 q) + DA1 fence. Does
        // conhost forward XTVERSION to the terminal, and does the terminal's
        // answer (DCS > | <name> <version> ST) come back through?
        log.line("PROBE: sending CSI > 0 q + CSI c");
        write_out(output, "\x1b[>0q\x1b[c");
        let mut bytes = String::new();
        collect(input, &mut log, &mut bytes, Duration::from_secs(3), Some("c"));
        log.line(&format!("PROBE bytes: {}", escaped(&bytes)));
        unsafe {
            SetConsoleMode(input, in_mode);
            SetConsoleMode(output, out_mode);
        }
        std::thread::sleep(Duration::from_secs(1));
        std::process::exit(0);
    }

    let push_only = std::env::args().any(|a| a == "--push-only");
    if push_only {
        // ---- Push-only experiment: skip the query (conhost answers that
        // itself); blind-push kitty flags and see what a numpad press sends.
        log.line("PUSH-ONLY: sending CSI > 9 u, waiting for numpad-8");
        write_out(output, "\x1b[>9u");
        write_out(
            output,
            "\r\n  *** PRESS NUMPAD-8 NOW (NumLock ON) ***\r\n  (closes by itself, 30s max)\r\n",
        );
        let mut bytes = String::new();
        collect(
            input,
            &mut log,
            &mut bytes,
            Duration::from_secs(30),
            Some("57407"),
        );
        log.line(&format!("PUSH-ONLY bytes: {}", escaped(&bytes)));
        let kp8 = bytes.contains("\u{1b}[57407");
        let plain8 = bytes.contains('8') && !kp8;
        log.line(&format!("PUSH-ONLY verdict: kp8_csi_u={kp8} plain_8={plain8}"));
        write_out(output, "\x1b[<u");
        unsafe {
            SetConsoleMode(input, in_mode);
            SetConsoleMode(output, out_mode);
        }
        let code = if kp8 { 0 } else { 2 };
        log.line(&format!("RESULT: {code}"));
        std::thread::sleep(Duration::from_secs(1));
        std::process::exit(code);
    }

    // ---- Phase A: kitty enhancement query + DA1 fence ----
    log.line("PHASE A: sending CSI ? u + CSI c");
    write_out(output, "\x1b[?u\x1b[c");
    let mut a_bytes = String::new();
    collect(input, &mut log, &mut a_bytes, Duration::from_secs(3), Some("c"));
    log.line(&format!("PHASE A bytes: {}", escaped(&a_bytes)));
    // kitty reply: ESC [ ? <flags> u ; DA1 reply: ESC [ ? ... c
    let a_kitty = a_bytes.contains("\u{1b}[?") && a_bytes.contains('u');
    let a_da1 = {
        // find a "\e[?...c" AFTER stripping a kitty reply's 'u' — cheap check:
        a_bytes.rfind('c').is_some() && a_bytes.matches("\u{1b}[?").count() >= if a_kitty { 2 } else { 1 }
    };
    log.line(&format!("PHASE A verdict: kitty_reply={a_kitty} da1_reply={a_da1}"));

    if !a_kitty {
        write_out(output, "  Phase A negative (no kitty reply) - closing.\r\n");
        log.line("RESULT: 3 (phase A negative)");
        std::thread::sleep(Duration::from_secs(2));
        unsafe {
            SetConsoleMode(input, in_mode);
            SetConsoleMode(output, out_mode);
        }
        std::process::exit(3);
    }

    // ---- Phase B: push flags, ask for numpad-8 ----
    log.line("PHASE B: pushing CSI > 9 u (disambiguate|report-all-keys)");
    write_out(output, "\x1b[>9u");
    write_out(
        output,
        "\r\n  *** PRESS NUMPAD-8 NOW (NumLock ON) ***\r\n  (window closes by itself, 30s max)\r\n",
    );
    let mut b_bytes = String::new();
    collect(
        input,
        &mut log,
        &mut b_bytes,
        Duration::from_secs(30),
        Some("\u{1b}[57407"),
    );
    log.line(&format!("PHASE B bytes: {}", escaped(&b_bytes)));
    let b_kp8 = b_bytes.contains("\u{1b}[57407");
    let b_plain8 = b_bytes.contains('8') && !b_kp8;
    log.line(&format!("PHASE B verdict: kp8_csi_u={b_kp8} plain_8={b_plain8}"));

    write_out(output, "\x1b[<u"); // pop kitty flags
    unsafe {
        SetConsoleMode(input, in_mode);
        SetConsoleMode(output, out_mode);
    }

    let code = if b_kp8 { 0 } else { 2 };
    log.line(&format!("RESULT: {code}"));
    write_out(output, "  Done - closing.\r\n");
    std::thread::sleep(Duration::from_secs(1));
    std::process::exit(code);
}
