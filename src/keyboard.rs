use bevy::ecs::world::FromWorld;
use bevy::ecs::system::SystemParam;
use bevy::input::ButtonState;
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::prelude::*;

use arboard::Clipboard;

use crate::config::{AppConfig, BindingAction, KeyBindingConfig};
use crate::mouse::TerminalSelection;
use crate::runtime::TerminalRuntime;
use crate::scene::{TerminalPlaneWarp, TerminalPresentation, TerminalViewport};
use crate::terminal::{TerminalRedrawState, TerminalSurface};

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

#[derive(Resource)]
pub struct TerminalKeyBindings {
    bindings: Vec<KeyBinding>,
}

impl FromWorld for TerminalKeyBindings {
    fn from_world(world: &mut World) -> Self {
        let app_config = world.resource::<AppConfig>();
        let mut bindings = vec![
            KeyBinding::new(
                KeyCode::Enter,
                BindingModifiers {
                    control: true,
                    alt: true,
                    ..default()
                },
                BindingAction::ToggleMode,
            ),
            KeyBinding::new(
                KeyCode::ArrowUp,
                BindingModifiers {
                    control: true,
                    alt: true,
                    ..default()
                },
                BindingAction::IncreaseWarp,
            ),
            KeyBinding::new(
                KeyCode::ArrowDown,
                BindingModifiers {
                    control: true,
                    alt: true,
                    ..default()
                },
                BindingAction::DecreaseWarp,
            ),
            KeyBinding::new(
                KeyCode::KeyC,
                BindingModifiers {
                    control: true,
                    alt: true,
                    ..default()
                },
                BindingAction::Copy,
            ),
            KeyBinding::new(
                KeyCode::KeyV,
                BindingModifiers {
                    control: true,
                    alt: true,
                    ..default()
                },
                BindingAction::Paste,
            ),
            KeyBinding::new(
                KeyCode::Equal,
                BindingModifiers {
                    control: true,
                    ..default()
                },
                BindingAction::IncreaseFontSize,
            ),
            KeyBinding::new(
                KeyCode::NumpadAdd,
                BindingModifiers {
                    control: true,
                    ..default()
                },
                BindingAction::IncreaseFontSize,
            ),
            KeyBinding::new(
                KeyCode::Minus,
                BindingModifiers {
                    control: true,
                    ..default()
                },
                BindingAction::DecreaseFontSize,
            ),
            KeyBinding::new(
                KeyCode::NumpadSubtract,
                BindingModifiers {
                    control: true,
                    ..default()
                },
                BindingAction::DecreaseFontSize,
            ),
        ];

        for binding in &app_config.bindings.keys {
            let Some(binding) = KeyBinding::from_config(binding) else {
                warn!(
                    "ignoring invalid key binding: key={} with={}",
                    binding.key, binding.with
                );
                continue;
            };

            if let Some(index) = bindings
                .iter()
                .position(|existing| existing.same_trigger(&binding))
            {
                bindings.remove(index);
            }

            if binding.action != BindingAction::None {
                bindings.push(binding);
            }
        }

        Self { bindings }
    }
}

impl TerminalKeyBindings {
    fn action_for(&self, key_code: KeyCode, modifiers: BindingModifiers) -> Option<BindingAction> {
        self.bindings
            .iter()
            .filter(|binding| binding.key_code == key_code && binding.modifiers.matches(modifiers))
            .max_by_key(|binding| binding.modifiers.count())
            .map(|binding| binding.action)
    }
}

#[derive(Default)]
pub struct TerminalKeyboard {
    pub(crate) ctrl_pressed: bool,
    pub(crate) alt_pressed: bool,
    pub(crate) shift_pressed: bool,
    pub(crate) super_pressed: bool,
}

impl TerminalKeyboard {
    pub fn handle_event_with_modes(
        &mut self,
        event: &KeyboardInput,
        application_cursor: bool,
    ) -> Option<Vec<u8>> {
        match event.key_code {
            KeyCode::ControlLeft | KeyCode::ControlRight => {
                self.ctrl_pressed = event.state == ButtonState::Pressed;
                return None;
            }
            KeyCode::AltLeft | KeyCode::AltRight => {
                self.alt_pressed = event.state == ButtonState::Pressed;
                return None;
            }
            KeyCode::ShiftLeft | KeyCode::ShiftRight => {
                self.shift_pressed = event.state == ButtonState::Pressed;
                return None;
            }
            KeyCode::SuperLeft | KeyCode::SuperRight => {
                self.super_pressed = event.state == ButtonState::Pressed;
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
            application_cursor,
        ))
    }

    fn modifiers(&self) -> BindingModifiers {
        BindingModifiers {
            control: self.ctrl_pressed,
            alt: self.alt_pressed,
            shift: self.shift_pressed,
            super_key: self.super_pressed,
        }
    }
}

#[derive(SystemParam)]
pub struct KeyboardSystemParams<'w, 's> {
    selection: ResMut<'w, TerminalSelection>,
    plane_warp: ResMut<'w, TerminalPlaneWarp>,
    presentation: ResMut<'w, TerminalPresentation>,
    clipboard: NonSendMut<'w, TerminalClipboard>,
    runtime: NonSendMut<'w, TerminalRuntime>,
    terminal: NonSendMut<'w, TerminalSurface>,
    viewport: Res<'w, TerminalViewport>,
    bindings: Res<'w, TerminalKeyBindings>,
    redraw: ResMut<'w, TerminalRedrawState>,
    _marker: std::marker::PhantomData<&'s ()>,
}

pub fn handle_keyboard_input(
    mut keyboard_events: MessageReader<KeyboardInput>,
    mut keyboard: Local<TerminalKeyboard>,
    mut params: KeyboardSystemParams,
) {
    for event in keyboard_events.read() {
        if event.state == ButtonState::Pressed
            && let Some(action) = params
                .bindings
                .action_for(event.key_code, keyboard.modifiers())
        {
            if event.repeat
                && !matches!(
                    action,
                    BindingAction::IncreaseFontSize
                        | BindingAction::DecreaseFontSize
                        | BindingAction::IncreaseWarp
                        | BindingAction::DecreaseWarp
                )
            {
                continue;
            }

            match action {
                BindingAction::None => {}
                BindingAction::ToggleMode => {
                    params.presentation.toggle();
                    params.selection.clear();
                    params.redraw.request();
                    continue;
                }
                BindingAction::IncreaseWarp | BindingAction::DecreaseWarp => {
                    let delta = if action == BindingAction::IncreaseWarp {
                        0.08
                    } else {
                        -0.08
                    };
                    params.plane_warp.adjust(delta);
                    params.redraw.request();
                    continue;
                }
                BindingAction::Copy => {
                    if let Some(text) = params.selection.selected_text(params.runtime.parser.screen())
                        && !text.is_empty()
                    {
                        params.clipboard.copy(&text);
                    }
                    if params.selection.clear() {
                        params.redraw.request();
                    }
                    continue;
                }
                BindingAction::Paste => {
                    if let Some(text) = params.clipboard.paste() {
                        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
                        let mut bytes = Vec::from(b"\x1b[200~".as_slice());
                        bytes.extend_from_slice(normalized.as_bytes());
                        bytes.extend_from_slice(b"\x1b[201~");
                        params.runtime.write_input(&bytes);
                    } else {
                        warn!("failed to read clipboard contents for paste");
                    }
                    if params.selection.clear() {
                        params.redraw.request();
                    }
                    continue;
                }
                BindingAction::IncreaseFontSize | BindingAction::DecreaseFontSize => {
                    let delta = if action == BindingAction::IncreaseFontSize {
                        1
                    } else {
                        -1
                    };
                    if params.terminal.adjust_font_size(delta) {
                        let char_dims = params.terminal.char_dimensions().max(UVec2::ONE);
                        let cols =
                            ((params.viewport.size.x / char_dims.x as f32).floor() as u16).max(1);
                        let rows =
                            ((params.viewport.size.y / char_dims.y as f32).floor() as u16).max(1);
                        params.runtime.resize(cols, rows);
                        params.terminal.resize(cols, rows);
                        params.redraw.request();
                    }
                    continue;
                }
            }
        }

        if event.state == ButtonState::Pressed
            && !is_modifier_key(event.key_code)
            && params.selection.clear()
        {
            params.redraw.request();
        }

        if let Some(input) =
            keyboard.handle_event_with_modes(
                event,
                params.runtime.parser.screen().application_cursor(),
            )
        {
            params.runtime.write_input(&input);
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct BindingModifiers {
    control: bool,
    alt: bool,
    shift: bool,
    super_key: bool,
}

impl BindingModifiers {
    fn matches(self, current: Self) -> bool {
        (!self.control || current.control)
            && (!self.alt || current.alt)
            && (!self.shift || current.shift)
            && (!self.super_key || current.super_key)
    }

    fn count(self) -> usize {
        self.control as usize + self.alt as usize + self.shift as usize + self.super_key as usize
    }
}

#[derive(Clone, Copy)]
struct KeyBinding {
    key_code: KeyCode,
    modifiers: BindingModifiers,
    action: BindingAction,
}

impl KeyBinding {
    fn new(key_code: KeyCode, modifiers: BindingModifiers, action: BindingAction) -> Self {
        Self {
            key_code,
            modifiers,
            action,
        }
    }

    fn from_config(config: &KeyBindingConfig) -> Option<Self> {
        let mut modifiers = BindingModifiers::default();
        let mut key_code = None;

        for token in config
            .key
            .split('|')
            .chain(config.with.split('|'))
            .map(str::trim)
            .filter(|part| !part.is_empty())
        {
            if let Some(modifier) = parse_modifier(token) {
                modifier.apply(&mut modifiers);
                continue;
            }

            if key_code.is_some() {
                return None;
            }

            key_code = parse_key_code(token);
        }

        Some(Self::new(key_code?, modifiers, config.action))
    }

    fn same_trigger(&self, other: &Self) -> bool {
        self.key_code == other.key_code && self.modifiers == other.modifiers
    }
}

#[derive(Clone, Copy)]
enum ParsedModifier {
    Control,
    Alt,
    Shift,
    Super,
}

impl ParsedModifier {
    fn apply(self, modifiers: &mut BindingModifiers) {
        match self {
            Self::Control => modifiers.control = true,
            Self::Alt => modifiers.alt = true,
            Self::Shift => modifiers.shift = true,
            Self::Super => modifiers.super_key = true,
        }
    }
}

fn translate_key(
    key_code: KeyCode,
    logical_key: &Key,
    text: Option<&str>,
    ctrl_pressed: bool,
    alt_pressed: bool,
    application_cursor: bool,
) -> Vec<u8> {
    let mut bytes = Vec::new();

    if ctrl_pressed && let Some(ctrl) = ctrl_keycode_byte(key_code) {
        if alt_pressed {
            bytes.push(0x1b);
        }
        bytes.push(ctrl);
        return bytes;
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
            } else if application_cursor {
                bytes.extend_from_slice(b"\x1bOA");
            } else {
                bytes.extend_from_slice(b"\x1b[A");
            }
        }
        KeyCode::ArrowDown => {
            if ctrl_pressed {
                bytes.extend_from_slice(b"\x1b[1;5B");
            } else if application_cursor {
                bytes.extend_from_slice(b"\x1bOB");
            } else {
                bytes.extend_from_slice(b"\x1b[B");
            }
        }
        KeyCode::ArrowRight => {
            if ctrl_pressed {
                bytes.extend_from_slice(b"\x1b[1;5C");
            } else if application_cursor {
                bytes.extend_from_slice(b"\x1bOC");
            } else {
                bytes.extend_from_slice(b"\x1b[C");
            }
        }
        KeyCode::ArrowLeft => {
            if ctrl_pressed {
                bytes.extend_from_slice(b"\x1b[1;5D");
            } else if application_cursor {
                bytes.extend_from_slice(b"\x1bOD");
            } else {
                bytes.extend_from_slice(b"\x1b[D");
            }
        }
        KeyCode::Delete => bytes.extend_from_slice(b"\x1b[3~"),
        KeyCode::Home => {
            if application_cursor {
                bytes.extend_from_slice(b"\x1bOH");
            } else {
                bytes.extend_from_slice(b"\x1b[H");
            }
        }
        KeyCode::End => {
            if application_cursor {
                bytes.extend_from_slice(b"\x1bOF");
            } else {
                bytes.extend_from_slice(b"\x1b[F");
            }
        }
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

fn parse_key_code(key: &str) -> Option<KeyCode> {
    match key.trim().to_ascii_lowercase().as_str() {
        "a" => Some(KeyCode::KeyA),
        "b" => Some(KeyCode::KeyB),
        "c" => Some(KeyCode::KeyC),
        "d" => Some(KeyCode::KeyD),
        "e" => Some(KeyCode::KeyE),
        "f" => Some(KeyCode::KeyF),
        "g" => Some(KeyCode::KeyG),
        "h" => Some(KeyCode::KeyH),
        "i" => Some(KeyCode::KeyI),
        "j" => Some(KeyCode::KeyJ),
        "k" => Some(KeyCode::KeyK),
        "l" => Some(KeyCode::KeyL),
        "m" => Some(KeyCode::KeyM),
        "n" => Some(KeyCode::KeyN),
        "o" => Some(KeyCode::KeyO),
        "p" => Some(KeyCode::KeyP),
        "q" => Some(KeyCode::KeyQ),
        "r" => Some(KeyCode::KeyR),
        "s" => Some(KeyCode::KeyS),
        "t" => Some(KeyCode::KeyT),
        "u" => Some(KeyCode::KeyU),
        "v" => Some(KeyCode::KeyV),
        "w" => Some(KeyCode::KeyW),
        "x" => Some(KeyCode::KeyX),
        "y" => Some(KeyCode::KeyY),
        "z" => Some(KeyCode::KeyZ),
        "0" => Some(KeyCode::Digit0),
        "1" => Some(KeyCode::Digit1),
        "2" => Some(KeyCode::Digit2),
        "3" => Some(KeyCode::Digit3),
        "4" => Some(KeyCode::Digit4),
        "5" => Some(KeyCode::Digit5),
        "6" => Some(KeyCode::Digit6),
        "7" => Some(KeyCode::Digit7),
        "8" => Some(KeyCode::Digit8),
        "9" => Some(KeyCode::Digit9),
        "f1" => Some(KeyCode::F1),
        "f2" => Some(KeyCode::F2),
        "f3" => Some(KeyCode::F3),
        "f4" => Some(KeyCode::F4),
        "f5" => Some(KeyCode::F5),
        "f6" => Some(KeyCode::F6),
        "f7" => Some(KeyCode::F7),
        "f8" => Some(KeyCode::F8),
        "f9" => Some(KeyCode::F9),
        "f10" => Some(KeyCode::F10),
        "f11" => Some(KeyCode::F11),
        "f12" => Some(KeyCode::F12),
        "up" => Some(KeyCode::ArrowUp),
        "down" => Some(KeyCode::ArrowDown),
        "left" => Some(KeyCode::ArrowLeft),
        "right" => Some(KeyCode::ArrowRight),
        "enter" => Some(KeyCode::Enter),
        "tab" => Some(KeyCode::Tab),
        "space" => Some(KeyCode::Space),
        "backspace" => Some(KeyCode::Backspace),
        "escape" | "esc" => Some(KeyCode::Escape),
        "delete" => Some(KeyCode::Delete),
        "home" => Some(KeyCode::Home),
        "end" => Some(KeyCode::End),
        "pageup" | "page_up" => Some(KeyCode::PageUp),
        "pagedown" | "page_down" => Some(KeyCode::PageDown),
        "equal" | "=" | "plus" | "+" => Some(KeyCode::Equal),
        "minus" | "-" => Some(KeyCode::Minus),
        "numpadadd" | "numpad_add" => Some(KeyCode::NumpadAdd),
        "numpadsubtract" | "numpad_subtract" => Some(KeyCode::NumpadSubtract),
        _ => None,
    }
}

fn parse_modifier(token: &str) -> Option<ParsedModifier> {
    match token.trim().to_ascii_lowercase().as_str() {
        "control" | "ctrl" => Some(ParsedModifier::Control),
        "alt" => Some(ParsedModifier::Alt),
        "shift" => Some(ParsedModifier::Shift),
        "super" | "cmd" | "command" | "meta" => Some(ParsedModifier::Super),
        _ => None,
    }
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
