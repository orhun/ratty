use std::{
    fs, io,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    DefaultTerminal,
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph, Widget},
};
use ratatui_ratty::{RattyGraphic, RattyGraphicSettings};

fn main() -> io::Result<()> {
    let mut terminal = ratatui::init();
    let result = run(&mut terminal);
    ratatui::restore();
    result
}

fn run(terminal: &mut DefaultTerminal) -> io::Result<()> {
    let mut document = TempleEditor::new();

    loop {
        terminal.draw(|frame| {
            let area = frame.area();
            let cursor = document.render(frame.buffer_mut(), area);
            frame.set_cursor_position(cursor);
        })?;

        if let Event::Key(key) = event::read()? {
            if matches!(key.code, KeyCode::Char('q')) && key.modifiers.is_empty() {
                document.clear()?;
                return Ok(());
            }
            document.handle_key(key);
        }
    }
}

struct TempleEditor<'a> {
    lines: Vec<Vec<DocCell>>,
    objects: Vec<Option<PlacedGraphic<'a>>>,
    asset_pool: Vec<String>,
    next_object_id: u32,
    debug_cells: bool,
    cursor_row: usize,
    cursor_col: usize,
    scroll: u16,
    viewport_height: u16,
}

impl<'a> TempleEditor<'a> {
    fn new() -> Self {
        let mut editor = Self {
            lines: initial_lines(),
            objects: Vec::new(),
            asset_pool: discover_obj_assets().unwrap_or_default(),
            next_object_id: 100,
            debug_cells: false,
            cursor_row: 0,
            cursor_col: 0,
            scroll: 0,
            viewport_height: 1,
        };
        editor.insert_startup_objects();
        editor
    }

    fn render(&mut self, buf: &mut Buffer, area: Rect) -> (u16, u16) {
        let header = Rect::new(area.x, area.y, area.width, 3);
        let body = Rect::new(
            area.x,
            area.y.saturating_add(3),
            area.width,
            area.height.saturating_sub(3),
        );

        Paragraph::new(Line::from(vec![
            Span::styled("arrows", Style::default().fg(Color::Cyan)),
            Span::raw(": move  "),
            Span::styled("ctrl+v", Style::default().fg(Color::Cyan)),
            Span::raw(": insert obj  "),
            Span::styled("enter", Style::default().fg(Color::Cyan)),
            Span::raw(": split  "),
            Span::styled("backspace/delete", Style::default().fg(Color::Cyan)),
            Span::raw(": edit  "),
            Span::styled("d", Style::default().fg(Color::Cyan)),
            Span::raw(": debug  "),
            Span::styled("q", Style::default().fg(Color::Cyan)),
            Span::raw(": quit"),
        ]))
        .block(Block::bordered().title(Span::styled(
            "Ratty Editor Demo",
            Style::default().fg(Color::Yellow),
        )))
        .render(header, buf);

        let block = Block::bordered().title("TempleOS-Notes.HC");
        let inner = block.inner(body);
        block.render(body, buf);

        self.viewport_height = inner.height.max(1);
        self.ensure_cursor_visible();

        for y in 0..inner.height {
            let row = self.scroll as usize + y as usize;
            if row >= self.lines.len() {
                continue;
            }
            self.render_line(buf, inner, row, y);
        }

        self.sync_objects(buf, inner);

        (
            inner.x.saturating_add(self.cursor_col as u16),
            inner
                .y
                .saturating_add(self.cursor_row as u16)
                .saturating_sub(self.scroll),
        )
    }

    fn render_line(&self, buf: &mut Buffer, inner: Rect, row: usize, view_y: u16) {
        let y = inner.y.saturating_add(view_y);
        let line = &self.lines[row];
        for (index, cell) in line.iter().take(inner.width as usize).enumerate() {
            let x = inner.x.saturating_add(index as u16);
            match cell {
                DocCell::Char(ch) => {
                    if let Some(screen_cell) = buf.cell_mut((x, y)) {
                        screen_cell.set_char(*ch);
                    }
                }
                DocCell::Object(_) => {
                    if let Some(screen_cell) = buf.cell_mut((x, y)) {
                        screen_cell.set_char(' ');
                        if self.debug_cells {
                            screen_cell.set_style(Style::default().bg(Color::Gray));
                        }
                    }
                }
            }
        }
    }

    fn sync_objects(&mut self, buf: &mut Buffer, inner: Rect) {
        let visible_top = self.scroll as usize;
        let visible_bottom = visible_top + inner.height as usize;

        for row in 0..self.lines.len() {
            if row < visible_top || row >= visible_bottom {
                continue;
            }

            let view_y = (row - visible_top) as u16;
            for (col, cell) in self.lines[row].iter().enumerate() {
                let DocCell::Object(index) = cell else {
                    continue;
                };
                let Some(object) = self.objects.get_mut(*index).and_then(Option::as_mut) else {
                    continue;
                };
                let anchor_x = inner.x.saturating_add(col as u16);
                let anchor_y = inner.y.saturating_add(view_y);
                let place = place_at_anchor(
                    &object.graphic,
                    anchor_x,
                    anchor_y,
                    object.width,
                    object.height,
                );

                if !object.visible {
                    emit_sequence(buf, anchor_x, anchor_y, &place);
                    emit_sequence(buf, anchor_x, anchor_y, &object.graphic.register_sequence());
                    object.visible = true;
                } else {
                    emit_sequence(buf, anchor_x, anchor_y, &place);
                }
            }
        }

        let mut keep_visible = vec![false; self.objects.len()];
        for row in visible_top..visible_bottom.min(self.lines.len()) {
            for cell in &self.lines[row] {
                if let DocCell::Object(index) = cell
                    && *index < keep_visible.len()
                {
                    keep_visible[*index] = true;
                }
            }
        }

        for (index, object) in self.objects.iter_mut().enumerate() {
            let Some(object) = object.as_mut() else {
                continue;
            };
            if object.visible && !keep_visible.get(index).copied().unwrap_or(false) {
                emit_sequence(buf, inner.x, inner.y, &object.graphic.delete_sequence());
                object.visible = false;
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Left => self.move_left(),
            KeyCode::Right => self.move_right(),
            KeyCode::Up => self.move_up(),
            KeyCode::Down => self.move_down(),
            KeyCode::Home => self.cursor_col = 0,
            KeyCode::End => self.cursor_col = self.current_line().len(),
            KeyCode::Enter => self.insert_newline(),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete(),
            KeyCode::Char('d') if key.modifiers.is_empty() => {
                self.debug_cells = !self.debug_cells;
            }
            KeyCode::Char('v') if key.modifiers == KeyModifiers::CONTROL => {
                self.insert_random_object()
            }
            KeyCode::Tab => {
                for _ in 0..4 {
                    self.insert_char(' ');
                }
            }
            KeyCode::Char(ch)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                self.insert_char(ch);
            }
            _ => {}
        }

        self.ensure_cursor_in_bounds();
        self.ensure_cursor_visible();
    }

    fn insert_object(
        &mut self,
        id: u32,
        path: &'a str,
        row: usize,
        col: usize,
        width: u16,
        height: u16,
        scale: f32,
        animate: bool,
    ) {
        while self.lines.len() <= row {
            self.lines.push(Vec::new());
        }
        while self.lines[row].len() < col {
            self.lines[row].push(DocCell::Char(' '));
        }

        let index = self.objects.len();
        self.objects.push(Some(PlacedGraphic {
            graphic: RattyGraphic::new(
                RattyGraphicSettings::new(path)
                    .id(id)
                    .scale(scale)
                    .depth(3.0)
                    .color(random_color(id))
                    .brightness(1.0)
                    .animate(animate),
            ),
            width,
            height,
            visible: false,
        }));
        self.lines[row].insert(col, DocCell::Object(index));
    }

    fn insert_startup_objects(&mut self) {
        let presets = [
            ("sprite_12_offset_32218.obj", 6, 36, 6, 4, 0.75),
            ("black.obj", 17, 12, 12, 7, 1.00),
            ("bomber.obj", 32, 10, 18, 7, 1.00),
            ("battle.obj", 32, 30, 14, 8, 1.00),
            ("sprite_18_offset_48048.obj", 47, 16, 16, 8, 0.95),
        ];

        for (name, row, col, width, height, scale) in presets {
            if self.asset_pool.iter().any(|asset| asset == name) {
                let path = Box::leak(format!("widget/assets/{name}").into_boxed_str());
                let id = self.next_object_id;
                self.next_object_id += 1;
                self.insert_object(id, path, row, col, width, height, scale, true);
            }
        }
    }

    fn insert_random_object(&mut self) {
        if self.asset_pool.is_empty() {
            return;
        }

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos() as usize)
            .unwrap_or(0);
        let asset = self.asset_pool[nanos % self.asset_pool.len()].clone();
        let path = Box::leak(format!("widget/assets/{asset}").into_boxed_str());
        let row = self.cursor_row;
        let col = self.cursor_col;
        let id = self.next_object_id;
        self.next_object_id += 1;
        self.insert_object(id, path, row, col, 16, 8, 1.0, true);
        self.cursor_col += 1;
    }

    fn insert_char(&mut self, ch: char) {
        let cursor_row = self.cursor_row;
        let cursor_col = self.cursor_col;
        self.lines[cursor_row].insert(cursor_col, DocCell::Char(ch));
        self.cursor_col += 1;
    }

    fn insert_newline(&mut self) {
        let tail = self.lines[self.cursor_row].split_off(self.cursor_col);
        self.lines.insert(self.cursor_row + 1, tail);
        self.cursor_row += 1;
        self.cursor_col = 0;
    }

    fn backspace(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
            self.remove_cell(self.cursor_row, self.cursor_col);
        } else if self.cursor_row > 0 {
            let current = self.lines.remove(self.cursor_row);
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].len();
            self.lines[self.cursor_row].extend(current);
        }
    }

    fn delete(&mut self) {
        if self.cursor_col < self.current_line().len() {
            self.remove_cell(self.cursor_row, self.cursor_col);
        } else if self.cursor_row + 1 < self.lines.len() {
            let next = self.lines.remove(self.cursor_row + 1);
            self.lines[self.cursor_row].extend(next);
        }
    }

    fn remove_cell(&mut self, row: usize, col: usize) {
        if col >= self.lines[row].len() {
            return;
        }
        let removed = self.lines[row].remove(col);
        if let DocCell::Object(index) = removed
            && let Some(object) = self.objects.get_mut(index).and_then(Option::take)
        {
            let _ = object.graphic.clear();
        }
    }

    fn move_left(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].len();
        }
    }

    fn move_right(&mut self) {
        if self.cursor_col < self.current_line().len() {
            self.cursor_col += 1;
        } else if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = 0;
        }
    }

    fn move_up(&mut self) {
        if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.cursor_col.min(self.current_line().len());
        }
    }

    fn move_down(&mut self) {
        if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = self.cursor_col.min(self.current_line().len());
        }
    }

    fn current_line(&self) -> &Vec<DocCell> {
        &self.lines[self.cursor_row]
    }

    fn ensure_cursor_in_bounds(&mut self) {
        self.cursor_row = self.cursor_row.min(self.lines.len().saturating_sub(1));
        self.cursor_col = self.cursor_col.min(self.current_line().len());
    }

    fn ensure_cursor_visible(&mut self) {
        let cursor_row = self.cursor_row as u16;
        if cursor_row < self.scroll {
            self.scroll = cursor_row;
        } else if cursor_row >= self.scroll.saturating_add(self.viewport_height) {
            self.scroll = cursor_row.saturating_sub(self.viewport_height.saturating_sub(1));
        }
    }

    fn clear(&self) -> io::Result<()> {
        for object in &self.objects {
            if let Some(object) = object {
                object.graphic.clear()?;
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
enum DocCell {
    Char(char),
    Object(usize),
}

struct PlacedGraphic<'a> {
    graphic: RattyGraphic<'a>,
    width: u16,
    height: u16,
    visible: bool,
}

fn initial_lines() -> Vec<Vec<DocCell>> {
    [
        "TempleOS was Terry A. Davis's operating system, compiler, editor,",
        "shell, graphics stack, and personal computing world built around",
        "a deliberately small and unified design.",
        "",
        "> In TempleOS, a program can contain sprite data inline, so art",
        "> assets for a game or demo can live in the same .HC file as",
        "> the code that draws or uses them.",
        "",
        "Ratty is inspired by that direction. The goal here is not only to",
        "render graphics in a terminal window, but to let them belong to the",
        "same editable surface as the text around them.",
        "",
        "This editor keeps 3D objects as first-class document cells.",
        "Move around, type, split lines, delete text, and the objects move",
        "with the surrounding document instead of floating above it.",
        "",
        "",
        "",
        "",
        "",
        "",
        "Above is one inline object placed directly in the document flow.",
        "It is not a detached overlay or preview. It lives in the same",
        "buffer as the lines describing what you are looking at.",
        "",
        "The important idea here is not only that objects can be rendered.",
        "It is that they can belong to the document itself.",
        "",
        "Here is another example. Two extracted TempleOS assets can sit",
        "side by side inside the same editable buffer, still anchored to",
        "cells, still moving when the surrounding text changes.",
        "",
        "",
        "",
        "",
        "The assets in this demo were extracted from TempleOS itself and",
        "dropped into the same editable surface as the text that describes them.",
        "",
        "That makes the command line feel less like a scrolling log and",
        "more like a document or notebook where code, notes, and graphics",
        "share one continuous space.",
        "",
        "Now try it yourself. Press Ctrl+V to insert a random TempleOS",
        "object anywhere in this document, then edit around it and watch",
        "it stay attached to the document instead of the screen.",
        "",
        "",
        "",
        "",
        "",
        "-----",
        "",
        "What's reality?",
        "I don't know. When my bird was looking at my computer monitor I",
        "thought, \"That bird has no idea what he's looking at.\" And yet",
        "what does the bird do? Does he panic? No, he can't really panic,",
        "he just does the best he can. Is he able to live in a world where",
        "he's so ignorant? Well, he doesn't really have a choice. The bird",
        "is okay even though he doesn't understand the world. You're that",
        "bird looking at the monitor, and you're thinking to yourself,",
        "\"I can figure this out.\" Maybe you have some bird ideas.",
        "",
        "Maybe that's the best you can do.",
        "",
        "— Terry Davis",
        "",
        "EOF",
    ]
    .into_iter()
    .map(|line| line.chars().map(DocCell::Char).collect())
    .collect()
}

fn emit_sequence(buf: &mut Buffer, x: u16, y: u16, sequence: &str) {
    let Some(cell) = buf.cell_mut((x, y)) else {
        return;
    };
    let existing = cell.symbol();
    let mut symbol = String::with_capacity(sequence.len() + existing.len());
    symbol.push_str(sequence);
    symbol.push_str(existing);
    cell.set_symbol(&symbol);
}

fn place_at_anchor(
    graphic: &RattyGraphic<'_>,
    anchor_x: u16,
    anchor_y: u16,
    width: u16,
    height: u16,
) -> String {
    let settings = graphic.settings();
    format!(
        "\x1b_ratty;g;p;id={};row={};col={};w={};h={};animate={};scale={};depth={};color={};brightness={}\x1b\\",
        settings.id,
        anchor_y,
        anchor_x,
        width.max(1),
        height.max(1),
        u8::from(settings.animate),
        settings.scale,
        settings.depth,
        settings
            .color
            .map(|[r, g, b]| format!("{r:02x}{g:02x}{b:02x}"))
            .unwrap_or_else(|| "ffffff".to_string()),
        settings.brightness,
    )
}

fn discover_obj_assets() -> io::Result<Vec<String>> {
    let assets_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets");
    let mut assets = fs::read_dir(assets_dir)?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            (path.extension().and_then(|ext| ext.to_str()) == Some("obj"))
                .then(|| path.file_name()?.to_str().map(ToOwned::to_owned))
                .flatten()
        })
        .collect::<Vec<_>>();
    assets.sort();
    Ok(assets)
}

fn random_color(seed: u32) -> [u8; 3] {
    const PALETTE: [[u8; 3]; 8] = [
        [255, 111, 97],
        [255, 179, 71],
        [255, 241, 118],
        [129, 199, 132],
        [79, 195, 247],
        [126, 87, 194],
        [244, 143, 177],
        [144, 202, 249],
    ];
    let mut seed = seed;
    seed ^= seed << 13;
    seed ^= seed >> 17;
    seed ^= seed << 5;
    PALETTE[(seed as usize) % PALETTE.len()]
}
