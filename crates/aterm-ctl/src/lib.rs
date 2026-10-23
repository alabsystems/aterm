// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm ctl` CLI client, control-socket discovery, and verb execution.
#![cfg_attr(
    test,
    allow(
        clippy::disallowed_types,
        reason = "test code: a stand-in child may inherit descriptors"
    )
)]

use std::collections::HashSet;
use std::env;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use aterm_types::control_socket;
use aterm_types::control_verbs::{MAX_REQUEST_LINE, MAX_TASK_BYTES};
use aterm_uds::CtlStream;

/// The `aterm-askpass` role.
pub mod askpass;
pub use askpass::main_entry as askpass_main_entry;
pub mod census;
mod conn;
pub use conn::conn_main_entry;
pub use conn::resource_run_main_entry;
mod await_legs;
mod await_task;
mod drift;
/// The `amacs` client's `edit` and `await edit` exchanges with the window hosting its tab, the wait re-armed past every bound and every update.
pub mod edit;
mod graph_hygiene;
pub use drift::{CastDriftFn, CastDriftJob, DriftCut, DriftPeer, LocalVerbs};
mod redial;
pub use redial::{EXIT_REPLACED, target_sid};
mod remote;
pub use remote::{DriveLink, DriveRefused, DriveStream, FleetDriveFn};
mod survey;
pub use survey::{
    DEFAULT_BUDGET, EXCERPT_CHARS, EXCERPT_WIDE_CHARS, EXCERPTS_CUT, FLEET_WITHIN, FleetAsk,
    FleetRead, FleetSurveyFn, LEFT_OUT_LEAD, NO_ROUTE_NOTE, PeerFault, PeerSurvey,
    UNANSWERED_REASON_CHARS, agent_word_shown, survey_rank, survey_urgent,
};

const SYNOPSIS: &str =
    "aterm-ctl [--sock PATH | --pid PID | --host H] [--timeout SECS] <verb> [args...]";

const HELP_PROSE: &str = "\
aterm-ctl — the client for aterm's control socket (protocol v1).

USAGE:
    aterm-ctl [--sock PATH | --pid PID | --host H] [--timeout SECS] <verb> [args...]

OPTIONS:
    --sock PATH   connect to the control socket at PATH.
    --pid PID     connect to instance PID's socket, <dir>/aterm-<PID>.sock.
    --host H      drive another Mac's aterm over the fleet (its host, node id
                  or address, as `aterm fleet peers` names it), when its person
                  lets this Mac (`aterm fleet allow drive --from <this Mac>`
                  there): a worker's verbs alone, each session by its sid —
                  spawn agent=claude [cwd=<absolute>] - < task, await task
                  @<w> since=0 (the first wait; later ones @<w>:<rev>),
                  @<w> result|status|text|screen|turn|key|send|close,
                  hold <w> on|off. `aterm help workers`.
    --timeout SECS
                  deadline on each socket read and write, in seconds (default
                  900, 0 = none; --timeout=SECS also works). Raise it for a
                  verb that runs longer; a fired deadline exits 124. On
                  `subscribe`, an explicit --timeout instead caps the whole
                  watch (frames so far are flushed, then exit 124); without
                  one, or with 0, the watch runs until the stream ends. The
                  wait for a successor (exit 75) spends what is left of it.
    -h, --help    print this help and exit.
    -V, --version print version information and exit.

EXIT CODES:
    0    the server answered OK; for `ls`/`instances`/`windows`, at least one
         instance answered. Also 0 when stdout's reader closes it (`| head`).
    1    a usage or connect error, or an ERR reply. For `ls`/`instances`/
         `windows`: the socket directory was read and holds no instance — the
         only case that prints `no live aterm instances found`.
    2    could not reach: the instance accepted the connection but did not
         serve it within 10 s (or a shorter --timeout), so the request was not
         sent. For `ls`/`instances`/`windows`: no socket found answered, or the
         directory is missing, unreadable or unresolvable ($HOME and
         $XDG_RUNTIME_DIR unset); stderr names each socket's cause and a hint.
    124  timeout: the client's deadline fired, or the server reported one (a
         `turn` with status=timeout, or `OK timeout` from await/ready/wait);
         for `ls`/`instances`/`windows`, every socket found timed out.
    75   the instance went away mid-exchange (a failed update, a crash, a
         quit) and nothing replaced it within 30 s, or within what an explicit
         --timeout had left. A self-update is followed (a --pid call follows
         only its own process): `subscribe` resumes on the successor, stderr
         says so, and each target restarts from its current state after a
         `GAP <local> replaced` line (a single-target stream's block and turn
         events resume after the last ones relayed); a blocking read (`text`,
         `wait`, `ready`, `await`, `inbox`) is asked once more. Still 75, with
         a note on what to do: a read anchored in the old process (`await
         seq|block|consent`, `await inbox since=`, `inbox get <id>`, `inbox
         since=`, an `@<n>` selector); a `turn`, whose text may already be
         typed; a `post`, which may already be queued (re-post only with the
         same `key=`, or after finding its `post`/`post-landed` rows in
         `timeline`); a `dial` whose remote closes first. Never 0: an update
         is not \"session over\".

OUTPUT:
    To a pipe or a file, stdout and stderr carry the server's bytes exactly.
    To a terminal, every character a terminal acts on or reorders text around
    (C0 but newline and tab, DEL, C1, the bidi controls, U+2028/U+2029) and
    every byte that is not UTF-8 prints as an escape (\\x1b, \\r, \\u{202e},
    \\xff), so text another party wrote (`inbox get`, a `dial`ed remote's
    reply, a screen) cannot retitle the tab, move the cursor or write the
    clipboard. Redirect (`> file`, `| cat`) for the bytes themselves.

STDIN PAYLOADS:
    Inline arguments are joined with single spaces and may not contain a
    newline. These forms read STDIN instead, every byte kept (<= 256 KiB):
    feed-bin      feed the STDIN bytes to the PTY verbatim (a length-prefixed
                  frame; no inline argument); prints the server's OK <n> bytes.
    send --stdin | send -
                  the same raw feed as feed-bin.
    paste --stdin | paste -
                  paste the STDIN bytes with the server's paste handling
                  (control bytes sanitized; ESC[200~/201~ bracketing when the
                  app enabled DECSET 2004), not as a raw feed.
    operator-propose-bin
                  one JSON proposal for the embedded operator (<= 64 KiB; no
                  selector or argument).

SOCKET RESOLUTION:
    --sock PATH, else --pid PID (<dir>/aterm-<PID>.sock), else, from a shell
    inside an aterm session, the instance hosting it (found through
    $ATERM_PARENT_SESSION_ID's <dir>/graph/<sid> entry, even when a newer
    instance owns the alias), else <dir>/aterm.sock, the link to the newest
    instance's socket — or, when nothing accepts there, the newest
    <dir>/aterm-<pid>.sock that does. <dir> is $XDG_RUNTIME_DIR/aterm when
    that is set, else ~/Library/Application Support/aterm (on every Unix). No
    environment variable selects a socket. When the socket is dead but aterm
    is running, a flagless call's error names each live instance with the
    --pid or --sock that reaches it; a --sock or --pid call is told only how
    many are live, never handed another instance to drive.

    DISCOVERY (`ls`, `instances`, `windows`) dials every <dir>/aterm-<pid>.sock
    and every --control-sock instance listed under <dir>/graph, 2 s each. With
    $XDG_RUNTIME_DIR set, ~/Library/Application Support/aterm is not
    consulted. A private --control-sock instance whose token this user cannot
    read is not dialled (`instances` names it on stderr). A stale socket is
    skipped silently while another instance answers; when none does, the
    report names each socket and its cause (EXIT CODES 1 and 2).

CROSS-TERMINAL DRIVING:
    Prefix a verb with @<sid> (`subscribe` takes it after the verb) to address
    that session on any same-user instance, e.g. aterm-ctl \"@s-ab12…\" text; a
    session on another instance is relayed to it (owner connections only).
    `ls` lists the sids. @self (alias @env) expands here to
    @$ATERM_PARENT_SESSION_ID — your own session, whichever tab is in front —
    and fails outside an aterm session. No selector, or @., targets the
    instance's active tab, which changes with every tab switch.

PUSH FRAMES (subscribe):
    After the `OK subscribe <n>` ack (on stderr) the connection is push-only
    and stdout carries these frames (as OUTPUT above), each tagged with a
    <local> channel id, not the sid. A BYTES body may contain newlines: once
    `bytes` is requested, do not parse the stream line by line.
      sub <local> <sid>          one per target, right after the ack.
      DELTA <local> seq=<n> screen <nrows>   then <nrows> rows: a screen
                                 snapshot at seq <n> (a skipped seq is
                                 coalescing, not loss).
      DELTA <local> seq=<n> cursor <row> <col> <visible> <style>   as the
                                 `cursor` verb; a DECTCEM hide/show pushes
                                 even with no caret move.
      DELTA <local> seq=<n> cells <nbytes>   then <nbytes> bytes of styled-cell
                                 JSON and a newline: a lossless state delta.
      EVENT <local> <kind> ...   a lifecycle line, <kind> one of turn,
                                 block-complete, meta, title, bell, agent,
                                 status, human, harness-policy, resize,
                                 render, the fabric kinds (inbox, inbox-seen,
                                 post, fetch, post-landed, hold, topic),
                                 closing, then exited. The `sessions` stream
                                 adds `EVENT * session-created`,
                                 `session-exited` and `fabric-retire`.
                                 `subscribe --help` gives each one's fields.
      BYTES <local> <len>        then <len> raw PTY bytes and a newline.
      MAIL <local> id=<n> off=<n> from=<p> kind=<k>[ re=<n>][ topic=<t>]
                                 one per row delivered to the session's inbox
                                 (`mail` stream); metadata only, `inbox get
                                 <id>` reads the body.
      T <local|*> <t_us>         a timestamp before the frame it stamps (the
                                 `timestamps`/`ts` modifier); at most one per
                                 channel per wake.
      GAP <local> ...            a discontinuity: `bytes-dropped=<n>` (queue
                                 overflow); `resync=<seq>` (engine reset:
                                 treat the next DELTA as a fresh snapshot, or
                                 re-read `screen`); `events-resync=<floor>` (a
                                 `since-turn=` anchor older than the turn
                                 ledger; records below <floor> are gone);
                                 `events-dropped=<n>` or `mail-dropped=<n>`
                                 (the timeline or the inbox ring evicted <n>
                                 this watch had not been shown); `replaced`
                                 (written by this client when the stream
                                 resumed on a successor: output, mail and
                                 records between the two were not delivered;
                                 re-read `screen`).

VERBS (the protocol verb table; `<verb> --help` prints one verb's entry):
";

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

const CLIENT_VERBS: &str = "\
\n\
CLIENT VERBS (answered by aterm-ctl itself; not in the protocol table):
    ls            every session of every live instance, one per line: the
                  instance's pid, its `sessions` row as sent, and ` *` on the
                  calling terminal's own session:
                  <pid> <local> <sid> <parent|-> <state> <title> meta=<0|1> nonce=<hex32>
                  window=<id|none|-> active=<0|1|-> wfocus=<0|1|-> detail=<pct|->
                  identity=<name|-> path=<frozen|live> program=<name|-> agent=<word|->
                  agent_detail=<pct|-> agent_rev=<n> agent_since_ms=<ms> agent_gen=<e.s|->
                  agent_fp=<hex16|-> human_ms=<ms|-> role=<pct|-> attention=<pct|->
                  user_title=<pct|-> supervisor=<holder|->
                  autopilot=<off|cruise|land|->
                  path_evidence=<measured:<ms>|unconfirmed:<ms>|carried|->
                  copy=<managed|foreign|-> upgrade=<-|<state>/<to>/<why>/<age>>
                  land_at=<unix-s|-> task=<open|landed|theirs|->
                  pause=<unix-s|-> task_state=<working|done|exited|stalled|->
                  task_rev=<n>[ *]
                  `help sessions` explains each field. detail=, program= and
                  agent= say whether a peer is another agent: a tab whose
                  foreground is an agent reads what you type as its prompt,
                  not as a shell line.
    instances     one line per live same-user instance:
                  <pid> <session-count> <sock>[ self]
                  (self = the instance hosting the calling terminal).
    windows       one line per window across the fleet, folded from `sessions`:
                  <pid> window=<id> focused=<0|1> sessions=<n> active=<sid>[,<sid>…]
                  focused= marks aterm's most recently focused window (one per
                  instance; a minimize or an app switch does not clear it);
                  active= lists the sids on its active tab, - for none; a
                  headless instance has one window, 0. Then
                  <pid> window=none sessions=<n> for sessions no window holds,
                  and <pid> window=- sessions=<n> for sessions whose window the
                  instance could not say (an older server, or its main thread
                  did not answer).

    None of the three takes an argument: `ls 1` is `ERR usage: ls` (exit 1).
    --sock or --pid scopes the listing to that one instance, which keeps an
    automated caller's private instance apart from the user's terminals. A
    scoped instance that does not answer exits 2 (124 if it timed out); a
    --pid that no live instance has exits 1. `help <verb>` answers these and
    `mux` from this block: the server's table does not hold them.

    mux           whether a multiplexer (screen/tmux) sits between this shell
                  and its aterm session:
                  mux=<kind|none> detected_by= outer_sid=<sid|-> marks=absent
                  self_target=<refused|allowed>
                  detected_by= is shell-integration (the integration marked
                  the pane), session-base ($ATERM_MUX_BASE, the multiplexer
                  environment the session shell recorded, differs from this
                  one) or environment ($TMUX/$STY corroborated by $TERM or
                  TERM_PROGRAM=tmux). aterm hosts one session for the whole
                  multiplexer, so a flagless or @self call from a pane would
                  drive the terminal running screen/tmux: such calls are
                  refused, naming the sid and the explicit form to use. Verbs
                  that address no session (version, sessions, who, whoami,
                  grant, flows, dial-*, help) still run. OSC 133 marks do not
                  cross the boundary, so blocks, exit codes and cwd are absent
                  (not empty) from inside; the first call from a multiplexer
                  says so once (ATERM_MUX_NOTICE=0 silences that and the
                  blocks/status note; ATERM_MUX=0 turns the check off).
";

fn completion_verb_list() -> String {
    let mut s = String::new();
    for spec in aterm_types::control_verbs::VERBS {
        s.push_str(spec.name);
        s.push(' ');
    }
    s.push_str("ls instances windows mux");
    s
}

const COMPLETION_FLAGS: &str = "--sock --pid --timeout --help --version";

fn completion_script(shell: &str) -> Option<String> {
    let verbs = completion_verb_list();
    match shell {
        "bash" => Some(bash_completion(&verbs)),
        "zsh" => Some(zsh_completion(&verbs)),
        "fish" => Some(fish_completion(&verbs)),
        _ => None,
    }
}

fn emit_completions(shell: &str) -> io::Result<ExitCode> {
    let script = completion_script(shell).ok_or_else(unknown_shell_error)?;
    let stdout = stdout_handle();
    let mut out = StdoutSink::new(stdout.lock());
    out.write_all(script.as_bytes())?;
    Ok(ExitCode::SUCCESS)
}

fn unknown_shell_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "ERR --completions: unknown shell (expected bash, zsh, or fish)",
    )
}

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

/// The front door's completion script for `shell` (`bash`/`zsh`/`fish`), or `None` for an unknown shell.
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

/// The front door's `--completions <shell>`.
pub fn front_door_completions_entry(
    shell: Option<&str>,
    verbs: &[&str],
    flags: &[(&str, &str)],
) -> ExitCode {
    match front_door_completions_result(shell, verbs, flags) {
        Ok(code) => code,
        Err(e) if is_stdout_closed(&e) => ExitCode::SUCCESS,
        Err(e) => {
            let stderr = io::stderr();
            let mut err = stderr.lock();
            let _ = err.write_all(b"aterm: ");
            let _ = err.write_all(e.to_string().as_bytes());
            let _ = err.write_all(b"\n");
            ExitCode::FAILURE
        }
    }
}

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
    let mut out = StdoutSink::new(stdout.lock());
    out.write_all(script.as_bytes())?;
    Ok(ExitCode::SUCCESS)
}

/// The control socket of the instance a front-door request should go to, or `None` when nothing is reachable.
#[must_use]
pub fn front_door_instance() -> Option<String> {
    let path = resolve_path(None, None, env::var(SELF_SID_ENV).ok()).ok()?;
    front_door_probe(&path)
}

#[must_use]
fn front_door_probe(path: &str) -> Option<String> {
    let resolved = aterm_uds::latest::resolve(path);
    CtlStream::connect(&resolved).is_ok().then_some(resolved)
}

/// Hand one request line to the instance at `path` and return its first reply line, trimmed.
pub fn front_door_send(path: &str, request: &str) -> io::Result<String> {
    front_door_send_within(path, request, FRONT_DOOR_DEADLINE, FRONT_DOOR_DEADLINE)
}

/// [`front_door_send`] with `accept` bounding the connect phase and the request's write, and `reply` the wait for the answer to the request.
pub fn front_door_send_within(
    path: &str,
    request: &str,
    accept: std::time::Duration,
    reply: std::time::Duration,
) -> io::Result<String> {
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
    if let Err(e) = (&stream)
        .write_all(request.as_bytes())
        .and_then(|()| (&stream).flush())
    {
        if is_peer_drop(&e)
            && let Some(line) = pending_status_line(&stream)
        {
            return Ok(line);
        }
        return Err(delivered(
            e.kind(),
            format!("could not confirm the request's delivery ({e})"),
        ));
    }
    let mut reader = BufReader::new(&stream);
    let mut line = String::new();
    match read_status_line(&mut reader, &mut line) {
        Ok(0) => Err(delivered(
            io::ErrorKind::UnexpectedEof,
            "closed without answering the request".to_string(),
        )),
        Ok(_) => Ok(line.trim_end_matches(['\r', '\n']).to_string()),
        Err(e) if is_peer_drop(&e) => Err(delivered(
            e.kind(),
            format!("closed before its reply was read ({e})"),
        )),
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

const FRONT_DOOR_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

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

/// Whether a [`front_door_send`] error came after writing the request began.
#[must_use]
pub fn front_door_delivered(e: &io::Error) -> bool {
    e.get_ref().is_some_and(|inner| inner.is::<Delivered>())
}

/// Return whether `line` from [`front_door_send`] is a connect-phase refusal.
#[must_use]
pub fn front_door_refused(line: &str) -> bool {
    is_connection_refusal(line) || line.starts_with("ERR auth;")
}

#[cfg(windows)]
fn allow_foreground_handoff(path: &str) {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn AllowSetForegroundWindow(pid: u32) -> i32;
    }
    let Some(pid) = instance_pid_of(path) else {
        return;
    };
    // SAFETY: documented `AllowSetForegroundWindow` call taking a plain PID with no pointers.
    let _ = unsafe { AllowSetForegroundWindow(pid) };
}

#[cfg(not(windows))]
const fn allow_foreground_handoff(_path: &str) {}

#[cfg(windows)]
fn instance_pid_of(path: &str) -> Option<u32> {
    let resolved = aterm_uds::latest::resolve(path);
    let name = std::path::Path::new(&resolved).file_name()?.to_str()?;
    control_socket::instance_pid(name)
}

const SOCK_FILE: &str = control_socket::LATEST_SOCK_FILE;

fn socket_dir() -> Option<PathBuf> {
    aterm_uds::control_socket_dir()
}

const SELF_SID_ENV: &str = "ATERM_PARENT_SESSION_ID";

fn self_instance_sock(self_sid: Option<&str>) -> Option<String> {
    self_instance_sock_in(&socket_dir()?, self_sid)
}

fn self_instance_sock_in(dir: &Path, self_sid: Option<&str>) -> Option<String> {
    // weird env value can never path-traverse out of the graph dir.
    graph_entry_sock_in(dir, aterm_types::control_request::Sid::parse(self_sid?)?)
}

fn graph_entry_sock_in(dir: &Path, sid: aterm_types::control_request::Sid<'_>) -> Option<String> {
    let body = std::fs::read_to_string(dir.join("graph").join(sid.as_str())).ok()?;
    control_socket::graph_entry_sock(&body)
}

/// The socket of the instance hosting the session `sid`.
#[must_use]
pub fn hosting_sock(dir: &Path, sid: aterm_types::control_request::Sid<'_>) -> Option<String> {
    let sock = graph_entry_sock_in(dir, sid)?;
    let path = Path::new(&sock);
    let canonical_dir = std::fs::canonicalize(dir).ok()?;
    let in_dir = path.is_absolute()
        && path.file_name().is_some()
        && path.parent().and_then(|p| std::fs::canonicalize(p).ok()) == Some(canonical_dir)
        && !std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink());
    in_dir.then_some(sock)
}

/// The flagless resolution as a library entry, for other clients in the binary that must find aterm the way `aterm ctl` does.
pub fn resolve_sock_for(self_sid: Option<&str>) -> io::Result<String> {
    resolve_path(None, None, self_sid.map(str::to_string))
}

/// The instance token beside the socket at `sock` ([`read_token_at`]: no-follow, regular file, bounded).
pub fn read_token_beside(sock: &str) -> io::Result<String> {
    read_token_at(sock)
        .map_err(|(path, e)| io::Error::new(e.kind(), format!("{}: {e}", path.display())))
}

/// A connection to the control socket at `sock`, and the token beside it read only once connected, as this crate's own connect phase does.
pub fn connect_then_read_token(sock: &str) -> io::Result<(CtlStream, io::Result<String>)> {
    connect_then_read(sock, read_token_beside)
}

fn connect_then_read<T>(sock: &str, read: impl FnOnce(&str) -> T) -> io::Result<(CtlStream, T)> {
    let pinned = pin_alias(sock);
    let stream = CtlStream::connect(&pinned)?;
    Ok((stream, read(&pinned)))
}

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

const MUX_ENV: &str = "ATERM_MUX";

const MUX_BASE_ENV: &str = "ATERM_MUX_BASE";

const MUX_OUTER_SID_ENV: &str = "ATERM_MUX_OUTER_SESSION_ID";

const TMUX_ENV: &str = "TMUX";

const SCREEN_ENV: &str = "STY";

const TERM_ENV: &str = "TERM";

const TERM_PROGRAM_ENV: &str = "TERM_PROGRAM";

#[derive(Debug, Clone, PartialEq, Eq)]
struct MuxNesting {
    kind: &'static str,
    outer_sid: Option<String>,
    detected_by: &'static str,
}

fn mux_kind(value: &str) -> Option<&'static str> {
    match value.split(['-', '.']).next().unwrap_or("") {
        "tmux" => Some("tmux"),
        "screen" => Some("screen"),
        _ => None,
    }
}

fn mux_env_signature(tmux: Option<&str>, sty: Option<&str>) -> String {
    let mut sig = String::from(tmux.unwrap_or(""));
    sig.push('|');
    sig.push_str(sty.unwrap_or(""));
    sig
}

fn marker_kind(tmux: Option<&str>, sty: Option<&str>) -> Option<&'static str> {
    if tmux.is_some_and(|v| !v.is_empty()) {
        Some("tmux")
    } else if sty.is_some_and(|v| !v.is_empty()) {
        Some("screen")
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct MuxEnv<'a> {
    mux: Option<&'a str>,
    base: Option<&'a str>,
    outer_sid: Option<&'a str>,
    tmux: Option<&'a str>,
    sty: Option<&'a str>,
    term: Option<&'a str>,
    self_sid: Option<&'a str>,
    term_program: Option<&'a str>,
}

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
        // pane. Different with no marker (unset by hand) names no multiplexer.
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
    outer.as_ref()?;
    let corroborated = set(term)
        .and_then(mux_kind)
        .or_else(|| (set(tmux).is_some() && set(term_program) == Some("tmux")).then_some("tmux"))?;
    Some(MuxNesting {
        kind: marker_kind(tmux, sty).unwrap_or(corroborated),
        outer_sid: outer,
        detected_by: "environment",
    })
}

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

const MUX_EXEMPT_VERBS: [&str; 4] = ["instances", "ls", "windows", "mux"];

fn addresses_no_session(verb: &str) -> bool {
    MUX_EXEMPT_VERBS.contains(&verb)
        || aterm_types::control_verbs::spec(verb)
            .is_some_and(|s| matches!(s.target, aterm_types::control_verbs::Target::Meta))
}

fn targets_own_session_implicitly(parts: &[String], sock: Option<&str>, pid: Option<u32>) -> bool {
    let Some(first) = parts.first() else {
        return false;
    };
    let verb_idx = usize::from(first.starts_with('@'));
    if parts
        .get(verb_idx)
        .is_some_and(|verb| addresses_no_session(verb))
    {
        return false;
    }
    let sel_idx = usize::from(first == "subscribe");
    let selector = parts.get(sel_idx).filter(|t| t.starts_with('@'));
    let unpinned = sock.is_none() && pid.is_none();
    match selector {
        Some(sel) if sel.split(',').any(|e| e == "@self" || e == "@env") => true,
        Some(sel) if sel.split(',').all(|e| e == "@.") => unpinned,
        Some(_) => false,
        None => unpinned,
    }
}

fn nested_self_drive_error(n: &MuxNesting) -> io::Error {
    let sid = n.outer_sid.as_deref().unwrap_or("<unknown>");
    let mut msg = String::from("ERR this shell is inside ");
    msg.push_str(n.kind);
    msg.push_str(": aterm hosts one session for the whole multiplexer and none per pane, so\n  $ATERM_PARENT_SESSION_ID (");
    msg.push_str(sid);
    msg.push_str(") names the terminal running ");
    msg.push_str(n.kind);
    msg.push_str(", not this pane, and this call was refused. Name the target:\n    aterm ctl @");
    msg.push_str(sid);
    msg.push_str(" <verb>     drive the outer terminal deliberately\n    aterm ctl --pid <pid> <verb>   or --sock <path>, to pin one instance\n    aterm ctl instances            lists the pid and socket of every live instance\n    aterm ctl mux                  what aterm can and cannot see from in here\n  OSC 133 marks do not cross ");
    msg.push_str(n.kind);
    msg.push_str(", so blocks, exit codes and cwd for ");
    msg.push_str(sid);
    msg.push_str(" are the multiplexer's, not this pane's.\n  ATERM_MUX=0 turns this check off if this shell is not inside ");
    msg.push_str(n.kind);
    msg.push('.');
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

const MUX_NOTICE_ENV: &str = "ATERM_MUX_NOTICE";

fn mux_boundary_notice(kind: &str) -> String {
    let mut msg = String::from("inside ");
    msg.push_str(kind);
    msg.push_str(
        " — no command blocks, exit codes or cwd tracking in these panes (`aterm ctl mux` \
         explains; ATERM_MUX_NOTICE=0 silences)",
    );
    msg
}

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

fn announce_mux_boundary(nesting: Option<&MuxNesting>, already_said: bool) {
    let Some(n) = nesting else {
        return;
    };
    announce_mux_boundary_with(n, already_said, claim_mux_notice);
}

fn announce_mux_boundary_with(
    n: &MuxNesting,
    already_said: bool,
    claim: impl FnOnce(&MuxNesting) -> bool,
) {
    // unrelated multiplexers.
    if n.outer_sid.is_none() {
        return;
    }
    if claim(n) && !already_said {
        let _ = stderr_line(&mux_boundary_notice(n.kind));
    }
}

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

fn mux_check_off(value: &str) -> bool {
    matches!(value, "0" | "off" | "no" | "none" | "false")
}

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
    note.push_str(". aterm hosts one session for the whole multiplexer, so:\n  * aterm has no session for a pane; ");
    if n.outer_sid.is_some() {
        note.push_str("flagless and @self calls are refused here (they would drive ");
        note.push_str(sid);
        note.push_str(").\n  * ");
    } else {
        note.push_str("no aterm session identity is in scope, so nothing is refused.\n  * ");
    }
    note.push_str(
        "OSC 133 marks are absent, not empty: a pane's shell runs no aterm prompt hooks, and \
         screen/tmux\n    do not pass the marks through. Command blocks, exit codes and cwd \
         tracking stop until you leave.\n  * \
         Verbs that address no session (version, sessions, who, whoami, grant, flows, help) \
         are\n    answered from in here; name a target (@<sid>, --pid, --sock) to drive a \
         session.\n  * \
         Not really inside a multiplexer? ATERM_MUX=0 turns this check off.",
    );
    stderr_line(&note)?;
    Ok(ExitCode::SUCCESS)
}

fn resolve_path(
    sock: Option<String>,
    pid: Option<u32>,
    self_sid: Option<String>,
) -> io::Result<String> {
    resolve_target(sock, pid, self_sid).map(|(path, _)| path)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TargetOrigin {
    Flagless,
    Pinned,
    Hosting,
}

impl TargetOrigin {
    const fn settled(self) -> Self {
        match self {
            Self::Hosting => Self::Flagless,
            origin => origin,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Targets {
    hosting: Vec<Dial>,
    last: LastTarget,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum LastTarget {
    Sock(String),
    Pid(u32, String),
    Flagless {
        dir: PathBuf,
        self_sid: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Dial {
    path: String,
    origin: TargetOrigin,
    follow: redial::Follow,
}

#[derive(Debug)]
struct NoHost(String);

impl std::fmt::Display for NoHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NoHost {}

fn is_no_host(e: &io::Error) -> bool {
    e.get_ref().is_some_and(|inner| inner.is::<NoHost>())
}

impl Targets {
    fn run<T>(&self, mut converse: impl FnMut(Dial) -> io::Result<T>) -> io::Result<T> {
        for target in &self.hosting {
            match converse(target.clone()) {
                Err(e) if is_no_host(&e) => {}
                done => return done,
            }
        }
        converse(self.last_target())
    }

    fn probed(&self) -> (String, TargetOrigin) {
        let dial = self.probed_dial();
        (dial.path, dial.origin)
    }

    fn probed_dial(&self) -> Dial {
        match self
            .hosting
            .iter()
            .find(|target| CtlStream::connect(&target.path).is_ok())
        {
            Some(target) => Dial {
                origin: target.origin.settled(),
                ..target.clone()
            },
            None => self.last_target(),
        }
    }

    fn last_target(&self) -> Dial {
        match &self.last {
            LastTarget::Sock(sock) => Dial {
                path: sock.clone(),
                origin: TargetOrigin::Pinned,
                follow: redial::Follow::Path(sock.clone()),
            },
            LastTarget::Pid(pid, sock) => Dial {
                path: sock.clone(),
                origin: TargetOrigin::Pinned,
                follow: redial::Follow::Nothing(redial::Unfollowed::PidPin(*pid)),
            },
            LastTarget::Flagless { dir, self_sid } => {
                let latest = dir.join(SOCK_FILE).to_string_lossy().into_owned();
                Dial {
                    path: flagless_target(dir, latest),
                    origin: TargetOrigin::Flagless,
                    follow: redial::Follow::Flagless {
                        dir: dir.clone(),
                        sid: self_sid.clone(),
                    },
                }
            }
        }
    }
}

fn resolve_target(
    sock: Option<String>,
    pid: Option<u32>,
    self_sid: Option<String>,
) -> io::Result<(String, TargetOrigin)> {
    Ok(targets(sock, pid, self_sid, None)?.probed())
}

fn targets(
    sock: Option<String>,
    pid: Option<u32>,
    self_sid: Option<String>,
    named: Option<aterm_types::control_request::Sid<'_>>,
) -> io::Result<Targets> {
    targets_in(socket_dir(), sock, pid, self_sid, named)
}

fn targets_in(
    dir: Option<PathBuf>,
    sock: Option<String>,
    pid: Option<u32>,
    self_sid: Option<String>,
    named: Option<aterm_types::control_request::Sid<'_>>,
) -> io::Result<Targets> {
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
    let pinned = |last| Targets {
        hosting: Vec::new(),
        last,
    };
    if let Some(pid) = pid {
        let dir = dir.ok_or_else(no_dir)?;
        let path = dir
            .join(control_socket::instance_sock_name(pid))
            .to_string_lossy()
            .into_owned();
        return Ok(pinned(LastTarget::Pid(pid, path)));
    }
    if let Some(s) = sock {
        return Ok(pinned(LastTarget::Sock(s)));
    }
    let dir = dir.ok_or_else(no_dir)?;
    let hosted = |path: String, sid: &str| Dial {
        path,
        origin: TargetOrigin::Hosting,
        follow: redial::Follow::Flagless {
            dir: dir.clone(),
            sid: Some(sid.to_string()),
        },
    };
    let mut hosting = Vec::new();
    if let Some(sid) = named
        && let Some(host) = hosting_sock(&dir, sid)
    {
        hosting.push(hosted(host, sid.as_str()));
    }
    if let Some(sid) = self_sid.as_deref()
        && let Some(own) = self_instance_sock_in(&dir, Some(sid))
        && !hosting.iter().any(|target| target.path == own)
    {
        hosting.push(hosted(own, sid));
    }
    Ok(Targets {
        hosting,
        last: LastTarget::Flagless { dir, self_sid },
    })
}

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

fn newest(
    live: impl IntoIterator<Item = (Option<std::time::SystemTime>, u32, String)>,
) -> Option<String> {
    live.into_iter().max().map(|(_, _, sock)| sock)
}

const PROBE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(2);

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

#[derive(Debug)]
enum Probe {
    Answered(Vec<String>),
    Refused { pid_running: Option<bool> },
    Denied(io::Error),
    NoToken(PathBuf, io::Error),
    AuthRejected,
    Busy(String),
    Replied(String),
    Foreign { token: PathBuf, facts: ForeignFacts },
    Timeout,
    Other(io::Error),
}

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
            Probe::Replied(status.to_string())
        } else {
            Probe::Other(malformed_header_error(status))
        };
    };
    if let Some(e) = unwritten {
        return io_probe(e);
    }
    let Some(count) = stream_count(tail) else {
        return Probe::Other(malformed_header_error(status));
    };
    match collect_reply_lines(count, MAX_BODY_BYTES, |line| {
        probe_read_bounded_line(&mut reader, line, deadline)
    }) {
        Ok(lines) => Probe::Answered(lines),
        Err(e) => io_probe(e),
    }
}

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

fn io_probe(e: io::Error) -> Probe {
    if is_timeout_error(&e) {
        Probe::Timeout
    } else {
        Probe::Other(e)
    }
}

#[derive(Debug)]
enum DirOutcome {
    Unresolvable,
    Missing { path: PathBuf, why: String },
    Unreadable { path: PathBuf, err: io::Error },
    Empty(PathBuf),
    Found { path: PathBuf, n: usize },
    Scoped { live_elsewhere: usize },
}

#[cfg(target_os = "macos")]
const DIR_ENV_UNSET: &str = "$XDG_RUNTIME_DIR and $HOME are both unset";
#[cfg(all(unix, not(target_os = "macos")))]
const DIR_ENV_UNSET: &str = "$XDG_RUNTIME_DIR, $XDG_STATE_HOME and $HOME are all unset";
#[cfg(windows)]
const DIR_ENV_UNSET: &str = "%TMP%, %TEMP% and %LOCALAPPDATA% are all unset";

#[cfg(not(windows))]
fn missing_dir_reason() -> String {
    if env::var_os("XDG_RUNTIME_DIR").is_some() {
        let mut why = String::from("resolved from $XDG_RUNTIME_DIR, which is set — so ");
        why.push_str(aterm_uds::CONTROL_SOCKET_DIR_DEFAULT);
        why.push_str(" was NOT consulted");
        why
    } else if cfg!(target_os = "macos") {
        "resolved from $HOME; no aterm instance has published a socket there".to_string()
    } else {
        "resolved from $XDG_STATE_HOME, else $HOME; no aterm instance has published a \
         socket there"
            .to_string()
    }
}
#[cfg(windows)]
fn missing_dir_reason() -> String {
    "resolved from %TMP%/%TEMP% (else %LOCALAPPDATA%\\Temp); no aterm instance has \
     published a socket there"
        .to_string()
}

fn other_elevation_note() -> Option<String> {
    #[cfg(windows)]
    {
        let elevated = aterm_uds::elevation::is_elevated();
        let other = socket_dir()?.with_file_name(if elevated {
            aterm_uds::CONTROL_DIR_NAME
        } else {
            aterm_uds::ELEVATED_CONTROL_DIR_NAME
        });
        if !other.is_dir() {
            return None;
        }
        let shown = other.display();
        Some(if elevated {
            format!(
                "NOTE: this shell is elevated, so it lists elevated aterm instances only; \
                 unelevated ones publish in {shown} (name one with --sock)"
            )
        } else {
            format!(
                "NOTE: elevated aterm instances keep their control sockets in {shown}, which \
                 only an elevated shell can list"
            )
        })
    }
    #[cfg(not(windows))]
    {
        None
    }
}

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

fn live_instances() -> Vec<(u32, String)> {
    inspect_fleet().1.into_iter().map(Target::listed).collect()
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Target {
    pid: u32,
    sock: String,
    written: Option<std::time::SystemTime>,
}

impl Target {
    const fn named(pid: u32, sock: String) -> Self {
        Self {
            pid,
            sock,
            written: None,
        }
    }

    fn listed(self) -> (u32, String) {
        (self.pid, self.sock)
    }
}

fn enumerate_instances(dir: &Path, entries: std::fs::ReadDir) -> Vec<Target> {
    let mut out: Vec<Target> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let remember = |seen: &mut HashSet<String>, sock: &str| -> bool {
        let key = control_socket::socket_key(
            &std::fs::canonicalize(sock)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| sock.to_string()),
        );
        seen.insert(key)
    };
    for (pid, sock) in instance_socks_in(dir, entries) {
        if remember(&mut seen, &sock) {
            out.push(Target::named(pid, sock));
        }
    }
    if let Ok(entries) = std::fs::read_dir(dir.join("graph")) {
        for e in entries.flatten() {
            let Ok((body, written)) = graph_hygiene::read_entry(&e.path()) else {
                continue;
            };
            let Some(sock) = control_socket::graph_entry_sock(&body) else {
                continue;
            };
            let pid = control_socket::graph_entry_pid(&body).unwrap_or(0);
            if pid != 0 && graph_hygiene::writer_gone(pid, written) {
                // Only a dead pid's entry is removed (`graph_hygiene` says why).
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

#[derive(Debug, Default, Clone, PartialEq, Eq)]
enum ForeignFacts {
    #[default]
    Unknown,
    Stale,
    NotAterm(String),
    Writer {
        orphaned: bool,
        binary_deleted: bool,
        up: Option<std::time::Duration>,
    },
}

#[derive(Debug)]
struct ProcessReading {
    birth: aterm_uds::process::ProcessBirth,
    slack: std::time::Duration,
    parent: Option<u32>,
    exe: Option<(PathBuf, io::Result<bool>)>,
    birth_after: Option<aterm_uds::process::ProcessBirth>,
}

impl ForeignFacts {
    fn read(pid: u32, written: Option<std::time::SystemTime>) -> Self {
        use aterm_uds::process::{read_process_parent, read_process_path};
        let (Ok(p), Some(_)) = (i32::try_from(pid), written) else {
            return Self::Unknown;
        };
        let clocks = graph_hygiene::Clocks::default();
        let Some(birth) = graph_hygiene::read_birth(p, &clocks) else {
            return Self::Unknown;
        };
        let parent = read_process_parent(p);
        let exe = read_process_path(p).map(|path| {
            let exists = path.try_exists();
            (path, exists)
        });
        let reading = ProcessReading {
            birth: birth.at,
            slack: birth.slack,
            parent,
            exe,
            birth_after: graph_hygiene::read_birth(p, &clocks).map(|b| b.at),
        };
        Self::judge(written, std::time::SystemTime::now(), reading)
    }

    fn judge(
        written: Option<std::time::SystemTime>,
        now: std::time::SystemTime,
        r: ProcessReading,
    ) -> Self {
        match written.and_then(|w| graph_hygiene::is_entry_writer(r.birth, r.slack, w)) {
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

    const fn orphaned(&self) -> bool {
        matches!(self, Self::Writer { orphaned: true, .. })
    }

    const fn writer_gone(&self) -> bool {
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
            said.push(if cfg!(target_vendor = "apple") {
                "its parent is launchd (its launcher exited, or launchd started it)".to_string()
            } else {
                "its parent is init (its launcher exited)".to_string()
            });
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

fn same_socket_path(a: &str, b: &str) -> bool {
    if a == b || control_socket::socket_key(a) == control_socket::socket_key(b) {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => false,
    }
}

fn discovery_targets(
    sock: Option<&str>,
    pid: Option<u32>,
) -> Result<Vec<(u32, String)>, (String, u8)> {
    if let Some(s) = sock {
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

fn run_discovery(
    verb: &str,
    sock: Option<&str>,
    pid: Option<u32>,
    nesting: Option<&MuxNesting>,
) -> io::Result<ExitCode> {
    let self_sid = env::var(SELF_SID_ENV).ok();
    let self_sock = self_instance_sock(self_sid.as_deref());
    let stdout = io::stdout();
    let mut out = StdoutSink::new(stdout.lock());
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
    let mut probes: Vec<(u32, String, Probe)> = Vec::with_capacity(targets.len());
    let mut answered: Vec<FleetSession> = Vec::new();
    let rendezvous = rendezvous_of(&dir);
    for (pid, sock, probe) in probe_sessions(targets, rendezvous) {
        if let Probe::Answered(sessions) = &probe {
            let is_self_instance = self_sock
                .as_deref()
                .is_some_and(|self_s| same_socket_path(self_s, &sock));
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
    if let DirOutcome::Scoped { live_elsewhere } = &mut dir
        && dead_target_remedy_due(&probes)
    {
        let dialled: Vec<&str> = probes.iter().map(|(_, s, _)| s.as_str()).collect();
        *live_elsewhere = live_instances_besides(&dialled).len();
    }
    let foreign = foreign_note(verb, &probes);
    let none_here = matches!(dir, DirOutcome::Empty(_) | DirOutcome::Missing { .. });
    let (report, code) = discovery_report(dir, &probes);
    if code != 0 {
        stderr_line(&report)?;
        if none_here && let Some(note) = other_elevation_note() {
            stderr_line(&note)?;
        }
        return Ok(ExitCode::from(code));
    }
    if let Some(note) = foreign {
        stderr_line(&note)?;
    }
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
    if let Some(n) = nesting.filter(|n| n.outer_sid.is_some()) {
        let mut note = String::from("NOTE: this shell is inside ");
        note.push_str(n.kind);
        note.push_str(", so the marked session hosts the WHOLE multiplexer, not your pane");
        stderr_line(&note)?;
    }
    Ok(ExitCode::SUCCESS)
}

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

fn rendezvous_of(dir: &DirOutcome) -> Option<PathBuf> {
    match dir {
        DirOutcome::Found { path, .. } => Some(path.clone()),
        _ => None,
    }
}

/// One live session in the fleet, exactly as `ls` prints it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetSession {
    pub pid: u32,
    pub row: String,
    pub is_self: bool,
}

impl FleetSession {
    /// The session id (the row's second column), what `@<sid>` addresses.
    #[must_use]
    pub fn sid(&self) -> Option<&str> {
        self.row.split_whitespace().nth(1)
    }
}

/// Why a fleet listing produced no sessions, as `ls` reports on stderr.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetListError {
    pub reason: String,
    pub code: u8,
}

impl std::fmt::Display for FleetListError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

/// The `ls` listing as data.
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

/// Every aterm instance that has published a control socket on this machine, as `(pid, socket path)`.
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

/// Whether `aterm ctl --pid <pid>` reaches the instance on `sock`.
#[must_use]
pub fn pid_reaches(pid: u32, sock: &str) -> bool {
    pid != 0 && reached_by_pid(socket_dir().as_deref(), pid, sock)
}

fn instance_sessions(pid: u32, rows: &[String], self_sid: Option<&str>) -> Vec<FleetSession> {
    rows.iter()
        .map(|row| FleetSession {
            pid,
            row: row.clone(),
            is_self: {
                let sid = row.split_whitespace().nth(1);
                sid.is_some() && sid == self_sid
            },
        })
        .collect()
}

fn duplicate_sids(sessions: &[FleetSession]) -> Vec<(String, Vec<u32>)> {
    let mut order: Vec<&str> = Vec::new();
    let mut claims: std::collections::HashMap<&str, Vec<u32>> = std::collections::HashMap::new();
    for session in sessions {
        let Some(sid) = session.sid() else {
            // Distinct branch.
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

fn fleet_listing(
    sessions: Vec<FleetSession>,
    verdict: (String, u8),
) -> Result<Vec<FleetSession>, FleetListError> {
    match verdict {
        (_, 0) => Ok(sessions),
        (reason, code) => Err(FleetListError { reason, code }),
    }
}

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

const EXIT_UNREACHABLE: u8 = 2;

const EXIT_EMPTY_FLEET: u8 = 1;

fn discovery_report(dir: DirOutcome, probes: &[(u32, String, Probe)]) -> (String, u8) {
    if probes
        .iter()
        .any(|(_, _, p)| matches!(p, Probe::Answered(_)))
    {
        return (String::new(), 0);
    }
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

fn socket_label(sock: &str, in_dir: Option<&Path>) -> String {
    let p = Path::new(sock);
    match (in_dir, p.file_name()) {
        (Some(dir), Some(name)) if p.parent() == Some(dir) => name.to_string_lossy().into_owned(),
        _ => sock.to_string(),
    }
}

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
        Probe::Replied(said) => format!("server replied: {said}"),
        Probe::Other(e) => e.to_string(),
    }
}

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

const fn cause_of(probe: &Probe) -> Option<Cause> {
    match probe {
        Probe::Answered(_) => None,
        Probe::Refused { .. } => Some(Cause::Refused),
        Probe::Denied(_) => Some(Cause::Denied),
        Probe::NoToken(..) => Some(Cause::NoToken),
        Probe::AuthRejected => Some(Cause::AuthRejected),
        Probe::Busy(_) => Some(Cause::Busy),
        Probe::Foreign { .. } => Some(Cause::Foreign),
        Probe::Timeout => Some(Cause::Timeout),
        Probe::Replied(_) | Probe::Other(_) => Some(Cause::Other),
    }
}

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

fn dead_target_remedy_due(probes: &[(u32, String, Probe)]) -> bool {
    !probes
        .iter()
        .any(|(_, _, p)| matches!(p, Probe::Answered(_)))
        && dominant_cause(probes) == Some(Cause::Refused)
        && !refused_but_running(probes)
}

fn dominant_hint(probes: &[(u32, String, Probe)], scoped: Option<usize>) -> Option<String> {
    let hint = match dominant_cause(probes)? {
        Cause::Denied => {
            "the control socket is refused by this sandbox — nothing to configure".to_string()
        }
        Cause::NoToken => "the token beside a socket must be readable by THIS user (it is \
                           mode 0600); another user's instance cannot be driven"
            .to_string(),
        Cause::AuthRejected => "re-run once; if it persists, `aterm ctl --sock <path> sessions` \
                                shows one socket's own answer"
            .to_string(),
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
        Cause::Foreign => {
            let drive = "only whoever started a `foreign` instance can drive it";
            let foreign = probes.iter().filter_map(|(pid, _, p)| match p {
                Probe::Foreign { facts, .. } => Some((*pid, facts)),
                _ => None,
            });
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

fn read_token_for(path: &str) -> Option<String> {
    read_token_at(path).ok()
}

const MAX_CONTROL_TOKEN_FILE_BYTES: u64 = 256;

#[cfg(all(
    any(target_os = "linux", target_os = "android"),
    any(target_arch = "x86", target_arch = "x86_64", target_arch = "riscv64"),
))]
const TOKEN_FILE_OPEN_FLAGS: i32 = 0x0002_0000 | 0x0000_0800; // O_NOFOLLOW | O_NONBLOCK

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

/// Read a control capability token without following a final symlink or ever allocating beyond the token-file bound.
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

fn read_token_at(path: &str) -> Result<String, (PathBuf, io::Error)> {
    read_token_file_at(path).map(|(_, token)| token)
}

fn read_token_file_at(path: &str) -> Result<(PathBuf, String), (PathBuf, io::Error)> {
    let p = Path::new(path);
    let (Some(dir), Some(file_name)) = (p.parent(), p.file_name()) else {
        return Err((
            PathBuf::from(path),
            io::Error::new(io::ErrorKind::NotFound, "socket path names no directory"),
        ));
    };
    let sock_name = aterm_uds::latest::target_name(p).unwrap_or_else(|| file_name.to_os_string());
    let names = control_socket::token_names_for_sock(&sock_name.to_string_lossy());
    let mut miss: Option<(PathBuf, io::Error)> = None;
    for name in names {
        let token_path = dir.join(name);
        match read_control_token_file(&token_path) {
            Ok(token) => return Ok((token_path, token)),
            Err(e) => {
                // reach past it for a directory-shared legacy credential.
                if matches!(
                    e.kind(),
                    io::ErrorKind::InvalidData | io::ErrorKind::InvalidInput
                ) {
                    return Err((token_path, e));
                }
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

const EXIT_TIMEOUT: u8 = 124;

fn is_timeout_error(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

fn no_reply_error(e: io::Error, path: &str, deadline: Option<std::time::Duration>) -> io::Error {
    let Some(deadline) = deadline else {
        return e;
    };
    if !is_timeout_error(&e) || is_worded(&e) {
        return e;
    }
    let mut msg = String::new();
    if is_unanswered(&e) {
        msg.push_str("no reply from ");
        msg.push_str(path);
        msg.push_str(" within ");
        msg.push_str(&deadline_words(deadline));
        msg.push_str(" (connected; the server did not answer)");
    } else {
        msg.push_str("reply from ");
        msg.push_str(path);
        msg.push_str(" stopped part-way: nothing more within ");
        msg.push_str(&deadline_words(deadline));
        msg.push_str(" (connected; the server began its answer, then went quiet)");
    }
    msg.push_str(" — the request timed out");
    io::Error::new(e.kind(), Worded(io::Error::other(msg)))
}

#[derive(Debug)]
struct Unanswered(io::Error);

impl std::fmt::Display for Unanswered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for Unanswered {}

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

fn is_ok_reply(header: &str) -> bool {
    header == "OK" || header.starts_with("OK ")
}

fn read_status_line<R: BufRead>(reader: &mut R, line: &mut String) -> io::Result<usize> {
    read_bounded_line(reader, line).map_err(unanswered)
}

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

#[derive(Debug)]
struct Worded(io::Error);

impl std::fmt::Display for Worded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for Worded {}

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

/// The whole client with no local verbs.
pub fn main_entry(argv: Vec<std::ffi::OsString>) -> ExitCode {
    main_entry_with(argv, LocalVerbs::NONE)
}

/// [`main_entry`] with the verbs the embedding binary answers in-process ([`LocalVerbs`]).
pub fn main_entry_with(argv: Vec<std::ffi::OsString>, local: LocalVerbs) -> ExitCode {
    match real_main(argv, local) {
        Ok(code) => code,
        Err(e) => {
            // wrong, so no line and status 0.
            if is_stdout_closed(&e) {
                return ExitCode::SUCCESS;
            }
            let _ = stderr_line(&e.to_string());
            exit_for(&e)
        }
    }
}

fn exit_for(e: &io::Error) -> ExitCode {
    if is_stdout_closed(e) {
        ExitCode::SUCCESS
    } else if is_timeout_error(e) {
        ExitCode::from(EXIT_TIMEOUT)
    } else {
        ExitCode::FAILURE
    }
}

#[derive(Debug)]
struct StdoutClosed;

impl std::fmt::Display for StdoutClosed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("stdout closed by its reader")
    }
}

impl std::error::Error for StdoutClosed {}

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

struct StdoutSink<W: Write> {
    out: W,
    tty: Option<aterm_codec::tty::SafeStream>,
}

impl<W: Write> StdoutSink<W> {
    fn new(out: W) -> Self {
        Self::onto(out, stdout_is_terminal())
    }

    fn onto(out: W, terminal: bool) -> Self {
        Self {
            out,
            tty: terminal.then(aterm_codec::tty::SafeStream::new),
        }
    }
}

impl<W: Write> Write for StdoutSink<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let Some(tty) = &mut self.tty else {
            return self.out.write(buf).map_err(stdout_failure);
        };
        let mut shown = String::with_capacity(buf.len());
        tty.push(buf, &mut shown);
        self.out
            .write_all(shown.as_bytes())
            .map_err(stdout_failure)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush().map_err(stdout_failure)
    }
}

impl<W: Write> Drop for StdoutSink<W> {
    fn drop(&mut self) {
        if let Some(tty) = &mut self.tty {
            let mut rest = String::new();
            tty.finish(&mut rest);
            if !rest.is_empty() {
                let _ = self.out.write_all(rest.as_bytes());
                let _ = self.out.flush();
            }
        }
    }
}

fn stdout_is_terminal() -> bool {
    io::IsTerminal::is_terminal(&stdout_handle())
}

fn stderr_is_terminal() -> bool {
    io::IsTerminal::is_terminal(&io::stderr())
}

fn stdout_failure(e: io::Error) -> io::Error {
    worded(stdout_closed(e))
}

fn usage_error() -> io::Error {
    let mut msg = String::from("usage: ");
    msg.push_str(SYNOPSIS);
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

const DISCOVERY_VERBS: [&str; 3] = ["ls", "instances", "windows"];

fn discovery_given_arguments(parts: &[String]) -> Option<&'static str> {
    let verb = parts.first().map(String::as_str)?;
    let verb = DISCOVERY_VERBS.iter().copied().find(|v| *v == verb)?;
    (parts.len() > 1).then_some(verb)
}

fn discovery_usage_error(verb: &str) -> io::Error {
    let mut msg = String::from("ERR usage: ");
    msg.push_str(verb);
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

/// The verbs the client-verbs block of `--help` documents, which `help <verb>` answers here.
pub const CLIENT_HELP_VERBS: [&str; 4] = ["ls", "instances", "windows", "mux"];

fn client_verb_entry(verb: &str) -> Option<Vec<String>> {
    const HELP_INDENT: &str = "    ";
    const CONTINUATION: &str = "                  ";
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HelpAsked {
    ByFlag,
    ByVerb,
}

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
                aterm_types::version::app_version()
            ))?;
            return Ok(ExitCode::SUCCESS);
        }
        stderr_line(status)?;
        return Ok(ExitCode::FAILURE);
    };
    let n = stream_count(tail).ok_or_else(|| malformed_header_error(status))?;
    let rows = read_reply_lines(&mut reader, n)?;
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

fn print_framed_lines(reply: &str) -> io::Result<()> {
    let stdout = stdout_handle();
    let mut out = StdoutSink::new(io::BufWriter::new(stdout.lock()));
    for line in reply.lines().skip(1) {
        out.write_all(line.as_bytes())?;
        out.write_all(b"\n")?;
    }
    out.flush()
}

#[allow(
    clippy::unnecessary_map_on_constructor,
    reason = "OsString::into_string is passed as a fn-item to a combinator so the Trust lane (`ship trust run`) keeps it an opaque cross-crate call instead of inlining std internals it cannot lower"
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

fn real_main(argv: Vec<std::ffi::OsString>, local: LocalVerbs) -> io::Result<ExitCode> {
    let mut args = utf8_args(argv)?.into_iter();
    let mut sock: Option<String> = None;
    let mut pid: Option<u32> = None;
    let mut host: Option<String> = None;
    let mut deadline: Option<std::time::Duration> = Some(EXCHANGE_DEADLINE);
    let mut timeout_explicit = false;
    let mut request_parts: Vec<String> = Vec::new();

    while let Some(arg) = args.next() {
        if arg == "-h" || arg == "--help" {
            let help = help_text();
            let stdout = stdout_handle();
            let mut out = StdoutSink::new(stdout.lock());
            out.write_all(help.as_bytes())?;
            drop(out);
            return Ok(ExitCode::SUCCESS);
        } else if arg == "-V" || arg == "--version" {
            print_stdout_line(&format!(
                "aterm-ctl {}",
                aterm_types::version::app_version()
            ))?;
            return Ok(ExitCode::SUCCESS);
        } else if arg == "--completions" {
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
        } else if arg == "--host" {
            host = Some(args.next().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "--host requires a HOST (a fleet peer's name, node id or address)",
                )
            })?);
        } else if let Some(h) = arg.strip_prefix("--host=") {
            host = Some(h.to_string());
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
            request_parts.extend(args.by_ref());
            break;
        } else if arg.starts_with('-') {
            let mut msg = String::from("unknown option ");
            msg.push_str(&arg);
            msg.push_str(" (aterm ctl --help lists them)");
            return Err(io::Error::new(io::ErrorKind::InvalidInput, msg));
        } else {
            request_parts.push(arg);
            request_parts.extend(args.by_ref());
            break;
        }
    }

    let mut help_asked = HelpAsked::ByVerb;
    if let Some(rewritten) = help_request_rewrite(&request_parts) {
        help_asked = HelpAsked::ByFlag;
        if matches!(request_parts[0].as_str(), "send" | "paste") {
            let verb = &request_parts[0];
            eprintln!(
                "note: `--help` describes `{verb}`; to send it as literal text, \
                 pipe it -- printf %s -- --help | aterm ctl {verb} --stdin"
            );
        }
        request_parts = rewritten;
    }
    if let [verb, target] = request_parts.as_slice()
        && verb == "status"
        && (target.starts_with('@') || target.starts_with("s-"))
    {
        request_parts = vec![
            format!("@{}", target.trim_start_matches('@')),
            "status".to_string(),
        ];
    }

    if let Some(host) = host.as_deref() {
        if sock.is_some() || pid.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "--host names another Mac's aterm; --sock and --pid name one of this Mac's",
            ));
        }
        refuse_empty_selectors(&request_parts)?;
        if let Some(reply) = client_help_reply(&request_parts, help_asked) {
            print_framed_lines(&reply)?;
            return Ok(ExitCode::SUCCESS);
        }
        return remote::run(
            host,
            &request_parts,
            timeout_explicit.then_some(deadline),
            &local,
        );
    }

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

    refuse_empty_selectors(&request_parts)?;
    expand_self_selector(&mut request_parts, env::var(SELF_SID_ENV).ok())?;
    add_sys_caller(&mut request_parts, env::var(SELF_SID_ENV).ok().as_deref());

    announce_mux_boundary(
        nesting.as_ref(),
        nesting
            .as_ref()
            .is_some_and(|n| mux_degradation_note(n, &request_parts).is_some()),
    );

    let Some(verb) = forwarded_verb(&request_parts) else {
        return Err(usage_error());
    };

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
    if is_sys_instances_all(&request_parts) {
        return run_sys_instances_all(&request_parts, sock.as_deref(), pid);
    }
    if survey::is_survey(&request_parts) {
        return survey::run_survey(&request_parts, sock.as_deref(), pid, &local);
    }
    if let Some(reply) = client_help_reply(&request_parts, help_asked) {
        print_framed_lines(&reply)?;
        return Ok(ExitCode::SUCCESS);
    }
    if let Some(code) = drift::offline_entry(&request_parts, &local)? {
        return Ok(code);
    }

    let pinned = sock.is_some() || pid.is_some();
    let joined = request_parts.join(" ");
    let targets = targets(
        sock,
        pid,
        env::var(SELF_SID_ENV).ok(),
        redial::target_sid(&joined),
    )?;

    if await_task::is_await_task(&request_parts) {
        validate_request_parts(&request_parts)?;
        let dial = targets.probed_dial();
        return await_task::run_await_task(
            &request_parts,
            pinned,
            &dial.path,
            &redial::Redial::new(dial.follow),
            timeout_explicit.then_some(deadline),
        );
    }
    if await_legs::is_rearmed_await(&request_parts) {
        validate_request_parts(&request_parts)?;
        let dial = targets.probed_dial();
        return await_legs::run_rearmed_await(
            &request_parts,
            &dial.path,
            dial.origin,
            &redial::Redial::new(dial.follow),
            timeout_explicit.then_some(deadline),
        );
    }

    let (selector, rest) = split_selector(&request_parts);
    let verb_tok = rest.first().map(String::as_str);
    let verb_args = rest.get(1..).unwrap_or(&[]);
    if verb_tok == Some("feed-bin") {
        // bare length) has no client path and would only desync the frame.
        if !verb_args.is_empty() {
            return Err(feed_bin_inline_error());
        }
        let payload = read_stdin_payload()?;
        let prefix = binary_frame_prefix(selector, "feed-bin");
        return targets.run(|t| feed_bin_exchange(&t.path, t.origin, &prefix, &payload, deadline));
    }
    if matches!(verb_tok, Some("send") | Some("paste"))
        && verb_args.len() == 1
        && matches!(verb_args[0].as_str(), "--stdin" | "-")
    {
        let payload = read_stdin_payload()?;
        let frame_verb = if verb_tok == Some("paste") {
            "paste-bin"
        } else {
            "feed-bin"
        };
        let prefix = binary_frame_prefix(selector, frame_verb);
        return targets.run(|t| feed_bin_exchange(&t.path, t.origin, &prefix, &payload, deadline));
    }
    if verb_tok == Some("spawn") && verb_args.iter().any(|a| a.starts_with("agent=")) {
        let starter = worker_starter(env::var(SELF_SID_ENV).ok());
        let selector = worker_selector(selector, pinned, starter.as_deref());
        let head = worker_spawn_head(
            selector.as_deref(),
            verb_args,
            starter.as_deref(),
            env::current_dir().ok().as_deref(),
        )?;
        let payload = read_task_payload()?;
        let head = format!("{head} len={}", payload.len());
        return targets.run(|t| worker_exchange(&t.path, t.origin, &head, &payload, deadline));
    }

    validate_request_parts(&request_parts)?;

    {
        let (selector, rest) = split_selector(&request_parts);
        if drift::is_cast_drift(rest) {
            return targets
                .run(|t| drift::live(&t.path, t.origin, selector, &rest[2..], deadline, &local));
        }
    }
    if let [help, name] = request_parts.as_slice()
        && help == "help"
        && aterm_types::control_verbs::spec(name).is_some()
    {
        return targets.run(|t| help_with_fallback(&t.path, t.origin, name, deadline, &local));
    }

    if let Some(note) = split_pattern_note(&request_parts) {
        let _ = stderr_line(&note);
    }

    let mut request = request_line(&request_parts)?;
    validate_request_line_len(
        &request,
        forwarded_request(&request_parts)
            .first()
            .map(String::as_str),
    )?;
    request.push('\n');

    let outcome = targets.run(|t| {
        exchange(
            &t.path,
            t.origin,
            &request,
            &verb,
            deadline,
            timeout_explicit,
            &redial::Redial::new(t.follow),
        )
    });
    if let Some(note) = nesting
        .as_ref()
        .filter(|_| env::var(MUX_NOTICE_ENV).ok().as_deref() != Some("0"))
        .and_then(|n| mux_degradation_note(n, &request_parts))
    {
        let _ = stderr_line(&note);
    }
    outcome
}

fn split_selector(parts: &[String]) -> (Option<&str>, &[String]) {
    match parts.first() {
        Some(s) if s.starts_with('@') => (Some(s.as_str()), &parts[1..]),
        _ => (None, parts),
    }
}

fn binary_frame_prefix(selector: Option<&str>, verb: &str) -> String {
    let mut s = String::new();
    if let Some(sel) = selector {
        s.push_str(sel);
        s.push(' ');
    }
    s.push_str(verb);
    s
}

fn selector_index(parts: &[String]) -> usize {
    usize::from(parts.first().map(String::as_str) == Some("subscribe"))
}

fn refuse_empty_selectors(parts: &[String]) -> io::Result<()> {
    let Some(tok) = parts
        .get(selector_index(parts))
        .filter(|t| t.starts_with('@'))
    else {
        return Ok(());
    };
    let names_nothing = tok.split(',').any(|element| {
        element
            .strip_prefix('@')
            .is_some_and(|body| !aterm_types::control_verbs::selector_body_names_something(body))
    });
    if names_nothing {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            aterm_types::control_verbs::EMPTY_SELECTOR,
        ));
    }
    Ok(())
}

fn expand_self_selector(parts: &mut [String], self_sid: Option<String>) -> io::Result<()> {
    let sel_idx = selector_index(parts);
    let Some(tok) = parts.get(sel_idx) else {
        return Ok(());
    };
    let has_self = tok.split(',').any(|e| e == "@self" || e == "@env");
    if !has_self {
        return Ok(());
    }
    let sid = self_sid
        .filter(|s| aterm_types::control_verbs::selector_body_names_something(s))
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

fn self_selector_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "ERR @self: not inside an aterm session ($ATERM_PARENT_SESSION_ID unset or empty)",
    )
}

fn parse_timeout(v: &str) -> io::Result<Option<std::time::Duration>> {
    let secs: u64 = v.parse().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "--timeout requires a non-negative integer number of SECONDS (0 = no deadline)",
        )
    })?;
    Ok((secs != 0).then(|| std::time::Duration::from_secs(secs)))
}

const MAX_FEED_BIN: usize = 256 * 1024;

fn feed_bin_inline_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "feed-bin reads its binary payload from stdin: pipe the bytes in \
         (e.g. `printf ... | aterm-ctl feed-bin`); no inline length is accepted",
    )
}

fn oversized_feed_bin_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "stdin payload exceeds the 256 KiB feed-bin cap; split it into smaller frames",
    )
}

fn read_stdin_payload() -> io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    io::stdin()
        .lock()
        .take(MAX_FEED_BIN as u64 + 1)
        .read_to_end(&mut buf)?;
    if buf.len() > MAX_FEED_BIN {
        return Err(oversized_feed_bin_error());
    }
    Ok(buf)
}

fn read_task_payload() -> io::Result<Vec<u8>> {
    let mut task = Vec::new();
    io::stdin()
        .lock()
        .take(MAX_TASK_BYTES as u64 + 1)
        .read_to_end(&mut task)?;
    validate_task_payload(&task)?;
    Ok(task)
}

fn validate_task_payload(task: &[u8]) -> io::Result<()> {
    if task.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "spawn agent=: the task on stdin is empty",
        ));
    }
    if task.len() > MAX_TASK_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("spawn agent=: the task on stdin is over {MAX_TASK_BYTES} bytes"),
        ));
    }
    Ok(())
}

fn worker_starter(self_sid: Option<String>) -> Option<String> {
    self_sid.filter(|sid| !sid.is_empty() && !sid.contains(char::is_whitespace))
}

fn worker_selector(given: Option<&str>, pinned: bool, starter: Option<&str>) -> Option<String> {
    match given {
        Some(sel) => Some(sel.to_string()),
        None if pinned => None,
        None => starter.map(|sid| format!("@{sid}")),
    }
}

fn worker_spawn_head(
    selector: Option<&str>,
    args: &[String],
    starter: Option<&str>,
    pwd: Option<&std::path::Path>,
) -> io::Result<String> {
    let usage = |why: &str| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "spawn agent=: {why} (aterm ctl @self spawn agent=claude [model=<m>] [cwd=<path>] - < task)"
            ),
        )
    };
    let Some((last, args)) = args.split_last() else {
        return Err(usage("the task is read from stdin: end the request with -"));
    };
    if last != "-" {
        return Err(usage("the task is read from stdin: end the request with -"));
    }
    if args.iter().any(|a| a.starts_with("len=")) {
        return Err(usage("len= is the client's to write"));
    }
    validate_request_parts(args)?;
    let here = || pwd.ok_or_else(|| usage("this directory cannot be read: say an absolute cwd="));
    let mut parts: Vec<String> = selector.map(str::to_string).into_iter().collect();
    parts.push("spawn".to_string());
    for arg in args {
        let given = arg.strip_prefix("cwd=").map(|v| {
            v.strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(v)
        });
        parts.push(match given {
            Some(dir) if !dir.is_empty() && Path::new(dir).is_relative() => {
                let dir: PathBuf = here()?.join(dir).components().collect();
                if !dir.is_absolute() {
                    return Err(usage("a folder on another drive: say an absolute cwd="));
                }
                worker_cwd_arg(&dir)
                    .map_err(|why| usage(&format!("{why}: say an absolute cwd=")))?
            }
            _ => arg.clone(),
        });
    }
    if let Some(sid) = starter
        && !args.iter().any(|a| a.starts_with("from="))
    {
        parts.push(format!("from={sid}"));
    }
    if !args.iter().any(|a| a.starts_with("cwd=")) {
        parts.push(worker_cwd_arg(here()?).map_err(|why| usage(&format!("{why}: say cwd=")))?);
    }
    let head = spawn_wire_line(&parts)?;
    validate_request_line_len(&head, Some("spawn"))?;
    Ok(head)
}

fn request_line(parts: &[String]) -> io::Result<String> {
    if forwarded_verb(parts).as_deref() == Some("spawn") {
        spawn_wire_line(parts)
    } else {
        Ok(parts.join(" "))
    }
}

fn spawn_wire_line(parts: &[String]) -> io::Result<String> {
    let parts = parts
        .iter()
        .map(|part| spawn_wire_cwd(part))
        .collect::<io::Result<Vec<_>>>()?;
    Ok(parts.join(" "))
}

fn spawn_wire_cwd(part: &str) -> io::Result<std::borrow::Cow<'_, str>> {
    use std::borrow::Cow;
    let Some(dir) = part.strip_prefix("cwd=") else {
        return Ok(Cow::Borrowed(part));
    };
    let wrapped = dir
        .strip_prefix('"')
        .and_then(|d| d.strip_suffix('"'))
        .is_some_and(|inner| !inner.contains('"'));
    if !dir.contains(char::is_whitespace) || wrapped {
        return Ok(Cow::Borrowed(part));
    }
    if dir.contains('"') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("spawn: cwd={dir} holds a space and a quote, which no request line can carry"),
        ));
    }
    Ok(Cow::Owned(format!("cwd=\"{dir}\"")))
}

fn worker_cwd_arg(dir: &Path) -> Result<String, &'static str> {
    dir.to_str()
        .filter(|p| !p.contains(['\n', '\r']))
        .map(|dir| format!("cwd={dir}"))
        .ok_or("this directory's path cannot be sent")
}

fn reply_is_timeout(status_line: &str) -> bool {
    let mut it = status_line.split_whitespace();
    if it.next() != Some("OK") {
        return false;
    }
    let rest: Vec<&str> = it.collect();
    if rest == ["timeout"] {
        return true;
    }
    rest.contains(&"status=timeout")
}

fn feed_bin_exchange(
    path: &str,
    origin: TargetOrigin,
    prefix: &str,
    payload: &[u8],
    deadline: Option<std::time::Duration>,
) -> io::Result<ExitCode> {
    let path = &aterm_uds::latest::resolve(path);
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
        print_frame_answer(&read_frame_answer(&mut reader)?)
    })?;
    Ok(served.unwrap_or_else(std::convert::identity))
}

fn worker_exchange(
    path: &str,
    origin: TargetOrigin,
    head: &str,
    task: &[u8],
    deadline: Option<std::time::Duration>,
) -> io::Result<ExitCode> {
    let path = &aterm_uds::latest::resolve(path);
    let go = aterm_types::control_verbs::task_frame_go(task.len());
    let go = go.trim_end_matches('\n');
    let served = converse_served(path, origin, deadline, |stream| {
        let mut line = String::from(head);
        line.push('\n');
        if let Some(code) = send_served(&stream, line.as_bytes())? {
            return Ok(code);
        }
        let mut reader = BufReader::new(&stream);
        let mut answer = read_frame_answer(&mut reader)?;
        if answer != go {
            if answer.starts_with("ERR usage: spawn") {
                stderr_line(&answer)?;
                stderr_line(
                    "the instance does not take a worker's task (it predates `spawn agent=`): \
                     no byte of the task was sent; update it first",
                )?;
                return Ok(ExitCode::FAILURE);
            }
            return print_frame_answer(&answer);
        }
        if let Some(code) = send_served(&stream, task)? {
            return Ok(code);
        }
        while answer == go {
            answer = read_frame_answer(&mut reader)?;
        }
        print_frame_answer(&answer)
    })?;
    Ok(served.unwrap_or_else(std::convert::identity))
}

fn read_frame_answer<R: BufRead>(reader: &mut R) -> io::Result<String> {
    let mut line = String::new();
    if read_status_line(reader, &mut line)? == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "server closed the connection without responding",
        ));
    }
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

fn print_frame_answer(answer: &str) -> io::Result<ExitCode> {
    if answer.split(' ').next() == Some("OK") {
        print_stdout_line(answer)?;
        Ok(ExitCode::SUCCESS)
    } else {
        stderr_line(answer)?;
        Ok(ExitCode::FAILURE)
    }
}

fn validate_request_parts(parts: &[String]) -> io::Result<()> {
    if parts.iter().any(|p| p.contains('\n') || p.contains('\r')) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "arguments must not contain newline/carriage-return (the control socket is line-delimited)",
        ));
    }
    Ok(())
}

fn validate_request_line_len(line: &str, verb: Option<&str>) -> io::Result<()> {
    if line.len() <= MAX_REQUEST_LINE {
        return Ok(());
    }
    let words = aterm_types::control_verbs::request_line_too_long();
    let why = match verb {
        Some(verb @ ("send" | "paste")) => {
            format!("{words}; a long payload goes on stdin: {verb} --stdin")
        }
        _ => words,
    };
    Err(io::Error::new(io::ErrorKind::InvalidInput, why))
}

fn forwarded_request(parts: &[String]) -> &[String] {
    match parts.first() {
        Some(sel) if sel.starts_with('@') => &parts[1..],
        _ => parts,
    }
}

fn leading_options(args: &[String]) -> impl Iterator<Item = &str> {
    args.iter()
        .map(String::as_str)
        .take_while(|a| *a != "--" && a.contains('='))
}

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

fn add_sys_caller(parts: &mut Vec<String>, self_sid: Option<&str>) {
    if forwarded_verb(parts).as_deref() != Some("sys")
        || parts.iter().any(|p| p.starts_with("caller="))
    {
        return;
    }
    let Some(sid) = self_sid.filter(|s| {
        !s.is_empty()
            && s.len() <= 64
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    }) else {
        return;
    };
    parts.push(format!("caller={sid}"));
}

fn is_sys_instances_all(parts: &[String]) -> bool {
    forwarded_verb(parts).as_deref() == Some("sys") && parts.iter().any(|p| p == "instances=all")
}

fn run_sys_instances_all(
    parts: &[String],
    sock: Option<&str>,
    pid: Option<u32>,
) -> io::Result<ExitCode> {
    let request: Vec<&str> = parts
        .iter()
        .map(String::as_str)
        .filter(|p| *p != "instances=all" && !p.starts_with('@'))
        .collect();
    let line = request.join(" ");
    let json = request.iter().any(|t| *t == "--json" || *t == "json");
    let targets = if sock.is_some() || pid.is_some() {
        match discovery_targets(sock, pid) {
            Ok(t) => t,
            Err((report, code)) => {
                stderr_line(&report)?;
                return Ok(ExitCode::from(code));
            }
        }
    } else {
        live_instances()
    };
    if targets.is_empty() {
        stderr_line("no aterm instance is running here")?;
        return Ok(ExitCode::FAILURE);
    }
    let probes: Vec<(u32, String, Probe)> = targets
        .iter()
        .map(|(pid, sock)| (*pid, sock.clone(), probe_lines(sock, &line, *pid)))
        .collect();
    let fold = fold_sys_instances(&probes, json);
    for note in &fold.notes {
        stderr_line(note)?;
    }
    let stdout = stdout_handle();
    let mut out = StdoutSink::new(stdout.lock());
    for l in &fold.lines {
        writeln!(out, "{l}")?;
    }
    out.flush()?;
    drop(out);
    stderr_line(&format!(
        "OK sys instances={} answered={}",
        targets.len(),
        fold.answered
    ))?;
    Ok(if fold.answered > 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

#[derive(Debug, Default, PartialEq, Eq)]
struct SysFold {
    lines: Vec<String>,
    notes: Vec<String>,
    answered: usize,
}

fn fold_sys_instances(probes: &[(u32, String, Probe)], json: bool) -> SysFold {
    let mut fold = SysFold::default();
    let mut objects = Vec::new();
    for (pid, sock, probe) in probes {
        let label = if *pid > 0 {
            format!("instance pid={pid}")
        } else {
            format!("instance sock={sock}")
        };
        match probe {
            Probe::Answered(lines) => {
                fold.answered += 1;
                if json {
                    let body = lines.first().map_or("{}", String::as_str);
                    objects.push(format!(
                        "{{\"pid\":{pid},\"sock\":\"{}\",\"reply\":{body}}}",
                        aterm_codec::json::escape(sock)
                    ));
                } else {
                    fold.lines.push(label);
                    fold.lines.extend(lines.iter().cloned());
                }
            }
            Probe::Refused {
                pid_running: Some(false),
            } => {}
            other => {
                fold.notes
                    .push(format!("{label}: {}", probe_cause(*pid, other)));
                if !json {
                    fold.lines
                        .push(format!("{label} {}", sys_instance_miss(other)));
                }
            }
        }
    }
    if json {
        fold.lines.push(format!(
            "{{\"schema\":1,\"kind\":\"sys.instances\",\"instances\":[{}]}}",
            objects.join(",")
        ));
    }
    fold
}

fn sys_instance_miss(probe: &Probe) -> String {
    let why = match probe {
        Probe::Replied(line) => return line.clone(),
        Probe::Answered(_) => "answered",
        Probe::Busy(_) => "busy",
        Probe::Timeout => "timeout",
        Probe::Denied(_) => "denied",
        Probe::NoToken(..) | Probe::AuthRejected | Probe::Foreign { .. } => "auth",
        Probe::Refused { .. } | Probe::Other(_) => "unreachable",
    };
    format!("unavailable({why})")
}

fn forwarded_verb(parts: &[String]) -> Option<String> {
    match parts.first() {
        Some(sel) if sel.starts_with('@') => {
            Some(parts.get(1).cloned().unwrap_or_else(|| sel.clone()))
        }
        Some(v) => Some(v.clone()),
        None => None,
    }
}

fn help_request_rewrite(parts: &[String]) -> Option<Vec<String>> {
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

fn stream_count(tail: &str) -> Option<usize> {
    let count: usize = tail.split_whitespace().next()?.parse().ok()?;
    (count <= MAX_STREAM_LINES).then_some(count)
}

/// The note [`search_incomplete_note`] prints for the depth cap.
pub const SEARCH_DEPTH_NOTE: &str = "older history not searched (search_history_lines)";

/// What `search`'s ` incomplete` header marker means, for stderr.
pub fn search_incomplete_note(tail: &str) -> Option<String> {
    const DEPTH: &str = SEARCH_DEPTH_NOTE;
    const LIMIT: &str = "results stop at 100000 matches";
    let mut words = tail.split_whitespace();
    let count = words.next().and_then(|n| n.parse::<usize>().ok());
    if words.next() != Some("incomplete") {
        return None;
    }
    let mut note = String::new();
    for cause in words {
        let said = match cause {
            "capped" => LIMIT,
            "rewrapping" => "screen only: the history is rewrapping",
            "long-line" => "a long line was not searched to its end",
            "moving" => "the screen's rows moved during the search; search again",
            "changed" => "the terminal changed while the search ran",
            "older" => DEPTH,
            // Where the screen was, and whether its frame was whole: neither is a cause.
            word if word.starts_with("screen_first=") || word.starts_with("frame=") => continue,
            _ => "the server reported an unrecognized search completeness cause",
        };
        if !note.is_empty() {
            note.push_str("; ");
        }
        note.push_str(said);
    }
    if note.is_empty() {
        note.push_str(if count.is_some_and(|n| n >= 100_000) {
            LIMIT
        } else {
            DEPTH
        });
    }
    Some(note)
}

/// The stderr note for a line-framed reply read on an unfinished DEC 2026 frame (`frame=open`).
pub const FRAME_OPEN_NOTE: &str = "the program's frame was unfinished at the read (frame=open): \
                                   its rows may be half a redraw; `await frame` and read again";

/// [`FRAME_OPEN_NOTE`] when a header shown nowhere else carries `frame=open` after its count.
#[must_use]
pub fn frame_open_note(tail: &str) -> Option<&'static str> {
    tail.split_whitespace()
        .skip(1)
        .any(|word| word == "frame=open")
        .then_some(FRAME_OPEN_NOTE)
}

const MAX_STREAM_LINES: usize = 200_000;

fn bytes_payload(verb: &str, request: &str) -> bool {
    aterm_types::control_verbs::framing_of(verb, request)
        == aterm_types::control_verbs::Framing::Bytes
}

fn byte_count(tail: &str) -> Option<usize> {
    let n: usize = tail.split_whitespace().next()?.parse().ok()?;
    (n <= MAX_BODY_BYTES).then_some(n)
}

const MAX_BODY_BYTES: usize = 256 << 20; // 256 MiB

const MAX_LINE_BYTES: usize = aterm_types::control_verbs::MAX_CONTROL_REPLY_LINE_BYTES;

const EXCHANGE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(900);

fn connect_error(path: &str, e: &io::Error, origin: TargetOrigin) -> io::Error {
    let dead = matches!(
        e.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    );
    if dead && origin == TargetOrigin::Hosting {
        let mut msg = String::from("connect ");
        msg.push_str(path);
        msg.push_str(": ");
        msg.push_str(&e.to_string());
        return io::Error::new(e.kind(), NoHost(msg));
    }
    let (dir, live) = if dead {
        (socket_dir(), live_instances_besides(&[path]))
    } else {
        (None, Vec::new())
    };
    connect_error_naming(path, e, origin, dir.as_deref(), &live)
}

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

fn live_instances_besides(failed: &[&str]) -> Vec<(u32, String)> {
    live_instances()
        .into_iter()
        .filter(|(_, sock)| !failed.iter().any(|path| same_socket_path(sock, path)))
        .filter(|(_, sock)| CtlStream::connect(sock).is_ok())
        .collect()
}

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
        TargetOrigin::Flagless | TargetOrigin::Hosting => running_elsewhere_flags(dir, live),
    }
}

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

#[cfg(target_os = "macos")]
const LAUNCH_REMEDY: &str = "launch aterm.app (`open -a aterm`)";
#[cfg(not(target_os = "macos"))]
const LAUNCH_REMEDY: &str = "start an aterm window (`aterm --window`)";

fn not_running_hint(origin: TargetOrigin) -> String {
    match origin {
        TargetOrigin::Flagless | TargetOrigin::Hosting => format!(
            " — aterm isn't running (nothing is serving this control socket); \
             {LAUNCH_REMEDY} and retry"
        ),
        TargetOrigin::Pinned => format!(
            " — aterm isn't running (nothing is serving this control socket); \
             {LAUNCH_REMEDY}, then `aterm ctl instances` names its pid"
        ),
    }
}

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

fn oversized_reply_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "oversized reply: control line exceeds the 8 MiB length bound",
    )
}

const REFUSAL_TAIL_MAX_LINES: usize = 16;

fn drain_refusal_tail<R: Read>(reader: &mut BufReader<R>) -> io::Result<()> {
    let terminal = stderr_is_terminal();
    let stderr = io::stderr();
    let mut err = stderr.lock();
    drain_refusal_tail_into(reader, &mut err, terminal)
}

fn drain_refusal_tail_into<R: Read>(
    reader: &mut BufReader<R>,
    err: &mut impl Write,
    terminal: bool,
) -> io::Result<()> {
    for _ in 0..REFUSAL_TAIL_MAX_LINES {
        let buffered = reader.buffer();
        let Some(nl) = buffered.iter().position(|b| *b == b'\n') else {
            return Ok(());
        };
        if nl >= MAX_LINE_BYTES {
            return Ok(());
        }
        let line = buffered[..nl].to_vec();
        reader.consume(nl + 1);
        let Ok(text) = std::str::from_utf8(&line) else {
            return Ok(());
        };
        let text = text.trim_end_matches('\r');
        if terminal {
            err.write_all(aterm_codec::tty::safe(text.as_bytes()).as_bytes())?;
        } else {
            err.write_all(text.as_bytes())?;
        }
        err.write_all(b"\n")?;
    }
    Ok(())
}

fn read_bounded_line<R: BufRead>(reader: &mut R, line: &mut String) -> io::Result<usize> {
    let mut bytes = Vec::new();
    let n = reader
        .take(MAX_LINE_BYTES as u64 + 1)
        .read_until(b'\n', &mut bytes)?;
    if n > MAX_LINE_BYTES && bytes.last() != Some(&b'\n') {
        return Err(oversized_reply_error());
    }
    if n > 0 && bytes.last() != Some(&b'\n') {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "server hung up mid-reply before the line ended",
        ));
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

fn read_reply_lines<R: BufRead>(reader: &mut R, count: usize) -> io::Result<Vec<String>> {
    collect_reply_lines(count, MAX_BODY_BYTES, |line| {
        read_bounded_line(reader, line)
    })
}

fn collect_reply_lines(
    count: usize,
    max_bytes: usize,
    mut read_line: impl FnMut(&mut String) -> io::Result<usize>,
) -> io::Result<Vec<String>> {
    if count > MAX_STREAM_LINES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "reply line count exceeds the bound",
        ));
    }
    let mut bytes_left = max_bytes;
    let mut lines = Vec::with_capacity(count.min(4096));
    for _ in 0..count {
        let mut line = String::new();
        if read_line(&mut line)? == 0 || !line.ends_with('\n') {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "server hung up before the complete line-framed response",
            ));
        }
        charge_reply_bytes(&mut bytes_left, line.len())?;
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

fn charge_reply_bytes(bytes_left: &mut usize, len: usize) -> io::Result<()> {
    *bytes_left = bytes_left.checked_sub(len).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "reply body exceeds the 256 MiB aggregate bound",
        )
    })?;
    Ok(())
}

fn connect_stream(path: &str, origin: TargetOrigin) -> io::Result<(CtlStream, Option<String>)> {
    connect_then_read(path, read_token_for).map_err(|e| connect_error(path, &e, origin))
}

const ACCEPT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

const ACCEPT_PROBE: &[u8] = b"version\n";

enum Acceptance {
    Served(CtlStream),
    Refused(String),
    NotAccepted(std::time::Duration),
    Dropped,
}

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

fn pending_status_line(stream: &CtlStream) -> Option<String> {
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

fn answer_left_behind(stream: &CtlStream, token_file: Option<&Path>) -> Acceptance {
    match pending_status_line(stream) {
        Some(line) => Acceptance::Refused(auth_refusal(&line, token_file)),
        None => Acceptance::Dropped,
    }
}

fn peer_already_closed(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::InvalidInput && e.raw_os_error().is_some()
}

fn open_accepted(
    path: &str,
    origin: TargetOrigin,
    deadline: Option<std::time::Duration>,
) -> io::Result<Acceptance> {
    let (stream, token) =
        connect_then_read(path, read_token_file_at).map_err(|e| connect_error(path, &e, origin))?;
    let token = token.ok();
    let patience = match token {
        Some(_) => deadline.map(|d| d.min(ACCEPT_DEADLINE)),
        None => deadline,
    };
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
    stream.set_read_timeout(deadline)?;
    Ok(Acceptance::Served(stream))
}

fn auth_refusal(line: &str, token_file: Option<&Path>) -> String {
    match token_file {
        Some(file) if line == "ERR auth" => format!(
            "ERR auth; the server refused the token in {}; re-run",
            file.display()
        ),
        _ => line.to_string(),
    }
}

fn is_connection_refusal(line: &str) -> bool {
    line == "ERR auth" || line.starts_with("ERR control server busy")
}

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

fn converse<T>(
    path: &str,
    origin: TargetOrigin,
    deadline: Option<std::time::Duration>,
    talk: impl FnOnce(CtlStream, Option<&str>) -> io::Result<T>,
) -> io::Result<T> {
    let (stream, token) = connect_stream(path, origin)?;
    talk(stream, token.as_deref()).map_err(|e| no_reply_error(e, path, deadline))
}

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

fn stderr_line(msg: &str) -> io::Result<()> {
    stderr_prefixed("aterm-ctl: ", msg).map_err(worded)
}

fn stderr_prefixed(prefix: &str, msg: &str) -> io::Result<()> {
    let terminal = stderr_is_terminal();
    let stderr = io::stderr();
    let mut err = stderr.lock();
    err.write_all(&prefixed_line(prefix, msg, terminal))
}

fn prefixed_line(prefix: &str, msg: &str, terminal: bool) -> Vec<u8> {
    let shown = if terminal {
        aterm_codec::tty::safe(msg.as_bytes())
    } else {
        std::borrow::Cow::Borrowed(msg)
    };
    let mut line = Vec::with_capacity(prefix.len() + shown.len() + 1);
    line.extend_from_slice(prefix.as_bytes());
    line.extend_from_slice(shown.as_bytes());
    line.push(b'\n');
    line
}

#[allow(
    clippy::unnecessary_literal_unwrap,
    reason = "io::stdout is passed as a fn-item to a combinator so the Trust lane (`ship trust run`) keeps it an opaque cross-crate call instead of inlining std's startup-semantics internals it cannot lower"
)]
fn stdout_handle() -> io::Stdout {
    None::<io::Stdout>.unwrap_or_else(io::stdout)
}

fn print_stdout_line(line: &str) -> io::Result<()> {
    let stdout = stdout_handle();
    let mut out = StdoutSink::new(stdout.lock());
    out.write_all(line.as_bytes())?;
    out.write_all(b"\n")
}

fn streams_payload(verb: &str, request: &str) -> bool {
    aterm_types::control_verbs::framing_of(verb, request)
        == aterm_types::control_verbs::Framing::Lines
}

fn copy_body<R: io::Read, W: Write>(reader: &mut R, out: &mut W, nbytes: u64) -> io::Result<u64> {
    io::copy(&mut reader.take(nbytes), out)
}

fn print_payload<R: BufRead>(reader: &mut R, count: usize) -> io::Result<()> {
    let stdout = stdout_handle();
    let mut out = StdoutSink::new(io::BufWriter::new(stdout.lock()));
    let mut bytes_left = MAX_BODY_BYTES;
    for _ in 0..count {
        let mut line = String::new();
        if read_bounded_line(reader, &mut line)? == 0 || !line.ends_with('\n') {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "server hung up before the complete line-framed response",
            ));
        }
        charge_reply_bytes(&mut bytes_left, line.len())?;
        let line = line.strip_suffix('\n').unwrap_or(&line);
        let line = line.strip_suffix('\r').unwrap_or(line);
        out.write_all(line.as_bytes())?;
        out.write_all(b"\n")?;
    }
    out.flush()
}

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
        if read_bounded_line(reader, &mut line)? == 0 || !line.ends_with('\n') {
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
    let mut out = StdoutSink::new(stdout.lock());
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

fn exchange(
    path: &str,
    origin: TargetOrigin,
    request: &str,
    verb: &str,
    deadline: Option<std::time::Duration>,
    timeout_explicit: bool,
    redial: &redial::Redial,
) -> io::Result<ExitCode> {
    let path = &aterm_uds::latest::resolve(path);
    converse_served(path, origin, deadline, |stream| {
        exchange_on(
            stream,
            path,
            request,
            verb,
            deadline,
            timeout_explicit,
            redial,
        )
    })
    .map(|served| served.unwrap_or_else(std::convert::identity))
}

fn exchange_on(
    stream: CtlStream,
    path: &str,
    request: &str,
    verb: &str,
    deadline: Option<std::time::Duration>,
    timeout_explicit: bool,
    redial: &redial::Redial,
) -> io::Result<ExitCode> {
    let server = redial::server_pid(&stream);
    let listener = redial::listener_at(path);

    let started = std::time::Instant::now();

    let sent = match send_served(&stream, request.as_bytes()) {
        Ok(Some(code)) => return Ok(code),
        Ok(None) => Ok(()),
        Err(e) => Err(e),
    };

    let mut reader = BufReader::new(&stream);
    let mut status_line = String::new();
    let got = sent.and_then(|()| read_status_line(&mut reader, &mut status_line));
    let request_parts: Vec<String> = request.split_whitespace().map(String::from).collect();
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
            let left = deadline.map(|d| {
                d.saturating_sub(started.elapsed())
                    .max(std::time::Duration::from_millis(1))
            });
            exchange(
                next,
                TargetOrigin::Pinned,
                request,
                verb,
                left,
                timeout_explicit,
                &redial::Redial::none(),
            )
            .map_err(worded)
        };
        return match host_gone {
            Some((pid, sock)) => {
                let host = redial.host(&sock, redial::target_sid(request).map(|sid| sid.as_str()));
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
    if let Some((pid, sock)) = host_gone {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!(
                "server closed the connection without responding: the aterm (pid {pid}) \
                 hosting the session went away under the forward to {sock}"
            ),
        ));
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
        stderr_line(status_line)?;
        drain_refusal_tail(&mut reader)?;
        return Ok(ExitCode::FAILURE);
    }

    let timed_out = reply_is_timeout(status_line);

    if verb == "subscribe" {
        stderr_line(status_line)?;
        let watch = deadline.filter(|_| timeout_explicit);
        let stdout = stdout_handle();
        let mut out = StdoutSink::new(stdout.lock());
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

    if bytes_payload(verb, request) {
        let nbytes = match byte_count(tail) {
            Some(n) => n,
            None => return Err(malformed_header_error(status_line)),
        };
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
        let mut out = StdoutSink::new(stdout.lock());
        let copied = copy_body(&mut reader, &mut out, nbytes as u64)?;
        if copied < nbytes as u64 {
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
        acknowledge_artifact_reply(&stream, &mut reader, verb, request)?;
        return Ok(ExitCode::SUCCESS);
    }

    if aterm_types::control_verbs::artifact_reply_requires_ack(verb, request) {
        match receive_guarded_artifact_reply(
            &stream,
            &mut reader,
            verb,
            request,
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

    if streams_payload(verb, request) {
        let count: usize = match stream_count(tail) {
            Some(count) => count,
            None => return Err(malformed_header_error(status_line)),
        };
        if let Some(note) = search_incomplete_note(tail) {
            stderr_line(&note)?;
        }
        let turn_verdict = tail.split_whitespace().nth(1) == Some("turn");
        if turn_verdict {
            let verdict: Vec<&str> = tail.split_whitespace().skip(1).collect();
            stderr_line(&verdict.join(" "))?;
        }
        if drift::is_drift_request(verb, request) {
            // and refuse by name rather than read a byte body as lines.
            if !tail.split_whitespace().any(|t| t.starts_with("verdict=")) {
                if let (1, Some(n)) = (tail.split_whitespace().count(), byte_count(tail)) {
                    io::copy(&mut (&mut reader).take(n as u64), &mut io::sink())?;
                }
                return Err(io::Error::other(
                    "the server predates `cast drift` (it answered the bare cast, not a \
                     report); save its `cast` and `text` and run `aterm ctl cast drift --file \
                     <cast> --screen <text>`",
                ));
            }
            print_stdout_line(status_line)?;
        } else if verb == "inbox"
            || verb == "offscreen"
            || verb == "search"
            || verb == "resizes"
            || verb == "result"
            || verb == "log"
            || verb == "brief"
            || verb == "sys"
            || (verb == "history" && tail.split_whitespace().nth(1).is_some())
        {
            stderr_line(status_line)?;
        } else if count == 0 {
            let mut msg = String::from(status_line);
            msg.push_str(" (");
            msg.push_str(verb);
            msg.push_str(": no results)");
            stderr_line(&msg)?;
        } else if !turn_verdict && let Some(note) = frame_open_note(tail) {
            stderr_line(note)?;
        }
        match print_payload(&mut reader, count) {
            Err(e) if turn_verdict && !is_stdout_closed(&e) => {
                stderr_line(&turn_rows_cut_line(&e, &request_parts))?;
                return Ok(ExitCode::from(redial::EXIT_REPLACED));
            }
            rows => rows?,
        }
    } else {
        print_stdout_line(status_line)?;
    }

    acknowledge_artifact_reply(&stream, &mut reader, verb, request)?;
    if timed_out {
        Ok(ExitCode::from(EXIT_TIMEOUT))
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

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
            stderr_line(&redial::gone_request_note(
                redial::gone_note(server, waited, by_deadline, redial.unfollowed()),
                request_parts,
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

fn turn_rows_cut_line(error: &io::Error, request_parts: &[String]) -> String {
    format!(
        "the turn's screen rows were cut after its verdict ({error}); {} (exit {})",
        redial::not_asked_again_note(request_parts),
        redial::EXIT_REPLACED
    )
}

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
