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
  the parent's, byte for byte, over the recorded inputs.

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
| `parent.toml` | What the parent committed to: the screen and layout digests, an adoption proof and its two frames over fixed inputs (`proof_*`), the list of files, and each session's carried geometry. From v0.94.0 each session row also records `history_take` (the lines its `.hist` sidecar carries), `history_lost` and `fg_holder`, and from v0.95.0 `absolute_row_counter` and `alt_absolute_row_counter` (the numbering the successor continues), `color` (whether an application colour diff rides the meta) and `shell_phase`, `shell_marks` and `shell_completed_seq` (the shell-integration state it carries). The guard does not read these yet. |

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

## The v0.94.0 desks

The same five desks, written by v0.94.0's producer, plus one desk for the
shapes v0.93.0 and v0.94.0 changed. v0.93.0 has no directory, so nothing
checks what its producer writes.

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
  fields v0.93.0 and v0.94.0 added: a running `history_lost` from an earlier
  handoff, `rekey` and `loader` for shells spawned with integration, and a
  `questions` word, which is also on its layout leaf. The `shell-integration`
  desk's shells carry `rekey` and `loader` as well.

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

`generator.rs.txt` is the v0.95.0 generator, the template for the next
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
   A worktree does not check out submodules. If the build cannot find
   `vendor/astream`, and the tag pins the same `vendor/astream` commit as the
   main tree (`git ls-tree <tag> vendor/astream`), replace the empty directory
   with a symlink to the main tree's checkout.
2. Append `generator.rs.txt` (in this directory) to that worktree's
   `crates/aterm-gui/src/seamless_carry_tests.rs`, and adapt it to that
   release's API:
   - `fx_capture` must be that release's own capture path. The template
     copies v0.95.0's (unchanged from v0.94.0): `carry_for_wire` for each
     session at the budgets `App::capture_parked_screens` prices, then
     `settle_wire_carries` over the pool. The repaint set is the sessions
     whose rung has `CarryRung::needs_repaint`. If the release changed the
     capture, copy the change.
   - `fx_write` must run the release's own worker steps in its order. In
     v0.94.0 and v0.95.0 that is the control carry's export, then the history
     carry's export and `stamp_manifest`, then `write_outgoing`, then the
     layout.
   - Give every record field the release added a value that a real session
     would carry, and keep the generator's self-check passing.
   - Keep the seven desks, and add a desk for any shape that release changed.
3. Run the generator, still in the worktree:

   ```sh
   ATERM_BUILD_GIT_COMMIT=<the tag's 12-digit short commit> \
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
4. In the main tree, add the new `(release, desk)` rows to `PINNED_DESKS` in
   `seamless_fixture_tests.rs` and run the guard:

   ```sh
   targo --unverified test -p aterm-gui --lib -- fixture_tests
   ```

5. Commit the new directory with the pinned rows. Do not commit the generator
   in the release worktree. Then remove the worktree:

   ```sh
   git worktree remove --force ~/aterm-vX.Y.0.noindex
   ```
