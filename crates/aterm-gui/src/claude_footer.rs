// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Claude Code footer, host side: aterm writes `◆ Opus 5.5 xhigh   ⌂ ~/aterm   ⎇ main`
//! right-aligned into the rule under Claude Code's input box, as Claude writes its effort tag
//! into the rule above. What the footer says, and what a narrow rule gives up, is decided in
//! `aterm_agent::harness::footer`; this module gets the facts off the event loop and onto the
//! glass.
//!
//! * FACTS. One background thread ([`request_footer`]) reads them — the
//!   process's `sessions/<pid>.json`, its transcript, its own `--model`,
//!   `.git/HEAD`, and what its transcripts APPENDED since the last read (the
//!   session's usage, folded incrementally in its `footer::FooterCache`) —
//!   and publishes them on the session's timeline
//!   (`SessionTimeline::set_claude_footer`), exactly as `session_program`'s
//!   resolver publishes the program. What it has read of each Claude Code
//!   process — its model pin among it — is kept while that process lives,
//!   across a lapsed or stopped watch ([`ProcessTails`]), and a new aterm
//!   process starts with none. The status sweep asks for a refresh at
//!   most every [`RECHECK`] while a Claude Code session is in front. A change
//!   wakes the loop ([`post_changed`] → `Wake::ClaudeFooter`) so the frame that
//!   shows it is not left to the next keystroke. Where nothing since the
//!   process started names its model or effort, the frame compose reads
//!   Claude's launch card off the pane (`footer::launch_card`) on every
//!   rebuild of the pane's text while that holds, and keeps the newest
//!   reading for that process and session (`SessionTimeline::note_claude_card`).
//! * AT REST, NOTHING (owner, 2026-10-15: "the user experince must be
//!   extremely smooth and zero latency!"). The pane's text, its readings
//!   (the rule, the lights' pill, the launch card) and the rule composed
//!   from the facts are kept per pane ([`PaneTextCache`], [`PaneFooter`]): a
//!   frame whose scratch and facts did not move allocates nothing, reads
//!   nothing and composes nothing; a rebuild of the text — a keystroke, a
//!   streamed row, a spinner's tick — reads only what its changed rows can
//!   move ([`Readings::after`]). The work is counted (`work_counts`) and
//!   asserted (`claude_footer_cost_tests.rs`) and its time measured by the
//!   `footer_pass_cost` bench.
//! * THE LIMIT WALL. A limit the session HIT, written into its transcript
//!   (`⧗ 5h limit · resets 3pm`), stands until its reset passes or a response
//!   is served after it; while one shows, the resolver keeps re-reading the
//!   session every [`WALL_RECHECK`] past [`WATCH_FOR`] for as long as its
//!   Claude Code process is alive, so a passed wall leaves an idle footer.
//!   Its reset is placed with a zone's offset AT an instant
//!   ([`offset_at_cached`]).
//! * THE SUBSCRIPTION. The selected Claude profile supplies its actual email, plan and
//!   account-validated weekly quota, read together off the event loop. Missing, expired
//!   and stale usage says `weekly unavailable`; an account switch replaces every field.
//!   The values stay in the volatile footer facts and never enter account-stat records.
//!   While a percentage is shown, the same local slow clock as a limit wall keeps
//!   checking it after idle watches lapse, so stale or reset quota clears within a minute.
//!   A settings/provider/profile-store check once per registered image and working
//!   directory suppresses the whole tuple when Claude uses a different auth source.
//!   Muse Code's tuple is its own login store's address (read on the resolver
//!   thread, `profile::muse_subscription_for`) with the tier and weekly quota read
//!   off its `/status` and `/usage` cards on the glass, as agy's is read off its
//!   launch card; until a card has shown, Muse Code's quota says `weekly
//!   unavailable` rather than a guess.
//!   After a known Muse account switch, or adoption of a terminal's old
//!   glass, anonymous usage cannot prove its account: quota stays unavailable
//!   for that terminal. A matching account-bearing status card may still
//!   supply its own plan. A pane rebuild or process restart does not clear
//!   that provenance boundary.
//! * THE ACCOUNT. Where `accounts.toml` lists two or more, `⎆ <label>` names
//!   the roster row the process's login is (`accounts::footer_label`),
//!   between the model and the path. The login is what the exec environment
//!   of the image Claude Code REGISTERED says, read with its command line
//!   once its registry row is seen ([`Identity`]) — never what a launcher
//!   that became Claude Code in place was started with; the roster is the
//!   resolver thread's own memo ([`RosterMemo`]), read again at most every
//!   [`ROSTER_RECHECK`] and only when a footer read answers, so a label that
//!   moved is on the glass one memo period after the next read. Nothing is
//!   shown rather than guessed: an environment that was not read or reads
//!   empty, a roster that is missing, refused or lists fewer than two, a
//!   login no row names, a home of its own, a credential in the environment.
//!   THE LIMITS: the resolver sees the environment's half only
//!   (`accounts::ProcessLogin::of_env`), so a Claude Code that a settings
//!   file or the vendor's profile store gives a credential is still
//!   labelled; and one started under another `HOME` is named by the row with
//!   no `dir` (`accounts::judged_label`).
//! * THE SURVEY'S EXCERPTS (`aterm_agent::harness::excerpt`: the newest
//!   prompts, words and tool in flight) come off the same tail read and are
//!   published beside the facts (`SessionTimeline::set_claude_excerpts`), so
//!   `survey` reads no transcript; a worker's task head is read once per
//!   image from the file its environment names ([`Identity::task`]).
//! * THE SIDE ([`FactSide`]: the absolute working directory, its
//!   repository's root, the session's traffic over time and its subagents'
//!   calls in flight) is published beside the facts too
//!   (`SessionTimeline::set_claude_side`), for the fact menus, the traffic
//!   card and the agents panel's dots (`crate::claude_panel`): never in the
//!   facts' `Hash`/`Eq`, read by a frame only for the flight table's `Arc`
//!   (one clone under the splice's one timeline lock), rebuilt only when one
//!   of its parts moved ([`side_after`]), each part kept as it was where its
//!   own generation did not.
//! * A CLOSED TAB's fold goes when the tab does ([`stop_session`], from the
//!   status observer's own retirement), not at the end of its watch; the
//!   process's tail stays while the process lives ([`ProcessTails`]).
//! * THE RULE. [`App::splice_claude_footer`] runs with the chrome splices, per
//!   visible pane and inside that pane's own columns — a split's sibling on
//!   the same window row is untouched. It finds the composer's bottom rule by
//!   STRUCTURE (`aterm_phase::phase::composer_bottom`), so the facts show in
//!   every mode, default mode (no pill) included, and nothing is painted
//!   while a dialog or a picker has replaced the composer. It covers ONLY
//!   cells that are plain rule glyphs in the rule's own colours
//!   (`footer::rule_run`); any other ink there is Claude's and stays. Claude's own
//!   footer row under the rule — its mode pill, `(shift+tab to cycle)`, `←
//!   for agents`, `esc to interrupt`, its right-hand notices — is not
//!   written, colours included, but for ONE relabel (owner, 2026-10-19:
//!   "Replace '⏵⏵ bypass permissions on' with 'YOLO on' (or off)"): on a
//!   Claude Code pane whose composer border aterm draws ([`relabels_pill`]),
//!   the bypass pill reads `YOLO on` and the manual-mode pill `YOLO off`, in
//!   the pill cells' own style, and the row's left items after it are packed
//!   left behind the label (`  YOLO on · 14 shells · ← for agents …`), the
//!   right-hand notices where Claude drew them ([`collect_pill_relabel`]).
//!   Every other mode keeps Claude's pill, and a cut pill, a selection, the
//!   cursor, a BiDi row or any cell that is not the row's plain text leaves
//!   the row as drawn. The glass then shows cells away from their grid
//!   columns, so the write records where ([`GlassColumns`]) and the pointer's
//!   every reader maps a glass column back to its grid cell
//!   (`App::pixel_to_cell`): a click on the glass's `14 shells` reports
//!   Claude's `14 shells` cell. The facts are drawn QUIET (owner,
//!   2026-09-30): `Script::Sub` — smaller and set low in the rule's cells,
//!   below the vivid rail of the cursor trail under the composer row above
//!   (`aterm_render::SUB_CEILING`) — and EASY TO READ (owner, 2026-10-08):
//!   bold, values in the rule's gray lifted toward the pane's ink, and SET
//!   as a line of type (`footer::set_facts`, owner 2026-10-14: the
//!   spacing): letters and words `footer::FACT_PITCH` apart, the facts an
//!   even `footer::GAP` apart, the rule running on up to them
//!   (`compose_rule_spans`). `[claude_footer] usage = false` hides `Σ` (shown by
//!   default), applied here and in the repaint key. The Claude lights
//!   (`crate::claude_lights`) go in the same rule, left of the facts, rule
//!   glyphs between. The agents panel under it (owner, 2026-10-19: "I want
//!   to see rainbow") is re-inked (fg only) and lit on glass while the
//!   harness theme is on: its names and a workflow's `▰` bar along the
//!   theme's palette, each dot telling its agent's work
//!   (`crate::claude_panel`, `[claude_footer] agents`).
//! * WHAT IS NOT TOUCHED. The terminal GRID. `text`/`screen` read the engine
//!   and keep Claude Code's real row (its `⏵⏵ bypass permissions on`
//!   included), because aterm's own supervisor reads it
//!   (`aterm_phase` busy detection keys on `esc to interrupt` there). `image`
//!   shows the glass, footer included — the capture paths run this splice too.
//! * THE REDRAW PATH STAYS ON. A host write into a window's scratch bumps its
//!   `snapshot_seq`, and every frame-reuse lane reads that bump as "a host
//!   wrote here, refill it all": the damage-scoped refill (DMG-1), the
//!   effect-only reuse that skips extraction, the strip un-splice (D-2) and
//!   the split compositor's retention (K2). A footer that stayed on screen
//!   would therefore switch all four off for as long as Claude Code is in
//!   front — which is always. So the write is UNDOABLE: the splice records
//!   exactly what it overwrote ([`FooterUndo`]), and the next frame calls
//!   [`undo`] before any of those lanes looks, restoring the cells and the
//!   token. And where the scratch was an untouched engine fill, the painted
//!   row's D-2 revision is set to the no-stamp sentinel and the splice token
//!   re-armed, so the renderer's per-row stamp compare stays on too, with
//!   the rule's row alone compared by content.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use aterm_agent::harness::accounts;
use aterm_agent::harness::footer::{self, FactKind, FactMask, FooterFacts, LaunchFacts};
use aterm_core::render::RenderInput;
use aterm_core::terminal::RenderCell;
use aterm_effects::cli_marks::{Build, CliMark};
use aterm_window::event_loop::EventLoopProxy;

use crate::session_timeline::SessionTimeline;
use crate::{App, VisibleContentRoute, Wake, WindowId};

/// How often a Claude Code session's facts are re-read while it is in front.
/// Model and effort move per turn, the branch on a checkout: two seconds is
/// well inside a turn and costs one small file, one tail and one `HEAD`.
pub(crate) const RECHECK: Duration = Duration::from_secs(2);

/// Whether the footer is drawn for `program`, a session's published program (its foreground
/// argv0's basename): any name the supervisor reads as Claude Code
/// ([`aterm_phase::program_of`]: `claude`, `claude-code`, any case), as the host does
/// ([`crate::harness_host::agent_of`]). The one predicate both askers (the status sweep,
/// `session_program`'s resolver) use. A process so named that is not Claude Code has no
/// `sessions/<pid>.json` (`footer::read_pid`), so nothing is painted for it; a Claude Code
/// started as `node`, which the host supervises by its screen, gets no footer.
pub(crate) fn drawn_for(program: Option<&str>) -> bool {
    program
        .and_then(aterm_phase::program_of)
        .is_some_and(|program| match program {
            aterm_phase::Program::Claude
            | aterm_phase::Program::Agy
            | aterm_phase::Program::Muse => true,
            aterm_phase::Program::Codex
            | aterm_phase::Program::Gemini
            | aterm_phase::Program::Generic => false,
        })
}

/// One pane's painted light block, held until the write says whether its row
/// reached the scratch (only a light the glass shows gets a hit).
struct Lit {
    session: u64,
    frame_row: usize,
    /// The frame column the rule's edit starts at — its key in the write.
    col: usize,
    term_row: usize,
    /// The frame column the lights (not the title) start at.
    lights_col: usize,
    /// The frame columns of the title slot's words (empty: no title).
    title: std::ops::Range<usize>,
    block: crate::claude_lights::Block,
    /// The write put its row on the frame: its hits are noted.
    shown: bool,
}

/// One pane's composed top rule to write: its cells at `col` of frame row
/// `frame_row` (leaf `leaf_index`'s [`ComposedTop`]) less the run of them
/// left on the glass (`glass`, [`ComposedTop::glass`]), as [`rule_edits`],
/// `ground` the pane's blank.
struct TopWrite {
    frame_row: usize,
    col: usize,
    leaf_index: usize,
    ground: RenderCell,
    glass: std::ops::Range<usize>,
}

/// The top-effort key's target on a top rule: frame columns `cols` of frame
/// row `frame_row` (`term_row` in the window's grid). `ghost` is aterm's
/// ghost, a target only where its write took; else Claude's own tag, a
/// target only where the frame still holds the key's own chars there.
struct TopHit {
    frame_row: usize,
    term_row: usize,
    cols: (usize, usize),
    session: u64,
    ghost: bool,
}

/// A top line's lockup on the glass, the hover title's target (design §7):
/// frame columns `cols` of frame row `frame_row` (`term_row` in the window's
/// grid), a target only where the write at `edit_col` took.
struct MarkWrite {
    frame_row: usize,
    term_row: usize,
    edit_col: usize,
    cols: std::ops::Range<usize>,
    session: u64,
}

/// What one pane's top line is spliced with ([`App::splice_top_line`],
/// design §6.2): the pane, its leaf and session and the group the key's
/// evidence is kept for, the program its lockup names (the identified
/// agent) and the build the host proves it ([`top_build`]), the pane's
/// blank, the harness switch and the theme the line wears, whether the
/// footer holds the program's facts (the hover title's version), and the
/// window's cell and the marks' ink.
struct TopLineAt {
    leaf_index: usize,
    session: u64,
    program_pgid: i32,
    program: aterm_phase::Program,
    build: Build,
    region: PaneTextRegion,
    pane: crate::claude_card::PaneGeom,
    blank: RenderCell,
    switch: Option<SwitchPaint>,
    palette: Option<aterm_effects::harness_card::CardPalette>,
    held: bool,
    cell: (u16, u16),
    mark_fg: [u8; 3],
}

/// One undoable row edit for agy's transcript presentation on `RenderInput`.
struct AgyRowEdit {
    frame_row: usize,
    col: usize,
    cells: Vec<RenderCell>,
    ground: RenderCell,
}

/// The footer splice's per-frame lists, kept on the window for their buffers
/// (`WindowState::claude_footer_lists`): taken at the top of a splice and handed back empty
/// at its end, so a frame allocates none of them once they are grown and keeps no pane's
/// text, facts or chips past its splice.
#[derive(Default)]
pub(crate) struct SpliceLists {
    panes: Vec<FooterPane>,
    seen: Vec<SeenPane>,
    /// Panes whose composer is on the glass with no plain rule glyph to
    /// write into (a selection over the whole rule): nothing is painted,
    /// but the keyboard's chord is still the lights'.
    bare: Vec<u64>,
    /// The rules' compositions, written as [`rule_edits`].
    rules: Vec<RuleWrite>,
    /// agy's glass-only transcript markdown edits.
    agy_edits: Vec<AgyRowEdit>,
    /// Claude's mode pills relabelled `YOLO on`/`YOLO off`
    /// ([`collect_pill_relabel`]), their cells in `relabel_cells`.
    relabels: Vec<RelabelWrite>,
    relabel_cells: Vec<RenderCell>,
    /// The composer top rules' compositions, written as their cells
    /// borrowed from the panes' footers ([`ComposedTop`]).
    tops: Vec<TopWrite>,
    /// The top-effort key's targets each top rule would put on the glass
    /// (Claude's tag, aterm's ghost): targets once the glass shows them
    /// ([`TopHit`]).
    top_hits: Vec<TopHit>,
    /// Sessions whose top rule showed Claude's top-effort tag for a process
    /// not yet noted as having shown it (`SessionFacts::top_seen`).
    top_seen: Vec<(u64, i32)>,
    /// The lockups each top line would put on the glass: the hover title's
    /// targets once their write took ([`MarkWrite`]).
    marks: Vec<MarkWrite>,
    /// The leaves whose top line alone was read (Gemini CLI's, Codex's:
    /// no footer facts): their rows and composition are kept as a footer's
    /// are ([`PaneTextCache::retain_active`]).
    top_only: Vec<usize>,
    /// The harness switches this splice drew on composer top rules, each
    /// noted as reachable once its write reached the glass.
    border_toggles: Vec<crate::claude_lights::BorderSwitch>,
    lit: Vec<Lit>,
    /// The facts each rule's write would put on the glass: targets once the
    /// write took ([`FactWrite`]).
    facts: Vec<FactWrite>,
    /// The rule row's coverable cells ([`rule_mask_into`]), one pane's at a time.
    mask: Vec<bool>,
    /// Sessions whose log the launch card's amend moved: their watchers are
    /// woken after the frame (addendum A28, ruling S29).
    log_owed: Vec<u64>,
    /// The agents panels this frame lights (`crate::claude_panel`).
    panels: Vec<crate::claude_panel::PanelLeaf>,
}

/// One pane's composition for its composer rule ([`compose_rule_spans`],
/// kept in the pane's [`PaneFooter`] on leaf `leaf_index`): its cells at
/// `col` of frame row `frame_row`, `ground` the pane's blank, and `glass`
/// the cells of it the rule already draws.
struct RuleWrite {
    frame_row: usize,
    col: usize,
    leaf_index: usize,
    ground: RenderCell,
    glass: std::ops::Range<usize>,
}

/// One fact a rule's write would put on the glass (`footer::fact_spans`,
/// laid at the rule's column): a target only where the edit that carries
/// it — the rule's tail, its facts — took (`write_edits`), as a light's
/// hits are only where its block did ([`Lit::shown`]).
struct FactWrite {
    frame_row: usize,
    term_row: usize,
    /// The column of the edit that writes the facts: the rule's own column,
    /// or the tail's past the rule glyphs the glass already shows
    /// ([`rule_edits`]).
    edit_col: usize,
    session: u64,
    cols: (usize, usize),
    kind: FactKind,
    restores: FactMask,
}

impl SpliceLists {
    /// Empty every list, keeping its buffer.
    fn clear(&mut self) {
        self.panes.clear();
        self.seen.clear();
        self.bare.clear();
        self.rules.clear();
        self.agy_edits.clear();
        self.relabels.clear();
        self.relabel_cells.clear();
        self.tops.clear();
        self.top_hits.clear();
        self.top_seen.clear();
        self.marks.clear();
        self.top_only.clear();
        self.border_toggles.clear();
        self.lit.clear();
        self.facts.clear();
        self.mask.clear();
        self.log_owed.clear();
        self.panels.clear();
    }
}

/// One Claude Code pane's composer rule, gathered under the window borrow
/// and painted after it (the lights need `&mut App`).
struct FooterPane {
    session: u64,
    /// The pane's leaf, whose [`PaneFooter`] holds what the footer shows
    /// (the facts, filled from the kept launch card) and its composition.
    leaf_index: usize,
    frame_row: usize,
    /// The same row as a window grid row.
    term_row: usize,
    col_off: usize,
    /// The pane columns aterm may write into (`footer::rule_run`).
    run: std::ops::Range<usize>,
    /// One of the rule's own glyph cells: what the separator between the
    /// lights and the facts is drawn with.
    rule: RenderCell,
    blank: RenderCell,
    /// The session's own Autopilot word and its view, read once this frame
    /// (`App::session_autopilot_for`): what the harness's chips say (`crate::claude_lights`).
    autopilot: (
        Option<aterm_agent::supervise::Autopilot>,
        crate::autopilot::AutopilotView,
    ),
    /// Whether Claude's mode pill reads `YOLO on`/`YOLO off` on this pane's
    /// glass: the relabel collected under [`relabels_pill`]'s gate
    /// ([`collect_pill_relabel`]'s answer, review G2), what the mode light's
    /// word follows (`crate::claude_lights`, review C8).
    relabelled: bool,
}

/// A visible Claude Code pane and the lights' reading of its rows, held
/// until the lights have observed this frame and the cache has dropped panes
/// that left it.
struct SeenPane {
    leaf_index: usize,
    session: u64,
    lights: LightsRead,
}

/// The lights' reading of a pane's rows (`lights::read_lights`): `None` where its reader
/// panicked (the lights fold nothing in), else the mode and the pill under the composer,
/// `None` without one.
pub(crate) type LightsRead = Option<Option<aterm_agent::harness::lights::Reading>>;

/// Each visible Claude pane's screen text for the footer and lights readers, kept while
/// that pane's scratch revision is unchanged, so effect-only paints (which [`undo`] returns
/// to the pre-footer revision) do not convert every cell of a long transcript again.
///
/// So are the readings of the text ([`Readings`]: the composer's frame, the rule's mode
/// row, the lights' pill), taken once per rebuild of the rows, shared by every frame that
/// reuses them and kept across a rebuild where no row they read changed; and so is the
/// footer composed from the facts ([`PaneFooter`], per leaf). A frame at rest reads, copies
/// and composes nothing.
#[derive(Debug, Default)]
pub(crate) struct PaneTextCache {
    panes: Vec<Option<CachedPaneText>>,
    /// Each leaf's footer as earlier frames composed it.
    footers: Vec<PaneFooter>,
    /// The last rebuild's number: each rebuild of a pane's rows takes the
    /// next ([`CachedPaneText::text_gen`]).
    rebuilds: u64,
    /// The rows a rebuild found changed ([`Self::rows`]), reused.
    changed: Vec<usize>,
    /// The top rules' mark contents, shared by the leaves ([`MarkContents`]).
    marks: MarkContents,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PaneTextKey {
    scratch: Tokens,
    session: u64,
    region: PaneTextRegion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PaneTextRegion {
    /// Rows of window chrome before the terminal cells, as the caller's
    /// `frame_rows - ws.rows` calculates it (not `scratch.rows`, which may
    /// include a host prepend).
    strip: usize,
    row_off: usize,
    col_off: usize,
    rows: usize,
    cols: usize,
}

struct CachedPaneText {
    key: PaneTextKey,
    rows: Arc<Vec<String>>,
    /// This rebuild's number, unique in the cache: the same number means the same cells, so
    /// the harness card keeps its layout without looking at them (`crate::claude_card`).
    text_gen: u64,
    /// The footer's and the lights' readings of `rows`: taken in the rebuild, or kept from
    /// the last where no row they read changed ([`Readings::after`]).
    readings: Readings,
    /// The Claude Code build the vendor-drift check last read these readings
    /// for ([`note_vendor_drift`], by a hash of its version): checked again
    /// only for another build or new readings.
    drift: std::cell::Cell<Option<u64>>,
    /// The launch card on `rows` for the footer's open model or effort, the
    /// reader that read it and the rows it was read from
    /// (`footer::launch_card_read`): read at most once per rebuild, and only
    /// while the facts leave one open ([`PaneTextCache::footer_card`]).
    footer_card: std::cell::OnceCell<Option<(aterm_phase::Program, aterm_phase::LaunchCardRead)>>,
    /// Claude Code's context indicator on `rows`, per cent left, where it
    /// shows it (`aterm_phase::phase::context_left_shown`), for the footer's
    /// context fact: read at most once per rebuild, and only for a Claude
    /// Code pane ([`PaneTextCache::context_reading`]).
    ctx_reading: std::cell::OnceCell<Option<u8>>,
    /// Whether `rows` say Claude Code's indicator has gone
    /// (`footer::context_gone`): read at most once per rebuild, and only
    /// while a reading is shown and `rows` show none
    /// ([`PaneTextCache::indicator_gone`]).
    ctx_gone: std::cell::OnceCell<bool>,
    /// The signed-in account on `rows`: agy's launch card's
    /// (`aterm_phase::agy::launch_card_account`) or Muse Code's `/status`
    /// card's (`aterm_phase::muse::status_card_account`): read at most once
    /// per rebuild, and only while the footer shows an account
    /// ([`PaneTextCache::screen_account`]).
    account: std::cell::OnceCell<Option<(String, Option<String>)>>,
    /// Muse Code's weekly quota on `rows`, as the footer's words
    /// (`aterm_phase::muse::usage_card_weekly`): read at most once per
    /// rebuild, and only while the footer shows an account
    /// ([`PaneTextCache::usage_weekly`]).
    weekly: std::cell::OnceCell<Option<aterm_phase::muse::UsageCardWeekly>>,
    /// Muse Code's subscription tier on `rows`, where a card names one
    /// with no account row for it to ride on
    /// (`aterm_phase::muse::subscription_card_plan`): read at most once
    /// per rebuild, and only while the footer shows an account
    /// ([`PaneTextCache::subscription_plan`]).
    plan: std::cell::OnceCell<Option<String>>,
    /// The rule's coverable run and its glyph on frame row `.0`
    /// ([`rule_mask`], `footer::rule_run`): taken at most once per rebuild —
    /// the scratch it was read off is the one `key`'s tokens name — and kept
    /// past one where the engine's revisions vouch its row's cells unchanged
    /// and no image is placed on it ([`Self::stamps`]).
    rule_run: std::cell::Cell<Option<KeptRun>>,
    /// The top rule's plain run on frame row `.frame_row` ([`read_top_run`]):
    /// taken at most once per rebuild, and kept as [`Self::rule_run`] is.
    top_run: std::cell::Cell<Option<KeptTop>>,
    /// Gemini CLI's top line on `rows`, its live zone's `─` rule
    /// (`aterm_phase::gemini::top_rule`, behind the reader's panic fence):
    /// read at most once per rebuild, and only for a Gemini pane
    /// ([`top_line`]).
    gemini_top: std::cell::OnceCell<Option<usize>>,
    /// Codex's top line on `rows`, its composer slab's top pad row
    /// (`aterm_phase::codex::composer_pad`, behind the reader's panic
    /// fence): a candidate its cells must prove ([`TopLine::Slab`]), read at
    /// most once per rebuild, and only for a Codex pane ([`top_line`]).
    codex_pad: std::cell::OnceCell<Option<usize>>,
    /// The harness card's reading of `rows` (`crate::claude_card::read_card`): taken at most
    /// once per rebuild and only when a card is wanted, or kept from the last rebuild where
    /// no row it reads changed (`CardReading::holds`); shared with every frame that reuses
    /// the rows, never copied.
    card: std::cell::OnceCell<crate::claude_card::CardReading>,
    /// The last reading's card, until this rebuild's reading is taken: an
    /// equal card is the same one, shared (a card that moved back allocates
    /// nothing), and the LAST rebuild's own is held through rows the reader
    /// does not place it on (`crate::claude_card::read_card`).
    card_before: std::cell::Cell<Option<crate::claude_card::CardBefore>>,
    /// The agents panel's reading of `rows` and the frame they were read
    /// from (`crate::claude_panel::read_panel`): taken at most once per
    /// rebuild, and only where the panel's gate holds ([`PaneTextCache::agents_panel`]).
    panel: std::cell::OnceCell<Option<Arc<crate::claude_panel::PanelRead>>>,
    /// The last reading's panel, until this rebuild's is taken: an equal
    /// reading is the same one, shared, and the panel's fold is not run for
    /// it (`App::compose_claude_panels`).
    panel_before: std::cell::Cell<Option<Arc<crate::claude_panel::PanelRead>>>,
    /// Claude Code's update notice under the card on `rows`
    /// (`aterm_phase::update_notice_scan`, for the harness banner drawn over
    /// it): scanned at most once per rebuild, only under a decorated card —
    /// or kept from the last rebuild where no row it read changed
    /// (`aterm_phase::update_notice_unmoved`). Apart from [`Self::card`], so
    /// a change to the notice's rows costs no card read. Keyed by the row
    /// the scan starts at (the card's foot); `None` inside: the reader
    /// panicked, and is not asked again until the rows are rebuilt.
    notice: std::cell::Cell<Option<(usize, Option<aterm_phase::NoticeScan>)>>,
    /// The engine's revisions of the rows `rows` were read from
    /// ([`RowStamps`]): what lets the next rebuild skip every row whose
    /// cells they vouch did not change — and keep [`Self::rule_run`] where
    /// its row is one of them.
    stamps: RowStamps,
}

/// The D-2 row revisions of the scratch a pane's rows were read from
/// (`RenderInput::row_rev`), one per pane row, and the lane they belong to
/// ([`StampLane`]: `None` where the scratch's stamps vouch for nothing).
#[derive(Debug, Default)]
struct RowStamps {
    lane: Option<StampLane>,
    revs: Vec<u64>,
}

/// What two scratches' row revisions must share to be compared — the
/// clauses of `aterm_render::row_revisions_comparable`, which the renderer
/// itself skips an unchanged row's cells by: the engine's lane (non-zero:
/// one terminal's clock), the frame's geometry and its host rows
/// (`row_shift`: a strip that grew moves every engine row), the screen (the
/// alternate keeps its own grid and clock) and the rows' identity (`base_y`:
/// a scroll moves rows under unmoved stamps; `absolute_row_revision`: a
/// renumbering). Taken only from a scratch whose stamps vouch for its cells
/// at all ([`Self::of`]). Two scratches of one lane, with a row's stamp
/// non-zero and the same in both, hold that row's cells, clusters and
/// combining marks unchanged — never its image placements, which no
/// revision vouches for and which the readers check on their own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StampLane {
    lane: u64,
    rows: usize,
    cols: usize,
    row_shift: usize,
    engine_alt: bool,
    base_y: i64,
    absolute_row_revision: u64,
}

impl StampLane {
    /// `scratch`'s lane, where its stamps vouch for its rows: an engine fill
    /// with a lane over every row, no host cell written since (`snapshot_seq
    /// == shifted_fill_seq`: the tab strip's blessed prepend alone), its
    /// rows in logical order, at the live screen. Else `None`: every row is
    /// compared.
    fn of(scratch: &RenderInput) -> Option<Self> {
        #[cfg(test)]
        if !READ_BY_STAMP.with(std::cell::Cell::get) {
            return None;
        }
        (scratch.row_rev_lane != 0
            && scratch.row_rev.len() == scratch.rows
            && scratch.cells.len() == scratch.rows
            && scratch.snapshot_seq == scratch.shifted_fill_seq
            && scratch.engine_row_order == aterm_core::render::RowOrder::Logical
            && scratch.display_offset == 0)
            .then_some(Self {
                lane: scratch.row_rev_lane,
                rows: scratch.rows,
                cols: scratch.cols,
                row_shift: scratch.row_shift,
                engine_alt: scratch.engine_alt,
                base_y: scratch.base_y,
                absolute_row_revision: scratch.absolute_row_revision,
            })
    }
}

#[cfg(test)]
thread_local! {
    /// Row revisions vouch for unchanged rows ([`StampLane::of`]): off,
    /// every rebuild compares every row and reads its rule's run anew — the
    /// glass the tests compare the stamped rebuilds' against
    /// ([`read_every_row_in`]).
    static READ_BY_STAMP: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

/// Run `f` with no row revision trusted: every rebuild of a pane's text
/// compares every row, and reads its rule's run anew.
#[cfg(test)]
pub(crate) fn read_every_row_in<R>(f: impl FnOnce() -> R) -> R {
    let was = READ_BY_STAMP.with(|k| k.replace(false));
    let out = f();
    READ_BY_STAMP.with(|k| k.set(was));
    out
}

/// Whether frame row `frame_row` of `scratch` holds an image placement:
/// what no row revision vouches for, so a run read off a row with one is
/// never kept across a rebuild.
fn has_images(scratch: &RenderInput, frame_row: usize) -> bool {
    scratch
        .images
        .get(frame_row)
        .is_some_and(|row| !row.is_empty())
}

/// A composer rule's run read off frame row `frame_row` ([`CachedPaneText::rule_run`]): the
/// coverable cells `start..end` and the rule's glyph, `None` where the row holds no rule to
/// write into.
#[derive(Debug, Clone, Copy)]
struct KeptRun {
    frame_row: usize,
    run: Option<(usize, usize, RenderCell)>,
    /// The row held an image placement when it was read.
    images: bool,
}

impl KeptRun {
    /// The frame row it was read off, where it may be kept past its
    /// rebuild: read off a row with no image on it.
    fn row(&self) -> Option<usize> {
        (!self.images).then_some(self.frame_row)
    }
}

/// A composer top line's plain run read off frame row `frame_row` as a line
/// of kind `line` for a pane ground `bg` ([`CachedPaneText::top_run`]):
/// `None` where the row holds none a brand edit may write on.
#[derive(Debug, Clone, Copy)]
struct KeptTop {
    frame_row: usize,
    line: TopLine,
    bg: [u8; 3],
    run: Option<TopRun>,
    /// Claude's own top-effort tag on the row, its key's frame columns
    /// (`footer::top_rule_tag`).
    tag: Option<(usize, usize)>,
    /// The row held an image placement when it was read.
    images: bool,
}

impl KeptTop {
    /// The frame row it was read off, where it may be kept past its
    /// rebuild: read off a row with no image on it.
    fn row(&self) -> Option<usize> {
        (!self.images).then_some(self.frame_row)
    }
}

impl std::fmt::Debug for CachedPaneText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedPaneText")
            .field("key", &self.key)
            .field("rows", &self.rows.len())
            .field("text_gen", &self.text_gen)
            .field("readings", &self.readings)
            .field("card", &self.card)
            .finish_non_exhaustive()
    }
}

/// The footer's and the lights' readings of one pane's rows, each by construction a
/// function of the composer's frame and the rows under its bottom rule alone
/// ([`read_pane`]): the rule and the mode row under it (`footer::mode_row_under`), and the
/// mode the lights' pill names and whether one is drawn (`lights::reading_under`). So a
/// rebuild whose changed rows leave the frame in place and change no row under it (a
/// keystroke's echo in the composer, a row Claude streams above it, a spinner's tick) keeps
/// them (`aterm_phase::phase::FrameReach::holds_under`), and searches for the frame again
/// only where a changed row is one its search read ([`Self::after`]).
#[derive(Debug, Clone, Copy)]
struct Readings {
    /// The composer's frame and the rows its search read; `None` where the
    /// search panicked (no reading is then kept: each rebuild reads anew).
    reach: Option<aterm_phase::phase::FrameReach>,
    /// The footer's: where its rule is, and whether a mode row this build
    /// reads sits under it.
    rule: FooterRead,
    /// The lights'.
    lights: LightsRead,
    /// Claude's mode pill under the bottom rule, where it reads `YOLO on`
    /// or `YOLO off` on the glass (`footer::relabel_under`): read with the
    /// rule and kept with it, so a frame at rest reads no row for it
    /// ([`collect_pill_relabel`]).
    relabel: Option<footer::Relabel>,
}

impl Readings {
    /// These readings brought up to `rows`, where `changed` lists the rows changed since:
    /// kept where no changed row is one they read, else read anew; also whether they were
    /// kept. The frame's search runs again only where a changed row is one it read
    /// (`FrameReach::unmoved`).
    fn after(self, session: u64, rows: &[String], changed: &[usize]) -> (Readings, bool) {
        let Some(reach) = self.reach else {
            return (read_pane(session, rows, None), false);
        };
        let now = if reach.unmoved(changed) {
            Some(reach)
        } else {
            #[cfg(test)]
            crate::work_counts::footer_frame_searched();
            crate::reader_guard::read_or_none(session, "Claude Code footer", rows, || {
                aterm_phase::phase::FrameReach::of_or_agy(rows)
            })
        };
        match now {
            Some(now) if reach.holds_under(&now, changed) => (
                Readings {
                    reach: Some(now),
                    ..self
                },
                true,
            ),
            now => (read_pane(session, rows, now), false),
        }
    }
}

/// Read pane `session`'s `rows` for the footer and the lights, each reader behind the
/// reader's panic fence (`crate::reader_guard`): the composer's frame (`found`, where the
/// caller has searched for it) and, under its bottom rule, the footer's mode row
/// ([`read_footer`]) and the lights' pill. Where the frame's own search panics, the footer
/// reads no rule and the lights' reader reads the rows whole, behind its own fence.
fn read_pane(
    session: u64,
    rows: &[String],
    found: Option<aterm_phase::phase::FrameReach>,
) -> Readings {
    #[cfg(test)]
    crate::work_counts::footer_pane_read();
    let reach = found.or_else(|| {
        crate::reader_guard::read_or_none(session, "Claude Code footer", rows, || {
            aterm_phase::phase::FrameReach::of_or_agy(rows)
        })
    });
    let rule =
        reach.and_then(|reach| read_footer(session, rows, |rows| read_rule_under(rows, reach)));
    let lights =
        crate::reader_guard::read_or_none(session, "Claude Code lights", rows, || match reach {
            Some(reach) => reach
                .rules()
                .map(|(_, bottom)| aterm_agent::harness::lights::reading_under(rows, bottom)),
            None => aterm_agent::harness::lights::read_lights(rows),
        });
    let relabel = rule.and_then(|read| {
        #[cfg(test)]
        crate::work_counts::pill_relabel_read();
        crate::reader_guard::read_or_none(session, "Claude Code mode pill", rows, || {
            footer::relabel_under(rows, read.rule)
        })
        .flatten()
    });
    Readings {
        reach,
        rule,
        lights,
        relabel,
    }
}

impl PaneTextCache {
    /// A removed leaf must not retain a transcript (or grow this cache over a
    /// long sequence of tab layouts).
    fn trim(&mut self, leaves: usize) {
        self.panes.truncate(leaves);
        self.footers.truncate(leaves);
    }

    /// A pane that no longer shows a Claude footer must not keep its old
    /// transcript resident just because another pane still has one — nor its
    /// facts or its composition. A pane whose card alone was read this frame
    /// (`card`: a drawer with no facts yet) keeps its transcript, where the
    /// card's reading lives, and forgets its footer. A pane whose top line
    /// alone was read (`top`: Gemini CLI's, Codex's) keeps its transcript and
    /// its top line's composition, and forgets the rest of its footer: no
    /// facts are its.
    fn retain_active(
        &mut self,
        active: impl Fn(usize) -> bool,
        card: impl Fn(usize) -> bool,
        top: impl Fn(usize) -> bool,
    ) {
        for (index, cached) in self.panes.iter_mut().enumerate() {
            if !active(index) && !card(index) && !top(index) {
                *cached = None;
            }
        }
        for (index, footer) in self.footers.iter_mut().enumerate() {
            if active(index) {
                continue;
            }
            if top(index) {
                if footer.from.is_some() || footer.rule.key.is_some() {
                    *footer = PaneFooter {
                        session: footer.session,
                        top: std::mem::take(&mut footer.top),
                        ..PaneFooter::default()
                    };
                }
            } else if footer.from.is_some() || footer.rule.key.is_some() || footer.top.key.is_some()
            {
                *footer = PaneFooter::default();
            }
        }
    }

    /// Leaf `leaf_index`'s rows, read as Claude Code's, rebuilt only where the
    /// scratch's revision, the session or the region moved, with their
    /// readings: Claude Code's footer and lights.
    ///
    /// A rebuild compares a row's text only where the engine's revision of
    /// it does not vouch it unchanged ([`RowStamps`]): a keystroke's frame
    /// compares the caret row alone, not every cell of the pane. And the
    /// rule's run read off the last rebuild's scratch is kept where its row
    /// is one the revisions vouch for and no image is placed on it — every
    /// cell, cluster and mark the run read the same.
    fn rows(
        &mut self,
        leaf_index: usize,
        session: u64,
        scratch: &RenderInput,
        region: PaneTextRegion,
    ) -> Arc<Vec<String>> {
        let key = PaneTextKey {
            scratch: Tokens::of(scratch),
            session,
            region,
        };
        if let Some(Some(cached)) = self.panes.get(leaf_index)
            && cached.key == key
        {
            return Arc::clone(&cached.rows);
        }
        // A typed echo changes the revision but few rows. The last rebuild's rows are
        // compared as the new ones are read and only the changed ones are written: into
        // their own buffers (the Arc reused) once the last frame's readers have gone. A
        // still-held Arc stays an immutable snapshot, shared by this rebuild where no row
        // changed and else replaced by fresh storage; another session or region gets fresh
        // storage so the old transcript does not linger in spare capacity.
        let last = self
            .panes
            .get_mut(leaf_index)
            .and_then(Option::take)
            .filter(|cached| cached.key.session == session && cached.key.region == region);
        let changed = &mut self.changed;
        changed.clear();
        // Row `r` of the pane: its frame row, its cells in the frame (`None`:
        // a row the frame does not hold), and their chars.
        let frame_row = |r: usize| region.strip + region.row_off + r;
        let span = |r: usize| {
            scratch.cells.get(frame_row(r)).map(|row| {
                let row = row.get(region.col_off..).unwrap_or_default();
                &row[..row.len().min(region.cols)]
            })
        };
        let cells = |r: usize| span(r).map(|row| row.iter().map(|c| c.ch));
        // Whether row `r`'s text is `was`, compared cell by cell.
        let same = |was: &str, r: usize| {
            #[cfg(test)]
            crate::work_counts::footer_row_compared();
            same_text(was, span(r))
        };
        let fresh = || -> Vec<String> {
            (0..region.rows)
                .map(|r| cells(r).map_or_else(String::new, Iterator::collect))
                .collect()
        };
        // The engine's revision of each pane row in this scratch (`0`: none
        // vouches for it), where its lane vouches for anything.
        let lane = StampLane::of(scratch);
        let rev = |r: usize| {
            lane.map_or(0, |_| {
                scratch.row_rev.get(frame_row(r)).copied().unwrap_or(0)
            })
        };
        let (
            text,
            readings,
            drift,
            card,
            card_before,
            notice,
            stamps,
            rule_run,
            panel_before,
            top_run,
        ) = match last {
            Some(cached) => {
                // Row `r` is the last rebuild's, cell for cell: the same
                // lane, and the same non-zero revision of it in both.
                let vouched = lane.is_some() && cached.stamps.lane == lane;
                let was = &cached.stamps.revs;
                let unmoved = |r: usize| {
                    vouched && {
                        let now = rev(r);
                        now != 0 && was.get(r) == Some(&now)
                    }
                };
                let mut rows = cached.rows;
                if let Some(texts) = Arc::get_mut(&mut rows) {
                    texts.resize_with(region.rows, String::new);
                    for (r, text) in texts.iter_mut().enumerate() {
                        if !unmoved(r) && !same(text, r) {
                            changed.push(r);
                            text.clear();
                            text.extend(cells(r).into_iter().flatten());
                        }
                    }
                } else {
                    changed.extend((0..region.rows).filter(|&r| {
                        !unmoved(r) && !same(rows.get(r).map_or("", String::as_str), r)
                    }));
                    if !changed.is_empty() {
                        rows = Arc::new(fresh());
                    }
                }
                // The runs read off the last rebuild's scratch, kept where
                // their row is unmoved and held no image then or now.
                let pane_row = |frame: usize| {
                    frame
                        .checked_sub(region.strip + region.row_off)
                        .filter(|&r| r < region.rows)
                };
                let keeps = |frame: usize| {
                    pane_row(frame).is_some_and(&unmoved) && !has_images(scratch, frame)
                };
                let rule_run = cached
                    .rule_run
                    .get()
                    .filter(|kept| kept.row().is_some_and(keeps));
                let top_run = cached
                    .top_run
                    .get()
                    .filter(|kept| kept.row().is_some_and(keeps));
                // The footer's and the lights' readings: kept where no row
                // they read changed, else read anew — and the drift check's
                // build with them.
                let (readings, kept) = cached.readings.after(session, &rows, changed);
                let drift = if kept { cached.drift.get() } else { None };
                // The card the last rebuild read here: kept where no row it
                // reads changed, else this rebuild's reading shares its card
                // where it is the same, and may HOLD it — the last reading's
                // own card, judged on the rows of the rebuild after that
                // reading (or on the same rows again: a rebuild that read no
                // card and changed no row hands it on as it was), never a
                // card from before a reading that had none, nor across a
                // change no reading saw.
                // The panel the last rebuild read here (or, where it read
                // none, the one before): an equal reading shares it.
                let panel_before = match cached.panel.into_inner() {
                    Some(read) => read,
                    None => cached.panel_before.into_inner(),
                };
                let reading = cached.card.into_inner();
                let before = cached.card_before.into_inner();
                let (card, card_before) = match reading {
                    Some(reading) if reading.holds(session, &rows, changed) => {
                        (std::cell::OnceCell::from(reading), None)
                    }
                    Some(reading) => (
                        std::cell::OnceCell::new(),
                        match reading.read {
                            Some(card) => Some(crate::claude_card::CardBefore { card, last: true }),
                            None => before.map(crate::claude_card::CardBefore::earlier),
                        },
                    ),
                    None if changed.is_empty() => (std::cell::OnceCell::new(), before),
                    None => (
                        std::cell::OnceCell::new(),
                        before.map(crate::claude_card::CardBefore::earlier),
                    ),
                };
                // The update notice the last rebuild scanned for: kept where
                // no row the scan read changed (exact, under the same card:
                // the cache asks again for another), else scanned again when
                // asked.
                let notice = cached.notice.get().filter(|(_, scan)| {
                    scan.is_some_and(|scan| {
                        crate::reader_guard::read_or_none(session, "harness notice", &rows, || {
                            aterm_phase::update_notice_unmoved(&rows, &scan, changed)
                        })
                        .unwrap_or(false)
                    })
                });
                // This scratch's revisions, into the last rebuild's buffer.
                let mut stamps = cached.stamps;
                stamps.lane = lane;
                stamps.revs.clear();
                if lane.is_some() {
                    stamps.revs.extend((0..region.rows).map(rev));
                }
                (
                    rows,
                    readings,
                    drift,
                    card,
                    card_before,
                    notice,
                    stamps,
                    rule_run,
                    panel_before,
                    top_run,
                )
            }
            None => {
                let rows = Arc::new(fresh());
                let readings = read_pane(session, &rows, None);
                let stamps = RowStamps {
                    lane,
                    revs: if lane.is_some() {
                        (0..region.rows).map(rev).collect()
                    } else {
                        Vec::new()
                    },
                };
                (
                    rows,
                    readings,
                    None,
                    std::cell::OnceCell::new(),
                    None,
                    None,
                    stamps,
                    None,
                    None,
                    None,
                )
            }
        };
        if self.panes.len() <= leaf_index {
            self.panes.resize_with(leaf_index + 1, || None);
        }
        #[cfg(test)]
        crate::work_counts::pane_text_rebuilt();
        self.rebuilds += 1;
        self.panes[leaf_index] = Some(CachedPaneText {
            key,
            rows: Arc::clone(&text),
            text_gen: self.rebuilds,
            readings,
            drift: std::cell::Cell::new(drift),
            footer_card: std::cell::OnceCell::new(),
            ctx_reading: std::cell::OnceCell::new(),
            ctx_gone: std::cell::OnceCell::new(),
            account: std::cell::OnceCell::new(),
            weekly: std::cell::OnceCell::new(),
            plan: std::cell::OnceCell::new(),
            rule_run: std::cell::Cell::new(rule_run),
            top_run: std::cell::Cell::new(top_run),
            gemini_top: std::cell::OnceCell::new(),
            codex_pad: std::cell::OnceCell::new(),
            card,
            card_before: std::cell::Cell::new(card_before),
            panel: std::cell::OnceCell::new(),
            panel_before: std::cell::Cell::new(panel_before),
            notice: std::cell::Cell::new(notice),
            stamps,
        });
        text
    }

    /// The footer's and the lights' readings of leaf `leaf_index`'s cached
    /// rows ([`Self::rows`] took them).
    fn readings(&self, leaf_index: usize) -> Option<Readings> {
        Some(self.panes.get(leaf_index)?.as_ref()?.readings)
    }

    /// The vendor-drift check ([`note_vendor_drift`]) of leaf `leaf_index`'s cached rows for
    /// build `version`: run once per reading of them and build, since running it again could
    /// only find what it found and log nothing.
    fn note_drift(&self, leaf_index: usize, session: u64, version: Option<&str>) {
        use std::hash::{Hash, Hasher};
        let Some(Some(cached)) = self.panes.get(leaf_index) else {
            return;
        };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        version.hash(&mut h);
        let build = h.finish();
        if cached.drift.replace(Some(build)) != Some(build) {
            note_vendor_drift(session, version, &cached.rows);
        }
    }

    /// The launch card on leaf `leaf_index`'s cached rows, while they are
    /// `session`'s, for the footer's open model or effort, with the reader
    /// that read it and its rows (`footer::launch_card_read`, behind the
    /// reader's panic fence): read at most once per rebuild of the rows,
    /// every frame that reuses them sharing the reading.
    fn footer_card(
        &self,
        leaf_index: usize,
        session: u64,
    ) -> Option<&(aterm_phase::Program, aterm_phase::LaunchCardRead)> {
        let cached = self.panes.get(leaf_index)?.as_ref()?;
        if cached.key.session != session {
            return None;
        }
        cached
            .footer_card
            .get_or_init(|| {
                #[cfg(test)]
                crate::work_counts::footer_card_read();
                crate::reader_guard::read_or_none(
                    session,
                    "Claude Code launch card",
                    &cached.rows,
                    || footer::launch_card_read(&cached.rows),
                )
                .flatten()
            })
            .as_ref()
    }

    /// Claude Code's context indicator on leaf `leaf_index`'s cached rows,
    /// per cent left, where the screen shows it
    /// (`aterm_phase::phase::context_left_shown`: above the composer, or at
    /// the end of 2.1.305's mode row; behind the reader's panic fence): read
    /// at most once per rebuild of the rows, every frame that reuses them
    /// sharing the reading — a frame at rest scans nothing.
    fn context_reading(&self, leaf_index: usize) -> Option<u8> {
        let cached = self.panes.get(leaf_index)?.as_ref()?;
        *cached.ctx_reading.get_or_init(|| {
            crate::reader_guard::read_or_none(
                cached.key.session,
                "Claude Code context indicator",
                &cached.rows,
                || aterm_phase::phase::context_left_shown(&cached.rows),
            )
            .flatten()
        })
    }

    /// Whether leaf `leaf_index`'s cached rows say Claude Code's context
    /// indicator has gone, as after `/compact` (`footer::context_gone`,
    /// behind the reader's panic fence; `false` where it panicked or there
    /// are no rows): read at most once per rebuild of the rows.
    fn indicator_gone(&self, leaf_index: usize) -> bool {
        let Some(Some(cached)) = self.panes.get(leaf_index) else {
            return false;
        };
        *cached.ctx_gone.get_or_init(|| {
            crate::reader_guard::read_or_none(
                cached.key.session,
                "Claude Code context indicator gone",
                &cached.rows,
                || footer::context_gone(&cached.rows),
            )
            .unwrap_or(false)
        })
    }

    /// The signed-in account on leaf `leaf_index`'s cached rows, its email
    /// and its plan: Muse Code's `/status` card's
    /// (`aterm_phase::muse::status_card_account`), any other `agent`'s
    /// agy's launch card's (`aterm_phase::agy::launch_card_account`), behind
    /// the reader's panic fence: read and copied at most once per rebuild of
    /// the rows.
    fn screen_account(
        &self,
        leaf_index: usize,
        agent: Option<aterm_phase::Program>,
    ) -> Option<(&str, Option<&str>)> {
        let cached = self.panes.get(leaf_index)?.as_ref()?;
        cached
            .account
            .get_or_init(|| {
                crate::reader_guard::read_or_none(
                    cached.key.session,
                    "launch or status card account",
                    &cached.rows,
                    || match agent {
                        Some(aterm_phase::Program::Muse) => {
                            aterm_phase::muse::status_card_account(&cached.rows)
                        }
                        _ => aterm_phase::agy::launch_card_account(&cached.rows)
                            .map(|(email, plan)| (email.to_owned(), plan.map(str::to_owned))),
                    },
                )
                .flatten()
            })
            .as_ref()
            .map(|(email, plan)| (email.as_str(), plan.as_deref()))
    }

    /// Muse Code's weekly quota on leaf `leaf_index`'s cached rows, as the
    /// footer's words (`aterm_phase::muse::usage_card_weekly`, behind the
    /// reader's panic fence): read at most once per rebuild of the rows.
    fn usage_weekly(&self, leaf_index: usize) -> Option<(&str, Option<&str>)> {
        let cached = self.panes.get(leaf_index)?.as_ref()?;
        cached
            .weekly
            .get_or_init(|| {
                crate::reader_guard::read_or_none(
                    cached.key.session,
                    "Muse Code usage card weekly",
                    &cached.rows,
                    || aterm_phase::muse::usage_card_weekly(&cached.rows),
                )
                .flatten()
            })
            .as_ref()
            .map(|weekly| (weekly.text.as_str(), weekly.reset.as_deref()))
    }

    /// Muse Code's subscription tier on leaf `leaf_index`'s cached rows
    /// (`aterm_phase::muse::subscription_card_plan`, behind the reader's
    /// panic fence): read at most once per rebuild of the rows. A
    /// `/usage` card alone names it, where [`Self::screen_account`] (its
    /// account row on `/status`) sees nothing.
    fn subscription_plan(&self, leaf_index: usize) -> Option<&str> {
        let cached = self.panes.get(leaf_index)?.as_ref()?;
        cached
            .plan
            .get_or_init(|| {
                crate::reader_guard::read_or_none(
                    cached.key.session,
                    "Muse Code subscription card plan",
                    &cached.rows,
                    || aterm_phase::muse::subscription_card_plan(&cached.rows),
                )
                .flatten()
            })
            .as_deref()
    }

    /// The run of leaf `leaf_index`'s composer rule on frame row `frame_row`
    /// of `scratch` that aterm may cover, and the rule's glyph
    /// ([`rule_mask_into`] into `mask`, `footer::rule_run`): read off the
    /// scratch at most once per rebuild of the pane's rows — a frame that
    /// reuses them has the scratch their key's tokens name, cell for cell
    /// (what [`undo`] gives back), so its rule reads as it read — and not
    /// at all on a rebuild whose revisions vouch the rule's row unchanged
    /// (a keystroke's, a streamed row's: [`Self::rows`]).
    fn rule_run(
        &self,
        leaf_index: usize,
        scratch: &RenderInput,
        frame_row: usize,
        (col_off, pane_cols): (usize, usize),
        mask: &mut Vec<bool>,
    ) -> Option<(std::ops::Range<usize>, RenderCell)> {
        let read = |mask: &mut Vec<bool>| {
            #[cfg(test)]
            crate::work_counts::footer_rule_masked();
            let rule = rule_mask_into(mask, scratch, frame_row, col_off, pane_cols)?;
            let run = footer::rule_run(mask)?;
            Some((run.start, run.end, rule))
        };
        let run = match self.panes.get(leaf_index) {
            Some(Some(cached)) => match cached.rule_run.get() {
                Some(kept) if kept.frame_row == frame_row => kept.run,
                _ => {
                    let run = read(mask);
                    cached.rule_run.set(Some(KeptRun {
                        frame_row,
                        run,
                        images: has_images(scratch, frame_row),
                    }));
                    run
                }
            },
            _ => read(mask),
        };
        run.map(|(start, end, rule)| (start..end, rule))
    }

    /// The plain run of leaf `leaf_index`'s composer top line (pane row
    /// `top_rule` of `region`, a line of kind `line`) on `scratch`
    /// ([`read_top_run`]) and, on a rule, Claude's top-effort tag on it, its
    /// key's frame columns (`footer::top_rule_tag` of the pane's rows, behind
    /// the reader's panic fence): read at most once per rebuild of the pane's
    /// rows, as [`Self::rule_run`] is, and kept past one whose revisions vouch
    /// the row unchanged.
    #[expect(
        clippy::too_many_arguments,
        reason = "the pane, its row and the line kind, as the read is keyed"
    )]
    fn top_run(
        &self,
        leaf_index: usize,
        (session, program): (u64, aterm_phase::Program),
        scratch: &RenderInput,
        region: PaneTextRegion,
        top_rule: usize,
        line: TopLine,
        blank: RenderCell,
    ) -> (Option<TopRun>, Option<(usize, usize)>) {
        let frame_row = region.strip + region.row_off + top_rule;
        let muse = program == aterm_phase::Program::Muse;
        let Some(Some(cached)) = self.panes.get(leaf_index) else {
            return (
                read_top_run(scratch, region, top_rule, line, blank, muse),
                None,
            );
        };
        match cached.top_run.get() {
            Some(kept)
                if kept.frame_row == frame_row && kept.line == line && kept.bg == blank.bg =>
            {
                (kept.run, kept.tag)
            }
            _ => {
                let run = read_top_run(scratch, region, top_rule, line, blank, muse);
                let tag = match line {
                    TopLine::Rule => crate::reader_guard::read_or_none(
                        session,
                        "Claude Code top rule",
                        &cached.rows,
                        || footer::top_rule_tag(cached.rows.get(top_rule)?),
                    )
                    .flatten()
                    .map(|(a, b)| (region.col_off + a, region.col_off + b)),
                    TopLine::Slab => None,
                };
                cached.top_run.set(Some(KeptTop {
                    frame_row,
                    line,
                    bg: blank.bg,
                    run,
                    tag,
                    images: has_images(scratch, frame_row),
                }));
                (run, tag)
            }
        }
    }

    /// Where leaf `leaf_index`'s cached rows put `program`'s top line, and
    /// its kind ([`top_line`]).
    fn top_line(
        &self,
        leaf_index: usize,
        program: aterm_phase::Program,
    ) -> Option<(usize, TopLine)> {
        top_line(program, self.panes.get(leaf_index)?.as_ref()?)
    }

    /// Leaf `leaf_index`'s top rule composed from `key`
    /// ([`PaneFooter::compose_top`]), its mark's content from the window's
    /// kept ones.
    fn compose_top(&mut self, leaf_index: usize, key: TopKey) -> &ComposedTop {
        let Self { footers, marks, .. } = self;
        footers[leaf_index].compose_top(key, marks)
    }

    /// Leaf `leaf_index`'s composed top rule's mark sprite ([`mark_layer`]).
    fn top_mark(&self, leaf_index: usize) -> Option<&aterm_core::render::FxLayer> {
        self.footers.get(leaf_index)?.top.mark.as_ref()
    }

    /// Leaf `leaf_index`'s composed top rule's cells ([`compose_top`]).
    fn top_cells(&self, leaf_index: usize) -> &[RenderCell] {
        self.footers
            .get(leaf_index)
            .map_or(&[], |f| f.top.cells.as_slice())
    }

    /// Leaf `leaf_index`'s footer, `session`'s: a pane that shows another
    /// session starts afresh.
    fn footer(&mut self, leaf_index: usize, session: u64) -> &mut PaneFooter {
        if self.footers.len() <= leaf_index {
            self.footers
                .resize_with(leaf_index + 1, PaneFooter::default);
        }
        let footer = &mut self.footers[leaf_index];
        if footer.session != session {
            *footer = PaneFooter {
                session,
                ..PaneFooter::default()
            };
        }
        footer
    }

    /// Leaf `leaf_index`'s footer as this frame gathered it.
    fn footer_mut(&mut self, leaf_index: usize) -> Option<&mut PaneFooter> {
        self.footers.get_mut(leaf_index)
    }

    /// The cells of leaf `leaf_index`'s composed rule ([`PaneFooter::compose`]).
    fn composed(&self, leaf_index: usize) -> &[RenderCell] {
        self.footers
            .get(leaf_index)
            .map_or(&[], |f| f.rule.paint.cells.as_slice())
    }

    /// The launch card on leaf `leaf_index`'s cached rows, while they are `session`'s
    /// (`crate::claude_card::read_card`): read at most once per rebuild of the rows, and not
    /// at all where the rebuild kept the last reading ([`Self::rows`]). A frame that reuses
    /// the rows shares the reading and allocates nothing for it, and a rebuild whose card
    /// reads as the last one did shares that card. With it, the number of the rows' current
    /// rebuild (`CachedPaneText::text_gen`), which names this frame's cells, not the rebuild
    /// a kept reading was taken in.
    pub(crate) fn launch_card(
        &self,
        leaf_index: usize,
        session: u64,
    ) -> Option<(Arc<crate::claude_card::CardRead>, u64)> {
        let cached = self.panes.get(leaf_index)?.as_ref()?;
        if cached.key.session != session {
            return None;
        }
        let read = cached
            .card
            .get_or_init(|| {
                crate::claude_card::read_card(session, &cached.rows, cached.card_before.take())
            })
            .read
            .clone()?;
        Some((read, cached.text_gen))
    }

    /// The agents panel under leaf `leaf_index`'s composer bottom rule
    /// `bottom`, while its rows are `session`'s, and what Claude drew it in
    /// off `scratch` — the frame the rows were read from, cell for cell (a
    /// frame that reuses them has the scratch their key's tokens name):
    /// read at most once per rebuild of the rows
    /// (`crate::claude_panel::read_panel`), every frame that reuses them
    /// sharing the reading, and a reading equal to the last one sharing that.
    pub(crate) fn agents_panel(
        &self,
        leaf_index: usize,
        session: u64,
        bottom: usize,
        scratch: &RenderInput,
        pane: crate::claude_card::PaneGeom,
        blank: RenderCell,
    ) -> Option<Arc<crate::claude_panel::PanelRead>> {
        let cached = self.panes.get(leaf_index)?.as_ref()?;
        if cached.key.session != session {
            return None;
        }
        cached
            .panel
            .get_or_init(|| {
                let read = crate::claude_panel::read_panel(
                    session,
                    &cached.rows,
                    bottom,
                    scratch,
                    pane,
                    blank,
                )?;
                match cached.panel_before.take() {
                    Some(before) if *before == read => Some(before),
                    _ => Some(Arc::new(read)),
                }
            })
            .clone()
    }

    /// Claude Code's update notice under the card at `place` on leaf
    /// `leaf_index`'s cached rows, while they are `session`'s, the pane
    /// `cols` wide (`aterm_phase::update_notice_scan`, behind the reader's
    /// panic fence; the rows are the pane's at one width, the cache's key):
    /// scanned at most once per rebuild of the rows, and not at all where
    /// the rebuild kept the last scan of the rows under the same card
    /// ([`Self::rows`]: no row it read changed). `None` where no notice is
    /// the vendor's whole, and where the reader panicked.
    pub(crate) fn update_notice(
        &self,
        leaf_index: usize,
        session: u64,
        place: &aterm_phase::CardPlace,
        cols: usize,
    ) -> Option<aterm_phase::NoticePlace> {
        let cached = self.panes.get(leaf_index)?.as_ref()?;
        if cached.key.session != session {
            return None;
        }
        let from = place.top.saturating_add(place.rows);
        if let Some((at, scan)) = cached.notice.get()
            && at == from
        {
            return scan.and_then(|scan| scan.place);
        }
        #[cfg(test)]
        crate::work_counts::card_notice_read();
        let scan =
            crate::reader_guard::read_or_none(session, "harness notice", &cached.rows, || {
                aterm_phase::update_notice_scan(&cached.rows, place, cols)
            });
        cached.notice.set(Some((from, scan)));
        scan.and_then(|scan| scan.place)
    }
}

/// One pane's footer as earlier frames composed it (a leaf's, in [`PaneTextCache`]): the
/// facts as the session's timeline held them and as the rule shows them, and the rule
/// composed from them. A frame whose inputs are the last one's (every frame at rest, every
/// keystroke) takes the rule as it stands: no copy of the facts, no fit, no composition, no
/// allocation. Each part is made anew only where its inputs changed, compared field by
/// field and cell by cell, never by a hash, so it always holds what making it anew would.
#[derive(Debug, Default)]
struct PaneFooter {
    session: u64,
    /// What [`Self::shown`] is made of: the facts the timeline holds for the
    /// group in front, the launch card it keeps for them, the `[claude_footer]
    /// usage` switch, and the facts the person hid in the session by a click
    /// (`crate::claude_facts`).
    from: Option<(FooterFacts, Option<aterm_phase::LaunchCard>, bool, FactMask)>,
    /// The live upgrade's note the timeline holds for the pane, whose words
    /// the rule lays in ([`Self::lay_upgrade`]).
    note: Option<footer::UpgradeNote>,
    /// What the rule shows: `from`'s facts filled from its card where
    /// nothing newer decided them (`FooterFacts::filled_from`), under its
    /// usage switch ([`with_usage_switch`]), its hidden facts laid in
    /// (`FooterFacts::off`), with the upgrade's words this frame says.
    shown: FooterFacts,
    /// Bumped on every change of `shown`; the kept chips' room and composition are keyed by
    /// it.
    shown_gen: u64,
    /// The repaint key's term for `from` ([`fingerprint`]), taken
    /// with it: the key asks for it every frame ([`Self::fingerprint_of`]).
    fp: u64,
    /// `footer::chips_room` of `shown` at `room` cells: `(shown_gen, room,
    /// chips_room)`.
    chips: Option<(u64, usize, usize)>,
    /// The rule composed from `shown`.
    rule: ComposedRule,
    /// The composer's top rule as the frames before composed it.
    top: ComposedTop,
    /// Whether `shown` names a Claude Code build that takes the top-effort
    /// key's toggle: `(shown_gen, typed)` ([`Self::key_typed`]).
    typed: Option<(u64, bool)>,
}

/// A footer rule as [`PaneFooter::compose`] composed it, and what it was
/// composed from.
#[derive(Debug, Default)]
struct ComposedRule {
    key: Option<RuleKey>,
    /// The chips' block it was composed with (`None`: no block), copied.
    block: Option<BlockCopy>,
    paint: RulePaint,
}

/// A composed footer's cells and native text side channels, kept together.
#[derive(Debug, Default)]
struct RulePaint {
    cells: Vec<RenderCell>,
    lights_at: Option<usize>,
    /// The title slot's words in `cells` (empty: none drawn).
    title: std::ops::Range<usize>,
    glass: std::ops::Range<usize>,
    /// Where the facts' cells start in `cells`, and where each fact lies in
    /// them past that start's blank (`footer::fact_spans`): what the
    /// pointer can act on, once the write took ([`FactWrite`]).
    facts_at: usize,
    spans: footer::FactSpans,
    /// The facts' native emoji clusters and combining marks at their
    /// columns in `cells` (`footer::SetFacts`), published only where the
    /// write took ([`FooterUndo::publish_text`]).
    clusters: Vec<(usize, Box<str>)>,
    combining: Vec<(usize, Box<[char]>)>,
}

/// What a footer rule is composed from, beside the chips' block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RuleKey {
    shown_gen: u64,
    room: usize,
    rule: RenderCell,
    blank: RenderCell,
    mark_fg: [u8; 3],
    /// The fact the pointer or the keyboard's selection is on, drawn lit
    /// (`claude_lights::WindowLights::lit_fact`): a hover's start and end
    /// compose once each, a frame under it none.
    lit: Option<FactKind>,
}

/// Every part of a chips' block (`claude_lights::Block`) a composition
/// reads: its chips and titles, laid in its cells, how much of the rule it
/// holds and whether its title is a reason — and the fit that laid the block
/// it was copied from or found equal to (`Block::fit`).
#[derive(Debug, Default)]
struct BlockCopy {
    lights: Vec<RenderCell>,
    title: Vec<RenderCell>,
    lean: [Vec<RenderCell>; crate::claude_lights::LEAN_FORMS],
    short_title: Vec<RenderCell>,
    reason: bool,
    holds: usize,
    fit: u64,
}

impl BlockCopy {
    /// Whether `block` is this copy's, cell for cell: known by its fit's
    /// number where it has one and it is this copy's (a block the window's
    /// fit kept, every frame its key stands), else compared part by part —
    /// and an equal block's number taken, so the next frame of it compares
    /// no cell.
    fn is(&mut self, block: &crate::claude_lights::Block) -> bool {
        if block.fit() != 0 && block.fit() == self.fit {
            return true;
        }
        let same = self.lights == block.lights
            && self.title == block.title
            && self.lean == block.lean
            && self.short_title == block.short_title
            && self.reason == block.reason
            && self.holds == block.holds;
        if same {
            self.fit = block.fit();
        }
        same
    }

    fn copy(&mut self, block: &crate::claude_lights::Block) {
        self.lights.clone_from(&block.lights);
        self.title.clone_from(&block.title);
        for (kept, lean) in self.lean.iter_mut().zip(&block.lean) {
            kept.clone_from(lean);
        }
        self.short_title.clone_from(&block.short_title);
        self.reason = block.reason;
        self.holds = block.holds;
        self.fit = block.fit();
    }
}

impl PaneFooter {
    /// Take in the facts the timeline holds (`facts`), the launch card it
    /// keeps for them (`card`), the usage switch and the facts the person
    /// hid (`off`): what the rule shows is made anew only where one of them
    /// moved — a copy of each then, none at rest. The upgrade's words stay
    /// as the last frame laid them.
    fn note_facts(
        &mut self,
        facts: &FooterFacts,
        card: Option<&aterm_phase::LaunchCard>,
        usage: bool,
        off: FactMask,
    ) {
        if let Some((was, kept, switch, hid)) = &self.from
            && was == facts
            && kept.as_ref() == card
            && *switch == usage
            && *hid == off
        {
            return;
        }
        let mut shown = facts.filled_from(card);
        with_usage_switch(&mut shown, usage);
        shown.off = off;
        shown.upgrade = self.shown.upgrade.take();
        self.shown = shown;
        self.shown_gen += 1;
        self.fp = fingerprint(Some(facts), card, usage, off);
        self.from = Some((facts.clone(), card.cloned(), usage, off));
    }

    /// The repaint key's term ([`fingerprint`]) for `session`'s `facts`, the `card` the
    /// timeline keeps for them and the usage switch, where they are the ones this footer
    /// holds: hashed when they were taken in, so the key compares instead of hashing again.
    /// Passing the card while no model or effort is open gives the same term: it fills
    /// nothing else (`filled_model_effort`).
    fn fingerprint_of(
        &self,
        session: u64,
        facts: &FooterFacts,
        card: Option<&aterm_phase::LaunchCard>,
        usage: bool,
        off: FactMask,
    ) -> Option<u64> {
        let (was, kept, switch, hid) = self.from.as_ref()?;
        (self.session == session
            && was == facts
            && kept.as_ref() == card
            && *switch == usage
            && *hid == off)
            .then_some(self.fp)
    }

    /// Take in the live upgrade's note the timeline holds for the pane.
    fn note_upgrade(&mut self, note: Option<&footer::UpgradeNote>) {
        if self.note.as_ref() != note {
            self.note = note.cloned();
        }
    }

    /// Lay in the upgrade's words for a composer that holds a draft (`Some(true)`) or none
    /// (`Some(false)`); `None` lays no words (no note, or the turn holds the move).
    fn lay_upgrade(&mut self, draft: Option<bool>) {
        let words = draft.and_then(|draft| self.note.as_ref()?.words(draft));
        if self.shown.upgrade.as_ref() != words {
            self.shown.upgrade = words.cloned();
            self.shown_gen += 1;
        }
    }

    /// The facts the rule shows.
    fn shown(&self) -> &FooterFacts {
        &self.shown
    }

    /// Whether the facts shown name a Claude Code build newer than 2.1.303
    /// — one whose `/effort` takes the top-effort key's session toggle —
    /// compared once per change of them, never per frame.
    fn key_typed(&mut self) -> bool {
        match self.typed {
            Some((at, typed)) if at == self.shown_gen => typed,
            _ => {
                let typed = self
                    .shown
                    .version
                    .as_deref()
                    .is_some_and(crate::claude_slash::takes_key_toggle);
                self.typed = Some((self.shown_gen, typed));
                typed
            }
        }
    }

    /// The facts the timeline holds for the group in front, as published
    /// (none of the card's fill): what a key's latch was taken against.
    fn published(&self) -> Option<&FooterFacts> {
        self.from.as_ref().map(|(facts, ..)| facts)
    }

    /// The facts' owner where they leave the model or the effort open: the owner a launch
    /// card may be kept for.
    fn card_owner(&self) -> Option<&footer::FactsOwner> {
        let (facts, _, _, _) = self.from.as_ref()?;
        (facts.model_open || facts.effort_open)
            .then_some(facts.owner.as_ref())
            .flatten()
    }

    /// The launch card the timeline keeps for the facts, as this frame read it.
    fn kept_card(&self) -> Option<&aterm_phase::LaunchCard> {
        self.from.as_ref()?.1.as_ref()
    }

    /// The most cells the chips may take in a rule of `room` cells
    /// (`footer::chips_room`), kept while the facts and the room stand.
    fn chips_room(&mut self, room: usize) -> usize {
        match self.chips {
            Some((shown_gen, at, chips)) if shown_gen == self.shown_gen && at == room => chips,
            _ => {
                let chips = footer::chips_room(&self.shown, room);
                self.chips = Some((self.shown_gen, room, chips));
                chips
            }
        }
    }

    /// The composer's top line composed from `key` ([`compose_top`], its
    /// hover title naming the shown facts' version) and its mark's sprite
    /// ([`mark_layer`], its content kept in `marks`): kept while `key` is the
    /// last frame's — a frame at rest composes nothing, seals nothing and
    /// allocates nothing — else composed anew into the kept buffer.
    fn compose_top(&mut self, key: TopKey, marks: &mut MarkContents) -> &ComposedTop {
        if self.top.key != Some(key) {
            // The hover title names the version the shown facts hold, only
            // while the key says they are the program's (`title_gen`).
            let version = (key.title_gen != 0)
                .then_some(self.shown.version.as_deref())
                .flatten();
            compose_top(&key, &mut self.top, version);
            self.top.mark = mark_layer(&key, &self.top, self.session, marks);
            self.top.key = Some(key);
        }
        &self.top
    }

    /// The rule's composition in `room` cells with the chips' `block`, the
    /// rule's own glyph `rule`, the pane's `blank` and the marks' `mark_fg`
    /// (`footer::fit_rule`, [`compose_rule_spans`]): kept while every one of
    /// them, the facts and every cell of the block are the last frame's,
    /// else composed anew; `lit`, the fact drawn lit, is part of the key.
    /// Its cells' count, where the chips start, the run of the rule's own
    /// glyphs in it, and where its facts lie ([`RulePaint::spans`]).
    fn compose(
        &mut self,
        room: usize,
        block: Option<&crate::claude_lights::Block>,
        rule: RenderCell,
        blank: RenderCell,
        mark_fg: [u8; 3],
        lit: Option<FactKind>,
    ) -> Composed {
        let key = RuleKey {
            shown_gen: self.shown_gen,
            room,
            rule,
            blank,
            mark_fg,
            lit,
        };
        let composed = &mut self.rule;
        let same_block = match (&mut composed.block, block) {
            (None, None) => true,
            (Some(kept), Some(block)) => kept.is(block),
            _ => false,
        };
        if composed.key != Some(key) || !same_block {
            #[cfg(test)]
            crate::work_counts::footer_rule_composed();
            let widths = block.map(|b| footer::LightsWidth {
                chips: b.holds,
                title: b.title.len(),
                lean: b.lean.each_ref().map(Vec::len),
                short: b.short_title.len(),
                reason: b.reason,
            });
            let fit = footer::fit_rule(&self.shown, room, widths);
            composed.key = Some(key);
            composed.paint = compose_rule_spans(&fit, block, rule, blank, mark_fg, room, lit);
            match block {
                Some(block) => composed
                    .block
                    .get_or_insert_with(BlockCopy::default)
                    .copy(block),
                None => composed.block = None,
            }
        }
        Composed {
            cells: composed.paint.cells.len(),
            lights_at: composed.paint.lights_at,
            title: composed.paint.title.clone(),
            glass: composed.paint.glass.clone(),
            facts_at: composed.paint.facts_at,
            spans: composed.paint.spans,
        }
    }
}

/// What a frame reads of a pane's composed rule ([`PaneFooter::compose`]):
/// its cells' count, where the chips start, the run of the rule's own
/// glyphs in it, where the facts start and where each fact lies past that
/// start's blank.
#[derive(Debug, Clone)]
struct Composed {
    cells: usize,
    lights_at: Option<usize>,
    /// The title slot's words in the cells (empty: none drawn).
    title: std::ops::Range<usize>,
    glass: std::ops::Range<usize>,
    facts_at: usize,
    spans: footer::FactSpans,
}

/// Whether `text` reads as `cells`' chars (`None`: a row the frame does not hold, read as
/// empty), compared as UTF-8 bytes with no char decoded: every keystroke compares every
/// cell of the pane, and most are one byte.
fn same_text(text: &str, cells: Option<&[RenderCell]>) -> bool {
    let Some(cells) = cells else {
        return text.is_empty();
    };
    let mut rest = text.as_bytes();
    for cell in cells {
        let ch = u32::from(cell.ch);
        if ch < 0x80 {
            match rest.split_first() {
                Some((&b, after)) if u32::from(b) == ch => rest = after,
                _ => return false,
            }
        } else {
            let mut utf8 = [0; 4];
            match rest.strip_prefix(cell.ch.encode_utf8(&mut utf8).as_bytes()) {
                Some(after) => rest = after,
                None => return false,
            }
        }
    }
    rest.is_empty()
}

/// The loop proxy [`post_changed`] wakes through. Installed once at launch; a
/// headless test App installs nothing and a post is then a no-op.
static PROXY: OnceLock<EventLoopProxy<Wake>> = OnceLock::new();

/// Install the proxy the footer wake rides. First install wins.
pub(crate) fn install_proxy(proxy: EventLoopProxy<Wake>) {
    let _ = PROXY.set(proxy);
}

/// A session's footer facts moved: repaint the windows that show it.
pub(crate) fn post_changed(session: u64) {
    if let Some(proxy) = PROXY.get() {
        let _ = proxy.send_event(Wake::ClaudeFooter { session });
    }
}

/// The follow-up reads after every request, measured from it. Two cases need
/// them, and both were seen on a dev build (2026-09-24): Claude Code writes
/// `sessions/<pid>.json` a moment AFTER it takes the foreground, so the read
/// that fires on first sight of `claude` found nothing (now the registry
/// watch's to catch, [`crate::claude_registry`]); and a turn's final
/// transcript row lands as the screen settles, after the last sweep that
/// asked. Bounded — a request buys these two reads and no more. They serve
/// the transcript chips, never the card's gate.
const FOLLOW_UPS: [Duration; 2] = [Duration::from_secs(2), Duration::from_secs(6)];

/// Why one read ran: the `registry-read` dev row's `cause=`, which a live
/// measurement counts (card engine §3.10, §8). `Ask`, `Event` and `Placed`
/// are pushed; `FollowUp` and `Idle` are the two clocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cause {
    /// A request ([`request_footer`]).
    Ask,
    /// One of [`FOLLOW_UPS`].
    FollowUp,
    /// The registry watch saw the process's Claude dir move
    /// ([`Command::RowsMoved`]).
    Event,
    /// A registry watch was newly placed for the process's Claude dir: what
    /// was written before it is read now, what follows by its event.
    Placed,
    /// The idle clock ([`IDLE_RECHECK`], [`WALL_RECHECK`]).
    Idle,
}

impl Cause {
    /// The word the dev row prints.
    const fn word(self) -> &'static str {
        match self {
            Cause::Ask => "ask",
            Cause::FollowUp => "follow-up",
            Cause::Event => "event",
            Cause::Placed => "placed",
            Cause::Idle => "idle",
        }
    }
}

/// How many times one turn places registry watches and reads what they were
/// placed for ([`Resolver::settle`]): a placement can find the next level or
/// the row itself there, which is placed and read in the next round. A
/// Claude dir missing every level from the home inward is three rounds; past
/// the bound the watches stand and the next turn goes on.
const PLACE_ROUNDS: usize = 8;

/// How often a Claude Code session's facts are re-read while nothing asks, and
/// for how long after the last ask. The status sweep asks only while the screen
/// moves, so otherwise a branch switched outside aterm, or an answer written
/// after the screen settled, would stand on an idle footer.
const IDLE_RECHECK: Duration = Duration::from_secs(15);
const WATCH_FOR: Duration = Duration::from_secs(10 * 60);

/// How often a session is re-read past [`WATCH_FOR`] while its footer shows
/// a limit wall or weekly percentage and its Claude Code process lives: a wall whose
/// reset passes, or cached quota that expires, leaves the glass within this.
const WALL_RECHECK: Duration = Duration::from_secs(60);

/// How long the resolver's roster answers from its last read ([`RosterMemo`]).
const ROSTER_RECHECK: Duration = Duration::from_secs(10);

/// The re-read interval of a session last asked for at `asked`: the idle
/// clock within its watch, the wall clock past it.
fn recheck_after(now: std::time::Instant, asked: std::time::Instant) -> Duration {
    if now.saturating_duration_since(asked) < WATCH_FOR {
        IDLE_RECHECK
    } else {
        WALL_RECHECK
    }
}

/// Whether a session last asked for at `asked` is still watched: within
/// [`WATCH_FOR`] of the ask, or past it while its footer shows a wall or weekly percentage
/// and its process is ALIVE (`walled_alive`, asked only then — liveness, not
/// a start time, so it holds on Linux too, and a sessions file a crash left
/// behind cannot keep the clock running for days).
fn still_watched(
    now: std::time::Instant,
    asked: std::time::Instant,
    walled_alive: impl FnOnce() -> bool,
) -> bool {
    now.saturating_duration_since(asked) < WATCH_FOR || walled_alive()
}

/// THE LOG'S WAKE from this thread (session log Phase 3B, rulings S12, A12,
/// A13): the session's watchers are rung under the timeline lock when the
/// transcript's steps moved what a reader of the session's log sees
/// (`SessionTimeline::slog_take_wake`) — no hop through the loop, which a
/// headless test App does not run — and under the `[session_log] clause`
/// switch the loop is told too (`Wake::SessionLogMoved`) so the tab's chrome
/// follows the clause. The loop is told as well when the steps armed the
/// turn tracker's settle, whose tick only the loop keeps. Carried by each
/// request (a field of the [`Job`]), never a static. The default (a test's)
/// has the switch off.
#[derive(Clone, Default)]
pub(crate) struct LogWake {
    clause: bool,
}

impl LogWake {
    /// The wake under the clause switch `clause`.
    pub(crate) fn new(clause: bool) -> LogWake {
        LogWake { clause }
    }

    /// After a batch of transcript steps was applied to session `session`'s
    /// log (its watchers already rung where the batch took the owed wake):
    /// tell the loop when it owes a settle tick or the clause switch is on.
    pub(crate) fn after(&self, session: u64, applied: &crate::session_log::transcript::Applied) {
        if (applied.owed && self.clause) || applied.settle.is_some() {
            post_log_moved(session);
        }
    }
}

/// A session's log moved off the loop in a way the loop must follow (its
/// settle tick, the clause in its chrome).
fn post_log_moved(session: u64) {
    if let Some(proxy) = PROXY.get() {
        let _ = proxy.send_event(Wake::SessionLogMoved { session });
    }
}

/// One resolution: the session, its timeline and the Claude Code process,
/// and what a moved log wakes ([`LogWake`]).
#[derive(Clone)]
struct Job {
    session: u64,
    timeline: Arc<Mutex<SessionTimeline>>,
    pgid: i32,
    wake: LogWake,
}

enum Command {
    Request(Job),
    /// `None` retires the session; a group-specific stop cannot erase a
    /// replacement Claude process that was requested before it arrived.
    Stop {
        session: u64,
        pgid: Option<i32>,
    },
    /// The registry watch saw the Claude dir `dir`'s rows move
    /// ([`crate::claude_registry`]): each process awaited there is read once.
    RowsMoved {
        dir: PathBuf,
    },
    /// The registry watch saw a level outside a `sessions/` move: the turn
    /// places the watches again, and reads only under a Claude dir where
    /// that placed one ([`Resolver::settle`]). Nothing is read for the event
    /// itself.
    LevelMoved,
}

impl From<crate::claude_registry::Told> for Command {
    fn from(told: crate::claude_registry::Told) -> Command {
        match told {
            crate::claude_registry::Told::Rows(dir) => Command::RowsMoved { dir },
            crate::claude_registry::Told::Level => Command::LevelMoved,
        }
    }
}

/// The one background thread footer facts are read on, started on first use
/// and shared by both askers (the status sweep, and `session_program`'s
/// resolver the moment it names Claude Code). An answer for a group that has
/// left the foreground is dropped by `SessionTimeline::set_claude_footer`.
static RESOLVER: Mutex<Option<Sender<Command>>> = Mutex::new(None);

/// Read `pgid`'s footer facts off-thread into `timeline` now, and again at
/// each of [`FOLLOW_UPS`] — and, while no read has found the process's
/// registry row, whenever the registry watch sees its Claude dir move
/// ([`crate::claude_registry`]). A newer request for the same session
/// replaces the follow-ups still owed for it. If the thread cannot be started
/// the footer simply does not appear.
pub(crate) fn request_footer(
    session: u64,
    timeline: &Arc<Mutex<SessionTimeline>>,
    pgid: i32,
    wake: &LogWake,
) {
    if pgid <= 0 {
        return;
    }
    #[cfg(test)]
    if CAPTURED_ASKS.with(|captured| {
        captured
            .borrow_mut()
            .as_mut()
            .map(|asks| asks.push((session, pgid)))
            .is_some()
    }) {
        return;
    }
    let mut slot = RESOLVER.lock().unwrap_or_else(|p| p.into_inner());
    if slot.is_none() {
        let (tx, rx) = channel::<Command>();
        // The registry watch's forwarder tells the resolver through its own
        // sender, so the thread's channel outlives no reader of it: the
        // forwarder goes with the watch, the watch with the thread.
        let rows = tx.clone();
        let sink: crate::claude_registry::Sink =
            Arc::new(move |told| rows.send(Command::from(told)).is_ok());
        let spawned = crate::qos::spawn_lane_thread(
            "aterm-claude-footer",
            crate::qos::Role::Responsive,
            move || {
                // It takes the session timeline lock the UI thread contends,
                // so the lock-holder floor applies.
                run(
                    &rx,
                    crate::claude_registry::RegistryWatch::new(atpkg::dirwatch::Watcher::new, sink),
                );
            },
        );
        match spawned {
            Ok(_) => *slot = Some(tx),
            Err(e) => {
                aterm_log::warn!("claude footer: could not start the resolver: {e}");
                return;
            }
        }
    }
    let job = Job {
        session,
        timeline: Arc::clone(timeline),
        pgid,
        wake: wake.clone(),
    };
    if slot
        .as_ref()
        .is_some_and(|tx| tx.send(Command::Request(job)).is_err())
    {
        // The thread is gone (it ends only when the channel closes, so this is
        // a panic in a read); start a fresh one next time.
        *slot = None;
    }
}

/// Stop refreshing the old foreground group. The sender lock serializes this
/// with requests; the resolver also matches `pgid`, so a late stop for an old
/// group cannot cancel a newer group's watch. It releases the session's usage
/// fold and wall mark with the watch: a Claude Code suspended and brought back
/// folds its transcript again from the start.
pub(crate) fn stop(session: u64, pgid: i32) {
    if pgid <= 0 {
        return;
    }
    send_stop(session, Some(pgid));
}

/// A retired session or a disabled status sweep: drop everything the resolver
/// holds for the session (its watch, owed follow-ups, usage fold and wall mark),
/// so a closed tab's fold goes with the tab.
pub(crate) fn stop_session(session: u64) {
    #[cfg(test)]
    STOPPED_SESSIONS.with(|s| s.borrow_mut().push(session));
    send_stop(session, None);
}

#[cfg(test)]
thread_local! {
    /// The sessions [`stop_session`] was called for on this thread (the
    /// status observer's retirement test reads it).
    pub(crate) static STOPPED_SESSIONS: std::cell::RefCell<Vec<u64>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// While `Some`, [`request_footer`] on this thread keeps each ask here,
    /// `(session, pgid)`, and starts no resolver: a status observer test
    /// reads which asks the sweep made without reading anyone's Claude home.
    pub(crate) static CAPTURED_ASKS: std::cell::RefCell<Option<Vec<(u64, i32)>>> =
        const { std::cell::RefCell::new(None) };
}

fn send_stop(session: u64, pgid: Option<i32>) {
    let mut slot = RESOLVER.lock().unwrap_or_else(|p| p.into_inner());
    if slot
        .as_ref()
        .is_some_and(|tx| tx.send(Command::Stop { session, pgid }).is_err())
    {
        *slot = None;
    }
}

#[derive(Default)]
struct WatchSchedule {
    owed: Vec<(std::time::Instant, Job)>,
    watched: HashMap<u64, Watch>,
}

/// One watched session: its latest job, when it was last ASKED for and last
/// READ — the idle re-reads ([`IDLE_RECHECK`]) run off these — and whether
/// its process is AWAITED: its last read found no registry row, so the
/// registry watch looks out for it ([`crate::claude_registry`]).
struct Watch {
    job: Job,
    asked: std::time::Instant,
    read: std::time::Instant,
    awaiting: bool,
}

/// What one batch of commands asks of the resolver ([`WatchSchedule::accept`]).
#[derive(Default)]
struct Accepted {
    /// The newest ask per session, to read now.
    latest: Vec<Job>,
    /// The watched sessions a stop retired.
    stopped: Vec<u64>,
    /// The Claude dirs whose rows the registry watch saw move.
    moved: HashSet<PathBuf>,
}

impl WatchSchedule {
    /// Drop the watches whose time is up: [`WATCH_FOR`] after the last ask,
    /// unless `walled_alive` — asked only then — says the session's footer
    /// shows a wall or weekly percentage and its process lives ([`still_watched`]).
    fn prune(&mut self, now: std::time::Instant, mut walled_alive: impl FnMut(u64, &Job) -> bool) {
        self.watched.retain(|session, watch| {
            still_watched(now, watch.asked, || walled_alive(*session, &watch.job))
        });
    }

    /// When the next read is owed on a clock: a follow-up or an idle
    /// re-read. An awaited process is owed nothing on a clock beyond those
    /// (no first-row ladder): a read sooner is the registry watch's.
    fn next(&self, now: std::time::Instant) -> Option<std::time::Instant> {
        self.owed
            .iter()
            .map(|(at, _)| *at)
            .chain(
                self.watched
                    .values()
                    .map(|watch| watch.read + recheck_after(now, watch.asked)),
            )
            .min()
    }

    fn matches(job: &Job, session: u64, pgid: Option<i32>) -> bool {
        job.session == session && pgid.is_none_or(|pgid| job.pgid == pgid)
    }

    /// Collapse queued requests while respecting a stop's position in the
    /// channel, and gather the Claude dirs the registry watch told.
    fn accept(&mut self, commands: impl IntoIterator<Item = Command>) -> Accepted {
        let mut latest: HashMap<u64, Job> = HashMap::new();
        let mut accepted = Accepted::default();
        for command in commands {
            match command {
                Command::Request(job) => {
                    latest.insert(job.session, job);
                }
                Command::Stop { session, pgid } => {
                    if latest
                        .get(&session)
                        .is_some_and(|job| Self::matches(job, session, pgid))
                    {
                        latest.remove(&session);
                    }
                    if self.stop(session, pgid) {
                        accepted.stopped.push(session);
                    }
                }
                Command::RowsMoved { dir } => {
                    accepted.moved.insert(dir);
                }
                // The turn's settle is what it asks for.
                Command::LevelMoved => {}
            }
        }
        accepted.latest = latest.into_values().collect();
        accepted
    }

    fn stop(&mut self, session: u64, pgid: Option<i32>) -> bool {
        if !self
            .watched
            .get(&session)
            .is_some_and(|watch| Self::matches(&watch.job, session, pgid))
        {
            return false;
        }
        self.watched.remove(&session);
        self.owed.retain(|(_, job)| job.session != session);
        true
    }

    /// Watch `job`, asked for and read at `asked`, that read having found
    /// its process's registry row (`row`) or not.
    fn watch(&mut self, job: Job, asked: std::time::Instant, row: bool) {
        self.owed
            .retain(|(_, pending)| pending.session != job.session);
        for delay in FOLLOW_UPS {
            self.owed.push((asked + delay, job.clone()));
        }
        self.watched.insert(
            job.session,
            Watch {
                job,
                asked,
                read: asked,
                awaiting: !row,
            },
        );
    }

    fn due_followups(&mut self, now: std::time::Instant) -> Vec<Job> {
        let (due, later): (Vec<_>, Vec<_>) = self.owed.drain(..).partition(|(at, _)| *at <= now);
        self.owed = later;
        due.into_iter().map(|(_, job)| job).collect()
    }

    fn due_idle(&self, now: std::time::Instant) -> Option<Job> {
        self.watched
            .values()
            .find(|watch| now >= watch.read + recheck_after(now, watch.asked))
            .map(|watch| watch.job.clone())
    }

    /// The job of each session in `sessions` whose process is awaited, in
    /// session order.
    fn awaited_in(&self, sessions: &HashSet<u64>) -> Vec<Job> {
        let mut jobs: Vec<Job> = sessions
            .iter()
            .filter_map(|session| self.watched.get(session))
            .filter(|watch| watch.awaiting)
            .map(|watch| watch.job.clone())
            .collect();
        jobs.sort_by_key(|job| job.session);
        jobs
    }

    /// `session` was read at `at`, the read having found its process's
    /// registry row (`row`) or not.
    fn read(&mut self, session: u64, at: std::time::Instant, row: bool) {
        if let Some(watch) = self.watched.get_mut(&session) {
            watch.read = at;
            watch.awaiting = !row;
        }
    }

    /// Whether `job`'s process is awaited as a read of it starts: its watch
    /// is for another process or none (a process never read is awaited), or
    /// its last read found no row.
    fn awaits(&self, job: &Job) -> bool {
        self.watched
            .get(&job.session)
            .is_none_or(|watch| watch.job.pgid != job.pgid || watch.awaiting)
    }

    /// One pass of the resolver thread ([`Resolver::turn`]) over the asks it
    /// just took (`latest`) and what is owed: each ask is read and watched,
    /// then each follow-up due at the pass's instant, then each awaited
    /// process of a session in `moved` (its Claude dir's rows moved) that
    /// this pass has not read yet — once, however many events told it —
    /// then each idle re-read due then. `read` reads a job for a [`Cause`],
    /// told whether its process was awaited ([`Self::awaits`]), and says
    /// whether it found the process's registry row ([`Found::row`]); `clock`
    /// is the time, asked as each read ends. Returns the pass's instant, and
    /// LEAVES NOTHING OWED AT OR BEFORE IT: every read moves its own clock
    /// past the instant, so the thread's next sleep ([`Self::next`]) is
    /// never zero.
    fn serve(
        &mut self,
        latest: Vec<Job>,
        moved: &HashSet<u64>,
        mut read: impl FnMut(&Job, Cause, bool) -> bool,
        mut clock: impl FnMut() -> std::time::Instant,
    ) -> std::time::Instant {
        let mut done: HashSet<u64> = HashSet::new();
        for job in latest {
            let row = read(&job, Cause::Ask, self.awaits(&job));
            done.insert(job.session);
            self.watch(job, clock(), row);
        }
        let now = clock();
        for job in self.due_followups(now) {
            let row = read(&job, Cause::FollowUp, self.awaits(&job));
            done.insert(job.session);
            self.read(job.session, clock(), row);
        }
        for job in self.awaited_in(moved) {
            if done.insert(job.session) {
                let row = read(&job, Cause::Event, true);
                self.read(job.session, clock(), row);
            }
        }
        while let Some(job) = self.due_idle(now) {
            let row = read(&job, Cause::Idle, self.awaits(&job));
            self.read(job.session, clock(), row);
        }
        now
    }
}

/// How many Claude Code processes the resolver keeps transcript caches for
/// at once ([`ProcessTails`]).
const MAX_KEPT: usize = 64;

/// How often the resolver asks the kernel whether the processes it keeps
/// caches for, and no longer watches, still live ([`ProcessTails`]).
const SWEEP_EVERY: Duration = Duration::from_secs(30);

/// The transcript caches (`footer::TailCache`: what the process decided, whether its model
/// is pinned) of the Claude Code processes the resolver has read, keyed by process (group
/// and kernel start), not by watch: a watch lapses after [`WATCH_FOR`] and ends at every
/// stop (ctrl-z, another program in front), while the process and its pin outlive both,
/// and a fresh cache cannot see a pin made before a `/resume`. A cache stays until no
/// process of its group born then remains or, with no readable start, until its session's
/// watch no longer reads that group; at most [`MAX_KEPT`], least recently read out first.
#[derive(Default)]
struct ProcessTails {
    kept: HashMap<(i32, Option<u64>), KeptTail>,
    /// When the kernel was last asked ([`SWEEP_EVERY`]).
    swept: Option<std::time::Instant>,
}

/// One process's cache in [`ProcessTails`].
struct KeptTail {
    /// The session the process was last read for.
    session: u64,
    cache: footer::TailCache,
    /// When it was last read.
    used: std::time::Instant,
}

impl ProcessTails {
    /// The cache of the process (`pgid`, `started`), read now for `session`.
    fn cache(
        &mut self,
        session: u64,
        pgid: i32,
        started: Option<u64>,
        now: std::time::Instant,
    ) -> &mut footer::TailCache {
        let key = (pgid, started);
        if !self.kept.contains_key(&key)
            && self.kept.len() >= MAX_KEPT
            && let Some(oldest) = self
                .kept
                .iter()
                .min_by_key(|(_, kept)| kept.used)
                .map(|(key, _)| *key)
        {
            self.kept.remove(&oldest);
        }
        let kept = self.kept.entry(key).or_insert_with(|| KeptTail {
            session,
            cache: footer::TailCache::default(),
            used: now,
        });
        kept.session = session;
        kept.used = now;
        &mut kept.cache
    }

    /// Drop the caches whose process has left for good, as `schedule` now
    /// watches: one with no start time once its session's watch no longer
    /// reads its group; one with a start, at most every [`SWEEP_EVERY`],
    /// once `alive` (the kernel, [`birth_seconds`]) no longer finds a process
    /// of its group born then. A lapsed or stopped watch alone drops none.
    fn retain(
        &mut self,
        schedule: &WatchSchedule,
        alive: impl Fn(i32, u64) -> bool,
        now: std::time::Instant,
    ) {
        let sweep = self
            .swept
            .is_none_or(|at| now.saturating_duration_since(at) >= SWEEP_EVERY);
        if sweep {
            self.swept = Some(now);
        }
        self.kept.retain(|&(pgid, started), kept| {
            let watched = schedule
                .watched
                .get(&kept.session)
                .is_some_and(|watch| watch.job.pgid == pgid);
            watched || started.is_some_and(|started| !sweep || alive(pgid, started))
        });
    }
}

/// The resolver thread: each request is read at once and again at each
/// follow-up, a process whose registry row no read has found yet whenever
/// the registry watch sees its Claude dir move, and the thread sleeps exactly
/// until the next clock read is owed or a command comes in. Each wake is one
/// turn ([`Resolver::turn`]), which leaves nothing owed at its instant.
fn run(rx: &std::sync::mpsc::Receiver<Command>, registry: crate::claude_registry::RegistryWatch) {
    let mut resolver = Resolver::new(registry, LIVE);
    loop {
        let now = std::time::Instant::now();
        resolver.retain(now);
        let first = match resolver.next(now) {
            Some(at) => match rx.recv_timeout(at.saturating_duration_since(now)) {
                Ok(job) => Some(job),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            },
            None => match rx.recv() {
                Ok(job) => Some(job),
                Err(_) => return,
            },
        };
        // Everything already queued, collapsed to the newest ask per session:
        // a slow read (a stalled mount under one session's cwd) is paid once,
        // not once per ask that piled up behind it. A stop in the batch wins
        // over an ask for the same group queued before it, and the registry
        // watch's events are one read per awaited process.
        resolver.turn(
            first
                .into_iter()
                .chain(std::iter::from_fn(|| rx.try_recv().ok())),
            std::time::Instant::now,
        );
    }
}

/// The resolver thread's whole state ([`run`]): its schedule; per session,
/// the process known for it, its usage fold and its wall mark; per process,
/// its transcript tail; the one roster; the registry watch over the rows it
/// awaits; and what a read asks outside Claude Code's own files ([`Asks`]).
/// A test drives it turn by turn on a clock it holds still.
struct Resolver {
    /// Per session: its latest job, when it was last ASKED for, when it was
    /// last READ — the idle re-reads ([`IDLE_RECHECK`]) run off this — and
    /// whether its process is awaited ([`Watch`]).
    schedule: WatchSchedule,
    known: HashMap<u64, Identity>,
    /// Per PROCESS: its transcript-tail memo, kept while the process lives.
    tails: ProcessTails,
    /// Per watched session: its usage fold, released with the watch.
    folds: HashMap<u64, footer::FooterCache>,
    /// Sessions whose footer has a wall or weekly percentage to expire on the slow clock.
    walled: HashSet<u64>,
    /// The accounts roster, for every session's label.
    roster: RosterMemo,
    registry: crate::claude_registry::RegistryWatch,
    asks: Asks,
}

impl Resolver {
    fn new(registry: crate::claude_registry::RegistryWatch, asks: Asks) -> Resolver {
        Resolver {
            schedule: WatchSchedule::default(),
            known: HashMap::new(),
            tails: ProcessTails::default(),
            folds: HashMap::new(),
            walled: HashSet::new(),
            roster: RosterMemo::default(),
            registry,
            asks,
        }
    }

    /// Before the thread sleeps at `now`: the watches whose time is up go,
    /// with what the reads hold for their sessions, the tails of processes
    /// gone, and the registry watches nothing awaits now.
    fn retain(&mut self, now: std::time::Instant) {
        let walled = &self.walled;
        self.schedule.prune(now, |session, job| {
            walled.contains(&session)
                && u32::try_from(job.pgid).is_ok_and(crate::control_auth::pid_alive)
        });
        let watched = &self.schedule.watched;
        self.known
            .retain(|session, _| watched.contains_key(session));
        self.folds
            .retain(|session, _| watched.contains_key(session));
        self.walled.retain(|session| watched.contains_key(session));
        self.tails.retain(
            &self.schedule,
            |pgid, started| birth_seconds(pgid) == Some(started),
            now,
        );
        self.registry.let_go(&self.awaited());
    }

    /// When the next clock read is owed ([`WatchSchedule::next`]).
    fn next(&self, now: std::time::Instant) -> Option<std::time::Instant> {
        self.schedule.next(now)
    }

    /// One turn over `commands`, the batch the thread just took: a stop
    /// forgets its session ([`forget`]; the process's tail stays while it
    /// lives, since a stopped group may come back with its pin,
    /// [`ProcessTails`]); the asks, the follow-ups due, the processes whose
    /// rows moved and the idle re-reads due are read
    /// ([`WatchSchedule::serve`]); and the registry watch is settled on what
    /// is awaited after them ([`Self::settle`]). `clock` is the time, asked
    /// as each read ends. Returns the pass's instant.
    fn turn(
        &mut self,
        commands: impl IntoIterator<Item = Command>,
        mut clock: impl FnMut() -> std::time::Instant,
    ) -> std::time::Instant {
        let now = self.serve_commands(commands, &mut clock);
        self.settle(&mut clock);
        now
    }

    /// [`Self::turn`]'s reads, before its registry watch is settled.
    fn serve_commands(
        &mut self,
        commands: impl IntoIterator<Item = Command>,
        clock: &mut impl FnMut() -> std::time::Instant,
    ) -> std::time::Instant {
        let accepted = self.schedule.accept(commands);
        for session in accepted.stopped {
            forget(session, &mut self.known, &mut self.folds, &mut self.walled);
        }
        let moved = self.sessions_under(&accepted.moved);
        let mut reads = Reads {
            known: &mut self.known,
            tails: &mut self.tails,
            folds: &mut self.folds,
            walled: &mut self.walled,
            roster: &mut self.roster,
            asks: self.asks,
        };
        self.schedule.serve(
            accepted.latest,
            &moved,
            |job, cause, awaited| reads.resolve(job, cause, awaited),
            clock,
        )
    }

    /// Place the registry watches the awaited processes need
    /// ([`crate::claude_registry::RegistryWatch::reconcile`]), then read
    /// each process awaited under a Claude dir where one was newly placed
    /// ([`Cause::Placed`]): what was written before the watch is found by
    /// that read, what follows by its event. Again while a round places a
    /// watch (a read can find the row's file there, whose watch is placed in
    /// the next), at most [`PLACE_ROUNDS`] times.
    fn settle(&mut self, clock: &mut impl FnMut() -> std::time::Instant) {
        for _ in 0..PLACE_ROUNDS {
            let fresh: HashSet<PathBuf> = self
                .registry
                .reconcile(&self.awaited())
                .into_iter()
                .collect();
            if fresh.is_empty() {
                return;
            }
            let jobs = self.schedule.awaited_in(&self.sessions_under(&fresh));
            let mut reads = Reads {
                known: &mut self.known,
                tails: &mut self.tails,
                folds: &mut self.folds,
                walled: &mut self.walled,
                roster: &mut self.roster,
                asks: self.asks,
            };
            for job in jobs {
                let row = reads.resolve(&job, Cause::Placed, true);
                self.schedule.read(job.session, clock(), row);
            }
        }
    }

    /// The awaited processes whose Claude dir a read has resolved: what the
    /// registry watch looks out for. One whose dir is not known yet (the
    /// first ask fell in a `/bin/sh` shim's window, whose environment macOS
    /// hides) is not watched; the re-ask at its drawer image's exec edge
    /// reads it again (`StatusObserver::request_footer_if_due`).
    fn awaited(&self) -> Vec<crate::claude_registry::Awaited> {
        let mut awaited: Vec<_> = self
            .schedule
            .watched
            .iter()
            .filter(|(_, watch)| watch.awaiting)
            .filter_map(|(session, watch)| {
                let identity = self
                    .known
                    .get(session)
                    .filter(|identity| identity.pid == watch.job.pgid)?;
                Some(crate::claude_registry::Awaited {
                    dir: identity.dir.clone()?,
                    pid: u32::try_from(watch.job.pgid).ok()?,
                })
            })
            .collect();
        awaited.sort_by(|a, b| (&a.dir, a.pid).cmp(&(&b.dir, b.pid)));
        awaited
    }

    /// The watched sessions whose process's Claude dir is one of `dirs`.
    fn sessions_under(&self, dirs: &HashSet<PathBuf>) -> HashSet<u64> {
        if dirs.is_empty() {
            return HashSet::new();
        }
        self.schedule
            .watched
            .iter()
            .filter(|(session, watch)| {
                self.known.get(session).is_some_and(|identity| {
                    identity.pid == watch.job.pgid
                        && identity.dir.as_ref().is_some_and(|dir| dirs.contains(dir))
                })
            })
            .map(|(session, _)| *session)
            .collect()
    }
}

/// The resolver's state one read touches: per session, the process known
/// for it, its usage fold and its wall mark; per process, its tail; the one
/// roster; and what a read asks outside Claude Code's own files.
struct Reads<'a> {
    known: &'a mut HashMap<u64, Identity>,
    tails: &'a mut ProcessTails,
    folds: &'a mut HashMap<u64, footer::FooterCache>,
    walled: &'a mut HashSet<u64>,
    roster: &'a mut RosterMemo,
    asks: Asks,
}

impl Reads<'_> {
    /// Read `job`'s session for `cause` and keep it in the walled set
    /// exactly while its published footer has a wall or weekly percentage. Whether the
    /// read found the process's registry row (whether it is still awaited).
    /// A read of an `awaited` process is a `registry-read` dev row
    /// ([`crate::holder_image::DEV_ROWS`]), `open=1` where the read opened
    /// its registry row's path (its Claude dir was known): what a live
    /// measurement counts, by cause, until the row is found. A read of a
    /// process whose row was found is none, so the steady re-reads of a long
    /// session never crowd its timeline.
    fn resolve(&mut self, job: &Job, cause: Cause, awaited: bool) -> bool {
        let found = resolve_and_post(
            self.known,
            self.tails,
            self.folds.entry(job.session).or_default(),
            self.roster,
            job,
            self.asks,
        );
        note_wall(self.walled, job.session, found.timed);
        if crate::holder_image::DEV_ROWS && awaited {
            let open = self
                .known
                .get(&job.session)
                .is_some_and(|identity| identity.pid == job.pgid);
            job.timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .record(
                    REGISTRY_READ,
                    format!(
                        "cause={} pid={} open={} row={} {}",
                        cause.word(),
                        job.pgid,
                        u8::from(open),
                        u8::from(found.row),
                        crate::holder_image::dev_tail()
                    ),
                );
        }
        found.row
    }
}

/// What the footer's fact menus act on and its traffic card draws, published
/// beside the facts (`SessionTimeline::set_claude_side`), never in them: a
/// new side repaints nothing, and no frame reads it. Shared read-only; a
/// reader that holds one knows it moved by `Arc::ptr_eq`.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct FactSide {
    /// Where the session's Bash tool stands, else its launch directory,
    /// absolute: the directory the path fact shows.
    pub(crate) cwd: Option<PathBuf>,
    /// Its repository's root: where `footer::read_git_head`'s climb found
    /// `.git`.
    pub(crate) repo_root: Option<PathBuf>,
    /// The session's tokens over time, once its usage fold caught up
    /// (`footer::FooterCache::traffic`); `None` for a program with no fold.
    pub(crate) traffic: Option<Arc<aterm_agent::harness::session_usage::TrafficView>>,
    /// The fold and generation `traffic` was built at
    /// (`footer::FooterCache::traffic_generation`).
    built_at: Option<(u64, u64)>,
    /// The session's agents and workflow runs with their calls in flight,
    /// once its usage fold caught up (`footer::FooterCache::flights`): what
    /// the agents panel's dots map their rows to.
    pub(crate) flights: Option<Arc<aterm_agent::harness::flight::FlightTable>>,
    /// The fold and generation `flights` was built at
    /// (`footer::FooterCache::flight_generation`).
    flights_at: Option<(u64, u64)>,
}

#[cfg(test)]
impl FactSide {
    /// A side at `cwd`, in the repository at `repo_root`, with no traffic:
    /// what a test publishes beside its facts.
    pub(crate) fn at(cwd: Option<PathBuf>, repo_root: Option<PathBuf>) -> Self {
        Self {
            cwd,
            repo_root,
            traffic: None,
            built_at: None,
            flights: None,
            flights_at: None,
        }
    }

    /// This side with `flights` beside it, as the resolver publishes a
    /// session's subagents' calls in flight.
    pub(crate) fn with_flights(
        mut self,
        flights: aterm_agent::harness::flight::FlightTable,
    ) -> Self {
        self.flights = Some(Arc::new(flights));
        self
    }

    /// This side with `traffic` beside it, as the resolver publishes a
    /// session's tokens over time. Built where its one caller, a unix
    /// test, is.
    #[cfg(unix)]
    pub(crate) fn with_traffic(
        mut self,
        traffic: aterm_agent::harness::session_usage::TrafficView,
    ) -> Self {
        self.traffic = Some(Arc::new(traffic));
        self
    }
}

/// What a [`side_after`] read found of the fold: the traffic's generation
/// and the flight table's, each with what builds it.
struct SideFold<T, F> {
    generation: Option<(u64, u64)>,
    traffic: T,
    flight_generation: Option<(u64, u64)>,
    flights: F,
}

/// The side for a read that found `cwd` and `repo_root` and a fold whose
/// parts are at `fold`'s generations: `held` itself (the same `Arc`) when
/// none of them moved, else a new one, each of the traffic and the flight
/// table `held`'s own where its generation did not move and built anew where
/// it did. Pure.
fn side_after<T, F>(
    held: Option<&Arc<FactSide>>,
    cwd: Option<PathBuf>,
    repo_root: Option<PathBuf>,
    fold: SideFold<T, F>,
) -> Arc<FactSide>
where
    T: FnOnce() -> Option<aterm_agent::harness::session_usage::TrafficView>,
    F: FnOnce() -> Option<aterm_agent::harness::flight::FlightTable>,
{
    let SideFold {
        generation,
        traffic,
        flight_generation,
        flights,
    } = fold;
    if let Some(held) = held
        && held.cwd == cwd
        && held.repo_root == repo_root
        && held.built_at == generation
        && held.flights_at == flight_generation
    {
        return Arc::clone(held);
    }
    let traffic = match held {
        Some(held) if held.built_at == generation => held.traffic.clone(),
        _ => generation.and_then(|_| traffic()).map(Arc::new),
    };
    let flights = match held {
        Some(held) if held.flights_at == flight_generation => held.flights.clone(),
        _ => flight_generation.and_then(|_| flights()).map(Arc::new),
    };
    Arc::new(FactSide {
        cwd,
        repo_root,
        traffic,
        built_at: generation,
        flights,
        flights_at: flight_generation,
    })
}

/// The dev timeline row of one read of an awaited process ([`Reads::resolve`]).
pub(crate) const REGISTRY_READ: &str = "registry-read";

/// What one read found, for the resolver's own state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Found {
    /// The process's registry row answered the read ([`footer::read_pid`]
    /// gave facts): the process is awaited until a read it is true for.
    row: bool,
    /// Its published wall or weekly percentage still needs clock-driven expiry.
    timed: bool,
}

/// Keep the slow clock while a footer value needs time-driven expiry.
fn note_wall(walled: &mut HashSet<u64>, session: u64, timed: bool) {
    if timed {
        walled.insert(session);
    } else {
        walled.remove(&session);
    }
}

/// Forget what the resolver's reads hold for `session` (its process, its usage fold and its
/// wall mark) once the schedule has stopped its watch ([`stop_session`] from a closed tab,
/// [`stop`] for a group that left). The process's tail is not the session's:
/// [`ProcessTails`] keeps it while the process lives.
fn forget(
    session: u64,
    known: &mut HashMap<u64, Identity>,
    folds: &mut HashMap<u64, footer::FooterCache>,
    walled: &mut HashSet<u64>,
) {
    known.remove(&session);
    folds.remove(&session);
    walled.remove(&session);
}

/// What the resolver knows about one Claude Code process for its whole life: its kernel
/// start (the pid-reuse guard), the Claude Code directory its exec environment named (one
/// `KERN_PROCARGS2` read per process), and what its command line and environment say
/// ([`footer::launch_facts_of`], [`login_of_args`]). Those are read once the registry names
/// an image (`startedAt`) and again for each new one: an in-place exec keeps the pid and
/// start but not the argv or environment, so an earlier read may be a launcher's.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Identity {
    pid: i32,
    started: Option<u64>,
    dir: Option<PathBuf>,
    launch: LaunchFacts,
    /// The image (`startedAt`) `launch`, `login` and `argv` were read for;
    /// `None` until a registry entry named one and that read answered.
    launch_for: Option<Option<u64>>,
    /// What the process's exec environment says of its login; `None` where
    /// it could not be read or reads empty. Read with `launch`.
    login: Option<accounts::ProcessLogin>,
    /// The process's argv, `argv[0]` first, from the same `KERN_PROCARGS2` read as `launch`
    /// (empty where it could not be read): the launch flags the frozen-program remedy's
    /// resume line carries ([`aterm_agent::harness::resume::of_entry`]).
    argv: Vec<String>,
    /// The head of the task a `spawn agent=` worker was started with, read
    /// once per image from the file its exec environment names
    /// (`ATERM_TASK_FILE`, [`crate::survey::task_prompt_of_env`]): the
    /// survey's `prompt=` for a tasked session.
    task: Option<String>,
    /// The image and settings directory the stored-login verdict belongs to.
    login_checked_for: Option<(Option<u64>, PathBuf)>,
    /// Settings, provider and profile-store evidence leave the stored login selected.
    stored_login: bool,
    /// Exact captured exec environment, fingerprinted without retaining credentials.
    env_stamp: Option<[u8; 32]>,
}

/// The kernel's start time of `pid`, unix seconds, where this platform can
/// read it for a process of ours.
fn birth_seconds(pid: i32) -> Option<u64> {
    #[cfg(unix)]
    {
        crate::seamless::read_process_birth(pid).map(|birth| birth.seconds)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        None
    }
}

/// `pid`'s argv and environment at exec, when this platform lets it be read.
fn process_args(pid: i32) -> Option<atpkg::caller_shell::ProcArgs> {
    u32::try_from(pid)
        .ok()
        .and_then(atpkg::caller_shell::process_args)
}

/// `pid`'s Claude Code directory, from its own environment at exec (`args`:
/// its `CLAUDE_CONFIG_DIR`, and the variable its `os.homedir()` reads,
/// [`accounts::HOMEDIR_ENV`], `USERPROFILE` on Windows) and, for a relative
/// `CLAUDE_CONFIG_DIR` (empty included), the directory it runs in, by the
/// one resolver ([`footer::claude_dir_of`]). An unread environment names no
/// directory, so the next request can resolve it again.
pub(crate) fn claude_dir_of_args(
    args: Option<&atpkg::caller_shell::ProcArgs>,
    pid: i32,
) -> Option<PathBuf> {
    match args {
        Some(args) => footer::claude_dir_of(
            args.env_var(accounts::HOME_ENV),
            args.env_var(accounts::HOMEDIR_ENV),
            || {
                u32::try_from(pid)
                    .ok()
                    .and_then(aterm_sysprobe::process_cwd)
            },
        ),
        None => None,
    }
}

/// What a process's environment at exec says of its login
/// ([`accounts::ProcessLogin::of_exec_env`]). `None` where it was not read,
/// and where it reads empty.
fn login_of_args(args: Option<&atpkg::caller_shell::ProcArgs>) -> Option<accounts::ProcessLogin> {
    accounts::ProcessLogin::of_exec_env(&args?.env)
}

/// Reuse an image's auth verdict only for its exact captured environment. Lengths
/// separate entries, and no raw credential value is retained in the identity cache.
fn exec_env_stamp(env: &[String]) -> [u8; 32] {
    let mut digest = aterm_digest::Sha256::new();
    for entry in env {
        digest.update(entry.len().to_le_bytes());
        digest.update(entry.as_bytes());
    }
    digest.finalize()
}

/// The accounts roster as the resolver last read it, for the footer's label
/// ([`accounts::footer_label`]). The resolver thread's own, so no lock.
#[derive(Default)]
struct RosterMemo {
    /// When `load` was last asked.
    read: Option<std::time::Instant>,
    /// What it answered. A lone row can bind a named login's profile identity.
    roster: Option<accounts::Roster>,
}

impl RosterMemo {
    /// The roster; `load` is asked at most every
    /// [`ROSTER_RECHECK`]. A load that fails is no roster until the next.
    fn at(
        &mut self,
        now: std::time::Instant,
        load: impl FnOnce() -> Option<accounts::Roster>,
    ) -> Option<&accounts::Roster> {
        if self
            .read
            .is_none_or(|at| now.saturating_duration_since(at) >= ROSTER_RECHECK)
        {
            self.read = Some(now);
            self.roster = load();
        }
        self.roster.as_ref()
    }
}

/// The roster beside the `aterm.toml` this window loads
/// ([`accounts::default_roster_path`]): one bounded read, through this
/// machine's logins (`accounts::Store::bound`). `None` where there is no
/// config path and where the file is refused; a missing file is the empty
/// roster. Under test it reads nothing: a test hands [`RosterMemo`]
/// its roster.
fn live_roster() -> Option<accounts::Roster> {
    if cfg!(test) {
        return None;
    }
    let roster = accounts::Roster::load(&accounts::default_roster_path()?).ok()?;
    // A missing/unconfigured roster needs no machine binding store. In particular,
    // the presentation still works while the harness host itself is switched off.
    if roster.rows.is_empty() {
        return Some(roster);
    }
    // Through this machine's logins: a renewed login is its row's.
    let now = aterm_time::unix_secs().and_then(|now| i64::try_from(now).ok())?;
    accounts::Store::at(&crate::harness_host::harness_state()?)
        .bound(&roster, now)
        .ok()
}

/// What a read asks outside Claude Code's own files: a process's exec read,
/// the accounts roster and the person's home (the resume line's `HOME` is
/// judged against its `.claude`). The resolver asks the machine ([`LIVE`]); a
/// test answers for it, so it never resolves the person's own `~/.claude`.
#[derive(Clone, Copy)]
struct Asks {
    args: fn(i32) -> Option<atpkg::caller_shell::ProcArgs>,
    roster: fn() -> Option<accounts::Roster>,
    home: fn() -> Option<PathBuf>,
    stored_login: fn(u32, &[String], &str) -> bool,
}

/// [`process_args`], [`live_roster`] and [`aterm_types::dirs::home_dir`].
const LIVE: Asks = Asks {
    args: process_args,
    roster: live_roster,
    home: aterm_types::dirs::home_dir,
    stored_login: crate::harness_netprobe::stored_login_of_agent,
};

/// [`resolve_into`], then wake the loop when the facts moved — and, when the
/// moved facts carry a repository read the kernel refused with `EPERM`, post that
/// too, so the App can raise its consent attention for this session
/// (`Wake::ProtectedRead`; the App decides whether the path is protected).
/// Whether the read found the process's registry row, and whether the
/// session's published footer now shows a limit wall.
fn resolve_and_post(
    known: &mut HashMap<u64, Identity>,
    tails: &mut ProcessTails,
    fold: &mut footer::FooterCache,
    roster: &mut RosterMemo,
    job: &Job,
    asks: Asks,
) -> Found {
    let (session, timeline, pgid) = (job.session, &job.timeline, job.pgid);
    let started = birth_seconds(pgid);
    let cached = known
        .get(&session)
        .is_some_and(|id| id.pid == pgid && id.started.is_some() && id.started == started)
        .then(|| known.remove(&session))
        .flatten();
    let mut identity = cached.unwrap_or_else(|| Identity {
        pid: pgid,
        started,
        dir: None,
        launch: LaunchFacts::default(),
        launch_for: None,
        login: None,
        argv: Vec::new(),
        task: None,
        login_checked_for: None,
        stored_login: false,
        env_stamp: None,
    });
    // A readable launcher's directory is only a candidate until its registry
    // names an image whose exec context was read. Keep it for watches, but
    // resolve it again after an in-place exec may have changed HOME/config.
    if identity.launch_for.is_none() {
        identity.dir = claude_dir_of_args((asks.args)(pgid).as_ref(), pgid);
    }
    let tail = tails.cache(
        session,
        identity.pid,
        identity.started,
        std::time::Instant::now(),
    );
    let (moved, found, logged) = resolve_into(timeline, &mut identity, tail, fold, roster, asks);
    job.wake.after(session, &logged);
    // No directory is not remembered: a launch through the atpkg twin is
    // `/bin/sh` for its first milliseconds, whose environment macOS hides,
    // and the `exec` into Claude Code keeps both the pid and the start time
    // — a cached `None` would outlive the shell and leave that process with
    // no footer for its whole life.
    if identity.dir.is_some() {
        known.insert(session, identity);
    }
    if let Some(denied) = moved {
        post_changed(session);
        if let Some(path) = denied
            && let Some(proxy) = PROXY.get()
        {
            let _ = proxy.send_event(Wake::ProtectedRead { session, path });
        }
    }
    found
}

/// A zone's offset from UTC at the instant `unix` (`None`: the local zone), by
/// `aterm_agent::supervise::limit::offset_at`, `None` for a zone this machine does not know
/// (the wall then shows nothing, never a guess). Cached because each is a `date` run and a
/// standing wall asks about the same few instants on every read: at most 64 answers, each
/// for an hour (the local zone can change under a running window). Resolver thread only.
fn offset_at_cached(zone: Option<&str>, unix: i64) -> Option<i64> {
    static ANSWERS: Mutex<Vec<OffsetAnswer>> = Mutex::new(Vec::new());
    let mut answers = ANSWERS.lock().unwrap_or_else(|p| p.into_inner());
    cached_offset(
        &mut answers,
        zone,
        unix,
        std::time::Instant::now(),
        aterm_agent::supervise::limit::offset_at,
    )
}

/// One remembered offset: the zone (`None`: local), the instant, when it
/// was read, and the offset.
type OffsetAnswer = (Option<String>, i64, std::time::Instant, i64);

/// [`offset_at_cached`] over `answers` at `now`, asking `read` on a miss. Only an answer is
/// kept: a read that failed (a `date` that could not be spawned under process pressure) is
/// asked again next time rather than hiding a standing wall for the hour an answer is kept.
fn cached_offset(
    answers: &mut Vec<OffsetAnswer>,
    zone: Option<&str>,
    unix: i64,
    now: std::time::Instant,
    read: impl FnOnce(Option<&str>, i64) -> Option<i64>,
) -> Option<i64> {
    const FRESH: Duration = Duration::from_secs(3600);
    const ANSWERS_MAX: usize = 64;
    if let Some(hit) = answers
        .iter()
        .find(|(z, t, at, _)| {
            z.as_deref() == zone && *t == unix && now.saturating_duration_since(*at) < FRESH
        })
        .map(|(_, _, _, offset)| *offset)
    {
        return Some(hit);
    }
    let offset = read(zone, unix)?;
    answers.retain(|(z, t, _, _)| !(z.as_deref() == zone && *t == unix));
    if answers.len() >= ANSWERS_MAX {
        answers.remove(0);
    }
    answers.push((zone.map(str::to_owned), unix, now, offset));
    Some(offset)
}

/// Read `identity`'s facts and publish them on `timeline`: `Some` when they
/// MOVED, carrying the working directory whose repository read was refused, if
/// one was; and what the read [`Found`] — the process's registry row, and
/// whether the footer the timeline now shows carries a limit wall. The file
/// reads run before the (leaf) timeline lock is taken. A registry entry that
/// names an image `identity` was not read for reads the command line and the
/// login into it first (`asks`). The facts are read
/// over the process's `tail`, the session's usage folded on in `fold`; a
/// read that answers is given its account from `roster`
/// ([`accounts::footer_label`]), which is asked only then, and only for a
/// process whose login was read.
fn resolve_into(
    timeline: &Arc<Mutex<SessionTimeline>>,
    identity: &mut Identity,
    tail: &mut footer::TailCache,
    fold: &mut footer::FooterCache,
    roster: &mut RosterMemo,
    asks: Asks,
) -> (
    Option<Option<PathBuf>>,
    Found,
    crate::session_log::transcript::Applied,
) {
    // THE SESSION LOG'S STEP TAP (phase 3B): armed before the read while the
    // log holds a run that reads a transcript, with that run's watermark and
    // floor — one short hold of the (leaf) timeline lock, released before
    // any file is read. A tap still waiting for its file is armed again, so
    // the run in front now gives it its floor and watermark.
    if !fold.steps_armed() || !fold.steps_bound() {
        let arm = timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .slog()
            .transcript_arm(crate::session_log::transcript::Source::Transcript);
        if let Some(arm) = arm {
            fold.set_floor(arm.floor_ms, arm.given);
        }
    }
    let (moved, found) = resolve_facts(timeline, identity, tail, fold, roster, asks);
    // The steps the read tapped, applied after the facts were published, at
    // most `transcript::CHUNK` events a hold (ruling S16).
    let logged = if fold.steps_armed() {
        let batch = fold.take_steps();
        let applied = crate::session_log::transcript::apply_batch(
            timeline,
            crate::session_log::transcript::Source::Transcript,
            &batch,
            fold.steps_bound(),
        );
        if applied.disarm {
            fold.disarm_steps();
        }
        applied
    } else {
        crate::session_log::transcript::Applied::default()
    };
    (moved, found, logged)
}

/// [`resolve_into`]'s footer half: read `identity`'s facts and publish them.
fn resolve_facts(
    timeline: &Arc<Mutex<SessionTimeline>>,
    identity: &mut Identity,
    tail: &mut footer::TailCache,
    fold: &mut footer::FooterCache,
    roster: &mut RosterMemo,
    asks: Asks,
) -> (Option<Option<PathBuf>>, Found) {
    // ONE footer read (`footer::read_pid`): one read of `sessions/<pid>.json`
    // feeds the model, the Σ, the wall and the frozen-program remedy's resume
    // line alike, so after `/clear` or `/resume` the footer never pairs one
    // conversation's model with another's Σ or wall, nor names another's
    // conversation to resume.
    // `read_at` is no later than that registry read: when this read saw the
    // process in the session the registry names.
    let clock = footer::ReadClock {
        read_at: std::time::SystemTime::now(),
        now: aterm_agent::supervise::limit::unix_now(),
        offset_at: &offset_at_cached,
    };
    resolve_facts_at(timeline, identity, tail, fold, roster, asks, &clock)
}

/// The same worker read with an explicit clock, so quota expiry can be driven without
/// sleeps or changing the process's wall clock.
fn resolve_facts_at(
    timeline: &Arc<Mutex<SessionTimeline>>,
    identity: &mut Identity,
    tail: &mut footer::TailCache,
    fold: &mut footer::FooterCache,
    roster: &mut RosterMemo,
    asks: Asks,
    clock: &footer::ReadClock<'_>,
) -> (Option<Option<PathBuf>>, Found) {
    resolve_facts_at_with_cwd(
        timeline,
        identity,
        tail,
        fold,
        roster,
        (asks, aterm_sysprobe::process_cwd),
        clock,
    )
}

/// The resolver's process-directory capability, passed explicitly so an
/// owned fixture never probes an arbitrary process bearing its example pid.
fn resolve_facts_at_with_cwd(
    timeline: &Arc<Mutex<SessionTimeline>>,
    identity: &mut Identity,
    tail: &mut footer::TailCache,
    fold: &mut footer::FooterCache,
    roster: &mut RosterMemo,
    capabilities: (Asks, impl Fn(u32) -> Option<PathBuf>),
    clock: &footer::ReadClock<'_>,
) -> (Option<Option<PathBuf>>, Found) {
    let (asks, cwd_of) = capabilities;
    let is_claude = timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .agent()
        .program
        .as_deref()
        .and_then(aterm_phase::program_of)
        == Some(aterm_phase::Program::Claude);
    let started = identity.started;
    let mut facts = u32::try_from(identity.pid)
        .ok()
        .zip(identity.dir.clone())
        .and_then(|(pid, dir)| {
            // The command line and the login are the image's the entry names: read at its
            // first row and again when a row names a new one. A refused read says nothing
            // of the image (never the last one's facts) and is asked again at the next.
            let launch = |entry: &footer::SessionEntry| {
                if identity.launch_for != Some(entry.started_at) {
                    let args = (asks.args)(identity.pid);
                    identity.launch = args
                        .as_ref()
                        .map(|a| footer::launch_facts_of(&a.argv, &a.env))
                        .unwrap_or_default();
                    identity.login = login_of_args(args.as_ref());
                    identity.task = args
                        .as_ref()
                        .and_then(|a| crate::survey::task_prompt_of_env(&a.env));
                    if args.is_some() {
                        identity.launch_for = Some(entry.started_at);
                    }
                    identity.env_stamp = args.as_ref().map(|args| exec_env_stamp(&args.env));
                    identity.argv = args.map(|a| a.argv).unwrap_or_default();
                }
                if is_claude
                    && identity
                        .login_checked_for
                        .as_ref()
                        .is_none_or(|(image, cwd)| *image != entry.started_at || *cwd != entry.cwd)
                {
                    identity.stored_login = false;
                    if identity.launch_for == Some(entry.started_at) {
                        identity.stored_login = u32::try_from(identity.pid)
                            .ok()
                            .zip(entry.cwd.to_str())
                            .is_some_and(|(pid, cwd)| {
                                (asks.stored_login)(pid, &identity.argv, cwd)
                            });
                        identity.login_checked_for = Some((entry.started_at, entry.cwd.clone()));
                    }
                }
                (identity.launch.clone(), identity.argv.clone())
            };
            footer::read_pid(
                &dir,
                pid,
                started,
                launch,
                tail,
                fold,
                clock,
                crate::harness_host::harness_state().as_deref(),
                (asks.home)().as_deref(),
            )
        });
    // The side's directories: the read's own when it found the process
    // (`footer::FooterCache::dirs`), else the agy/muse read's below.
    let mut dirs = facts
        .as_ref()
        .and(fold.dirs())
        .map(|dirs| (Some(dirs.cwd.clone()), dirs.repo_root.clone()));
    let mut generation = facts.as_ref().and(fold.traffic_generation());
    let mut flight_generation = facts.as_ref().and(fold.flight_generation());
    if facts.is_none() {
        let (non_registry_prog, prev_sub, prev_ctx) = {
            let held = timeline.lock().unwrap_or_else(|p| p.into_inner());
            let prog = held
                .agent()
                .program
                .as_deref()
                .and_then(aterm_phase::program_of)
                .filter(|p| matches!(p, aterm_phase::Program::Agy | aterm_phase::Program::Muse));
            let sub = held
                .claude_footer()
                .and_then(|f| f.subscription.clone())
                .unwrap_or_default();
            // The last reading off the glass, if any: no reading is no fact.
            let ctx = held.claude_footer().and_then(|f| f.context.clone());
            (prog, sub, ctx)
        };
        if let Some(prog) = non_registry_prog {
            let prefix = match prog {
                aterm_phase::Program::Muse => "muse",
                _ => "agy",
            };
            let cwd = u32::try_from(identity.pid).ok().and_then(cwd_of);
            let home = (asks.home)();
            let (branch, repo_read_denied, repo_root) =
                match cwd.as_deref().map(footer::read_git_head) {
                    Some(footer::GitHeadRead::Found(b, root)) => (Some(b), None, Some(root)),
                    Some(footer::GitHeadRead::Denied) => (None, cwd.clone(), None),
                    Some(footer::GitHeadRead::Absent) | None => (None, None, None),
                };
            dirs = Some((cwd.clone(), repo_root));
            generation = None;
            flight_generation = None;
            let args = (asks.args)(identity.pid);
            let launch = args
                .as_ref()
                .map(|a| footer::launch_facts_of(&a.argv, &a.env))
                .unwrap_or_default();
            let model = launch
                .model
                .as_deref()
                .and_then(footer::footer_model_name)
                .or(launch.model);
            // Muse Code's account is its own login store's, read here on
            // the resolver thread; its tier and weekly quota are read off
            // its `/status` and `/usage` cards on the glass and kept in
            // `prev_sub` between reads only while the store names the same
            // address. A different address replaces the whole tuple, so the
            // previous account's card facts cannot survive an account
            // switch in the same process. Where the store names none, the
            // previous display stays: a transient store read cannot erase
            // the only account evidence the glass has given.
            let mut sub = prev_sub;
            if prog == aterm_phase::Program::Muse
                && muse_weekly_expired(
                    sub.weekly_reset.as_ref().and_then(|reset| reset.at),
                    clock.now,
                )
            {
                sub.weekly = None;
                // Keep the occurrence: the same card can remain on glass
                // after this read and must not place itself anew.
            }
            if prog == aterm_phase::Program::Muse
                && let (Some(args), Some(home)) = (args.as_ref(), home.as_deref())
                && let Some(read) = aterm_agent::harness::profile::muse_subscription_for(
                    &args.argv, &args.env, home,
                )
            {
                match sub.email.as_deref() {
                    Some(previous) if read.email.as_deref() != Some(previous) => sub = read,
                    _ => sub.email = read.email,
                }
            }
            facts = Some(FooterFacts {
                model_open: model.is_none(),
                model,
                effort: None,
                effort_open: true,
                effort_for: None,
                path: cwd
                    .as_deref()
                    .and_then(|dir| footer::home_path(dir, home.as_deref())),
                branch,
                repo_read_denied,
                version: None,
                resume: None,
                usage: None,
                owner: Some(footer::FactsOwner {
                    floor: started,
                    session_id: format!("{prefix}-{}", identity.pid),
                }),
                upgrade: None,
                account: None,
                top_effort: None,
                off: footer::FactMask::default(),
                subscription: Some(sub),
                // The plan is read for a Claude Code's model only.
                plan: None,
                context: prev_ctx,
            });
        }
    }
    // The account is laid in here: only this thread holds the process's
    // login and the roster (`FooterFacts::account`).
    if let (Some(facts), Some(who)) = (facts.as_mut(), identity.login.as_ref()) {
        facts.account = roster
            .at(std::time::Instant::now(), asks.roster)
            .and_then(|listed| accounts::footer_label(listed, who))
            .map(str::to_owned);
    }
    // A profile is selected by the process's own captured exec environment, never by
    // the harness process's HOME. Replace all presentation fields on every read, so an
    // account change or failed read cannot retain the preceding account's weekly quota.
    // The plan reading comes from the same read, for the model the footer shows.
    let model = is_claude
        .then(|| {
            shown_model(
                &timeline.lock().unwrap_or_else(|p| p.into_inner()),
                facts.as_ref(),
            )
        })
        .flatten();
    let read = is_claude.then(|| {
        (asks.args)(identity.pid).and_then(|args| {
            // A refused auth-source check says nothing, and a registry write
            // missed during an exec cannot borrow the prior image's verdict.
            if !identity.stored_login
                || args.argv != identity.argv
                || Some(exec_env_stamp(&args.env)) != identity.env_stamp
            {
                return None;
            }
            aterm_agent::harness::profile::subscription_and_plan_for(
                &args.env,
                &(asks.home)()?,
                identity.dir.as_deref()?,
                roster.at(std::time::Instant::now(), asks.roster),
                clock.now,
                model.as_deref(),
            )
        })
    });
    let plan = read.as_ref().and_then(|read| read.as_ref()?.1);
    let subscription = read.map(|read| read.map(|(shown, _)| shown).unwrap_or_default());
    if let Some(facts) = facts.as_mut()
        && is_claude
    {
        facts.subscription = subscription.clone();
        facts.plan = plan;
    }
    let denied = facts.as_ref().and_then(|f| f.repo_read_denied.clone());
    // The survey's excerpts, from the same tail read: published beside the
    // facts, never in them, so a new excerpt repaints nothing.
    let excerpts = facts.as_ref().map(|_| crate::survey::ClaudeExcerpts {
        excerpts: tail.excerpts().cloned().unwrap_or_default(),
        task: identity.task.clone(),
        transcript_len: tail.transcript_len(),
    });
    // The side, rebuilt only when its directories or the fold's generation
    // moved: the one held is taken in its own short hold of the (leaf) lock,
    // and the traffic is built outside it.
    let side = facts.as_ref().map(|_| {
        let held = timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .claude_side()
            .cloned();
        let (cwd, repo_root) = dirs.take().unwrap_or_default();
        side_after(
            held.as_ref(),
            cwd,
            repo_root,
            SideFold {
                generation,
                traffic: || fold.traffic(),
                flight_generation,
                flights: || fold.flights(),
            },
        )
    });
    let row = facts.is_some();
    let mut timeline = timeline.lock().unwrap_or_else(|p| p.into_inner());
    // One unreadable read of the same process (Claude rewrites its sessions file on every
    // status change; a read can land mid-write) keeps the last good facts. The same process
    // means its group and kernel start (`SessionTimeline::claude_footer_identity`); with no
    // start there is no proof and nothing is kept.
    if facts.is_none()
        && identity.started.is_some()
        && timeline.claude_footer_identity() == Some((identity.pid, identity.started))
    {
        // A transient registry read may retain this process's model/usage fold, but
        // account presentation is always replaced from the independently selected profile.
        let moved = timeline.claude_footer().cloned().is_some_and(|mut kept| {
            kept.subscription = subscription;
            kept.plan = plan;
            timeline.set_claude_footer(identity.pid, identity.started, Some(kept))
        });
        return (
            moved.then_some(None),
            Found {
                row,
                timed: needs_recheck(&timeline),
            },
        );
    }
    // The context fact is the frame's reading off the glass, never the registry's: the same
    // process keeps its last reading through every re-read, and a process with none shows none.
    if let Some(f) = facts.as_mut()
        && timeline.claude_footer_identity() == Some((identity.pid, identity.started))
        && let Some(prev_ctx) = timeline
            .claude_footer()
            .and_then(|prev| prev.context.as_ref())
    {
        f.context = Some(prev_ctx.clone());
    }
    timeline.set_claude_excerpts(identity.pid, excerpts);
    timeline.set_claude_side(identity.pid, side);
    if row {
        timeline.note_claude_footer_read(identity.pid, std::time::Instant::now());
    }
    let moved = timeline
        .set_claude_footer(identity.pid, identity.started, facts)
        .then_some(denied);
    (
        moved,
        Found {
            row,
            timed: needs_recheck(&timeline),
        },
    )
}

/// One known reset occurrence has arrived. Shared by the resolver and a
/// new frame observation; an unplaced reset never expires on a guess.
fn muse_weekly_expired(reset_at: Option<i64>, now: i64) -> bool {
    reset_at.is_some_and(|at| at <= now)
}

fn muse_weekly_reset_at(reset: &str, clock: &footer::ReadClock<'_>) -> Option<i64> {
    let spec = aterm_agent::supervise::limit::parse_reset(reset)?;
    let local = (clock.offset_at)(None, clock.now)?;
    Some(aterm_agent::supervise::limit::reset_at_with(
        &spec,
        clock.now,
        local,
        &|zone, unix| (clock.offset_at)(Some(zone), unix),
    ))
}

/// Admit one card observation. An unchanged reset reuses its occurrence
/// and its expiry tombstone; settled frames need no clock, parsing or copy.
fn muse_weekly_observation<'a>(
    (weekly, reset): (&'a str, Option<&'a str>),
    kept: Option<&aterm_agent::harness::profile::SubscriptionDisplay>,
    clock: &dyn Fn() -> footer::ReadClock<'static>,
) -> (Option<&'a str>, Option<(&'a str, Option<i64>)>) {
    let Some(words) = reset else {
        return (Some(weekly), None);
    };
    if let Some(kept) = kept
        && let Some(reset) = kept
            .weekly_reset
            .as_ref()
            .filter(|reset| reset.words == words)
    {
        return (
            kept.weekly.as_ref().map(|_| weekly),
            Some((words, reset.at)),
        );
    }
    let clock = clock();
    let at = muse_weekly_reset_at(words, &clock);
    (
        (!muse_weekly_expired(at, clock.now)).then_some(weekly),
        Some((words, at)),
    )
}

/// Whether any currently published footer value still needs local clock checks.
fn needs_recheck(timeline: &SessionTimeline) -> bool {
    timeline.claude_footer().is_some_and(|facts| {
        facts
            .usage
            .as_ref()
            .is_some_and(|usage| usage.wall.is_some())
            || facts
                .subscription
                .as_ref()
                .is_some_and(|subscription| subscription.weekly.is_some())
            || facts.plan.is_some()
    })
}

/// The model the footer shows for `facts`, or for the facts `timeline` keeps where this
/// read found none: their own, else their launch card's
/// ([`FooterFacts::filled_model_effort`]). The card is the kept facts' owner's, so facts
/// of another owner take none.
fn shown_model(timeline: &SessionTimeline, facts: Option<&FooterFacts>) -> Option<String> {
    let kept = timeline.claude_footer();
    let facts = facts.or(kept)?;
    let card = timeline
        .claude_card()
        .filter(|_| kept.is_some_and(|kept| kept.owner == facts.owner));
    facts.filled_model_effort(card).0.map(str::to_owned)
}

/// A fingerprint of `facts` AS SHOWN — filled from the launch card `card`
/// the timeline keeps for them (`FooterFacts::filled_from`) — under the
/// `[claude_footer] usage` switch ([`with_usage_switch`]), with the facts
/// the person hid (`off`, `crate::claude_facts`), for the repaint key: `0`
/// when there are none, so a frame with no footer keys exactly as it did
/// before this module existed. Taken without a copy — the fill's
/// model and effort borrowed (`FooterFacts::filled_model_effort`) — so with
/// the switch off only the limit wall stands for the usage, and hidden
/// tokens that keep growing do not move the key. The destructure names
/// every field: a new one does not compile until it is hashed here.
pub(crate) fn fingerprint(
    facts: Option<&FooterFacts>,
    card: Option<&aterm_phase::LaunchCard>,
    usage: bool,
    off: FactMask,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let Some(facts) = facts else {
        return 0;
    };
    let (model, effort) = facts.filled_model_effort(card);
    let FooterFacts {
        model: _,
        effort: _,
        path,
        branch,
        repo_read_denied,
        version,
        resume,
        usage: usage_facts,
        model_open,
        effort_open,
        effort_for,
        owner,
        upgrade,
        account,
        // The top rule's, not this rule's: it has its own key.
        top_effort: _,
        // Never the timeline's: the window lays in the person's (`off`).
        off: _,
        subscription,
        // The Stats Deck's reading: the footer does not show it.
        plan: _,
        context,
    } = facts;
    let version = version
        .as_deref()
        .or_else(|| card.map(|c| c.version.as_str()).filter(|v| !v.is_empty()));
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (
        model,
        effort,
        path,
        branch,
        repo_read_denied,
        version,
        resume,
    )
        .hash(&mut h);
    (
        model_open,
        effort_open,
        effort_for,
        owner,
        upgrade,
        account,
        off,
    )
        .hash(&mut h);
    subscription.hash(&mut h);
    context.hash(&mut h);
    if usage {
        usage_facts.hash(&mut h);
    } else {
        usage_facts
            .as_ref()
            .and_then(|u| u.wall.as_ref())
            .hash(&mut h);
    }
    h.finish() | 1
}

/// `facts` as the rule shows them under the `[claude_footer] usage` switch
/// (`Config::claude_footer_usage`, on by default): off, only a standing limit wall is kept
/// of the `Σ` segment (`UsageFacts::wall_only`). Applied where the pane is gathered;
/// [`fingerprint`] keys the same view, so growing hidden tokens move neither glass nor key.
fn with_usage_switch(facts: &mut FooterFacts, usage: bool) {
    if !usage {
        facts.usage = facts
            .usage
            .take()
            .and_then(aterm_agent::harness::session_usage::UsageFacts::wall_only);
    }
}

/// Which of the composer rule's pane columns aterm may cover: a plain rule glyph (the cell
/// equal, colours and every attribute, to the rule's own `─`, whichever of the row's first
/// and last `─` styles more of the row) with nothing in the scratch's side channels at its
/// frame column (a cluster, a combining mark, an image, which the undo could not restore).
/// Anything else on the row is Claude's ink, and a selection or a search highlight over
/// the rule reads as ink too: the facts give way to it. Also returns the rule cell.
#[cfg(test)]
fn rule_mask(
    scratch: &RenderInput,
    frame_row: usize,
    col_off: usize,
    pane_cols: usize,
) -> Option<(Vec<bool>, RenderCell)> {
    let mut mask = Vec::new();
    let rule = rule_mask_into(&mut mask, scratch, frame_row, col_off, pane_cols)?;
    Some((mask, rule))
}

/// [`rule_mask`] into `mask`, cleared first and its buffer reused so a frame allocates
/// nothing for it; returns the rule cell.
fn rule_mask_into(
    mask: &mut Vec<bool>,
    scratch: &RenderInput,
    frame_row: usize,
    col_off: usize,
    pane_cols: usize,
) -> Option<RenderCell> {
    mask.clear();
    let row = scratch.cells.get(frame_row)?;
    let cells = row.get(col_off..row.len().min(col_off + pane_cols))?;
    let first = *cells.iter().find(|c| c.ch == '\u{2500}')?;
    let last = *cells.iter().rev().find(|c| c.ch == '\u{2500}')?;
    let count = |style: &RenderCell| cells.iter().filter(|c| *c == style).count();
    let rule = if first == last || count(&last) >= count(&first) {
        last
    } else {
        first
    };
    mask.extend((0..pane_cols).map(|c| {
        let col = col_off + c;
        cells.get(c) == Some(&rule)
            && !touches(&scratch.clusters, frame_row, &(col..col + 1))
            && !touches(&scratch.combining, frame_row, &(col..col + 1))
            && !touches(&scratch.images, frame_row, &(col..col + 1))
    }));
    Some(rule)
}

/// The cells aterm writes into `room` cells of the rule for one pane, and where in them the
/// lights' chips start: the lights (` <chips>  <title> `) at the left end, the rule's own
/// glyphs, then ` <facts> ` at the right end; without lights the facts alone, so a rule at
/// rest is written only where the facts are. A group is drawn only when `fit` keeps it.
///
/// The facts are `Script::Sub` and bold, below the cursor trail's rail
/// (`aterm_render::SUB_CEILING`), set as a line of type (`footer::set_facts`): values in
/// Claude's rule gray lifted [`VALUE_LIFT`] toward the pane's ink, marks in `mark_fg` mixed
/// halfway toward the rule gray. The chips stay full-size, since they are clicked, and
/// their place depends on nothing a title or the facts change, so a chip stays under the
/// pointer while its title comes and goes.
///
/// Also returned: the run between the lights and the facts that is the rule's own glyph
/// (`rule`), which the glass already shows and the splice leaves unwritten
/// ([`rule_edits`]); empty without lights and where the two groups meet.
///
/// And where the facts' cells start, and where each fact lies past that
/// start's blank (`footer::fact_spans`): the targets a click acts on. A
/// hidden fact's greyed `off` is drawn in Claude's rule gray (`rule.fg`),
/// not bold. The fact `lit` (hovered or selected) is drawn lit: its values
/// in the pane's ink, its mark in `mark_fg`, underlined where its glyphs are
/// drawn (`footer::drawn_cells`), each stretch of the line in the ink of the
/// lit glyph drawn over it.
fn compose_rule_spans(
    fit: &footer::RuleFit,
    block: Option<&crate::claude_lights::Block>,
    rule: RenderCell,
    blank: RenderCell,
    mark_fg: [u8; 3],
    room: usize,
    lit: Option<FactKind>,
) -> RulePaint {
    let mut out: Vec<RenderCell> = Vec::with_capacity(room);
    let mut lights_at = None;
    let mut title_at = 0..0;
    if let Some(block) = block.filter(|_| fit.chips) {
        let title: &[RenderCell] = match fit.title {
            footer::TitleFit::Full => &block.title,
            footer::TitleFit::Lean(n) => block.lean.get(usize::from(n)).map_or(&[][..], |t| t),
            footer::TitleFit::Short => &block.short_title,
            footer::TitleFit::None => &[],
        };
        out.push(blank);
        lights_at = Some(out.len());
        out.extend_from_slice(&block.lights);
        // The title's own two cells of gap (it ends with them) go between
        // the chips and its words.
        let gap = title.len().min(2);
        out.extend_from_slice(&title[title.len() - gap..]);
        title_at = out.len()..out.len() + (title.len() - gap);
        out.extend_from_slice(&title[..title.len() - gap]);
        out.push(blank);
    }
    let mut facts: Vec<RenderCell> = Vec::new();
    let mut spans: footer::FactSpans = [None; 6];
    let mut clusters = Vec::new();
    let mut combining = Vec::new();
    if !fit.segments.is_empty() {
        let quiet = |fg: [u8; 3]| RenderCell {
            fg,
            bold: true,
            script: aterm_core::terminal::Script::Sub { nudge: 0 },
            ..blank
        };
        let mark = quiet(mix(mark_fg, rule.fg, 0.5));
        let value = quiet(mix(rule.fg, blank.fg, VALUE_LIFT));
        // A hidden fact's `off`: Claude's own rule gray, not bold.
        let stub = RenderCell {
            bold: false,
            ..quiet(rule.fg)
        };
        let lifted = |cell: RenderCell, fg: [u8; 3]| RenderCell { fg, ..cell };
        // The set line, a blank cell either side: the rule meets it there.
        let set = footer::set_facts(&fit.segments);
        spans = footer::fact_spans(&set, &fit.segments);
        let lit_span = lit.and_then(|kind| {
            spans
                .iter()
                .flatten()
                .find(|span| span.kind == kind)
                .copied()
        });
        let lit_cells = lit_span.map(|span| usize::from(span.cells.0)..usize::from(span.cells.1));
        // Each lit glyph's middle as drawn, in nudges, and its ink: the
        // underline under it takes that ink.
        let mut lit_inks: Vec<(isize, [u8; 3])> = Vec::new();
        facts = vec![blank; set.cells + 2];
        for glyph in &set.glyphs {
            let lit = lit_cells.as_ref().is_some_and(|c| c.contains(&glyph.cell));
            // The wall's `♻` (for "resets") is a mark inside its value.
            let ink = match (
                glyph.mark || glyph.ch == footer::RESET_MARK,
                glyph.quiet,
                lit,
            ) {
                (true, _, false) => mark,
                (true, _, true) => lifted(mark, mark_fg),
                (false, true, false) => stub,
                (false, true, true) => lifted(stub, value.fg),
                (false, false, false) => value,
                (false, false, true) => lifted(value, blank.fg),
            };
            let cell = RenderCell {
                ch: glyph.ch,
                script: aterm_core::terminal::Script::Sub { nudge: glyph.nudge },
                ..ink
            };
            facts[1 + glyph.cell] = cell;
            if glyph.cols == 2 {
                facts[2 + glyph.cell] = RenderCell {
                    ch: ' ',
                    wide: true,
                    ..cell
                };
            }
            if lit {
                let n = isize::from(footer::NUDGES_PER_CELL);
                let middle = glyph.cell as isize * n
                    + isize::from(glyph.nudge)
                    + glyph.cols as isize * n / 2;
                lit_inks.push((middle, cell.fg));
            }
        }
        // The lit fact's underline goes under its glyphs where they are
        // drawn (`footer::drawn_cells`), not under the cells they stand in:
        // a cell there may hold a blank or a neighbour's glyph, which keeps
        // its own ink and wears the lit glyph's underline drawn over it.
        if let Some(span) = lit_span {
            let n = isize::from(footer::NUDGES_PER_CELL);
            for c in footer::drawn_cells(&set, &span) {
                let middle = c as isize * n + n / 2;
                let ink = lit_inks
                    .iter()
                    .min_by_key(|(at, _)| (at - middle).abs())
                    .map(|&(_, fg)| fg);
                let cell = &mut facts[1 + c];
                cell.underline = aterm_core::terminal::UnderlineStyle::Single;
                cell.underline_color = ink;
            }
        }
        clusters = set.clusters;
        combining = set.combining;
    }
    let mut glass = 0..0;
    if !out.is_empty() {
        let glyphs = room.saturating_sub(out.len() + facts.len());
        glass = out.len()..out.len() + glyphs;
        out.extend(std::iter::repeat_n(rule, glyphs));
    }
    let facts_at = out.len();
    // A glyph's cell is past the facts' leading blank.
    for (col, _) in &mut clusters {
        *col += facts_at + 1;
    }
    for (col, _) in &mut combining {
        *col += facts_at + 1;
    }
    out.extend(facts);
    RulePaint {
        cells: out,
        lights_at,
        title: title_at,
        glass,
        facts_at,
        spans,
        clusters,
        combining,
    }
}

/// [`compose_rule_spans`]' cells and where the chips start, as the tests
/// read a composition.
#[cfg(test)]
fn compose_rule(
    fit: &footer::RuleFit,
    block: Option<&crate::claude_lights::Block>,
    rule: RenderCell,
    blank: RenderCell,
    mark_fg: [u8; 3],
    room: usize,
) -> (Vec<RenderCell>, Option<usize>) {
    let paint = compose_rule_spans(fit, block, rule, blank, mark_fg, room, None);
    (paint.cells, paint.lights_at)
}

/// One rule's edits for [`write_edits`]: its `cells` at `col` of frame row `row`, less the
/// run `glass` of them ([`compose_rule_spans`]), so the lights and the facts go as two
/// edits. The run is the rule's own glyph over cells `rule_mask` found equal to that glyph
/// and free of side channels, so writing it would change no cell; leaving it out keeps the
/// glass and spares the write and its undo the rule's width. The head keeps `col`, the
/// column the lights' hits are keyed by (`Lit::col`). A top line's write goes the same
/// way ([`ComposedTop::glass`]): its head keeps the column its lockup and mark sprite are
/// keyed by, and its tail holds the switch whole.
fn rule_edits<'a>(
    row: usize,
    col: usize,
    cells: &'a [RenderCell],
    glass: &std::ops::Range<usize>,
    ground: RenderCell,
) -> impl Iterator<Item = (usize, usize, &'a [RenderCell], RenderCell)> {
    let (head, tail) = if glass.is_empty() {
        (cells, cells.len())
    } else {
        (&cells[..glass.start], glass.end)
    };
    [
        (row, col, head, ground),
        (row, col + tail, &cells[tail..], ground),
    ]
    .into_iter()
    .filter(|(_, _, cells, _)| !cells.is_empty())
}

// The facts' nudges are counted in the painter's own steps.
const _: () = assert!(footer::NUDGES_PER_CELL == aterm_core::terminal::Script::NUDGES_PER_CELL);

/// How far the values' ink is lifted from Claude's rule gray toward the
/// pane's text colour: a third of the way, so the small, low facts read
/// without effort and still stand below the conversation's text.
const VALUE_LIFT: f32 = 1.0 / 3.0;

/// The footer's value colour: Claude's gray `gray` lifted [`VALUE_LIFT`] toward the pane's
/// ink `ink`. The rule's values are drawn in it, and the harness card's line
/// (`crate::claude_card`) takes it as its base colour.
pub(crate) fn value_fg(gray: [u8; 3], ink: [u8; 3]) -> [u8; 3] {
    mix(gray, ink, VALUE_LIFT)
}

/// `a` moved `t` of the way toward `b`, per channel.
fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    std::array::from_fn(|i| {
        (f32::from(a[i]) + (f32::from(b[i]) - f32::from(a[i])) * t).round() as u8
    })
}

/// The scratch facts one footer write is valid against: every token a fill,
/// a prepend, a compose or another host write moves. [`undo`] restores only
/// while all of them still read exactly as the splice left them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Tokens {
    snapshot_seq: u64,
    engine_fill_seq: u64,
    shifted_fill_seq: u64,
    composed_fill_seq: u64,
    terminal_id: u64,
    extract_gen: u64,
    row_rev_lane: u64,
    row_shift: usize,
    rows: usize,
    cols: usize,
    cell_rows: usize,
}

impl Tokens {
    fn of(s: &RenderInput) -> Self {
        Self {
            snapshot_seq: s.snapshot_seq,
            engine_fill_seq: s.engine_fill_seq,
            shifted_fill_seq: s.shifted_fill_seq,
            composed_fill_seq: s.composed_fill_seq,
            terminal_id: s.terminal_id,
            extract_gen: s.extract_gen,
            row_rev_lane: s.row_rev_lane,
            row_shift: s.row_shift,
            rows: s.rows,
            cols: s.cols,
            cell_rows: s.cells.len(),
        }
    }
}

/// One row a footer splice wrote.
#[derive(Debug)]
struct RowUndo {
    frame_row: usize,
    col_off: usize,
    /// The row's length before the write (the engine trims trailing blanks,
    /// so the write may have padded it).
    len_before: usize,
    /// The cells the write replaced — the part of the span the row had.
    before: Vec<RenderCell>,
    /// The cells the write put there: the undo's proof they are still there.
    painted: Vec<RenderCell>,
    /// The row's D-2 revision before the write, when the write cleared it.
    rev_before: Option<u64>,
    clusters: TextUndo<Box<str>>,
    combining: TextUndo<Box<[char]>>,
}

/// Exact metadata owned by one cell edit, with returned payloads reused next frame.
#[derive(Debug, Default)]
struct TextUndo<T> {
    owned: Vec<(usize, T)>,
    live: usize,
    spare: Vec<T>,
}

impl<T: Clone + Eq> TextUndo<T> {
    fn publish(&mut self, channel: &mut Vec<(usize, T)>, col: usize, value: &T) {
        if self.live == self.owned.len() {
            self.owned.push((col, value.clone()));
        } else {
            let kept = &mut self.owned[self.live];
            kept.0 = col;
            if &kept.1 != value {
                kept.1.clone_from(value);
            }
        }
        self.live += 1;
        // A fresh engine fill can replace the preceding frame before undo ran.
        // Reserve its return slot now, so the first settled undo allocates nothing.
        if self.spare.capacity() < self.live {
            self.spare.reserve(self.live - self.spare.len());
        }
        // Several bases can carry differently sized marks. Match returned
        // payloads rather than popping them in reverse paint order and resizing
        // their boxes on every otherwise unchanged frame.
        let matching = self.spare.iter().position(|payload| payload == value);
        let mut payload = matching
            .map(|index| self.spare.swap_remove(index))
            .or_else(|| self.spare.pop())
            .unwrap_or_else(|| value.clone());
        if &payload != value {
            payload.clone_from(value);
        }
        channel.push((col, payload));
    }

    fn intact(&self, channel: &[(usize, T)], span: &std::ops::Range<usize>) -> bool {
        channel
            .iter()
            .filter(|(col, _)| span.contains(col))
            .eq(self.owned[..self.live].iter())
    }

    fn remove(&mut self, channel: &mut Vec<(usize, T)>, span: &std::ops::Range<usize>) {
        self.spare.extend(
            channel
                .extract_if(.., |(col, _)| span.contains(col))
                .map(|(_, payload)| payload),
        );
    }
}

/// What the last footer splice overwrote in one window's scratch, with the proof that
/// nothing has touched the scratch since. Held on the window
/// (`WindowState::claude_footer_undo`); once [`undo`] consumes the write, its row records
/// stay as buffers the next write fills ([`write_edits`]), so a footer painted every frame
/// allocates nothing for its record once they are grown.
#[derive(Debug, Default)]
pub(crate) struct FooterUndo {
    /// The write on the glass: `None` once [`undo`] consumed it, or when
    /// nothing was written.
    write: Option<WriteSeal>,
    /// Its rows, `rows[..live]`; the rest are spare buffers.
    rows: Vec<RowUndo>,
    live: usize,
    /// The rows of the write whose glass columns are not the grid's (a
    /// relabelled pill, [`GlassColumns`]): gone with the write.
    glass: GlassColumns,
}

/// The tokens a footer write is undone against ([`FooterUndo`]).
#[derive(Debug)]
struct WriteSeal {
    after: Tokens,
    seq_before: u64,
    shifted_before: u64,
    cluster_rows_before: usize,
    combining_rows_before: usize,
}

impl FooterUndo {
    /// The grid column under glass column `col` of window terminal row
    /// `row`, as the write on the glass left them ([`GlassColumns::grid_col`]):
    /// what every reader of the pointer's cell asks (`App::pixel_to_cell`),
    /// so a click, a hover, a link and a selection on the glass's `14
    /// shells` land on Claude's own `14 shells`.
    pub(crate) fn grid_col(&self, row: u16, col: u16) -> u16 {
        if self.write.is_none() {
            return col;
        }
        self.glass.grid_col(row, col)
    }

    /// Whether the write on the glass put `(frame_row, col)`'s edit there.
    fn took(&self, frame_row: usize, col: usize) -> bool {
        self.written()
            .iter()
            .any(|r| (r.frame_row, r.col_off) == (frame_row, col))
    }

    /// Each row span the write on the glass wrote: its frame row, its first
    /// column and its width — what another module's test asks.
    #[cfg(test)]
    pub(crate) fn painted_spans(&self) -> impl Iterator<Item = (usize, usize, usize)> + '_ {
        self.written()
            .iter()
            .map(|r| (r.frame_row, r.col_off, r.painted.len()))
    }

    /// Add native text only where this splice actually wrote its cells. A skipped
    /// vendor-owned span gets no metadata, and undo verifies these exact payloads.
    fn publish_text(
        &mut self,
        scratch: &mut RenderInput,
        row: usize,
        col: usize,
        paint: &RulePaint,
    ) {
        if self.write.is_none() || (paint.clusters.is_empty() && paint.combining.is_empty()) {
            return;
        }
        for (at, text) in &paint.clusters {
            let at = col + at;
            if let Some(edit) = self.rows[..self.live].iter_mut().find(|edit| {
                edit.frame_row == row
                    && (edit.col_off..edit.col_off + edit.painted.len()).contains(&at)
            }) {
                scratch
                    .clusters
                    .resize_with(scratch.clusters.len().max(row + 1), Vec::new);
                edit.clusters.publish(&mut scratch.clusters[row], at, text);
            }
        }
        for (at, marks) in &paint.combining {
            let at = col + at;
            if let Some(edit) = self.rows[..self.live].iter_mut().find(|edit| {
                edit.frame_row == row
                    && (edit.col_off..edit.col_off + edit.painted.len()).contains(&at)
            }) {
                scratch
                    .combining
                    .resize_with(scratch.combining.len().max(row + 1), Vec::new);
                edit.combining
                    .publish(&mut scratch.combining[row], at, marks);
            }
        }
        if let Some(entries) = scratch.clusters.get_mut(row) {
            entries.sort_unstable_by_key(|(col, _)| *col);
        }
        if let Some(entries) = scratch.combining.get_mut(row) {
            entries.sort_unstable_by_key(|(col, _)| *col);
        }
    }

    /// The rows the write on the glass wrote — none once it is undone.
    fn written(&self) -> &[RowUndo] {
        if self.write.is_some() {
            &self.rows[..self.live]
        } else {
            &[]
        }
    }

    /// How many cells the write on the glass wrote with what they already held: none while
    /// the rule's own glyphs between the chips and the facts are left out of the write
    /// ([`rule_edits`]).
    #[cfg(test)]
    pub(crate) fn rewritten_as_they_were(&self) -> usize {
        self.written()
            .iter()
            .map(|r| {
                r.before
                    .iter()
                    .zip(&r.painted)
                    .filter(|(was, now)| was == now)
                    .count()
            })
            .sum()
    }

    /// Whether a write is on the glass.
    #[cfg(test)]
    fn is_written(&self) -> bool {
        self.write.is_some()
    }

    /// Frame row `frame_row` of `scratch`, its cells in `cols`, as the
    /// PROGRAM wrote them: each cell the write on the glass put there — a
    /// rule's chips and facts and a hidden fact's `off`, the card's line,
    /// the banner over Claude's update notice, the top rule's border, ghost
    /// and switch, a relabelled pill and the run packed behind it, an agy
    /// transcript edit — read as the cell it covered, and the row ending
    /// where it ended before the write padded it: what [`undo`] would put
    /// back, read without writing and without allocating. A row the write
    /// left alone, and one whose painted span a later splice covered (the
    /// Autopilot menu, the paste question, the tab menu: the glass there is
    /// no longer the footer's), reads as presented.
    #[cfg(any(a11y_tree, test))]
    fn program_cells<'a>(
        &'a self,
        scratch: &'a RenderInput,
        frame_row: usize,
        cols: std::ops::Range<usize>,
    ) -> impl Iterator<Item = &'a RenderCell> + 'a {
        let row: &[RenderCell] = scratch.cells.get(frame_row).map_or(&[], Vec::as_slice);
        // The write's records on this row, in the order written: the first
        // saw the row as the program left it.
        let on_row = move || {
            self.written()
                .iter()
                .filter(move |r| r.frame_row == frame_row)
        };
        let glassed = on_row().next().is_some()
            && on_row().all(|r| {
                row.get(r.col_off..r.col_off + r.painted.len())
                    .is_some_and(|span| span == r.painted.as_slice())
            });
        let len = if glassed {
            on_row()
                .next()
                .map_or(row.len(), |r| r.len_before.min(row.len()))
        } else {
            row.len()
        };
        let end = cols.end.min(len);
        (cols.start.min(end)..end).map(move |c| {
            // The first record over the cell holds what the program drew
            // there (inside the row's old end, every record's `before` reaches
            // it); a cell no record covers is the program's as presented.
            glassed
                .then(|| on_row().find(|r| (r.col_off..r.col_off + r.painted.len()).contains(&c)))
                .flatten()
                .and_then(|r| r.before.get(c - r.col_off))
                .unwrap_or(&row[c])
        })
    }
}

/// A window's PRESENTED frame read as its programs wrote it — the cells
/// under every glass write of the footer's ([`FooterUndo::program_cells`]):
/// what the accessibility tree's grid text is read from (review G3, owner
/// decision 2026-10-20), so a screen reader hears Claude's words as copy,
/// `text`/`screen` and recordings keep them — `⏵⏵ bypass permissions on`,
/// Claude's update notice and its changelog link, the rule's own glyphs —
/// never the glass's `YOLO on`, `✦ alab.systems`, a fact or its `off`, or
/// the top rule's ghost. aterm's chips, mode light, switch, title slot,
/// facts and key reach a reader as their own nodes
/// (`App::claude_chips_a11y`), and every non-text layer
/// (ink, light, decoration) is not text. Built only while a reader is
/// attached; nothing is copied.
#[cfg(any(a11y_tree, test))]
pub(crate) struct ProgramCells<'a> {
    scratch: &'a RenderInput,
    undo: &'a FooterUndo,
}

#[cfg(any(a11y_tree, test))]
impl<'a> ProgramCells<'a> {
    /// `scratch` (the presented frame) under `undo` (its last footer write).
    pub(crate) fn of(scratch: &'a RenderInput, undo: &'a FooterUndo) -> Self {
        Self { scratch, undo }
    }
}

#[cfg(any(a11y_tree, test))]
impl crate::accessibility::CellRows for ProgramCells<'_> {
    fn cells_in(
        &self,
        r: usize,
        cols: std::ops::Range<usize>,
    ) -> impl Iterator<Item = &RenderCell> {
        self.undo.program_cells(self.scratch, r, cols)
    }
}

/// The pane's exact terminal extraction that supplied its cells to this frame. The
/// terminal can mutate between that extraction and the footer splice; reading it again
/// would contend with the PTY writer and mix generations. Pure splits use the canonical
/// leaf index because two views may mirror one session; mixed tabs use the stable
/// view-keyed committed cache.
fn frame_source<'a>(
    ws: &'a crate::WindowState,
    route: VisibleContentRoute,
    leaf_index: usize,
    leaf: &crate::tab_model::VisibleLeaf,
) -> Option<&'a RenderInput> {
    let source = match route {
        VisibleContentRoute::Terminal { composed: false } => &ws.input_scratch,
        VisibleContentRoute::Terminal { composed: true } if leaf.focused => {
            &ws.composed_focus_scratch
        }
        VisibleContentRoute::Terminal { composed: true } => {
            ws.unfocused_pane_scratch.get(&leaf_index)?
        }
        VisibleContentRoute::Heterogeneous => &ws.leaf_render_cache.get(&leaf.view)?.input,
        VisibleContentRoute::Native { .. } => return None,
    };
    (source.terminal_id != 0).then_some(source)
}

/// Put back what the last footer splice on `scratch` overwrote, only while the scratch is
/// exactly what that splice left: every [`Tokens`] field equal and every painted span still
/// holding the painted cells. Cells, row lengths, cleared revisions, `snapshot_seq` and
/// `shifted_fill_seq` return to their values before the write, so the scratch is as
/// reusable as before the footer was drawn.
///
/// Any doubt changes nothing: the footer's `snapshot_seq` bump stands and the next fill
/// takes the full arm. The record is consumed either way. Call it at the top of a frame,
/// before the first reader of the reuse tokens; the splice also calls it first, so a frame
/// that presents a scratch without refilling it never paints over its own footer.
pub(crate) fn undo(slot: &mut FooterUndo, scratch: &mut RenderInput) -> bool {
    slot.glass.clear();
    let Some(seal) = slot.write.take() else {
        return false;
    };
    let rows = &mut slot.rows[..std::mem::take(&mut slot.live)];
    if Tokens::of(scratch) != seal.after {
        return false;
    }
    let intact = rows.iter().all(|r| {
        let span = r.col_off..r.col_off + r.painted.len();
        scratch
            .cells
            .get(r.frame_row)
            .and_then(|row| row.get(r.col_off..r.col_off + r.painted.len()))
            .is_some_and(|span| span == r.painted.as_slice())
            && r.clusters.intact(
                scratch.clusters.get(r.frame_row).map_or(&[], Vec::as_slice),
                &span,
            )
            && r.combining.intact(
                scratch
                    .combining
                    .get(r.frame_row)
                    .map_or(&[], Vec::as_slice),
                &span,
            )
            && !touches(&scratch.images, r.frame_row, &span)
    });
    if !intact {
        return false;
    }
    // Newest first: two panes side by side write the same frame row, and the
    // second write's `len_before` is the first write's padded length.
    for r in rows.iter_mut().rev() {
        let span = r.col_off..r.col_off + r.painted.len();
        if let Some(entries) = scratch.clusters.get_mut(r.frame_row) {
            r.clusters.remove(entries, &span);
        }
        if let Some(entries) = scratch.combining.get_mut(r.frame_row) {
            r.combining.remove(entries, &span);
        }
        let row = &mut scratch.cells[r.frame_row];
        row[r.col_off..r.col_off + r.before.len()].copy_from_slice(&r.before);
        row.truncate(r.len_before);
        if let (Some(rev), Some(slot)) = (r.rev_before, scratch.row_rev.get_mut(r.frame_row)) {
            *slot = rev;
        }
    }
    if scratch
        .clusters
        .get(seal.cluster_rows_before..)
        .is_some_and(|rows| rows.iter().all(Vec::is_empty))
    {
        scratch.clusters.truncate(seal.cluster_rows_before);
    }
    if scratch
        .combining
        .get(seal.combining_rows_before..)
        .is_some_and(|rows| rows.iter().all(Vec::is_empty))
    {
        scratch.combining.truncate(seal.combining_rows_before);
    }
    scratch.snapshot_seq = seal.seq_before;
    scratch.shifted_fill_seq = seal.shifted_before;
    true
}

/// Whether a column-keyed side channel holds an entry in `span` of `row`.
fn touches<T>(channel: &[Vec<(usize, T)>], row: usize, span: &std::ops::Range<usize>) -> bool {
    channel
        .get(row)
        .is_some_and(|entries| entries.iter().any(|(col, _)| span.contains(col)))
}

/// Write each `(frame_row, col, cells, ground)` edit into `scratch` and keep in `slot` the
/// record [`undo`] needs (none when nothing was written), its rows reusing the buffers the
/// last consumed write left. A row the engine trimmed short is padded with `ground` (the
/// pane's blank). A span holding a grapheme cluster, a combining mark or an image cell is
/// skipped: the undo could not restore it.
///
/// Where the scratch is still an engine fill, possibly shifted by a blessed chrome prepend
/// (`RenderInput::host_prepend_blessing`), each written row's D-2 revision becomes `0` (the
/// no-stamp sentinel: the renderer compares that row's content) and the splice token is
/// re-armed after the bump, so every other row is still compared by stamp. Without a
/// prepend that is sound with or without the undo: `engine_fill_seq` still differs, so no
/// reuse lane mistakes the scratch for an engine fill. With one, the strip's inverse
/// (`undo_host_row_prepend`) would accept the re-armed token, so this rests on the order at
/// its one call site (`redraw_window_with_layout`): [`undo`] runs immediately before it and
/// either restores the cells and both tokens or leaves `snapshot_seq` moved, which the
/// inverse refuses.
fn write_edits<'a>(
    slot: &mut FooterUndo,
    scratch: &mut RenderInput,
    edits: impl IntoIterator<Item = (usize, usize, &'a [RenderCell], RenderCell)>,
) {
    let seq_before = scratch.snapshot_seq;
    let shifted_before = scratch.shifted_fill_seq;
    let cluster_rows_before = scratch.clusters.len();
    let combining_rows_before = scratch.combining.len();
    let restamp = scratch.shifted_fill_seq != 0 && scratch.host_prepend_blessing();
    slot.write = None;
    slot.live = 0;
    slot.glass.clear();
    for (frame_row, col_off, painted, ground) in edits {
        let span = col_off..col_off + painted.len();
        if touches(&scratch.clusters, frame_row, &span)
            || touches(&scratch.combining, frame_row, &span)
            || touches(&scratch.images, frame_row, &span)
        {
            continue;
        }
        let Some(cells) = scratch.cells.get_mut(frame_row) else {
            continue;
        };
        if slot.live == slot.rows.len() {
            slot.rows.push(RowUndo {
                frame_row,
                col_off,
                len_before: 0,
                before: Vec::new(),
                painted: Vec::new(),
                rev_before: None,
                clusters: TextUndo::default(),
                combining: TextUndo::default(),
            });
        }
        let row = &mut slot.rows[slot.live];
        slot.live += 1;
        row.frame_row = frame_row;
        row.col_off = col_off;
        row.len_before = cells.len();
        row.clusters.live = 0;
        row.combining.live = 0;
        row.before.clear();
        row.before.extend_from_slice(
            cells
                .get(col_off..row.len_before.min(span.end))
                .unwrap_or(&[]),
        );
        // The engine trims trailing blanks: a short row is padded before it is written
        // into, as `splice_tab_menu` does.
        if cells.len() < span.end {
            cells.resize(span.end, ground);
        }
        cells[span].copy_from_slice(painted);
        #[cfg(test)]
        crate::work_counts::footer_cells_written(painted.len());
        row.painted.clear();
        row.painted.extend_from_slice(painted);
        row.rev_before = if restamp {
            scratch
                .row_rev
                .get_mut(frame_row)
                .map(|rev| std::mem::replace(rev, 0))
        } else {
            None
        };
    }
    if slot.live == 0 {
        return;
    }
    scratch.snapshot_seq = scratch.snapshot_seq.wrapping_add(1);
    if restamp {
        scratch.shifted_fill_seq = scratch.snapshot_seq;
    }
    slot.write = Some(WriteSeal {
        after: Tokens::of(scratch),
        seq_before,
        shifted_before,
        cluster_rows_before,
        combining_rows_before,
    });
}

impl App {
    /// The footer facts of every Claude Code session in `wid`'s visible plan, the Autopilot
    /// its harness chips say, the harness card (`crate::claude_card`) and the window's
    /// lights' GUI state (`crate::claude_lights`), folded into one repaint-key term (`0` when
    /// none shows).
    ///
    /// Per Claude Code leaf it takes one timeline lock and, where the facts show, one
    /// Autopilot read ([`App::session_autopilot_for`], on the pass's shared claim clock) for
    /// both the chips' and the card's terms. It allocates nothing, and reads the clock only
    /// for a claim that lapses, a card whose episode is live, or a light's held refusal or
    /// notice (`claude_lights::lights_now`).
    pub(crate) fn claude_footer_fp(
        &self,
        wid: WindowId,
        plan: &crate::tab_model::VisibleLeafPlan,
    ) -> u64 {
        let Some(ws) = self.windows.get(&wid) else {
            return 0;
        };
        let mut fp = ws
            .claude_lights
            .fingerprint(crate::claude_lights::lights_now);
        let mut claude = false;
        let usage = self.config.claude_footer_usage();
        let claims = crate::autopilot::ClaimClock::default();
        for (index, leaf) in plan.leaves.iter().enumerate() {
            let Some(session) = self
                .view_store
                .get(leaf.view)
                .copied()
                .and_then(crate::tab_model::View::terminal_session)
            else {
                continue;
            };
            let Some(entry) = self.pool.get(session) else {
                continue;
            };
            let timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            let facts = timeline.claude_footer();
            claude |= facts.is_some();
            // What the footer shows (`FooterFacts::filled_from`) is hashed without a clone:
            // the fill changes `facts` only where a kept card can fill an open model or
            // effort, so only then is the card asked for, and its fill is hashed in place
            // (borrowed, never built); the hash applies the usage switch.
            let card = facts
                .filter(|f| f.model_open || f.effort_open)
                .and_then(|_| timeline.claude_card());
            // The pane's footer took this term with the facts where they are the ones it holds
            // ([`PaneFooter::fingerprint_of`]): at rest they are compared, not hashed again.
            // With the facts the person hid there (`crate::claude_facts`).
            let off = self.fact_mask(session);
            let kept = facts.and_then(|f| {
                ws.claude_footer_texts
                    .borrow()
                    .footers
                    .get(index)?
                    .fingerprint_of(session, f, timeline.claude_card(), usage, off)
            });
            let leaf = kept.unwrap_or_else(|| fingerprint(facts, card, usage, off));
            let shows = facts.is_some();
            // So is the live upgrade's note ([`App::splice_claude_footer`]),
            // laid in only where there are facts to lead: both its wordings,
            // hashed beside the facts rather than cloned into them. Which one
            // shows follows the composer, whose every change is a frame.
            let note = timeline
                .upgrade_note()
                .filter(|_| facts.is_some())
                .map_or(0, |note| {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    note.hash(&mut h);
                    h.finish()
                });
            let (agent, group, identified) = (
                crate::harness_host::agent_of(timeline.agent()),
                timeline.agent().program_pgid,
                crate::harness_host::identified_agent(timeline.agent()),
            );
            // So is what the top-effort key's ghost is decided by beside the
            // facts (`App::key_face`): its evidence and its latch.
            let key = self.claude_facts.get(&session).map_or(0, |f| {
                u64::from(f.top_seen == Some(group))
                    | u64::from(f.press.as_ref().is_some_and(|p| {
                        matches!(p.slash, crate::claude_slash::Slash::TopEffort(_))
                    })) << 1
                    | u64::from(f.latch.as_ref().is_some_and(|latch| {
                        latch.holds(group, facts.and_then(|f| f.model.as_deref()))
                    })) << 2
            });
            // The harness card's gate (card engine §3.8): the current drawer,
            // its reader and its build policy, which the splice judges the
            // card by — with facts or without them.
            let gate = timeline.card_gate().map_or(0, |gate| gate.key());
            let drawer = !shows && timeline.card_drawer_current();
            drop(timeline);
            // The agent as the host admits it: a vendor `codex` has no
            // harness of aterm's, so no chips ([`crate::harness_host::admitted`]).
            let agent = crate::harness_host::admitted_as(&entry.ctx, agent, group);
            // So are the harness's chips where the facts show — the
            // session's Autopilot and a click of it in flight
            // (`crate::claude_lights`) — and the harness card on its launch
            // card (`crate::claude_card`): one reading of the Autopilot for
            // both, with the timeline's lock released.
            let harness = if shows {
                let (word, view, supervised, _) =
                    self.session_autopilot_for(session, agent, &claims);
                let theme = crate::autopilot::harness_theme(
                    view,
                    supervised,
                    word,
                    self.unhosted_glass(agent),
                );
                ws.claude_lights
                    .autopilot_term(session, word, view, self.chip_glass(session))
                    ^ self
                        .claude_card_term(wid, ws, index, session, view, supervised)
                        .rotate_left(37)
                    // …and the Team chip (who drives, `crate::team`).
                    ^ self.team_term(session).rotate_left(53)
                    // …and the composer border's theme, which a claim landing
                    // or lapsing moves though the card is switched off.
                    ^ theme_term(theme).rotate_left(61)
                    // …and the agents panel the theme lights
                    // (`crate::claude_panel`): its switch, and the frame a
                    // panel on show wants.
                    ^ self
                        .claude_panel_term(ws, index, session, theme)
                        .rotate_left(43)
                    // …and the harness switch on the top rule: whether the
                    // host holds the session (its branding), a click's write
                    // turning it on, the pointer over it.
                    ^ ws.claude_lights
                        .switch_term(session, supervised)
                        .rotate_left(59)
            } else if drawer {
                // A drawer with no facts yet takes the card's path alone: no
                // chips, no team — the card is decorated on its first frame.
                let (_, view, supervised, _) = self.session_autopilot_for(session, agent, &claims);
                self.claude_card_term(wid, ws, index, session, view, supervised)
                    .rotate_left(37)
                    ^ 0xd7a3_5e11
            } else {
                0
            } ^ gate.rotate_left(11);
            // Gemini CLI's and Codex's top line, which no facts carry: its
            // switch, its theme and the pointer over it or the lockup.
            let top = match identified {
                Some(prog @ (aterm_phase::Program::Gemini | aterm_phase::Program::Codex)) => {
                    self.bare_top_term(ws, session, prog, agent, &claims)
                }
                _ => 0,
            };
            fp = fp.rotate_left(7)
                ^ leaf
                ^ note.rotate_left(17)
                ^ harness.rotate_left(29)
                ^ key.rotate_left(41)
                ^ top.rotate_left(31);
        }
        // The `[claude_footer] usage` switch, live: a reload that flips it
        // asks every window for a frame (the config apply), and this term
        // lets it through the key.
        if claude {
            fp ^= u64::from(usage).rotate_left(47);
        }
        fp
    }

    /// WHAT aterm DRAWS FOR `session`'s TOP-EFFORT KEY on its top rule
    /// (design §10.6.14), `tag` Claude's own on the row as the frame read it:
    /// for a Claude Code that takes the key's toggle alone (`stated` is
    /// `None` for any other: [`PaneFooter::key_typed`]), the ghost where
    /// Claude draws no tag — only on evidence the key exists for the
    /// process (the facts `stated` it, the tag was seen for `pgid`, or this
    /// session's own press of it is in flight) and not latched off for the
    /// process and its `model` by Claude's refusal — lit while hovered; the
    /// mark `◐` while the press runs, `▸` while the keyboard selects the
    /// key. Map lookups and compares: nothing allocated.
    fn key_face(
        &self,
        ws: &crate::WindowState,
        session: u64,
        pgid: i32,
        stated: Option<bool>,
        model: Option<&str>,
        tag: Option<(usize, usize)>,
    ) -> KeyFace {
        let Some(stated) = stated else {
            return KeyFace::default();
        };
        let facts = self.claude_facts.get(&session);
        let pressing = facts
            .and_then(|f| f.press.as_ref())
            .is_some_and(|p| matches!(p.slash, crate::claude_slash::Slash::TopEffort(_)));
        let evidence = stated || facts.and_then(|f| f.top_seen) == Some(pgid) || pressing;
        let latched = facts
            .and_then(|f| f.latch.as_ref())
            .is_some_and(|latch| latch.holds(pgid, model));
        let (hovered, selected) = ws.claude_lights.top_effort_read(session);
        let ghost = (tag.is_none() && evidence && !latched).then_some(hovered);
        let mark = if pressing {
            crate::autopilot::PENDING_GLYPH.chars().next()
        } else {
            selected.then_some('\u{25b8}')
        };
        KeyFace {
            ghost,
            tag,
            mark: mark.filter(|_| ghost.is_some() || tag.is_some()),
        }
    }

    /// The harness switch and the theme GEMINI CLI's or CODEX's top line
    /// (`prog`) wears, under `session`'s Autopilot `reading` for its admitted
    /// `agent` (design §6.2): a switch only where the host admits the agent
    /// and a host is — never for a vendor `codex`, which no host admits — and
    /// the theme only where a switch stands. codex-alab carries itself: a
    /// proven self-claim (`reading.3`) is its harness's, so its line wears
    /// the palette as a claim of this host's would; every other program
    /// keeps this host's claim alone.
    fn bare_top_harness(
        &self,
        ws: &crate::WindowState,
        session: u64,
        prog: aterm_phase::Program,
        agent: Option<aterm_phase::Program>,
        (word, view, supervised, self_proven): (
            Option<aterm_agent::supervise::Autopilot>,
            crate::autopilot::AutopilotView,
            bool,
            bool,
        ),
    ) -> (
        Option<SwitchPaint>,
        Option<aterm_effects::harness_card::CardPalette>,
    ) {
        let switch = agent
            .and_then(|_| crate::autopilot::HarnessSwitch::of(view))
            .map(|face| {
                let (face, hover) = ws.claude_lights.switch_shown(session, face);
                SwitchPaint { face, hover }
            });
        let carried = supervised || (prog == aterm_phase::Program::Codex && self_proven);
        let theme =
            crate::autopilot::harness_theme(view, carried, word, self.unhosted_glass(agent));
        (switch, theme.filter(|_| switch.is_some()))
    }

    /// The repaint key's term for GEMINI CLI's or CODEX's top line
    /// (`prog`, `session`'s, its admitted `agent`): what its switch and theme
    /// are made of ([`Self::bare_top_harness`]) and the pointer over the
    /// switch or the lockup — each a change of the line with no grid damage,
    /// which the footer's facts carry for the others. One Autopilot read.
    fn bare_top_term(
        &self,
        ws: &crate::WindowState,
        session: u64,
        prog: aterm_phase::Program,
        agent: Option<aterm_phase::Program>,
        claims: &crate::autopilot::ClaimClock,
    ) -> u64 {
        let reading = self.session_autopilot_for(session, agent, claims);
        let (word, view, supervised, self_proven) = reading;
        let carried = supervised || (prog == aterm_phase::Program::Codex && self_proven);
        let theme =
            crate::autopilot::harness_theme(view, carried, word, self.unhosted_glass(agent));
        view.code()
            ^ u64::from(agent.is_some()).rotate_left(5)
            ^ theme_term(theme).rotate_left(13)
            ^ ws.claude_lights
                .switch_term(session, carried)
                .rotate_left(23)
    }

    /// SPLICE ONE PANE'S TOP LINE (design §6.2): Claude Code's, agy's and
    /// Muse Code's composer top rule, Gemini CLI's live-zone rule, Codex's
    /// slab pad ([`top_line`]), its run read once per rebuild of the rows and
    /// kept with them, and its composition kept while what it is made of
    /// holds ([`PaneFooter::compose_top`]): a frame at rest reads, composes
    /// and allocates nothing for it. Its cells go in the footer's write
    /// (`lists.tops`), and its targets — the key's ghost or tag, the
    /// harness switch, the lockup the hover title answers — are noted once
    /// the write takes. On Codex's slab the slab's blank stands in for the
    /// pane's ([`TopLine::Slab`]).
    fn splice_top_line(
        &self,
        ws: &crate::WindowState,
        wid: WindowId,
        texts: &mut PaneTextCache,
        lists: &mut SpliceLists,
        at: &TopLineAt,
    ) {
        let TopLineAt {
            leaf_index,
            session,
            program_pgid,
            program,
            build,
            region,
            pane,
            blank,
            switch,
            palette,
            held,
            cell,
            mark_fg,
        } = *at;
        let Some((top, line)) = texts.top_line(leaf_index, program) else {
            return;
        };
        if self.chrome_owns_terminal_row(wid, region.row_off + top) {
            return;
        }
        let (Some(run), tag) = texts.top_run(
            leaf_index,
            (session, program),
            &ws.input_scratch,
            region,
            top,
            line,
            blank,
        ) else {
            return;
        };
        // The ground every ink is laid on — the pane's blank on a rule, the
        // slab's on Codex's slab — and the instance theme's accent floored
        // along its own hue to read 4.5:1 on it (§8). The accent is chosen
        // for the instance theme's own ground, which neither line need be
        // on: after a dark Codex slab outlives a turn to Default Light, the
        // light accent's `on` pill (`#1a7f37`) read 2.6:1 there, its letters
        // the slab's ink on it the same (codex-alab 0.164.0-dev, the
        // real-Codex lab, 2026-10-21); and a tab in a theme of its own
        // (`session_theme`: the pane's cells are its theme's, the chrome the
        // instance's) lays its rule on that theme's paper — an instance in
        // Dracula read the pill on a Default Light tab at 1.0:1. An accent
        // that already reads is kept.
        let ground = match line {
            TopLine::Rule => blank,
            TopLine::Slab => run.rule,
        };
        let mark_fg = legible_on(mark_fg, ground.bg);
        let mark_hover = ws.claude_lights.mark_hovered(session);
        let footer = texts.footer(leaf_index, session);
        let typed = program == aterm_phase::Program::Claude && footer.key_typed();
        let face = self.key_face(
            ws,
            session,
            program_pgid,
            typed.then(|| footer.shown().top_effort.is_some()),
            footer.published().and_then(|f| f.model.as_deref()),
            tag,
        );
        if face.tag.is_some()
            && self.claude_facts.get(&session).and_then(|f| f.top_seen) != Some(program_pgid)
        {
            lists.top_seen.push((session, program_pgid));
        }
        let frame_row = region.strip + region.row_off + top;
        // A SELECTION OVER THE LINE keeps it the program's (§6.6): the
        // selection is the renderer's overlay, never the cells, so nothing
        // above proved the row clear of it, and decoration laid under it
        // would read as selected while the copy gives the grid's own text.
        // No write, no layer, no lockup or switch to point at; Claude's own
        // tag is Claude's cells, a target still.
        if top_line_selected(&ws.input_scratch, frame_row, region) {
            if let Some(cols) = face.tag {
                lists.top_hits.push(TopHit {
                    frame_row,
                    term_row: region.row_off + top,
                    cols,
                    session,
                    ghost: false,
                });
            }
            return;
        }
        let key = TopKey {
            frame_row,
            term_row: region.row_off + top,
            line,
            run,
            program,
            build,
            switch,
            palette,
            blank: ground,
            mark_fg,
            face,
            cell,
            clip: pane_grid_px(cell, pane),
            mark_hover,
            title_gen: if mark_hover && held {
                footer.shown_gen
            } else {
                0
            },
        };
        let composed = texts.compose_top(leaf_index, key);
        // The key's target: aterm's ghost where its write takes, Claude's
        // tag where the glass shows it.
        let hit = |cols: (usize, usize), ghost: bool| TopHit {
            frame_row: key.frame_row,
            term_row: key.term_row,
            cols,
            session,
            ghost,
        };
        if let Some(cols) = composed.ghost {
            lists.top_hits.push(hit(cols, true));
        } else if let Some(cols) = face.tag {
            lists.top_hits.push(hit(cols, false));
        }
        // The harness switch the composition laid: a target once its write
        // reaches the glass.
        if let (Some(laid), Some(paint)) = (composed.switch.clone(), switch) {
            lists
                .border_toggles
                .push(crate::claude_lights::BorderSwitch {
                    frame_row: key.frame_row,
                    term_row: key.term_row,
                    cols: laid.cols,
                    switch: laid.switch,
                    button: laid.button,
                    session,
                    face: paint.face,
                    tier: laid.tier,
                    program,
                });
        }
        if !composed.cells.is_empty() {
            if !composed.lockup.is_empty() {
                lists.marks.push(MarkWrite {
                    frame_row: key.frame_row,
                    term_row: key.term_row,
                    edit_col: composed.col,
                    cols: composed.lockup.clone(),
                    session,
                });
            }
            lists.tops.push(TopWrite {
                frame_row: key.frame_row,
                col: composed.col,
                leaf_index,
                ground,
                glass: composed.glass.clone(),
            });
        }
    }

    /// Write the footer into each visible Claude Code pane's composer rule, inside that
    /// pane's columns and only over its plain rule glyphs. Runs after the tab strip is
    /// prepended (frame row = strip + pane row), so a find bar or settings panel that
    /// already claimed the row keeps it. A no-op for a pane with no facts, a pane scrolled
    /// into history, and a screen with no composer frame (a dialog or a picker up).
    ///
    /// The write is recorded for [`undo`] (see the module header), and an outstanding
    /// record is undone first: a frame that presents its scratch without refilling it must
    /// read the vendor's row, not last frame's footer.
    pub(crate) fn splice_claude_footer(
        &mut self,
        wid: WindowId,
        plan: &crate::tab_model::VisibleLeafPlan,
        route: VisibleContentRoute,
    ) {
        self.splice_claude_footer_at(wid, plan, route, &|| footer::ReadClock {
            read_at: std::time::SystemTime::now(),
            now: aterm_agent::supervise::limit::unix_now(),
            offset_at: &offset_at_cached,
        });
    }

    /// The same splice with a lazy observation clock: only a newly named
    /// Muse reset forces it, never an unrelated or settled drawer.
    fn splice_claude_footer_at(
        &mut self,
        wid: WindowId,
        plan: &crate::tab_model::VisibleLeafPlan,
        route: VisibleContentRoute,
        clock: &dyn Fn() -> footer::ReadClock<'static>,
    ) {
        // The splice's lists are the window's, kept for their buffers: taken
        // here, handed back empty.
        let Some(mut lists) = self.windows.get_mut(&wid).map(|ws| {
            // The pass over what the pet will take as occupied
            // (`crate::pet_chrome`): the focused pane in frame cells, the
            // strip above it; this write's cells are noted below, the
            // Autopilot menu's after it, and the menu's splice ends the pass.
            let strip = ws
                .input_scratch
                .cells
                .len()
                .saturating_sub(usize::from(ws.rows));
            ws.pet_chrome
                .begin(plan.leaves.iter().find(|leaf| leaf.focused).map(|leaf| {
                    (
                        strip + leaf.rect.origin.y.round().max(0.0) as usize,
                        leaf.rect.origin.x.round().max(0.0) as usize,
                        leaf.rect.size.height.round().max(1.0) as usize,
                        leaf.rect.size.width.round().max(1.0) as usize,
                    )
                }));
            std::mem::take(&mut ws.claude_footer_lists)
        }) else {
            return;
        };
        // The grid this paint lays the glass on: its marks are fitted to its
        // cell, and its lists are stamped with it (`settle_glass_hover`).
        let grid = self.glass_grid(wid);
        self.splice_claude_footer_with(wid, plan, route, &mut lists, clock, grid);
        // A switch or a lockup hovered that this paint moved from under a
        // still pointer is let go, and after a re-grid what is under it now
        // is hovered (`settle_glass_hover`).
        self.settle_glass_hover(wid, grid);
        // A process whose top rule showed Claude's top-effort tag: evidence
        // the key exists for it, which its ghost waits for.
        for (session, pgid) in lists.top_seen.drain(..) {
            self.claude_facts.entry(session).or_default().top_seen = Some(pgid);
        }
        // The launch card's amends rang their watchers in the hold that took
        // the owed wake; the loop follows the log (its settle tick, the
        // clause) on its next turn (`Wake::SessionLogMoved`), and a loop with
        // no proxy (a headless test App) now.
        for session in lists.log_owed.drain(..) {
            match self.proxy.as_ref() {
                Some(proxy) => {
                    let _ = proxy.send_event(Wake::SessionLogMoved { session });
                }
                None => self.on_session_log_moved(session),
            }
        }
        lists.clear();
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.claude_footer_lists = lists;
        }
    }

    /// [`Self::splice_claude_footer`] on the window's lists (`lists`, empty
    /// on entry).
    fn splice_claude_footer_with(
        &mut self,
        wid: WindowId,
        plan: &crate::tab_model::VisibleLeafPlan,
        route: VisibleContentRoute,
        lists: &mut SpliceLists,
        clock: &dyn Fn() -> footer::ReadClock<'static>,
        grid: Option<crate::app_mouse::GlassGrid>,
    ) {
        // Every Claude Code pane on the glass, for the harness card's pass.
        let mut cards = Vec::new();
        if let Some(ws) = self.windows.get_mut(&wid) {
            undo(&mut ws.claude_footer_undo, &mut ws.input_scratch);
            // The harness card's light comes off the same way (`crate::claude_card`).
            crate::claude_card::strip(&mut ws.claude_card, &mut ws.input_scratch);
            ws.claude_lights.clear_hits();
            ws.claude_footer_texts.borrow_mut().trim(plan.leaves.len());
            cards = ws.claude_card.take_leaves();
        }
        // The footer's facts are keyed by the foreground group the status sweep keeps
        // current; with `tab_status` off that sweep does not run, the group goes stale, and
        // a new Claude Code in the tab would be painted with the last one's facts. No sweep:
        // no footer, lights or harness card.
        if !self.config.tab_status_or_default() {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.claude_footer_texts.borrow_mut().trim(0);
                ws.claude_card.hide_all();
            }
            return;
        }
        let mark_fg = crate::chrome_band::band_colors(self.theme).accent;
        // The window's cell in device pixels: what the top rules' mark
        // sprites are fitted to (`cli_marks::fit`).
        let cell = {
            let (cw, ch) = grid.map_or_else(|| self.win_cell_size(wid), |g| g.cell());
            (
                u16::try_from(cw).unwrap_or(u16::MAX),
                u16::try_from(ch).unwrap_or(u16::MAX),
            )
        };
        let usage = self.config.claude_footer_usage();
        // The pass's claim clock: every pane's Autopilot read shares it.
        let claims = crate::autopilot::ClaimClock::default();
        {
            let Some(ws) = self.windows.get(&wid) else {
                return;
            };
            let frame_rows = ws.input_scratch.cells.len();
            let strip = frame_rows.saturating_sub(usize::from(ws.rows));
            for (leaf_index, leaf) in plan.leaves.iter().enumerate() {
                let Some(session) = self
                    .view_store
                    .get(leaf.view)
                    .copied()
                    .and_then(crate::tab_model::View::terminal_session)
                else {
                    continue;
                };
                let Some(entry) = self.pool.get(session) else {
                    continue;
                };
                let Some(source) = frame_source(ws, route, leaf_index, leaf) else {
                    continue;
                };
                if source.display_offset != 0 {
                    continue;
                }
                // What the footer shows — its facts, filled from the launch
                // card kept for them (`FooterFacts::filled_from`), the live
                // upgrade's note —
                // taken into the pane's footer ([`PaneFooter`]) only where
                // one of them moved: a frame at rest copies none of them. And
                // the process the facts are for (its group), which the
                // harness card matches its card against; the side's flight
                // table, shared, for the agents panel's dots; and the harness
                // card's gate (card engine §3.8): the current holder image's
                // token, reader and build policy, facts or none — a drawer
                // whose facts have not landed takes the card's path all the
                // same. The pane's one timeline lock this frame: the PTY
                // reader holds it while Claude streams.
                let (held, agent, flights, gate, identified, muse_cards_need_account) = {
                    let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
                    #[cfg(test)]
                    crate::work_counts::footer_timeline_locked();
                    if timeline.claude_footer().is_none()
                        && timeline.agent().program_pgid > 0
                        && let Some(prog @ (aterm_phase::Program::Agy | aterm_phase::Program::Muse)) =
                            timeline
                                .agent()
                                .program
                                .as_deref()
                                .and_then(aterm_phase::program_of)
                    {
                        let pgid = timeline.agent().program_pgid;
                        let prefix = match prog {
                            aterm_phase::Program::Muse => "muse",
                            _ => "agy",
                        };
                        timeline.set_claude_footer(
                            pgid,
                            None,
                            Some(FooterFacts {
                                model_open: true,
                                effort_open: true,
                                owner: Some(footer::FactsOwner {
                                    floor: None,
                                    session_id: format!("{prefix}-{pgid}"),
                                }),
                                subscription: Some(
                                    aterm_agent::harness::profile::SubscriptionDisplay::default(),
                                ),
                                ..FooterFacts::default()
                            }),
                        );
                    }
                    let held = timeline.claude_footer().is_some_and(|facts| {
                        let mut texts = ws.claude_footer_texts.borrow_mut();
                        let footer = texts.footer(leaf_index, session);
                        footer.note_facts(
                            facts,
                            timeline.claude_card(),
                            usage,
                            self.fact_mask(session),
                        );
                        footer.note_upgrade(timeline.upgrade_note());
                        true
                    });
                    (
                        held,
                        (
                            crate::harness_host::agent_of(timeline.agent()),
                            timeline.agent().program_pgid,
                        ),
                        timeline.claude_side().and_then(|side| side.flights.clone()),
                        timeline.card_gate(),
                        // The program the top line's lockup names: the
                        // agent that owns the prompt, admitted or not (a
                        // vendor `codex` wears its lockup too).
                        crate::harness_host::identified_agent(timeline.agent()),
                        timeline.muse_cards_need_account(),
                    )
                };
                // The process group Claude Code runs as: what a key's latch
                // and its evidence are kept for (`crate::claude_slash`).
                let program_pgid = agent.1;
                let row_off = leaf.rect.origin.y.round().max(0.0) as usize;
                let col_off = leaf.rect.origin.x.round().max(0.0) as usize;
                let pane_rows = leaf.rect.size.height.round().max(1.0) as usize;
                let pane_cols = leaf.rect.size.width.round().max(1.0) as usize;
                let pane = crate::claude_card::PaneGeom {
                    strip,
                    row_off,
                    col_off,
                    rows: pane_rows,
                    cols: pane_cols,
                };
                let region = PaneTextRegion {
                    strip,
                    row_off,
                    col_off,
                    rows: pane_rows,
                    cols: pane_cols,
                };
                let blank = source.implicit_blank;
                // Gemini CLI and Codex hold no footer facts: their top line is
                // spliced before either gate's `continue` (design §6.2), the
                // card's path untouched.
                let bare_top = match identified {
                    Some(prog @ (aterm_phase::Program::Gemini | aterm_phase::Program::Codex)) => {
                        Some(prog)
                    }
                    _ => None,
                };
                if !held && gate.is_none() {
                    // No drawer and no facts: a card this window keeps for
                    // the session left by its identity (Claude stopped or
                    // gone), which bounds its next show
                    // (`claude_card::RESHOW_MS`). Nothing is read.
                    if ws.claude_card.keeps(session) {
                        cards.push(crate::claude_card::drawerless_leaf(
                            leaf_index, session, pane,
                        ));
                    }
                    if let Some(prog) = bare_top {
                        let agent = crate::harness_host::admitted_as(&entry.ctx, agent.0, agent.1);
                        let reading = self.session_autopilot_for(session, agent, &claims);
                        let (switch, palette) =
                            self.bare_top_harness(ws, session, prog, agent, reading);
                        let mut texts = ws.claude_footer_texts.borrow_mut();
                        texts.rows(leaf_index, session, &ws.input_scratch, region);
                        lists.top_only.push(leaf_index);
                        self.splice_top_line(
                            ws,
                            wid,
                            &mut texts,
                            lists,
                            &TopLineAt {
                                leaf_index,
                                session,
                                program_pgid,
                                program: prog,
                                build: top_build(prog, agent),
                                region,
                                pane,
                                blank,
                                switch,
                                palette,
                                held: false,
                                cell,
                                mark_fg,
                            },
                        );
                    }
                    continue;
                }
                // The agent as the host admits it, read after the timeline's
                // lock.
                let agent = crate::harness_host::admitted_as(&entry.ctx, agent.0, agent.1);
                // The session's Autopilot, read ONCE for this frame with no
                // lock held (its own leaf lock alone): what the harness's
                // chips say and whether the harness card wraps the pane.
                let reading = self.session_autopilot_for(session, agent, &claims);
                let (word, view, supervised, _) = reading;
                // The gate's one verdict this frame, for the card and the
                // footer's kept card alike, made from the frame's own fence.
                let mut check = crate::claude_card::GateCheck::new(
                    gate,
                    crate::claude_card::FenceFrame::of(source),
                );
                let mut texts = ws.claude_footer_texts.borrow_mut();
                texts.rows(leaf_index, session, &ws.input_scratch, region);
                let texts = &mut *texts;
                // Nothing since this process started named its model or
                // effort: its launch card may, read off a live REPL
                // (`footer::launch_card_read`; agy's and Muse Code's live
                // status row may name the model). It is read again on every
                // rebuild of the pane's text while they stay open (every
                // frame that reuses the text shares the reading), and the
                // NEWEST reading of the drawer's own rows is kept
                // (`note_claude_card`): a predecessor's card still on the
                // glass, or a card quoted under the drawer's, is never the
                // drawer's. A frame with no card to read (it scrolled away, a
                // message sits under it) keeps the last reading: the card
                // still names what the process started on. A card kept anew
                // shows now. A reading the timeline already keeps for these
                // facts is no news, and takes no lock.
                if let Some(footer) = texts.footers.get(leaf_index).filter(|_| held)
                    && let Some(owner) = footer.card_owner()
                    && let Some((program, read)) = texts.footer_card(leaf_index, session)
                {
                    let card = &read.card;
                    let same_as_kept = footer.kept_card().is_some_and(|kept| {
                        kept.model == card.model
                            && kept.effort == card.effort
                            && (kept.version == card.version || card.version.is_empty())
                    });
                    // Only the drawer's own reading is kept: its card the one
                    // the gate's verdict admitted, its status row where the
                    // drawer drew (`GateCheck::admits_reading`, card engine
                    // §3.8). Judged before anything is copied or locked, so a
                    // reading the gate refuses — a predecessor's card, a
                    // quoted one, a Claude Code that is no drawer — costs a
                    // settled frame nothing.
                    if !same_as_kept
                        && check.admits_reading(
                            texts,
                            leaf_index,
                            session,
                            *program,
                            read.banner,
                            read.status,
                        )
                    {
                        let mut card = card.clone();
                        if card.version.is_empty()
                            && let Some(kept) = footer.kept_card()
                            && !kept.version.is_empty()
                        {
                            card.version.clone_from(&kept.version);
                        }
                        let owner = owner.clone();
                        let mut timeline =
                            entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
                        #[cfg(test)]
                        crate::work_counts::footer_timeline_locked();
                        if let Some((pgid, _)) = timeline.claude_footer_identity()
                            && timeline.note_claude_card(pgid, &owner, card)
                        {
                            // The card's amend of the session log: its owed
                            // wake rings its watchers in this hold, and the
                            // loop follows the log after the frame.
                            if timeline.slog_take_wake() {
                                lists.log_owed.push(session);
                            }
                            if let Some(facts) = timeline.claude_footer() {
                                texts.footer(leaf_index, session).note_facts(
                                    facts,
                                    timeline.claude_card(),
                                    usage,
                                    self.fact_mask(session),
                                );
                            }
                        }
                    }
                }
                // A footer held for facts reads its account and context off
                // the glass; a drawer with no facts has no footer to fill.
                // Read once per rebuild of the pane's rows and compared with
                // what the footer shows: a frame at rest scans and copies
                // nothing (THE FOOTER AT REST COSTS NOTHING).
                if held && texts.panes.get(leaf_index).is_some_and(Option::is_some) {
                    let shown = texts.footers[leaf_index].shown();
                    // agy's launch card and Muse Code's `/status` card each
                    // name their signed-in account on the glass; Muse
                    // Code's `/usage` card names its weekly quota beside
                    // it. Claude Code's account is its profile's, read on
                    // the resolver thread instead.
                    let want_acct = matches!(
                        agent,
                        Some(aterm_phase::Program::Agy | aterm_phase::Program::Muse)
                    ) && shown.subscription.is_some();
                    // The context fact follows Claude Code's own indicator: a reading or
                    // nothing, so whether one is shown yet is no gate. agy and Muse Code
                    // draw no such indicator (none in any screen measured of either).
                    let want_ctx = agent == Some(aterm_phase::Program::Claude);
                    if want_acct || want_ctx {
                        let screen_acct = want_acct
                            .then(|| texts.screen_account(leaf_index, agent))
                            .flatten();
                        // Muse Code's login store names its current
                        // account; a `/status` card naming another is the
                        // previous account's historical glass after a
                        // switch, so none of that card set grafts beside
                        // the store's account. It may still fill an address
                        // the store has not named.
                        let stale_muse_cards = agent == Some(aterm_phase::Program::Muse)
                            && screen_acct.is_some_and(|(email, _)| {
                                shown
                                    .subscription
                                    .as_ref()
                                    .and_then(|sub| sub.email.as_deref())
                                    .is_some_and(|current| current != email)
                            });
                        let screen_acct = (!stale_muse_cards).then_some(screen_acct).flatten();
                        let screen_weekly = (agent == Some(aterm_phase::Program::Muse)
                            && want_acct
                            && !stale_muse_cards
                            && !muse_cards_need_account)
                            .then(|| texts.usage_weekly(leaf_index))
                            .flatten()
                            .map(|reading| {
                                muse_weekly_observation(reading, shown.subscription.as_ref(), clock)
                            });
                        // A `/usage` card alone names the tier too, with no
                        // `/status` card's account row for it to ride on.
                        let screen_plan = (agent == Some(aterm_phase::Program::Muse)
                            && want_acct
                            && !stale_muse_cards)
                            .then(|| match screen_acct {
                                Some((_, plan)) => plan,
                                None if !muse_cards_need_account => {
                                    texts.subscription_plan(leaf_index)
                                }
                                None => None,
                            })
                            .flatten();
                        let screen_ctx = want_ctx
                            .then(|| texts.context_reading(leaf_index))
                            .flatten();
                        let need_acct = screen_acct.is_some_and(|(email, plan)| {
                            shown.subscription.as_ref().is_some_and(|sub| {
                                sub.email.as_deref() != Some(email) || sub.plan.as_deref() != plan
                            })
                        }) || screen_plan.is_some_and(|plan| {
                            shown
                                .subscription
                                .as_ref()
                                .is_some_and(|sub| sub.plan.as_deref() != Some(plan))
                        }) || screen_weekly.is_some_and(|(weekly, reset)| {
                            shown.subscription.as_ref().is_some_and(|sub| {
                                sub.weekly.as_deref() != weekly
                                    || sub
                                        .weekly_reset
                                        .as_ref()
                                        .map(|reset| (reset.words.as_str(), reset.at))
                                        != reset
                            })
                        });
                        // A new reading is written; a reading Claude Code no longer shows
                        // (as after `/compact`) goes. A box over the composer, a menu in
                        // its footer row's place, or another of its notices in the
                        // indicator's slot hides it without its going: the reading stays
                        // (`footer::context_gone`). Asked only while a reading is shown, so
                        // a pane with none reads nothing more per frame, and read at most
                        // once per rebuild of the rows, so a frame at rest reads nothing.
                        let need_ctx = match screen_ctx {
                            Some(pct) => context_left_of(shown.context.as_deref()) != Some(pct),
                            None => {
                                want_ctx
                                    && shown.context.is_some()
                                    && texts.indicator_gone(leaf_index)
                            }
                        };
                        if need_acct || need_ctx {
                            let mut timeline =
                                entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
                            if let Some((pgid, started)) = timeline.claude_footer_identity()
                                && let Some(mut facts) = timeline.claude_footer().cloned()
                                // A resolver can replace the account after this
                                // frame took its facts. No glass reading taken
                                // against that snapshot may amend its successor.
                                && Some(&facts) == texts.footers[leaf_index].published()
                                && timeline.muse_cards_need_account() == muse_cards_need_account
                            {
                                if let Some((email, plan)) = screen_acct
                                    && let Some(sub) = facts.subscription.as_mut()
                                {
                                    sub.email = Some(email.to_owned());
                                    sub.plan = plan.map(str::to_owned);
                                }
                                if let Some(plan) = screen_plan
                                    && let Some(sub) = facts.subscription.as_mut()
                                {
                                    sub.plan = Some(plan.to_owned());
                                }
                                if let Some((weekly, reset)) = screen_weekly
                                    && let Some(sub) = facts.subscription.as_mut()
                                {
                                    sub.weekly = weekly.map(str::to_owned);
                                    sub.weekly_reset = reset.map(|(words, at)| {
                                        aterm_agent::harness::profile::WeeklyReset {
                                            words: words.to_owned(),
                                            at,
                                        }
                                    });
                                }
                                if need_ctx {
                                    facts.context = screen_ctx.map(context_words);
                                }
                                timeline.set_claude_footer(pgid, started, Some(facts));
                                if let Some(facts) = timeline.claude_footer() {
                                    texts.footer(leaf_index, session).note_facts(
                                        facts,
                                        timeline.claude_card(),
                                        usage,
                                        self.fact_mask(session),
                                    );
                                }
                            }
                        }
                    }
                }
                // The harness card reads the same rows, rect and strip, judged
                // by the gate read under that lock — never by the footer's
                // facts, whose build is the rule's to show (`filled_from`
                // fills the model and effort alone).
                cards.push(crate::claude_card::card_leaf(
                    texts,
                    self.config.claude_footer_card(),
                    leaf_index,
                    session,
                    pane,
                    blank,
                    view,
                    supervised,
                    &mut check,
                ));
                // The footer's row is the facts' alone; Gemini's and Codex's
                // top line is theirs all the same (design §6.2).
                if !held {
                    if let Some(prog) = bare_top {
                        let (switch, palette) =
                            self.bare_top_harness(ws, session, prog, agent, reading);
                        lists.top_only.push(leaf_index);
                        self.splice_top_line(
                            ws,
                            wid,
                            texts,
                            lists,
                            &TopLineAt {
                                leaf_index,
                                session,
                                program_pgid,
                                program: prog,
                                build: top_build(prog, agent),
                                region,
                                pane,
                                blank,
                                switch,
                                palette,
                                held: false,
                                cell,
                                mark_fg,
                            },
                        );
                    }
                    continue;
                }
                let version = texts.footers[leaf_index].shown().version.as_deref();
                // The rule to write into, found by structure, behind the
                // reader's panic fence (`read_footer`) — a screen that panics
                // it paints nothing — unless the chrome claimed that row: the
                // reading the rows' rebuild took or kept ([`Readings`]).
                let Some(readings) = texts.readings(leaf_index) else {
                    continue;
                };
                let read = readings.rule;
                if read.is_some_and(|r| !r.mode_row) {
                    texts.note_drift(leaf_index, session, version);
                }
                let on_glass = read
                    .map(|r| r.rule)
                    .filter(|&r| !self.chrome_owns_terminal_row(wid, row_off + r));
                let rule = on_glass.and_then(|r| {
                    let frame_row = strip + row_off + r;
                    let (run, rule) = texts.rule_run(
                        leaf_index,
                        &ws.input_scratch,
                        frame_row,
                        (col_off, pane_cols),
                        &mut lists.mask,
                    )?;
                    Some((frame_row, row_off + r, run, rule))
                });
                if on_glass.is_some() && rule.is_none() {
                    lists.bare.push(session);
                }
                // The harness holds the session: agy's and Muse's glass
                // transcript stands. With no host nothing holds it.
                let harness_on = view.carries() && supervised;
                // The switch says whether the harness carries the session,
                // and turns it on or off (`autopilot::HarnessSwitch`): drawn
                // wherever a host is.
                let switch = crate::autopilot::HarnessSwitch::of(view).map(|face| {
                    let (face, hover) = ws.claude_lights.switch_shown(session, face);
                    SwitchPaint { face, hover }
                });
                // The harness glass's theme: the card's palette wherever the
                // harness's glass is worn — Off's white with no claim — and
                // the unhosted window's own where its policy would supervise
                // the agent (`harness_theme`, the one predicate), which the
                // agents panel wears. The composer's border wears it only
                // where its switch stands: with no host the rules carry no
                // branding. The transcript rewrites keep to where the
                // harness holds the session: Off keeps the raw transcript.
                let unhosted_on = self.unhosted_glass(agent);
                let theme = crate::autopilot::harness_theme(view, supervised, word, unhosted_on);
                let border = theme.filter(|_| switch.is_some());
                // The pill relabel's gate, read once: the relabel below is
                // collected only under it, and the mode light's word
                // (`FooterPane::relabelled`) follows the relabel the glass
                // actually takes — a pill left as Claude drew it (a
                // selection, the cursor on its row, a band over it, a cell
                // the write would skip) keeps Claude's word in the light too.
                let gate = agent.is_some_and(|prog| relabels_pill(prog, border.is_some()));
                let mut relabelled = false;
                let top_rule = readings.reach.and_then(|r| r.rules()).map(|(top, _)| top);
                if let Some(
                    prog @ (aterm_phase::Program::Claude
                    | aterm_phase::Program::Agy
                    | aterm_phase::Program::Muse),
                ) = agent
                {
                    let rule_cell = rule.as_ref().map_or(blank, |(_, _, _, rc)| *rc);
                    // The top rule's lockup and switch (`splice_top_line`).
                    self.splice_top_line(
                        ws,
                        wid,
                        texts,
                        lists,
                        &TopLineAt {
                            leaf_index,
                            session,
                            program_pgid,
                            program: identified.unwrap_or(prog),
                            build: top_build(identified.unwrap_or(prog), agent),
                            region,
                            pane,
                            blank,
                            switch,
                            palette: border,
                            held: true,
                            cell,
                            mark_fg,
                        },
                    );
                    if harness_on
                        && matches!(prog, aterm_phase::Program::Agy | aterm_phase::Program::Muse)
                        && let Some(Some(cached)) = texts.panes.get(leaf_index)
                    {
                        collect_agy_transcript_edits(
                            &ws.input_scratch,
                            prog,
                            &cached.rows,
                            region,
                            top_rule,
                            blank,
                            rule_cell,
                            mark_fg,
                            &mut lists.agy_edits,
                        );
                    }
                    // The agents panel under the footer wears the theme too
                    // (`crate::claude_panel`): read once per rebuild of the
                    // rows, under the bottom rule the reading holds.
                    if prog == aterm_phase::Program::Claude
                        && let Some(palette) = theme
                        && self.config.claude_footer_agents()
                        && let Some((_, bottom)) = readings.reach.and_then(|r| r.rules())
                        && let Some(read) = texts.agents_panel(
                            leaf_index,
                            session,
                            bottom,
                            &ws.input_scratch,
                            crate::claude_card::PaneGeom {
                                strip,
                                row_off,
                                col_off,
                                rows: pane_rows,
                                cols: pane_cols,
                            },
                            blank,
                        )
                    {
                        lists.panels.push(crate::claude_panel::PanelLeaf {
                            leaf_index,
                            session,
                            pane: crate::claude_card::PaneGeom {
                                strip,
                                row_off,
                                col_off,
                                rows: pane_rows,
                                cols: pane_cols,
                            },
                            blank,
                            palette,
                            read,
                            flights: flights.clone(),
                            skip: 0,
                        });
                    }
                    if gate
                        && let Some(relabel) = readings.relabel
                        && let Some(Some(cached)) = texts.panes.get(leaf_index)
                    {
                        relabelled = collect_pill_relabel(
                            &ws.input_scratch,
                            &cached.rows,
                            region,
                            relabel,
                            |term_row| self.chrome_owns_terminal_row(wid, term_row),
                            blank,
                            &mut lists.relabels,
                            &mut lists.relabel_cells,
                        );
                    }
                }
                // Every Claude Code pane is OBSERVED for its lights, painted
                // rule or not (`App::observe_claude_lights`).
                lists.seen.push(SeenPane {
                    leaf_index,
                    session,
                    lights: readings.lights,
                });
                let Some((frame_row, term_row, run, rule)) = rule else {
                    continue;
                };
                // The live upgrade's note leads the facts (`FooterFacts::upgrade`), worded for
                // the composer as the move's gate reads it (`App::claude_composer_now`: a dim
                // suggestion is no draft), and not while a turn runs: the turn holds the move.
                let footer = texts.footer(leaf_index, session);
                let draft = footer.note.is_some().then(|| {
                    let (screen, empty) = self.claude_composer_now(session)?;
                    (!screen.busy).then_some(!empty)
                });
                footer.lay_upgrade(draft.flatten());
                lists.panes.push(FooterPane {
                    session,
                    leaf_index,
                    frame_row,
                    term_row,
                    col_off,
                    run,
                    rule,
                    blank,
                    autopilot: (word, view),
                    relabelled,
                });
            }
            let seen = &lists.seen;
            let top_only = &lists.top_only;
            // A pane whose card was read with no facts keeps its text (the
            // card's reading is the text cache's), its footer forgotten; one
            // whose top line alone was read keeps its text and that line.
            ws.claude_footer_texts.borrow_mut().retain_active(
                |index| seen.iter().any(|pane| pane.leaf_index == index),
                |index| cards.iter().any(|card| card.reads(index)),
                |index| top_only.contains(&index),
            );
        }
        for pane in lists.seen.drain(..) {
            self.observe_claude_lights(wid, pane.session, pane.lights);
        }
        for session in lists.bare.drain(..) {
            self.note_claude_footer_row(wid, session, 0);
        }
        // The harness card: its light now, its line in the footer's write.
        self.compose_claude_cards(wid, cards);
        // The agents panels: their ink and light now, recorded with the card's.
        self.compose_claude_panels(wid, &mut lists.panels);
        let lines = self.windows.get(&wid).is_some_and(|ws| {
            ws.claude_card.lines().next().is_some()
                || ws.claude_card.notice_edits().next().is_some()
        });
        // Nothing to write: no pane's bottom rule, no card line or banner, no
        // transcript edit, no relabel — and no top rule's border, ghost, key
        // or switch, which stand where a band covers the bottom rule alone (a
        // find bar floated to the pane's bottom).
        if lists.panes.is_empty()
            && !lines
            && lists.agy_edits.is_empty()
            && lists.relabels.is_empty()
            && lists.tops.is_empty()
            && lists.top_hits.is_empty()
            && lists.border_toggles.is_empty()
        {
            return;
        }
        for pane in lists.panes.drain(..) {
            // The rule's room, and the chips' within it (`footer::fit_rule`). Only the chips
            // must fit for the block to be drawn; the title comes along only where it fits
            // too, so pointing at a chip never pushes the chips off. No chip, no block.
            let room = footer::rule_room(&pane.run);
            let Some(chips_room) = self.windows.get(&wid).and_then(|ws| {
                let mut texts = ws.claude_footer_texts.borrow_mut();
                Some(texts.footer_mut(pane.leaf_index)?.chips_room(room))
            }) else {
                continue;
            };
            // The rule is on the glass whether or not anything fits it: the
            // keyboard's chord is the lights' there (`on_key_claude_lights`).
            self.note_claude_footer_row(wid, pane.session, chips_room);
            self.note_claude_autopilot(wid, pane.session, pane.autopilot, pane.relabelled);
            self.note_claude_team(wid, pane.session);
            let block = self.claude_lights_block(
                wid,
                pane.session,
                pane.leaf_index,
                pane.blank,
                chips_room,
            );
            // A block that gave up a chip for room holds all of it
            // (`Block::holds`): the facts went before the chips, and stay
            // gone beside fewer of them. The composition is the pane's
            // footer's, kept while the facts, the room, the rule, the ground
            // and every cell of the block are the last frame's
            // ([`PaneFooter::compose`]).
            // The fact hovered or selected here is drawn lit.
            let composed = self.windows.get(&wid).and_then(|ws| {
                let lit = ws.claude_lights.lit_fact(pane.session);
                let mut texts = ws.claude_footer_texts.borrow_mut();
                Some(texts.footer_mut(pane.leaf_index)?.compose(
                    room,
                    block.as_ref(),
                    pane.rule,
                    pane.blank,
                    mark_fg,
                    lit,
                ))
            });
            let Some(Composed {
                cells,
                lights_at,
                title,
                glass,
                facts_at,
                spans,
            }) = composed.filter(|c| c.cells > 0)
            else {
                if let Some(block) = block {
                    self.recycle_claude_lights_block(wid, block);
                }
                continue;
            };
            // Right-aligned against the rule's last glyph, which stays.
            let col = pane.col_off + pane.run.end - 1 - cells;
            // Each fact, where the write that carries the facts would put
            // it: the set run starts one blank past `facts_at`.
            let edit_col = if glass.is_empty() {
                col
            } else {
                col + glass.end
            };
            let at = col + facts_at + 1;
            lists
                .facts
                .extend(spans.iter().flatten().map(|span| FactWrite {
                    frame_row: pane.frame_row,
                    term_row: pane.term_row,
                    edit_col,
                    session: pane.session,
                    cols: (
                        at + usize::from(span.cells.0),
                        at + usize::from(span.cells.1),
                    ),
                    kind: span.kind,
                    restores: span.restores,
                }));
            match (block, lights_at) {
                (Some(block), Some(at)) => lists.lit.push(Lit {
                    session: pane.session,
                    frame_row: pane.frame_row,
                    col,
                    term_row: pane.term_row,
                    lights_col: col + at,
                    title: col + title.start..col + title.end,
                    block,
                    shown: false,
                }),
                (Some(block), None) => self.recycle_claude_lights_block(wid, block),
                (None, _) => {}
            }
            lists.rules.push(RuleWrite {
                frame_row: pane.frame_row,
                col,
                leaf_index: pane.leaf_index,
                ground: pane.blank,
                glass,
            });
        }
        {
            let Some(ws) = self.windows.get_mut(&wid) else {
                return;
            };
            let crate::WindowState {
                claude_footer_undo,
                input_scratch,
                claude_card,
                claude_footer_texts,
                claude_lights,
                pet_chrome,
                ..
            } = &mut **ws;
            let texts = claude_footer_texts.borrow();
            // The card's lines and its banner over Claude's update notice
            // first, borrowed from the card, then the rules, borrowed from
            // the panes' footers.
            let card_edits = claude_card
                .lines()
                .map(crate::claude_card::CardLine::edit)
                .chain(claude_card.notice_edits());
            let rules = lists.rules.iter().flat_map(|r| {
                rule_edits(
                    r.frame_row,
                    r.col,
                    texts.composed(r.leaf_index),
                    &r.glass,
                    r.ground,
                )
            });
            let tops = lists.tops.iter().flat_map(|t| {
                rule_edits(
                    t.frame_row,
                    t.col,
                    texts.top_cells(t.leaf_index),
                    &t.glass,
                    t.ground,
                )
            });
            let agy = lists
                .agy_edits
                .iter()
                .map(|e| (e.frame_row, e.col, e.cells.as_slice(), e.ground));
            let relabel_cells = &lists.relabel_cells;
            let relabels = lists.relabels.iter().map(|e| {
                (
                    e.frame_row,
                    e.col,
                    &relabel_cells[e.cells.clone()],
                    e.ground,
                )
            });
            write_edits(
                claude_footer_undo,
                input_scratch,
                card_edits
                    .chain(rules)
                    .chain(tops)
                    .chain(agy)
                    .chain(relabels),
            );
            // A relabel the write took moves the glass's columns off the
            // grid's on its row: the pointer's readers map them back.
            for e in &lists.relabels {
                if claude_footer_undo.took(e.frame_row, e.col) {
                    claude_footer_undo.glass.push(e.shift);
                }
            }
            // A top rule's mark stands only over the cells its write put on
            // the glass (a write refused or never made — a cluster or a
            // combining mark in its span, a selection on the row, which
            // `splice_top_line` turns away — puts none), recorded in the
            // card's own record, so every strip of the card's takes it back
            // (`claude_card::strip`: the top of a frame, of a compose and of
            // this splice, and the capture's one-pane refill, which runs no
            // footer undo).
            for t in &lists.tops {
                if claude_footer_undo.took(t.frame_row, t.col)
                    && let Some(layer) = texts.top_mark(t.leaf_index)
                {
                    claude_card.paint_layer(input_scratch, layer.clone());
                }
            }
            // So with the lockups: only one the glass shows answers the
            // pointer with its hover title (design §7).
            for m in lists.marks.drain(..) {
                if claude_footer_undo.took(m.frame_row, m.edit_col) {
                    claude_lights.mark_hits.push(crate::claude_lights::MarkHit {
                        frame_row: m.frame_row,
                        term_row: m.term_row,
                        cols: m.cols,
                        session: m.session,
                    });
                }
            }
            for r in &lists.rules {
                if let Some(footer) = texts.footers.get(r.leaf_index) {
                    claude_footer_undo.publish_text(
                        input_scratch,
                        r.frame_row,
                        r.col,
                        &footer.rule.paint,
                    );
                }
            }
            // Only a light the glass shows can be pointed at: a pane whose
            // write was skipped (`write_edits`) records none — keyed by the
            // edit's row AND column, since two panes side by side share a
            // frame row.
            let rows = claude_footer_undo.written();
            // Every chrome cell written — the chips, the facts, the card's line,
            // the top border's mark and switch — is the resident pet's occupied
            // ground (`crate::pet_chrome`); a rule's own strokes, tinted or not,
            // are the rule's (`PetChrome::note_painted`). Transcript markdown
            // edits (`agy_edits`) are excluded so transcript text stays normal
            // walkable terrain rather than an impassable chrome wall.
            for r in rows {
                if !lists
                    .agy_edits
                    .iter()
                    .any(|e| e.frame_row == r.frame_row && e.col == r.col_off)
                {
                    pet_chrome.note_painted(r.frame_row, r.col_off, &r.painted);
                }
            }
            // And the harness card's glass — its border, trail, kitty and
            // stars, drawn over cells the grid reads as blank.
            for (row, col, rows, cols) in claude_card.glass_boxes() {
                pet_chrome.note(row, col, rows, cols);
            }
            let took = |at: (usize, usize)| rows.iter().any(|r| (r.frame_row, r.col_off) == at);
            for l in &mut lists.lit {
                l.shown = took((l.frame_row, l.col));
            }
            // So with the facts: only a fact the glass shows is a target.
            for f in lists.facts.drain(..) {
                if took((f.frame_row, f.edit_col)) {
                    claude_lights.fact_hits.push(crate::claude_lights::FactHit {
                        frame_row: f.frame_row,
                        term_row: f.term_row,
                        cols: f.cols,
                        session: f.session,
                        fact: crate::claude_lights::Fact::of(f.kind),
                        restores: f.restores,
                    });
                }
            }
            // The top-effort key's: aterm's ghost where its write took,
            // Claude's own tag where the frame still holds the key's chars.
            for h in lists.top_hits.drain(..) {
                let shown = if h.ghost {
                    rows.iter().any(|r| {
                        r.frame_row == h.frame_row
                            && r.col_off <= h.cols.0
                            && r.col_off + r.painted.len() >= h.cols.1
                    })
                } else {
                    input_scratch
                        .cells
                        .get(h.frame_row)
                        .and_then(|row| row.get(h.cols.0..h.cols.1))
                        .is_some_and(|cells| {
                            cells
                                .iter()
                                .map(|c| c.ch)
                                .eq(aterm_primer::CLAUDE_TOP_EFFORT_KEY.chars())
                        })
                };
                if shown {
                    claude_lights.fact_hits.push(crate::claude_lights::FactHit {
                        frame_row: h.frame_row,
                        term_row: h.term_row,
                        cols: h.cols,
                        session: h.session,
                        fact: crate::claude_lights::Fact::TopEffort,
                        restores: FactMask::default(),
                    });
                }
            }
            // Only a switch the glass shows can be pressed, hovered or read.
            for b in lists.border_toggles.drain(..) {
                if rows.iter().any(|r| {
                    r.frame_row == b.frame_row
                        && r.col_off <= b.cols.start
                        && r.col_off + r.painted.len() >= b.cols.end
                }) {
                    claude_lights.border_toggles.push(b);
                }
            }
            // So with the title slots: only a title the glass shows is read.
            for l in lists.lit.iter().filter(|l| l.shown && !l.title.is_empty()) {
                claude_lights
                    .title_hits
                    .push(crate::claude_lights::TitleHit {
                        frame_row: l.frame_row,
                        term_row: l.term_row,
                        cols: l.title.clone(),
                        session: l.session,
                    });
            }
            // So with the harness card's line and its banner: their ink and
            // their stars go where their cells did, and so do the banner's
            // hits.
            crate::claude_card::ink_lines(claude_card, input_scratch, took);
        }
        for l in lists.lit.drain(..) {
            if l.shown {
                self.note_claude_light_hits(
                    wid,
                    l.session,
                    l.frame_row,
                    l.term_row,
                    l.lights_col,
                    &l.block,
                );
            }
            self.recycle_claude_lights_block(wid, l.block);
        }
    }

    /// `Wake::ClaudeFooter`: a session's footer facts moved — redraw every
    /// window that shows it. The repaint key's footer term is what lets the
    /// redraw through.
    pub(crate) fn on_claude_footer_changed(&mut self, session: u64) {
        // The lights ride this wake too: a toggle's next step, its deadline
        // and a refusal's end all arrive here (`crate::claude_lights`).
        self.drain_claude_lights();
        // A fact's title someone reads is built again from the facts as they
        // now stand (`crate::claude_facts`).
        self.refresh_fact_readings(session);
        // An open fact menu shows the facts as they now stand
        // (`crate::fact_menu`).
        self.refresh_fact_menu(session);
        // New facts can disagree with the drawer image's build: a fact for
        // later, never a gate (card engine §3.10).
        self.note_claude_build_drift(session);
        self.redraw_claude_footer(session);
    }

    /// Redraw every window that shows `session`'s footer, without the lights' half of the
    /// wake: what a moved upgrade note needs (`App::apply_agent_upgrades`).
    pub(crate) fn redraw_claude_footer(&self, session: u64) {
        let mut windows = self.windows_with_focused_session(session);
        for (wid, _) in self.tabs_viewing_session(session) {
            if !windows.contains(&wid) {
                windows.push(wid);
            }
        }
        for wid in windows {
            if let Some(window) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                window.request_redraw();
            }
        }
    }
}

/// A pane's footer reading: where the composer's bottom rule is, and whether
/// a mode row this build reads sits under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RuleRead {
    rule: usize,
    mode_row: bool,
}

type FooterRead = Option<RuleRead>;

/// The footer's reading of a pane's rows: the composer's bottom rule
/// (`aterm_phase::phase::composer_bottom`) and the mode row under it
/// (`footer::mode_row_under`).
#[cfg(test)]
fn read_rule_row(rows: &[String]) -> FooterRead {
    read_rule_under(rows, aterm_phase::phase::FrameReach::of(rows))
}

/// [`read_rule_row`] for a frame already found (`reach`): its bottom rule, and whether a
/// mode row sits under it, read from the rows under the rule alone.
fn read_rule_under(rows: &[String], reach: aterm_phase::phase::FrameReach) -> FooterRead {
    reach.rules().map(|(_, rule)| RuleRead {
        rule,
        mode_row: footer::mode_row_under(rows, rule).is_some(),
    })
}

/// Vendor drift, logged: a Claude Code pane whose composer has a pill-shaped mode row
/// under it that this build does not read (`footer::opens_with_pill_glyph` but no
/// `footer::pill_indicator`). The lights read no mode from such a row; this logs the row
/// once per Claude Code build and row (at most [`DRIFT_ROWS_LOGGED`] rows a run), so a
/// renamed or added mode shows in the log rather than as lights that quietly went grey.
/// Behind the reader's panic fence, like every read of the pane.
fn note_vendor_drift(session: u64, version: Option<&str>, rows: &[String]) {
    static LOGGED: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let unread =
        crate::reader_guard::read_or_none(session, "Claude Code drift check", rows, || {
            unread_pill_row(rows)
        })
        .flatten();
    let Some(row) = unread else {
        return;
    };
    let build = version.unwrap_or("(unknown build)");
    // Keyed by build and the row's head, so one unreadable pill cannot take the only line
    // a build gets and hide a different one seen later.
    let head: String = row.split_whitespace().take(3).collect::<Vec<_>>().join(" ");
    let key = format!("{build}\t{head}");
    let mut logged = LOGGED.lock().unwrap_or_else(|p| p.into_inner());
    if logged.contains(&key) || logged.len() >= DRIFT_ROWS_LOGGED {
        return;
    }
    logged.push(key);
    aterm_log::warn!(
        "claude footer: Claude Code {build} drew a mode row this aterm does not read, so the lights stand down on it: {row:?}"
    );
}

/// How many distinct unreadable rows (per build) one run logs.
const DRIFT_ROWS_LOGGED: usize = 16;

/// The row under the composer that opens with a pill glyph but names no pill
/// this build knows — `None` for every screen the footer can read, and for
/// one with no composer (a box is up) or nothing pill-shaped under it.
fn unread_pill_row(rows: &[String]) -> Option<String> {
    let (_, bottom) = aterm_phase::phase::composer_rules(rows)?;
    let row = rows
        .get(bottom + 1..)?
        .iter()
        .take_while(|r| !r.trim().is_empty())
        .find(|r| footer::opens_with_pill_glyph(r))?;
    // A known pill cut short by a narrow pane is geometry, not drift.
    (footer::pill_indicator(row).is_none() && !footer::is_cut_pill(row))
        .then(|| row.trim_end().to_owned())
}

/// The harness switch's letter at `t` (`0..=1` along its glyphs,
/// `SwitchInk::Arc`) in the session's
/// theme `palette` on the pane whose blank cell is `blank`. On a dark ground
/// the palette's letter ink (`CardPalette::letter`) lifted a quarter toward
/// the pane's ink, as always. On a light ground the card's line letter at
/// rest there (`CardPalette::letter_on`: the paper law's point-mark ink of
/// the print ramp — the stop at full value, yellow bent to amber, darkened
/// along its own hue only as far as WCAG AA's 4.5:1 needs; Off's silver
/// darkened the same way), as the harness card's line letters are
/// (`harness_card`'s `LineInks`): the quarter lift left the old wordmark's
/// yellow and green letters near 1.6:1 on cream paper, washed out beside
/// the card's print rainbow.
fn brand_letter_fg(
    palette: aterm_effects::harness_card::CardPalette,
    t: f32,
    blank: RenderCell,
) -> [u8; 3] {
    let bg = u32::from_be_bytes([0, blank.bg[0], blank.bg[1], blank.bg[2]]);
    let rgb = |c: u32| [(c >> 16) as u8, (c >> 8) as u8, c as u8];
    if aterm_effects::harness_card::dark_ground(bg) {
        mix(rgb(palette.letter(t, true)), blank.fg, 0.25)
    } else {
        rgb(palette.letter_on(t, bg))
    }
}

/// A composer top rule's plain run ([`read_top_run`]): `len` cells of the
/// rule's own glyph `rule` from frame column `col`, none of them a cluster,
/// a combining mark or an image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TopRun {
    col: usize,
    len: usize,
    rule: RenderCell,
    /// Muse Code's rule at rest is labelled (`── Voice input (⌥ + v to
    /// start) ──…`, `aterm_phase::muse::is_idle_voice_rule`): where that
    /// label stands, untouched, the whole rule from its first `─` to its
    /// last, label and all (frame column, cells) — what the lockup takes
    /// where it fits there ([`laid_run`]).
    voice: Option<(usize, usize)>,
}

/// THE RUN A TOP LINE IS LAID ON ([`compose_top`]), and how many of its
/// cells from its left a label takes that the composition writes over.
/// Muse Code's idle voice label gives way to the lockup on the glass
/// (owner, 2026-10-21: the tool's logo stands at the rule's left, as
/// Claude's and agy's do), so where a rung of the ladder lays the lockup on
/// the whole rule ([`TopRun::voice`]), the run is the whole rule, label and
/// all, and every one of the label's cells is written over with the rule.
/// The lockup stands in every state, bare and with no host too, so the
/// label gives way wherever it does. Elsewhere — a voice state with news
/// (`Recording`, `Transcribing`, …, which keeps its words), a rule with no
/// room for the lockup, any other program's rule — the plain run as read,
/// and `0`. The grid keeps Muse Code's row for its readers either way.
fn laid_run(key: &TopKey) -> (TopRun, usize) {
    let run = key.run;
    let lockup = cli_mark(key.program).map(|mark| mark.lockup_cells(key.build));
    let whole = run.voice.filter(|&(_, len)| {
        lay_top(top_span(len, key.face), key.switch, lockup)
            .is_some_and(|(rung, _)| rung.lockup.is_some())
    });
    match whole {
        Some((col, len)) if col < run.col => (TopRun { col, len, ..run }, run.col - col),
        _ => (run, 0),
    }
}

/// The ghost's first cell in a run of `len` cells under `face`, where it is
/// drawn: right-aligned, one `─` after it.
fn ghost_at(len: usize, face: KeyFace) -> Option<usize> {
    face.ghost
        .filter(|_| len >= GHOST_RUN)
        .map(|_| len - GHOST_CELLS - 1)
}

/// The harness's span in a run of `len` cells under `face`: the run, short
/// of the ghost ([`ghost_at`]).
fn top_span(len: usize, face: KeyFace) -> usize {
    ghost_at(len, face).unwrap_or(len)
}

/// What a CLI's top line is drawn as ([`top_line`], [`read_top_run`]): what
/// aterm writes its lockup and switch on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TopLine {
    /// A `─` rule on the pane's ground: Claude Code's, agy's and Muse Code's
    /// composer top rule, Gemini CLI's live-zone rule.
    Rule,
    /// A tinted pad row: Codex's composer slab's top pad, every cell a blank
    /// on the slab's own ground — Codex's surface, never tinted.
    Slab,
}

/// WHERE `program`'S TOP LINE IS on a pane's cached rows, and its kind: one
/// exhaustive match, so a new program does not compile until it is placed.
/// Claude Code's, agy's and Muse Code's is the composer frame's top rule the
/// footer's readings found; Gemini CLI's its live zone's `─` rule above the
/// mode row, never its `▄` input band (`aterm_phase::gemini::top_rule`);
/// Codex's its composer slab's top pad (`aterm_phase::codex::composer_pad`),
/// a candidate the cells must prove ([`read_top_run`]). Gemini's and Codex's
/// are read at most once per rebuild of the rows, behind the reader's panic
/// fence. A shell has none.
fn top_line(program: aterm_phase::Program, cached: &CachedPaneText) -> Option<(usize, TopLine)> {
    use aterm_phase::Program;
    let session = cached.key.session;
    match program {
        Program::Claude | Program::Agy | Program::Muse => cached
            .readings
            .reach
            .and_then(|r| r.rules())
            .map(|(top, _)| (top, TopLine::Rule)),
        Program::Gemini => cached
            .gemini_top
            .get_or_init(|| {
                crate::reader_guard::read_or_none(
                    session,
                    "Gemini CLI top rule",
                    &cached.rows,
                    || aterm_phase::gemini::top_rule(&cached.rows),
                )
                .flatten()
            })
            .map(|row| (row, TopLine::Rule)),
        Program::Codex => cached
            .codex_pad
            .get_or_init(|| {
                crate::reader_guard::read_or_none(
                    session,
                    "Codex composer pad",
                    &cached.rows,
                    || aterm_phase::codex::composer_pad(&cached.rows),
                )
                .flatten()
            })
            .map(|row| (row, TopLine::Slab)),
        Program::Generic => None,
    }
}

/// Whether a selection reaches the pane's columns of frame row `frame_row`
/// ([`App::splice_top_line`]): the renderer draws a selection over the
/// cells, so no cell read shows it.
fn top_line_selected(scratch: &RenderInput, frame_row: usize, region: PaneTextRegion) -> bool {
    scratch
        .selection_row_span(frame_row)
        .is_some_and(|(lo, hi)| {
            usize::from(lo) < region.col_off + region.cols && usize::from(hi) >= region.col_off
        })
}

/// The rightmost plain run of the composer's top line (pane row `top_rule`
/// of `region`) on `scratch`, by its kind:
///
/// * [`TopLine::Rule`]: the rule's own `─` — the style most of its `─`
///   cells wear, its last one's on a tie — over a row whose every cell is on
///   the pane's ground `blank`. `None` where the row holds no `─`, or a cell
///   off the ground (a search highlight; a selection is no cell's, and
///   [`App::splice_top_line`] turns it away before this read is used). For
///   Muse Code (`muse`), also the whole rule past its idle voice label
///   ([`TopRun::voice`]).
/// * [`TopLine::Slab`]: the pane row from its left edge, PROVEN by its cells
///   (§10.2.7): every cell the one blank `' '`, not underlined, on a
///   background that is not the pane's own and is the background of the
///   prompt row's first cell under it, none a cluster or a combining mark;
///   the run ends at the row's first image cell, a picture the program put
///   on its own slab (codex-alab's composer kitty), and an image at the left
///   edge leaves none. Its `rule` is the slab's blank,
///   which stands in for the pane's in every ink and contrast law. Text alone
///   never puts aterm on a Codex row: a blank row on the pane's own ground,
///   a row the slab does not fill reads `None` (and a selection over the pad
///   is turned away before, [`App::splice_top_line`]).
fn read_top_run(
    scratch: &RenderInput,
    region: PaneTextRegion,
    top_rule: usize,
    line: TopLine,
    blank: RenderCell,
    muse: bool,
) -> Option<TopRun> {
    #[cfg(test)]
    crate::work_counts::footer_top_read();
    let frame_row = region.strip + region.row_off + top_rule;
    let frame_cells = scratch.cells.get(frame_row)?;
    let pane_end = frame_cells.len().min(region.col_off + region.cols);
    let pane_slice = frame_cells.get(region.col_off..pane_end)?;
    if line == TopLine::Slab {
        let slab = *pane_slice.first()?;
        let prompt = scratch
            .cells
            .get(frame_row + 1)?
            .get(region.col_off)
            .filter(|_| top_rule + 1 < region.rows)?;
        let span = region.col_off..region.col_off + region.cols;
        let proven = pane_slice.len() == region.cols
            && slab.ch == ' '
            && slab.underline == aterm_core::terminal::UnderlineStyle::None
            && slab.bg != blank.bg
            && prompt.bg == slab.bg
            && pane_slice.iter().all(|c| *c == slab)
            && !touches(&scratch.clusters, frame_row, &span)
            && !touches(&scratch.combining, frame_row, &span);
        // A picture the program placed over its own slab (codex-alab's
        // composer kitty, a 9×5-cell PNG at the pad row's right end) is the
        // program's ink: the run stops at its first cell, and a picture at
        // the slab's left edge leaves no run.
        let end = scratch
            .images
            .get(frame_row)
            .into_iter()
            .flatten()
            .map(|(col, _)| *col)
            .filter(|col| span.contains(col))
            .min()
            .unwrap_or(span.end);
        // The slab's blank stands in for the pane's in every ink law, which
        // lifts and quiets toward its ink, so that ink must read on the slab.
        // The slab cell's own foreground is only what Codex erased it with:
        // after the theme moves under a running Codex (an appearance pair
        // turning light at dusk), Codex keeps the slab it chose at launch
        // and its ink is the new theme's, `#2b2d33` on `#2d2f33` (codex-alab
        // 0.164.0-dev, 2026-10-21), which left the switch's letters at 3.1:1.
        // The pane's own pair, its ink or its ground, whichever reads farther
        // from the slab, is the ink the laws use.
        let pack = |c: [u8; 3]| u32::from_be_bytes([0, c[0], c[1], c[2]]);
        let reads = |c: [u8; 3]| aterm_effects::color_math::contrast_ratio(pack(c), pack(slab.bg));
        let ink = if reads(blank.bg) > reads(blank.fg) {
            blank.bg
        } else {
            blank.fg
        };
        return (proven && end > region.col_off).then_some(TopRun {
            col: region.col_off,
            len: end - region.col_off,
            rule: RenderCell { fg: ink, ..slab },
            voice: None,
        });
    }
    if pane_slice.is_empty() || pane_slice.iter().any(|c| c.bg != blank.bg) {
        return None;
    }
    let first = pane_slice.iter().find(|c| c.ch == '\u{2500}').copied()?;
    let last = pane_slice
        .iter()
        .rev()
        .find(|c| c.ch == '\u{2500}')
        .copied()
        .unwrap_or(first);
    let count = |style: &RenderCell| pane_slice.iter().filter(|c| *c == style).count();
    let rule = if first == last || count(&last) >= count(&first) {
        last
    } else {
        first
    };
    // The rightmost contiguous run of plain `─` cells in `rule`'s style.
    let mut best = 0..0;
    let mut cur_start = None;
    for (idx, cell) in pane_slice.iter().enumerate() {
        let col = region.col_off + idx;
        let plain = *cell == rule
            && !touches(&scratch.clusters, frame_row, &(col..col + 1))
            && !touches(&scratch.combining, frame_row, &(col..col + 1))
            && !touches(&scratch.images, frame_row, &(col..col + 1));
        if plain {
            let s = *cur_start.get_or_insert(idx);
            if idx + 1 - s >= best.len() {
                best = s..idx + 1;
            }
        } else {
            cur_start = None;
        }
    }
    let untouched = |idx: usize| {
        let col = region.col_off + idx;
        !touches(&scratch.clusters, frame_row, &(col..col + 1))
            && !touches(&scratch.combining, frame_row, &(col..col + 1))
            && !touches(&scratch.images, frame_row, &(col..col + 1))
    };
    let voice = muse
        .then(|| {
            let first = pane_slice.iter().position(|c| c.ch == '\u{2500}')?;
            let last = pane_slice.iter().rposition(|c| c.ch == '\u{2500}')?;
            (first < best.start
                && (first..=last).all(untouched)
                && aterm_phase::muse::is_idle_voice_rule(
                    &pane_slice.iter().map(|c| c.ch).collect::<String>(),
                ))
            .then_some((region.col_off + first, last + 1 - first))
        })
        .flatten();
    Some(TopRun {
        col: region.col_off + best.start,
        len: best.len(),
        rule,
        voice,
    })
}

/// THE HARNESS SWITCH'S PAINT on one pane's composer top rule
/// ([`compose_top`]): what it says, and whether the pointer is over it. The
/// switch carries no branding of its own: the theme the line wears
/// ([`TopKey::palette`]) inks it, and the lockup at the line's left names
/// the program ([`lay_top`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SwitchPaint {
    face: crate::autopilot::HarnessSwitch,
    hover: bool,
}

/// How one cell of a harness switch's face is inked ([`switch_face`],
/// [`switch_cell`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SwitchInk {
    /// A glyph of the switch's words and state on a line that wears a theme,
    /// at its stop of the theme's arc (`0..=255`): glyph `k` of the `n` from
    /// the switch's first letter through its state word, blanks skipped,
    /// stands at `t = k/(n−1)` ([`SwitchFace::on_arc`]).
    Arc(u8),
    /// The switch's label (`alab harness`), and the face's blanks.
    Label,
    /// The lit state, `● on`: the accent.
    Lit,
    /// The state the switch is in when off, `○ off`: the label's quiet ink.
    Unlit,
    /// A letter of the `on` button: the ground's ink on the button's fill.
    Button,
    /// A cap of the button (`▐`, `▌`): its fill as ink on the ground, so the
    /// button's ends are half a cell, a pill.
    Cap,
    /// `◐ turning on`: the accent, quieted and floored to 4.5:1
    /// ([`turning_ink`]).
    Turning,
}

/// The cells a switch face may take: the widest, ` alab harness ○ off ▐ on
/// ▌ ` and ` alab harness ◐ turning on `, are 27.
const SWITCH_FACE_MAX: usize = 28;

/// One harness switch face as laid ([`switch_face`]): its cells and where
/// its parts lie in them — the switch itself (label and state, the
/// accessibility tree's switch node) and the `on` button.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SwitchFace {
    cells: [(char, SwitchInk); SWITCH_FACE_MAX],
    len: usize,
    switch: std::ops::Range<usize>,
    button: Option<std::ops::Range<usize>>,
}

impl SwitchFace {
    fn cells(&self) -> &[(char, SwitchInk)] {
        &self.cells[..self.len]
    }

    /// The face's text, for the tests.
    #[cfg(test)]
    fn text(&self) -> String {
        self.cells().iter().map(|(ch, _)| *ch).collect()
    }

    fn push(&mut self, text: &str, ink: SwitchInk) {
        for ch in text.chars() {
            if self.len < SWITCH_FACE_MAX {
                self.cells[self.len] = (ch, ink);
                self.len += 1;
            }
        }
    }

    /// The face on a line that wears a theme: each glyph of the switch's own
    /// cells (its words and its state, blanks skipped) takes its stop of the
    /// theme's arc, `t = k/(n−1)` for glyph `k` of `n` ([`SwitchInk::Arc`]),
    /// so the switch's first glyph meets the rule's `t = 0` and its last the
    /// tail's `t = 1` ([`compose_top`]). The `on` button keeps its pill.
    fn on_arc(&mut self) {
        let span = self.switch.clone();
        let n = self.cells[span.clone()]
            .iter()
            .filter(|(ch, _)| *ch != ' ')
            .count();
        let mut k = 0usize;
        for (ch, ink) in &mut self.cells[span] {
            if *ch == ' ' {
                continue;
            }
            let stop = if n > 1 { k * 255 / (n - 1) } else { 0 };
            *ink = SwitchInk::Arc(u8::try_from(stop).unwrap_or(u8::MAX));
            k += 1;
        }
    }
}

/// How many forms a switch face has, widest first ([`switch_face`]'s
/// `tier`): each gives up words, never the switch, so the switch — the
/// only door to turning the harness on from the pane — goes last.
const SWITCH_TIERS: usize = 4;

/// THE HARNESS SWITCH'S FACE, `tier` steps down from its widest
/// (`0..SWITCH_TIERS`; `None` past the face's last form):
///
/// | switch | 0 | 1 | 2 | 3 |
/// |---|---|---|---|---|
/// | on | ` alab harness ● on ` | ` alab ● on ` | ` ● on ` | |
/// | off | ` alab harness ○ off ▐ on ▌ ` | ` alab ○ off ▐ on ▌ ` | ` ○ off ▐ on ▌ ` | ` ▐ on ▌ ` |
/// | turning on | ` alab harness ◐ turning on ` | ` alab ◐ turning on ` | ` ◐ turning on ` | ` ◐ on ` |
///
/// The switch names the harness alone: the program is the lockup's to name,
/// at the line's left ([`lay_top`]), so the ` ✦ alab kitty · ` wordmark and
/// the program's word left it (owner, 2026-10-21). Each narrowest form is as
/// wide as the one before the lockup (6, 8 and 6 cells), so the switch
/// stands wherever it stood. Off's `on` is the button (owner, 2026-10-20).
fn switch_face(paint: SwitchPaint, tier: usize) -> Option<SwitchFace> {
    use crate::autopilot::HarnessSwitch;
    let mut f = SwitchFace {
        cells: [(' ', SwitchInk::Label); SWITCH_FACE_MAX],
        len: 0,
        switch: 0..0,
        button: None,
    };
    // The label's words at `tier`, given up from the right inward:
    // `alab harness`, then `alab`, then none.
    let label = |f: &mut SwitchFace, words: usize| {
        if words >= 2 {
            f.push("alab harness ", SwitchInk::Label);
        } else if words == 1 {
            f.push("alab ", SwitchInk::Label);
        }
    };
    match paint.face {
        HarnessSwitch::On => {
            let words = [2, 1, 0].get(tier).copied()?;
            f.push(" ", SwitchInk::Label);
            f.switch.start = f.len;
            label(&mut f, words);
            f.push("\u{25cf} on", SwitchInk::Lit);
            f.switch.end = f.len;
            f.push(" ", SwitchInk::Label);
        }
        HarnessSwitch::Off(_) => {
            let words = [2, 1, 0, 0].get(tier).copied()?;
            f.push(" ", SwitchInk::Label);
            f.switch.start = f.len;
            label(&mut f, words);
            if tier < 3 {
                f.push("\u{25cb} off", SwitchInk::Unlit);
                f.switch.end = f.len;
                f.push(" ", SwitchInk::Label);
            } else {
                f.switch.end = f.len;
            }
            let at = f.len;
            push_on_button(&mut f);
            f.button = Some(at..f.len);
            f.push(" ", SwitchInk::Label);
        }
        HarnessSwitch::Turning => {
            let words = [2, 1, 0, 0].get(tier).copied()?;
            f.push(" ", SwitchInk::Label);
            f.switch.start = f.len;
            label(&mut f, words);
            f.push(
                if tier < 3 {
                    "\u{25d0} turning on"
                } else {
                    "\u{25d0} on"
                },
                SwitchInk::Turning,
            );
            f.switch.end = f.len;
            f.push(" ", SwitchInk::Label);
        }
    }
    Some(f)
}

/// The off switch's `on` button ([`switch_face`]): a pill, `▐ on ▌`, its
/// letters the ground's ink on the accent's fill, its caps half a cell of
/// the fill on the ground.
fn push_on_button(f: &mut SwitchFace) {
    f.push("\u{2590}", SwitchInk::Cap);
    f.push(" on ", SwitchInk::Button);
    f.push("\u{258c}", SwitchInk::Cap);
}

/// The widest of `paint`'s faces that leaves the rule `room` cells — two
/// of the rule's own on either side — and its tier: the switch alone, as it
/// stood before the lockup, the law [`lay_top`]'s ladder keeps the switch
/// to (its `alone` rung).
#[cfg(test)]
fn fitted_switch_face(paint: SwitchPaint, room: usize) -> Option<(usize, SwitchFace)> {
    (0..SWITCH_TIERS)
        .filter_map(|tier| Some((tier, switch_face(paint, tier)?)))
        .find(|(_, face)| face.len + 4 <= room)
}

/// A face cell in its ink on `blank`'s ground. Where the line wears a theme
/// (`palette`), the switch's glyphs walk its arc ([`SwitchInk::Arc`]) in the
/// switch's letter law ([`brand_letter_fg`]: Cruise's spectrum, Land's
/// sunset, Off's white moonbow, the print law's inks on paper) — bold while
/// on, regular while off (hands off reads quieter), and quieted 30% toward
/// the ground while it turns on. Bare (no theme), hb-int's law: the label in
/// the agent's ink and bold while on, quiet while off, the lit state and the
/// button in the accent (`mark_fg`), `○ off` quiet — each floored to 4.5:1
/// on the ground, its hue kept ([`legible_on`]). The `on` button is the
/// accent's pill on every theme, its letters the ground's ink on its fill: a
/// control does not change colour with the mode it controls. A hovered
/// switch says what a press acts on: the lit switch underlined and bold, an
/// off switch's button underlined, its fill lifted a quarter toward the
/// pane's ink.
fn switch_cell(
    ch: char,
    ink: SwitchInk,
    paint: SwitchPaint,
    palette: Option<aterm_effects::harness_card::CardPalette>,
    blank: RenderCell,
    mark_fg: [u8; 3],
) -> RenderCell {
    use crate::autopilot::HarnessSwitch;
    // Every bare ink is floored to 4.5:1 on the ground, its hue kept (§8):
    // the quiet label on a theme whose own ink reads low (Solarized Light's
    // 4.1:1, quieted 30%, read 2.8:1), the accent on a ground it was not
    // chosen for, and the pill's fill, whose letters are the ground's ink.
    let accent = || legible_on(mark_fg, blank.bg);
    let quiet = || legible_on(mix(blank.fg, blank.bg, 0.3), blank.bg);
    let agent = || legible_on(mix(accent(), blank.fg, 0.35), blank.bg);
    let fill = || {
        let accent = accent();
        if paint.hover {
            legible_on(mix(accent, blank.fg, 0.25), blank.bg)
        } else {
            accent
        }
    };
    let on = paint.face.is_on();
    let mut cell = RenderCell { ch, ..blank };
    match ink {
        SwitchInk::Arc(stop) => {
            // Laid only on a line that wears a theme; Cruise's arc otherwise.
            let letter = brand_letter_fg(
                palette.unwrap_or(aterm_effects::harness_card::CardPalette::Cruise),
                f32::from(stop) / 255.0,
                blank,
            );
            match paint.face {
                HarnessSwitch::On => {
                    cell.fg = letter;
                    cell.bold = true;
                }
                HarnessSwitch::Off(_) => cell.fg = letter,
                HarnessSwitch::Turning => cell.fg = turning_ink(letter, blank.bg),
            }
        }
        SwitchInk::Label => {
            cell.fg = if on { agent() } else { quiet() };
            cell.bold = on;
        }
        SwitchInk::Lit => {
            cell.fg = accent();
            cell.bold = true;
        }
        SwitchInk::Unlit => cell.fg = quiet(),
        SwitchInk::Button => {
            cell.fg = blank.bg;
            cell.bg = fill();
            cell.bold = true;
            if paint.hover && ch != ' ' {
                cell.underline = aterm_core::terminal::UnderlineStyle::Single;
            }
        }
        SwitchInk::Cap => cell.fg = fill(),
        SwitchInk::Turning => cell.fg = turning_ink(accent(), blank.bg),
    }
    if paint.hover
        && on
        && matches!(ink, SwitchInk::Label | SwitchInk::Lit | SwitchInk::Arc(_))
        && ch != ' '
    {
        cell.bold = true;
        cell.underline = aterm_core::terminal::UnderlineStyle::Single;
    }
    cell
}

/// THE SETTLING SWITCH'S INK (`◐ turning on`, [`switch_cell`]): `ink` — a
/// glyph's stop of the theme's arc, or the bare switch's accent — quieted
/// 30% toward the ground (§4.2), then floored back to 4.5:1 against it, its
/// hue kept (§8): the words a person reads to see the harness turning on
/// stay text, and the quieting, the regular weight and `◐` are the state's
/// cue. A quieted ink that already clears the floor is kept as it is.
fn turning_ink(ink: [u8; 3], ground: [u8; 3]) -> [u8; 3] {
    legible_on(mix(ink, ground, 0.3), ground)
}

/// `ink` floored along its own hue to read 4.5:1 on `ground`
/// ([`aterm_effects::cli_marks::NAME_CONTRAST`], §8): the floor every
/// letter aterm lays on a top line meets. An ink that already reads is kept
/// as it is.
fn legible_on(ink: [u8; 3], ground: [u8; 3]) -> [u8; 3] {
    let pack = |c: [u8; 3]| u32::from_be_bytes([0, c[0], c[1], c[2]]);
    rgb_of(aterm_effects::color_math::floor_contrast_keeping_hue(
        pack(ink),
        pack(ground),
        aterm_effects::cli_marks::NAME_CONTRAST,
    ))
}

/// The mark a program's top line wears ([`aterm_effects::cli_marks`]): one
/// exhaustive match, so a new program does not compile until it is placed.
/// Every program aterm reads a top line for has one; a shell has none.
fn cli_mark(program: aterm_phase::Program) -> Option<CliMark> {
    match program {
        aterm_phase::Program::Claude => Some(CliMark::Claude),
        aterm_phase::Program::Codex => Some(CliMark::Codex),
        aterm_phase::Program::Agy => Some(CliMark::Agy),
        aterm_phase::Program::Muse => Some(CliMark::Muse),
        aterm_phase::Program::Gemini => Some(CliMark::Gemini),
        aterm_phase::Program::Generic => None,
    }
}

/// The build a top line's `program` runs, as the host proves it (design
/// §6.2): ALab's for a Codex the host admits as `agent` — codex-alab, by
/// its proven self-claim, aterm's copy of its record or its ALab image
/// (`crate::harness_host::admitted_as`), the admission that gives its line
/// the switch — so its lockup reads `codex-alab` ([`CliMark::name`]); the
/// vendor's for a vendor `codex`, which no host admits, and for every other
/// program.
fn top_build(program: aterm_phase::Program, agent: Option<aterm_phase::Program>) -> Build {
    match (program, agent) {
        (aterm_phase::Program::Codex, Some(aterm_phase::Program::Codex)) => Build::Alab,
        _ => Build::Vendor,
    }
}

/// What a top line's lockup shows on a rung of the ladder ([`Rung`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lockup {
    /// The icon and the CLI's name, `[##]claude ` (`CliMark::lockup_cells`),
    /// or its ALab build's, `[##]codex-alab `.
    Full,
    /// The icon alone, `[##]` (`cli_marks::ICON_RUN`).
    Icon,
}

/// One rung of THE NARROW LADDER ([`ladder`]): the switch's form (`None`:
/// none drawn), the lockup (`None`: none), the cells before the lockup (its
/// first cell's place in the span) and the span the rung needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rung {
    tier: Option<usize>,
    lockup: Option<Lockup>,
    lead: usize,
    need: usize,
}

/// The most rungs a ladder has: a full rung a switch tier, then the icon,
/// the icon compact and the switch alone.
const LADDER_MAX: usize = SWITCH_TIERS + 3;

/// THE NARROW LADDER of a top line whose switch is `switch` (`None`: no
/// switch, no host to turn a harness on) and whose lockup takes `lockup`
/// cells (`None`: the program wears no mark), widest first; the first rung
/// a span holds wins ([`lay_top`]). The switch's words go first (`harness`,
/// then `alab`, then an off or turning switch's state words), then the CLI's
/// name, then the icon: the switch's last form outlasts all decoration,
/// because it is the pane's only way to turn the harness on (owner,
/// 2026-10-20), and stands at every span its narrowest form needs (`face +
/// 4`, the switch alone as it stood before the lockup). Two cells stay
/// free after the switch on every rung, so the key's mark (the span's last
/// cell) is always a `─`.
///
/// | rung | lockup | tier | lead | gap | tail | span needed |
/// |---|---|---|---|---|---|---|
/// | full s0 … sN | icon and name | 0 … last | 2 | 2 | 2 | lockup + face + 6 |
/// | icon sN | icon | last | 2 | 2 | 2 | 4 + face + 6 |
/// | icon sN compact | icon | last | 1 | 1 | 2 | 4 + face + 4 |
/// | alone sN | none | last | | | 2 | face + 4 |
/// | no switch: full, icon, compact | icon and name, the icon | | 2, 2, 1 | ≥ 2, 2, 1 | | lockup + 4, 8, 6 |
fn ladder(switch: Option<SwitchPaint>, lockup: Option<usize>) -> ([Rung; LADDER_MAX], usize) {
    let none = Rung {
        tier: None,
        lockup: None,
        lead: 0,
        need: usize::MAX,
    };
    let mut rungs = [none; LADDER_MAX];
    let mut n = 0;
    let mut push = |rung: Rung| {
        rungs[n] = rung;
        n += 1;
    };
    let icon = aterm_effects::cli_marks::ICON_RUN;
    match switch {
        Some(paint) => {
            let mut last = None;
            for tier in 0..SWITCH_TIERS {
                let Some(face) = switch_face(paint, tier) else {
                    break;
                };
                match lockup {
                    Some(cells) => push(Rung {
                        tier: Some(tier),
                        lockup: Some(Lockup::Full),
                        lead: 2,
                        need: cells + face.len + 6,
                    }),
                    // No mark: the switch alone, in its widest form that fits.
                    None => push(Rung {
                        tier: Some(tier),
                        lockup: None,
                        lead: 0,
                        need: face.len + 4,
                    }),
                }
                last = Some((tier, face.len));
            }
            if let (Some((tier, len)), Some(_)) = (last, lockup) {
                push(Rung {
                    tier: Some(tier),
                    lockup: Some(Lockup::Icon),
                    lead: 2,
                    need: icon + len + 6,
                });
                push(Rung {
                    tier: Some(tier),
                    lockup: Some(Lockup::Icon),
                    lead: 1,
                    need: icon + len + 4,
                });
                push(Rung {
                    tier: Some(tier),
                    lockup: None,
                    lead: 0,
                    need: len + 4,
                });
            }
        }
        None => {
            if let Some(cells) = lockup {
                push(Rung {
                    tier: None,
                    lockup: Some(Lockup::Full),
                    lead: 2,
                    need: cells + 4,
                });
                push(Rung {
                    tier: None,
                    lockup: Some(Lockup::Icon),
                    lead: 2,
                    need: icon + 4,
                });
                push(Rung {
                    tier: None,
                    lockup: Some(Lockup::Icon),
                    lead: 1,
                    need: icon + 2,
                });
            }
        }
    }
    (rungs, n)
}

/// The first of `rungs` a span of `span` cells holds.
fn first_fit(span: usize, rungs: &[Rung]) -> Option<Rung> {
    rungs.iter().copied().find(|r| r.need <= span)
}

/// THE LADDER LAID ([`ladder`]): the rung a span of `span` cells takes for
/// a line whose switch is `switch` and whose lockup takes `lockup` cells,
/// and the switch's face on it; `None` where no rung fits, and the line
/// stays the vendor's.
fn lay_top(
    span: usize,
    switch: Option<SwitchPaint>,
    lockup: Option<usize>,
) -> Option<(Rung, Option<SwitchFace>)> {
    let (rungs, n) = ladder(switch, lockup);
    let rung = first_fit(span, &rungs[..n])?;
    let face = rung
        .tier
        .zip(switch)
        .and_then(|(tier, paint)| switch_face(paint, tier));
    Some((rung, face))
}

/// What a composer top line is composed from ([`compose_top`]): where it
/// is and its kind (a rule, or Codex's slab, whose blank is then `blank`),
/// its run, the program whose mark the lockup wears and the build whose
/// name it prints ([`top_build`]), the harness switch it
/// shows (`None`: no host, no switch), the theme it wears (`None`: bare,
/// the harness's glass not worn), the pane's blank, the marks' ink, what
/// aterm draws for the top-effort key there, what the mark's sprite is laid
/// with — the window's cell in device pixels and the pane's box in grid
/// pixels, its clip — and the hover title's: whether the pointer is on the
/// lockup, and the footer's `shown_gen` while it is and the footer holds
/// the program's facts, whose version the title names (`0`: none).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TopKey {
    frame_row: usize,
    term_row: usize,
    line: TopLine,
    run: TopRun,
    program: aterm_phase::Program,
    build: Build,
    switch: Option<SwitchPaint>,
    palette: Option<aterm_effects::harness_card::CardPalette>,
    blank: RenderCell,
    mark_fg: [u8; 3],
    face: KeyFace,
    cell: (u16, u16),
    clip: aterm_core::render::PxRect,
    mark_hover: bool,
    title_gen: u64,
}

/// What aterm draws for the top-effort key on a top rule (design §10.6.14): none
/// of Claude's own ink — its tag is never restyled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct KeyFace {
    /// aterm's ` KEY off ` ghost in Claude's slot, the key off: `Some(lit)`,
    /// lit while the pointer is on it.
    ghost: Option<bool>,
    /// Claude's own tag on the row: its key's frame columns.
    tag: Option<(usize, usize)>,
    /// `◐` while a press of the key is in flight, `▸` while the keyboard
    /// selects it: in the `─` cell left of the ghost's or the tag's leading
    /// blank.
    mark: Option<char>,
}

/// The ghost's cells: ` KEY off `.
const GHOST_CELLS: usize = aterm_primer::CLAUDE_TOP_EFFORT_KEY.len() + 6;

/// The shortest run the ghost is drawn in: itself, the `─` after it as
/// Claude leaves one after its tag, and the rule's lead of `─` before it.
const GHOST_RUN: usize = GHOST_CELLS + 1 + footer::RULE_LEAD;

/// The ghost's glyphs, in order: the key spelled by its one const.
fn ghost_chars() -> impl Iterator<Item = char> {
    std::iter::once(' ')
        .chain(aterm_primer::CLAUDE_TOP_EFFORT_KEY.chars())
        .chain(" off ".chars())
}

/// A composer top rule as [`PaneFooter::compose_top`] composed it: the
/// cells to write at frame column `col` (none: the rule stays the vendor's),
/// the run of them the write leaves on the glass (`glass`, [`rule_edits`]),
/// the harness switch as laid in them, the ghost's glyphs' frame columns
/// where it is drawn, the lockup's frame columns (pads included; empty: none
/// laid) and its icon's first, and the mark's sprite over the icon's cells —
/// one layer per pane, kept while the key holds, so a frame at rest hands
/// the renderer the same `Arc` ([`mark_layer`]).
#[derive(Debug, Default)]
struct ComposedTop {
    key: Option<TopKey>,
    cells: Vec<RenderCell>,
    col: usize,
    /// Indices into `cells`: on a bare line, the vendor's own cells between
    /// the lockup and the first cell aterm draws after it, which the write
    /// leaves out ([`compose_top`]); empty where there is none.
    glass: std::ops::Range<usize>,
    switch: Option<LaidSwitch>,
    ghost: Option<(usize, usize)>,
    lockup: std::ops::Range<usize>,
    icon: Option<usize>,
    mark: Option<aterm_core::render::FxLayer>,
}

/// Where [`compose_top`] laid a harness switch, in frame columns: its form
/// ([`switch_face`]'s tier), every cell a press acts on, the switch's own
/// cells and an off switch's `on` button — what the splice records as the
/// `crate::claude_lights::BorderSwitch` once the write takes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LaidSwitch {
    tier: usize,
    cols: std::ops::Range<usize>,
    switch: std::ops::Range<usize>,
    button: Option<std::ops::Range<usize>>,
}

/// The context fact's words for an indicator reading `pct` per cent left
/// (`aterm_phase::phase::context_left_shown`): `ctx 84% left`.
fn context_words(pct: u8) -> String {
    format!("ctx {pct}% left")
}

/// The reading the context fact's words `shown` say ([`context_words`]'s
/// inverse), or `None` for none or any other words: compared with the
/// screen's reading each frame without allocating.
fn context_left_of(shown: Option<&str>) -> Option<u8> {
    shown?
        .strip_prefix("ctx ")?
        .strip_suffix("% left")?
        .parse()
        .ok()
}

/// A `0x00RRGGBB` colour's channels.
fn rgb_of(c: u32) -> [u8; 3] {
    [(c >> 16) as u8, (c >> 8) as u8, c as u8]
}

/// THE CLI'S TOP LINE — Claude Code's, agy's and Muse Code's composer top
/// rule, Gemini CLI's live-zone rule, Codex's composer slab's top pad
/// ([`top_line`]) — composed from `key` into `top` (owner, 2026-10-21:
/// "each CLI has an emoji-sized icon and color on the left side of the top
/// line of CLI to standardize"):
///
/// * THE LOCKUP, two `─` in from the run's left: a pad, the CLI's drawn
///   icon over two blank cells (a sprite on the glass, [`mark_layer`] —
///   never text glyphs, never the program's grid), a pad, the CLI's name
///   bold in its brand ink floored to 4.5:1 on the ground
///   (`cli_marks::name_fg`), a trailing pad: `──[##]claude ────`. The name
///   is the build's the host proves (`key.build`, [`top_build`]): a Codex
///   the host admits reads `codex-alab`, the owner's name for it, and a
///   vendor `codex` reads `codex`. It is the program's identity, not the
///   harness's state, so it stands in every state — bare and with no host
///   included ([`cli_mark`]).
/// * THE SWITCH at the rule's right, two `─` from the span's end
///   ([`switch_face`]): ` alab harness ● on `, ` alab harness ○ off ▐ on ▌ `,
///   ` alab harness ◐ turning on `, laid where a press, a hover and a screen
///   reader reach it (`top.switch`, `crate::claude_lights::BorderSwitch`);
///   with no host there is none.
/// * THE NARROW LADDER ([`ladder`]) gives up the switch's words first, then
///   the name, then the icon; the switch's last form stands wherever it
///   stood before the lockup.
/// * THE TINT, where the harness's glass is worn (`key.palette`, the
///   session's theme, [`crate::autopilot::harness_theme`]: the card's own —
///   Cruise's spectrum, Land's sunset, Off's white moonbow, the owner's of
///   2026-10-19): the switch's glyphs walk the arc from `t = 0` to `t = 1`
///   in the palette's legible letter ink ([`switch_cell`]), and the gap's
///   `─` walk it BACKWARDS, `t = 1` at the lockup down to `t = 0` beside the
///   switch's first glyph, so the rule flows into the switch on one stop; the
///   `─` before the lockup and after the switch stand at `t = 1`. One stroke,
///   no seam, and the lockup framed by the stop farthest from red. Bare (the
///   master switch or the agent's own off, a claim this host does not hold,
///   no host), the vendor's rule keeps its ink under the lockup and the
///   switch.
///
/// THE TOP-EFFORT KEY's ghost (`key.face`, design §10.6.14) takes Claude's slot
/// at the run's right end, the `─` after it left as Claude's: ` KEY off ` in
/// Claude's rule gray, not bold, at full size — lifted and underlined while
/// the pointer is on it — the tint, the switch and the lockup laid in the
/// span short of it. Its mark (`◐`, `▸`) takes the `─` left of its leading
/// blank, or of Claude's tag's: the span's last cell, which no rung takes.
/// Where no rung fits, only the ghost and its mark are written.
///
/// Every cell written lies in the run it is laid on ([`laid_run`]): a plain
/// `─` on the pane's ground, or a blank of Codex's proven slab
/// ([`TopLine::Slab`], whose blank stands in for the pane's in every ink and
/// contrast law, and which is never tinted: it is Codex's own surface) — and
/// Muse Code's idle voice label where the lockup takes it, so the lockup
/// stands at the rule's left (owner, 2026-10-21), the label written over
/// with the rule on the glass alone. Claude's labels and tag, Muse's voice
/// states with news, Gemini's band, Codex's prompt row and every other cell
/// the CLI inks itself are outside it, and are never written.
///
/// THE HOVER TITLE (design §7), while the pointer rests on the lockup
/// (`key.mark_hover`): the name gives way to `<product> <version>`
/// (`Program::product`, the footer's version, `version`), else the product
/// alone where no version is held (Gemini, Codex) or the version would leave
/// fewer than two `─` before the switch (or the span's end), else the name
/// — and the name wherever the title would be narrower than it (codex-alab's
/// `Codex`), which never reads less than the resting lockup.
/// The rung is the resting lockup's and the tint the resting layout's, so
/// the title covers gap cells alone and the switch never moves.
fn compose_top(key: &TopKey, top: &mut ComposedTop, version: Option<&str>) {
    compose_top_in(key, top, version, TITLE_LEAD);
}

/// The `─` the hover title leaves before the switch (or the span's end),
/// as the lockup's resting rungs leave: two.
const TITLE_LEAD: usize = 2;

/// [`compose_top`] with the hover title leaving `title_lead` `─` before the
/// switch: the tests' control lets it into the switch's lead.
fn compose_top_in(key: &TopKey, top: &mut ComposedTop, version: Option<&str>, title_lead: usize) {
    #[cfg(test)]
    crate::work_counts::footer_top_composed();
    top.cells.clear();
    top.glass = 0..0;
    top.switch = None;
    top.ghost = None;
    top.lockup = 0..0;
    top.icon = None;
    let TopKey {
        line,
        program,
        build,
        switch,
        palette,
        blank,
        mark_fg,
        face,
        ..
    } = *key;
    // Muse Code's idle label, where the lockup takes it (`label` cells).
    let (run, label) = laid_run(key);
    top.col = run.col;
    let rule_cell = run.rule;
    // Codex's slab is its own surface: the switch walks the theme's arc on
    // it, its blanks are never tinted.
    let tint = palette.filter(|_| line == TopLine::Rule);
    // The ghost's first cell, in the run: right-aligned, one `─` after it.
    let ghost_at = ghost_at(run.len, face);
    // The mark's cell, in the run: left of the ghost's leading blank, or of
    // Claude's tag's.
    let mark_at = face.mark.and_then(|_| match ghost_at {
        Some(at) => at.checked_sub(1),
        None => face
            .tag
            .and_then(|(start, _)| start.checked_sub(2)?.checked_sub(run.col))
            .filter(|&at| at < run.len),
    });
    let mark_cell = face.mark.map(|ch| RenderCell {
        ch,
        fg: mark_fg,
        bold: false,
        ..rule_cell
    });
    let ghost_cells = |lit: bool| {
        let fg = if lit {
            value_fg(rule_cell.fg, blank.fg)
        } else {
            rule_cell.fg
        };
        ghost_chars().map(move |ch| RenderCell {
            ch,
            fg,
            bold: false,
            script: aterm_core::terminal::Script::Normal,
            underline: if lit && ch != ' ' {
                aterm_core::terminal::UnderlineStyle::Single
            } else {
                aterm_core::terminal::UnderlineStyle::None
            },
            ..rule_cell
        })
    };
    if let Some(at) = ghost_at {
        top.ghost = Some((run.col + at + 1, run.col + at + GHOST_CELLS - 1));
    }
    // The harness's span: the run, short of the ghost.
    let span_len = top_span(run.len, face);
    let mark = cli_mark(program);
    let Some((rung, sface)) = lay_top(span_len, switch, mark.map(|m| m.lockup_cells(build))) else {
        // No rung fits: the rule stays the vendor's but for the key's ghost
        // and its mark.
        let first = mark_at.or(ghost_at);
        if let Some(first) = first {
            top.col = run.col + first;
            if let (Some(_), Some(mark)) = (mark_at, mark_cell) {
                top.cells.push(mark);
            }
            if let (Some(_), Some(lit)) = (ghost_at, face.ghost) {
                top.cells.extend(ghost_cells(lit));
            }
        }
        return;
    };
    // Where the lockup and the switch lie in the span.
    let lockup_at = rung.lockup.zip(mark).map(|(lockup, mark)| {
        let cells = match lockup {
            Lockup::Full => mark.lockup_cells(build),
            Lockup::Icon => aterm_effects::cli_marks::ICON_RUN,
        };
        rung.lead..rung.lead + cells
    });
    let switch_at = sface.as_ref().map(|f| {
        let start = span_len - 2 - f.len;
        start..start + f.len
    });
    if let (Some(f), Some(at), Some(tier)) = (&sface, &switch_at, rung.tier) {
        let col_start = run.col + at.start;
        let laid = |r: &std::ops::Range<usize>| col_start + r.start..col_start + r.end;
        top.switch = Some(LaidSwitch {
            tier,
            cols: col_start..col_start + f.len,
            switch: laid(&f.switch),
            button: f.button.as_ref().map(laid),
        });
    }
    // The cells written: with a tint the whole span, its `─` tinted; bare
    // (and on Codex's slab), from the first cell aterm draws to the last (to
    // the ghost, where it stands), the vendor's line between the lockup and
    // what follows it left on the glass (`top.glass`, below).
    let (lo, hi) = if tint.is_some() {
        (0, span_len)
    } else {
        let firsts = [
            lockup_at.as_ref().map(|r| r.start),
            switch_at.as_ref().map(|r| r.start),
            mark_at,
        ];
        let lasts = [
            lockup_at.as_ref().map(|r| r.end),
            switch_at.as_ref().map(|r| r.end),
            mark_at.map(|m| m + 1),
        ];
        let lo = firsts.into_iter().flatten().min().unwrap_or(0);
        let hi = match ghost_at {
            Some(_) => span_len,
            None => lasts.into_iter().flatten().max().unwrap_or(lo),
        };
        // A label the lockup took is written over to its last cell.
        (lo, hi.max(label))
    };
    top.col = run.col + lo;
    let cells = &mut top.cells;
    cells.resize(hi - lo, rule_cell);
    if let Some(palette) = tint {
        let dark = crate::claude_card::dark_ground(blank);
        // The gap: between the lockup's last cell (the span's first, with
        // none) and the switch's first, walked from `t = 1` down to `t = 0`.
        let gap = switch_at
            .as_ref()
            .map(|s| lockup_at.as_ref().map_or(0, |l| l.end)..s.start);
        for i in 0..span_len {
            let mine =
                |r: &Option<std::ops::Range<usize>>| r.as_ref().is_some_and(|r| r.contains(&i));
            if mine(&lockup_at) || mine(&switch_at) {
                continue;
            }
            let t = match &gap {
                Some(gap) if gap.contains(&i) => {
                    let (j, g) = (i - gap.start, gap.len());
                    if g <= 1 {
                        0.0
                    } else {
                        1.0 - j as f32 / (g - 1) as f32
                    }
                }
                _ => 1.0,
            };
            let lifted = mix(rgb_of(palette.art(t, dark)), blank.fg, 0.25);
            let mut fg = mix(rule_cell.fg, lifted, 0.68);
            if fg == rule_cell.fg {
                fg[0] ^= 1;
            }
            cells[i - lo] = RenderCell {
                ch: '\u{2500}',
                fg,
                ..rule_cell
            };
        }
    }
    if let (Some(at), Some(mark)) = (&lockup_at, mark) {
        // The pads and the icon's two cells: blanks on the ground, the
        // sprite drawn over the icon's.
        let pad = RenderCell { ch: ' ', ..blank };
        for c in at.clone() {
            cells[c - lo] = pad;
        }
        top.lockup = run.col + at.start..run.col + at.end;
        top.icon = Some(run.col + at.start + 1);
        if rung.lockup == Some(Lockup::Full) {
            let ground = crate::claude_card::pack(blank.bg);
            let name_at = at.start + aterm_effects::cli_marks::ICON_RUN;
            let word = mark.name(build);
            let word_len = word.chars().count();
            // The hover title, where it fits: its trailing pad and the
            // title's lead of `─` before the switch (or the span's end),
            // over gap cells alone.
            let limit = switch_at.as_ref().map_or(span_len, |s| s.start);
            let room = limit.saturating_sub(name_at + 1 + title_lead);
            let title = key
                .mark_hover
                .then(|| hover_title(program.product(), version, room))
                .flatten()
                .filter(|t| t.len >= word_len);
            match title {
                Some(t) => {
                    // A bare line writes to its last drawn cell: the title's
                    // pad may lie past it, over the vendor's rule.
                    let end = name_at + t.len;
                    if cells.len() <= end - lo {
                        cells.resize(end + 1 - lo, rule_cell);
                    }
                    // The name's inks spread over the title, so Gemini's
                    // gradient runs its whole length.
                    let letter = |k: usize| {
                        if t.len <= 1 {
                            0
                        } else {
                            (k * (word_len - 1) + (t.len - 1) / 2) / (t.len - 1)
                        }
                    };
                    for (k, ch) in t.chars().enumerate() {
                        cells[name_at + k - lo] = if ch == ' ' {
                            pad
                        } else {
                            RenderCell {
                                ch,
                                fg: rgb_of(aterm_effects::cli_marks::name_fg(
                                    mark,
                                    letter(k),
                                    ground,
                                )),
                                bold: true,
                                ..blank
                            }
                        };
                    }
                    cells[end - lo] = pad;
                    top.lockup.end = run.col + end + 1;
                }
                None => {
                    for (k, ch) in word.chars().enumerate() {
                        cells[name_at + k - lo] = RenderCell {
                            ch,
                            fg: rgb_of(aterm_effects::cli_marks::name_fg(mark, k, ground)),
                            bold: true,
                            ..blank
                        };
                    }
                }
            }
        }
    }
    if let (Some(mut f), Some(at), Some(paint)) = (sface, &switch_at, switch) {
        if palette.is_some() {
            f.on_arc();
        }
        for (k, &(ch, ink)) in f.cells().iter().enumerate() {
            cells[at.start + k - lo] = switch_cell(ch, ink, paint, palette, blank, mark_fg);
        }
    }
    if let (Some(at), Some(mark)) = (mark_at, mark_cell) {
        cells[at - lo] = mark;
    }
    if let (Some(_), Some(lit)) = (ghost_at, face.ghost) {
        cells.extend(ghost_cells(lit));
    }
    // Bare, the line between the lockup (its title and a label it took
    // included) and the first cell drawn after it is the vendor's, every
    // cell of it the run's own (`read_top_run` proves the run uniform): the
    // write leaves it on the glass. Written, Codex's slab blanks there were
    // cells aterm wrote, and the resident pet held the pad from the lockup
    // to the switch as the host's chrome (`crate::pet_chrome`): no rest on
    // codex-alab's prompt line under it.
    if tint.is_none() && !top.lockup.is_empty() {
        let from = (top.lockup.end - run.col).max(label);
        let next = [
            switch_at.as_ref().map(|r| r.start),
            mark_at,
            ghost_at.map(|_| span_len),
        ]
        .into_iter()
        .flatten()
        .min();
        if let Some(next) = next.filter(|&next| from < next && next <= hi) {
            top.glass = from - lo..next - lo;
        }
    }
}

/// THE HOVER TITLE's words ([`hover_title`]): the product, its version
/// where one is named, and their cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HoverTitle<'a> {
    product: &'a str,
    version: Option<&'a str>,
    len: usize,
}

impl HoverTitle<'_> {
    /// The title's glyphs: `Claude Code 2.1.304`.
    fn chars(&self) -> impl Iterator<Item = char> + '_ {
        self.product.chars().chain(
            self.version
                .into_iter()
                .flat_map(|v| std::iter::once(' ').chain(v.chars())),
        )
    }
}

/// The widest hover title (design §7) whose words fit `room` cells:
/// `<product> <version>`, else the product alone (where no `version` is
/// held, or the version leaves no room); `None` where neither fits, and the
/// name stays. Nothing allocated.
fn hover_title<'a>(
    product: &'a str,
    version: Option<&'a str>,
    room: usize,
) -> Option<HoverTitle<'a>> {
    let cells = product.chars().count();
    version
        .map(|v| HoverTitle {
            product,
            version: Some(v),
            len: cells + 1 + v.chars().count(),
        })
        .filter(|t| t.len <= room)
        .or_else(|| {
            (cells <= room).then_some(HoverTitle {
                product,
                version: None,
                len: cells,
            })
        })
}

/// The contents of the marks' sprites ([`aterm_effects::cli_marks::content`])
/// the window's panes were last laid with, by mark, fit and ground: a pane
/// reopening on a ground it was laid on seals nothing. At most
/// [`MarkContents::KEPT`]: two grounds for each of the marks.
#[derive(Debug, Default)]
struct MarkContents(Vec<MarkContent>);

/// One kept sprite content: its mark, fit and ground, and the content.
type MarkContent = (
    CliMark,
    aterm_effects::cli_marks::Fit,
    u32,
    Arc<aterm_core::render::FxContent>,
);

impl MarkContents {
    /// How many contents are kept.
    const KEPT: usize = 2 * CliMark::ALL.len();

    /// `mark`'s content at `fit` on `ground`: the kept one, or one sealed
    /// now and kept, the oldest let go past [`Self::KEPT`].
    fn content(
        &mut self,
        mark: CliMark,
        fit: aterm_effects::cli_marks::Fit,
        ground: u32,
    ) -> Arc<aterm_core::render::FxContent> {
        if let Some((.., kept)) = self
            .0
            .iter()
            .find(|(m, f, g, _)| (*m, *f, *g) == (mark, fit, ground))
        {
            return Arc::clone(kept);
        }
        let content = aterm_effects::cli_marks::content(mark, fit, ground);
        if self.0.len() >= Self::KEPT {
            self.0.remove(0);
        }
        self.0.push((mark, fit, ground, Arc::clone(&content)));
        content
    }
}

/// Pane `pane`'s box in grid pixels for a window cell of `cell` device
/// pixels: a mark sprite's clip, so it never draws past its pane. Saturates
/// past `i32`.
fn pane_grid_px(
    cell: (u16, u16),
    pane: crate::claude_card::PaneGeom,
) -> aterm_core::render::PxRect {
    let px = |cells: usize, side: u16| {
        i32::try_from(cells)
            .unwrap_or(i32::MAX)
            .saturating_mul(i32::from(side))
    };
    let top = pane.strip + pane.row_off;
    aterm_core::render::PxRect::new(
        px(pane.col_off, cell.0),
        px(top, cell.1),
        px(pane.col_off + pane.cols, cell.0),
        px(top + pane.rows, cell.1),
    )
}

/// The mark's layer id for `session`'s pane: a hash of `("top-mark",
/// session)`, stable for the session, so the renderer matches the layer
/// frame to frame and two panes' marks never share one.
pub(crate) fn top_mark_id(session: u64) -> u64 {
    // splitmix64 over the session salted with `top-mark`.
    let mut z = session ^ 0x746F_702D_6D61_726B;
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// THE MARK'S SPRITE for a top rule composed from `key` into `top`
/// (design §6.4): where the lockup laid its icon, the program's mark at the
/// fit law's master and scale for the window's cell
/// (`cli_marks::fit`), over its inks floored on the pane's ground, laid at
/// the icon's first cell in grid pixels and clipped to the pane, over the
/// glyphs (the card's kitty's slot) by the byte-exact law. Its content is
/// the kept one for the mark, fit and ground (`marks`), so a key that moved
/// the line alone (a resize) moves the layer and seals nothing. `None` where
/// no icon is laid or no master fits the cell (the lockup keeps its blank
/// cells).
fn mark_layer(
    key: &TopKey,
    top: &ComposedTop,
    session: u64,
    marks: &mut MarkContents,
) -> Option<aterm_core::render::FxLayer> {
    let col = top.icon?;
    let mark = cli_mark(key.program)?;
    let (cw, ch) = key.cell;
    let fit = aterm_effects::cli_marks::fit(mark, cw, ch)?;
    let content = marks.content(mark, fit, crate::claude_card::pack(key.blank.bg));
    let px = |cells: usize, side: u16| {
        i32::try_from(cells)
            .ok()
            .and_then(|c| c.checked_mul(i32::from(side)))
    };
    Some(aterm_effects::cli_marks::layer(
        top_mark_id(session),
        content,
        (px(col, cw)?, px(key.frame_row, ch)?),
        key.clip,
    ))
}

/// The words switch `b` paints over its switch and over its `on` button,
/// glyphs aside (`alab harness off`, `on`): what its accessibility nodes'
/// names begin with (WCAG 2.5.3, `native_ui::visible_label_in_name`).
/// Rebuilt from what the paint recorded, only where a screen reader asks.
#[cfg(a11y_tree)]
pub(crate) fn switch_words(b: &crate::claude_lights::BorderSwitch) -> (String, Option<String>) {
    let paint = SwitchPaint {
        face: b.face,
        hover: false,
    };
    let Some(face) = switch_face(paint, b.tier) else {
        return (String::new(), None);
    };
    let words = |range: &std::ops::Range<usize>| -> String {
        let text: String = face.cells[range.clone()]
            .iter()
            .map(|(ch, _)| *ch)
            .collect();
        text.split_whitespace()
            .filter(|w| w.chars().any(char::is_alphanumeric))
            .collect::<Vec<_>>()
            .join(" ")
    };
    (words(&face.switch), face.button.as_ref().map(words))
}

/// Whether `row` is an agy tool-call header (`● Ran (…)`, `● Explored …`,
/// `● Edited …`, `● Read(…)`, etc.) rather than an assistant prose reply.
fn is_agy_tool_call_row(row: &str) -> bool {
    let Some(rest) = row.strip_prefix("● ") else {
        return false;
    };
    if [
        "Ran ",
        "Explored ",
        "Edited ",
        "Created ",
        "Deleted ",
        "Searched ",
        "Viewed ",
        "Listed ",
    ]
    .iter()
    .any(|verb| rest.starts_with(verb))
    {
        return true;
    }
    let Some((name, _)) = rest.split_once('(') else {
        return false;
    };
    !name.is_empty()
        && name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && rest.trim_end().ends_with(')')
}

/// Render inline markdown (`***bold italic***`, `**bold**`, `*italic*`,
/// `` `code` ``, `[label](url)`) into `out` with clear visual distinction
/// (inline code pill background and tinted foreground, underlined links,
/// recursive inline spans) without changing row height.
fn push_agy_inline_markdown(
    text: &str,
    base: RenderCell,
    code_fg: [u8; 3],
    code_bg: [u8; 3],
    link_fg: [u8; 3],
    max_cols: usize,
    out: &mut Vec<RenderCell>,
) -> bool {
    let mut changed = false;
    let mut rest = text;
    while !rest.is_empty() && out.len() < max_cols {
        if let Some(after_open) = rest.strip_prefix("***")
            && !after_open.starts_with(' ')
            && let Some((inner, after_close)) = after_open.split_once("***")
            && !inner.is_empty()
            && !inner.ends_with(' ')
        {
            changed = true;
            let bold_italic_cell = RenderCell {
                bold: true,
                italic: true,
                ..base
            };
            push_agy_inline_markdown(
                inner,
                bold_italic_cell,
                code_fg,
                code_bg,
                link_fg,
                max_cols,
                out,
            );
            rest = after_close;
            continue;
        }
        if let Some(after_open) = rest.strip_prefix("**")
            && !after_open.starts_with('*')
            && let Some((inner, after_close)) = after_open.split_once("**")
            && !inner.is_empty()
        {
            changed = true;
            let bold_cell = RenderCell { bold: true, ..base };
            push_agy_inline_markdown(inner, bold_cell, code_fg, code_bg, link_fg, max_cols, out);
            rest = after_close;
            continue;
        }
        if let Some(after_open) = rest.strip_prefix('`')
            && let Some((inner, after_close)) = after_open.split_once('`')
            && !inner.is_empty()
        {
            changed = true;
            let code_cell = RenderCell {
                fg: code_fg,
                bg: code_bg,
                underline: aterm_core::terminal::UnderlineStyle::None,
                ..base
            };
            for ch in inner.chars() {
                if out.len() >= max_cols {
                    break;
                }
                out.push(RenderCell { ch, ..code_cell });
            }
            rest = after_close;
            continue;
        }
        if let Some(after_open) = rest.strip_prefix('[')
            && let Some((label, after_label)) = after_open.split_once("](")
            && !label.is_empty()
            && !label.contains(['[', ']'])
            && let Some((url, after_url)) = after_label.split_once(')')
            && !url.is_empty()
            && !url.contains(' ')
        {
            changed = true;
            let link_cell = RenderCell {
                fg: link_fg,
                underline: aterm_core::terminal::UnderlineStyle::Single,
                ..base
            };
            push_agy_inline_markdown(label, link_cell, code_fg, code_bg, link_fg, max_cols, out);
            rest = after_url;
            continue;
        }
        if let Some(after_open) = rest.strip_prefix('*')
            && !after_open.starts_with(['*', ' '])
            && let Some((inner, after_close)) = after_open.split_once('*')
            && !inner.is_empty()
            && !inner.ends_with(' ')
        {
            changed = true;
            let italic_cell = RenderCell {
                italic: true,
                ..base
            };
            push_agy_inline_markdown(inner, italic_cell, code_fg, code_bg, link_fg, max_cols, out);
            rest = after_close;
            continue;
        }
        let mut chars = rest.chars();
        if let Some(ch) = chars.next() {
            out.push(RenderCell { ch, ..base });
            rest = chars.as_str();
        }
    }
    changed
}

/// Format one non-fenced row of an assistant reply (`● …`, `◆ …`, or 2-space
/// continuation) with clear visual distinction across H1/H2/H3 headings,
/// inline code pills, markdown links, nested bullets, checkboxes, ordered
/// lists, horizontal rules, and quotes while keeping 1-to-1 row height so the
/// screen never blows up.
#[allow(clippy::too_many_arguments)]
fn render_agy_markdown_row(
    row: &str,
    max_cols: usize,
    orig_width: usize,
    blank: RenderCell,
    rule: RenderCell,
    mark_fg: [u8; 3],
) -> Option<Vec<RenderCell>> {
    let trimmed_end = row.trim_end_matches(' ');
    if trimmed_end.is_empty() {
        return None;
    }
    let code_fg = mix(mark_fg, blank.fg, 0.55);
    let code_bg = mix(blank.bg, rule.fg, 0.16);
    let link_fg = mix(mark_fg, blank.fg, 0.42);
    let quiet_fg = mix(rule.fg, blank.fg, VALUE_LIFT);
    let (lead, body, lead_glyph) = if let Some(rest) = trimmed_end.strip_prefix("● ") {
        ("● ", rest, Some('●'))
    } else if let Some(rest) = trimmed_end.strip_prefix("\u{25c6} ") {
        ("\u{25c6} ", rest, Some('\u{25c6}'))
    } else {
        let spaces = trimmed_end.bytes().take_while(|&b| b == b' ').count();
        (&trimmed_end[..spaces], &trimmed_end[spaces..], None)
    };
    let mut out = Vec::with_capacity(orig_width);
    let mut changed = false;
    if let Some(glyph) = lead_glyph {
        out.push(RenderCell {
            ch: glyph,
            fg: mark_fg,
            bold: true,
            ..blank
        });
        out.push(blank);
    } else {
        for _ in 0..lead.len().min(max_cols) {
            out.push(blank);
        }
    }
    let heading_level = body.bytes().take_while(|&b| b == b'#').count();
    if (1..=4).contains(&heading_level)
        && let Some(heading_text) = body[heading_level..].strip_prefix(' ')
    {
        changed = true;
        let heading_base = match heading_level {
            1 => RenderCell {
                fg: mix(mark_fg, blank.fg, 0.28),
                bold: true,
                underline: aterm_core::terminal::UnderlineStyle::Single,
                ..blank
            },
            2 => RenderCell {
                fg: mix(mark_fg, blank.fg, 0.52),
                bold: true,
                ..blank
            },
            _ => RenderCell {
                fg: mix(rule.fg, blank.fg, 0.78),
                bold: true,
                italic: true,
                ..blank
            },
        };
        push_agy_inline_markdown(
            heading_text,
            heading_base,
            code_fg,
            code_bg,
            link_fg,
            max_cols,
            &mut out,
        );
    } else if body.len() >= 3
        && (body.bytes().all(|b| b == b'-') || body.bytes().all(|b| b == b'*'))
    {
        changed = true;
        let dash_cell = RenderCell {
            ch: '\u{2500}',
            fg: rule.fg,
            ..blank
        };
        let target = orig_width.max(12).min(max_cols);
        while out.len() < target {
            out.push(dash_cell);
        }
    } else if let Some(list_item) = body.strip_prefix("- ").or_else(|| body.strip_prefix("* ")) {
        changed = true;
        let (marker, marker_fg, marker_bold, item_rest) =
            if let Some(unchecked) = list_item.strip_prefix("[ ] ") {
                ('\u{2610}', quiet_fg, false, unchecked)
            } else if let Some(checked) = list_item
                .strip_prefix("[x] ")
                .or_else(|| list_item.strip_prefix("[X] "))
            {
                ('\u{2713}', mark_fg, true, checked)
            } else if lead_glyph.is_none() && lead.len() >= 4 {
                ('\u{25e6}', quiet_fg, false, list_item)
            } else {
                ('\u{2022}', mark_fg, true, list_item)
            };
        if out.len() < max_cols {
            out.push(RenderCell {
                ch: marker,
                fg: marker_fg,
                bold: marker_bold,
                ..blank
            });
        }
        if out.len() < max_cols {
            out.push(blank);
        }
        push_agy_inline_markdown(
            item_rest, blank, code_fg, code_bg, link_fg, max_cols, &mut out,
        );
    } else if let digits = body.bytes().take_while(|b| b.is_ascii_digit()).count()
        && digits > 0
        && digits <= 3
        && let Some(item_rest) = body[digits..].strip_prefix(". ")
    {
        changed = true;
        for ch in body[..digits + 1].chars() {
            if out.len() < max_cols {
                out.push(RenderCell {
                    ch,
                    fg: mark_fg,
                    bold: true,
                    ..blank
                });
            }
        }
        if out.len() < max_cols {
            out.push(blank);
        }
        push_agy_inline_markdown(
            item_rest, blank, code_fg, code_bg, link_fg, max_cols, &mut out,
        );
    } else if let Some(quote_item) = body.strip_prefix("> ") {
        changed = true;
        if out.len() < max_cols {
            out.push(RenderCell {
                ch: '▎',
                fg: rule.fg,
                ..blank
            });
        }
        if out.len() < max_cols {
            out.push(blank);
        }
        let quote_base = RenderCell {
            fg: quiet_fg,
            italic: true,
            ..blank
        };
        push_agy_inline_markdown(
            quote_item, quote_base, code_fg, code_bg, link_fg, max_cols, &mut out,
        );
    } else {
        changed |=
            push_agy_inline_markdown(body, blank, code_fg, code_bg, link_fg, max_cols, &mut out);
    }
    if !changed {
        return None;
    }
    while out.len() < orig_width.min(max_cols) {
        out.push(blank);
    }
    Some(out)
}

/// Whether Claude's mode pill is relabelled on this pane's glass
/// (`footer::YOLO_ON`, `footer::YOLO_OFF`): on a Claude Code pane whose
/// composer border aterm draws — `bordered`, the border's own predicate
/// (`crate::autopilot::harness_theme` is some: Off's white included), as for
/// [`compose_top`] and the agents panel. The ONE
/// gate: where the border's predicate moves, this moves with it.
fn relabels_pill(program: aterm_phase::Program, bordered: bool) -> bool {
    program == aterm_phase::Program::Claude && bordered
}

/// The repaint key's term for a pane's harness theme: none, or which palette.
fn theme_term(theme: Option<aterm_effects::harness_card::CardPalette>) -> u64 {
    theme.map_or(0, |p| 1 + p as u64)
}

/// One relabelled pill ([`collect_pill_relabel`]): its cells, a range of
/// the splice's shared buffer, written at `col` of `frame_row`, and the
/// glass→grid shift its write leaves.
struct RelabelWrite {
    frame_row: usize,
    col: usize,
    cells: std::ops::Range<usize>,
    ground: RenderCell,
    shift: GlassShift,
}

/// One window row whose glass columns are not its grid's: Claude's pill
/// relabelled, the run behind it packed left. In WINDOW columns of window
/// terminal row `term_row` ([`footer::Relabel::grid_col`]'s map, moved by
/// the pane's column).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct GlassShift {
    term_row: usize,
    /// The pane's column 0.
    col_off: usize,
    relabel: Option<footer::Relabel>,
}

impl GlassShift {
    fn grid_col(&self, row: usize, col: usize) -> Option<usize> {
        let relabel = self.relabel?;
        if row != self.term_row || col < self.col_off {
            return None;
        }
        Some(self.col_off + relabel.grid_col(col - self.col_off))
    }
}

/// The most rows of shifted columns a window keeps: one per pane, for as
/// many panes as share a window before the rest go unrelabelled.
const GLASS_SHIFTS: usize = 8;

/// Every row of a window's glass whose columns are not the grid's, as the
/// last footer write left them: fixed, nothing allocated. Empty when nothing
/// was relabelled, where every column is the grid's.
#[derive(Debug, Default)]
pub(crate) struct GlassColumns {
    len: usize,
    rows: [GlassShift; GLASS_SHIFTS],
}

impl GlassColumns {
    fn clear(&mut self) {
        self.len = 0;
    }

    /// Keep `shift`; a window full of them keeps the first [`GLASS_SHIFTS`].
    fn push(&mut self, shift: GlassShift) {
        if let Some(slot) = self.rows.get_mut(self.len) {
            *slot = shift;
            self.len += 1;
        }
    }

    /// The grid column under glass column `col` of window terminal row
    /// `row`: `col` itself but on a relabelled row's packed run.
    pub(crate) fn grid_col(&self, row: u16, col: u16) -> u16 {
        self.rows[..self.len]
            .iter()
            .find_map(|s| s.grid_col(usize::from(row), usize::from(col)))
            .and_then(|c| u16::try_from(c).ok())
            .unwrap_or(col)
    }
}

/// Collect the relabel of Claude's mode pill `relabel` under pane rows
/// `rows`' bottom rule, as the pane's readings keep it
/// (`footer::relabel_under`, read with the rule): `⏵⏵ bypass permissions
/// on` reads `YOLO on`, `⏸ manual mode on` `YOLO off`, in Claude's own
/// pill cells' style, and the row's left items after it packed left behind
/// the label (`  YOLO on · 14 shells · ← for agents …`), the blanks they left
/// at their run's end; a right-hand notice past the run's gap stays where
/// Claude drew it. The edit is the footer write's, undone with it, and its
/// cells go in `cells` (the splice's buffer: nothing allocated once grown).
///
/// Nothing is relabelled where the glass is not plainly the row's text: a
/// chrome row over it (`chrome_owned`, by window terminal row), any
/// selection in the window (its highlight is the grid's columns), the
/// cursor on the row, a BiDi row (its column map is the original cells'), a
/// cell of the span that is wide or asks a presentation, has a background
/// of its own, or is not the char the row's text holds there; nor where
/// the span holds a grapheme cluster, a combining mark or an image cell,
/// which [`write_edits`] would skip. Whether it collected the write: the
/// glass then reads `YOLO on`/`YOLO off`, and the mode light names the
/// state the same way (`FooterPane::relabelled`, review C8) only then.
#[allow(clippy::too_many_arguments)]
fn collect_pill_relabel(
    scratch: &RenderInput,
    rows: &[String],
    region: PaneTextRegion,
    relabel: footer::Relabel,
    chrome_owned: impl Fn(usize) -> bool,
    blank: RenderCell,
    out: &mut Vec<RelabelWrite>,
    cells: &mut Vec<RenderCell>,
) -> bool {
    let term_row = region.row_off + relabel.row;
    let frame_row = region.strip + term_row;
    if relabel.run_end > region.cols
        || chrome_owned(term_row)
        || scratch.any_selection()
        || (scratch.cursor_visible && scratch.cursor_row == frame_row)
        || !scratch.visual_to_logical_row(frame_row).is_empty()
    {
        return false;
    }
    let from = region.col_off + relabel.start;
    let written = from..region.col_off + relabel.run_end;
    if touches(&scratch.clusters, frame_row, &written)
        || touches(&scratch.combining, frame_row, &written)
        || touches(&scratch.images, frame_row, &written)
    {
        return false;
    }
    let Some(span) = scratch
        .cells
        .get(frame_row)
        .and_then(|row| row.get(written))
    else {
        return false;
    };
    // Plain: each cell the row's own char, narrow, asking no presentation;
    // and the PILL's own cells on the pane's ground. The packed run's cells
    // are moved with their own style, so Claude's highlight of an item —
    // `inverse` on the one hovered (its mouse tracking reports the motion
    // at Claude's column, `Relabel::grid_col`) or selected (`↓ to manage`)
    // — moves with its text, and the row never snaps back under the
    // pointer.
    let text = rows[relabel.row].chars().skip(relabel.start);
    let pill = relabel.end - relabel.start;
    let plain = span.iter().zip(text).enumerate().all(|(k, (c, ch))| {
        c.ch == ch
            && !c.wide
            && !c.emoji_presentation
            && !c.text_presentation
            && (k >= pill || c.bg == blank.bg)
    });
    if !plain || span.len() != relabel.run_end - relabel.start {
        return false;
    }
    let style = span[relabel.words - relabel.start];
    let at = cells.len();
    cells.extend(relabel.label.chars().map(|ch| RenderCell { ch, ..style }));
    cells.extend_from_slice(&span[relabel.end - relabel.start..]);
    cells.extend(std::iter::repeat_n(blank, relabel.shift()));
    out.push(RelabelWrite {
        frame_row,
        col: from,
        cells: at..cells.len(),
        ground: blank,
        shift: GlassShift {
            term_row,
            col_off: region.col_off,
            relabel: Some(relabel),
        },
    });
    true
}

/// Collect glass-only undoable markdown edits for an `agy` or `muse` pane. Row
/// text alone does not prove startup or tool-output provenance, so no transcript
/// row is deleted as cruft. Fenced code is left literal; a viewport clipped
/// inside a fence still keeps its literal startup-like lines. With any active
/// selection or a row carrying wide/presentation flags, keep the original
/// cells rather than rebuild them from plain text. A BiDi row also stays
/// intact: its column map describes the original cells, not a markdown edit.
#[allow(clippy::too_many_arguments)]
fn collect_agy_transcript_edits(
    scratch: &RenderInput,
    program: aterm_phase::Program,
    rows: &[String],
    region: PaneTextRegion,
    top_rule: Option<usize>,
    blank: RenderCell,
    rule: RenderCell,
    mark_fg: [u8; 3],
    out: &mut Vec<AgyRowEdit>,
) {
    if scratch.any_selection() {
        return;
    }
    let limit = top_rule
        .unwrap_or_else(|| rows.len().min(3))
        .min(rows.len())
        .min(region.rows);
    let is_muse = program == aterm_phase::Program::Muse;
    let card_range = if is_muse {
        let (_, muse_place) = aterm_phase::muse::launch_card_scan(&rows[..limit]);
        muse_place.map(|p| p.top..p.top + 2)
    } else {
        let (_, agy_place) = aterm_phase::agy::launch_card_scan(&rows[..limit]);
        agy_place.map(|p| p.top..p.top + 5)
    };
    let mut in_user_msg = false;
    let mut in_muse_reply = false;
    let mut in_code_block = false;

    for (r, row_str) in rows.iter().enumerate().take(limit) {
        // Track fences before any card, user-message or row heuristic. A
        // literal code sample can contain every one of those shapes.
        let body = row_str
            .strip_prefix("● ")
            .or_else(|| row_str.strip_prefix("\u{25c6} "))
            .unwrap_or(row_str)
            .trim_start();
        if body.starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }
        if in_code_block {
            continue;
        }
        if card_range.as_ref().is_some_and(|rng| rng.contains(&r)) {
            in_user_msg = false;
            in_muse_reply = false;
            in_code_block = false;
            continue;
        }
        let orig_width = row_str
            .trim_end_matches(' ')
            .chars()
            .count()
            .min(region.cols);
        if orig_width == 0 {
            in_user_msg = false;
            in_muse_reply = false;
            continue;
        }
        if is_muse {
            if row_str == "\u{276f}"
                || row_str.starts_with("\u{276f} ")
                || row_str == "  \u{276f}"
                || row_str.starts_with("  \u{276f} ")
            {
                in_user_msg = true;
                in_muse_reply = false;
                in_code_block = false;
                continue;
            }
        } else if r > 0 && (row_str == ">" || row_str.starts_with("> ")) {
            let prev = rows[r - 1].trim_end_matches(' ');
            if prev.chars().count() >= 10 && prev.chars().all(|c| c == '\u{2500}') {
                in_user_msg = true;
                in_code_block = false;
                continue;
            }
        }
        if in_user_msg {
            if row_str.starts_with("  ") {
                continue;
            }
            in_user_msg = false;
        }
        if is_muse {
            if row_str.starts_with("\u{25c6} ") {
                in_muse_reply = true;
            } else if in_muse_reply {
                let trimmed_lead = row_str.trim_start();
                if !row_str.starts_with("  ")
                    || trimmed_lead.starts_with([
                        '\u{256d}', '\u{2502}', '\u{2570}', '\u{25c7}', '\u{25c8}', '└', '⎿', '↑',
                        '\u{276f}', '/',
                    ])
                    || (row_str.starts_with("  ")
                        && !row_str.starts_with("   ")
                        && row_str.matches(" \u{00B7} ").count() >= 2)
                {
                    in_muse_reply = false;
                }
            }
        }
        let frame_row = region.strip + region.row_off + r;
        if scratch.cursor_visible && scratch.cursor_row == frame_row {
            continue;
        }
        let Some(frame_cells) = scratch.cells.get(frame_row) else {
            continue;
        };
        let pane_slice = frame_cells
            .get(region.col_off..frame_cells.len().min(region.col_off + region.cols))
            .unwrap_or(&[]);
        // The text cache retains only `ch`, not these rendition flags.
        if !scratch.visual_to_logical_row(frame_row).is_empty()
            || pane_slice
                .iter()
                .any(|c| c.bg != blank.bg || c.wide || c.emoji_presentation || c.text_presentation)
        {
            continue;
        }
        if top_rule.is_none() {
            continue;
        }
        let is_reply_row = if is_muse {
            in_muse_reply
        } else {
            (row_str.starts_with("  ") && !row_str.trim_start().starts_with(['└', '⎿', '↑', '◆']))
                || (row_str.starts_with("● ") && !is_agy_tool_call_row(row_str))
        };
        if !is_reply_row {
            in_code_block = false;
            continue;
        }
        if let Some(cells) =
            render_agy_markdown_row(row_str, region.cols, orig_width, blank, rule, mark_fg)
        {
            out.push(AgyRowEdit {
                frame_row,
                col: region.col_off,
                cells,
                ground: blank,
            });
        }
    }
}

/// `read` over pane `session`'s rows inside the reader's panic fence
/// (`crate::reader_guard`): a panic is warned once and read as no rule, so the vendor's
/// own rows are left as drawn (the seam the tests inject a panicking reader through).
fn read_footer(
    session: u64,
    rows: &[String],
    read: impl FnOnce(&[String]) -> FooterRead,
) -> FooterRead {
    crate::reader_guard::read_or_none(session, "Claude Code footer", rows, || read(rows)).flatten()
}

/// A row as it reads on the glass: each cell's character, except that a run of set facts
/// (`Script::Sub` glyphs, each nudged from its cell to its place, `footer::set_facts`)
/// reads in drawing order, with a space for each pitch of room the line leaves between two
/// glyphs: the text the line was set from, whatever cells its glyphs stand in.
#[cfg(test)]
pub(crate) fn read_glass(row: &[RenderCell]) -> String {
    use aterm_core::terminal::Script;
    let glyph = |c: &RenderCell| matches!(c.script, Script::Sub { .. }) && !c.wide && c.ch != ' ';
    let in_run =
        |c: &RenderCell| matches!(c.script, Script::Sub { .. }) || (c.ch == ' ' && !c.wide);
    let step = 1.0 / f32::from(Script::NUDGES_PER_CELL);
    let mut out = String::new();
    let mut c = 0;
    while c < row.len() {
        if !glyph(&row[c]) {
            out.push(row[c].ch);
            c += 1;
            continue;
        }
        // The run: through its last glyph (and that glyph's right half).
        let mut end = c;
        let mut k = c;
        while k < row.len() && in_run(&row[k]) {
            if glyph(&row[k]) || row[k].wide {
                end = k + 1;
            }
            k += 1;
        }
        let mut last: Option<(f32, usize)> = None;
        for (k, cell) in row.iter().enumerate().take(end).skip(c) {
            let Script::Sub { nudge } = cell.script else {
                continue;
            };
            if !glyph(cell) {
                continue;
            }
            let cols = if row.get(k + 1).is_some_and(|n| n.wide) {
                2
            } else {
                1
            };
            let at = k as f32 + cols as f32 / 2.0 + f32::from(nudge) * step;
            if let Some((before, before_cols)) = last {
                let room = (at - before) / footer::FACT_PITCH - (before_cols + cols) as f32 / 2.0;
                out.extend(std::iter::repeat_n(' ', room.round().max(0.0) as usize));
            }
            out.push(cell.ch);
            last = Some((at, cols));
        }
        c = end;
    }
    out
}

/// The footer's pass on one Claude Code pane, as a fixture the cost tests
/// assert and the `footer_pass_cost` bench measures.
#[cfg(any(test, feature = "bench-support"))]
#[path = "claude_footer_probe.rs"]
pub(crate) mod probe;

#[cfg(test)]
#[path = "claude_footer_cost_tests.rs"]
mod cost_tests;

#[cfg(test)]
#[path = "claude_footer_top_tests.rs"]
mod top_tests;

#[cfg(test)]
#[path = "claude_footer_top_lines_tests.rs"]
mod top_lines_tests;

#[cfg(all(test, unix))]
#[path = "claude_footer_yolo_tests.rs"]
mod yolo_tests;

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
#[path = "claude_footer_registry_tests.rs"]
mod registry_tests;

/// THE FOOTER READ FOR OTHER MODULES' TESTS (session log Phase 3B, addendum
/// A25): a Claude home on disk, and one read of a process in it through the
/// resolver's own path ([`resolve_into`], then the wake [`LogWake::after`]),
/// the process's exec read and the roster answered here, never the machine's.
#[cfg(test)]
pub(crate) mod rig {
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    use aterm_agent::harness::footer;

    use super::{Asks, Identity, LaunchFacts, LogWake, RosterMemo};
    use crate::session_log::transcript::Applied;
    use crate::session_timeline::SessionTimeline;

    /// The session id the registry row [`claude_home`] writes names.
    pub(crate) const SESSION_ID: &str = "00000000-0000-4000-8000-000000000a01";

    /// The person's home a test's read judges the resume line against: this
    /// test binary's own (`aterm_types::dirs::test_root`), never the person's
    /// `~/.claude`.
    pub(crate) fn scratch_home() -> Option<PathBuf> {
        aterm_types::dirs::test_root().map(|root| root.join("home"))
    }

    /// Where [`claude_home`] puts the home named `name`: a short process root, so
    /// its work folder's project slug stays under 255 bytes whatever `$TMPDIR` is.
    pub(crate) fn home_root(name: &str) -> PathBuf {
        crate::test_scratch::short_process_root("aterm-cf-").join(name)
    }

    /// A Claude home under a fresh root ([`home_root`]), with the registry row of
    /// `pid` naming the image `started_at`: `(root, home)`.
    pub(crate) fn claude_home(name: &str, pid: i32, started_at: u64) -> (PathBuf, PathBuf) {
        let root = home_root(name);
        let _ = std::fs::remove_dir_all(&root);
        let (cwd, claude) = (root.join("work"), root.join("claude"));
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(claude.join("sessions")).unwrap();
        std::fs::write(
            claude.join(format!("sessions/{pid}.json")),
            format!(
                r#"{{"pid":{pid},"sessionId":"{SESSION_ID}","cwd":{},"startedAt":{},"version":"2.1.287"}}"#,
                // A JSON string, as Claude Code writes it (a Windows path's
                // backslashes escaped).
                aterm_json::to_string(&cwd.display().to_string()).unwrap(),
                started_at * 1000 + 684
            ),
        )
        .unwrap();
        (root, claude)
    }

    /// The transcript of [`claude_home`]'s session under `root`.
    pub(crate) fn transcript_of(root: &Path, claude: &Path) -> PathBuf {
        let dir = claude
            .join("projects")
            .join(footer::project_slug(&root.join("work")));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(format!("{SESSION_ID}.jsonl"))
    }

    /// One footer read of process `pid` under `claude` into `timeline`,
    /// folded on in `fold` over `tail`, then the wake `wake` for `session`:
    /// what the read applied to the session's log.
    pub(crate) fn read(
        timeline: &Arc<Mutex<SessionTimeline>>,
        pid: i32,
        claude: &Path,
        fold: &mut footer::FooterCache,
        tail: &mut footer::TailCache,
        session: u64,
        wake: &LogWake,
    ) -> Applied {
        let mut identity = Identity {
            pid,
            started: None,
            dir: Some(claude.to_path_buf()),
            launch: LaunchFacts::default(),
            launch_for: None,
            login: None,
            argv: Vec::new(),
            task: None,
            login_checked_for: None,
            stored_login: false,
            env_stamp: None,
        };
        let asks = Asks {
            args: |_| {
                Some(atpkg::caller_shell::ProcArgs {
                    argv: vec!["claude".into()],
                    env: vec!["HOME=/home/someone".into()],
                    ..Default::default()
                })
            },
            roster: || None,
            home: scratch_home,
            stored_login: |_, _, _| true,
        };
        let (_, _, applied) = super::resolve_into(
            timeline,
            &mut identity,
            tail,
            fold,
            &mut RosterMemo::default(),
            asks,
        );
        wake.after(session, &applied);
        applied
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE SWITCH'S FORMS ([`switch_face`], owner 2026-10-20: "there needs
    /// to be an \"on\" button drawn for the claude harness"; 2026-10-21:
    /// "alab harness on"): one switch in every state, naming the harness
    /// alone — no program word, no wordmark — its words given up before the
    /// switch, every off form keeping `▐ on ▌`; each narrowest form is as
    /// wide as hb-int's (6, 8 and 6 cells), and the fitter takes the widest
    /// form that leaves two rule cells either side.
    /// NEGATIVE CONTROL: a room one cell short of a form takes the next.
    #[test]
    fn the_switch_faces_give_words_up_before_the_switch() {
        use crate::autopilot::{HarnessSwitch, SwitchOff};
        let paint = |face| SwitchPaint { face, hover: false };
        let faces = |p: SwitchPaint| -> Vec<SwitchFace> {
            (0..SWITCH_TIERS)
                .filter_map(|t| switch_face(p, t))
                .collect()
        };
        let texts =
            |p: SwitchPaint| -> Vec<String> { faces(p).iter().map(SwitchFace::text).collect() };
        assert_eq!(
            texts(paint(HarnessSwitch::On)),
            [
                " alab harness \u{25cf} on ",
                " alab \u{25cf} on ",
                " \u{25cf} on "
            ]
        );
        for off in [
            SwitchOff::Session,
            SwitchOff::Master,
            SwitchOff::Agent(aterm_phase::Program::Claude),
        ] {
            let p = paint(HarnessSwitch::Off(off));
            assert_eq!(
                texts(p),
                [
                    " alab harness \u{25cb} off \u{2590} on \u{258c} ",
                    " alab \u{25cb} off \u{2590} on \u{258c} ",
                    " \u{25cb} off \u{2590} on \u{258c} ",
                    " \u{2590} on \u{258c} ",
                ],
                "{off:?}"
            );
            for f in faces(p) {
                let button = f.button.clone().expect("every off form has its button");
                let drawn: String = f.cells()[button].iter().map(|c| c.0).collect();
                assert_eq!(drawn, "\u{2590} on \u{258c}");
            }
        }
        assert_eq!(
            texts(paint(HarnessSwitch::Turning)),
            [
                " alab harness \u{25d0} turning on ",
                " alab \u{25d0} turning on ",
                " \u{25d0} turning on ",
                " \u{25d0} on ",
            ]
        );
        for face in [
            HarnessSwitch::On,
            HarnessSwitch::Off(SwitchOff::Session),
            HarnessSwitch::Turning,
        ] {
            for f in faces(paint(face)) {
                assert!(f.len <= SWITCH_FACE_MAX, "{face:?}: {:?}", f.text());
            }
        }
        let off = paint(HarnessSwitch::Off(SwitchOff::Session));
        for (tier, face) in faces(off).into_iter().enumerate() {
            let room = face.len + 4;
            assert_eq!(
                fitted_switch_face(off, room).map(|(t, _)| t),
                Some(tier),
                "{tier} fits its room"
            );
            assert_eq!(
                fitted_switch_face(off, room - 1).map(|(t, _)| t),
                (tier + 1 < SWITCH_TIERS).then_some(tier + 1),
                "CONTROL: a cell short of {tier}"
            );
        }
    }

    /// THE SWITCH'S ARC READS ON PAPER: every colour a glyph of the harness
    /// switch can take on a line that wears a theme (`SwitchInk::Arc`,
    /// [`brand_letter_fg`]), in every theme the border wears (Cruise's
    /// spectrum, Land's sunset, Off's moonbow), reads at 4.5:1 or more
    /// against every light built-in's ground — a pure hue (one channel at
    /// zero) as the card's line letters are under Cruise and Land, a silver
    /// under Off; on a dark ground a letter is the palette's letter ink
    /// lifted a quarter toward the pane's ink. The settling switch's glyphs
    /// (`◐ turning on`, [`switch_cell`]'s `Turning` face, and the bare
    /// switch's quieted accent) read at 4.5:1 or more on every built-in
    /// ground, light or dark, each a quieted stop or the accent floored back
    /// ([`turning_ink`]). NEGATIVE CONTROLS: that lift of the spectrum on a
    /// light ground leaves a letter under 2:1, the washed-out switch; and the
    /// quieting alone, unfloored, leaves a settling letter under 3:1 on paper.
    #[test]
    fn the_switch_arc_reads_on_paper() {
        use aterm_effects::color_math::relative_luminance;
        use aterm_effects::harness_card::CardPalette;
        use aterm_types::scheme::{Appearance, builtin, builtin_names};
        let contrast = |a: [u8; 3], b: [u8; 3]| {
            let l = |c: [u8; 3]| relative_luminance(u32::from_be_bytes([0, c[0], c[1], c[2]]));
            let (la, lb) = (l(a), l(b));
            (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
        };
        let rgb = |c: aterm_types::Rgb| [c.r, c.g, c.b];
        let px = |c: u32| [(c >> 16) as u8, (c >> 8) as u8, c as u8];
        let (mut light, mut washed, mut quieted) = (0, f32::MAX, f32::MAX);
        let turning = SwitchPaint {
            face: crate::autopilot::HarnessSwitch::Turning,
            hover: false,
        };
        for name in builtin_names() {
            let Some(scheme) = builtin(name) else {
                continue;
            };
            let blank = RenderCell {
                fg: rgb(scheme.foreground),
                bg: rgb(scheme.background),
                ..RenderCell::default()
            };
            // An accent of the theme's own: its blue.
            let accent = rgb(scheme.ansi[4]);
            let bare = switch_cell('\u{25d0}', SwitchInk::Turning, turning, None, blank, accent);
            let k = contrast(bare.fg, blank.bg);
            assert!(k >= 4.5, "{name}: the bare settling switch at {k:.2}:1");
            for palette in [CardPalette::Cruise, CardPalette::Land, CardPalette::Off] {
                for stop in 0..255u8 {
                    let t = f32::from(stop) / 255.0;
                    let settling = switch_cell(
                        't',
                        SwitchInk::Arc(stop),
                        turning,
                        Some(palette),
                        blank,
                        accent,
                    );
                    let k = contrast(settling.fg, blank.bg);
                    assert!(
                        k >= 4.5 && !settling.bold,
                        "{name} {palette:?} settling at {t:.3}: {k:.2}:1"
                    );
                    if scheme.appearance == Appearance::Light {
                        let raw = mix(brand_letter_fg(palette, t, blank), blank.bg, 0.3);
                        quieted = quieted.min(contrast(raw, blank.bg));
                    }
                }
                for stop in 0..255 {
                    let t = stop as f32 / 254.0;
                    let fg = brand_letter_fg(palette, t, blank);
                    let what = format!("{name} {palette:?} at {t:.3}: {fg:?}");
                    if scheme.appearance == Appearance::Light {
                        light += 1;
                        let k = contrast(fg, blank.bg);
                        assert!(k >= 4.5, "{what} at {k:.2}:1");
                        if palette == CardPalette::Off {
                            let spread =
                                fg.iter().max().unwrap_or(&0) - fg.iter().min().unwrap_or(&0);
                            assert!(spread <= 32, "{what} is not a silver");
                        } else {
                            assert!(fg.contains(&0), "{what} is not a pure hue");
                        }
                        let c = aterm_effects::spectrum::SPECTRUM_LUT[stop];
                        washed = washed.min(contrast(mix(px(c), blank.fg, 0.25), blank.bg));
                    } else {
                        assert_eq!(
                            fg,
                            mix(px(palette.letter(t, true)), blank.fg, 0.25),
                            "{what}: dark, the palette's letter lifted"
                        );
                    }
                }
            }
        }
        assert!(light >= 3 * 7 * 255, "{light} light letters");
        assert!(washed < 2.0, "CONTROL: {washed:.2}");
        assert!(quieted < 3.0, "CONTROL: the quieting alone, {quieted:.2}");
    }

    /// THE FOOTER STANDS WHERE THE HOST SUPERVISES CLAUDE CODE OR AGY BY NAME:
    /// the predicate was the literal `claude`, so a `claude-code` argv0, which
    /// the host supervised as Claude Code, got no footer, no chips and no
    /// harness card. Every name the host reads as Claude Code or agy by name
    /// draws it, in any case; NEGATIVE CONTROLS: Codex, a runtime with no
    /// reader, a shell, a near-miss name and no name at all draw none.
    #[test]
    fn the_footer_is_drawn_for_every_name_the_host_supervises_as_claude_code() {
        use crate::session_timeline::AgentPublication;
        for name in [
            "claude",
            "claude-code",
            "Claude",
            "CLAUDE-CODE",
            "agy",
            "AGY",
            "muse",
            "MUSE",
            "muse-code",
            "codex",
            "node",
            "bun",
            "zsh",
            "sh",
            "claudette",
            "claude.exe",
            "",
        ] {
            let publication = AgentPublication {
                program: Some(name.to_string()),
                ..AgentPublication::default()
            };
            let host = crate::harness_host::agent_of(&publication).is_some_and(|p| match p {
                aterm_phase::Program::Claude
                | aterm_phase::Program::Agy
                | aterm_phase::Program::Muse => true,
                aterm_phase::Program::Codex
                | aterm_phase::Program::Gemini
                | aterm_phase::Program::Generic => false,
            });
            assert_eq!(
                drawn_for(Some(name)),
                host,
                "{name:?}: the footer and the host must agree"
            );
        }
        assert!(
            drawn_for(Some("claude-code")) && drawn_for(Some("agy")) && drawn_for(Some("muse")),
            "claude-code, agy, and muse argv0s are supervised, so they get the footer"
        );
        assert!(!drawn_for(Some("codex")) && !drawn_for(None));
    }

    /// THE CHIPS NEVER TOUCH OTHER INK (main's 35d8c4ca7, 2026-09-28, a
    /// private window on 2.1.284): drawn on Claude's mode row, a chip was
    /// glued to the `◐ medium · /effort` tail Claude right-aligns there in
    /// auto mode, and main kept one cell clear
    /// wherever the pane had slack. The chips are in the composer's bottom
    /// rule now, at its left end, and the rule's own layout pads them: at
    /// EVERY room the fit draws them in, with or without a title, beside the
    /// whole facts, a limit wall and Σ, or none, the cell before the chips
    /// and the cell after them are blank — never a rule glyph, a title's
    /// word or a fact — and the chips always fit (main's other half: the gap
    /// never costs the chips their place). The widths are the rows' own:
    /// Autopilot at rest (`∞`, 1 cell; `⤓ land`, 6), a light alone (`⏸
    /// plan`, 6) and beside Autopilot (`∞ ⏸ plan`, 8), and wider rows a
    /// Team chip or a longer pill makes (3, 10, 14), each with the titles a
    /// hover gives them. The painted row, on the measured screen:
    /// `claude_lights`'
    /// `the_effort_tail_stays_on_claudes_row_and_every_chip_stands_clear_in_the_rule`.
    #[test]
    fn the_chips_keep_a_blank_cell_on_each_side_at_every_width() {
        let blank = cell(' ', [200, 200, 200]);
        let rule = cell('\u{2500}', [90, 90, 90]);
        // Cruise's hovered title, its explanation from the one table: the
        // length the app draws.
        let cruise = format!(
            "autopilot: cruise \u{2014} {} \u{b7} click: land  ",
            crate::autopilot::mode_words(aterm_agent::supervise::Autopilot::Cruise).chip
        );
        for facts in [facts(), facts_with_usage(), FooterFacts::default()] {
            for chips in [1usize, 3, 6, 8, 10, 14] {
                for (title, short) in [
                    ("", ""),
                    ("Permission mode: plan  ", "plan  "),
                    (cruise.as_str(), "autopilot: cruise  "),
                ] {
                    let block = crate::claude_lights::Block::of_chips(
                        row_of(&"\u{25CB}".repeat(chips), [150, 150, 150]),
                        row_of(title, [150, 150, 150]),
                        row_of(short, [150, 150, 150]),
                        false,
                    );
                    let widths = footer::LightsWidth {
                        chips,
                        title: title.chars().count(),
                        lean: [0; footer::LEAN_FORMS],
                        short: short.chars().count(),
                        reason: false,
                    };
                    for room in 0usize..160 {
                        let fit = footer::fit_rule(&facts, room, Some(widths));
                        if !fit.chips {
                            assert!(
                                chips > footer::chips_room(&facts, room),
                                "{room}/{chips}: the chips fit, yet were not drawn"
                            );
                            continue;
                        }
                        let (cells, at) =
                            compose_rule(&fit, Some(&block), rule, blank, [0, 128, 255], room);
                        let at = at.expect("drawn chips have a column");
                        let row = text_of(&cells);
                        assert!(cells.len() <= room, "{room}: {row:?}");
                        assert_eq!(cells[at - 1].ch, ' ', "{room}/{chips} before: {row:?}");
                        assert_eq!(cells[at + chips].ch, ' ', "{room}/{chips} after: {row:?}");
                    }
                }
            }
        }
    }

    fn watch_model_projection(schedule: &WatchSchedule) -> i64 {
        match schedule.watched.get(&7).map(|watch| watch.job.pgid) {
            None => 0,
            Some(42) => 1,
            Some(43) => 2,
            other => panic!("unexpected foreground group: {other:?}"),
        }
    }

    #[test]
    fn stopped_footer_watch_has_no_followup_or_idle_reads() {
        // Tier-1 for ClaudeFooterWatch: the real scheduler's retained job and
        // idle-read guard agree with the derived model before and after stop.
        let model = aterm_models_harness::claude_footer_watch_model();
        let mut state = model.init_state();
        let now = std::time::Instant::now();
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        let weak = Arc::downgrade(&timeline);
        let mut schedule = WatchSchedule::default();
        schedule.watch(
            Job {
                session: 7,
                timeline,
                pgid: 42,
                wake: LogWake::default(),
            },
            now,
            false,
        );
        assert!(model.fire("AskOld", &mut state));
        assert_eq!(state["watch"], watch_model_projection(&schedule));
        assert_eq!(
            model.action_enabled("IdleRead", &state),
            schedule.due_idle(now + IDLE_RECHECK).is_some()
        );
        // Its row unread, the process is awaited: by the registry watch,
        // never a clock — the next read owed is the first follow-up.
        assert!(schedule.watched[&7].awaiting);
        assert_eq!(schedule.next(now), Some(now + FOLLOW_UPS[0]));
        let Accepted {
            latest: immediate,
            stopped,
            ..
        } = schedule.accept([Command::Stop {
            session: 7,
            pgid: None,
        }]);
        assert!(model.fire("StopSession", &mut state));
        assert!(immediate.is_empty());
        assert_eq!(stopped, [7]);
        assert_eq!(state["watch"], watch_model_projection(&schedule));
        assert!(schedule.next(now).is_none());
        assert!(schedule.due_followups(now + FOLLOW_UPS[1]).is_empty());
        assert_eq!(
            model.action_enabled("IdleRead", &state),
            schedule.due_idle(now + IDLE_RECHECK).is_some()
        );
        assert!(weak.upgrade().is_none(), "the watch released its timeline");
    }

    #[test]
    fn stale_group_stop_preserves_replacement_footer_watch() {
        let model = aterm_models_harness::claude_footer_watch_model();
        let mut state = model.init_state();
        let now = std::time::Instant::now();
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        let mut schedule = WatchSchedule::default();
        schedule.watch(
            Job {
                session: 7,
                timeline: Arc::clone(&timeline),
                pgid: 42,
                wake: LogWake::default(),
            },
            now,
            true,
        );
        assert!(model.fire("AskOld", &mut state));
        let Accepted {
            latest: mut immediate,
            stopped,
            ..
        } = schedule.accept([
            Command::Request(Job {
                session: 7,
                timeline,
                pgid: 43,
                wake: LogWake::default(),
            }),
            Command::Stop {
                session: 7,
                pgid: Some(42),
            },
        ]);
        assert!(model.fire("AskNew", &mut state));
        assert!(model.fire("StopOld", &mut state));
        assert_eq!(stopped, [7]);
        assert_eq!(immediate.len(), 1);
        let replacement = immediate.pop().unwrap();
        assert_eq!(replacement.pgid, 43);
        schedule.watch(replacement, now, true);
        assert_eq!(state["watch"], watch_model_projection(&schedule));
        let Accepted {
            latest: immediate,
            stopped,
            ..
        } = schedule.accept([Command::Stop {
            session: 7,
            pgid: Some(42),
        }]);
        assert!(immediate.is_empty());
        assert!(stopped.is_empty());
        let due = schedule
            .due_idle(now + IDLE_RECHECK)
            .expect("active Claude keeps its idle refresh");
        assert_eq!(due.pgid, 43);
        assert!(model.action_enabled("IdleRead", &state));
    }

    /// A process's transcript cache outlives a [`WATCH_FOR`] lapse and a stop: a model pinned
    /// at `/resume A` is still named at `/resume B`. Negative controls: a dead process or one
    /// with no readable start loses its cache; at most [`MAX_KEPT`], least recently read first.
    #[test]
    fn a_watch_lapse_keeps_the_processs_pin() {
        use std::time::{Instant, UNIX_EPOCH};
        const START: u64 = 1_790_607_848; // 2026-09-28T15:04:08Z
        const S: &str = "00000000-0000-4000-8000-000000000c01";
        const A: &str = "00000000-0000-4000-8000-000000000c02";
        const B: &str = "00000000-0000-4000-8000-000000000c03";
        let root = super::rig::home_root("footer-lapse");
        let _ = std::fs::remove_dir_all(&root);
        let cwd = root.join("work");
        let claude = root.join("claude");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(claude.join("sessions")).unwrap();
        let project = claude.join("projects").join(footer::project_slug(&cwd));
        std::fs::create_dir_all(&project).unwrap();
        let register = |session: &str| {
            std::fs::write(
                claude.join("sessions/4242.json"),
                format!(
                    r#"{{"pid":4242,"sessionId":"{session}","cwd":{},"procStart":"{}","startedAt":{},"version":"2.1.284"}}"#,
                    // A JSON string, as Claude Code writes it (a Windows
                    // path's backslashes escaped).
                    aterm_json::to_string(&cwd.display().to_string()).unwrap(),
                    footer::lstart_utc(START),
                    START * 1000 + 684
                ),
            )
            .unwrap();
        };
        let answer = |session: &str, model: &str, at: &str| {
            std::fs::write(
                project.join(format!("{session}.jsonl")),
                format!(
                    "{{\"type\":\"assistant\",\"timestamp\":\"{at}\",\"effort\":\"high\",\"message\":{{\"model\":\"{model}\"}}}}\n"
                ),
            )
            .unwrap();
        };
        answer(S, "claude-opus-5-5", "2026-09-28T15:05:00.000Z");
        answer(A, "claude-fable-5-1", "2026-09-27T10:00:00.000Z");
        answer(B, "claude-sonnet-5", "2026-09-26T10:00:00.000Z");
        // One read of the process through the kept cache, at `START + secs`
        // on the wall clock.
        let read = |tails: &mut ProcessTails, started: Option<u64>, secs: u64, now: Instant| {
            let entry = footer::session_of_pid(&claude, 4242, started).expect("the registry");
            footer::facts_for_entry_at(
                &claude,
                4242,
                started,
                &entry,
                &LaunchFacts::default(),
                tails.cache(7, 4242, started, now),
                UNIX_EPOCH + Duration::from_secs(START + secs),
            )
            .model
        };
        let fable = Some("Fable 5.1".to_owned());
        // `/resume A` restores and pins; then the watch goes `how`, the
        // kernel says whether the process `lives`, and `/resume B`.
        let run = |started: Option<u64>, how: &str, lives: bool| {
            let t0 = Instant::now();
            let mut schedule = WatchSchedule::default();
            schedule.watch(
                Job {
                    session: 7,
                    timeline: Arc::new(Mutex::new(SessionTimeline::default())),
                    pgid: 4242,
                    wake: LogWake::default(),
                },
                t0,
                true,
            );
            let mut tails = ProcessTails::default();
            register(S);
            assert_eq!(
                read(&mut tails, started, 120, t0).as_deref(),
                Some("Opus 5.5")
            );
            register(A);
            assert_eq!(read(&mut tails, started, 180, t0), fable, "restored");
            let later = t0 + WATCH_FOR;
            match how {
                // A lapse with no limit wall on show.
                "lapse" => schedule.prune(later, |_, _| false),
                "stop" => {
                    let Accepted { stopped, .. } = schedule.accept([Command::Stop {
                        session: 7,
                        pgid: Some(4242),
                    }]);
                    assert_eq!(stopped, [7]);
                }
                other => panic!("{other}"),
            }
            assert!(schedule.watched.is_empty(), "the watch is gone");
            tails.retain(
                &schedule,
                |pgid, born| lives && (pgid, born) == (4242, START),
                later,
            );
            register(B);
            read(&mut tails, started, 900, later)
        };
        assert_eq!(
            run(Some(START), "lapse", true),
            fable,
            "the pin outlived the lapse"
        );
        assert_eq!(run(Some(START), "stop", true), fable, "and the stop");
        // Negative controls: the process gone, and no start to prove it the same: a first
        // sight of B, which names no model.
        assert_eq!(run(Some(START), "lapse", false), None);
        assert_eq!(run(None, "lapse", true), None);
        // The bound: the least recently read goes first.
        let t0 = Instant::now();
        let mut tails = ProcessTails::default();
        tails.cache(7, 4242, Some(START), t0);
        for n in 0..MAX_KEPT {
            let n64 = u64::try_from(n).unwrap();
            tails.cache(
                8 + n64,
                5000 + i32::try_from(n).unwrap(),
                Some(1),
                t0 + Duration::from_secs(1 + n64),
            );
        }
        assert_eq!(tails.kept.len(), MAX_KEPT);
        assert!(!tails.kept.contains_key(&(4242, Some(START))));
        assert!(tails.kept.contains_key(&(5000, Some(1))));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pane_text_reuses_only_the_same_scratch_revision_and_pane() {
        let mut scratch = RenderInput::empty();
        // A host prepend can make `scratch.rows` include its chrome row;
        // `strip` remains based on the window's terminal row count.
        scratch.rows = 4;
        scratch.cols = 4;
        scratch.cells = vec![
            row_of("chrome", [0, 0, 0]),
            row_of("abcd", [0, 0, 0]),
            row_of("efgh", [0, 0, 0]),
            row_of("ijkl", [0, 0, 0]),
        ];
        scratch.snapshot_seq = 5;
        let mut cache = PaneTextCache::default();
        let full = PaneTextRegion {
            strip: 1,
            row_off: 0,
            col_off: 0,
            rows: 3,
            cols: 4,
        };
        let first = cache.rows(0, 7, &scratch, full);
        assert_eq!(&**first, &["abcd", "efgh", "ijkl"]);
        let again = cache.rows(0, 7, &scratch, full);
        assert!(Arc::ptr_eq(&first, &again));

        let other_pane = cache.rows(
            1,
            7,
            &scratch,
            PaneTextRegion {
                strip: 1,
                row_off: 1,
                col_off: 1,
                rows: 2,
                cols: 2,
            },
        );
        assert_eq!(&**other_pane, &["fg", "jk"]);
        assert!(!Arc::ptr_eq(&first, &other_pane));
        let other_session = cache.rows(0, 8, &scratch, full);
        assert_eq!(&**other_session, &**first);
        assert!(!Arc::ptr_eq(&first, &other_session));

        scratch.cells[1][0].ch = 'z';
        scratch.snapshot_seq += 1;
        let changed = cache.rows(0, 8, &scratch, full);
        assert_eq!(&changed[0], "zbcd");
        assert!(!Arc::ptr_eq(&other_session, &changed));
        cache.retain_active(|index| index == 1, |_| false, |_| false);
        assert!(cache.panes[0].is_none());
        assert!(cache.panes[1].is_some());
        cache.trim(1);
        assert_eq!(cache.panes.len(), 1);
        cache.trim(0);
        assert!(cache.panes.is_empty());
    }

    #[test]
    fn changed_claude_frame_reuses_free_row_buffers_but_preserves_held_snapshots() {
        let mut scratch = RenderInput::empty();
        scratch.rows = 2;
        scratch.cols = 4;
        scratch.cells = vec![row_of("abcd", [0, 0, 0]), row_of("efgh", [0, 0, 0])];
        scratch.snapshot_seq = 1;
        let region = PaneTextRegion {
            strip: 0,
            row_off: 0,
            col_off: 0,
            rows: 2,
            cols: 4,
        };
        let mut cache = PaneTextCache::default();
        let first = cache.rows(0, 7, &scratch, region);
        let vec_ptr = first.as_ref().as_ptr();
        let row_ptrs = [first[0].as_ptr(), first[1].as_ptr()];
        drop(first);

        scratch.cells[0][0].ch = 'z';
        scratch.snapshot_seq += 1;
        let reused = cache.rows(0, 7, &scratch, region);
        assert_eq!(&**reused, &["zbcd", "efgh"]);
        assert_eq!(reused.as_ref().as_ptr(), vec_ptr);
        assert_eq!([reused[0].as_ptr(), reused[1].as_ptr()], row_ptrs);

        // A reader still using that exact frame must keep its old text while
        // the next echo builds a new snapshot from fresh storage.
        let held = Arc::clone(&reused);
        scratch.cells[0][1].ch = 'y';
        scratch.snapshot_seq += 1;
        let next = cache.rows(0, 7, &scratch, region);
        assert_eq!(&**held, &["zbcd", "efgh"]);
        assert_eq!(&**next, &["zycd", "efgh"]);
        assert!(!Arc::ptr_eq(&held, &next));

        // A revision that moved no row's text (a re-extract, a compose, a colour) shares the
        // held snapshot and allocates nothing, under a new rebuild number since its cells may
        // differ. Control: the rebuild above, which changed a row, was fresh storage.
        let rebuild = |cache: &PaneTextCache| cache.panes[0].as_ref().unwrap().text_gen;
        let before = rebuild(&cache);
        scratch.cells[1][0].fg = [9, 9, 9];
        scratch.snapshot_seq += 1;
        let (same, allocations) =
            crate::work_counts::allocations_in(|| cache.rows(0, 7, &scratch, region));
        assert!(Arc::ptr_eq(&same, &next), "the held snapshot, shared");
        assert_eq!(allocations, 0);
        assert_ne!(rebuild(&cache), before, "a rebuild all the same");
    }

    fn vendor_frame(scratch: &mut RenderInput) {
        let bottom = scratch.rows - 1;
        let rule = "\u{2500}".repeat(scratch.cols);
        scratch.cells[bottom - 3] = row_of(&rule, [40, 40, 40]);
        scratch.cells[bottom - 2] = row_of("\u{276F} ", [40, 40, 40]);
        scratch.cells[bottom - 1] = row_of(&rule, [40, 40, 40]);
        scratch.cells[bottom] = row_of(
            "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle) \u{00B7} \u{2190} for agents",
            [90, 90, 90],
        );
        // This fixture paints the vendor rows directly over an engine fill.
        scratch.snapshot_seq = scratch.snapshot_seq.wrapping_add(1);
    }

    fn cell(ch: char, fg: [u8; 3]) -> RenderCell {
        RenderCell {
            ch,
            fg,
            ..RenderCell::default()
        }
    }

    fn row_of(text: &str, fg: [u8; 3]) -> Vec<RenderCell> {
        text.chars().map(|c| cell(c, fg)).collect()
    }

    fn text_of(row: &[RenderCell]) -> String {
        read_glass(row)
    }

    /// The cells a footer reading (` ◆ Opus 5.5 xhigh   ⌂ ~/aterm `: its facts
    /// [`footer::GAP`] spaces apart) takes set: its line, and a blank cell either side.
    fn cells_for(reading: &str) -> usize {
        let segments: Vec<footer::Segment> = reading
            .trim()
            .split("   ")
            .map(|fact| {
                let (mark, text) = fact.split_once(' ').expect("<mark> <text>");
                footer::Segment::plain(mark.chars().next().expect("a mark"), text)
            })
            .collect();
        footer::segments_width(&segments) + 2
    }

    /// A cell's ink and rendition, without where its glyph is nudged to.
    fn unplaced(cell: RenderCell) -> RenderCell {
        match cell.script {
            aterm_core::terminal::Script::Sub { .. } => RenderCell {
                script: aterm_core::terminal::Script::Sub { nudge: 0 },
                ..cell
            },
            aterm_core::terminal::Script::Normal => cell,
        }
    }

    fn facts() -> FooterFacts {
        FooterFacts {
            model: Some("Opus 5.5".into()),
            effort: Some("xhigh".into()),
            path: Some("~/aterm".into()),
            branch: Some("main".into()),
            ..FooterFacts::default()
        }
    }

    /// A reader that panics on a pane's rows paints no rule and the frame goes on. Control:
    /// the real reader finds the rule and its mode row on the same rows.
    #[test]
    fn a_reader_panic_paints_no_footer_row() {
        let rows = vec![
            "\u{2500}".repeat(60),
            "\u{276F} ".to_string(),
            "\u{2500}".repeat(60),
            "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)".to_string(),
        ];
        assert_eq!(
            read_footer(9, &rows, read_rule_row),
            Some(RuleRead {
                rule: 2,
                mode_row: true
            }),
            "the real reader finds it"
        );
        let boom = |_: &[String]| -> FooterRead { panic!("stand-in reader panic") };
        assert!(read_footer(9, &rows, boom).is_none());
        assert!(read_footer(9, &rows, boom).is_none(), "every frame");
    }

    /// The footer's layout in a rule: one space either side, three between values, the
    /// rule's last glyph kept; bold `Script::Sub` facts, values lifted [`VALUE_LIFT`] toward
    /// the pane's ink, marks the accent mixed halfway toward the rule's gray.
    #[test]
    fn the_rule_carries_the_owners_footer() {
        let blank = cell(' ', [200, 200, 200]);
        let rule = cell('\u{2500}', [60, 60, 60]);
        let fit = footer::fit_rule(&facts(), 76, None);
        let (cells, lights) = compose_rule(&fit, None, rule, blank, [0, 128, 255], 76);
        assert_eq!(lights, None);
        assert_eq!(
            text_of(&cells),
            " \u{25C6} Opus 5.5 xhigh   \u{2302} ~/aterm   \u{2387} main "
        );
        let o = cells.iter().position(|c| c.ch == 'O').expect("the model");
        assert!(
            [1, o].iter().all(|&c| cells[c].bold
                && matches!(cells[c].script, aterm_core::terminal::Script::Sub { .. })),
            "the facts are drawn small, low and bold"
        );
        assert_eq!(cells[0].script, aterm_core::terminal::Script::Normal);
        assert_eq!(cells.len(), footer::segments_width(&fit.segments) + 2);
        // Nothing known: nothing written, no dangling pad.
        let none = footer::fit_rule(&FooterFacts::default(), 76, None);
        assert!(
            compose_rule(&none, None, rule, blank, [1, 1, 1], 76)
                .0
                .is_empty()
        );
        // With a harness host, Autopilot stands at the rule's left end at
        // rest (owner, 2026-10-15), full size — Cruise's `∞` in the marks'
        // accent itself — the rule's glyphs run on to the owner's facts,
        // unchanged at its right end.
        let harness = crate::claude_lights::Block::of_autopilot(
            crate::autopilot::AutopilotView::Cruise,
            blank,
            [0, 128, 255],
        );
        let widths = footer::LightsWidth {
            chips: harness.lights.len(),
            title: harness.title.len(),
            lean: [0; footer::LEAN_FORMS],
            short: harness.short_title.len(),
            reason: harness.reason,
        };
        let fit = footer::fit_rule(&facts(), 76, Some(widths));
        let (cells, lights) = compose_rule(&fit, Some(&harness), rule, blank, [0, 128, 255], 76);
        assert_eq!(lights, Some(1));
        assert_eq!(cells.len(), 76, "the rule's glyphs run on to the facts");
        let text = text_of(&cells);
        assert!(text.starts_with(" \u{221e} \u{2500}"), "{text:?}");
        assert!(
            text.ends_with(" \u{25C6} Opus 5.5 xhigh   \u{2302} ~/aterm   \u{2387} main "),
            "{text:?}"
        );
        assert_eq!(cells[1].fg, [0, 128, 255], "cruise in the accent");
        assert_eq!(cells[1].script, aterm_core::terminal::Script::Normal);
    }

    /// The facts are painted where `footer::set_facts` sets them: a blank cell, the line's
    /// cells, a blank cell, each glyph nudged to its place and every other cell blank.
    /// Control: the line is shorter than one cell per character.
    #[test]
    fn the_facts_are_painted_where_they_are_set() {
        let blank = cell(' ', [200, 200, 200]);
        let rule = cell('\u{2500}', [60, 60, 60]);
        let fit = footer::fit_rule(&facts(), 76, None);
        let (cells, _) = compose_rule(&fit, None, rule, blank, [0, 128, 255], 76);
        let set = footer::set_facts(&fit.segments);
        assert_eq!(cells.len(), set.cells + 2);
        let mut glyph_cells = std::collections::HashSet::new();
        for glyph in &set.glyphs {
            let painted = cells[1 + glyph.cell];
            assert_eq!(painted.ch, glyph.ch);
            assert_eq!(
                painted.script,
                aterm_core::terminal::Script::Sub { nudge: glyph.nudge },
                "{glyph:?}"
            );
            glyph_cells.insert(1 + glyph.cell);
        }
        for (c, painted) in cells.iter().enumerate() {
            if !glyph_cells.contains(&c) {
                assert_eq!(*painted, blank, "cell {c} is blank");
            }
        }
        let reading = " \u{25C6} Opus 5.5 xhigh   \u{2302} ~/aterm   \u{2387} main ";
        assert_eq!(text_of(&cells), reading);
        assert!(
            cells.len() < reading.chars().count(),
            "CONTROL: {}",
            cells.len()
        );
    }

    /// A LIT FACT'S UNDERLINE LIES UNDER ITS INK (lab, 2026-10-20: on the
    /// model and the effort it sat about a glyph off the text, on the path it
    /// ran two to three cells past it, the clicks landing right). On the
    /// lab's line, the account and the weekly wall ahead of the model, each
    /// glyph is drawn well off the cell it stands in; hovering each fact in
    /// turn, the underline takes exactly the cells whose middles lie between
    /// the left edge of the fact's first glyph and the right edge of its
    /// last, as the painter places them (its cell plus its nudge), each in
    /// the ink of the lit glyph drawn nearest it; the fact's glyphs are
    /// lifted, and every other cell keeps its rest glyph and ink (a
    /// neighbour's glyph under the underline included). NEGATIVE CONTROL: on
    /// this line the cells the path's glyphs stand in start more than a cell
    /// and a half left of its ink and end as far past it, so an underline on
    /// the glyphs' own cells fails.
    #[test]
    fn a_lit_facts_underline_lies_under_its_ink() {
        use aterm_core::terminal::{Script, UnderlineStyle};
        let blank = cell(' ', [200, 200, 200]);
        let rule = cell('\u{2500}', [60, 60, 60]);
        let mark_fg = [0, 128, 255];
        let fit = footer::RuleFit {
            segments: vec![
                footer::Segment::plain(footer::ACCOUNT_MARK, "account unavailable"),
                footer::Segment::plain(footer::WEEKLY_MARK, "weekly unavailable"),
                footer::Segment {
                    split: Some("Opus 5.5 ".len()),
                    ..footer::Segment::plain(footer::MODEL_MARK, "Opus 5.5 xhigh")
                },
                footer::Segment::plain(footer::PATH_MARK, "/private/tmp/at.SnYH/proj"),
                footer::Segment::plain(footer::BRANCH_MARK, "main"),
            ],
            title: footer::TitleFit::None,
            chips: false,
        };
        let room = 120;
        let rest = compose_rule_spans(&fit, None, rule, blank, mark_fg, room, None);
        assert!(
            rest.cells
                .iter()
                .all(|c| c.underline == UnderlineStyle::None),
            "CONTROL: nothing underlined at rest"
        );
        let step = 1.0 / f32::from(Script::NUDGES_PER_CELL);
        for kind in [
            FactKind::Model,
            FactKind::Effort,
            FactKind::Path,
            FactKind::Branch,
        ] {
            let paint = compose_rule_spans(&fit, None, rule, blank, mark_fg, room, Some(kind));
            let span = paint
                .spans
                .iter()
                .flatten()
                .find(|span| span.kind == kind)
                .copied()
                .expect("the fact is on the line");
            let at = paint.facts_at + 1;
            let own = at + usize::from(span.cells.0)..at + usize::from(span.cells.1);
            // The fact's glyphs where the painter draws them.
            let mut drawn: Vec<(f32, f32, [u8; 3])> = Vec::new();
            for c in own.clone() {
                let cell = paint.cells[c];
                if cell.ch == ' ' {
                    continue;
                }
                let Script::Sub { nudge } = cell.script else {
                    panic!("{kind:?}: cell {c} is no set glyph");
                };
                let cols = if paint.cells.get(c + 1).is_some_and(|n| n.wide) {
                    2.0
                } else {
                    1.0
                };
                let left = c as f32 + f32::from(nudge) * step;
                drawn.push((left, left + cols, cell.fg));
                assert_ne!(cell.fg, rest.cells[c].fg, "{kind:?}: cell {c} is lifted");
            }
            let left = drawn.iter().map(|d| d.0).fold(f32::MAX, f32::min);
            let right = drawn.iter().map(|d| d.1).fold(f32::MIN, f32::max);
            for (c, cell) in paint.cells.iter().enumerate() {
                let middle = c as f32 + 0.5;
                let under = (left..right).contains(&middle);
                assert_eq!(
                    cell.underline == UnderlineStyle::Single,
                    under,
                    "{kind:?}: cell {c} ({:?}), the ink {left}..{right}",
                    cell.ch
                );
                if under {
                    let nearest = drawn
                        .iter()
                        .min_by(|a, b| {
                            let d = |g: &(f32, f32, [u8; 3])| ((g.0 + g.1) / 2.0 - middle).abs();
                            d(a).total_cmp(&d(b))
                        })
                        .map(|g| g.2);
                    assert_eq!(cell.underline_color, nearest, "{kind:?}: cell {c}'s ink");
                }
                if !own.contains(&c) {
                    assert_eq!(
                        (cell.ch, cell.fg, cell.script),
                        (rest.cells[c].ch, rest.cells[c].fg, rest.cells[c].script),
                        "{kind:?}: cell {c} is as at rest"
                    );
                }
            }
            if kind == FactKind::Path {
                assert!(
                    left - own.start as f32 > 1.5 && own.end as f32 - right > 1.5,
                    "NEGATIVE CONTROL: the path's own cells {own:?} are not its ink {left}..{right}"
                );
            }
        }
    }

    /// `⎆ <label>` stands between the model and the path, mark and label in their inks, whole
    /// or not at all (a clipped label would name another account), and nothing else moves for
    /// it. Control: with no account the rule reads `the_rule_carries_the_owners_footer`'s.
    #[test]
    fn the_rule_names_the_account_between_the_model_and_the_path() {
        let blank = cell(' ', [200, 200, 200]);
        let rule = cell('\u{2500}', [60, 60, 60]);
        let listed = FooterFacts {
            account: Some("work-2".into()),
            ..facts()
        };
        let paint = |facts: &FooterFacts, room: usize| {
            let fit = footer::fit_rule(facts, room, None);
            compose_rule(&fit, None, rule, blank, [0, 128, 255], room).0
        };
        let plain = " \u{25C6} Opus 5.5 xhigh   \u{2302} ~/aterm   \u{2387} main ";
        let named =
            " \u{25C6} Opus 5.5 xhigh   \u{2386} work-2   \u{2302} ~/aterm   \u{2387} main ";
        let cells = paint(&listed, 76);
        assert_eq!(text_of(&cells), named);
        let at = cells
            .iter()
            .position(|c| c.ch == footer::ACCOUNT_MARK)
            .expect("the account's mark");
        assert_eq!(
            unplaced(cells[at]),
            unplaced(RenderCell {
                ch: footer::ACCOUNT_MARK,
                ..cells[1]
            }),
            "the mark takes the marks' ink"
        );
        let w = at
            + cells[at..]
                .iter()
                .position(|c| c.ch == 'w')
                .expect("the label");
        let o = cells.iter().position(|c| c.ch == 'O').expect("the model");
        assert_eq!(
            unplaced(cells[w]),
            unplaced(RenderCell {
                ch: 'w',
                ..cells[o]
            }),
            "the label takes the values' ink"
        );
        // Negative control: no account, and no mark for one.
        assert_eq!(text_of(&paint(&facts(), 76)), plain);

        // The real splice, at 80 columns.
        let (mut app, wid, session) = glass(80);
        let rows = claude_rows("footer-bypass-idle", 80);
        let bottom = aterm_phase::phase::composer_bottom(&rows).expect("the composer");
        feed_rows(&app, session, &rows);
        let shown = |app: &mut App, facts: FooterFacts| {
            publish(app, session, facts);
            frame(app, wid);
            text_of(&app.windows[&wid].input_scratch.cells[strip(app, wid) + bottom])
        };
        let glass = shown(&mut app, listed);
        assert!(
            glass.ends_with(&format!("\u{2500}{named}\u{2500}")),
            "{glass:?}"
        );
        let glass = shown(&mut app, facts());
        assert!(
            glass.ends_with(&format!("\u{2500}{plain}\u{2500}")),
            "{glass:?}"
        );
    }

    /// A branch or directory name can be wide: it takes a lead cell and a
    /// continuation, and the layout counts both.
    #[test]
    fn wide_names_take_two_cells() {
        let wide = FooterFacts {
            branch: Some("\u{6F22}\u{5B57}".into()),
            ..FooterFacts::default()
        };
        let blank = cell(' ', [1, 1, 1]);
        let fit = footer::fit_rule(&wide, 20, None);
        let (row, _) = compose_rule(
            &fit,
            None,
            cell('\u{2500}', [2, 2, 2]),
            blank,
            [3, 3, 3],
            20,
        );
        let lead = row.iter().position(|c| c.ch == '\u{6F22}').unwrap();
        assert!(
            !row[lead].wide && row[lead + 1].wide,
            "lead, then its right half"
        );
        assert_eq!(
            row[lead + 2..].iter().find(|c| c.ch != ' ').map(|c| c.ch),
            Some('\u{5B57}')
        );
        assert_eq!(row.len(), footer::segments_width(&fit.segments) + 2);
        // `⎇ 漢字` is six pitches, five cells of ink: a line of five cells
        // and two of pad, so a 6-cell rule drops it.
        assert_eq!(cells_for(" \u{2387} \u{6F22}\u{5B57} "), 7);
        assert!(!footer::fit_rule(&wide, 7, None).segments.is_empty());
        assert!(footer::fit_rule(&wide, 6, None).segments.is_empty());
    }

    fn facts_with_usage() -> FooterFacts {
        use aterm_agent::harness::session_usage::{ModelTokens, UsageFacts, Wall};
        FooterFacts {
            usage: UsageFacts::of(
                (
                    vec![ModelTokens {
                        label: "opus".into(),
                        input: 48_000_000,
                        output: 310_000,
                    }],
                    aterm_agent::harness::session_usage::More::default(),
                ),
                Some(Wall {
                    label: "5h",
                    resets: "3pm".into(),
                }),
                None,
            ),
            ..facts()
        }
    }

    /// Every fact's mark takes one ink, the wall's, its `♻` and `Σ`'s among
    /// them; what a narrow rule gives up, and in what order, is
    /// `footer::fit_rule`'s (its own tests).
    #[test]
    fn the_wall_and_usage_marks_take_the_marks_ink() {
        let blank = cell(' ', [200, 200, 200]);
        let rule = cell('\u{2500}', [60, 60, 60]);
        let facts = facts_with_usage();
        let room = 200;
        let fit = footer::fit_rule(&facts, room, None);
        let cells = compose_rule(&fit, None, rule, blank, [0, 128, 255], room).0;
        let ink = |mark: char| unplaced(cells[cells.iter().position(|c| c.ch == mark).unwrap()]);
        for mark in ['\u{29D7}', footer::RESET_MARK, '\u{03A3}'] {
            assert_eq!(
                ink(mark),
                RenderCell {
                    ch: mark,
                    ..ink('\u{25C6}')
                }
            );
        }
    }

    /// The offset cache keeps an answer for an hour and no failure at all: a `date` that
    /// could not run is asked again on the next read, so one failed spawn does not hide a
    /// standing wall for an hour.
    #[test]
    fn the_offset_cache_keeps_answers_and_asks_again_after_a_failure() {
        let mut answers = Vec::new();
        let t0 = std::time::Instant::now();
        let asked = std::cell::Cell::new(0);
        let mut ask = |at: std::time::Instant, answer: Option<i64>| {
            cached_offset(&mut answers, Some("UTC"), 100, at, |_, _| {
                asked.set(asked.get() + 1);
                answer
            })
        };
        assert_eq!(ask(t0, None), None, "the read failed");
        assert_eq!(ask(t0, Some(0)), Some(0), "asked again, not the failure");
        assert_eq!(asked.get(), 2);
        assert_eq!(ask(t0 + Duration::from_secs(60), Some(7)), Some(0), "kept");
        assert_eq!(
            asked.get(),
            2,
            "an answer is not read again within the hour"
        );
        assert_eq!(
            ask(t0 + Duration::from_secs(3600), Some(7)),
            Some(7),
            "an hour on, read afresh"
        );
    }

    /// The resolver's clocks: the idle re-read within the watch, the wall's slower one past
    /// it, and past it only while a wall shows and its process lives (liveness is asked
    /// only then).
    #[test]
    fn a_walled_live_session_stays_watched_on_the_slow_clock() {
        let asked = std::time::Instant::now();
        let within = asked + WATCH_FOR - Duration::from_secs(1);
        let past = asked + WATCH_FOR + Duration::from_secs(1);
        assert_eq!(recheck_after(within, asked), IDLE_RECHECK);
        assert_eq!(recheck_after(past, asked), WALL_RECHECK);
        assert!(still_watched(within, asked, || panic!("not asked within")));
        assert!(still_watched(past, asked, || true));
        assert!(!still_watched(past, asked, || false));
        let mut walled = HashSet::new();
        note_wall(&mut walled, 7, true);
        assert!(walled.contains(&7));
        note_wall(&mut walled, 7, false);
        assert!(walled.is_empty());
        // The schedule keeps the same clocks: past its watch a session stays
        // only while walled and alive, and is re-read on the slow clock.
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        let mut schedule = WatchSchedule::default();
        schedule.watch(
            Job {
                session: 7,
                timeline,
                pgid: 42,
                wake: LogWake::default(),
            },
            asked,
            true,
        );
        let _ = schedule.due_followups(past);
        schedule.read(7, past, true);
        schedule.prune(past, |session, job| session == 7 && job.pgid == 42);
        assert_eq!(schedule.next(past), Some(past + WALL_RECHECK));
        assert!(schedule.due_idle(past + IDLE_RECHECK).is_none());
        assert!(schedule.due_idle(past + WALL_RECHECK).is_some());
        schedule.prune(past, |_, _| false);
        assert!(schedule.watched.is_empty(), "no wall: the watch ends");
    }

    /// A retired session's resolver state goes whole: its fold, its watch,
    /// the follow-ups owed and its wall mark — another session's stays.
    #[test]
    fn forgetting_a_session_drops_its_fold_and_watch() {
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        let job = |session| Job {
            session,
            timeline: Arc::clone(&timeline),
            pgid: 1,
            wake: LogWake::default(),
        };
        let now = std::time::Instant::now();
        let mut schedule = WatchSchedule::default();
        let mut known: HashMap<u64, Identity> = HashMap::new();
        let mut folds: HashMap<u64, footer::FooterCache> = HashMap::new();
        let mut walled: HashSet<u64> = HashSet::new();
        for session in [1, 2] {
            schedule.watch(job(session), now, true);
            known.insert(
                session,
                Identity {
                    pid: 1,
                    started: None,
                    dir: None,
                    launch: LaunchFacts::default(),
                    launch_for: None,
                    login: None,
                    argv: Vec::new(),
                    task: None,
                    login_checked_for: None,
                    stored_login: false,
                    env_stamp: None,
                },
            );
            folds.insert(session, footer::FooterCache::default());
            walled.insert(session);
        }
        // What the status observer's retirement sends (`stop_session`).
        let Accepted {
            latest: immediate,
            stopped,
            ..
        } = schedule.accept([Command::Stop {
            session: 1,
            pgid: None,
        }]);
        assert!(immediate.is_empty());
        assert_eq!(stopped, [1]);
        for session in stopped {
            forget(session, &mut known, &mut folds, &mut walled);
        }
        // Session 2 was watched at `now`, so every one of its follow-ups is
        // due by the last one; none of session 1's is left owed.
        let due = schedule.due_followups(now + FOLLOW_UPS[FOLLOW_UPS.len() - 1]);
        assert_eq!(
            due.len(),
            FOLLOW_UPS.len(),
            "the other session's follow-ups stay owed"
        );
        assert!(
            due.iter().all(|job| job.session == 2),
            "no follow-up owed for the retired session"
        );
        for held in [
            known.contains_key(&1),
            folds.contains_key(&1),
            walled.contains(&1),
            schedule.watched.contains_key(&1),
        ] {
            assert!(!held);
        }
        assert!(known.contains_key(&2) && folds.contains_key(&2) && walled.contains(&2));
        assert!(schedule.watched.contains_key(&2));
    }

    /// A group that leaves the foreground (Claude Code suspended, or another
    /// program in front) releases its session's fold with the watch; the fold
    /// is rebuilt from the transcript when Claude Code is asked for again. A
    /// late stop naming an older group releases nothing.
    #[test]
    fn a_group_leaving_the_foreground_releases_its_fold() {
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        let now = std::time::Instant::now();
        let mut schedule = WatchSchedule::default();
        let mut known: HashMap<u64, Identity> = HashMap::new();
        let mut folds: HashMap<u64, footer::FooterCache> = HashMap::new();
        let mut walled: HashSet<u64> = HashSet::new();
        schedule.watch(
            Job {
                session: 1,
                timeline: Arc::clone(&timeline),
                pgid: 40,
                wake: LogWake::default(),
            },
            now,
            true,
        );
        folds.insert(1, footer::FooterCache::default());
        walled.insert(1);
        let mut stop = |pgid| {
            let Accepted {
                latest: immediate,
                stopped,
                ..
            } = schedule.accept([Command::Stop {
                session: 1,
                pgid: Some(pgid),
            }]);
            assert!(immediate.is_empty());
            for session in &stopped {
                forget(*session, &mut known, &mut folds, &mut walled);
            }
            (stopped, folds.contains_key(&1), walled.contains(&1))
        };
        assert_eq!(stop(39), (vec![], true, true), "an older group's stop");
        assert_eq!(
            stop(40),
            (vec![1], false, false),
            "the watched group's stop"
        );
    }

    /// A roster of two: this machine's own login and one with a directory.
    fn two_accounts() -> accounts::Roster {
        let row = |label: &str, dir: Option<&str>| accounts::Row {
            label: label.into(),
            class: accounts::Class::Dedicated,
            dir: dir.map(str::to_owned),
            ..accounts::Row::default()
        };
        accounts::Roster {
            rows: vec![row("main", None), row("work-2", Some("/logins/work-2"))],
        }
    }

    /// One process's exec read with `env` as its `KEY=value` entries.
    fn args_of(env: &[&str]) -> atpkg::caller_shell::ProcArgs {
        atpkg::caller_shell::ProcArgs {
            argv: vec!["claude".into()],
            env: env.iter().map(|kv| (*kv).to_owned()).collect(),
            ..Default::default()
        }
    }

    /// The exec read of a process under the login directory `dir`.
    fn login_under(dir: &str) -> Option<accounts::ProcessLogin> {
        let login = format!("{}={dir}", accounts::LOGIN_ENV);
        login_of_args(Some(&args_of(&["HOME=/home/someone", &login])))
    }

    use super::rig::{claude_home, scratch_home};

    /// The login is read only off an environment that was read: no read, and one that reads
    /// empty, name no login ("no variable set" would mean the default one, labelling a
    /// process nobody read). Control: an environment with only `HOME` is the default
    /// login, and one that names a login names it.
    #[test]
    fn an_environment_that_was_not_read_names_no_login() {
        assert_eq!(login_of_args(None), None, "no read");
        assert_eq!(login_of_args(Some(&args_of(&[]))), None, "reads empty");
        let roster = two_accounts();
        let default = login_of_args(Some(&args_of(&["HOME=/home/someone"]))).expect("read");
        assert_eq!(accounts::footer_label(&roster, &default), Some("main"));
        let named = login_under("/logins/work-2").expect("read");
        assert_eq!(accounts::footer_label(&roster, &named), Some("work-2"));
        // The same read is the directory's, so what takes a process out of
        // the default home is seen here too.
        let own_home = login_of_args(Some(&args_of(&[
            "HOME=/home/someone",
            "CLAUDE_CONFIG_DIR=/elsewhere",
        ])))
        .expect("read");
        assert_eq!(accounts::footer_label(&roster, &own_home), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn unread_exec_directory_is_retried_for_the_same_process() {
        fn recovered_args(_: i32) -> Option<atpkg::caller_shell::ProcArgs> {
            let home = super::rig::home_root("footer-exec-context");
            Some(atpkg::caller_shell::ProcArgs {
                argv: vec!["claude".to_string()],
                env: vec![
                    format!("HOME={}", home.display()),
                    format!("CLAUDE_CONFIG_DIR={}", home.join("claude").display()),
                ],
                ..Default::default()
            })
        }

        let pid = i32::try_from(std::process::id()).unwrap();
        let started = birth_seconds(pid).expect("this test process's kernel start");
        // Fail before any registry read if the old guessed-home branch returns.
        assert_eq!(claude_dir_of_args(None, pid), None);
        let (root, claude) = claude_home("footer-exec-context", pid, started);
        let profile =
            aterm_agent::harness::profile::path_in(&root, Some(&claude.to_string_lossy()), &claude);
        std::fs::write(
            profile,
            r#"{"oauthAccount":{"accountUuid":"recovered-account","emailAddress":"recovered@example.test","planDisplayName":"Pro"}}"#,
        )
        .unwrap();
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        {
            let mut held = timeline.lock().unwrap();
            held.note_foreground_group(pid);
            held.set_program(pid, Some("claude".to_string()));
        }
        let job = Job {
            session: 17,
            timeline: Arc::clone(&timeline),
            pgid: pid,
            wake: LogWake::new(false),
        };
        let asks = Asks {
            args: |_| None,
            roster: || None,
            home: scratch_home,
            stored_login: |_, _, _| true,
        };
        let mut known = HashMap::new();
        let mut tails = ProcessTails::default();
        let mut fold = footer::FooterCache::default();
        let mut roster = RosterMemo::default();
        let first = resolve_and_post(&mut known, &mut tails, &mut fold, &mut roster, &job, asks);
        assert!(!first.row);
        assert!(
            known.is_empty(),
            "an unread exec context must not pin a guessed directory"
        );

        let recovered = resolve_and_post(
            &mut known,
            &mut tails,
            &mut fold,
            &mut roster,
            &job,
            Asks {
                args: recovered_args,
                ..asks
            },
        );
        assert!(
            recovered.row,
            "the next request finds the custom directory's registry"
        );
        let identity = known.get(&job.session).unwrap();
        assert_eq!(
            identity.started,
            Some(started),
            "the process did not restart"
        );
        assert_eq!(identity.dir.as_deref(), Some(claude.as_path()));
        assert_eq!(
            timeline
                .lock()
                .unwrap()
                .claude_footer()
                .unwrap()
                .subscription
                .as_ref()
                .unwrap()
                .email
                .as_deref(),
            Some("recovered@example.test")
        );
        assert_eq!(
            claude_dir_of_args(Some(&args_of(&["HOME=/home/someone"])), pid),
            Some(PathBuf::from("/home/someone/.claude")),
            "a readable HOME still supplies the default Claude directory"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn readable_launcher_directory_is_provisional_until_the_registered_image_is_read() {
        thread_local! {
            static EXEC_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
            static AUTH_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
        }
        fn args(registered: bool) -> Option<atpkg::caller_shell::ProcArgs> {
            EXEC_READS.set(EXEC_READS.get() + 1);
            let root = super::rig::home_root("footer-readable-launcher");
            let mut env = vec![format!(
                "HOME={}",
                root.join(if registered {
                    "selected-home"
                } else {
                    "launcher-home"
                })
                .display()
            )];
            if registered {
                env.push(format!(
                    "CLAUDE_CONFIG_DIR={}",
                    root.join("claude").display()
                ));
            }
            Some(atpkg::caller_shell::ProcArgs {
                argv: vec![if registered { "claude" } else { "launcher" }.into()],
                env,
                ..Default::default()
            })
        }

        let pid = i32::try_from(std::process::id()).unwrap();
        let started = birth_seconds(pid).expect("this test process's kernel start");
        let (root, claude) = claude_home("footer-readable-launcher", pid, started);
        let launcher_dir = root.join("launcher-home/.claude");
        std::fs::create_dir_all(&launcher_dir).unwrap();
        let now = aterm_agent::supervise::limit::unix_now();
        let profile = aterm_json::json!({
            "oauthAccount": {
                "accountUuid": "selected-account",
                "emailAddress": "selected@example.test",
                "planDisplayName": "Pro"
            },
            "cachedUsageUtilization": {
                "fetchedAtMs": now * 1000,
                "accountUuid": "selected-account",
                "utilization": {
                    "seven_day": { "utilization": 71, "resets_at": "2099-01-01T00:00:00Z" }
                }
            }
        });
        std::fs::write(
            claude.join(".claude.json"),
            aterm_json::to_string(&profile).unwrap(),
        )
        .unwrap();
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        {
            let mut held = timeline.lock().unwrap();
            held.note_foreground_group(pid);
            held.set_program(pid, Some("claude".into()));
        }
        let job = Job {
            session: 18,
            timeline: Arc::clone(&timeline),
            pgid: pid,
            wake: LogWake::new(false),
        };
        let asks = Asks {
            args: |_| args(false),
            roster: || None,
            home: scratch_home,
            stored_login: |_, _, _| {
                AUTH_READS.set(AUTH_READS.get() + 1);
                true
            },
        };
        let mut known = HashMap::new();
        let mut tails = ProcessTails::default();
        let mut fold = footer::FooterCache::default();
        let mut roster = RosterMemo::default();
        let first = resolve_and_post(&mut known, &mut tails, &mut fold, &mut roster, &job, asks);
        assert!(!first.row);
        let provisional = known.get(&job.session).unwrap();
        assert_eq!(provisional.dir.as_deref(), Some(launcher_dir.as_path()));
        assert_eq!(provisional.launch_for, None);

        let selected = Asks {
            args: |_| args(true),
            ..asks
        };
        let recovered = resolve_and_post(
            &mut known,
            &mut tails,
            &mut fold,
            &mut roster,
            &job,
            selected,
        );
        assert!(
            recovered.row,
            "the same process now names its custom directory"
        );
        let confirmed = known.get(&job.session).unwrap().clone();
        assert_eq!(confirmed.started, Some(started));
        assert_eq!(confirmed.dir.as_deref(), Some(claude.as_path()));
        assert_eq!(confirmed.launch_for, Some(Some(started)));
        let display = timeline
            .lock()
            .unwrap()
            .claude_footer()
            .unwrap()
            .subscription
            .clone()
            .unwrap();
        assert_eq!(display.email.as_deref(), Some("selected@example.test"));
        assert_eq!(display.weekly.as_deref(), Some("weekly 29% left"));
        assert_eq!(AUTH_READS.get(), 1);

        // A confirmed image keeps its directory, launch and auth-source verdict;
        // only the subscription's fresh exec guard reads argv/environment again.
        for registry_present in [true, false] {
            if !registry_present {
                std::fs::remove_file(claude.join(format!("sessions/{pid}.json"))).unwrap();
            }
            let before = EXEC_READS.get();
            let found = resolve_and_post(
                &mut known,
                &mut tails,
                &mut fold,
                &mut roster,
                &job,
                selected,
            );
            assert_eq!(found.row, registry_present);
            assert_eq!(EXEC_READS.get(), before + 1);
            assert_eq!(AUTH_READS.get(), 1);
            assert_eq!(known.get(&job.session), Some(&confirmed));
            assert_eq!(
                timeline
                    .lock()
                    .unwrap()
                    .claude_footer()
                    .unwrap()
                    .subscription
                    .as_ref(),
                Some(&display)
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    /// The resolver's roster is read on the first ask, answered from memory
    /// within [`ROSTER_RECHECK`] and read again after it: a footer read costs
    /// no file read of its own, and a roster edited under a running window
    /// shows within one period. CONTROLS: one row, a load that fails and an
    /// empty roster are kept too: a lone row can bind a named login, and
    /// every answer is memoized for the same period.
    #[test]
    fn the_roster_is_read_at_most_every_recheck() {
        let t0 = std::time::Instant::now();
        let asked = std::cell::Cell::new(0);
        let mut memo = RosterMemo::default();
        let mut at = |now: std::time::Instant, answer: Option<accounts::Roster>| {
            memo.at(now, || {
                asked.set(asked.get() + 1);
                answer
            })
            .cloned()
        };
        let two = two_accounts();
        let mut three = two.clone();
        three.rows.push(accounts::Row {
            label: "pool-a".into(),
            class: accounts::Class::Shared,
            dir: Some("/logins/pool-a".into()),
            ..accounts::Row::default()
        });
        let one = accounts::Roster {
            rows: two.rows[..1].to_vec(),
        };
        assert_eq!(at(t0, Some(two.clone())), Some(two.clone()));
        let within = t0 + ROSTER_RECHECK - Duration::from_millis(1);
        assert_eq!(at(within, Some(three.clone())), Some(two.clone()), "kept");
        assert_eq!(asked.get(), 1, "not read again within the period");
        let t1 = t0 + ROSTER_RECHECK;
        assert_eq!(at(t1, Some(three.clone())), Some(three), "read afresh");
        assert_eq!(asked.get(), 2);
        // Negative controls.
        let t2 = t1 + ROSTER_RECHECK;
        assert_eq!(
            at(t2, Some(one.clone())),
            Some(one.clone()),
            "one account binds identity"
        );
        assert_eq!(
            at(t2 + Duration::from_secs(1), Some(two.clone())),
            Some(one),
            "the answer is kept for the period"
        );
        assert_eq!(asked.get(), 3);
        let t3 = t2 + ROSTER_RECHECK;
        assert_eq!(at(t3, None), None, "a load that fails");
        let t4 = t3 + ROSTER_RECHECK;
        assert_eq!(
            at(t4, Some(accounts::Roster::default())),
            Some(accounts::Roster::default()),
            "none listed"
        );
        let t5 = t4 + ROSTER_RECHECK;
        assert_eq!(at(t5, Some(two.clone())), Some(two), "listed again");
        assert_eq!(asked.get(), 6);
    }

    /// The exec read of a process on this machine's own login.
    fn on_main(_: i32) -> Option<atpkg::caller_shell::ProcArgs> {
        Some(args_of(&["HOME=/home/someone"]))
    }

    /// The exec read of a process under `work-2`'s login directory, started
    /// as `claude --resume`.
    fn on_work(_: i32) -> Option<atpkg::caller_shell::ProcArgs> {
        let login = format!("{}=/logins/work-2", accounts::LOGIN_ENV);
        Some(atpkg::caller_shell::ProcArgs {
            argv: vec!["claude".into(), "--resume".into()],
            ..args_of(&["HOME=/home/someone", &login])
        })
    }

    /// An identity as [`resolve_and_post`] builds one: the directory, and
    /// nothing read of the image yet.
    fn unread(pid: i32, claude: &std::path::Path) -> Identity {
        Identity {
            pid,
            started: None,
            dir: Some(claude.to_path_buf()),
            launch: LaunchFacts::default(),
            launch_for: None,
            login: None,
            argv: Vec::new(),
            task: None,
            login_checked_for: None,
            stored_login: false,
            env_stamp: None,
        }
    }

    /// One read of `identity`: the account the timeline then shows — `None`
    /// for no footer, `Some(None)` for a footer that names no account.
    fn account_after(
        identity: &mut Identity,
        roster: &mut RosterMemo,
        asks: Asks,
    ) -> Option<Option<String>> {
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        timeline.lock().unwrap().note_foreground_group(identity.pid);
        resolve_into(
            &timeline,
            identity,
            &mut footer::TailCache::default(),
            &mut footer::FooterCache::default(),
            roster,
            asks,
        );
        let timeline = timeline.lock().unwrap();
        timeline.claude_footer().map(|facts| facts.account.clone())
    }

    #[test]
    fn selected_profile_changes_replace_subscription_even_when_registry_is_unreadable() {
        use aterm_agent::harness::profile::Bucket;

        const PID: i32 = 4243;
        const START: u64 = 1_790_607_848;
        let (root, claude) = claude_home("footer-subscription-resolve", PID, START);
        let profile =
            aterm_agent::harness::profile::path_in(&root, Some(&claude.to_string_lossy()), &claude);
        let now = aterm_agent::supervise::limit::unix_now();
        let content = |email: &str, account: &str| {
            aterm_json::json!({
                "oauthAccount": {
                    "accountUuid": account,
                    "emailAddress": email,
                    "organizationType": "claude_max",
                    "planDisplayName": "Max 5x"
                },
                "cachedUsageUtilization": {
                    "fetchedAtMs": now * 1000,
                    "accountUuid": "first-account",
                    "utilization": {
                        "seven_day": { "utilization": 71, "resets_at": "2099-01-01T00:00:00Z" },
                        "seven_day_opus": { "utilization": 80, "resets_at": "2099-01-01T00:00:00Z" }
                    }
                }
            })
        };
        std::fs::write(
            &profile,
            aterm_json::to_string(&content("first@example.com", "first-account")).unwrap(),
        )
        .unwrap();
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        {
            let mut held = timeline.lock().unwrap();
            held.note_foreground_group(PID);
            held.set_program(PID, Some("claude".into()));
        }
        let mut identity = unread(PID, &claude);
        identity.started = Some(START);
        let mut tail = footer::TailCache::default();
        let mut fold = footer::FooterCache::default();
        let mut memo = RosterMemo::default();
        let asks = Asks {
            args: |_| {
                let root = super::rig::home_root("footer-subscription-resolve");
                Some(args_of(&[
                    &format!("HOME={}", root.display()),
                    &format!("CLAUDE_CONFIG_DIR={}", root.join("claude").display()),
                ]))
            },
            roster: || None,
            home: scratch_home,
            stored_login: |_, _, _| true,
        };
        let mut refresh = || {
            let (_, found) = resolve_facts(
                &timeline,
                &mut identity,
                &mut tail,
                &mut fold,
                &mut memo,
                asks,
            );
            let held = timeline.lock().unwrap();
            let facts = held.claude_footer().unwrap();
            let binding = facts.plan.map(|plan| (plan.window, plan.used_x10));
            (facts.subscription.clone().unwrap(), binding, found.timed)
        };
        let (first, plan, _) = refresh();
        assert_eq!(first.email.as_deref(), Some("first@example.com"));
        assert_eq!(first.weekly.as_deref(), Some("weekly 29% left"));
        assert_eq!(
            plan,
            Some((Bucket::SevenDay, 710)),
            "no model is named, so only the account-wide windows gate it"
        );
        // The launch card names the model the facts leave open: its own weekly bucket
        // now gates the plan, on the next read and on a read that keeps the facts.
        {
            let mut held = timeline.lock().unwrap();
            let owner = held.claude_footer().unwrap().owner.clone().unwrap();
            let card = aterm_phase::LaunchCard {
                version: "2.1.287".into(),
                model: "Opus 5.5".into(),
                effort: None,
            };
            assert!(held.note_claude_card(PID, &owner, card));
        }
        assert_eq!(
            refresh(),
            (first.clone(), Some((Bucket::SevenDayOpus, 800)), true)
        );
        std::fs::remove_file(claude.join(format!("sessions/{PID}.json"))).unwrap();
        assert_eq!(refresh(), (first, Some((Bucket::SevenDayOpus, 800)), true));
        std::fs::write(
            &profile,
            aterm_json::to_string(&content("second@example.com", "second-account")).unwrap(),
        )
        .unwrap();
        let (second, plan, timed) = refresh();
        assert_eq!(second.email.as_deref(), Some("second@example.com"));
        assert!(
            second.weekly.is_none(),
            "the previous account's quota cannot survive"
        );
        assert_eq!(plan, None, "nor can its plan, on the kept facts");
        assert!(!timed);
        // A plan with no weekly quota to show still expires on the clock.
        let five_hour = aterm_json::json!({
            "oauthAccount": { "accountUuid": "second-account", "emailAddress": "second@example.com" },
            "cachedUsageUtilization": {
                "fetchedAtMs": now * 1000, "accountUuid": "second-account",
                "utilization": { "five_hour": { "utilization": 40, "resets_at": "2099-01-01T00:00:00Z" } }
            }
        });
        std::fs::write(&profile, aterm_json::to_string(&five_hour).unwrap()).unwrap();
        let (shown, plan, timed) = refresh();
        assert!(shown.weekly.is_none());
        assert_eq!(plan, Some((Bucket::FiveHour, 400)));
        assert!(timed, "the plan alone keeps the clock");
        std::fs::write(&profile, "{").unwrap();
        assert_eq!(
            refresh(),
            (
                aterm_agent::harness::profile::SubscriptionDisplay::default(),
                None,
                false
            )
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_muse_login_store_switch_drops_the_previous_accounts_card_facts() {
        const PID: i32 = 4246;
        let root = crate::test_scratch::process_root("footer-muse-account-switch");
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        let store = home.join(".config/muse");
        std::fs::create_dir_all(&store).unwrap();
        let auth = store.join("auth.json");
        let write_auth = |email: &str| {
            std::fs::write(
                &auth,
                format!(
                    r#"{{"providers":{{"meta":{{"mechanism":"oauth","user_email":"{email}"}}}}}}"#
                ),
            )
            .unwrap()
        };
        write_auth("first@example.test");
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        {
            let mut held = timeline.lock().unwrap();
            held.note_foreground_group(PID);
            held.set_program(PID, Some("muse".into()));
            assert!(held.set_claude_footer(
                PID,
                None,
                Some(FooterFacts {
                    owner: Some(footer::FactsOwner {
                        floor: None,
                        session_id: format!("muse-{PID}"),
                    }),
                    subscription: Some(aterm_agent::harness::profile::SubscriptionDisplay {
                        email: Some("first@example.test".into()),
                        plan: Some("Muse Code Max Usage".into()),
                        weekly: Some("weekly 29% left".into()),
                        weekly_reset: None,
                    },),
                    ..FooterFacts::default()
                }),
            ));
        }
        let mut identity = unread(PID, &root.join("claude"));
        let mut tail = footer::TailCache::default();
        let mut fold = footer::FooterCache::default();
        let mut memo = RosterMemo::default();
        let asks = Asks {
            args: |_| {
                let root = crate::test_scratch::process_root("footer-muse-account-switch");
                Some(atpkg::caller_shell::ProcArgs {
                    argv: vec!["muse".into()],
                    env: vec![format!("HOME={}", root.join("home").display())],
                    ..Default::default()
                })
            },
            roster: || None,
            home: scratch_home,
            stored_login: |_, _, _| true,
        };
        let refresh = |identity: &mut Identity,
                       tail: &mut footer::TailCache,
                       fold: &mut footer::FooterCache,
                       memo: &mut RosterMemo,
                       now| {
            let clock = footer::ReadClock {
                read_at: std::time::UNIX_EPOCH,
                now,
                offset_at: &|_, _| Some(0),
            };
            resolve_facts_at_with_cwd(
                &timeline,
                identity,
                tail,
                fold,
                memo,
                (asks, |_| None),
                &clock,
            );
            timeline
                .lock()
                .unwrap()
                .claude_footer()
                .and_then(|facts| facts.subscription.clone())
                .unwrap()
        };
        const RESET: i64 = 1_792_947_600;
        let first = refresh(&mut identity, &mut tail, &mut fold, &mut memo, RESET - 1);
        assert_eq!(first.email.as_deref(), Some("first@example.test"));
        assert_eq!(first.plan.as_deref(), Some("Muse Code Max Usage"));
        assert_eq!(first.weekly.as_deref(), Some("weekly 29% left"));
        // This is still A's window, before any switch. Its occurrence is
        // placed once, remains valid before the deadline, then expires at
        // it while the account and plan stand. The retained reset is what
        // makes an unchanged card an expired observation, not a fresh one.
        let clock = footer::ReadClock {
            read_at: std::time::UNIX_EPOCH,
            now: RESET - 1,
            offset_at: &|_, _| Some(0),
        };
        let reset = aterm_agent::harness::profile::WeeklyReset {
            words: "Oct 25 at 5:00 PM".into(),
            at: muse_weekly_reset_at("Oct 25 at 5:00 PM", &clock),
        };
        assert_eq!(reset.at, Some(RESET));
        {
            let mut held = timeline.lock().unwrap();
            let mut facts = held.claude_footer().cloned().expect("facts");
            let sub = facts.subscription.as_mut().expect("subscription");
            sub.weekly_reset = Some(reset.clone());
            assert!(held.set_claude_footer(PID, None, Some(facts)));
        }
        let before = refresh(&mut identity, &mut tail, &mut fold, &mut memo, RESET - 1);
        assert_eq!(before.weekly.as_deref(), Some("weekly 29% left"));
        assert_eq!(before.weekly_reset.as_ref(), Some(&reset));
        let expired = refresh(&mut identity, &mut tail, &mut fold, &mut memo, RESET);
        assert_eq!(expired.email.as_deref(), Some("first@example.test"));
        assert_eq!(expired.plan.as_deref(), Some("Muse Code Max Usage"));
        assert_eq!(
            (expired.weekly, expired.weekly_reset.as_ref()),
            (None, Some(&reset)),
            "expiry keeps the observation's occurrence, not its percentage"
        );
        // A relative reset is placed once; repeated reads cannot keep
        // moving that occurrence an hour into the future.
        let relative = aterm_agent::harness::profile::WeeklyReset {
            words: "in 1 hour".into(),
            at: muse_weekly_reset_at("in 1 hour", &clock),
        };
        let relative_at = RESET - 1 + 3600;
        assert_eq!(relative.at, Some(relative_at));
        {
            let mut held = timeline.lock().unwrap();
            let mut facts = held.claude_footer().cloned().unwrap();
            let sub = facts.subscription.as_mut().unwrap();
            sub.weekly = Some("weekly 29% left".into());
            sub.weekly_reset = Some(relative.clone());
            assert!(held.set_claude_footer(PID, None, Some(facts)));
        }
        let before = refresh(
            &mut identity,
            &mut tail,
            &mut fold,
            &mut memo,
            relative_at - 1,
        );
        assert_eq!(before.weekly.as_deref(), Some("weekly 29% left"));
        let expired = refresh(&mut identity, &mut tail, &mut fold, &mut memo, relative_at);
        assert_eq!(
            (expired.weekly, expired.weekly_reset),
            (None, Some(relative))
        );
        // Unknown reset words retain their explicit lack of a placed
        // occurrence: the clock cannot invent an expiry for them.
        let unknown = aterm_agent::harness::profile::WeeklyReset {
            words: "not a reset".into(),
            at: muse_weekly_reset_at("not a reset", &clock),
        };
        assert_eq!(unknown.at, None);
        {
            let mut held = timeline.lock().unwrap();
            let mut facts = held.claude_footer().cloned().unwrap();
            let sub = facts.subscription.as_mut().unwrap();
            sub.weekly = Some("weekly 29% left".into());
            sub.weekly_reset = Some(unknown.clone());
            assert!(held.set_claude_footer(PID, None, Some(facts)));
        }
        let kept = refresh(
            &mut identity,
            &mut tail,
            &mut fold,
            &mut memo,
            relative_at + 60,
        );
        assert_eq!(kept.weekly.as_deref(), Some("weekly 29% left"));
        assert_eq!(kept.weekly_reset, Some(unknown));
        write_auth("second@example.test");
        let second = refresh(
            &mut identity,
            &mut tail,
            &mut fold,
            &mut memo,
            relative_at + 61,
        );
        assert_eq!(second.email.as_deref(), Some("second@example.test"));
        assert_eq!(
            (second.plan, second.weekly, second.weekly_reset),
            (None, None, None),
            "the previous account's tier, quota and reset cannot survive"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn subscription_requires_stored_auth_for_the_registered_image_and_directory() {
        const PID: i32 = 4245;
        const IMAGE: u64 = 1_790_607_848;
        thread_local! {
            static CHECKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
            static ALLOWED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        }
        fn args(_: i32) -> Option<atpkg::caller_shell::ProcArgs> {
            let root = super::rig::home_root("footer-subscription-auth");
            Some(atpkg::caller_shell::ProcArgs {
                argv: vec![
                    "claude".into(),
                    "--settings".into(),
                    r#"{"apiKeyHelper":"/opt/key.sh"}"#.into(),
                ],
                env: vec![
                    format!("HOME={}", root.display()),
                    format!("CLAUDE_CONFIG_DIR={}", root.join("claude").display()),
                ],
                ..Default::default()
            })
        }
        let (root, claude) = claude_home("footer-subscription-auth", PID, IMAGE);
        let profile =
            aterm_agent::harness::profile::path_in(&root, Some(&claude.to_string_lossy()), &claude);
        let now = aterm_agent::supervise::limit::unix_now();
        let content = aterm_json::json!({
            "oauthAccount": { "accountUuid": "auth-account", "emailAddress": "auth@example.com", "planDisplayName": "Max 5x" },
            "cachedUsageUtilization": {
                "fetchedAtMs": now * 1000, "accountUuid": "auth-account",
                "utilization": { "seven_day": { "utilization": 71, "resets_at": "2099-01-01T00:00:00Z" } }
            }
        });
        std::fs::write(&profile, aterm_json::to_string(&content).unwrap()).unwrap();
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        {
            let mut held = timeline.lock().unwrap();
            held.note_foreground_group(PID);
            held.set_program(PID, Some("claude".into()));
        }
        let mut identity = unread(PID, &claude);
        identity.started = Some(IMAGE);
        let mut tail = footer::TailCache::default();
        let mut fold = footer::FooterCache::default();
        let mut memo = RosterMemo::default();
        let asks = Asks {
            args,
            roster: || None,
            home: scratch_home,
            stored_login: |pid, argv, cwd| {
                assert_eq!(pid, PID as u32);
                assert_eq!(argv[0], "claude");
                assert!(std::path::Path::new(cwd).is_absolute());
                CHECKS.set(CHECKS.get() + 1);
                ALLOWED.get()
            },
        };
        let mut refresh = |asks| {
            resolve_facts(
                &timeline,
                &mut identity,
                &mut tail,
                &mut fold,
                &mut memo,
                asks,
            );
            timeline
                .lock()
                .unwrap()
                .claude_footer()
                .unwrap()
                .subscription
                .clone()
                .unwrap()
        };
        let unavailable = aterm_agent::harness::profile::SubscriptionDisplay::default();
        assert_eq!(
            refresh(asks),
            unavailable,
            "helper/provider/refused sources suppress every field"
        );
        assert_eq!(refresh(asks), unavailable);
        assert_eq!(
            CHECKS.get(),
            1,
            "one auth-source gather per registered image"
        );
        ALLOWED.set(true);
        assert_eq!(
            refresh(asks),
            unavailable,
            "the verdict belongs to the old image"
        );

        let registry = claude.join(format!("sessions/{PID}.json"));
        let mut row: aterm_json::Value =
            aterm_json::from_slice(&std::fs::read(&registry).unwrap()).unwrap();
        row.as_object_mut()
            .unwrap()
            .insert("startedAt".into(), aterm_json::json!((IMAGE + 60) * 1000));
        std::fs::write(&registry, aterm_json::to_string(&row).unwrap()).unwrap();
        let display = refresh(asks);
        assert_eq!(display.email.as_deref(), Some("auth@example.com"));
        assert_eq!(display.plan.as_deref(), Some("Max 5x"));
        assert_eq!(display.weekly.as_deref(), Some("weekly 29% left"));
        assert_eq!(refresh(asks), display);
        assert_eq!(CHECKS.get(), 2);

        let cwd = root.join("another-work");
        std::fs::create_dir(&cwd).unwrap();
        row.as_object_mut()
            .unwrap()
            .insert("cwd".into(), aterm_json::json!(cwd));
        std::fs::write(&registry, aterm_json::to_string(&row).unwrap()).unwrap();
        assert_eq!(refresh(asks), display);
        assert_eq!(
            CHECKS.get(),
            3,
            "different project settings need a new gather"
        );
        let unregistered = Asks {
            args: |pid| {
                let mut args = args(pid)?;
                args.argv.push("--new-image".into());
                Some(args)
            },
            ..asks
        };
        assert_eq!(
            refresh(unregistered),
            unavailable,
            "an exec cannot borrow a prior image's verdict"
        );
        assert_eq!(CHECKS.get(), 3);
        let changed_home = Asks {
            args: |pid| {
                let mut args = args(pid)?;
                args.env[0].push_str("/another-home");
                Some(args)
            },
            ..asks
        };
        assert_eq!(
            refresh(changed_home),
            unavailable,
            "the same argv with another HOME cannot borrow the old settings verdict"
        );
        assert_eq!(CHECKS.get(), 3);
        assert_ne!(
            exec_env_stamp(&["A=B".into(), "C=D".into()]),
            exec_env_stamp(&["A=BC=D".into()]),
            "entry boundaries are part of the environment identity"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn weekly_quota_expires_on_the_local_clock_after_the_idle_watch_lapses() {
        const PID: i32 = 4244;
        const START: u64 = 1_790_607_848;
        const OBSERVED: i64 = 1_792_050_000;
        let (root, claude) = claude_home("footer-subscription-expiry", PID, START);
        let profile =
            aterm_agent::harness::profile::path_in(&root, Some(&claude.to_string_lossy()), &claude);
        let asks = Asks {
            args: |_| {
                let root = super::rig::home_root("footer-subscription-expiry");
                Some(args_of(&[
                    &format!("HOME={}", root.display()),
                    &format!("CLAUDE_CONFIG_DIR={}", root.join("claude").display()),
                ]))
            },
            roster: || None,
            home: scratch_home,
            stored_login: |_, _, _| true,
        };
        // Both the natural fifteen-minute age and a reset before that age must
        // clear a settled percentage. No sleep, real account or network is involved.
        for (reset, expires_after) in [("2099-01-01T00:00:00Z", 900), ("2026-10-15T07:50:50Z", 650)]
        {
            let content = aterm_json::json!({
                "oauthAccount": {
                    "accountUuid": "expiry-account",
                    "emailAddress": "expiry@example.com",
                    "organizationType": "claude_max",
                    "planDisplayName": "Max 5x"
                },
                "cachedUsageUtilization": {
                    "fetchedAtMs": OBSERVED * 1000,
                    "accountUuid": "expiry-account",
                    "utilization": { "seven_day": { "utilization": 71, "resets_at": reset } }
                }
            });
            std::fs::write(&profile, aterm_json::to_string(&content).unwrap()).unwrap();
            let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
            {
                let mut held = timeline.lock().unwrap();
                held.note_foreground_group(PID);
                held.set_program(PID, Some("claude".into()));
            }
            let mut identity = unread(PID, &claude);
            identity.started = Some(START);
            let mut tail = footer::TailCache::default();
            let mut fold = footer::FooterCache::default();
            let mut memo = RosterMemo::default();
            let mut refresh = |elapsed| {
                let (_, found) = resolve_facts_at(
                    &timeline,
                    &mut identity,
                    &mut tail,
                    &mut fold,
                    &mut memo,
                    asks,
                    &footer::ReadClock {
                        read_at: std::time::SystemTime::now(),
                        now: OBSERVED + elapsed,
                        offset_at: &|_, _| Some(0),
                    },
                );
                let display = timeline
                    .lock()
                    .unwrap()
                    .claude_footer()
                    .unwrap()
                    .subscription
                    .clone()
                    .unwrap();
                (found, display)
            };
            let (found, display) = refresh(601);
            assert_eq!(display.weekly.as_deref(), Some("weekly 29% left"));
            assert!(found.timed, "weekly without a wall still needs the clock");

            let asked = std::time::Instant::now();
            let mut schedule = WatchSchedule::default();
            schedule.watch(
                Job {
                    session: 7,
                    timeline: timeline.clone(),
                    pgid: PID,
                    wake: LogWake::default(),
                },
                asked,
                true,
            );
            let past = asked + Duration::from_secs(601);
            let _ = schedule.due_followups(past);
            schedule.read(7, past, found.row);
            schedule.prune(past, |_, _| found.timed);
            assert!(
                schedule.watched.contains_key(&7),
                "quota keeps the idle clock"
            );
            for elapsed in (661..=961).step_by(60) {
                let at = asked + Duration::from_secs(elapsed as u64);
                assert_eq!(schedule.next(at), Some(at));
                assert!(schedule.due_idle(at).is_some());
                let (found, display) = refresh(elapsed);
                assert_eq!(display.email.as_deref(), Some("expiry@example.com"));
                assert_eq!(display.plan.as_deref(), Some("Max 5x"));
                schedule.read(7, at, found.row);
                schedule.prune(at, |_, _| found.timed);
                if elapsed > expires_after {
                    assert!(display.weekly.is_none(), "stale quota cleared: {reset}");
                    assert!(!found.timed);
                    assert!(
                        schedule.watched.is_empty(),
                        "quota expiry releases the clock"
                    );
                    assert!(elapsed <= expires_after + 60, "bounded by the slow clock");
                    break;
                }
                assert!(display.weekly.is_some());
            }
            assert!(schedule.watched.is_empty());
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    /// The read that answers carries the label of the row the process's login names.
    /// Controls, each publishing facts with no account: a login no row names; no roster; an
    /// environment that reads empty and a refused exec read, for which the roster is not
    /// asked (the refused read is asked again at the next footer read). A read with no
    /// answer (no registry row) asks neither the process nor the roster.
    #[test]
    fn a_read_that_answers_is_given_its_account() {
        const PID: i32 = 4242;
        let (root, claude) = claude_home("footer-account", PID, 1_790_607_848);
        let listed = |args| Asks {
            args,
            roster: || Some(two_accounts()),
            home: scratch_home,
            stored_login: |_, _, _| true,
        };
        let unasked = |args| Asks {
            args,
            roster: || unreachable!("no login was read: the roster is not asked"),
            home: scratch_home,
            stored_login: |_, _, _| true,
        };
        let read = |asks: Asks| {
            let mut memo = RosterMemo::default();
            let account = account_after(&mut unread(PID, &claude), &mut memo, asks);
            (account, memo.read.is_some())
        };
        assert_eq!(read(listed(on_work)), (Some(Some("work-2".into())), true));
        assert_eq!(
            read(listed(on_main)),
            (Some(Some("main".into())), true),
            "the default login is the row with no directory"
        );
        // Negative controls: the footer is published, and names no account.
        let nobody = |_| {
            let login = format!("{}=/logins/nobody", accounts::LOGIN_ENV);
            Some(args_of(&["HOME=/home/someone", &login]))
        };
        assert_eq!(read(listed(nobody)), (Some(None), true), "no row names it");
        let none = Asks {
            args: on_main,
            roster: || None,
            home: scratch_home,
            stored_login: |_, _, _| true,
        };
        assert_eq!(read(none), (Some(None), true), "no roster: asked, none");
        assert_eq!(
            read(unasked(|_| Some(args_of(&[])))),
            (Some(None), false),
            "an environment that reads empty"
        );
        // An exec read that is refused: no label, and the next read asks the
        // process again.
        let mut identity = unread(PID, &claude);
        let mut memo = RosterMemo::default();
        assert_eq!(
            account_after(&mut identity, &mut memo, unasked(|_| None)),
            Some(None)
        );
        assert_eq!(
            identity.launch_for, None,
            "not read: nothing is kept for it"
        );
        assert_eq!(
            account_after(&mut identity, &mut memo, listed(on_main)),
            Some(Some("main".into())),
            "read at the next"
        );
        // A read with no answer (no registry row) asks nothing.
        std::fs::remove_file(claude.join(format!("sessions/{PID}.json"))).unwrap();
        let nothing = Asks {
            args: |_| unreachable!("no registry row: the process is not read"),
            roster: || unreachable!("no facts: the roster is not asked"),
            home: scratch_home,
            stored_login: |_, _, _| true,
        };
        assert_eq!(read(nothing), (None, false));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// THE SURVEY'S EXCERPTS are published beside the facts by the read that
    /// answers, from the same tail read: the newest prompt and words, none
    /// painted (the facts compare equal with or without them). A worker's
    /// task head comes only from a file its environment names (none here).
    /// NEGATIVE CONTROL: a read with no answer (no registry row) publishes
    /// none.
    #[test]
    fn a_read_publishes_the_excerpts_beside_the_facts() {
        const PID: i32 = 4243;
        let (root, claude) = claude_home("footer-excerpts", PID, 1_790_607_848);
        let project = claude
            .join("projects")
            .join(footer::project_slug(&root.join("work")));
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("00000000-0000-4000-8000-000000000a01.jsonl"),
            concat!(
                r#"{"type":"user","timestamp":"2026-09-28T15:00:00Z","message":{"content":"wire the survey"}}"#,
                "\n",
                r#"{"type":"assistant","timestamp":"2026-09-28T15:00:05Z","message":{"id":"m1","model":"claude-opus-5-5","content":[{"type":"text","text":"On it."}]}}"#,
                "\n",
            ),
        )
        .unwrap();
        let asks = Asks {
            args: |_| Some(args_of(&["HOME=/home/someone"])),
            roster: || None,
            home: scratch_home,
            stored_login: |_, _, _| true,
        };
        let read = |asks: Asks| {
            let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
            timeline.lock().unwrap().note_foreground_group(PID);
            resolve_into(
                &timeline,
                &mut unread(PID, &claude),
                &mut footer::TailCache::default(),
                &mut footer::FooterCache::default(),
                &mut RosterMemo::default(),
                asks,
            );
            let timeline = timeline.lock().unwrap();
            (
                timeline.claude_footer().is_some(),
                timeline.claude_excerpts().cloned(),
            )
        };
        let (facts, excerpts) = read(asks);
        assert!(facts, "the footer is published");
        let excerpts = excerpts.expect("and its excerpts");
        assert_eq!(excerpts.excerpts.prompts.len(), 1);
        assert_eq!(excerpts.excerpts.prompts[0].text, "wire the survey");
        assert_eq!(excerpts.excerpts.said.as_deref(), Some("On it."));
        assert_eq!(excerpts.task, None, "no task file named");
        std::fs::remove_file(claude.join(format!("sessions/{PID}.json"))).unwrap();
        let nothing = Asks {
            args: |_| unreachable!("no registry row: the process is not read"),
            roster: || unreachable!("no facts: the roster is not asked"),
            home: scratch_home,
            stored_login: |_, _, _| true,
        };
        assert_eq!(read(nothing), (false, None));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// THE SIDE RIDES BESIDE THE FACTS: a read publishes the absolute cwd
    /// (the registry's, outside any repository: no root) and no traffic
    /// before the session has a transcript; a read that moved nothing
    /// republishes the SAME side, the facts unchanged, so nothing repaints;
    /// and a new side on the timeline leaves the facts as they were (a
    /// republish of them moves nothing). CONTROLS, each a new side: a
    /// repository around the cwd names its root; a transcript, once folded,
    /// brings its traffic; a moved cwd. A group not in front is neither
    /// given a side nor shown one.
    #[test]
    fn the_side_rides_beside_the_facts() {
        const PID: i32 = 4244;
        const IMAGE: u64 = 1_790_607_848;
        let (root, claude) = claude_home("footer-side", PID, IMAGE);
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        timeline.lock().unwrap().note_foreground_group(PID);
        let asks = Asks {
            args: |_| Some(args_of(&["HOME=/home/someone"])),
            roster: || None,
            home: scratch_home,
            stored_login: |_, _, _| false,
        };
        let mut identity = unread(PID, &claude);
        let (mut tail, mut fold, mut memo) = (
            footer::TailCache::default(),
            footer::FooterCache::default(),
            RosterMemo::default(),
        );
        let mut read = || {
            let (moved, ..) = resolve_into(
                &timeline,
                &mut identity,
                &mut tail,
                &mut fold,
                &mut memo,
                asks,
            );
            let timeline = timeline.lock().unwrap();
            (
                moved.is_some(),
                Arc::clone(timeline.claude_side().expect("a side")),
                timeline.claude_footer().cloned().expect("facts"),
            )
        };
        let work = root.join("work");
        let (moved, first, facts) = read();
        assert!(moved, "the first facts");
        assert_eq!(first.cwd.as_deref(), Some(work.as_path()));
        assert_eq!(
            (first.repo_root.as_ref(), first.traffic.as_ref()),
            (None, None)
        );
        let (moved, again, same) = read();
        assert!(!moved, "nothing moved: nothing repaints");
        assert!(
            Arc::ptr_eq(&first, &again),
            "the same side, not a rebuilt one"
        );
        assert_eq!(same, facts);
        // A new side leaves the facts as they were: republishing them moves
        // nothing.
        {
            let mut timeline = timeline.lock().unwrap();
            timeline.set_claude_side(PID, Some(Arc::new(FactSide::default())));
            assert_eq!(timeline.claude_footer(), Some(&facts));
            assert!(!timeline.set_claude_footer(PID, None, Some(facts.clone())));
            timeline.set_claude_side(PID, Some(Arc::clone(&first)));
        }
        // CONTROL: a repository around the cwd.
        std::fs::create_dir_all(work.join(".git")).unwrap();
        std::fs::write(work.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        let (_, rooted, _) = read();
        assert!(!Arc::ptr_eq(&first, &rooted));
        assert_eq!(rooted.repo_root.as_deref(), Some(work.as_path()));
        // CONTROL: a transcript, graphed once folded, and held while nothing
        // moved.
        std::fs::write(
            super::rig::transcript_of(&root, &claude),
            concat!(
                r#"{"type":"assistant","timestamp":"2026-10-19T12:00:05Z","message":{"id":"m1","model":"claude-opus-5-5","usage":{"input_tokens":10,"output_tokens":7,"cache_read_input_tokens":990}}}"#,
                "\n",
            ),
        )
        .unwrap();
        let (_, graphed, _) = read();
        assert!(!Arc::ptr_eq(&rooted, &graphed));
        let traffic = graphed.traffic.as_ref().expect("its traffic");
        assert_eq!(
            (traffic.totals, &*traffic.buckets),
            ((1_000, 7), &[(1_000, 7)][..])
        );
        let (moved, held, _) = read();
        assert!(
            !moved && Arc::ptr_eq(&graphed, &held),
            "held while nothing moved"
        );
        // CONTROL: a moved cwd.
        let elsewhere = root.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(
            claude.join(format!("sessions/{PID}.json")),
            format!(
                r#"{{"pid":{PID},"sessionId":"{}","cwd":{},"startedAt":{},"version":"2.1.287"}}"#,
                super::rig::SESSION_ID,
                aterm_json::to_string(&elsewhere.display().to_string()).unwrap(),
                IMAGE * 1000 + 684
            ),
        )
        .unwrap();
        let (_, moved_side, _) = read();
        assert!(!Arc::ptr_eq(&held, &moved_side));
        assert_eq!(moved_side.cwd.as_deref(), Some(elsewhere.as_path()));
        // A group not in front: its side is not taken, and none is shown.
        let mut timeline = timeline.lock().unwrap();
        timeline.set_claude_side(PID + 1, Some(Arc::new(FactSide::default())));
        assert!(Arc::ptr_eq(
            timeline.claude_side().expect("still this group's"),
            &moved_side
        ));
        timeline.note_foreground_group(PID + 1);
        assert!(timeline.claude_side().is_none());
        drop(timeline);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The login is the registered image's: what the process was started with before Claude
    /// Code wrote its registry row may be a launcher's (a shim named `claude` that sets
    /// variables and execs in place), so the login and command line are read at the first
    /// row whatever the identity held, and again when a row names a new image. Control: a
    /// row naming the same image reads nothing and keeps what was read.
    #[test]
    fn the_login_is_the_registered_images() {
        const PID: i32 = 4242;
        const IMAGE: u64 = 1_790_607_848;
        let (root, claude) = claude_home("footer-login", PID, IMAGE);
        let listed = |args| Asks {
            args,
            roster: || Some(two_accounts()),
            home: scratch_home,
            stored_login: |_, _, _| true,
        };
        let mut memo = RosterMemo::default();
        // What a read before the registry row held: the launcher's.
        let mut identity = Identity {
            login: login_of_args(on_main(PID).as_ref()),
            argv: vec!["shim".into()],
            ..unread(PID, &claude)
        };
        assert_eq!(
            account_after(&mut identity, &mut memo, listed(on_work)),
            Some(Some("work-2".into())),
            "the first row: read again"
        );
        assert_eq!(identity.launch_for, Some(Some(IMAGE)));
        assert_eq!(
            identity.argv,
            ["claude", "--resume"],
            "its command line too"
        );
        // Negative control: the same image.
        let same = listed(|_| unreachable!("the image was read: not again"));
        assert_eq!(
            account_after(&mut identity, &mut memo, same),
            Some(Some("work-2".into()))
        );
        // A new image in place.
        let _ = claude_home("footer-login", PID, IMAGE + 60);
        assert_eq!(
            account_after(&mut identity, &mut memo, listed(on_main)),
            Some(Some("main".into())),
            "a new image: read again"
        );
        assert_eq!(identity.launch_for, Some(Some(IMAGE + 60)));
        assert_eq!(identity.argv, ["claude"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A rule cell counts as the rule's only while it is the rule's own glyph
    /// in the rule's own colours with nothing in a side channel: a label in
    /// another colour, a selection's background and a cluster are ink.
    #[test]
    fn only_plain_rule_glyphs_are_coverable() {
        let mut s = RenderInput::empty();
        s.rows = 1;
        s.cols = 20;
        let mut row = row_of(&"\u{2500}".repeat(20), [60, 60, 60]);
        row[3].fg = [255, 0, 0]; // a label's ink
        row[6].bg = [0, 0, 200]; // a selection
        s.cells = vec![row];
        s.clusters = vec![vec![(9, "e\u{301}".into())]];
        s.combining = vec![Vec::new()];
        s.images = vec![Vec::new()];
        let (mask, rule) = rule_mask(&s, 0, 0, 20).unwrap();
        assert_eq!(rule.fg, [60, 60, 60]);
        // A highlight over the rule's end is not the rule's style.
        let mut lit = s.clone();
        for c in &mut lit.cells[0][17..] {
            c.bg = [0, 0, 200];
        }
        let (mask_lit, rule_lit) = rule_mask(&lit, 0, 0, 20).unwrap();
        assert_eq!(
            rule_lit.bg,
            [0, 0, 0],
            "the style most of the rule is drawn in"
        );
        assert_eq!(footer::rule_run(&mask_lit), Some(10..17));
        let ink: Vec<usize> = (0..20).filter(|&c| !mask[c]).collect();
        assert_eq!(ink, [3, 6, 9]);
        assert_eq!(footer::rule_run(&mask), Some(10..20));
        // A trimmed row: the missing cells are no rule.
        s.cells[0].truncate(12);
        let (mask, _) = rule_mask(&s, 0, 0, 20).unwrap();
        assert!(!mask[12] && mask[11]);
        // No rule glyph at all: no mask.
        s.cells[0] = row_of("  hello", [1, 1, 1]);
        assert!(rule_mask(&s, 0, 0, 20).is_none());
    }

    /// A scratch as an engine fill leaves it: every reuse token armed, a live
    /// revision lane, rows trimmed of trailing blanks.
    fn engine_fill(rows: usize) -> RenderInput {
        let mut s = RenderInput::empty();
        s.rows = rows;
        s.cols = 40;
        s.cells = (0..rows)
            .map(|r| row_of(&format!("engine row {r}"), [9, 9, 9]))
            .collect();
        s.clusters = (0..rows).map(|_| Vec::new()).collect();
        s.combining = (0..rows).map(|_| Vec::new()).collect();
        s.images = (0..rows).map(|_| Vec::new()).collect();
        s.snapshot_seq = 40;
        s.engine_fill_seq = 40;
        s.shifted_fill_seq = 40;
        s.terminal_id = 7;
        s.extract_gen = 3;
        s.row_rev_lane = 7;
        s.row_rev = (0..rows).map(|r| 100 + r as u64).collect();
        s
    }

    /// [`write_edits`] of owned edits into a fresh slot.
    fn written(
        s: &mut RenderInput,
        edits: Vec<(usize, usize, Vec<RenderCell>, RenderCell)>,
    ) -> FooterUndo {
        let mut slot = FooterUndo::default();
        write_edits(
            &mut slot,
            s,
            edits
                .iter()
                .map(|(row, col, cells, ground)| (*row, *col, &cells[..], *ground)),
        );
        slot
    }

    /// A write undone and made again fills the rows the last one left and allocates nothing,
    /// the cells and the undo exactly as a fresh slot's. Control: the first write into a
    /// fresh slot allocates its rows.
    #[test]
    fn a_footer_written_again_allocates_nothing() {
        let mut s = engine_fill(4);
        let original = s.cells.clone();
        let painted = row_of("  \u{25C6} Opus 5.5 xhigh", [1, 1, 1]);
        let chip = row_of("\u{221e} \u{23F8} plan", [2, 2, 2]);
        let ground = cell(' ', [0, 0, 0]);
        let edits = [(3, 0, &painted[..], ground), (2, 30, &chip[..], ground)];
        let mut slot = FooterUndo::default();
        let ((), fresh) = crate::work_counts::allocations_in(|| {
            write_edits(&mut slot, &mut s, edits);
        });
        assert!(fresh > 0, "CONTROL: a fresh slot allocates its rows");
        let first = s.cells.clone();
        for frame in 0..3 {
            assert!(undo(&mut slot, &mut s), "frame {frame}");
            assert_eq!(s.cells, original, "frame {frame}: undone exactly");
            let ((), again) = crate::work_counts::allocations_in(|| {
                write_edits(&mut slot, &mut s, edits);
            });
            assert_eq!(again, 0, "frame {frame}: the rows' buffers reused");
            assert_eq!(s.cells, first, "frame {frame}: the same write");
        }
    }

    #[test]
    fn unicode_footer_side_channels_undo_exactly_reuse_buffers_and_respect_foreign_writes() {
        let blank = cell(' ', [0, 0, 0]);
        let rule = cell('\u{2500}', [60, 60, 60]);
        let facts = FooterFacts {
            subscription: Some(aterm_agent::harness::profile::SubscriptionDisplay {
                email: Some("jose\u{0301}👩‍💻@example.com".into()),
                plan: Some("Pro".into()),
                weekly: Some("weekly 29% left".into()),
                weekly_reset: None,
            }),
            ..FooterFacts::default()
        };
        let fit = footer::fit_rule(&facts, 76, None);
        let paint = compose_rule_spans(&fit, None, rule, blank, [0, 128, 255], 76, None);
        assert!(!paint.clusters.is_empty() && !paint.combining.is_empty());
        let mut scratch = engine_fill(3);
        scratch.cells[2] = vec![rule; 100];
        scratch.clusters.resize_with(3, Vec::new);
        scratch.combining.resize_with(3, Vec::new);
        scratch.combining[0].push((0, vec!['\u{0300}'].into_boxed_slice()));
        let original = scratch.clone();
        let mut slot = FooterUndo::default();
        let write = |slot: &mut FooterUndo, scratch: &mut RenderInput| {
            write_edits(
                slot,
                scratch,
                rule_edits(2, 10, &paint.cells, &paint.glass, blank),
            );
            slot.publish_text(scratch, 2, 10, &paint);
        };
        write(&mut slot, &mut scratch);
        let painted = scratch.clone();
        for frame in 0..3 {
            assert!(undo(&mut slot, &mut scratch), "frame {frame}");
            assert_eq!(
                scratch, original,
                "cells, metadata and reuse identity restored"
            );
            let ((), allocations) =
                crate::work_counts::allocations_in(|| write(&mut slot, &mut scratch));
            assert_eq!(allocations, 0, "frame {frame}: metadata buffers reused");
            assert_eq!(scratch, painted);
        }
        scratch.combining[2][0].1 = vec!['\u{0300}'].into_boxed_slice();
        let changed = scratch.clone();
        assert!(
            !undo(&mut slot, &mut scratch),
            "another writer changed our mark"
        );
        assert_eq!(
            scratch, changed,
            "foreign metadata and cells are left alone"
        );

        let mut occupied = original;
        occupied.combining[2].push((
            10 + paint.combining[0].0,
            vec!['\u{0300}'].into_boxed_slice(),
        ));
        let before = occupied.clone();
        write(&mut slot, &mut occupied);
        assert_eq!(
            occupied, before,
            "a skipped vendor span gets no footer metadata"
        );
        assert!(!slot.is_written());
    }

    /// The footer comes off exactly and the scratch is an engine fill again: the state
    /// DMG-1's scoped refill, the effect-only reuse and D-2's stamp compare all require.
    #[test]
    fn the_undo_gives_back_the_engine_fill_exactly() {
        let mut s = engine_fill(4);
        let original = s.cells.clone();
        let painted = row_of("  \u{25C6} Opus 5.5 xhigh", [1, 1, 1]);
        let mut slot = written(&mut s, vec![(3, 0, painted.clone(), cell(' ', [0, 0, 0]))]);
        assert!(slot.is_written());
        assert_eq!(&s.cells[3][..painted.len()], painted.as_slice());
        assert_ne!(s.snapshot_seq, s.engine_fill_seq, "the write is declared");
        assert_eq!(
            s.shifted_fill_seq, s.snapshot_seq,
            "an untouched engine fill keeps the renderer's stamp compare"
        );
        assert_eq!(s.row_rev[3], 0, "the written row is compared by content");
        assert_eq!(s.row_rev[2], 102, "every other row keeps its stamp");
        assert!(undo(&mut slot, &mut s));
        assert!(!slot.is_written(), "consumed");
        assert_eq!(s.cells, original, "cells and trimmed lengths restored");
        assert_eq!(s.snapshot_seq, 40);
        assert_eq!(s.engine_fill_seq, 40);
        assert_eq!(s.shifted_fill_seq, 40);
        assert_eq!(s.row_rev, vec![100, 101, 102, 103]);
    }

    /// THE PROGRAM'S CELLS UNDER THE GLASS (review G3,
    /// [`FooterUndo::program_cells`]): read without writing, every row reads
    /// in every column window what the undo puts back — a span over a row's
    /// text as the text it covered, a span past a row's trimmed end as the
    /// row ending where it ended — and nothing is allocated. A row whose
    /// painted span a later splice covered reads as presented: the glass
    /// there is no longer the footer's. NEGATIVE CONTROL: the presented rows
    /// hold the glass's cells.
    #[test]
    fn the_program_cells_read_what_the_undo_puts_back() {
        let mut s = engine_fill(4);
        let original = s.cells.clone();
        let ground = cell(' ', [0, 0, 0]);
        let over = row_of("YOLO on", [1, 1, 1]);
        let past = row_of("\u{25C6} Opus", [2, 2, 2]);
        let slot = written(&mut s, vec![(1, 2, over, ground), (2, 20, past, ground)]);
        let text = |cells: &mut dyn Iterator<Item = &RenderCell>| -> String {
            cells.map(|c| c.ch).collect()
        };
        assert!(text(&mut s.cells[1].iter()).contains("YOLO on"), "CONTROL");
        assert!(
            text(&mut s.cells[2].iter()).ends_with("\u{25C6} Opus"),
            "CONTROL"
        );
        for (row, engine) in original.iter().enumerate() {
            for cols in [0..40, 0..5, 3..30, 25..40, 13..13] {
                let want: String = engine
                    .iter()
                    .skip(cols.start)
                    .take(cols.len())
                    .map(|c| c.ch)
                    .collect();
                assert_eq!(
                    text(&mut slot.program_cells(&s, row, cols.clone())),
                    want,
                    "row {row}, columns {cols:?}"
                );
            }
        }
        let (count, allocations) =
            crate::work_counts::allocations_in(|| slot.program_cells(&s, 2, 0..40).count());
        assert_eq!((count, allocations), (original[2].len(), 0));
        // A later splice over the relabel's cells: the row as presented.
        s.cells[1][3].ch = 'M';
        let presented = text(&mut s.cells[1].iter());
        assert_eq!(text(&mut slot.program_cells(&s, 1, 0..40)), presented);
    }

    /// Anything else writing after the footer — or a refill that moved no
    /// token but changed the cells — and the undo changes nothing.
    #[test]
    fn a_scratch_touched_since_is_left_alone() {
        let painted = row_of("  footer", [1, 1, 1]);
        let mut s = engine_fill(3);
        let mut slot = written(&mut s, vec![(2, 0, painted.clone(), cell(' ', [0, 0, 0]))]);
        s.snapshot_seq += 1; // another host write
        let before = (s.cells.clone(), s.snapshot_seq, s.row_rev.clone());
        assert!(!undo(&mut slot, &mut s));
        assert_eq!((s.cells.clone(), s.snapshot_seq, s.row_rev.clone()), before);
        assert!(!slot.is_written(), "a stale record is dropped");

        let mut s = engine_fill(3);
        let mut slot = written(&mut s, vec![(2, 0, painted, cell(' ', [0, 0, 0]))]);
        s.cells[2][3].ch = 'X';
        assert!(
            !undo(&mut slot, &mut s),
            "the painted span must still be ours"
        );
        assert_eq!(s.cells[2][3].ch, 'X');
    }

    /// A composition with chips goes on as two edits ([`rule_edits`]), the rule's own glyphs
    /// between the chips and the facts left as the glass shows them: the same glass and tokens
    /// as the whole composition written, undone to the same engine fill, for the chips' and
    /// the facts' cells alone, none written over itself. Without chips it is one edit.
    /// Negative controls: written whole, every cell is written and the middle over itself;
    /// over a middle that is not the rule's glyph, the two writes differ.
    #[test]
    fn a_rule_with_chips_is_written_where_it_changes() {
        let rule = cell('\u{2500}', [5, 5, 5]);
        let ground = cell(' ', [0, 0, 0]);
        let fill = || {
            let mut s = engine_fill(3);
            s.cells[2] = vec![rule; 48];
            s
        };
        let mut cells = row_of(" \u{221e} \u{23F8} plan ", [2, 2, 2]);
        let glass = cells.len()..cells.len() + 24;
        cells.extend(std::iter::repeat_n(rule, glass.len()));
        cells.extend(row_of(" Opus 5.5 ", [3, 3, 3]));
        let col = 48 - 1 - cells.len();
        let split_write = |s: &mut RenderInput, glass: &std::ops::Range<usize>| {
            let mut slot = FooterUndo::default();
            let _ = crate::work_counts::take_footer_cells_written();
            write_edits(&mut slot, s, rule_edits(2, col, &cells, glass, ground));
            (slot, crate::work_counts::take_footer_cells_written())
        };
        let original = fill().cells;
        let mut whole = fill();
        let _ = crate::work_counts::take_footer_cells_written();
        let mut whole_slot = written(&mut whole, vec![(2, col, cells.clone(), ground)]);
        let whole_cost = crate::work_counts::take_footer_cells_written();
        let mut split = fill();
        let (mut slot, cost) = split_write(&mut split, &glass);
        assert_eq!(split.cells, whole.cells, "the same glass");
        assert_eq!(
            (split.snapshot_seq, &split.row_rev),
            (whole.snapshot_seq, &whole.row_rev),
            "the same tokens"
        );
        assert_eq!(cost, cells.len() - glass.len(), "the chips and the facts");
        assert_eq!(slot.rewritten_as_they_were(), 0);
        assert_eq!(whole_cost, cells.len(), "CONTROL: whole, every cell");
        assert_eq!(
            whole_slot.rewritten_as_they_were(),
            glass.len(),
            "CONTROL: whole, the middle written over itself"
        );
        assert!(undo(&mut slot, &mut split));
        assert!(undo(&mut whole_slot, &mut whole));
        assert_eq!((&split.cells, &whole.cells), (&original, &original));
        // No chips: the composition is one edit, written whole.
        let mut bare = fill();
        let (_, cost) = split_write(&mut bare, &(0..0));
        assert_eq!(cost, cells.len());
        // Negative control: a middle that is not the rule's glyph.
        let (mut whole, mut split) = (fill(), fill());
        for s in [&mut whole, &mut split] {
            s.cells[2][col + glass.start + 3] = cell('x', [5, 5, 5]);
        }
        let _ = written(&mut whole, vec![(2, col, cells.clone(), ground)]);
        let _ = split_write(&mut split, &glass);
        assert_ne!(
            split.cells, whole.cells,
            "CONTROL: the skip keeps what is there"
        );
    }

    /// Two panes side by side write one frame row; the undo peels them off
    /// newest first, down to the engine's trimmed length.
    #[test]
    fn two_writes_on_one_row_come_off_in_order() {
        let mut s = engine_fill(2);
        let original = s.cells.clone();
        let left = row_of("  left footer      ", [1, 1, 1]);
        let right = row_of("  right footer", [2, 2, 2]);
        let ground = cell(' ', [0, 0, 0]);
        let mut slot = written(&mut s, vec![(1, 0, left, ground), (1, 20, right, ground)]);
        assert_eq!(s.cells[1].len(), 34, "padded to the right pane's edge");
        assert!(undo(&mut slot, &mut s));
        assert_eq!(s.cells, original);
    }

    /// A mid-rule span comes off exactly, its D-2 stamp with it; a span past a trimmed row is
    /// padded with the pane's blank, and the undo gives the trimmed length back.
    #[test]
    fn a_span_in_the_rule_row_comes_off_exactly() {
        let mut s = engine_fill(4);
        s.cells[2] = row_of(&"\u{2500}".repeat(40), [60, 60, 60]);
        let original = s.cells.clone();
        let facts = footer::fit_rule(&facts(), 36, None);
        let blank = cell(' ', [200, 200, 200]);
        let (cells, _) = compose_rule(&facts, None, s.cells[2][0], blank, [0, 128, 255], 36);
        let col = 40 - 1 - cells.len();
        let mut slot = written(&mut s, vec![(2, col, cells.clone(), blank)]);
        assert_eq!(s.cells[2].len(), 40, "nothing padded");
        assert_eq!(&s.cells[2][col..39], cells.as_slice());
        assert_eq!(
            s.cells[2][39], original[2][39],
            "the rule's last glyph stays"
        );
        assert!(s.cells[2][..col].iter().all(|c| *c == original[2][0]));
        assert_eq!(s.row_rev[2], 0, "the rule row is compared by content");
        assert!(undo(&mut slot, &mut s));
        assert_eq!(s.cells, original);
        assert_eq!(s.row_rev, vec![100, 101, 102, 103]);
        // Past a trimmed row: padded with the pane's blank.
        let mut s = engine_fill(2);
        let original = s.cells.clone();
        let chip = cell('\u{23F8}', [9, 9, 9]);
        let mut slot = written(&mut s, vec![(1, 30, vec![chip], blank)]);
        assert!(
            s.cells[1][12..30].iter().all(|c| *c == blank),
            "{:?}",
            &s.cells[1][12..30]
        );
        assert!(undo(&mut slot, &mut s));
        assert_eq!(s.cells, original);
    }

    /// A span holding a grapheme cluster is not the plain row the planner
    /// read, and the undo could not restore a removed cluster: no write.
    #[test]
    fn a_span_with_side_channel_content_is_not_written() {
        let mut s = engine_fill(2);
        s.clusters[1].push((4, "e\u{301}".into()));
        let before = s.cells.clone();
        let slot = written(
            &mut s,
            vec![(1, 0, row_of("  footer", [1, 1, 1]), cell(' ', [0, 0, 0]))],
        );
        assert!(!slot.is_written());
        assert_eq!(s.cells, before);
        assert_eq!(s.snapshot_seq, 40, "nothing written, nothing declared");
    }

    /// A scratch some other host already wrote is not re-blessed: the
    /// renderer's stamp compare stays off, exactly as that write left it.
    #[test]
    fn only_an_untouched_engine_fill_keeps_its_stamps() {
        let mut s = engine_fill(2);
        s.snapshot_seq = 41; // e.g. the find bar, earlier this frame
        let mut slot = written(
            &mut s,
            vec![(1, 0, row_of("  footer", [1, 1, 1]), cell(' ', [0, 0, 0]))],
        );
        assert_eq!(s.shifted_fill_seq, 40, "not re-armed");
        assert_eq!(s.row_rev[1], 101, "not cleared");
        assert!(undo(&mut slot, &mut s));
        assert_eq!(s.snapshot_seq, 41, "back to the other write's own bump");
    }

    /// Under a blessed chrome prepend the footer keeps the stamp compare on, and the
    /// production order (footer undo, then the strip's inverse) gives back the engine fill.
    #[test]
    fn a_blessed_chrome_prepend_keeps_the_stamps_and_its_inverse() {
        let mut s = engine_fill(3);
        let engine_cells = s.cells.clone();
        // The prepend, as `prepend_strip_rows` makes it.
        let blessed = s.host_prepend_blessing();
        assert!(blessed);
        s.cells.insert(0, row_of("strip", [7, 7, 7]));
        s.clusters.insert(0, Vec::new());
        s.combining.insert(0, Vec::new());
        s.images.insert(0, Vec::new());
        s.rows += 1;
        s.snapshot_seq += 1;
        s.note_host_row_prepend(1, blessed);
        let mut slot = written(
            &mut s,
            vec![(3, 0, row_of("  footer", [1, 1, 1]), cell(' ', [0, 0, 0]))],
        );
        assert_eq!(
            s.shifted_fill_seq, s.snapshot_seq,
            "the stamp compare stays on under the strip"
        );
        assert_eq!(s.row_rev[3], 0, "the written row is compared by content");
        assert!(undo(&mut slot, &mut s));
        let mut pool = Vec::new();
        assert!(
            s.undo_host_row_prepend(3, &mut pool, 1),
            "the strip's inverse still runs"
        );
        assert_eq!(s.cells, engine_cells);
        assert_eq!(s.snapshot_seq, s.engine_fill_seq);
    }

    /// The drift check names only a pill-shaped row this build cannot read:
    /// never a screen the footer reads, never a box, never plain text.
    #[test]
    fn only_an_unreadable_pill_row_is_drift() {
        let screen = |mode_row: &str| -> Vec<String> {
            let rule = "\u{2500}".repeat(60);
            vec![rule.clone(), "\u{276F} ".into(), rule, mode_row.into()]
        };
        let renamed = "  \u{23F5}\u{23F5} turbo mode on (shift+tab to cycle)";
        assert_eq!(unread_pill_row(&screen(renamed)).as_deref(), Some(renamed));
        for fine in [
            "  \u{23F5}\u{23F5} auto mode on (shift+tab to cycle)",
            "  \u{23F8} manual mode on \u{00B7} ? for shortcuts",
            "  ? for shortcuts",
            "  \u{23F5}\u{23F5} bypass permissi",
        ] {
            assert_eq!(unread_pill_row(&screen(fine)), None, "{fine:?}");
        }
        assert_eq!(unread_pill_row(&["$ ls".to_owned()]), None, "no composer");
    }

    // ---- On the glass: a real engine, a real frame, the real splice ----

    /// The measured 2.1.283 screen `name` (`src/fixtures` of aterm-phase,
    /// 80x24), at `cols` columns: every full-width rule rebuilt at that
    /// width, its label kept at the right end.
    fn claude_rows(name: &str, cols: usize) -> Vec<String> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../aterm-phase/src/fixtures")
            .join(format!("claude-2.1.283-{name}.txt"));
        let rows: Vec<String> = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
            .lines()
            .skip(1)
            .map(|row| {
                let rule = row.starts_with('\u{2500}') && row.trim_end().ends_with('\u{2500}');
                if !rule {
                    return row.to_owned();
                }
                let label: String = row.trim_start_matches('\u{2500}').to_owned();
                let n = cols - label.chars().count();
                format!("{}{label}", "\u{2500}".repeat(n))
            })
            .collect();
        assert!(rows.len() <= 24, "{name}: an 80x24 capture");
        rows
    }

    /// The SGR a row is drawn in, as Claude draws it: rules dim, a rule's
    /// label and the mode pill in hues of their own, the rest of the mode
    /// row dim — so a cell the footer wrongly took shows up in its colour.
    fn painted(row: &str) -> String {
        const RULE: &str = "\x1b[38;2;90;90;90m";
        const LABEL: &str = "\x1b[38;2;200;120;255m";
        const PILL: &str = "\x1b[38;2;255;110;90m";
        const DIM: &str = "\x1b[38;2;130;130;130m";
        if row.starts_with('\u{2500}') {
            // A labelled rule as the vendors draw it: the dashes in the
            // rule's colour on both sides, the label between in its own
            // (Claude Code's effort tag; Muse Code's `Voice input …`,
            // measured `4b4f55` rule, `8a9098` label).
            let rule: String = row.chars().take_while(|c| *c == '\u{2500}').collect();
            let rest = &row[rule.len()..];
            let trail = rest
                .chars()
                .rev()
                .take_while(|c| *c == '\u{2500}')
                .map(char::len_utf8)
                .sum::<usize>();
            let (label, end) = rest.split_at(rest.len() - trail);
            return format!("{RULE}{rule}{LABEL}{label}{RULE}{end}\x1b[0m");
        }
        if footer::is_mode_row(row) {
            let at = row.find(" on").map_or(row.len(), |i| i + 3);
            return format!("{PILL}{}{DIM}{}\x1b[0m", &row[..at], &row[at..]);
        }
        row.to_owned()
    }

    fn feed_rows(app: &App, session: u64, rows: &[String]) {
        let mut out = String::from("\x1b[?2004h\x1b[2J");
        for (i, row) in rows.iter().enumerate() {
            out.push_str(&format!("\x1b[{};1H{}", i + 1, painted(row)));
        }
        let caret = rows
            .iter()
            .rposition(|r| r.starts_with(['\u{276F}', '>']))
            .unwrap_or(0);
        out.push_str(&format!("\x1b[{};3H", caret + 1));
        let term = app.pool.get(session).expect("session").term.clone();
        crate::term_lock(&term).process(out.as_bytes());
    }

    fn publish(app: &App, session: u64, facts: FooterFacts) {
        let entry = app.pool.get(session).expect("session");
        let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline.note_foreground_group(4242);
        assert!(timeline.set_claude_footer(4242, None, Some(facts)));
    }

    /// One frame through the capture route's splices (the terminal arm of
    /// `App::render_image`): extract, the strip, then the footer.
    fn frame(app: &mut App, wid: WindowId) {
        frame_with_clock(app, wid, None);
    }

    fn frame_with_clock(
        app: &mut App,
        wid: WindowId,
        clock: Option<&dyn Fn() -> footer::ReadClock<'static>>,
    ) {
        let prepared = app.prepare_terminal_capture_grid_with_cursor_fx_and_plan_outcome(
            wid,
            crate::app_render::ComposedCursorFxClock::Advance(std::time::Instant::now()),
        );
        let crate::app_render::CapturePreparation::Ready((grid, plan)) = prepared else {
            panic!("the headless capture must produce a frame");
        };
        app.splice_tab_strip(wid);
        let route = VisibleContentRoute::Terminal {
            composed: grid.composed,
        };
        match clock {
            Some(clock) => app.splice_claude_footer_at(wid, &plan, route, clock),
            None => app.splice_claude_footer(wid, &plan, route),
        }
    }

    fn glass(cols: u16) -> (App, WindowId, u64) {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let session = app.focused_session_id(wid).expect("the front session");
        if cols != 80 {
            assert!(app.apply_term_resize(wid, 24, cols));
        }
        (app, wid, session)
    }

    fn strip(app: &App, wid: WindowId) -> usize {
        app.windows[&wid].input_scratch.cells.len() - usize::from(app.windows[&wid].rows)
    }

    /// Over the measured 2.1.283 screens at 80, 131 and 150 columns the facts end the rule
    /// in the footer's inks, only plain rule glyphs are covered, and Claude's own row under
    /// the rule is byte for byte what it drew.
    #[test]
    fn the_facts_go_into_the_rule_and_claudes_row_stays_whole() {
        let footer_text = " \u{25C6} Opus 5.5 xhigh   \u{2302} ~/aterm   \u{2387} main \u{2500}";
        for (name, cols, default_mode) in [
            ("footer-bypass-idle", 80, false),
            ("footer-bypass-idle", 131, false),
            ("footer-bypass-idle", 150, false),
            ("footer-busy", 80, false),
            ("footer-busy", 131, false),
            ("footer-plan", 80, false),
            ("footer-manual", 150, false),
            ("footer-accept-edits", 80, false),
            ("footer-auto-draft", 131, false),
            ("footer-bypass-idle", 80, true),
        ] {
            let label = format!(
                "{name}@{cols}{}",
                if default_mode { " (default mode)" } else { "" }
            );
            let (mut app, wid, session) = glass(cols);
            let mut rows = claude_rows(name, usize::from(cols));
            let bottom = aterm_phase::phase::composer_bottom(&rows).expect("the composer");
            if default_mode {
                rows[bottom + 1] = "  ? for shortcuts".into();
            }
            feed_rows(&app, session, &rows);
            frame(&mut app, wid);
            let strip = strip(&app, wid);
            let vendor: Vec<Vec<RenderCell>> = app.windows[&wid].input_scratch.cells.clone();
            publish(&app, session, facts());
            frame(&mut app, wid);
            let glass = &app.windows[&wid].input_scratch.cells;
            let rule = &glass[strip + bottom];
            assert!(
                text_of(rule).trim_end().ends_with(footer_text),
                "{label}: {:?}",
                text_of(rule)
            );
            let at = rule.iter().position(|c| c.ch == '\u{25C6}').unwrap();
            assert_eq!(rule.len(), usize::from(cols), "{label}");
            let set = footer_text.trim_end_matches('\u{2500}');
            assert_eq!(
                at,
                usize::from(cols) - cells_for(set),
                "{label}: right-aligned, the rule's last glyph after it"
            );
            let o = rule.iter().position(|c| c.ch == 'O').unwrap();
            let glyph = *vendor[strip + bottom].last().unwrap();
            let ink = app.windows[&wid].input_scratch.implicit_blank.fg;
            assert_eq!(
                rule[o].fg,
                mix(glyph.fg, ink, VALUE_LIFT),
                "{label}: the value, in the rule's gray lifted toward the pane's ink"
            );
            assert!(
                matches!(rule[o].script, aterm_core::terminal::Script::Sub { .. }),
                "{label}: small and low"
            );
            for (c, (was, now)) in vendor[strip + bottom].iter().zip(rule).enumerate() {
                if was != now {
                    assert_eq!(*was, glyph, "{label}: col {c} was not a plain rule glyph");
                }
            }
            assert_eq!(
                rule.last(),
                Some(&glyph),
                "{label}: the rule's last glyph stays"
            );
            for r in bottom + 1..rows.len() {
                assert_eq!(
                    glass[strip + r],
                    vendor[strip + r],
                    "{label}: row {r} under the rule is Claude's, every cell"
                );
            }
            for r in 0..bottom {
                assert_eq!(glass[strip + r], vendor[strip + r], "{label}: row {r}");
            }
        }
        // The usage switch (`[claude_footer] usage`, default on): `Σ` shows beside a
        // standing limit wall, and `usage = false` takes `Σ` away, the wall staying.
        let (mut app, wid, session) = glass(150);
        let rows = claude_rows("footer-bypass-idle", 150);
        let bottom = aterm_phase::phase::composer_bottom(&rows).expect("the composer");
        feed_rows(&app, session, &rows);
        publish(&app, session, facts_with_usage());
        frame(&mut app, wid);
        let rule =
            |app: &App| text_of(&app.windows[&wid].input_scratch.cells[strip(app, wid) + bottom]);
        let shown = rule(&app);
        assert!(
            shown.contains('\u{29D7}') && shown.contains('\u{03A3}'),
            "{shown:?}"
        );
        app.config.claude_footer = Some(crate::app_config::ClaudeFooterConfig {
            usage: Some(false),
            ..Default::default()
        });
        frame(&mut app, wid);
        let hidden = rule(&app);
        assert!(
            hidden.contains('\u{29D7}') && !hidden.contains('\u{03A3}'),
            "{hidden:?}"
        );
    }

    /// Nothing is painted where no composer is drawn: a dialog or a picker that replaced it
    /// (the effort-switch box). Control: the same facts on the idle screen are painted.
    #[test]
    fn no_composer_no_footer() {
        let (mut app, wid, session) = glass(80);
        let boxed: Vec<String> = [
            "\u{25CF} done",
            "",
            &"\u{2594}".repeat(80),
            "   Change effort level?",
            "",
            "   \u{276F} 1. Yes, switch to high",
            "     2. No, go back",
        ]
        .iter()
        .map(|r| (*r).to_owned())
        .collect();
        feed_rows(&app, session, &boxed);
        publish(&app, session, facts());
        frame(&mut app, wid);
        assert!(
            !app.windows[&wid]
                .input_scratch
                .cells
                .iter()
                .any(|row| row.iter().any(|c| c.ch == '\u{25C6}')),
            "no footer anywhere"
        );
        feed_rows(&app, session, &claude_rows("footer-bypass-idle", 80));
        frame(&mut app, wid);
        assert!(
            app.windows[&wid]
                .input_scratch
                .cells
                .iter()
                .any(|row| row.iter().any(|c| c.ch == '\u{25C6}'))
        );
    }

    /// In a split pane the footer goes into the Claude pane's own rule, inside its columns,
    /// and gives way to its width (a half-width rule cuts a long path to its last
    /// directory); the sibling's cells on that row are untouched.
    #[test]
    fn a_split_panes_rule_carries_its_own_footer() {
        let (mut app, wid, session) = glass(80);
        let _other = app.split_active_stub_tab(wid);
        let (pane_rows, cols) = {
            let term = crate::term_lock(&app.pool.get(session).unwrap().term);
            (usize::from(term.rows()), usize::from(term.cols()))
        };
        assert!(cols < 60, "a half-width pane: {cols}");
        // The pane's content sits below its subtab title row: its last row is
        // the window's last row whatever the header takes from the top.
        let last = usize::from(app.windows[&wid].rows) - 1;
        assert!(
            pane_rows < last + 1,
            "a split pane gives a row to its title"
        );
        let rule = "\u{2500}".repeat(cols);
        // Bypass: at rest, no light is drawn.
        let rows: Vec<String> = (0..pane_rows - 4)
            .map(|_| String::new())
            .chain([
                rule.clone(),
                "\u{276F} ".to_owned(),
                rule,
                "  \u{23F5}\u{23F5} bypass permissions on".to_owned(),
            ])
            .collect();
        feed_rows(&app, session, &rows);
        frame(&mut app, wid);
        let strip = strip(&app, wid);
        let before = app.windows[&wid].input_scratch.cells[strip + last - 1].clone();
        let mode_before = app.windows[&wid].input_scratch.cells[strip + last].clone();
        let deep = FooterFacts {
            path: Some("~/src/github.com/someone/aterm".into()),
            ..facts()
        };
        publish(&app, session, deep);
        frame(&mut app, wid);
        let row = &app.windows[&wid].input_scratch.cells[strip + last - 1];
        let text = text_of(&row[..cols]);
        assert!(
            text.ends_with(
                " \u{25C6} Opus 5.5 xhigh   \u{2302} \u{2026}/aterm   \u{2387} main \u{2500}"
            ),
            "{text:?}"
        );
        assert_eq!(&row[cols..], &before[cols..], "the sibling's cells");
        assert_eq!(
            app.windows[&wid].input_scratch.cells[strip + last],
            mode_before,
            "Claude's row"
        );
    }

    /// The launch card fills only what nothing since the start named; a transcript's word
    /// outranks it. Controls: another build's card; a new owner drops the kept card; the kept
    /// card outlives scrolling away; noting it repaints.
    #[test]
    fn a_launch_card_fills_only_an_open_footer() {
        let (mut app, wid, session) = glass(80);
        let owner = footer::FactsOwner {
            floor: Some(1_790_607_848),
            session_id: "00000000-0000-4000-8000-000000000004".into(),
        };
        let open = FooterFacts {
            path: Some("~/aterm".into()),
            version: Some("2.1.283".into()),
            model_open: true,
            effort_open: true,
            owner: Some(owner.clone()),
            ..FooterFacts::default()
        };
        let rows = claude_rows("footer-bypass-idle", 80);
        let bottom = aterm_phase::phase::composer_bottom(&rows).unwrap();
        // Its card is its drawer's own: a 2.1.283 image, the card painted
        // after its cut (card engine §3.8).
        super::probe::drawer(&app, session, 4242, "claude", "2.1.283");
        feed_rows(&app, session, &rows);
        publish(&app, session, open.clone());
        let plan = app.active_visible_leaf_plan(wid).unwrap();
        let fp_before = app.claude_footer_fp(wid, &plan);
        frame(&mut app, wid);
        let strip = strip(&app, wid);
        let rule = |app: &App| text_of(&app.windows[&wid].input_scratch.cells[strip + bottom]);
        assert!(
            rule(&app).ends_with(" \u{25C6} Opus 5.5 xhigh   \u{2302} ~/aterm \u{2500}"),
            "{:?}",
            rule(&app)
        );
        assert_ne!(
            app.claude_footer_fp(wid, &plan),
            fp_before,
            "noting it repaints"
        );
        // The card scrolls away; the kept card still speaks.
        let mut scrolled = rows.clone();
        for r in scrolled.iter_mut().take(5) {
            r.clear();
        }
        feed_rows(&app, session, &scrolled);
        frame(&mut app, wid);
        assert!(
            rule(&app).contains("\u{25C6} Opus 5.5 xhigh"),
            "{:?}",
            rule(&app)
        );
        // A new owner drops it.
        let entry = app.pool.get(session).unwrap();
        {
            let mut timeline = entry.ctx.timeline.lock().unwrap();
            assert!(timeline.set_claude_footer(
                4242,
                None,
                Some(FooterFacts {
                    owner: Some(footer::FactsOwner {
                        floor: Some(1_790_607_999),
                        ..owner.clone()
                    }),
                    ..open.clone()
                })
            ));
            assert!(timeline.claude_card().is_none());
        }
        frame(&mut app, wid);
        assert!(!rule(&app).contains('\u{25C6}'), "{:?}", rule(&app));
        // A transcript's choice outranks the card.
        feed_rows(&app, session, &rows);
        publish(
            &app,
            session,
            FooterFacts {
                model: Some("Fable 5.1".into()),
                model_open: false,
                ..open.clone()
            },
        );
        frame(&mut app, wid);
        assert!(
            rule(&app).contains("\u{25C6} Fable 5.1 xhigh"),
            "{:?}",
            rule(&app)
        );
        // A card of another build is not this process's: a 2.1.284 image
        // under the 2.1.283 card. Its facts name no build, so the fill's own
        // version filter (`filled_model_effort`) would pass the card: the
        // gate alone refuses it.
        let (mut app, wid, session) = glass(80);
        super::probe::drawer(&app, session, 4242, "claude", "2.1.284");
        feed_rows(&app, session, &rows);
        publish(
            &app,
            session,
            FooterFacts {
                version: None,
                ..open
            },
        );
        frame(&mut app, wid);
        let text = text_of(&app.windows[&wid].input_scratch.cells[strip + bottom]);
        assert!(!text.contains("Opus"), "{text:?}");
        let entry = app.pool.get(session).unwrap();
        assert!(
            entry.ctx.timeline.lock().unwrap().claude_card().is_none(),
            "not kept"
        );
    }

    /// A CARD QUOTED UNDER THE DRAWER'S OWN IS NOT KEPT (card engine §3.8:
    /// the footer keeps only the reading the gate admitted): a tool's output
    /// quoting a launch card — another build's, or the very build's — sits
    /// under the drawer's own card, where the footer's reader takes the last
    /// row naming a banner. Its banner row is not the card the gate admitted,
    /// so the drawer's own card stays kept and the footer keeps its model and
    /// effort. CONTROL: the drawer's own card is kept first. Mutation-checked:
    /// a reading held to the verdict alone, not to its banner row, keeps the
    /// quote.
    #[test]
    fn a_card_quoted_under_the_drawers_own_is_not_kept() {
        for quoted in ["2.1.200", "2.1.283"] {
            let (mut app, wid, session) = glass(80);
            let open = FooterFacts {
                path: Some("~/aterm".into()),
                version: Some("2.1.283".into()),
                model_open: true,
                effort_open: true,
                owner: Some(footer::FactsOwner {
                    floor: Some(1_790_607_848),
                    session_id: "00000000-0000-4000-8000-000000000006".into(),
                }),
                ..FooterFacts::default()
            };
            let rows = claude_rows("footer-bypass-idle", 80);
            let bottom = aterm_phase::phase::composer_bottom(&rows).unwrap();
            super::probe::drawer(&app, session, 4242, "claude", "2.1.283");
            feed_rows(&app, session, &rows);
            publish(&app, session, open);
            frame(&mut app, wid);
            let strip = strip(&app, wid);
            let rule = |app: &App| text_of(&app.windows[&wid].input_scratch.cells[strip + bottom]);
            assert!(
                rule(&app).contains("\u{25C6} Opus 5.5 xhigh"),
                "CONTROL: the drawer's own card: {:?}",
                rule(&app)
            );
            let mut quoting = rows.clone();
            quoting[10] = format!(
                "  \u{23BF}  \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}   Claude Code \
                 v{quoted}"
            );
            quoting[11] = "     Haiku 4.5 \u{00B7} API Usage Billing".into();
            feed_rows(&app, session, &quoting);
            frame(&mut app, wid);
            let read = app.windows[&wid]
                .claude_footer_texts
                .borrow()
                .footer_card(0, session)
                .map(|(_, read)| (read.banner, read.card.model.clone()));
            assert_eq!(
                read,
                Some((Some(10), "Haiku 4.5".to_string())),
                "{quoted}: the footer's reader takes the quote"
            );
            assert!(
                rule(&app).contains("\u{25C6} Opus 5.5 xhigh"),
                "{quoted}: the quote is not kept: {:?}",
                rule(&app)
            );
            let entry = app.pool.get(session).unwrap();
            let kept = entry.ctx.timeline.lock().unwrap().claude_card().cloned();
            assert_eq!(
                kept.map(|card| (card.version, card.model)),
                Some(("2.1.283".to_string(), "Opus 5.5".to_string())),
                "{quoted}"
            );
        }
    }

    /// THE NEWEST CARD WINS, AND ONLY THE DRAWER'S: while the facts stay
    /// open the card is read on every rebuild of the pane's text, and a
    /// newer reading of the drawer's own replaces the kept one. A
    /// predecessor's card of the same build, still on the glass between the
    /// new owner and the new process's first paint, is not the drawer's
    /// (card engine §3.8: the gate's one verdict serves the footer's kept
    /// card) and is never kept; the new process's own card is, the moment
    /// it draws it. A frame with no card to read (it scrolled away) keeps
    /// the newest reading. CONTROL: the same predecessor's card painted
    /// after the drawer's cut — the drawer's own — is kept, so the gate,
    /// not the reader, refuses it.
    #[test]
    fn a_newer_launch_card_replaces_the_kept_one() {
        let open = FooterFacts {
            path: Some("~/aterm".into()),
            version: Some("2.1.283".into()),
            model_open: true,
            effort_open: true,
            owner: Some(footer::FactsOwner {
                floor: Some(1_790_607_848),
                session_id: "00000000-0000-4000-8000-000000000005".into(),
            }),
            ..FooterFacts::default()
        };
        let rows = claude_rows("footer-bypass-idle", 80);
        let bottom = aterm_phase::phase::composer_bottom(&rows).unwrap();
        // The predecessor's card: `Haiku 4.5`, no effort.
        let stale: Vec<String> = rows
            .iter()
            .map(|r| r.replace("Opus 5.5 with xhigh effort", "Haiku 4.5"))
            .collect();
        assert_ne!(stale, rows, "the fixture's card names Opus 5.5");
        let (mut app, wid, session) = glass(80);
        // Still on the glass when the new process takes the front.
        feed_rows(&app, session, &stale);
        super::probe::drawer(&app, session, 4242, "claude", "2.1.283");
        publish(&app, session, open.clone());
        frame(&mut app, wid);
        let strip = strip(&app, wid);
        let rule = |app: &App| text_of(&app.windows[&wid].input_scratch.cells[strip + bottom]);
        assert!(
            !rule(&app).contains('\u{25C6}'),
            "the predecessor's card is not kept: {:?}",
            rule(&app)
        );
        // The new process draws its own card: kept.
        feed_rows(&app, session, &rows);
        frame(&mut app, wid);
        assert!(
            rule(&app).contains("\u{25C6} Opus 5.5 xhigh   \u{2302} ~/aterm"),
            "{:?}",
            rule(&app)
        );
        // It draws another (`/model`): the newer reading wins.
        feed_rows(&app, session, &stale);
        frame(&mut app, wid);
        assert!(
            rule(&app).contains("\u{25C6} Haiku 4.5   \u{2302} ~/aterm"),
            "{:?}",
            rule(&app)
        );
        // Its card scrolls away: the newest reading stands.
        let mut scrolled = stale.clone();
        for r in scrolled.iter_mut().take(5) {
            r.clear();
        }
        feed_rows(&app, session, &scrolled);
        frame(&mut app, wid);
        assert!(
            rule(&app).contains("\u{25C6} Haiku 4.5"),
            "{:?}",
            rule(&app)
        );
        // CONTROL: the predecessor's card painted after the cut is kept.
        let (mut app, wid, session) = glass(80);
        super::probe::drawer(&app, session, 4242, "claude", "2.1.283");
        feed_rows(&app, session, &stale);
        publish(&app, session, open);
        frame(&mut app, wid);
        let rule = text_of(&app.windows[&wid].input_scratch.cells[strip + bottom]);
        assert!(
            rule.contains("\u{25C6} Haiku 4.5   \u{2302} ~/aterm"),
            "CONTROL: {rule:?}"
        );
    }

    /// The held move's note leads the rule in the marks' ink, worded for the composer (a dim
    /// suggestion is no draft), never while a turn runs; a look that only restamps the person
    /// owes no frame. Negative controls: no recent look, a move under way, another wait.
    #[test]
    fn a_move_the_person_holds_leads_the_rule_until_it_goes() {
        use aterm_agent::harness::upgrade::Phase;
        use aterm_agent::harness::upgrade_drive::Row;
        let (mut app, wid, session) = glass(150);
        let rows = claude_rows("footer-bypass-idle", 150);
        let bottom = aterm_phase::phase::composer_bottom(&rows).unwrap();
        let caret = rows
            .iter()
            .rposition(|r| r.starts_with('\u{276F}'))
            .unwrap();
        // The caret row as Claude Code draws it, the cursor where the
        // fixture's is: its suggestion dim, a person's draft not.
        let compose = |app: &App, text: &str, dim: bool| {
            let sgr = if dim { "\x1b[2m" } else { "" };
            let row = caret + 1;
            let out = format!("\x1b[{row};1H\x1b[2K\u{276F} {sgr}{text}\x1b[0m\x1b[{row};3H");
            let term = app.pool.get(session).expect("session").term.clone();
            crate::term_lock(&term).process(out.as_bytes());
        };
        let suggestion = "Try \"refactor lib.rs\"";
        feed_rows(&app, session, &rows);
        compose(&app, suggestion, true);
        publish(&app, session, facts());
        let tab = {
            let store = app.store.read().unwrap_or_else(|p| p.into_inner());
            store
                .by_local(session)
                .expect("the front session")
                .sid
                .as_str()
                .to_string()
        };
        let now = crate::upgrade_host::now_s();
        let held = Row {
            session: "conv".into(),
            tab,
            from: "2.1.285".into(),
            to: "2.1.286".into(),
            source: "managed".into(),
            phase: Phase::Announced {
                at_s: now - 600,
                asks: 1,
            },
            behind_since: now - (5 * 3_600 + 38 * 60),
            wait: "attended".into(),
            wait_since: now - 20,
            wait_seen: now - 5,
            person_named: true,
            ready: true,
            ..Row::default()
        };
        let note = held.pause_note(now).expect("held by the person: noted");
        let using = note.using.as_ref().expect("an empty composer's words");
        let plan = app.active_visible_leaf_plan(wid).unwrap();
        let fp_before = app.claude_footer_fp(wid, &plan);
        app.apply_agent_upgrades(vec![held.clone()]);
        let fp_held = app.claude_footer_fp(wid, &plan);
        assert_ne!(fp_held, fp_before, "the note repaints");
        frame(&mut app, wid);
        let strip = strip(&app, wid);
        let rule = |app: &App| app.windows[&wid].input_scratch.cells[strip + bottom].clone();
        let facts_text = "\u{25C6} Opus 5.5 xhigh   \u{2302} ~/aterm   \u{2387} main \u{2500}";
        let led = |words: &str| format!(" \u{21E3} {words}   {facts_text}");
        let text = text_of(&rule(&app));
        assert!(text.ends_with(&led(&using.whole)), "{text:?}");
        let cells = rule(&app);
        let at = cells.iter().position(|c| c.ch == '\u{21E3}').unwrap();
        // The note's mark is one of the rule's marks: the same ink as the
        // model's, not the values' gray.
        let model_mark = cells.iter().position(|c| c.ch == '\u{25C6}').unwrap();
        assert_eq!(cells[at].fg, cells[model_mark].fg, "the note's mark");
        let word = at + 1 + cells[at + 1..].iter().position(|c| c.ch != ' ').unwrap();
        assert_ne!(cells[at].fg, cells[word].fg, "a mark, not a value");
        // A look that only restamps the person: no frame owed.
        app.apply_agent_upgrades(vec![Row {
            wait_seen: now,
            ..held.clone()
        }]);
        assert_eq!(app.claude_footer_fp(wid, &plan), fp_held, "no churn");
        // A person's draft in the composer: the draft's words — and the same
        // through the look that finds them paused with it.
        compose(&app, "fix the build", false);
        frame(&mut app, wid);
        let text = text_of(&rule(&app));
        assert!(text.ends_with(&led(&note.draft.whole)), "typing: {text:?}");
        let paused = Row {
            wait: "draft".into(),
            wait_seen: 0,
            person_named: false,
            ..held.clone()
        };
        app.apply_agent_upgrades(vec![paused.clone()]);
        frame(&mut app, wid);
        let text = text_of(&rule(&app));
        assert!(text.ends_with(&led(&note.draft.whole)), "paused: {text:?}");
        // The draft sent or cleared: nothing for an empty composer until the
        // next look says what holds the move.
        for (why, text, dim) in [("cleared", "", false), ("its suggestion", suggestion, true)] {
            compose(&app, text, dim);
            frame(&mut app, wid);
            let text = text_of(&rule(&app));
            assert!(!text.contains('\u{21E3}'), "{why}: {text:?}");
            assert!(text.ends_with(&format!(" {facts_text}")), "{why}: {text:?}");
        }
        // A turn running holds the move, whatever the row says of the person.
        let busy = claude_rows("footer-busy", 150);
        let busy_bottom = aterm_phase::phase::composer_bottom(&busy).unwrap();
        feed_rows(&app, session, &busy);
        app.apply_agent_upgrades(vec![held.clone()]);
        frame(&mut app, wid);
        let text = text_of(&app.windows[&wid].input_scratch.cells[strip + busy_bottom]);
        assert!(!text.contains('\u{21E3}'), "a turn runs: {text:?}");
        assert!(text.contains(facts_text), "a turn runs: {text:?}");
        feed_rows(&app, session, &rows);
        compose(&app, suggestion, true);
        // Negative controls: each takes the note down, the facts staying.
        for (why, gone) in [
            (
                "no look has seen the person lately",
                Row {
                    person_named: false,
                    ..held.clone()
                },
            ),
            (
                "the move is under way",
                Row {
                    phase: Phase::Exiting { at_s: now },
                    ..held.clone()
                },
            ),
            (
                "its own work holds it",
                Row {
                    wait: "not-idle:busy".into(),
                    ..held.clone()
                },
            ),
        ] {
            app.apply_agent_upgrades(vec![held.clone()]);
            frame(&mut app, wid);
            assert!(text_of(&rule(&app)).contains('\u{21E3}'), "{why}: held");
            app.apply_agent_upgrades(vec![gone]);
            assert_ne!(app.claude_footer_fp(wid, &plan), fp_held, "{why}: repaints");
            frame(&mut app, wid);
            let text = text_of(&rule(&app));
            assert!(!text.contains('\u{21E3}'), "{why}: {text:?}");
            assert!(text.ends_with(&format!(" {facts_text}")), "{why}: {text:?}");
        }
    }

    #[test]
    fn no_facts_keys_like_no_footer() {
        let none = FactMask::default();
        assert_eq!(fingerprint(None, None, true, none), 0);
        assert_ne!(fingerprint(Some(&facts()), None, false, none), 0);
        let mut other = facts();
        other.branch = Some("feature".into());
        assert_ne!(
            fingerprint(Some(&facts()), None, true, none),
            fingerprint(Some(&other), None, true, none)
        );
        // A fact the person hid keys apart: its `off` is a repaint.
        assert_ne!(
            fingerprint(Some(&facts()), None, true, none),
            fingerprint(
                Some(&facts()),
                None,
                true,
                none.with(FactKind::Branch, true)
            )
        );
    }

    /// Facts that differ only in the account key differently, with the usage switch on or
    /// off, and publish as a move. Control: equal accounts key equal.
    #[test]
    fn the_account_moves_the_repaint_key() {
        let on = |label: &str| FooterFacts {
            account: Some(label.into()),
            ..facts()
        };
        for usage in [true, false] {
            let key =
                |facts: &FooterFacts| fingerprint(Some(facts), None, usage, FactMask::default());
            assert_ne!(key(&on("main")), key(&on("work-2")), "another account");
            assert_ne!(key(&on("main")), key(&facts()), "one where there was none");
            assert_eq!(key(&on("main")), key(&on("main")), "the same account");
        }
        let (app, wid, session) = glass(80);
        let plan = app.active_visible_leaf_plan(wid).unwrap();
        // `publish` holds the timeline to answering that the facts moved.
        publish(&app, session, on("main"));
        let main = app.claude_footer_fp(wid, &plan);
        publish(&app, session, on("work-2"));
        assert_ne!(app.claude_footer_fp(wid, &plan), main);
    }

    /// A PTY update between grid extraction and host chrome must not move the
    /// footer onto a newer viewport or recolour it from newer OSC defaults.
    #[test]
    fn footer_uses_the_cell_frames_viewport_and_blank_after_terminal_moves() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let plan = app.active_visible_leaf_plan(wid).unwrap();
        let front = app.front_terminal(wid).unwrap().clone();
        let (rows, cols) = {
            let ws = &app.windows[&wid];
            (usize::from(ws.rows), usize::from(ws.cols))
        };
        {
            let entry = app.pool.get(front.session).unwrap();
            let mut timeline = entry.ctx.timeline.lock().unwrap();
            timeline.note_foreground_group(42);
            assert!(timeline.set_claude_footer(42, None, Some(facts())));
        }
        {
            let mut term = crate::term_lock(&front.term);
            term.resize(rows as u16, cols as u16);
            term.set_default_foreground(aterm_core::terminal::Rgb {
                r: 10,
                g: 20,
                b: 30,
            });
            term.set_default_background(aterm_core::terminal::Rgb {
                r: 40,
                g: 50,
                b: 60,
            });
            for _ in 0..rows + 4 {
                term.process(b"history\r\n");
            }
            let ws = app.windows.get_mut(&wid).unwrap();
            term.cell_frame_into(&mut ws.input_scratch, rows, cols);
            vendor_frame(&mut ws.input_scratch);
        }
        let extracted_blank = app.windows[&wid].input_scratch.implicit_blank;
        assert_eq!(extracted_blank.fg, [10, 20, 30]);
        assert_eq!(extracted_blank.bg, [40, 50, 60]);
        assert_eq!(app.windows[&wid].input_scratch.display_offset, 0);
        {
            let mut term = crate::term_lock(&front.term);
            term.process(b"\x1b]10;rgb:aa/bb/cc\x1b\\\x1b]11;rgb:dd/ee/ff\x1b\\");
            term.scroll_display(1);
            assert!(term.grid().display_offset() > 0);
            assert_ne!(term.implicit_blank_render_cell(), extracted_blank);
        }
        app.splice_claude_footer(
            wid,
            &plan,
            VisibleContentRoute::Terminal { composed: false },
        );
        let footer = &app.windows[&wid].input_scratch.cells[rows - 2];
        let mark = footer
            .iter()
            .position(|c| c.ch == '\u{25C6}')
            .expect("the extracted live frame still paints");
        assert_eq!(footer[mark + 2].bg, extracted_blank.bg);

        // A second paint of the retained scratch first undoes the footer and then reads
        // the same vendor frame: it reuses that frame's text and still paints and observes
        // the footer again.
        let text_before = Arc::clone(
            &app.windows[&wid].claude_footer_texts.borrow().panes[0]
                .as_ref()
                .unwrap()
                .rows,
        );
        app.splice_claude_footer(
            wid,
            &plan,
            VisibleContentRoute::Terminal { composed: false },
        );
        let text_after = Arc::clone(
            &app.windows[&wid].claude_footer_texts.borrow().panes[0]
                .as_ref()
                .unwrap()
                .rows,
        );
        assert!(Arc::ptr_eq(&text_before, &text_after));
        assert_eq!(
            app.windows[&wid].input_scratch.cells[rows - 2][mark].ch,
            '\u{25C6}'
        );

        // Conversely, a retained history frame stays history even if the
        // terminal has already returned to the live tail before this splice.
        {
            let mut term = crate::term_lock(&front.term);
            let ws = app.windows.get_mut(&wid).unwrap();
            term.cell_frame_into(&mut ws.input_scratch, rows, cols);
            vendor_frame(&mut ws.input_scratch);
            assert!(ws.input_scratch.display_offset > 0);
            term.scroll_display(-1);
            assert_eq!(term.grid().display_offset(), 0);
        }
        app.splice_claude_footer(
            wid,
            &plan,
            VisibleContentRoute::Terminal { composed: false },
        );
        let row = &app.windows[&wid].input_scratch.cells[rows - 2];
        assert!(
            row.iter().all(|c| c.ch == '\u{2500}'),
            "history frame keeps the vendor's rule"
        );
    }

    #[test]
    fn footer_source_uses_leaf_index_for_splits_and_view_for_mixed_tabs() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let mut plan = app.active_visible_leaf_plan(wid).unwrap();
        let leaf = plan.leaves.remove(0);
        let mut sibling = leaf.clone();
        sibling.focused = false;
        let ws = app.windows.get_mut(&wid).unwrap();
        ws.input_scratch.terminal_id = 11;
        ws.composed_focus_scratch.terminal_id = 22;
        ws.unfocused_pane_scratch.insert(1, RenderInput::empty());
        ws.unfocused_pane_scratch.get_mut(&1).unwrap().terminal_id = 33;
        ws.leaf_render_cache
            .entry(leaf.view)
            .or_default()
            .input
            .terminal_id = 44;

        let id =
            |route: VisibleContentRoute, index: usize, leaf: &crate::tab_model::VisibleLeaf| {
                frame_source(ws, route, index, leaf).map(|s| s.terminal_id)
            };
        assert_eq!(
            id(VisibleContentRoute::Terminal { composed: false }, 0, &leaf),
            Some(11)
        );
        assert_eq!(
            id(VisibleContentRoute::Terminal { composed: true }, 0, &leaf),
            Some(22)
        );
        assert_eq!(
            id(
                VisibleContentRoute::Terminal { composed: true },
                1,
                &sibling
            ),
            Some(33)
        );
        assert_eq!(id(VisibleContentRoute::Heterogeneous, 0, &leaf), Some(44));
        assert_eq!(
            id(
                VisibleContentRoute::Terminal { composed: true },
                2,
                &sibling
            ),
            None
        );
    }

    #[test]
    fn agy_launch_card_and_composer_footer_render_on_the_glass() {
        let (mut app, wid, session) = glass(120);
        {
            let entry = app.pool.get(session).expect("session");
            let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            timeline.note_foreground_group(4242);
            timeline.set_program(4242, Some("agy".into()));
            assert!(timeline.set_claude_footer(
                4242,
                None,
                Some(FooterFacts {
                    model_open: true,
                    effort_open: true,
                    path: Some("~/aterm".into()),
                    branch: Some("main".into()),
                    owner: Some(footer::FactsOwner {
                        floor: None,
                        session_id: "agy-4242".into(),
                    }),
                    ..FooterFacts::default()
                }),
            ));
        }
        let host = crate::harness_host::HostHandle::for_supervised_test(
            Vec::new(),
            std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
        );
        app.harness = Some(host.clone());
        let ctx = app.pool.get(session).expect("session").ctx.clone();
        crate::session_timeline::claim_supervisor(
            &ctx,
            &crate::harness_host::holder(),
            None,
            None,
            crate::metrics::now_us(),
        )
        .expect("claim");

        let rule = "\u{2500}".repeat(120);
        let rows = vec![
            String::new(),
            "      \u{2584}\u{2580}\u{2580}\u{2584}        Antigravity CLI 1.4.0".into(),
            "     \u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}       redacted-user@example.test (Google AI Ultra)".into(),
            "    \u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}      Gemini 4 Argon (Medium)".into(),
            "   \u{2584}\u{2580}\u{2580}    \u{2580}\u{2580}\u{2584}     ~/aterm".into(),
            "  \u{2584}\u{2580}\u{2580}      \u{2580}\u{2580}\u{2584}".into(),
            String::new(),
            rule.clone(),
            "> ".into(),
            rule,
            "? for shortcuts                                                                                  Gemini 4 Argon \u{00B7} medium".into(),
        ];
        // agy's card is its drawer's own (ProvenanceOnly: the fence alone).
        super::probe::drawer(&app, session, 4242, "agy", "1.4.2");
        feed_rows(&app, session, &rows);
        frame(&mut app, wid);

        let strip = strip(&app, wid);
        let card_line = text_of(&app.windows[&wid].input_scratch.cells[strip + 5]);
        assert!(
            card_line.contains("enhanced by aterm harness")
                && card_line.contains("\u{00B7} agy \u{00B7}")
                && card_line.contains("autopilot"),
            "agy card line on row top+4: {card_line:?}"
        );
        assert!(
            app.windows[&wid]
                .claude_card
                .painted_layers()
                .iter()
                .any(|l| l.slot == aterm_core::render::FxSlot::Under
                    && !l.content.items().is_empty()),
            "agy card rainbow border and trail drawn"
        );
        assert!(
            app.windows[&wid]
                .claude_card
                .painted_layers()
                .iter()
                .any(
                    |l| l.slot == aterm_core::render::FxSlot::Over && !l.content.items().is_empty()
                ),
            "agy card kitty and stars drawn"
        );
        let top_rule = text_of(&app.windows[&wid].input_scratch.cells[strip + 7]);
        assert!(
            top_rule.starts_with("\u{2500}\u{2500}    agy \u{2500}")
                && top_rule.contains(" alab harness \u{25CF} on "),
            "agy top rule has its lockup and the harness switch: {top_rule:?}"
        );
        assert!(
            app.windows[&wid]
                .claude_card
                .painted_layers()
                .iter()
                .any(|l| l.id == top_mark_id(session) && !l.content.items().is_empty()),
            "agy's icon drawn over its lockup's cells"
        );
        let bottom_rule = text_of(&app.windows[&wid].input_scratch.cells[strip + 9]);
        assert!(
            bottom_rule.contains('\u{221E}'),
            "agy composer rule has the autopilot chip: {bottom_rule:?}"
        );
        assert!(
            bottom_rule
                .contains("\u{25C6} Gemini 4 Argon medium   \u{2302} ~/aterm   \u{2387} main"),
            "agy composer rule has model, effort, path, and branch: {bottom_rule:?}"
        );
        host.shutdown_and_join();
    }

    /// AGY'S MODEL FOLLOWS ITS OWN STATUS ROW (card engine §3.8): agy's
    /// facts leave the model open, and agy's own status row under its
    /// composer names the model it runs on. Once its card has scrolled away,
    /// a model switch shows in aterm's footer from that row alone — a row
    /// the drawer drew, inside its fence. CONTROL: a predecessor's agy
    /// screen, there before the drawer took the front, names no model the
    /// footer keeps. Mutation-checked: a status row not held to the fence
    /// keeps the predecessor's model.
    #[test]
    fn agy_follows_its_own_status_row_once_its_card_is_gone() {
        let rule = "\u{2500}".repeat(120);
        let screen = |card: bool, model: &str| -> Vec<String> {
            let mut rows = vec![String::new()];
            if card {
                rows.extend([
                    "      \u{2584}\u{2580}\u{2580}\u{2584}        Antigravity CLI 1.4.0".to_string(),
                    "     \u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}       redacted-user@example.test (Google AI Ultra)".to_string(),
                    "    \u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}      Gemini 4 Argon (Medium)".to_string(),
                    "   \u{2584}\u{2580}\u{2580}    \u{2580}\u{2580}\u{2584}     ~/aterm".to_string(),
                    "  \u{2584}\u{2580}\u{2580}      \u{2580}\u{2580}\u{2584}".to_string(),
                ]);
            } else {
                rows.extend([
                    "\u{25CF} Switched the model.".to_string(),
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                ]);
            }
            rows.extend([
                String::new(),
                rule.clone(),
                "> ".into(),
                rule.clone(),
                format!("? for shortcuts{}{model}", " ".repeat(60)),
            ]);
            rows
        };
        let agy_facts = |app: &App, session: u64| {
            let entry = app.pool.get(session).expect("session");
            let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            timeline.note_foreground_group(4242);
            timeline.set_program(4242, Some("agy".into()));
            assert!(timeline.set_claude_footer(
                4242,
                None,
                Some(FooterFacts {
                    model_open: true,
                    effort_open: true,
                    path: Some("~/aterm".into()),
                    owner: Some(footer::FactsOwner {
                        floor: None,
                        session_id: "agy-4242".into(),
                    }),
                    ..FooterFacts::default()
                }),
            ));
        };
        let bottom = |app: &App, wid: WindowId| {
            text_of(&app.windows[&wid].input_scratch.cells[strip(app, wid) + 9])
        };
        let (mut app, wid, session) = glass(120);
        agy_facts(&app, session);
        super::probe::drawer(&app, session, 4242, "agy", "1.4.2");
        feed_rows(
            &app,
            session,
            &screen(true, "Gemini 4 Argon \u{00B7} medium"),
        );
        frame(&mut app, wid);
        assert!(
            bottom(&app, wid).contains("\u{25C6} Gemini 4 Argon medium"),
            "{:?}",
            bottom(&app, wid)
        );
        // The card scrolled away; agy switched its model.
        feed_rows(&app, session, &screen(false, "Gemini 4 Pro \u{00B7} high"));
        frame(&mut app, wid);
        assert!(
            bottom(&app, wid).contains("\u{25C6} Gemini 4 Pro high"),
            "its own status row: {:?}",
            bottom(&app, wid)
        );
        // CONTROL: a predecessor's screen, there before the drawer's cut.
        let (mut app, wid, session) = glass(120);
        agy_facts(&app, session);
        feed_rows(&app, session, &screen(false, "Gemini 4 Pro \u{00B7} high"));
        super::probe::drawer(&app, session, 4242, "agy", "1.4.2");
        frame(&mut app, wid);
        assert!(
            !bottom(&app, wid).contains("Gemini 4 Pro"),
            "CONTROL: not the drawer's row: {:?}",
            bottom(&app, wid)
        );
    }

    #[test]
    fn muse_launch_card_and_composer_footer_render_on_the_glass() {
        let (mut app, wid, session) = glass(120);
        {
            let entry = app.pool.get(session).expect("session");
            let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            timeline.note_foreground_group(5151);
            timeline.set_program(5151, Some("muse".into()));
            assert!(timeline.set_claude_footer(
                5151,
                None,
                Some(FooterFacts {
                    model_open: true,
                    effort_open: true,
                    path: Some("~/aterm".into()),
                    branch: Some("main".into()),
                    owner: Some(footer::FactsOwner {
                        floor: None,
                        session_id: "muse-5151".into(),
                    }),
                    ..FooterFacts::default()
                }),
            ));
        }
        let host = crate::harness_host::HostHandle::for_supervised_test(
            Vec::new(),
            std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
        );
        app.harness = Some(host.clone());
        let ctx = app.pool.get(session).expect("session").ctx.clone();
        crate::session_timeline::claim_supervisor(
            &ctx,
            &crate::harness_host::holder(),
            None,
            None,
            crate::metrics::now_us(),
        )
        .expect("claim");

        let rule = "\u{2500}".repeat(120);
        let rows = vec![
            String::new(),
            "  Muse Code 0.9.3".into(),
            String::new(),
            rule.clone(),
            "\u{276F} ".into(),
            rule,
            "  gpt-5.4 \u{00B7} high \u{00B7} ~/aterm \u{00B7} Auto-review".into(),
        ];
        // Muse Code's card is its drawer's own (ProvenanceOnly).
        super::probe::drawer(&app, session, 5151, "muse", "0.9.3");
        feed_rows(&app, session, &rows);
        frame(&mut app, wid);

        let strip = strip(&app, wid);
        let card_line = text_of(&app.windows[&wid].input_scratch.cells[strip + 2]);
        assert!(
            card_line.contains("enhanced by aterm harness")
                && card_line.contains("\u{00B7} muse \u{00B7}")
                && card_line.contains("autopilot"),
            "muse card line on row top+1: {card_line:?}"
        );
        assert!(
            app.windows[&wid]
                .claude_card
                .painted_layers()
                .iter()
                .any(|l| l.slot == aterm_core::render::FxSlot::Under
                    && !l.content.items().is_empty()),
            "muse card rainbow border and trail drawn"
        );
        assert!(
            app.windows[&wid]
                .claude_card
                .painted_layers()
                .iter()
                .any(
                    |l| l.slot == aterm_core::render::FxSlot::Over && !l.content.items().is_empty()
                ),
            "muse card kitty and stars drawn"
        );
        let top_rule = text_of(&app.windows[&wid].input_scratch.cells[strip + 3]);
        assert!(
            top_rule.starts_with("\u{2500}\u{2500}    muse \u{2500}")
                && top_rule.contains(" alab harness \u{25CF} on "),
            "muse top rule has its lockup and the harness switch: {top_rule:?}"
        );
        assert!(
            app.windows[&wid]
                .claude_card
                .painted_layers()
                .iter()
                .any(|l| l.id == top_mark_id(session) && !l.content.items().is_empty()),
            "muse's icon drawn over its lockup's cells"
        );
        let bottom_rule = text_of(&app.windows[&wid].input_scratch.cells[strip + 5]);
        assert!(
            bottom_rule.contains('\u{221E}'),
            "muse composer rule has the autopilot chip: {bottom_rule:?}"
        );
        assert!(
            bottom_rule.contains("\u{25C6} gpt-5.4 high   \u{2302} ~/aterm   \u{2387} main"),
            "muse composer rule has model, effort, path, and branch: {bottom_rule:?}"
        );
        host.shutdown_and_join();
    }

    /// MUSE CODE'S SIGNED-IN CARDS POPULATE THE FOOTER (owner, 2026-10-21;
    /// measured on 1.4.11 signed in): the login store has given the
    /// account address, and a `/usage` card alone over the composer fills
    /// its tier and `weekly 29% left` beside it — `/status` need not be
    /// opened first — while the top rule's lockup stands where its idle
    /// `Voice input` label was. CONTROL: the same pane before any card
    /// shows the address with `weekly unavailable` and keeps it.
    #[test]
    fn muse_signed_in_cards_populate_the_footer() {
        let (mut app, wid, session) = glass(160);
        const RESET: i64 = 1_792_947_600;
        let now = std::cell::Cell::new(RESET - 1);
        let clock_reads = std::cell::Cell::new(0usize);
        let clock = || {
            clock_reads.set(clock_reads.get() + 1);
            footer::ReadClock {
                read_at: std::time::UNIX_EPOCH,
                now: now.get(),
                offset_at: &|_, _| Some(0),
            }
        };
        let frame = |app: &mut App, wid| frame_with_clock(app, wid, Some(&clock));
        {
            let entry = app.pool.get(session).expect("session");
            let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            timeline.note_foreground_group(6161);
            timeline.set_program(6161, Some("muse".into()));
            assert!(timeline.set_claude_footer(
                6161,
                None,
                Some(FooterFacts {
                    model_open: true,
                    effort_open: true,
                    path: Some("~/aterm".into()),
                    branch: Some("main".into()),
                    owner: Some(footer::FactsOwner {
                        floor: None,
                        session_id: "muse-6161".into(),
                    }),
                    subscription: Some(aterm_agent::harness::profile::SubscriptionDisplay {
                        email: Some("redacted-user@example.test".into()),
                        ..Default::default()
                    },),
                    ..FooterFacts::default()
                }),
            ));
            assert!(!timeline.muse_cards_need_account());
        }
        let host = crate::harness_host::HostHandle::for_supervised_test(
            Vec::new(),
            std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
        );
        app.harness = Some(host.clone());
        let ctx = app.pool.get(session).expect("session").ctx.clone();
        crate::session_timeline::claim_supervisor(
            &ctx,
            &crate::harness_host::holder(),
            None,
            None,
            crate::metrics::now_us(),
        )
        .expect("claim");

        let rule = "\u{2500}".repeat(160);
        let labelled = format!(
            "\u{2500}\u{2500} Voice input (\u{2325} + v to start) {}",
            "\u{2500}".repeat(128)
        );
        let footer_row = "  gpt-5.4 \u{00B7} high \u{00B7} ~/aterm \u{00B7} Auto-review";
        let bare = vec![
            String::new(),
            "  Muse Code 0.9.3".into(),
            String::new(),
            labelled.clone(),
            "\u{276F} ".into(),
            rule.clone(),
            footer_row.into(),
        ];
        feed_rows(&app, session, &bare);
        frame(&mut app, wid);
        let strip = strip(&app, wid);
        let bottom = text_of(&app.windows[&wid].input_scratch.cells[strip + 5]);
        assert!(
            bottom.contains("redacted-user@example.test") && bottom.contains("weekly unavailable"),
            "CONTROL: no card shown yet: {bottom:?}"
        );

        let mut carded = vec![
            String::new(),
            "  Muse Code 0.9.3".into(),
            "\u{250C}".to_owned() + &"\u{2500}".repeat(55) + "\u{2510}",
            "\u{2502}  Session usage                                        \u{2502}".into(),
            "\u{2502}  Subscription \u{00B7} Muse Code Max Usage                   \u{2502}".into(),
            "\u{2502}    Weekly         71% used \u{00B7} Resets Oct 25 at 5:00 PM \u{2502}".into(),
            "\u{2514}".to_owned() + &"\u{2500}".repeat(55) + "\u{2518}",
            labelled.clone(),
            "\u{276F} ".into(),
            rule.clone(),
            footer_row.into(),
        ];
        feed_rows(&app, session, &carded);
        frame(&mut app, wid);
        // The reading is written back for the next compose.
        frame(&mut app, wid);
        let top = text_of(&app.windows[&wid].input_scratch.cells[strip + 7]);
        assert!(
            !top.contains("Voice input") && top.starts_with("\u{2500}\u{2500}    muse \u{2500}"),
            "the lockup at the top rule's left, where its idle label was: {top:?}"
        );
        let bottom = text_of(&app.windows[&wid].input_scratch.cells[strip + 9]);
        assert!(
            bottom.contains("redacted-user@example.test")
                && bottom.contains("Muse Code Max Usage")
                && bottom.contains("weekly 29% left")
                && !bottom.contains("unavailable"),
            "the usage card populates its tier and weekly beside the account: {bottom:?}"
        );
        // Even before a switch, an account-bearing status without its own
        // billing row cannot borrow a separate card's plan. Usage-only
        // bootstrap is distinct from filling a known status card's holes.
        let mut account_only = carded.clone();
        let status = vec![
            "┌".to_owned() + &"─".repeat(83) + "┐",
            format!("│{:<83}│", "  MUSE CODE 1.4.11 / first-session IDLE"),
            format!(
                "│{:<83}│",
                "  ACCOUNT        Redacted User (redacted-user@example.test)"
            ),
            "└".to_owned() + &"─".repeat(83) + "┘",
        ];
        drop(account_only.splice(7..7, status));
        let account_only_bottom = strip + account_only.len() - 2;
        feed_rows(&app, session, &account_only);
        frame(&mut app, wid);
        frame(&mut app, wid);
        let bottom = text_of(&app.windows[&wid].input_scratch.cells[account_only_bottom]);
        assert!(
            bottom.contains("redacted-user@example.test")
                && !bottom.contains("Muse Code Max Usage"),
            "a named status card cannot borrow another card's plan: {bottom:?}"
        );
        feed_rows(&app, session, &carded);
        frame(&mut app, wid);
        frame(&mut app, wid);
        let bottom = text_of(&app.windows[&wid].input_scratch.cells[strip + 9]);
        assert!(
            bottom.contains("Muse Code Max Usage") && bottom.contains("weekly 29% left"),
            "usage-only bootstrap remains available before any switch: {bottom:?}"
        );
        let already_read = clock_reads.get();
        frame(&mut app, wid);
        assert_eq!(
            clock_reads.get(),
            already_read,
            "a settled reset does not read the clock again"
        );
        // The actual resolver expires this SAME occurrence at its bound.
        // All external reads are inert: no registry directory, exec args,
        // roster, home or process-directory probe is supplied.
        now.set(RESET);
        let timeline = &app.pool.get(session).expect("session").ctx.timeline;
        let mut identity = unread(6161, std::path::Path::new("unused"));
        identity.dir = None;
        resolve_facts_at_with_cwd(
            timeline,
            &mut identity,
            &mut footer::TailCache::default(),
            &mut footer::FooterCache::default(),
            &mut RosterMemo::default(),
            (
                Asks {
                    args: |_| None,
                    roster: || None,
                    home: || None,
                    stored_login: |_, _, _| false,
                },
                |_| None,
            ),
            &footer::ReadClock {
                read_at: std::time::UNIX_EPOCH,
                now: now.get(),
                offset_at: &|_, _| Some(0),
            },
        );
        {
            let held = timeline.lock().unwrap();
            let sub = held.claude_footer().unwrap().subscription.as_ref().unwrap();
            assert!(sub.weekly.is_none());
            assert_eq!(
                sub.weekly_reset.as_ref().and_then(|reset| reset.at),
                Some(RESET)
            );
        }
        // No new terminal output: both reusing the pane and rebuilding it
        // must respect the retained expiry, not import the old card again.
        frame(&mut app, wid);
        app.windows
            .get_mut(&wid)
            .unwrap()
            .claude_footer_texts
            .borrow_mut()
            .trim(0);
        frame(&mut app, wid);
        let bottom = text_of(&app.windows[&wid].input_scratch.cells[strip + 9]);
        assert!(
            bottom.contains("redacted-user@example.test")
                && bottom.contains("weekly unavailable")
                && !bottom.contains("weekly 29% left"),
            "an unchanged expired card cannot graft its quota again: {bottom:?}"
        );
        assert_eq!(
            clock_reads.get(),
            already_read,
            "expiry tombstones need no frame clock"
        );
        // A genuinely later reset is a new observation, admitted normally
        // while the account still stands. Its quota must then be revoked
        // by the known switch below, along with the reset occurrence.
        carded[5] = carded[5].replace("Oct 25", "Oct 26");
        feed_rows(&app, session, &carded);
        frame(&mut app, wid);
        frame(&mut app, wid);
        let bottom = text_of(&app.windows[&wid].input_scratch.cells[strip + 9]);
        assert!(
            bottom.contains("weekly 29% left"),
            "a new window is admitted: {bottom:?}"
        );
        // A login-store switch in the same process. Publication must drop
        // unattributed fields even if a caller carries the previous tuple;
        // the first account's cards still on the glass cannot refill them.
        {
            let entry = app.pool.get(session).expect("session");
            let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            let mut facts = timeline.claude_footer().cloned().expect("facts");
            facts.subscription.as_mut().expect("subscription").email =
                Some("second@example.test".into());
            assert!(timeline.set_claude_footer(6161, None, Some(facts)));
            assert!(timeline.muse_cards_need_account());
            let sub = timeline
                .claude_footer()
                .unwrap()
                .subscription
                .as_ref()
                .unwrap();
            assert_eq!(
                (&sub.plan, &sub.weekly, &sub.weekly_reset),
                (&None, &None, &None)
            );
        }
        // The usage-only card is STILL the first account's, even though it
        // has no address for the mismatch guard to compare. No terminal
        // output is needed to expose the account change.
        frame(&mut app, wid);
        frame(&mut app, wid);
        let bottom = text_of(&app.windows[&wid].input_scratch.cells[strip + 9]);
        assert!(
            bottom.contains("second@example.test")
                && bottom.contains("weekly unavailable")
                && !bottom.contains("Muse Code Max Usage")
                && !bottom.contains("weekly 29% left"),
            "anonymous old usage cannot be attributed to the new login: {bottom:?}"
        );
        // Neither forgetting a pane's cache nor unrelated new terminal
        // text supplies the missing account provenance.
        app.windows
            .get_mut(&wid)
            .expect("window")
            .claude_footer_texts
            .borrow_mut()
            .trim(0);
        let mut rebuilt = carded.clone();
        rebuilt[10] = "  gpt-5.4 · high · ~/other · Auto-review".into();
        feed_rows(&app, session, &rebuilt);
        frame(&mut app, wid);
        frame(&mut app, wid);
        let bottom = text_of(&app.windows[&wid].input_scratch.cells[strip + 9]);
        assert!(
            bottom.contains("second@example.test")
                && bottom.contains("weekly unavailable")
                && !bottom.contains("Muse Code Max Usage")
                && !bottom.contains("weekly 29% left"),
            "a new pane/text generation cannot reattribute the old card: {bottom:?}"
        );
        let stale = vec![
            String::new(),
            "  Muse Code 0.9.3".into(),
            "\u{250C}".to_owned() + &"\u{2500}".repeat(83) + "\u{2510}",
            "\u{2502}  MUSE CODE 1.4.11 / redacted-session                                            IDLE \u{2502}".into(),
            "\u{2502}  ACCOUNT        Redacted User (redacted-user@example.test)                          \u{2502}".into(),
            "\u{2502}  BILLING        Subscription \u{00B7} Muse Code Max Usage                                  \u{2502}".into(),
            "\u{2514}".to_owned() + &"\u{2500}".repeat(83) + "\u{2518}",
            "\u{250C}".to_owned() + &"\u{2500}".repeat(55) + "\u{2510}",
            "\u{2502}  Session usage                                        \u{2502}".into(),
            "\u{2502}  Subscription \u{00B7} Muse Code Max Usage                   \u{2502}".into(),
            "\u{2502}    Weekly         71% used \u{00B7} Resets Oct 25 at 5:00 PM \u{2502}".into(),
            "\u{2514}".to_owned() + &"\u{2500}".repeat(55) + "\u{2518}",
            labelled,
            "\u{276F} ".into(),
            rule,
            footer_row.into(),
        ];
        let stale_bottom_row = strip + stale.len() - 2;
        feed_rows(&app, session, &stale);
        frame(&mut app, wid);
        frame(&mut app, wid);
        let bottom = text_of(&app.windows[&wid].input_scratch.cells[stale_bottom_row]);
        assert!(
            bottom.contains("second@example.test")
                && bottom.contains("weekly unavailable")
                && !bottom.contains("redacted-user@example.test")
                && !bottom.contains("weekly 29% left"),
            "the previous account's cards stay historical after a switch: {bottom:?}"
        );
        // A matching account-bearing card supplies its OWN plan, but the
        // separate old usage card below it still names no account. Even
        // matching status glass cannot attach that anonymous quota to B.
        let mut current = stale.clone();
        current[4] = format!(
            "│{:<83}│",
            "  ACCOUNT        Second User (second@example.test)"
        );
        current[5] = format!("│{:<83}│", "  BILLING        Subscription · Muse Code Pro");
        feed_rows(&app, session, &current);
        frame(&mut app, wid);
        frame(&mut app, wid);
        let bottom = text_of(&app.windows[&wid].input_scratch.cells[stale_bottom_row]);
        assert!(
            bottom.contains("second@example.test")
                && bottom.contains("Muse Code Pro")
                && bottom.contains("weekly unavailable")
                && !bottom.contains("Muse Code Max Usage")
                && !bottom.contains("weekly 29% left"),
            "only the matching status card's own fields are attributable: {bottom:?}"
        );
        {
            let entry = app.pool.get(session).expect("session");
            let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            assert!(timeline.clear_claude_footer());
            timeline.note_foreground_group(6162);
            timeline.set_program(6162, Some("muse".into()));
            assert!(
                timeline.muse_cards_need_account(),
                "clearing facts and changing process cannot clear surviving old glass"
            );
        }
        host.shutdown_and_join();
    }

    /// NO `ctx auto` (`docs/DESIGN-harness-autopilot-2026-10-15.md` §12.4.3 item 5, slice 4e):
    /// the context fact is a reading or nothing. A fresh Claude Code pane (its facts from its
    /// registry row, through the resolver), a fresh agy and a fresh Muse Code pane (the facts
    /// the frame makes for them) and an agy pane the resolver fills with no registry row show
    /// their facts and no `◐`. Once Claude Code's glass shows its indicator the frame reads it
    /// and the rule draws `◐ ctx 84% left`; the reading survives the next registry read and
    /// follows the indicator where 2.1.305 draws it, on the mode row under the composer
    /// (`◐ ctx 61% left`). An agy or a Muse Code pane (neither draws such an indicator) with a
    /// reading published draws it, and agy's keeps it through the resolver's next fill.
    /// CONTROLS: main's placeholders drew `◐ ctx auto` on every fresh pane; main's gate
    /// (`want_ctx = shown.context.is_some()`) without them never reads the indicator; main's
    /// reader (`context_left`) never reads the mode row's.
    #[test]
    fn a_pane_with_no_context_reading_draws_no_context_fact() {
        // Wide enough for the registry row's scratch directory as the path, beside which
        // the context fact, the first to give way, still fits.
        const COLS: usize = 240;
        const READING: &str = "ctx 84% left";
        let mark = footer::CONTEXT_MARK;
        let rule = "\u{2500}".repeat(COLS);
        let indicator = format!(
            "{:>COLS$}",
            "Context left until auto-compact: 84%",
            COLS = COLS
        );
        let over_composer = |shown: bool| {
            if shown {
                indicator.clone()
            } else {
                String::new()
            }
        };
        let claude_screen = |ctx: bool, mode_row_end: &str| -> Vec<String> {
            let hints = "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)";
            let pad = COLS - hints.chars().count() - mode_row_end.chars().count();
            vec![
                String::new(),
                " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259C}\u{258C}   Claude Code v2.1.92".into(),
                "\u{259D}\u{259C}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{259B}\u{2598}  Opus 4.6 \u{00B7} Claude Max".into(),
                "  \u{2598}\u{2598} \u{259D}\u{259D}    ~/aterm".into(),
                over_composer(ctx),
                rule.clone(),
                "\u{276F} ".into(),
                rule.clone(),
                format!("{hints}{}{mode_row_end}", " ".repeat(pad)),
            ]
        };
        let agy_screen = vec![
            "      \u{2584}\u{2580}\u{2580}\u{2584}        Antigravity CLI 1.4.0".to_string(),
            "    \u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}      Gemini 4 Argon (Medium)".into(),
            String::new(),
            rule.clone(),
            "> ".into(),
            rule.clone(),
            "? for shortcuts".into(),
        ];
        let muse_screen = vec![
            String::new(),
            "  Muse Code 0.9.3".to_string(),
            String::new(),
            rule.clone(),
            "\u{276F} ".into(),
            rule.clone(),
            "  gpt-5.4 \u{00B7} high \u{00B7} ~/aterm \u{00B7} Auto-review".into(),
        ];
        // The rule under the input box, where the facts go.
        let bottom = |app: &App, wid: WindowId, rows: &[String]| {
            let at = rows
                .iter()
                .rposition(|r| r.starts_with('\u{2500}'))
                .expect("the composer's bottom rule");
            text_of(&app.windows[&wid].input_scratch.cells[strip(app, wid) + at])
        };
        let named = |app: &App, session: u64, pgid: i32, program: &str| {
            let entry = app.pool.get(session).expect("session");
            let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            timeline.note_foreground_group(pgid);
            assert!(timeline.set_program(pgid, Some(program.into())));
            entry.ctx.timeline.clone()
        };
        let context = |timeline: &Arc<Mutex<SessionTimeline>>| {
            timeline
                .lock()
                .unwrap()
                .claude_footer()
                .expect("the pane's facts")
                .context
                .clone()
        };
        let asks = Asks {
            args: |_| Some(args_of(&["HOME=/home/someone"])),
            roster: || None,
            home: scratch_home,
            stored_login: |_, _, _| true,
        };
        let fresh = |shown: &str, program: &str| {
            assert!(
                shown.contains(footer::ACCOUNT_MARK) && !shown.contains(mark),
                "{program}: the facts, and no context fact without a reading: {shown:?}"
            );
        };
        let drawn = |shown: &str, program: &str, reading: &str| {
            assert!(
                shown.contains(&format!("{mark} {reading}")),
                "{program}: a reading is drawn: {shown:?}"
            );
        };

        // Claude Code, its facts read off its registry row.
        const PID: i32 = 4261;
        const START: u64 = 1_790_607_861;
        let (root, home) = claude_home("footer-no-context-reading", PID, START);
        let (mut app, wid, session) = glass(COLS as u16);
        let timeline = named(&app, session, PID, "claude");
        let mut identity = unread(PID, &home);
        identity.started = Some(START);
        let mut tail = footer::TailCache::default();
        let mut fold = footer::FooterCache::default();
        let mut memo = RosterMemo::default();
        resolve_facts(
            &timeline,
            &mut identity,
            &mut tail,
            &mut fold,
            &mut memo,
            asks,
        );
        assert_eq!(context(&timeline), None, "claude: no reading, no fact");
        let screen = claude_screen(false, "");
        feed_rows(&app, session, &screen);
        frame(&mut app, wid);
        fresh(&bottom(&app, wid, &screen), "claude");
        let screen = claude_screen(true, "");
        feed_rows(&app, session, &screen);
        frame(&mut app, wid);
        drawn(&bottom(&app, wid, &screen), "claude", READING);
        resolve_facts(
            &timeline,
            &mut identity,
            &mut tail,
            &mut fold,
            &mut memo,
            asks,
        );
        assert_eq!(
            context(&timeline).as_deref(),
            Some(READING),
            "claude: the reading stays through the next registry read"
        );
        // Claude Code 2.1.305 draws its indicator at the right end of its mode row.
        let screen = claude_screen(false, "61% until auto-compact");
        feed_rows(&app, session, &screen);
        frame(&mut app, wid);
        drawn(&bottom(&app, wid, &screen), "claude", "ctx 61% left");
        std::fs::remove_dir_all(root).unwrap();

        // agy and Muse Code, the facts the frame makes for a pane that has none yet.
        for (pgid, program, screen) in [(7001, "agy", &agy_screen), (7002, "muse", &muse_screen)] {
            let (mut app, wid, session) = glass(COLS as u16);
            let timeline = named(&app, session, pgid, program);
            feed_rows(&app, session, screen);
            frame(&mut app, wid);
            assert_eq!(context(&timeline), None, "{program}: no reading, no fact");
            fresh(&bottom(&app, wid, screen), program);
            {
                let mut held = timeline.lock().unwrap();
                let mut facts = held.claude_footer().cloned().expect("the frame's facts");
                facts.context = Some(READING.into());
                assert!(held.set_claude_footer(pgid, None, Some(facts)));
            }
            frame(&mut app, wid);
            drawn(&bottom(&app, wid, screen), program, READING);
        }

        // agy filled by the resolver, with no registry row (and no process to read: above
        // any pid a kernel hands out).
        const NO_PROCESS: i32 = 999_983;
        let (app, _, session) = glass(COLS as u16);
        let timeline = named(&app, session, NO_PROCESS, "agy");
        let mut identity = unread(NO_PROCESS, &home);
        identity.dir = None;
        let fill = |identity: &mut Identity| {
            resolve_facts(
                &timeline,
                identity,
                &mut footer::TailCache::default(),
                &mut footer::FooterCache::default(),
                &mut RosterMemo::default(),
                asks,
            );
        };
        fill(&mut identity);
        assert_eq!(
            context(&timeline),
            None,
            "agy, resolved: no reading, no fact"
        );
        {
            let mut held = timeline.lock().unwrap();
            let mut facts = held.claude_footer().cloned().expect("the resolver's facts");
            facts.context = Some(READING.into());
            assert!(held.set_claude_footer(NO_PROCESS, None, Some(facts)));
        }
        fill(&mut identity);
        assert_eq!(
            context(&timeline).as_deref(),
            Some(READING),
            "agy, resolved: the reading stays through the next fill"
        );
    }

    /// A READING GOES WITH CLAUDE CODE'S INDICATOR (the review of slice 4e). The rule draws
    /// `◐ ctx 61% left` while 2.1.306's mode row ends with `61% until auto-compact`, and keeps
    /// it while Claude Code's `◐ medium · /effort` holds the indicator's slot, while the `/`
    /// menu stands in the mode row's place and while a box stands over the composer; once the
    /// mode row stands with its slot empty (as after `/compact`) the fact goes, from
    /// the rule and from the session's facts, and comes back with the indicator.
    /// CONTROLS: a frame that keeps a reading until the process exits (the slice's first build)
    /// still draws `◐ ctx 61% left` after the compaction; one that drops a reading whenever it
    /// reads none drops it under the notice, the menu and the box.
    #[test]
    fn a_context_reading_goes_when_claude_code_stops_showing_it() {
        const COLS: usize = 160;
        const PID: i32 = 4262;
        let mark = footer::CONTEXT_MARK;
        let rule = "\u{2500}".repeat(COLS);
        // Claude Code's own footer row ends two columns short of the rule.
        let footer_row = |left: &str, end: &str| {
            let pad = COLS - 2 - left.chars().count() - end.chars().count();
            format!("{left}{}{end}", " ".repeat(pad))
        };
        let rest = "  \u{23F8} manual mode on \u{00B7} ? for shortcuts";
        let screen = |caret: &str, under: Vec<String>| -> Vec<String> {
            let mut rows = vec![
                "\u{23FA} Hi! I am the local lab stub, not a model. (reply 1)".to_string(),
                String::new(),
                rule.clone(),
                caret.to_string(),
                rule.clone(),
            ];
            rows.extend(under);
            rows
        };
        let shown = screen("\u{276F}", vec![footer_row(rest, "61% until auto-compact")]);
        let notice = screen(
            "\u{276F} h",
            vec![footer_row(
                "  \u{23F8} manual mode on",
                "\u{25D0} medium \u{00B7} /effort",
            )],
        );
        let menu = screen(
            "\u{276F} /",
            vec![
                "  \u{276F} /add-dir                 Add a new working directory".to_string(),
                "    /agents                  Manage agent configurations".to_string(),
                "    /autocompact             Set how full the context gets before auto-summarizing"
                    .to_string(),
            ],
        );
        let mut boxed = aterm_phase::prompt::fixtures::bash_one_row();
        let last = boxed.len() - 1;
        boxed[last] = footer_row(rest, "");
        let gone = screen("\u{276F}", vec![footer_row(rest, "")]);
        // The rule under the input box, where the facts go.
        let bottom = |app: &App, wid: WindowId, rows: &[String]| {
            let at = rows
                .iter()
                .rposition(|r| r.starts_with('\u{2500}'))
                .expect("the composer's bottom rule");
            text_of(&app.windows[&wid].input_scratch.cells[strip(app, wid) + at])
        };
        let (mut app, wid, session) = glass(COLS as u16);
        let timeline = {
            let entry = app.pool.get(session).expect("session");
            let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            timeline.note_foreground_group(PID);
            assert!(timeline.set_program(PID, Some("claude".into())));
            assert!(timeline.set_claude_footer(
                PID,
                None,
                Some(FooterFacts {
                    subscription: Some(
                        aterm_agent::harness::profile::SubscriptionDisplay::default(),
                    ),
                    ..FooterFacts::default()
                }),
            ));
            entry.ctx.timeline.clone()
        };
        let context = || {
            timeline
                .lock()
                .unwrap()
                .claude_footer()
                .expect("the pane's facts")
                .context
                .clone()
        };
        let mut show = |rows: &[String]| {
            feed_rows(&app, session, rows);
            frame(&mut app, wid);
            bottom(&app, wid, rows)
        };
        let reading = format!("{mark} ctx 61% left");
        let drawn = show(&shown);
        assert!(drawn.contains(&reading), "the indicator, read: {drawn:?}");
        for (what, rows) in [
            ("the effort notice in the slot", &notice),
            ("the / menu in the mode row's place", &menu),
            ("a box over the composer", &boxed),
        ] {
            let drawn = show(rows);
            assert!(
                drawn.contains(&reading),
                "{what}: the reading stays: {drawn:?}"
            );
            assert_eq!(context().as_deref(), Some("ctx 61% left"), "{what}");
        }
        let drawn = show(&gone);
        assert!(
            drawn.contains(footer::ACCOUNT_MARK) && !drawn.contains(mark),
            "the slot empty: the fact goes with the indicator: {drawn:?}"
        );
        assert_eq!(context(), None, "and from the session's facts");
        let drawn = show(&shown);
        assert!(
            drawn.contains(&reading),
            "shown again, drawn again: {drawn:?}"
        );
    }

    /// THE SINGLE TOGGLE TURNS THE HARNESS WHITE AND BACK, for Claude Code,
    /// agy and Muse Code: Off (one call, the running session's) takes the
    /// harness's hands off — agy's transcript goes back to its raw cells —
    /// and turns its glass white: the composer's switch says `○ off` and the
    /// launch card says `autopilot ○ off`; the same call again is Cruise,
    /// the switch `● on` and the card Cruise's.
    #[test]
    fn single_toggle_turns_the_harness_white_and_back_for_claude_agy_and_muse() {
        let rule = "\u{2500}".repeat(160);
        let claude_fixture = vec![
            String::new(),
            " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259C}\u{258C}   Claude Code v2.1.92".into(),
            "\u{259D}\u{259C}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{259B}\u{2598}  Opus 4.6 \u{00B7} Claude Max".into(),
            "  \u{2598}\u{2598} \u{259D}\u{259D}    ~/aterm".into(),
            String::new(),
            rule.clone(),
            "\u{276F} ".into(),
            rule.clone(),
            // Claude Code's own context indicator, which the rule's context fact follows.
            format!(
                "{:<136}{}",
                "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)",
                "84% until auto-compact"
            ),
        ];
        let agy_fixture = vec![
            "      \u{2584}\u{2580}\u{2580}\u{2584}        Antigravity CLI 1.4.0".into(),
            "     \u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}       redacted-user@example.test (Google AI Ultra)".into(),
            "    \u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}      Gemini 4 Argon (Medium)".into(),
            "   \u{2584}\u{2580}\u{2580}    \u{2580}\u{2580}\u{2584}     ~/aterm".into(),
            "  \u{2584}\u{2580}\u{2580}      \u{2580}\u{2580}\u{2584}".into(),
            "\u{25CF} # Heading".into(),
            rule.clone(),
            "> ".into(),
            rule.clone(),
            "? for shortcuts                                                                                  Gemini 4 Argon \u{00B7} medium".into(),
        ];
        let muse_fixture = vec![
            String::new(),
            "  Muse Code 0.9.3".into(),
            String::new(),
            // Muse Code's own top rule at rest is labelled: the harness
            // branding replaces the label on the glass (owner, 2026-10-21).
            format!(
                "\u{2500}\u{2500} Voice input (\u{2325} + v to start) {}",
                "\u{2500}".repeat(128)
            ),
            "\u{276F} ".into(),
            rule,
            "  gpt-5.4 \u{00B7} high \u{00B7} ~/aterm \u{00B7} Auto-review".into(),
        ];

        for (prog, build, rows, top_rule_row, card_row) in [
            ("claude", "2.1.92", claude_fixture, 5usize, 4usize),
            ("agy", "1.4.2", agy_fixture, 6usize, 4usize),
            ("muse", "0.9.3", muse_fixture, 3usize, 2usize),
        ] {
            let (mut app, wid, session) = glass(160);
            {
                let entry = app.pool.get(session).expect("session");
                let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
                timeline.note_foreground_group(7000);
                timeline.set_program(7000, Some(prog.into()));
                assert!(timeline.set_claude_footer(
                    7000,
                    None,
                    Some(FooterFacts {
                        model_open: true,
                        effort_open: true,
                        path: Some("~/aterm".into()),
                        branch: Some("main".into()),
                        owner: Some(footer::FactsOwner {
                            floor: None,
                            session_id: format!("{prog}-7000"),
                        }),
                        subscription: Some(
                            aterm_agent::harness::profile::SubscriptionDisplay::default(),
                        ),
                        context: Some("ctx 84% left".into()),
                        ..FooterFacts::default()
                    }),
                ));
            }
            let host = crate::harness_host::HostHandle::for_supervised_test(
                Vec::new(),
                std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
            );
            app.harness = Some(host.clone());
            let ctx = app.pool.get(session).expect("session").ctx.clone();
            crate::session_timeline::claim_supervisor(
                &ctx,
                &crate::harness_host::holder(),
                None,
                None,
                crate::metrics::now_us(),
            )
            .expect("claim");

            // The card is the drawer's own: its image, the card after its cut.
            super::probe::drawer(&app, session, 7000, prog, build);
            feed_rows(&app, session, &rows);
            frame(&mut app, wid);

            let strip = strip(&app, wid);
            // The lockup: two `─`, the icon's pads and cells, the CLI's name.
            let expected_lockup = format!("\u{2500}\u{2500}    {prog} \u{2500}");
            let expected_switch = " alab harness \u{25CF} on ";
            let top_on = text_of(&app.windows[&wid].input_scratch.cells[strip + top_rule_row]);
            assert!(
                top_on.starts_with(&expected_lockup) && top_on.contains(expected_switch),
                "{prog}: active harness lays the lockup at the top rule's left and the switch at its right: {top_on:?}"
            );
            let bottom_on =
                text_of(&app.windows[&wid].input_scratch.cells[strip + top_rule_row + 2]);
            assert!(
                bottom_on.contains("\u{2386} ")
                    && bottom_on.contains("\u{25f7} weekly ")
                    && bottom_on.contains("\u{25D0} ctx 84% left"),
                "{prog}: standardized bottom rule carries account email, weekly budget, and context: {bottom_on:?}"
            );
            if prog == "agy" {
                assert!(
                    bottom_on.contains("redacted-user@example.test")
                        && bottom_on.contains("Google AI Ultra"),
                    "agy: launch card account email and plan populate standardized footer: {bottom_on:?}"
                );
            }
            if prog == "muse" {
                // Muse Code's idle voice label gives way to the lockup on
                // the glass (owner, 2026-10-21), so it stands at the rule's
                // left as Claude's and agy's do: `starts_with` above.
                assert!(
                    !top_on.contains("Voice input"),
                    "muse: the lockup takes the idle voice label's cells: {top_on:?}"
                );
                let row = &app.windows[&wid].input_scratch.cells[strip + top_rule_row];
                let at = row
                    .windows(4)
                    .position(|w| w.iter().map(|c| c.ch).eq("muse".chars()))
                    .expect("the muse word");
                for letter in &row[at..at + 4] {
                    assert!(
                        letter.fg[2] > 150 && letter.fg[0] < 80,
                        "muse: the name is Meta blue, not purple: {:?}",
                        letter.fg
                    );
                }
            }
            let card_on = text_of(&app.windows[&wid].input_scratch.cells[strip + card_row]);
            assert!(
                card_on.contains("enhanced by aterm harness") && card_on.contains("autopilot"),
                "{prog}: active harness draws launch card banner: {card_on:?}"
            );
            assert!(
                app.windows[&wid]
                    .claude_card
                    .painted_layers()
                    .iter()
                    .any(|l| l.slot == aterm_core::render::FxSlot::Over
                        && !l.content.items().is_empty()),
                "{prog}: active harness draws rainbow kitty"
            );

            // Toggle OFF with a single switch call during the running session.
            let off_view = app.toggle_session_harness(session);
            assert_eq!(
                off_view.mode(),
                Some(aterm_agent::supervise::Autopilot::Off),
                "{prog}: single toggle switches harness to Off"
            );
            frame(&mut app, wid);

            let top_off = text_of(&app.windows[&wid].input_scratch.cells[strip + top_rule_row]);
            assert!(
                top_off.contains(" alab harness \u{25CB} off \u{2590} on \u{258C} ")
                    && top_off.starts_with(&expected_lockup)
                    && !top_off.contains("alab kitty"),
                "{prog}: toggled-off harness draws its off switch, `on` its button, on the \
                 white top rule, the lockup still at its left and no wordmark: {top_off:?}"
            );
            let card_off = text_of(&app.windows[&wid].input_scratch.cells[strip + card_row]);
            assert!(
                card_off.contains("enhanced by aterm harness")
                    && card_off.contains("autopilot \u{25CB} off"),
                "{prog}: toggled-off harness wears the white card: {card_off:?}"
            );
            if prog == "agy" {
                let raw_md = text_of(&app.windows[&wid].input_scratch.cells[strip + 5]);
                assert_eq!(
                    raw_md.trim_end(),
                    "\u{25CF} # Heading",
                    "agy: toggled-off harness restores raw transcript row"
                );
            }

            // Toggle back ON with the same single switch during the running session.
            let on_view = app.toggle_session_harness(session);
            assert_eq!(
                on_view.mode(),
                Some(aterm_agent::supervise::Autopilot::Cruise),
                "{prog}: second single toggle seamlessly re-enables Cruise"
            );
            frame(&mut app, wid);

            let top_reenabled =
                text_of(&app.windows[&wid].input_scratch.cells[strip + top_rule_row]);
            assert!(
                top_reenabled.contains(expected_switch),
                "{prog}: re-enabled harness restores composer top rule branding: {top_reenabled:?}"
            );
            let card_reenabled = text_of(&app.windows[&wid].input_scratch.cells[strip + card_row]);
            assert!(
                card_reenabled.contains("enhanced by aterm harness"),
                "{prog}: re-enabled harness restores launch card banner: {card_reenabled:?}"
            );

            // The switch stays registered in both On and Off states, so a
            // click on it turns the harness off and on again.
            assert!(
                !app.windows[&wid].claude_lights.border_toggles.is_empty(),
                "{prog}: border_toggles registered when On"
            );
            app.toggle_session_harness(session);
            frame(&mut app, wid);
            assert!(
                !app.windows[&wid].claude_lights.border_toggles.is_empty(),
                "{prog}: border_toggles still registered when Off, so its `on` turns it back on"
            );

            host.shutdown_and_join();
        }
    }

    /// THE COMPOSER BORDER WEARS THE CARD'S PALETTE (owner, 2026-10-19: "I
    /// want a white themed for 'off' and that this theme is incorporated into
    /// the persistent harness border"; 2026-10-21: "alab harness on" in
    /// rainbow): Cruise's rule runs the spectrum's whole arc BACKWARD over
    /// the gap between the lockup and the switch — `t = 1` at the lockup down
    /// to `t = 0` at the switch, whose glyphs walk it forward again — its
    /// lead and tail at `t = 1`, so true blue shows in the gap; Land's is the
    /// sunset, warm in every cell; Off's is the moonbow, near neutral in
    /// every cell, its switch saying `○ off` with its `on` button and no
    /// wordmark (owner, 2026-10-20), as white with its claim given back (the
    /// real host's Off) as with it — and a press on that switch turns the
    /// harness back on (Cruise). With the card switched off, a claim landing
    /// moves the footer's repaint key (the border's theme, folded in:
    /// nothing else in it reads the claim then) and the border follows. On a
    /// light ground the switch's letters and the lockup's name are legible
    /// ink, and Off's state reads too. NEGATIVE CONTROLS: the old reading of
    /// the spectrum (`SPECTRUM_LUT[i * 255 / n]`, half of its 511 entries)
    /// never reaches blue or violet, and its letters on paper are too pale
    /// to read; Cruise's run is not warm; an unclaimed Cruise is bare, its
    /// rule untinted; and under the master switch off, the word Off draws
    /// the bare off switch, no wordmark. (The seam between the gap and the
    /// switch, and its control, are `top_tests::the_rule_flows_into_the_switch`.)
    #[test]
    fn the_composer_border_wears_the_cards_palette() {
        use aterm_agent::supervise::Autopilot;
        let rule = "\u{2500}".repeat(120);
        let rows: Vec<String> = vec![
            String::new(),
            " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259C}\u{258C}   Claude Code v2.1.92".into(),
            "\u{259D}\u{259C}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{259B}\u{2598}  Opus 4.6 \u{00B7} Claude Max".into(),
            "  \u{2598}\u{2598} \u{259D}\u{259D}    ~/aterm".into(),
            String::new(),
            rule.clone(),
            "\u{276F} ".into(),
            rule,
            "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)".into(),
        ];
        let (mut app, wid, session) = glass(120);
        {
            let entry = app.pool.get(session).expect("session");
            let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            timeline.note_foreground_group(7000);
            timeline.set_program(7000, Some("claude".into()));
            assert!(timeline.set_claude_footer(
                7000,
                None,
                Some(FooterFacts {
                    model_open: true,
                    effort_open: true,
                    path: Some("~/aterm".into()),
                    branch: Some("main".into()),
                    owner: Some(footer::FactsOwner {
                        floor: None,
                        session_id: "claude-7000".into(),
                    }),
                    ..FooterFacts::default()
                }),
            ));
        }
        let host = crate::harness_host::HostHandle::for_supervised_test(
            Vec::new(),
            std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
        );
        app.harness = Some(host.clone());
        let ctx = app.pool.get(session).expect("session").ctx.clone();
        let claim = || {
            crate::session_timeline::claim_supervisor(
                &ctx,
                &crate::harness_host::holder(),
                None,
                None,
                crate::metrics::now_us(),
            )
            .expect("claim");
        };
        let release = || {
            crate::session_timeline::release_supervisor(
                &ctx,
                None,
                crate::metrics::now_us(),
                aterm_slog::Actor::Ctl,
            );
        };
        feed_rows(&app, session, &rows);
        // The rule's run under `word`: its `─` cells' inks, and its text.
        let under = |app: &mut App, word: Autopilot| {
            app.set_session_autopilot(session, Some(word), crate::autopilot::Surface::Chip);
            frame(app, wid);
            let top = strip(app, wid) + 5;
            let cells = &app.windows[&wid].input_scratch.cells[top];
            let run: Vec<[u8; 3]> = cells
                .iter()
                .filter(|c| c.ch == '\u{2500}')
                .map(|c| c.fg)
                .collect();
            (run, text_of(cells))
        };

        // The card switched off, Cruise without the claim: bare. The claim
        // landing moves the key, and the border follows.
        app.config.claude_footer = Some(crate::app_config::ClaudeFooterConfig {
            card: Some(false),
            ..Default::default()
        });
        let (bare, text) = under(&mut app, Autopilot::Cruise);
        assert!(
            bare.windows(2).all(|w| w[0] == w[1]) && !text.contains("alab kitty"),
            "CONTROL: unclaimed bare, the rule untinted: {text:?}"
        );
        let plan = app.active_visible_leaf_plan(wid).expect("plan");
        let unclaimed = app.claude_footer_fp(wid, &plan);
        assert_eq!(app.claude_footer_fp(wid, &plan), unclaimed, "no churn");
        claim();
        assert_ne!(
            app.claude_footer_fp(wid, &plan),
            unclaimed,
            "the claim landing repaints the border with the card off"
        );
        let (claimed, text) = under(&mut app, Autopilot::Cruise);
        assert!(
            text.contains("harness \u{25CF} on") && claimed.windows(2).any(|w| w[0] != w[1]),
            "claimed, tinted: {text:?}"
        );
        app.config.claude_footer = None;
        let blue_violet = |c: [u8; 3]| {
            i32::from(c[2]) >= i32::from(c[0]) + 24 && i32::from(c[2]) >= i32::from(c[1]) + 24
        };
        let warm = |c: [u8; 3]| c[0] >= c[1] && c[0] >= c[2];
        // True blue, not azure nor violet: blue well over red and green, and
        // no more red than green. Over the dark ground's spectrum, as the rule
        // mixes it, the whole-run mapping's bluest visible cell is the azure
        // just left of the switch (blue about 46 over green) and the visible
        // mapping's is about 110 over.
        let blue = |c: [u8; 3]| c[0] <= c[1] && i32::from(c[2]) >= i32::from(c[0].max(c[1])) + 56;

        let (cruise, text) = under(&mut app, Autopilot::Cruise);
        assert!(text.contains("harness \u{25CF} on"), "{text:?}");
        assert!(cruise.len() > 60, "the run: {}", cruise.len());
        assert!(
            cruise.iter().copied().any(blue_violet),
            "Cruise's rule reaches blue and violet: {cruise:?}"
        );
        assert!(
            cruise.iter().copied().any(blue),
            "Cruise's visible rule shows true blue: {cruise:?}"
        );
        // The rule as laid: the 120-cell run, the 11-cell lockup two cells
        // in (2..13, `    claude `), the 19-cell lit switch two cells from
        // its end (99..118), each `─` the palette's stop at `t` mixed as the
        // border mixes it, over the rule's ink and the pane's: the lead and
        // the tail at `t = 1`, the gap's 86 cells walking from 1 to 0.
        let ground = app.windows[&wid].input_scratch.implicit_blank;
        let dark = crate::claude_card::dark_ground(ground);
        let paint = |t: f32| {
            let c = aterm_effects::harness_card::CardPalette::Cruise.art(t, dark);
            let stop = [(c >> 16) as u8, (c >> 8) as u8, c as u8];
            super::mix([90, 90, 90], super::mix(stop, ground.fg, 0.25), 0.68)
        };
        assert!(
            text.starts_with("\u{2500}\u{2500}    claude \u{2500}")
                && text.ends_with(" alab harness \u{25CF} on \u{2500}\u{2500}"),
            "the lockup at the rule's left, the switch at its right: {text:?}"
        );
        let paint = |t: f32| {
            let fg = paint(t);
            if fg == [90, 90, 90] {
                [fg[0] ^ 1, fg[1], fg[2]]
            } else {
                fg
            }
        };
        let shown: Vec<[u8; 3]> = [paint(1.0), paint(1.0)]
            .into_iter()
            .chain((0..86).map(|j| paint(1.0 - j as f32 / 85.0)))
            .chain([paint(1.0), paint(1.0)])
            .collect();
        assert_eq!(cruise, shown, "the palette backward over the gap");
        assert!(
            !cruise.iter().copied().all(warm),
            "CONTROL: Cruise's rule is not a sunset"
        );
        let mut old_half = (0..=255).map(|i| {
            let c = aterm_effects::spectrum::SPECTRUM_LUT[i];
            [(c >> 16) as u8, (c >> 8) as u8, c as u8]
        });
        assert!(
            !old_half.any(blue_violet),
            "CONTROL: the old reading stopped short of blue"
        );

        let (land, text) = under(&mut app, Autopilot::Land);
        assert!(text.contains("harness \u{25CF} on"), "{text:?}");
        assert!(
            land.iter().copied().all(warm),
            "Land's rule is the sunset: {land:?}"
        );

        let (off, text) = under(&mut app, Autopilot::Off);
        assert!(
            text.contains(" alab harness \u{25CB} off \u{2590} on \u{258C} ")
                && !text.contains("alab kitty"),
            "Off's switch says off, `on` its button, no wordmark: {text:?}"
        );
        for c in &off {
            let spread = c.iter().max().unwrap() - c.iter().min().unwrap();
            assert!(spread <= 24, "Off's rule is the moonbow: {c:?} in {off:?}");
        }
        assert_ne!(off, cruise, "Off is not Cruise");
        // Off as the host runs it: its worker stopped, the claim given back.
        release();
        assert!(!app.session_autopilot(session).2, "the claim is given back");
        let (released, text) = under(&mut app, Autopilot::Off);
        assert!(
            text.contains("alab harness \u{25CB} off"),
            "no claim: {text:?}"
        );
        assert_eq!(released, off, "white with no claim");
        // A press on the white rule's switch: the harness back on.
        let (frame_row, cols) = app.windows[&wid]
            .claude_lights
            .border_toggles
            .iter()
            .find(|t| t.session == session)
            .map(|t| (t.frame_row, t.cols.clone()))
            .expect("the switch is a target");
        let col = cols.start + 2;
        let (cw, ch) = app.win_cell_size(wid);
        let (ox, oy) = app.frame_origin(wid);
        let x = ox as f64 + (app.win_pad(wid) + col * cw + cw / 2) as f64;
        let y =
            oy as f64 + (app.win_pad_top(wid) + app.win_head(wid) + frame_row * ch + ch / 2) as f64;
        assert_eq!(app.frame_cell_at(wid, x, y), Some((frame_row, col)));
        app.windows.get_mut(&wid).expect("window").last_cursor_px = (x, y);
        assert!(app.press_claude_light(wid), "the switch takes the press");
        assert_eq!(
            app.session_autopilot(session).0,
            Some(Autopilot::Cruise),
            "Off's switch turns the harness back on"
        );

        // On paper (a white ground, a dark ink): every glyph of the lit
        // switch, `alab harness ● on`, holds the paper law the card's line
        // holds (WCAG 4.5:1 or more against the ground) under Cruise, and so
        // do the lockup's name and Off's state. CONTROL: the old letters, raw
        // spectrum mixed a quarter toward the ink, are not all legible.
        claim();
        {
            let term = app.pool.get(session).expect("session").term.clone();
            crate::term_lock(&term)
                .process(b"\x1b]11;rgb:ff/ff/ff\x1b\\\x1b]10;rgb:20/20/20\x1b\\");
        }
        let contrast = |c: [u8; 3]| {
            use aterm_effects::color_math::relative_luminance;
            let y = relative_luminance(u32::from_be_bytes([0, c[0], c[1], c[2]]));
            let paper = relative_luminance(0x00FF_FFFF);
            (y.max(paper) + 0.05) / (y.min(paper) + 0.05)
        };
        for word in [Autopilot::Cruise, Autopilot::Off] {
            let (_, text) = under(&mut app, word);
            let cells = &app.windows[&wid].input_scratch.cells[strip(&app, wid) + 5];
            if word == Autopilot::Off {
                assert!(
                    !text.contains('\u{2726}'),
                    "Off wears no wordmark: {text:?}"
                );
                let off = cells.iter().position(|c| c.ch == '\u{25CB}').unwrap();
                assert!(contrast(cells[off].fg) >= 4.5, "Off's state on paper");
                continue;
            }
            let face: Vec<char> = "alab harness \u{25CF} on".chars().collect();
            let at = (0..cells.len().saturating_sub(face.len()))
                .find(|&k| {
                    cells[k..k + face.len()]
                        .iter()
                        .map(|c| c.ch)
                        .eq(face.iter().copied())
                })
                .unwrap_or_else(|| panic!("{word:?}: the switch on paper: {text:?}"));
            let letters: Vec<[u8; 3]> = cells[at..at + face.len()]
                .iter()
                .chain(&cells[6..12])
                .filter(|c| c.ch != ' ')
                .map(|c| c.fg)
                .collect();
            assert_eq!(
                letters.len(),
                "alabharness\u{25CF}onclaude".chars().count(),
                "{text:?}"
            );
            for c in &letters {
                assert!(contrast(*c) >= 4.5, "{word:?}: {c:?} too pale on paper");
            }
        }
        let ink = [0x20, 0x20, 0x20];
        let head: Vec<char> = " alab harness \u{25CF} on ".chars().collect();
        let old_pale = (0..head.len()).filter(|&k| head[k] != ' ').any(|k| {
            let c = aterm_effects::spectrum::SPECTRUM_LUT[(k * 255) / head.len()];
            contrast(mix([(c >> 16) as u8, (c >> 8) as u8, c as u8], ink, 0.25)) < 4.5
        });
        assert!(old_pale, "CONTROL: the old letters vanish on paper");

        // NEGATIVE CONTROL: no harness runs under the master switch off.
        app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
            crate::app_config::HarnessPolicy::read("[harness]\nenabled = false\n"),
        );
        let (_, text) = under(&mut app, Autopilot::Off);
        assert!(
            !text.contains("alab kitty")
                && text.contains(" alab harness \u{25CB} off \u{2590} on \u{258C} "),
            "master off: the bare off switch {text:?}"
        );
        host.shutdown_and_join();
    }

    #[test]
    fn agy_glass_presentation_preserves_literal_cells_and_undoes_cleanly() {
        fn collect(scratch: &RenderInput) -> Vec<AgyRowEdit> {
            collect_for(scratch, aterm_phase::Program::Agy)
        }
        fn collect_for(scratch: &RenderInput, program: aterm_phase::Program) -> Vec<AgyRowEdit> {
            let rows: Vec<String> = scratch
                .cells
                .iter()
                .map(|r| r.iter().map(|c| c.ch).collect())
                .collect();
            let mut edits = Vec::new();
            collect_agy_transcript_edits(
                scratch,
                program,
                &rows,
                PaneTextRegion {
                    strip: 0,
                    row_off: 0,
                    col_off: 0,
                    rows: scratch.rows,
                    cols: scratch.cols,
                },
                Some(scratch.rows),
                RenderCell::default(),
                RenderCell {
                    fg: [80, 80, 80],
                    ..RenderCell::default()
                },
                [180, 100, 80],
                &mut edits,
            );
            edits
        }
        let rows = [
            "Signing in...",
            "Authenticated.",
            "  └ Tip: literal tool output",
            "^[[?2026$p^[P>|aterm(0.42.0)",
            "○ Thought for 1s (literal suffix)",
            "● # Overview",
            "  Here is **bold**, *italic*, and `code`.",
            "  ```text",
            "  Signing in...",
            "  └ Tip: literal code",
            "  **literal**",
            "  ^[[?2026$p",
            "  ```",
            "  - First item",
            "  ```text",
            "──────────────",
            "> x",
            "hello",
            "  **literal**",
            "      ▄▀▀▄        Antigravity CLI 1.4.0",
            "     ▀▀▀▀▀▀       redacted-user@example.test (Google AI Ultra)",
            "    ▀▀▀▀▀▀▀▀      Gemini 4 Argon (Medium)",
            "   ▄▀▀    ▀▀▄     ~/aterm",
            "  ▄▀▀      ▀▀▄",
            "  **still literal after a quoted card**",
            "  ```",
        ];
        let quoted: Vec<String> = rows.iter().map(|r| (*r).to_owned()).collect();
        assert!(aterm_phase::agy::launch_card_scan(&quoted).1.is_some());
        let mut scratch = RenderInput::empty();
        scratch.rows = rows.len();
        scratch.cols = 100;
        scratch.cursor_visible = false;
        scratch.cells = rows.iter().map(|r| row_of(r, [0, 0, 0])).collect();
        let original = scratch.cells.clone();
        let edits = collect(&scratch);
        assert_eq!(
            edits.iter().map(|e| e.frame_row).collect::<Vec<_>>(),
            vec![5, 6, 13]
        );
        let mut undo_slot = FooterUndo::default();
        write_edits(
            &mut undo_slot,
            &mut scratch,
            edits
                .iter()
                .map(|e| (e.frame_row, e.col, e.cells.as_slice(), e.ground)),
        );
        assert_eq!(text_of(&scratch.cells[5]).trim_end(), "● Overview");
        assert!(
            scratch.cells[5][2].bold
                && scratch.cells[5][2].underline == aterm_core::terminal::UnderlineStyle::Single,
            "H1 heading is bold + underlined"
        );
        assert_eq!(
            text_of(&scratch.cells[6]).trim_end(),
            "  Here is bold, italic, and code."
        );
        assert!(scratch.cells[6][10].bold && scratch.cells[6][16].italic);
        assert_ne!(
            scratch.cells[6][28].bg,
            RenderCell::default().bg,
            "inline code pill background tinted"
        );
        assert_eq!(text_of(&scratch.cells[13]).trim_end(), "  • First item");
        for (r, original_row) in original.iter().enumerate().take(scratch.rows) {
            if ![5, 6, 13].contains(&r) {
                assert_eq!(&scratch.cells[r], original_row);
            }
        }
        assert!(undo(&mut undo_slot, &mut scratch));
        assert_eq!(scratch.cells, original);

        // Verify H2/H3 visual hierarchy, links, checkboxes, nested bullets, and quotes.
        let md_rows = [
            "  ## Section",
            "  ### Detail",
            "  See [docs](https://example.com) now.",
            "  - [ ] Pending",
            "    - Nested",
            "  > Quoted",
        ];
        scratch.rows = md_rows.len();
        scratch.cells = md_rows.iter().map(|r| row_of(r, [0, 0, 0])).collect();
        let md_edits = collect(&scratch);
        assert_eq!(md_edits.len(), 6);
        assert_eq!(text_of(&md_edits[0].cells).trim_end(), "  Section");
        assert!(
            md_edits[0].cells[2].bold
                && !md_edits[0].cells[2].italic
                && md_edits[0].cells[2].underline == aterm_core::terminal::UnderlineStyle::None
        );
        assert_eq!(text_of(&md_edits[1].cells).trim_end(), "  Detail");
        assert!(md_edits[1].cells[2].bold && md_edits[1].cells[2].italic);
        assert_eq!(text_of(&md_edits[2].cells).trim_end(), "  See docs now.");
        assert_eq!(
            md_edits[2].cells[6].underline,
            aterm_core::terminal::UnderlineStyle::Single
        );
        assert_eq!(text_of(&md_edits[3].cells).trim_end(), "  \u{2610} Pending");
        assert_eq!(
            text_of(&md_edits[4].cells).trim_end(),
            "    \u{25E6} Nested"
        );
        assert_eq!(text_of(&md_edits[5].cells).trim_end(), "  \u{258E} Quoted");

        // Verify Muse transcript grammar: `◆ ` reply lead and continuations are styled,
        // while `❯ ` user prompts (and wrapped 2-space user continuations), tool boxes
        // (`  ╭─`, `  │ `), and status lines (`  ◇ `) stay untouched.
        let muse_rows = [
            "  Muse Code 0.9.3",
            "\u{276f} user prompt",
            "  **wrapped user text stays literal**",
            "  \u{256d}\u{2500} Bash",
            "  \u{2502} **tool output stays literal**",
            "  \u{2570}\u{2500}",
            "  \u{25c7} Thinking **literal**...",
            "\u{25c6} # Muse Heading",
            "  Here is ***bold italic*** and **bold `code`**.",
            "  ---",
        ];
        scratch.rows = muse_rows.len();
        scratch.cells = muse_rows.iter().map(|r| row_of(r, [0, 0, 0])).collect();
        let muse_edits = collect_for(&scratch, aterm_phase::Program::Muse);
        assert_eq!(
            muse_edits.iter().map(|e| e.frame_row).collect::<Vec<_>>(),
            vec![7, 8, 9],
            "only Muse assistant reply rows (7, 8, 9) are styled"
        );
        assert_eq!(
            text_of(&muse_edits[0].cells).trim_end(),
            "\u{25c6} Muse Heading"
        );
        assert_eq!(
            text_of(&muse_edits[1].cells).trim_end(),
            "  Here is bold italic and bold code."
        );
        assert!(muse_edits[1].cells[10].bold && muse_edits[1].cells[10].italic);
        assert!(
            muse_edits[2]
                .cells
                .iter()
                .skip(2)
                .take(10)
                .all(|c| c.ch == '\u{2500}'),
            "horizontal rule rendered as box-drawing line"
        );

        // A clipped viewport has no opening fence to establish code context.
        // Startup-like strings still cannot authorize deletion.
        scratch.rows = 3;
        scratch.cells = [8, 9, 11]
            .into_iter()
            .map(|r| original[r].clone())
            .collect();
        assert!(collect(&scratch).is_empty());

        // Ordinary markdown still styles; wide continuation and presentation
        // flags require the untouched cells, not plain-character rebuilding.
        scratch.rows = 1;
        scratch.cells = vec![row_of("  **text**", [0, 0, 0])];
        assert_eq!(collect(&scratch).len(), 1);
        scratch.cells = vec![row_of("  **你 **", [0, 0, 0])];
        scratch.cells[0][5].wide = true;
        assert!(collect(&scratch).is_empty());
        scratch.cells = vec![row_of("  **❤ **", [0, 0, 0])];
        scratch.cells[0][4].emoji_presentation = true;
        scratch.cells[0][5].wide = true;
        assert!(collect(&scratch).is_empty());
        scratch.cells = vec![row_of("  **😀**", [0, 0, 0])];
        scratch.cells[0][4].text_presentation = true;
        assert!(collect(&scratch).is_empty());
        scratch.cells = vec![row_of("  **גבא**", [0, 0, 0])];
        let mut map: Vec<u16> = (0..scratch.cols)
            .map(|c| u16::try_from(c).unwrap())
            .collect();
        map.swap(4, 6);
        scratch.visual_to_logical = vec![map];
        assert!(collect(&scratch).is_empty());
        scratch.visual_to_logical.clear();
        assert_eq!(collect(&scratch).len(), 1);
        scratch.cells = vec![row_of("  **text**", [0, 0, 0])];
        scratch.selection.start_selection(
            0,
            2,
            aterm_core::selection::SelectionSide::Left,
            aterm_core::selection::SelectionType::Simple,
        );
        scratch
            .selection
            .update_selection(0, 7, aterm_core::selection::SelectionSide::Right);
        scratch.selection.complete_selection();
        assert!(scratch.any_selection());
        assert!(collect(&scratch).is_empty());
    }
}
