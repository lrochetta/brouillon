//! DEV-ONLY test input channel (cargo feature `test-input`, never part of a release build).
//!
//! Lets a test driver inject keyboard, pointer and wheel events straight into the floem window handle, so a GUI test
//! needs neither OS focus, nor the foreground, nor the real mouse. Commands are queued from any thread and run on the
//! GUI thread (one `UserEvent::TestInput` wakes the winit loop). The events enter at the same place winit's own
//! `WindowEvent`s do (`WindowHandle::key_data_event`, `pointer_move`, `mouse_input`, `mouse_wheel`, `size`), so floem's
//! focus, hit-testing, hover, click counting and the app's own handlers all run exactly as for real input.
//!
//! What it does not reproduce: IME composition, OS key auto-repeat, mouse capture outside the window, the OS
//! cursor shape and `SetCursorPos`-dependent code. See docs/TESTING.md.

use std::collections::VecDeque;

use floem_winit::{
    dpi::LogicalSize,
    event::{ElementState, MouseButton, MouseScrollDelta},
    keyboard::{Key, KeyCode, KeyLocation, NamedKey, PhysicalKey, SmolStr},
    window::WindowBuilder,
};
use parking_lot::Mutex;
pub use floem_winit::keyboard::NativeKeyCode;
pub use crate::pointer::PointerButton;
use peniko::kurbo::Point;

use crate::{app::Application, app::UserEvent, keyboard::{KeyData, Modifiers}, window_handle::WindowHandle};

/// One injected action. All coordinates are window-client LOGICAL pixels (the same unit floem views use).
pub enum Cmd {
    /// Press + release of one key with the given held modifiers (modifier key events are emitted too).
    Key { logical: Key, physical: PhysicalKey, text: Option<String>, mods: Modifiers },
    /// Type text: one key press + release per character (what winit delivers for typed characters).
    Text(String),
    Move(Point),
    Button { pos: Point, button: PointerButton, down: bool },
    /// Press + release, `count` times in a row (2 = double click, 3 = triple click).
    Click { pos: Point, button: PointerButton, count: u8, mods: Modifiers },
    Drag { from: Point, to: Point, steps: u32 },
    Wheel { pos: Point, dx: f32, dy: f32 },
    /// Resize the window client area (logical px).
    Resize { w: f64, h: f64 },
    /// Run a closure on the GUI thread (state dumps read signals there).
    Run(Box<dyn FnOnce() + Send>),
    Quit,
}

static QUEUE: Mutex<VecDeque<Cmd>> = Mutex::new(VecDeque::new());

/// Queue a command and wake the event loop. Callable from any thread.
pub fn send(cmd: Cmd) {
    QUEUE.lock().push_back(cmd);
    Application::with_event_loop_proxy(|proxy| {
        let _ = proxy.send_event(UserEvent::TestInput);
    });
}

thread_local! {
    static FOCUS: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

/// Description of the floem view that owns the keyboard focus (GUI thread only; refreshed before every command, so a
/// `Cmd::Run` closure sees the focus as of that moment). Not the OS focus: floem's own focused view.
pub fn focus_owner() -> String {
    FOCUS.with(|f| f.borrow().clone())
}

pub(crate) fn drain() -> Vec<Cmd> {
    QUEUE.lock().drain(..).collect()
}

/// `BROUILLON_TEST_INPUT_BACKGROUND=1`: create the window without activating it, at the bottom of the z-order (the app then makes
/// it invisible and click-through, see brouillon's test_input), so a test run never takes the foreground and nothing appears on the owner's screen.
pub(crate) fn background_window(b: WindowBuilder) -> WindowBuilder {
    if std::env::var("BROUILLON_TEST_INPUT_BACKGROUND").map(|v| v == "1").unwrap_or(false) {
        b.with_active(false)
            .with_window_level(floem_winit::window::WindowLevel::AlwaysOnBottom)
    } else {
        b
    }
}

fn to_winit(b: PointerButton) -> MouseButton {
    match b {
        PointerButton::Secondary => MouseButton::Right,
        PointerButton::Auxiliary => MouseButton::Middle,
        PointerButton::X1 => MouseButton::Back,
        PointerButton::X2 => MouseButton::Forward,
        _ => MouseButton::Left,
    }
}

fn mod_keys(m: Modifiers) -> Vec<(NamedKey, KeyCode, Modifiers)> {
    let mut v = Vec::new();
    if m.control() {
        v.push((NamedKey::Control, KeyCode::ControlLeft, Modifiers::CONTROL));
    }
    if m.alt() {
        v.push((NamedKey::Alt, KeyCode::AltLeft, Modifiers::ALT));
    }
    if m.shift() {
        v.push((NamedKey::Shift, KeyCode::ShiftLeft, Modifiers::SHIFT));
    }
    if m.meta() {
        v.push((NamedKey::Super, KeyCode::SuperLeft, Modifiers::META));
    }
    v
}

fn key_data(logical: Key, physical: PhysicalKey, text: Option<String>, down: bool) -> KeyData {
    KeyData {
        physical_key: physical,
        logical_key: logical,
        text: text.map(|t| SmolStr::new(t)),
        location: KeyLocation::Standard,
        state: if down { ElementState::Pressed } else { ElementState::Released },
        repeat: false,
    }
}

impl WindowHandle {
    fn t_key(&mut self, logical: Key, physical: PhysicalKey, text: Option<String>, mods: Modifiers) {
        let held = mod_keys(mods);
        let mut cur = Modifiers::empty();
        for (n, c, flag) in &held {
            self.key_data_event(key_data(Key::Named(*n), PhysicalKey::Code(*c), None, true));
            cur |= *flag;
            self.modifiers = cur;
        }
        self.modifiers = mods;
        // With Ctrl or Alt held winit delivers no text for the key; keep that.
        let text = if mods.control() || mods.alt() { None } else { text };
        self.key_data_event(key_data(logical.clone(), physical, text, true));
        self.key_data_event(key_data(logical, physical, None, false));
        for (n, c, flag) in held.iter().rev() {
            cur.remove(*flag);
            self.modifiers = cur;
            self.key_data_event(key_data(Key::Named(*n), PhysicalKey::Code(*c), None, false));
        }
        self.modifiers = Modifiers::empty();
    }

    pub(crate) fn test_apply(&mut self, cmd: &mut Cmd) {
        let desc = match self.app_state.focus {
            Some(id) if id.is_alive() => {
                let names = id.state().borrow().debug_name.join("/");
                format!("{} {:?} {}", id.view().borrow().debug_name(), id, names)
            }
            Some(id) => format!("dead {:?}", id),
            None => "none".to_string(),
        };
        FOCUS.with(|f| *f.borrow_mut() = desc);
        match std::mem::replace(cmd, Cmd::Quit) {
            Cmd::Key { logical, physical, text, mods } => self.t_key(logical, physical, text, mods),
            Cmd::Text(s) => {
                for ch in s.chars() {
                    match ch {
                        '\r' => {}
                        '\n' => self.t_key(Key::Named(NamedKey::Enter), PhysicalKey::Code(KeyCode::Enter), Some("\r".into()), Modifiers::empty()),
                        '\t' => self.t_key(Key::Named(NamedKey::Tab), PhysicalKey::Code(KeyCode::Tab), Some("\t".into()), Modifiers::empty()),
                        c => {
                            let t = c.to_string();
                            let shift = c.is_uppercase();
                            let mods = if shift { Modifiers::SHIFT } else { Modifiers::empty() };
                            // winit gives Character(typed char) + text for a typed character; Shift is held for capitals.
                            self.t_key(Key::Character(SmolStr::new(&t)), PhysicalKey::Unidentified(NativeKeyCode::Unidentified), Some(t), mods)
                        }
                    }
                }
            }
            Cmd::Move(p) => self.pointer_move(p),
            Cmd::Button { pos, button, down } => {
                let button = to_winit(button);
                self.pointer_move(pos);
                self.mouse_input(button, if down { ElementState::Pressed } else { ElementState::Released });
            }
            Cmd::Click { pos, button, count, mods } => {
                let button = to_winit(button);
                self.pointer_move(pos);
                // modifiers held for the whole click (Shift+click, Ctrl+click)
                self.modifiers = mods;
                for _ in 0..count.max(1) {
                    self.mouse_input(button, ElementState::Pressed);
                    self.mouse_input(button, ElementState::Released);
                }
                self.modifiers = Modifiers::empty();
            }
            Cmd::Drag { from, to, steps } => {
                self.pointer_move(from);
                self.mouse_input(MouseButton::Left, ElementState::Pressed);
                let n = steps.max(1);
                for i in 1..=n {
                    let t = i as f64 / n as f64;
                    self.pointer_move(Point::new(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t));
                }
                self.mouse_input(MouseButton::Left, ElementState::Released);
            }
            Cmd::Wheel { pos, dx, dy } => {
                self.pointer_move(pos);
                self.mouse_wheel(MouseScrollDelta::LineDelta(dx, dy));
            }
            Cmd::Resize { w, h } => {
                if let Some(win) = self.window.as_ref() {
                    let _ = win.request_inner_size(LogicalSize::new(w, h));
                }
            }
            Cmd::Run(f) => f(),
            Cmd::Quit => crate::app::quit_app(),
        }
    }
}
