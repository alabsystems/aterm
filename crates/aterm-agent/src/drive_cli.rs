// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-drive` — the AI-friendly "sugar" CLI over the core `await`/`send`/`key`/
//! `text` primitives. It teaches itself through `--help` and emits actionable
//! errors, so an AI agent builds correct intuition for the kernel without docs.
//! All real work uses the core control verbs: a configured prompt reuses one
//! [`RelayClient`] connection, while discovery and the other commands retain the
//! std-only `aterm-ctl` client.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use crate::{ControlClient, CtlClient, DRIVE_HELP, RelayClient, SelfGovernor, Turn};

/// Resolve the `aterm-ctl` binary: `$ATERM_CTL`, then a sibling of this binary
/// (the cargo/install layout), then bare `aterm-ctl` on `PATH`.
fn resolve_ctl() -> PathBuf {
    if let Ok(p) = std::env::var("ATERM_CTL") {
        return PathBuf::from(p);
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sib = dir.join("aterm-ctl");
        if sib.is_file() {
            return sib;
        }
    }
    PathBuf::from("aterm-ctl")
}

#[derive(Debug)]
struct Opts {
    socket: Option<String>,
    /// A saved REMOTE connection name to `dial` via the local host (`--dial`).
    dial: Option<String>,
    idle_ms: u64,
    timeout_ms: u64,
    /// The prompt-ready regex for the best-effort settle confirm. `None` = the
    /// built-in default; `Some("")` = idle-only (skip the confirm entirely).
    ready: Option<String>,
    cmd: Vec<String>,
}

/// Resolve the prompt-ready pattern: `--ready` beats `$ATERM_DRIVE_READY`, which
/// beats the built-in default.
///
/// The default ([`crate::claude_prompt_ready_pattern`]) matches a Claude input
/// caret, which is only correct when the driven session IS Claude. Driving any
/// other REPL/agent needs its own prompt, and driving a plain shell wants no
/// pattern at all — so this is a knob, not a constant. It stays a BEST-EFFORT
/// extra settle either way: a non-matching pattern costs a bounded wait, never
/// a failed turn.
fn resolve_ready(flag: Option<String>) -> String {
    flag.or_else(|| std::env::var("ATERM_DRIVE_READY").ok())
        .unwrap_or_else(|| crate::claude_prompt_ready_pattern().to_string())
}

fn parse(argv: Vec<std::ffi::OsString>) -> Result<Opts, String> {
    let mut socket = None;
    let mut dial = None;
    let mut idle_ms = 600u64;
    let mut timeout_ms = 180_000u64;
    let mut ready = None;
    let mut cmd = Vec::new();
    let mut it = argv.into_iter().map(|a| a.to_string_lossy().into_owned());
    while let Some(a) = it.next() {
        match a.as_str() {
            "--socket" | "--sock" => {
                socket = Some(it.next().ok_or("--socket needs a PATH")?);
            }
            "--dial" | "--remote" => {
                dial = Some(it.next().ok_or("--dial needs a saved connection NAME")?);
            }
            "--idle" => {
                idle_ms = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("--idle needs a millisecond integer")?;
            }
            "--timeout" => {
                timeout_ms = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("--timeout needs a millisecond integer")?;
            }
            // An EMPTY value is meaningful (idle-only), so this takes the next
            // argument verbatim rather than treating "" as "unset".
            "--ready" => {
                ready = Some(it.next().ok_or(
                    "--ready needs a REGEX (use '' for idle-only, no prompt-ready confirm)",
                )?);
            }
            "-h" | "--help" | "help" => {
                cmd = vec!["help".to_string()];
                break;
            }
            _ => {
                cmd.push(a);
                cmd.extend(it.by_ref());
                break;
            }
        }
    }
    Ok(Opts {
        socket,
        dial,
        idle_ms,
        timeout_ms,
        ready,
        cmd,
    })
}

/// Resolve an explicitly configured LOCAL control endpoint. `Ok(None)` means
/// neither `--socket` nor `$ATERM_CONTROL_SOCK` selected one, so a local prompt
/// must retain the existing `aterm-ctl` discovery path. Once a socket IS selected,
/// a missing token is an error rather than permission to silently drive some
/// other discovered instance.
fn resolve_configured_local_endpoint_with(
    flag_socket: Option<String>,
    env_socket: Option<String>,
    env_token: Option<String>,
    read_token: impl FnOnce(&std::path::Path) -> Option<String>,
) -> Result<Option<(String, String)>, String> {
    let Some(sock) = flag_socket
        .or(env_socket)
        .filter(|s| !s.is_empty() && s != "0" && s != "off")
    else {
        return Ok(None);
    };
    let token = env_token
        .filter(|s| !s.is_empty())
        .or_else(|| {
            // The SHARED convention, not a hand-rolled one: an instance socket
            // (`aterm-<pid>.sock`) pairs with `aterm-<pid>.token`, and an explicit
            // `$ATERM_CONTROL_SOCK` path with a token named after that socket,
            // resolved in the socket's own directory and through the `latest`
            // alias. This used to derive `<stem>.token`, which the server never
            // wrote, so `--dial` silently failed to authenticate where `aterm ctl`
            // worked; deriving it here AGAIN is how that came back.
            let tok_path = aterm_uds::latest::token_path_for_sock(&sock)?;
            read_token(&tok_path).map(|s| s.trim().to_string())
        })
        .filter(|s| !s.is_empty())
        .ok_or(
            "could not resolve the LOCAL control token: set ATERM_CONTROL_TOKEN, or ensure the \
             token file beside the socket is readable (`aterm-<pid>.token` for an instance \
             socket, else the token named after the socket itself)",
        )?;
    Ok(Some((sock, token)))
}

/// Resolve the LOCAL host's control socket + capability token when a socket was
/// explicitly selected by `--socket` or `$ATERM_CONTROL_SOCK`. The token comes
/// from `$ATERM_CONTROL_TOKEN`, else the sibling token file located by the SHARED
/// convention (`aterm_uds::latest::token_path_for_sock`) — `aterm-<pid>.token`
/// for an instance socket, and a token named after any explicit socket. The
/// legacy shared `aterm.token` is accepted only when the per-socket file is absent.
fn resolve_configured_local_endpoint(opts: &Opts) -> Result<Option<(String, String)>, String> {
    resolve_configured_local_endpoint_with(
        opts.socket.clone(),
        std::env::var("ATERM_CONTROL_SOCK").ok(),
        std::env::var("ATERM_CONTROL_TOKEN").ok(),
        |path| aterm_ctl::read_control_token_file(path).ok(),
    )
}

/// `--dial` always needs an explicit/env LOCAL endpoint; it has no discovery
/// fallback because the local host is the authority that resolves the saved name.
fn resolve_local_endpoint(opts: &Opts) -> Result<(String, String), String> {
    resolve_configured_local_endpoint(opts)?.ok_or_else(|| {
        "--dial needs the LOCAL host socket: pass --socket <PATH> or set ATERM_CONTROL_SOCK"
            .to_string()
    })
}

#[derive(Debug, PartialEq, Eq)]
enum LocalPromptRoute {
    Persistent { socket: String, token: String },
    ShellDiscovery,
}

/// A missing/unreadable token keeps the pre-existing shell client in charge: it
/// has its own discovery and classified authentication diagnostics. Only a fully
/// resolved endpoint opts into the persistent protocol path.
fn local_prompt_route(endpoint: Result<Option<(String, String)>, String>) -> LocalPromptRoute {
    match endpoint {
        Ok(Some((socket, token))) => LocalPromptRoute::Persistent { socket, token },
        Ok(None) | Err(_) => LocalPromptRoute::ShellDiscovery,
    }
}

fn run_prompt_turn<C: ControlClient>(
    opts: &Opts,
    client: &mut C,
    text: &str,
) -> Result<String, String>
where
    C::Error: std::fmt::Display,
{
    let mut gov = SelfGovernor::disabled(64, 8, 5_000_000);
    gov.enable_self_write();
    let turn = Turn {
        idle: Duration::from_millis(opts.idle_ms),
        timeout: Duration::from_millis(opts.timeout_ms),
        ready_pattern: resolve_ready(opts.ready.clone()),
    };
    turn.run(client, &mut gov, text.as_bytes())
        .map_err(|e| e.to_string())
}

/// Run the configured-endpoint prompt fast path when `route` selected it.
/// Keeping route resolution outside this seam makes the protocol test
/// independent of ambient process environment while production still uses the
/// exact same connection/Turn body.
fn run_persistent_local_prompt(
    opts: &Opts,
    route: LocalPromptRoute,
) -> Option<Result<String, String>> {
    if opts.cmd.first().map(String::as_str) != Some("prompt") {
        return None;
    }
    let LocalPromptRoute::Persistent {
        socket: sock,
        token,
    } = route
    else {
        return None;
    };
    let text = opts.cmd[1..].join(" ");
    if text.is_empty() {
        return Some(Err(
            "prompt needs text, e.g. `aterm-drive prompt 'say hi'`".to_string()
        ));
    }
    Some(
        RelayClient::connect_local(&sock, &token)
            .map_err(|e| {
                format!(
                    "cannot reach the configured target aterm at control socket '{sock}' ({e}).\n  \
                     • Is that aterm still running?\n  \
                     • Is the socket/token pair current? (--socket / ATERM_CONTROL_SOCK / \
                     ATERM_CONTROL_TOKEN)"
                )
            })
            .and_then(|mut client| run_prompt_turn(opts, &mut client, &text)),
    )
}

/// The whole drive CLI as a callable: `argv[1..]` in, exit code out. Served
/// in-process by the ONE `aterm` binary (`aterm drive …` / argv0 alias) and
/// by the thin standalone bin.
pub fn main_entry(argv: Vec<std::ffi::OsString>) -> ExitCode {
    let opts = match parse(argv) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("aterm-drive: {e}\n\nRun `aterm-drive --help` for usage.");
            return ExitCode::FAILURE;
        }
    };
    match run(&opts) {
        Ok(out) => {
            print!("{out}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("aterm-drive: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(opts: &Opts) -> Result<String, String> {
    let verb = opts.cmd.first().map(String::as_str).unwrap_or("help");
    if verb == "help" {
        return Ok(format!("{DRIVE_HELP}\n"));
    }

    // `--dial <name>`: drive a REMOTE aterm over the local host's `dial` relay. A
    // persistent `RelayClient` speaks the SAME verbs as the local path, so the Turn
    // is byte-identical — predicates run on the authoritative remote host. Supports
    // the `prompt` drive loop (the remote use case); other verbs stay local.
    if let Some(name) = &opts.dial {
        if verb != "prompt" {
            return Err(format!(
                "--dial supports the `prompt` command (the drive loop); got `{verb}`. \
                 Run local read/await/shot without --dial."
            ));
        }
        let text = opts.cmd[1..].join(" ");
        if text.is_empty() {
            return Err(
                "prompt needs text, e.g. `aterm-drive --dial work prompt 'say hi'`".to_string(),
            );
        }
        let (sock, token) = resolve_local_endpoint(opts)?;
        let mut client = RelayClient::dial_via_local(&sock, &token, name).map_err(|e| {
            format!(
                "could not dial remote connection '{name}' via the local host ({e}).\n  \
                 • Is '{name}' a connection saved on THIS host? (the local aterm resolves it)\n  \
                 • Is the LOCAL socket/token right? (--socket / ATERM_CONTROL_SOCK / ATERM_CONTROL_TOKEN)"
            )
        })?;
        return run_prompt_turn(opts, &mut client, &text);
    }

    // A configured local endpoint already gives us everything the control CLI
    // would rediscover on every verb. Keep one authenticated connection for the
    // entire prompt turn (send/key/await/text), and let the connect itself replace
    // the subprocess `cursor` preflight. Other commands deliberately retain the
    // shell client because this fast path is scoped to the Turn flow.
    if let Some(result) = run_persistent_local_prompt(
        opts,
        local_prompt_route(resolve_configured_local_endpoint(opts)),
    ) {
        return result;
    }

    let ctl = resolve_ctl();
    let mut client = CtlClient::new(ctl.clone(), opts.socket.clone());

    // A friendly preflight: if we cannot even read the screen, explain why before
    // attempting to drive — this is the error an AI hits most and learns from.
    if let Err(e) = client.run(&["cursor"]) {
        return Err(format!(
            "cannot reach a target aterm over the control socket ({e}).\n  \
             • Is a host aterm running? Launch one headless:\n      \
             aterm-gui --headless &\n  \
             • Point at its socket (it prints 'control socket listening at <PATH>'):\n      \
             export ATERM_CONTROL_SOCK=<PATH>   (or pass --socket <PATH>)\n  \
             • aterm-ctl resolved to: {}",
            ctl.display()
        ));
    }

    match verb {
        "prompt" => {
            let text = opts.cmd[1..].join(" ");
            if text.is_empty() {
                return Err("prompt needs text, e.g. `aterm-drive prompt 'say hi'`".to_string());
            }
            run_prompt_turn(opts, &mut client, &text)
        }
        "read" => client.run(&["text"]),
        "shot" => {
            let path = opts.cmd.get(1).cloned();
            let mut args = vec!["image"];
            if let Some(p) = &path {
                args.push(p);
            }
            client.run(&args)
        }
        "await" => {
            if opts.cmd.len() < 2 {
                return Err(
                    "await needs a condition: idle <ms> | match <regex> | seq | block\n  \
                     e.g. `aterm-drive await match 'BUILD SUCCESSFUL'`"
                        .to_string(),
                );
            }
            // Pass the condition straight through to the core verb, but supply the
            // tool's --timeout so a bare `await idle 500` still has a sane bound.
            let mut args: Vec<String> = opts.cmd[1..].to_vec();
            if !args.iter().any(|a| a == "timeout") {
                args.push("timeout".to_string());
                args.push(opts.timeout_ms.to_string());
            }
            let mut a: Vec<&str> = vec!["await"];
            a.extend(args.iter().map(String::as_str));
            client.run(&a)
        }
        other => Err(format!(
            "unknown command '{other}'. Valid: prompt | read | await | shot | help.\n  \
             Run `aterm-drive --help` for the full guide."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt_opts(socket: String) -> Opts {
        Opts {
            socket: Some(socket),
            dial: None,
            idle_ms: 7,
            timeout_ms: 1_000,
            ready: Some(String::new()),
            cmd: vec!["prompt".to_string(), "hello".to_string()],
        }
    }

    fn unique_endpoint() -> (std::path::PathBuf, std::path::PathBuf) {
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT: AtomicU64 = AtomicU64::new(0);
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "aterm-agent-fast-path-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir(&dir).expect("create unique endpoint directory");
        (dir.join("control.sock"), dir)
    }

    /// The prompt-ready pattern must be a KNOB, not a constant: the default only
    /// matches a Claude input caret, so driving any other REPL — or a plain
    /// shell, which wants no confirm at all — needs an override.
    #[test]
    fn ready_pattern_precedence_flag_beats_env_beats_default() {
        let dflt = crate::claude_prompt_ready_pattern();

        // Flag wins outright (no env read needed for this branch).
        assert_eq!(resolve_ready(Some(r"^\$ ".to_string())), r"^\$ ");

        // An EMPTY flag is meaningful — idle-only — and must NOT fall through
        // to the default. This is the branch a naive `filter(|s| !s.is_empty())`
        // would silently break.
        assert_eq!(resolve_ready(Some(String::new())), "");

        // No flag, no env -> the built-in default.
        // (Guarded: another test in this process could have set the var.)
        if std::env::var("ATERM_DRIVE_READY").is_err() {
            assert_eq!(resolve_ready(None), dflt);
        }
    }

    /// `--ready` takes its value verbatim, including an empty string, and a
    /// missing value is a clear usage error rather than swallowing the command.
    #[test]
    fn ready_flag_parses_and_requires_a_value() {
        let os = |v: &[&str]| -> Vec<std::ffi::OsString> {
            v.iter().map(std::ffi::OsString::from).collect()
        };

        let o = parse(os(&["--ready", r"^> ", "prompt", "hi"])).expect("parses");
        assert_eq!(o.ready.as_deref(), Some(r"^> "));
        assert_eq!(o.cmd, vec!["prompt".to_string(), "hi".to_string()]);

        let empty = parse(os(&["--ready", "", "read"])).expect("empty value is legal");
        assert_eq!(empty.ready.as_deref(), Some(""), "'' means idle-only");

        let err = parse(os(&["--ready"])).expect_err("a missing value is an error");
        assert!(err.contains("--ready needs a REGEX"), "actionable: {err}");
    }

    /// Regression: the drive CLI must not re-derive the token path itself. The
    /// shared helper is the single source of truth for the convention.
    ///
    /// This used to pin the RESULT (`…/aterm.token`) rather than the agreement,
    /// which made it a second statement of the convention — and it went red the
    /// day the convention moved: an explicit socket now pairs with a token named
    /// after ITSELF, because one shared `aterm.token` per directory meant two
    /// private instances there overwrote each other's credential (F9). The guard
    /// that actually holds is the one asserted here — whatever the rule says, the
    /// drive CLI says the same thing, because it asks the same function.
    #[test]
    fn token_path_uses_the_shared_convention_not_a_stem_swap() {
        for sock in ["/tmp/run/c.sock", "/tmp/run/aterm-42.sock", "/tmp/run/ctl"] {
            let resolved = aterm_uds::latest::token_path_for_sock(sock).expect("resolves");
            // Named through the SAME mirror the CLI itself calls, so this crate
            // gains no dependency to state the rule twice.
            let expected = std::path::Path::new(sock)
                .parent()
                .expect("a directory")
                .join(aterm_uds::latest::token_name_for_sock(
                    std::path::Path::new(sock)
                        .file_name()
                        .expect("a file name")
                        .to_string_lossy()
                        .as_ref(),
                ));
            assert_eq!(
                resolved, expected,
                "{sock}: the drive CLI must name the token the shared rule names"
            );
        }
        // And the hand-rolled stem swap this test was born to forbid stays gone:
        // `c.sock` pairs with `c.sock.token`, never `c.token`.
        let p = aterm_uds::latest::token_path_for_sock("/tmp/run/c.sock").expect("resolves");
        assert!(
            !p.to_string_lossy().ends_with("/c.token"),
            "the old hand-rolled <stem>.token derivation is gone, got {p:?}"
        );
    }

    #[test]
    fn configured_endpoint_uses_one_persistent_route_and_flag_precedence() {
        let endpoint = resolve_configured_local_endpoint_with(
            Some("/run/flag.sock".to_string()),
            Some("/run/env.sock".to_string()),
            Some("capability".to_string()),
            |_| panic!("an env token must avoid a token-file read"),
        )
        .expect("configured endpoint resolves");

        assert_eq!(
            local_prompt_route(Ok(endpoint)),
            LocalPromptRoute::Persistent {
                socket: "/run/flag.sock".to_string(),
                token: "capability".to_string(),
            },
            "the prompt fast path receives the exact explicit endpoint and token"
        );
    }

    #[test]
    fn configured_prompt_uses_one_connection_and_no_cursor_preflight() {
        use std::io::{BufRead, BufReader, Write};

        let (socket, dir) = unique_endpoint();
        let socket_text = socket.to_string_lossy().into_owned();
        let token_path = aterm_uds::latest::token_path_for_sock(&socket_text).expect("token path");
        std::fs::write(&token_path, "capability\n").expect("write token");
        let listener = aterm_uds::CtlListener::bind(&socket).expect("bind endpoint");

        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("one persistent connection");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("read timeout");
            stream
                .set_write_timeout(Some(Duration::from_secs(2)))
                .expect("write timeout");
            let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
            let mut writer = stream;
            let mut requests = Vec::new();

            for reply in [b"".as_slice(), b"OK\n", b"OK\n", b"OK idle 1\n"] {
                let mut request = String::new();
                reader.read_line(&mut request).expect("request line");
                requests.push(request.trim_end().to_string());
                writer.write_all(reply).expect("reply");
            }
            let mut request = String::new();
            reader.read_line(&mut request).expect("text request");
            requests.push(request.trim_end().to_string());
            writer
                .write_all(b"OK 2\nsettled\n> \n")
                .expect("text reply");
            requests
        });

        let out = run_persistent_local_prompt(
            &prompt_opts(socket_text.clone()),
            LocalPromptRoute::Persistent {
                socket: socket_text,
                token: "capability".to_string(),
            },
        )
        .expect("persistent route selected")
        .expect("persistent prompt turn");
        let requests = server.join().expect("server completes");

        assert_eq!(out, "settled\n> \n");
        assert_eq!(
            requests,
            [
                "AUTH capability",
                "send hello",
                "key enter",
                "await idle 7 timeout 1000",
                "text",
            ],
            "one connection carries the whole Turn and never sends a cursor preflight"
        );

        std::fs::remove_file(&socket).expect("remove socket");
        std::fs::remove_file(&token_path).expect("remove token");
        std::fs::remove_dir(dir).expect("remove endpoint directory");
    }

    #[test]
    fn absent_or_disabled_endpoint_preserves_shell_discovery_fallback() {
        for flag in [
            None,
            Some(String::new()),
            Some("0".to_string()),
            Some("off".to_string()),
        ] {
            let endpoint = resolve_configured_local_endpoint_with(flag, None, None, |_| {
                panic!("fallback must not probe a token file")
            })
            .expect("no configured endpoint is not an error");
            assert_eq!(
                local_prompt_route(Ok(endpoint)),
                LocalPromptRoute::ShellDiscovery,
                "None selects the existing shell discovery path"
            );
        }
    }

    #[test]
    fn env_endpoint_resolves_the_per_socket_token_file() {
        let endpoint = resolve_configured_local_endpoint_with(
            None,
            Some("/tmp/run/custom.sock".to_string()),
            None,
            |path| {
                assert!(
                    path.ends_with("custom.sock.token"),
                    "per-socket token path: {path:?}"
                );
                Some("  file-capability\n".to_string())
            },
        )
        .expect("env endpoint resolves");

        assert_eq!(
            endpoint,
            Some((
                "/tmp/run/custom.sock".to_string(),
                "file-capability".to_string()
            ))
        );
    }

    #[test]
    fn unresolved_token_preserves_shell_diagnostics_while_dial_still_gets_the_error() {
        let err = resolve_configured_local_endpoint_with(
            None,
            Some("/tmp/run/custom.sock".to_string()),
            None,
            |_| None,
        )
        .expect_err("the endpoint itself cannot resolve without its token");

        assert!(
            err.contains("could not resolve the LOCAL control token"),
            "{err}"
        );
        assert_eq!(
            local_prompt_route(Err(err)),
            LocalPromptRoute::ShellDiscovery,
            "local prompt retains aterm-ctl's existing classified error path"
        );
    }

    #[test]
    fn resolved_but_unreachable_endpoint_surfaces_connect_error_without_retargeting() {
        let (socket, dir) = unique_endpoint();
        let socket_text = socket.to_string_lossy().into_owned();
        let token_path = aterm_uds::latest::token_path_for_sock(&socket_text).expect("token path");
        std::fs::write(&token_path, "capability\n").expect("write token");

        let err = run(&prompt_opts(socket_text))
            .expect_err("a resolved endpoint must not silently retarget");
        assert!(
            err.contains("cannot reach the configured target aterm"),
            "actionable endpoint error: {err}"
        );

        std::fs::remove_file(token_path).expect("remove token");
        std::fs::remove_dir(dir).expect("remove endpoint directory");
    }
}
