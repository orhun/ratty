use std::sync::mpsc::TryRecvError;

use bevy::app::AppExit;
use bevy::ecs::message::{MessageReader, MessageWriter};
use bevy::input::ButtonState;
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::prelude::*;
use ratatui::style::Style;
use ratatui::widgets::Paragraph;

use crate::config::{CURSOR_DEPTH, CURSOR_SCALE_FACTOR, THEME_BG, THEME_FG};
use crate::model::AssetShowcase;
use crate::runtime::TerminalRuntime;
use crate::scene::TerminalViewport;
use crate::soft_terminal::SoftTerminal;

pub fn handle_keyboard_input(
    mut keyboard_events: MessageReader<KeyboardInput>,
    keys: Res<ButtonInput<KeyCode>>,
    runtime: NonSend<TerminalRuntime>,
) {
    let ctrl_pressed = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    let alt_pressed = keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight);

    for event in keyboard_events.read() {
        if event.state != ButtonState::Pressed {
            continue;
        }

        let mut input = Vec::new();
        if ctrl_pressed {
            if let Some(ctrl) = ctrl_keycode_byte(event.key_code) {
                input.push(ctrl);
                runtime.write_input(&input);
                continue;
            }
        }

        match event.key_code {
            KeyCode::Enter | KeyCode::NumpadEnter => input.push(b'\r'),
            KeyCode::Tab => input.push(b'\t'),
            KeyCode::Backspace => input.push(0x7f),
            KeyCode::Escape => input.push(0x1b),
            KeyCode::ArrowUp => input.extend_from_slice(b"\x1b[A"),
            KeyCode::ArrowDown => input.extend_from_slice(b"\x1b[B"),
            KeyCode::ArrowRight => input.extend_from_slice(b"\x1b[C"),
            KeyCode::ArrowLeft => input.extend_from_slice(b"\x1b[D"),
            KeyCode::Delete => input.extend_from_slice(b"\x1b[3~"),
            KeyCode::Home => input.extend_from_slice(b"\x1b[H"),
            KeyCode::End => input.extend_from_slice(b"\x1b[F"),
            KeyCode::PageUp => input.extend_from_slice(b"\x1b[5~"),
            KeyCode::PageDown => input.extend_from_slice(b"\x1b[6~"),
            _ => {
                if let Key::Character(chars) = &event.logical_key {
                    if alt_pressed {
                        input.push(0x1b);
                    }
                    input.extend_from_slice(chars.as_bytes());
                }
            }
        }

        runtime.write_input(&input);
    }
}

pub fn pump_pty_output(
    mut runtime: NonSendMut<TerminalRuntime>,
    mut app_exit: MessageWriter<AppExit>,
) {
    loop {
        match runtime.rx.try_recv() {
            Ok(chunk) => runtime.parser.process(&chunk),
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                if !runtime.pty_disconnected {
                    runtime.pty_disconnected = true;
                    app_exit.write(AppExit::Success);
                }
                break;
            }
        }
    }
}

pub fn redraw_soft_terminal(
    runtime: NonSend<TerminalRuntime>,
    mut soft_terminal: NonSendMut<SoftTerminal>,
    mut images: ResMut<Assets<Image>>,
) {
    let screen = runtime.parser.screen();
    let text = screen.contents();

    let _ = soft_terminal.terminal.draw(|frame| {
        let area = frame.area();
        frame.render_widget(
            Paragraph::new(text.as_str()).style(Style::default().fg(THEME_FG).bg(THEME_BG)),
            area,
        );
    });

    if let Some(handle) = soft_terminal.image_handle.as_ref()
        && let Some(image) = images.get_mut(handle)
    {
        image.data = Some(soft_terminal.terminal.backend().get_pixmap_data_as_rgba());
    }
}

pub fn sync_asset_to_terminal_cursor(
    runtime: NonSend<TerminalRuntime>,
    soft_terminal: NonSend<SoftTerminal>,
    viewport: Res<TerminalViewport>,
    time: Res<Time>,
    mut query: Query<(&mut Transform, &mut Visibility), With<AssetShowcase>>,
) {
    let cols = soft_terminal.cols.max(1) as f32;
    let rows = soft_terminal.rows.max(1) as f32;
    let cell_width = viewport.size.x / cols;
    let cell_height = viewport.size.y / rows;
    let scale = cell_width.min(cell_height) * CURSOR_SCALE_FACTOR;

    let screen = runtime.parser.screen();
    let (cursor_row, cursor_col) = screen.cursor_position();
    let cursor_col = cursor_col.min(soft_terminal.cols.saturating_sub(1)) as f32;
    let cursor_row = cursor_row.min(soft_terminal.rows.saturating_sub(1)) as f32;

    let world_x = viewport.center.x - viewport.size.x * 0.5 + (cursor_col + 0.5) * cell_width;
    let world_y = viewport.center.y + viewport.size.y * 0.5 - (cursor_row + 0.5) * cell_height;
    let spin = time.elapsed_secs() * 1.4;
    let bob = (time.elapsed_secs() * 2.2).sin() * cell_height * 0.08;

    for (mut transform, mut visibility) in &mut query {
        transform.translation = Vec3::new(world_x, world_y + bob, CURSOR_DEPTH);
        transform.rotation = Quat::from_rotation_y(spin) * Quat::from_rotation_x(-0.25);
        transform.scale = Vec3::splat(scale.max(0.001));
        *visibility = if screen.hide_cursor() {
            Visibility::Hidden
        } else {
            Visibility::Visible
        };
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
