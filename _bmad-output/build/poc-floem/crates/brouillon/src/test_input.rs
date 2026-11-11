//! DEV-ONLY test input channel (cargo feature `test-input`; refused at compile time in a release build).
//!
//! A local named pipe that lets a test driver inject keys, text, clicks, wheel and resizes into the running app,
//! take a screenshot of the window content and read a state dump, without OS focus, the real mouse or the
//! foreground. It is enabled at run time only when BOTH `BROUILLON_TEST_INPUT=<pipe name>` and
//! `BROUILLON_TEST_INPUT_TOKEN=<random token>` are set; the first line on the pipe must carry that token.
//! No network: the pipe is `\\.\pipe\<name>`. Threat model and protocol: docs/TESTING.md.
//!
//! Wire format: JSON lines. Request `{"id":1,"op":"key","chord":"ctrl+k"}`, reply `{"id":1,"ok":true,...}`.
#![cfg(feature = "test-input")]

#[cfg(all(feature = "test-input", not(debug_assertions)))]
compile_error!(
    "feature `test-input` is a DEV-ONLY test hook and must never be part of a release build. Build it in the dev \
     profile, or deliberately set CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS=true for a profiling build (docs/TESTING.md)."
);

use std::cell::RefCell;
use std::io::{BufRead, BufReader, Write};
use std::sync::mpsc;
use std::time::Duration;

use floem::keyboard::{Key, KeyCode, Modifiers, NamedKey, PhysicalKey};
use floem::peniko::kurbo::Point;
use floem::test_input::{self as ti, Cmd};
use floem::pointer::PointerButton;
use interprocess::local_socket::{prelude::*, GenericNamespaced, ListenerOptions};
use serde_json::{json, Value};

thread_local! {
    static STATE_HOOK: RefCell<Option<Box<dyn Fn() -> Value>>> = const { RefCell::new(None) };
}

/// Register (on the GUI thread) the closure that reads the app's signals for the `state` op.
pub fn set_state_hook(f: impl Fn() -> Value + 'static) {
    STATE_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(f)));
}

/// Start the pipe server when the two environment variables are present; does nothing otherwise.
pub fn start_from_env() {
    let (Ok(name), Ok(token)) = (std::env::var("BROUILLON_TEST_INPUT"), std::env::var("BROUILLON_TEST_INPUT_TOKEN")) else {
        return;
    };
    if name.is_empty() || token.len() < 16 {
        eprintln!("[test-input] refused: need BROUILLON_TEST_INPUT and a token of at least 16 characters");
        return;
    }
    brouillon_core::probe::enable();
    let listener = match name.clone().to_ns_name::<GenericNamespaced>().and_then(|n| ListenerOptions::new().name(n).create_sync()) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[test-input] cannot create the pipe: {e}");
            return;
        }
    };
    #[cfg(windows)]
    if std::env::var("BROUILLON_TEST_INPUT_BACKGROUND").map(|v| v == "1").unwrap_or(false) {
        std::thread::spawn(screenshot::make_invisible);
    }
    eprintln!("[test-input] DEV-ONLY channel listening on pipe '{name}' (this build is not a release build)");
    std::thread::spawn(move || loop {
        match listener.accept() {
            Ok(conn) => serve(conn, &token),
            Err(_) => break,
        }
    });
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

fn serve(conn: interprocess::local_socket::Stream, token: &str) {
    let mut reader = BufReader::new(&conn);
    let mut w = &conn;
    let mut line = String::new();
    // First line: {"auth":"<token>"}.
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let ok = serde_json::from_str::<Value>(&line).ok().and_then(|v| v.get("auth").and_then(|a| a.as_str()).map(|a| ct_eq(a.as_bytes(), token.as_bytes()))).unwrap_or(false);
    if !ok {
        let _ = writeln!(w, "{}", json!({"ok": false, "error": "auth"}));
        return;
    }
    let _ = writeln!(w, "{}", json!({"ok": true, "hello": "brouillon-test-input", "proto": 1}));
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let _ = writeln!(w, "{}", json!({"ok": false, "error": format!("bad json: {e}")}));
                continue;
            }
        };
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let mut reply = match handle(&req) {
            Ok(v) => v,
            Err(e) => json!({"ok": false, "error": e}),
        };
        reply["id"] = id;
        let quit = req.get("op").and_then(|o| o.as_str()) == Some("quit");
        if writeln!(w, "{reply}").is_err() || quit {
            return;
        }
    }
}

fn num(req: &Value, k: &str) -> Result<f64, String> {
    req.get(k).and_then(|v| v.as_f64()).ok_or_else(|| format!("missing number '{k}'"))
}

fn button(req: &Value) -> PointerButton {
    match req.get("button").and_then(|b| b.as_str()).unwrap_or("left") {
        "right" => PointerButton::Secondary,
        "middle" => PointerButton::Auxiliary,
        _ => PointerButton::Primary,
    }
}

/// Wait until the GUI thread ran everything queued so far.
fn barrier() -> Result<(), String> {
    run_on_gui(|| Value::Null).map(|_| ())
}

fn run_on_gui(f: impl FnOnce() -> Value + Send + 'static) -> Result<Value, String> {
    let (tx, rx) = mpsc::channel();
    ti::send(Cmd::Run(Box::new(move || {
        let _ = tx.send(f());
    })));
    rx.recv_timeout(Duration::from_secs(5)).map_err(|_| "the GUI thread did not answer within 5 s (window not up yet, or busy)".to_string())
}

fn handle(req: &Value) -> Result<Value, String> {
    let op = req.get("op").and_then(|o| o.as_str()).ok_or("missing 'op'")?;
    match op {
        "ping" => {
            barrier()?;
            Ok(json!({"ok": true}))
        }
        "key" => {
            let chord = req.get("chord").and_then(|c| c.as_str()).ok_or("missing 'chord'")?;
            let (logical, physical, text, mods) = parse_chord(chord)?;
            ti::send(Cmd::Key { logical, physical, text, mods });
            barrier()?;
            Ok(json!({"ok": true}))
        }
        // `hold`/`release` keep modifiers (`mods`: "ctrl", "ctrl+shift"...) down between requests; `tap` presses one key while they
        // are held. Together they drive "hold Ctrl, press Tab twice, release Ctrl" (the document switcher).
        "hold" | "release" => {
            let mods = parse_chord(&format!("{}+a", req.get("mods").and_then(|m| m.as_str()).ok_or("missing 'mods'")?))?.3;
            ti::send(Cmd::Hold { mods, down: op == "hold" });
            barrier()?;
            Ok(json!({"ok": true}))
        }
        "tap" => {
            let chord = req.get("chord").and_then(|c| c.as_str()).ok_or("missing 'chord'")?;
            let (logical, physical, text, mods) = parse_chord(chord)?;
            ti::send(Cmd::Tap { logical, physical, text, mods });
            barrier()?;
            Ok(json!({"ok": true}))
        }
        "text" => {
            let t = req.get("text").and_then(|c| c.as_str()).ok_or("missing 'text'")?;
            ti::send(Cmd::Text(t.to_string()));
            barrier()?;
            Ok(json!({"ok": true}))
        }
        "latency" => {
            // Types `text` one character at a time, `gap_ms` apart; for each character reports the time from the
            // injection to the end of the first frame painted afterwards (renderer finish/present call).
            let text = req.get("text").and_then(|c| c.as_str()).ok_or("missing 'text'")?;
            let gap = req.get("gap_ms").and_then(|v| v.as_u64()).unwrap_or(60);
            let _ = ti::lat_take();
            let mut missed = 0u32;
            for ch in text.chars() {
                let before = ti::lat_count();
                ti::send(Cmd::Text(ch.to_string()));
                barrier()?;
                let t0 = std::time::Instant::now();
                while ti::lat_count() == before && t0.elapsed() < Duration::from_millis(1500) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                if ti::lat_count() == before {
                    missed += 1;
                }
                std::thread::sleep(Duration::from_millis(gap));
            }
            let s = ti::lat_take();
            let col = |i: usize| s.iter().map(|r| r[i]).collect::<Vec<f64>>();
            Ok(json!({"ok": true, "ms": col(0), "handled": col(1), "wait": col(2), "paint": col(3), "missed": missed}))
        }
        "move" => {
            ti::send(Cmd::Move(Point::new(num(req, "x")?, num(req, "y")?)));
            barrier()?;
            Ok(json!({"ok": true}))
        }
        "click" => {
            let count = req.get("count").and_then(|c| c.as_u64()).unwrap_or(1) as u8;
            // optional `mods`: "shift", "ctrl+shift"... held during the click
            let mods = match req.get("mods").and_then(|m| m.as_str()) {
                Some(m) => parse_chord(&format!("{m}+a"))?.3,
                None => Modifiers::empty(),
            };
            ti::send(Cmd::Click { pos: Point::new(num(req, "x")?, num(req, "y")?), button: button(req), count, mods });
            barrier()?;
            Ok(json!({"ok": true}))
        }
        // `down` / `up`: one button edge at (x, y), so a client can leave a real-mouse gap between press and release.
        "down" | "up" => {
            ti::send(Cmd::Button { pos: Point::new(num(req, "x")?, num(req, "y")?), button: button(req), down: op == "down" });
            barrier()?;
            Ok(json!({"ok": true}))
        }
        "drag" => {
            let steps = req.get("steps").and_then(|c| c.as_u64()).unwrap_or(12) as u32;
            ti::send(Cmd::Drag { from: Point::new(num(req, "x1")?, num(req, "y1")?), to: Point::new(num(req, "x2")?, num(req, "y2")?), steps });
            barrier()?;
            Ok(json!({"ok": true}))
        }
        "wheel" => {
            // dy > 0 scrolls the content up (wheel away from the user), like winit's LineDelta.
            ti::send(Cmd::Wheel { pos: Point::new(num(req, "x")?, num(req, "y")?), dx: num(req, "dx").unwrap_or(0.0) as f32, dy: num(req, "dy")? as f32 });
            barrier()?;
            Ok(json!({"ok": true}))
        }
        "resize" => {
            ti::send(Cmd::Resize { w: num(req, "w")?, h: num(req, "h")? });
            barrier()?;
            Ok(json!({"ok": true}))
        }
        "screenshot" => {
            let path = req.get("path").and_then(|c| c.as_str()).ok_or("missing 'path'")?;
            barrier()?; // let pending input finish; the paint that follows is a separate GUI-thread turn
            std::thread::sleep(Duration::from_millis(req.get("settle_ms").and_then(|v| v.as_u64()).unwrap_or(250)));
            let (w, h) = screenshot::capture_png(std::path::Path::new(path))?;
            Ok(json!({"ok": true, "path": path, "width": w, "height": h}))
        }
        "state" => {
            let st = run_on_gui(|| {
                let mut v = STATE_HOOK.with(|h| h.borrow().as_ref().map(|f| f()).unwrap_or_else(|| json!({"hook": "not registered yet"})));
                v["focus"] = Value::String(ti::focus_owner());
                v
            })?;
            let m = brouillon_core::memtrim::mem_stats();
            let mut st = st;
            st["private_bytes"] = json!(m.private_bytes);
            st["working_set"] = json!(m.working_set);
            let n = req.get("lines").and_then(|v| v.as_u64()).unwrap_or(60) as usize;
            Ok(json!({"ok": true, "state": st, "lines": brouillon_core::probe::recent(n)}))
        }
        "quit" => {
            ti::send(Cmd::Quit);
            Ok(json!({"ok": true}))
        }
        other => Err(format!("unknown op '{other}'")),
    }
}

type ParsedKey = (Key, PhysicalKey, Option<String>, Modifiers);

/// `ctrl+shift+f`, `alt+left`, `f3`, `a`, `ctrl+,`: modifiers (ctrl shift alt meta) then one key.
fn parse_chord(chord: &str) -> Result<ParsedKey, String> {
    let lower = chord.to_lowercase();
    // A trailing '+' key ("ctrl++") is the plus sign.
    let (head, key) = if let Some(h) = lower.strip_suffix("++") { (h.to_string(), "+".to_string()) } else {
        match lower.rsplit_once('+') {
            Some((h, k)) if !k.is_empty() => (h.to_string(), k.to_string()),
            _ => (String::new(), lower.clone()),
        }
    };
    let mut mods = Modifiers::empty();
    for m in head.split('+').filter(|s| !s.is_empty()) {
        match m {
            "ctrl" | "control" => mods |= Modifiers::CONTROL,
            "shift" => mods |= Modifiers::SHIFT,
            "alt" => mods |= Modifiers::ALT,
            "meta" | "win" | "super" => mods |= Modifiers::META,
            other => return Err(format!("unknown modifier '{other}' in '{chord}'")),
        }
    }
    let named = |n: NamedKey, c: KeyCode, t: Option<&str>| Ok((Key::Named(n), PhysicalKey::Code(c), t.map(str::to_string), mods));
    match key.as_str() {
        "enter" | "return" => return named(NamedKey::Enter, KeyCode::Enter, Some("\r")),
        "tab" => return named(NamedKey::Tab, KeyCode::Tab, Some("\t")),
        "esc" | "escape" => return named(NamedKey::Escape, KeyCode::Escape, None),
        "back" | "backspace" => return named(NamedKey::Backspace, KeyCode::Backspace, None),
        "delete" | "del" => return named(NamedKey::Delete, KeyCode::Delete, None),
        "insert" => return named(NamedKey::Insert, KeyCode::Insert, None),
        "space" => return named(NamedKey::Space, KeyCode::Space, Some(" ")),
        "left" => return named(NamedKey::ArrowLeft, KeyCode::ArrowLeft, None),
        "right" => return named(NamedKey::ArrowRight, KeyCode::ArrowRight, None),
        "up" => return named(NamedKey::ArrowUp, KeyCode::ArrowUp, None),
        "down" => return named(NamedKey::ArrowDown, KeyCode::ArrowDown, None),
        "home" => return named(NamedKey::Home, KeyCode::Home, None),
        "end" => return named(NamedKey::End, KeyCode::End, None),
        "pgup" | "pageup" => return named(NamedKey::PageUp, KeyCode::PageUp, None),
        "pgdn" | "pagedown" => return named(NamedKey::PageDown, KeyCode::PageDown, None),
        "comma" => return Ok((Key::Character(",".into()), PhysicalKey::Code(KeyCode::Comma), Some(",".into()), mods)),
        "f1" => return named(NamedKey::F1, KeyCode::F1, None),
        "f2" => return named(NamedKey::F2, KeyCode::F2, None),
        "f3" => return named(NamedKey::F3, KeyCode::F3, None),
        "f4" => return named(NamedKey::F4, KeyCode::F4, None),
        "f5" => return named(NamedKey::F5, KeyCode::F5, None),
        "f6" => return named(NamedKey::F6, KeyCode::F6, None),
        "f7" => return named(NamedKey::F7, KeyCode::F7, None),
        "f8" => return named(NamedKey::F8, KeyCode::F8, None),
        "f9" => return named(NamedKey::F9, KeyCode::F9, None),
        "f10" => return named(NamedKey::F10, KeyCode::F10, None),
        "f11" => return named(NamedKey::F11, KeyCode::F11, None),
        "f12" => return named(NamedKey::F12, KeyCode::F12, None),
        _ => {}
    }
    let mut it = key.chars();
    let (Some(c), None) = (it.next(), it.next()) else { return Err(format!("unknown key '{key}' in '{chord}'")) };
    const LETTERS: [KeyCode; 26] = [
        KeyCode::KeyA, KeyCode::KeyB, KeyCode::KeyC, KeyCode::KeyD, KeyCode::KeyE, KeyCode::KeyF, KeyCode::KeyG, KeyCode::KeyH,
        KeyCode::KeyI, KeyCode::KeyJ, KeyCode::KeyK, KeyCode::KeyL, KeyCode::KeyM, KeyCode::KeyN, KeyCode::KeyO, KeyCode::KeyP,
        KeyCode::KeyQ, KeyCode::KeyR, KeyCode::KeyS, KeyCode::KeyT, KeyCode::KeyU, KeyCode::KeyV, KeyCode::KeyW, KeyCode::KeyX,
        KeyCode::KeyY, KeyCode::KeyZ,
    ];
    const DIGITS: [KeyCode; 10] = [
        KeyCode::Digit0, KeyCode::Digit1, KeyCode::Digit2, KeyCode::Digit3, KeyCode::Digit4, KeyCode::Digit5, KeyCode::Digit6,
        KeyCode::Digit7, KeyCode::Digit8, KeyCode::Digit9,
    ];
    let physical = match c {
        'a'..='z' => PhysicalKey::Code(LETTERS[(c as u8 - b'a') as usize]),
        '0'..='9' => PhysicalKey::Code(DIGITS[(c as u8 - b'0') as usize]),
        ',' => PhysicalKey::Code(KeyCode::Comma),
        '.' => PhysicalKey::Code(KeyCode::Period),
        '/' => PhysicalKey::Code(KeyCode::Slash),
        ';' => PhysicalKey::Code(KeyCode::Semicolon),
        '\'' => PhysicalKey::Code(KeyCode::Quote),
        '[' => PhysicalKey::Code(KeyCode::BracketLeft),
        ']' => PhysicalKey::Code(KeyCode::BracketRight),
        '\\' => PhysicalKey::Code(KeyCode::Backslash),
        '-' => PhysicalKey::Code(KeyCode::Minus),
        '=' | '+' => PhysicalKey::Code(KeyCode::Equal),
        '`' => PhysicalKey::Code(KeyCode::Backquote),
        _ => PhysicalKey::Unidentified(ti::NativeKeyCode::Unidentified),
    };
    let s = if mods.shift() && c.is_ascii_lowercase() { c.to_ascii_uppercase().to_string() } else { c.to_string() };
    Ok((Key::Character(s.as_str().into()), physical, Some(s), mods))
}

#[cfg(windows)]
mod screenshot {
    //! Window content only, taken with PrintWindow(PW_CLIENTONLY | PW_RENDERFULLCONTENT): DWM renders the window into
    //! our bitmap, so the window may be behind others, off-screen or unfocused. A minimised window yields nothing.
    use std::ffi::c_void;
    use std::path::Path;

    type HWND = *mut c_void;
    type HDC = *mut c_void;
    type HGDIOBJ = *mut c_void;

    #[repr(C)]
    struct RECT {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }
    #[repr(C)]
    struct BITMAPINFOHEADER {
        size: u32,
        width: i32,
        height: i32,
        planes: u16,
        bit_count: u16,
        compression: u32,
        size_image: u32,
        xppm: i32,
        yppm: i32,
        clr_used: u32,
        clr_important: u32,
    }

    #[link(name = "user32")]
    extern "system" {
        fn EnumWindows(cb: unsafe extern "system" fn(HWND, isize) -> i32, l: isize) -> i32;
        fn GetWindowThreadProcessId(h: HWND, pid: *mut u32) -> u32;
        fn IsWindowVisible(h: HWND) -> i32;
        fn GetClientRect(h: HWND, r: *mut RECT) -> i32;
        fn PrintWindow(h: HWND, dc: HDC, flags: u32) -> i32;
        fn IsIconic(h: HWND) -> i32;
        fn GetDC(h: HWND) -> HDC;
        fn ReleaseDC(h: HWND, dc: HDC) -> i32;
    }
    #[link(name = "gdi32")]
    extern "system" {
        fn CreateCompatibleDC(dc: HDC) -> HDC;
        fn CreateDIBSection(dc: HDC, bi: *const BITMAPINFOHEADER, usage: u32, bits: *mut *mut c_void, section: *mut c_void, offset: u32) -> HGDIOBJ;
        fn SelectObject(dc: HDC, o: HGDIOBJ) -> HGDIOBJ;
        fn DeleteObject(o: HGDIOBJ) -> i32;
        fn DeleteDC(dc: HDC) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcessId() -> u32;
    }

    struct Best {
        pid: u32,
        h: HWND,
        area: i64,
    }

    unsafe extern "system" fn visit(h: HWND, l: isize) -> i32 {
        let b = &mut *(l as *mut Best);
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, &mut pid);
        if pid != b.pid || IsWindowVisible(h) == 0 {
            return 1;
        }
        let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        if GetClientRect(h, &mut r) != 0 {
            let a = (r.right - r.left) as i64 * (r.bottom - r.top) as i64;
            if a > b.area {
                b.area = a;
                b.h = h;
            }
        }
        1
    }

    #[link(name = "user32")]
    extern "system" {
        fn GetWindowLongPtrW(h: HWND, i: i32) -> isize;
        fn SetWindowLongPtrW(h: HWND, i: i32, v: isize) -> isize;
        fn SetLayeredWindowAttributes(h: HWND, key: u32, alpha: u8, flags: u32) -> i32;
    }

    /// Background mode: once the window exists, make it a layered, click-through, no-activate tool window with
    /// alpha 1/255. It stays on-screen (so it is painted and PrintWindow can read it) but the owner cannot see it,
    /// click it by accident, or find it in the taskbar / Alt+Tab.
    pub fn make_invisible() {
        for _ in 0..400 {
            std::thread::sleep(std::time::Duration::from_millis(25));
            // SAFETY: Win32 calls on a window handle of this process.
            unsafe {
                let mut best = Best { pid: GetCurrentProcessId(), h: std::ptr::null_mut(), area: 0 };
                EnumWindows(visit, &mut best as *mut Best as isize);
                if best.h.is_null() {
                    continue;
                }
                const GWL_EXSTYLE: i32 = -20;
                // LAYERED 0x80000 | TRANSPARENT 0x20 | TOOLWINDOW 0x80 | NOACTIVATE 0x08000000
                let ex = GetWindowLongPtrW(best.h, GWL_EXSTYLE);
                SetWindowLongPtrW(best.h, GWL_EXSTYLE, ex | 0x0008_00A0 | 0x0800_0000);
                SetLayeredWindowAttributes(best.h, 0, 1, 2);
                return;
            }
        }
    }

    pub fn capture_png(path: &Path) -> Result<(u32, u32), String> {
        // SAFETY: plain Win32 calls on handles we create and release here; the DIB memory is read before DeleteObject.
        unsafe {
            let mut best = Best { pid: GetCurrentProcessId(), h: std::ptr::null_mut(), area: 0 };
            for _ in 0..20 {
                EnumWindows(visit, &mut best as *mut Best as isize);
                if !best.h.is_null() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            if best.h.is_null() {
                return Err("no visible window of this process".into());
            }
            if IsIconic(best.h) != 0 {
                return Err("the window is minimised; PrintWindow cannot render it (use the off-screen mode instead)".into());
            }
            let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
            GetClientRect(best.h, &mut r);
            let (w, h) = ((r.right - r.left) as u32, (r.bottom - r.top) as u32);
            if w == 0 || h == 0 {
                return Err("empty client area".into());
            }
            let screen = GetDC(std::ptr::null_mut());
            let mem = CreateCompatibleDC(screen);
            let bi = BITMAPINFOHEADER { size: std::mem::size_of::<BITMAPINFOHEADER>() as u32, width: w as i32, height: -(h as i32), planes: 1, bit_count: 32, compression: 0, size_image: 0, xppm: 0, yppm: 0, clr_used: 0, clr_important: 0 };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let bmp = CreateDIBSection(screen, &bi, 0, &mut bits, std::ptr::null_mut(), 0);
            let result = if bmp.is_null() || bits.is_null() {
                Err("CreateDIBSection failed".to_string())
            } else {
                let old = SelectObject(mem, bmp);
                // PW_CLIENTONLY (1) | PW_RENDERFULLCONTENT (2)
                let ok = PrintWindow(best.h, mem, 3);
                let px = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize);
                let mut rgb = Vec::with_capacity((w * h * 3) as usize);
                for p in px.chunks_exact(4) {
                    rgb.extend_from_slice(&[p[2], p[1], p[0]]);
                }
                SelectObject(mem, old);
                if ok == 0 { Err("PrintWindow failed".to_string()) } else { super::write_png(path, w, h, &rgb).map(|_| (w, h)) }
            };
            if !bmp.is_null() {
                DeleteObject(bmp);
            }
            DeleteDC(mem);
            ReleaseDC(std::ptr::null_mut(), screen);
            result
        }
    }
}

#[cfg(not(windows))]
mod screenshot {
    pub fn make_invisible() {}
    pub fn capture_png(_: &std::path::Path) -> Result<(u32, u32), String> {
        Err("screenshot is implemented for Windows only".into())
    }
}

fn crc32(data: &[u8], mut crc: u32) -> u32 {
    crc = !crc;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// Minimal PNG writer (RGB8, zlib "stored" blocks): no dependency, files are large but exact.
fn write_png(path: &std::path::Path, w: u32, h: u32, rgb: &[u8]) -> Result<(), String> {
    fn chunk(out: &mut Vec<u8>, ty: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(ty);
        out.extend_from_slice(data);
        let c = crc32(data, crc32(ty, 0));
        out.extend_from_slice(&c.to_be_bytes());
    }
    let mut raw = Vec::with_capacity(rgb.len() + h as usize);
    for row in rgb.chunks_exact(w as usize * 3) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65535).collect();
    for (i, b) in blocks.iter().enumerate() {
        z.push(if i + 1 == blocks.len() { 1 } else { 0 });
        z.extend_from_slice(&(b.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(b.len() as u16)).to_le_bytes());
        z.extend_from_slice(b);
    }
    let (mut a, mut b2) = (1u32, 0u32);
    for &x in &raw {
        a = (a + x as u32) % 65521;
        b2 = (b2 + a) % 65521;
    }
    z.extend_from_slice(&((b2 << 16) | a).to_be_bytes());
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    std::fs::write(path, out).map_err(|e| format!("write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords_parse_with_modifiers_and_named_keys() {
        let (k, p, t, m) = parse_chord("ctrl+shift+K").unwrap();
        assert_eq!(k, Key::Character("K".into()));
        assert_eq!(p, PhysicalKey::Code(KeyCode::KeyK));
        assert_eq!(t.as_deref(), Some("K"));
        assert!(m.control() && m.shift() && !m.alt());
        let (k, _, _, m) = parse_chord("f3").unwrap();
        assert_eq!(k, Key::Named(NamedKey::F3));
        assert!(m.is_empty());
        let (k, p, _, _) = parse_chord("ctrl+1").unwrap();
        assert_eq!(k, Key::Character("1".into()));
        assert_eq!(p, PhysicalKey::Code(KeyCode::Digit1));
        assert!(parse_chord("hyper+x").is_err());
        assert!(parse_chord("ctrl+nokey").is_err());
    }

    #[test]
    fn png_has_signature_and_valid_chunk_crcs() {
        let dir = std::env::temp_dir().join("brouillon-ti-png-test.png");
        let rgb: Vec<u8> = (0..(5 * 3 * 3)).map(|i| i as u8).collect();
        write_png(&dir, 5, 3, &rgb).unwrap();
        let b = std::fs::read(&dir).unwrap();
        let _ = std::fs::remove_file(&dir);
        assert_eq!(&b[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        // IHDR crc
        assert_eq!(u32::from_be_bytes(b[29..33].try_into().unwrap()), crc32(&b[12..29], 0));
    }

    #[test]
    fn token_compare_is_exact() {
        assert!(ct_eq(b"abcd", b"abcd"));
        assert!(!ct_eq(b"abcd", b"abce"));
        assert!(!ct_eq(b"abc", b"abcd"));
    }
}
