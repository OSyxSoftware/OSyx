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

impl Default for Theme {
    fn default() -> Theme {
        MIDNIGHT
    }
}
