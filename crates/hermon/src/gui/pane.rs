//! A live session's pane: the streaming transcript, its follow-scroll state
//! machine, and the chrome around both — the desktop twin of
//! [`crate::ui::pane`].
//!
//! Three decisions shape this module.
//!
//! Text is drawn as one [`LayoutJob`] per display line, each [`Seg`] a styled
//! section in the shared [`Sem`] color. Nothing about a renderer's output
//! changes on the way here.
//!
//! Wrapping is [`crate::ui::pane::wrap_styled`]'s, not the layout job's. The
//! pane is monospaced, so breaking on character columns is exact, and it buys
//! two things a job-wrapped pane cannot have: display lines of uniform
//! height, which is what lets [`egui::ScrollArea::show_rows`] lay out only
//! the ~40 rows on screen instead of all 5000 in the buffer, and breaks in
//! the same places the TUI puts them. Measured on a 5000-line buffer, that is
//! ~0.3 ms a frame against ~2 ms for laying every line out. The wrap is
//! cached per pane and recomputed only when the buffer changes or the pane
//! is resized.
//!
//! Follow-scroll is tracked here rather than left to `stick_to_bottom`.
//! egui's stickiness is private state that only re-arms when the offset lands
//! exactly on the end, so a "jump to latest" button has nothing to press.
//! Instead [`PaneView`] remembers the bottom offset each frame and drives the
//! scroll area to it while following; a wheel or drag moves the offset off
//! the bottom, [`Follow::observe`] sees that and pauses, and the badge counts
//! what arrives meanwhile. The transitions are plain data, so #77's `g`/`G`
//! keys are two method calls rather than a rewrite.

use std::collections::VecDeque;

use eframe::egui::text::LayoutJob;
use eframe::egui::{
    self, Align, Color32, FontId, Layout, Rect, RichText, Sense, Stroke, TextFormat, TextStyle, Ui,
    Vec2,
};

use crate::render::{Sem, StyledLine};
use crate::source::Liveness;
use crate::ui::pane::{attention_status, wrap_styled};

use super::palette;

/// Frames a jump to the tail holds the pane there regardless of where the
/// scroll area ended up. egui keeps applying a wheel's momentum for a few
/// frames after the wheel itself stopped, and that leftover must not read as
/// the user immediately scrolling away again.
const SETTLE_FRAMES: u8 = 4;

/// A session's pane: everything the widget needs about it that is not scroll
/// state. Borrowed straight from the app's roster row and buffer.
pub struct Pane<'a> {
    pub key: &'a str,
    pub model: &'a str,
    pub state: Liveness,
    pub selected: bool,
    pub pinned: bool,
    /// Seconds in the current attention state, for the inline status line.
    pub attn_elapsed: Option<f64>,
    /// The session's transcript buffer, oldest first.
    pub lines: &'a VecDeque<StyledLine>,
}

/// Whether a pane is following its tail, and how much it has missed since it
/// stopped. The pure core of the scroll behaviour: every transition is a
/// method, so the draw code only reports what the user did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Follow {
    pub following: bool,
    /// Lines that arrived since following stopped — the `▼ N new` badge.
    pub unseen: usize,
}

impl Default for Follow {
    fn default() -> Self {
        Follow {
            following: true,
            unseen: 0,
        }
    }
}

impl Follow {
    /// `n` lines appended to the buffer. A followed pane shows them, so only
    /// a paused one has anything to count.
    pub fn appended(&mut self, n: usize) {
        if !self.following {
            self.unseen = self.unseen.saturating_add(n);
        }
    }

    /// Where the scroll area ended up this frame. Scrolling away pauses the
    /// follow; scrolling back to the bottom resumes it, exactly as clicking
    /// the badge does.
    pub fn observe(&mut self, at_bottom: bool) {
        match (self.following, at_bottom) {
            (true, false) => self.following = false,
            (false, true) => *self = Follow::default(),
            _ => {}
        }
    }

    /// "Jump to latest" — the badge click, and #77's `G`.
    pub fn resume(&mut self) {
        *self = Follow::default();
    }
}

/// A pane's scroll state across frames: its follow flag, the wrapped
/// transcript it is showing, and the geometry needed to drive the scroll area
/// to the bottom without asking egui where the bottom is.
#[derive(Debug, Default)]
pub struct PaneView {
    pub follow: Follow,
    /// The buffer wrapped to `cols`, plus the attention status line.
    wrapped: Vec<StyledLine>,
    cols: usize,
    /// Set when the buffer changed under us, so the wrap is recomputed once
    /// rather than every frame.
    dirty: bool,
    /// Last frame's bottom offset and row count, which together give this
    /// frame's bottom offset: the content grows by exactly one row height per
    /// new row.
    bottom: Option<f32>,
    rows: usize,
    /// An offset to jump to next frame, from `g`/`G` or the badge.
    pending: Option<f32>,
    /// Frames left of [`SETTLE_FRAMES`] after a jump to the tail.
    settling: u8,
}

impl PaneView {
    /// The buffer changed: re-wrap before the next draw.
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    /// Scrolls to the top and stops following, the desktop's `g`.
    pub fn jump_top(&mut self) {
        self.follow.following = false;
        self.pending = Some(0.0);
    }

    /// Back to the tail, the desktop's `G` and what the badge does. No
    /// explicit offset: following recomputes the bottom every frame, and the
    /// one measured here is already stale by the rows that arrived since.
    pub fn jump_latest(&mut self) {
        self.follow.resume();
        self.pending = None;
        self.settling = SETTLE_FRAMES;
    }

    /// The display lines for a `cols`-wide pane, re-wrapping only when the
    /// buffer or the width changed.
    fn reflow(&mut self, pane: &Pane, cols: usize) -> &[StyledLine] {
        if self.dirty || self.cols != cols {
            let mut wrapped = wrap_styled(pane.lines, cols);
            if let Some(status) = attention_status(pane.state, pane.attn_elapsed) {
                wrapped.extend(wrap_styled(&VecDeque::from([status]), cols));
            }
            self.wrapped = wrapped;
            self.cols = cols;
            self.dirty = false;
        }
        &self.wrapped
    }
}

/// What the user did to a pane this frame, for the caller to act on: a click
/// selects the session, a double click zooms it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PaneAction {
    pub clicked: bool,
    pub double_clicked: bool,
}

/// Draws the pane — border, title, transcript and badge — into the space
/// `ui` has left, and reports what the user did to it.
pub fn render(ui: &mut Ui, pane: &Pane, view: &mut PaneView) -> PaneAction {
    // Claimed before the contents are drawn, not after: egui gives a
    // contested click to the last widget registered, so the badge inside has
    // to come after this or it would never be clickable.
    let response = ui.interact(
        ui.available_rect_before_wrap(),
        ui.id().with(("pane", pane.key)),
        Sense::click(),
    );

    let stroke = Stroke::new(1.0, border_color(pane));
    egui::Frame::NONE
        .stroke(stroke)
        .inner_margin(6.0)
        .corner_radius(4.0)
        .show(ui, |ui| {
            title_bar(ui, pane);
            transcript(ui, pane, view);
        });

    PaneAction {
        clicked: response.clicked(),
        double_clicked: response.double_clicked(),
    }
}

/// `✓ C:0f865f — sonnet-4.5`: the state glyph a finished session carries in
/// the roster too, its key, and the model it is running.
fn title_bar(ui: &mut Ui, pane: &Pane) {
    let color = border_color(pane);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if pane.state == Liveness::Done {
            let (glyph, glyph_color) = palette::glyph_for_liveness(pane.state);
            ui.label(RichText::new(glyph).color(glyph_color).monospace());
        }
        ui.label(RichText::new(pane.key).color(color).monospace().strong());
        ui.label(
            RichText::new(format!("\u{2014} {}", pane.model))
                .color(palette::DIM)
                .monospace(),
        );
    });
}

/// The scrolling transcript, plus the `▼ N new` badge when it is paused.
fn transcript(ui: &mut Ui, pane: &Pane, view: &mut PaneView) {
    // Transcript rows sit flush, like a terminal's. This also keeps the row
    // pitch a single number: `show_rows` reserves `row_height` per row and
    // the rows drawn inside take exactly that.
    ui.spacing_mut().item_spacing.y = 0.0;
    let font = TextStyle::Monospace.resolve(ui.style());
    let (row_height, col_width) =
        ui.fonts_mut(|fonts| (fonts.row_height(&font), fonts.glyph_width(&font, ' ')));
    // The scrollbar eats into the text width; wrapping past it would leave a
    // column of clipped characters.
    let width = ui.available_width() - ui.spacing().scroll.bar_width;
    let cols = (width / col_width).floor().max(1.0) as usize;

    let settling = view.settling > 0;
    view.settling = view.settling.saturating_sub(1);
    let rows = view.reflow(pane, cols).len();
    let offset = view.pending.take().or_else(|| {
        // Following: the bottom of the content, which is last frame's bottom
        // plus a row per row that has arrived since.
        view.follow.following.then_some(())?;
        let grown = rows.saturating_sub(view.rows) as f32;
        Some(view.bottom? + grown * row_height)
    });

    let mut area = egui::ScrollArea::vertical()
        .id_salt(("pane-scroll", pane.key))
        .auto_shrink([false, false])
        // Only load-bearing before the first frame has measured the pane:
        // after that `offset` above drives the tail explicitly.
        .stick_to_bottom(view.follow.following);
    if let Some(offset) = offset {
        area = area.vertical_scroll_offset(offset);
    }

    let out = area.show_rows(ui, row_height, rows, |ui, range| {
        for line in &view.wrapped[range] {
            ui.label(job(line, &font));
        }
    });

    let bottom = (out.content_size.y - out.inner_rect.height()).max(0.0);
    if settling {
        // Still shaking off the wheel that paused it; the tail is where the
        // jump asked to be, whatever the offset says this frame.
        ui.ctx().request_repaint();
    } else {
        // Within a line of the bottom is the bottom: egui eases the offset
        // into place over several frames, and that must not read as the user
        // scrolling away. A wheel tick moves several rows, so a real scroll
        // still registers on the frame it happens.
        view.follow
            .observe(out.state.offset.y >= bottom - row_height);
    }
    view.bottom = Some(bottom);
    view.rows = rows;

    if !view.follow.following && badge(ui, out.inner_rect, view.follow.unseen) {
        view.jump_latest();
    }
}

/// The floating jump-to-latest button over the bottom-right of the
/// transcript; `true` when it was clicked. It paints over the last line
/// rather than taking a row of its own, as the TUI's `▼ N more` does, and it
/// is the whole affordance for resuming, so a paused pane with nothing new
/// still gets one.
fn badge(ui: &mut Ui, area: Rect, unseen: usize) -> bool {
    let label = match unseen {
        0 => "\u{25bc} latest".to_string(),
        n => format!("\u{25bc} {n} new"),
    };
    let button = egui::Button::new(RichText::new(label).color(palette::BG).monospace())
        .fill(palette::color(Sem::Stat))
        .corner_radius(3.0);
    let size = Vec2::new(90.0, 20.0);
    let rect = Rect::from_min_size(area.max - size - Vec2::new(4.0, 4.0), size);
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        ui.with_layout(Layout::right_to_left(Align::BOTTOM), |ui| {
            ui.add(button).clicked()
        })
        .inner
    })
    .inner
}

/// One display line as a layout job: a section per segment in its semantic
/// color, monospaced, and never re-wrapped — [`wrap_styled`] already broke it
/// to the pane's width.
fn job(line: &StyledLine, font: &FontId) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    for seg in &line.0 {
        job.append(
            &seg.text,
            0.0,
            TextFormat {
                font_id: font.clone(),
                color: palette::color(seg.sem),
                ..TextFormat::default()
            },
        );
    }
    job
}

/// The border and title tint, mirroring [`crate::ui::pane::border_style`]:
/// cyan for the selected pane, then the session's own state — amber waiting
/// on you, red stuck, dim finished, plain chrome while it works — with a
/// pinned finished pane staying amber rather than going dim.
fn border_color(pane: &Pane) -> Color32 {
    if pane.selected {
        return palette::color(Sem::Stat);
    }
    match pane.state {
        Liveness::Live => palette::BORDER,
        Liveness::Attention(crate::source::Attn::PermWait) => palette::color(Sem::User),
        Liveness::Attention(crate::source::Attn::Stuck) => palette::color(Sem::Error),
        Liveness::Done if pane.pinned => palette::color(Sem::User),
        Liveness::Done => palette::color(Sem::Dim),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::Seg;
    use crate::source::Attn;

    fn buffer(n: usize) -> VecDeque<StyledLine> {
        (0..n)
            .map(|i| {
                StyledLine(vec![
                    Seg::new(Sem::Tool, format!("Bash{i:04} ")),
                    Seg::new(Sem::Dim, "ls -la /some/long/path --with-flags"),
                ])
            })
            .collect()
    }

    fn pane(lines: &VecDeque<StyledLine>) -> Pane<'_> {
        Pane {
            key: "C:aaaaaa",
            model: "sonnet-4.5",
            state: Liveness::Live,
            selected: false,
            pinned: false,
            attn_elapsed: None,
            lines,
        }
    }

    /// One egui pass with no window behind it, sized like a real pane.
    fn pass(ctx: &egui::Context, mut body: impl FnMut(&mut Ui)) -> usize {
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.set_max_size(Vec2::new(600.0, 400.0));
            body(ui);
        });
        out.textures_delta.clear();
        out.shapes.len()
    }

    #[test]
    fn a_fresh_pane_follows_its_tail_with_no_badge() {
        let follow = Follow::default();
        assert!(follow.following);
        assert_eq!(follow.unseen, 0);
    }

    /// The whole state machine the seam note asks for: scrolling away pauses
    /// the follow, lines arriving while paused feed the badge, and either
    /// scrolling back or the badge itself resumes and clears it.
    #[test]
    fn scrolling_away_pauses_the_follow_and_counts_what_arrives() {
        let mut follow = Follow::default();
        follow.appended(5);
        assert_eq!(follow.unseen, 0, "a followed pane misses nothing");

        follow.observe(false);
        assert!(!follow.following);
        follow.appended(3);
        follow.appended(2);
        assert_eq!(follow.unseen, 5);

        // Still away: more lines keep counting, the pane stays paused.
        follow.observe(false);
        assert!(!follow.following);
        assert_eq!(follow.unseen, 5);

        follow.resume();
        assert_eq!(follow, Follow::default());
    }

    #[test]
    fn scrolling_back_to_the_bottom_resumes_the_follow() {
        let mut follow = Follow::default();
        follow.observe(false);
        follow.appended(7);
        follow.observe(true);
        assert_eq!(follow, Follow::default());
    }

    #[test]
    fn a_re_wrap_only_happens_when_the_buffer_or_width_changes() {
        let lines = buffer(3);
        let filled = pane(&lines);
        let empty = VecDeque::new();
        let emptied = pane(&empty);
        let mut view = PaneView {
            dirty: true,
            ..PaneView::default()
        };
        let wrapped = view.reflow(&filled, 20).len();
        assert!(wrapped > 3, "40-odd columns of text must wrap to {wrapped}");

        // Same width, no invalidation: the cache answers, buffer or not.
        assert_eq!(view.reflow(&emptied, 20).len(), wrapped);
        // A narrower pane re-wraps, and so does a buffer that changed.
        assert_ne!(view.reflow(&filled, 10).len(), wrapped);
        view.invalidate();
        assert_eq!(view.reflow(&emptied, 10).len(), 0);
    }

    /// The attention status line rides at the end of the transcript, wrapped
    /// with it, exactly as the TUI appends it.
    #[test]
    fn an_attention_pane_ends_with_its_status_line() {
        let lines = buffer(1);
        let waiting = Pane {
            state: Liveness::Attention(Attn::PermWait),
            attn_elapsed: Some(45.0),
            ..pane(&lines)
        };
        let mut view = PaneView {
            dirty: true,
            ..PaneView::default()
        };
        let text: String = view
            .reflow(&waiting, 80)
            .iter()
            .map(|l| l.to_plain())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("waiting on permission prompt \u{b7} 45s"),
            "{text}"
        );
    }

    #[test]
    fn a_live_pane_has_no_status_line() {
        let lines = buffer(1);
        let mut view = PaneView {
            dirty: true,
            ..PaneView::default()
        };
        let text: String = view
            .reflow(&pane(&lines), 80)
            .iter()
            .map(|l| l.to_plain())
            .collect();
        assert!(!text.contains("waiting on"), "{text}");
    }

    /// The performance claim, headless: a full 5000-line buffer must not put
    /// 5000 lines' worth of shapes in the frame. Only the rows the viewport
    /// can show are laid out at all.
    #[test]
    fn a_full_buffer_only_lays_out_the_visible_rows() {
        let ctx = egui::Context::default();
        let lines = buffer(crate::ui::pane::SCROLLBACK);
        let mut view = PaneView {
            dirty: true,
            ..PaneView::default()
        };
        // First pass measures, second scrolls to the tail.
        let shapes = {
            pass(&ctx, |ui| {
                render(ui, &pane(&lines), &mut view);
            });
            pass(&ctx, |ui| {
                render(ui, &pane(&lines), &mut view);
            })
        };
        assert!(
            view.wrapped.len() >= crate::ui::pane::SCROLLBACK,
            "the whole buffer is wrapped: {}",
            view.wrapped.len()
        );
        assert!(
            shapes < 500,
            "a 400pt viewport must not paint {shapes} shapes"
        );
        assert!(view.follow.following, "a fresh pane follows its tail");
    }

    /// The follow state machine against egui's real scroll handling, with no
    /// window: a wheel-up over the pane pauses the follow and the lines that
    /// arrive next feed the badge, then jumping to the latest puts the pane
    /// back on its tail.
    #[test]
    fn a_wheel_scroll_pauses_the_follow_and_a_jump_resumes_it() {
        let ctx = egui::Context::default();
        let mut lines = buffer(400);
        let mut view = PaneView {
            dirty: true,
            ..PaneView::default()
        };
        fn frame(
            ctx: &egui::Context,
            input: egui::RawInput,
            lines: &VecDeque<StyledLine>,
            view: &mut PaneView,
        ) {
            let mut out = ctx.run_ui(input, |ui| {
                ui.set_max_size(Vec2::new(600.0, 400.0));
                render(ui, &pane(lines), view);
            });
            out.textures_delta.clear();
        }

        // Two settling frames: the first measures the pane, the second parks
        // it on the tail.
        frame(&ctx, egui::RawInput::default(), &lines, &mut view);
        frame(&ctx, egui::RawInput::default(), &lines, &mut view);
        assert!(view.follow.following, "a fresh pane follows");

        let wheel = egui::RawInput {
            events: vec![
                egui::Event::PointerMoved(egui::pos2(300.0, 200.0)),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: Vec2::new(0.0, 120.0),
                    phase: egui::TouchPhase::Move,
                    modifiers: egui::Modifiers::default(),
                },
            ],
            ..egui::RawInput::default()
        };
        frame(&ctx, wheel, &lines, &mut view);
        assert!(!view.follow.following, "scrolling up pauses the follow");

        lines.push_back(StyledLine::default());
        view.invalidate();
        view.follow.appended(1);
        frame(&ctx, egui::RawInput::default(), &lines, &mut view);
        assert_eq!(view.follow.unseen, 1, "the badge counts what it hides");
        assert!(!view.follow.following, "and the pane stays where it was");

        // Jumping while the wheel's momentum is still decaying still lands
        // on the tail and stays there.
        view.jump_latest();
        for _ in 0..6 {
            frame(&ctx, egui::RawInput::default(), &lines, &mut view);
        }
        assert_eq!(view.follow, Follow::default(), "the jump lands on the tail");
    }

    /// Clicking the badge resumes the follow — and the pane-wide click
    /// target, which covers the badge, must not swallow the click.
    #[test]
    fn clicking_the_badge_resumes_the_follow() {
        let ctx = egui::Context::default();
        let lines = buffer(400);
        let mut view = PaneView {
            follow: Follow {
                following: false,
                unseen: 9,
            },
            dirty: true,
            ..PaneView::default()
        };
        let mut action = PaneAction::default();
        let frame = |input: egui::RawInput, view: &mut PaneView, action: &mut PaneAction| {
            let mut out = ctx.run_ui(input, |ui| {
                ui.set_max_size(Vec2::new(600.0, 400.0));
                *action = render(ui, &pane(&lines), view);
            });
            out.textures_delta.clear();
        };
        frame(egui::RawInput::default(), &mut view, &mut action);

        // The badge sits in the bottom-right corner of the transcript, inside
        // the pane's own frame margin and clear of the scrollbar.
        let at = egui::pos2(600.0 - 6.0 - 4.0 - 45.0, 400.0 - 6.0 - 4.0 - 10.0);
        let click = |pressed| egui::RawInput {
            events: vec![
                egui::Event::PointerMoved(at),
                egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::default(),
                },
            ],
            ..egui::RawInput::default()
        };
        frame(click(true), &mut view, &mut action);
        frame(click(false), &mut view, &mut action);

        assert!(
            view.follow.following,
            "the badge puts the pane back on its tail"
        );
        assert_eq!(view.follow.unseen, 0);
    }

    /// Every pane state lays out without panicking, at sane and silly sizes.
    #[test]
    fn every_state_draws_at_any_size() {
        let ctx = egui::Context::default();
        let lines = buffer(30);
        let empty = VecDeque::new();
        for state in [
            Liveness::Live,
            Liveness::Done,
            Liveness::Attention(Attn::PermWait),
            Liveness::Attention(Attn::Stuck),
        ] {
            for (w, h) in [(600.0, 400.0), (120.0, 40.0), (1.0, 1.0)] {
                let mut view = PaneView {
                    dirty: true,
                    follow: Follow {
                        following: false,
                        unseen: 12,
                    },
                    ..PaneView::default()
                };
                let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                    ui.set_max_size(Vec2::new(w, h));
                    let pane = Pane {
                        state,
                        selected: true,
                        pinned: true,
                        attn_elapsed: Some(9.0),
                        ..pane(if w > 100.0 { &lines } else { &empty })
                    };
                    render(ui, &pane, &mut view);
                });
                out.textures_delta.clear();
            }
        }
    }

    #[test]
    fn the_border_takes_the_session_state_unless_the_pane_is_selected() {
        let empty = VecDeque::new();
        let base = pane(&empty);
        assert_eq!(
            border_color(&Pane {
                selected: true,
                state: Liveness::Done,
                ..base
            }),
            palette::color(Sem::Stat)
        );
        assert_eq!(border_color(&base), palette::BORDER);
        assert_eq!(
            border_color(&Pane {
                state: Liveness::Attention(Attn::Stuck),
                ..base
            }),
            palette::color(Sem::Error)
        );
        assert_eq!(
            border_color(&Pane {
                state: Liveness::Done,
                ..base
            }),
            palette::color(Sem::Dim)
        );
        assert_eq!(
            border_color(&Pane {
                state: Liveness::Done,
                pinned: true,
                ..base
            }),
            palette::color(Sem::User)
        );
    }

    /// Colors come from the shared [`Sem`] table, so a pane and the TUI paint
    /// the same event the same way.
    #[test]
    fn a_layout_job_carries_each_segments_semantic_color() {
        let line = StyledLine(vec![
            Seg::new(Sem::Tool, "Bash"),
            Seg::new(Sem::Dim, " ls -la"),
        ]);
        let job = job(&line, &FontId::monospace(12.0));
        assert_eq!(job.sections.len(), 2);
        assert_eq!(job.sections[0].format.color, palette::color(Sem::Tool));
        assert_eq!(job.sections[1].format.color, palette::color(Sem::Dim));
        assert_eq!(job.text, "Bash ls -la");
    }
}
