//! The OSyx look for PRISM's own drawing (decorations, shadows, cursor,
//! fallback background): midnight blue and charcoal with a restrained blue
//! accent. The UI toolkit uses the same palette for client content.

use osyx_gfx::{rgb, Color};

pub const DESKTOP_FALLBACK: Color = rgb(13, 18, 30);
pub const TITLE_ACTIVE: Color = rgb(28, 34, 48);
pub const TITLE_INACTIVE: Color = rgb(22, 27, 38);
pub const WINDOW_BODY: Color = rgb(18, 22, 31);
pub const TITLE_TEXT: Color = rgb(228, 233, 242);
pub const TITLE_TEXT_INACTIVE: Color = rgb(128, 138, 158);
pub const BUTTON_HOVER: Color = rgb(46, 55, 76);
pub const BUTTON_PRESSED: Color = rgb(60, 72, 99);
pub const CLOSE_HOVER: Color = rgb(214, 66, 76);
pub const GLYPH: Color = rgb(186, 195, 212);
pub const ACCENT: Color = rgb(76, 141, 255);
pub const OUTLINE_INACTIVE: Color = rgb(255, 255, 255);

/// The colours PRISM draws with. The constants above are the dark set;
/// [`Compositor::set_palette`](crate::Compositor::set_palette) switches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub desktop_fallback: Color,
    pub title_active: Color,
    pub title_inactive: Color,
    pub window_body: Color,
    pub title_text: Color,
    pub title_text_inactive: Color,
    pub button_hover: Color,
    pub button_pressed: Color,
    pub close_hover: Color,
    pub glyph: Color,
    pub accent: Color,
    pub outline_inactive: Color,
}

impl Palette {
    pub const DARK: Palette = Palette {
        desktop_fallback: DESKTOP_FALLBACK,
        title_active: TITLE_ACTIVE,
        title_inactive: TITLE_INACTIVE,
        window_body: WINDOW_BODY,
        title_text: TITLE_TEXT,
        title_text_inactive: TITLE_TEXT_INACTIVE,
        button_hover: BUTTON_HOVER,
        button_pressed: BUTTON_PRESSED,
        close_hover: CLOSE_HOVER,
        glyph: GLYPH,
        accent: ACCENT,
        outline_inactive: OUTLINE_INACTIVE,
    };

    /// The light set, matching the toolkit's light theme.
    pub const LIGHT: Palette = Palette {
        desktop_fallback: rgb(206, 214, 230),
        title_active: rgb(228, 232, 240),
        title_inactive: rgb(238, 240, 245),
        window_body: rgb(246, 247, 250),
        title_text: rgb(26, 30, 40),
        title_text_inactive: rgb(120, 128, 146),
        button_hover: rgb(208, 214, 226),
        button_pressed: rgb(190, 198, 214),
        close_hover: rgb(214, 66, 76),
        glyph: rgb(70, 78, 96),
        accent: rgb(40, 110, 235),
        outline_inactive: rgb(0, 0, 0),
    };
}

/// Height of the title bar of decorated windows.
pub const TITLE_HEIGHT: u32 = 34;
/// Corner radius of window frames.
pub const RADIUS: u32 = 10;
/// Reach of the drop shadow beyond the frame.
pub const SHADOW: u32 = 22;
/// Vertical offset of the shadow (light from above).
pub const SHADOW_OFFSET_Y: i32 = 6;
pub const SHADOW_ALPHA_ACTIVE: u32 = 120;
pub const SHADOW_ALPHA_INACTIVE: u32 = 70;
/// Decoration buttons: size and gap, right-aligned in the title bar.
pub const BUTTON: u32 = 24;
pub const BUTTON_GAP: u32 = 6;
pub const BUTTON_MARGIN: u32 = 8;
/// Width of the invisible resize border around frames.
pub const RESIZE_BORDER: u32 = 6;

/// Durations of window animations (milliseconds).
pub const FADE_IN_MS: u64 = 160;
pub const FADE_OUT_MS: u64 = 120;
