// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `--record` and `--replay` tape setup and teardown for CLI commands.

use std::sync::Arc;

use astream_effects::EffectRecord;

use super::args::{Global, Invocation, tape_tokens};
use super::verbs::{self, Class};
use super::{Ctx, out};
use crate::error::{Error, Replay, Result, Sub};
use crate::fx::tape::{self, TapeEndpoints, TapeHeader, TapeMode};
use crate::fx::{Fx, read_tape};
use crate::identity::MapEnv;
use crate::redact::Redactor;
use crate::retry::RetryPolicy;
use crate::wire::Endpoints;
use crate::wire::http::HttpTransport;

/// Strips `--record` and `--replay` flags before `--`, preserving all other arguments in order.
pub(crate) fn without_tape_flags(args: &[String]) -> Vec<String> {
    let drop = tape_tokens(args);
    args.iter()
        .enumerate()
        .filter(|(i, _)| !drop.contains(i))
        .map(|(_, arg)| arg.clone())
        .collect()
}

/// Reads the tape named by `--replay PATH` once (`read_tape`).
///
/// # Errors
///
/// `read_tape`'s; usage, next `agh help`, when no `--replay` was given.
pub(crate) fn read_taped(global: &Global) -> Result<(TapeHeader, Vec<EffectRecord>)> {
    let Some(path) = &global.replay else {
        return Err(Error::usage("--replay names no tape").with_next("agh help"));
    };
    read_tape(path)
}

/// Whether a recording of `inv` is credentialed (plan §14, body 4): a Write or Destructive
/// verb, `release download`, an `api` whose parsed method is POST, PATCH, PUT or DELETE, or
/// any verb under `--as`. Every other recording is anonymous, as in M1.
fn credentialed(inv: &Invocation) -> bool {
    let path = inv.path.as_slice();
    let class = verbs::find(path).map(|verb| verb.class);
    inv.global.as_profile.is_some()
        || matches!(class, Some(Class::Write | Class::Destructive))
        || path == ["release", "download"]
        || (path == ["api"] && {
            let method = api_method(inv);
            method.eq_ignore_ascii_case("POST")
                || method.eq_ignore_ascii_case("PATCH")
                || method.eq_ignore_ascii_case("PUT")
                || method.eq_ignore_ascii_case("DELETE")
        })
}

/// The method `cmd_api` sends: `--method` (which `-X` sets), `GET` when absent.
fn api_method(inv: &Invocation) -> &str {
    inv.flag("--method").unwrap_or("GET")
}

/// A one-shot write recorded against github.com (plan §14, body 5): `repo create`, `release create`,
/// `release ensure`, `ref create`, `ref update-atomic`, or an `api` POST,
/// `--dry-run` included.
fn one_shot(inv: &Invocation) -> bool {
    let path = inv.path.as_slice();
    matches!(
        path,
        ["repo", "create"]
            | ["repo", "push"]
            | ["release", "create"]
            | ["release", "ensure"]
            | ["ref", "create"]
            | ["ref", "update-atomic"]
    ) || (path == ["api"] && api_method(inv).eq_ignore_ascii_case("POST"))
}

/// Configures `ctx` for `--record` or `--replay` before the verb runs.
///
/// A recording is credentialed (`mode credentialed`) when `credentialed` says so: its sink
/// passes every record through `ctx.redactor`, whose clone shares the credentials the run
/// learns later, and `ctx.env` and `ctx.dirs` are kept, so the router loads agh.toml and the
/// tokens, with vault adoption settled before the tape opens (`settle_adoption`). An anonymous
/// recording, and every replay, runs on an empty environment with no directories, as in M1: a
/// replay takes each decision from the tape's route door and loads no token.
///
/// # Errors
///
/// Returns an error if the tape cannot be read/created or `argv` does not match or redacts:
/// under `--replay`, `Replay::Argv` unless the header's `argv` equals `argv`, each argument
/// through `Redactor::shapes` as the tape holds it; under `--record` alone, a usage error for
/// an argument the shapes would change, or a command line whose taped `argv` line they would
/// change once its arguments are joined (`tape::head_redacts`); a usage error, next
/// `record it against a loopback fake, or run it without --record`, for a one-shot write
/// recorded against github.com; for a credentialed recording, what its first `Ctx::github`
/// meets live (`settle_adoption`). Each check comes before any file is created.
pub(crate) fn begin_with(
    ctx: &mut Ctx,
    inv: &Invocation,
    argv: &[String],
    taped: Option<(TapeHeader, Vec<EffectRecord>)>,
) -> Result<()> {
    let global = &inv.global;
    let fx = match (&global.replay, &global.record) {
        (Some(replay), record) => {
            let (header, records) = match taped {
                Some(taped) => taped,
                None => read_tape(replay)?,
            };
            if header.argv.len() != argv.len()
                || header
                    .argv
                    .iter()
                    .zip(argv)
                    .any(|(expected, arg)| *expected != Redactor::shapes(arg))
            {
                return Err(Error::replay(
                    Replay::Argv,
                    format!(
                        "this command line is not the tape's, which ran: {}",
                        header.argv.join(" ")
                    ),
                ));
            }
            if header.endpoints == TapeEndpoints::GithubCom {
                ctx.endpoints = Endpoints::github_com();
            }
            let fx = match record {
                Some(record) => Fx::rerecord_from(&header, records, record)?,
                None => Fx::replay_from(&header, records),
            };
            fx.with_source(replay)
        }
        (None, Some(record)) => {
            if let Some(n) = argv.iter().position(|arg| Redactor::shapes(arg) != *arg) {
                return Err(Error::usage(format!(
                    "argument {} holds a token shape: a tape of this command line could not \
                     replay",
                    n + 1
                ))
                .with_next("record a command line that names no token"));
            }
            let header = TapeHeader::for_run(argv, &ctx.endpoints);
            if tape::head_redacts(&header) {
                return Err(Error::usage(
                    "the command line holds a token shape across its arguments: a tape of it \
                     could not replay",
                )
                .with_next("record a command line that names no token"));
            }
            if header.endpoints == TapeEndpoints::GithubCom && one_shot(inv) {
                return Err(Error::usage(format!(
                    "{} cannot be recorded against github.com: a one-shot write is journaled \
                     on the tape, not in agh's write journal",
                    inv.path.join(" ")
                ))
                .with_next("record it against a loopback fake, or run it without --record"));
            }
            if ctx.transport.is_none() {
                ctx.transport = Some(Arc::new(HttpTransport::new(ctx.env.clone())));
            }
            if credentialed(inv) {
                let header = TapeHeader {
                    mode: TapeMode::Credentialed,
                    ..header
                };
                settle_adoption(ctx, inv)?;
                let fx = Fx::record_with(record, &header, ctx.redactor.clone())?;
                build_over(ctx, fx.with_endpoints(&ctx.endpoints));
                return Ok(());
            }
            Fx::record(record, &header)?
        }
        (None, None) => return Ok(()),
    };
    ctx.env = Arc::new(MapEnv::new());
    ctx.dirs = None;
    ctx.fx = Arc::new(fx.with_endpoints(&ctx.endpoints));
    ctx.retry = RetryPolicy::STANDARD;
    Ok(())
}

/// The backstop `inv`'s client is built with live: `--timeout`, which `run_verb` sets before
/// the verb runs, except under `release wait`, which takes the timeout as its whole wait
/// before it builds its client (`cmd_wait::release_wait`), so its client has none.
fn backstop(inv: &Invocation) -> Option<std::time::Duration> {
    (inv.path != ["release", "wait"])
        .then_some(inv.global.timeout)
        .flatten()
}

/// A credentialed recording's vault adoption, settled before its tape exists. The replay has no
/// directories and adopts nothing, so nothing adoption sends may land on the tape: a vault
/// token whose login the record lacks (first use, or after a rotation) is learned here, over
/// the run's own seam, as the live run would (`Ctx::github`, its note lines on stderr, under
/// the run's `--anonymous` and backstop).
///
/// # Errors
///
/// What the same command line meets live at its first `Ctx::github`, its next made the vault's
/// as `run_verb` makes it: an invalid agh.toml, or adoption's refusal (a vault token that now
/// speaks as another login, an untrusted record). No tape file is made for it.
fn settle_adoption(ctx: &mut Ctx, inv: &Invocation) -> Result<()> {
    ctx.anonymous = inv.global.anonymous;
    ctx.timeout = backstop(inv);
    let settled = if inv.path == ["repo", "create"] && inv.has("--dry-run") {
        ctx.github_recorded()
    } else {
        ctx.github_noting(true)
    };
    ctx.github = None;
    if let Err(mut e) = settled {
        ctx.vault_next(&mut e);
        return Err(out::with_verb_help(e, &inv.verb_name()));
    }
    ctx.adoption = None;
    Ok(())
}

/// The client the verb runs on, built over `recording` from the record `settle_adoption` left
/// (`vault::view`, as under `--anonymous`), so it asks GitHub nothing more, with the backstop
/// `settle_adoption` set: a token whose login could not be learned (GitHub did not answer) does
/// not route, as the settling call found. The settling call refused nothing, and this build
/// reads the same agh.toml and record, so it refuses nothing either; if a file changed between
/// the two, no client is kept and the verb's own `Ctx::github` builds one and reports what it
/// meets.
fn build_over(ctx: &mut Ctx, recording: Fx) {
    ctx.fx = Arc::new(recording);
    ctx.retry = RetryPolicy::STANDARD;
    ctx.github = None;
    ctx.adoption = None;
    let anonymous = std::mem::replace(&mut ctx.anonymous, true);
    let _ = ctx.github_noting(false);
    ctx.anonymous = anonymous;
}

/// Finishes the taped run (`ctx.fx.finish(exit)`) and reports any new sink error.
pub(crate) fn end(ctx: &mut Ctx, verb: &str, exit: u8, replay_failed: bool) -> u8 {
    match ctx.fx.finish(exit) {
        Ok(()) => exit,
        Err(e) if replay_failed && matches!(e.sub, Sub::Replay(_)) => 3,
        Err(e) => out::fail(ctx, verb, &e),
    }
}

#[cfg(test)]
mod tests {
    use super::super::args::{Parsed, parse};
    use super::*;
    use crate::fx::Mode;
    use crate::wire::scripted::ScriptedTransport;

    #[test]
    fn begin_with_replays_the_tape_it_was_handed_and_reads_no_file() {
        let dir = crate::test_fixtures::scratch_dir("taping");
        let path = dir.path().join("handed.atape");
        let argv: Vec<String> = ["repo", "view", "alabsystems/aterm"]
            .map(String::from)
            .to_vec();
        let header = TapeHeader::for_run(&argv, &Endpoints::github_com());
        std::fs::write(&path, tape::render(&header, &[])).expect("the tape");
        let mut args = vec!["--replay".to_string(), path.display().to_string()];
        args.extend(argv.iter().cloned());
        let Ok(Parsed::Run(_, inv)) = parse(&args) else {
            panic!("{args:?} parses as a run");
        };
        let taped = read_taped(&inv.global).expect("the tape reads");
        std::fs::remove_file(&path).expect("the tape removed");
        let ctx = || {
            Ctx::for_tests(
                dir.path(),
                Arc::new(ScriptedTransport::new()),
                Endpoints::github_com(),
                MapEnv::new(),
            )
            .0
        };
        let mut handed = ctx();
        begin_with(&mut handed, &inv, &argv, Some(taped)).expect("replays what it was handed");
        assert_eq!(handed.fx.mode(), Mode::Replaying);
        assert!(handed.dirs.is_none());
        let mut unhanded = ctx();
        let refused = begin_with(&mut unhanded, &inv, &argv, None).expect_err("no tape on disk");
        assert_eq!(refused.sub, crate::error::Sub::Local);
    }

    #[cfg(unix)]
    #[test]
    fn end_prints_the_sinks_own_error_after_a_latched_refusal() {
        use std::os::unix::fs::PermissionsExt;

        use crate::wire::{Deadlines, Method, WireBody, WireRequest};

        let dir = crate::test_fixtures::scratch_dir("taping");
        let sealed = dir.path().join("write-only");
        std::fs::create_dir(&sealed).expect("the parent");
        std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o300))
            .expect("write and search only");
        struct Readable<'a>(&'a std::path::Path);
        impl Drop for Readable<'_> {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(self.0, std::fs::Permissions::from_mode(0o700));
            }
        }
        let _readable = Readable(&sealed);
        if std::fs::read_dir(&sealed).is_ok() {
            // Skip when running as root.
            return;
        }
        let path = sealed.join("new.atape");
        let e = Endpoints::github_com();
        let argv: Vec<String> = ["api", "/x"].map(String::from).to_vec();
        let (mut ctx, captured) = Ctx::for_tests(
            dir.path(),
            Arc::new(ScriptedTransport::new()),
            e.clone(),
            MapEnv::new(),
        );
        ctx.fx = Arc::new(Fx::record(&path, &TapeHeader::for_run(&argv, &e)).expect("a recording"));
        let mut req = WireRequest::new(
            Method::Post,
            "https://api.github.com/x",
            Vec::new(),
            1 << 20,
            Deadlines::JSON,
        );
        req.body = WireBody::Bytes(b"{}");
        let transport = ScriptedTransport::new();
        let refused = ctx.fx.gh(&transport, &e, req).expect_err("a body");
        assert!(matches!(refused.sub, Sub::Replay(_)), "{refused:?}");
        let before = captured.stderr().len();
        assert_eq!(end(&mut ctx, "api", 3, true), 3);
        let stderr = String::from_utf8(captured.stderr()).expect("UTF-8");
        assert!(stderr.len() > before, "end printed nothing");
        assert!(stderr.contains(&path.display().to_string()), "{stderr}");
        assert!(transport.requests().is_empty());
    }
}
