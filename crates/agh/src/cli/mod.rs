// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The CLI: `main_entry`, `run`, the context a verb runs in and what a verb answers.

pub mod args;
mod cmd_api;
mod cmd_asset;
mod cmd_auth;
mod cmd_can;
mod cmd_content;
mod cmd_credential;
mod cmd_git;
mod cmd_intent;
mod cmd_meta;
mod cmd_policy;
mod cmd_rate;
mod cmd_ref;
mod cmd_release;
mod cmd_release_write;
mod cmd_repo;
mod cmd_wait;
mod cmd_whoami;
mod dry_run;
pub mod format;
pub mod help;
pub mod jq;
pub mod out;
pub mod project;
pub mod schema;
mod taping;
pub mod verbs;
pub mod write_out;

use std::io::IsTerminal;
use std::sync::Arc;
#[cfg(any(test, debug_assertions))]
use std::sync::{Mutex, PoisonError};

use astream_effects::EffectRecord;

use crate::client::{Github, GithubBuilder, Meter};
use crate::config::Dirs;
use crate::download::DigestCheck;
use crate::error::{Error, IdentityRef, Replay, Result, Sub};
use crate::fx::Fx;
use crate::fx::tape::TapeHeader;
#[cfg(any(test, debug_assertions))]
use crate::git::HelperCommand;
#[cfg(any(test, debug_assertions))]
use crate::identity::MapEnv;
use crate::identity::vault::Adoption;
use crate::identity::{EnvSource, ProcessEnv};
use crate::model::Sha256Digest;
#[cfg(any(test, debug_assertions))]
use crate::notify::NoNotifier;
use crate::notify::{Notifier, SessionNotifier};
use crate::page::{Consistency, End};
use crate::policy::Router;
use crate::rate::{RateBook, RateSnapshot};
use crate::redact::Redactor;
use crate::retry::RetryPolicy;
use crate::wire::{Endpoints, Transport};

use args::{Invocation, ParseError, Parsed};
use out::OutputOpts;
use verbs::{Noun, Verb};

/// `aterm agh`, argv0 `agh` and the dev shim: the arguments after the program's name.
#[must_use = "handle or propagate the command's exit status"]
pub fn main_entry(args: Vec<std::ffi::OsString>) -> std::process::ExitCode {
    aterm_uds::secret::no_core_dumps();
    let mut ctx = Ctx::production();
    let mut text = Vec::with_capacity(args.len());
    for (i, arg) in args.into_iter().enumerate() {
        match arg.into_string() {
            Ok(arg) => text.push(arg),
            Err(_) => {
                let error =
                    Error::usage(format!("argument {} is not UTF-8", i + 1)).with_next("agh help");
                return std::process::ExitCode::from(out::fail(&mut ctx, "-", &error));
            }
        }
    }
    // Only native Git CLI commands adopt stop signals, before request/child threads.
    // Library callers use `run` and keep their signal policy; other CLI verbs keep theirs.
    // The waiter only publishes a stop. The actual Git owner retires its child/group.
    #[cfg(unix)]
    if matches!(
        args::parse(&text),
        Ok(Parsed::Run(verb, _)) if matches!(
            verb.path,
            ["repo", "fetch" | "fetch-main" | "public-refs" | "clone" | "push" | "check-local" | "commit-ancestry"]
                | ["ref", "delete"]
        )
    ) {
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let requested = stop.clone();
        if !aterm_uds::process::take_stop_signals("agh-stop", move |_| {
            requested.store(true, std::sync::atomic::Ordering::Release);
        }) {
            return std::process::ExitCode::from(out::fail(
                &mut ctx,
                "-",
                &Error::local("cannot start the CLI stop-signal waiter"),
            ));
        }
        ctx.fx = Arc::new(Fx::live().with_git_stop(Some(stop)));
    }
    std::process::ExitCode::from(run(&text, &mut ctx))
}

/// Parse and run one command line in `ctx`; the exit code.
pub fn run(args: &[String], ctx: &mut Ctx) -> u8 {
    ctx.github = None;
    ctx.adoption = None;
    ctx.anonymous = false;
    ctx.no_anonymous_api = false;
    match args::parse(args) {
        Err(ParseError { verb, error }) => out::fail(ctx, &verb, &error),
        Ok(Parsed::Bare) => out::write_stdout(ctx, help::brief().as_bytes(), 0),
        Ok(Parsed::Help(words)) => match help::render(&words) {
            Ok(page) => out::write_stdout(ctx, page.as_bytes(), 0),
            Err(e) => out::fail(ctx, "help", &e),
        },
        Ok(Parsed::ReplayTaped(global)) => {
            // The tape is read once, here, and handed to the run: a tape that cannot be read
            // twice (a pipe, `/dev/stdin`, a FIFO) replays as a file does.
            let tape = match taping::read_taped(&global) {
                Ok(tape) => tape,
                Err(e) => return out::fail(ctx, "-", &e),
            };
            let taped = tape.0.argv.clone();
            match args::parse(&taped) {
                Ok(Parsed::Run(verb, mut inv)) => {
                    inv.global.replay.clone_from(&global.replay);
                    inv.global.record.clone_from(&global.record);
                    if let Err(e) = args::tape_rule(verb, &inv.global)
                        .and_then(|()| {
                            args::tape_targets(verb, &inv.global, &inv.positionals, &inv.flags)
                        })
                        .and_then(|()| args::bare_replay_rule(verb, &inv))
                    {
                        return out::fail(ctx, &inv.verb_name(), &e);
                    }
                    run_verb(ctx, verb, &inv, &taped, Some(tape))
                }
                Ok(_) | Err(_) => {
                    let error =
                        Error::replay(Replay::Format, "the tape's command line names no verb");
                    out::fail(ctx, "-", &error)
                }
            }
        }
        Ok(Parsed::Run(verb, inv)) => {
            let argv = taping::without_tape_flags(args);
            run_verb(ctx, verb, &inv, &argv, None)
        }
    }
}

/// Steps 3 to 6 of `run`: the tapes, the verb, its answer, and the tapes' end. `tape` is the
/// tape a bare `--replay` already read for its command line (`taping::read_taped`), handed on
/// to `taping::begin_with` so it is never read twice.
fn run_verb(
    ctx: &mut Ctx,
    verb: &'static Verb,
    inv: &Invocation,
    argv: &[String],
    tape: Option<(TapeHeader, Vec<EffectRecord>)>,
) -> u8 {
    let name = inv.verb_name();
    if ctx.caller == "cli" || ctx.caller.starts_with("cli:") {
        ctx.caller = format!("cli:{name}");
    }
    let opts = OutputOpts::from_global(&inv.global);
    if inv.flag("--output") == Some("-")
        && let Err(e) = out::refuse_projection_flags(&name, &opts)
    {
        return out::fail(ctx, &name, &e.with_next(args::help_next(&inv.path)));
    }
    if let Err(e) = out::check_opts(&name, verb.noun, &opts) {
        return out::fail(ctx, &name, &e);
    }
    let taped = inv.global.record.is_some() || inv.global.replay.is_some();
    if taped && let Err(e) = taping::begin_with(ctx, inv, argv, tape) {
        return out::fail(ctx, &name, &e);
    }
    ctx.timeout = inv.global.timeout;
    ctx.anonymous = inv.global.anonymous;
    ctx.no_anonymous_api = inv.global.no_anonymous_api;
    let t0 = ctx.fx.mono();
    let (exit, replay_failed) = match (verb.run)(ctx, inv) {
        Ok(mut outcome) => {
            if let Answer::FailedWith { error, .. } = &mut outcome.answer {
                ctx.vault_next(error);
            }
            outcome.receipt.elapsed_ms = match (t0, ctx.fx.mono()) {
                (Ok(t0), Ok(t1)) => {
                    u64::try_from(t1.saturating_sub(t0).as_millis()).unwrap_or(u64::MAX)
                }
                _ => 0,
            };
            (out::emit(ctx, outcome, &opts), false)
        }
        Err(mut e) => {
            ctx.vault_next(&mut e);
            let replay_failed = matches!(e.sub, Sub::Replay(_));
            let e = out::with_verb_help(e, &name);
            let exit = out::fail(ctx, &name, &e);
            if opts.meta
                && let Some(gh) = &ctx.github
            {
                let receipt = Receipt::new(&name).metered(&gh.meter());
                let line =
                    out::receipt_line(&receipt, &ctx.redactor).replacen("agh: ok", "agh: meter", 1);
                out::note(ctx, &line);
            }
            (exit, replay_failed)
        }
    };
    if taped {
        taping::end(ctx, &name, exit, replay_failed)
    } else {
        exit
    }
}

/// What a verb runs in: the environment, the files, the streams, the effect seam and the client.
pub struct Ctx {
    /// Environment reads: proxy variables, env profiles, ambient tokens.
    pub env: Arc<dyn EnvSource + Send + Sync>,
    /// `None` when the platform names no config, state or home directory.
    pub dirs: Option<Dirs>,
    /// Standard output.
    pub stdout: Box<dyn std::io::Write>,
    /// Standard error.
    pub stderr: Box<dyn std::io::Write>,
    /// Standard input.
    pub stdin: Box<dyn std::io::Read>,
    /// Whether stdin is a terminal.
    pub stdin_is_terminal: bool,
    /// Whether stdout is a terminal.
    pub stdout_is_terminal: bool,
    /// Whether stderr is a terminal.
    pub stderr_is_terminal: bool,
    /// THE effect seam (plan §4.6.27).
    pub fx: Arc<Fx>,
    /// `None`: `HttpTransport::new(env)`.
    pub transport: Option<Arc<dyn Transport>>,
    /// The GitHub the client speaks to.
    pub endpoints: Endpoints,
    /// The `User-Agent` caller: `cli`.
    pub caller: String,
    /// Shared with the client: what the client learns, `out` applies.
    pub redactor: Redactor,
    /// The client's retry policy: STANDARD in production, NONE under `for_tests`.
    pub retry: RetryPolicy,
    /// `--timeout`, which `run` sets before the verb runs.
    pub timeout: Option<std::time::Duration>,
    /// Where `--notify` posts go: `SessionNotifier::from_env` in production, `NoNotifier` under
    /// `for_tests`.
    pub notifier: Arc<dyn Notifier>,
    /// Tests: the helper command the git runner mounts (`with_git_helper`). `None`, and every
    /// release build, which has no such field, mount `HelperCommand::current()`. Test-only
    /// because the builder setter it feeds, `GithubBuilder::git_helper_for_tests`, is.
    #[cfg(any(test, debug_assertions))]
    pub git_helper: Option<HelperCommand>,
    /// Tests: the git remotes `with_git_remote` mapped, slug → local repository, in the order
    /// given. `Ctx::github()` hands each to `GithubBuilder::git_remote_for_tests`. Private, like
    /// M1's `github` field, so no literal outside agh can build a `Ctx`.
    #[cfg(any(test, debug_assertions))]
    git_remotes: Vec<(String, std::path::PathBuf)>,
    github: Option<Github>,
    /// What vault adoption found for this command line, once the client was built with no
    /// agh.toml (design §6.10).
    adoption: Option<Adoption>,
    /// `--anonymous`, which `run` sets before the verb runs: adoption then asks GitHub nothing,
    /// because learning a login sends the token.
    anonymous: bool,
    no_anonymous_api: bool,
}

/// stdout through the door: each write is written whole and flushed.
struct StdoutDoor;

impl std::io::Write for StdoutDoor {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        aterm_uds::stdout::write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// stdin past std's buffer. On unix the first read takes a duplicate of fd 0, and every read
/// goes straight into the caller's buffer, so a token `auth add --stdin` reads is never also
/// held in std's 8 KiB `Stdin` buffer, which nothing scrubs. Elsewhere (where `auth add`
/// refuses by name) it is `std::io::stdin()`.
#[derive(Default)]
struct StdinDoor {
    #[cfg(unix)]
    fd: Option<std::fs::File>,
}

impl std::io::Read for StdinDoor {
    #[cfg(unix)]
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use std::os::fd::AsFd;
        if let Some(fd) = &mut self.fd {
            return std::io::Read::read(fd, buf);
        }
        let fd = std::io::stdin().as_fd().try_clone_to_owned()?;
        std::io::Read::read(self.fd.insert(std::fs::File::from(fd)), buf)
    }

    #[cfg(not(unix))]
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        std::io::Read::read(&mut std::io::stdin(), buf)
    }
}

impl Ctx {
    /// The real process.
    #[must_use]
    pub fn production() -> Ctx {
        Ctx {
            env: Arc::new(ProcessEnv),
            dirs: Dirs::from_env(),
            stdout: Box::new(StdoutDoor),
            stderr: Box::new(std::io::stderr()),
            stdin: Box::new(StdinDoor::default()),
            stdin_is_terminal: std::io::stdin().is_terminal(),
            stdout_is_terminal: std::io::stdout().is_terminal(),
            stderr_is_terminal: std::io::stderr().is_terminal(),
            fx: Arc::new(Fx::live()),
            transport: None,
            endpoints: Endpoints::github_com(),
            caller: "cli".to_string(),
            redactor: Redactor::new(),
            retry: RetryPolicy::STANDARD,
            timeout: None,
            notifier: Arc::new(SessionNotifier::from_env(&ProcessEnv)),
            #[cfg(any(test, debug_assertions))]
            git_helper: None,
            #[cfg(any(test, debug_assertions))]
            git_remotes: Vec::new(),
            github: None,
            adoption: None,
            anonymous: false,
            no_anonymous_api: false,
        }
    }

    /// A test context.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn for_tests(
        root: &std::path::Path,
        transport: Arc<dyn Transport>,
        endpoints: Endpoints,
        env: MapEnv,
    ) -> (Ctx, Captured) {
        let captured = Captured::default();
        let ctx = Ctx {
            env: Arc::new(env),
            dirs: Some(Dirs::under(root)),
            stdout: Box::new(SharedBuf(captured.stdout.clone())),
            stderr: Box::new(SharedBuf(captured.stderr.clone())),
            stdin: Box::new(std::io::empty()),
            stdin_is_terminal: false,
            stdout_is_terminal: false,
            stderr_is_terminal: false,
            fx: Arc::new(Fx::simulated(1_760_000_000)),
            transport: Some(transport),
            endpoints,
            caller: "test".to_string(),
            redactor: Redactor::new(),
            retry: RetryPolicy::NONE,
            timeout: None,
            notifier: Arc::new(NoNotifier),
            git_helper: None,
            git_remotes: Vec::new(),
            github: None,
            adoption: None,
            anonymous: false,
            no_anonymous_api: false,
        };
        (ctx, captured)
    }

    /// The same context reading `bytes` as stdin.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn with_stdin(mut self, bytes: Vec<u8>) -> Ctx {
        self.stdin = Box::new(std::io::Cursor::new(bytes));
        self
    }

    /// The same context with these terminal flags.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub const fn with_terminals(mut self, stdout: bool, stderr: bool) -> Ctx {
        self.stdout_is_terminal = stdout;
        self.stderr_is_terminal = stderr;
        self
    }

    /// The same context, its stdin a terminal or not.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub const fn with_stdin_terminal(mut self, stdin: bool) -> Ctx {
        self.stdin_is_terminal = stdin;
        self
    }

    /// The same context with `notifier`.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn with_notifier(mut self, notifier: Arc<dyn Notifier>) -> Ctx {
        self.notifier = notifier;
        self
    }

    /// The same context mounting `helper` (tests: `CARGO_BIN_EXE_agh`).
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn with_git_helper(mut self, helper: HelperCommand) -> Ctx {
        self.git_helper = Some(helper);
        self
    }

    /// The same context mapping `slug` to the local repository at `path` for git (§4.6.13).
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn with_git_remote(mut self, slug: &str, path: &std::path::Path) -> Ctx {
        self.git_remotes
            .push((slug.to_string(), path.to_path_buf()));
        self
    }

    /// The client, built on first use and kept for the rest of the command line. It routes by
    /// agh.toml; with none, or one that says `vault = true`, by the vault's tokens too
    /// (`vault::adopt`), whose note lines, for a vault token found and not routed, go to
    /// stderr. Each adopted profile sends the very token adoption judged (`Github::pin`). The
    /// requests adoption sent count in the client's meter.
    /// # Errors
    /// `config::load`'s refusal: the CLI fails closed on an invalid agh.toml; then adoption's
    /// refusal (`vault::Adoption::router`).
    pub fn github(&mut self) -> Result<Github> {
        self.github_noting(true)
    }

    /// Recorded routing only: no identity-learning requests or adoption writes.
    pub(crate) fn github_recorded(&mut self) -> Result<Github> {
        self.github_mode(false, true)
    }

    /// `github`, writing adoption's note lines only when `noting`.
    fn github_noting(&mut self, noting: bool) -> Result<Github> {
        self.github_mode(noting, false)
    }

    fn github_mode(&mut self, noting: bool, recorded: bool) -> Result<Github> {
        if let Some(gh) = &self.github {
            return Ok(gh.clone());
        }
        let base = self.client(Router::empty());
        let gh = if let Some(dirs) = self.dirs.clone() {
            let resolved = if self.anonymous || recorded {
                crate::routing::view(&dirs, base.env())?
            } else {
                crate::routing::adopt(&base, &dirs)?
            };
            if noting
                && !self.anonymous
                && let Some(adoption) = resolved.adoption()
            {
                for line in adoption.note(&dirs.home) {
                    out::note(self, &line);
                }
            }
            let bound = resolved.bind(&base);
            self.adoption = resolved.into_adoption();
            bound?
        } else {
            base
        };
        self.github = Some(gh.clone());
        Ok(gh)
    }

    /// What vault adoption found, adopting first when the client is not built yet, with no note
    /// on stderr (its caller reports it): `None` when an agh.toml routes alone, or when there is
    /// no config directory.
    pub(crate) fn adoption(&mut self) -> Option<&Adoption> {
        if self.github.is_none() && self.adoption.is_none() {
            // A refusal is the adoption's own, which it carries for the caller to report.
            let _ = self.github_noting(false);
        }
        self.adoption.as_ref()
    }

    /// `error`'s next step made the vault's, when the call routed by adoption's rows and its
    /// own step names a row or a file adoption has none of (`Adoption::next_for`).
    fn vault_next(&self, error: &mut Error) {
        if let Some(next) = self.adoption.as_ref().and_then(|a| a.next_for(error)) {
            error.next = next;
        }
    }

    /// The client over no rows, never adopting: `agh auth add` binds its new profile's
    /// login on it, so provisioning works whatever adoption found.
    pub(crate) fn provisioning_client(&self) -> Github {
        self.client(Router::empty())
    }

    /// The client this context configures, over `router`: the write journal's directories when
    /// there are any, the notifier, and (test and debug builds only) the git helper and remotes
    /// a test set. Under a tape the rate book is none whatever `dirs` is: it is not on the tape.
    fn client(&self, router: Router) -> Github {
        let taped = self.fx.mode().is_taped();
        let rate_book = match &self.dirs {
            Some(dirs) if !taped => RateBook::at(dirs.rate()),
            _ => RateBook::none(),
        };
        let mut builder = GithubBuilder::new(router)
            .endpoints(self.endpoints.clone())
            .env(self.env.clone())
            .rate_book(rate_book)
            .fx(self.fx.clone())
            .caller(&self.caller)
            .retry(self.retry)
            .redactor(self.redactor.clone())
            .backstop(self.timeout)
            .refuse_anonymous_api(self.no_anonymous_api)
            .notifier(self.notifier.clone());
        if let Some(dirs) = &self.dirs {
            builder = builder.journal(dirs.clone());
        }
        if let Some(transport) = &self.transport {
            builder = builder.transport(transport.clone());
        }
        #[cfg(any(test, debug_assertions))]
        if let Some(helper) = &self.git_helper {
            builder = builder.git_helper_for_tests(helper.clone());
        }
        #[cfg(any(test, debug_assertions))]
        for (slug, path) in &self.git_remotes {
            builder = builder.git_remote_for_tests(slug, path);
        }
        builder.build()
    }
}

/// A writer into a shared buffer.
#[cfg(any(test, debug_assertions))]
struct SharedBuf(Arc<Mutex<Vec<u8>>>);

#[cfg(any(test, debug_assertions))]
impl std::io::Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// What a test reads back.
#[cfg(any(test, debug_assertions))]
#[derive(Clone, Default)]
pub struct Captured {
    stdout: Arc<Mutex<Vec<u8>>>,
    stderr: Arc<Mutex<Vec<u8>>>,
}

#[cfg(any(test, debug_assertions))]
impl Captured {
    /// Everything written to stdout.
    #[must_use]
    pub fn stdout(&self) -> Vec<u8> {
        self.stdout
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Everything written to stderr.
    #[must_use]
    pub fn stderr(&self) -> Vec<u8> {
        self.stderr
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Lossy UTF-8.
    #[must_use]
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout()).into_owned()
    }

    /// Lossy UTF-8.
    #[must_use]
    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr()).into_owned()
    }
}

/// What a verb answers.
#[derive(Debug)]
pub struct Outcome {
    /// The answer.
    pub answer: Answer,
    /// The receipt.
    pub receipt: Receipt,
}

/// The answer's shape (design §8.2).
#[derive(Debug)]
pub enum Answer {
    /// One document.
    Object {
        /// What it is a document of.
        noun: Noun,
        /// The document.
        value: aterm_json::Value,
    },
    /// A list, already in the noun's order.
    Items {
        /// What each item is a document of.
        noun: Noun,
        /// The items.
        items: Vec<aterm_json::Value>,
        /// Whether it is the whole collection.
        completeness: Completeness,
    },
    /// Bytes, printed exactly (`cat`, help pages, `api` raw media).
    Bytes(Vec<u8>),
    /// Bytes the verb already wrote to stdout itself (`release download --output -`).
    Streamed {
        /// How many.
        written: u64,
    },
    /// Files written (`release download`), printed as a list of `Download` rows.
    Files(Vec<FileWritten>),
    /// Nothing on stdout.
    Nothing,
    /// A report on stdout, then the error's two lines.
    FailedWith {
        /// The report.
        value: aterm_json::Value,
        /// The error.
        error: Error,
    },
}

/// Whether a list is the whole collection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Completeness {
    /// The whole collection.
    Complete,
    /// Cut at `--limit`.
    Limited {
        /// The limit asked.
        asked: usize,
    },
    /// Incomplete, accepted under `--partial`.
    Partial {
        /// Why.
        why: String,
    },
}

impl Completeness {
    /// `End::Complete` → Complete; `Limited { at }` → `Limited { asked: at }`;
    /// `Capped { pages }`, which a listing ends with only under `--partial` →
    /// `Partial { why: "the listing stopped at the {pages}-page cap" }`.
    #[must_use]
    pub fn of(end: End) -> Completeness {
        match end {
            End::Complete => Completeness::Complete,
            End::Limited { at } => Completeness::Limited { asked: at },
            End::Capped { pages } => Completeness::Partial {
                why: crate::page::capped_why(pages),
            },
        }
    }
}

/// One downloaded file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileWritten {
    /// Where it is.
    pub path: std::path::PathBuf,
    /// Its size.
    pub bytes: u64,
    /// Its digest.
    pub sha256: Sha256Digest,
    /// The asset's id.
    pub asset_id: u64,
    /// False when the file already held these bytes.
    pub written: bool,
    /// How the digest was checked.
    pub digest: DigestCheck,
}

/// The receipt (design §8.6).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receipt {
    /// The verb, dotted.
    pub verb: String,
    /// The repository, when there is one.
    pub repo: Option<String>,
    /// Who the requests were sent as.
    pub as_: Option<IdentityRef>,
    /// The requests sent.
    pub requests: u32,
    /// The pages read.
    pub pages: u32,
    /// Whether the answer is whole.
    pub complete: bool,
    /// Whether a listing was revalidated.
    pub consistency: Option<Consistency>,
    /// The verb's time.
    pub elapsed_ms: u64,
    /// The last rate-limit snapshot.
    pub rate: Option<RateSnapshot>,
    /// Extra `key=value` pairs, in order (`digest=…`, `written=0`, `moved=…`).
    pub notes: Vec<(String, String)>,
    /// Printed even without `--meta` (design §8.1, cases 2 to 4).
    pub must_print: bool,
}

/// The anonymous budget under which a fall to anonymous prints the receipt.
const ANONYMOUS_LOW: u64 = 10;

impl Receipt {
    /// `verb` (dotted), `complete: true`, everything else empty.
    #[must_use]
    pub fn new(verb: &str) -> Receipt {
        Receipt {
            verb: verb.to_string(),
            repo: None,
            as_: None,
            requests: 0,
            pages: 0,
            complete: true,
            consistency: None,
            elapsed_ms: 0,
            rate: None,
            notes: Vec::new(),
            must_print: false,
        }
    }

    /// The receipt of a verb about one repository.
    #[must_use]
    pub(crate) fn for_repo(inv: &Invocation, gh: &Github, slug: &crate::slug::Slug) -> Receipt {
        let mut receipt = Receipt::new(&inv.verb_name()).metered(&gh.meter());
        receipt.repo = Some(slug.to_string());
        receipt
    }

    /// Mark the answer partial: `complete=0`, the note `partial={why}`, and `must_print`.
    pub(crate) fn partial(&mut self, why: String) {
        self.complete = false;
        self.notes.push(("partial".into(), why));
        self.must_print = true;
    }

    /// Fill `requests`, `as_` and `rate` from `meter`.
    #[must_use]
    pub fn metered(mut self, meter: &Meter) -> Receipt {
        self.requests = meter.requests;
        if let Ok(counts) = aterm_json::to_string(&meter.rest_requests) {
            self.notes.push(("rest".into(), counts));
        }
        self.as_.clone_from(&meter.as_);
        self.rate.clone_from(&meter.rate);
        if let Some((old, new)) = &meter.moved {
            self.notes.push(("moved".into(), format!("{old}->{new}")));
            self.must_print = true;
        }
        if let Some(notify) = &meter.notify {
            self.notes.push(("notify".into(), notify.clone()));
        }
        let low = meter
            .rate
            .as_ref()
            .is_some_and(|rate| rate.remaining < ANONYMOUS_LOW);
        if meter.fell_to_anonymous && low {
            self.must_print = true;
        }
        self
    }
}
