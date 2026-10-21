// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm init`'s secrets vault step (docs/DESIGN-secrets-2026-10-20.md §7): find the vault
//! (the ALab vault when it answers on this network, else the one the person names; Enter
//! keeps the ALab default, `none` skips), ask for its join password (`none` skips there too),
//! join it, and install the subscriber that keeps `~/.secrets` in sync. It runs before the
//! GitHub step, so the token files the vault carries (`~/.secrets/gh_access_token`) exist
//! before `gh` is authenticated.
//!
//! The vault is reached through one port, [`SecretsOps`]: [`LiveSecrets`] calls
//! `aterm_secrets::client` and `aterm_secrets::service`; tests script a fake. A probe reads
//! only this Mac's own state (its vault files and whether the subscriber runs; no network);
//! only an apply run at a terminal looks for the ALab vault, prompts or joins. Every prompt
//! comes before anything changes, so a Ctrl-C at one stops the run with nothing changed.
//! `--plan` and `--check` never prompt and never touch the network. No row names a secret
//! value: rows carry the vault's source (without any `user:password@` part), its name and
//! seq, a count of names, and the join code an admin compares.

use crate::items::{Ctx, Item};
use crate::model::{Mode, Probe, User};
use crate::port::System;
use aterm_secrets::SecretBytes;
use aterm_secrets::client::{JoinOutcome, Membership, Paths, ProbeOutcome, Status};
use aterm_secrets::service::State;
use aterm_secrets::transport::Source;
use std::cell::{Cell, OnceCell};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// The item family.
pub const FAMILY: &str = "secrets";

/// The prompt that asks for the vault.
pub const PROMPT: &str =
    "Secrets vault URL or path (Enter keeps http://alab-secrets.local:7484/v1/alab, none skips): ";

/// The prompt that asks for the vault's join password (hidden): an admin Mac shows it with
/// `aterm secrets password`, and its subscriber lets this Mac in within seconds. `none` or
/// `skip` skips the step, as at [`PROMPT`]: on the ALab LAN this is the step's only question.
pub const PASSWORD_PROMPT: &str = "Vault join password (an admin Mac shows it: aterm secrets password; Enter to be approved by code instead, none skips): ";

/// What a step skipped at a prompt says: nothing was changed.
const SKIPPED: &str = "skipped at the prompt; aterm secrets join <url> joins a vault later";

/// How long an interactive join waits for an admin to approve this Mac; the subscriber
/// finishes the join after that anyway.
pub const APPROVAL_WAIT: Duration = Duration::from_secs(10 * 60);

/// What a Ctrl-C at the join password prompt answers: it comes before anything changes.
const STOPPED_AT_PASSWORD: &str = "stopped at the join password prompt; nothing was changed";

/// How long the look for the ALab vault waits for an answer (plus the transport's margin):
/// a Mac on the ALab LAN answers within a second, and one elsewhere is asked for the URL.
pub const PROBE_WAIT: Duration = Duration::from_secs(3);

/// Why a run has no vault to join, when it may not ask for one.
const UNCONFIGURED: &str =
    "no vault configured; run aterm init in a terminal, or aterm secrets join <url>";

/// What an apply puts in place on a Mac whose vault is configured but whose subscriber does
/// not run.
const SUBSCRIBED: &str = "subscriber running (aterm secrets watch keeps ~/.secrets in sync)";

/// The vault, as `aterm init` sees it.
pub trait SecretsOps: Send + Sync {
    /// This Mac's vault state, read from local files only.
    ///
    /// # Errors
    /// The state cannot be read; the error names no value.
    fn status(&self) -> aterm_secrets::Result<Status>;

    /// The subscriber's state (`aterm_secrets::service::state`): launchd's or systemd's own
    /// view and the unit file, never the network.
    ///
    /// # Errors
    /// The service manager could not be asked.
    fn subscriber(&self) -> aterm_secrets::Result<State>;

    /// Whether a person can be asked: this process's controlling terminal (`/dev/tty`, where
    /// [`System::ask`] reads) opens and is a terminal. An unconfigured Mac's apply looks for
    /// the ALab vault and prompts only then.
    fn terminal(&self) -> bool;

    /// Whether the ALab vault answers on this network under its compiled-in signer
    /// (`aterm_secrets::client::probe_default`, one GET bounded by [`PROBE_WAIT`]): found, no
    /// such vault there (or no signer compiled in), or something there that is not it (a
    /// redirect or a login page among them).
    ///
    /// # Errors
    /// Nothing answered, or the server refused the GET as the vault server can (a `5xx`).
    fn probe_alab(&self) -> aterm_secrets::Result<ProbeOutcome>;

    /// Join the vault at `source` (a URL, `#<signer>` pin allowed, or a path). `password`: the
    /// join password the person typed before anything changed ([`ask_password`]; `None`, an
    /// approval by code), proved with the request when this Mac is not in yet. `interactive`:
    /// a person is at the terminal (`sys.ask` reaches them), so an unpinned signer may be
    /// confirmed by fingerprint, and the join waits up to [`APPROVAL_WAIT`] for an approval,
    /// ending as soon as this Mac is in, whoever applied the version that let it in.
    /// `requested` is called once the join request is queued and before any wait; it answers
    /// whether the subscriber is now in place to finish the join by itself.
    ///
    /// # Errors
    /// The join failed; the error names no value.
    fn join(
        &self,
        sys: &dyn System,
        source: &str,
        password: Option<&SecretBytes>,
        interactive: bool,
        requested: &dyn Fn() -> bool,
    ) -> aterm_secrets::Result<JoinOutcome>;

    /// Install (or replace) and start the subscriber (`aterm secrets watch`). Returns a line
    /// saying what was done.
    ///
    /// # Errors
    /// The job could not be installed or started.
    fn install_subscriber(&self) -> aterm_secrets::Result<String>;
}

/// The live vault of one user: `aterm_secrets::client` over `<home>/.config/aterm/secrets`
/// and `<home>/.secrets`, and the subscriber as `aterm_secrets::service` installs it.
#[derive(Debug, Clone)]
pub struct LiveSecrets {
    home: PathBuf,
}

impl LiveSecrets {
    /// The vault of the user whose home is `home`.
    #[must_use]
    pub fn new(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
        }
    }

    fn paths(&self) -> Paths {
        Paths::for_home(&self.home)
    }
}

/// Wait up to `wait` for an admin to let this Mac in, as `client::join`'s own wait does
/// (a long-poll of a URL, a pass every 5 s of a path), but ending as soon as this Mac reads as
/// in. The subscriber the join's `requested` hook started long-polls the same vault, and when
/// it applies the approving version first, this process's pass finds nothing newer: a wait
/// that ends only on a pass that applies (`client::join`'s) would run out its ten minutes and
/// report a Mac that is in as waiting. `subscribed`: that subscriber is in place.
/// `mistyped`: a join password was typed, and how a mistyped one is fixed ([`retype`]), said
/// with the wait because that wait is otherwise all a mistyped password shows. `None`: still
/// waiting at the end.
fn wait_for_approval(
    paths: &Paths,
    wait: Duration,
    subscribed: bool,
    mistyped: Option<&str>,
    say: &dyn Fn(&str),
) -> Option<JoinOutcome> {
    if wait.is_zero() {
        return None;
    }
    say(if subscribed {
        "waiting for the approval (Ctrl-C stops waiting; this Mac joins by itself once approved)"
    } else {
        "waiting for the approval (Ctrl-C stops waiting)"
    });
    if let Some(mistyped) = mistyped {
        say(&format!(
            "with the join password an admin Mac's subscriber approves within seconds; {mistyped}"
        ));
    }
    let long_poll = matches!(
        aterm_secrets::client::config(paths),
        Ok(Some(config)) if matches!(config.source, Source::Url(_))
    );
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        let started = Instant::now();
        let left = deadline
            .saturating_duration_since(started)
            .min(Duration::from_secs(50));
        if let Err(error) = aterm_secrets::client::sync(paths, long_poll.then_some(left)) {
            say(&format!("still waiting ({error})"));
        }
        if let Ok(Status {
            membership: Membership::In { seq, .. },
            names,
            ..
        }) = aterm_secrets::client::status(paths)
        {
            return Some(JoinOutcome::Joined { seq, names });
        }
        let pause = Duration::from_secs(5).saturating_sub(started.elapsed());
        std::thread::sleep(pause.min(deadline.saturating_duration_since(Instant::now())));
    }
    None
}

impl SecretsOps for LiveSecrets {
    fn status(&self) -> aterm_secrets::Result<Status> {
        aterm_secrets::client::status(&self.paths())
    }

    fn subscriber(&self) -> aterm_secrets::Result<State> {
        aterm_secrets::service::state(&self.home, aterm_secrets::service::Job::Subscriber)
    }

    fn terminal(&self) -> bool {
        use std::io::IsTerminal as _;
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .is_ok_and(|tty| tty.is_terminal())
    }

    fn probe_alab(&self) -> aterm_secrets::Result<ProbeOutcome> {
        eprintln!("aterm init: secrets: looking for the ALab vault on this network");
        let probed = aterm_secrets::client::probe_default(PROBE_WAIT);
        match &probed {
            Ok(ProbeOutcome::Found(found)) => eprintln!(
                "aterm init: secrets: the ALab vault answers here (seq {}, signer {})",
                found.seq, found.fingerprint
            ),
            // Said, so a person who expected the ALab vault knows why the URL is asked.
            Ok(ProbeOutcome::NotThisVault(why)) => eprintln!(
                "aterm init: secrets: something answers at the ALab vault's address, but it is not the ALab vault ({why})"
            ),
            Ok(ProbeOutcome::Absent) | Err(_) => {}
        }
        probed
    }

    fn join(
        &self,
        sys: &dyn System,
        source: &str,
        password: Option<&SecretBytes>,
        interactive: bool,
        requested: &dyn Fn() -> bool,
    ) -> aterm_secrets::Result<JoinOutcome> {
        let paths = self.paths();
        let confirm = |fingerprint: &str| confirm_signer(sys, fingerprint);
        let say = |line: &str| eprintln!("aterm init: secrets: {line}");
        let mistyped = password.and_then(|_| retype(source));
        let password = || password.cloned();
        let subscribed = Cell::new(false);
        let queued = || subscribed.set(requested());
        let options = aterm_secrets::client::JoinOptions {
            source: source.to_owned(),
            host: None,
            interactive,
            // The wait is this port's own, `wait_for_approval`, which ends once this Mac is in.
            wait: Duration::ZERO,
            confirm: &confirm,
            password: &password,
            requested: &queued,
            say: &say,
        };
        match aterm_secrets::client::join(&paths, &options)? {
            JoinOutcome::Pending { code, host } => Ok(wait_for_approval(
                &paths,
                approval_wait(interactive),
                subscribed.get(),
                mistyped.as_deref(),
                &say,
            )
            .unwrap_or(JoinOutcome::Pending { code, host })),
            done => Ok(done),
        }
    }

    fn install_subscriber(&self) -> aterm_secrets::Result<String> {
        let program = aterm_secrets::service::program_for_service()?;
        aterm_secrets::service::install(
            &self.home,
            aterm_secrets::service::Job::Subscriber,
            &program,
            &["secrets".to_owned(), "watch".to_owned()],
        )
    }
}

/// How long a join waits for an approval: [`APPROVAL_WAIT`] with a person at the terminal,
/// none without (the subscriber finishes it).
#[must_use]
pub const fn approval_wait(interactive: bool) -> Duration {
    if interactive {
        APPROVAL_WAIT
    } else {
        Duration::ZERO
    }
}

/// What the person answered at the join password prompt ([`ask_password`]).
#[derive(Debug)]
pub enum Typed {
    /// A join password, blanks around it dropped.
    Password(SecretBytes),
    /// Nothing: the join waits for an approval by code.
    Nothing,
    /// `none` or `skip`: this Mac joins no vault now.
    Skip,
}

/// Ask the person for the vault's join password ([`PASSWORD_PROMPT`], no echo), blanks
/// around it dropped. `none` or `skip`, in any case, is [`Typed::Skip`]: no join password
/// is that short (`aterm_secrets::join::MIN_LEN`), so neither word is ever a password.
///
/// # Errors
/// The read failed (no terminal after all); [`io::ErrorKind::Interrupted`] when the person
/// stopped the prompt (Ctrl-C).
pub fn ask_password(sys: &dyn System) -> io::Result<Typed> {
    let Some(answer) = sys.ask(PASSWORD_PROMPT, true)? else {
        return Ok(Typed::Nothing);
    };
    let typed = answer.bytes().trim_ascii();
    Ok(if typed.is_empty() {
        Typed::Nothing
    } else if typed.eq_ignore_ascii_case(b"none") || typed.eq_ignore_ascii_case(b"skip") {
        Typed::Skip
    } else {
        Typed::Password(SecretBytes::from(typed))
    })
}

/// Ask the person whether to trust a vault signer seen for the first time: `y` or `yes`
/// trusts it; anything else, nothing typed, or no terminal does not.
pub fn confirm_signer(sys: &dyn System, fingerprint: &str) -> bool {
    let question = format!(
        "The vault is signed by {fingerprint}; compare it with aterm secrets status on an admin Mac. Trust it? [y/N] "
    );
    match sys.ask(&question, false) {
        Ok(Some(answer)) => {
            let answer = answer.bytes().trim_ascii();
            answer.eq_ignore_ascii_case(b"y") || answer.eq_ignore_ascii_case(b"yes")
        }
        Ok(None) | Err(_) => false,
    }
}

/// The live port for the one user `aterm init` configures.
///
/// # Errors
/// The run does not name exactly one user.
pub fn live(ctx: &Ctx) -> Result<Arc<dyn SecretsOps>, String> {
    let user = only_user(ctx)?;
    Ok(Arc::new(LiveSecrets::new(Path::new(&user.home))))
}

fn only_user(ctx: &Ctx) -> Result<&User, String> {
    match ctx.users.as_slice() {
        [user] => Ok(user),
        _ => Err("aterm init secrets requires exactly one local user".into()),
    }
}

/// The secrets vault item for this run.
///
/// A Mac that has a vault configured gets the item for it, and nobody is asked anything (an
/// apply starts the subscriber if it does not run). One that has none is helped to one only
/// in an apply run with a terminal: first the ALab vault is looked for on this network
/// ([`SecretsOps::probe_alab`]), and when it answers, the person is asked only for its join
/// password ([`PASSWORD_PROMPT`]); otherwise they are asked for a vault ([`PROMPT`]: Enter
/// keeps [`aterm_secrets::ALAB_DEFAULT_URL`]), then, for a URL, for its join password. At
/// either prompt `none` or `skip` skips the step with nothing changed. Both are asked here,
/// before anything changes. Without a
/// terminal, or in `--plan`/`--check`, the row says no vault is configured and how to join
/// one: never a prompt there, and never the network.
///
/// # Errors
/// The run does not name exactly one user, or the person stopped a prompt (Ctrl-C); nothing
/// was changed then.
pub fn items(
    sys: &dyn System,
    ctx: &Ctx,
    ops: Arc<dyn SecretsOps>,
    mode: Mode,
) -> Result<Vec<Box<dyn Item>>, String> {
    let user = only_user(ctx)?.clone();
    let status = match ops.status() {
        Ok(status) => status,
        Err(why) => {
            return Ok(vec![Box::new(Note::new(
                user,
                Probe::Unusable {
                    why: format!("cannot read this Mac's vault state: {why}"),
                },
            ))]);
        }
    };
    if let Some(config) = &status.config {
        let source = match &config.source {
            Source::Url(url) => url.clone(),
            Source::Path(path) => path.display().to_string(),
        };
        return Ok(vec![Box::new(Vault::new(
            user,
            ops,
            source,
            Origin::Configured,
            None,
        ))]);
    }
    if mode != Mode::Apply || !ops.terminal() {
        return Ok(vec![Box::new(Note::not_applicable(user, UNCONFIGURED))]);
    }
    // Nothing answering is the usual answer off the ALab LAN: the person names a vault.
    let source = if let Ok(ProbeOutcome::Found(_)) = ops.probe_alab() {
        aterm_secrets::ALAB_DEFAULT_URL.to_owned()
    } else {
        let answer = match sys.ask(PROMPT, false) {
            Ok(answer) => answer,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {
                return Err("stopped at the secrets vault prompt; nothing was changed".into());
            }
            // No terminal: nobody to ask.
            Err(_) => return Ok(vec![Box::new(Note::not_applicable(user, UNCONFIGURED))]),
        };
        let typed = match &answer {
            None => String::new(),
            Some(line) => match std::str::from_utf8(line.bytes()) {
                Ok(text) => text.trim().to_owned(),
                Err(_) => {
                    return Ok(vec![Box::new(Note::new(
                        user,
                        Probe::Blocked {
                            why: "the answer at the vault prompt is not UTF-8 (not shown); run aterm init again, or aterm secrets join <url>".into(),
                        },
                    ))]);
                }
            },
        };
        drop(answer);
        if typed.eq_ignore_ascii_case("none") || typed.eq_ignore_ascii_case("skip") {
            return Ok(vec![Box::new(Note::not_applicable(user, SKIPPED))]);
        }
        if typed.is_empty() {
            aterm_secrets::ALAB_DEFAULT_URL.to_owned()
        } else {
            typed
        }
    };
    // Asked here rather than by the join, which has made this Mac's key and saved the source
    // by then: a Ctrl-C at it must leave nothing behind, and so must a skip, which on the
    // ALab LAN is the only way to say no. A path has no join queue, so nothing to prove; an
    // answer that is no source at all is never joined.
    let password = if is_url(&source) {
        match ask_password(sys) {
            Ok(Typed::Password(password)) => Some(password),
            Ok(Typed::Nothing) => None,
            Ok(Typed::Skip) => return Ok(vec![Box::new(Note::not_applicable(user, SKIPPED))]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {
                return Err(STOPPED_AT_PASSWORD.into());
            }
            // No terminal after all: nobody to ask.
            Err(_) => return Ok(vec![Box::new(Note::not_applicable(user, UNCONFIGURED))]),
        }
    } else {
        None
    };
    Ok(vec![Box::new(Vault::new(
        user,
        ops,
        source,
        Origin::Chosen,
        password,
    ))])
}

fn item_id(user: &User) -> String {
    format!("init.secrets@{}", user.name)
}

/// Whether `source` is an `http://` or `https://` URL: a vault with a join queue.
fn is_url(source: &str) -> bool {
    let lower = source.to_ascii_lowercase();
    ["http://", "https://"]
        .iter()
        .any(|scheme| lower.starts_with(scheme) && lower.len() > scheme.len())
}

/// Whether `source` is what `aterm secrets join` takes: an `http://` or `https://` URL, an
/// absolute path, or a `~/` path.
fn plausible_source(source: &str) -> bool {
    is_url(source) || source.starts_with('/') || source.starts_with("~/")
}

/// `source` as a row shows it: a URL without query or fragment, or a generic label if
/// it is not accepted by the HTTP parser; a path as typed; anything else not at all (a person may have
/// pasted the wrong thing at the prompt).
fn shown(source: &str) -> String {
    if !plausible_source(source) {
        return "(the answer at the prompt, not shown)".into();
    }
    if is_url(source) {
        aterm_secrets::transport::describe(source)
    } else {
        source.to_owned()
    }
}

/// How the run came to the vault an item reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    /// This Mac had it configured: an apply only starts a subscriber that does not run, and
    /// asks nobody anything.
    Configured,
    /// A person at the terminal is joining it (they named it, pressed Enter, or the ALab
    /// vault answered), and gave its join password or none: the join may confirm a signer
    /// and wait.
    Chosen,
}

/// The vault this Mac reads, joined on apply.
struct Vault {
    user: User,
    ops: Arc<dyn SecretsOps>,
    /// What [`SecretsOps::join`] is given: as typed, the ALab default, or the configured
    /// source.
    source: String,
    origin: Origin,
    /// The join password typed for a chosen URL; taken by the apply's join and wiped with it.
    password: Mutex<Option<SecretBytes>>,
    /// Why the last apply stood aside: this Mac waits for an admin's approval.
    aside: Mutex<Option<String>>,
}

impl Vault {
    fn new(
        user: User,
        ops: Arc<dyn SecretsOps>,
        source: String,
        origin: Origin,
        password: Option<SecretBytes>,
    ) -> Self {
        Self {
            user,
            ops,
            source,
            origin,
            password: Mutex::new(password),
            aside: Mutex::new(None),
        }
    }

    fn set_aside(&self, why: Option<String>) {
        *self.aside.lock().unwrap_or_else(PoisonError::into_inner) = why;
    }

    /// Install the subscriber, saying so on the terminal.
    fn install(&self) -> Result<(), String> {
        let installed = self
            .ops
            .install_subscriber()
            .map_err(|why| format!("the subscriber could not be installed: {why}"))?;
        eprintln!("aterm init: secrets: {installed}");
        Ok(())
    }

    /// Install the subscriber once the join request is queued, saying at once when that
    /// fails: the person may stop the wait that follows, and the row saying it comes only at
    /// the end. Answers whether it is in place.
    fn install_queued(&self, installed: &OnceCell<Result<(), String>>) -> bool {
        installed
            .get_or_init(|| {
                let done = self.install();
                if let Err(why) = &done {
                    eprintln!(
                        "aterm init: secrets: {why}; run aterm secrets agent on to keep this Mac in sync (until then it joins only while aterm init waits)"
                    );
                }
                done
            })
            .is_ok()
    }

    /// Stand aside while this Mac waits for an admin to approve `code`.
    fn wait_for_admin(&self, code: Option<&str>) {
        self.set_aside(Some(format!(
            "waiting for an admin: {}",
            approval(code, &self.source)
        )));
    }
}

/// What an admin runs to let this Mac in, and how a mistyped join password for the vault at
/// `source` is fixed ([`retype`]).
fn approval(code: Option<&str>, source: &str) -> String {
    let approve = match code {
        Some(code) => format!(
            "approve this Mac on an admin Mac: aterm secrets approve {code} (the subscriber finishes the join once it is approved)"
        ),
        None => "approve this Mac on an admin Mac: aterm secrets requests, then aterm secrets approve <code> (the subscriber finishes the join once it is approved)".into(),
    };
    match retype(source) {
        Some(retype) => format!("{approve}; {retype}"),
        None => approve,
    }
}

/// How a mistyped join password for the vault at `source` is fixed: the join again, which
/// asks for the password again and asks to join with its proof. Without this a Mac whose
/// proof matched nothing just waits, its row saying only what an admin runs. `None` for a
/// path, which has no join queue and so no password. The ALab default is the join's own
/// default; another vault uses a placeholder for its full URL rather than a redacted
/// URL that may not address the same vault.
fn retype(source: &str) -> Option<String> {
    if !is_url(source) {
        return None;
    }
    let join = if source == aterm_secrets::ALAB_DEFAULT_URL {
        "aterm secrets join".to_owned()
    } else {
        "aterm secrets join <vault-url>".to_owned()
    };
    let source_note = if source == aterm_secrets::ALAB_DEFAULT_URL {
        ""
    } else {
        "; replace <vault-url> with this vault's full configured URL, including any query or signer pin"
    };
    Some(format!(
        "a mistyped join password is fixed by running {join} again (it asks for the password again){source_note}"
    ))
}

/// `n` secrets, in words.
fn secrets(n: usize) -> String {
    if n == 1 {
        "1 secret".into()
    } else {
        format!("{n} secrets")
    }
}

/// Why the subscriber in `state` keeps nothing in sync, or `None` while it runs. A unit
/// without a process is not enough: a plist whose `launchctl bootstrap` failed after it was
/// written, and a job that keeps ending, look the same, and either way nothing syncs until
/// an apply installs and starts it again.
fn idle(state: &State) -> Option<&'static str> {
    match state {
        State::Installed { pid: Some(_), .. } => None,
        State::Installed { pid: None, .. } => Some("subscriber installed but not running"),
        State::Absent => Some("subscriber not installed"),
    }
}

/// What this Mac's vault state means for the item reading `source`. `unsynced`: why the
/// subscriber keeps nothing in sync, `None` while it runs ([`idle`]; read only for a Mac that
/// is in, reading a plain file, or waiting for an admin).
fn judge(status: &Status, source: &str, unsynced: Option<&str>) -> Probe {
    let name = status
        .config
        .as_ref()
        .map_or("(unnamed)", |config| config.name.as_str());
    let code = status.machine.as_ref().map(|(_, code)| code.as_str());
    let inside = status.names.iter().filter(|n| !n.starts_with("~/")).count();
    let elsewhere = status.names.len() - inside;
    let held = if elsewhere == 0 {
        format!("{} in ~/.secrets", secrets(inside))
    } else {
        format!(
            "{} in ~/.secrets and {elsewhere} elsewhere in ~",
            secrets(inside)
        )
    };
    let unwatched = |now: String| Probe::Change {
        from: format!("{now}, {}", unsynced.unwrap_or_default()),
        to: SUBSCRIBED.into(),
    };
    let watched = unsynced.is_none();
    match &status.membership {
        Membership::In { seq, .. } if watched => Probe::Ok {
            now: format!("in vault {name} (seq {seq}): {held}, kept in sync by the subscriber"),
        },
        Membership::In { seq, .. } => unwatched(format!("in vault {name} (seq {seq})")),
        Membership::Plain if watched => Probe::Ok {
            now: format!(
                "reading the plain text file {}: {held}, kept in sync by the subscriber",
                shown(source)
            ),
        },
        Membership::Plain => unwatched(format!("reading the plain text file {}", shown(source))),
        Membership::Pending if watched => Probe::Person {
            step: "secrets.approve".into(),
            how: approval(code, source),
        },
        Membership::Pending => unwatched(format!("waiting for an admin to approve vault {name}")),
        Membership::Revoked { seq } => Probe::Blocked {
            why: format!(
                "an admin removed this Mac from vault {name} at seq {seq}; to rejoin, run aterm secrets join and have an admin approve it"
            ),
        },
        Membership::Unconfigured if !plausible_source(source) => Probe::Blocked {
            why: "the answer at the vault prompt is not an http(s):// URL, an absolute path or a ~/ path (not shown); run aterm init again, or aterm secrets join <url>".into(),
        },
        Membership::Unconfigured => Probe::Change {
            from: "no vault".into(),
            to: shown(source),
        },
    }
}

impl Item for Vault {
    fn id(&self) -> String {
        item_id(&self.user)
    }

    fn family(&self) -> &'static str {
        FAMILY
    }

    fn title(&self) -> String {
        format!(
            "Secrets vault {} synced into ~/.secrets (aterm help secrets)",
            shown(&self.source)
        )
    }

    fn probe(&self, _sys: &dyn System, _ctx: &Ctx) -> Probe {
        let status = match self.ops.status() {
            Ok(status) => status,
            Err(why) => {
                return Probe::Unusable {
                    why: format!("cannot read this Mac's vault state: {why}"),
                };
            }
        };
        let unsynced = match status.membership {
            Membership::In { .. } | Membership::Plain | Membership::Pending => {
                match self.ops.subscriber() {
                    Ok(state) => idle(&state),
                    Err(why) => {
                        return Probe::Unusable {
                            why: format!("cannot read whether the subscriber runs: {why}"),
                        };
                    }
                }
            }
            Membership::Unconfigured | Membership::Revoked { .. } => None,
        };
        judge(&status, &self.source, unsynced)
    }

    /// Join, then install the subscriber; on a Mac whose vault is configured, only install
    /// (and so start) it. A join that waits for an admin is not a failure: the apply stands
    /// aside saying what the admin runs, and the subscriber, installed as soon as the join
    /// request is queued (so a Ctrl-C during the wait leaves a Mac that joins by itself),
    /// finishes the join once it is approved. The subscriber is installed once per apply.
    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        self.set_aside(None);
        if self.origin == Origin::Configured {
            self.install()?;
            if let Ok(status) = self.ops.status()
                && status.membership == Membership::Pending
            {
                self.wait_for_admin(status.machine.as_ref().map(|(_, code)| code.as_str()));
            }
            return Ok(());
        }
        let installed: OnceCell<Result<(), String>> = OnceCell::new();
        let requested = || self.install_queued(&installed);
        let password = self
            .password
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        // Chosen at the terminal: the join may confirm a signer and wait.
        let outcome = self
            .ops
            .join(sys, &self.source, password.as_ref(), true, &requested)
            .map_err(|why| format!("joining the vault: {why}"));
        drop(password);
        let outcome = outcome?;
        installed
            .get_or_init(|| self.install())
            .clone()
            .map_err(|why| format!("the vault answered, but {why}"))?;
        if let JoinOutcome::Pending { code, .. } = &outcome {
            // The subscriber may have applied the version that let this Mac in as the join
            // gave up waiting: this Mac's own files say which.
            let inside = self
                .ops
                .status()
                .is_ok_and(|status| matches!(status.membership, Membership::In { .. }));
            if !inside {
                self.wait_for_admin(Some(code));
            }
        }
        Ok(())
    }

    fn stood_aside(&self) -> Option<aterm_converge::StoodAside> {
        self.aside
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .map(aterm_converge::StoodAside::Waiting)
    }
}

/// A row with nothing to apply: no vault to join (not configured, skipped, no terminal), or
/// a state that cannot be read.
struct Note {
    user: User,
    probe: Probe,
}

impl Note {
    fn new(user: User, probe: Probe) -> Self {
        Self { user, probe }
    }

    fn not_applicable(user: User, why: &str) -> Self {
        Self::new(user, Probe::NotApplicable { why: why.into() })
    }
}

impl Item for Note {
    fn id(&self) -> String {
        item_id(&self.user)
    }

    fn family(&self) -> &'static str {
        FAMILY
    }

    fn title(&self) -> String {
        "Secrets vault (aterm help secrets)".into()
    }

    fn probe(&self, _sys: &dyn System, _ctx: &Ctx) -> Probe {
        self.probe.clone()
    }

    fn apply(&self, _sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        Err("no vault to join".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{self, NoJournal};
    use crate::items::defaults::tests::{ctx, user};
    use crate::model::{ItemReport, Outcome};
    use crate::port::{Cmd, Fake, Ran, RunFailed, Secret, Stat};
    use aterm_secrets::age::Recipient;
    use aterm_secrets::client::{Config, Probed};
    use std::collections::VecDeque;

    /// A vault whose state answers from a script (the last one sticks), and which records
    /// every look for the ALab vault, join and subscriber install. Its subscriber is absent
    /// until an install succeeds, unless a state is scripted; an install that succeeds
    /// replaces a scripted state with a running subscriber.
    #[derive(Default)]
    struct Scripted {
        statuses: Mutex<VecDeque<aterm_secrets::Result<Status>>>,
        subscriber: Mutex<Option<aterm_secrets::Result<State>>>,
        terminal: Mutex<bool>,
        probed: Mutex<Option<aterm_secrets::Result<ProbeOutcome>>>,
        outcome: Mutex<Option<aterm_secrets::Result<JoinOutcome>>>,
        /// The join queues a request (calls `requested`) before it ends, as `client::join`
        /// does for a Mac not in yet.
        requests: Mutex<bool>,
        installed: Mutex<Option<aterm_secrets::Result<String>>>,
        probes: Mutex<u32>,
        joins: Mutex<Vec<(String, bool)>>,
        /// Whether each join was given a join password (never the password itself).
        passwords: Mutex<Vec<bool>>,
        /// What each `requested` call answered: the subscriber is in place.
        hooked: Mutex<Vec<bool>>,
        /// How many installs had happened when each join ended.
        installed_by_join: Mutex<Vec<u32>>,
        installs: Mutex<u32>,
    }

    impl Scripted {
        fn new(statuses: Vec<aterm_secrets::Result<Status>>) -> Arc<Self> {
            Arc::new(Self {
                statuses: Mutex::new(statuses.into()),
                terminal: Mutex::new(true),
                probed: Mutex::new(Some(Ok(ProbeOutcome::Absent))),
                outcome: Mutex::new(Some(Ok(JoinOutcome::Joined {
                    seq: 7,
                    names: vec!["gh_access_token".into()],
                }))),
                installed: Mutex::new(Some(Ok(
                    "systems.alab.aterm-secrets: launchd user agent".into()
                ))),
                ..Self::default()
            })
        }

        /// The ALab vault answers on this network.
        fn alab_answers(&self) {
            *self.probed.lock().unwrap() = Some(Ok(ProbeOutcome::Found(Probed {
                seq: 7,
                fingerprint: "0123-4567-89ab-cdef".into(),
            })));
        }

        fn subscribed(&self, state: aterm_secrets::Result<State>) {
            *self.subscriber.lock().unwrap() = Some(state);
        }

        fn probes(&self) -> u32 {
            *self.probes.lock().unwrap()
        }

        fn joins(&self) -> Vec<(String, bool)> {
            self.joins.lock().unwrap().clone()
        }

        fn installs(&self) -> u32 {
            *self.installs.lock().unwrap()
        }
    }

    fn installed_state() -> State {
        State::Installed {
            pid: Some(4242),
            unit: "/Users//alice/Library/LaunchAgents/systems.alab.aterm-secrets.plist".into(),
        }
    }

    impl SecretsOps for Scripted {
        fn status(&self) -> aterm_secrets::Result<Status> {
            let mut statuses = self.statuses.lock().unwrap();
            if statuses.len() > 1 {
                statuses.pop_front().unwrap()
            } else {
                statuses.front().cloned().expect("a scripted status")
            }
        }

        fn subscriber(&self) -> aterm_secrets::Result<State> {
            if let Some(state) = self.subscriber.lock().unwrap().clone() {
                return state;
            }
            let installed = *self.installs.lock().unwrap() > 0
                && self
                    .installed
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(Result::is_ok);
            Ok(if installed {
                installed_state()
            } else {
                State::Absent
            })
        }

        fn terminal(&self) -> bool {
            *self.terminal.lock().unwrap()
        }

        fn probe_alab(&self) -> aterm_secrets::Result<ProbeOutcome> {
            *self.probes.lock().unwrap() += 1;
            self.probed
                .lock()
                .unwrap()
                .clone()
                .expect("a scripted probe")
        }

        fn join(
            &self,
            _sys: &dyn System,
            source: &str,
            password: Option<&SecretBytes>,
            interactive: bool,
            requested: &dyn Fn() -> bool,
        ) -> aterm_secrets::Result<JoinOutcome> {
            self.joins
                .lock()
                .unwrap()
                .push((source.to_owned(), interactive));
            self.passwords.lock().unwrap().push(password.is_some());
            if *self.requests.lock().unwrap() {
                let answer = requested();
                self.hooked.lock().unwrap().push(answer);
            }
            self.installed_by_join.lock().unwrap().push(self.installs());
            self.outcome
                .lock()
                .unwrap()
                .clone()
                .expect("a scripted join")
        }

        fn install_subscriber(&self) -> aterm_secrets::Result<String> {
            *self.installs.lock().unwrap() += 1;
            let installed = self
                .installed
                .lock()
                .unwrap()
                .clone()
                .expect("a scripted install");
            if installed.is_ok() {
                *self.subscriber.lock().unwrap() = None;
            }
            installed
        }
    }

    const CODE: &str = "abcd-ef23-ghij-k234";

    fn unconfigured() -> Status {
        Status {
            config: None,
            machine: None,
            membership: Membership::Unconfigured,
            names: Vec::new(),
        }
    }

    fn configured(membership: Membership) -> Status {
        Status {
            config: Some(Config {
                source: Source::Url(aterm_secrets::ALAB_DEFAULT_URL.into()),
                name: "alab".into(),
                signer: Some([3; 32]),
                followed: Vec::new(),
                host: "m22".into(),
            }),
            machine: Some((Recipient([7; 32]), CODE.into())),
            membership,
            names: vec![
                "gh_access_token".into(),
                "gh_access_token_alabsystems".into(),
                "~/.aterm/release-credentials.toml".into(),
            ],
        }
    }

    fn fake() -> Fake {
        let mut sys = Fake::root();
        sys.euid = 501;
        sys
    }

    fn run(sys: &Fake, ops: &Arc<Scripted>, mode: Mode) -> Vec<ItemReport> {
        let context = ctx(&[user("alice", 501)]);
        let port: Arc<dyn SecretsOps> = ops.clone();
        let items = items(sys, &context, port, mode).expect("items");
        assert_eq!(items.len(), 1);
        engine::run(&items, mode, sys, &context, &mut NoJournal)
    }

    fn asked(sys: &Fake) -> usize {
        sys.calls().iter().filter(|c| c.starts_with("ask ")).count()
    }

    /// A port whose person presses Ctrl-C at the prompt `stop`: the one answer the fake
    /// cannot give by itself. Every other prompt goes to the fake.
    struct StopsAt {
        inner: Fake,
        stop: &'static str,
    }

    impl System for StopsAt {
        fn ask(&self, prompt: &str, secret: bool) -> io::Result<Option<Secret>> {
            if prompt == self.stop {
                return Err(io::ErrorKind::Interrupted.into());
            }
            self.inner.ask(prompt, secret)
        }
        fn run(&self, cmd: &Cmd) -> Result<Ran, RunFailed> {
            self.inner.run(cmd)
        }
        fn run_tty(&self, cmd: &Cmd) -> Result<Option<i32>, RunFailed> {
            self.inner.run_tty(cmd)
        }
        fn read_file(&self, path: &Path) -> io::Result<Vec<u8>> {
            self.inner.read_file(path)
        }
        fn write_file(&self, path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
            self.inner.write_file(path, bytes, mode)
        }
        fn write_user_file(
            &self,
            user: &User,
            path: &Path,
            bytes: &[u8],
            mode: u32,
        ) -> io::Result<()> {
            self.inner.write_user_file(user, path, bytes, mode)
        }
        fn remove(&self, path: &Path) -> io::Result<()> {
            self.inner.remove(path)
        }
        fn create_dir_all(&self, path: &Path, mode: u32) -> io::Result<()> {
            self.inner.create_dir_all(path, mode)
        }
        fn chown(&self, path: &Path, uid: u32, gid: u32) -> io::Result<()> {
            self.inner.chown(path, uid, gid)
        }
        fn stat(&self, path: &Path) -> io::Result<Stat> {
            self.inner.stat(path)
        }
        fn list_dir(&self, path: &Path) -> io::Result<Vec<String>> {
            self.inner.list_dir(path)
        }
        fn sha256(&self, path: &Path) -> io::Result<String> {
            self.inner.sha256(path)
        }
        fn copy_file(&self, from: &Path, to: &Path, mode: u32) -> io::Result<()> {
            self.inner.copy_file(from, to, mode)
        }
        fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
            self.inner.rename(from, to)
        }
        fn euid(&self) -> u32 {
            self.inner.euid()
        }
        fn now(&self) -> u64 {
            self.inner.now()
        }
    }

    #[test]
    fn a_mac_already_in_the_vault_is_ok_without_a_prompt_or_a_join() {
        for mode in [Mode::Apply, Mode::Plan] {
            let sys = fake();
            let ops = Scripted::new(vec![Ok(configured(Membership::In {
                seq: 7,
                heard: 1_800_000_000,
            }))]);
            ops.subscribed(Ok(installed_state()));
            let rows = run(&sys, &ops, mode);
            assert_eq!(rows[0].outcome, Outcome::Ok, "{rows:?}");
            assert_eq!(rows[0].id, "init.secrets@alice");
            assert_eq!(rows[0].family, "secrets");
            assert_eq!(
                rows[0].to.as_deref(),
                Some(
                    "in vault alab (seq 7): 2 secrets in ~/.secrets and 1 elsewhere in ~, kept in sync by the subscriber"
                )
            );
            assert_eq!(asked(&sys), 0);
            assert_eq!(ops.probes(), 0);
            assert!(ops.joins().is_empty());
            assert_eq!(ops.installs(), 0);
        }
    }

    #[test]
    fn a_configured_mac_without_its_subscriber_is_a_change_that_only_installs_it() {
        for mode in [Mode::Plan, Mode::Apply] {
            let sys = fake();
            let mut status = configured(Membership::In { seq: 7, heard: 0 });
            status.names.truncate(1);
            let ops = Scripted::new(vec![Ok(status)]);
            let rows = run(&sys, &ops, mode);
            let want = if mode == Mode::Plan {
                Outcome::Planned
            } else {
                Outcome::Changed
            };
            assert_eq!(rows[0].outcome, want, "{rows:?}");
            assert_eq!(
                rows[0].from.as_deref(),
                Some("in vault alab (seq 7), subscriber not installed")
            );
            assert_eq!(rows[0].to.as_deref(), Some(SUBSCRIBED));
            assert_eq!(asked(&sys), 0);
            assert_eq!(ops.probes(), 0);
            assert!(ops.joins().is_empty(), "nothing is joined again");
            assert_eq!(ops.installs(), u32::from(mode == Mode::Apply));
        }

        // A unit with no process (a plist launchd never loaded, or a job that keeps ending)
        // syncs nothing either: a change, whose apply installs and starts it again.
        for mode in [Mode::Plan, Mode::Apply] {
            let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
            let loaded = State::Installed {
                pid: None,
                unit: "/Users//alice/Library/LaunchAgents/systems.alab.aterm-secrets.plist".into(),
            };
            ops.subscribed(Ok(loaded));
            let rows = run(&fake(), &ops, mode);
            let want = if mode == Mode::Plan {
                Outcome::Planned
            } else {
                Outcome::Changed
            };
            assert_eq!(rows[0].outcome, want, "{rows:?}");
            assert_eq!(
                rows[0].from.as_deref(),
                Some("in vault alab (seq 7), subscriber installed but not running")
            );
            assert_eq!(rows[0].to.as_deref(), Some(SUBSCRIBED));
            assert!(ops.joins().is_empty());
            assert_eq!(ops.installs(), u32::from(mode == Mode::Apply));
        }

        // A Mac waiting for an admin gets its subscriber and still waits.
        let sys = fake();
        let ops = Scripted::new(vec![Ok(configured(Membership::Pending))]);
        let rows = run(&sys, &ops, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
        let why = rows[0].why.as_deref().unwrap();
        assert!(
            why.starts_with("waiting for an admin: approve this Mac on an admin Mac: aterm secrets approve abcd-ef23-ghij-k234"),
            "{why}"
        );
        assert!(ops.joins().is_empty());
        assert_eq!(ops.installs(), 1);

        // An install that fails is the row's reason.
        let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
        *ops.installed.lock().unwrap() = Some(Err(aterm_secrets::Error::new(
            "/bin/launchctl bootstrap: exit 5",
        )));
        let rows = run(&fake(), &ops, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Failed, "{rows:?}");
        assert_eq!(
            rows[0].why.as_deref(),
            Some("the subscriber could not be installed: /bin/launchctl bootstrap: exit 5")
        );

        // A subscriber whose state cannot be read is unknown, never ok.
        let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
        ops.subscribed(Err(aterm_secrets::Error::new(
            "/bin/launchctl: hung for 30 s; killed",
        )));
        let rows = run(&fake(), &ops, Mode::Plan);
        assert_eq!(rows[0].outcome, Outcome::Unknown, "{rows:?}");
        assert!(
            rows[0]
                .why
                .as_deref()
                .unwrap()
                .starts_with("cannot read whether the subscriber runs")
        );
    }

    #[test]
    fn enter_at_the_prompt_joins_the_alab_default_and_installs_the_subscriber() {
        for typed in [None, Some(""), Some("   ")] {
            let sys = fake();
            sys.answer(typed);
            // Enter at the join password prompt too: approval by code.
            sys.answer(None);
            let ops = Scripted::new(vec![
                Ok(unconfigured()),
                Ok(unconfigured()),
                Ok(configured(Membership::In {
                    seq: 7,
                    heard: 1_800_000_000,
                })),
            ]);
            let rows = run(&sys, &ops, Mode::Apply);
            assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
            assert_eq!(rows[0].from.as_deref(), Some("no vault"));
            assert_eq!(rows[0].to.as_deref(), Some(aterm_secrets::ALAB_DEFAULT_URL));
            assert_eq!(
                ops.joins(),
                [(aterm_secrets::ALAB_DEFAULT_URL.to_owned(), true)]
            );
            assert_eq!(ops.probes(), 1, "the ALab vault was looked for first");
            assert_eq!(ops.installs(), 1);
            assert_eq!(
                sys.calls(),
                [
                    format!("ask {PROMPT}"),
                    format!("ask secret {PASSWORD_PROMPT}")
                ]
            );
            assert_eq!(*ops.passwords.lock().unwrap(), [false]);
        }
    }

    #[test]
    fn the_alab_vault_answering_is_joined_asking_only_for_the_join_password() {
        // Built at run time, never a literal.
        let password = ["pass", "-", &"Zq".repeat(8)].concat();
        let sys = fake();
        sys.answer(Some(&password));
        let ops = Scripted::new(vec![
            Ok(unconfigured()),
            Ok(unconfigured()),
            Ok(configured(Membership::In {
                seq: 8,
                heard: 1_800_000_000,
            })),
        ]);
        ops.alab_answers();
        // The password proof gets this Mac in during the join's wait.
        *ops.requests.lock().unwrap() = true;
        let rows = run(&sys, &ops, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
        assert_eq!(rows[0].to.as_deref(), Some(aterm_secrets::ALAB_DEFAULT_URL));
        assert_eq!(
            ops.joins(),
            [(aterm_secrets::ALAB_DEFAULT_URL.to_owned(), true)]
        );
        assert_eq!(*ops.passwords.lock().unwrap(), [true]);
        assert_eq!(
            sys.calls(),
            [format!("ask secret {PASSWORD_PROMPT}")],
            "no URL prompt"
        );
        assert_eq!(
            ops.installs(),
            1,
            "installed once, when the request was queued"
        );
        assert_eq!(*ops.installed_by_join.lock().unwrap(), [1]);
        assert_eq!(*ops.hooked.lock().unwrap(), [true]);
        let report = aterm_json::to_string(&rows).unwrap();
        assert!(!report.contains(&password), "{report}");

        // No vault there, something else answering, or nothing answering falls back to the
        // URL prompt.
        for probed in [
            Ok(ProbeOutcome::Absent),
            Ok(ProbeOutcome::NotThisVault(
                "envelope: signed by a key the pin does not endorse".into(),
            )),
            Err(aterm_secrets::Error::new(
                "alab-secrets.local: no such host",
            )),
        ] {
            let sys = fake();
            sys.answer(Some("none"));
            let ops = Scripted::new(vec![Ok(unconfigured())]);
            *ops.probed.lock().unwrap() = Some(probed);
            let rows = run(&sys, &ops, Mode::Apply);
            assert_eq!(rows[0].outcome, Outcome::NotApplicable, "{rows:?}");
            assert_eq!(sys.calls(), [format!("ask {PROMPT}")]);
            assert!(ops.joins().is_empty());
        }
    }

    #[test]
    fn a_typed_source_is_joined_as_typed_and_shown_without_credentials() {
        let sys = fake();
        let url = "http://vault.example:7484/v1/lab?token=private-query#private-pin";
        sys.answer(Some(url));
        sys.answer(None);
        let ops = Scripted::new(vec![
            Ok(unconfigured()),
            Ok(unconfigured()),
            Ok(configured(Membership::In { seq: 1, heard: 0 })),
        ]);
        let rows = run(&sys, &ops, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
        assert_eq!(ops.joins(), [(url.to_owned(), true)]);
        let report = aterm_json::to_string(&rows).unwrap();
        assert!(!report.contains("private-query"), "{report}");
        assert!(!report.contains("private-pin"), "{report}");
        assert!(
            report.contains("http://vault.example:7484/v1/lab"),
            "{report}"
        );
    }

    #[test]
    fn none_or_skip_at_the_prompt_joins_nothing() {
        for typed in ["none", "SKIP", " None "] {
            let sys = fake();
            sys.answer(Some(typed));
            let ops = Scripted::new(vec![Ok(unconfigured())]);
            let rows = run(&sys, &ops, Mode::Apply);
            assert_eq!(rows[0].outcome, Outcome::NotApplicable, "{rows:?}");
            assert!(rows[0].why.as_deref().unwrap().contains("skipped"));
            assert!(ops.joins().is_empty());
            assert_eq!(ops.installs(), 0);
        }
    }

    /// On the ALab LAN the join password is the step's only question, so it is where a
    /// person who wants this Mac out of the vault says so: `none` or `skip` there joins
    /// nothing (no key, no source, no request, no subscriber), as at the vault prompt, and
    /// leaves a row that needs nothing more of anyone.
    #[test]
    fn none_or_skip_at_the_password_prompt_joins_nothing() {
        for typed in ["none", "SKIP", " None "] {
            let sys = fake();
            sys.answer(Some(typed));
            let ops = Scripted::new(vec![Ok(unconfigured())]);
            ops.alab_answers();
            let rows = run(&sys, &ops, Mode::Apply);
            assert_eq!(rows[0].outcome, Outcome::NotApplicable, "{rows:?}");
            assert_eq!(rows[0].why.as_deref(), Some(SKIPPED));
            assert_eq!(sys.calls(), [format!("ask secret {PASSWORD_PROMPT}")]);
            assert_eq!(ops.probes(), 1);
            assert!(ops.joins().is_empty(), "nothing joined for {typed:?}");
            assert_eq!(ops.installs(), 0);
        }

        // The password prompt after a URL typed at the vault prompt skips the same way.
        let sys = fake();
        sys.answer(Some("https://vault.example/v1/lab"));
        sys.answer(Some("skip"));
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        let rows = run(&sys, &ops, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::NotApplicable, "{rows:?}");
        assert!(ops.joins().is_empty());
        assert_eq!(ops.installs(), 0);
    }

    #[test]
    fn plan_and_check_never_prompt_probe_or_join_an_unconfigured_mac() {
        let sys = fake();
        // An answer is there to be taken; a plan must not take it.
        sys.answer(Some(aterm_secrets::ALAB_DEFAULT_URL));
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        ops.alab_answers();
        let rows = run(&sys, &ops, Mode::Plan);
        assert_eq!(rows[0].outcome, Outcome::NotApplicable, "{rows:?}");
        assert_eq!(rows[0].why.as_deref(), Some(UNCONFIGURED));
        assert_eq!(asked(&sys), 0);
        assert_eq!(ops.probes(), 0, "a plan never touches the network");
        assert!(ops.joins().is_empty());
        assert_eq!(ops.installs(), 0);
    }

    #[test]
    fn without_a_terminal_an_apply_says_how_to_join_and_changes_nothing() {
        let sys = fake();
        sys.answer(Some(aterm_secrets::ALAB_DEFAULT_URL));
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        ops.alab_answers();
        *ops.terminal.lock().unwrap() = false;
        let rows = run(&sys, &ops, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::NotApplicable, "{rows:?}");
        assert_eq!(rows[0].why.as_deref(), Some(UNCONFIGURED));
        assert_eq!(asked(&sys), 0);
        assert_eq!(ops.probes(), 0, "no network without a person to ask");
        assert!(ops.joins().is_empty());

        // A terminal that fails at the prompt (no answer scripted: the fake's ask fails as
        // an unreadable /dev/tty does) is no terminal either.
        let sys = fake();
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        let rows = run(&sys, &ops, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::NotApplicable, "{rows:?}");
        assert_eq!(rows[0].why.as_deref(), Some(UNCONFIGURED));
        assert_eq!(asked(&sys), 1);
        assert!(ops.joins().is_empty());
    }

    #[test]
    fn a_join_that_waits_for_an_admin_stands_aside_naming_the_code() {
        let sys = fake();
        // Enter at the URL prompt, then Enter at the password prompt: approval by code.
        sys.answer(None);
        sys.answer(None);
        let ops = Scripted::new(vec![
            Ok(unconfigured()),
            Ok(unconfigured()),
            Ok(configured(Membership::Pending)),
        ]);
        *ops.requests.lock().unwrap() = true;
        *ops.outcome.lock().unwrap() = Some(Ok(JoinOutcome::Pending {
            code: CODE.into(),
            host: "m22".into(),
        }));
        let rows = run(&sys, &ops, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
        let why = rows[0].why.as_deref().unwrap();
        assert!(why.starts_with("waiting for an admin: "), "{why}");
        assert!(
            why.contains(&format!("aterm secrets approve {CODE}")),
            "{why}"
        );
        assert_eq!(*ops.passwords.lock().unwrap(), [false]);
        // Installed when the request was queued, before the wait a Ctrl-C may end, and not
        // again after the join.
        assert_eq!(*ops.installed_by_join.lock().unwrap(), [1]);
        assert_eq!(*ops.hooked.lock().unwrap(), [true]);
        assert_eq!(ops.installs(), 1, "the subscriber finishes the join");
    }

    #[test]
    fn a_join_that_gave_up_waiting_on_a_mac_the_subscriber_let_in_is_done() {
        // The subscriber applied the approving version while the join was still waiting on
        // its own pass: the join says pending, this Mac's files say in.
        let sys = fake();
        sys.answer(None);
        let ops = Scripted::new(vec![
            Ok(unconfigured()),
            Ok(unconfigured()),
            Ok(configured(Membership::In {
                seq: 8,
                heard: 1_800_000_000,
            })),
        ]);
        ops.alab_answers();
        *ops.requests.lock().unwrap() = true;
        *ops.outcome.lock().unwrap() = Some(Ok(JoinOutcome::Pending {
            code: CODE.into(),
            host: "m22".into(),
        }));
        let rows = run(&sys, &ops, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
        assert_eq!(rows[0].why, None);
        assert_eq!(ops.installs(), 1);
    }

    #[test]
    fn ctrl_c_at_a_prompt_stops_the_run_with_nothing_joined() {
        // The ALab vault answers, so the join password prompt is the step's only question.
        let sys = StopsAt {
            inner: fake(),
            stop: PASSWORD_PROMPT,
        };
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        ops.alab_answers();
        let context = ctx(&[user("alice", 501)]);
        let port: Arc<dyn SecretsOps> = ops.clone();
        let stopped = items(&sys, &context, port, Mode::Apply).err();
        assert_eq!(stopped.as_deref(), Some(STOPPED_AT_PASSWORD));
        assert!(ops.joins().is_empty(), "no key, no source, no request");
        assert_eq!(ops.installs(), 0);

        // A URL typed at the vault prompt, then Ctrl-C at the password prompt.
        let sys = StopsAt {
            inner: fake(),
            stop: PASSWORD_PROMPT,
        };
        sys.inner.answer(Some("https://vault.example/v1/lab"));
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        let port: Arc<dyn SecretsOps> = ops.clone();
        let stopped = items(&sys, &context, port, Mode::Apply).err();
        assert_eq!(stopped.as_deref(), Some(STOPPED_AT_PASSWORD));
        assert_eq!(
            sys.inner.calls(),
            [format!("ask {PROMPT}")],
            "the vault prompt was answered"
        );
        assert!(ops.joins().is_empty());

        // Ctrl-C at the vault prompt.
        let sys = StopsAt {
            inner: fake(),
            stop: PROMPT,
        };
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        let port: Arc<dyn SecretsOps> = ops.clone();
        let stopped = items(&sys, &context, port, Mode::Apply).err();
        assert_eq!(
            stopped.as_deref(),
            Some("stopped at the secrets vault prompt; nothing was changed")
        );
        assert!(ops.joins().is_empty());

        // A path has no join queue: no password is asked for it.
        let sys = fake();
        sys.answer(Some("~/vault.txt"));
        let ops = Scripted::new(vec![
            Ok(unconfigured()),
            Ok(unconfigured()),
            Ok(configured(Membership::Plain)),
        ]);
        let rows = run(&sys, &ops, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
        assert_eq!(sys.calls(), [format!("ask {PROMPT}")]);
        assert_eq!(*ops.passwords.lock().unwrap(), [false]);
    }

    #[test]
    fn a_pending_mac_needs_an_admin_and_a_revoked_one_is_blocked() {
        let sys = fake();
        let ops = Scripted::new(vec![Ok(configured(Membership::Pending))]);
        ops.subscribed(Ok(installed_state()));
        let rows = run(&sys, &ops, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Person, "{rows:?}");
        assert_eq!(rows[0].to.as_deref(), Some("secrets.approve"));
        let why = rows[0].why.as_deref().unwrap();
        assert!(
            why.contains(
                "approve this Mac on an admin Mac: aterm secrets approve abcd-ef23-ghij-k234"
            ),
            "{why}"
        );
        // A proof that matched nothing leaves the Mac pending: the row says how to send
        // another, naming the vault unless it is the ALab default (a bare join would move
        // this Mac to that one).
        assert!(why.contains("running aterm secrets join again"), "{why}");
        assert!(ops.joins().is_empty());
        assert_eq!(ops.installs(), 0);

        let mut elsewhere = configured(Membership::Pending);
        elsewhere.config.as_mut().unwrap().source =
            Source::Url("https://vault.example/v1/lab".into());
        let ops = Scripted::new(vec![Ok(elsewhere)]);
        ops.subscribed(Ok(installed_state()));
        let rows = run(&sys, &ops, Mode::Plan);
        let why = rows[0].why.as_deref().unwrap();
        assert!(
            why.contains("running aterm secrets join <vault-url> again"),
            "{why}"
        );

        let ops = Scripted::new(vec![Ok(configured(Membership::Revoked { seq: 9 }))]);
        let rows = run(&sys, &ops, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
        assert!(rows[0].why.as_deref().unwrap().contains("seq 9"));
        assert!(ops.joins().is_empty());
        assert_eq!(ops.installs(), 0);

        let mut plain = configured(Membership::Plain);
        plain.config.as_mut().unwrap().source = Source::Path("/Volumes/Keys/vault.txt".into());
        plain.names.truncate(1);
        let ops = Scripted::new(vec![Ok(plain)]);
        ops.subscribed(Ok(installed_state()));
        let rows = run(&sys, &ops, Mode::Plan);
        assert_eq!(rows[0].outcome, Outcome::Ok, "{rows:?}");
        assert_eq!(
            rows[0].to.as_deref(),
            Some(
                "reading the plain text file /Volumes/Keys/vault.txt: 1 secret in ~/.secrets, kept in sync by the subscriber"
            )
        );
    }

    #[test]
    fn failures_report_what_failed_and_never_what_was_typed() {
        // A pasted token instead of a URL: built at run time, never a literal.
        let pasted = ["ghp", "_", &"Q7".repeat(18)].concat();
        let sys = fake();
        sys.answer(Some(&pasted));
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        let rows = run(&sys, &ops, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
        let report = aterm_json::to_string(&rows).unwrap();
        assert!(!report.contains(&pasted), "{report}");
        assert!(ops.joins().is_empty(), "nothing is joined with it");

        // A join or install error is the row's reason, as the vault worded it.
        let sys = fake();
        sys.answer(None);
        sys.answer(None);
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        *ops.outcome.lock().unwrap() = Some(Err(aterm_secrets::Error::new(
            "vault alab: the signature does not verify",
        )));
        let rows = run(&sys, &ops, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Failed, "{rows:?}");
        assert_eq!(
            rows[0].why.as_deref(),
            Some("joining the vault: vault alab: the signature does not verify")
        );
        assert_eq!(ops.installs(), 0);

        // An install that fails, after the join or when its request was queued, fails the
        // row, and is not tried twice; when the request was queued, the join is told the
        // subscriber is not in place (and the person is told at once, on the terminal).
        for requests in [false, true] {
            let sys = fake();
            sys.answer(None);
            sys.answer(None);
            let ops = Scripted::new(vec![Ok(unconfigured())]);
            *ops.requests.lock().unwrap() = requests;
            *ops.installed.lock().unwrap() = Some(Err(aterm_secrets::Error::new(
                "no systemctl at /usr/bin/systemctl",
            )));
            let rows = run(&sys, &ops, Mode::Apply);
            assert_eq!(rows[0].outcome, Outcome::Failed, "{rows:?}");
            assert_eq!(
                rows[0].why.as_deref(),
                Some(
                    "the vault answered, but the subscriber could not be installed: no systemctl at /usr/bin/systemctl"
                )
            );
            assert_eq!(ops.installs(), 1);
            let hooked: &[bool] = if requests { &[false] } else { &[] };
            assert_eq!(*ops.hooked.lock().unwrap(), hooked);
        }

        // State that cannot be read is unknown, never ok.
        let sys = fake();
        let ops = Scripted::new(vec![Err(aterm_secrets::Error::new(
            "source.toml: mode 644 lets other users read it; run chmod 600 on it",
        ))]);
        let rows = run(&sys, &ops, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Unknown, "{rows:?}");
        assert!(rows[0].outcome.is_failure());
        assert_eq!(asked(&sys), 0);
    }

    #[test]
    fn a_signer_is_trusted_only_on_yes() {
        for (typed, trusted) in [
            (Some("y"), true),
            (Some(" YES "), true),
            (Some("n"), false),
            (Some("yess"), false),
            (None, false),
        ] {
            let sys = fake();
            sys.answer(typed);
            assert_eq!(
                confirm_signer(&sys, "0123-4567-89ab-cdef"),
                trusted,
                "{typed:?}"
            );
            assert_eq!(
                sys.calls(),
                [
                    "ask The vault is signed by 0123-4567-89ab-cdef; compare it with aterm secrets status on an admin Mac. Trust it? [y/N] "
                ]
            );
        }
        // No terminal: not trusted.
        assert!(!confirm_signer(&fake(), "0123-4567-89ab-cdef"));
        assert_eq!(approval_wait(true), Duration::from_secs(600));
        assert_eq!(approval_wait(false), Duration::ZERO);
    }

    #[test]
    fn a_source_is_shown_without_its_pin_or_credentials() {
        assert_eq!(
            shown("http://alab-secrets.local:7484/v1/alab"),
            "http://alab-secrets.local:7484/v1/alab"
        );
        assert_eq!(shown("https://u:p@h/v1/x#pin"), "the vault URL");
        assert_eq!(shown("https://u@h"), "the vault URL");
        assert_eq!(shown("~/vault.txt"), "~/vault.txt");
        assert_eq!(shown("/srv/a#b.vault"), "/srv/a#b.vault");
        assert_eq!(shown("/srv/x://u@y"), "/srv/x://u@y");
        assert_eq!(shown("HTTP://h?q=private-query#private-pin"), "http://h/");
        assert_eq!(
            shown("vault.example"),
            "(the answer at the prompt, not shown)"
        );
        assert_eq!(shown("http://"), "(the answer at the prompt, not shown)");
        let private = "https://h/v1/lab?token=private-query#private-pin";
        assert_eq!(shown(private), "https://h/v1/lab");
        let hint = retype(private).expect("a URL");
        assert!(
            hint.contains("<vault-url>") && hint.contains("full configured URL"),
            "{hint}"
        );
        assert!(
            !hint.contains("private-query") && !hint.contains("private-pin"),
            "{hint}"
        );
    }

    #[test]
    fn the_run_names_exactly_one_user() {
        let sys = fake();
        let ops: Arc<dyn SecretsOps> = Scripted::new(vec![Ok(unconfigured())]);
        let two = ctx(&[user("alice", 501), user("bob", 502)]);
        assert!(items(&sys, &two, ops, Mode::Plan).is_err());
        assert!(live(&two).is_err());
    }
}
