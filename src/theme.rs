//! The one place the app's own chrome takes its sizes, fonts, colours and
//! icon weights from (`ui.rs`, `picker.rs`, `privacy.rs`).
//!
//! Fonts come from AppKit's text styles, so the chrome follows the system
//! (and its text size) instead of eleven ad-hoc point sizes. Spacing is on a
//! 4-point grid. Colours are AppKit's dynamic system colours; anything that
//! has to be copied into a layer is re-applied when the appearance changes
//! (see `ui::reapply_layer_colors`).

use objc2::rc::Retained;
use objc2_app_kit::{NSColor, NSFont, NSFontWeight, NSImage, NSImageSymbolConfiguration, NSImageSymbolScale};

// ---- spacing (4-pt grid) ----------------------------------------------------

pub const GRID: f64 = 4.0;
pub const SIDEBAR_W: f64 = 250.0;
pub const TOOLBAR_H: f64 = 44.0;
pub const TAB_STRIP_H: f64 = 34.0;
pub const FIND_H: f64 = 36.0;
/// The page-load progress line under the toolbar.
pub const PROGRESS_H: f64 = 2.0;
pub const ICON_BUTTON: f64 = 28.0;
pub const TOOLBAR_GAP: f64 = 8.0;
pub const ADDRESS_H: f64 = 28.0;
pub const SIDEBAR_ROW_H: f64 = 44.0;
pub const SIDEBAR_ROW_PAD: f64 = 12.0;
pub const SIDEBAR_BADGE: f64 = 28.0;
pub const BADGE_TEXT_GAP: f64 = 8.0;
/// The selected sidebar row's leading bar in the client's colour.
pub const SELECTION_BAR_W: f64 = 3.0;
pub const TOOLBAR_CHIP_BADGE: f64 = 18.0;
pub const CHIP_RADIUS: f64 = 7.0;
/// Sheets and Settings: labels right-aligned in one column, fields on one
/// leading edge.
pub const LABEL_COL_W: f64 = 96.0;
pub const SHEET_MARGIN: f64 = 20.0;
pub const SECTION_GAP: f64 = 20.0;
pub const FIELD_H: f64 = 24.0;
pub const BUTTON_W: f64 = 88.0;
pub const BUTTON_H: f64 = 28.0;
/// Error and empty states: secondary text never wider than this.
pub const MESSAGE_MAX_W: f64 = 460.0;

// ---- type -------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Text {
    /// The launcher heading.
    LargeTitle,
    /// Error and empty-state headings.
    Title2,
    /// Sheet and Settings headings.
    Title3,
    /// Sidebar client names, section titles.
    Headline,
    /// Address bar, fields, ordinary text.
    Body,
    /// Tab titles, the toolbar's client chip.
    Callout,
    /// Sidebar subtitles, hints, captions.
    Footnote,
    /// Section labels such as CLIENTS.
    Caption,
}

fn style(t: Text) -> &'static objc2_app_kit::NSFontTextStyle {
    use objc2_app_kit::*;
    // SAFETY: AppKit's text style names are immutable constants.
    unsafe {
        match t {
            Text::LargeTitle => NSFontTextStyleLargeTitle,
            Text::Title2 => NSFontTextStyleTitle2,
            Text::Title3 => NSFontTextStyleTitle3,
            Text::Headline => NSFontTextStyleHeadline,
            Text::Body => NSFontTextStyleBody,
            Text::Callout => NSFontTextStyleCallout,
            Text::Footnote => NSFontTextStyleFootnote,
            Text::Caption => NSFontTextStyleCaption1,
        }
    }
}

/// The system font for a text style.
pub fn font(t: Text) -> Retained<NSFont> {
    let options = objc2_foundation::NSDictionary::new();
    unsafe { NSFont::preferredFontForTextStyle_options(style(t), &options) }
}

pub fn size(t: Text) -> f64 {
    font(t).pointSize()
}

/// A text style's size at another weight (tab titles are all medium, so
/// selecting one never changes its width).
pub fn font_weight(t: Text, weight: Weight) -> Retained<NSFont> {
    NSFont::systemFontOfSize_weight(size(t), weight.ns())
}

/// Counts, percentages and the downloads badge: digits all the same width,
/// so numbers do not jitter as they change.
pub fn digits(t: Text) -> Retained<NSFont> {
    NSFont::monospacedDigitSystemFontOfSize_weight(size(t), Weight::Regular.ns())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Weight {
    Regular,
    Medium,
    Semibold,
    Bold,
}

impl Weight {
    fn ns(self) -> NSFontWeight {
        // SAFETY: AppKit's weight constants.
        unsafe {
            match self {
                Weight::Regular => objc2_app_kit::NSFontWeightRegular,
                Weight::Medium => objc2_app_kit::NSFontWeightMedium,
                Weight::Semibold => objc2_app_kit::NSFontWeightSemibold,
                Weight::Bold => objc2_app_kit::NSFontWeightBold,
            }
        }
    }
}

/// Tracking for small-caps section labels.
pub const CAPTION_TRACKING: f64 = 0.5;

// ---- icons ------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Icon {
    /// Toolbar buttons: 15 pt, medium.
    Toolbar,
    /// Inside tabs: 12 pt, medium.
    Tab,
    /// Error and empty states.
    Hero,
}

fn icon_points(i: Icon) -> f64 {
    match i {
        Icon::Toolbar => 15.0,
        Icon::Tab => 12.0,
        Icon::Hero => 40.0,
    }
}

/// An SF Symbol at one of the app's icon sizes, always the same weight.
pub fn symbol(name: &str, icon: Icon) -> Option<Retained<NSImage>> {
    thread_local! {
        static CACHE: std::cell::RefCell<std::collections::HashMap<(String, Icon), Option<Retained<NSImage>>>> = std::cell::RefCell::new(std::collections::HashMap::new());
    }
    CACHE.with_borrow_mut(|c| {
        c.entry((name.to_string(), icon))
            .or_insert_with(|| {
                let base = NSImage::imageWithSystemSymbolName_accessibilityDescription(&crate::util::ns(name), None)?;
                let cfg = NSImageSymbolConfiguration::configurationWithPointSize_weight_scale(icon_points(icon), Weight::Medium.ns(), NSImageSymbolScale::Medium);
                base.imageWithSymbolConfiguration(&cfg)
            })
            .clone()
    })
}

// ---- colour -----------------------------------------------------------------

/// The Search workspace's neutral blue (never a client's colour).
pub const SEARCH_BLUE: &str = "#4285f4";

/// How strongly the active client's colour tints the tab strip.
pub fn client_tint_alpha(dark: bool) -> f64 {
    if dark { 0.10 } else { 0.07 }
}

/// Is this appearance a dark one?
pub fn is_dark(appearance: &objc2_app_kit::NSAppearance) -> bool {
    use objc2_app_kit::{NSAppearanceNameAqua, NSAppearanceNameDarkAqua};
    let names = unsafe { objc2_foundation::NSArray::from_slice(&[NSAppearanceNameAqua, NSAppearanceNameDarkAqua]) };
    appearance.bestMatchFromAppearancesWithNames(&names).is_some_and(|n| unsafe { &*n == NSAppearanceNameDarkAqua })
}

/// The active client's colour as a faint wash (7 % in light mode, 10 % in
/// dark), resolved for whichever appearance it is drawn in, so the tab strip
/// stays right after the Mac switches between light and dark.
pub fn client_tint(hex: &str) -> Retained<NSColor> {
    let base = crate::util::color(hex);
    let light = base.colorWithAlphaComponent(client_tint_alpha(false));
    let dark = base.colorWithAlphaComponent(client_tint_alpha(true));
    let provider = block2::RcBlock::new(move |a: std::ptr::NonNull<objc2_app_kit::NSAppearance>| {
        let chosen = if is_dark(unsafe { a.as_ref() }) { &dark } else { &light };
        std::ptr::NonNull::from(&**chosen)
    });
    unsafe { NSColor::colorWithName_dynamicProvider(None, &provider) }
}

pub fn secondary_text() -> Retained<NSColor> {
    NSColor::secondaryLabelColor()
}

pub fn separator() -> Retained<NSColor> {
    NSColor::separatorColor()
}

pub fn window_background() -> Retained<NSColor> {
    NSColor::windowBackgroundColor()
}

/// The selected tab's fill.
pub fn selected_tab_fill() -> Retained<NSColor> {
    NSColor::controlAccentColor().colorWithAlphaComponent(0.16)
}

/// A tab or icon button under the mouse.
pub fn hover_fill() -> Retained<NSColor> {
    NSColor::labelColor().colorWithAlphaComponent(0.06)
}

/// The toolbar's client chip.
pub fn chip_fill() -> Retained<NSColor> {
    NSColor::quaternaryLabelColor().colorWithAlphaComponent(0.4)
}

pub fn destructive() -> Retained<NSColor> {
    NSColor::systemRedColor()
}

/// Near-black for initials on light badge colours (Apple's label black).
pub const DARK_GLYPH: &str = "#1d1d1f";

/// WCAG relative luminance of an sRGB colour.
pub fn luminance((r, g, b): (f64, f64, f64)) -> f64 {
    let lin = |c: f64| if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

pub fn contrast(a: (f64, f64, f64), b: (f64, f64, f64)) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// The text colour for initials or a symbol on a badge of `hex`: white, or
/// near-black when white would fall short of 4.5:1 (WCAG AA for small
/// text) — whichever reads better.
pub fn glyph_on(hex: &str) -> &'static str {
    let bg = crate::model::hex_to_rgb(hex);
    let white = contrast(bg, (1.0, 1.0, 1.0));
    let dark = contrast(bg, crate::model::hex_to_rgb(DARK_GLYPH));
    if white >= 4.5 || white >= dark { "#ffffff" } else { DARK_GLYPH }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn badge_text_always_meets_aa_contrast_or_the_best_available() {
        for (name, hex) in crate::model::PROFILE_COLORS {
            let bg = crate::model::hex_to_rgb(hex);
            let fg = crate::model::hex_to_rgb(glyph_on(hex));
            let ratio = contrast(bg, fg);
            assert!(ratio >= 4.5, "{name} {hex}: {ratio:.2}:1");
        }
        assert_eq!(glyph_on("#ffffff"), DARK_GLYPH, "a white custom colour gets dark text");
        assert_eq!(glyph_on("#000000"), "#ffffff");
        assert_eq!(glyph_on("#f2a516"), DARK_GLYPH, "amber was 2.07:1 with white");
    }

    #[test]
    fn contrast_matches_wcag_reference_values() {
        assert!((contrast((1.0, 1.0, 1.0), (0.0, 0.0, 0.0)) - 21.0).abs() < 0.01);
        assert!((contrast((1.0, 1.0, 1.0), (1.0, 1.0, 1.0)) - 1.0).abs() < 0.001);
    }

    #[test]
    fn spacing_is_on_the_four_point_grid() {
        for v in [TOOLBAR_H, ICON_BUTTON, TOOLBAR_GAP, ADDRESS_H, SIDEBAR_ROW_H, SIDEBAR_ROW_PAD, BADGE_TEXT_GAP, LABEL_COL_W, SECTION_GAP, SIDEBAR_BADGE] {
            assert_eq!(v % GRID, 0.0, "{v}");
        }
    }
}
