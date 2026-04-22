use bevy::prelude::*;
use ratatui::Terminal;
use soft_ratatui::{CosmicText, SoftBackend};

use crate::config::TERMINAL_FONT_SIZE;

static TERMINAL_FONT_DATA: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/fonts/CaskaydiaCoveNerdFontComplete-Regular.otf"
));

pub struct SoftTerminal {
    pub terminal: Terminal<SoftBackend<CosmicText>>,
    pub image_handle: Option<Handle<Image>>,
    pub cols: u16,
    pub rows: u16,
}

impl SoftTerminal {
    pub fn new(cols: u16, rows: u16) -> Self {
        let backend =
            SoftBackend::<CosmicText>::new(cols, rows, TERMINAL_FONT_SIZE, TERMINAL_FONT_DATA);

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
