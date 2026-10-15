// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WALL IS CONFIRMED BEFORE IT IS ACTED ON — the transcript's half of
//! H6-HARNESS-1 (ruling 2026-10-08).
//!
//! The screen's reader ([`aterm_phase::wall`], `phase::block_is_vendors`)
//! reads a wall for RECALL, and cannot tell some of a tool's output from
//! Claude Code's own row: an MCP result of several content blocks (each its
//! own column-2 `⎿` row in verbose output, where an MCP result and then a
//! wall look the same), the output under a PreToolUse hook's row, a long
//! output whose call row has scrolled off, the worker's own words. Acted
//! on, such a misreading typed `/login` or
//! `/compact` into the session, parked it until a reset, moved it to
//! another account or raised the login-gone alarm, on the say of whoever
//! wrote a file, a page or a server's reply. So the turn-end policy acts on a
//! wall only where the session's own Claude Code transcript (its tail, from
//! the host: [`IdleHost::transcript_tail`]) CONFIRMS it: its newest
//! main-chain entry ([`transcript_ends_on`]) is Claude Code's own notice row
//! of the screen wall's kind ([`confirms`]). A wall it does not confirm is
//! not acted on: one journal line says so (`SKIPPED seq=<n>
//! rule=wall-confirm@v1 unconfirmed wall: <kind> — the transcript ends on
//! <entry>: <the screen's words>`, once a wall), and the point is an
//! ordinary turn end — its reading carries no wall, a limit's point no
//! limit. Claude Code's critical-memory banner is no transcript row and no
//! tool's output (it is read by position in the live zone), and a Codex
//! writes no such transcript: neither is asked.
//!
//! WHERE THE TRANSCRIPT IS: the host finds it the way the window's footer
//! does — from the agent's own PROCESS (its exec environment's
//! `CLAUDE_CONFIG_DIR`, else its `HOME`'s `.claude`; the record that
//! directory's `sessions/<pid>.json` keeps of it, its start time checked;
//! the project's own `projects/<slug>/<id>.jsonl` first), so a Claude Code
//! launched with a config directory of its own is confirmed like any other
//! (`upgrade_drive::process_transcript_tail`).
//!
//! NO TRANSCRIPT (no host — `aterm drive watch`, `aterm drive supervise` —,
//! no record of the process yet, the conversation not written yet, two
//! transcripts of its id and none in its project's own place, a file that
//! cannot be read): a wall the screen read UNDER A TOOL'S CALL
//! ([`aterm_phase::Wall::under_call`]: a later column-2 row under a call the
//! owner walk recognises — `⏺ Name(…)`, an MCP tool's row, a shell-mode `!
//! cmd`, an integration's `Name[tool](…)` — a hook's row above the output or
//! not) is not acted on, said as `… unconfirmed wall: <kind> — read under a
//! tool's call, and no transcript to confirm it: …`; any other wall — under
//! the person's turn, a notice, the worker's message, a collapsed group, or
//! no owner the walk reaches — is acted on as the screen reads it. The walk
//! is given EVERY ROW THE SCREEN SHOWS for it: the loop reads the screen's
//! tail (`tail=40`, the server's own agent-verdict cut, which every other
//! read keeps), and a tail can hold the wall without the call above it, so
//! a gutter wall read on a tail with no call over it is read again from the
//! whole grid before it is acted on ([`Session::under_call_on_screen`]).
//!
//! RULING 2026-10-08 (H6-HARNESS-1, pass 5): with no transcript, a wall
//! under a recognised tool call — an MCP call's and a `!` command's
//! included — is not acted on. Main DID act on walls under MCP and `!`
//! calls: its walk did not know them as calls, and read every `⎿` row under
//! them as Claude Code's, the output's first row included. So this is a
//! KNOWING LOSS OF RECALL on the hostless paths (`aterm drive watch`, `aterm
//! drive supervise`) and for a session whose transcript cannot be found — a
//! real context wall after an MCP tool's output gets no `/compact` there,
//! where main typed one (measured in the lab) — accepted to refuse the MCP
//! multi-block and PreToolUse-hook spoofs there, which nothing on the screen
//! tells from a wall. Under a `⏺ Name(…)` call main read no wall, but for
//! one behind a blank row of the output (below).
//!
//! EXTENDED THE SAME DAY (pass 6, the review of pass 5): the screen's owner
//! walk goes on over a BLANK ROW INSIDE A CALL'S ROWS to the call
//! (`phase::block_is_vendors`): a blank row ends the walk only where the next
//! row below opens an item of its own, a row at column 2 or less that is no
//! `⎿` row (a prompt's second paragraph, a collapsed group's row). Under a
//! row past column 2 it is a blank line of the output (column 5) or of the
//! command (a heredoc's, column 6); under a column-2 `⎿` row it ends a block
//! of the output (no measured screen draws Claude Code's own `⎿` row behind a
//! blank row under a call's rows; after a `/compact` it draws one under its
//! own `✻ Conversation compacted` row, which no call owns). The review saw `/login` typed with no transcript (`aterm drive
//! watch`, 2.1.294) on an MCP result whose first block holds a paragraph
//! break, as real issue bodies and pages do, and pass 6 saw it again on one
//! whose first block ends on a blank line (two blank rows right above the
//! second block): the walk stopped at the blank row and read the second block
//! under no owner, as main's did. Now it reads it under the call, and refuses
//! it there; a heredoc command's output, read the same way by main and pass
//! 5, is the first row under its call, and no wall. The price, by the same
//! ruling: Claude Code's real wall under an output that holds a blank row (a
//! Bash output's, measured) is read under the call too, and with no
//! transcript gets no `/compact`, where main and pass 5 typed one. What the
//! rule does not reach, as on main: a tool's output whose call row has
//! scrolled off the screen is read with no owner, and acted on.
//!
//! RESIDUALS, known and left: (1) THE FOOTER AFTER `--continue`. Claude
//! Code's own footer `Not logged in · Run /login` (read as a lost login,
//! [`aterm_phase::Placement::Footer`]), drawn after a `--continue` onto a
//! conversation that ended on the model's reply, is not acted on with a host:
//! the transcript ends on that reply (measured by the review of pass 5,
//! 2.1.294: `… unconfirmed wall: auth — the transcript ends on the model's
//! reply: Not logged in · Run /login`). Nothing is done until a request fails
//! and writes the lost login's notice, which confirms it: the continuation's
//! carry-on, where it types one, is such a request; with the continuation
//! off, the person's next turn is. The footer's placement is NOT exempted: a
//! custom statusline prints whatever its command prints there, a tool's or a
//! page's words included. (2) A wall a screen reads under no owner where the
//! call row has scrolled off the whole screen (above). (3) The tab still
//! publishes `agent=wall:<kind>` for a wall refused here: the window's agent
//! verdict (aterm-gui, `presence::agent_verdict`) reads the screen, not the
//! transcript.
//!
//! THE DECISION'S READ ONLY: the transcript is asked (and, with none, the
//! whole screen read) at the reads the point is DECIDED on
//! ([`Check::Decide`]: a new point, a wait run out — on an unmoved screen
//! too, where a transcript that had not caught up may have since — a
//! switch's break). The loop reads the same point again for its own books
//! after the decision — what an act was taken at (`TurnEndState::acted`),
//! the restart made, waited for or not made, the accounts probe's gate —
//! and by then the transcript has moved on, as it should: the carry-on the
//! act typed, the relaunched conversation. Asked there, it refused the
//! wall just acted on (a model bucket's relaunch lost the return its reset
//! scheduled: the review of pass 3). So those reads keep the DECISION'S
//! VERDICT for that screen ([`Check::AsDecided`], by its `seq`), confirmed
//! or refused, and `decide_turn_end` stays a pure function of the reading
//! it is handed.
//!
//! WHERE: at the seam between the loop's reading and the decider
//! ([`Session::confirm_wall`], called by `turn_end_reading` on the screen's
//! reading before anything reads its wall), so every reader downstream of
//! it — the decider, the policy's memory of the wall
//! (`TurnEndState::observe`), the wall the loop remembers while its rows are
//! out of view, the reset it places, the API's reach it asks for, the
//! account advice, the exceptions engine — sees the same wall or none. A
//! flag in the reading would have had each of them check it.
//!
//! WHAT CLAUDE CODE 2.1.294 WRITES (measured in `tools/real-claude-lab`,
//! 2026-10-08): every wall the policy acts on is an `assistant` row of the
//! `<synthetic>` model with `isApiErrorMessage: true`, the notice as its
//! text and an `error` word, then a `system` row `turn_duration`:
//! * a 529, `API Error: Repeated 529 Overloaded errors. The API is at
//!   capacity …` (`server_error`, `apiErrorStatus` 529); a 500, `API Error:
//!   500 Internal server error. …` (`server_error`, 500);
//! * the API unreachable, `API Error: Connection refused — a firewall or
//!   proxy may be blocking it (ECONNREFUSED)` (`server_error`, no status);
//! * a full context, `Prompt is too long` (`invalid_request`, 400), while
//!   the screen draws `Context limit reached · /compact or /clear to
//!   continue` — whether the request followed the person's turn or a tool's
//!   result;
//! * no login, `Not logged in · Please run /login` (`authentication_failed`,
//!   no status); a key the API refused, `Invalid API key · Fix external API
//!   key` (`authentication_failed`, 401);
//! * the session and weekly limits, `You've hit your session limit · resets
//!   11:33am (America/Los_Angeles)` and `You've hit your weekly limit ·
//!   resets Oct 11 at 9:35am (…)` (`rate_limit`, 429, `quotaLimits`), the
//!   session limit's auto-continue after it as a `system` `informational`
//!   row (`Usage limit reached · continuing automatically at 11:33am · esc to
//!   cancel`, drawn as a `⏺` row); an API key's rate limit, `API Error:
//!   Request rejected (429) · …` (`rate_limit`, 429). A model bucket and a
//!   spend limit are built by the same call (`error: "rate_limit"`, read in
//!   the binary), not driven.
//!
//! THE ORDER IS THE FILE'S, with ONE measured exception: on the first turn
//! of a relaunched Claude Code, the session limit's notice, its
//! auto-continue row and its `turn_duration` were written BEFORE the prompt
//! row they answered — the conversation's first row, the one with no parent
//! (`parentUuid: null`). Such a row opens the conversation, and is read so
//! wherever the file holds it ([`transcript_ends_on`]). Rows are not ordered
//! by their own `timestamp`s: a clock stepped back would have kept an old
//! notice "newest" through the step, confirming spoofs of its class. A
//! row's stamp is read once, against the loop's own clock: a LIMIT A CANCEL
//! LEFT that the loop reads as the answer to a turn — its own act, or anyone's
//! asked since the policy last looked (`TurnEndState::left_answer`) — is
//! confirmed only by a notice row written since that turn was asked, less
//! [`LEFT_ANSWER_SLACK_S`]: a conversation `/resume`d meanwhile ends on its
//! own older cancelled limit, of the same class. Where the screen cannot
//! tell that notice from the one the policy already saw (the same limit
//! answering again, the same reset: `LeftAnswer::screen_tells`), that row,
//! and its stamp, are the only evidence: with no transcript, or a row that
//! names no time, it is refused with no word (the screen read no wall). A
//! clock stepped back refuses a real answer there (the continue policy's
//! next act meets the limit, as before the answer was read); it confirms
//! nothing.
//!
//! LOCAL COMMANDS ARE NO TURNS (measured on 2.1.294, 2026-10-08, each command
//! opened and left with Esc;
//! `fixtures/claude-2.1.294-transcript-local-commands.jsonl`). A slash
//! command run in Claude Code writes its invocation and its output, in one of
//! two ways. `/model`, `/status`, `/usage`, `/mcp` and a `/config` that
//! changed nothing write two `system` `local_command` rows
//! (`<command-name>/model</command-name>…`, then `<local-command-stdout>Kept
//! model as …`, `Settings dialog dismissed`, `MCP dialog dismissed`, `Config
//! dialog dismissed`). A dismissed `/login`, a `/config` that changed a
//! setting and `/compact` write `user` rows: a meta caveat
//! (`<local-command-caveat>…`, `isMeta`), the invocation
//! (`<command-name>/login</command-name>…`) and the output
//! (`<local-command-stdout>Login interrupted`, `Disabled auto-compact`,
//! `Compacted`); `/compact` writes its bare name (`/compact`, its arguments
//! after it) as a `user` row first, and once it has compacted a
//! `compact_boundary` and the summary (`isCompactSummary`). A PROMPT command
//! (`/doctor`, measured by the review of pass 5) writes a `user` row that
//! opens `<command-message>doctor</command-message>`, then its prompt as a
//! meta row; the model's reply, or the notice of a request that failed,
//! follows it as for any turn. None of these rows is a turn of the
//! conversation's, and every one is PASSED OVER — never a notice, never an
//! entry — so a person who opens `/usage`, `/login` or `/model` at a standing
//! limit leaves the limit's notice the newest entry (the review of pass 4:
//! read as "a user turn", the real limit was refused). A tool's output cannot
//! write a `user` row of these shapes (it is a `tool_result` part); a
//! shell-mode `!` command writes `<bash-input>…` and `<bash-stdout>…` rows,
//! which stay the person's turn.
//!
//! THE ONE LOCAL COMMAND THAT IS A NOTICE: a `/compact` that FAILED (the
//! context wall the policy meets again after its own `/compact`) writes no
//! notice row of the API's but, after its own rows, a `system`
//! `local_command` row, `<local-command-stderr>Compaction failed ·
//! conversation could not be reduced below the context limit
//! </local-command-stderr>`. That row is Claude Code's own — a `system`
//! row, which neither a tool's output nor a person's prompt is ever written
//! as — so a `system` local command's output that reads as a full context
//! is taken for the context wall's notice; a `user` row's never is.
//!
//! WHAT A TOOL'S OUTPUT LEAVES (measured the same day): a `user` row
//! holding a `tool_result` part, then the model's reply — an empty one is
//! still an `assistant` row of the model's, with Claude Code's meta nudge
//! (`Your previous response had no visible output…`) and a second empty row
//! after it; a shell-mode command: `user` rows `<bash-input>…` and
//! `<bash-stdout>…</bash-stdout>`, then the reply. None of them is a
//! notice, so the screen's wall in that output is unconfirmed.
//!
//! THE LAG: the notice row reached the transcript within one poll (70-110
//! ms) of the screen drawing it, in five trials (a context wall, a 529) —
//! and the hosted loop, which decides the moment the busy row leaves,
//! refused one real wall in the lab as ending on "the model's reply" (a
//! context wall after an MCP call, 2026-10-08; the tail it read was not
//! kept, and the turn's one reply was the model's tool call, so the tail
//! was most likely cut before the tool's result and the notice). Claude Code
//! closes every turn it ends, a wall's included, with a `turn_duration` row
//! after the notice (measured on each wall above and after each tool's
//! output), so a tail that ends on anything but a notice with no
//! `turn_duration` after it has not caught up: it is read again,
//! [`LAG_TRIES`] times [`LAG_STEP`] apart, before the wall is refused. A
//! turn stopped with Esc writes no `turn_duration`, and its wall (if the
//! screen reads one) is refused a second late.

use aterm_json::Value;
use aterm_phase::WallKind;

use super::super::policy::turn_end::LeftAnswer;
use super::*;

/// The rule an unconfirmed wall's journal line is written under.
pub(super) const RULE_WALL_CONFIRM: &str = "wall-confirm@v1";

/// How many times a transcript that has not caught up with the screen is
/// read again before its wall is refused ([`TranscriptEnd::lagging`]).
pub(super) const LAG_TRIES: u32 = 4;

/// How long apart those reads are: the four cover ten times the lag
/// measured on 2.1.294.
pub(super) const LAG_STEP: Duration = Duration::from_millis(250);

/// The newest main-chain entry of a Claude Code transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Entry {
    /// Nothing of the conversation's in the tail read.
    Nothing,
    /// Claude Code's own notice row: an `isApiErrorMessage` row (its text
    /// and `error` word), or a local command's output that reads as a full
    /// context (a failed `/compact`).
    Notice { text: String, error: Option<String> },
    /// A tool's result: a `user` row holding a `tool_result` part.
    ToolResult,
    /// A turn of the person's (or the harness's): a prompt, a shell-mode
    /// command and its output.
    Input,
    /// The model's own words or tool call.
    Reply,
}

impl Entry {
    /// The entry, as the journal line names it.
    fn word(&self) -> String {
        match self {
            Entry::Nothing => "nothing of the conversation's".to_string(),
            Entry::Notice { text, .. } => format!("a notice of its own ({})", clip(text)),
            Entry::ToolResult => "a tool's result".to_string(),
            Entry::Input => "a user turn".to_string(),
            Entry::Reply => "the model's reply".to_string(),
        }
    }
}

/// What a transcript's tail ends on ([`transcript_ends_on`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TranscriptEnd {
    /// The newest main-chain entry.
    pub(super) last: Entry,
    /// Claude Code closed the turn after it: a `system` row `turn_duration`.
    pub(super) closed: bool,
    /// The id (`uuid`) of the row the newest entry was read from (`None`: no
    /// entry, or a row that carries none): a usage limit's notice confirmed
    /// is remembered by it ([`served_since_notice`]).
    pub(super) row: Option<String>,
    /// When that row says it was written (its own `timestamp`, unix
    /// seconds; `None`: none, or none the parser reads): what tells a limit
    /// a cancel left in answer to the loop's act from an older one
    /// ([`Session::judge_wall`]).
    pub(super) at: Option<i64>,
}

impl TranscriptEnd {
    /// The transcript has not caught up with the turn the screen ended: no
    /// `turn_duration` row closed it, and its newest entry is no notice —
    /// the turn's input (a prompt, a tool's result), the model's call or
    /// words the rest of the turn follows, or nothing. A notice settles it,
    /// and so does a closed turn.
    #[must_use]
    pub(super) fn lagging(&self) -> bool {
        !self.closed && !matches!(self.last, Entry::Notice { .. })
    }
}

/// One row of the main chain, as [`transcript_ends_on`] reads it.
enum Row {
    /// An entry of the conversation's.
    Entry(Entry),
    /// Claude Code closed the turn: a `system` row `turn_duration`.
    Closed,
}

/// What the main chain of `jsonl` (a transcript's tail; a torn first line is
/// passed over) ends on: its newest entry — an `assistant` row (a notice
/// when `isApiErrorMessage`, else the model's reply), a `user` row (a tool's
/// result, or a turn: a prompt, a shell-mode command and its output) — and
/// whether a `turn_duration` row closed the turn after it. Not entries: a
/// subagent's rows (`isSidechain`), Claude Code's meta rows (`isMeta`: a
/// local command's caveat, its nudge after an empty reply), a compaction's
/// summary (`isCompactSummary`), a local command's own rows, `user` or
/// `system` (its invocation, its output: [`local_command`]) — except a
/// `system` row's output that reads as a full context, which is the context
/// wall's notice (module docs) — and every other row (attachments, a
/// `system` row of another kind, the mode and snapshot rows).
///
/// NEWEST BY ITS PLACE IN THE FILE, but for the ONE measured exception
/// (module docs): a prompt row with no parent (`parentUuid: null`) opens the
/// conversation, and written after rows of the conversation's — 2.1.294
/// wrote a relaunched Claude Code's first prompt after the session limit's
/// notice, auto-continue and `turn_duration` that answered it (the lab,
/// 2026-10-08) — it is older than they are, and ends nothing.
pub(super) fn transcript_ends_on(jsonl: &str) -> TranscriptEnd {
    let mut end = TranscriptEnd {
        last: Entry::Nothing,
        closed: false,
        row: None,
        at: None,
    };
    for line in jsonl.lines() {
        let Ok(v) = crate::harness::transcript::conversation(line) else {
            continue;
        };
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        match row_of(&v) {
            Some(Row::Closed) => end.closed = true,
            Some(Row::Entry(entry)) => {
                if entry == Entry::Input && opens_the_conversation(&v) && end.last != Entry::Nothing
                {
                    continue;
                }
                end = TranscriptEnd {
                    last: entry,
                    closed: false,
                    row: row_id(&v),
                    at: row_time(&v),
                };
            }
            None => {}
        }
    }
    end
}

/// A row's own id (`uuid`).
fn row_id(v: &Value) -> Option<String> {
    v.get("uuid").and_then(Value::as_str).map(str::to_owned)
}

/// When a row says it was written: its own `timestamp`, unix seconds.
fn row_time(v: &Value) -> Option<i64> {
    v.get("timestamp")
        .and_then(Value::as_str)
        .and_then(crate::harness::upgrade_models::parse_utc)
        .and_then(|t| i64::try_from(t).ok())
}

/// How much earlier than the loop's own record of its act the notice that
/// answered it may say it was written: the act is recorded once the server
/// has taken the turn, and the stub's 429 is written within milliseconds of
/// the submit (2.1.299: 19 ms after the prompt's own row). An older notice —
/// the history a `/resume`d conversation ends on — is minutes or days older.
pub(super) const LEFT_ANSWER_SLACK_S: i64 = 10;

/// A TURN SERVED SINCE THE NOTICE ROW `notice` (its `uuid`), in `jsonl` (a
/// transcript's tail): that row is in it, and the newest main-chain answer
/// of the API's after it is a REPLY of the model's — an `assistant` row that
/// is no notice (`isApiErrorMessage`) and not Claude Code's own
/// (`<synthetic>`), the API answering a request made after the limit: the
/// limit lifted. A notice after that reply (the next request met the limit
/// again) takes it back. Nothing else says so: a prompt (a person's, the
/// harness's own `[aterm harness] …`) the limit may still answer, and the
/// rows Claude Code writes itself — a `<task-notification>` when a
/// background task ends (measured in the lab on 2.1.300, in the same second
/// as the 429 its wake met), a local command's rows. A tail that does not
/// hold the row says nothing: ANOTHER conversation's transcript (a person
/// `/resume`d it: the host hands over the file the process now writes, its
/// own older limit and replies under it — the review of 2026-10-14), a tail
/// the turn's output pushed the row out of.
pub(super) fn served_since_notice(jsonl: &str, notice: &str) -> bool {
    let mut after = false;
    let mut served = false;
    for line in jsonl.lines() {
        let Ok(v) = crate::harness::transcript::conversation(line) else {
            continue;
        };
        if !after {
            after = row_id(&v).as_deref() == Some(notice);
            continue;
        }
        let flag = |key: &str| v.get(key).and_then(Value::as_bool) == Some(true);
        if flag("isSidechain") || v.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let synthetic = v
            .get("message")
            .and_then(|m| m.get("model"))
            .and_then(Value::as_str)
            == Some("<synthetic>");
        if flag("isApiErrorMessage") {
            served = false;
        } else if !synthetic {
            served = true;
        }
    }
    served
}

/// A main-chain row as [`transcript_ends_on`] reads it; `None` for a row
/// that is neither an entry nor a turn's close.
fn row_of(v: &Value) -> Option<Row> {
    match v.get("type").and_then(Value::as_str) {
        Some("assistant") if v.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true) => {
            Some(Row::Entry(Entry::Notice {
                text: texts(v),
                error: v.get("error").and_then(Value::as_str).map(str::to_owned),
            }))
        }
        Some("assistant") => Some(Row::Entry(Entry::Reply)),
        Some("user") => user_entry(v).map(Row::Entry),
        Some("system") => match v.get("subtype").and_then(Value::as_str) {
            Some("turn_duration") => Some(Row::Closed),
            Some("local_command") => v
                .get("content")
                .and_then(Value::as_str)
                .and_then(command_notice)
                .map(Row::Entry),
            _ => None,
        },
        _ => None,
    }
}

/// Whether row `v` names NO parent (`parentUuid: null`, written so): the
/// first row of a conversation's chain. A row that does not carry the key
/// is no such row.
fn opens_the_conversation(v: &Value) -> bool {
    v.get("parentUuid").is_some_and(Value::is_null)
}

/// A `user` row as an entry: `None` for Claude Code's own — its meta rows, a
/// compaction's summary, and a LOCAL COMMAND's rows ([`local_command`]).
/// Any other — a prompt, a shell-mode command and its output
/// (`<bash-input>…`, `<bash-stdout>…`) — is a turn's input.
fn user_entry(v: &Value) -> Option<Entry> {
    let flag = |key: &str| v.get(key).and_then(Value::as_bool) == Some(true);
    if flag("isMeta") || flag("isCompactSummary") {
        return None;
    }
    match v.get("message").and_then(|m| m.get("content")) {
        Some(Value::String(text)) => (!local_command(text)).then_some(Entry::Input),
        Some(Value::Array(parts))
            if parts
                .iter()
                .any(|p| p.get("type").and_then(Value::as_str) == Some("tool_result")) =>
        {
            Some(Entry::ToolResult)
        }
        _ => Some(Entry::Input),
    }
}

/// Whether a `user` row's text is a LOCAL COMMAND's own, as 2.1.294 writes
/// one (module docs): its invocation (`<command-name>/login</command-name>`,
/// the bare `/compact` a compaction writes first, and a prompt command's
/// `<command-message>doctor</command-message>…`) or its output
/// (`<local-command-stdout>…`, `<local-command-stderr>…`) — no turn of the
/// conversation's, whatever it says.
fn local_command(text: &str) -> bool {
    let t = text.trim();
    t == "/compact"
        || t.starts_with("/compact ")
        || [
            "<command-name>",
            "<command-message>",
            "<local-command-stdout>",
            "<local-command-stderr>",
        ]
        .iter()
        .any(|open| t.starts_with(open))
}

/// A `system` `local_command` row's output
/// (`<local-command-stderr>…</local-command-stderr>`, or `stdout`) as the
/// context wall's notice, where its words read as a full context
/// (`Compaction failed · …`, `Error during compaction: …`); `None` for any
/// other output.
fn command_notice(content: &str) -> Option<Entry> {
    let t = content.trim();
    let inner = ["<local-command-stderr>", "<local-command-stdout>"]
        .iter()
        .find_map(|open| t.strip_prefix(open))?;
    let inner = inner
        .split("</local-command-")
        .next()
        .unwrap_or(inner)
        .trim();
    (aterm_phase::classify_wall(inner) == Some(WallKind::Context)).then(|| Entry::Notice {
        text: inner.to_owned(),
        error: None,
    })
}

/// Every text part of a row's `message.content`, joined (a string content
/// as it is).
fn texts(v: &Value) -> String {
    match v.get("message").and_then(|m| m.get("content")) {
        Some(Value::String(t)) => t.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

/// What a wall asks of the policy, by kind: a limit waited out or moved
/// from, a full context compacted, a lost login signed in again, a failed
/// request retried. Within a class the screen's own reading decides the act
/// (its reset, its cause): the transcript says only that Claude Code ended
/// the turn on a wall of that class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Limit,
    Context,
    Auth,
    Api,
    Memory,
}

fn class(kind: WallKind) -> Class {
    if kind.reads_limited() {
        return Class::Limit;
    }
    match kind {
        WallKind::Context => Class::Context,
        WallKind::Auth => Class::Auth,
        WallKind::Memory => Class::Memory,
        _ => Class::Api,
    }
}

/// The class of a notice whose words the wall table does not name, by the
/// `error` word Claude Code wrote with it.
fn error_class(error: &str) -> Option<Class> {
    match error {
        "authentication_failed" => Some(Class::Auth),
        "rate_limit" => Some(Class::Limit),
        "server_error" | "overloaded" | "unknown" => Some(Class::Api),
        _ => None,
    }
}

/// Whether the transcript's end CONFIRMS a screen wall of kind `screen`: its
/// newest entry is Claude Code's own notice ([`Entry::Notice`]) of the same
/// class ([`Class`]) — by its words, through the same table the screen is
/// read by ([`aterm_phase::classify_wall`]), else by its `error` word.
#[must_use]
pub(super) fn confirms(screen: WallKind, end: &TranscriptEnd) -> bool {
    let Entry::Notice { text, error } = &end.last else {
        return false;
    };
    let notice = aterm_phase::classify_wall(text)
        .map(class)
        .or_else(|| error.as_deref().and_then(error_class));
    notice == Some(class(screen))
}

/// How a reading's wall is checked ([`Session::confirm_wall`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Check {
    /// A read the point is DECIDED on: the transcript is asked, and its
    /// verdict kept for the screen.
    Decide,
    /// The point read again for the loop's own books after its decision (an
    /// act taken, a restart made or waited for): the wall keeps the verdict
    /// the decision gave it at that screen, and the transcript — moved on
    /// by the act itself — is not asked. A screen no decision was read at
    /// is checked as a decision's.
    AsDecided,
}

/// A wall refused at a decision's read ([`Session::confirm_wall`]).
#[derive(Debug, Clone)]
pub(super) struct Refusal {
    /// What makes it the same wall at a later read (its kind, its words,
    /// why): said once while it stands.
    key: String,
    /// The journal line; `None` for a limit a cancel left that only a
    /// transcript could tell from the notice already seen, and none did: the
    /// screen read no wall, and there is nothing to say.
    line: Option<String>,
}

/// THE LIMIT A CANCEL LEFT that a decision's read took for the answer to a
/// turn ([`TurnEndState::left_answer`]), by the screen it was read at
/// ([`Session::left_read`]).
#[derive(Debug, Clone)]
pub(super) struct LeftRead {
    /// The screen it was read at.
    pub(super) seq: u64,
    pub(super) answer: LeftAnswer,
    /// A transcript row written since the turn was asked confirmed it
    /// ([`Session::judge_wall`]): its print is dated. Where only the screen
    /// read it, nothing dates its notice ([`Session::wall_reset_at`]).
    pub(super) dated: bool,
}

/// The verdict of the last decision's read ([`Check::Decide`]).
#[derive(Debug, Clone)]
pub(super) struct Verdict {
    /// The screen it was read at.
    seq: u64,
    /// Its wall, refused; `None`: kept (confirmed, not asked, or none).
    refused: Option<Refusal>,
}

impl<C: Ctl> Session<'_, C> {
    /// THE WALL OF `reading` CONFIRMED, or taken out of it (module docs). At
    /// a decision's read ([`Check::Decide`]) a Claude Code's wall (not its
    /// memory banner) is kept where the host's transcript of the session
    /// confirms it ([`confirms`]; a transcript that has not caught up with
    /// the screen, [`TranscriptEnd::lagging`], is read again first), or
    /// where no transcript can be read and the wall was not read under a
    /// tool's call ([`aterm_phase::Wall::under_call`]) on the whole screen
    /// ([`Self::under_call_on_screen`]); otherwise it is refused. A read for
    /// the books ([`Check::AsDecided`]) keeps the verdict the decision's read
    /// gave the same screen. A refused wall
    /// leaves the reading with none — a limit's phase `idle`, whose evidence
    /// is then the composer's frame alone, as for any idle screen — and its
    /// refusal kept for its journal line ([`Self::say_wall_refused`]) and the
    /// point's phase ([`Self::point_as_decided`]).
    pub(super) fn confirm_wall(
        &mut self,
        reading: &mut aterm_phase::Reading,
        screen: &Screen,
        opts: &SuperviseOpts,
        check: Check,
    ) {
        let seq = screen.seq;
        let decided = self.wall_verdict.as_ref().filter(|v| v.seq == seq);
        let refused = match (check, decided) {
            (Check::AsDecided, Some(v)) => v.refused.is_some(),
            _ => {
                let refused = self.judge_wall(reading, screen, opts);
                let out = refused.is_some();
                // No refused wall at a decision: the refused one went away,
                // whichever step takes the point (the host's own among them,
                // which says nothing of walls), and the next is said again.
                if !out {
                    self.wall_refused_said = None;
                }
                self.wall_verdict = Some(Verdict { seq, refused });
                out
            }
        };
        if !refused {
            return;
        }
        reading.wall = None;
        if matches!(reading.phase, Phase::Limited { .. }) {
            reading.phase = Phase::Idle;
        }
        if reading.phase == Phase::Idle {
            reading.phase_authoritative &= aterm_phase::phase::has_composer_frame(&screen.rows);
        }
    }

    /// The decision's check of `reading`'s wall ([`Self::confirm_wall`]):
    /// `None` where it stands, else its refusal.
    fn judge_wall(
        &mut self,
        reading: &aterm_phase::Reading,
        screen: &Screen,
        opts: &SuperviseOpts,
    ) -> Option<Refusal> {
        let w = reading.wall.as_ref().filter(|w| {
            reading.program == aterm_phase::Program::Claude && w.kind != WallKind::Memory
        })?;
        let host = opts.idle_host.as_ref();
        // A LIMIT A CANCEL LEFT read as a turn's answer at this screen
        // ([`Session::left_read`]): when that turn was asked, on the loop's
        // clock, and whether the screen alone tells its notice from the one
        // the policy already saw.
        let left = self
            .left_read
            .as_ref()
            .filter(|l| l.seq == screen.seq)
            .map(|l| (self.unix_of(l.answer.since), l.answer.screen_tells));
        let untold = left.is_some_and(|(_, tells)| !tells);
        // The limit's notice row this wall is confirmed by, if it is: what a
        // reply must follow for the transcript to say a turn was served
        // since it, once the notice leaves the screen. A wall the screen did
        // not read (`untold`, refused) leaves the one kept.
        let kept = self.limit_notice_row.take();
        let unsaid = |s: &mut Self| {
            s.limit_notice_row = kept.clone();
            Some(Refusal {
                key: format!("{} left, untold", w.kind.name()),
                line: None,
            })
        };
        let why = match host.and_then(|h| h.transcript_tail()) {
            // No transcript to tell the notice the screen draws from the one
            // the policy already saw: no answer.
            None if untold => return unsaid(self),
            // No transcript: the screen alone decides, but never on a wall
            // read under a tool's call — the whole screen's rows walked
            // (module docs).
            None if self.under_call_on_screen(w, screen) => {
                "read under a tool's call, and no transcript to confirm it".to_string()
            }
            None => return None,
            Some(tail) => {
                let mut end = transcript_ends_on(&tail);
                for _ in 0..LAG_TRIES {
                    if !end.lagging() {
                        break;
                    }
                    std::thread::sleep(LAG_STEP);
                    match host.and_then(|h| h.transcript_tail()) {
                        Some(tail) => end = transcript_ends_on(&tail),
                        None => break,
                    }
                }
                // A LIMIT A CANCEL LEFT, read as the answer to a turn
                // ([`Session::left_read`]), is that turn's only where its
                // notice row was written since the turn was asked: a
                // conversation `/resume`d meanwhile ends on its own older
                // cancelled limit, whose row confirms its class all the same.
                // Where the screen cannot tell it from the notice already
                // seen, a row that names no time says nothing either.
                let older = left.is_some_and(|(since, tells)| {
                    end.at
                        .map_or(!tells, |at| at < since.saturating_sub(LEFT_ANSWER_SLACK_S))
                });
                if confirms(w.kind, &end) && !older {
                    self.limit_notice_row = end.row.filter(|_| class(w.kind) == Class::Limit);
                    if let Some(l) = self.left_read.as_mut().filter(|l| l.seq == screen.seq) {
                        l.dated = end.at.is_some();
                    }
                    return None;
                }
                if untold {
                    return unsaid(self);
                }
                if older {
                    "the transcript's notice was written before the turn it would answer"
                        .to_string()
                } else {
                    format!("the transcript ends on {}", end.last.word())
                }
            }
        };
        let words = one_line(&w.message);
        Some(Refusal {
            key: format!("{} {words} {why}", w.kind.name()),
            line: Some(format!(
                "SKIPPED seq={} rule={RULE_WALL_CONFIRM} unconfirmed wall: {} — {why}: {}",
                screen.seq,
                w.kind.name(),
                clip(&words)
            )),
        })
    }

    /// A TURN SERVED SINCE A USAGE LIMIT WHOSE NOTICE LEFT, read in the
    /// session's transcript ([`TurnEndReading::served_in_transcript`]): the
    /// host's tail of it holds the limit's own notice row — the one that
    /// confirmed the limit at the last decision's read of it
    /// (`Session::limit_notice_row`) — and a reply of the model's after it
    /// ([`served_since_notice`]). Asked by a decision's read alone, at a point
    /// whose screen draws no limit's notice to order a turn by
    /// ([`aterm_phase::since_limit`] `None`: the notice scrolled off the
    /// rows read under a reply). A person's local commands at the limit
    /// (`/usage`, `/model`, `/login` dismissed) write no reply; the harness's
    /// own `/usage` probe is one of them. `false` with no transcript to read,
    /// and with no notice row remembered (a limit the screen alone decided, a
    /// loop that took the wait up from its predecessor's ledger row): the
    /// reset is waited out, as for a wall.
    pub(super) fn served_in_transcript(&self, opts: &SuperviseOpts) -> bool {
        let Some(notice) = self.limit_notice_row.as_deref() else {
            return false;
        };
        opts.idle_host
            .as_ref()
            .and_then(|h| h.transcript_tail())
            .is_some_and(|tail| served_since_notice(&tail, notice))
    }

    /// Whether wall `w`, read from `screen`, stands UNDER A TOOL'S CALL on
    /// the WHOLE SCREEN ([`aterm_phase::Wall::under_call`]). The loop's reads
    /// are the screen's tail (`tail=`, [`Screen::first`] past 0), which can
    /// hold the wall and not the call above it: the owner walk then reaches
    /// the top of the rows read and finds no owner (the review of pass 4: an
    /// MCP call's row at row 9 of 50, the weekly limit's words under it read
    /// from rows 10-49 — acted on). So a gutter wall read on a tail with no
    /// call over it is read again from the whole grid ([`Self::grid`], read
    /// once more here and nowhere else: the tail stays every other read's,
    /// the server's own agent-verdict cut), and stands only where that read
    /// finds the same wall and no call over it. A whole read that fails, or
    /// a screen that no longer reads the same wall, counts as under a call:
    /// nothing is acted on that the screen's rows above were not read for.
    fn under_call_on_screen(&mut self, w: &aterm_phase::Wall, screen: &Screen) -> bool {
        if w.under_call {
            return true;
        }
        if screen.first == 0 || w.placement != aterm_phase::Placement::Gutter {
            return false;
        }
        let Ok(whole) = self.grid() else {
            return true;
        };
        let read = self
            .reader(&whole.rows)
            .read(&whole.rows, Some(whole.cursor_col));
        read.wall
            .is_none_or(|x| x.under_call || x.kind != w.kind || x.message != w.message)
    }

    /// The point `turn` read again as its decision read it
    /// ([`Self::confirm_wall`]): a limit whose wall was refused at that
    /// screen is no limit — no episode opens on it, nothing is escalated for
    /// it and the host is not told the session is limited; it is an idle
    /// point. And the LIMIT A CANCEL LEFT that the decision took for the
    /// answer to a turn ([`TurnEndState::left_answer`], kept as
    /// [`Session::left_read`]) and did not refuse is a limit point, as the
    /// same wall read before the cancel is: its episode opens, the host is
    /// told the session is limited, the host's own steps wait on it.
    pub(super) fn point_as_decided(&self, turn: &mut Turn) {
        let refused = self
            .wall_verdict
            .as_ref()
            .is_some_and(|v| v.seq == turn.screen.seq && v.refused.is_some());
        if refused && matches!(turn.phase, Phase::Limited { .. }) {
            turn.phase = Phase::Idle;
        } else if !refused
            && turn.phase == Phase::Idle
            && let Some(l) = self.left_read.as_ref().filter(|l| l.seq == turn.screen.seq)
        {
            turn.phase = Phase::Limited {
                message: l.answer.wall.message.clone(),
                reset: l.answer.wall.reset.clone(),
            };
        }
    }

    /// Journal the wall the last decision refused, ONCE while it stands (the
    /// same wall read again, at every look and every wait that runs out, is
    /// one line). Its episode ends where the wall goes away, whatever path
    /// sees that: a decision with no refused wall ([`Self::confirm_wall`]),
    /// the agent's live turn ([`Self::wall_refusal_over`]).
    pub(super) fn say_wall_refused(&mut self, review: &mut dyn Review) {
        if let Some(r) = self.wall_verdict.as_ref().and_then(|v| v.refused.as_ref())
            && let Some(line) = r.line.as_deref()
            && self.wall_refused_said.as_deref() != Some(r.key.as_str())
        {
            review.note(line);
            self.wall_refused_said = Some(r.key.clone());
        }
    }

    /// The agent works again (its live turn): the refused wall's episode is
    /// over, and the same wall met at a later point is said again.
    pub(super) fn wall_refusal_over(&mut self) {
        self.wall_refused_said = None;
    }
}

#[cfg(test)]
#[path = "wall_confirm_tests.rs"]
mod tests;
