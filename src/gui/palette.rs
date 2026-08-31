//! The Tokyo Night palette as egui colors — the desktop twin of
//! [`crate::ui::palette`], which maps the same [`Sem`] table to ratatui
//! styles. Both read their colors from [`crate::render`], so the TUI and the
//! window can never drift apart.

use eframe::egui::{self, Color32, Stroke};

use crate::render::{self, Rgb, Sem};
use crate::source::{Attn, Liveness};
use crate::ui::palette::glyphs;

const fn color32(rgb: Rgb) -> Color32 {
    Color32::from_rgb(rgb.r, rgb.g, rgb.b)
}

pub const BG: Color32 = color32(render::BG);
pub const CHROME: Color32 = color32(render::CHROME);
pub const BORDER: Color32 = color32(render::BORDER);
pub const SELECTION: Color32 = color32(render::SELECTION);
pub const FG: Color32 = color32(render::FG);
pub const DIM: Color32 = color32(render::DIM);

/// The color a semantic role paints with — the egui half of
/// [`Sem::color`](crate::render::Sem::color).
pub fn color(sem: Sem) -> Color32 {
    color32(sem.color())
}

/// A session's state glyph and its color, from the same glyph table the TUI
/// uses (`HERMON_ASCII` still picks the fallback set).
pub fn glyph_for_liveness(state: Liveness) -> (&'static str, Color32) {
    let glyphs = glyphs();
    match state {
        Liveness::Live => (glyphs.live, color(Sem::Ok)),
        Liveness::Done => (glyphs.done, color(Sem::Dim)),
        Liveness::Attention(Attn::PermWait) => (glyphs.perm_wait, color(Sem::User)),
        Liveness::Attention(Attn::Stuck) => (glyphs.stuck, color(Sem::Error)),
    }
}

/// Installs the dark theme on the context: egui's own dark visuals with
/// every color hermon has an opinion about overridden.
pub fn install(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = BG;
    visuals.window_fill = CHROME;
    visuals.extreme_bg_color = CHROME;
    visuals.faint_bg_color = CHROME;
    visuals.override_text_color = Some(FG);
    visuals.window_stroke = Stroke::new(1.0, BORDER);
    visuals.selection.bg_fill = SELECTION;
    visuals.selection.stroke = Stroke::new(1.0, FG);
    visuals.hyperlink_color = color(Sem::Stat);
    visuals.warn_fg_color = color(Sem::User);
    visuals.error_fg_color = color(Sem::Error);
    ctx.set_visuals(visuals);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sem_colors_match_the_shared_palette() {
        for sem in [Sem::Plain, Sem::Dim, Sem::Error, Sem::User, Sem::Ok] {
            let Rgb { r, g, b } = sem.color();
            assert_eq!(color(sem), Color32::from_rgb(r, g, b), "{sem:?}");
        }
    }

    #[test]
    fn liveness_glyphs_are_colored_by_severity() {
        assert_eq!(glyph_for_liveness(Liveness::Live).1, color(Sem::Ok));
        assert_eq!(glyph_for_liveness(Liveness::Done).1, color(Sem::Dim));
    }
}
