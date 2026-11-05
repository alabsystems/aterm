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
directory per desk, `rendezvous.toml` and, from v0.107.0, `fields.toml`.
v0.96.0 and v0.99.0 were never cut and have no directory.

**A red guard is never fixed by regenerating or editing a fixture.** Those
bytes are what that release sends for as long as installs of it exist. Fix the
consumer so it admits them again: consumer changes only ever become more
lenient.

## The desk guard

`seamless::fixture_tests::every_shipped_producers_desk_adopts_exactly_and_proves`
(`crates/aterm-gui/src/seamless_fixture_tests.rs`) runs every desk of every
release through this build's `take_incoming`. It judges against the desk's
`parent.toml` and, for a record's column debt, the blocks a full clear blanked
and the Settings drafts, against the producer's own bytes. It asserts that:

- every session is adopted exactly: repainted only when its producer flagged
  it, each screen byte for byte at its carried geometry, each control carry
  read, the layout placed;
- each meta is reassembled as the parent recorded it, and each session owes its
  program exactly the render debt its record carries;
- the screen and layout digests, the adoption proof and its `ProofReady` and
  `Commit` frames are the parent's, byte for byte;
- every history sidecar is taken before the proof at the depth it stamps, and
  after Commit its lines import in front of the checkpoint's, cell for cell,
  losing only `history_dropped`;
- after the import, application colours are live, every carried mark keeps its
  rows, columns and exit, every block keeps exactly its recorded full-clear
  flags (`text_cleared`, `command_text_cleared`), and every shell mark still
  names its line (every shell on these desks prompts as `dev@devhost`). A
  repainted session's marks are not read, nor a prompt row a full clear
  blanked unless an uncleared block prompts there. The blanked blocks are read off the parent's checkpoint, whose flags
  are first held to the producer's `s<id>.meta.json` read with `aterm_json`'s
  value tree, never this build's decode;
- every Settings draft is placed on the leaf that carried it, none dropped as
  unreadable;
- every held pane is shown in manifest order at its geometry, without
  scrollback or clickable links, with its exit, session id, control carry and
  whether its failure was seen; its sidecar imports with every link dropped
  (`assert_held_history_imports`).

The guard reads history sidecars with the line codec, not the consumer's
reader, because the import is what it checks. It counts the layout's raw
`[[….settings_drafts]]` tables, independent of the consumer, but reads each
draft's text with `aterm_toml`, which the consumer's parse also uses. A
record's render debt is the rows `parent.toml` records (`render_debt`) plus the
manifest record's own `render_debt_cut` columns, read off its TOML tree; from
v0.109.0 `parent.toml` copies the columns too, and they must agree.

Its negative controls, the `a_consumer_that_…` tests, drop the shell state,
lose or move a Settings draft, drop column debt, move a full clear's flags or
rows, or decode a meta without its clear flags, all on the consumer's side, so
the digests and proof still pass and the targeted check must catch it.
`every_release_since_the_floor_pins_the_required_desks` checks that every
release pins each `REQUIRED_DESKS` desk from its floor on.

## What the desks cover: `fields.toml`

The guard checks only the shapes its desks carry. Each release's `fields.toml`
records, per record (`manifest`, `layout`, `meta`, `control`), the fields its
desks wrote with a value and those left unwritten (`Struct.field`), and the
variants of each reached enum they took and did not (`Enum::Variant`). The
lists are read off the serde derives (`seamless_fixture_cover.rs`), so a new
field or variant is in the next walk with no table to update.

The generator holds the desks strictly: every field and variant a desk reaches
must be written by some desk with the value a real session carries, or be one
no handoff writes; and everything the newest recorded release wrote must still
be written. Anything else stops `generate_handoff_fixtures`, in every ordinary
`-p aterm-gui --lib` run and in the cut, one `<record> <field> (<why>)` line
each. Fix it in a desk, as a real session, program or host would.

The hold cannot see what serde does not spell: a string is one field, and a
desk can stand for a state that writes no field of its own (a parser stopped
mid-sequence, a policy's path). A release that adds such a state adds a desk
for it, and a `REQUIRED_DESKS` row with that release as the floor once its
fixtures are checked in.

A field or variant no handoff writes (one another writer sets, as the crash
carry's `Wire.turn_ids`; retired; or set by nothing, as DECSCL's missing
levels) goes in the generator's `NO_HANDOFF_WRITES` with why. The hold refuses
a row there that a desk writes, or that neither the current walk reaches nor
the newest recorded release wrote. So a deleted field's row goes once a release
records its omission (v0.110.0 no longer records the deleted
`SessionRecord.identity` and `TerminalLeafRestore.identity`); older fixtures
still hold the consumer to their bytes.

## The other direction

The guard runs one way because an update hands only to a newer build
(`native_updater_service`'s
`no_update_authority_names_a_build_at_or_below_the_running_one`); the one
same-image hop (the QA seam, a launch-record renewal) runs this very build, and
an older bundle a person puts back starts cold.

## What a desk directory holds

`<p>` is `seamless-<pid>-<nonce>`.

| file | what it is |
| --- | --- |
| `<p>.toml` | The manifest: the nonce on the first line, then the TOML. |
| `<p>.layout.toml` | The layout sidecar. |
| `<p>.s<id>.grid` / `.altgrid` | A session's main and inactive grid sidecars. |
| `<p>.s<id>.ctl` | A session's control-carry sidecar. |
| `<p>.s<id>.hist` | A session's history-carry sidecar (v0.94.0 on, when its history is deeper than its checkpoint carries). |
| `<p>.h<id>.heldgrid` / `.heldalt` | A held pane's main and inactive grid sidecars (v0.100.0 on). |
| `<p>.h<id>.hist` / `.h<id>.ctl` | A held pane's history-carry and control-carry sidecars (v0.101.0 on). |
| `s<id>.meta.json` | A copy of the meta JSON the manifest embeds, for review; the guard checks it equals the embedded copy. |
| `parent.toml` | What the parent committed to (below). |

Every file is as the producer wrote it, except that the manifest's absolute
grid sidecar paths name `@HANDOFF_DIR@` (`dir_placeholder`) for the producer's
private directory; the guard puts the staging directory back. No digest covers
the manifest's bytes, only its meta strings, and the guard recomputes the
recorded digests from the files.

### `parent.toml`

Every release records `producer_version`, `producer_commit` and
`outgoing_build` (the release's identity, which the cut checks), `manifest`,
`nonce`, `dir_placeholder`, `screen_digest`, `layout_digest`, `files`, an
adoption proof and its two frames over fixed inputs (`proof_target_build`,
`proof_target_commit`, `proof_identities`, `proof_count`, `proof_digest`,
`ready_wire`, `commit_wire`), and a `[[session]]` row per session (`local_id`,
`rows`, `cols`, `history_lines`, `alt_grid`, `control`). Later releases add:

| from | keys | what they record | guard |
| --- | --- | --- | --- |
| v0.92.0 | `require_shell_integration_nonce`, `shell_integration_nonce` | the shell-integration posture | checked |
| v0.94.0 | `history_take`, `history_lost`, `fg_holder` | the lines the `.hist` sidecar carries (`0`: none), the running loss, the foreground holder | checked against the manifest, the sidecar and the import |
| v0.95.0 | `absolute_row_counter`, `alt_absolute_row_counter`, `color`, `shell_phase`, `shell_marks`, `shell_completed_seq` | the row numbering, whether a colour diff rides the meta, the shell-integration state | checked against the reassembled checkpoint |
| v0.97.0 | `park_carry_ceiling` (top level), `rung`, `links_stripped` | the park's ceiling, the `CarryRung` (`full`, `stripped-links`, `visible-only`, `repaint`), the lines that lost their OSC 8 links | not read |
| v0.97.0 | `repaint`, `history_dropped` | flagged for a redraw; the history lines this hop leaves behind | checked; absent means none |
| v0.98.0 | `history_links_stripped` | the `.hist` lines written without their links | not read |
| v0.100.0 | `[[held]]`: `local_id`, `rows`, `cols`, `alt_grid`, `exit_code` or `exit_signal` | each held pane and how its command ended | checked |
| v0.101.0 | `output_state`, `render_debt` | whether the meta carries the in-flight output state; the rows owed a carried alternate screen (`"<shift>,<pushed>"`, absent for none) | checked |
| v0.101.0 | `[[held]]`: `sid`, `history_take`, `history_dropped`, `control`, `failure_seen` | what a held pane carries beside its screen | checked |
| v0.109.0 | `render_debt_cut` | the columns the record owes (absent for none) | checked against the wire |

## The desks

Each desk is listed with the first release that writes it. `REQUIRED_DESKS`
makes the thirteen desks through `held-pane` mandatory from v0.97.0 on
(`link-dense-history` from v0.98.0, `held-pane` from v0.100.0); v0.108.0's six
desks have no row.

- `incident-55x149` (v0.91.0): Claude Code in 1049, entered on the last row of
  a 56-row grid before the message band took a row (through v0.101.0 its DECSC
  slot is on row 55 of 55), beside a shell with 256 lines of styled history.
- `claude-code-1049` (v0.91.0): Claude Code in 1049 with a ledger turn, beside
  a shell.
- `history` (v0.91.0): three shells whose history exercises SGR colours, wide
  and combining text, OSC 8 and OSC 7, and terminal modes.
- `twelve-panes` (v0.91.0): twelve sessions in two windows.
- `shell-integration` (v0.92.0): shells signing OSC 133/633 marks with their
  tab's nonce and requiring it.
- `history-carry` (v0.93.0): 1,500 lines of history, and 1,000 under `less` in
  1049; from v0.94.0 each names a `.hist` sidecar.
- `stalled-sequence` (v0.93.0; not in v0.94.0 or v0.95.0): parsers stopped over
  a quiet PTY in an OSC title (`OscString`), an SGR (`CsiParam`) and an APC in
  1049 (`SosPmApcString`).
- `colour-and-shell` (v0.95.0): a signed shell with a base16 theme, 150
  finished commands and a running `cargo build`, beside vim in 1049 that pushed
  colours (OSC 30001) under an unsigned shell.
- `link-dense` (v0.97.0): rows of twelve 8 KiB links carried at
  `stripped-links`, and a row of two under the per-record cap
  (`16 KiB + cols * 512`) that keeps them. No over-cap line is in a `.hist`
  sidecar, because v0.97.0's own consumer refuses one; `handoff_history_tests`'
  `a_link_dense_line_a_shipped_producer_wrote_costs_only_its_links` holds the
  leniency for those v0.94.0 through v0.97.0 wrote.
- `policy-visible` and `policy-repaint` (v0.97.0): parked under
  `carry = "visible"` and `carry = "repaint"` (a blank canonical screen, and
  before v0.100.0 no control carry), counting every line left behind.
- `link-dense-history` (v0.98.0): a dense row in the imported part of a `.hist`
  sidecar (`history_take = 480`), one stripped by the screen carry, and a
  Settings tab (`/text-fonts`) with an unsaved `font_family` draft.
- `held-pane` (v0.100.0): two `--hold` panes, a `make` that exited 2 and a vim
  killed by SIGKILL in 1049, named by the manifest's `held` list in sidecars
  outside every digest; an older successor keeps its placeholder. From v0.101.0
  each names a `.h<id>.hist` (11 and 12 lines) and a `.h<id>.ctl`.
- `carried-state` (v0.108.0): what sessions, windows and the desk carry beside
  the screens after a crash and a cold restore: driven shells, mailboxes, owed
  agent relaunches, a `lost` agent, the band's notices. Gemini CLI is retired,
  so a consumer reads its leaf as no agent's; desks from v0.140.0 write that
  session as a plain shell.
- `engine-state` (v0.108.0): meta state no other desk writes: every carried
  parser state, charsets, pushed titles and colours, saved modes and cursors,
  an open OSC 8 link, VT52 mid-address, htop narrowed from 100 columns to 80
  after drawing 99 (`render_debt_cut = 19`), shell integration revoked, held,
  stale or cleared, and Claude Code's two drawers. From v0.137.0 an eighteenth
  session holds Codex's OSC 7501 program state.
- `held-failed` and `held-withheld` (v0.108.0): a held failed `make` whose lines
  are left behind as a failure (`history_failed`) or as the policy's choice
  (`history_withheld`).
- `fold` (v0.108.0): 65 windows, one past `MAX_WINDOWS`, so the layout folds.
- `variants` (v0.108.0): every carried enum variant no other desk takes.
- `lone-workers-list` (v0.155.0): a window that held only an empty Workers list,
  with no session, which a parent sends only to a target that declares `desk1`.

## Producer shapes by release

What each producer changed that reaches these bytes, up to v0.108.0. From
v0.107.0 each release's `fields.toml` lists what its desks write; compare two
releases' files for what a later producer added or dropped. The hand-adapted
generators that wrote v0.91.0 through v0.102.0 are in git history
(`generator*.rs.txt`, last at 86b918a4e); v0.103.0 on, the cut runs the
in-tree one.

- **v0.93.0**: the capture is the producer ladder (`carry_for_wire`, then
  `settle_wire_carries`). A parser left mid-sequence is carried without the
  partial sequence, as CAN would (`Terminal::checkpoint_carry_abandoning_partial`);
  earlier releases refuse to hand off such a session.
- **v0.94.0**: records carry `fg_holder`, `questions` (on the layout leaf
  too), `rekey`, `loader` and `history_lost`; history deeper than the
  checkpoint carries (256 lines, or none under an alternate screen) goes in a
  `.s<id>.hist` sidecar (`history = "<len> <sha> <take>"`).
- **v0.95.0**: `CheckpointMeta` adds the row counters, `color` and `shell`.
- **v0.97.0**: every park captures under the successor's signed policy
  (`App::park_policy`). Releases ship `publish/handoff-policy.toml` empty, so
  all but the policy desks park at `Full`. An over-cap line loses only its
  OSC 8 links (`CarryRung::StrippedLinks`).
- **v0.98.0**: the history export strips over-cap lines' links, so every
  sidecar frame decodes under the strict caps; layouts carry Settings drafts; a
  record writes `history_withheld` only when `true`.
- **v0.100.0**: a partial escape sequence rides the meta (`parser`); only a
  hooked DCS is still abandoned. Held panes arrive. Records add
  `claim_known = true` and `timeline_id`, the manifest `roster_seq`, and
  layouts the legacy mirrors (`tabs`, `native_tabs`, `tab_order`) beside
  `restored_tabs` and, on macOS, the window's `show`.
- **v0.101.0**: `output_state`, `render_debt` (`incident-55x149` owes `-1,0`)
  and held panes' history and control carry; `next_turn_id` becomes the ids
  minted plus `TURN_ID_RESERVE`; windows name `inner_px` and the window carry
  `messages_captured_unix_ms`. v0.101.0's own consumer kept
  held-pane links clickable, so link dropping is checked against this build
  only.
- **v0.102.0**: the sidecars are v0.101.0's byte for byte; each meta adds
  `"win32_input_enabled":false`, and the DECSC slot follows its text across a
  resize (`Grid::lend_saved_cursor`), so `incident-55x149`'s slot is row 54;
  the guard still admits the earlier slot past the grid.
- **v0.108.0**: every `.ctl` carries its session log (`log`); records carry
  `log_ids` and `log_seq`; terminal leaves carry `sid`, `log_ep` and the
  operator's metadata.

## The rendezvous transport

On macOS the successor gets its PTY masters and the readiness and Commit pipes
over the rendezvous (`crates/aterm-gui/src/handoff_rendezvous.rs`), not on
disk. It dials a socket the parent bound and claims with a frame (`ATRZ1C` and
the claim secret); the parent answers with a grant, an `ATRZ1G` header carrying
every descriptor through `SCM_RIGHTS`, then a body naming each session. A
change to a magic, the header, the body or the descriptor order passes every
same-build test and fails every update, so each release directory holds
`rendezvous.toml`: what that release's transport does over fixed inputs
(`claim_input`, `grant_nonce`, `grant_sessions`, `proof_layout_digest`,
`proof_screen_digest`).

| key | what it is |
| --- | --- |
| `claim_frame_hex` | The claim that release's dialer (`dial_for_test`) writes for `claim_input`, read off a plain listener. |
| `grant_header_hex`, `grant_body` | The grant its parent sends through a real `Rendezvous::bind`, claim, `accept_claim` and `transfer` of three `openpty` masters and two pipes, received by `recv_with_fds`. |
| `descriptor_order` | The order the descriptors arrived in: a master by its `st_rdev`, a pipe end by its inode. |
| `env_rendezvous`, `rendezvous_file_shape`, `env_claim`, `claim_hex_len` | The launch variables' names, the socket's file name (pid and nonce elided) and the claim secret's length. |
| `env_proof_term`, `proof_term_launched`, `proof_term_fork` | The proof-term variable and the two values the parent writes. |
| `env_target`, `target_value` | `ATERM_SEAMLESS_TARGET` and that release's `encode_target_identity` of its own build and commit. |
| `env_parent_birth`, `parent_birth_value` | `ATERM_HANDOFF_PARENT_BIRTH` and the value that release's `outgoing_parent_env` published. Only its shape is read. |
| `proof_terms`, `proof_count`, `proof_digest` | The masters' PTY device terms (`proof_identities_in_device_terms`) and that release's `adoption_proof` over them. |

The claim secret's key is `claim_input`, not `claim_secret`: the public
export's gitleaks scan (built-in rules only; the publication engine refuses
repo-local allowlists) reads `…secret = "<hex>"` as a leaked API key. The
value is a fixed test input.

The guard is `seamless::fixture_tests::every_shipped_producers_rendezvous_grants_this_successor`
(macOS). Every release `PINNED_DESKS` pins has a record, and every record is
pinned. For each, it asserts that:

- this build's claim, grant header, body and descriptor order over the fixed
  inputs are the recorded ones, so a one-message grant (up to 62 sessions: 64
  descriptors less the two pipes) keeps its shape byte for byte;
- this build's successor (`dial_and_claim`), dialled at a stub parent that
  checks its claim and replays the recorded grant with fresh masters and pipes,
  pairs every `local_id` in its `ATERM_SEAMLESS_FDS` wire with the master sent
  for it and takes the readiness and Commit ends;
- the environment names are this build's, and the recorded target, birth
  record and proof-term values parse here (`parse_target_identity`,
  `ProcessBirth::from_wire`, `parse_proof_term`), never re-encoded;
- this build's `adoption_proof` over the recorded terms and target is the
  recorded proof.

## How a release adds its fixtures

`targo --unverified ship release` does it; there is no hand step.

The generator is `crates/aterm-gui/src/seamless_fixture_generator_tests.rs`,
mounted as `seamless::carry_tests::fixture_generator` in every
`targo --unverified test -p aterm-gui --lib` build. It calls the release's own
policy reader, capture ladder, history carry, writer and consumer, so a
producer change that breaks it fails to compile. Its two tests run in isolated
child processes, into a private directory under this build's own pins unless
an output directory is set (the cut and the hand run set it, and the pins):
`generate_handoff_fixtures` writes every desk and
`fields.toml`, and `generate_rendezvous_fixture` (macOS) writes
`rendezvous.toml` with the guard's own `capture_transport`. The release's own
consumer must adopt every desk exactly, the desks must cover `fields.toml`,
every desk must pass the desk guard's per-desk check
(`assert_written_desks_adopt`), and every manifest, layout and meta must read
back as the serde corpus's
`state_written_by_shipped_producers_answers_as_recorded` will read it
(`serde_corpus_tests::shipped_state_with`, answers written to
`ATERM_SHIPPED_STATE_OUT` when set). So a desk the guard cannot read fails the
tree that writes it, not main after the release lands it.

Nothing in it is edited per release. `ATERM_BUILD_GIT_COMMIT` (12 digits),
`ATERM_BUILD_GIT_COMMIT_FULL` and `SOURCE_DATE_EPOCH` (the ledger's build
number) are the pins: `fx_install_release_stamp` refuses to run without them
and installs them as the shipped binary's stamp, so `producer_version`,
`producer_commit` and `outgoing_build` are the release's. The version is the
tree's `[workspace.package] version`; the stamp's compiler is `trustc -vV` of
`$TRUST_STAGE2_BIN`, else of the `trustc` on PATH. The successor is a build a
day newer (`fx_target_build`) at the next MINOR (`fx_successor_version`).

The cut (`crates/aterm-release/src/fixtures.rs`):

1. The `build` step, once build n and commit C are reserved and before
   anything is claimed, runs `targo --unverified test -p aterm-gui --lib --
   generate_handoff_fixtures generate_rendezvous_fixture --test-threads 1` in
   `target-fixture-guard` with C's and n's pins and the prover's
   `TRUST_STAGE2_BIN`, into
   `<clone>-release.noindex/handoff-fixtures/vX.Y.0-<n>-<C12>/`, the
   shipped-state answers (`shipped-state.txt`) beside `vX.Y.0/`. A failing
   test, a missing `REQUIRED_DESKS` desk or `rendezvous.toml` (macOS), a desk
   not written as C and n, or no shipped-state answers refuses the release. A
   staged identity (the journal's `fixtures-staged`) is not run again.
2. The `after-cut` step, once the release is published, pushes one commit onto
   origin/main that adds `vX.Y.0/`, its `("vX.Y.0", "<desk>")` rows in
   `PINNED_DESKS` and its cases in `../serde-corpus/shipped-state.txt`, so the
   corpus's sweep stays green (a file that cannot take the cases gets the desks
   alone, and the step says so). A lost fast-forward race rebuilds on the new
   tip, up to five tries (`MAX_CLAIM_ATTEMPTS`); any other push failure refuses
   the step, and running the release again lands what is staged. A tip whose
   fixtures already pass the next cut's gate is left alone; one holding
   `vX.Y.0/` in a shape that gate refuses is never overwritten.

The next cut's gate (`gates::handoff_fixture_gate_at`) refuses a release whose
predecessor's fixtures are not checked in and pinned. `--dry-run` says what
both steps would do. `PINNED_DESKS` is the only table a release adds rows to;
the rendezvous guard pins the same releases.

A release that changes its producer's shape changes the generator in the same
commit and gives every field it adds a real session's value in a desk, its
layout built the way `App::capture_manifest` builds one (`fx_window`). A `REQUIRED_DESKS`
row is `("vX.Y.0", "<desk>")`: never move an existing row's floor, and never
give a new row a floor earlier than the release that first writes the desk,
because earlier fixtures are frozen.

### By hand

When a release's cut did not land its fixtures (an older cutter, or an
`after-cut` refusal that names this section), run that release's own generator
at its tag, in a worktree of its own, never in the shared checkout:

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
