<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Andrew Yates -->

# vendor/alab-harness — the shared harness library, as a copy pinned by tree id

`vendor/alab-harness` is the whole tree of one release tag of **alab-harness**, Andrew Yates's
private repository of the half every ALab harness shares (`~/alab-harness` on the owner's
machines). `vendor/alab-harness.pin` names that tag and its git tree id, as two lines:

```
tag=<vX.Y.Z>
tree=<40 lowercase hex digits>
```

alab-harness's own `tools/pin` writes the copy and the pin together, byte for byte; nothing else
may. It is first-party code (`aterm_forge::provenance::FIRST_PARTY_VENDORED`), copied rather than
taken as a submodule like `vendor/astream` because alab-harness has no remote every clone of aterm
reaches: a plain copy keeps public clones and hermetic releases complete.

## What aterm uses from it

| crate | how |
|---|---|
| `alab-policy` | the one source of the turn-end deciders: needs, credentials, Land's check, the go-ahead, Cruise's carry, the press guard, approval classification, the ladder, the words each act types, the screen-reader vocabulary and the policy keys and timings. The root `[workspace.dependencies]` points `alab-policy` at `vendor/alab-harness/crates/alab-policy`; aterm, aterm-agent, aterm-gui, aterm-lab, aterm-phase, aterm-slog and aterm-types inherit it. claude-code-alab carries an independent whole canonical pin for policy and wire. aterm-agent still runs its own sleeps, git-config and shell-startup readers; switching them onto alab-policy's is HX-21c. |
| `alab-wire` | carried with the tag, linked by nothing here (HX-15b1 links it). |

## The rule: change it in alab-harness, tag, repin

Never edit, format or build anything inside `vendor/alab-harness`. A fix lands in alab-harness,
passes its gate there (`targo --unverified test --locked`, `targo tippy --locked --all-targets --
-D warnings`, `targo fmt --check`) and is tagged; aterm then takes the tag whole:

```
~/alab-harness/tools/pin <aterm checkout> <tag>
git -C <aterm checkout> add -f vendor/alab-harness vendor/alab-harness.pin
targo update -p alab-policy          # when the tag's version moved
~/alab-harness/tools/check-pin <aterm checkout>
targo --unverified forge check
```

`-f` because the copy is whole even where an ignore rule names a path. Each receiving harness
chooses its own qualified canonical tag; its pin need not equal aterm's.

`verify vendor-out --into <claude-code-alab checkout>` projects only `aterm-phase` and
`aterm-agent-names`, with `vendor/VENDORED` sealing those projections. The receiving checkout
must first carry and stage its own whole canonical copy and pin, written by `tools/pin`.
Vendor-out checks that target pin independently before checking or writing projections, leaves
both canonical artifacts untouched, and makes phase depend on
`../alab-harness/crates/alab-policy`. No seal or new output may claim canonical paths; all
other unowned vendor files still refuse the write. Old flattened policy files are removed
only where the prior valid sealed list owns them. After a phase or name-table change lands,
regenerate and land those projections (AGENTS.md); aterm's repin alone does not update them.

## What holds it

- forge's `[OB-1]`, `aterm-verify` before any check runs, and `verify vendor-out` before it reads
  the copy (`aterm_forge::tree_pins`): the index's subtree at `vendor/alab-harness` is the pin's
  tree, and nothing under it or at the pin is unstaged, untracked or ignored. A build inside the
  copy leaves an ignored `target/`, which fails it. A merge left unresolved elsewhere in the
  checkout does not touch it; one inside the copy fails it.
- alab-harness's `tools/check-pin <checkout>` adds that the tag is that tree upstream.
- The root manifest excludes `vendor/alab-harness`: it is its own workspace, so its crates take
  their version and lints from its root manifest. Census reads `crates/` and `apps/`, and only its
  `seam-caller` guard also reads `vendor/alab-harness/crates/`, since aterm's release build
  compiles that code too: a debug-only item there that release-compiled code names fails it.
  `vendor/alab-harness/crates/alab-policy/clippy.toml` (no file, process, environment, network,
  clock or sleep access) is the purity check in census's place.
- `.gitattributes` marks it `-text`, so no line-ending conversion touches the pinned bytes.
- alab-policy's own tests run in alab-harness's gate at the tag, not in aterm's verify. The rows of
  `crates/aterm-agent/SPECS.md` under `../../vendor/alab-harness/` name them there.
