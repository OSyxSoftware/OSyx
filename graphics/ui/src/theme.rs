//! The OSyx look: midnight blue and charcoal surfaces, a restrained blue
//! accent, soft rounded corners. PRISM draws its decorations with the same
//! palette (`prism_core::theme`), so client content and frames match.

use osyx_gfx::{rgb, Color};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    /// Window content background (matches PRISM's window body).
    pub window: Color,
    /// Cards, side bars, tool bars.
    pub surface: Color,
    /// Rows hovered or raised controls.
    pub raised: Color,
    pub border: Color,
    pub text: Color,
    pub text_dim: Color,
    pub text_faint: Color,
    pub accent: Color,
    pub accent_hover: Color,
    pub accent_pressed: Color,
    /// Text on accent-coloured controls.
    pub on_accent: Color,
    pub danger: Color,
    pub danger_hover: Color,
    pub success: Color,
    pub warning: Color,
    /// Selected list rows.
    pub selection: Color,
    pub field: Color,
    pub field_border: Color,
    pub scrollbar: Color,
    pub scrollbar_hover: Color,
    /// Modal dimming of the content behind a dialog.
    pub scrim: Color,
    pub radius: u32,
    pub radius_small: u32,
    pub spacing: u32,
    /// Height of buttons, fields and list rows.
    pub control_height: u32,
}

pub const MIDNIGHT: Theme = Theme {
    window: rgb(18, 22, 31),
    surface: rgb(23, 28, 39),
    raised: rgb(32, 39, 54),
    border: rgb(43, 51, 68),
    text: rgb(226, 231, 240),
    text_dim: rgb(150, 160, 178),
    text_faint: rgb(98, 108, 128),
    accent: rgb(76, 141, 255),
    accent_hover: rgb(99, 157, 255),
    accent_pressed: rgb(58, 118, 226),
    on_accent: rgb(255, 255, 255),
    danger: rgb(214, 66, 76),
    danger_hover: rgb(230, 88, 98),
    success: rgb(96, 214, 150),
    warning: rgb(240, 186, 90),
    selection: rgb(36, 60, 108),
    field: rgb(13, 16, 23),
    field_border: rgb(48, 57, 76),
    scrollbar: rgb(56, 66, 86),
    scrollbar_hover: rgb(82, 95, 122),
    scrim: rgb(5, 7, 11),
    radius: 10,
    radius_small: 6,
    spacing: 12,
    control_height: 32,
};

/// The light look: paper-white surfaces, dark text, the same accent.
pub const DAYLIGHT: Theme = Theme {
    window: rgb(246, 247, 250),
    surface: rgb(236, 238, 243),
    raised: rgb(224, 228, 236),
    border: rgb(204, 209, 220),
    text: rgb(26, 30, 40),
    text_dim: rgb(84, 92, 110),
    text_faint: rgb(134, 142, 160),
    accent: rgb(40, 110, 235),
    accent_hover: rgb(62, 128, 245),
    accent_pressed: rgb(28, 92, 208),
    on_accent: rgb(255, 255, 255),
    danger: rgb(204, 52, 64),
    danger_hover: rgb(222, 74, 86),
    success: rgb(30, 142, 84),
    warning: rgb(176, 116, 12),
    selection: rgb(196, 216, 250),
    field: rgb(255, 255, 255),
    field_border: rgb(190, 197, 212),
    scrollbar: rgb(190, 196, 210),
    scrollbar_hover: rgb(160, 168, 186),
    scrim: rgb(60, 66, 80),
    radius: 10,
    radius_small: 6,
    spacing: 12,
    control_height: 32,
};

/// Dark or light.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    Dark,
    Light,
}

/// What the user chose (`/etc/osyx/theme`): a scheme, or the one the sun
/// decides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchemeChoice {
    Dark,
    Light,
    /// Light between sunrise and sunset.
    Automatic,
}

impl SchemeChoice {
    /// From the configuration file's text; anything unknown is dark, the
    /// look OSyx had before there was a choice.
    pub fn parse(text: &str) -> SchemeChoice {
        match text.trim() {
            "light" => SchemeChoice::Light,
            "auto" => SchemeChoice::Automatic,
            _ => SchemeChoice::Dark,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            SchemeChoice::Dark => "dark",
            SchemeChoice::Light => "light",
            SchemeChoice::Automatic => "auto",
        }
    }

    /// The scheme to show at Unix time `unix` (UTC) at the given place.
    pub fn at(self, unix: i64, latitude: f64, longitude: f64) -> Scheme {
        match self {
            SchemeChoice::Dark => Scheme::Dark,
            SchemeChoice::Light => Scheme::Light,
            SchemeChoice::Automatic => {
                if crate::time::sun_is_up(unix, latitude, longitude) {
                    Scheme::Light
                } else {
                    Scheme::Dark
                }
            }
        }
    }
}

impl Theme {
    pub const fn of(scheme: Scheme) -> Theme {
        match scheme {
            Scheme::Dark => MIDNIGHT,
            Scheme::Light => DAYLIGHT,
        }
    }

    /// Whether this is a light theme (dark text on a light window).
    pub fn is_light(&self) -> bool {
        let lum = |c: Color| ((c >> 16) & 255) * 3 + ((c >> 8) & 255) * 6 + (c & 255);
        lum(self.window) > lum(self.text)
    }
}

impl Default for Theme {
    fn default() -> Theme {
        MIDNIGHT
    }
}
