use bevy::ecs::world::FromWorld;
use bevy::input::ButtonState;
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::prelude::*;

use arboard::Clipboard;

use crate::mouse::TerminalSelection;
use crate::runtime::TerminalRuntime;
use crate::scene::TerminalPresentation;
use crate::terminal::TerminalRedrawState;

pub struct TerminalClipboard {
    clipboard: Option<Clipboard>,
}

impl FromWorld for TerminalClipboard {
    fn from_world(_world: &mut World) -> Self {
        Self {
            clipboard: Clipboard::new().ok(),
        }
    }
}

impl TerminalClipboard {
    fn copy(&mut self, text: &str) {
        let Some(clipboard) = self.clipboard.as_mut() else {
            warn!("clipboard unavailable for copy");
            return;
        };

        if let Err(error) = clipboard.set_text(text.to_owned()) {
            warn!("failed to copy terminal selection to clipboard: {error}");
        }
    }

    fn paste(&mut self) -> Option<String> {
        let clipboard = self.clipboard.as_mut()?;
        clipboard.get_text().ok()
    }
}

#[derive(Default)]
pub struct TerminalKeyboard {
    pub(crate) ctrl_pressed: bool,
    pub(crate) alt_pressed: bool,
}

impl TerminalKeyboard {
    pub fn handle_event(&mut self, event: &KeyboardInput) -> Option<Vec<u8>> {
        match event.key_code {
            KeyCode::ControlLeft | KeyCode::ControlRight => {
                self.ctrl_pressed = event.state == ButtonState::Pressed;
                return None;
            }
            KeyCode::AltLeft | KeyCode::AltRight => {
                self.alt_pressed = event.state == ButtonState::Pressed;
                return None;
            }
            _ => {}
        }

        if event.state != ButtonState::Pressed {
            return None;
        }

        Some(translate_key(
            event.key_code,
            &event.logical_key,
            event.text.as_deref(),
            self.ctrl_pressed,
            self.alt_pressed,
        ))
    }
}

pub fn handle_keyboard_input(
    mut keyboard_events: MessageReader<KeyboardInput>,
    mut keyboard: Local<TerminalKeyboard>,
    mut selection: ResMut<TerminalSelection>,
    mut presentation: ResMut<TerminalPresentation>,
    mut clipboard: NonSendMut<TerminalClipboard>,
    runtime: NonSend<TerminalRuntime>,
    mut redraw: ResMut<TerminalRedrawState>,
) {
    for event in keyboard_events.read() {
        if event.state == ButtonState::Pressed && !event.repeat && event.key_code == KeyCode::F2 {
            presentation.toggle();
            selection.clear();
            redraw.request();
            continue;
        }

        if event.state == ButtonState::Pressed && !event.repeat {
            if is_ctrl_alt_shortcut(&keyboard, KeyCode::KeyC, event.key_code) {
                if let Some(text) = selection.selected_text(runtime.parser.screen())
                    && !text.is_empty()
                {
                    clipboard.copy(&text);
                }
                if selection.clear() {
                    redraw.request();
                }
                continue;
            }

            if is_ctrl_alt_shortcut(&keyboard, KeyCode::KeyV, event.key_code) {
                if let Some(text) = clipboard.paste() {
                    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
                    let mut bytes = Vec::from(b"\x1b[200~".as_slice());
                    bytes.extend_from_slice(normalized.as_bytes());
                    bytes.extend_from_slice(b"\x1b[201~");
                    runtime.write_input(&bytes);
                } else {
                    warn!("failed to read clipboard contents for paste");
                }
                if selection.clear() {
                    redraw.request();
                }
                continue;
            }
        }

        if event.state == ButtonState::Pressed
            && !is_modifier_key(event.key_code)
            && selection.clear()
        {
            redraw.request();
        }

        if let Some(input) = keyboard.handle_event(event) {
            runtime.write_input(&input);
        }
    }
}

fn translate_key(
    key_code: KeyCode,
    logical_key: &Key,
    text: Option<&str>,
    ctrl_pressed: bool,
    alt_pressed: bool,
) -> Vec<u8> {
    let mut bytes = Vec::new();

    if ctrl_pressed {
        if let Some(ctrl) = ctrl_keycode_byte(key_code) {
            if alt_pressed {
                bytes.push(0x1b);
            }
            bytes.push(ctrl);
            return bytes;
        }
    }

    if alt_pressed {
        bytes.push(0x1b);
    }

    match key_code {
        KeyCode::Enter | KeyCode::NumpadEnter => bytes.push(b'\r'),
        KeyCode::Tab => bytes.push(b'\t'),
        KeyCode::Space => bytes.push(b' '),
        KeyCode::Backspace => bytes.push(0x7f),
        KeyCode::Escape => bytes.push(0x1b),
        KeyCode::ArrowUp => {
            if ctrl_pressed {
                bytes.extend_from_slice(b"\x1b[1;5A");
            } else {
                bytes.extend_from_slice(b"\x1b[A");
            }
        }
        KeyCode::ArrowDown => {
            if ctrl_pressed {
                bytes.extend_from_slice(b"\x1b[1;5B");
            } else {
                bytes.extend_from_slice(b"\x1b[B");
            }
        }
        KeyCode::ArrowRight => {
            if ctrl_pressed {
                bytes.extend_from_slice(b"\x1b[1;5C");
            } else {
                bytes.extend_from_slice(b"\x1b[C");
            }
        }
        KeyCode::ArrowLeft => {
            if ctrl_pressed {
                bytes.extend_from_slice(b"\x1b[1;5D");
            } else {
                bytes.extend_from_slice(b"\x1b[D");
            }
        }
        KeyCode::Delete => bytes.extend_from_slice(b"\x1b[3~"),
        KeyCode::Home => bytes.extend_from_slice(b"\x1b[H"),
        KeyCode::End => bytes.extend_from_slice(b"\x1b[F"),
        KeyCode::PageUp => bytes.extend_from_slice(b"\x1b[5~"),
        KeyCode::PageDown => bytes.extend_from_slice(b"\x1b[6~"),
        _ => {
            if let Some(text) = text {
                bytes.extend_from_slice(text.as_bytes());
            } else if let Key::Character(chars) = logical_key {
                bytes.extend_from_slice(chars.as_bytes());
            }
        }
    }

    bytes
}

fn is_ctrl_alt_shortcut(
    keyboard: &TerminalKeyboard,
    shortcut_key: KeyCode,
    event_key: KeyCode,
) -> bool {
    event_key == shortcut_key && keyboard.ctrl_pressed && keyboard.alt_pressed
}

fn is_modifier_key(key: KeyCode) -> bool {
    matches!(
        key,
        KeyCode::ControlLeft
            | KeyCode::ControlRight
            | KeyCode::AltLeft
            | KeyCode::AltRight
            | KeyCode::ShiftLeft
            | KeyCode::ShiftRight
            | KeyCode::SuperLeft
            | KeyCode::SuperRight
    )
}

fn ctrl_keycode_byte(key: KeyCode) -> Option<u8> {
    match key {
        KeyCode::KeyA => Some(0x01),
        KeyCode::KeyB => Some(0x02),
        KeyCode::KeyC => Some(0x03),
        KeyCode::KeyD => Some(0x04),
        KeyCode::KeyE => Some(0x05),
        KeyCode::KeyF => Some(0x06),
        KeyCode::KeyG => Some(0x07),
        KeyCode::KeyH => Some(0x08),
        KeyCode::KeyI => Some(0x09),
        KeyCode::KeyJ => Some(0x0a),
        KeyCode::KeyK => Some(0x0b),
        KeyCode::KeyL => Some(0x0c),
        KeyCode::KeyM => Some(0x0d),
        KeyCode::KeyN => Some(0x0e),
        KeyCode::KeyO => Some(0x0f),
        KeyCode::KeyP => Some(0x10),
        KeyCode::KeyQ => Some(0x11),
        KeyCode::KeyR => Some(0x12),
        KeyCode::KeyS => Some(0x13),
        KeyCode::KeyT => Some(0x14),
        KeyCode::KeyU => Some(0x15),
        KeyCode::KeyV => Some(0x16),
        KeyCode::KeyW => Some(0x17),
        KeyCode::KeyX => Some(0x18),
        KeyCode::KeyY => Some(0x19),
        KeyCode::KeyZ => Some(0x1a),
        _ => None,
    }
}
