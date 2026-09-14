// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `xtask gate help-surfaces` — every CLI help surface in the workspace has been
//! READ against its handler, and a surface that appears or changes afterwards is
//! refused until it is read again.
//!
//! WHY THIS EXISTS. Between 2026-08-31 and 2026-09-10, 138 help/usage claims
//! across the alab repos were found to contradict the code that implements them
//! — rosters hand-typed beside derived sets, exit codes that no longer happen,
//! remedies naming verbs that do not do the thing (in this workspace: the ctl
//! catalog, atpkg's usage and doctor, the fleet/drive CLIs, aterm-dev, the
//! release cutter). Every one was found by reading a help surface against its
//! handler. That read is a dated fact about ONE version of the text, and nothing
//! made it durable: a new binary's usage text, or an edit to an existing one,
//! stayed unread until somebody thought to sweep again. This gate turns "all help
//! surfaces are read-verified" from a snapshot into an invariant of every green
//! `gate all` / `cargo test -p xtask`.
//!
//! WHAT A HELP SURFACE IS. A source file that carries user-facing usage/help
//! text, discovered mechanically: a string literal with a line that starts a
//! usage block (`Usage:`, `OPTIONS:`, `COMMANDS:`, `VERBS:`, …) or code that
//! exists only to print help (`fn usage`/`print_usage`/`help_text`/`cmd_help`, a
//! `USAGE`/`HELP`-segment const, a clap `#[command(`/`about =`/`long_about`).
//! [`NOT_HELP`] classifies what discovery over-finds (each with a reason), so
//! every discovered file is accounted for one way or the other.
//!
//! WHAT IS HASHED. The file's PROSE: every string literal plus every doc comment
//! (`///`, `//!`, `/** */`), in source order, with plain comments, char literals
//! and code dropped. A code-only edit leaves a verified row alone; any change to
//! text a user can read invalidates it.
//!
//! REFUSALS, each naming its remedy:
//!   R1 UNROSTERED  a discovered surface has no row in [`SURFACES`] or [`NOT_HELP`]
//!   R2 CHANGED     a surface's prose differs from the hash recorded when it was read
//!   R3 GONE        a rostered path no longer exists
//!   R4 STALE       a row's date is malformed or older than [`MAX_AGE_DAYS`]
//!   R5 DEAD_ALLOW  a [`NOT_HELP`] entry names a file that is gone or no longer discovered
//!   R6 DUPLICATE   a path appears twice
//!   R7 PROGRAM     a binary entry point (`src/main.rs`, `src/bin/*.rs`,
//!                  `src/bin/*/main.rs`, a `[[bin]] path`) under [`SCAN_ROOTS`] is in
//!                  neither list — every PROGRAM is accounted for, even one whose
//!                  help lives elsewhere (say where, in [`NOT_HELP`]) or that has none
//!   R8 EMBEDDED    a file a rostered surface pulls in with `include_str!` /
//!                  `include_bytes!` is in neither list. Such a file IS shipped help
//!                  text — it reaches the binary and the reader — but discovery walks
//!                  `*.rs`, and the only string the lexer sees at the macro is the
//!                  PATH, so its content never enters the including file's hash.
//!                  Measured 2026-09-13: the four primer skill assets, 1,281 lines of
//!                  agent-facing CLI documentation `include_str!`-ed into a rostered
//!                  surface and installed into agents' own context files, were
//!                  invisible to this gate and to every other.
//!
//! The remedy for R1/R2 is a READ, not a hash bump: the refusal line carries the
//! row to paste, dated today. Pasting it is the assertion that the text was read
//! against its handler on that date.
//!
//! WHAT CHANGED. An R2 line ends `(see: xtask gate help-surfaces --diff PATH)`.
//! That verb finds the version the recorded read was of — the newest commit in
//! `git log -- PATH` whose version hashes to the row — and prints a unified diff
//! of the PROSE ONLY from it to the working tree, item by item as the hasher sees
//! it ([`prose_lines`]; a long item continues on lines of its own): a literal
//! continued across source lines with no quote on the continuation still shows,
//! by the line that changed. A row recorded from an uncommitted tree matches no
//! commit; the verb says so and diffs from the commit that last touched the row
//! instead.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// One read-verified surface: workspace-relative path, prose hash
/// ([`prose_hash`], FNV-1a 64 as 16 hex digits), the `YYYY-MM-DD` it was read,
/// and by what method.
pub(crate) type SurfaceRow<'a> = (&'a str, &'a str, &'a str, &'a str);
/// A file discovery over-finds: path and why it is not a help surface.
pub(crate) type NotHelpRow<'a> = (&'a str, &'a str);

/// A read older than this is stale: the text may still be byte-identical, but
/// the code under it has had half a year to move.
const MAX_AGE_DAYS: i64 = 180;
/// Where discovery walks (workspace-relative). Test, example and bench trees are
/// skipped by [`excluded_path`].
const SCAN_ROOTS: &[&str] = &["crates"];

/// The roster. Every entry is a dated assertion; see the module doc.
const SURFACES: &[SurfaceRow<'static>] = &[
    // HELP_SURFACES_ROSTER_BEGIN
    (
        "crates/aterm-agent/src/supervise/run.rs",
        "789ec5e4f644b859",
        "2026-09-14",
        "read crates/aterm-agent/src/supervise/run.rs's outage prose (the module doc, CtlReply::lost and the LOST/LOST_SOCKET/TURNED_AWAY docs, DEFAULT_RECONNECT/RECONNECT_PAUSE, Fail, Past, Rode, Outage, Looking.moved, Pressing, Session's outage/probing/last/stray fields and the call, unserved, fault, read, wait, spent_turn, await_turn, await_turn_from, supervise, watch, drive, look, ride_out, auto_read, moved_past, press_one_guarded, turn_of, stray_digit and render_phase docs, and the test docs this change added or rewrote: the mock's handoff model and the fifteen outage tests) against Session::call's outage books (kind = the verb and its first word, ended by that kind served or an `await seq` that latched, never by the probe), unserved's no-such-session-only-in-an-outage rule, lost's whole-phrase TURNED_AWAY match, wait's unserved-before-124 order, ride_out's since-anchored window, told/back flags, outage-wide pause, top-of-function deadline check and Rode::Spent, drive's fresh-only handed reset and look-through outage reset, look's take of state.moved, its post-deadline stray check and its deadline check before a review point is reported, auto_read's Past::Still(None) arm returning the parsed turn unread (current_turn, its last caller gone, removed with its doc line), moved_past's below-seq check read, the fallback press's Pressing::Lost cases, the server's `ERR control server busy; retry` and `ERR auth` lines (crates/aterm-gui/src/control.rs) and SeqAdvanced's content_seq > after latch (crates/aterm-core/src/terminal/observe.rs), and aterm-ctl's resolve_path/self_instance_sock order and exchange's connect-then-token-read (crates/aterm-ctl/src/lib.rs), by feat/handoff-survival on 2026-09-12 after the adversarial review of f6845c667; five slips fixed before this row (TURNED_AWAY had the `latest` race as token read before connect, but exchange connects first; ride_out said the probe goes through the same socket and that the default socket is the aterm.sock alias, where every aterm-ctl run re-resolves and prefers the instance hosting the calling terminal; moved_past said the successor does not reach a stale count for hours, softened to may not; a test doc gave relapses a RECONNECTED line they never print), and three links from public docs to private items (TURNED_AWAY, Session::unserved, spent_turn) made plain code spans so rustdoc gains no private_intra_doc_links warning; the rest of the file's prose unchanged since the round-4 read of 2026-09-12; 2026-09-12 (0.84 train): the supervisor's screen read was renamed from `read` to `screen` and given a doc comment saying why (the lock-order census identifies a lock by its name) — a method doc, not user-facing help; no other sentence moved; 2026-09-13 read of the prose feat/round-7-offscreen moved since the recorded read (shown by `xtask gate help-surfaces --diff`, matched at 68d1fb931) — SuperviseOpts::report's doc, Review::review's report argument, watch's --report sentence, brief_report's doc, ReportBrief, reported, reported_event_line and its `EVENT {} seq={} complete={} rows={} {}` line, the `report failed: {}` note, the mock's history/offscreen arms and the four new test docs — against look's `opts.report && reported(&point.phase)` gate (Idle, Question, Limited only; a prompt's EVENT and an APPROVED line unchanged), supervise's report:false override, brief_report's arms (Fail::Lost propagated to drive's ride_out; Fail::Hard noted and reported `complete=0 rows=0`, the loop kept), report.rs gather_report (`history 8`, then one `offscreen since=<mark>|tail=<n> max=<n> screen=1`, `text --json` on a host without the verb) and the request sequences the tests assert; no claim contradicted; 2026-09-13 read of the prose feat/round-8-survey moved (shown by `xtask gate help-surfaces --diff`, matched at 88afd4e09) — SuperviseOpts::dismiss_surveys, SURVEY_ROW, Looking.survey, Survey (Closed, Said, Tried), Pressing::Withheld, Dismiss (Pressed, Skipped, Unguarded), Session.survey_gone, and the look, survey, hand_survey, dismiss_survey, auto_read, press_one_guarded, stray_digit, typed_draft, survey_event_line and render_phase_and_survey docs, the four new notes (dismissed, handed to the manager, a backspaced 0, the fallback press withheld), the `DISMISSED survey seq={seq}` and `EVENT survey seq={seq} dismiss with: aterm ctl{target} key 'if={SURVEY_ROW}' 0` lines, `survey 0`, the mock's row_matcher-judged `key if=` and the survey tests' docs — against survey()'s order (the stray-0 check under Tried with a press, a Tried survey judged only on a look with no box and no draft unless it has gone, Said reset by a read that saw it gone, the press only from Closed with the survey open), screen()'s survey_gone latch on every read, dismiss_survey's caps.key_if probe (unknown_form to Unguarded, `ERR busy sink` retried, a request not served ridden out, any other ERR hard), the fallback's survey_open check on the confirming read before `key 1`, typed_draft over composer_draft and Screen::cursor_index, phase.rs survey_open/is_survey_question/is_survey_options/is_parked_above_composer, the server's input_if_row_matches (the guard tested against every visible row and the key written under one terminal lock, crates/aterm-gui/src/control_input.rs) and aterm_observe::row_matcher; zsh -f with extended_glob measured to refuse the unquoted `if=^●…` word as `no matches found`; three slips fixed before this row (SURVEY_ROW's and dismiss_survey's docs said no copy of the survey on the screen matches, where a worker message that opens with the question on a platform that draws the message glyph as `●` does — now said, the copies named as quoted ones; Dismiss::Skipped gave only the survey leaving first, where an open survey whose row the guard missed is skipped too; survey_event_line said a turn opening with any digit is a rating, where `0` dismisses); 2026-09-13 read (2026-09-14 UTC) of the prose feat/round-9-context moved (shown by `xtask gate help-surfaces --diff`, matched at 20cd46a07) — SuperviseOpts::context_warn, COMPACTED_RISE, Context (Armed, Warned), Session's context_warn and context fields, the await_turn_from, watch_context, supervise, watch, look and render_phase_and_survey docs, the `EVENT context seq={} {left}% until auto-compact`, `EVENT compacted seq={}` and `context {left}%` lines, and the docs of the ten context tests and their three helpers (with_context, descent, narrowed) — against watch_context (0 returns before a read is judged; Armed warns at left <= warn; Warned keeps the latest reading, and a None on a screen with has_composer_frame and no parse_prompt box, or a reading >= last + 30, says compacted and re-arms), await_turn_from's watch call only on a read before the deadline, await_turn's say that drops every line, drive arming context_warn and Context::Armed at each loop's start, StopAtReview::say (stderr) and Lines::say through emit (stdout, flushed), render_phase_and_survey's order (render_phase, survey 0, context last), phase.rs context_left/context_reading/is_against_right_edge/status_block (the zone from under the status row, or the last transcript row with none, to the top rule; a row ending within three columns of the bottom rule's width and starting at column 6 or later; the whole trimmed row the indicator, 0 to 100), drive_cli.rs parse_sub's --context-warn (watch and supervise only, 0 to 100) and DEFAULT_CONTEXT_WARN = 10, SuperviseOpts' derived Default (0) and parse_prompt (any row with `Esc to cancel`); four slips fixed before this row (await_turn_from's doc said every read is shown to the watch, where a read at or after the deadline is not; watch_context's said a read with a box up shows nothing either way, where only the indicator's absence is ignored under a box and a reading on it still counts; SuperviseOpts::context_warn's left the box out of what makes the indicator gone; a test doc said await-turn watches no indicator, where it runs the watch with a say that drops every line) and COMPACTED_RISE's unmeasured wobble claim replaced by the unmeasured `/model` case",
    ),
    (
        "crates/aterm-forge/src/budget.rs",
        "ec175d41d878d291",
        "2026-09-12",
        "re-read on 2026-09-12 against the code that moved under it since the roster was minted, by the 2026-09-12 drift sweep (lane aterm-forge); the package_dir mechanism sentence was false for a direct-path dep and is fixed here",
    ),
    (
        "crates/aterm-forge/src/attest.rs",
        "2f237c94d3892422",
        "2026-09-12",
        "re-read on 2026-09-12 against the code that moved under it since the roster was minted, by the 2026-09-12 drift sweep (lane aterm-forge); the [OB-1] exemption wrongly covered [OB-10] and is fixed here",
    ),
    (
        "crates/aterm-agent/src/fleet_cli.rs",
        "e20833cc05228f40",
        "2026-09-10",
        "read against its parser/dispatch by the 2026-09-10 sweep; findings fixed in 412cf3acd",
    ),
    (
        "crates/aterm-agent/src/lib.rs",
        "dab431a96e3635f2",
        "2026-09-14",
        "read crates/aterm-agent/src/lib.rs (DRIVE_HELP's await-turn timeout sentence, supervise's TIMEOUT sentence, watch's dedup and last-line paragraphs, and the rewritten --reconnect-s entry) against supervise/run.rs Session::call/unserved/ride_out (the outage-wide window from the first unserved request, one RECONNECT and one RECONNECTED per outage, 0.5 s doubling to 8 s across it, Rode::Spent → TIMEOUT, no such session not yet an answer only inside an outage, ERR exited always final), drive's fresh-only handed reset, moved_past's check read, the fallback press's Pressing::Lost handling and look's stray check, spent_turn/render_phase's `no screen` reason, drive_cli.rs main_entry (an Err exits 1) and await-turn's timed_out → 124, CtlReply::lost's TURNED_AWAY lines, and aterm-ctl's resolve_path (--sock, then $ATERM_CONTROL_SOCK, then self_instance_sock, then aterm.sock) and the `instances` verb's per-instance socket column (crates/aterm-ctl/src/lib.rs), by feat/handoff-survival on 2026-09-12 after the adversarial review of f6845c667; one slip fixed before this row (watch's dedup said the point is reprinted when the connection was lost, where the code resets on an outage's first answer, turned-away requests included — reworded to an outage came); the rest of DRIVE_HELP unchanged since the round-4 read of 2026-09-12; 2026-09-13 read of DRIVE_HELP's round-7 additions (shown by `xtask gate help-surfaces --diff`, matched at 718dda231) — the watch/report synopsis lines, watch's --report paragraph, the report entry and the two examples — against drive_cli.rs parse_sub (--since through Mark::parse, --max-rows > 0, --report) and its report arm (an Err exits 1), supervise/report.rs (newest_turn over `history 8`, find_start/is_user_row/user_text/paste_like/paste_fits with MARKER_CHARS 60 and PASTE_CHARS 200, offscreen_snapshot's since=/tail= max= screen=1 read, parse_offscreen's stderr header and its NoHeader fallback to `text --json`, join's back_at/pin skip of re-shown rows the read got, assess, gap_after's one-row recheck, finish's blank trim, Report::header/render), phase.rs transcript_end/is_done_row/is_parked_above_composer, run.rs reported/reported_event_line, and Turn::run (prompt types with send + key enter, so it leaves no ledger turn); two slips fixed before this row (main-screen said `the screen alone`, where the one offscreen read still joins the rows archived since the mark when the worker left the alternate screen — it now says what the reason means; complete=1's conditions left out the host keeping an archive, which no-archive refuses) and the reasons paragraph reflowed (one line had run to 109 columns); 2026-09-13 read of DRIVE_HELP's round-8 additions (shown by `xtask gate help-surfaces --diff`, matched at 547991d5b) — the supervise and watch synopsis `[--dismiss-surveys]`, phase's `survey 0` paragraph, await-turn's `exactly like phase`, supervise's withheld-fallback clause and survey sentence, watch's `EVENT survey` paragraph and the --dismiss-surveys entry — against drive_cli.rs parse_sub (--dismiss-surveys refused off watch and supervise) and phase_reply (phase and await-turn both render_phase_and_survey, 124 on a timed-out turn), phase.rs survey_open (the `●` question row in column 0 right over the options row, then only blank rows, right-aligned hints and `⎿  Tip:` rows down to the top rule, never without the composer frame), run.rs survey/hand_survey/dismiss_survey/typed_draft/survey_event_line and press_one_guarded's Withheld arm, StopAtReview::say (stderr) and Lines::say (stdout), and the server's input_if_row_matches (the check and the key under one terminal lock, `OK skipped` when no visible row matches; crates/aterm-gui/src/control_input.rs); three slips fixed before this row (the guard was said to let no copy of the survey on the screen through, where only a quoted copy — indented, or under `⎿` — is sure not to match; supervise's survey sentence said --dismiss-surveys dismisses it and says DISMISSED, where the line comes only once a fresh read shows it gone; a `0` found in the composer was put down to the survey leaving first alone, where the code backspaces one an open survey did not take as well); 2026-09-13 read (2026-09-14 UTC) of DRIVE_HELP's round-9 additions (shown by `xtask gate help-surfaces --diff`, matched at 20cd46a07) — the supervise and watch synopsis `[--context-warn PCT]`, phase's `context <n>%` paragraph, await-turn's `survey 0` and `context <n>%` clause (the rest of that paragraph reflowed, its words unchanged), supervise's stderr sentence, watch's `EVENT context`/`EVENT compacted` paragraph and the --context-warn entry — against drive_cli.rs parse_sub (--context-warn refused off watch and supervise, a value 0 to 100) and DEFAULT_CONTEXT_WARN, phase_reply (phase and await-turn both render_phase_and_survey, context after survey 0), phase.rs context_left (status_block's from to the top rule, is_against_right_edge, context_reading's two spellings), run.rs watch_context (the inclusive threshold, the frame-and-no-box rule for gone, the 30-point rise, the re-arm), await_turn_from's per-read call, drive's arming at each run's start, StopAtReview::say (stderr), Lines::say (stdout) and the tests supervise_watches_one_run and the_context_watch_is_inclusive_and_needs_the_frame; one slip fixed before this row (phase's paragraph put the indicator under the status row alone, where with no status row it is read under the last transcript row)",
    ),
    (
        "crates/aterm-cli/src/lib.rs",
        "a0a478ffae64bb9c",
        "2026-09-12",
        "re-read on 2026-09-12 against the code that moved under it since the roster was minted, by the 2026-09-12 drift sweep (lane small-1); no claim contradicted the moved code",
    ),
    (
        "crates/aterm-cli/src/manual.rs",
        "91a87a48988c910c",
        "2026-09-14",
        "read crates/aterm-cli/src/manual.rs (DRIVE_PAGE's await-turn and supervise timeout clauses, watch's dedup and EXIT list, and the rewritten --reconnect-s entry) against aterm-agent supervise/run.rs Session::call/unserved/ride_out (one outage window, lines once per outage, Rode::Spent → TIMEOUT, no such session not yet an answer inside an outage), drive's fresh-only handed reset, moved_past's check read, spent_turn, CtlReply::lost's TURNED_AWAY lines, drive_cli.rs main_entry's exit 1, and aterm-ctl's resolve_path order (crates/aterm-ctl/src/lib.rs), by feat/handoff-survival on 2026-09-12 after the adversarial review of f6845c667; no slip found in this pass; every other page unchanged since the 2026-09-11 read; AND read crates/aterm-cli/src/manual.rs (DRIVE_PAGE's phase, await-turn, supervise and watch entries) against aterm-agent supervise/phase.rs worker_phase/busy_signal/limit_notice, supervise/run.rs await_turn_from/drive/review_key/watch and drive_cli.rs main_entry/watch_exit_line, by aterm-supervise-round-4 on 2026-09-12 after the round-4 review; one slip (the dedup not naming the reset on an approval) fixed before this row; every other page unchanged since the 2026-09-11 read; 2026-09-12 re-read of the one sentence added since (a launch pass that finds another aterm's install in flight waits for it, then runs, and never reports it as a failure) against spawn_pkg_update_check's --wait-lock / ATPKG_WAIT_LOCK_SECS path, the Wake::PkgLockWaiting info row that is never the failure bar, and contention_is_busy_never_refused_regression_2026_09_10; the merged file is exactly those two read changes together (the drive page from feat/handoff-survival, the launch-wait sentence from main), recorded at the merge; 2026-09-13 read of DRIVE_PAGE's round-7 additions (shown by `xtask gate help-surfaces --diff`, matched at e2ae167f2) — watch's [--report] and its --report sentence, and the report entry — against the code crates/aterm-agent/src/lib.rs's row names (drive_cli.rs parse_sub and the report arm, supervise/report.rs gather_report/newest_turn/find_start/join/assess/Report::header, phase.rs transcript_end, run.rs reported_event_line); one slip fixed before this row (`main-screen or no-archive (the screen alone)`: main-screen still carries the rows archived since the mark when the worker left the alternate screen — it now says the main screen's scrollback is not read) and the header synopsis reflowed; 2026-09-13 merge of main into feat/round-7-offscreen: main re-read this page on 2026-09-13 (`aterm help fabric` carried three false fabric claims: post refuses with no bridge, and the halt shape) and fixed it; `gate help-surfaces --diff` at the merge shows the merged prose is exactly round 7's read DRIVE_PAGE additions plus main's three fabric corrections (a post that waits is refused no-bridge=1, others queue; ERR halted reason= origin=), spot-checked against fabric.rs fabric_wait_refusal; recorded at the merge; 2026-09-13 read of DRIVE_PAGE's round-8 additions (shown by `xtask gate help-surfaces --diff`, matched at bd8f70ca9) — phase's `survey 0` sentence, await-turn's `survey 0` clause, supervise's `[--dismiss-surveys]`, withheld-fallback clause and survey sentence, watch's `EVENT survey` sentence and the --dismiss-surveys entry — against the code crates/aterm-agent/src/lib.rs's round-8 read names (drive_cli.rs parse_sub and phase_reply, phase.rs survey_open, run.rs survey/dismiss_survey/typed_draft/survey_event_line and press_one_guarded's Withheld arm, the server's input_if_row_matches); three slips fixed before this row (a copy in the transcript narrowed to a quoted one; the fallback was said never to press while the survey is open, where it checks the confirming read — reworded, the box named as yours; the --dismiss-surveys entry implied a DISMISSED line and a note for a skipped `0`, which gets neither) and await-turn's clause reflowed; 2026-09-13 read (2026-09-14 UTC) of DRIVE_PAGE's round-9 additions (shown by `xtask gate help-surfaces --diff`, matched at 20cd46a07) — phase's `context <n>%` sentence, await-turn's clause, supervise's and watch's `[--context-warn PCT]` and their EVENT context/compacted sentences, and the --context-warn entry — against the code crates/aterm-agent/src/lib.rs's round-9 read names (drive_cli.rs parse_sub, DEFAULT_CONTEXT_WARN and phase_reply, phase.rs context_left, run.rs watch_context/await_turn_from/drive and the two Review says); one slip fixed before this row (phase's sentence called `context <n>%` a last line right after `survey 0`'s last line — it now says it comes after any `survey 0`) and await-turn's clause reflowed; 2026-09-13 read of the fabric-page prose moved since that read (shown by `xtask gate help-surfaces --diff`, matched at cc86c5cfe91c) — FABRIC_PAGE's new A WAIT THAT DOES NOT LAND block, the rewritten `connected` and `disconnected` rows of IS IT ON HERE?, the `aterm-link` to `aterm link` respellings (hook install, the mirror synopsis, the fabric_page doc) and the test needles that pin them — against aterm-gui fabric.rs cmd_post (its option grammar is to/kind/re/dl/via/len/--wait and PostRow keeps no caller-chosen key, so `post` really carries no idempotency key and the broker's (producer_id, producer_seq) dedup cannot collapse a re-post), fabric_wait_refusal over bridge_reachable (queued=1 while a supervisor is armed or a bridge has ever attached, no-bridge=1 otherwise) and the wait loop's deadline arm (`ERR timeout id=<n>`), cmd_outbox's PEEK with control.rs cmd_fabric_attach → fabric_launch::arm/note_bridge_supervised/supervise (a post refused no-bridge=1 is still in `posts`, the attach arms a supervisor that relaunches for ever, and the next bridge drains that same queue — so `aterm ctl fabric attach <command...>` does drain the same outbox, and `fabric` is a Meta/Owner verb so no selector is needed), control.rs serve_bridge's bridge_attached (connected is stored when the bridge's INHERITED fd starts being served, before any broker contact) and bridge_lost (disconnected plus the fabric-lost hold only when a bridge fd closes) with aterm-link bridge.rs Bridge::run (an unreachable broker is retried RECONNECT_MIN..RECONNECT_MAX for ever and the bridge exits only when aterm closes, so a bridge aimed at a socket that does not exist reports connected and killing the BROKER cannot move the state off connected — both claims true), and the respellings against aterm/src/main.rs Verb::Link → aterm_link::cli::dispatch's hook/mirror arms, hook.rs install_claude (four hooks — SessionStart, UserPromptSubmit, PreToolUse, Stop — into .claude/settings.json) and mirror.rs (`<root> --sock <path>`, the four inbox/outbox/sent/.cursor files, POLL_DEFAULT 250 ms), with aterm-release bundle.rs's alias list showing why the verb spelling is the safe one (aterm-link was only just added to the shipped argv0 symlinks); ONE CONTRADICTION, left for the orchestrator: the block's header says a wait that does not land has THREE answers and that ALL THREE MEAN QUEUED, where cmd_post has a FOURTH — a post the bridge retires with `outbox sent <sid> <id> off=- reason=<why>` (bridge.rs Route::reason gives `unroutable` and `ambiguous`, the endpoint's DEAD_DEFAULT is `undeliverable`) wakes the parked wait through retire_post's notify_all and answers `ERR <reason> id=<n>`, and that is the one answer that does NOT mean queued: the row is dead, cmd_outbox filters `!p.dead` so no bridge ever drains it again, and an agent applying the page's rule would report a message nothing will publish as queued. THE CONTRADICTION THAT READ FOUND WAS FIXED IN THIS COMMIT, NOT RECORDED AROUND: the block claimed a wait that does not land has THREE answers and that all three mean queued, and `cmd_post`'s wait loop has a FOURTH — a row the bridge RETIRED (`outbox sent <sid> <id> off=- reason=<why>`; bridge.rs Route::reason gives `unroutable`/`ambiguous`, the endpoint default is `undeliverable`) releases the wait with `ERR <reason> id=<n>`, and `cmd_outbox` filters `!p.dead` so nothing drains it again — the one answer that is NOT queued, which an agent obeying the page would have reported as queued. The page now says FOUR ANSWERS, AND THREE OF THEM MEAN QUEUED, names the fourth and what to do about it, and the test's needle list and its `THREE OF THEM MEAN QUEUED` assertion pin it; the hash recorded here is the CORRECTED page",
    ),
    (
        "crates/aterm-cli/src/windowing.rs",
        "fd8be119f5c354f8",
        "2026-09-10",
        "read against its parser/dispatch by the 2026-09-10 sweep; findings fixed in 412cf3acd",
    ),
    (
        "crates/aterm-control/src/selection.rs",
        "7269abf2d2722432",
        "2026-09-10",
        "read against its parser/dispatch by the 2026-09-10 sweep (aterm-B-window-and-clients); findings fixed in the commit that added this row",
    ),
    (
        "crates/aterm-ctl/src/conn.rs",
        "90ea0edfcaad43d2",
        "2026-09-10",
        "read against its parser/dispatch by the 2026-09-10 sweep (aterm-B-window-and-clients); findings fixed in the commit that added this row",
    ),
    (
        "crates/aterm-ctl/src/lib.rs",
        "e69b808bb02c752a",
        "2026-09-13",
        "re-read on 2026-09-12 against the code that moved under it since the roster was minted, by the 2026-09-12 drift sweep (lane small-1); the cell attribute roster omitted `hidden` and is fixed here; the 0.84 train's release-candidate merge (this machine's provenance/repair/TCC work over the peers' 2026-09-12 re-read at the tip) moved the bytes once more — both halves were read by their authors as recorded, the merged file is their union, and this row records the union's hash; 2026-09-13 read of the prose feat/round-7-offscreen moved (shown by `xtask gate help-surfaces --diff`, matched at 550ace022) — `offscreen` joining `inbox` as the verbs whose `OK <n> …` header goes to stderr with stdout kept as rows, and the two `offscreen` cases in streams_payload_gates_by_verb_and_image_read — against exchange's header branch, stderr_line's `aterm-ctl: ` prefix, streams_payload → control_verbs::framing_of (offscreen's catalog row is Lines) and aterm-agent report.rs parse_offscreen, which reads that stderr header (CtlClient shells out to this client); no claim contradicted",
    ),
    (
        "crates/aterm-dev/src/main.rs",
        "e81d6e1a4ed79f2f",
        "2026-09-10",
        "read against its parser/dispatch by the 2026-09-10 sweep; findings fixed in 412cf3acd",
    ),
    (
        "crates/aterm-forge/src/cli.rs",
        "082ffd0b7dab9c05",
        "2026-09-12",
        "re-read on 2026-09-12 against the code that moved under it since the roster was minted, by the 2026-09-12 drift sweep (lane aterm-forge); no claim contradicted the moved code",
    ),
    (
        "crates/aterm-forge/src/lib.rs",
        "ac6752148aef0c5f",
        "2026-09-12",
        "re-read on 2026-09-12 against the code that moved under it since the roster was minted, by the 2026-09-12 drift sweep (lane aterm-forge); the astream-classifier figures and the inclusion claim were false and are fixed here",
    ),
    (
        "crates/aterm-gui/src/cli.rs",
        "c2463a7eb85e25a7",
        "2026-09-14",
        "re-read on 2026-09-12 against the code that moved under it since the roster was minted, by the 2026-09-12 drift sweep (lane aterm-gui-2); no claim contradicted the moved code; 2026-09-13 leftovers lane (89e3292d1, shown by `xtask gate help-surfaces --diff`: 1 hunk, 2 template lines rewritten as 5): the STARTER_CONFIG comments on `allow_notifications` (OSC 9/99/777; macOS delivers through terminal-notifier if installed, else osascript, a subprocess under aterm's identity) read against notify.rs deliver (Command terminal-notifier .status(), Err → the osascript `display notification` fallback; both subprocesses of aterm) and `allow_osc52_query` (a program's OSC 52 READ, answered only when on; on macOS 26 that read raises the system's \"aterm would like to paste from …\" alert) read against spawn.rs's ClipboardOperation::Query arm (reached only through a minted ClipboardQueryCapability, answers control::pbpaste — an in-process NSPasteboard read, clipboard.rs pbpaste); no claim contradicted; 2026-09-13 release-candidate read of the ONE prose line 47d9a1560 moved (the rainbow-gap merge, arriving on origin/main): the sample config's `cursor_trail_intensity` comment now reads `1.0` where it read `0.7`, and the comment states a DEFAULT, so it was checked against the code that supplies one — app_config.rs `cursor_trail_intensity_or_default` is `self.cursor_trail_intensity.unwrap_or(1.0)`, clamped to 0.0..=1.0 with a non-finite value failing OFF to 0.0, so the new literal is the default an unset key really gets and the old one had rotted; the remaining `intensity: 0.7` in this crate is a `resolve_cursor_glow` test fixture, not a default; the stated range `0.0..=1.0` still matches the clamp; 2026-09-13 read of the ONE line e61ee4b2b moved (the September 13 integration with the comet and the vivid rail): the `cursor_trail_style` roster in the sample config gained `rainbow kitty flat (the A/B control: the flat body of 2026-09-13, before the comet body and its vivid rail; aliases \"rainbow flat\"/\"flat rainbow\"/\"nyan flat\")`, checked against prefs.rs — the canonical name is in the style roster, the three aliases map to it in the alias table, and it resolves in the style match — so every spelling the line offers is one a config can actually set; no other style row changed",
    ),
    (
        "crates/aterm-gui/src/control.rs",
        "7464b4ce74ffa2d7",
        "2026-09-14",
        "re-read on 2026-09-12 against the code that moved under it since the roster was minted, by the 2026-09-12 drift sweep (lane aterm-gui-1); no claim contradicted the moved code; re-hashed because its prose moved upstream after the row was minted; 2026-09-13 read of the prose ca54aaadd moved since the recorded read (shown by `xtask gate help-surfaces --diff`, matched at 0f7667e8a) — rainbow_kitty_window's new doc (a REAL pipe whose read end is returned and must be held; a key whose write fails is revoked at the seam, stamp AND press credit) and its `pipe(2)` assert message, both test-only — against the fixture body (libc::pipe, SinkWriter::new over the write end, the read end returned as a File), app_input.rs's InputOutcome::WriteFailed arm calling CursorGlow::revoke_failed_input_at, and that fn's revoke_input_hints_at (type_hint.revoke_at) plus type_press_ring.revoke_at (crates/aterm-effects/src/cursor_glow.rs); no claim contradicted; 2026-09-13 read of the prose feat/round-7-offscreen moved (shown by `xtask gate help-surfaces --diff`, matched at ca54aaadd) — the `offscreen` router arm on the target term, its json_unsupported and read-edge test-set entries, and the docs and strings of paint_alt_frame, history_prints_the_turn_start_arch_mark_before_text and offscreen_refuses_json_honestly — against control_query::cmd_offscreen, control_session::cmd_turn's ArchMark::of stamp before the yield, cmd_history's `arch=` before `text=`, take_json_flag/JSON_CAPABLE_VERBS and the catalog row's Read/Session classing; no claim contradicted; 2026-09-13 release-candidate read of the prose 0720efafb moved (`fix(control): hidden and cross-session paste, paste-bin, feed and send replies report a failed write instead of an unconditional OK`, on origin/main, which changed this file's help/doc text WITHOUT re-recording this row — origin/main is red on it at c64956327..18f19eea6 + the four commits after, and the row is recorded here at the merge that carries it): the removed unconditional `OK\n` replies and the added `ERR write failed\n` arms, the new `background_reply_outcome` doc, the `cross_raw_input` return-the-verdict sentence and the reply-fidelity paragraph, read against control.rs `cross_input` (control thread, `tracked_egress` at `EgressMode::Backpressured`, `background_reply_outcome(&ctx.sink, receipt, paste)` where `paste` is `matches!(ev, InputEvent::Paste(..))` — so only a paste asks for the kernel receipt), `cross_raw_input` (same seam, returns `egress_to_outcome(receipt.egress)`), `background_reply_outcome` itself (`kernel_receipt && outcome == Ok && !sink.wait_egress_drained_to_kernel()` -> `WriteFailed`), and aterm-session sink.rs `wait_egress_drained_to_kernel` (parks on the `drained` condvar while `draining && !buf.is_empty()`, then answers `buf.is_empty() && !failed` — the sticky `failed` flag origin/main added is what keeps a spill the drainer DISCARDED after a dead peer from reading as a successful drain, which is exactly the `never a false OK` claim); no claim contradicted; and origin/main's own read of the same change, at the same hash, recorded in parallel: ad (2026-09-14 UTC) at the merge of feat/round-9-context over main: the prose moved only by 0720efafb (hidden and cross-session paste, paste-bin, feed and send reply a failed write instead of an unconditional OK) — the two removed unconditional OK replies and the new test strings (dead-peer paste-bin/feed-bin/send/key answer ERR write failed, never a false OK, never touch the front session, the next request stays framed) read against control.rs's InputOutcome::WriteFailed and Egress::Reported(Delivery::Failed) arms and control_input.rs's GuardedInput::Pressed(Delivery::Failed); no slip found",
    ),
    (
        "crates/aterm-gui/src/control_input.rs",
        "b2dbb205d461aece",
        "2026-09-11",
        "re-read against control.rs's cross-session `key` arms, post_input_reply and lib.rs's Wake::Input arm, input_if_row_matches, pty_idem KEYED_VERBS, cmd_scroll and parse_tab by the 2026-09-10 round-3 reader after the round-2 merge; the cross-session `key` usage (code, control.rs, pinned by a test), the `mouse` fire-and-forget claim, the `sole encoder caller` claim, GUARDED_VERBS' `no id=` claim, the `hello id=1` count, and the `scroll`/`tab` grammar lines fixed in the commit that updated this row; parse_tab's doc sentence still omits `close`/`move`; merged with origin/main's own 2026-09-11 re-read of this file, conflicts resolved by the orchestrator (upstream wording kept where both sides fixed the same claim)",
    ),
    (
        "crates/aterm-gui/src/control_media.rs",
        "b360ecc1931515c4",
        "2026-09-13",
        "re-read on 2026-09-13 by drift sweep of 2026-09-13 (aterm:aterm-gui); the Screen Recording contrast, the ERR enumeration and two trail rosters were stale and are fixed in that commit",
    ),
    (
        "crates/aterm-gui/src/control_privacy.rs",
        "6f59cf99e9966e3f",
        "2026-09-13",
        "the row hashed a checkout behind 0a8275760/acc20c5fd/da5d75e33; re-read against parse_consent_timeout, covers_split and PrivacySnapshot::lines by the 2026-09-10 round-3 reader, the SERVICES doc (`never uncovered` vs NEVER_COVERED) fixed in 33311eda0; re-read against observer_fda_value/inert_fda_probe, ConsentState::fda -> ConsentCache::get_or_probe, app_settings.rs macos_access_projection and consent_policy by the round-3 fixer; the `live` field's `observer row's third value` claim (the row renders the inert labels as `off`, pinned at the fda=off/responsible=off asserts), the consent_panel_facts/session_consent `no syscall` claims (the cached probe's one open(TCC.db) on a miss), ConsentPanelFacts' `no covers= list` claim (the panel takes covers_split directly) and the consent_policy doc line displaced onto consent_probe_interval fixed in the commit that updated this row; the catalog's `covers=`/`uncovered=` split in control_verbs.rs still omits `unmeasured=` — that surface's own row; NOTE's doc now says its `covers is empty` clause is written for the UNMEASURED evidence read_privacy hard-codes and must turn evidence-conditional when §7 S4 lands (the measured arm of lines, tests-only today, renders a covers= list above it), the one-line fix for the round-3 reader's note finding; merged with origin/main's own 2026-09-11 re-read of this file, conflicts resolved by the orchestrator (upstream wording kept where both sides fixed the same claim); 2026-09-13 read of the one prose change since (instance_attribution's doc, shown by `xtask gate help-surfaces --diff`) against instance_attribution (Attribution::Adopted iff App::handoff_successor, else Live; ConsentPolicy::adoption maps it to Unknown only when the policy is disabled), main_entry's `handoff_successor: adopting` (true whenever the boot re-adopted handed-off shells) and app_restore.rs take_session0_shell (None, so a fresh session 0, when window 0's layout has no terminal leaf, which leaves handoff_successor true); no claim contradicted",
    ),
    (
        "crates/aterm-gui/src/control_query.rs",
        "b2343fe27d084882",
        "2026-09-14",
        "the row predates ffda97813 (`text tail=`/`rows=`), which the round-2 merge carried; re-read against cmd_modes, cmd_cell, cmd_metrics/cmd_metrics_json by the 2026-09-10 round-3 reader and against text_args/TextShape::select/frame_rows_reply/cmd_text_opt by the round-3 fixer (that delta matches its code); the `modes` frame (`OK <n>` and twelve keys, not `OK` and seven — pinned by modes_frames_its_count_and_twelve_keys), the `cell` attrs vocabulary (`wide`/`wide_cont`, pinned by cell_attrs_carry_the_width_markers) and the JSON `percentiles` doc (it now names the reflow quartet the text form carries and the JSON body omits; the code gap stands) fixed in the commit that updated this row; merged with origin/main's own 2026-09-11 re-read of this file, conflicts resolved by the orchestrator (upstream wording kept where both sides fixed the same claim); 2026-09-13 read of the `offscreen` prose feat/round-7-offscreen added (shown by `xtask gate help-surfaces --diff`, matched at a94e1bc9b) — OFFSCREEN_USAGE/OFFSCREEN_BAD_SINCE/OFFSCREEN_DEFAULT_MAX, OffscreenSince/OffscreenArgs, offscreen_number, offscreen_args, cmd_offscreen and format_offscreen_reply, the header format strings, and the offscreen_tests/offscreen_wire_tests docs — against the parser (each key once, ASCII digits only, tail/max > 0, screen=1 only), cmd_offscreen's other-origin/bad-since arms and its one-lock clone-out, format_offscreen_reply's field order (back_at/pin only when back > 0, enabled=0 only when off, last= the page's own last row), aterm-core alt_archive.rs AltArchive::read/page_last/set_budget/set_enabled/wipe and AltArchiveRead's fields, and env_opted_out; one slip fixed before this row (OFFSCREEN_DEFAULT_MAX said a bare poll never ships a whole archive, false for one under 2000 rows — it now says at most that many rows); 2026-09-13 release-candidate merge of the perf port: the ONE prose change since that read is the per-site UI-thread terminal-mutex wait fragment the port added (`text_term_wait_fields` and its JSON twin `json_term_wait_fields`, plus the two `metrics` format strings that carry them), read here against crates/aterm-gui/src/metrics.rs TermWaitSite (a THREE-member enum RedrawA/RedrawB/Press with `ALL` a const array of all three and `label()` a const match to `redraw_a`/`redraw_b`/`press` — so the doc's `empty never (the sites are static)` holds and the report order is the doc's order), term_wait_distribution/term_wait_max_ns, and the two builders themselves: the text form writes a LEADING-SPACE run ` n_term_wait_<site>=<count> term_wait_<site>_p50_ms= _p95_ms= _p99_ms= max_term_wait_<site>_ms=` per site and the JSON twin the same fields LEADING-COMMA and quoted, each from the same histogram and the same `term_wait_max_ns`, appended at the matching `{}` of the text and JSON percentile lines; no claim contradicted",
    ),
    (
        "crates/aterm-gui/src/control_session.rs",
        "88feeb25b3c90527",
        "2026-09-13",
        "re-read on 2026-09-12 against the code that moved under it since the roster was minted, by the 2026-09-12 drift sweep (lane aterm-gui-1); no claim contradicted the moved code; 2026-09-13 read of the prose feat/round-7-offscreen moved (shown by `xtask gate help-surfaces --diff`, matched at c811b7c5a) — the history record's `arch={}` field and cmd_history's doc — against cmd_history's format (arch= before text=), turn_ledger.rs ArchMark (Display `<origin>:<last>`, ArchMark::of from the term's archive) stamped in cmd_turn after preflight and before the yield, control_query::offscreen_args's since=<origin>:<i>, and the readers that cut a row at ` text=` (aterm-link hook.rs strip_body, aterm-agent report.rs parse_turn_line); no claim contradicted",
    ),
    (
        "crates/aterm-gui/src/fabric.rs",
        "95452948449e8768",
        "2026-09-12",
        "re-read on 2026-09-12 against the code that moved under it since the roster was minted, by the 2026-09-12 drift sweep (lane aterm-gui-2); the note_bridge_supervised caller claim was falsified by `fabric attach` and is fixed here",
    ),
    (
        "crates/aterm-gui/src/hwkey.rs",
        "b2436705411d74a4",
        "2026-09-12",
        "re-read on 2026-09-12 against the code that moved under it since the roster was minted, by the 2026-09-12 drift sweep (lane aterm-gui-2); the main-thread-park claim was falsified by the off-thread drawable and is fixed here",
    ),
    (
        "crates/aterm-gui/src/menu.rs",
        "bad7bda51956a5f9",
        "2026-09-10",
        "read against its parser/dispatch by the 2026-09-10 sweep (aterm-B-window-and-clients); findings fixed in the commit that added this row",
    ),
    (
        "crates/aterm-gui/src/operator_host.rs",
        "f4682d40f4cdb0f4",
        "2026-09-10",
        "read against its parser/dispatch by the 2026-09-10 sweep (aterm-A-ctl-verb-usages); findings fixed in the commit that added this row",
    ),
    (
        "crates/aterm-gui/src/pty_idem.rs",
        "4ac6ab2f17c9cd57",
        "2026-09-11",
        "read pty_idem.rs (module/Realm/PRODUCER_CAP/dup_reply/guarded/record_in_doubt docs, USAGE, KEYED_VERBS, test docs) against control.rs dispatch + run_feed_bin_routed, control_input.rs guarded press/take_leading_options, control_session.rs cmd_turn_guarded, control_verbs.rs catalog + framing_of, aterm-ctl stream_count/malformed header, aterm-link bridge feed key by workflow aterm-help-surfaces-read on 2026-09-10; 4 low findings left; merged with origin/main's own 2026-09-11 re-read of this file, conflicts resolved by the orchestrator (upstream wording kept where both sides fixed the same claim)",
    ),
    (
        "crates/aterm-link/src/cli.rs",
        "b8d74e7ea510e151",
        "2026-09-13",
        "re-read on 2026-09-12 against the code that moved under it since the roster was minted, by the 2026-09-12 drift sweep (lane aterm-link); no claim contradicted the moved code; the 0.84 train's release-candidate merge (this machine's provenance/repair/TCC work over the peers' 2026-09-12 re-read at the tip) moved the bytes once more — both halves were read by their authors as recorded, the merged file is their union, and this row records the union's hash",
    ),
    (
        "crates/aterm-link/src/hook.rs",
        "39776e013e4484f8",
        "2026-09-12",
        "re-read on 2026-09-12 against the code that moved under it since the roster was minted, by the 2026-09-12 drift sweep (lane aterm-link); the fleet-origin and human-words claims were falsified by `hold` becoming OwnerOnly and are fixed here",
    ),
    (
        "crates/aterm-link/src/mirror.rs",
        "e0c1f9f7a10f2426",
        "2026-09-12",
        "re-read on 2026-09-12 against the code that moved under it since the roster was minted, by the 2026-09-12 drift sweep (lane aterm-link); no claim contradicted the moved code",
    ),
    (
        "crates/aterm-link/src/notify.rs",
        "887108b75779e9d3",
        "2026-09-10",
        "read against its parser/dispatch by the 2026-09-10 sweep (aterm-C-link-forge-verify); findings fixed in the commit that added this row",
    ),
    (
        "crates/aterm-link/src/tui.rs",
        "24f9b1b08fee982e",
        "2026-09-12",
        "re-read on 2026-09-12 against the code that moved under it since the roster was minted, by the 2026-09-12 drift sweep (lane aterm-link); no claim contradicted the moved code",
    ),
    (
        "crates/aterm-nest/src/main.rs",
        "50a498270b151279",
        "2026-09-10",
        "read against its parser/dispatch by the 2026-09-10 sweep (aterm-B-window-and-clients); findings fixed in the commit that added this row",
    ),
    (
        "crates/aterm-primer/src/lib.rs",
        "efac73a871897d95",
        "2026-09-14",
        "re-read on 2026-09-13; FABRIC_NOTE and its doc carried the same false `post refuses` claim as the assets and are fixed in that commit; 2026-09-13 read of the prose 9758cd022 moved since that read (shown by `xtask gate help-surfaces --diff`, matched at 2493a60a7) — FABRIC_NOTE's rewritten `fabric=absent` sentence and the whole-block budget test's re-measured doc comment — against aterm-gui fabric.rs cmd_post (the row pushed into `posts` before any wait and `OK <id>` returned when `wait` is None, `--wait` ON by default only for `ask`/`task` via `matches!(kind, ask|task)`, and the option tokens to/kind/re/dl/via/len/--wait, none of them a caller-supplied dedup key, so the no-idempotency-key clause holds), fabric_wait_refusal/bridge_reachable/fabric_state, the wait loop's `ERR timeout id={id}` return, and Inbox::trim_retired_posts (evicts only rows with `off=` or `dead`, so a timed-out post is still queued), cross-checked against control_verbs.rs's `post` row and manual.rs's fabric page (all three not-landed answers mean queued; `aterm link mirror` is the file mirror the note points at); the budget doc's figures were MEASURED by building `primer_block` out of tree rather than recalled — codex 4354 and the other three 3564 against the test's `widest <= 4_400`, and 4297/3507 before the change — which confirms 4354/3564 but contradicts the doc's Fifty-four bytes on every agent's every turn: 54 is the overshoot past the retired 4_300 cap, while the recurring price is 57 (FABRIC_NOTE 1119 -> 1176 bytes, every agent's block +57), and the earlier raise from a guessed 4200 appears nowhere in this file's history (the cap was born 4_300 at 273a1d56d), both left for the orchestrator; the agent-facing sentence itself is sound except that `fabric=absent` alone does not imply `no-bridge=1` (fabric_wait_refusal answers `queued=1` while a supervisor is armed and the first bridge is still attaching) and an explicit `--wait` takes the same refusal on any kind, not only `ask`/`task` — both narrowings inherited from the sentence it replaced THE CONTRADICTION THAT READ FOUND WAS FIXED IN THIS COMMIT: the budget test's doc priced the addition at `Fifty-four bytes on every agent's every turn`, and the measured per-turn price is FIFTY-SEVEN (FABRIC_NOTE 1119 -> 1176, codex 4297 -> 4354, the other three 3507 -> 3564); 54 is the new widest block's overshoot past the RETIRED 4300 cap, a different quantity. The doc now states the measurement and says which slip it was. The unverifiable clause about `a guessed 4200` (no such cap literal exists in this file's history — the test was born at 4_300 and has been raised once, to 4_400) was removed with it. The hash recorded here is the CORRECTED doc",
    ),
    (
        "crates/aterm-release/src/cli.rs",
        "a69416b748ec8eee",
        "2026-09-13",
        "read against its parser/dispatch by the 2026-09-10 sweep; findings fixed in 412cf3acd; 2026-09-13 leftovers lane (8035c0c56, shown by `xtask gate help-surfaces --diff`: 4 hunks, 1 line removed, 17 added): the provision usage's `[--cert-dir <folder>]` and its paragraph (the request is copied there for the upload dialog, the .cer looked for there and in ~/.aterm/apple; without it every read stays inside ~/.aterm/apple; naming one is the consent and a note says so before the errand) read against apple.rs Watch::new/dirs (named first, then ~/.aterm/apple), find_matching_cert (reads only Watch::dirs), surface_csr (called only with the named folder, await_then_install's `.and_then(|named| surface_csr(…))`) and Watch::consent_note (printed before errand_lines when the named folder is under a protected root); the four parse refusals (`--cert-dir given twice`, `needs a folder`, `not an empty string`, the flag itself) read against parse's \"--cert-dir\" arm; the Cmd::Provision cert_dir field doc read against the same; no claim contradicted",
    ),
    (
        "crates/aterm-types/src/control_verbs.rs",
        "7da458b969decaab",
        "2026-09-14",
        "catalog read in full against the handlers by aterm-help-surfaces-read on 2026-09-10; the fabric row and the reworded hold row read against dispatch_fabric_verb / dispatch_hold_verb and the access-set pins by verify:F1 and verify:F2 of aterm-fabric-attach-round-3 on 2026-09-10; goldens regenerated as a pair from the merged catalog on 2026-09-11; merged with origin/main's own 2026-09-11 re-read of this file, conflicts resolved by the orchestrator (upstream wording kept where both sides fixed the same claim). 2026-09-12: the ONE prose change since that read is SHORT_CATALOG_MAX_BYTES' raise paragraph, read in full and MEASURED rather than recalled — aterm-gui's bare cmd_help is 9 610 B against the new 9 728 ceiling, VERBS is 101 rows, the summary rows alone are 9 238 B; the fabric, fx and hold rows and the OpClass/Access/Framing/Target docs re-read against dispatch_fabric_verb, cmd_fabric/cmd_fabric_attach/fabric_status_line, dispatch_hold_verb/fabric::cmd_hold and the access-set pin, the other 98 catalog rows sampled. One contradicted claim fixed in the commit that updated this row — Access::OwnerOnly called `hold` the ONE member whose handler tells the two owner-class scopes apart, and `fabric`'s handler is a second (Scope::Owner exactly, the bridge refused), as aterm-gui's own is_owner_class doc already records; 2026-09-13 (UTC; 2026-09-12 local) re-read of the trail row's paste-sweep additions (the licence= classes, program-row, the inserts_* status keys, which inputs stamp an insert and which stay dark, and the `inserts_delivered>0 inserts_lit=0` reading after a drop) against aterm-effects cursor_glow.rs AdmissionRecord::line/LICENCE_*, move_licensed, insert_echo/insert_rewrite/INSERT_HINT_FRESH, note_insert_delivered/lay_insert/retract_insert/insert_tally and TrailStatus::line, app_render.rs tick_cursor_fx's Rainbow-Kitty-gated delivery prelude, app_input.rs input_paste and insert_gesture_armed, lib.rs drop_file/deliver_paste, and control.rs's flagless, front-routed, background, guarded and run_feed_bin_routed paste-bin routes, fixing the contradictions in the row (paste-bin into the tab on screen stamps an insert, not nothing; the insert class and counters are Rainbow Kitty only; `A move paints only if a keypress LICENSED it` now admits the delivered insert; the key class gains the composer newline and ⌃V's gesture; inserts_delivered counts a bare Tab and ⌃V too; last_insert_cells is the 32-cell bound when unpriced; the drop reading covers an echo never seen inside the 2 s window) and regenerating both goldens as a pair; the 0.84 train's release-candidate merge (this machine's provenance/repair/TCC work over the peers' 2026-09-12 re-read at the tip) moved the bytes once more — both halves were read by their authors as recorded, the merged file is their union, and this row records the union's hash; 2026-09-13 read of the prose ca54aaadd moved since the recorded read (shown by `xtask gate help-surfaces --diff`, matched at 0f7667e8a) — the trail row's `trail status` key list gaining `park_returns= park_flushed=` and its new HELD PARKS sentence — against aterm-effects cursor_glow.rs TrailStatus::line (the two keys last, after swallowed_no_echo=), InFlightTally's park_returns/park_flushed, HeldPark, park_candidate (Rainbow Kitty, same-row backward, a fresh stamp or presses in flight), spawn's RETURN arm (a forward move from the landing past the origin within TYPE_HINT_FRESH = 0.25 s, park_return_paid, judged origin -> target) and flush_park, and the flushes at the stale-park tick, note_backspace, note_scroll and the next other move; no claim contradicted; 2026-09-13 read of the prose feat/round-7-offscreen moved (shown by `xtask gate help-surfaces --diff`, matched at ca54aaadd) — the new `offscreen` row (Read, Lines, Session; the summary, and the detail sentence by sentence) and the `history` row's arch= clause — against aterm-gui control_query.rs offscreen_args/cmd_offscreen/format_offscreen_reply/OFFSCREEN_DEFAULT_MAX, aterm-core alt_archive.rs (ALT_ARCHIVE_DEFAULT_BUDGET, env_opted_out's 0/off/false/no, AltArchive::read and wipe, the gap kinds, the ESU commit and the 16 ms epilogue fallback), control.rs json_unsupported and control_session.rs cmd_history's arch= field; two slips fixed before this row (a committed frame was only `a DEC 2026 close`, where an app that never sends one is committed at the batch epilogue at most once per 16 ms; lost= counted evicted rows only, where a reset or turning the archive off wipes rows into it too) and both goldens regenerated as a pair; 2026-09-13 merge of main into feat/round-7-offscreen: main's drift-sweep re-read on 2026-09-13 (lane small-b) found the trail licence= row saying an unpaid press logs `key` and fixed it; `gate help-surfaces --diff` at the merge shows the merged prose is exactly round 7's read offscreen row and history arch= clause plus main's trail licence=/decline-reasons correction and its rainbow-kitty v2_ sentence (added 2026-09-12, before main's recorded read); recorded at the merge; 2026-09-13 release-candidate merge of the leftovers lane over the peers' round-7/round-8 reads: the merged prose is the UNION of the two halves — main's `offscreen` row and `history` arch= clause (read by round 7's author, recorded above) and the leftovers lane's `momentum_glow=` key and the absorbed rainbow-kitty v2_ sentence (read by that lane, recorded next) — each half read in full by its author against the code it describes, neither contradicted by the other, and this row records the UNION's hash: re-read on 2026-09-13 by drift sweep of 2026-09-13 (lane small-b); the trail licence= roster said an unpaid press logs `key` and is fixed in that commit; that read recorded 212628b6b90c5780 (834cd5f9f), and main 2f15705bc was ALREADY red on this row: the wrapped-row band merge (169dafa7b) had added a sentence to the `trail status` row that nobody read — `While rainbow kitty owns the frame the row ends with v2_quads= v2_halos= v2_stars= v2_meteors= v2_bridged= ribbon_retired=` and its glosses; 2026-09-13 leftovers lane: TWO prose changes since that read, both read here: (1) the `momentum_glow=` key in the backticked run and its sentence (MomentumGlow::value beside the cat's `momentum=`), against TrailStatus::line in cursor_glow.rs and App::trail_status in app_render.rs (`ws.momentum_glow.value(now, MOMENTUM_GLOW_TAU_S)`), pinned by control.rs trail_status_help_enumerates_exactly_the_keys_the_row_emits; (2) the absorbed v2 sentence, against TrailStatus::line_v2 in cursor_glow.rs (appends exactly ` v2_quads= v2_halos= v2_stars= v2_meteors= v2_bridged= ribbon_retired=` from rk::Status, and only when CursorGlow::v2_status is Some — `self.v2.engaged()`, so only while rainbow kitty owns the frame, at the tail), rainbow_kitty/mod.rs Status (quads/halos written this frame, stars and meteors live; `bridged` = cells the echo ledger relit for a late echo the admission ring scored declined; `retired` = ribbon cells retired by CONTENT over the engine's life — Engine::witness_rows when the glyph under a cell changed or went, Engine::retire_row when the caret was seen on another row through a declined move — cumulative across Engine::reset) and app_render.rs (per-window `ws.cursor_glow`, so `the window's cumulative count`); no claim contradicted; 2026-09-13 (2026-09-14 UTC) read of the prose 9758cd022 moved since the recorded read (shown by `xtask gate help-surfaces --diff`, matched at 0333f4a441d2) — the `post` row's rewritten `no-bridge=1` paragraph (NARROWER rather than the opposite, not a verdict on the message, `fabric attach <command...>` arms a supervisor and that same outbox drains, further posts only until `ERR outbox full`) and its new THIRD OUTCOME sentence (`ERR timeout id=<n>`, the `--wait` expiring with no landing reported, queued exactly like the other two), plus the two `contains` assertions in the_fabric_rows_state_the_bounds_they_are_held_to that pin `none is coming ON ITS OWN` and `ERR timeout id=<n>` — against aterm-gui fabric.rs cmd_post (its option tokens are to/kind/re/dl/via/--wait/len only, so there is no idempotency key that could collapse a re-post; the refusal at the door answers `ERR outbox full queued= bytes=` at OUTBOX_CAP = 128 or OUTBOX_BYTES_MAX = 4 MiB measured by queued_load + caller_sized_bytes; the PostRow is pushed BEFORE the wait loop), that loop's deadline arm returning `ERR timeout id={id}` with the row untouched (only trim_retired_posts removes rows and only ones carrying `off` or `dead`, so a timed-out post is still queued and still drained), fabric_wait_refusal and bridge_reachable (`no-bridge=1` is exactly `!supervised && state == absent`), note_bridge_supervised, fabric_launch.rs spawn_supervisor/arm/preflight and control.rs cmd_fabric_attach (`fabric attach <command...>` is the one seam that starts a supervise thread at RUNTIME and flips the latch), and aterm-link bridge.rs drain_outbox, whose bare `outbox` peek takes exactly the `off.is_none() && !dead` rows and runs on the reconcile path a freshly attached bridge takes; no claim contradicted — the retired `Nothing will publish it, no answer can arrive` WAS false, and control.rs fabric_attach_arms_the_supervisor_of_a_running_instance_once_and_for_owner_only shows the flip (`no-bridge=1` at id=1 and id=2, `queued=1` at id=3 after the attach, the two earlier posts still queued); one narrowness left standing rather than fixed here: the row gives an instance with no `[fabric] command` as the cause, where the predicate is bridge_reachable, so a CONFIGURED command whose program fails arm's preflight answers `no-bridge=1` too (that same test's `/nonexistent/aterm-link`, id=2), and the remedy the row prints is the one spawn_supervisor's own warn line names for that case as well AND the same completeness slip the manual.rs read found was fixed here in this commit: the row enumerated three outcomes and `cmd_post` has a fourth — `ERR <reason> id=<n>` for a post the bridge retired (`unroutable`/`ambiguous`/`undeliverable`), which `outbox` then omits, so it is the one outcome that does NOT mean queued; the row now says so and `the_fabric_rows_state_the_...` pins `ERR <reason> id=<n>` and `unroutable` beside the other three. The hash recorded here is the CORRECTED row, and both goldens were regenerated as a pair",
    ),
    (
        "crates/aterm-verify/src/cli.rs",
        "09229027122eacc5",
        "2026-09-12",
        "re-read on 2026-09-12 against the code that moved under it since the roster was minted, by the 2026-09-12 drift sweep (lane small-2); the 45-minute ceiling was two raises stale and is now DERIVED from DEFAULT_CHILD_CEILING",
    ),
    (
        "crates/aterm-verify/src/lib.rs",
        "8c813af689c52829",
        "2026-09-11",
        "the row hashed a checkout behind 51b8f88c8; re-read against run/toolchain_header_line by the 2026-09-10 round-3 reader; run's doc block, displaced onto toolchain_header_line, put back at the row's previous update, and its `only the ladder` claim (run writes the toolchain header first) fixed in the commit that updated this row; the residual `nothing else` claim fixed by the round-3 fixer — run also writes the prelude rungs, pin_hooks' `hooks pinned:` note and the verdict or the gate-defect FAIL/COULD NOT RUN lines; merged with origin/main's own 2026-09-11 re-read of this file, conflicts resolved by the orchestrator (upstream wording kept where both sides fixed the same claim)",
    ),
    (
        "crates/atpkg-keys/src/main.rs",
        "a491ce402d28ccc7",
        "2026-09-10",
        "read against its parser/dispatch by the 2026-09-10 sweep (aterm-D-keys-xtask-misc); findings fixed in the commit that added this row",
    ),
    (
        "crates/atpkg/src/cli.rs",
        "fd53cbe067ce4310",
        "2026-09-13",
        "re-read on 2026-09-13 by drift sweep of 2026-09-13 (aterm:atpkg); `repair`'s tracked-install sentence covered one of the record's two causes and is fixed in that commit",
    ),
    (
        "crates/xtask/src/gate.rs",
        "03c2a754c45e4047",
        "2026-09-14",
        "re-read on 2026-09-13 by drift sweep of 2026-09-13 (lane xtask); three enumerations had drifted twice and are now derived from the dispatch and the verify driver; 2026-09-13 release candidate: the ONE prose change since that read is the doc comment this cut added to `WRAPPED_BY_VERIFY_SH` (it was `tippy`-warned as never used because only `the_wrapped_list_is_exactly_what_the_verify_driver_calls` and the two doc paragraphs that link it read it; the const is now `#[cfg_attr(not(test), allow(dead_code))]` and the comment says why). Read against its only consumer, that test, and against the verify driver's stage list it is checked against; the list's six verbs are unchanged; no claim contradicted",
    ),
    (
        "crates/xtask/src/main.rs",
        "1dd2dd4a34fbe408",
        "2026-09-13",
        "re-read on 2026-09-13 by drift sweep of 2026-09-13 (lane xtask); its opt-in list now derives from gate::opt_in_names()",
    ),
    (
        "crates/aterm-census/src/main.rs",
        "15f0c30a18130a47",
        "2026-09-10",
        "usage text written 2026-09-10 exact to its parser (root default `.`, five selection flags and aliases, last-wins, exit 0/1/2) in the program-entry pass; read against main() the same day",
    ),
    (
        "crates/aterm-primer/assets/aterm-fabric-skill.md",
        "1fab837479aace46",
        "2026-09-14",
        "read in full against the code it teaches on 2026-09-13 by the primer-skill read of 2026-09-13, the first time any gate saw it — it is include_str!-ed into aterm-primer and installed into agents' own context files, so a wrong line here is TYPED; four false claim(s) found and fixed in that commit; 2026-09-13 read (2026-09-14 UTC) of the four prose changes 9758cd022 made here — the `fabric=connected` trap paragraph, the rewritten `no-bridge=1` bullet with its `queued, unpublishable until this instance has a bridge` / `not sent` wording (the only two lines the gate's --diff extracts), the new `ERR timeout id=<n>` bullet, and the `aterm-link` to `aterm link` respelling of `serve`, `mirror` and `hook install claude` — against fabric.rs (bridge_attached/fabric_state/bridge_lost, cmd_post pushing the PostRow into the outbox BEFORE any refusal path, fabric_wait_refusal and bridge_reachable = supervised || state != absent, the `ERR timeout id={id}` returned at the deadline, WAIT_DEFAULT_MS 30 s / WAIT_MAX_MS 600 s), control.rs serve_bridge (which stamps connected as the inherited Scope::Bridge lane starts being served, before the child has dialled any broker, and clears it only through BridgeLostGuard), aterm-link's Bridge::run reconnect loop (an unreachable broker is a back-off, never an exit — a bridge that exits on a broker hiccup lifts the fleet halt by dying) and cmd_outbox's peek-that-removes-nothing that lets a `fabric attach` drain the same queue, cli::dispatch's serve/ls/hook/mirror arms which `aterm link` (aterm/src/main.rs Verb::Link) and the `aterm-link` argv0 alias both reach, hook.rs's four-event `.claude/settings.json` template with its session-start/user-prompt-submit/pre-tool-use/stop arms, and fabric attach's own control_verbs.rs row; no claim contradicted, and the one gap worth naming is that the `third outcome` bullet does not reach a FOURTH — the bridge's retirement verdict `ERR <undeliverable|expired|ambiguous> id=<n>` from retire_post/DEAD_DEFAULT, which unlike the other three does NOT mean queued (`ctl help post` omits it too) and the shipped skill gained the FOURTH outcome in this commit, for the same reason the manual.rs and control_verbs.rs rows did: its list stopped at `ERR timeout id=<n>` as `the third outcome, and it means queued too`, where a post the bridge RETIRED answers `ERR unroutable|ambiguous|undeliverable id=<n>` and is the one outcome that is not queued. The hash recorded here is the CORRECTED asset",
    ),
    (
        "crates/aterm-primer/assets/drive-aterm-skill.md",
        "402f198cc4b11566",
        "2026-09-14",
        "read in full against the code it teaches on 2026-09-13 by the primer-skill read of 2026-09-13, the first time any gate saw it — it is include_str!-ed into aterm-primer and installed into agents' own context files, so a wrong line here is TYPED; six false claim(s) found and fixed in that commit; 2026-09-13 read of round 7's additions (the `aterm drive report` command line and paragraph, and `watch --report`) against drive_cli.rs's report arm, supervise/report.rs (the six reasons archive-gap, archive-reset, max-rows, marker-not-found, no-archive, main-screen; the header report complete= reason= marker= turn= rows= archived= screen= last=), run.rs reported() (idle, question, limited only) and reported_event_line (EVENT <phase> seq= complete= rows= then the summary), and control_session.rs's turn-start ArchMark with history printing arch= before text=; confirmed live on 2026-09-13 on a headless replay of a real Claude Code byte stream (offscreen returned every row the offline prototype recovered) and against the installed 0.84 server (report fell back to no-archive and found the start through marker=ledger); no slip found; recorded at the merge of main into feat/round-7-offscreen; 2026-09-13 read of round 8's paragraph (`Never rate a session for the human`: the survey's two rows, phase's and await-turn's `survey 0`, the guarded `key 'if=^●.How.is.Claude.doing' 0` command quoted for zsh, watch's EVENT survey line, --dismiss-surveys' DISMISSED line and its still-open hand-over, and the monitor grep with DISMISSED) against phase.rs survey_open, drive_cli.rs phase_reply, run.rs survey/dismiss_survey/survey_event_line and SURVEY_ROW, and the server's row_matcher guard; the gate's --diff shows only the quoted strings it extracts from a skill (the one new `@$SID`), so the paragraph was read from git diff; one slip fixed before this row (a copy of the survey elsewhere on the screen was said to match nothing, where only a quoted copy is sure not to); 2026-09-13 read (2026-09-14 UTC) of round 9's paragraph (`A worker running out of context is a decision point too`: the indicator's two spellings, phase's and await-turn's `context <n>%`, watch's `EVENT context` once a descent at or below --context-warn (default 10, 0 off) and `EVENT compacted` once the indicator has gone from above the composer or jumped 30 points, supervise's stderr lines for its run only and the `phase` check before the next run, the right-edge rule for a quoted copy) against phase.rs context_left, drive_cli.rs parse_sub/DEFAULT_CONTEXT_WARN/phase_reply and run.rs watch_context/drive/StopAtReview::say; the gate's --diff shows none of it (the paragraph quotes no string the gate extracts, so the hash is unchanged), and the paragraph was read from git diff; one slip fixed before this row (EVENT context was said to come once, where it comes once a descent and again after a compaction) and `gone` narrowed to gone from above the composer",
    ),
    (
        "crates/aterm-primer/assets/rust-in-aterm-skill.md",
        "fd2da99cf414c6cd",
        "2026-09-13",
        "read in full against the code it teaches on 2026-09-13 by the primer-skill read of 2026-09-13, the first time any gate saw it — it is include_str!-ed into aterm-primer and installed into agents' own context files, so a wrong line here is TYPED; three false claim(s) found and fixed in that commit",
    ),
    (
        "crates/aterm-primer/assets/supervise-agent-skill.md",
        "8df0f500dbcf74c8",
        "2026-09-14",
        "read in full against the code it teaches on 2026-09-13 by the primer-skill read of 2026-09-13, the first time any gate saw it — it is include_str!-ed into aterm-primer and installed into agents' own context files, so a wrong line here is TYPED; four, one of them the --sandbox write-confinement claim false claim(s) found and fixed in that commit; 2026-09-13 read of round 7's additions (the `aterm drive report` command line and paragraph, and `watch --report`) against drive_cli.rs's report arm, supervise/report.rs (the six reasons archive-gap, archive-reset, max-rows, marker-not-found, no-archive, main-screen; the header report complete= reason= marker= turn= rows= archived= screen= last=), run.rs reported() (idle, question, limited only) and reported_event_line (EVENT <phase> seq= complete= rows= then the summary), and control_session.rs's turn-start ArchMark with history printing arch= before text=; confirmed live on 2026-09-13 on a headless replay of a real Claude Code byte stream (offscreen returned every row the offline prototype recovered) and against the installed 0.84 server (report fell back to no-archive and found the start through marker=ledger); no slip found; recorded at the merge of main into feat/round-7-offscreen; 2026-09-13 read (2026-09-14 UTC) of round 9's bullet (`EVENT context … means the worker is about to compact; EVENT compacted … means it has`: phase's and await-turn's `context <n>%`, watch's first reading at or below --context-warn (default 10, 0 off) once a descent even mid-turn, `EVENT compacted` on the indicator gone or up 30 points, supervise's stderr lines for its run only and the `aterm drive phase` check before the next supervise) against phase.rs context_left, drive_cli.rs parse_sub/DEFAULT_CONTEXT_WARN/phase_reply and run.rs watch_context/drive/StopAtReview::say; the gate's --diff shows none of it (the bullet quotes no string the gate extracts, so the hash is unchanged), and the bullet was read from git diff; two slips fixed before this row (`gone from the screen`, where a box covering the composer takes the indicator off the screen and is no compaction — now gone from above the composer; the warning said once, now once a descent)",
    ),
    // HELP_SURFACES_ROSTER_END
];

/// Files discovery finds that carry no user-facing help text.
const NOT_HELP: &[NotHelpRow<'static>] = &[
    // HELP_SURFACES_NOT_HELP_BEGIN
    (
        "crates/xtask/src/help_surfaces.rs",
        "this gate's own module: its strings are the refusal lines and the roster rows; a row for it would change its own prose and refuse itself forever — the module is reviewed with the roster it carries",
    ),
    (
        "crates/aterm-gpu/src/metal/ffi.rs",
        "Metal Objective-C FFI bindings for a library crate (no fn main, no println!/eprintln!, no usage/help string). The USAGE hits are the MTLTextureUsage bitmask constants TEXTURE_USAGE_SHADER_READ / TEXTURE_USAGE_RENDER_TARGET / TEXTURE_USAGE_PIXEL_FORMAT_VIEW (lines 573-578) and the `setUsage:` / `usage` selectors (846-847, 1586).",
    ),
    (
        "crates/aterm-gui/src/accesskit_tree.rs",
        "Pure accessibility-tree projection (SettingsState/grid -> accesskit TreeUpdate). Its strings are screen-reader labels/descriptions and node-id docs, not CLI help or usage text; nothing here is printed to a terminal.",
    ),
    (
        "crates/aterm-http/src/verifier/apple.rs",
        "SecTrust TLS chain verifier for a library crate (no fn main, no printed text). The USAGE hit is the OSStatus constant ERR_SEC_INVALID_EXTENDED_KEY_USAGE = -67609 (line 107), mapped to an EKU rejection at line 534.",
    ),
    (
        "crates/aterm-http/src/verifier/windows.rs",
        "CryptoAPI/schannel TLS chain verifier for a library crate (no fn main, no printed text). The USAGE hits are the Win32 imports CERT_USAGE_MATCH / CTL_USAGE / USAGE_MATCH_TYPE_AND (lines 76-81, 228-230) and the HRESULT constant CERT_E_WRONG_USAGE (line 99); the module doc's mention of CERT_USAGE_MATCH (line 27) is an implementation note, not operator help.",
    ),
    (
        "crates/aterm-types/src/app_inspection.rs",
        "Wire-grammar parser for the `inspect app/v1` / `act app/v1` / `open app` control-socket verbs. Its ParseError::Usage strings are ERR-line payloads returned over the socket (surfaced via error.to_string() in aterm-gui/src/app_control.rs:114-139), not a --help surface. They were checked anyway and match parse_inspect/parse_act/parse_open_app exactly; no findings.",
    ),
    (
        "crates/aterm-agent/src/bin/aterm-drive.rs",
        "a one-line bin shim: its help is DRIVE_HELP in crates/aterm-agent/src/lib.rs (rostered)",
    ),
    (
        "crates/aterm-agent/src/bin/aterm-fleet.rs",
        "a bin shim: its help is crates/aterm-agent/src/fleet_cli.rs (rostered)",
    ),
    (
        "crates/aterm-ctl/src/main.rs",
        "a bin shim: its help is HELP_PROSE in crates/aterm-ctl/src/lib.rs (rostered)",
    ),
    (
        "crates/aterm-forge/src/main.rs",
        "a bin shim over crates/aterm-forge/src/cli.rs (rostered); its two own strings (--root unresolvable, no workspace Cargo.toml above cwd) read against canonical_root/discover_root 2026-09-10",
    ),
    (
        "crates/aterm-gui/src/bin/aterm-redraw-conformance.rs",
        "takes no arguments and reads no env; a conformance driver whose exit codes are owned by aterm_gui::control_redraw_conformance",
    ),
    (
        "crates/aterm-gui/src/main.rs",
        "a bin shim: the window's --help is HELP_HEAD in crates/aterm-gui/src/cli.rs (rostered)",
    ),
    (
        "crates/aterm-link/src/main.rs",
        "an argv0-symlink shim: its help is crates/aterm-link/src/cli.rs (rostered)",
    ),
    (
        "crates/aterm-release/src/main.rs",
        "a bin shim: its help is USAGE in crates/aterm-release/src/cli.rs (rostered)",
    ),
    (
        "crates/aterm-scrollback/fuzz/fuzz_targets/lz4_decompress.rs",
        "a libfuzzer target, not a program",
    ),
    (
        "crates/aterm-verify/src/main.rs",
        "a bin shim over crates/aterm-verify/src/cli.rs (rostered); its own root error names exactly the two files locate_root requires and the --root > ATERM_VERIFY_ROOT > walk precedence holds (read 2026-09-10)",
    ),
    (
        "crates/aterm/src/main.rs",
        "the front door delegates its --help to HELP_HEAD in crates/aterm-cli/src/lib.rs (rostered); its own printed strings (update status|check usage, the windowing warning, the reroute and staged-update notices, the never-checked line) were read against their code 2026-09-10 and one false platform claim fixed",
    ),
    (
        "crates/atpkg/src/main.rs",
        "a bin shim: its help is VERB_USAGE and cmd_help in crates/atpkg/src/cli.rs (rostered)",
    ),
    (
        "crates/aterm-agent/src/supervise/fixtures/wait_bg7.out",
        "a captured terminal screen used as a test input by the `#[test]` at supervise/run.rs:2508 \
         (`Claude Code parks under the done row`); include_str!-ed by that test only, so it ships in \
         no binary a reader runs and states nothing",
    ),
    // HELP_SURFACES_NOT_HELP_END
];

/// `xtask gate help-surfaces`: the verdict, with the transcript on stderr.
pub(crate) fn gate_help_surfaces() -> bool {
    let (ok, log) = help_surfaces_report(&crate::workspace_root());
    eprint!("{log}");
    ok
}

/// The verb over an arbitrary root, returning the verdict plus the transcript.
pub(crate) fn help_surfaces_report(root: &Path) -> (bool, String) {
    let refusals = help_surfaces_check(
        root,
        SCAN_ROOTS,
        SURFACES,
        NOT_HELP,
        MAX_AGE_DAYS,
        today_days(),
    );
    let mut log = String::new();
    let _ = writeln!(
        log,
        "=== gate help-surfaces (every CLI help surface read against its handler) ==="
    );
    if refusals.is_empty() {
        let _ = writeln!(
            log,
            "gate help-surfaces: GREEN — {} surface(s) read-verified, {} classified not-help, \
             max age {MAX_AGE_DAYS} days",
            SURFACES.len(),
            NOT_HELP.len()
        );
        return (true, log);
    }
    for r in &refusals {
        let _ = writeln!(log, "gate help-surfaces: {r}");
    }
    let _ = writeln!(
        log,
        "gate help-surfaces: FAILED — {} refusal(s). Read the named text against its handler, \
         then record the row it prints in crates/xtask/src/help_surfaces.rs.",
        refusals.len()
    );
    (false, log)
}

/// The pure check: refusal lines (empty = green). `today` is days since the Unix
/// epoch, so a fixture can pin the calendar.
pub(crate) fn help_surfaces_check<'a>(
    root: &Path,
    scan_roots: &[&str],
    surfaces: &[SurfaceRow<'a>],
    not_help: &[NotHelpRow<'a>],
    max_age_days: i64,
    today: i64,
) -> Vec<String> {
    let mut out = Vec::new();
    let today_iso = civil_from_days(today);

    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for (p, ..) in surfaces {
        *seen.entry(p).or_insert(0) += 1;
    }
    for (p, _) in not_help {
        *seen.entry(p).or_insert(0) += 1;
    }
    for (p, k) in &seen {
        if *k > 1 {
            out.push(format!(
                "R6 DUPLICATE  {p} appears {k} times across SURFACES/NOT_HELP — keep one row"
            ));
        }
    }

    let discovered = discover(root, scan_roots);
    let rostered: BTreeMap<&str, &SurfaceRow<'a>> = surfaces.iter().map(|r| (r.0, r)).collect();
    let allowed: BTreeMap<&str, &str> = not_help.iter().copied().collect();

    for (rel, reasons) in &discovered {
        if rostered.contains_key(rel.as_str()) || allowed.contains_key(rel.as_str()) {
            continue;
        }
        let hash = read_prose_hash(&root.join(rel)).unwrap_or_default();
        out.push(format!(
            "R1 UNROSTERED {rel} carries help text ({}) and no row says it was read — read it \
             against its handler, then add: (\"{rel}\", \"{hash}\", \"{today_iso}\", \"<who read it against what>\"),",
            reasons.join(", ")
        ));
    }

    for (rel, hash, verified, method) in surfaces {
        let path = root.join(rel);
        if !path.is_file() {
            out.push(format!(
                "R3 GONE       {rel} is rostered but not on disk — drop the row or fix the path"
            ));
            continue;
        }
        let current = read_prose_hash(&path).unwrap_or_default();
        if current != *hash {
            out.push(format!(
                "R2 CHANGED    {rel}: help/doc text changed since it was read on {verified} — re-read \
                 it against its handler, then replace the row with: (\"{rel}\", \"{current}\", \
                 \"{today_iso}\", \"{method}\"), (see: xtask gate help-surfaces --diff {rel})"
            ));
        }
        match days_from_iso(verified) {
            None => out.push(format!(
                "R4 STALE      {rel}: verified date {verified:?} is not YYYY-MM-DD"
            )),
            Some(d) if today - d > max_age_days => out.push(format!(
                "R4 STALE      {rel}: read {} days ago (limit {max_age_days}) — re-read it and re-date the row",
                today - d
            )),
            Some(_) => {}
        }
    }

    let programs = program_entry_points(root, scan_roots);
    let embedded = embedded_files(root, surfaces);
    for (rel, _) in not_help {
        if !root.join(rel).is_file() {
            out.push(format!(
                "R5 DEAD_ALLOW {rel} is in NOT_HELP but not on disk — prune it"
            ));
        } else if !discovered.contains_key(*rel)
            && !programs.contains(*rel)
            && !embedded.contains_key(*rel)
        {
            out.push(format!(
                "R5 DEAD_ALLOW {rel} is in NOT_HELP but discovery no longer finds it — prune it"
            ));
        }
    }
    for (rel, via) in &embedded {
        if rostered.contains_key(rel.as_str()) || allowed.contains_key(rel.as_str()) {
            continue;
        }
        let hash = read_prose_hash(&root.join(rel)).unwrap_or_default();
        out.push(format!(
            "R8 EMBEDDED   {rel} is compiled into {via} by include_str!/include_bytes! and is in neither \
             list — it is shipped text a reader sees; read it against the code it describes and add: \
             (\"{rel}\", \"{hash}\", \"{today_iso}\", \"<who read it against what>\"), or record in \
             NOT_HELP why it is not help"
        ));
    }
    for rel in &programs {
        if rostered.contains_key(rel.as_str()) || allowed.contains_key(rel.as_str()) {
            continue;
        }
        let hash = read_prose_hash(&root.join(rel)).unwrap_or_default();
        out.push(format!(
            "R7 PROGRAM    {rel} is a binary entry point in neither list — read its help against \
             its parser and add: (\"{rel}\", \"{hash}\", \"{today_iso}\", \"<who read it against \
             what>\"), or record in NOT_HELP where its help lives (or that it takes no arguments)"
        ));
    }
    out
}

/// Every binary entry point under `scan_roots`: the conventional `src/main.rs`,
/// `src/bin/*.rs` and `src/bin/*/main.rs` of each Cargo package, plus any explicit
/// `[[bin]] path = "…"`. Test, example, bench and target trees are skipped as
/// everywhere else, and so is `vendor/`.
/// Files a rostered surface compiles in, as `{path: the surface that includes it}`.
///
/// `include_str!("../assets/x.md")` puts a whole file in the binary, and in this
/// workspace that is how shipped agent documentation travels. The lexer sees only
/// the PATH at the macro, so the CONTENT never reaches the including file's hash:
/// editing the asset moves nothing. Resolved relative to the including file, as
/// the macro resolves it. Comments are blanked first — strings are NOT, because
/// the path this looks for is one.
fn embedded_files(
    root: &Path,
    surfaces: &[SurfaceRow<'_>],
) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    for (rel, ..) in surfaces {
        let path = root.join(rel);
        if path.extension().is_none_or(|x| x != "rs") || !path.is_file() {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        let code = strip_comments_keep_strings(&src);
        for inc in include_paths(&code) {
            let Some(dir) = path.parent() else {
                continue;
            };
            let target = normalize(&dir.join(&inc));
            // A `.rs` target needs no rule of its own: the `*.rs` walk already
            // reaches it, and whether it carries help is R1's question.
            // Including it here would be a second, wrong answer to a question
            // another rule owns — measured 2026-09-13, when nine of R8's first
            // sixteen hits were `.rs` files a TEST include_str!s to scan its own
            // source. Test and fixture trees are excluded for the same reason
            // they are everywhere else: a golden is derived text, not a claim.
            if target.extension().is_some_and(|x| x == "rs") || !target.is_file() {
                continue;
            }
            if let Ok(r) = target.strip_prefix(root)
                && excluded_path(&r.to_string_lossy().replace('\\', "/"))
            {
                continue;
            }
            if let Ok(r) = target.strip_prefix(root) {
                out.entry(r.to_string_lossy().replace('\\', "/"))
                    .or_insert_with(|| (*rel).to_string());
            }
        }
    }
    out
}

/// Every `include_str!`/`include_bytes!` path literal in `code`.
fn include_paths(code: &str) -> Vec<String> {
    let mut out = Vec::new();
    for macro_name in ["include_str!", "include_bytes!"] {
        let mut from = 0;
        while let Some(i) = code[from..].find(macro_name) {
            let at = from + i + macro_name.len();
            from = at;
            let rest = code[at..].trim_start();
            let Some(open) = rest.strip_prefix('(') else {
                continue;
            };
            let open = open.trim_start();
            let Some(body) = open.strip_prefix('"') else {
                continue;
            };
            if let Some(end) = body.find('"') {
                out.push(body[..end].to_string());
            }
        }
    }
    out
}

/// Comments blanked, string literals INTACT — `..` and `.` resolved so a
/// relative include path becomes a real repo-relative one without touching the
/// filesystem's symlinks.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// Comments blanked, string literals INTACT. The include path this looks for IS
/// a string literal, so a stripper that blanks literal bodies would erase it;
/// and a `//` inside a literal is not a comment, so literals must be skipped
/// rather than ignored.
fn strip_comments_keep_strings(src: &str) -> String {
    let b = src.as_bytes();
    let n = b.len();
    let mut out = String::with_capacity(n);
    let mut i = 0;
    while i < n {
        if b[i..].starts_with(b"//") {
            let j = src[i..].find('\n').map_or(n, |k| i + k);
            out.extend(std::iter::repeat_n(' ', j - i));
            i = j;
            continue;
        }
        if b[i..].starts_with(b"/*") {
            let mut depth = 1usize;
            let mut j = i + 2;
            while j < n && depth > 0 {
                if b[j..].starts_with(b"/*") {
                    depth += 1;
                    j += 2;
                } else if b[j..].starts_with(b"*/") {
                    depth -= 1;
                    j += 2;
                } else {
                    j += 1;
                }
            }
            out.extend(
                src[i..j]
                    .chars()
                    .map(|c| if c == '\n' { '\n' } else { ' ' }),
            );
            i = j;
            continue;
        }
        // A raw string: copy it whole, hashes and all, so nothing inside is read.
        if let Some((open_len, hashes)) = raw_string_open(b, i) {
            let start = i + open_len;
            let close = format!("\"{}", "#".repeat(hashes));
            let end = src[start..]
                .find(&close)
                .map_or(n, |k| start + k + close.len());
            out.push_str(&src[i..end]);
            i = end;
            continue;
        }
        if b[i] == b'"' {
            let mut j = i + 1;
            while j < n {
                if b[j] == b'\\' {
                    j += 2;
                    continue;
                }
                if b[j] == b'"' {
                    j += 1;
                    break;
                }
                j += 1;
            }
            out.push_str(&src[i..j.min(n)]);
            i = j;
            continue;
        }
        let len = utf8_len(b[i]);
        out.push_str(&src[i..(i + len).min(n)]);
        i += len;
    }
    out
}

fn program_entry_points(root: &Path, scan_roots: &[&str]) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    let mut manifests = Vec::new();
    for sr in scan_roots {
        collect_named(&root.join(sr), "Cargo.toml", &mut manifests);
    }
    manifests.sort();
    for toml in manifests {
        let Some(dir) = toml.parent() else {
            continue;
        };
        let Ok(rel_dir) = dir.strip_prefix(root) else {
            continue;
        };
        let rel_dir = rel_dir.to_string_lossy().replace('\\', "/");
        if excluded_path(&format!("{rel_dir}/x.rs")) || format!("/{rel_dir}/").contains("/vendor/")
        {
            continue;
        }
        let mut candidates = vec![dir.join("src/main.rs")];
        let mut bins = Vec::new();
        collect_rs(&dir.join("src/bin"), &mut bins);
        bins.sort();
        for b in bins {
            let depth = b
                .strip_prefix(dir.join("src/bin"))
                .map_or(0, |p| p.components().count());
            if depth == 1 || (depth == 2 && b.file_name().is_some_and(|n| n == "main.rs")) {
                candidates.push(b);
            }
        }
        if let Ok(text) = std::fs::read_to_string(&toml) {
            let mut in_bin = false;
            for line in text.lines() {
                let l = line.trim();
                if l.starts_with('[') {
                    in_bin = l == "[[bin]]";
                    continue;
                }
                if in_bin
                    && let Some(rest) = l.strip_prefix("path")
                    && let Some(v) = rest.trim_start().strip_prefix('=')
                {
                    candidates.push(dir.join(v.trim().trim_matches('"')));
                }
            }
        }
        for c in candidates {
            if !c.is_file() {
                continue;
            }
            let Ok(rel) = c.strip_prefix(root) else {
                continue;
            };
            let rel = rel.to_string_lossy().replace('\\', "/");
            if !excluded_path(&rel) {
                out.insert(rel);
            }
        }
    }
    out
}

fn collect_named(dir: &Path, name: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            if p.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            collect_named(&p, name, out);
        } else if p.file_name().is_some_and(|n| n == name) {
            out.push(p);
        }
    }
}

// ------------------------------------------------------------------ discovery

/// Workspace-relative path -> why it was discovered, for every `.rs` file under
/// `scan_roots` that carries help text.
fn discover(root: &Path, scan_roots: &[&str]) -> BTreeMap<String, Vec<&'static str>> {
    let mut found = BTreeMap::new();
    for sr in scan_roots {
        let mut files = Vec::new();
        collect_rs(&root.join(sr), &mut files);
        files.sort();
        for f in files {
            let Ok(rel) = f.strip_prefix(root) else {
                continue;
            };
            let rel = rel.to_string_lossy().replace('\\', "/");
            if excluded_path(&rel) {
                continue;
            }
            let Ok(src) = std::fs::read_to_string(&f) else {
                continue;
            };
            let reasons = discovery_reasons(&src);
            if !reasons.is_empty() {
                found.insert(rel, reasons);
            }
        }
    }
    found
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            if p.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            collect_rs(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Test, example and bench trees are not shipped surfaces.
fn excluded_path(rel: &str) -> bool {
    let s = format!("/{rel}");
    ["/examples/", "/benches/", "/tests/", "/target/"]
        .iter()
        .any(|d| s.contains(d))
        || s.ends_with("_test.rs")
        || s.ends_with("_tests.rs")
        || s.ends_with("/tests.rs")
        || s.ends_with("/kani_proofs.rs")
}

fn discovery_reasons(src: &str) -> Vec<&'static str> {
    let (strings, _docs) = prose_of_rust(src);
    let mut reasons = Vec::new();
    if strings.iter().any(|s| has_usage_block_line(s)) {
        reasons.push("usage-string");
    }
    if let Some(r) = help_code_marker(src) {
        reasons.push(r);
    }
    reasons
}

/// A literal with a line that starts a usage block: `usage:`, `OPTIONS:`, ….
fn has_usage_block_line(literal: &str) -> bool {
    const KEYS: &[&str] = &[
        "usage",
        "options",
        "subcommands",
        "commands",
        "arguments",
        "verbs",
        "flags",
        "exit codes",
        "exit code",
    ];
    literal.lines().any(|line| {
        let l = line.trim_start().to_ascii_lowercase();
        KEYS.iter().any(|k| {
            l.strip_prefix(k)
                .is_some_and(|rest| rest.trim_start().starts_with(':'))
        })
    })
}

/// Code that exists only to print help.
fn help_code_marker(src: &str) -> Option<&'static str> {
    const FNS: &[&str] = &[
        "fn usage",
        "fn print_usage",
        "fn help_text",
        "fn usage_text",
        "fn print_help",
        "fn cmd_help",
        "fn verb_help",
        "fn usage_error",
    ];
    for f in FNS {
        for (i, _) in src.match_indices(f) {
            let rest = &src[i + f.len()..];
            if rest.starts_with('(') || rest.starts_with('<') {
                return Some("help-fn");
            }
        }
    }
    for kw in ["const ", "static "] {
        for (i, _) in src.match_indices(kw) {
            let ident: String = src[i + kw.len()..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if ident.split('_').any(|seg| seg == "USAGE" || seg == "HELP") {
                return Some("help-const");
            }
        }
    }
    if src.contains("#[command(")
        || src.contains("#[clap(")
        || src.contains("about = \"")
        || src.contains("long_about")
    {
        return Some("clap-help");
    }
    None
}

// ----------------------------------------------------------------- extraction

/// `(string literals, doc comments)` in source order; plain comments, char
/// literals and code are dropped.
pub(crate) fn prose_of_rust(src: &str) -> (Vec<String>, Vec<String>) {
    let b = src.as_bytes();
    let n = b.len();
    let mut strings = Vec::new();
    let mut docs = Vec::new();
    let mut i = 0;
    while i < n {
        let c = b[i];
        if b[i..].starts_with(b"//") {
            let j = src[i..].find('\n').map_or(n, |k| i + k);
            let line = &src[i..j];
            if line.starts_with("///") || line.starts_with("//!") {
                docs.push(line[3..].trim().to_string());
            }
            i = j + 1;
            continue;
        }
        if b[i..].starts_with(b"/*") {
            let doc = b[i..].starts_with(b"/**") || b[i..].starts_with(b"/*!");
            let mut depth = 1;
            let mut j = i + 2;
            while j < n && depth > 0 {
                if b[j..].starts_with(b"/*") {
                    depth += 1;
                    j += 2;
                } else if b[j..].starts_with(b"*/") {
                    depth -= 1;
                    j += 2;
                } else {
                    j += 1;
                }
            }
            if doc {
                let end = j.saturating_sub(2).max(i + 3);
                docs.push(src[i + 3..end].trim().to_string());
            }
            i = j;
            continue;
        }
        // raw strings: r"…", r#"…"#, br"…", cr"…"
        if let Some((open_len, hashes)) = raw_string_open(b, i) {
            let start = i + open_len;
            let close = format!("\"{}", "#".repeat(hashes));
            let end = src[start..].find(&close).map_or(n, |k| start + k);
            strings.push(src[start..end].to_string());
            i = end + close.len();
            continue;
        }
        if c == b'"' && !(i > 0 && i + 1 < n && b[i - 1] == b'\'' && b[i + 1] == b'\'') {
            let mut j = i + 1;
            let mut buf = String::new();
            while j < n {
                let ch = b[j];
                if ch == b'\\' {
                    let nxt = b.get(j + 1).copied();
                    if nxt == Some(b'\n') {
                        j += 2;
                        while j < n && matches!(b[j], b' ' | b'\t' | b'\r' | b'\n') {
                            j += 1;
                        }
                        continue;
                    }
                    buf.push('\\');
                    if let Some(x) = nxt {
                        buf.push(x as char);
                    }
                    j += 2;
                    continue;
                }
                if ch == b'"' {
                    break;
                }
                // Copy one UTF-8 scalar.
                let ch_len = utf8_len(ch);
                buf.push_str(&src[j..(j + ch_len).min(n)]);
                j += ch_len;
            }
            strings.push(buf);
            i = j + 1;
            continue;
        }
        if c == b'\'' {
            if i + 2 < n && b[i + 1] == b'\\' {
                let j = src[i + 2..].find('\'').map_or(n, |k| i + 2 + k);
                i = j + 1;
                continue;
            }
            // 'x' where x may be multi-byte
            if i + 1 < n {
                let l = utf8_len(b[i + 1]);
                if i + 1 + l < n && b[i + 1 + l] == b'\'' {
                    i += 2 + l;
                    continue;
                }
            }
            i += 1;
            continue;
        }
        i += utf8_len(c);
    }
    (strings, docs)
}

/// `Some((opener length, hash count))` when a raw string opens at `i`.
fn raw_string_open(b: &[u8], i: usize) -> Option<(usize, usize)> {
    let mut k = i;
    if k < b.len() && (b[k] == b'b' || b[k] == b'c') {
        k += 1;
    }
    if k >= b.len() || b[k] != b'r' {
        return None;
    }
    if i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_') {
        return None;
    }
    k += 1;
    let mut hashes = 0;
    while k < b.len() && b[k] == b'#' {
        hashes += 1;
        k += 1;
    }
    if k < b.len() && b[k] == b'"' {
        Some((k + 1 - i, hashes))
    } else {
        None
    }
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1,
    }
}

/// FNV-1a 64 over the prose, as 16 hex digits. Change detection, not a
/// certificate — the roster is reviewed text, not a trust boundary.
pub(crate) fn prose_hash(strings: &[String], docs: &[String]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for &x in bytes {
            h ^= u64::from(x);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    for s in strings {
        feed(s.as_bytes());
        feed(b"\n\x00");
    }
    feed(b"\n\x01");
    for d in docs {
        feed(d.as_bytes());
        feed(b"\n\x00");
    }
    format!("{h:016x}")
}

fn read_prose_hash(path: &Path) -> Option<String> {
    Some(source_prose_hash(&std::fs::read_to_string(path).ok()?))
}

/// [`prose_hash`] of a source text's [`prose_of_rust`] — the gate's hash.
fn source_prose_hash(src: &str) -> String {
    let (s, d) = prose_of_rust(src);
    prose_hash(&s, &d)
}

// --------------------------------------------------------------------- --diff

/// Where the roster lives, workspace-relative: `--diff`'s fallback blames its row.
const ROSTER_FILE: &str = "crates/xtask/src/help_surfaces.rs";

const DIFF_USAGE: &str = "usage: xtask gate help-surfaces [--diff PATH]...\n  \
    no argument   the gate itself\n  \
    --diff PATH   not the gate: the prose PATH changed since the read its row records, \
    item by item as the gate hashes it (PATH as the row spells it; repeatable)";

/// `xtask gate help-surfaces [--diff PATH]...`. With no argument, the gate (the
/// `all` roster calls [`gate_help_surfaces`] directly). Each `--diff PATH` prints
/// [`prose_diff_report`] to stdout instead — a reading aid, not a verdict: it
/// fails only when it cannot produce the diff.
pub(crate) fn gate_help_surfaces_args(rest: &[String]) -> bool {
    if rest.is_empty() {
        return gate_help_surfaces();
    }
    let mut paths = Vec::new();
    let mut args = rest.iter();
    while let Some(arg) = args.next() {
        if arg != "--diff" {
            eprintln!("gate help-surfaces: unknown argument `{arg}`.\n{DIFF_USAGE}");
            return false;
        }
        let Some(path) = args.next() else {
            eprintln!("gate help-surfaces: `--diff` needs a path.\n{DIFF_USAGE}");
            return false;
        };
        paths.push(path.as_str());
    }
    let root = crate::workspace_root();
    let mut ok = true;
    for path in paths {
        let (produced, report) = prose_diff_report(&root, path, SURFACES, NOT_HELP, ROSTER_FILE);
        print!("{report}");
        ok &= produced;
    }
    ok
}

/// `--diff PATH` over an arbitrary root: `(produced, report)`.
///
/// It finds the version the row's read was of: walking `git log -- PATH` newest
/// to oldest, the first commit whose version hashes — by [`prose_of_rust`] and
/// [`prose_hash`], exactly as the gate hashes — to the row's recorded value. The
/// report is [`unified_diff`] of [`prose_lines`] from that version to the working
/// tree, headed by that commit's sha, date and subject. When no committed version
/// matches (the row was recorded from an uncommitted tree) it says so and diffs
/// from the version at the commit that last touched the row in `roster_file`
/// instead — which is NOT the text that was read, and the report says that too.
pub(crate) fn prose_diff_report(
    root: &Path,
    path: &str,
    surfaces: &[SurfaceRow<'_>],
    not_help: &[NotHelpRow<'_>],
    roster_file: &str,
) -> (bool, String) {
    let rel = workspace_rel(root, path);
    let mut log = String::new();
    let _ = writeln!(
        log,
        "=== gate help-surfaces --diff {rel} (the prose changed since the recorded read) ==="
    );
    let Some(&(_, recorded, verified, _)) = surfaces.iter().find(|r| r.0 == rel) else {
        let why = match not_help.iter().find(|r| r.0 == rel) {
            Some((_, reason)) => {
                format!("is in NOT_HELP ({reason}), so none of its prose is hashed")
            }
            None => "has no row in SURFACES, so no read is recorded to diff from".to_string(),
        };
        let _ = writeln!(
            log,
            "gate help-surfaces --diff: {rel} {why} — nothing to diff"
        );
        return (false, log);
    };
    let now_src = match std::fs::read_to_string(root.join(&rel)) {
        Ok(src) => src,
        Err(e) => {
            let _ = writeln!(
                log,
                "gate help-surfaces --diff: cannot read {rel}: {e} — a rostered file that is \
                 gone is R3, and there is nothing to diff"
            );
            return (false, log);
        }
    };
    let now_hash = source_prose_hash(&now_src);
    let _ = writeln!(log, "recorded: {recorded}, read on {verified}");
    if now_hash == recorded {
        let _ = writeln!(
            log,
            "now:      {now_hash} in the working tree — the same prose, so the row is green \
             and nothing changed since the read"
        );
        return (true, log);
    }
    let _ = writeln!(
        log,
        "now:      {now_hash} in the working tree — changed, so the gate refuses the row R2"
    );
    let (old_label, old_src) = match diff_base(root, &rel, recorded, roster_file, &mut log) {
        Ok(base) => base,
        Err(e) => {
            let _ = writeln!(log, "gate help-surfaces --diff: COULD NOT RUN — {e}");
            return (false, log);
        }
    };
    let diff = unified_diff(
        &old_label,
        &format!("{rel} (working tree)"),
        &prose_lines(&old_src),
        &prose_lines(&now_src),
        3,
    );
    if diff.hunks == 0 {
        let _ = writeln!(
            log,
            "gate help-surfaces --diff: the working tree's prose is that version's, line for \
             line — what the row recorded is in no committed version, so re-read the file whole"
        );
    } else {
        log.push_str(&diff.text);
        let _ = writeln!(
            log,
            "gate help-surfaces --diff: {} hunk(s), {} prose line(s) removed, {} added — read \
             them against their handlers, then record the row `xtask gate help-surfaces` prints",
            diff.hunks, diff.removed, diff.added
        );
    }
    (true, log)
}

/// The version `--diff` diffs from, as `(label, source)`, saying into `log` how
/// it was chosen.
fn diff_base(
    root: &Path,
    rel: &str,
    recorded: &str,
    roster_file: &str,
    log: &mut String,
) -> Result<(String, String), String> {
    let listing = git_stdout(root, &["log", "--format=%H", "--", rel])?;
    let shas: Vec<&str> = listing.lines().filter(|l| !l.is_empty()).collect();
    let mut blobs = Blobs::spawn(root)?;
    for (walked, sha) in shas.iter().enumerate() {
        let Some(src) = blobs.read(&format!("{sha}:{rel}"))? else {
            continue;
        };
        if source_prose_hash(&src) == recorded {
            let _ = writeln!(log, "matched:  {}", describe_commit(root, sha)?);
            let _ = writeln!(
                log,
                "          the newest commit whose version of {rel} hashes to the recorded value \
                 ({} of the {} commit(s) that touch it walked)",
                walked + 1,
                shas.len()
            );
            note_changes_since(root, rel, &shas[..walked], log)?;
            return Ok((
                format!("{rel} @ {} (the recorded read)", short_sha(sha)),
                src,
            ));
        }
    }
    let _ = writeln!(
        log,
        "matched:  none — no committed version of {rel} hashes to {recorded} (all {} commit(s) \
         that touch it walked): the row was recorded from an uncommitted tree",
        shas.len()
    );
    let (spec, label) = match row_commit(root, roster_file, rel)? {
        Some(sha) => {
            let _ = writeln!(log, "fallback: {}", describe_commit(root, &sha)?);
            let _ = writeln!(
                log,
                "          the commit that last touched {rel}'s row in {roster_file}. Its version \
                 is NOT the text that was read: prose edited between that read and this commit \
                 does not show below"
            );
            (
                format!("{sha}:{rel}"),
                format!(
                    "{rel} @ {} (the commit that last touched the row)",
                    short_sha(&sha)
                ),
            )
        }
        None => {
            let _ = writeln!(
                log,
                "fallback: HEAD — no line of {rel}'s row in {roster_file} is committed yet"
            );
            (format!("HEAD:{rel}"), format!("{rel} @ HEAD"))
        }
    };
    if let Some(src) = blobs.read(&spec)? {
        return Ok((label, src));
    }
    let _ = writeln!(
        log,
        "          {rel} does not exist in that version, so every prose line below is new"
    );
    Ok((format!("{label}, absent"), String::new()))
}

/// Who moved the prose since the match: the `newer` commits that touch `rel`
/// (newest first, the first [`SINCE_LISTED`]) and the working tree's own edits.
fn note_changes_since(
    root: &Path,
    rel: &str,
    newer: &[&str],
    log: &mut String,
) -> Result<(), String> {
    if !newer.is_empty() {
        let listed = &newer[..newer.len().min(SINCE_LISTED)];
        let shown = git_stdout(
            root,
            &[&["show", "-s", "--format=%h %cs %s"][..], listed].concat(),
        )?;
        let _ = writeln!(
            log,
            "since:    {} commit(s) touch {rel} after it, newest first:",
            newer.len()
        );
        for line in shown.lines().filter(|l| !l.is_empty()) {
            let _ = writeln!(log, "          {line}");
        }
        if newer.len() > listed.len() {
            let _ = writeln!(
                log,
                "          ... and {} older ones (git log -- {rel})",
                newer.len() - listed.len()
            );
        }
    }
    let dirty = !git_stdout(root, &["status", "--porcelain", "--", rel])?
        .trim()
        .is_empty();
    if dirty {
        let lead = if newer.is_empty() {
            "since:    only"
        } else {
            "          and"
        };
        let _ = writeln!(
            log,
            "{lead} the working tree's own uncommitted edits to {rel}"
        );
    }
    Ok(())
}

/// How many of the commits since the match `--diff` names.
const SINCE_LISTED: usize = 10;

/// The commit that last touched `rel`'s row in `roster_file`: of the commits
/// `git blame` gives the row's lines (its path line through its closing `),`),
/// the newest by committer time. `None` when no line of the row is committed.
fn row_commit(root: &Path, roster_file: &str, rel: &str) -> Result<Option<String>, String> {
    let text = std::fs::read_to_string(root.join(roster_file))
        .map_err(|e| format!("cannot read {roster_file}: {e}"))?;
    let (first, last) =
        row_line_span(&text, rel).ok_or_else(|| format!("{roster_file} has no row for {rel}"))?;
    let porcelain = git_stdout(
        root,
        &[
            "blame",
            "--porcelain",
            "-L",
            &format!("{first},{last}"),
            "--",
            roster_file,
        ],
    )?;
    Ok(newest_blamed_commit(&porcelain))
}

/// The 1-based, inclusive line span of `rel`'s row in the roster text: from the
/// line that is exactly its quoted path to the next `),`, searched from the
/// roster's begin marker when there is one.
fn row_line_span(text: &str, rel: &str) -> Option<(usize, usize)> {
    let quoted = format!("\"{rel}\",");
    let lines: Vec<&str> = text.lines().collect();
    let from = lines
        .iter()
        .position(|l| l.trim() == "// HELP_SURFACES_ROSTER_BEGIN")
        .unwrap_or(0);
    let begin = from + lines[from..].iter().position(|l| l.trim() == quoted)?;
    let end = begin + lines[begin..].iter().position(|l| l.trim() == "),")?;
    Some((begin + 1, end + 1))
}

/// Of the commits a `git blame --porcelain` transcript gives lines to, the newest
/// by committer time — the all-zero id of a line not committed yet skipped.
fn newest_blamed_commit(porcelain: &str) -> Option<String> {
    let mut current: Option<&str> = None;
    let mut newest: Option<(i64, &str)> = None;
    for line in porcelain.lines() {
        if line.starts_with('\t') {
            continue;
        }
        let first = line.split(' ').next().unwrap_or("");
        if matches!(first.len(), 40 | 64)
            && first.bytes().all(|b| b.is_ascii_hexdigit())
            && line.len() > first.len()
        {
            current = Some(first);
        } else if let Some(t) = line.strip_prefix("committer-time ")
            && let (Some(sha), Ok(t)) = (current, t.trim().parse::<i64>())
            && sha.bytes().any(|b| b != b'0')
            && newest.is_none_or(|(seen, _)| t > seen)
        {
            newest = Some((t, sha));
        }
    }
    newest.map(|(_, sha)| sha.to_string())
}

/// `path` as the roster spells it: workspace-relative, `/`-separated, no `./`.
fn workspace_rel(root: &Path, path: &str) -> String {
    let p = Path::new(path);
    let rel = p
        .strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/");
    rel.strip_prefix("./").map_or(rel.clone(), str::to_string)
}

fn short_sha(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

/// `<sha> <committer date> <subject>`.
fn describe_commit(root: &Path, sha: &str) -> Result<String, String> {
    Ok(
        git_stdout(root, &["show", "-s", "--format=%H %ci %s", sha])?
            .trim_end()
            .to_string(),
    )
}

fn git_stdout(root: &Path, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|e| format!("could not run `git {}`: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!(
            "`git {}` exited {:?} in {}: {}",
            args.join(" "),
            out.status.code(),
            root.display(),
            String::from_utf8_lossy(&out.stderr).trim_end()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// One `git cat-file --batch` for the whole walk: a file with hundreds of
/// commits costs one spawn, not hundreds.
struct Blobs {
    child: std::process::Child,
    stdin: Option<std::process::ChildStdin>,
    out: std::io::BufReader<std::process::ChildStdout>,
}

impl Blobs {
    fn spawn(root: &Path) -> Result<Self, String> {
        use std::process::Stdio;
        let mut child = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("could not run `git cat-file --batch`: {e}"))?;
        let stdin = child.stdin.take();
        let Some(out) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("`git cat-file --batch` gave no stdout".to_string());
        };
        Ok(Self {
            child,
            stdin,
            out: std::io::BufReader::new(out),
        })
    }

    /// The text of `<rev>:<path>`, or `None` when that version has no such file.
    fn read(&mut self, spec: &str) -> Result<Option<String>, String> {
        use std::io::{BufRead as _, Read as _, Write as _};
        let broken = |e: std::io::Error| format!("`git cat-file --batch` broke on {spec}: {e}");
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| "`git cat-file --batch` has no stdin".to_string())?;
        writeln!(stdin, "{spec}").map_err(broken)?;
        stdin.flush().map_err(broken)?;
        let mut header = String::new();
        if self.out.read_line(&mut header).map_err(broken)? == 0 {
            return Err(format!(
                "`git cat-file --batch` ended before answering {spec}"
            ));
        }
        // `<oid> <type> <size>`, or `<spec> missing` (and `ambiguous`) with no
        // body — read off the END, since a spec can hold a space.
        let header = header.trim_end();
        if header.ends_with(" missing") || header.ends_with(" ambiguous") {
            return Ok(None);
        }
        let fields: Vec<&str> = header.split(' ').collect();
        let [_, kind, size] = fields.as_slice() else {
            return Err(format!(
                "`git cat-file --batch` answered {header:?} for {spec}"
            ));
        };
        let size: usize = size
            .parse()
            .map_err(|e| format!("`git cat-file --batch` header {header:?}: {e}"))?;
        let mut body = vec![0u8; size + 1];
        self.out.read_exact(&mut body).map_err(broken)?;
        body.pop();
        if *kind != "blob" {
            return Ok(None);
        }
        Ok(Some(String::from_utf8_lossy(&body).into_owned()))
    }
}

impl Drop for Blobs {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let _ = self.child.wait();
    }
}

/// The prose as [`prose_hash`] sees it, as lines to diff: every string literal,
/// then every doc comment — the order the hash feeds them — one item per line.
/// See [`prose_lines_of`] for an item whose text breaks.
pub(crate) fn prose_lines(src: &str) -> Vec<String> {
    let (strings, docs) = prose_of_rust(src);
    prose_lines_of(&strings, &docs)
}

/// [`prose_lines`] over items already extracted. `str>` opens a literal and
/// `doc>` a doc comment; an item whose text breaks — a newline in it, or a `\n`
/// escape in a literal — continues on `str|`/`doc|` lines, so a 200-line manual
/// page diffs by the line that changed rather than as one line. A literal breaks
/// AFTER its `\n` escape (the escape stays, visible), and one that ends in a `\n`
/// escape gets no empty last line. A piece longer than [`WRAP_AT`] bytes — a
/// catalog row continued over forty source lines is one piece of 5 KB — is also
/// soft-wrapped after each `. `, continuing on `str~`/`doc~` lines that rejoin
/// with one space, so an added sentence shows as that sentence. Every break is
/// recoverable from the lines, so two sources render the same lines exactly when
/// they hash the same, and a diff of these lines is empty exactly when the gate
/// sees no change.
pub(crate) fn prose_lines_of(strings: &[String], docs: &[String]) -> Vec<String> {
    let mut lines = Vec::new();
    for s in strings {
        push_prose_item(&mut lines, "str", s, true);
    }
    for d in docs {
        push_prose_item(&mut lines, "doc", d, false);
    }
    lines
}

/// A piece of an item longer than this many bytes is soft-wrapped at its
/// sentence ends (see [`prose_lines_of`]); a manual page's lines stay whole.
const WRAP_AT: usize = 100;

fn push_prose_item(lines: &mut Vec<String>, tag: &str, text: &str, escapes: bool) {
    let mut mark = '>';
    let mut emit = |piece: &str| {
        let sentences: Vec<&str> = if piece.len() > WRAP_AT {
            piece.split(". ").collect()
        } else {
            vec![piece]
        };
        for (k, sentence) in sentences.iter().enumerate() {
            let m = if k == 0 { mark } else { '~' };
            let dot = if k + 1 < sentences.len() { "." } else { "" };
            lines.push(if sentence.is_empty() && dot.is_empty() {
                format!("{tag}{m}")
            } else {
                format!("{tag}{m} {sentence}{dot}")
            });
        }
        mark = '|';
    };
    let mut start = 0;
    let mut after_escape = false;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c == '\n' {
            emit(&text[start..i]);
            start = i + 1;
            after_escape = false;
        } else if c == '\\' && escapes {
            match chars.peek() {
                Some(&(j, 'n')) => {
                    chars.next();
                    emit(&text[start..=j]);
                    start = j + 1;
                    after_escape = true;
                }
                // A backslash that ends a raw string's line escapes nothing: the
                // newline after it breaks on its own.
                Some(&(_, '\n')) | None => {}
                Some(_) => {
                    chars.next();
                }
            }
        }
    }
    if start < text.len() || !after_escape {
        emit(&text[start..]);
    }
}

/// What [`unified_diff`] found: the text (empty when the sides are equal) and
/// its tallies.
#[derive(Debug, Default)]
pub(crate) struct LineDiff {
    pub(crate) text: String,
    pub(crate) hunks: usize,
    pub(crate) removed: usize,
    pub(crate) added: usize,
}

/// A unified diff of `old` -> `new` with `context` lines around each change:
/// `---`/`+++` headed, `@@ -a,b +c,d @@` hunks (a count of 1 omitted, as git
/// does), a change's removals before its additions. The edit is a shortest one
/// ([`edit_script`]). Empty, with zero tallies, when the lines are equal.
pub(crate) fn unified_diff(
    old_label: &str,
    new_label: &str,
    old: &[String],
    new: &[String],
    context: usize,
) -> LineDiff {
    let mut ids = std::collections::HashMap::new();
    let a: Vec<u32> = old.iter().map(|s| intern(&mut ids, s)).collect();
    let b: Vec<u32> = new.iter().map(|s| intern(&mut ids, s)).collect();
    // Each edit with the old and new index it applies at.
    let mut at = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0, 0);
    for e in edit_script(&a, &b) {
        at.push((e, i, j));
        match e {
            Edit::Keep => (i, j) = (i + 1, j + 1),
            Edit::Del => i += 1,
            Edit::Ins => j += 1,
        }
    }
    let changed: Vec<usize> = (0..at.len()).filter(|&k| at[k].0 != Edit::Keep).collect();
    let mut diff = LineDiff::default();
    if changed.is_empty() {
        return diff;
    }
    let _ = writeln!(diff.text, "--- {old_label}\n+++ {new_label}");
    let mut g = 0;
    while g < changed.len() {
        let mut h = g;
        while h + 1 < changed.len() && changed[h + 1] - changed[h] - 1 <= 2 * context {
            h += 1;
        }
        let span =
            &at[changed[g].saturating_sub(context)..(changed[h] + context + 1).min(at.len())];
        let old_n = span.iter().filter(|x| x.0 != Edit::Ins).count();
        let new_n = span.iter().filter(|x| x.0 != Edit::Del).count();
        let _ = writeln!(
            diff.text,
            "@@ -{} +{} @@",
            hunk_range(span[0].1, old_n),
            hunk_range(span[0].2, new_n)
        );
        let mut k = 0;
        while k < span.len() {
            if span[k].0 == Edit::Keep {
                let _ = writeln!(diff.text, " {}", old[span[k].1]);
                k += 1;
                continue;
            }
            let run = &span[k..k + span[k..].iter().take_while(|x| x.0 != Edit::Keep).count()];
            for &(_, oi, _) in run.iter().filter(|x| x.0 == Edit::Del) {
                let _ = writeln!(diff.text, "-{}", old[oi]);
                diff.removed += 1;
            }
            for &(_, _, ni) in run.iter().filter(|x| x.0 == Edit::Ins) {
                let _ = writeln!(diff.text, "+{}", new[ni]);
                diff.added += 1;
            }
            k += run.len();
        }
        diff.hunks += 1;
        g = h + 1;
    }
    diff
}

fn intern<'a>(ids: &mut std::collections::HashMap<&'a str, u32>, s: &'a str) -> u32 {
    let next = u32::try_from(ids.len()).unwrap_or(u32::MAX);
    *ids.entry(s).or_insert(next)
}

/// A hunk side's `start,count`: 1-based, except that an empty side names the
/// line before it (`0,0` at the top), and a count of 1 is omitted.
fn hunk_range(index: usize, count: usize) -> String {
    match count {
        0 => format!("{index},0"),
        1 => format!("{}", index + 1),
        n => format!("{},{n}", index + 1),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edit {
    Keep,
    Del,
    Ins,
}

/// A shortest edit script from `a` to `b` — its `Keep`s are a longest common
/// subsequence — by Hirschberg's divide and conquer: linear space, and the
/// common prefix and suffix peeled at every level cost nothing.
fn edit_script(a: &[u32], b: &[u32]) -> Vec<Edit> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    hirschberg(a, b, &mut out);
    out
}

fn hirschberg(a: &[u32], b: &[u32], out: &mut Vec<Edit>) {
    use std::iter::repeat_n;
    let pre = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    out.extend(repeat_n(Edit::Keep, pre));
    let (a, b) = (&a[pre..], &b[pre..]);
    let suf = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (a, b) = (&a[..a.len() - suf], &b[..b.len() - suf]);
    if a.is_empty() || b.is_empty() {
        out.extend(repeat_n(Edit::Del, a.len()));
        out.extend(repeat_n(Edit::Ins, b.len()));
    } else if a.len() == 1 {
        if let Some(j) = b.iter().position(|y| *y == a[0]) {
            out.extend(repeat_n(Edit::Ins, j));
            out.push(Edit::Keep);
            out.extend(repeat_n(Edit::Ins, b.len() - j - 1));
        } else {
            out.push(Edit::Del);
            out.extend(repeat_n(Edit::Ins, b.len()));
        }
    } else {
        let mid = a.len() / 2;
        let fwd = lcs_prefix_row(&a[..mid], b);
        let bwd = lcs_suffix_row(&a[mid..], b);
        let split = (0..=b.len())
            .max_by_key(|&k| (fwd[k] + bwd[k], std::cmp::Reverse(k)))
            .unwrap_or(0);
        hirschberg(&a[..mid], &b[..split], out);
        hirschberg(&a[mid..], &b[split..], out);
    }
    out.extend(repeat_n(Edit::Keep, suf));
}

/// `row[k]` = the LCS length of `a` and `b[..k]`.
fn lcs_prefix_row(a: &[u32], b: &[u32]) -> Vec<usize> {
    let mut prev = vec![0; b.len() + 1];
    let mut cur = vec![0; b.len() + 1];
    for x in a {
        for (k, y) in b.iter().enumerate() {
            cur[k + 1] = if x == y {
                prev[k] + 1
            } else {
                prev[k + 1].max(cur[k])
            };
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev
}

/// `row[k]` = the LCS length of `a` and `b[k..]`.
fn lcs_suffix_row(a: &[u32], b: &[u32]) -> Vec<usize> {
    let mut prev = vec![0; b.len() + 1];
    let mut cur = vec![0; b.len() + 1];
    for x in a.iter().rev() {
        for (k, y) in b.iter().enumerate().rev() {
            cur[k] = if x == y {
                prev[k + 1] + 1
            } else {
                prev[k].max(cur[k + 1])
            };
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev
}

// ---------------------------------------------------------------------- dates

fn today_days() -> i64 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    i64::try_from(secs / 86_400).unwrap_or(0)
}

/// `YYYY-MM-DD` -> days since 1970-01-01 (proleptic Gregorian); `None` when
/// malformed.
fn days_from_iso(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let y: i64 = s[0..4].parse().ok()?;
    let m: i64 = s[5..7].parse().ok()?;
    let d: i64 = s[8..10].parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // Howard Hinnant's days_from_civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// Inverse of [`days_from_iso`], for the rows the refusal lines print.
fn civil_from_days(z: i64) -> String {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "//! A tool.\nconst USAGE: &str = \"Usage: tool <file> [--fast]\\n\\nOptions:\\n  --fast  skip checks\";\nfn main() { let _c = '\"'; // not a \"string\"\n    /* plain comment \"not prose\" */ println!(\"{USAGE}\"); }\n";
    const OTHER: &str = "fn helper() -> u8 { 7 } // no help here\n";
    const TODAY: i64 = 20_706; // 2026-09-10

    fn tree(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "xtask-help-surfaces-{}-{label}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("crates/t/src")).unwrap();
        std::fs::write(
            root.join("crates/t/Cargo.toml"),
            "[package]\nname = \"t\"\n",
        )
        .unwrap();
        std::fs::write(root.join("crates/t/src/main.rs"), SRC).unwrap();
        std::fs::write(root.join("crates/t/src/helper.rs"), OTHER).unwrap();
        root
    }

    fn tags(refusals: &[String]) -> String {
        refusals
            .iter()
            .map(|r| r.split_whitespace().next().unwrap_or("").to_string())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The verb-level red fixture: the REAL check over a temp tree, GREEN on a
    /// read row and RED — with the rule named — on every way a roster can lie.
    #[test]
    fn a_changed_or_unrostered_help_surface_fails_the_help_surfaces_verb() {
        let root = tree("verb");
        let hash = read_prose_hash(&root.join("crates/t/src/main.rs")).unwrap();
        let row = (
            "crates/t/src/main.rs",
            hash.as_str(),
            "2026-09-10",
            "fixture",
        );
        let check = |surfaces: &[SurfaceRow<'_>], not_help: &[NotHelpRow<'_>]| {
            help_surfaces_check(&root, &["crates"], surfaces, not_help, 180, TODAY)
        };

        assert!(
            check(&[row], &[]).is_empty(),
            "control: a read row is green"
        );
        let unrostered = check(&[], &[]);
        assert!(!unrostered.is_empty(), "no row at all must be RED");
        assert!(tags(&unrostered).contains("R1"), "{unrostered:?}");
        assert!(
            tags(&check(&[row], &[("crates/t/src/helper.rs", "planted")])).contains("R5"),
            "NOT_HELP naming a file discovery does not find"
        );
        assert!(
            tags(&check(&[row, row], &[])).contains("R6"),
            "the same path rostered twice"
        );
        assert!(
            tags(&check(
                &[("crates/t/src/main.rs", &hash, "2025-01-01", "f")],
                &[]
            ))
            .contains("R4"),
            "a row older than the limit"
        );
        assert!(
            tags(&check(
                &[("crates/t/src/main.rs", &hash, "yesterday", "f")],
                &[]
            ))
            .contains("R4"),
            "a malformed date"
        );

        // A code-only edit leaves the row green; a help-literal edit turns it red.
        std::fs::write(
            root.join("crates/t/src/main.rs"),
            SRC.replace("let _c", "let _d"),
        )
        .unwrap();
        assert!(check(&[row], &[]).is_empty(), "code-only edit: still green");
        std::fs::write(
            root.join("crates/t/src/main.rs"),
            SRC.replace("skip checks", "skip nothing"),
        )
        .unwrap();
        let red = check(&[row], &[]);
        assert!(!red.is_empty(), "an edited help literal must be RED");
        assert!(tags(&red).contains("R2"), "edited help literal: {red:?}");
        assert!(
            red.iter().any(|r| r.contains("2026-09-10")),
            "the refusal carries today's row to paste: {red:?}"
        );
        assert!(
            red.iter().any(|r| r.starts_with("R2")
                && r.ends_with("(see: xtask gate help-surfaces --diff crates/t/src/main.rs)")),
            "the R2 line ends by naming the verb that shows what changed: {red:?}"
        );

        // A program with no help text and no row: R7, cleared by a NOT_HELP row
        // that R5 must not then refuse.
        std::fs::write(root.join("crates/t/src/main.rs"), SRC).unwrap();
        std::fs::create_dir_all(root.join("crates/t/src/bin")).unwrap();
        std::fs::write(
            root.join("crates/t/src/bin/quiet.rs"),
            "fn main() { let _ = std::env::args(); }\n",
        )
        .unwrap();
        let quiet = check(&[row], &[]);
        assert!(!quiet.is_empty(), "an unaccounted program must be RED");
        assert!(tags(&quiet).contains("R7"), "{quiet:?}");
        assert!(
            check(
                &[row],
                &[("crates/t/src/bin/quiet.rs", "takes no arguments")]
            )
            .is_empty(),
            "a NOT_HELP row accounts for a program"
        );
        std::fs::remove_file(root.join("crates/t/src/bin/quiet.rs")).unwrap();

        // A non-`.rs` file the surface COMPILES IN: R8, because the lexer sees
        // only the path at the macro, so the asset's text is in no hash. A
        // `.rs` include is NOT R8's question — the `*.rs` walk already reaches
        // it — and a NOT_HELP row clears the real one without R5 refusing it.
        std::fs::create_dir_all(root.join("crates/t/assets")).unwrap();
        std::fs::write(
            root.join("crates/t/assets/guide.md"),
            "Usage: tool <file>\n",
        )
        .unwrap();
        std::fs::write(root.join("crates/t/assets/scan.rs"), "fn scanned() {}\n").unwrap();
        let with_rs_include = SRC.replace(
            "fn main()",
            "const S: &str = include_str!(\"../assets/scan.rs\");\nfn main()",
        );
        std::fs::write(root.join("crates/t/src/main.rs"), &with_rs_include).unwrap();
        let rs_hash = read_prose_hash(&root.join("crates/t/src/main.rs")).unwrap();
        let rs_row = (
            "crates/t/src/main.rs",
            rs_hash.as_str(),
            "2026-09-13",
            "fixture",
        );
        assert!(
            check(&[rs_row], &[]).is_empty(),
            "a .rs include is R1's question, not R8's"
        );
        let both = with_rs_include.replace(
            "fn main()",
            "const G: &str = include_str!(\"../assets/guide.md\");\nfn main()",
        );
        std::fs::write(root.join("crates/t/src/main.rs"), &both).unwrap();
        let both_hash = read_prose_hash(&root.join("crates/t/src/main.rs")).unwrap();
        let both_row = (
            "crates/t/src/main.rs",
            both_hash.as_str(),
            "2026-09-13",
            "fixture",
        );
        let embedded = check(&[both_row], &[]);
        assert!(
            !embedded.is_empty(),
            "an unaccounted embedded file must be RED"
        );
        assert!(tags(&embedded).contains("R8"), "{embedded:?}");
        assert!(
            check(&[both_row], &[("crates/t/assets/guide.md", "planted")]).is_empty(),
            "a NOT_HELP row accounts for an embedded file"
        );
        std::fs::write(root.join("crates/t/src/main.rs"), SRC).unwrap();
        std::fs::remove_dir_all(root.join("crates/t/assets")).unwrap();

        std::fs::remove_file(root.join("crates/t/src/main.rs")).unwrap();
        assert!(
            tags(&check(&[row], &[])).contains("R3"),
            "rostered file removed"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The gate itself, in the test lane: the workspace roster is complete and
    /// current. A red here names the surface and prints the row a fresh read
    /// should record.
    #[test]
    fn the_workspace_help_surfaces_are_read_verified() {
        let (ok, log) = help_surfaces_report(&crate::workspace_root());
        assert!(ok, "{log}");
    }

    #[test]
    fn extraction_keeps_literals_and_doc_comments_and_drops_the_rest() {
        let (strings, docs) = prose_of_rust(SRC);
        assert_eq!(docs, vec!["A tool.".to_string()]);
        assert_eq!(strings.len(), 2, "{strings:?}");
        assert!(strings[0].starts_with("Usage: tool"));
        assert!(
            !strings
                .iter()
                .any(|s| s.contains("not prose") || s.contains("not a"))
        );
        let (raw, _) = prose_of_rust("let r = r#\"a \"quoted\" b\"#; let c = '\\'';");
        assert_eq!(raw, vec!["a \"quoted\" b".to_string()]);
    }

    #[test]
    fn iso_dates_round_trip() {
        for (iso, days) in [
            ("1970-01-01", 0),
            ("2026-09-10", TODAY),
            ("2000-02-29", 11_016),
        ] {
            assert_eq!(days_from_iso(iso), Some(days), "{iso}");
            assert_eq!(civil_from_days(days), iso);
        }
        assert_eq!(days_from_iso("2026-13-01"), None);
        assert_eq!(days_from_iso("26-09-10"), None);
    }

    /// Rebuilds the items [`prose_lines_of`] rendered — the inverse the doc
    /// promises, which is what makes "the lines differ exactly when the hash
    /// does" true rather than hoped.
    fn items_of_lines(lines: &[String]) -> (Vec<String>, Vec<String>) {
        fn ends_in_escaped_n(line: &str) -> bool {
            let mut chars = line.chars().peekable();
            let mut last_was_n_escape = false;
            while let Some(c) = chars.next() {
                last_was_n_escape = false;
                if c == '\\'
                    && let Some(&next) = chars.peek()
                {
                    chars.next();
                    last_was_n_escape = next == 'n';
                }
            }
            last_was_n_escape
        }
        let (mut strings, mut docs) = (Vec::new(), Vec::new());
        let mut open: Option<(bool, String, bool)> = None; // (doc, text, last line ended in `\n`)
        let close =
            |open: Option<(bool, String, bool)>, s: &mut Vec<String>, d: &mut Vec<String>| {
                if let Some((doc, text, _)) = open {
                    if doc { d.push(text) } else { s.push(text) }
                }
            };
        for line in lines {
            let (tag, rest) = line.split_at(3);
            let mark = rest.chars().next().expect("a mark after the tag");
            let body = rest.get(2..).unwrap_or("");
            let doc = tag == "doc";
            if mark == '>' {
                close(open.take(), &mut strings, &mut docs);
                open = Some((doc, body.to_string(), !doc && ends_in_escaped_n(body)));
            } else {
                let (_, text, escaped) = open.as_mut().expect("a continuation opens nothing");
                if mark == '~' {
                    text.push(' ');
                } else if !*escaped {
                    text.push('\n');
                }
                text.push_str(body);
                *escaped = !doc && ends_in_escaped_n(body);
            }
        }
        close(open, &mut strings, &mut docs);
        (strings, docs)
    }

    /// `--diff`'s split, on the shape a naive grep for quotes misses: a literal
    /// continued across source lines (`\` at the line end) whose middle line has
    /// no quote at all, and a sentence continued with no `\n` in it. Both diff,
    /// by the line that changed.
    #[test]
    fn a_continued_literal_diffs_on_its_quoteless_continuation_line() {
        let old = "const USAGE: &str = \"usage: t <file>\\n\\\n    --fast  skip checks\\n\\\n    \
                   --slow  run all\";\nconst NOTE: &str = \"one sentence that \\\n    continues here\";\n";
        let new = old
            .replace("skip checks", "skip nothing")
            .replace("continues here", "continues there");
        assert_eq!(
            prose_lines(old),
            [
                "str> usage: t <file>\\n",
                "str| --fast  skip checks\\n",
                "str| --slow  run all",
                "str> one sentence that continues here",
            ],
            "one line per `\\n`-ended piece, and a joined continuation is one line"
        );
        let d = unified_diff("old", "new", &prose_lines(old), &prose_lines(&new), 3);
        assert_eq!(
            d.text,
            "--- old\n+++ new\n@@ -1,4 +1,4 @@\n str> usage: t <file>\\n\n-str| --fast  skip checks\\n\n\
             +str| --fast  skip nothing\\n\n str| --slow  run all\n-str> one sentence that continues \
             here\n+str> one sentence that continues there\n"
        );
        assert_eq!((d.hunks, d.removed, d.added), (1, 2, 2));
    }

    #[test]
    fn an_unchanged_or_code_only_edited_file_diffs_empty() {
        let lines = prose_lines(SRC);
        for other in [SRC.to_string(), SRC.replace("let _c", "let _d")] {
            let d = unified_diff("a", "b", &lines, &prose_lines(&other), 3);
            assert!(d.text.is_empty(), "{}", d.text);
            assert_eq!((d.hunks, d.removed, d.added), (0, 0, 0));
        }
    }

    /// A catalog row continued over many source lines is ONE piece of prose; a
    /// sentence added to it shows as that sentence, not as the row twice.
    #[test]
    fn a_sentence_added_to_a_long_catalog_row_shows_as_that_sentence() {
        let row = |extra: &str| {
            format!(
                "const ROW: &str = \"trail [<n>]: the focused window's last verdicts, newest \\\n    \
                 last. `licence=` names the class that admitted the row. {extra}Every observed \\\n    \
                 move is counted exactly once.\";\n"
            )
        };
        let (old, new) = (row(""), row("A held park is judged as one echo. "));
        assert_eq!(
            prose_lines(&old),
            [
                "str> trail [<n>]: the focused window's last verdicts, newest last.",
                "str~ `licence=` names the class that admitted the row.",
                "str~ Every observed move is counted exactly once.",
            ]
        );
        let d = unified_diff("a", "b", &prose_lines(&old), &prose_lines(&new), 3);
        assert_eq!((d.hunks, d.removed, d.added), (1, 0, 1), "{}", d.text);
        assert!(
            d.text
                .contains("\n+str~ A held park is judged as one echo.\n"),
            "{}",
            d.text
        );
    }

    #[test]
    fn a_doc_comment_only_edit_shows_in_the_diff() {
        let old = "/// Frobs the widget.\n/// Twice.\npub fn frob() -> &'static str { \"frob\" }\n";
        let new = old.replace("the widget", "every widget");
        let d = unified_diff("a", "b", &prose_lines(old), &prose_lines(&new), 3);
        assert_eq!(
            d.text,
            "--- a\n+++ b\n@@ -1,3 +1,3 @@\n str> frob\n-doc> Frobs the widget.\n\
             +doc> Frobs every widget.\n doc> Twice.\n"
        );
    }

    /// Every break in an item is recoverable from its lines — tricky texts and
    /// the prose of every rostered surface in this tree alike — so two sources
    /// render the same lines only when the hasher sees the same items.
    #[test]
    fn prose_lines_render_every_item_recoverably() {
        let tricky: Vec<String> = [
            "",
            "\\n",
            "a\\nb",
            "a\\n\nb",
            "a\\n",
            "a\\n\n",
            "a\nb",
            "a\n",
            "\n",
            "a\\\\nb",
            "C:\\",
            "shell \\\n  --flag",
            "tab\\tthen\\n\\nblank",
            "ü\\n€\\n",
            "\\\"quoted\\\"\\n",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .chain([
            format!("{}. {}. ", "a".repeat(60), "b".repeat(60)),
            format!("One. Two has an escape\\n{}. Tail. ", "c".repeat(120)),
            format!("{}\\. then more. {}", "d".repeat(80), "e".repeat(40)),
            format!("{}. . .\\n. x", "f".repeat(101)),
        ])
        .collect();
        for t in &tricky {
            let items = (
                vec![t.clone(), "x".to_string()],
                vec![t.replace("\\n", "n")],
            );
            assert_eq!(
                items_of_lines(&prose_lines_of(&items.0, &items.1)),
                items,
                "{t:?}"
            );
        }
        for (a, b) in [
            (vec!["a\\nb"], vec!["a\\n\nb"]),
            (vec!["ab"], vec!["a", "b"]),
            (vec!["a\\n", ""], vec!["a\\n"]),
        ] {
            let own = |v: Vec<&str>| v.into_iter().map(str::to_string).collect::<Vec<_>>();
            assert_ne!(
                prose_lines_of(&own(a.clone()), &[]),
                prose_lines_of(&own(b.clone()), &[]),
                "{a:?} vs {b:?}"
            );
        }
        let s = vec!["x".to_string()];
        assert_ne!(prose_lines_of(&s, &[]), prose_lines_of(&[], &s));

        let root = crate::workspace_root();
        for (rel, ..) in SURFACES {
            let src = std::fs::read_to_string(root.join(rel)).unwrap();
            let (strings, docs) = prose_of_rust(&src);
            let lines = prose_lines_of(&strings, &docs);
            assert!(
                lines.iter().all(|l| !l.contains('\n')),
                "{rel}: a line holds a newline"
            );
            assert_eq!(items_of_lines(&lines), (strings, docs), "{rel}");
        }
    }

    /// The hunks git would print, and a shortest edit on random pairs: the
    /// script rebuilds `new` from `old`, and its keeps are a longest common
    /// subsequence (checked against the full table).
    #[test]
    fn the_line_diff_is_a_shortest_edit_in_unified_hunks() {
        let old: Vec<String> = (1..=12).map(|n| n.to_string()).collect();
        let mut new = old.clone();
        new[0] = "one".into();
        new[11] = "twelve".into();
        assert_eq!(
            unified_diff("a", "b", &old, &new, 3).text,
            "--- a\n+++ b\n@@ -1,4 +1,4 @@\n-1\n+one\n 2\n 3\n 4\n@@ -9,4 +9,4 @@\n 9\n 10\n 11\n-12\n+twelve\n"
        );
        new[5] = "six".into();
        let one_hunk = unified_diff("a", "b", &old, &new, 3);
        assert_eq!(
            one_hunk.hunks, 1,
            "changes 4 lines apart share a hunk at context 3"
        );
        assert_eq!((one_hunk.removed, one_hunk.added), (3, 3));
        assert_eq!(
            unified_diff("a", "b", &[], &["x".to_string()], 3).text,
            "--- a\n+++ b\n@@ -0,0 +1 @@\n+x\n"
        );

        fn lcs_len(a: &[u32], b: &[u32]) -> usize {
            let mut t = vec![vec![0; b.len() + 1]; a.len() + 1];
            for i in 0..a.len() {
                for j in 0..b.len() {
                    t[i + 1][j + 1] = if a[i] == b[j] {
                        t[i][j] + 1
                    } else {
                        t[i][j + 1].max(t[i + 1][j])
                    };
                }
            }
            t[a.len()][b.len()]
        }
        let mut seed: u64 = 0x2026_0913;
        let mut next = |m: u64| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (seed >> 33) % m
        };
        for _ in 0..500 {
            let a: Vec<u32> = (0..next(10)).map(|_| next(4) as u32).collect();
            let b: Vec<u32> = (0..next(10)).map(|_| next(4) as u32).collect();
            let (mut i, mut j, mut keeps, mut rebuilt) = (0, 0, 0, Vec::new());
            for e in edit_script(&a, &b) {
                match e {
                    Edit::Keep => {
                        assert_eq!(a[i], b[j], "{a:?} -> {b:?}");
                        rebuilt.push(a[i]);
                        (i, j, keeps) = (i + 1, j + 1, keeps + 1);
                    }
                    Edit::Del => i += 1,
                    Edit::Ins => {
                        rebuilt.push(b[j]);
                        j += 1;
                    }
                }
            }
            assert_eq!((i, j), (a.len(), b.len()), "{a:?} -> {b:?}");
            assert_eq!(rebuilt, b, "{a:?} -> {b:?}");
            assert_eq!(
                keeps,
                lcs_len(&a, &b),
                "{a:?} -> {b:?}: not a shortest edit"
            );
        }
    }

    #[test]
    fn blame_porcelain_names_the_newest_committed_owner() {
        let z = "0".repeat(40);
        let (old, new) = ("a".repeat(40), "b".repeat(40));
        let porcelain = format!(
            "{old} 3 3 1\nauthor x\ncommitter-time 100\nsummary s\nfilename f\n\t\"p\",\n\
             {new} 4 4 1\ncommitter-time 300\nfilename f\n\t\"h\",\n\
             {z} 5 5 1\ncommitter-time 900\nfilename f\n\t\"d\",\n{old} 6 6\n\t),\n"
        );
        assert_eq!(newest_blamed_commit(&porcelain), Some(new));
        assert_eq!(
            newest_blamed_commit(&format!("{z} 1 1 1\ncommitter-time 9\n\tx\n")),
            None
        );
        let roster = "x\n    // HELP_SURFACES_ROSTER_BEGIN\n    (\n        \"a/b.rs\",\n        \"h\",\n    ),\n";
        assert_eq!(row_line_span(roster, "a/b.rs"), Some((4, 6)));
        assert_eq!(row_line_span(roster, "a/c.rs"), None);
    }

    #[test]
    fn diff_flags_other_than_diff_path_are_refused() {
        let args = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        assert!(!gate_help_surfaces_args(&args(&["--dif", "x"])));
        assert!(!gate_help_surfaces_args(&args(&["--diff"])));
        let root = crate::workspace_root();
        let (ok, out) =
            prose_diff_report(&root, "crates/no/such.rs", SURFACES, NOT_HELP, ROSTER_FILE);
        assert!(!ok && out.contains("has no row in SURFACES"), "{out}");
        let (ok, out) = prose_diff_report(&root, ROSTER_FILE, SURFACES, NOT_HELP, ROSTER_FILE);
        assert!(!ok && out.contains("is in NOT_HELP"), "{out}");
    }

    /// `--diff` over a real (scratch) history: it names the NEWEST commit whose
    /// version hashes to the row — a code-only commit on top of the read still
    /// matches — and diffs only the prose changed since; when no committed version
    /// matches, it says so and diffs from the commit that last touched the row.
    #[test]
    fn diff_walks_history_to_the_recorded_read_or_falls_back_to_the_rows_commit() {
        let root = std::env::temp_dir().join(format!(
            "xtask-help-surfaces-{}-diff-history",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("crates/t/src")).unwrap();
        let rel = "crates/t/src/main.rs";
        let mut day = 0;
        let mut commit = |paths: &[&str], msg: &str| -> String {
            day += 1;
            let date = format!("2026-09-{day:02}T12:00:00+0000");
            let git = |args: &[&str]| {
                let out = std::process::Command::new("git")
                    .arg("-C")
                    .arg(&root)
                    .args([
                        "-c",
                        "commit.gpgsign=false",
                        "-c",
                        "core.hooksPath=/dev/null",
                    ])
                    .args([
                        "-c",
                        "user.name=fixture",
                        "-c",
                        "user.email=fixture@example.invalid",
                    ])
                    .args(args)
                    .env("GIT_AUTHOR_DATE", &date)
                    .env("GIT_COMMITTER_DATE", &date)
                    .output()
                    .expect("git runs");
                assert!(out.status.success(), "git {args:?}: {out:?}");
                String::from_utf8_lossy(&out.stdout).trim().to_string()
            };
            if day == 1 {
                git(&["init", "-q"]);
            }
            git(&[&["add", "--"][..], paths].concat());
            git(&["commit", "-q", "--no-verify", "-m", msg]);
            git(&["rev-parse", "HEAD"])
        };
        let write = |name: &str, text: &str| std::fs::write(root.join(name), text).unwrap();
        let roster = |hash: &str| {
            format!(
                "    (\n        \"{rel}\",\n        \"{hash}\",\n        \"2026-09-10\",\n        \
                 \"fixture\",\n    ),\n"
            )
        };
        let v1 = "/// The tool.\nconst USAGE: &str = \"usage: t\\n\\\n    --fast  skip checks\";\n\
                  fn main() {}\n";
        let v2 = v1.replace("The tool.", "The tool, read.");
        let v3 = v2.replace("fn main() {}", "fn main() { let _ = 1; }");
        let v4 = v3.replace("skip checks", "skip nothing");
        write(rel, v1);
        commit(&[rel], "v1: first text");
        write(rel, &v2);
        commit(&[rel], "v2: the text that was read");
        write(rel, &v3);
        let c3 = commit(&[rel], "v3: a code-only edit");
        let read = source_prose_hash(&v2);
        write("roster.rs", &roster(&read));
        commit(&["roster.rs"], "the read's row");
        write(rel, &v4);

        let surfaces = [(rel, read.as_str(), "2026-09-10", "fixture")];
        let (ok, out) = prose_diff_report(&root, rel, &surfaces, &[], "roster.rs");
        assert!(ok, "{out}");
        assert!(
            out.contains(&format!(
                "matched:  {c3} 2026-09-03 12:00:00 +0000 v3: a code-only edit\n"
            )),
            "the newest matching commit, with its date and subject: {out}"
        );
        assert!(
            out.contains("(1 of the 3 commit(s) that touch it walked)"),
            "{out}"
        );
        assert!(
            out.contains("-str| --fast  skip checks\n+str| --fast  skip nothing\n"),
            "{out}"
        );
        assert!(
            !out.contains("-doc> ") && !out.contains("+doc> "),
            "the doc edit before the read is not a change since it: {out}"
        );
        assert!(
            out.contains(&format!(
                "since:    only the working tree's own uncommitted edits to {rel}\n"
            )),
            "{out}"
        );

        // A row recorded from a tree no commit holds: the fallback names the
        // commit that last touched the row, and says its text is not the read.
        let uncommitted = source_prose_hash(&v4.replace("usage", "Usage"));
        write("roster.rs", &roster(&uncommitted));
        let row_commit = commit(
            &["roster.rs"],
            "the row re-recorded from an uncommitted tree",
        );
        let surfaces = [(rel, uncommitted.as_str(), "2026-09-10", "fixture")];
        let (ok, out) = prose_diff_report(&root, rel, &surfaces, &[], "roster.rs");
        assert!(ok, "{out}");
        assert!(
            out.contains("matched:  none — no committed version"),
            "{out}"
        );
        assert!(out.contains(&format!("fallback: {row_commit} ")), "{out}");
        assert!(out.contains("is NOT the text that was read"), "{out}");
        assert!(out.contains("+str| --fast  skip nothing\n"), "{out}");

        // Once the edit is committed, it is named as the change since the read.
        let c6 = commit(&[rel], "v4: the edit since the read");
        let surfaces = [(rel, read.as_str(), "2026-09-10", "fixture")];
        let (ok, out) = prose_diff_report(&root, rel, &surfaces, &[], "roster.rs");
        assert!(ok, "{out}");
        assert!(out.contains(&format!("matched:  {c3} ")), "{out}");
        let since = format!("since:    1 commit(s) touch {rel} after it, newest first:\n");
        let named = out
            .split_once(since.as_str())
            .and_then(|(_, rest)| rest.lines().next())
            .and_then(|l| l.strip_prefix("          "))
            .and_then(|l| l.split_once(' '));
        assert!(
            named.is_some_and(|(abbrev, rest)| c6.starts_with(abbrev)
                && abbrev.len() >= 7
                && rest == "2026-09-06 v4: the edit since the read"),
            "{out}"
        );
        assert!(!out.contains("uncommitted edits"), "{out}");

        // The working tree IS the read: nothing to diff.
        let now = source_prose_hash(&v4);
        let surfaces = [(rel, now.as_str(), "2026-09-10", "fixture")];
        let (ok, out) = prose_diff_report(&root, rel, &surfaces, &[], "roster.rs");
        assert!(ok && out.contains("the row is green"), "{out}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
