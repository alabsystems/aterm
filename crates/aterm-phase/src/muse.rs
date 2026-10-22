// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Muse Code's screen grammar (Meta's `muse`), MEASURED on two builds, both
//! on 2026-10-16:
//!
//! * muse 1.4.9-R6297.1 in a private headless aterm at 120×40, the vendor's
//!   build under a scratch HOME: its offline `--provider echo`, and the
//!   default provider signed out;
//! * muse 1.4.10-R6427.1 at 144×50 in the lab's jail (`crates/aterm-lab`):
//!   the real build with a scratch HOME, a fake `META_API_KEY` and
//!   `--base-url` at the lab's stub of Meta's model API, so no account was
//!   used and nothing left the machine. A model's calls drew its approval
//!   boxes, its question and its walls;
//! * 1.4.9 again with `--base-url` at a local scripted stand-in for Meta's
//!   Responses endpoint (a dummy key, no login and no model), which returned
//!   the tool calls that drew its boxes, several questions and their review;
//!   and the person's own 1.4.10, signed in, working a real task in a
//!   private headless aterm (the dogfood runs: its network box, its hint);
//! * 1.4.10-R6427.1 at 144×50, 100×30, 72 and 60 columns OUTSIDE the lab's
//!   jail, so its own managed sandbox applied: a private headless aterm
//!   against a local fake backend (the proxy variables at it, refusing any
//!   other host; an lsof sampler over the process tree saw no endpoint but
//!   loopback), whose calls of a local MCP server's tool and of `bash` with
//!   `unix_socket_paths` drew the tool-action box and the Unix-socket box.
//!
//! Fixtures in [`fixtures`], each naming its provenance on line 1 (1.4.9's
//! colours and cursor too). The screens 1.4.9 drew read the same on 1.4.10:
//! idle, a draft, a running turn, a turn's end, the `/` menu, the exit and
//! the trust box. [`crate::reader::MuseReader`] is this module behind the
//! reader trait.
//!
//! **THE LAYOUT.** Past its trust box Muse Code draws inline, its composer a
//! Claude-Code lookalike — the caret is Claude Code's `❯` — whose top rule
//! carries a label:
//!
//! ```text
//!   Muse Code 1.4.10                             the banner
//! ❯ hello there                                  the person's message
//! ◇ Thinking (2s · esc to interrupt)             the STATUS row, while a turn
//!                                                  runs (◇, ◈ and ◆ cycle)
//! ◆ Hi! …                                        the reply, once it ended
//! ── Voice input (⌥ + v to start) ────────────   the COMPOSER: the labelled rule,
//! ❯                                                the caret and NO placeholder
//! ────────────────────────────────────────────     (the cursor at column 2), a
//!   lab-stub · high · /path/to/work · Auto-review  rule and the footer
//! ```
//!
//! Signed in, 1.4.10 draws a grey hint (676c74) in the empty composer, `❯
//! /export saves the conversation to a text file` (measured on the person's
//! own build, [`fixtures::END_OF_TURN_FOOTNOTES_HINT`]): its colour tells it
//! from a draft. The rule's label follows voice input's state (the binary's
//! `Hold ⌥ v to talk`, `Recording`, `Transcribing`, `Voice offline`, `Voice
//! error`); any of them is Muse Code's. The SIDE CHAT's composer (`Side Chat
//! · …` on its rule, `Side chat mode` in its footer) is another
//! conversation's, and a DRAWER or a PICKER (`/model`, `/settings`, …, its
//! key row `↑↓ move · enter confirm · esc cancel`) takes the composer's
//! keys: neither is typed into. A session opens on its banner: rows above
//! it are an earlier session's in the tab ([`fixtures::IDLE_RELAUNCHED`]).
//!
//! `/` opens the command menu BETWEEN the caret row and the bottom rule
//! (`  /compact   Summarize …`, `  ↓ 43 more`). One ctrl+c at an empty
//! composer turns the footer into `  Press Ctrl-C again to quit`. `/usage`
//! and `/status` draw a card (`┌ … ┘` in column 0) into the transcript under
//! the turn's end; Esc does not close it, and the composer stays open under
//! it. A `/status` card names the run's state at its head's right (`IDLE`
//! before any turn, `RUNNING` while a turn runs, `COMPLETED` after a turn,
//! `FAILED` after a wall, `CANCELLED` after a person's Esc; measured
//! 2026-10-21); a `/usage` card names none, only the turns the session
//! counts (`0` before any, and after a first turn a person stopped).
//! Signed in with a Meta account (1.4.11, measured 2026-10-21,
//! [`fixtures::USAGE_SIGNED_IN`]) the `/status` card gains `ACCOUNT
//! Name (address)` and `BILLING Subscription · <tier>` rows and the
//! `/usage` card a subscription block (`Current NN% used · Resets …`,
//! `Weekly NN% used · Resets …`, `as of …`): what
//! [`status_card_account`] and [`usage_card_weekly`] read for the
//! window's footer, as agy's launch card is read for its account.
//! Entered while a turn runs, the card goes ABOVE the turn's status row,
//! which stays over the composer, and the turn's reply is drawn whole under
//! the card once it ends. So a screen of cards alone above the open
//! composer, the turn's end scrolled away under them, is a session at rest
//! on the run the newest card names (`Turn::Carded`): idle as evidence, its
//! last words out of sight — and, where that card says `IDLE`, a session
//! with no turn yet, its banner scrolled away under them (`Turn::Fresh`).
//! A local command's rows (`/rules` prints three, two in) are a block of
//! their own under a blank row: no part of the notice above them
//! ([`notice`]).
//!
//! **TYPING.** An Enter in the same burst as the text is taken as a NEWLINE
//! (the draft grows a row); an Enter after a pause submits (measured on
//! both builds: on 1.4.10 `keep going` and `\r` in one write left a draft of
//! two rows, [`fixtures::DRAFT_BURST_ENTER`]). A draft longer than the row
//! wraps between words, its continuation rows indented two
//! ([`fixtures::DRAFT_WRAPPED`]).
//!
//! **THE BOXES** (1.4.10). Muse Code asks before a shell call that wants to
//! leave its sandbox (`bash` with `sandbox_permissions: require_escalated`),
//! before a write or an edit to a protected path (`.agents/`), and before a
//! connection its tools make to a host (`web_fetch` of a URL, `monitor` of a
//! WebSocket; measured on the lab's own stub, loopback, after the approval
//! judge's side calls), before an MCP server's tool (one not annotated
//! `readOnlyHint`, which runs unasked) and before a shell call reaches a Unix
//! socket it names (`unix_socket_paths`, under the default `proxy-only`
//! sandbox network). Under the default on-request mode, a sandboxed command
//! or a write inside the workspace runs without asking. The edit draws the
//! write's box:
//!
//! | box | title | body | options |
//! |---|---|---|---|
//! | shell | `Would you like to run the following command?` | `  $ <command>` | `⟩ 1. Yes, proceed (y)` · `2. No, and tell Muse Code what to do differently (esc)` |
//! | file access | `Would you like to allow this file access?` | `  write path: <absolute path>` | the same |
//! | network | `Would you like to allow this network access?` | `  network: <host:port> http` · `  full URL: <url>` | `⟩ 1. Yes, proceed (y)` · `2. Yes, don't ask again this session (p)  <host:port> (http)` · `3. Always allow this network destination  <host:port> (http)` · `4. No, … (esc)` |
//! | tool action | `Would you like to allow this tool action?` | `  tool action: <tool>` | `⟩ 1. Yes, proceed (y)` · ``2. Yes, don't ask again this session (p)  tool `<tool>` `` · ``3. Always allow this MCP tool  tool `<tool>` `` · `4. No, … (esc)` |
//! | Unix socket | `Would you like to allow this network access?` | `  requested by:` · `  $ <command>` | `⟩ 1. Allow once (y)` · `2. Reject (esc)` |
//!
//! The network box is a [`PromptKind::NetworkRequest`] of its full URL, its
//! `network:` row its note ([`fixtures::BOX_NETWORK`]): `Yes, proceed` the
//! one-shot allow, the session grant a [`Role::Session`], the always grant a
//! [`Role::Persist`], `No, …` the refusal; the destination a grant names
//! beside its label (past two blanks) is no part of the label. In a
//! 60-column tab the URL breaks at the row's edge, its tail two in (joined
//! with nothing, as a path is), the session grant's destination is cut at
//! the edge, and the always grant's `(http)` goes onto a row of its own in
//! COLUMN 0, read as that option's ([`fixtures::BOX_NETWORK_NARROW`]).
//!
//! A body too long for its row WRAPS (measured at 60 columns): the command
//! between words, its rows indented four, and a word longer than the row
//! broken at the row's edge ([`fixtures::BOX_SHELL_WRAPPED`]); a path that
//! does not fit beside `write path:` goes under it, alone on its row, the
//! path's rows indented two and broken at the edge
//! ([`fixtures::BOX_WRITE_WRAPPED`]). The command's rows are joined with a
//! blank, as Codex's and agy's are, so a word broken at the edge reads with
//! a blank in it; the path's rows are joined with nothing.
//!
//! A sandboxed pipeline may be asked for ONE STAGE at a time (measured on
//! 1.4.10, `env | cut -d= -f1 | sort > envnames.txt`): the shell box's title
//! and command, then `Skipped Stage 1/3: matched mode:on_request`, `Stage
//! 3/3` and `Current argv: ["sort"]` (its notes), and the options `⟩ 1.
//! Allow this stage once (y)` · `2. Abort the entire command (esc)`, the
//! one-shot allow and the refusal ([`fixtures::BOX_SHELL_STAGE`]).
//!
//! Each is drawn INSIDE the composer's frame, in the caret row's place:
//! the labelled rule, the title, the body, the options, `Change approval
//! defaults in /permissions`, the bottom rule and the footer. The status
//! row `◆ Calling tools (2s · esc to interrupt)` stays above the frame.
//! Enter on the focused first option runs the call. Esc denies it (`◆ Ran
//! touch probe.txt — denied` over `└ approval aborted`), and the result sent
//! back reads `tool denied: approval aborted …`. `Yes, proceed` is the
//! one-shot allow, `No, …` the refusal, a yes naming `this session` a
//! session grant, and any other `Yes, …` — or `Always allow …` — a standing
//! grant. The binary holds `Yes, and don't ask again for this command in
//! this session` too, not drawn here. Its Esc is the option marked `(esc)`.
//! Every measured box draws its title on the row under the rule: with that
//! row blank, the box is caught mid-draw and is no box yet. On 1.4.9
//! `--approval-mode untrusted` draws the stage box with `Always allow in
//! this workspace: …` between its allow and its abort, a standing grant
//! ([`fixtures::BOX_COMMAND_STAGED`]).
//!
//! The network box a sandboxed `pip install` drew on the person's 1.4.10
//! names no URL: `requested by:` and the command (`  $ …`) under its title,
//! where the lab's web fetch drew `network:` and `full URL:`, and the same
//! four options, the session grant and `Always allow this network
//! destination` naming the host beside their labels
//! ([`fixtures::BOX_NETWORK_COMMAND`]). With no `full URL:` row it is a box
//! of no kind, read with no roles: a person's. Its command (the `$ ` row and
//! the rows it wrapped onto, joined with a blank as the shell box's are) is
//! read as [`PromptV2::command`], for the person it goes to; no command row
//! is read, so nothing classifies it as a shell call.
//!
//! DIGITS AND `y` ANSWER A BOX AT ONCE (measured): `2` refused a shell box
//! and `1` ran one, `y` ran one, `4` refused a network box and `1` ran one —
//! no key guard was seen (a `4` pressed the moment the box was drawn took).
//! A key typed before the box is drawn goes to the composer
//! ([`fixtures::BOX_NETWORK_DIGIT_APPROVED`] holds a draft `1`). The harness
//! still chooses by the cursor and Enter ([`Select::ArrowsEnter`]): the
//! one-shot allow is the option focused as drawn, and its Enter is guarded
//! on that very row, where a digit is guarded on nothing that names it.
//!
//! THE TOOL-ACTION BOX (measured outside the lab's jail): an MCP server's
//! tool, stdio or streamable HTTP alike, is asked for under `tool action:
//! <tool>` (its session id, `mcp__<server>__<tool>`); no row names the
//! server, its command or its URL. It is a [`PromptKind::Tool`] of that
//! tool ([`fixtures::BOX_TOOL_ACTION`]): `Yes, proceed` the one-shot allow
//! (the tool ran once, and the next call asked again), the session grant a
//! [`Role::Session`], `Always allow this MCP tool` a [`Role::Persist`],
//! `No, …` the refusal (Esc: `◆ Used tool operation — denied` over `└
//! approval aborted`, and no call reached the server); each grant's ``tool
//! `<tool>` `` beside its label is no part of it. At 100×30 and 72 columns
//! it is drawn as at 144; at 60 the session grant's tool is cut at the
//! row's edge ([`fixtures::BOX_TOOL_ACTION_NARROW`]). Before it was read it
//! was a box of no kind and no roles, its tool unread.
//!
//! THE UNIX-SOCKET BOX (measured there too): a `bash` call naming a Unix
//! socket draws the NETWORK title over `requested by:` and the command, as
//! the network box of a command does, but its options are `⟩ 1. Allow once
//! (y)` and `2. Reject (esc)`, and no row names the socket
//! ([`fixtures::BOX_UNIX_SOCKET`]). Allowed, the command reached the socket;
//! Esc refused it (`Unix socket access was not approved: "<path>"`). With no
//! `full URL:` row it is a box of no kind, read with no roles — a person's,
//! its command read, as the network box of a command is — its Esc a refusal
//! by its label (read before as leaving the box undecided). At 72 and 60
//! columns the command wraps between words, its tail indented four.
//!
//! **THE BACKGROUND-WORK PANEL** (1.4.10): while a monitor, a backgrounded
//! shell or other work runs (or just failed), the frame draws a panel
//! between its bottom rule and the footer — `main · ↓ to select` (`main`
//! alone while the composer holds a draft) over a branch per piece of work,
//! `└ ◆ Wait in the background  running  3s` — and the transcript a `(↓ to
//! view monitors)` row ([`fixtures::END_OF_TURN_WORK_RUNNING`],
//! [`fixtures::BOX_NETWORK_MONITOR`]). The composer, a box and the footer
//! read over it as without it. Before the reader read it, every screen with
//! the panel read as no composer, and the network box over it as idle. When
//! the backgrounded shell ended, `◆ Finished · <description> · ✓` was drawn
//! and Muse Code took a turn of its own on the output. The panel's head
//! reads `main` in a git repository on another branch too (measured on a
//! branch `topic-review`): it names the session's main thread, not a
//! branch. A session's first backgrounded shell (or monitor) draws the
//! transcript's pointer, `  (↓ to view terminals)`, as a block of its own
//! UNDER the reply of the turn that sent it there, a moment after it
//! ([`fixtures::END_OF_TURN_WORK_HINT`]): no part of the reply, so the turn
//! still ends on the reply's last words (a done token, a question).
//!
//! A branch's STATE says whether its work still runs: `running` (a
//! backgrounded shell) and `watching` (a monitor) while it does, `ran 1s,
//! exit 0` and `failed` once it is over (measured on 1.4.10 again on
//! 2026-10-17, [`fixtures::END_OF_TURN_BG_RUNNING`]: the glyph and the
//! count redraw every second while it runs, so the screen never holds
//! still; the finished branch stood 8-11 s, then the panel went,
//! [`fixtures::END_OF_TURN_BG_FINISHED`]). A turn's end over a branch that
//! runs is the agent's own background work ([`background_wait`]): busy as
//! evidence, so nothing is typed into a session Muse Code itself will wake —
//! the backgrounded shell's end drew `◆ Finished · <description> · ✓` and
//! Muse Code took a turn of its own on its output. As Claude Code's reader
//! reads its own (`phase::worker_phase`): a backgrounded shell is the HARD
//! rule, busy over a question the turn ended on too, as a shell Claude Code
//! left running is; a monitor is the SOFT one ([`crate::phase::Busy::soft`]),
//! which a question outranks. A limit and a wall the turn ended on stand
//! over either (limited, idle on its wall). The composer and the reply's
//! last words read over it as before. A panel whose work is over reads
//! none. Before, such a turn's end read plainly idle: a point the turn-end
//! policy continued, its `keep going` typed while the work ran.
//!
//! **THE QUESTION** (`request_user_input`, 1.4.10) is drawn ABOVE an open
//! composer:
//!
//! ```text
//! ◈ Request user input Color — running (1s)      its head (the glyph cycles)
//!
//!   Which color should the probe use?            the question
//!
//!   ⟩ 1. Red (Recommended)  Warm and loud.       the model's options, each
//!     2. Blue               Cool and calm.         with its description
//!     3. None of the above  Optionally, add details in notes (tab).
//!
//!   Enter to select · ↑/↓ to move · Tab for an optional note · Esc to interrupt
//! ```
//!
//! Down moves the `⟩`, and Enter answers with the focused option
//! (`selected_label`). A DIGIT ONLY MOVES THE `⟩` there (measured: `2` and
//! `3` answered nothing), on several questions and a multi-select too.
//! Enter on `None of the above`, which Muse Code adds, answers at once with
//! the free text `None of the above` and opens no text field.
//! `(Recommended)` is the model's own suffix: the tool tells the model to
//! put its recommended option first and mark it so, and Muse Code draws the
//! label as given. The answer leaves `  Structured user input answered —
//! Color: Blue` in the transcript.
//!
//! **THE QUESTION'S NOTE** (Tab, measured): `      Note (optional): ▌` opens
//! six columns in under the focused option; what is typed stands there
//! (wrapped between words onto rows six in), follows the focus when it
//! moves, and goes with the answer the next Enter gives: Enter on `None of
//! the above` holding a note went to the REVIEW (below), and its answer
//! reached the model as `{"free_text": "None of the above", "note":
//! "<text>"}` — the harness's whole 276-byte policy text
//! ([`fixtures::QUESTION_NOTE_LONG_ANSWERED`]). Esc in the field, or Tab,
//! CLOSES it and CLEARS the text (Tab reopened it empty), as Codex's notes
//! field does; a second Esc stops the turn. [`question_notes`] reads it.
//! 1.4.9 draws it the same ([`fixtures::BOX_QUESTION_NOTE`]).
//!
//! **SEVERAL QUESTIONS AND A MULTI-SELECT** (measured): several questions
//! are drawn one at a time, `1 of 2` over the key row, which names `←/→ to
//! switch question` ([`fixtures::QUESTION_MULTI`]; 1.4.9's the same,
//! [`fixtures::BOX_QUESTION_1_OF_2`]); Enter answers one and draws the
//! next, and Left goes back to an answered one with no mark of its answer.
//! A multi-select draws `[ ]` boxes, `None of the above` among them, and
//! `5. Submit answer (0 checked)` under them over the key row `Enter to
//! toggle · Submit row to continue · …` ([`fixtures::QUESTION_MULTISELECT`]).
//! Both are read whole as questions WITH NO ROLES, a person's to answer:
//! their answers span screens (the questions, then the review) the loop
//! cannot tell its own keys' from a person's on, and the review's second
//! row stops the turn.
//!
//! **THE REVIEW**: the answers of several questions, of a multi-select and
//! of a question holding a note are shown before they go — `Review answers
//! before submit · Enter to edit or submit · ↑/↓ to move · Esc to go back`
//! (two rows at 60 columns), each answer four columns in (`Color: None of
//! the above; note: …`, wrapped between words onto rows four in), then `⟩
//! Submit answers` and `Interrupt turn`, unnumbered
//! ([`fixtures::QUESTION_MULTI_REVIEW`]; 1.4.9's the same,
//! [`fixtures::BOX_QUESTION_REVIEW`]). [`is_review`] names it; no option
//! of it carries a role, and its Esc goes back. Enter on an answer EDITS
//! it: the cursor moved onto one (`  ⟩ ` before it, not measured) leaves
//! the review read, its cursor on no option. Before these were read,
//! several questions and the review read as no box at all.
//!
//! In a NARROW TAB the question is drawn otherwise (measured at 72 and 60
//! columns). The key row (77 columns) wraps between words onto two rows,
//! `… Esc to` / `interrupt` at 72 and `… note ·` / `Esc to interrupt` at 60.
//! At 72 the descriptions keep a column of their own beside the labels,
//! wrapped in it; at 60 each description goes under its label, indented
//! seven, `None of the above`'s too ([`fixtures::QUESTION_WRAPPED`],
//! [`fixtures::QUESTION_WRAPPED_72`]). Before the reader read both, the
//! question at either width read as no question at all: the harness raised
//! nothing and answered nothing.
//!
//! **THE AWAY RECAP** (1.4.10, `away_recap_enabled`, on by default): a
//! message typed two minutes after a turn's end drew `◆ Recap: <summary>`
//! (the model's, asked in a side call) under the last reply, at once, while
//! the message was still in the composer; the message then went under it
//! ([`fixtures::END_OF_TURN_FINISHED`]). An answer keyed into a question that
//! had waited five minutes drew one too, under the reply and under `◆ Worked
//! for 5m 27s · 2:29 PM · 16/Oct`, the time that turn took
//! ([`fixtures::END_OF_TURN_WORKED_RECAP`]). Both are Muse Code's own rows
//! and no part of a turn: the turn ended on the block above them, as it does
//! above the binary's notices of background work (`◆ Finished <name> ·
//! 12s`, `◆ Message from …`). A reply too tall for the screen, its head
//! scrolled away, is read by its rows in sight. A message
//! typed 4 min 50 s after a turn's end, five minutes after a recap, drew
//! none, and so did a continuation the harness typed two minutes after one.
//! The harness's continuation typed 2 min 43 s after a usage wall drew one
//! above its message, and the since-removed `aterm drive task`'s nudge 2 min 14 s after a
//! turn's end drew one too; both were submitted. The recap's side call
//! (`x-tbh-agent-role: away-recap`) goes out with the first key.
//!
//! **AN INTERRUPT** (Esc in a turn). On 1.4.9 the echo provider's turn was
//! RETRACTED: the message left the transcript and came back as the
//! composer's draft, with no marker. That screen is a person's draft, and
//! is read as one. On 1.4.10 a model's turn keeps the text it had received
//! (`◆ tick 1 tick 2 …`) and ends on `◆ Interrupted · Report issues with
//! /feedback` ([`interrupted`]). For a moment the footer then says
//! `interrupting run` at its right: the run is still stopping. While a turn
//! runs, its text is never drawn piece by piece; only the status row is.
//!
//! **THE WALLS** (1.4.10) are drawn as the turn's last `◆` row ([`wall`]):
//!
//! | wall | row |
//! |---|---|
//! | 429, 500, retried | `◆ model failed: API error 429: <message> (<type>) (after 3 provider attempts)` |
//! | 401, never retried | `◆ your API key from META_API_KEY was rejected — update or unset it` |
//! | usage | `◆ Usage limit reached · /upgrade (<url>) for increased limits, or wait for usage to reset at 1:06 PM` |
//!
//! While a 429 or a 500 is retried, the row under the status row says so:
//! `└ Meta Model API rate limited (HTTP 429) · retrying in 5s · attempt 2/3`
//! ([`retrying`]). The turn is running, and that row is no wall. The default
//! is 10 attempts, honouring `retry-after`. A refused key and the binary's
//! other lost logins (`Your saved Meta credentials are invalid. …`, the
//! Keychain refusing them) are a sign-in wall (`muse login`, in a shell);
//! the binary's `You've used your <window> quota (resets at <time>).` is a
//! usage wall; its faults only a person fixes (`A configuration problem
//! stopped this turn.`) are no evidence. The
//! usage wall's URL is read as text and opened by nothing here; its window
//! is not named, so it is read as the session window.
//!
//! **THE TRUST BOX** in a folder it has not seen: `⟩ 1  Trust and continue`
//! / `  2  Quit` over `Use Up/Down or 1/2, then Enter. Esc quits.` A typed
//! `y` answered it (measured on 1.4.9), so it is read as a prompt with no
//! composer, never as a place to type.
//! **SIGNED OUT** it draws `Log in with browser · Enter to choose` over `Set
//! an API key` — whose Enter opens a browser on the person's desktop. It is
//! a WALL ([`wall`], `WallKind::Auth`) and never a box: no option of it is
//! read, so no policy can press one. A person signs in with `muse login` in
//! a shell.
//!
//! **WHAT THE LAB CANNOT DRAW** (1.4.10), and why:
//!
//! * Anything that rides on Muse Code's own managed sandbox: macOS refuses
//!   that sandbox inside the lab's jail (`sandbox-exec: sandbox_apply:
//!   Operation not permitted`; every sandboxed shell call then fails, and
//!   `bash`'s `unix_socket_paths` answers `requires managed macOS sandboxing
//!   with proxy-only networking`). The Unix-socket box was drawn outside the
//!   jail instead (above), and so was the tool-action box, every capture of
//!   it with Muse Code's sandbox on. It was not tried inside the jail: the
//!   lab configures no MCP server for Muse Code (its `aterm-lab
//!   mcp-standin` serves Codex's and Gemini CLI's captures), so whether that
//!   box needs the sandbox is not known, and no lab verb redraws these
//!   captures. The binary's
//!   process subject (`ProcessSubject`, `Allow Cua to control process …`)
//!   is its computer-use agent's, which acts on the desktop, and was not
//!   drawn.
//! * A META ACCOUNT'S WALLS: `Model API requires a payment method on your
//!   account — …`, `Muse Code requires a payment method — subscribe at …`
//!   and `your Meta account isn't set up for Model API yet — …`, each ending
//!   `then log in again`. They follow an ACCOUNT login's key mint, which the
//!   lab never makes: `META_API_KEY` beats any login, `muse login` opens a
//!   browser on Meta's site and is refused there, and a real account is out
//!   of bounds. Read from the binary's strings as sign-in walls ([`wall`]),
//!   drawn as the turn's last `◆` row as the key walls are — their
//!   placement unmeasured.

use crate::anchors::anchor_text;
use crate::phase::{Phase, is_rule, leading_spaces, plain_rule_width};
use crate::prompt::{Cancel, CancelEffect, Opt, PromptKind, PromptV2, Role, Select};
use crate::text::blank;
use crate::turn::{CardPlace, LaunchCard};
use crate::wall::{ApiCause, Placement, Wall, WallKind};

/// The composer, read whole.
#[derive(Debug, Clone, Copy)]
struct Frame {
    top: usize,
    caret: usize,
    bottom: usize,
    /// The footer: the last row drawn, right under the bottom rule or under
    /// the background-work panel ([`bottom_and_footer`]).
    footer: usize,
}

/// The background-work panel's head, the first row under a bottom rule while
/// work runs in the background (measured on 1.4.10, a monitor and a
/// backgrounded shell): `main · ↓ to select`, or `main` alone while the
/// composer holds a draft, in column 0.
fn is_panel_head(row: &str) -> bool {
    let t = crate::text::trim_end(row);
    t == "main"
        || t.strip_prefix("main · ")
            .is_some_and(|rest| rest == const { anchor_text("muse.panel.select") })
}

/// A row of the background-work panel under its head: one piece of work as a
/// branch of the tree in column 0 (`└ ◆ Watch the lab socket  watching  6s`,
/// measured; `├ ` and `│ ` for the rows above the last, as a tree draws them).
fn is_panel_row(row: &str) -> bool {
    row.starts_with("└ ") || row.starts_with("├ ") || row.starts_with("│ ")
}

/// The bottom rule and the footer of the composer's frame, as `(bottom,
/// footer)`, read up from the last row drawn: the footer at column 2, and
/// right above it the plain bottom rule — or, while work runs in the
/// background, the panel between them ([`is_panel_head`], [`is_panel_row`]:
/// a head and at least one row). `None` for any other shape.
fn bottom_and_footer(rows: &[String]) -> Option<(usize, usize)> {
    let footer = rows.iter().rposition(|r| !blank(r))?;
    if leading_spaces(&rows[footer]) != 2 {
        return None;
    }
    let mut k = footer.checked_sub(1)?;
    if plain_rule_width(&rows[k]).is_some() {
        return Some((k, footer));
    }
    let last_branch = k;
    while is_panel_row(&rows[k]) {
        k = k.checked_sub(1)?;
    }
    if k == last_branch || !is_panel_head(&rows[k]) {
        return None;
    }
    let bottom = k.checked_sub(1)?;
    plain_rule_width(&rows[bottom]).map(|_| (bottom, footer))
}

/// A composer's frame ([`Frame`]): the last `❯` row in column 0, a rule
/// right above it (labelled or not), the first plain rule under it with no
/// other rule between, and the footer under that at column 2 as the last
/// row drawn — right under it, or under the background-work panel
/// ([`bottom_and_footer`]). A rule between is a box's frame under a
/// message's `❯` row ([`approval_box`]). Muse Code erases its composer when
/// it exits.
fn any_frame(rows: &[String]) -> Option<Frame> {
    let caret = rows.iter().rposition(|r| r.starts_with('❯'))?;
    let top = caret.checked_sub(1)?;
    if !is_rule(&rows[top]) {
        return None;
    }
    let bottom = (caret + 1..rows.len()).find(|&i| plain_rule_width(&rows[i]).is_some())?;
    if rows[caret + 1..bottom].iter().any(|r| is_rule(r)) {
        return None;
    }
    let (under, footer) = bottom_and_footer(rows)?;
    (under == bottom).then_some(Frame {
        top,
        caret,
        bottom,
        footer,
    })
}

/// The composer ([`any_frame`]) of this conversation: not the side chat's
/// (`Side Chat · …` on its top rule, its footer ending `Side chat mode`; the
/// binary's strings), another conversation's, where nothing is typed and
/// whose rows are no turn of this one.
fn frame(rows: &[String]) -> Option<Frame> {
    any_frame(rows).filter(|f| {
        !crate::text::contains(&rows[f.top], const { anchor_text("muse.sidechat.label") })
            && !crate::text::trim(&rows[f.footer])
                .ends_with(const { anchor_text("muse.sidechat.footer") })
    })
}

/// The composer's frame with nothing over it that takes its keys: no
/// question ([`question_box`]) — its key row names Enter, the arrows, Tab
/// and Esc as its own (text typed under it was not tried) — no review of
/// several answers ([`review_box`]), and no drawer or picker ([`drawer`]).
fn open_frame(rows: &[String]) -> Option<Frame> {
    frame(rows).filter(|&f| {
        question_box(rows, f).is_none() && review_box(rows, f).is_none() && !drawer(rows, f)
    })
}

/// A key-hint row of a drawer or a picker (the binary's `/model`, `/effort`,
/// `/settings`, `/export`, … draw them: `↑↓ move · enter confirm · esc
/// cancel`, `enter use - tab switch - esc close - type filter`, `… · esc
/// back`): indented, two hints or more split by ` · ` or ` - `, one of them
/// `esc` and a verb, lowercase. The question's and the review's key rows
/// say `Esc to …`, Claude Code's toasts `esc to cancel`; the composer's
/// footer names no Esc, and the status row is in column 0.
fn is_key_hint_row(row: &str) -> bool {
    if leading_spaces(row) == 0 {
        return false;
    }
    let t = crate::text::trim(row);
    let hints: Vec<&str> = t
        .split(if t.contains(" · ") { " · " } else { " - " })
        .collect();
    hints.len() >= 2
        && hints.iter().any(|h| {
            crate::text::trim(h)
                .strip_prefix("esc ")
                .is_some_and(|verb| !verb.is_empty() && !verb.contains(' '))
        })
}

/// Whether a drawer or a picker is up over the composer of frame `f`, by its
/// key-hint row ([`is_key_hint_row`]) as the last row over the top rule or
/// a row between the caret row and the bottom rule. Not measured: drawn, it
/// takes the composer's keys, so nothing is typed there; a person opened it.
fn drawer(rows: &[String], f: Frame) -> bool {
    (0..f.top)
        .rev()
        .find(|&i| !blank(&rows[i]))
        .is_some_and(|i| is_key_hint_row(&rows[i]))
        || rows[f.caret + 1..f.bottom]
            .iter()
            .any(|r| is_key_hint_row(r))
}

/// The composer's caret row: `None` while a box is up.
#[must_use]
pub fn composer(rows: &[String]) -> Option<usize> {
    open_frame(rows).map(|f| f.caret)
}

/// A row of the `/` command menu, drawn under the caret row while the draft
/// opens with `/` (`  /compact   Summarize …`, `  ↓ 43 more`, and `↑ N more`
/// over the rows once it scrolled).
fn is_menu_row(row: &str) -> bool {
    let t = crate::text::trim_start(row);
    leading_spaces(row) == 2 && (t.starts_with('/') || t.starts_with("↓ ") || t.starts_with("↑ "))
}

/// The composer's text as `(caret row, lines)`: the caret row's text first
/// (its `❯` stripped — empty when nothing is typed), then each row down to
/// the bottom rule, the command menu's rows aside. `None` while a box is up.
/// The hint 1.4.10 draws there signed in (`❯ /export saves the conversation
/// to a text file`, measured, [`fixtures::END_OF_TURN_FOOTNOTES_HINT`]) is
/// read as text: its colour, not its words, tells it from a draft
/// ([`crate::reader::ScreenReader::placeholder_fg`]).
#[must_use]
pub fn composer_draft(rows: &[String]) -> Option<(usize, Vec<String>)> {
    let f = open_frame(rows)?;
    let caret = rows[f.caret].strip_prefix('❯').unwrap_or(&rows[f.caret]);
    let caret = crate::text::trim(caret);
    let menu = caret.starts_with('/');
    let mut lines = vec![caret.to_string()];
    lines.extend(
        rows[f.caret + 1..f.bottom]
            .iter()
            .filter(|r| !(menu && is_menu_row(r)))
            .map(|r| crate::text::trim(r).to_string()),
    );
    Some((f.caret, lines))
}

/// The diamonds the status row cycles through in column 0 (measured live: ◇,
/// ◈ and ◆ over 381 status frames of three turns). The question's head
/// cycles them too.
const STATUS_GLYPHS: [char; 3] = ['◇', '◈', '◆'];

/// The status row while a turn runs: `◆ Thinking (2s · esc to interrupt)` —
/// one of [`STATUS_GLYPHS`] in column 0 (the verb `Working`, then
/// `Thinking`; `Calling tools` over a box, `Interrupting` after a ctrl+c)
/// and the Esc hint's parenthesis ending the row.
fn is_status_row(row: &str) -> bool {
    let hint = const { anchor_text("muse.busy.interrupt") };
    row.starts_with(STATUS_GLYPHS)
        && crate::text::trim_end(row)
            .strip_suffix(')')
            .is_some_and(|r| r.ends_with(hint))
}

/// A reply's head: `◆ <text>` in column 0 — no status row, and no
/// question's head, which cycles the same glyphs while the question waits.
fn is_reply_head(row: &str) -> bool {
    row.starts_with("◆ ") && !is_status_row(row) && !is_question_head(row)
}

/// Muse Code's banner naming its build: `  Muse Code 1.4.9`.
fn is_banner_row(row: &str) -> bool {
    crate::text::trim(row)
        .strip_prefix(const { anchor_text("muse.banner") })
        .and_then(|v| v.strip_prefix(' '))
        .is_some_and(|v| v.starts_with(|c: char| c.is_ascii_digit()))
}

/// A SESSION's banner ([`is_banner_row`]) on row `i`: the row over it,
/// blanks aside, is the shell's — in column 0, no glyph of Muse Code's, and
/// no `❯` but a prompt launching it (starship's, pure's) — or there is none.
/// Every measured banner has one of those over it; a reply quoting the line
/// has its own rows over it.
fn is_session_banner(rows: &[String], i: usize) -> bool {
    is_banner_row(&rows[i])
        && (0..i).rev().find(|&k| !blank(&rows[k])).is_none_or(|k| {
            let r = &rows[k];
            leading_spaces(r) == 0
                && !r.starts_with(STATUS_GLYPHS)
                && r.strip_prefix('❯').is_none_or(|cmd| {
                    cmd.split_whitespace()
                        .next()
                        .and_then(crate::reader::program_of)
                        == Some(crate::reader::Program::Muse)
                })
        })
}

/// This session's rows over the composer's top rule `top`, as `(its banner,
/// its last message, the row its last turn's rows start on)`. A session
/// opens on its banner ([`is_session_banner`]): the rows above the last one
/// are an earlier session's in this tab (measured: Muse Code run again after
/// it exited, [`fixtures::IDLE_RELAUNCHED`]).
fn scope(rows: &[String], top: usize) -> (Option<usize>, Option<usize>, usize) {
    let banner = (0..top).rev().find(|&i| is_session_banner(rows, i));
    let start = banner.map_or(0, |b| b + 1);
    let user = (start..top).rev().find(|&i| rows[i].starts_with('❯'));
    (banner, user, user.map_or(start, |u| u + 1))
}

/// A row of a card `/usage` or `/status` draws into the transcript (`┌──`,
/// `│  Session usage  │`, `└──┘`, all in column 0). A tool's output row
/// (`└ approval aborted`) is no card row: its `└` is followed by a blank.
fn is_card_row(row: &str) -> bool {
    row.starts_with('┌') || row.starts_with('│') || row.starts_with("└─")
}

/// The state of the session's run the NEWEST `/status` card among `rows`
/// names at its head's right: `│  MUSE CODE 1.4.10 / sea-pasiphae  RUNNING │`
/// (the session's name after the build once it has one). Measured on
/// 1.4.10 (the lab, 2026-10-16 and 2026-10-21): `IDLE` before any turn
/// ([`fixtures::FRESH_STATUS_CARDS`]), `COMPLETED` after a turn's end
/// ([`fixtures::STATUS_CARD`], [`fixtures::COMPLETED_STATUS_CARDS`]),
/// `RUNNING` for a card entered while a turn ran
/// ([`fixtures::STATUS_MID_TURN`]), `FAILED` after the usage wall and after
/// a 429 its retries did not get past ([`fixtures::USAGE_WALL_STATUS_CARDS`],
/// [`fixtures::LIMIT_UNNAMED_STATUS_CARDS`]), and `CANCELLED` after a
/// person's Esc ([`fixtures::INTERRUPTED_STATUS_CARDS`]). The words are put
/// together at run time (the binary holds none in capitals). `None` with no
/// readable state on the newest head, or no `/status` head among `rows`
/// (a `/usage` card names no state).
fn card_state(rows: &[String]) -> Option<&str> {
    status_head(rows).and_then(|(_, state)| state)
}

/// A card's row, `│  <text>  │`, as its text trimmed: `None` for any other row.
fn card_text(row: &str) -> Option<&str> {
    let inner = crate::text::trim_end(row)
        .strip_prefix('│')?
        .strip_suffix('│')?;
    Some(crate::text::trim(inner))
}

/// The NEWEST `/status` card's head among `rows`, as `(its row, its readable
/// state)` ([`card_state`]). A recognizable head whose right border or state
/// is unreadable still takes precedence over every older card.
fn status_head(rows: &[String]) -> Option<(usize, Option<&str>)> {
    let (i, row) = rows.iter().enumerate().rev().find(|(_, row)| {
        row.strip_prefix('│')
            .map(crate::text::trim_start)
            .is_some_and(|text| text.starts_with(const { anchor_text("muse.card.status") }))
    })?;
    let state = card_text(row)
        .and_then(|text| text.strip_prefix(const { anchor_text("muse.card.status") }))
        .and_then(|text| text.rsplit_once(' ').map(|(_, state)| state))
        .filter(|state| !state.is_empty() && state.bytes().all(|b| b.is_ascii_uppercase()));
    Some((i, state))
}

/// Whether the NEWEST `/status` card among `rows` counts no token spent: its
/// `USAGE` row `0 tokens · 0 turns · 0 subagents`. Measured on 1.4.10: so
/// before any turn ([`fixtures::FRESH_STATUS_CARDS`]), and after a first
/// turn a person stopped ([`fixtures::INTERRUPTED_STATUS_CARDS`]); the count
/// is the session's, cumulative, and every turn the model answered adds to
/// it ([`fixtures::COMPLETED_STATUS_CARDS`]: `67,077 tokens`).
fn status_spent_none(rows: &[String]) -> bool {
    status_head(rows).is_some_and(|(head, _)| {
        rows[head + 1..]
            .iter()
            .take_while(|r| r.starts_with('│'))
            .filter_map(|r| card_text(r))
            .find_map(|t| t.strip_prefix(const { anchor_text("muse.card.usage") }))
            .and_then(|spent| crate::text::trim(spent).split_once(' '))
            .is_some_and(|(count, unit)| {
                count == "0" && unit.starts_with(const { anchor_text("muse.card.tokens") })
            })
    })
}

/// The turns the NEWEST `/usage` card among `rows` counts, `│    Turns    1 │`.
/// Measured on 1.4.10: `0` before any turn ([`fixtures::FRESH_STATUS_CARDS`])
/// — and after a first turn a person stopped, as the `/status` card's own
/// count says there ([`fixtures::INTERRUPTED_STATUS_CARDS`]) — and `1` after
/// one ([`fixtures::USAGE_CARD`]). `None` with no such row among `rows`.
fn usage_turns(rows: &[String]) -> Option<u64> {
    rows.iter().rev().find_map(|r| {
        let count = card_text(r)?.strip_prefix(const { anchor_text("muse.card.turns") })?;
        let count = count.strip_prefix(' ').map(crate::text::trim)?;
        (!count.is_empty() && count.bytes().all(|b| b.is_ascii_digit()))
            .then(|| count.parse().ok())
            .flatten()
    })
}

/// What CARDS ALONE over the open composer say of the run the session rests
/// on ([`Turn::Carded`]): the newest `/status` card's state ([`card_state`]),
/// or, with none in sight, a `/usage` card's count of a turn taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rest {
    /// `COMPLETED`: the last turn ended as a turn does.
    Completed,
    /// `FAILED`: the last turn ended on a wall (the usage wall, a 429 its
    /// retries did not get past), its notice out of sight.
    Failed,
    /// `CANCELLED`: a person stopped the last turn with Esc
    /// ([`interrupted`]).
    Cancelled,
    /// No `/status` card in sight, and the newest `/usage` card counts a
    /// turn or more: a turn was taken, and how it ended is out of sight.
    Unnamed,
}

/// The turn cards alone over the composer read as (`rows` the session's rows
/// above it, no message, banner, reply or status row among them): a session
/// at rest on the run the newest `/status` card names ([`Rest`]); one with
/// no turn yet where it says `IDLE` and counts no token spent
/// ([`status_spent_none`]: both measured so, its banner scrolled away under
/// them, [`fixtures::FRESH_STATUS_CARDS`]; an `IDLE` over a count of tokens
/// spent, which nothing measured draws, is no evidence); and, with no `/status`
/// card in sight, at rest where a `/usage` card counts a turn taken. NO
/// EVIDENCE (`Unknown`) where the newest `/status` card's state is unreadable,
/// says `RUNNING` with no status row under it (nothing measured draws it), or
/// names a state nothing measured; or none is in sight and no `/usage` card counts a
/// turn: `0` is a session with no turn yet and one whose first turn a
/// person stopped alike. Read as at rest, a fresh session's cards owed a
/// continuation nobody's task asked for.
fn carded(rows: &[String]) -> Turn {
    match card_state(rows) {
        Some(s) if s == const { anchor_text("muse.card.idle") } && status_spent_none(rows) => {
            Turn::Fresh
        }
        Some(s) if s == const { anchor_text("muse.card.completed") } => {
            Turn::Carded(Rest::Completed)
        }
        Some(s) if s == const { anchor_text("muse.card.failed") } => Turn::Carded(Rest::Failed),
        Some(s) if s == const { anchor_text("muse.card.cancelled") } => {
            Turn::Carded(Rest::Cancelled)
        }
        // `RUNNING` with no status row under the cards (nothing measured
        // draws it), or a state nothing measured.
        Some(_) => Turn::Unknown,
        None if status_head(rows).is_none() && usage_turns(rows).is_some_and(|n| n > 0) => {
            Turn::Carded(Rest::Unnamed)
        }
        None => Turn::Unknown,
    }
}

/// Muse Code's pointer to the background-work panel on row `i`: `  (↓ to
/// view terminals)` (or `monitors`), two columns in, a block of its own
/// under a blank row. Measured on 1.4.10: drawn once into the transcript
/// when a session's first shell (or monitor) goes to the background, a
/// moment after the reply of the turn that sent it there, so it can be the
/// last thing drawn above the composer ([`fixtures::END_OF_TURN_WORK_HINT`]).
/// It is no part of that reply (a paragraph of a reply reading exactly so
/// would be taken for it; none measured does).
fn is_view_hint(rows: &[String], i: usize) -> bool {
    leading_spaces(&rows[i]) == 2
        && i.checked_sub(1).is_some_and(|above| blank(&rows[above]))
        && crate::text::trim(&rows[i])
            .strip_prefix(const { anchor_text("muse.panel.view") })
            .and_then(|kind| kind.strip_suffix(')'))
            .is_some_and(|kind| !kind.is_empty() && kind.chars().all(|c| c.is_ascii_lowercase()))
}

/// Whether row `i` is of Muse Code's ABNORMAL-END WARNING, drawn as a
/// resumed session opens after a run that recorded no clean shutdown (a
/// SIGHUP, a SIGKILL; measured on 1.4.10, the harness's relaunch on exit,
/// [`fixtures::RESUMED_AFTER_CRASH`]): `  warning: the previous run of this
/// session ended abnormally around <time> (no clean shutdown was recorded).
/// It was resumed cleanly.`, two columns in, a block of its own under a
/// blank row, its wrapped rows indented two. It is no part of the reply
/// above it: read as one, the reply's last words were the warning's, and a
/// done reply under it no longer ended on the worker's token.
fn in_abnormal_end(rows: &[String], i: usize) -> bool {
    let start = (0..=i)
        .rev()
        .take_while(|&j| !blank(&rows[j]) && leading_spaces(&rows[j]) == 2)
        .last();
    start.is_some_and(|s| {
        s.checked_sub(1).is_some_and(|above| blank(&rows[above]))
            && crate::text::trim(&rows[s])
                .strip_prefix("warning: ")
                .is_some_and(|w| w.starts_with(const { anchor_text("muse.resume.abnormal") }))
    })
}

/// Where the last turn stands, read above the composer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Turn {
    /// The status row is up.
    Running,
    /// A message with nothing under it yet: sent, its status row not drawn.
    Sent,
    /// The block whose head is on `.0` (a reply, or Muse Code's own row) is
    /// the last thing drawn, its last row `.1`.
    Ended(usize, usize),
    /// A reply taller than the rows read: its head and its message scrolled
    /// away, its rows from `.0` down to `.1`, all indented, the last thing
    /// drawn.
    Tail(usize, usize),
    /// THE TURN'S END SCROLLED AWAY UNDER CARDS (`/usage`, `/status`): no
    /// message, banner, reply or status row in sight above the open
    /// composer, only cards (measured on 1.4.10, the lab of 2026-10-21:
    /// `/status` entered three times under the usage wall pushed the notice
    /// out of sight, [`fixtures::USAGE_WALL_STATUS_CARDS`]). The session is
    /// at rest: a card entered while a turn runs is drawn ABOVE the turn's
    /// status row, which stays over the composer, and the turn's text goes
    /// on under the card ([`fixtures::STATUS_MID_TURN`],
    /// [`fixtures::STATUS_MID_TURN_ENDED`]); a message sent since would be
    /// in sight under the cards. The run it rests on is the newest card's
    /// ([`carded`]): not where that card says `RUNNING` with no status row
    /// under it, which nothing measured draws, nor `IDLE` (no turn yet).
    Carded(Rest),
    /// No turn yet: the banner and no message — or cards alone whose newest
    /// `/status` card says `IDLE`, the banner scrolled away under them
    /// ([`carded`]).
    Fresh,
    /// Nothing measured says: rows this grammar does not know under the
    /// last message, or its head scrolled away with no reply in sight.
    Unknown,
}

fn turn(rows: &[String], top: usize) -> Turn {
    let (banner, user, from) = scope(rows, top);
    if rows[from..top].iter().any(|r| is_status_row(r)) {
        return Turn::Running;
    }
    if user.is_none() && banner.is_some() {
        return Turn::Fresh;
    }
    // The cards a command drew under the turn's end are no part of it, nor
    // the pointer to the background-work panel drawn under its reply, nor a
    // resumed session's abnormal-end warning.
    let drawn = |i: usize| {
        !blank(&rows[i])
            && !is_card_row(&rows[i])
            && !is_view_hint(rows, i)
            && !in_abnormal_end(rows, i)
    };
    let Some(last) = (from..top).rev().find(|&i| drawn(i)) else {
        let cards = rows[from..top].iter().any(|r| is_card_row(r));
        return match (user, cards) {
            (Some(_), false) => Turn::Sent,
            (None, true) => carded(&rows[from..top]),
            _ => Turn::Unknown,
        };
    };
    // The reply is the last block: its head the last row in column 0, every
    // row under it indented. An away recap or a turn's time drawn under it
    // is Muse Code's own row, no part of the turn: the block above it is the
    // turn's end.
    let mut last = last;
    loop {
        match (from..=last)
            .rev()
            .find(|&i| leading_spaces(&rows[i]) == 0 && !blank(&rows[i]))
        {
            Some(head) if is_own_row_head(&rows[head]) => {
                let Some(above) = (from..head).rev().find(|&i| drawn(i)) else {
                    return Turn::Unknown;
                };
                last = above;
            }
            Some(head) if is_reply_head(&rows[head]) => return Turn::Ended(head, last),
            // No message, banner or head in sight, and every row indented:
            // the rows of a reply too tall for the screen.
            None if user.is_none()
                && rows[from..=last]
                    .iter()
                    .all(|r| blank(r) || (leading_spaces(r) >= 2 && !is_banner_row(r))) =>
            {
                return Turn::Tail(from, last);
            }
            _ => return Turn::Unknown,
        }
    }
}

/// One of Muse Code's OWN rows under a turn's end (measured on 1.4.10): the
/// away recap, `◆ Recap: <summary>` (`away_recap_enabled`, on by default; a
/// key typed after a pause drew it at once, its summary the model's, asked
/// in a side call), and the time a long turn took, `◆ Worked for 5m 27s ·
/// 2:29 PM · 16/Oct` (drawn under the reply of a turn whose question waited
/// five minutes) — and the binary's notices of its background work, `◆
/// Finished <name> · 12s` and `◆ Message from …`. None says anything of the
/// turn's end. A reply that begins `Worked for` or `Finished` with no time
/// after it is a reply.
fn is_own_row_head(row: &str) -> bool {
    row.strip_prefix("◆ ").is_some_and(|r| {
        r.starts_with(const { anchor_text("muse.recap") })
            || r.starts_with(const { anchor_text("muse.notice.message") })
            || r.strip_prefix(const { anchor_text("muse.worked") })
                .and_then(|t| t.split_once(" · "))
                .is_some_and(|(took, _)| is_elapsed(took))
            || r.strip_prefix(const { anchor_text("muse.notice.finished") })
                .and_then(|t| t.rsplit_once(" · "))
                .is_some_and(|(_, took)| is_elapsed(took))
    })
}

/// `12s`, `350ms`, `5m 27s`, `2h 5m`: a time taken as Muse Code prints one.
fn is_elapsed(t: &str) -> bool {
    let t = crate::text::trim(t);
    !t.is_empty()
        && t.split(' ').all(|w| {
            let d = w.bytes().take_while(u8::is_ascii_digit).count();
            d > 0 && matches!(&w[d..], "ms" | "s" | "m" | "h")
        })
}

/// The block the last turn ended on, as `(its head row, its rows)`: the head
/// less its `◆`, then each row under it, trimmed, blank rows left out — of a
/// reply too tall for the screen ([`Turn::Tail`]), its rows in sight. `None`
/// while a box is up or a turn runs, before any turn, and where [`phase`]
/// cannot say the turn ended.
fn ended(rows: &[String]) -> Option<(usize, Vec<&str>)> {
    let (head, last) = ended_rows(rows)?;
    Some((head, block_lines(rows, head, last)))
}

/// The rows of the block [`ended`] reads, `(its head, its last row)`.
fn ended_rows(rows: &[String]) -> Option<(usize, usize)> {
    let f = open_frame(rows)?;
    let (Turn::Ended(head, last) | Turn::Tail(head, last)) = turn(rows, f.top) else {
        return None;
    };
    Some((head, last))
}

/// Rows `head..=last` of a block: the head less its `◆`, then each row under
/// it, trimmed, blank rows left out.
fn block_lines(rows: &[String], head: usize, last: usize) -> Vec<&str> {
    let first = rows[head].strip_prefix('◆').unwrap_or(&rows[head]);
    std::iter::once(crate::text::trim(first))
        .chain(rows[head + 1..=last].iter().map(|r| crate::text::trim(r)))
        .filter(|t| !t.is_empty())
        .collect()
}

/// THE NOTICE the last turn ended on, as one line: the first paragraph of
/// the block [`ended`] reads — its head less its `◆` and the rows under it
/// up to the first blank row, trimmed and joined with one space. Muse
/// Code's own notices (a wall, a fault, the interrupt) are one line each,
/// wrapped onto the rows under their head, two in, with no blank between
/// (measured on 1.4.10: the usage wall's time on `  AM` under it,
/// [`fixtures::USAGE_WALL`]). What a local command prints under one is a
/// block of its own under a blank row, no part of it: `/rules` entered at
/// the empty composer under the usage wall printed its three rows so
/// (measured in the W2 lab, 2026-10-21, [`fixtures::USAGE_WALL_RULES_UNDER`]).
/// Read as the whole block, they were folded into the wall's message, and
/// its reset named no time.
fn notice(rows: &[String]) -> Option<(usize, String)> {
    let (head, last) = ended_rows(rows)?;
    // A reply too tall for the screen ([`Turn::Tail`]) may open on blank
    // rows: its paragraph in sight starts at the first row that is not.
    let start = (head..=last).find(|&i| !blank(&rows[i]))?;
    let end = (start + 1..=last)
        .find(|&i| blank(&rows[i]))
        .map_or(last, |b| b - 1);
    Some((head, block_lines(rows, start, end).join(" ")))
}

/// The footer says the run is still stopping: `interrupting run` at its
/// right, a moment after an Esc (measured on 1.4.10).
fn interrupting(rows: &[String], f: Frame) -> bool {
    rows.get(f.footer).is_some_and(|r| {
        crate::text::trim_end(r)
            .strip_suffix(const { anchor_text("muse.footer.interrupting") })
            .is_some_and(|left| left.ends_with("  "))
    })
}

/// What [`background_wait`] names: a branch of the background-work panel
/// whose state is `running` (a backgrounded shell) or `watching` (a
/// monitor).
const WORK_RUNNING: &str = "background work running";
const MONITOR_WATCHING: &str = "a monitor watching";

/// The work one branch of the background-work panel says runs (module
/// header): `└ ◈ Wait in the background  running  13s` (a backgrounded
/// shell), `└ ◇ Watch the lab socket  watching  14s` (a monitor) — the
/// branch, a status diamond, the description, its state and its time, each
/// two blanks or more apart. A branch that finished (`ran 1s, exit 0`,
/// `failed`), and a row of any other shape, says none.
fn panel_work(row: &str) -> Option<&'static str> {
    let item = row
        .strip_prefix("└ ")
        .or_else(|| row.strip_prefix("├ "))?
        .strip_prefix(STATUS_GLYPHS)?
        .strip_prefix(' ')?;
    let cells: Vec<&str> = item
        .split("  ")
        .map(crate::text::trim)
        .filter(|c| !c.is_empty())
        .collect();
    let [_, .., state, took] = cells.as_slice() else {
        return None;
    };
    if !is_elapsed(took) {
        return None;
    }
    if *state == const { anchor_text("muse.panel.running") } {
        Some(WORK_RUNNING)
    } else if *state == const { anchor_text("muse.panel.watching") } {
        Some(MONITOR_WATCHING)
    } else {
        None
    }
}

/// The work the panel under the open composer's bottom rule says runs
/// ([`panel_work`] over its branches): a backgrounded shell's
/// ([`WORK_RUNNING`], the hard rule) over a monitor's, as Claude Code's
/// reader names its hard rules first (`phase::busy_signal`).
fn work(rows: &[String]) -> Option<&'static str> {
    let f = open_frame(rows)?;
    let branches = &rows[f.bottom + 1..f.footer];
    let named = |what: &'static str| {
        branches
            .iter()
            .any(|r| panel_work(r) == Some(what))
            .then_some(what)
    };
    named(WORK_RUNNING).or_else(|| named(MONITOR_WATCHING))
}

/// THE AGENT'S OWN BACKGROUND WORK RUNS AND ITS TURN IS OVER (module header):
/// the open composer, the turn over it ended — idle (on a wall too), a
/// question or a limit, as evidence ([`turn_phase`]) — and a branch of the
/// panel under it running or watching. `Some(what runs)`; `None` for a
/// running turn (its status row over the panel), a box, a turn whose end is
/// no evidence, a panel whose work finished (`ran 1s, exit 0`, `failed`),
/// and no panel.
#[must_use]
pub fn background_wait(rows: &[String]) -> Option<&'static str> {
    let what = work(rows)?;
    matches!(
        turn_phase(rows),
        (Phase::Idle | Phase::Question | Phase::Limited { .. }, true)
    )
    .then_some(what)
}

/// The phase, and whether it is evidence (module header): the trust box, an
/// approval box, the question, the review of its answers and a drawer are a
/// prompt; the status row is busy; a reply as the last thing drawn above the
/// composer (a card under it or not; its rows in sight, of one taller than
/// the screen) is idle — a question when its last words end in `?`, limited
/// on a rate limit or a usage wall — and so is a session with no turn yet
/// (its banner, or cards alone whose newest `/status` card says `IDLE`),
/// and cards alone over the composer, the turn's end scrolled away under
/// them, where the newest card says how the run ended ([`Turn::Carded`]: no
/// words read); the sign-in wall is idle on its wall. A message with
/// nothing under it,
/// and a turn's end whose footer still says `interrupting run`, are busy NOT
/// authoritatively; a turn stopped on a fault only a person can fix
/// ([`faulted`]), and any other screen, are `idle` NOT authoritatively. No
/// screen with a box on it reads idle. An ended turn that would read idle,
/// with no wall, while the panel says the agent's own background work runs
/// ([`background_wait`]) is BUSY as evidence, as Claude Code's reader reads
/// its background work (`phase::worker_phase`): a backgrounded shell over a
/// question the turn ended on too (the hard rule, a shell's), a monitor not
/// (the soft rule: the question stands); a limit and a wall stand over
/// either.
#[must_use]
pub fn phase(rows: &[String]) -> (Phase, bool) {
    let read = turn_phase(rows);
    let Some(what) = work(rows) else {
        return read;
    };
    let held = match read {
        (Phase::Idle, true) => true,
        (Phase::Question, true) => what == WORK_RUNNING,
        _ => false,
    };
    if held && wall(rows).is_none() {
        return (Phase::Busy, true);
    }
    read
}

/// [`phase`], the agent's background work aside.
fn turn_phase(rows: &[String]) -> (Phase, bool) {
    if trust_box(rows).is_some() || approval_box(rows).is_some() {
        return (Phase::Prompt, true);
    }
    let Some(f) = frame(rows) else {
        return (Phase::Idle, signed_out(rows).is_some());
    };
    if question_box(rows, f).is_some() || review_box(rows, f).is_some() || drawer(rows, f) {
        return (Phase::Prompt, true);
    }
    match turn(rows, f.top) {
        Turn::Running => (Phase::Busy, true),
        Turn::Sent => (Phase::Busy, false),
        Turn::Fresh => (Phase::Idle, true),
        Turn::Unknown => (Phase::Idle, false),
        Turn::Ended(..) | Turn::Tail(..) | Turn::Carded(_) if interrupting(rows, f) => {
            (Phase::Busy, false)
        }
        // At rest under its cards, its last words out of sight: idle as
        // evidence, with none read (no wall, no question, no fault).
        Turn::Carded(_) => (Phase::Idle, true),
        Turn::Ended(..) | Turn::Tail(..) if faulted(rows) => (Phase::Idle, false),
        Turn::Ended(..) | Turn::Tail(..) => {
            if let Some(w) = wall(rows).filter(|w| w.kind.reads_limited()) {
                return (
                    Phase::Limited {
                        message: w.message,
                        reset: w.reset,
                    },
                    true,
                );
            }
            let asks = said_tail(rows).is_some_and(|t| crate::turn::asks(&t));
            (if asks { Phase::Question } else { Phase::Idle }, true)
        }
    }
}

/// No turn yet: the banner above the composer and no message, or cards alone
/// whose newest `/status` card says `IDLE` ([`carded`]).
#[must_use]
pub fn fresh(rows: &[String]) -> bool {
    open_frame(rows).is_some_and(|f| turn(rows, f.top) == Turn::Fresh)
}

/// A MESSAGE STANDS ON `rows` ([`crate::ScreenReader::message_drawn`]): this
/// session's last person's message (`❯` in column 0 under its banner,
/// [`scope`]) above the composer's top rule (anywhere where no composer is
/// drawn), answered or not.
#[must_use]
pub fn message_drawn(rows: &[String]) -> bool {
    let top = frame(rows).map_or(rows.len(), |f| f.top);
    scope(rows, top).1.is_some()
}

/// The run cards alone over the open composer say the session rests on
/// ([`Turn::Carded`]); `None` on any other screen.
fn rests_on(rows: &[String]) -> Option<Rest> {
    match open_frame(rows).map(|f| turn(rows, f.top)) {
        Some(Turn::Carded(rest)) => Some(rest),
        _ => None,
    }
}

/// THE SESSION RESTS ON A TURN'S END ITS CARDS HIDE: cards alone over the
/// open composer, idle as evidence ([`phase`]: no `interrupting run`, no
/// work of its own running in the panel), whose newest `/status` card says
/// the run `FAILED`, or, with none in sight, whose `/usage` card counts a
/// turn taken ([`Rest::Failed`], [`Rest::Unnamed`]). Nothing in sight says
/// a turn went on past that end, or that a person stopped it: the wall the
/// supervisor last read there may be the end still (measured: three
/// `/status` under the usage wall, and under a 429 its retries did not get
/// past, say `FAILED`, [`fixtures::USAGE_WALL_STATUS_CARDS`],
/// [`fixtures::LIMIT_UNNAMED_STATUS_CARDS`]). Not where it says `COMPLETED`
/// (a turn ended as a turn does since) or `CANCELLED` (a person's Esc).
#[must_use]
pub fn end_hidden(rows: &[String]) -> bool {
    phase(rows) == (Phase::Idle, true)
        && matches!(rests_on(rows), Some(Rest::Failed | Rest::Unnamed))
}

/// The heads of Muse Code's own tool rows, which are no words of the agent
/// (measured: `Ran command · <description> · ✓ · 2.3s · ctrl+o`, `Ran touch
/// probe.txt — denied`, `Wrote .agents/probe-write.md (+1)`, `Edited
/// .agents/probe-write.md — denied`).
const TOOL_HEADS: [&str; 3] = [
    anchor_text("muse.tool.ran"),
    anchor_text("muse.tool.wrote"),
    anchor_text("muse.tool.edited"),
];

/// Whether a reply's head row `head` (its `◆` and indent stripped) is Muse
/// Code's own tool row ([`TOOL_HEADS`]), no word of the agent's. A `Ran `
/// head is the binary's whatever follows (a command is any words). A
/// `Wrote ` or `Edited ` head names a path ([`names_a_path`]), where the
/// agent's own sentence goes on in words: `Wrote the parser module and its
/// unit tests in this turn.` is the agent's report (measured 2026-10-20,
/// `docs/measured/forever-lab-2026-10-20.md` P2: read as a tool row, it left
/// the turn no last words, and a need, defense or done claim in it unread).
fn tool_row(head: &str) -> bool {
    let [ran, wrote, edited] = TOOL_HEADS;
    head.starts_with(ran)
        || [wrote, edited]
            .iter()
            .any(|h| head.strip_prefix(h).is_some_and(names_a_path))
}

/// Whether the words after a `Wrote `/`Edited ` head are a tool row's (every
/// one the 1.4.10 captures measured): a first word holding a `/`, a `.` or
/// the cut's `…` (`.agents/probe-write.md (+1)`, a path cut at the row's
/// end), a lone word, or one the row's own tail follows (`(+1)`, `— denied`).
fn names_a_path(rest: &str) -> bool {
    let (first, after) = rest.split_once(' ').unwrap_or((rest, ""));
    first.contains(['/', '.', '…'])
        || after.is_empty()
        || after.starts_with("(+")
        || after.starts_with("— ")
}

/// The agent's last words: the reply the last turn ended on, its `◆` and
/// indent stripped, one line per row. `None` while a box is up or a turn
/// runs, before any turn, where [`phase`] cannot say the turn ended, and
/// where it ended on Muse Code's own row: a tool row, the interrupt, or a
/// wall or a fault read off its notice ([`notice`]), a local command's rows
/// under it aside.
#[must_use]
pub fn said_tail(rows: &[String]) -> Option<String> {
    let (_, lines) = ended(rows)?;
    let head = *lines.first()?;
    let (_, message) = notice(rows)?;
    if head == const { anchor_text("muse.turn.interrupted") }
        || tool_row(head)
        || wall_of(&message).is_some()
        || is_fault(&message)
    {
        return None;
    }
    Some(lines.join("\n"))
}

/// WHETHER THE REPLY [`said_tail`] READS IS WHOLE ON `rows`: its `◆` head is
/// one of them ([`Turn::Ended`]). `false` for a reply taller than the rows,
/// its head and its message above them ([`Turn::Tail`]: its tail is then the
/// rows in sight, and what stood above them — a need, an open item — is not
/// in it), and where there are no last words.
#[must_use]
pub fn said_whole(rows: &[String]) -> bool {
    said_tail(rows).is_some()
        && open_frame(rows).is_some_and(|f| matches!(turn(rows, f.top), Turn::Ended(..)))
}

/// THE AGENT'S OTHER WORDS IN THE TURN ITS LAST WORDS END ([`said_tail`]):
/// every `◆` reply of the same turn above its last — the words it said
/// before a tool call, say — oldest first, each as [`said_tail`] reads one
/// (its `◆` and indent stripped, one line per row, blank rows left out),
/// joined by a blank line. Muse Code's own blocks are passed over as
/// [`said_tail`] passes them: a tool's row (`◆ Ran command · …`), the
/// interrupt, a wall, a fault, a recap or a turn's time
/// ([`is_own_row_head`]), and its pointer to the panel and its abnormal-end
/// warning under a reply. The turn is the rows under the person's last
/// message (`❯`), or from the session's banner or the rows' top where it is
/// above them. `None` where the turn holds no other reply on `rows`, or the
/// last reply's head is not on them.
#[must_use]
pub fn turn_said_before(rows: &[String]) -> Option<String> {
    let (_, said) = turn_before(rows)?;
    (!said.is_empty()).then(|| said.join("\n\n"))
}

/// WHETHER THE TURN [`said_tail`]'s REPLY ENDS IS WHOLE ON `rows`: the reply
/// is ([`said_whole`]), and so is the person's message that opened the turn
/// (its `❯` row). `false` where the turn reaches the rows' top: an earlier
/// reply of the turn, and a need said in it, may stand above them.
#[must_use]
pub fn turn_whole(rows: &[String]) -> bool {
    turn_before(rows).is_some_and(|(opened, _)| opened)
}

/// A row a read of the history above the screen stops at, for a turn
/// [`turn_whole`] finds cut: the person's message that opened it (`❯` in
/// column 0), or the session's banner over it, read walking up.
#[must_use]
pub fn turn_history_stop(row: &str) -> bool {
    row.starts_with('❯') || is_banner_row(row)
}

/// THE MESSAGE THAT OPENED THE TURN [`said_tail`]'s REPLY ENDS: the person's
/// last message (`❯` in column 0, the `❯` stripped) and the rows it wrapped
/// onto (indented, up to a blank row or a row in column 0), joined by a
/// space. `None` where there are no last words, and where that message is
/// not on `rows`.
#[must_use]
pub fn turn_opened_by(rows: &[String]) -> Option<String> {
    said_tail(rows)?;
    let f = open_frame(rows)?;
    let (_, user, _) = scope(rows, f.top);
    let user = user?;
    let end = (user + 1..f.top)
        .find(|&i| blank(&rows[i]) || leading_spaces(&rows[i]) == 0)
        .unwrap_or(f.top);
    let first = rows[user].strip_prefix('❯').unwrap_or(&rows[user]);
    let text: Vec<&str> = std::iter::once(first)
        .chain(rows[user + 1..end].iter().map(String::as_str))
        .map(crate::text::trim)
        .filter(|t| !t.is_empty())
        .collect();
    Some(text.join(" "))
}

/// The turn [`turn_said_before`] reads: whether the person's message that
/// opened it is on `rows` ([`turn_whole`]), and the words of its other
/// replies, oldest first. `None` where the last reply's head is not on
/// `rows` ([`said_whole`]), or there are no last words.
fn turn_before(rows: &[String]) -> Option<(bool, Vec<String>)> {
    if !said_whole(rows) {
        return None;
    }
    let f = open_frame(rows)?;
    let Turn::Ended(head, _) = turn(rows, f.top) else {
        return None;
    };
    let (_, user, from) = scope(rows, f.top);
    let mut said = Vec::new();
    for h in (from..head).filter(|&h| is_reply_head(&rows[h]) && !is_own_row_head(&rows[h])) {
        // The block's rows: up to the next row in column 0.
        let end = (h + 1..head)
            .find(|&k| leading_spaces(&rows[k]) == 0 && !blank(&rows[k]))
            .unwrap_or(head);
        let first = rows[h].strip_prefix('◆').unwrap_or(&rows[h]);
        let lines: Vec<&str> = std::iter::once(crate::text::trim(first))
            .chain(
                (h + 1..end)
                    .filter(|&k| !is_view_hint(rows, k) && !in_abnormal_end(rows, k))
                    .map(|k| crate::text::trim(&rows[k])),
            )
            .filter(|t| !t.is_empty())
            .collect();
        let Some(&top) = lines.first() else {
            continue;
        };
        let message = lines.join(" ");
        if top == const { anchor_text("muse.turn.interrupted") }
            || tool_row(top)
            || wall_of(&message).is_some()
            || is_fault(&message)
        {
            continue;
        }
        said.push(lines.join("\n"));
    }
    Some((user.is_some(), said))
}

/// The faults a turn can stop on that only a person can fix (the binary's
/// strings, unmeasured on a screen): a configuration problem, a
/// conversation it could not prepare, saved state it could not reach.
fn is_fault(message: &str) -> bool {
    [
        anchor_text("muse.fault.config"),
        anchor_text("muse.fault.prepare"),
        anchor_text("muse.fault.state"),
    ]
    .iter()
    .any(|f| message.starts_with(f))
}

/// Whether the last turn stopped on a fault ([`is_fault`]): no continuation
/// gets past it, so its end is no evidence.
fn faulted(rows: &[String]) -> bool {
    ended(rows).is_some_and(|(_, lines)| is_fault(&lines.join(" ")))
}

/// Whether a PERSON stopped the last turn with Esc: it ended on `◆
/// Interrupted · Report issues with /feedback` (1.4.10, a model's turn). The
/// next message is theirs. 1.4.9's retracted echo turn draws no marker, and
/// reads as a draft (module header). So too where cards alone hide that
/// marker and the newest `/status` card says the run was `CANCELLED`
/// (measured: Esc on a turn, then `/status` three times,
/// [`fixtures::INTERRUPTED_STATUS_CARDS`]).
#[must_use]
pub fn interrupted(rows: &[String]) -> bool {
    ended(rows).is_some_and(|(_, lines)| {
        lines.first().copied() == Some(const { anchor_text("muse.turn.interrupted") })
    }) || rests_on(rows) == Some(Rest::Cancelled)
}

/// The sign-in wall's first option row (`Log in with browser · Enter to
/// choose`): drawn instead of a composer, its key row `↓↑ to select · Esc to
/// quit` the last row drawn.
fn signed_out(rows: &[String]) -> Option<usize> {
    let end = rows.iter().rposition(|r| !blank(r))?;
    if crate::text::trim(&rows[end]) != const { anchor_text("muse.signin.keys") } {
        return None;
    }
    rows[..end].iter().rposition(|r| {
        crate::text::trim(r).starts_with(const { anchor_text("muse.signin.browser") })
    })
}

/// What a turn's last `◆` block says of a wall (module header), as `(kind,
/// reset)`, read whole from its head — the vendor's own shapes, never words
/// anywhere in a reply:
///
/// * the key refused (401, measured), and its two siblings in the binary's
///   strings (`your saved API key was rejected — …`, `no login to refresh a
///   key from; …`): [`WallKind::Auth`]; and the walls of a Meta ACCOUNT's
///   login, from the binary's strings alone (`Model API requires a payment
///   method on your account — …`, `Muse Code requires a payment method —
///   subscribe at …`, `your Meta account isn't set up for Model API yet —
///   …`), each ending `then log in again`: [`WallKind::Auth`] too — the lab
///   cannot draw them (module header, "WHAT THE LAB CANNOT DRAW");
/// * the binary's lost logins (`Your saved Meta credentials are invalid. …`,
///   `No Meta credentials were found. …`, the Keychain refusing or not
///   reaching them): [`WallKind::Auth`];
/// * `Usage limit reached · /upgrade (<url>) for increased limits[, or wait
///   for usage to reset at <time>]` (measured with the reset), or bare as
///   the binary has it: [`WallKind::UsageSession`], reset at that time;
/// * the binary's `You've used your <window> quota (resets at <time>).`:
///   [`WallKind::UsageSession`], reset at that time;
/// * `model failed: API error <status>: …` (429 and 500 measured):
///   [`WallKind::ApiError`] with the status, retryable at 408, 409, 429 and
///   every 5xx. A `model failed: ` naming no status is one too, retryable.
fn wall_of(message: &str) -> Option<(WallKind, Option<String>)> {
    let auth = [
        anchor_text("muse.wall.key"),
        anchor_text("muse.wall.saved_key"),
        anchor_text("muse.wall.no_login"),
        anchor_text("muse.wall.payment_api"),
        anchor_text("muse.wall.payment_muse"),
        anchor_text("muse.wall.onboarding"),
    ];
    let lost = [
        anchor_text("muse.wall.invalid"),
        anchor_text("muse.wall.missing"),
        anchor_text("muse.wall.denied"),
        anchor_text("muse.wall.canceled"),
        anchor_text("muse.wall.unreachable"),
    ];
    if auth.contains(&message) || lost.iter().any(|l| message.starts_with(l)) {
        return Some((WallKind::Auth, None));
    }
    if let Some(rest) = message.strip_prefix(const { anchor_text("muse.wall.usage") }) {
        if matches!(rest, "" | ".") {
            return Some((WallKind::UsageSession, None));
        }
        let rest = rest.strip_prefix(const { anchor_text("muse.wall.usage.upgrade") })?;
        let (_, tail) = rest.split_once(const { anchor_text("muse.wall.usage.more") })?;
        let reset = tail
            .split_once(const { anchor_text("muse.wall.usage.reset") })
            .map(|(_, at)| crate::text::trim(at).trim_end_matches('.').to_string())
            .filter(|at| !at.is_empty());
        return Some((WallKind::UsageSession, reset));
    }
    if let Some((_, tail)) = message
        .strip_prefix(const { anchor_text("muse.wall.quota") })
        .and_then(|r| r.split_once(" quota"))
    {
        let reset = tail
            .strip_prefix(" (resets at ")
            .and_then(|r| r.split_once(')'))
            .map(|(at, _)| crate::text::trim(at).to_string())
            .filter(|at| !at.is_empty());
        return Some((WallKind::UsageSession, reset));
    }
    let rest = message.strip_prefix(const { anchor_text("muse.wall.model_failed") })?;
    let code = rest
        .strip_prefix(const { anchor_text("muse.wall.api_error") })
        .and_then(|r| r.get(..4))
        .and_then(|c| c.strip_suffix(':'))
        .and_then(|c| c.parse::<u16>().ok());
    Some((
        WallKind::ApiError {
            code,
            retryable: code.is_none_or(|c| matches!(c, 408 | 409 | 429) || c >= 500),
            cause: ApiCause::Server,
        },
        None,
    ))
}

/// Muse Code's OWN REMEDY for a lost login, in its own words: the clause of
/// its key walls after the dash or the semicolon — `update or unset it`
/// (`it` being `META_API_KEY`, named here), `run \`muse login\` or add a new
/// key`, `run \`muse login\` or set META_API_KEY` — and of its account walls
/// after the dash (`add one at https://dev.meta.ai, then log in again`). The
/// first matters: a key in `META_API_KEY` beats the saved login, so `muse
/// login` does not lift it. `None` for any other message: its signed-out
/// menu draws no remedy row.
#[must_use]
pub fn remedy(message: &str) -> Option<String> {
    let m = crate::text::trim(message);
    if m == const { anchor_text("muse.wall.key") } {
        let (_, r) = m.split_once(" — ")?;
        return Some(
            r.strip_suffix(" it")
                .map_or_else(|| r.to_string(), |verb| format!("{verb} META_API_KEY")),
        );
    }
    if m == const { anchor_text("muse.wall.saved_key") } {
        return m.split_once(" — ").map(|(_, r)| r.to_string());
    }
    if m == const { anchor_text("muse.wall.no_login") } {
        return m.split_once("; ").map(|(_, r)| r.to_string());
    }
    if [
        anchor_text("muse.wall.payment_api"),
        anchor_text("muse.wall.payment_muse"),
        anchor_text("muse.wall.onboarding"),
    ]
    .contains(&m)
    {
        return m
            .split_once(" — ")
            .map(|(_, r)| r.trim_end_matches('.').to_string());
    }
    None
}

/// The wall on the screen: Muse Code signed out (`WallKind::Auth`, its login
/// menu never read), or the notice the turn's last `◆` block opens on
/// ([`notice`]: its head and the rows it wraps onto, joined with one space,
/// a local command's rows under it left out) in a wall's own shape
/// ([`wall_of`]) — the vendor's error row as the last thing said
/// ([`Placement::ErrorRow`]). `None` while a turn runs: a retry row
/// ([`retrying`]) is no wall.
#[must_use]
pub fn wall(rows: &[String]) -> Option<Wall> {
    if let Some(row) = signed_out(rows) {
        return Some(Wall::sign_in(rows, row));
    }
    let (head, message) = notice(rows)?;
    let (kind, reset) = wall_of(&message)?;
    Some(Wall {
        kind,
        message,
        reset,
        row: head,
        placement: Placement::ErrorRow,
        under_call: false,
    })
}

/// What Muse Code is retrying while a turn runs: its row under the status
/// row, `└ Meta Model API rate limited (HTTP 429) · retrying in 5s · attempt
/// 2/3` (and `server error (HTTP 500)`, measured).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retry {
    /// The status in `(HTTP <status>)`, when the row names one.
    pub code: Option<u16>,
    /// The wait before the next attempt, as drawn (`5s`).
    pub wait: String,
    /// The attempt to come, and how many there will be at most.
    pub attempt: u32,
    pub of: u32,
}

/// The retry under a running turn's status row ([`Retry`]); `None` with no
/// turn running or no such row right under it.
#[must_use]
pub fn retrying(rows: &[String]) -> Option<Retry> {
    let f = open_frame(rows)?;
    if turn(rows, f.top) != Turn::Running {
        return None;
    }
    let status = (0..f.top).rev().find(|&i| is_status_row(&rows[i]))?;
    let t = crate::text::trim(rows.get(status + 1)?.strip_prefix("└ ")?);
    let rest = t.strip_prefix(const { anchor_text("muse.retry.api") })?;
    let (reason, tail) = rest.split_once(const { anchor_text("muse.retry.in") })?;
    let (wait, attempt) = tail.split_once(const { anchor_text("muse.retry.attempt") })?;
    let (k, n) = attempt.split_once('/')?;
    let code = reason
        .rsplit_once("(HTTP ")
        .and_then(|(_, c)| c.strip_suffix(')'))
        .and_then(|c| c.parse().ok());
    Some(Retry {
        code,
        wait: wait.to_string(),
        attempt: k.parse().ok()?,
        of: crate::text::trim(n).parse().ok()?,
    })
}

// ---------------------------------------------------------------------------
// The boxes.
// ---------------------------------------------------------------------------

/// `⟩ 1  Trust and continue` / `  2  Quit` → `(1, "Trust and continue",
/// focused)`: the number, two blanks, the label.
fn option(row: &str) -> Option<(u8, &str, bool)> {
    crate::prompt::numbered_option(row, "⟩ ", "  ")
}

/// The trust box as `(title row, its option rows, key row)`: `Do you trust
/// this workspace?`, the numbered options under it, and the key row ending
/// `Esc quits.` as the last row drawn.
fn trust_box(rows: &[String]) -> Option<(usize, std::ops::Range<usize>, usize)> {
    let keys = rows.iter().rposition(|r| !blank(r))?;
    if !crate::text::trim(&rows[keys]).ends_with(const { anchor_text("muse.trust.keys") }) {
        return None;
    }
    let title = rows[..keys]
        .iter()
        .position(|r| crate::text::trim(r) == const { anchor_text("muse.trust.title") })?;
    let first = (title + 1..keys).find(|&i| option(&rows[i]).is_some())?;
    let end = (first..keys)
        .find(|&i| option(&rows[i]).is_none())
        .unwrap_or(keys);
    Some((title, first..end, keys))
}

/// The trust box, read whole: its folder, `Trust and continue` the trust and
/// `Quit` the exit (roles only where the options run `1`, `2` with the
/// cursor on exactly one), chosen by the cursor (or a digit) and then Enter,
/// its Esc quitting Muse Code.
fn trust_prompt(
    rows: &[String],
    (title, opts, keys): (usize, std::ops::Range<usize>, usize),
) -> PromptV2 {
    let mut options: Vec<Opt> = opts
        .clone()
        .filter_map(|i| {
            option(&rows[i]).map(|(n, label, focused)| Opt {
                n: Some(n),
                label: label.to_string(),
                role: Role::Other,
                focused,
                row: i,
                kept: false,
            })
        })
        .collect();
    if sound(&options) {
        for o in &mut options {
            o.role = if o.label == const { anchor_text("muse.trust.yes") } {
                Role::Trust
            } else if o.label == const { anchor_text("muse.trust.quit") } {
                Role::Exit
            } else {
                Role::Other
            };
        }
    }
    let path = rows[title + 1..opts.start].iter().find_map(|r| {
        crate::text::trim(r)
            .strip_prefix(const { anchor_text("muse.trust.path") })
            .map(|p| crate::text::trim(p).to_string())
    });
    let footer = crate::text::trim(&rows[keys]).to_string();
    let verb = footer
        .to_lowercase()
        .split_once("esc ")
        .map(|(_, v)| v.trim().trim_end_matches('.').to_string())
        .unwrap_or_default();
    PromptV2 {
        kind: PromptKind::Trust,
        title: crate::text::trim(&rows[title]).to_string(),
        command_rows: Vec::new(),
        description_rows: 0,
        gutter: false,
        command: path.clone().unwrap_or_default(),
        description: String::new(),
        notes: Vec::new(),
        auto_deny: None,
        path,
        options,
        select: Select::ArrowsEnter,
        cancel: Some(Cancel {
            key: "esc".to_string(),
            verb,
            effect: CancelEffect::Exit,
        }),
        question: Some(title),
        span: (title, keys),
        footer: Some(footer),
        head_off_screen: false,
        question_dialog: None,
        reason_rows: Vec::new(),
    }
}

/// Whether options carry roles: they run `1`, `2`, … in order with the
/// cursor on exactly one.
fn sound(options: &[Opt]) -> bool {
    options
        .iter()
        .enumerate()
        .all(|(k, o)| o.n.is_some_and(|n| usize::from(n) == k + 1))
        && options.iter().filter(|o| o.focused).count() == 1
}

/// `⟩ 1. Yes, proceed (y)` / `  2. No, …` → `(1, "Yes, proceed (y)",
/// focused)`: an approval box's option, in column 0.
fn box_option(row: &str) -> Option<(u8, &str, bool)> {
    crate::prompt::numbered_option(row, "⟩ ", ". ")
}

/// `  ⟩ 1. Red (Recommended)  Warm and loud.` / `    2. Blue …`: a
/// question's option, two columns in.
fn question_option(row: &str) -> Option<(u8, &str, bool)> {
    box_option(row.strip_prefix("  ")?)
}

/// `Yes, proceed (y)` → `("Yes, proceed", Some("y"))`: a label less the
/// one-key shortcut marked after it.
fn label_and_key(text: &str) -> (&str, Option<&str>) {
    match text.strip_suffix(')').and_then(|t| t.rsplit_once(" (")) {
        Some((label, key))
            if (1..=3).contains(&key.len()) && key.chars().all(|c| c.is_ascii_lowercase()) =>
        {
            (label, Some(key))
        }
        _ => (text, None),
    }
}

/// A box drawn in the caret row's place ([`approval_box`]).
#[derive(Debug, Clone, Copy)]
struct Approval {
    /// The title row (`Would you like to …?`): the first row under the
    /// frame's labelled rule. `None` with that rule off the rows read.
    title: Option<usize>,
    /// The first and last option rows (a wrapped label's tail between).
    first: usize,
    last: usize,
    /// The box's last row: the hint `Change approval defaults in
    /// /permissions`, or its last option.
    end: usize,
}

/// The labels Muse Code draws on its composer's top rule: `Voice input (⌥ +
/// v to start)` (measured), and the binary's others — voice input's other
/// states and the side chat's.
const TOP_LABELS: [&str; 8] = [
    anchor_text("muse.composer.voice"),
    anchor_text("muse.composer.voice_prefix"),
    anchor_text("muse.composer.talk"),
    anchor_text("muse.composer.recording"),
    anchor_text("muse.composer.transcribing"),
    anchor_text("muse.composer.offline"),
    anchor_text("muse.composer.error"),
    anchor_text("muse.sidechat.label"),
];

/// Whether `rule` carries one of [`TOP_LABELS`].
fn labelled(rule: &str) -> bool {
    TOP_LABELS.iter().any(|l| crate::text::contains(rule, l))
}

/// An approval box (module header): drawn inside the composer's frame with
/// no caret row in it — the frame's labelled rule, the title, the numbered
/// options with the `⟩` cursor, the `/permissions` hint, the bottom rule and
/// the footer as the last row drawn. With the labelled rule off the rows
/// read, the hint over the bottom rule names it.
fn approval_box(rows: &[String]) -> Option<Approval> {
    let (bottom, _) = bottom_and_footer(rows)?;
    let end = (0..bottom).rev().find(|&i| !blank(&rows[i]))?;
    let hint = crate::text::trim(&rows[end]) == const { anchor_text("muse.box.permissions") };
    let last = if hint {
        (0..end).rev().find(|&i| !blank(&rows[i]))?
    } else {
        end
    };
    box_option(&rows[last])?;
    let mut first = last;
    while first > 0
        && (box_option(&rows[first - 1]).is_some()
            || is_wrap(&rows[first - 1])
            || is_edge_tail(&rows[first - 1]))
    {
        first -= 1;
    }
    while box_option(&rows[first]).is_none() {
        first += 1;
    }
    match (0..first).rev().find(|&i| is_rule(&rows[i])) {
        Some(top) => {
            if !labelled(&rows[top]) || rows[top + 1..first].iter().any(|r| r.starts_with('❯')) {
                return None;
            }
            // Every measured box draws its title right under the rule; with
            // that row blank the box is caught mid-draw, and is no box yet.
            let title = top + 1;
            if title >= first || blank(&rows[title]) {
                return None;
            }
            Some(Approval {
                title: Some(title),
                first,
                last,
                end,
            })
        }
        None if hint => Some(Approval {
            title: None,
            first,
            last,
            end,
        }),
        None => None,
    }
}

/// A wrapped option label's tail: a non-blank row indented to the label's
/// column (five) or past it, no option itself. Not measured: every label
/// drawn fit its row, and at 48 columns the refusal's label was cut at the
/// row's edge (`… what to do different`) rather than wrapped.
fn is_wrap(row: &str) -> bool {
    !blank(row) && leading_spaces(row) >= 5 && box_option(row).is_none()
}

/// An option's text carried past the row's edge onto a row of its own in
/// column 0: measured at 60 columns on the network box, whose always grant
/// names its destination beside its label and drew the destination's
/// `(http)` alone under it ([`fixtures::BOX_NETWORK_NARROW`]). No option, no
/// rule, no `❯` row; between two options it is the one above's
/// ([`approval_prompt`]), and above the first it is no option's.
fn is_edge_tail(row: &str) -> bool {
    !blank(row)
        && leading_spaces(row) == 0
        && !is_rule(row)
        && !row.starts_with('❯')
        && box_option(row).is_none()
}

/// An approval box's kind, by its title and body (module header's table): a
/// file box is a write by its `write path:` row, the network box a network
/// request by its `full URL:` row, and the tool-action box a
/// [`PromptKind::Tool`] call by its `tool action:` row. Any other title or
/// body — a file box naming another access, the network box of a command
/// (`requested by:`: a host, a Unix socket) — is [`PromptKind::Other`].
fn approval_kind(title: &str, write_path: bool, url: bool, tool: bool) -> PromptKind {
    let t = crate::text::trim(title);
    if t == const { anchor_text("muse.box.shell") } {
        PromptKind::Bash
    } else if t == const { anchor_text("muse.box.file") } && write_path {
        PromptKind::Write
    } else if t == const { anchor_text("muse.box.network") } && url {
        PromptKind::NetworkRequest
    } else if t == const { anchor_text("muse.box.tool") } && tool {
        PromptKind::Tool
    } else {
        PromptKind::Other
    }
}

/// What choosing an approval box's `label` does (module header): `Yes,
/// proceed` and a pipeline's `Allow this stage once` allow this one call or
/// stage; `No`, `No, …`, `Abort the entire command` and the Unix-socket
/// box's `Reject` refuse; a yes that names `this session` (the network and
/// tool-action boxes' `Yes, don't ask again this session`, measured, and the
/// binary's `Yes, and don't ask again for this command in this session`)
/// grants the rest of the session ([`Role::Session`]); any other `Yes, …`,
/// and `Always allow …` (the network box's `Always allow this network
/// destination`, the tool-action box's `Always allow this MCP tool`, 1.4.9's
/// staged box's `Always allow in this workspace: …`), are standing grants
/// kept past the session ([`Role::Persist`]).
fn approval_role(label: &str) -> Role {
    if label == const { anchor_text("muse.box.once") }
        || label == const { anchor_text("muse.box.stage_once") }
    {
        Role::Once
    } else if label == "No"
        || label.starts_with("No,")
        || label == const { anchor_text("muse.box.stage_abort") }
        || label == const { anchor_text("muse.box.reject") }
    {
        Role::Deny
    } else if (label.starts_with("Yes,") || label.starts_with("Yes "))
        && label.ends_with("this session")
    {
        Role::Session
    } else if label.starts_with("Yes,")
        || label.starts_with("Yes ")
        || label.starts_with(const { anchor_text("muse.box.always_prefix") })
    {
        Role::Persist
    } else {
        Role::Other
    }
}

/// A row the shell box draws under its command while it asks for ONE STAGE
/// of a pipeline (measured on 1.4.10, `env | cut -d= -f1 | sort >
/// envnames.txt`): `Skipped Stage 1/3: matched mode:on_request`, `Stage
/// 3/3`, `Current argv: ["sort"]`. No part of the command.
fn is_stage_row(t: &str) -> bool {
    t.starts_with(const { anchor_text("muse.box.stage_skipped") })
        || t.starts_with(const { anchor_text("muse.box.stage_argv") })
        || t.strip_prefix(const { anchor_text("muse.box.stage") })
            .is_some_and(|r| r.starts_with(|c: char| c.is_ascii_digit()))
}

/// The approval box, read whole ([`PromptV2`]): its kind and title, the
/// shell box's command (the `$ ` row and the rows under it to a blank or a
/// stage row, [`is_stage_row`], the stage rows its notes) and the file box's
/// path (its `write path: ` row, or a bare `write path:` row, and the rows
/// a long path wrapped onto, joined with nothing), the network box's URL
/// (its `full URL: ` row and the rows it broke onto, joined with nothing)
/// and its `network:` row (its note), the tool-action box's tool (its `tool
/// action: ` row, the rows under it joined with nothing), the command the
/// network box of a command names (its `$ ` row and the rows under it,
/// joined as the shell box's are: its [`PromptV2::command`] alone, for a box
/// of no kind), the options with their roles (only for a measured kind whose
/// options are [`sound`]), their one-key shortcuts and what a grant names
/// beside them stripped, chosen by the cursor and Enter. Its Esc is the
/// option marked `(esc)`: a refusal where that option's label refuses
/// ([`approval_role`]: `No, …`, `Abort the entire command`, `Reject`),
/// whether or not the options carry roles.
fn approval_prompt(rows: &[String], b: Approval) -> PromptV2 {
    let body = b.title.map_or(0, |t| t + 1)..b.first;
    let trimmed = |i: usize| crate::text::trim(&rows[i]);
    let block_from = |at: usize| -> Vec<&str> {
        (at..b.first)
            .map(trimmed)
            .take_while(|t| !t.is_empty() && !is_stage_row(t))
            .collect()
    };
    let notes: Vec<String> = body
        .clone()
        .map(trimmed)
        .filter(|t| is_stage_row(t))
        .map(str::to_string)
        .collect();
    let command_rows: Vec<String> = body
        .clone()
        .find(|&i| trimmed(i).starts_with("$ "))
        .map(|at| {
            block_from(at)
                .into_iter()
                .enumerate()
                .map(|(k, t)| {
                    if k == 0 {
                        t.trim_start_matches('$').trim_start().to_string()
                    } else {
                        t.to_string()
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    // `write path: <path>`, or `write path:` alone on its row with the path
    // under it, where the path does not fit beside it
    // ([`fixtures::BOX_WRITE_WRAPPED`]).
    let write_label = const { anchor_text("muse.box.write_path") }.trim_end();
    let path = body
        .clone()
        .find(|&i| {
            trimmed(i)
                .strip_prefix(write_label)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
        })
        .and_then(|at| {
            let rows = block_from(at);
            let first = rows[0].strip_prefix(write_label).unwrap_or_default().trim();
            let out: String = std::iter::once(first)
                .chain(rows[1..].iter().copied())
                .collect();
            (!out.is_empty()).then_some(out)
        });
    // The network box's `full URL: <url>` row and the rows a long URL was
    // broken onto at the row's edge (joined with nothing, as a path's are),
    // and its `network: <host:port> <protocol>` row
    // ([`fixtures::BOX_NETWORK`], [`fixtures::BOX_NETWORK_NARROW`]).
    let url_label = const { anchor_text("muse.box.full_url") };
    let url = body
        .clone()
        .find(|&i| trimmed(i).starts_with(url_label))
        .and_then(|at| {
            let rows = block_from(at);
            let first = rows[0].strip_prefix(url_label).unwrap_or_default().trim();
            let out: String = std::iter::once(first)
                .chain(rows[1..].iter().copied())
                .collect();
            (!out.is_empty()).then_some(out)
        });
    let network: Vec<String> = body
        .clone()
        .map(trimmed)
        .filter(|t| t.starts_with(const { anchor_text("muse.box.network_row") }))
        .map(str::to_string)
        .collect();
    // The tool-action box's `tool action: <tool>` row
    // ([`fixtures::BOX_TOOL_ACTION`]); a name too long for its row is not
    // measured, and is joined with nothing as a URL is.
    let tool_label = const { anchor_text("muse.box.tool_row") };
    let tool = body
        .clone()
        .find(|&i| trimmed(i).starts_with(tool_label))
        .and_then(|at| {
            let rows = block_from(at);
            let first = rows[0].strip_prefix(tool_label).unwrap_or_default().trim();
            let out: String = std::iter::once(first)
                .chain(rows[1..].iter().copied())
                .collect();
            (!out.is_empty()).then_some(out)
        });
    let kind = match b.title {
        Some(t) => approval_kind(&rows[t], path.is_some(), url.is_some(), tool.is_some()),
        None => PromptKind::Other,
    };
    // The network box of a command (`requested by:` and `$ <command>`, no
    // `full URL:` row: [`fixtures::BOX_NETWORK_COMMAND`],
    // [`fixtures::BOX_UNIX_SOCKET`]): a box of no kind whose command is read
    // for the person it goes to, never as a command row a decider classifies.
    let network_command = (kind == PromptKind::Other
        && b.title
            .is_some_and(|t| trimmed(t) == const { anchor_text("muse.box.network") }))
    .then(|| command_rows.join(" "));
    let (command_rows, path, notes) = match kind {
        PromptKind::Bash => (command_rows, None, notes),
        PromptKind::Write => (Vec::new(), path, Vec::new()),
        PromptKind::NetworkRequest => (Vec::new(), None, network),
        _ => (Vec::new(), None, Vec::new()),
    };

    // Each option's text, a tail it was carried onto joined with a blank,
    // then read: the text past two blanks names what a grant is for (the
    // network box's destination, `127.0.0.1:50581 (http)`; the tool-action
    // box's ``tool `<tool>` ``), and is no part of the label; the one-key
    // mark is stripped off what is left.
    let mut texts: Vec<(u8, String, bool, usize)> = Vec::new();
    for (i, row) in rows.iter().enumerate().take(b.last + 1).skip(b.first) {
        if let Some((n, text, focused)) = box_option(row) {
            texts.push((n, text.to_string(), focused, i));
        } else if let Some((_, text, _, _)) = texts.last_mut() {
            *text = format!("{text} {}", crate::text::trim(row));
        }
    }
    let mut options: Vec<Opt> = Vec::new();
    let mut keys: Vec<Option<String>> = Vec::new();
    for (n, text, focused, i) in &texts {
        let named = text.split_once("  ").map_or(text.as_str(), |(l, _)| l);
        let (label, key) = label_and_key(crate::text::trim(named));
        options.push(Opt {
            n: Some(*n),
            label: label.to_string(),
            role: Role::Other,
            focused: *focused,
            row: *i,
            kept: false,
        });
        keys.push(key.map(str::to_string));
    }
    if matches!(
        kind,
        PromptKind::Bash | PromptKind::Write | PromptKind::NetworkRequest | PromptKind::Tool
    ) && sound(&options)
    {
        for o in &mut options {
            o.role = approval_role(&o.label);
        }
    }
    let cancel = options
        .iter()
        .zip(&keys)
        .find(|(_, k)| k.as_deref() == Some("esc"))
        .map(|(o, _)| Cancel {
            key: "esc".to_string(),
            verb: o.label.clone(),
            effect: match approval_role(&o.label) {
                Role::Deny => CancelEffect::Reject,
                _ => CancelEffect::Back,
            },
        });
    let command = match kind {
        PromptKind::Bash => command_rows.join(" "),
        PromptKind::NetworkRequest => url.unwrap_or_default(),
        PromptKind::Tool => tool.unwrap_or_default(),
        _ => network_command.or_else(|| path.clone()).unwrap_or_default(),
    };
    let title_row = b.title.unwrap_or(b.first);
    PromptV2 {
        kind,
        title: trimmed(title_row).to_string(),
        command_rows,
        description_rows: 0,
        gutter: false,
        command,
        description: String::new(),
        notes,
        auto_deny: None,
        path,
        options,
        select: Select::ArrowsEnter,
        cancel,
        question: b.title,
        span: (title_row, b.end),
        footer: None,
        head_off_screen: b.title.is_none(),
        question_dialog: None,
        reason_rows: Vec::new(),
    }
}

/// A question drawn over the composer ([`question_box`]).
#[derive(Debug, Clone)]
struct Question {
    /// Its head (`◆ Request user input <header> — running (1s)`); `None`
    /// with it off the rows read.
    head: Option<usize>,
    /// The first option row, and the options' last row: the last option,
    /// or a description, a note or a wrapped tail under it.
    first: usize,
    last: usize,
    /// The row naming which question of a call that asks several is drawn
    /// (`1 of 2`, between the options and the key row; measured), if any.
    position: Option<usize>,
    /// The key row's first and last rows (`Enter to select · … · Esc to
    /// interrupt`, wrapped onto two rows in a narrow tab), and its words,
    /// those rows joined with a blank.
    keys: (usize, usize),
    key_words: String,
}

/// The question's key row whose last row is `end`, as `(its first row, its
/// words)`: two columns in, `Enter to …` through `Esc to interrupt` — the
/// single answer's ([`anchor_text`] `muse.question.keys`), several
/// questions' (`muse.question.keys_multi`), a multi-select's
/// (`muse.question.keys_toggle`), all measured, or the binary's other — on
/// one row, or wrapped between words onto the rows above `end` where the
/// tab is too narrow for it (two rows at 60 and at 72 columns,
/// [`fixtures::QUESTION_WRAPPED`]), joined with a blank.
fn question_keys(rows: &[String], end: usize) -> Option<(usize, String)> {
    let mut words = String::new();
    for start in (end.saturating_sub(2)..=end).rev() {
        let row = &rows[start];
        if blank(row) || leading_spaces(row) != 2 {
            return None;
        }
        let t = crate::text::trim(row);
        words = if words.is_empty() {
            t.to_string()
        } else {
            format!("{t} {words}")
        };
        if words.starts_with("Enter to ") {
            return words
                .ends_with(const { anchor_text("muse.question.esc") })
                .then_some((start, words));
        }
    }
    None
}

/// The question's head: a status glyph, then `Request user input`.
fn is_question_head(row: &str) -> bool {
    row.strip_prefix(STATUS_GLYPHS)
        .and_then(|r| r.strip_prefix(' '))
        .is_some_and(|r| r.starts_with(const { anchor_text("muse.question.title") }))
}

/// `  1 of 2`: which question of several is drawn, two columns in, between
/// the options and the key row (measured on 1.4.10, [`fixtures::QUESTION_MULTI`],
/// and on 1.4.9, [`fixtures::BOX_QUESTION_1_OF_2`]).
fn is_position_row(row: &str) -> bool {
    leading_spaces(row) == 2
        && crate::text::trim(row)
            .split_once(" of ")
            .is_some_and(|(k, n)| {
                [k, n]
                    .iter()
                    .all(|d| !d.is_empty() && d.len() <= 2 && d.bytes().all(|b| b.is_ascii_digit()))
            })
}

/// A row of the question's NOTE (measured on 1.4.10): `Note (optional):
/// <text>` six columns in under the focused option, opened by Tab, its
/// cursor `▌` after the text while the field has the keys, and the rows a
/// long note wraps onto between words, six columns in too
/// ([`fixtures::QUESTION_NOTE_LONG`]). A description under its label is
/// seven in ([`is_question_wrap`]).
fn is_note_row(row: &str) -> bool {
    !blank(row) && leading_spaces(row) == 6 && question_option(row).is_none()
}

/// The question over the composer of frame `f` (module header): its key row
/// ([`question_keys`]) the last thing drawn above the frame's labelled rule,
/// several questions' `1 of 2` row over it, the options over that (the last
/// option perhaps under its description's or its note's rows), and its head
/// above them, the first row in column 0.
fn question_box(rows: &[String], f: Frame) -> Option<Question> {
    let end = (0..f.top).rev().find(|&i| !blank(&rows[i]))?;
    let (start, key_words) = question_keys(rows, end)?;
    let mut last = (0..start).rev().find(|&i| !blank(&rows[i]))?;
    let position = is_position_row(&rows[last]).then_some(last);
    if position.is_some() {
        last = (0..last).rev().find(|&i| !blank(&rows[i]))?;
    }
    let under = |row: &str| is_question_wrap(row) || is_note_row(row);
    let mut option_row = last;
    while under(&rows[option_row]) {
        option_row = option_row.checked_sub(1)?;
    }
    question_option(&rows[option_row])?;
    let mut first = option_row;
    while first > 0 && (question_option(&rows[first - 1]).is_some() || under(&rows[first - 1])) {
        first -= 1;
    }
    while question_option(&rows[first]).is_none() {
        first += 1;
    }
    let head = (0..first)
        .rev()
        .find(|&i| !blank(&rows[i]) && leading_spaces(&rows[i]) == 0)
        .filter(|&i| is_question_head(&rows[i]));
    Some(Question {
        head,
        first,
        last,
        position,
        keys: (start, end),
        key_words,
    })
}

/// A row under an option: its description's, under its label (a narrow
/// tab, [`fixtures::QUESTION_WRAPPED`]) or wrapped in its own column beside
/// it (measured at 72 columns), or a wrapped label's tail (not measured) —
/// indented to the label's column (seven) or past it, no option itself.
fn is_question_wrap(row: &str) -> bool {
    !blank(row) && leading_spaces(row) >= 7 && question_option(row).is_none()
}

/// The question's title: its head less the glyph and the running clock
/// (`Request user input Color`), or the first option where the head is off
/// the rows read.
fn question_title(rows: &[String], head: Option<usize>, first: usize) -> String {
    match head {
        Some(h) => {
            let t = crate::text::trim(rows[h].strip_prefix(STATUS_GLYPHS).unwrap_or(&rows[h]));
            t.rsplit_once(" — ").map_or(t, |(t, _)| t).to_string()
        }
        None => crate::text::trim(&rows[first]).to_string(),
    }
}

/// The question, read whole ([`PromptV2`]): [`PromptKind::Question`], titled
/// by its head ([`question_title`]), its question the rows between, the
/// options' labels (each less its description: the text past two blanks,
/// or, where no description is drawn beside its label, the rows under it;
/// a note's rows are no part of any), chosen by the cursor and Enter.
/// Several questions' `1 of 2` row is its note. Only the measured single
/// answer, its head on the rows read and its options [`sound`], carries
/// roles: each of the model's answers [`Role::Answer`], `None of the above`
/// [`Role::Other`] — the person's own words, which a question rule never
/// takes for an answer. A call of several questions and a multi-select
/// carry none (module header). Its Esc stops the turn and refuses nothing.
fn question_prompt(rows: &[String], q: Question) -> PromptV2 {
    let trimmed = |i: usize| crate::text::trim(&rows[i]);
    let beside = |text: &str| {
        text.split_once("  ")
            .is_some_and(|(_, rest)| !crate::text::trim(rest).is_empty())
    };
    // Where no description fits beside its label (`None of the above` always
    // has one), each is drawn under its label: the rows under an option are
    // its description's ([`fixtures::QUESTION_WRAPPED`]).
    let stacked = !(q.first..=q.last)
        .any(|i| question_option(&rows[i]).is_some_and(|(_, text, _)| beside(text)));
    let mut options: Vec<Opt> = Vec::new();
    let mut described = false;
    for (i, row) in rows.iter().enumerate().take(q.last + 1).skip(q.first) {
        if let Some((n, text, focused)) = question_option(row) {
            let (label, _) = text.split_once("  ").unwrap_or((text, ""));
            options.push(Opt {
                n: Some(n),
                label: crate::text::trim(label).to_string(),
                role: Role::Other,
                focused,
                row: i,
                kept: false,
            });
            described = stacked || beside(text);
        } else if is_note_row(row) {
            // The note's: no part of a label.
        } else if !described && let Some(o) = options.last_mut() {
            // A wrapped label; under a described option, the description's.
            o.label = format!("{} {}", o.label, crate::text::trim(row));
        }
    }
    let keys = q.key_words.clone();
    if q.head.is_some()
        && q.position.is_none()
        && keys == const { anchor_text("muse.question.keys") }
        && sound(&options)
    {
        for o in &mut options {
            o.role = if o.label == const { anchor_text("muse.question.none") } {
                Role::Other
            } else {
                Role::Answer
            };
        }
    }
    let asked: Vec<usize> = (q.head.map_or(0, |h| h + 1)..q.first)
        .filter(|&i| !blank(&rows[i]))
        .collect();
    PromptV2 {
        kind: PromptKind::Question,
        title: question_title(rows, q.head, q.first),
        command_rows: Vec::new(),
        description_rows: 0,
        gutter: false,
        command: String::new(),
        description: asked
            .iter()
            .map(|&i| trimmed(i))
            .collect::<Vec<_>>()
            .join(" "),
        notes: q
            .position
            .map(|r| trimmed(r).to_string())
            .into_iter()
            .collect(),
        auto_deny: None,
        path: None,
        options,
        select: Select::ArrowsEnter,
        cancel: Some(Cancel {
            key: "esc".to_string(),
            verb: "interrupt".to_string(),
            effect: CancelEffect::Interrupt,
        }),
        question: asked.first().copied(),
        span: (q.head.unwrap_or(q.first), q.keys.1),
        footer: Some(keys),
        head_off_screen: q.head.is_none(),
        question_dialog: None,
        reason_rows: Vec::new(),
    }
}

/// A question's note field as read ([`question_notes`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionNotes {
    /// The field's first row: what a key typed into it is guarded on.
    pub row: usize,
    /// Its text as DRAWN, the label and the cursor `▌` stripped and the rows
    /// it wrapped onto joined with one space.
    pub text: String,
}

/// THE NOTE of the question on `rows` (module header, "THE QUESTION'S
/// NOTE"): its first row (`      Note (optional): <text>`, what a key typed into it
/// is guarded on) and its text, the label and the cursor `▌` stripped and
/// the rows it wrapped onto joined with one blank. `None` when no question
/// is up or no note is drawn: Tab opens the field, and Esc or Tab in it
/// CLOSES it and CLEARS the text (measured: Tab reopened it empty).
#[must_use]
pub fn question_notes(rows: &[String]) -> Option<QuestionNotes> {
    let f = frame(rows)?;
    let q = question_box(rows, f)?;
    let label = const { anchor_text("muse.question.note") }.trim_end();
    let at = (q.first..=q.last)
        .find(|&i| is_note_row(&rows[i]) && crate::text::trim(&rows[i]).starts_with(label))?;
    let mut text = crate::text::trim(&rows[at])
        .strip_prefix(label)
        .unwrap_or_default()
        .trim_start()
        .to_string();
    for r in rows[at + 1..=q.last].iter().take_while(|r| is_note_row(r)) {
        text.push(' ');
        text.push_str(crate::text::trim(r));
    }
    let text = text.trim_end_matches('▌').trim_end().to_string();
    Some(QuestionNotes { row: at, text })
}

/// The review a request of several questions, a multi-select, or a question
/// answered with a note goes to before its answers are sent
/// ([`review_box`]).
#[derive(Debug, Clone)]
struct Review {
    head: Option<usize>,
    /// The key row's first and last rows, and its words.
    keys: (usize, usize),
    key_words: String,
    /// The answer rows between the key row and the options (`    Color:
    /// Blue`, a long one wrapped onto rows four in too).
    answers: std::ops::Range<usize>,
    /// The options' rows: `Submit answers`, `Interrupt turn`.
    options: std::ops::RangeInclusive<usize>,
}

/// A review's option: `  ⟩ Submit answers` (focused) or `    Interrupt
/// turn`, unnumbered, as `(label, focused)`.
fn review_option(row: &str) -> Option<(&'static str, bool)> {
    let (focused, label) = match row.strip_prefix("  ⟩ ") {
        Some(rest) => (true, rest),
        None => (false, row.strip_prefix("    ")?),
    };
    let label = crate::text::trim(label);
    [
        anchor_text("muse.question.submit"),
        anchor_text("muse.question.interrupt"),
    ]
    .into_iter()
    .find(|&l| l == label)
    .map(|l| (l, focused))
}

/// A row of the review's answers: four columns in (`    Color: Blue`, the
/// rows a long one wrapped onto four in too), or two in under the cursor
/// (`  ⟩ Color: Blue`) where it was moved onto an answer to edit it — Enter
/// on an answer EDITS it, by the key row's words; that row is not measured.
fn is_review_answer_row(row: &str) -> bool {
    !blank(row) && (leading_spaces(row) >= 4 || row.starts_with("  ⟩ "))
}

/// THE REVIEW over the composer of frame `f` (measured on 1.4.10, and on
/// 1.4.9, [`fixtures::BOX_QUESTION_REVIEW`]): its options (`Submit answers`,
/// `Interrupt turn`) the last thing drawn above the frame's labelled rule,
/// the answers over them ([`is_review_answer_row`]), the key row `Review
/// answers before submit · Enter to edit or submit · ↑/↓ to move · Esc to
/// go back` two in over those (wrapped onto two rows in a narrow tab,
/// [`fixtures::QUESTION_MULTI_REVIEW_NARROW`]), and the question's head
/// above it.
fn review_box(rows: &[String], f: Frame) -> Option<Review> {
    let end = (0..f.top).rev().find(|&i| !blank(&rows[i]))?;
    review_option(&rows[end])?;
    let mut first = end;
    while first > 0 && review_option(&rows[first - 1]).is_some() {
        first -= 1;
    }
    let mut answers_start = first;
    while answers_start > 0 && is_review_answer_row(&rows[answers_start - 1]) {
        answers_start -= 1;
    }
    let keys_end = answers_start.checked_sub(1)?;
    let mut words = String::new();
    let mut keys_start = None;
    for k in (keys_end.saturating_sub(1)..=keys_end).rev() {
        if blank(&rows[k]) || leading_spaces(&rows[k]) != 2 {
            return None;
        }
        let t = crate::text::trim(&rows[k]);
        words = if words.is_empty() {
            t.to_string()
        } else {
            format!("{t} {words}")
        };
        if words.starts_with(const { anchor_text("muse.question.review") }) {
            keys_start = Some(k);
            break;
        }
    }
    let keys_start = keys_start?;
    if !words.ends_with(const { anchor_text("muse.question.review.back") }) {
        return None;
    }
    let head = (0..keys_start)
        .rev()
        .find(|&i| !blank(&rows[i]) && leading_spaces(&rows[i]) == 0)
        .filter(|&i| is_question_head(&rows[i]));
    Some(Review {
        head,
        keys: (keys_start, keys_end),
        key_words: words,
        answers: answers_start..first,
        options: first..=end,
    })
}

/// The review, read whole ([`PromptV2`]): a [`PromptKind::Question`] titled
/// by the question's head, its answers' rows (joined with one blank, a
/// cursor on one stripped) its description, `Submit answers` and `Interrupt
/// turn` its options — no number, no role: Enter on the first sends the
/// answers, Enter on the second stops the turn — chosen by the cursor and
/// Enter, its Esc going back to the questions. With the cursor on an answer
/// no option is focused. It follows answers no rule gives — several
/// questions', a multi-select's — or the loop's own note
/// (`aterm-agent`'s policy sends only that one).
fn review_prompt(rows: &[String], r: Review) -> PromptV2 {
    let options: Vec<Opt> = r
        .options
        .clone()
        .filter_map(|i| {
            review_option(&rows[i]).map(|(label, focused)| Opt {
                n: None,
                label: label.to_string(),
                role: Role::Other,
                focused,
                row: i,
                kept: false,
            })
        })
        .collect();
    let answers: Vec<&str> = r
        .answers
        .clone()
        .map(|i| {
            let t = crate::text::trim(&rows[i]);
            t.strip_prefix("⟩ ").unwrap_or(t)
        })
        .collect();
    PromptV2 {
        kind: PromptKind::Question,
        title: question_title(rows, r.head, r.keys.0),
        command_rows: Vec::new(),
        description_rows: 0,
        gutter: false,
        command: String::new(),
        description: answers.join(" "),
        notes: Vec::new(),
        auto_deny: None,
        path: None,
        options,
        select: Select::ArrowsEnter,
        cancel: Some(Cancel {
            key: "esc".to_string(),
            verb: "go back".to_string(),
            effect: CancelEffect::Back,
        }),
        question: Some(r.keys.0),
        span: (r.head.unwrap_or(r.keys.0), *r.options.end()),
        footer: Some(r.key_words),
        head_off_screen: r.head.is_none(),
        question_dialog: None,
        reason_rows: Vec::new(),
    }
}

/// Whether `p` is the REVIEW ([`review_box`]): its footer is the review's
/// key row.
#[must_use]
pub fn is_review(p: &PromptV2) -> bool {
    p.kind == PromptKind::Question
        && p.footer
            .as_deref()
            .is_some_and(|f| f.starts_with(const { anchor_text("muse.question.review") }))
}

/// The box on `rows`, read whole ([`PromptV2`]): the trust box, an approval
/// box, the question or its review (module header).
#[must_use]
pub fn prompt(rows: &[String]) -> Option<PromptV2> {
    if let Some(t) = trust_box(rows) {
        return Some(trust_prompt(rows, t));
    }
    if let Some(b) = approval_box(rows) {
        return Some(approval_prompt(rows, b));
    }
    let f = frame(rows)?;
    if let Some(q) = question_box(rows, f) {
        return Some(question_prompt(rows, q));
    }
    review_box(rows, f).map(|r| review_prompt(rows, r))
}

/// Where a running turn's status row can be ([`crate::reader::ScreenReader::
/// busy_zone_start`]): under this session's last message ([`scope`]), so a
/// status row quoted in an earlier reply, or left above a new banner by a
/// Muse Code killed mid-turn, never holds a wait. The whole screen where no
/// composer is drawn: a box is drawn in its place, the status row over it.
#[must_use]
pub fn busy_zone_start(rows: &[String]) -> usize {
    frame(rows).map_or(0, |f| scope(rows, f.top).2)
}

/// Whether `rows` are Muse Code's by their layout alone, for a session whose
/// program name says nothing: its composer under a rule carrying one of its
/// own labels ([`TOP_LABELS`]: `── Voice input (⌥ + v to start) ──`, the side
/// chat's) — which Claude Code's frame test takes for Claude Code's, so
/// [`crate::reader::identify`] asks this first — an approval box in that
/// frame, its trust box, or its sign-in wall. The question, the walls and
/// the cards are drawn over its composer.
pub(crate) fn is_muse_screen(rows: &[String]) -> bool {
    any_frame(rows).is_some_and(|f| labelled(&rows[f.top]))
        || approval_box(rows).is_some()
        || trust_box(rows).is_some()
        || signed_out(rows).is_some()
}

/// Both composer rules, `(top, bottom)`, when Muse Code's open composer frame
/// is on the screen.
#[must_use]
pub fn composer_rules(rows: &[String]) -> Option<(usize, usize)> {
    open_frame(rows).map(|f| (f.top, f.bottom))
}

fn muse_banner_version(row: &str) -> Option<&str> {
    if leading_spaces(row) != 2 {
        return None;
    }
    let after = crate::text::trim(row)
        .strip_prefix(const { anchor_text("muse.banner") })?
        .strip_prefix(' ')?;
    let len = after
        .bytes()
        .take_while(|b| b.is_ascii_alphanumeric() || matches!(*b, b'.' | b'-' | b'_' | b'+'))
        .count();
    let version = &after[..len];
    (!version.is_empty()
        && version.starts_with(|c: char| c.is_ascii_digit())
        && crate::text::trim(&after[len..]).is_empty())
    .then_some(version)
}

/// Muse Code's banner is not known to print its running build: no recorded
/// card of a store build has been matched to the store's version
/// (`crate::BannerBuild::ProvenanceOnly`).
pub const LAUNCH_CARD_BUILD: crate::BannerBuild = crate::BannerBuild::ProvenanceOnly;

/// The rows [`launch_card_scan`] reads of a card from its banner down: the
/// banner row alone.
pub const LAUNCH_CARD_ROWS: usize = 1;

fn muse_card_at(rows: &[String], top: usize) -> Option<CardPlace> {
    let row0 = rows.get(top)?;
    muse_banner_version(row0)?;
    let end0 = crate::text::trim_end(row0).chars().count();
    Some(CardPlace {
        top,
        rows: 1,
        text_col: 2,
        mascot_end: 0,
        ends: [end0, end0, end0],
    })
}

/// Scan `rows` for Muse Code's launch banner: `(banner_row, place)`.
#[must_use]
pub fn launch_card_scan(rows: &[String]) -> (Option<usize>, Option<CardPlace>) {
    for i in (0..rows.len()).rev() {
        if is_session_banner(rows, i) {
            return (Some(i), muse_card_at(rows, i));
        }
    }
    (None, None)
}

/// Whether [`launch_card_scan`] reads `rows` as it read the rows they were
/// before the rows `changed` names changed (the count of rows the same) —
/// known without a scan. `banner` is that reading's banner row. Whether a
/// row is a session's banner ([`is_session_banner`]) reads it and the rows
/// over it up to the nearest nonblank one, so a changed row can make or
/// unmake a banner only on itself or on the first nonblank row under it
/// (a blank row between that changed answers for itself). So the reading
/// holds while the banner's row did not change and none of those rows at
/// or under it draws a banner (the banner's own row among them is one whose
/// row over it changed); with no banner, while none of them draws one. A
/// keystroke in the composer under the card costs a banner test of its row
/// and the row under it. Never panics, on any rows and indices.
#[must_use]
pub fn launch_card_unmoved(rows: &[String], banner: Option<usize>, changed: &[usize]) -> bool {
    if banner.is_some_and(|b| b >= rows.len() || changed.contains(&b))
        || changed.iter().any(|&row| row >= rows.len())
    {
        return false;
    }
    let draws =
        |i: usize| banner.is_none_or(|b| i >= b) && rows.get(i).is_some_and(|r| is_banner_row(r));
    changed.iter().all(|&j| {
        let under = (j + 1..rows.len()).find(|&k| !blank(&rows[k]));
        !draws(j) && under.is_none_or(|k| !draws(k))
    })
}

/// The version Muse Code's launch banner at `place` names (`1.4.10`).
#[must_use]
pub fn launch_card_version<'a>(rows: &'a [String], place: &CardPlace) -> Option<&'a str> {
    muse_banner_version(rows.get(place.top)?)
}

/// The text of Muse Code's launch banner at `place`.
#[must_use]
pub fn launch_card_words<'a>(rows: &'a [String], place: &CardPlace) -> Option<[&'a str; 3]> {
    let w = crate::text::trim(rows.get(place.top)?);
    (!w.is_empty()).then_some([w, w, w])
}

/// Whether `row` is the composer's top rule at rest: the idle voice label
/// (`── Voice input (⌥ + v to start) ──…`) and no other voice state
/// (`Recording`, `Transcribing`, … are Muse Code's news, not its rest).
/// The window's harness branding replaces this label on the glass; the
/// grid keeps Muse Code's row for its readers.
#[must_use]
pub fn is_idle_voice_rule(row: &str) -> bool {
    is_rule(row) && crate::text::contains(row, const { anchor_text("muse.composer.voice") })
}

/// The newest card of each kind over the composer (`/status`'s `MUSE CODE`
/// card, `/usage`'s `Session usage` card): the ranges of their body rows,
/// newest last. A reply merely quoting a card's words has no `┌`/`└` frame
/// of its own to be read as one.
fn newest_cards(rows: &[String]) -> Vec<std::ops::Range<usize>> {
    let Some(f) = frame(rows) else {
        return Vec::new();
    };
    let mut cards = Vec::new();
    let mut i = 0;
    while i < f.top {
        if rows[i].starts_with('┌')
            && let Some(end) = (i + 1..f.top).find(|&j| rows[j].starts_with('└'))
        {
            let head = rows.get(i + 1).map_or("", String::as_str);
            if crate::text::contains(head, const { anchor_text("muse.card.status") })
                || crate::text::contains(head, const { anchor_text("muse.card.session_usage") })
            {
                cards.push(i + 1..end);
            }
            i = end + 1;
            continue;
        }
        i += 1;
    }
    cards
}

/// The newest labelled value over the cards ([`newest_cards`]): a row of
/// `label` at the label column, its value after the run of spaces.
fn card_value<'a>(rows: &'a [String], label: &str) -> Option<&'a str> {
    newest_cards(rows).into_iter().rev().find_map(|body| {
        body.rev().find_map(|j| {
            let t = card_text(&rows[j])?;
            let rest = t.strip_prefix(label)?;
            if !rest.starts_with(|c: char| c.is_whitespace()) {
                return None;
            }
            let value = crate::text::trim(rest);
            (!value.is_empty()).then_some(value)
        })
    })
}

/// The subscription tier a card names (`Subscription · Muse Code Max
/// Usage`): the words after the `·`.
fn card_plan(rows: &[String]) -> Option<String> {
    let billing = card_value(rows, const { anchor_text("muse.card.billing") });
    let line = billing.or_else(|| {
        newest_cards(rows).into_iter().rev().find_map(|body| {
            body.rev().find_map(|j| {
                let t = card_text(&rows[j])?;
                t.starts_with(const { anchor_text("muse.card.subscription") })
                    .then_some(t)
            })
        })
    })?;
    let (_, tier) = line.rsplit_once(" · ")?;
    let tier = crate::text::trim(tier);
    (!tier.is_empty()).then(|| tier.to_owned())
}

/// The signed-in address and subscription tier on Muse Code's `/status`
/// card (`ACCOUNT  Name (address)`, `BILLING  Subscription · <tier>`;
/// MEASURED signed in, [`fixtures::USAGE_SIGNED_IN`]). An API-key session
/// draws no `ACCOUNT` row: `None`, never a guess. The tier may also be
/// read off a `/usage` card's own subscription line.
#[must_use]
pub fn status_card_account(rows: &[String]) -> Option<(String, Option<String>)> {
    let account = card_value(rows, const { anchor_text("muse.card.account") })?;
    let email = account
        .strip_suffix(')')
        .and_then(|s| s.rsplit_once(" ("))
        .map(|(_, email)| crate::text::trim(email))?;
    if !email.contains('@') || email.chars().any(char::is_whitespace) {
        return None;
    }
    Some((email.to_owned(), card_plan(rows)))
}

/// The weekly subscription quota on Muse Code's `/usage` card (`Weekly
/// 71% used · Resets …`; MEASURED signed in,
/// [`fixtures::USAGE_SIGNED_IN`]), as the footer's words (`weekly 29%
/// left`). Past 100% used the quota left is none: `weekly 0% left`.
#[must_use]
pub fn usage_card_weekly(rows: &[String]) -> Option<String> {
    let weekly = card_value(rows, const { anchor_text("muse.card.weekly") })?;
    let (used, rest) = weekly.split_once('%')?;
    if !crate::text::trim(rest).starts_with("used") {
        return None;
    }
    let used: u32 = crate::text::trim(used).parse().ok()?;
    Some(format!("weekly {}% left", 100u32.saturating_sub(used)))
}

fn parse_muse_footer_model(row: &str) -> Option<(String, Option<String>)> {
    if leading_spaces(row) != 2 {
        return None;
    }
    let parts: Vec<&str> = crate::text::trim(row).split(" \u{00B7} ").collect();
    if parts.len() >= 4 {
        let model = crate::text::trim(parts[0]);
        let effort = crate::text::trim(parts[1]).to_ascii_lowercase();
        let effort = matches!(effort.as_str(), "low" | "medium" | "high" | "xhigh" | "max")
            .then_some(effort);
        if !model.is_empty() {
            return Some((model.to_owned(), effort));
        }
    } else if parts.len() == 3 {
        let model = crate::text::trim(parts[0]);
        if !model.is_empty() {
            return Some((model.to_owned(), None));
        }
    }
    None
}

/// Muse Code's launch banner and live footer model/effort for the harness footer.
#[must_use]
pub fn launch_card(rows: &[String]) -> Option<LaunchCard> {
    launch_card_read(rows).map(|read| read.card)
}

/// [`launch_card`] with the rows it was read from
/// ([`crate::LaunchCardRead`]): the banner's row where its version came from
/// the banner, and the live footer's row where the model came from there.
/// Never panics, on any rows.
#[must_use]
pub fn launch_card_read(rows: &[String]) -> Option<crate::LaunchCardRead> {
    let f = frame(rows)?;
    let footer_model = rows
        .get(f.footer)
        .and_then(|r| parse_muse_footer_model(r))
        .map(|model| (f.footer, model));
    let (_, place) = launch_card_scan(&rows[..f.top]);
    let banner = place
        .as_ref()
        .and_then(|p| Some((p.top, launch_card_version(rows, p)?)))
        .filter(|(_, version)| !version.is_empty())
        .map(|(top, version)| (top, version.to_owned()));
    let (card, banner, status) = match (banner, footer_model) {
        (Some((top, version)), Some((row, (model, effort)))) => (
            LaunchCard {
                version,
                model,
                effort,
            },
            Some(top),
            Some(row),
        ),
        (Some((top, version)), None) => (
            LaunchCard {
                version,
                model: String::new(),
                effort: None,
            },
            Some(top),
            None,
        ),
        (None, Some((row, (model, effort)))) => (
            LaunchCard {
                version: String::new(),
                model,
                effort,
            },
            None,
            Some(row),
        ),
        (None, None) => return None,
    };
    Some(crate::LaunchCardRead {
        card,
        banner,
        status,
    })
}

/// Muse Code's screens, measured, for this crate's tests and a dependent
/// crate's `#[cfg(test)]` code; not part of the documented API. Read them
/// with [`crate::prompt::fixtures::screen`].
#[doc(hidden)]
pub mod fixtures {
    /// The workspace-trust box, drawn from row 0.
    pub const BOX_TRUST: &str = include_str!("fixtures/muse-1.4.9-R6297.1-box-trust.txt");
    /// Idle at its composer, no turn yet.
    pub const IDLE: &str = include_str!("fixtures/muse-1.4.9-R6297.1-idle.txt");
    /// A typed, unsent draft.
    pub const DRAFT: &str = include_str!("fixtures/muse-1.4.9-R6297.1-draft.txt");
    /// 2 s into a turn: the status row over the open composer.
    pub const BUSY: &str = include_str!("fixtures/muse-1.4.9-R6297.1-busy-thinking.txt");
    /// A turn running with the status row's third diamond, `◈`, under seven
    /// earlier turns.
    pub const BUSY_ALT_GLYPH: &str =
        include_str!("fixtures/muse-1.4.9-R6297.1-busy-thinking-alt-glyph.txt");
    /// The end of a turn: the reply, the composer idle.
    pub const END_OF_TURN: &str = include_str!("fixtures/muse-1.4.9-R6297.1-end-of-turn.txt");
    /// Esc in a turn: the message back in the composer as its draft.
    pub const INTERRUPTED: &str =
        include_str!("fixtures/muse-1.4.9-R6297.1-interrupted-draft-restored.txt");
    /// `/` typed: the command menu between the caret row and the bottom rule.
    pub const SLASH_MENU: &str = include_str!("fixtures/muse-1.4.9-R6297.1-slash-menu.txt");
    /// One ctrl+c at an empty composer.
    pub const EXIT_CONFIRM: &str = include_str!("fixtures/muse-1.4.9-R6297.1-exit-confirm.txt");
    /// After the second ctrl+c: the resume line, then the shell.
    pub const EXITED: &str = include_str!("fixtures/muse-1.4.9-R6297.1-exited-resume.txt");
    /// Signed out: the login menu drawn instead of a composer.
    pub const SIGNED_OUT: &str = include_str!("fixtures/muse-1.4.9-R6297.1-wall-signed-out.txt");

    // 1.4.10-R6427.1, in the lab's jail against its stub of Meta's model API.

    /// The workspace-trust box, before any key.
    pub const TRUST_1_4_10: &str = include_str!("fixtures/muse-1.4.10-R6427.1-box-trust.txt");
    /// Idle at its composer, no turn yet.
    pub const IDLE_1_4_10: &str = include_str!("fixtures/muse-1.4.10-R6427.1-idle.txt");
    /// A typed, unsent draft.
    pub const DRAFT_1_4_10: &str = include_str!("fixtures/muse-1.4.10-R6427.1-draft.txt");
    /// `/` typed: the command menu.
    pub const SLASH_MENU_1_4_10: &str = include_str!("fixtures/muse-1.4.10-R6427.1-slash-menu.txt");
    /// The end of a turn: the stub's reply, the composer idle.
    pub const END_OF_TURN_1_4_10: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-end-of-turn.txt");
    /// The pane at 50 columns: a reply of two paragraphs, the second
    /// wrapped over two rows at column two, its last `every caller now.
    /// Which do you prefer (1 or 2)?`.
    pub const REPLY_WRAPPED_50_1_4_10: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-lab-reply-wrapped-50col.txt");
    /// A slow turn 3 s in: the status row over the open composer.
    pub const BUSY_1_4_10: &str = include_str!("fixtures/muse-1.4.10-R6427.1-busy-thinking.txt");
    /// Esc in that turn, 2 s later: the text received so far, `◆
    /// Interrupted · Report issues with /feedback`, and the footer's
    /// `interrupting run`.
    pub const INTERRUPTING: &str = include_str!("fixtures/muse-1.4.10-R6427.1-interrupted.txt");
    /// The same once the footer no longer says `interrupting run`.
    pub const INTERRUPTED_1_4_10: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-interrupted-settled.txt");
    /// One ctrl+c at an empty composer, the run settled.
    pub const EXIT_CONFIRM_1_4_10: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-exit-confirm.txt");
    /// After the second ctrl+c: the resume line, then the shell.
    pub const EXITED_1_4_10: &str = include_str!("fixtures/muse-1.4.10-R6427.1-exited-resume.txt");
    /// A SIGKILLed session resumed (`muse resume <uuid>`, the harness's
    /// relaunch on exit, 2026-10-17): its replayed turns (the harness's
    /// continuations among them), then Muse's own abnormal-end warning above
    /// the idle composer — the screen the relaunch types its continuation
    /// into.
    pub const RESUMED_AFTER_CRASH: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-resumed-after-crash.txt");
    /// The shell box (`touch probe.txt`), before any key.
    pub const BOX_SHELL: &str = include_str!("fixtures/muse-1.4.10-R6427.1-box-shell.txt");
    /// The shell box answered with Enter on its first option.
    pub const BOX_SHELL_APPROVED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-shell-approved.txt");
    /// The shell box answered with Esc.
    pub const BOX_SHELL_DENIED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-shell-denied.txt");
    /// The file-access box of a write under `.agents/`.
    pub const BOX_WRITE: &str = include_str!("fixtures/muse-1.4.10-R6427.1-box-write.txt");
    /// The write's box answered with Enter.
    pub const BOX_WRITE_APPROVED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-write-approved.txt");
    /// The file-access box of an edit of the same file: the write's box.
    pub const BOX_EDIT: &str = include_str!("fixtures/muse-1.4.10-R6427.1-box-edit.txt");
    /// The edit's box answered with Esc.
    pub const BOX_EDIT_DENIED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-edit-denied.txt");
    /// The question, before any key.
    pub const QUESTION: &str = include_str!("fixtures/muse-1.4.10-R6427.1-question.txt");
    /// The question after one Down.
    pub const QUESTION_DOWN: &str = include_str!("fixtures/muse-1.4.10-R6427.1-question-down.txt");
    /// The question after one Down and Enter.
    pub const QUESTION_ANSWERED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-answered.txt");
    /// The question after two Downs: `None of the above` focused.
    pub const QUESTION_NONE: &str = include_str!("fixtures/muse-1.4.10-R6427.1-question-none.txt");
    /// The question after two Downs and Enter.
    pub const QUESTION_NONE_ENTER: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-none-enter.txt");
    /// A 429 being retried: the retry row under the status row.
    pub const RATE_LIMIT_RETRY: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-rate-limit-retry.txt");
    /// The 429's final error, after 3 attempts.
    pub const RATE_LIMIT_FINAL: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-rate-limit-final.txt");
    /// A 500 being retried.
    pub const SERVER_ERROR_RETRY: &str = include_str!("fixtures/muse-1.4.10-R6427.1-500-retry.txt");
    /// The 500's final error, after 2 attempts.
    pub const SERVER_ERROR_FINAL: &str = include_str!("fixtures/muse-1.4.10-R6427.1-500-final.txt");
    /// A 401: the key refused, never retried.
    pub const KEY_REJECTED: &str = include_str!("fixtures/muse-1.4.10-R6427.1-401.txt");
    /// A 429 `usage_limit_reached`: the usage wall.
    pub const USAGE_WALL: &str = include_str!("fixtures/muse-1.4.10-R6427.1-limit-429.txt");
    /// `/usage` after a turn: its card under the reply.
    pub const USAGE_CARD: &str = include_str!("fixtures/muse-1.4.10-R6427.1-usage.txt");
    /// `/status` after that: its card under the `/usage` card.
    pub const STATUS_CARD: &str = include_str!("fixtures/muse-1.4.10-R6427.1-status.txt");

    // 1.4.11-R6667.1, signed in with a Meta account.

    /// `/status` and `/usage` at an idle composer, signed in: the status
    /// card's `ACCOUNT` and `BILLING` rows and the usage card's
    /// subscription rows (`Current`, `Weekly`, `as of`).
    pub const USAGE_SIGNED_IN: &str =
        include_str!("fixtures/muse-1.4.11-R6667.1-usage-signed-in.txt");
    /// The usage wall, its time wrapped onto `  AM`, and one `/rules` under
    /// it: three indented rows, a block of their own under a blank row (the
    /// W2 lab of 2026-10-21, the harness's own 40-row read).
    pub const USAGE_WALL_RULES_UNDER: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-limit-rules-under.txt");
    /// `/status` entered three times under the usage wall: cards alone above
    /// the open composer, the notice out of sight, each card's state
    /// `FAILED`.
    pub const USAGE_WALL_STATUS_CARDS: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-limit-status-cards.txt");
    /// `/status` entered 5 s into a slow turn: the turn's text so far, the
    /// card (`RUNNING`), and the status row under it, over the composer.
    pub const STATUS_MID_TURN: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-status-mid-turn.txt");
    /// That turn once it ended: its reply drawn whole under the card.
    pub const STATUS_MID_TURN_ENDED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-status-mid-turn-ended.txt");
    /// A session with no turn yet: `/status`, `/usage` and `/status` twice
    /// more at the empty composer, the banner out of sight, each `/status`
    /// card's state `IDLE`, the `/usage` card counting `Turns 0`.
    pub const FRESH_STATUS_CARDS: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-fresh-status-cards.txt");
    /// A first turn stopped by Esc, then `/status` three times: cards alone,
    /// each `CANCELLED` (and counting `0 turns`).
    pub const INTERRUPTED_STATUS_CARDS: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-interrupted-status-cards.txt");
    /// A `usage_limit_reached` 429 that names no reset (no `resets_at`):
    /// retried as a rate limit, then `model failed: API error 429: …` as the
    /// turn's end.
    pub const LIMIT_UNNAMED: &str = include_str!("fixtures/muse-1.4.10-R6427.1-limit-unnamed.txt");
    /// That wall, then `/status` three times: cards alone, each `FAILED`.
    pub const LIMIT_UNNAMED_STATUS_CARDS: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-limit-unnamed-status-cards.txt");
    /// A turn's reply after that, then `/status` three times: cards alone,
    /// each `COMPLETED`.
    pub const COMPLETED_STATUS_CARDS: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-completed-status-cards.txt");

    // 1.4.10-R6427.1 again, the next lab (the harness's step: its boxes,
    // typing and turn ends).

    /// The shell box asking for one stage of a sandboxed pipeline: its
    /// stage rows under the command, `Allow this stage once` and `Abort the
    /// entire command`.
    pub const BOX_SHELL_STAGE: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-shell-stage.txt");
    /// The shell box in a 60-column tab: the command wrapped between words,
    /// its rows indented four, a word longer than the row broken at its
    /// edge.
    pub const BOX_SHELL_WRAPPED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-shell-wrapped.txt");
    /// The file-access box in a 60-column tab: `write path:` alone on its
    /// row, the path under it, indented two and broken at the row's edge.
    pub const BOX_WRITE_WRAPPED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-write-wrapped.txt");
    /// The question in a 60-column tab: each description under its label,
    /// the key row wrapped onto two rows.
    pub const QUESTION_WRAPPED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-wrapped.txt");
    /// The same question at 72 columns: the descriptions in a column of
    /// their own beside the labels, the key row wrapped onto two rows.
    pub const QUESTION_WRAPPED_72: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-wrapped-72.txt");
    /// A 301-character draft, unsent: wrapped between words, its
    /// continuation rows indented two.
    pub const DRAFT_WRAPPED: &str = include_str!("fixtures/muse-1.4.10-R6427.1-draft-wrapped.txt");
    /// `keep going` and an Enter written in ONE burst, then `X`: the Enter
    /// was a newline, the draft two rows.
    pub const DRAFT_BURST_ENTER: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-draft-burst-enter.txt");
    /// A finished report: its last row `ATERM-TASK-DONE`, under an away
    /// recap (`◆ Recap: ok`) drawn above its message.
    pub const END_OF_TURN_FINISHED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-end-of-turn-finished.txt");
    /// A question answered five minutes after it was drawn: the reply, then
    /// `◆ Worked for 5m 27s · …` and `◆ Recap: ok` under it.
    pub const END_OF_TURN_WORKED_RECAP: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-end-of-turn-worked-recap.txt");

    // 1.4.9-R6297.1 in a private headless aterm at 120×40, `--base-url` at a
    // local scripted stand-in for Meta's Responses endpoint (`--model`, a
    // dummy key: no login and no model), which returns the tool call that
    // raises each box (the box is Muse Code's own).

    /// The shell box (`touch muse-approved.txt`), before any key.
    pub const BOX_COMMAND: &str = include_str!("fixtures/muse-1.4.9-R6297.1-box-command.txt");
    /// The shell box after one Down: the refusal focused.
    pub const BOX_COMMAND_FOCUS_DENY: &str =
        include_str!("fixtures/muse-1.4.9-R6297.1-box-command-focus-deny.txt");
    /// The shell box of a command wrapped onto two rows.
    pub const BOX_COMMAND_WRAPPED: &str =
        include_str!("fixtures/muse-1.4.9-R6297.1-box-command-wrapped.txt");
    /// `--approval-mode untrusted`: one stage of a command, with `Always
    /// allow in this workspace: ls ... (p)` between the allow and the abort.
    pub const BOX_COMMAND_STAGED: &str =
        include_str!("fixtures/muse-1.4.9-R6297.1-box-command-staged.txt");
    /// The second stage of two (`ls -la && pwd`), its first allowed.
    pub const BOX_COMMAND_STAGE_2_OF_2: &str =
        include_str!("fixtures/muse-1.4.9-R6297.1-box-command-stage-2-of-2.txt");
    /// The shell box answered with Esc.
    pub const COMMAND_DENIED: &str = include_str!("fixtures/muse-1.4.9-R6297.1-command-denied.txt");
    /// A turn's end under its `Ran command` row.
    pub const END_OF_TURN_RAN_COMMAND: &str =
        include_str!("fixtures/muse-1.4.9-R6297.1-end-of-turn-ran-command.txt");
    /// The question, before any key.
    pub const BOX_QUESTION: &str = include_str!("fixtures/muse-1.4.9-R6297.1-box-question.txt");
    /// The question with a note opened (Tab).
    pub const BOX_QUESTION_NOTE: &str =
        include_str!("fixtures/muse-1.4.9-R6297.1-box-question-note.txt");
    /// The first of two questions: `  1 of 2` over a key row that adds
    /// `←/→ to switch question`.
    pub const BOX_QUESTION_1_OF_2: &str =
        include_str!("fixtures/muse-1.4.9-R6297.1-box-question-1-of-2.txt");
    /// The review after both answers: the answers, `Submit answers`
    /// focused and `Interrupt turn`.
    pub const BOX_QUESTION_REVIEW: &str =
        include_str!("fixtures/muse-1.4.9-R6297.1-box-question-review.txt");
    /// A multi-select question: `[ ]` rows and a submit row.
    pub const BOX_QUESTION_MULTI_SELECT: &str =
        include_str!("fixtures/muse-1.4.9-R6297.1-box-question-multi-select.txt");
    /// A question whose answers carry a preview pane.
    pub const BOX_QUESTION_PREVIEW: &str =
        include_str!("fixtures/muse-1.4.9-R6297.1-box-question-preview.txt");
    /// The question answered with Enter: the turn's end.
    pub const QUESTION_ANSWERED_1_4_9: &str =
        include_str!("fixtures/muse-1.4.9-R6297.1-question-answered.txt");
    /// The question answered with Esc: the turn interrupted.
    pub const QUESTION_INTERRUPTED: &str =
        include_str!("fixtures/muse-1.4.9-R6297.1-question-interrupted.txt");
    /// A model's turn stopped with Esc once its reply had begun.
    pub const INTERRUPTED_MARKER: &str =
        include_str!("fixtures/muse-1.4.9-R6297.1-interrupted-marker.txt");
    /// Muse Code run again in the tab after it exited: the earlier
    /// session's rows, the shell's command line, the new banner, no turn.
    pub const IDLE_RELAUNCHED: &str =
        include_str!("fixtures/muse-1.4.9-R6297.1-idle-relaunched.txt");
    /// A fresh session whose HOME holds `~/.claude/CLAUDE.md` and no rules
    /// file of Muse Code's: its notice that it imports Claude Code's
    /// personal rules, between its banner and its composer.
    pub const IDLE_RULES_NOTICE: &str =
        include_str!("fixtures/muse-1.4.9-R6297.1-idle-claude-rules-notice.txt");

    // 1.4.10-R6427.1, the person's own build signed in, working a real task
    // in a private headless aterm (the dogfood run of 2026-10-16).

    /// The network box a sandboxed `pip install` raised: `requested by:` and
    /// the command under its title, no `full URL:` row; its session and
    /// standing grants name the destination beside their labels.
    pub const BOX_NETWORK_COMMAND: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-network-command.txt");
    /// A finished report under `◆ Worked for …` and `◆ Recap: …`, and the
    /// grey hint in the empty composer (`❯ /export saves the conversation to
    /// a text file`).
    pub const END_OF_TURN_FOOTNOTES_HINT: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-end-of-turn-footnotes-hint.txt");

    // 1.4.10-R6427.1, the lab of the harness's follow-ups: the network box,
    // the background-work panel, digits and `y` on the boxes, several
    // questions, a multi-select, the note and the review.

    /// The network box of a web fetch (the lab's own stub, loopback), before
    /// any key: its `network:` and `full URL:` rows, a one-shot allow, a
    /// session grant, an always grant and the refusal.
    pub const BOX_NETWORK: &str = include_str!("fixtures/muse-1.4.10-R6427.1-box-network.txt");
    /// The same box in a 60-column tab: the URL broken at the edge, the
    /// always grant's `(http)` carried onto a row of its own in column 0.
    pub const BOX_NETWORK_NARROW: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-network-narrow.txt");
    /// The network box of a monitor's WebSocket, the background-work panel
    /// under its bottom rule.
    pub const BOX_NETWORK_MONITOR: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-network-monitor.txt");
    /// The network box answered with the digit `4`: refused.
    pub const BOX_NETWORK_DIGIT_DENIED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-network-digit-denied.txt");
    /// The network box answered with the digit `1`: run; a `1` typed before
    /// it was drawn stands in the composer.
    pub const BOX_NETWORK_DIGIT_APPROVED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-network-digit-approved.txt");
    /// A shell box answered with the digit `2` (refused), the next box up.
    pub const BOX_SHELL_DIGIT_2: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-shell-digit-2.txt");
    /// The next shell box answered with `y` (run), the third box up.
    pub const BOX_SHELL_KEY_Y: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-shell-key-y.txt");
    /// The third shell box answered with the digit `1`: run, the turn ended.
    pub const BOX_SHELL_DIGIT_APPROVED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-shell-digit-approved.txt");
    /// A turn's end with the background-work panel (a failed monitor) under
    /// the composer's bottom rule.
    pub const END_OF_TURN_WORK_PANEL: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-end-of-turn-work-panel.txt");
    /// A turn's end with a backgrounded shell running in the panel.
    pub const END_OF_TURN_WORK_RUNNING: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-end-of-turn-work-running.txt");
    /// A fresh session's first backgrounded shell: its turn's end with the
    /// pointer `(↓ to view terminals)` drawn as a block of its own under the
    /// reply, and the panel under the bottom rule.
    pub const END_OF_TURN_WORK_HINT: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-end-of-turn-work-hint.txt");
    /// A draft over the panel, whose head is then `main` alone.
    pub const DRAFT_WORK_RUNNING: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-draft-work-running.txt");
    /// A running turn over the panel.
    pub const BUSY_WORK_RUNNING: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-busy-work-running.txt");
    /// A backgrounded shell 3 s in (2026-10-17): the turn's end, the
    /// pointer under its reply, the panel's branch `running`.
    pub const END_OF_TURN_BG_RUNNING: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-end-of-turn-bg-running.txt");
    /// The same session once that shell ended: `◆ Finished · … · ✓`, Muse
    /// Code's own turn on its output, and no panel.
    pub const END_OF_TURN_BG_FINISHED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-end-of-turn-bg-finished.txt");
    /// A call of two questions, the first drawn: `1 of 2` over the key row.
    pub const QUESTION_MULTI: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-multi.txt");
    /// The second question of the two, after Enter on the first.
    pub const QUESTION_MULTI_SECOND: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-multi-second.txt");
    /// The second question after the digit `2`: the cursor moved, nothing
    /// answered.
    pub const QUESTION_MULTI_DIGIT: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-multi-digit.txt");
    /// The first question with a note typed under its focused option.
    pub const QUESTION_MULTI_NOTE_TYPED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-multi-note-typed.txt");
    /// The two questions' review.
    pub const QUESTION_MULTI_REVIEW: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-multi-review.txt");
    /// The two questions submitted from the review.
    pub const QUESTION_MULTI_ANSWERED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-multi-answered.txt");
    /// The first of two questions in a 60-column tab.
    pub const QUESTION_MULTI_NARROW: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-multi-narrow.txt");
    /// The two questions' review in a 60-column tab: its key row on two rows.
    pub const QUESTION_MULTI_REVIEW_NARROW: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-multi-review-narrow.txt");
    /// That review with the cursor on `Interrupt turn`.
    pub const QUESTION_MULTI_REVIEW_INTERRUPT: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-multi-review-interrupt.txt");
    /// A multi-select question, before any key.
    pub const QUESTION_MULTISELECT: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-multiselect.txt");
    /// The multi-select after Enter on its first option: checked.
    pub const QUESTION_MULTISELECT_TOGGLED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-multiselect-toggled.txt");
    /// The multi-select's review.
    pub const QUESTION_MULTISELECT_REVIEW: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-multiselect-review.txt");
    /// The multi-select submitted from its review.
    pub const QUESTION_MULTISELECT_ANSWERED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-multiselect-answered.txt");
    /// The multi-select in a 60-column tab.
    pub const QUESTION_MULTISELECT_NARROW: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-multiselect-narrow.txt");
    /// The one question, the cursor moved onto `None of the above` by the
    /// digit `3`, and Tab: the note field open and empty.
    pub const QUESTION_NOTE_OPEN: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-note-open.txt");
    /// The note holding a typed text.
    pub const QUESTION_NOTE_TYPED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-note-typed.txt");
    /// The note holding the harness's long-term policy text, wrapped.
    pub const QUESTION_NOTE_LONG: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-note-long.txt");
    /// Enter on `None of the above` holding a note: the review.
    pub const QUESTION_NOTE_REVIEW: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-note-review.txt");
    /// The long-term policy text's review, its answer wrapped.
    pub const QUESTION_NOTE_LONG_REVIEW: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-note-long-review.txt");
    /// That review submitted: the whole text reached the model as its note.
    pub const QUESTION_NOTE_LONG_ANSWERED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-note-long-answered.txt");
    /// Esc in the note field holding the long text: the field closed.
    pub const QUESTION_NOTE_ESC: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-note-esc.txt");
    /// The note field holding `short words`.
    pub const QUESTION_NOTE_SHORT: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-note-short.txt");
    /// After Tab in that field and Tab again: the field open and EMPTY.
    pub const QUESTION_NOTE_TAB_CLEARED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-note-tab-cleared.txt");

    // 1.4.10-R6427.1 OUTSIDE the lab's jail, so its own managed sandbox
    // applied: a private headless aterm against a local fake backend (the
    // proxy variables at it, an lsof sampler over the process tree), the
    // fake's calls of a local MCP server's tool and of `bash` with
    // `unix_socket_paths`.

    /// The tool-action box of a local stdio MCP server's tool
    /// (`mcp__probe__ping`), before any key: `tool action: <tool>`, a
    /// one-shot allow, a session grant, an always grant and the refusal.
    pub const BOX_TOOL_ACTION: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-tool-action.txt");
    /// The same box in a 100x30 tab.
    pub const BOX_TOOL_ACTION_100X30: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-tool-action-100x30.txt");
    /// The same box in a 60-column tab: the session grant's tool cut at the
    /// row's edge.
    pub const BOX_TOOL_ACTION_NARROW: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-tool-action-narrow.txt");
    /// The tool-action box after one Down: the session grant focused.
    pub const BOX_TOOL_ACTION_DOWN: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-tool-action-down.txt");
    /// The tool-action box of a streamable-HTTP MCP server's tool
    /// (loopback): the same box.
    pub const BOX_TOOL_ACTION_HTTP: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-tool-action-http.txt");
    /// The tool-action box answered `Yes, proceed`: the tool ran once.
    pub const BOX_TOOL_ACTION_APPROVED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-tool-action-approved.txt");
    /// The next call's tool-action box answered with Esc: denied.
    pub const BOX_TOOL_ACTION_DENIED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-tool-action-denied.txt");
    /// The box a `bash` call naming a Unix socket draws: the network title
    /// over `requested by:` and the command, `Allow once` and `Reject`.
    pub const BOX_UNIX_SOCKET: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-unix-socket.txt");
    /// That box after one Down, `└ last event 39s ago` under the status row.
    pub const BOX_UNIX_SOCKET_DOWN: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-unix-socket-down.txt");
    /// That box in a 60-column tab: the command wrapped.
    pub const BOX_UNIX_SOCKET_NARROW: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-unix-socket-narrow.txt");
    /// That box answered `Allow once`: the command ran.
    pub const BOX_UNIX_SOCKET_APPROVED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-unix-socket-approved.txt");
    /// The next call's box answered with Esc: the socket refused.
    pub const BOX_UNIX_SOCKET_DENIED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-unix-socket-denied.txt");
}

#[cfg(test)]
#[path = "muse_tests.rs"]
mod tests;
