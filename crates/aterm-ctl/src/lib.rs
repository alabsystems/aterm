// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-ctl`: a small, dependency-free client for the aterm introspection
//! control socket (protocol v1).
//!
//! The protocol is newline-delimited text: one request line (`"VERB
//! [args...]\n"`) and one response. This client frames a request from the
//! command line, prints the relevant part of the response, and maps `OK`/`ERR`
//! onto the exit status. It uses only `std`, `aterm-uds` (the portable socket
//! stream; on Windows AF_UNIX over winsock, where the `aterm.sock` alias is a
//! pointer file resolved before connecting) and `aterm-types` (socket naming
//! and discovery shared with the server).
//!
//! # Usage
//!
//! ```text
//! aterm-ctl [--sock PATH | --pid PID] [--timeout SECS] <verb> [args...]
//! ```
//!
//! The socket is `--sock PATH`, else `--pid PID` (`<dir>/aterm-<PID>.sock`),
//! else, inside an aterm session, the instance hosting the calling terminal
//! (found through `$ATERM_PARENT_SESSION_ID`'s graph entry `<dir>/graph/<sid>`),
//! else the `latest` alias `<dir>/aterm.sock`, falling back to the newest
//! `<dir>/aterm-<pid>.sock` that accepts when the alias is dead. `<dir>` is
//! `$XDG_RUNTIME_DIR/aterm` when set, else `~/Library/Application
//! Support/aterm` on macOS.
//!
//! The server accepts only same-uid peers presenting a per-launch capability
//! token. The client reads it from the file beside the socket
//! (`aterm-<pid>.token` for a per-instance socket, else `<name>.token`) and
//! sends `AUTH <hex>\n` first on every connection; an unreadable token is
//! refused by the server with `ERR auth`.
//!
//! The verb reference is generated from `aterm_types::control_verbs` (`aterm
//! ctl --help`). The discovery verbs `instances`, `ls`, `windows` and `mux` are
//! answered by this client; `--sock`/`--pid` scope them to the addressed
//! instance, so a caller with a private instance sees only that one.
//!
//! # `image` and `window` are disjoint
//!
//! * `image [path]` renders aterm's own client frame, including
//!   compiled native-app surfaces: the current application-render artifact, a
//!   re-render through the renderer and composition rules the application present
//!   uses. It has no native OS chrome, works headless and never needs OS
//!   screen-capture permission.
//! * `window [<target>] [path]` captures a full-window artifact with native
//!   platform chrome: macOS photographs the composited window through
//!   CoreGraphics; Windows stitches chrome around a `PrintWindow` client capture
//!   from the exact client destination from a successful application present.
//!   Either way it does not observe compositor selection, colour management,
//!   scanout or occlusion. It photographs aterm's OWN window, so it
//!   needs no Screen Recording permission on macOS — do not grant one for it (a
//!   held grant re-prompts on a cooldown since macOS 15).
//!
//! # Response framing
//!
//! Output follows the verb's framing in
//! [`aterm_types::control_verbs::framing_of`], the table the server answers
//! from:
//!
//! * Lines: `"OK <n>\n"` then `<n>` data lines, which are printed; the header
//!   goes to stderr only as a diagnostic.
//! * Bytes (bare `cast`, bare `temporal`, `inbox get`, `outbox`): `"OK
//!   <nbytes>\n"` then a body copied to stdout verbatim.
//! * Push (`subscribe`): the `OK subscribe <n>` ack goes to stderr and the
//!   frames are relayed to stdout until the watch ends.
//! * Status: the single `OK …` line is printed.
//!
//! An `ERR …` response or a connection failure prints to stderr and exits 1. A
//! server-reported timeout (`OK timeout`, or a `turn` verdict with
//! `status=timeout`) exits 124.

use std::collections::HashSet;
use std::env;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use aterm_types::control_socket;
use aterm_uds::CtlStream;

pub mod census;
mod conn;
pub use conn::conn_main_entry;
mod drift;
mod graph_hygiene;
pub use drift::{CastDriftFn, CastDriftJob, DriftCut, DriftPeer, LocalVerbs};
mod redial;
pub use redial::EXIT_REPLACED;

/// Usage synopsis — the one-line invocation shape, shared by `--help` and the
/// "no verb given" usage error so they never drift apart.
const SYNOPSIS: &str = "aterm-ctl [--sock PATH | --pid PID] [--timeout SECS] <verb> [args...]";

/// The hand-written prose of `--help`: synopsis, options, exit codes, stdin
/// payload forms, socket resolution and the cross-terminal note. Ends with the
/// `VERBS:` header; [`help_text`] appends the catalog generated from
/// [`aterm_types::control_verbs::catalog_lines`].
const HELP_PROSE: &str = "\
aterm-ctl — a tiny client for the aterm introspection control socket (v1).

USAGE:
    aterm-ctl [--sock PATH | --pid PID] [--timeout SECS] <verb> [args...]

OPTIONS:
    --sock PATH   connect to the control socket at PATH.
    --pid PID     connect to a specific instance's <dir>/aterm-<PID>.sock.
    --timeout SECS
                  per-op socket read/write deadline, in SECONDS (default 900;
                  0 disables the deadline). Also accepts --timeout=SECS. Raise
                  it for a long synchronous verb; a fired deadline exits 124.
                  On `subscribe` an EXPLICIT --timeout is instead a max-watch
                  WALL-CLOCK bound (frames so far are flushed, then exit 124);
                  the flagless default (and an explicit 0) watches forever.
                  A server that goes away does not extend it: the wait for a
                  successor (exit 75) spends what the deadline has left.
    -h, --help    print this help and exit.
    -V, --version print version information and exit.

EXIT CODES:
    0    OK — the server answered OK. For the client-answered discovery verbs
         (`ls`, `instances`, `windows`): at least one instance answered.
    1    failure — a usage/connect error, or the server replied ERR. For
         `ls`/`instances`/`windows`: the control-socket directory was readable
         and held NO instance socket — a truly empty fleet, and the ONLY case
         that prints `no live aterm instances found`.
    2    `ls`/`instances`/`windows` only — could not LOOK, or could not REACH:
         sockets were found but none answered (a sandbox refusing AF_UNIX
         connect(), stale sockets of exited pids, an unreadable or rejected
         token), or the directory is missing, unreadable, or unresolvable
         ($HOME and $XDG_RUNTIME_DIR unset). The report on stderr names every
         socket and its cause, plus one hint for the dominant cause — never
         the false claim that the fleet is empty.
    124  timeout — the client's --timeout deadline fired, OR the server
         reported a timeout (a `turn` verdict status=timeout, or an
         await/ready/wait `OK timeout`); for `ls`/`instances`/`windows`,
         EVERY found socket timed out. Additive: still nonzero, so existing
         `nonzero == failure` scripts are unaffected.
    75   the aterm instance went away mid-exchange and nothing replaced it
         within 30 s, or within what an explicit --timeout had left (an
         update that did not come back, a crash, a quit): a `subscribe`
         stream, a blocking read (`await`, `wait`, `ready`, `text`,
         `inbox`), a `turn` or a `post` could not be resumed. An aterm SELF-UPDATE is
         followed instead: `subscribe` keeps relaying from the successor (a
         stderr line says so; its new `sub` lines start a new stream, each
         target sending its current state after a `GAP <local> replaced`
         line; each kind of event a single-target stream already relayed
         goes on after the last one, and the rest start from the
         successor's newest; and a target named by local number
         `@<n>` is followed by the session id its `sub` line gave), and a
         blocking read is asked once more — except `await seq`, `await
         block`, `await consent` (each waits for a change after its arm,
         which the update may have made), `await inbox since=`, `inbox get
         <id>`, `inbox since=<id>` and any request naming its session
         `@<n>`, whose anchor names a position in the old process (a successor numbers its sessions
         afresh): those exit 75 too, and so does a `turn`, which is never
         asked again — its text may already have been typed — and a `post`,
         which may already be queued and carried to the successor under its
         id: re-post it only with the same `key=`, or after `outbox`. So
         does a `dial` whose remote closes the connection before it
         answers, or before a stream's sessions exit: dial again — but a
         dialled `turn`, `post`, `await block`, `await consent` or bare
         `await seq` may already have taken effect, or seen its change, on
         the remote, so check there first, as above. Never 0: a pipeline
         must not read an update as \"session over\".

STDIN PAYLOADS (multi-line, whitespace-preserving; each <= 256 KiB):
    feed-bin      read the raw binary payload from STDIN and feed it to the PTY
                  VERBATIM via a length-prefixed frame; print the server's
                  OK <n> bytes. (No inline length is accepted.)
    send --stdin | send -
                  send the EXACT STDIN bytes RAW — the same verbatim frame as
                  feed-bin — bypassing the argv newline guard the inline
                  `send <text>` form keeps.
    paste --stdin | paste -
                  paste the STDIN bytes with the server's paste handling
                  (control-byte sanitize, and ESC[200~/201~ bracketing when the
                  app enabled DECSET 2004) — a multi-line paste with real paste
                  semantics, NOT the raw byte feed of send/feed-bin.

SOCKET RESOLUTION:
    The socket path is resolved as: --sock PATH if given, else --pid PID
    (a specific instance's <dir>/aterm-<PID>.sock), else — from a shell an
    aterm window or --headless instance spawned — the instance hosting the
    calling terminal (located via $ATERM_PARENT_SESSION_ID's
    <dir>/graph/<sid> entry, so a flagless call always drives YOUR
    terminal's instance even when a newer instance owns the symlink), else the per-user default <dir>/aterm.sock where <dir> is
    $XDG_RUNTIME_DIR/aterm (when set) or ~/Library/Application Support/aterm
    (every Unix). The default is a symlink the server atomically points at the
    newest instance's aterm-<pid>.sock; when nothing accepts on it (that
    instance exited or crashed while an older one lives on) the flagless
    flow falls back to the newest <dir>/aterm-<pid>.sock that does, so it
    reaches a live instance. When nothing serves the socket but aterm IS
    running, a FLAGLESS call's error names each live instance with the
    --pid/--sock that reaches it; a socket PINNED by --sock or --pid is
    told only how many other instances are live (`aterm ctl instances`
    lists them) — never handed a different instance to drive. No
    environment variable selects a socket: --sock is the one spelling.

    DISCOVERY (`ls`, `instances`, `windows`): the client enumerates <dir>/aterm-<pid>.sock
    (plus the <dir>/graph entries of instances bound with --control-sock)
    and dials each one with a 2 s deadline. When $XDG_RUNTIME_DIR is set,
    ~/Library/Application Support/aterm is NOT consulted, so a missing
    $XDG_RUNTIME_DIR/aterm reports the environment, not an empty fleet. A
    stale socket (its pid exited without cleanup) is skipped silently while
    another instance answers, and named — with its cause — only in the
    failure report when nothing does (see EXIT CODES 1 vs 2).

CROSS-TERMINAL DRIVING:
    Every session of every same-user instance is addressable by its stable
    sid: prefix any verb with @<sid> (e.g. aterm-ctl \"@s-ab12…\" text). A sid
    hosted by ANOTHER instance is relayed to it transparently (owner
    connections only). Discover peers with `ls`, then send/read/await as if
    the session were local.

    A first-class SELF selector: `@self` (alias `@env`) expands client-side to
    `@$ATERM_PARENT_SESSION_ID` — YOUR OWN session, stable across the human's
    tab switches — before the request is framed. Flagless / `@.` instead target
    the currently ACTIVE tab, which retargets on every tab switch. `@self`
    errors if $ATERM_PARENT_SESSION_ID is unset (not inside an aterm session).

PUSH FRAMES (subscribe):
    After the one-shot `OK subscribe <n>` ack (to STDERR) the connection flips
    PUSH-ONLY and STDOUT carries these frames VERBATIM, each tagged by a compact
    <local> channel id (NOT the sid). A raw BYTES body may contain newlines, so
    do NOT parse the stream line-by-line once `bytes` is requested.
      sub <local> <sid>          one per target, right after the ack: maps the
                                 <local> channel id to its stable sid.
      DELTA <local> seq=<n> screen <nrows>   then <nrows> rows — a FULL screen
                                 snapshot at seq <n> (a seq SKIP is coalescing,
                                 NOT loss).
      DELTA <local> seq=<n> cursor <row> <col> <visible> <style>   (<visible>
                                 0|1 — a DECTCEM hide/show pushes even with no
                                 caret move; matches the poll `cursor` verb)
      DELTA <local> seq=<n> cells <nbytes>   then <nbytes> bytes of styled-cell
                                 JSON + a trailing newline — a lossless state delta.
      EVENT <local> <kind> ...   a lifecycle digest line — `turn <id> submitted=
                                 status= dur_ms=`, `block-complete <id> exit=<code|->`,
                                 `meta`, `title`, `bell total=<n>`, `agent <word> rev=`
                                 (the server's read of a claude/codex screen moved),
                                 `status phase= outcome=` (the classified status moved),
                                 `human` (a person's key, click or scroll), the fabric kinds
                                 (`inbox`, `inbox-seen`, `post`, `fetch`, `post-landed`,
                                 `hold`, `topic`), `resize n= id= to= run=[ dropped=]` (grid
                                 resizes, coalesced per wake), `render desync-risk …|healed …`
                                 (see `resizes`), `closing reason= by=`, then `exited`; the
                                 `sessions` stream adds
                                 `EVENT * session-created|session-exited|fabric-retire`.
                                 `subscribe --help` prints the full contract.
      BYTES <local> <len>        then <len> RAW PTY bytes + a trailing newline.
      MAIL <local> id=<n> off=<n> from=<p> kind=<k>[ re=<n>][ topic=<t>]   one per
                                 row delivered to that session's inbox (`mail`
                                 stream) — metadata only; `inbox get <id>` reads
                                 the body.
      T <local|*> <t_us>         a frame timestamp (the `timestamps`/`ts` modifier),
                                 written before the frame it stamps; at most one
                                 per channel per wake.
      GAP <local> ...            a discontinuity marker: `bytes-dropped=<n>`
                                 (queue overflow), `resync=<seq>` (engine reset —
                                 state was dropped; treat the next DELTA as a fresh
                                 snapshot / re-read with `screen`), or
                                 `events-resync=<floor>` (a `since-turn=` anchor
                                 older than the retained turn ledger; records
                                 below <floor> are gone), `events-dropped=<n>` (the
                                 session's timeline evicted <n> records this watch
                                 had not been shown), `mail-dropped=<n>` (the
                                 inbox ring evicted <n> rows this watch had not
                                 been shown), or `replaced` (written by this
                                 client, never the server: aterm was replaced
                                 and the stream resumed on its successor —
                                 output, mail and records between the two were
                                 not delivered; re-read with `screen`).

VERBS (generated from the protocol verb table — always current, cannot drift):
";

/// The full `--help` text: [`HELP_PROSE`] followed by the verb catalog
/// generated from [`aterm_types::control_verbs::catalog_lines`]. Built with
/// `push_str` (the strict Trust gate cannot lower inline `format_args!`).
fn help_text() -> String {
    let mut s = String::from(HELP_PROSE);
    for line in aterm_types::control_verbs::catalog_lines() {
        s.push_str("    ");
        s.push_str(&line);
        s.push('\n');
    }
    s.push_str(CLIENT_VERBS);
    s
}

/// The client-answered verbs, documented in a fixed block appended to `--help`
/// after the generated catalog (they are not in the protocol table). Also the
/// source `help <client verb>` answers from ([`client_help_reply`]), entry by
/// entry: a verb line at the 4-column indent plus continuation lines at the
/// 18-column gutter, nothing else.
const CLIENT_VERBS: &str = "\
\n\
CLIENT VERBS (answered by aterm-ctl itself, no server round-trip):
    ls            every session of every live instance, one per line:
                  <pid> <local> <sid> <parent|-> <state> <title> meta=<0|1> nonce=<hex32>
                  window=<id|none|-> active=<0|1|-> wfocus=<0|1|-> detail=<pct|->
                  identity=<name|-> path=<frozen|live> program=<name|-> agent=<word|->
                  agent_detail=<pct|-> agent_rev=<n> agent_since_ms=<ms> agent_gen=<e.s|->
                  agent_fp=<hex16|-> human_ms=<ms|-> role=<pct|-> attention=<pct|->
                  user_title=<pct|-> supervisor=<holder|->
                  path_evidence=<measured:<ms>|unconfirmed:<ms>|carried|->
                  copy=<managed|foreign|-> upgrade=<-|<state>/<to>/<why>/<age>>[ *]
                  (* = the calling terminal's own session; window= is the
                  hosting window — a headless instance reports its one logical
                  window 0; none = a session no window holds; - = the instance
                  could not ask its main thread; active= the session is on that
                  window's active tab; wfocus= that window is aterm's most
                  recently focused one (unchanged by a minimize or by the app
                  deactivating); detail= the sanitized running command, never
                  its arguments — e.g. claude, codex, targo%20test — so it says
                  whether a peer is another agent; identity= the agent identity the
                  session was spawned under (`spawn identity=<name>`: its
                  agents' own login, kept apart from the human's), - for the
                  human's own agent config — `identities` lists them; path=
                  whether `claude` and `codex` typed at that session's prompt
                  run aterm's managed builds: frozen = a foreign copy (a
                  native install, a brew cask) wins its PATH. Measured from a
                  program the shell itself started where one has run, else
                  the mark the shell was adopted with; the row's closing
                  path_evidence=, copy= and upgrade= columns say which, which
                  build an agent in front runs, and its live upgrade — `help
                  sessions` has them. A tab whose foreground is an agent is
                  never the place to type a shell line: it is a prompt there).
    instances     one line per live same-user instance:
                  <pid> <session-count> <sock>[ self]
                  (self = the instance hosting the calling terminal).
    windows       one line per WINDOW across the fleet, folded from `sessions`:
                  <pid> window=<id> focused=<0|1> sessions=<n> active=<sid>[,<sid>…]
                  (focused= aterm's most recently focused window — one per
                  instance, unchanged by a minimize or by the app deactivating;
                  active= the sids on that window's active tab, - when none;
                  a headless instance folds under its one logical window 0),
                  then <pid> window=none sessions=<n> for sessions no window
                  holds, and <pid> window=- sessions=<n> for an instance that
                  could not say (an older server, or its main thread did not
                  answer) — never silently folded into a window.

    None of the three takes an argument: `ls 1` is `ERR usage: ls`,
    `instances 1` is `ERR usage: instances`, `windows 1` is `ERR usage:
    windows` (exit 1) — never a listing the caller did not ask for.
    These HONOUR --sock/--pid, which SCOPE the listing to the addressed
    instance instead of the whole fleet. Use that to keep an automated
    caller's own instance isolated from the user's real terminals. A
    scoped listing whose instance does not answer reports that distinctly
    (exit 2, or 124 when every probe timed out); `--pid` naming a pid no
    live instance has is a usage error (exit 1).
    `help ls` / `help instances` / `help windows` / `help mux` print the
    entry from this block, answered here as well (the server's `help`
    knows only the protocol table, so it cannot).

    mux           whether a MULTIPLEXER (screen/tmux) sits between this shell
                  and its aterm session, and what that costs:
                  mux=<kind|none> detected_by= outer_sid= marks= self_target=
                  detected_by is shell-integration (the integration marked the
                  pane itself), session-base ($ATERM_MUX_BASE — the multiplexer
                  environment the SESSION shell recorded, compared against this
                  one) or environment ($TMUX/$STY corroborated by TERM).
                  aterm hosts ONE session for a whole multiplexer, so inside a
                  pane a flagless / @self call would drive the terminal RUNNING
                  screen or tmux — those calls are REFUSED here, naming the sid
                  and the explicit form to use. Verbs that address no session
                  (version/sessions/who/whoami/grant/flows/dial-*/help) are NOT
                  refused: they have no target to redirect. OSC 133 marks do not
                  cross the boundary either, so blocks/exit codes/cwd are ABSENT
                  (not empty) for the duration — said once per multiplexer on
                  the first call from inside one (ATERM_MUX_NOTICE=0 silences
                  that sentence and the blocks/status note; ATERM_MUX=0
                  disables the whole check).
";

/// The verbs a completion script offers as the first argument: every protocol
/// verb from `aterm_types::control_verbs::VERBS` plus the client-answered
/// verbs, space-joined in table order.
fn completion_verb_list() -> String {
    let mut s = String::new();
    for spec in aterm_types::control_verbs::VERBS {
        s.push_str(spec.name);
        s.push(' ');
    }
    // The client-only verbs (`run_discovery`, `run_mux_report`), absent from the
    // protocol table.
    s.push_str("ls instances windows mux");
    s
}

/// The global flags a completion script offers: those parsed before the verb
/// in [`real_main`]. Per-verb argument completion is out of scope.
const COMPLETION_FLAGS: &str = "--sock --pid --timeout --help --version";

/// The completion script for `shell` (`bash`/`zsh`/`fish`), or `None` for an
/// unknown shell. Each offers [`completion_verb_list`] as the first argument
/// plus [`COMPLETION_FLAGS`].
fn completion_script(shell: &str) -> Option<String> {
    let verbs = completion_verb_list();
    match shell {
        "bash" => Some(bash_completion(&verbs)),
        "zsh" => Some(zsh_completion(&verbs)),
        "fish" => Some(fish_completion(&verbs)),
        _ => None,
    }
}

/// Print the completion script for `shell` to stdout (exit SUCCESS), or fail with
/// a clear `ERR` for an unknown shell name (surfaced on stderr, nonzero exit).
/// Driven by the hidden `--completions <shell>` flag and by `install.sh`.
fn emit_completions(shell: &str) -> io::Result<ExitCode> {
    let script = completion_script(shell).ok_or_else(unknown_shell_error)?;
    let stdout = stdout_handle();
    let mut out = StdoutSink(stdout.lock());
    out.write_all(script.as_bytes())?;
    Ok(ExitCode::SUCCESS)
}

/// The error for `--completions <shell>` with an unrecognized shell name (a
/// static message; the strict Trust gate cannot lower inline `format_args!`).
fn unknown_shell_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "ERR --completions: unknown shell (expected bash, zsh, or fish)",
    )
}

/// A bash completion: a `_aterm_ctl` function offering the verb set for a plain
/// word and [`COMPLETION_FLAGS`] for a `-`-prefixed word, wired with `complete -F`.
fn bash_completion(verbs: &str) -> String {
    let mut s = String::new();
    s.push_str("# aterm-ctl bash completion (generated by `aterm-ctl --completions bash`).\n");
    s.push_str("_aterm_ctl() {\n");
    s.push_str("    local cur=\"${COMP_WORDS[COMP_CWORD]}\"\n");
    s.push_str("    local verbs=\"");
    s.push_str(verbs);
    s.push_str("\"\n");
    s.push_str("    local flags=\"");
    s.push_str(COMPLETION_FLAGS);
    s.push_str("\"\n");
    s.push_str("    if [[ \"$cur\" == -* ]]; then\n");
    s.push_str("        COMPREPLY=( $(compgen -W \"$flags\" -- \"$cur\") )\n");
    s.push_str("    else\n");
    s.push_str("        COMPREPLY=( $(compgen -W \"$verbs\" -- \"$cur\") )\n");
    s.push_str("    fi\n");
    s.push_str("}\n");
    s.push_str("complete -F _aterm_ctl aterm-ctl\n");
    s
}

/// A zsh completion: an autoloaded `#compdef` body that offers the verb set for
/// the first positional (via `compadd`) and describes [`COMPLETION_FLAGS`] with
/// `_arguments`. Installed as `_aterm-ctl` on a `$fpath` directory.
fn zsh_completion(verbs: &str) -> String {
    let mut s = String::new();
    s.push_str("#compdef aterm-ctl\n");
    s.push_str("# aterm-ctl zsh completion (generated by `aterm-ctl --completions zsh`).\n");
    s.push_str("local -a verbs\n");
    s.push_str("verbs=(");
    s.push_str(verbs);
    s.push_str(")\n");
    push_zsh_ctl_arguments(&mut s, "", "verbs");
    s
}

/// The zsh `_arguments` + verb-`compadd` body for the ctl surface, rendered by
/// both the `aterm-ctl` compdef ([`zsh_completion`]) and the `aterm ctl` arm
/// of the front-door compdef ([`front_door_completion_script`]). `indent`
/// prefixes every line; `array` names the zsh array holding the verb set.
fn push_zsh_ctl_arguments(s: &mut String, indent: &str, array: &str) {
    for line in [
        "local state\n",
        "_arguments -C \\\n",
        "    '--sock[control socket path]:path:_files' \\\n",
        "    '--pid[instance pid]:pid' \\\n",
        "    '--timeout[per-op deadline in seconds]:seconds' \\\n",
        "    '(- *)--help[print help and exit]' \\\n",
        "    '(- *)--version[print version and exit]' \\\n",
        "    '1: :->verb' \\\n",
        "    '*:: :->args'\n",
        "case $state in\n",
    ] {
        s.push_str(indent);
        s.push_str(line);
    }
    s.push_str(indent);
    s.push_str("    verb) compadd -a ");
    s.push_str(array);
    s.push_str(" ;;\n");
    s.push_str(indent);
    s.push_str("esac\n");
}

/// A fish completion: the verb set gated to the first token
/// (`__fish_use_subcommand`) plus the global flags, all via `complete -c aterm-ctl`.
fn fish_completion(verbs: &str) -> String {
    let mut s = String::new();
    s.push_str("# aterm-ctl fish completion (generated by `aterm-ctl --completions fish`).\n");
    s.push_str("complete -c aterm-ctl -f\n");
    s.push_str("complete -c aterm-ctl -n __fish_use_subcommand -a '");
    s.push_str(verbs);
    s.push_str("'\n");
    push_fish_ctl_flags(&mut s, "complete -c aterm-ctl ");
    s
}

/// The ctl flag lines for fish, shared by the `aterm-ctl` script and the front
/// door's `aterm ctl` arm ([`front_door_completion_script`]). `preamble` is the
/// per-surface `complete -c <cmd> [-n <gate>] ` prefix.
fn push_fish_ctl_flags(s: &mut String, preamble: &str) {
    for line in [
        "-l sock -r -d 'control socket path'\n",
        "-l pid -r -d 'instance pid'\n",
        "-l timeout -r -d 'per-op deadline in seconds'\n",
        "-l help -d 'print help and exit'\n",
        "-l version -d 'print version and exit'\n",
    ] {
        s.push_str(preamble);
        s.push_str(line);
    }
}

/// The front door's completion script for `shell` (`bash`/`zsh`/`fish`), or
/// `None` for an unknown shell. The installed command is `aterm` (the installer
/// keeps `aterm-ctl` off `PATH`). `verbs`/`flags` are `aterm`'s own
/// first-position surface, supplied by the `aterm` crate; the `aterm ctl <TAB>`
/// arm renders from [`completion_verb_list`] / [`COMPLETION_FLAGS`].
///
/// Contract (install.sh): the zsh script's first line is `#compdef aterm`.
#[must_use]
pub fn front_door_completion_script(
    shell: &str,
    verbs: &[&str],
    flags: &[(&str, &str)],
) -> Option<String> {
    let front_verbs = join_words(verbs);
    let ctl_verbs = completion_verb_list();
    match shell {
        "bash" => Some(front_door_bash(&front_verbs, flags, &ctl_verbs)),
        "zsh" => Some(front_door_zsh(&front_verbs, flags, &ctl_verbs)),
        "fish" => Some(front_door_fish(&front_verbs, flags, &ctl_verbs)),
        _ => None,
    }
}

/// Space-join `words` — the shape every shell's word-list slot takes.
fn join_words(words: &[&str]) -> String {
    let mut s = String::new();
    for (i, word) in words.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(word);
    }
    s
}

/// The front-door bash completion: `_aterm` offers the front-door verbs at the
/// first position, the front-door flags for a `-` word, and after a
/// first-position `ctl` the ctl verb and flag sets.
fn front_door_bash(verbs: &str, flags: &[(&str, &str)], ctl_verbs: &str) -> String {
    let mut s = String::new();
    s.push_str("# aterm bash completion (generated by `aterm --completions bash`).\n");
    s.push_str("_aterm() {\n");
    s.push_str("    local cur=\"${COMP_WORDS[COMP_CWORD]}\"\n");
    s.push_str("    local verbs=\"");
    s.push_str(verbs);
    s.push_str("\"\n");
    s.push_str("    local flags=\"");
    s.push_str(&join_flag_names(flags));
    s.push_str("\"\n");
    s.push_str("    local ctl_verbs=\"");
    s.push_str(ctl_verbs);
    s.push_str("\"\n");
    s.push_str("    local ctl_flags=\"");
    s.push_str(COMPLETION_FLAGS);
    s.push_str("\"\n");
    s.push_str("    if [[ $COMP_CWORD -ge 2 && \"${COMP_WORDS[1]}\" == \"ctl\" ]]; then\n");
    s.push_str("        if [[ \"$cur\" == -* ]]; then\n");
    s.push_str("            COMPREPLY=( $(compgen -W \"$ctl_flags\" -- \"$cur\") )\n");
    s.push_str("        else\n");
    s.push_str("            COMPREPLY=( $(compgen -W \"$ctl_verbs\" -- \"$cur\") )\n");
    s.push_str("        fi\n");
    s.push_str("        return\n");
    s.push_str("    fi\n");
    s.push_str("    if [[ \"$cur\" == -* ]]; then\n");
    s.push_str("        COMPREPLY=( $(compgen -W \"$flags\" -- \"$cur\") )\n");
    s.push_str("    elif [[ $COMP_CWORD -eq 1 ]]; then\n");
    s.push_str("        COMPREPLY=( $(compgen -W \"$verbs\" -- \"$cur\") )\n");
    s.push_str("    fi\n");
    s.push_str("}\n");
    s.push_str("complete -F _aterm aterm\n");
    s
}

/// Space-join the flag names out of `(flag, description)` pairs (bash offers
/// bare words; the descriptions are zsh/fish material).
fn join_flag_names(flags: &[(&str, &str)]) -> String {
    let mut s = String::new();
    for (i, (flag, _)) in flags.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(flag);
    }
    s
}

/// The front-door zsh completion: `#compdef aterm`, the front-door verb/flag
/// sets via `_arguments`, and a first-word `ctl` dispatch onto the ctl surface
/// ([`push_zsh_ctl_arguments`]).
fn front_door_zsh(verbs: &str, flags: &[(&str, &str)], ctl_verbs: &str) -> String {
    let mut s = String::new();
    s.push_str("#compdef aterm\n");
    s.push_str("# aterm zsh completion (generated by `aterm --completions zsh`).\n");
    s.push_str("local -a verbs ctl_verbs\n");
    s.push_str("verbs=(");
    s.push_str(verbs);
    s.push_str(")\n");
    s.push_str("ctl_verbs=(");
    s.push_str(ctl_verbs);
    s.push_str(")\n");
    s.push_str("if (( CURRENT > 2 )) && [[ ${words[2]} == ctl ]]; then\n");
    s.push_str("    # `aterm ctl …` IS the ctl client: shift the verb off and complete\n");
    s.push_str("    # with the ctl surface's own flag/verb set.\n");
    s.push_str("    shift words\n");
    s.push_str("    (( CURRENT-- ))\n");
    push_zsh_ctl_arguments(&mut s, "    ", "ctl_verbs");
    s.push_str("    return\n");
    s.push_str("fi\n");
    s.push_str("local state\n");
    s.push_str("_arguments -C \\\n");
    for (flag, desc) in flags {
        s.push_str("    '");
        s.push_str(flag);
        s.push('[');
        s.push_str(desc);
        s.push_str("]' \\\n");
    }
    s.push_str("    '1: :->verb' \\\n");
    s.push_str("    '*:: :->args'\n");
    s.push_str("case $state in\n");
    s.push_str("    verb) compadd -a verbs ;;\n");
    s.push_str("esac\n");
    s
}

/// The front-door fish completion: front-door verbs/flags gated to the first
/// token (`__fish_use_subcommand`), plus the ctl verb/flag sets gated behind a
/// seen `ctl` ([`push_fish_ctl_flags`]).
fn front_door_fish(verbs: &str, flags: &[(&str, &str)], ctl_verbs: &str) -> String {
    let mut s = String::new();
    s.push_str("# aterm fish completion (generated by `aterm --completions fish`).\n");
    s.push_str("complete -c aterm -f\n");
    s.push_str("complete -c aterm -n __fish_use_subcommand -a '");
    s.push_str(verbs);
    s.push_str("'\n");
    s.push_str("complete -c aterm -n '__fish_seen_subcommand_from ctl' -a '");
    s.push_str(ctl_verbs);
    s.push_str("'\n");
    push_fish_ctl_flags(
        &mut s,
        "complete -c aterm -n '__fish_seen_subcommand_from ctl' ",
    );
    // `aterm conn` (SESSION_CONNECTIONS.md §6.1) is implemented in this crate,
    // so its subverbs complete here, gated behind a seen `conn`.
    s.push_str("complete -c aterm -n '__fish_seen_subcommand_from conn' -a '");
    s.push_str(conn::CONN_SUBVERBS);
    s.push_str("'\n");
    for (flag, desc) in flags {
        s.push_str("complete -c aterm -n __fish_use_subcommand -l ");
        s.push_str(flag.trim_start_matches('-'));
        s.push_str(" -d '");
        s.push_str(desc);
        s.push_str("'\n");
    }
    s
}

/// The front door's `--completions <shell>`: print the `aterm` completion
/// script to stdout, or an error to stderr (exit FAILURE) for a missing or
/// unknown shell name. `verbs`/`flags` as in [`front_door_completion_script`].
pub fn front_door_completions_entry(
    shell: Option<&str>,
    verbs: &[&str],
    flags: &[(&str, &str)],
) -> ExitCode {
    match front_door_completions_result(shell, verbs, flags) {
        Ok(code) => code,
        // `aterm --completions zsh | head`: the reader has what it wanted.
        Err(e) if is_stdout_closed(&e) => ExitCode::SUCCESS,
        Err(e) => {
            // Manual `eprintln!("aterm: {e}")` (the strict Trust gate cannot lower
            // inline `format_args!`); a failed write is ignored while exiting.
            let stderr = io::stderr();
            let mut err = stderr.lock();
            let _ = err.write_all(b"aterm: ");
            let _ = err.write_all(e.to_string().as_bytes());
            let _ = err.write_all(b"\n");
            ExitCode::FAILURE
        }
    }
}

/// [`front_door_completions_entry`]'s fallible core: a missing and an unknown
/// shell name are its two errors (the entry prefixes `aterm: `).
fn front_door_completions_result(
    shell: Option<&str>,
    verbs: &[&str],
    flags: &[(&str, &str)],
) -> io::Result<ExitCode> {
    let Some(shell) = shell else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--completions requires a shell name (bash, zsh, or fish)",
        ));
    };
    let script = front_door_completion_script(shell, verbs, flags).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "--completions: unknown shell (bash, zsh, or fish)",
        )
    })?;
    let stdout = stdout_handle();
    let mut out = StdoutSink(stdout.lock());
    out.write_all(script.as_bytes())?;
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// The front door's single-instance forward
// ---------------------------------------------------------------------------
//
// `aterm new-tab` under `windowing_behavior = "attach"` needs the same
// instance resolution and liveness answer as `aterm ctl`, so they are exposed
// here rather than reimplemented. It cannot just call [`main_entry`]: it needs
// a reachability probe before routing, silence on success, and the server's
// `ERR` text as a value so it can choose between reporting and falling back.

/// The control socket of the instance a front-door request should go to, or
/// `None` when nothing is reachable.
///
/// Resolution is the flagless `aterm-ctl` rule ([`flagless_target`]), then a
/// liveness probe: a connect, since a crashed instance leaves its socket file
/// behind.
#[must_use]
pub fn front_door_instance() -> Option<String> {
    let path = resolve_path(None, None, env::var(SELF_SID_ENV).ok()).ok()?;
    front_door_probe(&path)
}

/// The liveness probe behind [`front_door_instance`]. Returns the resolved
/// path, so the caller forwards to exactly the instance that answered. On
/// Windows the flagless `aterm.sock` is a pointer file, not a socket
/// (`aterm_uds::latest`), so it must be resolved before connecting; on Unix the
/// kernel resolves the symlink.
#[must_use]
fn front_door_probe(path: &str) -> Option<String> {
    let resolved = aterm_uds::latest::resolve(path);
    CtlStream::connect(&resolved).is_ok().then_some(resolved)
}

/// Hand one request line to the instance at `path` and return its first reply
/// line, trimmed (`OK …` or `ERR …`).
///
/// `request` must end in `\n` and contain no other line terminator (the caller
/// frames it; `WindowRequest::control_request` refuses an unframeable cwd).
///
/// With a token, the request goes out only once the connect phase
/// ([`open_accepted`]: `AUTH` plus [`ACCEPT_PROBE`]) has proved the connection
/// served; with none there is nothing to probe and it goes straight out, which
/// the server answers `ERR auth` (it can never open a tab). A connection
/// parked in an undrained listen queue (a stuck accept thread, a stopped or
/// saturated process) passes [`front_door_instance`]'s connect; had the request
/// been written into it, the server would read it after the launcher gave up
/// and refuse it unrun (the caller hung up), so nothing would open anywhere.
/// A connect phase that gets no answer is a plain [`io::ErrorKind::TimedOut`],
/// never [`front_door_delivered`]: the request was not sent, and the caller
/// opens its own window. A connect-phase refusal (`ERR auth; …`, `ERR control
/// server busy; retry`) is returned as the reply.
///
/// A ten-second deadline for each phase rather than the client's 900 s
/// default: a hung forward is worse than a launch that opens its own window.
///
/// A deadline or an unreadable reply once the request is written is marked
/// [`front_door_delivered`] and worded for a person (`did not answer within
/// 10s`): the running aterm may still act on it (its main-thread hop waits
/// longer than this deadline), so the caller must not open a second window
/// for it. A hang-up with no reply is not: it is a listener dropping a
/// request it did not take.
pub fn front_door_send(path: &str, request: &str) -> io::Result<String> {
    front_door_send_within(path, request, FRONT_DOOR_DEADLINE, FRONT_DOOR_DEADLINE)
}

/// [`front_door_send`] with `accept` bounding the connect phase and the
/// request's write, and `reply` the wait for the answer to the request.
fn front_door_send_within(
    path: &str,
    request: &str,
    accept: std::time::Duration,
    reply: std::time::Duration,
) -> io::Result<String> {
    // Pinned: `path` is the instance the probe just answered; on failure the
    // launcher opens its own window, since `aterm new-tab` takes no
    // `--pid`/`--sock` remedy.
    let stream = match open_accepted(path, TargetOrigin::Pinned, Some(accept))? {
        Acceptance::Served(stream) => stream,
        Acceptance::Refused(line) => return Ok(line),
        Acceptance::NotAccepted(waited) => {
            let mut msg = String::from("took no connection within ");
            msg.push_str(&deadline_words(waited));
            msg.push_str("; the request was not sent");
            return Err(io::Error::new(io::ErrorKind::TimedOut, msg));
        }
        Acceptance::Dropped => return Err(io::ErrorKind::UnexpectedEof.into()),
    };
    stream.set_read_timeout(Some(reply))?;
    allow_foreground_handoff(path);
    // The token went with the probe. A server that hung up mid-send may have
    // left its reason (the busy reply).
    if let Err(e) = (&stream)
        .write_all(request.as_bytes())
        .and_then(|()| (&stream).flush())
    {
        if !is_peer_drop(&e) {
            return Err(e);
        }
        return pending_status_line(&stream).ok_or(e);
    }
    let mut reader = BufReader::new(&stream);
    let mut line = String::new();
    match read_status_line(&mut reader, &mut line) {
        // A close with no reply is a listener dropping a request it did not
        // take (the server answers every failure it takes with `ERR`).
        Ok(0) => Err(io::ErrorKind::UnexpectedEof.into()),
        Ok(_) => Ok(line.trim_end_matches(['\r', '\n']).to_string()),
        Err(e) if is_peer_drop(&e) => Err(e),
        Err(e) if is_timeout_error(&e) => Err(delivered(
            e.kind(),
            format!("did not answer within {}", deadline_words(reply)),
        )),
        Err(e) => Err(delivered(
            e.kind(),
            "sent a reply this aterm cannot read".to_string(),
        )),
    }
}

/// How long [`front_door_send`] waits for the running aterm, in each phase.
const FRONT_DOOR_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// A [`front_door_send`] failure after the request was written.
#[derive(Debug)]
struct Delivered(String);

impl std::fmt::Display for Delivered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Delivered {}

fn delivered(kind: io::ErrorKind, words: String) -> io::Error {
    io::Error::new(kind, Delivered(words))
}

/// Whether a [`front_door_send`] error came after the request reached the
/// running aterm, which may then still open what was asked.
#[must_use]
pub fn front_door_delivered(e: &io::Error) -> bool {
    e.get_ref().is_some_and(|inner| inner.is::<Delivered>())
}

/// Windows: hand our right to set the foreground window to the instance we
/// forward to, so its `SetForegroundWindow` raises the window instead of
/// flashing the taskbar button.
///
/// The OS refuses `SetForegroundWindow` from a background process;
/// `AllowSetForegroundWindow(pid)` is the documented handoff, and the launching
/// `aterm.exe` has the right (started by the foreground shell or Explorer).
/// Best-effort: no pid, a stale alias or a refusal leaves the receiver to flash
/// the taskbar. Called after the connect and before the request line, because
/// the grant lasts only until the next input event.
#[cfg(windows)]
fn allow_foreground_handoff(path: &str) {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn AllowSetForegroundWindow(pid: u32) -> i32;
    }
    let Some(pid) = instance_pid_of(path) else {
        return;
    };
    // SAFETY: a documented one-argument user32 call taking a plain pid, with no
    // pointers or preconditions; a `0` (refusal) return is ignored.
    let _ = unsafe { AllowSetForegroundWindow(pid) };
}

#[cfg(not(windows))]
fn allow_foreground_handoff(_path: &str) {}

/// The pid of the instance a front-door socket path belongs to, from the
/// per-instance filename `aterm-<pid>.sock`, resolving the `latest` pointer
/// file first (`aterm_uds::latest`). `None` for an explicit `--control-sock`
/// path or a dangling/foreign alias.
#[cfg(windows)]
fn instance_pid_of(path: &str) -> Option<u32> {
    let resolved = aterm_uds::latest::resolve(path);
    let name = std::path::Path::new(&resolved).file_name()?.to_str()?;
    control_socket::instance_pid(name)
}

/// Socket filename inside the per-user directory — the server-maintained
/// `latest` symlink to the newest instance's socket.
const SOCK_FILE: &str = control_socket::LATEST_SOCK_FILE;

/// The per-user directory holding the control socket and token, from
/// [`aterm_uds::control_socket_dir`] (shared with the server). Windows:
/// `%TMP%\aterm`; Unix: `$XDG_RUNTIME_DIR/aterm`, else `~/Library/Application
/// Support/aterm`.
fn socket_dir() -> Option<PathBuf> {
    aterm_uds::control_socket_dir()
}

/// Environment variable naming the aterm session hosting this process's
/// terminal (injected into every session's shell).
const SELF_SID_ENV: &str = "ATERM_PARENT_SESSION_ID";

/// In-session self-location: the socket of the instance hosting the calling
/// terminal, from `sid`'s graph entry (`<dir>/graph/<sid>`, parsed with
/// [`control_socket::graph_entry_sock`]). The `latest` alias names the newest
/// instance, which need not be ours.
///
/// `<dir>` is the default rendezvous dir ([`socket_dir`]). The server publishes
/// every session's graph entry there too, with the socket's absolute path, so
/// an instance on an explicit `--control-sock` elsewhere is still found.
///
/// `None` (fall back to the alias) outside a session, when the entry is
/// missing, or when nothing is listening on the recorded socket.
fn self_instance_sock(self_sid: Option<&str>) -> Option<String> {
    self_instance_sock_in(&socket_dir()?, self_sid)
}

/// [`self_instance_sock`] in the rendezvous dir `dir` — also what a redial
/// follows ([`redial::Follow::Flagless`]), against the dir the call resolved.
fn self_instance_sock_in(dir: &Path, self_sid: Option<&str>) -> Option<String> {
    let sid = self_sid?;
    // The sid shape is server-generated (`s-<hex>`); refuse anything else so a
    // weird env value can never path-traverse out of the graph dir.
    let hex = sid.strip_prefix("s-")?;
    if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let body = std::fs::read_to_string(dir.join("graph").join(sid)).ok()?;
    let sock = control_socket::graph_entry_sock(&body)?;
    // Liveness probe, not existence: only a socket something accepts on counts.
    if CtlStream::connect(&sock).is_ok() {
        Some(sock)
    } else {
        None
    }
}

/// The flagless resolution as a library entry, for other clients in the
/// binary (aterm-link's) that must find aterm the way `aterm ctl` does.
///
/// The order is [`resolve_path`]'s: the instance hosting `self_sid` (its graph
/// entry, probed for a listener, [`self_instance_sock`]), then the `latest`
/// alias `<dir>/aterm.sock`, and when that is dead the newest instance socket
/// in the directory that accepts ([`flagless_target`]). Nothing is cached, so a
/// caller that resolves each run follows a self-update's new socket.
///
/// # Errors
///
/// No rendezvous directory can be resolved (`$XDG_RUNTIME_DIR` and `$HOME`
/// both unset).
pub fn resolve_sock_for(self_sid: Option<&str>) -> io::Result<String> {
    resolve_path(None, None, self_sid.map(str::to_string))
}

/// The instance token beside the socket at `sock` ([`read_token_at`]:
/// no-follow, regular file, bounded). A caller about to present it reads it
/// once connected, through [`connect_then_read_token`].
///
/// # Errors
///
/// Names the token file looked for and why it could not be read.
pub fn read_token_beside(sock: &str) -> io::Result<String> {
    read_token_at(sock)
        .map_err(|(path, e)| io::Error::new(e.kind(), format!("{}: {e}", path.display())))
}

/// A connection to the control socket at `sock`, and the token beside it read
/// only once connected, as this crate's own connect phase does
/// ([`open_accepted`]).
///
/// An instance writes its token before binding its socket, so a token read
/// after the connect names the listener that took it (or its replacement). A
/// token read before could name a listener the connect no longer reaches (an
/// update's successor binds the same path with its own token). Both use the
/// socket the `latest` alias named at connect time ([`pin_alias`]).
///
/// # Errors
///
/// The connect's own error. A connection whose token cannot be read comes back
/// with that miss beside it, so "nothing answers" differs from "no token".
pub fn connect_then_read_token(sock: &str) -> io::Result<(CtlStream, io::Result<String>)> {
    connect_then_read(sock, read_token_beside)
}

/// A connection to `sock` and the token to present on it, taken at one socket:
/// `sock` pinned ([`pin_alias`]), connected, then `read` handed that same
/// pinned path. The read is a parameter so a test can act at its moment.
fn connect_then_read<T>(sock: &str, read: impl FnOnce(&str) -> T) -> io::Result<(CtlStream, T)> {
    let pinned = pin_alias(sock);
    let stream = CtlStream::connect(&pinned)?;
    Ok((stream, read(&pinned)))
}

/// The instance socket the `latest` alias at `path` names, read once, so a
/// connection and its token belong to one instance even if the alias is
/// republished in between. (macOS also answers `EINVAL` to a connect through a
/// symlink being renamed.)
///
/// Unix: a symlink whose target is a bare `aterm-<pid>.sock`
/// ([`aterm_uds::latest::publish`]), joined to the link's directory; Windows:
/// the pointer file ([`aterm_uds::latest::resolve`]). Any other path is
/// returned as given.
fn pin_alias(path: &str) -> String {
    #[cfg(unix)]
    {
        let p = Path::new(path);
        if let (Some(target), Some(dir)) = (read_alias(p), p.parent())
            && let Some(name) = target.to_str()
            && name.ends_with(".sock")
            && control_socket::instance_pid(name).is_some()
        {
            return dir.join(name).to_string_lossy().into_owned();
        }
        path.to_string()
    }
    #[cfg(not(unix))]
    {
        aterm_uds::latest::resolve(path)
    }
}

/// `readlink` of the alias at `p`, retried while it answers `EINVAL` for a path
/// that is still a symlink (macOS does so for a symlink being renamed over).
/// Anything else (no such file, not a link) is `None` at once.
#[cfg(unix)]
fn read_alias(p: &Path) -> Option<PathBuf> {
    for _ in 0..8 {
        match std::fs::read_link(p) {
            Ok(target) => return Some(target),
            Err(e)
                if e.kind() == io::ErrorKind::InvalidInput
                    && std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()) => {}
            Err(_) => return None,
        }
    }
    None
}

// ---------------------------------------------------------------------------
// The multiplexer boundary (screen / tmux)
//
// aterm hosts one session per PTY; a multiplexer running in it draws every
// pane inside that one session. `$ATERM_PARENT_SESSION_ID` rides into each pane
// unchanged, so flagless self-location would drive the outer terminal.
//
// Inside a multiplexer, a call that names its target only implicitly
// (flagless, `@.`, or `@self`) is refused with the outer sid and the explicit
// form to use. `@<sid>`, `--pid` and `--sock` are unchanged.
// ---------------------------------------------------------------------------

/// Marker the shell integration exports when the shell it loaded into is a
/// multiplexer (`tmux`/`screen`) child of an aterm session shell.
///
/// Set from inside the pane, so it exists only where the integration is
/// sourced there (a hand-installed `source …` line, or fish's vendor `conf.d`).
/// aterm's managed bash/zsh injection reaches only the shell aterm starts, so
/// the marker is usually absent in a pane and [`MUX_BASE_ENV`] decides.
///
/// Its disable spellings (`0`/`off`/`no`/`none`/`false`) override a stale
/// inherited `$TMUX`/`$STY`.
const MUX_ENV: &str = "ATERM_MUX";

/// The multiplexer environment the aterm session shell was started under,
/// recorded by the shell integration as `"<$TMUX>|<$STY>"` and inherited by
/// every pane.
///
/// A pane's own `$TMUX`/`$STY` no longer match the recorded base; an aterm
/// window launched from a pane re-stamps the base as its own and matches. This
/// also catches a tmux with `default-terminal "xterm-256color"`, which the
/// `TERM` heuristic cannot.
const MUX_BASE_ENV: &str = "ATERM_MUX_BASE";

/// The aterm session hosting the detected multiplexer, copied from
/// `$ATERM_PARENT_SESSION_ID` by the integration beside [`MUX_ENV`]: the
/// terminal a flagless call would have driven, which the refusal names. When
/// absent, `$ATERM_PARENT_SESSION_ID` itself is the fallback.
const MUX_OUTER_SID_ENV: &str = "ATERM_MUX_OUTER_SESSION_ID";

/// tmux's per-client marker (`<socket>,<pid>,<session>`), set in every pane.
const TMUX_ENV: &str = "TMUX";

/// GNU screen's per-session marker (`<pid>.<tty>.<host>`), set in every window.
const SCREEN_ENV: &str = "STY";

/// The terminal type, corroborating `$TMUX`/`$STY`.
///
/// Both multiplexers rewrite `TERM` for their panes (screen `screen*`, tmux
/// `screen-256color` by default), while aterm forces `xterm-256color` for every
/// session shell. An aterm launched from a pane inherits `$TMUX`/`$STY`, so
/// `TERM` is what tells it from a real pane.
const TERM_ENV: &str = "TERM";

/// The terminal program, tier 3's second corroboration: tmux 3.2+ sets `tmux`
/// in every pane whatever `TERM` is, and aterm stamps `aterm` for every session
/// shell.
const TERM_PROGRAM_ENV: &str = "TERM_PROGRAM";

/// A multiplexer standing between the calling shell and the aterm session a
/// flagless call would drive.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MuxNesting {
    /// `tmux` or `screen` — the multiplexer around this shell.
    kind: &'static str,
    /// The aterm session hosting it, when the environment names one. `None`: a
    /// multiplexer with no aterm session in scope (nothing to mis-target), so
    /// only the report mentions it.
    outer_sid: Option<String>,
    /// Which tier saw it, as the `mux` report's `detected_by=` field:
    /// `shell-integration` ([`MUX_ENV`]), `session-base` ([`MUX_BASE_ENV`]),
    /// or `environment` (the `$TMUX`/`$STY`/`$TERM` heuristic).
    detected_by: &'static str,
}

/// The multiplexer a marker value or a `TERM` names, or `None` for anything
/// else. Matches the family head (`screen`, `screen-256color`,
/// `screen.xterm-256color`, `tmux`, `tmux-256color` …), never a substring.
fn mux_kind(value: &str) -> Option<&'static str> {
    match value.split(['-', '.']).next().unwrap_or("") {
        "tmux" => Some("tmux"),
        "screen" => Some("screen"),
        _ => None,
    }
}

/// The current environment's multiplexer markers as [`MUX_BASE_ENV`] spells
/// them: `"<$TMUX>|<$STY>"`, empty for an unset side. Compared for equality
/// only.
fn mux_env_signature(tmux: Option<&str>, sty: Option<&str>) -> String {
    let mut sig = String::from(tmux.unwrap_or(""));
    sig.push('|');
    sig.push_str(sty.unwrap_or(""));
    sig
}

/// The multiplexer the per-pane markers name, or `None` when neither is set.
/// `$TMUX` goes first: tmux's default `TERM` is `screen-256color`.
fn marker_kind(tmux: Option<&str>, sty: Option<&str>) -> Option<&'static str> {
    if tmux.is_some_and(|v| !v.is_empty()) {
        Some("tmux")
    } else if sty.is_some_and(|v| !v.is_empty()) {
        Some("screen")
    } else {
        None
    }
}

/// The environment values [`detect_mux_nesting`] decides from, each `None`
/// when unset, so tests drive the decision without the process environment
/// ([`mux_nesting_from_env`] reads the real one).
#[derive(Debug, Clone, Copy, Default)]
struct MuxEnv<'a> {
    /// [`MUX_ENV`], the in-pane verdict.
    mux: Option<&'a str>,
    /// [`MUX_BASE_ENV`], the session shell's own multiplexer environment.
    base: Option<&'a str>,
    /// [`MUX_OUTER_SID_ENV`].
    outer_sid: Option<&'a str>,
    /// `$TMUX`.
    tmux: Option<&'a str>,
    /// `$STY`.
    sty: Option<&'a str>,
    /// `$TERM`.
    term: Option<&'a str>,
    /// [`SELF_SID_ENV`], the session identity in scope.
    self_sid: Option<&'a str>,
    /// [`TERM_PROGRAM_ENV`].
    term_program: Option<&'a str>,
}

/// Decide whether a multiplexer sits between this shell and its aterm session,
/// from the environment values alone.
///
/// Three tiers, in order:
///
/// 1. [`MUX_ENV`]: a verdict the integration reached inside the pane; its
///    disable spellings win outright.
/// 2. [`MUX_BASE_ENV`]: the session shell's own multiplexer environment,
///    inherited by every pane; this answers in a real bash/zsh pane. A
///    signature matching the base is a definite no (e.g. an aterm window
///    launched from a pane); one that differs and names a multiplexer is a
///    definite yes, `TERM` unread.
/// 3. The `$TMUX`/`$STY` + `TERM` heuristic, for a shell with neither mark. It
///    fires only when an aterm session identity is in scope and `TERM` (or
///    `TERM_PROGRAM=tmux`, [`TERM_PROGRAM_ENV`]) names a multiplexer.
///
/// Known gap: a pre-3.2 tmux (no `TERM_PROGRAM`) with `default-terminal
/// "xterm-256color"`, in a shell with neither mark; `aterm ctl mux` reports
/// `mux=none` there.
fn detect_mux_nesting(env: &MuxEnv<'_>) -> Option<MuxNesting> {
    fn set(v: Option<&str>) -> Option<&str> {
        v.filter(|s| !s.is_empty())
    }
    let MuxEnv {
        mux,
        base,
        outer_sid,
        tmux,
        sty,
        term,
        self_sid,
        term_program,
    } = *env;
    let outer = set(outer_sid).or_else(|| set(self_sid)).map(str::to_string);
    if let Some(marker) = set(mux) {
        if mux_check_off(marker) {
            return None;
        }
        return Some(MuxNesting {
            kind: mux_kind(marker).unwrap_or("multiplexer"),
            outer_sid: outer,
            detected_by: "shell-integration",
        });
    }
    if let Some(base) = set(base) {
        // Equal to the recorded base: nothing entered since. Different and
        // marked: a pane. Different with no marker (unset by hand) names no
        // multiplexer, so fall through.
        if base == mux_env_signature(tmux, sty) {
            return None;
        }
        if let Some(kind) = marker_kind(tmux, sty) {
            return Some(MuxNesting {
                kind,
                outer_sid: outer,
                detected_by: "session-base",
            });
        }
        return None;
    }
    // Nothing to mis-target without an aterm identity in scope (a screen in a
    // plain xterm may drive aterm windows flaglessly). `as_ref()?` rather than
    // an early return: `question_mark` is denied, and `outer` is moved below.
    outer.as_ref()?;
    // Either corroboration suffices: a multiplexer-written `TERM`, or tmux
    // 3.2+'s `TERM_PROGRAM=tmux`. aterm stamps `TERM_PROGRAM=aterm` for every
    // session shell, so an aterm launched from a pane stays distinguishable.
    let corroborated = set(term)
        .and_then(mux_kind)
        .or_else(|| (set(tmux).is_some() && set(term_program) == Some("tmux")).then_some("tmux"))?;
    Some(MuxNesting {
        kind: marker_kind(tmux, sty).unwrap_or(corroborated),
        outer_sid: outer,
        detected_by: "environment",
    })
}

/// [`detect_mux_nesting`] over the real process environment.
fn mux_nesting_from_env() -> Option<MuxNesting> {
    let var = |name: &str| env::var(name).ok();
    let (mux, base, outer_sid, tmux, sty, term, self_sid, term_program) = (
        var(MUX_ENV),
        var(MUX_BASE_ENV),
        var(MUX_OUTER_SID_ENV),
        var(TMUX_ENV),
        var(SCREEN_ENV),
        var(TERM_ENV),
        var(SELF_SID_ENV),
        var(TERM_PROGRAM_ENV),
    );
    detect_mux_nesting(&MuxEnv {
        mux: mux.as_deref(),
        base: base.as_deref(),
        outer_sid: outer_sid.as_deref(),
        tmux: tmux.as_deref(),
        sty: sty.as_deref(),
        term: term.as_deref(),
        self_sid: self_sid.as_deref(),
        term_program: term_program.as_deref(),
    })
}

/// Verbs answered by this client, absent from the shared verb table
/// [`addresses_no_session`] consults. They address no session.
const MUX_EXEMPT_VERBS: [&str; 4] = ["instances", "ls", "windows", "mux"];

/// Whether `verb` addresses no session, so a multiplexer cannot redirect it.
///
/// Read from the SHARED verb table rather than a second hand-kept list:
/// `Target::Meta` is the table's own name for "self-scoped or fleet-wide; a
/// selector is meaningless here" (`version`, `sessions`, `who`, `whoami`,
/// `grant`, `flows`, `dial-*`, `help`, `verbs`, …). Refusing those inside a
/// multiplexer protected nothing — there is no session to point at the wrong
/// terminal — and the refusal's own advice made it worse: `aterm ctl @<sid>
/// sessions` is answered `ERR denied: sessions takes no @<sid>` by the server,
/// because a selector on an owner-only meta verb is exactly what it rejects.
fn addresses_no_session(verb: &str) -> bool {
    MUX_EXEMPT_VERBS.contains(&verb)
        || aterm_types::control_verbs::spec(verb)
            .is_some_and(|s| matches!(s.target, aterm_types::control_verbs::Target::Meta))
}

/// Whether the request names its target only implicitly, following
/// `$ATERM_PARENT_SESSION_ID` or the active tab.
///
/// * `@self` / `@env`: always implicit; inside a pane the claim "the session I
///   am in" is false whatever socket was chosen.
/// * no selector, or `@.`: implicit only while no `--sock`/`--pid` pins the
///   instance.
/// * a concrete `@<sid>`: never implicit.
fn targets_own_session_implicitly(parts: &[String], sock: Option<&str>, pid: Option<u32>) -> bool {
    let Some(first) = parts.first() else {
        return false;
    };
    if addresses_no_session(first) {
        return false;
    }
    // `subscribe` carries its selector second; every other verb takes an
    // optional leading `@<sel>` proxy token.
    let sel_idx = usize::from(first == "subscribe");
    let selector = parts.get(sel_idx).filter(|t| t.starts_with('@'));
    // Only the flagless case leaves the target to aterm.
    let unpinned = sock.is_none() && pid.is_none();
    match selector {
        Some(sel) if sel.split(',').any(|e| e == "@self" || e == "@env") => true,
        Some(sel) if sel.split(',').all(|e| e == "@.") => unpinned,
        Some(_) => false,
        None => unpinned,
    }
}

/// The refusal for an implicitly self-targeting call inside a multiplexer:
/// what is wrong, which session it would have driven, and the explicit forms.
///
/// Every remedy it names is accepted by the guard: `@<sid>` works for the
/// session- and app-targeted verbs that reach the guard (the `Target::Meta`
/// verbs, where a selector is denied, are never refused, see
/// [`addresses_no_session`]); `--pid`/`--sock` are deliberate pins; and
/// `instances` shows their values from inside a pane.
fn nested_self_drive_error(n: &MuxNesting) -> io::Error {
    let sid = n.outer_sid.as_deref().unwrap_or("<unknown>");
    let mut msg = String::from("ERR this shell is inside ");
    msg.push_str(n.kind);
    msg.push_str(", and aterm hosts ONE session for the WHOLE multiplexer — it has no session for a pane.\n  $ATERM_PARENT_SESSION_ID (");
    msg.push_str(sid);
    msg.push_str(") therefore names the terminal RUNNING ");
    msg.push_str(n.kind);
    msg.push_str(", not the pane you are looking at,\n  so this call was refused rather than driving the wrong session silently. Say what you mean:\n    aterm ctl @");
    msg.push_str(sid);
    msg.push_str(" <verb>     drive the outer terminal deliberately\n    aterm ctl --pid <pid> <verb>   or --sock <path>, to pin one instance\n    aterm ctl instances            lists the pid and socket of every live instance\n    aterm ctl mux                  what aterm can and cannot see from in here\n  OSC 133 marks do not cross ");
    msg.push_str(n.kind);
    msg.push_str(" either, so blocks/exit codes/cwd for ");
    msg.push_str(sid);
    msg.push_str("\n  describe the multiplexer, not your pane. Set ATERM_MUX=0 if this shell is not really inside ");
    msg.push_str(n.kind);
    msg.push('.');
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

/// `0` silences the one-time boundary notice, spelled as the shell integration
/// spells it so one value silences both halves; [`MUX_ENV`]'s disable values
/// turn the whole guard off instead.
const MUX_NOTICE_ENV: &str = "ATERM_MUX_NOTICE";

/// The one-time boundary notice, telling a person their command blocks stopped
/// existing. Word for word the shell integration's, with the [`stderr_line`]
/// `aterm-ctl: ` prefix.
fn mux_boundary_notice(kind: &str) -> String {
    let mut msg = String::from("inside ");
    msg.push_str(kind);
    msg.push_str(
        " — no command blocks, exit codes or cwd tracking in these panes (`aterm ctl mux` \
         explains; ATERM_MUX_NOTICE=0 silences)",
    );
    msg
}

/// The stamp making the notice once per multiplexer, keyed by the
/// multiplexer's own id so all its panes share it.
///
/// Path, key and sanitising match the shell integration byte for byte
/// (`${XDG_RUNTIME_DIR:-${TMPDIR:-/tmp}}/aterm/mux-notice/<kind>-<id>`, id =
/// `${TMUX:-${STY:-$TERM}}` with every character outside `[A-Za-z0-9._-]`
/// replaced by `_`), so whichever half speaks first claims it.
fn mux_notice_stamp(
    kind: &str,
    tmux: Option<&str>,
    sty: Option<&str>,
    term: Option<&str>,
    runtime_dir: Option<&str>,
    tmp_dir: Option<&str>,
) -> PathBuf {
    fn set(v: Option<&str>) -> Option<&str> {
        v.filter(|s| !s.is_empty())
    }
    let root = set(runtime_dir).or_else(|| set(tmp_dir)).unwrap_or("/tmp");
    let id = set(tmux).or_else(|| set(sty)).or_else(|| set(term));
    let mut name = String::from(kind);
    name.push('-');
    for ch in id.unwrap_or("").chars() {
        name.push(
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            },
        );
    }
    Path::new(root).join("aterm").join("mux-notice").join(name)
}

/// Claim the one-time notice for this multiplexer: `true` exactly once, for
/// whichever process creates the stamp first (`create_new` is atomic).
/// Errors are swallowed: a read-only runtime dir must not fail a verb, and
/// returning `false` skips the sentence rather than repeating it.
fn claim_mux_notice(n: &MuxNesting) -> bool {
    if env::var(MUX_NOTICE_ENV).ok().as_deref() == Some("0") {
        return false;
    }
    claim_mux_notice_at(&mux_notice_stamp(
        n.kind,
        env::var(TMUX_ENV).ok().as_deref(),
        env::var(SCREEN_ENV).ok().as_deref(),
        env::var(TERM_ENV).ok().as_deref(),
        env::var("XDG_RUNTIME_DIR").ok().as_deref(),
        env::var("TMPDIR").ok().as_deref(),
    ))
}

/// [`claim_mux_notice`] with the stamp already resolved, so a test can drive
/// the real claim against a real path.
fn claim_mux_notice_at(stamp: &Path) -> bool {
    let Some(dir) = stamp.parent() else {
        return false;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(stamp)
        .is_ok()
}

/// Print the boundary notice once, unless something better is about to.
///
/// The refusal, the `mux` report and the `blocks`/`status` note carry the fact
/// already; those callers pass `already_said = true`, which claims the stamp
/// without printing.
fn announce_mux_boundary(nesting: Option<&MuxNesting>, already_said: bool) {
    let Some(n) = nesting else {
        return;
    };
    announce_mux_boundary_with(n, already_said, claim_mux_notice);
}

/// [`announce_mux_boundary`]'s decision with the claim injected, so a test can
/// hand in a claim that writes where it can see (retargeting
/// `$XDG_RUNTIME_DIR` would race other tests).
fn announce_mux_boundary_with(
    n: &MuxNesting,
    already_said: bool,
    claim: impl FnOnce(&MuxNesting) -> bool,
) {
    // No aterm identity in scope: no block model to have lost. The shell half
    // also announces only under `$ATERM_PARENT_SESSION_ID`, and this keeps
    // stamps from being left for unrelated multiplexers.
    if n.outer_sid.is_none() {
        return;
    }
    if claim(n) && !already_said {
        let _ = stderr_line(&mux_boundary_notice(n.kind));
    }
}

/// The note for a verb whose answer the boundary shapes: `blocks` and `status`
/// read the OSC 133 block model, and marks emitted inside a multiplexer never
/// reach aterm. Returned only when the request addresses the session hosting
/// the multiplexer.
fn mux_degradation_note(n: &MuxNesting, parts: &[String]) -> Option<String> {
    let sid = n.outer_sid.as_deref()?;
    let (selector, rest) = split_selector(parts);
    if !matches!(rest.first().map(String::as_str), Some("blocks" | "status")) {
        return None;
    }
    let addressed = match selector {
        None | Some("@.") => true,
        Some(sel) => sel.strip_prefix('@') == Some(sid),
    };
    if !addressed {
        return None;
    }
    let mut msg = String::from("NOTE: this shell is inside ");
    msg.push_str(n.kind);
    msg.push_str(", so no OSC 133 mark from your pane ever reaches aterm — session ");
    msg.push_str(sid);
    msg.push_str(
        "'s\n  blocks, exit codes and cwd are the multiplexer's, and command blocks from inside ",
    );
    msg.push_str(n.kind);
    msg.push_str(
        " are ABSENT,\n  not empty. `aterm ctl mux` explains; ATERM_MUX_NOTICE=0 silences this.",
    );
    Some(msg)
}

/// Whether `value` is one of [`MUX_ENV`]'s disable spellings.
fn mux_check_off(value: &str) -> bool {
    matches!(value, "0" | "off" | "no" | "none" | "false")
}

/// What `aterm ctl mux` says when it detects no multiplexer. A user's
/// [`MUX_ENV`] disable value means the check never ran, so it says that rather
/// than "no multiplexer". Otherwise: in a session a flagless call follows this
/// instance's active tab (not necessarily this shell's); with no aterm session
/// in scope it reaches the newest instance.
fn mux_none_explanation(mux: Option<&str>, in_session: bool) -> String {
    if let Some(v) = mux.filter(|v| mux_check_off(v)) {
        return format!("multiplexer check off (ATERM_MUX={v})");
    }
    if in_session {
        "no multiplexer between this shell and its aterm session: a flagless call drives the tab \
         you are looking at; @self drives this shell"
            .to_string()
    } else {
        "not inside an aterm session: a flagless call drives the newest aterm instance; pick \
         another with --pid or --sock (`aterm ctl ls`)"
            .to_string()
    }
}

/// The `mux` client verb: what aterm can and cannot see from this shell, as one
/// machine-readable line on stdout plus the explanation on stderr.
fn run_mux_report(nesting: Option<&MuxNesting>) -> io::Result<ExitCode> {
    let Some(n) = nesting else {
        print_stdout_line("mux=none")?;
        stderr_line(&mux_none_explanation(
            env::var(MUX_ENV).ok().as_deref(),
            env::var(SELF_SID_ENV).is_ok_and(|s| !s.is_empty()),
        ))?;
        return Ok(ExitCode::SUCCESS);
    };
    let sid = n.outer_sid.as_deref().unwrap_or("-");
    let mut line = String::from("mux=");
    line.push_str(n.kind);
    line.push_str(" detected_by=");
    line.push_str(n.detected_by);
    line.push_str(" outer_sid=");
    line.push_str(sid);
    line.push_str(" marks=absent self_target=");
    line.push_str(if n.outer_sid.is_some() {
        "refused"
    } else {
        "allowed"
    });
    print_stdout_line(&line)?;
    let mut note = String::from("this shell is inside ");
    note.push_str(n.kind);
    note.push_str(". aterm hosts ONE session for the whole multiplexer, so:\n  * aterm has no session for a pane; ");
    if n.outer_sid.is_some() {
        note.push_str("flagless / @self calls are REFUSED here (they would drive ");
        note.push_str(sid);
        note.push_str(").\n  * ");
    } else {
        note.push_str("no aterm session identity is in scope, so nothing is refused.\n  * ");
    }
    note.push_str(
        "OSC 133 marks are ABSENT, not empty: a pane's shell runs no aterm prompt hooks, and \
         screen/tmux\n    do not pass the marks through. Command blocks, exit codes and cwd \
         tracking stop until you leave.\n  * \
         Verbs that address no session (version, sessions, who, whoami, grant, flows, dial-*, \
         help) are\n    answered from in here; name a target (@<sid>, --pid, --sock) to drive a \
         session.\n  * \
         Not really inside a multiplexer? ATERM_MUX=0 turns this check off.",
    );
    stderr_line(&note)?;
    Ok(ExitCode::SUCCESS)
}

/// The socket path [`resolve_target`] resolves, without how it was chosen. Used
/// by [`front_door_instance`] and by the public [`resolve_sock_for`].
fn resolve_path(
    sock: Option<String>,
    pid: Option<u32>,
    self_sid: Option<String>,
) -> io::Result<String> {
    resolve_target(sock, pid, self_sid).map(|(path, _)| path)
}

/// How the socket a client dials was chosen — which decides what a connect
/// failure on it may offer instead ([`running_elsewhere_hint`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TargetOrigin {
    /// Nothing named a target: the flagless rule chose the socket. When that
    /// instance is dead the error may name each live instance with the flag
    /// that reaches it.
    Flagless,
    /// The caller pinned the socket (`--sock` or `--pid`), or it is the
    /// instance the front door's probe just answered. A dead pin is never
    /// answered with a different instance to drive (an agent whose private
    /// socket died could be handed the human's window); like
    /// [`discovery_targets`], it points at `instances` and names no other.
    Pinned,
}

/// Resolve the socket path from the flags, and how it was chosen
/// ([`TargetOrigin`]). `--sock` names the socket; `--pid` targets
/// `<dir>/aterm-<pid>.sock`. With no flag, a caller inside an aterm session
/// (`self_sid` = `$ATERM_PARENT_SESSION_ID`) resolves to the instance hosting
/// its own terminal via the discovery graph. No environment variable selects a
/// socket.
fn resolve_target(
    sock: Option<String>,
    pid: Option<u32>,
    self_sid: Option<String>,
) -> io::Result<(String, TargetOrigin)> {
    let no_dir = || {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("cannot resolve the control-socket directory ({DIR_ENV_UNSET}); pass --sock"),
        )
    };
    if sock.is_some() && pid.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--sock and --pid are mutually exclusive",
        ));
    }
    if let Some(pid) = pid {
        let dir = socket_dir().ok_or_else(no_dir)?;
        let path = dir
            .join(control_socket::instance_sock_name(pid))
            .to_string_lossy()
            .into_owned();
        return Ok((path, TargetOrigin::Pinned));
    }
    if let Some(s) = sock {
        return Ok((s, TargetOrigin::Pinned));
    }
    // Flagless: the instance hosting this terminal, else the `latest` alias,
    // else (alias dead) the newest listening instance.
    match self_instance_sock(self_sid.as_deref()) {
        Some(own) => Ok((own, TargetOrigin::Flagless)),
        None => {
            let dir = socket_dir().ok_or_else(no_dir)?;
            let latest = dir.join(SOCK_FILE).to_string_lossy().into_owned();
            Ok((flagless_target(&dir, latest), TargetOrigin::Flagless))
        }
    }
}

/// Where a successor of the server this call reaches may answer ([`redial`]),
/// from [`resolve_target`]'s inputs: a `--pid` pin follows only its own process
/// back onto a fresh listener, `--sock` follows its path, and the flagless rule
/// follows the calling session's graph entry and the `latest` alias, never
/// [`flagless_target`]'s dead-alias fallback.
fn redial_follow(sock: Option<&str>, pid: Option<u32>, self_sid: Option<String>) -> redial::Follow {
    redial_follow_in(socket_dir(), sock, pid, self_sid)
}

/// [`redial_follow`] against the rendezvous dir `dir` (`None`: no `$HOME` or
/// `$XDG_RUNTIME_DIR`, so a flagless call has nothing to follow).
fn redial_follow_in(
    dir: Option<PathBuf>,
    sock: Option<&str>,
    pid: Option<u32>,
    self_sid: Option<String>,
) -> redial::Follow {
    if let Some(pid) = pid {
        return redial::Follow::Nothing(redial::Unfollowed::PidPin(pid));
    }
    if let Some(sock) = sock {
        return redial::Follow::Path(sock.to_string());
    }
    match dir {
        Some(dir) => redial::Follow::Flagless { dir, self_sid },
        None => redial::Follow::Nothing(redial::Unfollowed::NoRendezvousDir),
    }
}

/// The dead-`latest` fallback: the flagless target when nothing pins one.
///
/// `latest` (`<dir>/aterm.sock`) names the newest instance; nothing hands it
/// back to an older survivor (except an update-handoff rollback, whose cleanup
/// republishes the parent). When its instance goes away the alias is dead:
///
/// * a graceful exit removes the link (`connect` reports `ENOENT`);
/// * a crash leaves the socket file, so `connect` is refused
///   (`ECONNREFUSED`); the next default-dir spawn sweeps it and republishes the
///   alias to itself.
///
/// The rule: the alias when something accepts on it; else the newest
/// per-instance socket in `dir` that accepts ([`newest_live_instance_in`]);
/// else the alias, so the caller's error names the given path. Same-directory
/// instances only: an explicit `--control-sock` instance never claims the
/// alias, and a private instance must not receive a flagless `send` (it is
/// still named when the connect fails, [`running_elsewhere_hint`]).
///
/// Dead means `NotFound` or `ConnectionRefused` only; `EPERM` (a sandbox) or a
/// timeout is no evidence of a dead instance, so the alias is returned and the
/// original error surfaces. The probe is the plain connect
/// `self_instance_sock` and `front_door_probe` use, not discovery's
/// single-worker connector.
fn flagless_target(dir: &Path, latest: String) -> String {
    match CtlStream::connect(aterm_uds::latest::resolve(&latest)) {
        Ok(_) => latest,
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            newest_live_instance_in(dir).unwrap_or(latest)
        }
        Err(_) => latest,
    }
}

/// The newest per-instance socket in `dir` something accepts on, as an
/// absolute path — `None` when none does (or `dir` cannot be read).
fn newest_live_instance_in(dir: &Path) -> Option<String> {
    let entries = std::fs::read_dir(dir).ok()?;
    newest(
        instance_socks_in(dir, entries)
            .into_iter()
            .filter(|(_, sock)| CtlStream::connect(sock).is_ok())
            .map(|(pid, sock)| {
                let bound = std::fs::metadata(&sock).and_then(|m| m.modified()).ok();
                (bound, pid, sock)
            }),
    )
}

/// The selection rule behind [`newest_live_instance_in`]: newest by the socket
/// file's mtime (when the server bound it), never by pid, since pids wrap.
/// Ties and an unreadable mtime (`None`, ordered below every `Some`) fall back
/// to pid order.
fn newest(
    live: impl IntoIterator<Item = (Option<std::time::SystemTime>, u32, String)>,
) -> Option<String> {
    live.into_iter().max().map(|(_, _, sock)| sock)
}

/// The per-socket deadline discovery dials with; sockets are dialled in
/// sequence, so one stuck peer must not hang the sweep.
const PROBE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(2);

/// One monotonic budget for every socket phase of a discovery probe. Socket
/// timeouts are re-armed from the remaining budget before each read/write, so
/// partial progress cannot restart the clock.
#[derive(Clone, Copy)]
struct ProbeDeadline {
    started: std::time::Instant,
    budget: std::time::Duration,
}

impl ProbeDeadline {
    fn after(budget: std::time::Duration) -> Self {
        Self {
            started: std::time::Instant::now(),
            budget,
        }
    }

    fn remaining(self) -> io::Result<std::time::Duration> {
        self.budget
            .checked_sub(self.started.elapsed())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(probe_timeout_error)
    }
}

fn probe_timeout_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        "control-socket discovery exceeded its socket-I/O deadline",
    )
}

/// A read/write view re-arming the socket from one absolute deadline before
/// every syscall, so each read of a trickling line gets less time.
struct ProbeIo<'a> {
    stream: &'a CtlStream,
    deadline: ProbeDeadline,
}

impl Read for ProbeIo<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        armed(
            self.stream
                .set_read_timeout(Some(self.deadline.remaining()?)),
        )?;
        Read::read(&mut self.stream, buf)
    }
}

impl Write for ProbeIo<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        armed(
            self.stream
                .set_write_timeout(Some(self.deadline.remaining()?)),
        )?;
        Write::write(&mut self.stream, buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        armed(
            self.stream
                .set_write_timeout(Some(self.deadline.remaining()?)),
        )?;
        Write::flush(&mut self.stream)
    }
}

/// The outcome of arming a probe socket's timeout. macOS refuses socket options
/// (`EINVAL`) once the peer has closed, though its bytes are still buffered and
/// nothing can block; the syscall then goes ahead unarmed.
fn armed(set: io::Result<()>) -> io::Result<()> {
    match set {
        Err(e)
            if cfg!(target_vendor = "apple")
                && e.kind() == io::ErrorKind::InvalidInput
                && e.raw_os_error().is_some() =>
        {
            Ok(())
        }
        other => other,
    }
}

struct ProbeConnectJob {
    path: String,
    reply: std::sync::mpsc::SyncSender<io::Result<CtlStream>>,
}

/// `CtlStream::connect` has no portable timeout, so it runs on one lazy worker
/// and the caller waits only for its remaining budget. `busy` is claimed with a
/// non-blocking CAS, so a stuck connect strands at most this worker and later
/// probes fail as timeouts. A late result goes to a dropped channel and its
/// stream is closed.
struct ProbeConnector {
    sender: Option<std::sync::mpsc::SyncSender<ProbeConnectJob>>,
    busy: std::sync::Arc<std::sync::atomic::AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl ProbeConnector {
    fn spawn<F>(connect: F) -> io::Result<Self>
    where
        F: Fn(&str) -> io::Result<CtlStream> + Send + 'static,
    {
        // `busy` admits at most one request, and the channel encodes that same
        // bound instead of relying on an unbounded type for a one-job protocol.
        let (sender, requests) = std::sync::mpsc::sync_channel::<ProbeConnectJob>(1);
        let busy = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_busy = std::sync::Arc::clone(&busy);
        let worker = std::thread::Builder::new()
            .name("aterm-ctl-probe-connect".to_owned())
            .spawn(move || {
                while let Ok(job) = requests.recv() {
                    let result = connect(&job.path);
                    worker_busy.store(false, std::sync::atomic::Ordering::Release);
                    let _ = job.reply.send(result);
                }
            })?;
        Ok(Self {
            sender: Some(sender),
            busy,
            worker: Some(worker),
        })
    }

    fn connect(&self, path: &str, deadline: ProbeDeadline) -> io::Result<CtlStream> {
        let _ = deadline.remaining()?;
        if self
            .busy
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_err()
        {
            return Err(probe_timeout_error());
        }
        let (reply, result) = std::sync::mpsc::sync_channel(1);
        let Some(sender) = self.sender.as_ref() else {
            self.busy.store(false, std::sync::atomic::Ordering::Release);
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "control-socket connector is unavailable",
            ));
        };
        if sender
            .send(ProbeConnectJob {
                path: path.to_owned(),
                reply,
            })
            .is_err()
        {
            self.busy.store(false, std::sync::atomic::Ordering::Release);
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "control-socket connector stopped",
            ));
        }
        match result.recv_timeout(deadline.remaining()?) {
            Ok(result) => {
                let _ = deadline.remaining()?;
                result
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(probe_timeout_error()),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "control-socket connector stopped",
            )),
        }
    }
}

impl Drop for ProbeConnector {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn shared_probe_connector() -> io::Result<&'static ProbeConnector> {
    static CONNECTOR: std::sync::OnceLock<Result<ProbeConnector, String>> =
        std::sync::OnceLock::new();
    match CONNECTOR.get_or_init(|| {
        ProbeConnector::spawn(|path| CtlStream::connect(path)).map_err(|error| error.to_string())
    }) {
        Ok(connector) => Ok(connector),
        Err(error) => Err(io::Error::other(error.clone())),
    }
}

/// One socket's answer to the discovery probe, with every failure class kept
/// distinct, so [`discovery_report`] can say why a listing is empty (e.g. a
/// sandbox refusing `connect()` with `EPERM`) instead of claiming no instances
/// exist.
#[derive(Debug)]
enum Probe {
    /// `OK <n>` and its `n` body lines.
    Answered(Vec<String>),
    /// `ECONNREFUSED`: the socket file exists but nothing accepts (an instance
    /// that exited without cleanup, or one still booting). `pid_running` is the
    /// liveness of the pid the socket names (`None` when it names none).
    Refused { pid_running: Option<bool> },
    /// `EPERM`/`EACCES`/`ENOTCONN` on `connect()`: a sandbox (Codex CLI's
    /// seatbelt allows AF_UNIX connect only under writable roots) or another
    /// user's 0600 socket; not "not running".
    Denied(io::Error),
    /// The token beside the socket could not be read, so no `AUTH` line went
    /// out and the server answered `ERR auth`. Carries the file it looked for.
    NoToken(PathBuf, io::Error),
    /// A token was sent and the server still answered `ERR auth`: the file on
    /// disk is not the token the server holds (a restarted instance rewrote it).
    AuthRejected,
    /// The instance is up and said so: `ERR control server busy; retry` (every
    /// request lane at work, or its connection cap reached). Carries the line —
    /// the report quotes the server's own words, never "did not answer".
    Busy(String),
    /// NOT DIALED: a private `--control-sock` instance, outside the rendezvous
    /// directory, with no token beside its socket this client can read
    /// ([`graph_hygiene::foreign_token_miss`]). The request could only be
    /// refused — and logged as a denial by that instance — so it is never sent.
    /// Carries the token file that was looked for, and what the pid's process
    /// says about it ([`ForeignFacts`]), read once when the probe was made.
    Foreign { token: PathBuf, facts: ForeignFacts },
    /// The shared [`PROBE_DEADLINE`] fired during connect, AUTH/request write,
    /// or response read. The small regular token file is byte-bounded but, like
    /// ordinary local filesystem I/O, is not claimed interruptible by `std`.
    Timeout,
    /// Anything else, with the raw cause: a socket file that vanished between
    /// readdir and dial, a non-auth `ERR`, a malformed header, a connection
    /// closed without a reply.
    Other(io::Error),
}

/// One framed request and response against the socket at `path` without
/// printing: authenticate, send `verb`, read the `OK <n>` header and `n` body
/// lines as [`Probe::Answered`], or the classified failure. `pid` is the pid
/// the socket names (`0` = unknown), used only to label a refused connect.
/// Lines go through [`read_bounded_line`] over [`ProbeIo`], so memory is
/// bounded and [`PROBE_DEADLINE`] bounds total socket time even for a
/// trickling peer.
fn probe_lines(path: &str, verb: &str, pid: u32) -> Probe {
    probe_lines_with_deadline(path, verb, pid, PROBE_DEADLINE)
}

fn probe_read_bounded_line<R: BufRead>(
    reader: &mut R,
    line: &mut String,
    deadline: ProbeDeadline,
) -> io::Result<usize> {
    let _ = deadline.remaining()?;
    let read = read_bounded_line(reader, line)?;
    let _ = deadline.remaining()?;
    Ok(read)
}

fn probe_lines_with_deadline(
    path: &str,
    verb: &str,
    pid: u32,
    budget: std::time::Duration,
) -> Probe {
    // The connect and the token read below name one socket ([`pin_alias`]).
    let path = &pin_alias(path);
    let connect_deadline = ProbeDeadline::after(budget);
    let connector = match shared_probe_connector() {
        Ok(connector) => connector,
        Err(error) => return classify_connect_error(error, pid),
    };
    let stream = match connector.connect(path, connect_deadline) {
        Ok(stream) => stream,
        Err(e) => return classify_connect_error(e, pid),
    };
    let remaining_socket_budget = match connect_deadline.remaining() {
        Ok(remaining) => remaining,
        Err(error) => return io_probe(error),
    };
    // An unreadable token is not fatal yet: the request goes out without `AUTH`,
    // and only the server's `ERR auth` proves the miss mattered.
    let (token, token_miss) = match read_token_at(path) {
        Ok(token) => (Some(token), None),
        Err((token_path, error)) => (None, Some(Probe::NoToken(token_path, error))),
    };
    probe_exchange(
        &stream,
        token.as_deref(),
        token_miss,
        verb,
        ProbeDeadline::after(remaining_socket_budget),
    )
}

/// [`probe_lines`]' request and its classified answer, over a connected socket.
///
/// A server may hang up BEFORE the request is written — the listener answers a
/// connection it cannot admit with `ERR control server busy; retry` and drops
/// it at once, often before a client that reads its token file after the
/// connect has written a byte — and then the write fails (`EPIPE`) while the
/// server's line waits in this end's receive buffer. That line is the answer:
/// it is read and classified ([`Probe::Busy`], an `ERR auth`, …), and the
/// write's error is kept only when the server said nothing.
fn probe_exchange(
    stream: &CtlStream,
    token: Option<&str>,
    token_miss: Option<Probe>,
    verb: &str,
    deadline: ProbeDeadline,
) -> Probe {
    let mut io = ProbeIo { stream, deadline };
    let wrote = (|| {
        if let Some(token) = token {
            io.write_all(format!("AUTH {token}\n").as_bytes())?;
        }
        deadline.remaining()?;
        io.write_all(format!("{verb}\n").as_bytes())?;
        io.flush()
    })();
    let unwritten = match wrote {
        Ok(()) => None,
        Err(e) if redial::is_hangup(&e) => Some(e),
        Err(e) => return io_probe(e),
    };
    let mut reader = BufReader::new(io);
    let mut status = String::new();
    match probe_read_bounded_line(&mut reader, &mut status, deadline) {
        Err(e) => return io_probe(unwritten.unwrap_or(e)),
        Ok(0) => {
            return unwritten.map_or_else(
                || {
                    Probe::Other(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "the server closed the connection without replying",
                    ))
                },
                io_probe,
            );
        }
        Ok(_) => {}
    }
    let status = status.trim_end();
    let Some(tail) = status.strip_prefix("OK ") else {
        return if status.starts_with("ERR auth") {
            token_miss.unwrap_or(Probe::AuthRejected)
        } else if status.starts_with(aterm_types::control_verbs::CONTROL_BUSY_REPLY) {
            Probe::Busy(status.to_string())
        } else if status.starts_with("ERR") {
            Probe::Other(io::Error::other(format!("server replied: {status}")))
        } else {
            Probe::Other(malformed_header_error(status))
        };
    };
    // An `OK` cannot answer a request that never went out whole.
    if let Some(e) = unwritten {
        return io_probe(e);
    }
    let Some(count) = stream_count(tail) else {
        return Probe::Other(malformed_header_error(status));
    };
    let mut lines = Vec::with_capacity(count);
    for _ in 0..count {
        let mut line = String::new();
        match probe_read_bounded_line(&mut reader, &mut line, deadline) {
            Err(e) => return io_probe(e),
            Ok(0) => {
                return Probe::Other(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the server closed before the complete discovery response",
                ));
            }
            Ok(_) => lines.push(line.trim_end_matches(['\r', '\n']).to_string()),
        }
    }
    Probe::Answered(lines)
}

/// Map a failed `connect()` to its [`Probe`] class. `ECONNREFUSED` is a stale
/// or booting instance, labelled by whether `pid` is alive;
/// `EPERM`/`EACCES`/`ENOTCONN` are a sandbox or foreign socket mode, not "not
/// running". Anything else keeps the raw cause, framed like [`connect_error`].
fn classify_connect_error(e: io::Error, pid: u32) -> Probe {
    match e.kind() {
        io::ErrorKind::ConnectionRefused => Probe::Refused {
            pid_running: (pid != 0).then(|| aterm_uds::process::pid_alive(pid)),
        },
        io::ErrorKind::PermissionDenied | io::ErrorKind::NotConnected => Probe::Denied(e),
        _ if is_timeout_error(&e) => Probe::Timeout,
        _ => Probe::Other(io::Error::new(e.kind(), format!("connect: {e}"))),
    }
}

/// A read/write failure after the connect: the deadline firing is
/// [`Probe::Timeout`]; anything else is [`Probe::Other`] with the raw cause.
fn io_probe(e: io::Error) -> Probe {
    if is_timeout_error(&e) {
        Probe::Timeout
    } else {
        Probe::Other(e)
    }
}

/// What the rendezvous directory turned out to be: unresolvable (`$HOME`
/// unset), missing, unreadable, empty or found. Only [`DirOutcome::Empty`]
/// licenses "no live aterm instances found".
#[derive(Debug)]
enum DirOutcome {
    /// No environment variable that names the directory is set.
    Unresolvable,
    /// The resolved directory does not exist; `why` says which variable it came
    /// from (and, under `$XDG_RUNTIME_DIR`, that the default was not consulted).
    Missing { path: PathBuf, why: String },
    /// `read_dir` failed for a reason other than absence — a sandbox refusing
    /// readdir, a mode that excludes this user, a non-directory in the path,
    /// or the process out of descriptors.
    Unreadable { path: PathBuf, err: io::Error },
    /// Readable and holding no instance socket and no graph entry: a truly
    /// empty fleet.
    Empty(PathBuf),
    /// Readable, with `n` instances (readdir + graph entries) to dial.
    Found { path: PathBuf, n: usize },
    /// `--sock`/`--pid` named the target; the directory was not the question.
    /// `live_elsewhere` counts the other instances that accept a connect,
    /// dialled only when the report would otherwise say to launch aterm
    /// ([`dead_target_remedy_due`]); `0` otherwise.
    Scoped { live_elsewhere: usize },
}

/// The variables [`socket_dir`] resolves from, for the message that none is set
/// (two on Unix, three on Windows).
#[cfg(not(windows))]
const DIR_ENV_UNSET: &str = "$XDG_RUNTIME_DIR and $HOME are both unset";
#[cfg(windows)]
const DIR_ENV_UNSET: &str = "%TMP%, %TEMP% and %LOCALAPPDATA% are all unset";

/// Why a resolved but absent directory was the one looked in. Under
/// `$XDG_RUNTIME_DIR` it names the default that was not consulted, where a
/// human's instance may be running.
#[cfg(not(windows))]
fn missing_dir_reason() -> String {
    if env::var_os("XDG_RUNTIME_DIR").is_some() {
        "resolved from $XDG_RUNTIME_DIR, which is set — so \
         ~/Library/Application Support/aterm was NOT consulted"
            .to_string()
    } else {
        "resolved from $HOME; no aterm instance has published a socket there".to_string()
    }
}
#[cfg(windows)]
fn missing_dir_reason() -> String {
    "resolved from %TMP%/%TEMP% (else %LOCALAPPDATA%\\Temp); no aterm instance has \
     published a socket there"
        .to_string()
}

/// Resolve and CLASSIFY the rendezvous directory, enumerating its instances in
/// the same readdir: `(what the directory is, every instance to dial)`. The
/// list is empty unless the outcome is [`DirOutcome::Found`].
fn inspect_fleet() -> (DirOutcome, Vec<Target>) {
    let Some(dir) = socket_dir() else {
        return (DirOutcome::Unresolvable, Vec::new());
    };
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let why = missing_dir_reason();
            return (DirOutcome::Missing { path: dir, why }, Vec::new());
        }
        Err(err) => return (DirOutcome::Unreadable { path: dir, err }, Vec::new()),
    };
    let instances = enumerate_instances(&dir, entries);
    let outcome = if instances.is_empty() {
        DirOutcome::Empty(dir)
    } else {
        DirOutcome::Found {
            n: instances.len(),
            path: dir,
        }
    };
    (outcome, instances)
}

/// Every live same-user aterm instance as `(pid, sock_path)`, deduped by
/// socket path and sorted by pid. Empty whenever the directory could not be
/// read; [`inspect_fleet`] tells that apart from an empty fleet.
fn live_instances() -> Vec<(u32, String)> {
    inspect_fleet().1.into_iter().map(Target::listed).collect()
}

/// One instance discovery found: the `(pid, socket)` every caller lists, and —
/// for one a graph entry named — when that entry was last written, which is how
/// [`ForeignFacts`] tells the entry's writer from a later process handed the
/// same pid ([`graph_hygiene::is_entry_writer`]). Ordered by pid, then socket.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Target {
    pid: u32,
    sock: String,
    written: Option<std::time::SystemTime>,
}

impl Target {
    /// A target no graph entry vouches for: a readdir socket, or one
    /// `--sock`/`--pid` named.
    fn named(pid: u32, sock: String) -> Self {
        Self {
            pid,
            sock,
            written: None,
        }
    }

    /// The `(pid, socket)` pair callers outside discovery list.
    fn listed(self) -> (u32, String) {
        (self.pid, self.sock)
    }
}

/// The enumeration behind [`live_instances`], over an open readdir of `dir`:
///
/// 1. Per-instance sockets directly in the default dir (`<dir>/aterm-<pid>.sock`;
///    the alias and other names skipped).
/// 2. Explicit `--control-sock` instances, whose socket lives elsewhere: each
///    registers a graph entry `<dir>/graph/<sid>` carrying the absolute socket
///    path and hosting pid, used verbatim so `probe_lines` dials the real socket
///    and `read_token_at` finds its own token.
///
/// A graph entry whose WRITER is gone is skipped without a connect and pruned
/// ([`graph_hygiene::prune_dead_entry`]): nothing serves it, and it would
/// otherwise be re-read, and its socket dialed, on every call. Gone is a dead
/// hosting pid, or one that now names a process younger than the entry
/// ([`graph_hygiene::writer_gone`]) — a SIGKILLed instance's pid handed on to
/// a stranger, which the entry would otherwise attribute to it.
fn enumerate_instances(dir: &Path, entries: std::fs::ReadDir) -> Vec<Target> {
    let mut out: Vec<Target> = Vec::new();
    // Dedup by the CANONICAL socket path, not the raw string: a default instance
    // appears both in the readdir (raw `<dir>/aterm-<pid>.sock`) and, once it has
    // registered a session, in a graph entry whose path was canonicalized at
    // publish time (`/var` → `/private/var` on macOS). Both name the SAME file;
    // the canonical key collapses them so the instance is listed exactly once.
    let mut seen: HashSet<String> = HashSet::new();
    let remember = |seen: &mut HashSet<String>, sock: &str| -> bool {
        // Through `socket_key` after canonicalizing: on Windows canonicalize
        // refuses a socket file, and the two publishers spell one socket as
        // `C:\…` and `\\?\C:\…`.
        let key = control_socket::socket_key(
            &std::fs::canonicalize(sock)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| sock.to_string()),
        );
        seen.insert(key)
    };
    // (1) Per-instance sockets directly in the default dir.
    for (pid, sock) in instance_socks_in(dir, entries) {
        if remember(&mut seen, &sock) {
            out.push(Target::named(pid, sock));
        }
    }
    // (2) Explicit-socket instances from the default dir's graph entries. A
    //     default instance's own entry is deduped by the canonical key above.
    if let Ok(entries) = std::fs::read_dir(dir.join("graph")) {
        for e in entries.flatten() {
            let Ok((body, written)) = graph_hygiene::read_entry(&e.path()) else {
                continue;
            };
            let Some(sock) = control_socket::graph_entry_sock(&body) else {
                continue;
            };
            // An explicit socket's filename encodes no pid; the entry records it.
            // `0` is the placeholder for an entry lacking one.
            let pid = control_socket::graph_entry_pid(&body).unwrap_or(0);
            // A gone writer serves nothing: skip it WITHOUT a connect, and
            // retire the entry so the next call does not read it again — nor
            // list a stranger now holding its pid ahead of the live entry for
            // the same socket. A legacy `0` names no pid to judge, so it is
            // dialed as before.
            if pid != 0 && graph_hygiene::writer_gone(pid, written) {
                // Only a DEAD pid's entry is removed. A live pid now naming a
                // process younger than the entry is skipped and KEPT: the
                // comparison rests on the wall clock, and a clock stepped back
                // between an instance's fork and its write would make a live
                // writer look younger than its own entry — removing that would
                // hide a live instance until it rewrote its entry.
                if !aterm_uds::process::pid_alive(pid) {
                    graph_hygiene::prune_dead_entry(&e.path(), &body);
                }
                continue;
            }
            if remember(&mut seen, &sock) {
                out.push(Target { pid, sock, written });
            }
        }
    }
    out.sort_unstable();
    out
}

/// Step (1) of [`enumerate_instances`] alone, the population the dead-alias
/// fallback ([`newest_live_instance_in`]) wants: per-instance sockets directly
/// in `dir` (not the alias or the token files), as `(pid, absolute path)` in
/// readdir order. Existence only; the caller decides what "live" means.
fn instance_socks_in(dir: &Path, entries: std::fs::ReadDir) -> Vec<(u32, String)> {
    entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let pid = control_socket::instance_pid(&name)?;
            name.ends_with(".sock")
                .then(|| (pid, dir.join(&name).to_string_lossy().into_owned()))
        })
        .collect()
}

/// The one stderr line `instances` adds when the walk found foreign instances
/// ([`Probe::Foreign`]) it did not dial. `ls` and `windows` list sessions,
/// which a foreign instance cannot show, and say nothing. `None` otherwise; a
/// fleet where nothing answered names them in [`discovery_report`].
fn foreign_note(verb: &str, probes: &[(u32, String, Probe)]) -> Option<String> {
    if verb != "instances" {
        return None;
    }
    let foreign: Vec<(u32, &str, &ForeignFacts)> = probes
        .iter()
        .filter_map(|(pid, sock, p)| match p {
            Probe::Foreign { facts, .. } => Some((*pid, sock.as_str(), facts)),
            _ => None,
        })
        .collect();
    foreign_note_text(&foreign)
}

/// [`foreign_note`]'s text for instances already read: each one's pid, socket
/// and [`ForeignFacts`], and the `kill` an orphan needs, since nothing else will
/// end it — offered only for a process proven to be the instance its entry
/// names ([`ForeignFacts::Writer`]), never for whatever now holds a pid. An
/// entry whose writer is gone ([`ForeignFacts::writer_gone`]) is no instance
/// and is not counted.
fn foreign_note_text(foreign: &[(u32, &str, &ForeignFacts)]) -> Option<String> {
    let foreign: Vec<(u32, &str, &ForeignFacts)> = foreign
        .iter()
        .copied()
        .filter(|(_, _, facts)| !facts.writer_gone())
        .collect();
    if foreign.is_empty() {
        return None;
    }
    let rows: Vec<String> = foreign
        .iter()
        .map(|(pid, sock, facts)| match facts.describe() {
            Some(said) => format!("pid {pid} {sock} [{said}]"),
            None => format!("pid {pid} {sock}"),
        })
        .collect();
    let n = foreign.len();
    let (noun, it) = if n == 1 {
        ("instance", "it")
    } else {
        ("instances", "them")
    };
    let mut note = format!(
        "NOTE: {n} private {noun} only whoever started {it} can drive ({})",
        rows.join(", ")
    );
    if let Some(kill) = orphan_kill(foreign.iter().map(|(pid, _, facts)| (*pid, *facts))) {
        note.push_str(&format!("; {kill}"));
    }
    Some(note)
}

/// The `kill` offered for the foreign instances whose parent is launchd
/// ([`ForeignFacts::orphaned`]) — nothing else will end one — or `None` when
/// none is. Shared by [`foreign_note_text`] and [`dominant_hint`].
fn orphan_kill<'a>(foreign: impl Iterator<Item = (u32, &'a ForeignFacts)>) -> Option<String> {
    let orphans: Vec<String> = foreign
        .filter(|(_, facts)| facts.orphaned())
        .map(|(pid, _)| pid.to_string())
        .collect();
    if orphans.is_empty() {
        return None;
    }
    let it = if orphans.len() == 1 { "it" } else { "them" };
    Some(format!(
        "`kill {}` ends {it} once nothing in {it} is yours to keep",
        orphans.join(" ")
    ))
}

/// What a foreign instance's PROCESS says about it, since its socket says
/// nothing to this client: whether its parent is launchd (its launcher exited
/// and launchd adopted it, or launchd started it — the kernel records no
/// difference),
/// whether the binary it runs has been deleted under it (an app updated past a
/// still-running copy), and how long it has run — so an orphan left by a test
/// is named as one, not only by pid and socket. macOS reads all three;
/// elsewhere they are unknown and nothing is said.
///
/// A pid is not an identity: a SIGKILLed instance leaves its graph entry, and
/// pids wrap here in about ten minutes, so the process at that pid now may be
/// any app of this user's. Facts — and the `kill` an orphan is offered — are
/// therefore said only of [`ForeignFacts::Writer`], a process proven to be the
/// one that wrote the entry and to be aterm.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
enum ForeignFacts {
    /// Nothing proven: no birth readable, no entry time to judge it by, or the
    /// process ended while it was read. Nothing is said, nothing offered.
    #[default]
    Unknown,
    /// The pid now names a process born after the entry was last written
    /// ([`graph_hygiene::is_entry_writer`]): the instance that wrote it is gone.
    /// The walk skips such an entry before it is probed
    /// ([`graph_hygiene::writer_gone`]), so this is the pid changing hands
    /// between that walk and this read.
    Stale,
    /// The pid's process runs an executable that is not aterm (its file name).
    NotAterm(String),
    /// The entry's writer, provably, and what its process says.
    Writer {
        orphaned: bool,
        binary_deleted: bool,
        up: Option<std::time::Duration>,
    },
}

/// One pass of reads about a pid, for the pure [`ForeignFacts::judge`].
#[derive(Debug)]
struct ProcessReading {
    /// Its birth, read first.
    birth: aterm_uds::process::ProcessBirth,
    parent: Option<u32>,
    /// The executable the kernel names, with `try_exists` on it; `None` when
    /// the kernel named none.
    exe: Option<(PathBuf, io::Result<bool>)>,
    /// Its birth read again after everything else: equal to `birth` unless the
    /// process ended, or the pid changed hands, while it was being read.
    birth_after: Option<aterm_uds::process::ProcessBirth>,
}

impl ForeignFacts {
    /// Read the process at `pid` and judge it against its graph entry's last
    /// write `written` (`None` when no entry vouches for the target).
    fn read(pid: u32, written: Option<std::time::SystemTime>) -> Self {
        use aterm_uds::process::{read_process_birth, read_process_parent, read_process_path};
        let (Ok(p), Some(_)) = (i32::try_from(pid), written) else {
            return Self::Unknown;
        };
        let Some(birth) = read_process_birth(p) else {
            return Self::Unknown;
        };
        let parent = read_process_parent(p);
        let exe = read_process_path(p).map(|path| {
            let exists = path.try_exists();
            (path, exists)
        });
        let reading = ProcessReading {
            birth,
            parent,
            exe,
            birth_after: read_process_birth(p),
        };
        Self::judge(written, std::time::SystemTime::now(), reading)
    }

    /// The verdict on one [`ProcessReading`], PURE so every branch is
    /// table-tested:
    ///
    /// 1. identity — a process born after the entry was last written is
    ///    [`Self::Stale`]; with no entry time, [`Self::Unknown`];
    /// 2. the same process throughout — a birth that changed or vanished by the
    ///    last read is [`Self::Unknown`]: the facts may be another process's;
    /// 3. aterm — a named executable whose file is not `aterm` or `aterm-*` is
    ///    [`Self::NotAterm`];
    /// 4. a deleted binary is claimed only when the kernel named NO path for
    ///    the same live process (`read_process_path`'s measured contract), or
    ///    the path it named is `Ok(false)`. An `Err` — `EPERM`, a TCC refusal —
    ///    is unknown, never "deleted".
    fn judge(
        written: Option<std::time::SystemTime>,
        now: std::time::SystemTime,
        r: ProcessReading,
    ) -> Self {
        match written.and_then(|w| graph_hygiene::is_entry_writer(r.birth, w)) {
            Some(true) => {}
            Some(false) => return Self::Stale,
            None => return Self::Unknown,
        }
        if r.birth_after != Some(r.birth) {
            return Self::Unknown;
        }
        let binary_deleted = match &r.exe {
            None => true,
            Some((path, exists)) => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if name != "aterm" && !name.starts_with("aterm-") {
                    return Self::NotAterm(name);
                }
                matches!(exists, Ok(false))
            }
        };
        let up = now
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|now| now.checked_sub(std::time::Duration::from_secs(r.birth.seconds)));
        Self::Writer {
            orphaned: r.parent == Some(1),
            binary_deleted,
            up,
        }
    }

    /// Whether this is the entry's writer and launchd is its parent — the one
    /// case `instances` offers a `kill` for.
    fn orphaned(&self) -> bool {
        matches!(self, Self::Writer { orphaned: true, .. })
    }

    /// The facts worth saying, `; `-joined, or `None` when there are none.
    /// The verdict is that the pid no longer names the entry's writer.
    fn writer_gone(&self) -> bool {
        matches!(self, Self::Stale | Self::NotAterm(_))
    }

    fn describe(&self) -> Option<String> {
        let (orphaned, binary_deleted, up) = match self {
            Self::Unknown => return None,
            Self::Stale => {
                return Some(
                    "its pid now belongs to a newer process; this entry is stale".to_string(),
                );
            }
            Self::NotAterm(name) => {
                return Some(format!(
                    "its pid now runs `{name}`, not aterm — not the instance that wrote \
                     this entry"
                ));
            }
            Self::Writer {
                orphaned,
                binary_deleted,
                up,
            } => (*orphaned, *binary_deleted, *up),
        };
        let mut said = Vec::new();
        if orphaned {
            said.push(
                "its parent is launchd (its launcher exited, or launchd started it)".to_string(),
            );
        }
        if binary_deleted {
            said.push("running a deleted binary".to_string());
        }
        if let Some(up) = up {
            said.push(format!("up {}", fmt_uptime(up)));
        }
        (!said.is_empty()).then(|| said.join("; "))
    }
}

/// `7d1h`, `3h12m`, `5m`, `42s`: the two largest units.
fn fmt_uptime(up: std::time::Duration) -> String {
    let s = up.as_secs();
    let (d, h, m) = (s / 86_400, (s % 86_400) / 3_600, (s % 3_600) / 60);
    if d > 0 {
        format!("{d}d{h}h")
    } else if h > 0 {
        format!("{h}h{m}m")
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{s}s")
    }
}

/// Whether two socket path strings name the same socket, tolerating a
/// symlinked ancestor (macOS `/var` → `/private/var`): a raw match, else
/// canonical forms compared (an unresolvable path is not self).
fn same_socket_path(a: &str, b: &str) -> bool {
    if a == b || control_socket::socket_key(a) == control_socket::socket_key(b) {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => false,
    }
}

/// The instances discovery reports on, honouring `--sock`/`--pid`.
///
/// * `--pid P`: instance `P`, filtered from the normal enumeration.
/// * `--sock S`: only the instance at `S`, its pid from the filename when it
///   encodes one (`aterm-<pid>.sock`), else the `0` placeholder.
/// * neither: every live instance.
///
/// `Err((message, exit code))` when the named instance is not live, rather
/// than widening to the whole fleet.
fn discovery_targets(
    sock: Option<&str>,
    pid: Option<u32>,
) -> Result<Vec<(u32, String)>, (String, u8)> {
    if let Some(s) = sock {
        // Trust the explicit socket: it may be live without a graph entry (no
        // session registered yet). `run_discovery` skips it if it does not
        // answer.
        let name = Path::new(s)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let p = control_socket::instance_pid(&name).unwrap_or(0);
        return Ok(vec![(p, s.to_string())]);
    }
    if let Some(want) = pid {
        return pid_targets(want, inspect_fleet());
    }
    Ok(live_instances())
}

/// `--pid want` over what [`inspect_fleet`] found. A directory that could not
/// be looked in is reported as such ([`discovery_report`]'s words), never as a
/// pid that is not live; only a readable directory licenses that claim. The
/// exit code is the one unscoped discovery gives the same state.
fn pid_targets(
    want: u32,
    (outcome, instances): (DirOutcome, Vec<Target>),
) -> Result<Vec<(u32, String)>, (String, u8)> {
    if matches!(
        outcome,
        DirOutcome::Unresolvable | DirOutcome::Missing { .. } | DirOutcome::Unreadable { .. }
    ) {
        return Err(discovery_report(outcome, &[]));
    }
    // `parse_pid` refuses 0, the placeholder an entry with no `pid` line
    // enumerates as, so `want` never matches an unknown pid.
    let hit: Vec<(u32, String)> = instances
        .into_iter()
        .map(Target::listed)
        .filter(|(p, _)| *p == want)
        .collect();
    if hit.is_empty() {
        return Err((
            format!("no live aterm instance with pid {want}; `aterm ctl instances` lists them"),
            1,
        ));
    }
    Ok(hit)
}

/// `instances` / `ls` / `windows`: the client-side discovery verbs.
/// `instances` prints `<pid> <session-count> <sock>` (` self` on the instance
/// hosting the calling terminal); `ls` prints `<pid> <sessions line>` per
/// session (` *` on the caller's own). Inside a multiplexer a note on stderr
/// says the markers name the terminal running it.
fn run_discovery(
    verb: &str,
    sock: Option<&str>,
    pid: Option<u32>,
    nesting: Option<&MuxNesting>,
) -> io::Result<ExitCode> {
    let self_sid = env::var(SELF_SID_ENV).ok();
    let self_sock = self_instance_sock(self_sid.as_deref());
    let stdout = io::stdout();
    let mut out = StdoutSink(stdout.lock());
    // With `--sock`/`--pid` the directory is not the question; unscoped
    // discovery classifies it, since "no such directory" and "empty" differ.
    let (mut dir, targets) = if sock.is_some() || pid.is_some() {
        match discovery_targets(sock, pid) {
            Ok(t) => {
                let t = t.into_iter().map(|(p, s)| Target::named(p, s)).collect();
                (DirOutcome::Scoped { live_elsewhere: 0 }, t)
            }
            Err((msg, code)) => {
                eprintln!("aterm-ctl: {msg}");
                return Ok(ExitCode::from(code));
            }
        }
    } else {
        inspect_fleet()
    };
    // Every probe is kept: rows print as they arrive, and when nothing answered
    // the classified misses are the report.
    let mut probes: Vec<(u32, String, Probe)> = Vec::with_capacity(targets.len());
    // Answered rows, kept for the ambiguity sweep below (the only step that
    // waits for all of them).
    let mut answered: Vec<FleetSession> = Vec::new();
    let rendezvous = rendezvous_of(&dir);
    for (pid, sock, probe) in probe_sessions(targets, rendezvous) {
        if let Probe::Answered(sessions) = &probe {
            let is_self_instance = self_sock
                .as_deref()
                .is_some_and(|self_s| same_socket_path(self_s, &sock));
            // The same mapping `fleet_sessions` uses, so `ls`, the fleet bridge and
            // the sweep agree.
            let rows = instance_sessions(pid, sessions, self_sid.as_deref());
            if verb == "instances" {
                let marker = if is_self_instance { " self" } else { "" };
                writeln!(out, "{pid} {} {sock}{marker}", sessions.len())?;
            } else if verb == "windows" {
                for line in window_rows_from_sessions(pid, sessions) {
                    writeln!(out, "{line}")?;
                }
            } else {
                for session in &rows {
                    let marker = if session.is_self { " *" } else { "" };
                    writeln!(out, "{} {}{marker}", session.pid, session.row)?;
                }
            }
            answered.extend(rows);
        }
        probes.push((pid, sock, probe));
    }
    // A scoped run dials only its targets; before its report tells a dead
    // target to launch aterm, count the instances serving elsewhere.
    if let DirOutcome::Scoped { live_elsewhere } = &mut dir
        && dead_target_remedy_due(&probes)
    {
        let dialled: Vec<&str> = probes.iter().map(|(_, s, _)| s.as_str()).collect();
        *live_elsewhere = live_instances_besides(&dialled).len();
    }
    let foreign = foreign_note(verb, &probes);
    let (report, code) = discovery_report(dir, &probes);
    if code != 0 {
        stderr_line(&report)?;
        return Ok(ExitCode::from(code));
    }
    if let Some(note) = foreign {
        stderr_line(&note)?;
    }
    // A sid two live instances both answer to addresses neither unambiguously;
    // call it out on stderr, leaving the parsed rows untouched.
    for (sid, pids) in duplicate_sids(&answered) {
        let list = pids
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        stderr_line(&format!(
            "WARNING: session id {sid} is served by {} live instances (pids {list}) \
             — `@{sid}` is AMBIGUOUS and may reach either; address one with --pid",
            pids.len()
        ))?;
    }
    // Inside a multiplexer the ` self` / ` *` markers name the terminal running
    // screen/tmux, not the pane; say so.
    if let Some(n) = nesting.filter(|n| n.outer_sid.is_some()) {
        let mut note = String::from("NOTE: this shell is inside ");
        note.push_str(n.kind);
        note.push_str(", so the marked session hosts the WHOLE multiplexer, not your pane");
        stderr_line(&note)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// Dial each target for its `sessions` listing lazily, so a printing caller
/// emits an answered instance's rows while a later peer burns its
/// [`PROBE_DEADLINE`]. Shared by [`run_discovery`] and [`fleet_sessions`].
///
/// `rendezvous` is the directory an unscoped walk read (`None` for a
/// `--sock`/`--pid` target, always dialled); a target foreign to it
/// ([`graph_hygiene::foreign_token_miss`]) is [`Probe::Foreign`] without a
/// connect.
fn probe_sessions(
    targets: Vec<Target>,
    rendezvous: Option<PathBuf>,
) -> impl Iterator<Item = (u32, String, Probe)> {
    targets
        .into_iter()
        .map(move |Target { pid, sock, written }| {
            let foreign = rendezvous
                .as_deref()
                .and_then(|dir| graph_hygiene::foreign_token_miss(dir, &sock));
            let probe = match foreign {
                Some(token) => Probe::Foreign {
                    token,
                    facts: ForeignFacts::read(pid, written),
                },
                None => probe_lines(&sock, "sessions", pid),
            };
            (pid, sock, probe)
        })
}

/// The directory an unscoped walk read, against which [`probe_sessions`] judges
/// foreignness; `None` when nothing was found or `--sock`/`--pid` was given.
fn rendezvous_of(dir: &DirOutcome) -> Option<PathBuf> {
    match dir {
        DirOutcome::Found { path, .. } => Some(path.clone()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// `ls` as data: the in-process listing
//
// The same walk, probes, row mapping and verdict text as the printing path,
// exposed to in-process callers along with why a listing is empty.
// ---------------------------------------------------------------------------

/// One live session in the fleet, exactly as `ls` prints it.
///
/// `row` is the server's `sessions` line verbatim (`<local> <sid> <parent>
/// <state> <title…>`); the addressed columns have accessors, so the row is
/// parsed in one place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetSession {
    /// The pid of the instance hosting this session (what `--pid` addresses);
    /// `0` when the socket names no pid.
    pub pid: u32,
    /// The server's `sessions` row for this session, verbatim.
    pub row: String,
    /// Whether this is the calling terminal's own session — the row `ls` marks
    /// with ` *`.
    pub is_self: bool,
}

impl FleetSession {
    /// The session id (the row's second column), what `@<sid>` addresses.
    /// `None` for a row too short to carry one.
    #[must_use]
    pub fn sid(&self) -> Option<&str> {
        self.row.split_whitespace().nth(1)
    }
}

/// Why a fleet listing produced no sessions, as `ls` reports on stderr.
/// [`is_empty_fleet`](Self::is_empty_fleet) distinguishes "nothing there" from
/// "discovery is broken".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetListError {
    /// The operator-facing reason, byte-identical to `aterm-ctl ls`'s stderr
    /// (without the `aterm-ctl: ` prefix).
    pub reason: String,
    /// The status `ls` would exit with: [`EXIT_EMPTY_FLEET`] (1) for a readable
    /// directory with no instance, [`EXIT_UNREACHABLE`] (2) for could-not-look
    /// or could-not-reach, [`EXIT_TIMEOUT`] (124) when every instance timed out.
    pub code: u8,
}

impl FleetListError {
    /// Whether the fleet is genuinely empty (a readable rendezvous directory
    /// with no instance), as opposed to unreadable or unreachable.
    #[must_use]
    pub fn is_empty_fleet(&self) -> bool {
        self.code == EXIT_EMPTY_FLEET
    }
}

impl std::fmt::Display for FleetListError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

/// The `ls` listing as data: every session on every reachable instance, or the
/// classified reason there is none. Same scope as a bare `aterm-ctl ls`, with
/// the ` *` marker as [`FleetSession::is_self`].
///
/// # Errors
///
/// [`FleetListError`] whenever no instance answered, with the CLI's reason and
/// status. `Ok(rows)` means at least one instance answered.
pub fn fleet_sessions() -> Result<Vec<FleetSession>, FleetListError> {
    let self_sid = env::var(SELF_SID_ENV).ok();
    let (dir, targets) = inspect_fleet();
    let mut sessions = Vec::new();
    let mut probes: Vec<(u32, String, Probe)> = Vec::with_capacity(targets.len());
    for (pid, sock, probe) in probe_sessions(targets, rendezvous_of(&dir)) {
        if let Probe::Answered(rows) = &probe {
            sessions.extend(instance_sessions(pid, rows, self_sid.as_deref()));
        }
        probes.push((pid, sock, probe));
    }
    fleet_listing(sessions, discovery_report(dir, &probes))
}

/// Every aterm instance that has published a control socket on this machine,
/// as `(pid, socket path)`: the same rendezvous walk as `instances`, `ls` and
/// `windows`, without dialling. Sorted by pid; pid `0` for an explicit-socket
/// instance whose graph entry names none. A listed socket may not accept (a
/// crashed instance leaves its file); dial it and classify the answer.
///
/// # Errors
///
/// [`FleetListError`] when the directory could not be read (unresolvable,
/// missing or unreadable). A readable directory with no instance is `Ok` and
/// empty.
pub fn local_instances() -> Result<Vec<(u32, String)>, FleetListError> {
    let (dir, targets) = inspect_fleet();
    match dir {
        DirOutcome::Found { .. } | DirOutcome::Empty(_) | DirOutcome::Scoped { .. } => {
            Ok(targets.into_iter().map(Target::listed).collect())
        }
        other => {
            let (reason, code) = discovery_report(other, &[]);
            Err(FleetListError { reason, code })
        }
    }
}

/// Whether `aterm ctl --pid <pid>` reaches the instance on `sock`: `--pid`
/// dials only the default directory's per-instance socket, so an explicit
/// `--control-sock` instance needs `--sock`.
#[must_use]
pub fn pid_reaches(pid: u32, sock: &str) -> bool {
    pid != 0 && reached_by_pid(socket_dir().as_deref(), pid, sock)
}

/// The sessions one answered instance contributes to a listing; pure, shared
/// by the `ls` printer and [`fleet_sessions`].
fn instance_sessions(pid: u32, rows: &[String], self_sid: Option<&str>) -> Vec<FleetSession> {
    rows.iter()
        .map(|row| FleetSession {
            pid,
            row: row.clone(),
            // The calling terminal's own session: the row's sid (its 2nd field)
            // equals $ATERM_PARENT_SESSION_ID.
            is_self: {
                let sid = row.split_whitespace().nth(1);
                sid.is_some() && sid == self_sid
            },
        })
        .collect()
}

/// The session ids claimed by more than one live instance, each with its pids,
/// in listing order; empty for a healthy fleet. Such an id cannot address
/// either instance unambiguously. Pids are deduped first, so one instance
/// listed twice is not a second claimant.
fn duplicate_sids(sessions: &[FleetSession]) -> Vec<(String, Vec<u32>)> {
    let mut order: Vec<&str> = Vec::new();
    let mut claims: std::collections::HashMap<&str, Vec<u32>> = std::collections::HashMap::new();
    for session in sessions {
        let Some(sid) = session.sid() else {
            continue; // a row too short to carry one — the listing skips it too
        };
        let pids = claims.entry(sid).or_insert_with(|| {
            order.push(sid);
            Vec::new()
        });
        if !pids.contains(&session.pid) {
            pids.push(session.pid);
        }
    }
    order
        .into_iter()
        .filter_map(|sid| {
            let pids = claims.remove(sid)?;
            (pids.len() > 1).then(|| (sid.to_string(), pids))
        })
        .collect()
}

/// Fold a sweep into the listing result: [`discovery_report`]'s `(report,
/// code)` decides success for both the data and CLI paths. Code zero means
/// some instance answered.
fn fleet_listing(
    sessions: Vec<FleetSession>,
    verdict: (String, u8),
) -> Result<Vec<FleetSession>, FleetListError> {
    match verdict {
        (_, 0) => Ok(sessions),
        (reason, code) => Err(FleetListError { reason, code }),
    }
}

/// The `windows` rows for one instance, folded from its `sessions` lines. One
/// row per window id ascending: `<pid> window=<id> focused=<0|1> sessions=<n>
/// active=<sid>[,<sid>…]` (`active=-` when nothing is on the active tab), then
/// `<pid> window=none sessions=<n>` for sessions no window hosts, then `<pid>
/// window=- sessions=<n>` for lines that could not say (no `window=` token, or
/// `window=-`). A `--headless` instance's sessions fold under `window=0`.
///
/// Tokens are read by [`roster_tail`], from the end of the line back to the
/// `meta=` anchor, never by column: the title before it is program-settable
/// and may be empty.
fn window_rows_from_sessions(pid: u32, sessions: &[String]) -> Vec<String> {
    use std::collections::BTreeMap;
    struct Fold {
        focused: bool,
        sessions: usize,
        active: Vec<String>,
    }
    let mut windows: BTreeMap<u64, Fold> = BTreeMap::new();
    let mut detached = 0usize;
    let mut unknown = 0usize;
    for line in sessions {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let (Some(_local), Some(sid)) = (fields.first(), fields.get(1)) else {
            continue;
        };
        let tail = roster_tail(&fields);
        let token = |key: &str| tail.iter().find_map(|f| f.strip_prefix(key));
        match token("window=") {
            Some("none") => detached += 1,
            Some(id) => match id.parse::<u64>() {
                Ok(id) => {
                    let fold = windows.entry(id).or_insert(Fold {
                        focused: false,
                        sessions: 0,
                        active: Vec::new(),
                    });
                    fold.sessions += 1;
                    fold.focused |= token("wfocus=") == Some("1");
                    if token("active=") == Some("1") {
                        fold.active.push((*sid).to_string());
                    }
                }
                Err(_) => unknown += 1,
            },
            None => unknown += 1,
        }
    }
    let mut rows = Vec::with_capacity(windows.len() + 2);
    for (id, fold) in windows {
        let active = if fold.active.is_empty() {
            "-".to_string()
        } else {
            fold.active.join(",")
        };
        rows.push(format!(
            "{pid} window={id} focused={} sessions={} active={active}",
            u8::from(fold.focused),
            fold.sessions
        ));
    }
    if detached > 0 {
        rows.push(format!("{pid} window=none sessions={detached}"));
    }
    if unknown > 0 {
        rows.push(format!("{pid} window=- sessions={unknown}"));
    }
    rows
}

/// The `key=value` columns of one whitespace-split `sessions` line, walked
/// backwards from the end to the `meta=` token closing the positional columns;
/// empty when there is no anchor. Nothing before it (such as a title spelling
/// `window=7`) is taken for a column, and an empty title costs nothing.
fn roster_tail<'a>(fields: &[&'a str]) -> Vec<&'a str> {
    let mut tail = Vec::new();
    for field in fields.iter().rev() {
        if field.starts_with("meta=") {
            return tail;
        }
        if !field.contains('=') {
            break;
        }
        tail.push(*field);
    }
    Vec::new()
}

/// Exit code for "could not LOOK, or could not REACH": sockets (or a named
/// instance) were found and none answered — or none answered but to say it was
/// busy — or the directory is missing, unreadable or unresolvable. DISTINCT
/// from [`EXIT_EMPTY_FLEET`], which
/// `ls`/`instances`/`windows` reserve for a readable directory holding no
/// instance socket. Additive over the 0/1/124 contract: still nonzero for
/// `nonzero == failure` scripts.
const EXIT_UNREACHABLE: u8 = 2;

/// Exit code for a genuinely empty fleet: the rendezvous directory was read and
/// holds no instance socket and no graph entry. The only status that licenses
/// "no live aterm instances found" ([`FleetListError::is_empty_fleet`]).
const EXIT_EMPTY_FLEET: u8 = 1;

/// The `ls`/`instances`/`windows` verdict, pure: `dir` is what the directory
/// turned out to be, `probes` every socket dialled with its classified outcome;
/// the result is the stderr message (without the [`stderr_line`] prefix) and
/// the exit code. Any `Answered` probe is success: `("", 0)`.
///
/// "No live aterm instances found" comes only from a readable empty directory.
/// Every other arm names the directory or socket and the cause, and the
/// found-but-unreached arms end with one hint for the dominant cause (ties go
/// to the most actionable). The code is 124 only when every probe timed out.
fn discovery_report(dir: DirOutcome, probes: &[(u32, String, Probe)]) -> (String, u8) {
    if probes
        .iter()
        .any(|(_, _, p)| matches!(p, Probe::Answered(_)))
    {
        return (String::new(), 0);
    }
    // Every socket reached said it was busy: the heading says so, never that it
    // did not answer (the rows quote its line).
    let all_busy = !probes.is_empty() && probes.iter().all(|(_, _, p)| matches!(p, Probe::Busy(_)));
    let (mut msg, in_dir): (String, Option<&Path>) = match &dir {
        DirOutcome::Unresolvable => {
            return (
                format!("cannot resolve the control-socket directory ({DIR_ENV_UNSET})"),
                EXIT_UNREACHABLE,
            );
        }
        DirOutcome::Missing { path, why } => {
            let shown = path.display();
            return (
                format!("control-socket directory {shown} does not exist ({why})"),
                EXIT_UNREACHABLE,
            );
        }
        DirOutcome::Unreadable { path, err } => {
            let shown = path.display();
            // Unix EPERM (1) is what a seatbelt returns, as in [`probe_cause`];
            // asked only of a denial, since Windows' raw 1 is
            // `ERROR_INVALID_FUNCTION`. A cause is named only for the errors
            // that imply one; any other (`EMFILE`, `EIO`, …) is its own text alone.
            let why = if err.kind() == io::ErrorKind::PermissionDenied {
                if err.raw_os_error() == Some(1) {
                    " — a sandbox is refusing it"
                } else {
                    " — another user's directory, or a sandbox refusing it"
                }
            } else if err.kind() == io::ErrorKind::NotADirectory {
                // `ENOTDIR` names any component of the path, not only the last.
                " — that path, or one above it, is not a directory"
            } else {
                ""
            };
            return (
                format!("control-socket directory {shown} exists but cannot be read: {err}{why}"),
                EXIT_UNREACHABLE,
            );
        }
        DirOutcome::Empty(path) => {
            let shown = path.display();
            return (
                format!("no live aterm instances found (looked in {shown})"),
                EXIT_EMPTY_FLEET,
            );
        }
        DirOutcome::Found { path, n } => {
            let shown = path.display();
            let plural = if *n == 1 { "" } else { "s" };
            let heading = if all_busy {
                format!("found {n} control socket{plural} in {shown}; each answered busy:")
            } else {
                format!("found {n} control socket{plural} in {shown} but could not reach any:")
            };
            (heading, Some(path.as_path()))
        }
        DirOutcome::Scoped { .. } if all_busy => {
            ("the addressed instance answered busy:".to_string(), None)
        }
        DirOutcome::Scoped { .. } => (
            "the addressed instance could not be reached:".to_string(),
            None,
        ),
    };
    let labels: Vec<String> = probes
        .iter()
        .map(|(_, sock, _)| socket_label(sock, in_dir))
        .collect();
    let width = labels.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    for ((pid, _, probe), label) in probes.iter().zip(&labels) {
        msg.push_str("\n  ");
        msg.push_str(label);
        msg.push_str(&" ".repeat(width.saturating_sub(label.chars().count())));
        msg.push_str("  ");
        msg.push_str(&probe_cause(*pid, probe));
    }
    let scoped = match dir {
        DirOutcome::Scoped { live_elsewhere } => Some(live_elsewhere),
        _ => None,
    };
    if let Some(hint) = dominant_hint(probes, scoped) {
        msg.push_str("\n  hint: ");
        msg.push_str(&hint);
    }
    let all_timed_out =
        !probes.is_empty() && probes.iter().all(|(_, _, p)| matches!(p, Probe::Timeout));
    let code = if all_timed_out {
        EXIT_TIMEOUT
    } else {
        EXIT_UNREACHABLE
    };
    (msg, code)
}

/// How a socket is named in the report: its file name when it lives in the
/// directory the report is about, else its full path (an explicit-socket
/// instance's out-of-dir socket, or a `--sock` target).
fn socket_label(sock: &str, in_dir: Option<&Path>) -> String {
    let p = Path::new(sock);
    match (in_dir, p.file_name()) {
        (Some(dir), Some(name)) if p.parent() == Some(dir) => name.to_string_lossy().into_owned(),
        _ => sock.to_string(),
    }
}

/// The one-line cause for a socket that did not answer. `pid` is the pid the
/// socket names (`0` = none). A refused connect is labelled by liveness; a
/// denied one by errno: Unix `EPERM` (1) is what a seatbelt returns, so it
/// names the sandbox; anything else (`EACCES`, `ENOTCONN`, and every Windows
/// denial, which is never raw 1) could be another user's socket mode and says
/// so.
fn probe_cause(pid: u32, probe: &Probe) -> String {
    match probe {
        Probe::Answered(lines) => {
            let n = lines.len();
            format!("answered ({n} lines)")
        }
        Probe::Refused {
            pid_running: Some(false),
        } => format!("connect: Connection refused — stale, pid {pid} is not running"),
        Probe::Refused {
            pid_running: Some(true),
        } => format!(
            "connect: Connection refused — pid {pid} is running but nothing is serving this socket"
        ),
        Probe::Refused { pid_running: None } => {
            "connect: Connection refused — nothing is serving this socket (it names no pid)"
                .to_string()
        }
        Probe::Denied(e) => {
            if e.raw_os_error() == Some(1) {
                format!("connect: {e} — a sandbox is refusing AF_UNIX connect()")
            } else {
                format!(
                    "connect: {e} — another user's socket, or a sandbox refusing AF_UNIX connect()"
                )
            }
        }
        Probe::NoToken(path, e) => {
            let shown = path.display();
            format!("token {shown} unreadable: {e}")
        }
        Probe::AuthRejected => {
            "server rejected the token (ERR auth) — a restarted instance rewrites its token"
                .to_string()
        }
        Probe::Busy(said) => {
            let mut cause = said.clone();
            cause.push_str(" — the instance is up");
            cause
        }
        Probe::Foreign { token, facts } => {
            let shown = token.display();
            let said = facts
                .describe()
                .map_or_else(String::new, |said| format!(" [{said}]"));
            // A verdict that the pid is no longer the entry's writer is never
            // framed as an instance being there: the row names the entry.
            if facts.writer_gone() {
                format!(
                    "foreign — pid {pid}, the entry a private instance left (token {shown}); \
                     not dialed{said}"
                )
            } else {
                format!(
                    "foreign — pid {pid}, a private instance whose token ({shown}) this client \
                     cannot read; not dialed{said}"
                )
            }
        }
        Probe::Timeout => "timed out after 2s".to_string(),
        Probe::Other(e) => e.to_string(),
    }
}

/// The failure classes a hint is chosen among, in priority order: on a tie the
/// earlier wins, as its remedy is actionable (a denied connect needs an
/// allowance; a stale socket beside it needs nothing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cause {
    Denied,
    NoToken,
    AuthRejected,
    Busy,
    Timeout,
    Refused,
    Foreign,
    Other,
}

const CAUSE_PRIORITY: [Cause; 8] = [
    Cause::Denied,
    Cause::NoToken,
    Cause::AuthRejected,
    Cause::Busy,
    Cause::Timeout,
    Cause::Refused,
    Cause::Foreign,
    Cause::Other,
];

fn cause_of(probe: &Probe) -> Option<Cause> {
    match probe {
        Probe::Answered(_) => None,
        Probe::Refused { .. } => Some(Cause::Refused),
        Probe::Denied(_) => Some(Cause::Denied),
        Probe::NoToken(..) => Some(Cause::NoToken),
        Probe::AuthRejected => Some(Cause::AuthRejected),
        Probe::Busy(_) => Some(Cause::Busy),
        Probe::Foreign { .. } => Some(Cause::Foreign),
        Probe::Timeout => Some(Cause::Timeout),
        Probe::Other(_) => Some(Cause::Other),
    }
}

/// The cause most sockets share, ties broken by [`CAUSE_PRIORITY`].
fn dominant_cause(probes: &[(u32, String, Probe)]) -> Option<Cause> {
    let mut best: Option<(usize, Cause)> = None;
    for cause in CAUSE_PRIORITY {
        let n = probes
            .iter()
            .filter(|(_, _, p)| cause_of(p) == Some(cause))
            .count();
        if n > 0 && best.is_none_or(|(best_n, _)| n > best_n) {
            best = Some((n, cause));
        }
    }
    best.map(|(_, cause)| cause)
}

/// Whether some refused socket's pid is running: it is still starting up, or
/// exited without cleanup.
fn refused_but_running(probes: &[(u32, String, Probe)]) -> bool {
    probes.iter().any(|(_, _, p)| {
        matches!(
            p,
            Probe::Refused {
                pid_running: Some(true)
            }
        )
    })
}

/// Whether a scoped report would end in [`dominant_hint`]'s dead-target
/// remedy: nothing answered, refusals dominate, and no refused pid is running.
/// Only then does [`run_discovery`] dial the rest of the fleet for
/// [`DirOutcome::Scoped`]'s `live_elsewhere`.
fn dead_target_remedy_due(probes: &[(u32, String, Probe)]) -> bool {
    !probes
        .iter()
        .any(|(_, _, p)| matches!(p, Probe::Answered(_)))
        && dominant_cause(probes) == Some(Cause::Refused)
        && !refused_but_running(probes)
}

/// The one hint block for the dominant cause. A denied connect is a sandbox
/// doing its job: the hint names it and offers nothing to configure
/// (owner, 2026-09-22). `scoped` is a `--sock`/`--pid` listing, carrying how
/// many other instances accept a connect: a dead pinned socket is not where a
/// new instance listens, so with none its remedy names where the new pid is
/// listed, and with some it says aterm is running, as [`connect_error_naming`]
/// does for a pinned call.
fn dominant_hint(probes: &[(u32, String, Probe)], scoped: Option<usize>) -> Option<String> {
    let hint = match dominant_cause(probes)? {
        Cause::Denied => "the control socket is refused by this sandbox; aterm drives such a \
                          session from outside and it takes no part in messaging — nothing \
                          to configure"
            .to_string(),
        Cause::NoToken => "the token beside a socket must be readable by THIS user (it is \
                           mode 0600); another user's instance cannot be driven"
            .to_string(),
        Cause::AuthRejected => "re-run once; if it persists, `aterm ctl --sock <path> sessions` \
                                shows one socket's own answer"
            .to_string(),
        // The busy row already quotes the server's own "retry".
        Cause::Busy => return None,
        Cause::Timeout => {
            "`aterm ctl --sock <path> --timeout 30 sessions` waits longer".to_string()
        }
        Cause::Refused => {
            if refused_but_running(probes) {
                "a pid that is running but not serving is still starting up (or exited \
                 without cleanup); retry in a moment"
                    .to_string()
            } else if let Some(others @ 1..) = scoped {
                running_elsewhere_count(others)
            } else if scoped.is_some() {
                format!("{LAUNCH_REMEDY}, then `aterm ctl instances` names its pid")
            } else {
                format!(
                    "nothing is serving these sockets — leftovers of exited instances; \
                     {LAUNCH_REMEDY} and retry"
                )
            }
        }
        // The rows above are the ONLY listing that names a foreign instance:
        // `aterm doctor`'s census counts every PUBLISHED pid as reached, and a
        // foreign instance is published by definition (its graph entry is how
        // the walk found it), so doctor reports it as fine.
        Cause::Foreign => {
            let drive = "only whoever started a `foreign` instance can drive it";
            let foreign = probes.iter().filter_map(|(pid, _, p)| match p {
                Probe::Foreign { facts, .. } => Some((*pid, facts)),
                _ => None,
            });
            // Entries whose writer is gone are no instance to drive; their
            // rows already say what they are.
            if foreign.clone().all(|(_, facts)| facts.writer_gone()) {
                return None;
            }
            match orphan_kill(foreign) {
                Some(kill) => format!("{drive}; {kill}"),
                None => drive.to_string(),
            }
        }
        Cause::Other => {
            "`aterm ctl --sock <path> sessions` shows one socket's own answer".to_string()
        }
    };
    Some(hint)
}

/// Read the per-launch capability token beside the socket at `path`. A
/// per-instance socket (directly or through the `latest` symlink) pairs with
/// its `aterm-<pid>.token`; an explicit socket with its own `<name>.token`.
/// `None` if unreadable; the connection then goes without `AUTH` and the server
/// answers `ERR auth`.
fn read_token_for(path: &str) -> Option<String> {
    read_token_at(path).ok()
}

/// A launch token is 64 hex bytes plus optional whitespace; this bounds the
/// token file read even if a same-user process replaces it concurrently.
const MAX_CONTROL_TOKEN_FILE_BYTES: u64 = 256;

/// Linux's generic open(2) values, which x86, x86_64 and riscv64 keep.
#[cfg(all(
    any(target_os = "linux", target_os = "android"),
    any(target_arch = "x86", target_arch = "x86_64", target_arch = "riscv64"),
))]
const TOKEN_FILE_OPEN_FLAGS: i32 = 0x0002_0000 | 0x0000_0800; // O_NOFOLLOW | O_NONBLOCK

/// arm and arm64 Linux spell `O_NOFOLLOW` 0o100000. The generic 0x20000 is
/// `O_LARGEFILE` there, and an open with it followed a planted link.
#[cfg(all(
    any(target_os = "linux", target_os = "android"),
    any(target_arch = "aarch64", target_arch = "arm"),
))]
const TOKEN_FILE_OPEN_FLAGS: i32 = 0x0000_8000 | 0x0000_0800; // O_NOFOLLOW | O_NONBLOCK

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly",
))]
const TOKEN_FILE_OPEN_FLAGS: i32 = 0x0000_0100 | 0x0000_0004; // O_NOFOLLOW | O_NONBLOCK

/// Open only the regular file itself on the shipped Unix targets. `O_NONBLOCK`
/// makes a planted FIFO/device immediately inspectable and `O_NOFOLLOW` rejects
/// a planted symlink; descriptor metadata then rejects every non-regular kind.
///
/// A symlink's `ELOOP` is answered as a non-regular file, not as absence, so
/// [`read_token_at`] does not pass over it to the directory-shared legacy file.
#[cfg(any(
    all(
        any(target_os = "linux", target_os = "android"),
        any(
            target_arch = "x86",
            target_arch = "x86_64",
            target_arch = "riscv64",
            target_arch = "aarch64",
            target_arch = "arm",
        ),
    ),
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly",
))]
fn open_regular_token_file(path: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(TOKEN_FILE_OPEN_FLAGS)
        .open(path)
        .map_err(|e| {
            if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
                io::Error::new(io::ErrorKind::InvalidData, "token path is a symlink")
            } else {
                e
            }
        })?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "token path is not a regular file",
        ));
    }
    Ok(file)
}

/// Fallback for platforms without the audited open-flag values: a pre-open
/// kind check, then a descriptor check detecting replacement before any read.
#[cfg(not(any(
    all(
        any(target_os = "linux", target_os = "android"),
        any(
            target_arch = "x86",
            target_arch = "x86_64",
            target_arch = "riscv64",
            target_arch = "aarch64",
            target_arch = "arm",
        ),
    ),
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly",
)))]
fn open_regular_token_file(path: &Path) -> io::Result<std::fs::File> {
    if !std::fs::symlink_metadata(path)?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "token path is not a regular file",
        ));
    }
    let file = std::fs::File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "token path changed away from a regular file",
        ));
    }
    Ok(file)
}

/// Read a control capability token without following a final symlink or ever
/// allocating beyond the token-file bound.
///
/// # Errors
///
/// Returns an error for missing, non-regular, symlinked, oversized, or non-UTF-8
/// files. On the shipped Unix targets the open is also nonblocking, so a planted
/// FIFO/device is rejected before it can park the caller.
pub fn read_control_token_file(path: &Path) -> io::Result<String> {
    let file = open_regular_token_file(path)?;
    if file.metadata()?.len() > MAX_CONTROL_TOKEN_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "token file exceeds the 256-byte bound",
        ));
    }
    let mut bytes = Vec::with_capacity(MAX_CONTROL_TOKEN_FILE_BYTES as usize);
    file.take(MAX_CONTROL_TOKEN_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_CONTROL_TOKEN_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "token file exceeds the 256-byte bound",
        ));
    }
    let raw = String::from_utf8(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "token file is not UTF-8"))?;
    let token = raw.trim();
    if token.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "token file is empty",
        ));
    }
    if token.contains(['\r', '\n']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "token file contains an embedded line terminator",
        ));
    }
    Ok(token.to_string())
}

/// [`read_token_for`] with the miss kept: which file was looked for and why it
/// could not be read, so discovery reports it per socket.
///
/// Candidates follow [`control_socket::token_names_for_sock`]: the per-socket
/// file, then (explicit sockets only) the legacy shared `aterm.token`. Only an
/// absent or unreadable file moves on; an existing but invalid per-socket token
/// fails there rather than reaching for a directory-shared credential. A miss
/// names the per-socket path.
fn read_token_at(path: &str) -> Result<String, (PathBuf, io::Error)> {
    read_token_file_at(path).map(|(_, token)| token)
}

/// [`read_token_at`] that also names the file a token was read from, so a
/// refusal of it can say which file the server did not accept.
fn read_token_file_at(path: &str) -> Result<(PathBuf, String), (PathBuf, io::Error)> {
    let p = Path::new(path);
    let (Some(dir), Some(file_name)) = (p.parent(), p.file_name()) else {
        return Err((
            PathBuf::from(path),
            io::Error::new(io::ErrorKind::NotFound, "socket path names no directory"),
        ));
    };
    // `latest::target_name` is `read_link` + final component on Unix, and the
    // pointer file's validated contents on Windows — the same relative name.
    let sock_name = aterm_uds::latest::target_name(p).unwrap_or_else(|| file_name.to_os_string());
    let names = control_socket::token_names_for_sock(&sock_name.to_string_lossy());
    let mut miss: Option<(PathBuf, io::Error)> = None;
    for name in names {
        let token_path = dir.join(name);
        match read_control_token_file(&token_path) {
            Ok(token) => return Ok((token_path, token)),
            Err(e) => {
                // A malformed or non-regular file exists for this socket; do not
                // reach past it for a directory-shared legacy credential.
                if matches!(
                    e.kind(),
                    io::ErrorKind::InvalidData | io::ErrorKind::InvalidInput
                ) {
                    return Err((token_path, e));
                }
                // Keep the first (per-socket) miss: it is the file this build
                // wants, and the one whose absence a report should name.
                miss.get_or_insert((token_path, e));
            }
        }
    }
    Err(miss.unwrap_or_else(|| {
        (
            PathBuf::from(path),
            io::Error::new(io::ErrorKind::NotFound, "socket path names no token"),
        )
    }))
}

/// The exit code for a timeout, additive over 0 = OK / 1 = failure (still
/// nonzero). Used for a client-side socket deadline ([`is_timeout_error`]) and
/// for a server reply reporting one (a `turn` verdict with `status=timeout`, or
/// an `await`/`ready`/`wait` `OK timeout`; [`reply_is_timeout`]).
const EXIT_TIMEOUT: u8 = 124;

/// Whether an [`io::Error`] is a socket deadline expiry: `WouldBlock` (Unix
/// `SO_RCVTIMEO`/`SO_SNDTIMEO`) or `TimedOut` (Windows). Maps to
/// [`EXIT_TIMEOUT`].
fn is_timeout_error(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

/// The words for a request the server never answered: the connect to `path`
/// succeeded, then a read or write waited out the whole `deadline`. The OS says
/// only EAGAIN, so the line names the socket and the wait and says `timed out`
/// (the words `CtlReply::lost` in aterm-agent's supervise/run.rs reads). The
/// kind is kept, so the run exits [`EXIT_TIMEOUT`].
///
/// Before any of the answer came ([`Unanswered`]): `no reply from <sock> within
/// <N>s (connected; the server did not answer)`. After that: `reply from <sock>
/// stopped part-way: nothing more within <N>s (connected; the server began its
/// answer, then went quiet)`.
///
/// Passed on unchanged: every other kind, an already [`Worded`] error, and any
/// error when no deadline was set.
fn no_reply_error(e: io::Error, path: &str, deadline: Option<std::time::Duration>) -> io::Error {
    no_reply_error_from(e, path, true, deadline)
}

/// [`no_reply_error`] naming `who` (a socket path, or a `dial`'s remote and
/// relay). `local` adds `connected; the server …`, which a `dial` cannot say
/// (remote or relay went quiet, indistinguishably).
fn no_reply_error_from(
    e: io::Error,
    who: &str,
    local: bool,
    deadline: Option<std::time::Duration>,
) -> io::Error {
    let Some(deadline) = deadline else {
        return e;
    };
    if !is_timeout_error(&e) || is_worded(&e) {
        return e;
    }
    let mut msg = String::new();
    if is_unanswered(&e) {
        msg.push_str("no reply from ");
        msg.push_str(who);
        msg.push_str(" within ");
        msg.push_str(&deadline_words(deadline));
        if local {
            msg.push_str(" (connected; the server did not answer)");
        }
    } else {
        msg.push_str("reply from ");
        msg.push_str(who);
        msg.push_str(" stopped part-way: nothing more within ");
        msg.push_str(&deadline_words(deadline));
        if local {
            msg.push_str(" (connected; the server began its answer, then went quiet)");
        }
    }
    msg.push_str(" — the request timed out");
    io::Error::new(e.kind(), Worded(io::Error::other(msg)))
}

/// A deadline-kind error from before any of the answer came: sending the
/// request ([`send_request`], a `--stdin` payload) or waiting for the status
/// line ([`read_status_line`]). Only this gets `no reply … (connected; the
/// server did not answer)`; kind and words are the inner error's.
#[derive(Debug)]
struct Unanswered(io::Error);

impl std::fmt::Display for Unanswered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for Unanswered {}

/// Mark a deadline-kind `e` [`Unanswered`]; every other error, one already
/// marked, and one already [`Worded`] come back as they came.
fn unanswered(e: io::Error) -> io::Error {
    if is_timeout_error(&e) && !is_worded(&e) && !is_unanswered(&e) {
        io::Error::new(e.kind(), Unanswered(e))
    } else {
        e
    }
}

fn is_unanswered(e: &io::Error) -> bool {
    e.get_ref().is_some_and(|inner| inner.is::<Unanswered>())
}

/// Read the reply's status line via [`read_bounded_line`], marking a deadline
/// that fires here [`Unanswered`]. Every [`converse`] conversation reads its
/// status line through this.
fn read_status_line<R: BufRead>(reader: &mut R, line: &mut String) -> io::Result<usize> {
    read_bounded_line(reader, line).map_err(unanswered)
}

/// A deadline as [`no_reply_error`] says it: whole seconds as `900s`, a
/// remainder to a tenth (`12.5s`), under a second in milliseconds (`250ms`).
/// Fractions come from the one retry after a hang-up, which runs on what the
/// deadline had left (1 ms floor).
fn deadline_words(d: std::time::Duration) -> String {
    let mut words = String::new();
    if d.subsec_nanos() == 0 {
        words.push_str(&d.as_secs().to_string());
        words.push('s');
    } else if d.as_secs() == 0 {
        words.push_str(&d.as_millis().max(1).to_string());
        words.push_str("ms");
    } else {
        words.push_str(&d.as_secs().to_string());
        words.push('.');
        words.push_str(&(d.subsec_millis() / 100).to_string());
        words.push('s');
    }
    words
}

/// A deadline-kind error whose words are final, passed on by
/// [`no_reply_error`] as is: its own line, a stdout/stderr write that would
/// block ([`StdoutSink`], [`stderr_line`]), and a successor's own exchange
/// after a hang-up. Kind and words are the inner error's.
#[derive(Debug)]
struct Worded(io::Error);

impl std::fmt::Display for Worded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for Worded {}

/// Mark a deadline-kind `e` [`Worded`]; every other error, and one already
/// marked, comes back as it came.
fn worded(e: io::Error) -> io::Error {
    if is_timeout_error(&e) && !is_worded(&e) {
        io::Error::new(e.kind(), Worded(e))
    } else {
        e
    }
}

fn is_worded(e: &io::Error) -> bool {
    e.get_ref().is_some_and(|inner| inner.is::<Worded>())
}

/// The whole client: `argv[1..]` in, process exit code out. The `aterm` binary
/// calls this for `aterm ctl …` (and the `aterm-ctl` argv0 alias); `src/main.rs`
/// wraps it for a standalone build.
pub fn main_entry(argv: Vec<std::ffi::OsString>) -> ExitCode {
    main_entry_with(argv, LocalVerbs::NONE)
}

/// [`main_entry`] with the verbs the embedding binary answers in-process
/// ([`LocalVerbs`]): the `aterm` binary links the engine and hands in the `cast
/// drift` analyzer. This crate stays dependency-free; the standalone
/// `aterm-ctl` passes none.
pub fn main_entry_with(argv: Vec<std::ffi::OsString>, local: LocalVerbs) -> ExitCode {
    match real_main(argv, local) {
        Ok(code) => code,
        Err(e) => {
            // The reader closed stdout (`aterm ctl help --full | head`): nothing is
            // wrong, so no line and status 0.
            if is_stdout_closed(&e) {
                return ExitCode::SUCCESS;
            }
            // Manual `eprintln!("aterm-ctl: {e}")` (the strict Trust gate cannot
            // lower inline `format_args!`); a failed write is ignored while
            // exiting.
            let _ = stderr_line(&e.to_string());
            exit_for(&e)
        }
    }
}

/// The exit status of a failed run: 124 for a client-side socket deadline, 0
/// when the reader closed stdout, 1 otherwise.
fn exit_for(e: &io::Error) -> ExitCode {
    if is_stdout_closed(e) {
        ExitCode::SUCCESS
    } else if is_timeout_error(e) {
        ExitCode::from(EXIT_TIMEOUT)
    } else {
        ExitCode::FAILURE
    }
}

/// The reader closed stdout (`| head`), so the run ends silently with status 0.
/// Only stdout writes carry this mark; a broken pipe on the socket is the
/// server hanging up and keeps its non-zero exit.
#[derive(Debug)]
struct StdoutClosed;

impl std::fmt::Display for StdoutClosed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("stdout closed by its reader")
    }
}

impl std::error::Error for StdoutClosed {}

/// Mark a stdout write's broken pipe as [`StdoutClosed`]; every other failure
/// (ENOSPC, a dead downstream) is returned as is.
fn stdout_closed(e: io::Error) -> io::Error {
    if e.kind() == io::ErrorKind::BrokenPipe {
        io::Error::new(io::ErrorKind::BrokenPipe, StdoutClosed)
    } else {
        e
    }
}

fn is_stdout_closed(e: &io::Error) -> bool {
    e.get_ref().is_some_and(|inner| inner.is::<StdoutClosed>())
}

/// A stdout writer whose broken pipe is [`StdoutClosed`]. Every stdout path
/// writes through it (`every_stdout_writer_goes_through_the_sink`), so `|
/// head` ends each the same way. A write that would block is [`Worded`], never
/// reported as the server's silence ([`no_reply_error`]).
struct StdoutSink<W>(W);

impl<W: Write> Write for StdoutSink<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf).map_err(stdout_failure)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush().map_err(stdout_failure)
    }
}

/// A stdout write's failure, marked: broken pipe is [`StdoutClosed`], would-block
/// is [`Worded`]; every other failure is as it came.
fn stdout_failure(e: io::Error) -> io::Error {
    worded(stdout_closed(e))
}

/// The usage error for an invocation with no verb: `usage: <SYNOPSIS>`,
/// byte-identical to the previous `format!("usage: {SYNOPSIS}")`.
fn usage_error() -> io::Error {
    let mut msg = String::from("usage: ");
    msg.push_str(SYNOPSIS);
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

/// Whether `parts` is a `dial` request without a verb for the remote (`dial` or
/// `dial <name>`). The remote never closes the relay on its own, so the client
/// rejects it ([`dial_needs_verb_error`]) rather than block forever.
fn dial_missing_verb(parts: &[String]) -> bool {
    parts.first().map(String::as_str) == Some("dial") && parts.len() < 3
}

/// The error for a bare `dial <name>`: the remote is a persistent connection,
/// so a one-shot read would never finish. Hand-composed (no inline
/// `format_args!`); exits FAILURE, not 124.
fn dial_needs_verb_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "ERR dial: give a verb to run on the remote (dial <name> <verb...>)",
    )
}

/// The discovery verbs this client answers; none takes an argument
/// (`--sock`/`--pid` narrow the scope).
const DISCOVERY_VERBS: [&str; 3] = ["ls", "instances", "windows"];

/// The discovery verb in `parts` when it carries arguments it has no use for;
/// `None` for a bare one or a non-discovery request. `windows 1` or `instances
/// --all` meant something a silently ignored argument would pretend to honour,
/// so it is rejected before any socket is dialled.
fn discovery_given_arguments(parts: &[String]) -> Option<&'static str> {
    let verb = parts.first().map(String::as_str)?;
    let verb = DISCOVERY_VERBS.iter().copied().find(|v| *v == verb)?;
    (parts.len() > 1).then_some(verb)
}

/// The usage error for a discovery verb given arguments, in the `ERR usage:
/// <verb>` shape server-side usage errors take. Exits FAILURE.
fn discovery_usage_error(verb: &str) -> io::Error {
    let mut msg = String::from("ERR usage: ");
    msg.push_str(verb);
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

/// The verbs the CLIENT VERBS block of `--help` documents — the set `help <verb>`
/// is answered from that block, here. None of them is in the protocol table, so
/// the server's `help` cannot know them: `help ls` came back `ERR unknown verb
/// 'ls' (help lists them)` while `ls` itself worked, which told an agent the
/// listing verb it had just been taught did not exist.
pub const CLIENT_HELP_VERBS: [&str; 4] = ["ls", "instances", "windows", "mux"];

/// The `--help` entry for one client verb from [`CLIENT_VERBS`]: its line plus
/// continuation lines (18-column gutter), up to the next entry or the blank
/// line before the prose, with the 4-column indent removed so rows read like
/// the server's `help <verb>`. `None` for an undocumented verb.
fn client_verb_entry(verb: &str) -> Option<Vec<String>> {
    const HELP_INDENT: &str = "    ";
    const CONTINUATION: &str = "                  ";
    // A verb line is the indent, the verb padded to the gutter, then text in the
    // gutter column. Prose at the same indent never pads a word to the gutter,
    // and a verb that prefixes another is not matched.
    let head_prefix = format!(
        "{HELP_INDENT}{verb:<width$}",
        width = CONTINUATION.len() - HELP_INDENT.len()
    );
    let mut lines = CLIENT_VERBS.lines();
    let head = lines.find(|l| {
        l.strip_prefix(head_prefix.as_str())
            .is_some_and(|text| !text.is_empty() && !text.starts_with(' '))
    })?;
    let mut entry = vec![head[HELP_INDENT.len()..].to_string()];
    entry.extend(
        lines
            .take_while(|l| l.starts_with(CONTINUATION))
            .map(|l| l[HELP_INDENT.len()..].to_string()),
    );
    Some(entry)
}

/// How a `help <verb>` request was asked, which decides who answers a protocol
/// verb (a client verb is always answered here: the server has never heard of it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HelpAsked {
    /// `<verb> --help`, rewritten to `help <verb>` ([`help_request_rewrite`]):
    /// answered from the linked `control_verbs` table with no socket, so a
    /// dead or missing socket does not turn the question into a connect error.
    ByFlag,
    /// `help <verb>` typed as such: the server's question, since its `cmd_help`
    /// describes its own build.
    ByVerb,
}

/// The client-side answer to `help <verb>`: a client verb from the client-verbs
/// block either way, and a protocol verb from the `control_verbs` table only
/// when asked as `<verb> --help` ([`HelpAsked::ByFlag`]). `None` for everything
/// else, which goes to the server (an explicit `help <protocol verb>`, bare
/// `help`, `help --full`, `help --json`, an unknown name, `help ls extra`).
/// Framed as the server frames `help <verb>` (`OK <n>` + n rows).
fn client_help_reply(parts: &[String], asked: HelpAsked) -> Option<String> {
    let [verb, name] = parts else {
        return None;
    };
    if verb != "help" {
        return None;
    }
    let lines = if CLIENT_HELP_VERBS.contains(&name.as_str()) {
        client_verb_entry(name)?
    } else if asked == HelpAsked::ByFlag {
        aterm_types::control_verbs::spec(name)?.entry_lines()
    } else {
        return None;
    };
    let mut reply = format!("OK {}\n", lines.len());
    for line in lines {
        reply.push_str(&line);
        reply.push('\n');
    }
    Some(reply)
}

/// `help <verb>` for a verb this client's table knows. The server's entry is
/// printed as is. What an older server cannot say is filled in: a verb it calls
/// unknown gets this client's entry (with a stderr note that the server will
/// refuse it), and `help cast` from a server without `cast drift` notes that
/// this client computes it. Other refusals print as in `exchange`; an
/// unaccepted connection is named within [`ACCEPT_DEADLINE`], and a deadline
/// once served is [`converse`]'s line ([`converse_served`]).
fn help_with_fallback(
    path: &str,
    origin: TargetOrigin,
    name: &str,
    deadline: Option<std::time::Duration>,
    local: &LocalVerbs,
) -> io::Result<ExitCode> {
    converse_served(path, origin, deadline, |stream| {
        help_on(stream, name, local)
    })
    .map(|served| served.unwrap_or_else(std::convert::identity))
}

/// [`help_with_fallback`] once served: `stream` is accepted, authenticated and
/// bounded ([`converse_served`]), so the request goes out ([`send_served`]).
fn help_on(stream: CtlStream, name: &str, local: &LocalVerbs) -> io::Result<ExitCode> {
    if let Some(code) = send_served(&stream, format!("help {name}\n").as_bytes())? {
        return Ok(code);
    }
    let mut reader = BufReader::new(&stream);
    let mut status = String::new();
    if read_status_line(&mut reader, &mut status)? == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "server closed the connection without responding",
        ));
    }
    let status = status.trim_end_matches(['\r', '\n']);
    let Some(tail) = status.strip_prefix("OK ") else {
        drain_refusal_tail(&mut reader)?;
        if status.starts_with("ERR unknown verb")
            && let Some(spec) = aterm_types::control_verbs::spec(name)
        {
            let rows = spec.entry_lines();
            let mut reply = format!("OK {}\n", rows.len());
            for row in rows {
                reply.push_str(&row);
                reply.push('\n');
            }
            print_framed_lines(&reply)?;
            stderr_line(&format!(
                "the aterm that answered predates `{name}` ({status}); the entry above is this \
                 client's (aterm {}), and that aterm will refuse the verb",
                aterm_types::version::APP_VERSION
            ))?;
            return Ok(ExitCode::SUCCESS);
        }
        stderr_line(status)?;
        return Ok(ExitCode::FAILURE);
    };
    let n = stream_count(tail).ok_or_else(|| malformed_header_error(status))?;
    let rows = drift::read_lines(&mut reader, n)?;
    let mut reply = format!("OK {}\n", rows.len());
    for row in &rows {
        reply.push_str(row);
        reply.push('\n');
    }
    print_framed_lines(&reply)?;
    if name == "cast"
        && local.cast_drift.is_some()
        && !rows.iter().any(|row| row.contains("cast drift"))
    {
        stderr_line(
            "the aterm that answered predates `cast drift`; this client computes it itself \
             (`aterm ctl cast drift`, its header ending computed=client) — this client's entry \
             is in `aterm help introspection`",
        )?;
    }
    Ok(ExitCode::SUCCESS)
}

/// Print a line-framed reply (`OK <n>` + n rows) as [`exchange`] does: rows to
/// stdout, header consumed, one buffered write flushed explicitly.
fn print_framed_lines(reply: &str) -> io::Result<()> {
    let stdout = stdout_handle();
    let mut out = StdoutSink(io::BufWriter::new(stdout.lock()));
    for line in reply.lines().skip(1) {
        out.write_all(line.as_bytes())?;
        out.write_all(b"\n")?;
    }
    out.flush()
}

/// The command-line arguments past `argv[0]` as UTF-8 strings. The protocol
/// frames requests as UTF-8, so a non-UTF-8 argument is a usage error here
/// (`env::args` would panic). `into_string` goes through the fn-item shape (see
/// `read_token_for`).
#[allow(
    clippy::unnecessary_map_on_constructor,
    reason = "OsString::into_string is passed as a fn-item to a combinator so the strict Trust gate keeps it an opaque cross-crate call instead of inlining std internals it cannot lower"
)]
fn utf8_args(argv: Vec<std::ffi::OsString>) -> io::Result<Vec<String>> {
    let mut args = Vec::new();
    for arg in argv {
        match Some(arg)
            .map(std::ffi::OsString::into_string)
            .and_then(Result::ok)
        {
            Some(arg) => args.push(arg),
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "arguments must be valid UTF-8 (the control protocol is UTF-8 text)",
                ));
            }
        }
    }
    Ok(args)
}

/// A `--pid` value: a number naming a process. 0 names none (it is the
/// placeholder an entry with no `pid` line enumerates as), so it is refused
/// here, once, for every verb.
fn parse_pid(v: &str) -> io::Result<u32> {
    match v.parse::<u32>() {
        Ok(0) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--pid 0 names no process; address that instance with --sock <path> (`aterm ctl \
             instances` lists the sockets)",
        )),
        Ok(pid) => Ok(pid),
        Err(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--pid requires a numeric PID",
        )),
    }
}

/// Parse arguments, frame the request, talk to the server, and print the reply.
///
/// Returns the process exit code on a completed exchange (`SUCCESS` for `OK`,
/// `FAILURE` for an `ERR`/unexpected status), or an [`io::Error`] for usage or
/// connection problems (surfaced on stderr by [`main`]).
fn real_main(argv: Vec<std::ffi::OsString>, local: LocalVerbs) -> io::Result<ExitCode> {
    // Flag parsing stops at the first positional: everything from the verb on is
    // the request, so a literal "--sock" in a payload is never taken as a flag.
    let mut args = utf8_args(argv)?.into_iter();
    let mut sock: Option<String> = None;
    let mut pid: Option<u32> = None;
    // The per-op socket read/write deadline. `Some(EXCHANGE_DEADLINE)` is the
    // default; `--timeout SECS` overrides it (0 => `None`, no deadline).
    let mut deadline: Option<std::time::Duration> = Some(EXCHANGE_DEADLINE);
    // Whether `--timeout` was passed explicitly: `subscribe` watches forever by
    // default and treats an explicit value as a wall-clock max, which the
    // deadline value alone cannot tell apart.
    let mut timeout_explicit = false;
    let mut request_parts: Vec<String> = Vec::new();

    while let Some(arg) = args.next() {
        if arg == "-h" || arg == "--help" {
            // Only before the verb; a later --help is payload (handled below).
            // `help_text` already ends in a newline, so write it directly.
            let help = help_text();
            let stdout = stdout_handle();
            let mut out = StdoutSink(stdout.lock());
            out.write_all(help.as_bytes())?;
            // Drop the guard explicitly so the lexical lock-order census sees it
            // released before any later helper takes stdout.
            drop(out);
            return Ok(ExitCode::SUCCESS);
        } else if arg == "-V" || arg == "--version" {
            print_stdout_line(&format!("aterm-ctl {}", aterm_types::version::APP_VERSION))?;
            return Ok(ExitCode::SUCCESS);
        } else if arg == "--completions" {
            // Hidden pre-verb flag (not in `--help`): print the shell completion
            // script and exit. After a verb it is part of the request.
            let shell = args.next().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "--completions requires a shell name (bash, zsh, or fish)",
                )
            })?;
            return emit_completions(&shell);
        } else if let Some(shell) = arg.strip_prefix("--completions=") {
            return emit_completions(shell);
        } else if arg == "--sock" {
            sock = Some(args.next().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "--sock requires a PATH")
            })?);
        } else if let Some(p) = arg.strip_prefix("--sock=") {
            sock = Some(p.to_string());
        } else if arg == "--pid" {
            let v = args.next().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "--pid requires a PID")
            })?;
            pid = Some(parse_pid(&v)?);
        } else if let Some(v) = arg.strip_prefix("--pid=") {
            pid = Some(parse_pid(v)?);
        } else if arg == "--timeout" {
            let v = args.next().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "--timeout requires a value in SECONDS (0 = no deadline)",
                )
            })?;
            deadline = parse_timeout(&v)?;
            timeout_explicit = true;
        } else if let Some(v) = arg.strip_prefix("--timeout=") {
            deadline = parse_timeout(v)?;
            timeout_explicit = true;
        } else if arg == "--" {
            // End of the client's options: what follows is the verb and its
            // arguments, whatever they start with.
            request_parts.extend(args.by_ref());
            break;
        } else if arg.starts_with('-') {
            // A dash-leading token before the verb is an option this client does
            // not have; forwarded, it would become the verb.
            let mut msg = String::from("unknown option ");
            msg.push_str(&arg);
            msg.push_str(" (aterm ctl --help lists them)");
            return Err(io::Error::new(io::ErrorKind::InvalidInput, msg));
        } else {
            // First positional is the verb; the remainder is its argument list.
            request_parts.push(arg);
            request_parts.extend(args.by_ref());
            break;
        }
    }

    // `<verb> … --help` describes the verb and never runs it (otherwise `turn
    // --help` would press Enter and `window --help` would write a file named
    // `--help`). It answers with the verb's `help <verb>` catalog row from the
    // linked table, with no socket ([`HelpAsked::ByFlag`]); an explicit `help
    // <verb>` still asks the server. It runs before the socket dial, the `mux`
    // report and the discovery verbs, so it covers the whole surface.
    let mut help_asked = HelpAsked::ByVerb;
    if let Some(rewritten) = help_request_rewrite(&request_parts) {
        help_asked = HelpAsked::ByFlag;
        // `send` and `paste` take arbitrary literal text, so they are the only
        // verbs where this can intercept something meant; name the escape.
        if matches!(request_parts[0].as_str(), "send" | "paste") {
            let verb = &request_parts[0];
            eprintln!(
                "note: `--help` describes `{verb}`; to send it as literal text, \
                 pipe it -- printf %s -- --help | aterm ctl {verb} --stdin"
            );
        }
        request_parts = rewritten;
    }

    // The multiplexer boundary, decided before `@self` is expanded (after
    // expansion the implicit-target shape is gone). `mux` reports the decision;
    // other verbs are refused only when their target is implicit and an aterm
    // session identity is in scope.
    let nesting = mux_nesting_from_env();
    if request_parts.first().map(String::as_str) == Some("mux") {
        announce_mux_boundary(nesting.as_ref(), true);
        return run_mux_report(nesting.as_ref());
    }
    if let Some(n) = nesting.as_ref()
        && n.outer_sid.is_some()
        && targets_own_session_implicitly(&request_parts, sock.as_deref(), pid)
    {
        announce_mux_boundary(nesting.as_ref(), true);
        return Err(nested_self_drive_error(n));
    }

    // Expand a leading `@self` / `@env` to `@$ATERM_PARENT_SESSION_ID` before
    // anything reads the selector.
    expand_self_selector(&mut request_parts, env::var(SELF_SID_ENV).ok())?;

    // The boundary notice: a bash/zsh pane never sources the integration (see
    // [`MUX_ENV`]), so `aterm ctl` says it once per multiplexer, sharing the
    // integration's stamp. After `@self` expansion so the `blocks`/`status`
    // check matches the trailing degradation note.
    announce_mux_boundary(
        nesting.as_ref(),
        nesting
            .as_ref()
            .is_some_and(|n| mux_degradation_note(n, &request_parts).is_some()),
    );

    // `dial <name> <verb...>` runs one verb on a saved remote and reads back its
    // framed reply. A bare `dial <name>` would block forever (the remote never
    // closes), so it fails before anything is sent.
    if dial_missing_verb(&request_parts) {
        return Err(dial_needs_verb_error());
    }

    let Some(verb) = forwarded_verb(&request_parts) else {
        return Err(usage_error());
    };

    // The discovery verbs enumerate every live instance's socket, so this client
    // answers them; anything else is one request to one socket. `--sock`/`--pid`
    // scope the enumeration, so a caller with an isolated instance never sees
    // the user's real terminals.
    if let Some(verb) = request_parts
        .first()
        .map(String::as_str)
        .filter(|v| DISCOVERY_VERBS.contains(v))
    {
        if let Some(verb) = discovery_given_arguments(&request_parts) {
            return Err(discovery_usage_error(verb));
        }
        return run_discovery(verb, sock.as_deref(), pid, nesting.as_ref());
    }
    // `help <verb>` for a client verb (unknown to the server) or a protocol verb
    // asked as `<verb> --help` is answered here with no socket; an explicit
    // `help <protocol verb>` goes to the server.
    if let Some(reply) = client_help_reply(&request_parts, help_asked) {
        print_framed_lines(&reply)?;
        return Ok(ExitCode::SUCCESS);
    }
    // `cast drift --file <cast> [--screen <text>] [--until <t>]` replays saved
    // files, so it is answered here too, before any socket (the `drift` module).
    if let Some(code) = drift::offline_entry(&request_parts, &local)? {
        return Ok(code);
    }

    // Where a successor may answer if an aterm self-update replaces the server
    // mid-exchange: the same resolution inputs, minus the dead-alias fallback.
    let redial = redial::Redial::new(redial_follow(
        sock.as_deref(),
        pid,
        env::var(SELF_SID_ENV).ok(),
    ));
    let (path, origin) = resolve_target(sock, pid, env::var(SELF_SID_ENV).ok())?;

    // Stdin payloads: `feed-bin`, and the `send`/`paste` `--stdin`/`-` forms,
    // deliver raw stdin bytes through the length-prefixed binary frame, the one
    // wire form carrying embedded newlines exactly (a request line would split
    // or reject them).
    let (selector, rest) = split_selector(&request_parts);
    let verb_tok = rest.first().map(String::as_str);
    let verb_args = rest.get(1..).unwrap_or(&[]);
    if verb_tok == Some("operator-propose-bin") {
        if selector.is_some() || !verb_args.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "operator-propose-bin reads one JSON proposal from stdin and takes no selector or inline arguments",
            ));
        }
        let payload = read_stdin_payload()?;
        validate_operator_proposal_size(payload.len())?;
        return feed_bin_exchange(&path, origin, "operator-propose-bin", &payload, deadline);
    }
    if verb_tok == Some("feed-bin") {
        // The payload is stdin, not an inline argument; an inline token (e.g. a
        // bare length) has no client path and would only desync the frame.
        if !verb_args.is_empty() {
            return Err(feed_bin_inline_error());
        }
        let payload = read_stdin_payload()?;
        return feed_bin_exchange(
            &path,
            origin,
            &binary_frame_prefix(selector, "feed-bin"),
            &payload,
            deadline,
        );
    }
    if matches!(verb_tok, Some("send") | Some("paste"))
        && verb_args.len() == 1
        && matches!(verb_args[0].as_str(), "--stdin" | "-")
    {
        let payload = read_stdin_payload()?;
        // `send --stdin` is a raw feed (`feed-bin`); `paste --stdin` goes through
        // the server's paste seam (`paste-bin`: bracketed paste + control-byte
        // sanitizing, as the inline `paste` verb).
        let frame_verb = if verb_tok == Some("paste") {
            "paste-bin"
        } else {
            "feed-bin"
        };
        return feed_bin_exchange(
            &path,
            origin,
            &binary_frame_prefix(selector, frame_verb),
            &payload,
            deadline,
        );
    }

    // Reject line terminators before framing: the socket is newline-delimited,
    // so an embedded '\n'/'\r' would inject a second authenticated verb.
    validate_request_parts(&request_parts)?;

    // `cast drift` asks the server, and computes the report here (the `drift`
    // module) when the server predates the verb and this binary links the
    // engine; `dial <name> [@<sid>] cast drift` too, all through the relay.
    {
        let (selector, rest) = split_selector(&request_parts);
        if drift::is_cast_drift(rest) {
            let route = drift::Route {
                dial: None,
                selector,
            };
            return drift::live(&path, origin, route, &rest[2..], deadline, &local);
        }
        if let Some((name, selector, rest)) = drift::split_dial(&request_parts)
            && drift::is_cast_drift(rest)
        {
            let route = drift::Route {
                dial: Some(name),
                selector,
            };
            return drift::live(&path, origin, route, &rest[2..], deadline, &local);
        }
    }
    // `help <verb>` (and `<verb> --help`) asks the server, whose table is what it
    // speaks, and says what an older server cannot (`help_with_fallback`).
    if let [help, name] = request_parts.as_slice()
        && help == "help"
        && aterm_types::control_verbs::spec(name).is_some()
    {
        return help_with_fallback(&path, origin, name, deadline, &local);
    }

    // The wire never quotes (see `split_pattern_note`): note which pattern
    // element the join is about to split, then still send it. A failed note
    // write is ignored.
    if let Some(note) = split_pattern_note(&request_parts) {
        let _ = stderr_line(&note);
    }

    // One request per line: "VERB [args...]\n", args joined with single spaces
    // (inter-arg whitespace collapses).
    let mut request = request_parts.join(" ");
    request.push('\n');

    // For `dial <name> <verb...>` the reply is the remote's answer to
    // `<verb...>`, so framing follows the forwarded sub-request, not the `dial
    // <name> …` line sent to the local relay.
    let frame_request = if request_parts.first().map(String::as_str) == Some("dial") {
        request_parts[2..].join(" ")
    } else {
        request.clone()
    };

    let outcome = exchange(
        &path,
        origin,
        &request,
        &verb,
        &frame_request,
        deadline,
        timeout_explicit,
        &redial,
    );
    // After the reply, so the caveat lands under the result it explains, and
    // silenced by the same ATERM_MUX_NOTICE=0 as the boundary notice. A failed
    // write is ignored.
    if let Some(note) = nesting
        .as_ref()
        .filter(|_| env::var(MUX_NOTICE_ENV).ok().as_deref() != Some("0"))
        .and_then(|n| mux_degradation_note(n, &request_parts))
    {
        let _ = stderr_line(&note);
    }
    outcome
}

/// Split an optional leading `@<selector>` off the request parts, returning
/// `(selector, rest)` with `rest` starting at the verb. `@self` is already
/// expanded upstream ([`expand_self_selector`]).
fn split_selector(parts: &[String]) -> (Option<&str>, &[String]) {
    match parts.first() {
        Some(s) if s.starts_with('@') => (Some(s.as_str()), &parts[1..]),
        _ => (None, parts),
    }
}

/// A binary-frame request head without the length: `<verb>` or `@<sel> <verb>`,
/// where `<verb>` is `feed-bin` (raw feed) or `paste-bin` (paste seam). The
/// caller appends ` <len>\n` and the body.
fn binary_frame_prefix(selector: Option<&str>, verb: &str) -> String {
    let mut s = String::new();
    if let Some(sel) = selector {
        s.push_str(sel);
        s.push(' ');
    }
    s.push_str(verb);
    s
}

/// Expand `@self` / `@env` to a concrete `@<sid>` from
/// `$ATERM_PARENT_SESSION_ID` (`self_sid`) before framing. Unlike flagless and
/// `@.`, which follow the active tab, this is stable across tab switches.
/// `@<sid>` and `@.` pass through. Errors when the variable is unset or empty;
/// `parts` is then unchanged.
///
/// Expands in every selector position: the leading proxy selector (`@self
/// screen`), `subscribe`'s selector-second form (`subscribe @self
/// screen,events`), and inside a comma list (`subscribe @self,@1 screen`).
fn expand_self_selector(parts: &mut [String], self_sid: Option<String>) -> io::Result<()> {
    // Selector token position(s): `subscribe` is selector-SECOND, every other verb
    // takes an optional leading `@<sel>` proxy at parts[0].
    let sel_idx = if parts.first().map(String::as_str) == Some("subscribe") {
        1
    } else {
        0
    };
    let Some(tok) = parts.get(sel_idx) else {
        return Ok(());
    };
    // A comma list element is `@self`/`@env` exactly (not a substring of a real sid).
    let has_self = tok.split(',').any(|e| e == "@self" || e == "@env");
    if !has_self {
        return Ok(());
    }
    let sid = self_sid
        .filter(|s| !s.is_empty())
        .ok_or_else(self_selector_error)?;
    let expanded = tok
        .split(',')
        .map(|e| {
            if e == "@self" || e == "@env" {
                format!("@{sid}")
            } else {
                e.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    parts[sel_idx] = expanded;
    Ok(())
}

/// The error for `@self` / `@env` outside an aterm session (a static message;
/// the strict Trust gate cannot lower inline `format_args!`).
fn self_selector_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "ERR @self: not inside an aterm session ($ATERM_PARENT_SESSION_ID unset)",
    )
}

/// Parse `--timeout <secs>` into the per-op socket deadline: `0` disables it
/// (`None`), any other non-negative integer is that many seconds; anything else
/// is a usage error.
fn parse_timeout(v: &str) -> io::Result<Option<std::time::Duration>> {
    let secs: u64 = v.parse().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "--timeout requires a non-negative integer number of SECONDS (0 = no deadline)",
        )
    })?;
    Ok((secs != 0).then(|| std::time::Duration::from_secs(secs)))
}

/// The largest `feed-bin` payload the server accepts in one frame (256 KiB, the
/// server's `MAX_FEED_BIN`). Larger stdin payloads are refused up front, since
/// the server would refuse the body and drop the connection.
const MAX_FEED_BIN: usize = 256 * 1024;

/// The error for `feed-bin` with an inline argument: the payload comes from
/// stdin, and an inline token would desync the frame.
fn feed_bin_inline_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "feed-bin reads its binary payload from stdin: pipe the bytes in \
         (e.g. `printf ... | aterm-ctl feed-bin`); no inline length is accepted",
    )
}

/// The error for a stdin payload over the server's 256 KiB `feed-bin` cap,
/// raised before any bytes are sent.
fn oversized_feed_bin_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "stdin payload exceeds the 256 KiB feed-bin cap; split it into smaller frames",
    )
}

/// Read the whole binary payload from stdin for a `feed-bin` frame, bounded by
/// [`MAX_FEED_BIN`] + 1 so an over-cap body is rejected here. Returns the raw
/// bytes.
fn read_stdin_payload() -> io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    // `take(cap + 1)` lets us detect an over-cap body without reading an
    // unbounded amount from a pipe.
    io::stdin()
        .lock()
        .take(MAX_FEED_BIN as u64 + 1)
        .read_to_end(&mut buf)?;
    if buf.len() > MAX_FEED_BIN {
        return Err(oversized_feed_bin_error());
    }
    Ok(buf)
}

fn validate_operator_proposal_size(size: usize) -> io::Result<()> {
    if size > aterm_types::control_verbs::MAX_OPERATOR_PROPOSAL_BYTES {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "operator proposal exceeds the 65536-byte limit",
        ))
    } else {
        Ok(())
    }
}

/// Whether a status line reports a timeout: a bare `OK timeout` (the
/// `await`/`ready`/`wait`/selection-wait reply) or a `turn` verdict with
/// `status=timeout`. An `ERR ...` line never is.
fn reply_is_timeout(status_line: &str) -> bool {
    let mut it = status_line.split_whitespace();
    if it.next() != Some("OK") {
        return false;
    }
    let rest: Vec<&str> = it.collect();
    // `OK timeout` — the blocking verbs' timeout outcome.
    if rest == ["timeout"] {
        return true;
    }
    // `OK <rows> turn ... status=timeout ...` — the turn verdict.
    rest.contains(&"status=timeout")
}

/// Deliver a length-prefixed `feed-bin` frame carrying `payload` to the
/// target's PTY. `prefix` is the head without the length (`feed-bin` or `@<sel>
/// feed-bin`). Used by the stdin payload paths (`feed-bin`, `send --stdin`,
/// `paste --stdin`), since only this frame carries embedded newlines exactly.
/// Authenticates like [`exchange`], runs under the same `deadline`, and prints
/// the server's `OK <n> bytes` (an `ERR` goes to stderr and exits FAILURE).
/// `origin` is for the connect error ([`connect_error`]).
fn feed_bin_exchange(
    path: &str,
    origin: TargetOrigin,
    prefix: &str,
    payload: &[u8],
    deadline: Option<std::time::Duration>,
) -> io::Result<ExitCode> {
    let path = &aterm_uds::latest::resolve(path);
    // `AUTH <token>\n` and the connect probe ([`converse_served`]), then `<prefix>
    // <len>\n` and the body, sent only once the instance has proved it serves.
    let served = converse_served(path, origin, deadline, |stream| {
        let mut head = String::from(prefix);
        head.push(' ');
        head.push_str(&payload.len().to_string());
        head.push('\n');
        let mut frame = head.into_bytes();
        frame.extend_from_slice(payload);
        if let Some(code) = send_served(&stream, &frame)? {
            return Ok(code);
        }

        let mut reader = BufReader::new(&stream);
        let mut status_line = String::new();
        if read_status_line(&mut reader, &mut status_line)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "server closed the connection without responding",
            ));
        }
        let status_line = status_line.trim_end_matches(['\r', '\n']);
        if status_line.split(' ').next() == Some("OK") {
            print_stdout_line(status_line)?;
            Ok(ExitCode::SUCCESS)
        } else {
            stderr_line(status_line)?;
            Ok(ExitCode::FAILURE)
        }
    })?;
    Ok(served.unwrap_or_else(std::convert::identity))
}

/// Reject request arguments containing a line terminator. The server reads one
/// verb per line, so an embedded `\n` or `\r` would frame a second,
/// already-authenticated verb.
fn validate_request_parts(parts: &[String]) -> io::Result<()> {
    if parts.iter().any(|p| p.contains('\n') || p.contains('\r')) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "arguments must not contain newline/carriage-return (the control socket is line-delimited)",
        ));
    }
    Ok(())
}

/// The request from its verb on, skipping an optional proxy selector (`@<sid>` /
/// `@.`) and the `dial <name> [@<sid>]` head as [`forwarded_verb`] does. Empty
/// when there is no verb.
fn forwarded_request(parts: &[String]) -> &[String] {
    match parts.first() {
        Some(d) if d == "dial" => match parts.get(2) {
            Some(t) if t.starts_with('@') => parts.get(3..).unwrap_or(&[]),
            _ => parts.get(2..).unwrap_or(&[]),
        },
        Some(sel) if sel.starts_with('@') => &parts[1..],
        _ => parts,
    }
}

/// The leading `key=value` option elements of a verb's arguments: every element
/// up to the first without `=` (where the payload starts) or `--`. A superset of
/// what the server treats as options (`turn` stops at an unknown key; `key` and
/// `send` know only `id=`/`if=`), so [`split_pattern_note`] may over-fire but
/// never misses.
fn leading_options(args: &[String]) -> impl Iterator<Item = &str> {
    args.iter()
        .map(String::as_str)
        .take_while(|a| *a != "--" && a.contains('='))
}

/// The wire never quotes: the request line is the argv joined with single
/// spaces and re-split server-side, so a pattern with spaces arrives as several
/// tokens (`await gone 'esc to interrupt'` would arm `esc`; `turn
/// settle=match:'BUILD SUCCESSFUL' …` would arm `BUILD` and type the rest).
/// This names the element about to be split for the pattern-taking shapes:
/// `await match|gone <re>` (with the server's `timeout=<ms>` / `timeout <ms>`
/// peel mirrored), `turn`'s `settle=match:<re>` / `settle=gone:<re>`, and
/// `key`/`send`'s `if=<re>`. A note, not a refusal. `None` when nothing will be
/// split.
fn split_pattern_note(parts: &[String]) -> Option<String> {
    let rest = forwarded_request(parts);
    let verb = rest.first()?.as_str();
    let args = &rest[1..];
    let has_ws = |s: &str| s.contains(char::is_whitespace);
    let arg = match verb {
        "await" => {
            let mut predicate: Vec<&str> = Vec::new();
            let mut i = 0;
            while i < args.len() {
                if args[i].starts_with("timeout=") {
                    i += 1;
                } else if args[i] == "timeout" && i + 1 < args.len() {
                    i += 2;
                } else {
                    predicate.push(args[i].as_str());
                    i += 1;
                }
            }
            match predicate.as_slice() {
                ["match" | "gone", re, ..] if has_ws(re) => Some(*re),
                _ => None,
            }
        }
        "turn" => leading_options(args).find(|a| {
            (a.starts_with("settle=match:") || a.starts_with("settle=gone:")) && has_ws(a)
        }),
        "key" | "send" => leading_options(args).find(|a| a.starts_with("if=") && has_ws(a)),
        _ => None,
    }?;
    Some(format!(
        "note: '{arg}' contains whitespace and is split on the wire; \
         write the pattern as ONE token (e.g. esc.to.interrupt)"
    ))
}

/// The verb that determines response framing, skipping an optional proxy
/// selector (`@<sid>` / `@.`), so `@child screen` frames as `screen`. `None`
/// only for an empty request.
fn forwarded_verb(parts: &[String]) -> Option<String> {
    match parts.first() {
        // `dial <name> [@<sid>] <verb...>`: the reply is the remote's answer, so
        // framing follows the forwarded verb, skipping an optional remote
        // selector as the local `@sel` arm does. A bare `dial <name>` is
        // rejected upstream.
        Some(d) if d == "dial" => match parts.get(2) {
            Some(t) if t.starts_with('@') => parts.get(3).cloned(),
            other => other.cloned(),
        },
        Some(sel) if sel.starts_with('@') => {
            Some(parts.get(1).cloned().unwrap_or_else(|| sel.clone()))
        }
        Some(v) => Some(v.clone()),
        None => None,
    }
}

/// `<verb> … --help` rewritten to `help <verb>`; `None` when no help token
/// follows the verb.
///
/// Only an argument that is exactly `-h` or `--help` counts, and a request
/// whose verb is already `help` is left alone, except `help --help` / `help -h`
/// alone, which becomes `help help`. The verb is found with [`forwarded_verb`],
/// so `@<sid>` and `dial <name> <verb>` are handled too.
fn help_request_rewrite(parts: &[String]) -> Option<Vec<String>> {
    let is_help_token = |a: &str| a == "-h" || a == "--help";
    // `dial --help` / `dial <name> --help` name no remote verb: they ask about
    // `dial` itself (without this they end in dial's missing-verb refusal).
    if parts.first().map(String::as_str) == Some("dial")
        && forwarded_verb(parts).is_none_or(|v| is_help_token(&v))
        && parts.iter().skip(1).any(|a| is_help_token(a))
    {
        return Some(vec![String::from("help"), String::from("dial")]);
    }
    let verb = forwarded_verb(parts)?;
    if verb == "help" {
        let tail: Vec<&str> = parts
            .iter()
            .map(String::as_str)
            .skip_while(|p| *p != "help")
            .skip(1)
            .collect();
        return matches!(tail.as_slice(), ["-h"] | ["--help"])
            .then(|| vec![String::from("help"), String::from("help")]);
    }
    parts
        .iter()
        .skip(1)
        .any(|a| a == "-h" || a == "--help")
        .then(|| vec![String::from("help"), verb])
}

/// The follow-up line count from a streaming verb's `OK <count>` header `tail`
/// (everything after the first space). Only the FIRST whitespace token is the
/// count: the server may append a free-form marker after it — `cmd_search`
/// writes `OK <n> incomplete[ capped| rewrapping]` when the list is not
/// exhaustive (control_query.rs) —
/// and parsing the whole tail would reject that header and drop every match.
/// Framing: first token = count, then n lines.
///
/// Capped at [`MAX_STREAM_LINES`], fail-closed (`None`): the count is
/// peer-supplied, and an unclamped `OK <huge>` from a hostile same-user socket
/// would size `Vec::with_capacity`.
fn stream_count(tail: &str) -> Option<usize> {
    let count: usize = tail.split_whitespace().next()?.parse().ok()?;
    (count <= MAX_STREAM_LINES).then_some(count)
}

/// What `search`'s ` incomplete` header marker means, for stderr: the cause
/// token after it, or the depth cap when there is none. An older server sends a
/// bare ` incomplete` at the match limit too, so a bare marker at 100000 names
/// the limit. `None` for a header without the marker.
fn search_incomplete_note(tail: &str) -> Option<&'static str> {
    let mut words = tail.split_whitespace();
    let count = words.next().and_then(|n| n.parse::<usize>().ok());
    (words.next() == Some("incomplete")).then(|| match words.next() {
        Some("capped") => "results stop at 100000 matches",
        None if count.is_some_and(|n| n >= 100_000) => "results stop at 100000 matches",
        Some("rewrapping") => "screen only: the history is rewrapping",
        _ => "older history not searched (search_history_lines)",
    })
}

/// Upper bound on a peer-supplied `OK <n>` line count (mirrors aterm-agent's
/// `MAX_TEXT_LINES` bound on the same wire framing).
const MAX_STREAM_LINES: usize = 200_000;

/// Whether `verb` answers `OK <nbytes>\n` followed by an `<nbytes>`-byte body:
/// `cast` streams the asciicast v2 recording, `temporal` the reconstructed
/// past-instant screen text.
fn bytes_payload(verb: &str, request: &str) -> bool {
    // From `framing_of`: bare `cast`/`temporal`, `inbox get` and `outbox` are
    // byte-framed; `cast frames`, `temporal status` and `outbox sent` are not.
    aterm_types::control_verbs::framing_of(verb, request)
        == aterm_types::control_verbs::Framing::Bytes
}

/// The body length from a byte-framed verb's `OK <nbytes>` header `tail`: only
/// the first token, capped at [`MAX_BODY_BYTES`] and fail-closed (`None`),
/// since the value is peer-supplied.
fn byte_count(tail: &str) -> Option<usize> {
    let n: usize = tail.split_whitespace().next()?.parse().ok()?;
    (n <= MAX_BODY_BYTES).then_some(n)
}

/// Upper bound on a peer-supplied byte-framed body (`cast`/`temporal`), far
/// above any legitimate body. The body is streamed through `Read::take`, so
/// this bounds output and time, not memory.
const MAX_BODY_BYTES: usize = 256 << 20; // 256 MiB

/// Upper bound on one reply line's bytes, enforced by [`read_bounded_line`] on
/// every line read from a control socket, so a server streaming bytes without
/// a newline cannot grow the buffer without bound.
///
/// Sized for the largest legitimate line: `screen` returns the whole styled
/// frame as one JSON line (~4.5 MiB for a 240×96 grid), and `image read`
/// returns base64 of the server's 4 MiB frame cap (~5.33 MiB).
const MAX_LINE_BYTES: usize = aterm_types::control_verbs::MAX_CONTROL_REPLY_LINE_BYTES;

/// Hard deadline on each socket read/write in [`exchange`]. Above the server's
/// 600 000 ms clamp on blocking verbs (`await`, `ready`, `wait`, `turn`), with
/// margin. `update check` can take longer on a slow link (its download budget
/// scales with the size cap) and needs an explicit `--timeout` (`0` disables).
/// A silent server stalls the client at most one deadline per read; a
/// trickling peer hits [`MAX_LINE_BYTES`] instead.
const EXCHANGE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(900);

// The strict Trust gate fails closed on inline `format_args!` expansions and on
// functions too big for its VC budget, so `exchange` is split into small
// panic-free helpers and user-visible strings are built with
// `push_str`/`write_all`.

/// The "connect <path>: <cause>" error, preserving the [`io::ErrorKind`].
/// `NotFound` and `ConnectionRefused` both mean no engine serves that socket,
/// so they carry a remedy: [`not_running_hint`] when no instance is reachable,
/// [`running_elsewhere_hint`] when some is. How much the latter says depends on
/// `origin` ([`TargetOrigin`]): a flagless call lists each live instance with
/// its flag, a pinned one only how many, never a different instance to drive.
/// Every other kind keeps the raw cause (a permission error is not "not
/// running"). The fleet look-up happens only for those two kinds;
/// [`connect_error_naming`] is the pure rendering.
fn connect_error(path: &str, e: &io::Error, origin: TargetOrigin) -> io::Error {
    let (dir, live) = if matches!(
        e.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    ) {
        (socket_dir(), live_instances_besides(&[path]))
    } else {
        (None, Vec::new())
    };
    connect_error_naming(path, e, origin, dir.as_deref(), &live)
}

/// [`connect_error`]'s rendering, given the target's origin, the instances
/// alive elsewhere, and the default socket dir they came from (`None` when
/// unresolvable, and the fleet is then empty).
fn connect_error_naming(
    path: &str,
    e: &io::Error,
    origin: TargetOrigin,
    dir: Option<&Path>,
    live: &[(u32, String)],
) -> io::Error {
    let mut msg = String::from("connect ");
    msg.push_str(path);
    msg.push_str(": ");
    msg.push_str(&e.to_string());
    if matches!(
        e.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    ) {
        if live.is_empty() {
            msg.push_str(&not_running_hint(origin));
        } else {
            msg.push_str(&running_elsewhere_hint(origin, dir, live));
        }
    }
    io::Error::new(e.kind(), msg)
}

/// Every instance of [`live_instances`] that accepts a connect, minus the
/// sockets in `failed` that were just dialled; sorted by pid.
///
/// Wider than `aterm ctl instances` prints (that also requires a `sessions`
/// answer within [`PROBE_DEADLINE`]), so the remedy says "lists" rather than
/// promising a match: a wedged instance is counted here because something is
/// serving, and "aterm isn't running" would be false.
fn live_instances_besides(failed: &[&str]) -> Vec<(u32, String)> {
    live_instances()
        .into_iter()
        .filter(|(_, sock)| !failed.iter().any(|path| same_socket_path(sock, path)))
        .filter(|(_, sock)| CtlStream::connect(sock).is_ok())
        .collect()
}

/// The remedy [`connect_error_naming`] appends when the socket dialled is dead
/// but other instances answer. It never suggests launching aterm, and how much
/// more it says depends on the [`TargetOrigin`]:
///
/// * `Pinned` ([`running_elsewhere_count`]): how many other instances are live
///   and that `aterm ctl instances` lists them, naming none and handing out no
///   flag, since the caller chose this socket.
/// * `Flagless` ([`running_elsewhere_flags`]): each live instance as the flag
///   that reaches it.
fn running_elsewhere_hint(
    origin: TargetOrigin,
    dir: Option<&Path>,
    live: &[(u32, String)],
) -> String {
    match origin {
        TargetOrigin::Pinned => {
            let mut hint = String::from(" — ");
            hint.push_str(&running_elsewhere_count(live.len()));
            hint
        }
        TargetOrigin::Flagless => running_elsewhere_flags(dir, live),
    }
}

/// [`running_elsewhere_hint`] for a pinned target, and a scoped listing's
/// hint ([`dominant_hint`]): aterm is running, how many other instances are
/// live, and where they are listed (as discovery's dead-`--pid` error does,
/// [`discovery_targets`]).
fn running_elsewhere_count(others: usize) -> String {
    let mut hint = String::from("nothing is serving this control socket, but aterm IS running (");
    hint.push_str(&others.to_string());
    hint.push_str(if others == 1 {
        " other instance; `aterm ctl instances` lists it)"
    } else {
        " other instances; `aterm ctl instances` lists them)"
    });
    hint
}

/// [`running_elsewhere_hint`] for a flagless target: each live instance as the
/// flag that reaches it. `--pid <N> (<sock>)` when the socket is the default
/// dir's per-instance socket for `N` ([`reached_by_pid`]); otherwise `--sock
/// <sock>`, with ` (pid N)` when the graph entry carried a pid. The graph
/// entry's pid is not the discriminator: every entry records one, including an
/// explicit-socket instance's, which `--pid` cannot dial.
fn running_elsewhere_flags(dir: Option<&Path>, live: &[(u32, String)]) -> String {
    let mut hint =
        String::from(" — nothing is serving this control socket, but aterm IS running: ");
    for (i, (pid, sock)) in live.iter().enumerate() {
        if i > 0 {
            hint.push_str(", ");
        }
        if reached_by_pid(dir, *pid, sock) {
            hint.push_str("--pid ");
            hint.push_str(&pid.to_string());
            hint.push_str(" (");
            hint.push_str(sock);
            hint.push(')');
        } else {
            hint.push_str("--sock ");
            hint.push_str(sock);
            if *pid != 0 {
                hint.push_str(" (pid ");
                hint.push_str(&pid.to_string());
                hint.push(')');
            }
        }
    }
    hint.push_str("; target one with that flag (`aterm ctl instances` lists every live instance)");
    hint
}

/// Whether `--pid <pid>` reaches `sock`: `--pid` dials exactly
/// `<default dir>/aterm-<pid>.sock`, so `sock`'s filename must encode `pid`
/// ([`control_socket::instance_pid`], a cheap reject before any canonicalize)
/// and it must be the same socket as the default dir's: first a component-wise
/// [`Path`] comparison (folding separator spelling), then [`same_socket_path`]
/// (tolerating symlinked ancestors). `--control-sock /elsewhere/aterm-4242.sock`
/// fails the second test. With no default dir, `--sock` is the only remedy.
fn reached_by_pid(dir: Option<&Path>, pid: u32, sock: &str) -> bool {
    let Some(dir) = dir else {
        return false;
    };
    let named = Path::new(sock)
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(control_socket::instance_pid);
    if named != Some(pid) {
        return false;
    }
    let own = dir.join(control_socket::instance_sock_name(pid));
    Path::new(sock) == own.as_path() || same_socket_path(sock, &own.to_string_lossy())
}

/// How to start aterm on this platform: `open -a aterm` for the macOS app
/// bundle; elsewhere the binary is on PATH (Linux `open` is a different
/// program).
#[cfg(target_os = "macos")]
const LAUNCH_REMEDY: &str = "launch aterm.app (`open -a aterm`)";
#[cfg(not(target_os = "macos"))]
const LAUNCH_REMEDY: &str = "start aterm (`aterm`)";

/// The remedy [`connect_error`] appends when nothing serves the socket. A
/// session serves no control socket, so the fix is launching the app
/// ([`LAUNCH_REMEDY`]). A pinned target is not where a new instance listens
/// (`--pid` names the dead pid's socket, `--sock` a path the app does not
/// bind), so retrying the same call cannot reach it: the pin's remedy names
/// where the new pid is listed instead.
fn not_running_hint(origin: TargetOrigin) -> String {
    match origin {
        TargetOrigin::Flagless => format!(
            " — aterm isn't running (nothing is serving this control socket); \
             {LAUNCH_REMEDY} and retry"
        ),
        TargetOrigin::Pinned => format!(
            " — aterm isn't running (nothing is serving this control socket); \
             {LAUNCH_REMEDY}, then `aterm ctl instances` names its pid"
        ),
    }
}

/// The "malformed response header" error for a status line that lacks a
/// parseable count. Renders `status_line` exactly like `{status_line:?}`:
/// `Debug` for `str` quotes the string and escapes every char per
/// [`char::escape_debug`], except that single quotes stay literal.
fn malformed_header_error(status_line: &str) -> io::Error {
    let mut msg = String::from("malformed response header: \"");
    for c in status_line.chars() {
        if c == '\'' {
            msg.push('\'');
        } else {
            msg.extend(c.escape_debug());
        }
    }
    msg.push('"');
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

/// The "oversized reply" error for a reply line that runs past
/// [`MAX_LINE_BYTES`] without a newline. A static message (no inline
/// `format_args!`), like the other hand-composed errors above.
fn oversized_reply_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "oversized reply: control line exceeds the 8 MiB length bound",
    )
}

/// The most continuation lines a refusal may carry past its first; the reply is
/// peer-supplied. The drain checks each buffered line against
/// [`MAX_LINE_BYTES`] before printing it.
const REFUSAL_TAIL_MAX_LINES: usize = 16;

/// Print whatever a refusal wrote after its first line, verbatim and
/// unprefixed (continuations are already indented), bounded by
/// [`REFUSAL_TAIL_MAX_LINES`].
///
/// Issues no read: the server keeps the connection open after a refusal, so
/// reading until EOF would block until the per-op deadline. The tail is taken
/// from what the reader already holds; the refusal and its continuations
/// arrive in one write, buffered by the `fill_buf` that produced the status
/// line. A continuation sent in a later write, or a partial final line, is not
/// printed.
fn drain_refusal_tail<R: Read>(reader: &mut BufReader<R>) -> io::Result<()> {
    let stderr = io::stderr();
    let mut err = stderr.lock();
    for _ in 0..REFUSAL_TAIL_MAX_LINES {
        let buffered = reader.buffer();
        // No complete line in hand — including none at all — ends the drain.
        let Some(nl) = buffered.iter().position(|b| *b == b'\n') else {
            return Ok(());
        };
        // Bounded like every other reply line, and by the same constant.
        if nl >= MAX_LINE_BYTES {
            return Ok(());
        }
        let line = buffered[..nl].to_vec();
        reader.consume(nl + 1);
        // A refusal is a help surface; non-UTF-8 in one is not worth a
        // diagnostic of its own, and the refusal itself is already printed.
        let Ok(text) = std::str::from_utf8(&line) else {
            return Ok(());
        };
        err.write_all(text.trim_end_matches('\r').as_bytes())?;
        err.write_all(b"\n")?;
    }
    Ok(())
}

/// `BufRead::read_line` with the [`MAX_LINE_BYTES`] cap: identical on success
/// (trailing `\n` kept, a partial line at EOF returned, invalid UTF-8 rejected
/// with std's message and `line` untouched), but a line past the cap without a
/// newline fails with [`oversized_reply_error`]. Bytes are UTF-8-checked once
/// at the end, so an over-long line always reports "oversized".
fn read_bounded_line<R: BufRead>(reader: &mut R, line: &mut String) -> io::Result<usize> {
    let mut bytes = Vec::new();
    let n = reader
        .take(MAX_LINE_BYTES as u64 + 1)
        .read_until(b'\n', &mut bytes)?;
    if n > MAX_LINE_BYTES && bytes.last() != Some(&b'\n') {
        return Err(oversized_reply_error());
    }
    match std::str::from_utf8(&bytes) {
        Ok(s) => {
            line.push_str(s);
            Ok(n)
        }
        Err(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "stream did not contain valid UTF-8",
        )),
    }
}

/// Connect to the control socket at `path` and read the token for that
/// connection ([`connect_then_read`]), wrapping a connect failure in
/// [`connect_error`] so the message names the path and, per `origin`, the
/// alternatives. The token is `None` when unreadable (the server then answers
/// `ERR auth`).
fn connect_stream(path: &str, origin: TargetOrigin) -> io::Result<(CtlStream, Option<String>)> {
    connect_then_read(path, read_token_for).map_err(|e| connect_error(path, &e, origin))
}

/// How long `aterm ctl` waits for an instance to accept its connection and
/// start serving it: the connect phase, distinct from a verb's runtime.
///
/// The server never acknowledges `AUTH`, so a connection parked in an undrained
/// listen queue would look like a slow verb for the whole
/// [`EXCHANGE_DEADLINE`]. Every authenticated exchange first sends
/// [`ACCEPT_PROBE`] and waits at most this long for the answer; only then is the
/// request sent. A healthy instance answers in milliseconds. A shorter explicit
/// `--timeout` bounds this phase too; `--timeout 0` disables it.
const ACCEPT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// The accept probe: `version`, which every server answers for any
/// authenticated scope, immediately, on the accepting lane, with one status
/// line. An existing verb keeps the protocol additive both ways. Its reply is
/// consumed and never printed.
const ACCEPT_PROBE: &[u8] = b"version\n";

/// How the connect phase ended ([`open_accepted`]).
enum Acceptance {
    /// Accepted, authenticated and being served: send the request.
    Served(CtlStream),
    /// The connect phase was refused: `ERR auth`, or `ERR control server busy;
    /// retry` when every lane is taken. The request was never sent.
    Refused(String),
    /// Nothing answered within the connect deadline: an undrained listen queue,
    /// or a process not running (stopped, App Nap, saturated machine). The
    /// request was never sent.
    NotAccepted(std::time::Duration),
    /// The server closed or reset the connection without a word, as a
    /// supervised listener does to connections queued on a socket it replaces;
    /// worth one reconnect.
    Dropped,
}

/// Whether an I/O error means the peer dropped the connection (as opposed to a
/// deadline or a local fault).
fn is_peer_drop(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::NotConnected
            | io::ErrorKind::UnexpectedEof
    )
}

/// After a send failed because the server hung up (EPIPE/ECONNRESET), read the
/// status line it may have left: a busy listener writes `ERR control server
/// busy; retry` and closes without reading, so the write fails but the line is
/// the answer.
fn pending_status_line(stream: &CtlStream) -> Option<String> {
    // Darwin answers EINVAL to SO_RCVTIMEO once an AF_UNIX peer has detached,
    // while `recv` still yields the queued bytes and then EOF, so a refused
    // bound means "read without blocking". Windows' `CtlStream` refuses only a
    // zero timeout and never reaches the fallback.
    if stream
        .set_read_timeout(Some(std::time::Duration::from_secs(1)))
        .is_err()
    {
        #[cfg(unix)]
        stream.set_nonblocking(true).ok()?;
        #[cfg(not(unix))]
        return None;
    }
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    match read_bounded_line(&mut reader, &mut line) {
        Ok(n) if n > 0 => Some(line.trim_end_matches(['\r', '\n']).to_string()),
        _ => None,
    }
}

/// What a server that already hung up on the connect phase left behind: its
/// refusal line (`ERR control server busy; retry`, `ERR auth`), or nothing —
/// a drop, which [`connect_served`] retries once. `token_file` is the file
/// whose token was sent, which a refusal of it names.
fn answer_left_behind(stream: &CtlStream, token_file: Option<&Path>) -> Acceptance {
    match pending_status_line(stream) {
        Some(line) => Acceptance::Refused(auth_refusal(&line, token_file)),
        None => Acceptance::Dropped,
    }
}

/// Whether a refused socket timeout means the peer already closed (Darwin's
/// EINVAL once an AF_UNIX peer detached). Only an OS error counts; std's
/// refusal of a zero timeout and the Windows stream's carry no errno.
fn peer_already_closed(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::InvalidInput && e.raw_os_error().is_some()
}

/// Connect to `path` and prove the connection is accepted and served before any
/// request is sent: `AUTH` plus [`ACCEPT_PROBE`], then one reply line within
/// [`ACCEPT_DEADLINE`] (or `deadline`, if shorter). With no token there is
/// nothing to probe and the request goes straight out. On `Served` the stream's
/// timeouts are `deadline`.
///
/// The token is read beside the socket once this connection exists, never
/// earlier and never reused across attempts. An instance writes its token
/// before binding (`provision_token` precedes the bind in `aterm-gui`'s
/// `bind_control_listener`), so a read after the connect names the listener
/// that took it or its replacement; a token read up front could be refused
/// `ERR auth` by an update's successor.
fn open_accepted(
    path: &str,
    origin: TargetOrigin,
    deadline: Option<std::time::Duration>,
) -> io::Result<Acceptance> {
    let (stream, token) =
        connect_then_read(path, read_token_file_at).map_err(|e| connect_error(path, &e, origin))?;
    let token = token.ok();
    // The connect phase's read bound: the probe's patience with a token; with
    // none nothing is probed, and the caller's deadline applies from here.
    let patience = match token {
        Some(_) => deadline.map(|d| d.min(ACCEPT_DEADLINE)),
        None => deadline,
    };
    // A busy listener can accept, answer and close before these run; Darwin then
    // refuses SO_SNDTIMEO/SO_RCVTIMEO with EINVAL. Read the queued answer
    // instead of reporting "Invalid argument".
    if let Err(e) = stream
        .set_write_timeout(deadline)
        .and_then(|()| stream.set_read_timeout(patience))
    {
        if !peer_already_closed(&e) {
            return Err(e);
        }
        return Ok(answer_left_behind(
            &stream,
            token.as_ref().map(|(file, _)| file.as_path()),
        ));
    }
    let Some((file, token)) = &token else {
        return Ok(Acceptance::Served(stream));
    };
    let mut hello = Vec::with_capacity(token.len() + 16);
    hello.extend_from_slice(b"AUTH ");
    hello.extend_from_slice(token.as_bytes());
    hello.push(b'\n');
    hello.extend_from_slice(ACCEPT_PROBE);
    if let Err(e) = (&stream).write_all(&hello).and_then(|()| (&stream).flush()) {
        if !is_peer_drop(&e) {
            return Err(e);
        }
        return Ok(answer_left_behind(&stream, Some(file.as_path())));
    }
    let mut reader = BufReader::new(&stream);
    let mut line = String::new();
    match read_bounded_line(&mut reader, &mut line) {
        Ok(0) => return Ok(Acceptance::Dropped),
        Ok(_) => {}
        Err(e) if is_timeout_error(&e) => {
            return Ok(Acceptance::NotAccepted(patience.unwrap_or(ACCEPT_DEADLINE)));
        }
        Err(e) if is_peer_drop(&e) => return Ok(Acceptance::Dropped),
        Err(e) => return Err(e),
    }
    // The server can have sent nothing past the probe's one line: the request
    // it would answer next has not been written yet.
    if !reader.buffer().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "server answered past the connect probe",
        ));
    }
    drop(reader);
    let line = line.trim_end_matches(['\r', '\n']);
    if is_connection_refusal(line) {
        return Ok(Acceptance::Refused(auth_refusal(
            line,
            Some(file.as_path()),
        )));
    }
    // Any other answer — `OK …`, or an `ERR` a server gives `version` itself —
    // proves the connection was accepted and is being served.
    stream.set_read_timeout(deadline)?;
    Ok(Acceptance::Served(stream))
}

/// The connect phase's refusal as printed: a bare `ERR auth` to a token this
/// client sent gains the file it came from, which the client knows and the
/// server does not. The `ERR auth` head and the `;` after it are what a
/// supervisor keys on; every other line is the server's own.
fn auth_refusal(line: &str, token_file: Option<&Path>) -> String {
    match token_file {
        Some(file) if line == "ERR auth" => format!(
            "ERR auth; the server refused the token in {}; re-run",
            file.display()
        ),
        _ => line.to_string(),
    }
}

/// Whether a connect-phase answer ends the CONNECTION rather than answering the
/// probe: the token was refused (`ERR auth`, after which the server closes), or
/// no lane could take it (`ERR control server busy; retry`).
fn is_connection_refusal(line: &str) -> bool {
    line == "ERR auth" || line.starts_with("ERR control server busy")
}

/// The report for a connection no instance took a request on: nothing answered
/// the connect probe. Both causes look alike from here, so the advice to
/// restart is conditional on the process running (a stopped or napping one
/// needs none).
fn not_accepted_message(path: &str, waited: std::time::Duration) -> String {
    let mut msg = String::from("no answer from ");
    msg.push_str(path);
    msg.push_str(" within ");
    msg.push_str(&waited.as_secs().max(1).to_string());
    msg.push_str(
        "s: the instance took no request, so none was sent — its control listener is not \
         taking connections, or the process is not running (stopped, App Nap, a saturated \
         machine). Retry; if it persists while that aterm is running, restart it.",
    );
    msg
}

/// Run the connect phase ([`open_accepted`]) with one reconnect after a silent
/// drop: a supervised listener resets connections queued on a socket it
/// replaces, and an update's predecessor drops its connections at Commit, so
/// the retry lands on the fresh listener or successor (each attempt reads its
/// own token). A drop means the request was never read, so retrying cannot run
/// it twice. `Ok(Err(code))` is a finished exchange (its report is already on
/// stderr).
fn connect_served(
    path: &str,
    origin: TargetOrigin,
    deadline: Option<std::time::Duration>,
) -> io::Result<Result<CtlStream, ExitCode>> {
    let mut attempt = 0;
    loop {
        attempt += 1;
        match open_accepted(path, origin, deadline)? {
            Acceptance::Served(stream) => return Ok(Ok(stream)),
            Acceptance::Refused(line) => {
                stderr_line(&line)?;
                return Ok(Err(ExitCode::FAILURE));
            }
            Acceptance::NotAccepted(waited) => {
                stderr_line(&not_accepted_message(path, waited))?;
                return Ok(Err(ExitCode::from(EXIT_UNREACHABLE)));
            }
            Acceptance::Dropped if attempt < 2 => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Acceptance::Dropped => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "server closed the connection without responding",
                ));
            }
        }
    }
}

/// Send `bytes` on a served connection. A server that hung up mid-send may have
/// left its reason (such as the busy reply): report that line and fail rather
/// than "Broken pipe". A deadline here is [`Unanswered`], as in
/// [`send_request`].
fn send_served(stream: &CtlStream, bytes: &[u8]) -> io::Result<Option<ExitCode>> {
    match (&*stream)
        .write_all(bytes)
        .and_then(|()| (&*stream).flush())
    {
        Ok(()) => Ok(None),
        Err(e) if is_peer_drop(&e) => match pending_status_line(stream) {
            Some(line) => {
                stderr_line(&line)?;
                Ok(Some(ExitCode::FAILURE))
            }
            None => Err(e),
        },
        Err(e) => Err(unanswered(e)),
    }
}

/// Connect to `path` ([`connect_stream`]) and hold the conversation `talk` has
/// on the connection, given its token, under `deadline`. A connect failure keeps
/// its `connect <path>: …` words; a deadline that fires once connected is
/// [`no_reply_error`]'s line, whose wording relies on `talk` marking its send
/// and reading its status line through [`read_status_line`]. Used by `cast
/// drift`'s client-side fetches and `aterm conn`; [`converse_served`] adds the
/// connect phase for [`exchange`], [`feed_bin_exchange`], `help <verb>` and
/// `cast drift`'s probe.
fn converse<T>(
    path: &str,
    origin: TargetOrigin,
    deadline: Option<std::time::Duration>,
    talk: impl FnOnce(CtlStream, Option<&str>) -> io::Result<T>,
) -> io::Result<T> {
    let (stream, token) = connect_stream(path, origin)?;
    talk(stream, token.as_deref()).map_err(|e| no_reply_error(e, path, deadline))
}

/// [`converse`] behind the connect phase ([`connect_served`]): the connection
/// is proved accepted and served (`AUTH` plus [`ACCEPT_PROBE`] within
/// [`ACCEPT_DEADLINE`]) before `talk` sends a byte, so an undrained queue or a
/// busy refusal is reported in seconds. `Ok(Err(code))` is a connect phase that
/// finished the run (its line already on stderr). Once served, the stream's
/// timeouts are `deadline` and a deadline inside `talk` is [`no_reply_error`]'s
/// line. `talk` sends with [`send_served`] (the token went with the probe) and
/// reads its status line through [`read_status_line`].
fn converse_served<T>(
    path: &str,
    origin: TargetOrigin,
    deadline: Option<std::time::Duration>,
    talk: impl FnOnce(CtlStream) -> io::Result<T>,
) -> io::Result<Result<T, ExitCode>> {
    let stream = match connect_served(path, origin, deadline)? {
        Ok(stream) => stream,
        Err(code) => return Ok(Err(code)),
    };
    talk(stream)
        .map(Ok)
        .map_err(|e| no_reply_error(e, path, deadline))
}

/// Send `AUTH <token>\n` (when a token is readable) and the request line, then
/// flush. A deadline here is [`Unanswered`].
fn send_request(stream: &CtlStream, token: Option<&str>, request: &str) -> io::Result<()> {
    send_request_lines(stream, token, request).map_err(unanswered)
}

fn send_request_lines(
    mut stream: &CtlStream,
    token: Option<&str>,
    request: &str,
) -> io::Result<()> {
    if let Some(token) = token {
        let mut line = Vec::new();
        line.extend_from_slice(b"AUTH ");
        line.extend_from_slice(token.as_bytes());
        line.push(b'\n');
        stream.write_all(&line)?;
    }
    stream.write_all(request.as_bytes())?;
    stream.flush()
}

/// How long to wait for the optional acknowledgement trailer after a guarded
/// artifact response; the server releases its artifact retention only after
/// the client's ACK. Older servers send no trailer and leave the socket open,
/// hence the bound. A new server writes response and challenge in one flush;
/// if a relay delays the trailer past this, the client closes and the server
/// quarantines on the failed ACK.
const ARTIFACT_ACK_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(250);

fn acknowledge_artifact_reply(
    mut stream: &CtlStream,
    reader: &mut BufReader<&CtlStream>,
    verb: &str,
    request: &str,
) -> io::Result<()> {
    if !aterm_types::control_verbs::artifact_reply_requires_ack(verb, request) {
        return Ok(());
    }
    stream.set_read_timeout(Some(ARTIFACT_ACK_PROBE_TIMEOUT))?;
    let mut challenge = String::new();
    match read_bounded_line(reader, &mut challenge) {
        Ok(0) => return Ok(()),
        Ok(_) => {}
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ) =>
        {
            return Ok(());
        }
        Err(error) => return Err(error),
    }
    let challenge = challenge.trim_end_matches(['\r', '\n']);
    let Some(nonce) = challenge
        .strip_prefix(aterm_types::control_verbs::ARTIFACT_REPLY_CHALLENGE_PREFIX)
        .filter(|nonce| aterm_types::control_verbs::valid_artifact_ack_nonce(nonce))
    else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "server sent a malformed artifact acknowledgement challenge",
        ));
    };
    stream.write_all(aterm_types::control_verbs::ARTIFACT_REPLY_ACK_PREFIX.as_bytes())?;
    stream.write_all(nonce.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()
}

/// Write `"aterm-ctl: <msg>\n"` to stderr (manual `eprintln!`). A broken stderr
/// propagates the error instead of panicking. A write that would block is
/// [`Worded`], as [`StdoutSink`]'s is.
fn stderr_line(msg: &str) -> io::Result<()> {
    let stderr = io::stderr();
    let mut err = stderr.lock();
    let mut line = Vec::new();
    line.extend_from_slice(b"aterm-ctl: ");
    line.extend_from_slice(msg.as_bytes());
    line.push(b'\n');
    err.write_all(&line).map_err(worded)
}

/// The stdout handle. SIGPIPE stays ignored (Rust's startup default), so a
/// broken pipe surfaces as an [`io::Error`] through [`StdoutSink`]: a closed
/// reader ends the run silently with status 0, any other failure exits
/// non-zero. `io::stdout` is passed as a function item (see `read_token_for`)
/// so the strict gate treats it as an opaque call.
#[allow(
    clippy::unnecessary_literal_unwrap,
    reason = "io::stdout is passed as a fn-item to a combinator so the strict Trust gate keeps it an opaque cross-crate call instead of inlining std's startup-semantics internals it cannot lower"
)]
fn stdout_handle() -> io::Stdout {
    None::<io::Stdout>.unwrap_or_else(io::stdout)
}

/// Print one line to stdout (manual `println!`); errors propagate.
fn print_stdout_line(line: &str) -> io::Result<()> {
    let stdout = stdout_handle();
    let mut out = StdoutSink(stdout.lock());
    out.write_all(line.as_bytes())?;
    out.write_all(b"\n")
}

/// Whether `verb` streams `<n>` data lines after its `OK <n>` header rather
/// than a single status line, from `aterm_types::control_verbs` (shared with
/// the server; sub-forms such as `image read` and `cast frames` included).
fn streams_payload(verb: &str, request: &str) -> bool {
    // Defined once in `control_verbs::framing_of`, shared with the server.
    aterm_types::control_verbs::framing_of(verb, request)
        == aterm_types::control_verbs::Framing::Lines
}

/// Copy exactly `nbytes` of byte-framed body from `reader` to `out`, verbatim
/// (a `cast` body feeds `asciinema play -`). Returns the bytes copied; fewer
/// than `nbytes` means the server hung up mid-body. The `take` never reads past
/// the body.
fn copy_body<R: io::Read, W: Write>(reader: &mut R, out: &mut W, nbytes: u64) -> io::Result<u64> {
    io::copy(&mut reader.take(nbytes), out)
}

/// Stream exactly `count` payload lines from `reader` to stdout, normalizing
/// line endings while preserving each row's content verbatim. Early EOF is an
/// error so a guarded artifact reply is never acknowledged while truncated.
fn print_payload(reader: &mut BufReader<&CtlStream>, count: usize) -> io::Result<()> {
    let stdout = stdout_handle();
    // `io::Stdout` is always a `LineWriter`, which would issue one `write(2)` per
    // row. The payload is a bounded one-shot body, so buffer it; the explicit
    // `flush()` below keeps broken-pipe/ENOSPC errors propagating. The
    // `subscribe` relay (`redial::relay_subscription`) flushes per frame
    // instead.
    let mut out = StdoutSink(io::BufWriter::new(stdout.lock()));
    for _ in 0..count {
        let mut line = String::new();
        if read_bounded_line(reader, &mut line)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "server hung up before the complete line-framed response",
            ));
        }
        // Normalize the line ending; preserve the row's content verbatim.
        let line = line.strip_suffix('\n').unwrap_or(&line);
        let line = line.strip_suffix('\r').unwrap_or(line);
        out.write_all(line.as_bytes())?;
        out.write_all(b"\n")?;
    }
    out.flush()
}

/// Guarded line replies are `video frames` (at most 64 short path rows). The
/// whole frame is read before the ACK, so acknowledgement never depends on a
/// backpressured stdout; the aggregate cap bounds the buffer against a skewed
/// server.
const MAX_GUARDED_ARTIFACT_LINES: usize = 64;
const MAX_GUARDED_PATH_BYTES: usize = 4 * 1024 * 1024;

fn read_guarded_payload<R: BufRead>(
    reader: &mut R,
    count: usize,
    max_bytes: usize,
) -> io::Result<Vec<String>> {
    if count > MAX_GUARDED_ARTIFACT_LINES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "guarded artifact response exceeds its 64-line bound",
        ));
    }
    let mut total = 0_usize;
    let mut lines = Vec::with_capacity(count);
    for _ in 0..count {
        let mut line = String::new();
        if read_bounded_line(reader, &mut line)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "server hung up before the complete guarded artifact response",
            ));
        }
        total = total.checked_add(line.len()).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "guarded artifact response size overflow",
            )
        })?;
        if total > max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "guarded artifact response exceeds its 4 MiB aggregate bound",
            ));
        }
        if line.ends_with('\n') {
            line.pop();
        }
        if line.ends_with('\r') {
            line.pop();
        }
        lines.push(line);
    }
    Ok(lines)
}

fn print_guarded_payload(lines: &[String]) -> io::Result<()> {
    let stdout = stdout_handle();
    let mut out = StdoutSink(stdout.lock());
    for line in lines {
        out.write_all(line.as_bytes())?;
        out.write_all(b"\n")?;
    }
    Ok(())
}

enum GuardedArtifactOutput {
    Status,
    Lines(Vec<String>),
}

/// Consume a complete guarded frame and acknowledge it before any output, so
/// the server's handoff does not depend on stdout backpressure.
fn receive_guarded_artifact_reply(
    stream: &CtlStream,
    reader: &mut BufReader<&CtlStream>,
    verb: &str,
    request: &str,
    status_line: &str,
    tail: &str,
) -> io::Result<GuardedArtifactOutput> {
    if streams_payload(verb, request) {
        let count = stream_count(tail).ok_or_else(|| malformed_header_error(status_line))?;
        let lines = read_guarded_payload(reader, count, MAX_GUARDED_PATH_BYTES)?;
        acknowledge_artifact_reply(stream, reader, verb, request)?;
        Ok(GuardedArtifactOutput::Lines(lines))
    } else {
        acknowledge_artifact_reply(stream, reader, verb, request)?;
        Ok(GuardedArtifactOutput::Status)
    }
}

/// Connect to `path`, authenticate, send `request`, and print the response for
/// `verb`.
///
/// `frame_request` decides the response framing (`framing_of`): normally the
/// same as `request`, but for `dial <name> <verb...>` the forwarded sub-request,
/// since the reply is the remote's answer to it.
///
/// The server requires `AUTH <hex>\n` first on every connection; the token is
/// read from beside the socket and sent transparently. The `AUTH` line gets no
/// reply, so the first line read answers `request`.
///
/// The conversation runs under `deadline` ([`EXCHANGE_DEADLINE`] by default,
/// `--timeout SECS` otherwise, `None` disables) and every line under
/// [`MAX_LINE_BYTES`]. A deadline once connected is reported by name
/// ([`converse`]'s wording; exit 124).
///
/// `timeout_explicit` matters only for `subscribe`, which watches forever by
/// default and treats an explicit `--timeout` as a wall-clock max
/// ([`redial::relay_subscription`]). `origin` is how `path` was chosen
/// ([`connect_error`]). `redial` is where a successor may answer after a
/// self-update ([`redial`]).
#[allow(
    clippy::too_many_arguments,
    reason = "the request's framing inputs plus the two policies (deadline, redial) it runs under; \
              a wrapper struct would relocate the list, not simplify it"
)]
fn exchange(
    path: &str,
    origin: TargetOrigin,
    request: &str,
    verb: &str,
    frame_request: &str,
    deadline: Option<std::time::Duration>,
    timeout_explicit: bool,
    redial: &redial::Redial,
) -> io::Result<ExitCode> {
    // The `latest` alias is a symlink on Unix (resolved by the kernel) and a
    // pointer file on Windows; resolve it client-side for both.
    let path = &aterm_uds::latest::resolve(path);
    // The connect phase has its own shorter bound ([`ACCEPT_DEADLINE`],
    // [`converse_served`]); once served every operation is bounded by
    // `deadline`. Behind a `dial` the local server only relays, so a deadline
    // names both the remote's silence and the relay's.
    let request_parts: Vec<&str> = request.split_whitespace().collect();
    let remote = redial::dial_name(&request_parts);
    converse_served(path, origin, deadline, |stream| {
        exchange_on(
            stream,
            path,
            request,
            verb,
            frame_request,
            deadline,
            timeout_explicit,
            redial,
        )
        .map_err(|e| match remote {
            Some(name) => no_reply_error_from(e, &remote_words(name, path), false, deadline),
            None => e,
        })
    })
    .map(|served| served.unwrap_or_else(std::convert::identity))
}

/// How a line names the remote a `dial <name>` reached, and the local relay
/// at `path` it went through.
fn remote_words(name: &str, path: &str) -> String {
    format!("the remote '{}' (through {path})", name.escape_debug())
}

/// [`exchange`] once served: `stream` is the accepted, authenticated connection
/// to `path`, its timeouts already `deadline`.
#[allow(
    clippy::too_many_arguments,
    reason = "exchange's own inputs, the connection in place of the origin; a wrapper \
              struct would relocate the list, not simplify it"
)]
fn exchange_on(
    stream: CtlStream,
    path: &str,
    request: &str,
    verb: &str,
    frame_request: &str,
    deadline: Option<std::time::Duration>,
    timeout_explicit: bool,
    redial: &redial::Redial,
) -> io::Result<ExitCode> {
    // Who answered: the server's pid and the listening socket it accepted this
    // connection through, so a hang-up can tell "it ended the exchange" from "it
    // was replaced" (by another process, or the same pid on a fresh listener:
    // [`redial::ListenerId`]).
    let server = redial::server_pid(&stream);
    let listener = redial::listener_at(path);

    // Every socket operation is bounded by `deadline` ([`EXCHANGE_DEADLINE`] by
    // default, clearing the blocking verbs' 600 s server clamp; `--timeout`
    // overrides, 0 => `None`), set by [`converse_served`]. It is measured from
    // the request: a server going away under a blocking read does not extend it
    // (the successor wait and the one retry spend what is left).
    let started = std::time::Instant::now();

    // `&CtlStream` is both `Read` and `Write`: send the request, then read the
    // response through a buffer. A send cut short with a reason left behind is
    // that answer; one cut short silently is a hang-up, judged by the read below.
    let sent = match send_served(&stream, request.as_bytes()) {
        Ok(Some(code)) => return Ok(code),
        Ok(None) => Ok(()),
        Err(e) => Err(e),
    };

    let mut reader = BufReader::new(&stream);
    let mut status_line = String::new();
    let got = sent.and_then(|()| read_status_line(&mut reader, &mut status_line));
    // A blocking read cut before its reply (the server went away under an
    // `await`, `wait` or `text`) is the self-update case: follow the successor
    // once, or say it could not be resumed. Anything else keeps its error.
    let request_parts: Vec<String> = request.split_whitespace().map(String::from).collect();
    // A FORWARD'S HOST THAT WENT AWAY (round eight of the update audit,
    // finding 12): the server dialled relayed this request to the aterm that
    // hosts its session, which went away before it answered, and the relay
    // said so in place of the reply. It is that host's hang-up, never the
    // relay's answer: read as an `ERR`, a `turn` exited 1 — the code callers
    // retry — and a read that could have been asked again was lost.
    let host_gone = match &got {
        Ok(n) if *n > 0 => aterm_types::control_verbs::host_gone(&status_line)
            .map(|(pid, sock)| (pid, sock.to_string())),
        _ => None,
    };
    let hung_up = host_gone.is_some()
        || match &got {
            Ok(0) => true,
            Ok(_) => false,
            Err(e) => redial::is_hangup(e),
        };
    if hung_up && redial::reports_replacement(&request_parts) {
        let until = deadline.map(|d| started + d);
        let again = |next: &str| {
            // The retry gets what the deadline has left, never a fresh one
            // (a set timeout must be non-zero, hence the 1 ms floor: a
            // spent deadline fires at once, as exit 124).
            let left = deadline.map(|d| {
                d.saturating_sub(started.elapsed())
                    .max(std::time::Duration::from_millis(1))
            });
            // The successor's exchange words its own deadline (its socket,
            // what was left); `worded` keeps this one's from re-wording it.
            exchange(
                next,
                TargetOrigin::Pinned,
                request,
                verb,
                frame_request,
                left,
                timeout_explicit,
                &redial::Redial::none(),
            )
            .map_err(worded)
        };
        return match host_gone {
            // The host's successor, where it republishes the session — never
            // the relay, which only forwarded the request.
            Some((pid, sock)) => {
                let host = redial.host(&sock, redial::target_sid(&request_parts));
                answer_replacement(
                    host.after_host_gone(&sock, pid, until),
                    Some(pid),
                    &request_parts,
                    &host,
                    again,
                    None,
                )
            }
            None => replaced_before_reply(
                path,
                server,
                listener,
                &request_parts,
                redial,
                until,
                again,
                got.err(),
            ),
        };
    }
    // Any other request its forward's host went away under: the connection
    // that closed before its reply, in the words a supervisor rides out.
    if let Some((pid, sock)) = host_gone {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!(
                "server closed the connection without responding: the aterm (pid {pid}) \
                 hosting the session went away under the forward to {sock}"
            ),
        ));
    }
    // A `dial` cut before its reply while the local server still serves: that
    // server only relays, so the connection to the remote is what closed.
    if hung_up
        && let Some(note) = redial::remote_closed_note_for(&request_parts, false)
        && redial::still_serving(path, server, listener)
    {
        stderr_line(&note)?;
        return Ok(ExitCode::from(redial::EXIT_REPLACED));
    }
    if got? == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "server closed the connection without responding",
        ));
    }
    let status_line = status_line.trim_end_matches(['\r', '\n']);

    let mut tokens = status_line.splitn(2, ' ');
    let status = tokens.next().unwrap_or("");
    let tail = tokens.next().unwrap_or("");

    if status != "OK" {
        // "ERR <msg>", "ERR", or any unexpected reply: report all of it (e.g.
        // `subscribe`'s refusal spells its vocabulary on a second line) and fail.
        stderr_line(status_line)?;
        drain_refusal_tail(&mut reader)?;
        return Ok(ExitCode::FAILURE);
    }

    // A server-reported timeout (a bare `OK timeout`, or a `turn` verdict with
    // `status=timeout`) prints as usual but exits 124.
    let timed_out = reply_is_timeout(status_line);

    if verb == "subscribe" {
        // `OK subscribe <n>`: the connection is push-only from here. The ack goes
        // to stderr so stdout carries the DELTA/EVENT/GAP/BYTES/MAIL/T frames
        // byte-exact (`cells`/`bytes` frames embed byte bodies).
        stderr_line(status_line)?;
        // A subscription idles between frames, so by default it watches forever
        // (until its sessions exit or the user interrupts; a self-update is
        // followed, and a server gone with no successor exits 75). An explicit
        // `--timeout` bounds the whole relay as a wall-clock max (flush, exit
        // 124); an explicit 0 watches forever.
        let watch = deadline.filter(|_| timeout_explicit);
        let stdout = stdout_handle();
        let mut out = StdoutSink(stdout.lock());
        return redial::relay_subscription(
            redial::FirstLeg {
                stream: &stream,
                reader: &mut reader,
                dialed: path,
                server,
                listener,
            },
            request,
            watch,
            redial,
            &mut out,
        );
    }

    if bytes_payload(verb, frame_request) {
        // `OK <nbytes>` + a byte body (cast/temporal), streamed to stdout through a
        // bounded `take` copy, byte-exact.
        let nbytes = match byte_count(tail) {
            Some(n) => n,
            None => return Err(malformed_header_error(status_line)),
        };
        // `inbox get` may carry `truncated=1 len=<true-size>`: the body is complete
        // as held by this endpoint, not necessarily the whole message. Keep its
        // header on stderr while stdout stays byte-exact.
        if verb == "inbox" {
            stderr_line(status_line)?;
        } else if nbytes == 0 {
            let mut msg = String::from(status_line);
            msg.push_str(" (");
            msg.push_str(verb);
            msg.push_str(": empty body)");
            stderr_line(&msg)?;
        }
        let stdout = stdout_handle();
        let mut out = StdoutSink(stdout.lock());
        let copied = copy_body(&mut reader, &mut out, nbytes as u64)?;
        if copied < nbytes as u64 {
            // The header promised more than arrived: keep what was written but
            // fail, so a truncated recording is never taken for complete.
            let mut msg = String::from("server hung up mid-body (got ");
            msg.push_str(&copied.to_string());
            msg.push_str(" of ");
            msg.push_str(&nbytes.to_string());
            msg.push_str(" bytes)");
            drop(out);
            stderr_line(&msg)?;
            return Ok(ExitCode::FAILURE);
        }
        drop(out);
        acknowledge_artifact_reply(&stream, &mut reader, verb, frame_request)?;
        return Ok(ExitCode::SUCCESS);
    }

    // A guarded response is consumed from the socket, acknowledged, and only
    // then copied to stdout, so a blocked stdout consumer cannot exhaust the
    // server's ACK window.
    if aterm_types::control_verbs::artifact_reply_requires_ack(verb, frame_request) {
        match receive_guarded_artifact_reply(
            &stream,
            &mut reader,
            verb,
            frame_request,
            status_line,
            tail,
        )? {
            GuardedArtifactOutput::Lines(lines) => {
                if lines.is_empty() {
                    let mut msg = String::from(status_line);
                    msg.push_str(" (");
                    msg.push_str(verb);
                    msg.push_str(": no results)");
                    stderr_line(&msg)?;
                }
                print_guarded_payload(&lines)?;
            }
            GuardedArtifactOutput::Status => print_stdout_line(status_line)?,
        }
        return if timed_out {
            Ok(ExitCode::from(EXIT_TIMEOUT))
        } else {
            Ok(ExitCode::SUCCESS)
        };
    }

    if streams_payload(verb, frame_request) {
        let count: usize = match stream_count(tail) {
            Some(count) => count,
            None => return Err(malformed_header_error(status_line)),
        };
        // `search` appends ` incomplete` after the count when the list is not
        // exhaustive, then the cause unless it is the depth cap; surface it so
        // callers know which results they did not get.
        if let Some(note) = search_incomplete_note(tail) {
            stderr_line(note)?;
        }
        // `turn` carries its verdict after the count (`turn submitted=… status=…
        // seq=…`): stdout stays screen rows, the verdict goes to stderr.
        if tail.split_whitespace().nth(1) == Some("turn") {
            let verdict: Vec<&str> = tail.split_whitespace().skip(1).collect();
            stderr_line(&verdict.join(" "))?;
        }
        // A zero-count reply would be silent on stdout, so the OK count goes to
        // stderr ("worked, no results"). Some headers carry facts the rows need
        // and always go to stderr: `inbox` (hold/seen/dropped/pending),
        // `offscreen` (`last=` is the next `since=`, `screen_rows=` splits archive
        // from screen, `lost=`/`breaks=`; `aterm drive report` reads it here) and
        // `resizes` (`render=`, `flaps=`, `unledgered=`, `lost=`).
        if drift::is_drift_request(verb, frame_request) {
            // Every `cast drift` is dispatched to the `drift` module before any
            // exchange; this is a backstop. A server predating the verb answers
            // the bare cast (`OK <nbytes>` + body, no `verdict=`): read it off
            // and refuse by name rather than read a byte body as lines.
            if !tail.split_whitespace().any(|t| t.starts_with("verdict=")) {
                if let (1, Some(n)) = (tail.split_whitespace().count(), byte_count(tail)) {
                    io::copy(&mut (&mut reader).take(n as u64), &mut io::sink())?;
                }
                return Err(io::Error::other(
                    "the aterm this `dial` reached predates `cast drift` (it answered the bare \
                     cast, not a report); ask it with a newer `aterm ctl` on its own machine, or \
                     save its `cast` and `text` and run `aterm ctl cast drift --file <cast> \
                     --screen <text>`",
                ));
            }
            // `cast drift`'s header IS the verdict (`verdict=`, `culprit=`, …): it
            // leads its rows on stdout, as the `drift` module prints it.
            print_stdout_line(status_line)?;
        } else if verb == "inbox"
            || verb == "offscreen"
            || verb == "resizes"
            // `history`'s header carries ` unheld_below=<id>` when an update could
            // not carry the ledger whole.
            || (verb == "history" && tail.split_whitespace().nth(1).is_some())
        {
            stderr_line(status_line)?;
        } else if count == 0 {
            let mut msg = String::from(status_line);
            msg.push_str(" (");
            msg.push_str(verb);
            msg.push_str(": no results)");
            stderr_line(&msg)?;
        }
        print_payload(&mut reader, count)?;
    } else {
        print_stdout_line(status_line)?;
    }

    acknowledge_artifact_reply(&stream, &mut reader, verb, frame_request)?;
    if timed_out {
        Ok(ExitCode::from(EXIT_TIMEOUT))
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

/// A blocking read (`await`, `wait`, `ready`, `text`, `inbox`) whose server hung
/// up before replying, ordinarily an aterm self-update. A successor answering
/// where this call follows ([`redial::Redial::after_hangup`]) is asked once
/// more through `again`, unless the request carries an old-process anchor
/// ([`redial::retries_across_replacement`]), which ends with a note to
/// re-issue it. No successor: [`redial::EXIT_REPLACED`]. The same server still
/// serving, or a platform that cannot tell: the original error. `until` caps
/// the successor wait at the call's deadline.
#[allow(
    clippy::too_many_arguments,
    reason = "the server the exchange reached (where, which pid, which listener), the request, \
              the redial policy and deadline, the retry and the original error; a wrapper struct \
              would relocate the list, not simplify it"
)]
fn replaced_before_reply(
    dialed: &str,
    server: Option<u32>,
    listener: Option<redial::ListenerId>,
    request_parts: &[String],
    redial: &redial::Redial,
    until: Option<std::time::Instant>,
    again: impl FnOnce(&str) -> io::Result<ExitCode>,
    error: Option<io::Error>,
) -> io::Result<ExitCode> {
    answer_replacement(
        redial.after_hangup(dialed, server, listener, until),
        server,
        request_parts,
        redial,
        again,
        error,
    )
}

/// What a request cut before its reply comes to, once the hang-up of `server`
/// is classified ([`redial::Redial::after_hangup`], or
/// [`redial::Redial::after_host_gone`] behind a forward): asked once of a
/// successor through `again`, or ended with [`redial::EXIT_REPLACED`] and a
/// note, or — the same server still serving, or a platform that cannot tell —
/// the original `error`.
fn answer_replacement(
    hangup: redial::Hangup,
    server: Option<u32>,
    request_parts: &[String],
    redial: &redial::Redial,
    again: impl FnOnce(&str) -> io::Result<ExitCode>,
    error: Option<io::Error>,
) -> io::Result<ExitCode> {
    let original = || {
        error.unwrap_or_else(|| {
            io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "server closed the connection without responding",
            )
        })
    };
    match hangup {
        redial::Hangup::StillServing | redial::Hangup::Unknown => Err(original()),
        redial::Hangup::Gone {
            waited,
            by_deadline,
        } => {
            stderr_line(&redial::gone_note(
                server,
                waited,
                by_deadline,
                redial.unfollowed(),
            ))?;
            Ok(ExitCode::from(redial::EXIT_REPLACED))
        }
        redial::Hangup::Successor { path, pid } => {
            let old = server.unwrap_or(0);
            if redial::retries_across_replacement(request_parts) {
                stderr_line(&format!(
                    "{} before it answered; asking its successor once",
                    redial::replaced_words(old, pid, "")
                ))?;
                again(&path)
            } else {
                stderr_line(&not_asked_again_line(old, pid, request_parts))?;
                Ok(ExitCode::from(redial::EXIT_REPLACED))
            }
        }
    }
}

/// The whole line for a request cut by a replacement and not asked of its
/// successor (pid `pid`, replacing `old`): the replacement, the reason and what
/// to do instead ([`redial::not_asked_again_note`]), and the exit code.
fn not_asked_again_line(old: u32, pid: u32, request_parts: &[String]) -> String {
    format!(
        "{} before it answered; {} (exit {})",
        redial::replaced_words(old, pid, ""),
        redial::not_asked_again_note(request_parts),
        redial::EXIT_REPLACED
    )
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "lib_foreign_facts_tests.rs"]
mod foreign_facts_tests;
