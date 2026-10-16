<!--
Copyright 2026 Andrew Yates
SPDX-License-Identifier: Apache-2.0
-->
# Handoff fixtures: what each shipped release's producer really writes

An in-session update is a handoff between two builds: the outgoing (older)
build writes it and the incoming (newer) build reads it. So the N → N+1 hop is
decided by release N's frozen producer and N+1's consumer, and nothing in N+1
can change what N writes. Every other handoff test writes and reads with the
same build, so a consumer change that refuses an older producer's bytes passes
the suite and fails in the field.

Each `v<release>/` directory holds what that release's producer wrote: one
directory per desk, and `rendezvous.toml`. When they were written, that
release's own consumer adopted every desk exactly. v0.96.0 and v0.99.0 were
never cut and have no directory.

**A red guard is never fixed by regenerating or editing a fixture.** Those
bytes are what that release sends for as long as installs of it exist. Fix the
consumer so it admits them again: consumer changes only ever become more
lenient.

## The desk guard

`seamless::fixture_tests::every_shipped_producers_desk_adopts_exactly_and_proves`
(`crates/aterm-gui/src/seamless_fixture_tests.rs`) runs every desk of every
release through this build's `take_incoming`. It asks only about what the
desk's `parent.toml` records, and asserts that:

- every session is adopted exactly: repainted only when its producer flagged
  it, each screen byte for byte at its carried geometry, each control carry
  read, and the layout placed;
- each session's meta is reassembled as the parent recorded it (the absolute
  row counters, whether an application colour diff rides it, the
  shell-integration phase, marks and completed-command count, whether the
  engine's in-flight output state rides it), and the session owes its program
  exactly the render debt its record carries, none when it carries none;
- the screen and layout digests are the ones the parent committed to, and this
  build's adoption proof and its `ProofReady` and `Commit` frames match the
  parent's byte for byte over the recorded inputs;
- every history sidecar the parent named is taken before the proof at the depth
  it stamps, and after Commit the import puts every one of its lines in front
  of the lines the checkpoint restored, cell for cell, losing only the lines the
  parent counted as left behind (`history_dropped`), with the running loss
  (`history_lost`) and the foreground holder carried. A session that names no
  sidecar imports nothing;
- after the import, in an engine whose owner allows palette changes, every
  colour the application set is live, the completed-command count is the
  parent's, and every carried shell mark still names its line: its prompt row
  reads as a prompt (every shell on these desks prompts as `dev@devhost`) and
  its output end is readable. A repainted session's marks are not read: its
  screen is blank until its program redraws;
- every unsaved Settings draft the layout carries is placed on the leaf that
  carried it, with its key and text, and none is dropped as unreadable. Whether
  this build's Settings view still has an input for the key is past what a
  fixture can see;
- every held pane is shown, in the order the manifest names it, at its carried
  geometry, with no scrollback, with how its command ended and with no link
  left to click. Where recorded, it keeps its session id, its scrollback
  sidecar is taken at the depth it stamps and imported with every link dropped
  (`assert_held_history_imports`), its control carry crosses, and it says
  whether its failure had been seen.

The guard reads history sidecars straight off the checked-in bytes with the
line codec, never through the consumer's reader, because the import is what it
checks. It counts the `[[….settings_drafts]]` tables in the layout's raw text
and reads each draft with `aterm_toml`, which also sits under the consumer's
parse: the count is independent of the consumer, the text is not.

Beside it:

- `a_consumer_that_drops_the_shell_state_fails_only_the_meta_check`, the meta
  check's negative control, stages a desk unmodified and drops session 0's
  shell state on the consumer's side, so the digests and proof still pass.
  (Deleting the key from the staged bytes would move the screen digest, and the
  control would show nothing.)
- `a_consumer_that_loses_or_moves_a_settings_draft_fails_the_drafts_check`, the
  drafts check's, stages the newest `link-dense-history` desk and changes only
  the placed layout: the draft dropped uncounted, dropped and counted
  unreadable, and the Settings tab moved to the other tab's position.
- `every_release_since_the_floor_pins_the_required_desks` checks that every
  release pins each desk in `REQUIRED_DESKS` from that desk's floor on.

## What a desk directory holds

| file | what it is |
| --- | --- |
| `seamless-<pid>-<nonce>.toml` | The manifest: the nonce on the first line, then the TOML. |
| `seamless-<pid>-<nonce>.layout.toml` | The layout sidecar. |
| `seamless-<pid>-<nonce>.s<id>.grid` / `.altgrid` | Each session's main and inactive grid sidecars. |
| `seamless-<pid>-<nonce>.s<id>.ctl` | Each session's control-carry sidecar. |
| `seamless-<pid>-<nonce>.s<id>.hist` | Each session's history-carry sidecar (v0.94.0 on, for a session whose history is deeper than its checkpoint carries). |
| `seamless-<pid>-<nonce>.h<id>.heldgrid` / `.heldalt` | Each held pane's main and inactive grid sidecars (v0.100.0 on). |
| `seamless-<pid>-<nonce>.h<id>.hist` / `.h<id>.ctl` | Each held pane's history-carry and control-carry sidecars (v0.101.0 on). |
| `s<id>.meta.json` | A copy of the meta JSON the manifest embeds for session `<id>`, for review. The guard checks it equals the embedded copy. |
| `parent.toml` | What the parent committed to (below). |

Every file is exactly as the producer wrote it, with one exception: the
manifest names each grid sidecar by its absolute path in the producer's private
directory, and the generator replaced that directory with `@HANDOFF_DIR@`
(`dir_placeholder`). The guard puts the staging directory back. No digest
covers the manifest's bytes; only the meta strings inside it are hashed, which
the substitution cannot reach, and the guard recomputes the recorded digests
from the files.

### `parent.toml`

Every release records `producer_version`, `producer_commit` and
`outgoing_build` (the release's identity, which the cut checks), `manifest`,
`nonce`, `dir_placeholder`, `screen_digest`, `layout_digest`, `files`, an
adoption proof and its two frames over fixed inputs (`proof_target_build`,
`proof_target_commit`, `proof_identities`, `proof_count`, `proof_digest`,
`ready_wire`, `commit_wire`), and a `[[session]]` row per session with its
carried geometry (`local_id`, `rows`, `cols`, `history_lines`, `alt_grid`,
`control`). Later releases add:

| from | keys | what they record | guard |
| --- | --- | --- | --- |
| v0.92.0 | `require_shell_integration_nonce`, `shell_integration_nonce` | the session's shell-integration posture | checked |
| v0.94.0 | `history_take`, `history_lost`, `fg_holder` | the lines its `.hist` sidecar carries (`0`: none), its running loss, its foreground holder | checked against the manifest, the sidecar and the import |
| v0.95.0 | `absolute_row_counter`, `alt_absolute_row_counter`, `color`, `shell_phase`, `shell_marks`, `shell_completed_seq` | the row numbering the successor continues, whether a colour diff rides the meta, the shell-integration state | checked against the reassembled checkpoint |
| v0.97.0 | `park_carry_ceiling` (top level), `rung`, `links_stripped` | the ceiling the park captured under, the `CarryRung` (`full`, `stripped-links`, and on a policy desk `visible-only` or `repaint`), the lines that lost their OSC 8 links | not read |
| v0.97.0 | `repaint`, `history_dropped` (policy desks) | flagged for a redraw; the history lines this hop leaves behind | checked; absent means none |
| v0.98.0 | `history_links_stripped` | the `.hist` lines written without their OSC 8 links | not read |
| v0.100.0 | `[[held]]`: `local_id`, `rows`, `cols`, `alt_grid`, `exit_code` or `exit_signal` | each held pane and how its command ended | checked |
| v0.101.0 | `output_state`, `render_debt` | whether the meta carries the in-flight output state; the rows owed a carried alternate screen (`"<shift>,<pushed>"`, absent for none) | checked |
| v0.101.0 | `[[held]]`: `sid`, `history_take`, `history_dropped`, `control`, `failure_seen` | what a held pane carries beside its screen | checked |

## The desks

Each desk is listed with the first release that has it; later releases write
the same sessions with their own producer. `REQUIRED_DESKS` makes every desk
mandatory from v0.97.0 on (`link-dense-history` from v0.98.0, `held-pane` from
v0.100.0).

- `incident-55x149` (v0.91.0): session 0 is Claude Code in 1049, entered from a
  prompt on the last row of a 56-row grid, after which the message band took
  one row; through v0.101.0 its DECSC slot is on row 55 of a 55-row grid.
  Session 1 is a shell at the same size with 256 lines of styled history.
- `claude-code-1049` (v0.91.0): a Claude Code transcript in 1049 with a turn in
  its ledger (a real control carry), split beside a shell.
- `history` (v0.91.0): three shells whose history has SGR 256 and truecolour,
  curly underline colour, wide CJK, emoji, combining marks, OSC 8 links, OSC 7
  directories, bracketed paste, kitty keyboard, a scroll region and edited tab
  stops.
- `twelve-panes` (v0.91.0): twelve sessions in two windows: a 2x2 tab, a
  three-column tab, splits, two editors in 1049, Claude Code in a pane,
  per-record metadata and connection triples.
- `shell-integration` (v0.92.0): two shells, each signing its OSC 133/633 marks
  with its own tab's nonce and requiring it, one of them in 1049 with an
  editor. From v0.94.0 the records carry `rekey` and `loader`; from v0.95.0 the
  meta carries the shell state.
- `history-carry` (v0.93.0): a shell with 1,500 lines of history and a shell
  with 1,000 lines running `less` in 1049, whose checkpoint carries none of it.
  v0.93.0 carries only the screens (256 lines and none); from v0.94.0 each
  names a `.hist` sidecar, and the records carry a running `history_lost` from
  an earlier handoff, `rekey` and `loader`, and a `questions` word, which is
  also on its layout leaf.
- `stalled-sequence` (v0.93.0; not in v0.94.0 or v0.95.0, whose generators
  required every parser at Ground): three sessions whose parsers stopped
  mid-sequence over a quiet PTY: a shell after an unterminated OSC title
  (`printf '\e]0;building'`, `OscString`), an ssh session halfway through an
  SGR (`CsiParam`), and an editor in 1049 whose kitty-graphics upload stopped
  inside its APC payload (`SosPmApcString`).
- `colour-and-shell` (v0.95.0): a shell with aterm's signed integration, a
  base16 theme (OSC 4 with `allow_palette_reconfigure` on, and OSC 10/11/12),
  150 finished commands, so its history is deeper than its checkpoint carries,
  and a `cargo build` still running. Beside it, a shell whose own script sends
  unsigned marks, running vim in 1049, which pushed the colours (OSC 30001) and
  set its background, cursor and selection colours; its checkpoint carries none
  of the shell's history, and its marks sit on the saved primary.
- `link-dense` (v0.97.0): a shell that listed two CI runs' signed log links:
  one row of twelve 8 KiB links in its history, another on screen, and a row
  of two links under the per-record cap (`16 KiB + cols * 512`) that keeps
  them. Its history fits its checkpoint, so it names no sidecar. Beside it, a
  shell with 600 lines of history that printed a row of twelve such links and
  then opened vim in 1049: the row is on the saved primary, and the `.hist`
  sidecar carries the saved primary's history. Both are carried at
  `stripped-links` (two lines and one). The desk keeps every over-cap line out
  of the `.hist` sidecars, because v0.97.0's own consumer refuses a sidecar
  frame holding one. v0.94.0 through v0.97.0 write such sidecars; the
  consumer's leniency for them is held by `handoff_history_tests`'
  `a_link_dense_line_a_shipped_producer_wrote_costs_only_its_links`, not by
  these fixtures.
- `policy-visible` (v0.97.0): parked under a successor policy of
  `carry = "visible"`. A shell with 1,500 lines of history and a running loss
  of 12, a shell under aterm's signed integration with a `cargo build` still
  running (its marks on the visible rows), and Claude Code in 1049 with a turn
  in its ledger. Every session is carried at `visible-only`, keeps its control
  carry, names no sidecar, and counts every line of history as dropped.
- `policy-repaint` (v0.97.0): parked under `carry = "repaint"`. A shell with
  1,500 lines of history, vim in 1049 (so its blank carry has an inactive
  grid), a shell stalled inside an unterminated OSC title, and Claude Code in
  1049. Every session is in the repaint set, carries a blank canonical screen
  and no history (and, before v0.100.0, no control carry), and counts every
  line it leaves behind.
- `link-dense-history` (v0.98.0): session 0 listed two CI runs' links 400 lines
  apart. The first dense row is older than the 256 lines its checkpoint
  carries, so it is in the part of the `.hist` sidecar the successor imports
  (`history_take = 480`); the second is inside those 256 lines, stripped by the
  screen carry (`stripped-links`, one line) and held stripped in the sidecar
  past its take. A row of two links keeps them. Session 1, with 500 lines of
  history, printed a dense row and 80 more lines, then opened vim in 1049; it
  is carried at `full`, and its sidecar carries the dense row stripped. The
  window's second tab is Settings (`/text-fonts`) holding an unsaved
  `font_family` draft.
- `held-pane` (v0.100.0): two live shells, each beside a pane kept open by
  `--hold` after its command exited. `make` (pane 2) failed with status 2 after
  printing a link to its CI log; vim (pane 3) was killed by SIGKILL in 1049, so
  its held screen has an inactive grid too. Neither has a session record or a
  PTY: the manifest's `held` list names each one's screen and exit in sidecars
  outside every digest, and the layout names each pane by its id. The
  successor shows each read-only with every link stripped; an older successor
  skips the list and keeps its placeholder. From v0.101.0 each pane also names
  a `.h<id>.hist` sidecar (11 and 12 lines, none left behind) and a `.h<id>.ctl`
  sidecar; `make`'s failure, in the tab in front, had been seen and vim's had
  not.

## Producer shapes by release

What each release's producer changed that reaches these bytes. The
hand-adapted generators that wrote v0.91.0 through v0.102.0 are in git history
(`generator*.rs.txt` in this directory, last at 86b918a4e); from v0.103.0 the
release's cut runs the in-tree generator ("How a release adds its fixtures").

### The fixtures v0.91.0 wrote

Grid, control and layout sidecars; no history carry.

### The fixtures v0.92.0 wrote

Each checkpoint carries its shell's shell-integration nonce, and the window
carry holds the unified messages instead of status bars. Adds
`shell-integration`.

### The fixtures v0.93.0 wrote

- The capture is the producer ladder: `carry_for_wire` for each session, then
  `settle_wire_carries`. Every session here is carried at `CarryRung::Full`.
- A parser left mid-sequence over a quiet PTY is carried with the partial
  sequence left out, as CAN would
  (`Terminal::checkpoint_carry_abandoning_partial`); the live parser is left as
  it was. Earlier releases refuse to hand off such a session. Adds
  `stalled-sequence`.
- No history carry, and none of the record and leaf fields v0.94.0 adds. Adds
  `history-carry` with its screens only.

### The fixtures v0.94.0 wrote

- Every record carries the foreground holder the park reads (`fg_holder`): the
  shell's pid at a prompt, else the job's.
- The history carry: a session whose history is deeper than its checkpoint
  carries (256 lines, or none under an alternate screen) names a `.s<id>.hist`
  sidecar on its record (`history = "<len> <sha> <take>"`).
- Records and layout leaves add `questions`, `rekey`, `loader`, `history`,
  `history_dropped` and `history_lost`.

### The fixtures v0.95.0 wrote

The meta (`CheckpointMeta`) adds:

- `absolute_row_counter` and, with an inactive grid, `alt_absolute_row_counter`:
  the successor continues the parent's row numbering;
- `color`: the colours an application set, as a diff from the configured theme
  (OSC 4 palette entries, OSC 10/11/12/17/19), and the XTPUSHCOLORS stack.
  Absent when nothing is overridden;
- `shell`: the OSC 133/633 phase, the command marks and output blocks still
  readable in the carried history, the one being built, and the counters. A
  command running across the handoff completes on the successor.

The layout leaf gains `agent`, which a handoff layout never fills. Adds
`colour-and-shell`.

### The fixtures v0.97.0 wrote

- The signed handoff policy. Every park reads the successor's policy
  (`App::park_policy`) and captures under its ceiling (`carry_for_wire_within`,
  `HistoryPlan::deferred_under`). Releases ship `publish/handoff-policy.toml`
  empty, so every desk but the policy desks is captured under the `Full`
  ceiling (`park_carry_ceiling = "full"`). Under a ceiling that carries no
  scrollback, no `.hist` sidecar is named and every line beyond the
  checkpoint's is counted (`history_dropped`, and added to `history_lost`). A
  `repaint` carry is a
  blank canonical screen whose scalars come from
  `Terminal::carry_meta_abandoning_partial`. Adds `policy-visible` and
  `policy-repaint` (written by a second generator,
  `generator-v0.97.0-policy.rs.txt`).
- The link-stripping rung: a line record over the per-record cap costs that
  line its OSC 8 links (`CarryRung::StrippedLinks`), not the screen, and keeps
  the control carry. Adds `link-dense`.
- The capture walks the pool in session-id order. `stalled-sequence` returns;
  its title shell and its editor's saved primary now name `.hist` sidecars.

### The fixtures v0.98.0 wrote

- The history export drops the OSC 8 links of every line over the per-record
  cap before it writes the frame, so every frame of a sidecar decodes under the
  strict caps and the line keeps its text, attributes and wrap flag.
- A record's `history_withheld` says the successor's policy asked for no
  scrollback; it is written only when `true` (the policy desks' sessions that
  leave lines behind).
- The handoff layout carries Settings drafts: `App::capture_handoff_layout`
  puts each Settings view's unsaved field drafts on its leaf
  (`settings_drafts`). Adds `link-dense-history`.

Through v0.98.0 every layout holds `restored_tabs` only.

### The fixtures v0.100.0 wrote

- The parser carry: a partial escape sequence rides the meta (`parser`) and the
  successor continues it; only a hooked DCS is still abandoned.
  `stalled-sequence`'s sessions carry `OscString`, `CsiParam` and
  `SosPmApcString`.
- Every session's control carry travels: the `repaint` rung drops only the
  differ's state, so every `policy-repaint` session names a `.ctl` sidecar.
- Records carry `claim_known = true` and a `timeline_id`, and the manifest a
  `roster_seq`.
- Layouts carry the legacy mirrors (`tabs`, `native_tabs`, `tab_order`) beside
  `restored_tabs` and, on macOS, the window's `show` state.
- Held panes: the manifest's `held` list and `.h<id>.heldgrid` / `.heldalt`
  sidecars. Adds `held-pane`.

### The fixtures v0.101.0 wrote

- The meta carries the engine's in-flight output state (`output_state`: the REP
  character, an SGR 58 underline colour, an open OSC 8 link, a VT52 address in
  progress, a pending ZWJ, the XTPUSHSGR stack, the XTSAVE'd modes). Every
  session that printed anything has a REP character, so every session carries
  it.
- A record carries the render debt, the rows the engine owed a carried
  alternate screen (`render_debt`). `incident-55x149`'s Claude Code, resized in
  1049 from 56 rows to 55 and never fully redrawn, owes `-1,0`; no other
  session owes anything.
- Held panes carry their scrollback and control carry (`.h<id>.hist`,
  `.h<id>.ctl`), their `sid` and `failure_seen`. v0.101.0's own consumer kept
  held-pane links clickable, so the held import's link dropping is checked
  against this build's consumer only.
- The manifest carries `next_turn_id` (the ids minted plus `TURN_ID_RESERVE`);
  each layout window and the window carry name the raw inner size
  (`inner_px`); the window carry says when its messages were captured
  (`messages_captured_unix_ms`).
- The policy desks' successor seals its rescue as a `[[ranges]]` table naming
  the release's build, the only shape a cut seals
  (`PolicyFile::parse_for_cut`).

### The fixtures v0.102.0 wrote

Grid, control, history and layout sidecars are byte for byte v0.101.0's; only
the metas, and with them the manifests, digests and proof, moved:

- Every meta, the held panes' included, carries `"win32_input_enabled":false`
  (`TerminalModes::win32_input_enabled`), which a Unix session's posture leaves
  off.
- The DECSC slot follows its text across a resize (`Grid::lend_saved_cursor`).
  `incident-55x149`'s `saved_cursor_main` is now row 54; every earlier release
  carries the slot past the grid, so the guard keeps checking that this build
  admits it.

`parent.toml` records nothing new.

## The rendezvous transport

On macOS the successor gets its PTY masters and the readiness and Commit pipes
over the rendezvous (`crates/aterm-gui/src/handoff_rendezvous.rs`), not on
disk. The successor dials a socket the parent bound and claims with a frame
(`ATRZ1C` and the claim secret); the parent answers with a grant, an `ATRZ1G`
header carrying every descriptor through `SCM_RIGHTS`, then a body naming each
session. A change to a magic, the header, the body or the descriptor order
passes every same-build test and fails every update, so each release directory
holds `rendezvous.toml`: what that release's transport does over fixed inputs
(`claim_input`, `grant_nonce`, `grant_sessions`, and the proof's
`proof_layout_digest` and `proof_screen_digest`).

| key | what it is |
| --- | --- |
| `claim_frame_hex` | The claim that release's dialer (`dial_for_test`, which writes `claim_frame`) writes for `claim_input`, read off a plain listener. |
| `grant_header_hex`, `grant_body` | The grant its parent sends: a real `Rendezvous::bind`, a real claim, `accept_claim`, then `transfer` with three `openpty` masters and two pipes, received by a real `recv_with_fds`. |
| `descriptor_order` | The order the descriptors arrived in: a master identified by its `st_rdev`, a pipe end by its inode. |
| `env_rendezvous`, `rendezvous_file_shape`, `env_claim`, `claim_hex_len` | The launch variables' names, the rendezvous socket's file name (pid and nonce elided) and the claim secret's length. |
| `env_proof_term`, `proof_term_launched`, `proof_term_fork` | The proof-term variable and the two values the parent writes, read from the match arms of that release's `app_update_handoff.rs`. |
| `env_target`, `target_value` | `ATERM_SEAMLESS_TARGET` and its value from that release's `encode_target_identity`, for its own build and commit. |
| `env_parent_birth`, `parent_birth_value` | `ATERM_HANDOFF_PARENT_BIRTH` and the value that release's `outgoing_parent_env` published for the generator's process. Only its shape is read. |
| `proof_terms`, `proof_count`, `proof_digest` | The masters' PTY device terms (`proof_identities_in_device_terms`), and that release's `adoption_proof` over them, the target in `target_value` and the fixed digests. |

The claim secret's key is `claim_input`, not `claim_secret`: the public
export's gitleaks scan (built-in rules only; the publication engine refuses
repo-local allowlists) reads `…secret = "<hex>"` as a leaked API key. The
value is a fixed test input.

The guard is `seamless::fixture_tests::every_shipped_producers_rendezvous_grants_this_successor`
(macOS). For every release `PINNED_DESKS` pins, it asserts that:

- this build's claim for the secret is the recorded frame, and this build's
  grant for the nonce and sessions (captured by the same code the generator
  runs) is the recorded header, body and descriptor order, so a grant of up to
  62 sessions keeps its shape byte for byte;
- this build's successor (`dial_and_claim`), dialled at a stub parent that
  checks its claim against the recorded frame and replays the recorded header
  and body with fresh masters and pipes in the recorded order, pairs every
  `local_id` in its `ATERM_SEAMLESS_FDS` wire with the master whose `st_rdev`
  matches the one sent for it, and takes the readiness and Commit ends it was
  sent;
- the environment names are this build's, and the recorded target, birth
  record and proof-term values parse here (`parse_target_identity`,
  `ProcessBirth::from_wire`, `parse_proof_term`), read from the strings, never
  re-encoded;
- this build's `adoption_proof` over the recorded terms and target is the
  recorded proof.

## How a release adds its fixtures

`targo --unverified ship release` does it; there is no hand step.

The generator is `crates/aterm-gui/src/seamless_fixture_generator_tests.rs`,
mounted as `seamless::carry_tests::fixture_generator` and compiled into every
`targo --unverified test -p aterm-gui --lib` build. It calls the release's own
policy reader, capture ladder, history carry, writer and consumer, so a
producer change that breaks it fails to compile, and each release's tree
carries the generator that matches its producer. Its two tests are
`#[ignore]`d and run only by name: `generate_handoff_fixtures` writes every
desk in `REQUIRED_DESKS`, and `generate_rendezvous_fixture` (macOS) writes
`rendezvous.toml` with the guard's own capture (`capture_transport`). Each
self-checks: the release's own consumer must adopt every desk exactly, held
panes and history imports included.

Nothing in it is edited per release. Every release input is derived at run
time:

- `ATERM_BUILD_GIT_COMMIT` (12 digits), `ATERM_BUILD_GIT_COMMIT_FULL` and
  `SOURCE_DATE_EPOCH`: the release commit the tag names and the ledger's build
  number, the pins the release build hands `crates/aterm/build.rs`
  (`aterm-release`'s `buildplan`). `fx_install_release_stamp` installs the
  stamp the shipped binary carries, so `producer_version`, `producer_commit`
  and each manifest's `outgoing_build` are the release's. It refuses to run
  without them.
- The version: the tree's `[workspace.package] version`.
- The stamp's compiler: `trustc -vV` of `$TRUST_STAGE2_BIN` (the release's
  prover), else of the `trustc` on PATH; the `release` profile, the
  `--unverified` lane's `off` and `release_build`. None of these reaches a
  fixture byte.
- The successor: a build a day newer than the release (`fx_target_build`,
  `proof_target_build`), at the next MINOR (`fx_successor_version`, the
  version the update row's title names), verified an hour after it was built.
- The policy desks' `[[ranges]]` rescue names the release's own build.

The cut (`crates/aterm-release/src/fixtures.rs`):

1. Step 6 (`build`), beside `ship build`, once the step has reserved the
   release's identity (build n and the release commit C) and before anything
   is published or claimed: both tests run in the clone at R (C is R plus the
   ledger line and the rolled changelog), `targo --unverified test -p
   aterm-gui --lib -- generate_handoff_fixtures generate_rendezvous_fixture
   --ignored --test-threads 1` in
   `target-fixture-guard` (removed after), with C's and n's pins and the
   prover's `TRUST_STAGE2_BIN`, into the release cache
   (`<clone>-release.noindex/handoff-fixtures/vX.Y.0-<n>-<C12>/`, outside the
   clone). A failing test, a desk `REQUIRED_DESKS` asks of the release left
   out, a missing `rendezvous.toml` (macOS), or a desk not written as C and n
   refuses the release with nothing claimed or published. A re-run that finds
   the identity staged (the journal's `fixtures-staged`) runs nothing; a
   reservation the ledger passed, reserved anew, is written afresh.
2. Step 11 (`after-cut`), once the cut has published the release: one commit
   on origin/main's tip adds `vX.Y.0/` and its `("vX.Y.0", "<desk>")` rows in
   `PINNED_DESKS`, pushed as a fast-forward of main (a compare-and-swap). A
   lost race rebuilds the commit on the new tip, up to five tries, as the
   ledger claim does; any other push failure refuses the step, and running the
   release again lands what is staged. A tip whose fixtures for the release
   already pass the next cut's gate is left alone. A tip holding `vX.Y.0/` in
   a shape that gate refuses is someone's work and is never overwritten. If
   the cache lost the staging, step 11 writes it again first, from the clone,
   now at C (the same producer).

`--dry-run` says what both steps would do. `PINNED_DESKS` is the only table a
release adds rows to: the rendezvous guard pins the same releases.

A release that changes its producer's shape changes the generator in the same
commit, adds a desk for the shape to `REQUIRED_DESKS` with itself as the floor,
`("vX.Y.0", "<desk>")`, and says what changed in a section of its own under
"Producer shapes by release". Never move an existing row's floor, and never
give a new row an earlier floor than the release that first writes it: the
fixtures of the releases before it are frozen and can never gain the desk.
Build each layout the way `App::capture_manifest` does (`fx_window`), and give
every record field the release adds a value a real session would carry.

### By hand

For a release whose cut did not land its fixtures (one cut by an older
cutter, or one whose `after-cut` step refuses and names this section), run the
release's own generator at its tag, in a worktree of its own, never in the
shared checkout:

```sh
git worktree add --detach ~/aterm-vX.Y.0.noindex vX.Y.0
git -C ~/aterm-vX.Y.0.noindex submodule update --init vendor/astream
git worktree add -b test/handoff-fixtures-vXY0 ~/aterm-wt-fxXY0 origin/main
cd ~/aterm-vX.Y.0.noindex
OUT=~/aterm-wt-fxXY0/crates/aterm-gui/tests/fixtures/handoff/vX.Y.0
ATERM_BUILD_GIT_COMMIT=$(git rev-parse --short=12 'vX.Y.0^{commit}') \
ATERM_BUILD_GIT_COMMIT_FULL=$(git rev-parse 'vX.Y.0^{commit}') \
SOURCE_DATE_EPOCH=<the release's build number in RELEASES.ledger> \
ATERM_HANDOFF_FIXTURE_OUT=$OUT ATERM_RENDEZVOUS_FIXTURE_OUT=$OUT \
  targo --unverified test -p aterm-gui --lib -- \
  generate_handoff_fixtures generate_rendezvous_fixture --ignored --test-threads 1
```

The tag is annotated, so take the commit it points at, not the tag object's own
sha. Then, in the landing worktree, add the release's `("vX.Y.0", "<desk>")`
rows to `PINNED_DESKS` in `seamless_fixture_tests.rs`, run `targo --unverified
test -p aterm-gui --lib -- fixture_tests`, commit, land, and remove both
worktrees.
