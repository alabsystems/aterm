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
| `s<id>.meta.json` | A copy of the meta JSON the manifest embeds for session `<id>`, extracted so it can be reviewed. The guard checks that it equals the embedded copy. |
| `parent.toml` | What the parent committed to: the screen and layout digests, an adoption proof and its two frames over fixed inputs (`proof_*`), the list of files, and each session's carried geometry. |

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

`generator.rs.txt` is the v0.92.0 generator, the template for the next
release.

## Adding the next release's fixtures

Do this once for each release, after its tag exists (see docs/RELEASING.md,
"Every release adds its handoff fixtures").

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
   - `fx_capture` must be that release's own capture path, not the v0.91.0
     loop that the template copies (0.92.0 still captured that way). From the
     first release carrying the 2026-09-22/23 update audit's producer ladder,
     that is `carry_for_wire` for each session and then `settle_wire_carries`
     over the pool. The repaint set is the sessions whose rung has
     `CarryRung::needs_repaint`.
   - From that release on, `write_outgoing` takes the repaint set after the
     screens and returns a `Result`.
   - Keep the five desks, and add a desk for any shape that release changed.
3. Run the generator, still in the worktree:

   ```sh
   ATERM_HANDOFF_FIXTURE_OUT=~/aterm/crates/aterm-gui/tests/fixtures/handoff/vX.Y.0 \
     targo --unverified test -p aterm-gui --lib -- generate_handoff_fixtures --ignored
   ```

   It refuses a desk that the release's own capture would not park, or that
   its own consumer would not adopt exactly.
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
