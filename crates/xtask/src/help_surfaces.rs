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
        "317c69c5f318b75f",
        "2026-09-17",
        "read crates/aterm-agent/src/supervise/run.rs's outage prose (the module doc, CtlReply::lost and the LOST/LOST_SOCKET/TURNED_AWAY docs, DEFAULT_RECONNECT/RECONNECT_PAUSE, Fail, Past, Rode, Outage, Looking.moved, Pressing, Session's outage/probing/last/stray fields and the call, unserved, fault, read, wait, spent_turn, await_turn, await_turn_from, supervise, watch, drive, look, ride_out, auto_read, moved_past, press_one_guarded, turn_of, stray_digit and render_phase docs, and the test docs this change added or rewrote: the mock's handoff model and the fifteen outage tests) against Session::call's outage books (kind = the verb and its first word, ended by that kind served or an `await seq` that latched, never by the probe), unserved's no-such-session-only-in-an-outage rule, lost's whole-phrase TURNED_AWAY match, wait's unserved-before-124 order, ride_out's since-anchored window, told/back flags, outage-wide pause, top-of-function deadline check and Rode::Spent, drive's fresh-only handed reset and look-through outage reset, look's take of state.moved, its post-deadline stray check and its deadline check before a review point is reported, auto_read's Past::Still(None) arm returning the parsed turn unread (current_turn, its last caller gone, removed with its doc line), moved_past's below-seq check read, the fallback press's Pressing::Lost cases, the server's `ERR control server busy; retry` and `ERR auth` lines (crates/aterm-gui/src/control.rs) and SeqAdvanced's content_seq > after latch (crates/aterm-core/src/terminal/observe.rs), and aterm-ctl's resolve_path/self_instance_sock order and exchange's connect-then-token-read (crates/aterm-ctl/src/lib.rs), by feat/handoff-survival on 2026-09-12 after the adversarial review of f6845c667; five slips fixed before this row (TURNED_AWAY had the `latest` race as token read before connect, but exchange connects first; ride_out said the probe goes through the same socket and that the default socket is the aterm.sock alias, where every aterm-ctl run re-resolves and prefers the instance hosting the calling terminal; moved_past said the successor does not reach a stale count for hours, softened to may not; a test doc gave relapses a RECONNECTED line they never print), and three links from public docs to private items (TURNED_AWAY, Session::unserved, spent_turn) made plain code spans so rustdoc gains no private_intra_doc_links warning; the rest of the file's prose unchanged since the round-4 read of 2026-09-12; 2026-09-12 (0.84 train): the supervisor's screen read was renamed from `read` to `screen` and given a doc comment saying why (the lock-order census identifies a lock by its name) — a method doc, not user-facing help; no other sentence moved; 2026-09-13 read of the prose feat/round-7-offscreen moved since the recorded read (shown by `xtask gate help-surfaces --diff`, matched at 68d1fb931) — SuperviseOpts::report's doc, Review::review's report argument, watch's --report sentence, brief_report's doc, ReportBrief, reported, reported_event_line and its `EVENT {} seq={} complete={} rows={} {}` line, the `report failed: {}` note, the mock's history/offscreen arms and the four new test docs — against look's `opts.report && reported(&point.phase)` gate (Idle, Question, Limited only; a prompt's EVENT and an APPROVED line unchanged), supervise's report:false override, brief_report's arms (Fail::Lost propagated to drive's ride_out; Fail::Hard noted and reported `complete=0 rows=0`, the loop kept), report.rs gather_report (`history 8`, then one `offscreen since=<mark>|tail=<n> max=<n> screen=1`, `text --json` on a host without the verb) and the request sequences the tests assert; no claim contradicted; 2026-09-13 read of the prose feat/round-8-survey moved (shown by `xtask gate help-surfaces --diff`, matched at 88afd4e09) — SuperviseOpts::dismiss_surveys, SURVEY_ROW, Looking.survey, Survey (Closed, Said, Tried), Pressing::Withheld, Dismiss (Pressed, Skipped, Unguarded), Session.survey_gone, and the look, survey, hand_survey, dismiss_survey, auto_read, press_one_guarded, stray_digit, typed_draft, survey_event_line and render_phase_and_survey docs, the four new notes (dismissed, handed to the manager, a backspaced 0, the fallback press withheld), the `DISMISSED survey seq={seq}` and `EVENT survey seq={seq} dismiss with: aterm ctl{target} key 'if={SURVEY_ROW}' 0` lines, `survey 0`, the mock's row_matcher-judged `key if=` and the survey tests' docs — against survey()'s order (the stray-0 check under Tried with a press, a Tried survey judged only on a look with no box and no draft unless it has gone, Said reset by a read that saw it gone, the press only from Closed with the survey open), screen()'s survey_gone latch on every read, dismiss_survey's caps.key_if probe (unknown_form to Unguarded, `ERR busy sink` retried, a request not served ridden out, any other ERR hard), the fallback's survey_open check on the confirming read before `key 1`, typed_draft over composer_draft and Screen::cursor_index, phase.rs survey_open/is_survey_question/is_survey_options/is_parked_above_composer, the server's input_if_row_matches (the guard tested against every visible row and the key written under one terminal lock, crates/aterm-gui/src/control_input.rs) and aterm_observe::row_matcher; zsh -f with extended_glob measured to refuse the unquoted `if=^●…` word as `no matches found`; three slips fixed before this row (SURVEY_ROW's and dismiss_survey's docs said no copy of the survey on the screen matches, where a worker message that opens with the question on a platform that draws the message glyph as `●` does — now said, the copies named as quoted ones; Dismiss::Skipped gave only the survey leaving first, where an open survey whose row the guard missed is skipped too; survey_event_line said a turn opening with any digit is a rating, where `0` dismisses); 2026-09-13 read (2026-09-14 UTC) of the prose feat/round-9-context moved (shown by `xtask gate help-surfaces --diff`, matched at 20cd46a07) — SuperviseOpts::context_warn, COMPACTED_RISE, Context (Armed, Warned), Session's context_warn and context fields, the await_turn_from, watch_context, supervise, watch, look and render_phase_and_survey docs, the `EVENT context seq={} {left}% until auto-compact`, `EVENT compacted seq={}` and `context {left}%` lines, and the docs of the ten context tests and their three helpers (with_context, descent, narrowed) — against watch_context (0 returns before a read is judged; Armed warns at left <= warn; Warned keeps the latest reading, and a None on a screen with has_composer_frame and no parse_prompt box, or a reading >= last + 30, says compacted and re-arms), await_turn_from's watch call only on a read before the deadline, await_turn's say that drops every line, drive arming context_warn and Context::Armed at each loop's start, StopAtReview::say (stderr) and Lines::say through emit (stdout, flushed), render_phase_and_survey's order (render_phase, survey 0, context last), phase.rs context_left/context_reading/is_against_right_edge/status_block (the zone from under the status row, or the last transcript row with none, to the top rule; a row ending within three columns of the bottom rule's width and starting at column 6 or later; the whole trimmed row the indicator, 0 to 100), drive_cli.rs parse_sub's --context-warn (watch and supervise only, 0 to 100) and DEFAULT_CONTEXT_WARN = 10, SuperviseOpts' derived Default (0) and parse_prompt (any row with `Esc to cancel`); four slips fixed before this row (await_turn_from's doc said every read is shown to the watch, where a read at or after the deadline is not; watch_context's said a read with a box up shows nothing either way, where only the indicator's absence is ignored under a box and a reading on it still counts; SuperviseOpts::context_warn's left the box out of what makes the indicator gone; a test doc said await-turn watches no indicator, where it runs the watch with a say that drops every line) and COMPACTED_RISE's unmeasured wobble claim replaced by the unmeasured `/model` case; 2026-09-14 read of the prose feat/round-11-fabric-ledger added (SuperviseOpts::journal, the StopAtReview and Lines docs, approved_line, Lines::line, watch/watch_to/supervise_to, ReportBrief's doc and its new turn field, and the four journal test docs) against the code that implements it: Lines::line records the line in the journal BEFORE emit prints it and passes a point's turn only when --report read one, StopAtReview::approved/review journal the APPROVED and EVENT lines `watch` would have printed while printing nothing themselves and its say journals then logs, watch_to opens the journal from opts.journal with the sid the loop was given and records its last TIMEOUT/EXIT line too, supervise_to journals TIMEOUT on End::Timeout and `EXIT <exit_reason>` on the error it returns, journal.rs Journal::open (create + append, mode 0o600 on unix, one warning on failure) and Journal::record (a write that fails warns once and the next line is still tried), and brief_report's ReportBrief.turn = Report::turn (Some only at marker=ledger, None when the report failed); no claim contradicted; 2026-09-14 read again on the merge branch of the journal prose (SuperviseOpts::journal, the Lines and StopAtReview docs, approved_line, watch_to and supervise_to) against Lines::line, which records into the journal BEFORE emit prints, StopAtReview::approved/review, which journal the APPROVED and EVENT lines `watch` would have printed while printing none themselves, supervise_to's TIMEOUT record on End::Timeout and its `EXIT <reason>` record on the error it returns, and drive_cli.rs main_entry, whose pre-loop failure path opens the journal itself from journal_arg/watch_sid_arg read off the argv as typed - so the claim that EVERY line the loop prints is journaled holds on the path where the loop never started; no claim contradicted; 2026-09-14 (feat/round-13-fabric-on, round 13 C): no sentence of the prose moved — the hasher saw the string literal `fixtures/wait_bg7.out` leave a test body, the saved capture now being `aterm_phase::prompt::fixtures::WAIT_BG7` since the phase reader and its fixtures moved to the aterm-phase crate (`supervise::phase` and `supervise::prompt` are re-exports of it); the outage, report, survey, context and journal prose re-read unchanged against the same code; 2026-09-14 (feat/round-14-mail, round 14 D1/D2/D4) read of the --mail and task prose — DRIVE_HELP's synopsis lines, the watch, task and --mail entries and the journal's mail kind and report field (lib.rs); DRIVE_PAGE's watch, task, --mail and --journal entries (manual.rs); run.rs's module doc, SuperviseOpts::mail, Fold, MailIn, NoLane, Sink, the Review, StopAtReview, Lines and folded_event_line docs, supervise_mail, supervise_with, run_loop, watch_mail, watch_with, look's fold gate, hold_for_report, event_line_as, render_result_mail and the six mail test docs; the two skills' loop, watch, task and ledger paragraphs — against mail.rs Lane::serve (an inbox 1 --peek --meta baseline; await inbox since=<newest> kinds=<all nine> timeout <step>, re-armed on OK timeout, the kinds list dropped on ERR usage; inbox since=<id> --peek --meta, then inbox get <id> for the watched worker's report only; the MAIL line per row through the loop's sink; lane_off said once on any other ERR or a lapsed reconnect window), Lane::call's retry of a lost reply (pause doubling to pause_max, within reconnect), task() (post to=@sid kind=task [dl=<ms>] <text> from --inbox or @self, the offset from the OK's off=, one text --json read and `turn idle=600 timeout=2500 Inbox: task @<off>` only on Phase::Idle with submitted=1 as nudged, the inbox 1 baseline read BEFORE the post, await inbox … kinds=answer,report,ack re-armed on rows without re=<off>, the TIMEOUT line and EXIT_TIMEOUT), run.rs hold_for_report (pending drained; a delivery within the window folds at once, else recv_timeout to min(now + grace, deadline); Disconnected gives None so the line is as without the flag), look's Phase::Idle gate with the brief skipped only on Fold::Report, run_loop's thread::scope join (the lane returns within one mail_step) and drive_cli.rs parse_sub (--mail, --report-window and --idle-grace watch's and supervise's only; --inbox needs a leading @; --deadline, --wait and --no-nudge task's only), mail_needs_sid and task_opts (--wait bounded by --deadline, else --timeout); measured on the live worker s-1e918c4662a1b7b8bd43 (busy for the whole 25 s budget: TIMEOUT, the process ending 40 s in as the lane's parked wait was joined within its 20 s step, no MAIL lane off) and by the mock tests; no claim contradicted; 2026-09-15 read of run.rs's round-14-review prose — Ctl::interrupter and Interrupter, MailIn's floor/busy_at/handed and is_this_turns, Held, set_mail_step/set_hold_step, run_loop's cut, hold_for_report's hold-step safety net and SuperviseOpts::mail — against the code beside each (hold_for_report's slice loop and review_key compare, look's carried turn and floor reset, await_turn_from's busy_at stamp, CtlClient::cutter), by feat/round-14-mail on 2026-09-15 after the wake-economy review; no claim contradicted; 2026-09-14 (2026-09-15 UTC) read of run.rs's prose the gate's --diff shows moved since main's read: the module doc's --mail paragraph, SuperviseOpts::mail, End::Stopped, Fold, MailIn and is_this_turns, Held, NoLane, Sink, Looking::carried, Review::review, StopAtReview, Lines, folded_event_line, Session's mail/mail_step/hold_step fields, set_mail_step, set_hold_step, supervise_mail, supervise_with, run_loop, watch_mail, watch_with, look's carried-screen and floor comments, hold_for_report, event_line_as, render_result_mail, render_result_rows, the mock's release/release_at and MailMock docs and the eleven mail tests' docs — each against the code beside it (the fold gate on Phase::Idle only; --report's brief skipped only on Fold::Report; floor/busy_at/handed reset at a reported() point; busy_at stamped in await_turn_from's busy branch; the slice loop's `slice == left` skip of the last read; carried filtered to a non-busy screen with a composer frame; the interrupter called after the stop flag is stored) and against mail.rs Lane::run/serve and Delivery; no claim contradicted; 2026-09-17 (feat/round-17-limits) read of the limit-episode prose this change added — the module doc's round-17 paragraph, RESET_GRACE/ANSWER_WINDOW/PROBE_BACKOFF/PROBE/REBRIEF/ATTENTION_BYTES, Review::note/unattended and both impls, Resume, Episode, Probed, Session's manager/resume/limit/rebrief_due/clock/local_offset/zone_offset/probe_backoff/answer_window fields and the set_manager/set_resume/set_clock/set_zone_lookup/set_probe_timing docs, drive's mutable deadline, look's episode paragraph and its close-on-worked block, and the whole limit impl (now_unix, local_offset_s, reset_unix, instant_of, rules, probe_due, arm_probe_at_reset, arm_probe_in, mail_turn_boundary, limit_before, limit_after, escalate, close_episode, extend, probe_ready, wait_for_next, probe_now, probe, settle_probe, rebrief, fit_bytes, reply_word) and the eleven new test docs — against the code under each: look() closes the episode only when a read saw the worker busy or a box is up and never on a limited turn; limit_before probes only with --resume, an open episode and a non-limited, non-prompt point, deferring on a typed draft and leaving a question to the manager; limit_after escalates once (Episode absent), refreshes only a LATER reset, arms the probe for the reset (past = now) and extends once a reset; escalate's `meta set attention` cut to 256 bytes and its `post to=<manager> kind=control` through the WORKER's client, both replies journaled and never fatal; close_episode's `meta unset attention` (the wire refuses `meta set attention ''` with the usage line: crates/aterm-gui/src/control_session.rs cmd_meta's MetaWriteError::Empty arm, measured); extend's until = reset + 10 min only when past the deadline; wait_for_next's bound at min(deadline, probe_at) and the probe on the point still showing; probe's `turn idle=600 timeout=2500` with submitted=1 as the proof, the answer a busy read or a status row not the probe point's, the window 120 s; settle_probe's backoff index (10 min, then 30) never before a later reset; rebrief's re-read of the file, one_line, and the rebrief-failed lines; supervise/limit.rs reset_at (this year, next when > 30 days gone; today, or tomorrow when > 12 h passed; the zone via TZ=<zone> date +%z only for a zoneinfo file that exists, measured: an unknown TZ reads +0000) and the Mock's turn verdict, turn_releases, stall_sleep and dead_after_turns; no claim contradicted; pinned by run.rs's a_limit_escalates_once_and_extends_the_budget_once, a_refused_escalation_is_journaled_and_the_loop_goes_on, a_passed_reset_extends_nothing_and_the_lines_are_as_before, at_the_reset_one_probe_goes_and_the_answer_is_resumed_and_rebriefed, the_screen_leaving_the_notice_probes_at_once, the_notice_again_after_a_probe_is_still_limited_not_a_new_episode, no_answer_backs_off_ten_then_thirty, a_draft_in_the_composer_defers_the_probe, a_worked_point_closes_the_episode_and_restates_the_rules and limit.rs's five tests; 2026-09-17 (round-17 adversarial fixes): re-read the limit prose that changed — the ANSWER_WINDOW and RESET_HORIZON docs, Episode.reset_text, arm_probe_at_reset, refresh_reset, limit_after, extend, probe_ready and probe, and the five tests added — against the code: probe's window restarting on a turn timed out busy while `Instant::now() < deadline` (at the budget's end `still-limited … the budget ran out while it was answering`), refresh_reset's same-text-is-the-same-reset early return and later-only refresh, arm_probe_at_reset's max with the held time only when `failed > 0`, extend's `reset - now_unix > RESET_HORIZON` return after `extended = true`, probe_ready's has_composer_frame check ahead of typed_draft; 2026-09-17 read of the limit episode's prose at the rebase onto main (ca8ab9aef) — the module doc's limited paragraph and the docs of Resume, Episode, refresh_reset, limit_before, limit_after, escalate, close_episode, extend, probe_ready, wait_for_next, probe_now, probe, settle_probe, rebrief and arm_probe_at_reset — against their bodies: escalate's `meta set attention` cut by fit_bytes at ATTENTION_BYTES = 256 and `post to=<manager> kind=control` from the worker's session (set_manager: --inbox, else $ATERM_PARENT_SESSION_ID, else `skipped:` in the ESCALATED line); look's close on `worked || Phase::Prompt` under review.unattended() with `meta unset attention` (aterm-gui control_session.rs's MetaWriteError::Empty arm is why not `set ''`); extend's RESET_GRACE 10 min, `extended` once a reset, RESET_HORIZON 8 days; probe_ready's has_composer_frame-then-typed_draft order and the own-`❯`-row question; probe's `turn idle=600 timeout=2500` + PROBE, `submitted=1`, ANSWER_WINDOW 120 s restarted on a busy timeout within the budget; settle_probe's PROBE_BACKOFF [10 min, 30 min] held past a future reset; arm_probe_at_reset's max(reset, held) once failed > 0; rebrief's re-read and one_line under REBRIEF; no claim contradicted, prose unchanged since the row's hash",
    ),
    (
        "crates/aterm-forge/src/budget.rs",
        "5f1d4201da04506e",
        "2026-09-16",
        "re-read on 2026-09-12 against the code that moved under it since the roster was minted, by the 2026-09-12 drift sweep (lane aterm-forge); the package_dir mechanism sentence was false for a direct-path dep and is fixed here; 2026-09-14 drift sweep (lane forge, 46 claims checked over the group): re-read against the code that moved under it since this row's anchor commit; every candidate went to a verifier prompted to REFUTE it — no claim contradicted the moved code; 2026-09-16 dead-code sweep: `seed_from_live` and its five-line doc comment were deleted as unreferenced — nothing in the workspace called it, and the row's whole prose delta is that doc plus the two-piece `refusing to seed ... from an incomplete measurement` error string it owned. The surviving prose was re-read against its handlers and is unchanged: the seed-body doc still names `seed_shape` (which `unarmed` still calls), `run` still measures before it learns the file is empty, and `unarmed` still renders from that one measurement. No claim contradicted",
    ),
    (
        "crates/aterm-forge/src/attest.rs",
        "e3440d18f94bece1",
        "2026-09-14",
        "re-read on 2026-09-12 against the code that moved under it since the roster was minted, by the 2026-09-12 drift sweep (lane aterm-forge); the [OB-1] exemption wrongly covered [OB-10] and is fixed here; 2026-09-14 drift sweep (lane forge, 46 claims checked over the group): re-read against the code that moved under it since this row's anchor commit; every candidate went to a verifier prompted to REFUTE it — ONE slip fixed before this row: `[OB-1]`'s doc said every vendor directory is claimed by a patch or by the direct-bundle record, where there is a THIRD shape it also accepts, a FIRST_PARTY_VENDORED row a member really reaches by `path = …`",
    ),
    (
        "crates/aterm-agent/src/fleet_cli.rs",
        "e20833cc05228f40",
        "2026-09-10",
        "read against its parser/dispatch by the 2026-09-10 sweep; findings fixed in 412cf3acd",
    ),
    (
        "crates/aterm-agent/src/lib.rs",
        "917a13fd79a537cf",
        "2026-09-17",
        "read crates/aterm-agent/src/lib.rs (DRIVE_HELP's await-turn timeout sentence, supervise's TIMEOUT sentence, watch's dedup and last-line paragraphs, and the rewritten --reconnect-s entry) against supervise/run.rs Session::call/unserved/ride_out (the outage-wide window from the first unserved request, one RECONNECT and one RECONNECTED per outage, 0.5 s doubling to 8 s across it, Rode::Spent → TIMEOUT, no such session not yet an answer only inside an outage, ERR exited always final), drive's fresh-only handed reset, moved_past's check read, the fallback press's Pressing::Lost handling and look's stray check, spent_turn/render_phase's `no screen` reason, drive_cli.rs main_entry (an Err exits 1) and await-turn's timed_out → 124, CtlReply::lost's TURNED_AWAY lines, and aterm-ctl's resolve_path (--sock, then $ATERM_CONTROL_SOCK, then self_instance_sock, then aterm.sock) and the `instances` verb's per-instance socket column (crates/aterm-ctl/src/lib.rs), by feat/handoff-survival on 2026-09-12 after the adversarial review of f6845c667; one slip fixed before this row (watch's dedup said the point is reprinted when the connection was lost, where the code resets on an outage's first answer, turned-away requests included — reworded to an outage came); the rest of DRIVE_HELP unchanged since the round-4 read of 2026-09-12; 2026-09-13 read of DRIVE_HELP's round-7 additions (shown by `xtask gate help-surfaces --diff`, matched at 718dda231) — the watch/report synopsis lines, watch's --report paragraph, the report entry and the two examples — against drive_cli.rs parse_sub (--since through Mark::parse, --max-rows > 0, --report) and its report arm (an Err exits 1), supervise/report.rs (newest_turn over `history 8`, find_start/is_user_row/user_text/paste_like/paste_fits with MARKER_CHARS 60 and PASTE_CHARS 200, offscreen_snapshot's since=/tail= max= screen=1 read, parse_offscreen's stderr header and its NoHeader fallback to `text --json`, join's back_at/pin skip of re-shown rows the read got, assess, gap_after's one-row recheck, finish's blank trim, Report::header/render), phase.rs transcript_end/is_done_row/is_parked_above_composer, run.rs reported/reported_event_line, and Turn::run (prompt types with send + key enter, so it leaves no ledger turn); two slips fixed before this row (main-screen said `the screen alone`, where the one offscreen read still joins the rows archived since the mark when the worker left the alternate screen — it now says what the reason means; complete=1's conditions left out the host keeping an archive, which no-archive refuses) and the reasons paragraph reflowed (one line had run to 109 columns); 2026-09-13 read of DRIVE_HELP's round-8 additions (shown by `xtask gate help-surfaces --diff`, matched at 547991d5b) — the supervise and watch synopsis `[--dismiss-surveys]`, phase's `survey 0` paragraph, await-turn's `exactly like phase`, supervise's withheld-fallback clause and survey sentence, watch's `EVENT survey` paragraph and the --dismiss-surveys entry — against drive_cli.rs parse_sub (--dismiss-surveys refused off watch and supervise) and phase_reply (phase and await-turn both render_phase_and_survey, 124 on a timed-out turn), phase.rs survey_open (the `●` question row in column 0 right over the options row, then only blank rows, right-aligned hints and `⎿  Tip:` rows down to the top rule, never without the composer frame), run.rs survey/hand_survey/dismiss_survey/typed_draft/survey_event_line and press_one_guarded's Withheld arm, StopAtReview::say (stderr) and Lines::say (stdout), and the server's input_if_row_matches (the check and the key under one terminal lock, `OK skipped` when no visible row matches; crates/aterm-gui/src/control_input.rs); three slips fixed before this row (the guard was said to let no copy of the survey on the screen through, where only a quoted copy — indented, or under `⎿` — is sure not to match; supervise's survey sentence said --dismiss-surveys dismisses it and says DISMISSED, where the line comes only once a fresh read shows it gone; a `0` found in the composer was put down to the survey leaving first alone, where the code backspaces one an open survey did not take as well); 2026-09-13 read (2026-09-14 UTC) of DRIVE_HELP's round-9 additions (shown by `xtask gate help-surfaces --diff`, matched at 20cd46a07) — the supervise and watch synopsis `[--context-warn PCT]`, phase's `context <n>%` paragraph, await-turn's `survey 0` and `context <n>%` clause (the rest of that paragraph reflowed, its words unchanged), supervise's stderr sentence, watch's `EVENT context`/`EVENT compacted` paragraph and the --context-warn entry — against drive_cli.rs parse_sub (--context-warn refused off watch and supervise, a value 0 to 100) and DEFAULT_CONTEXT_WARN, phase_reply (phase and await-turn both render_phase_and_survey, context after survey 0), phase.rs context_left (status_block's from to the top rule, is_against_right_edge, context_reading's two spellings), run.rs watch_context (the inclusive threshold, the frame-and-no-box rule for gone, the 30-point rise, the re-arm), await_turn_from's per-read call, drive's arming at each run's start, StopAtReview::say (stderr), Lines::say (stdout) and the tests supervise_watches_one_run and the_context_watch_is_inclusive_and_needs_the_frame; one slip fixed before this row (phase's paragraph put the indicator under the status row alone, where with no status row it is read under the last transcript row); 2026-09-14 read of DRIVE_HELP's round-10 changes (shown by `xtask gate help-surfaces --diff`, matched at cc86c5cfe) — the report entry's archive-reset and archive-gap clauses and its no-turn start — against aterm-gui handoff_carry.rs (export, tail_from, encode_within, decode and adopted_ledger: one sidecar carries the ledger and the archive tail, and a dropped or busy-locked ledger leaves the adopted one empty), aterm-core alt_archive.rs AltArchive::import (the carried origin kept; Refused when its indices are out of range or the archive is off) and carry_rows, spawn.rs's install right after restore_checkpoint, supervise/report.rs gather_report/ledger_turn/assess (archive-reset only when the mark's origin is not the read's, so a dropped carry opens at marker=user-row and only --since an older mark says archive-reset) and seamless_carry_tests' end-to-end resize variant (archive-gap, duplicates allowed, no row lost); one slip fixed before this row (the entry implied a failed carry reports archive-reset, where a ledger it could not carry leaves no turn and the start is the last `❯` row — the no-turn sentence now names that cause); 2026-09-14 read of DRIVE_HELP's round-11 additions (the supervise/watch synopsis `[--journal FILE]`, report's `[--final | --messages]` and its two view paragraphs, the whole `ledger` entry, and the `--journal FILE` entry) against drive_cli.rs parse_sub (--journal on watch/supervise/ledger only, --final/--messages on report only and never both, --format text|md|html and --out on ledger only, ledger's --since through supervise::parse_since where every other verb's is a Mark), ledger_verb (the worker from @sid else journal_sid's single sid, else an actionable error; the ledger dispatched before the host preflight; --out through write_private at 0o600 and the `wrote <path>` reply), clock_anchor (fleet_sessions for the pid, local_instances for the socket, its birth time as the process clock's zero) and tz_offset_s/parse_zone (`date +%z`, UTC when it cannot run), supervise/ledger.rs gather (whoami, `history`, one `offscreen tail=20000 max=20000 screen=1`, `@self inbox --peek --meta`, `@self timeline`, each miss a SOURCES row), reply_sizes (report's own join and the `❯` row found by the turn's first 60 characters or a fitting `[Pasted text …]` row, a turn whose row is nowhere named in the span that holds it), summary/items (the latency from each EVENT idle/question to the next turn's start, only with the clock placed; `~` on every unplaced time; a carried turn with no time), render_text/render_md/ledger_html::render_html (the four lanes, the inline style and script, the `default-src 'none'` policy), supervise/blocks.rs (the head rules NOTICE_HEADS/is_call_head/the next-row `⎿` rule/is_group_row, and view_rows: Final is the last Message block and the first Done after it, Messages every User and Message block without its `⎿` runs plus each Done row) and report.rs render_view (`view=`/`kept=` after `last=`, `rows=` unchanged, View::All byte-identical to render); measured for this row: the control socket's birth time is within 0.12 s of the fabric bus's own stamps on the same three messages on this machine, where `ps -o lstart=` was 5.3 s early; the --final clause was sharpened after the live read of 2026-09-14 showed blocks.rs is_group_row takes a group STILL RUNNING (`Running 3 shell commands · 9m 13s…`) for a tool row too; no claim contradicted; 2026-09-14 read again on the merge branch of every DRIVE_HELP block this round added: the `ledger` block against drive_cli.rs ledger_verb (the worker is `@sid` else journal_sid, and a journal naming two sids names none) and ledger.rs reply_rows, whose request is `offscreen tail=20000 max=20000 screen=1` from LEDGER_MAX_ROWS = 20_000 exactly as the help spells it, and against gather/render_ledger, which name a source they could not read and still return a Reply::text, so the exit stays 0; the `--journal` block against journal.rs JournalRecord::of_line (the six kinds the loops write - event, approved, dismissed, reconnect, timeout, exit - and the eight phase words, every field taken from the line) and Journal::open (create + append, 0600 only on creation, one warning); `report --final|--messages` against report.rs render_view (` view=<name> kept=<n>` appended after the header's own `last=`, `rows=` still the whole report, View::All returning render() unchanged) and blocks.rs view_rows/head_kind/is_tool_head; --format, --out, --since and --journal ownership against parse_sub's per-verb refusals; no claim contradicted, and one slip fixed OUTSIDE the roster in blocks.rs's module doc, which named five of NOTICE_HEADS' eight notices and gave `searched memories` as a whole collapsed-group row where is_group_row requires the first clause capitalised; 2026-09-14 (feat/round-14-mail, round 14 D1/D2/D4) read of the --mail and task prose — DRIVE_HELP's synopsis lines, the watch, task and --mail entries and the journal's mail kind and report field (lib.rs); DRIVE_PAGE's watch, task, --mail and --journal entries (manual.rs); run.rs's module doc, SuperviseOpts::mail, Fold, MailIn, NoLane, Sink, the Review, StopAtReview, Lines and folded_event_line docs, supervise_mail, supervise_with, run_loop, watch_mail, watch_with, look's fold gate, hold_for_report, event_line_as, render_result_mail and the six mail test docs; the two skills' loop, watch, task and ledger paragraphs — against mail.rs Lane::serve (an inbox 1 --peek --meta baseline; await inbox since=<newest> kinds=<all nine> timeout <step>, re-armed on OK timeout, the kinds list dropped on ERR usage; inbox since=<id> --peek --meta, then inbox get <id> for the watched worker's report only; the MAIL line per row through the loop's sink; lane_off said once on any other ERR or a lapsed reconnect window), Lane::call's retry of a lost reply (pause doubling to pause_max, within reconnect), task() (post to=@sid kind=task [dl=<ms>] <text> from --inbox or @self, the offset from the OK's off=, one text --json read and `turn idle=600 timeout=2500 Inbox: task @<off>` only on Phase::Idle with submitted=1 as nudged, the inbox 1 baseline read BEFORE the post, await inbox … kinds=answer,report,ack re-armed on rows without re=<off>, the TIMEOUT line and EXIT_TIMEOUT), run.rs hold_for_report (pending drained; a delivery within the window folds at once, else recv_timeout to min(now + grace, deadline); Disconnected gives None so the line is as without the flag), look's Phase::Idle gate with the brief skipped only on Fold::Report, run_loop's thread::scope join (the lane returns within one mail_step) and drive_cli.rs parse_sub (--mail, --report-window and --idle-grace watch's and supervise's only; --inbox needs a leading @; --deadline, --wait and --no-nudge task's only), mail_needs_sid and task_opts (--wait bounded by --deadline, else --timeout); measured on the live worker s-1e918c4662a1b7b8bd43 (busy for the whole 25 s budget: TIMEOUT, the process ending 40 s in as the lane's parked wait was joined within its 20 s step, no MAIL lane off) and by the mock tests; no claim contradicted; 2026-09-15 read of DRIVE_HELP's --mail and task entries as the round-14 reviews (wake-economy, report-hook-safety) changed them — the one screen read per 20 s hold step, a superseded point said as idle-no-report with what followed (a footer tick held on), a report attributed to its turn (after the worker was read busy for it, or after the point; never one from before the last point handed over or between it and the next busy read; --report-window only for one from before the turn was seen to begin), the lane's parked wait cut short (aterm-ctl signalled) so supervise --mail returns at once and watch --mail ends with its last line, the resync from the bus offset after a self-update, and task --wait re-armed past the host's 600 s clamp — against supervise/run.rs hold_for_report (hold_step slices, review_key compare, Held::carried), MailIn::is_this_turns (floor, busy_at, handed, window), await_turn_from's busy_at stamp, look's floor reset on reported() points, run_loop's Ctl::interrupter call, lib.rs CtlClient::cutter (SIGTERM to the pid under the lock, the cut flag checked after spawn), supervise/mail.rs Lane::resync (inbox --peek --meta whole, rows above the last off, since= the successor's newest id) and task's re-arm (OK timeout → continue while ≥ 1 ms is left), by feat/round-14-mail on 2026-09-15; no claim contradicted; pinned by run.rs's a_question_turns_report_is_not_the_next_turns, a_second_report_of_a_turn_is_not_the_next_turns, a_mid_turn_report_older_than_the_window_is_still_its_turns, a_prompt_after_an_idle_is_seen_within_a_hold_step, supervise_mails_result_does_not_wait_for_the_lanes_parked_step and mail.rs's a_wait_re_arms_on_the_hosts_own_timeout_while_time_is_left, the_lane_hears_the_successors_rows_after_a_handoff; 2026-09-14 (2026-09-15 UTC) read, before the merge, of every prose line `xtask gate help-surfaces --diff` shows moved since main's recorded read (matched at 6318eb4f6): DRIVE_HELP's synopsis lines, the watch entry's --mail sentence, the whole task entry and the whole --mail entry — against supervise/mail.rs (Lane::serve: the `inbox 1 --peek --meta` baseline, `await inbox since=<id> kinds=<nine> timeout <step>` re-armed on OK timeout, the kinds list dropped on a usage error, the listing since=<id> and `inbox get` for the watched worker's report only; Lane::resync after a lost request, from the bus offset; task(): the baseline read before the post, `post to=@sid kind=task [dl=<ms>] <text>`, `task not posted: <ERR>` and `task not landed` for a reply without off=, one `text --json` read and `turn idle=600 timeout=2500 Inbox: task @<off>` on Phase::Idle only, submitted=1 as nudged, `await inbox … kinds=answer,report,ack` re-armed on the host's timeout while at least 1 ms is left, the TIMEOUT line and EXIT_TIMEOUT), supervise/run.rs (MailIn::is_this_turns: floor, busy_at, handed, window; hold_for_report: pending drained, the newest of this turn's folds, else recv_timeout in hold_step slices with one screen read per slice, an equal review_key holds on, else NoReport with the screen carried; look: the carried screen is the next turn unless busy; run_loop: the stop flag then the interrupter, thread::scope join; render_result_mail; folded_event_line), lib.rs CtlClient::cutter (SIGTERM to the registered pid, the cut flag checked after the spawn), drive_cli.rs parse_sub/mail_needs_sid/task_opts and aterm-gui fabric.rs cmd_post (WAIT_DEFAULT_MS = 30 s, `OK <id> off=<n>` on a landing, `ERR timeout id=<n>` past the wait); ONE claim contradicted and fixed in this commit: the task entry said a worker with round 12's wake hooks reads a task on its next Stop, where aterm-link hook.rs may_wake/accepted wakes only for a human or a sender whose OWNER (`s-…`) the hook's --accept-from lists — measured on the owner's live worker, whose hooks list the node id and whose inbox holds three of the manager's tasks, kind=task, none woken for — so the entry now says the manager's sid must be in --accept-from; no other claim contradicted; 2026-09-17 (feat/round-17-limits) read of DRIVE_HELP's round-17 prose — the watch synopsis lines' `[--resume [RULES]]`, the watch entry's usage-limit paragraph, the `--resume [RULES]` entry and the --journal entry's kind and phase lists — against supervise/run.rs look/limit_before/limit_after/escalate/close_episode/extend/probe/settle_probe/rebrief (the attention text and its 256-byte cut, the control post from the worker's session to --inbox else $ATERM_PARENT_SESSION_ID else skipped, one ESCALATED line an episode, EXTEND once a reset at reset + 10 min only when the deadline falls before it, `meta unset attention` on close, the probe's exact text and `turn idle=600 timeout=2500`, idle-and-empty-composer only with a draft deferred a step and a question left to the manager, the 120 s answer window, EVENT resumed/rebriefed/rebrief-failed/still-limited and the 10-then-30 min backoff never before a later reset), drive_cli.rs parse_sub's --resume arm (watch only; the next word unless a flag or an @sid), resume_opts (read now, never empty) and manager_sid, supervise/limit.rs parse_reset/reset_at/zone_offset_s, and journal.rs of_line's extend/escalated/cleared/probe kinds; no claim contradicted; 2026-09-17 (round-17 adversarial fixes): re-read DRIVE_HELP's watch limit paragraph (the span-from-the-print and 8-day clauses, the no-composer deferral, the busy-past-120-s wait, `no reaction within 120 s`, the same-text clause) and the --journal phase list (`rebrief-failed` added) against supervise/run.rs refresh_reset, RESET_HORIZON and extend, probe_ready, probe and settle_probe, and supervise/journal.rs's phase roster; 2026-09-17 read of DRIVE_HELP's usage-limit paragraph under watch, the `--resume [RULES]` entry, the two synopsis lines and the journal record's kind/phase lists at the rebase onto main (ca8ab9aef) against supervise/run.rs (escalate, close_episode, extend, probe_ready, probe, settle_probe, rebrief, arm_probe_at_reset, refresh_reset; ATTENTION_BYTES 256, RESET_GRACE 10 min, RESET_HORIZON 8 d, ANSWER_WINDOW 120 s, PROBE_BACKOFF 10/30 min, PROBE_IDLE/PROBE_TIMEOUT), drive_cli.rs (`--resume` refused off watch, the next word the file unless it starts with `-` or `@`, resume_opts's unreadable/empty refusal, manager_sid's --inbox else $ATERM_PARENT_SESSION_ID), journal.rs's kind list (mail|extend|escalated|cleared|probe) and phase list (…|resumed|still-limited|rebriefed|rebrief-failed), limit.rs (SAME_DAY 19 h, the nearest-year date, zone_offset_s via `date`), and the attention-badge claim against aterm-types control_verbs.rs's own `meta` help (the menu-bar status item badges `attention`); no claim contradicted, prose unchanged since the row's hash",
    ),
    (
        "crates/aterm-cli/src/lib.rs",
        "aef435bade0a9a82",
        "2026-09-18",
        "re-read on 2026-09-18 for the v0.88.0 assembly: the only usage change since the 2026-09-12 sweep is the kitty-commands merge (2a06b13c2) — `list-kitty-commands` in the USAGE list and its module doc — read against the dispatcher (`\"list-kitty-commands\" => list_kitty_commands_report()`), which prints aterm-lexicon's trick rows with no banner exactly as the doc says, and against `diag_report`, which proves the subcommand dispatchable; the rest of the surface carries the reads recorded before this row",
    ),
    (
        "crates/aterm-cli/src/manual.rs",
        "9025ac132b0ba0e5",
        "2026-09-18",
        "re-read on 2026-09-18 for the v0.88.0 assembly: the kitty-commands merge (2a06b13c2) added the `aterm list-kitty-commands` index line and the `help kitty` page (KITTY_PAGE_HEAD plus the generated vocabulary); read against the topic router (`pet | cat | tricks | kitty-commands | list-kitty-commands => kitty`, so every alias the page names resolves), `kitty_page` (the vocabulary printed IS the compiled TrickLexicon, as the page claims) and aterm-gui's `[sparkle_words] tricks` config (SparkleTricksConfig carries the `enabled` key the page documents); the page's behavioural sentences describe aterm_effects::typed_tricks and PetBrain::note_trick and are the feature author's, not re-derived in this read; the earlier reads this row carried still hold",
    ),
    (
        "crates/aterm-cli/src/windowing.rs",
        "fd8be119f5c354f8",
        "2026-09-14",
        "read against its parser/dispatch by the 2026-09-10 sweep; findings fixed in 412cf3acd; 2026-09-14 drift sweep (lane cli-and-types, 25 claims checked over the group): re-read against the code that moved under it since this row's anchor commit; every candidate went to a verifier prompted to REFUTE it — no claim contradicted the moved code",
    ),
    (
        "crates/aterm-control/src/selection.rs",
        "7269abf2d2722432",
        "2026-09-10",
        "read against its parser/dispatch by the 2026-09-10 sweep (aterm-B-window-and-clients); findings fixed in the commit that added this row",
    ),
    (
        "crates/aterm-ctl/src/conn.rs",
        "8ce0726820ccb121",
        "2026-09-18",
        "read against its parser/dispatch by the 2026-09-10 sweep (aterm-B-window-and-clients); findings fixed in the commit that added this row; 2026-09-14 drift sweep (lane ctl, 61 claims checked over the group): re-read against the code that moved under it since this row's anchor commit; every candidate went to a verifier prompted to REFUTE it — no claim contradicted the moved code re-read on 2026-09-18 after the dead-`latest`-alias fallback landed: the one prose item it adds here — the doc on `ConnWire`'s new `origin` field — was read against `conn_real_main`'s `--sock`/`--pid` parsing and its `resolve_target` call, `ConnWire::request`'s `connect_stream(&path, self.origin)`, and aterm-ctl's `resolve_target`/`connect_error`/`running_elsewhere_hint`: every flag and an explicit `$ATERM_CONTROL_SOCK` resolve to `TargetOrigin::Pinned`, whose dead socket is told only how many other instances are live and is handed no instance to drive. `CONN_USAGE` still carries both flags. No claim contradicted.",
    ),
    (
        "crates/aterm-ctl/src/lib.rs",
        "fab73a90ea712097",
        "2026-09-18",
        "re-read on 2026-09-12 against the code that moved under it since the roster was minted, by the 2026-09-12 drift sweep (lane small-1); the cell attribute roster omitted `hidden` and is fixed here; the 0.84 train's release-candidate merge (this machine's provenance/repair/TCC work over the peers' 2026-09-12 re-read at the tip) moved the bytes once more — both halves were read by their authors as recorded, the merged file is their union, and this row records the union's hash; 2026-09-13 read of the prose feat/round-7-offscreen moved (shown by `xtask gate help-surfaces --diff`, matched at 550ace022) — `offscreen` joining `inbox` as the verbs whose `OK <n> …` header goes to stderr with stdout kept as rows, and the two `offscreen` cases in streams_payload_gates_by_verb_and_image_read — against exchange's header branch, stderr_line's `aterm-ctl: ` prefix, streams_payload → control_verbs::framing_of (offscreen's catalog row is Lines) and aterm-agent report.rs parse_offscreen, which reads that stderr header (CtlClient shells out to this client); no claim contradicted; 2026-09-14 drift sweep (lane ctl, 61 claims checked over the group): re-read against the code that moved under it since this row's anchor commit; every candidate went to a verifier prompted to REFUTE it — no claim contradicted the moved code; 2026-09-14 round-12: two public entries added for aterm-link's hooks, resolve_sock_for and read_token_beside; their docs read against resolve_path (an explicit or disabled $ATERM_CONTROL_SOCK directive first, then self_instance_sock's graph entry with its liveness probe, then default_sock_path's latest alias) and read_token_at (the per-socket token, then for a non-instance socket name the legacy aterm.token; an InvalidData/InvalidInput miss stops the search); nothing else in the file moved; 2026-09-14 read of the two public entries added since (`local_instances` and `instance_token`, and their docs) against inspect_fleet/enumerate_instances (the readdir plus graph-entry walk `instances`/`ls`/`windows` share, sorted by pid, `0` for a graph entry naming none, no dial), discovery_report's early arms (Unresolvable/Missing/Unreadable carry the same text and EXIT_UNREACHABLE `ls` exits with; Empty and Found are answers, returned as Ok) and read_token_at (the per-socket token through the `latest` alias, the legacy shared file for an explicit socket only, a malformed per-socket file refused without reaching past it), by feat/round-11-fabric-ledger for crates/aterm-link/src/fabric.rs, their one caller; no claim contradicted; 2026-09-14 read again on the merge branch: local_instances' and instance_token's docs against inspect_fleet (it answers Unresolvable, Missing or Unreadable only for a directory it could not look in, and Empty/Found for one it could, which is exactly the Ok/Err split the doc claims) and enumerate_instances (the readdir pass and the graph-entry pass deduped by the CANONICAL socket path, `out.sort_unstable()` so the pairs come back pid-ordered, graph_entry_pid's `unwrap_or(0)` behind the `0` placeholder sentence) and read_token_at (per-socket file through `latest`, the legacy shared file for an explicit socket only, a malformed per-socket file refused without reaching past it); no claim contradicted; 2026-09-14 merge read of feat/round-12-hooks over main: the merged prose is exactly this branch's resolve_sock_for and read_token_beside docs (one hunk after self_instance_sock) plus main's local_instances and instance_token docs (one hunk after fleet_sessions), checked against resolve_path's argument order (SOCK_ENV, NO_SOCK_ENV, then self_sid), inspect_fleet's DirOutcome arms in local_instances (Found, Empty and Scoped are Ok; every other outcome goes through discovery_report into FleetListError) and read_token_at, which both read_token_beside and instance_token wrap with the same `{}: {e}` map — the one string each side's --diff shows as the other's addition; neither side's hunk touches the other's anchors, and no sentence is wrong; the two identical bodies are a duplication for the owner to fold, recorded here rather than changed at the merge; 2026-09-15 read of the prose this change adds (duplicate_sids' doc, the two run_discovery comments — every answered row kept for the ambiguity sweep, and one mapping deciding what `ls` shows, what the bridge federates and what the sweep reads — and the new `WARNING: session id … is served by N live instances …` line) against duplicate_sids itself (pids deduped per sid before the >1 count, so one instance listing a sid twice is not two claimants; listing order kept by the `order` vec; a row with no sid column skipped exactly as the printer skips it), against run_discovery's loop (rows are still written as each instance answers — the sweep is the only thing that waits for all of them — and instance_sessions is now called once per answered instance for all three verbs, the `ls` arm iterating it by reference), and against stderr_line's `aterm-ctl: ` prefix; the doc's claim that `graph/<sid>` records exactly one host per id read against aterm-gui proxy::write_graph_entry (one file per sid, last writer wins); no claim contradicted; 2026-09-17 read of the `ls` shape (identity=<name|-> last, before the self mark) against control_session::sessions_lines and roster_tail's by-key walk re-read on 2026-09-18 against the handler, in full, because the fallback commit moved most of this surface's prose: the module doc's and SOCKET RESOLUTION's dead-alias sentences and the docs of `resolve_path`, `TargetOrigin`, `resolve_target`, `flagless_target`, `newest_live_instance_in`, `newest`, `instance_socks_in`, `connect_error`, `connect_error_naming`, `live_instances_besides`, `running_elsewhere_hint`/`_count`/`_flags`, `reached_by_pid`, `connect_stream`, `exchange`, `feed_bin_exchange` and `front_door_instance` were read against the code they describe (the Pinned arms; the PerInstance arm's `flagless_target`; the `NotFound`/`ConnectionRefused`-only fallback over `instance_socks_in`, filtered by an accepted connect and ordered mtime-then-pid; `connect_error`'s dir+fleet look-up and `connect_error_naming`'s empty-fleet split; `reached_by_pid`'s filename-then-component-then-`same_socket_path` rule; and `run_discovery` dispatching before `resolve_target`, so `ls`/`instances`/`windows` never enter the fallback) and against the code outside it (aterm-gui's `cleanup_socket` with `symlink_targets_pid`, `sweep_stale_instances` via `stale_instance_files`/`instance_pid`, which can never match the fixed alias name, `publish_latest_link`'s forward-only publish, `write_graph_entry`'s per-entry hosting pid, `discovery_targets`' dead-`--pid` message, and aterm-cli's `new-tab` grammar, which takes no `--pid`/`--sock`). Every claim held. THREE SLIPS FIXED in the commit that records this row: `resolve_sock_for`'s order stopped at the `latest` alias though it now inherits the fallback, `resolve_path`'s doc named only the front door though the public `resolve_sock_for` also calls it, and `live_instances_besides` said `aterm ctl instances` \"would list\" a set that is wider than what that verb prints (it counts an accepted connect; the verb prints those that also answer `sessions` inside `PROBE_DEADLINE`, so a wedged instance is counted and not listed). LEFT FOR THE OWNER, recorded rather than changed: the aterm-link hook.rs mirrors of that stale order (a rostered surface whose own hash did not move), and the fallback's two probes carrying no deadline where discovery's connector has one, so a wedged peer can stall a flagless call at resolution time.",
    ),
    (
        "crates/aterm-dev/src/main.rs",
        "2eb70f54c29b309b",
        "2026-09-18",
        "read against its parser/dispatch by the 2026-09-10 sweep; findings fixed in 412cf3acd; AND 2026-09-18 re-read of the `ship` passthrough's help and doc text (the driver ladder `$CARGO` → `aterm pkg which targo` → the store's current targo → a prefix-bound PATH targo → cargo, the `--version` lane sniff, `targo --unverified ship …`) against ship_driver/which_line_store_path/cargo_lane_args/run_ship in the same file, by the rust-free host-lane round; no slip found",
    ),
    (
        "crates/aterm-forge/src/cli.rs",
        "082ffd0b7dab9c05",
        "2026-09-14",
        "re-read on 2026-09-12 against the code that moved under it since the roster was minted, by the 2026-09-12 drift sweep (lane aterm-forge); no claim contradicted the moved code; 2026-09-14 drift sweep (lane forge, 46 claims checked over the group): re-read against the code that moved under it since this row's anchor commit; every candidate went to a verifier prompted to REFUTE it — no claim contradicted the moved code",
    ),
    (
        "crates/aterm-forge/src/lib.rs",
        "ac6752148aef0c5f",
        "2026-09-14",
        "re-read on 2026-09-12 against the code that moved under it since the roster was minted, by the 2026-09-12 drift sweep (lane aterm-forge); the astream-classifier figures and the inclusion claim were false and are fixed here; 2026-09-14 drift sweep (lane forge, 46 claims checked over the group): re-read against the code that moved under it since this row's anchor commit; every candidate went to a verifier prompted to REFUTE it — no claim contradicted the moved code",
    ),
    (
        "crates/aterm-gui/src/agent_identity.rs",
        "a5a30d8972d98ce2",
        "2026-09-17",
        "read in full against its handlers on 2026-09-17 by the round-18 identities surfaces pass: the module doc, parse_name's refusals, the ensure/ensure_in/restorable/carry_claude_hooks docs, and the `identities` verb's reply table (IDENTITIES_USAGE, cmd_identities/identities_reply and the list/one/forget arms) against the code beneath each and control_verbs' `identities` row — every reply the doc names is produced by exactly one arm and pinned by the four identities_* tests; `identities <name>` answers `OK <1+agents>` because aterm-ctl streams exactly n lines; 2026-09-17 read of the review-fix prose (--diff matched at 74d637316): parse_name's `forget` refusal (after the fold, so FORGET and Forget too; forget2 and forgetful pass) against its body, ensure's create=false clause against ensure_in's identity_dir early return (symlink_metadata + is_dir, the verb's own predicate), the `existing` twin's doc against restore_terminal_leaf's bootstrap_wears_leaf_identity check and main_entry's first_leaf_identity read (lib.rs), restorable's stderr line, and the two REVIEW test docs against the shapes they pin; no claim contradicted",
    ),
    (
        "crates/aterm-gui/src/cli.rs",
        "dd50180b929f8b5c",
        "2026-09-17",
        "re-read on 2026-09-12 against the code that moved under it since the roster was minted, by the 2026-09-12 drift sweep (lane aterm-gui-2); no claim contradicted the moved code; 2026-09-13 leftovers lane (89e3292d1, shown by `xtask gate help-surfaces --diff`: 1 hunk, 2 template lines rewritten as 5): the STARTER_CONFIG comments on `allow_notifications` (OSC 9/99/777; macOS delivers through terminal-notifier if installed, else osascript, a subprocess under aterm's identity) read against notify.rs deliver (Command terminal-notifier .status(), Err → the osascript `display notification` fallback; both subprocesses of aterm) and `allow_osc52_query` (a program's OSC 52 READ, answered only when on; on macOS 26 that read raises the system's \"aterm would like to paste from …\" alert) read against spawn.rs's ClipboardOperation::Query arm (reached only through a minted ClipboardQueryCapability, answers control::pbpaste — an in-process NSPasteboard read, clipboard.rs pbpaste); no claim contradicted; 2026-09-13 release-candidate read of the ONE prose line 47d9a1560 moved (the rainbow-gap merge, arriving on origin/main): the sample config's `cursor_trail_intensity` comment now reads `1.0` where it read `0.7`, and the comment states a DEFAULT, so it was checked against the code that supplies one — app_config.rs `cursor_trail_intensity_or_default` is `self.cursor_trail_intensity.unwrap_or(1.0)`, clamped to 0.0..=1.0 with a non-finite value failing OFF to 0.0, so the new literal is the default an unset key really gets and the old one had rotted; the remaining `intensity: 0.7` in this crate is a `resolve_cursor_glow` test fixture, not a default; the stated range `0.0..=1.0` still matches the clamp; 2026-09-13 read of the ONE line e61ee4b2b moved (the September 13 integration with the comet and the vivid rail): the `cursor_trail_style` roster in the sample config gained `rainbow kitty flat (the A/B control: the flat body of 2026-09-13, before the comet body and its vivid rail; aliases \"rainbow flat\"/\"flat rainbow\"/\"nyan flat\")`, checked against prefs.rs — the canonical name is in the style roster, the three aliases map to it in the alias table, and it resolves in the style match — so every spelling the line offers is one a config can actually set; no other style row changed; 2026-09-14 the prose moved by a TEST added here, not by a help claim — `aterm help config` states how many keys `--write-config` ships and that number lives in aterm-cli, which cannot read this private const, so `starter_config_key_count_matches_the_manual` counts STARTER_CONFIG's distinct commented `key =` names (table-scoped included, the same thing a reader counts in the written file) and reds when they move, naming the manual line to move with it; the count was measured at 159 against the const and the manual's `158` was the stale copy; 2026-09-15: the commented `# [machine]` starter block read against app_config.rs MachineConfig (two keys, both defaults act; [key_sequences] stays last); 2026-09-14 v0.86 candidate: the starter config's own prose is unchanged — what moved here is its two TEST pins, the key count (159 -> 161, together with the manual's line) and the `aterm pkg machine apply` disclosure check, which now reads the command the way the comment's wrap actually carries it because reflowing the block to keep the command on one line adds a Settings category (measured: `category_layout_matches_grouping_table` goes red). No claim about what the file tells a user changed; AND UPSTREAM'S READ OF THE SAME PROSE, kept: re-read on 2026-09-12 against the code that moved under it since the roster was minted, by the 2026-09-12 drift sweep (lane aterm-gui-2); no claim contradicted the moved code; 2026-09-13 leftovers lane (89e3292d1, shown by `xtask gate help-surfaces --diff`: 1 hunk, 2 template lines rewritten as 5): the STARTER_CONFIG comments on `allow_notifications` (OSC 9/99/777; macOS delivers through terminal-notifier if installed, else osascript, a subprocess under aterm's identity) read against notify.rs deliver (Command terminal-notifier .status(), Err → the osascript `display notification` fallback; both subprocesses of aterm) and `allow_osc52_query` (a program's OSC 52 READ, answered only when on; on macOS 26 that read raises the system's \"aterm would like to paste from …\" alert) read against spawn.rs's ClipboardOperation::Query arm (reached only through a minted ClipboardQueryCapability, answers control::pbpaste — an in-process NSPasteboard read, clipboard.rs pbpaste); no claim contradicted; 2026-09-13 release-candidate read of the ONE prose line 47d9a1560 moved (the rainbow-gap merge, arriving on origin/main): the sample config's `cursor_trail_intensity` comment now reads `1.0` where it read `0.7`, and the comment states a DEFAULT, so it was checked against the code that supplies one — app_config.rs `cursor_trail_intensity_or_default` is `self.cursor_trail_intensity.unwrap_or(1.0)`, clamped to 0.0..=1.0 with a non-finite value failing OFF to 0.0, so the new literal is the default an unset key really gets and the old one had rotted; the remaining `intensity: 0.7` in this crate is a `resolve_cursor_glow` test fixture, not a default; the stated range `0.0..=1.0` still matches the clamp; 2026-09-13 read of the ONE line e61ee4b2b moved (the September 13 integration with the comet and the vivid rail): the `cursor_trail_style` roster in the sample config gained `rainbow kitty flat (the A/B control: the flat body of 2026-09-13, before the comet body and its vivid rail; aliases \"rainbow flat\"/\"flat rainbow\"/\"nyan flat\")`, checked against prefs.rs — the canonical name is in the style roster, the three aliases map to it in the alias table, and it resolves in the style match — so every spelling the line offers is one a config can actually set; no other style row changed; 2026-09-14 the prose moved by a TEST added here, not by a help claim — `aterm help config` states how many keys `--write-config` ships and that number lives in aterm-cli, which cannot read this private const, so `starter_config_key_count_matches_the_manual` counts STARTER_CONFIG's distinct commented `key =` names (table-scoped included, the same thing a reader counts in the written file) and reds when they move, naming the manual line to move with it; the count was measured at 159 against the const and the manual's `158` was the stale copy; 2026-09-15: the commented `# [machine]` starter block read against app_config.rs MachineConfig (two keys, both defaults act; [key_sequences] stays last); 2026-09-15 integration read: the starter contains 161 distinct commented keys after adding both machine settings. CONFIG_PAGE and starter_config_key_count_matches_the_manual now name that measured count. The complete `aterm pkg machine apply` command stays on one starter-comment line, matching the CLI handler and its disclosure regression; 2026-09-14 v0.86 candidate merge: this file carries BOTH halves of prose — upstream's and the candidate's — each read by its own author against the same handlers, and the row records the UNION's hash; 2026-09-15 read at the merge of feat/round-15-receipts over main of the prose f3a5467ed moved with no row (shown by `xtask gate help-surfaces --diff`, matched at 595e13613): HELP_TITLE split out of HELP_HEAD, which now opens on its blank line, and the --help format `{HELP_TITLE}{}\\n{HELP_HEAD}{}{HELP_TAIL}` — against parse_cli's `-h | --help` arm (the title, aterm_types::identity::ORIGIN_LINE, then the body with keys_help and HELP_TAIL, then WINDOWS_HELP_TAIL on Windows only); no claim contradicted; landing of the erase pending-wrap fix, over f3a5467ed: read the split-out HELP_TITLE, HELP_HEAD's new leading newline, the --help format string and its origin-line comment against the -h/--help print arm, aterm_types::identity::ORIGIN_LINE and its assembly test, keys_help, the man page NAME line, and the 1200-byte window of the Windows help-arm scan (now at 941 bytes), and no changed claim contradicted the code; the unchanged doc comment the commit moved onto HELP_TITLE still called the help text a single concat when it is five pieces joined at print time (HELP_TITLE, ORIGIN_LINE, HELP_HEAD, keys_help, HELP_TAIL), and is fixed in the same commit that records this read to say the text is kept in constants printed only by the --help arm, so a no-arg / Finder launch never touches them; 2026-09-15 merge read of main (round 15) over origin/main 58bb2fc63: the merged file is byte-identical to origin/main's, whose recorded read (0bd2d71bd) already hashes to the merged prose; 2026-09-16 re-read of STARTER_CONFIG's cursor block against the keys it documents, after `cursor_trail_wake_ms` was retired: the commented line for it is deleted (the key is no longer a `Config` field and `native_config_language::RETIRED_CONFIG_KEYS` carries it, so a starter that still advertised it would be teaching a dead key), every surviving `cursor_trail*` line still names a live resolver (`cursor_trail_ms`/`_length`/`_intensity`/`_radius`/`_ring` -> the `*_or_default` methods on `app_config::Config`), and `starter_config_key_count_matches_the_manual` re-counts the file at 160 distinct keys, moved here and in `aterm help config` together; 2026-09-17 read of CHILD-SHELL ENV HYGIENE's one exception against agent_identity::env and aterm_pty::build_child_env's env_add-after-deny order",
    ),
    (
        "crates/aterm-gui/src/control.rs",
        "33a658bb8137f94e",
        "2026-09-18",
        "re-read on 2026-09-18 for the v0.88.0 assembly: the change since 2026-09-17 is 668119edd (a dead-peer verdict rests on a peer no inherited descriptor can revive) — `kill_peer_in_kernel`, the socketpair/dup2 subjects and the stranger-holds-the-read-end negative control, plus its doc; every changed literal sits inside `mod tests` (from line 9666), so no control-verb help text moved; read by diffing the surface (`gate help-surfaces --diff`) and locating each changed string; the earlier reads this row carried still hold",
    ),
    (
        "crates/aterm-gui/src/control_input.rs",
        "604873b4760f90e7",
        "2026-09-16",
        "re-read against control.rs's cross-session `key` arms, post_input_reply and lib.rs's Wake::Input arm, input_if_row_matches, pty_idem KEYED_VERBS, cmd_scroll and parse_tab by the 2026-09-10 round-3 reader after the round-2 merge; the cross-session `key` usage (code, control.rs, pinned by a test), the `mouse` fire-and-forget claim, the `sole encoder caller` claim, GUARDED_VERBS' `no id=` claim, the `hello id=1` count, and the `scroll`/`tab` grammar lines fixed in the commit that updated this row; parse_tab's doc sentence still omits `close`/`move`; merged with origin/main's own 2026-09-11 re-read of this file, conflicts resolved by the orchestrator (upstream wording kept where both sides fixed the same claim); 2026-09-14 drift sweep (lane gui, 147 claims checked over the group): re-read against the code that moved under it since this row's anchor commit; every candidate went to a verifier prompted to REFUTE it — ONE slip fixed before this row: `key_arms_own_license`'s doc said anything MODIFIED (Ctrl/Alt/Super) answers false and keeps the fence, and a79895918 added a second arm returning true for a bare Ctrl-V (the delivered-insert class beside the gesture class). The sibling help in control_media.rs had been updated for that change; this doc had not, so a bare `key ctrl+v` skipped the licence pre-clear the doc promised; 2026-09-16 read of the one prose line a29417119 added — the ERR why reply in cmd_tab — against cmd_tab's Ok(Err(why)) arm and against the claim in the comment beside it that the aimed twin and the sibling @sid close verb already use this vocabulary: cmd_tab_aimed EXISTS (crates/aterm-gui/src/control_media.rs:2597, dispatched by control.rs's tab-if-is_cross arm through Wake::TabCmdAimed) and cmd_close (control_media.rs:2741) answers ERR for its own refusals, so the vocabulary claim holds; the reasons the arm forwards come from apply_tab_cmd_in's bounds checks and close_active_native_tab's Err arms; no claim contradicted",
    ),
    (
        "crates/aterm-gui/src/control_media.rs",
        "93de2f6e5aeff231",
        "2026-09-17",
        "re-read on 2026-09-13 by drift sweep of 2026-09-13 (aterm:aterm-gui); the Screen Recording contrast, the ERR enumeration and two trail rosters were stale and are fixed in that commit; 2026-09-14 drift sweep (lane gui, 147 claims checked over the group): re-read against the code that moved under it since this row's anchor commit; every candidate went to a verifier prompted to REFUTE it — no claim contradicted the moved code; 2026-09-15 read of the new cmd_kitty doc against its body and the main-thread pair: the `Lines` framing claim against the catalog entry (v(\"kitty\", Write, Lines, App, ..)) and against the wear arm's `OK 1\n{row}\n`, the list arm's `OK {n}` + one row each; the usage refusals against the two `ERR usage:` arms; `a main-thread hop like trail/tone` against the two call_main sites and Wake::KittyCollection / Wake::KittyWear in lib.rs, whose handlers are App::kitty_collection_rows and App::wear_kitty_on_front; the `pins the cat that would ride ANYWAY` and `monotone, needs no unpin` claims against App::promotable_kitty and KittyLog::favourite_look/max_ts respectively. No claim contradicted; 2026-09-17 read of SPAWN_USAGE (`[identity=<name>|-]`), IdentitySpec/resolve_spawn_identity and the parse_spawn_args identity= arm's docs against the parser and the inherit/opt-out rule of 79ce005c3; 2026-09-17 read of the raise= prose (--diff matched at 49aefbcc7): SPAWN_USAGE's `[raise=<t|f>]`, the parse doc's `true|false` too, as `split=` takes `vertical`, and the REVIEW test doc against the parser's t|true and f|false arms beside v|vertical and h|horizontal, and the catalog's spawn summary (control_verbs.rs) saying the same letters; no claim contradicted",
    ),
    (
        "crates/aterm-gui/src/control_privacy.rs",
        "7cb7450bbe5371fb",
        "2026-09-16",
        "the row hashed a checkout behind 0a8275760/acc20c5fd/da5d75e33; re-read against parse_consent_timeout, covers_split and PrivacySnapshot::lines by the 2026-09-10 round-3 reader, the SERVICES doc (`never uncovered` vs NEVER_COVERED) fixed in 33311eda0; re-read against observer_fda_value/inert_fda_probe, ConsentState::fda -> ConsentCache::get_or_probe, app_settings.rs macos_access_projection and consent_policy by the round-3 fixer; the `live` field's `observer row's third value` claim (the row renders the inert labels as `off`, pinned at the fda=off/responsible=off asserts), the consent_panel_facts/session_consent `no syscall` claims (the cached probe's one open(TCC.db) on a miss), ConsentPanelFacts' `no covers= list` claim (the panel takes covers_split directly) and the consent_policy doc line displaced onto consent_probe_interval fixed in the commit that updated this row; the catalog's `covers=`/`uncovered=` split in control_verbs.rs still omits `unmeasured=` — that surface's own row; NOTE's doc now says its `covers is empty` clause is written for the UNMEASURED evidence read_privacy hard-codes and must turn evidence-conditional when §7 S4 lands (the measured arm of lines, tests-only today, renders a covers= list above it), the one-line fix for the round-3 reader's note finding; merged with origin/main's own 2026-09-11 re-read of this file, conflicts resolved by the orchestrator (upstream wording kept where both sides fixed the same claim); 2026-09-13 read of the one prose change since (instance_attribution's doc, shown by `xtask gate help-surfaces --diff`) against instance_attribution (Attribution::Adopted iff App::handoff_successor, else Live; ConsentPolicy::adoption maps it to Unknown only when the policy is disabled), main_entry's `handoff_successor: adopting` (true whenever the boot re-adopted handed-off shells) and app_restore.rs take_session0_shell (None, so a fresh session 0, when window 0's layout has no terminal leaf, which leaves handoff_successor true); no claim contradicted; 2026-09-14 drift sweep (lane gui, 147 claims checked over the group): re-read against the code that moved under it since this row's anchor commit; every candidate went to a verifier prompted to REFUTE it — no claim contradicted the moved code; 2026-09-14 read of the three literals f4988d1ae added (the target_os = macos cfg pair and the non-macOS `not-a-bundle` arm, shown by `xtask gate help-surfaces --diff`) against install_posture_rows, which_copy.rs's macOS-only `posture_from` re-export, bundle.rs InstallPosture::NotABundle (`nothing is wrong; there is simply no bundle`, the sentence the arm's comment quotes) and the PrivacyStatus.install doc that already lists `not-a-bundle` among its tokens; no slip found; 2026-09-14 read of the one sentence 6c6a65787 added to install_posture_rows's doc (canonicalized on unix only, shown by `xtask gate help-surfaces --diff`) against the cfg(unix) canonicalize in install_posture_rows, which_copy.rs observe's own cfg(unix) canonicalize and its comment on the verbatim spelling Windows answers, and the `install={} running={}` row that prints the path for a human; no slip found; 2026-09-14 (third audit lane, last merge): origin's 47198639f ('stabilize kitty motion and redraw trails') moved this file's prose (51 line(s)) without a row and this lane never touched the file; the union's hash is recorded at the merge, the read owed to that commit's author; 2026-09-15 read of 47198639f (12 hunks, 30 removed / 77 added, shown by `xtask gate help-surfaces --diff`) against covers_split, observed_fda_scope, app_data_covered, finish_privacy_read, PrivacyRead, session_consent and NOTE: app-data enters `covers=` iff fda=Granted and the scope is ThisProcess|NewProcesses, a Granted probe with scope Unknown is promoted to ThisProcess and never to NewProcesses, every other service stays `unmeasured` until fda_coverage_measured and NEVER_COVERED stays `uncovered` either way — the docs' new posture to the letter; the worker's wait has one deadline, Pending cannot extend it and a failed re-read keeps the last Pending snapshot (pinned by privacy_read_has_one_deadline_and_keeps_stuck_work_pending); PrivacyRead is `lines` plus a `pending` bool and the public rows are unchanged; session_consent still joins with SpikeEvidence::UNMEASURED and observed_eperm=false, so an adopted session inherits nothing (the derive model's NoInheritedGrant invariant); the one surface this change contradicted was the CATALOG's privacy row in control_verbs.rs (`so fda_scope=unknown`), fixed in the commit that updated this row; no claim in this file contradicted; 2026-09-15: install_posture_rows doc re-read after the Linux fix — off macOS the token is `not-a-bundle` by construction, and the unix-only canonicalize note from upstream is kept beside it 2026-09-15: re-read folder_names() against consent::Folder::ALL after the roster gained `app-data`; the `folder` row now carries a fourth name before the two volume classes, the format is unchanged, and the doc says what the new name is.",
    ),
    (
        "crates/aterm-gui/src/control_query.rs",
        "8e966d250bcbe31b",
        "2026-09-16",
        "the row predates ffda97813 (`text tail=`/`rows=`), which the round-2 merge carried; re-read against cmd_modes, cmd_cell, cmd_metrics/cmd_metrics_json by the 2026-09-10 round-3 reader and against text_args/TextShape::select/frame_rows_reply/cmd_text_opt by the round-3 fixer (that delta matches its code); the `modes` frame (`OK <n>` and twelve keys, not `OK` and seven — pinned by modes_frames_its_count_and_twelve_keys), the `cell` attrs vocabulary (`wide`/`wide_cont`, pinned by cell_attrs_carry_the_width_markers) and the JSON `percentiles` doc (it now names the reflow quartet the text form carries and the JSON body omits; the code gap stands) fixed in the commit that updated this row; merged with origin/main's own 2026-09-11 re-read of this file, conflicts resolved by the orchestrator (upstream wording kept where both sides fixed the same claim); 2026-09-13 read of the `offscreen` prose feat/round-7-offscreen added (shown by `xtask gate help-surfaces --diff`, matched at a94e1bc9b) — OFFSCREEN_USAGE/OFFSCREEN_BAD_SINCE/OFFSCREEN_DEFAULT_MAX, OffscreenSince/OffscreenArgs, offscreen_number, offscreen_args, cmd_offscreen and format_offscreen_reply, the header format strings, and the offscreen_tests/offscreen_wire_tests docs — against the parser (each key once, ASCII digits only, tail/max > 0, screen=1 only), cmd_offscreen's other-origin/bad-since arms and its one-lock clone-out, format_offscreen_reply's field order (back_at/pin only when back > 0, enabled=0 only when off, last= the page's own last row), aterm-core alt_archive.rs AltArchive::read/page_last/set_budget/set_enabled/wipe and AltArchiveRead's fields, and env_opted_out; one slip fixed before this row (OFFSCREEN_DEFAULT_MAX said a bare poll never ships a whole archive, false for one under 2000 rows — it now says at most that many rows); 2026-09-14 read of the offscreen doc's since= bullet round 10 moved (shown by `xtask gate help-surfaces --diff`, matched at 547991d5b) — a mark from a restarted process, or from one whose self-update handoff could not carry the archive, reads from the start, and one that did keeps its origin — against cmd_offscreen's other-origin arm (a since= origin that is not archive.origin() reads from 0), aterm-core alt_archive.rs AltArchive::import (a non-zero carried origin replaces this process's; Refused leaves it) and spawn.rs alt_archive_origin/new_live_terminal; no claim contradicted; 2026-09-13 release-candidate merge of the perf port: the ONE prose change since that read is the per-site UI-thread terminal-mutex wait fragment the port added (`text_term_wait_fields` and its JSON twin `json_term_wait_fields`, plus the two `metrics` format strings that carry them), read here against crates/aterm-gui/src/metrics.rs TermWaitSite (a THREE-member enum RedrawA/RedrawB/Press with `ALL` a const array of all three and `label()` a const match to `redraw_a`/`redraw_b`/`press` — so the doc's `empty never (the sites are static)` holds and the report order is the doc's order), term_wait_distribution/term_wait_max_ns, and the two builders themselves: the text form writes a LEADING-SPACE run ` n_term_wait_<site>=<count> term_wait_<site>_p50_ms= _p95_ms= _p99_ms= max_term_wait_<site>_ms=` per site and the JSON twin the same fields LEADING-COMMA and quoted, each from the same histogram and the same `term_wait_max_ns`, appended at the matching `{}` of the text and JSON percentile lines; no claim contradicted; 2026-09-14 drift sweep (lane gui, 147 claims checked over the group): re-read against the code that moved under it since this row's anchor commit; every candidate went to a verifier prompted to REFUTE it — no claim contradicted the moved code; 2026-09-14 merge read of feat/round-10-carry over main: the merged prose is exactly round 10's offscreen since= bullet (a mark from a restarted process, or from one whose self-update handoff could not carry the archive, reads from the start; one that did keeps its origin) plus main's perf-port term-wait fragment (the text_term_wait_fields and json_term_wait_fields docs and strings, and the extra `{}` in both metrics format strings) — `xtask gate help-surfaces --diff` against main's read (matched at 23f4c32fe) shows round 10's one hunk and against round 10's read (matched at a4a023d4a) main's three, nothing else — checked against the merged cmd_offscreen other-origin arm, aterm-core AltArchive::import and spawn.rs new_live_terminal/alt_archive_origin (main's spawn.rs hunks do not reach them), and metrics.rs TermWaitSite (ALL = RedrawA, RedrawB, Press; label() redraw_a/redraw_b/press) with the two builders' leading-space and leading-comma fragments appended ahead of the echo_rtt fields in cmd_metrics and cmd_metrics_json; no claim contradicted; 2026-09-15 (2026-09-16 UTC) read of the prose main's seven metrics/watchdog commits moved since the recorded read (shown by `xtask gate help-surfaces --diff`, matched at a4c4d3eaa) — the `percentiles` text and JSON format strings' new key_queue and acquire_queue fields, the module doc's WHEN, AND WHOSE / THE PARK OUTSIDE THE REDRAW / US OR THEM, ON THE SAME LINE blocks, the reworded `input_*` bullet and the new `present_glass` sentence, and the docs of the five new or rewritten tests — each against the code beside it: cmd_metrics's percentiles arm (n_key_queue + key_queue_p50/p95/p99_ms + last_/max_key_queue_ms spliced BESIDE the n_key_write it splits, n_acquire_queue beside n_acquire, then text_term_wait_fields, echo_rtt::percentile_fields_text and metrics::present_glass_fields_text) and its summary arm (metrics::lateness_fields_text after stale_arm_heals, then echo_rtt::percentile_fields_text and watchdog::turn_census_fields_text, in that order, after first_visible_ms); metrics.rs note_key_arrival_queued (the NSEvent backdate also recorded into H_KEY_QUEUE/LAST_/MAX_KEY_QUEUE_NS), INPUT_STAMP_NS's PER-WINDOW ATTRIBUTION note and book_input_present (a key routed to a window is closed only by that window's content present; a session-routed stamp still by any window's), lateness_fields_text/_json (max_wake_late_ms/_owner/_at_ms, max_deadline_late_*, max_present_latency_at_ms, max_present_latency_gap_ms, max_input_present_at_ms and metrics_now_ms, every stamp on the now_ns process clock), the wake arms (a Timer wake stores now - due into LAST_WAKE_LATE_NS and the next WaitCancelled stores 0 over it, while MAX_WAKE_LATE_NS fetch_maxes and stamps _AT_NS), should_log_slow_present (SLOW_PRESENT_LOG_THRESHOLD_NS = 3 x SLOW_FRAME_THRESHOLD_NS = 100 ms, SLOW_PRESENT_LOG_MIN_GAP_NS = 10 s, ungated in every build beside the $ATERM_TRACE_LATENCY stderr line), and present_glass_fields_text against aterm-gpu present_glass.rs + metal/swapchain.rs (presentDrawable: registration -> the drawable's presentedTime); watchdog.rs turn_census_fields_text/_json (max_turn_ms, max_turn_owner, max_turn_at_ms, last_turn_ms, turns, long_turns, long_turn_threshold_ms) with LONG_TURN_THRESHOLD_NS = metrics::SLOW_FRAME_THRESHOLD_NS (33.33 ms, one 30 fps frame), Breadcrumb::metric_name's user_event and RELEASE_STALL_THRESHOLD = 5 s; and renderer.rs await_frame_slot / drain_pending (both unbounded waitUntilCompleted, the ring drain booked through add_gpu_park before the present's work timer starts). No claim in this file contradicted — but the ctl catalog's copy of the same summary line was stale and is fixed in this commit (see the crates/aterm-types/src/control_verbs.rs row); 2026-09-16 read at the merge of 48695d2d6 (the find highlight and a width reflow), which landed without re-recording: the doc of FullHistorySearch::history_renumber_epoch (shown by `xtask gate help-surfaces --diff`) — that it is `Grid::history_renumber_epoch()` the rows were keyed against, travels with the results as absolute_row_revision does, and covers a width reflow that renumbers every retained row while moving no footer revision — against aterm-grid pin_methods.rs history_renumber_epoch (a monotonic epoch of renumberings invisible to the (content_gen, absolute_row_revision) pair; a WIDTH reflow advances it), the key taken beside the results (`t.grid().history_renumber_epoch()` at the snapshot) and the torn check that compares it with the revision and content_seq; no claim contradicted the code",
    ),
    (
        "crates/aterm-gui/src/control_session.rs",
        "0725024abb2c15a9",
        "2026-09-17",
        "re-read on 2026-09-12 against the code that moved under it since the roster was minted, by the 2026-09-12 drift sweep (lane aterm-gui-1); no claim contradicted the moved code; 2026-09-13 read of the prose feat/round-7-offscreen moved (shown by `xtask gate help-surfaces --diff`, matched at c811b7c5a) — the history record's `arch={}` field and cmd_history's doc — against cmd_history's format (arch= before text=), turn_ledger.rs ArchMark (Display `<origin>:<last>`, ArchMark::of from the term's archive) stamped in cmd_turn after preflight and before the yield, control_query::offscreen_args's since=<origin>:<i>, and the readers that cut a row at ` text=` (aterm-link hook.rs strip_body, aterm-agent report.rs parse_turn_line); no claim contradicted; 2026-09-14 read of the prose round 10 added (shown by `xtask gate help-surfaces --diff`, matched at 151dc6e1b) — history's ` carried=1` format piece and cmd_history's doc, NEXT_TURN_ID's handoff sentences, turn_ids_minted and raise_turn_ids — against cmd_history's format (carried=1 between arch= and text=), turn_ledger.rs TurnRecord::carried (started_ms on the old process's epoch, seq its engine's content_seq), cmd_turn's mint (fetch_add then + 1), raise_turn_ids' fetch_max clamped to handoff_carry::MAX_TURN_ID, seamless.rs take_incoming's raise from the manifest's next_turn_id and each carry's high_turn_id while single-threaded, and app_update_handoff.rs's manifest_turn_id(turn_ids_minted()); one slip fixed before this row (turn_ids_minted said the last id this process minted, 0 before the first, where after an adoption it is the count continued from the old process before this one mints any); 2026-09-14 drift sweep (lane gui, 147 claims checked over the group): re-read against the code that moved under it since this row's anchor commit; every candidate went to a verifier prompted to REFUTE it — no claim contradicted the moved code; 2026-09-14 merge read of feat/round-10-carry over main: the merged prose is exactly round 10's read (history's ` carried=1` piece and cmd_history's doc, NEXT_TURN_ID's handoff sentences, turn_ids_minted and raise_turn_ids) plus no prose of main's: main's one change to this file is code (the two test SessionCtx literals gain `modes` and `ui_waiting`), so the hash is round 10's, and `xtask gate help-surfaces --diff` against main's drift-sweep read (matched at 23f4c32fe) shows round 10's three hunks and nothing else; checked against the merged cmd_history (carried=1 between arch= and text=), cmd_turn's mint, raise_turn_ids' fetch_max clamped to handoff_carry::MAX_TURN_ID (i64::MAX) and seamless.rs take_incoming, none of which main touched; no claim contradicted; 2026-09-17 read of the `sessions` doc's identity= column (last, after detail=, `-` for none) against sessions_lines and the identity pair test",
    ),
    (
        "crates/aterm-gui/src/fabric.rs",
        "07b285a2214431b4",
        "2026-09-17",
        "re-read on 2026-09-12 against the code that moved under it since the roster was minted, by the 2026-09-12 drift sweep (lane aterm-gui-2); the note_bridge_supervised caller claim was falsified by `fabric attach` and is fixed here; 2026-09-14 drift sweep (lane gui, 147 claims checked over the group): re-read against the code that moved under it since this row's anchor commit; every candidate went to a verifier prompted to REFUTE it — no claim contradicted the moved code; 2026-09-14 read of the round-13 prose (FABRIC_STALLED, LINK_STARTING / LINK_BRIDGE_LOST, LinkReport, LANE_GENERATION and its two accessors, fabric_link_facts / fabric_status_tail, bridge_attached's attach-is-stalled paragraph, link_report, cmd_link / LINK_USAGE, link_reason_token, wake_parked, bridge_lost's link stamp, cmd_post's stalled note, fabric_wait_refusal's stalled paragraph, the `link` HALT_EXEMPT entry and the three new test docs) against the code beside each: link_report drops a report whose generation is not the owner's, one arriving before any attach, and one arriving once the state is DISCONNECTED, stores CONNECTED or STALLED under the generation lock and wakes every registered session only when an up link went down; bridge_attached stores STALLED with reason=starting and clears rtt/acked_at; bridge_lost keeps the numbers and stamps reason=bridge-lost; cmd_post's entry and per-wake checks read fabric_state() != connected so a stalled fabric answers fabric_wait_refusal at once, and bridge_reachable is true once a bridge has attached so the token is queued=1; no claim contradicted; 2026-09-14 read of the round-13 review's prose (LinkReport.reported's doc, bridge_attached's amended doc, link_evidence's doc with its measured claim, link_report's first-report clause, wait_is_futile's doc, the `apply_fleet_halts` renames and the four new test docs) against link_evidence (the owner generation only, under FABRIC_STALLED only, returns once `reported` or off `starting`, rtt None and acked_at now, state -> CONNECTED), link_report (`reported = true`; `(was_up || was_starting) && !up` wakes the parked waiters), wait_is_futile (connected false; stalled -> `reported || reason != starting`; else true), cmd_deliver and cmd_outbox_sent calling link_evidence(lane_generation()), and aterm-link bridge.rs apply_fleet_halts/converge_hold under those names; no claim contradicted; 2026-09-15 read of round 15's key prose (shown by `xtask gate help-surfaces --diff`, matched at c1fc82257): POST_USAGE's key= and OUTBOX_USAGE's dup=1 against cmd_post/cmd_outbox_sent, the outbox line's `key=` and the in-flight post row's against cmd_outbox/cmd_inbox, PostRow's key and dup docs against retire_post (the key cleared with the body; dup sticky across a retried retirement and refused on off=-) and wait_landing's `OK <id> off=<n> dup=1`, KEY_MAX/valid_key's charset, the `a post without key=` clause in fabric_wait_refusal's doc and the wait-refusal test's, and the post-key test's doc and strings against that test; no claim contradicted; 2026-09-15 read of round 15's receipts and fetch-by-offset prose (shown by `xtask gate help-surfaces --diff`): the usage strings (DELIVER_USAGE's fetched= and receipt= forms, INBOX_USAGE's `inbox get @<off>`, POST_USAGE's --wait-ack, await inbox re=) against the parsers they name (cmd_deliver -> deliver_fetched/deliver_receipt/parse_deliver_fields, cmd_inbox_get -> inbox_get_at, cmd_post, cmd_await_inbox); the reply formats (Fetched::render, cmd_inbox's oldest_on_bus header, wait_receipt's ack=/ERR expired/ERR timeout … off=); the constants' docs (VERDICTS, FETCH_SLOTS/FETCH_WAIT_MS against aterm-link bridge.rs IDLE_TICK 250 ms and the roster backstop, RECEIPTS_OWED_MAX, EXPIRY_GRACE_MS against the bound cmd_post computes); OwedReceipt's history against the bridge's reconnect back-off (every mailbox item but Closed(Aterm) is dropped) and send_receipt; deliver_fetched's chunk rule (at= must equal the partial body's length, at=0 restarts, len= above BODY_MAX accepted, a refusal fails the parked read); and the test docs against their tests. ONE doc INCOMPLETE and FIXED here: FetchSlot::Failed listed the bridge's reasons without `refused` (answer_fetch's follow-up to a chunk the endpoint refused) and did not say `malformed` is written by refuse_fetched itself. No claim contradicted; 2026-09-14 v0.86 candidate: the file carries both halves of prose — upstream's and this candidate's — each read by its author against the same handlers, and this row records the UNION's hash; AND UPSTREAM'S READ OF THE SAME PROSE, kept: re-read on 2026-09-12 against the code that moved under it since the roster was minted, by the 2026-09-12 drift sweep (lane aterm-gui-2); the note_bridge_supervised caller claim was falsified by `fabric attach` and is fixed here; 2026-09-14 drift sweep (lane gui, 147 claims checked over the group): re-read against the code that moved under it since this row's anchor commit; every candidate went to a verifier prompted to REFUTE it — no claim contradicted the moved code; 2026-09-14 read of the round-13 prose (FABRIC_STALLED, LINK_STARTING / LINK_BRIDGE_LOST, LinkReport, LANE_GENERATION and its two accessors, fabric_link_facts / fabric_status_tail, bridge_attached's attach-is-stalled paragraph, link_report, cmd_link / LINK_USAGE, link_reason_token, wake_parked, bridge_lost's link stamp, cmd_post's stalled note, fabric_wait_refusal's stalled paragraph, the `link` HALT_EXEMPT entry and the three new test docs) against the code beside each: link_report drops a report whose generation is not the owner's, one arriving before any attach, and one arriving once the state is DISCONNECTED, stores CONNECTED or STALLED under the generation lock and wakes every registered session only when an up link went down; bridge_attached stores STALLED with reason=starting and clears rtt/acked_at; bridge_lost keeps the numbers and stamps reason=bridge-lost; cmd_post's entry and per-wake checks read fabric_state() != connected so a stalled fabric answers fabric_wait_refusal at once, and bridge_reachable is true once a bridge has attached so the token is queued=1; no claim contradicted; 2026-09-14 read of the round-13 review's prose (LinkReport.reported's doc, bridge_attached's amended doc, link_evidence's doc with its measured claim, link_report's first-report clause, wait_is_futile's doc, the `apply_fleet_halts` renames and the four new test docs) against link_evidence (the owner generation only, under FABRIC_STALLED only, returns once `reported` or off `starting`, rtt None and acked_at now, state -> CONNECTED), link_report (`reported = true`; `(was_up || was_starting) && !up` wakes the parked waiters), wait_is_futile (connected false; stalled -> `reported || reason != starting`; else true), cmd_deliver and cmd_outbox_sent calling link_evidence(lane_generation()), and aterm-link bridge.rs apply_fleet_halts/converge_hold under those names; no claim contradicted; 2026-09-15 recovery audit: read the explicit-incarnation additions against link_evidence and link_report, which require Some of the current nonzero generation and preserve lost-bridge precedence under the existing generation lock; read the scoped fixture, None/stale/current admission matrix and historical negative control against serve_bridge TLS setup and the two real writers. Corrected the old Ok(bool) return description to its actual bool API; no claim contradicted.; 2026-09-14 v0.86 candidate merge: this file carries BOTH halves of prose — upstream's and the candidate's — each read by its own author against the same handlers, and the row records the UNION's hash; 2026-09-15 merge read of feat/round-15-receipts over main: the merged prose is exactly round 15's change (the key, receipts and fetch-by-offset prose: POST_USAGE, OUTBOX_USAGE, DELIVER_USAGE and INBOX_USAGE, the outbox and inbox row formats, the PostRow, OwedReceipt, FetchSlot and Fetched docs, VERDICTS, FETCH_SLOTS, FETCH_WAIT_MS, RECEIPTS_OWED_MAX and EXPIRY_GRACE_MS, the cmd_inbox_get, inbox_get_at, deliver_fetched, deliver_receipt and wait_receipt docs, and the round-15 test docs and strings, shown by `xtask gate help-surfaces --diff` against main's read at 595e13613) plus upstream's change (the explicit-incarnation sentences of link_evidence and link_report and link_report's bool return, the LINK_SECTION and link_write_is_foreign docs, on_lane's doc, and the unscoped-delivery and admission-model test strings, shown against round 15's read at 030ba1788) plus ONE tense fixed at this merge: LINK_SECTION's doc said a None lane generation `is exactly the shape link_evidence's own gate accepts`, where since the recovery audit that gate returns on None; it now says the gate accepted it until it came to require the explicit incarnation that owns the link, checked against link_evidence and link_report (each returns before any store unless the generation is Some of the nonzero owner; link_report also on a DISCONNECTED state), link_write_is_foreign under with_link_reset's Section, cmd_deliver's fetched= and receipt= forms (deliver_fetched, deliver_receipt), cmd_outbox's receipt and fetch trailer lines, and aterm-link bridge.rs drain_outbox, which answers the parked fetches first and reads the roster only before receipts or posts, so the fetch docs' `next drain (the idle tick, 250 ms; the roster backstop, 2 s, under load)` still holds; 2026-09-15 read of the new `kitty` HALT_EXEMPT entry against what the verb does: `puts no bytes on a PTY, retires no session and drives no program` read against control_media::cmd_kitty (two call_main hops only), App::wear_kitty_on_front and App::wear_kitty in app_input.rs (KittyLogHost::wear plus cursor_cat.on_collect and a redraw — no sink, no session retirement), so it is the `rain`/`fx` class it claims; `writes the machine-owned toy ledger` read against KittyLogHost::wear's delta.favourite_collectible + maybe_flush onto the kitty-collectibles path, which is also the argument menu.rs already records for FavouriteKitty's WriteInput class. The doc block's `SIX MEMBERS ARE NOT Target::Session ROWS` count is untouched: `kitty` joins the EXEMPT list, not the halt set, and both the_halt_set_is_derived_from_the_verb_table and the_hold_help_enumerates_exactly_the_halt_set pass. No claim contradicted; 2026-09-17 read of the `identities` HALT_EXEMPT argument against is_pty_reaching and agent_identity::forget_reply (a directory read/removal, refused while a live session carries the identity)",
    ),
    (
        "crates/aterm-gui/src/hwkey.rs",
        "b2436705411d74a4",
        "2026-09-14",
        "re-read on 2026-09-12 against the code that moved under it since the roster was minted, by the 2026-09-12 drift sweep (lane aterm-gui-2); the main-thread-park claim was falsified by the off-thread drawable and is fixed here; 2026-09-14 drift sweep (lane gui, 147 claims checked over the group): re-read against the code that moved under it since this row's anchor commit; every candidate went to a verifier prompted to REFUTE it — no claim contradicted the moved code",
    ),
    (
        "crates/aterm-gui/src/menu.rs",
        "f4881807f5180815",
        "2026-09-15",
        "read against its parser/dispatch by the 2026-09-10 sweep (aterm-B-window-and-clients); findings fixed in the commit that added this row; 2026-09-15 read of the new NextKitty action doc and its `Next Kitty` View row: `FavouriteKitty pins the cat that would ride ANYWAY (the focused window's tenured program cat, else the launch kitty)` read against App::promotable_kitty (kitty_tenure.worn().map(|i| i.look).unwrap_or(self.launch_kitty)) and App::favourite_kitty, which promotes exactly that; `walks the collection, one cat per press, wrapping at the end` read against App::wear_next_kitty ((i + 1) % rows.len() over KittyLogHost::wearable's roster, starting at 0 when nothing is worn); `kitty wear <key> is the addressed form` read against the shipped verb row v(\"kitty\", Write, Lines, App, ..) and control_media::cmd_kitty. The WriteInput comment beside it read against the same toy-ledger argument the FavouriteKitty arm above already carries. No claim contradicted 2026-09-16: the one sentence b690b30403 moved (FavouriteKitty's doc, which now names `App::favourite_kitty_checked` instead of the discarding wrapper it deleted) read against the code it points at, by the macOS-doctor audit lane, which found the row red and owed: app_input.rs's MenuAction::FavouriteKitty arm calls favourite_kitty_checked(wid, now) and puts its Err in `pending_action_refusal`, so the doc's claim that it answers on glass is now literally what happens rather than a discarded verdict; favourite_kitty_checked exists at app_input.rs:3928 and gates on ensure_sparkle + the effects master and feline sub-gates; wire id 46 matches menu.rs's to-id (394) and from-id (456); the legacy `FavouriteSessionKitty` spelling is still mapped by canonical_invoke_name (481); and the action is absent from requires_terminal_tab (490) and present in the WriteInput class (664), which is what the process-wide, never-terminal-only claim says. No slip found; the rest of the page is unchanged since the 2026-09-15 read.",
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
        "2026-09-14",
        "read pty_idem.rs (module/Realm/PRODUCER_CAP/dup_reply/guarded/record_in_doubt docs, USAGE, KEYED_VERBS, test docs) against control.rs dispatch + run_feed_bin_routed, control_input.rs guarded press/take_leading_options, control_session.rs cmd_turn_guarded, control_verbs.rs catalog + framing_of, aterm-ctl stream_count/malformed header, aterm-link bridge feed key by workflow aterm-help-surfaces-read on 2026-09-10; 4 low findings left; merged with origin/main's own 2026-09-11 re-read of this file, conflicts resolved by the orchestrator (upstream wording kept where both sides fixed the same claim); 2026-09-14 drift sweep (lane gui, 147 claims checked over the group): re-read against the code that moved under it since this row's anchor commit; every candidate went to a verifier prompted to REFUTE it — no claim contradicted the moved code",
    ),
    (
        "crates/aterm-link/src/cli.rs",
        "ea0cb01ddf7c8c5b",
        "2026-09-17",
        "re-read on 2026-09-12 against the code that moved under it since the roster was minted, by the 2026-09-12 drift sweep (lane aterm-link); no claim contradicted the moved code; the 0.84 train's release-candidate merge (this machine's provenance/repair/TCC work over the peers' 2026-09-12 re-read at the tip) moved the bytes once more — both halves were read by their authors as recorded, the merged file is their union, and this row records the union's hash; 2026-09-14 drift sweep (lane link, 26 claims checked over the group): re-read against the code that moved under it since this row's anchor commit; every candidate went to a verifier prompted to REFUTE it — ONE slip fixed before this row, and it inverted a SECURITY boundary: `broker`'s doc said the socket 'is same-uid only'. Nothing chmods it and no peer uid is checked — the USAGE string ten lines below already says it is created world-connectable and that the boundary is the 0700 DIRECTORY you put it in, so the doc contradicted the help it introduces; 2026-09-14 read of the prose added since (USAGE's `aterm-link fabric` line and parse's doc on why it is now pub(crate)) against dispatch's new `fabric` arm (crate::fabric::main, the same entry crates/aterm/src/main.rs routes `aterm fabric` to) and crates/aterm-link/src/fabric.rs's bridge_config, which reads `[fabric] command` through this parse, by feat/round-11-fabric-ledger; no claim contradicted; 2026-09-14 read again on the merge branch: USAGE's `aterm-link fabric` line and parse's pub(crate) doc against dispatch's `fabric` arm (crate::fabric::main, the entry crates/aterm/src/main.rs also routes `aterm fabric` to) and fabric.rs bridge_config, which reads the `[fabric] command`'s fleet, broker, cap files and state dir through THIS parse; no claim contradicted; 2026-09-14 round-12: the `hook` synopsis line gained --merge, --dry-run and --check, read against hook::parse's flag table; nothing else in the file moved; 2026-09-14 read of the prose added since (USAGE's `aterm-link fabric` line and parse's doc on why it is now pub(crate)) against dispatch's new `fabric` arm (crate::fabric::main, the same entry crates/aterm/src/main.rs routes `aterm fabric` to) and crates/aterm-link/src/fabric.rs's bridge_config, which reads `[fabric] command` through this parse, by feat/round-11-fabric-ledger; no claim contradicted; 2026-09-14 read again on the merge branch: USAGE's `aterm-link fabric` line and parse's pub(crate) doc against dispatch's `fabric` arm (crate::fabric::main, the entry crates/aterm/src/main.rs also routes `aterm fabric` to) and fabric.rs bridge_config, which reads the `[fabric] command`'s fleet, broker, cap files and state dir through THIS parse; no claim contradicted; 2026-09-14 merge read of feat/round-12-hooks over main: the merged prose is exactly this branch's `hook` synopsis line (--merge, --dry-run and --check, against hook.rs parse's flag table) plus main's `aterm-link fabric` USAGE line, the `fabric` word in the verb list and parse's pub(crate) doc (checked against dispatch's `fabric` arm calling crate::fabric::main, fabric.rs USAGE and parse_args — `[status] [--json]`, `tail [--bodies] [--from <offset>]`, `help` — and bridge_config's crate::cli::parse(&flags)); ONE SLIP FIXED at the merge, in this branch's prose: the rewritten synopsis had silently dropped `[--rewake]`, which hook.rs parse and hook's own USAGE line still take — restored between [--dry-run] and `| run`; 2026-09-14 read of the USAGE prose added (the `fabric` line's `on|off|doctor` forms, the paragraph on the read-side verbs defaulting their flags from the rendezvous file, and the broker usage's and mint doc's `aterm fabric on` in place of the script) against dispatch (ls, glance and tui through enable::with_rendezvous_defaults, mirror through enable::with_default_sock, serve deliberately not — the comment on the arm says why) and crates/aterm-link/src/enable.rs fill_defaults (only an absent flag is prepended, so a flag on the command line wins), with_default_sock (`<dir>/aterm.sock` only when the rendezvous file and that socket exist) and Rendezvous::read (no file leaves argv alone; an unreadable one is said on stderr and leaves it alone), by feat/round-13-fabric-on; no claim contradicted; 2026-09-14 read of the round-13 C prose (feat/round-13-fabric-on: the module doc's writer roster — `driving=` alone with no writer, `role=`/`detail=`/`phase=`/`context=`/`title=` written by a meta-mode bridge, the six columns appended to §7's — USAGE's `--presence` entry and the `ls` paragraph, `column`'s two comments naming presence::token, Parsed::presence_given, and the column test's rewritten docs) against parse (`--presence meta|minimal` through presence::Mode::parse, presence_given set only by the flag), run's serve arm (cfg.presence = fabric::presence_from_config() when the flag was absent; `ls` never reads the file), row's format string (phase= context= title= after epoch=, every column through `column`, so the bridge's `context=12%` prints `12%25`), and presence.rs Fields::tokens; no claim contradicted; 2026-09-14 read of the rewritten `--presence` entry (phase= read off the last TAIL_ROWS = 40 rows only on a `status revision=` move, a session running anything but Claude Code reads idle, title= is `meta set title` alone, minimal reads no screen, else the file's `[fabric] presence`, else meta) against presence.rs read_meta (`user_title=` alone, `-` when unset), phase_word over aterm_phase::worker_phase (Idle when no prompt box, busy signal, limit notice or trailing `?` is on the rows), bridge.rs sample_presence with Slot::needs_screen (the tail read only when the revision moved) and fabric::presence_from_config, and of the merged USAGE (the round-12 `hook` synopsis beside the round-13 `fabric` line) at the rebase onto main's round-12 merge; no claim contradicted; 2026-09-14 audit fixes read against dispatch: the USAGE's backslash-n line continuations that lost the broker/mint rows' indent are plain newlines, the `asb` clause names `aterm link broker`; dispatch gained `-h|--help|help` (stdout, exit 0 — the contract `aterm --help` and the `broker -h`/`mint -h` children already kept) and an unknown-subcommand arm refused BY NAME (the header's own rule, which `wake|pin|lash` got and a typo did not); `broker` refuses a surplus argument and removes a log THIS call created when the bind is refused; `mint` refuses a repeated --secret-file (the empty key used to win last, exit 0); 2026-09-14 merge read of main over 28508563a: the merged prose is exactly round 13's `--presence` entry, the ls row's phase=/context=/title= columns and the module doc's SIX ADDED COLUMNS / THREE-without-a-writer paragraphs plus main's audit additions (the reflowed USAGE rows, `aterm link broker` in the identity clause, dispatch's `-h | --help | help` and unknown-subcommand strings, broker's unexpected-argument and mint's --secret-file-twice refusals), checked against dispatch (the help arm prints USAGE on stdout and exits 0; an unknown word is refused by name, exit 2), broker (a surplus word is `unexpected argument`), mint (`--secret-file given twice`, SECRET_MIN = 32), parse's --presence arm and crate::presence Fields::tokens. ONE SLIP IN ROUND 13'S OWN PROSE FIXED at the merge, and the hash recorded here is the corrected file: the module doc said title= is the session's user title `else its terminal title`, where presence.rs takes `meta user_title=` and NOTHING ELSE and publishes `-` without one — the doc now says so; 2026-09-15 read of the `--receipts`/`--no-receipts` usage entry, the flag names in the parser's strings and the receipts_given doc against parse_args (both flags set cfg.receipts and receipts_given; accepted on every verb and read only by `serve`, as `--presence` is), main's `serve` arm (receipts_from_config only when neither flag was given), fabric.rs receipts_from_config (off when the key or the file is absent; off with a stderr line when unreadable or not a boolean), enable.rs (writes `receipts = true` once when the table lacks the key; an operator's `false` stands) and bridge.rs send_receipt (acks only an ask/task with one of the three verdicts, onto the sender's lane per receipt_lane; with receipts off every owed receipt is retired `off=-`); no claim contradicted; 2026-09-15 read of the top-level `hook` synopsis line's new `[--report-to @sid]` against hook.rs parse (the flag is accepted by `install claude` and `run`, acted on by `stop`) and hook.rs USAGE; no claim contradicted; 2026-09-14 (2026-09-15 UTC) re-read of the `hook` synopsis line's `[--report-to @sid]` against hook.rs parse (the flag is accepted on every hook argv; the installer writes it on the Stop command only, claude_settings_for) and hook.rs USAGE; no claim contradicted; 2026-09-15 merge read of feat/round-15-receipts over main: the merged prose is exactly round 15's change (the `--receipts`/`--no-receipts` usage entry, the two flag names among parse_args' strings and the receipts_given doc: 3 hunks, shown by `xtask gate help-surfaces --diff` against main's read at 6d66e6858) plus upstream's change (the `hook` synopsis line's `[--report-to @sid]`: 1 hunk, shown against round 15's read at 030ba1788), checked against parse_args (--receipts/--no-receipts set cfg.receipts and receipts_given), the serve arm (receipts_from_config only when neither flag was given), fabric.rs receipts_from_config (off when the key or the file is absent), and hook.rs parse's `--report-to` arm and claude_settings_for (written on the Stop command only); 2026-09-15 round-16 read (feat/round-16-crosshost) of the new broker usage and doc, the USAGE broker/fabric lines and the build line: broker()'s parse (--tcp/--key-file/--secret-file once each, --allow-remote, the positional log required with --tcp, surplus words refused), the default-build refusal before any other check (transport::SEALED_UNAVAILABLE), the always-guarded TCP rule, is_loopback_endpoint's refusal text, read_private_key_file/read_secret (0600, 32+ bytes) before the log is opened, serve_sealed, the `listening <bound addr>` line, and build_line!/SEALED_BUILD_MARK against enable.rs binary_has_sealed; the fabric synopsis against fabric.rs parse_args; no claim contradicted; 2026-09-15 read of the broker prose the round-16 review's twin added (the doc's two-listeners paragraph, USAGE's `[--unix <socket>]` synopsis, its entry and the second `listening` line, the `--unix needs --tcp` refusal, `--unix given twice`, and the mode line's `and the unix socket`) against broker(): --unix parsed once, refused without --tcp before the log is opened, served by broker.serve(sock) on the SAME Broker after serve_sealed succeeds (one log, one guard, two acceptors; a refused socket drops the TCP handle and removes a log this call created empty), `listening <tcp>` then `listening <sock>` printed only after both are bound, both handles kept for the process; and astream-broker serve_tcp_with's MAX_PREAUTH_CONNS = 64 / PREAUTH_TIMEOUT = 5 s, which gate the TCP accept path only (serve() has no pre-auth wrap); no claim contradicted; 2026-09-15 read of every prose line the round-16 branch changed since main, by the round-16 merge pass: the rendezvous-defaults paragraph against enable.rs fill_defaults (--tcp --key-file defaulted only beside a --broker defaulted from a rendezvous file that names key_file) and finish (key_file written only for Wire::Sealed with no bind, i.e. by join) and dispatch (ls/glance/tui through with_rendezvous_defaults, mirror through with_default_sock): one gap, the paragraph named `on` alone and not the sealed pair a joined host defaults, fixed before this row; the broker USAGE and doc against broker() (each flag once, --key-file/--allow-remote/--unix refused without --tcp, the SEALED refusal first, --secret-file required and the log positional required with --tcp, is_loopback_endpoint before the key is read, listening lines only after both binds) and build_line!; no other claim contradicted; 2026-09-17 re-read after rebasing onto main e6f88a94b (the bytes unchanged by the rebase): USAGE's broker lines and the rendezvous paragraph against enable.rs fill_defaults (--tcp --key-file added only when the rendezvous names key_file and no --broker was given) and broker()'s parsing (each flag once; --key-file, --allow-remote and --unix refused without --tcp; the SEALED refusal before any other word; --secret-file and the log required with --tcp; is_loopback_endpoint then read_private_key_file before the log is opened; read_secret's check_private and SECRET_LEN; the `listening` lines only after both binds) and build_line!; no claim contradicted; 2026-09-17 read again by the pass that folded feat/round-16-crosshost into its two commits: the gate run with the roster as it stood before this row's 2026-09-17 entry named exactly these five surfaces, and each `--diff` above was re-read against the handlers named, hook.rs's and this row's claims included; no claim contradicted",
    ),
    (
        "crates/aterm-link/src/fabric.rs",
        "7226dba630b52230",
        "2026-09-17",
        "written and read by feat/round-11-fabric-ledger on 2026-09-14 against its own handlers and the code it describes: USAGE and the module doc against parse_args (no argument is `status`; `--json` is status-only, `--bodies`/`--from` tail-only, each refused by name elsewhere), resolve_command/command_in_toml (ATERM_FABRIC_COMMAND first, then `[fabric] command` through aterm_toml, blank as absent — aterm-gui fabric_launch::configured_command's order), bridge_config over crate::cli::parse, probe_broker (connect with IO_TIMEOUT, `hello`, every cap attached, `Fetch max=0`; broker_pid from the process table, launchd_label by pid), gather's walk over aterm_ctl::local_instances with read_instance (`fabric status`, the child running `link serve`) and read_session (`meta`, `inbox 0 --peek --meta`, `inbox --peek --meta`, and `timeline` only when held — every read `--peek`, every inbox read `--meta`), last_records/traffic_of (TRAFFIC_ROWS = 10, metadata only; the text reaches stdout only through tail_line with `--bodies`, after the trust label), warnings() (each warning checked by every_condition_that_makes_connected_a_lie_or_loses_mail_is_a_warning; the shared-state-dir sentence against Bridge::on_inbox_record's not-hosted `undeliverable` and the broker's SubscribeGroup resuming every member from the one committed cursor; the `connected` sentence against the 2026-09-12 measurement FABRIC_PAGE records), status_main/exit_of (0, 1 with any warning, 2 from Off) and tail (exit 1 when the broker is unreachable or closes the stream); the live run on this machine matched the text: fleet local, the launchd broker's pid and label, instance 66439's supervised bridge, both sessions' inbox numbers, the bus records by offset; 2026-09-14 RE-READ of the three module-doc bullets the adversarial review's fixes moved (the BROKER bullet's head-query sentence, the WARNINGS bullet's node-gone sentence, and the fallback-source sentence) against the handlers they now describe: BrokerView::read_ok (reachable && attached && head.is_some()) as probe_broker leaves it — attach answered, `Fetch max=0` refused, so `view.attached` is true with no head — against warnings()'s second arm and render_text's `reachable` line, which now read the same predicate and print `yes, but the bus cannot be read: <the broker's own error>` and exit 1 (a_head_query_that_failed_is_not_a_broker_that_answered, and the end-to-end a_broker_that_refuses_the_read_is_not_reachable_yes against a broker that answers Hello and Attach and errors the Fetch); warnings()'s new node arm against join_sessions, which fills report.nodes from the roster rows whose owner is `node` (so it is empty whenever the head query failed and nothing is claimed about a bus nobody read) and against bridge.rs's will (`state=gone fabric=disconnected`) — a_node_the_bus_calls_gone_is_a_warning_while_its_instance_says_connected also pins that a REMOTE node's `gone` row is not this machine's warning; and the fallback sentence against resolve_command's Source::Instance arm, which learns the command from a running instance and cannot tell `aterm ctl fabric attach` from a launch with $ATERM_FABRIC_COMMAND set, so neither the doc nor the warning claims which it was; USAGE itself is unchanged, and the live run on this machine matched again: the same broker pid and label, and the node-gone warning now printed for n-1b631315bf5cae35 under instance 66439's `connected` bridge, exit 1; 2026-09-14 read again on the merge branch (the round rebased onto main at d0b30e46a): USAGE's six-section synopsis, its flag ownership and its exit paragraph against parse_args (no argument AND a leading flag are both `status`; `help`/`-h`/`--help` answer Cmd::Help; `--bodies`/`--from` under `status` and `--json` under `tail` are refused by name; any other word is a usage error), main/status_main/tail_main (exit 2 for a usage error and for every Off arm, exit_of's 0-or-1 straight off the warning list) and render_text, whose six section heads are the six USAGE names (CONFIG, BROKER, BRIDGES, SESSIONS, TRAFFIC, WARNINGS) and whose `reachable` line has the three arms read_ok/reachable/neither; TRAFFIC_ROWS = 10 behind `the last 10 bus records`; tail()'s Err on a broker that will not connect and on a closed subscription, both ExitCode::FAILURE; warnings()'s node arm reading r.nodes only for an instance whose own fabric is `connected`, so a remote node's row is still not this machine's warning; no claim contradicted; 2026-09-14 read of the prose added for `on|off|doctor` (USAGE's three synopsis lines and their entries, the module doc's grammar block and its rendezvous-file sentence, Source::Rendezvous's describe and token, Off::message naming `aterm fabric on`, the two warnings that name it, and the both-spellings doc naming `on` as the writer of the `aterm link serve` form) against parse_args (on's --dry-run/--service/--fleet/--tcp/--key-file and off's --dry-run/--service, each refused by name on the other verbs), main's three new arms, rendezvous_command in gather and tail_main (env, then `[fabric] command`, then the rendezvous file's command, then a running instance), and crates/aterm-link/src/enable.rs (on's steps in USAGE's order, Service::parse's three words, the --tcp/--key-file refusal text and its reason, off's kept identity and bus-log line, doctor's fix_for and its rendezvous check, Paths::resolve's ATERM_FABRIC_FLEET default), by feat/round-13-fabric-on; no claim contradicted; 2026-09-14 read of the round-13 prose (the module doc's BROKER, BRIDGES and WARNINGS bullets, InstanceView's reason / rtt_ms / link_age_ms docs, fabric_cell's doc, the stalled warning and the reworded connected-but-unanswered warning, the test docs) against read_instance (kv of reason / rtt_ms / link_age_ms off `fabric status`, a `-` reason dropped), fabric_cell's three arms, warnings()'s `stalled` arm and its `connected` arm's new wording, and enable.rs fix_for's `broker link is down` and `does not answer this report's own probe` arms with every_warning_shape_has_a_fix pinning both; no claim contradicted; 2026-09-14 read of the round-13 C prose (feat/round-13-fabric-on: the SESSIONS bullet of the module doc, USAGE's SESSIONS clause, the presence_in_toml and presence_from_config docs, SessionRow's bus_role/bus_title/detail/phase/context docs, bus_field, session_role and session_title, Report::presence, the CONFIG `presence` line and the two new test docs) against join_sessions (bus_field on both arms, `-` and empty read as absent), render_text (columns ROLE DETAIL PHASE CTX TITLE; the local instance's meta first for role and title, the bus row's for a remote session), render_json (detail, phase, context as a number off `<n>%`, config.presence) and gather (a `--presence` on the command wins, else presence_from_config, which is meta on a missing file, on a bad value with a word on stderr, and on an absent key); no claim contradicted; 2026-09-14 read of the `starting` warning (attached, no link report yet: a bridge older than the report shows connected at its first delivery and `post --wait` parks; a newer one reports within its dial) against aterm-gui fabric.rs bridge_attached (reason=starting, reported=false), link_evidence (a `deliver` or `outbox sent` from a never-reporting incarnation moves it to connected) and wait_is_futile (no refusal under starting), and against enable.rs fix_for's `has not reported its broker link` arm; no claim contradicted; 2026-09-15 read of round 15's deadline prose (shown by `xtask gate help-surfaces --diff`, matched at c1fc82257): the Overdue struct and the OVERDUE_SCAN_SPAN/overdue_work/overdue_of/OverdueScan docs against the fold (an ask/task with dl= whose t+dl is past, settled by an answer|report|ack carrying that re=, an `expired` reported and not settling, a note's dl= ignored, one page of bodies held at a time), the WARNINGS rule sentence and the two tails of the overdue warning against warnings(), and the overdue test fixtures against their assertions. The doc says settled by a reply `anywhere on the fleet's in lanes`, which is what the fold does — broader than the bridge's asker's-lane rule, so a reply delivered to another session hides the warning but not the `expired`; accurate as written, left for a follow-up. No claim contradicted; 2026-09-15 read of round 15's receipts prose: receipts_in_toml/receipts_from_config and their error strings against the code, and the report's `receipts on|off` line against the command's serve flags else the config file (the bridge's own precedence); no claim contradicted; 2026-09-15 round-16 read (feat/round-16-crosshost) of USAGE's on --tcp/--allow-remote, mint-for, --out, join and its flags, the seven-section status and the guarded-broker sentence, plus the module doc's NODES and guarded-read paragraphs, against parse_args (each flag on its own verb, refused by name elsewhere; mint-for's one positional), enable.rs wire_for_on/key_step/finish, join.rs mint_for/join (probe before any write, node refusal, install, local_broker_step, --service none touches no supervisor), probe_broker's head fallback on `unauthorized`, readable_faces/in_filters/last_records/overdue_work/tail_faces, render_nodes/node_where/live_sessions_on and BRIDGES keeping to local instances, and last_records' one shared window over every granted face (merged by offset, a record two faces match kept once); no claim contradicted; 2026-09-15 read of the prose the round-16 review moved (USAGE's --dry-run, --tcp/--key-file, mint-for, --out and join entries) against parse_args (join's --tcp a bare flag and --key-file required by join::check_inputs; on's --tcp <h:p>), join::mint_for (the SEALED refusal first, exit 2; the joined-root refusal when <root>/node.cap exists and minted_here is false; symlink_metadata refusing a non-regular --out; private_mode making an identical cap 0600 before the `(0600)` line) and join::join/probe_remote (the node's Last{/f/<F>/pub/<node>/node/presence} read after the probe, a live row on a root whose link-state/node is not the cap's node refused exit 2), and the dry-run caveat against astream-broker's stage_bind (the first attach of a bound rw grant appends one hidden /a/bind record, a repeat appends nothing); no claim contradicted; 2026-09-15 read of the twin's prose (BrokerView::serves_tcp, serves_socket's doc, the BROKER section's `serves` line and USAGE's `--tcp` entry: the port as well as the socket, this host's own bridges on the socket, the `wire` step, serves_tcp in the rendezvous) against status() (serves_tcp copied from the rendezvous file only when its broker is this command's, never probed), render_text's `serves` kv and render_json's `serves_tcp`, broker_pid's `--unix <sock>` match, and enable.rs wire_step/finish (the rendezvous's serves_tcp/serves_key_file on Wire::Sealed{bind: Some}); no claim contradicted; 2026-09-15 read of every prose line the round-16 branch changed since main, by the round-16 merge pass: USAGE's verbs and flags against parse_args (--fleet on and mint-for only, --dry-run/--service on/off/join, --tcp/--key-file on or join with join's --tcp bare, --allow-remote on, --broker/--cap-file/--node/--accept-from join only, --out mint-for only, one mint-for positional), the mint-for and join entries against join.rs mint_for (SEALED refusal, malformed and own id, the joined-root refusal through minted_here, the symlink refusal, private_mode on an identical cap) and join (a different node id refused before the probe, probe_remote before any write, the live-node refusal exit 2, install 0600, local_broker_step leaving --service none alone), and the module doc's NODES/guarded-read/serves prose against probe_broker (the whole fleet first, the first granted face on unauthorized), readable_faces, render_nodes, broker_pid's --unix match and broker_pid_tcp; no claim contradicted; 2026-09-17 read after rebasing onto main e6f88a94b: the status USAGE's SESSIONS clause against render_text's sessions table (the NODE column only when the fleet has more than one node, many_nodes — one slip fixed before this row: it said each session's node), the seven sections' order against render_text (CONFIG, BROKER, NODES, BRIDGES, SESSIONS, TRAFFIC, WARNINGS), and the mint-for/join entries and the module doc's NODES and guarded-read prose again against join.rs mint_for/join, render_nodes/node_where/live_sessions_on, readable_faces, probe_broker's refused arm and tail_faces; no other claim contradicted; 2026-09-17 read again by the pass that folded feat/round-16-crosshost into its two commits: the gate run with the roster as it stood before this row's 2026-09-17 entry named exactly these five surfaces, and each `--diff` above was re-read against the handlers named, hook.rs's and this row's claims included; no claim contradicted",
    ),
    (
        "crates/aterm-link/src/hook.rs",
        "b1fede1fde850b57",
        "2026-09-17",
        "re-read on 2026-09-12 against the code that moved under it since the roster was minted, by the 2026-09-12 drift sweep (lane aterm-link); the fleet-origin and human-words claims were falsified by `hold` becoming OwnerOnly and are fixed here; 2026-09-14 drift sweep (lane link, 26 claims checked over the group): re-read against the code that moved under it since this row's anchor commit; every candidate went to a verifier prompted to REFUTE it — no claim contradicted the moved code; 2026-09-14 round-12 read (the fabric-hooks incident fix, SPEC12): the module doc's two-verdicts rule, its finding-aterm section and the USAGE block (--check, --merge, --dry-run, --exe, the --sock default, the install paragraph and the exit-code paragraph) were written beside the handlers and re-read against them — run's EVENTS check and its --check gate, open/connect/resolve (aterm_ctl::resolve_sock_for for the session, then --token-file, $ATERM_CONTROL_TOKEN, aterm_ctl::read_token_beside), pre_tool_use's three exit-0 arms and its one exit-2 under hold=1, stop's open and listing arms, install_claude's self-test-before-print order (exit 2 and nothing written on a failed self-test, exit 1 with the block printed on an existing file without --merge, --dry-run writes nothing), merge_into's backup-then-write_atomic, command_form's basename rule and OWN_MARK's ` hook run `; every claim holds, and tests/hooks.rs's round-12 cases execute each one against a live headless instance; 2026-09-14 read of the prose the round-12 adversarial fix moved (shown by `xtask gate help-surfaces --diff`, matched at 6fe85460d; 8 hunks, 21 lines out, 112 in) against its handlers in the same file and in aterm-link ctl.rs and aterm-ctl: the USAGE --merge line's symlink-and-mode clause and the --exe line's absolute/$PATH clause against install_claude (link_target before the exists/merge/dry-run arms, absolute_exe before claude_settings_for and the self-test, every message carrying ` (a link to <target>)` when the path was a link), link_target (read_link by hand, 32 hops, a relative target joined to the link's parent, a dangling target kept), absolute_exe (an absolute path as given; a path with a `/` joined to current_dir with `.` components dropped and `..` kept; a bare name found on $PATH with an empty entry meaning the current directory; exit 2 through install_claude's `{why}; nothing was written` for a bare name off $PATH or an unreadable cwd), Merged/merged_document (stat's mode & 0o7777, read, UTF-8, parse, merge_hooks — nothing on disk touched), merge_into's order (merged_document, write_backup, write_atomic with Some(mode)), write_backup (create_with_mode on `.bak-<s>`, then `.bak-<s>-<n>` on AlreadyExists, the too-many-backups sentence), create_with_mode (create_new + mode, then set_permissions on the descriptor before a byte), write_atomic's optional mode, the dry-run arm (merged_document computed and printed, its refusal exit 2 with the same message the real run prints); the module doc's LANE_DEADLINE clause and connect's bounded-lane paragraph against Ctl::connect_within (read and write timeouts set on the stream BEFORE authenticate writes AUTH), guarded's TimedOut/WouldBlock mapping to the one `aterm did not answer within the lane's deadline` sentence with `lost` latched, LANE_DEADLINE = 3 s, stop's set_deadline(left + LANE_DEADLINE) around the one await and back to LANE_DEADLINE after; the doc's `2 s probes / 10 s forwarded verbs` against aterm-ctl's PROBE_DEADLINE and FORWARD_DEADLINE, and its `60 s per tool call, 30 s per prompt and 600 s per stop` against claude_settings_for's UserPromptSubmit timeout 30 and Stop timeout 600 with PreToolUse left at the vendor's 60 s default; ONE SLIP, FIXED IN CODE RATHER THAN PROSE: the header's rule that every failure of the hook's own prints its reason on stderr before exit 0 was false of stop's await arm, which returned without a word on an `ERR` reply or a lane error (the deadline firing included) — it now prints `await answered <reason>` or `await: <error>` and carries on, and tests/hooks.rs's a_stop_whose_await_is_never_answered_names_the_deadline_and_carries_on holds an aterm that answers the listing and never the await; every other claim holds; 2026-09-15 read of the round-14 D3 additions — the module doc's `The report` section, USAGE's --report-to and --check lines, and the doc comments of recipient_check, report, last_assistant_text_within, trim_report, report_key, Posted and newest_unhandled_task — against the code beside them: stop() (the report goes BEFORE the wait, the loop breaker is honoured right after it, and without the flag the early return is where it was, before a connection is opened), check()/recipient_check (`@<sid> status` on the same connection, ` report-to=<sid>` appended to the ok line, `not ok …: --report-to @<sid>: <reason>` otherwise), parse's --report-to arm (one optional `@`, an `s-` prefix and subject::is_principal, else a usage error), claude_settings_for's stop_tail (the flag on the Stop command only, through sh_word), transcript_path (Json::parse, the top-level key), last_assistant_text_within (a seek to len - TRANSCRIPT_TAIL_MAX, the partial first line dropped, every candidate line parsed with `type` == assistant checked on the document and isSidechain skipped, Str and Array `message.content`, the text blocks joined), trim_report (REPORT_MAX with the marker inside the bound, cut on a char boundary), report_key (bridge::fnv1a_64 over uuid, NUL, body), Posted (<state>/report/<sid>, tmp + rename, best effort both ways), newest_unhandled_task (kind=task and id > the header's seen=, the highest id's off), and report's `@<sid> post to=@<to> kind=report[ re=<off>] len=<n>` through Ctl::request_with_body with Posted::record and Ledger::charge only after an OK; no claim contradicted; pinned by hook::tests' nine round-14 tests and tests/hooks.rs's five round-14 e2e tests; 2026-09-15 read of the --report-to and --check usage entries as the report-hook-safety review changed them — a regular file or nothing (metadata.is_file before the open, the read capped by take(tail_max)), re= the newest task unhandled or newer than the last report (report_task over listing's rows with Posted's high= line), the recipient's status before the post, the post's --wait=1500 and Landing's landed/queued/dead/refused outcomes with their charge/key/stderr, --check's fabric_cannot_carry on `fabric status` state=absent supervised=0 — against report, report_task, Posted::parse/record, Landing::of, recipient_check and fabric_cannot_carry, by feat/round-14-mail on 2026-09-15; no claim contradicted; pinned by hook::tests' a_transcript_that_is_not_a_regular_file_is_refused_at_once, a_landing_is_read_from_the_posts_reply, check_refuses_an_instance_with_no_bridge_and_none_coming and tests/hooks.rs's a_report_answers_the_task_the_worker_just_marked_handled, a_recipient_that_vanished_after_install_is_named_not_silently_charged, no_broker_the_stop_hook_says_the_report_is_queued, a_fifo_transcript_does_not_park_the_stop_hook; 2026-09-14 (2026-09-15 UTC) read of the prose --diff shows moved since main's read: the module doc's `The report` section, USAGE's --report-to and --check entries, the docs of recipient_check, fabric_cannot_carry, stop, Row::id/off, TRANSCRIPT_TAIL_MAX, REPORT_MAX, LastMessage, transcript_path, last_assistant_text, last_assistant_text_within, trim_report, report_key, Posted, LastReport, report_task, REPORT_WAIT_MS, Landing, report, claude_settings_for's stop_tail comment, parse's --report-to arm and the round-14 tests' docs — against the code beside each (stop(): the report before the wait and the loop breaker honoured right after it; report(): transcript, key and dedup before any request, ledger.spent, the recipient's `status`, report_task over listing's rows with the header's seen= and Posted's high=, `post … --wait=1500`, Posted::record and Ledger::charge per Landing arm; last_assistant_text_within: metadata.is_file before the open, a seek to len - tail_max, take(tail_max), the torn first line dropped, every candidate parsed, isSidechain skipped; recipient_check: `@<to> status` then `fabric status` through fabric_cannot_carry; claude_settings_for: --accept-from on every command, --report-to on Stop's tail) and bridge.rs classify_kind/accepted (the node-level --accept-from that keeps a task undemoted) and fnv1a_64; three claims corrected in this commit: the module doc's `re=` was the newest UNHANDLED task where report_task also answers one newer than the last report, its `charged to the wake budget like a wake` holds only once the post landed or was queued (a dead outbox and a refusal charge nothing), and its and USAGE's `exit 0` for a failed report is now `no exit code of its own` (the wait that follows decides it: a wake there is exit 2); no other claim contradicted; 2026-09-15 round-16 addendum read (the report posts what the worker DISPLAYED): the module doc's report paragraph, the USAGE block's --report-to entry and the docs of LastMessage, last_assistant_message, Turn, Stamp, final_message_within, scan_turn, is_prompt, displayed, vendor_text, NARRATION, is_narration, signature_kind, proto_field, varint, base64_decode, Pace, CATCH_UP, Settled, settle and displayed_message were written beside those handlers and re-read against them — scan_turn stops at the last user line that is neither a tool_result nor isMeta, walks past messages with nothing displayed, takes every line of the first message (by message.id) with anything in it, joins its text and narration blocks with a newline and keys on the uuid of its last contributing line, and never takes a thinking block with text whose signature is not field 2 > 1 > 8 == narration (undisplayed then, and nothing older); settle returns at once when the turn's last assistant line reads as the vendor's last_assistant_message, else after 300 ms without a size/mtime change, bounded at 2 s (CATCH_UP.max, the USAGE's 'at most 2 s'); displayed_message posts the vendor's text with a stderr note and no uuid when the transcript did not catch up, is missing or unreadable, and says why with nothing posted when there is neither; the 2.1.268 facts the prose cites (FLUSH_INTERVAL_MS=100, the Stop input's last_assistant_message as the last assistant message's text blocks joined with '.') were read in the vendor binary; hook.rs's seven new unit tests and tests/hooks.rs's the_report_posts_what_the_worker_displayed_and_never_its_hidden_reasoning execute each claim, the latter against a live headless instance; 2026-09-15 read of the prose the round-16 review moved (the module doc's report paragraph, LastMessage/Turn and their anchor, last_assistant_message's binary quote, scan_turn, STOP_FEEDBACK, is_wake, vendor_text, report_key, already_reported, the Posted/LastReport docs and the four new test docs) against scan_turn (a prompt or an isMeta string starting `Stop hook feedback:` ends the walk and is the anchor; after the message is whole only user lines that are not tool results are parsed), vendor_text's `\\n` join, displayed_message's fallback keeping settled.turn.anchor, report()'s already_reported check and its fallback flag, and against the 2.1.268 binary (`U=F?Pr(F.message.content,`\\n`).trim()||void 0`, `Og` = findLast assistant, `Pr` = filter text, map, join; a blocking Stop hook's `Ce({content:eZe(blockingError),isMeta:!0})`, `IIt(e,n)` = `${e} hook feedback:\\n${n}`) and the live worker's transcript read on structure only (55 feedback lines, each followed by an attachment and stop_hook_summary, after the final assistant line in 51, a permission-mode line in 4; other isMeta lines mid-turn: images after a tool result, local-command output); two slips fixed before this row (the feedback line's neighbours were first stated from 3 samples, and isMeta's other uses as an attachment's text); 2026-09-15 read of every prose line the round-16 branch changed since the recorded read, by the round-16 merge pass: the --report-to USAGE entry and the module doc against displayed_message (the vendor text posted, with a note and no uuid, only when it exists and the file did not catch up or could not be read) and settle with CATCH_UP (2 s max, 300 ms quiet, 20 ms poll), scan_turn/is_prompt/is_wake/displayed (the walk ends at a prompt or a Stop-feedback line, one message by message.id, text and narration kept, other thinking with text only marks undisplayed), vendor_text (newline join, trimmed), report_key (anchor, uuid and body) and already_reported's fallback arm with Posted's fallback=1 line; the vendor-binary quotes and live-transcript counts were not re-measured in this pass; no claim contradicted; 2026-09-17 re-read after rebasing onto main e6f88a94b (the bytes unchanged by the rebase): the module doc and the scan/settle/report docs against scan_turn (the walk ends at a prompt — is_prompt: not isMeta, a string or an array without a tool_result — or at a wake — is_wake: isMeta and a string content starting with STOP_FEEDBACK; one message by message.id; displayed() keeps non-empty text and narration-marked thinking, a thinking block with text and no mark only sets undisplayed), is_narration/signature_kind (base64, then field 2 → field 1 → field 8 == narration), settle (caught up when tail_text equals the vendor's text, else when the file held still for Pace::quiet; re-read only when size or mtime moved; CATCH_UP max 2 s, quiet 300 ms, poll 20 ms), displayed_message (the vendor's text as the fallback with a note and no uuid when the file did not catch up or could not be read; say and nothing for no transcript_path without vendor text, a turn that displayed nothing, an undisplayed last message), report_key (anchor, uuid, body) and already_reported's fallback arm; no claim contradicted",
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
        "34a9e61154b2bc07",
        "2026-09-18",
        "re-read on 2026-09-13; FABRIC_NOTE and its doc carried the same false `post refuses` claim as the assets and are fixed in that commit; 2026-09-13 read of the prose 9758cd022 moved since that read (shown by `xtask gate help-surfaces --diff`, matched at 2493a60a7) — FABRIC_NOTE's rewritten `fabric=absent` sentence and the whole-block budget test's re-measured doc comment — against aterm-gui fabric.rs cmd_post (the row pushed into `posts` before any wait and `OK <id>` returned when `wait` is None, `--wait` ON by default only for `ask`/`task` via `matches!(kind, ask|task)`, and the option tokens to/kind/re/dl/via/len/--wait, none of them a caller-supplied dedup key, so the no-idempotency-key clause holds), fabric_wait_refusal/bridge_reachable/fabric_state, the wait loop's `ERR timeout id={id}` return, and Inbox::trim_retired_posts (evicts only rows with `off=` or `dead`, so a timed-out post is still queued), cross-checked against control_verbs.rs's `post` row and manual.rs's fabric page (all three not-landed answers mean queued; `aterm link mirror` is the file mirror the note points at); the budget doc's figures were MEASURED by building `primer_block` out of tree rather than recalled — codex 4354 and the other three 3564 against the test's `widest <= 4_400`, and 4297/3507 before the change — which confirms 4354/3564 but contradicts the doc's Fifty-four bytes on every agent's every turn: 54 is the overshoot past the retired 4_300 cap, while the recurring price is 57 (FABRIC_NOTE 1119 -> 1176 bytes, every agent's block +57), and the earlier raise from a guessed 4200 appears nowhere in this file's history (the cap was born 4_300 at 273a1d56d), both left for the orchestrator; the agent-facing sentence itself is sound except that `fabric=absent` alone does not imply `no-bridge=1` (fabric_wait_refusal answers `queued=1` while a supervisor is armed and the first bridge is still attaching) and an explicit `--wait` takes the same refusal on any kind, not only `ask`/`task` — both narrowings inherited from the sentence it replaced THE CONTRADICTION THAT READ FOUND WAS FIXED IN THIS COMMIT: the budget test's doc priced the addition at `Fifty-four bytes on every agent's every turn`, and the measured per-turn price is FIFTY-SEVEN (FABRIC_NOTE 1119 -> 1176, codex 4297 -> 4354, the other three 3507 -> 3564); 54 is the new widest block's overshoot past the RETIRED 4300 cap, a different quantity. The doc now states the measurement and says which slip it was. The unverifiable clause about `a guessed 4200` (no such cap literal exists in this file's history — the test was born at 4_300 and has been raised once, to 4_400) was removed with it. The hash recorded here is the CORRECTED doc; 2026-09-14 drift sweep (lane primer, 46 claims checked over the group): re-read against the code that moved under it since this row's anchor commit; every candidate went to a verifier prompted to REFUTE it — no claim contradicted the moved code; 2026-09-14 audit fixes read against the code beside them: FABRIC_NOTE's doc comment no longer claims a `fabric=` gate sentence the paragraph never had (PRIMER_BODY's aterm-detection gate is the only one it needs) and names `ERR timeout id=<n>` as the third queued outcome (read against fabric.rs cmd_post's wait deadline arm); the fabric doc is ONE body (assets/aterm-fabric-body.md) under four per-agent headers — Claude's frontmatter, Codex's none, OpenCode's description-only, Gemini's TOML — pinned by every_agent_gets_the_body_under_its_own_header; join_with_xdg's doc says a `.config/` row follows $XDG_CONFIG_HOME as OpenCode does (read against home_join's call sites and display_path); no claim contradicted; 2026-09-15 read of FABRIC_NOTE's `never re-post, unless under the same key=` against aterm-gui fabric.rs cmd_post (key= accepted on every kind and carried on the queued row) and aterm-link bridge.rs drain_outbox (a re-post under a key reuses the sequence the key reserved, the broker dedups it and the post answers the original offset with dup=1), and of the whole-block budget doc's round-15 note, whose sizes were DERIVED: MEASURED here with a throwaway test over primer_block — codex 4352, the other three 3562, FABRIC_NOTE 1174 — matching the derivation; the note now says measured. No claim contradicted; 2026-09-15 updater audit integration: read the changed path-context docs against auto_prime, status_line, agents_report and their with_xdg implementations. Each public boundary captures XDG once; detection, file operations and display use that same value. Scratch tests pass both roots explicitly, including relocated OpenCode install, idempotence, status and removal with an untouched inactive-default sentinel. Agent-report status/install/remove and skill rows now use display_path too; exact relocated paths and the original default column formatting are regression-tested. Primer/skill assets are unchanged.; 2026-09-15 merge read of feat/round-15-receipts over main: the merged prose is exactly round 15's change (FABRIC_NOTE's `never re-post, unless under the same key=` and the budget doc's round-15 note: 2 hunks, shown by `xtask gate help-surfaces --diff` against main's read at 02285ecc6) plus upstream's change (the status, install and remove report lines printed through display_path as `{:<9} {:<30}` where they were `{:<9} ~/{:<28}`, the status test's new pin that every agent's default `~/{:<28}` spelling and padding stay byte-identical, agents_report's XDG_CONFIG_HOME doc, and the relocated-OpenCode test strings: 5 hunks, shown against round 15's read at 478963013), checked against display_path (`~/<rel>` unless $XDG_CONFIG_HOME moved a .config/ entry, so the default spelling and its 30-column padding are byte-identical), agents_report/agents_report_with_xdg, and primer_block, which upstream did not touch — the budget doc's measured sizes (codex 4352, the other three 3562, the cap 4400) stand, as the crate's own block-size test re-measures; 2026-09-17 read of the AGENT_FILES `var` column (CLAUDE_CONFIG_DIR, CODEX_HOME; gemini/opencode None until measured) and the AgentHome/agent_homes docs against agent_identity::env and the 79ce005c3 tests; 2026-09-17 read of the auto_prime_identity prose and the two test docs (--diff matched at 49aefbcc7) against auto_prime_identity's auto_prime_with_xdg(dir, None) beside auto_prime's xdg_config_home() read and join_with_xdg's `.config/` redirection; no claim contradicted re-read on 2026-09-18 for the first-launch disclosure: `AUTO_PRIME_OFF_SWITCH`, `AgentOutcome::wrote`, `AutoPrime::changed`, `auto_prime`'s summary paragraph and its four new tests were read against `auto_prime_with_xdg`'s write-result fold (`PrimerWrite::Created|Appended|Replaced`, `SkillWrite::Installed|Updated`), `AUTO_PRIME_NOTE` (which carries the switch verbatim, printed by the `status` and `remove` arms), `spawn.rs`'s `run_agent_prime` (which logs the summary only when `changed()`), and `MAX_RECORD_BYTES`. No claim contradicted.",
    ),
    (
        "crates/aterm-release/src/cli.rs",
        "c405eb2018470f62",
        "2026-09-18",
        "read against its parser/dispatch by the 2026-09-10 sweep; findings fixed in 412cf3acd; 2026-09-13 leftovers lane (8035c0c56, shown by `xtask gate help-surfaces --diff`: 4 hunks, 1 line removed, 17 added): the provision usage's `[--cert-dir <folder>]` and its paragraph (the request is copied there for the upload dialog, the .cer looked for there and in ~/.aterm/apple; without it every read stays inside ~/.aterm/apple; naming one is the consent and a note says so before the errand) read against apple.rs Watch::new/dirs (named first, then ~/.aterm/apple), find_matching_cert (reads only Watch::dirs), surface_csr (called only with the named folder, await_then_install's `.and_then(|named| surface_csr(…))`) and Watch::consent_note (printed before errand_lines when the named folder is under a protected root); the four parse refusals (`--cert-dir given twice`, `needs a folder`, `not an empty string`, the flag itself) read against parse's \"--cert-dir\" arm; the Cmd::Provision cert_dir field doc read against the same; no claim contradicted; AND 2026-09-18 re-read of the `ship provision` help's audited-items sentence (the front door is the store's targo + trustc answering --version, rustup's link reported informationally) against provision.rs front_door_check/front_door_verdict and rustup_note, by the rust-free host-lane round; no slip found",
    ),
    (
        "crates/aterm-types/src/control_verbs.rs",
        "60c0695d16ac8aef",
        "2026-09-17",
        "catalog read in full against the handlers by aterm-help-surfaces-read on 2026-09-10; the fabric row and the reworded hold row read against dispatch_fabric_verb / dispatch_hold_verb and the access-set pins by verify:F1 and verify:F2 of aterm-fabric-attach-round-3 on 2026-09-10; goldens regenerated as a pair from the merged catalog on 2026-09-11; merged with origin/main's own 2026-09-11 re-read of this file, conflicts resolved by the orchestrator (upstream wording kept where both sides fixed the same claim). 2026-09-12: the ONE prose change since that read is SHORT_CATALOG_MAX_BYTES' raise paragraph, read in full and MEASURED rather than recalled — aterm-gui's bare cmd_help is 9 610 B against the new 9 728 ceiling, VERBS is 101 rows, the summary rows alone are 9 238 B; the fabric, fx and hold rows and the OpClass/Access/Framing/Target docs re-read against dispatch_fabric_verb, cmd_fabric/cmd_fabric_attach/fabric_status_line, dispatch_hold_verb/fabric::cmd_hold and the access-set pin, the other 98 catalog rows sampled. One contradicted claim fixed in the commit that updated this row — Access::OwnerOnly called `hold` the ONE member whose handler tells the two owner-class scopes apart, and `fabric`'s handler is a second (Scope::Owner exactly, the bridge refused), as aterm-gui's own is_owner_class doc already records; 2026-09-13 (UTC; 2026-09-12 local) re-read of the trail row's paste-sweep additions (the licence= classes, program-row, the inserts_* status keys, which inputs stamp an insert and which stay dark, and the `inserts_delivered>0 inserts_lit=0` reading after a drop) against aterm-effects cursor_glow.rs AdmissionRecord::line/LICENCE_*, move_licensed, insert_echo/insert_rewrite/INSERT_HINT_FRESH, note_insert_delivered/lay_insert/retract_insert/insert_tally and TrailStatus::line, app_render.rs tick_cursor_fx's Rainbow-Kitty-gated delivery prelude, app_input.rs input_paste and insert_gesture_armed, lib.rs drop_file/deliver_paste, and control.rs's flagless, front-routed, background, guarded and run_feed_bin_routed paste-bin routes, fixing the contradictions in the row (paste-bin into the tab on screen stamps an insert, not nothing; the insert class and counters are Rainbow Kitty only; `A move paints only if a keypress LICENSED it` now admits the delivered insert; the key class gains the composer newline and ⌃V's gesture; inserts_delivered counts a bare Tab and ⌃V too; last_insert_cells is the 32-cell bound when unpriced; the drop reading covers an echo never seen inside the 2 s window) and regenerating both goldens as a pair; the 0.84 train's release-candidate merge (this machine's provenance/repair/TCC work over the peers' 2026-09-12 re-read at the tip) moved the bytes once more — both halves were read by their authors as recorded, the merged file is their union, and this row records the union's hash; 2026-09-13 read of the prose ca54aaadd moved since the recorded read (shown by `xtask gate help-surfaces --diff`, matched at 0f7667e8a) — the trail row's `trail status` key list gaining `park_returns= park_flushed=` and its new HELD PARKS sentence — against aterm-effects cursor_glow.rs TrailStatus::line (the two keys last, after swallowed_no_echo=), InFlightTally's park_returns/park_flushed, HeldPark, park_candidate (Rainbow Kitty, same-row backward, a fresh stamp or presses in flight), spawn's RETURN arm (a forward move from the landing past the origin within TYPE_HINT_FRESH = 0.25 s, park_return_paid, judged origin -> target) and flush_park, and the flushes at the stale-park tick, note_backspace, note_scroll and the next other move; no claim contradicted; 2026-09-13 read of the prose feat/round-7-offscreen moved (shown by `xtask gate help-surfaces --diff`, matched at ca54aaadd) — the new `offscreen` row (Read, Lines, Session; the summary, and the detail sentence by sentence) and the `history` row's arch= clause — against aterm-gui control_query.rs offscreen_args/cmd_offscreen/format_offscreen_reply/OFFSCREEN_DEFAULT_MAX, aterm-core alt_archive.rs (ALT_ARCHIVE_DEFAULT_BUDGET, env_opted_out's 0/off/false/no, AltArchive::read and wipe, the gap kinds, the ESU commit and the 16 ms epilogue fallback), control.rs json_unsupported and control_session.rs cmd_history's arch= field; two slips fixed before this row (a committed frame was only `a DEC 2026 close`, where an app that never sends one is committed at the batch epilogue at most once per 16 ms; lost= counted evicted rows only, where a reset or turning the archive off wipes rows into it too) and both goldens regenerated as a pair; 2026-09-13 merge of main into feat/round-7-offscreen: main's drift-sweep re-read on 2026-09-13 (lane small-b) found the trail licence= row saying an unpaid press logs `key` and fixed it; `gate help-surfaces --diff` at the merge shows the merged prose is exactly round 7's read offscreen row and history arch= clause plus main's trail licence=/decline-reasons correction and its rainbow-kitty v2_ sentence (added 2026-09-12, before main's recorded read); recorded at the merge; 2026-09-14 read of the prose round 10 moved (shown by `xtask gate help-surfaces --diff`, matched at bd8f70ca9) — the offscreen row's `in memory` (no longer `only`), its self-update sentence (the origin kept; the rows after the last 8 submitted turns' marks and at least the running app's last 8 screens, the newest without a turn, up to 1 MiB with older ones in lost=; a new origin when not carried; the resize breaks= gap) and breaks='s scroll-back clause, and the history row's carried=1 sentence — against aterm-gui handoff_carry.rs export/tail_from/TAIL_TURNS/CARRY_ARCHIVE_BYTES/encode_within/decode, aterm-core alt_archive.rs carry_head, carry_reach (REANCHOR_SCREENS screens, never below the app run's floor, and the re-shown run), carry_rows, import and shift_down's Jump gap on a scroll-back past the oldest retained row, control_session.rs cmd_history (` carried=1` before text=) and raise_turn_ids, turn_ledger.rs TurnRecord::carried, and seamless.rs take_incoming's next_turn_id raise; one slip fixed before this row (the carry was said to reach at least the last 8 screens, where carry_reach stops at the running app's floor — now the running app's last 8 screens) and both goldens regenerated as a pair; 2026-09-13 release-candidate merge of the leftovers lane over the peers' round-7/round-8 reads: the merged prose is the UNION of the two halves — main's `offscreen` row and `history` arch= clause (read by round 7's author, recorded above) and the leftovers lane's `momentum_glow=` key and the absorbed rainbow-kitty v2_ sentence (read by that lane, recorded next) — each half read in full by its author against the code it describes, neither contradicted by the other, and this row records the UNION's hash: re-read on 2026-09-13 by drift sweep of 2026-09-13 (lane small-b); the trail licence= roster said an unpaid press logs `key` and is fixed in that commit; that read recorded 212628b6b90c5780 (834cd5f9f), and main 2f15705bc was ALREADY red on this row: the wrapped-row band merge (169dafa7b) had added a sentence to the `trail status` row that nobody read — `While rainbow kitty owns the frame the row ends with v2_quads= v2_halos= v2_stars= v2_meteors= v2_bridged= ribbon_retired=` and its glosses; 2026-09-13 leftovers lane: TWO prose changes since that read, both read here: (1) the `momentum_glow=` key in the backticked run and its sentence (MomentumGlow::value beside the cat's `momentum=`), against TrailStatus::line in cursor_glow.rs and App::trail_status in app_render.rs (`ws.momentum_glow.value(now, MOMENTUM_GLOW_TAU_S)`), pinned by control.rs trail_status_help_enumerates_exactly_the_keys_the_row_emits; (2) the absorbed v2 sentence, against TrailStatus::line_v2 in cursor_glow.rs (appends exactly ` v2_quads= v2_halos= v2_stars= v2_meteors= v2_bridged= ribbon_retired=` from rk::Status, and only when CursorGlow::v2_status is Some — `self.v2.engaged()`, so only while rainbow kitty owns the frame, at the tail), rainbow_kitty/mod.rs Status (quads/halos written this frame, stars and meteors live; `bridged` = cells the echo ledger relit for a late echo the admission ring scored declined; `retired` = ribbon cells retired by CONTENT over the engine's life — Engine::witness_rows when the glyph under a cell changed or went, Engine::retire_row when the caret was seen on another row through a declined move — cumulative across Engine::reset) and app_render.rs (per-window `ws.cursor_glow`, so `the window's cumulative count`); no claim contradicted; 2026-09-13 (2026-09-14 UTC) read of the prose 9758cd022 moved since the recorded read (shown by `xtask gate help-surfaces --diff`, matched at 0333f4a441d2) — the `post` row's rewritten `no-bridge=1` paragraph (NARROWER rather than the opposite, not a verdict on the message, `fabric attach <command...>` arms a supervisor and that same outbox drains, further posts only until `ERR outbox full`) and its new THIRD OUTCOME sentence (`ERR timeout id=<n>`, the `--wait` expiring with no landing reported, queued exactly like the other two), plus the two `contains` assertions in the_fabric_rows_state_the_bounds_they_are_held_to that pin `none is coming ON ITS OWN` and `ERR timeout id=<n>` — against aterm-gui fabric.rs cmd_post (its option tokens are to/kind/re/dl/via/--wait/len only, so there is no idempotency key that could collapse a re-post; the refusal at the door answers `ERR outbox full queued= bytes=` at OUTBOX_CAP = 128 or OUTBOX_BYTES_MAX = 4 MiB measured by queued_load + caller_sized_bytes; the PostRow is pushed BEFORE the wait loop), that loop's deadline arm returning `ERR timeout id={id}` with the row untouched (only trim_retired_posts removes rows and only ones carrying `off` or `dead`, so a timed-out post is still queued and still drained), fabric_wait_refusal and bridge_reachable (`no-bridge=1` is exactly `!supervised && state == absent`), note_bridge_supervised, fabric_launch.rs spawn_supervisor/arm/preflight and control.rs cmd_fabric_attach (`fabric attach <command...>` is the one seam that starts a supervise thread at RUNTIME and flips the latch), and aterm-link bridge.rs drain_outbox, whose bare `outbox` peek takes exactly the `off.is_none() && !dead` rows and runs on the reconcile path a freshly attached bridge takes; no claim contradicted — the retired `Nothing will publish it, no answer can arrive` WAS false, and control.rs fabric_attach_arms_the_supervisor_of_a_running_instance_once_and_for_owner_only shows the flip (`no-bridge=1` at id=1 and id=2, `queued=1` at id=3 after the attach, the two earlier posts still queued); one narrowness left standing rather than fixed here: the row gives an instance with no `[fabric] command` as the cause, where the predicate is bridge_reachable, so a CONFIGURED command whose program fails arm's preflight answers `no-bridge=1` too (that same test's `/nonexistent/aterm-link`, id=2), and the remedy the row prints is the one spawn_supervisor's own warn line names for that case as well AND the same completeness slip the manual.rs read found was fixed here in this commit: the row enumerated three outcomes and `cmd_post` has a fourth — `ERR <reason> id=<n>` for a post the bridge retired (`unroutable`/`ambiguous`/`undeliverable`), which `outbox` then omits, so it is the one outcome that does NOT mean queued; the row now says so and `the_fabric_rows_state_the_...` pins `ERR <reason> id=<n>` and `unroutable` beside the other three. The hash recorded here is the CORRECTED row, and both goldens were regenerated as a pair; 2026-09-14 drift sweep (lane cli-and-types, 25 claims checked over the group): re-read against the code that moved under it since this row's anchor commit; every candidate went to a verifier prompted to REFUTE it — no claim contradicted the moved code The 2026-09-14 merge of the audit2 lane over the cli-and-types drift sweep moved the bytes once more; `--diff` shows exactly two prose lines, both the audit lane's own and read here against the code: the `no-fresh-hint` gloss's Rainbow Kitty clause (seam_licensed gates a fresh typed stamp on typed_credits_within >= 1 under GlowStyle::RainbowKitty, logged DECLINE_NO_FRESH_HINT; pinned by a_caret_that_advances_with_no_unpaid_press_still_buys_nothing), and the `inflight_forgotten=` edge list (forget_typed_credits at note_kill, at the keyless backward/cross-row refusal, and at the licensed move when the licence is not Typed, the hop is cross-row and unhinted, credit_starved or typed_over_cap — a same-row `no-fresh-hint` refusal is none of those and keeps the pool; a glyph's echo with Enter fresh takes the Typed licence, pinned by a_glyph_echo_does_not_spend_the_enter_pressed_behind_it); no claim contradicted; 2026-09-14 merge read of feat/round-10-carry over main: the merged prose is exactly round 10's offscreen and history rows (`in memory`, the self-update carry sentence, breaks='s scroll-back clause, carried=1) plus main's reads recorded above (the trail rows' paid-press no-fresh-hint gloss, the `momentum_glow=` key and sentence and the inflight_forgotten edge list; the post row's NARROWER no-bridge=1, its THIRD and FOURTH outcomes and their test needles) — `xtask gate help-surfaces --diff` against main's read (matched at d68552079) shows round 10's two hunks and against round 10's read (matched at a4a023d4a) main's three, nothing else — checked against the merged handoff_carry.rs (CARRY_ARCHIVE_BYTES, TAIL_TURNS), alt_archive.rs (REANCHOR_SCREENS, carry_reach, import) and control_session.rs cmd_history, untouched by main, and against cursor_glow.rs seam_licensed (a Rainbow Kitty typed stamp licenses only while the press ring owes a cell), TrailStatus::line (momentum_glow= after momentum_display=), forget_typed_credits' edges and fabric.rs cmd_post's four wait outcomes, untouched by round 10; no claim contradicted; both goldens regenerated as a pair at the merge and unchanged 2026-09-14 (merge of the new-line fade round): the ONE prose change is the `ribbon_retired=` sentence, which now says the count includes cells RELEASED to the swoosh when their text went, as well as cells retired on the fast melt when it was replaced — read against Engine::witness_rows and Status::retired (rainbow_kitty/mod.rs), which count both on the same tally.; 2026-09-14 read of the round-13 additions (the `status` row's fabric=<connected|stalled|disconnected|absent>, fabric_rtt_ms= and fabric_link_age_ms= sentences, the `fabric` row's state/reason/rtt_ms/link_age_ms reply shape, and the new bridge-only `link` row) against aterm-gui fabric.rs fabric_state, fabric_link_facts, fabric_status_tail, bridge_attached (an attach stores STALLED with reason=starting and no numbers), link_report (accepted only for the owning generation, dropped once DISCONNECTED, CONNECTED/STALLED stored under the generation lock, every registered session woken on the up-to-down transition) and cmd_link (the grammar, `OK stale=1` for a ghost lane), control.rs fabric_status_line and dispatch_bridge_verb's `link` arm, session_status.rs's record tail, and aterm-link bridge.rs LinkReport::ack_wants_report / down_wants_report (change, more-than-2x past LINK_MOVE_GAP = 250 ms, or LINK_REFRESH = 2 s), link_reason's tokens, ACK_DEADLINE = 5 s and RECONNECT_MIN = 100 ms / RECONNECT_MAX = 5 s; goldens regenerated as a pair; SHORT_CATALOG_MAX_BYTES raised to 9856 for the row with the same accounting as the three raises before it; no claim contradicted; 2026-09-15 read of round 15's key and deadline catalog prose (shown by `xtask gate help-surfaces --diff`, matched at 135cf7646): `post`'s key= (aterm-gui fabric.rs valid_key; aterm-link state.rs key_seq/set_key_seq and KEYS_KEEP; bridge.rs drain_outbox's reserved sequence, and `dup=1` only for a key-chosen deduped publish) and dl= (note_deadline, settle_deadline_for, settled_on_bus, expire_deadlines); `outbox`'s `[key=<token>]` and `outbox sent … dup=1` against cmd_outbox/cmd_outbox_sent/retire_post; and the needle table's new rows against the rows they pin. THREE defects FIXED here: the `inbox` post-row grammar omitted the `key=` cmd_inbox prints on it; `post`'s futile-wait answer named `<absent|disconnected>` although fabric_wait_refusal answers `stalled` too; and `late=1` was unqualified although the bridge's `expired` map is in memory only, so a relaunched bridge delivers a late reply unflagged. Both help goldens regenerated as a pair. No other claim contradicted; 2026-09-15 read of round 15's receipts and fetch-by-offset catalog prose: `await inbox re=` against aterm-gui fabric.rs cmd_await_inbox (re= accepts every kind, since= defaults to 0); `inbox`'s oldest_on_bus header and verdict= against cmd_inbox/render_row; `inbox get @<off>` against inbox_get_at, deliver_fetched and Fetched::render (the ring answers only a whole row, FETCH_SLOTS 8, FETCH_WAIT_MS 10 s, `ERR fabric <state> off=` at once, chunked answers up to 256 KiB, one `ERR no such record`); `inbox seen`'s receipt against cmd_inbox_seen/owe_receipt and aterm-link bridge.rs send_receipt/retire_receipt; `post`'s --wait-ack bound (explicit ms, else dl= plus EXPIRY_GRACE_MS, else 30 s); `deliver`'s fetched= and receipt= forms against deliver_fetched/deliver_receipt and fetched_lines; `outbox`'s receipt and fetch lines against cmd_outbox; and SHORT_CATALOG_MAX_BYTES' round-15 raise, re-measured by printing cmd_help(\"\")'s length in help_short_form_is_bounded_and_is_the_summary_catalog: 9 863 B under 9 984. ONE defect FIXED here: a line continuation rendered `deliver`'s `verdict=<handled| refused|deferred>` with a stray space. Both help goldens regenerated as a pair. No other claim contradicted; AND THE v0.86 CANDIDATE'S READS THAT MAIN'S ROW CARRIED AHEAD OF THE READ ABOVE, kept rather than dropped: catalog read in full against the handlers by aterm-help-surfaces-read on 2026-09-10; the fabric row and the reworded hold row read against dispatch_fabric_verb / dispatch_hold_verb and the access-set pins by verify:F1 and verify:F2 of aterm-fabric-attach-round-3 on 2026-09-10; goldens regenerated as a pair from the merged catalog on 2026-09-11; merged with origin/main's own 2026-09-11 re-read of this file, conflicts resolved by the orchestrator (upstream wording kept where both sides fixed the same claim). 2026-09-12: the ONE prose change since that read is SHORT_CATALOG_MAX_BYTES' raise paragraph, read in full and MEASURED rather than recalled — aterm-gui's bare cmd_help is 9 610 B against the new 9 728 ceiling, VERBS is 101 rows, the summary rows alone are 9 238 B; the fabric, fx and hold rows and the OpClass/Access/Framing/Target docs re-read against dispatch_fabric_verb, cmd_fabric/cmd_fabric_attach/fabric_status_line, dispatch_hold_verb/fabric::cmd_hold and the access-set pin, the other 98 catalog rows sampled. One contradicted claim fixed in the commit that updated this row — Access::OwnerOnly called `hold` the ONE member whose handler tells the two owner-class scopes apart, and `fabric`'s handler is a second (Scope::Owner exactly, the bridge refused), as aterm-gui's own is_owner_class doc already records; 2026-09-13 (UTC; 2026-09-12 local) re-read of the trail row's paste-sweep additions (the licence= classes, program-row, the inserts_* status keys, which inputs stamp an insert and which stay dark, and the `inserts_delivered>0 inserts_lit=0` reading after a drop) against aterm-effects cursor_glow.rs AdmissionRecord::line/LICENCE_*, move_licensed, insert_echo/insert_rewrite/INSERT_HINT_FRESH, note_insert_delivered/lay_insert/retract_insert/insert_tally and TrailStatus::line, app_render.rs tick_cursor_fx's Rainbow-Kitty-gated delivery prelude, app_input.rs input_paste and insert_gesture_armed, lib.rs drop_file/deliver_paste, and control.rs's flagless, front-routed, background, guarded and run_feed_bin_routed paste-bin routes, fixing the contradictions in the row (paste-bin into the tab on screen stamps an insert, not nothing; the insert class and counters are Rainbow Kitty only; `A move paints only if a keypress LICENSED it` now admits the delivered insert; the key class gains the composer newline and ⌃V's gesture; inserts_delivered counts a bare Tab and ⌃V too; last_insert_cells is the 32-cell bound when unpriced; the drop reading covers an echo never seen inside the 2 s window) and regenerating both goldens as a pair; the 0.84 train's release-candidate merge (this machine's provenance/repair/TCC work over the peers' 2026-09-12 re-read at the tip) moved the bytes once more — both halves were read by their authors as recorded, the merged file is their union, and this row records the union's hash; 2026-09-13 read of the prose ca54aaadd moved since the recorded read (shown by `xtask gate help-surfaces --diff`, matched at 0f7667e8a) — the trail row's `trail status` key list gaining `park_returns= park_flushed=` and its new HELD PARKS sentence — against aterm-effects cursor_glow.rs TrailStatus::line (the two keys last, after swallowed_no_echo=), InFlightTally's park_returns/park_flushed, HeldPark, park_candidate (Rainbow Kitty, same-row backward, a fresh stamp or presses in flight), spawn's RETURN arm (a forward move from the landing past the origin within TYPE_HINT_FRESH = 0.25 s, park_return_paid, judged origin -> target) and flush_park, and the flushes at the stale-park tick, note_backspace, note_scroll and the next other move; no claim contradicted; 2026-09-13 read of the prose feat/round-7-offscreen moved (shown by `xtask gate help-surfaces --diff`, matched at ca54aaadd) — the new `offscreen` row (Read, Lines, Session; the summary, and the detail sentence by sentence) and the `history` row's arch= clause — against aterm-gui control_query.rs offscreen_args/cmd_offscreen/format_offscreen_reply/OFFSCREEN_DEFAULT_MAX, aterm-core alt_archive.rs (ALT_ARCHIVE_DEFAULT_BUDGET, env_opted_out's 0/off/false/no, AltArchive::read and wipe, the gap kinds, the ESU commit and the 16 ms epilogue fallback), control.rs json_unsupported and control_session.rs cmd_history's arch= field; two slips fixed before this row (a committed frame was only `a DEC 2026 close`, where an app that never sends one is committed at the batch epilogue at most once per 16 ms; lost= counted evicted rows only, where a reset or turning the archive off wipes rows into it too) and both goldens regenerated as a pair; 2026-09-13 merge of main into feat/round-7-offscreen: main's drift-sweep re-read on 2026-09-13 (lane small-b) found the trail licence= row saying an unpaid press logs `key` and fixed it; `gate help-surfaces --diff` at the merge shows the merged prose is exactly round 7's read offscreen row and history arch= clause plus main's trail licence=/decline-reasons correction and its rainbow-kitty v2_ sentence (added 2026-09-12, before main's recorded read); recorded at the merge; 2026-09-14 read of the prose round 10 moved (shown by `xtask gate help-surfaces --diff`, matched at bd8f70ca9) — the offscreen row's `in memory` (no longer `only`), its self-update sentence (the origin kept; the rows after the last 8 submitted turns' marks and at least the running app's last 8 screens, the newest without a turn, up to 1 MiB with older ones in lost=; a new origin when not carried; the resize breaks= gap) and breaks='s scroll-back clause, and the history row's carried=1 sentence — against aterm-gui handoff_carry.rs export/tail_from/TAIL_TURNS/CARRY_ARCHIVE_BYTES/encode_within/decode, aterm-core alt_archive.rs carry_head, carry_reach (REANCHOR_SCREENS screens, never below the app run's floor, and the re-shown run), carry_rows, import and shift_down's Jump gap on a scroll-back past the oldest retained row, control_session.rs cmd_history (` carried=1` before text=) and raise_turn_ids, turn_ledger.rs TurnRecord::carried, and seamless.rs take_incoming's next_turn_id raise; one slip fixed before this row (the carry was said to reach at least the last 8 screens, where carry_reach stops at the running app's floor — now the running app's last 8 screens) and both goldens regenerated as a pair; 2026-09-13 release-candidate merge of the leftovers lane over the peers' round-7/round-8 reads: the merged prose is the UNION of the two halves — main's `offscreen` row and `history` arch= clause (read by round 7's author, recorded above) and the leftovers lane's `momentum_glow=` key and the absorbed rainbow-kitty v2_ sentence (read by that lane, recorded next) — each half read in full by its author against the code it describes, neither contradicted by the other, and this row records the UNION's hash: re-read on 2026-09-13 by drift sweep of 2026-09-13 (lane small-b); the trail licence= roster said an unpaid press logs `key` and is fixed in that commit; that read recorded 212628b6b90c5780 (834cd5f9f), and main 2f15705bc was ALREADY red on this row: the wrapped-row band merge (169dafa7b) had added a sentence to the `trail status` row that nobody read — `While rainbow kitty owns the frame the row ends with v2_quads= v2_halos= v2_stars= v2_meteors= v2_bridged= ribbon_retired=` and its glosses; 2026-09-13 leftovers lane: TWO prose changes since that read, both read here: (1) the `momentum_glow=` key in the backticked run and its sentence (MomentumGlow::value beside the cat's `momentum=`), against TrailStatus::line in cursor_glow.rs and App::trail_status in app_render.rs (`ws.momentum_glow.value(now, MOMENTUM_GLOW_TAU_S)`), pinned by control.rs trail_status_help_enumerates_exactly_the_keys_the_row_emits; (2) the absorbed v2 sentence, against TrailStatus::line_v2 in cursor_glow.rs (appends exactly ` v2_quads= v2_halos= v2_stars= v2_meteors= v2_bridged= ribbon_retired=` from rk::Status, and only when CursorGlow::v2_status is Some — `self.v2.engaged()`, so only while rainbow kitty owns the frame, at the tail), rainbow_kitty/mod.rs Status (quads/halos written this frame, stars and meteors live; `bridged` = cells the echo ledger relit for a late echo the admission ring scored declined; `retired` = ribbon cells retired by CONTENT over the engine's life — Engine::witness_rows when the glyph under a cell changed or went, Engine::retire_row when the caret was seen on another row through a declined move — cumulative across Engine::reset) and app_render.rs (per-window `ws.cursor_glow`, so `the window's cumulative count`); no claim contradicted; 2026-09-13 (2026-09-14 UTC) read of the prose 9758cd022 moved since the recorded read (shown by `xtask gate help-surfaces --diff`, matched at 0333f4a441d2) — the `post` row's rewritten `no-bridge=1` paragraph (NARROWER rather than the opposite, not a verdict on the message, `fabric attach <command...>` arms a supervisor and that same outbox drains, further posts only until `ERR outbox full`) and its new THIRD OUTCOME sentence (`ERR timeout id=<n>`, the `--wait` expiring with no landing reported, queued exactly like the other two), plus the two `contains` assertions in the_fabric_rows_state_the_bounds_they_are_held_to that pin `none is coming ON ITS OWN` and `ERR timeout id=<n>` — against aterm-gui fabric.rs cmd_post (its option tokens are to/kind/re/dl/via/--wait/len only, so there is no idempotency key that could collapse a re-post; the refusal at the door answers `ERR outbox full queued= bytes=` at OUTBOX_CAP = 128 or OUTBOX_BYTES_MAX = 4 MiB measured by queued_load + caller_sized_bytes; the PostRow is pushed BEFORE the wait loop), that loop's deadline arm returning `ERR timeout id={id}` with the row untouched (only trim_retired_posts removes rows and only ones carrying `off` or `dead`, so a timed-out post is still queued and still drained), fabric_wait_refusal and bridge_reachable (`no-bridge=1` is exactly `!supervised && state == absent`), note_bridge_supervised, fabric_launch.rs spawn_supervisor/arm/preflight and control.rs cmd_fabric_attach (`fabric attach <command...>` is the one seam that starts a supervise thread at RUNTIME and flips the latch), and aterm-link bridge.rs drain_outbox, whose bare `outbox` peek takes exactly the `off.is_none() && !dead` rows and runs on the reconcile path a freshly attached bridge takes; no claim contradicted — the retired `Nothing will publish it, no answer can arrive` WAS false, and control.rs fabric_attach_arms_the_supervisor_of_a_running_instance_once_and_for_owner_only shows the flip (`no-bridge=1` at id=1 and id=2, `queued=1` at id=3 after the attach, the two earlier posts still queued); one narrowness left standing rather than fixed here: the row gives an instance with no `[fabric] command` as the cause, where the predicate is bridge_reachable, so a CONFIGURED command whose program fails arm's preflight answers `no-bridge=1` too (that same test's `/nonexistent/aterm-link`, id=2), and the remedy the row prints is the one spawn_supervisor's own warn line names for that case as well AND the same completeness slip the manual.rs read found was fixed here in this commit: the row enumerated three outcomes and `cmd_post` has a fourth — `ERR <reason> id=<n>` for a post the bridge retired (`unroutable`/`ambiguous`/`undeliverable`), which `outbox` then omits, so it is the one outcome that does NOT mean queued; the row now says so and `the_fabric_rows_state_the_...` pins `ERR <reason> id=<n>` and `unroutable` beside the other three. The hash recorded here is the CORRECTED row, and both goldens were regenerated as a pair; 2026-09-14 drift sweep (lane cli-and-types, 25 claims checked over the group): re-read against the code that moved under it since this row's anchor commit; every candidate went to a verifier prompted to REFUTE it — no claim contradicted the moved code The 2026-09-14 merge of the audit2 lane over the cli-and-types drift sweep moved the bytes once more; `--diff` shows exactly two prose lines, both the audit lane's own and read here against the code: the `no-fresh-hint` gloss's Rainbow Kitty clause (seam_licensed gates a fresh typed stamp on typed_credits_within >= 1 under GlowStyle::RainbowKitty, logged DECLINE_NO_FRESH_HINT; pinned by a_caret_that_advances_with_no_unpaid_press_still_buys_nothing), and the `inflight_forgotten=` edge list (forget_typed_credits at note_kill, at the keyless backward/cross-row refusal, and at the licensed move when the licence is not Typed, the hop is cross-row and unhinted, credit_starved or typed_over_cap — a same-row `no-fresh-hint` refusal is none of those and keeps the pool; a glyph's echo with Enter fresh takes the Typed licence, pinned by a_glyph_echo_does_not_spend_the_enter_pressed_behind_it); no claim contradicted; 2026-09-14 merge read of feat/round-10-carry over main: the merged prose is exactly round 10's offscreen and history rows (`in memory`, the self-update carry sentence, breaks='s scroll-back clause, carried=1) plus main's reads recorded above (the trail rows' paid-press no-fresh-hint gloss, the `momentum_glow=` key and sentence and the inflight_forgotten edge list; the post row's NARROWER no-bridge=1, its THIRD and FOURTH outcomes and their test needles) — `xtask gate help-surfaces --diff` against main's read (matched at d68552079) shows round 10's two hunks and against round 10's read (matched at a4a023d4a) main's three, nothing else — checked against the merged handoff_carry.rs (CARRY_ARCHIVE_BYTES, TAIL_TURNS), alt_archive.rs (REANCHOR_SCREENS, carry_reach, import) and control_session.rs cmd_history, untouched by main, and against cursor_glow.rs seam_licensed (a Rainbow Kitty typed stamp licenses only while the press ring owes a cell), TrailStatus::line (momentum_glow= after momentum_display=), forget_typed_credits' edges and fabric.rs cmd_post's four wait outcomes, untouched by round 10; no claim contradicted; both goldens regenerated as a pair at the merge and unchanged 2026-09-14 (merge of the new-line fade round): the ONE prose change is the `ribbon_retired=` sentence, which now says the count includes cells RELEASED to the swoosh when their text went, as well as cells retired on the fast melt when it was replaced — read against Engine::witness_rows and Status::retired (rainbow_kitty/mod.rs), which count both on the same tally. 2026-09-14 (the Rainbow Path v3 curtain merge): the ONE prose change `--diff` shows is the `trail status` key list gaining `ribbon_drawn= ribbon_curtain_ms=` between `ribbon_hue_bands=` and `field=`, READ against TrailStatus::line in aterm-effects cursor_glow.rs, whose format string emits them in exactly that position and prints `ribbon_curtain_ms=none` through `Option::map_or_else` when no curtain is falling; against CursorGlow::ribbon_drawn and CursorGlow::curtain_left_ms, both gated on `self.v2.engaged()` and answering 0 / None for every other style; against Engine::ribbon_drawn = Ribbon::ink_quad_count (the LAST FRAME'S own quads whose composite `reads_as_ink`) and Engine::curtain_left_ms = Ribbon::curtain_left_ms; and against App::trail_status in app_render.rs, which fills both. One COMPLETENESS slip fixed in the same commit rather than recorded around: the merge added the two keys to the list and glossed neither, and they are the pair a reader cannot use unglossed — `ribbon_segments=` is floored once for the arc's DIMMEST stop (`STATUS_LIT_COV`) and is a bound on what may be CLAIMED, which is why it read 0 for the last half of every curtain while a thousand quads were still compositing, and `ribbon_drawn=`/`ribbon_curtain_ms=` are the fact and the reason beside that claim. The row now says so, in the same words `TrailStatus::ribbon_drawn`'s and `CursorGlow::ribbon_drawn`'s own doc comments use, and both goldens were regenerated as a pair; the hash recorded here is the GLOSSED row. No claim contradicted.; 2026-09-14 v0.86 release-candidate merge: this file now carries BOTH halves of prose — upstream's and this candidate's — each read in full by its own author against the same handlers, neither contradicting the other, and this row records the UNION's hash; AND UPSTREAM'S READ OF THE SAME PROSE, kept rather than dropped: catalog read in full against the handlers by aterm-help-surfaces-read on 2026-09-10; the fabric row and the reworded hold row read against dispatch_fabric_verb / dispatch_hold_verb and the access-set pins by verify:F1 and verify:F2 of aterm-fabric-attach-round-3 on 2026-09-10; goldens regenerated as a pair from the merged catalog on 2026-09-11; merged with origin/main's own 2026-09-11 re-read of this file, conflicts resolved by the orchestrator (upstream wording kept where both sides fixed the same claim). 2026-09-12: the ONE prose change since that read is SHORT_CATALOG_MAX_BYTES' raise paragraph, read in full and MEASURED rather than recalled — aterm-gui's bare cmd_help is 9 610 B against the new 9 728 ceiling, VERBS is 101 rows, the summary rows alone are 9 238 B; the fabric, fx and hold rows and the OpClass/Access/Framing/Target docs re-read against dispatch_fabric_verb, cmd_fabric/cmd_fabric_attach/fabric_status_line, dispatch_hold_verb/fabric::cmd_hold and the access-set pin, the other 98 catalog rows sampled. One contradicted claim fixed in the commit that updated this row — Access::OwnerOnly called `hold` the ONE member whose handler tells the two owner-class scopes apart, and `fabric`'s handler is a second (Scope::Owner exactly, the bridge refused), as aterm-gui's own is_owner_class doc already records; 2026-09-13 (UTC; 2026-09-12 local) re-read of the trail row's paste-sweep additions (the licence= classes, program-row, the inserts_* status keys, which inputs stamp an insert and which stay dark, and the `inserts_delivered>0 inserts_lit=0` reading after a drop) against aterm-effects cursor_glow.rs AdmissionRecord::line/LICENCE_*, move_licensed, insert_echo/insert_rewrite/INSERT_HINT_FRESH, note_insert_delivered/lay_insert/retract_insert/insert_tally and TrailStatus::line, app_render.rs tick_cursor_fx's Rainbow-Kitty-gated delivery prelude, app_input.rs input_paste and insert_gesture_armed, lib.rs drop_file/deliver_paste, and control.rs's flagless, front-routed, background, guarded and run_feed_bin_routed paste-bin routes, fixing the contradictions in the row (paste-bin into the tab on screen stamps an insert, not nothing; the insert class and counters are Rainbow Kitty only; `A move paints only if a keypress LICENSED it` now admits the delivered insert; the key class gains the composer newline and ⌃V's gesture; inserts_delivered counts a bare Tab and ⌃V too; last_insert_cells is the 32-cell bound when unpriced; the drop reading covers an echo never seen inside the 2 s window) and regenerating both goldens as a pair; the 0.84 train's release-candidate merge (this machine's provenance/repair/TCC work over the peers' 2026-09-12 re-read at the tip) moved the bytes once more — both halves were read by their authors as recorded, the merged file is their union, and this row records the union's hash; 2026-09-13 read of the prose ca54aaadd moved since the recorded read (shown by `xtask gate help-surfaces --diff`, matched at 0f7667e8a) — the trail row's `trail status` key list gaining `park_returns= park_flushed=` and its new HELD PARKS sentence — against aterm-effects cursor_glow.rs TrailStatus::line (the two keys last, after swallowed_no_echo=), InFlightTally's park_returns/park_flushed, HeldPark, park_candidate (Rainbow Kitty, same-row backward, a fresh stamp or presses in flight), spawn's RETURN arm (a forward move from the landing past the origin within TYPE_HINT_FRESH = 0.25 s, park_return_paid, judged origin -> target) and flush_park, and the flushes at the stale-park tick, note_backspace, note_scroll and the next other move; no claim contradicted; 2026-09-13 read of the prose feat/round-7-offscreen moved (shown by `xtask gate help-surfaces --diff`, matched at ca54aaadd) — the new `offscreen` row (Read, Lines, Session; the summary, and the detail sentence by sentence) and the `history` row's arch= clause — against aterm-gui control_query.rs offscreen_args/cmd_offscreen/format_offscreen_reply/OFFSCREEN_DEFAULT_MAX, aterm-core alt_archive.rs (ALT_ARCHIVE_DEFAULT_BUDGET, env_opted_out's 0/off/false/no, AltArchive::read and wipe, the gap kinds, the ESU commit and the 16 ms epilogue fallback), control.rs json_unsupported and control_session.rs cmd_history's arch= field; two slips fixed before this row (a committed frame was only `a DEC 2026 close`, where an app that never sends one is committed at the batch epilogue at most once per 16 ms; lost= counted evicted rows only, where a reset or turning the archive off wipes rows into it too) and both goldens regenerated as a pair; 2026-09-13 merge of main into feat/round-7-offscreen: main's drift-sweep re-read on 2026-09-13 (lane small-b) found the trail licence= row saying an unpaid press logs `key` and fixed it; `gate help-surfaces --diff` at the merge shows the merged prose is exactly round 7's read offscreen row and history arch= clause plus main's trail licence=/decline-reasons correction and its rainbow-kitty v2_ sentence (added 2026-09-12, before main's recorded read); recorded at the merge; 2026-09-14 read of the prose round 10 moved (shown by `xtask gate help-surfaces --diff`, matched at bd8f70ca9) — the offscreen row's `in memory` (no longer `only`), its self-update sentence (the origin kept; the rows after the last 8 submitted turns' marks and at least the running app's last 8 screens, the newest without a turn, up to 1 MiB with older ones in lost=; a new origin when not carried; the resize breaks= gap) and breaks='s scroll-back clause, and the history row's carried=1 sentence — against aterm-gui handoff_carry.rs export/tail_from/TAIL_TURNS/CARRY_ARCHIVE_BYTES/encode_within/decode, aterm-core alt_archive.rs carry_head, carry_reach (REANCHOR_SCREENS screens, never below the app run's floor, and the re-shown run), carry_rows, import and shift_down's Jump gap on a scroll-back past the oldest retained row, control_session.rs cmd_history (` carried=1` before text=) and raise_turn_ids, turn_ledger.rs TurnRecord::carried, and seamless.rs take_incoming's next_turn_id raise; one slip fixed before this row (the carry was said to reach at least the last 8 screens, where carry_reach stops at the running app's floor — now the running app's last 8 screens) and both goldens regenerated as a pair; 2026-09-13 release-candidate merge of the leftovers lane over the peers' round-7/round-8 reads: the merged prose is the UNION of the two halves — main's `offscreen` row and `history` arch= clause (read by round 7's author, recorded above) and the leftovers lane's `momentum_glow=` key and the absorbed rainbow-kitty v2_ sentence (read by that lane, recorded next) — each half read in full by its author against the code it describes, neither contradicted by the other, and this row records the UNION's hash: re-read on 2026-09-13 by drift sweep of 2026-09-13 (lane small-b); the trail licence= roster said an unpaid press logs `key` and is fixed in that commit; that read recorded 212628b6b90c5780 (834cd5f9f), and main 2f15705bc was ALREADY red on this row: the wrapped-row band merge (169dafa7b) had added a sentence to the `trail status` row that nobody read — `While rainbow kitty owns the frame the row ends with v2_quads= v2_halos= v2_stars= v2_meteors= v2_bridged= ribbon_retired=` and its glosses; 2026-09-13 leftovers lane: TWO prose changes since that read, both read here: (1) the `momentum_glow=` key in the backticked run and its sentence (MomentumGlow::value beside the cat's `momentum=`), against TrailStatus::line in cursor_glow.rs and App::trail_status in app_render.rs (`ws.momentum_glow.value(now, MOMENTUM_GLOW_TAU_S)`), pinned by control.rs trail_status_help_enumerates_exactly_the_keys_the_row_emits; (2) the absorbed v2 sentence, against TrailStatus::line_v2 in cursor_glow.rs (appends exactly ` v2_quads= v2_halos= v2_stars= v2_meteors= v2_bridged= ribbon_retired=` from rk::Status, and only when CursorGlow::v2_status is Some — `self.v2.engaged()`, so only while rainbow kitty owns the frame, at the tail), rainbow_kitty/mod.rs Status (quads/halos written this frame, stars and meteors live; `bridged` = cells the echo ledger relit for a late echo the admission ring scored declined; `retired` = ribbon cells retired by CONTENT over the engine's life — Engine::witness_rows when the glyph under a cell changed or went, Engine::retire_row when the caret was seen on another row through a declined move — cumulative across Engine::reset) and app_render.rs (per-window `ws.cursor_glow`, so `the window's cumulative count`); no claim contradicted; 2026-09-13 (2026-09-14 UTC) read of the prose 9758cd022 moved since the recorded read (shown by `xtask gate help-surfaces --diff`, matched at 0333f4a441d2) — the `post` row's rewritten `no-bridge=1` paragraph (NARROWER rather than the opposite, not a verdict on the message, `fabric attach <command...>` arms a supervisor and that same outbox drains, further posts only until `ERR outbox full`) and its new THIRD OUTCOME sentence (`ERR timeout id=<n>`, the `--wait` expiring with no landing reported, queued exactly like the other two), plus the two `contains` assertions in the_fabric_rows_state_the_bounds_they_are_held_to that pin `none is coming ON ITS OWN` and `ERR timeout id=<n>` — against aterm-gui fabric.rs cmd_post (its option tokens are to/kind/re/dl/via/--wait/len only, so there is no idempotency key that could collapse a re-post; the refusal at the door answers `ERR outbox full queued= bytes=` at OUTBOX_CAP = 128 or OUTBOX_BYTES_MAX = 4 MiB measured by queued_load + caller_sized_bytes; the PostRow is pushed BEFORE the wait loop), that loop's deadline arm returning `ERR timeout id={id}` with the row untouched (only trim_retired_posts removes rows and only ones carrying `off` or `dead`, so a timed-out post is still queued and still drained), fabric_wait_refusal and bridge_reachable (`no-bridge=1` is exactly `!supervised && state == absent`), note_bridge_supervised, fabric_launch.rs spawn_supervisor/arm/preflight and control.rs cmd_fabric_attach (`fabric attach <command...>` is the one seam that starts a supervise thread at RUNTIME and flips the latch), and aterm-link bridge.rs drain_outbox, whose bare `outbox` peek takes exactly the `off.is_none() && !dead` rows and runs on the reconcile path a freshly attached bridge takes; no claim contradicted — the retired `Nothing will publish it, no answer can arrive` WAS false, and control.rs fabric_attach_arms_the_supervisor_of_a_running_instance_once_and_for_owner_only shows the flip (`no-bridge=1` at id=1 and id=2, `queued=1` at id=3 after the attach, the two earlier posts still queued); one narrowness left standing rather than fixed here: the row gives an instance with no `[fabric] command` as the cause, where the predicate is bridge_reachable, so a CONFIGURED command whose program fails arm's preflight answers `no-bridge=1` too (that same test's `/nonexistent/aterm-link`, id=2), and the remedy the row prints is the one spawn_supervisor's own warn line names for that case as well AND the same completeness slip the manual.rs read found was fixed here in this commit: the row enumerated three outcomes and `cmd_post` has a fourth — `ERR <reason> id=<n>` for a post the bridge retired (`unroutable`/`ambiguous`/`undeliverable`), which `outbox` then omits, so it is the one outcome that does NOT mean queued; the row now says so and `the_fabric_rows_state_the_...` pins `ERR <reason> id=<n>` and `unroutable` beside the other three. The hash recorded here is the CORRECTED row, and both goldens were regenerated as a pair; 2026-09-14 drift sweep (lane cli-and-types, 25 claims checked over the group): re-read against the code that moved under it since this row's anchor commit; every candidate went to a verifier prompted to REFUTE it — no claim contradicted the moved code The 2026-09-14 merge of the audit2 lane over the cli-and-types drift sweep moved the bytes once more; `--diff` shows exactly two prose lines, both the audit lane's own and read here against the code: the `no-fresh-hint` gloss's Rainbow Kitty clause (seam_licensed gates a fresh typed stamp on typed_credits_within >= 1 under GlowStyle::RainbowKitty, logged DECLINE_NO_FRESH_HINT; pinned by a_caret_that_advances_with_no_unpaid_press_still_buys_nothing), and the `inflight_forgotten=` edge list (forget_typed_credits at note_kill, at the keyless backward/cross-row refusal, and at the licensed move when the licence is not Typed, the hop is cross-row and unhinted, credit_starved or typed_over_cap — a same-row `no-fresh-hint` refusal is none of those and keeps the pool; a glyph's echo with Enter fresh takes the Typed licence, pinned by a_glyph_echo_does_not_spend_the_enter_pressed_behind_it); no claim contradicted; 2026-09-14 merge read of feat/round-10-carry over main: the merged prose is exactly round 10's offscreen and history rows (`in memory`, the self-update carry sentence, breaks='s scroll-back clause, carried=1) plus main's reads recorded above (the trail rows' paid-press no-fresh-hint gloss, the `momentum_glow=` key and sentence and the inflight_forgotten edge list; the post row's NARROWER no-bridge=1, its THIRD and FOURTH outcomes and their test needles) — `xtask gate help-surfaces --diff` against main's read (matched at d68552079) shows round 10's two hunks and against round 10's read (matched at a4a023d4a) main's three, nothing else — checked against the merged handoff_carry.rs (CARRY_ARCHIVE_BYTES, TAIL_TURNS), alt_archive.rs (REANCHOR_SCREENS, carry_reach, import) and control_session.rs cmd_history, untouched by main, and against cursor_glow.rs seam_licensed (a Rainbow Kitty typed stamp licenses only while the press ring owes a cell), TrailStatus::line (momentum_glow= after momentum_display=), forget_typed_credits' edges and fabric.rs cmd_post's four wait outcomes, untouched by round 10; no claim contradicted; both goldens regenerated as a pair at the merge and unchanged 2026-09-14 (merge of the new-line fade round): the ONE prose change is the `ribbon_retired=` sentence, which now says the count includes cells RELEASED to the swoosh when their text went, as well as cells retired on the fast melt when it was replaced — read against Engine::witness_rows and Status::retired (rainbow_kitty/mod.rs), which count both on the same tally.; 2026-09-14 read of the round-13 additions (the `status` row's fabric=<connected|stalled|disconnected|absent>, fabric_rtt_ms= and fabric_link_age_ms= sentences, the `fabric` row's state/reason/rtt_ms/link_age_ms reply shape, and the new bridge-only `link` row) against aterm-gui fabric.rs fabric_state, fabric_link_facts, fabric_status_tail, bridge_attached (an attach stores STALLED with reason=starting and no numbers), link_report (accepted only for the owning generation, dropped once DISCONNECTED, CONNECTED/STALLED stored under the generation lock, every registered session woken on the up-to-down transition) and cmd_link (the grammar, `OK stale=1` for a ghost lane), control.rs fabric_status_line and dispatch_bridge_verb's `link` arm, session_status.rs's record tail, and aterm-link bridge.rs LinkReport::ack_wants_report / down_wants_report (change, more-than-2x past LINK_MOVE_GAP = 250 ms, or LINK_REFRESH = 2 s), link_reason's tokens, ACK_DEADLINE = 5 s and RECONNECT_MIN = 100 ms / RECONNECT_MAX = 5 s; goldens regenerated as a pair; SHORT_CATALOG_MAX_BYTES raised to 9856 for the row with the same accounting as the three raises before it; no claim contradicted; 2026-09-15 the privacy row re-read against control_privacy.rs after 47198639f: its `which services it covers is not measured here, so fda_scope=unknown` clause contradicted covers_split/observed_fda_scope, which render `fda_scope=this_process` and `covers=app-data` for a Granted probe; reworded to the two things a grant now establishes and the three it still does not (other services unmeasured, adopted sessions inherit nothing, folder rows unknown), the scope-ruling sentence kept, the test's needles moved with it, both goldens regenerated; no other claim contradicted; 2026-09-14 v0.86 candidate merge: the file carries BOTH halves of prose — upstream's and this candidate's — each read in full by its author against the same handlers, neither contradicting the other, and this row records the UNION's hash; AND UPSTREAM'S READ OF THE SAME PROSE, kept: catalog read in full against the handlers by aterm-help-surfaces-read on 2026-09-10; the fabric row and the reworded hold row read against dispatch_fabric_verb / dispatch_hold_verb and the access-set pins by verify:F1 and verify:F2 of aterm-fabric-attach-round-3 on 2026-09-10; goldens regenerated as a pair from the merged catalog on 2026-09-11; merged with origin/main's own 2026-09-11 re-read of this file, conflicts resolved by the orchestrator (upstream wording kept where both sides fixed the same claim). 2026-09-12: the ONE prose change since that read is SHORT_CATALOG_MAX_BYTES' raise paragraph, read in full and MEASURED rather than recalled — aterm-gui's bare cmd_help is 9 610 B against the new 9 728 ceiling, VERBS is 101 rows, the summary rows alone are 9 238 B; the fabric, fx and hold rows and the OpClass/Access/Framing/Target docs re-read against dispatch_fabric_verb, cmd_fabric/cmd_fabric_attach/fabric_status_line, dispatch_hold_verb/fabric::cmd_hold and the access-set pin, the other 98 catalog rows sampled. One contradicted claim fixed in the commit that updated this row — Access::OwnerOnly called `hold` the ONE member whose handler tells the two owner-class scopes apart, and `fabric`'s handler is a second (Scope::Owner exactly, the bridge refused), as aterm-gui's own is_owner_class doc already records; 2026-09-13 (UTC; 2026-09-12 local) re-read of the trail row's paste-sweep additions (the licence= classes, program-row, the inserts_* status keys, which inputs stamp an insert and which stay dark, and the `inserts_delivered>0 inserts_lit=0` reading after a drop) against aterm-effects cursor_glow.rs AdmissionRecord::line/LICENCE_*, move_licensed, insert_echo/insert_rewrite/INSERT_HINT_FRESH, note_insert_delivered/lay_insert/retract_insert/insert_tally and TrailStatus::line, app_render.rs tick_cursor_fx's Rainbow-Kitty-gated delivery prelude, app_input.rs input_paste and insert_gesture_armed, lib.rs drop_file/deliver_paste, and control.rs's flagless, front-routed, background, guarded and run_feed_bin_routed paste-bin routes, fixing the contradictions in the row (paste-bin into the tab on screen stamps an insert, not nothing; the insert class and counters are Rainbow Kitty only; `A move paints only if a keypress LICENSED it` now admits the delivered insert; the key class gains the composer newline and ⌃V's gesture; inserts_delivered counts a bare Tab and ⌃V too; last_insert_cells is the 32-cell bound when unpriced; the drop reading covers an echo never seen inside the 2 s window) and regenerating both goldens as a pair; the 0.84 train's release-candidate merge (this machine's provenance/repair/TCC work over the peers' 2026-09-12 re-read at the tip) moved the bytes once more — both halves were read by their authors as recorded, the merged file is their union, and this row records the union's hash; 2026-09-13 read of the prose ca54aaadd moved since the recorded read (shown by `xtask gate help-surfaces --diff`, matched at 0f7667e8a) — the trail row's `trail status` key list gaining `park_returns= park_flushed=` and its new HELD PARKS sentence — against aterm-effects cursor_glow.rs TrailStatus::line (the two keys last, after swallowed_no_echo=), InFlightTally's park_returns/park_flushed, HeldPark, park_candidate (Rainbow Kitty, same-row backward, a fresh stamp or presses in flight), spawn's RETURN arm (a forward move from the landing past the origin within TYPE_HINT_FRESH = 0.25 s, park_return_paid, judged origin -> target) and flush_park, and the flushes at the stale-park tick, note_backspace, note_scroll and the next other move; no claim contradicted; 2026-09-13 read of the prose feat/round-7-offscreen moved (shown by `xtask gate help-surfaces --diff`, matched at ca54aaadd) — the new `offscreen` row (Read, Lines, Session; the summary, and the detail sentence by sentence) and the `history` row's arch= clause — against aterm-gui control_query.rs offscreen_args/cmd_offscreen/format_offscreen_reply/OFFSCREEN_DEFAULT_MAX, aterm-core alt_archive.rs (ALT_ARCHIVE_DEFAULT_BUDGET, env_opted_out's 0/off/false/no, AltArchive::read and wipe, the gap kinds, the ESU commit and the 16 ms epilogue fallback), control.rs json_unsupported and control_session.rs cmd_history's arch= field; two slips fixed before this row (a committed frame was only `a DEC 2026 close`, where an app that never sends one is committed at the batch epilogue at most once per 16 ms; lost= counted evicted rows only, where a reset or turning the archive off wipes rows into it too) and both goldens regenerated as a pair; 2026-09-13 merge of main into feat/round-7-offscreen: main's drift-sweep re-read on 2026-09-13 (lane small-b) found the trail licence= row saying an unpaid press logs `key` and fixed it; `gate help-surfaces --diff` at the merge shows the merged prose is exactly round 7's read offscreen row and history arch= clause plus main's trail licence=/decline-reasons correction and its rainbow-kitty v2_ sentence (added 2026-09-12, before main's recorded read); recorded at the merge; 2026-09-14 read of the prose round 10 moved (shown by `xtask gate help-surfaces --diff`, matched at bd8f70ca9) — the offscreen row's `in memory` (no longer `only`), its self-update sentence (the origin kept; the rows after the last 8 submitted turns' marks and at least the running app's last 8 screens, the newest without a turn, up to 1 MiB with older ones in lost=; a new origin when not carried; the resize breaks= gap) and breaks='s scroll-back clause, and the history row's carried=1 sentence — against aterm-gui handoff_carry.rs export/tail_from/TAIL_TURNS/CARRY_ARCHIVE_BYTES/encode_within/decode, aterm-core alt_archive.rs carry_head, carry_reach (REANCHOR_SCREENS screens, never below the app run's floor, and the re-shown run), carry_rows, import and shift_down's Jump gap on a scroll-back past the oldest retained row, control_session.rs cmd_history (` carried=1` before text=) and raise_turn_ids, turn_ledger.rs TurnRecord::carried, and seamless.rs take_incoming's next_turn_id raise; one slip fixed before this row (the carry was said to reach at least the last 8 screens, where carry_reach stops at the running app's floor — now the running app's last 8 screens) and both goldens regenerated as a pair; 2026-09-13 release-candidate merge of the leftovers lane over the peers' round-7/round-8 reads: the merged prose is the UNION of the two halves — main's `offscreen` row and `history` arch= clause (read by round 7's author, recorded above) and the leftovers lane's `momentum_glow=` key and the absorbed rainbow-kitty v2_ sentence (read by that lane, recorded next) — each half read in full by its author against the code it describes, neither contradicted by the other, and this row records the UNION's hash: re-read on 2026-09-13 by drift sweep of 2026-09-13 (lane small-b); the trail licence= roster said an unpaid press logs `key` and is fixed in that commit; that read recorded 212628b6b90c5780 (834cd5f9f), and main 2f15705bc was ALREADY red on this row: the wrapped-row band merge (169dafa7b) had added a sentence to the `trail status` row that nobody read — `While rainbow kitty owns the frame the row ends with v2_quads= v2_halos= v2_stars= v2_meteors= v2_bridged= ribbon_retired=` and its glosses; 2026-09-13 leftovers lane: TWO prose changes since that read, both read here: (1) the `momentum_glow=` key in the backticked run and its sentence (MomentumGlow::value beside the cat's `momentum=`), against TrailStatus::line in cursor_glow.rs and App::trail_status in app_render.rs (`ws.momentum_glow.value(now, MOMENTUM_GLOW_TAU_S)`), pinned by control.rs trail_status_help_enumerates_exactly_the_keys_the_row_emits; (2) the absorbed v2 sentence, against TrailStatus::line_v2 in cursor_glow.rs (appends exactly ` v2_quads= v2_halos= v2_stars= v2_meteors= v2_bridged= ribbon_retired=` from rk::Status, and only when CursorGlow::v2_status is Some — `self.v2.engaged()`, so only while rainbow kitty owns the frame, at the tail), rainbow_kitty/mod.rs Status (quads/halos written this frame, stars and meteors live; `bridged` = cells the echo ledger relit for a late echo the admission ring scored declined; `retired` = ribbon cells retired by CONTENT over the engine's life — Engine::witness_rows when the glyph under a cell changed or went, Engine::retire_row when the caret was seen on another row through a declined move — cumulative across Engine::reset) and app_render.rs (per-window `ws.cursor_glow`, so `the window's cumulative count`); no claim contradicted; 2026-09-13 (2026-09-14 UTC) read of the prose 9758cd022 moved since the recorded read (shown by `xtask gate help-surfaces --diff`, matched at 0333f4a441d2) — the `post` row's rewritten `no-bridge=1` paragraph (NARROWER rather than the opposite, not a verdict on the message, `fabric attach <command...>` arms a supervisor and that same outbox drains, further posts only until `ERR outbox full`) and its new THIRD OUTCOME sentence (`ERR timeout id=<n>`, the `--wait` expiring with no landing reported, queued exactly like the other two), plus the two `contains` assertions in the_fabric_rows_state_the_bounds_they_are_held_to that pin `none is coming ON ITS OWN` and `ERR timeout id=<n>` — against aterm-gui fabric.rs cmd_post (its option tokens are to/kind/re/dl/via/--wait/len only, so there is no idempotency key that could collapse a re-post; the refusal at the door answers `ERR outbox full queued= bytes=` at OUTBOX_CAP = 128 or OUTBOX_BYTES_MAX = 4 MiB measured by queued_load + caller_sized_bytes; the PostRow is pushed BEFORE the wait loop), that loop's deadline arm returning `ERR timeout id={id}` with the row untouched (only trim_retired_posts removes rows and only ones carrying `off` or `dead`, so a timed-out post is still queued and still drained), fabric_wait_refusal and bridge_reachable (`no-bridge=1` is exactly `!supervised && state == absent`), note_bridge_supervised, fabric_launch.rs spawn_supervisor/arm/preflight and control.rs cmd_fabric_attach (`fabric attach <command...>` is the one seam that starts a supervise thread at RUNTIME and flips the latch), and aterm-link bridge.rs drain_outbox, whose bare `outbox` peek takes exactly the `off.is_none() && !dead` rows and runs on the reconcile path a freshly attached bridge takes; no claim contradicted — the retired `Nothing will publish it, no answer can arrive` WAS false, and control.rs fabric_attach_arms_the_supervisor_of_a_running_instance_once_and_for_owner_only shows the flip (`no-bridge=1` at id=1 and id=2, `queued=1` at id=3 after the attach, the two earlier posts still queued); one narrowness left standing rather than fixed here: the row gives an instance with no `[fabric] command` as the cause, where the predicate is bridge_reachable, so a CONFIGURED command whose program fails arm's preflight answers `no-bridge=1` too (that same test's `/nonexistent/aterm-link`, id=2), and the remedy the row prints is the one spawn_supervisor's own warn line names for that case as well AND the same completeness slip the manual.rs read found was fixed here in this commit: the row enumerated three outcomes and `cmd_post` has a fourth — `ERR <reason> id=<n>` for a post the bridge retired (`unroutable`/`ambiguous`/`undeliverable`), which `outbox` then omits, so it is the one outcome that does NOT mean queued; the row now says so and `the_fabric_rows_state_the_...` pins `ERR <reason> id=<n>` and `unroutable` beside the other three. The hash recorded here is the CORRECTED row, and both goldens were regenerated as a pair; 2026-09-14 drift sweep (lane cli-and-types, 25 claims checked over the group): re-read against the code that moved under it since this row's anchor commit; every candidate went to a verifier prompted to REFUTE it — no claim contradicted the moved code The 2026-09-14 merge of the audit2 lane over the cli-and-types drift sweep moved the bytes once more; `--diff` shows exactly two prose lines, both the audit lane's own and read here against the code: the `no-fresh-hint` gloss's Rainbow Kitty clause (seam_licensed gates a fresh typed stamp on typed_credits_within >= 1 under GlowStyle::RainbowKitty, logged DECLINE_NO_FRESH_HINT; pinned by a_caret_that_advances_with_no_unpaid_press_still_buys_nothing), and the `inflight_forgotten=` edge list (forget_typed_credits at note_kill, at the keyless backward/cross-row refusal, and at the licensed move when the licence is not Typed, the hop is cross-row and unhinted, credit_starved or typed_over_cap — a same-row `no-fresh-hint` refusal is none of those and keeps the pool; a glyph's echo with Enter fresh takes the Typed licence, pinned by a_glyph_echo_does_not_spend_the_enter_pressed_behind_it); no claim contradicted; 2026-09-14 merge read of feat/round-10-carry over main: the merged prose is exactly round 10's offscreen and history rows (`in memory`, the self-update carry sentence, breaks='s scroll-back clause, carried=1) plus main's reads recorded above (the trail rows' paid-press no-fresh-hint gloss, the `momentum_glow=` key and sentence and the inflight_forgotten edge list; the post row's NARROWER no-bridge=1, its THIRD and FOURTH outcomes and their test needles) — `xtask gate help-surfaces --diff` against main's read (matched at d68552079) shows round 10's two hunks and against round 10's read (matched at a4a023d4a) main's three, nothing else — checked against the merged handoff_carry.rs (CARRY_ARCHIVE_BYTES, TAIL_TURNS), alt_archive.rs (REANCHOR_SCREENS, carry_reach, import) and control_session.rs cmd_history, untouched by main, and against cursor_glow.rs seam_licensed (a Rainbow Kitty typed stamp licenses only while the press ring owes a cell), TrailStatus::line (momentum_glow= after momentum_display=), forget_typed_credits' edges and fabric.rs cmd_post's four wait outcomes, untouched by round 10; no claim contradicted; both goldens regenerated as a pair at the merge and unchanged 2026-09-14 (merge of the new-line fade round): the ONE prose change is the `ribbon_retired=` sentence, which now says the count includes cells RELEASED to the swoosh when their text went, as well as cells retired on the fast melt when it was replaced — read against Engine::witness_rows and Status::retired (rainbow_kitty/mod.rs), which count both on the same tally.; 2026-09-14 read of the round-13 additions (the `status` row's fabric=<connected|stalled|disconnected|absent>, fabric_rtt_ms= and fabric_link_age_ms= sentences, the `fabric` row's state/reason/rtt_ms/link_age_ms reply shape, and the new bridge-only `link` row) against aterm-gui fabric.rs fabric_state, fabric_link_facts, fabric_status_tail, bridge_attached (an attach stores STALLED with reason=starting and no numbers), link_report (accepted only for the owning generation, dropped once DISCONNECTED, CONNECTED/STALLED stored under the generation lock, every registered session woken on the up-to-down transition) and cmd_link (the grammar, `OK stale=1` for a ghost lane), control.rs fabric_status_line and dispatch_bridge_verb's `link` arm, session_status.rs's record tail, and aterm-link bridge.rs LinkReport::ack_wants_report / down_wants_report (change, more-than-2x past LINK_MOVE_GAP = 250 ms, or LINK_REFRESH = 2 s), link_reason's tokens, ACK_DEADLINE = 5 s and RECONNECT_MIN = 100 ms / RECONNECT_MAX = 5 s; goldens regenerated as a pair; SHORT_CATALOG_MAX_BYTES raised to 9856 for the row with the same accounting as the three raises before it; no claim contradicted; 2026-09-15 recovery audit: read the update synopsis and full help against cmd_update and update_control::Snapshot. Apply acknowledges a request before asynchronous validation; checks use the current GUI source and notify on every completion. relaunch_ready indicates stage existence, the enumerated apply_posture values match the exact-artifact projection including unknown/unreconciled, and apply_policy_reason is emitted only for a current policy block. No readiness or timing guarantee is asserted; both generated help fixtures are regenerated together. The subsequent privacy-help merge was read against observed_fda_scope, covers_split and per-session privacy observations: a host grant establishes this_process and app-data coverage, other services remain unmeasured/uncovered, and adopted sessions and folder rows retain unknown access. The row records both changes.; 2026-09-14 v0.86 candidate merge: this file carries BOTH halves of prose — upstream's and the candidate's — each read by its own author against the same handlers, and the row records the UNION's hash; 2026-09-15 merge read of feat/round-15-receipts over main: the merged prose is exactly round 15's change (the `await`, `inbox`, `inbox get`, `inbox seen`, `post`, `deliver`, `outbox` and `outbox sent` rows, the needle table's new rows, SHORT_CATALOG_MAX_BYTES' 9984 note and the `inbox get` history's round-15 aside: 8 hunks, shown by `xtask gate help-surfaces --diff` against main's read at 595e13613) plus upstream's change (the `update` row's apply-posture sentences, the `privacy` row's this_process, app-data and unmeasured sentences and needles and the `trail status` row's ribbon_drawn=/ribbon_curtain_ms= keys and gloss, all read by main — and f3a5467ed's `version` row, which main recorded no row for: flavor=t|r, trust= as Trust's own version (`none` on an upstream build), rust_compat=, and trustc=/trustc_commit=/trustc_host= carrying the rustc-shaped token, shown against round 15's read at 030ba1788) plus ONE sentence added at this merge to `post`'s key= text — its ADDRESS is still resolved first, against the live roster, so a re-post to one that no longer routes is retired `ERR unroutable` (or `ambiguous`), appends nothing and leaves the key naming its record (both help goldens regenerated as a pair from the merged catalog), checked against aterm-gui build_info.rs control_line (trust= is `none` off flavor t and `unreported` on a Trust build predating the marker, a value the row does not list but does not deny; rust_compat= and trustc= are both compiler_release()), update_control.rs apply_posture and apply_policy_reason (the twelve words the row lists, none other), control_privacy.rs covers_split and observed_fda_scope, aterm-effects cursor_glow.rs TrailStatus::line, and aterm-link bridge.rs drain_outbox (resolve_to before StateDir::key_seq; the non-routing arm retires `off=- reason=` and clears only the post's pin) with the r15_receipts test a_keyed_re_post_to_a_session_whose_exit_line_never_arrives_is_unroutable_and_the_key_holds;  2026-09-14 (the Rainbow Path v3 curtain merge): the ONE prose change `--diff` shows is the `trail status` key list gaining `ribbon_drawn= ribbon_curtain_ms=` between `ribbon_hue_bands=` and `field=`, READ against TrailStatus::line in aterm-effects cursor_glow.rs, whose format string emits them in exactly that position and prints `ribbon_curtain_ms=none` through `Option::map_or_else` when no curtain is falling; against CursorGlow::ribbon_drawn and CursorGlow::curtain_left_ms, both gated on `self.v2.engaged()` and answering 0 / None for every other style; against Engine::ribbon_drawn = Ribbon::ink_quad_count (the LAST FRAME'S own quads whose composite `reads_as_ink`) and Engine::curtain_left_ms = Ribbon::curtain_left_ms; and against App::trail_status in app_render.rs, which fills both. One COMPLETENESS slip fixed in the same commit rather than recorded around: the merge added the two keys to the list and glossed neither, and they are the pair a reader cannot use unglossed — `ribbon_segments=` is floored once for the arc's DIMMEST stop (`STATUS_LIT_COV`) and is a bound on what may be CLAIMED, which is why it read 0 for the last half of every curtain while a thousand quads were still compositing, and `ribbon_drawn=`/`ribbon_curtain_ms=` are the fact and the reason beside that claim. The row now says so, in the same words `TrailStatus::ribbon_drawn`'s and `CursorGlow::ribbon_drawn`'s own doc comments use, and both goldens were regenerated as a pair; the hash recorded here is the GLOSSED row. No claim contradicted.; AND UPSTREAM'S OWN READ OF THE SAME PROSE, recorded in parallel and kept rather than dropped: catalog read in full against the handlers by aterm-help-surfaces-read on 2026-09-10; the fabric row and the reworded hold row read against dispatch_fabric_verb / dispatch_hold_verb and the access-set pins by verify:F1 and verify:F2 of aterm-fabric-attach-round-3 on 2026-09-10; goldens regenerated as a pair from the merged catalog on 2026-09-11; merged with origin/main's own 2026-09-11 re-read of this file, conflicts resolved by the orchestrator (upstream wording kept where both sides fixed the same claim). 2026-09-12: the ONE prose change since that read is SHORT_CATALOG_MAX_BYTES' raise paragraph, read in full and MEASURED rather than recalled — aterm-gui's bare cmd_help is 9 610 B against the new 9 728 ceiling, VERBS is 101 rows, the summary rows alone are 9 238 B; the fabric, fx and hold rows and the OpClass/Access/Framing/Target docs re-read against dispatch_fabric_verb, cmd_fabric/cmd_fabric_attach/fabric_status_line, dispatch_hold_verb/fabric::cmd_hold and the access-set pin, the other 98 catalog rows sampled. One contradicted claim fixed in the commit that updated this row — Access::OwnerOnly called `hold` the ONE member whose handler tells the two owner-class scopes apart, and `fabric`'s handler is a second (Scope::Owner exactly, the bridge refused), as aterm-gui's own is_owner_class doc already records; 2026-09-13 (UTC; 2026-09-12 local) re-read of the trail row's paste-sweep additions (the licence= classes, program-row, the inserts_* status keys, which inputs stamp an insert and which stay dark, and the `inserts_delivered>0 inserts_lit=0` reading after a drop) against aterm-effects cursor_glow.rs AdmissionRecord::line/LICENCE_*, move_licensed, insert_echo/insert_rewrite/INSERT_HINT_FRESH, note_insert_delivered/lay_insert/retract_insert/insert_tally and TrailStatus::line, app_render.rs tick_cursor_fx's Rainbow-Kitty-gated delivery prelude, app_input.rs input_paste and insert_gesture_armed, lib.rs drop_file/deliver_paste, and control.rs's flagless, front-routed, background, guarded and run_feed_bin_routed paste-bin routes, fixing the contradictions in the row (paste-bin into the tab on screen stamps an insert, not nothing; the insert class and counters are Rainbow Kitty only; `A move paints only if a keypress LICENSED it` now admits the delivered insert; the key class gains the composer newline and ⌃V's gesture; inserts_delivered counts a bare Tab and ⌃V too; last_insert_cells is the 32-cell bound when unpriced; the drop reading covers an echo never seen inside the 2 s window) and regenerating both goldens as a pair; the 0.84 train's release-candidate merge (this machine's provenance/repair/TCC work over the peers' 2026-09-12 re-read at the tip) moved the bytes once more — both halves were read by their authors as recorded, the merged file is their union, and this row records the union's hash; 2026-09-13 read of the prose ca54aaadd moved since the recorded read (shown by `xtask gate help-surfaces --diff`, matched at 0f7667e8a) — the trail row's `trail status` key list gaining `park_returns= park_flushed=` and its new HELD PARKS sentence — against aterm-effects cursor_glow.rs TrailStatus::line (the two keys last, after swallowed_no_echo=), InFlightTally's park_returns/park_flushed, HeldPark, park_candidate (Rainbow Kitty, same-row backward, a fresh stamp or presses in flight), spawn's RETURN arm (a forward move from the landing past the origin within TYPE_HINT_FRESH = 0.25 s, park_return_paid, judged origin -> target) and flush_park, and the flushes at the stale-park tick, note_backspace, note_scroll and the next other move; no claim contradicted; 2026-09-13 read of the prose feat/round-7-offscreen moved (shown by `xtask gate help-surfaces --diff`, matched at ca54aaadd) — the new `offscreen` row (Read, Lines, Session; the summary, and the detail sentence by sentence) and the `history` row's arch= clause — against aterm-gui control_query.rs offscreen_args/cmd_offscreen/format_offscreen_reply/OFFSCREEN_DEFAULT_MAX, aterm-core alt_archive.rs (ALT_ARCHIVE_DEFAULT_BUDGET, env_opted_out's 0/off/false/no, AltArchive::read and wipe, the gap kinds, the ESU commit and the 16 ms epilogue fallback), control.rs json_unsupported and control_session.rs cmd_history's arch= field; two slips fixed before this row (a committed frame was only `a DEC 2026 close`, where an app that never sends one is committed at the batch epilogue at most once per 16 ms; lost= counted evicted rows only, where a reset or turning the archive off wipes rows into it too) and both goldens regenerated as a pair; 2026-09-13 merge of main into feat/round-7-offscreen: main's drift-sweep re-read on 2026-09-13 (lane small-b) found the trail licence= row saying an unpaid press logs `key` and fixed it; `gate help-surfaces --diff` at the merge shows the merged prose is exactly round 7's read offscreen row and history arch= clause plus main's trail licence=/decline-reasons correction and its rainbow-kitty v2_ sentence (added 2026-09-12, before main's recorded read); recorded at the merge; 2026-09-14 read of the prose round 10 moved (shown by `xtask gate help-surfaces --diff`, matched at bd8f70ca9) — the offscreen row's `in memory` (no longer `only`), its self-update sentence (the origin kept; the rows after the last 8 submitted turns' marks and at least the running app's last 8 screens, the newest without a turn, up to 1 MiB with older ones in lost=; a new origin when not carried; the resize breaks= gap) and breaks='s scroll-back clause, and the history row's carried=1 sentence — against aterm-gui handoff_carry.rs export/tail_from/TAIL_TURNS/CARRY_ARCHIVE_BYTES/encode_within/decode, aterm-core alt_archive.rs carry_head, carry_reach (REANCHOR_SCREENS screens, never below the app run's floor, and the re-shown run), carry_rows, import and shift_down's Jump gap on a scroll-back past the oldest retained row, control_session.rs cmd_history (` carried=1` before text=) and raise_turn_ids, turn_ledger.rs TurnRecord::carried, and seamless.rs take_incoming's next_turn_id raise; one slip fixed before this row (the carry was said to reach at least the last 8 screens, where carry_reach stops at the running app's floor — now the running app's last 8 screens) and both goldens regenerated as a pair; 2026-09-13 release-candidate merge of the leftovers lane over the peers' round-7/round-8 reads: the merged prose is the UNION of the two halves — main's `offscreen` row and `history` arch= clause (read by round 7's author, recorded above) and the leftovers lane's `momentum_glow=` key and the absorbed rainbow-kitty v2_ sentence (read by that lane, recorded next) — each half read in full by its author against the code it describes, neither contradicted by the other, and this row records the UNION's hash: re-read on 2026-09-13 by drift sweep of 2026-09-13 (lane small-b); the trail licence= roster said an unpaid press logs `key` and is fixed in that commit; that read recorded 212628b6b90c5780 (834cd5f9f), and main 2f15705bc was ALREADY red on this row: the wrapped-row band merge (169dafa7b) had added a sentence to the `trail status` row that nobody read — `While rainbow kitty owns the frame the row ends with v2_quads= v2_halos= v2_stars= v2_meteors= v2_bridged= ribbon_retired=` and its glosses; 2026-09-13 leftovers lane: TWO prose changes since that read, both read here: (1) the `momentum_glow=` key in the backticked run and its sentence (MomentumGlow::value beside the cat's `momentum=`), against TrailStatus::line in cursor_glow.rs and App::trail_status in app_render.rs (`ws.momentum_glow.value(now, MOMENTUM_GLOW_TAU_S)`), pinned by control.rs trail_status_help_enumerates_exactly_the_keys_the_row_emits; (2) the absorbed v2 sentence, against TrailStatus::line_v2 in cursor_glow.rs (appends exactly ` v2_quads= v2_halos= v2_stars= v2_meteors= v2_bridged= ribbon_retired=` from rk::Status, and only when CursorGlow::v2_status is Some — `self.v2.engaged()`, so only while rainbow kitty owns the frame, at the tail), rainbow_kitty/mod.rs Status (quads/halos written this frame, stars and meteors live; `bridged` = cells the echo ledger relit for a late echo the admission ring scored declined; `retired` = ribbon cells retired by CONTENT over the engine's life — Engine::witness_rows when the glyph under a cell changed or went, Engine::retire_row when the caret was seen on another row through a declined move — cumulative across Engine::reset) and app_render.rs (per-window `ws.cursor_glow`, so `the window's cumulative count`); no claim contradicted; 2026-09-13 (2026-09-14 UTC) read of the prose 9758cd022 moved since the recorded read (shown by `xtask gate help-surfaces --diff`, matched at 0333f4a441d2) — the `post` row's rewritten `no-bridge=1` paragraph (NARROWER rather than the opposite, not a verdict on the message, `fabric attach <command...>` arms a supervisor and that same outbox drains, further posts only until `ERR outbox full`) and its new THIRD OUTCOME sentence (`ERR timeout id=<n>`, the `--wait` expiring with no landing reported, queued exactly like the other two), plus the two `contains` assertions in the_fabric_rows_state_the_bounds_they_are_held_to that pin `none is coming ON ITS OWN` and `ERR timeout id=<n>` — against aterm-gui fabric.rs cmd_post (its option tokens are to/kind/re/dl/via/--wait/len only, so there is no idempotency key that could collapse a re-post; the refusal at the door answers `ERR outbox full queued= bytes=` at OUTBOX_CAP = 128 or OUTBOX_BYTES_MAX = 4 MiB measured by queued_load + caller_sized_bytes; the PostRow is pushed BEFORE the wait loop), that loop's deadline arm returning `ERR timeout id={id}` with the row untouched (only trim_retired_posts removes rows and only ones carrying `off` or `dead`, so a timed-out post is still queued and still drained), fabric_wait_refusal and bridge_reachable (`no-bridge=1` is exactly `!supervised && state == absent`), note_bridge_supervised, fabric_launch.rs spawn_supervisor/arm/preflight and control.rs cmd_fabric_attach (`fabric attach <command...>` is the one seam that starts a supervise thread at RUNTIME and flips the latch), and aterm-link bridge.rs drain_outbox, whose bare `outbox` peek takes exactly the `off.is_none() && !dead` rows and runs on the reconcile path a freshly attached bridge takes; no claim contradicted — the retired `Nothing will publish it, no answer can arrive` WAS false, and control.rs fabric_attach_arms_the_supervisor_of_a_running_instance_once_and_for_owner_only shows the flip (`no-bridge=1` at id=1 and id=2, `queued=1` at id=3 after the attach, the two earlier posts still queued); one narrowness left standing rather than fixed here: the row gives an instance with no `[fabric] command` as the cause, where the predicate is bridge_reachable, so a CONFIGURED command whose program fails arm's preflight answers `no-bridge=1` too (that same test's `/nonexistent/aterm-link`, id=2), and the remedy the row prints is the one spawn_supervisor's own warn line names for that case as well AND the same completeness slip the manual.rs read found was fixed here in this commit: the row enumerated three outcomes and `cmd_post` has a fourth — `ERR <reason> id=<n>` for a post the bridge retired (`unroutable`/`ambiguous`/`undeliverable`), which `outbox` then omits, so it is the one outcome that does NOT mean queued; the row now says so and `the_fabric_rows_state_the_...` pins `ERR <reason> id=<n>` and `unroutable` beside the other three. The hash recorded here is the CORRECTED row, and both goldens were regenerated as a pair; 2026-09-14 drift sweep (lane cli-and-types, 25 claims checked over the group): re-read against the code that moved under it since this row's anchor commit; every candidate went to a verifier prompted to REFUTE it — no claim contradicted the moved code The 2026-09-14 merge of the audit2 lane over the cli-and-types drift sweep moved the bytes once more; `--diff` shows exactly two prose lines, both the audit lane's own and read here against the code: the `no-fresh-hint` gloss's Rainbow Kitty clause (seam_licensed gates a fresh typed stamp on typed_credits_within >= 1 under GlowStyle::RainbowKitty, logged DECLINE_NO_FRESH_HINT; pinned by a_caret_that_advances_with_no_unpaid_press_still_buys_nothing), and the `inflight_forgotten=` edge list (forget_typed_credits at note_kill, at the keyless backward/cross-row refusal, and at the licensed move when the licence is not Typed, the hop is cross-row and unhinted, credit_starved or typed_over_cap — a same-row `no-fresh-hint` refusal is none of those and keeps the pool; a glyph's echo with Enter fresh takes the Typed licence, pinned by a_glyph_echo_does_not_spend_the_enter_pressed_behind_it); no claim contradicted; 2026-09-14 merge read of feat/round-10-carry over main: the merged prose is exactly round 10's offscreen and history rows (`in memory`, the self-update carry sentence, breaks='s scroll-back clause, carried=1) plus main's reads recorded above (the trail rows' paid-press no-fresh-hint gloss, the `momentum_glow=` key and sentence and the inflight_forgotten edge list; the post row's NARROWER no-bridge=1, its THIRD and FOURTH outcomes and their test needles) — `xtask gate help-surfaces --diff` against main's read (matched at d68552079) shows round 10's two hunks and against round 10's read (matched at a4a023d4a) main's three, nothing else — checked against the merged handoff_carry.rs (CARRY_ARCHIVE_BYTES, TAIL_TURNS), alt_archive.rs (REANCHOR_SCREENS, carry_reach, import) and control_session.rs cmd_history, untouched by main, and against cursor_glow.rs seam_licensed (a Rainbow Kitty typed stamp licenses only while the press ring owes a cell), TrailStatus::line (momentum_glow= after momentum_display=), forget_typed_credits' edges and fabric.rs cmd_post's four wait outcomes, untouched by round 10; no claim contradicted; both goldens regenerated as a pair at the merge and unchanged 2026-09-14 (merge of the new-line fade round): the ONE prose change is the `ribbon_retired=` sentence, which now says the count includes cells RELEASED to the swoosh when their text went, as well as cells retired on the fast melt when it was replaced — read against Engine::witness_rows and Status::retired (rainbow_kitty/mod.rs), which count both on the same tally.; 2026-09-14 read of the round-13 additions (the `status` row's fabric=<connected|stalled|disconnected|absent>, fabric_rtt_ms= and fabric_link_age_ms= sentences, the `fabric` row's state/reason/rtt_ms/link_age_ms reply shape, and the new bridge-only `link` row) against aterm-gui fabric.rs fabric_state, fabric_link_facts, fabric_status_tail, bridge_attached (an attach stores STALLED with reason=starting and no numbers), link_report (accepted only for the owning generation, dropped once DISCONNECTED, CONNECTED/STALLED stored under the generation lock, every registered session woken on the up-to-down transition) and cmd_link (the grammar, `OK stale=1` for a ghost lane), control.rs fabric_status_line and dispatch_bridge_verb's `link` arm, session_status.rs's record tail, and aterm-link bridge.rs LinkReport::ack_wants_report / down_wants_report (change, more-than-2x past LINK_MOVE_GAP = 250 ms, or LINK_REFRESH = 2 s), link_reason's tokens, ACK_DEADLINE = 5 s and RECONNECT_MIN = 100 ms / RECONNECT_MAX = 5 s; goldens regenerated as a pair; SHORT_CATALOG_MAX_BYTES raised to 9856 for the row with the same accounting as the three raises before it; no claim contradicted; 2026-09-14 v0.86 release-candidate merge: this file now carries BOTH halves of prose — upstream's and this candidate's — each read in full by its own author against the same handlers, neither contradicting the other, and this row records the UNION's hash; AND UPSTREAM'S READ OF THE SAME PROSE, kept rather than dropped: catalog read in full against the handlers by aterm-help-surfaces-read on 2026-09-10; the fabric row and the reworded hold row read against dispatch_fabric_verb / dispatch_hold_verb and the access-set pins by verify:F1 and verify:F2 of aterm-fabric-attach-round-3 on 2026-09-10; goldens regenerated as a pair from the merged catalog on 2026-09-11; merged with origin/main's own 2026-09-11 re-read of this file, conflicts resolved by the orchestrator (upstream wording kept where both sides fixed the same claim). 2026-09-12: the ONE prose change since that read is SHORT_CATALOG_MAX_BYTES' raise paragraph, read in full and MEASURED rather than recalled — aterm-gui's bare cmd_help is 9 610 B against the new 9 728 ceiling, VERBS is 101 rows, the summary rows alone are 9 238 B; the fabric, fx and hold rows and the OpClass/Access/Framing/Target docs re-read against dispatch_fabric_verb, cmd_fabric/cmd_fabric_attach/fabric_status_line, dispatch_hold_verb/fabric::cmd_hold and the access-set pin, the other 98 catalog rows sampled. One contradicted claim fixed in the commit that updated this row — Access::OwnerOnly called `hold` the ONE member whose handler tells the two owner-class scopes apart, and `fabric`'s handler is a second (Scope::Owner exactly, the bridge refused), as aterm-gui's own is_owner_class doc already records; 2026-09-13 (UTC; 2026-09-12 local) re-read of the trail row's paste-sweep additions (the licence= classes, program-row, the inserts_* status keys, which inputs stamp an insert and which stay dark, and the `inserts_delivered>0 inserts_lit=0` reading after a drop) against aterm-effects cursor_glow.rs AdmissionRecord::line/LICENCE_*, move_licensed, insert_echo/insert_rewrite/INSERT_HINT_FRESH, note_insert_delivered/lay_insert/retract_insert/insert_tally and TrailStatus::line, app_render.rs tick_cursor_fx's Rainbow-Kitty-gated delivery prelude, app_input.rs input_paste and insert_gesture_armed, lib.rs drop_file/deliver_paste, and control.rs's flagless, front-routed, background, guarded and run_feed_bin_routed paste-bin routes, fixing the contradictions in the row (paste-bin into the tab on screen stamps an insert, not nothing; the insert class and counters are Rainbow Kitty only; `A move paints only if a keypress LICENSED it` now admits the delivered insert; the key class gains the composer newline and ⌃V's gesture; inserts_delivered counts a bare Tab and ⌃V too; last_insert_cells is the 32-cell bound when unpriced; the drop reading covers an echo never seen inside the 2 s window) and regenerating both goldens as a pair; the 0.84 train's release-candidate merge (this machine's provenance/repair/TCC work over the peers' 2026-09-12 re-read at the tip) moved the bytes once more — both halves were read by their authors as recorded, the merged file is their union, and this row records the union's hash; 2026-09-13 read of the prose ca54aaadd moved since the recorded read (shown by `xtask gate help-surfaces --diff`, matched at 0f7667e8a) — the trail row's `trail status` key list gaining `park_returns= park_flushed=` and its new HELD PARKS sentence — against aterm-effects cursor_glow.rs TrailStatus::line (the two keys last, after swallowed_no_echo=), InFlightTally's park_returns/park_flushed, HeldPark, park_candidate (Rainbow Kitty, same-row backward, a fresh stamp or presses in flight), spawn's RETURN arm (a forward move from the landing past the origin within TYPE_HINT_FRESH = 0.25 s, park_return_paid, judged origin -> target) and flush_park, and the flushes at the stale-park tick, note_backspace, note_scroll and the next other move; no claim contradicted; 2026-09-13 read of the prose feat/round-7-offscreen moved (shown by `xtask gate help-surfaces --diff`, matched at ca54aaadd) — the new `offscreen` row (Read, Lines, Session; the summary, and the detail sentence by sentence) and the `history` row's arch= clause — against aterm-gui control_query.rs offscreen_args/cmd_offscreen/format_offscreen_reply/OFFSCREEN_DEFAULT_MAX, aterm-core alt_archive.rs (ALT_ARCHIVE_DEFAULT_BUDGET, env_opted_out's 0/off/false/no, AltArchive::read and wipe, the gap kinds, the ESU commit and the 16 ms epilogue fallback), control.rs json_unsupported and control_session.rs cmd_history's arch= field; two slips fixed before this row (a committed frame was only `a DEC 2026 close`, where an app that never sends one is committed at the batch epilogue at most once per 16 ms; lost= counted evicted rows only, where a reset or turning the archive off wipes rows into it too) and both goldens regenerated as a pair; 2026-09-13 merge of main into feat/round-7-offscreen: main's drift-sweep re-read on 2026-09-13 (lane small-b) found the trail licence= row saying an unpaid press logs `key` and fixed it; `gate help-surfaces --diff` at the merge shows the merged prose is exactly round 7's read offscreen row and history arch= clause plus main's trail licence=/decline-reasons correction and its rainbow-kitty v2_ sentence (added 2026-09-12, before main's recorded read); recorded at the merge; 2026-09-14 read of the prose round 10 moved (shown by `xtask gate help-surfaces --diff`, matched at bd8f70ca9) — the offscreen row's `in memory` (no longer `only`), its self-update sentence (the origin kept; the rows after the last 8 submitted turns' marks and at least the running app's last 8 screens, the newest without a turn, up to 1 MiB with older ones in lost=; a new origin when not carried; the resize breaks= gap) and breaks='s scroll-back clause, and the history row's carried=1 sentence — against aterm-gui handoff_carry.rs export/tail_from/TAIL_TURNS/CARRY_ARCHIVE_BYTES/encode_within/decode, aterm-core alt_archive.rs carry_head, carry_reach (REANCHOR_SCREENS screens, never below the app run's floor, and the re-shown run), carry_rows, import and shift_down's Jump gap on a scroll-back past the oldest retained row, control_session.rs cmd_history (` carried=1` before text=) and raise_turn_ids, turn_ledger.rs TurnRecord::carried, and seamless.rs take_incoming's next_turn_id raise; one slip fixed before this row (the carry was said to reach at least the last 8 screens, where carry_reach stops at the running app's floor — now the running app's last 8 screens) and both goldens regenerated as a pair; 2026-09-13 release-candidate merge of the leftovers lane over the peers' round-7/round-8 reads: the merged prose is the UNION of the two halves — main's `offscreen` row and `history` arch= clause (read by round 7's author, recorded above) and the leftovers lane's `momentum_glow=` key and the absorbed rainbow-kitty v2_ sentence (read by that lane, recorded next) — each half read in full by its author against the code it describes, neither contradicted by the other, and this row records the UNION's hash: re-read on 2026-09-13 by drift sweep of 2026-09-13 (lane small-b); the trail licence= roster said an unpaid press logs `key` and is fixed in that commit; that read recorded 212628b6b90c5780 (834cd5f9f), and main 2f15705bc was ALREADY red on this row: the wrapped-row band merge (169dafa7b) had added a sentence to the `trail status` row that nobody read — `While rainbow kitty owns the frame the row ends with v2_quads= v2_halos= v2_stars= v2_meteors= v2_bridged= ribbon_retired=` and its glosses; 2026-09-13 leftovers lane: TWO prose changes since that read, both read here: (1) the `momentum_glow=` key in the backticked run and its sentence (MomentumGlow::value beside the cat's `momentum=`), against TrailStatus::line in cursor_glow.rs and App::trail_status in app_render.rs (`ws.momentum_glow.value(now, MOMENTUM_GLOW_TAU_S)`), pinned by control.rs trail_status_help_enumerates_exactly_the_keys_the_row_emits; (2) the absorbed v2 sentence, against TrailStatus::line_v2 in cursor_glow.rs (appends exactly ` v2_quads= v2_halos= v2_stars= v2_meteors= v2_bridged= ribbon_retired=` from rk::Status, and only when CursorGlow::v2_status is Some — `self.v2.engaged()`, so only while rainbow kitty owns the frame, at the tail), rainbow_kitty/mod.rs Status (quads/halos written this frame, stars and meteors live; `bridged` = cells the echo ledger relit for a late echo the admission ring scored declined; `retired` = ribbon cells retired by CONTENT over the engine's life — Engine::witness_rows when the glyph under a cell changed or went, Engine::retire_row when the caret was seen on another row through a declined move — cumulative across Engine::reset) and app_render.rs (per-window `ws.cursor_glow`, so `the window's cumulative count`); no claim contradicted; 2026-09-13 (2026-09-14 UTC) read of the prose 9758cd022 moved since the recorded read (shown by `xtask gate help-surfaces --diff`, matched at 0333f4a441d2) — the `post` row's rewritten `no-bridge=1` paragraph (NARROWER rather than the opposite, not a verdict on the message, `fabric attach <command...>` arms a supervisor and that same outbox drains, further posts only until `ERR outbox full`) and its new THIRD OUTCOME sentence (`ERR timeout id=<n>`, the `--wait` expiring with no landing reported, queued exactly like the other two), plus the two `contains` assertions in the_fabric_rows_state_the_bounds_they_are_held_to that pin `none is coming ON ITS OWN` and `ERR timeout id=<n>` — against aterm-gui fabric.rs cmd_post (its option tokens are to/kind/re/dl/via/--wait/len only, so there is no idempotency key that could collapse a re-post; the refusal at the door answers `ERR outbox full queued= bytes=` at OUTBOX_CAP = 128 or OUTBOX_BYTES_MAX = 4 MiB measured by queued_load + caller_sized_bytes; the PostRow is pushed BEFORE the wait loop), that loop's deadline arm returning `ERR timeout id={id}` with the row untouched (only trim_retired_posts removes rows and only ones carrying `off` or `dead`, so a timed-out post is still queued and still drained), fabric_wait_refusal and bridge_reachable (`no-bridge=1` is exactly `!supervised && state == absent`), note_bridge_supervised, fabric_launch.rs spawn_supervisor/arm/preflight and control.rs cmd_fabric_attach (`fabric attach <command...>` is the one seam that starts a supervise thread at RUNTIME and flips the latch), and aterm-link bridge.rs drain_outbox, whose bare `outbox` peek takes exactly the `off.is_none() && !dead` rows and runs on the reconcile path a freshly attached bridge takes; no claim contradicted — the retired `Nothing will publish it, no answer can arrive` WAS false, and control.rs fabric_attach_arms_the_supervisor_of_a_running_instance_once_and_for_owner_only shows the flip (`no-bridge=1` at id=1 and id=2, `queued=1` at id=3 after the attach, the two earlier posts still queued); one narrowness left standing rather than fixed here: the row gives an instance with no `[fabric] command` as the cause, where the predicate is bridge_reachable, so a CONFIGURED command whose program fails arm's preflight answers `no-bridge=1` too (that same test's `/nonexistent/aterm-link`, id=2), and the remedy the row prints is the one spawn_supervisor's own warn line names for that case as well AND the same completeness slip the manual.rs read found was fixed here in this commit: the row enumerated three outcomes and `cmd_post` has a fourth — `ERR <reason> id=<n>` for a post the bridge retired (`unroutable`/`ambiguous`/`undeliverable`), which `outbox` then omits, so it is the one outcome that does NOT mean queued; the row now says so and `the_fabric_rows_state_the_...` pins `ERR <reason> id=<n>` and `unroutable` beside the other three. The hash recorded here is the CORRECTED row, and both goldens were regenerated as a pair; 2026-09-14 drift sweep (lane cli-and-types, 25 claims checked over the group): re-read against the code that moved under it since this row's anchor commit; every candidate went to a verifier prompted to REFUTE it — no claim contradicted the moved code The 2026-09-14 merge of the audit2 lane over the cli-and-types drift sweep moved the bytes once more; `--diff` shows exactly two prose lines, both the audit lane's own and read here against the code: the `no-fresh-hint` gloss's Rainbow Kitty clause (seam_licensed gates a fresh typed stamp on typed_credits_within >= 1 under GlowStyle::RainbowKitty, logged DECLINE_NO_FRESH_HINT; pinned by a_caret_that_advances_with_no_unpaid_press_still_buys_nothing), and the `inflight_forgotten=` edge list (forget_typed_credits at note_kill, at the keyless backward/cross-row refusal, and at the licensed move when the licence is not Typed, the hop is cross-row and unhinted, credit_starved or typed_over_cap — a same-row `no-fresh-hint` refusal is none of those and keeps the pool; a glyph's echo with Enter fresh takes the Typed licence, pinned by a_glyph_echo_does_not_spend_the_enter_pressed_behind_it); no claim contradicted; 2026-09-14 merge read of feat/round-10-carry over main: the merged prose is exactly round 10's offscreen and history rows (`in memory`, the self-update carry sentence, breaks='s scroll-back clause, carried=1) plus main's reads recorded above (the trail rows' paid-press no-fresh-hint gloss, the `momentum_glow=` key and sentence and the inflight_forgotten edge list; the post row's NARROWER no-bridge=1, its THIRD and FOURTH outcomes and their test needles) — `xtask gate help-surfaces --diff` against main's read (matched at d68552079) shows round 10's two hunks and against round 10's read (matched at a4a023d4a) main's three, nothing else — checked against the merged handoff_carry.rs (CARRY_ARCHIVE_BYTES, TAIL_TURNS), alt_archive.rs (REANCHOR_SCREENS, carry_reach, import) and control_session.rs cmd_history, untouched by main, and against cursor_glow.rs seam_licensed (a Rainbow Kitty typed stamp licenses only while the press ring owes a cell), TrailStatus::line (momentum_glow= after momentum_display=), forget_typed_credits' edges and fabric.rs cmd_post's four wait outcomes, untouched by round 10; no claim contradicted; both goldens regenerated as a pair at the merge and unchanged 2026-09-14 (merge of the new-line fade round): the ONE prose change is the `ribbon_retired=` sentence, which now says the count includes cells RELEASED to the swoosh when their text went, as well as cells retired on the fast melt when it was replaced — read against Engine::witness_rows and Status::retired (rainbow_kitty/mod.rs), which count both on the same tally.; 2026-09-14 read of the round-13 additions (the `status` row's fabric=<connected|stalled|disconnected|absent>, fabric_rtt_ms= and fabric_link_age_ms= sentences, the `fabric` row's state/reason/rtt_ms/link_age_ms reply shape, and the new bridge-only `link` row) against aterm-gui fabric.rs fabric_state, fabric_link_facts, fabric_status_tail, bridge_attached (an attach stores STALLED with reason=starting and no numbers), link_report (accepted only for the owning generation, dropped once DISCONNECTED, CONNECTED/STALLED stored under the generation lock, every registered session woken on the up-to-down transition) and cmd_link (the grammar, `OK stale=1` for a ghost lane), control.rs fabric_status_line and dispatch_bridge_verb's `link` arm, session_status.rs's record tail, and aterm-link bridge.rs LinkReport::ack_wants_report / down_wants_report (change, more-than-2x past LINK_MOVE_GAP = 250 ms, or LINK_REFRESH = 2 s), link_reason's tokens, ACK_DEADLINE = 5 s and RECONNECT_MIN = 100 ms / RECONNECT_MAX = 5 s; goldens regenerated as a pair; SHORT_CATALOG_MAX_BYTES raised to 9856 for the row with the same accounting as the three raises before it; no claim contradicted; 2026-09-15 the privacy row re-read against control_privacy.rs after 47198639f: its `which services it covers is not measured here, so fda_scope=unknown` clause contradicted covers_split/observed_fda_scope, which render `fda_scope=this_process` and `covers=app-data` for a Granted probe; reworded to the two things a grant now establishes and the three it still does not (other services unmeasured, adopted sessions inherit nothing, folder rows unknown), the scope-ruling sentence kept, the test's needles moved with it, both goldens regenerated; no other claim contradicted; 2026-09-14 v0.86 candidate merge: the file carries BOTH halves of prose — upstream's and this candidate's — each read in full by its author against the same handlers, neither contradicting the other, and this row records the UNION's hash; AND UPSTREAM'S READ OF THE SAME PROSE, kept: catalog read in full against the handlers by aterm-help-surfaces-read on 2026-09-10; the fabric row and the reworded hold row read against dispatch_fabric_verb / dispatch_hold_verb and the access-set pins by verify:F1 and verify:F2 of aterm-fabric-attach-round-3 on 2026-09-10; goldens regenerated as a pair from the merged catalog on 2026-09-11; merged with origin/main's own 2026-09-11 re-read of this file, conflicts resolved by the orchestrator (upstream wording kept where both sides fixed the same claim). 2026-09-12: the ONE prose change since that read is SHORT_CATALOG_MAX_BYTES' raise paragraph, read in full and MEASURED rather than recalled — aterm-gui's bare cmd_help is 9 610 B against the new 9 728 ceiling, VERBS is 101 rows, the summary rows alone are 9 238 B; the fabric, fx and hold rows and the OpClass/Access/Framing/Target docs re-read against dispatch_fabric_verb, cmd_fabric/cmd_fabric_attach/fabric_status_line, dispatch_hold_verb/fabric::cmd_hold and the access-set pin, the other 98 catalog rows sampled. One contradicted claim fixed in the commit that updated this row — Access::OwnerOnly called `hold` the ONE member whose handler tells the two owner-class scopes apart, and `fabric`'s handler is a second (Scope::Owner exactly, the bridge refused), as aterm-gui's own is_owner_class doc already records; 2026-09-13 (UTC; 2026-09-12 local) re-read of the trail row's paste-sweep additions (the licence= classes, program-row, the inserts_* status keys, which inputs stamp an insert and which stay dark, and the `inserts_delivered>0 inserts_lit=0` reading after a drop) against aterm-effects cursor_glow.rs AdmissionRecord::line/LICENCE_*, move_licensed, insert_echo/insert_rewrite/INSERT_HINT_FRESH, note_insert_delivered/lay_insert/retract_insert/insert_tally and TrailStatus::line, app_render.rs tick_cursor_fx's Rainbow-Kitty-gated delivery prelude, app_input.rs input_paste and insert_gesture_armed, lib.rs drop_file/deliver_paste, and control.rs's flagless, front-routed, background, guarded and run_feed_bin_routed paste-bin routes, fixing the contradictions in the row (paste-bin into the tab on screen stamps an insert, not nothing; the insert class and counters are Rainbow Kitty only; `A move paints only if a keypress LICENSED it` now admits the delivered insert; the key class gains the composer newline and ⌃V's gesture; inserts_delivered counts a bare Tab and ⌃V too; last_insert_cells is the 32-cell bound when unpriced; the drop reading covers an echo never seen inside the 2 s window) and regenerating both goldens as a pair; the 0.84 train's release-candidate merge (this machine's provenance/repair/TCC work over the peers' 2026-09-12 re-read at the tip) moved the bytes once more — both halves were read by their authors as recorded, the merged file is their union, and this row records the union's hash; 2026-09-13 read of the prose ca54aaadd moved since the recorded read (shown by `xtask gate help-surfaces --diff`, matched at 0f7667e8a) — the trail row's `trail status` key list gaining `park_returns= park_flushed=` and its new HELD PARKS sentence — against aterm-effects cursor_glow.rs TrailStatus::line (the two keys last, after swallowed_no_echo=), InFlightTally's park_returns/park_flushed, HeldPark, park_candidate (Rainbow Kitty, same-row backward, a fresh stamp or presses in flight), spawn's RETURN arm (a forward move from the landing past the origin within TYPE_HINT_FRESH = 0.25 s, park_return_paid, judged origin -> target) and flush_park, and the flushes at the stale-park tick, note_backspace, note_scroll and the next other move; no claim contradicted; 2026-09-13 read of the prose feat/round-7-offscreen moved (shown by `xtask gate help-surfaces --diff`, matched at ca54aaadd) — the new `offscreen` row (Read, Lines, Session; the summary, and the detail sentence by sentence) and the `history` row's arch= clause — against aterm-gui control_query.rs offscreen_args/cmd_offscreen/format_offscreen_reply/OFFSCREEN_DEFAULT_MAX, aterm-core alt_archive.rs (ALT_ARCHIVE_DEFAULT_BUDGET, env_opted_out's 0/off/false/no, AltArchive::read and wipe, the gap kinds, the ESU commit and the 16 ms epilogue fallback), control.rs json_unsupported and control_session.rs cmd_history's arch= field; two slips fixed before this row (a committed frame was only `a DEC 2026 close`, where an app that never sends one is committed at the batch epilogue at most once per 16 ms; lost= counted evicted rows only, where a reset or turning the archive off wipes rows into it too) and both goldens regenerated as a pair; 2026-09-13 merge of main into feat/round-7-offscreen: main's drift-sweep re-read on 2026-09-13 (lane small-b) found the trail licence= row saying an unpaid press logs `key` and fixed it; `gate help-surfaces --diff` at the merge shows the merged prose is exactly round 7's read offscreen row and history arch= clause plus main's trail licence=/decline-reasons correction and its rainbow-kitty v2_ sentence (added 2026-09-12, before main's recorded read); recorded at the merge; 2026-09-14 read of the prose round 10 moved (shown by `xtask gate help-surfaces --diff`, matched at bd8f70ca9) — the offscreen row's `in memory` (no longer `only`), its self-update sentence (the origin kept; the rows after the last 8 submitted turns' marks and at least the running app's last 8 screens, the newest without a turn, up to 1 MiB with older ones in lost=; a new origin when not carried; the resize breaks= gap) and breaks='s scroll-back clause, and the history row's carried=1 sentence — against aterm-gui handoff_carry.rs export/tail_from/TAIL_TURNS/CARRY_ARCHIVE_BYTES/encode_within/decode, aterm-core alt_archive.rs carry_head, carry_reach (REANCHOR_SCREENS screens, never below the app run's floor, and the re-shown run), carry_rows, import and shift_down's Jump gap on a scroll-back past the oldest retained row, control_session.rs cmd_history (` carried=1` before text=) and raise_turn_ids, turn_ledger.rs TurnRecord::carried, and seamless.rs take_incoming's next_turn_id raise; one slip fixed before this row (the carry was said to reach at least the last 8 screens, where carry_reach stops at the running app's floor — now the running app's last 8 screens) and both goldens regenerated as a pair; 2026-09-13 release-candidate merge of the leftovers lane over the peers' round-7/round-8 reads: the merged prose is the UNION of the two halves — main's `offscreen` row and `history` arch= clause (read by round 7's author, recorded above) and the leftovers lane's `momentum_glow=` key and the absorbed rainbow-kitty v2_ sentence (read by that lane, recorded next) — each half read in full by its author against the code it describes, neither contradicted by the other, and this row records the UNION's hash: re-read on 2026-09-13 by drift sweep of 2026-09-13 (lane small-b); the trail licence= roster said an unpaid press logs `key` and is fixed in that commit; that read recorded 212628b6b90c5780 (834cd5f9f), and main 2f15705bc was ALREADY red on this row: the wrapped-row band merge (169dafa7b) had added a sentence to the `trail status` row that nobody read — `While rainbow kitty owns the frame the row ends with v2_quads= v2_halos= v2_stars= v2_meteors= v2_bridged= ribbon_retired=` and its glosses; 2026-09-13 leftovers lane: TWO prose changes since that read, both read here: (1) the `momentum_glow=` key in the backticked run and its sentence (MomentumGlow::value beside the cat's `momentum=`), against TrailStatus::line in cursor_glow.rs and App::trail_status in app_render.rs (`ws.momentum_glow.value(now, MOMENTUM_GLOW_TAU_S)`), pinned by control.rs trail_status_help_enumerates_exactly_the_keys_the_row_emits; (2) the absorbed v2 sentence, against TrailStatus::line_v2 in cursor_glow.rs (appends exactly ` v2_quads= v2_halos= v2_stars= v2_meteors= v2_bridged= ribbon_retired=` from rk::Status, and only when CursorGlow::v2_status is Some — `self.v2.engaged()`, so only while rainbow kitty owns the frame, at the tail), rainbow_kitty/mod.rs Status (quads/halos written this frame, stars and meteors live; `bridged` = cells the echo ledger relit for a late echo the admission ring scored declined; `retired` = ribbon cells retired by CONTENT over the engine's life — Engine::witness_rows when the glyph under a cell changed or went, Engine::retire_row when the caret was seen on another row through a declined move — cumulative across Engine::reset) and app_render.rs (per-window `ws.cursor_glow`, so `the window's cumulative count`); no claim contradicted; 2026-09-13 (2026-09-14 UTC) read of the prose 9758cd022 moved since the recorded read (shown by `xtask gate help-surfaces --diff`, matched at 0333f4a441d2) — the `post` row's rewritten `no-bridge=1` paragraph (NARROWER rather than the opposite, not a verdict on the message, `fabric attach <command...>` arms a supervisor and that same outbox drains, further posts only until `ERR outbox full`) and its new THIRD OUTCOME sentence (`ERR timeout id=<n>`, the `--wait` expiring with no landing reported, queued exactly like the other two), plus the two `contains` assertions in the_fabric_rows_state_the_bounds_they_are_held_to that pin `none is coming ON ITS OWN` and `ERR timeout id=<n>` — against aterm-gui fabric.rs cmd_post (its option tokens are to/kind/re/dl/via/--wait/len only, so there is no idempotency key that could collapse a re-post; the refusal at the door answers `ERR outbox full queued= bytes=` at OUTBOX_CAP = 128 or OUTBOX_BYTES_MAX = 4 MiB measured by queued_load + caller_sized_bytes; the PostRow is pushed BEFORE the wait loop), that loop's deadline arm returning `ERR timeout id={id}` with the row untouched (only trim_retired_posts removes rows and only ones carrying `off` or `dead`, so a timed-out post is still queued and still drained), fabric_wait_refusal and bridge_reachable (`no-bridge=1` is exactly `!supervised && state == absent`), note_bridge_supervised, fabric_launch.rs spawn_supervisor/arm/preflight and control.rs cmd_fabric_attach (`fabric attach <command...>` is the one seam that starts a supervise thread at RUNTIME and flips the latch), and aterm-link bridge.rs drain_outbox, whose bare `outbox` peek takes exactly the `off.is_none() && !dead` rows and runs on the reconcile path a freshly attached bridge takes; no claim contradicted — the retired `Nothing will publish it, no answer can arrive` WAS false, and control.rs fabric_attach_arms_the_supervisor_of_a_running_instance_once_and_for_owner_only shows the flip (`no-bridge=1` at id=1 and id=2, `queued=1` at id=3 after the attach, the two earlier posts still queued); one narrowness left standing rather than fixed here: the row gives an instance with no `[fabric] command` as the cause, where the predicate is bridge_reachable, so a CONFIGURED command whose program fails arm's preflight answers `no-bridge=1` too (that same test's `/nonexistent/aterm-link`, id=2), and the remedy the row prints is the one spawn_supervisor's own warn line names for that case as well AND the same completeness slip the manual.rs read found was fixed here in this commit: the row enumerated three outcomes and `cmd_post` has a fourth — `ERR <reason> id=<n>` for a post the bridge retired (`unroutable`/`ambiguous`/`undeliverable`), which `outbox` then omits, so it is the one outcome that does NOT mean queued; the row now says so and `the_fabric_rows_state_the_...` pins `ERR <reason> id=<n>` and `unroutable` beside the other three. The hash recorded here is the CORRECTED row, and both goldens were regenerated as a pair; 2026-09-14 drift sweep (lane cli-and-types, 25 claims checked over the group): re-read against the code that moved under it since this row's anchor commit; every candidate went to a verifier prompted to REFUTE it — no claim contradicted the moved code The 2026-09-14 merge of the audit2 lane over the cli-and-types drift sweep moved the bytes once more; `--diff` shows exactly two prose lines, both the audit lane's own and read here against the code: the `no-fresh-hint` gloss's Rainbow Kitty clause (seam_licensed gates a fresh typed stamp on typed_credits_within >= 1 under GlowStyle::RainbowKitty, logged DECLINE_NO_FRESH_HINT; pinned by a_caret_that_advances_with_no_unpaid_press_still_buys_nothing), and the `inflight_forgotten=` edge list (forget_typed_credits at note_kill, at the keyless backward/cross-row refusal, and at the licensed move when the licence is not Typed, the hop is cross-row and unhinted, credit_starved or typed_over_cap — a same-row `no-fresh-hint` refusal is none of those and keeps the pool; a glyph's echo with Enter fresh takes the Typed licence, pinned by a_glyph_echo_does_not_spend_the_enter_pressed_behind_it); no claim contradicted; 2026-09-14 merge read of feat/round-10-carry over main: the merged prose is exactly round 10's offscreen and history rows (`in memory`, the self-update carry sentence, breaks='s scroll-back clause, carried=1) plus main's reads recorded above (the trail rows' paid-press no-fresh-hint gloss, the `momentum_glow=` key and sentence and the inflight_forgotten edge list; the post row's NARROWER no-bridge=1, its THIRD and FOURTH outcomes and their test needles) — `xtask gate help-surfaces --diff` against main's read (matched at d68552079) shows round 10's two hunks and against round 10's read (matched at a4a023d4a) main's three, nothing else — checked against the merged handoff_carry.rs (CARRY_ARCHIVE_BYTES, TAIL_TURNS), alt_archive.rs (REANCHOR_SCREENS, carry_reach, import) and control_session.rs cmd_history, untouched by main, and against cursor_glow.rs seam_licensed (a Rainbow Kitty typed stamp licenses only while the press ring owes a cell), TrailStatus::line (momentum_glow= after momentum_display=), forget_typed_credits' edges and fabric.rs cmd_post's four wait outcomes, untouched by round 10; no claim contradicted; both goldens regenerated as a pair at the merge and unchanged 2026-09-14 (merge of the new-line fade round): the ONE prose change is the `ribbon_retired=` sentence, which now says the count includes cells RELEASED to the swoosh when their text went, as well as cells retired on the fast melt when it was replaced — read against Engine::witness_rows and Status::retired (rainbow_kitty/mod.rs), which count both on the same tally.; 2026-09-14 read of the round-13 additions (the `status` row's fabric=<connected|stalled|disconnected|absent>, fabric_rtt_ms= and fabric_link_age_ms= sentences, the `fabric` row's state/reason/rtt_ms/link_age_ms reply shape, and the new bridge-only `link` row) against aterm-gui fabric.rs fabric_state, fabric_link_facts, fabric_status_tail, bridge_attached (an attach stores STALLED with reason=starting and no numbers), link_report (accepted only for the owning generation, dropped once DISCONNECTED, CONNECTED/STALLED stored under the generation lock, every registered session woken on the up-to-down transition) and cmd_link (the grammar, `OK stale=1` for a ghost lane), control.rs fabric_status_line and dispatch_bridge_verb's `link` arm, session_status.rs's record tail, and aterm-link bridge.rs LinkReport::ack_wants_report / down_wants_report (change, more-than-2x past LINK_MOVE_GAP = 250 ms, or LINK_REFRESH = 2 s), link_reason's tokens, ACK_DEADLINE = 5 s and RECONNECT_MIN = 100 ms / RECONNECT_MAX = 5 s; goldens regenerated as a pair; SHORT_CATALOG_MAX_BYTES raised to 9856 for the row with the same accounting as the three raises before it; no claim contradicted; 2026-09-15 recovery audit: read the update synopsis and full help against cmd_update and update_control::Snapshot. Apply acknowledges a request before asynchronous validation; checks use the current GUI source and notify on every completion. relaunch_ready indicates stage existence, the enumerated apply_posture values match the exact-artifact projection including unknown/unreconciled, and apply_policy_reason is emitted only for a current policy block. No readiness or timing guarantee is asserted; both generated help fixtures are regenerated together. The subsequent privacy-help merge was read against observed_fda_scope, covers_split and per-session privacy observations: a host grant establishes this_process and app-data coverage, other services remain unmeasured/uncovered, and adopted sessions and folder rows retain unknown access. The row records both changes.; 2026-09-14 v0.86 candidate merge: this file carries BOTH halves of prose — upstream's and the candidate's — each read by its own author against the same handlers, and the row records the UNION's hash; 2026-09-15 read at the landing of the erase pending-wrap fix, over f3a5467ed: read the version verb's new key detail (flavor=t|r, trust= as Trust's own version and none on upstream, rust_compat= as the compatible Rust release, trustc=/trustc_commit=/trustc_host= as release token, commit and host, the v0.10.0 rename, and trustc= equal to rust_compat=) against build_info control_line, compiler_release, compiler_commit_short, compiler_probe parse_rustc_vv and detect_flavor, build.rs, both version dispatch sites in aterm-gui control.rs, the CHANGELOG 0.10.0 entry, the two help goldens and this box's live rustc -vV, and no claim contradicted the code.; 2026-09-15 merge read of main (round 15) over origin/main 58bb2fc63: the merged prose is round 15's read change plus main's, and it hashes to this row's recorded value, so no prose moved at the merge; 2026-09-15 read of the new `kitty` verb entry against its handlers: the summary `kitty [wear <key>]` and every field of the two documented row shapes read against App::kitty_collection_rows (`cat key={} coat={} iris={} age={} seen={} worn={worn}`, `seen=` being the roster row's `count`) and App::wear_kitty_on_front (`worn key={key} coat={} iris={}`) in crates/aterm-gui/src/app_input.rs; the `worn=1` claim against KittyLogHost::wearable, which derives the worn key from the cached `favourite` the companion reads rather than a second roster election (two pins inside one RFC3339 second tie, and the key breaks the tie the other way — found and fixed while writing the test); `the key is the row's own key=` against KittyLog::wear's `item.key == key`; the Favourite-This-Kitty claim against App::promotable_kitty (kitty_tenure.worn().map(|i| i.look).unwrap_or(self.launch_kitty)); `elects by GREATEST pin stamp` against KittyLog::favourite_look's max_by over `favourite`, and `survives a merge` against max_ts in favourite_collectible/merge_collectible; the three refusal cases against App::wear_kitty's sparkle gate, cfg.feline gate and None arm. `Write` rather than ConfigWrite is pinned by control.rs's required_op census. No claim contradicted; 2026-09-15 (2026-09-16 UTC) read of the `metrics` row's echo paragraph, added by fe8ec4d58 (shown by `xtask gate help-surfaces --diff`, matched at b3a62ce1d), against crates/aterm-gui/src/control_query.rs cmd_metrics and crates/aterm-gui/src/echo_rtt.rs percentile_fields_text — the echo fragment does ride the PLAIN summary line, and the who-owes-the-time reading matches the episode the code itself records (n_echo=499 echo_p95_ms=75.69 echo_p99_ms=150.04 against aterm's own key_write_p99_ms=6.29). ONE SLIP FIXED in the commit that updated this row: the row said the line `ends ... then the CHILD's own round trip`, but the summary's LAST fragment is watchdog::turn_census_fields_text (e50cbbff4), spliced after echo_rtt's — the row now names that turn census and its seven fields as the end of the line, and lists echo_total= with the rest of the ledger percentile_fields_text actually prints; 2026-09-16 read of the tab entry's rewritten reply sentence a29417119 changed (OK active count only when the action HAPPENED, ERR reason when it did not: an index no tab holds, or a tab host that declined the close — a native document waiting on a durable checkpoint, or a close deferred behind a pending update handoff) against apply_tab_cmd_in's no_such bounds refusals for Select, Close(N) and both ends of Move; close_active_native_tab's Err arms (tab close deferred until update child rollback, returned behind defer_pending_update_handoff_teardown, which queues the mutation onto the pending handoff's teardown and applies it after the cancellation lands; native document close is waiting for a durable checkpoint; terminal close requires confirmation); and the regression test an_out_of_range_tab_action_refuses_instead_of_reporting_ok, which pins each refusal beside an in-range positive control; the headless sentence is unchanged and still holds; no claim contradicted; 2026-09-17 read of the round-18 identities rows: the recut `spawn` summary and its identity= sentences against control_media::parse_spawn_args/resolve_spawn_identity and control::escalated_op's fence; the `sessions` summary/detail `identity=` against control_session::sessions_lines; the `status` reply list against session_status::session_status_record; the new `identities` row's reply table against agent_identity::identities_reply arm by arm (list; one, `OK <n>` counting the agent lines since the client streams exactly n; forget unconfirmed, in use, removed, unknown, usage); the SHORT_CATALOG_MAX_BYTES paragraph against the short help measured on the live headless instance; 2026-09-17 read of the accounting paragraph's recount (--diff matched at 74d637316): 105 entries (help_catalog_full.txt carries 105 rows) and `OK 108` against control.rs's VERBS.len() + 3 status line; no claim contradicted",
    ),
    (
        "crates/aterm-verify/src/cli.rs",
        "ef2c009f74b5a4d2",
        "2026-09-17",
        "re-read on 2026-09-12 against the code that moved under it since the roster was minted, by the 2026-09-12 drift sweep (lane small-2); the 45-minute ceiling was two raises stale and is now DERIVED from DEFAULT_CHILD_CEILING; 2026-09-13 read (2026-09-14 UTC) of the prose b734f1069 added (shown by `xtask gate help-surfaces --diff`, matched at 56f7c5de5) — the --in-place flag, its usage example and help paragraph, the module doc, Args.in_place and the new test's strings — against parse's --in-place arm (a bare flag; --in-place=yes is Unknown), main.rs choose_source (--in-place and --selftest run in place, and so does a root that is not a git checkout, with a header note; otherwise snapshot::prepare at ATERM_VERIFY_SNAPSHOT or default_root's <root>-verify.noindex), snapshot::sync (HEAD, git diff --binary HEAD, untracked non-ignored files, flag-hidden edits), identity::Tripwire::check (source only for a git tree, toolchain always) and exec::Timings (a side channel; the ladder bytes are the same with or without it); ONE claim was false and was fixed in the commit before this row: the help said every run except --selftest verifies a snapshot and that a moved source tree always makes the run COULD NOT RUN, while a root that is not a git checkout runs in place with no source tripwire — the paragraph now scopes both to a git checkout; 2026-09-14 re-read of the module doc and the --in-place help paragraph against main.rs choose_source, identity::Tripwire::check, lib.rs run's tripped-check arm, verdict::verdict's failure-before-could-not-run order and exec::Timings::create; the mid-run move, snapshot scope and timings claims were corrected on 2026-09-14: the module doc said a run verifies a snapshot by default with no git-checkout scope (now scoped, with --selftest and a non-git root running in place); the help said a moved compiler or source tree makes the run COULD NOT RUN unconditionally, where it stops every stage not yet started and adds a source identity COULD NOT RUN row, so a run with nothing failed ends COULD NOT RUN (exit 3) and one with a FAILED stage keeps FAIL (exit 1); and it said the timings file gets per-child rows, where Timings::create truncates it each run and writes a row per child plus one (stage) row per stage; 2026-09-17 read of the `--changed` entry's pre-push clause (the only prose that moved) against `.githooks/pre-push` as it now stands and against crates/aterm-verify/src/receipt.rs: the hook runs no tier at all and refuses a push whose commit carries no passing receipt, which is what the clause now says; the four flag entries, the exit codes and the env list were re-read against cli::parse and Args and no claim contradicted the code, by lane a-gatetruth",
    ),
    (
        "crates/aterm-verify/src/lib.rs",
        "961ff9e82860b502",
        "2026-09-19",
        "re-read on 2026-09-18: HOOK_CLAIM (the sentence the gate prints to every clone) now names the three pushes the receipt hook admits without a receipt of their own — a tag, the release cutter's claim over origin's tip touching only CHANGELOG.md and RELEASES.ledger, and a clean automatic merge of a receipted commit onto origin's tip whose tree is byte-equal to git's own merge of its parents — beside the ATERM_PUSH_NO_GATE=1 exception; read against .githooks/pre-push (`release_claim`, `gated_merge`, `receipt_passes`, the refs/tags/* arm) and measured by tests/push_gate.rs (18 tests), which runs the shipped hook for each admission and each refusal the sentence implies",
    ),
    (
        "crates/aterm-winsign/src/lib.rs",
        "c64c32cb488903df",
        "2026-09-16",
        "NEW SURFACE, never rostered: `cargo winsign` landed at b83bb56b0 and grew its Trusted Signing lane at a7ad1ad0b. USAGE read in full against parse_flags and run_inner on 2026-09-15: the four advertised verbs (sign/verify/doctor/help — plus `status`, an undocumented alias of verify that the usage block does not claim), `--credentials <file>` against values_from_profile's `winsign_<key> = \"…\"` grammar, the other nine option spellings against KEYS one for one, and the stated precedence flag > ATERM_WINSIGN_<KEY> > profile against resolve_config's first-wins source order; `--lane` against the inference arms (both configured and no --lane is refused by name), `--thumbprint` against the 40-hex-digit gate and sign_args's `/sha1` (the module doc's `never /f <pfx> /p <password>` holds — no lane takes a password), `--timestamp` against default_timestamp (acs.microsoft.com for Trusted Signing, digicert for a store cert), `--signtool` against find_signtool (PATH, then Windows Kits 10 bin newest SDK first) and `--dlib` against find_dlib (newest microsoft.trusted.signing.client under the NuGet cache); the tier prose against tier_from/pins::anchor_active, judge (signed, verifies under `/pa` — verify_args passes it — leaf `Issued to:` equal to the anchor, timestamp required) and report_file's readback after every sign; doctor's `Exit 0 when a sign would run` against its ok-tracking (0 or 2). ONE DEFECT FIXED: the Inactive bullet claimed `Verification never fails a build in this state`, but report_file returns false for an unsigned or untrusted-root exe whatever the tier, so run exits 1 — pinned by verify_reports_an_untrusted_chain_with_exit_one_even_when_inactive and thrown on by apps/aterm-win/build.ps1 -Sign; the bullet now says the TIER refuses nothing while the exit code still reports. ONE DEFECT REPORTED, NOT FIXED HERE (code, not doc, and aterm-winsign is another lane's file this hour): the EXIT block promises `1 … or signing failed`, but a signtool `sign` that runs and exits non-zero leaves run_inner as Err and run() maps every Err to 2, so a real signing failure exits 2 — the code should return 1 there. THAT DEFECT IS NOW FIXED in the same round: a signtool that runs and exits non-zero returns `Ok(1)` after printing its output, so exit 1 means the signature failed and exit 2 stays with \"nothing to sign with\" — pinned both ways by `a_signtool_that_ran_and_refused_is_exit_one_not_two`, whose twin (return `Err` there again) reddens exactly that test. Re-read 2026-09-16 after that change and after the lane that routed every Windows-layout path through `win_join`: USAGE is unchanged by both, the module doc's Inactive bullet is the one edited above, and the EXIT block now matches its handler; SECOND ROW FOR THIS PATH, MERGED HERE VERBATIM 2026-09-16 (it was the R6 DUPLICATE): first read of the crate, for the help-surfaces gate round of 2026-09-15 (2026-09-16 UTC): USAGE and the module doc read against the parser and the verbs — the four synopsis lines against run_inner's verb match (sign, verify, doctor, help; `status` an undocumented alias of verify; a bare argv and --help both reach help), the OPTIONS header against resolve_config(&[flags, env, profile]) (first source wins, so flag > ATERM_WINSIGN_<KEY> > the profile's winsign_<key>) with KEYS, values_from_env and values_from_profile (an unknown winsign_ key is refused, a non-winsign_ key skipped), --lane against resolve_config (a lane is inferred when only one is configured and refused by name when both are), --thumbprint against its 40-hex-digit SHA-1 check, --timestamp against default_timestamp (acs.microsoft.com for Trusted Signing, digicert for a store certificate), --signtool against find_signtool (configured, then PATH, then the newest Windows Kits 10 bin/<version>/<arch>), --dlib against find_dlib (configured, else the newest microsoft.trusted.signing.client under NUGET_PACKAGES or the home .nuget cache), and the two tier paragraphs against tier_from/pins::anchor_active and judge (Active demands signed + verified under /pa + Issued to exactly the anchored publisher + timestamped; Inactive judges every report Ok). THREE SLIPS FIXED in the commit that added this row: (1) the EXIT block said `1 a signature is wrong for the tier, or signing failed`, where a non-zero `signtool sign` returns Err from run_inner and run maps EVERY Err to 2 — exit 1 is only report_file's bad verdict — so EXIT now reads 1 = a verdict was read and it is bad, 2 = no verdict (nothing to sign with, a missing tool, or signtool itself failing); (2) the Tier::Inactive bullet said `Verification never fails a build in this state`, where report_file returns false for an unsigned or untrusted-chain exe in EVERY tier and the crate's own verify_reports_an_untrusted_chain_with_exit_one_even_when_inactive pins that exit 1 — the bullet now says the tier refuses nothing while the Code-Integrity verdict still exits 1; and (3) the `only RealHost touches the machine` sentence was made precise in the same commit: `sign` unlinks its own Trusted Signing metadata file through std::fs, outside the Host seam. 2026-09-16 re-read after the review of that commit, which found slip (2) only half fixed: the SAME contradicted claim still stood in two other pieces of this file's prose — the `Tier::Inactive` variant doc (`No anchor: signing is optional and verification is advisory`) and, user-visibly, the `impl Display for Tier` string that doctor, sign and report_file's UNSIGNED line all print (`signing optional, verification advisory`) — both read again against judge (Inactive is Ok for every report), report_file (false whenever !verified, in EVERY tier) and verify_reports_an_untrusted_chain_with_exit_one_even_when_inactive, and both now say the tier itself refuses nothing while an unsigned or untrusted chain is still reported and exits 1; verification in this tier is not advisory and no prose in the crate says it is any more. The EXIT block was read in the same pass against parse_flags, run_inner, run, resolve_config and doctor: `run` maps EVERY Err to 2, so the 2 line now names the argument and usage refusals it had left out (an unknown verb or option, a flag with no value, no exe named, a path that is not a file, a credentials profile that will not read, and a lane that does not resolve — resolve_config's both-lanes-configured, incomplete-triple, unknown-lane and not-40-hex-digits arms) alongside the missing tool and signtool's own non-zero exit; and the 0 line, which said only `signed and judged good`, now covers what actually returns 0 — sign/verify on a verdict the tier accepts, doctor's READY (`if ok { 0 } else { 2 }`, so NOT READY is a 2) and help (run_inner returns Ok(0) after printing USAGE); 2026-09-15 re-read after the same branch's follow-up commit edited the very prose the first row recorded (the row had been taken from an uncommitted tree): the reworded Tier::Inactive Display string and variant doc, and the rewritten EXIT block, read against run (every Err from run_inner becomes 2), report_file (judge accepts under Inactive, yet an unverified report still answers false, so verify/sign exit 1 in that tier — pinned by verify_reports_an_untrusted_chain_with_exit_one_even_when_inactive), doctor's READY/NOT READY arms, and help's Ok(0); no claim contradicted the code; 2026-09-16 (this row): the path carried TWO rows — R6 DUPLICATE — because two lanes read this new crate in the same round; they are MERGED here, both notes kept verbatim above, and the prose re-read whole against the handlers. Read the USAGE block and the module doc against parse_flags, run_inner, run, report_file, judge, doctor, find_signtool, find_dlib, find_dotnet_runtime and win_join, and the fake-host tests against the code they pin. THREE FALSE CLAIMS FOUND AND FIXED. (1) THE EXIT BLOCK, and it was a live contradiction between the two merged rows: the second row's rewrite put `signtool itself failing` under exit 2 on the reasoning that `run` maps every `Err` to 2, but the first row's own lane had already changed run_inner so a `sign` whose signtool RAN and exited non-zero returns `Ok(1)` after printing its output — pinned by a_signtool_that_ran_and_refused_is_exit_one_not_two, which asserts 1 for a refused credential and 2 for an absent SDK. EXIT now reads 1 = a bad verdict OR a signtool that ran and refused, 2 = nothing signed and no verdict read (parse_flags' unknown verb/option and missing value, the empty file list, `not a file`, an unreadable credentials profile, resolve_config's unresolvable lane, find_signtool/find_dlib/find_dotnet_runtime misses, a Host::run that cannot launch the tool, and doctor's `if ok { 0 } else { 2 }`); the test doc and the in-code comment that QUOTE those two USAGE lines were re-quoted to match. (2) win_path's doc claimed `Production builds these paths with PathBuf::join ... and must stay PathBuf::join`, which b29cf13ad falsified after 8e2487739 wrote it: every Windows tool layout goes through win_join now (Windows' `\\` on every build host) and Path::join survives only in find_on_path's PATH entries and the temp metadata file, both host-native on purpose — the doc now says that. (3) USAGE's closing line still sourced the identity from `Azure Trusted Signing` after 734f9caaa renamed the service everywhere else in the crate (module doc, the `sign` lane line, doctor's lane line) and in apps/aterm-win/SIGNING.md: it now reads Azure Artifact Signing (formerly Trusted Signing), the spelling the rest of the file prints. Everything else re-read and TRUE: Tier::Inactive's `the tier refuses nothing` against judge's `Tier::Inactive => Ok(())` and its `still reported and exits 1` against report_file (false whenever `!verified`, in every tier); DLIB_PACKAGES `newest lineage first` (microsoft.artifactsigning.client before microsoft.trusted.signing.client) and find_dlib's `newest version of the newest lineage` against the package loop with versions_newest_first, pinned by the_renamed_package_lineage_wins_over_the_unlisted_one; the `one exception` to the RealHost seam against the literal `std::fs::remove_file(m)` on the metadata file; win_path's separator/collapse/trailing rules and its NOT-case-folded note against its body",
    ),
    (
        "crates/atpkg-keys/src/main.rs",
        "a491ce402d28ccc7",
        "2026-09-10",
        "read against its parser/dispatch by the 2026-09-10 sweep (aterm-D-keys-xtask-misc); findings fixed in the commit that added this row",
    ),
    (
        "crates/atpkg/src/cli.rs",
        "208039c5ad9be5d3",
        "2026-09-19",
        "re-read on 2026-09-13 by drift sweep of 2026-09-13 (aterm:atpkg); `repair`'s tracked-install sentence covered one of the record's two causes and is fixed in that commit; 2026-09-14 drift sweep (lane atpkg-and-verify, 38 claims checked over the group): re-read against the code that moved under it since this row's anchor commit; every candidate went to a verifier prompted to REFUTE it — no claim contradicted the moved code; 2026-09-14 read of the arm-1c prose 622a87275 added (index_program_shadowed_by_unrelated_line's doc and format strings, its test's doc and literals; the other commit since, 4fe5ed5e7, moved only code — the channel_for lookups) against which_line (arms 1 and 1b return on a managed shim or a live bundle, so 1c is the not-installed case; it also needs no pending stub, an index program with no system= and no extra that is not an agent program, and system_binary_on_path), state::UNAVAILABLE_PREFIX/BLOCKED_PREFIX/NEEDS_ADMIN_PREFIX (the three reasons the doc names, quoted as-is), not_installed_fix's `aterm pkg install` spelling and state::system; the new test runs green; one slip fixed: the test doc quoted the old arm-2 answer as `/usr/bin/trust — system — not managed by aterm`, but state::system renders `system: <path> — not managed by aterm`, so the quote now reads `trust → /usr/bin/trust — system: /usr/bin/trust — not managed by aterm` as the fixing commit itself recorded it; 2026-09-14 read of the `machine` prose 2e3fa303d added (carried by the merge of origin/main; `--diff` against this row's hash shows exactly that commit's lines, and against origin/main's recorded row shows them plus the arm-1c `which` prose this side read at 985cd1c72): VERB_USAGE's machine entry, the strings cmd_machine and apply_machine_settings print, apply_machine_settings' rewritten doc, machine_home_mismatch/_between's docs, cmd_machine's doc, and the docs of machine_settings_apply_before_anything_can_fail_in_every_pass, a_redirected_home_is_a_synthetic_machine and machine_grammar_is_exact — against the handlers themselves, the three call sites at the top of cmd_update_all_code/cmd_install_default_set_code/cmd_seed (and the pre-2e3fa303d file, where update's call sat inside `if failures == 0` at the pass's end, as the test doc says), doctor.rs's Spotlight ok/warn sentences and its doctor_line call, machine.rs apply_universal_control, noindex.rs SUPPORTED/Scan::counts/apply_under, platform/unix.rs account_home (getpwuid_r), and aterm-gui's spawn_pkg_update_check + PackagesRequest::InstallDefaultSet (the three passes the window runs); two slips fixed before this row: cmd_machine's doc said the bare read prints `the same two lines` doctor prints, where only the Universal Control line is doctor_line's and the Spotlight line is a count over the doctor's scan, not its remedy sentence; machine_home_mismatch's doc called the function pure where it reads $HOME and the passwd record — only machine_home_mismatch_between is; 2026-09-14 read of the prose the merge 516d330df carried (shown by `xtask gate help-surfaces --diff`: the dev-linked `which` arm 1b' and its doc, the repair tracked-install and re-seed lines, the linked-from-the-start `list` rows and their doc, the default_set_adopts doc, the NET_FAILED whitespace, and the which/list/adoption test docs and the codex-to-emacs fixture rename) against which_line's arm order and dev_linked_program_line, linkmode is_linked/linked_checkout/linked_bins, stub::is_agent_program, list_porcelain_lines/list_human_lines (the `-` build column, the linked_only entry counted as a program and a dev-link), default_set_adopts and its caller in the --default-set pass (activated, failures, !before.is_empty(), skipped_narrowed, remaining_missing), uninstall's `Re-adopt with aterm pkg install --default-set` line and clear_adoption, provenance::REMEDY, install.rs decide_tracked_stage (KeepRecorded and InProcessRecorded both write the record), lay.rs tracked_policy_of (unset is Allow) and stage_helper.rs plan_helper; two slips fixed before this row: the repair line said `the default, ATPKG_REFUSE_TRACKED_INSTALL=1 refuses instead`, which reads as if =1 were the default where tracked_policy_of makes unset Allow — it now says that is the default and =1 refuses instead; and the re-seed line said the launchd job runs `a clean copy of itself`, where plan_helper runs an untagged binary in place and copies only a tagged one — it now says so; 2026-09-14 (third audit lane, final merge): origin re-read this file after this lane's last read; the only prose since is this lane's `lock-acquired: ` marker (6d95caa50), already read against mutator_store_lock and read_seed_markers above; the union's hash is recorded at the merge; 2026-09-15: `machine` verb usage + cmd_machine/apply_machine_settings/machine_home* docs and the lock-refusal hint re-read against the code by a reader prompted to refute (an earlier cut claimed a window reader that did not yet exist; it exists now — aterm-gui parses `machine-state:`); MACHINE_VERDICT_PREFIX/MACHINE_NOT_APPLIED_PREFIX are the GUI's contract literals; 2026-09-15 audit integration read: MACHINE_APPLY_FAILED_PREFIX and record_universal_control_outcome preserve partial Spotlight changes while reporting a failed Universal Control write independently of exit status. Read against apply_machine_settings, the producer regression, and GUI machine_apply_stdout/machine_apply_completion, which deliver change events before a Failed command outcome. The existing success/revert and no-op messages retain their behavior; 2026-09-15 read at the merge of feat/round-15-receipts over main of the prose f3a5467ed moved with no row (shown by `xtask gate help-surfaces --diff`, matched at 0504383fa): cmd_help's `{}` line — against cmd_help (the title line, then aterm_types::identity::ORIGIN_LINE, then the usage line); no claim contradicted; landing of the erase pending-wrap fix, over f3a5467ed: read the new format literal in cmd_help, which prints the origin line under the atpkg title, against aterm_types::identity::ORIGIN_LINE and its parts test, the help, -h and --help dispatch in main_entry, the aterm pkg forwarding in crates/aterm/src/main.rs, the unchanged static and HOME-free doc comment, the identity module doc and CHANGELOG entry that name atpkg help, and the help tests in cli.rs and settings_action_completion.rs, which pin exit codes only; no claim contradicted the code; 2026-09-15 merge read of main (round 15) over origin/main 58bb2fc63: the merged file is byte-identical to origin/main's; 58bb2fc63 moved its prose after main's recorded read, so read here: THE FAULT LIFT doc and its test strings (the aborted:/error: lift, the three fault fixtures, the dropped 'error: x' other-arm fixture) against up_to_date_row_needs_rewrite, doctor::is_problem_state's aborted:/error: prefixes, report_channel_apply's Aborted arm and the coherence-group abort format at cli.rs:7703; no slip found; 2026-09-15 read of the exec-root wiring on fix/tippy-view-shims (the only prose since 58bb2fc63 is this working tree's, shown by `xtask gate help-surfaces --diff`): reconcile_exec_roots' doc and its four format strings against compat::reconcile/Report (built lines only for Ensured::Built, which needs_root gates; routed = shims whose bytes differed from the render, routed or back to plain; swept = sweep's dot debris, non-directories at numeric names and roots whose build is gone or needs none; errors to eprintln) and its five call sites (cmd_update_all_code after print_gc_abstentions, install_default_set_inner after reconcile_aliases, cmd_seed after reassert_rustup_seam, cmd_rollback's trust branch, repair_store at Deep); the GUI claim against aterm-gui run_pass (stdout through read_seed_markers, which acts only on marker lines; stderr kept whole by read_pass_stderr); run_repair's steps 3/4 and exit-1 sentence and repair_store's doc and its new repair line against repair_store (reconcile after relay_shims, exec_roots_ok folded into the SUCCESS condition); remove_exec_roots' doc and warn line against compat::remove_all, ops::uninstall's compat::remove_program and gc's sweep (which walks only compat/trust, hence `any trust exec root left in it`); print_gc_sweeps' two new lines against GcReport.swept_exec_roots/exec_root_errors; the new test docs and literals against seed_affected_trust, repair_store and the call sites they pin. Four slips fixed before this row: the built line said a `separate copy` where needs_root also answers for a non-copy file; both removed/swept lines said `its build is gone or no longer needs one` over sweep's debris and non-directory arms; the repair line said a root `could not be laid` where any reconcile error (a listing, a removal, a render, a lay) reaches it; the remove_exec_roots line said `aterm pkg gc` retries, where gc sweeps only compat/trust roots, not compat/ itself; and the doc's first cut claimed the GUI discards a pass's stdout, where run_pass pipes it for markers; 2026-09-15 read of the review fixes on fix/tippy-view-shims (shown by `xtask gate help-surfaces --diff`, matched at 0e70d4b94; only this working tree's edits since): the built line's format literal became `{}` over compat::laid_line, whose text is the removed literal byte for byte, printed for each Report.built as before and now also by activate::install_tools_env and flow::rollback_member on Ensured::Built; the_pass_reconcile_routes_an_up_to_date_affected_install's rewritten doc and its new literals against the five call sites: cmd_update_all_code's call sits after print_gc_abstentions and before `if failures == 0 {` (the pass's earlier returns are the disabled manager, no layout, the empty-update line and an index that fails to resolve — a whole-pass failure; a failed program only adds to `failures`, so `a pass with one failed program still heals` holds), install_default_set_inner's after reconcile_aliases and before reconcile_shadowed, cmd_seed's after reassert_rustup_seam and before `let Some(seed_dir) = crate::bundled_seed_dir() else {` whose else-arm returns after report_seedless_posture, cmd_rollback's inside the SEAM_PROGRAM branch before its ExitCode::SUCCESS, repair_store's after relay_shims and before `repair: done`; `write the account's real defaults or rc wiring from inside a test process` against apply_machine_settings at the top of cmd_update_all_code and cmd_seed and hooks::refresh in cmd_update_all_code and run_repair; the mutation claim against two probes run on this branch (the seed call moved below the seedless return, the pass's call moved inside `if failures == 0`), each of which fails the test now (exit 101). One slip fixed before this row: the first cut said most launches return at the seedless return, where cmd_seed's own comment says most launches return below it without installing — only a Mac with no bundled seed returns there; 2026-09-15 read after rebasing fix/tippy-view-shims onto d348f3f51, whose prose moved the hash: settle_net_lane_index's doc (on an EMPTY store the apply lane never runs, against `if !installed.is_empty()`; the row it writes against the apply lane's Err arm, same `error: {e}` state and `update failed: {e}` outcome; the retire against clear_status_row; the apply-resolved arm that neither un-resolves nor re-opens), the `index_resolved` comment (the apply lane's `return 1` on Err; the set-completion lane returns an outcome), the failed-pass exit's `if index_resolved` comment, DefaultSetOutcome.index_error's doc (install_default_set_inner's two early exits, resolve_verified_index Err and channel_for None as FlowError::NoChannel, are the only constructions carrying Some; the pass-end one carries None; the wrappers construct none) and the new test's comments; 19019c7aa's `current_triple(),` moved only code; the exec-root claims above re-checked on the rebased tree (reconcile_exec_roots still after print_gc_abstentions and before `if failures == 0`). One slip fixed on this branch: record_success's doc said a stamping pass finished without a failure and was called only at success exits, against the failed-pass exit's stamp (since 2026-09-14, now guarded by index_resolved); it now names update's clean exit, its resolved failed-pass exit and install --default-set's zero-failure exits; 2026-09-15 read, on rebasing fix/tippy-view-shims over 52469ede9, b7bd41d61 and 616fc8011 (shown by `xtask gate help-surfaces --diff` on origin/main f53f94ad0, whose own gate refused this row: the three commits recorded no read): refuse_seed_for_disk's doc against its body (the `*toolset*` `blocked: insufficient disk space` row with `needs {} free`, `have = None` is no refusal through is_none_or, clear_adoption only when adopted_by_this_run, the marker left to the caller) and cmd_seed's order (the `!manager_enabled()` return with both `lane skipped (fail-closed)` lines sits before `adopted_by_this_run = !adopted(&layout)` and record_adoption, and the disk gate calls refuse_seed_for_disk with that flag); seedless_serves_us's doc against its body and report_seedless_posture (the `!crate::active_builds(layout).is_empty()` argument answered before the deferred closure builds resolve_fetcher and runs resolve_verified_index and seed_serviceable) and the three new test docs and literals against the code they pin; this branch's reconcile_exec_roots call in cmd_seed now sits behind the disabled-manager return, so a disabled manager lays no exec root either, which is the kill switch's rule; no claim contradicted the code; 2026-09-15 (2026-09-16 UTC) read of the prose four commits moved since the recorded read (shown by `xtask gate help-surfaces --diff`, matched at ca6a3a6a3) — cmd_verify_pkg's `FAIL: machine {} signed this manifest, but no client can read it` line and the five reasons pkg_post_verify_refusal spells, uninstall_and_retire's THE INTENT IS WRITTEN BEFORE THE DELETE block and retirement_name_gate's ROW DELETION IS A NEW CAPABILITY block, the reusable_index / recover_missing_roots / install_default_set(_with_path) / cmd_update_all_code one-verify-select docs, and the new tests' docs and literals — against manifest.rs parse_pkg (only schema > SUPPORTED_SCHEMA = 2 is Reject::Schema, so `newer than this build reads` holds), validate_rows (kind = vendor-fetch is RetiredKind; a github-release or https row without asset/sha256, and a pkg row without url/sha256, are Malformed) and parse_toml (not UTF-8, not TOML, a missing or duplicated field is Malformed) — the malformed sentence names each of those; cli.rs uninstall_and_retire (retirement_name_gate BEFORE record_removed/clear_optin/clear_retired and crate::uninstall, so a refused name mints no marker) and ops.rs uninstall_name_shape (empty, `.`, `..`, a separator or NUL — exactly the set the doc says it rejects); and reusable_index's flow::index_is_fresh re-gate. ONE SLIP FIXED in the commit that updated this row: pkg_post_verify_refusal's doc said Reject::ShimEnv and Reject::RetiredKind `each promise in their own doc that this verb names the fix`, where RetiredKind's doc promises the authoring machine's own `atpkg install` names it; the doc now states each promise as it is actually written; 2026-09-16 read of the prose the merge b3def0952 carried in (shown by `xtask gate help-surfaces --diff`, matched at 8ff8f99f4): `__pending`'s `[args...]` usage line and pending_passthrough's doc against cmd_pending's dispatch (args.get(2..)), stub::is_agent_program + vendor::system_binary_on_path and run_pending's exec arm (one stderr line, argv verbatim, 126 if the exec fails, 127 off Unix); managed_state_of's doc against its arms with state::managed/managed_pin; record_index_freshness against flow::last_resolve and status::stamp_index_freshness (the `(index from cache - why)` suffix, added once); GROUP_ABORTED_MARKER/TRACKED_INSTALL_MARKER and clip_cause against report_channel_apply's Aborted arm, install.rs's stage record and the 240-char first-line clip; apply_machine_settings' parent_is_gone early return; print_provenance_carriers against provenance::tagged_files_in/_under over active builds (macOS only, one line per pass); the group-abort `- {cause}` strings and the `N program(s) failed` aggregate; and the two new test docs against clip_cause and pending_passthrough. THREE FALSE CLAIMS FOUND AND FIXED, all three of them doc blocks the merge ORPHANED by inserting a new item between a doc comment and the item it documents. (a) run_pending's whole doc block sat on managed_state_of, so a state-string helper was documented as `the pending verb's body ... ALWAYS exits 127` and run_pending had no doc at all; the block is back on run_pending, and its `ALWAYS exits 127` — false since the passthrough arm landed — now names the exception (an agent program's foreign copy is exec'ed in this process's place; 126 if that exec fails; 127 off Unix). (b) The `STABLE stdout markers` group header sat on GROUP_ABORTED_MARKER, and its enumeration of the markers the ledger leaves unclassified named LOCK_WAITING/LOCK_ACQUIRED/SHADOWED/MANAGED_CURRENT/MACHINE_SETTINGS but not the two new ones — neither of which is in announcement::MARKERS, so `note` classifies neither; the header is back on SEED_STARTING_MARKER and names them, and GROUP_ABORTED's own `A TERMINAL` now says which kind (a terminal ROW on the window's toolchain lane via toolchain_failed, not an answer in the ledger). (c) print_managed_current's doc block sat on print_provenance_carriers, leaving print_managed_current undocumented; it is back. Nothing else contradicted the code; 2026-09-16 read of what 56242acc4 added: the fourth input `config_unreadable` and its precedence, against the gate itself — `!declined && !config_unreadable && (auto_install || adopted)` (cli.rs) — so declined still outranks everything, an unreadable `[packages]` outranks BOTH adopted and auto_install, and the doc's reason (the lane is narrowed by `[packages].exclude`, which an unreadable table cannot supply) matches `config::PackagesConfig::unreadable_table`'s `unreadable: true` default-everything-else shape; the two new test-name strings describe exactly those two arms 2026-09-15 depth read: cmd_machine's new `not walked:` line re-read against noindex::skipped_names (built from SKIP_DIRS, dot-names filtered) and noindex::DOCTOR_DEPTH, which the same commit raised from 3 to 5 with Budget::DOCTOR raised to 60 000 / 3 s; measured on the developer's home, where depth 3 saw 1 exposed target dir and depth 5 sees 5, and doctor.rs's depth rationale now defers to the constant; the count sentence above it is unchanged and still says what an apply can hide. 2026-09-15 lock-ordering read: the edge's new apply_machine_settings_once() call and the reworded lock-refusal hint re-read against main_entry (the call sits above `let _store_lock = ...`, gated by verb_applies_machine_settings = install|seed|update), apply_machine_settings_once's doc (a OnceLock, so the bodies replay what the edge did rather than walking $HOME twice) and cmd_machine, which calls the RAW apply so an explicit gesture always re-reads; the hint now says the settings were applied above the refusal and still names `aterm pkg machine apply` as the re-apply door, which crates/atpkg/tests/store_lock_wait.rs pins on stderr while its new no_progress_marker helper pins that stdout carries only host-work lines, never a progress marker. 2026-09-16 audit-3 read: the machine read's new no-home line, the `nothing to apply from what was seen` verdict (scan_complete now starts false and is set only when a home was walked), machine_refusal's unreadable-config sentence, and the Spotlight `machine settings failed —` line re-read against noindex::Applied::Failed (the new variant the rollback/plan/link sites return and Applied::failure() reports), render_applied's FAILED row and its `, N failed` summary tail, config::parse_machine's unreadable flag, machine::MachineState::config_unreadable and machine_state_line's appended `config=unreadable` key; every other sentence in the verb is unchanged.; 2026-09-16 read of the prose that moved after this row was recorded, in two changes. (1) The peer repair-arity fix 5d9cdd94e, which landed without re-recording: its dispatch-arm comment (a store MUTATOR that took any argument and ignored it), the paragraph added to zero_arity's doc, and the prose of the assertions its test gained — read against the arm itself (repair now calls zero_arity(&args[1..], repair) and reaches run_repair only when no stray argument was given), against zero_arity (one stray argument prints `atpkg repair: unknown argument ... this verb takes none` plus usage_of(repair) and exits 2) and against run_repair (hooks::refresh_rewiring_rc, the `repair: shell integration rewritten (~/.aterm/shell.d + rc wiring)` line, lay_reroute_stubs, reassert_rustup_seam, then repair_store's relay_shims) — every claim held. (2) This change's own prose: cmd_install_argv's doc (the elevation flag belongs to the ONE-PROGRAM form alone), the new parse_install_argv doc, its whole-set refusal comment, the three lines that refusal prints, and the new test's doc — read against cmd_install_elevated, whose --default-set branch returns into cmd_install_default_set() ABOVE the single set_elevation(door_elevation(explicit, stdin_is_tty())) call, against cmd_install_default_set_code (which reads no elevation at all), against crate::elevate (Deferred is the thread-local default, so the whole-set pass never elevates) and against the manual's aterm pkg install block, which has only ever documented --elevate on the one-program spelling; 2026-09-16 read at the landing of the winit notice fix, over f37f01ae8 and 5fdea3bfd: the __pending SKIPPED lines read against the Phase::Skipped arm (Sink::finished retires a skipped program from the queue and advances programs_done, and the bump file only reorders what is still queued, so nothing later in the pass installs it; the reason is the recorded row error, sanitized), and the uninstall --all lines against foreign_copy_note (system_path, installed_via_path), print_left_alone, and the removed.is_empty() branch that prints the removed-nothing summary only when no removal failed and still names what was left alone; no claim contradicted the code; 2026-09-16 read of the pass's own `machine-state:` record (b8fab35f6 and this tree, shown by `xtask gate help-surfaces --diff`): the `atpkg: {MACHINE_STATE_MARKER}{}` literal print_machine_state_after_apply prints, the SpotlightCensus field docs, spotlight_census's and spotlight_after_apply's docs, print_machine_state_after_apply's doc, and the two new tests' docs and literals — against apply_machine_settings (census = Some(spotlight_after_apply(..)) inside the Spotlight half, print_machine_state_after_apply called after `machine_settings_line(&entries)` under `if crate::noindex::SUPPORTED && let Some(home)`, with census.or_else(|| (!entries.is_empty()).then(|| spotlight_census(home))) — so a pass whose Spotlight half is off walks only when its entries are non-empty, which with that half off can only be the Universal Control change), spotlight_after_apply (a Migrated or Failed outcome re-walks through spotlight_census; else scan.counts() with would_migrate 0), noindex::apply_one (the dry run's refusals — no Cargo.toml beside, a live build, a name in the way — are the real apply's Skipped arms, so an exposed dir a real apply left is one a dry run does not plan), noindex::apply_under_scan (the scan handed back beside the outcomes), machine::universal_control_state + platform::unix::universal_control_state (the two keys, one `defaults read` each) and machine::machine_state_line (the read's own builder, MACHINE_STATE_MARKER); aterm-gui's read_seed_markers/parse_seed_line take the record as Wake::PkgMachineState and the card's expectation is opened on the change line, which is why the order is pinned. ONE SLIP FIXED before this row: the printer's doc said `one defaults read` where the state is two keys, one read each; 2026-09-16 read of the four test literals 0dc0914a9 added to the_apply_prints_its_state_record_after_its_change_line (shown by `xtask gate help-surfaces --diff`): `cfg.universal_control()` counted once in apply_machine_settings' body — against the body, where the one call sits at `let policy = cfg.universal_control();` above apply_universal_control and print_machine_state_after_apply takes `policy` as a parameter (the record printer contains no `universal_control()` call) — and machine_refusal/cmd_machine, which read the policy on their own paths outside that body; no claim contradicted the code; 2026-09-16 re-read of the repair_store, uninstall_and_retire, remove_exec_roots and gc-report comments changed by the clone construction against compat::reconcile/ensure_root_in_process (Deep: clones plus the stock names' bytes, a hard-link root rebuilt), compat::remove_all, seam::detach and gc::run's closing compat::sweep; no user-facing string of the verb changed; 2026-09-16 read of the prose the Intel-Mac hold and its follow-up added (the union re-read at the merge with the 2026-09-15 rows): the set-completion Unpublished arm's `no artifact for <triple> — coherence group skipped whole (§6 clean skip)` line against the prescan's identical words and its note_finished(Skipped) loop; the UpToDate arm's `(no longer held)` against was_held and the `held:` row prefix the hold writes; the update lane's `NOT updated — <member>'s pinned build N is not published for <triple>; staying on the current builds` line and its per-installed-member `held:` rows against apply_group_txn's TxnOutcome::Unpublished { member, build, triple } and the installed.get(prog) guard; the loud arm's Aborted `why` (`its current build was recalled and the group's new pin (<member> build N) is not published for <triple>, so its commands are disabled until it is`) against disable_revoked_currents and the `aborted: <phase> — <cause>` row the Aborted arm records; the set-completion prescan's disabled-program clause — `program_disabled_here`'s doc (a `tombstoned:`/`aborted:` row, or a tombstone shim among the program's exposed tools, falling back to the program's own name when the build is no longer counted installed, exactly `repair`'s K2 measurement) against `is_tombstone_shim` and `installed_exposes`, and the `no build for this machine (<triple>) — and its current build is disabled here (see `aterm pkg doctor`); the row stays` line against the `continue` that skips `record_status` while `wanted.remove` still runs; the docs of an_installed_group_whose_new_pin_is_unpublished_here_is_held_not_failed and an_unpublished_pin_never_quietly_holds_a_revoked_current_build against the tests themselves, which run green on x86_64 macOS; re-read on 2026-09-16 after the rebase onto dd3808bc9 (the row upstream recorded there): `--diff` against that read shows exactly the prose the two carried commits added and this side had read at 40f502671e2b4c20 — the `tombstoned:`/`aborted:` row markers, the no-build-and-disabled row, the coherence-group clean-skip line, the `blocked`/`held`/`(no longer {lifted})` wording, the held-group NOT-updated row and their tests — and no line the merge itself wrote; nothing new to read against a handler; 2026-09-17 read of the hidden landing verb's prose (cmd_landing's doc and the dispatch comment above it) against the code beside it: the verb is dispatched before the verb match and BEFORE the store lock, alongside __pending and the reroute verb — which is precisely what lets it answer while the pass that is landing the build HOLDS that lock for the whole wait; HIDDEN_VERB = __landing; the optional PREFIX operand is taken only when it is not `--` and Path::is_absolute, so a run with no HOME still finds the store instead of exiting 1 with the tool never run; WAIT_SECS_ENV, DEFAULT_WAIT_SECS = 45 and REFRESH_SECS = 2 as landing.rs defines them, both pinned by its own tests; and SIGINT stops the wait without killing the command (the io.sleep contract at landing.rs:459 answers false when a SIGINT arrived, and every ending still runs the tool). No claim contradicted; 2026-09-17 merge read of main (round 16) over origin/main: the merged prose hashes to this row's recorded value, so no prose moved at the merge re-read on 2026-09-17 against the handler after 9613620b2 and the merge d0b23f22f, which changed this prose without re-recording: `cmd_landing`'s Windows hand-over and Ctrl-C sentences and `landing_ctrl`/`landing_wait_in_process` were read against `landing::HandOver::parse`, `landing::shim_command`, `platform::exec_or_run`, `platform::add_ctrl_handler`/`remove_ctrl_handler`, `platform::cmd_landing_prelude` with `CMD_FORWARD_TAIL`, and `activate::reconcile_agents`/`twin_is_rendered`; the string-set delta was read too (the new `system` literal is the `extern \"system\"` ABI of `landing_ctrl`, the two `--` literals moved verbatim into `HandOver::parse`, and the removed duplicate is the collapsed `#[cfg(unix)]` pair — no usage text lost its `--`). Six findings were FIXED in the commit that records this row rather than left as drift: the unconditional Windows Ctrl-C promise (arming the console handler is best-effort, so that one ending may not run the tool), the usage header that omitted `[<prefix>]`, the `LANDING_INTERRUPTED` doc that named only the SIGINT handler, an ending enumeration that missed the not-landing case, the manual's doubled `Terminate batch job` caveat that had dropped its landing-path scope, and its \"every later re-lay is safe\" and \"carries the same wait\" sentences, which now admit the residual byte-offset gap the code records and the fail-closed empty prelude that leaves a twin with no wait at all. re-read on 2026-09-18 for the first-launch disclosure, against `net_announcement`, `should_complete_set`, `clear_adoption` (which drops the who-adopted record with the marker), `Index::channel_for` (the per-target pin view this lane's `seed_install_bytes` must keep), `verb_mutates_store` with `mutator_store_lock` and `StoreLockError::Contended`'s retry sentence, and `aterm-gui`'s `toolchain_announced` (text after the LAST `(`, clipped at 40 by `sanitize_for_tty`). ONE SLIP FIXED: the log-cap check named the line `atpkg seed starting: {detail}`, but one `Wake::PkgSeedStarted` serves the seed and wire lanes and the GUI logs `atpkg install pass starting: {detail}`, so the sentence and the prefix the check measures with were corrected — the first-launch line is 426 bytes as logged, not the 418/420 the lane claimed. 2026-09-18 read of the rc-report prose (`repair_hook_lines`' doc and every line it prints, `run_repair`'s shell-integration comment, and `repair_names_the_rc_it_relaid_over_an_opt_out`'s doc and assert messages) against atpkg hooks.rs — `RC_BEGIN`/`RC_END`, the four-row `RC_FILES` roster, `RC_LEDGER` (`~/.aterm/rc-wired`, one CANONICAL path per line), `refresh` versus `refresh_rewiring_rc`, `refresh_at`/`pass_at`, `HookPass`'s three variants and each of `RcOutcome`'s seven, and `ensure_rc_sources_hooks`' exits in order (absent, unresolvable, the `protected::under_protected_root` consent fence, not a regular file, not UTF-8, marker present, ledger opt-out, temp+rename) — plus `aterm-update-core`'s `ensure_private_dir` (symlink, foreign owner and group/other-writable all refused) and `crate::protected`'s HOME_FOLDERS/LIBRARY_DOMAINS/ABSOLUTE_ROOTS for the folder list the consent-fence line quotes, `platform::our_uid` for the test's non-root guard, and `run_repair` as the ONE caller of `hooks::refresh_rewiring_rc` (`git grep` over crates/). THREE SLIPS FIXED before recording: the ported line said an rc over 4 MiB is left alone, but main has no size bound on the rc read (no `RC_MAX_BYTES`), so that clause is gone; \"nothing was touched\" on the not-hardened arm overclaimed, since `ensure_private_dir` may have created `~/.aterm` before refusing `~/.aterm/shell.d`, so it now says \"no rc was read or written\", which is what the arm actually guarantees; and the hooks-not-written arm no longer says the rc step \"still ran\" — on main `refresh_at` gates the rc wiring on the hook write, so that list is always empty. The historical line at the `repair` dispatch arm (\"printed \\\"repair: shell integration rewritten\\\" and exited 0\") is past tense about the pre-`zero_arity` verb and is left as history; 2026-09-18 read of the waited-launch stand-down prose added for the owner's \"Another aterm is installing\" report (the WAITED_FOR_LOCK / PROCESS_START_UNIX / FIRST_CONTENDED_UNIX docs, pass_finished_since_start and pass_finished_while_we_waited, stood_down_after_wait_line's `atpkg: update: up to date — a pass on this store succeeded N s ago, while this one waited for it; nothing to redo`, the LOCK_WAITING/LOCK_ACQUIRED marker docs now saying the GUI logs the line and paints no row, and parent_is_gone's orphan-watch sentence) against cmd_update_all_code's early return after `layout()`, the still_wanted closure in mutator_store_lock (contended polls only), and the once-pass exemption on ATPKG_UPDATE_INTERVAL_SECS=0; the stand-down keys on last_success_at strictly after the first contended second, so `up to date` is only ever said on a success stamp — no claim contradicts the code.",
    ),
    (
        "crates/xtask/src/gate.rs",
        "41574f240f96b8a6",
        "2026-09-19",
        "re-read on 2026-09-18 for the v0.88.0 assembly after merging main at 5b13575de: the surface gained the cells gate's cache-dir literals (ae614355d — `cell_target_dir` resolves ATERM_CELL_TARGET_DIR, else $XDG_CACHE_HOME/aterm/cells, else $HOME/.cache/aterm/cells, and refuses the temp dir, read against that function and its fixture paths) and the `[patch.crates-io]` manifest-reader strings (fa57c6f97, read against the reader that names the manifest it could not read); the fmt-editions and roster reads recorded before this row still hold, and `the_header_roster_sentence_names_exactly_the_roster` still holds the header",
    ),
    (
        "crates/xtask/src/main.rs",
        "1dd2dd4a34fbe408",
        "2026-09-14",
        "re-read on 2026-09-13 by drift sweep of 2026-09-13 (lane xtask); its opt-in list now derives from gate::opt_in_names(); 2026-09-14 drift sweep (lane xtask, 17 claims checked over the group): re-read against the code that moved under it since this row's anchor commit; every candidate went to a verifier prompted to REFUTE it — no claim contradicted the moved code",
    ),
    (
        "crates/aterm-census/src/main.rs",
        "15f0c30a18130a47",
        "2026-09-14",
        "usage text written 2026-09-10 exact to its parser (root default `.`, five selection flags and aliases, last-wins, exit 0/1/2) in the program-entry pass; read against main() the same day; 2026-09-14 drift sweep (lane atpkg-and-verify, 38 claims checked over the group): re-read against the code that moved under it since this row's anchor commit; every candidate went to a verifier prompted to REFUTE it — no claim contradicted the moved code",
    ),
    (
        "crates/aterm-primer/assets/aterm-fabric-body.md",
        "9abb6a80ba6cddb2",
        "2026-09-15",
        "read in full against the code it teaches on 2026-09-13 by the primer-skill read of 2026-09-13, the first time any gate saw it — it is include_str!-ed into aterm-primer and installed into agents' own context files, so a wrong line here is TYPED; four false claim(s) found and fixed in that commit; 2026-09-13 read (2026-09-14 UTC) of the four prose changes 9758cd022 made here — the `fabric=connected` trap paragraph, the rewritten `no-bridge=1` bullet with its `queued, unpublishable until this instance has a bridge` / `not sent` wording (the only two lines the gate's --diff extracts), the new `ERR timeout id=<n>` bullet, and the `aterm-link` to `aterm link` respelling of `serve`, `mirror` and `hook install claude` — against fabric.rs (bridge_attached/fabric_state/bridge_lost, cmd_post pushing the PostRow into the outbox BEFORE any refusal path, fabric_wait_refusal and bridge_reachable = supervised || state != absent, the `ERR timeout id={id}` returned at the deadline, WAIT_DEFAULT_MS 30 s / WAIT_MAX_MS 600 s), control.rs serve_bridge (which stamps connected as the inherited Scope::Bridge lane starts being served, before the child has dialled any broker, and clears it only through BridgeLostGuard), aterm-link's Bridge::run reconnect loop (an unreachable broker is a back-off, never an exit — a bridge that exits on a broker hiccup lifts the fleet halt by dying) and cmd_outbox's peek-that-removes-nothing that lets a `fabric attach` drain the same queue, cli::dispatch's serve/ls/hook/mirror arms which `aterm link` (aterm/src/main.rs Verb::Link) and the `aterm-link` argv0 alias both reach, hook.rs's four-event `.claude/settings.json` template with its session-start/user-prompt-submit/pre-tool-use/stop arms, and fabric attach's own control_verbs.rs row; no claim contradicted, and the one gap worth naming is that the `third outcome` bullet does not reach a FOURTH — the bridge's retirement verdict `ERR <undeliverable|expired|ambiguous> id=<n>` from retire_post/DEAD_DEFAULT, which unlike the other three does NOT mean queued (`ctl help post` omits it too) and the shipped skill gained the FOURTH outcome in this commit, for the same reason the manual.rs and control_verbs.rs rows did: its list stopped at `ERR timeout id=<n>` as `the third outcome, and it means queued too`, where a post the bridge RETIRED answers `ERR unroutable|ambiguous|undeliverable id=<n>` and is the one outcome that is not queued. The hash recorded here is the CORRECTED asset; 2026-09-14 round-12: the Being woken Claude Code bullet rewritten (install with --merge and a backup, live loading, --check, exit 0 on a failure of the hook's own) and read against aterm-link hook.rs's install_claude/check/open; the gate's prose lexer sees no string literal or doc comment in this markdown, so its hash did not move — the read is recorded here regardless; 2026-09-14 read of the round-13 rewrite (the `status` synopsis, the four-row table with `stalled`, the no-heartbeat paragraph, and the queued=1 bullet naming stalled as answered at once) against the same handlers as the manual's fabric page — aterm-gui fabric.rs link_report / fabric_state / cmd_post / fabric_wait_refusal, control.rs fabric_status_line, aterm-link bridge.rs ACK_DEADLINE, LinkReport and link_reason; no claim contradicted; 2026-09-14 read of the new `Who is doing what, without reading a screen` section (feat/round-13-fabric-on, round 13 C) against crates/aterm-link/src/presence.rs Fields::tokens and phase_word (attention= role= detail= phase= [context=<n>%] title=; the six phase words; Mode::Minimal writes attention= alone), cli.rs row (the `aterm link ls` columns) and fabric.rs render_text (ROLE DETAIL PHASE CTX); the `busy worker gets the mail only` sentence is round 14's D2 as the spec states it, not a claim about this build's `post`, and reads as advice to the manager; no claim contradicted; 2026-09-14 read of the whole diff against main at the rebase onto main's round-12 merge (the `fabric=` table and paragraph, the `Who is doing what` section, the `absent|stalled|disconnected` bullet, and round 12's Being woken bullet kept beside them) with two sentences amended: a wait under `stalled reason=starting` parks (aterm-gui fabric.rs wait_is_futile) and `survey` is the presence word for `aterm drive phase`'s `survey 0` line (presence.rs phase_word over aterm_phase::survey_open; aterm-agent run.rs render_phase_and_survey prints the phase word and that line); `title=` read against presence.rs read_meta (`meta set title` alone); no other claim contradicted; 2026-09-14 SPLIT: this asset became aterm-fabric-body.md (the Markdown every agent gets) plus aterm-fabric-frontmatter.md (Claude's auto-discovery header alone) — an audit found Codex and OpenCode receiving byte-identical copies of the Claude SKILL.md, frontmatter included. The body was read against the verbs it teaches: `inbox`/`inbox get`/`inbox seen`/`post`/`await inbox` and the trust set trust=<human|agent|relayed|screen> in crates/aterm-types/src/control_verbs.rs, the two watermarks and dropped=/truncated= in aterm-gui fabric.rs cmd_inbox, the halt against `aterm ctl help hold` (the body keeps no second copy of the verb list on purpose), the three queued not-landed answers against fabric.rs cmd_post's wait loop, and the mirror's `--session <sid>` hazard against aterm-link mirror.rs USAGE (default is EVERY session; the mirror runs `inbox seen` on the agent's behalf); 2026-09-14 merge read of main over 28508563a (git carried round 13's edits onto the renamed aterm-fabric-body.md): the merged prose is exactly round 13's `stalled` row, no-heartbeat paragraph, `Who is doing what` section and `absent|stalled|disconnected` bullet plus main's split (the frontmatter moved to aterm-fabric-frontmatter.md), its trust=`screen` sentence with the `aterm ctl help inbox` pointer, and the mirror's `--session <sid>` line and paragraph with its `notice`-line sentence — checked against aterm-gui fabric.rs TRUSTS (human, agent, relayed, screen) and control_verbs.rs's inbox row (the same four), aterm-link mirror.rs Options::sessions (empty = EVERY session), run_inbox's `inbox seen` on the agent's behalf and dropped_notice (the `notice` line carrying the endpoint's eviction count), crates/aterm-primer/src/lib.rs FABRIC_BODY / FABRIC_SKILL_BODY (the body alone for every agent, frontmatter + body for Claude) and presence.rs (title= is the user title alone, else `-`); no claim contradicted; 2026-09-15 read of round 15's key and deadline paragraphs from `git diff` against the branch base (the gate's Rust lexer sees little of a Markdown file, so the hash did not move with them): the key= paragraph against aterm-gui fabric.rs cmd_post/valid_key and aterm-link bridge.rs drain_outbox/StateDir::key_seq (first wins, KEYS_KEEP 4096, dup=1 only for a key-chosen deduped publish), the queued-answer `without key=` clause, and the deadline paragraph against note_deadline/settle_deadline_for/expire_deadlines. ONE sentence FIXED here: `late=1` now says a restarted bridge delivers the late reply unflagged (the bridge's `expired` map is in memory only). No other claim contradicted; 2026-09-15 read of round 15's receipts and fetch-by-offset additions, the same way: the `inbox get @<off>` command line and the header's oldest_on_bus= against aterm-gui fabric.rs cmd_inbox/inbox_get_at; the dropped=/truncated=1 bullets and the Nothing-lost paragraph against inbox_get_at (the ring answers only a whole row), deliver_fetched and aterm-link bridge.rs fetched_lines (chunks, 256 KiB cut with len=), Fetched::render and fetch_failure; the Receipts paragraph against cmd_inbox_seen/owe_receipt, cmd_outbox's receipt lines, send_receipt/retire_receipt and enable.rs's `receipts = true`; `--wait-ack` against cmd_post/wait_receipt. ONE sentence FIXED here: `bounded by dl= plus a few seconds` now states the rule cmd_post computes (`--wait-ack=<ms>`, else dl= plus 5 s, else 30 s). No other claim contradicted; 2026-09-15 read of the Being woken Claude Code bullet's new --report-to sentences against aterm-link hook.rs stop/report/recipient_check (the post before the wait, re= the newest unhandled task, 4 KiB, once per message by the content key, --check's `report-to=<sid>` tail and the installer's refusal of a recipient the instance does not host); the gate's prose lexer sees no literal in this markdown, so its hash did not move — the read is recorded here regardless; no claim contradicted; 2026-09-14 (2026-09-15 UTC) read of the --report-to sentences against hook.rs report/report_task/recipient_check/fabric_cannot_carry and self_test, and of the wake bullet against may_wake/accepted: one sentence added in this commit, that `--accept-from` names who may wake you beside every human by OWNER sid, the bridge's node id being another list; the lexer sees no literal here, so the hash stands; no claim contradicted; 2026-09-15 merge read of feat/round-15-receipts over main: the merged prose is exactly round 15's change (the `inbox get @<off>` command line, the header's oldest_on_bus=, the dropped= and truncated=1 bullets, the Nothing-lost paragraph, the queued-answers `without key=` clause and the Exactly-once, Deadline and Receipts paragraphs, shown by `git diff` of main against the merged tree — the gate's Rust lexer sees no literal in this markdown, so neither side's hash moved) plus upstream's change (the Being-woken Claude Code bullet's --report-to and --accept-from sentences, shown by `git diff 88f04bba9 main`) plus ONE sentence added at this merge to the Exactly-once paragraph — the address is still resolved first, so a re-post to a session that has gone is retired `ERR unroutable` and puts nothing new on the bus; the Receipts paragraph's `inbox seen <id> handled` and the --report-to bullet's `a task you inbox seen <id> handled before you stop is still answered` agree, a verdict acking the manager's task while report_task still answers it, checked against aterm-link bridge.rs drain_outbox, resolve_to and refresh_sessions, send_receipt, answer_fetch and fetched_lines, hook.rs stop, report, report_task and recipient_check, and aterm-gui fabric.rs inbox_get_at, cmd_inbox_seen and owe_receipt",
    ),
    (
        "crates/aterm-primer/assets/aterm-fabric-frontmatter.md",
        "8b66ffbe67127ae1",
        "2026-09-14",
        "2026-09-14: the Claude Code skills frontmatter alone — `name: aterm-fabric` and the `description:` trigger text auto-discovery matches on — read against crates/aterm-primer/src/lib.rs skills_for (the `claude` arm is the only one that prepends it; FABRIC_SKILL_BODY = frontmatter + body) and every_agent_gets_the_body_under_its_own_header, which pins that Codex's prompt begins with the body and OpenCode's frontmatter carries description: and no name:; the description names the triggers (inbox, post to, ERR halted, fabric=absent, no-bridge) that the body's sections answer; no claim contradicted",
    ),
    (
        "crates/aterm-primer/assets/drive-aterm-skill.md",
        "d7a1f36f438339ce",
        "2026-09-14",
        "read in full against the code it teaches on 2026-09-13 by the primer-skill read of 2026-09-13, the first time any gate saw it — it is include_str!-ed into aterm-primer and installed into agents' own context files, so a wrong line here is TYPED; six false claim(s) found and fixed in that commit; 2026-09-13 read of round 7's additions (the `aterm drive report` command line and paragraph, and `watch --report`) against drive_cli.rs's report arm, supervise/report.rs (the six reasons archive-gap, archive-reset, max-rows, marker-not-found, no-archive, main-screen; the header report complete= reason= marker= turn= rows= archived= screen= last=), run.rs reported() (idle, question, limited only) and reported_event_line (EVENT <phase> seq= complete= rows= then the summary), and control_session.rs's turn-start ArchMark with history printing arch= before text=; confirmed live on 2026-09-13 on a headless replay of a real Claude Code byte stream (offscreen returned every row the offline prototype recovered) and against the installed 0.84 server (report fell back to no-archive and found the start through marker=ledger); no slip found; recorded at the merge of main into feat/round-7-offscreen; 2026-09-13 read of round 8's paragraph (`Never rate a session for the human`: the survey's two rows, phase's and await-turn's `survey 0`, the guarded `key 'if=^●.How.is.Claude.doing' 0` command quoted for zsh, watch's EVENT survey line, --dismiss-surveys' DISMISSED line and its still-open hand-over, and the monitor grep with DISMISSED) against phase.rs survey_open, drive_cli.rs phase_reply, run.rs survey/dismiss_survey/survey_event_line and SURVEY_ROW, and the server's row_matcher guard; the gate's --diff shows only the quoted strings it extracts from a skill (the one new `@$SID`), so the paragraph was read from git diff; one slip fixed before this row (a copy of the survey elsewhere on the screen was said to match nothing, where only a quoted copy is sure not to); 2026-09-13 read (2026-09-14 UTC) of round 9's paragraph (`A worker running out of context is a decision point too`: the indicator's two spellings, phase's and await-turn's `context <n>%`, watch's `EVENT context` once a descent at or below --context-warn (default 10, 0 off) and `EVENT compacted` once the indicator has gone from above the composer or jumped 30 points, supervise's stderr lines for its run only and the `phase` check before the next run, the right-edge rule for a quoted copy) against phase.rs context_left, drive_cli.rs parse_sub/DEFAULT_CONTEXT_WARN/phase_reply and run.rs watch_context/drive/StopAtReview::say; the gate's --diff shows none of it (the paragraph quotes no string the gate extracts, so the hash is unchanged), and the paragraph was read from git diff; one slip fixed before this row (EVENT context was said to come once, where it comes once a descent and again after a compaction) and `gone` narrowed to gone from above the composer; 2026-09-14 read of round 10's sentences (an aterm self-update hands the archive and turn ledger over, `carried=1`, a report across one whole when its rows fit what it carries, a resize then `archive-gap` with nothing lost, and a carry that failed: `history` empty, marker=user-row or marker-not-found, `--since` an older mark archive-reset) against handoff_carry.rs export/tail_from/CARRY_ARCHIVE_BYTES/adopted_ledger, alt_archive.rs import, control_session.rs cmd_history, report.rs gather_report/assess and seamless_carry_tests' end-to-end, resize and dropped-carry tests; the gate's --diff shows none of it (the paragraph quotes no new string the gate extracts, so the hash is unchanged), and the paragraph was read from git diff main; one slip fixed before this row (whole across a self-update, where the carry keeps at most the newest 1 MiB — now whole when the rows since the turn fit what it carries); 2026-09-14 drift sweep (lane primer, 46 claims checked over the group): re-read against the code that moved under it since this row's anchor commit; every candidate went to a verifier prompted to REFUTE it — no claim contradicted the moved code; 2026-09-14 merge read of feat/round-10-carry over main: the merged prose is exactly round 10's paragraph (the self-update carry, `carried=1`, the 1 MiB bound, the resize `archive-gap`, and a failed carry's empty `history` and `archive-reset`) plus nothing of main's: main changed none of the file, only this row's note (the run id replaced by the read's date, and its drift sweep), and the hash is the same on both sides because round 10's paragraph quotes no string the gate extracts, so the paragraph was read from git diff; checked against the merged handoff_carry.rs (CARRY_ARCHIVE_BYTES, adopted_ledger), alt_archive.rs import, control_session.rs cmd_history and supervise/report.rs, none of which main touched; no claim contradicted; 2026-09-14 read of the round-11 additions (the three new cheat-sheet lines, the rewritten `Everything from classify down` sentence, and the `aterm drive ledger` paragraph) against aterm-agent drive_cli.rs (the verb list its unknown-command error prints, parse_sub's per-verb flags, ledger_verb), supervise/ledger.rs (the four sources, the clock placed by the control socket's birth time, `~` for what cannot be placed, no latency claimed then), supervise/journal.rs (one JSON object per line, append-only, 0600 when created, one warning that stops nothing) and supervise/blocks.rs view_rows; no claim contradicted; 2026-09-14 read of the paragraphs this round adds, taken from `git diff main` (the three new example command lines, the `Everything from classify down` sentence, and the `aterm drive ledger replays the loop` block), against drive_cli.rs ledger_verb and parse_sub, ledger.rs gather/render_ledger (the four sources, each named when unread; the `~` mark and the withheld latency when no clock anchor places a stamp), ledger_html.rs (one page, inline style and script, nothing fetched) and journal.rs (one object per printed line, append-only, 0600 on creation, one warning that stops nothing); the `--final` line against blocks.rs view_rows; no claim contradicted; 2026-09-14 (feat/round-14-mail, round 14 D1/D2/D4) read of the --mail and task prose — DRIVE_HELP's synopsis lines, the watch, task and --mail entries and the journal's mail kind and report field (lib.rs); DRIVE_PAGE's watch, task, --mail and --journal entries (manual.rs); run.rs's module doc, SuperviseOpts::mail, Fold, MailIn, NoLane, Sink, the Review, StopAtReview, Lines and folded_event_line docs, supervise_mail, supervise_with, run_loop, watch_mail, watch_with, look's fold gate, hold_for_report, event_line_as, render_result_mail and the six mail test docs; the two skills' loop, watch, task and ledger paragraphs — against mail.rs Lane::serve (an inbox 1 --peek --meta baseline; await inbox since=<newest> kinds=<all nine> timeout <step>, re-armed on OK timeout, the kinds list dropped on ERR usage; inbox since=<id> --peek --meta, then inbox get <id> for the watched worker's report only; the MAIL line per row through the loop's sink; lane_off said once on any other ERR or a lapsed reconnect window), Lane::call's retry of a lost reply (pause doubling to pause_max, within reconnect), task() (post to=@sid kind=task [dl=<ms>] <text> from --inbox or @self, the offset from the OK's off=, one text --json read and `turn idle=600 timeout=2500 Inbox: task @<off>` only on Phase::Idle with submitted=1 as nudged, the inbox 1 baseline read BEFORE the post, await inbox … kinds=answer,report,ack re-armed on rows without re=<off>, the TIMEOUT line and EXIT_TIMEOUT), run.rs hold_for_report (pending drained; a delivery within the window folds at once, else recv_timeout to min(now + grace, deadline); Disconnected gives None so the line is as without the flag), look's Phase::Idle gate with the brief skipped only on Fold::Report, run_loop's thread::scope join (the lane returns within one mail_step) and drive_cli.rs parse_sub (--mail, --report-window and --idle-grace watch's and supervise's only; --inbox needs a leading @; --deadline, --wait and --no-nudge task's only), mail_needs_sid and task_opts (--wait bounded by --deadline, else --timeout); measured on the live worker s-1e918c4662a1b7b8bd43 (busy for the whole 25 s budget: TIMEOUT, the process ending 40 s in as the lane's parked wait was joined within its 20 s step, no MAIL lane off) and by the mock tests; no claim contradicted; 2026-09-14 (2026-09-15 UTC) read of the round-14 paragraphs from `git diff main` (the three command lines, the reworded `Everything from classify down` sentence and the `With the fabric on` paragraph) against supervise/mail.rs Lane::serve/task() and supervise/run.rs hold_for_report/MailIn::is_this_turns/run_loop; one claim amended in this commit: `a worker with the wake hooks needs --no-nudge` now says the hooks must accept you (hook.rs may_wake/accepted: a human, or a listed OWNER sid); the gate's lexer hashes only the backticked variables of this markdown, so the hash above stands; no other claim contradicted",
    ),
    (
        "crates/aterm-primer/assets/rust-in-aterm-skill.md",
        "fd2da99cf414c6cd",
        "2026-09-13",
        "read in full against the code it teaches on 2026-09-13 by the primer-skill read of 2026-09-13, the first time any gate saw it — it is include_str!-ed into aterm-primer and installed into agents' own context files, so a wrong line here is TYPED; three false claim(s) found and fixed in that commit; 2026-09-15 (fix/help-rust-targo-line): the one paragraph describing what `aterm help rust` prints was rewritten against crates/aterm-cli/src/manual.rs rust_page (the gates' pick and refused candidates, `targo on PATH` followed through an atpkg shim, the PATH vs gates comparison, rustc's sysroot, the pin and the .cargo/config off-switch) — this row's hash did not move because the lexer the gate hashes .md assets with sees no string literal in that paragraph, so this clause is the only record of the read; the rest of the file was not re-read",
    ),
    (
        "crates/aterm-primer/assets/supervise-agent-skill.md",
        "99727d7bc386bcb6",
        "2026-09-17",
        "read in full against the code it teaches on 2026-09-13 by the primer-skill read of 2026-09-13, the first time any gate saw it — it is include_str!-ed into aterm-primer and installed into agents' own context files, so a wrong line here is TYPED; four, one of them the --sandbox write-confinement claim false claim(s) found and fixed in that commit; 2026-09-13 read of round 7's additions (the `aterm drive report` command line and paragraph, and `watch --report`) against drive_cli.rs's report arm, supervise/report.rs (the six reasons archive-gap, archive-reset, max-rows, marker-not-found, no-archive, main-screen; the header report complete= reason= marker= turn= rows= archived= screen= last=), run.rs reported() (idle, question, limited only) and reported_event_line (EVENT <phase> seq= complete= rows= then the summary), and control_session.rs's turn-start ArchMark with history printing arch= before text=; confirmed live on 2026-09-13 on a headless replay of a real Claude Code byte stream (offscreen returned every row the offline prototype recovered) and against the installed 0.84 server (report fell back to no-archive and found the start through marker=ledger); no slip found; recorded at the merge of main into feat/round-7-offscreen; 2026-09-13 read (2026-09-14 UTC) of round 9's bullet (`EVENT context … means the worker is about to compact; EVENT compacted … means it has`: phase's and await-turn's `context <n>%`, watch's first reading at or below --context-warn (default 10, 0 off) once a descent even mid-turn, `EVENT compacted` on the indicator gone or up 30 points, supervise's stderr lines for its run only and the `aterm drive phase` check before the next supervise) against phase.rs context_left, drive_cli.rs parse_sub/DEFAULT_CONTEXT_WARN/phase_reply and run.rs watch_context/drive/StopAtReview::say; the gate's --diff shows none of it (the bullet quotes no string the gate extracts, so the hash is unchanged), and the bullet was read from git diff; two slips fixed before this row (`gone from the screen`, where a box covering the composer takes the indicator off the screen and is no compaction — now gone from above the composer; the warning said once, now once a descent); 2026-09-14 drift sweep (lane primer, 46 claims checked over the group): re-read against the code that moved under it since this row's anchor commit; every candidate went to a verifier prompted to REFUTE it — no claim contradicted the moved code; 2026-09-14 read of the round-11 additions (the `Keep a --journal too` paragraph under *Set up the worker*, the REVIEW step's `report --final` paragraph ahead of the whole-report one, the watch section's `--journal` bullet, and the new *See how the loop ran* section) against aterm-agent supervise/journal.rs (the record's fields read from the line, append-only, 0600 when created, the single warning), run.rs (what watch prints and what supervise journals for what it decides silently), supervise/blocks.rs view_rows (Final: the last message block and the done row that ended the turn; Messages: every message and `❯` block, no tool row, no `⎿` output) and report.rs render_view (the header kept, `view=`/`kept=` added), and supervise/ledger.rs gather/summary/items for the section's claims about the four sources, the latency numbers, the four lanes and the self-contained page; the two measured facts in it (the notes file holding none of the watcher's decisions; reports of 689 and 249 rows read to find a final message of about 70) are this round's own measurements, recorded in the spec; no claim contradicted; 2026-09-14 read of the paragraphs this round adds, taken from `git diff main` (`Keep a --journal too`, the `report --final` step, the `--journal FILE records every line it prints` bullet and the `See how the loop ran` section), against journal.rs (the kinds, the Unix stamp, the phase and seq read off the line), report.rs render_view with blocks.rs view_rows (the header kept, so `complete=` still answers), and ledger.rs gather/render_ledger with ledger_html.rs for the swimlanes, the SUMMARY counters and `--since`; no claim contradicted; 2026-09-14 (feat/round-14-mail, round 14 D1/D2/D4) read of the --mail and task prose — DRIVE_HELP's synopsis lines, the watch, task and --mail entries and the journal's mail kind and report field (lib.rs); DRIVE_PAGE's watch, task, --mail and --journal entries (manual.rs); run.rs's module doc, SuperviseOpts::mail, Fold, MailIn, NoLane, Sink, the Review, StopAtReview, Lines and folded_event_line docs, supervise_mail, supervise_with, run_loop, watch_mail, watch_with, look's fold gate, hold_for_report, event_line_as, render_result_mail and the six mail test docs; the two skills' loop, watch, task and ledger paragraphs — against mail.rs Lane::serve (an inbox 1 --peek --meta baseline; await inbox since=<newest> kinds=<all nine> timeout <step>, re-armed on OK timeout, the kinds list dropped on ERR usage; inbox since=<id> --peek --meta, then inbox get <id> for the watched worker's report only; the MAIL line per row through the loop's sink; lane_off said once on any other ERR or a lapsed reconnect window), Lane::call's retry of a lost reply (pause doubling to pause_max, within reconnect), task() (post to=@sid kind=task [dl=<ms>] <text> from --inbox or @self, the offset from the OK's off=, one text --json read and `turn idle=600 timeout=2500 Inbox: task @<off>` only on Phase::Idle with submitted=1 as nudged, the inbox 1 baseline read BEFORE the post, await inbox … kinds=answer,report,ack re-armed on rows without re=<off>, the TIMEOUT line and EXIT_TIMEOUT), run.rs hold_for_report (pending drained; a delivery within the window folds at once, else recv_timeout to min(now + grace, deadline); Disconnected gives None so the line is as without the flag), look's Phase::Idle gate with the brief skipped only on Fold::Report, run_loop's thread::scope join (the lane returns within one mail_step) and drive_cli.rs parse_sub (--mail, --report-window and --idle-grace watch's and supervise's only; --inbox needs a leading @; --deadline, --wait and --no-nudge task's only), mail_needs_sid and task_opts (--wait bounded by --deadline, else --timeout); measured on the live worker s-1e918c4662a1b7b8bd43 (busy for the whole 25 s budget: TIMEOUT, the process ending 40 s in as the lane's parked wait was joined within its 20 s step, no MAIL lane off) and by the mock tests; no claim contradicted; 2026-09-15: the loop paragraph's `--report-to \"@$ME\"` attribution corrected from `round 12` to `round 12's hooks, round 14's flag` (the four hooks are round 12's install_claude; the flag is hook.rs's round-14 --report-to) and re-read against hook.rs; the markdown hash does not move (the lexer sees no literal), so the read is recorded here; 2026-09-14 (2026-09-15 UTC) read of the round-14 sections from `git diff main` (the loop's four commands and its `watch --mail` paragraph, the watch section's --mail and --report bullets, the `Assign work by mail` section and the ledger's sentence) against supervise/mail.rs task()/Lane::serve, supervise/run.rs hold_for_report/MailIn::is_this_turns/run_loop, supervise/ledger.rs ended_on_words/MarkKind::of_record (turn and idle-no-report as a stop; a `mail` record listed as MarkKind::Other) and aterm-link hook.rs may_wake/accepted; two claims amended in this commit: the install line gains `--accept-from $ME` with why (the hook wakes for a human or a listed OWNER sid — measured on the owner's live worker, whose hooks list the node id and whose inbox holds the manager's tasks, kind=task, none woken for), and `A worker with round 12's hooks … wakes on its next Stop` now says hooks that accept you; no other claim contradicted; 2026-09-17 (feat/round-17-limits) read of the round-17 additions — the two launch lines' `--resume \"$RULES\"`, the rewritten `limited` EVENT bullet and the new `A usage limit — the worker's, and your own` section (the measured 2026-09-15 16:51 → 09-17 08:55 stall, the plain-process Monitor recipe, the rules file, `/login` in the worker's window as the owner's step) — against supervise/run.rs look/limit_after/escalate/extend/probe/settle_probe/rebrief and drive_cli.rs resume_opts (the escalation, EXTEND, the probe's cue on the screen leaving the notice, EVENT resumed/rebriefed/still-limited, the 10-then-30 backoff, no work invented); no claim contradicted; 2026-09-17 read of the round-17 changes (`git diff main` at the rebase onto ca8ab9aef: `--resume \"$RULES\"` in the three watch lines, the rewritten `limited` bullet and the new `A usage limit — the worker's, and your own` section) against supervise/run.rs escalate/close_episode/extend/probe/settle_probe/rebrief (the attention meta, `kind=control` mail, ESCALATED once, EXTEND 10 min past the reset once, ONE fixed PROBE turn, EVENT resumed/rebriefed/still-limited, the backoff 10 then 30 min, one_line's line breaks as spaces, the rules file read when sent), drive_cli.rs resume_opts (refused unreadable or empty at the launch), aterm-types control_verbs.rs's `history` verb and its `meta` help (`attention` is the escalation the menu-bar status item badges), and SPEC17 §C (a plain process under a Monitor, the rules file, `/login` in the worker's window as the owner's call); no claim contradicted, prose unchanged since the row's hash; 2026-09-17 read of the launch recipe (`spawn identity=worker`, the `detail=claude identity=worker` expectation, the identities/forget lines, the older-build fallback and the plain-session paragraph's identity clause) against control_media::parse_spawn_args, session_status_record's identity= tail and agent_identity::forget_reply; 2026-09-17 read of the merged text at the rebase onto main (git diff main: the identity recipe and its plain-session clause) beside main's round-17 usage-limit section, against control_media::parse_spawn_args, session_status's status line (detail= early, identity= last) and agent_identity::forget_reply; three sentences fixed before this row: the status comment implied detail= and identity= adjacent, the sign-in-once sentence claimed the human-run measurement the docs record as a TODO (now an expectation), and step 3 of the limit section still gave `/login` in the worker's window as the whole step where the identity recipe above is now what keeps the logins apart; the gate hashes a .md through the Rust prose lexer, so only its quoted spans count and these edits move no hash — read from the diff, not the hash",
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
        "Metal Objective-C FFI bindings for a library crate (no fn main, no println!/eprintln!, no usage/help string). The USAGE hits are the MTLTextureUsage bitmask constants TEXTURE_USAGE_SHADER_READ / TEXTURE_USAGE_RENDER_TARGET / TEXTURE_USAGE_PIXEL_FORMAT_VIEW (lines 671-676) and the `setUsage:` / `usage` selectors (1061-1062, 1802).",
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
        "crates/aterm-winsign/src/main.rs",
        "a bin shim: its help is USAGE in crates/aterm-winsign/src/lib.rs (rostered). Its own prose is the three-line module doc; it parses nothing, forwarding argv straight to aterm_winsign::run and exiting with the code that returns (read 2026-09-15)",
    ),
    (
        "crates/aterm/src/main.rs",
        "the front door delegates its --help to HELP_HEAD in crates/aterm-cli/src/lib.rs (rostered); its own printed strings (update status|check usage, the windowing warning, the reroute and staged-update notices, the never-checked line) were read against their code 2026-09-10 and one false platform claim fixed",
    ),
    (
        "crates/atpkg/src/main.rs",
        "a bin shim: its help is VERB_USAGE and cmd_help in crates/atpkg/src/cli.rs (rostered)",
    ),
    // HELP_SURFACES_NOT_HELP_END
];

/// A surface's subject is its OWN package by default — the nearest `Cargo.toml`
/// above it. These rows add the packages a surface ALSO describes, and they are
/// the whole point of [`drift_report`].
///
/// WHY. The 2026-09-12 drift sweep grouped moved code by crate and re-read that
/// crate's surfaces, so a surface describing ANOTHER crate's code was never
/// reached. Measured cost: three false `fabric` claims sat in `aterm-cli`'s
/// manual for days because they describe `aterm-gui`'s `fabric.rs`, and
/// `aterm-cli` had not moved. A whole-manual read on 2026-09-14 found 22 more of
/// the same shape. An edge here means the report names the surface when the
/// package it describes moves, whether or not its own package did.
///
/// Adding an edge is cheap and never wrong to add: this is a WORKLIST, not a
/// refusal, so a subject that turns out not to matter costs one line of report.
type DescribesRow<'a> = (&'a str, &'a [&'a str]);

const DESCRIBES: &[DescribesRow<'static>] = &[
    // `aterm help <topic>` — almost every page describes another crate.
    (
        "crates/aterm-cli/src/manual.rs",
        &[
            "crates/aterm",
            "crates/aterm-agent",
            "crates/aterm-gui",
            "crates/aterm-link",
            "crates/aterm-primer",
            "crates/aterm-release",
            "crates/aterm-types",
            "crates/aterm-verify",
            "crates/atpkg",
        ],
    ),
    // The primer block and the four skill assets are written INTO agents' context
    // files; they describe the control server, the fabric and the toolchain seams.
    (
        "crates/aterm-primer/src/lib.rs",
        &[
            "crates/aterm-agent",
            "crates/aterm-ctl",
            "crates/aterm-gui",
            "crates/atpkg",
        ],
    ),
    // `aterm help config` states how many keys the starter ships; the starter is
    // aterm-gui's.
    ("crates/aterm-gui/src/cli.rs", &["crates/aterm-cli"]),
    // The ctl client's help describes the verbs the GUI's control server serves.
    ("crates/aterm-ctl/src/lib.rs", &["crates/aterm-gui"]),
    // `aterm fabric` reports what the GUI's fabric verbs answer (`fabric status`,
    // the inbox header, the timeline's hold rows) over aterm-ctl's discovery.
    (
        "crates/aterm-link/src/fabric.rs",
        &["crates/aterm-ctl", "crates/aterm-gui"],
    ),
    (
        "crates/aterm-types/src/control_verbs.rs",
        &["crates/aterm-gui"],
    ),
    // xtask's gate prose describes what the verify stages run.
    ("crates/xtask/src/main.rs", &["crates/aterm-verify"]),
    ("crates/xtask/src/gate.rs", &["crates/aterm-verify"]),
];

/// What a drift SWEEP already answered, per surface: the commit it asked up to.
///
/// A sweep reads a surface against its subject's diff and often concludes the
/// change reaches NONE of its claims. That is a real answer, and it is narrower
/// than a read against the handler — so it must not move the row's date, which
/// is what [`MAX_AGE_DAYS`] measures. Without somewhere to put it the next
/// sweep re-derives the same negative, and the one after that too: on
/// 2026-09-14 nine of the thirty-six surfaces [`drift_report`] named were
/// exactly this case.
///
/// [`drift_report`] counts from whichever of the row's own commit and its sweep
/// bound is LATER, so the worklist converges. A bound naming a commit this
/// repository does not have is REFUSED by
/// `every_swept_row_names_a_rostered_surface_and_a_real_commit` rather than
/// ignored — an ignored bound would make the surface look swept for ever, which
/// is the one way this table can do harm.
type SweptRow<'a> = (&'a str, &'a str);

const SWEPT: &[SweptRow<'static>] = &[
    // The 2026-09-14 drift sweep (429 claims over nine groups) read these and
    // found the moved code reaches none of their claims.
    ("crates/aterm-gui/src/menu.rs", "f59b2b700"),
    ("crates/aterm-gui/src/operator_host.rs", "f59b2b700"),
    (
        "crates/aterm-primer/assets/rust-in-aterm-skill.md",
        "f59b2b700",
    ),
    ("crates/aterm-agent/src/fleet_cli.rs", "f59b2b700"),
    ("crates/aterm-cli/src/lib.rs", "f59b2b700"),
    ("crates/aterm-link/src/mirror.rs", "f59b2b700"),
    ("crates/aterm-link/src/notify.rs", "f59b2b700"),
    ("crates/aterm-link/src/tui.rs", "f59b2b700"),
    ("crates/aterm-verify/src/lib.rs", "f59b2b700"),
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

const DIFF_USAGE: &str = "usage: xtask gate help-surfaces [--diff PATH]... | --drift\n  \
    no argument   the gate itself\n  \
    --diff PATH   not the gate: the prose PATH changed since the read its row records, \
    item by item as the gate hashes it (PATH as the row spells it; repeatable)\n  \
    --drift       not the gate: which read-verified surfaces describe code that has \
    MOVED since they were read — the maintenance worklist";

/// `xtask gate help-surfaces [--diff PATH]...`. With no argument, the gate (the
/// `all` roster calls [`gate_help_surfaces`] directly). Each `--diff PATH` prints
/// [`prose_diff_report`] to stdout instead — a reading aid, not a verdict: it
/// fails only when it cannot produce the diff.
pub(crate) fn gate_help_surfaces_args(rest: &[String]) -> bool {
    if rest.is_empty() {
        return gate_help_surfaces();
    }
    if rest.len() == 1 && rest[0] == "--drift" {
        let root = crate::workspace_root();
        let (produced, report) = drift_report(&root, SURFACES, DESCRIBES, SWEPT, ROSTER_FILE);
        print!("{report}");
        return produced;
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
/// `--drift`: which read-verified surfaces describe code that MOVED since they
/// were read. A REPORT, never a refusal.
///
/// Why not a refusal. These repos advance dozens of commits a day, so a gate
/// keyed on upstream motion would be permanently red and would teach
/// `--no-verify` — which costs more than the drift it names. The gate refuses an
/// unread or changed SURFACE; this verb hands the next sweep its worklist.
///
/// A row's read is anchored the way `--diff` anchors it: the newest commit whose
/// version of the file hashes to the row, falling back to the commit that last
/// touched the row in `roster_file` (and saying which). Its subject is its own
/// package — the nearest `Cargo.toml` above it — plus every package
/// [`DESCRIBES`] adds for it. A subject with commits in `read..HEAD` is drift.
fn drift_report(
    root: &Path,
    surfaces: &[SurfaceRow<'_>],
    describes: &[DescribesRow<'_>],
    swept: &[SweptRow<'_>],
    roster_file: &str,
) -> (bool, String) {
    let mut out = String::new();
    let mut blobs = match Blobs::spawn(root) {
        Ok(b) => b,
        Err(e) => {
            let _ = writeln!(out, "drift: {e}");
            return (false, out);
        }
    };
    let mut moved = 0usize;
    let mut cross = 0usize;
    for (rel, recorded, date, _) in surfaces {
        let anchor = match read_anchor(root, rel, recorded, roster_file, &mut blobs) {
            Ok(a) => a,
            Err(e) => {
                let _ = writeln!(out, "drift: {rel}: {e}");
                return (false, out);
            }
        };
        let anchor = match swept.iter().find(|(p, _)| p == rel) {
            // The later of the two bounds wins: a sweep that ran BEFORE the row
            // was last touched answers a question the row already supersedes.
            Some((_, bound)) => match anchor {
                Some((sha, exact)) => match is_ancestor(root, &sha, bound) {
                    Ok(true) => Some(((*bound).to_string(), true)),
                    Ok(false) => Some((sha, exact)),
                    Err(e) => {
                        let _ = writeln!(out, "drift: {rel}: {e}");
                        return (false, out);
                    }
                },
                None => Some(((*bound).to_string(), true)),
            },
            None => anchor,
        };
        let Some((sha, exact)) = anchor else {
            let _ = writeln!(
                out,
                "drift: {rel} (read {date}) — no commit anchors this read, so nothing can be \
                 diffed against it; re-read it",
            );
            moved += 1;
            continue;
        };
        let own = own_package(root, rel);
        let extra: Vec<&str> = describes
            .iter()
            .filter(|(p, _)| p == rel)
            .flat_map(|(_, subs)| subs.iter().copied())
            .filter(|s| *s != own)
            .collect();
        if !extra.is_empty() {
            cross += 1;
        }
        let mut hits: Vec<(String, usize)> = Vec::new();
        for subject in std::iter::once(own.as_str()).chain(extra.iter().copied()) {
            match commits_touching(root, &sha, subject) {
                Ok(0) => {}
                Ok(n) => hits.push((subject.to_string(), n)),
                Err(e) => {
                    let _ = writeln!(out, "drift: {rel}: {e}");
                    return (false, out);
                }
            }
        }
        if hits.is_empty() {
            continue;
        }
        moved += 1;
        let subjects = hits
            .iter()
            .map(|(s, n)| format!("{s} ({n} commit(s))"))
            .collect::<Vec<_>>()
            .join(", ");
        let how = if exact {
            "row recorded"
        } else {
            "prose last moved"
        };
        let _ = writeln!(
            out,
            "drift: {rel} ({how} {date} @ {}) — subject moved: {subjects}",
            short_sha(&sha),
        );
    }
    let _ = writeln!(
        out,
        "drift: {moved} of {} surface(s) describe code that moved since they were read \
         ({cross} row(s) declare a cross-crate subject beyond their own)",
        surfaces.len(),
    );
    (true, out)
}

/// The commit a row's read is anchored to: `(sha, exact)`.
///
/// FIRST the commit that last touched the row in `roster_file`: a row is a dated
/// assertion, and the commit that RECORDED it is when the read happened. Failing
/// that, the newest commit whose version of the file hashes to the row — the
/// text that was read, though it may have been committed long before the
/// reading, which is why it is the fallback and not the first choice. Measured
/// 2026-09-14: taking the hash match first left `drive-aterm-skill.md` anchored
/// three commits before the row that re-read it, so the sweep's own work showed
/// as drift.
fn read_anchor(
    root: &Path,
    rel: &str,
    recorded: &str,
    roster_file: &str,
    blobs: &mut Blobs,
) -> Result<Option<(String, bool)>, String> {
    if let Some(sha) = row_commit(root, roster_file, rel)? {
        return Ok(Some((sha, true)));
    }
    let listing = git_stdout(root, &["log", "--format=%H", "--", rel])?;
    for sha in listing.lines().filter(|l| !l.is_empty()) {
        if let Some(src) = blobs.read(&format!("{sha}:{rel}"))?
            && source_prose_hash(&src) == recorded
        {
            return Ok(Some((sha.to_string(), false)));
        }
    }
    Ok(None)
}

/// Is `a` an ancestor of (or equal to) `b`?
fn is_ancestor(root: &Path, a: &str, b: &str) -> Result<bool, String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["merge-base", "--is-ancestor", a, b])
        .output()
        .map_err(|e| format!("could not run `git merge-base`: {e}"))?;
    Ok(out.status.success())
}

/// How many commits in `since..HEAD` touch `subject`.
fn commits_touching(root: &Path, since: &str, subject: &str) -> Result<usize, String> {
    let listing = git_stdout(
        root,
        &[
            "log",
            "--format=%H",
            &format!("{since}..HEAD"),
            "--",
            subject,
        ],
    )?;
    Ok(listing.lines().filter(|l| !l.is_empty()).count())
}

/// The package `rel` belongs to: the nearest ancestor directory holding a
/// `Cargo.toml`, workspace-relative. The workspace root itself when there is
/// none above it.
fn own_package(root: &Path, rel: &str) -> String {
    let mut dir = Path::new(rel).parent();
    while let Some(d) = dir {
        if d.as_os_str().is_empty() {
            break;
        }
        if root.join(d).join("Cargo.toml").is_file() {
            return d.to_string_lossy().replace('\\', "/");
        }
        dir = d.parent();
    }
    ".".to_string()
}

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

    /// Every [`DESCRIBES`] row names a surface the roster actually carries and
    /// packages that actually exist. A cross-crate edge that has rotted is worse
    /// than none: `--drift` would silently stop naming the surface when the crate
    /// it describes moves, which is the exact failure the table exists to prevent.
    #[test]
    fn every_describes_row_names_a_rostered_surface_and_a_real_package() {
        let root = crate::workspace_root();
        let rostered: std::collections::BTreeSet<&str> =
            SURFACES.iter().map(|(p, ..)| *p).collect();
        for (path, subjects) in DESCRIBES {
            assert!(
                rostered.contains(path),
                "DESCRIBES names `{path}`, which is not a rostered surface — a row for a \
                 file the gate does not track is never consulted"
            );
            assert!(
                !subjects.is_empty(),
                "DESCRIBES row for `{path}` adds no subject; delete it instead"
            );
            for subject in *subjects {
                assert!(
                    root.join(subject).join("Cargo.toml").is_file(),
                    "DESCRIBES says `{path}` describes `{subject}`, which is not a package \
                     (no Cargo.toml) — a subject git cannot log is drift nobody is told about"
                );
            }
        }
    }

    /// Every [`SWEPT`] row names a surface the roster carries and a commit this
    /// repository has. A bound that resolves to nothing is IGNORED by
    /// [`drift_report`], so the surface would look swept for ever and never be
    /// named again — the one way this table can do harm, and the reason it needs
    /// a check rather than a comment.
    #[test]
    fn every_swept_row_names_a_rostered_surface_and_a_real_commit() {
        let root = crate::workspace_root();
        let rostered: std::collections::BTreeSet<&str> =
            SURFACES.iter().map(|(p, ..)| *p).collect();
        for (path, bound) in SWEPT {
            assert!(
                rostered.contains(path),
                "SWEPT names `{path}`, which is not a rostered surface — a bound for a file the \
                 gate does not track is never consulted"
            );
            let ok = std::process::Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(["rev-parse", "--verify", &format!("{bound}^{{commit}}")])
                .output()
                .expect("git runs")
                .status
                .success();
            assert!(
                ok,
                "SWEPT says `{path}` was swept at `{bound}`, which this repository has no commit \
                 for — an unresolvable bound is silently ignored, so the surface would look swept \
                 for ever"
            );
        }
    }

    /// A surface's own subject is the package it lives in, not the workspace and
    /// not its own directory.
    #[test]
    fn own_package_walks_up_to_the_nearest_cargo_toml() {
        let root = std::env::temp_dir().join(format!(
            "xtask-help-surfaces-{}-own-package",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("crates/t/src/deep")).unwrap();
        std::fs::write(
            root.join("crates/t/Cargo.toml"),
            "[package]\nname = \"t\"\n",
        )
        .unwrap();
        assert_eq!(own_package(&root, "crates/t/src/deep/cli.rs"), "crates/t");
        assert_eq!(own_package(&root, "crates/t/src/cli.rs"), "crates/t");
        // Nothing above it: the walk ends rather than climbing out of the tree.
        assert_eq!(own_package(&root, "docs/notes.rs"), ".");
        let _ = std::fs::remove_dir_all(&root);
    }
}
