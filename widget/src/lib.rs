use std::borrow::Cow;
use std::io::{self, Write};
use std::path::Path;
use ratatui_core::{buffer::Buffer, layout::Rect, widgets::Widget};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectFormat {
    Obj,
    Glb,
}

impl ObjectFormat {
    fn as_str(self) -> &'static str {
        match self {
            Self::Obj => "obj",
            Self::Glb => "glb",
        }
    }

    fn infer(path: &str) -> Self {
        match Path::new(path)
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .as_deref()
        {
            Some("obj") => Self::Obj,
            _ => Self::Glb,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RattyGraphicSettings<'a> {
    pub id: u32,
    pub path: Cow<'a, str>,
    pub format: ObjectFormat,
    pub animate: bool,
    pub scale: f32,
}

impl<'a> RattyGraphicSettings<'a> {
    pub fn new(path: impl Into<Cow<'a, str>>) -> Self {
        let path = path.into();
        Self {
            id: 1,
            format: ObjectFormat::infer(&path),
            path,
            animate: true,
            scale: 1.0,
        }
    }

    pub fn id(mut self, id: u32) -> Self {
        self.id = id;
        self
    }

    pub fn format(mut self, format: ObjectFormat) -> Self {
        self.format = format;
        self
    }

    pub fn animate(mut self, animate: bool) -> Self {
        self.animate = animate;
        self
    }

    pub fn scale(mut self, scale: f32) -> Self {
        self.scale = scale;
        self
    }
}

pub struct RattyGraphic<'a> {
    settings: RattyGraphicSettings<'a>,
}

impl<'a> RattyGraphic<'a> {
    pub fn new(settings: RattyGraphicSettings<'a>) -> Self {
        Self { settings }
    }

    pub fn settings(&self) -> &RattyGraphicSettings<'a> {
        &self.settings
    }

    pub fn settings_mut(&mut self) -> &mut RattyGraphicSettings<'a> {
        &mut self.settings
    }

    pub fn register_sequence(&self) -> String {
        format!(
            "\x1b_ratty;g;r;id={};fmt={};path={}\x1b\\",
            self.settings.id,
            self.settings.format.as_str(),
            self.settings.path
        )
    }

    pub fn register(&self) -> io::Result<()> {
        io::stdout().write_all(self.register_sequence().as_bytes())?;
        io::stdout().flush()
    }

    pub fn place_sequence(&self, area: Rect) -> String {
        format!(
            "\x1b_ratty;g;p;id={};row={};col={};w={};h={};animate={};scale={}\x1b\\",
            self.settings.id,
            area.y,
            area.x,
            area.width.max(1),
            area.height.max(1),
            u8::from(self.settings.animate),
            self.settings.scale,
        )
    }

    pub fn delete_sequence(&self) -> String {
        format!("\x1b_ratty;g;d;id={}\x1b\\", self.settings.id)
    }

    pub fn clear(&self) -> io::Result<()> {
        io::stdout().write_all(self.delete_sequence().as_bytes())?;
        io::stdout().flush()
    }
}

impl Widget for &RattyGraphic<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }

        let place = self.place_sequence(area);

        if let Some(cell) = buf.cell_mut((area.x, area.y)) {
            let existing = cell.symbol();
            let mut symbol = String::with_capacity(place.len() + existing.len());
            symbol.push_str(&place);
            symbol.push_str(existing);
            cell.set_symbol(&symbol);
        }
    }
}
