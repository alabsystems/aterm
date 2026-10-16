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
  SessionStart/0.json           one file per payload, in arrival order
  SessionEnd/0.json
  PermissionRequest/0.json, 1.json   the `echo`, then the `rm`
  PreToolUse/0.json, 1.json
  PostToolUse/0.json, 1.json
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

- Every payload parses, carries a `session_id`, and names its directory's
  event; each build has `PermissionRequest` and `PreToolUse` payloads.
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
