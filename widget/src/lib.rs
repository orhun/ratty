#![doc = include_str!("../README.md")]

use base64::Engine as _;
use ratatui_core::{buffer::Buffer, layout::Rect, widgets::Widget};
use std::borrow::Cow;
use std::io::{self, Write};
use std::path::Path;

const PAYLOAD_CHUNK_SIZE: usize = 3072;

/// Placement clipping settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RattyGraphicClip {
    /// Whether clipping is enabled.
    pub enabled: bool,
    /// Optional explicit clip rectangle in terminal cells.
    pub region: Option<Rect>,
}

impl RattyGraphicClip {
    /// Disables clipping.
    pub const fn disabled() -> Self {
        Self {
            enabled: false,
            region: None,
        }
    }

    /// Enables clipping to the placement rectangle.
    pub const fn enabled() -> Self {
        Self {
            enabled: true,
            region: None,
        }
    }

    /// Enables clipping to an explicit terminal-cell rectangle.
    pub const fn region(region: Rect) -> Self {
        Self {
            enabled: true,
            region: Some(region),
        }
    }
}

impl From<bool> for RattyGraphicClip {
    fn from(enabled: bool) -> Self {
        if enabled {
            Self::enabled()
        } else {
            Self::disabled()
        }
    }
}

/// Object asset format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectFormat {
    /// Wavefront OBJ.
    Obj,
    /// Binary glTF.
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

    fn payload_name(self) -> &'static str {
        match self {
            Self::Obj => "payload.obj",
            Self::Glb => "payload.glb",
        }
    }
}

/// Ratty graphic widget settings.
#[derive(Debug, Clone)]
pub struct RattyGraphicSettings<'a> {
    /// Object identifier.
    pub id: u32,
    /// Asset path.
    pub path: Cow<'a, str>,
    /// Asset format.
    pub format: ObjectFormat,
    /// Enables default animation.
    pub animate: bool,
    /// Scale multiplier.
    pub scale: f32,
    /// Extrusion depth.
    pub depth: f32,
    /// Optional object color.
    pub color: Option<[u8; 3]>,
    /// Object brightness multiplier.
    pub brightness: f32,
    /// Placement clipping settings.
    pub clip: RattyGraphicClip,
    /// Translation offset relative to the anchor.
    pub offset: [f32; 3],
    /// Rotation in degrees.
    pub rotation: [f32; 3],
    /// Non-uniform scale multiplier.
    pub scale3: [f32; 3],
}

impl<'a> RattyGraphicSettings<'a> {
    /// Creates widget settings for an asset path.
    pub fn new(path: impl Into<Cow<'a, str>>) -> Self {
        let path = path.into();
        Self {
            id: 1,
            format: ObjectFormat::infer(&path),
            path,
            animate: true,
            scale: 1.0,
            depth: 0.0,
            color: None,
            brightness: 1.0,
            clip: RattyGraphicClip::disabled(),
            offset: [0.0, 0.0, 0.0],
            rotation: [0.0, 0.0, 0.0],
            scale3: [1.0, 1.0, 1.0],
        }
    }

    /// Sets the object identifier.
    pub fn id(mut self, id: u32) -> Self {
        self.id = id;
        self
    }

    /// Sets the asset format.
    pub fn format(mut self, format: ObjectFormat) -> Self {
        self.format = format;
        self
    }

    /// Enables or disables animation.
    pub fn animate(mut self, animate: bool) -> Self {
        self.animate = animate;
        self
    }

    /// Sets the scale multiplier.
    pub fn scale(mut self, scale: f32) -> Self {
        self.scale = scale;
        self
    }

    /// Sets the extrusion depth.
    pub fn depth(mut self, depth: f32) -> Self {
        self.depth = depth;
        self
    }

    /// Sets the object color.
    pub fn color(mut self, color: [u8; 3]) -> Self {
        self.color = Some(color);
        self
    }

    /// Sets the brightness multiplier.
    pub fn brightness(mut self, brightness: f32) -> Self {
        self.brightness = brightness;
        self
    }

    /// Sets clipping against the placement rectangle or an explicit clip region.
    pub fn clip(mut self, clip: impl Into<RattyGraphicClip>) -> Self {
        self.clip = clip.into();
        self
    }

    /// Sets the translation offset relative to the anchor.
    pub fn offset(mut self, offset: [f32; 3]) -> Self {
        self.offset = offset;
        self
    }

    /// Sets the rotation in degrees.
    pub fn rotation(mut self, rotation: [f32; 3]) -> Self {
        self.rotation = rotation;
        self
    }

    /// Sets the non-uniform scale multiplier.
    pub fn scale3(mut self, scale3: [f32; 3]) -> Self {
        self.scale3 = scale3;
        self
    }
}

/// Ratty graphic widget.
pub struct RattyGraphic<'a> {
    settings: RattyGraphicSettings<'a>,
}

impl<'a> RattyGraphic<'a> {
    /// Creates a graphic widget.
    pub fn new(settings: RattyGraphicSettings<'a>) -> Self {
        Self { settings }
    }

    /// Returns the widget settings.
    pub fn settings(&self) -> &RattyGraphicSettings<'a> {
        &self.settings
    }

    /// Returns mutable widget settings.
    pub fn settings_mut(&mut self) -> &mut RattyGraphicSettings<'a> {
        &mut self.settings
    }

    /// Returns the RGP register sequence.
    pub fn register_sequence(&self) -> String {
        format!(
            "\x1b_ratty;g;r;id={};fmt={};path={}\x1b\\",
            self.settings.id,
            self.settings.format.as_str(),
            self.settings.path
        )
    }

    /// Returns the RGP register sequences for a payload-backed asset.
    pub fn register_payload_sequences(&self, bytes: &[u8]) -> Vec<String> {
        self.register_payload_sequences_with_name(bytes, None)
    }

    /// Returns the RGP register sequences for a payload-backed asset with an explicit source name.
    pub fn register_payload_sequences_with_name(
        &self,
        bytes: &[u8],
        name: Option<&str>,
    ) -> Vec<String> {
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        let default_name = Path::new(self.settings.path.as_ref())
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| self.settings.format.payload_name());
        let name = name.unwrap_or(default_name);
        let mut sequences = Vec::new();

        for (index, chunk_start) in (0..encoded.len()).step_by(PAYLOAD_CHUNK_SIZE).enumerate() {
            let chunk_end = (chunk_start + PAYLOAD_CHUNK_SIZE).min(encoded.len());
            let more = u8::from(chunk_end < encoded.len());
            let chunk = &encoded[chunk_start..chunk_end];
            sequences.push(if index == 0 {
                format!(
                    "\x1b_ratty;g;r;id={};fmt={};source=payload;more={};name={};{}\x1b\\",
                    self.settings.id,
                    self.settings.format.as_str(),
                    more,
                    name,
                    chunk
                )
            } else {
                format!(
                    "\x1b_ratty;g;r;id={};fmt={};source=payload;more={};{}\x1b\\",
                    self.settings.id,
                    self.settings.format.as_str(),
                    more,
                    chunk
                )
            });
        }

        if sequences.is_empty() {
            sequences.push(format!(
                "\x1b_ratty;g;r;id={};fmt={};source=payload;more=0;name={};\x1b\\",
                self.settings.id,
                self.settings.format.as_str(),
                name,
            ));
        }

        sequences
    }

    /// Writes the RGP register sequence to stdout.
    ///
    /// # Errors
    ///
    /// Returns an error if stdout cannot be written or flushed.
    pub fn register(&self) -> io::Result<()> {
        io::stdout().write_all(self.register_sequence().as_bytes())?;
        io::stdout().flush()
    }

    /// Writes the RGP register sequences for a payload-backed asset to stdout.
    ///
    /// # Errors
    ///
    /// Returns an error if stdout cannot be written or flushed.
    pub fn register_payload(&self, bytes: &[u8]) -> io::Result<()> {
        self.register_payload_with_name(bytes, None)
    }

    /// Writes the RGP register sequences for a payload-backed asset to stdout with an explicit source name.
    ///
    /// # Errors
    ///
    /// Returns an error if stdout cannot be written or flushed.
    pub fn register_payload_with_name(&self, bytes: &[u8], name: Option<&str>) -> io::Result<()> {
        let mut stdout = io::stdout();
        for sequence in self.register_payload_sequences_with_name(bytes, name) {
            stdout.write_all(sequence.as_bytes())?;
        }
        stdout.flush()
    }

    /// Returns the RGP place sequence for an area.
    pub fn place_sequence(&self, area: Rect) -> String {
        let center_row = area.y.saturating_add(area.height.saturating_sub(1) / 2);
        let center_col = area.x.saturating_add(area.width.saturating_sub(1) / 2);
        let clip_fields = match self.settings.clip.region {
            Some(region) => format!(
                ";clip_row={};clip_col={};clip_w={};clip_h={}",
                region.y,
                region.x,
                region.width.max(1),
                region.height.max(1),
            ),
            None => String::new(),
        };
        format!(
            "\x1b_ratty;g;p;id={};row={};col={};w={};h={};animate={};scale={};depth={};color={};brightness={};clip={}{};px={};py={};pz={};rx={};ry={};rz={};sx={};sy={};sz={}\x1b\\",
            self.settings.id,
            center_row,
            center_col,
            area.width.max(1),
            area.height.max(1),
            u8::from(self.settings.animate),
            self.settings.scale,
            self.settings.depth,
            self.settings
                .color
                .map(|[r, g, b]| format!("{r:02x}{g:02x}{b:02x}"))
                .unwrap_or_else(|| "ffffff".to_string()),
            self.settings.brightness,
            u8::from(self.settings.clip.enabled),
            clip_fields,
            self.settings.offset[0],
            self.settings.offset[1],
            self.settings.offset[2],
            self.settings.rotation[0],
            self.settings.rotation[1],
            self.settings.rotation[2],
            self.settings.scale3[0],
            self.settings.scale3[1],
            self.settings.scale3[2],
        )
    }

    /// Returns the RGP update sequence.
    pub fn update_sequence(&self) -> String {
        let clip_fields = match self.settings.clip.region {
            Some(region) => format!(
                ";clip_row={};clip_col={};clip_w={};clip_h={}",
                region.y,
                region.x,
                region.width.max(1),
                region.height.max(1),
            ),
            None => String::new(),
        };
        format!(
            "\x1b_ratty;g;u;id={};animate={};scale={};depth={};color={};brightness={};clip={}{};px={};py={};pz={};rx={};ry={};rz={};sx={};sy={};sz={}\x1b\\",
            self.settings.id,
            u8::from(self.settings.animate),
            self.settings.scale,
            self.settings.depth,
            self.settings
                .color
                .map(|[r, g, b]| format!("{r:02x}{g:02x}{b:02x}"))
                .unwrap_or_else(|| "ffffff".to_string()),
            self.settings.brightness,
            u8::from(self.settings.clip.enabled),
            clip_fields,
            self.settings.offset[0],
            self.settings.offset[1],
            self.settings.offset[2],
            self.settings.rotation[0],
            self.settings.rotation[1],
            self.settings.rotation[2],
            self.settings.scale3[0],
            self.settings.scale3[1],
            self.settings.scale3[2],
        )
    }

    /// Returns the RGP delete sequence.
    pub fn delete_sequence(&self) -> String {
        format!("\x1b_ratty;g;d;id={}\x1b\\", self.settings.id)
    }

    /// Writes the RGP delete sequence to stdout.
    ///
    /// # Errors
    ///
    /// Returns an error if stdout cannot be written or flushed.
    pub fn clear(&self) -> io::Result<()> {
        io::stdout().write_all(self.delete_sequence().as_bytes())?;
        io::stdout().flush()
    }

    /// Writes the RGP update sequence to stdout.
    ///
    /// # Errors
    ///
    /// Returns an error if stdout cannot be written or flushed.
    pub fn update(&self) -> io::Result<()> {
        io::stdout().write_all(self.update_sequence().as_bytes())?;
        io::stdout().flush()
    }
}

/// Renders the place sequence into a Ratatui buffer.
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
