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
//! for a per-instance socket (resolved through the `latest` symlink), else
//! `aterm.token` — and sends `AUTH <hex>\n` as the FIRST line of every
//! connection, before the verb. Normal same-user usage is therefore unchanged
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
//! * `search <pat>`    — print one `"<row> <col> <len>"` line per match.
//! * `send <text>`     — write `<text>` to the PTY (trailing literal `\n` ⇒ CR).
//! * `key <name>`      — send a named key (`enter`, `tab`, `up`, …) to the PTY.
//! * `image [path]`    — render the screen to a PNG; print `OK <w> <h> <path>`.
//!   WYSIWYG: includes cursor blink phase / unfocused-hollow override (headless
//!   sessions are always deterministic); use `cursor` for phase-independent state.
//! * `window [<target>] [path]` — capture an ENTIRE macOS window to a PNG; print
//!   `OK <w> <h> <path>`. `<target>` selects which window: omitted/`front` = the
//!   front terminal window (native OS chrome — titlebar, traffic lights, unified
//!   toolbar, full-width tab strip — AND the terminal content); `prefs`/`settings`
//!   = the Settings overlay (composited into the front window, so this is the
//!   front capture with the overlay up); `perf`/`performance` = the Performance
//!   control panel. Unlike `image` (terminal content framebuffer only), this photographs
//!   the real composited on-screen pixels via CoreGraphics, so an AI can SEE the
//!   whole window or any GUI screen. A first token that is a known target keyword
//!   always selects that window — to write to a file literally named
//!   `prefs`/`perf`/`front`, give a target first (e.g. `window front prefs`). macOS
//!   only; needs Screen Recording permission (a clear `ERR` explains how to grant it
//!   if missing); a not-open aux window or headless / off-macOS gets a clear `ERR`.
//! * `controls <target>` — dump an auxiliary GUI window's controls as text:
//!   `prefs`/`settings` lists each setting (`field key=… label=… value=…
//!   effective=…`), `perf`/`performance` lists the HUD toggles (`toggle key=…
//!   label=… enabled=…`). The analogue of `chrome` for the settings/perf GUIs —
//!   works HEADLESS (built from the live config/panel model, no screenshot or
//!   Screen Recording grant needed). `"OK <n>\n"` + `<n>` lines.
//! * `open <target>`   — bring an auxiliary GUI screen UP: `prefs`/`settings` opens
//!   the Settings overlay on the front window, `perf`/`performance` the Performance
//!   control panel. The piece that lets a driver introspect a CLOSED screen: `open
//!   prefs` then `window prefs` / `controls prefs`. Overlay targets (prefs / about /
//!   menu) work HEADLESS too (they composite into the virtual frame `image` reads);
//!   `perf` is a real `NSWindow` — windowed macOS only (headless gets `ERR`).
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
//! Two verbs are answered by THIS CLIENT (no single server owns the answer):
//!
//! * `instances`       — one line per LIVE same-user aterm instance:
//!   `<pid> <sessions> <sock_path>[ self]` (`self` marks the instance hosting
//!   the calling terminal). Discovers `<dir>/aterm-<pid>.sock` files and probes
//!   each with its own token.
//! * `ls`              — every session of every live instance, one line each:
//!   `<pid> <local> <sid> <parent|-> <state> <title>[ *]` (the server's
//!   `sessions` line prefixed with the instance pid; `*` marks the calling
//!   terminal's own session, from `$ATERM_PARENT_SESSION_ID`; the title is
//!   pct-encoded and often mirrors the cwd via shell integration). The
//!   one-shot peer-discovery view an agent uses to pick which terminal to
//!   drive; combine with `aterm-ctl "@<sid>" <verb>` to drive a session in
//!   ANY instance — the server relays an unknown-but-published sid to its
//!   hosting sibling.
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
    0    OK — the server answered OK.
    1    failure — a usage/connect error, or the server replied ERR.
    124  timeout — the client's --timeout deadline fired, OR the server
         reported a timeout (a `turn` verdict status=timeout, or an
         await/ready/wait `OK timeout`). Additive: still nonzero, so existing
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

/// The two CLIENT-answered discovery verbs, documented in a FIXED block appended
/// to `--help` AFTER the generated protocol catalog. `ls` and `instances` are
/// intercepted by aterm-ctl itself (no server round-trip), so they are NOT in the
/// protocol verb table [`aterm_types::control_verbs::catalog_lines`] renders and
/// would otherwise be invisible to an AI reading `--help`. Hand-written (they are
/// hand-implemented in this same file, in `run_discovery`, so this cannot drift).
const CLIENT_VERBS: &str = "\
\n\
CLIENT VERBS (answered by aterm-ctl itself, no server round-trip):
    ls            every session of every live instance, one per line:
                  <pid> <local> <sid> <parent|-> <state> <title>[ *]
                  (* = the calling terminal's own session).
    instances     one line per live same-user instance:
                  <pid> <session-count> <sock>[ self]
                  (self = the instance hosting the calling terminal).
";

/// The verbs a completion script offers as the FIRST argument: every protocol
/// verb from the shared table (`aterm_types::control_verbs::VERBS`) PLUS the two
/// CLIENT-answered discovery verbs (`ls`/`instances`), which aterm-ctl intercepts
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
    // The client-only discovery verbs, answered by `run_discovery` (never framed
    // on the socket), so the protocol table above does not carry them.
    s.push_str("ls instances");
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
    s.push_str("local state\n");
    s.push_str("_arguments -C \\\n");
    s.push_str("    '--sock[control socket path]:path:_files' \\\n");
    s.push_str("    '--pid[instance pid]:pid' \\\n");
    s.push_str("    '--timeout[per-op deadline in seconds]:seconds' \\\n");
    s.push_str("    '(- *)--help[print help and exit]' \\\n");
    s.push_str("    '(- *)--version[print version and exit]' \\\n");
    s.push_str("    '1: :->verb' \\\n");
    s.push_str("    '*:: :->args'\n");
    s.push_str("case $state in\n");
    s.push_str("    verb) compadd -a verbs ;;\n");
    s.push_str("esac\n");
    s
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
    s.push_str("complete -c aterm-ctl -l sock -r -d 'control socket path'\n");
    s.push_str("complete -c aterm-ctl -l pid -r -d 'instance pid'\n");
    s.push_str("complete -c aterm-ctl -l timeout -r -d 'per-op deadline in seconds'\n");
    s.push_str("complete -c aterm-ctl -l help -d 'print help and exit'\n");
    s.push_str("complete -c aterm-ctl -l version -d 'print version and exit'\n");
    s
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

/// One framed request→response against the socket at `path` WITHOUT printing:
/// authenticate, send `verb`, read the `OK <n>` header + `n` body lines, and
/// return them. Errors (connect refused, `ERR …`, malformed header, a read or
/// write past the deadline) map to `None` so discovery can skip a dead, wedged,
/// or hostile instance and keep going — discovery dials EVERY socket in the
/// shared dir sequentially, so one stuck peer must never hang the whole sweep.
/// Lines are read through [`read_bounded_line`]: the deadline alone cannot
/// stop a peer that STREAMS newline-less bytes (each read succeeds, so the
/// clock keeps resetting), so the accumulation cap is what bounds memory.
fn query_lines(path: &str, verb: &str) -> Option<Vec<String>> {
    let stream = CtlStream::connect(path).ok()?;
    let deadline = Some(std::time::Duration::from_secs(2));
    stream.set_read_timeout(deadline).ok()?;
    stream.set_write_timeout(deadline).ok()?;
    if let Some(token) = read_token_for(path) {
        (&stream)
            .write_all(format!("AUTH {token}\n").as_bytes())
            .ok()?;
    }
    (&stream).write_all(format!("{verb}\n").as_bytes()).ok()?;
    (&stream).flush().ok()?;
    let mut reader = BufReader::new(&stream);
    let mut status = String::new();
    read_bounded_line(&mut reader, &mut status).ok()?;
    let count: usize = stream_count(status.trim_end().strip_prefix("OK ")?)?;
    let mut lines = Vec::with_capacity(count);
    for _ in 0..count {
        let mut line = String::new();
        if read_bounded_line(&mut reader, &mut line).ok()? == 0 {
            break;
        }
        lines.push(line.trim_end_matches(['\r', '\n']).to_string());
    }
    Some(lines)
}

/// Every live same-user aterm instance: `(pid, sock_path)`, discovered two ways
/// and DEDUPED by socket path so no instance is listed twice. Sorted by pid.
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
///    out-of-dir path), so `query_lines` dials the real socket and `read_token_for`
///    finds that socket's own token beside it.
fn live_instances() -> Vec<(u32, String)> {
    let Some(dir) = socket_dir() else {
        return Vec::new();
    };
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
    if let Ok(entries) = std::fs::read_dir(&dir) {
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

fn run_discovery(verb: &str) -> io::Result<ExitCode> {
    let self_sid = env::var(SELF_SID_ENV).ok();
    let self_sock = self_instance_sock(self_sid.as_deref());
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut any = false;
    for (pid, sock) in live_instances() {
        let Some(sessions) = query_lines(&sock, "sessions") else {
            continue; // unreachable/dead instance — skip, keep discovering
        };
        any = true;
        let is_self_instance = self_sock
            .as_deref()
            .is_some_and(|self_s| same_socket_path(self_s, &sock));
        if verb == "instances" {
            let marker = if is_self_instance { " self" } else { "" };
            writeln!(out, "{pid} {} {sock}{marker}", sessions.len())?;
        } else {
            for line in sessions {
                // The calling terminal's own session: its sid (2nd field of the
                // `sessions` line) equals $ATERM_PARENT_SESSION_ID.
                let sid = line.split_whitespace().nth(1);
                let marker = if sid.is_some() && sid == self_sid.as_deref() {
                    " *"
                } else {
                    ""
                };
                writeln!(out, "{pid} {line}{marker}")?;
            }
        }
    }
    if !any {
        eprintln!("aterm-ctl: no live aterm instances found");
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::SUCCESS)
}

/// Read the per-launch capability token sitting beside the socket at `path`.
/// A per-instance socket (reached directly or through the `latest` symlink)
/// pairs with its `aterm-<pid>.token`; anything else falls back to the
/// sibling `aterm.token`. Returns `None` if unreadable (e.g. a different
/// user, or aterm not running); the connection is then attempted without an
/// `AUTH` line and the server refuses it with `ERR auth`.
fn read_token_for(path: &str) -> Option<String> {
    let p = Path::new(path);
    let dir = p.parent()?;
    // `latest::target_name` is `read_link` + final component on Unix, and the
    // pointer file's validated contents on Windows — the same relative name.
    let sock_name = aterm_uds::latest::target_name(p).unwrap_or(p.file_name()?.to_os_string());
    let token_name = control_socket::token_name_for_sock(&sock_name.to_string_lossy());
    let raw = std::fs::read_to_string(dir.join(token_name)).ok()?;
    let t = raw.trim().to_string();
    if t.is_empty() { None } else { Some(t) }
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
    matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)
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
            return Ok(ExitCode::SUCCESS);
        } else if arg == "-V" || arg == "--version" {
            // Compile-time concatenation: no runtime formatting needed.
            print_stdout_line(concat!("aterm-ctl ", env!("CARGO_PKG_VERSION")))?;
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

    // Expand a leading `@self` / `@env` selector to `@$ATERM_PARENT_SESSION_ID`
    // BEFORE anything reads the selector (framing, discovery, stdin routing), so
    // the rest of the path sees only a concrete `@<sid>`.
    expand_self_selector(&mut request_parts, env::var(SELF_SID_ENV).ok())?;

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
    if request_parts.first().map(String::as_str) == Some("instances")
        || request_parts.first().map(String::as_str) == Some("ls")
    {
        return run_discovery(&request_parts[0]);
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

    exchange(
        &path,
        &request,
        &verb,
        &frame_request,
        deadline,
        timeout_explicit,
    )
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
/// [`query_lines`]' 2 s discovery deadline: the blocking verbs (`await`,
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
/// [`io::ErrorKind`]. Message bytes are identical to the previous
/// `format!("connect {path}: {e}")`.
fn connect_error(path: &str, e: &io::Error) -> io::Error {
    let mut msg = String::from("connect ");
    msg.push_str(path);
    msg.push_str(": ");
    msg.push_str(&e.to_string());
    io::Error::new(e.kind(), msg)
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

/// Stream up to `count` payload lines from `reader` to stdout, normalizing
/// line endings while preserving each row's content verbatim.
fn print_payload(reader: &mut BufReader<&CtlStream>, count: usize) -> io::Result<()> {
    let stdout = stdout_handle();
    let mut out = stdout.lock();
    for _ in 0..count {
        let mut line = String::new();
        if read_bounded_line(reader, &mut line)? == 0 {
            break; // server hung up early; print what we have.
        }
        // Normalize the line ending; preserve the row's content verbatim.
        let line = line.strip_suffix('\n').unwrap_or(&line);
        let line = line.strip_suffix('\r').unwrap_or(line);
        out.write_all(line.as_bytes())?;
        out.write_all(b"\n")?;
    }
    Ok(())
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
/// We read the token from the socket's sibling `aterm.token` and send it
/// transparently; only then do we send the actual request line. There is no
/// server response to the `AUTH` line itself (it is consumed silently on
/// success), so the first line we read back is the response to `request`.
///
/// The whole conversation runs under `deadline` (default [`EXCHANGE_DEADLINE`],
/// or whatever `--timeout SECS` set — `None` disables it) and every reply line
/// under the [`MAX_LINE_BYTES`] cap, so a wedged or regressed server surfaces as
/// a clear error instead of an indefinite stall or an unboundedly growing String
/// — the same defenses [`query_lines`] applies to discovery.
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

    // Bound every socket operation, as `query_lines` already does for
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
        return Ok(ExitCode::SUCCESS);
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
        assert_eq!(forwarded_verb(&p("dial myhost text")).as_deref(), Some("text"));
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
        assert!(ok, "dial myhost text should complete the exchange (OK reply)");
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
    /// it to the `format!("connect {path}: {e}")` it replaced, and check the
    /// original error kind is preserved.
    #[test]
    fn connect_error_matches_format_and_keeps_kind() {
        let cause = io::Error::new(io::ErrorKind::ConnectionRefused, "Connection refused");
        let path = "/tmp/aterm-test/aterm.sock";
        let e = connect_error(path, &cause);
        assert_eq!(e.to_string(), format!("connect {path}: {cause}"));
        assert_eq!(e.kind(), io::ErrorKind::ConnectionRefused);
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
            aterm_types::control_verbs::catalog_lines()
                .all(|line| help.contains(&line)),
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
        let line = format!("aterm-ctl {}", env!("CARGO_PKG_VERSION"));
        assert!(line.starts_with("aterm-ctl "));
        assert!(!env!("CARGO_PKG_VERSION").is_empty());
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
            assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{bad:?} should error");
        }
    }

    /// The 124 exit code is reserved for a TIMEOUT and is additive: still
    /// nonzero, so a script's `nonzero == failure` check is unaffected, but
    /// distinguishable from the generic-failure 1.
    #[test]
    fn timeout_exit_code_is_124_and_client_kinds_map_to_it() {
        assert_eq!(EXIT_TIMEOUT, 124);
        assert!(is_timeout_error(&io::Error::from(io::ErrorKind::WouldBlock)));
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
        assert!(reply_is_timeout("OK 24 turn submitted=1 status=timeout seq=9"));
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
        assert_eq!(p[1], "@s-x,@1,@s-x", "each self element in the list expands");
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

    /// The two CLIENT-answered discovery verbs (`ls`, `instances`) — intercepted
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
            assert!(script.contains(wiring), "{shell} script has its shell wiring");
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
}
