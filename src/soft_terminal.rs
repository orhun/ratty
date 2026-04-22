use bevy::prelude::*;
use ratatui::Terminal;
use soft_ratatui::embedded_graphics_unicodefonts::{
    mono_8x13_atlas, mono_8x13_bold_atlas, mono_8x13_italic_atlas,
};
use soft_ratatui::{EmbeddedGraphics, SoftBackend};

pub struct SoftTerminal {
    pub terminal: Terminal<SoftBackend<EmbeddedGraphics>>,
    pub image_handle: Option<Handle<Image>>,
    pub cols: u16,
    pub rows: u16,
}

impl SoftTerminal {
    pub fn new(cols: u16, rows: u16) -> Self {
        let backend = SoftBackend::<EmbeddedGraphics>::new(
            cols,
            rows,
            mono_8x13_atlas(),
            Some(mono_8x13_bold_atlas()),
            Some(mono_8x13_italic_atlas()),
        );

        let mut terminal =
            Terminal::new(backend).expect("soft_ratatui backend is infallible for Terminal::new");
        let _ = terminal.clear();
        terminal.backend_mut().cursor = false;

        Self {
            terminal,
            image_handle: None,
            cols,
            rows,
        }
    }
}
