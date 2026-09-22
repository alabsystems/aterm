<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->
# The vendor corpus

Bytes a real Claude Code really sent, and screens it really painted, kept so
the harness can be judged against the program it wraps rather than against
this repository's beliefs about that program.

One directory per vendor build, named for the version:

```
2.1.278/
  manifest.toml            provenance + a digest per file
  SessionStart/0.json      one file per payload, in arrival order
  SessionEnd/0.json
  PermissionRequest/0.json, /1.json    the `echo`, then the `rm`
  PreToolUse/0.json, /1.json
  PostToolUse/0.json, /1.json
  StatusLine/0.json …      the statusLine JSON, once per refresh
  screen/usage.txt         what the vendor PAINTED (rank-1 evidence)
  screen/usage.status      the `status` line beside it
```

## How it got here

`tools/harness-capture.sh`. It installs the harness into a scratch settings
file, moves the installed `bridge.sh` aside and puts a recorder in its place,
spawns a session, drives three real turns over aterm's control socket, and
copies what the recorder caught. The recorder runs the real bridge on the same
bytes, so a capture run is also a live harness run — what the corpus records
and what the harness answered are one invocation, not two.

Re-run it after a Claude Code update:

```sh
tools/harness-capture.sh              # drive a live vendor, write a corpus
tools/harness-capture.sh --from-raw <scratch>/raw   # redact + manifest again, no vendor
targo --unverified test -p aterm-agent --test vendor_corpus
```

## These files are REDACTED, and that is stated rather than hidden

What the vendor sends names the machine it ran on: the owner's home, the
scratch directory's random name, the per-session scratchpad, the session
UUIDs. Those are rewritten to stable placeholders before the manifest is
computed — `/Users//owner`, `/tmp/aterm-capture`, `00000001-0000-4000-8000-…`
— including the vendor's own dash-mangled spelling of a project directory,
where `/`, `.` and `_` all become `-`.

Everything the corpus exists for survives that: every key, every nesting,
every type, every field the harness reads and every field it must tolerate
without reading. A path becomes a path of the same shape; a UUID becomes a
UUID; two payloads that shared a session id still share one.

The digests in `manifest.toml` are of the **redacted** files — the ones
checked in. They detect an edit to what is here, which is what they are for.
A fixture is an observation: to change one, re-capture.

## What a green `vendor_corpus` run means

For every payload this vendor build was observed to send, the harness reaches
a decision, that decision agrees with the pure policy that is supposed to
reach it, an event-class hook prints nothing, a decide-class hook prints
either nothing or a well-formed decision naming its own event, and every
firing leaves a ledger row. It does not mean the vendor will not send
something else tomorrow. It means that the day it does, a re-capture says so.

## What is NOT in here, and why

`StopFailure` and `PostModelSwitch` are not captured: one needs a real API
failure and the other a real model switch, and neither can be caused on
demand without faking the condition — which would make the fixture a
hand-written payload wearing a capture's clothes. They are exercised by the
hand-written suites in `src/harness/*_tests.rs`, which say plainly that that
is what they are.

`Notification` is not here either, and for a duller reason: the vendor sends
it when it wants the human's attention, and the three turns this capture
drives did not produce one. A run that idles at a prompt does. It is a hook
the harness registers, so a future capture may well carry it; nothing in the
replay requires it.

`screen/usage.txt` is ONE SCREEN, not the whole `/usage` page — it ends on
the vendor's own `↓` scroll indicator, so the "What's contributing to your
limits usage?" body below the fold is not in the corpus. The three windows
the replay reads (`Current session`, `Current week (all models)`, `Current
week (Fable)`) are all above it. The capture reads a frame, and a frame is
what a frame holds.
