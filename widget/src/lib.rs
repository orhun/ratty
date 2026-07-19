#![doc = include_str!("../README.md")]

use base64::Engine as _;
use ratatui_core::{buffer::Buffer, layout::Rect, widgets::Widget};
use std::borrow::Cow;
use std::io::{self, Write};
use std::path::Path;

const PAYLOAD_CHUNK_SIZE: usize = 3072;

/// Object asset format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectFormat {
    /// Wavefront OBJ.
    Obj,
    /// Binary glTF.
    Glb,
    // STL
    Stl,
}

impl ObjectFormat {
    fn as_str(self) -> &'static str {
        match self {
            Self::Obj => "obj",
            Self::Glb => "glb",
            Self::Stl => "stl",
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
            Some("stl") => Self::Stl,
            _ => Self::Glb,
        }
    }

    fn payload_name(self) -> &'static str {
        match self {
            Self::Obj => "payload.obj",
            Self::Glb => "payload.glb",
            Self::Stl => "payload.stl",
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
    /// Controls registration-time normalization for OBJ assets.
    pub normalize: bool,
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
            normalize: true,
            animate: true,
            scale: 1.0,
            depth: 0.0,
            color: None,
            brightness: 1.0,
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

    /// Enables or disables registration-time normalization for OBJ assets.
    ///
    /// Normalization is enabled by default. With normalization enabled, Ratty
    /// recenters each OBJ mesh around its bounding-box center and scales it by
    /// the largest bounding-box axis so imported models have a predictable
    /// origin and approximate unit size.
    ///
    /// Use `normalize(false)` when the OBJ coordinates are already meaningful,
    /// for example a generated object that uses Ratty's scene coordinates or a
    /// larger assembly made from multiple separately registered objects.
    pub fn normalize(mut self, normalize: bool) -> Self {
        self.normalize = normalize;
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
            "\x1b_ratty;g;r;id={};fmt={};path={};normalize={}\x1b\\",
            self.settings.id,
            self.settings.format.as_str(),
            self.settings.path,
            u8::from(self.settings.normalize)
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
                    "\x1b_ratty;g;r;id={};fmt={};source=payload;more={};name={};normalize={};{}\x1b\\",
                    self.settings.id,
                    self.settings.format.as_str(),
                    more,
                    name,
                    u8::from(self.settings.normalize),
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
                "\x1b_ratty;g;r;id={};fmt={};source=payload;more=0;name={};normalize={};\x1b\\",
                self.settings.id,
                self.settings.format.as_str(),
                name,
                u8::from(self.settings.normalize),
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
        format!(
            "\x1b_ratty;g;p;id={};row={};col={};w={};h={};animate={};scale={};depth={};color={};brightness={};px={};py={};pz={};rx={};ry={};rz={};sx={};sy={};sz={}\x1b\\",
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
        format!(
            "\x1b_ratty;g;u;id={};animate={};scale={};depth={};color={};brightness={};px={};py={};pz={};rx={};ry={};rz={};sx={};sy={};sz={}\x1b\\",
            self.settings.id,
            u8::from(self.settings.animate),
            self.settings.scale,
            self.settings.depth,
            self.settings
                .color
                .map(|[r, g, b]| format!("{r:02x}{g:02x}{b:02x}"))
                .unwrap_or_else(|| "ffffff".to_string()),
            self.settings.brightness,
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

    /// Returns the RGP sequence that deletes every Ratty graphic object.
    ///
    /// This emits `d` without an `id`, which is intentionally broader than
    /// [`Self::delete_sequence`]. Use it for demo cleanup or full-scene reset
    /// flows where removing all currently registered RGP objects is expected.
    pub fn delete_all_sequence() -> String {
        "\x1b_ratty;g;d\x1b\\".to_string()
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

    /// Deletes every Ratty graphic object.
    ///
    /// This writes the RGP delete-all sequence to stdout. It affects all RGP
    /// objects currently known to Ratty, not only objects created by this
    /// process.
    ///
    /// # Errors
    ///
    /// Returns an error if stdout cannot be written or flushed.
    pub fn clear_all() -> io::Result<()> {
        io::stdout().write_all(Self::delete_all_sequence().as_bytes())?;
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
