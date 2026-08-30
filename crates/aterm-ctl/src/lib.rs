// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-ctl` — a tiny, dependency-free client for the aterm introspection
//! control socket (protocol v1).
//!
//! The aterm engine optionally exposes a Unix-domain control socket whose
//! protocol is newline-delimited text: the client sends one request line
//! (`"VERB [args...]\n"`) and reads exactly one response. This binary frames a
//! single request from the command line, prints the relevant part of the
//! response to stdout, and maps the protocol's `OK`/`ERR` outcome onto the
//! process exit status.
//!
//! It uses only `std` plus two std-only workspace crates: `aterm-uds` (the
//! portable control-socket stream — std Unix-domain sockets on Unix, AF_UNIX
//! over winsock on Windows, where the `aterm.sock` `latest` alias is a
//! pointer FILE this client resolves before connecting) and the platform-free
//! `aterm-types` engine crate, which carries the socket naming/discovery
//! decisions shared with the server; there are no external dependencies.
//!
//! # Usage
//!
//! ```text
//! aterm-ctl [--sock PATH | --pid PID] <verb> [args...]
//! ```
//!
//! The socket path is resolved as: `--sock PATH` if given, else `--pid PID`
//! (a specific instance's `<dir>/aterm-<PID>.sock`), else the
//! `$ATERM_CONTROL_SOCK` environment variable (whose `0`/`off` disable
//! keywords are honoured as on the server), else — INSIDE an aterm session —
//! the caller's OWN instance, located through `$ATERM_PARENT_SESSION_ID`'s
//! discovery graph entry (`<dir>/graph/<sid>`, which records the hosting
//! instance's socket; so a flagless call from within aterm always reaches the
//! instance that hosts the calling terminal, even when a NEWER instance owns
//! the `latest` symlink), else the per-user default `<dir>/aterm.sock` where
//! `<dir>` is `$XDG_RUNTIME_DIR/aterm` (when set) or
//! `~/Library/Application Support/aterm` (macOS). The default is a symlink
//! the server atomically points at the newest instance's `aterm-<pid>.sock`,
//! so the flagless flow reaches a live instance. This matches the server's
//! resolution exactly, plus the in-session self-location step.
//!
//! ## Authentication (transparent)
//!
//! The server is access-controlled by default: it accepts only same-uid peers
//! and requires a per-launch capability token. This client reads that token
//! from the socket's sibling token file — the matching `aterm-<pid>.token`
//! for a per-instance socket (resolved through the `latest` symlink), else the
//! socket's OWN `<name>.token` (so two private instances in one directory keep
//! their own credentials) — and sends `AUTH <hex>\n` as the FIRST line of
//! every connection, before the verb. Normal same-user usage is therefore unchanged
//! — there is no flag and no prompt. If the token file is unreadable
//! (different user, or aterm not running) the connection is refused by the
//! server with `ERR auth`.
//!
//! ## Verbs
//!
//! * `text`            — print the visible screen, one row per line.
//! * `cursor`          — print `OK <row> <col> <visible> <style>` (`<style>`
//!   is the DECSCUSR style, lowercase: `blinking_block`, `steady_block`,
//!   `blinking_underline`, `steady_underline`, `blinking_bar`, `steady_bar`,
//!   `hidden`, `hollow_block`).
//! * `cell <r> <c>`    — print `OK <codepoint> <fg> <bg>`.
//! * `search <pat>`    — print one `"<row> <col> <len>"` line per match. A hit
//!   that straddles a SOFT WRAP is one match, reported at the row and column it
//!   starts on, with `col + len` running past the grid width — the overflow
//!   continues at column 0 of the following (wrapped) row.
//! * `send <text>`     — write `<text>` to the PTY (trailing literal `\n` ⇒ CR).
//! * `key <name>`      — send a named key (`enter`, `tab`, `up`, …) to the PTY.
//! * `image [path]`    — render aterm's own client frame to a PNG, including
//!   compiled native-app surfaces; print `OK <w> <h> <path>`. It yields the
//!   current application-render artifact — a re-render through the same
//!   renderer and composition rules the application present uses. So it
//!   has no native OS chrome, works headless, and never needs OS
//!   screen-capture permission. It includes the cursor blink phase /
//!   unfocused-hollow override (headless sessions are deterministic); use `cursor`
//!   for phase-independent state.
//! * `window [<target>] [path]` — capture a full-window artifact to a PNG; print
//!   `OK <w> <h> <path>`. `<target>` selects which window: omitted/`front` = the
//!   front terminal window WITH native platform chrome (titlebar, traffic lights,
//!   unified toolbar, full-width tab strip) AND the terminal content;
//!   `prefs`/`settings` = the native Settings tab in that same window. How the
//!   chrome is obtained differs by platform: macOS photographs the real composited
//!   window via CoreGraphics, while Windows stitches chrome around a `PrintWindow`
//!   client capture taken from the exact client destination from a successful
//!   application present. Either way it does not observe compositor selection,
//!   colour management, scanout, occlusion, or photons. A first token that is a known
//!   target keyword always selects that window — to write to a file literally named
//!   `prefs`/`front`, give a target first (e.g. `window front prefs`). It requires
//!   an attached OS window; macOS also needs Screen Recording permission (a clear
//!   `ERR` explains how to grant it if missing). A missing target, headless
//!   instance, or unsupported platform gets a clear `ERR`.
//! * `controls <target>` — dump a GUI surface's controls as text. For
//!   `prefs`/`settings`, compatibility `field key=… label=… value=… effective=…`
//!   rows describe only setting controls on the current native route; the following
//!   canonical `ui …` rows serialize that route's full compiled semantic tree. A
//!   closed Settings tab reports zero controls instead of fabricating an off-screen
//!   catalog. This is the analogue of `chrome` for app surfaces and works HEADLESS
//!   (built from the live model, with no screenshot or Screen Recording grant needed).
//!   `"OK <n>\n"` + `<n>` lines.
//! * `open <target>`   — open an own-rendered surface; `prefs`/`settings` opens the
//!   native Settings tab. This lets a driver introspect a closed surface: `open prefs`
//!   then `window prefs` / `controls prefs`. These targets work HEADLESS too because
//!   their native tab-app trees compile into the virtual frame `image` reads.
//! * `resize <r> <c>`  — resize the engine + PTY (each dimension 1..=4096;
//!   out-of-range requests get `ERR out of range`).
//! * `select <r1> <c1> <r2> <c2>` — select from cell `(r1,c1)` to `(r2,c2)`,
//!   both endpoint cells inclusive (live-screen coords; negative rows reach
//!   into scrollback). `select clear` clears the selection.
//! * `select word <r> <c>` — word-select the cell via the engine's builtin
//!   smart-selection rules (URLs/paths/words; a whitespace cell selects just
//!   itself) — the double-click gesture.
//! * `select line <r>` — select the full line of row `r` (triple-click).
//! * `select block <r1> <c1> <r2> <c2>` — rectangular selection, the two
//!   cells as inclusive corners (alt-drag).
//! * `select extend <r> <c>` — extend the existing selection so `(r,c)` is
//!   its new inclusive endpoint (shift-click); errors with no selection.
//! * `selection`       — print the selected text, one line per selected row.
//! * `copy`            — copy the selection to the system clipboard
//!   (`pbcopy`); print `OK <byte-count>` (`OK 0` when nothing is selected).
//! * `tab <new|N|next|prev>` — drive the FRONT window's tabs: `new` opens a tab,
//!   `<N>` (0-based) selects tab N, `next`/`prev` cycle. Prints
//!   `OK <active_index> <tab_count>`. Drives the native macOS tab switcher.
//!
//! ## Cross-terminal verbs (client-side)
//!
//! The discovery verbs are answered by THIS CLIENT (no single server owns the answer):
//!
//! * `instances`       — one line per LIVE same-user aterm instance:
//!   `<pid> <sessions> <sock_path>[ self]` (`self` marks the instance hosting
//!   the calling terminal). Discovers `<dir>/aterm-<pid>.sock` files and probes
//!   each with its own token.
//! * `ls`              — every session of every live instance, one line each:
//!   `<pid> <local> <sid> <parent|-> <state> <title> meta=<0|1> window=<id|none|->
//!   active=<0|1|-> wfocus=<0|1|-> detail=<pct|->[ *]` (the server's
//!   `sessions` line prefixed with the instance pid; `*` marks the calling
//!   terminal's own session, from `$ATERM_PARENT_SESSION_ID`; the title is
//!   pct-encoded and often mirrors the cwd via shell integration). The
//!   one-shot peer-discovery view an agent uses to pick which terminal to
//!   drive; combine with `aterm-ctl "@<sid>" <verb>` to drive a session in
//!   ANY instance — the server relays an unknown-but-published sid to its
//!   hosting sibling.
//! * `windows`         — one line per WINDOW across the fleet, folded from the
//!   same `sessions` lines: `<pid> window=<id> focused=<0|1> sessions=<n>
//!   active=<sid>[,<sid>…]`, then `window=none` / `window=-` rows for the
//!   sessions no window holds / the instances that could not say. A headless
//!   instance folds under its one logical window 0. Like `ls` and `instances`
//!   it takes no argument: a trailing word is `ERR usage: <verb>` (exit 1).
//!
//!   `ls`/`instances`/`windows` are answered CLIENT-side, but they still honour
//!   `--sock`/`--pid`, which SCOPE the listing to the addressed instance. That
//!   matters for isolation: an automated caller that launched its own instance
//!   under a private `$ATERM_CONTROL_SOCK` gets back only that instance, never
//!   the user's real terminals. A scoped listing that finds nothing says so
//!   distinctly instead of degrading to "no live instances".
//!
//! For `text`, `search`, `modes`, `selection`, `chrome`, and `controls` the
//! response is `"OK <n>\n"` followed by `<n>` data lines, and those data lines are
//! what gets printed.
//! For every other verb the single `OK …` status line itself is printed. An
//! `ERR …` response is written to stderr and yields exit code 1; so does any
//! connection failure.

use std::collections::HashSet;
use std::env;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use aterm_types::control_socket::{self, SocketDirective};
use aterm_uds::CtlStream;

mod conn;
pub use conn::conn_main_entry;

/// Usage synopsis — the one-line invocation shape, shared by `--help` and the
/// "no verb given" usage error so they never drift apart.
const SYNOPSIS: &str = "aterm-ctl [--sock PATH | --pid PID] [--timeout SECS] <verb> [args...]";

/// The hand-written PROSE of `--help`: synopsis, options, exit codes, the
/// stdin-payload forms, the socket-resolution rule, and the cross-terminal note.
/// The VERBS reference is deliberately NOT here — [`help_text`] appends it,
/// GENERATED from [`aterm_types::control_verbs::catalog_lines`] (the very table
/// the server answers its `help` verb from), so the CLI's verb list can never
/// drift from the protocol this build actually speaks. Ends with the `VERBS:`
/// header line so the generated catalog slots directly beneath it.
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

STDIN PAYLOADS (multi-line, whitespace-preserving; each <= 256 KiB):
    feed-bin      read the raw binary payload from STDIN and feed it to the PTY
                  VERBATIM via a length-prefixed frame; print the server's
                  OK <n> bytes. (No inline length is accepted.)
    send --stdin | send -
                  send the EXACT STDIN bytes RAW — the same verbatim frame as
                  feed-bin — bypassing the argv newline guard the inline
                  `send <text>` form keeps.
    paste --stdin | paste -
                  paste the STDIN bytes through the server's PASTE seam
                  (control-byte sanitize, and ESC[200~/201~ bracketing when the
                  app enabled DECSET 2004) — a multi-line paste with real paste
                  semantics, NOT the raw byte feed of send/feed-bin.

SOCKET RESOLUTION:
    The socket path is resolved as: --sock PATH if given, else --pid PID
    (a specific instance's <dir>/aterm-<PID>.sock), else the
    $ATERM_CONTROL_SOCK environment variable (whose 0/off disable keywords
    are honoured as on the server), else — INSIDE an aterm session — the
    instance hosting the calling terminal (located via
    $ATERM_PARENT_SESSION_ID's <dir>/graph/<sid> entry, so a flagless call
    always drives YOUR terminal's instance even when a newer instance owns
    the symlink), else the per-user default <dir>/aterm.sock where <dir> is
    $XDG_RUNTIME_DIR/aterm (when set) or ~/Library/Application Support/aterm
    (macOS). The default is a symlink the server atomically points at the
    newest instance's aterm-<pid>.sock, so the flagless flow reaches a live
    instance.

    DISCOVERY (`ls`, `instances`, `windows`): the client enumerates <dir>/aterm-<pid>.sock
    (plus the <dir>/graph entries of explicit-$ATERM_CONTROL_SOCK instances)
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
      EVENT <local> <kind> ...   a lifecycle digest line: `turn <id> submitted=
                                 status= dur_ms=`, `block-complete <id> exit=<n>`,
                                 or `exited`.
      BYTES <local> <len>        then <len> RAW PTY bytes + a trailing newline.
      GAP <local> ...            a discontinuity marker: `bytes-dropped=<n>`
                                 (queue overflow) or `resync=<seq>` (engine reset —
                                 state was dropped; treat the next DELTA as a fresh
                                 snapshot / re-read with `screen`).

VERBS (generated from the protocol verb table — always current, cannot drift):
";

/// The full `--help` text: the hand-written [`HELP_PROSE`] followed by the VERBS
/// catalog generated from the protocol table via
/// [`aterm_types::control_verbs::catalog_lines`], so the verb list is always
/// EXACTLY what this build speaks (there is no hand-maintained copy left to drift
/// out of sync). Built with `push_str` — the strict Trust gate never sees an
/// inline `format_args!` here (the per-line `format!` lives behind the opaque
/// cross-crate `catalog_lines` call).
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

/// The CLIENT-answered discovery verbs, documented in a FIXED block appended
/// to `--help` AFTER the generated protocol catalog. `ls`, `instances` and `windows` are
/// intercepted by aterm-ctl itself (no server round-trip), so they are NOT in the
/// protocol verb table [`aterm_types::control_verbs::catalog_lines`] renders and
/// would otherwise be invisible to an AI reading `--help`. Hand-written (they are
/// hand-implemented in this same file, in `run_discovery`, so this cannot drift).
/// Also the source `help <client verb>` is answered from ([`client_help_reply`]),
/// entry by entry — so an entry here is a verb line at the 4-column indent plus
/// continuation lines at the 18-column gutter, and nothing else in between.
const CLIENT_VERBS: &str = "\
\n\
CLIENT VERBS (answered by aterm-ctl itself, no server round-trip):
    ls            every session of every live instance, one per line:
                  <pid> <local> <sid> <parent|-> <state> <title> meta=<0|1>
                  window=<id|none|-> active=<0|1|-> wfocus=<0|1|-> detail=<pct|->[ *]
                  (* = the calling terminal's own session; window= is the
                  hosting window — a headless instance reports its one logical
                  window 0; none = a session no window holds; - = the instance
                  could not ask its main thread; active= the session is on that
                  window's active tab; wfocus= that window is aterm's most
                  recently focused one (unchanged by a minimize or by the app
                  deactivating); detail= the sanitized running command, never
                  its arguments — e.g. claude, codex, targo%20test — so read it
                  before typing into a peer).
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
    scoped listing that finds nothing reports that distinctly (exit 1).
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
                  that sentence; ATERM_MUX=0 disables the whole check).
";

/// The verbs a completion script offers as the FIRST argument: every protocol
/// verb from the shared table (`aterm_types::control_verbs::VERBS`) PLUS the
/// CLIENT-answered discovery verbs (`ls`/`instances`/`windows`), which aterm-ctl intercepts
/// itself ([`run_discovery`]) and are therefore ABSENT from the protocol table.
/// Space-joined in table order. Sourced from the table so the completion list can
/// never drift from the protocol this build speaks — the single-source discipline
/// [`help_text`] also follows.
fn completion_verb_list() -> String {
    let mut s = String::new();
    for spec in aterm_types::control_verbs::VERBS {
        s.push_str(spec.name);
        s.push(' ');
    }
    // The client-only verbs, answered here and never framed on the socket
    // (`run_discovery`, `run_mux_report`), so the protocol table above does not
    // carry them.
    s.push_str("ls instances windows mux");
    s
}

/// The global flags a completion script offers — exactly the ones parsed BEFORE
/// the verb in [`real_main`]. Per-verb argument completion is deliberately out of
/// scope: completing the verb name is the bulk of the value.
const COMPLETION_FLAGS: &str = "--sock --pid --timeout --help --version";

/// The completion script for `shell` (`bash`/`zsh`/`fish`), or `None` for an
/// unknown shell name. Each script offers [`completion_verb_list`] as the first
/// argument plus [`COMPLETION_FLAGS`]. Built with `push_str` (no inline
/// `format_args!`), like every other user-visible string in this binary.
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
    let mut out = stdout.lock();
    out.write_all(script.as_bytes())?;
    Ok(ExitCode::SUCCESS)
}

/// The clear error for `--completions <shell>` with an unrecognized shell name. A
/// static message, like the other hand-composed client errors (the strict Trust
/// gate cannot lower inline `format_args!`). Exits FAILURE (nonzero).
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

/// The zsh `_arguments` + verb-`compadd` body for the CTL surface — rendered by
/// BOTH the `aterm-ctl` compdef ([`zsh_completion`]) and the `aterm ctl`
/// delegation arm of the front-door compdef ([`front_door_completion_script`]),
/// so the two scripts describe the same ctl flags and can never drift. `indent`
/// prefixes every line (the delegation arm nests inside an `if`); `array` names
/// the zsh array holding the ctl verb set.
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

/// The ctl flag lines for fish — ONE renderer for the `aterm-ctl` script and
/// the front door's `aterm ctl` arm ([`front_door_completion_script`]), so the
/// flag/description text cannot drift between the two. `preamble` carries the
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

/// The FRONT DOOR's completion script for `shell` (`bash`/`zsh`/`fish`), or
/// `None` for an unknown shell name. The installed command is `aterm` — the
/// installer strips the `aterm-ctl` sibling off `PATH`, so a completion that
/// only knows the sibling completes a command nobody has. `verbs`/`flags` are
/// `aterm`'s OWN first-position surface, supplied by the `aterm` crate (whose
/// routing tables they must mirror — pinned by that crate's tests). The
/// `aterm ctl <TAB>` delegation arm renders from THIS crate's
/// [`completion_verb_list`] / [`COMPLETION_FLAGS`] — the same source the
/// `aterm-ctl` scripts render — so the ctl verb set cannot drift between the
/// sibling script and the front door's.
///
/// CONTRACT (install.sh): the zsh script's FIRST line is `#compdef aterm`;
/// bash and fish follow the same conventions as the `aterm-ctl` scripts.
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

/// The front-door bash completion: `_aterm` offers the front-door verb set at
/// the first position, the front-door flags for a `-`-prefixed word, and — past
/// a first-position `ctl` — the CTL verb/flag sets, exactly the pair the
/// `aterm-ctl` script offers.
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

/// Space-join the flag NAMES out of `(flag, description)` pairs (bash offers
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

/// The front-door zsh completion: `#compdef aterm` (the first line is the
/// install.sh contract), the front-door verb/flag sets via `_arguments`, and a
/// first-word `ctl` dispatch that shifts onto the CTL surface — the SAME
/// `_arguments` body [`zsh_completion`] renders, via [`push_zsh_ctl_arguments`].
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
/// token (`__fish_use_subcommand`), plus the CTL verb/flag sets gated behind a
/// seen `ctl` — the flag lines through [`push_fish_ctl_flags`], the same
/// renderer the `aterm-ctl` script uses.
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
    // The ONE binary also fronts `aterm conn` (the session-connections CLI,
    // SESSION_CONNECTIONS.md §6.1), implemented in this crate — so its subverbs
    // complete here, gated behind a seen `conn`, exactly as `ctl`'s do.
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

/// The front door's `--completions <shell>` as a callable: print the `aterm`
/// completion script to stdout, or a clear error to stderr (FAILURE) for a
/// missing or unknown shell name — the same errors the sibling `aterm-ctl`
/// flag reports. `verbs`/`flags` as in [`front_door_completion_script`]; the
/// ONE `aterm` binary supplies them from its own routing tables.
pub fn front_door_completions_entry(
    shell: Option<&str>,
    verbs: &[&str],
    flags: &[(&str, &str)],
) -> ExitCode {
    match front_door_completions_result(shell, verbs, flags) {
        Ok(code) => code,
        Err(e) => {
            // Manual form of `eprintln!("aterm: {e}")` (the strict Trust gate
            // cannot lower inline `format_args!`); a failed diagnostic write is
            // ignored — the process is already exiting FAILURE.
            let stderr = io::stderr();
            let mut err = stderr.lock();
            let _ = err.write_all(b"aterm: ");
            let _ = err.write_all(e.to_string().as_bytes());
            let _ = err.write_all(b"\n");
            ExitCode::FAILURE
        }
    }
}

/// [`front_door_completions_entry`]'s fallible core, mirroring
/// [`emit_completions`]: a missing shell name and an unknown one are the same
/// two clear errors the `aterm-ctl --completions` path raises.
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
    let script =
        front_door_completion_script(shell, verbs, flags).ok_or_else(unknown_shell_error)?;
    let stdout = stdout_handle();
    let mut out = stdout.lock();
    out.write_all(script.as_bytes())?;
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// THE FRONT DOOR'S SINGLE-INSTANCE FORWARD (S12 / design §5)
// ---------------------------------------------------------------------------
//
// `aterm new-tab` under `windowing_behavior = "attach"` has to answer two
// questions this crate already answers for its own verbs: *which* instance, and
// *is it alive*. Both are re-exported here rather than reimplemented in the
// `aterm` crate, because a second copy of the resolution rule is exactly how a
// front door ends up driving a different terminal than `aterm ctl` does.
//
// Why not just call [`main_entry`] with `["spawn", "cwd=…"]`? Because the front
// door needs three things that client does not offer: a reachability probe
// BEFORE it commits to a route (the routing decision is a pure function of it),
// silence on success (`wt new-tab` prints nothing; `OK s-1a2b` on a shell prompt
// is noise), and the server's own `ERR` text back as a value so the caller can
// decide between reporting it and falling back. Everything below is a thin
// composition of the same helpers `real_main` uses.

/// The control socket of the instance a front-door request should be handed to,
/// or `None` when nothing is reachable.
///
/// Resolution is the flagless `aterm-ctl` rule, unchanged and deterministic:
/// `$ATERM_CONTROL_SOCK` when explicit (and `None` outright when it disables the
/// socket), otherwise the instance HOSTING the calling terminal when the caller
/// is inside an aterm session, otherwise the `latest` pointer at the newest
/// instance. Then a LIVENESS probe — a connect, not an existence check, because
/// a crashed instance leaves its socket file behind and `attach` must not route
/// a tab into a corpse.
#[must_use]
pub fn front_door_instance() -> Option<String> {
    let path = resolve_path(
        None,
        None,
        env::var(SOCK_ENV).ok(),
        env::var(NO_SOCK_ENV).ok(),
        env::var(SELF_SID_ENV).ok(),
    )
    .ok()?;
    front_door_probe(&path)
}

/// The liveness probe behind [`front_door_instance`], and the alias step it
/// cannot skip: the returned path is the RESOLVED one, so the caller forwards to
/// exactly the instance that answered.
///
/// WINDOWS: the flagless `aterm.sock` is a pointer FILE, not a socket
/// (`aterm_uds::latest`), so connecting to it directly ALWAYS fails — and since
/// this probe is the whole input to `route_launch`'s `instance_reachable`, the
/// entire `attach` lane was dead there: `aterm new-tab` opened a new window
/// every time, no matter what `windowing_behavior` said, and the jump list's
/// New Tab task (which is gated on `attach` being configured) with it. Every
/// other client path in this crate already resolves the alias before dialing
/// (`connect_stream`); this one did not. Unix is unaffected — the kernel
/// resolves the symlink during `connect`, and `latest::resolve` is the identity
/// there.
#[must_use]
fn front_door_probe(path: &str) -> Option<String> {
    let resolved = aterm_uds::latest::resolve(path);
    CtlStream::connect(&resolved).is_ok().then_some(resolved)
}

/// Hand one request line to the instance at `path` and return its FIRST reply
/// line, trimmed (`OK …` or `ERR …`).
///
/// `request` must already end in `\n` and contain no other line terminator —
/// the caller frames it (`WindowRequest::control_request` refuses a cwd that
/// cannot be framed). Authentication is the same transparent `AUTH <token>`
/// line every other client path sends, read from the token file beside the
/// socket.
///
/// A short deadline, not the client's 900 s default: a front-door forward is a
/// launch, and a launch that hangs is worse than a launch that starts its own
/// window. Ten seconds is generous for a main-thread hop and still bounded well
/// under any human's patience.
pub fn front_door_send(path: &str, request: &str) -> io::Result<String> {
    const FORWARD_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);
    let stream = connect_stream(path)?;
    stream.set_read_timeout(Some(FORWARD_DEADLINE))?;
    stream.set_write_timeout(Some(FORWARD_DEADLINE))?;
    allow_foreground_handoff(path);
    send_request(&stream, read_token_for(path).as_deref(), request)?;
    let mut reader = BufReader::new(&stream);
    let mut reply = String::new();
    if read_bounded_line(&mut reader, &mut reply)? == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "the running aterm closed the connection without replying",
        ));
    }
    Ok(reply.trim_end_matches(['\r', '\n']).to_string())
}

/// WINDOWS: hand our right to set the foreground window to the instance we are
/// forwarding to, so its `SW_RESTORE` + `SetForegroundWindow` actually raises the
/// window the new tab lands in instead of blinking the taskbar button.
///
/// The OS refuses `SetForegroundWindow` from a process that is not itself in the
/// foreground; `AllowSetForegroundWindow(pid)` is the documented handoff, and the
/// launching `aterm.exe` HAS that right (it was started by the foreground
/// process — the shell the user typed in, or Explorer for the jump-list task).
/// Best-effort in every direction: no pid (an explicit `$ATERM_CONTROL_SOCK`
/// path names no instance), a stale alias, or a refusal all leave the receiver
/// to fall back to the taskbar flash, which is Windows' own answer for "a
/// background app wants your attention".
///
/// Called AFTER the connect (the instance is proven live) and BEFORE the request
/// line, because the grant lasts only until the next input event — the receiver
/// must be able to spend it the moment the spawn completes. Nothing about the
/// forward's success depends on it: the reply framing, the deadlines and the
/// caller's routing are all unchanged whether the grant lands or not.
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
    // pointers and no preconditions; a refusal is reported as a `0` return,
    // which is deliberately ignored (see the doc comment).
    let _ = unsafe { AllowSetForegroundWindow(pid) };
}

#[cfg(not(windows))]
fn allow_foreground_handoff(_path: &str) {}

/// The pid of the instance a front-door socket path belongs to, parsed from the
/// per-instance filename `aterm-<pid>.sock`, resolving the `latest` POINTER FILE
/// first (on Windows the alias is a regular file naming the instance socket —
/// see `aterm_uds::latest`). `None` when the path names no instance: an explicit
/// `$ATERM_CONTROL_SOCK` override, or a dangling/foreign alias.
#[cfg(windows)]
fn instance_pid_of(path: &str) -> Option<u32> {
    let resolved = aterm_uds::latest::resolve(path);
    let name = std::path::Path::new(&resolved).file_name()?.to_str()?;
    control_socket::instance_pid(name)
}

/// Environment variable consulted for the socket path when `--sock`/`--pid`
/// are absent. `0`/`off` mean the server runs without a socket.
const SOCK_ENV: &str = "ATERM_CONTROL_SOCK";

/// Environment kill switch: set (truthy) means the server has no socket.
const NO_SOCK_ENV: &str = "ATERM_NO_CONTROL_SOCK";

/// Socket filename inside the per-user directory — the server-maintained
/// `latest` symlink to the newest instance's socket.
const SOCK_FILE: &str = control_socket::LATEST_SOCK_FILE;

/// Resolve the per-user directory holding the control socket + token — delegated to
/// [`aterm_uds::control_socket_dir`] so the client and the server
/// (`control_auth::socket_dir`) share ONE decision and can never dial different dirs.
/// Windows: `%TEMP%\aterm` (outside the OneDrive/AppData subtree afunix connect can't
/// reach); Unix: `$XDG_RUNTIME_DIR/aterm`, else `~/Library/Application Support/aterm`.
fn socket_dir() -> Option<PathBuf> {
    aterm_uds::control_socket_dir()
}

/// The default socket path: `<socket_dir>/aterm.sock`. `None` only when neither
/// `XDG_RUNTIME_DIR` nor `HOME` is set.
fn default_sock_path() -> Option<String> {
    Some(socket_dir()?.join(SOCK_FILE).to_string_lossy().into_owned())
}

/// Environment variable naming the aterm session that HOSTS this process's
/// terminal (injected into every session's shell). Its discovery graph entry
/// locates the instance socket that hosts the calling terminal.
const SELF_SID_ENV: &str = "ATERM_PARENT_SESSION_ID";

/// IN-SESSION SELF-LOCATION: the socket of the instance hosting the calling
/// terminal, located via `sid`'s graph entry (`<dir>/graph/<sid>`, parsed with
/// the shared [`control_socket::graph_entry_sock`]). With several instances
/// running, the `latest` symlink points at the NEWEST one — not necessarily
/// OURS — so a flagless in-session call would otherwise drive a stranger.
///
/// `<dir>` here is the DEFAULT rendezvous dir ([`socket_dir`]). This resolves
/// the calling terminal's instance even when that instance was launched on an
/// EXPLICIT `$ATERM_CONTROL_SOCK` whose socket lives in some OTHER directory:
/// the server publishes each session's graph entry into the default dir too
/// (not only beside the explicit socket), and the entry carries the socket's
/// ABSOLUTE path, so the returned socket points at the right instance wherever
/// its socket actually lives (FINDING #2 — the split-brain fix that makes the
/// "flagless calls always reach the instance hosting the calling terminal"
/// promise hold for explicit-socket instances, not just default ones).
///
/// Returns `None` (→ fall back to the symlink) when not inside an aterm
/// session, the entry is missing (e.g. an older server), or nothing is
/// LISTENING on the recorded socket (a crashed own-instance leaves the socket
/// FILE behind — a bare existence check would then error out on a dead socket
/// instead of falling back).
fn self_instance_sock(self_sid: Option<&str>) -> Option<String> {
    let sid = self_sid?;
    // The sid shape is server-generated (`s-<hex>`); refuse anything else so a
    // weird env value can never path-traverse out of the graph dir.
    let hex = sid.strip_prefix("s-")?;
    if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let body = std::fs::read_to_string(socket_dir()?.join("graph").join(sid)).ok()?;
    let sock = control_socket::graph_entry_sock(&body)?;
    // Liveness probe, not existence: only a socket something ACCEPTS on counts.
    if CtlStream::connect(&sock).is_ok() {
        Some(sock)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// THE MULTIPLEXER BOUNDARY (screen / tmux)
//
// aterm hosts ONE session per PTY. Run `screen` or `tmux` in that PTY and the
// multiplexer owns it: every pane it draws lives inside the SAME aterm session,
// and aterm has no name for a pane. But `$ATERM_PARENT_SESSION_ID` is an
// ordinary exported variable, so it rides into every pane shell unchanged — and
// the flagless self-location above then resolves it to the session HOSTING the
// multiplexer. A `aterm ctl send …` typed in a pane therefore drives the outer
// terminal, and for an agent driving ITSELF that is silent mis-targeting: the
// keystrokes land, the reply says OK, and the wrong session moved.
//
// The rule this seam implements: inside a multiplexer, a call that names its
// target only IMPLICITLY (flagless, `@.`, or the `@self` that literally claims
// "this session") is REFUSED with the outer sid and the explicit form to use.
// Everything that names a target — `@<sid>`, `--pid`, `--sock`, an explicit
// `$ATERM_CONTROL_SOCK` — is unchanged, because none of those is silent.
// ---------------------------------------------------------------------------

/// Environment marker the shell integration exports when the shell it loaded
/// into is a MULTIPLEXER CHILD of an aterm session shell — `tmux` or `screen`.
///
/// Set from INSIDE the pane, so it exists only where the integration is really
/// sourced there: a hand-installed `source …` line in the user's rc, or fish
/// (whose vendor `conf.d` rides `$XDG_DATA_DIRS` into every pane). Under aterm's
/// own managed injection a bash or zsh pane shell never sources the script at
/// all — `--rcfile` and `ZDOTDIR` are one-shot argv/env mechanisms that only the
/// shell ATERM ITSELF starts receives — so this marker is usually ABSENT in a
/// real pane and [`MUX_BASE_ENV`] is what carries the verdict there. Measured on
/// this host in a real GNU screen 4.09.01 window: `$STY`, the multiplexer
/// `$TERM` and the inherited loader guard were all set and `$ATERM_MUX` was
/// still empty.
///
/// Its disable spellings (`0`/`off`/`no`/`none`/`false`) are the escape hatch:
/// a stale `$TMUX`/`$STY` inherited by a nested shell can otherwise make the
/// heuristics refuse a call that was never in a pane at all.
const MUX_ENV: &str = "ATERM_MUX";

/// The multiplexer environment the aterm SESSION shell itself was started under,
/// recorded by the shell integration as `"<$TMUX>|<$STY>"`.
///
/// This is the one detection input that comes from a place which ACTUALLY RUNS
/// for every session aterm starts — the tail of the integration script, past the
/// loader guard — and it reaches a pane the only way anything can: ordinary
/// environment inheritance. A pane's own `$TMUX`/`$STY` are the multiplexer's
/// and no longer match the recorded base, which is the crossing; an aterm window
/// merely LAUNCHED from a pane re-runs the integration and re-stamps the base as
/// its own, so it matches and is not refused. That is the same question the
/// loader guard was invented to answer, asked where the answer is available.
///
/// It also closes the hole `TERM` cannot: a tmux configured with
/// `default-terminal "xterm-256color"` looks exactly like an aterm window to the
/// `TERM` heuristic, and looks like a pane to this.
const MUX_BASE_ENV: &str = "ATERM_MUX_BASE";

/// The aterm session HOSTING the detected multiplexer — a copy of
/// `$ATERM_PARENT_SESSION_ID` taken by the shell integration at the boundary and
/// exported beside [`MUX_ENV`]. It names the terminal a flagless call WOULD have
/// driven, which is exactly what the refusal has to hand back to the caller.
/// Absent whenever [`MUX_ENV`] is (see there); `$ATERM_PARENT_SESSION_ID` itself
/// is the fallback, and it rides into a pane unchanged.
const MUX_OUTER_SID_ENV: &str = "ATERM_MUX_OUTER_SESSION_ID";

/// tmux's per-client marker (`<socket>,<pid>,<session>`), set in every pane.
const TMUX_ENV: &str = "TMUX";

/// GNU screen's per-session marker (`<pid>.<tty>.<host>`), set in every window.
const SCREEN_ENV: &str = "STY";

/// The terminal type — the corroborating witness `$TMUX`/`$STY` need.
///
/// The INNERMOST layer to write `TERM` wins, and both multiplexers always
/// rewrite it for their pane children (screen forces `screen*`; tmux's
/// `default-terminal` is `screen-256color` out of the box), while aterm's spawn
/// seam forces `xterm-256color` for every session shell. `$TMUX`/`$STY` alone
/// cannot carry the decision because an aterm launched FROM a pane inherits
/// them verbatim — `TERM` is what tells that window apart from a real pane.
const TERM_ENV: &str = "TERM";

/// A multiplexer standing between the calling shell and the aterm session a
/// flagless call would drive.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MuxNesting {
    /// `tmux` or `screen` — the multiplexer around this shell.
    kind: &'static str,
    /// The aterm session hosting it, when the environment names one. `None`
    /// means a multiplexer with no aterm session identity in scope (e.g. screen
    /// inside a non-aterm terminal): there is nothing to mis-target, so the
    /// refusal below does not apply and only the report mentions it.
    outer_sid: Option<String>,
    /// Which tier saw it, verbatim as the `mux` report's `detected_by=` field:
    /// `shell-integration` ([`MUX_ENV`], set from inside the pane),
    /// `session-base` ([`MUX_BASE_ENV`], stamped by the session shell and
    /// compared here), or `environment` (the `$TMUX`/`$STY`/`$TERM` heuristic).
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

/// The multiplexer markers of the CURRENT environment, in the spelling the shell
/// integration stamps into [`MUX_BASE_ENV`]: `"<$TMUX>|<$STY>"`, empty for an
/// unset side. Compared for equality only — it is an identity, not a parse.
fn mux_env_signature(tmux: Option<&str>, sty: Option<&str>) -> String {
    let mut sig = String::from(tmux.unwrap_or(""));
    sig.push('|');
    sig.push_str(sty.unwrap_or(""));
    sig
}

/// The multiplexer the per-pane MARKERS name, or `None` when neither is set.
/// `$TMUX` is consulted first because tmux's default `TERM` is
/// `screen-256color`, so only the marker can tell the two programs apart.
fn marker_kind(tmux: Option<&str>, sty: Option<&str>) -> Option<&'static str> {
    if tmux.is_some_and(|v| !v.is_empty()) {
        Some("tmux")
    } else if sty.is_some_and(|v| !v.is_empty()) {
        Some("screen")
    } else {
        None
    }
}

/// Decide whether a multiplexer sits between this shell and its aterm session,
/// from the environment values alone (passed in, so the decision is a pure
/// function the tests can drive without touching the process environment).
///
/// Three tiers, in order:
///
/// 1. [`MUX_ENV`] — a verdict the integration reached INSIDE the pane, including
///    its explicit disable spellings, which win outright. Present only where the
///    script is genuinely sourced in a pane (see [`MUX_ENV`]).
/// 2. [`MUX_BASE_ENV`] — the multiplexer environment the aterm SESSION shell
///    stamped for itself. Set for every session aterm starts through a shell the
///    integration supports, and inherited verbatim by every pane, so this is the
///    tier that answers in a real bash/zsh pane. A signature that MATCHES the
///    base is an authoritative NO (this shell's multiplexer environment is the
///    session shell's own, which is exactly the aterm-window-launched-from-a-pane
///    case); a signature that differs AND names a multiplexer is an
///    authoritative YES, `TERM` unread.
/// 3. The `$TMUX`/`$STY` + `TERM` heuristic, for a shell that carries neither
///    mark — an old session shell started before the base existed, or a shell
///    aterm did not start. It fires only when BOTH an aterm session identity is
///    in scope (something to mis-target) and `TERM` names a multiplexer (the
///    corroboration `$TMUX`/`$STY` need — see [`TERM_ENV`]).
///
/// The known gap, stated rather than papered over: a tmux configured with
/// `default-terminal "xterm-256color"` defeats tier 3's `TERM` corroboration.
/// Tier 2 catches it — it never reads `TERM` — for every session started by a
/// shell the integration loads into; a pane of a shell with neither mark is
/// still invisible, and `aterm ctl mux` says `mux=none` there rather than
/// pretending.
fn detect_mux_nesting(
    mux: Option<&str>,
    base: Option<&str>,
    outer_sid: Option<&str>,
    tmux: Option<&str>,
    sty: Option<&str>,
    term: Option<&str>,
    self_sid: Option<&str>,
) -> Option<MuxNesting> {
    fn set(v: Option<&str>) -> Option<&str> {
        v.filter(|s| !s.is_empty())
    }
    let outer = set(outer_sid).or_else(|| set(self_sid)).map(str::to_string);
    if let Some(marker) = set(mux) {
        if matches!(marker, "0" | "off" | "no" | "none" | "false") {
            return None;
        }
        return Some(MuxNesting {
            kind: mux_kind(marker).unwrap_or("multiplexer"),
            outer_sid: outer,
            detected_by: "shell-integration",
        });
    }
    if let Some(base) = set(base) {
        // The session shell recorded what it was born into. Equal means nothing
        // has been entered since; different-and-marked means a pane. Different
        // with no marker at all (someone unset $TMUX/$STY by hand) names no
        // multiplexer to report, so fall through rather than invent one.
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
    // Nothing to mis-target without an aterm identity in scope: a screen inside
    // a plain xterm legitimately drives the user's aterm windows flaglessly.
    // `as_ref()?` rather than an `is_none()` early return: the workspace lints
    // deny `question_mark`, and `outer` is still moved into `outer_sid` below,
    // so the borrow-and-discard is the equivalent that keeps it owned.
    outer.as_ref()?;
    let term_kind = mux_kind(set(term)?)?;
    Some(MuxNesting {
        kind: marker_kind(tmux, sty).unwrap_or(term_kind),
        outer_sid: outer,
        detected_by: "environment",
    })
}

/// [`detect_mux_nesting`] over the real process environment.
fn mux_nesting_from_env() -> Option<MuxNesting> {
    detect_mux_nesting(
        env::var(MUX_ENV).ok().as_deref(),
        env::var(MUX_BASE_ENV).ok().as_deref(),
        env::var(MUX_OUTER_SID_ENV).ok().as_deref(),
        env::var(TMUX_ENV).ok().as_deref(),
        env::var(SCREEN_ENV).ok().as_deref(),
        env::var(TERM_ENV).ok().as_deref(),
        env::var(SELF_SID_ENV).ok().as_deref(),
    )
}

/// Verbs answered by this CLIENT, which never reach a server and so are not in
/// the shared verb table [`addresses_no_session`] consults. They address no
/// session and cannot be mis-targeted by a multiplexer.
const MUX_EXEMPT_VERBS: [&str; 4] = ["instances", "ls", "windows", "mux"];

/// Whether `verb` addresses NO SESSION, and so has nothing a multiplexer could
/// silently redirect.
///
/// Read from the SHARED verb table rather than a second hand-kept list:
/// `Target::Meta` is the table's own name for "self-scoped or fleet-wide; a
/// selector is meaningless here" (`version`, `sessions`, `who`, `whoami`,
/// `grant`, `flows`, `dial-*`, `help`, `verbs`, …). Refusing those inside a
/// multiplexer protected nothing — there is no session to point at the wrong
/// terminal — and the refusal's own advice made it worse: `aterm ctl @<sid>
/// sessions` is answered `ERR denied` by the server, because a selector on an
/// owner-only meta verb is exactly what it rejects. Measured against a live
/// instance: `@<sid>` with `sessions`/`who`/`whoami`/`flows` → `ERR denied`.
fn addresses_no_session(verb: &str) -> bool {
    MUX_EXEMPT_VERBS.contains(&verb)
        || aterm_types::control_verbs::spec(verb)
            .is_some_and(|s| matches!(s.target, aterm_types::control_verbs::Target::Meta))
}

/// Whether the request names its target only IMPLICITLY — the shape that
/// silently follows `$ATERM_PARENT_SESSION_ID` or "whatever tab is in front".
///
/// * `@self` / `@env` — always implicit: the token literally claims "the session
///   I am in", and inside a pane that claim is false however the socket was
///   chosen, so flags do not rescue it.
/// * no selector, or `@.` — implicit only while nothing else names a target;
///   both follow the instance's ACTIVE tab. `--sock`, `--pid` and an explicit
///   `$ATERM_CONTROL_SOCK` each pin the instance deliberately, and a deliberate
///   choice is not the silent mis-targeting this guards.
/// * a concrete `@<sid>` — never implicit.
fn targets_own_session_implicitly(
    parts: &[String],
    sock: Option<&str>,
    pid: Option<u32>,
    env_sock: Option<&str>,
) -> bool {
    let Some(first) = parts.first() else {
        return false;
    };
    if addresses_no_session(first) {
        return false;
    }
    // `subscribe` carries its selector SECOND; every other verb takes an
    // optional leading `@<sel>` proxy token.
    let sel_idx = usize::from(first == "subscribe");
    let selector = parts.get(sel_idx).filter(|t| t.starts_with('@'));
    // Only the flagless per-instance case leaves the target to the environment;
    // Explicit/Disabled both mean the caller (or the environment) already
    // decided, and `Disabled` has its own clearer error downstream.
    let unpinned = sock.is_none()
        && pid.is_none()
        && matches!(
            control_socket::socket_directive(env_sock, None),
            SocketDirective::PerInstance
        );
    match selector {
        Some(sel) if sel.split(',').any(|e| e == "@self" || e == "@env") => true,
        Some(sel) if sel.split(',').all(|e| e == "@.") => unpinned,
        Some(_) => false,
        None => unpinned,
    }
}

/// The refusal for an implicitly self-targeting call made inside a multiplexer:
/// what is wrong, which session it would have driven, and the ways to say what
/// you meant. Long on purpose — this is the one message standing between an
/// agent and a keystroke landing in the wrong terminal. Hand-composed (no inline
/// `format_args!`), like every other client error here.
///
/// Every remedy it names is one this refusal's own reachable set accepts, which
/// is a correctness property and not a wording one. `@<sid>` is offered because
/// what survives the guard is exactly the SESSION- and APP-targeted verbs, where
/// a selector is how you name a target; the session-less (`Target::Meta`) verbs
/// that a selector would earn `ERR denied` for are never refused in the first
/// place (see [`addresses_no_session`]), so this message never sends anyone
/// there. `--pid`/`--sock` are pins the guard treats as deliberate, and
/// `instances` is how you learn either value from inside a pane — so the line
/// naming them is followed all the way through instead of assuming the reader
/// already knows the pid.
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
    msg.push_str(" <verb>     drive the outer terminal deliberately\n    aterm ctl --pid <pid> <verb>   or --sock <path>, to pin one instance\n    aterm ctl instances            lists the pid and socket of every live instance\n    aterm ctl mux                  what aterm can and cannot see from in here\n  Verbs that address no session at all (version, sessions, who, whoami, grant, flows, dial-*, help)\n  are answered from in here unrefused — they have no target to redirect, and a selector on them is\n  what the server rejects. OSC 133 marks do not cross ");
    msg.push_str(n.kind);
    msg.push_str(" either, so blocks/exit codes/cwd for ");
    msg.push_str(sid);
    msg.push_str("\n  describe the multiplexer, not your pane. Set ATERM_MUX=0 if this shell is not really inside ");
    msg.push_str(n.kind);
    msg.push('.');
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

/// `0` silences the one-time boundary notice. Spelled exactly as the shell
/// integration spells it (a literal `0`, nothing else) so one value silences
/// both halves; the richer disable family belongs to [`MUX_ENV`], which turns
/// the whole guard off rather than just the sentence.
const MUX_NOTICE_ENV: &str = "ATERM_MUX_NOTICE";

/// The one-time boundary notice — signal #1, the sentence that tells a person
/// their command blocks just stopped existing.
///
/// It reads as one sentence with the [`stderr_line`] `aterm-ctl: ` prefix, and
/// is otherwise word-for-word the shell integration's, because the two are the
/// same statement said by whichever half gets to run.
fn mux_boundary_notice(kind: &str) -> String {
    let mut msg = String::from("inside ");
    msg.push_str(kind);
    msg.push_str(" — command blocks, exit codes and cwd tracking do not cross the multiplexer,\n  so aterm records none of them for these panes. `aterm ctl mux` explains; ATERM_MUX_NOTICE=0 silences this.");
    msg
}

/// The stamp that makes the notice ONCE-per-multiplexer instead of once per
/// call, keyed by the multiplexer's OWN id so every pane of one screen or tmux
/// shares it.
///
/// Path, key and sanitising are the shell integration's byte for byte
/// (`${XDG_RUNTIME_DIR:-${TMPDIR:-/tmp}}/aterm/mux-notice/<kind>-<id>`, id =
/// `${TMUX:-${STY:-$TERM}}` with every character outside `[A-Za-z0-9._-]`
/// replaced by `_`). That is the point: where BOTH halves run — a hand-installed
/// integration in a pane, or fish's vendor `conf.d` — whichever speaks first
/// claims the stamp and the other stays quiet, so nobody is told twice.
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
/// whichever process creates the stamp first.
///
/// `create_new` is the whole mechanism — an atomic "I am the first", so six
/// panes racing on one screen produce one sentence rather than six. Every error
/// is swallowed deliberately: a read-only runtime dir must cost the caller
/// nothing, and a notice is not worth failing a verb over. Returning `false`
/// when the stamp cannot be written also means the sentence is skipped rather
/// than repeated on every call, which is the kinder failure.
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

/// [`claim_mux_notice`] with the stamp already resolved — the whole mechanism,
/// separated from the environment read so a test can drive the real claim
/// against a real path instead of asserting about a fabricated one.
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

/// Say the boundary out loud once, unless something better is about to say it.
///
/// The refusal, the `mux` report and the `blocks`/`status` degradation note each
/// already carry the whole fact in a sharper form; those callers pass
/// `already_said = true`, which CLAIMS the stamp without printing so the short
/// notice does not turn up afterwards as a duplicate.
fn announce_mux_boundary(nesting: Option<&MuxNesting>, already_said: bool) {
    let Some(n) = nesting else {
        return;
    };
    announce_mux_boundary_with(n, already_said, claim_mux_notice);
}

/// [`announce_mux_boundary`]'s decision, with the CLAIM injected.
///
/// The gate below is the whole subject: a boundary that cost nothing must not
/// even reach the stamp. Testing that against the shipping `claim_mux_notice`
/// means testing against `$XDG_RUNTIME_DIR`, which a std-only test cannot
/// safely retarget (mutating the process environment races every other test in
/// the binary). So the claim is the seam, and the test hands in one that writes
/// where it can see.
fn announce_mux_boundary_with(
    n: &MuxNesting,
    already_said: bool,
    claim: impl FnOnce(&MuxNesting) -> bool,
) {
    // No aterm identity in scope means no aterm block model to have LOST — a
    // screen inside somebody else's terminal never had one. The shell half
    // draws the line in the same place (it announces only under
    // `$ATERM_PARENT_SESSION_ID`), and this is also what keeps `aterm ctl` from
    // leaving stamps for multiplexers that have nothing to do with aterm.
    if n.outer_sid.is_none() {
        return;
    }
    if claim(n) && !already_said {
        let _ = stderr_line(&mux_boundary_notice(n.kind));
    }
}

/// The honest note for a verb whose answer is SHAPED by the boundary: `blocks`
/// and `status` read the OSC 133 block model, and no mark emitted inside a
/// multiplexer ever reaches aterm. Returned only when the request actually
/// addresses the session hosting the multiplexer (explicitly, or implicitly with
/// a pinned instance) — otherwise the note would be about a session the caller
/// never asked for.
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
    msg.push_str(" are ABSENT,\n  not empty. `aterm ctl mux` explains; ATERM_MUX=0 silences this.");
    Some(msg)
}

/// The `mux` CLIENT verb: what aterm can and cannot see from this shell, as one
/// machine-readable line on STDOUT plus the human explanation on STDERR (so a
/// script parses the line and a person still gets told what it means).
fn run_mux_report(nesting: Option<&MuxNesting>) -> io::Result<ExitCode> {
    let Some(n) = nesting else {
        print_stdout_line("mux=none")?;
        stderr_line(
            "no multiplexer between this shell and its aterm session: \
             a flagless call drives the session you are looking at",
        )?;
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
        "OSC 133 marks are ABSENT, not empty: aterm delivers its shell integration through the\n    \
         argv the SPAWN SEAM controls (bash --rcfile, zsh's ZDOTDIR), and a pane shell is started\n    \
         by the multiplexer, not by aterm — so the hooks that emit the marks are never defined\n    \
         here, and neither screen nor tmux forwards an unknown OSC to the outer terminal anyway.\n    \
         Command blocks, exit codes and cwd tracking are dead for the duration.\n  * \
         Verbs that address no session (version, sessions, who, whoami, grant, flows, dial-*) are\n    \
         answered from in here unrefused; name a target explicitly (@<sid>, --pid, --sock) to\n    \
         drive a real aterm session.",
    );
    stderr_line(&note)?;
    Ok(ExitCode::SUCCESS)
}

/// Resolve the socket path from the flags and the environment values. Flags
/// win over the environment; `--pid` targets one instance's
/// `<dir>/aterm-<pid>.sock` directly. The env interpretation (explicit path
/// vs `0`/`off` disable keywords vs per-instance default) is the engine's
/// [`control_socket::socket_directive`], identical to the server's — plus one
/// client-side refinement: in the per-instance-default case, a caller INSIDE an
/// aterm session (`self_sid` = `$ATERM_PARENT_SESSION_ID`) resolves to the
/// instance hosting ITS OWN terminal via the discovery graph, not to whichever
/// instance most recently claimed the `latest` symlink.
fn resolve_path(
    sock: Option<String>,
    pid: Option<u32>,
    env_sock: Option<String>,
    env_no_sock: Option<String>,
    self_sid: Option<String>,
) -> io::Result<String> {
    let no_dir = || {
        io::Error::new(
            io::ErrorKind::NotFound,
            "cannot resolve control socket: set --sock, $ATERM_CONTROL_SOCK, \
             or $XDG_RUNTIME_DIR/$HOME",
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
        return Ok(dir
            .join(control_socket::instance_sock_name(pid))
            .to_string_lossy()
            .into_owned());
    }
    if let Some(s) = sock {
        return Ok(s);
    }
    match control_socket::socket_directive(env_sock.as_deref(), env_no_sock.as_deref()) {
        SocketDirective::Disabled => Err(io::Error::new(
            io::ErrorKind::NotFound,
            "the control socket is disabled in this environment \
             ($ATERM_CONTROL_SOCK=0/off or $ATERM_NO_CONTROL_SOCK)",
        )),
        SocketDirective::Explicit(p) => Ok(p),
        // Flagless: prefer the instance HOSTING this terminal (in-session
        // self-location) over the newest-instance `latest` symlink.
        SocketDirective::PerInstance => self_instance_sock(self_sid.as_deref())
            .map(Ok)
            .unwrap_or_else(|| default_sock_path().ok_or_else(no_dir)),
    }
}

/// The per-socket deadline discovery dials with. Discovery dials EVERY socket
/// in the shared dir sequentially, so one stuck peer must never hang the whole
/// sweep; 2 s is generous for a main-thread `sessions` answer.
const PROBE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(2);

/// One socket's answer to the discovery probe, with every failure class KEPT
/// DISTINCT. `ls`/`instances` used to fold all of them into `None` and then,
/// with zero survivors, print the fleet-wide claim "no live aterm instances
/// found". Under Codex CLI's seatbelt that claim was FALSE: readdir found the
/// sockets and `connect()` was refused with `EPERM` (finding F8). An agent that
/// reads "empty" stops looking; one that reads "could not reach: Operation not
/// permitted" asks for the socket allowance. So the probe classifies, and
/// [`discovery_report`] says which.
#[derive(Debug)]
enum Probe {
    /// `OK <n>` and its `n` body lines.
    Answered(Vec<String>),
    /// `ECONNREFUSED`: the socket file exists but nothing accepts on it — the
    /// leftover of a pid that exited without cleanup, or a pid still booting.
    /// `pid_running` is the liveness verdict on the pid the socket names
    /// (`None` when it names none), resolved HERE so the report stays pure.
    Refused { pid_running: Option<bool> },
    /// `EPERM`/`EACCES`/`ENOTCONN` on `connect()`: a sandbox (Codex CLI's
    /// seatbelt allows AF_UNIX connect only under its writable roots) or
    /// another user's 0600 socket. NOT "not running" — the remedy differs.
    Denied(io::Error),
    /// The token beside the socket could not be read, so no `AUTH` line went
    /// out and the server answered `ERR auth`. Carries the file it looked for.
    NoToken(PathBuf, io::Error),
    /// A token WAS sent and the server still answered `ERR auth`: the file on
    /// disk is not the token the server holds (a restarted instance rewrote it).
    AuthRejected,
    /// The [`PROBE_DEADLINE`] fired on a read or a write.
    Timeout,
    /// Anything else, with the raw cause: a socket file that vanished between
    /// readdir and dial, a non-auth `ERR`, a malformed header, a connection
    /// closed without a reply.
    Other(io::Error),
}

/// One framed request→response against the socket at `path` WITHOUT printing:
/// authenticate, send `verb`, read the `OK <n>` header + `n` body lines, and
/// return them as [`Probe::Answered`] — or the CLASSIFIED failure, never a
/// bare `None`. `pid` is the instance pid the socket names (`0` = unknown),
/// consulted only to label a refused connect as stale-or-booting. Lines are
/// read through [`read_bounded_line`]: the deadline alone cannot stop a peer
/// that STREAMS newline-less bytes (each read succeeds, so the clock keeps
/// resetting), so the accumulation cap is what bounds memory.
fn probe_lines(path: &str, verb: &str, pid: u32) -> Probe {
    let stream = match CtlStream::connect(path) {
        Ok(stream) => stream,
        Err(e) => return classify_connect_error(e, pid),
    };
    if let Err(e) = stream
        .set_read_timeout(Some(PROBE_DEADLINE))
        .and_then(|()| stream.set_write_timeout(Some(PROBE_DEADLINE)))
    {
        return Probe::Other(e);
    }
    // An unreadable token is not fatal YET: the request still goes out without
    // an `AUTH` line, and only the server's `ERR auth` proves the miss mattered
    // (an instance that answers anyway is simply answered).
    let token_miss = match read_token_at(path) {
        Ok(token) => {
            if let Err(e) = (&stream).write_all(format!("AUTH {token}\n").as_bytes()) {
                return io_probe(e);
            }
            None
        }
        Err((token_path, e)) => Some(Probe::NoToken(token_path, e)),
    };
    if let Err(e) = (&stream)
        .write_all(format!("{verb}\n").as_bytes())
        .and_then(|()| (&stream).flush())
    {
        return io_probe(e);
    }
    let mut reader = BufReader::new(&stream);
    let mut status = String::new();
    match read_bounded_line(&mut reader, &mut status) {
        Err(e) => return io_probe(e),
        Ok(0) => {
            return Probe::Other(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the server closed the connection without replying",
            ));
        }
        Ok(_) => {}
    }
    let status = status.trim_end();
    let Some(tail) = status.strip_prefix("OK ") else {
        return if status.starts_with("ERR auth") {
            token_miss.unwrap_or(Probe::AuthRejected)
        } else if status.starts_with("ERR") {
            Probe::Other(io::Error::other(format!("server replied: {status}")))
        } else {
            Probe::Other(malformed_header_error(status))
        };
    };
    let Some(count) = stream_count(tail) else {
        return Probe::Other(malformed_header_error(status));
    };
    let mut lines = Vec::with_capacity(count);
    for _ in 0..count {
        let mut line = String::new();
        match read_bounded_line(&mut reader, &mut line) {
            Err(e) => return io_probe(e),
            Ok(0) => break,
            Ok(_) => lines.push(line.trim_end_matches(['\r', '\n']).to_string()),
        }
    }
    Probe::Answered(lines)
}

/// Map a failed `connect()` to its [`Probe`] class. `ECONNREFUSED` is a stale
/// (or still-booting) instance, labelled by whether `pid` is alive;
/// `EPERM`/`EACCES`/`ENOTCONN` are a refusal by the caller's sandbox or by a
/// foreign user's socket mode — NOT "not running", and the report never says
/// so. Everything else keeps the raw cause, framed like [`connect_error`].
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

/// What the rendezvous directory turned out to be — the OTHER half of "could
/// not look" the old fleet claim hid: `$HOME` unset, `$XDG_RUNTIME_DIR` naming
/// a directory that does not exist, a readdir the sandbox refuses. Only
/// [`DirOutcome::Empty`] licenses the words "no live aterm instances found".
#[derive(Debug)]
enum DirOutcome {
    /// No environment variable that names the directory is set.
    Unresolvable,
    /// The resolved directory does not exist; `why` says which variable it was
    /// resolved from (and, under `$XDG_RUNTIME_DIR`, that the default was
    /// therefore NOT consulted).
    Missing { path: PathBuf, why: String },
    /// `read_dir` failed for a reason other than absence — a sandbox refusing
    /// readdir, or a mode that excludes this user.
    Unreadable { path: PathBuf, err: io::Error },
    /// Readable and holding no instance socket and no graph entry: a truly
    /// empty fleet.
    Empty(PathBuf),
    /// Readable, with `n` instances (readdir + graph entries) to dial.
    Found { path: PathBuf, n: usize },
    /// `--sock`/`--pid` named the target; the directory was not the question.
    Scoped,
}

/// The variables [`socket_dir`] resolves from, for the message that says none
/// of them is set (`aterm_uds::control_socket_dir`'s two on Unix, its three on
/// Windows).
#[cfg(not(windows))]
const DIR_ENV_UNSET: &str = "$XDG_RUNTIME_DIR and $HOME are both unset";
#[cfg(windows)]
const DIR_ENV_UNSET: &str = "%TMP%, %TEMP% and %LOCALAPPDATA% are all unset";

/// Why a resolved-but-absent directory was the one looked in. Under
/// `$XDG_RUNTIME_DIR` this must name the default that was NOT consulted: an
/// agent whose harness exports the variable otherwise reads "no such
/// directory" as "no aterm", while the human's instance sits in
/// `~/Library/Application Support/aterm` untouched.
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
fn inspect_fleet() -> (DirOutcome, Vec<(u32, String)>) {
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

/// Every live same-user aterm instance: `(pid, sock_path)`, discovered two ways
/// and DEDUPED by socket path so no instance is listed twice. Sorted by pid.
/// Empty whenever the directory could not be looked in — a caller that must
/// tell that apart from an empty fleet uses [`inspect_fleet`].
fn live_instances() -> Vec<(u32, String)> {
    inspect_fleet().1
}

/// The enumeration behind [`live_instances`], over an already-open readdir of
/// `dir`:
///
/// 1. Per-instance sockets living directly in the default dir
///    (`<dir>/aterm-<pid>.sock`; the `latest` symlink and non-instance names are
///    skipped). This is the default-launch case.
/// 2. EXPLICIT-`$ATERM_CONTROL_SOCK` instances, whose socket lives OUTSIDE the
///    default dir and is therefore NOT in the readdir above: they register a
///    discovery graph entry in the default dir (`<dir>/graph/<sid>`), which carries
///    the absolute socket path + the hosting pid. Without this pass such an
///    instance would be invisible to `ls`/`instances` even though `@<sid>` can
///    reach it (FINDING #2). The entry's `sock` is used verbatim (an absolute
///    out-of-dir path), so `probe_lines` dials the real socket and `read_token_at`
///    finds that socket's own token beside it.
fn enumerate_instances(dir: &Path, entries: std::fs::ReadDir) -> Vec<(u32, String)> {
    let mut out: Vec<(u32, String)> = Vec::new();
    // Dedup by the CANONICAL socket path, not the raw string: a default instance
    // appears both in the readdir (raw `<dir>/aterm-<pid>.sock`) and, once it has
    // registered a session, in a graph entry whose path was canonicalized at
    // publish time (`/var` → `/private/var` on macOS). Both name the SAME file;
    // the canonical key collapses them so the instance is listed exactly once.
    let mut seen: HashSet<String> = HashSet::new();
    let remember = |seen: &mut HashSet<String>, sock: &str| -> bool {
        let key = std::fs::canonicalize(sock)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| sock.to_string());
        seen.insert(key)
    };
    // (1) Per-instance sockets directly in the default dir.
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(pid) = control_socket::instance_pid(&name) else {
            continue; // non-instance name (latest symlink, sibling token, …)
        };
        if !name.ends_with(".sock") {
            continue; // token files carry pids too; sockets only
        }
        let sock = dir.join(&name).to_string_lossy().into_owned();
        if remember(&mut seen, &sock) {
            out.push((pid, sock));
        }
    }
    // (2) Explicit-socket instances registered via the default dir's graph entries.
    //     Their socket lives OUTSIDE the default dir, so the readdir above misses
    //     it; the entry carries the absolute path + hosting pid. A default
    //     instance's own graph entry is deduped away by the canonical key above.
    if let Ok(entries) = std::fs::read_dir(dir.join("graph")) {
        for e in entries.flatten() {
            let Ok(body) = std::fs::read_to_string(e.path()) else {
                continue;
            };
            let Some(sock) = control_socket::graph_entry_sock(&body) else {
                continue;
            };
            // An explicit socket's filename encodes no pid; the entry records the
            // hosting one. `0` is a benign placeholder for a legacy entry lacking it.
            let pid = control_socket::graph_entry_pid(&body).unwrap_or(0);
            if remember(&mut seen, &sock) {
                out.push((pid, sock));
            }
        }
    }
    out.sort_unstable();
    out
}

/// `instances` / `ls` — the client-side cross-terminal discovery verbs.
///
/// * `instances`: one line per live instance — `<pid> <session-count> <sock>`,
///   with ` self` appended on the instance hosting the calling terminal.
/// * `ls`: one line per SESSION across every live instance — `<pid> <sessions
///   line>`, with ` *` appended on the calling terminal's own session — the
///   one-shot view an agent uses to pick a peer terminal to drive by
///   `@<sid>`.
///
/// A socket that does not answer `sessions` (died between readdir and dial, or
/// a foreign server) is skipped silently: discovery reports what is REACHABLE,
/// exactly like the shell convention for `ls` on a churning directory.
///
/// Inside a MULTIPLEXER those two markers become a half-truth — the session they
/// point at hosts the whole screen/tmux, not the pane the caller is in — so a
/// note goes to STDERR saying so. STDERR, not the rows: the marker spelling is
/// what scripts parse, and the truth about it is what humans need.
/// Whether two socket path strings name the SAME socket, tolerant of a symlinked
/// ancestor (macOS `/var` → `/private/var`, an `XDG_RUNTIME_DIR` under `$TMPDIR`).
/// `self_instance_sock` canonicalizes its side but `live_instances` yields the RAW
/// readdir path, so a plain `==` dropped the ` self` marker whenever the socket dir
/// had a symlinked component. A raw match is the fast path; else compare canonical
/// forms (best-effort — an unresolvable path is simply not self).
fn same_socket_path(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => false,
    }
}

/// The instances discovery will report on, honouring an explicit `--sock`/`--pid`.
///
/// * `--pid P` — the single instance `P`, filtered out of the normal enumeration
///   (so it still benefits from graph-entry discovery of explicit-socket instances).
/// * `--sock S` — ONLY the instance at `S`. Its pid comes from the filename when
///   that encodes one (`aterm-<pid>.sock`); an explicitly-named socket does not,
///   so it falls back to the `0` placeholder this module already uses for a graph
///   entry with no `pid` line (see `control_socket::graph_entry_pid`).
/// * neither — every live instance, as before.
///
/// Returns `Err(message)` when the caller named an instance that is not live, so
/// the flags fail LOUDLY instead of silently widening to the whole fleet.
fn discovery_targets(sock: Option<&str>, pid: Option<u32>) -> Result<Vec<(u32, String)>, String> {
    if let Some(s) = sock {
        // Trust the caller's explicit socket: an instance may be perfectly live
        // without a graph entry in the default dir (a custom `$ATERM_CONTROL_SOCK`
        // that has not registered a session yet), so do NOT require enumeration
        // to know about it. `run_discovery` skips it if it does not answer.
        let name = Path::new(s)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let p = control_socket::instance_pid(&name).unwrap_or(0);
        return Ok(vec![(p, s.to_string())]);
    }
    if let Some(want) = pid {
        let hit: Vec<(u32, String)> = live_instances()
            .into_iter()
            .filter(|(p, _)| *p == want)
            .collect();
        if hit.is_empty() {
            return Err(format!(
                "no live aterm instance with pid {want} (run `aterm-ctl instances` to list them)"
            ));
        }
        return Ok(hit);
    }
    Ok(live_instances())
}

fn run_discovery(
    verb: &str,
    sock: Option<&str>,
    pid: Option<u32>,
    nesting: Option<&MuxNesting>,
) -> io::Result<ExitCode> {
    let self_sid = env::var(SELF_SID_ENV).ok();
    let self_sock = self_instance_sock(self_sid.as_deref());
    let stdout = io::stdout();
    let mut out = stdout.lock();
    // `--sock`/`--pid` name the target, so the directory is not the question
    // there; unscoped discovery classifies the directory itself, because "no
    // such directory" and "an empty directory" are different answers.
    let (dir, targets) = if sock.is_some() || pid.is_some() {
        match discovery_targets(sock, pid) {
            Ok(t) => (DirOutcome::Scoped, t),
            Err(msg) => {
                eprintln!("aterm-ctl: {msg}");
                return Ok(ExitCode::FAILURE);
            }
        }
    } else {
        inspect_fleet()
    };
    // Every probe is kept, answered or not: the rows print as they arrive, and
    // when NOTHING answered the classified misses ARE the report. A miss beside
    // an instance that answered stays silent — the success path gains no noise.
    let mut probes: Vec<(u32, String, Probe)> = Vec::with_capacity(targets.len());
    for (pid, sock, probe) in probe_sessions(targets) {
        if let Probe::Answered(sessions) = &probe {
            let is_self_instance = self_sock
                .as_deref()
                .is_some_and(|self_s| same_socket_path(self_s, &sock));
            if verb == "instances" {
                let marker = if is_self_instance { " self" } else { "" };
                writeln!(out, "{pid} {} {sock}{marker}", sessions.len())?;
            } else if verb == "windows" {
                for line in window_rows_from_sessions(pid, sessions) {
                    writeln!(out, "{line}")?;
                }
            } else {
                // The rows an in-process caller gets from `fleet_sessions`, printed:
                // ONE mapping decides both what `ls` shows and what the fleet bridge
                // federates, so the two can never drift apart.
                for session in instance_sessions(pid, sessions, self_sid.as_deref()) {
                    let marker = if session.is_self { " *" } else { "" };
                    writeln!(out, "{} {}{marker}", session.pid, session.row)?;
                }
            }
        }
        probes.push((pid, sock, probe));
    }
    let (report, code) = discovery_report(dir, &probes);
    if code != 0 {
        stderr_line(&report)?;
        return Ok(ExitCode::from(code));
    }
    // The ` self` / ` *` markers mean "hosts the calling terminal", and inside a
    // multiplexer that terminal is the one RUNNING screen/tmux — an honest note
    // rather than a marker silently read as "your pane".
    if let Some(n) = nesting.filter(|n| n.outer_sid.is_some()) {
        let mut note = String::from("NOTE: this shell is inside ");
        note.push_str(n.kind);
        note.push_str(", so the marked session hosts the WHOLE multiplexer, not your pane");
        stderr_line(&note)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// Dial each target for its `sessions` listing, LAZILY: the probe for an
/// instance runs only when the iterator reaches it, so a caller that prints as
/// it goes still emits an answered instance's rows while a later, wedged peer is
/// burning its [`PROBE_DEADLINE`]. The single place discovery decides WHAT it
/// asks every instance ([`run_discovery`] and [`fleet_sessions`] both ask this).
fn probe_sessions(targets: Vec<(u32, String)>) -> impl Iterator<Item = (u32, String, Probe)> {
    targets.into_iter().map(|(pid, sock)| {
        let probe = probe_lines(&sock, "sessions", pid);
        (pid, sock, probe)
    })
}

// ---------------------------------------------------------------------------
// `ls` AS DATA — the in-process listing
//
// `ls` is a CLIENT verb: it walks the rendezvous directory, dials every live
// instance and classifies each answer. An in-process caller that wants the same
// listing used to have only one way to get it — fork the `aterm-ctl` binary and
// re-parse its stdout — which throws away the very thing the F8 classification
// work added: WHY the listing is empty. `Command::output()` also swallows the
// child's stderr, where that reason was written. So the listing is exposed as
// DATA here, sharing the walk, the probes, the row mapping and the verdict text
// with the printing path above: one implementation, two callers.
// ---------------------------------------------------------------------------

/// One live session in the fleet, exactly as `ls` prints it.
///
/// `row` is the server's `sessions` line VERBATIM (`<local> <sid> <parent>
/// <state> <title…>`), so a consumer reads the same bytes the CLI shows; the
/// columns callers actually address by have accessors rather than copies, so
/// there is one parse of the row, not two that can disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetSession {
    /// The pid of the instance hosting this session — what `--pid` addresses.
    /// `0` when the socket names no pid (an explicit-socket instance found
    /// through a graph entry with no `pid` line), the same placeholder the
    /// printed rows carry.
    pub pid: u32,
    /// The server's `sessions` row for this session, verbatim.
    pub row: String,
    /// Whether this is the CALLING terminal's own session — the row `ls` marks
    /// with ` *`.
    pub is_self: bool,
}

impl FleetSession {
    /// The instance-local channel number — the row's first column (`ls`'s
    /// second). `None` for a row too short to carry one.
    #[must_use]
    pub fn local(&self) -> Option<&str> {
        self.row.split_whitespace().next()
    }

    /// The stable session id — the row's second column (`ls`'s third), and what
    /// `@<sid>` addresses. `None` for a row too short to carry one, which is the
    /// row a listing consumer skips (as the column parse always did).
    #[must_use]
    pub fn sid(&self) -> Option<&str> {
        self.row.split_whitespace().nth(1)
    }
}

/// Why a fleet listing produced no sessions — the distinction `ls` writes to
/// stderr and exits on, handed to an in-process caller instead of a shell.
///
/// The point of the type is that [`is_empty_fleet`](Self::is_empty_fleet) is a
/// question a caller can ASK: "nothing to federate" and "discovery is broken"
/// are different operational facts, and a bridge that folds them together goes
/// quietly blind (finding F8, one layer up).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetListError {
    /// The operator-facing reason, byte-identical to what `aterm-ctl ls` writes
    /// to stderr (without the `aterm-ctl: ` prefix): the classified cause per
    /// socket, and the hint for the dominant one.
    pub reason: String,
    /// The status `ls` would have exited with: [`EXIT_EMPTY_FLEET`] (1) for a
    /// readable directory holding no instance, [`EXIT_UNREACHABLE`] (2) for
    /// could-not-look or could-not-reach, [`EXIT_TIMEOUT`] (124) when every
    /// instance timed out.
    pub code: u8,
}

impl FleetListError {
    /// Whether the fleet is genuinely EMPTY — a readable rendezvous directory
    /// with no instance in it — as opposed to a directory that could not be
    /// looked in or instances that could not be reached.
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

/// The `ls` listing as data: every session on every REACHABLE instance of the
/// fleet, or the classified reason there is none.
///
/// Identical in scope and content to a bare `aterm-ctl ls` — the whole fleet
/// (discovery verbs are never narrowed by `$ATERM_CONTROL_SOCK`; only the
/// `--sock`/`--pid` flags scope them, and this entry point takes neither) — with
/// the ` *` self marker carried as [`FleetSession::is_self`] instead of a
/// trailing token.
///
/// # Errors
///
/// [`FleetListError`] whenever no instance answered, carrying the same reason
/// and status code the CLI reports. An empty `Ok` is impossible to confuse with
/// it: `Ok(rows)` means at least one instance answered.
pub fn fleet_sessions() -> Result<Vec<FleetSession>, FleetListError> {
    let self_sid = env::var(SELF_SID_ENV).ok();
    let (dir, targets) = inspect_fleet();
    let mut sessions = Vec::new();
    let mut probes: Vec<(u32, String, Probe)> = Vec::with_capacity(targets.len());
    for (pid, sock, probe) in probe_sessions(targets) {
        if let Probe::Answered(rows) = &probe {
            sessions.extend(instance_sessions(pid, rows, self_sid.as_deref()));
        }
        probes.push((pid, sock, probe));
    }
    fleet_listing(sessions, discovery_report(dir, &probes))
}

/// The sessions one ANSWERED instance contributes to a listing — PURE, so the
/// row mapping (including the ` *` self rule) is table-testable and is shared by
/// the `ls` printer and [`fleet_sessions`].
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

/// Fold a sweep into the listing result — PURE: [`discovery_report`]'s `(report,
/// code)` verdict is the ONE authority on whether a listing succeeded, so the
/// data path and the CLI path agree by construction. A zero code is the success
/// arm (some instance answered); anything else is the failure the caller must be
/// able to tell apart from an empty result.
fn fleet_listing(
    sessions: Vec<FleetSession>,
    verdict: (String, u8),
) -> Result<Vec<FleetSession>, FleetListError> {
    match verdict {
        (_, 0) => Ok(sessions),
        (reason, code) => Err(FleetListError { reason, code }),
    }
}

/// The `windows` rows for one instance, folded from its `sessions` lines — PURE,
/// so the fold is table-testable. One row per window id in ascending order:
/// `<pid> window=<id> focused=<0|1> sessions=<n> active=<sid>[,<sid>…]` (`active=-`
/// when nothing is on the active tab), then `<pid> window=none sessions=<n>` for
/// the sessions no window hosts, then `<pid> window=- sessions=<n>` for the lines
/// that could not say — an older server with no `window=` token, or one whose
/// main thread did not answer (`window=-`). The last two are kept apart on
/// purpose: "detached" is a fact about the session, "could not say" is a fact
/// about the listing, and folding either into a window would be a lie. (A
/// `--headless` instance owns one logical window, id 0, and its sessions fold
/// under `window=0` like any other's; `none` is a session no window holds.)
///
/// The tokens are read by [`roster_tail`] — from the END of the line back to
/// the `meta=` anchor — never by column position. The fifth column is the
/// title: pct-encoded by a codec that leaves `=` alone, settable by any program
/// in the pane, and possibly EMPTY (two spaces, which a whitespace split
/// collapses, so a column count would slide every key one place left). A
/// title spelling `window=7` sits before `meta=` and is never reached; a server
/// that appends more `key=value` columns after `detail=` changes nothing here.
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

/// The additive `key=value` columns of one whitespace-split `sessions` line:
/// walked BACKWARDS from the end until the `meta=` token that closes the
/// positional columns, and EMPTY when that anchor is never met — so nothing
/// before it (the title above all, which any program can set to `window=7`)
/// is ever taken for a column, and a pre-roster server with no `meta=` at all
/// recognises no tail whatever its title spells. Reading from the end is what
/// lets an empty title (two spaces, collapsed by the split) cost nothing.
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
/// instance) were found and none answered, or the directory is missing,
/// unreadable or unresolvable. DISTINCT from [`EXIT_EMPTY_FLEET`], which
/// `ls`/`instances`/`windows` reserve for a readable directory holding no
/// instance socket. Additive over the 0/1/124 contract: still nonzero for
/// `nonzero == failure` scripts.
const EXIT_UNREACHABLE: u8 = 2;

/// Exit code for a fleet that is genuinely EMPTY: the rendezvous directory was
/// read and holds no instance socket and no graph entry. The only status that
/// licenses "no live aterm instances found" — named so the in-process listing
/// ([`FleetListError::is_empty_fleet`]) can ask the same question the shell asks
/// of `$?`.
const EXIT_EMPTY_FLEET: u8 = 1;

/// The `ls`/`instances`/`windows` verdict — PURE, so every line is table-testable: `dir`
/// is what the directory turned out to be, `probes` every socket dialled with
/// its classified outcome, and the result is the stderr message (without the
/// `aterm-ctl: ` prefix [`stderr_line`] adds) plus the exit code. Any probe
/// that `Answered` is the success path: `("", 0)`, nothing to report.
///
/// The fleet-wide claim "no live aterm instances found" comes from exactly ONE
/// arm — a readable directory with nothing in it. Every other arm names the
/// directory or the socket AND the cause, and the found-but-unreached arms end
/// with ONE hint chosen by the dominant cause (ties go to the most actionable).
/// The code is 124 only when EVERY probe timed out — a wedged fleet, not an
/// unreachable one.
fn discovery_report(dir: DirOutcome, probes: &[(u32, String, Probe)]) -> (String, u8) {
    if probes
        .iter()
        .any(|(_, _, p)| matches!(p, Probe::Answered(_)))
    {
        return (String::new(), 0);
    }
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
            return (
                format!(
                    "control-socket directory {shown} exists but cannot be read: {err} \
                     — a sandbox may be refusing readdir(), or the directory belongs to \
                     another user"
                ),
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
            (
                format!("found {n} control socket{plural} in {shown} but could not reach any:"),
                Some(path.as_path()),
            )
        }
        DirOutcome::Scoped => (
            "the addressed instance did not answer (wrong --sock/--pid, not running, \
             or a different user):"
                .to_string(),
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
    if let Some(hint) = dominant_hint(probes, in_dir) {
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
/// socket names (`0` = none). A refused connect is labelled by liveness, and a
/// denied one by errno: `EPERM` (1) is what a seatbelt hands back, so it names
/// the sandbox outright; anything else (`EACCES`, `ENOTCONN`) could as well be
/// another user's socket mode, and says so.
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
            "server rejected the token (ERR auth) — a restarted instance rewrites its token; re-run"
                .to_string()
        }
        Probe::Timeout => "timed out after 2s".to_string(),
        Probe::Other(e) => e.to_string(),
    }
}

/// The failure classes a hint is chosen among, in PRIORITY order: when two
/// classes tie for the most sockets, the earlier one wins because its remedy
/// is the one the reader can act on (a denied connect is fixed by an
/// allowance; a stale socket beside it is fixed by nothing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cause {
    Denied,
    NoToken,
    AuthRejected,
    Timeout,
    Refused,
    Other,
}

const CAUSE_PRIORITY: [Cause; 6] = [
    Cause::Denied,
    Cause::NoToken,
    Cause::AuthRejected,
    Cause::Timeout,
    Cause::Refused,
    Cause::Other,
];

fn cause_of(probe: &Probe) -> Option<Cause> {
    match probe {
        Probe::Answered(_) => None,
        Probe::Refused { .. } => Some(Cause::Refused),
        Probe::Denied(_) => Some(Cause::Denied),
        Probe::NoToken(..) => Some(Cause::NoToken),
        Probe::AuthRejected => Some(Cause::AuthRejected),
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

/// The ONE hint block for the dominant cause. For a denied connect the
/// directories to allow are the parents of the sockets that were actually
/// refused — each named once, in probe order — so the Codex flag can be pasted
/// as printed and covers every refused socket: an explicit-`$ATERM_CONTROL_SOCK`
/// instance publishes its socket outside the fleet directory (discovery finds it
/// through `graph/<sid>`), and a hint that always named the fleet directory left
/// that socket refused after the paste. The fleet directory is only the fallback
/// for a socket path with no parent to name.
fn dominant_hint(probes: &[(u32, String, Probe)], in_dir: Option<&Path>) -> Option<String> {
    let hint = match dominant_cause(probes)? {
        Cause::Denied => {
            let mut dirs: Vec<String> = Vec::new();
            for (_, sock, probe) in probes {
                if !matches!(probe, Probe::Denied(_)) {
                    continue;
                }
                let Some(parent) = Path::new(sock)
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                else {
                    continue;
                };
                let shown = parent.display().to_string();
                if !dirs.contains(&shown) {
                    dirs.push(shown);
                }
            }
            if dirs.is_empty() {
                dirs.push(in_dir.map(|d| d.display().to_string()).unwrap_or_default());
            }
            let flags: Vec<String> = dirs
                .iter()
                .map(|d| format!("--allow-unix-socket \"{d}\""))
                .collect();
            format!(
                "inside Codex CLI run with {} or ask for the command to be escalated;\n        \
                 `aterm ctl --sock <path> sessions` shows one socket's own answer",
                flags.join(" ")
            )
        }
        Cause::NoToken => "the token beside a socket must be readable by THIS user (it is \
                           mode 0600); another user's instance cannot be driven"
            .to_string(),
        Cause::AuthRejected => "re-run once (a restarting instance rewrites its token); if it \
                                persists, `aterm ctl --sock <path> sessions` shows one socket's \
                                own answer"
            .to_string(),
        Cause::Timeout => "no answer within 2s — the instance may be wedged or busy; \
                           `aterm ctl --sock <path> --timeout 30 sessions` waits longer"
            .to_string(),
        Cause::Refused => {
            let booting = probes.iter().any(|(_, _, p)| {
                matches!(
                    p,
                    Probe::Refused {
                        pid_running: Some(true)
                    }
                )
            });
            if booting {
                "a pid that is running but not serving is still starting up (or exited \
                 without cleanup); retry in a moment"
                    .to_string()
            } else {
                "nothing is serving these sockets — leftovers of exited instances; launch \
                 aterm.app (`open -a aterm`) and retry"
                    .to_string()
            }
        }
        Cause::Other => {
            "`aterm ctl --sock <path> sessions` shows one socket's own answer".to_string()
        }
    };
    Some(hint)
}

/// Read the per-launch capability token sitting beside the socket at `path`.
/// A per-instance socket (reached directly or through the `latest` symlink)
/// pairs with its `aterm-<pid>.token`; an explicit socket pairs with its OWN
/// `<name>.token`, so two private instances in one directory keep their own
/// credentials. Returns `None` if unreadable (e.g. a different user, or aterm
/// not running); the connection is then attempted without an `AUTH` line and
/// the server refuses it with `ERR auth`.
fn read_token_for(path: &str) -> Option<String> {
    read_token_at(path).ok()
}

/// [`read_token_for`] with the miss KEPT: which file was looked for, and why
/// it could not be read (absent, another user's 0600, empty). Discovery
/// reports that per socket instead of letting the server's bare `ERR auth`
/// stand in for "the token file is unreadable".
///
/// The candidates are the shared rule's, in ITS order
/// ([`control_socket::token_names_for_sock`]): the per-socket file this build's
/// server writes, then — for an explicit socket only — the legacy shared
/// `aterm.token` an OLDER server wrote for that same socket. Only an ABSENT
/// (or unreadable) file moves on; a per-socket token that exists but is empty
/// fails right there, because it belongs to the instance being dialed and
/// reaching past it for a directory-shared file would be reaching for someone
/// else's credential. A miss always names the PER-SOCKET path, the file this
/// build expects to find.
fn read_token_at(path: &str) -> Result<String, (PathBuf, io::Error)> {
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
        let raw = match std::fs::read_to_string(&token_path) {
            Ok(raw) => raw,
            Err(e) => {
                // Keep the FIRST (per-socket) miss: it is the file this build
                // wants, and the one whose absence a report should name.
                miss.get_or_insert((token_path, e));
                continue;
            }
        };
        let t = raw.trim().to_string();
        return if t.is_empty() {
            Err(miss.unwrap_or_else(|| {
                (
                    token_path,
                    io::Error::new(io::ErrorKind::InvalidData, "token file is empty"),
                )
            }))
        } else {
            Ok(t)
        };
    }
    Err(miss.unwrap_or_else(|| {
        (
            PathBuf::from(path),
            io::Error::new(io::ErrorKind::NotFound, "socket path names no token"),
        )
    }))
}

/// The distinct exit code for a TIMEOUT, additive over the 0=OK / 1=failure
/// convention so existing `nonzero == failure` scripts still treat it as failure.
/// Returned both for a CLIENT-side socket-deadline expiry (see
/// [`is_timeout_error`]) and for a SERVER reply that reports a timeout (a `turn`
/// verdict carrying `status=timeout`, or an `await`/`ready`/`wait` `OK timeout`;
/// see [`reply_is_timeout`]).
const EXIT_TIMEOUT: u8 = 124;

/// Whether an [`io::Error`] is a socket read/write DEADLINE expiry: the per-op
/// timeout firing surfaces as `WouldBlock` (Unix `SO_RCVTIMEO`/`SO_SNDTIMEO`) or
/// `TimedOut` (Windows), NOT as a generic failure — so it maps to [`EXIT_TIMEOUT`]
/// and a wedged server is distinguishable from an `ERR`/usage/connect failure.
fn is_timeout_error(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

/// The whole client as a callable: `argv[1..]` in, process exit code out.
/// The ONE `aterm` binary calls this in-process for `aterm ctl …` (and via
/// the `aterm-ctl` argv0 compat alias); the thin `src/main.rs` bin wraps it
/// for a standalone build. Everything below is unchanged from the binary era.
pub fn main_entry(argv: Vec<std::ffi::OsString>) -> ExitCode {
    match real_main(argv) {
        Ok(code) => code,
        Err(e) => {
            // Manual form of `eprintln!("aterm-ctl: {e}")` (the strict Trust
            // gate cannot lower inline `format_args!`), byte-identical output.
            // A failure to write the diagnostic is ignored — the process is
            // already exiting FAILURE and has nowhere left to report to.
            let _ = stderr_line(&e.to_string());
            // A client-side socket-deadline expiry exits 124 (distinct from a
            // generic failure), matching the server-reported-timeout mapping in
            // `exchange`.
            if is_timeout_error(&e) {
                ExitCode::from(EXIT_TIMEOUT)
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

/// The usage error for an invocation with no verb: `usage: <SYNOPSIS>`,
/// byte-identical to the previous `format!("usage: {SYNOPSIS}")`.
fn usage_error() -> io::Error {
    let mut msg = String::from("usage: ");
    msg.push_str(SYNOPSIS);
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

/// Whether `parts` is a `dial` request WITHOUT a verb to run on the remote —
/// `dial` alone or `dial <name>`, but NOT `dial <name> <verb...>`. A bare dial
/// cannot complete one-shot (the remote never closes the relay on its own), so the
/// client rejects it (see [`dial_needs_verb_error`]) instead of sending a line that
/// would deadlock.
fn dial_missing_verb(parts: &[String]) -> bool {
    parts.first().map(String::as_str) == Some("dial") && parts.len() < 3
}

/// The clear error for a bare `dial <name>` (no verb). The remote is a PERSISTENT
/// control connection that never closes on its own, so a one-shot `dial <name>`
/// would block reading a reply that never comes — the deadlock this rejects.
/// Hand-composed (no inline `format_args!`) like the other static client errors, so
/// the strict Trust gate never has to lower a `format!`. Exits FAILURE (nonzero,
/// not the 124 timeout) via `main`'s [`is_timeout_error`] mapping.
fn dial_needs_verb_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "ERR dial: give a verb to run on the remote (dial <name> <verb...>)",
    )
}

/// The discovery verbs this CLIENT answers, none of which takes an argument —
/// the fleet is the scope, `--sock`/`--pid` narrow it. Read by the dispatch
/// that intercepts them and by [`discovery_given_arguments`].
const DISCOVERY_VERBS: [&str; 3] = ["ls", "instances", "windows"];

/// The discovery verb in `parts` when it carries arguments it has no use for;
/// `None` for a bare one, and for any request that is not a discovery verb. An
/// agent that types `windows 1` — or `ls 1`, `instances --all` — meant something
/// (one window? one instance?) that a silently ignored argument would have let
/// it believe it got; `ls` and `instances` let trailing words through for as
/// long as they existed, which is exactly the hole the house rule names.
/// Rejected up front, before any socket is dialled.
fn discovery_given_arguments(parts: &[String]) -> Option<&'static str> {
    let verb = parts.first().map(String::as_str)?;
    let verb = DISCOVERY_VERBS.iter().copied().find(|v| *v == verb)?;
    (parts.len() > 1).then_some(verb)
}

/// The usage error for a discovery verb given arguments: the synopsis, in the
/// `ERR usage: <verb>` shape every server-side usage error takes. Composed with
/// `push_str` like the other client errors. Exits FAILURE.
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
const CLIENT_HELP_VERBS: [&str; 4] = ["ls", "instances", "windows", "mux"];

/// The `--help` entry for one client verb, as [`CLIENT_VERBS`] prints it: the
/// verb's own line plus its continuation lines (the block's 18-column gutter),
/// up to the next entry or the blank line before the prose — with the 4-column
/// `--help` indent removed, so the rows read like the server's `help <verb>`
/// rows. `None` for a verb the block does not document.
fn client_verb_entry(verb: &str) -> Option<Vec<String>> {
    const HELP_INDENT: &str = "    ";
    const CONTINUATION: &str = "                  ";
    // A verb line is the `--help` indent, the verb padded out to the block's
    // gutter, then its text starting IN the gutter column. A prose line at the
    // same indent (`None of the three takes an argument …`) never pads a word
    // out to the gutter, so it is never taken for a verb — nor is a verb whose
    // name is a prefix of another's.
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

/// The client-side answer to `help <client verb>`; `None` for every other request,
/// which goes to the server as before (`help`, `help text`, `help --full`, and a
/// `help ls extra` the server rejects as `ERR usage`). Framed exactly as the
/// server frames its own `help <verb>` — `OK <n>` + n rows — so the reply prints
/// through the same shape the server's does and reads the same to a parser.
fn client_help_reply(parts: &[String]) -> Option<String> {
    let [verb, name] = parts else {
        return None;
    };
    if verb != "help" || !CLIENT_HELP_VERBS.contains(&name.as_str()) {
        return None;
    }
    let lines = client_verb_entry(name)?;
    let mut reply = format!("OK {}\n", lines.len());
    for line in lines {
        reply.push_str(&line);
        reply.push('\n');
    }
    Some(reply)
}

/// Print a line-framed reply (`OK <n>` + n rows) the way [`exchange`] prints a
/// server's: the rows to stdout, the header consumed. One buffered write, flushed
/// explicitly so a broken pipe surfaces as the error it is.
fn print_framed_lines(reply: &str) -> io::Result<()> {
    let stdout = stdout_handle();
    let mut out = io::BufWriter::new(stdout.lock());
    for line in reply.lines().skip(1) {
        out.write_all(line.as_bytes())?;
        out.write_all(b"\n")?;
    }
    out.flush()
}

/// The command-line arguments (already past `argv[0]`) as UTF-8 strings. The CLI
/// boundary is modeled as OS strings (byte-exact) and converted explicitly:
/// the control protocol frames requests as UTF-8 text, so a non-UTF-8
/// argument is a clean usage error here (`env::args` would panic on it
/// instead). `into_string` goes through the fn-item shape (see
/// `read_token_for`) like every other OS-string conversion.
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

/// Parse arguments, frame the request, talk to the server, and print the reply.
///
/// Returns the process exit code on a completed exchange (`SUCCESS` for `OK`,
/// `FAILURE` for an `ERR`/unexpected status), or an [`io::Error`] for usage or
/// connection problems (surfaced on stderr by [`main`]).
fn real_main(argv: Vec<std::ffi::OsString>) -> io::Result<ExitCode> {
    // Flag parsing stops at the first positional argument: everything from the
    // verb onward is part of the request, so a literal "--sock" inside e.g. a
    // `send`/`search` payload is never mistaken for our own flag.
    let mut args = utf8_args(argv)?.into_iter();
    let mut sock: Option<String> = None;
    let mut pid: Option<u32> = None;
    // The per-op socket read/write deadline. `Some(EXCHANGE_DEADLINE)` is the
    // default; `--timeout SECS` overrides it (0 => `None`, no deadline).
    let mut deadline: Option<std::time::Duration> = Some(EXCHANGE_DEADLINE);
    // Whether `--timeout` was passed EXPLICITLY (vs. the default). `subscribe`
    // needs the distinction: the default watches forever, an explicit value
    // bounds the watch as a wall-clock max — indistinguishable from `deadline`
    // alone, since the default and an explicit `--timeout 900` share a value.
    let mut timeout_explicit = false;
    let mut request_parts: Vec<String> = Vec::new();

    let parse_pid = |v: &str| {
        v.parse::<u32>().map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "--pid requires a numeric PID")
        })
    };
    while let Some(arg) = args.next() {
        if arg == "-h" || arg == "--help" {
            // Recognized only before the first positional verb; a --help that
            // follows a verb is part of the request payload (handled by the
            // `else` branch below, which stops flag parsing). `help_text` already
            // ends in a newline (the last catalog line's), so write it directly
            // rather than through `print_stdout_line` (which appends one).
            let help = help_text();
            let stdout = stdout_handle();
            let mut out = stdout.lock();
            out.write_all(help.as_bytes())?;
            // End the guard explicitly before any later branch-local helper can
            // acquire stdout. The branches are mutually exclusive at runtime;
            // the explicit drop also makes that fact visible to the lexical
            // lock-order census.
            drop(out);
            return Ok(ExitCode::SUCCESS);
        } else if arg == "-V" || arg == "--version" {
            print_stdout_line(&format!("aterm-ctl {}", aterm_types::version::APP_VERSION))?;
            return Ok(ExitCode::SUCCESS);
        } else if arg == "--completions" {
            // Hidden pre-verb flag (kept OUT of `--help`): print the shell
            // completion script for the named shell and exit. Recognized here,
            // before verb dispatch, exactly like `--help`/`--version` — a verb
            // that follows is part of the request, never this flag.
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
        } else {
            // First positional is the verb; the remainder is its argument list.
            request_parts.push(arg);
            request_parts.extend(args.by_ref());
            break;
        }
    }

    // THE MULTIPLEXER BOUNDARY, decided BEFORE `@self` is expanded away — after
    // expansion the selector is a concrete sid and the "implicit target" shape
    // this guards is no longer visible. `mux` is the client-answered report of
    // that decision; everything else gets refused only when its target is named
    // implicitly AND an aterm session identity is in scope to be mis-targeted.
    let nesting = mux_nesting_from_env();
    if request_parts.first().map(String::as_str) == Some("mux") {
        announce_mux_boundary(nesting.as_ref(), true);
        return run_mux_report(nesting.as_ref());
    }
    if let Some(n) = nesting.as_ref()
        && n.outer_sid.is_some()
        && targets_own_session_implicitly(
            &request_parts,
            sock.as_deref(),
            pid,
            env::var(SOCK_ENV).ok().as_deref(),
        )
    {
        announce_mux_boundary(nesting.as_ref(), true);
        return Err(nested_self_drive_error(n));
    }

    // Expand a leading `@self` / `@env` selector to `@$ATERM_PARENT_SESSION_ID`
    // BEFORE anything reads the selector (framing, discovery, stdin routing), so
    // the rest of the path sees only a concrete `@<sid>`.
    expand_self_selector(&mut request_parts, env::var(SELF_SID_ENV).ok())?;

    // SIGNAL #1 — the sentence that says the command blocks just stopped
    // existing. It used to be the shell integration's alone, which meant it was
    // never said at all in a real bash or zsh pane: nothing sources the script
    // there (see [`MUX_ENV`]). A pane cannot say it, so `aterm ctl` does — once
    // per multiplexer, on the first call from inside one, sharing the
    // integration's own stamp so a fish or hand-installed pane that already said
    // it does not say it twice. Read AFTER `@self` expansion so the
    // `blocks`/`status` check below sees the same request the trailing
    // degradation note will.
    announce_mux_boundary(
        nesting.as_ref(),
        nesting
            .as_ref()
            .is_some_and(|n| mux_degradation_note(n, &request_parts).is_some()),
    );

    // `dial <name> <verb...>` runs ONE verb on a saved remote over the relay and
    // reads back the REMOTE's framed reply. A BARE `dial <name>` (or `dial` alone)
    // cannot complete one-shot: the remote is a PERSISTENT control connection that
    // never closes on its own, so the client would block reading a reply that never
    // comes (the deadlock this rejects). Fail clearly BEFORE sending anything.
    if dial_missing_verb(&request_parts) {
        return Err(dial_needs_verb_error());
    }

    let Some(verb) = forwarded_verb(&request_parts) else {
        return Err(usage_error());
    };

    // The cross-terminal discovery verbs are answered by THIS CLIENT — they
    // enumerate EVERY live instance's socket, so no single server connection
    // could answer them. Anything else is one framed request to one socket.
    //
    // `--sock`/`--pid` SCOPE the enumeration (they used to be silently ignored,
    // which is the footgun this closes: an agent that launched an isolated
    // instance under its own `$XDG_RUNTIME_DIR` and then ran `--sock <that> ls`
    // was handed the USER'S REAL terminals and could go on to drive them).
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
    // `help <client verb>` is answered here as well, from the same CLIENT VERBS
    // block `--help` prints — the server's `help` knows only the protocol table
    // and would call `ls` an unknown verb. No socket is needed for it.
    if let Some(reply) = client_help_reply(&request_parts) {
        print_framed_lines(&reply)?;
        return Ok(ExitCode::SUCCESS);
    }

    let path = resolve_path(
        sock,
        pid,
        env::var(SOCK_ENV).ok(),
        env::var(NO_SOCK_ENV).ok(),
        env::var(SELF_SID_ENV).ok(),
    )?;

    // STDIN PAYLOAD PATHS. `feed-bin` (no inline length), and the `send`/`paste`
    // `--stdin`/`-` forms, all deliver the RAW stdin bytes to the PTY through the
    // one wire form that carries a body with embedded newlines exactly — the
    // length-prefixed `feed-bin` binary frame — instead of the argv-joined,
    // newline-guarded, whitespace-collapsing request line. This is what makes an
    // EXACT multi-line payload sendable at all (the line-delimited request framing
    // would otherwise split it, and `validate_request_parts` would reject it).
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
        return feed_bin_exchange(&path, "operator-propose-bin", &payload, deadline);
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
        // `send --stdin` is a RAW feed (`feed-bin`); `paste --stdin` routes the
        // bytes through the server's PASTE seam (`paste-bin` — bracketed-paste +
        // control-byte sanitize, mirroring the inline `paste` verb), so a
        // multi-line paste keeps paste semantics instead of degrading to a raw feed.
        let frame_verb = if verb_tok == Some("paste") {
            "paste-bin"
        } else {
            "feed-bin"
        };
        return feed_bin_exchange(
            &path,
            &binary_frame_prefix(selector, frame_verb),
            &payload,
            deadline,
        );
    }

    // Reject line terminators before framing: the socket is newline-delimited,
    // so an embedded '\n'/'\r' would inject a second authenticated verb.
    validate_request_parts(&request_parts)?;

    // One request per line: "VERB [args...]\n". Args are joined with single
    // spaces; for `send`/`search` this reconstructs the free-form rest-of-line
    // payload (modulo collapsed inter-arg whitespace).
    let mut request = request_parts.join(" ");
    request.push('\n');

    // For `dial <name> <verb...>` the reply we read is the REMOTE's answer to
    // `<verb...>`, so framing must follow the forwarded SUB-request (`text`,
    // `cursor`, `image read`, `cast frames`, `text --json`), NOT the `dial <name>
    // …` line we send to the local relay. Everything else frames on the request
    // it sends (unchanged). `verb` (from `forwarded_verb`) is already the
    // forwarded verb token for both paths.
    let frame_request = if request_parts.first().map(String::as_str) == Some("dial") {
        request_parts[2..].join(" ")
    } else {
        request.clone()
    };

    let outcome = exchange(
        &path,
        &request,
        &verb,
        &frame_request,
        deadline,
        timeout_explicit,
    );
    // AFTER the reply, so the caveat lands under the (necessarily empty) result
    // it explains rather than ahead of it. A failure to write the note is
    // ignored — the exchange's own outcome is what this call is for.
    if let Some(note) = nesting
        .as_ref()
        .and_then(|n| mux_degradation_note(n, &request_parts))
    {
        let _ = stderr_line(&note);
    }
    outcome
}

/// Split an optional leading `@<selector>` proxy token off the request parts,
/// returning `(selector, rest)` where `rest` starts at the verb. The selector
/// (`@<sid>` / `@.` / `@self`) routes a stdin-payload frame to a peer session
/// exactly as it does an ordinary verb line. `@self` is expanded to a concrete
/// `@<sid>` upstream (see [`expand_self_selector`]) before this sees it.
fn split_selector(parts: &[String]) -> (Option<&str>, &[String]) {
    match parts.first() {
        Some(s) if s.starts_with('@') => (Some(s.as_str()), &parts[1..]),
        _ => (None, parts),
    }
}

/// A binary-frame request head (WITHOUT the length): `<verb>`, or `@<sel> <verb>`
/// when a proxy selector routes the frame to a peer. `<verb>` is the raw feed
/// (`feed-bin`, for `feed-bin` / `send --stdin`) or the paste-seam feed
/// (`paste-bin`, for `paste --stdin`). The caller appends ` <len>\n` + the body.
fn binary_frame_prefix(selector: Option<&str>, verb: &str) -> String {
    let mut s = String::new();
    if let Some(sel) = selector {
        s.push_str(sel);
        s.push(' ');
    }
    s.push_str(verb);
    s
}

/// Expand `@self` / `@env` selectors to a concrete `@<sid>` from
/// `$ATERM_PARENT_SESSION_ID` (passed as `self_sid`) BEFORE the request is framed
/// — a first-class "my own terminal" selector, stable across the human's tab
/// switches, unlike flagless / `@.` which follow the ACTIVE tab. A pure string
/// expansion: `@<sid>` and `@.` pass through untouched. Errors clearly when the
/// env var is unset/empty (not inside an aterm session); `parts` is then left as-is.
///
/// Expands in EVERY position a selector token can appear — the leading proxy
/// selector (`@self screen`), and `subscribe`'s selector-SECOND form
/// (`subscribe @self screen,events`, the primary live-watch verb) — and inside a
/// comma-separated selector LIST (`subscribe @self,@1 screen`). Without covering the
/// subscribe position, `@self` reached the server verbatim and resolved to a
/// nonexistent sid (`ERR no such session`) — so there was no working `@self`
/// live-watch at all.
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

/// The clear error for `@self` / `@env` outside an aterm session (the env var the
/// selector expands from is unset). A static message, like the other hand-composed
/// errors (the strict Trust gate cannot lower inline `format_args!`).
fn self_selector_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "ERR @self: not inside an aterm session ($ATERM_PARENT_SESSION_ID unset)",
    )
}

/// Parse `--timeout <secs>` into the per-op socket deadline: `0` disables the
/// deadline (`None`), any other non-negative integer is that many SECONDS. A
/// negative or non-numeric value is a clean usage error (a static message, no
/// inline `format_args!`).
fn parse_timeout(v: &str) -> io::Result<Option<std::time::Duration>> {
    let secs: u64 = v.parse().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "--timeout requires a non-negative integer number of SECONDS (0 = no deadline)",
        )
    })?;
    Ok((secs != 0).then(|| std::time::Duration::from_secs(secs)))
}

/// The maximum `feed-bin` payload the server accepts in one binary frame
/// (256 KiB, mirroring the server's `MAX_FEED_BIN`). The stdin payload paths
/// refuse a larger body up front with a clear error rather than pipelining bytes
/// a server would refuse to read — which would desync the frame and drop the
/// connection.
const MAX_FEED_BIN: usize = 256 * 1024;

/// The clear error for a `feed-bin` invocation that carries an inline argument
/// (e.g. a bare length): the client sources the binary payload from STDIN, so an
/// inline token has no path and would only desync the frame. A static message
/// (no inline `format_args!`), like the other hand-composed errors.
fn feed_bin_inline_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "feed-bin reads its binary payload from stdin: pipe the bytes in \
         (e.g. `printf ... | aterm-ctl feed-bin`); no inline length is accepted",
    )
}

/// The clear error for a stdin payload larger than the server's 256 KiB
/// `feed-bin` cap — refused BEFORE any bytes reach the server (a static message,
/// no inline `format_args!`).
fn oversized_feed_bin_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "stdin payload exceeds the 256 KiB feed-bin cap; split it into smaller frames",
    )
}

/// Read the entire binary payload from stdin for a length-prefixed `feed-bin`
/// frame. Bounded by [`MAX_FEED_BIN`] + 1 so an over-cap body is REJECTED here
/// (a clear client error) instead of being pipelined to a server that would
/// refuse to read it and drop the connection. Returns the raw bytes verbatim.
fn read_stdin_payload() -> io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    // `take(cap + 1)` lets us DETECT an over-cap body without reading an
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

/// Whether a server status line reports a TIMEOUT outcome: a bare `OK timeout`
/// (the `await`/`ready`/`wait`/selection-wait timeout reply) or a `turn` verdict
/// carrying `status=timeout`. Pure, so the [`EXIT_TIMEOUT`] mapping is
/// unit-testable without a socket. An `ERR ...` line is never a timeout here (a
/// connect/usage/ERR failure stays exit 1).
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

/// Deliver a length-prefixed `feed-bin` binary frame carrying `payload` to the
/// resolved target's PTY. `prefix` is the request head WITHOUT the length —
/// `feed-bin` or `@<sel> feed-bin`. This is the wire form the stdin payload paths
/// use (`feed-bin`, `send --stdin`, `paste --stdin`): the ONLY server frame that
/// carries a body with embedded newlines exactly, without the line-delimited
/// request framing splitting it (and without the argv newline guard rejecting
/// it). Authenticates transparently like [`exchange`], runs under the same
/// per-op `deadline`, and prints the server's `OK <n> bytes` reply (an `ERR`
/// goes to stderr and exits FAILURE).
fn feed_bin_exchange(
    path: &str,
    prefix: &str,
    payload: &[u8],
    deadline: Option<std::time::Duration>,
) -> io::Result<ExitCode> {
    let path = &aterm_uds::latest::resolve(path);
    let stream = connect_stream(path)?;
    stream.set_read_timeout(deadline)?;
    stream.set_write_timeout(deadline)?;
    // `AUTH <token>\n` (transparent) + `<prefix> <len>\n`, then the raw body.
    let mut head = String::from(prefix);
    head.push(' ');
    head.push_str(&payload.len().to_string());
    head.push('\n');
    send_request(&stream, read_token_for(path).as_deref(), &head)?;
    (&stream).write_all(payload)?;
    (&stream).flush()?;

    let mut reader = BufReader::new(&stream);
    let mut status_line = String::new();
    if read_bounded_line(&mut reader, &mut status_line)? == 0 {
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
}

/// Reject request arguments that carry a line terminator. The control socket is
/// strictly newline-delimited — the server reads one verb per line with
/// `read_line` — so an embedded `\n` (or `\r`) in an argument would frame a
/// SECOND, already-authenticated verb after the intended one. Callers pass the
/// raw argv parts here before joining them into the request line.
fn validate_request_parts(parts: &[String]) -> io::Result<()> {
    if parts.iter().any(|p| p.contains('\n') || p.contains('\r')) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "arguments must not contain newline/carriage-return (the control socket is line-delimited)",
        ));
    }
    Ok(())
}

/// The verb that determines response framing (whether to read `OK <n>` follow-up
/// lines, and `image read` detection), skipping an optional leading PROXY SELECTOR
/// (`@<sid>` / `@.`; `@self` is already expanded upstream). Without this, a
/// proxied `@child screen` is treated as
/// verb `@child` and the client never reads the payload the relay faithfully
/// delivers. `None` only for an empty request.
fn forwarded_verb(parts: &[String]) -> Option<String> {
    match parts.first() {
        // `dial <name> [@<sid>] <verb...>`: the reply is the REMOTE's answer to
        // `<verb>`, so response framing follows the forwarded verb token — skipping an
        // optional leading REMOTE SELECTOR exactly like the local `@sel` arm below.
        // Without the skip, `dial host @s-remote text` framed on "@s-remote" → Status
        // and silently dropped every payload row of the documented "drive a specific
        // remote session" workflow. A bare `dial <name>` is rejected upstream.
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

/// The follow-up line count from a streaming verb's `OK <count>` header `tail`
/// (everything after the first space). Only the FIRST whitespace token is the
/// count: the server may append a free-form marker after it — `cmd_search`
/// writes `OK <n> incomplete` when scrollback was evicted (control_query.rs) —
/// and parsing the whole tail would reject that header and drop every match.
/// Mirrors aterm-nest's `verb_stream` framing (first token = count, then n lines).
///
/// Capped at [`MAX_STREAM_LINES`], fail-closed (`None`, like a malformed header):
/// the count is PEER-supplied and discovery dials every socket in the shared dir,
/// so an unclamped value would let a hostile same-user socket size our
/// `Vec::with_capacity` with `OK <huge>` (allocation abort before any body read).
fn stream_count(tail: &str) -> Option<usize> {
    let count: usize = tail.split_whitespace().next()?.parse().ok()?;
    (count <= MAX_STREAM_LINES).then_some(count)
}

/// Upper bound on a peer-supplied `OK <n>` line count (mirrors aterm-agent's
/// `MAX_TEXT_LINES` bound on the same wire framing).
const MAX_STREAM_LINES: usize = 200_000;

/// Whether `verb` answers `OK <nbytes>\n` followed by an `<nbytes>`-byte BODY
/// (byte-framed, not line-framed): `cast` streams the session's asciicast v2
/// recording, `temporal` the reconstructed past-instant screen text. CLIENT-1:
/// before this the client printed only the `OK <nbytes>` header and DISCARDED
/// the body — the exact invocations the CHANGELOG advertises produced nothing.
fn bytes_payload(verb: &str, request: &str) -> bool {
    // Single-sourced (see `streams_payload`): only the BARE `cast`/`temporal`
    // bodies are byte-framed; `cast frames` is line-framed, handled by framing_of.
    aterm_types::control_verbs::framing_of(verb, request)
        == aterm_types::control_verbs::Framing::Bytes
}

/// The body length from a byte-framed verb's `OK <nbytes>` header `tail`. Like
/// [`stream_count`], only the FIRST whitespace token is the length (a future
/// marker after it must not reject the header), and the value is PEER-supplied —
/// cap it at [`MAX_BODY_BYTES`], fail-closed (`None`, like a malformed header),
/// so a hostile same-user socket cannot size an unbounded read with `OK <huge>`.
fn byte_count(tail: &str) -> Option<usize> {
    let n: usize = tail.split_whitespace().next()?.parse().ok()?;
    (n <= MAX_BODY_BYTES).then_some(n)
}

/// Upper bound on a peer-supplied byte-framed body (`cast`/`temporal`). Sized
/// far above the largest LEGITIMATE body — the cast recorder is bounded
/// drop-oldest server-side and a temporal body is one screen of text — while
/// still bounding a hostile `OK <huge>` header. The body is STREAMED through a
/// `Read::take` copy (never allocated whole), so this caps wall-clock/output,
/// not memory.
const MAX_BODY_BYTES: usize = 256 << 20; // 256 MiB

/// Upper bound on a single reply line's bytes. Replies are read with
/// `read_line`-style accumulation, which grows its buffer until a `\n`
/// arrives — so a regressed or hostile server that streams bytes WITHOUT a
/// newline would otherwise grow that String until the machine dies (the
/// read-to-EOF-on-`/dev/urandom` failure mode, replayed on the client side).
/// Enforced by [`read_bounded_line`] on EVERY line this binary reads from a
/// control socket.
///
/// SIZED to the largest LEGITIMATE single reply line, not to aterm-agent's
/// 1 MiB (its `RelayClient` never reads the big verbs): `screen` returns the
/// whole styled frame as ONE JSON line (~193 B per blank cell — ~4.5 MiB for
/// a 240×96 grid), and `image read` returns base64 of the server's 4 MiB raw
/// frame cap, ~5.33 MiB on one line. 8 MiB clears both with headroom while
/// still bounding a hostile stream to one small allocation.
const MAX_LINE_BYTES: usize = 8 << 20; // 8 MiB

/// Hard deadline on each socket read/write in [`exchange`]. This CANNOT be
/// [`probe_lines`]' 2 s discovery deadline: the blocking verbs (`await`,
/// `ready`, `wait`) legitimately hold their reply until the server-side
/// timeout, and the server clamps that timeout to 600 000 ms
/// (control_session.rs / control_selection.rs); `update check` is answered
/// synchronously too, with its own curl budget of 30 s (API `--max-time`) +
/// 600 s (download `--max-time`) plus verify/stage disk work. So the client
/// deadline sits ABOVE the worst legitimate synchronous verb (~650 s), with
/// margin. It exists so a SILENTLY wedged server stalls aterm-ctl at most one
/// deadline per read — a byte-trickling peer instead runs into the
/// [`MAX_LINE_BYTES`] accumulation cap, which bounds memory and total bytes
/// (not wall-clock); between the two, never unbounded on either axis.
const EXCHANGE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(900);

// NOTE on the helper decomposition below: the strict Trust gate fails closed
// on inline `format_args!` expansions (`format!` / `println!` / `eprintln!` /
// `writeln!` embed a `fmt::Arguments` construction it cannot lower) and on
// functions big enough to blow its VC-generation budget. `exchange` is
// therefore split into small, structurally panic-free helpers, and every
// user-visible string is composed with `push_str`/`write_all` (byte-identical
// output; `Display` rendering happens behind opaque `to_string` calls).

/// The "connect <path>: <cause>" connection error, preserving the original
/// [`io::ErrorKind`]. `NotFound` and `ConnectionRefused` on a control socket
/// both mean ONE operator-visible thing — no aterm engine is serving that
/// socket — so those two carry [`NOT_RUNNING_HINT`] on the line the user
/// actually reads, instead of a bare `No such file or directory (os error 2)`.
/// Every other kind keeps the raw cause alone: a permission error is NOT "not
/// running", and claiming so would send the operator to the wrong fix.
fn connect_error(path: &str, e: &io::Error) -> io::Error {
    let mut msg = String::from("connect ");
    msg.push_str(path);
    msg.push_str(": ");
    msg.push_str(&e.to_string());
    if matches!(
        e.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    ) {
        msg.push_str(NOT_RUNNING_HINT);
    }
    io::Error::new(e.kind(), msg)
}

/// The remedy [`connect_error`] appends when nothing serves the socket. A
/// SESSION serves no control socket (`aterm --help`): the introspectable
/// engine is the window/headless mode, so the fix is launching the app.
const NOT_RUNNING_HINT: &str = " — aterm isn't running (nothing is serving this \
control socket); launch aterm.app (`open -a aterm`) and retry";

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

/// `BufRead::read_line` with the [`MAX_LINE_BYTES`] accumulation cap: read one
/// `\n`-terminated line from `reader`, append it to `line`, and return the
/// bytes read (`0` = EOF, exactly like `read_line`). The success path is
/// byte-identical to `read_line` — trailing `\n` kept, a partial final line at
/// EOF returned as-is, invalid UTF-8 rejected with std's exact message and
/// `line` left untouched — but a line that runs past the cap without a newline
/// fails with [`oversized_reply_error`] instead of growing the String
/// unboundedly. Bytes are accumulated raw and UTF-8-checked once at the end,
/// so an over-long line ALWAYS reports "oversized", never a spurious UTF-8
/// error from the cap slicing a multi-byte char in half.
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

/// Connect to the control socket at `path`, wrapping a failure in
/// [`connect_error`] so the surfaced message names the path tried.
fn connect_stream(path: &str) -> io::Result<CtlStream> {
    match CtlStream::connect(path) {
        Ok(stream) => Ok(stream),
        Err(e) => Err(connect_error(path, &e)),
    }
}

/// Send the transparent `AUTH <token>\n` line (when a token is readable) and
/// the request line, then flush. The auth line is framed as raw bytes —
/// exactly the bytes the old `format!("AUTH {token}\n")` produced.
fn send_request(mut stream: &CtlStream, token: Option<&str>, request: &str) -> io::Result<()> {
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

/// Confirm that a guarded artifact response was consumed in full. The server
/// releases its exact path/handle retention only after receiving this frame.
///
/// Servers predating the acknowledgement trailer leave the persistent socket
/// open after the ordinary response. Bound the optional trailer read so a new
/// client remains compatible with them. A new server writes the response and
/// challenge in one flush; if a relay delays that trailer past this bound, the
/// client safely falls back to close and the server's failed-ACK quarantine.
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

/// Write `"aterm-ctl: <msg>\n"` to stderr — the manual form of the previous
/// `eprintln!("aterm-ctl: {msg}")`, byte-identical on success. (On a broken
/// stderr this propagates the error instead of panicking; either way the
/// process exits non-zero without completing.)
fn stderr_line(msg: &str) -> io::Result<()> {
    let stderr = io::stderr();
    let mut err = stderr.lock();
    err.write_all(b"aterm-ctl: ")?;
    err.write_all(msg.as_bytes())?;
    err.write_all(b"\n")
}

/// The stdout handle, with aterm-ctl's EXPLICIT process-signal policy:
/// SIGPIPE stays at Rust's startup default (ignored), so a broken pipe never
/// kills the process asynchronously — it surfaces as an [`io::Error`] from
/// `write_all`, which every caller here propagates (the process then exits
/// non-zero via `main`). `io::stdout` is PASSED AS A FUNCTION ITEM to a
/// combinator (see `read_token_for`) so the strict gate keeps it a plain
/// opaque cross-crate call instead of inlining std's startup-semantics
/// internals it cannot prove.
#[allow(
    clippy::unnecessary_literal_unwrap,
    reason = "io::stdout is passed as a fn-item to a combinator so the strict Trust gate keeps it an opaque cross-crate call instead of inlining std's startup-semantics internals it cannot lower"
)]
fn stdout_handle() -> io::Stdout {
    None::<io::Stdout>.unwrap_or_else(io::stdout)
}

/// Print one line to stdout — the manual form of `println!("{line}")`,
/// byte-identical output. Errors propagate as [`io::Error`] (consistent with
/// the streaming-payload path, which always propagated them).
fn print_stdout_line(line: &str) -> io::Result<()> {
    let stdout = stdout_handle();
    let mut out = stdout.lock();
    out.write_all(line.as_bytes())?;
    out.write_all(b"\n")
}

/// Whether `verb` streams `<n>` follow-up data lines after its `OK <n>` header,
/// rather than answering with a single status line. The framing is defined ONCE in
/// `aterm_types::control_verbs` (shared with the server that produces the reply),
/// so this is a thin projection of that single source — the client and server can
/// never disagree on which verbs stream lines. Selector-aware sub-forms (`image
/// read`, `cast frames`) are handled there too.
fn streams_payload(verb: &str, request: &str) -> bool {
    // Single-sourced: the reply framing is defined once in aterm-types
    // (`control_verbs::framing_of`), shared with the server, so the client and
    // server can never disagree on which verbs stream `OK <n>` + n lines.
    aterm_types::control_verbs::framing_of(verb, request)
        == aterm_types::control_verbs::Framing::Lines
}

/// Copy EXACTLY `nbytes` of byte-framed body from `reader` to `out`, verbatim
/// (no line normalization — a `cast` body feeds `asciinema play -` byte-exact).
/// Returns the bytes actually copied: fewer than `nbytes` means the server hung
/// up mid-body (the caller reports the truncation and fails). Bounded by the
/// `take`, so a `MAX_BODY_BYTES`-capped header can never read past the body.
fn copy_body<R: io::Read, W: Write>(reader: &mut R, out: &mut W, nbytes: u64) -> io::Result<u64> {
    io::copy(&mut reader.take(nbytes), out)
}

/// Stream exactly `count` payload lines from `reader` to stdout, normalizing
/// line endings while preserving each row's content verbatim. Early EOF is an
/// error so a guarded artifact reply is never acknowledged while truncated.
fn print_payload(reader: &mut BufReader<&CtlStream>, count: usize) -> io::Result<()> {
    let stdout = stdout_handle();
    // `io::Stdout` is a `LineWriter` unconditionally (not TTY-dependent), so the
    // trailing `\n` below drained the buffer to the fd on EVERY row — one
    // `write(2)` per payload line, with `count` peer-supplied and capped only by
    // `MAX_STREAM_LINES` (200_000). This is the print path for every line-framed
    // verb (`text`, `search`, `selection`, `chrome`, `controls`, `history`,
    // `image read`, `cast frames`) — exactly what an agent drive loop calls per
    // turn. Batching is invisible here because the payload is a bounded one-shot
    // body: nothing reads our stdout mid-payload, and the explicit `flush()`
    // below (NOT BufWriter's Drop) keeps a broken-pipe/ENOSPC error propagating
    // to the caller. Deliberately NOT applied to `subscribe_watch`, which
    // flushes per frame for liveness by design.
    let mut out = io::BufWriter::new(stdout.lock());
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

/// Guarded line replies are currently `video frames`, whose server-side maximum
/// is 64 short metadata/path rows. Read the complete wire frame before ACK so
/// acknowledgement never depends on a potentially backpressured stdout pipe.
/// The aggregate cap also prevents a compromised/skewed server from turning
/// that small handoff buffer into an unbounded allocation.
const MAX_GUARDED_ARTIFACT_LINES: usize = 64;
const MAX_GUARDED_ARTIFACT_BYTES: usize = 4 * 1024 * 1024;

fn read_guarded_payload(
    reader: &mut BufReader<&CtlStream>,
    count: usize,
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
        if total > MAX_GUARDED_ARTIFACT_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "guarded artifact response exceeds its 4 MiB bound",
            ));
        }
        let line = line.strip_suffix('\n').unwrap_or(&line);
        let line = line.strip_suffix('\r').unwrap_or(line);
        lines.push(line.to_string());
    }
    Ok(lines)
}

fn print_guarded_payload(lines: &[String]) -> io::Result<()> {
    let stdout = stdout_handle();
    let mut out = stdout.lock();
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

/// Consume a complete guarded frame and acknowledge it before returning any
/// material to the user-output path. This ordering makes server handoff
/// independent of stdout backpressure or a suspended downstream process.
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
        let lines = read_guarded_payload(reader, count)?;
        acknowledge_artifact_reply(stream, reader, verb, request)?;
        Ok(GuardedArtifactOutput::Lines(lines))
    } else {
        acknowledge_artifact_reply(stream, reader, verb, request)?;
        Ok(GuardedArtifactOutput::Status)
    }
}

/// Connect to `path`, AUTHENTICATE, send `request`, and print the response for
/// `verb`.
///
/// `frame_request` is the request string whose shape decides the RESPONSE framing
/// (`framing_of`): normally the same line as `request`, but for `dial <name>
/// <verb...>` it is the forwarded SUB-request (`<verb...>`) — the reply on the wire
/// is the remote's answer to `<verb...>`, so `image read` / `cast frames` / `--json`
/// sub-forms frame correctly even though the line SENT is `dial <name> …`.
///
/// The server requires `AUTH <hex>\n` as the first line of every connection.
/// We read the token from the file beside the socket (its own `<name>.token`,
/// or `aterm-<pid>.token` for a per-instance socket) and send it
/// transparently; only then do we send the actual request line. There is no
/// server response to the `AUTH` line itself (it is consumed silently on
/// success), so the first line we read back is the response to `request`.
///
/// The whole conversation runs under `deadline` (default [`EXCHANGE_DEADLINE`],
/// or whatever `--timeout SECS` set — `None` disables it) and every reply line
/// under the [`MAX_LINE_BYTES`] cap, so a wedged or regressed server surfaces as
/// a clear error instead of an indefinite stall or an unboundedly growing String
/// — the same defenses [`probe_lines`] applies to discovery.
///
/// `timeout_explicit` distinguishes a user-supplied `--timeout` from the default:
/// it matters only for `subscribe`, whose default watches FOREVER but whose
/// explicit `--timeout` is a max-watch wall-clock bound (see [`subscribe_watch`]).
fn exchange(
    path: &str,
    request: &str,
    verb: &str,
    frame_request: &str,
    deadline: Option<std::time::Duration>,
    timeout_explicit: bool,
) -> io::Result<ExitCode> {
    // The `latest` alias is a symlink on Unix (identity here — the kernel
    // resolves it during connect) and a pointer FILE on Windows; resolve it
    // client-side so both platforms dial the live instance socket.
    let path = &aterm_uds::latest::resolve(path);
    let stream = connect_stream(path)?;

    // Bound every socket operation, as `probe_lines` already does for
    // discovery: a wedged server (or one that stalls mid-reply) must surface
    // as a timeout error, never hang aterm-ctl forever. The default deadline is
    // [`EXCHANGE_DEADLINE`], not discovery's 2 s — it has to clear the blocking
    // verbs' 600 s server-side clamp; `--timeout` overrides it (0 => `None`).
    stream.set_read_timeout(deadline)?;
    stream.set_write_timeout(deadline)?;

    // `&CtlStream` implements both `Read` and `Write`, so the two borrows can
    // coexist: send the auth line + request, then buffer-read the response.
    send_request(&stream, read_token_for(path).as_deref(), request)?;

    let mut reader = BufReader::new(&stream);
    let mut status_line = String::new();
    if read_bounded_line(&mut reader, &mut status_line)? == 0 {
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
        // "ERR <msg>", "ERR", or any unexpected reply: report and fail.
        stderr_line(status_line)?;
        return Ok(ExitCode::FAILURE);
    }

    // A SERVER-reported timeout is an OK reply that still exits 124 (distinct
    // from success): a bare `OK timeout` (await/ready/wait) or a `turn` verdict
    // carrying `status=timeout`. The lines/status are still printed as usual; only
    // the exit code changes. (The subscribe/bytes branches never carry a timeout.)
    let timed_out = reply_is_timeout(status_line);

    if verb == "subscribe" {
        // `OK subscribe <n>` acknowledged — the connection is PUSH-ONLY from here
        // (CLIENT-1: there was previously no client at all for the push face). The
        // ack goes to STDERR so stdout carries the DELTA/EVENT/GAP/BYTES frames
        // VERBATIM (byte-exact — `cells`/`bytes` frames embed byte bodies) for a
        // parser/pipe.
        stderr_line(status_line)?;
        // A subscription legitimately idles between frames, so the DEFAULT clears
        // the read deadline and watches forever (session exit / app quit / user
        // interrupt ends it). An EXPLICIT `--timeout` instead bounds the watch as
        // a WALL-CLOCK max — flush frames so far, exit 124 — while an explicit 0
        // (`deadline == None`) still watches forever, like the default.
        match deadline {
            Some(watch) if timeout_explicit => return subscribe_watch(&stream, &mut reader, watch),
            _ => {}
        }
        stream.set_read_timeout(None)?;
        let stdout = stdout_handle();
        let mut out = stdout.lock();
        io::copy(&mut reader, &mut out)?;
        return Ok(ExitCode::SUCCESS);
    }

    if bytes_payload(verb, frame_request) {
        // `OK <nbytes>` + an nbytes BYTE body (cast/temporal). Streamed to stdout
        // through a bounded `take` copy — byte-exact (no line normalization; a
        // cast body is consumed by `asciinema play -`), never allocated whole.
        let nbytes = match byte_count(tail) {
            Some(n) => n,
            None => return Err(malformed_header_error(status_line)),
        };
        // Mirror the zero-count stderr hint below: a silent empty body is
        // indistinguishable from a failed call.
        if nbytes == 0 {
            let mut msg = String::from(status_line);
            msg.push_str(" (");
            msg.push_str(verb);
            msg.push_str(": empty body)");
            stderr_line(&msg)?;
        }
        let stdout = stdout_handle();
        let mut out = stdout.lock();
        let copied = copy_body(&mut reader, &mut out, nbytes as u64)?;
        if copied < nbytes as u64 {
            // Header promised more than arrived: print what we have (already
            // written) but FAIL, so a truncated recording is never mistaken
            // for a complete one.
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

    // A guarded response is first consumed completely from the socket, then
    // causally acknowledged, and only then copied to user-facing stdout. This
    // keeps a blocked/suspended stdout consumer from exhausting the server's ACK
    // window even though the client has already received the complete frame.
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
        // `search` appends ` incomplete` after the count when scrollback was
        // evicted; the count above already ignores it, but surface the truncation
        // signal so callers know the result set is not exhaustive.
        if tail.split_whitespace().nth(1) == Some("incomplete") {
            stderr_line("results may be incomplete (scrollback evicted)")?;
        }
        // `turn` carries its exchange verdict after the count (`turn submitted=…
        // status=… seq=…`): stdout stays pure screen rows for parsers, the verdict
        // goes to stderr so a driver can still see whether the submit verifiably
        // landed and whether the reply settled or timed out.
        if tail.split_whitespace().nth(1) == Some("turn") {
            let verdict: Vec<&str> = tail.split_whitespace().skip(1).collect();
            stderr_line(&verdict.join(" "))?;
        }
        // A content verb prints its lines to stdout; a ZERO-count reply would otherwise
        // be SILENT — indistinguishable from a failed/hung call. Surface the OK count on
        // STDERR so a human/AI sees "worked, no results" while stdout stays clean for
        // parsers (the empty-response ambiguity that made introspection hard to trust).
        // Built by hand (byte-identical to the previous
        // `eprintln!("aterm-ctl: {status_line} ({verb}: no results)")`) so the
        // strict Trust gate never sees an inline `format_args!`.
        if count == 0 {
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

/// Relay a `subscribe` push stream to stdout under an EXPLICIT `--timeout` treated
/// as a max-watch WALL-CLOCK deadline: watch at most `watch`, then flush the frames
/// received so far and exit [`EXIT_TIMEOUT`]. Unlike a socket read timeout (an
/// INACTIVITY bound a chatty session would keep resetting forever), this bounds
/// TOTAL watch time. The read timeout is set to a short POLL interval — not the
/// remaining time — so a near-deadline tick never rounds to a zero (== infinite)
/// SO_RCVTIMEO, and the wall clock is re-checked at least every poll; a `--timeout`
/// is whole seconds, so the sub-poll overshoot is negligible. Server hang-up (EOF)
/// exits SUCCESS with whatever was streamed.
fn subscribe_watch(
    stream: &CtlStream,
    reader: &mut BufReader<&CtlStream>,
    watch: std::time::Duration,
) -> io::Result<ExitCode> {
    let start = std::time::Instant::now();
    let poll = std::time::Duration::from_millis(250);
    stream.set_read_timeout(Some(poll))?;
    let stdout = stdout_handle();
    let mut out = stdout.lock();
    let mut buf = [0u8; 8192];
    loop {
        if start.elapsed() >= watch {
            out.flush()?;
            return Ok(ExitCode::from(EXIT_TIMEOUT));
        }
        match reader.read(&mut buf) {
            Ok(0) => {
                out.flush()?;
                return Ok(ExitCode::SUCCESS); // server hung up
            }
            Ok(n) => {
                out.write_all(&buf[..n])?;
                out.flush()?; // push each frame promptly (the loop may block next)
            }
            // A poll-interval read timeout is not an error here: loop back and
            // re-check the wall clock. Any OTHER io error ends the watch.
            Err(ref e) if is_timeout_error(e) => {}
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operator_proposal_limit_matches_the_server_protocol_bound() {
        let limit = aterm_types::control_verbs::MAX_OPERATOR_PROPOSAL_BYTES;
        assert!(validate_operator_proposal_size(limit).is_ok());
        let error = validate_operator_proposal_size(limit + 1).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("65536-byte limit"));
    }

    /// CLIENT-1: `cast`/`temporal` are BYTE-framed (`OK <nbytes>` + body) — gated by
    /// verb so their bodies stream to stdout instead of being DISCARDED (the old
    /// behavior printed only the header); everything else keeps its framing.
    #[test]
    fn bytes_payload_gates_cast_and_temporal() {
        assert!(bytes_payload("cast", "cast"));
        assert!(bytes_payload("temporal", "temporal"));
        // `cast frames` is line-framed, not byte-framed.
        assert!(!bytes_payload("cast", "cast frames"));
        assert!(streams_payload("cast", "cast frames"));
        for verb in [
            "text",
            "screen",
            "subscribe",
            "image",
            "send",
            "lines",
            "copy",
        ] {
            assert!(!bytes_payload(verb, verb), "{verb} is not byte-framed");
        }
    }

    /// `byte_count` takes the FIRST token (a future trailing marker must not reject
    /// the header, mirroring `stream_count`), rejects garbage, and caps the
    /// PEER-supplied length fail-closed at `MAX_BODY_BYTES`.
    #[test]
    fn byte_count_first_token_and_cap() {
        assert_eq!(byte_count("1234"), Some(1234));
        assert_eq!(byte_count("0"), Some(0));
        assert_eq!(byte_count("77 incomplete"), Some(77));
        assert_eq!(byte_count(""), None);
        assert_eq!(byte_count("x"), None);
        assert_eq!(
            byte_count(&MAX_BODY_BYTES.to_string()),
            Some(MAX_BODY_BYTES)
        );
        assert_eq!(
            byte_count(&(MAX_BODY_BYTES as u64 + 1).to_string()),
            None,
            "cap is fail-closed"
        );
    }

    /// `copy_body` is byte-exact and honest about truncation: a full body copies
    /// VERBATIM (no line normalization — asciinema consumes it raw), a mid-body
    /// hang-up yields the short count (the caller fails the exchange), and the
    /// `take` bound never reads past `nbytes` even when more bytes are available
    /// (they would be the next protocol frame).
    #[test]
    fn copy_body_is_byte_exact_and_reports_truncation() {
        let body: &[u8] = b"{\"version\": 2}\n[0.1, \"o\", \"hi\\r\\n\"]\n";
        let mut out = Vec::new();
        let copied = copy_body(&mut &body[..], &mut out, body.len() as u64).unwrap();
        assert_eq!(copied, body.len() as u64);
        assert_eq!(out, body, "verbatim — no line normalization");
        // Truncated: the reader EOFs early; the short count is reported, not an error.
        let mut short = Vec::new();
        let copied = copy_body(&mut &body[..10], &mut short, body.len() as u64).unwrap();
        assert_eq!(copied, 10);
        assert_eq!(short, &body[..10]);
        // Bounded: exactly nbytes, even with more available on the stream.
        let mut bounded = Vec::new();
        let copied = copy_body(&mut &body[..], &mut bounded, 5).unwrap();
        assert_eq!(copied, 5);
        assert_eq!(bounded, &body[..5]);
    }

    /// A proxied `@<sid> <verb>` must frame its response by the FORWARDED verb, not
    /// the selector — else the client drops the `OK <n>` payload (the bug the live
    /// recursion demo exposed: `@child screen`/`text` returned only the status line).
    #[test]
    fn forwarded_verb_skips_proxy_selector() {
        let p = |s: &str| s.split_whitespace().map(String::from).collect::<Vec<_>>();
        assert_eq!(forwarded_verb(&p("screen")).as_deref(), Some("screen"));
        assert_eq!(
            forwarded_verb(&p("@s-abc screen")).as_deref(),
            Some("screen")
        );
        assert_eq!(forwarded_verb(&p("@. text")).as_deref(), Some("text"));
        assert_eq!(
            forwarded_verb(&p("@s-abc image read")).as_deref(),
            Some("image")
        );
        assert_eq!(forwarded_verb(&p("send hi there")).as_deref(), Some("send"));
        assert_eq!(forwarded_verb(&[]), None);
        // Degenerate bare selector: falls back to it (the server then errors).
        assert_eq!(forwarded_verb(&p("@s-abc")).as_deref(), Some("@s-abc"));
    }

    /// `dial <name> <verb...>` frames the reply by the FORWARDED verb (`parts[2]`),
    /// never "dial" or the remote name — the bytes on the wire are the remote's
    /// answer to `<verb...>`. The forwarded SUB-request also frames positional
    /// sub-forms (`image read`, `cast frames`) and bare `cast` correctly.
    #[test]
    fn dial_frames_response_by_forwarded_verb_not_dial() {
        let p = |s: &str| s.split_whitespace().map(String::from).collect::<Vec<_>>();
        assert_eq!(
            forwarded_verb(&p("dial myhost text")).as_deref(),
            Some("text")
        );
        assert_eq!(
            forwarded_verb(&p("dial myhost cursor")).as_deref(),
            Some("cursor")
        );
        assert_eq!(
            forwarded_verb(&p("dial myhost image read")).as_deref(),
            Some("image")
        );
        // `dial <name> @<sid> <verb>` drives a SPECIFIC remote session: the framing
        // verb must SKIP the remote selector (else it framed on "@s-remote" → Status
        // and silently dropped every payload row). Regression for the round-3 fix.
        assert_eq!(
            forwarded_verb(&p("dial myhost @s-remote text")).as_deref(),
            Some("text"),
            "dial + remote selector frames on the forwarded verb, not the selector"
        );
        assert_eq!(
            forwarded_verb(&p("dial myhost @s-remote image read")).as_deref(),
            Some("image")
        );
        // `frame_request` (what `real_main` passes for a dial = `request_parts[2..]`)
        // decides Lines/Bytes/Status exactly as if the verb had been run locally.
        assert!(streams_payload("text", "text"), "dial ... text => Lines");
        assert!(
            !streams_payload("cursor", "cursor") && !bytes_payload("cursor", "cursor"),
            "dial ... cursor => Status"
        );
        assert!(
            streams_payload("image", "image read"),
            "dial ... image read => Lines"
        );
        assert!(
            streams_payload("cast", "cast frames"),
            "dial ... cast frames => Lines"
        );
        assert!(bytes_payload("cast", "cast"), "dial ... cast => Bytes");
    }

    /// A BARE `dial <name>` (or `dial` alone) is rejected BEFORE any bytes are sent —
    /// a one-shot dial with no verb would deadlock (the remote never closes the
    /// relay), so the client fails it clearly with a nonzero (non-124) exit.
    #[test]
    fn bare_dial_is_rejected_before_send() {
        let p = |s: &str| s.split_whitespace().map(String::from).collect::<Vec<_>>();
        assert!(dial_missing_verb(&p("dial")), "`dial` alone has no verb");
        assert!(
            dial_missing_verb(&p("dial myhost")),
            "`dial <name>` has no verb"
        );
        assert!(
            !dial_missing_verb(&p("dial myhost text")),
            "`dial <name> <verb>` is complete"
        );
        assert!(
            !dial_missing_verb(&p("text")),
            "non-dial verbs are unaffected"
        );
        assert!(!dial_missing_verb(&[]), "empty request is not a dial");

        let e = dial_needs_verb_error();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
        assert!(
            !is_timeout_error(&e),
            "bare dial is a plain failure, not a 124 timeout"
        );
        assert_eq!(
            e.to_string(),
            "ERR dial: give a verb to run on the remote (dial <name> <verb...>)"
        );
    }

    /// End-to-end (mock UDS): `dial myhost text` sends the dial line verbatim to the
    /// local instance and reads back the REMOTE's Lines-framed `text` reply WITHOUT
    /// blocking — the server prepends the verb so the remote answers and the relay
    /// pumps it back. Full cross-machine dial (a live TLS remote) is verified BY
    /// CONSTRUCTION: server prebuffer-forwards `<verb...>` (control.rs `try_net_dial`)
    /// and this client frames the reply by that forwarded verb.
    ///
    /// UNIX ONLY: the mock instance is a `std::os::unix::net::UnixListener`,
    /// which does not exist on Windows — so without this gate the whole
    /// `aterm-ctl` TEST TARGET failed to compile there (E0433, `cannot find
    /// unix in os`) and not one of this module's tests could run on Windows.
    /// The client half under test is platform-free; what is unix-only is the
    /// listener standing in for a live instance.
    #[cfg(unix)]
    #[test]
    fn dial_with_verb_reads_lines_reply_without_hanging() {
        use std::io::{BufRead, Read, Write};
        use std::os::unix::net::UnixListener;
        use std::sync::mpsc;

        // A private dir holding ONLY the socket (no sibling token file), so
        // `read_token_for` returns None and the client sends the request line with
        // no AUTH prefix — the mock reads `dial myhost text` as its first line.
        let dir = std::env::temp_dir().join(format!("aterm-ctl-dialtest-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sock = dir.join("mock.sock");
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).expect("bind mock socket");

        // Mock LOCAL instance: accept, read the relayed `dial` line, echo the
        // Lines-framed reply the remote's `text` answer pumps back, then drain until
        // the client closes (proving the client does not wait for US to close).
        let srv = std::thread::spawn(move || {
            let (conn, _) = listener.accept().expect("accept");
            let mut r = std::io::BufReader::new(conn.try_clone().expect("clone"));
            let mut first = String::new();
            r.read_line(&mut first).expect("read request line");
            let mut w = conn;
            w.write_all(b"OK 2\nhello\nworld\n").expect("write reply");
            w.flush().expect("flush");
            let mut sink = Vec::new();
            let _ = r.read_to_end(&mut sink);
            first
        });

        // Drive `exchange` exactly as `real_main` does for `dial myhost text`: the
        // line SENT is the full dial line; framing follows the forwarded verb `text`.
        let sockpath = sock.to_str().expect("utf8 path").to_string();
        let (tx, rx) = mpsc::channel();
        let cli = std::thread::spawn(move || {
            let res = exchange(
                &sockpath,
                "dial myhost text\n",
                "text",
                "text",
                Some(std::time::Duration::from_secs(5)),
                false,
            );
            let _ = tx.send(res.is_ok());
        });

        // The whole exchange must finish well within the deadline — proof of no hang.
        let ok = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("dial exchange must not block indefinitely");
        assert!(
            ok,
            "dial myhost text should complete the exchange (OK reply)"
        );
        cli.join().expect("client thread");
        let relayed = srv.join().expect("server thread");
        assert_eq!(
            relayed, "dial myhost text\n",
            "client must relay the dial line verbatim"
        );
        let _ = std::fs::remove_file(&sock);
        let _ = std::fs::remove_dir(&dir);
    }

    /// The `search` verb's header is `OK <n> incomplete` when the server evicted
    /// scrollback (`SearchIndex::results_may_be_incomplete` latches true after the
    /// first eviction). The count parser must read only the FIRST token, else the
    /// client rejects the header as malformed and drops every match line the
    /// server streamed — for all of a long-lived terminal's subsequent searches.
    #[test]
    fn stream_count_ignores_incomplete_marker() {
        // `OK 3 incomplete` -> tail "3 incomplete" -> count 3.
        assert_eq!(stream_count("3 incomplete"), Some(3));
        // Clean `OK <n>` headers (every other streaming verb) still parse.
        assert_eq!(stream_count("42"), Some(42));
        assert_eq!(stream_count("0"), Some(0));
        // A non-numeric first token is genuinely malformed.
        assert_eq!(stream_count(""), None);
        assert_eq!(stream_count("incomplete"), None);
        // A peer-supplied count past the bound is rejected like a malformed
        // header — it must never size an allocation.
        assert_eq!(
            stream_count(&MAX_STREAM_LINES.to_string()),
            Some(MAX_STREAM_LINES)
        );
        assert_eq!(stream_count(&(MAX_STREAM_LINES + 1).to_string()), None);
        assert_eq!(stream_count("18446744073709551615"), None);
    }

    /// `malformed_header_error` renders the status line by hand (the strict
    /// Trust gate cannot lower inline `format_args!`), so pin it byte-for-byte
    /// to the `format!("... {status_line:?}")` it replaced — including the
    /// `Debug`-for-`str` corner cases: literal single quotes, escaped double
    /// quotes/backslashes, control chars, and grapheme-extended chars.
    #[test]
    fn malformed_header_error_matches_debug_formatting() {
        for s in [
            "OK x",
            "",
            "it's fine",
            "say \"hi\"",
            "back\\slash",
            "tab\tnl\nnul\0",
            "caf\u{e9} + combining e\u{301}",
            "emoji \u{1f980}",
        ] {
            let e = malformed_header_error(s);
            assert_eq!(
                e.to_string(),
                format!("malformed response header: {s:?}"),
                "drifted for input {s:?}"
            );
            assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        }
    }

    /// `usage_error` composes its message by hand for the same reason; pin it
    /// to the `format!("usage: {SYNOPSIS}")` it replaced.
    #[test]
    fn usage_error_matches_format() {
        let e = usage_error();
        assert_eq!(e.to_string(), format!("usage: {SYNOPSIS}"));
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
    }

    /// `connect_error` composes its message by hand for the same reason; pin
    /// the `connect {path}: {cause}` frame and check the original error kind
    /// is preserved. The two "no engine serves this socket" kinds additionally
    /// carry the not-running remedy — a raw `No such file or directory (os
    /// error 2)` told the one user the installer leaves with only `aterm` on
    /// PATH nothing about what to DO.
    #[test]
    fn connect_error_matches_format_and_keeps_kind() {
        let path = "/tmp/aterm-test/aterm.sock";
        // A kind that does NOT mean "not running" keeps the bare cause: a
        // permission error must never claim aterm is down.
        let cause = io::Error::new(io::ErrorKind::PermissionDenied, "Permission denied");
        let e = connect_error(path, &cause);
        assert_eq!(e.to_string(), format!("connect {path}: {cause}"));
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);

        // NotFound / ConnectionRefused say what the failure MEANS and what to
        // do, still framed on the path tried, still kind-preserving (the exit
        // mapping and callers key on the kind).
        for (kind, oserr) in [
            (
                io::ErrorKind::NotFound,
                "No such file or directory (os error 2)",
            ),
            (
                io::ErrorKind::ConnectionRefused,
                "Connection refused (os error 61)",
            ),
        ] {
            let cause = io::Error::new(kind, oserr);
            let e = connect_error(path, &cause);
            let msg = e.to_string();
            assert!(
                msg.starts_with(&format!("connect {path}: {oserr}")),
                "the raw cause stays first: {msg}"
            );
            assert!(msg.contains("aterm isn't running"), "{msg}");
            assert!(msg.contains("open -a aterm"), "the remedy is named: {msg}");
            assert_eq!(e.kind(), kind);
        }
    }

    /// The streaming-verb set and the selector-aware `image read` detection
    /// moved into `streams_payload`; keep its behavior nailed down.
    #[test]
    fn streams_payload_gates_by_verb_and_image_read() {
        // Streaming verbs frame `OK <n>` + n lines.
        for verb in [
            "text",
            "screen",
            "search",
            "modes",
            "selection",
            "blocks",
            "blocktext",
            "chrome",
            "panes",
            "sessions",
            "family",
            "who",
            "edges",
            "grants",
            "controls",
        ] {
            assert!(streams_payload(verb, verb), "{verb} should stream");
        }
        // Single-line verbs echo their status line (even integer-tailed ones).
        for verb in ["cursor", "copy", "lines", "feed", "signal", "image"] {
            assert!(!streams_payload(verb, verb), "{verb} should not stream");
        }
        // `image read` streams; detection skips a leading proxy selector.
        assert!(streams_payload("image", "image read\n"));
        assert!(streams_payload("image", "@s-abc image read\n"));
        assert!(!streams_payload("image", "image shot.png\n"));
        assert!(!streams_payload("image", "@s-abc image shot.png\n"));
    }

    #[test]
    fn guarded_artifact_ack_echoes_only_the_server_nonce() {
        let (client, mut server) = CtlStream::pair().unwrap();
        server
            .write_all(b"ACK-CHALLENGE 00112233445566778899aabbccddeeff\n")
            .unwrap();
        server.flush().unwrap();
        let peer = std::thread::spawn(move || {
            let mut line = String::new();
            BufReader::new(&server).read_line(&mut line).unwrap();
            line
        });
        let mut reader = BufReader::new(&client);
        acknowledge_artifact_reply(&client, &mut reader, "image", "image shot.png").unwrap();
        assert_eq!(
            peer.join().unwrap(),
            "ACK 00112233445566778899aabbccddeeff\n"
        );
    }

    #[test]
    fn guarded_artifact_reply_accepts_a_legacy_server_without_hanging() {
        let (client, _legacy_server) = CtlStream::pair().unwrap();
        let mut reader = BufReader::new(&client);
        acknowledge_artifact_reply(&client, &mut reader, "image", "image shot.png")
            .expect("an absent optional challenge is a bounded legacy fallback");
    }

    #[test]
    fn guarded_payload_ack_precedes_the_user_output_handoff() {
        let (client, mut server) = CtlStream::pair().unwrap();
        let (acked_tx, acked_rx) = std::sync::mpsc::channel();
        let peer = std::thread::spawn(move || {
            server
                .write_all(
                    b"OK 1\nframe n=1 /private/frame.png\n\
                      ACK-CHALLENGE 00112233445566778899aabbccddeeff\n",
                )
                .unwrap();
            server.flush().unwrap();
            let mut ack = String::new();
            BufReader::new(&server).read_line(&mut ack).unwrap();
            assert_eq!(ack, "ACK 00112233445566778899aabbccddeeff\n");
            acked_tx.send(()).unwrap();
        });

        let mut reader = BufReader::new(&client);
        let mut status = String::new();
        reader.read_line(&mut status).unwrap();
        let status = status.trim_end();
        let output = receive_guarded_artifact_reply(
            &client,
            &mut reader,
            "video",
            "video frames count=1",
            status,
            "1",
        )
        .unwrap();
        acked_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("wire ACK must complete before the caller can block on stdout");
        match output {
            GuardedArtifactOutput::Lines(lines) => {
                assert_eq!(lines, ["frame n=1 /private/frame.png"]);
            }
            GuardedArtifactOutput::Status => panic!("video frames must retain its payload"),
        }
        peer.join().unwrap();
    }

    /// The socket is line-delimited (`read_line`, one verb per line). An argument
    /// carrying an embedded `\n` would frame a SECOND authenticated verb after the
    /// intended one, so framing must reject any line terminator in the parts.
    #[test]
    fn validate_request_parts_rejects_embedded_line_terminators() {
        let injected = vec!["send".to_string(), "hi\nAUTH deadbeef".to_string()];
        let err = validate_request_parts(&injected).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        // A bare CR is rejected too (CRLF-splitting servers).
        assert!(validate_request_parts(&["a\rb".to_string()]).is_err());
        // Clean free-form arguments (spaces included) still frame fine.
        let clean = vec!["send".to_string(), "hello world".to_string()];
        assert!(validate_request_parts(&clean).is_ok());
    }

    #[test]
    fn resolve_refuses_both_sock_and_pid() {
        let err = resolve_path(Some("/tmp/a.sock".into()), Some(7), None, None, None).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn resolve_pid_targets_that_instance_socket() {
        let path = resolve_path(None, Some(42), None, None, None).expect("per-user dir");
        // Platform-native separator: '/' on Unix, '\' on Windows.
        let want = format!("{}aterm-42.sock", std::path::MAIN_SEPARATOR);
        assert!(path.ends_with(&want), "got {path}");
    }

    #[test]
    fn resolve_flag_beats_environment() {
        let path = resolve_path(
            Some("/tmp/a.sock".into()),
            None,
            Some("/elsewhere.sock".into()),
            None,
            None,
        )
        .unwrap();
        assert_eq!(path, "/tmp/a.sock");
    }

    #[test]
    fn resolve_honours_environment_disable_keywords() {
        for (env_sock, env_kill) in [(Some("0"), None), (Some("off"), None), (None, Some("1"))] {
            let err = resolve_path(
                None,
                None,
                env_sock.map(String::from),
                env_kill.map(String::from),
                None,
            )
            .unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::NotFound);
        }
        // ...but an explicit path value passes straight through.
        let path = resolve_path(None, None, Some("/tmp/x.sock".into()), None, None).unwrap();
        assert_eq!(path, "/tmp/x.sock");
    }

    /// IN-SESSION SELF-LOCATION: with several instances running, a flagless call
    /// from inside an aterm session must reach the instance hosting THAT session
    /// — through its graph entry — not whichever instance owns the `latest`
    /// symlink. Malformed sids and dangling entries fall back (fail-safe).
    #[test]
    fn self_location_prefers_own_instance_graph_entry() {
        // The graph parse is the SHARED engine helper (one on-disk format).
        assert_eq!(
            control_socket::graph_entry_sock("sock /d/aterm-7.sock\nnonce ab\n").as_deref(),
            Some("/d/aterm-7.sock")
        );
        assert_eq!(control_socket::graph_entry_sock("nonce ab\n"), None);
        assert_eq!(control_socket::graph_entry_sock("sock \n"), None);

        // self_instance_sock: sid-shape validation fails closed (no traversal).
        assert_eq!(self_instance_sock(None), None);
        assert_eq!(self_instance_sock(Some("nope")), None);
        assert_eq!(self_instance_sock(Some("s-")), None);
        assert_eq!(self_instance_sock(Some("s-../../etc/passwd")), None);
        assert_eq!(self_instance_sock(Some("s-XYZ")), None);
    }

    /// EXHAUSTIVE drift guard: the VERBS section of `--help` is GENERATED from
    /// the protocol verb table (`catalog_lines`), so EVERY verb the build speaks
    /// must appear in the help output — not a hand-picked sample that silently
    /// falls behind as verbs are added (the drift this replaced: 31 of 65
    /// documented). The hand-written prose sections must also survive.
    #[test]
    fn help_text_lists_every_verb_and_the_prose() {
        let help = help_text();
        // The synopsis is shared between --help and the no-verb usage error, so
        // both must surface the exact invocation shape.
        assert!(help.contains(SYNOPSIS), "help should embed the synopsis");
        // Every verb in the table appears — the generated catalog cannot drift.
        for spec in aterm_types::control_verbs::VERBS {
            assert!(
                help.contains(spec.name),
                "help must document the `{}` verb",
                spec.name
            );
        }
        // The exact generated catalog line for a representative verb is present
        // verbatim — proof the section is the table's own rendering, not prose.
        assert!(
            aterm_types::control_verbs::catalog_lines().all(|line| help.contains(&line)),
            "every generated catalog line must appear in --help"
        );
        // The hand-written prose sections survive.
        assert!(help.contains("$ATERM_CONTROL_SOCK"));
        assert!(help.contains("aterm-<pid>.sock"));
        // The new OPTIONS + EXIT CODES prose.
        assert!(help.contains("--timeout"));
        assert!(help.contains("124"));
        // The stdin-payload forms are documented.
        assert!(help.contains("send --stdin"));
    }

    /// Every socket read goes through `read_bounded_line`, so its success
    /// path must be BYTE-IDENTICAL to `BufRead::read_line` (headers, CRLF
    /// body rows, the EOF-cut partial line, and std's exact invalid-UTF-8
    /// error) — the cap is a new failure mode, never a behavior change.
    #[test]
    fn read_bounded_line_matches_read_line_on_wellformed_input() {
        let data: &[u8] = b"OK 2 incomplete\r\nalpha\n\nbeta"; // header, body, blank, EOF-cut tail
        let mut bounded = io::Cursor::new(data);
        let mut plain = io::Cursor::new(data);
        loop {
            let (mut a, mut b) = (String::new(), String::new());
            let na = read_bounded_line(&mut bounded, &mut a).unwrap();
            let nb = plain.read_line(&mut b).unwrap();
            assert_eq!((na, &a), (nb, &b));
            if na == 0 {
                break;
            }
        }
        // Invalid UTF-8: same kind, same message, `line` left untouched.
        let mut out = String::new();
        let e = read_bounded_line(&mut io::Cursor::new(&b"\xffbad\n"[..]), &mut out).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        assert_eq!(e.to_string(), "stream did not contain valid UTF-8");
        assert!(out.is_empty());
    }

    /// The accumulation cap is what actually bounds memory against a server
    /// that streams bytes WITHOUT a newline (a read deadline never fires while
    /// bytes keep arriving) — the client-side twin of the read-to-EOF-on-
    /// `/dev/urandom` exhaustion this crate family was audited for.
    #[test]
    fn read_bounded_line_caps_newlineless_floods() {
        // A newline right at the cap still parses: content == MAX_LINE_BYTES.
        let mut ok_line = vec![b'x'; MAX_LINE_BYTES];
        ok_line.push(b'\n');
        let mut out = String::new();
        let n = read_bounded_line(&mut io::Cursor::new(&ok_line), &mut out).unwrap();
        assert_eq!(n, MAX_LINE_BYTES + 1);
        assert_eq!(out.len(), MAX_LINE_BYTES + 1);
        // One more content byte with no newline is an oversized reply — even
        // when the cap slices a multi-byte char (still "oversized", never a
        // spurious UTF-8 error).
        let ascii_flood = vec![b'x'; MAX_LINE_BYTES + 1];
        let multibyte_flood = "é".repeat(MAX_LINE_BYTES).into_bytes();
        for flood in [ascii_flood, multibyte_flood] {
            let mut out = String::new();
            let err = read_bounded_line(&mut io::Cursor::new(&flood), &mut out).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidData);
            assert!(err.to_string().contains("oversized reply"), "got {err}");
            assert!(out.is_empty(), "must not leak a partial line");
        }
    }

    /// The `exchange` deadline must clear every legitimate synchronous verb,
    /// or the CLIENT deadline kills a healthy reply first: the blocking verbs'
    /// server-side clamp (`await`/`ready`/`wait` cap their timeout at
    /// 600 000 ms in control_session.rs / control_selection.rs) AND `update
    /// check`'s synchronous curl budget (30 s API `--max-time` + 600 s
    /// download `--max-time`, before verify/stage disk work).
    #[test]
    fn exchange_deadline_clears_the_server_blocking_clamp() {
        assert!(EXCHANGE_DEADLINE > std::time::Duration::from_millis(600_000));
        // update check: 30 s + 600 s of curl, plus real margin for staging.
        assert!(EXCHANGE_DEADLINE >= std::time::Duration::from_secs(650 + 60));
    }

    #[test]
    fn version_line_is_well_formed() {
        // Mirror what the --version branch prints; guard against a blank or
        // mis-prefixed version line.
        let line = format!("aterm-ctl {}", aterm_types::version::APP_VERSION);
        assert!(line.starts_with("aterm-ctl "));
        assert!(!aterm_types::version::APP_VERSION.is_empty());
    }

    /// `--timeout` parses SECONDS into the per-op deadline: `0` disables it
    /// (`None`), a positive integer is that many seconds, and the default `900`
    /// reproduces [`EXCHANGE_DEADLINE`]. Negative / non-numeric is a usage error.
    #[test]
    fn parse_timeout_zero_disables_positive_is_seconds() {
        assert_eq!(parse_timeout("0").unwrap(), None);
        assert_eq!(
            parse_timeout("5").unwrap(),
            Some(std::time::Duration::from_secs(5))
        );
        assert_eq!(parse_timeout("900").unwrap(), Some(EXCHANGE_DEADLINE));
        for bad in ["", "-1", "abc", "1.5", "  "] {
            let e = parse_timeout(bad).unwrap_err();
            assert_eq!(
                e.kind(),
                io::ErrorKind::InvalidInput,
                "{bad:?} should error"
            );
        }
    }

    /// The 124 exit code is reserved for a TIMEOUT and is additive: still
    /// nonzero, so a script's `nonzero == failure` check is unaffected, but
    /// distinguishable from the generic-failure 1.
    #[test]
    fn timeout_exit_code_is_124_and_client_kinds_map_to_it() {
        assert_eq!(EXIT_TIMEOUT, 124);
        assert!(is_timeout_error(&io::Error::from(
            io::ErrorKind::WouldBlock
        )));
        assert!(is_timeout_error(&io::Error::from(io::ErrorKind::TimedOut)));
        assert!(!is_timeout_error(&io::Error::from(
            io::ErrorKind::ConnectionRefused
        )));
        assert!(!is_timeout_error(&io::Error::from(io::ErrorKind::NotFound)));
    }

    /// A SERVER-reported timeout maps to 124: a bare `OK timeout` (the blocking
    /// verbs) and a `turn` verdict carrying `status=timeout`. A settled turn, a
    /// normal reply, and any `ERR` line are NOT timeouts.
    #[test]
    fn reply_is_timeout_detects_await_and_turn_forms() {
        assert!(reply_is_timeout("OK timeout"));
        assert!(reply_is_timeout(
            "OK 24 turn submitted=1 status=timeout seq=9"
        ));
        assert!(!reply_is_timeout(
            "OK 24 turn submitted=1 status=settled seq=9"
        ));
        assert!(!reply_is_timeout("OK ready prompt"));
        assert!(!reply_is_timeout("OK 12 bytes"));
        assert!(!reply_is_timeout("OK"));
        // An ERR/other status is never a timeout (stays generic-failure exit 1).
        assert!(!reply_is_timeout("ERR timeout"));
        assert!(!reply_is_timeout("timeout"));
    }

    /// `split_selector` peels a leading `@<selector>` off the request parts (so a
    /// stdin frame can route to a peer), and leaves a flagless verb untouched.
    #[test]
    fn split_selector_peels_leading_at() {
        let p = |s: &str| s.split_whitespace().map(String::from).collect::<Vec<_>>();
        let parts = p("@s-abc feed-bin");
        let (sel, rest) = split_selector(&parts);
        assert_eq!(sel, Some("@s-abc"));
        assert_eq!(rest, &["feed-bin".to_string()]);
        let parts = p("send --stdin");
        let (sel, rest) = split_selector(&parts);
        assert_eq!(sel, None);
        assert_eq!(rest, &["send".to_string(), "--stdin".to_string()]);
        assert_eq!(split_selector(&[]), (None, &[][..]));
    }

    /// The binary-frame request head is built by hand (no inline `format_args!`):
    /// bare `<verb>`, or `@<sel> <verb>` when a proxy selector routes the frame to
    /// a peer. `send --stdin`/`feed-bin` use the RAW `feed-bin` verb; `paste
    /// --stdin` uses `paste-bin` (the server's paste seam). Caller appends ` <len>\n`.
    #[test]
    fn binary_frame_prefix_prepends_selector_and_carries_verb() {
        assert_eq!(binary_frame_prefix(None, "feed-bin"), "feed-bin");
        assert_eq!(
            binary_frame_prefix(Some("@s-abc"), "feed-bin"),
            "@s-abc feed-bin"
        );
        assert_eq!(binary_frame_prefix(Some("@."), "feed-bin"), "@. feed-bin");
        // `paste --stdin` routes through the paste seam, not the raw feed.
        assert_eq!(binary_frame_prefix(None, "paste-bin"), "paste-bin");
        assert_eq!(
            binary_frame_prefix(Some("@s-abc"), "paste-bin"),
            "@s-abc paste-bin"
        );
    }

    /// `@self` / `@env` expand to a concrete `@<sid>` from the passed
    /// `$ATERM_PARENT_SESSION_ID` before framing; a missing/empty env var is a
    /// clear error (parts left as-is), and other selectors / flagless verbs pass
    /// through untouched.
    #[test]
    fn expand_self_selector_expands_env_and_errors_when_unset() {
        let mut p = vec!["@self".to_string(), "text".to_string()];
        expand_self_selector(&mut p, Some("s-abc123".into())).unwrap();
        assert_eq!(p[0], "@s-abc123");
        // The `@env` alias behaves identically.
        let mut p = vec!["@env".to_string(), "send".to_string(), "hi".to_string()];
        expand_self_selector(&mut p, Some("s-ff".into())).unwrap();
        assert_eq!(p[0], "@s-ff");
        // Unset OR empty: a clear error naming the env var, parts untouched.
        for env in [None, Some(String::new())] {
            let mut p = vec!["@self".to_string(), "text".to_string()];
            let e = expand_self_selector(&mut p, env).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
            assert!(
                e.to_string().contains("$ATERM_PARENT_SESSION_ID unset"),
                "clear message: {e}"
            );
            assert_eq!(p[0], "@self", "left untouched on error");
        }
        // `@<sid>`, `@.`, and a flagless verb are all untouched.
        for sel in ["@s-xyz", "@.", "text"] {
            let mut p = vec![sel.to_string(), "text".to_string()];
            expand_self_selector(&mut p, Some("s-abc".into())).unwrap();
            assert_eq!(p[0], sel);
        }
        // Empty request is a no-op (no panic on `first()`).
        expand_self_selector(&mut [], None).unwrap();

        // SUBSCRIBE selector-SECOND: `subscribe @self <streams>` expands parts[1]
        // (there was no working `@self` live-watch before this).
        let mut p = vec![
            "subscribe".to_string(),
            "@self".to_string(),
            "screen,events".to_string(),
        ];
        expand_self_selector(&mut p, Some("s-live".into())).unwrap();
        assert_eq!(p[1], "@s-live", "subscribe selector-second is expanded");
        assert_eq!(p[2], "screen,events", "streams untouched");
        // A COMMA LIST expands each @self/@env element, leaving concrete sids alone.
        let mut p = vec![
            "subscribe".to_string(),
            "@self,@1,@env".to_string(),
            "screen".to_string(),
        ];
        expand_self_selector(&mut p, Some("s-x".into())).unwrap();
        assert_eq!(
            p[1], "@s-x,@1,@s-x",
            "each self element in the list expands"
        );
        // `subscribe` with a concrete selector is untouched; unset env still errors
        // when the subscribe selector needs it.
        let mut p = vec![
            "subscribe".to_string(),
            "@self".to_string(),
            "screen".to_string(),
        ];
        assert_eq!(
            expand_self_selector(&mut p, None).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    /// The CLIENT-answered discovery verbs (`ls`, `instances`, `windows`) — intercepted
    /// by aterm-ctl, so NOT in the generated protocol catalog — are documented in
    /// `--help` with their output shapes, and the subscribe PUSH-FRAME grammar is
    /// spelled out (an AI reading `--help` must not reverse-engineer either).
    #[test]
    fn help_text_documents_client_verbs_and_push_frames() {
        let help = help_text();
        assert!(help.contains("CLIENT VERBS"), "client-verb block present");
        // `ls`/`instances` output shapes (the finding: both were undiscoverable).
        assert!(help.contains("instances"), "the `instances` verb is named");
        assert!(
            help.contains("<pid> <local> <sid> <parent|-> <state> <title>"),
            "ls output shape documented"
        );
        assert!(
            help.contains("<pid> <session-count> <sock>"),
            "instances output shape documented"
        );
        // The roster's window/detail columns and the per-window fold (F2/F5).
        assert!(
            help.contains("window=<id|none|-> active=<0|1|-> wfocus=<0|1|-> detail=<pct|->"),
            "ls trailing columns documented"
        );
        assert!(
            help.contains("<pid> window=<id> focused=<0|1> sessions=<n> active=<sid>[,<sid>…]"),
            "windows output shape documented"
        );
        for (verb, sentence) in [
            ("ls", "`ls 1` is `ERR usage: ls`"),
            ("instances", "`instances 1` is `ERR usage: instances`"),
            ("windows", "`windows 1` is `ERR usage:\n    windows`"),
        ] {
            assert!(
                help.contains(sentence),
                "{verb} documents that it takes no argument"
            );
        }
        // A headless instance is NOT `window=none`: it owns logical window 0,
        // and both listings say so, so a reader of `ls` on a headless peer is
        // not sent looking for a detached session.
        assert!(
            help.contains(
                "a headless instance reports its one logical
                  window 0"
            ) && help.contains("a headless instance folds under its one logical window 0"),
            "ls and windows both document headless placement as window 0"
        );
        // What `focused=`/`wfocus=` mean — the question a driver asks first when
        // two windows are open: aterm's most recently focused window, which a
        // minimize or an app deactivation does not change (the OS key window is
        // a different fact). Both listings say so.
        assert!(
            help.contains(
                "wfocus= that window is aterm's most
                  recently focused one (unchanged by a minimize or by the app
                  deactivating)"
            ),
            "ls documents wfocus="
        );
        assert!(
            help.contains(
                "focused= aterm's most recently focused window — one per
                  instance, unchanged by a minimize or by the app deactivating"
            ),
            "windows documents focused="
        );
        assert!(
            help.contains("`help ls` / `help instances` / `help windows` / `help mux` print the"),
            "the block says help <client verb> is answered from it"
        );
        assert!(
            completion_verb_list()
                .split_whitespace()
                .any(|v| v == "windows"),
            "windows completes like ls/instances"
        );
        // The push-frame wire grammar names every frame shape.
        assert!(help.contains("PUSH FRAMES"), "push-frame section present");
        for shape in [
            "sub <local> <sid>",
            "DELTA <local> seq=",
            "EVENT <local>",
            "BYTES <local> <len>",
            "GAP <local>",
        ] {
            assert!(help.contains(shape), "push frame `{shape}` documented");
        }
    }

    /// The stdin-payload cap mirrors the server's 256 KiB `feed-bin` limit and is
    /// enforced CLIENT-SIDE (a clear error before any byte is pipelined into a
    /// frame the server would refuse and desync on).
    #[test]
    fn feed_bin_cap_matches_server_and_errors_are_static() {
        assert_eq!(MAX_FEED_BIN, 256 * 1024);
        assert_eq!(
            oversized_feed_bin_error().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(feed_bin_inline_error().kind(), io::ErrorKind::InvalidInput);
    }

    /// The hidden `--completions <shell>` generator: every supported shell yields a
    /// NON-EMPTY script that names its shell wiring, the representative verbs the
    /// audit finding calls out, EVERY protocol verb (the same drift guard
    /// `help_text` gets — a new verb cannot silently miss completion), and the two
    /// client-only discovery verbs; an unknown shell is a clear error.
    #[test]
    fn completions_cover_every_verb_per_shell_and_reject_unknown() {
        for (shell, wiring) in [
            ("bash", "complete -F _aterm_ctl aterm-ctl"),
            ("zsh", "#compdef aterm-ctl"),
            ("fish", "complete -c aterm-ctl"),
        ] {
            let script = completion_script(shell).expect("known shell yields a script");
            assert!(!script.is_empty(), "{shell} script is non-empty");
            assert!(
                script.contains(wiring),
                "{shell} script has its shell wiring"
            );
            // Representative verbs called out by the audit finding.
            for verb in ["text", "turn", "subscribe"] {
                assert!(script.contains(verb), "{shell} completes `{verb}`");
            }
            // EXHAUSTIVE: every protocol verb appears — the completion list IS the
            // table's own rendering (`completion_verb_list`), so it cannot drift.
            for spec in aterm_types::control_verbs::VERBS {
                assert!(
                    script.contains(spec.name),
                    "{shell} must complete the `{}` verb",
                    spec.name
                );
            }
            // Both CLIENT-answered verbs (absent from the protocol table) are added,
            // adjacently — proof `completion_verb_list` appended them.
            assert!(
                script.contains("ls instances"),
                "{shell} completes the client verbs `ls`/`instances`"
            );
        }
        // An unrecognized shell name has no script and maps to a clear error.
        assert!(completion_script("powershell").is_none());
        let e = unknown_shell_error();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
        assert!(e.to_string().contains("unknown shell"), "got {e}");
    }

    /// The FRONT-DOOR generator: the completions the installer actually wires
    /// complete `aterm` (the sibling is stripped off PATH), and the `aterm ctl`
    /// delegation arm renders the SAME ctl verb set as the sibling script —
    /// both come out of `completion_verb_list`, so they cannot drift. The
    /// front-door verb/flag tables themselves are supplied (and pinned against
    /// the routing tables) by the `aterm` crate's tests.
    #[test]
    fn front_door_completions_complete_aterm_and_carry_the_ctl_surface() {
        let verbs = ["help", "ctl", "pkg", "fleet", "drive"];
        let flags = [
            ("--window", "open the GPU window"),
            ("--version", "print the version"),
        ];
        for (shell, wiring) in [
            ("bash", "complete -F _aterm aterm\n"),
            ("zsh", "#compdef aterm\n"),
            ("fish", "complete -c aterm -f\n"),
        ] {
            let script = front_door_completion_script(shell, &verbs, &flags)
                .expect("known shell yields a script");
            assert!(script.contains(wiring), "{shell} wires `aterm`: {script}");
            for verb in verbs {
                assert!(script.contains(verb), "{shell} completes `{verb}`");
            }
            for (flag, _) in flags {
                // fish names long flags dash-less (`-l window`).
                let probe = if shell == "fish" {
                    flag.trim_start_matches('-')
                } else {
                    flag
                };
                assert!(script.contains(probe), "{shell} completes `{flag}`");
            }
            // The `aterm ctl` arm renders the ctl surface: every protocol verb,
            // the client-only discovery verbs, and the ctl flags.
            for spec in aterm_types::control_verbs::VERBS {
                assert!(
                    script.contains(spec.name),
                    "{shell} must complete `aterm ctl {}`",
                    spec.name
                );
            }
            assert!(
                script.contains("ls instances"),
                "{shell} has the client verbs"
            );
            let sock = if shell == "fish" { "-l sock" } else { "--sock" };
            assert!(script.contains(sock), "{shell} has the ctl flags");
        }
        // CONTRACT (install.sh): the zsh script's FIRST line is `#compdef aterm`.
        let zsh = front_door_completion_script("zsh", &verbs, &flags).expect("zsh");
        assert_eq!(zsh.lines().next(), Some("#compdef aterm"));
        // An unknown shell has no script, exactly like the sibling generator.
        assert!(front_door_completion_script("powershell", &verbs, &flags).is_none());
    }

    /// `aterm-uds` keeps a std-only MIRROR of the token-filename rule so it can
    /// stay dependency-free (`aterm-agent`'s `--dial` path reads the token
    /// through it, and `aterm-agent` does not depend on `aterm-types`). This
    /// crate is the only one that depends on BOTH, so the agreement is pinned
    /// here — for the name the server WRITES and for the ordered list a client
    /// READS. A drift means a `--dial` drive authenticates against a file the
    /// server never wrote — the exact bug the mirror replaced.
    #[test]
    fn uds_token_name_mirror_matches_aterm_types() {
        let cases = [
            // per-instance sockets -> per-instance tokens
            "aterm-1.sock",
            "aterm-42.sock",
            "aterm-4294967295.sock",
            // the `latest` alias and every explicit shape -> a token named
            // after that socket alone
            "aterm.sock",
            "c.sock",
            "control.sock",
            "aterm-.sock",
            "aterm-abc.sock",
            "aterm-1x.sock",
            // pid ROUND-TRIPS through u32: leading zeros normalize, and a pid
            // past u32::MAX falls through to the explicit form. Slicing-only
            // mirrors drift on both.
            "aterm-01.sock",
            "aterm-007.sock",
            "aterm-99999999999.sock",
            "aterm-+1.sock",
            "aterm-1.socket",
            "aterm-1",
            "",
            // Names that would APPEND onto a reserved token name, and the one
            // shape whose `.sock`/`.token` handling used to differ BETWEEN the
            // two mirrors (`aterm_types` accepted the `.token` spelling as an
            // instance name and named the socket file itself; the mirror did
            // not). Both now key on `.sock` alone.
            "aterm",
            "aterm-4242",
            "aterm-42.token",
            "aterm.token",
            "ctl",
            "a.b.c.sock",
            "\u{65e5}\u{672c}.sock",
        ];
        for name in cases {
            assert_eq!(
                aterm_uds::latest::token_name_for_sock(name),
                control_socket::token_name_for_sock(name),
                "token-name mirror drifted for {name:?}"
            );
            assert_eq!(
                aterm_uds::latest::token_names_for_sock(name),
                control_socket::token_names_for_sock(name),
                "token-name READ ORDER mirror drifted for {name:?}"
            );
            // Neither mirror may hand two different sockets one token file:
            // every explicit name stays out of the reserved `aterm.token` /
            // `aterm-<pid>.token` space.
            let token = control_socket::token_name_for_sock(name);
            if control_socket::instance_pid(name).is_none() || !name.ends_with(".sock") {
                assert_ne!(
                    token,
                    control_socket::SIBLING_TOKEN_FILE,
                    "{name:?} must not derive the legacy shared token"
                );
                assert_eq!(
                    control_socket::instance_pid(&token),
                    None,
                    "{name:?} must not derive a per-instance token name"
                );
            }
        }
    }

    /// The client's reader follows the shared rule, per socket: two explicit
    /// sockets in ONE directory read two different tokens (F9 — they used to
    /// read one file, so the second instance to start locked out the first
    /// one's clients while it was still listening).
    #[test]
    fn two_explicit_sockets_in_one_directory_read_their_own_tokens() {
        let dir = std::env::temp_dir().join(format!("aterm-ctl-tok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("test dir");
        let a = dir.join("a.sock").to_string_lossy().into_owned();
        let b = dir.join("b.sock").to_string_lossy().into_owned();
        std::fs::write(dir.join("a.sock.token"), "aaaa\n").expect("token a");
        std::fs::write(dir.join("b.sock.token"), "bbbb\n").expect("token b");
        assert_eq!(read_token_for(&a).as_deref(), Some("aaaa"));
        assert_eq!(read_token_for(&b).as_deref(), Some("bbbb"));

        // A stale shared `aterm.token` from an older build cannot displace
        // either: the per-socket file exists, so the fallback is never reached.
        std::fs::write(dir.join("aterm.token"), "cccc\n").expect("legacy token");
        assert_eq!(read_token_for(&a).as_deref(), Some("aaaa"));
        assert_eq!(read_token_for(&b).as_deref(), Some("bbbb"));

        // COMPATIBILITY: an instance from a build that wrote only the shared
        // name is still reachable — the fallback fires exactly when the
        // per-socket file is absent, and it resolves to the same path the
        // dependency-free mirror picks.
        let c = dir.join("c.sock").to_string_lossy().into_owned();
        assert_eq!(read_token_for(&c).as_deref(), Some("cccc"));
        assert_eq!(
            aterm_uds::latest::token_path_for_sock(&c).expect("resolves"),
            dir.join("aterm.token")
        );

        // A per-instance socket never falls back onto the shared file.
        let inst = dir.join("aterm-4242.sock").to_string_lossy().into_owned();
        let (miss, err) = read_token_at(&inst).expect_err("no per-instance token");
        assert_eq!(miss, dir.join("aterm-4242.token"));
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(read_token_for(&inst).is_none());

        // An EMPTY per-socket token fails right there: it belongs to the
        // instance being dialed, and the shared file may be anyone's.
        std::fs::write(dir.join("a.sock.token"), "   \n").expect("empty token");
        let (path, err) = read_token_at(&a).expect_err("an empty token is a miss");
        assert_eq!(path, dir.join("a.sock.token"));
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// REGRESSION: `ls`/`instances` are client-answered, and they used to
    /// SILENTLY IGNORE `--sock`/`--pid` and enumerate the whole fleet. An agent
    /// that launched an isolated instance under a private socket and then ran
    /// `--sock <that> ls` was handed the USER'S REAL terminals — and, acting on
    /// that list, could drive them. `--sock` must scope to exactly one target.
    #[test]
    fn discovery_scopes_to_an_explicit_socket() {
        let t = discovery_targets(Some("/tmp/private-run/c.sock"), None)
            .expect("an explicit socket is always a valid target");
        assert_eq!(
            t,
            vec![(0, "/tmp/private-run/c.sock".to_string())],
            "exactly the addressed socket, and NEVER the ambient fleet"
        );
    }

    /// An explicit socket whose filename DOES encode a pid reports it, so the
    /// `<pid> …` line shape is preserved; a custom name falls back to the `0`
    /// placeholder this module already uses for a pid-less graph entry.
    #[test]
    fn discovery_reads_the_pid_from_an_instance_socket_name() {
        let named = discovery_targets(Some("/tmp/run/aterm-4242.sock"), None).expect("valid");
        assert_eq!(named, vec![(4242, "/tmp/run/aterm-4242.sock".to_string())]);

        let custom = discovery_targets(Some("/tmp/run/weird.sock"), None).expect("valid");
        assert_eq!(custom[0].0, 0, "no pid in the name -> 0 placeholder");

        // Leading zeros normalize through u32, matching `instance_pid`.
        let zeros = discovery_targets(Some("/tmp/run/aterm-007.sock"), None).expect("valid");
        assert_eq!(zeros[0].0, 7);
    }

    /// `--pid` for an instance that is not live must fail LOUDLY. Silently
    /// widening to the fleet is the exact hazard this closes.
    #[test]
    fn discovery_pid_miss_is_an_error_not_a_fleet_listing() {
        // A pid that cannot be live (0 is never a real instance pid here).
        let err = discovery_targets(None, Some(0));
        match err {
            Err(msg) => assert!(
                msg.contains("no live aterm instance with pid 0"),
                "actionable message, got {msg:?}"
            ),
            Ok(list) => assert!(
                list.is_empty(),
                "a --pid miss must never return other instances, got {list:?}"
            ),
        }
    }

    /// Unscoped discovery is unchanged — the whole fleet, as before.
    #[test]
    fn discovery_unscoped_is_the_whole_fleet() {
        // Equality with `live_instances()` is the property; both may be empty on
        // a machine with no aterm running, which is still a valid agreement.
        assert_eq!(
            discovery_targets(None, None).expect("unscoped never errors"),
            live_instances()
        );
    }

    // -----------------------------------------------------------------------
    // DISCOVERY MUST NEVER SAY "EMPTY" WHEN IT MEANS "COULD NOT LOOK" (F8)
    //
    // Fixture: the two tables in docs/AGENT-EXPERIENCE-2026-08-26.md §2.8 —
    // five unrelated conditions that all printed `no live aterm instances
    // found` against a live five-session instance, plus the honest `--sock`
    // control. Every row below is one of them, then the remaining causes.
    // -----------------------------------------------------------------------

    /// A pid no host hands out (above every pid_max), so `pid_alive` is false.
    const DEAD_PID: u32 = 0x7fff_fff0;

    /// The fleet-wide claim, licensed by exactly one condition.
    const FLEET_CLAIM: &str = "no live aterm instances found";

    fn strings(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|p| (*p).to_string()).collect()
    }

    /// `help <client verb>` is answered from the CLIENT VERBS block, framed like
    /// the server's own `help <verb>`: `OK <n>` then exactly the entry's rows —
    /// the verb line and its continuation lines, nothing from the neighbouring
    /// entry or the prose paragraph, each row present verbatim in `--help`.
    #[test]
    fn help_for_a_client_verb_is_answered_from_the_client_block() {
        let help = help_text();
        for verb in CLIENT_HELP_VERBS {
            let reply = client_help_reply(&strings(&["help", verb]))
                .unwrap_or_else(|| panic!("help {verb} is client-answered"));
            let mut lines = reply.lines();
            let header = lines.next().expect("framed");
            let rows: Vec<&str> = lines.collect();
            assert_eq!(header, format!("OK {}", rows.len()), "{verb}: {reply}");
            assert!(rows.len() > 1, "{verb}: an entry has continuation rows");
            assert!(
                rows[0].starts_with(&format!("{verb} ")),
                "{verb}: the first row is the verb line: {}",
                rows[0]
            );
            for row in &rows[1..] {
                assert!(
                    row.starts_with("              "),
                    "{verb}: continuation rows keep the gutter: {row}"
                );
                assert!(
                    !row.trim_start().starts_with("None of the three"),
                    "{verb}: the prose paragraph is not part of an entry"
                );
            }
            for row in &rows {
                assert!(
                    help.contains(&format!("    {row}\n")),
                    "{verb}: every row is a `--help` line verbatim: {row}"
                );
            }
        }
        // The specific reply an agent reading the F2 finding asked for.
        let windows = client_help_reply(&strings(&["help", "windows"])).unwrap();
        assert!(
            windows.contains("<pid> window=<id> focused=<0|1> sessions=<n> active=<sid>[,<sid>…]"),
            "{windows}"
        );
        assert!(windows.contains("could not say"), "{windows}");
        let ls = client_help_reply(&strings(&["help", "ls"])).unwrap();
        assert!(ls.contains("detail=<pct|->[ *]"), "{ls}");
        assert!(!ls.contains("instances     one line per"), "{ls}");
    }

    /// Everything that is not exactly `help <client verb>` still reaches the
    /// server: a bare `help`, `help <table verb>`, `help --full`, an unknown
    /// name (the server's `ERR unknown verb` is the right answer there), a
    /// trailing argument (the server's `ERR usage`), and a selector form.
    #[test]
    fn help_for_a_server_verb_still_reaches_the_server() {
        for parts in [
            vec!["help"],
            vec!["help", "text"],
            vec!["help", "sessions"],
            vec!["help", "--full"],
            vec!["help", "nonesuch"],
            vec!["help", "ls", "extra"],
            vec!["@s-abc", "help", "ls"],
            vec!["ls"],
            vec!["text", "ls"],
        ] {
            assert_eq!(
                client_help_reply(&strings(&parts)),
                None,
                "{parts:?} is the server's to answer"
            );
        }
        // And every client verb the block documents is one the table lacks —
        // the reason the interception exists — while `help` itself is a table verb
        // framed as lines, the shape the client reply copies.
        for verb in CLIENT_HELP_VERBS {
            assert!(
                aterm_types::control_verbs::spec(verb).is_none(),
                "{verb} must stay out of the protocol table"
            );
        }
        assert_eq!(
            aterm_types::control_verbs::framing_of("help", "help ls"),
            aterm_types::control_verbs::Framing::Lines
        );
    }

    /// A verb the block does not document has no entry, and the parser reads an
    /// entry as verb line + gutter rows only — a prose line at the 4-column
    /// indent (`None of the three …`) is never mistaken for a verb.
    #[test]
    fn client_verb_entry_reads_only_documented_verbs() {
        assert_eq!(client_verb_entry("text"), None);
        assert_eq!(client_verb_entry("None"), None);
        assert_eq!(client_verb_entry("l"), None, "a prefix is not a verb");
        let ls = client_verb_entry("ls").expect("documented");
        assert!(
            ls[0].starts_with("ls            every session"),
            "{}",
            ls[0]
        );
    }

    /// The Codex allowance names the directory of every socket that was actually
    /// refused, once each and in probe order — never a blanket fleet directory:
    /// an explicit-`$ATERM_CONTROL_SOCK` instance publishes its socket elsewhere,
    /// and a hint the agent pastes must cover that socket too.
    #[test]
    fn the_codex_hint_names_each_denied_directory_once() {
        let probes = vec![
            (1, fleet_sock(1), Probe::Denied(eperm())),
            (2, fleet_sock(2), Probe::Denied(eperm())),
            (
                3,
                "/private/tmp/agent/aterm-3.sock".to_string(),
                Probe::Denied(eperm()),
            ),
            (
                4,
                "/elsewhere/aterm-4.sock".to_string(),
                Probe::Refused {
                    pid_running: Some(false),
                },
            ),
        ];
        let hint = dominant_hint(&probes, Some(Path::new(FLEET))).expect("denied dominates");
        assert!(
            hint.contains(&format!(
                "--allow-unix-socket \"{FLEET}\" --allow-unix-socket \"/private/tmp/agent\""
            )),
            "{hint}"
        );
        assert_eq!(hint.matches("--allow-unix-socket").count(), 2, "{hint}");
        assert!(
            !hint.contains("/elsewhere"),
            "a refused (not denied) socket's dir is not offered: {hint}"
        );
        // A scoped probe (no fleet dir) names the denied socket's own parent.
        let scoped = vec![(
            6,
            "/tmp/run/aterm-6.sock".to_string(),
            Probe::Denied(eacces()),
        )];
        let hint = dominant_hint(&scoped, None).unwrap();
        assert!(hint.contains("--allow-unix-socket \"/tmp/run\""), "{hint}");
        assert_eq!(hint.matches("--allow-unix-socket").count(), 1, "{hint}");
    }

    fn eperm() -> io::Error {
        io::Error::from_raw_os_error(1)
    }

    fn eacces() -> io::Error {
        io::Error::from_raw_os_error(13)
    }

    /// One row: what the directory and the probes were, what the report must
    /// say, what it must NEVER say, and the exit code.
    struct Row {
        name: &'static str,
        dir: DirOutcome,
        probes: Vec<(u32, String, Probe)>,
        code: u8,
        says: &'static [&'static str],
        never_says: &'static [&'static str],
    }

    fn check_row(row: Row) {
        let with_probes = !row.probes.is_empty();
        let (msg, code) = discovery_report(row.dir, &row.probes);
        assert_eq!(code, row.code, "[{}] exit code; report:\n{msg}", row.name);
        for s in row.says {
            assert!(
                msg.contains(s),
                "[{}] must say {s:?}; report:\n{msg}",
                row.name
            );
        }
        for s in row.never_says {
            assert!(
                !msg.contains(s),
                "[{}] must never say {s:?}; report:\n{msg}",
                row.name
            );
        }
        // Exactly ONE hint block whenever sockets were dialled, none otherwise.
        let hints = msg.matches("hint:").count();
        assert_eq!(
            hints,
            usize::from(with_probes),
            "[{}] hint blocks; report:\n{msg}",
            row.name
        );
    }

    const FLEET: &str = "/Users//someone/Library/Application Support/aterm";

    fn fleet_sock(pid: u32) -> String {
        format!("{FLEET}/aterm-{pid}.sock")
    }

    /// The five-condition table of §2.8, plus its `--sock` control row: every
    /// condition that used to print the fleet claim now names ITS cause, and
    /// the claim itself survives only for a readable, empty directory.
    #[test]
    fn discovery_report_names_the_cause_for_every_row_of_the_f8_table() {
        let rows = vec![
            Row {
                name: "XDG_RUNTIME_DIR set, $XDG_RUNTIME_DIR/aterm does not exist",
                dir: DirOutcome::Missing {
                    path: PathBuf::from("/nonexistent/aterm"),
                    why: "resolved from $XDG_RUNTIME_DIR, which is set — so \
                          ~/Library/Application Support/aterm was NOT consulted"
                        .to_string(),
                },
                probes: vec![],
                code: EXIT_UNREACHABLE,
                says: &[
                    "control-socket directory /nonexistent/aterm does not exist",
                    "$XDG_RUNTIME_DIR",
                    "~/Library/Application Support/aterm was NOT consulted",
                ],
                never_says: &[FLEET_CLAIM],
            },
            Row {
                name: "HOME pointed elsewhere",
                dir: DirOutcome::Missing {
                    path: PathBuf::from("/elsewhere/Library/Application Support/aterm"),
                    why: "resolved from $HOME; no aterm instance has published a socket there"
                        .to_string(),
                },
                probes: vec![],
                code: EXIT_UNREACHABLE,
                says: &[
                    "control-socket directory /elsewhere/Library/Application Support/aterm does not exist",
                    "$HOME",
                ],
                never_says: &[FLEET_CLAIM],
            },
            Row {
                name: "env -i (no HOME, no XDG_RUNTIME_DIR)",
                dir: DirOutcome::Unresolvable,
                probes: vec![],
                code: EXIT_UNREACHABLE,
                says: &["cannot resolve the control-socket directory", DIR_ENV_UNSET],
                never_says: &[FLEET_CLAIM],
            },
            Row {
                name: "sandbox denies connect() (EPERM) — the Codex case, beside a stale socket",
                dir: DirOutcome::Found {
                    path: PathBuf::from(FLEET),
                    n: 2,
                },
                probes: vec![
                    (10718, fleet_sock(10718), Probe::Denied(eperm())),
                    (
                        10274,
                        fleet_sock(10274),
                        Probe::Refused {
                            pid_running: Some(false),
                        },
                    ),
                ],
                code: EXIT_UNREACHABLE,
                says: &[
                    "found 2 control sockets in /Users//someone/Library/Application Support/aterm but could not reach any:",
                    "\n  aterm-10718.sock  connect: Operation not permitted (os error 1) — a sandbox is refusing AF_UNIX connect()",
                    "\n  aterm-10274.sock  connect: Connection refused — stale, pid 10274 is not running",
                    "hint: inside Codex CLI run with --allow-unix-socket \"/Users//someone/Library/Application Support/aterm\"",
                    "ask for the command to be escalated",
                    "`aterm ctl --sock <path> sessions` shows one socket's own answer",
                ],
                never_says: &[FLEET_CLAIM, "aterm isn't running"],
            },
            Row {
                name: "token file unreadable → the server answers ERR auth",
                dir: DirOutcome::Found {
                    path: PathBuf::from(FLEET),
                    n: 1,
                },
                probes: vec![(
                    10718,
                    fleet_sock(10718),
                    Probe::NoToken(
                        PathBuf::from(format!("{FLEET}/aterm-10718.token")),
                        eacces(),
                    ),
                )],
                code: EXIT_UNREACHABLE,
                says: &[
                    "found 1 control socket in",
                    "\n  aterm-10718.sock  token /Users//someone/Library/Application Support/aterm/aterm-10718.token unreadable: Permission denied (os error 13)",
                    "hint: the token beside a socket must be readable by THIS user",
                ],
                never_says: &[FLEET_CLAIM, "sockets in"],
            },
            Row {
                name: "(control) stale socket of a dead pid, --sock form",
                dir: DirOutcome::Scoped,
                probes: vec![(
                    10274,
                    "/tmp/aterm-10274.sock".to_string(),
                    Probe::Refused {
                        pid_running: Some(false),
                    },
                )],
                code: EXIT_UNREACHABLE,
                says: &[
                    "the addressed instance did not answer (wrong --sock/--pid, not running, or a different user):",
                    "\n  /tmp/aterm-10274.sock  connect: Connection refused — stale, pid 10274 is not running",
                    "open -a aterm",
                ],
                never_says: &[FLEET_CLAIM, "found 1 control socket"],
            },
        ];
        for row in rows {
            check_row(row);
        }
    }

    /// The remaining causes, the exit-code ladder, and the report's shape rules:
    /// `Empty` is the ONE arm that says the claim (and names where it looked),
    /// every timeout is 124, one timeout beside a refusal is 2, an out-of-dir
    /// socket keeps its full path, ties for the hint go to the actionable cause,
    /// and an answered fleet reports nothing.
    #[test]
    fn discovery_report_covers_stale_running_auth_timeout_and_empty() {
        let rows = vec![
            Row {
                name: "readable, empty directory — the one honest fleet claim",
                dir: DirOutcome::Empty(PathBuf::from(FLEET)),
                probes: vec![],
                code: 1,
                says: &[
                    "no live aterm instances found (looked in /Users//someone/Library/Application Support/aterm)",
                ],
                never_says: &["could not"],
            },
            Row {
                name: "directory exists but readdir is refused",
                dir: DirOutcome::Unreadable {
                    path: PathBuf::from(FLEET),
                    err: eperm(),
                },
                probes: vec![],
                code: EXIT_UNREACHABLE,
                says: &[
                    "control-socket directory /Users//someone/Library/Application Support/aterm exists but cannot be read: Operation not permitted (os error 1)",
                    "readdir()",
                ],
                never_says: &[FLEET_CLAIM],
            },
            Row {
                name: "refused, but the pid IS running (still booting)",
                dir: DirOutcome::Found {
                    path: PathBuf::from(FLEET),
                    n: 1,
                },
                probes: vec![(
                    4242,
                    fleet_sock(4242),
                    Probe::Refused {
                        pid_running: Some(true),
                    },
                )],
                code: EXIT_UNREACHABLE,
                says: &[
                    "\n  aterm-4242.sock  connect: Connection refused — pid 4242 is running but nothing is serving this socket",
                    "hint: a pid that is running but not serving is still starting up",
                ],
                never_says: &[FLEET_CLAIM, "stale"],
            },
            Row {
                name: "refused, pid unknown (a --sock name that encodes none)",
                dir: DirOutcome::Scoped,
                probes: vec![(
                    0,
                    "/tmp/run/c.sock".to_string(),
                    Probe::Refused { pid_running: None },
                )],
                code: EXIT_UNREACHABLE,
                says: &[
                    "\n  /tmp/run/c.sock  connect: Connection refused — nothing is serving this socket (it names no pid)",
                ],
                never_says: &[FLEET_CLAIM, "pid 0"],
            },
            Row {
                name: "the token on disk is not the one the server holds",
                dir: DirOutcome::Found {
                    path: PathBuf::from(FLEET),
                    n: 1,
                },
                probes: vec![(10718, fleet_sock(10718), Probe::AuthRejected)],
                code: EXIT_UNREACHABLE,
                says: &[
                    "\n  aterm-10718.sock  server rejected the token (ERR auth) — a restarted instance rewrites its token; re-run",
                    "hint: re-run once",
                ],
                never_says: &[FLEET_CLAIM],
            },
            Row {
                name: "every socket timed out → 124",
                dir: DirOutcome::Found {
                    path: PathBuf::from(FLEET),
                    n: 2,
                },
                probes: vec![
                    (1, fleet_sock(1), Probe::Timeout),
                    (2, fleet_sock(2), Probe::Timeout),
                ],
                code: EXIT_TIMEOUT,
                says: &[
                    "\n  aterm-1.sock  timed out after 2s",
                    "\n  aterm-2.sock  timed out after 2s",
                    "hint: no answer within 2s",
                    "--timeout 30 sessions",
                ],
                never_says: &[FLEET_CLAIM],
            },
            Row {
                name: "one timeout beside a refusal → 2, not 124",
                dir: DirOutcome::Found {
                    path: PathBuf::from(FLEET),
                    n: 2,
                },
                probes: vec![
                    (1, fleet_sock(1), Probe::Timeout),
                    (
                        2,
                        fleet_sock(2),
                        Probe::Refused {
                            pid_running: Some(false),
                        },
                    ),
                ],
                code: EXIT_UNREACHABLE,
                says: &[
                    "timed out after 2s",
                    "stale, pid 2 is not running",
                    "hint: no answer within 2s",
                ],
                never_says: &[FLEET_CLAIM],
            },
            Row {
                name: "an explicit-socket instance keeps its out-of-dir path; Other passes the cause through",
                dir: DirOutcome::Found {
                    path: PathBuf::from(FLEET),
                    n: 1,
                },
                probes: vec![(
                    77,
                    "/private/tmp/agent/x.sock".to_string(),
                    Probe::Other(io::Error::new(
                        io::ErrorKind::NotFound,
                        "connect: No such file or directory (os error 2)",
                    )),
                )],
                code: EXIT_UNREACHABLE,
                says: &[
                    "\n  /private/tmp/agent/x.sock  connect: No such file or directory (os error 2)",
                    "hint: `aterm ctl --sock <path> sessions`",
                ],
                never_says: &[FLEET_CLAIM],
            },
            Row {
                name: "tie for the hint: one denied, one stale → the Codex allowance wins",
                dir: DirOutcome::Scoped,
                probes: vec![
                    (
                        5,
                        "/tmp/run/aterm-5.sock".to_string(),
                        Probe::Refused {
                            pid_running: Some(false),
                        },
                    ),
                    (
                        6,
                        "/tmp/run/aterm-6.sock".to_string(),
                        Probe::Denied(eacces()),
                    ),
                ],
                code: EXIT_UNREACHABLE,
                says: &[
                    "\n  /tmp/run/aterm-6.sock  connect: Permission denied (os error 13) — another user's socket, or a sandbox refusing AF_UNIX connect()",
                    "hint: inside Codex CLI run with --allow-unix-socket \"/tmp/run\"",
                ],
                never_says: &[FLEET_CLAIM, "open -a aterm"],
            },
        ];
        for row in rows {
            check_row(row);
        }

        // An answered probe is the success path: nothing to report, exit 0 —
        // and a stale neighbour is NOT mentioned (no noise on success).
        let (msg, code) = discovery_report(
            DirOutcome::Found {
                path: PathBuf::from(FLEET),
                n: 2,
            },
            &[
                (
                    10718,
                    fleet_sock(10718),
                    Probe::Answered(vec!["r s-1".to_string()]),
                ),
                (
                    10274,
                    fleet_sock(10274),
                    Probe::Refused {
                        pid_running: Some(false),
                    },
                ),
            ],
        );
        assert_eq!((msg.as_str(), code), ("", 0));
    }

    /// The per-socket label is aligned into a column so the causes line up.
    #[test]
    fn discovery_report_aligns_the_cause_column() {
        let (msg, _) = discovery_report(
            DirOutcome::Found {
                path: PathBuf::from(FLEET),
                n: 2,
            },
            &[
                (7, fleet_sock(7), Probe::Timeout),
                (10718, fleet_sock(10718), Probe::Timeout),
            ],
        );
        assert!(msg.contains("\n  aterm-7.sock      timed out"), "{msg}");
        assert!(msg.contains("\n  aterm-10718.sock  timed out"), "{msg}");
    }

    /// `connect()` failures classify by KIND, never by guess: a permission
    /// error is Denied (a sandbox or a foreign user), a refusal is Refused with
    /// no liveness verdict for a pid-less socket, a deadline is Timeout, and
    /// the rest keep the raw cause under the `connect:` frame.
    #[test]
    fn connect_failures_classify_by_kind_not_by_guess() {
        assert!(matches!(
            classify_connect_error(eperm(), 0),
            Probe::Denied(_)
        ));
        assert!(matches!(
            classify_connect_error(eacces(), 0),
            Probe::Denied(_)
        ));
        assert!(matches!(
            classify_connect_error(io::Error::new(io::ErrorKind::NotConnected, "x"), 0),
            Probe::Denied(_)
        ));
        assert!(matches!(
            classify_connect_error(io::Error::new(io::ErrorKind::ConnectionRefused, "x"), 0),
            Probe::Refused { pid_running: None }
        ));
        assert!(matches!(
            classify_connect_error(
                io::Error::new(io::ErrorKind::ConnectionRefused, "x"),
                DEAD_PID
            ),
            Probe::Refused {
                pid_running: Some(false)
            }
        ));
        assert!(matches!(
            classify_connect_error(io::Error::new(io::ErrorKind::WouldBlock, "x"), 0),
            Probe::Timeout
        ));
        match classify_connect_error(io::Error::new(io::ErrorKind::NotFound, "gone"), 0) {
            Probe::Other(e) => {
                assert_eq!(e.kind(), io::ErrorKind::NotFound);
                assert_eq!(e.to_string(), "connect: gone");
            }
            other => panic!("a vanished socket is Other, got {other:?}"),
        }
        assert!(matches!(
            io_probe(io::Error::new(io::ErrorKind::WouldBlock, "x")),
            Probe::Timeout
        ));
        assert!(matches!(
            io_probe(io::Error::new(io::ErrorKind::TimedOut, "x")),
            Probe::Timeout
        ));
        assert!(matches!(
            io_probe(io::Error::new(io::ErrorKind::BrokenPipe, "x")),
            Probe::Other(_)
        ));
    }

    /// A token miss names the FILE it looked for (the per-instance mirror,
    /// resolved in the socket's own directory), so the report can print it;
    /// the `Option` wrapper other callers use is unchanged.
    #[test]
    fn a_token_miss_names_the_file_it_looked_for() {
        let (path, err) =
            read_token_at("/nonexistent-aterm-dir/aterm-77.sock").expect_err("no such token");
        assert_eq!(path, Path::new("/nonexistent-aterm-dir/aterm-77.token"));
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(read_token_for("/nonexistent-aterm-dir/aterm-77.sock").is_none());
    }

    /// A mock instance at `dir/<name>` that accepts ONE connection, reads
    /// request lines past any `AUTH` line, answers `reply` (an EMPTY reply
    /// hangs up instead), then drains until the client closes. Returns the
    /// socket path and the thread, whose value is the request line it saw.
    #[cfg(unix)]
    fn mock_instance(
        dir: &Path,
        name: &str,
        reply: &'static [u8],
    ) -> (String, std::thread::JoinHandle<String>) {
        use std::io::{BufRead, Read, Write};
        let sock = dir.join(name);
        let _ = std::fs::remove_file(&sock);
        let listener = aterm_uds::CtlListener::bind(&sock).expect("bind mock instance");
        let srv = std::thread::spawn(move || {
            let (conn, _) = listener.accept().expect("accept");
            let mut r = std::io::BufReader::new(conn.try_clone().expect("clone"));
            let mut line = String::new();
            loop {
                line.clear();
                let n = r.read_line(&mut line).expect("read request line");
                if n == 0 || !line.starts_with("AUTH ") {
                    break;
                }
            }
            if reply.is_empty() {
                return line; // hang up without a word
            }
            let mut w = conn;
            w.write_all(reply).expect("write reply");
            w.flush().expect("flush");
            let mut sink = Vec::new();
            let _ = r.read_to_end(&mut sink);
            line
        });
        (sock.to_str().expect("utf8 path").to_string(), srv)
    }

    /// The probe against REAL sockets: a bound-then-abandoned socket file is
    /// Refused (labelled by the pid's liveness), a missing file is Other,
    /// `ERR auth` is NoToken when nothing was on disk and AuthRejected when a
    /// token WAS sent, `OK n` is Answered, a non-auth `ERR` keeps the server's
    /// words, and a hang-up is Other(UnexpectedEof). UNIX ONLY: the mock is a
    /// std `UnixListener`.
    #[cfg(unix)]
    #[test]
    fn probe_lines_classifies_real_sockets() {
        let dir = std::env::temp_dir().join(format!("aterm-ctl-f8-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("private dir");

        // Refused: a socket FILE with nobody accepting — bind, then drop the
        // listener; the file stays behind exactly like a crashed instance's.
        let stale = dir.join(control_socket::instance_sock_name(DEAD_PID));
        drop(aterm_uds::CtlListener::bind(&stale).expect("bind then abandon"));
        let stale_s = stale.to_str().expect("utf8").to_string();
        assert!(matches!(
            probe_lines(&stale_s, "sessions", DEAD_PID),
            Probe::Refused {
                pid_running: Some(false)
            }
        ));
        assert!(matches!(
            probe_lines(&stale_s, "sessions", 0),
            Probe::Refused { pid_running: None }
        ));
        assert!(matches!(
            probe_lines(&stale_s, "sessions", std::process::id()),
            Probe::Refused {
                pid_running: Some(true)
            }
        ));

        // Vanished: no such file at all.
        let gone = dir.join("aterm-1.sock");
        match probe_lines(gone.to_str().expect("utf8"), "sessions", 1) {
            Probe::Other(e) => {
                assert_eq!(e.kind(), io::ErrorKind::NotFound);
                assert!(e.to_string().starts_with("connect: "), "{e}");
            }
            other => panic!("a missing socket file is Other(NotFound), got {other:?}"),
        }

        // NoToken: a live mock with NO token beside it answers ERR auth, and
        // the request went out with no AUTH line.
        let (sock, srv) = mock_instance(&dir, "mock.sock", b"ERR auth\n");
        match probe_lines(&sock, "sessions", 0) {
            // The miss names the PER-SOCKET file this build writes, not the
            // legacy shared one.
            Probe::NoToken(path, _) => assert_eq!(path, dir.join("mock.sock.token")),
            other => panic!("no token on disk + ERR auth is NoToken, got {other:?}"),
        }
        assert_eq!(srv.join().expect("mock"), "sessions\n");

        // AuthRejected: the token IS on disk, was sent, and still ERR auth.
        std::fs::write(dir.join("mock.sock.token"), "deadbeef\n").expect("token");
        let (sock, srv) = mock_instance(&dir, "mock.sock", b"ERR auth\n");
        assert!(matches!(
            probe_lines(&sock, "sessions", 0),
            Probe::AuthRejected
        ));
        assert_eq!(srv.join().expect("mock"), "sessions\n");

        // Answered: the happy path, body rows trimmed of CR/LF.
        let (sock, srv) = mock_instance(&dir, "mock.sock", b"OK 2\nalpha\r\nbeta\n");
        match probe_lines(&sock, "sessions", 0) {
            Probe::Answered(lines) => assert_eq!(lines, vec!["alpha", "beta"]),
            other => panic!("OK 2 + two rows is Answered, got {other:?}"),
        }
        srv.join().expect("mock");

        // Other: a non-auth ERR keeps the server's words.
        let (sock, srv) = mock_instance(&dir, "mock.sock", b"ERR usage: sessions\n");
        match probe_lines(&sock, "sessions", 0) {
            Probe::Other(e) => assert_eq!(e.to_string(), "server replied: ERR usage: sessions"),
            other => panic!("a non-auth ERR is Other, got {other:?}"),
        }
        srv.join().expect("mock");

        // Other: hung up without a reply.
        let (sock, srv) = mock_instance(&dir, "mock.sock", b"");
        match probe_lines(&sock, "sessions", 0) {
            Probe::Other(e) => assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof),
            other => panic!("a hang-up is Other(UnexpectedEof), got {other:?}"),
        }
        srv.join().expect("mock");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `--help` documents the discovery exit-code ladder (1 = truly empty,
    /// 2 = could not look / could not reach, 124 = every socket timed out) and
    /// the directory rule that makes a missing `$XDG_RUNTIME_DIR/aterm` an
    /// environment fact rather than a fleet fact.
    #[test]
    fn help_documents_the_discovery_exit_codes_and_the_dir_rule() {
        let help = help_text();
        assert!(
            help.contains("could not LOOK, or could not REACH"),
            "exit 2 is explained"
        );
        assert!(
            help.contains("`no live aterm instances found`"),
            "exit 1's one meaning is named"
        );
        assert!(
            help.contains("for `ls`/`instances`/`windows`,\n         EVERY found socket timed out"),
            "124 for discovery, every discovery verb named"
        );
        // The ladder is three verbs tall on every rung, not two: `windows` shares
        // `ls`/`instances`' codes and used to be missing from all four lines.
        assert!(
            help.contains("(`ls`, `instances`, `windows`): at least one instance answered"),
            "exit 0 names windows"
        );
        assert!(
            help.contains("`ls`/`instances`/`windows`: the control-socket directory was readable"),
            "exit 1 names windows"
        );
        assert!(
            help.contains("`ls`/`instances`/`windows` only — could not LOOK, or could not REACH"),
            "exit 2 names windows"
        );
        assert!(
            help.contains("DISCOVERY (`ls`, `instances`, `windows`)"),
            "the resolution rule has a paragraph naming every discovery verb"
        );
        assert!(help.contains("is NOT consulted"), "the XDG rule is stated");
    }

    /// The mirror must resolve in the socket's OWN directory — the concrete
    /// regressions: an explicit `$ATERM_CONTROL_SOCK` path like `/tmp/c.sock`
    /// pairs with `/tmp/c.sock.token`, NEVER the hand-rolled `/tmp/c.token`
    /// (which no server writes) and no longer the directory-wide
    /// `/tmp/aterm.token` (which the next explicit socket would overwrite).
    #[test]
    fn uds_token_path_resolves_in_the_socket_dir() {
        let p = aterm_uds::latest::token_path_for_sock("/tmp/run/c.sock")
            .expect("a socket path with a parent resolves");
        assert_eq!(p, Path::new("/tmp/run/c.sock.token"));

        let inst = aterm_uds::latest::token_path_for_sock("/tmp/run/aterm-77.sock")
            .expect("instance socket resolves");
        assert_eq!(inst, Path::new("/tmp/run/aterm-77.token"));
    }

    /// The `attach` lane's foreground handoff must name the RECEIVING instance,
    /// and all the forward path holds is a socket path. Both reachable shapes
    /// resolve: a per-instance socket names its pid outright, and the flagless
    /// `latest` alias is a pointer FILE whose contents name it. Anything that
    /// names no instance stays `None` — a pid guessed out of an arbitrary
    /// filename would hand the foreground right to an unrelated process.
    #[cfg(windows)]
    #[test]
    fn the_forward_target_pid_comes_from_the_socket_name_or_its_alias() {
        let dir = std::env::temp_dir().join(format!("aterm-ctl-fgpid-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let named = dir.join("aterm-77.sock");
        assert_eq!(
            instance_pid_of(named.to_str().expect("utf8 path")),
            Some(77),
            "a per-instance socket names its own pid"
        );

        let alias = dir.join(control_socket::LATEST_SOCK_FILE);
        std::fs::write(&alias, "aterm-4242.sock\n").expect("write the latest pointer file");
        assert_eq!(
            instance_pid_of(alias.to_str().expect("utf8 path")),
            Some(4242),
            "the flagless alias resolves to the instance it points at"
        );

        // An explicit `$ATERM_CONTROL_SOCK` override names no instance.
        assert_eq!(
            instance_pid_of(dir.join("c.sock").to_str().expect("utf8")),
            None
        );
        // A pointer file that does not name an instance socket resolves to
        // nothing rather than to the alias's own (pid-less) name.
        std::fs::write(&alias, "..\\..\\somewhere-else.sock").expect("rewrite the pointer file");
        assert_eq!(instance_pid_of(alias.to_str().expect("utf8 path")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The front door's liveness probe must dial the instance the flagless
    /// alias POINTS AT. On Windows that alias is a pointer file, so probing it
    /// directly can never connect — and this probe is the entire input to
    /// `route_launch`'s `instance_reachable`, so the whole `attach` lane
    /// (`aterm new-tab` forwarding, and the jump list's New Tab task) silently
    /// fell back to opening a new window. Measured with a real listener bound
    /// at the instance path and a real pointer file beside it.
    #[test]
    fn the_front_door_probe_dials_through_the_latest_alias() {
        let dir = std::env::temp_dir().join(format!("aterm-ctl-probe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("private probe dir");
        let instance = dir.join(control_socket::instance_sock_name(std::process::id()));
        let alias = dir.join(control_socket::LATEST_SOCK_FILE);
        let alias_str = alias.to_str().expect("utf8 path").to_string();

        // Nothing bound yet: the alias exists but the instance is a corpse.
        aterm_uds::latest::publish(&alias, instance.to_str().expect("utf8 path"));
        assert_eq!(
            front_door_probe(&alias_str),
            None,
            "a dangling alias must not route a tab into a corpse"
        );

        // A live listener at the instance path — the probe must find it THROUGH
        // the alias. What it answers with is PLATFORM LAW, not preference:
        // on Windows the alias is a pointer file no connect can dial, so the
        // probe must answer the RESOLVED instance path (the forward and the
        // pid it grants foreground to must name the same instance); on unix
        // the alias is a symlink the kernel follows during `connect` itself,
        // `latest::resolve` is the documented identity, and the probe answers
        // the alias back — resolving by hand there would be a second answer
        // to a question the kernel already owns.
        let listener = aterm_uds::CtlListener::bind(&instance).expect("bind the mock instance");
        #[cfg(windows)]
        let expected = instance.to_str().expect("utf8 path");
        #[cfg(not(windows))]
        let expected = alias_str.as_str();
        assert_eq!(
            front_door_probe(&alias_str).as_deref(),
            Some(expected),
            "the probe must dial the live instance through the alias"
        );
        drop(listener);
        let _ = std::fs::remove_file(&instance);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // THE MULTIPLEXER BOUNDARY
    //
    // Measured on this host with GNU screen 4.09.01 against a real headless
    // instance before any of this existed: inside a `screen` window the whole
    // run collapses into ONE outer block (`cmdline=screen%20-S%20muxprobe`,
    // still `executing`), `meta` reports `title=` empty, and a flagless
    // `aterm ctl status` typed in the window answered `OK … phase=quiet` FROM
    // THE OUTER SESSION — success, wrong terminal, no signal. tmux is not
    // installed here; it is covered by construction rather than by claim (see
    // the tmux-shaped cases below): its `$TMUX` marker is checked FIRST, and
    // its default `TERM` is `screen-256color`, which is exactly why the kind is
    // taken from the marker and only the family from `TERM`.
    // -----------------------------------------------------------------------

    /// Tier 1: the shell integration's own `$ATERM_MUX` marker decides, and its
    /// disable spellings turn the whole guard off (the escape hatch for a stale
    /// marker inherited by a shell that is not in a pane at all).
    #[test]
    fn mux_marker_decides_and_its_disable_spellings_turn_the_guard_off() {
        let n = detect_mux_nesting(
            Some("screen"),
            None,
            Some("s-outer"),
            None,
            None,
            None,
            None,
        )
        .expect("a marked boundary is nested");
        assert_eq!(n.kind, "screen");
        assert_eq!(n.outer_sid.as_deref(), Some("s-outer"));
        assert_eq!(
            n.detected_by, "shell-integration",
            "the in-pane marker is the top tier"
        );
        // The marker wins even when nothing else in the environment agrees —
        // including a session base that would otherwise answer "no crossing".
        let n = detect_mux_nesting(
            Some("tmux"),
            Some("|"),
            None,
            None,
            None,
            Some("xterm-256color"),
            Some("s-env"),
        )
        .expect("marked");
        assert_eq!(n.kind, "tmux");
        assert_eq!(
            n.outer_sid.as_deref(),
            Some("s-env"),
            "$ATERM_PARENT_SESSION_ID stands in when the marker carries no outer sid"
        );
        for off in ["0", "off", "no", "none", "false"] {
            assert_eq!(
                detect_mux_nesting(
                    Some(off),
                    Some("|"),
                    Some("s-outer"),
                    Some("/tmp/tmux-1000/default,9,0"),
                    None,
                    Some("screen-256color"),
                    Some("s-outer"),
                ),
                None,
                "ATERM_MUX={off} must disable the guard outright"
            );
        }
    }

    /// Tier 2 — the tier that actually answers in a real pane.
    ///
    /// THE MEASUREMENT THIS EXISTS FOR: in a real GNU screen 4.09.01 window
    /// under a headless aterm on this host, the pane shell had `$STY`,
    /// `TERM=screen.xterm-256color` and the inherited loader guard, and
    /// `$ATERM_MUX` was EMPTY with no integration hook defined — because aterm
    /// injects bash through `--rcfile`, which a shell started by screen never
    /// receives. Tier 1 is therefore unreachable from a bash or zsh pane, and
    /// the session shell's own `$ATERM_MUX_BASE` is what crosses instead.
    #[test]
    fn mux_session_base_decides_a_pane_without_reading_term() {
        // A session shell born outside any multiplexer stamps "|"; the pane's
        // own $STY no longer matches, and that mismatch alone is the crossing.
        let n = detect_mux_nesting(
            None,
            Some("|"),
            None,
            None,
            Some("4242.pts-3.host"),
            Some("screen.xterm-256color"),
            Some("s-outer"),
        )
        .expect("STY differing from the session base is a pane");
        assert_eq!(n.kind, "screen");
        assert_eq!(n.outer_sid.as_deref(), Some("s-outer"));
        assert_eq!(n.detected_by, "session-base");
        // THE GAP TIER 3 CANNOT SEE: tmux with `default-terminal
        // "xterm-256color"` looks exactly like an aterm window to TERM. The base
        // never reads TERM, so it calls this a pane anyway.
        let n = detect_mux_nesting(
            None,
            Some("|"),
            None,
            Some("/tmp/tmux-1000/default,9,0"),
            None,
            Some("xterm-256color"),
            Some("s-outer"),
        )
        .expect("a tmux pane with aterm's own TERM is still a pane");
        assert_eq!(n.kind, "tmux", "the marker names the program, not TERM");
        assert_eq!(n.detected_by, "session-base");
        // An aterm window LAUNCHED FROM a pane re-runs the integration and
        // re-stamps the base as its own, so its inherited $TMUX matches and it
        // is NOT refused — the case the loader guard was invented for, decided
        // here without TERM.
        assert_eq!(
            detect_mux_nesting(
                None,
                Some("/tmp/tmux-1000/default,9,0|"),
                None,
                Some("/tmp/tmux-1000/default,9,0"),
                None,
                Some("xterm-256color"),
                Some("s-fresh"),
            ),
            None,
            "a base equal to this environment means nothing was entered since"
        );
        // A nested multiplexer inside that window is a crossing again.
        let n = detect_mux_nesting(
            None,
            Some("/tmp/tmux-1000/default,9,0|"),
            None,
            Some("/tmp/tmux-1000/default,9,7"),
            None,
            Some("screen-256color"),
            Some("s-fresh"),
        )
        .expect("a DIFFERENT tmux than the session's own is a pane");
        assert_eq!(n.kind, "tmux");
        // Different base, but no marker names a multiplexer here (someone unset
        // $TMUX by hand): there is nothing to report, and inventing a kind from
        // TERM would be the papering-over this whole seam refuses.
        assert_eq!(
            detect_mux_nesting(
                None,
                Some("/tmp/tmux-1000/default,9,0|"),
                None,
                None,
                None,
                Some("screen-256color"),
                Some("s-fresh"),
            ),
            None,
            "a base mismatch with no live marker names no multiplexer"
        );
    }

    /// Tier 3, for a shell carrying neither mark — a session started before the
    /// base existed, or one aterm did not start. It needs BOTH an aterm identity
    /// to mis-target AND a multiplexer-written `TERM`.
    ///
    /// The `TERM` corroboration is what keeps an aterm window LAUNCHED FROM a
    /// pane out of the refusal: it inherits `$TMUX`/`$STY` verbatim, and only
    /// `TERM` (which aterm's spawn seam forces to `xterm-256color`) tells it
    /// apart from a real pane.
    #[test]
    fn mux_fallback_needs_an_aterm_identity_and_a_multiplexer_term() {
        let n = detect_mux_nesting(
            None,
            None,
            None,
            None,
            Some("4242.pts-3.host"),
            Some("screen.xterm-256color"),
            Some("s-outer"),
        )
        .expect("STY + a screen TERM inside an aterm session is a pane");
        assert_eq!(n.kind, "screen");
        assert_eq!(n.outer_sid.as_deref(), Some("s-outer"));
        assert_eq!(n.detected_by, "environment", "no mark was present");
        // tmux's DEFAULT TERM is screen-256color, so the marker — not TERM —
        // names the program. Reporting "screen" for a tmux pane would send the
        // reader to the wrong manual.
        let n = detect_mux_nesting(
            None,
            None,
            None,
            Some("/tmp/tmux-1000/default,9,0"),
            None,
            Some("screen-256color"),
            Some("s-outer"),
        )
        .expect("tmux pane");
        assert_eq!(n.kind, "tmux");
        // An aterm window launched FROM a pane: markers inherited, TERM aterm's.
        assert_eq!(
            detect_mux_nesting(
                None,
                None,
                None,
                Some("/tmp/tmux-1000/default,9,0"),
                Some("4242.pts-3.host"),
                Some("xterm-256color"),
                Some("s-fresh"),
            ),
            None,
            "a stale $TMUX/$STY with aterm's own TERM is not a pane"
        );
        // A screen inside a NON-aterm terminal has no aterm identity to
        // mis-target: flagless calls there legitimately drive the user's
        // windows through the `latest` pointer, and must not be refused.
        assert_eq!(
            detect_mux_nesting(
                None,
                None,
                None,
                None,
                Some("1.pts-0.h"),
                Some("screen"),
                None
            ),
            None,
            "no aterm session identity in scope means nothing to mis-target"
        );
        // Empty is unset, everywhere (the spawn seam and the user's environment
        // both deliver empty values) — including the base, whose real spelling
        // is never shorter than "|".
        assert_eq!(
            detect_mux_nesting(
                Some(""),
                Some(""),
                Some(""),
                Some(""),
                Some(""),
                Some(""),
                Some("")
            ),
            None
        );
    }

    /// Which invocation shapes name their target only IMPLICITLY — the ones a
    /// multiplexer silently redirects to the outer terminal.
    #[test]
    fn implicit_self_targeting_is_exactly_the_flagless_at_self_and_at_dot_shapes() {
        let parts = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<String>>();
        // Flagless with nothing pinned: the case the audit found.
        assert!(targets_own_session_implicitly(
            &parts(&["text"]),
            None,
            None,
            None
        ));
        // Any explicit instance pin is a deliberate choice, not silence.
        assert!(!targets_own_session_implicitly(
            &parts(&["text"]),
            Some("/tmp/x.sock"),
            None,
            None
        ));
        assert!(!targets_own_session_implicitly(
            &parts(&["text"]),
            None,
            Some(42),
            None
        ));
        assert!(!targets_own_session_implicitly(
            &parts(&["text"]),
            None,
            None,
            Some("/tmp/explicit.sock")
        ));
        // `@self`/`@env` CLAIM this session by name — false in a pane however
        // the socket was chosen, so a pin does not rescue them.
        for sel in ["@self", "@env"] {
            assert!(targets_own_session_implicitly(
                &parts(&[sel, "send", "hi"]),
                Some("/tmp/x.sock"),
                Some(42),
                None
            ));
        }
        // …including inside `subscribe`'s selector-SECOND position and a list.
        assert!(targets_own_session_implicitly(
            &parts(&["subscribe", "@self,@1", "screen"]),
            None,
            None,
            None
        ));
        // `@.` follows the ACTIVE tab — as implicit as flagless, and pinned the
        // same way.
        assert!(targets_own_session_implicitly(
            &parts(&["@.", "text"]),
            None,
            None,
            None
        ));
        assert!(!targets_own_session_implicitly(
            &parts(&["@.", "text"]),
            None,
            Some(7),
            None
        ));
        // A concrete sid names its target; nothing implicit is left.
        assert!(!targets_own_session_implicitly(
            &parts(&["@s-abc", "text"]),
            None,
            None,
            None
        ));
        // The client-answered verbs address no session at all.
        for verb in MUX_EXEMPT_VERBS {
            assert!(
                !targets_own_session_implicitly(&parts(&[verb]), None, None, None),
                "{verb} addresses no session"
            );
        }
        // Neither does anything the shared table calls Target::Meta. Refusing
        // these protected nothing and the refusal's own advice was a dead end:
        // `@<sid> sessions` is `ERR denied` at the server, measured against a
        // live instance for sessions/who/whoami/flows. Driven from the TABLE, so
        // a verb added there as Meta is exempt the day it lands.
        let meta: Vec<&str> = aterm_types::control_verbs::VERBS
            .iter()
            .filter(|s| matches!(s.target, aterm_types::control_verbs::Target::Meta))
            .map(|s| s.name)
            .collect();
        assert!(
            meta.len() > 10,
            "the table should carry the whole session-less family: {meta:?}"
        );
        for verb in [
            "version", "sessions", "who", "whoami", "grant", "flows", "help",
        ] {
            assert!(meta.contains(&verb), "{verb} must be Target::Meta");
        }
        for verb in &meta {
            assert!(
                !targets_own_session_implicitly(&parts(&[verb]), None, None, None),
                "{verb} addresses no session and cannot be mis-targeted by a multiplexer"
            );
        }
        // The exemption is exactly the session-less set — a SESSION or APP verb
        // is still refused, because those really can land in the wrong terminal.
        for verb in ["text", "send", "key", "blocks", "window", "tab", "spawn"] {
            assert!(
                targets_own_session_implicitly(&parts(&[verb]), None, None, None),
                "{verb} names a real target and stays guarded"
            );
        }
        // Empty argv is the usage error's business, not this guard's.
        assert!(!targets_own_session_implicitly(&[], None, None, None));
    }

    /// The refusal has to be ACTIONABLE, not just correct: it names the session
    /// a flagless call would have driven, the explicit form that drives it on
    /// purpose, the two instance pins, and the way to switch the guard off.
    #[test]
    fn nested_self_drive_error_names_the_outer_session_and_every_way_forward() {
        let n = MuxNesting {
            kind: "tmux",
            outer_sid: Some("s-1a2b".to_string()),
            detected_by: "session-base",
        };
        let e = nested_self_drive_error(&n);
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
        let msg = e.to_string();
        for needle in [
            "inside tmux",
            "s-1a2b",
            "aterm ctl @s-1a2b <verb>",
            "--pid",
            "--sock",
            "aterm ctl instances",
            "aterm ctl mux",
            "ATERM_MUX=0",
            "OSC 133",
        ] {
            assert!(msg.contains(needle), "refusal must name {needle:?}: {msg}");
        }
        // EVERY remedy it names must be one the reachable set accepts. `@<sid>`
        // is offered to the verbs that survive the guard — Session and App — and
        // never to a session-less one, where the server answers `ERR denied` to
        // a selector. That is the property, not the wording: if some Meta verb
        // ever became refusable again, this fails.
        let parts = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<String>>();
        for spec in aterm_types::control_verbs::VERBS {
            if matches!(spec.target, aterm_types::control_verbs::Target::Meta) {
                assert!(
                    !targets_own_session_implicitly(&parts(&[spec.name]), None, None, None),
                    "{} is session-less, so `@<sid> {}` must never be prescribed \
                     — the server rejects a selector there",
                    spec.name,
                    spec.name
                );
            }
        }
    }

    /// The block-model caveat rides only on the verbs whose ANSWER is shaped by
    /// the boundary, and only when the request actually addresses the session
    /// hosting the multiplexer.
    #[test]
    fn mux_degradation_note_marks_the_block_reading_verbs_only() {
        let parts = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<String>>();
        let n = MuxNesting {
            kind: "screen",
            outer_sid: Some("s-1a2b".to_string()),
            detected_by: "environment",
        };
        for verb in ["blocks", "status"] {
            let note = mux_degradation_note(&n, &parts(&[verb]))
                .unwrap_or_else(|| panic!("{verb} reads the block model"));
            assert!(note.contains("inside screen"), "{note}");
            assert!(note.contains("s-1a2b"), "{note}");
            assert!(note.contains("ABSENT"), "absent, not empty: {note}");
            // The same verb aimed at the hosting session explicitly still earns
            // the note — that IS the session the multiplexer is starving.
            let sel = mux_degradation_note(&n, &parts(&["@s-1a2b", verb]));
            assert!(sel.is_some(), "{verb} at the outer sid still applies");
        }
        // A different session is not the one the pane is starving.
        assert_eq!(
            mux_degradation_note(&n, &parts(&["@s-other", "blocks"])),
            None
        );
        // Verbs whose answer the boundary does not shape stay quiet.
        for verb in ["text", "cursor", "send", "meta"] {
            assert_eq!(
                mux_degradation_note(&n, &parts(&[verb])),
                None,
                "{verb} does not read the block model"
            );
        }
        // No aterm identity, no note to give.
        let anon = MuxNesting {
            kind: "screen",
            outer_sid: None,
            detected_by: "shell-integration",
        };
        assert_eq!(mux_degradation_note(&anon, &parts(&["blocks"])), None);
    }

    /// `mux` is a CLIENT verb, so it must be discoverable exactly where the
    /// other two are: the `--help` client block and the completion verb list.
    #[test]
    fn the_mux_client_verb_is_documented_and_completable() {
        let help = help_text();
        assert!(
            help.contains("    mux "),
            "the mux verb is in --help: {help}"
        );
        assert!(
            help.contains("ATERM_MUX=0"),
            "--help names the escape hatch"
        );
        let verbs = completion_verb_list();
        assert!(
            verbs.split_whitespace().any(|v| v == "mux"),
            "mux completes like ls/instances"
        );
    }

    /// SIGNAL #1's stamp, which is the whole reason the sentence is said once
    /// and not once per call — and which `aterm ctl` now has to compute
    /// IDENTICALLY to the shell integration, because the two halves share it.
    ///
    /// The shell spells it
    /// `${XDG_RUNTIME_DIR:-${TMPDIR:-/tmp}}/aterm/mux-notice/<kind>-<id>` with
    /// `id=${TMUX:-${STY:-$TERM}}` and `${id//[!A-Za-z0-9._-]/_}`. Anything else
    /// here and a fish pane would be told twice.
    #[test]
    fn the_mux_notice_stamp_matches_the_shell_integrations_path_exactly() {
        // tmux's marker wins the id, and every byte outside the shell's class
        // (`/`, `,`) becomes `_` — one substitution per character, not per run.
        assert_eq!(
            mux_notice_stamp(
                "tmux",
                Some("/tmp/tmux-1000/default,9,0"),
                Some("4242.pts-3.h"),
                Some("screen-256color"),
                Some("/run/user/1000"),
                None,
            ),
            Path::new("/run/user/1000/aterm/mux-notice/tmux-_tmp_tmux-1000_default_9_0"),
        );
        // $STY when there is no $TMUX; dots and dashes survive untouched, which
        // is what keeps one screen's panes on ONE stamp.
        assert_eq!(
            mux_notice_stamp(
                "screen",
                None,
                Some("4242.pts-3.host"),
                Some("screen.xterm-256color"),
                Some("/run/user/1000"),
                Some("/tmp"),
            ),
            Path::new("/run/user/1000/aterm/mux-notice/screen-4242.pts-3.host"),
        );
        // The root falls through XDG_RUNTIME_DIR -> TMPDIR -> /tmp, and TERM is
        // the id of last resort, exactly as the shell's `${TMUX:-${STY:-$TERM}}`.
        assert_eq!(
            mux_notice_stamp(
                "screen",
                Some(""),
                None,
                Some("screen"),
                Some(""),
                Some("/x")
            ),
            Path::new("/x/aterm/mux-notice/screen-screen"),
        );
        assert_eq!(
            mux_notice_stamp("screen", None, None, None, None, None),
            Path::new("/tmp/aterm/mux-notice/screen-"),
        );
    }

    /// The notice is the SAME SENTENCE the shell integration prints, because it
    /// is the same statement said by whichever half gets to run — and in a real
    /// bash or zsh pane the shell half never runs at all.
    #[test]
    fn the_mux_boundary_notice_says_what_stopped_existing() {
        let msg = mux_boundary_notice("screen");
        for needle in [
            "inside screen",
            "command blocks, exit codes and cwd tracking do not cross the multiplexer",
            "aterm ctl mux",
            "ATERM_MUX_NOTICE=0",
        ] {
            assert!(msg.contains(needle), "notice must say {needle:?}: {msg}");
        }
        assert!(
            mux_boundary_notice("tmux").contains("inside tmux"),
            "the notice names the multiplexer it found"
        );
    }

    /// The notice belongs to a boundary that COST something. A screen inside
    /// somebody else's terminal never had an aterm block model to lose, so
    /// neither half announces there — the shell integration draws the line at
    /// `$ATERM_PARENT_SESSION_ID` and this must draw it in the same place, or
    /// `aterm ctl` starts leaving stamps for multiplexers aterm has no part in.
    ///
    /// DRIVEN ON A REAL PATH. This test used to compute a stamp under a root
    /// (`/nonexistent-runtime-dir-for-this-test`) that the shipping claim never
    /// resolves — `claim_mux_notice` reads `$XDG_RUNTIME_DIR`/`$TMPDIR` — so
    /// `!before.exists()` was true no matter what the gate did, and deleting the
    /// gate outright would not have failed it. Here the root is a real scratch
    /// dir, the stamp is resolved by the SHIPPING resolver from that root, and
    /// the POSITIVE control runs on the very same path: the negative claim means
    /// something only because the positive one demonstrably writes.
    #[test]
    fn the_boundary_is_announced_only_where_it_cost_a_session_something() {
        let root = std::env::temp_dir().join(format!("aterm-ctl-muxnotice-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("private scratch root");
        // The path the SHELL and `aterm ctl` both compute, resolved by the
        // shipping resolver off this test's root.
        let stamp = mux_notice_stamp(
            "screen",
            None,
            Some("77.pts-1.h"),
            None,
            root.to_str(),
            None,
        );
        assert_eq!(
            stamp,
            root.join("aterm")
                .join("mux-notice")
                .join("screen-77.pts-1.h"),
            "fixture: the stamp under test is the shell's own path, resolved from this root"
        );
        // The claim the seam is handed: the SHIPPING mechanism (`create_new` on
        // the resolved stamp), aimed at this test's root instead of the
        // process-wide `$XDG_RUNTIME_DIR` — which a std-only test must not
        // mutate out from under its sibling tests.
        let claim = |_: &MuxNesting| claim_mux_notice_at(&stamp);

        // ANONYMOUS: no aterm session outside, so the gate returns before the
        // claim runs at all — nothing written, nothing spent.
        let anon = MuxNesting {
            kind: "screen",
            outer_sid: None,
            detected_by: "shell-integration",
        };
        announce_mux_boundary_with(&anon, false, claim);
        assert!(
            !stamp.exists(),
            "an anonymous multiplexer must leave no stamp behind"
        );

        // OURS: the same call on the same path, with an aterm session outside.
        // `already_said = true` claims without printing, so the assertion is
        // about the stamp rather than about this test's stderr.
        let ours = MuxNesting {
            kind: "screen",
            outer_sid: Some("s1".to_string()),
            detected_by: "shell-integration",
        };
        announce_mux_boundary_with(&ours, true, claim);
        assert!(
            stamp.exists(),
            "a boundary that cost an aterm session its block model claims the stamp"
        );

        // ONCE. `create_new` is the whole once-per-multiplexer mechanism: the
        // second pane to reach it is refused, which is what keeps six panes of
        // one screen down to one sentence.
        assert!(
            !claim_mux_notice_at(&stamp),
            "a claimed stamp must refuse the next claimant"
        );

        // …and the `None` nesting is a no-op, not a panic.
        announce_mux_boundary(None, false);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A `sessions` row, as a real server frames one.
    fn session_rows(rows: &[&str]) -> Vec<String> {
        rows.iter().map(|r| (*r).to_string()).collect()
    }

    /// The listing rows an in-process caller gets are the rows `ls` PRINTS: the
    /// server's line verbatim, the hosting pid, the columns `@<sid>` addresses
    /// by, and the ` *` self marker as a bit instead of a trailing token.
    #[test]
    fn fleet_sessions_carry_the_ls_row_verbatim_and_the_self_marker_as_data() {
        let rows = session_rows(&[
            "0 s-a - alive sh meta=0 window=1 active=1 wfocus=1 detail=claude",
            "1 s-b s-a alive sh meta=1 window=1 active=0 wfocus=1 detail=-",
        ]);
        let listed = instance_sessions(4242, &rows, Some("s-b"));

        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].pid, 4242);
        assert_eq!(listed[0].row, rows[0], "the server's row, byte for byte");
        assert_eq!(listed[0].local(), Some("0"));
        assert_eq!(listed[0].sid(), Some("s-a"));
        assert!(!listed[0].is_self);
        // Exactly the row `ls` would mark ` *`: its sid is the calling terminal's.
        assert!(listed[1].is_self, "the caller's own session is flagged");

        // No `$ATERM_PARENT_SESSION_ID` marks nothing — a row whose sid column is
        // absent must never match a missing self sid by both being `None`.
        let outside = instance_sessions(1, &session_rows(&["0"]), None);
        assert_eq!(outside[0].sid(), None, "a row too short names no session");
        assert!(!outside[0].is_self);
    }

    /// THE DISTINCTION THE BRIDGE RUNS ON: an in-process listing must report an
    /// empty fleet, an unreachable one and a wedged one as three different
    /// things — the same three `ls` exits on. `aterm-fleet federate` folded all
    /// of them (plus a missing binary) into `Vec::new()`, so a bridge that had
    /// gone blind and a fleet with nothing running looked identical.
    #[test]
    fn a_fleet_listing_failure_is_never_an_empty_listing() {
        let sock = fleet_sock(4242);
        let answered =
            |rows: &[&str]| vec![(4242, sock.clone(), Probe::Answered(session_rows(rows)))];

        // Reachable: the sessions, however many — Ok is the only "empty fleet".
        let rows = instance_sessions(4242, &session_rows(&["0 s-a - alive sh meta=0"]), None);
        let dir = DirOutcome::Found {
            path: PathBuf::from(FLEET),
            n: 1,
        };
        let ok = fleet_listing(
            rows,
            discovery_report(dir, &answered(&["0 s-a - alive sh meta=0"])),
        )
        .expect("an instance answered");
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].sid(), Some("s-a"));

        // Genuinely empty: the ONE condition that licenses the fleet claim.
        let empty = fleet_listing(
            Vec::new(),
            discovery_report(DirOutcome::Empty(PathBuf::from(FLEET)), &[]),
        )
        .expect_err("no instance answered");
        assert!(empty.is_empty_fleet(), "a readable, empty directory");
        assert_eq!(empty.code, EXIT_EMPTY_FLEET);
        assert!(empty.reason.contains(FLEET_CLAIM));

        // Found but unreachable (the seatbelt case): NOT empty, and it says so
        // with the cause `Command::output()` used to swallow.
        let denied = vec![(
            4242,
            sock.clone(),
            Probe::Denied(io::Error::from_raw_os_error(1)),
        )];
        let blind = fleet_listing(
            Vec::new(),
            discovery_report(
                DirOutcome::Found {
                    path: PathBuf::from(FLEET),
                    n: 1,
                },
                &denied,
            ),
        )
        .expect_err("nothing answered");
        assert!(!blind.is_empty_fleet(), "sockets were found, not absent");
        assert_eq!(blind.code, EXIT_UNREACHABLE);
        assert!(
            blind
                .reason
                .contains("sandbox is refusing AF_UNIX connect()")
        );
        assert!(!blind.reason.contains(FLEET_CLAIM));

        // Wedged: every probe timed out, which is its own status (124).
        let wedged = fleet_listing(
            Vec::new(),
            discovery_report(
                DirOutcome::Found {
                    path: PathBuf::from(FLEET),
                    n: 1,
                },
                &[(4242, sock, Probe::Timeout)],
            ),
        )
        .expect_err("nothing answered");
        assert_eq!(wedged.code, EXIT_TIMEOUT);
        assert!(!wedged.is_empty_fleet());
    }

    /// The `windows` fold over real `sessions` lines: windows in id order, the
    /// front bit from `wfocus=`, the active-tab sids joined, the detached count
    /// apart from the could-not-say count — and a pre-`window=` server folds
    /// into `window=-`, never into a window.
    #[test]
    fn windows_rows_fold_sessions_per_window_and_keep_unknown_apart_from_none() {
        let lines: Vec<String> = [
            "0 s-a - alive sh meta=0 window=1 active=1 wfocus=1 detail=claude",
            "1 s-b s-a alive sh meta=1 window=1 active=0 wfocus=1 detail=-",
            "2 s-c - alive sh meta=0 window=0 active=1 wfocus=0 detail=codex",
            "3 s-d - alive sh meta=0 window=none active=0 wfocus=0 detail=-",
            "4 s-e - alive sh meta=0 window=- active=- wfocus=- detail=-",
            "5 s-f - alive sh meta=0",
            "6 s-g - alive sh meta=0 window=1 active=1 wfocus=1 detail=-",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        assert_eq!(
            window_rows_from_sessions(42, &lines),
            vec![
                "42 window=0 focused=0 sessions=1 active=s-c".to_string(),
                "42 window=1 focused=1 sessions=3 active=s-a,s-g".to_string(),
                "42 window=none sessions=1".to_string(),
                "42 window=- sessions=2".to_string(),
            ]
        );
        // A window with nothing on its active tab says so.
        let lines = vec!["0 s-a - alive sh meta=0 window=2 active=0 wfocus=0 detail=-".to_string()];
        assert_eq!(
            window_rows_from_sessions(7, &lines),
            vec!["7 window=2 focused=0 sessions=1 active=-".to_string()]
        );
        // No sessions: no rows (the instance is still listed by `instances`).
        assert!(window_rows_from_sessions(7, &[]).is_empty());
    }

    /// The title is the fifth field and any program can set it; `pct_encode`
    /// leaves `=` alone, so a title of `window=none` / `window=7` / `active=1`
    /// / `wfocus=1` / `meta=1` is a legal title token. The fold must read the
    /// SERVER's tokens (after `meta=`), never the title's: a session on
    /// window 3 titled `window=none` stays on window 3, one titled `active=1`
    /// is not on the active tab, one titled `wfocus=1` does not front its
    /// window, and one titled `window=7` files under the window the server
    /// named. The relay side (`ls`) prints the line verbatim and is unaffected.
    #[test]
    fn windows_rows_never_read_the_title_as_a_token() {
        let lines: Vec<String> = [
            "0 s-a - alive window=none meta=0 window=3 active=0 wfocus=0 detail=-",
            "1 s-b - alive active=1 meta=0 window=3 active=0 wfocus=0 detail=-",
            "2 s-c - alive wfocus=1 meta=0 window=3 active=1 wfocus=0 detail=-",
            "3 s-d - alive window=7 meta=0 window=none active=0 wfocus=0 detail=-",
            "4 s-e - alive window=7 meta=1 window=5 active=1 wfocus=1 detail=-",
            "5 s-f - alive meta=1 meta=0 window=5 active=0 wfocus=1 detail=-",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        assert_eq!(
            window_rows_from_sessions(9, &lines),
            vec![
                "9 window=3 focused=0 sessions=3 active=s-c".to_string(),
                "9 window=5 focused=1 sessions=2 active=s-e".to_string(),
                "9 window=none sessions=1".to_string(),
            ]
        );
        // A pre-`window=` server whose title happens to be `window=1` (or
        // `wfocus=1`, or `detail=x`): still "could not say", never window 1.
        for title in ["window=1", "wfocus=1", "detail=x"] {
            let lines = vec![format!("0 s-a - alive {title} meta=0")];
            assert_eq!(
                window_rows_from_sessions(9, &lines),
                vec!["9 window=- sessions=1".to_string()],
                "{title}"
            );
        }
        // No `meta=` anchor at all (a pre-roster server): no tail is
        // recognised, whatever `=`-bearing words the title carries.
        let lines = vec!["0 s-a - alive window=1".to_string()];
        assert_eq!(
            window_rows_from_sessions(9, &lines),
            vec!["9 window=- sessions=1".to_string()]
        );
    }

    /// An EMPTY title is two spaces on the wire (`pct_encode("")` is empty), and
    /// a whitespace split collapses it: the tokens are read from the end, so
    /// the keys still land — and so does a column a later server appends after
    /// `detail=`.
    #[test]
    fn windows_rows_tolerate_an_empty_title_and_trailing_new_columns() {
        let lines: Vec<String> = [
            "0 s-a - alive  meta=0 window=2 active=1 wfocus=1 detail=-",
            "1 s-b - alive  meta=0",
            "2 s-c - alive sh meta=0 window=2 active=0 wfocus=1 detail=- extra=1 more=x",
            "3 s-d - alive  meta=0 window=none active=0 wfocus=0 detail=- extra=1",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        assert_eq!(
            window_rows_from_sessions(9, &lines),
            vec![
                "9 window=2 focused=1 sessions=2 active=s-a".to_string(),
                "9 window=none sessions=1".to_string(),
                "9 window=- sessions=1".to_string(),
            ]
        );
    }

    /// `roster_tail` is the anchor rule on its own: the `key=value` run behind
    /// `meta=` in end-first order, nothing when the anchor is missing, and the
    /// walk stops at the first `=`-less word so it can never reach the
    /// positional columns.
    #[test]
    fn roster_tail_reads_back_to_the_meta_anchor_or_nothing() {
        let f = |s: &str| s.split_whitespace().map(str::to_string).collect::<Vec<_>>();
        let full = f("0 s-a - alive window=7 meta=0 window=1 active=1 wfocus=0 detail=-");
        let fields: Vec<&str> = full.iter().map(String::as_str).collect();
        assert_eq!(
            roster_tail(&fields),
            vec!["detail=-", "wfocus=0", "active=1", "window=1"]
        );
        let bare = f("0 s-a - alive x=1 meta=0");
        let fields: Vec<&str> = bare.iter().map(String::as_str).collect();
        assert!(roster_tail(&fields).is_empty(), "nothing after the anchor");
        let old = f("0 s-a - alive window=1 active=1");
        let fields: Vec<&str> = old.iter().map(String::as_str).collect();
        assert!(roster_tail(&fields).is_empty(), "no anchor, no tail");
        assert!(roster_tail(&[]).is_empty());
    }

    /// `windows` takes no argument: `windows 1` is `ERR usage: windows` (exit
    /// FAILURE), not a fleet listing the caller did not ask for; a bare
    /// `windows` is fine, and a request that is no discovery verb at all is
    /// not this check's to judge.
    #[test]
    fn windows_rejects_trailing_arguments_with_the_usage_shape() {
        let parts = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        assert_eq!(
            discovery_given_arguments(&parts(&["windows", "1"])),
            Some("windows")
        );
        assert_eq!(
            discovery_given_arguments(&parts(&["windows", "--all"])),
            Some("windows")
        );
        assert_eq!(discovery_given_arguments(&parts(&["windows"])), None);
        assert_eq!(discovery_given_arguments(&parts(&["sessions", "1"])), None);
        assert_eq!(discovery_given_arguments(&parts(&[])), None);
        let e = discovery_usage_error("windows");
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(e.to_string(), "ERR usage: windows");
        assert!(
            !is_timeout_error(&e),
            "a usage error exits FAILURE, not 124"
        );
    }

    /// `ls` never took an argument and used to ignore one silently — `ls 1`
    /// listed the whole fleet as if the `1` had been read. It is `ERR usage: ls`
    /// (exit FAILURE) now, the `windows` shape exactly; a bare `ls` is fine.
    #[test]
    fn ls_rejects_trailing_arguments_with_the_usage_shape() {
        let parts = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        assert_eq!(discovery_given_arguments(&parts(&["ls", "1"])), Some("ls"));
        assert_eq!(
            discovery_given_arguments(&parts(&["ls", "--json"])),
            Some("ls")
        );
        assert_eq!(discovery_given_arguments(&parts(&["ls"])), None);
        let e = discovery_usage_error("ls");
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(e.to_string(), "ERR usage: ls");
        assert!(
            !is_timeout_error(&e),
            "a usage error exits FAILURE, not 124"
        );
    }

    /// The same for `instances`: `instances 1` is `ERR usage: instances`, not
    /// the fleet; a bare `instances` is fine.
    #[test]
    fn instances_rejects_trailing_arguments_with_the_usage_shape() {
        let parts = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        assert_eq!(
            discovery_given_arguments(&parts(&["instances", "1"])),
            Some("instances")
        );
        assert_eq!(
            discovery_given_arguments(&parts(&["instances", "self"])),
            Some("instances")
        );
        assert_eq!(discovery_given_arguments(&parts(&["instances"])), None);
        let e = discovery_usage_error("instances");
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(e.to_string(), "ERR usage: instances");
        assert!(
            !is_timeout_error(&e),
            "a usage error exits FAILURE, not 124"
        );
    }
}
