<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->
# The vendor corpus

Hook payloads a real Claude Code sent and a screen it painted, so
`tests/vendor_corpus.rs` judges aterm's readers against the program itself
rather than against hand-written payloads. One directory per vendor build,
named for its version:

```
2.1.278/
  manifest.toml                 provenance, plus size and sha256 per file
  PermissionRequest/0.json, 1.json   the `echo`, then the `rm`
  screen/usage.txt              one painted screen of `/usage`
  screen/usage.status           the session's `status` line beside it
```

`screen/usage.txt` ends at the vendor's `↓` scroll indicator; the windows the
replay reads are above it. `src/harness/usage.rs` also embeds this file with
`include_str!`.

The capture recorded through a hook bridge, and aterm installs nothing into an
agent, so nothing in the tree re-captures this corpus.

## What a green run means

```sh
targo --unverified test -p aterm-agent --test agent_suite -- vendor_corpus::
```

- The command in each `PermissionRequest`, drawn into a Bash permission box,
  is pressed (`1`) at full power (`approve = "all"`), and at least one is a
  box `approve = "safe"` would not press.
- The `/usage` screen yields its windows, including
  `seven_day_overage_included` from `Current week (Fable)`; the `status` line,
  the negative control, yields none.

A green run says nothing about what a later vendor build sends.

## Redaction

Machine names are rewritten before the manifest is computed: the home to
`/Users//owner`, the scratch directory to `/tmp/aterm-capture`, the session
scratchpad root to `/tmp/claude-agent`, each UUID to a distinct placeholder
(`00000001-0000-4000-8000-000000000001`, …), and the same paths in the
vendor's dash-mangled project-directory spelling (`/`, `.` and `_` become
`-`). Keys, nesting, types and path shapes survive, and payloads that shared a
session id still share one. The manifest's digests are of the redacted files
and record what was captured: a fixture is an observation, so never edit one.

## What is not in here

The other hook payloads the capture caught (`SessionStart`, `SessionEnd`,
`PreToolUse`, `PostToolUse`) were dropped with the hook bridge that read
them; git history keeps them.
