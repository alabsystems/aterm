// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `--record` and `--replay` (design §21.3): what a taped command line runs in. M1's tapes are
//! anonymous: no agh.toml, no token, no rate book.

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

/// The command line with every `--record` and `--replay` (with its value, or its `=value`)
/// before any `--` removed, everything else kept in order. Which tokens those are is the
/// parser's own reading (`args::tape_tokens`), so a flag's value that looks like a tape flag
/// (`-f --record=1`) stays, and the taped line parses as the run did.
pub(crate) fn without_tape_flags(args: &[String]) -> Vec<String> {
    let drop = tape_tokens(args);
    args.iter()
        .enumerate()
        .filter(|(i, _)| !drop.contains(i))
        .map(|(_, arg)| arg.clone())
        .collect()
}

/// The tape `--replay PATH` names, read once (`read_tape`): its header and records. For
/// `--replay PATH` given with no verb, `cli::run` takes the command line from `header.argv`
/// and hands the same tape to `begin_with`, so a tape that cannot be read twice (a pipe,
/// `/dev/stdin`, a FIFO) still replays and rerecords.
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

/// Switch `ctx` over before the verb runs (plan §4.6.28's `begin`):
/// 1. **Under `--replay`.** The tape is `taped`, the one `read_taped` already read, or else read
///    once here (`read_tape`). Its header's `argv` must equal `argv`, each argument through
///    `Redactor::shapes` as the tape holds it, else `Replay::Argv`. This check comes before any
///    file is created. The seam is `Fx::replay_from` of that header and those records, or with
///    `--record` as well `Fx::rerecord_from(header, records, record)` (`Fx::rerecord`'s).
///    `taped` is ignored without `--replay`.
/// 2. **Under `--record` alone.** An argument the shapes would change, or a command line whose
///    taped `argv` line they would change once its arguments are joined (`tape::head_redacts`),
///    is a usage error, before any file is created: its tape would hold, and its bare replay
///    run, another command line. The seam is
///    `Fx::record(record, &TapeHeader::for_run(argv, &ctx.endpoints))`. A transport not yet
///    set becomes `HttpTransport::new(ctx.env)`, so the proxy variables still apply.
/// 3. **The environment.** `ctx.env` becomes `MapEnv::new()`, so no env profile and no ambient
///    token is read. Under `--replay`, a tape whose header says `endpoints github.com` also
///    sets `ctx.endpoints = Endpoints::github_com()`; a `loopback` tape keeps the test's own.
/// 4. **The files.** `ctx.dirs` becomes `None`, so `Ctx::github` builds `Router::empty()` and
///    no rate book, and agh.toml, the token files and the rate book are neither read nor
///    written.
/// 5. **The seam.** `ctx.fx` becomes the new one.
/// 6. **The retries.** `ctx.retry` becomes `RetryPolicy::STANDARD` under either flag, the
///    shipped binary's: a retry's sleep and its second exchange are on the tape, so a tape
///    replays under the policy it was recorded with, whatever the context's own (a test's
///    `RetryPolicy::NONE`). A replayed sleep takes no time.
///
/// The fields this switches stay switched after `end`: `begin` and `end` hand nothing back,
/// so a `Ctx` that ran a taped command line is not reused for another (a later record after
/// the tape's exit is refused by the sink, not written).
///
/// # Errors
///
/// As the steps say.
pub(crate) fn begin_with(
    ctx: &mut Ctx,
    inv: &Invocation,
    argv: &[String],
    taped: Option<(TapeHeader, Vec<EffectRecord>)>,
) -> Result<()> {
    let global = &inv.global;
    let fx = match (&global.replay, &global.record) {
        (Some(replay), record) => {
            // The argv is checked against the records this run replays. A handed tape is the one
            // `read_taped` read for a bare `--replay`, so one that cannot be read twice (a pipe)
            // still replays and rerecords. `None`, for a command line that names its verb,
            // reads it here, its one read.
            let (header, records) = match taped {
                Some(taped) => taped,
                None => read_tape(replay)?,
            };
            // The tape holds each argument shaped (`TapeSink::create`), so the typed ones are
            // compared shaped too.
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
            // An argument the shapes would change is taped redacted, and a bare replay of the
            // tape would run another command line: refused before the file is made.
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

/// The taped run's end: `ctx.fx.finish(exit)`. `Ok` → `exit`, so a replay exits as its tape's
/// run did. `Err(e)` of a replay's (`Sub::Replay`) → 3 when the verb's own error was a
/// replay's (`replay_failed`): every such error `finish` answers is the latch's, which
/// `out::fail` printed already. Any other `Err(e)` (the sink's own, `Sub::Local`, which a
/// recording meets after a refusal it latched without discarding the tape) →
/// `out::fail(ctx, verb, &e)`.
pub(crate) fn end(ctx: &mut Ctx, verb: &str, exit: u8, replay_failed: bool) -> u8 {
    match ctx.fx.finish(exit) {
        Ok(()) => exit,
        Err(e) if replay_failed && matches!(e.sub, Sub::Replay(_)) => 3,
        Err(e) => out::fail(ctx, verb, &e),
    }
}

// `begin_with`'s one read, handed or not; `tape_fixtures` replays a fixture from a pipe
// (`std::io::pipe`, named `/dev/fd/N`) with a bare `--replay`, through `cli::run`.
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
        // A pipe's second read finds nothing: here the file is gone instead.
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
        // Handed nothing, it reads the path, which is gone.
        let mut unhanded = ctx();
        let refused = begin_with(&mut unhanded, &inv, &argv, None).expect_err("no tape on disk");
        assert_eq!(refused.sub, crate::error::Sub::Local);
    }

    // A recording that latched a refusal without discarding its tape (a request with a body)
    // and then met a sink error of its own: the verb printed the refusal, and `end` prints the
    // sink's error, which names the tape left on disk. The sink's error here is a directory
    // that cannot be flushed (a parent that cannot be opened for reading).
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
        // Readable again on every way out, a panic included, so the scratch directory's own
        // drop can list and remove it.
        struct Readable<'a>(&'a std::path::Path);
        impl Drop for Readable<'_> {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(self.0, std::fs::Permissions::from_mode(0o700));
            }
        }
        let _readable = Readable(&sealed);
        if std::fs::read_dir(&sealed).is_ok() {
            // A user the mode does not bind (root): no directory here fails to flush.
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
