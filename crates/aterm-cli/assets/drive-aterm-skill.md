---
name: drive-aterm
description: Drive, observe, and screenshot another aterm terminal session (another tab, window, or machine) over its control socket — for running things in a real interactive terminal, reading what a human would see, capturing pixels, waiting for a command to actually finish, or orchestrating several sessions. Use when asked to drive/control/watch another terminal or another agent's session.
---

<!-- aterm skill v1 — MANAGED FILE, written by `aterm agents install`.
     Edits are overwritten on update. To keep your own version, remove this
     marker line and aterm will leave the file alone (reported as `foreign`). -->

# Driving another aterm session

## There is NO MCP server — use the CLI or the library API

aterm ships **no MCP server**, deliberately. Do not look for one, do not add one. Two
supported paths:

1. **The CLI** — `aterm ctl`, `aterm drive`, `aterm fleet` (also the argv0 aliases
   `aterm-ctl`, `aterm-drive`, `aterm-fleet`; one binary serves all of them).
2. **The library API** — `aterm-ctl`'s `CtlClient`/`RelayClient`, `aterm-agent`'s
   `Turn`/`SelfGovernor`, for embedding rather than shelling out.

**The verb table is generated at build time — run `aterm ctl --help` for the authoritative
verb list and never guess a verb.** This file carries only the idioms `--help` does not.

## When to use this

- Run something in a *real* terminal and read what a human would see (TUIs, REPLs,
  installers, another coding agent).
- You need pixels, not just text (`image`).
- You need to wait for a turn to actually finish instead of `sleep`-and-hope.
- You are orchestrating several sessions at once (`fleet`).

Not for one-shot non-interactive commands — use Bash for those.

## Environment

| Var | Read by | Meaning |
|---|---|---|
| `ATERM_CONTROL_SOCK` | server **and** client | Server: bind this exact path. Client: default socket. `0`/`off` disables. |
| `ATERM_NO_CONTROL_SOCK` | server | Anything but `0`/empty disables the socket; wins over an explicit path. |
| `ATERM_PARENT_SESSION_ID` | **client** | Injected into every child shell. Powers `@self`, the ` *` marker in `ls`, and flagless socket resolution to *your own* instance. |
| `XDG_RUNTIME_DIR` | both | Rendezvous dir is `$XDG_RUNTIME_DIR/aterm`, else `~/Library/Application Support/aterm`. **Client needs the same value as the server** or discovery looks in the wrong place. |
| `ATERM_HEADLESS=1` | server | No window; engine + PTY + socket only. Same as `--headless`. |
| `ATERM_COLUMNS` / `ATERM_LINES` | server | Initial grid (clamped 20..=500 / 5..=300). |
| `ATERM_EXEC` | server | Run this in the PTY then exec `$SHELL` — deterministic paint instead of a host-specific prompt. |
| `ATERM_CTL` | `drive`, `fleet` | Path to the `aterm-ctl` client they shell out to. |
| `ATERM_CONTROL_TOKEN` | **only `aterm drive --dial`** | Not read by `aterm ctl` — that reads the sibling token *file*. |

Auth is automatic: a per-launch 32-byte token file sits beside the socket
(`aterm-<pid>.token` for a default socket; the fixed `aterm.token` for an explicit
`$ATERM_CONTROL_SOCK` path — **not** `<basename>.token`). Socket is 0600, same-uid only.

## Spin up a target (optional)

```sh
RUN=$(mktemp -d); SOCK="$RUN/c.sock"
ATERM_HEADLESS=1 ATERM_CONTROL_SOCK="$SOCK" XDG_RUNTIME_DIR="$RUN" \
  ATERM_COLUMNS=100 ATERM_LINES=30 SHELL=/bin/sh \
  aterm-gui >"$RUN/gui.log" 2>&1 &
for _ in $(seq 1 100); do [ -S "$SOCK" ] && break; sleep 0.1; done
```

Server announces on stderr: `aterm-gui: control socket listening at <PATH> (token-gated, same-uid only)`.

For a screen with no prompt noise:
`ATERM_EXEC='printf "\033[2J\033[H"; printf "READY\r\n"; exec sleep 86400'`

## Discover sessions

```sh
aterm ctl ls            # every session of every live instance
aterm ctl instances     # one line per instance
aterm ctl --sock "$SOCK" sessions   # one instance's registry (owner-only)
```

`ls`/`instances` are answered by the *client*, with no server round-trip — but they **do
honour `--sock`/`--pid`**, which **scope** the listing to the addressed instance. That is
the isolation lever: if you launched your own instance, address it and you will never be
handed the user's real terminals.

```sh
aterm ctl --sock "$SOCK" ls          # ONLY your instance
aterm ctl --pid 81608 instances      # ONLY that instance
aterm ctl ls                         # the whole fleet (unscoped)
```

A scoped listing that finds nothing says so distinctly and exits 1 — it never silently
widens to the fleet. Unscoped, the enumeration reads the rendezvous dir, so a custom
`XDG_RUNTIME_DIR` must match the server's.

*(`--sock`/`--pid` used to be silently ignored here, so a scoped `ls` returned the user's
real terminals. Fixed 2026-07-26; a stale build still has the old behavior.)*

Line shapes:
- `ls` → `<pid> <local> <sid> <parent|-> <state> <title-pct-encoded> meta=<0|1>[ *]`
- `sessions` → the same without the leading pid
- `instances` → `<pid> <session-count> <sock-abs-path>[ self]`

`*` / `self` mark the caller's own session/instance. The title is percent-encoded and **may
be empty**, producing two consecutive spaces — parse by field index carefully, or key only
on the sid (field 3 of `ls`, field 2 of `sessions`). Session ids are `s-` + 20 hex chars.

## Address a session

The selector is a **leading** argument, before the verb. Quote it so the shell doesn't eat `@`.

| Selector | Meaning |
|---|---|
| *(omitted)* or `@.` | the currently **active** tab — retargets when the human switches tabs |
| `@<sid>` | that stable session; relayed transparently if another instance hosts it |
| `@self` (alias `@env`) | expands client-side to `@$ATERM_PARENT_SESSION_ID` — your own session, stable across tab switches |
| `@<n>` | that instance's local tab id |

```sh
aterm ctl "@s-69d0080c3cc90873" text
aterm ctl @self image shot.png
aterm ctl --pid 81608 "@s-…" cursor
aterm ctl --sock "$SOCK" "@s-…" cursor
```

Targeting classes matter. **Session** verbs (`text screen cursor image turn await ready
wait send close` …) act on the resolved session. **App** verbs (`window chrome panes
controls tab open invoke settings metrics video spawn inspect`) route the selector to the
*instance* and act on its **front window**. **Meta** verbs (`sessions who whoami version
help grant dial*`) reject a selector outright.

Asymmetry: `spawn` is App-targeted (`aterm ctl spawn` → `OK <sid>`), `close` is
Session-targeted (`aterm ctl "@<sid>" close` → `OK closed <sid>`).

## Observe

```sh
aterm ctl "@$SID" text          # visible screen, one row per line — the CHEAP read
aterm ctl "@$SID" text --json   # {"rows":[…],"cursor":{…},"dims":{…},"seq":N}
aterm ctl "@$SID" cursor        # OK <row> <col> <visible 0|1> <style>   (0-based)
aterm ctl "@$SID" dims
aterm ctl "@$SID" screen        # lossless styled JSON — one enormous line
aterm ctl "@$SID" search 'panic'
```

- `text` takes **no arguments** — extra tokens are silently ignored, so `text 20` is *not*
  "last 20 lines". Returns exactly `rows()` lines, blanks included, trailing spaces trimmed.
- `screen` is always JSON and budgets ~213 bytes per cell — **~400 KB on a 24×80 grid**.
  Reach for `text` (130 B blank, 1.6 KB full) unless you genuinely need attributes.
- `search` replies `OK <n>` (or `OK <n> incomplete` when scrollback was evicted mid-scan),
  then one `<row> <col> <len>` per match where **row is the ABSOLUTE scrollback row**.
- `--json` goes *after* the verb, honored only by `text screen cursor dims blocks edges grants`.
- Free-text fields anywhere (titles, `history` `text=`, meta values) are percent-encoded.

### Pixels

```sh
resp=$(aterm ctl "@$SID" image shot.png)
path=$(printf '%s' "$resp" | cut -d' ' -f4-)   # rest-of-line: the path CONTAINS SPACES
aterm ctl "@$SID" image plain clean.png        # suppress cursor trail / sparkle / scene
aterm ctl "@$SID" image --bytes                # OK 1 + "<w> <h> <nbytes> <base64-png>"
```

- Reply is `OK <w> <h> <path>`. On macOS the path contains `Application Support` —
  **never** `awk '{print $4}'`; take the rest of the line.
- Filename must be a **bare filename**; captures are confined to `<socket-dir>/images/`.
- **A background tab cannot be screenshotted:** `ERR no window displays the target session
  (background tab?)`, and no file is written. `text`/`screen` have no such limit.
- `--bytes` is the only capture form a remote (`dial`) driver can use, since a path names
  the *server's* filesystem. PNG is 8-bit RGB, no alpha, full device-pixel (Retina 2×) —
  budget ~0.8–1.2 MB of base64 per shot.
- `OK` means the file is fully written and readable.

## Act

```sh
aterm ctl "@$SID" send 'echo hi\n'   # raw to the PTY; trailing literal \n becomes CR
aterm ctl "@$SID" key enter
aterm ctl "@$SID" ctrl c
aterm ctl "@$SID" paste 'some text'  # bracketed-paste seam
aterm ctl "@$SID" resize 30 100      # ROWS first
```

`send|key|ctrl|feed|mouse|paste` reply `OK seq=<n>` — the content baseline *before* the
input. `send` writes straight to the PTY and builds no input event, so typing-reactive
effects stay inert; use `key` when you want "a human typed this" provenance.

### Exact bytes without shell-quoting hell

Request lines are newline-delimited and args are joined with single spaces, so inline
`send` **collapses internal whitespace and rejects embedded newlines**. For anything
multi-line or whitespace-exact, use the stdin-payload forms (length-prefixed binary frames):

```sh
printf 'line one\nline two\n' | aterm ctl "@$SID" send --stdin   # RAW, verbatim
cat script.py                 | aterm ctl "@$SID" paste --stdin  # through the PASTE seam
cat blob.bin                  | aterm ctl "@$SID" feed-bin       # RAW, no inline length
```

- `send --stdin` (alias `send -`) and `feed-bin` are the same raw frame. `paste --stdin`
  keeps paste semantics: control-byte sanitize + `ESC[200~/201~` when the app enabled DECSET 2004.
- Each payload caps at **256 KiB**. A leading `@<sel>` routes the frame to a peer session.

### One human turn (the primitive you usually want)

```sh
aterm ctl "@$SID" turn 'echo HELLO'
aterm ctl "@$SID" turn idle=800 timeout=30000 'make test'
aterm ctl "@$SID" turn submit=none 'draft text'
aterm ctl "@$SID" turn settle=match:'BUILD SUCCESSFUL' 'make'
```

`turn` types the text, verifies the submit landed (re-pressing if needed), waits for
settle, and returns the settled screen.

**The reply is split across two streams — verdict to stderr, rows to stdout:**

```
stderr: aterm-ctl: turn submitted=1 status=settled seq=9 id=1 dur_ms=1658 hash=ffaf3105907867ca
stdout: <the settled screen, one row per line>
```

```sh
aterm ctl "@$SID" turn 'cmd' 2>/dev/null     # rows only
aterm ctl "@$SID" turn 'cmd' 2>&1 >/dev/null # verdict only
```

Defaults: `idle=1500 timeout=240000 submit=enter submit_window=2000 presses=3
submit_verify=auto`. Options are `k=v` tokens parsed only from the **leading run** — the
first non-option token ends parsing and everything after is the message verbatim. `status`
is only `settled` or `timeout`; `submitted=0` means no press verifiably landed. `presses=1`
disables re-press where a duplicate Enter would be harmful.

Busy paths: `ERR busy turn=<id>` or `ERR busy lease=<holder>`.

### Cooperative lease (advisory)

```sh
aterm ctl "@$SID" lease acquire ttl=5000 holder=agent-a
aterm ctl who        # 0 s-… driving=lease:agent-a watchers=0 turns=3 alive
aterm ctl "@$SID" lease release holder=agent-a
```

Advisory — it blocks `turn`, but raw `send`/`key` still go through. `turn` is the hard arbiter.

## Wait for completion

Event-driven server-side, no polling. Server timeouts are **milliseconds**, clamped to 600000.

```sh
aterm ctl "@$SID" ready                              # OK ready prompt | OK ready idle | OK timeout
aterm ctl "@$SID" await idle 400 timeout 5000        # OK idle <seq>
aterm ctl "@$SID" await match 'DONE' timeout 20000   # OK match <seq>
aterm ctl "@$SID" await block                        # OK block <seq>
aterm ctl "@$SID" wait 30000                         # OK complete <id> exit=<code|->
```

- **`await seq <n>` is the cheap dirty check** — level-triggered, so it latches immediately
  when `content_seq` has *already* moved past `<n>`. Record the `seq` from `text --json`
  (or an input verb's `OK … seq=<n>` reply) one turn, pass it back the next:
  `await seq <n> timeout 0` answers `OK seq <new>` (~15 bytes) or `OK timeout`, instead of
  re-reading the screen. *(It was edge-triggered and silently answered `OK timeout` until
  the fix in `observe.rs`; a stale build will still show the old behavior.)*
- **Caveat: `seq` is per-grid.** An alt-screen (1049) round trip leaves the main grid's
  counter untouched, so a seq check alone can miss a whole TUI session. It also does not
  move on render-only changes (OSC recolor, DECSCNM, DECTCEM) or cursor moves.
- `await` accepts `timeout <ms>` and `timeout=<ms>` from any position. `await match` also
  takes `rows <a> <b>`. Bad regex → `ERR badregex`.
- `ready` accepts a bare `<ms>` or `timeout=<ms>` (default 30000). `wait` accepts a **bare**
  `<ms>` only.
- **`wait`, `await block`, and `blocks` require OSC-133 shell integration.** Against a plain
  `/bin/sh` they return `OK timeout` / `OK 0`. Fall back to `await match`, or type
  `<cmd>; echo SENTINEL` and wait for the sentinel.

### Incremental history (better than re-reading the screen)

```sh
aterm ctl "@$SID" history since=<id>    # 512-record turn ledger
aterm ctl "@$SID" timeline since=<id>   # 512 events
aterm ctl "@$SID" blocks; aterm ctl "@$SID" blocktext <id>   # 1000-block store
```

These give **true incremental content** rather than a re-sent screen — prefer them when
shell integration is available.

### Exit codes

`0` = OK. `1` = usage/connect error or server `ERR`. **`124` = timeout** (client deadline,
or server `OK timeout`, or a `turn` verdict with `status=timeout`).

Unit mismatch: the client's `--timeout` is in **SECONDS** (default 900, `0` disables);
every server-side verb timeout is in **milliseconds**.

### Reply framing

Status-framed verbs print their `OK …` line to stdout. Lines-framed verbs (`text`,
`blocks`, `history`, `search`, `image read`, anything `--json`) send an `OK <n>` header the
client **consumes** — only the `n` body lines reach stdout. Zero count is announced on
stderr as `… (<verb>: no results)` with exit 0. `ERR` goes to stderr with exit 1.

## The live stream

```sh
aterm ctl subscribe "@$SID" screen,events
aterm ctl --timeout 3 subscribe "@$SID1,@$SID2" events,sessions
aterm ctl subscribe "@$SID" cells,ts since=1234 every-frame
```

Grammar: `subscribe [@<sel>[,<sel>…]] <streams> [since=<n>] [since-turn=<n>]
[since-block=<n>] [every-frame]`. **The selector is the SECOND token here** — the one verb
where it follows the verb name. Streams ⊆ `screen,cursor,events,cells,bytes,sessions,timestamps|ts`,
comma-joined into the **single `<streams>` token** — `ts` is a modifier of that token, not a
trailing arg, so `… cells since=1234 ts` is `ERR unknown subscribe arg` while `… cells,ts
since=1234` is what you want. Fail-closed on unknown tokens **and on a modifier-only list**
(bare `ts` is `ERR usage` — it would ack and then push nothing forever). `sessions` is
**Owner-only**: it reports the whole instance roster, which your per-target read grants do not
cover, so a scoped edge gets `ERR denied` rather than an empty stream. Max 256 targets;
`since=` anchors require a single target.

After the ack the connection is **push-only forever**.

- Ack `OK subscribe <n>` → **stderr**; frames then go to stdout verbatim.
- `sub <local> <sid>` — learn this map first; every later frame is tagged by `<local>`.
- `DELTA <local> seq=<n> screen <nrows>` + rows
- `DELTA <local> seq=<n> cursor <row> <col> <visible> <style>`
- `DELTA <local> seq=<n> cells <nbytes>` + bytes + newline
- `EVENT <local> turn <id> submitted= status= dur_ms=` / `block-complete <id> exit=` / `title` / `bell` / `exited`
- `BYTES <local> <len>` + raw PTY bytes
- `GAP <local> resync=<seq>` | `bytes-dropped=<n>` | `events-resync=<floor>`
- `EVENT * session-created <sid>` / `session-exited <sid>` — `sessions` only; `*` is the
  instance tag, not a `<local>`, so it resolves against no `sub` map entry.
- `T <tag> <t_us>` — `ts` only, once per channel per wake, immediately before that
  channel's frames. `<tag>` is a `<local>` for session frames and `*` for `EVENT *`, so
  **do not parse the second token as a number**.

Semantics that matter:

- **Every `screen`/`cells` DELTA is a FULL SNAPSHOT, not a diff.** Frames are independent
  and idempotent. A `seq` jump is **coalescing, not loss** — there is no delta history.
- `since=<n>` does **not** replay the interval: it seeds a watermark. Live seq > n → one
  snapshot immediately; equal → silence until content moves; less → `GAP resync=` + snapshot.
- `GAP resync=` fires on backward counter (engine reset) or an **alt-screen (1049) flip** —
  the alt grid has its own counter. Treat the next DELTA as a fresh baseline.
- `content_seq` is **per-session and per-grid**. Never compare across sessions; expect it to
  go stale across an alt-screen swap.
- Event replay uses `since-turn=`/`since-block=` against real bounded ledgers. `bytes` is
  live-only, 4 MiB queue, overflow → `GAP bytes-dropped=`.
- **Once `bytes` is in the list, do not parse line-by-line** — raw bodies contain newlines.
- Explicit `--timeout N` on `subscribe` is a **max-watch wall clock**: flush, then exit 124.

## Remote

```sh
aterm ctl dial-list
aterm ctl dial <name> text             # ONE verb on the remote; reply is the REMOTE's
aterm drive --dial <name> prompt 'run the tests'
```

- **`dial <name>` with no verb is rejected** — a bare dial would deadlock a one-shot client.
- Dialing out is owner-only. Use `image --bytes` remotely (a path names the server's disk).
- `aterm drive --dial` needs `$ATERM_CONTROL_TOKEN` set explicitly — its fallback derives
  `<sock without .sock>.token`, which does **not** match `aterm ctl`'s convention.

## Fleet

```sh
aterm fleet events > fleet.ndjson
printf '@%s turn make test\n@%s text\n' "$SID1" "$SID2" | aterm fleet exec
```

`events` merges `subscribe … events` across every live instance as NDJSON, rescanning every
second so new instances federate too:
`{"subject":"/fleet/<pid>/events/<sid>","instance":"<pid>","sid":"<sid>","event":"turn 3 …"}`.
`GAP` frames are dropped — `events` is a lossy digest by design.

`exec` reads `@<sid> <verb> [args…]` lines from stdin (blank/`#` skipped) and emits
`{"subject":"/fleet/commands/<sid>/result","sid":"…","ok":true,"reply":"…"}`. Everything
after the verb is passed as **one** argument, so payload spacing survives. `reply` is stdout
on success, stderr on failure — so a `turn` verdict lands in `reply` only when it failed.

`fleet` is pure glue over `aterm-ctl` and depends on default-dir discovery — same
`XDG_RUNTIME_DIR` caveat as `ls`.

## `aterm drive` — the sugar wrapper

```sh
aterm drive --socket "$SOCK" read
aterm drive --socket "$SOCK" --idle 800 --timeout 20000 prompt 'echo hi'
aterm drive --socket "$SOCK" --ready '^\$ ' prompt 'make test'   # a shell prompt
aterm drive --socket "$SOCK" --ready '' prompt 'echo hi'         # idle-only
aterm drive --socket "$SOCK" await match 'BUILD SUCCESSFUL'
aterm drive --socket "$SOCK" shot out.png
```

`prompt` is `send` → `key enter` → `await idle <ms> timeout <ms>` → best-effort
`await match <ready-pattern>` → `text`. Its `--idle`/`--timeout` are **milliseconds**
(600 / 180000), unlike `aterm ctl --timeout` in seconds.

**`--ready REGEX`** sets the prompt-ready row pattern for that final best-effort settle.
The default matches a **Claude** input caret (`(^|\s)❯(\s|$)`) — right only when the driven
program *is* Claude. Point it at your own REPL's prompt otherwise, or pass `''` for
idle-only. Also settable via `$ATERM_DRIVE_READY` (the flag wins). A non-matching pattern
costs a bounded extra wait, never a failed turn.

`drive` **shells out** to `aterm-ctl`, so through a bare symlink with no sibling client it
fails with `could not run aterm-ctl`. Set `$ATERM_CTL`, or just use `aterm ctl` — that path
is in-process.

## Gotchas (quick reference)

1. `ls`/`instances` are client-answered but DO honour `--sock`/`--pid` — use them to scope
   to your own instance. Unscoped, match `XDG_RUNTIME_DIR` or you list the wrong terminals.
2. `turn` verdict → stderr, rows → stdout.
3. Exit 124 = timeout, not failure-to-connect.
4. `await seq <n> timeout 0` is the cheap "did anything change?" check — but `seq` is
   per-grid and misses alt-screen flips, recolors, and cursor moves.
5. `wait`/`await block`/`blocks` are silent no-ops without OSC-133.
6. `image` can't capture a background tab, needs a bare filename, returns a path with spaces.
7. Inline `send` collapses whitespace and forbids newlines — use `--stdin` forms (≤256 KiB).
8. `search` rows are absolute scrollback rows; header may carry ` incomplete`.
9. `screen` is ~400 KB; `text` is the cheap read.
10. Client `--timeout` is seconds; server-side timeouts are milliseconds (cap 600000).
11. A subscribe `seq` skip is coalescing, never loss; a `GAP resync=` is a real discontinuity.
12. `@.` follows the human's tab switches; `@self` and `@<sid>` do not.
