// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A wall is confirmed before it is acted on.
//!
//! The screen's reader ([`aterm_phase::wall`]) reads a wall for recall and cannot tell some of a
//! tool's output from Claude Code's own row (an MCP result of several content blocks, output
//! under a PreToolUse hook's row, a long output whose call row has scrolled off, the worker's own
//! words). Acted on, such a misreading would type `/login` or `/compact`, park the session until a
//! reset, move it to another account or raise the login-gone alarm on the say of whoever wrote a
//! file, a page or a server's reply. So the turn-end policy acts on a Claude Code wall only where
//! the session's own transcript confirms it: the tail the host reads
//! ([`IdleHost::transcript_tail`], `upgrade_drive::process_transcript_tail`) has as its newest
//! main-chain entry ([`transcript_ends_on`]) Claude Code's own notice of the screen wall's class
//! ([`confirms`]). An unconfirmed wall is journaled once (`SKIPPED seq=<n> rule=wall-confirm@v1
//! unconfirmed wall: <kind> — <why>: <the screen's words>`) and the point is an ordinary turn
//! end. The critical-memory banner (read by position) and agents other than Claude Code (no such
//! transcript) are not asked.
//!
//! With no transcript (no host, as under `aterm drive watch|supervise`; no record of the process
//! yet; an unreadable file), a wall read under a tool's call ([`aterm_phase::Wall::under_call`],
//! MCP and shell-mode `!` calls included) is refused and any other is acted on as read: recall is
//! given up there (a real context wall after a tool's output gets no `/compact`) to refuse spoofs
//! nothing on the screen tells from a wall. The owner walk (`phase::block_is_vendors`) goes on
//! over a blank row inside a call's rows, so a real wall under an output that holds a blank row is
//! refused too; a gutter wall read on a tail with no call over it is read again from the whole
//! grid ([`Session::under_call_on_screen`]).
//!
//! NO TRANSCRIPT (no host — a loop run without the window's —, an agent
//! whose record cannot be read — a Codex —, no record of the process yet, the conversation not written yet, two
//! transcripts of its id and none in its project's own place, a file that
//! cannot be read): a wall the screen read UNDER A TOOL'S CALL
//! ([`aterm_phase::Wall::under_call`]: a later column-2 row under a call the
//! owner walk recognises — `⏺ Name(…)`, an MCP tool's row, a shell-mode `!
//! cmd`, an integration's `Name[tool](…)` — a hook's row above the output or
//! not) is not acted on, said as `… unconfirmed wall: <kind> — read under a
//! tool's call, and no transcript to confirm it: …`; any other wall — under
//! the person's turn, a notice, the worker's message, a collapsed group, or
//! no owner the walk reaches — is acted on as the screen reads it. A
//! program's words behind one of Claude Code's own labels — a Monitor's event
//! row, a Stop hook's `Stop hook error: [<command>]: …` row, a hook's
//! `systemMessage` row (`<hook> says: …`) — are no wall on the screen at
//! all ([`aterm_phase::classify_wall`]): read as one
//! (2.1.302, measured), the since-removed `aterm drive watch` typed `/login` on a Monitor's
//! event and on a Stop hook's stderr. Nor are a hook's words with no label
//! under a row of Claude Code's own: a Stop hook's `stopReason` under the
//! stop-hook summary, and the row under another hook's error row (`<hook>
//! hook returned blocking error`, `<hook> hook error`, no colon: its
//! blocking reason, or its stderr behind `Failed with non-blocking status
//! code: `) — the owner walk reads them as output, under a call or not
//! (`aterm-phase`'s `block_is_vendors`, rule 4). Read as walls, `aterm drive
//! watch` typed `/login` on a `stopReason` and parked the session until the
//! reset a double-spaced one named; a PostToolUse hook's blocking reason was
//! read and refused here as under a call (measured). Where the hook drew no
//! words of its own under its error row — a blocking reason of blank space
//! (2.1.304, measured) — Claude Code's wall stands in their place, drawn as
//! a hook's reason that is the wall's words is: with the transcript it is
//! found there ([`Session::transcript_found`], the row showing the class of
//! the notice the transcript ends on), with none it is not acted on, a
//! knowing loss of recall. The walk
//! is given EVERY ROW THE SCREEN SHOWS for it: the loop reads the screen's
//! tail (`tail=40`, the server's own agent-verdict cut, which every other
//! read keeps), and a tail can hold the wall without the call above it, so
//! a gutter wall read on a tail with no call over it is read again from the
//! whole grid before it is acted on ([`Session::under_call_on_screen`]).
//!
//! (H6-HARNESS-1, pass 5): with no transcript, a wall
//! under a recognised tool call — an MCP call's and a `!` command's
//! included — is not acted on. Main DID act on walls under MCP and `!`
//! calls: its walk did not know them as calls, and read every `⎿` row under
//! them as Claude Code's, the output's first row included. So this is a
//! KNOWING LOSS OF RECALL where no transcript can be read (a loop with no
//! host, an agent whose record cannot be read, a session whose transcript
//! cannot be found) — a
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
//! transcript gets no `/compact`, where main and pass 5 typed one. A tool's
//! output whose call row has scrolled off the screen, read with no owner and
//! acted on until , is judged with the main screen's history
//! right above the screen, read up to the owner
//! ([`Session::under_call_on_screen`]).
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
//! off, the person's next turn is. The footer's placement is NOT exempted. A
//! custom statusline's own rows are not read as a notice at all
//! (`phase::footer_notice` reads only where Claude Code draws its own,
//! ), but one that prints a status glyph first, or
//! right-aligns its words, reads as the vendor's. (2) With no transcript, a
//! wall whose owner is above the screen is refused where the history cannot
//! be read up to it: the alternate screen (the fullscreen renderer keeps no
//! history), an evicted or unreadable row, an owner more than
//! [`OWNER_ROWS`] rows up. (3) The tab still publishes `agent=wall:<kind>`
//! for a wall refused here: the window's agent verdict (aterm-gui,
//! `presence::agent_verdict`) reads the screen, not the transcript.
//!
//! Local commands are no turns (`fixtures/claude-2.1.294-transcript-local-commands.jsonl`):
//! `/model`, `/status`, `/usage`, `/mcp` and an unchanged `/config` write `system`
//! `local_command` rows; a dismissed `/login`, a changing `/config` and `/compact` write `user`
//! rows (a meta caveat, `<command-name>…`, `<local-command-stdout>…`; `/compact` its bare name
//! first); a prompt command (`/doctor`) a `<command-message>…` row and a meta prompt. All are
//! passed over, so a person who opens `/usage` at a standing limit leaves its notice the newest
//! entry; a tool's output is a `tool_result` part and cannot write a `user` row of these shapes,
//! and a shell-mode command's `<bash-input>…`/`<bash-stdout>…` rows are input. One is a notice:
//! a failed `/compact` writes a `system` `local_command` row `<local-command-stderr>Compaction
//! failed · …`, taken for the context wall's notice (a tool's output or a prompt is never a
//! `system` row).
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
//! WHICH LIMIT (KI-1015-1 W3, measured on 2.1.300): the transcript also
//! says which limit a usage limit is, where the screen cannot. Claude Code
//! 2.1.300 draws the generic `⚠ Usage limit reached · resets …` in its
//! footer over a model's bucket (`⎿  You've hit your Sonnet limit · …`).
//! The screen's reader takes the limit the turn's own notice names where
//! that notice is on the screen (`aterm_phase`'s `named_by_the_turn`); with
//! it scrolled off, the footer's generic banner is all it reads (the
//! 2.1.300 sweep before that read: the bucket was a session limit, the
//! fallback model never ran, and the session waited days for the bucket's
//! reset). So where the transcript confirms a limit, the limit its row
//! names — by its words through the wall table, else its
//! `quotaLimits.rateLimitType` ([`limit_named`]) — is the wall's kind, and
//! its words the wall's ([`Named`]; the screen's countdown kept after
//! them), where that is another kind than the screen read. The screen's
//! reset TEXT is kept: the loop's episode is matched by it when the host's
//! steps read the screen again. The verdict carries it, so every read of
//! the same screen — the decision's and its books' — takes the same wall.
//!
//! A LIMIT ONLY THE TRANSCRIPT SHOWS (KI-1015-1 LIM-1): the transcript also
//! FINDS a limit where the screen draws none. A relaunched or restored
//! Claude Code redraws its conversation and then its own `⏺ Auto mode lets
//! Claude …` paragraph (the reader passes that over: LC-10), and a cold
//! restore reopens the tab under a new session with nothing remembered; the
//! relaunch's carry-on, then `keep going`, met a weekly limit that still
//! stood. So at the first decision a loop reads of an agent's process and
//! the conversation it holds where the screen reads no wall, the limit row
//! its transcript stands at — from the host, which names none the harness
//! moved the conversation off (a bucket's fallback, the way back, another
//! account: owner decision 3, kept through the relaunches written over the
//! move) — is the wall ([`Session::read_standing_limit`]), held until a turn
//! is seen running; and a host step about to be taken with no wall or
//! episode asks the host the same, last (`Session::host_steps_here`). The
//! conversation is the host's last read of it: a `/resume` in the same
//! process is seen once something the host does reads the agent again, and
//! until then only where the resumed screen draws the limit itself.
//!
//! WHAT CLAUDE CODE 2.1.294 WRITES (measured in the real-Claude lab,
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
//! which stay the person's turn. A prompt command's invocation passed over
//! after the newest entry is marked on the end (`TranscriptEnd::prompted`):
//! its prompt asks the model, so where that entry is an older notice the
//! screen's `⏺` row does not show, the row is the answer not written yet, and
//! is read again once, as a turn's input is.
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
//! A TURN THAT ENDED ON CLAUDE CODE'S OWN ERROR ROW IS NEVER AN ORDINARY
//! IDLE POINT (2026-10-15, the owner: the harness "doesn't respect …
//! errors"). Claude Code 2.1.300 draws every `isApiErrorMessage` row its
//! renderer has no case of its own for as its `⏺` bullet and the text at
//! column 2 — the prose's shape, in a warning colour a row of text does not
//! keep — so where the words are no `API Error …` the table names (`There's
//! an issue with the selected model (x). …`, `Your account is on hold …`,
//! `Image was too large. …`) the screen reads no wall, and the ordinary
//! policy answered it `keep going` at 0, 2, 4 … minutes and then hourly for
//! good. So at a decision's read of an idle point where the screen read no
//! wall, the transcript is asked the other way round
//! ([`Session::transcript_found`]): where its newest entry is Claude Code's
//! own notice, CLOSED by a `turn_duration`, and the screen's last thing said
//! is a row Claude Code draws a notice as (a `⏺` message that is no call, a
//! vendor `⎿` block) showing that notice's first words, the point carries
//! the wall the notice is ([`transcript_wall`]): by its words through the one
//! table, else by the vendor's typed fields — an `apiError` of 2.1.300's
//! terminal set, an `error` word of a refused request, a status the vendor
//! does not retry — and anything else (`unknown`, `server_error`, the bare
//! `API Error`) the server's, retried, never a refusal. Only a row Claude
//! Code writes itself names one (no tool's output and no prompt is an
//! `isApiErrorMessage` row), the screen must show it (a row the renderer
//! hides, a `/resume`d conversation's older error under newer words, name
//! none), and the act names the kind in aterm's own words (H6-HARNESS-1).
//! The typed kind is also told to the policy
//! ([`TurnEndState::notice_typed_was`]) for the remedy a refused request's
//! exception names. The lag re-reads below run here only where the row has
//! the vendor's error row's own shape (`API Error …`, a short notice and its
//! remedy): a turn stopped with Esc writes no `turn_duration`, and an idle
//! point that ends on prose is no reason to sleep. A `⏺` row shaped like
//! prose gets one re-read, and only where the transcript still ends, open,
//! on the turn's own input (its prompt, a tool's result): it has not written
//! what the screen says after it. A notice the row shows whose turn is not
//! closed yet is read again until it is: the `turn_duration` follows it by 1
//! to 14 ms, and a read that falls between them took the point for an
//! ordinary turn end: the continuation typed into it, and after a retry of
//! the wall's its track dropped and its ladder started over. The cost: one
//! transcript read at a decision on such a row. The reads for the books
//! keep the wall the decision found, by its screen, as they keep a refusal.
//!
//! The same holds where the screen DID read a wall but only the table's own
//! fallback for it: an `API Error: <the server's words>` with no status the
//! table names nothing of is the server's, retried (1, 5, 15, 30 minutes,
//! then hourly), while the notice that confirms it may be typed as a refused
//! request (`API Error: Request blocked by your organization's policy.`,
//! `dlp_request_denied`; a minimum version an organization requires,
//! `claude_code_version_too_old`; `API Error (<model>): … Try /model to
//! switch to …`, `model_not_found`). There the confirmed wall takes the kind
//! the notice's typed fields name, within its class ([`refined`]), where the
//! notice is the row the screen read (its first words), and the reads for
//! the books keep it.
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
//! screen reads one) is refused a second late. A re-read asks the file
//! again, never the agent (`IdleHost::transcript_tail_again`: the host read
//! the process in the tab once, for the decision's first read).

use aterm_json::Value;
use aterm_phase::{ApiCause, WallKind};

use super::super::policy::turn_end::{Conversation, LeftAnswer, WaitedLimit, ended_under};
use super::super::report::{Offscreen, parse_offscreen};
use super::*;

/// The conversation named by this exact tail; an unread or unnamed host
/// transcript supplies no ownership, even when the screen alone confirms a wall.
pub(super) fn conversation_of(tail: &str) -> Conversation {
    tail.lines()
        .rev()
        .find_map(|line| {
            let v = crate::harness::transcript::conversation(line).ok()?;
            if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
                return None;
            }
            v.get("sessionId").map(|id| {
                id.as_str()
                    .filter(|id| !id.is_empty())
                    .map_or(Conversation::Unknown, |id| {
                        Conversation::Named(id.to_owned())
                    })
            })
        })
        .unwrap_or(Conversation::Unknown)
}

/// All ownership evidence from one host read, not independently refreshed tails.
pub(super) struct LimitTranscript {
    pub(super) conversation: Conversation,
    pub(super) served: bool,
    pub(super) chosen: Option<String>,
    pub(super) elsewhere: bool,
}

/// The rule an unconfirmed wall's journal line is written under.
pub(super) const RULE_WALL_CONFIRM: &str = "wall-confirm@v1";

/// How many times a transcript that has not caught up with the screen is
/// read again before its wall is refused ([`TranscriptEnd::lagging`]).
pub(crate) const LAG_TRIES: u32 = 4;

/// How long apart those reads are: the four cover ten times the lag
/// measured on 2.1.294.
pub(crate) const LAG_STEP: Duration = Duration::from_millis(250);

/// The newest main-chain entry of a Claude Code transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Entry {
    /// Nothing of the conversation's in the tail read.
    Nothing,
    /// Claude Code's own notice row: an `isApiErrorMessage` row (its text,
    /// its `error` word, and since 2.1.295 its typed `apiError` and the
    /// `apiErrorStatus` it answered; a usage limit's, which of the account's
    /// limits it is: `quotaLimits.rateLimitType`), or a local command's output
    /// that reads as a full context (a failed `/compact`).
    Notice {
        text: String,
        error: Option<String>,
        api_error: Option<String>,
        status: Option<u16>,
        limit: Option<String>,
    },
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
pub(crate) struct TranscriptEnd {
    /// The newest main-chain entry.
    pub(crate) last: Entry,
    /// Claude Code closed the turn after it: a `system` row `turn_duration`.
    pub(crate) closed: bool,
    /// The id (`uuid`) of the row the newest entry was read from: a usage limit's notice
    /// confirmed is remembered by it ([`served_since_notice`]).
    pub(crate) row: Option<String>,
    /// That row's own `timestamp` (unix seconds): what tells a limit a cancel left in answer to
    /// the loop's act from an older one ([`Session::judge_wall`]).
    pub(crate) at: Option<i64>,
    /// A PROMPT COMMAND's row (`<command-message>…`, [`prompt_command`]) was
    /// passed over after the newest entry: its prompt is the model's to
    /// answer, and the notice or reply that answers it is not written yet
    /// ([`Session::transcript_found`]).
    pub(crate) prompted: bool,
}

impl TranscriptEnd {
    /// The typed kind Claude Code wrote with the notice it ends on: its
    /// `apiError`, else its `error` word; `None` for any other end.
    #[must_use]
    pub(super) fn typed(&self) -> Option<String> {
        match &self.last {
            Entry::Notice {
                error, api_error, ..
            } => api_error.clone().or_else(|| error.clone()),
            _ => None,
        }
    }

    /// When the notice it ends on was written ([`Self::at`]); `None` for any
    /// other end.
    #[must_use]
    pub(super) fn notice_at(&self) -> Option<i64> {
        self.at
            .filter(|_| matches!(self.last, Entry::Notice { .. }))
    }

    /// The transcript has not caught up with the turn the screen ended: no `turn_duration` closed
    /// it and its newest entry is no notice.
    #[must_use]
    pub(crate) fn lagging(&self) -> bool {
        !self.closed && !matches!(self.last, Entry::Notice { .. })
    }
}

/// One row of the main chain, as [`transcript_ends_on`] reads it.
pub(crate) enum Row {
    /// An entry of the conversation's.
    Entry(Entry),
    /// Claude Code closed the turn: a `system` row `turn_duration`.
    Closed,
}

/// What the main chain of `jsonl` (a transcript's tail; a torn first line is passed over) ends
/// on: its newest entry (an `assistant` row, a notice when `isApiErrorMessage`; a `user` row, a
/// tool's result or a turn's input) and whether a `turn_duration` row closed the turn after it.
/// Not entries: a subagent's rows (`isSidechain`), meta rows (`isMeta`), a compaction's summary
/// (`isCompactSummary`), a local command's own rows ([`local_command`]) except a `system` output
/// that reads as a full context, and every other row. Newest by place in the file, except that a
/// prompt row with no parent opens the conversation and ends nothing (module docs).
pub(crate) fn transcript_ends_on(jsonl: &str) -> TranscriptEnd {
    let mut end = TranscriptEnd {
        last: Entry::Nothing,
        closed: false,
        row: None,
        at: None,
        prompted: false,
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
                    prompted: false,
                };
            }
            None => end.prompted |= prompt_command(&v),
        }
    }
    end
}

/// Whether row `v` is a PROMPT COMMAND's invocation (`/doctor`, a project's
/// `/review`: a `user` row opening `<command-message>…`, module docs): passed
/// over as no turn of the conversation's ([`local_command`]), though its
/// prompt (the meta row after it) asks the model, and the reply or notice that
/// answers it follows.
fn prompt_command(v: &Value) -> bool {
    v.get("type").and_then(Value::as_str) == Some("user")
        && v.get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_str)
            .is_some_and(|t| t.trim().starts_with("<command-message>"))
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

/// How much earlier than the loop's own record of its act the notice that answered it may say it
/// was written: the stub's 429 is written within milliseconds of the submit (19 ms measured on
/// 2.1.299), while a `/resume`d conversation's older notice is minutes or days older.
pub(super) const LEFT_ANSWER_SLACK_S: i64 = 10;

/// A turn served since the notice row `notice` (its `uuid`) in `jsonl`: that row is in the tail
/// and the newest main-chain `assistant` row after it is a reply of the model's (no
/// `isApiErrorMessage`, not `<synthetic>`), so the limit lifted; a notice after that reply takes
/// it back. Nothing else says so: a prompt the limit may still answer, a `<task-notification>`
/// Claude Code writes when a background task ends, a local command's rows. A tail without the
/// row (another conversation's transcript after a `/resume`, or the row pushed out) says nothing.
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

/// THE NEWEST MODEL CHOSEN SINCE THE NOTICE ROW `notice` (its `uuid`), in
/// `jsonl` (a transcript's tail): the output of a `/model` command written
/// after that row — `<local-command-stdout>Set model to `Opus 5.5` and saved
/// as your default for new sessions</local-command-stdout>` under its
/// `<command-name>/model</command-name>` row, as `user` rows on 2.1.302 (the
/// lab's `r21lebk5`) and `system` `local_command` rows on 2.1.294 — its words
/// after `Set model to ` (the anchor `model.set`), the backticks the
/// transcript sets the model's name in dropped: `Opus 5.5 and saved as your
/// default for new sessions`, as the screen draws it
/// ([`aterm_phase::SinceLimit::chosen_model`]). A `/model` left with Esc
/// (`Kept model as …`) chose nothing. `None` where none was chosen since, or
/// the tail does not hold the row: another conversation's transcript, a tail
/// cut past it.
pub(super) fn chosen_since_notice(jsonl: &str, notice: &str) -> Option<String> {
    let set = aterm_phase::anchor("model.set");
    let mut after = false;
    let mut model = false;
    let mut chosen = None;
    for line in jsonl.lines() {
        let Ok(v) = crate::harness::transcript::conversation(line) else {
            continue;
        };
        if !after {
            after = row_id(&v).as_deref() == Some(notice);
            continue;
        }
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let text = match v.get("type").and_then(Value::as_str) {
            Some("user") => texts(&v),
            Some("system") if v.get("subtype").and_then(Value::as_str) == Some("local_command") => {
                v.get("content")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            }
            _ => continue,
        };
        let text = text.trim();
        if text.starts_with("<command-name>") {
            model = text.starts_with("<command-name>/model</command-name>");
        } else if let Some(out) = text.strip_prefix("<local-command-stdout>")
            && std::mem::take(&mut model)
            && let Some(words) = out
                .split("</local-command-stdout>")
                .next()
                .and_then(|o| o.trim().strip_prefix(set))
        {
            chosen = Some(words.replace('`', "").trim().to_owned());
        }
    }
    chosen
}

/// How far the account's own reset that a notice row carries
/// (`quotaLimits.resetsAt`) may lie from the reset the loop waits on for the
/// row to be that limit's ([`waited_notice_row`]). The loop places a reset
/// from the clock its notice names, to the minute; an older notice naming
/// the same clock — another conversation's, `/resume`d — names it half a day
/// before at the least.
pub(super) const RESET_TIE_SLACK_S: u64 = 3600;

/// THE NOTICE ROW OF THE LIMIT WAITED ON, in `jsonl` (a transcript's tail),
/// where the loop keeps no notice row id to find it by (a loop that took the
/// wait over from its predecessor's ledger row: a seamless update, a new
/// window, a loop restarted): the id of the newest main-chain notice row that
/// NAMES THE RESET WAITED ON — a limit of its class whose words name the same
/// clock ([`WaitedLimit::named_by`], by the words through the wall table and
/// its `quotaLimits.rateLimitType`, [`limit_named`]), and where the row
/// carries the account's `quotaLimits.resetsAt`, that reset within
/// [`RESET_TIE_SLACK_S`] of `reset` (the reset waited on, unix seconds): a
/// clock names no date, and another conversation's older notice can name
/// the same one (the residual review's R2). What a turn served since the
/// limit ([`served_since_notice`]) and a model chosen since it
/// ([`chosen_since_notice`]) are read after, as after the row a loop kept
/// (`Session::limit_transcript`). `None` where no row names it.
pub(super) fn waited_notice_row(jsonl: &str, waited: &WaitedLimit, reset: i64) -> Option<String> {
    let mut newest = None;
    for line in jsonl.lines() {
        let Ok(v) = crate::harness::transcript::conversation(line) else {
            continue;
        };
        let flag = |key: &str| v.get(key).and_then(Value::as_bool) == Some(true);
        if flag("isSidechain")
            || !flag("isApiErrorMessage")
            || v.get("type").and_then(Value::as_str) != Some("assistant")
        {
            continue;
        }
        let text = texts(&v);
        let quota = v.get("quotaLimits");
        let window = quota
            .and_then(|q| q.get("rateLimitType"))
            .and_then(Value::as_str);
        let resets_at = quota
            .and_then(|q| q.get("resetsAt"))
            .and_then(Value::as_i64);
        let ties = limit_named(&text, window)
            .is_some_and(|kind| waited.named_by(kind, aterm_phase::notice_reset(&text).as_deref()))
            && resets_at.is_none_or(|at| at.abs_diff(reset) <= RESET_TIE_SLACK_S);
        if ties {
            newest = row_id(&v);
        }
    }
    newest
}

/// THE TURN ENDED WAITING ON BACKGROUND WORK, by `jsonl` (a transcript's
/// tail) of the Claude Code launched at `launched` (unix seconds) — the
/// transcript's half of the wait Claude Code carries (BG-1;
/// `aterm_phase::phase`'s module docs), read where the screen draws no
/// glyph row at all (the waiting row scrolled off, or `showTurnDuration:
/// false`, where Claude Code draws none). Every one holds:
/// * the newest main-chain `turn_duration` row counts work still pending
///   (`pendingBackgroundAgentCount` or `pendingWorkflowCount` past 0);
/// * it CLOSES the newest main-chain entry ([`transcript_ends_on`]'s), and
///   that entry is the model's reply: a later entry means the file lags the
///   screen, which then decides, and a turn that ended on a notice is the
///   wall's;
/// * it was written by THIS process, after `launched`: a `--resume` or
///   aterm's relaunch kills the background work, and the dead process's
///   count stays in the file;
/// * no person's row follows it: a local command they ran there (a `user`
///   row Claude Code writes for it, not an entry) is their hand at the tab,
///   and the screen decides.
///
/// A row that names no time is no evidence.
pub(super) fn waits_on_background(jsonl: &str, launched: i64) -> bool {
    turn_closed(jsonl, launched) == Some(true)
}

/// THE TURN CLOSED WITH NOTHING PENDING, by `jsonl` of the Claude Code
/// launched at `launched` — the transcript's word against a wait the screen
/// still carries (BG-1, the verifier's correction 3; `aterm_phase::phase::
/// carried_wait_alone`): the same newest main-chain `turn_duration` as
/// [`waits_on_background`] reads, every condition of it holding (it closes
/// the model's reply, this process wrote it, no person's row follows), but
/// its counts say NOTHING pending: each absent (Claude Code writes no zero
/// count: 583 of the trust session's 1,313 rows carry neither key, none a
/// 0) or 0. No such row, a stale one or anything less: `false`, and the
/// screen's wait stands.
pub(super) fn ended_with_nothing_pending(jsonl: &str, launched: i64) -> bool {
    turn_closed(jsonl, launched) == Some(false)
}

/// How the newest turn in `jsonl` closed, where the transcript says it for
/// certain ([`waits_on_background`]'s conditions): `Some(true)` waiting on
/// background work, `Some(false)` with nothing pending; `None` where the
/// newest `turn_duration` closes no reply of this process's, names no time,
/// or a person's row follows it.
fn turn_closed(jsonl: &str, launched: i64) -> Option<bool> {
    let mut last = Entry::Nothing;
    // The newest `turn_duration` after the newest entry: whether it counts
    // work pending, and when it was written.
    let mut close: Option<(bool, Option<i64>)> = None;
    let mut person = false;
    for line in jsonl.lines() {
        let Ok(v) = crate::harness::transcript::conversation(line) else {
            continue;
        };
        let flag = |key: &str| v.get(key).and_then(Value::as_bool) == Some(true);
        if flag("isSidechain") {
            continue;
        }
        match row_of(&v) {
            Some(Row::Closed) => {
                let pending = ["pendingBackgroundAgentCount", "pendingWorkflowCount"]
                    .iter()
                    .any(|key| v.get(key).and_then(Value::as_u64).is_some_and(|n| n > 0));
                close = Some((pending, row_time(&v)));
                person = false;
            }
            Some(Row::Entry(entry)) => {
                if entry == Entry::Input && opens_the_conversation(&v) && last != Entry::Nothing {
                    continue;
                }
                last = entry;
                close = None;
                person = false;
            }
            None => {
                person |= close.is_some()
                    && v.get("type").and_then(Value::as_str) == Some("user")
                    && !flag("isMeta")
                    && !flag("isCompactSummary")
                    && !flag("queueTranscriptOnly");
            }
        }
    }
    let (pending, at) = close?;
    (last == Entry::Reply && !person && at.is_some_and(|at| at > launched)).then_some(pending)
}

/// A main-chain row as [`transcript_ends_on`] reads it; `None` for a row
/// that is neither an entry nor a turn's close.
pub(crate) fn row_of(v: &Value) -> Option<Row> {
    match v.get("type").and_then(Value::as_str) {
        Some("assistant") if v.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true) => {
            Some(Row::Entry(Entry::Notice {
                text: texts(v),
                error: v.get("error").and_then(Value::as_str).map(str::to_owned),
                api_error: v.get("apiError").and_then(Value::as_str).map(str::to_owned),
                status: v
                    .get("apiErrorStatus")
                    .and_then(Value::as_u64)
                    .and_then(|s| u16::try_from(s).ok()),
                limit: v
                    .get("quotaLimits")
                    .and_then(|q| q.get("rateLimitType"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
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

/// Whether row `v` names no parent (`parentUuid: null`, written so): the first row of a
/// conversation's chain.
fn opens_the_conversation(v: &Value) -> bool {
    v.get("parentUuid").is_some_and(Value::is_null)
}

/// A `user` row as an entry: `None` for Claude Code's own (its meta rows, a compaction's summary,
/// a local command's rows, [`local_command`]); any other is a turn's input or a tool's result.
fn user_entry(v: &Value) -> Option<Entry> {
    let flag = |key: &str| v.get(key).and_then(Value::as_bool) == Some(true);
    if flag("isMeta") || flag("isCompactSummary") || flag("queueTranscriptOnly") {
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

/// Whether a `user` row's text is a local command's own (module docs): its invocation
/// (`<command-name>…`, a compaction's bare `/compact`, a prompt command's `<command-message>…`)
/// or its output (`<local-command-stdout>…`, `<local-command-stderr>…`), whatever it says.
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
        api_error: None,
        status: None,
        limit: None,
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
/// (its reset, its cause): the transcript says that Claude Code ended the
/// turn on a wall of that class — and, of a limit, WHICH limit it is
/// ([`limit_named`]: the screen's generic `Usage limit reached` footer names
/// none), and of an `API Error` whose words name nothing, the kind its
/// notice is typed as ([`refined`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Class {
    Limit,
    Context,
    Auth,
    Api,
    Memory,
}

pub(crate) fn class(kind: WallKind) -> Class {
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
/// `error` word Claude Code wrote with it (2.1.300's own list: a request it
/// refused, a model not found, an account on hold or unverified, an org that
/// allows no OAuth, a reply past its output token maximum, a cloud
/// provider's credentials are API errors; a billing error is a limit).
fn error_class(error: &str) -> Option<Class> {
    match error {
        "authentication_failed" => Some(Class::Auth),
        "rate_limit" | "billing_error" => Some(Class::Limit),
        "server_error"
        | "overloaded"
        | "unknown"
        | "invalid_request"
        | "model_not_found"
        | "max_output_tokens"
        | "account_on_hold"
        | "verification_required"
        | "oauth_org_not_allowed"
        | "cloud_credential_error" => Some(Class::Api),
        _ => None,
    }
}

/// The `apiError` kinds 2.1.300 documents as TERMINAL or a person's to fix
/// (its own schema's words: `tool_history_mismatch: … re-sending the same
/// history fails the same way, and rewinding the conversation is the
/// remedy`, `dlp_request_denied: … terminal for that request body`,
/// `safety_monitor_blocked: … terminal for the turn`): a notice that carries
/// one is a refused request ([`transcript_wall`]).
const TERMINAL_API_ERRORS: &[&str] = &[
    "tool_history_mismatch",
    "dlp_request_denied",
    "safety_monitor_blocked",
    "claude_code_version_too_old",
    "effort_requires_thinking",
    "advisor_incompatible",
    "pdf_too_large",
    "pdf_password_protected",
    "field_not_granted",
    "org_disabled_credential",
    "api_key_auth_disabled",
    "invalid_credential_header",
    "autocompact_thrashing",
];

/// The `apiError` kinds of the Cloud gateway's own sign-in (2.1.301's schema:
/// `gateway_signin_required: the Cloud gateway provider is configured and
/// nobody is signed in to it; no request was sent`, and
/// `gateway_session_expired`): the lost login whatever its words say, its
/// remedy the vendor's `/login`, never a retry (the residual review's R1).
/// The words are read first ([`aterm_phase::classify_wall`]); these hold
/// where a later build rewords them.
const LOGIN_API_ERRORS: &[&str] = &["gateway_signin_required", "gateway_session_expired"];

/// Whether a notice's typed `apiError` is the gateway's sign-in
/// ([`LOGIN_API_ERRORS`]).
fn signs_in(api_error: Option<&str>) -> bool {
    api_error.is_some_and(|a| LOGIN_API_ERRORS.contains(&a))
}

/// Whether a notice's typed fields say the LOGIN failed: the gateway's
/// sign-in ([`signs_in`]), or the `error` word `authentication_failed` with
/// no `apiError` of the vendor's terminal set ([`TERMINAL_API_ERRORS`]:
/// `org_disabled_credential`, `api_key_auth_disabled`, refusals a 403 also
/// carries that no login fixes).
fn lost_login(error: Option<&str>, api_error: Option<&str>) -> bool {
    signs_in(api_error)
        || (error.and_then(error_class) == Some(Class::Auth)
            && !api_error.is_some_and(|a| TERMINAL_API_ERRORS.contains(&a)))
}

/// The `error` words 2.1.300 writes with a request it refuses, sent again
/// unchanged or not ([`transcript_wall`]).
const REFUSED_ERRORS: &[&str] = &[
    "invalid_request",
    "model_not_found",
    "account_on_hold",
    "verification_required",
    "oauth_org_not_allowed",
];

/// THE WALL A CLOSED NOTICE IS (module docs, `A TURN THAT ENDED ON CLAUDE
/// CODE'S OWN ERROR ROW`): where the transcript's newest entry is Claude
/// Code's own notice and a `turn_duration` closed the turn after it, the
/// wall the notice is ([`notice_wall`]). `None` for an open turn and an end
/// that is no notice.
#[must_use]
pub(crate) fn transcript_wall(end: &TranscriptEnd) -> Option<WallKind> {
    if !end.closed {
        return None;
    }
    notice_wall(&end.last)
}

/// THE WALL CLAUDE CODE'S OWN NOTICE `entry` IS: the wall its words name
/// ([`aterm_phase::classify_wall`]; a notice whose remedy is the vendor's
/// `Run /compact` — an image past the many-image size, a request its images
/// and attachments pushed past the API's size — is the full context's,
/// which the policy's `/compact` answers), else the wall its typed fields
/// name, in this order — an `apiError` of the vendor's terminal set
/// ([`TERMINAL_API_ERRORS`]) or an `error` word of a refused request
/// ([`REFUSED_ERRORS`]) is [`aterm_phase::ApiCause::Request`]; a
/// `billing_error` is [`WallKind::Spend`]; an `apiErrorStatus` the vendor
/// does not retry (400-499 but 401, 408, 409, 429 and 451) is a refused
/// request, unless its `error` word is `authentication_failed`: that is the
/// lost login's whatever its status (2.1.300's `Failed to authenticate. API
/// Error: 401 …`, which no table names, and a 403's) — and
/// anything else (`unknown`, `server_error`, no status) the server's,
/// retried, never a refusal: the vendor's own fallbacks (`API Error:
/// terminated`, the bare `API Error`) pass. The table's own fallback for an
/// `API Error` it names nothing of (no status, the server's) leaves the
/// typed fields to decide; words that name nothing but the status they
/// print (`API Error: 403 Forbidden`,
/// [`aterm_phase::wall::named_by_status_alone`]) are the lost login's where
/// the typed fields say the login failed ([`lost_login`]), as a typed status
/// is. `None` for an entry that is no notice, and a
/// notice the table reads as no wall (`API Error: Request was aborted.`).
/// Nothing of the notice is typed: the act names the kind in aterm's own
/// words (H6-HARNESS-1).
fn notice_wall(entry: &Entry) -> Option<WallKind> {
    let Entry::Notice {
        text,
        error,
        api_error,
        status,
        ..
    } = entry
    else {
        return None;
    };
    match aterm_phase::classify_wall(text) {
        Some(WallKind::ApiError {
            code: None,
            cause: ApiCause::Server,
            ..
        }) => {}
        // `API Error: 403 Forbidden`, the API's JSON body behind its status:
        // words that name nothing but the status say only what its range
        // does, and the vendor's typed word that the LOGIN failed decides over
        // a status printed as over one typed (below): the lost login's
        // `/login`, never a refused request's slow retries (the review's
        // KR-LIM-3).
        Some(WallKind::ApiError { .. })
            if aterm_phase::wall::named_by_status_alone(text)
                && lost_login(error.as_deref(), api_error.as_deref()) =>
        {
            return Some(WallKind::Auth);
        }
        Some(kind) => return Some(kind),
        None if aterm_phase::wall::not_a_wall(text) => return None,
        // `An image in the conversation exceeds the dimension limit for
        // many-image requests (2000px). Run /compact to remove old images
        // from context, or start a new session.` and `Request too large (max
        // 32 MB; …). Accumulated images and attachments in the conversation
        // pushed the request over the limit. Run /compact, or …` (2.1.300,
        // `invalid_request`, drawn only where `/compact` is on): the vendor's
        // own remedy is the one the full context's act types.
        None if ["wall.compact_images", "wall.compact_attachments"]
            .iter()
            .any(|id| {
                text.to_lowercase()
                    .contains(&aterm_phase::anchor(id).to_lowercase())
            }) =>
        {
            return Some(WallKind::Context);
        }
        None => {}
    }
    if signs_in(api_error.as_deref()) {
        return Some(WallKind::Auth);
    }
    let refused = WallKind::ApiError {
        code: *status,
        retryable: false,
        cause: ApiCause::Request,
    };
    if api_error
        .as_deref()
        .is_some_and(|a| TERMINAL_API_ERRORS.contains(&a))
    {
        return Some(refused);
    }
    match error.as_deref() {
        Some(e) if REFUSED_ERRORS.contains(&e) => return Some(refused),
        Some("billing_error") => return Some(WallKind::Spend),
        _ => {}
    }
    // The vendor's typed word that the LOGIN failed decides over the status
    // it answered: a 403 `authentication_failed` (a token revoked, in words
    // no table names) is the lost login's, whose remedy is `/login`, never a
    // refused request retried slowly. The refusals a 403
    // also carries that no login fixes are typed otherwise
    // ([`TERMINAL_API_ERRORS`]: `org_disabled_credential`,
    // `api_key_auth_disabled`, …) and are read above.
    if error.as_deref().and_then(error_class) == Some(Class::Auth) {
        return Some(WallKind::Auth);
    }
    if status.is_some_and(|c| (400..500).contains(&c) && !matches!(c, 401 | 408 | 409 | 429 | 451))
    {
        return Some(refused);
    }
    Some(WallKind::ApiError {
        code: *status,
        retryable: status.is_none_or(|c| matches!(c, 408 | 409 | 429) || c >= 500),
        cause: ApiCause::Server,
    })
}

/// How many of a notice's first words the screen's last thing said must
/// show for the transcript's notice to be the row the screen draws
/// ([`Session::transcript_found`]): a wrap moves no word, and the words
/// that tell one notice from another are its first.
const SHOWN_WORDS: usize = 6;

/// Whether the screen's words `said` show the notice `text`: their first
/// [`SHOWN_WORDS`] words the same, whitespace as it falls.
fn shows(said: &str, text: &str) -> bool {
    let want: Vec<&str> = text.split_whitespace().take(SHOWN_WORDS).collect();
    !want.is_empty()
        && said
            .split_whitespace()
            .take(SHOWN_WORDS)
            .eq(want.iter().copied())
}

/// Whether the transcript's end confirms a screen wall of kind `screen`: its newest entry is
/// Claude Code's own notice of the same [`Class`], by its words through the table the screen is
/// read by ([`aterm_phase::classify_wall`]), else by its `error` word; words
/// naming nothing but their status confirm the lost login too where the
/// typed fields say the login failed ([`lost_login`]).
#[must_use]
pub(crate) fn confirms(screen: WallKind, end: &TranscriptEnd) -> bool {
    let Entry::Notice {
        text,
        error,
        api_error,
        ..
    } = &end.last
    else {
        return false;
    };
    // The table's own fallback for an `API Error` it names nothing of (no
    // status, the server's) says less than the `error` word, as no wall did.
    let named = aterm_phase::classify_wall(text);
    let fallback = matches!(
        named,
        Some(WallKind::ApiError {
            code: None,
            cause: ApiCause::Server,
            ..
        })
    );
    let notice = match named {
        Some(kind) if !fallback => Some(class(kind)),
        _ => error
            .as_deref()
            .and_then(error_class)
            .or_else(|| named.map(class)),
    };
    // Words that name nothing but the status they print (`API Error: 403
    // Forbidden`) are of their status's class, and the lost login's as well
    // where the typed fields say the login failed ([`notice_wall`]).
    let login = || {
        class(screen) == Class::Auth
            && aterm_phase::wall::named_by_status_alone(text)
            && lost_login(error.as_deref(), api_error.as_deref())
    };
    notice == Some(class(screen)) || login()
}

/// THE SCREEN'S UNNAMED `API Error` NAMED BY THE NOTICE THAT CONFIRMED IT
/// (module docs, `A TURN THAT ENDED ON CLAUDE CODE'S OWN ERROR ROW`): a
/// screen wall `w` that is the table's own fallback (an `API Error:
/// <the server's words>` with no status: the server's, retried), confirmed
/// by the transcript's end `end`, is the wall that notice is
/// ([`notice_wall`]: its typed `apiError`, `error` word and status) where
/// that is another of the same class — `API Error: Request blocked by your
/// organization's policy.` (`dlp_request_denied`), a minimum version an
/// organization requires (`claude_code_version_too_old`), `API Error
/// (<model>): … Try /model to switch to …` (`model_not_found`) are refused
/// requests, which the screen's words alone retried hourly for good. The
/// notice need not be closed: its typed fields are its own row's, and the
/// screen already ended the turn on it. It must be the row the screen
/// read, its first words the wall's own ([`shows`]): an older notice can be
/// the newest entry while the turn's own rows are passed over (a prompt
/// command's, before its reply or notice is written), and confirms the
/// class all the same. A screen wall its words name by the status they
/// print alone (`API Error: 403 Forbidden`,
/// [`aterm_phase::wall::named_by_status_alone`]) is renamed only to the lost
/// login, where the notice's typed fields say the login failed. `None`: any
/// other wall, a notice the screen does not show, one that names nothing
/// more, or one of another class (which [`confirms`] refused).
fn refined(w: &aterm_phase::Wall, end: &TranscriptEnd) -> Option<aterm_phase::Wall> {
    let unnamed = matches!(
        w.kind,
        WallKind::ApiError {
            code: None,
            cause: ApiCause::Server,
            ..
        }
    );
    let Entry::Notice {
        text, api_error, ..
    } = &end.last
    else {
        return None;
    };
    // The screen's `API Error: 403 Forbidden`, named by the status its words
    // print alone: the lost login where the notice's typed fields say the
    // login failed ([`notice_wall`]), and nothing else renamed (the review's
    // KR-LIM-3).
    let by_status = matches!(w.kind, WallKind::ApiError { code: Some(_), .. })
        && aterm_phase::wall::named_by_status_alone(text);
    if !(unnamed || by_status) || !shows(&w.message, text) {
        return None;
    }
    // Of the same class, save the gateway's sign-in, which is the lost login
    // whatever the words ([`LOGIN_API_ERRORS`]).
    let login = signs_in(api_error.as_deref());
    let kind = notice_wall(&end.last).filter(|k| {
        *k != w.kind
            && if by_status {
                *k == WallKind::Auth
            } else {
                class(*k) == class(w.kind) || (login && *k == WallKind::Auth)
            }
    })?;
    Some(aterm_phase::Wall { kind, ..w.clone() })
}

/// `reading` read with `wall` in the screen's place, as the reader reads a
/// limit's notice: its phase `limited`.
fn place_standing(reading: &mut aterm_phase::Reading, wall: aterm_phase::Wall) {
    reading.phase = Phase::Limited {
        message: wall.message.clone(),
        reset: wall.reset.clone(),
    };
    reading.wall = Some(wall);
}

/// The limit the transcript's end NAMES for the screen's limit wall `w`,
/// where that is another kind than the screen read ([`Named`]); `None` for a
/// wall of another class, a notice that names no limit, and one of the
/// screen's own kind (a model's bucket the same bucket, however its consent
/// reads).
fn named_by(w: &aterm_phase::Wall, end: &TranscriptEnd) -> Option<Named> {
    let Entry::Notice { text, limit, .. } = &end.last else {
        return None;
    };
    if class(w.kind) != Class::Limit {
        return None;
    }
    let kind = limit_named(text, limit.as_deref())?;
    if std::mem::discriminant(&kind) == std::mem::discriminant(&w.kind) {
        return None;
    }
    let text = limit_words(text, limit.as_deref());
    let message = if limit::resumes_by_itself(&w.message) {
        format!("{text} \u{2014} {}", w.message)
    } else {
        text
    };
    Some(Named { kind, message })
}

/// WHICH LIMIT a usage limit's notice row names, by the row itself: its
/// words, through the wall table the screen is read by
/// ([`aterm_phase::classify_wall`]), else the account's limit it was written
/// for (`quotaLimits.rateLimitType`, the vendor's own table read in the
/// 2.1.286 bundle: `five_hour` the session limit, `seven_day` the weekly,
/// `seven_day_sonnet`, `seven_day_opus` and `seven_day_overage_included` a
/// model's bucket, `overage` the usage credits) — which also names the limit
/// of words that name none of their own (`You've hit your limit · …`, read
/// as a session limit that names no window). `None` where neither names a
/// limit.
pub(crate) fn limit_named(text: &str, limit: Option<&str>) -> Option<WallKind> {
    let typed = limit.and_then(|limit| match limit {
        "five_hour" => Some(WallKind::UsageSession),
        "seven_day" => Some(WallKind::UsageWeekly),
        "seven_day_sonnet" | "seven_day_opus" | "seven_day_overage_included" => {
            Some(WallKind::ModelBucket { consent: false })
        }
        "overage" => Some(WallKind::Spend),
        _ => None,
    });
    match aterm_phase::classify_wall(text).filter(WallKind::reads_limited) {
        Some(WallKind::UsageSession) if !limit::names_a_session_window(text) => {
            typed.or(Some(WallKind::UsageSession))
        }
        Some(kind) => Some(kind),
        None => typed,
    }
}

/// The wall a closed notice the screen could not name is
/// ([`Session::transcript_found`]), and its words: a limit as its row names
/// it ([`limit_named`], [`limit_words`]: a model's bucket its
/// `rateLimitType` names where the words name none, as where the screen
/// read the limit), any other wall the notice's own words.
pub(super) fn found_named(kind: WallKind, text: &str, limit: Option<&str>) -> (WallKind, String) {
    match limit_named(text, limit).filter(|_| class(kind) == Class::Limit) {
        Some(named) => (named, limit_words(text, limit)),
        None => (kind, one_line(text)),
    }
}

/// A LIMIT ROW'S WORDS as the wall's message: the row's own, on one line —
/// and where they name no model and its `quotaLimits.rateLimitType` names a
/// model's bucket (`seven_day_opus`, `seven_day_sonnet`), that type after
/// them, so the bucket's model is read off the message as off words that
/// name it ([`super::super::policy::turn_end::bucket_model`]): the fallback is
/// never the very model at its limit, and the way back returns to it.
/// Transcript text, never typed.
pub(super) fn limit_words(text: &str, limit: Option<&str>) -> String {
    let words = one_line(text);
    let bucket = |l: &&str| {
        matches!(
            limit_named(text, Some(l)),
            Some(WallKind::ModelBucket { .. })
        ) && super::super::policy::turn_end::bucket_model(&words).is_none()
            && super::super::policy::turn_end::bucket_model(l).is_some()
    };
    match limit.filter(bucket) {
        Some(limit) => format!("{words} \u{2014} {limit}"),
        None => words,
    }
}

/// THE KIND [`limit_words`]'S WORDS READ BACK AS where only the words are
/// kept (the ledger's wall row, `approvals::wall_wait_reason`): a model's
/// bucket its `rateLimitType` named after words that name no limit (`You've
/// hit your limit · resets 4pm — seven_day_opus`) is that bucket, as it was
/// read ([`limit_named`]); any other notice is its words'
/// ([`aterm_phase::classify_wall`]). Read by its words alone, that bucket was
/// a usage window: a try at it was never met again
/// (`approvals::UsageAttempt::met_ms`), and a loop that follows took up its open
/// wall as a window's track (`Session::seed_wall_memory`; the verifier of
/// e09018898, B5-3).
pub(in crate::supervise) fn words_kind(words: &str) -> Option<WallKind> {
    if let Some((text, limit)) = words.rsplit_once(" \u{2014} ")
        && limit_words(text, Some(limit)) == words
    {
        return limit_named(text, Some(limit));
    }
    aterm_phase::classify_wall(words)
}

/// THE LIMIT THE TRANSCRIPT NAMES where the screen read another of its class
/// ([`Session::confirm_wall`]): the kind of the notice row that confirmed it
/// ([`limit_named`]), and that row's words ([`limit_words`]) — the screen's
/// own after them where they say the vendor goes on by itself, so its
/// countdown is kept.
/// SCREEN AND TRANSCRIPT TEXT both, never typed: the model a bucket names
/// ([`super::policy::turn_end::bucket_model`]) and the escalations a person
/// reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Named {
    pub(super) kind: WallKind,
    pub(super) message: String,
}

/// How a reading's wall is checked ([`Session::confirm_wall`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Check {
    /// A read the point is decided on: the transcript is asked, and its verdict kept for the
    /// screen.
    Decide,
    /// The point read again for the loop's own books after its decision: the wall keeps the
    /// decision's verdict at that screen, and the transcript, moved on by the act itself, is not
    /// asked. A screen no decision was read at is checked as a decision's.
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

/// The limit a cancel left that a decision's read took for the answer to a turn
/// ([`TurnEndState::left_answer`]), by the screen it was read at ([`Session::left_read`]).
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

/// A LIMIT ONLY THE TRANSCRIPT SHOWS ([`Session::read_standing_limit`]):
/// whom the host was last asked about, and what it said.
#[derive(Debug, Clone, Default)]
pub(super) struct StandingRead {
    /// WHOM the host was last asked about: the agent's process by its pid
    /// and the conversation it holds, as the host read them in answering
    /// ([`IdleHost::agent_pid`], [`IdleHost::agent_conversation`], read after
    /// the ask; `None` where it names none) — recorded once an OPEN read
    /// asked (no wall on the screen, an idle point or a question), so asked
    /// once a process and conversation, and again where the host named no
    /// conversation yet.
    asked_for: Option<(Option<u32>, Option<String>)>,
    /// The limit row the host named, held while it stands: the wall made of
    /// it, the reset its row names, the row's id, and the last thing the
    /// screen said when it was read (`aterm_phase::last_said_row`) — a
    /// screen that says something else since, no read having seen a turn
    /// run, is asked about again.
    found: Option<Standing>,
    /// The screen the last decision placed it on ([`Check::AsDecided`]'s
    /// reads of that screen take it too).
    placed: Option<u64>,
}

/// A limit row the host named ([`StandingRead::found`]).
#[derive(Debug, Clone)]
struct Standing {
    wall: aterm_phase::Wall,
    until: Option<u64>,
    row: Option<String>,
    said: Option<String>,
    conversation: Conversation,
    /// Found at an ask right after a turn seen running: the transcript may
    /// not have written that turn's reply yet, so the next open decision
    /// asks again ([`Session::read_standing_limit`]).
    after_turn: bool,
}

/// The verdict of the last decision's read ([`Check::Decide`]).
#[derive(Debug, Clone)]
pub(super) struct Verdict {
    /// The screen it was read at.
    pub(super) seq: u64,
    /// Its wall, refused; `None`: kept (confirmed, not asked, or none).
    refused: Option<Refusal>,
    /// The wall the transcript named where the screen read none
    /// ([`Session::transcript_found`]), or named for the screen's unnamed
    /// `API Error` it confirmed ([`refined`]): put in the reading again at
    /// every read of the same screen.
    found: Option<aterm_phase::Wall>,
    /// Its wall kept as ANOTHER LIMIT than the screen read: the one the
    /// transcript's row names ([`Named`]). Every read of the same screen
    /// takes it, the decision's and its books' alike, so the wall the loop
    /// remembers, persists and opens its episode on is the one decided.
    /// Never set beside `found`: `named` is a limit the screen read, `found`
    /// a wall where the screen read none or an `API Error` it could not name.
    named: Option<Named>,
    /// The current context actually read at this point, distinct from the
    /// accepted wall's owner where its remembered authority belongs elsewhere.
    pub(super) conversation: Option<Conversation>,
}

/// A row that opens in column 0 with words: the owner an owner walk reaches
/// (`⏺`, `❯`, a done row), as the screen's own walk reads it.
fn is_owner_row(row: &str) -> bool {
    !row.is_empty() && !row.starts_with(char::is_whitespace)
}

/// Whether two reads of a screen are the same rows, trailing blanks aside.
pub(super) fn same_rows(a: &[String], b: &[String]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.trim_end() == y.trim_end())
}

/// The most rows of the history above the screen a wall's owner is looked
/// for in ([`Session::rows_up_to_owner`]): an owner further up counts as a
/// call (a verbose output longer than this past the screen's top, with no
/// transcript, gets no act).
pub(super) const OWNER_ROWS: u64 = 400;

/// The rows [`Session::rows_up_to_at`] read above the screen, oldest first,
/// and the absolute row of the screen's first row (`None` on the alternate
/// screen, whose archive has no absolute rows).
pub(super) type RowsAbove = (Vec<String>, Option<u64>);
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
    /// point's phase ([`Self::point_as_decided`]). A LIMIT the transcript
    /// confirms as ANOTHER of its class than the screen read (2.1.300's
    /// footer draws the generic `⚠ Usage limit reached · …` over a model's
    /// bucket: KI-1015-1 W3) is kept as the one the transcript's row names
    /// ([`Named`]): its kind and words in the reading's wall and its
    /// `limited` phase, its reset as the screen reads it.
    pub(super) fn confirm_wall(
        &mut self,
        reading: &mut aterm_phase::Reading,
        screen: &Screen,
        opts: &SuperviseOpts,
        check: Check,
    ) {
        let seq = screen.seq;
        let decided = self.wall_verdict.as_ref().filter(|v| v.seq == seq);
        let (refused, found, named) = match (check, decided) {
            (Check::AsDecided, Some(v)) => (v.refused.is_some(), v.found.clone(), v.named.clone()),
            _ => {
                // What the transcript's notice is typed as, and when it was
                // written, told afresh at every decision
                // ([`TurnEndState::notice_typed_was`],
                // [`TurnEndState::notice_written_at`]). Until a read names
                // the conversation, the point's evidence is what no read
                // named: unread under a host, except the hostless lane's on
                // an agent whose screen is all the evidence.
                self.wall_conversation = Some((
                    seq,
                    Conversation::unread(opts.idle_host.is_some(), reading.program),
                ));
                self.turn_end.notice_typed_was(None);
                self.turn_end.notice_written_at(None);
                let mut refined_wall = None;
                let (refused, named) =
                    match self.judge_wall(reading, screen, opts, &mut refined_wall) {
                        Ok(named) => (None, named),
                        Err(refusal) => (Some(refusal), None),
                    };
                let found = refined_wall.or_else(|| self.transcript_found(reading, screen, opts));
                let out = refused.is_some();
                // No refused wall at a decision: the refused one went away,
                // whichever step takes the point (the host's own among them,
                // which says nothing of walls), and the next is said again.
                if !out {
                    self.wall_refused_said = None;
                }
                self.wall_verdict = Some(Verdict {
                    seq,
                    refused,
                    found: found.clone(),
                    named: named.clone(),
                    conversation: None,
                });
                (out, found, named)
            }
        };
        apply_verdict(reading, screen, refused, found, named);
    }

    /// THE DECISION'S WALL VERDICT FOR `screen`, put on `reading` and nothing
    /// asked (the cross-lane review's XT-2): the wall the decision at this
    /// screen (its `seq`) found, named or refused ([`Self::confirm_wall`]),
    /// as [`apply_verdict`] puts it. What the host's gate reads a point by
    /// (`Session::host_steps_here`): a wall only the transcript named
    /// (`model_not_found`, an account on hold, a DLP refusal) withholds the
    /// host's step as the screen's own does, and a screen wall the decision
    /// refused does not. Reads no transcript, records nothing and says
    /// nothing; a screen no decision read is left as read.
    pub(super) fn wall_as_decided(&self, reading: &mut aterm_phase::Reading, screen: &Screen) {
        if let Some(v) = self.wall_verdict.as_ref().filter(|v| v.seq == screen.seq) {
            apply_verdict(
                reading,
                screen,
                v.refused.is_some(),
                v.found.clone(),
                v.named.clone(),
            );
        }
    }

    /// THE DECIDED POINT READ AGAIN AT A MOVED SCREEN (the decision review's
    /// RR-DEC-2): the look's point already handed over, at `new` where its
    /// decision read it at `old`, its review key unchanged — a footer tick,
    /// a banner, a repaint, nothing the point is made of. It is not decided
    /// again, so what the decision read of its wall is moved to `new`: the
    /// verdict ([`Self::wall_as_decided`], the host's gate), a limit a
    /// cancel left read as a turn's answer ([`LeftRead`]) and a limit only
    /// the transcript shows ([`StandingRead::placed`]), each only where it
    /// was the decision's at `old`. Read-only: nothing is asked.
    pub(super) fn carry_point(&mut self, old: u64, new: u64) {
        if let Some(v) = self.wall_verdict.as_mut().filter(|v| v.seq == old) {
            v.seq = new;
        }
        if let Some(l) = self.left_read.as_mut().filter(|l| l.seq == old) {
            l.seq = new;
        }
        if let Some((seq, _)) = self
            .wall_conversation
            .as_mut()
            .filter(|(seq, _)| *seq == old)
        {
            *seq = new;
        }
        if self.standing_read.placed == Some(old) {
            self.standing_read.placed = Some(new);
        }
    }

    /// The decision's check of `reading`'s wall ([`Self::confirm_wall`]):
    /// `Ok` where it stands — with the limit the transcript names where that
    /// is another than the screen read ([`Named`]) — else its refusal. A
    /// confirmed unnamed `API Error` the notice names better ([`refined`]) is
    /// put in `refined_wall`: the two are of different classes, so at most
    /// one is named.
    fn judge_wall(
        &mut self,
        reading: &aterm_phase::Reading,
        screen: &Screen,
        opts: &SuperviseOpts,
        refined_wall: &mut Option<aterm_phase::Wall>,
    ) -> Result<Option<Named>, Refusal> {
        let wall = match reading.program {
            Program::Claude => reading.wall.as_ref().filter(|w| w.kind != WallKind::Memory),
            Program::Codex | Program::Agy | Program::Muse | Program::Gemini | Program::Generic => {
                None
            }
        };
        let Some(w) = wall else {
            return Ok(None);
        };
        let host = opts.idle_host.as_ref();
        // A limit a cancel left read as a turn's answer at this screen ([`Session::left_read`]):
        // when that turn was asked, on the loop's clock, and whether the screen alone tells its
        // notice from the one the policy already saw.
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
            Err(Refusal {
                key: format!("{} left, untold", w.kind.name()),
                line: None,
            })
        };
        let why = match host.and_then(|h| h.transcript_tail()) {
            // No transcript to tell the notice drawn from the one already seen: no answer.
            None if untold => return unsaid(self),
            // No transcript: the screen alone decides, never on a wall read under a tool's call
            // on the whole screen (module docs).
            None if self.under_call_on_screen(w, screen) => {
                "read under a tool's call, and no transcript to confirm it".to_string()
            }
            None => return Ok(None),
            Some(mut tail) => {
                let mut end = transcript_ends_on(&tail);
                for _ in 0..LAG_TRIES {
                    if !end.lagging() {
                        break;
                    }
                    self.fx.sleep(LAG_STEP);
                    match host.and_then(|h| h.transcript_tail_again()) {
                        Some(next) => {
                            end = transcript_ends_on(&next);
                            tail = next;
                        }
                        None => break,
                    }
                }
                self.turn_end.notice_typed_was(end.typed());
                self.turn_end.notice_written_at(end.notice_at());
                // A limit a cancel left, read as a turn's answer, is that turn's only where its
                // notice row was written since the turn was asked (module docs); where the screen
                // cannot tell it from the notice already seen, a row naming no time says nothing.
                let older = left.is_some_and(|(since, tells)| {
                    end.at
                        .map_or(!tells, |at| at < since.saturating_sub(LEFT_ANSWER_SLACK_S))
                });
                if confirms(w.kind, &end) && !older {
                    self.wall_conversation = Some((screen.seq, conversation_of(&tail)));
                    *refined_wall = refined(w, &end);
                    let named = named_by(w, &end);
                    self.limit_notice_row = end.row.filter(|_| class(w.kind) == Class::Limit);
                    if let Some(l) = self.left_read.as_mut().filter(|l| l.seq == screen.seq) {
                        l.dated = end.at.is_some();
                    }
                    return Ok(named);
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
        Err(Refusal {
            key: format!("{} {words} {why}", w.kind.name()),
            line: Some(format!(
                "SKIPPED seq={} rule={RULE_WALL_CONFIRM} unconfirmed wall: {} — {why}: {}",
                screen.seq,
                w.kind.name(),
                clip(&words)
            )),
        })
    }

    /// THE WALL THE TRANSCRIPT NAMES WHERE THE SCREEN READ NONE (module docs,
    /// `A TURN THAT ENDED ON CLAUDE CODE'S OWN ERROR ROW`): at a decision's
    /// read of a Claude Code's idle point with no wall, where the screen's
    /// last thing said is a row Claude Code draws a notice of its own as
    /// ([`aterm_phase::phase::last_said_notice`]: a `⏺` message that is no
    /// call, or a vendor `⎿` block) and the session's transcript ends on a
    /// CLOSED notice whose first words that row shows ([`shows`]) — or, on
    /// the row right under a hook's error row, whose class it shows: the wall
    /// the notice is ([`transcript_wall`]), placed on that row, its words the
    /// notice's own. The transcript is read again for its lag
    /// ([`LAG_TRIES`]) only where the row has the vendor's error row's own
    /// shape (`API Error …`): a turn stopped with Esc writes no
    /// `turn_duration`, and an idle point that ends on the worker's prose
    /// is no reason to sleep. A `⏺` row shaped like prose is read again
    /// once, [`LAG_STEP`] later, where the transcript ends, open, on the
    /// turn's own input (its prompt, a tool's result): it has written
    /// nothing the screen says after it yet. The notice the row shows, its
    /// turn not closed yet, is read again until it closes ([`LAG_TRIES`] at
    /// most): its `turn_duration` follows it by milliseconds, and a read can
    /// fall between the two. `None` everywhere else — no host, no
    /// transcript, a row the table reads as no wall (`API Error: Request was
    /// aborted.`, not read), an open turn, a notice the screen does not show (a row
    /// the renderer hides, a `/resume`d conversation's older error under a
    /// newer screen) — and the point is the ordinary turn end it was.
    pub(super) fn transcript_found(
        &mut self,
        reading: &aterm_phase::Reading,
        screen: &Screen,
        opts: &SuperviseOpts,
    ) -> Option<aterm_phase::Wall> {
        let claude = match reading.program {
            Program::Claude => true,
            Program::Codex | Program::Agy | Program::Muse | Program::Gemini | Program::Generic => {
                false
            }
        };
        if reading.wall.is_some() || !claude || reading.phase != Phase::Idle {
            return None;
        }
        let host = opts.idle_host.as_ref()?;
        // A row the table reads as no wall (`API Error: Request was
        // aborted.`) names none, whatever the transcript says: not read.
        let said = aterm_phase::phase::last_said_notice(&screen.rows)
            .filter(|s| !aterm_phase::wall::not_a_wall(&s.text))?;
        let mut tail = host.transcript_tail()?;
        let mut end = transcript_ends_on(&tail);
        let error_row = said.placement == aterm_phase::Placement::ErrorRow;
        // THE TRANSCRIPT BEHIND THE SCREEN'S `⏺` ROW ON AN OLDER NOTICE: its
        // newest entry a notice that row does not show, with a prompt
        // command's invocation passed over after it ([`TranscriptEnd::prompted`]).
        // A prompt command's rows are no entry, so a request it sent right
        // after an error notice leaves that notice the newest entry until the
        // one its request met is written (read once, the new
        // notice drawn like prose was an ordinary turn end, and one
        // continuation was typed into it). Whatever the row is — that notice,
        // or the model's reply — the file has not written it yet.
        let behind = |end: &TranscriptEnd| {
            error_row
                && end.prompted
                && matches!(&end.last, Entry::Notice { text, .. } if !shows(&said.text, text))
        };
        // A row shaped like prose is read again ONCE, and only where the
        // transcript holds nothing the turn said after its input (it ends,
        // open, on the prompt or a tool's result, or on an older notice a
        // prompt command followed) under a `⏺` message: a reply already written
        // ends an ordinary turn, and is no reason to sleep.
        let unsaid = error_row
            && ((!end.closed && matches!(end.last, Entry::Input | Entry::ToolResult))
                || behind(&end));
        let tries = if said.shaped {
            LAG_TRIES
        } else {
            u32::from(unsaid)
        };
        for _ in 0..tries {
            if !end.lagging() && !behind(&end) {
                break;
            }
            self.fx.sleep(LAG_STEP);
            match host.transcript_tail_again() {
                Some(next) => {
                    end = transcript_ends_on(&next);
                    tail = next;
                }
                None => break,
            }
        }
        // Whether the transcript's newest entry is the notice the row shows:
        // its first words; or, on the row right under a hook's error row
        // (`SaidNotice::hook_words`), its class — the row is the screen's
        // spelling of the notice (`Context limit reached · …` for `Prompt is
        // too long`), where the hook drew no words of its own (a blocking
        // reason of blank space, 2.1.304, measured: KI-1015-14). The hook's
        // own words there in a notice's spelling leave the transcript on the
        // model's reply, which is no notice.
        let names = |end: &TranscriptEnd| match &end.last {
            Entry::Notice { text, .. } => {
                shows(&said.text, text)
                    || (said.hook_words
                        && aterm_phase::classify_wall(&said.text).is_some_and(|k| confirms(k, end)))
            }
            _ => false,
        };
        // The notice the screen shows, its turn not closed yet: Claude Code
        // writes the `turn_duration` right after it (1 to 14 ms later in every
        // measured transcript), and a read can fall between the two. Read
        // again until it closes, [`LAG_TRIES`] times at most.
        for _ in 0..LAG_TRIES {
            let open = !end.closed && names(&end);
            if !open {
                break;
            }
            self.fx.sleep(LAG_STEP);
            match host.transcript_tail_again() {
                Some(next) => {
                    end = transcript_ends_on(&next);
                    tail = next;
                }
                None => break,
            }
        }
        self.turn_end.notice_typed_was(end.typed());
        self.turn_end.notice_written_at(end.notice_at());
        if !names(&end) {
            return None;
        }
        let Entry::Notice { text, limit, .. } = &end.last else {
            return None;
        };
        let (kind, message) = found_named(transcript_wall(&end)?, text, limit.as_deref());
        self.wall_conversation = Some((screen.seq, conversation_of(&tail)));
        Some(aterm_phase::Wall {
            kind,
            message,
            reset: aterm_phase::notice_reset(text),
            row: said.row,
            placement: said.placement,
            under_call: false,
        })
    }

    /// A LIMIT ONLY THE TRANSCRIPT SHOWS IS THE WALL (KI-1015-1 LIM-1): where
    /// the screen reads no wall at a decision's read of a Claude Code idle
    /// point or question, and the host names a limit row the agent's
    /// conversation stands at ([`IdleHost::standing_limit`]: none where the
    /// harness moved the conversation off it), that row is read as the wall
    /// — its kind ([`limit_named`]), its words, the reset its words name, a
    /// [`aterm_phase::Placement::Gutter`] row — and the point as `limited`.
    /// Everything downstream then takes it as a wall confirmed here: the
    /// limit episode opens, the host's steps wait on it, the policy decides
    /// by the wall's own rule, the wall is remembered and its reset placed
    /// where the row names it ([`Self::standing_reset`]).
    ///
    /// WHY: the screen can draw none. A relaunched or restored Claude Code
    /// (2.1.300) redraws its conversation and then its own `⏺ Auto mode lets
    /// Claude …` paragraph under it (KI-1015-1 LC-10), or a cold restore
    /// reopens the tab under a new session with nothing remembered: the
    /// relaunch's carry-on, then `keep going`, were typed into a weekly limit
    /// still standing, where the upgrade's own notice, which reads the
    /// transcript, was held.
    ///
    /// ASKED ONCE A PROCESS AND CONVERSATION: at the first decision this
    /// loop reads OPEN — no wall on the screen, an idle point or a question
    /// — of the agent's process and the conversation it holds, as the host
    /// last read them (the loop's first, a relaunch's, a conversation the
    /// host names since: one it named none of yet is asked about again),
    /// whatever the policy tracks — the relaunched process's start-up reads
    /// busy, and read as work it dropped the limit's track; a decision that
    /// is not open asks nothing and records nothing. Asked too at an open
    /// decision of a point the host wants a step at, as that step asks
    /// ([`Session::host_steps_here`]), and at one after a turn seen
    /// running: a relaunch's first decision on the snapshot the host kept
    /// from before it names the old process, and its start-up is such a
    /// turn. Asked again while a row is held where the screen says something
    /// new with no turn seen running (a person's short turn, a local
    /// command), or where the host wants a step — and ONCE MORE at the next
    /// open decision after a row found right after a turn seen running: the
    /// loop decides the moment the busy row leaves, and Claude Code writes a
    /// reply's row a flush or more after it drew the reply (RR4-TE-3), so
    /// that ask can find the row the turn already served (the review of the
    /// lab's W1 fix, 2026-10-21: held, the point backed off and then waited
    /// on the remembered limit's conversation). A turn seen running drops
    /// what is held and asks afresh: the model answered (a reply after the
    /// row, which stands no limit), the screen draws the limit again, or a
    /// relaunched process started on a conversation still at it. With no
    /// turn seen running since (that once asked), the same process and
    /// conversation are asked nothing. A read for the books
    /// ([`Check::AsDecided`]) takes what the decision placed on its screen,
    /// and asks nothing.
    ///
    /// NOT WHERE THE SCREEN SHOWS THAT LIMIT ENDED under its own notice
    /// ([`ended_under`]: the vendor's own continuation drawn under it, or a
    /// turn served since): the screen is ahead of a transcript that has not
    /// written the reply yet, and the row is dropped. The host's answer
    /// already leaves out a row Claude Code's own continuation was written
    /// after (`upgrade::transcript_standing_limit`), the notice drawn or not.
    /// Returns true when this placement reuses an earlier answer: it is a
    /// remembered wall whose current conversation still needs a fresh read.
    pub(super) fn read_standing_limit(
        &mut self,
        reading: &mut aterm_phase::Reading,
        screen: &Screen,
        worked: bool,
        opts: &SuperviseOpts,
        check: Check,
    ) -> bool {
        if check == Check::AsDecided {
            if self.standing_read.placed == Some(screen.seq)
                && reading.wall.is_none()
                && let Some(found) = self.standing_read.found.clone()
            {
                place_standing(reading, found.wall);
            }
            return false;
        }
        self.standing_read.placed = None;
        let Some(host) = opts.idle_host.as_ref() else {
            return false;
        };
        match reading.program {
            Program::Claude => {}
            Program::Codex | Program::Agy | Program::Muse | Program::Gemini | Program::Generic => {
                return false;
            }
        }
        let said = aterm_phase::phase::last_said_row(&screen.rows).map(str::to_owned);
        let whom = (host.agent_pid(), host.agent_conversation());
        let unasked = self.standing_read.asked_for.as_ref() != Some(&whom);
        if unasked || worked {
            self.standing_read.found = None;
        }
        let open = reading.wall.is_none() && matches!(reading.phase, Phase::Idle | Phase::Question);
        let again = !unasked
            && !worked
            && self
                .standing_read
                .found
                .as_ref()
                .is_some_and(|f| f.said != said || f.after_turn);
        // A POINT THE HOST WANTS A STEP AT is asked about as that step asks
        // ([`Session::host_steps_here`]'s last guard): the decision and the
        // step read the point alike. Keyed on whom alone, a relaunch's first
        // decision on the snapshot the host kept from before it named the
        // process already asked about, asked nothing, and decided the
        // standing weekly limit away while the step, asked after it, held
        // its carry-on behind the same row. And A POINT
        // AFTER A TURN SEEN RUNNING: the turn drops what is held, and is no
        // proof the model answered — a relaunched process's start-up reads
        // busy, and a host that wants no step there (the carry-on settled or
        // recorded, a person's own relaunch) still names the old process, so
        // the decision read the limit away and backed off toward `keep going`
        // (the review's KR-LIM-2). The transcript says whether the model
        // answered: a reply after the row stands no limit.
        let steps = host.wants();
        let asked = open && (unasked || again || steps || worked);
        if asked {
            self.standing_read.found = host.standing_limit().and_then(|limit| {
                let kind = limit_named(&limit.text, limit.window.as_deref())?;
                let message = limit_words(&limit.text, limit.window.as_deref());
                Some(Standing {
                    wall: aterm_phase::Wall {
                        kind,
                        reset: aterm_phase::notice_reset(&one_line(&limit.text)),
                        message,
                        row: aterm_phase::phase::last_said_index(&screen.rows).unwrap_or(0),
                        placement: aterm_phase::Placement::Gutter,
                        under_call: false,
                    },
                    until: limit.until,
                    row: limit.row,
                    said: said.clone(),
                    // standing_limit refreshes the host snapshot before answering.
                    conversation: host
                        .agent_conversation()
                        .filter(|id| !id.is_empty())
                        .map_or(Conversation::Unknown, Conversation::Named),
                    after_turn: worked,
                })
            });
            // WHOM IT ANSWERED FOR, read after the ask: the host reads the
            // agent again to answer (the GUI's refreshes its snapshot), so
            // the key read before it can be older than the answer — a
            // relaunched process not yet named, its conversation not yet
            // registered — and the next decision asked it all over again.
            self.standing_read.asked_for = Some((host.agent_pid(), host.agent_conversation()));
        }
        // THE SCREEN AHEAD OF ITS TRANSCRIPT: Claude Code draws a reply before
        // it writes the reply's row, so a tail read at the turn's end can
        // still stand on the limit row the screen already shows ended under
        // that limit's own notice — the vendor's own continuation fired there
        // and answered (`⏺ Usage limit reset · continuing automatically`, the
        // reply under it), or a turn served since ([`ended_under`], the rule
        // that ends a remembered wall). The row is history: placed, the
        // landed defense under the continuation read as the limit, the point
        // backed off and nothing landed (the lab's W1 on 2.1.306,
        // 2026-10-21). Dropped, it is asked again only as any row is: a new
        // process or conversation, a turn seen running. The continuation's
        // own prompt row, which the tail holds by then, the host's answer
        // reads (`upgrade::transcript_standing_limit`: no limit stands past
        // it), where the reply has scrolled the notice off and this screen
        // shows nothing the limit ended under (the review of this fix).
        if open
            && self.standing_read.found.as_ref().is_some_and(|f| {
                ended_under(
                    aterm_phase::since_limit(&screen.rows).as_ref(),
                    f.wall.kind,
                    limit::ResetClock::of(f.wall.reset.as_deref()),
                )
            })
        {
            self.standing_read.found = None;
            return false;
        }
        if open && let Some(found) = self.standing_read.found.clone() {
            self.standing_read.placed = Some(screen.seq);
            self.wall_conversation = Some((screen.seq, found.conversation));
            self.limit_notice_row = found.row;
            place_standing(reading, found.wall);
            return !asked;
        }
        false
    }

    /// The reset of the limit only the transcript shows, placed on `seq`'s
    /// screen ([`Self::read_standing_limit`]), on the loop's clock: where its
    /// row names it (`quotaLimits.resetsAt`), not where its words would be
    /// placed on a print the screen does not show.
    pub(super) fn standing_reset(&self, seq: u64) -> Option<Instant> {
        let found = self
            .standing_read
            .found
            .as_ref()
            .filter(|_| self.standing_read.placed == Some(seq))?;
        found
            .until
            .and_then(|at| i64::try_from(at).ok())
            .map(|at| self.instant_of(at))
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
    /// own `/usage` probe is one of them. WITH NO NOTICE ROW REMEMBERED — a
    /// loop that took the wait up from its predecessor's ledger row, a limit
    /// the screen alone decided — the newest notice row that names the reset
    /// the policy waits on stands in for it ([`waited_notice_row`]): Claude
    /// Code's own continuation at the reset, whose reply scrolled the notice
    /// off the screen, ends the wait as in the loop that saw the notice (the
    /// verifier of the merged harness fixes: there the wait stood to the
    /// reset, and `keep going` was typed into the reply's done report).
    /// `false` with no transcript to read: the reset is waited out, as for a
    /// wall. With it, A MODEL CHOSEN since that same row
    /// ([`chosen_since_notice`], [`TurnEndReading::chosen_in_transcript`]):
    /// a person's `/model` off a bucket whose notice scrolled off a short
    /// pane, read from the one tail asked.
    pub(super) fn limit_transcript(&self, opts: &SuperviseOpts) -> LimitTranscript {
        let Some(host) = opts.idle_host.as_ref() else {
            return LimitTranscript {
                conversation: Conversation::Hostless,
                served: false,
                chosen: None,
                elsewhere: false,
            };
        };
        let Some(tail) = host.transcript_tail() else {
            return LimitTranscript {
                conversation: Conversation::Unknown,
                served: false,
                chosen: None,
                elsewhere: false,
            };
        };
        let kept = self.limit_notice_row.as_deref();
        let row = match (kept, self.turn_end.waited_limit()) {
            (Some(notice), _) => Some(notice.to_owned()),
            (None, Some(w)) => waited_notice_row(&tail, &w, self.unix_of(w.at)),
            (None, None) => None,
        };
        let elsewhere = kept.is_some_and(|id| {
            !tail.lines().any(|line| {
                crate::harness::transcript::conversation(line)
                    .ok()
                    .and_then(|v| row_id(&v))
                    .as_deref()
                    == Some(id)
            })
        });
        LimitTranscript {
            conversation: conversation_of(&tail),
            served: row
                .as_ref()
                .is_some_and(|row| served_since_notice(&tail, row)),
            chosen: row.as_ref().and_then(|row| chosen_since_notice(&tail, row)),
            elsewhere,
        }
    }

    #[cfg(test)]
    pub(super) fn notice_elsewhere(&self, opts: &SuperviseOpts) -> bool {
        self.limit_transcript(opts).elsewhere
    }

    /// THE TRANSCRIPT'S WAIT (BG-1): `screen`, which Claude Code's reader
    /// read IDLE, is a BREAK in the agent's own background work by the
    /// session's transcript ([`waits_on_background`] over the host's tail
    /// and its record of the process's launch), where the screen says
    /// nothing of how the last turn ended: its composer frame drawn and no
    /// glyph row above it at all ([`aterm_phase::phase::status_unseen`]: the
    /// waiting row scrolled off, or Claude Code draws none), and no prompt
    /// just submitted (the last thing said is no `❯` row). A drawn done or
    /// waiting row is fresher than the file, and the screen decides
    /// (`aterm_phase::phase`'s carried wait). The loop reads such a screen as
    /// the agent's background work, never a point ([`Self::await_turn_from`],
    /// as a remembered wall overrides a read), so no act is typed into a turn
    /// Claude Code itself will wake. The verdict is kept for the screen
    /// (`seq`) it was read at, and asked again once the screen moves. No
    /// host, no transcript or no launch record: `false`, the screen decides.
    pub(super) fn transcript_wait(&mut self, reader: &dyn ScreenReader, screen: &Screen) -> bool {
        let rows = &screen.rows;
        let claude = match reader.program() {
            Program::Claude => true,
            Program::Codex | Program::Agy | Program::Muse | Program::Gemini | Program::Generic => {
                false
            }
        };
        if !claude
            || !aterm_phase::phase::status_unseen(rows)
            || aterm_phase::phase::last_said_row(rows)
                .is_some_and(|r| r.trim_start().starts_with('❯'))
        {
            self.transcript_wait_seq = None;
            return false;
        }
        if self.transcript_wait_seq == Some(screen.seq) {
            return true;
        }
        // The tail first: the host reads the process in the tab afresh
        // there, and its launch is that process's.
        let waits = self.stall_host.as_ref().is_some_and(|host| {
            host.transcript_tail().is_some_and(|tail| {
                host.launched_at()
                    .is_some_and(|at| waits_on_background(&tail, at))
            })
        });
        self.transcript_wait_seq = waits.then_some(screen.seq);
        waits
    }

    /// THE TRANSCRIPT ENDS A CARRIED WAIT (BG-1, the verifier's correction
    /// 3): `screen`, which Claude Code's reader read BUSY by nothing but the
    /// wait it carries ([`aterm_phase::phase::carried_wait_alone`]: a
    /// `Waiting for N … to finish` row over the transcript, no status row
    /// drawn), is an IDLE point where the session's transcript says the turn
    /// closed since with nothing pending ([`ended_with_nothing_pending`] over
    /// the host's tail and its record of the process's launch). Claude Code's
    /// renderer decides which done rows it draws, and one that hid a finished
    /// wait's done row would leave the waiting row standing for good, the
    /// session never continued; the scoped transcript is the fresher word
    /// there. Anything less — no host, no transcript, no launch record, a
    /// row older than the launch, a later entry, pending work — is `false`,
    /// and the screen's soft busy stands. The verdict is kept for the screen
    /// (`seq`) it was read at ([`Self::turn_end_reading`] and
    /// [`Self::turn_of`] read that screen idle too); a screen still
    /// carrying the wait is asked again at each read, since the file can move
    /// on while the screen holds still — the point's due, the probe's close
    /// ([`Self::point_of`]) and the host's step ([`Self::host_steps_here`])
    /// read it so, as the look that decided the point did.
    pub(super) fn transcript_wait_over(
        &mut self,
        reader: &dyn ScreenReader,
        screen: &Screen,
    ) -> bool {
        let claude = match reader.program() {
            Program::Claude => true,
            Program::Codex | Program::Agy | Program::Muse | Program::Gemini | Program::Generic => {
                false
            }
        };
        if !claude || !aterm_phase::phase::carried_wait_alone(&screen.rows) {
            self.transcript_over_seq = None;
            return false;
        }
        if self.transcript_over_seq == Some(screen.seq) {
            return true;
        }
        let over = self.stall_host.as_ref().is_some_and(|host| {
            host.transcript_tail().is_some_and(|tail| {
                host.launched_at()
                    .is_some_and(|at| ended_with_nothing_pending(&tail, at))
            })
        });
        self.transcript_over_seq = over.then_some(screen.seq);
        over
    }

    /// Whether wall `w`, read from `screen`, stands under a tool's call on the whole screen
    /// ([`aterm_phase::Wall::under_call`]). The loop reads the screen's tail, which can hold the
    /// wall and not the call above it, so a gutter wall read on a tail with no call over it is read
    /// again from the whole grid ([`Self::grid`], here alone: every other read keeps the tail) and
    /// stands only where that read finds the same wall and no call over it. A whole read that
    /// fails, or one that no longer reads the same wall, counts as under a call: nothing is acted
    /// on that the rows above were not read for.
    ///
    /// THE OWNER ABOVE THE SCREEN: where the walk reaches
    /// the whole grid's first row with no owner found
    /// ([`aterm_phase::phase::owner_above_rows`]) — a call row scrolled off
    /// over its long output — the rows right above the screen are read up
    /// to the owner ([`Self::rows_up_to_owner`]) and the wall read again with
    /// those rows over the screen's. Measured on 2.1.304: Claude Code's
    /// inline renderer scrolls its rows into the main screen's history, and
    /// the rows right above the screen are the call's (`⏺ Bash(…)`, its
    /// output's first rows) with no gap; on the alternate screen, where the
    /// fullscreen renderer keeps no history, aterm's archive of the rows that
    /// scrolled off it holds them. On the base such a wall was read under no
    /// owner and acted on: a tool's own words in a later column-2 row (an MCP
    /// result's second block, measured on 2.1.294) typed `/login`. Rows that
    /// cannot all be read — an evicted or unreadable row, a gap in the
    /// archive, an owner further up than [`OWNER_ROWS`], a screen that moved
    /// under the reads — count as under a call, as a failed whole read does.
    fn under_call_on_screen(&mut self, w: &aterm_phase::Wall, screen: &Screen) -> bool {
        if w.under_call {
            return true;
        }
        if w.placement != aterm_phase::Placement::Gutter {
            return false;
        }
        let (rows, cursor_col, wall) = if screen.first == 0 {
            (screen.rows.clone(), screen.cursor_col, w.clone())
        } else {
            let Ok(whole) = self.grid() else {
                return true;
            };
            let read = self
                .reader(&whole.rows)
                .read(&whole.rows, Some(whole.cursor_col));
            match read.wall {
                Some(x) if !x.under_call && x.kind == w.kind && x.message == w.message => {
                    (whole.rows, whole.cursor_col, x)
                }
                _ => return true,
            }
        };
        if !aterm_phase::phase::owner_above_rows(&rows, wall.row) {
            return false;
        }
        let Ok(Some(mut joined)) = self.rows_up_to_owner(&rows) else {
            return true;
        };
        joined.extend(rows);
        let read = self.reader(&joined).read(&joined, Some(cursor_col));
        read.wall
            .is_none_or(|x| x.under_call || x.kind != wall.kind || x.message != wall.message)
    }

    /// The rows right above the screen `rows` (the whole grid a wall was
    /// read on), oldest first, from the nearest row that opens in column 0 —
    /// the owner the screen's owner walk would reach — down to the screen's
    /// first row. On the main screen they are its HISTORY: `search` with no
    /// pattern places the screen, `line` reads each row up from it, and the
    /// grid read again after them is `rows` (and the screen's place the
    /// same), so they are the rows over the screen the wall was read on. On
    /// the alternate screen they are the ARCHIVE of the rows that scrolled
    /// off it ([`Self::archived_up_to`]). `None` where they cannot all
    /// be read: a host without the verbs, an evicted or unreadable row, no
    /// owner within [`OWNER_ROWS`], or the screen moved by the time the rows
    /// were read.
    pub(super) fn rows_up_to_owner(
        &mut self,
        rows: &[String],
    ) -> Result<Option<Vec<String>>, Fail> {
        self.rows_up_to(rows, &is_owner_row)
    }

    /// [`Self::rows_up_to_owner`], the walk up stopping at the nearest row
    /// `stop` names rather than any that opens in column 0 (a reader's
    /// [`aterm_phase::ScreenReader::said_history_stop`]).
    pub(super) fn rows_up_to(
        &mut self,
        rows: &[String],
        stop: &dyn Fn(&str) -> bool,
    ) -> Result<Option<Vec<String>>, Fail> {
        Ok(self.rows_up_to_at(rows, stop)?.map(|(above, _)| above))
    }

    /// [`Self::rows_up_to`], and the ABSOLUTE ROW of the screen's first row
    /// (`search`'s `screen_first=`, `line`'s coordinates), the rows above it
    /// counting down from it: what a reader of their cells addresses (`cell
    /// abs=<n>`). `None` for it on the alternate screen, whose archive has no
    /// such rows.
    pub(super) fn rows_up_to_at(
        &mut self,
        rows: &[String],
        stop: &dyn Fn(&str) -> bool,
    ) -> Result<Option<RowsAbove>, Fail> {
        if self.alt_screen_now()? {
            return Ok(self.archived_up_to(rows, stop)?.map(|above| (above, None)));
        }
        let screen_first = |s: &mut Self| -> Result<Option<u64>, Fail> {
            let r = s.call(&["search"])?;
            if s.unserved(&r) {
                return Err(Fail::Lost(format!("search failed: {}", r.stderr.trim())));
            }
            Ok(r.ok()
                .then(|| super::box_head::search_screen_first(&r))
                .flatten())
        };
        let Some(first) = screen_first(self)? else {
            return Ok(None);
        };
        // No history above the screen: nothing to read, and nothing read to
        // hold against the screen again.
        if first == 0 {
            return Ok(Some((Vec::new(), Some(first))));
        }
        let mut above = Vec::new();
        let mut at = first;
        while at > 0 {
            if first - at >= OWNER_ROWS {
                return Ok(None);
            }
            at -= 1;
            let r = self.call(&["line", &at.to_string()])?;
            if self.unserved(&r) {
                return Err(Fail::Lost(format!("line failed: {}", r.stderr.trim())));
            }
            if !r.ok() {
                return Ok(None);
            }
            let line = r.stdout.lines().next().unwrap_or("");
            let text = line.strip_prefix("OK").unwrap_or(line);
            let text = text.strip_prefix(' ').unwrap_or(text).to_string();
            let owner = stop(&text);
            above.push(text);
            if owner {
                break;
            }
        }
        if screen_first(self)? != Some(first) || !same_rows(&self.grid()?.rows, rows) {
            return Ok(None);
        }
        above.reverse();
        Ok(Some((above, Some(first))))
    }

    /// The rows of the ALTERNATE screen's archive right above the screen
    /// `rows`, oldest first, from the nearest that opens in column 0 (as
    /// [`Self::rows_up_to_owner`]): Claude Code's fullscreen renderer
    /// repaints the alternate screen in place and keeps no history, and
    /// aterm archives each row that leaves its top (`offscreen`). Read with
    /// the screen under the same lock (`offscreen since=<i> tail=<n>
    /// screen=1`, the archive's newest index read first): the screen read
    /// there is `rows`, the rows since that index none evicted (`lost=0`)
    /// and with no gap among them or after them (`breaks=0`: a redraw with no
    /// overlap, a resize, a reset), none shown on the screen again (`back=0`),
    /// the archive the same (`origin=`) and on. `None` otherwise, or with no
    /// owner among the newest [`OWNER_ROWS`]. Before, the alternate screen
    /// counted as under a call: a real wall under a long prompt there was
    /// left to the person with no transcript (the review of 87faa9bf23).
    fn archived_up_to(
        &mut self,
        rows: &[String],
        stop: &dyn Fn(&str) -> bool,
    ) -> Result<Option<Vec<String>>, Fail> {
        let read = |s: &mut Self, args: &[&str]| -> Result<Option<Offscreen>, Fail> {
            let mut call = vec!["offscreen"];
            call.extend_from_slice(args);
            let r = s.call(&call)?;
            if s.unserved(&r) {
                return Err(Fail::Lost(format!("offscreen failed: {}", r.stderr.trim())));
            }
            Ok(r.ok().then(|| parse_offscreen(&r).ok()).flatten())
        };
        let Some(head) = read(self, &["tail=1"])? else {
            return Ok(None);
        };
        let since = format!("since={}", head.last.saturating_sub(OWNER_ROWS));
        let tail = format!("tail={OWNER_ROWS}");
        let Some(off) = read(self, &[&since, &tail, "screen=1"])? else {
            return Ok(None);
        };
        let whole = off.alt
            && off.enabled
            && off.lost == 0
            && off.breaks == 0
            && off.back == 0
            && off.origin == head.origin
            && same_rows(&off.screen, rows);
        if !whole {
            return Ok(None);
        }
        Ok(off
            .archived
            .iter()
            .rposition(|r| stop(r))
            .map(|owner| off.archived[owner..].to_vec()))
    }

    /// The point `turn` read again as its decision read it
    /// ([`Self::confirm_wall`]): a limit whose wall was refused at that
    /// screen is no limit — no episode opens on it, nothing is escalated for
    /// it and the host is not told the session is limited; it is an idle
    /// point. A limit the transcript named where the screen read none
    /// ([`Self::transcript_found`]) is a limit point, as the same wall read
    /// off the screen is. A limit the transcript named as another of its
    /// class ([`Named`]) is that one: its words in the point's `limited`
    /// phase, the screen's reset text kept (what the episode's reset is
    /// matched by when the host's steps read the screen again). A LIMIT ONLY
    /// THE TRANSCRIPT SHOWS that the decision placed on the screen
    /// ([`Self::read_standing_limit`]) is a limit point too. And the LIMIT A
    /// CANCEL LEFT that the decision took for the
    /// answer to a turn ([`TurnEndState::left_answer`], kept as
    /// [`Session::left_read`]) and did not refuse is a limit point, as the
    /// same wall read before the cancel is: its episode opens, the host is
    /// told the session is limited, the host's own steps wait on it.
    pub(super) fn point_as_decided(&self, turn: &mut Turn) {
        let verdict = self
            .wall_verdict
            .as_ref()
            .filter(|v| v.seq == turn.screen.seq);
        let refused = verdict.is_some_and(|v| v.refused.is_some());
        let found = verdict
            .and_then(|v| v.found.as_ref())
            .filter(|w| w.kind.reads_limited());
        let named = verdict.and_then(|v| v.named.as_ref());
        if refused && matches!(turn.phase, Phase::Limited { .. }) {
            turn.phase = Phase::Idle;
        } else if let Some(w) = found.filter(|_| turn.phase == Phase::Idle) {
            // A limit the transcript named where the screen read none: a
            // limit point, as the same wall read off the screen is.
            turn.phase = Phase::Limited {
                message: w.message.clone(),
                reset: w.reset.clone(),
            };
        } else if let Some(n) = named
            && let Phase::Limited { message, .. } = &mut turn.phase
        {
            // The limit the transcript named: its words, the screen's reset.
            message.clone_from(&n.message);
        } else if !refused
            && turn.phase == Phase::Idle
            && let Some(l) = self.left_read.as_ref().filter(|l| l.seq == turn.screen.seq)
        {
            turn.phase = Phase::Limited {
                message: l.answer.wall.message.clone(),
                reset: l.answer.wall.reset.clone(),
            };
        }
        if matches!(turn.phase, Phase::Idle | Phase::Question)
            && self.standing_read.placed == Some(turn.screen.seq)
            && let Some(found) = self.standing_read.found.as_ref()
        {
            // A limit only the transcript shows: the limit point it is.
            turn.phase = Phase::Limited {
                message: found.wall.message.clone(),
                reset: found.wall.reset.clone(),
            };
        }
    }

    /// Journal the wall the last decision refused, once while it stands. Its episode ends where
    /// the wall goes away: a decision with no refused wall ([`Self::confirm_wall`]), the agent's
    /// live turn ([`Self::wall_refusal_over`]).
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

/// A decision's wall verdict (`refused`, the wall `found`, the limit
/// `named`) put on `reading` of `screen` ([`Session::confirm_wall`], and as
/// decided, [`Session::wall_as_decided`]).
fn apply_verdict(
    reading: &mut aterm_phase::Reading,
    screen: &Screen,
    refused: bool,
    found: Option<aterm_phase::Wall>,
    named: Option<Named>,
) {
    // A WALL THE SCREEN COULD NOT NAME, named by the transcript's closed
    // notice (module docs): a wall read here, its phase `limited` for a
    // limit as the reader reads one. Or the screen's unnamed `API Error`,
    // named by the notice that confirmed it ([`refined`]): the same row.
    if let Some(w) = found.filter(|f| {
        reading
            .wall
            .as_ref()
            .is_none_or(|w| w.row == f.row && w.message == f.message)
    }) {
        if w.kind.reads_limited() && reading.phase == Phase::Idle {
            reading.phase = Phase::Limited {
                message: w.message.clone(),
                reset: w.reset.clone(),
            };
        }
        reading.wall = Some(w);
        return;
    }
    // A LIMIT the transcript names as ANOTHER of its class ([`Named`]):
    // its kind and words, the screen's reset.
    if let Some(n) = named
        && let Some(w) = reading.wall.as_mut()
    {
        w.kind = n.kind;
        w.message.clone_from(&n.message);
        if let Phase::Limited { message, .. } = &mut reading.phase {
            *message = n.message;
        }
    }
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

#[cfg(test)]
#[path = "wall_confirm_tests.rs"]
mod tests;
