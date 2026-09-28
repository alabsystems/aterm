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
fails in the field. On 2026-09-22/23 that is what happened: a rule on one side
disagreed with what the other side sent, and every shell stayed on the old
build for a day.

Each directory here holds the bytes a released producer wrote, one directory
per release and one per desk inside it. The guard
`seamless::fixture_tests::every_shipped_producers_desk_adopts_exactly_and_proves`
(`crates/aterm-gui/src/seamless_fixture_tests.rs`) runs every fixture of every
release through this build's `take_incoming`. It asserts that:

- every session is adopted exactly: none repainted, each screen byte-for-byte
  at its carried geometry, each control carry read, and the layout placed;
- the screen and layout digests equal what that release's parent committed to;
- this build's adoption proof and its `ProofReady` and `Commit` frames match
  the parent's, byte for byte, over the recorded inputs;
- every history sidecar the parent named (v0.94.0 on) is taken before the
  proof at the depth it stamps, and after Commit the import puts every one of
  its lines back: the session's history is then the sidecar's lines in front
  of the lines its checkpoint restored, line for line and cell for cell, with
  nothing lost at the hop and the session's running loss (`history_lost`) and
  foreground holder carried. A session that names no sidecar, which is every
  session of a release before the history carry, imports nothing and keeps
  exactly the history its checkpoint restored.

The guard reads the sidecar's lines itself, straight off the checked-in
bytes with the line codec. It does not use the consumer's reader, because the
import is what it checks.

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
| `s<id>.meta.json` | A copy of the meta JSON the manifest embeds for session `<id>`, extracted so it can be reviewed. The guard checks that it equals the embedded copy. |
| `parent.toml` | What the parent committed to: the screen and layout digests, an adoption proof and its two frames over fixed inputs (`proof_*`), the list of files, and each session's carried geometry. From v0.94.0 each session row also records `history_take` (the lines its `.hist` sidecar carries), `history_lost` and `fg_holder`, and from v0.95.0 `absolute_row_counter` and `alt_absolute_row_counter` (the numbering the successor continues), `color` (whether an application colour diff rides the meta) and `shell_phase`, `shell_marks` and `shell_completed_seq` (the shell-integration state it carries). From v0.97.0 the parent also records `park_carry_ceiling` (the ceiling the park captured under, from the successor's handoff policy), and each session row `rung` (the `CarryRung` the ladder carried it at: `full`, or `stripped-links`) and `links_stripped` (how many of its lines lost their OSC 8 links). The guard checks the v0.94.0 fields (`history_take`, `history_lost`, `fg_holder`) against the manifest and the sidecar's bytes, and against what this build takes and imports; it does not read the v0.95.0 or v0.97.0 fields yet. |

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

## The v0.97.0 desks

The same seven desks, written by v0.97.0's producer, `stalled-sequence` again,
and one desk for the shape v0.97.0 changed. v0.96.0 has no directory: it was
never cut (`RELEASES.ledger` has no build for it and origin has no tag), so no
install runs its producer.

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

The new desk:

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
that reason. This was measured in the v0.97.0 worktree, with the line older
than the checkpoint's 256 lines and with it inside them. Producers from v0.94.0
on already write such sidecars, so it is a gap to fix in the consumer, not a
shape these fixtures record.

`generator.rs.txt` is the v0.97.0 generator, the template for the next
release.

## Adding the next release's fixtures

Do this once for each release, after its tag exists (see docs/RELEASING.md,
"Every release adds its handoff fixtures").

`ship cut` enforces it. Before the claim, the cut of the next release refuses
unless this release's directory exists, at least one desk in it holds a
`parent.toml`, and `PINNED_DESKS` has at least one row for it
(`gates::handoff_fixture_gate` in `crates/aterm-release/src/gates.rs`). The
gate reads those rows from the source of `seamless_fixture_tests.rs`, so keep
them in the table's shape: one `("vX.Y.0", "<desk>")` pair per row. "This
release" is the newest version in `RELEASES.ledger`, other than the one being
cut, whose `vX.Y.0` tag is on origin. So a recut asks for the same directory
as its first attempt, and a claim that was abandoned and then skipped is
passed over for the release before it. The fixtures can only come from the tag, so add them while it is
fresh. A `--dry-run` of the next cut shows whether they are in.

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

   That is what the v0.93.0 generation did; its tag pins a different astream
   commit from the main tree. A symlink to the main tree's checkout is right
   only when `git ls-tree <tag> vendor/astream` names the same commit as the
   main tree.
2. Append `generator.rs.txt` (in this directory) to that worktree's
   `crates/aterm-gui/src/seamless_carry_tests.rs`, and adapt it to that
   release's API:
   - `fx_capture` must be that release's own capture path. The template
     copies v0.97.0's: the pool walked in session-id order, each session
     carried by `carry_for_wire_within` under the park's policy ceiling at
     the budgets `App::capture_parked_screens` prices, a mid-sequence parser
     carried only over a quiet PTY, then `settle_wire_carries` over the pool.
     The repaint set is the sessions whose rung has
     `CarryRung::needs_repaint`. If the release changed the capture, copy the
     change.
   - `fx_write` must run the release's own steps in its order. In v0.97.0
     that is the policy read (`fx_park_policy`, the release's checked-in
     `publish/handoff-policy.toml` read out of a bundle), the PTYs, the
     capture, the fork lane's history plan (`HistoryPlan::deferred_under`),
     the worker's policy check, the control carry's export, the history
     carry's export and `stamp_manifest`, `write_outgoing`, then the layout.
   - Give every record field the release added a value that a real session
     would carry, and keep the generator's self-check passing.
   - Keep the nine desks in `generator.rs.txt`, and add a desk for any shape
     the release changed.
3. Run the generator, still in the worktree:

   ```sh
   ATERM_BUILD_GIT_COMMIT=<the tag's 12-digit short commit> \
   SOURCE_DATE_EPOCH=<the release's build number in RELEASES.ledger> \
   ATERM_HANDOFF_FIXTURE_OUT=~/aterm/crates/aterm-gui/tests/fixtures/handoff/vX.Y.0 \
     targo --unverified test -p aterm-gui --lib -- generate_handoff_fixtures --ignored
   ```

   It refuses a desk that the release's own capture would not park, or that
   its own consumer would not adopt exactly. `ATERM_BUILD_GIT_COMMIT` keeps
   `producer_commit` the release's commit: without it, the appended generator
   makes the build read the tree as dirty and write `<commit>-dirty`. It
   reaches no fixture byte except that `parent.toml` field. The tag is
   annotated, so take the commit it points at
   (`git rev-parse --short=12 'vX.Y.0^{commit}'`), not the tag object's own
   sha.
   `SOURCE_DATE_EPOCH` makes the build number the one the release was cut
   with, so each manifest's `outgoing_build` is what the shipped binary
   writes. Without it a dev build takes the tag commit's epoch instead.
4. In the main tree, add the new `(release, desk)` rows to `PINNED_DESKS` in
   `seamless_fixture_tests.rs` and run the guard:

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
