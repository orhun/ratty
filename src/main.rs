use std::env;
use std::io::{ErrorKind, Read, Write};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::Context;
use bevy::app::AppExit;
use bevy::input::ButtonState;
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::prelude::*;
use bevy::sprite::Anchor;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use vte::{Params, Parser, Perform};

const WINDOW_WIDTH: f32 = 1200.0;
const WINDOW_HEIGHT: f32 = 720.0;
const FONT_SIZE: f32 = 18.0;
const LINE_HEIGHT: f32 = 20.0;
const PADDING_X: f32 = 10.0;
const PADDING_Y: f32 = 10.0;
const DEFAULT_COLS: u16 = 120;
const DEFAULT_ROWS: u16 = 36;

fn main() -> anyhow::Result<()> {
    let runtime = TerminalRuntime::spawn(DEFAULT_COLS, DEFAULT_ROWS)?;

    App::new()
        .insert_resource(ClearColor(Color::BLACK))
        .insert_non_send_resource(runtime)
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "ratterm".into(),
                resolution: (WINDOW_WIDTH, WINDOW_HEIGHT).into(),
                resizable: false,
                ..default()
            }),
            ..default()
        }))
        .add_plugins(TerminalPlugin)
        .run();

    Ok(())
}

struct TerminalPlugin;

impl Plugin for TerminalPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_terminal_ui)
            .add_systems(Update, pump_pty_output)
            .add_systems(Update, handle_keyboard_input.after(pump_pty_output))
            .add_systems(Update, refresh_terminal_rows.after(pump_pty_output));
    }
}

#[derive(Component, Clone, Copy)]
struct TerminalRow(usize);

struct TerminalRuntime {
    rx: Receiver<Vec<u8>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    _master: Box<dyn MasterPty + Send>,
    _child: Box<dyn portable_pty::Child + Send + Sync>,
    pty_disconnected: bool,
    parser: Parser,
    state: TerminalState,
}

impl TerminalRuntime {
    fn spawn(cols: u16, rows: u16) -> anyhow::Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("failed to create PTY pair")?;

        let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        let mut cmd = CommandBuilder::new(shell);
        cmd.env("TERM", "xterm-256color");
        let child = pair
            .slave
            .spawn_command(cmd)
            .context("failed to spawn shell")?;
        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .context("failed to clone PTY reader")?;
        let writer = pair
            .master
            .take_writer()
            .context("failed to create PTY writer")?;

        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        thread::spawn(move || {
            let mut buf = [0_u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(size) => {
                        if tx.send(buf[..size].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            rx,
            writer: Arc::new(Mutex::new(writer)),
            _master: pair.master,
            _child: child,
            pty_disconnected: false,
            parser: Parser::new(),
            state: TerminalState::new(cols, rows),
        })
    }

    fn write_input(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }

        if let Ok(mut writer) = self.writer.lock() {
            let _ = writer.write_all(bytes);
            let _ = writer.flush();
        }
    }
}

#[derive(Clone)]
struct TerminalState {
    cols: usize,
    rows: usize,
    grid: Vec<Vec<char>>,
    cursor_x: usize,
    cursor_y: usize,
}

impl TerminalState {
    fn new(cols: u16, rows: u16) -> Self {
        let cols = cols as usize;
        let rows = rows as usize;
        let grid = vec![vec![' '; cols]; rows];

        Self {
            cols,
            rows,
            grid,
            cursor_x: 0,
            cursor_y: 0,
        }
    }

    fn row_text(&self, row: usize) -> String {
        let mut text: String = self.grid[row].iter().collect();
        while text.ends_with(' ') {
            text.pop();
        }
        text
    }

    fn print(&mut self, ch: char) {
        if self.cols == 0 || self.rows == 0 {
            return;
        }

        if self.cursor_x >= self.cols {
            self.newline();
        }

        if self.cursor_y >= self.rows {
            self.cursor_y = self.rows - 1;
        }

        self.grid[self.cursor_y][self.cursor_x] = ch;
        self.cursor_x += 1;

        if self.cursor_x >= self.cols {
            self.newline();
        }
    }

    fn newline(&mut self) {
        self.cursor_x = 0;

        if self.cursor_y + 1 >= self.rows {
            self.scroll_up();
        } else {
            self.cursor_y += 1;
        }
    }

    fn carriage_return(&mut self) {
        self.cursor_x = 0;
    }

    fn backspace(&mut self) {
        if self.cursor_x > 0 {
            self.cursor_x -= 1;
        }
    }

    fn tab(&mut self) {
        let next_tab = ((self.cursor_x / 8) + 1) * 8;
        while self.cursor_x < next_tab {
            self.print(' ');
        }
    }

    fn scroll_up(&mut self) {
        self.grid.remove(0);
        self.grid.push(vec![' '; self.cols]);
    }

    fn move_cursor(&mut self, row: usize, col: usize) {
        self.cursor_y = row.min(self.rows.saturating_sub(1));
        self.cursor_x = col.min(self.cols.saturating_sub(1));
    }

    fn clear_screen(&mut self) {
        for row in &mut self.grid {
            for cell in row {
                *cell = ' ';
            }
        }
        self.move_cursor(0, 0);
    }

    fn erase_display(&mut self, mode: usize) {
        match mode {
            0 => {
                self.erase_line_from_cursor(0);
                for row in (self.cursor_y + 1)..self.rows {
                    self.grid[row].fill(' ');
                }
            }
            1 => {
                self.erase_line_from_cursor(1);
                for row in 0..self.cursor_y {
                    self.grid[row].fill(' ');
                }
            }
            2 => self.clear_screen(),
            _ => {}
        }
    }

    fn erase_line_from_cursor(&mut self, mode: usize) {
        match mode {
            0 => {
                for col in self.cursor_x..self.cols {
                    self.grid[self.cursor_y][col] = ' ';
                }
            }
            1 => {
                for col in 0..=self.cursor_x.min(self.cols.saturating_sub(1)) {
                    self.grid[self.cursor_y][col] = ' ';
                }
            }
            2 => {
                self.grid[self.cursor_y].fill(' ');
            }
            _ => {}
        }
    }
}

struct TerminalPerformer<'a> {
    state: &'a mut TerminalState,
}

impl<'a> TerminalPerformer<'a> {
    fn new(state: &'a mut TerminalState) -> Self {
        Self { state }
    }
}

impl Perform for TerminalPerformer<'_> {
    fn print(&mut self, c: char) {
        self.state.print(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' => self.state.newline(),
            b'\r' => self.state.carriage_return(),
            b'\t' => self.state.tab(),
            0x08 => self.state.backspace(),
            _ => {}
        }
    }

    fn csi_dispatch(
        &mut self,
        params: &Params,
        _intermediates: &[u8],
        _ignore: bool,
        action: char,
    ) {
        let p0 = param(params, 0).unwrap_or(0);
        let p1 = param(params, 1).unwrap_or(1);

        match action {
            'A' => {
                let amount = p0.max(1);
                let row = self.state.cursor_y.saturating_sub(amount);
                self.state.move_cursor(row, self.state.cursor_x);
            }
            'B' => {
                let amount = p0.max(1);
                self.state
                    .move_cursor(self.state.cursor_y + amount, self.state.cursor_x);
            }
            'C' => {
                let amount = p0.max(1);
                self.state
                    .move_cursor(self.state.cursor_y, self.state.cursor_x + amount);
            }
            'D' => {
                let amount = p0.max(1);
                let col = self.state.cursor_x.saturating_sub(amount);
                self.state.move_cursor(self.state.cursor_y, col);
            }
            'H' | 'f' => {
                let row = p0.max(1) - 1;
                let col = p1.max(1) - 1;
                self.state.move_cursor(row, col);
            }
            'J' => self.state.erase_display(p0),
            'K' => self.state.erase_line_from_cursor(p0),
            'm' => {}
            _ => {}
        }
    }
}

fn param(params: &Params, index: usize) -> Option<usize> {
    params
        .iter()
        .nth(index)
        .and_then(|values| values.first())
        .map(|value| *value as usize)
}

fn setup_terminal_ui(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    runtime: NonSend<TerminalRuntime>,
) {
    commands.spawn(Camera2dBundle::default());

    let font = asset_server.load("fonts/DejaVuSansMono.ttf");
    let origin_x = -WINDOW_WIDTH * 0.5 + PADDING_X;
    let origin_y = WINDOW_HEIGHT * 0.5 - PADDING_Y;

    for row in 0..runtime.state.rows {
        commands.spawn((
            TerminalRow(row),
            Text2dBundle {
                text: Text::from_section(
                    "",
                    TextStyle {
                        font: font.clone(),
                        font_size: FONT_SIZE,
                        color: Color::WHITE,
                    },
                ),
                text_anchor: Anchor::TopLeft,
                transform: Transform::from_translation(Vec3::new(
                    origin_x,
                    origin_y - (row as f32 * LINE_HEIGHT),
                    0.0,
                )),
                ..default()
            },
        ));
    }
}

fn pump_pty_output(mut runtime: NonSendMut<TerminalRuntime>, mut app_exit: EventWriter<AppExit>) {
    let TerminalRuntime {
        rx,
        parser,
        state,
        pty_disconnected,
        ..
    } = &mut *runtime;

    loop {
        match rx.try_recv() {
            Ok(chunk) => {
                let mut performer = TerminalPerformer::new(state);
                for byte in chunk {
                    parser.advance(&mut performer, byte);
                }
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                if !*pty_disconnected {
                    *pty_disconnected = true;
                    app_exit.send(AppExit::Success);
                }
                break;
            }
        }
    }
}

fn handle_keyboard_input(
    mut keyboard_events: EventReader<KeyboardInput>,
    keys: Res<ButtonInput<KeyCode>>,
    runtime: NonSend<TerminalRuntime>,
) {
    let mut input = Vec::new();

    for event in keyboard_events.read() {
        if event.state != ButtonState::Pressed {
            continue;
        }

        let ctrl_pressed = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
        match &event.logical_key {
            Key::Character(chars) => {
                if ctrl_pressed {
                    if let Some(byte) = ctrl_character_byte(chars) {
                        input.push(byte);
                    }
                } else {
                    input.extend_from_slice(chars.as_bytes());
                }
            }
            Key::Enter => input.push(b'\r'),
            Key::Tab => input.push(b'\t'),
            Key::Backspace => input.push(0x7f),
            Key::ArrowUp => input.extend_from_slice(b"\x1b[A"),
            Key::ArrowDown => input.extend_from_slice(b"\x1b[B"),
            Key::ArrowRight => input.extend_from_slice(b"\x1b[C"),
            Key::ArrowLeft => input.extend_from_slice(b"\x1b[D"),
            Key::Delete => input.extend_from_slice(b"\x1b[3~"),
            Key::Home => input.extend_from_slice(b"\x1b[H"),
            Key::End => input.extend_from_slice(b"\x1b[F"),
            Key::PageUp => input.extend_from_slice(b"\x1b[5~"),
            Key::PageDown => input.extend_from_slice(b"\x1b[6~"),
            Key::Escape => input.push(0x1b),
            _ => {}
        }
    }

    runtime.write_input(&input);
}

fn ctrl_character_byte(chars: &str) -> Option<u8> {
    let ch = chars.chars().next()?.to_ascii_lowercase();
    if !ch.is_ascii_lowercase() {
        return None;
    }

    Some((ch as u8) - b'a' + 1)
}

fn refresh_terminal_rows(
    runtime: NonSend<TerminalRuntime>,
    mut row_text_query: Query<(&TerminalRow, &mut Text)>,
) {
    for (row, mut text) in &mut row_text_query {
        text.sections[0].value = runtime.state.row_text(row.0);
    }
}
