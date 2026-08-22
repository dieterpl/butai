//! The USAGE page: which agent account stops you first, and when it comes back.
//!
//! **Limits, not spend.** Cost never appears here. The question is whether the
//! account you are about to start a long job on has room, which is a different
//! question from what the month has cost and is answered by different numbers.
//!
//! **Every CLI is on screen at once.** The obvious build is the DOCKER shape —
//! a list on the left, the selected one on the right — and it is wrong for this
//! page: the question is *which* account is closest to stopping you, and a
//! list-and-detail answers it with the other rows hidden behind a cursor.
//! Stacking every CLI costs about twenty rows and removes the navigation.
//!
//! **A window with no ceiling draws a total, not a bar.** A bar needs a
//! denominator, and any denominator this page invented would be read as the
//! provider's. So one appears only where a ceiling actually came from
//! somewhere: a limit the CLI published (`claude` caches its own, and those
//! windows arrive as percentages with a real reset instant) or a budget the
//! user declared in `[[budgets]]`. A CLI that publishes nothing still arrives
//! with `of: null`, and its windows draw as totals.
//!
//! **The right-hand column answers "when does it come back".** A limit is two
//! numbers — how full, and how long until it empties — and the second is the
//! one that decides whether to start a long job now or after lunch. The
//! percentage is already on the bar, so the tail spends itself on the reset
//! wherever the window has a real boundary. A rolling window has none, and
//! spends the tail on its total instead.
//!
//! **Colour is a level, not a verdict.** [`super::Role::Ok`] — green — is
//! deliberately unused: 12% of a quota is not a success, and painting it green
//! spends the loudest signal in the palette on the row nobody needs to look at.
//! Amber means this bites during the session you are in; red means it is about
//! to stop you. The same three roles carry the bar, the rail badge and the
//! footer notice, so a tightening limit changes colour everywhere at once.

use butai_protocol::api::{CliState, CliUsageDto, UsageDto, UsageUnit, UsageWindowDto};
use ratatui::buffer::Buffer;

use super::{draw_box, ellipsize, put_str, Geom, LRect, Pen, Role, Theme};

/// The page's own state: what the daemon last said, and where the cursor is.
///
/// Its own struct rather than fields on [`View`], for the reason
/// [`super::Docker`] and [`super::Git`] have theirs — it is about one page.
#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub dto: UsageDto,
    /// Which CLI the cursor is on. Rows are per CLI, not per window: the CLI is
    /// the thing you have an account with.
    pub sel: usize,
    /// Whether the page has read `/v1/usage` since it was entered.
    pub loaded: bool,
}

impl Usage {
    pub fn move_sel(&mut self, delta: isize) {
        let len = self.dto.clis.len();
        if len == 0 {
            self.sel = 0;
            return;
        }
        self.sel = (self.sel as isize + delta).clamp(0, len as isize - 1) as usize;
    }
}

/// Width of the label column, sized to the longest window label the daemon
/// writes (`week · all models`, 17) plus room to breathe.
const LABEL_W: u16 = 19;
/// Width of a budget bar. Only drawn where a ceiling was declared.
const BAR_W: u16 = 24;
/// How wide the page lets its content grow.
///
/// The numbers are right-aligned against this rather than against the box, so
/// a 200-column terminal does not strand `39.1M tokens` two feet from the label
/// it belongs to. Every page here that is a list of values rather than a
/// document does the same thing; a full-width row is for prose.
const CONTENT_W: u16 = 78;

/// How tightly a window is drawn: a level, never a verdict. See the module
/// note on why green is missing.
pub fn pressure(used: u64, of: u64) -> Role {
    if of == 0 {
        return Role::Info;
    }
    let pct = used.saturating_mul(100) / of;
    match pct {
        p if p >= 90 => Role::Danger,
        p if p >= 75 => Role::Attention,
        _ => Role::Info,
    }
}

/// `4394242` -> `4.4M`. Token counts are read as magnitudes, and nine digits of
/// precision on a number whose denominator is unknown is false authority.
pub fn compact(n: u64) -> String {
    match n {
        n if n >= 100_000_000 => format!("{}M", n / 1_000_000),
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1_000_000.0),
        n if n >= 100_000 => format!("{}k", n / 1_000),
        n if n >= 1_000 => format!("{:.1}k", n as f64 / 1_000.0),
        n => n.to_string(),
    }
}

/// The value column for one window — `4.4M tokens`, or `4.4M / 20.0M tokens`
/// once a budget gives it a denominator.
///
/// A percentage is its own denominator: `56%`, never `56 / 100 %`.
pub fn value_text(w: &UsageWindowDto) -> String {
    let unit = match w.unit {
        UsageUnit::Tokens => "tokens",
        UsageUnit::Requests => "requests",
        UsageUnit::Percent => return format!("{}%", w.used),
    };
    match w.of {
        Some(of) => format!("{} / {} {unit}", compact(w.used), compact(of)),
        None => format!("{} {unit}", compact(w.used)),
    }
}

/// How long until a window empties — `4d 6h`, `3h 12m`, `8m`.
///
/// Two units at most: the hour matters when the reset is today and stops
/// mattering when it is Tuesday, and a countdown to the second on a five-hour
/// window is a number that changes while you read it.
pub fn until(reset_ms: u64, now_ms: u64) -> String {
    let secs = reset_ms.saturating_sub(now_ms) / 1000;
    match secs {
        s if s < 60 => "now".into(),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h {}m", s / 3600, (s % 3600) / 60),
        s => format!("{}d {}h", s / 86_400, (s % 86_400) / 3600),
    }
}

/// The right-hand column for one window: when it comes back, or what it holds.
///
/// See the module note — the reset wins the slot wherever there is one, because
/// the bar has already said how full the window is.
pub fn tail_text(w: &UsageWindowDto, now_ms: u64) -> String {
    match (w.resets_ms, w.unit) {
        (Some(r), _) => format!("resets in {}", until(r, now_ms)),
        // A percentage with no boundary has said everything it has to say on
        // the bar; repeating it out here would be furniture.
        (None, UsageUnit::Percent) => String::new(),
        (None, _) => value_text(w),
    }
}

/// The one number worth carrying onto the pages that are *not* this one.
///
/// Only a window with a ceiling produces one — published by the provider or
/// declared by the user. Without one there is no proportion, and "39.0M" on a
/// fourteen-column rail is a number nobody can act on. So a machine whose CLIs
/// publish nothing and which has declared nothing shows no badge, which is
/// correct: there is no threshold to have crossed.
pub fn badge(dto: &UsageDto) -> Option<(String, Role)> {
    let worst = dto
        .clis
        .iter()
        .flat_map(|c| &c.windows)
        .filter_map(|w| w.of.map(|of| (w.used.saturating_mul(100) / of.max(1), of, w.used)))
        .max_by_key(|(pct, _, _)| *pct)?;
    let (pct, of, used) = worst;
    Some((format!("{}%", pct.min(999)), pressure(used, of)))
}

/// How long ago the daemon sampled, for the header's right-hand summary. A
/// stale limit is worse than no limit, so the age is always drawn.
pub fn age(sampled_ms: u64, now_ms: u64) -> String {
    if sampled_ms == 0 {
        return "never".into();
    }
    let secs = now_ms.saturating_sub(sampled_ms) / 1000;
    match secs {
        s if s < 60 => format!("{s}s ago"),
        s if s < 3600 => format!("{}m ago", s / 60),
        s => format!("{}h ago", s / 3600),
    }
}

/// One CLI's block, as rows of (indent, text, role) the page paints.
///
/// Split out from the painting so the shape can be asserted without a buffer:
/// what makes this page right or wrong is which rows exist for which state, not
/// where the pixels land.
#[derive(Debug, PartialEq)]
pub enum Row<'a> {
    /// The name line: name, the account behind it, and a right-hand summary.
    Head {
        cli: &'a CliUsageDto,
    },
    /// A window, with a bar only when it has a ceiling.
    Window {
        w: &'a UsageWindowDto,
    },
    /// The provenance line under a CLI's windows.
    Note(&'a str),
    Blank,
}

/// Every row the page would draw, in order.
pub fn rows(dto: &UsageDto) -> Vec<Row<'_>> {
    let mut out = Vec::new();
    for cli in &dto.clis {
        out.push(Row::Head { cli });
        for w in &cli.windows {
            out.push(Row::Window { w });
        }
        // A note under the windows explains where they came from. For the
        // states that have no windows the note *is* the row, and is drawn on
        // the head line beside the name instead — see `draw_head`.
        if !cli.windows.is_empty() {
            if let Some(note) = &cli.note {
                out.push(Row::Note(note));
            }
        }
        out.push(Row::Blank);
    }
    out.pop(); // no trailing blank
    out
}

/// The row a CLI's block starts at, for scrolling to the cursor.
fn head_rows(dto: &UsageDto) -> Vec<usize> {
    rows(dto)
        .iter()
        .enumerate()
        .filter_map(|(i, r)| matches!(r, Row::Head { .. }).then_some(i))
        .collect()
}

pub fn draw(buf: &mut Buffer, geom: &Geom, u: Option<&Usage>, theme: &Theme) {
    let area = geom.stage_box;
    draw_box(buf, area, " USAGE ", theme.border(true), theme.ground);
    let inner = LRect::new(
        area.x + 1,
        area.y + 1,
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    );
    let bound = inner.x + inner.width;
    let Some(state) = u else { return };

    if state.dto.clis.is_empty() {
        let text =
            if state.loaded { "no agent CLIs configured" } else { "reading account standing…" };
        put_str(buf, inner.x + 1, inner.y, text, bound, Pen::new(theme.faint, theme.ground));
        return;
    }

    // The verb line sits on the last interior row, as every rail's does.
    let list_h = inner.height.saturating_sub(1);
    put_str(
        buf,
        inner.x,
        inner.y + list_h,
        "r refresh   j/k move",
        bound,
        Pen::new(theme.faint, theme.ground),
    );

    // One clock reading for the whole frame, so every countdown on screen is
    // relative to the same instant.
    let now = super::now_ms();
    let sampled = age(state.dto.sampled_ms, now);
    let all = rows(&state.dto);
    let heads = head_rows(&state.dto);
    let sel = state.sel.min(heads.len().saturating_sub(1));
    let first = scroll_to(heads.get(sel).copied().unwrap_or(0), all.len(), list_h as usize);

    for (i, row) in all.iter().enumerate().skip(first) {
        let y = inner.y + (i - first) as u16;
        if y >= inner.y + list_h {
            break;
        }
        let cursor = heads.get(sel) == Some(&i);
        match row {
            Row::Head { cli } => draw_head(buf, inner, y, cli, cursor, &sampled, theme),
            Row::Window { w } => draw_window(buf, inner, y, w, now, theme),
            Row::Note(note) => {
                let x = inner.x + 3 + LABEL_W;
                put_str(
                    buf,
                    x,
                    y,
                    &ellipsize(note, bound.saturating_sub(x) as usize),
                    bound,
                    Pen::new(theme.faint, theme.ground),
                );
            }
            Row::Blank => {}
        }
    }
}

/// The column the page's numbers right-align against.
fn content_edge(inner: LRect) -> u16 {
    inner.x + inner.width.min(CONTENT_W)
}

/// Keep the cursor's block on screen without jumping the list around it.
fn scroll_to(target: usize, len: usize, height: usize) -> usize {
    if len <= height || height == 0 {
        return 0;
    }
    // Show the cursor's block with what follows it, which is what a reader
    // wants: the windows *under* the name are the answer, not the names above.
    target.min(len.saturating_sub(height))
}

fn draw_head(
    buf: &mut Buffer,
    inner: LRect,
    y: u16,
    cli: &CliUsageDto,
    cursor: bool,
    // How old the daemon's sample is, rendered once by the caller: it is the
    // same string on every head row, and the clock must not move mid-frame.
    sampled: &str,
    theme: &Theme,
) {
    let bound = inner.x + inner.width;
    let edge = content_edge(inner);
    let bg = theme.row_bg(cursor);
    for x in inner.x..bound {
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_symbol(" ");
            cell.set_bg(bg);
        }
    }
    put_str(
        buf,
        inner.x + 1,
        y,
        &cli.name,
        bound,
        Pen { fg: if cursor { theme.ink } else { theme.muted }, bg, bold: cursor },
    );

    // States with no windows carry their explanation here, beside the name:
    // "not installed", "no account limits". A row that says nothing to show and
    // why is what stops someone hunting for a number that does not exist.
    // Measured before the detail is placed, so the detail's room is whatever is
    // actually left rather than a fixed reservation — a constant here
    // ellipsized a 52-character account line on a 150-column terminal with 60
    // columns to spare.
    let summary = match cli.state {
        CliState::Metered | CliState::Counted => {
            let agents = match cli.panes.len() {
                0 => String::new(),
                1 => "1 agent · ".into(),
                n => format!("{n} agents · "),
            };
            format!("{agents}{sampled}")
        }
        _ => String::new(),
    };

    let detail = match cli.state {
        CliState::Metered | CliState::Counted => {
            let mut parts: Vec<&str> = Vec::new();
            if let Some(p) = &cli.plan {
                parts.push(p);
            }
            if let Some(a) = &cli.account {
                parts.push(a);
            }
            if let Some(v) = &cli.version {
                parts.push(v);
            }
            parts.join(" · ")
        }
        _ => cli.note.clone().unwrap_or_default(),
    };
    let dx = inner.x + 10;
    let sx = edge.saturating_sub(summary.chars().count() as u16);
    // A CLI with nothing to summarise gives its whole line to the note.
    let room =
        if summary.is_empty() { bound.saturating_sub(dx) } else { sx.saturating_sub(dx + 1) };
    put_str(buf, dx, y, &ellipsize(&detail, room as usize), bound, Pen::new(theme.faint, bg));

    // Right-hand summary: who is burning this account right now, and how old
    // the numbers are. Only for the states that have numbers.
    if !summary.is_empty() {
        put_str(buf, sx, y, &summary, bound, Pen::new(theme.faint, bg));
    }
}

fn draw_window(
    buf: &mut Buffer,
    inner: LRect,
    y: u16,
    w: &UsageWindowDto,
    now_ms: u64,
    theme: &Theme,
) {
    let bound = inner.x + inner.width;
    let edge = content_edge(inner);
    put_str(buf, inner.x + 3, y, &w.label, bound, Pen::new(theme.muted, theme.ground));

    let tail = tail_text(w, now_ms);
    let vx = edge.saturating_sub(tail.chars().count() as u16);
    let role = match w.of {
        Some(of) => pressure(w.used, of),
        // No ceiling: the number is a fact, not a level, and is drawn as one.
        None => Role::Ink,
    };
    // The reset is a fact about the clock, not a pressure level — a window that
    // is 95% full is still red on the bar, but *when* it comes back is not an
    // alarm and is drawn faint so the bar keeps the eye.
    let tail_pen = match w.resets_ms {
        Some(_) => Pen::new(theme.faint, theme.ground),
        None => Pen::new(theme.role(role), theme.ground),
    };
    put_str(buf, vx, y, &tail, bound, tail_pen);

    // The bar exists only where a budget gave the window a denominator.
    let Some(of) = w.of else { return };
    let filled =
        w.used.saturating_mul(BAR_W as u64).checked_div(of).unwrap_or(0).min(BAR_W as u64) as u16;
    let bx = inner.x + 3 + LABEL_W;
    let fg = theme.role(pressure(w.used, of));
    put_str(buf, bx, y, &"▇".repeat(filled as usize), bound, Pen::new(fg, theme.ground));
    put_str(
        buf,
        bx + filled,
        y,
        &"▁".repeat(BAR_W.saturating_sub(filled) as usize),
        bound,
        Pen::new(theme.faint, theme.ground),
    );
    let pct = format!("{}%", (w.used.saturating_mul(100) / of.max(1)).min(999));
    put_str(buf, bx + BAR_W + 2, y, &pct, bound, Pen::new(fg, theme.ground));
}

#[cfg(test)]
mod tests {
    use super::*;
    use butai_protocol::api::UsageSource;

    fn win(label: &str, used: u64, of: Option<u64>) -> UsageWindowDto {
        UsageWindowDto { label: label.into(), used, of, unit: UsageUnit::Tokens, resets_ms: None }
    }

    fn cli(name: &str, state: CliState, windows: Vec<UsageWindowDto>) -> CliUsageDto {
        CliUsageDto {
            name: name.into(),
            command: name.into(),
            state,
            version: Some("1.0".into()),
            account: Some("you@example.com".into()),
            plan: Some("max 5x".into()),
            windows,
            panes: Vec::new(),
            source: UsageSource::Transcripts,
            note: Some("counted from transcripts".into()),
        }
    }

    /// A published window: a percentage against 100, with a real boundary.
    fn pub_win(label: &str, used: u64, resets_ms: Option<u64>) -> UsageWindowDto {
        UsageWindowDto {
            label: label.into(),
            used,
            of: Some(100),
            unit: UsageUnit::Percent,
            resets_ms,
        }
    }

    #[test]
    fn a_window_with_no_ceiling_is_a_total_not_a_proportion() {
        assert_eq!(value_text(&win("last 5h", 4_394_242, None)), "4.4M tokens");
        assert_eq!(value_text(&win("last 5h", 4_394_242, Some(20_000_000))), "4.4M / 20.0M tokens");
    }

    #[test]
    fn a_percentage_is_its_own_denominator() {
        assert_eq!(
            value_text(&pub_win("session", 56, None)),
            "56%",
            "`56 / 100 %` says the same thing twice and reads as a bug"
        );
    }

    #[test]
    fn a_reset_counts_down_in_two_units() {
        let now = 1_000_000_000;
        assert_eq!(until(now + 30_000, now), "now", "under a minute is not worth a number");
        assert_eq!(until(now + 8 * 60_000, now), "8m");
        assert_eq!(until(now + (3 * 3600 + 12 * 60) * 1000, now), "3h 12m");
        assert_eq!(until(now + (4 * 86_400 + 6 * 3600) * 1000, now), "4d 6h");
        assert_eq!(until(now - 5_000, now), "now", "a boundary already past never goes negative");
    }

    #[test]
    fn the_tail_spends_itself_on_the_reset_when_there_is_one() {
        let now = 1_000_000_000;
        assert_eq!(
            tail_text(&pub_win("session", 42, Some(now + 2 * 3600 * 1000)), now),
            "resets in 2h 0m",
            "the bar already said 42%; the tail says when it comes back"
        );
        assert_eq!(
            tail_text(&pub_win("session", 0, None), now),
            "",
            "a percentage with no boundary would only repeat the bar"
        );
        assert_eq!(
            tail_text(&win("last 5h", 4_394_242, None), now),
            "4.4M tokens",
            "a rolling window has no boundary and spends the tail on its total"
        );
    }

    #[test]
    fn a_published_limit_earns_a_badge_without_any_declared_budget() {
        let dto = UsageDto {
            clis: vec![cli(
                "claude",
                CliState::Metered,
                vec![pub_win("session", 12, None), pub_win("week · all models", 56, None)],
            )],
            sampled_ms: 1,
        };
        let (text, role) = badge(&dto).expect("the provider published a ceiling");
        assert_eq!(text, "56%", "the tightest window is the one that follows you");
        assert_eq!(role, Role::Info);
    }

    #[test]
    fn compact_reads_as_a_magnitude() {
        assert_eq!(compact(0), "0");
        assert_eq!(compact(999), "999");
        assert_eq!(compact(1_500), "1.5k");
        assert_eq!(compact(150_000), "150k");
        assert_eq!(compact(4_394_242), "4.4M");
        assert_eq!(compact(150_000_000), "150M");
    }

    #[test]
    fn pressure_is_a_level_and_never_green() {
        assert_eq!(pressure(10, 100), Role::Info);
        assert_eq!(pressure(80, 100), Role::Attention);
        assert_eq!(pressure(95, 100), Role::Danger);
        assert_eq!(pressure(200, 100), Role::Danger, "over the ceiling stays at the top role");
        // The one that would be green anywhere else on this workbench.
        assert_ne!(pressure(1, 100), Role::Ok);
    }

    #[test]
    fn the_badge_needs_a_declared_ceiling() {
        let counted = UsageDto {
            clis: vec![cli("claude", CliState::Counted, vec![win("last 5h", 9, None)])],
            sampled_ms: 1,
        };
        assert_eq!(badge(&counted), None, "a total is not a threshold to have crossed");

        let metered = UsageDto {
            clis: vec![cli(
                "claude",
                CliState::Metered,
                vec![win("last 5h", 50, Some(100)), win("last 7d", 92, Some(100))],
            )],
            sampled_ms: 1,
        };
        let (text, role) = badge(&metered).expect("a declared budget gives a percentage");
        assert_eq!(text, "92%", "the tightest window is the one that follows you");
        assert_eq!(role, Role::Danger);
    }

    #[test]
    fn every_cli_gets_a_block_and_only_windowed_ones_get_a_note() {
        let dto = UsageDto {
            clis: vec![
                cli("claude", CliState::Counted, vec![win("last 5h", 1, None)]),
                cli("codex", CliState::Absent, vec![]),
            ],
            sampled_ms: 1,
        };
        let r = rows(&dto);
        assert!(matches!(r[0], Row::Head { .. }));
        assert!(matches!(r[1], Row::Window { .. }));
        assert!(matches!(r[2], Row::Note(_)), "provenance sits under the windows");
        assert!(matches!(r[3], Row::Blank));
        assert!(matches!(r[4], Row::Head { .. }));
        assert_eq!(r.len(), 5, "an absent CLI is one row, and there is no trailing blank");
    }

    #[test]
    fn the_cursor_walks_clis_not_windows() {
        let dto = UsageDto {
            clis: vec![
                cli("claude", CliState::Counted, vec![win("a", 1, None), win("b", 2, None)]),
                cli("codex", CliState::Absent, vec![]),
            ],
            sampled_ms: 1,
        };
        let mut u = Usage { dto, sel: 0, loaded: true };
        u.move_sel(1);
        assert_eq!(u.sel, 1, "one press moves past the whole first block");
        u.move_sel(1);
        assert_eq!(u.sel, 1, "and stops at the end rather than wrapping");
        u.move_sel(-5);
        assert_eq!(u.sel, 0);
    }

    /// The USAGE page as painted rows of text, through the real `draw` path.
    ///
    /// The helper tests above check the strings; this checks that they reach
    /// the screen — the page draws cell by cell, so a row that reads correctly
    /// here is the only evidence the layout put it somewhere visible.
    fn screen(state: &Usage) -> Vec<String> {
        use ratatui::layout::Rect;

        use super::super::{Page, Scene, View};
        use butai_protocol::api::SysDto;

        const COLS: u16 = 120;
        const ROWS: u16 = 40;
        let mut buf = Buffer::empty(Rect::new(0, 0, COLS, ROWS));
        let sys = SysDto::default();
        let view = View { page: Page::Usage, ..Default::default() };
        let scene = Scene { usage: Some(state), ..Scene::new(&[], &sys) };
        super::super::draw(&mut buf, COLS, ROWS, &scene, &view, &Theme::default());
        (0..ROWS)
            .map(|y| {
                (0..COLS)
                    .filter_map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()))
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn a_published_window_puts_its_reset_on_screen() {
        let now = super::super::now_ms();
        let dto = UsageDto {
            clis: vec![cli(
                "claude",
                CliState::Metered,
                vec![
                    // Half a minute of slack: `until` floors, and `draw` reads
                    // the clock again, so a reset landing exactly on the minute
                    // would render as 2h 14m about half the time.
                    pub_win("session", 42, Some(now + (2 * 3600 + 15 * 60 + 30) * 1000)),
                    pub_win("week · all models", 91, None),
                ],
            )],
            sampled_ms: now,
        };
        let lines = screen(&Usage { dto, sel: 0, loaded: true });
        let session = lines.iter().find(|l| l.contains("session")).expect("the session row drew");
        assert!(session.contains("42%"), "the bar's percentage: {session:?}");
        assert!(session.contains("resets in 2h 15m"), "and when it comes back: {session:?}");

        // The window with no boundary keeps its bar and spends no tail on a
        // number the bar already carries.
        let week = lines.iter().find(|l| l.contains("all models")).expect("the week row drew");
        assert!(week.contains("91%"), "{week:?}");
        assert!(!week.contains("resets"), "nothing to count down to: {week:?}");
    }

    /// One CLI's windows can arrive from two sources at once: the daemon keeps
    /// the published windows a stale cache still speaks for and counts the rest
    /// from transcripts. The page has no idea that happened — it draws each
    /// window from its own `unit` and `of` — and this is the proof that being
    /// unaware is enough.
    #[test]
    fn a_block_whose_windows_came_from_two_sources_draws_both_kinds_of_row() {
        let now = super::super::now_ms();
        let dto = UsageDto {
            clis: vec![cli(
                "claude",
                CliState::Metered,
                vec![
                    pub_win("week · all models", 63, Some(now + (4 * 86_400 + 3600) * 1000)),
                    win("last 5h", 1_292_722, None),
                    win("last 7d", 42_449_248, None),
                ],
            )],
            sampled_ms: now,
        };
        let lines = screen(&Usage { dto, sel: 0, loaded: true });

        let week = lines.iter().find(|l| l.contains("all models")).expect("the published row drew");
        assert!(week.contains("63%"), "the provider's number keeps its bar: {week:?}");
        assert!(week.contains("resets in 4d"), "and its real boundary: {week:?}");

        let five = lines.iter().find(|l| l.contains("last 5h")).expect("the counted row drew");
        assert!(five.contains("1.3M tokens"), "a counted window draws its total: {five:?}");
        assert!(
            !five.contains('%'),
            "and no bar, because nothing published a ceiling for it: {five:?}"
        );
    }

    #[test]
    fn ages_read_as_a_glance() {
        assert_eq!(age(0, 10_000), "never");
        assert_eq!(age(1_000, 41_000), "40s ago");
        assert_eq!(age(1_000, 121_000), "2m ago");
        assert_eq!(age(1_000, 7_201_000), "2h ago");
    }
}
