// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `--record` and `--replay` tape setup and teardown for CLI commands.

use std::sync::Arc;

use astream_effects::EffectRecord;

use super::args::{Global, Invocation, tape_tokens};
use super::{Ctx, out};
use crate::error::{Error, Replay, Result, Sub};
use crate::fx::tape::{self, TapeEndpoints, TapeHeader};
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

/// Configures `ctx` for `--record` or `--replay` before the verb runs.
///
/// # Errors
///
/// Returns an error if the tape cannot be read/created or `argv` does not match or redacts:
/// under `--replay`, `Replay::Argv` unless the header's `argv` equals `argv`, each argument
/// through `Redactor::shapes` as the tape holds it; under `--record` alone, a usage error for
/// an argument the shapes would change, or a command line whose taped `argv` line they would
/// change once its arguments are joined (`tape::head_redacts`). Each check comes before any
/// file is created.
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
            let typed: Vec<String> = argv.iter().map(|arg| Redactor::shapes(arg)).collect();
            if header.argv != typed {
                return Err(Error::replay(
                    Replay::Argv,
                    format!(
                        "this command line is not the tape's, which ran: {}",
                        header.argv.join(" ")
                    ),
                ));
            }
            let fx = match record {
                Some(record) => Fx::rerecord_from(&header, records, record)?,
                None => Fx::replay_from(&header, records),
            };
            if header.endpoints == TapeEndpoints::GithubCom {
                ctx.endpoints = Endpoints::github_com();
            }
            fx
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
            let fx = Fx::record(record, &header)?;
            if ctx.transport.is_none() {
                ctx.transport = Some(Arc::new(HttpTransport::new(ctx.env.clone())));
            }
            fx
        }
        (None, None) => return Ok(()),
    };
    ctx.env = Arc::new(MapEnv::new());
    ctx.dirs = None;
    ctx.fx = Arc::new(fx);
    ctx.retry = RetryPolicy::STANDARD;
    Ok(())
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
        let dir = aterm_tempfile::Builder::new()
            .prefix("agh-taping-")
            .tempdir()
            .expect("a scratch directory");
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

        let dir = aterm_tempfile::Builder::new()
            .prefix("agh-taping-")
            .tempdir()
            .expect("a scratch directory");
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
