//! Keyboard input handling.

use bevy::ecs::system::SystemParam;
use bevy::ecs::world::FromWorld;
use bevy::input::ButtonState;
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::prelude::*;
use bevy::window::{PrimaryWindow, Window};

use arboard::Clipboard;

use crate::camera::{
    ActivateTerminalCameraPreset, TerminalCameraInteraction, TerminalCameraSlots,
    TerminalMobiusSource,
};
use crate::config::{AppConfig, BindingAction, FontConfig, KeyBindingConfig};
use crate::mouse::{TerminalSelection, encode_mouse_wheel};
use crate::runtime::TerminalRuntime;
use crate::scene::{
    MobiusEnterZoomFloor, MobiusTransition, TerminalPlaneBackLayoutQuery, TerminalPlaneLayoutQuery,
    TerminalPlaneWarp, TerminalPresentationMode, TerminalViewport, sync_terminal_layout,
};
use crate::terminal::{TerminalRedrawState, TerminalSurface, render_scale_for_window};
use crate::vt::{self, MouseProtocolMode};

/// Clipboard bridge for terminal copy and paste.
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

/// Resolved terminal key bindings.
#[derive(Resource)]
pub struct TerminalKeyBindings {
    bindings: Vec<KeyBinding>,
}

impl FromWorld for TerminalKeyBindings {
    fn from_world(world: &mut World) -> Self {
        let app_config = world.resource::<AppConfig>();
        let mut bindings = default_bindings();

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

fn default_bindings() -> Vec<KeyBinding> {
    let mut bindings = vec![
        KeyBinding::new(
            KeyCode::Enter,
            BindingModifiers {
                control: true,
                alt: true,
                ..default()
            },
            BindingAction::ToggleOrtho3DMode,
        ),
        KeyBinding::new(
            KeyCode::KeyP,
            BindingModifiers {
                control: true,
                alt: true,
                ..default()
            },
            BindingAction::TogglePersp3DMode,
        ),
        KeyBinding::new(
            KeyCode::KeyM,
            BindingModifiers {
                control: true,
                alt: true,
                ..default()
            },
            BindingAction::ToggleMobiusMode,
        ),
        KeyBinding::new(
            KeyCode::PageUp,
            BindingModifiers {
                alt: true,
                ..default()
            },
            BindingAction::ScrollPageUp,
        ),
        KeyBinding::new(
            KeyCode::PageDown,
            BindingModifiers {
                alt: true,
                ..default()
            },
            BindingAction::ScrollPageDown,
        ),
        KeyBinding::new(
            KeyCode::ArrowUp,
            BindingModifiers {
                alt: true,
                ..default()
            },
            BindingAction::ScrollUp,
        ),
        KeyBinding::new(
            KeyCode::ArrowDown,
            BindingModifiers {
                alt: true,
                ..default()
            },
            BindingAction::ScrollDown,
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
        KeyBinding::new(
            KeyCode::Digit0,
            BindingModifiers {
                control: true,
                alt: true,
                ..default()
            },
            BindingAction::ResetFontSize,
        ),
        KeyBinding::new(
            KeyCode::Numpad0,
            BindingModifiers {
                control: true,
                alt: true,
                ..default()
            },
            BindingAction::ResetFontSize,
        ),
    ];
    let slot_keys = [
        KeyCode::Digit0,
        KeyCode::Digit1,
        KeyCode::Digit2,
        KeyCode::Digit3,
        KeyCode::Digit4,
        KeyCode::Digit5,
        KeyCode::Digit6,
        KeyCode::Digit7,
        KeyCode::Digit8,
        KeyCode::Digit9,
    ];
    let slot_actions = [
        BindingAction::ActivateCameraSlot0,
        BindingAction::ActivateCameraSlot1,
        BindingAction::ActivateCameraSlot2,
        BindingAction::ActivateCameraSlot3,
        BindingAction::ActivateCameraSlot4,
        BindingAction::ActivateCameraSlot5,
        BindingAction::ActivateCameraSlot6,
        BindingAction::ActivateCameraSlot7,
        BindingAction::ActivateCameraSlot8,
        BindingAction::ActivateCameraSlot9,
    ];
    // No numpad variants: with Num Lock on Windows/X11, Shift+numpad delivers
    // navigation logical keys, which would misroute these chords to the
    // warp/scroll bindings instead of slot activation.
    bindings.extend(
        slot_keys
            .into_iter()
            .zip(slot_actions)
            .map(|(key, action)| {
                KeyBinding::new(
                    key,
                    BindingModifiers {
                        control: true,
                        alt: true,
                        shift: true,
                        ..default()
                    },
                    action,
                )
            }),
    );
    bindings
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

/// Keyboard modifier state.
#[derive(Default)]
pub struct TerminalKeyboard {
    pub(crate) ctrl_pressed: bool,
    pub(crate) left_alt_pressed: bool,
    pub(crate) right_alt_pressed: bool,
    pub(crate) shift_pressed: bool,
    pub(crate) super_pressed: bool,
}

impl TerminalKeyboard {
    fn modifiers(&self) -> BindingModifiers {
        BindingModifiers {
            control: self.ctrl_pressed,
            alt: self.left_alt_pressed,
            shift: self.shift_pressed,
            super_key: self.super_pressed,
        }
    }

    /// Translates a keyboard event into terminal input bytes.
    pub fn handle_event_with_modes(
        &mut self,
        event: &KeyboardInput,
        application_cursor: bool,
        kitty_keyboard_flags: u8,
        modify_other_keys: Option<u8>,
    ) -> Option<Vec<u8>> {
        match event.key_code {
            KeyCode::ControlLeft | KeyCode::ControlRight => {
                self.ctrl_pressed = event.state == ButtonState::Pressed;
                return None;
            }
            KeyCode::AltLeft => {
                self.left_alt_pressed = event.state == ButtonState::Pressed;
                return None;
            }
            KeyCode::AltRight => {
                self.right_alt_pressed = event.state == ButtonState::Pressed;
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
            KeyTranslationContext {
                logical_key: &event.logical_key,
                text: event.text.as_deref(),
                ctrl_pressed: self.ctrl_pressed,
                alt_pressed: self.left_alt_pressed,
                alt_gr_pressed: self.right_alt_pressed,
                shift_pressed: self.shift_pressed,
                application_cursor,
                kitty_keyboard_flags,
                modify_other_keys,
            },
        ))
    }
}

/// Keyboard input system parameters.
#[derive(SystemParam)]
pub struct KeyboardSystemParams<'w, 's> {
    keys: Res<'w, ButtonInput<KeyCode>>,
    selection: ResMut<'w, TerminalSelection>,
    plane_warp: ResMut<'w, TerminalPlaneWarp>,
    camera_slots: ResMut<'w, TerminalCameraSlots>,
    camera_interaction: ResMut<'w, TerminalCameraInteraction>,
    camera_activations: MessageWriter<'w, ActivateTerminalCameraPreset>,
    mobius_transition: ResMut<'w, MobiusTransition>,
    clipboard: NonSendMut<'w, TerminalClipboard>,
    runtime: ResMut<'w, TerminalRuntime>,
    terminal: ResMut<'w, TerminalSurface>,
    primary_window: Query<'w, 's, &'static Window, With<PrimaryWindow>>,
    viewport: ResMut<'w, TerminalViewport>,
    plane_query: TerminalPlaneLayoutQuery<'w, 's>,
    plane_back_query: TerminalPlaneBackLayoutQuery<'w, 's>,
    bindings: Res<'w, TerminalKeyBindings>,
    redraw: ResMut<'w, TerminalRedrawState>,
    _marker: std::marker::PhantomData<&'s ()>,
}

/// Handles terminal keyboard input.
pub fn handle_keyboard_input(
    mut keyboard_events: MessageReader<KeyboardInput>,
    mut keyboard: Local<TerminalKeyboard>,
    mut params: KeyboardSystemParams,
) {
    for event in keyboard_events.read() {
        let binding_key_code = navigation_key_code(&event.logical_key).unwrap_or(event.key_code);
        let modifiers = current_modifiers(&params.keys).union(keyboard.modifiers());
        if event.state == ButtonState::Pressed
            && let Some(action) = params.bindings.action_for(binding_key_code, modifiers)
            && !(is_scroll_action(action) && vt::alternate_screen(&params.runtime.term))
        {
            if event.repeat
                && !matches!(
                    action,
                    BindingAction::IncreaseFontSize
                        | BindingAction::DecreaseFontSize
                        | BindingAction::ResetFontSize
                        | BindingAction::ScrollPageUp
                        | BindingAction::ScrollPageDown
                        | BindingAction::ScrollUp
                        | BindingAction::ScrollDown
                        | BindingAction::IncreaseWarp
                        | BindingAction::DecreaseWarp
                )
            {
                continue;
            }

            if let Some(slot) = action.camera_slot() {
                params
                    .camera_activations
                    .write(ActivateTerminalCameraPreset { slot });
                params.selection.clear();
                continue;
            }

            match action {
                BindingAction::None => {}
                BindingAction::ToggleOrtho3DMode => {
                    let preset = params.camera_slots.active_mut();
                    preset.mode = if preset.mode == TerminalPresentationMode::Plane3d {
                        TerminalPresentationMode::Flat2d
                    } else {
                        TerminalPresentationMode::Plane3d
                    };
                    preset.mobius_source = None;
                    params.camera_interaction.reset();
                    params.mobius_transition.stop();
                    params.selection.clear();
                    continue;
                }
                BindingAction::TogglePersp3DMode => {
                    let preset = params.camera_slots.active_mut();
                    preset.mode = if preset.mode == TerminalPresentationMode::Perspective3d {
                        TerminalPresentationMode::Flat2d
                    } else {
                        TerminalPresentationMode::Perspective3d
                    };
                    preset.mobius_source = None;
                    params.camera_interaction.reset();
                    params.mobius_transition.stop();
                    params.selection.clear();
                    continue;
                }
                BindingAction::ToggleMobiusMode => {
                    toggle_mobius_presentation(
                        &mut params.camera_slots,
                        &mut params.camera_interaction,
                        &mut params.mobius_transition,
                    );
                    params.selection.clear();
                    continue;
                }
                BindingAction::ScrollPageUp
                | BindingAction::ScrollPageDown
                | BindingAction::ScrollUp
                | BindingAction::ScrollDown => {
                    let amount = match action {
                        BindingAction::ScrollPageUp | BindingAction::ScrollPageDown => {
                            usize::from(params.terminal.rows.saturating_sub(1).max(1))
                        }
                        BindingAction::ScrollUp | BindingAction::ScrollDown => 1,
                        _ => unreachable!(),
                    };
                    let direction = match action {
                        BindingAction::ScrollPageUp | BindingAction::ScrollUp => 1isize,
                        BindingAction::ScrollPageDown | BindingAction::ScrollDown => -1isize,
                        _ => unreachable!(),
                    };

                    let mouse_mode = vt::mouse_protocol_mode(&params.runtime.term);
                    if params.camera_slots.active().mode == TerminalPresentationMode::Flat2d
                        && mouse_mode != MouseProtocolMode::None
                    {
                        let encoding = vt::mouse_protocol_encoding(&params.runtime.term);
                        let (row, col) = vt::cursor_position(&params.runtime.term);
                        let cell = UVec2::new(col as u32, row as u32);
                        for _ in 0..amount {
                            params.runtime.write_input(&encode_mouse_wheel(
                                cell,
                                direction.is_positive(),
                                encoding,
                            ));
                        }
                    } else {
                        let current = vt::scrollback(&params.runtime.term);
                        let next = if direction.is_positive() {
                            current.saturating_add(amount)
                        } else {
                            current.saturating_sub(amount)
                        };
                        vt::set_scrollback(&mut params.runtime.term, next);
                        params.selection.clear();
                        params.redraw.request();
                    }
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
                    if let Some(text) = params.selection.selected_text(&params.runtime.term)
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
                        let bracketed = vt::bracketed_paste(&params.runtime.term);
                        params.runtime.write_input(&encode_paste(&text, bracketed));
                    } else {
                        warn!("failed to read clipboard contents for paste");
                    }
                    if params.selection.clear() {
                        params.redraw.request();
                    }
                    continue;
                }
                BindingAction::IncreaseFontSize
                | BindingAction::DecreaseFontSize
                | BindingAction::ResetFontSize => {
                    let resized = match action {
                        BindingAction::IncreaseFontSize => params.terminal.adjust_font_size(1),
                        BindingAction::DecreaseFontSize => params.terminal.adjust_font_size(-1),
                        BindingAction::ResetFontSize => {
                            let target = FontConfig::default().size;
                            let delta = target - params.terminal.font_size();
                            delta != 0 && params.terminal.adjust_font_size(delta)
                        }
                        _ => false,
                    };
                    if resized {
                        let Ok(window) = params.primary_window.single() else {
                            continue;
                        };
                        let layout = params.terminal.resize_to_fit(
                            window.resolution.size().max(Vec2::ONE),
                            render_scale_for_window(window),
                        );
                        let pty_pixels = layout.pty_pixels();
                        params.runtime.resize(
                            layout.cols,
                            layout.rows,
                            pty_pixels.x as u16,
                            pty_pixels.y as u16,
                        );
                        sync_terminal_layout(
                            layout,
                            &mut params.viewport,
                            &mut params.plane_query,
                            &mut params.plane_back_query,
                        );
                        params.redraw.request();
                    }
                    continue;
                }
                BindingAction::ActivateCameraSlot0
                | BindingAction::ActivateCameraSlot1
                | BindingAction::ActivateCameraSlot2
                | BindingAction::ActivateCameraSlot3
                | BindingAction::ActivateCameraSlot4
                | BindingAction::ActivateCameraSlot5
                | BindingAction::ActivateCameraSlot6
                | BindingAction::ActivateCameraSlot7
                | BindingAction::ActivateCameraSlot8
                | BindingAction::ActivateCameraSlot9 => unreachable!("handled above"),
            }
        }

        if event.state == ButtonState::Pressed
            && !is_modifier_key(binding_key_code)
            && params.selection.clear()
        {
            params.redraw.request();
        }

        if let Some(input) = keyboard.handle_event_with_modes(
            event,
            vt::application_cursor(&params.runtime.term),
            params.runtime.kitty_keyboard_flags(),
            params.runtime.modify_other_keys(),
        ) {
            if vt::scrollback(&params.runtime.term) != 0 {
                vt::set_scrollback(&mut params.runtime.term, 0);
                params.redraw.request();
            }
            params.runtime.write_input(&input);
        }
    }
}

fn toggle_mobius_presentation(
    camera_slots: &mut TerminalCameraSlots,
    interaction: &mut TerminalCameraInteraction,
    mobius_transition: &mut MobiusTransition,
) {
    let slot = camera_slots.active_slot();
    let preset = *camera_slots.active();
    if preset.mode == TerminalPresentationMode::Mobius3d {
        let source = preset.mobius_source.unwrap_or(TerminalMobiusSource {
            mode: TerminalPresentationMode::Plane3d,
            pose: preset.pose,
        });
        if mobius_transition.active
            && mobius_transition.direction == crate::scene::MobiusTransitionDirection::Exiting
        {
            // Toggling during an exit turns the animation back around; the
            // preset is still Mobius, so restarting the exit here would lock
            // out re-entry until the exit finished.
            mobius_transition.begin_enter(
                slot,
                &source,
                &preset.pose,
                MobiusEnterZoomFloor::KeyboardTarget,
            );
        } else {
            mobius_transition.prepare_source(slot, source.mode, &source.pose);
            let current_zoom = if mobius_transition.active {
                mobius_transition.current_zoom()
            } else {
                preset.pose.orthographic_scale
            };
            mobius_transition.begin_exit(slot, &preset.pose, current_zoom);
        }
    } else {
        let source = TerminalMobiusSource {
            mode: preset.mode,
            pose: preset.pose,
        };
        mobius_transition.begin_enter(
            slot,
            &source,
            &preset.pose,
            MobiusEnterZoomFloor::KeyboardTarget,
        );
        let preset = camera_slots.active_mut();
        preset.mobius_source = Some(source);
        preset.mode = TerminalPresentationMode::Mobius3d;
    }
    interaction.reset();
}

/// Enters Mobius mode without toggling back out when it is already active.
pub(crate) fn enter_mobius_presentation(
    camera_slots: &mut TerminalCameraSlots,
    interaction: &mut TerminalCameraInteraction,
    mobius_transition: &mut MobiusTransition,
) {
    let already_entering_or_active = camera_slots.active().mode
        == TerminalPresentationMode::Mobius3d
        && !(mobius_transition.active
            && mobius_transition.direction == crate::scene::MobiusTransitionDirection::Exiting);
    if !already_entering_or_active {
        toggle_mobius_presentation(camera_slots, interaction, mobius_transition);
    }
}

fn is_scroll_action(action: BindingAction) -> bool {
    matches!(
        action,
        BindingAction::ScrollPageUp
            | BindingAction::ScrollPageDown
            | BindingAction::ScrollUp
            | BindingAction::ScrollDown
    )
}

fn current_modifiers(keys: &ButtonInput<KeyCode>) -> BindingModifiers {
    BindingModifiers {
        control: keys.any_pressed([KeyCode::ControlLeft, KeyCode::ControlRight]),
        alt: keys.pressed(KeyCode::AltLeft),
        shift: keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]),
        super_key: keys.any_pressed([KeyCode::SuperLeft, KeyCode::SuperRight]),
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

    fn union(self, other: Self) -> Self {
        Self {
            control: self.control || other.control,
            alt: self.alt || other.alt,
            shift: self.shift || other.shift,
            super_key: self.super_key || other.super_key,
        }
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

struct KeyTranslationContext<'a> {
    logical_key: &'a Key,
    text: Option<&'a str>,
    ctrl_pressed: bool,
    alt_pressed: bool,
    alt_gr_pressed: bool,
    shift_pressed: bool,
    application_cursor: bool,
    kitty_keyboard_flags: u8,
    modify_other_keys: Option<u8>,
}

fn translate_key(key_code: KeyCode, ctx: KeyTranslationContext<'_>) -> Vec<u8> {
    let mut bytes = Vec::new();

    if ctx.alt_gr_pressed
        && let Some(text) = printable_text(ctx.text, ctx.logical_key)
    {
        bytes.extend_from_slice(text.as_bytes());
        return bytes;
    }

    if ctx.ctrl_pressed
        && let Some(ctrl) = ctrl_keycode_byte(key_code)
    {
        if ctx.alt_pressed {
            bytes.push(0x1b);
        }
        bytes.push(ctrl);
        return bytes;
    }

    // Kitty flag bit 0 requests disambiguated escape codes, which gives us an unambiguous
    // encoding for modified special keys such as Ctrl+Enter.
    let kitty_disambiguate = ctx.kitty_keyboard_flags & 1 != 0;
    if let Some(sequence) = encode_modified_special_key(
        key_code,
        ctx.ctrl_pressed,
        ctx.alt_pressed,
        ctx.shift_pressed,
        kitty_disambiguate,
        ctx.modify_other_keys,
    ) {
        bytes.extend_from_slice(&sequence);
        return bytes;
    }

    let navigation_key = NavigationKey::from_key_code(key_code)
        .or_else(|| NavigationKey::from_logical_key(ctx.logical_key));
    if let Some(key) = navigation_key {
        bytes.extend_from_slice(&key.encode(
            ctx.ctrl_pressed,
            ctx.alt_pressed,
            ctx.shift_pressed,
            ctx.application_cursor,
        ));
        return bytes;
    }

    if ctx.alt_pressed {
        bytes.push(0x1b);
    }

    match key_code {
        KeyCode::Enter | KeyCode::NumpadEnter => bytes.push(b'\r'),
        KeyCode::Tab => bytes.push(b'\t'),
        KeyCode::Space => bytes.push(b' '),
        KeyCode::Backspace => bytes.push(0x7f),
        KeyCode::Escape => bytes.push(0x1b),
        _ => {
            if let Some(text) = printable_text(ctx.text, ctx.logical_key) {
                bytes.extend_from_slice(text.as_bytes());
            }
        }
    }

    bytes
}

/// Bracketed paste start marker (DECSET 2004).
const PASTE_START: &[u8] = b"\x1b[200~";
/// Bracketed paste end marker.
const PASTE_END: &[u8] = b"\x1b[201~";

/// Encodes clipboard text as terminal input, wrapping it in bracketed paste
/// markers when DECSET 2004 is active. The 7-bit `ESC` and 8-bit `CSI` control
/// introducers are dropped from bracketed payloads so a paste can never
/// terminate its own bracket (paste injection).
fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        let sanitized: String = normalized
            .chars()
            .filter(|&ch| ch != '\u{1b}' && ch != '\u{9b}')
            .collect();
        let mut bytes = Vec::with_capacity(sanitized.len() + PASTE_START.len() + PASTE_END.len());
        bytes.extend_from_slice(PASTE_START);
        bytes.extend_from_slice(sanitized.as_bytes());
        bytes.extend_from_slice(PASTE_END);
        bytes
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

/// Determine the text to send for a key event.
fn printable_text<'a>(text: Option<&'a str>, logical_key: &'a Key) -> Option<&'a str> {
    text.or_else(|| match logical_key {
        Key::Character(chars) => Some(chars.as_str()),
        _ => None,
    })
    .filter(|text| !text.is_empty())
}

#[derive(Clone, Copy)]
enum NavigationKey {
    ArrowUp,
    ArrowDown,
    ArrowRight,
    ArrowLeft,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
}

impl NavigationKey {
    fn from_key_code(key_code: KeyCode) -> Option<Self> {
        match key_code {
            KeyCode::ArrowUp => Some(Self::ArrowUp),
            KeyCode::ArrowDown => Some(Self::ArrowDown),
            KeyCode::ArrowRight => Some(Self::ArrowRight),
            KeyCode::ArrowLeft => Some(Self::ArrowLeft),
            KeyCode::Home => Some(Self::Home),
            KeyCode::End => Some(Self::End),
            KeyCode::PageUp => Some(Self::PageUp),
            KeyCode::PageDown => Some(Self::PageDown),
            KeyCode::Insert => Some(Self::Insert),
            KeyCode::Delete => Some(Self::Delete),
            _ => None,
        }
    }

    fn from_logical_key(logical_key: &Key) -> Option<Self> {
        // Keypad navigation with numlock disabled often arrives as a Numpad physical key paired
        // with a logical navigation key such as Home or PageUp. Use the logical meaning so keypad
        // navigation behaves like the dedicated navigation cluster.
        match logical_key {
            Key::ArrowUp => Some(Self::ArrowUp),
            Key::ArrowDown => Some(Self::ArrowDown),
            Key::ArrowRight => Some(Self::ArrowRight),
            Key::ArrowLeft => Some(Self::ArrowLeft),
            Key::Home => Some(Self::Home),
            Key::End => Some(Self::End),
            Key::PageUp => Some(Self::PageUp),
            Key::PageDown => Some(Self::PageDown),
            Key::Insert => Some(Self::Insert),
            Key::Delete => Some(Self::Delete),
            _ => None,
        }
    }

    fn encode(
        self,
        ctrl_pressed: bool,
        alt_pressed: bool,
        shift_pressed: bool,
        application_cursor: bool,
    ) -> Vec<u8> {
        let modifier_code =
            1 + shift_pressed as u8 + (alt_pressed as u8 * 2) + (ctrl_pressed as u8 * 4);

        if modifier_code != 1 {
            let arrow_suffix = match self {
                Self::ArrowUp => Some('A'),
                Self::ArrowDown => Some('B'),
                Self::ArrowRight => Some('C'),
                Self::ArrowLeft => Some('D'),
                Self::Home => Some('H'),
                Self::End => Some('F'),
                _ => None,
            };

            if let Some(suffix) = arrow_suffix {
                return format!("\x1b[1;{modifier_code}{suffix}").into_bytes();
            }

            let tilde_code = match self {
                Self::PageUp => Some(5),
                Self::PageDown => Some(6),
                Self::Insert => Some(2),
                Self::Delete => Some(3),
                _ => None,
            };

            if let Some(code) = tilde_code {
                return format!("\x1b[{code};{modifier_code}~").into_bytes();
            }
        }

        match self {
            Self::ArrowUp => {
                if ctrl_pressed {
                    b"\x1b[1;5A".to_vec()
                } else if application_cursor {
                    b"\x1bOA".to_vec()
                } else {
                    b"\x1b[A".to_vec()
                }
            }
            Self::ArrowDown => {
                if ctrl_pressed {
                    b"\x1b[1;5B".to_vec()
                } else if application_cursor {
                    b"\x1bOB".to_vec()
                } else {
                    b"\x1b[B".to_vec()
                }
            }
            Self::ArrowRight => {
                if ctrl_pressed {
                    b"\x1b[1;5C".to_vec()
                } else if application_cursor {
                    b"\x1bOC".to_vec()
                } else {
                    b"\x1b[C".to_vec()
                }
            }
            Self::ArrowLeft => {
                if ctrl_pressed {
                    b"\x1b[1;5D".to_vec()
                } else if application_cursor {
                    b"\x1bOD".to_vec()
                } else {
                    b"\x1b[D".to_vec()
                }
            }
            Self::Home => {
                if application_cursor {
                    b"\x1bOH".to_vec()
                } else {
                    b"\x1b[1~".to_vec()
                }
            }
            Self::End => {
                if application_cursor {
                    b"\x1bOF".to_vec()
                } else {
                    b"\x1b[4~".to_vec()
                }
            }
            Self::PageUp => b"\x1b[5~".to_vec(),
            Self::PageDown => b"\x1b[6~".to_vec(),
            Self::Insert => b"\x1b[2~".to_vec(),
            Self::Delete => b"\x1b[3~".to_vec(),
        }
    }
}

fn navigation_key_code(logical_key: &Key) -> Option<KeyCode> {
    match logical_key {
        Key::ArrowUp => Some(KeyCode::ArrowUp),
        Key::ArrowDown => Some(KeyCode::ArrowDown),
        Key::ArrowRight => Some(KeyCode::ArrowRight),
        Key::ArrowLeft => Some(KeyCode::ArrowLeft),
        Key::Home => Some(KeyCode::Home),
        Key::End => Some(KeyCode::End),
        Key::PageUp => Some(KeyCode::PageUp),
        Key::PageDown => Some(KeyCode::PageDown),
        Key::Insert => Some(KeyCode::Insert),
        Key::Delete => Some(KeyCode::Delete),
        _ => None,
    }
}

fn encode_modified_special_key(
    key_code: KeyCode,
    ctrl_pressed: bool,
    alt_pressed: bool,
    shift_pressed: bool,
    kitty_disambiguate: bool,
    modify_other_keys: Option<u8>,
) -> Option<Vec<u8>> {
    let codepoint = match key_code {
        KeyCode::Enter | KeyCode::NumpadEnter => 13,
        KeyCode::Tab => 9,
        KeyCode::Backspace => 127,
        KeyCode::Escape => 27,
        _ => return None,
    };

    if !ctrl_pressed && !alt_pressed && !shift_pressed {
        return None;
    }

    let modifier_code =
        1 + shift_pressed as u8 + (alt_pressed as u8 * 2) + (ctrl_pressed as u8 * 4);

    // Kitty keyboard protocol uses CSI codepoint ; modifiers u for modified special keys.
    if kitty_disambiguate {
        return Some(format!("\x1b[{};{}u", codepoint, modifier_code).into_bytes());
    }

    // xterm modifyOtherKeys falls back to CSI 27 ; modifiers ; codepoint ~ for the same class of
    // keys when the foreground app explicitly enabled that mode.
    if modify_other_keys.is_some() {
        return Some(format!("\x1b[27;{};{}~", modifier_code, codepoint).into_bytes());
    }

    None
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
        "a" | "keya" => Some(KeyCode::KeyA),
        "b" | "keyb" => Some(KeyCode::KeyB),
        "c" | "keyc" => Some(KeyCode::KeyC),
        "d" | "keyd" => Some(KeyCode::KeyD),
        "e" | "keye" => Some(KeyCode::KeyE),
        "f" | "keyf" => Some(KeyCode::KeyF),
        "g" | "keyg" => Some(KeyCode::KeyG),
        "h" | "keyh" => Some(KeyCode::KeyH),
        "i" | "keyi" => Some(KeyCode::KeyI),
        "j" | "keyj" => Some(KeyCode::KeyJ),
        "k" | "keyk" => Some(KeyCode::KeyK),
        "l" | "keyl" => Some(KeyCode::KeyL),
        "m" | "keym" => Some(KeyCode::KeyM),
        "n" | "keyn" => Some(KeyCode::KeyN),
        "o" | "keyo" => Some(KeyCode::KeyO),
        "p" | "keyp" => Some(KeyCode::KeyP),
        "q" | "keyq" => Some(KeyCode::KeyQ),
        "r" | "keyr" => Some(KeyCode::KeyR),
        "s" | "keys" => Some(KeyCode::KeyS),
        "t" | "keyt" => Some(KeyCode::KeyT),
        "u" | "keyu" => Some(KeyCode::KeyU),
        "v" | "keyv" => Some(KeyCode::KeyV),
        "w" | "keyw" => Some(KeyCode::KeyW),
        "x" | "keyx" => Some(KeyCode::KeyX),
        "y" | "keyy" => Some(KeyCode::KeyY),
        "z" | "keyz" => Some(KeyCode::KeyZ),
        "0" | "digit0" => Some(KeyCode::Digit0),
        "1" | "digit1" => Some(KeyCode::Digit1),
        "2" | "digit2" => Some(KeyCode::Digit2),
        "3" | "digit3" => Some(KeyCode::Digit3),
        "4" | "digit4" => Some(KeyCode::Digit4),
        "5" | "digit5" => Some(KeyCode::Digit5),
        "6" | "digit6" => Some(KeyCode::Digit6),
        "7" | "digit7" => Some(KeyCode::Digit7),
        "8" | "digit8" => Some(KeyCode::Digit8),
        "9" | "digit9" => Some(KeyCode::Digit9),
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
        "up" | "arrowup" => Some(KeyCode::ArrowUp),
        "down" | "arrowdown" => Some(KeyCode::ArrowDown),
        "left" | "arrowleft" => Some(KeyCode::ArrowLeft),
        "right" | "arrowright" => Some(KeyCode::ArrowRight),
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

#[cfg(test)]
mod key_code_tests {
    use super::*;

    #[test]
    fn parses_physical_key_names_from_default_config() {
        assert_eq!(parse_key_code("Digit0"), Some(KeyCode::Digit0));
        assert_eq!(parse_key_code("KeyP"), Some(KeyCode::KeyP));
        assert_eq!(parse_key_code("ArrowUp"), Some(KeyCode::ArrowUp));
    }

    #[test]
    fn default_camera_slot_bindings_cover_all_slots() {
        let mut slots = default_bindings()
            .into_iter()
            .filter_map(|binding| binding.action.camera_slot())
            .collect::<Vec<_>>();
        slots.sort_unstable();
        assert_eq!(slots, (0..10).collect::<Vec<_>>());
    }

    #[test]
    fn default_bindings_have_no_duplicate_triggers() {
        let bindings = default_bindings();
        for (index, binding) in bindings.iter().enumerate() {
            assert!(
                bindings[index + 1..]
                    .iter()
                    .all(|other| !binding.same_trigger(other)),
                "duplicate default trigger for {:?}",
                binding.action
            );
        }
    }

    #[test]
    fn camera_slot_zero_does_not_replace_font_reset() {
        let bindings = TerminalKeyBindings {
            bindings: default_bindings(),
        };
        let control_alt = BindingModifiers {
            control: true,
            alt: true,
            ..default()
        };
        let control_alt_shift = BindingModifiers {
            shift: true,
            ..control_alt
        };

        assert_eq!(
            bindings.action_for(KeyCode::Digit0, control_alt),
            Some(BindingAction::ResetFontSize)
        );
        assert_eq!(
            bindings.action_for(KeyCode::Digit0, control_alt_shift),
            Some(BindingAction::ActivateCameraSlot0)
        );
    }

    #[test]
    fn distributed_camera_bindings_parse_without_trigger_collisions() {
        let config: AppConfig =
            toml::from_str(include_str!("../config/ratty.toml")).expect("distributed config");
        let bindings = config
            .bindings
            .keys
            .iter()
            .map(|binding| KeyBinding::from_config(binding).expect("valid distributed binding"))
            .collect::<Vec<_>>();

        for (index, binding) in bindings.iter().enumerate() {
            assert!(
                bindings[index + 1..]
                    .iter()
                    .all(|other| !binding.same_trigger(other)),
                "duplicate distributed trigger for {:?}",
                binding.action
            );
        }
        let mut slots = bindings
            .iter()
            .filter_map(|binding| binding.action.camera_slot())
            .collect::<Vec<_>>();
        slots.sort_unstable();
        assert_eq!(slots, (0..10).collect::<Vec<_>>());
    }

    #[test]
    fn mobius_exit_uses_the_active_slots_protocol_pose() {
        use crate::camera::{
            OptionalVec3, TerminalCameraUpdate, activate_terminal_camera_presets,
            apply_terminal_camera_updates,
        };

        let mut app = App::new();
        app.init_resource::<TerminalCameraSlots>()
            .init_resource::<TerminalCameraInteraction>()
            .init_resource::<MobiusTransition>()
            .add_message::<TerminalCameraUpdate>()
            .add_message::<ActivateTerminalCameraPreset>()
            .add_systems(
                Update,
                (
                    apply_terminal_camera_updates,
                    activate_terminal_camera_presets,
                )
                    .chain(),
            );
        app.world_mut().write_message(TerminalCameraUpdate {
            slot: 2,
            activate: true,
            mode: Some(TerminalPresentationMode::Mobius3d),
            scale: Some(2.5),
            fov: None,
            translation: OptionalVec3 {
                x: Some(12.0),
                y: Some(-8.0),
                z: Some(30.0),
            },
            rotation_degrees: OptionalVec3 {
                x: Some(15.0),
                y: Some(35.0),
                z: None,
            },
        });
        app.update();
        app.world_mut()
            .write_message(ActivateTerminalCameraPreset { slot: 1 });
        app.update();
        app.world_mut()
            .write_message(ActivateTerminalCameraPreset { slot: 2 });
        app.update();

        let expected = *app.world().resource::<TerminalCameraSlots>().active();
        let mut slots = app
            .world_mut()
            .remove_resource::<TerminalCameraSlots>()
            .expect("camera slots");
        let mut interaction = app
            .world_mut()
            .remove_resource::<TerminalCameraInteraction>()
            .expect("camera interaction");
        interaction.rotating = true;
        let mut transition = app
            .world_mut()
            .remove_resource::<MobiusTransition>()
            .expect("mobius transition");

        toggle_mobius_presentation(&mut slots, &mut interaction, &mut transition);

        assert!(transition.active);
        assert!(matches!(
            transition.direction,
            crate::scene::MobiusTransitionDirection::Exiting
        ));
        assert!(transition.source_is_for(2));
        assert_eq!(transition.source_mode, TerminalPresentationMode::Flat2d);
        assert_eq!(transition.end_zoom, expected.pose.orthographic_scale);
        assert_eq!(transition.end_translation, expected.pose.translation);
        assert_eq!(transition.end_yaw, expected.pose.yaw);
        assert_eq!(transition.end_pitch, expected.pose.pitch);
        assert!(!interaction.rotating);
    }

    #[test]
    fn mobius_toggle_during_exit_turns_back_into_mobius() {
        use crate::scene::MobiusTransitionDirection;

        let mut slots = TerminalCameraSlots::default();
        let mut interaction = TerminalCameraInteraction::default();
        let mut transition = MobiusTransition::default();

        toggle_mobius_presentation(&mut slots, &mut interaction, &mut transition);
        assert_eq!(slots.active().mode, TerminalPresentationMode::Mobius3d);
        transition.stop();

        toggle_mobius_presentation(&mut slots, &mut interaction, &mut transition);
        assert!(matches!(
            transition.direction,
            MobiusTransitionDirection::Exiting
        ));
        transition.elapsed_secs =
            MobiusTransition::VIEW_RESET_SECS + MobiusTransition::MORPH_SECS * 0.5;
        let morph_before = transition.morph_progress();

        toggle_mobius_presentation(&mut slots, &mut interaction, &mut transition);

        assert!(transition.active);
        assert!(matches!(
            transition.direction,
            MobiusTransitionDirection::Entering
        ));
        assert!((transition.morph_progress() - morph_before).abs() < 1e-6);
        assert_eq!(slots.active().mode, TerminalPresentationMode::Mobius3d);
        assert_eq!(
            slots.active().mobius_source.expect("Mobius source").mode,
            TerminalPresentationMode::Flat2d
        );
        assert_eq!(transition.source_mode, TerminalPresentationMode::Flat2d);
    }
}

#[cfg(test)]
mod keyboard_translation_tests {
    use super::*;

    fn translate_navigation_key(
        key_code: KeyCode,
        logical_key: Key,
        ctrl_pressed: bool,
        alt_pressed: bool,
        shift_pressed: bool,
        application_cursor: bool,
    ) -> Vec<u8> {
        translate_key(
            key_code,
            KeyTranslationContext {
                logical_key: &logical_key,
                text: None,
                ctrl_pressed,
                alt_pressed,
                alt_gr_pressed: false,
                shift_pressed,
                application_cursor,
                kitty_keyboard_flags: 0,
                modify_other_keys: None,
            },
        )
    }

    #[test]
    fn encodes_alt_arrow_keys_as_modified_csi_sequences() {
        assert_eq!(
            translate_navigation_key(KeyCode::ArrowUp, Key::ArrowUp, false, true, false, false),
            b"\x1b[1;3A"
        );
        assert_eq!(
            translate_navigation_key(
                KeyCode::ArrowDown,
                Key::ArrowDown,
                false,
                true,
                false,
                false
            ),
            b"\x1b[1;3B"
        );
    }

    #[test]
    fn modified_navigation_keys_do_not_use_application_cursor_mode() {
        assert_eq!(
            translate_navigation_key(KeyCode::ArrowUp, Key::ArrowUp, false, true, false, true),
            b"\x1b[1;3A"
        );
        assert_eq!(
            translate_navigation_key(KeyCode::ArrowUp, Key::ArrowUp, false, false, false, true),
            b"\x1bOA"
        );
    }

    #[test]
    fn encodes_modified_page_keys_with_tilde_sequences() {
        assert_eq!(
            translate_navigation_key(KeyCode::PageUp, Key::PageUp, false, true, false, false),
            b"\x1b[5;3~"
        );
        assert_eq!(
            translate_navigation_key(KeyCode::PageDown, Key::PageDown, false, true, false, false),
            b"\x1b[6;3~"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bracketed_paste_wraps_payload_in_markers() {
        assert_eq!(
            encode_paste("echo hi", true),
            b"\x1b[200~echo hi\x1b[201~".to_vec()
        );
    }

    #[test]
    fn bracketed_paste_normalizes_newlines() {
        assert_eq!(
            encode_paste("one\r\ntwo\rthree\n", true),
            b"\x1b[200~one\ntwo\nthree\n\x1b[201~".to_vec()
        );
    }

    #[test]
    fn bracketed_paste_strips_control_introducers() {
        // A 7-bit ESC or 8-bit CSI end marker embedded in the payload must be
        // neutralized so the paste cannot terminate its own bracket.
        assert_eq!(
            encode_paste("before\x1b[201~after", true),
            b"\x1b[200~before[201~after\x1b[201~".to_vec()
        );
        assert_eq!(
            encode_paste("before\u{9b}201~after", true),
            b"\x1b[200~before201~after\x1b[201~".to_vec()
        );
    }

    #[test]
    fn plain_paste_sends_no_markers() {
        assert_eq!(encode_paste("echo hi", false), b"echo hi".to_vec());
    }

    #[test]
    fn plain_paste_sends_newlines_as_carriage_returns() {
        assert_eq!(
            encode_paste("one\r\ntwo\nthree", false),
            b"one\rtwo\rthree".to_vec()
        );
    }
}
