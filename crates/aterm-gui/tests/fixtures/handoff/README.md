<!--
Copyright 2026 Andrew Yates
SPDX-License-Identifier: Apache-2.0
-->
# Handoff fixtures: what each shipped release's producer really writes

An in-session update is a handoff between two builds. The outgoing (older)
build writes the handoff and the incoming (newer) build reads it. So the
N → N+1 hop is decided by release N's frozen producer and N+1's consumer.
Nothing in N+1 can change what N writes.

Every other handoff test writes and reads with the same build. A consumer
change that refuses an older producer's bytes therefore passes the suite and
fails in the field.

Each directory here holds the bytes a released producer wrote, one directory
per release and one per desk inside it. The guard
`seamless::fixture_tests::every_shipped_producers_desk_adopts_exactly_and_proves`
(`crates/aterm-gui/src/seamless_fixture_tests.rs`) runs every fixture of every
release through this build's `take_incoming`. It asserts that:

- every session is adopted exactly: repainted only when its producer flagged
  it (a `carry = "repaint"` policy desk, v0.97.0 on), each screen
  byte-for-byte at its carried geometry, each control carry read, and the
  layout placed;
- every unsaved Settings draft the layout carries (v0.98.0 on) is on the
  placed layout, on the leaf that carried it, with its key and text, and none
  is dropped as unreadable. A layout that carries no draft, which is every
  layout before v0.98.0, places none. The check stops at the placed layout:
  whether this build's Settings view still has an input for the draft's key
  when the restore reopens it is past what a fixture can see;
- every held pane the parent carried (v0.100.0 on: a pane kept open by
  `--hold` after its command exited, in the layout but never handed) is
  shown, in the order the manifest names it, at its carried geometry, with no
  scrollback, with how its command ended and with no link left to click. A
  desk whose producer carried none, which is every desk before v0.100.0,
  shows none. From v0.101.0 each held pane also keeps the session id it was
  known by, its scrollback sidecar is taken at the depth it stamps, its
  control carry crosses, and it says whether its failure had been seen;
- each session's meta is reassembled as its parent recorded it (v0.95.0 on):
  the absolute row counters, whether an application colour diff rides it,
  and the shell-integration phase, marks and completed-command count, and
  (v0.101.0 on) whether the engine's in-flight output state rides it;
- each session owes its program exactly the render debt its record carries
  (v0.101.0 on: the rows the parent's engine owed a carried alternate
  screen), and a session whose record carries none, which is every session
  before v0.101.0, owes nothing;
- the screen and layout digests equal what that release's parent committed to;
- this build's adoption proof and its `ProofReady` and `Commit` frames match
  the parent's, byte for byte, over the recorded inputs;
- every history sidecar the parent named (v0.94.0 on) is taken before the
  proof at the depth it stamps, and after Commit the import puts every one of
  its lines back: the session's history is then the sidecar's lines in front
  of the lines its checkpoint restored, line for line and cell for cell, with
  nothing lost at the hop but the lines the parent itself counted as left
  behind (`history_dropped`, v0.97.0 on), and the session's running loss
  (`history_lost`) and foreground holder carried. A session that names no
  sidecar, which is every session of a release before the history carry,
  imports nothing and keeps exactly the history its checkpoint restored;
- after the import (v0.95.0 on), in an engine whose owner opted into palette
  changes, every colour the application set is live, the completed-command
  count is the parent's, and every carried shell mark still names its line:
  its prompt row reads as a prompt (every shell on these desks prompts as
  `dev@devhost`) and its output end is readable. A repainted session's marks
  are not read, because its screen is blank until its program redraws.

`a_consumer_that_drops_the_shell_state_fails_only_the_meta_check` is the
meta check's negative control. It stages a desk unmodified, so both digests
and the proof are the parent's own, and then drops session 0's shell state on
the consumer's side, as a consumer that stopped reading the `shell` key
would. Every other check still passes, and the meta check fails it. (Deleting
the key from the staged bytes instead would move the screen digest, so the
digest and proof checks would catch it and the control would show nothing.)
`a_consumer_that_loses_or_moves_a_settings_draft_fails_the_drafts_check` is
the drafts check's negative control. It stages the newest `link-dense-history`
desk unmodified and changes only the placed layout: the draft dropped
uncounted, the draft dropped and counted unreadable, and the Settings tab moved
to the other tab's position, which keeps every draft's key and text on the
layout. The drafts check fails each one.
`every_release_since_the_floor_pins_the_required_desks` checks that every
release pins each desk in `REQUIRED_DESKS` from that desk's own floor on.

The guard reads the sidecar's lines itself, straight off the checked-in
bytes with the line codec. It does not use the consumer's reader, because the
import is what it checks. It reads the Settings drafts the same way: it counts
the `[[….settings_drafts]]` tables in the layout's raw text and takes each
draft's leaf and text from a plain TOML reading of it, never from the
consumer's parse, sanitize or bound. That reading is `aterm_toml`, the library
under the consumer's parse too, so the count is independent of the consumer
and the text is not.

**A red guard is never fixed by regenerating or editing a fixture.** Those
bytes are what that release sends, for as long as installs of it exist. Fix
the consumer so it admits them again. Consumer changes may only become more
lenient (law L3 of the 2026-09-22/23 audit).

## What a desk directory holds

| file | what it is |
| --- | --- |
| `seamless-<pid>-<nonce>.toml` | The manifest as the producer wrote it: the nonce on the first line, then the TOML. |
| `seamless-<pid>-<nonce>.layout.toml` | The layout sidecar, exactly as written. |
| `seamless-<pid>-<nonce>.s<id>.grid` / `.altgrid` | Each session's main and inactive grid sidecars, exactly as written. |
| `seamless-<pid>-<nonce>.s<id>.ctl` | Each session's control-carry sidecar, exactly as written. |
| `seamless-<pid>-<nonce>.s<id>.hist` | Each session's history-carry sidecar, exactly as written (from v0.94.0, for a session whose history is deeper than its checkpoint carries). |
| `seamless-<pid>-<nonce>.h<id>.heldgrid` / `.heldalt` | Each held pane's main and inactive grid sidecars, exactly as written (from v0.100.0, for a pane kept open after its command exited). |
| `seamless-<pid>-<nonce>.h<id>.hist` / `.h<id>.ctl` | Each held pane's history-carry and control-carry sidecars, exactly as written (from v0.101.0). |
| `s<id>.meta.json` | A copy of the meta JSON the manifest embeds for session `<id>`, extracted so it can be reviewed. The guard checks that it equals the embedded copy. |
| `parent.toml` | What the parent committed to: the screen and layout digests, an adoption proof and its two frames over fixed inputs (`proof_*`), the list of files, and each session's carried geometry. From v0.94.0 each session row also records `history_take` (the lines its `.hist` sidecar carries), `history_lost` and `fg_holder`, and from v0.95.0 `absolute_row_counter` and `alt_absolute_row_counter` (the numbering the successor continues), `color` (whether an application colour diff rides the meta) and `shell_phase`, `shell_marks` and `shell_completed_seq` (the shell-integration state it carries). From v0.97.0 the parent also records `park_carry_ceiling` (the ceiling the park captured under, from the successor's handoff policy), and each session row `rung` (the `CarryRung` the ladder carried it at: `full` or `stripped-links`, and on a policy desk `visible-only` or `repaint`) and `links_stripped` (how many of its lines lost their OSC 8 links), and from v0.98.0 `history_links_stripped` (how many lines of its `.hist` sidecar the export wrote with their OSC 8 links dropped). The policy desks (v0.97.0's, which `generator-v0.97.0-policy.rs.txt` wrote, and v0.98.0's, which `generator.rs.txt` wrote) add `repaint` (whether the producer flagged the session for a redraw) and `history_dropped` (the lines of its history this hop leaves behind, counted) on each row, and v0.98.0's parents record `park_carry_ceiling` as the policy's `visible` or `repaint`. The guard checks the v0.94.0 fields (`history_take`, `history_lost`, `fg_holder`) against the manifest and the sidecar's bytes, and against what this build takes and imports; the v0.95.0 fields against the checkpoint this build reassembles from the meta; and `repaint` and `history_dropped` against what this build adopts and imports (a row without them repaints nothing and drops nothing). It does not read `park_carry_ceiling`, `rung`, `links_stripped` or `history_links_stripped`. From v0.100.0 the parent also records each held pane (`[[held]]`): its `local_id`, `rows`, `cols`, `alt_grid` and how its command ended (`exit_code` or `exit_signal`), which the guard checks against what this build shows. From v0.101.0 each session row also records `output_state` (whether the meta carries the engine's in-flight output state) and, for a session that owes it, `render_debt` (`"<shift>,<pushed>"`), and each held pane `sid`, `history_take` (the lines its `.h<id>.hist` sidecar carries), `history_dropped`, `control` (whether it names a `.h<id>.ctl` sidecar) and `failure_seen`. The guard checks all of them but `history_dropped` against what this build adopts and shows (a row without `render_debt` owes nothing). |

The bytes are unchanged with one exception. The manifest names each grid
sidecar by its absolute path in the producer's private directory, and the
generator replaced that directory with `@HANDOFF_DIR@`. The guard puts the
directory back when it stages the desk. No digest covers the manifest's
bytes. Only the meta strings inside it are hashed, and the substitution cannot
reach those: the guard checks each one against its `s<id>.meta.json` and
recomputes the recorded digest from the files.

## The v0.91.0 desks

The generator self-checks each desk, so v0.91.0's own consumer adopts every
one of them exactly:

- `incident-55x149`: the 2026-09-22 incident desk. Session 0 is Claude Code in
  1049, entered from a prompt on the last row of a 56-row grid, after which the
  message band took one row. Its DECSC slot is on row 55 of a 55-row grid.
  Session 1 is a shell at the same size with 256 lines of styled history.
- `claude-code-1049`: a Claude Code transcript in 1049 with a turn in its
  ledger (so it has a real control carry), split beside a shell.
- `history`: three shells carrying history with SGR 256 and truecolour, curly
  underline colour, wide CJK, emoji, combining marks, OSC 8 links, OSC 7
  directories, bracketed paste, kitty keyboard, a scroll region and edited tab
  stops.
- `twelve-panes`: twelve sessions in two windows: a 2x2 tab, a three-column
  tab, splits, two editors in 1049, Claude Code in a pane, per-record metadata
  and connection triples.

## The v0.92.0 desks

The same four desks, written by v0.92.0's producer (the v0.91.0 capture
loop, whose checkpoints now carry each shell's shell-integration nonce, and a
window carry holding the unified messages instead of status bars), plus one
desk for the shape v0.92.0 changed:

- `shell-integration`: two shells, each signing its OSC 133/633 marks with
  its own tab's nonce and requiring it, one of them in 1049 with an editor.
  Their `parent.toml` rows record `require_shell_integration_nonce` and the
  nonce, and the guard checks that this build reassembles both from the
  older producer's meta. v0.92.0 is the first release whose rows carry them.

## The v0.93.0 desks

Added after v0.94.0's, from the v0.93.0 tag (73a9b424007d), because installs
of v0.93.0 still hand off to every later release. `generator-v0.93.0.rs.txt`
is the exact code that wrote them. v0.93.0's producer sits between the other
two:

- Its capture is already the producer ladder: `carry_for_wire` for each
  session, then `settle_wire_carries`. Every session here is carried at
  `CarryRung::Full`. `write_outgoing` already takes the repaint set.
- It has no history carry and no `.hist` sidecar. Its records and layout
  leaves carry none of the fields v0.94.0 added (`questions`, `fg_holder`,
  `rekey`, `loader`, `history`, `history_dropped`, `history_lost`), and its
  layout windows have no `show`.

Its `parent.toml` rows record no `history_take`, so the guard expects no
sidecar for any session. The desks are the six v0.94.0 has, plus one for the
shape v0.93.0 changed:

- `history-carry` keeps the name, but here it has only the screens: the
  1,500-line shell and the shell running `less` in 1049. Carried by a release
  before the history carry, the first crosses with the 256 lines its
  checkpoint holds and the second with none. The consumer must adopt both
  exactly and import nothing.
- `shell-integration`'s records carry no `rekey` or `loader`.
- `stalled-sequence`: three sessions whose parsers were left mid-sequence
  over a quiet PTY. One is a shell after an unterminated OSC title
  (`printf '\e]0;building'`, `OscString`). One is an ssh session stopped
  halfway through an SGR (`CsiParam`). The third is an editor in 1049 whose
  kitty-graphics upload stopped inside its APC payload (`SosPmApcString`).
  Through v0.92.0 such a session refused every in-session update. v0.93.0
  carries it with the partial sequence left out, as CAN would
  (`Terminal::checkpoint_carry_abandoning_partial`), and leaves the live
  parser as it was. The generator's capture takes the PTYs first, as
  v0.93.0's does, and checks each such master is quiet before carrying it.

The generator ran with `SOURCE_DATE_EPOCH` set to the release's build number
from `RELEASES.ledger` (1790305290), so the manifests' `outgoing_build` is
the one the shipped binary writes. v0.94.0's desks were generated without it,
so theirs is the tag commit's epoch, one above the ledger's. The consumer
reads only whether the field is present.

## The v0.94.0 desks

The same five desks, written by v0.94.0's producer, plus one desk for the
shapes v0.94.0 changed.

v0.94.0's producer differs from v0.92.0's in four ways that reach these
bytes:

- The capture is the producer ladder: `carry_for_wire` for each session at
  the budgets `App::capture_parked_screens` prices, then
  `settle_wire_carries`. Every session here is carried at the top rung
  (`CarryRung::Full`), so the repaint set is empty.
- Every record carries the foreground holder the park reads (`fg_holder`):
  the shell's pid at a prompt, else the job's.
- The history carry. The generator runs the fork lane's export and join
  (`HistoryPlan::Deferred`, then `handoff_history::stamp_manifest`) for a
  successor newer than v0.94.0. A session whose history is deeper than its
  checkpoint carries (256 lines, or none under an alternate screen) names a
  `.s<id>.hist` sidecar on its record (`history = "<len> <sha> <take>"`).
- The self-check also runs the adoption proof and, after Commit, the history
  import. Every session gets its parent's whole history back, line for line.

The new desk:

- `history-carry`: a shell with 1,500 lines of history, and a shell with
  1,000 lines running `less` in 1049, whose checkpoint carries none of that
  history, so its sidecar carries all of it. The records also carry the other
  fields v0.94.0 added (v0.93.0 added none of them): a running
  `history_lost` from an earlier handoff, `rekey` and `loader` for shells
  spawned with integration, and a `questions` word, which is also on its
  layout leaf. The `shell-integration` desk's shells carry `rekey` and
  `loader` as well.

v0.94.0 has no `stalled-sequence` desk: its generator asserts every parser
is at Ground.

## The v0.95.0 desks

The same six desks, written by v0.95.0's producer, plus one desk for the
shape v0.95.0 changed.

v0.95.0's capture, worker order and writer are v0.94.0's. What changed is the
meta each session's screen carries (`CheckpointMeta`):

- `absolute_row_counter`, on every session, and `alt_absolute_row_counter`
  for a session with an inactive grid. The successor continues the parent's
  row numbering, so an absolute row names the same line after the handoff.
  Every session's meta therefore differs from what v0.94.0 wrote.
- `color`: the colours an application set, as a diff from the configured
  theme (OSC 4 palette entries, OSC 10/11/12/17/19), and the XTPUSHCOLORS
  stack whole. Absent when nothing is overridden.
- `shell`: the OSC 133/633 state: the phase, the command marks and output
  blocks still readable in the carried history, the one being built, and the
  counters. A command running across the handoff completes on the
  successor. The `shell-integration` desk's shells carry it now.

The self-check also asserts that these cross the wire intact, that after the
history import every carried mark still names its line, and that a running
command completes on the successor. The layout leaf gained `agent`, which a
handoff layout never fills, so it writes no bytes here.

The new desk:

- `colour-and-shell`: a shell with aterm's signed integration, a base16
  theme (OSC 4, with `allow_palette_reconfigure` on, and OSC 10/11/12), 150
  finished commands, so its history is deeper than its checkpoint carries,
  and a `cargo build` still running. Beside it, a shell whose own script
  sends unsigned marks, running vim in 1049, which pushed the colours
  (OSC 30001) and set its background, cursor and selection colours. Its
  checkpoint carries none of the shell's history, and the marks it carries
  sit on the saved primary.

`generator-v0.95.0.rs.txt` is the exact code that wrote them.

## The v0.97.0 desks

The same seven desks, written by v0.97.0's producer, `stalled-sequence` again,
one desk for the link-stripping rung v0.97.0 added, and two for the lower
ceilings of the signed handoff policy it added. v0.96.0 has no directory: it
was never cut (`RELEASES.ledger` has no build for it and origin has no tag),
so no install runs its producer.

Two generators ran at the v0.97.0 tag. The `generator.rs.txt` of that time
(bf99347d0) wrote the nine desks. `generator-v0.97.0-policy.rs.txt`, written
independently, wrote the same shapes plus the two policy desks. Only its `policy-visible` and
`policy-repaint` are checked in: its own copies of the other desks are the
same shapes as the first generator's, and its `stripped-links` desk is the
shape `link-dense` stands for.

v0.97.0's writer and meta are v0.95.0's. In the seven desks, every session's
grids, control and history sidecars and meta are the bytes v0.95.0 wrote,
except the wall-clock times in the shell marks' meta. What changed is how the
park decides what to carry:

- The signed handoff policy (75f60b08c, eb807651f, 3d6ebfdda). Every park
  reads the successor's policy (`App::park_policy`) and captures under its
  ceiling (`carry_for_wire_within`). The history plan follows the same answer
  (`HistoryPlan::deferred_under`), and the fork lane's worker refuses a capture
  taken above the policy it reads. Releases ship `publish/handoff-policy.toml`
  empty (`schema = 1`), so there is nothing to follow and the park captures at
  the `Full` ceiling. The generator lays that file out as the cut does, reads it
  with the producer's own reader (`read_from_bundle`, then `adopt`), and records
  the ceiling (`park_carry_ceiling = "full"`).
- The capture walks the pool in session-id order (eb807651f). A parser left
  mid-sequence is carried, with the partial sequence left out, only over a
  quiet PTY; over queued output the park misses. So the generator makes the
  PTYs before the capture, as the v0.93.0 generator did, and `stalled-sequence`
  is back. v0.94.0's and v0.95.0's generators asserted every parser was at
  Ground and had no such desk. With the history carry, its title shell and its
  editor's saved primary now name `.hist` sidecars.
- The link-stripping rung (b2275989d, its work bounded by 510affa38). A line
  record over the wire's per-record cap (`16 KiB + cols * 512`) now costs that
  line its OSC 8 links (`CarryRung::StrippedLinks`), not the screen. Through
  v0.95.0 such a screen went blank until its program redrew (the Repaint rung).
  Such a line only in the scrollback cost the whole scrollback, and through the
  capture's history latch every later tab's too. The stripped carry needs no
  repaint and keeps its control carry.

The self-check also asserts that each session is carried at the rung its desk
stands for, and that the pool's self-check lowers nothing. For a stripped
session, it asserts that only the over-cap lines lost their links, at the whole
depth asked for. It also asserts that both grids adopt byte for byte and that a
stalled session's carry says Ground while its live parser is untouched.

The new desks:

- `link-dense`: a shell that listed two CI runs' signed log links. One row of
  twelve 8 KiB links has scrolled into its history, another is on screen, and a
  row of two links stays under the cap and keeps them. Its history fits in the
  256 lines its checkpoint carries, so it names no sidecar. Beside it, a shell
  with 600 lines of history printed a row of twelve such links and then opened
  vim in 1049. That row is on the saved primary (the inactive grid), which the
  carry strips, and the `.hist` sidecar carries the saved primary's whole
  history. Both sessions are carried at `stripped-links`: two lines lost their
  links in the first session and one in the second.

The desk keeps every link-dense line out of the `.hist` sidecars. It has to:
v0.97.0's own consumer refuses a sidecar with such a line in any frame it
decodes ("the sidecar arrived with a frame that does not decode"). The sidecar
reader decodes each frame under the strict per-record cap, and the history
export does not strip links. A tab whose deep history holds such a line
therefore gets none of the history its sidecar carries: the import fails with
that reason. Producers from v0.94.0
through v0.97.0 write such sidecars. v0.98.0's consumer admits them, keeping
each such line with its links dropped (190bc4d50, 8defd246f); that leniency is
guarded by `handoff_history_tests`' shipped-producer tests
(`a_link_dense_line_a_shipped_producer_wrote_costs_only_its_links`), not by
these fixtures.

The two policy desks, from `generator-v0.97.0-policy.rs.txt`. It captures each
desk under a CEILING it names, as `carry_for_wire_within` does under a
successor's signed policy, where the other v0.97.0 generator read the
release's empty policy and so captured every desk at `Full`. `visible` carries
no scrollback and labels every carry `VisibleOnly`. `repaint` carries every session as a
blank canonical screen whose scalars come from
`Terminal::carry_meta_abandoning_partial`, with no grid projected. Under a
ceiling that carries no scrollback no history is priced and the history plan
is `Withheld`: no `.hist` sidecar is named, and every line the park saw beyond
the checkpoint's is counted on the record (`history_dropped`, and
`history_lost` on top of the running loss).

- `policy-visible`: the successor's policy says `carry = "visible"`. A shell
  with 1,500 lines of history and a running loss of 12 from an earlier
  handoff, a shell under aterm's signed integration with a `cargo build`
  still running (its marks sit on the visible rows the carry keeps), and
  Claude Code in 1049 with a turn in its ledger. Every session is carried at
  `VisibleOnly`, keeps its control carry, and names no sidecar. Every line of
  history is counted as dropped.
- `policy-repaint`: the successor's policy says `carry = "repaint"`. A shell
  with 1,500 lines of history, vim in 1049 (so its blank carry has an
  inactive grid), a shell stalled inside an unterminated OSC title, and
  Claude Code in 1049. Every session is in the repaint set, carries no
  control carry and no history, and counts every line it leaves behind.

## The v0.98.0 desks

The same nine desks, written by v0.98.0's producer, one desk for the shape
v0.98.0 changed, and the two policy desks.

v0.98.0's policy read, capture, worker order, writer and meta are v0.97.0's.
In the nine desks every session's grids, control and history sidecars and
layout are the bytes v0.97.0 wrote; the manifests and metas differ only in the
release's stamps (nonce, `sid`, `outgoing_build`, the carried update row) and
the wall-clock times in the shell marks. What changed:

- The history export drops over-cap links (190bc4d50, 8defd246f). Before it
  writes a frame, the export drops the OSC 8 links of every line whose record
  is over the per-record cap (`16 KiB + cols * 512`), the screen carry's rule.
  So every frame of a v0.98.0 sidecar decodes under the strict caps, and the
  line crosses with its text, attributes and wrap flag exact. Through v0.97.0
  the export wrote such a line whole.
- A record's `history_withheld` (25223b3a6) says a successor's policy asked
  for no scrollback. Under the empty shipped policy it is `false`, which is
  not written.
- The handoff layout carries Settings drafts (37294ce2f, plan P2-2): the
  park's layout is `App::capture_handoff_layout`, which puts each Settings
  view's unsaved field drafts on its leaf (`settings_drafts`).

The self-check also reads every `.hist` sidecar off its bytes. It asserts
that every frame decodes under the strict caps, and that the sidecar is the
parent's whole history with only the over-cap lines' links dropped. After the
import it compares every session's history with the parent's cell for cell,
under the same rule, and asserts the import repaired nothing
(`links_dropped = 0`). It asserts that v0.98.0's own consumer places each
Settings draft with its tab. The guard asserts the same of this build: every
draft the layout's bytes carry is placed on the leaf that carried it, with its
text, and none is dropped as unreadable.

The new desk:

- `link-dense-history`: session 0 is a shell that listed two CI runs' signed
  log links 400 lines apart. The first row of twelve 8 KiB links is older than
  the 256 lines its checkpoint carries, so it is in the part of the `.hist`
  sidecar the successor imports (`history_take = 480`). The second is inside
  those 256 lines: the screen carry strips it (`stripped-links`, one line),
  and the sidecar holds it stripped past its take. A row of two links stays
  under the cap and keeps them. Session 1 is a shell with 500 lines of history
  that printed a row of twelve links, 80 more lines, and then opened vim in
  1049. Its checkpoint carries none of the saved primary's history and no
  dense row is on screen, so it is carried at `full`. Its sidecar carries the
  dense row stripped, with the rest of its history. The two sidecars hold 2
  and 1 lines without their links (`history_links_stripped`), and one of each
  is in the lines the successor imports. The window's second tab is Settings
  (`/text-fonts`) holding an unsaved `font_family` draft. The layout is built
  by hand, as every desk's is, and holds `restored_tabs` only: the Settings
  leaf is spelled as `App::capture_handoff_layout` spells one, but the legacy
  mirrors a real capture also fills (`tabs`, `native_tabs`, `tab_order`) are
  empty, and so is the macOS window show state (`show`). No fixture layout of
  any release carries any of them.

The two policy desks, `policy-visible` and `policy-repaint`, are v0.97.0's
desks (the same sessions, the same ceilings), written by v0.98.0's producer.
They come from a second run of the generator at the same tag, which ported
`fx_policy_visible_desk` and `fx_policy_repaint_desk` from
`generator-v0.97.0-policy.rs.txt` into `generator.rs.txt`. Each desk names the
ceiling of the successor's policy it is parked under. For these two the
generator lays out a successor policy asking for it (`schema = 1`, `carry =
"visible"` or `"repaint"`) and reads it with the producer's own
`read_from_bundle` and `adopt`, so `park_carry_ceiling` is the producer's own
reading. Beyond v0.97.0's, the records now say the withheld history as the
policy's (`history_withheld = true` on every session that leaves lines
behind), and the self-check asserts that v0.98.0's own consumer reads it so
(`withheld`, on the adopted history and the import's report). That run wrote
all twelve desks into a scratch directory, and v0.98.0's own consumer adopted
each exactly. Only the two policy desks were copied in; the ten above are the
first run's bytes.

The v0.98.0 generator, the one that writes all twelve desks, is in git
history as `generator.rs.txt` before v0.100.0's replaced it.

## The v0.100.0 desks

The same twelve desks, written by v0.100.0's producer, and one desk for the
shape v0.100.0 added. v0.99.0 has no directory: it was published as source
only and never cut (`RELEASES.ledger` has no build for it), so no install runs
its producer. The v0.100.0 generator, the one that writes all thirteen
desks, is in git history as `generator.rs.txt` before v0.101.0's replaced it;
its header lists what changed from v0.98.0's. It ran once at the v0.100.0 tag
(6b38259f7da1) with `SOURCE_DATE_EPOCH=1790807622`, and v0.100.0's own
consumer adopted every desk exactly.

v0.100.0's policy read, capture ladder, worker order and history carry are
v0.98.0's. What changed and reaches these bytes:

- The parser carry (ec141e044). A partial escape sequence rides the meta
  (`parser`) and the successor continues it; only a hooked DCS is still
  abandoned as CAN would. `stalled-sequence`'s three sessions now carry
  `OscString`, `CsiParam` and `SosPmApcString`, where through v0.98.0 each
  was carried at Ground. Over queued output the park still misses for a
  sequence no carry holds or one the carry would only swallow (the kitty
  upload's APC string), so those are carried over a quiet PTY only, as here.
- Every session's control carry travels (8d470b7cb). A rung that does not
  keep the screen (`repaint`) now drops only the differ's state, so every
  `policy-repaint` session names a `.ctl` sidecar with its turn ledger.
- Records carry the fields a real park writes:
  `claim_known = true` and a `timeline_id` on every session, and the
  manifest's `roster_seq`. `hold`, `supervisor`, `attention_owners`,
  `fabric`, `viewport_from_bottom`, `fabric_attached` and the window's font
  zoom are what none of these sessions has, so none is written.
- The layouts carry the legacy mirrors `App::capture_manifest` fills beside
  `restored_tabs` (`tabs`, `native_tabs`, `tab_order`) and, on macOS, the
  window's `show` state. Through v0.98.0 every fixture layout held
  `restored_tabs` only.

The new desk:

- `held-pane`: two live shells, each beside a pane kept open by `--hold`
  after its command exited. `make` (pane 2) failed with status 2 after
  printing a link to its CI log; vim (pane 3) was killed by SIGKILL in 1049,
  so its held screen has an inactive grid too. Neither has a session record
  or a PTY: the manifest's `held` list names each one's screen and exit, in
  `.h<id>.heldgrid` / `.heldalt` sidecars outside every digest, and the
  layout names each pane by its id. The successor shows each read-only, with
  every link stripped; an older successor skips the list and keeps its
  placeholder, so no digest moves.

## The v0.101.0 desks

The same thirteen desks, written by v0.101.0's producer. No shape v0.101.0
changed needs a desk of its own: each one is carried by a desk that already
stands for it, so `REQUIRED_DESKS` is unchanged. `generator.rs.txt` is the
v0.101.0 generator, the one that writes all thirteen desks, and the template
for the next release; its header lists what changed from v0.100.0's. It ran
once at the v0.101.0 tag (8ed1063a8dff) with `SOURCE_DATE_EPOCH=1791501389`
and both commit pins, and v0.101.0's own consumer adopted every desk exactly,
including each held pane's history import.

v0.101.0's capture ladder, worker order and history carry for the handed
sessions are v0.100.0's. Every handed session's grid, control and history
sidecar is byte for byte the one v0.100.0 wrote. Every meta is v0.100.0's plus
one key, except the wall-clock times in the shell marks (`*_time_ms`:
`colour-and-shell` s0 and s1, `shell-integration` s0 and s1, `policy-visible`
s1). What changed and reaches these bytes:

- The output-state carry (round eight of the update audit, finding 502). The
  meta carries the engine's in-flight output state (`output_state`: the REP
  character, an SGR 58 underline colour, an open OSC 8 link, a VT52 address in
  progress, a pending ZWJ, the XTPUSHSGR stack, the XTSAVE'd modes) to a
  successor that reads it. Every session that printed anything has a REP
  character, so all 41 sessions carry it, as the section on `output_state`
  below foresaw. To such a successor an open link or a VT52 address is no
  park hazard, and on the fork lane neither is a picture the carry swallows (a
  kitty APC, an OSC 1337 image, a sixel string). `stalled-sequence` still
  carries `OscString`, `CsiParam` and `SosPmApcString` in its meta.
- The render debt (the render stream's R08). A record carries the rows the
  engine owed a carried alternate screen (`render_debt = "<shift>,<pushed>"`).
  `incident-55x149`'s Claude Code, resized in 1049 from 56 rows to 55 and
  never fully redrawn, owes `-1,0`. That desk stands for the debt, and no
  other session owes anything.
- Held panes carry their scrollback and control carry (round eight, findings
  21, 39 and 730, and U34). `held-pane`'s two panes now name a `.h<id>.hist`
  sidecar (11 and 12 lines, none left behind) and a `.h<id>.ctl` sidecar. Each
  manifest entry names both, the pane's `sid` and `failure_seen`. `make`'s
  failure, in the tab in front, had been seen; vim's, in the tab behind it,
  had not.
- Every manifest carries a turn-id count (`next_turn_id`): the ids the process
  minted plus the 4,096 the parent may still mint before it exits
  (`TURN_ID_RESERVE`). That is 4,096 on a desk with no turns, and 4,137 where
  v0.100.0's said 41. The control carry is spelled in the successor's words.
- Each layout window and the window carry name the window's raw inner size
  (`inner_px`). Here it is the frame a headless `App` of that build gives the
  grid and its message band. The window carry also says when its messages
  were captured (`messages_captured_unix_ms`). No window has a presence row.
- The window carry's `next_message_id` is 1 where v0.100.0's was 0, on the
  eleven desks with no update row. It is the headless `App`'s own carry
  (`carried.next_message_id`), so this is the producer's change, not the
  generator's. The two desks with an update row (`incident-55x149`,
  `policy-visible`) still say 2. That row's title is a generator input and
  names the successor, `Installing aterm v0.102.0`, where v0.100.0's named
  v0.101.0.
- The policy desks' successor seals its rescue as a `[[ranges]]` table naming
  v0.101.0's build (1791501389), with its reason and the top-level hull. That
  is the only shape a cut seals since 5afd825d6, and the generator asserts
  `PolicyFile::parse_for_cut` accepts it. The producer reads the range and
  parks under it.

The producer also folds a desk past the layout's caps
(`RestoreManifest::fold_into_caps`). No desk here is past them, so none is
folded. Two fields stay empty on every desk: a record's `journal_key` (each
desk's process wrote its own crash journal image) and a handoff leaf's
`agent` (no desk owes a relaunch).

The guard checks the new facts each `parent.toml` records: `output_state`
and `render_debt` on every session, and each held pane's `sid`,
`history_take`, `history_dropped`, `control` and `failure_seen`. It also runs
this build's held import over the held panes' `.h<id>.hist` bytes, the way
`App::restore_held_leaf` and the import worker run it
(`assert_held_history_imports`). Every line the parent stamped must land in
front of the held screen, cell for cell, with nothing lost, and with every
link dropped, as on the screen. v0.101.0's own consumer kept those links
clickable, so `h2.hist`'s two links and `h3.hist`'s one are checked against
this build's consumer only.

As a template, `generator.rs.txt` predates one API change. Since 08ec7b748
(2026-10-08) the build stamp is `aterm_types::version::BuildStamp`, installed
with `aterm_types::version::install`. `crate::build_info` no longer exports
`BuildStamp` or `install_build_stamp`, so `fx_install_release_stamp` does not
compile at a later tag. Rewrite it to install the new stamp. It has nine more
fields, and each one is what the release's shipped binary stamps:

- `app_version` is the tag's `[workspace.package] version`.
  `crate::build_info::version()` reads it now, so it is what keeps
  `producer_version` right.
- `compiler_version_line`, `compiler_commit`, `compiler_host` and
  `compiler_trust_version` come from the `trustc -vV` that built the release,
  and `compiler_flavor` is `"t"`.
- `build_profile`, `trust_verify` and `release_build` are set as the shipped
  binary sets them: the `release` profile, its verification state and
  `true`. About and `aterm ctl version` on that binary show them.

The git fields and `build_number` come from the same three pins as before.

`rendezvous.toml` is v0.100.0's transport: the same claim frame, grant header,
body and descriptor order. Only the identity fields differ: the producer, the
target, the birth record, the device terms and the proof over them.

## The rendezvous transport

The desks pin what a producer writes to disk. On macOS the successor gets its
PTY masters, and the readiness and Commit pipes, over a different wire: the
rendezvous (`crates/aterm-gui/src/handoff_rendezvous.rs`). The successor dials
a socket the parent bound and claims with a frame (`ATRZ1C` and the claim
secret); the parent answers with a grant, an `ATRZ1G` header carrying every
descriptor through `SCM_RIGHTS`, then a body naming each session. None of that
reached a desk, so a change to a magic, the header, the body or the descriptor
order passed every same-build test and would have failed every update.

Each release directory therefore also holds `rendezvous.toml`, what that
release's transport does over fixed inputs (the claim secret, the nonce
`c0ffee00aa55aa55c0ffee00aa55aa55`, and sessions `(0,4000)`, `(1,4001)`,
`(7,4002)`):

| key | what it is |
| --- | --- |
| `claim_frame_hex` | The claim that release's dialer (`dial_for_test`, which writes `claim_frame`) writes for `claim_input` (the fixed claim secret, `RZ_SECRET`), read off a plain listener. |
| `grant_header_hex`, `grant_body` | The grant its parent sends: a real `Rendezvous::bind`, a real claim, `accept_claim`, then `transfer` with three `openpty` masters and two pipes, received by a real `recv_with_fds`. |
| `descriptor_order` | The order the descriptors arrived in, each identified by what it is: a master by its `st_rdev`, a pipe end by its inode. |
| `env_rendezvous`, `rendezvous_file_shape`, `env_claim`, `claim_hex_len` | The launch variables' names, the rendezvous socket's file name (pid and nonce elided) and the claim secret's length. |
| `env_proof_term`, `proof_term_launched`, `proof_term_fork` | The proof-term variable and the two values the parent writes, read from the match arms of that release's `app_update_handoff.rs` (no function returns them). |
| `env_target`, `target_value` | `ATERM_SEAMLESS_TARGET` and its value from that release's `encode_target_identity`, for its own build and commit. |
| `env_parent_birth`, `parent_birth_value` | `ATERM_HANDOFF_PARENT_BIRTH` and the value that release's `outgoing_parent_env` published for the generator's own process. Only its shape is read. |
| `proof_terms`, `proof_count`, `proof_digest` | The PTY device terms (`proof_identities_in_device_terms`) of the capture's masters, and that release's `adoption_proof` over them, the target in `target_value` and the fixed digests `proof_layout_digest` and `proof_screen_digest`. |

The claim secret's key is `claim_input`, not `claim_secret`: the public export's gitleaks scan (built-in rules only; the engine refuses repo-local allowlists) reads `…secret = "<hex>"` as a leaked API key. The value is a fixed test input, the same in every release's record.

The guard is `seamless::fixture_tests::every_shipped_producers_rendezvous_grants_this_successor`.
For every release in `PINNED_RENDEZVOUS` it asserts that:

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

Changing `CLAIM_MAGIC` to `ATRZ2C`, or pushing the Commit end before the
readiness end, turns it red.

All nine releases with fixtures are pinned: v0.91.0, v0.92.0, v0.93.0,
v0.94.0, v0.95.0, v0.97.0, v0.98.0, v0.100.0 and v0.101.0 (v0.96.0 and
v0.99.0 were never cut). Each file was
written by the capture at the end of `generator.rs.txt`, appended on its own to
`seamless_carry_tests.rs` in a worktree at the tag and run with the tag's
commit and ledger build number (step 3 below). Through v0.100.0 every API it
used was present from v0.91.0 on. v0.101.0's parent also checks the
dialler's code, so its `accept_claim` takes a `DialerCode`, and the capture
passes `DialerCode::this_process()`. Only the target, birth record, device
terms and proof differ between the files.

## Adding the next release's fixtures

Do this once for each release, after its tag exists (see docs/RELEASING.md,
"Every release adds its handoff fixtures").

`ship cut` enforces it, before the claim, in two gates
(`crates/aterm-release/src/gates.rs`):

- `gates::handoff_fixture_gate` refuses unless this release's directory
  exists, at least one desk in it holds a `parent.toml`, and `PINNED_DESKS`
  has a row for it. The rows must also cover every desk in `REQUIRED_DESKS`
  whose floor is at or below this release: one pinned desk is no longer
  enough. The gate reads both tables from the source of
  `seamless_fixture_tests.rs`, so keep them in their shape: one
  `("vX.Y.0", "<desk>")` pair per row. In `PINNED_DESKS` the release is the
  one whose fixtures hold the desk; in `REQUIRED_DESKS` it is the desk's
  floor, the first release that must write it. `targo --unverified ship release`
  asks the cut's own gate before anything is staged.
- `gates::handoff_fixture_guard_gate` runs the guard itself in the cut tree
  (`targo --unverified test -p aterm-gui --lib` filtered to
  `seamless::fixture_tests::`, in its own target directory), and refuses on a
  failing test, or when no test ran.

"This release" is the newest version in `RELEASES.ledger`, other than the one
being cut, whose `vX.Y.0` tag is on origin. So a recut asks for the same
directory as its first attempt, and a claim that was abandoned and then
skipped is passed over for the release before it. The fixtures can only come
from the tag, so add them while it is fresh. A `--dry-run` of the next cut
shows whether they are in.

1. Make a worktree at the tag, outside the repo tree. The `.noindex` suffix
   keeps Spotlight out of it:

   ```sh
   git worktree add --detach ~/aterm-vX.Y.0.noindex vX.Y.0
   ```

   If its `target` is a dangling symlink, `mkdir -p target.noindex` inside it.
   A worktree does not check out submodules. Check out the tag's own
   `vendor/astream`:

   ```sh
   git -C ~/aterm-vX.Y.0.noindex submodule update --init vendor/astream
   ```

   A symlink to the main tree's checkout is right
   only when `git ls-tree <tag> vendor/astream` names the same commit as the
   main tree.
2. Append `generator.rs.txt` (in this directory) to that worktree's
   `crates/aterm-gui/src/seamless_carry_tests.rs`, and adapt it to that
   release's API:
   - `fx_capture` must be that release's own capture path. The template
     copies v0.101.0's fork lane. The pool is walked in session-id order.
     Each session is carried by `carry_for_wire_within` under the park's
     policy ceiling, at the budgets `App::capture_parked_screens` prices. A
     partial sequence no carry holds, or one the carry would only swallow
     that is not a picture, is carried only over a quiet PTY. Every session's
     control carry is taken (without the differ's state below a rung that
     keeps the screen), and so is the render debt of a carried alternate
     screen. Then `settle_wire_carries` runs over the pool. Last come the
     held panes (`fx_capture_held`, `App::capture_held_screens`), each with
     its sid, its scrollback's fence and its control carry. The repaint set
     is the sessions whose rung has `CarryRung::needs_repaint`. If the
     release changed the capture, copy the change.
   - `fx_write` must run the release's own steps in its order. In v0.101.0
     that is:
     1. the policy read (`fx_park_policy`: the release's checked-in
        `publish/handoff-policy.toml` read out of a bundle, or a successor's
        `[[ranges]]` rescue for a policy desk);
     2. the PTYs, the capture, and the records stamped with the foreground
        holders and render debts;
     3. the layout, folded into the caps and committed
        (`commit_handoff_layout`);
     4. the fork lane's history plan (`HistoryPlan::deferred_under`, over
        the handed sessions and the held panes);
     5. the worker: its policy check, its layout round trip, the control
        carry's export (`export_for`, with the turn-id count it writes as
        `next_turn_id`), the history carry's export with the held panes'
        exports taken out (`take_held`), and `stamp_manifest`;
     6. `write_outgoing_with_held`, then the layout sidecar
        (`handoff_diet::seal_layout`).
   - Give every record field and layout leaf field the release added a value
     that a real session would carry, and keep the generator's self-check
     passing.
   - Write every desk in `REQUIRED_DESKS`: the thirteen desks
     `generator.rs.txt` writes, the two policy desks among them, each parked
     under the ceiling its desk names (`fx_park_policy`). Each session names
     the rung its desk stands for. Add a desk for any shape the release
     changed, and add it to `REQUIRED_DESKS` with this release as its floor,
     `("vX.Y.0", "<desk>")`, so this release and every later one must write
     it. Never move an existing row's floor, and never give a new row an
     earlier floor than the release that first writes it. The fixtures of the
     releases before it are frozen and can never gain the desk.
   - Build each layout the way `App::capture_manifest` does, with the legacy
     mirrors (`tabs`, `native_tabs`, `tab_order`) and, on macOS, `show` beside
     `restored_tabs`, or take it from a real `capture_handoff_layout` on a
     headless `App`. Through v0.98.0 `fx_window` built `restored_tabs` only;
     from v0.100.0 it derives the mirrors and `show` from the tabs it is
     given, and from v0.101.0 each window's `inner_px` (`fx_inner_px`).
3. Run the generator, still in the worktree:

   ```sh
   ATERM_BUILD_GIT_COMMIT=<the tag's 12-digit short commit> \
   ATERM_BUILD_GIT_COMMIT_FULL=<the tag's 40-digit commit> \
   SOURCE_DATE_EPOCH=<the release's build number in RELEASES.ledger> \
   ATERM_HANDOFF_FIXTURE_OUT=~/aterm/crates/aterm-gui/tests/fixtures/handoff/vX.Y.0 \
     targo --unverified test -p aterm-gui --lib -- generate_handoff_fixtures --ignored
   ```

   It refuses a desk that the release's own capture would not park, or that
   its own consumer would not adopt exactly.

   The three pins are the release's build stamp. Since 2026-10-05 the stamp
   is the binary's (`crates/aterm/build.rs` emits it and `main` installs it).
   This library's test binary reads a fixed test stamp instead (build
   3000000000, commit `7e57b1d0c0de`), and no compile-time variable reaches
   it. So the generator reads the pins at run time and installs what the
   shipped binary's build script stamped from them
   (`fx_install_release_stamp`, first, before anything reads provenance).
   It refuses to run without them. At a tag that contains 08ec7b748, rewrite
   `fx_install_release_stamp` for `aterm_types::version` first. The v0.101.0
   section says how.
   - The commit pins keep `producer_commit` the release's commit. They reach
     no fixture byte except that `parent.toml` field and the rendezvous
     record's target and the proof over it. The tag is annotated, so take the commit it points at
     (`git rev-parse 'vX.Y.0^{commit}'`, and `--short=12` for the short
     one), not the tag object's own sha.
   - `SOURCE_DATE_EPOCH` makes the build number the one the release was cut
     with. Each manifest's `outgoing_build` is then what the shipped binary
     writes, and the wire caps and history export judge the hop from that
     build.

   A generator that predates the runtime stamp (v0.100.0's and older) takes
   only the two compile-time pins, and run on a tag after 2026-10-05 it
   writes the test stamp's build.
   Then write the release's `rendezvous.toml` with the rendezvous capture, the
   section at the end of `generator.rs.txt` (from `THE RENDEZVOUS TRANSPORT
   CAPTURE` on). Append that section alone, with `fx_rfc3339` and
   `fx_install_release_stamp`, if the desk generator is not appended, and run
   it the same way:

   ```sh
   ATERM_BUILD_GIT_COMMIT=<the tag's 12-digit short commit> \
   ATERM_BUILD_GIT_COMMIT_FULL=<the tag's 40-digit commit> \
   SOURCE_DATE_EPOCH=<the release's build number in RELEASES.ledger> \
   ATERM_RENDEZVOUS_FIXTURE_OUT=~/aterm/crates/aterm-gui/tests/fixtures/handoff/vX.Y.0 \
     targo --unverified test -p aterm-gui --lib -- generate_rendezvous_fixture --ignored
   ```

   It refuses a build whose commit is not the tag's 12 digits and a descriptor
   it cannot identify.
4. In the main tree, add the new `(release, desk)` rows to `PINNED_DESKS` and
   the `"vX.Y.0"` row to `PINNED_RENDEZVOUS` in `seamless_fixture_tests.rs`, and
   run the guard:

   ```sh
   targo --unverified test -p aterm-gui --lib -- fixture_tests
   ```

5. Commit the new directory with the pinned rows. Do not commit the generator
   in the release worktree. Commit it here, beside the fixtures: as
   `generator.rs.txt` when this release is the newest one with fixtures (it
   is then the next release's template), otherwise as
   `generator-vX.Y.0.rs.txt`. Then remove the worktree:

   ```sh
   git worktree remove --force ~/aterm-vX.Y.0.noindex
   ```

## `render_debt` (additive, from v0.101.0)

`SessionRecord::render_debt` (`"<shift>,<pushed>"`, the rows the outgoing engine
owed the alternate screen) is a lenient additive record key. No producer
before v0.101.0 writes it, so every desk of those releases is a producer
WITHOUT the key, and the guard adopts each of them with no debt
(`Adopted::render_debt` is `None`: the successor owes the program nothing it
was not told). v0.101.0 is the first shipped producer WITH it:
`v0.101.0/incident-55x149` carries `-1,0` for its Claude Code, its
`parent.toml` records it, and the guard checks this build adopts exactly that
debt. Every other session of that release records none and owes nothing.
This build's own producer is held by
`seamless_carry_tests::the_render_debt_crosses_the_handoff_and_only_onto_a_carried_alt_screen`,
which writes the key through `write_outgoing`, reads it back through
`take_incoming`, and holds its negative controls (a blank-on-purpose screen,
the main screen, no key). `manifests_cross_between_the_two_shapes_both_ways`
holds that an older reader skips it.

## `output_state` (additive, from v0.101.0)

`CheckpointMeta::output_state` (round eight of the update audit, finding 502:
the REP character, an
SGR 58 underline colour, an open OSC 8 link, a VT52 address in progress, a ZWJ
waiting for its emoji, the XTPUSHSGR stack, the XTSAVE'd modes) is a lenient
additive meta key. No producer before v0.101.0 writes it, so every desk of
those releases restores with none of it, as those producers' successors always
did. v0.101.0 is the first shipped producer WITH it. Every session that has
printed anything writes it (its REP character), so every session of every
v0.101.0 desk carries it, with no desk of its own. Each `parent.toml` session
row records `output_state = true`, and the guard checks the checkpoint this
build reassembles carries it.
This build's own producer is held by
`seamless::f4_adoption_proof_asymmetry::the_in_flight_output_state_crosses_the_meta_wire_inside_the_digest`.
It writes the key, reads it back through the strict parser and the screen
digest, and holds the same meta without the key as its negative control.
`App::capture_parked_screens` asks `output_state_in_flight` only on a hop to
an older successor (`WireCaps::carries_output_state`). The generator's hop is
to a successor that carries the state, so its capture never asks it. The PTY
keeper's META never writes the key
(`seamless::keeper_meta`), and a keeper recovery drops it from any META that
has it (`recovered_checkpoint`).
