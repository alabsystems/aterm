// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A transient, auto-dismissing in-window banner that surfaces config-load/reload
//! NOTICES — chiefly dropped `[key_sequences]`/`[keybindings]` rules (bad chord/escape/
//! empty/oversized value/unknown action), plus restart-only edits that a live reload
//! can't apply (`columns`/`lines`; see `app_config::restart_notices`). stderr is
//! invisible to a Finder-launched .app, so a fat-fingered or restart-only config edit
//! would otherwise take no effect with zero feedback.
//!
//! Pure + clock-injected (mirrors `aterm-predict`): the lifecycle (TTL/expiry) and the row
//! builder take an explicit `now`/inputs so they unit-test without a window. The banner
//! is drawn by `App::splice_config_notice` (app_render.rs), which OVERWRITES the top
//! rows in place — no geometry change — exactly like the settings panel.

use std::time::{Duration, Instant};

use aterm_core::terminal::RenderCell;
use aterm_render::Theme;

use crate::chrome_band;
use crate::settings::{blank_row, write_str};

/// How long a config-warning banner stays up before auto-dismissing.
pub(crate) const NOTICE_TTL: Duration = Duration::from_secs(8);

/// Cap the banner height so a config with many bad rules can't paper over the screen.
const MAX_NOTICE_ROWS: usize = 6;

/// A transient in-window banner listing config-load/reload warnings. GLOBAL (config is
/// window-uniform); painted into every window's frame; auto-expires after [`NOTICE_TTL`].
pub(crate) struct ConfigNotice {
    pub(crate) lines: Vec<String>,
    until: Instant,
}

impl ConfigNotice {
    /// Build from collected warnings; `None` when there are none (so no banner).
    pub(crate) fn new(lines: Vec<String>, now: Instant) -> Option<Self> {
        if lines.is_empty() {
            None
        } else {
            Some(Self {
                lines,
                until: now + NOTICE_TTL,
            })
        }
    }

    /// The instant the banner auto-dismisses (folded into the event-loop wait deadline).
    pub(crate) fn deadline(&self) -> Instant {
        self.until
    }

    pub(crate) fn is_expired(&self, now: Instant) -> bool {
        now >= self.until
    }

    /// Rows to paint: a title row + one row per DISPLAY row, capped at
    /// [`MAX_NOTICE_ROWS`].
    ///
    /// Counted in display rows rather than notices because a notice is one PROBLEM,
    /// not one LINE — see [`notice_display_rows`]. Asking for one row per notice is
    /// what let a five-line parse error be handed a single row to be crushed into.
    pub(crate) fn wanted_rows(&self) -> usize {
        (display_row_count(&self.lines) + 1).min(MAX_NOTICE_ROWS)
    }

    /// Fold more notices into a LIVE banner and restart its clock, skipping any
    /// line already listed. Merging rather than replacing is the point: the
    /// deferred lane below can arrive while the startup banner is still up, and
    /// `self.config_notice = ConfigNotice::new(..)` would silently delete the
    /// user's config warnings to show one late chrome notice. The TTL restarts so
    /// the newly-added line gets its full read time — an 8 s banner that vanishes
    /// in 200 ms because it inherited an old deadline is the same as no banner.
    /// De-duplicating keeps a lane that re-queues (a live reload re-declining the
    /// same material) from stacking the same sentence until it overflows.
    pub(crate) fn extend(&mut self, lines: impl IntoIterator<Item = String>, now: Instant) {
        let before = self.lines.len();
        for line in lines {
            if !self.lines.contains(&line) {
                self.lines.push(line);
            }
        }
        if self.lines.len() != before {
            self.until = now + NOTICE_TTL;
        }
    }
}

/// THE DEFERRED NOTICE LANE. Not every honest explanation is discovered somewhere
/// that can reach `App` — the client-backdrop declines are decided on the backend
/// BUILD THREAD, inside an `AppRt` chrome call that is handed only a `&Window`, and
/// in `run()` before `App` exists at all. Those sites used to `eprintln!` and stop,
/// which on a GUI-subsystem Windows launch (Start Menu, Explorer, a pinned tile)
/// means the sentence reaches nobody: the user sets `background_material`, sees no
/// Mica, and has no way to learn why.
///
/// So they queue here instead, and the event loop drains it on its next park
/// ([`crate::App::drain_deferred_config_notices`]) into the same in-window banner
/// the config warnings use. A `Mutex<Vec<String>>` behind an `AtomicBool` so the
/// drain — which runs on EVERY park — is one relaxed load in the overwhelmingly
/// common empty case and never touches the lock.
static DEFERRED_PENDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static DEFERRED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Cap the deferred lane: a pathological loop that queued forever must not grow
/// this vector without bound between two parks. The banner shows at most
/// [`MAX_NOTICE_ROWS`] anyway, and every queuing site is `Once`-guarded, so this is
/// a backstop rather than a policy.
const MAX_DEFERRED: usize = 16;

/// Queue one notice for the next event-loop park. Safe from ANY thread and from
/// before `App` exists. Callers should still `eprintln!` the same text — a console
/// launch keeps its diagnostic, and this adds the surface a windowed launch has.
pub(crate) fn queue_deferred(line: String) {
    let Ok(mut q) = DEFERRED.lock() else {
        return; // a poisoned notice queue must never take the app down
    };
    if q.len() < MAX_DEFERRED && !q.contains(&line) {
        q.push(line);
        DEFERRED_PENDING.store(true, std::sync::atomic::Ordering::Release);
    }
}

/// SERIALIZE the lane across tests. The queue is process-global by design (its
/// whole point is being reachable from threads that have no `App`), so two tests
/// exercising it in the same binary would steal each other's lines. Every test
/// that queues or drains takes this first.
#[cfg(test)]
pub(crate) fn lane_test_guard() -> std::sync::MutexGuard<'static, ()> {
    static LANE_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LANE_TEST.lock().unwrap_or_else(|p| p.into_inner())
}

/// Take everything queued (empty when nothing is). The `Acquire` load pairs with
/// [`queue_deferred`]'s `Release` store, so a line queued on the backend build
/// thread is visible to the event loop that observes the flag.
pub(crate) fn take_deferred() -> Vec<String> {
    if !DEFERRED_PENDING.load(std::sync::atomic::Ordering::Acquire) {
        return Vec::new();
    }
    DEFERRED_PENDING.store(false, std::sync::atomic::Ordering::Relaxed);
    DEFERRED
        .lock()
        .map(|mut q| std::mem::take(&mut *q))
        .unwrap_or_default()
}

/// The banner's left margin and the extra indent its warning list hangs at, in cells.
const MARGIN: usize = 2;
const BULLET_INDENT: usize = MARGIN + 2;

/// One control character, made VISIBLE. The band spends exactly one cell per char
/// (`settings::write_str`), so a control byte in a diagnostic neither draws anything
/// nor honestly occupies the cell it took — it silently eats a column of the row it
/// landed in. `paste_banner::sanitized` spells a hostile clipboard's control bytes
/// this way for the same reason; a config diagnostic is the same hazard arriving from
/// the other direction.
const CONTROL_MARK: char = '\u{00b7}';

/// The PHYSICAL rows one notice asks the band for.
///
/// A NOTICE IS ONE PROBLEM, NOT ONE LINE. The config lane hands this band whole
/// diagnostics, and a rejected `aterm.toml` arrives from
/// `App::surface_native_config_lane_error` as a `toml` parse error carrying its caret
/// diagram across five lines. Written into the band as one string it was not one row
/// of anything: each `\n` took a cell and drew nothing, and the diagram's gutter ran
/// inline through the sentence — `Config observation was not valid TOML: TOML parse
/// error at line 7, column 3   |7 | foo = [bad  |      ^^^…` — until the width cut off
/// whatever was left. The module header has named that input as real since the band was
/// written; it was only ever described, never handled.
///
/// Split here, ONCE, so the painted rows and the announced rows are the same rows by
/// construction (see [`band_text`]). Blank rows are dropped — a `toml` diagram opens on
/// a bare gutter line, and a band with five rows to spend cannot spend one on nothing —
/// trailing whitespace goes with them, and any control character that survives the split
/// becomes [`CONTROL_MARK`]. Every notice yields at least one row, so a notice can never
/// leave the band silently.
fn notice_display_rows(line: &str) -> Vec<String> {
    let mut rows: Vec<String> = line
        .lines()
        .map(|row| {
            row.trim_end()
                .chars()
                .map(|ch| if ch.is_control() { CONTROL_MARK } else { ch })
                .collect::<String>()
        })
        .filter(|row| !row.trim().is_empty())
        .collect();
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

/// How many display rows `lines` asks for in total — [`ConfigNotice::wanted_rows`]'s
/// half of the same count [`band_text`] then lays out.
fn display_row_count(lines: &[String]) -> usize {
    lines
        .iter()
        .map(|line| notice_display_rows(line).len())
        .sum()
}

/// `s` cut to `max` cells with a trailing ellipsis when it does not fit. A hard cut ends
/// a warning mid-token and reads like corruption; an ellipsis says "there is more".
fn ellipsized(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

/// The banner's TITLE row for `count` warnings, and the name a screen reader hears for
/// the same band. One function so the painted words and the announced words cannot drift.
///
/// "config: 3 notice(s) (auto-dismiss)" said the same thing three ways and made the user
/// parse a plural stub. The mark carries the severity, the count carries the scale, and
/// the fact that it leaves on its own is a quiet aside on the right of the painted row.
/// The mark stays inside `write_str`'s stated contract — ASCII and a few narrow BMP
/// glyphs, one char per CELL. A warning triangle would be ambiguous-width and could bleed
/// into the next cell on the terminals that treat it as emoji, which is a poor trade for
/// a mark whose severity the warn colour already carries.
fn title_for(count: usize) -> String {
    format!(
        "!  Config \u{00B7} {count} {}",
        if count == 1 { "notice" } else { "notices" }
    )
}

/// The WORDS the band puts on screen, in reading order — the one source both the pixels
/// and the announcement are built from.
///
/// A live region is spoken in full the instant its sentence changes, so what it says has
/// to be what the eye can see. The `lines` behind this band are RAW diagnostics of
/// unbounded length: a rejected `aterm.toml` arrives as a TOML parse error carrying five
/// embedded newlines and an ASCII caret diagram, and a band that paints one ellipsized row
/// of it while announcing the whole paragraph is describing a screen nobody is looking at.
/// Both readers pass the same `cols`/`panel_rows` the splice uses
/// (`App::splice_config_notice`), so the cut a reader hears and the cut a reader sees are
/// one cut by construction.
pub(crate) struct BandText {
    /// The title row. Counts EVERY notice, including the ones the panel has no room for.
    pub(crate) title: String,
    /// One entry per display ROW the panel has room for, each already cut to the width
    /// the band writes it at. A notice that spans several lines contributes several.
    pub(crate) warnings: Vec<BandWarning>,
    /// The `+N more` tally, present only when the list overflowed AND a row was left to
    /// say so — a capped list that looks complete is worse than no banner.
    pub(crate) more: Option<String>,
}

/// One warning ROW of the band.
pub(crate) struct BandWarning {
    /// The row's words, already cut to the width the band writes it at.
    pub(crate) text: String,
    /// This row CONTINUES the notice above it instead of starting a new one, so the
    /// band hangs it under the bullet rather than giving it a second one. Five bullets
    /// down the side of one parse error would read as five separate problems, and a
    /// bullet in front of a caret diagram's gutter reads as nothing at all.
    pub(crate) continuation: bool,
}

/// [`BandText`] for a banner of `lines` painted `panel_rows` tall in a `cols`-wide window.
pub(crate) fn band_text(lines: &[String], cols: usize, panel_rows: usize) -> BandText {
    // Every notice split into the rows it actually occupies FIRST, so the capacity
    // arithmetic below counts the same rows the band will paint. Counting notices
    // instead is what handed a five-line parse error one row.
    let rows: Vec<BandWarning> = lines
        .iter()
        .flat_map(|line| {
            notice_display_rows(line)
                .into_iter()
                .enumerate()
                .map(|(index, text)| BandWarning {
                    text,
                    continuation: index > 0,
                })
        })
        .collect();
    let total = rows.len();
    // Capacity for warning rows, reserving one for the "+N more" tally when the list
    // overflows and there is room to say so.
    let capacity = panel_rows.saturating_sub(1);
    let overflow = total > capacity;
    let shown = if overflow && capacity >= 2 {
        capacity - 1
    } else {
        capacity
    };
    let text_w = cols.saturating_sub(BULLET_INDENT + MARGIN);
    BandText {
        // The title still counts NOTICES — one malformed file is one notice however
        // many rows its diagnostic needs, and "5 notices" for one parse error would
        // be the same miscount in the other direction.
        title: title_for(lines.len()),
        warnings: rows
            .into_iter()
            .take(shown)
            .map(|row| BandWarning {
                text: ellipsized(&row.text, text_w),
                continuation: row.continuation,
            })
            .collect(),
        more: (overflow && shown < capacity).then(|| {
            // "rows", not "notices": what was dropped is measured in rows now that one
            // notice can ask for several, and the tally must not imply a count of
            // problems it is not counting.
            let more = format!("+{} more rows \u{2014} see the log", total - shown);
            ellipsized(&more, text_w)
        }),
    }
}

/// PURE grid-cell row builder: exactly `panel_rows` rows, each
/// exactly `cols` wide so the splice overwrites frame rows in place. Row 0 is a bold
/// title on a seam (the panel's top edge) with a right-aligned "it goes away by itself"
/// hint; each following row is one warning, bulleted, hanging-indented and ellipsized to
/// width, in the theme's WARN/VALUE colours. When the list does not fit, the last row
/// COUNTS what was dropped rather than silently ending — a banner that shows 5 of 20
/// problems while looking complete is worse than no banner. Reuses
/// `settings::{blank_row,write_str}` so a row is always full-width (the splice
/// debug-asserts `row.len() == cols`).
pub(crate) fn notice_rows(
    lines: &[String],
    cols: usize,
    panel_rows: usize,
    theme: Theme,
) -> Vec<Vec<RenderCell>> {
    let c = chrome_band::band_colors(theme);
    let mut rows: Vec<Vec<RenderCell>> = (0..panel_rows)
        .map(|r| blank_row(cols, c.label, c.bar_bg, r == 0))
        .collect();
    if panel_rows == 0 {
        return rows;
    }
    let band = band_text(lines, cols, panel_rows);
    write_str(
        &mut rows[0],
        cols,
        MARGIN,
        &band.title,
        c.warn,
        c.bar_bg,
        true,
    );
    const HINT: &str = "dismisses on its own";
    let hint_col = cols.saturating_sub(HINT.len() + MARGIN);
    if hint_col > MARGIN + band.title.chars().count() + 2 {
        write_str(&mut rows[0], cols, hint_col, HINT, c.label, c.bar_bg, false);
    }

    for (i, warning) in band.warnings.iter().enumerate() {
        // The bullet marks a NOTICE, so a row that continues one does not get a second.
        if !warning.continuation {
            write_str(
                &mut rows[1 + i],
                cols,
                MARGIN,
                "\u{2022}",
                c.label,
                c.bar_bg,
                false,
            );
        }
        write_str(
            &mut rows[1 + i],
            cols,
            BULLET_INDENT,
            &warning.text,
            c.value,
            c.bar_bg,
            false,
        );
    }
    if let Some(more) = &band.more {
        write_str(
            &mut rows[1 + band.warnings.len()],
            cols,
            BULLET_INDENT,
            more,
            c.label,
            c.bar_bg,
            false,
        );
    }
    // THE SEAM IS THE WHOLE EDGE, AND ONE TONE. `blank_row` stamped it and the two
    // writes above chipped it straight back out — `write_str` rebuilds every cell it
    // touches with no overline — so the band's top edge reached the screen as three
    // stubs with `!  Config \u{00B7} 1 notice` and `dismisses on its own` punched out
    // of it. Closed across the finished row, in the label tone, because this row
    // carries two inks and the rule belongs to neither.
    chrome_band::seal_band_top(&mut rows[0], c.label);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_none_when_empty() {
        let now = Instant::now();
        assert!(ConfigNotice::new(vec![], now).is_none());
        assert!(ConfigNotice::new(vec!["x".into()], now).is_some());
    }

    /// Merging, not replacing — and the merged banner gets a FULL read window.
    /// Both halves are load-bearing: the deferred lane fires while the startup
    /// config banner is up, so replacing would delete the user's real warnings,
    /// and inheriting the old deadline would flash the new line for whatever was
    /// left of the previous eight seconds.
    #[test]
    fn extend_merges_dedupes_and_restarts_the_clock() {
        let t0 = Instant::now();
        let mut n = ConfigNotice::new(vec!["config warning".into()], t0).unwrap();
        let later = t0 + Duration::from_secs(5);
        n.extend(["chrome notice".to_string()], later);
        assert_eq!(n.lines, vec!["config warning", "chrome notice"]);
        assert_eq!(n.deadline(), later + NOTICE_TTL, "the clock restarts");
        // A repeat of a line already shown adds nothing and does NOT extend the
        // banner's life — a lane that re-queues the same sentence on every reload
        // must not be able to pin the banner open.
        let even_later = later + Duration::from_secs(1);
        n.extend(["chrome notice".to_string()], even_later);
        assert_eq!(n.lines.len(), 2, "no duplicate row");
        assert_eq!(n.deadline(), later + NOTICE_TTL, "and no clock restart");
    }

    /// The lane a non-`App` context queues into: it round-trips, it de-duplicates,
    /// and a take on an empty lane is free (the flag short-circuits before the
    /// lock — this asserts the observable half, that it yields nothing).
    #[test]
    fn the_deferred_lane_round_trips_and_dedupes() {
        let _guard = lane_test_guard();
        let _ = take_deferred(); // start from a known-empty lane
        assert!(take_deferred().is_empty(), "an empty lane yields nothing");
        queue_deferred("first".into());
        queue_deferred("first".into());
        queue_deferred("second".into());
        assert_eq!(take_deferred(), vec!["first", "second"]);
        assert!(take_deferred().is_empty(), "taking drains");
    }

    /// The backstop: a site that queued without bound between two parks cannot
    /// grow the lane past [`MAX_DEFERRED`].
    #[test]
    fn the_deferred_lane_is_capped() {
        let _guard = lane_test_guard();
        let _ = take_deferred();
        for i in 0..(MAX_DEFERRED * 3) {
            queue_deferred(format!("line {i}"));
        }
        assert_eq!(take_deferred().len(), MAX_DEFERRED);
    }

    #[test]
    fn expiry_after_ttl() {
        let t0 = Instant::now();
        let n = ConfigNotice::new(vec!["x".into()], t0).unwrap();
        assert!(!n.is_expired(t0));
        assert!(!n.is_expired(t0 + NOTICE_TTL - Duration::from_millis(1)));
        assert!(n.is_expired(t0 + NOTICE_TTL));
        assert_eq!(n.deadline(), t0 + NOTICE_TTL);
    }

    fn text_of(row: &[RenderCell]) -> String {
        row.iter().map(|cell| cell.ch).collect()
    }

    #[test]
    fn notice_rows_shape() {
        let lines = vec![
            "first warning".to_string(),
            "x".repeat(200), // longer than cols -> must ellipsize, no panic
            "third".to_string(),
        ];
        let cols = 80;
        let rows = notice_rows(&lines, cols, 4, Theme::default());
        assert_eq!(rows.len(), 4);
        for row in &rows {
            assert_eq!(row.len(), cols, "every row must be exactly cols wide");
        }
        let row0 = text_of(&rows[0]);
        assert!(row0.contains("3 notices"), "{row0}");
        assert!(row0.contains("dismisses on its own"), "{row0}");
        // Each warning is bulleted and hangs at one indent.
        let row1 = text_of(&rows[1]);
        assert!(row1.trim_start().starts_with('\u{2022}'), "{row1:?}");
        assert!(row1.contains("first warning"), "{row1:?}");
        // The over-long warning is cut with an ellipsis, not mid-token.
        let row2 = text_of(&rows[2]);
        assert!(row2.contains('\u{2026}'), "{row2:?}");
        // wanted_rows caps at MAX_NOTICE_ROWS even with many lines.
        let many: Vec<String> = (0..20).map(|i| format!("w{i}")).collect();
        assert_eq!(
            ConfigNotice::new(many, Instant::now())
                .unwrap()
                .wanted_rows(),
            MAX_NOTICE_ROWS
        );
    }

    /// A CAPPED LIST MUST SAY IT IS CAPPED. Five of twenty warnings shown with no tally
    /// reads as "these are all of them", and the user fixes five of their twenty broken
    /// key bindings.
    #[test]
    fn an_overflowing_list_counts_what_it_dropped() {
        let lines: Vec<String> = (0..20).map(|i| format!("warning {i}")).collect();
        let rows = notice_rows(&lines, 80, MAX_NOTICE_ROWS, Theme::default());
        let last = text_of(&rows[MAX_NOTICE_ROWS - 1]);
        // 6 rows = title + 4 warnings + the tally for the remaining 16.
        assert!(last.contains("+16 more"), "{last:?}");
        assert!(
            text_of(&rows[4]).contains("warning 3"),
            "four warnings shown"
        );
        // A list that FITS says nothing about overflow.
        let rows = notice_rows(&lines[..3], 80, 4, Theme::default());
        assert!(!text_of(&rows[3]).contains("more"), "no spurious tally");
    }

    /// THE SEAM IS THE BAND'S TOP EDGE, so it must run the whole width. It did not:
    /// `blank_row` stamped it and then the title and the right-aligned hint punched
    /// themselves out of it (`write_str` rebuilds every cell it touches, with no
    /// overline), so the rule reached the screen as three stubs — left margin, the gap
    /// between the two runs, right margin — which reads as rendering debris rather than
    /// as a boundary. The same defect `link_target::caption_row` named first and
    /// `the_seam_runs_unbroken_and_one_toned_across_every_cell_of_the_band` pins there.
    #[test]
    fn the_seam_runs_unbroken_and_one_toned_across_the_title_row() {
        let theme = Theme::default();
        let cols = 80;
        let rows = notice_rows(
            &["config line 10: show_scene_hud was removed".into()],
            cols,
            2,
            theme,
        );
        let title = &rows[0];
        assert!(
            title.iter().all(|cell| cell.overline),
            "the seam breaks at columns {:?}",
            title
                .iter()
                .enumerate()
                .filter(|(_, cell)| !cell.overline)
                .map(|(col, _)| col)
                .collect::<Vec<_>>()
        );
        // AND IT IS ONE TONE. The row deliberately carries two inks — the warn-coloured
        // title beside the label-coloured aside — so a seam left to each cell's `fg`
        // would run bright yellow for the title's cells and dim for the rest.
        let seams: std::collections::BTreeSet<Option<[u8; 3]>> =
            title.iter().map(|cell| cell.overline_color).collect();
        assert_eq!(
            seams.len(),
            1,
            "the rule takes as many tones as the band has inks: {seams:?}"
        );
        assert!(
            seams.iter().all(Option::is_some),
            "an uncoloured seam falls back to its cell's ink: {seams:?}"
        );
        let inks: std::collections::BTreeSet<[u8; 3]> = title.iter().map(|cell| cell.fg).collect();
        assert!(
            inks.len() > 1,
            "the title row is meant to carry two inks, so this proof is not vacuous: {inks:?}"
        );
        // ONLY the top row is an edge. The warning rows sit inside the band and a rule
        // over each of them would be a stack of hairlines, not a boundary.
        assert!(rows[1].iter().all(|cell| !cell.overline));
    }

    /// A NOTICE IS ONE PROBLEM, NOT ONE LINE. A rejected `aterm.toml` reaches the band
    /// as a `toml` parse error with its caret diagram — the input the module header has
    /// named as real since the band was written — and it used to be handed exactly one
    /// row: each `\n` took a cell and drew nothing, the diagram's gutter ran inline
    /// through the sentence, and the width cut off whatever was left. The error string
    /// here comes from the real parser, not a literal, so the test cannot pass because
    /// the shape it assumes has changed.
    #[test]
    fn a_multi_line_diagnostic_is_painted_as_the_rows_it_actually_has() {
        let error = "x = [bad\n"
            .parse::<aterm_toml::edit::DocumentMut>()
            .expect_err("a malformed document")
            .to_string();
        assert!(
            error.contains('\n'),
            "this test is about a multi-line diagnostic: {error:?}"
        );
        let line = format!("Config observation was not valid TOML: {error}");
        let notice = ConfigNotice::new(vec![line.clone()], Instant::now()).unwrap();
        assert!(
            notice.wanted_rows() > 2,
            "one notice of several lines asks for several rows, not one: {}",
            notice.wanted_rows()
        );

        let cols = 80;
        let rows = notice_rows(&[line], cols, notice.wanted_rows(), Theme::default());
        let painted: Vec<String> = rows.iter().map(|row| text_of(row)).collect();

        // NO CONTROL CHARACTER REACHES A CELL. `write_str` spends one cell per char, so
        // a `\n` in a cell is a column that draws nothing and says nothing.
        for row in &rows {
            assert!(
                row.iter().all(|cell| !cell.ch.is_control()),
                "a control character took a cell: {:?}",
                row.iter().map(|cell| cell.ch).collect::<String>()
            );
        }
        // The diagnostic's own opening sentence and its caret row land on DIFFERENT
        // rows. Matched on a short prefix, because the sentence is the one row wide
        // enough to be ellipsized and the cut is not what this test is about.
        let first = "TOML parse error";
        let caret = error
            .lines()
            .find(|row| row.contains('^'))
            .expect("the parser's caret row")
            .trim_end();
        let row_of = |needle: &str| {
            painted
                .iter()
                .position(|row| row.contains(needle))
                .unwrap_or_else(|| panic!("{needle:?} is on no row: {painted:?}"))
        };
        assert_ne!(
            row_of(first),
            row_of(caret),
            "the caret diagram must not share a row with the sentence: {painted:?}"
        );
        // ONE PROBLEM, ONE BULLET. A bullet per row would read as several problems, and
        // one in front of the caret gutter reads as nothing at all.
        let bullets = painted
            .iter()
            .filter(|row| row.trim_start().starts_with('\u{2022}'))
            .count();
        assert_eq!(bullets, 1, "one notice takes one bullet: {painted:?}");
        // …and the title still counts NOTICES, not rows.
        assert!(painted[0].contains("1 notice"), "{:?}", painted[0]);

        // WHAT THE EYE SEES IS WHAT THE READER HEARS: the announcement is built from
        // the same split, so it cannot describe a screen nobody is looking at.
        let band = band_text(
            &[format!("Config observation was not valid TOML: {error}")],
            cols,
            notice.wanted_rows(),
        );
        assert!(
            band.warnings.len() > 1 && !band.warnings[0].continuation,
            "{:?}",
            band.warnings
                .iter()
                .map(|w| (&w.text, w.continuation))
                .collect::<Vec<_>>()
        );
        assert!(
            band.warnings[1..].iter().all(|w| w.continuation),
            "every row after the first continues the same notice"
        );
    }

    /// Degenerate geometry must not panic: one row, zero columns, a single warning wider
    /// than the window.
    #[test]
    fn degenerate_geometry_never_panics() {
        for cols in [0_usize, 1, 3, 8, 200] {
            for panel_rows in [0_usize, 1, 2, 6] {
                let lines = vec!["y".repeat(300), "z".into()];
                let rows = notice_rows(&lines, cols, panel_rows, Theme::default());
                assert_eq!(rows.len(), panel_rows);
                for row in &rows {
                    assert_eq!(row.len(), cols);
                }
            }
        }
    }

    #[test]
    fn ellipsized_only_cuts_what_overflows() {
        assert_eq!(ellipsized("short", 10), "short");
        assert_eq!(ellipsized("exactly-ten", 11), "exactly-ten");
        assert_eq!(ellipsized("abcdef", 3), "ab\u{2026}");
        assert_eq!(ellipsized("abcdef", 0), "");
        // Multi-byte input is counted in CHARS, not bytes.
        assert_eq!(
            ellipsized("\u{00e9}\u{00e9}\u{00e9}", 4),
            "\u{00e9}\u{00e9}\u{00e9}"
        );
    }
}
