// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm init` secrets vault discovery, join, and subscriber installation step.

use crate::items::{Ctx, Item};
use crate::model::{Mode, Probe, User};
use crate::port::System;
use aterm_converge::StoodAside;
use aterm_secrets::SecretBytes;
use aterm_secrets::client::{Config, JoinOutcome, Look, Membership, Paths, ProbeOutcome, Status};
use aterm_secrets::service::State;
use aterm_secrets::transport::Source;
use std::cell::{Cell, OnceCell};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// The item family.
pub const FAMILY: &str = "secrets";

/// The prompt that asks for the vault again when the one `--vault` names holds none.
pub const PROMPT: &str = "Secrets vault URL or path (none skips): ";

/// The prompt that asks for the vault's join password (hidden).
pub const PASSWORD_PROMPT: &str = "Vault join password (an admin Mac shows it: aterm secrets password; Enter joins without one: an open vault lets this Mac in, any other waits for approval by code; none skips): ";

/// The prompt after a join with the join password that no admin Mac approved in
/// [`JOIN_WINDOW`] (hidden).
pub const RETYPE_PROMPT: &str = "Not approved yet. Retype the join password, or press Enter to keep waiting for approval by code: ";

const SKIPPED: &str = "skipped at the prompt; aterm secrets join <url> joins a vault later";

/// What an unconfigured Mac's row says where no ALab vault answers: nothing is asked.
pub const NOT_HERE: &str = "no secrets vault on this network; aterm secrets join <url> or aterm init --vault <url> joins yours";

/// How the vault item's stand-aside begins when the vault's address answered with no vault there
/// ([`Look::NoVault`]), so a caller tells that answer from one nobody gave.
pub const NO_VAULT_NOW: &str = "no vault is there now, so nothing was changed";

/// What it says where macOS refuses aterm the local network, so no vault could be looked for.
const LAN_REFUSED: &str = "macOS does not let aterm use the local network, so no vault was looked for: turn aterm on in System Settings > Privacy & Security > Local Network, or name one with aterm init --vault <url>";

/// The one line while macOS holds aterm off the local network until a person answers its
/// question ([`local_network_pending`]).
const LOCAL_NETWORK_ASK: &str =
    "macOS is asking whether aterm may use the local network: choose Allow";

/// How many looks, a second apart, wait for that answer before init goes on without the vault:
/// a hang detector, not a budget.
const LOCAL_NETWORK_TRIES: u32 = 60;

/// What the row says when macOS still held the look once those tries were spent: nobody
/// answered its question, so whether a vault answers here is unknown and nothing is kept.
const LAN_UNANSWERED: &str = "macOS still asks whether aterm may use the local network, so no vault was looked for: choose Allow, then run aterm init again, or name one with aterm init --vault <url>";

/// How long `aterm init` waits for an admin Mac's subscriber to approve a join made with the
/// join password: two of its 2 s polls and a publish. A Mac not in by then is asked for the
/// password again ([`RETYPE_PROMPT`]), and one whose password an admin Mac refused is asked at
/// once ([`REFUSED`]); a join without one waits for nobody.
pub const JOIN_WINDOW: Duration = Duration::from_secs(6);

const STOPPED_AT_PASSWORD: &str = "stopped at the join password prompt; nothing was changed";

/// What `aterm init` says before [`RETYPE_PROMPT`] when an admin Mac's verdict refused the join
/// password it sent.
const REFUSED: &str = "an admin Mac refused that join password: it is not the vault's";

const STOPPED_AT_TRUST: &str =
    "stopped at the question whether to trust the vault's signing key; nothing was changed";

/// How long the look for the ALab vault takes at most, all of it.
pub const PROBE_WAIT: Duration = aterm_secrets::client::PROBE_DEADLINE;

/// How long the read of the vault `--vault` names waits for an answer (plus the transport's
/// margin).
pub const CHECK_WAIT: Duration = Duration::from_secs(3);

/// How long the look before a join or a sync waits for the vault ([`SecretsOps::reachable`]).
pub const REACH_WAIT: Duration = Duration::from_secs(30);

/// How long that look keeps trying while macOS holds this app's lookups and LAN connects for a
/// person's answer to its Local Network question.
pub const CONSENT_WAIT: Duration = Duration::from_secs(3 * 60);

static CONSENT_SPENT: AtomicBool = AtomicBool::new(false);

fn patient_look(
    spent: &AtomicBool,
    held: &dyn Fn(&str),
    look: impl FnOnce(Duration, &dyn Fn(&str)) -> Look,
) -> Look {
    let patience = if spent.load(Ordering::Relaxed) {
        Duration::ZERO
    } else {
        CONSENT_WAIT
    };
    let was_held = Cell::new(false);
    let told = |why: &str| {
        was_held.set(true);
        held(why);
    };
    let answer = look(patience, &told);
    if was_held.get() && matches!(answer, Look::Unreached(_) | Look::Held(_)) {
        spent.store(true, Ordering::Relaxed);
    }
    answer
}

/// What an Offline row adds when macOS's Local Network gate held the vault's connection
/// ([`Look::Held`]): a quiet pass says nothing else, so the row is where a Don't Allow is named.
const LOCAL_NETWORK: &str = "if macOS asked whether this app may find devices on your local \
     network and it was not allowed, turn it on in System Settings > Privacy & Security > Local \
     Network";

const UNCONFIGURED: &str =
    "no vault configured; run aterm init in a terminal, or aterm secrets join <url>";

const SUBSCRIBED: &str = "subscriber running (aterm secrets watch keeps ~/.secrets in sync)";

const WRITTEN: &str = "every vault file written (aterm secrets sync)";

/// The vault, as `aterm init` sees it.
pub trait SecretsOps: Send + Sync {
    /// This Mac's vault state, read from local files only.
    /// # Errors
    /// The state cannot be read; the error names no value.
    fn status(&self) -> aterm_secrets::Result<Status>;

    /// The vault this Mac is configured for now (its `source.toml`), without the rest of its
    /// state.
    /// # Errors
    /// The configuration cannot be read; the error names no value.
    fn configured(&self) -> aterm_secrets::Result<Option<Config>>;

    /// The subscriber's state (`aterm_secrets::service::state`).
    /// # Errors
    /// The service manager could not be asked.
    fn subscriber(&self) -> aterm_secrets::Result<State>;

    /// Whether a person can be asked.
    fn terminal(&self) -> bool;

    /// Whether the ALab vault answers on this network under its compiled-in signer.
    /// # Errors
    /// Nothing answered, or the server refused the GET as the vault server can (a `5xx`).
    fn probe_alab(&self) -> aterm_secrets::Result<ProbeOutcome>;

    /// What the vault `source` names holds, read once ([`CHECK_WAIT`]) and changing nothing.
    /// # Errors
    /// No vault answered there; the error says why and never repeats a credential.
    fn check(&self, source: &str) -> Result<Checked, String> {
        let _ = source;
        Err("this port reads no vault source".into())
    }

    /// What the vault at `source` answers now.
    fn reachable(&self, source: &Source) -> Look;

    /// Copy this Mac's vault files (`source.toml`, `state.toml`) into `into` before a join
    /// replaces them, admitting only `replaces`, under the snapshot's state lock.
    /// # Errors
    /// A file could not be read or copied; the join is then not made.
    fn keep(&self, into: &Path, replaces: &Replaces) -> aterm_secrets::Result<()>;

    /// Join the vault at `source.0` (a URL, `#<signer>` pin allowed, or a path), replacing only
    /// the configuration `source.1` admits, under the client's write lock.
    /// # Errors
    /// The join failed; the error names no value.
    fn join(
        &self,
        sys: &dyn System,
        source: (&str, &Replaces),
        password: Option<&SecretBytes>,
        interactive: bool,
        wait: Duration,
        requested: &dyn Fn() -> bool,
    ) -> aterm_secrets::Result<JoinOutcome>;

    /// Wait up to `window` for an admin to let this Mac into the vault it asked to join, ending
    /// sooner once `stopped` says so or an admin Mac refused the join password it sent (said as
    /// [`REFUSED`]); whether it is in.
    fn await_approval(&self, window: Duration, stopped: &dyn Fn() -> bool) -> bool {
        let _ = (window, stopped);
        matches!(
            self.status(),
            Ok(Status {
                membership: Membership::In { .. },
                ..
            })
        )
    }

    /// Install (or replace) and start the subscriber (`aterm secrets watch`).
    /// # Errors
    /// The job could not be installed or started.
    fn install_subscriber(&self) -> aterm_secrets::Result<String>;

    /// Install a missing subscriber unless this login turned sync off; `None` means it was left
    /// off. The live port orders the check and install with a turn-off
    /// (`aterm_secrets::service::repair_subscriber`).
    fn repair_subscriber(&self) -> aterm_secrets::Result<Option<String>> {
        if self.turned_off() {
            return Ok(None);
        }
        self.install_subscriber().map(Some)
    }

    /// Whether the person turned the subscriber off
    /// (`aterm_secrets::service::subscriber_turned_off`): their recorded choice, which System
    /// State's setup installs nothing over.
    fn turned_off(&self) -> bool;

    /// Wait for a subscriber System State's setup just installed to keep running, read again
    /// through launchd's restart window (`aterm_secrets::service::back_within_restart_window`).
    /// # Errors
    /// It does not keep running (the error names its log), or cannot be read.
    fn started(&self) -> Result<(), String>;

    /// The running aterm when it is a copy that goes away when it quits.
    fn fleeting(&self) -> Option<PathBuf>;

    /// Whether this Mac's last sync wrote every file and each still holds what it wrote.
    /// # Errors
    /// The state cannot be read.
    fn synced(&self) -> aterm_secrets::Result<bool>;

    /// Fetch and apply the vault once (`aterm_secrets::client::sync`, no long-poll), writing
    /// again a file that failed, was deleted or was changed.
    /// # Errors
    /// The vault could not be fetched or applied; the error names no value.
    fn sync(&self) -> aterm_secrets::Result<()>;

    /// Say one progress line (never a value) to the person: `aterm init`'s terminal.
    fn say(&self, line: &str);

    /// Pause between two looks while macOS holds this app off the local network.
    fn pause(&self, pause: Duration) {
        std::thread::sleep(pause);
    }
}

/// What the vault a person names holds ([`SecretsOps::check`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checked {
    /// A vault whose signer the source pins (its `#<signer>`, or the key built into aterm for
    /// the ALab vault), or a plain text file.
    Pinned,
    /// A vault signed by a key nothing pins: its fingerprint, and the `#<signer>` pin a join
    /// takes once the person trusts it.
    Unpinned {
        /// What the person compares with `aterm secrets status` on an admin Mac.
        fingerprint: String,
        /// The signer in the pin's base64.
        pin: String,
    },
}

/// The look for the ALab vault `aterm init` starts before it reads anything else of the Mac
/// ([`Lookahead::start`]), so the look overlaps that reading instead of preceding the rest of
/// the run; the secrets step takes its answer ([`LiveSecrets::looking`]).
pub struct Lookahead(Mutex<Option<std::thread::JoinHandle<aterm_secrets::Result<ProbeOutcome>>>>);

impl Lookahead {
    /// Start the look for the login whose home is `home` ([`look_here`]).
    #[must_use]
    pub fn start(home: &Path) -> Self {
        let home = home.to_path_buf();
        let look = std::thread::Builder::new()
            .name("aterm-init-vault-look".into())
            .spawn(move || look_here(&home))
            .ok();
        Self(Mutex::new(look))
    }

    /// The look's answer, once: `None` when it was taken already, or its thread did not start
    /// or panicked.
    fn take(&self) -> Option<aterm_secrets::Result<ProbeOutcome>> {
        let look = self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()?;
        look.join().ok()
    }
}

impl std::fmt::Debug for Lookahead {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Lookahead").finish_non_exhaustive()
    }
}

/// Whether a person can be asked: this process has a terminal.
fn at_terminal() -> bool {
    use std::io::IsTerminal as _;
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .is_ok_and(|tty| tty.is_terminal())
}

/// Start the look for the ALab vault that this login's `aterm init` apply would make
/// ([`items`]): for a person at this Mac's console ([`crate::console::here`]: the login that
/// owns it, in its own session, at a terminal, not started by a coding agent), with no vault
/// configured. `None` where the secrets step asks nothing of the network: a login away from
/// the console (over SSH, or while nobody, or someone else, is logged in at the Mac) sets up no
/// workstation layer, and an agent's run or one without a terminal only plans the vault's
/// step, so a look there would only cost a request and a record nobody reads.
#[must_use]
pub fn look_ahead() -> Option<Arc<Lookahead>> {
    if crate::console::here() != crate::console::Presence::AtConsole {
        return None;
    }
    let paths = Paths::live().ok()?;
    let unconfigured = matches!(aterm_secrets::client::config(&paths), Ok(None));
    unconfigured.then(|| Arc::new(Lookahead::start(&paths.home)))
}

/// One look for the ALab vault from the login whose home is `home`. A look that heard another
/// vault answer on this network within [`aterm_secrets::heard::ABSENT_FOR`] stands for it, so
/// no request is made. Otherwise the vault is asked ([`PROBE_WAIT`]) and a definitive answer
/// (the vault, or another vault's signed envelope) is kept for the next run on this network;
/// anything else (a timeout, a refused connect, a server holding no vault yet, an answer that
/// is no vault, macOS's Local Network privacy) is not. A look that heard the vault spares no
/// later look: it answers within a fraction of a second where it is.
fn look_here(home: &Path) -> aterm_secrets::Result<ProbeOutcome> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    look_here_with(home, aterm_secrets::heard::network(), now, || {
        aterm_secrets::client::probe_default(PROBE_WAIT)
    })
}

/// [`look_here`] on `network` at `now`, asking the vault with `probe`.
fn look_here_with(
    home: &Path,
    network: Option<String>,
    now: u64,
    probe: impl FnOnce() -> aterm_secrets::Result<ProbeOutcome>,
) -> aterm_secrets::Result<ProbeOutcome> {
    use aterm_secrets::heard::{self, Heard, Verdict};
    let paths = Paths::for_home(home);
    if let Some(network) = &network
        && heard::read(&paths).and_then(|kept| kept.standing(network, now)) == Some(Heard::Absent)
    {
        return Ok(ProbeOutcome::Absent);
    }
    let probed = probe();
    // Only a definitive answer is kept: the vault, or another vault's signed envelope at its
    // address. A 404 (a server whose store holds no vault yet, moving or new), an answer that
    // is no vault (an error page or redirect from a server mid-restart), a refused or
    // timed-out connect (its Mac asleep) and a lookup macOS holds while its Local Network
    // question is open prove nothing about this network, and a record of them would skip the
    // vault here for a day.
    let heard = match &probed {
        Ok(ProbeOutcome::Found(_)) => Some(Heard::Found),
        Ok(ProbeOutcome::AnotherVault(_)) => Some(Heard::Absent),
        Ok(ProbeOutcome::Absent | ProbeOutcome::NotThisVault(_)) | Err(_) => None,
    };
    if let (Some(heard), Some(network)) = (heard, network) {
        // The record only spares a later look: one not written costs that look, nothing else.
        let _ = heard::write(
            &paths,
            &Verdict {
                at: now,
                heard,
                network,
            },
        );
    }
    probed
}

/// Whether a failed look is macOS holding this app off the local network until a person
/// answers its question: a connect to the address the vault's `.local` name resolved to that
/// fails "no route" (`EHOSTUNREACH`, or `ENETUNREACH`), as macOS fails it for an app it has not
/// let onto the LAN yet. A refusal ([`aterm_secrets::client::refused_by_local_network`]) is the
/// answer Don't Allow, not a pending one. The transport keeps only its error's text, which
/// carries the errno's own words.
fn local_network_pending(why: &aterm_secrets::Error) -> bool {
    !aterm_secrets::client::refused_by_local_network(why)
        && [libc::EHOSTUNREACH, libc::ENETUNREACH]
            .into_iter()
            .any(|errno| {
                why.0
                    .contains(&io::Error::from_raw_os_error(errno).to_string())
            })
}

/// The live vault of one user.
#[derive(Debug, Clone)]
pub struct LiveSecrets {
    home: PathBuf,
    /// Nothing is said ([`SecretsOps::say`]): System State's unattended pass.
    quiet: bool,
    /// The look `aterm init` started first, which [`SecretsOps::probe_alab`] answers with once.
    ahead: Option<Arc<Lookahead>>,
    /// When this port's last join request was queued: the vault refuses a changed request
    /// sooner than [`aterm_secrets::server::JOIN_CHANGE_GAP`] after it, so a retyped password
    /// waits out the rest of that gap first.
    asked: Arc<Mutex<Option<Instant>>>,
    /// Where a test hears what is said, instead of stderr.
    #[cfg(test)]
    heard: Option<Arc<Mutex<Vec<String>>>>,
}

impl LiveSecrets {
    /// The vault of the user whose home is `home`, saying its progress on stderr.
    #[must_use]
    pub fn new(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
            quiet: false,
            ahead: None,
            asked: Arc::default(),
            #[cfg(test)]
            heard: None,
        }
    }

    /// This vault, answering its first look for the ALab vault with `ahead`'s.
    #[must_use]
    pub fn looking(self, ahead: Option<Arc<Lookahead>>) -> Self {
        Self { ahead, ..self }
    }

    /// The vault of the user whose home is `home`, saying nothing.
    #[must_use]
    pub fn unattended(home: &Path) -> Self {
        Self {
            quiet: true,
            ..Self::new(home)
        }
    }

    fn paths(&self) -> Paths {
        Paths::for_home(&self.home)
    }

    fn with_join_options(
        &self,
        sys: &dyn System,
        (source, replaces): (&str, &Replaces),
        password: Option<&SecretBytes>,
        interactive: bool,
        requested: &dyn Fn(),
        join: impl FnOnce(&aterm_secrets::client::JoinOptions<'_>) -> aterm_secrets::Result<JoinOutcome>,
    ) -> aterm_secrets::Result<JoinOutcome> {
        let confirm = |fingerprint: &str| confirm_signer(sys, fingerprint);
        // A URL vault's join says nothing on its own: the row, and the code a join without the
        // password waits on, are the person's lines. A path vault has no join queue, so its
        // line names the key an admin approves.
        let say = |event: &aterm_secrets::client::JoinEvent<'_>| {
            if !is_url(source) {
                self.say(&event.to_string());
            }
        };
        // Retrying this membership's password does not rename its machine. A
        // different vault, or a fresh join, still asks under the default name.
        let host = match replaces {
            Replaces::Any => None,
            Replaces::Read(configured) => selected_vault(source, &self.home, configured.as_ref())
                .map_err(aterm_secrets::Error::new)?
                .map(|configured| configured.host.clone()),
        };
        let options = aterm_secrets::client::JoinOptions {
            source: source.to_owned(),
            host,
            interactive,
            wait: Duration::ZERO,
            confirm: &confirm,
            password: aterm_secrets::client::JoinPassword::Given(password.cloned()),
            fetched: None,
            requested,
            say: &say,
        };
        join(&options)
    }
}

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
    let joined = || match aterm_secrets::client::status(paths) {
        Ok(Status {
            membership: Membership::In { seq, .. },
            names,
            ..
        }) => Some(JoinOutcome::Joined { seq, names }),
        _ => None,
    };
    // The subscriber the join started can finish it before this wait begins: a sync
    // then long-polls for a version newer than the one this Mac already holds.
    if let Some(joined) = joined() {
        return Some(joined);
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
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        let started = Instant::now();
        let left = deadline
            .saturating_duration_since(started)
            .min(Duration::from_secs(50));
        if let Err(error) = aterm_secrets::client::sync_within(paths, left) {
            say(&format!("still waiting ({error})"));
        }
        if let Some(joined) = joined() {
            return Some(joined);
        }
        let pause = Duration::from_secs(5).saturating_sub(started.elapsed());
        std::thread::sleep(pause.min(deadline.saturating_duration_since(Instant::now())));
    }
    None
}

/// How long one long-poll of [`approved_within`] holds, and its read of a verdict after it, so a
/// caught Ctrl-C ends the wait within about twice that.
const APPROVAL_SLICE: Duration = Duration::from_secs(1);

/// Wait up to `window` for this Mac to be let into the vault it asked to join, in long-polls
/// of at most [`APPROVAL_SLICE`], ending sooner once `stopped` says so, or once an admin Mac's
/// verdict says the join password it sent is not the vault's
/// ([`aterm_secrets::client::read_verdict`]), which no wait changes; whether it is in.
fn approved_within(paths: &Paths, window: Duration, stopped: &dyn Fn() -> bool) -> bool {
    let inside = || {
        matches!(
            aterm_secrets::client::status(paths),
            Ok(Status {
                membership: Membership::In { .. },
                ..
            })
        )
    };
    let deadline = Instant::now() + window;
    while !inside() {
        let started = Instant::now();
        if stopped() || started >= deadline {
            return false;
        }
        let slice = deadline
            .saturating_duration_since(started)
            .min(APPROVAL_SLICE);
        // A sync that failed is the next slice's to retry; the window bounds them all.
        let _ = aterm_secrets::client::sync_within(paths, slice);
        if inside() {
            return true;
        }
        if stopped() {
            return false;
        }
        // The verdict also ends the long-poll, so it is read as soon as it is kept, within
        // what is left of the window (at most a slice), as the long-poll took its own.
        let left = deadline
            .saturating_duration_since(Instant::now())
            .min(APPROVAL_SLICE);
        if !left.is_zero()
            && aterm_secrets::client::read_verdict_within(paths, left).unwrap_or(false)
        {
            return false;
        }
        std::thread::sleep(slice.saturating_sub(started.elapsed()));
    }
    true
}

impl SecretsOps for LiveSecrets {
    fn status(&self) -> aterm_secrets::Result<Status> {
        aterm_secrets::client::status(&self.paths())
    }

    fn configured(&self) -> aterm_secrets::Result<Option<Config>> {
        aterm_secrets::client::config(&self.paths())
    }

    fn subscriber(&self) -> aterm_secrets::Result<State> {
        aterm_secrets::service::state(&self.home, aterm_secrets::service::Job::Subscriber)
    }

    fn terminal(&self) -> bool {
        at_terminal()
    }

    fn probe_alab(&self) -> aterm_secrets::Result<ProbeOutcome> {
        match self.ahead.as_ref().and_then(|ahead| ahead.take()) {
            Some(looked) => looked,
            None => look_here(&self.home),
        }
    }

    fn check(&self, source: &str) -> Result<Checked, String> {
        use aterm_secrets::transport::Fetched;
        let (parsed, pin) = aterm_secrets::transport::parse_source(source, &self.home)
            .map_err(|why| why.to_string())?;
        let name = aterm_secrets::transport::vault_name(&parsed).map_err(|why| why.to_string())?;
        let at = shown(&source_text(&parsed));
        let bytes = match aterm_secrets::transport::fetch(&parsed, None, CHECK_WAIT)
            .map_err(|why| why.to_string())?
        {
            Fetched::Body(bytes) => bytes,
            Fetched::Missing => return Err(format!("{at}: no vault there")),
            Fetched::NoNewer(_) => return Err(format!("{at} answered with no vault")),
        };
        if !bytes.starts_with(aterm_secrets::envelope::MAGIC.as_bytes()) {
            return match parsed {
                Source::Path(_) => Ok(Checked::Pinned),
                Source::Url(_) => Err(format!("{at} does not serve an aterm secrets vault")),
            };
        }
        let envelope = aterm_secrets::envelope::parse(&bytes).map_err(|why| why.to_string())?;
        envelope.verify_signature().map_err(|why| why.to_string())?;
        // A pin the vault does not answer to is said here, where the person can fix it, rather
        // than by the join after the base layer.
        if let Some(pin) = &pin {
            aterm_secrets::envelope::verify_for_reader(&bytes, &name, pin, &[], None)
                .map_err(|why| format!("{at}: {why}"))?;
        }
        let alab = matches!(&parsed, Source::Url(url) if url == aterm_secrets::ALAB_DEFAULT_URL);
        Ok(if pin.is_some() || alab {
            Checked::Pinned
        } else {
            Checked::Unpinned {
                fingerprint: aterm_secrets::envelope::fingerprint(&envelope.signer),
                pin: aterm_secrets::b64::encode_padded(&envelope.signer),
            }
        })
    }

    fn reachable(&self, source: &Source) -> Look {
        let held = |why: &str| {
            self.say(&format!(
                "the vault does not answer yet ({why}); if macOS asks whether this app may find devices on your local network, Allow lets this Mac reach it; aterm keeps trying for up to {} minutes",
                CONSENT_WAIT.as_secs() / 60
            ));
        };
        patient_look(&CONSENT_SPENT, &held, |patience, held| {
            aterm_secrets::client::look(source, REACH_WAIT, patience, held)
        })
    }

    fn keep(&self, into: &Path, replaces: &Replaces) -> aterm_secrets::Result<()> {
        match replaces {
            Replaces::Any => aterm_secrets::client::keep_config(&self.paths(), into),
            Replaces::Read(expected) => aterm_secrets::client::keep_config_if_unchanged(
                &self.paths(),
                into,
                expected.as_ref(),
            ),
        }
    }

    fn join(
        &self,
        sys: &dyn System,
        (source, replaces): (&str, &Replaces),
        password: Option<&SecretBytes>,
        interactive: bool,
        wait: Duration,
        requested: &dyn Fn() -> bool,
    ) -> aterm_secrets::Result<JoinOutcome> {
        let paths = self.paths();
        let say = |line: &str| self.say(line);
        let mistyped = password.and_then(|_| retype(source));
        let gap = aterm_secrets::server::JOIN_CHANGE_GAP;
        let last = *self.asked.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(since) = last.map(|asked| asked.elapsed())
            && since < gap
        {
            std::thread::sleep(gap - since);
        }
        let subscribed = Cell::new(false);
        let queued = || {
            *self.asked.lock().unwrap_or_else(PoisonError::into_inner) = Some(Instant::now());
            subscribed.set(requested());
        };
        let outcome = self.with_join_options(
            sys,
            (source, replaces),
            password,
            interactive,
            &queued,
            |options| match replaces {
                Replaces::Any => aterm_secrets::client::join(&paths, options),
                Replaces::Read(expected) => {
                    aterm_secrets::client::join_if_unchanged(&paths, options, expected.as_ref())
                }
            },
        )?;
        match outcome {
            JoinOutcome::Pending { code, host } => {
                Ok(
                    wait_for_approval(&paths, wait, subscribed.get(), mistyped.as_deref(), &say)
                        .unwrap_or(JoinOutcome::Pending { code, host }),
                )
            }
            done => Ok(done),
        }
    }

    fn await_approval(&self, window: Duration, stopped: &dyn Fn() -> bool) -> bool {
        let paths = self.paths();
        let inside = approved_within(&paths, window, stopped);
        if !inside && aterm_secrets::client::password_refused(&paths) {
            self.say(REFUSED);
        }
        inside
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

    fn repair_subscriber(&self) -> aterm_secrets::Result<Option<String>> {
        if self.turned_off() {
            return Ok(None);
        }
        use aterm_secrets::service::{self, Job};
        let program = match service::kept_program(&self.home, Job::Subscriber) {
            Some(program) => program,
            None => service::program_for_service()?,
        };
        service::repair_subscriber(
            &self.home,
            &program,
            &["secrets".to_owned(), "watch".to_owned()],
        )
    }

    fn turned_off(&self) -> bool {
        aterm_secrets::service::subscriber_turned_off(&self.home)
    }

    fn started(&self) -> Result<(), String> {
        let unread =
            |why: aterm_secrets::Error| format!("cannot read whether the subscriber runs: {why}");
        let mut running = || self.subscriber().map(|state| idle(&state).is_none());
        let up = match running() {
            Ok(true) => true,
            Ok(false) => aterm_secrets::service::back_within_restart_window(
                &mut None,
                &mut Instant::now,
                &mut running,
                &mut std::thread::sleep,
            )
            .map_err(unread)?,
            Err(why) => return Err(unread(why)),
        };
        if up {
            return Ok(());
        }
        Err(format!(
            "the secrets subscriber was installed but does not keep running; its log is {}",
            aterm_secrets::service::log_path(&self.home, aterm_secrets::service::Job::Subscriber)
                .display()
        ))
    }

    fn fleeting(&self) -> Option<PathBuf> {
        std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .ok()
            .filter(|exe| aterm_secrets::service::transient(exe))
    }

    fn synced(&self) -> aterm_secrets::Result<bool> {
        aterm_secrets::client::materialization_ready(&self.paths())
    }

    fn sync(&self) -> aterm_secrets::Result<()> {
        aterm_secrets::client::sync(&self.paths(), None).map(drop)
    }

    fn say(&self, line: &str) {
        #[cfg(test)]
        if let Some(heard) = &self.heard {
            heard
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(line.to_owned());
            return;
        }
        if !self.quiet {
            eprintln!("aterm init: secrets: {line}");
        }
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

/// Ask the person for the vault's join password ([`PASSWORD_PROMPT`], no echo), blanks around
/// it dropped.
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

/// Ask the person whether to trust a vault signer seen for the first time: `y` or `yes` trusts
/// it.
pub fn confirm_signer(sys: &dyn System, fingerprint: &str) -> bool {
    trusts(sys, fingerprint).unwrap_or(false)
}

/// [`confirm_signer`]'s question.
/// # Errors
/// The read failed; [`io::ErrorKind::Interrupted`] when the person stopped it (Ctrl-C).
fn trusts(sys: &dyn System, fingerprint: &str) -> io::Result<bool> {
    let question = format!(
        "The vault is signed by {fingerprint}; compare it with aterm secrets status on an admin Mac. Trust it? [y/N] "
    );
    Ok(sys.ask(&question, false)?.is_some_and(|answer| {
        let answer = answer.bytes().trim_ascii();
        answer.eq_ignore_ascii_case(b"y") || answer.eq_ignore_ascii_case(b"yes")
    }))
}

/// The live port for the one user `aterm init` configures, answering its first look for the
/// ALab vault with `ahead`'s ([`Lookahead`]).
/// # Errors
/// The run does not name exactly one user.
pub fn live(ctx: &Ctx, ahead: Option<Arc<Lookahead>>) -> Result<Arc<dyn SecretsOps>, String> {
    let user = only_user(ctx)?;
    Ok(Arc::new(
        LiveSecrets::new(Path::new(&user.home)).looking(ahead),
    ))
}

/// The live port for System State's unattended Secrets stage: [`LiveSecrets::unattended`].
/// # Errors
/// The run does not name exactly one user.
pub fn unattended(ctx: &Ctx) -> Result<Arc<dyn SecretsOps>, String> {
    let user = only_user(ctx)?;
    Ok(Arc::new(LiveSecrets::unattended(Path::new(&user.home))))
}

fn only_user(ctx: &Ctx) -> Result<&User, String> {
    match ctx.users.as_slice() {
        [user] => Ok(user),
        _ => Err("aterm init secrets requires exactly one local user".into()),
    }
}

/// The configured vault (`configured`) when it is the one `source` names.
/// # Errors
/// `source` is not a vault source; the error does not repeat it.
pub fn selected_vault<'a>(
    source: &str,
    home: &Path,
    configured: Option<&'a Config>,
) -> Result<Option<&'a Config>, String> {
    let (source, pin) = aterm_secrets::transport::parse_source(source, home)
        .map_err(|_| "the selected vault source is invalid")?;
    let name = aterm_secrets::transport::vault_name(&source)
        .map_err(|_| "the selected vault source needs a valid vault name")?;
    Ok(configured.filter(|config| {
        config.source == source
            && config.name == name
            && aterm_secrets::client::pin_admitted(config, pin)
    }))
}

/// Which vault configuration a selected vault's join may replace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Replaces {
    /// Whichever its probe reads.
    Any,
    /// Only the one the caller read (`None`: no vault configured) when it chose this try on
    /// that read.
    Read(Option<Config>),
}

fn configured_text(config: Option<&Config>) -> String {
    match config {
        None => "no vault".into(),
        Some(config) => format!(
            "vault {} ({})",
            config.name,
            shown(&source_text(&config.source))
        ),
    }
}

/// The vault item of System State's Secrets stage.
/// # Errors
/// The run does not name exactly one user.
pub(crate) fn selected(
    ctx: &Ctx,
    ops: Arc<dyn SecretsOps>,
    source: &str,
    wait: Duration,
    replaces: Replaces,
) -> Result<Vault, String> {
    let user = only_user(ctx)?.clone();
    let mut vault = Vault::new(user, ops, source.to_owned(), Origin::Selected, None, wait);
    vault.replaces = replaces;
    Ok(vault)
}

/// The secrets step of one `aterm init` run ([`items`]).
pub struct Secrets {
    /// Its item.
    pub items: Vec<Box<dyn Item>>,
    /// The item a GitHub credential waits for ([`crate::init_github::items`]): this run joins
    /// a vault, or this Mac waits to be let into one, so the token file the vault writes may
    /// not be there yet.
    pub waited_on: Option<String>,
}

impl Secrets {
    fn note(user: User, probe: Probe) -> Self {
        Self {
            items: vec![Box::new(Note::new(user, probe))],
            waited_on: None,
        }
    }

    fn not_applicable(user: User, why: &str) -> Self {
        Self::note(user, Probe::NotApplicable { why: why.into() })
    }
}

/// What the look for the ALab vault decided ([`look`]).
enum Looked {
    /// It answers: its join password is the one question.
    Found,
    /// None answers here: nothing is asked.
    NotHere,
    /// macOS refuses aterm the local network.
    Refused,
    /// macOS still held the look for a person's answer after [`LOCAL_NETWORK_TRIES`] looks.
    Unanswered,
}

/// Look for the ALab vault ([`SecretsOps::probe_alab`]). While macOS holds this app off the
/// local network for a person's answer ([`local_network_pending`]) it says so once and looks
/// again every second, up to [`LOCAL_NETWORK_TRIES`] looks.
fn look(ops: &dyn SecretsOps) -> Looked {
    let mut told = false;
    for _ in 0..LOCAL_NETWORK_TRIES {
        match ops.probe_alab() {
            Ok(ProbeOutcome::Found(_)) => return Looked::Found,
            Err(why) if aterm_secrets::client::refused_by_local_network(&why) => {
                return Looked::Refused;
            }
            Err(why) if local_network_pending(&why) => {
                if !told {
                    told = true;
                    ops.say(LOCAL_NETWORK_ASK);
                }
                ops.pause(Duration::from_secs(1));
            }
            Ok(
                ProbeOutcome::Absent
                | ProbeOutcome::NotThisVault(_)
                | ProbeOutcome::AnotherVault(_),
            )
            | Err(_) => {
                return Looked::NotHere;
            }
        }
    }
    Looked::Unanswered
}

/// What the person settled at the vault prompts ([`chosen`]).
enum Chose {
    /// Join this source.
    Join(String),
    /// Join nothing; the row says why.
    Note(Probe),
}

/// The vault `--vault` names, read at once ([`SecretsOps::check`]) and asked for again in
/// place, naming why, while no vault answers there. A signer nothing pins is put to the person
/// here, before anything changes, and the join is handed its `#<signer>` pin, so the join asks
/// nothing.
/// # Errors
/// The person stopped a prompt (Ctrl-C), the trust question included; nothing was changed.
fn chosen(sys: &dyn System, ops: &dyn SecretsOps, typed: &str) -> Result<Chose, String> {
    let mut source = typed.trim().to_owned();
    loop {
        let why = match ops.check(&source) {
            Ok(Checked::Pinned) => return Ok(Chose::Join(source)),
            Ok(Checked::Unpinned { fingerprint, pin }) => {
                return match trusts(sys, &fingerprint) {
                    Ok(true) => Ok(Chose::Join(format!("{source}#{pin}"))),
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {
                        Err(STOPPED_AT_TRUST.into())
                    }
                    Ok(false) | Err(_) => Ok(Chose::Note(Probe::NotApplicable {
                        why: "the vault's signing key was not trusted at the prompt; nothing was changed".into(),
                    })),
                };
            }
            Err(why) => why,
        };
        ops.say(&format!("no vault answers at {}: {why}", shown(&source)));
        let answer = match sys.ask(PROMPT, false) {
            Ok(answer) => answer,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {
                return Err("stopped at the secrets vault prompt; nothing was changed".into());
            }
            Err(_) => {
                return Ok(Chose::Note(Probe::NotApplicable {
                    why: UNCONFIGURED.into(),
                }));
            }
        };
        let typed = match &answer {
            None => String::new(),
            Some(line) => match std::str::from_utf8(line.bytes()) {
                Ok(text) => text.trim().to_owned(),
                Err(_) => {
                    return Ok(Chose::Note(Probe::Blocked {
                        why: "the answer at the vault prompt is not UTF-8 (not shown); run aterm init again, or aterm secrets join <url>".into(),
                    }));
                }
            },
        };
        drop(answer);
        if typed.is_empty()
            || typed.eq_ignore_ascii_case("none")
            || typed.eq_ignore_ascii_case("skip")
        {
            return Ok(Chose::Note(Probe::NotApplicable {
                why: SKIPPED.into(),
            }));
        }
        source = typed;
    }
}

/// The secrets step for this run. An unconfigured Mac joins the ALab vault where it answers,
/// asking only its join password, or the vault `vault` (`--vault`) names; where neither, it
/// asks nothing.
/// # Errors
/// The run does not name exactly one user, or the person stopped a prompt (Ctrl-C); nothing
/// was changed then.
pub fn items(
    sys: &dyn System,
    ctx: &Ctx,
    ops: Arc<dyn SecretsOps>,
    mode: Mode,
    vault: Option<&str>,
) -> Result<Secrets, String> {
    let user = only_user(ctx)?.clone();
    let status = match ops.status() {
        Ok(status) => status,
        Err(why) => {
            return Ok(Secrets::note(
                user,
                Probe::Unusable {
                    why: format!("cannot read this Mac's vault state: {why}"),
                },
            ));
        }
    };
    if let Some(config) = &status.config {
        let other = vault.is_some_and(|typed| {
            !matches!(
                selected_vault(typed, Path::new(&user.home), Some(config)),
                Ok(Some(_))
            )
        });
        if other {
            return Ok(Secrets::note(
                user,
                Probe::Blocked {
                    why: format!(
                        "this Mac is in {}, not the vault --vault names; aterm secrets join <url> moves it",
                        configured_text(Some(config))
                    ),
                },
            ));
        }
        let waited_on = matches!(status.membership, Membership::Pending).then(|| item_id(&user));
        return Ok(Secrets {
            items: vec![Box::new(Vault::new(
                user,
                ops,
                source_text(&config.source),
                Origin::Configured,
                None,
                Duration::ZERO,
            ))],
            waited_on,
        });
    }
    if mode != Mode::Apply || !ops.terminal() {
        return Ok(Secrets::not_applicable(user, UNCONFIGURED));
    }
    let source = match vault {
        Some(typed) => match chosen(sys, ops.as_ref(), typed)? {
            Chose::Join(source) => source,
            Chose::Note(probe) => return Ok(Secrets::note(user, probe)),
        },
        None => match look(ops.as_ref()) {
            Looked::Found => {
                ops.say("the ALab vault answers on this network");
                aterm_secrets::ALAB_DEFAULT_URL.to_owned()
            }
            Looked::NotHere => return Ok(Secrets::not_applicable(user, NOT_HERE)),
            Looked::Refused => return Ok(Secrets::not_applicable(user, LAN_REFUSED)),
            Looked::Unanswered => return Ok(Secrets::not_applicable(user, LAN_UNANSWERED)),
        },
    };
    let password = if is_url(&source) {
        match ask_password(sys) {
            Ok(Typed::Password(password)) => Some(password),
            Ok(Typed::Nothing) => None,
            Ok(Typed::Skip) => return Ok(Secrets::not_applicable(user, SKIPPED)),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {
                return Err(STOPPED_AT_PASSWORD.into());
            }
            Err(_) => return Ok(Secrets::not_applicable(user, UNCONFIGURED)),
        }
    } else {
        None
    };
    let id = item_id(&user);
    Ok(Secrets {
        items: vec![Box::new(Vault::new(
            user,
            ops,
            source,
            Origin::Chosen,
            password,
            JOIN_WINDOW,
        ))],
        waited_on: Some(id),
    })
}

/// The secrets row's id, before its user's name.
const ITEM: &str = "init.secrets@";

pub(crate) fn item_id(user: &User) -> String {
    format!("{ITEM}{}", user.name)
}

/// Whether `row` is the vault's own wait for an admin to approve this Mac: the subscriber, which
/// runs without `aterm init` or its person, finishes it. A row waiting for that one (GitHub's
/// sign-in) is not: only another run finishes it.
#[must_use]
pub fn finishes_by_itself(row: &crate::model::ItemReport) -> bool {
    row.id.starts_with(ITEM) && matches!(row.stood_aside, Some(StoodAside::Waiting(_)))
}

fn is_url(source: &str) -> bool {
    let lower = source.to_ascii_lowercase();
    ["http://", "https://"]
        .iter()
        .any(|scheme| lower.starts_with(scheme) && lower.len() > scheme.len())
}

fn plausible_source(source: &str) -> bool {
    is_url(source) || source.starts_with('/') || source.starts_with("~/")
}

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
    /// This Mac had it configured: an apply starts a subscriber that does not run and writes
    /// again the files a sync left unwritten, asking nobody anything.
    Configured,
    /// A person at the terminal is joining it (they named it, pressed Enter, or the ALab
    /// vault answered), and gave its join password or none: the join may confirm a signer
    /// and wait.
    Chosen,
    /// System State's plan named it ([`selected`]): an unattended pass joins it when this Mac
    /// is not in it, and otherwise reads as [`Origin::Configured`] does.
    Selected,
}

/// What an apply is to do, as the last probe planned it: what its journal line's revert undoes,
/// whether it needs the vault to answer, and everything the apply makes. What changed since the
/// probe is the next try's to plan.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Planned {
    /// A join: this Mac's key (made when it has none), the source and state files, a request
    /// in the vault's queue, the vault's files.
    join: bool,
    /// For a join: the vault configuration the probe read (`None`: none), the one the join
    /// replaces and its line's revert names. A vault configured in its place since (a join
    /// from a terminal while the vault was asked) is not replaced: the next try reads it.
    over: Option<Config>,
    /// The subscriber's launchd user agent (or systemd unit), written and started.
    subscribe: bool,
    /// The vault's files written again (a sync).
    files: bool,
    /// For a join from a vault this Mac reads: the probe's time, which names where that
    /// vault's files are kept first ([`kept_dir`]).
    kept: Option<u64>,
    /// For a join: this Mac has no key, so the join makes one.
    key: bool,
}

fn kept_dir(dir: &Path, at: u64) -> PathBuf {
    dir.join(format!("before-join-{at}"))
}

enum Step {
    /// This Mac is not in the selected vault: a join, from this state.
    Join(String),
    /// The selected vault is this Mac's: read as a configured one is.
    Read,
}

/// The vault this Mac reads, joined on apply.
pub(crate) struct Vault {
    user: User,
    ops: Arc<dyn SecretsOps>,
    /// What [`SecretsOps::join`] is given: as typed, the ALab default, the configured
    /// source, or the one System State's plan names.
    source: String,
    origin: Origin,
    /// How long a join waits for an approval.
    wait: Duration,
    /// The join password typed for a chosen URL, or System State's; taken by the apply's
    /// join, and wiped with it once the join has used it.
    password: Mutex<Option<SecretBytes>>,
    /// Why the last apply stood aside: this Mac waits for an admin's approval.
    aside: Mutex<Option<String>>,
    /// Why the last probe found this Mac waiting for an admin's approval with its subscriber
    /// running, which finishes the join by itself ([`Item::read_aside`]).
    waiting: Mutex<Option<String>>,
    /// What the last probe planned.
    planned: Mutex<Planned>,
    /// Which vault configuration a join may replace ([`selected`]).
    replaces: Replaces,
}

impl Vault {
    fn new(
        user: User,
        ops: Arc<dyn SecretsOps>,
        source: String,
        origin: Origin,
        password: Option<SecretBytes>,
        wait: Duration,
    ) -> Self {
        Self {
            user,
            ops,
            source,
            origin,
            wait,
            password: Mutex::new(password),
            aside: Mutex::new(None),
            waiting: Mutex::new(None),
            planned: Mutex::new(Planned::default()),
            replaces: Replaces::Any,
        }
    }

    /// Hold `password` for the next join.
    pub(crate) fn hold_password(&self, password: Option<SecretBytes>) {
        *self.password.lock().unwrap_or_else(PoisonError::into_inner) = password;
    }

    /// The join password no join has spent.
    pub(crate) fn unspent_password(&self) -> Option<SecretBytes> {
        self.password
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    fn has_password(&self) -> bool {
        self.password
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }

    fn set_aside(&self, why: Option<String>) {
        *self.aside.lock().unwrap_or_else(PoisonError::into_inner) = why;
    }

    fn aside_also(&self, why: String) {
        let mut aside = self.aside.lock().unwrap_or_else(PoisonError::into_inner);
        *aside = Some(match aside.take() {
            Some(first) => format!("{first}; {why}"),
            None => why,
        });
    }

    fn plan(&self, planned: Planned) {
        *self.planned.lock().unwrap_or_else(PoisonError::into_inner) = planned;
    }

    fn planned(&self) -> Planned {
        self.planned
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Whether this selected vault is to be joined, and from what.
    /// # Errors
    /// The plan's source is not a vault source.
    fn step(&self, status: &Status) -> Result<Step, String> {
        let Some(config) = selected_vault(
            &self.source,
            Path::new(&self.user.home),
            status.config.as_ref(),
        )?
        else {
            return Ok(Step::Join(match &status.config {
                None => "no vault".into(),
                Some(other) => format!(
                    "vault {} ({}), not the one System State names",
                    other.name,
                    shown(&source_text(&other.source))
                ),
            }));
        };
        let name = &config.name;
        Ok(match &status.membership {
            _ if config.signer.is_some() && status.machine.is_none() => {
                Step::Join(format!("vault {name}, but this Mac's key is missing"))
            }
            Membership::Revoked { seq } => Step::Join(format!(
                "an admin removed this Mac from vault {name} at seq {seq}"
            )),
            Membership::Pending if self.has_password() => Step::Join(format!(
                "waiting for an admin to approve vault {name}, with a join password not yet sent"
            )),
            _ => Step::Read,
        })
    }

    fn configured_since(&self, planned: &Planned) -> Option<StoodAside> {
        if !planned.join || self.origin != Origin::Selected {
            return None;
        }
        match self.ops.configured() {
            Ok(now) if now.as_ref() == planned.over.as_ref() => None,
            Ok(now) => Some(StoodAside::Lock(format!(
                "this Mac's vault changed while the vault was asked (now {}), so nothing was changed; the next try reads it",
                configured_text(now.as_ref())
            ))),
            Err(why) => Some(StoodAside::Lock(format!(
                "cannot read which vault this Mac is in now ({why}), so nothing was changed"
            ))),
        }
    }

    fn install(&self) -> Result<bool, String> {
        let installed = if self.planned().join {
            self.ops.install_subscriber().map(Some)
        } else {
            self.ops.repair_subscriber()
        }
        .map_err(|why| format!("the subscriber could not be installed: {why}"))?;
        Ok(installed.is_some())
    }

    /// Put the subscriber in place when the probe planned it.
    /// # Errors
    /// The install failed.
    fn subscribe(&self) -> Result<bool, String> {
        if !self.planned().subscribe {
            return Ok(true);
        }
        // A subscriber that runs is left as it runs: only a join moves it to this aterm, so
        // an apply on a configured Mac never swaps the program a person chose (a held snapshot).
        if self.origin != Origin::Chosen
            && self
                .ops
                .subscriber()
                .is_ok_and(|state| idle(&state).is_none())
        {
            return Ok(true);
        }
        match self.install() {
            Err(why) if self.origin == Origin::Selected => match self.ops.fleeting() {
                Some(exe) => {
                    self.aside_also(fleeting_wait(&exe));
                    Ok(false)
                }
                None => Err(why),
            },
            Ok(true) if self.origin == Origin::Selected => self.ops.started().map(|()| true),
            installed => installed,
        }
    }

    fn subscribe_queued(&self, installed: &OnceCell<Result<bool, String>>) -> bool {
        installed
            .get_or_init(|| {
                let done = self.subscribe();
                if let Err(why) = &done {
                    self.ops.say(&format!(
                        "{why}; run aterm secrets agent on to keep this Mac in sync (until then it joins only while aterm init waits)"
                    ));
                }
                done
            })
            .clone()
            .unwrap_or(false)
    }

    fn join(&self, sys: &dyn System, interactive: bool) -> Result<(), String> {
        let planned = self.planned();
        let replaces = match self.origin {
            Origin::Selected => Replaces::Read(planned.over),
            Origin::Chosen | Origin::Configured => Replaces::Any,
        };
        if let Some(at) = planned.kept {
            let dir = Paths::for_home(Path::new(&self.user.home)).dir;
            self.ops
                .keep(&kept_dir(&dir, at), &replaces)
                .map_err(|why| {
                    format!("keeping the vault this Mac reads before the join: {why}")
                })?;
        }
        let installed: OnceCell<Result<bool, String>> = OnceCell::new();
        let requested = || self.subscribe_queued(&installed);
        let password = self
            .password
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        // A person's join asks and returns: one sent with the join password then waits out its
        // window here, and one without waits for nobody, since the subscriber finishes it.
        let chosen = self.origin == Origin::Chosen;
        if chosen {
            self.ops.say(&format!("joining {}…", shown(&self.source)));
        }
        let sent = password.is_some();
        let outcome = self.ops.join(
            sys,
            (&self.source, &replaces),
            password.as_ref(),
            interactive,
            if chosen { Duration::ZERO } else { self.wait },
            &requested,
        );
        let outcome = match outcome {
            Ok(outcome) => {
                drop(password);
                outcome
            }
            Err(why) => {
                if self.origin == Origin::Selected {
                    self.hold_password(password);
                } else {
                    drop(password);
                }
                return Err(format!("joining the vault: {why}"));
            }
        };
        installed
            .get_or_init(|| self.subscribe())
            .clone()
            .map_err(|why| format!("the vault answered, but {why}"))?;
        let JoinOutcome::Pending { mut code, .. } = outcome else {
            return Ok(());
        };
        let inside = || {
            self.ops
                .status()
                .is_ok_and(|status| matches!(status.membership, Membership::In { .. }))
        };
        let mut waits = chosen && sent;
        while waits && !inside() {
            waits = false;
            if self.approved(sys) != Some(false) {
                break;
            }
            let Some(retyped) = retyped(sys) else {
                break;
            };
            let again = self
                .ops
                .join(
                    sys,
                    (&self.source, &replaces),
                    Some(&retyped),
                    interactive,
                    Duration::ZERO,
                    &requested,
                )
                .map_err(|why| format!("joining the vault: {why}"))?;
            drop(retyped);
            if let JoinOutcome::Pending { code: asked, .. } = again {
                code = asked;
                waits = true;
            }
        }
        if !inside() {
            self.wait_for_admin(Some(&code), sent || !chosen);
        }
        Ok(())
    }

    /// Wait out [`Vault::wait`] for an admin Mac to approve a join sent with the join password,
    /// holding Ctrl-C ([`System::catch_stops`]) so that it ends only the wait: whether this Mac
    /// is in, or `None` when the person stopped the wait.
    fn approved(&self, sys: &dyn System) -> Option<bool> {
        let _stops = sys.catch_stops();
        let inside = self
            .ops
            .await_approval(self.wait, &|| sys.stopped().is_some());
        (inside || sys.stopped().is_none()).then_some(inside)
    }

    fn settle(&self) -> Result<(), String> {
        let planned = self.planned();
        if !planned.join && self.ops.turned_off() {
            return Ok(());
        }
        self.subscribe()?;
        // A turn-off that waited on the install's decision lock still wins: no readiness
        // retry or file sync follows.
        if !planned.join && self.ops.turned_off() {
            return Ok(());
        }
        let Ok(status) = self.ops.status() else {
            return Ok(());
        };
        match status.membership {
            Membership::Pending => {
                self.wait_for_admin(status.machine.as_ref().map(|(_, code)| code.as_str()), true);
            }
            Membership::In { .. } | Membership::Plain
                if planned.files && !self.ops.synced().unwrap_or(false) =>
            {
                if !planned.join && self.ops.turned_off() {
                    return Ok(());
                }
                self.ops
                    .sync()
                    .map_err(|why| format!("writing the vault's files: {why}"))?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Stand aside waiting for an admin, naming the fix for a mistyped join password when one
    /// may have been sent (`retyped`).
    fn wait_for_admin(&self, code: Option<&str>, retyped: bool) {
        let admin = format!(
            "waiting for an admin: {}",
            approval(code, &self.source, retyped)
        );
        let mut aside = self.aside.lock().unwrap_or_else(PoisonError::into_inner);
        *aside = Some(match aside.take() {
            Some(then) => format!("{admin}; {then}"),
            None => admin,
        });
    }
}

fn fleeting_wait(exe: &Path) -> String {
    format!(
        "this aterm runs from {}, which goes away when it quits, so it cannot keep ~/.secrets in sync; move aterm to Applications and open it from there, and setup finishes this step",
        exe.display()
    )
}

fn source_text(source: &Source) -> String {
    match source {
        Source::Url(url) => url.clone(),
        Source::Path(path) => path.display().to_string(),
    }
}

/// The approval an admin gives, and, when a join password may have been sent (`retyped`), how
/// a mistyped one is fixed.
fn approval(code: Option<&str>, source: &str, retyped: bool) -> String {
    let approve = match code {
        Some(code) => format!(
            "approve this Mac on an admin Mac: aterm secrets approve {code} (the subscriber finishes the join once it is approved)"
        ),
        None => "approve this Mac on an admin Mac: aterm secrets requests, then aterm secrets approve <code> (the subscriber finishes the join once it is approved)".into(),
    };
    match retype(source).filter(|_| retyped) {
        Some(retype) => format!("{approve}; {retype}"),
        None => approve,
    }
}

/// The join password retyped at [`RETYPE_PROMPT`], blanks around it dropped; `None` keeps
/// waiting for approval by code (Enter, or the prompt stopped).
fn retyped(sys: &dyn System) -> Option<SecretBytes> {
    let answer = sys.ask(RETYPE_PROMPT, true).ok()??;
    let typed = answer.bytes().trim_ascii();
    (!typed.is_empty()).then(|| SecretBytes::from(typed))
}

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

fn secrets(n: usize) -> String {
    if n == 1 {
        "1 secret".into()
    } else {
        format!("{n} secrets")
    }
}

const fn idle(state: &State) -> Option<&'static str> {
    match state {
        State::Installed { pid: Some(_), .. } => None,
        State::Installed { pid: None, .. } => Some("subscriber installed but not running"),
        State::Absent => Some("subscriber not installed"),
    }
}

/// What a member Mac whose subscriber the person turned off says of its sync.
const SYNC_OFF: &str = "sync is off on this Mac by your choice; aterm secrets agent on turns it on";

fn judge(status: &Status, source: &str, unsynced: Option<&str>, synced: bool, off: bool) -> Probe {
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
    let kept = "kept in sync by the subscriber";
    let unwritten = |now: String| Probe::Change {
        from: format!("{now}, but a file it wrote failed, is missing or was changed"),
        to: WRITTEN.into(),
    };
    match &status.membership {
        // Off is the person's choice even with a file missing or changed: no setup path
        // repairs files or installs the subscriber without a new join.
        Membership::In { seq, .. } if off => Probe::Ok {
            now: format!("in vault {name} (seq {seq}); {SYNC_OFF}"),
        },
        Membership::Plain if off => Probe::Ok {
            now: format!("reading the plain text file {}; {SYNC_OFF}", shown(source)),
        },
        Membership::In { seq, .. } if watched && !synced => {
            unwritten(format!("in vault {name} (seq {seq})"))
        }
        Membership::Plain if watched && !synced => {
            unwritten(format!("reading the plain text file {}", shown(source)))
        }
        Membership::In { seq, .. } if watched => Probe::Ok {
            now: format!("in vault {name} (seq {seq}): {held}, {kept}"),
        },
        Membership::In { seq, .. } => unwatched(format!("in vault {name} (seq {seq})")),
        Membership::Plain if watched => Probe::Ok {
            now: format!(
                "reading the plain text file {}: {held}, {kept}",
                shown(source)
            ),
        },
        Membership::Plain => unwatched(format!("reading the plain text file {}", shown(source))),
        // Approved, the join lands only through the subscriber the person turned off:
        // said as theirs, with the command that turns it on, and nothing to wait on.
        Membership::Pending if off => Probe::Ok {
            now: format!("waiting for an admin to approve vault {name}; {SYNC_OFF}, and an approval lands once it is on"),
        },
        // The subscriber finishes the join once an admin approves: nothing for this Mac to
        // do, so the row waits ([`Item::read_aside`]) rather than asking the person.
        Membership::Pending if watched => Probe::Unusable {
            why: format!("waiting for an admin: {}", approval(code, source, true)),
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

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        self.plan(Planned::default());
        *self.waiting.lock().unwrap_or_else(PoisonError::into_inner) = None;
        let status = match self.ops.status() {
            Ok(status) => status,
            Err(why) => {
                return Probe::Unusable {
                    why: format!("cannot read this Mac's vault state: {why}"),
                };
            }
        };
        if self.origin == Origin::Selected {
            match self.step(&status) {
                Err(why) => return Probe::Blocked { why },
                Ok(Step::Join(from)) => {
                    // A later configuration, including a new machine name or signer at
                    // this address, is the next try's to read, not this one's to replace.
                    if let Replaces::Read(read) = &self.replaces
                        && read.as_ref() != status.config.as_ref()
                    {
                        return Probe::Unusable {
                            why: format!(
                                "this Mac's vault changed after it was read (now {}), so nothing was changed; the next try reads it",
                                configured_text(status.config.as_ref())
                            ),
                        };
                    }
                    // A join is the person asking for this Mac in the vault: it turns
                    // sync on, over a record that they turned it off (the install
                    // clears it), or an approved join would never land.
                    let subscribe = self
                        .ops
                        .subscriber()
                        .map_or(true, |state| idle(&state).is_some());
                    self.plan(Planned {
                        join: true,
                        over: status.config.clone(),
                        subscribe,
                        files: true,
                        kept: status.config.is_some().then(|| sys.now()),
                        key: status.machine.is_none(),
                    });
                    return Probe::Change {
                        from,
                        to: shown(&self.source),
                    };
                }
                Ok(Step::Read) => {}
            }
        }
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
        let synced = match status.membership {
            Membership::In { .. } | Membership::Plain => match self.ops.synced() {
                Ok(synced) => synced,
                Err(why) => {
                    return Probe::Unusable {
                        why: format!("cannot read whether the vault files are written: {why}"),
                    };
                }
            },
            _ => true,
        };
        // Sync off stands for System State and `aterm init` alike: no change, the files as
        // last synced. Only a new join turns it on.
        let off = self.origin != Origin::Chosen && unsynced.is_some() && self.ops.turned_off();
        let unsynced = unsynced.filter(|_| !off);
        let probe = judge(&status, &self.source, unsynced, synced, off);
        if let (Membership::Pending, Probe::Unusable { why }) = (&status.membership, &probe) {
            *self.waiting.lock().unwrap_or_else(PoisonError::into_inner) = Some(why.clone());
        }
        if matches!(probe, Probe::Change { .. }) {
            self.plan(match self.origin {
                Origin::Chosen => Planned {
                    join: true,
                    over: status.config.clone(),
                    subscribe: true,
                    files: true,
                    kept: None,
                    key: status.machine.is_none(),
                },
                // Sync off never reaches this plan: `judge` reads it as the person's
                // choice (no change), so nothing installs or rewrites over it.
                // A subscriber that runs is left as it runs, so only one the probe found
                // absent or down is planned.
                Origin::Configured | Origin::Selected => Planned {
                    subscribe: unsynced.is_some(),
                    files: !synced,
                    ..Planned::default()
                },
            });
        }
        probe
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        self.set_aside(None);
        match self.origin {
            Origin::Configured => self.settle(),
            Origin::Chosen => self.join(sys, true),
            Origin::Selected => {
                let status = self
                    .ops
                    .status()
                    .map_err(|why| format!("cannot read this Mac's vault state: {why}"))?;
                let planned = self.planned();
                match self.step(&status)? {
                    Step::Join(_)
                        if planned.join && planned.over.as_ref() == status.config.as_ref() =>
                    {
                        self.join(sys, false)
                    }
                    Step::Join(now) if !planned.join => Err(format!(
                        "this Mac's vault state changed after it was read ({now}), so nothing was changed; the next try plans the join"
                    )),
                    Step::Join(now) => Err(format!(
                        "this Mac's vault state changed after it was read ({now}), so nothing was changed; the next try reads it"
                    )),
                    Step::Read => self.settle(),
                }
            }
        }
    }

    fn revert(&self) -> Option<String> {
        const DIR: &str = "~/.config/aterm/secrets";
        let planned = self.planned();
        let mut steps = Vec::new();
        if planned.subscribe {
            steps.push("aterm secrets agent off (stops and removes the subscriber)".to_owned());
        }
        if planned.join {
            steps.push(
                "rm each file aterm secrets ls names (in ~/.secrets, or in ~ for a ~/ name)"
                    .to_owned(),
            );
        }
        if planned.join || planned.files {
            steps.push(
                "a file the vault's took the place of is in ~/.secrets/.replaced, to move back"
                    .to_owned(),
            );
        }
        if planned.join {
            steps.push(match planned.kept {
                Some(at) => {
                    let kept = kept_dir(Path::new(DIR), at);
                    let kept = kept.display();
                    format!(
                        "cp {kept}/source.toml {kept}/state.toml {DIR}/ and run aterm secrets sync (this Mac reads the vault it read before again)"
                    )
                }
                None => format!(
                    "rm {DIR}/source.toml {DIR}/state.toml (forgets the vault on this Mac)"
                ),
            });
            if planned.key {
                steps.push(format!("rm {DIR}/identity (the key the join made)"));
            }
            steps.push("aterm secrets revoke <host> on an admin Mac lets it go".to_owned());
        }
        (!steps.is_empty()).then(|| steps.join("; then "))
    }

    fn stands_aside(&self, _sys: &dyn System, _ctx: &Ctx) -> Option<StoodAside> {
        let planned = self.planned();
        if !(planned.join || (planned.files && !planned.subscribe)) {
            return None;
        }
        let (source, _) =
            aterm_secrets::transport::parse_source(&self.source, Path::new(&self.user.home))
                .ok()?;
        match self.ops.reachable(&source) {
            Look::Answers => self.configured_since(&planned),
            Look::Unreached(why) => Some(StoodAside::Offline(format!(
                "the vault does not answer, so nothing was changed: {why}"
            ))),
            Look::Held(why) => Some(StoodAside::Offline(format!(
                "the vault does not answer, so nothing was changed: {why}; {LOCAL_NETWORK}"
            ))),
            Look::NoVault(why) => Some(StoodAside::Offline(format!("{NO_VAULT_NOW}: {why}"))),
        }
    }

    fn stood_aside(&self) -> Option<aterm_converge::StoodAside> {
        self.aside
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .map(aterm_converge::StoodAside::Waiting)
    }

    fn read_aside(&self) -> Option<aterm_converge::StoodAside> {
        self.waiting
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .map(aterm_converge::StoodAside::Waiting)
    }
}

struct Note {
    user: User,
    probe: Probe,
}

impl Note {
    const fn new(user: User, probe: Probe) -> Self {
        Self { user, probe }
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
pub(crate) mod tests {
    use super::*;
    use crate::engine::{self, NoJournal};
    use crate::items::defaults::tests::{ctx, user};
    use crate::model::{ItemReport, Outcome};
    use crate::port::{Cmd, Fake, Ran, RunFailed, Secret, Stat};
    use aterm_secrets::age::Recipient;
    use aterm_secrets::client::{Config, Probed};
    use std::collections::VecDeque;

    /// A vault whose state answers from a script (the last one sticks), and which records
    /// every look for the ALab vault, join and subscriber install.
    #[derive(Default)]
    pub(crate) struct Scripted {
        statuses: Mutex<VecDeque<aterm_secrets::Result<Status>>>,
        subscriber: Mutex<Option<aterm_secrets::Result<State>>>,
        terminal: Mutex<bool>,
        probed: Mutex<Option<aterm_secrets::Result<ProbeOutcome>>>,
        outcome: Mutex<Option<aterm_secrets::Result<JoinOutcome>>>,
        /// The join queues a request (calls `requested`) before it ends, as `client::join`
        /// does for a Mac not in yet.
        requests: Mutex<bool>,
        /// The exact caller-read configuration handed to the copy and join boundaries.
        replacement_reads: Mutex<Vec<Replaces>>,
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
        /// What `synced` answers until a sync makes it true.
        unwritten: Mutex<bool>,
        syncs: Mutex<u32>,
        /// What `reachable` answers (`None`: the vault answers): nothing answered, or no
        /// vault there; and how many times it was asked.
        unreachable: Mutex<Option<Look>>,
        absent: Mutex<Option<String>>,
        reached: Mutex<u32>,
        /// What this Mac's vault state and subscriber become while `reachable` is asked:
        /// the subscriber, running beside the setup, applying a version or stopping.
        meanwhile: Mutex<Option<(Status, State)>>,
        /// Why `keep` fails (`None`: it keeps).
        unkept: Mutex<Option<String>>,
        /// A file the vault wrote is changed as the subscriber is installed.
        spoiled_by_install: Mutex<bool>,
        /// What `fleeting` answers: this aterm runs from a copy that goes away, and every
        /// install is refused as `aterm_secrets::service::service_program` refuses it.
        fleeting: Mutex<Option<PathBuf>>,
        /// The wait each join was given.
        waits: Mutex<Vec<Duration>>,
        /// Every change in order (`join <source>`, `install`, `sync`), with the lines a
        /// [`Lines`] journal records before them.
        log: Mutex<Vec<String>>,
        /// What `say` was given.
        said: Mutex<Vec<String>>,
        /// The person turned the subscriber off.
        turned_off: Mutex<bool>,
        /// Why a subscriber just installed does not keep running (`None`: it does).
        dies: Mutex<Option<String>>,
        /// Looks answered first, in order, before `probed` answers the rest.
        looks: Mutex<VecDeque<aterm_secrets::Result<ProbeOutcome>>>,
        /// What each `check` answers, in order (`Pinned` once they run out), and the sources it
        /// was asked about.
        checked: Mutex<VecDeque<Result<Checked, String>>>,
        checks: Mutex<Vec<String>>,
        /// Whether each approval wait ends with this Mac let in (`false` once they run out), and
        /// the window each was given.
        approvals: Mutex<VecDeque<bool>>,
        windows: Mutex<Vec<Duration>>,
        /// How many pauses between looks were taken.
        pauses: Mutex<u32>,
    }

    impl Scripted {
        pub(crate) fn new(statuses: Vec<aterm_secrets::Result<Status>>) -> Arc<Self> {
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

        fn alab_answers(&self) {
            *self.probed.lock().unwrap() = Some(Ok(ProbeOutcome::Found(Probed {
                seq: 7,
                fingerprint: "0123-4567-89ab-cdef".into(),
                envelope: Vec::new(),
            })));
        }

        fn subscribed(&self, state: aterm_secrets::Result<State>) {
            *self.subscriber.lock().unwrap() = Some(state);
        }

        pub(crate) fn probes(&self) -> u32 {
            *self.probes.lock().unwrap()
        }

        fn joins(&self) -> Vec<(String, bool)> {
            self.joins.lock().unwrap().clone()
        }

        fn installs(&self) -> u32 {
            *self.installs.lock().unwrap()
        }

        fn log(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }

        fn changes(&self) -> Vec<String> {
            self.log()
                .into_iter()
                .filter(|event| !event.starts_with("line "))
                .collect()
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

        fn configured(&self) -> aterm_secrets::Result<Option<Config>> {
            let statuses = self.statuses.lock().unwrap();
            let status = statuses.front().cloned().expect("a scripted status");
            status.map(|status| status.config)
        }

        fn subscriber(&self) -> aterm_secrets::Result<State> {
            if let Some(state) = self.subscriber.lock().unwrap().clone() {
                return state;
            }
            let installed = *self.installs.lock().unwrap() > 0
                && self.fleeting().is_none()
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
            if let Some(look) = self.looks.lock().unwrap().pop_front() {
                return look;
            }
            self.probed
                .lock()
                .unwrap()
                .clone()
                .expect("a scripted probe")
        }

        fn check(&self, source: &str) -> Result<Checked, String> {
            self.checks.lock().unwrap().push(source.to_owned());
            self.checked
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Ok(Checked::Pinned))
        }

        fn await_approval(&self, window: Duration, stopped: &dyn Fn() -> bool) -> bool {
            self.windows.lock().unwrap().push(window);
            !stopped() && self.approvals.lock().unwrap().pop_front().unwrap_or(false)
        }

        fn pause(&self, _pause: Duration) {
            *self.pauses.lock().unwrap() += 1;
        }

        fn reachable(&self, _source: &Source) -> Look {
            *self.reached.lock().unwrap() += 1;
            if let Some((status, state)) = self.meanwhile.lock().unwrap().take() {
                *self.statuses.lock().unwrap() = vec![Ok(status)].into();
                *self.subscriber.lock().unwrap() = Some(Ok(state));
            }
            if let Some(look) = self.unreachable.lock().unwrap().clone() {
                return look;
            }
            match self.absent.lock().unwrap().clone() {
                Some(why) => Look::NoVault(why),
                None => Look::Answers,
            }
        }

        fn keep(&self, into: &Path, replaces: &Replaces) -> aterm_secrets::Result<()> {
            self.replacement_reads
                .lock()
                .unwrap()
                .push(replaces.clone());
            self.log
                .lock()
                .unwrap()
                .push(format!("keep {}", into.display()));
            match self.unkept.lock().unwrap().clone() {
                Some(why) => Err(aterm_secrets::Error::new(why)),
                None => Ok(()),
            }
        }

        fn join(
            &self,
            _sys: &dyn System,
            (source, replaces): (&str, &Replaces),
            password: Option<&SecretBytes>,
            interactive: bool,
            wait: Duration,
            requested: &dyn Fn() -> bool,
        ) -> aterm_secrets::Result<JoinOutcome> {
            self.replacement_reads
                .lock()
                .unwrap()
                .push(replaces.clone());
            self.log.lock().unwrap().push(format!("join {source}"));
            self.waits.lock().unwrap().push(wait);
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
            self.log.lock().unwrap().push("install".into());
            *self.installs.lock().unwrap() += 1;
            if let Some(exe) = self.fleeting() {
                aterm_secrets::service::service_program(&exe, &|_| false)?;
            }
            if *self.spoiled_by_install.lock().unwrap() {
                *self.unwritten.lock().unwrap() = true;
            }
            let installed = self
                .installed
                .lock()
                .unwrap()
                .clone()
                .expect("a scripted install");
            if installed.is_ok() {
                *self.subscriber.lock().unwrap() = None;
                // As `aterm_secrets::service::install`: the subscriber installed is on
                // again, and the person's record that it was off goes.
                *self.turned_off.lock().unwrap() = false;
            }
            installed
        }

        fn turned_off(&self) -> bool {
            *self.turned_off.lock().unwrap()
        }

        fn started(&self) -> Result<(), String> {
            self.dies.lock().unwrap().clone().map_or(Ok(()), Err)
        }

        fn fleeting(&self) -> Option<PathBuf> {
            self.fleeting.lock().unwrap().clone()
        }

        fn synced(&self) -> aterm_secrets::Result<bool> {
            Ok(!*self.unwritten.lock().unwrap())
        }

        fn sync(&self) -> aterm_secrets::Result<()> {
            self.log.lock().unwrap().push("sync".into());
            *self.syncs.lock().unwrap() += 1;
            *self.unwritten.lock().unwrap() = false;
            Ok(())
        }

        fn say(&self, line: &str) {
            self.said.lock().unwrap().push(line.to_owned());
        }
    }

    struct Lines {
        ops: Arc<Scripted>,
        refuse: bool,
    }

    impl crate::engine::Journal for Lines {
        fn before_change(
            &mut self,
            id: &str,
            from: &str,
            to: &str,
            revert: Option<&str>,
        ) -> Result<(), String> {
            if self.refuse {
                return Err("No space left on device (os error 28)".into());
            }
            self.ops.log.lock().unwrap().push(format!(
                "line {id}: {from} -> {to}; revert: {}",
                revert.unwrap_or("none")
            ));
            Ok(())
        }
    }

    const CODE: &str = "abcd-ef23-ghij-k234";

    pub(crate) fn unconfigured() -> Status {
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
        run_with(sys, ops, mode, None)
    }

    /// [`run`], its journal lines in `ops`'s log ([`Lines`]).
    fn run_journaled(sys: &Fake, ops: &Arc<Scripted>, mode: Mode) -> Vec<ItemReport> {
        let context = ctx(&[user("alice", 501)]);
        let port: Arc<dyn SecretsOps> = ops.clone();
        let step = items(sys, &context, port, mode, None).expect("items");
        let mut journal = Lines {
            ops: Arc::clone(ops),
            refuse: false,
        };
        engine::run(&step.items, mode, sys, &context, &mut journal)
    }

    /// [`run`] with `--vault` naming `vault`.
    fn run_with(
        sys: &dyn System,
        ops: &Arc<Scripted>,
        mode: Mode,
        vault: Option<&str>,
    ) -> Vec<ItemReport> {
        let context = ctx(&[user("alice", 501)]);
        let port: Arc<dyn SecretsOps> = ops.clone();
        let step = items(sys, &context, port, mode, vault).expect("items");
        assert_eq!(step.items.len(), 1);
        engine::run(&step.items, mode, sys, &context, &mut NoJournal)
    }

    fn assert_report_matches_current(sys: &Fake, ops: &Arc<Scripted>, row: &ItemReport) {
        let current = run(sys, ops, Mode::Plan);
        assert_eq!(current[0].outcome, Outcome::Ok);
        assert_eq!(
            row.to, current[0].to,
            "the applied report names its read-back"
        );
    }

    fn asked(sys: &Fake) -> usize {
        sys.calls().iter().filter(|c| c.starts_with("ask ")).count()
    }

    /// A Fake whose person presses Ctrl-C at the prompt `stop`, or, with `sigint`, during any
    /// wait ([`System::stopped`]). A Ctrl-C nothing catches ([`System::catch_stops`]) ends the
    /// process, so a wait that reads one uncaught fails the test.
    struct StopsAt {
        inner: Fake,
        stop: &'static str,
        sigint: bool,
    }

    impl System for StopsAt {
        fn ask(&self, prompt: &str, secret: bool) -> io::Result<Option<Secret>> {
            if !self.stop.is_empty() && prompt.starts_with(self.stop) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            self.inner.ask(prompt, secret)
        }
        fn catch_stops(&self) -> Box<dyn std::any::Any> {
            self.inner.catch_stops()
        }
        fn stopped(&self) -> Option<i32> {
            let calls = self.inner.calls();
            let held = |call: &str| calls.iter().filter(|c| *c == call).count();
            assert!(
                !self.sigint || held("catch stops") > held("stops restored"),
                "an uncaught Ctrl-C ends the process"
            );
            self.sigint.then_some(libc::SIGINT)
        }
        fn read_owned_file(
            &self,
            path: &Path,
            uid: u32,
            max: usize,
            private: bool,
        ) -> io::Result<Vec<u8>> {
            self.inner.read_owned_file(path, uid, max, private)
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
    fn only_a_look_held_to_no_answer_spends_the_consent_wait() {
        let spent = AtomicBool::new(false);
        let heard = std::cell::RefCell::new(Vec::new());
        let held = |why: &str| heard.borrow_mut().push(why.to_owned());
        let given = std::cell::RefCell::new(Vec::new());
        let look = |answer: Look, holds: bool| {
            let given = &given;
            move |patience: Duration, told: &dyn Fn(&str)| {
                given.borrow_mut().push(patience);
                if holds {
                    told("dns: no answer within 8000ms");
                }
                answer
            }
        };
        let refused = || Look::Unreached("fetching …: Connection refused".into());
        assert_eq!(
            patient_look(&spent, &held, look(refused(), false)),
            refused()
        );
        assert_eq!(
            patient_look(&spent, &held, look(Look::Answers, true)),
            Look::Answers
        );
        let none = Look::NoVault("a login page".into());
        assert_eq!(patient_look(&spent, &held, look(none.clone(), true)), none);
        assert!(!spent.load(Ordering::Relaxed));
        let unheard = Look::Held("dns: no answer within 8000ms".into());
        assert_eq!(
            patient_look(&spent, &held, look(unheard.clone(), true)),
            unheard
        );
        assert!(spent.load(Ordering::Relaxed));
        assert_eq!(
            patient_look(&spent, &held, look(unheard.clone(), false)),
            unheard
        );
        assert_eq!(
            *given.borrow(),
            [
                CONSENT_WAIT,
                CONSENT_WAIT,
                CONSENT_WAIT,
                CONSENT_WAIT,
                Duration::ZERO
            ]
        );
        assert_eq!(heard.borrow().len(), 3);
        assert!(
            heard
                .borrow()
                .iter()
                .all(|why| why == "dns: no answer within 8000ms")
        );

        let fresh = AtomicBool::new(false);
        given.borrow_mut().clear();
        patient_look(&fresh, &held, look(unheard, false));
        assert_eq!(*given.borrow(), [CONSENT_WAIT]);
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
    fn a_mac_in_the_vault_whose_files_are_not_all_written_is_a_change_that_syncs() {
        for mode in [Mode::Plan, Mode::Apply] {
            let sys = fake();
            let ops = Scripted::new(vec![Ok(configured(Membership::In {
                seq: 7,
                heard: 1_800_000_000,
            }))]);
            ops.subscribed(Ok(installed_state()));
            *ops.unwritten.lock().unwrap() = true;
            let rows = run(&sys, &ops, mode);
            let want = if mode == Mode::Plan {
                Outcome::Planned
            } else {
                Outcome::Changed
            };
            assert_eq!(rows[0].outcome, want, "{rows:?}");
            assert_eq!(
                rows[0].from.as_deref(),
                Some(
                    "in vault alab (seq 7), but a file it wrote failed, is missing or was changed"
                )
            );
            if mode == Mode::Plan {
                assert_eq!(rows[0].to.as_deref(), Some(WRITTEN));
            } else {
                assert_report_matches_current(&sys, &ops, &rows[0]);
            }
            assert_eq!(*ops.syncs.lock().unwrap(), u32::from(mode == Mode::Apply));
            assert!(ops.joins().is_empty(), "nothing is joined again");
            assert_eq!(
                ops.installs(),
                0,
                "the subscriber that runs is left as it runs"
            );
        }
        // Its journal line undoes only the sync: no `agent off` of a subscriber it never touched.
        let sys = fake();
        let ops = Scripted::new(vec![Ok(configured(Membership::In {
            seq: 7,
            heard: 1_800_000_000,
        }))]);
        ops.subscribed(Ok(installed_state()));
        *ops.unwritten.lock().unwrap() = true;
        let rows = run_journaled(&sys, &ops, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
        assert_eq!(ops.changes(), ["sync"]);
        journaled_first(&ops.log());
        assert!(!ops.log()[0].contains("agent off"), "{:?}", ops.log());
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
            if mode == Mode::Plan {
                assert_eq!(rows[0].to.as_deref(), Some(SUBSCRIBED));
            } else {
                assert_report_matches_current(&sys, &ops, &rows[0]);
            }
            assert_eq!(asked(&sys), 0);
            assert_eq!(ops.probes(), 0);
            assert!(ops.joins().is_empty(), "nothing is joined again");
            assert_eq!(ops.installs(), u32::from(mode == Mode::Apply));
        }

        for mode in [Mode::Plan, Mode::Apply] {
            let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
            let loaded = State::Installed {
                pid: None,
                unit: "/Users//alice/Library/LaunchAgents/systems.alab.aterm-secrets.plist".into(),
            };
            ops.subscribed(Ok(loaded));
            let sys = fake();
            let rows = run(&sys, &ops, mode);
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
            if mode == Mode::Plan {
                assert_eq!(rows[0].to.as_deref(), Some(SUBSCRIBED));
            } else {
                assert_report_matches_current(&sys, &ops, &rows[0]);
            }
            assert!(ops.joins().is_empty());
            assert_eq!(ops.installs(), u32::from(mode == Mode::Apply));
        }

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
    fn off_the_alab_lan_init_asks_nothing_and_joins_nothing() {
        for probed in [
            Ok(ProbeOutcome::Absent),
            Ok(ProbeOutcome::NotThisVault(
                "envelope: signed by a key the pin does not endorse".into(),
            )),
            Err(aterm_secrets::Error::new(
                "fetching http://alab-secrets.local:7484/v1/alab: dns: no answer within 8000ms",
            )),
        ] {
            let sys = fake();
            let ops = Scripted::new(vec![Ok(unconfigured())]);
            *ops.probed.lock().unwrap() = Some(probed);
            let rows = run(&sys, &ops, Mode::Apply);
            assert_eq!(rows[0].outcome, Outcome::NotApplicable, "{rows:?}");
            assert_eq!(rows[0].why.as_deref(), Some(NOT_HERE));
            assert_eq!(asked(&sys), 0, "{:?}", sys.calls());
            assert_eq!(ops.probes(), 1);
            assert!(ops.joins().is_empty());
            assert_eq!(ops.installs(), 0);
        }

        // Control: where the ALab vault answers, its join password is the one question.
        let sys = fake();
        sys.answer(None);
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        ops.alab_answers();
        run(&sys, &ops, Mode::Apply);
        assert_eq!(sys.calls(), [format!("ask secret {PASSWORD_PROMPT}")]);
        assert_eq!(ops.joins().len(), 1);

        // macOS refusing aterm the local network is named, and nothing is asked either.
        let sys = fake();
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        *ops.probed.lock().unwrap() = Some(Err(aterm_secrets::Error::new(format!(
            "fetching http://alab-secrets.local:7484/v1/alab: dns: alab-secrets.local: {}",
            aterm_http::LOCAL_NETWORK_REFUSED
        ))));
        let rows = run(&sys, &ops, Mode::Apply);
        assert_eq!(rows[0].why.as_deref(), Some(LAN_REFUSED));
        assert_eq!(asked(&sys), 0);
    }

    fn pending_look() -> aterm_secrets::Result<ProbeOutcome> {
        Err(aterm_secrets::Error::new(format!(
            "fetching http://alab-secrets.local:7484/v1/alab: {}",
            io::Error::from_raw_os_error(libc::EHOSTUNREACH)
        )))
    }

    #[test]
    fn a_look_macos_holds_for_the_local_network_answer_waits_for_it_without_a_prompt() {
        let sys = fake();
        sys.answer(None);
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        ops.alab_answers();
        *ops.looks.lock().unwrap() = vec![pending_look(), pending_look()].into();
        run(&sys, &ops, Mode::Apply);
        assert_eq!(ops.probes(), 3);
        assert_eq!(*ops.pauses.lock().unwrap(), 2);
        assert_eq!(
            sys.calls(),
            [format!("ask secret {PASSWORD_PROMPT}")],
            "no vault prompt: the ALab vault answered once macOS let it"
        );
        let said = ops.said.lock().unwrap().clone();
        assert_eq!(
            said.iter()
                .filter(|line| *line == LOCAL_NETWORK_ASK)
                .count(),
            1,
            "{said:?}"
        );

        // Held to the hang detector, init goes on without the vault, still without a prompt,
        // and its row says what is unanswered rather than that no vault is here.
        let sys = fake();
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        *ops.probed.lock().unwrap() = Some(pending_look());
        let rows = run(&sys, &ops, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::NotApplicable, "{rows:?}");
        assert_eq!(rows[0].why.as_deref(), Some(LAN_UNANSWERED));
        assert_eq!(ops.probes(), LOCAL_NETWORK_TRIES);
        assert_eq!(asked(&sys), 0);
    }

    #[test]
    fn a_look_kept_for_this_network_spares_the_next_and_a_held_one_is_not_kept() {
        use aterm_secrets::heard::{self, Heard, Verdict};
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(root.path());
        let here = || Some("192.168.4.0/24".to_owned());
        let asked = Cell::new(0);
        let probe = |answer: aterm_secrets::Result<ProbeOutcome>| {
            let asked = &asked;
            move || {
                asked.set(asked.get() + 1);
                answer
            }
        };
        let other = || Ok(ProbeOutcome::AnotherVault("another vault".into()));
        let looked = look_here_with(root.path(), here(), 1_000, probe(other()));
        assert!(matches!(looked, Ok(ProbeOutcome::AnotherVault(_))));
        assert_eq!(asked.get(), 1);
        assert_eq!(
            heard::read(&paths),
            Some(Verdict {
                at: 1_000,
                heard: Heard::Absent,
                network: "192.168.4.0/24".into(),
            })
        );
        // The next run on this network makes no request.
        let looked = look_here_with(root.path(), here(), 2_000, probe(other()));
        assert_eq!(looked.unwrap(), ProbeOutcome::Absent);
        assert_eq!(asked.get(), 1, "no look on a network a look found empty");
        // Another network is looked at again; a look macOS held is not kept.
        let looked = look_here_with(
            root.path(),
            Some("10.1.0.0/16".into()),
            2_000,
            probe(pending_look()),
        );
        assert!(looked.is_err());
        assert_eq!(asked.get(), 2);
        assert_eq!(heard::read(&paths).unwrap().network, "192.168.4.0/24");
        // Nor is a 404, an answer that is no vault, a timeout or a refused connect: none is an
        // answer about the network.
        let elsewhere = || Some("172.16.0.0/12".to_owned());
        for (n, failed) in [
            Ok(ProbeOutcome::Absent),
            Ok(ProbeOutcome::NotThisVault("503 Service Unavailable".into())),
            Err(aterm_secrets::Error::new("request deadline exceeded")),
            Err(aterm_secrets::Error::new(
                "connect: Connection refused (os error 61)",
            )),
        ]
        .into_iter()
        .enumerate()
        {
            let _ = look_here_with(root.path(), elsewhere(), 3_000 + n as u64, probe(failed));
            assert_eq!(
                heard::read(&paths).unwrap().network,
                "192.168.4.0/24",
                "{n}"
            );
        }
        assert_eq!(asked.get(), 6, "each was asked, none kept");
    }

    #[test]
    fn the_alab_vault_answering_is_joined_asking_only_for_the_join_password() {
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
        *ops.requests.lock().unwrap() = true;
        let rows = run(&sys, &ops, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
        assert_report_matches_current(&sys, &ops, &rows[0]);
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
        assert_eq!(
            *ops.said.lock().unwrap(),
            [
                "the ALab vault answers on this network".to_owned(),
                format!("joining {}…", aterm_secrets::ALAB_DEFAULT_URL)
            ]
        );
    }

    #[test]
    fn a_named_source_is_joined_as_typed_and_shown_without_credentials() {
        let sys = fake();
        let url = "http://vault.example:7484/v1/lab?token=private-query#private-pin";
        sys.answer(None);
        let ops = Scripted::new(vec![
            Ok(unconfigured()),
            Ok(unconfigured()),
            Ok(configured(Membership::In { seq: 1, heard: 0 })),
        ]);
        let rows = run_with(&sys, &ops, Mode::Apply, Some(url));
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
        assert_eq!(ops.joins(), [(url.to_owned(), true)]);
        assert_eq!(*ops.checks.lock().unwrap(), [url]);
        assert_eq!(
            ops.probes(),
            0,
            "a named vault needs no look for the ALab one"
        );
        let report = aterm_json::to_string(&rows).unwrap();
        assert!(!report.contains("private-query"), "{report}");
        assert!(!report.contains("private-pin"), "{report}");
        assert!(
            report.contains("http://vault.example:7484/v1/lab"),
            "{report}"
        );
    }

    #[test]
    fn a_named_vault_that_holds_none_is_asked_for_again_and_never_joined() {
        const TYPO: &str = "http://vualt.example/v1/lab";
        let unanswered =
            || Err("fetching http://vualt.example/v1/lab: dns: no such host".to_owned());
        for typed in [Some("none"), Some("SKIP"), Some(" None "), None] {
            let sys = fake();
            sys.answer(typed);
            let ops = Scripted::new(vec![Ok(unconfigured())]);
            *ops.checked.lock().unwrap() = vec![unanswered()].into();
            let rows = run_with(&sys, &ops, Mode::Apply, Some(TYPO));
            assert_eq!(rows[0].outcome, Outcome::NotApplicable, "{rows:?}");
            assert_eq!(rows[0].why.as_deref(), Some(SKIPPED));
            assert_eq!(sys.calls(), [format!("ask {PROMPT}")]);
            assert!(ops.joins().is_empty());
            assert_eq!(ops.installs(), 0);
            let said = ops.said.lock().unwrap().clone();
            assert!(said[0].contains("dns: no such host"), "{said:?}");
        }

        let retyped = "http://vault.example/v1/lab";
        let sys = fake();
        sys.answer(Some(retyped));
        sys.answer(None);
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        *ops.checked.lock().unwrap() = vec![unanswered()].into();
        *ops.outcome.lock().unwrap() = Some(Ok(JoinOutcome::Pending {
            code: CODE.into(),
            host: "m22".into(),
        }));
        run_with(&sys, &ops, Mode::Apply, Some(TYPO));
        assert_eq!(*ops.checks.lock().unwrap(), [TYPO, retyped]);
        assert_eq!(ops.joins(), [(retyped.to_owned(), true)], "never the typo");
    }

    #[test]
    fn an_unpinned_vault_is_trusted_at_the_prompt_and_joined_with_its_pin() {
        let url = "https://vault.example/v1/lab";
        let context = ctx(&[user("alice", 501)]);
        let unpinned = || {
            Ok(Checked::Unpinned {
                fingerprint: "0123-4567-89ab-cdef".into(),
                pin: "c2lnbmVy".into(),
            })
        };
        let sys = fake();
        sys.answer(Some("y"));
        sys.answer(None);
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        *ops.checked.lock().unwrap() = vec![unpinned()].into();
        *ops.outcome.lock().unwrap() = Some(Ok(JoinOutcome::Pending {
            code: CODE.into(),
            host: "m22".into(),
        }));
        let port: Arc<dyn SecretsOps> = ops.clone();
        let step = items(&sys, &context, port, Mode::Apply, Some(url)).unwrap();
        let prompts = sys.calls();
        assert!(
            prompts[0].starts_with("ask The vault is signed by 0123-4567-89ab-cdef"),
            "the signer is asked before anything runs: {prompts:?}"
        );
        assert_eq!(prompts[1], format!("ask secret {PASSWORD_PROMPT}"));
        engine::run(&step.items, Mode::Apply, &sys, &context, &mut NoJournal);
        assert_eq!(
            asked(&sys),
            2,
            "nothing is asked once the run makes changes"
        );
        assert_eq!(ops.joins(), [(format!("{url}#c2lnbmVy"), true)]);

        let sys = fake();
        sys.answer(Some("n"));
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        *ops.checked.lock().unwrap() = vec![unpinned()].into();
        let rows = run_with(&sys, &ops, Mode::Apply, Some(url));
        assert_eq!(rows[0].outcome, Outcome::NotApplicable, "{rows:?}");
        assert!(ops.joins().is_empty());
    }

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

        let sys = fake();
        sys.answer(Some("skip"));
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        let rows = run_with(
            &sys,
            &ops,
            Mode::Apply,
            Some("https://vault.example/v1/lab"),
        );
        assert_eq!(rows[0].outcome, Outcome::NotApplicable, "{rows:?}");
        assert!(ops.joins().is_empty());
        assert_eq!(ops.installs(), 0);
    }

    #[test]
    fn plan_and_check_never_prompt_probe_or_join_an_unconfigured_mac() {
        let sys = fake();
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

        *ops.terminal.lock().unwrap() = false;
        let rows = run_with(
            &sys,
            &ops,
            Mode::Apply,
            Some("https://vault.example/v1/lab"),
        );
        assert_eq!(rows[0].why.as_deref(), Some(UNCONFIGURED));
        assert!(ops.checks.lock().unwrap().is_empty(), "nor a named vault");
    }

    #[test]
    fn a_join_that_waits_for_an_admin_stands_aside_naming_the_code() {
        let sys = fake();
        sys.answer(None);
        let ops = Scripted::new(vec![
            Ok(unconfigured()),
            Ok(unconfigured()),
            Ok(configured(Membership::Pending)),
        ]);
        ops.alab_answers();
        *ops.requests.lock().unwrap() = true;
        *ops.outcome.lock().unwrap() = Some(Ok(JoinOutcome::Pending {
            code: CODE.into(),
            host: "m22".into(),
        }));
        let rows = run(&sys, &ops, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
        assert!(matches!(rows[0].stood_aside, Some(StoodAside::Waiting(_))));
        let why = rows[0].why.as_deref().unwrap();
        assert!(why.starts_with("waiting for an admin: "), "{why}");
        assert!(
            why.contains(&format!("aterm secrets approve {CODE}")),
            "{why}"
        );
        assert!(!why.contains("mistyped"), "no password was typed: {why}");
        assert_eq!(*ops.passwords.lock().unwrap(), [false]);
        assert_eq!(
            *ops.waits.lock().unwrap(),
            [Duration::ZERO],
            "the join is not awaited"
        );
        assert!(ops.windows.lock().unwrap().is_empty(), "nor is an approval");
        assert_eq!(*ops.installed_by_join.lock().unwrap(), [1]);
        assert_eq!(*ops.hooked.lock().unwrap(), [true]);
        assert_eq!(ops.installs(), 1, "the subscriber finishes the join");
    }

    #[test]
    fn a_join_that_gave_up_waiting_on_a_mac_the_subscriber_let_in_is_done() {
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
    fn a_password_join_not_approved_in_its_window_asks_for_the_password_again() {
        let pending = || {
            Some(Ok(JoinOutcome::Pending {
                code: CODE.into(),
                host: "m22".into(),
            }))
        };
        let sys = fake();
        sys.answer(Some("mistyped"));
        sys.answer(Some("the-right-one"));
        let ops = Scripted::new(vec![
            Ok(unconfigured()),
            Ok(unconfigured()),
            Ok(configured(Membership::Pending)),
            Ok(configured(Membership::Pending)),
            Ok(configured(Membership::In { seq: 8, heard: 0 })),
        ]);
        ops.alab_answers();
        *ops.outcome.lock().unwrap() = pending();
        *ops.approvals.lock().unwrap() = vec![false, true].into();
        let rows = run(&sys, &ops, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
        let asks: Vec<String> = sys
            .calls()
            .into_iter()
            .filter(|call| call.starts_with("ask "))
            .collect();
        assert_eq!(
            asks,
            [
                format!("ask secret {PASSWORD_PROMPT}"),
                format!("ask secret {RETYPE_PROMPT}")
            ]
        );
        assert_eq!(
            sys.calls().iter().filter(|c| *c == "catch stops").count(),
            2,
            "Ctrl-C is caught for each wait"
        );
        assert_eq!(*ops.passwords.lock().unwrap(), [true, true]);
        assert_eq!(*ops.windows.lock().unwrap(), [JOIN_WINDOW, JOIN_WINDOW]);
        assert_eq!(ops.installs(), 1);

        // Enter at it keeps waiting for approval by code: a row that waits, naming the code
        // and how a mistyped password is fixed.
        let sys = fake();
        sys.answer(Some("mistyped"));
        sys.answer(None);
        let ops = Scripted::new(vec![
            Ok(unconfigured()),
            Ok(unconfigured()),
            Ok(configured(Membership::Pending)),
        ]);
        ops.alab_answers();
        *ops.outcome.lock().unwrap() = pending();
        let rows = run(&sys, &ops, Mode::Apply);
        assert!(matches!(rows[0].stood_aside, Some(StoodAside::Waiting(_))));
        let why = rows[0].why.as_deref().unwrap();
        assert!(
            why.contains(&format!("aterm secrets approve {CODE}")),
            "{why}"
        );
        assert!(why.contains("mistyped join password"), "{why}");
        assert_eq!(ops.joins().len(), 1);
    }

    #[test]
    fn ctrl_c_during_the_approval_wait_ends_only_the_wait_and_github_waits_for_the_join() {
        const PROFILE: &[u8] =
            b"[github]\naccount='octocat'\nowner='octocat'\ntoken_file='~/.secrets/github'\n";
        const INIT_TOML: &str = "/Users//alice/.config/aterm/init.toml";
        let configured_github = |sys: &Fake| {
            sys.file(INIT_TOML, PROFILE).stat_of(
                INIT_TOML,
                Stat {
                    uid: 501,
                    gid: 20,
                    mode: 0o644,
                    dir: false,
                    symlink: false,
                    len: PROFILE.len() as u64,
                },
            );
        };
        let context = ctx(&[user("alice", 501)]);
        let github_after = |rows: &[ItemReport]| {
            let auth = rows
                .iter()
                .find(|row| row.family == "github")
                .expect("the GitHub rows are still produced");
            assert!(
                auth.why
                    .as_deref()
                    .unwrap()
                    .starts_with("waits for init.secrets@alice: waiting for an admin"),
                "{auth:?}"
            );
            assert!(matches!(auth.stood_aside, Some(StoodAside::Waiting(_))));
        };

        let sys = StopsAt {
            inner: fake(),
            stop: "",
            sigint: true,
        };
        configured_github(&sys.inner);
        sys.inner.answer(Some("the-join-password"));
        let ops = Scripted::new(vec![
            Ok(unconfigured()),
            Ok(unconfigured()),
            Ok(configured(Membership::Pending)),
        ]);
        ops.alab_answers();
        *ops.outcome.lock().unwrap() = Some(Ok(JoinOutcome::Pending {
            code: CODE.into(),
            host: "m22".into(),
        }));
        *ops.approvals.lock().unwrap() = vec![true].into();
        let port: Arc<dyn SecretsOps> = ops.clone();
        let step = items(&sys, &context, port, Mode::Apply, None).unwrap();
        let mut all = step.items;
        all.extend(crate::init_github::items(&sys, &context, step.waited_on.as_deref()).unwrap());
        let rows = engine::run(&all, Mode::Apply, &sys, &context, &mut NoJournal);
        assert!(
            matches!(rows[0].stood_aside, Some(StoodAside::Waiting(_))),
            "{rows:?}"
        );
        assert_eq!(*ops.windows.lock().unwrap(), [JOIN_WINDOW]);
        assert_eq!(
            sys.inner
                .calls()
                .iter()
                .filter(|c| c.starts_with("ask "))
                .count(),
            1,
            "a stopped wait asks nothing more"
        );
        github_after(&rows);

        // The next run, this Mac still waiting: the GitHub credential waits, not fails.
        let sys = fake();
        configured_github(&sys);
        let ops = Scripted::new(vec![Ok(configured(Membership::Pending))]);
        ops.subscribed(Ok(installed_state()));
        let port: Arc<dyn SecretsOps> = ops.clone();
        let step = items(&sys, &context, port, Mode::Apply, None).unwrap();
        let mut all = step.items;
        all.extend(crate::init_github::items(&sys, &context, step.waited_on.as_deref()).unwrap());
        let rows = engine::run(&all, Mode::Apply, &sys, &context, &mut NoJournal);
        github_after(&rows);
    }

    #[test]
    fn ctrl_c_at_a_prompt_stops_the_run_with_nothing_joined() {
        let sys = StopsAt {
            inner: fake(),
            stop: PASSWORD_PROMPT,
            sigint: false,
        };
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        ops.alab_answers();
        let context = ctx(&[user("alice", 501)]);
        let port: Arc<dyn SecretsOps> = ops.clone();
        let stopped = items(&sys, &context, port, Mode::Apply, None).err();
        assert_eq!(stopped.as_deref(), Some(STOPPED_AT_PASSWORD));
        assert!(ops.joins().is_empty(), "no key, no source, no request");
        assert_eq!(ops.installs(), 0);

        let sys = StopsAt {
            inner: fake(),
            stop: PASSWORD_PROMPT,
            sigint: false,
        };
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        let port: Arc<dyn SecretsOps> = ops.clone();
        let stopped = items(
            &sys,
            &context,
            port,
            Mode::Apply,
            Some("https://vault.example/v1/lab"),
        )
        .err();
        assert_eq!(stopped.as_deref(), Some(STOPPED_AT_PASSWORD));
        assert!(ops.joins().is_empty());

        let sys = StopsAt {
            inner: fake(),
            stop: PROMPT,
            sigint: false,
        };
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        *ops.checked.lock().unwrap() = vec![Err("dns: no such host".into())].into();
        let port: Arc<dyn SecretsOps> = ops.clone();
        let stopped = items(
            &sys,
            &context,
            port,
            Mode::Apply,
            Some("https://vault.example/v1/lab"),
        )
        .err();
        assert_eq!(
            stopped.as_deref(),
            Some("stopped at the secrets vault prompt; nothing was changed")
        );
        assert!(ops.joins().is_empty());

        // Ctrl-C at the question whether to trust a vault's key stops the run too, rather than
        // reading as a No and going on.
        let sys = StopsAt {
            inner: fake(),
            stop: "The vault is signed by ",
            sigint: false,
        };
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        *ops.checked.lock().unwrap() = vec![Ok(Checked::Unpinned {
            fingerprint: "0123-4567-89ab-cdef".into(),
            pin: "c2lnbmVy".into(),
        })]
        .into();
        let port: Arc<dyn SecretsOps> = ops.clone();
        let stopped = items(
            &sys,
            &context,
            port,
            Mode::Apply,
            Some("https://vault.example/v1/lab"),
        )
        .err();
        assert_eq!(stopped.as_deref(), Some(STOPPED_AT_TRUST));
        assert!(ops.joins().is_empty());

        let sys = fake();
        let ops = Scripted::new(vec![
            Ok(unconfigured()),
            Ok(unconfigured()),
            Ok(configured(Membership::Plain)),
        ]);
        let rows = run_with(&sys, &ops, Mode::Apply, Some("~/vault.txt"));
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
        assert_eq!(asked(&sys), 0, "a path has no join password");
        assert_eq!(*ops.passwords.lock().unwrap(), [false]);
    }

    #[test]
    fn a_pending_mac_needs_an_admin_and_a_revoked_one_is_blocked() {
        let sys = fake();
        let ops = Scripted::new(vec![Ok(configured(Membership::Pending))]);
        ops.subscribed(Ok(installed_state()));
        let rows = run(&sys, &ops, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
        assert!(
            matches!(rows[0].stood_aside, Some(StoodAside::Waiting(_))),
            "the subscriber finishes it once approved: {rows:?}"
        );
        let why = rows[0].why.as_deref().unwrap();
        assert!(
            why.contains(
                "approve this Mac on an admin Mac: aterm secrets approve abcd-ef23-ghij-k234"
            ),
            "{why}"
        );
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
        let pasted = ["ghp", "_", &"Q7".repeat(18)].concat();
        let sys = fake();
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        let rows = run_with(&sys, &ops, Mode::Apply, Some(&pasted));
        assert_eq!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
        let report = aterm_json::to_string(&rows).unwrap();
        assert!(!report.contains(&pasted), "{report}");
        assert!(ops.joins().is_empty(), "nothing is joined with it");

        let sys = fake();
        sys.answer(None);
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        ops.alab_answers();
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

        for requests in [false, true] {
            let sys = fake();
            sys.answer(None);
            let ops = Scripted::new(vec![Ok(unconfigured())]);
            ops.alab_answers();
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
        assert!(!confirm_signer(&fake(), "0123-4567-89ab-cdef"));
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
        assert!(items(&sys, &two, ops, Mode::Plan, None).is_err());
        assert!(live(&two, None).is_err());
        assert!(unattended(&two).is_err());
    }

    const SETUP_WAIT: Duration = Duration::from_secs(20);

    fn setup_pass(
        ops: &Arc<Scripted>,
        source: &str,
        password: &mut Option<SecretBytes>,
        refuse: bool,
    ) -> ItemReport {
        setup_pass_over(ops, source, Replaces::Any, password, refuse)
    }

    fn setup_pass_over(
        ops: &Arc<Scripted>,
        source: &str,
        replaces: Replaces,
        password: &mut Option<SecretBytes>,
        refuse: bool,
    ) -> ItemReport {
        let context = ctx(&[user("alice", 501)]);
        let port: Arc<dyn SecretsOps> = ops.clone();
        let mut journal = Lines {
            ops: Arc::clone(ops),
            refuse,
        };
        let mut rows = crate::init::run_initial_secrets_with(
            &fake(),
            &context,
            port,
            source,
            replaces,
            password,
            SETUP_WAIT,
            &mut journal,
        )
        .expect("the stage runs");
        assert_eq!(rows.len(), 1);
        rows.remove(0)
    }

    fn journaled_first(log: &[String]) {
        let (line, changes) = log.split_first().expect("a line and its changes");
        assert!(line.starts_with("line init.secrets@alice: "), "{log:?}");
        assert!(!changes.is_empty(), "{log:?}");
        assert!(
            changes.iter().all(|change| !change.starts_with("line ")),
            "one line per apply: {log:?}"
        );
        for change in changes {
            let undone = match change.as_str() {
                "install" => "aterm secrets agent off",
                "sync" => "~/.secrets/.replaced",
                join if join.starts_with("join ") => "source.toml",
                keep if keep.starts_with("keep ") => "~/.config/aterm/secrets/before-join-",
                other => panic!("an unknown change {other}"),
            };
            assert!(line.contains(undone), "{change} is not undone by {line}");
        }
    }

    fn password() -> SecretBytes {
        SecretBytes::from(["setup", "-", &"Wq".repeat(8)].concat().as_bytes())
    }

    #[test]
    fn a_selected_vault_journals_each_change_before_it_is_made() {
        let source = aterm_secrets::ALAB_DEFAULT_URL;
        let ops = Scripted::new(vec![
            Ok(unconfigured()),
            Ok(unconfigured()),
            Ok(configured(Membership::In {
                seq: 7,
                heard: 1_800_000_000,
            })),
        ]);
        *ops.requests.lock().unwrap() = true;
        let mut held = Some(password());
        let row = setup_pass(&ops, source, &mut held, false);
        assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
        assert_eq!(row.from.as_deref(), Some("no vault"));
        assert_eq!(ops.changes(), [format!("join {source}"), "install".into()]);
        journaled_first(&ops.log());
        assert_eq!(ops.joins(), [(source.to_owned(), false)], "unattended");
        assert_eq!(*ops.waits.lock().unwrap(), [SETUP_WAIT]);
        assert_eq!(*ops.passwords.lock().unwrap(), [true]);
        assert!(held.is_none(), "the proof is with the vault");
        assert_eq!(*ops.reached.lock().unwrap(), 1, "asked before the line");

        let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
        *ops.unwritten.lock().unwrap() = true;
        let row = setup_pass(&ops, source, &mut None, false);
        assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
        assert_eq!(ops.changes(), ["install", "sync"]);
        journaled_first(&ops.log());
        assert!(ops.joins().is_empty());

        let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
        ops.subscribed(Ok(installed_state()));
        *ops.unwritten.lock().unwrap() = true;
        let row = setup_pass(&ops, source, &mut None, false);
        assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
        assert_eq!(ops.changes(), ["sync"]);
        journaled_first(&ops.log());
        assert!(
            !ops.log()[0].contains("aterm secrets agent off"),
            "{:?}",
            ops.log()
        );

        let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
        ops.subscribed(Ok(installed_state()));
        let row = setup_pass(&ops, source, &mut None, false);
        assert_eq!(row.outcome, Outcome::Ok, "{row:?}");
        assert!(ops.log().is_empty());
        assert_eq!(*ops.reached.lock().unwrap(), 0, "no network");
        assert!(ops.said.lock().unwrap().is_empty());
    }

    #[test]
    fn a_setup_from_a_disk_image_waits_to_be_opened_from_applications() {
        let source = aterm_secrets::ALAB_DEFAULT_URL;
        let image = PathBuf::from("/Volumes/aterm/aterm.app/Contents/MacOS/aterm");
        let moved =
            "move aterm to Applications and open it from there, and setup finishes this step";
        assert!(fleeting_wait(&image).contains(moved));
        assert!(!fleeting_wait(&image).contains("--program"));

        let ops = Scripted::new(vec![
            Ok(unconfigured()),
            Ok(unconfigured()),
            Ok(configured(Membership::In { seq: 7, heard: 0 })),
        ]);
        *ops.requests.lock().unwrap() = true;
        *ops.fleeting.lock().unwrap() = Some(image.clone());
        let row = setup_pass(&ops, source, &mut Some(password()), false);
        assert_eq!(row.outcome, Outcome::Blocked, "{row:?}");
        assert_eq!(row.why.as_deref(), Some(fleeting_wait(&image).as_str()));
        assert_eq!(ops.changes(), [format!("join {source}"), "install".into()]);
        journaled_first(&ops.log());
        assert_eq!(*ops.hooked.lock().unwrap(), [false], "not in place");

        let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
        *ops.fleeting.lock().unwrap() = Some(image.clone());
        let row = setup_pass(&ops, source, &mut None, false);
        assert_eq!(row.outcome, Outcome::Blocked, "{row:?}");
        assert_eq!(row.why.as_deref(), Some(fleeting_wait(&image).as_str()));
        assert_eq!(ops.changes(), ["install"]);

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
        *ops.fleeting.lock().unwrap() = Some(image.clone());
        let row = setup_pass(&ops, source, &mut None, false);
        assert_eq!(row.outcome, Outcome::Blocked, "{row:?}");
        let why = row.why.as_deref().unwrap();
        assert!(why.starts_with("waiting for an admin: "), "{why}");
        assert!(
            why.contains(&format!("aterm secrets approve {CODE}")),
            "{why}"
        );
        assert!(
            why.ends_with(&format!("; {}", fleeting_wait(&image))),
            "{why}"
        );

        let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
        let refusal = aterm_secrets::service::service_program(&image, &|_| false)
            .expect_err("a disk image is refused")
            .to_string();
        *ops.installed.lock().unwrap() = Some(Err(aterm_secrets::Error::new(refusal.clone())));
        let row = setup_pass(&ops, source, &mut None, false);
        assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
        assert_eq!(
            row.why,
            Some(format!("the subscriber could not be installed: {refusal}"))
        );

        let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
        *ops.fleeting.lock().unwrap() = Some(image.clone());
        let rows = run(&fake(), &ops, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Failed, "{rows:?}");
        let why = rows[0].why.as_deref().unwrap();
        assert!(why.contains("--program"), "{why}");
        assert!(!why.contains(moved), "{why}");
    }

    #[test]
    fn the_setup_stage_journals_into_the_accounts_journal() {
        let sys = fake();
        let ops = Scripted::new(vec![
            Ok(unconfigured()),
            Ok(unconfigured()),
            Ok(configured(Membership::In { seq: 7, heard: 0 })),
        ]);
        *ops.requests.lock().unwrap() = true;
        let port: Arc<dyn SecretsOps> = ops.clone();
        let rows = crate::init::run_initial_secrets(
            &sys,
            &ctx(&[user("alice", 501)]),
            port,
            aterm_secrets::ALAB_DEFAULT_URL,
            Replaces::Any,
            &mut None,
            SETUP_WAIT,
        )
        .unwrap();
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
        let journal = String::from_utf8(sys.written(Fake::JOURNAL).expect("journaled")).unwrap();
        let [line] = journal.lines().collect::<Vec<_>>()[..] else {
            panic!("one line: {journal}");
        };
        let line: aterm_json::Value = aterm_json::from_str(line).unwrap();
        assert_eq!(line["id"].as_str(), Some("init.secrets@alice"));
        assert_eq!(line["from"].as_str(), Some("no vault"));
        let revert = line["revert"].as_str().unwrap();
        assert!(revert.contains("aterm secrets agent off"), "{revert}");
        assert!(
            revert.contains("~/.config/aterm/secrets/state.toml"),
            "{revert}"
        );
        assert!(revert.contains("~/.secrets/.replaced"), "{revert}");
    }

    #[test]
    fn a_refused_journal_line_refuses_the_selected_vaults_change() {
        let source = aterm_secrets::ALAB_DEFAULT_URL;
        for refuse in [true, false] {
            let ops = Scripted::new(vec![
                Ok(unconfigured()),
                Ok(unconfigured()),
                Ok(configured(Membership::In { seq: 7, heard: 0 })),
            ]);
            *ops.requests.lock().unwrap() = true;
            let mut held = Some(password());
            let row = setup_pass(&ops, source, &mut held, refuse);
            if refuse {
                assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
                assert!(
                    row.why
                        .as_deref()
                        .unwrap()
                        .starts_with("not applied: the journal refused the record"),
                    "{row:?}"
                );
                assert!(ops.log().is_empty(), "{:?}", ops.log());
                assert!(held.is_some(), "kept for the next try");
            } else {
                assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
                assert!(!ops.changes().is_empty());
            }
        }
        for refuse in [true, false] {
            let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
            *ops.unwritten.lock().unwrap() = true;
            let row = setup_pass(&ops, source, &mut None, refuse);
            assert_eq!(ops.changes().is_empty(), refuse, "{row:?}");
            assert_eq!(*ops.syncs.lock().unwrap(), u32::from(!refuse));
            assert_eq!(ops.installs(), u32::from(!refuse));
        }
    }

    #[test]
    fn an_unreachable_selected_vault_stands_aside_with_nothing_journaled_or_made() {
        let source = aterm_secrets::ALAB_DEFAULT_URL;
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        *ops.unreachable.lock().unwrap() = Some(Look::Held(
            "fetching http://alab-secrets.local:7484/v1/alab: No route to host (os error 65)"
                .into(),
        ));
        let mut held = Some(password());
        for attempt in 1..=5 {
            let row = setup_pass(&ops, source, &mut held, false);
            assert_eq!(row.outcome, Outcome::Blocked, "{row:?}");
            assert!(
                matches!(row.stood_aside, Some(StoodAside::Offline(_))),
                "{row:?}"
            );
            assert_eq!(
                row.why.as_deref(),
                Some(
                    "the vault does not answer, so nothing was changed: fetching http://alab-secrets.local:7484/v1/alab: No route to host (os error 65); if macOS asked whether this app may find devices on your local network and it was not allowed, turn it on in System Settings > Privacy & Security > Local Network"
                )
            );
            assert!(ops.log().is_empty(), "{:?}", ops.log());
            assert!(ops.joins().is_empty());
            assert_eq!(*ops.reached.lock().unwrap(), attempt);
            assert!(held.is_some());
        }
        let pending = Scripted::new(vec![Ok(configured(Membership::Pending))]);
        pending.subscribed(Ok(installed_state()));
        *pending.unreachable.lock().unwrap() =
            Some(Look::Held("No route to host (os error 65)".into()));
        let row = setup_pass(&pending, source, &mut None, false);
        assert!(
            row.outcome == Outcome::Blocked
                && matches!(row.stood_aside, Some(StoodAside::Waiting(_))),
            "{row:?}"
        );
        assert_eq!(*pending.reached.lock().unwrap(), 0);
        assert!(pending.log().is_empty());

        *ops.unreachable.lock().unwrap() = None;
        *ops.statuses.lock().unwrap() = vec![
            Ok(unconfigured()),
            Ok(unconfigured()),
            Ok(configured(Membership::In { seq: 7, heard: 0 })),
        ]
        .into();
        let row = setup_pass(&ops, source, &mut held, false);
        assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
        journaled_first(&ops.log());
        assert!(held.is_none());
    }

    #[test]
    fn a_pending_selected_vault_waits_and_a_new_password_asks_once_more() {
        let source = aterm_secrets::ALAB_DEFAULT_URL;
        let ops = Scripted::new(vec![Ok(configured(Membership::Pending))]);
        ops.subscribed(Ok(installed_state()));
        let row = setup_pass(&ops, source, &mut None, false);
        assert!(
            row.outcome == Outcome::Blocked
                && matches!(row.stood_aside, Some(StoodAside::Waiting(_))),
            "{row:?}"
        );
        assert!(ops.log().is_empty());

        let mut held = Some(password());
        *ops.outcome.lock().unwrap() = Some(Ok(JoinOutcome::Pending {
            code: CODE.into(),
            host: "m22".into(),
        }));
        let row = setup_pass(&ops, source, &mut held, false);
        assert_eq!(row.outcome, Outcome::Blocked, "{row:?}");
        assert!(
            matches!(row.stood_aside, Some(StoodAside::Waiting(_))),
            "{row:?}"
        );
        assert_eq!(
            ops.changes(),
            [
                "keep /Users//alice/.config/aterm/secrets/before-join-1800000000".to_owned(),
                format!("join {source}")
            ]
        );
        journaled_first(&ops.log());
        assert_eq!(*ops.passwords.lock().unwrap(), [true]);
        assert!(held.is_none(), "spent: its proof is queued");
        let row = setup_pass(&ops, source, &mut held, false);
        assert!(
            row.outcome == Outcome::Blocked
                && matches!(row.stood_aside, Some(StoodAside::Waiting(_))),
            "{row:?}"
        );
        assert_eq!(ops.joins().len(), 1);

        let mut held = Some(password());
        *ops.outcome.lock().unwrap() = Some(Err(aterm_secrets::Error::new(
            "posting the join request: Connection reset by peer (os error 54)",
        )));
        let row = setup_pass(&ops, source, &mut held, false);
        assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
        assert!(held.is_some(), "kept for the next try");
    }

    #[test]
    fn a_selected_vault_joins_only_when_this_mac_is_not_in_it() {
        let context = ctx(&[user("alice", 501)]);
        let planned = |status: Status, source: &str| {
            let ops = Scripted::new(vec![Ok(status)]);
            ops.subscribed(Ok(installed_state()));
            let vault = selected(&context, ops, source, SETUP_WAIT, Replaces::Any).unwrap();
            match vault.probe(&fake(), &context) {
                Probe::Change { from, .. } => vault.planned().join.then_some(from),
                _ => None,
            }
        };
        let alab = aterm_secrets::ALAB_DEFAULT_URL;
        let pending = || configured(Membership::Pending);
        assert_eq!(planned(pending(), alab), None);
        let mut elsewhere = pending();
        elsewhere.config.as_mut().unwrap().source =
            Source::Url("https://vault.example/v1/alab".into());
        assert_eq!(
            planned(elsewhere, alab).as_deref(),
            Some("vault alab (https://vault.example/v1/alab), not the one System State names")
        );
        let mut keyless = pending();
        keyless.machine = None;
        assert_eq!(
            planned(keyless, alab).as_deref(),
            Some("vault alab, but this Mac's key is missing")
        );
        assert!(planned(configured(Membership::Revoked { seq: 9 }), alab).is_some());
        assert_eq!(planned(unconfigured(), alab).as_deref(), Some("no vault"));

        let team = "https://vault.example/v1/team";
        let mut rotated = pending();
        let config = rotated.config.as_mut().unwrap();
        config.source = Source::Url(team.into());
        config.name = "team".into();
        config.signer = Some([8; 32]);
        config.followed = vec![[7; 32]];
        let pinned = |pin: [u8; 32]| format!("{team}#{}", aterm_secrets::b64::encode_padded(&pin));
        assert_eq!(planned(rotated.clone(), &pinned([7; 32])), None);
        assert_eq!(planned(rotated.clone(), &pinned([8; 32])), None);
        assert!(planned(rotated, &pinned([9; 32])).is_some());

        let mut plain = configured(Membership::Plain);
        plain.config = Some(Config {
            source: Source::Path("/Volumes/Keys/team.vault".into()),
            name: "team".into(),
            signer: None,
            followed: Vec::new(),
            host: "m22".into(),
        });
        plain.machine = None;
        assert_eq!(planned(plain, "/Volumes/Keys/team.vault"), None);
    }

    #[test]
    fn an_unreachable_vault_stands_aside_from_a_sync_with_nothing_journaled() {
        let source = aterm_secrets::ALAB_DEFAULT_URL;
        let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
        ops.subscribed(Ok(installed_state()));
        *ops.unwritten.lock().unwrap() = true;
        *ops.unreachable.lock().unwrap() = Some(Look::Unreached(
            "fetching http://alab-secrets.local:7484/v1/alab: Connection refused (os error 61)"
                .into(),
        ));
        let row = setup_pass(&ops, source, &mut None, false);
        assert_eq!(row.outcome, Outcome::Blocked, "{row:?}");
        assert!(
            matches!(row.stood_aside, Some(StoodAside::Offline(_))),
            "{row:?}"
        );
        assert_eq!(
            row.why.as_deref(),
            Some(
                "the vault does not answer, so nothing was changed: fetching http://alab-secrets.local:7484/v1/alab: Connection refused (os error 61)"
            ),
            "a refusal is no Local Network hold"
        );
        assert!(ops.log().is_empty(), "{:?}", ops.log());
        assert_eq!(*ops.reached.lock().unwrap(), 1);

        *ops.unreachable.lock().unwrap() = None;
        let row = setup_pass(&ops, source, &mut None, false);
        assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
        assert_eq!(ops.changes(), ["sync"]);
        journaled_first(&ops.log());
    }

    /// A member whose subscriber the person turned off (`aterm secrets agent off`) is no change
    /// to make; with no record (the control) it is a change that installs it. A new join turns
    /// sync on: its install clears the record.
    #[test]
    fn a_subscriber_turned_off_is_left_off_by_setup() {
        let source = aterm_secrets::ALAB_DEFAULT_URL;
        for off in [true, false] {
            let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
            *ops.turned_off.lock().unwrap() = off;
            let row = setup_pass(&ops, source, &mut None, false);
            if off {
                assert_eq!(row.outcome, Outcome::Ok, "{row:?}");
                assert!(
                    row.to.as_deref().is_some_and(|to| to.ends_with(SYNC_OFF)),
                    "{row:?}"
                );
                assert!(ops.changes().is_empty(), "{:?}", ops.changes());
            } else {
                assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
                assert_eq!(ops.changes(), ["install"]);
            }
        }

        let ops = Scripted::new(vec![Ok(unconfigured())]);
        *ops.turned_off.lock().unwrap() = true;
        let _ = setup_pass(&ops, source, &mut None, false);
        assert_eq!(ops.joins().len(), 1, "the join is made");
        assert_eq!(ops.installs(), 1, "a new join turns sync on");
        assert!(!ops.turned_off(), "and the record that it was off goes");
    }

    #[test]
    fn a_newer_sync_off_survives_an_already_probed_subscriber_repair() {
        struct OffBeforeChange {
            ops: Arc<Scripted>,
            off: bool,
            called: bool,
        }

        impl crate::engine::Journal for OffBeforeChange {
            fn before_change(
                &mut self,
                _id: &str,
                _from: &str,
                _to: &str,
                _revert: Option<&str>,
            ) -> Result<(), String> {
                self.called = true;
                *self.ops.turned_off.lock().unwrap() = self.off;
                Ok(())
            }
        }

        for off in [false, true] {
            let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
            let context = ctx(&[user("alice", 501)]);
            let port: Arc<dyn SecretsOps> = ops.clone();
            let mut journal = OffBeforeChange {
                ops: Arc::clone(&ops),
                off,
                called: false,
            };
            let rows = crate::init::run_initial_secrets_with(
                &fake(),
                &context,
                port,
                aterm_secrets::ALAB_DEFAULT_URL,
                Replaces::Any,
                &mut None,
                SETUP_WAIT,
                &mut journal,
            )
            .unwrap();
            assert!(
                journal.called,
                "the real converge change boundary was reached"
            );
            assert_eq!(rows.len(), 1);
            assert_eq!(
                ops.installs(),
                u32::from(!off),
                "an automatic repair must preserve a newer Sync off choice"
            );
            assert_eq!(ops.turned_off(), off);
            if off {
                assert!(
                    rows[0]
                        .to
                        .as_deref()
                        .is_some_and(|to| to.ends_with(SYNC_OFF))
                );
            }
            assert!(ops.joins().is_empty(), "a member needs no new join");
        }
    }

    #[test]
    fn a_newer_sync_off_cancels_an_already_planned_file_sync() {
        struct OffBeforeChange(Arc<Scripted>);
        impl crate::engine::Journal for OffBeforeChange {
            fn before_change(
                &mut self,
                _id: &str,
                _from: &str,
                _to: &str,
                _revert: Option<&str>,
            ) -> Result<(), String> {
                *self.0.turned_off.lock().unwrap() = true;
                self.0.subscribed(Ok(State::Absent));
                Ok(())
            }
        }
        for running in [false, true] {
            let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
            *ops.unwritten.lock().unwrap() = true;
            if running {
                ops.subscribed(Ok(installed_state()));
            }
            let context = ctx(&[user("alice", 501)]);
            let port: Arc<dyn SecretsOps> = ops.clone();
            let rows = crate::init::run_initial_secrets_with(
                &fake(),
                &context,
                port,
                aterm_secrets::ALAB_DEFAULT_URL,
                Replaces::Any,
                &mut None,
                SETUP_WAIT,
                &mut OffBeforeChange(Arc::clone(&ops)),
            )
            .unwrap();
            assert_eq!(*ops.syncs.lock().unwrap(), 0, "a newer Off stops file work");
            assert_eq!(ops.installs(), 0);
            assert!(ops.turned_off());
            assert!(
                *ops.unwritten.lock().unwrap(),
                "last-synced files stay as they are"
            );
            assert_eq!(rows[0].outcome, Outcome::Changed);
            assert!(
                rows[0]
                    .to
                    .as_deref()
                    .is_some_and(|to| to.ends_with(SYNC_OFF))
            );
        }
    }

    #[test]
    fn sync_off_keeps_the_last_synced_files_without_installing() {
        let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
        *ops.unwritten.lock().unwrap() = true;
        *ops.turned_off.lock().unwrap() = true;
        let row = setup_pass(&ops, aterm_secrets::ALAB_DEFAULT_URL, &mut None, false);
        assert_eq!(row.outcome, Outcome::Ok);
        assert!(ops.changes().is_empty());
        assert!(*ops.unwritten.lock().unwrap());
        assert!(ops.turned_off());
    }

    /// A subscriber that never keeps running through launchd's restart window fails its step,
    /// naming its log, and is never a wait; one that keeps running (the control) is changed.
    #[test]
    fn an_install_that_leaves_no_subscriber_running_is_a_failed_step_naming_its_log() {
        let source = aterm_secrets::ALAB_DEFAULT_URL;
        let log = "/Users//alice/.config/aterm/secrets/systems.alab.aterm-secrets.log";
        for dies in [true, false] {
            let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
            *ops.dies.lock().unwrap() = dies.then(|| {
                format!(
                    "the secrets subscriber was installed but does not keep running; its log is {log}"
                )
            });
            let row = setup_pass(&ops, source, &mut None, false);
            assert_eq!(ops.changes(), ["install"]);
            if dies {
                assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
                assert!(
                    row.why.as_deref().is_some_and(|why| why.contains(log)),
                    "{row:?}"
                );
            } else {
                assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
            }
        }
    }

    #[test]
    fn no_vault_at_the_source_stands_aside_before_a_line_or_a_key() {
        let source = "/Volumes/Keys/team.vault";
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        *ops.absent.lock().unwrap() = Some("/Volumes/Keys/team.vault: no vault there".into());
        let mut held = Some(password());
        for attempt in 1..=3 {
            let row = setup_pass(&ops, source, &mut held, false);
            assert_eq!(row.outcome, Outcome::Blocked, "{row:?}");
            assert!(
                matches!(row.stood_aside, Some(StoodAside::Offline(_))),
                "{row:?}"
            );
            assert_eq!(
                row.why.as_deref(),
                Some(
                    "no vault is there now, so nothing was changed: /Volumes/Keys/team.vault: no vault there"
                )
            );
            assert!(ops.log().is_empty(), "{:?}", ops.log());
            assert_eq!(*ops.reached.lock().unwrap(), attempt);
            assert!(held.is_some());
        }
        *ops.absent.lock().unwrap() = None;
        let mut joined = configured(Membership::In { seq: 7, heard: 0 });
        joined.config = Some(Config {
            source: Source::Path(source.into()),
            name: "team".into(),
            signer: None,
            followed: Vec::new(),
            host: "m22".into(),
        });
        *ops.statuses.lock().unwrap() =
            vec![Ok(unconfigured()), Ok(unconfigured()), Ok(joined)].into();
        let row = setup_pass(&ops, source, &mut held, false);
        assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
        assert_eq!(ops.joins(), [(source.to_owned(), false)]);
        journaled_first(&ops.log());
    }

    #[test]
    fn a_join_from_another_vault_keeps_it_and_its_line_puts_this_mac_back() {
        let source = aterm_secrets::ALAB_DEFAULT_URL;
        let team = || {
            let mut team = configured(Membership::In { seq: 4, heard: 0 });
            let config = team.config.as_mut().unwrap();
            config.source = Source::Url("https://vault.example/v1/team".into());
            config.name = "team".into();
            team
        };
        let ops = Scripted::new(vec![
            Ok(team()),
            Ok(team()),
            Ok(configured(Membership::In { seq: 7, heard: 0 })),
        ]);
        ops.subscribed(Ok(installed_state()));
        let row = setup_pass(&ops, source, &mut Some(password()), false);
        assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
        let kept = "~/.config/aterm/secrets/before-join-1800000000";
        let log = ops.log();
        assert_eq!(
            log[1..],
            [
                "keep /Users//alice/.config/aterm/secrets/before-join-1800000000".to_owned(),
                format!("join {source}"),
            ],
            "{log:?}"
        );
        journaled_first(&log);
        let line = &log[0];
        assert!(
            line.contains(&format!(
                "cp {kept}/source.toml {kept}/state.toml ~/.config/aterm/secrets/ and run aterm secrets sync"
            )),
            "{line}"
        );
        assert!(
            line.contains("rm each file aterm secrets ls names"),
            "{line}"
        );
        assert!(
            !line.contains("rm ~/.config/aterm/secrets/source.toml"),
            "{line}"
        );
        assert!(!line.contains("identity"), "the key was there: {line}");
        assert!(!line.contains("agent off"), "the subscriber ran: {line}");

        let ops = Scripted::new(vec![Ok(team())]);
        ops.subscribed(Ok(installed_state()));
        *ops.unkept.lock().unwrap() = Some("Permission denied (os error 13)".into());
        let mut held = Some(password());
        let row = setup_pass(&ops, source, &mut held, false);
        assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
        assert!(ops.joins().is_empty());
        assert!(held.is_some());

        let ops = Scripted::new(vec![
            Ok(unconfigured()),
            Ok(unconfigured()),
            Ok(configured(Membership::In { seq: 7, heard: 0 })),
        ]);
        let row = setup_pass(&ops, source, &mut None, false);
        assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
        assert_eq!(ops.changes(), [format!("join {source}"), "install".into()]);
        let line = &ops.log()[0];
        assert!(
            line.contains(
                "rm ~/.config/aterm/secrets/source.toml ~/.config/aterm/secrets/state.toml"
            ),
            "{line}"
        );
        assert!(
            line.contains("rm ~/.config/aterm/secrets/identity (the key the join made)"),
            "{line}"
        );
        assert!(!line.contains("before-join-"), "{line}");
    }

    #[test]
    fn an_apply_makes_only_what_the_probe_planned_and_its_line_undoes() {
        let source = aterm_secrets::ALAB_DEFAULT_URL;
        let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
        ops.subscribed(Ok(installed_state()));
        *ops.unwritten.lock().unwrap() = true;
        *ops.meanwhile.lock().unwrap() = Some((
            configured(Membership::In { seq: 7, heard: 0 }),
            State::Absent,
        ));
        let row = setup_pass(&ops, source, &mut None, false);
        assert_eq!(ops.changes(), ["sync"], "{row:?}");
        assert!(!ops.log()[0].contains("agent off"), "{:?}", ops.log());
        let row = setup_pass(&ops, source, &mut None, false);
        assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
        assert_eq!(ops.changes(), ["sync", "install"]);
        assert!(ops.log()[2].contains("agent off"), "{:?}", ops.log());

        let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
        ops.subscribed(Ok(installed_state()));
        *ops.unwritten.lock().unwrap() = true;
        *ops.meanwhile.lock().unwrap() = Some((
            configured(Membership::Revoked { seq: 8 }),
            installed_state(),
        ));
        let row = setup_pass(&ops, source, &mut Some(password()), false);
        assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
        assert!(
            row.why
                .as_deref()
                .unwrap()
                .starts_with("this Mac's vault state changed after it was read"),
            "{row:?}"
        );
        assert!(ops.changes().is_empty(), "{:?}", ops.log());
        *ops.unwritten.lock().unwrap() = false;
        *ops.statuses.lock().unwrap() = vec![
            Ok(configured(Membership::Revoked { seq: 8 })),
            Ok(configured(Membership::Revoked { seq: 8 })),
            Ok(configured(Membership::In { seq: 9, heard: 0 })),
        ]
        .into();
        let row = setup_pass(&ops, source, &mut Some(password()), false);
        assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
        assert_eq!(ops.joins().len(), 1);
        assert!(
            ops.log()[1].contains("an admin removed this Mac"),
            "{:?}",
            ops.log()
        );

        let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
        *ops.spoiled_by_install.lock().unwrap() = true;
        setup_pass(&ops, source, &mut None, false);
        assert_eq!(ops.changes(), ["install"], "{:?}", ops.log());
        assert!(!ops.log()[0].contains(".replaced"), "{:?}", ops.log());
        *ops.spoiled_by_install.lock().unwrap() = false;
        let row = setup_pass(&ops, source, &mut None, false);
        assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
        assert_eq!(ops.changes(), ["install", "sync"]);
        assert!(ops.log()[2].contains(".replaced"), "{:?}", ops.log());
    }

    fn joined_elsewhere() -> Status {
        let mut status = configured(Membership::In { seq: 3, heard: 0 });
        let config = status.config.as_mut().unwrap();
        config.source = Source::Url("https://vault.example/v1/me?k=fixture-query".into());
        config.name = "me".into();
        status
    }

    #[test]
    fn a_vault_joined_while_the_vault_is_asked_is_not_replaced() {
        let source = aterm_secrets::ALAB_DEFAULT_URL;
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        *ops.meanwhile.lock().unwrap() = Some((joined_elsewhere(), installed_state()));
        let mut held = Some(password());
        let row = setup_pass(&ops, source, &mut held, false);
        assert_eq!(row.outcome, Outcome::Blocked, "{row:?}");
        assert!(
            matches!(row.stood_aside, Some(StoodAside::Lock(_))),
            "{row:?}"
        );
        assert_eq!(
            row.why.as_deref(),
            Some(
                "this Mac's vault changed while the vault was asked (now vault me (https://vault.example/v1/me)), so nothing was changed; the next try reads it"
            )
        );
        assert!(ops.log().is_empty(), "{:?}", ops.log());
        assert!(ops.joins().is_empty());
        assert!(held.is_some(), "kept for a try that joins");
    }

    #[test]
    fn an_apply_joins_only_over_the_vault_its_probe_read() {
        let context = ctx(&[user("alice", 501)]);
        let ops = Scripted::new(vec![Ok(unconfigured()), Ok(joined_elsewhere())]);
        let port: Arc<dyn SecretsOps> = ops.clone();
        let vault = selected(
            &context,
            port,
            aterm_secrets::ALAB_DEFAULT_URL,
            SETUP_WAIT,
            Replaces::Any,
        )
        .unwrap();
        let sys = fake();
        assert!(matches!(vault.probe(&sys, &context), Probe::Change { .. }));
        let refused = vault.apply(&sys, &context).unwrap_err();
        assert_eq!(
            refused,
            "this Mac's vault state changed after it was read (vault me (https://vault.example/v1/me), not the one System State names), so nothing was changed; the next try reads it"
        );
        assert!(ops.changes().is_empty(), "{:?}", ops.log());
    }

    #[test]
    fn a_join_replaces_only_the_vault_its_caller_read() {
        let source = aterm_secrets::ALAB_DEFAULT_URL;
        let ops = Scripted::new(vec![Ok(joined_elsewhere())]);
        let row = setup_pass_over(&ops, source, Replaces::Read(None), &mut None, false);
        assert_eq!(row.outcome, Outcome::Unknown, "{row:?}");
        assert_eq!(
            row.why.as_deref(),
            Some(
                "this Mac's vault changed after it was read (now vault me (https://vault.example/v1/me)), so nothing was changed; the next try reads it"
            )
        );
        assert!(ops.log().is_empty(), "{:?}", ops.log());
        assert_eq!(*ops.reached.lock().unwrap(), 0);

        let ops = Scripted::new(vec![
            Ok(joined_elsewhere()),
            Ok(joined_elsewhere()),
            Ok(configured(Membership::In { seq: 7, heard: 0 })),
        ]);
        ops.subscribed(Ok(installed_state()));
        let read = joined_elsewhere().config;
        let row = setup_pass_over(&ops, source, Replaces::Read(read), &mut None, false);
        assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
        assert_eq!(
            ops.changes(),
            [
                "keep /Users//alice/.config/aterm/secrets/before-join-1800000000".to_owned(),
                format!("join {source}")
            ]
        );
        assert_eq!(
            *ops.replacement_reads.lock().unwrap(),
            vec![Replaces::Read(joined_elsewhere().config); 2],
            "the snapshot and the actual join receive the same admitted configuration"
        );

        let ops = Scripted::new(vec![Ok(unconfigured())]);
        *ops.outcome.lock().unwrap() = Some(Err(aterm_secrets::Error::new(
            "injected conditional join refusal",
        )));
        let mut held = Some(password());
        let row = setup_pass_over(&ops, source, Replaces::Read(None), &mut held, false);
        assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
        assert!(held.is_some());
        assert_eq!(ops.changes(), [format!("join {source}")]);
        assert_eq!(
            *ops.replacement_reads.lock().unwrap(),
            [Replaces::Read(None)]
        );
    }

    fn caller_read_vault() -> (String, Status) {
        let source = "https://vault.example/v1/team";
        let mut status = configured(Membership::Pending);
        let config = status.config.as_mut().unwrap();
        config.source = Source::Url(source.into());
        config.name = "team".into();
        let pinned = format!(
            "{source}#{}",
            aterm_secrets::b64::encode_padded(&config.signer.unwrap())
        );
        (pinned, status)
    }

    fn assert_caller_read_vault_is_not_rebound(change: impl FnOnce(&mut Config)) {
        let (source, read) = caller_read_vault();
        let mut newer = read.clone();
        change(newer.config.as_mut().unwrap());
        let ops = Scripted::new(vec![Ok(newer)]);
        ops.subscribed(Ok(installed_state()));
        *ops.outcome.lock().unwrap() = Some(Ok(JoinOutcome::Pending {
            code: CODE.into(),
            host: "newer-name".into(),
        }));
        let mut held = Some(password());
        let row = setup_pass_over(&ops, &source, Replaces::Read(read.config), &mut held, false);
        assert!(
            ops.log().is_empty(),
            "a newer configuration must not authorize the older join: {:?}",
            ops.log()
        );
        assert_eq!(row.outcome, Outcome::Unknown, "{row:?}");
        assert!(ops.replacement_reads.lock().unwrap().is_empty());
        assert_eq!(*ops.reached.lock().unwrap(), 0);
        assert_eq!(ops.installs(), 0);
        assert_eq!(*ops.syncs.lock().unwrap(), 0);
        assert!(held.is_some(), "the refused join keeps its password");
    }

    #[test]
    fn a_new_host_does_not_replace_the_caller_read_vault_snapshot() {
        assert_caller_read_vault_is_not_rebound(|config| config.host = "newer-name".into());
    }

    #[test]
    fn a_new_signer_does_not_replace_the_caller_read_vault_snapshot() {
        assert_caller_read_vault_is_not_rebound(|config| config.signer = Some([8; 32]));
    }

    #[test]
    fn a_caller_read_vault_snapshot_change_is_refused_at_later_join_boundaries() {
        let changes: [fn(&mut Config); 2] = [
            |config| config.host = "newer-name".into(),
            |config| config.signer = Some([8; 32]),
        ];
        for change in changes {
            let (source, read) = caller_read_vault();
            let mut newer = read.clone();
            change(newer.config.as_mut().unwrap());
            let ops = Scripted::new(vec![Ok(read.clone())]);
            ops.subscribed(Ok(installed_state()));
            *ops.meanwhile.lock().unwrap() = Some((newer.clone(), installed_state()));
            let mut held = Some(password());
            let row = setup_pass_over(
                &ops,
                &source,
                Replaces::Read(read.config.clone()),
                &mut held,
                false,
            );
            assert_eq!(row.outcome, Outcome::Blocked, "{row:?}");
            assert!(matches!(row.stood_aside, Some(StoodAside::Lock(_))));
            assert_eq!(*ops.reached.lock().unwrap(), 1);
            assert!(ops.log().is_empty());
            assert!(ops.replacement_reads.lock().unwrap().is_empty());
            assert!(held.is_some());

            let context = ctx(&[user("alice", 501)]);
            let ops = Scripted::new(vec![Ok(read), Ok(newer)]);
            let vault =
                selected(&context, ops.clone(), &source, SETUP_WAIT, Replaces::Any).unwrap();
            vault.hold_password(Some(password()));
            assert!(matches!(
                vault.probe(&fake(), &context),
                Probe::Change { .. }
            ));
            assert!(vault.apply(&fake(), &context).is_err());
            assert!(ops.log().is_empty());
            assert!(ops.replacement_reads.lock().unwrap().is_empty());
            assert!(vault.unspent_password().is_some());
        }
    }

    #[test]
    fn the_caller_read_vault_snapshot_can_still_join_with_its_verified_pin() {
        let (source, read) = caller_read_vault();
        for rotated in [false, true] {
            let mut current = read.clone();
            if rotated {
                let config = current.config.as_mut().unwrap();
                config.followed = vec![config.signer.unwrap()];
                config.signer = Some([8; 32]);
            }
            let expected = current.config.clone();
            let ops = Scripted::new(vec![Ok(current)]);
            ops.subscribed(Ok(installed_state()));
            *ops.outcome.lock().unwrap() = Some(Ok(JoinOutcome::Pending {
                code: CODE.into(),
                host: "m22".into(),
            }));
            let mut held = Some(password());
            let row = setup_pass_over(
                &ops,
                &source,
                Replaces::Read(expected.clone()),
                &mut held,
                false,
            );
            assert_eq!(row.outcome, Outcome::Blocked, "{row:?}");
            assert!(matches!(row.stood_aside, Some(StoodAside::Waiting(_))));
            journaled_first(&ops.log());
            assert_eq!(ops.joins(), [(source.clone(), false)]);
            assert_eq!(
                *ops.replacement_reads.lock().unwrap(),
                vec![Replaces::Read(expected); 2]
            );
            assert!(held.is_none(), "the admitted request consumed its password");
        }

        let mut rotated = read.clone();
        let config = rotated.config.as_mut().unwrap();
        config.followed = vec![config.signer.unwrap()];
        config.signer = Some([8; 32]);
        let ops = Scripted::new(vec![Ok(rotated)]);
        ops.subscribed(Ok(installed_state()));
        let row = setup_pass_over(&ops, &source, Replaces::Read(read.config), &mut None, false);
        assert!(
            row.outcome == Outcome::Blocked
                && matches!(row.stood_aside, Some(StoodAside::Waiting(_))),
            "{row:?}"
        );
        assert!(ops.log().is_empty());
        assert_eq!(*ops.reached.lock().unwrap(), 0);
    }

    fn signed_file_vault(root: &Path, name: &str) -> String {
        let source = root.join(format!("{name}.vault")).display().to_string();
        let admin = Paths::for_home(&root.join(format!("{name}-admin")));
        let created = aterm_secrets::client::create(&admin, &source, None, Some("admin")).unwrap();
        format!("{source}#{}", created.signer)
    }

    fn pending_file_join(paths: &Paths, source: &str, host: &str) {
        let outcome = aterm_secrets::client::join(
            paths,
            &aterm_secrets::client::JoinOptions {
                source: source.into(),
                host: Some(host.into()),
                interactive: false,
                wait: Duration::ZERO,
                confirm: &|_| false,
                password: aterm_secrets::client::JoinPassword::Given(None),
                fetched: None,
                requested: &|| {},
                say: &|_| {},
            },
        )
        .unwrap();
        assert!(matches!(outcome, JoinOutcome::Pending { .. }));
    }

    fn live_file_join(live: &LiveSecrets, source: &str, replaces: &Replaces) -> JoinOutcome {
        let paths = live.paths();
        let held = password();
        live.with_join_options(
            &fake(),
            (source, replaces),
            Some(&held),
            false,
            &|| {},
            |options| {
                // Supply the platform default at the client boundary, so the real
                // signed-file join never asks scutil or reads this Mac's name.
                let options = aterm_secrets::client::JoinOptions {
                    source: options.source.clone(),
                    host: Some(
                        options
                            .host
                            .clone()
                            .unwrap_or_else(|| "inert-default".into()),
                    ),
                    interactive: options.interactive,
                    wait: options.wait,
                    confirm: options.confirm,
                    password: options.password.clone(),
                    fetched: options.fetched,
                    requested: options.requested,
                    say: options.say,
                };
                match replaces {
                    Replaces::Any => aterm_secrets::client::join(&paths, &options),
                    Replaces::Read(expected) => aterm_secrets::client::join_if_unchanged(
                        &paths,
                        &options,
                        expected.as_ref(),
                    ),
                }
            },
        )
        .unwrap()
    }

    #[test]
    fn a_join_approved_before_the_wait_begins_ends_it_at_once() {
        let root = aterm_tempfile::tempdir().unwrap();
        let source = signed_file_vault(root.path(), "team");
        let admin = Paths::for_home(&root.path().join("team-admin"));
        let reader = Paths::for_home(&root.path().join("reader"));
        pending_file_join(&reader, &source, "reader");
        let (recipient, _) = aterm_secrets::client::status(&reader)
            .unwrap()
            .machine
            .unwrap();
        let opened = aterm_secrets::client::admin_open(&admin).unwrap();
        let mut doc = opened.doc.clone();
        doc.add_machine("reader", &recipient.to_bech32()).unwrap();
        aterm_secrets::client::admin_publish(&admin, &opened, &doc).unwrap();
        // The subscriber the join started applies the approval first.
        aterm_secrets::client::sync(&reader, None).unwrap();
        let said = Mutex::new(Vec::new());
        let outcome = wait_for_approval(&reader, Duration::from_secs(60), true, None, &|line| {
            said.lock().unwrap().push(line.to_owned());
        });
        assert!(
            matches!(outcome, Some(JoinOutcome::Joined { .. })),
            "{outcome:?}"
        );
        assert!(said.lock().unwrap().is_empty(), "{said:?}");
    }

    /// `--vault`'s read against a live loopback vault: an unpinned vault hands back its
    /// fingerprint and a pin the next read accepts, a pin the vault does not answer to and a
    /// vault the server does not hold are refused there, before anything changes.
    #[test]
    fn a_named_vault_is_read_once_and_a_pin_it_does_not_answer_to_is_refused() {
        let root = aterm_tempfile::tempdir().unwrap();
        let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let running = aterm_secrets::server::start(
            socket,
            &aterm_secrets::server::Options {
                listen: "127.0.0.1:0".parse().unwrap(),
                store: root.path().join("store"),
                advertise: None,
                pins: Vec::new(),
                hold: Duration::from_millis(200),
                keep_awake: false,
            },
        )
        .unwrap();
        let admin = Paths::for_home(&root.path().join("admin"));
        let url = format!("http://{}/v1/team", running.addr);
        let created = aterm_secrets::client::create(&admin, &url, None, Some("admin")).unwrap();
        let live = LiveSecrets::new(&root.path().join("joiner"));

        let Ok(Checked::Unpinned { fingerprint, pin }) = live.check(&url) else {
            panic!("an unpinned vault is put to the person");
        };
        assert_eq!(fingerprint, created.fingerprint);
        assert_eq!(live.check(&format!("{url}#{pin}")), Ok(Checked::Pinned));

        let other = aterm_secrets::b64::encode_padded(&[7u8; 32]);
        let refused = live.check(&format!("{url}#{other}"));
        assert!(refused.is_err(), "{refused:?}");

        let missing = live.check(&format!("http://{}/v1/nobody", running.addr));
        assert!(missing.is_err(), "{missing:?}");
        running.stop();
        assert!(
            !root.path().join("joiner").exists(),
            "a read changes nothing"
        );
    }

    #[test]
    fn a_url_join_says_nothing_itself_and_its_approval_lands_within_the_wait() {
        let root = aterm_tempfile::tempdir().unwrap();
        let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let running = aterm_secrets::server::start(
            socket,
            &aterm_secrets::server::Options {
                listen: "127.0.0.1:0".parse().unwrap(),
                store: root.path().join("store"),
                advertise: None,
                pins: Vec::new(),
                hold: Duration::from_millis(200),
                keep_awake: false,
            },
        )
        .unwrap();
        let admin = Paths::for_home(&root.path().join("admin"));
        let url = format!("http://{}/v1/team", running.addr);
        let created = aterm_secrets::client::create(&admin, &url, None, Some("admin")).unwrap();
        let source = format!("{url}#{}", created.signer);
        let held = aterm_secrets::client::admin_open(&admin)
            .unwrap()
            .doc
            .join_password()
            .unwrap();
        let heard = Arc::new(Mutex::new(Vec::new()));
        let live = LiveSecrets {
            heard: Some(heard.clone()),
            ..LiveSecrets::new(&root.path().join("joiner"))
        };
        let outcome = live
            .with_join_options(
                &fake(),
                (&source, &Replaces::Any),
                Some(&SecretBytes::from(held.as_slice())),
                true,
                &|| {},
                |options| {
                    aterm_secrets::client::join(
                        &live.paths(),
                        &aterm_secrets::client::JoinOptions {
                            host: Some("joiner".into()),
                            source: options.source.clone(),
                            password: options.password.clone(),
                            ..*options
                        },
                    )
                },
            )
            .unwrap();
        assert!(
            matches!(outcome, JoinOutcome::Pending { .. }),
            "{outcome:?}"
        );
        assert!(heard.lock().unwrap().is_empty(), "{heard:?}");
        assert!(
            !live.await_approval(Duration::from_secs(60), &|| true),
            "a stopped wait ends at once"
        );
        let stop = Arc::new(AtomicBool::new(false));
        let approver = {
            let stop = stop.clone();
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let _ = aterm_secrets::client::auto_approve(&admin);
                    std::thread::sleep(Duration::from_millis(200));
                }
            })
        };
        // A hang detector, not a budget: the approval lands within the approver's next pass.
        let inside = live.await_approval(Duration::from_secs(60), &|| false);
        stop.store(true, Ordering::SeqCst);
        approver.join().unwrap();
        running.stop();
        assert!(inside);
        assert!(heard.lock().unwrap().is_empty(), "{heard:?}");
    }

    /// A join password an admin Mac's approver refuses ends the join window at once, said as
    /// [`REFUSED`], so the retype prompt follows the verdict instead of the window's end.
    #[test]
    fn a_refused_join_password_ends_the_join_window_at_the_verdict() {
        let root = aterm_tempfile::tempdir().unwrap();
        let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let running = aterm_secrets::server::start(
            socket,
            &aterm_secrets::server::Options {
                listen: "127.0.0.1:0".parse().unwrap(),
                store: root.path().join("store"),
                advertise: None,
                pins: Vec::new(),
                hold: Duration::from_millis(200),
                keep_awake: false,
            },
        )
        .unwrap();
        let admin = Paths::for_home(&root.path().join("admin"));
        let url = format!("http://{}/v1/team", running.addr);
        let created = aterm_secrets::client::create(&admin, &url, None, Some("admin")).unwrap();
        let source = format!("{url}#{}", created.signer);
        let heard = Arc::new(Mutex::new(Vec::new()));
        let live = LiveSecrets {
            heard: Some(heard.clone()),
            ..LiveSecrets::new(&root.path().join("joiner"))
        };
        let outcome = live
            .with_join_options(
                &fake(),
                (&source, &Replaces::Any),
                Some(&SecretBytes::from("a-mistyped-join-password".as_bytes())),
                true,
                &|| {},
                |options| {
                    aterm_secrets::client::join(
                        &live.paths(),
                        &aterm_secrets::client::JoinOptions {
                            host: Some("joiner".into()),
                            source: options.source.clone(),
                            password: options.password.clone(),
                            ..*options
                        },
                    )
                },
            )
            .unwrap();
        assert!(
            matches!(outcome, JoinOutcome::Pending { .. }),
            "{outcome:?}"
        );
        let refused = aterm_secrets::client::auto_approve(&admin).unwrap();
        assert_eq!(refused.refused.len(), 1, "{refused:?}");
        // A window far past the hang detector: only the verdict ends it within it.
        let (ended, waited) = std::sync::mpsc::channel();
        let joiner = root.path().join("joiner");
        let heard_by = heard.clone();
        std::thread::spawn(move || {
            let live = LiveSecrets {
                heard: Some(heard_by),
                ..LiveSecrets::new(&joiner)
            };
            let _ = ended.send(live.await_approval(Duration::from_secs(600), &|| false));
        });
        let inside = waited
            .recv_timeout(Duration::from_secs(60))
            .expect("CONTROL: a window that reads no verdict runs its 600 s");
        running.stop();
        assert!(!inside);
        assert_eq!(*heard.lock().unwrap(), [REFUSED.to_string()]);
    }

    #[test]
    fn live_join_keeps_the_name_of_the_pending_selected_vault() {
        let root = aterm_tempfile::tempdir().unwrap();
        let source = signed_file_vault(root.path(), "team");
        let live = LiveSecrets::unattended(&root.path().join("reader"));
        pending_file_join(&live.paths(), &source, "custom-name");
        let expected = aterm_secrets::client::config(&live.paths())
            .unwrap()
            .unwrap();
        assert_eq!(expected.host, "custom-name");
        assert!(matches!(
            aterm_secrets::client::status(&live.paths())
                .unwrap()
                .membership,
            Membership::Pending
        ));
        let outcome = live_file_join(&live, &source, &Replaces::Read(Some(expected.clone())));
        let saved = aterm_secrets::client::config(&live.paths())
            .unwrap()
            .unwrap();
        assert_eq!(
            saved, expected,
            "a password retry keeps the configured name"
        );
        assert!(matches!(
            outcome,
            JoinOutcome::Pending { host, .. } if host == "custom-name"
        ));
    }

    #[test]
    fn live_join_uses_the_default_name_for_a_fresh_or_different_vault() {
        let root = aterm_tempfile::tempdir().unwrap();
        let source = signed_file_vault(root.path(), "team");
        let other = signed_file_vault(root.path(), "other");
        let live = LiveSecrets::unattended(&root.path().join("reader"));
        let outcome = live_file_join(&live, &source, &Replaces::Read(None));
        assert!(matches!(
            outcome,
            JoinOutcome::Pending { host, .. } if host == "inert-default"
        ));
        assert_eq!(
            aterm_secrets::client::config(&live.paths())
                .unwrap()
                .unwrap()
                .host,
            "inert-default"
        );
        pending_file_join(&live.paths(), &source, "custom-name");
        let before = aterm_secrets::client::config(&live.paths()).unwrap();
        let outcome = live_file_join(&live, &other, &Replaces::Read(before));
        assert!(matches!(
            outcome,
            JoinOutcome::Pending { host, .. } if host == "inert-default"
        ));
        let after = aterm_secrets::client::config(&live.paths())
            .unwrap()
            .unwrap();
        assert_eq!(after.host, "inert-default");
        assert_eq!(after.name, "other");
    }

    #[test]
    fn a_source_that_is_no_vault_source_is_not_called_offline() {
        let sys = fake();
        sys.answer(None);
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        *ops.unreachable.lock().unwrap() =
            Some(Look::Unreached("vault.example: no such host".into()));
        *ops.outcome.lock().unwrap() = Some(Err(aterm_secrets::Error::new(
            "that URL is not usable: it needs a host, and no user:password@, spaces or control characters",
        )));
        let rows = run_with(
            &sys,
            &ops,
            Mode::Apply,
            Some("https://who:typed@vault.example/v1/team"),
        );
        assert_eq!(rows[0].outcome, Outcome::Failed, "{rows:?}");
        assert!(rows[0].stood_aside.is_none(), "{rows:?}");
        assert_eq!(*ops.reached.lock().unwrap(), 0);
    }

    #[test]
    fn an_offline_join_at_the_terminal_stands_aside_before_anything_is_made() {
        let sys = fake();
        sys.answer(None);
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        ops.alab_answers();
        *ops.unreachable.lock().unwrap() =
            Some(Look::Unreached("alab-secrets.local: no such host".into()));
        let rows = run(&sys, &ops, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
        assert_eq!(
            rows[0].stood_aside.as_ref().map(StoodAside::exit_status),
            Some(69)
        );
        assert!(ops.joins().is_empty());
        assert_eq!(ops.installs(), 0);
    }

    /// A join waiting for an admin on a Mac with Sync off says so and is no step and nothing to
    /// wait on (an approval lands only once sync is on), and setup installs nothing over the
    /// record. With no record (the control) the subscriber is installed.
    #[test]
    fn a_pending_join_with_sync_off_says_so_and_installs_nothing() {
        let source = aterm_secrets::ALAB_DEFAULT_URL;
        for off in [false, true] {
            let ops = Scripted::new(vec![Ok(configured(Membership::Pending))]);
            ops.subscribed(Ok(State::Absent));
            *ops.turned_off.lock().unwrap() = off;
            let row = setup_pass(&ops, source, &mut None, false);
            if off {
                assert_eq!(row.outcome, Outcome::Ok, "{row:?}");
                assert!(
                    row.to.as_deref().is_some_and(|to| to.contains(SYNC_OFF)
                        && to.ends_with("an approval lands once it is on")),
                    "{row:?}"
                );
                assert!(ops.changes().is_empty(), "{:?}", ops.changes());
            } else {
                assert!(
                    ops.changes().iter().any(|change| change == "install"),
                    "{:?} {row:?}",
                    ops.changes()
                );
            }
        }
    }

    /// `aterm init` and System State keep Sync off for a member or plain source even with its
    /// files unwritten: no path installs the subscriber, repairs the files or clears Off.
    /// Without Off (the control) both install and repair as needed.
    #[test]
    fn aterm_init_keeps_the_persons_sync_off() {
        for membership in [Membership::In { seq: 7, heard: 0 }, Membership::Plain] {
            for selected in [false, true] {
                for off in [true, false] {
                    for unwritten in [true, false] {
                        let ops = Scripted::new(
                            (0..4).map(|_| Ok(configured(membership.clone()))).collect(),
                        );
                        ops.subscribed(Ok(State::Absent));
                        *ops.turned_off.lock().unwrap() = off;
                        *ops.unwritten.lock().unwrap() = unwritten;
                        let row = if selected {
                            setup_pass(&ops, aterm_secrets::ALAB_DEFAULT_URL, &mut None, false)
                        } else {
                            run(&fake(), &ops, Mode::Apply).remove(0)
                        };
                        let case = format!(
                            "{membership:?} selected={selected} off={off} unwritten={unwritten}"
                        );
                        assert_eq!(
                            ops.installs() > 0,
                            !off,
                            "{case}: {:?} {row:?}",
                            ops.changes()
                        );
                        assert_eq!(
                            *ops.syncs.lock().unwrap(),
                            u32::from(!off && unwritten),
                            "{case}"
                        );
                        assert_eq!(ops.turned_off(), off, "{case}");
                        assert_eq!(*ops.unwritten.lock().unwrap(), off && unwritten, "{case}");
                        if off {
                            assert_eq!(row.outcome, Outcome::Ok, "{case}: {row:?}");
                            let to = row.to.as_deref().expect("Sync off value");
                            assert!(to.ends_with(SYNC_OFF), "{case}: {row:?}");
                            assert!(
                                !to.contains(" in ~/.secrets") && !to.contains("kept in sync"),
                                "{case}: no materialization claim: {row:?}"
                            );
                            assert!(ops.changes().is_empty(), "{case}: {:?}", ops.changes());
                        }
                    }
                }
            }
        }
    }
}
