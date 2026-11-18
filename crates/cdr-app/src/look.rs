//! The app's look: its colors in light and dark, and its icons. The colors
//! follow the design (docs: the CDR Mac app design canvas); the platform
//! switches between light and dark with the system.

use nsh_core::element::{AdaptiveColor, ImageSrc, Svg, svg};
use nsh_core::prelude::*;

pub mod color {
    use super::AdaptiveColor;

    const fn c(light: u32, dark: u32) -> AdaptiveColor {
        AdaptiveColor::rgb(light, dark)
    }

    pub const WINDOW: AdaptiveColor = c(0xFFFFFF, 0x1E1E1E);
    /// The footer.
    pub const BAR: AdaptiveColor = c(0xF6F6F6, 0x252525);
    pub const TEXT: AdaptiveColor = c(0x1D1D1F, 0xF2F2F2);
    pub const MUTED: AdaptiveColor = c(0x6E6E73, 0xA1A1A6);
    /// Tracks not read yet.
    pub const FAINT: AdaptiveColor = c(0xAEAEB2, 0x6C6C70);
    pub const BORDER: AdaptiveColor = c(0xE5E5E5, 0x323232);
    pub const BORDER_STRONG: AdaptiveColor = c(0xD2D2D7, 0x48484A);
    pub const BUTTON: AdaptiveColor = c(0xFFFFFF, 0x3A3A3C);
    pub const FIELD: AdaptiveColor = c(0xFFFFFF, 0x2C2C2E);
    /// Every other track row.
    pub const STRIPE: AdaptiveColor = c(0xF7F7F7, 0x242424);
    /// The row being read, and the chosen release.
    pub const CURRENT: AdaptiveColor = c(0xEFEFFB, 0x2A2940);
    /// A progress bar's empty part.
    pub const TRACK: AdaptiveColor = c(0xE8E8ED, 0x38383A);
    pub const ACCENT: AdaptiveColor = c(0x5856D6, 0x6E6AF0);
    /// Text and icons on the accent and on verdict colors.
    pub const ON_COLOR: AdaptiveColor = c(0xFFFFFF, 0xFFFFFF);
    pub const OK: AdaptiveColor = c(0x248A3D, 0x32D74B);
    pub const OK_BG: AdaptiveColor = c(0xEEF8F0, 0x1D2A20);
    pub const OK_BORDER: AdaptiveColor = c(0xC5E4CC, 0x245032);
    pub const WARN: AdaptiveColor = c(0xB25E00, 0xFF9F0A);
    pub const WARN_BG: AdaptiveColor = c(0xFFF6EA, 0x2A2318);
    pub const WARN_BORDER: AdaptiveColor = c(0xEED5B4, 0x5C4320);
    pub const BAD: AdaptiveColor = c(0xD70015, 0xFF453A);
    pub const BAD_BG: AdaptiveColor = c(0xFDEEEE, 0x2E1C1B);
    pub const BAD_BORDER: AdaptiveColor = c(0xF2C2C4, 0x5E2A27);
    pub const TRANSPARENT: AdaptiveColor = AdaptiveColor::TRANSPARENT;
}

/// SVG icons drawn in the element's text color.
pub mod icons {
    pub const DISC: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 104 104" fill="none" stroke="currentColor"><circle cx="52" cy="52" r="48" stroke-width="2" opacity="0.5"/><circle cx="52" cy="52" r="34" stroke-width="1.5" stroke-dasharray="3 5" opacity="0.5"/><circle cx="52" cy="52" r="12" stroke-width="2"/><circle cx="52" cy="52" r="3" fill="currentColor" stroke="none"/></svg>"#;
    pub const FOLDER: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linejoin="round"><path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/></svg>"#;
    pub const ALERT: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round"><circle cx="12" cy="12" r="9" stroke-width="2"/><path d="M12 7.5v5.5"/><path d="M12 16.5v.5"/></svg>"#;
    pub const CHECK: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" stroke-linejoin="round"><path d="M5 12.5l4.5 4.5L19 7.5"/></svg>"#;
    pub const PENCIL: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linejoin="round"><path d="M4 20h4L19 9l-4-4L4 16z"/></svg>"#;
    /// A disc, at toolbar size.
    pub const DISC_SMALL: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"><circle cx="12" cy="12" r="9"/><circle cx="12" cy="12" r="2.5"/></svg>"#;
    pub const SERVER: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M4 5h16v6H4zM4 13h16v6H4z"/><path d="M8 8h.01M8 16h.01" stroke-width="2.4"/></svg>"#;
    pub const CHEVRON: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" stroke-linejoin="round"><path d="M6 9l6 6 6-6"/></svg>"#;
}

/// Icon `markup` at `size` points square.
pub fn icon(markup: &'static str, size: f32) -> Svg {
    svg(ImageSrc::svg_markup(markup))
        .size(px(size))
        .flex_shrink_0()
}

/// An icon whose markup changes.
pub fn icon_with(markup: impl Fn() -> &'static str + 'static, size: f32) -> Svg {
    svg(move || ImageSrc::svg_markup(markup()))
        .size(px(size))
        .flex_shrink_0()
}
