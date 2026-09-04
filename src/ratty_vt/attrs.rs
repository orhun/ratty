use crate::ratty_vt::term::BufWrite as _;

/// Represents a foreground or background color for cells.
#[derive(Eq, PartialEq, Debug, Copy, Clone, Default)]
pub enum Color {
    /// The default terminal color.
    #[default]
    Default,

    /// An indexed terminal color.
    Idx(u8),

    /// An RGB terminal color. The parameters are (red, green, blue).
    Rgb(u8, u8, u8),
}

const TEXT_MODE_INTENSITY: u8 = 0b0000_0011;
const TEXT_MODE_BOLD: u8 = 0b0000_0001;
const TEXT_MODE_DIM: u8 = 0b0000_0010;
const TEXT_MODE_ITALIC: u8 = 0b0000_0100;
const TEXT_MODE_UNDERLINE: u8 = 0b0000_1000;
const TEXT_MODE_INVERSE: u8 = 0b0001_0000;
// ratty-vt: SGR 5 (slow blink) and SGR 6 (rapid blink). Mutually exclusive,
// like the intensity bits; SGR 25 clears both.
const TEXT_MODE_BLINK: u8 = 0b0110_0000;
const TEXT_MODE_BLINK_SLOW: u8 = 0b0010_0000;
const TEXT_MODE_BLINK_RAPID: u8 = 0b0100_0000;

/// The blink attribute of a cell or of newly drawn text.
#[derive(Eq, PartialEq, Debug, Copy, Clone, Default)]
pub enum Blink {
    /// Not blinking (SGR 25).
    #[default]
    None,
    /// Slow blink (SGR 5).
    Slow,
    /// Rapid blink (SGR 6).
    Rapid,
}

#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub struct Attrs {
    pub fgcolor: Color,
    pub bgcolor: Color,
    pub mode: u8,
}

impl Attrs {
    pub fn bold(&self) -> bool {
        self.mode & TEXT_MODE_BOLD != 0
    }

    pub fn dim(&self) -> bool {
        self.mode & TEXT_MODE_DIM != 0
    }

    fn intensity(&self) -> u8 {
        self.mode & TEXT_MODE_INTENSITY
    }

    pub fn set_bold(&mut self) {
        self.mode &= !TEXT_MODE_INTENSITY;
        self.mode |= TEXT_MODE_BOLD;
    }

    pub fn set_dim(&mut self) {
        self.mode &= !TEXT_MODE_INTENSITY;
        self.mode |= TEXT_MODE_DIM;
    }

    pub fn set_normal_intensity(&mut self) {
        self.mode &= !TEXT_MODE_INTENSITY;
    }

    pub fn italic(&self) -> bool {
        self.mode & TEXT_MODE_ITALIC != 0
    }

    pub fn set_italic(&mut self, italic: bool) {
        if italic {
            self.mode |= TEXT_MODE_ITALIC;
        } else {
            self.mode &= !TEXT_MODE_ITALIC;
        }
    }

    pub fn underline(&self) -> bool {
        self.mode & TEXT_MODE_UNDERLINE != 0
    }

    pub fn set_underline(&mut self, underline: bool) {
        if underline {
            self.mode |= TEXT_MODE_UNDERLINE;
        } else {
            self.mode &= !TEXT_MODE_UNDERLINE;
        }
    }

    pub fn inverse(&self) -> bool {
        self.mode & TEXT_MODE_INVERSE != 0
    }

    pub fn set_inverse(&mut self, inverse: bool) {
        if inverse {
            self.mode |= TEXT_MODE_INVERSE;
        } else {
            self.mode &= !TEXT_MODE_INVERSE;
        }
    }

    // ratty-vt: blink attribute.
    pub fn blink(&self) -> Blink {
        match self.mode & TEXT_MODE_BLINK {
            TEXT_MODE_BLINK_SLOW => Blink::Slow,
            TEXT_MODE_BLINK_RAPID => Blink::Rapid,
            _ => Blink::None,
        }
    }

    pub fn set_blink(&mut self, blink: Blink) {
        self.mode &= !TEXT_MODE_BLINK;
        self.mode |= match blink {
            Blink::None => 0,
            Blink::Slow => TEXT_MODE_BLINK_SLOW,
            Blink::Rapid => TEXT_MODE_BLINK_RAPID,
        };
    }

    pub fn write_escape_code_diff(&self, contents: &mut Vec<u8>, other: &Self) {
        if self != other && self == &Self::default() {
            crate::ratty_vt::term::ClearAttrs.write_buf(contents);
            return;
        }

        let attrs = crate::ratty_vt::term::Attrs::default();

        let attrs = if self.fgcolor == other.fgcolor {
            attrs
        } else {
            attrs.fgcolor(self.fgcolor)
        };
        let attrs = if self.bgcolor == other.bgcolor {
            attrs
        } else {
            attrs.bgcolor(self.bgcolor)
        };
        let attrs = if self.intensity() == other.intensity() {
            attrs
        } else {
            attrs.intensity(match self.intensity() {
                0 => crate::ratty_vt::term::Intensity::Normal,
                TEXT_MODE_BOLD => crate::ratty_vt::term::Intensity::Bold,
                TEXT_MODE_DIM => crate::ratty_vt::term::Intensity::Dim,
                _ => unreachable!(),
            })
        };
        let attrs = if self.italic() == other.italic() {
            attrs
        } else {
            attrs.italic(self.italic())
        };
        let attrs = if self.underline() == other.underline() {
            attrs
        } else {
            attrs.underline(self.underline())
        };
        let attrs = if self.inverse() == other.inverse() {
            attrs
        } else {
            attrs.inverse(self.inverse())
        };
        // ratty-vt: blink.
        let attrs = if self.blink() == other.blink() {
            attrs
        } else {
            attrs.blink(self.blink())
        };

        attrs.write_buf(contents);
    }
}
