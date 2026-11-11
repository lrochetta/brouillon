use bitflags::bitflags;
pub use floem_winit::keyboard::{
    Key, KeyCode, KeyLocation, ModifiersState, NamedKey, NativeKey, PhysicalKey,
};
#[cfg(not(target_arch = "wasm32"))]
pub use floem_winit::platform::modifier_supplement::KeyEventExtModifierSupplement;

/// The key payload of a [`KeyEvent`]. Normally winit's own `KeyEvent`. winit's type cannot be built outside
/// winit (private platform field), so the DEV-ONLY `test-input` feature swaps in a field-compatible struct
/// that the test channel can synthesise; only the public fields are used anywhere in the app.
#[cfg(not(feature = "test-input"))]
pub type KeyData = floem_winit::event::KeyEvent;

#[cfg(feature = "test-input")]
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct KeyData {
    pub physical_key: PhysicalKey,
    pub logical_key: Key,
    pub text: Option<floem_winit::keyboard::SmolStr>,
    pub location: KeyLocation,
    pub state: floem_winit::event::ElementState,
    pub repeat: bool,
}

#[cfg(feature = "test-input")]
impl From<floem_winit::event::KeyEvent> for KeyData {
    fn from(k: floem_winit::event::KeyEvent) -> Self {
        KeyData {
            physical_key: k.physical_key,
            logical_key: k.logical_key,
            text: k.text,
            location: k.location,
            state: k.state,
            repeat: k.repeat,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct KeyEvent {
    pub key: KeyData,
    pub modifiers: Modifiers,
}

bitflags! {
    /// Represents the current state of the keyboard modifiers
    ///
    /// Each flag represents a modifier and is set if this modifier is active.
    #[derive(Default, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct Modifiers: u32 {
        /// The "shift" key.
        const SHIFT = 0b100;
        /// The "control" key.
        const CONTROL = 0b100 << 3;
        /// The "alt" key.
        const ALT = 0b100 << 6;
        /// This is the "windows" key on PC and "command" key on Mac.
        const META = 0b100 << 9;
        /// The "altgr" key.
        const ALTGR = 0b100 << 12;
    }
}

impl Modifiers {
    /// Returns `true` if the shift key is pressed.
    pub fn shift(&self) -> bool {
        self.intersects(Self::SHIFT)
    }
    /// Returns `true` if the control key is pressed.
    pub fn control(&self) -> bool {
        self.intersects(Self::CONTROL)
    }
    /// Returns `true` if the alt key is pressed.
    pub fn alt(&self) -> bool {
        self.intersects(Self::ALT)
    }
    /// Returns `true` if the meta key is pressed.
    pub fn meta(&self) -> bool {
        self.intersects(Self::META)
    }
    /// Returns `true` if the altgr key is pressed.
    pub fn altgr(&self) -> bool {
        self.intersects(Self::ALTGR)
    }
}

impl From<ModifiersState> for Modifiers {
    fn from(value: ModifiersState) -> Self {
        let mut modifiers = Modifiers::empty();
        if value.shift_key() {
            modifiers.set(Modifiers::SHIFT, true);
        }
        if value.alt_key() {
            modifiers.set(Modifiers::ALT, true);
        }
        if value.control_key() {
            modifiers.set(Modifiers::CONTROL, true);
        }
        if value.super_key() {
            modifiers.set(Modifiers::META, true);
        }
        modifiers
    }
}
