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
directory per desk, `rendezvous.toml` and, from v0.107.0, `fields.toml` (what
its desks cover, below). When they were written, that release's own consumer
adopted every desk exactly. v0.96.0 and v0.99.0 were
never cut and have no directory.

**A red guard is never fixed by regenerating or editing a fixture.** Those
bytes are what that release sends for as long as installs of it exist. Fix the
consumer so it admits them again: consumer changes only ever become more
lenient.

## The desk guard

`seamless::fixture_tests::every_shipped_producers_desk_adopts_exactly_and_proves`
(`crates/aterm-gui/src/seamless_fixture_tests.rs`) runs every desk of every
release through this build's `take_incoming`. It asks about what the desk's
`parent.toml` records and, where `parent.toml` records nothing (a record's
render debt columns, the blocks a full clear blanked, the Settings drafts),
about the producer's own bytes, and asserts that:

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
  parent's, every carried mark keeps its exact rows, columns and exit, and
  every carried block keeps the full-clear flags the parent recorded
  (`text_cleared`, `command_text_cleared`), no more and no fewer, and every
  carried shell mark still names its line: its prompt row reads as a prompt
  (every shell on these desks prompts as `dev@devhost`) and its output end is
  readable. A repainted session's marks are not read: its screen is blank until
  its program redraws; nor is the prompt row of a mark whose block a full clear
  blanked, which the live terminal reads as Evicted, unless a block no clear
  blanked prompts on that row. Which marks a clear blanked is read off the
  parent's checkpoint, never the engine under test, and that checkpoint's
  flags are first held to the producer's own meta (`s<id>.meta.json`, read
  with `aterm_json`'s value tree, never this build's decode of it);
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
parse: the count is independent of the consumer, the text is not. A record's
render debt is the rows `parent.toml` records (`render_debt`) and the columns of
the record's own `render_debt_cut` key, read off the manifest's TOML tree, never
the consumer's record. New generators also copy the column count to the optional
parent summary, which must agree with the wire; older immutable summaries omit it. v0.107.0's producer is
the first to write the key, on a session that owes columns; v0.108.0's
`engine-state` htop is the first desk record that carries one.

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
- `a_consumer_that_drops_column_debt_fails_the_exact_debt_check`, the render
  debt check's, stages the newest desk whose record carries a cut and drops the
  cut from the adopted session's debt, which no digest or proof covers.
- `a_consumer_that_moves_a_full_clears_flags_or_rows_fails_the_mark_check`,
  the mark check's, lands the newest `engine-state` desk's cleared shell and an
  uncleared one as carried, then judges, against the parent's checkpoint, an
  engine restored with the clear's flags dropped, one with every block of the
  uncleared shell flagged, and one with the cleared shell's marks and blocks
  moved down a row.
- `a_consumer_that_decodes_a_meta_without_its_clear_flags_fails_the_flag_check`,
  the decode's, hands the same cleared shell to the guard as a consumer whose
  meta reader dropped the flags would decode it: the guard must fail where it
  holds the decoded flags to the producer's meta, not later on the blank
  prompt row the clear left.
- `every_release_since_the_floor_pins_the_required_desks` checks that every
  release pins each desk in `REQUIRED_DESKS` from that desk's floor on.

## What the desks cover: `fields.toml`

The guard checks only the shapes its desks carry, so a field the producer can
write but no desk does is one the next consumer's parse is never held to.
Each release's `fields.toml` records, record by record (`manifest`, `layout`,
`meta`: every carried screen's engine state, and `control`: every `.ctl`
sidecar), which fields its desks wrote with a value and which they left
unwritten (skipped, `None`, an empty list or table), as `Struct.field`, and
which variants of each enum they reach they took and which they did not, as
`Enum::Variant`. The lists are read off the serde derives
(`seamless_fixture_cover.rs`): a field added to a carried struct, or a variant
to a carried enum, is in the next walk with no table to update. A variant is on
no list a serializer sees, so the walk reads each desk's values back with a
name no variant has in each place they wrote one, and the enum that refuses it
names all its variants, whether it is externally or internally tagged, inside
another's buffered content, or behind a lenient field that reads a refusal as
its default.

The generator writes the file beside the desks and holds them strictly. Every
field of every struct a desk reaches, and every variant of every enum a desk
reaches, must be written by some desk, with the value a real session carries,
or be one no handoff writes (below); and every field and variant the newest
release that recorded a file wrote must still be written here, whether or not
a desk here still reaches it. Anything else stops `generate_handoff_fixtures`,
in every ordinary `-p aterm-gui --lib` run and in the cut, naming each one:

```text
carried fields and variants no desk here writes (held since v0.107.0); ...
  manifest CarriedMessage.fill_permille (no desk writes it)
  meta CursorStyle::SteadyBar (no desk writes it)
```

Fix it in a desk, by giving the field the value a real session carries, or by
taking the variant as a program or the host would. A field added to a carried
struct, or a variant to a carried enum, is held from the run that adds it, so
the change that adds it also gives it a value in a desk. A struct is reached
through a field or a variant, so the only carried struct no desk reaches is one
behind a field or variant no handoff writes.

What the hold cannot see is what serde does not spell: a string is one field,
whatever it says (the engine state is a JSON string, walked as its own record;
a row's words are its own parser's), and a desk can stand for a state (a parser
stopped mid-sequence, a policy's path) that writes no field of its own. So a
release that adds such a state still adds a desk for it, and a
`REQUIRED_DESKS` row with that release as the floor once its fixtures are
checked in.

A field or variant no handoff writes (one the update's producer never sets:
written by another writer of its struct, as the crash carry's `Wire.turn_ids`,
retired, or set by nothing in this build, as DECSCL's missing levels) goes in
the generator's `NO_HANDOFF_WRITES` with why. The hold refuses a row there that
a desk writes, and one absent from both the current walk and the newest
release's written fields. A deleted field needs its retirement row only while
that newest record still lists it as written. Once a release records its
omission, remove the stale row; older immutable fixtures continue to hold the
consumer to their bytes. For example, v0.110.0 no longer records the deleted
`SessionRecord.identity` and `TerminalLeafRestore.identity` fields, whose old
keys the consumer reads past without writing them again. v0.107.0's file was
recorded after its cut, at its tag, by the same walk over these bytes.

## The other direction

The guard runs one way, an older producer's bytes into this consumer, because
an update hands only to a newer build. No update authority names a build at or
below the running one: a download staged there and an installed bundle there
mint no stage and back no activation
(`native_updater_service`'s
`no_update_authority_names_a_build_at_or_below_the_running_one`, which fails
when either comparison is made `>=`), and the one same-image hop (the QA seam,
a launch-record renewal) runs this very build. An older bundle a person puts
back starts cold; nothing hands it a session. So no shipped consumer reads this
producer's bytes, and there is no reverse hop to check.

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

- `carried-state` (v0.108.0): the state a session, its window and its desk
  carry beside the screens, which no other desk writes. The last run's window
  crashed and the desk was restored cold. Session 0 is a shell scrolled back
  12 lines whose driver set its title, icon, attention, a keyed `wait`
  escalation and Autopilot `cruise` with a deadline, set over `aterm ctl` (the
  leaf's `autopilot_by`); it is under a fleet and a local hold (which the
  status item saw), an external supervisor's lease, a working task, a spawn
  identity, its own theme and the lineage of the restores it continues. Its
  mailbox was driven through the bridge plane's verbs: a queued answer with
  `re`, `dl`, `via` and `key`, a note left behind by id (`dl` past what TOML
  writes), receipts owed on the current and an earlier bus log and one left
  behind, a ring overflowed by broadcasts on its topic, and the bridge's
  mirror took its log (`log_mirror`). Session 1, which session 0 spawned, is a
  restored tab whose Codex relaunch is still owed (its leaf's `agent`, with
  the `CODEX_HOME` its run set) and whose crash journal image was the crashed
  window's (`journal_key`). Session 2 is a worker Claude Code adopted across
  an earlier update (its ledger carried with a shed floor), in a shell an older
  build spawned (`frozen_path`): the harness pressed `keep going` and the
  person scrolled it back, so its sidecar carries `pressed`, `shed_below` and
  the differ's `below`, and its leaf the agent the host supervises, under its
  own Claude home and login. Sessions 3 to 6 are restored tabs whose Muse
  Code, agy, Gemini CLI and Claude Code relaunches are owed, the last for the
  explicit restart (`cause`, the tab it ran in); Gemini CLI is retired (agy
  replaces it), so a consumer reads session 5's leaf as no agent's and
  relaunches nothing there, and desks after v0.138.0 write it as a plain shell. Session 7's agent cannot be
  brought back (the leaf's `lost`). A second window co-views session 0. The
  first window is zoomed two steps, and its band holds a wire notice posted
  twice, a row restated a mark worse than it was posted, an unanswered
  update-health warning, a metered build, the crash's record, the worker's
  lost shell integration and its lost login waiting on the owner's sign-in (a
  standing ask with its band's short title, `short_title`). The manifest names the bridge `fabric attach` armed
  (`fabric_attached`), the fleet need the owner was told, a closed session's
  mirrored tail and that the mirror is served.
- `engine-state` (v0.108.0): seventeen sessions whose engines hold the state
  a carried screen's meta writes and no other desk does: parsers stopped in
  every carried state (a CSI parameter and intermediate, an OSC picture, a
  split UTF-8 character, an APC and a DCS), the colours pushed with OSC 30001,
  the charsets in G1 to G3, its title pushed with the icon name cleared, mouse
  modes saved, the alternate screen's own saved cursor and kitty keyboard
  stack, an open OSC 8 link, VT52 mode with an address in progress, a 1049
  screen that owes columns after a narrowing (htop drew 99 of its 100 columns
  and was narrowed to 80 with no redraw since, so its record carries
  `render_debt_cut = 19`), a shell resized back by the handback,
  shells whose integration state was revoked, held, stale or cleared, and
  Claude Code's two drawers with their cut holders.
- `held-failed` and `held-withheld` (v0.108.0): a shell beside a `make` that
  failed and was kept open, its build log in its scrollback. Under
  `held-withheld`'s successor policy (`carry = "visible"`) the pane's lines are
  left behind as the policy's choice (`history_withheld`); under
  `held-failed`'s the person dragged the divider after the park captured the
  pane, so its lines are left behind as a failure (`history_failed`). Either
  way every line is counted.
- `fold` (v0.108.0): a shell in front and a document in each of 64 windows
  behind it (the config in the Editor with two selections, notes in Markdown,
  one with a passage selected), one window past the layout's cap, so the
  layout is folded (`fold`).
- `variants` (v0.108.0): every variant of a carried enum no other desk takes.
  A demo of DEC's national sets holds four in G0 to G3 of its screen, of the
  main screen's DECSC slot and of the alternate screen's, with GL and GR
  shifted and an SS2 waiting, on a VT100 in a light-mode window; a Latin-2
  viewer holds DEC Supplemental and two 96-character sets with an SS3 waiting,
  on a VT220 with BiDi off in the config; with them, the DECSCUSR shapes,
  mouse modes and encodings, taskbar progress states, conformance levels and
  paragraph directions no other desk takes, and explicit BiDi. The last tab
  is a split focused in its second pane, beside the placeholder a pane no
  session was carried for became on the update before.

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

### The fixtures v0.108.0 writes (this tree's generator)

The generator now writes what the park does where earlier ones did not:

- Every handed session's `.ctl` sidecar carries its session log (`log`): the
  park has exported it to a newer successor since the session log's phase 2A
  (`export_for_target`), but every generator through v0.107.0 called the
  export without it, so no checked-in sidecar holds one.
- Every terminal leaf names its session's `sid` and log epoch (`log_ep`) and
  carries the operator's metadata in lockstep with the record
  (`App::view_restore_descriptor_carrying`); each record carries its log's
  `log_ids` and `log_seq`, drawn with the manifest.
- Adds `carried-state`, `engine-state`, `held-failed`, `held-withheld`,
  `fold` and `variants`, and `fields.toml` beside the desks, held strictly.

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
carries the generator that matches its producer. Its two tests run in isolated
child processes with private output in ordinary tests; the release cutter
supplies an explicit output directory and its reserved identity:
`generate_handoff_fixtures` writes every desk (each one `REQUIRED_DESKS` asks
among them) and `fields.toml`, and `generate_rendezvous_fixture` (macOS) writes
`rendezvous.toml` with the guard's own capture (`capture_transport`). Each
self-checks: the release's own consumer must adopt every desk exactly, held
panes and history imports included, and the desks must cover every field the
newest recorded release's did (`fields.toml`, above). `generate_handoff_fixtures`
then asks of every desk it wrote what the desk guard asks once the desks are
checked in (`assert_written_desks_adopt`, the guard's own per-desk check), so a
desk the guard cannot read as written fails the run of the tree that writes it,
not main after the release lands it (as v0.108.0's htop record did, 8f45f2dc7d).
Last it reads every manifest, layout and meta under the fixture root with its
desks in place of `v<version>/`, as the serde corpus's
`state_written_by_shipped_producers_answers_as_recorded` reads them once they
are checked in (`serde_corpus_tests::shipped_state_with`): every one of its own
must read back, and with `ATERM_SHIPPED_STATE_OUT` set it writes that answers
file there.

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
   --test-threads 1` in
   `target-fixture-guard` (removed after), with C's and n's pins and the
   prover's `TRUST_STAGE2_BIN`, into the release cache
   (`<clone>-release.noindex/handoff-fixtures/vX.Y.0-<n>-<C12>/`, outside the
   clone), the shipped-state answers beside `vX.Y.0/` (`shipped-state.txt`).
   A failing test, a desk `REQUIRED_DESKS` asks of the release left out, a
   missing `rendezvous.toml` (macOS), a desk not written as C and n, or no
   shipped-state answers for the release's desks refuses the release with
   nothing claimed or published. A re-run that finds
   the identity staged (the journal's `fixtures-staged`) runs nothing; a
   reservation the ledger passed, reserved anew, is written afresh.
2. Step 11 (`after-cut`), once the cut has published the release: one commit
   on origin/main's tip adds `vX.Y.0/`, its `("vX.Y.0", "<desk>")` rows in
   `PINNED_DESKS` and its cases in `../serde-corpus/shipped-state.txt`, so the
   commit that adds the desks leaves the corpus's sweep of them green (four
   releases running, v0.105.0 to v0.108.0, landed without them and turned main
   red until a hand re-record). The cases go after the case they follow in the
   staged answers, found by its id in main's file, so an answer main
   re-recorded since the cut stays as main holds it; a file that cannot take
   them (it lost that case, or records the release already) gets the desks
   alone, and the step says the answers were not recorded. The commit is
   pushed as a fast-forward of main (a compare-and-swap). A
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
commit, gives every field it adds a value in a desk (the field record refuses
it otherwise), and says what changed in a section of its own under "Producer
shapes by release". A desk it adds may also get a `REQUIRED_DESKS` row with
itself as the floor, `("vX.Y.0", "<desk>")`, once that release's fixtures are
checked in (a floor names a release with fixtures). Never move an existing row's floor, and never
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
  generate_handoff_fixtures generate_rendezvous_fixture --test-threads 1
```

The tag is annotated, so take the commit it points at, not the tag object's own
sha. Then, in the landing worktree, add the release's `("vX.Y.0", "<desk>")`
rows to `PINNED_DESKS` in `seamless_fixture_tests.rs`, record the release's
shipped-state answers (`ATERM_BLESS_SERDE_CORPUS=1 targo --unverified test -p
aterm-gui --lib -- state_written_by_shipped_producers`; check it added only
`vX.Y.0/` cases), run `targo --unverified test -p aterm-gui --lib --
fixture_tests serde_corpus_tests`, commit, land, and remove both worktrees.
