---
name: drive-aterm
description: Drive, observe and screenshot another aterm session (tab, window or machine) over its control socket. Use when asked to watch or control another terminal or another agent's session, wait for a command to really finish, or run several sessions at once.
---

<!-- aterm skill v3 — MANAGED FILE, written by `aterm agents install` and by aterm itself.
     Edits are overwritten on update. To keep your own version, remove this
     marker line and aterm will leave the file alone (reported as `foreign`). -->

# Driving another aterm session

`aterm ctl help` (one row per verb), `aterm ctl help <verb>` (one full entry), `aterm ctl
--help` (flags, exit codes), `aterm help` (the agent brief). A guessed verb answers `ERR
unknown verb`. There is no MCP server: use `aterm ctl`, `aterm drive`, `aterm fleet`, or
the `aterm-agent` library.

A one-shot non-interactive command belongs in your own shell, not in a session.

## Peers are often other agents

Text typed into an agent's prompt reaches it as its human's next instruction. Before a
`turn`, `send`, `key` or `paste` into a peer, read what is there:

- `aterm ctl "@$SID" status`: `detail=` is the running program (`claude`, `codex`,
  `targo%20test`; `-` when idle), `program=` the foreground program, `agent=` the
  server's verdict on an agent's screen (`busy|prompt|question|idle|wall:<kind>`),
  `supervisor=` other than `-` means its prompts are already answered
  (`aterm-harness@<pid>` is the window's own host), `human_ms=` how long since a person
  typed there, and `input=stalled|stopped` a frozen program whose screen looks live.
  When `detail=` is a shell builtin (`cd`, `export`), confirm with `text`.
- `aterm ctl "@$SID" meta`: `role=` is what its owner stamped.
- Reads (`text`, `image`, `status`, `blocks`) change nothing.

Stamp yourself so peers can tell: `aterm ctl @self meta set role agent:<your-name>`.

## Find and address sessions

```sh
aterm ctl windows              # one row per window
aterm ctl ls                   # every session of every instance; parse by key=, not by column
aterm ctl --sock "$SOCK" ls    # only the instance you launched
```

- The selector is a leading, quoted argument: `aterm ctl "@s-…" text`. `@self` is your
  own session, `@<sid>` is stable, and an omitted selector or `@.` follows the human's
  active tab.
- A listing that finds nothing prints why. A sandbox that refuses the socket is not
  worked around.

## A private instance of your own

```sh
RUN=$(mktemp -d); SOCK="$RUN/c.sock"
HOME="$RUN/home" XDG_RUNTIME_DIR="$RUN" SHELL=/bin/sh \
  aterm --headless --control-sock "$SOCK" --columns 100 --lines 30 >"$RUN/gui.log" 2>&1 & PID=$!
for _ in $(seq 1 100); do [ -S "$SOCK" ] && break; sleep 0.1; done
aterm ctl --sock "$SOCK" ls
kill "$PID"    # by PID: never pkill anything named aterm
```

End the launch with `-e /bin/sh -c '…'` (after the flags) for a deterministic screen.

## Observe

```sh
aterm ctl "@$SID" text trim             # the visible screen, the cheap read
aterm ctl "@$SID" text tail=20 trim     # a bottom-pinned TUI such as Claude Code
aterm ctl "@$SID" image shot.png        # OK <w> <h> <path>
aterm ctl "@$SID" blocks                # shell-integration command blocks (OSC-133)
```

- `screen` is the whole styled grid as JSON; `text` is almost always enough.
- `image` takes a bare filename and captures only a tab a window shows. The reply path
  contains spaces on macOS: take the rest of the line, not `$4`. A remote driver uses
  `image --bytes`.
- A session that answers `ERR no such session` is gone: `aterm ctl exits` says when, why
  and who closed it.
- A full-screen app drawing on the wrong rows after a resize: `status` (`render=`), then
  `resizes`, then `cast drift`. All three only read (`aterm ctl help cast`).

## Act

```sh
aterm ctl "@$SID" key enter
aterm ctl "@$SID" key if=Do.you.want.to.proceed 1                # pressed only if a visible row matches
printf 'line one\nline two\n' | aterm ctl "@$SID" send --stdin   # exact bytes
cat script.py | aterm ctl "@$SID" paste --stdin                  # with paste semantics
```

- Inline `send` rejects newlines; use the `--stdin` forms.
- `key if=<re>` checks and presses under one lock. `OK skipped` (exit 0) means no
  visible row matched and nothing was written: the box is gone or a different one is up,
  so read the screen again. `if-gen=<gen>` and `if-fp=<hash>` fence a press to the read
  you decided on.
- **A pattern is one whitespace-free token.** The wire splits on whitespace and nothing
  quotes: write `esc.to.interrupt`, not `'esc to interrupt'`, which arms `esc` and, in a
  `turn`, types the rest as the message.
- `send` writes raw bytes; `key` carries "a human typed this" provenance.

## One turn

```sh
aterm ctl "@$SID" turn 'make test'                                         # a shell
aterm ctl "@$SID" turn settle=gone:esc.to.interrupt timeout=600000 'msg'   # an interactive agent
```

`turn` types the text, verifies the submit landed, waits for it to settle and prints the
screen: the verdict on stderr, the rows on stdout. `ERR busy turn=<id>` or `ERR busy
lease=<holder>` means another driver has it.

Instructions for a model worker go by mail (`aterm drive task`), not the composer: a
`turn` arrives as one bracketed paste, which Claude Code treats as not its human's and
may question instead of acting on.

## Wait

- Idle-settle is not completion for an interactive agent: Claude Code sits static for
  seconds mid-turn with its prompt glyph showing. Its busy footer leaving is the signal:
  `await gone esc.to.interrupt timeout=600000` (level-triggered, so arm it only while the
  footer is up) or `turn settle=gone:…`, which waits for the footer to appear first.
  `await agent idle,prompt,question,wall` waits on the server's own verdict.
- `await seq <n> timeout=0` is the cheap "did anything change?" check. `seq` is per grid
  and repeats after an alternate-screen re-entry.
- `wait`, `await block` and `blocks` need OSC-133 shell integration. On a plain shell,
  type `<cmd>; echo SENTINEL` and `await match SENTINEL`.
- Each parked wait holds a slot every tab shares: park one, and no sleep loops.
- Exit 75: the instance went away mid-exchange (a crash, a quit, or a self-update a `turn`
  or `post` cannot cross). Either may already have taken effect: find the session with
  `ls`, read the screen before re-sending a `turn`, and re-post only under the same `key=`.

## Lease, close

- `lease acquire ttl=<ms> holder=<you>` blocks other drivers' `turn`s; `lease acquire …
  hard` holds every input path. The harness holds a tab this way across a live agent
  upgrade: `ERR busy lease=…` there clears on its own.
- `close` closes one pane, and a running job refuses it.

## `aterm drive`

`aterm drive --help` is the reference; its SUPERVISING A WORKER section covers the loop.

What a driver meets:

- A `watch` you start on a session the window's host supervises
  (`supervisor=aterm-harness@<pid>`, every Claude Code, Codex and agy session in an
  aterm window) only watches; hold a `lease` to drive the session yourself.
- A rating survey (`● How is Claude doing this session?` over `1: Bad 2: Fine 3: Good
  0: Dismiss`) takes a first character of 1, 2 or 3 as the human's rating. `phase` ends
  `survey 0` while one is open: dismiss it first with `aterm ctl "@$SID" key
  'if=^●.How.is.Claude.doing' 0`. The drafted-feedback card, the transcript-share
  question and the web-setup offer are not surveys, and their `0` changes settings.
- `phase` ending `context <n>%` means Claude Code compacts soon, and a compaction can drop
  your standing rules: re-send them after `EVENT compacted`. `/compact` or `/clear` typed
  into a worker rewrites its whole context, its human's words included.
