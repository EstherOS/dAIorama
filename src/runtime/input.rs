//! Input (05, nine control tools; P5-INPUT-TOOLS): in-binary virtual input over the session's
//! own Wayland socket. One connection per call — no subprocess per event (the whole point of the
//! in-binary ruling) and no state to pool or leak across sessions.
//!
//! Pointer: `zwlr_virtual_pointer_v1`. `pointer_move` is in **screenshot pixels**; the compositor's
//! absolute space is logical, so the call divides by the session scale and passes logical extents.
//! Keyboard: `zwp_virtual_keyboard_v1` with a built-in US keymap (v1 contract — no libxkbcommon at
//! runtime, no layout detection). Text outside the US table is refused with the offset, never
//! silently dropped.
//!
//! Missing protocol is the canonical `internal` code with a message naming the interface (05's
//! set is closed; no new code).

use std::os::unix::net::UnixStream;

use wayland_client::globals::GlobalListContents;
pub use wayland_client::protocol::wl_keyboard::KeyState;
use wayland_client::protocol::wl_pointer::{Axis, ButtonState};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;
use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1;
use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1;

use crate::ipc::ErrorEnvelope;

/// The session-side facts input needs. Built from `CaptureEnv` + the record's geometry.
#[derive(Debug, Clone)]
pub struct Target {
    pub runtime_dir: std::path::PathBuf,
    pub wayland_display: String,
    /// Logical output size (the compositor's coordinate space).
    pub width: u32,
    pub height: u32,
    /// Session scale; screenshot pixels are logical × scale.
    pub scale: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InputError {
    MissingProtocol(&'static str),
    UnknownKey(String),
    BadText { offset: usize },
    BadTarget(String),
    Connection(String),
}

impl InputError {
    pub fn envelope(&self) -> ErrorEnvelope {
        match self {
            InputError::MissingProtocol(iface) => ErrorEnvelope::internal(format!(
                "compositor does not offer {iface}; virtual input is unavailable for this session"
            )),
            InputError::UnknownKey(key) => {
                ErrorEnvelope::bad_request(format!("unknown key {key:?}"))
            }
            InputError::BadText { offset } => ErrorEnvelope::bad_request(format!(
                "text contains a character outside the US keymap at byte {offset}"
            )),
            InputError::BadTarget(detail) => ErrorEnvelope::internal(detail.clone()),
            InputError::Connection(detail) => ErrorEnvelope::internal(detail.clone()),
        }
    }
}

/// Buttons are the three 05 names, mapped to Linux input codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Middle,
}

impl Button {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "left" => Some(Button::Left),
            "right" => Some(Button::Right),
            "middle" => Some(Button::Middle),
            _ => None,
        }
    }

    fn code(self) -> u32 {
        match self {
            Button::Left => 0x110,
            Button::Right => 0x111,
            Button::Middle => 0x112,
        }
    }
}

struct State;

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _state: &mut State,
        _: &WlRegistry,
        _: wayland_client::protocol::wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        _state: &mut State,
        _: &WlSeat,
        _: wayland_client::protocol::wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrVirtualPointerManagerV1, ()> for State {
    fn event(
        _state: &mut State,
        _: &ZwlrVirtualPointerManagerV1,
        _: <ZwlrVirtualPointerManagerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrVirtualPointerV1, ()> for State {
    fn event(
        _state: &mut State,
        _: &ZwlrVirtualPointerV1,
        _: <ZwlrVirtualPointerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpVirtualKeyboardManagerV1, ()> for State {
    fn event(
        _state: &mut State,
        _: &ZwpVirtualKeyboardManagerV1,
        _: <ZwpVirtualKeyboardManagerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpVirtualKeyboardV1, ()> for State {
    fn event(
        _state: &mut State,
        _: &ZwpVirtualKeyboardV1,
        _: <ZwpVirtualKeyboardV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

/// A connected session compositor with the two virtual-input devices created.
struct Input {
    connection: Connection,
    event_queue: wayland_client::EventQueue<State>,
    pointer: ZwlrVirtualPointerV1,
    keyboard: ZwpVirtualKeyboardV1,
    target: Target,
}

impl Input {
    fn connect(target: &Target) -> Result<Self, InputError> {
        let socket = target.runtime_dir.join(&target.wayland_display);
        if !socket.exists() {
            return Err(InputError::Connection(format!(
                "session socket {} is missing",
                socket.display()
            )));
        }
        let stream = UnixStream::connect(&socket).map_err(|err| {
            InputError::Connection(format!("cannot connect to {}: {err}", socket.display()))
        })?;
        let connection = Connection::from_socket(stream)
            .map_err(|err| InputError::Connection(format!("wayland connection: {err}")))?;
        let (globals, event_queue) =
            wayland_client::globals::registry_queue_init::<State>(&connection)
                .map_err(|err| InputError::Connection(format!("wayland registry: {err}")))?;
        let qh = event_queue.handle();
        let seat: WlSeat = globals
            .bind(&qh, 1..=9, ())
            .map_err(|err| InputError::Connection(format!("cannot bind wl_seat: {err}")))?;
        let pointer_manager: ZwlrVirtualPointerManagerV1 = globals
            .bind(&qh, 1..=2, ())
            .map_err(|_| InputError::MissingProtocol("zwlr_virtual_pointer_manager_v1"))?;
        let keyboard_manager: ZwpVirtualKeyboardManagerV1 = globals
            .bind(&qh, 1..=1, ())
            .map_err(|_| InputError::MissingProtocol("zwp_virtual_keyboard_manager_v1"))?;
        let pointer = pointer_manager.create_virtual_pointer(Some(&seat), &qh, ());
        let keyboard = keyboard_manager.create_virtual_keyboard(&seat, &qh, ());
        Ok(Self {
            connection,
            event_queue,
            pointer,
            keyboard,
            target: target.clone(),
        })
    }

    fn upload_keymap(&mut self) -> Result<(), InputError> {
        // The protocol ships the keymap as an fd the compositor mmaps and may forward to its
        // clients. A memfd with seals behaves like wlroots' own keymap buffer; a plain file is
        // what a client's `mmap` chokes on (EPERM) after the compositor adopts it.
        let mut text = US_KEYMAP.as_bytes().to_vec();
        text.push(0);
        let name = std::ffi::CString::new("daiorama-keymap").expect("static name");
        let fd = unsafe {
            libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING)
        };
        if fd < 0 {
            return Err(InputError::Connection(format!(
                "memfd_create: {}",
                std::io::Error::last_os_error()
            )));
        }
        use std::io::Write;
        use std::os::fd::{AsFd, FromRawFd};
        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        file.write_all(&text)
            .map_err(|err| InputError::Connection(format!("keymap write: {err}")))?;
        // No seals: a client may mmap the keymap writable, and a seal turns that into EPERM.
        self.keyboard.keymap(1, file.as_fd(), text.len() as u32);
        // A virtual keyboard's keymap is applied asynchronously by the compositor: a key sent in
        // the same dispatch as the keymap is translated against the not-yet-installed map and
        // silently dropped. Measured against the real chain (Hyprland 0.56.2, L1): every
        // short-lived connection lost exactly its first press/release pair, so a one-pair
        // `key_tap` delivered nothing at all and `type_text` lost its first character. The
        // settle roundtrip makes the map live before the first key leaves this connection.
        self.event_queue
            .roundtrip(&mut State)
            .map_err(|err| InputError::Connection(format!("wayland roundtrip: {err}")))?;
        Ok(())
    }

    fn pointer_move(&mut self, x: f64, y: f64) -> Result<(), InputError> {
        let scale = if self.target.scale > 0.0 {
            self.target.scale
        } else {
            1.0
        };
        let logical_x = (x / scale).round() as u32;
        let logical_y = (y / scale).round() as u32;
        self.pointer.motion_absolute(
            0,
            logical_x.min(self.target.width.saturating_sub(1)),
            logical_y.min(self.target.height.saturating_sub(1)),
            self.target.width.max(1),
            self.target.height.max(1),
        );
        self.pointer.frame();
        self.flush()
    }

    fn button(&mut self, button: Button, down: bool) -> Result<(), InputError> {
        self.pointer.button(
            0,
            button.code(),
            if down {
                ButtonState::Pressed
            } else {
                ButtonState::Released
            },
        );
        self.pointer.frame();
        self.flush()
    }

    fn scroll(&mut self, dx: f64, dy: f64) -> Result<(), InputError> {
        // One wheel line is the conventional 10 units in wl_pointer's continuous axis.
        const LINE: f64 = 10.0;
        if dx != 0.0 {
            self.pointer.axis(0, Axis::HorizontalScroll, dx * LINE);
        }
        if dy != 0.0 {
            self.pointer.axis(0, Axis::VerticalScroll, dy * LINE);
        }
        self.pointer.frame();
        self.flush()
    }

    /// A modifier must be visible to the focused client before the key it modifies: the
    /// compositor hands `wl_keyboard.modifiers` to clients on its own dispatch, so a key pressed
    /// in the same turn is translated with the old modifier state. Measured live: `ABCD` and a
    /// shifted keysym both arrived unshifted (`abc1`, `x`) until this settle was added.
    fn shift(&mut self, down: bool) -> Result<(), InputError> {
        let state = if down {
            KeyState::Pressed
        } else {
            KeyState::Released
        };
        self.key(SHIFT_L, state)?;
        // A client translates a key with the mask of the LAST `wl_keyboard.modifiers` event it
        // received, so the modifier plane has to be stated on the virtual-keyboard protocol
        // (`modifiers`), not left for the compositor to infer from the key press. Measured live:
        // without this, `ABCD` and a shifted keysym both arrived unshifted.
        let mask = if down { 1 } else { 0 };
        self.keyboard.modifiers(mask, 0, 0, 0);
        self.flush()
    }

    fn key(&mut self, key: Key, state: KeyState) -> Result<(), InputError> {
        // virtual-keyboard's `state` is the raw wl_keyboard value: 0 released, 1 pressed.
        let state = if state == KeyState::Pressed { 1 } else { 0 };
        self.keyboard.key(0, key.code, state);
        self.flush()
    }

    fn flush(&mut self) -> Result<(), InputError> {
        // A roundtrip makes the compositor process every request before this short-lived
        // connection closes, and surfaces protocol errors (bad keymap, wrong extents) as an
        // `internal` instead of a silent drop.
        self.event_queue
            .roundtrip(&mut State)
            .map(|_| ())
            .map_err(|err| InputError::Connection(format!("wayland roundtrip: {err}")))
    }
}

/// Public verbs. Each opens its own connection (see module docs).
pub fn pointer_move(target: &Target, x: f64, y: f64) -> Result<(), InputError> {
    let mut input = Input::connect(target)?;
    input.pointer_move(x, y)
}

pub fn pointer_button(target: &Target, button: Button, down: bool) -> Result<(), InputError> {
    let mut input = Input::connect(target)?;
    input.button(button, down)
}

pub fn pointer_click(target: &Target, button: Button) -> Result<(), InputError> {
    let mut input = Input::connect(target)?;
    input.button(button, true)?;
    input.button(button, false)
}

pub fn pointer_scroll(target: &Target, dx: f64, dy: f64) -> Result<(), InputError> {
    let mut input = Input::connect(target)?;
    input.scroll(dx, dy)
}

pub fn key(target: &Target, name: &str, state: KeyState) -> Result<(), InputError> {
    let key = lookup_symbol(name).ok_or_else(|| InputError::UnknownKey(name.to_string()))?;
    let mut input = Input::connect(target)?;
    input.upload_keymap()?;
    input.key(key, state)
}

pub fn key_tap(target: &Target, name: &str) -> Result<(), InputError> {
    key_tap_with(target, name, &[])
}

/// The xkb modifier bit a keysym name owns, when it is a modifier. This is the same bit layout
/// `wl_keyboard.modifiers` uses (Shift=1, Lock=2, Control=4, Mod1=8, Mod4=64).
fn modifier_bit(key: Key) -> Option<u32> {
    match key.code {
        42 | 54 => Some(1),    // Shift_L / Shift_R
        29 | 97 => Some(4),    // Control_L / Control_R
        56 => Some(8),         // Alt_L (Alt_R is AltGr/Mod5, not a held chord modifier)
        125 | 126 => Some(64), // Super_L / Super_R
        _ => None,
    }
}

pub fn modifier_bit_for(name: &str) -> Option<u32> {
    lookup_symbol(name).and_then(modifier_bit)
}

pub fn is_modifier(name: &str) -> bool {
    modifier_bit_for(name).is_some()
}

impl Input {
    /// Presses the held modifier keys and states the mask on the virtual-keyboard protocol.
    /// Returns the mask so a caller can re-state it after releasing a per-character Shift.
    fn press_modifiers(&mut self, held: &[String]) -> Result<u32, InputError> {
        let mut mask = 0;
        for name in held {
            let key =
                lookup_symbol(name).ok_or_else(|| InputError::UnknownKey(name.to_string()))?;
            let bit = modifier_bit(key).ok_or_else(|| InputError::UnknownKey(name.to_string()))?;
            self.key(key, KeyState::Pressed)?;
            mask |= bit;
        }
        if mask != 0 {
            self.keyboard.modifiers(mask, 0, 0, 0);
            self.flush()?;
        }
        Ok(mask)
    }

    fn release_modifiers(&mut self, held: &[String]) -> Result<(), InputError> {
        for name in held.iter().rev() {
            if let Some(key) = lookup_symbol(name) {
                self.key(key, KeyState::Released)?;
            }
        }
        self.keyboard.modifiers(0, 0, 0, 0);
        self.flush()
    }

    fn chord_key(&mut self, key: Key, base_mask: u32) -> Result<(), InputError> {
        // A held Shift already supplies the level; adding a second Shift press/release inside
        // the chord would be redundant and would clear the modifier mask between characters.
        let extra_shift = key.shift && (base_mask & 1) == 0;
        if extra_shift {
            self.key(SHIFT_L, KeyState::Pressed)?;
            self.keyboard.modifiers(base_mask | 1, 0, 0, 0);
            self.flush()?;
        }
        self.key(key, KeyState::Pressed)?;
        self.key(key, KeyState::Released)?;
        if extra_shift {
            self.key(SHIFT_L, KeyState::Released)?;
            self.keyboard.modifiers(base_mask, 0, 0, 0);
            self.flush()?;
        }
        Ok(())
    }
}

/// `key_tap` with modifiers held from earlier `key_down` calls: a chord is assembled inside one
/// connection, because a virtual-keyboard device (and its modifier state) dies with the call.
pub fn key_tap_with(target: &Target, name: &str, held: &[String]) -> Result<(), InputError> {
    let key = lookup_symbol(name).ok_or_else(|| InputError::UnknownKey(name.to_string()))?;
    let mut input = Input::connect(target)?;
    input.upload_keymap()?;
    let mask = input.press_modifiers(held)?;
    input.chord_key(key, mask)?;
    input.release_modifiers(held)
}

pub fn type_text(target: &Target, text: &str) -> Result<(), InputError> {
    type_text_with(target, text, &[])
}

pub fn type_text_with(target: &Target, text: &str, held: &[String]) -> Result<(), InputError> {
    let mut keys = Vec::new();
    for (offset, ch) in text.char_indices() {
        let key = match ch {
            '\n' => lookup_symbol("Return").expect("built-in table"),
            '\t' => lookup_symbol("Tab").expect("built-in table"),
            _ => lookup_char(ch).ok_or(InputError::BadText { offset })?,
        };
        keys.push(key);
    }
    let mut input = Input::connect(target)?;
    input.upload_keymap()?;
    let mask = input.press_modifiers(held)?;
    for key in keys {
        input.chord_key(key, mask)?;
    }
    input.release_modifiers(held)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Key {
    code: u32,
    shift: bool,
}

const SHIFT_L: Key = Key {
    code: 42,
    shift: false,
};

/// A complete keymap that resolves US symbols from the host's xkb data. The compositor compiles
/// it; this binary carries no xkb implementation (the pinned v1 contract).
const US_KEYMAP: &str = r#"xkb_keymap {
    xkb_keycodes  { include "evdev+aliases(qwerty)" };
    xkb_types     { include "complete" };
    xkb_compat    { include "complete" };
    xkb_symbols   { include "pc+us+inet(evdev)" };
};"#;

fn lookup_char(ch: char) -> Option<Key> {
    let (code, shift) = match ch {
        'a'..='z' => (
            match ch {
                'a' => 30,
                'b' => 48,
                'c' => 46,
                'd' => 32,
                'e' => 18,
                'f' => 33,
                'g' => 34,
                'h' => 35,
                'i' => 23,
                'j' => 36,
                'k' => 37,
                'l' => 38,
                'm' => 50,
                'n' => 49,
                'o' => 24,
                'p' => 25,
                'q' => 16,
                'r' => 19,
                's' => 31,
                't' => 20,
                'u' => 22,
                'v' => 47,
                'w' => 17,
                'x' => 45,
                'y' => 21,
                'z' => 44,
                _ => unreachable!(),
            },
            false,
        ),
        'A'..='Z' => (lookup_char(ch.to_ascii_lowercase())?.code, true),
        '0' => (11, false),
        '1'..='9' => (2 + (ch as u32 - '1' as u32), false),
        ' ' => (57, false),
        '-' => (12, false),
        '_' => (12, true),
        '=' => (13, false),
        '+' => (13, true),
        '[' => (26, false),
        '{' => (26, true),
        ']' => (27, false),
        '}' => (27, true),
        '\\' => (43, false),
        '|' => (43, true),
        ';' => (39, false),
        ':' => (39, true),
        '\'' => (40, false),
        '"' => (40, true),
        '`' => (41, false),
        '~' => (41, true),
        ',' => (51, false),
        '<' => (51, true),
        '.' => (52, false),
        '>' => (52, true),
        '/' => (53, false),
        '?' => (53, true),
        '!' => (2, true),
        '@' => (3, true),
        '#' => (4, true),
        '$' => (5, true),
        '%' => (6, true),
        '^' => (7, true),
        '&' => (8, true),
        '*' => (9, true),
        '(' => (10, true),
        ')' => (11, true),
        _ => return None,
    };
    Some(Key { code, shift })
}

fn lookup_symbol(name: &str) -> Option<Key> {
    match name {
        "Return" | "KP_Enter" => Some(Key {
            code: 28,
            shift: false,
        }),
        "Tab" => Some(Key {
            code: 15,
            shift: false,
        }),
        "BackSpace" => Some(Key {
            code: 14,
            shift: false,
        }),
        "Escape" => Some(Key {
            code: 1,
            shift: false,
        }),
        "Delete" => Some(Key {
            code: 111,
            shift: false,
        }),
        "Insert" => Some(Key {
            code: 110,
            shift: false,
        }),
        "Home" => Some(Key {
            code: 102,
            shift: false,
        }),
        "End" => Some(Key {
            code: 107,
            shift: false,
        }),
        "Page_Up" => Some(Key {
            code: 104,
            shift: false,
        }),
        "Page_Down" => Some(Key {
            code: 109,
            shift: false,
        }),
        "Left" => Some(Key {
            code: 105,
            shift: false,
        }),
        "Right" => Some(Key {
            code: 106,
            shift: false,
        }),
        "Up" => Some(Key {
            code: 103,
            shift: false,
        }),
        "Down" => Some(Key {
            code: 108,
            shift: false,
        }),
        "Shift_L" => Some(SHIFT_L),
        "Shift_R" => Some(Key {
            code: 54,
            shift: false,
        }),
        "Control_L" => Some(Key {
            code: 29,
            shift: false,
        }),
        "Control_R" => Some(Key {
            code: 97,
            shift: false,
        }),
        "Alt_L" => Some(Key {
            code: 56,
            shift: false,
        }),
        "Alt_R" => Some(Key {
            code: 100,
            shift: false,
        }),
        "Super_L" => Some(Key {
            code: 125,
            shift: false,
        }),
        "Super_R" => Some(Key {
            code: 126,
            shift: false,
        }),
        "Caps_Lock" => Some(Key {
            code: 58,
            shift: false,
        }),
        "F1" => Some(Key {
            code: 59,
            shift: false,
        }),
        "F2" => Some(Key {
            code: 60,
            shift: false,
        }),
        "F3" => Some(Key {
            code: 61,
            shift: false,
        }),
        "F4" => Some(Key {
            code: 62,
            shift: false,
        }),
        "F5" => Some(Key {
            code: 63,
            shift: false,
        }),
        "F6" => Some(Key {
            code: 64,
            shift: false,
        }),
        "F7" => Some(Key {
            code: 65,
            shift: false,
        }),
        "F8" => Some(Key {
            code: 66,
            shift: false,
        }),
        "F9" => Some(Key {
            code: 67,
            shift: false,
        }),
        "F10" => Some(Key {
            code: 68,
            shift: false,
        }),
        "F11" => Some(Key {
            code: 87,
            shift: false,
        }),
        "F12" => Some(Key {
            code: 88,
            shift: false,
        }),
        "space" => Some(Key {
            code: 57,
            shift: false,
        }),
        other => {
            // Keysym names for shifted punctuation; the unshifted ASCII form is the key itself.
            let ch = match other {
                "exclam" => '!',
                "at" => '@',
                "numbersign" => '#',
                "dollar" => '$',
                "percent" => '%',
                "asciicircum" => '^',
                "ampersand" => '&',
                "asterisk" => '*',
                "parenleft" => '(',
                "parenright" => ')',
                "underscore" => '_',
                "plus" => '+',
                "braceleft" => '{',
                "braceright" => '}',
                "bar" => '|',
                "colon" => ':',
                "quotedbl" => '"',
                "asciitilde" => '~',
                "less" => '<',
                "greater" => '>',
                "question" => '?',
                other => {
                    // A one-character keysym name is the character itself: `xkb_keysym_from_name("a")`
                    // is 0x61, and 05's examples name `a` beside `Return`. Resolve it through the same
                    // US table so a single letter is a valid key name.
                    let mut chars = other.chars();
                    return match (chars.next(), chars.next()) {
                        (Some(ch), None) => lookup_char(ch),
                        _ => None,
                    };
                }
            };
            Some(lookup_char(ch).expect("shifted table"))
        }
    }
}

/// Resolve a `{session_id}` control call into a Target. Session state/visibility is the caller's
/// job; this only reads geometry and env facts.
pub fn target(
    env: &crate::manager::CaptureEnv,
    resolution: &str,
    scale: f64,
) -> Result<Target, InputError> {
    let (width, height) = parse_resolution(resolution).ok_or_else(|| {
        InputError::BadTarget(format!("session resolution {resolution:?} is not <w>x<h>"))
    })?;
    Ok(Target {
        runtime_dir: env.rt_dir.clone(),
        wayland_display: env.wayland_display.clone(),
        width,
        height,
        scale: if scale > 0.0 { scale } else { 1.0 },
    })
}

pub fn parse_resolution(text: &str) -> Option<(u32, u32)> {
    let (width, height) = text.split_once('x')?;
    let width = width.parse().ok()?;
    let height = height.parse().ok()?;
    (width > 0 && height > 0).then_some((width, height))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_falls_out_of_the_us_table_and_the_rest_refuses_with_an_offset() {
        assert_eq!(
            lookup_char('a'),
            Some(Key {
                code: 30,
                shift: false
            })
        );
        assert_eq!(
            lookup_char('A'),
            Some(Key {
                code: 30,
                shift: true
            })
        );
        assert_eq!(
            lookup_char('!'),
            Some(Key {
                code: 2,
                shift: true
            })
        );
        assert_eq!(lookup_char('\n'), None, "newline is handled by type_text");
        assert_eq!(lookup_char('é'), None);
    }

    #[test]
    fn keysym_names_resolve_through_the_same_table() {
        assert_eq!(
            lookup_symbol("Return"),
            Some(Key {
                code: 28,
                shift: false
            })
        );
        assert_eq!(
            lookup_symbol("exclam"),
            Some(Key {
                code: 2,
                shift: true
            })
        );
        assert_eq!(lookup_symbol("Shift_L"), Some(SHIFT_L));
        // A one-character keysym name is the character itself (05 names `a` beside `Return`).
        assert_eq!(lookup_symbol("a"), lookup_char('a'));
        assert_eq!(lookup_symbol("A"), lookup_char('A'));
        assert_eq!(lookup_symbol("1"), lookup_char('1'));
        assert!(lookup_symbol("NotAKey").is_none());
    }

    #[test]
    fn resolution_parsing_refuses_junk_and_zero() {
        assert_eq!(parse_resolution("1280x720"), Some((1280, 720)));
        assert_eq!(parse_resolution("0x720"), None);
        assert_eq!(parse_resolution("1280"), None);
    }

    #[test]
    fn missing_protocol_is_internal_and_names_the_interface() {
        let envelope = InputError::MissingProtocol("zwlr_virtual_pointer_manager_v1").envelope();
        assert_eq!(envelope.code, crate::ipc::ErrorCode::Internal);
        assert!(envelope.message.contains("zwlr_virtual_pointer_manager_v1"));
    }

    #[test]
    fn text_with_an_unsupported_character_names_the_byte_offset() {
        let envelope = InputError::BadText { offset: 3 }.envelope();
        assert_eq!(envelope.code, crate::ipc::ErrorCode::BadRequest);
        assert!(envelope.message.contains("byte 3"), "{}", envelope.message);
    }

    /// Manual leg against a real compositor (not run in CI):
    /// `DAIO_LIVE_SOCKET=/run/user/x/wayland-1 DAIO_LIVE_W=1280 DAIO_LIVE_H=720
    ///  DAIO_LIVE_SCALE=1 DAIO_LIVE_OP=move DAIO_LIVE_X=100 DAIO_LIVE_Y=100
    ///  cargo test --bin daiorama live_probe -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_probe() {
        let socket = std::env::var("DAIO_LIVE_SOCKET").expect("DAIO_LIVE_SOCKET");
        let path = std::path::PathBuf::from(socket);
        let target = Target {
            runtime_dir: path.parent().unwrap().to_path_buf(),
            wayland_display: path.file_name().unwrap().to_string_lossy().into_owned(),
            width: std::env::var("DAIO_LIVE_W").unwrap().parse().unwrap(),
            height: std::env::var("DAIO_LIVE_H").unwrap().parse().unwrap(),
            scale: std::env::var("DAIO_LIVE_SCALE").unwrap().parse().unwrap(),
        };
        let op = std::env::var("DAIO_LIVE_OP").unwrap();
        let outcome = match op.as_str() {
            "move" => pointer_move(
                &target,
                std::env::var("DAIO_LIVE_X").unwrap().parse().unwrap(),
                std::env::var("DAIO_LIVE_Y").unwrap().parse().unwrap(),
            ),
            "click" => pointer_click(
                &target,
                Button::parse(&std::env::var("DAIO_LIVE_BUTTON").unwrap()).unwrap(),
            ),
            "scroll" => pointer_scroll(&target, 0.0, 1.0),
            "key" => key_tap(&target, &std::env::var("DAIO_LIVE_KEY").unwrap()),
            "text" => type_text(&target, &std::env::var("DAIO_LIVE_TEXT").unwrap()),
            other => panic!("unknown op {other}"),
        };
        match outcome {
            Ok(()) => println!("LIVE_OK {op}"),
            Err(err) => panic!("LIVE_FAIL {op}: {err:?}"),
        }
    }

    #[test]
    fn modifier_keys_map_to_the_wl_modifier_bits() {
        assert_eq!(modifier_bit_for("Shift_L"), Some(1));
        assert_eq!(modifier_bit_for("Shift_R"), Some(1));
        assert_eq!(modifier_bit_for("Caps_Lock"), None);
        assert_eq!(modifier_bit_for("Control_L"), Some(4));
        assert_eq!(modifier_bit_for("Control_R"), Some(4));
        assert_eq!(modifier_bit_for("Alt_L"), Some(8));
        assert_eq!(modifier_bit_for("Super_L"), Some(64));
        assert_eq!(modifier_bit_for("a"), None);
        assert_eq!(modifier_bit_for("Return"), None);
        assert!(is_modifier("Shift_L"));
        assert!(!is_modifier("NotAKey"));
    }
}
