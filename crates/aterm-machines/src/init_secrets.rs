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
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// The item family.
pub const FAMILY: &str = "secrets";

/// The prompt that asks for the vault again when the one `--vault` names holds none.
pub const PROMPT: &str = "Secrets vault URL or path (none skips): ";

const SKIPPED: &str = "skipped at the prompt; aterm secrets join <url> joins a vault later";

use aterm_secrets::alab_join::JoinPlace;
/// Shared progress for automatic ALab LAN admission, as `aterm members join` reports it.
/// The join queues at the vault and asks reachable members ([`SecretsOps::ask_members`]); the
/// subscriber keeps retrying while it waits (`aterm_secrets::client::on_ask_members`).
pub use aterm_secrets::members::join::LET_IN;

/// A native possession exchange is still ambiguous; this alone proves no LAN grant.
pub const NATIVE_ADMISSION_PENDING: &str = "Joining ALab is not confirmed yet";

/// Progress while this Mac's automatic ALab join is pending.
pub fn quiet_first() -> String {
    "joining ALab automatically".to_owned()
}

/// What a Mac whose ask no member heard does before [`LET_IN`]: ask them all.
const ASK_FIRST: &str = "looking for ALab members on this LAN";

/// What a Mac whose ask a member heard says anywhere but the ALab LAN, where nothing may join it
/// automatically ([`quiet_first`] is the ALab LAN's).
const ASKED: &str = "the ALab members it reached heard it ask";

/// What a Mac a member let in waits for: its vault to record it, which needs nobody.
const ALLOWED: &str =
    "a member let this Mac in, and the subscriber finishes the join once the vault records it";

/// An ask of every ALab member this Mac hears, for the user whose home it is given: how many
/// heard it.
pub type AskMembers = fn(&Path, &(dyn Fn() -> Result<(), String> + Sync)) -> Result<usize, String>;

/// What asks every ALab member this Mac hears for an Allow ([`on_ask_members`]); unset, the
/// Secrets stage asks nobody, as in a test.
static ASK_MEMBERS: std::sync::OnceLock<AskMembers> = std::sync::OnceLock::new();

/// Ask ALab's members through `ask` (`aterm members join --no-wait`'s ask) once a join waits on
/// the ALab vault. The first call wins.
pub fn on_ask_members(ask: AskMembers) {
    let _ = ASK_MEMBERS.set(ask);
}

/// Only the ALab vault says a Mac is ALab's (init design §8.1): `status` holds a membership of a
/// vault pinned to the ALab key ([`aterm_secrets::client::alab_vault`]), opened under this Mac's
/// own recipient key. An admin signed that locker after this Mac existed, so no replayed envelope
/// of another Mac produces it. A pending request to the ALab vault starts a join and nothing
/// more; anyone can run a vault of their own, read a plain file, hold a fleet key, or ask to join
/// some vault, and none of that is a reason to set up a Mac as ALab's or sign it out of Messages
/// and FaceTime. System State's window and `aterm init` read this one rule.
#[must_use]
pub fn member_of_the_alab_vault(status: &Status) -> bool {
    let Some(config) = status.config.as_ref() else {
        return false;
    };
    match status.membership {
        Membership::In { .. } => aterm_secrets::client::alab_vault(config),
        Membership::Pending
        | Membership::Plain
        | Membership::Unconfigured
        | Membership::Revoked { .. } => false,
    }
}

/// [`member_of_the_alab_vault`] of this login's vault state, read now from its records alone
/// (no network): where no window caches it, `aterm init` on Linux.
#[must_use]
pub fn alab_member_here() -> bool {
    Paths::live()
        .ok()
        .and_then(|paths| aterm_secrets::client::status(&paths).ok())
        .is_some_and(|status| member_of_the_alab_vault(&status))
}

/// Who `aterm init`'s secrets step runs for ([`items_at`]). Nothing it does asks for the
/// vault's join password: the open ALab vault is joined with no question, and a person who has
/// it uses `aterm secrets join --password-stdin` or System State's field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Here {
    /// A person at a terminal (this Mac's console, or a Linux login): `--vault` may ask its
    /// questions, and the row says how an admin lets this Mac in.
    Person,
    /// Nobody to ask, in the console login's own session (an agent's tab, no terminal).
    Session,
    /// Nobody to ask, in a remote shell (SSH) of the login that owns the console.
    Remote,
    /// A person at a terminal, as [`Here::Person`], in a run nobody asked for: aterm's own,
    /// finishing setup after their package install or update ([`crate::init::Decider::Aterm`]).
    /// The row says how an admin lets this Mac in, but the run asks nothing: no `--vault`
    /// question and no signer for its join to confirm.
    Unbidden,
}

/// What an unconfigured Mac's row says where no ALab vault answers: nothing is asked.
pub const NOT_HERE: &str = "no secrets vault on this network; aterm secrets join <url> or aterm init --vault <url> joins yours";

/// How the vault item's stand-aside begins when the vault's address answered with no vault there
/// ([`Look::NoVault`]), so a caller tells that answer from one nobody gave.
pub const NO_VAULT_NOW: &str = "no vault is there now, so nothing was changed";

/// How a join's failure after the vault answered it starts (the subscriber not installed or not
/// staying up): the vault was reached.
pub const VAULT_ANSWERED_BUT: &str = "the vault answered, but";

/// What a wait to be let in ([`wait_for_approval`]) says when an admin Mac's verdict
/// refused the join password the join sent (System State's field's, or `aterm secrets join`'s).
const REFUSED: &str = "an admin Mac refused that join password: it is not the vault's";

const STOPPED_AT_TRUST: &str =
    "stopped at the question whether to trust the vault's signing key; nothing was changed";

/// How long the look for the ALab vault takes at most, all of it.
pub const PROBE_WAIT: Duration = aterm_secrets::client::PROBE_DEADLINE;

/// How long the read of the vault `--vault` names waits for an answer (plus the transport's
/// margin).
pub const CHECK_WAIT: Duration = Duration::from_secs(3);

/// How long the look before a join or a sync waits for the vault ([`SecretsOps::reachable`]).
pub const REACH_WAIT: Duration = aterm_secrets::client::REACH_WAIT;

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
    /// the configuration `source.1` admits, under the client's write lock. `current` must
    /// remain valid at each local publication, request and confirmation effect.
    /// # Errors
    /// The join failed; the error names no value.
    #[expect(
        clippy::too_many_arguments,
        reason = "the join options and the authority checked at each effect"
    )]
    fn join(
        &self,
        sys: &dyn System,
        source: (&str, &Replaces),
        password: Option<&SecretBytes>,
        interactive: bool,
        wait: Duration,
        requested: &dyn Fn() -> bool,
        current: &(dyn Fn() -> Result<(), String> + Sync),
    ) -> aterm_secrets::Result<JoinOutcome>;

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
    fn sync(&self, current: &(dyn Fn() -> Result<(), String> + Sync)) -> aterm_secrets::Result<()>;

    /// Say one progress line (never a value) to the person: `aterm init`'s terminal.
    fn say(&self, line: &str);

    /// Whether `[swarm] enabled` in `aterm.toml` leaves swarm participation on.
    /// # Errors
    /// `aterm.toml` cannot be read or has an invalid `[swarm]` section.
    fn swarm_enabled(&self) -> aterm_secrets::Result<bool> {
        Ok(true)
    }

    /// Ask every ALab member this Mac hears for an Allow, as `aterm members join --no-wait`
    /// does: how many heard it. The live port asks through what the binary registered
    /// ([`on_ask_members`]); a port with none asks nobody.
    /// # Errors
    /// No member heard it, or nothing here asks; the error names no value.
    fn ask_members(
        &self,
        current: &(dyn Fn() -> Result<(), String> + Sync),
    ) -> Result<usize, String> {
        current()?;
        Err("this port asks no member".into())
    }

    /// Retain an unconfigured default join for the subscriber, without claiming membership.
    /// This runs only at the authorized apply boundary.
    /// # Errors
    /// The identity is isolated, sharing is off, or private state cannot be retained.
    fn prepare_bootstrap(&self) -> aterm_secrets::Result<()> {
        Ok(())
    }

    /// Try a native member proof before vault discovery. The callback rechecks `current`
    /// and the exact source under its publication lock, and admits only the compiled vault.
    /// # Errors
    /// Authorization changed, or the proof could not run safely.
    fn native_bootstrap(
        &self,
        _current: &(dyn Fn() -> aterm_secrets::Result<()> + Sync),
    ) -> aterm_secrets::Result<aterm_secrets::client::NativeAdmission> {
        Ok(aterm_secrets::client::NativeAdmission::None)
    }

    /// Whether a member let this Mac in and its vault has yet to record it
    /// (`aterm_secrets::client::holds_allowed`).
    fn allowed(&self) -> bool {
        false
    }

    /// What this Mac's row says where its vault let it go (`Membership::Revoked` at `seq`) with
    /// no act against it that it can read (`aterm_secrets::client::dropped_words`): a rotation
    /// left its recipient out, every desk that read that rotation refuses its key, and a join
    /// keeps that key, so it comes back only as a new Mac, its vault folder set aside (one-mesh
    /// E2). `None` for a Mac an act took out, whose join an admin's approve or a person's Allow
    /// again lets in, and for a port with no vault folder.
    fn dropped(&self, seq: u64) -> Option<String> {
        let _ = seq;
        None
    }

    /// Why this Mac's LAN cannot be reached, in the broker's own words: on macOS no hello is
    /// answered, neither by the root LAN broker, through which aterm's LAN acts go, nor while none
    /// is installed by the allowed direct path ([`crate::lan_reach::broker_unready`]); `None`
    /// where one answers, and elsewhere. A port that reaches no LAN of its own (a test's) answers
    /// `None`.
    fn broker_unready(&self) -> Option<String> {
        None
    }

    /// Where this Mac is for its ALab join's words (`aterm_secrets::alab_join::JoinPlace`): the
    /// live port reads its network place ([`JoinPlace::here`]); a port with none cannot tell.
    fn join_place(&self) -> JoinPlace {
        JoinPlace::Unknown
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

/// Whether a person can be asked: this process has a terminal.
fn at_terminal() -> bool {
    use std::io::IsTerminal as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/dev/tty")
        .is_ok_and(|tty| tty.is_terminal())
}

/// One look for the ALab vault from the login whose home is `home`. A look that heard another
/// vault answer on this network within [`aterm_secrets::heard::ABSENT_FOR`] stands for it, so
/// no request is made. Otherwise the vault is asked ([`PROBE_WAIT`]) and a definitive answer
/// (the vault, or another vault's signed envelope) is kept for the next run on this network;
/// anything else (a timeout, a refused connect, a server holding no vault yet, an answer that
/// is no vault, no LAN broker to reach the LAN through) is not. A look that heard the vault spares no
/// later look: it answers within a fraction of a second where it is.
fn look_here(home: &Path) -> aterm_secrets::Result<ProbeOutcome> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    look_here_with(home, aterm_secrets::heard::network_key(), now, || {
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
    if kept_elsewhere(home, network.as_deref(), now) {
        return Ok(ProbeOutcome::Absent);
    }
    let probed = probe();
    keep_look(home, network, now, &probed);
    probed
}

/// Whether a look kept under `home` ([`aterm_secrets::heard`]) heard another vault answer at the
/// ALab vault's address on `network` within [`aterm_secrets::heard::ABSENT_FOR`] of `now`: the
/// one kept answer that stands for a look, for `aterm init`'s and System State's alike.
#[must_use]
pub fn kept_elsewhere(home: &Path, network: Option<&str>, now: u64) -> bool {
    use aterm_secrets::heard::{self, Heard};
    network.is_some_and(|network| {
        matches!(
            heard::read(&Paths::for_home(home))
                .as_ref()
                .and_then(|kept| kept.standing(network, now)),
            Some(Heard::Absent)
        )
    })
}

/// Keep what a look for the ALab vault on `network` at `now` heard (`probed`) under `home`, for
/// the next look on that network ([`kept_elsewhere`]).
///
/// Only a definitive answer is kept: the vault, or another vault's signed envelope at its
/// address. A 404 (a server whose store holds no vault yet, moving or new), an answer that is no
/// vault (an error page or redirect from a server mid-restart), a refused or timed-out connect
/// (its Mac asleep) and a LAN the broker could not reach prove nothing about this network, and a record of them would skip the vault here for a day.
pub fn keep_look(
    home: &Path,
    network: Option<String>,
    now: u64,
    probed: &aterm_secrets::Result<ProbeOutcome>,
) {
    use aterm_secrets::heard::{self, Heard, Verdict};
    let heard = match probed {
        // The vault answered: keep its address and signer as a hint (never a pin) for the Peers
        // row and the join. `probe_default` asks the ALab vault's own address.
        Ok(ProbeOutcome::Found(found)) => Some(Heard::Found {
            url: found.url.clone(),
            signer: found.fingerprint.clone(),
        }),
        Ok(ProbeOutcome::AnotherVault(_)) => Some(Heard::Absent),
        Ok(ProbeOutcome::Absent | ProbeOutcome::NotThisVault(_)) | Err(_) => None,
    };
    if let (Some(heard), Some(network)) = (heard, network) {
        // The record only spares a later look: one not written costs that look, nothing else.
        let _ = heard::write(
            &Paths::for_home(home),
            &Verdict {
                at: now,
                heard,
                network,
            },
        );
    }
}

/// The live vault of one user.
#[derive(Debug, Clone)]
pub struct LiveSecrets {
    home: PathBuf,
    /// Nothing is said ([`SecretsOps::say`]): System State's unattended pass.
    quiet: bool,
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
            asked: Arc::default(),
            #[cfg(test)]
            heard: None,
        }
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
    source: &str,
    wait: Duration,
    subscribed: bool,
    mistyped: Option<&str>,
    say: &dyn Fn(&str),
    current: &(dyn Fn() -> aterm_secrets::Result<()> + Sync),
) -> aterm_secrets::Result<Option<JoinOutcome>> {
    if wait.is_zero() {
        return Ok(None);
    }
    let withdrawn = Mutex::new(None::<aterm_secrets::Error>);
    let admitted = || {
        let mut withdrawn = withdrawn.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(error) = &*withdrawn {
            return Err(error.clone());
        }
        let result = (|| {
            current()?;
            let configured = aterm_secrets::client::config(paths)?;
            if selected_vault(source, &paths.home, configured.as_ref())
                .map_err(aterm_secrets::Error::new)?
                .is_none()
            {
                return Err(aterm_secrets::Error::new(
                    "the selected vault changed during confirmation",
                ));
            }
            Ok(())
        })();
        // A refused confirmation stays refused even if a subsequent observation is allowed.
        // Transport failures may retry; an old setup admission may never resume as a new one.
        if let Err(error) = &result {
            *withdrawn = Some(error.clone());
        }
        result
    };
    admitted()?;
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
        return Ok(Some(joined));
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
    let deadline = crate::port::deadline_after(wait);
    while Instant::now() < deadline {
        admitted()?;
        let started = Instant::now();
        let left = deadline
            .saturating_duration_since(started)
            .min(Duration::from_secs(1));
        if let Err(error) = aterm_secrets::client::sync_within_admitted(paths, left, &admitted) {
            admitted()?;
            say(&format!("still waiting ({error})"));
        }
        admitted()?;
        if let Some(joined) = joined() {
            return Ok(Some(joined));
        }
        let left = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_secs(1));
        if !left.is_zero()
            && aterm_secrets::client::read_verdict_within_admitted(paths, left, &admitted)
                .unwrap_or(false)
        {
            say(REFUSED);
            return Ok(None);
        }
        admitted()?;
        let pause = Duration::from_secs(1).saturating_sub(started.elapsed());
        std::thread::sleep(pause.min(deadline.saturating_duration_since(Instant::now())));
    }
    Ok(None)
}

impl SecretsOps for LiveSecrets {
    fn status(&self) -> aterm_secrets::Result<Status> {
        aterm_secrets::client::status(&self.paths())
    }

    fn configured(&self) -> aterm_secrets::Result<Option<Config>> {
        aterm_secrets::client::config(&self.paths())
    }

    fn prepare_bootstrap(&self) -> aterm_secrets::Result<()> {
        let paths = self.paths();
        aterm_secrets::client::refuse_lab_identity(&paths)?;
        aterm_secrets::client::prepare_alab_join_retry(&paths)
    }

    fn native_bootstrap(
        &self,
        current: &(dyn Fn() -> aterm_secrets::Result<()> + Sync),
    ) -> aterm_secrets::Result<aterm_secrets::client::NativeAdmission> {
        aterm_secrets::client::native_bootstrap(&self.paths(), current)
    }

    fn subscriber(&self) -> aterm_secrets::Result<State> {
        aterm_secrets::service::state(&self.home, aterm_secrets::service::Job::Subscriber)
    }

    fn terminal(&self) -> bool {
        at_terminal()
    }

    fn probe_alab(&self) -> aterm_secrets::Result<ProbeOutcome> {
        look_here(&self.home)
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
        let alab = matches!(&parsed, Source::Url(url) if aterm_secrets::is_alab_default_url(url));
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
        aterm_secrets::client::look(source, REACH_WAIT, Duration::ZERO, &|_| {})
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
        current: &(dyn Fn() -> Result<(), String> + Sync),
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
        let admitted = || current().map_err(aterm_secrets::Error::new);
        let outcome = self.with_join_options(
            sys,
            (source, replaces),
            password,
            interactive,
            &queued,
            |options| match replaces {
                Replaces::Any => aterm_secrets::client::join_admitted(&paths, options, &admitted),
                Replaces::Read(expected) => aterm_secrets::client::join_if_unchanged_admitted(
                    &paths,
                    options,
                    expected.as_ref(),
                    &admitted,
                ),
            },
        )?;
        match outcome {
            JoinOutcome::Pending { code, host } => Ok(wait_for_approval(
                &paths,
                source,
                wait,
                subscribed.get(),
                mistyped.as_deref(),
                &say,
                &admitted,
            )?
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
        aterm_secrets::client::materialization_ready_for_setup(&self.paths())
    }

    fn ask_members(
        &self,
        current: &(dyn Fn() -> Result<(), String> + Sync),
    ) -> Result<usize, String> {
        let ask = ASK_MEMBERS.get().ok_or("this build asks no member")?;
        ask(&self.home, current)
    }

    fn allowed(&self) -> bool {
        aterm_secrets::client::holds_allowed(&self.paths())
    }

    fn dropped(&self, seq: u64) -> Option<String> {
        aterm_secrets::client::dropped_words(&self.paths(), seq)
    }

    fn broker_unready(&self) -> Option<String> {
        crate::lan_reach::broker_unready()
    }

    fn join_place(&self) -> JoinPlace {
        JoinPlace::here()
    }

    fn sync(&self, current: &(dyn Fn() -> Result<(), String> + Sync)) -> aterm_secrets::Result<()> {
        aterm_secrets::client::sync_admitted(&self.paths(), &|| {
            current().map_err(aterm_secrets::Error::new)
        })
        .map(drop)
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

    fn swarm_enabled(&self) -> aterm_secrets::Result<bool> {
        aterm_secrets::swarm::enabled_in_home(&self.home)
    }
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

/// The live port for the one user `aterm init` configures.
/// # Errors
/// The run does not name exactly one user.
pub fn live(ctx: &Ctx) -> Result<Arc<dyn SecretsOps>, String> {
    let user = only_user(ctx)?;
    Ok(Arc::new(LiveSecrets::new(Path::new(&user.home))))
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
    let default_alab = pin.is_none()
        && matches!(&source, Source::Url(url) if aterm_secrets::is_alab_default_url(url));
    Ok(configured.filter(|config| {
        config.name == name
            && if config.source == source {
                aterm_secrets::client::pin_admitted(config, pin)
            } else {
                default_alab
                    && matches!(config.source, Source::Url(_))
                    && aterm_secrets::client::alab_vault(config)
            }
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
    /// not be there yet. Not where the vault's reach waits for the LAN broker and its subscriber
    /// is down, by the one reading the vault's row judges by too: nothing lands
    /// the join on that run, so GitHub reads its own row.
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

/// Why the fallback probe found no ALab vault, after native member admission was attempted.
fn alab_unavailable(ops: &dyn SecretsOps) -> Option<&'static str> {
    match ops.probe_alab() {
        Ok(ProbeOutcome::Found(_)) => None,
        Ok(ProbeOutcome::Absent | ProbeOutcome::NotThisVault(_)) => Some(NO_VAULT_NOW),
        Ok(ProbeOutcome::AnotherVault(_)) | Err(_) => Some(NOT_HERE),
    }
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

/// The secrets step of a run for a person at a terminal ([`items_at`], [`Here::Person`]).
/// # Errors
/// As [`items_at`].
pub fn items(
    sys: &dyn System,
    ctx: &Ctx,
    ops: Arc<dyn SecretsOps>,
    mode: Mode,
    vault: Option<&str>,
) -> Result<Secrets, String> {
    items_at(sys, ctx, ops, mode, vault, Here::Person)
}

/// What the row says first where a run neither looked for the ALab vault nor reached a vault on
/// the LAN, because the LAN broker, through which aterm reaches this Mac's network on macOS,
/// does not answer and no allowed direct path serves in its place
/// ([`SecretsOps::broker_unready`]): there is no other way to the LAN without an alert.
const LAN_BROKER_WAIT: &str = "not looked for or reached yet, for the LAN broker: ";

/// The row's words where the LAN broker does not answer, `why` its own ([`LAN_BROKER_WAIT`]).
fn lan_broker_absent(why: &str) -> String {
    format!("{LAN_BROKER_WAIT}{why}; the next pass looks once it answers")
}

/// Whether `config`'s vault is on the LAN, so reaching it goes through the LAN broker: its
/// source, or a mirror, at a URL whose host is on the LAN ([`crate::lan_reach::on_the_lan`]). A
/// vault at a path is no network use (as `aterm::converge`'s publications read it).
fn config_on_the_lan(config: &Config) -> bool {
    matches!(&config.source, Source::Url(url) if crate::lan_reach::on_the_lan(url))
        || config
            .mirrors
            .iter()
            .any(|mirror| crate::lan_reach::on_the_lan(mirror))
}

/// The secrets step for this run, for `here`. An unconfigured Mac joins the open default ALab
/// vault on its LAN with no question, and says no code: a person at a terminal included, and an
/// agent's tab or a remote shell too ([`Here`]). Only the vault `vault` (`--vault`) names may ask
/// something, of a person at a terminal (its signing key, or another source where none answers
/// there); a join password is never asked. Where no vault answers, it asks nothing. A run looks
/// for the ALab vault (at a `.local` address), joins a `--vault` on the LAN, or reaches a vault
/// on the LAN this Mac is in, only while the LAN broker answers ([`SecretsOps::broker_unready`],
/// asked only then); a vault at a path or at a URL off the LAN is reached as anywhere, and one
/// already in sync is reached by nothing ([`Vault::holding_for_lan`]).
/// # Errors
/// The run does not name exactly one user, or the person stopped a prompt (Ctrl-C); nothing
/// was changed then.
pub fn items_at(
    sys: &dyn System,
    ctx: &Ctx,
    ops: Arc<dyn SecretsOps>,
    mode: Mode,
    vault: Option<&str>,
    here: Here,
) -> Result<Secrets, String> {
    let user = only_user(ctx)?.clone();
    let person = here == Here::Person;
    let says_codes = matches!(here, Here::Person | Here::Unbidden);
    if vault.is_none() {
        match ops.swarm_enabled() {
            Ok(true) => {}
            Ok(false) => {
                return Ok(Secrets::not_applicable(
                    user,
                    aterm_secrets::swarm::SWARM_OFF,
                ));
            }
            Err(why) => {
                return Ok(Secrets::note(
                    user,
                    Probe::Unusable {
                        why: format!("cannot read aterm.toml: {why}"),
                    },
                ));
            }
        }
    }
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
    // A run reaches the LAN only while the LAN broker answers: neither to look for the open ALab
    // vault (its address is a `.local` one) nor to reach a vault on the LAN, whose sync,
    // subscriber or wait for an admin goes through it as a look does. A read of the vault this
    // Mac is in (`--check`, `--plan`, and an apply's own probe) reads this Mac's own records and
    // reaches nothing, so only an apply that would reach it waits. The broker is asked once, and
    // only for a step that would reach the LAN.
    let unready = std::cell::OnceCell::new();
    let broker_unready = || unready.get_or_init(|| ops.broker_unready()).clone();
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
        let lan_held = (mode == Mode::Apply && config_on_the_lan(config))
            .then(broker_unready)
            .flatten();
        // A Mac waiting to be let in owes GitHub its token once the subscriber lands the join.
        // Held for the LAN broker, an idle subscriber is not started (the row reads
        // Deferred, with no wait of its own, or the sync the person turned off), so nothing
        // lands the join this run: GitHub then reads its own row (no token on this Mac yet)
        // rather than one blocked on the vault. A running subscriber (a local read) still
        // finishes the join, and GitHub waits for it, as it does where the subscriber cannot be
        // read (the vault's row then fails on that read). The subscriber is read once, here, and
        // the vault's probe judges by that same reading ([`Vault::holding_for_lan`]): one that
        // stops or starts between two reads (launchd restarting its KeepAlive job) would leave
        // GitHub waiting on a row deferred with no wait of its own, or owed nothing beside a
        // join that lands.
        let held = lan_held.map(|why| (why, ops.subscriber()));
        let idle_held = matches!(&held, Some((_, Ok(state))) if idle(state).is_some());
        let waited_on = (matches!(status.membership, Membership::Pending) && !idle_held)
            .then(|| item_id(&user));
        let vault = Vault::new(
            user,
            ops,
            source_text(&config.source),
            Origin::Configured,
            None,
            Duration::ZERO,
        );
        return Ok(Secrets {
            items: vec![Box::new(
                vault
                    .saying_codes(says_codes)
                    .asking(person)
                    .holding_for_lan(held),
            )],
            waited_on,
        });
    }
    // `--vault`'s questions need a person at the terminal; the open ALab vault needs nobody.
    let asks = person && ops.terminal();
    if mode != Mode::Apply || (vault.is_some() && !asks) {
        return Ok(Secrets::not_applicable(user, UNCONFIGURED));
    }
    // A named LAN vault waits without changing anything. The implicit default retains its
    // guarded bootstrap and subscriber below, so the broker's arrival can finish that join.
    let reaches_lan = vault.is_some_and(|typed| crate::lan_reach::on_the_lan(typed.trim()));
    if let Some(why) = reaches_lan.then(broker_unready).flatten() {
        return Ok(Secrets::note(
            user,
            Probe::Deferred {
                why: lan_broker_absent(&why),
            },
        ));
    }
    let source = match vault {
        Some(typed) => match chosen(sys, ops.as_ref(), typed)? {
            Chose::Join(source) => source,
            Chose::Note(probe) => return Ok(Secrets::note(user, probe)),
        },
        None => {
            let id = item_id(&user);
            let vault = selected(
                ctx,
                ops,
                aterm_secrets::ALAB_DEFAULT_URL,
                Duration::ZERO,
                Replaces::Read(None),
            )?;
            return Ok(Secrets {
                items: vec![Box::new(vault.saying_codes(says_codes).asking(person))],
                waited_on: Some(id),
            });
        }
    };
    let id = item_id(&user);
    let vault = Vault::new(user, ops, source, Origin::Chosen, None, Duration::ZERO);
    Ok(Secrets {
        items: vec![Box::new(vault.saying_codes(says_codes).asking(person))],
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

/// Whether `row` is the vault's look, or its reach of a vault on the LAN, left until the LAN
/// broker answers ([`LAN_BROKER_WAIT`]): setup's one elevation installs it, and a later pass
/// looks.
#[must_use]
pub fn waits_for_lan_broker(row: &crate::model::ItemReport) -> bool {
    row.id.starts_with(ITEM)
        && row
            .why
            .as_deref()
            .is_some_and(|why| why.starts_with(LAN_BROKER_WAIT))
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
    /// `aterm init` is joining it, with no join password: a person named it (`--vault`, whose
    /// signer they confirmed), or the open ALab vault answered. The join waits for nobody.
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
    /// A rotation dropped this Mac from the selected vault with no `Remove`
    /// ([`SecretsOps::dropped`]): a join keeps its key, which every desk that read the rotation
    /// refuses, so none is planned; the row's words.
    Dropped(String),
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
    /// Why the last apply stood aside: this Mac waits to be let in.
    aside: Mutex<Option<StoodAside>>,
    /// Why the last probe found this Mac waiting to be let in with its subscriber
    /// running, which finishes the join by itself ([`Item::read_aside`]).
    waiting: Mutex<Option<String>>,
    /// What the last probe planned.
    planned: Mutex<Planned>,
    /// Which vault configuration a join may replace ([`selected`]).
    replaces: Replaces,
    /// The join is the converge pass's, not a person's ([`Vault::keeping_off`]): the subscriber
    /// it puts in place is a repair, so a turn-off recorded since the pass read this Mac stays.
    keeps_off: bool,
    /// Whether its row may name the code an admin approves this Mac by
    /// ([`Vault::saying_codes`]): only where a person is at the console, never in an agent's
    /// transcript or a remote shell's.
    says_codes: bool,
    /// Whether its join may ask the person a question (the signer of a vault whose source pins
    /// none: [`SecretsOps::join`]'s `interactive`), set by [`Vault::asking`]: only in a person's
    /// own run ([`Here::Person`]), never in one aterm decided ([`Here::Unbidden`]), which shows
    /// the codes but asks nothing.
    asks: bool,
    /// Whether an apply waits for the LAN broker ([`Vault::holding_for_lan`]): the probe reads a
    /// change it would make as deferred ([`LAN_BROKER_WAIT`]). Held, it is why the broker does
    /// not answer, in its words, and the subscriber's state as [`items_at`] read it, which the
    /// probe judges by.
    lan_held: Option<(String, aterm_secrets::Result<State>)>,
}

fn retain_authority(
    current: impl Fn() -> Result<(), String> + Sync,
) -> impl Fn() -> Result<(), String> + Sync {
    let withdrawn = Mutex::new(None::<String>);
    move || {
        let mut withdrawn = withdrawn.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(why) = &*withdrawn {
            return Err(why.clone());
        }
        let result = current();
        if let Err(why) = &result {
            *withdrawn = Some(why.clone());
        }
        result
    }
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
            keeps_off: false,
            says_codes: true,
            asks: false,
            lan_held: None,
        }
    }

    /// This vault item, its row naming the code an admin approves this Mac by only when
    /// `codes`: a person at the terminal, in their own run or one aterm decided
    /// ([`Here::Person`], [`Here::Unbidden`]).
    const fn saying_codes(mut self, codes: bool) -> Self {
        self.says_codes = codes;
        self
    }

    /// This vault item, its chosen source's join ([`Origin::Chosen`]) free to ask the person
    /// (a signer to confirm) only when `asks`: their own run ([`Here::Person`]). A vault built
    /// without it asks nothing.
    const fn asking(mut self, asks: bool) -> Self {
        self.asks = asks;
        self
    }

    /// This vault item, its apply waiting for the LAN broker when `held` is why the broker does
    /// not answer and the subscriber's state as [`items_at`] read it: a vault on the LAN this Mac
    /// is in, in an apply run while the broker does not answer. Its probe still reads this Mac's own records first,
    /// reaching nothing, and judges the subscriber by that one reading, never a second (GitHub's
    /// wait, [`Secrets::waited_on`], was decided by it), so a vault in sync (or waiting on its
    /// running subscriber to be let in) reads as it is; only a change, which an apply makes by
    /// reaching the vault (its sync, its subscriber's start, a wait to be let in that asks
    /// ALab's members, the look before it), reads as deferred ([`LAN_BROKER_WAIT`]), and nothing
    /// is planned or applied.
    fn holding_for_lan(mut self, held: Option<(String, aterm_secrets::Result<State>)>) -> Self {
        self.lan_held = held;
        self
    }

    /// This vault item, its join the converge pass's (`sync.peers`): the subscriber a join puts
    /// in place is installed as a repair ([`SecretsOps::repair_subscriber`], ordered with a
    /// turn-off), never as a person's On, which clears the record that sync was turned off. A
    /// person's `aterm secrets agent off` recorded after the pass read this Mac stays, and no
    /// subscriber starts over it; System State's and `aterm init`'s joins, a person's choice of
    /// the vault, still turn sync on.
    pub(crate) const fn keeping_off(mut self) -> Self {
        self.keeps_off = true;
        self
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
        *self.aside.lock().unwrap_or_else(PoisonError::into_inner) = why.map(StoodAside::Waiting);
    }

    fn aside_also(&self, why: String) {
        let mut aside = self.aside.lock().unwrap_or_else(PoisonError::into_inner);
        *aside = Some(StoodAside::Waiting(match aside.take() {
            Some(first) => format!("{}; {why}", first.why()),
            None => why,
        }));
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

    /// The row's words for a Mac a rotation dropped from its vault with no `Remove`
    /// ([`SecretsOps::dropped`], asked only of a `Membership::Revoked` Mac); `None` otherwise.
    fn dropped(&self, status: &Status) -> Option<String> {
        let Membership::Revoked { seq } = &status.membership else {
            return None;
        };
        let name = status
            .config
            .as_ref()
            .map_or("(unnamed)", |config| config.name.as_str());
        self.ops
            .dropped(*seq)
            .map(|words| format!("vault {name}: {words}"))
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
            Membership::Revoked { seq } => match self.dropped(status) {
                Some(words) => Step::Dropped(words),
                None => Step::Join(format!(
                    "this Mac was removed from vault {name}{}",
                    aterm_secrets::client::out_since(*seq)
                )),
            },
            Membership::Pending if self.has_password() => Step::Join(format!(
                "waiting to be let into vault {name}, with a join password not yet sent"
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

    fn install(&self, current: &(dyn Fn() -> Result<(), String> + Sync)) -> Result<bool, String> {
        current()?;
        let installed = if self.planned().join && !self.keeps_off {
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
    fn subscribe(&self, current: &(dyn Fn() -> Result<(), String> + Sync)) -> Result<bool, String> {
        current()?;
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
        match self.install(current) {
            Err(why) if self.origin == Origin::Selected => match self.ops.fleeting() {
                Some(exe) => {
                    self.aside_also(fleeting_wait(&exe));
                    Ok(false)
                }
                None => Err(why),
            },
            Ok(true) if self.origin == Origin::Selected => {
                current()?;
                self.ops.started().map(|()| true)
            }
            installed => installed,
        }
    }

    fn subscribe_queued(
        &self,
        installed: &OnceCell<Result<bool, String>>,
        current: &(dyn Fn() -> Result<(), String> + Sync),
    ) -> bool {
        if current().is_err() {
            return false;
        }
        installed
            .get_or_init(|| {
                let done = self.subscribe(current);
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

    fn native_first(&self) -> bool {
        let planned = self.planned();
        self.origin == Origin::Selected
            && planned.join
            && planned.over.is_none()
            && !self.has_password()
            && aterm_secrets::is_alab_default_url(&self.source)
    }

    fn join(
        &self,
        sys: &dyn System,
        interactive: bool,
        current: &(dyn Fn() -> Result<(), String> + Sync),
    ) -> Result<(), String> {
        current()?;
        let planned = self.planned();
        let native_first = self.native_first();
        let before = planned.over.clone();
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
        let requested = || self.subscribe_queued(&installed, current);
        if native_first {
            self.ops
                .prepare_bootstrap()
                .map_err(|why| why.to_string())?;
            let admitted = || {
                current().map_err(aterm_secrets::Error::new)?;
                if self.ops.configured()?.is_some() {
                    return Err(aterm_secrets::Error::new(
                        "the vault source changed while native bootstrap ran",
                    ));
                }
                if !self.ops.swarm_enabled()? {
                    return Err(aterm_secrets::Error::new(
                        "sharing is off; bootstrap is held",
                    ));
                }
                Ok(())
            };
            if let Some(why) = self.ops.broker_unready() {
                // Keep the admitted default join in its existing recovery owner without
                // discovering or reaching any peer before the broker can serve the LAN.
                let subscribed = self.subscribe(&|| admitted().map_err(|why| why.to_string()));
                current()?;
                let owner = match &subscribed {
                    Ok(true) => Some(self.ops.subscriber()),
                    _ => None,
                };
                // Starting the subscriber can publish this very default vault. That is
                // progress for the ordinary read-back, never authority for another effect.
                let now = self.ops.configured().map_err(|why| why.to_string())?;
                current()?;
                if let Some(now) = now {
                    if aterm_secrets::client::alab_vault(&now)
                        && selected_vault(&self.source, Path::new(&self.user.home), Some(&now))?
                            .is_some()
                    {
                        return Ok(());
                    }
                    return Err("the vault source changed while bootstrap was starting".into());
                }
                let running = subscribed?
                    && !self.ops.turned_off()
                    && owner
                        .transpose()
                        .map_err(|why| why.to_string())?
                        .is_some_and(|state| idle(&state).is_none());
                let mut aside = self.aside.lock().unwrap_or_else(PoisonError::into_inner);
                let why = match aside.take() {
                    Some(previous) => {
                        format!("{}; {}", previous.why(), lan_broker_absent(&why))
                    }
                    None => lan_broker_absent(&why),
                };
                *aside = Some(if running {
                    StoodAside::Waiting(why)
                } else {
                    StoodAside::Offline(why)
                });
                return Ok(());
            }
            match self
                .ops
                .native_bootstrap(&admitted)
                .map_err(|why| why.to_string())?
            {
                aterm_secrets::client::NativeAdmission::None => {
                    admitted().map_err(|why| why.to_string())?
                }
                result => {
                    current()?;
                    self.subscribe(current)?;
                    if result == aterm_secrets::client::NativeAdmission::Uncertain {
                        let follows = if self.ops.turned_off() {
                            "joining resumes when sync is on"
                        } else {
                            "sync retries automatically"
                        };
                        self.aside_also(format!("{NATIVE_ADMISSION_PENDING}; {follows}"));
                    }
                    // A successful proof is read back through the ordinary subscriber and
                    // complete-materialization checks, never treated as readiness by itself.
                    return Ok(());
                }
            }
            let why = alab_unavailable(self.ops.as_ref());
            if let Some(why) = why {
                current()?;
                let running = self.subscribe(current)?;
                let then = if running && !self.ops.turned_off() {
                    "the subscriber retries setup automatically"
                } else {
                    "setup resumes once sync can run"
                };
                let mut aside = self.aside.lock().unwrap_or_else(PoisonError::into_inner);
                let why = match aside.take() {
                    Some(previous) => format!("{}; {why}; {then}", previous.why()),
                    None => format!("{why}; {then}"),
                };
                *aside = Some(StoodAside::Offline(why));
                return Ok(());
            }
        }
        let password = self
            .password
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        // `aterm init`'s join carries no join password and waits for nobody: the subscriber
        // finishes it. System State's join waits its window inside the join, which ends the
        // moment the Mac is in, only when it carries the join password the window's field took,
        // which an approver answers within seconds; one by code waits for nobody either: its row
        // names the code, and the subscriber's state.toml wakes the parked run once an admin
        // approves it.
        let chosen = self.origin == Origin::Chosen;
        if chosen {
            self.ops.say(&format!("joining {}…", shown(&self.source)));
        }
        let sent = password.is_some();
        current()?;
        let outcome = self.ops.join(
            sys,
            (&self.source, &replaces),
            password.as_ref(),
            interactive,
            if chosen || !sent {
                Duration::ZERO
            } else {
                self.wait
            },
            &requested,
            current,
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
                let why = format!("joining the vault: {why}");
                if native_first && self.ops.configured().is_ok_and(|now| now.is_none()) {
                    current()?;
                    if self.subscribe_queued(&installed, current) {
                        return Err(format!("{why}; the subscriber retries setup automatically"));
                    }
                }
                // A join that fails after saving the vault (its request refused or its reply
                // lost) leaves this Mac set to it and waiting, its request due at once: the
                // subscriber that asks goes in now, as `aterm secrets join`'s does. A change to
                // some other vault (another program's join meanwhile) is not this join's.
                let home = Path::new(&self.user.home);
                let saved = self.ops.configured().ok().flatten().is_some_and(|after| {
                    before.as_ref() != Some(&after)
                        && matches!(
                            selected_vault(&self.source, home, Some(&after)),
                            Ok(Some(_))
                        )
                });
                current()?;
                if saved && self.subscribe_queued(&installed, current) {
                    return Err(format!(
                        "{why}; this Mac is now set to {} and its subscriber asks to join again shortly",
                        shown(&self.source)
                    ));
                }
                return Err(why);
            }
        };
        current()?;
        installed
            .get_or_init(|| self.subscribe(current))
            .clone()
            .map_err(|why| format!("{VAULT_ANSWERED_BUT} {why}"))?;
        let JoinOutcome::Pending { code, .. } = outcome else {
            return Ok(());
        };
        // At a path, which has no queue, an admin approves this Mac's key, not its code.
        let status = self.ops.status().ok();
        if !status
            .as_ref()
            .is_some_and(|status| matches!(status.membership, Membership::In { .. }))
        {
            let approve = status
                .and_then(|status| status.approve_command())
                .unwrap_or_else(|| format!("aterm secrets approve {code}"));
            self.wait_to_be_let_in(Some(&approve), sent, current)?;
        }
        Ok(())
    }

    /// Whether this Mac is set to the ALab vault at a URL ([`alab_at_url`]) now.
    fn on_alab(&self) -> bool {
        alab_at_url(self.ops.configured().ok().flatten().as_ref())
    }

    fn settle(
        &self,
        observed: Option<&Config>,
        current: &(dyn Fn() -> Result<(), String> + Sync),
    ) -> Result<(), String> {
        let admitted = retain_authority(|| {
            current()?;
            let now = self.ops.configured().map_err(|why| why.to_string())?;
            let same_source = now.as_ref().zip(observed).is_some_and(|(now, observed)| {
                now.source == observed.source
                    && now.name == observed.name
                    && now.host == observed.host
                    && now.mirrors == observed.mirrors
                    && match observed.signer {
                        Some(pin) => aterm_secrets::client::pin_admitted(now, Some(pin)),
                        None => now.signer.is_none(),
                    }
            });
            if !same_source {
                return Err("the configured vault changed during setup's repair".into());
            }
            Ok(())
        });
        let current = &admitted;
        current()?;
        let planned = self.planned();
        if !planned.join && self.ops.turned_off() {
            return Ok(());
        }
        self.subscribe(current)?;
        current()?;
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
                self.wait_to_be_let_in(status.approve_command().as_deref(), true, current)?;
            }
            Membership::In { .. } | Membership::Plain
                if planned.files && !self.ops.synced().unwrap_or(false) =>
            {
                if !planned.join && self.ops.turned_off() {
                    return Ok(());
                }
                self.ops
                    .sync(current)
                    .map_err(|why| format!("writing the vault's files: {why}"))?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Record pending join progress ([`approval`]), including a mistyped-password remedy when
    /// one may have been sent (`retyped`). ALab asks reachable members automatically unless a
    /// member already let this Mac in. The subscriber continues the pending join; a probe only
    /// reads its progress. Other vaults retain their explicit approval instructions.
    fn wait_to_be_let_in(
        &self,
        approve: Option<&str>,
        retyped: bool,
        current: &(dyn Fn() -> Result<(), String> + Sync),
    ) -> Result<(), String> {
        current()?;
        let approve = approve.filter(|_| self.says_codes);
        let alab = self.on_alab();
        let waiting = if alab && self.ops.allowed() {
            format!("waiting to be let in: {ALLOWED}")
        } else {
            let asked = alab && {
                self.ops.say("joining ALab…");
                let heard = self.ops.ask_members(current);
                current()?;
                heard.is_ok_and(|heard| heard > 0)
            };
            let members = alab || alab_words(None, &self.source);
            format!(
                "waiting to be let in: {}",
                approval(
                    approve,
                    (&self.source, members),
                    retyped,
                    (asked, &|| self.ops.join_place())
                )
            )
        };
        let mut aside = self.aside.lock().unwrap_or_else(PoisonError::into_inner);
        *aside = Some(StoodAside::Waiting(match aside.take() {
            Some(then) => format!("{waiting}; {}", then.why()),
            None => waiting,
        }));
        Ok(())
    }
}

fn fleeting_wait(exe: &Path) -> String {
    format!(
        "this aterm runs from {}, which cannot keep sync running after it quits; open the released aterm app to install it automatically and continue setup",
        exe.display()
    )
}

fn source_text(source: &Source) -> String {
    match source {
        Source::Url(url) => url.clone(),
        Source::Path(path) => path.display().to_string(),
    }
}

/// Pending join progress: automatic admission at the ALab vault (`alab`, [`alab_words`]) in the
/// words for where this Mac is (`at`, read only then: [`LET_IN`] on the ALab LAN, else
/// `aterm_secrets::alab_join::let_in`'s, which promise no automatic admission off it), after
/// asking every member ([`ASK_FIRST`]) unless one heard this Mac ask (`asked`); or an admin's
/// approval at a path or another vault (`approve`, [`Status::approve_command`]). When a join
/// password may have been sent (`retyped`), include its mistyping remedy.
fn approval(
    approve: Option<&str>,
    (source, alab): (&str, bool),
    retyped: bool,
    (asked, at): (bool, &dyn Fn() -> JoinPlace),
) -> String {
    let at_path = source.starts_with('/') || source.starts_with("~/");
    let approve = match approve {
        Some(approve) if at_path || !alab => format!(
            "approve this Mac on an admin Mac: {approve} (the subscriber finishes the join once it is approved)"
        ),
        None if at_path || !alab => {
            "an admin Mac approves this Mac (the subscriber finishes the join once it is approved)"
                .to_owned()
        }
        _ => {
            let at = at();
            let let_in = aterm_secrets::alab_join::let_in(at);
            match (asked, at) {
                (true, JoinPlace::Home) => format!(
                    "{}, {let_in} (the subscriber finishes the join once it is let in)",
                    quiet_first()
                ),
                (true, _) => format!(
                    "{ASKED}; {let_in} (the subscriber finishes the join once it is let in)"
                ),
                (false, _) => format!(
                    "{ASK_FIRST}, then {let_in} (the subscriber finishes the join once it is let in)"
                ),
            }
        }
    };
    match retype(source).filter(|_| retyped) {
        Some(retype) => format!("{approve}; {retype}"),
        None => approve,
    }
}

fn retype(source: &str) -> Option<String> {
    if !is_url(source) {
        return None;
    }
    let join = if aterm_secrets::is_alab_default_url(source) {
        "aterm secrets join".to_owned()
    } else {
        "aterm secrets join <vault-url>".to_owned()
    };
    let source_note = if aterm_secrets::is_alab_default_url(source) {
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

/// Whether a Mac waiting on `source` is told to ask ALab's members: the ALab vault by its signer
/// ([`alab_at_url`]) or at its default URL. Another vault's members show no Allow.
fn alab_words(config: Option<&Config>, source: &str) -> bool {
    alab_at_url(config) || source == aterm_secrets::ALAB_DEFAULT_URL
}

/// Whether `config` is the ALab vault at a URL, whose members can let this Mac in.
fn alab_at_url(config: Option<&Config>) -> bool {
    config.is_some_and(|config| {
        matches!(config.source, Source::Url(_)) && aterm_secrets::client::alab_vault(config)
    })
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

fn judge(
    status: &Status,
    source: &str,
    (unsynced, synced, off): (Option<&str>, bool, bool),
    at: &dyn Fn() -> JoinPlace,
) -> Probe {
    let name = status
        .config
        .as_ref()
        .map_or("(unnamed)", |config| config.name.as_str());
    let approve = status.approve_command();
    let alab = alab_words(status.config.as_ref(), source);
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
            now: format!("waiting to be let into vault {name}; {SYNC_OFF}, and the join lands once it is on"),
        },
        // The subscriber finishes the join once this Mac is let in: nothing for this Mac to
        // do, so the row waits ([`Item::read_aside`]) rather than asking the person.
        Membership::Pending if watched => Probe::Unusable {
            why: format!(
                "waiting to be let in: {}",
                approval(approve.as_deref(), (source, alab), true, (false, at))
            ),
        },
        Membership::Pending => unwatched(format!("waiting to be let into vault {name}")),
        Membership::Revoked { seq } if alab => Probe::Blocked {
            why: format!(
                "this Mac was removed from vault {name}{}; to rejoin, run aterm members join and press Allow again on any Mac in ALab",
                aterm_secrets::client::out_since(*seq)
            ),
        },
        Membership::Revoked { seq } => Probe::Blocked {
            why: format!(
                "this Mac was removed from vault {name}{}; to rejoin, an admin Mac approves it again: {}",
                aterm_secrets::client::out_since(*seq),
                approve.as_deref().unwrap_or("aterm secrets approve <its code>")
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
                Err(why) | Ok(Step::Dropped(why)) => return Probe::Blocked { why },
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
                    // clears it), or an approved join would never land. The converge
                    // pass's join is no person's ask: its install is a repair
                    // (`keeps_off`), which leaves a record of a turn-off in place.
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
        // A Mac a rotation dropped with no `Remove` comes back only as a new Mac: the row says
        // how and nothing is planned (`aterm init` on its configured vault; a selected one said
        // so in `step`).
        if let Some(why) = self.dropped(&status) {
            return Probe::Blocked { why };
        }
        let unsynced = match status.membership {
            Membership::In { .. } | Membership::Plain | Membership::Pending => {
                // Held, the reading GitHub's wait was decided by.
                let reading = self
                    .lan_held
                    .as_ref()
                    .map_or_else(|| self.ops.subscriber(), |(_, held)| held.clone());
                match reading {
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
        // Away from the console no row names the code an admin approves this Mac by.
        let uncoded;
        let judged = if self.says_codes {
            &status
        } else {
            uncoded = Status {
                machine: None,
                ..status.clone()
            };
            &uncoded
        };
        let probe = match judge(judged, &self.source, (unsynced, synced, off), &|| {
            self.ops.join_place()
        }) {
            // A member let this Mac into the ALab vault: the read says it waits for the vault,
            // as the pass that applies does ([`Vault::wait_to_be_let_in`]).
            Probe::Unusable { .. }
                if matches!(status.membership, Membership::Pending)
                    && alab_at_url(status.config.as_ref())
                    && self.ops.allowed() =>
            {
                Probe::Unusable {
                    why: format!("waiting to be let in: {ALLOWED}"),
                }
            }
            probe => probe,
        };
        if let (Membership::Pending, Probe::Unusable { why }) = (&status.membership, &probe) {
            *self.waiting.lock().unwrap_or_else(PoisonError::into_inner) = Some(why.clone());
        }
        if let (Some((why, _)), Probe::Change { .. }) = (&self.lan_held, &probe) {
            return Probe::Deferred {
                why: lan_broker_absent(why),
            };
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
                    over: status.config.clone(),
                    subscribe: unsynced.is_some(),
                    files: !synced,
                    ..Planned::default()
                },
            });
        }
        probe
    }

    fn apply(&self, sys: &dyn System, ctx: &Ctx) -> Result<(), String> {
        self.apply_guarded(sys, ctx, &|| Ok(()))
    }

    fn apply_guarded(
        &self,
        sys: &dyn System,
        _ctx: &Ctx,
        current: &(dyn Fn() -> Result<(), String> + Sync),
    ) -> Result<(), String> {
        let was_off = self.ops.turned_off();
        let admitted_now = retain_authority(|| {
            current()?;
            if self.origin != Origin::Chosen
                && !self.ops.swarm_enabled().map_err(|why| why.to_string())?
            {
                return Err("sharing was turned off during automatic vault setup".to_owned());
            }
            if !was_off && self.ops.turned_off() {
                return Err(
                    "the vault subscriber was turned off during this join; setup left it off"
                        .into(),
                );
            }
            Ok(())
        });
        let current = &admitted_now;
        current()?;
        self.set_aside(None);
        match self.origin {
            Origin::Configured => self.settle(self.planned().over.as_ref(), current),
            Origin::Chosen => self.join(sys, self.asks, current),
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
                        self.join(sys, self.asks, current)
                    }
                    Step::Join(now) if !planned.join => Err(format!(
                        "this Mac's vault state changed after it was read ({now}), so nothing was changed; the next try plans the join"
                    )),
                    Step::Join(now) => Err(format!(
                        "this Mac's vault state changed after it was read ({now}), so nothing was changed; the next try reads it"
                    )),
                    Step::Dropped(now) => Err(format!(
                        "this Mac's vault state changed after it was read ({now}), so nothing was changed; the next try reads it"
                    )),
                    Step::Read => self.settle(
                        if planned.join {
                            status.config.as_ref()
                        } else {
                            planned.over.as_ref()
                        },
                        current,
                    ),
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
        if self.native_first() {
            return self.configured_since(&planned);
        }
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
            Look::NoVault(why) => Some(StoodAside::Offline(format!("{NO_VAULT_NOW}: {why}"))),
        }
    }

    fn stood_aside(&self) -> Option<aterm_converge::StoodAside> {
        self.aside
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
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
    use aterm_uds::{AcceptWait, AcceptedMode};
    use std::collections::VecDeque;

    /// The row's words without a LAN broker ([`Scripted::broker_absent`]): the broker's own,
    /// not installed.
    fn absent() -> String {
        lan_broker_absent(&aterm_uds::lan::Unavailable::NotInstalled.to_string())
    }

    /// A vault whose state answers from a script (the last one sticks), and which records
    /// every look for the ALab vault, join and subscriber install.
    #[derive(Default)]
    pub(crate) struct Scripted {
        bootstrap: Mutex<Option<(aterm_secrets::client::NativeAdmission, Option<Status>)>>,
        prepared: Mutex<u32>,
        native_tries: Mutex<u32>,
        stop_at_native: Mutex<Option<Arc<AtomicBool>>>,
        off_at_native: Mutex<bool>,
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
        /// Exercise the live readiness and sync paths with isolated on-disk custody.
        local_home: Mutex<Option<PathBuf>>,
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
        /// The subscriber publishes a source before its installer returns.
        published_by_install: Mutex<Option<Status>>,
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
        /// Optional override for `[swarm] enabled` (`None`: `Ok(true)`).
        swarm: Mutex<Option<aterm_secrets::Result<bool>>>,
        /// Why a subscriber just installed does not keep running (`None`: it does).
        dies: Mutex<Option<String>>,
        /// Looks answered first, in order, before `probed` answers the rest.
        looks: Mutex<VecDeque<aterm_secrets::Result<ProbeOutcome>>>,
        /// What `subscriber` answers first, in order, before `subscriber` answers the rest: a
        /// subscriber that stops or starts between two reads.
        readings: Mutex<VecDeque<aterm_secrets::Result<State>>>,
        /// What each `check` answers, in order (`Pinned` once they run out), and the sources it
        /// was asked about.
        checked: Mutex<VecDeque<Result<Checked, String>>>,
        checks: Mutex<Vec<String>>,
        /// What this Mac's vault state becomes as a join runs, before its outcome: the client
        /// saving the vault before the request it then fails to queue.
        saved_by_join: Mutex<Option<Status>>,
        /// A join observes sharing withdrawn after saving its source; a later read is on again.
        withdraw_at_join: Mutex<bool>,
        /// How many times ALab's members were asked, what each ask answers (none heard it when
        /// unset), and whether a member already let this Mac in.
        member_asks: Mutex<u32>,
        members_hear: Mutex<Option<Result<usize, String>>>,
        let_in: Mutex<bool>,
        /// The LAN broker does not answer (`broker_unready`).
        broker_absent: Mutex<bool>,
        /// Where this Mac is for its join's words (`None`: on the ALab LAN).
        place: Mutex<Option<JoinPlace>>,
        /// What `dropped` answers (`None`: an act took this Mac out), and each seq it was asked
        /// with.
        drop_words: Mutex<Option<String>>,
        dropped_asks: Mutex<Vec<u64>>,
    }

    /// The scripted port's words for a Mac a rotation dropped: a stand-in, so a test holds the
    /// routing, not the prose (`aterm_secrets::client::dropped_words`'s own test holds that).
    pub(crate) const DROPPED: &str = "dropped: the scripted port's words";

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

        /// Puts this Mac where `place` says for its join's words.
        pub(crate) fn placed(&self, place: JoinPlace) {
            *self.place.lock().unwrap() = Some(place);
        }

        /// Has `dropped` answer `words`: a rotation dropped this Mac with no `Remove`.
        pub(crate) fn drops(&self, words: &str) {
            *self.drop_words.lock().unwrap() = Some(words.to_owned());
        }

        /// Each seq `dropped` was asked with.
        pub(crate) fn dropped_asks(&self) -> Vec<u64> {
            self.dropped_asks.lock().unwrap().clone()
        }

        pub(crate) fn alab_answers(&self) {
            *self.probed.lock().unwrap() = Some(Ok(ProbeOutcome::Found(Probed {
                url: aterm_secrets::ALAB_DEFAULT_URL.into(),
                seq: 7,
                fingerprint: "0123-4567-89ab-cdef".into(),
                envelope: Vec::new(),
            })));
        }

        pub(crate) fn bootstraps_as(
            &self,
            admission: aterm_secrets::client::NativeAdmission,
            status: Option<Status>,
        ) {
            *self.bootstrap.lock().unwrap() = Some((admission, status));
        }

        pub(crate) fn stops_during_bootstrap(&self, stopped: Arc<AtomicBool>) {
            *self.stop_at_native.lock().unwrap() = Some(stopped);
        }

        pub(crate) fn turns_off_during_bootstrap(&self) {
            *self.off_at_native.lock().unwrap() = true;
        }

        fn subscribed(&self, state: aterm_secrets::Result<State>) {
            *self.subscriber.lock().unwrap() = Some(state);
        }

        pub(crate) fn probes(&self) -> u32 {
            *self.probes.lock().unwrap()
        }

        /// The LAN broker does not answer.
        pub(crate) fn broker_absent(&self) {
            *self.broker_absent.lock().unwrap() = true;
        }

        /// A join ends with `outcome`, this Mac's vault state becoming `saved` as it runs.
        pub(crate) fn joins_as(
            &self,
            saved: Option<Status>,
            outcome: aterm_secrets::Result<JoinOutcome>,
        ) {
            *self.saved_by_join.lock().unwrap() = saved;
            *self.outcome.lock().unwrap() = Some(outcome);
        }

        /// Whether each join was given a join password.
        pub(crate) fn passwords(&self) -> Vec<bool> {
            self.passwords.lock().unwrap().clone()
        }

        /// The person turns the subscriber off.
        pub(crate) fn turn_off(&self) {
            *self.turned_off.lock().unwrap() = true;
        }

        pub(crate) fn joins(&self) -> Vec<(String, bool)> {
            self.joins.lock().unwrap().clone()
        }

        pub(crate) fn installs(&self) -> u32 {
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

    /// A vault this machine is in, its subscriber installed and every file it wrote in place:
    /// converged.
    pub(crate) fn converged() -> Arc<Scripted> {
        let ops = Scripted::new(vec![Ok(configured(Membership::In {
            seq: 7,
            heard: 1_800_000_000,
        }))]);
        ops.subscribed(Ok(installed_state()));
        ops
    }

    fn installed_state() -> State {
        State::Installed {
            pid: Some(4242),
            unit: "/Users//alice/Library/LaunchAgents/systems.alab.aterm-secrets.plist".into(),
        }
    }

    impl SecretsOps for Scripted {
        fn prepare_bootstrap(&self) -> aterm_secrets::Result<()> {
            *self.prepared.lock().unwrap() += 1;
            Ok(())
        }

        fn native_bootstrap(
            &self,
            current: &(dyn Fn() -> aterm_secrets::Result<()> + Sync),
        ) -> aterm_secrets::Result<aterm_secrets::client::NativeAdmission> {
            current()?;
            *self.native_tries.lock().unwrap() += 1;
            if *self.off_at_native.lock().unwrap() {
                *self.turned_off.lock().unwrap() = true;
                current()?;
            }
            if let Some(stop) = self.stop_at_native.lock().unwrap().as_ref() {
                stop.store(true, Ordering::SeqCst);
                current()?;
            }
            let Some((admission, status)) = self.bootstrap.lock().unwrap().take() else {
                return Ok(aterm_secrets::client::NativeAdmission::None);
            };
            if let Some(status) = status {
                *self.statuses.lock().unwrap() = vec![Ok(status)].into();
            }
            Ok(admission)
        }
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
            if let Some(state) = self.readings.lock().unwrap().pop_front() {
                return state;
            }
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

        fn ask_members(
            &self,
            current: &(dyn Fn() -> Result<(), String> + Sync),
        ) -> Result<usize, String> {
            current()?;
            *self.member_asks.lock().unwrap() += 1;
            self.members_hear
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| Err("no ALab member answers on this network".into()))
        }

        fn allowed(&self) -> bool {
            *self.let_in.lock().unwrap()
        }

        fn dropped(&self, seq: u64) -> Option<String> {
            self.dropped_asks.lock().unwrap().push(seq);
            self.drop_words.lock().unwrap().clone()
        }

        fn broker_unready(&self) -> Option<String> {
            self.broker_absent
                .lock()
                .unwrap()
                .then(|| aterm_uds::lan::Unavailable::NotInstalled.to_string())
        }

        fn join_place(&self) -> JoinPlace {
            self.place.lock().unwrap().unwrap_or(JoinPlace::Home)
        }

        fn swarm_enabled(&self) -> aterm_secrets::Result<bool> {
            self.swarm.lock().unwrap().clone().unwrap_or(Ok(true))
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
            current: &(dyn Fn() -> Result<(), String> + Sync),
        ) -> aterm_secrets::Result<JoinOutcome> {
            current().map_err(aterm_secrets::Error::new)?;
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
            if let Some(saved) = self.saved_by_join.lock().unwrap().take() {
                *self.statuses.lock().unwrap() = vec![Ok(saved)].into();
            }
            if *self.withdraw_at_join.lock().unwrap() {
                *self.swarm.lock().unwrap() = Some(Ok(false));
                let refused = current().expect_err("the join observes sharing off");
                *self.swarm.lock().unwrap() = Some(Ok(true));
                return Err(aterm_secrets::Error::new(refused));
            }
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
                if let Some(status) = self.published_by_install.lock().unwrap().take() {
                    *self.statuses.lock().unwrap() = vec![Ok(status)].into();
                }
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
            if let Some(home) = self.local_home.lock().unwrap().as_ref() {
                return LiveSecrets::new(home).synced();
            }
            Ok(!*self.unwritten.lock().unwrap())
        }

        fn sync(
            &self,
            current: &(dyn Fn() -> Result<(), String> + Sync),
        ) -> aterm_secrets::Result<()> {
            current().map_err(aterm_secrets::Error::new)?;
            self.log.lock().unwrap().push("sync".into());
            *self.syncs.lock().unwrap() += 1;
            if let Some(home) = self.local_home.lock().unwrap().as_ref() {
                return LiveSecrets::new(home).sync(current);
            }
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

    pub(crate) fn configured(membership: Membership) -> Status {
        Status {
            config: Some(Config {
                source: Source::Url(aterm_secrets::ALAB_DEFAULT_URL.into()),
                mirrors: Vec::new(),
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

    #[test]
    fn default_native_admission_precedes_discovery_and_requires_complete_readback() {
        use aterm_secrets::client::NativeAdmission;
        for (mode, incomplete) in [
            (Mode::Plan, false),
            (Mode::Apply, false),
            (Mode::Apply, true),
        ] {
            let sys = fake();
            let ops = Scripted::new(vec![Ok(unconfigured())]);
            *ops.bootstrap.lock().unwrap() = Some((
                NativeAdmission::Admitted,
                Some(configured(Membership::In { seq: 7, heard: 0 })),
            ));
            *ops.unwritten.lock().unwrap() = incomplete;
            let rows = run(&sys, &ops, mode);
            let want = if mode == Mode::Plan {
                Outcome::NotApplicable
            } else if incomplete {
                Outcome::Failed
            } else {
                Outcome::Changed
            };
            assert_eq!(rows[0].outcome, want, "{mode:?}/{incomplete}: {rows:?}");
            assert_eq!(ops.probes(), 0, "native admission preceded vault discovery");
            assert!(ops.joins().is_empty());
            assert_eq!(*ops.reached.lock().unwrap(), 0, "no HTTP preflight");
            assert_eq!(
                *ops.native_tries.lock().unwrap(),
                u32::from(mode == Mode::Apply)
            );
            assert_eq!(
                *ops.prepared.lock().unwrap(),
                u32::from(mode == Mode::Apply)
            );
            assert_eq!(ops.installs(), u32::from(mode == Mode::Apply));
        }
    }

    #[test]
    fn ambiguous_native_admission_starts_its_subscriber_without_another_enrollment() {
        use aterm_secrets::client::NativeAdmission;
        let sys = fake();
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        *ops.bootstrap.lock().unwrap() = Some((NativeAdmission::Uncertain, None));
        let rows = run(&sys, &ops, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
        assert!(
            matches!(&rows[0].stood_aside, Some(StoodAside::Waiting(why)) if why.starts_with(NATIVE_ADMISSION_PENDING))
        );
        assert_eq!(ops.probes(), 0);
        assert_eq!(*ops.reached.lock().unwrap(), 0);
        assert!(ops.joins().is_empty());
        assert_eq!(ops.installs(), 1);
    }

    #[test]
    fn a_late_stop_reaches_native_publication_and_prevents_fallback() {
        let sys = fake();
        let context = ctx(&[user("alice", 501)]);
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        let stopped = Arc::new(AtomicBool::new(false));
        *ops.stop_at_native.lock().unwrap() = Some(Arc::clone(&stopped));
        let rows = crate::init::run_initial_secrets_guarded(
            &sys,
            &context,
            ops.clone(),
            aterm_secrets::ALAB_DEFAULT_URL,
            Replaces::Read(None),
            &mut None,
            Duration::ZERO,
            &|| {
                if stopped.load(Ordering::SeqCst) {
                    Err("setup stopped during native proof".into())
                } else {
                    Ok(())
                }
            },
        )
        .unwrap();
        assert_eq!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
        assert!(
            matches!(&rows[0].stood_aside, Some(StoodAside::Waiting(why)) if why.contains("setup stopped"))
        );
        assert_eq!(*ops.native_tries.lock().unwrap(), 1);
        assert_eq!(ops.probes(), 0);
        assert_eq!(*ops.reached.lock().unwrap(), 0);
        assert!(ops.joins().is_empty());
        assert_eq!(ops.installs(), 0);
    }

    #[test]
    fn a_source_chosen_during_native_lookup_is_never_replaced_by_default_fallback() {
        let sys = fake();
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        let mut chosen = configured(Membership::Pending);
        chosen.config.as_mut().unwrap().source =
            Source::Url("https://chosen.example/v1/alab".into());
        *ops.bootstrap.lock().unwrap() = Some((
            aterm_secrets::client::NativeAdmission::None,
            Some(chosen.clone()),
        ));
        let rows = run(&sys, &ops, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Failed, "{rows:?}");
        assert_eq!(ops.status().unwrap().config, chosen.config);
        assert_eq!(ops.probes(), 0);
        assert!(ops.joins().is_empty());
        assert_eq!(ops.installs(), 0);
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
        fn run_tty(
            &self,
            cmd: &Cmd,
            before: &mut dyn FnMut(&mut Cmd) -> Result<(), RunFailed>,
        ) -> Result<Option<i32>, RunFailed> {
            self.inner.run_tty(cmd, before)
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

    fn configured_alab(membership: Membership) -> Status {
        let mut status = configured(membership);
        status.config.as_mut().unwrap().signer = Some(
            aterm_secrets::b64::decode_padded(aterm_secrets::ALAB_SIGNER)
                .unwrap()
                .try_into()
                .unwrap(),
        );
        status
    }

    #[test]
    fn configured_settle_and_a_default_published_after_probe_retain_current_authority() {
        for published_late in [false, true] {
            for withdraw in [false, true] {
                let pending = configured_alab(Membership::Pending);
                let ops = Scripted::new(vec![Ok(if published_late {
                    unconfigured()
                } else {
                    pending.clone()
                })]);
                let context = ctx(&[user("alice", 501)]);
                let vault = selected(
                    &context,
                    ops.clone(),
                    aterm_secrets::ALAB_DEFAULT_URL,
                    Duration::ZERO,
                    Replaces::Read(None),
                )
                .unwrap()
                .keeping_off();
                assert!(matches!(
                    vault.probe(&fake(), &context),
                    Probe::Change { .. }
                ));
                if published_late {
                    *ops.statuses.lock().unwrap() = vec![Ok(pending)].into();
                }
                let current = || {
                    if withdraw && ops.installs() > 0 {
                        Err("setup stopped during subscriber installation".into())
                    } else {
                        Ok(())
                    }
                };
                let result = vault.apply_guarded(&fake(), &context, &current);
                assert_eq!(
                    result.is_err(),
                    withdraw,
                    "late={published_late}, withdraw={withdraw}: {result:?}"
                );
                assert!(
                    ops.joins().is_empty(),
                    "the already-configured vault is settled"
                );
                assert_eq!(ops.installs(), 1);
                assert_eq!(*ops.member_asks.lock().unwrap(), u32::from(!withdraw));
                assert_eq!(*ops.syncs.lock().unwrap(), 0);
            }
        }
    }

    #[test]
    fn a_selected_join_never_recovers_its_first_observed_sharing_refusal() {
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        ops.alab_answers();
        ops.joins_as(
            Some(configured(Membership::Pending)),
            Ok(JoinOutcome::Pending {
                code: "fixture-code".into(),
                host: "alice".into(),
            }),
        );
        *ops.withdraw_at_join.lock().unwrap() = true;
        let row = setup_pass(&ops, aterm_secrets::ALAB_DEFAULT_URL, &mut None, false);
        assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
        assert!(
            ops.swarm_enabled().unwrap(),
            "the later observation has recovered"
        );
        assert_eq!(ops.joins().len(), 1);
        assert_eq!(
            ops.installs(),
            0,
            "the refused attempt must not install its saved partial join"
        );
        assert_eq!(*ops.member_asks.lock().unwrap(), 0);
        assert_eq!(*ops.syncs.lock().unwrap(), 0);
    }

    #[test]
    fn a_configured_automatic_repair_holds_a_sharing_off_since_its_probe() {
        let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
        let context = ctx(&[user("alice", 501)]);
        let vault = Vault::new(
            user("alice", 501),
            ops.clone(),
            aterm_secrets::ALAB_DEFAULT_URL.into(),
            Origin::Configured,
            None,
            Duration::ZERO,
        );
        assert!(matches!(
            vault.probe(&fake(), &context),
            Probe::Change { .. }
        ));
        *ops.swarm.lock().unwrap() = Some(Ok(false));
        assert!(vault.apply_guarded(&fake(), &context, &|| Ok(())).is_err());
        assert_eq!(ops.installs(), 0);
        assert_eq!(*ops.syncs.lock().unwrap(), 0);
        assert_eq!(*ops.member_asks.lock().unwrap(), 0);
        assert!(ops.joins().is_empty());
    }

    #[test]
    fn settle_keeps_the_observed_source_but_accepts_its_trusted_pin_successor() {
        for configured_origin in [false, true] {
            for rotated in [false, true] {
                let ops = Scripted::new(vec![Ok(configured_alab(Membership::Pending))]);
                let vault = if configured_origin {
                    Vault::new(
                        user("alice", 501),
                        ops.clone(),
                        aterm_secrets::ALAB_DEFAULT_URL.into(),
                        Origin::Configured,
                        None,
                        Duration::ZERO,
                    )
                } else {
                    selected(
                        &ctx(&[user("alice", 501)]),
                        ops.clone(),
                        aterm_secrets::ALAB_DEFAULT_URL,
                        Duration::ZERO,
                        Replaces::Any,
                    )
                    .unwrap()
                };
                let context = ctx(&[user("alice", 501)]);
                assert!(matches!(
                    vault.probe(&fake(), &context),
                    Probe::Change { .. }
                ));
                let changed = AtomicBool::new(false);
                let current = || {
                    if ops.installs() > 0 && !changed.swap(true, Ordering::SeqCst) {
                        let mut statuses = ops.statuses.lock().unwrap();
                        let config = statuses
                            .front_mut()
                            .unwrap()
                            .as_mut()
                            .unwrap()
                            .config
                            .as_mut()
                            .unwrap();
                        if rotated {
                            config.followed.push(config.signer.unwrap());
                            config.signer = Some([0x55; 32]);
                        } else {
                            config.source = Source::Url("https://chosen.example/v1/alab".into());
                        }
                    }
                    Ok(())
                };
                let result = vault.apply_guarded(&fake(), &context, &current);
                assert_eq!(
                    result.is_ok(),
                    rotated,
                    "configured={configured_origin}, rotation={rotated}: {result:?}"
                );
                assert_eq!(*ops.member_asks.lock().unwrap(), u32::from(rotated));
                assert_eq!(*ops.syncs.lock().unwrap(), 0);
                assert!(ops.joins().is_empty());
            }
        }
    }

    #[test]
    fn configured_settle_rechecks_after_the_real_http_response_before_materializing() {
        use std::io::{BufRead, Write};
        for withdraw in [false, true] {
            let root = aterm_tempfile::tempdir().unwrap();
            let paths = Paths::for_home(&root.path().join("member"));
            let source = root.path().join("team.vault");
            let created = aterm_secrets::client::create(
                &paths,
                source.to_str().unwrap(),
                None,
                Some("member"),
            )
            .unwrap();
            let admin = aterm_secrets::client::admin_open(&paths).unwrap();
            let mut document = admin.doc.clone();
            document
                .set_entry(
                    &aterm_secrets::text::Audience::Everyone,
                    "settle-fixture",
                    b"fixture-value",
                )
                .unwrap();
            aterm_secrets::client::admin_publish(&paths, &admin, &document).unwrap();
            let cache_file = paths.dir.join("vault");
            let state_file = paths.dir.join("state.toml");
            let bytes = std::fs::read(&cache_file).unwrap();
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!(
                "http://{}/v1/team#{}",
                listener.local_addr().unwrap(),
                created.signer
            );
            let previous = aterm_secrets::client::config(&paths).unwrap();
            aterm_secrets::client::join_if_unchanged(
                &paths,
                &aterm_secrets::client::JoinOptions {
                    source: url.clone(),
                    host: Some("member".into()),
                    interactive: false,
                    wait: Duration::ZERO,
                    confirm: &|_| false,
                    password: aterm_secrets::client::JoinPassword::Given(None),
                    fetched: Some(&bytes),
                    requested: &|| {},
                    say: &|_| {},
                },
                previous.as_ref(),
            )
            .unwrap();
            let target = paths.home.join(".secrets/settle-fixture");
            std::fs::write(&target, b"preserved-local-value").unwrap();
            let state = std::fs::read(&state_file).unwrap();
            let cache = std::fs::read(&cache_file).unwrap();
            let ops = Scripted::new(vec![Ok(aterm_secrets::client::status(&paths).unwrap())]);
            *ops.local_home.lock().unwrap() = Some(paths.home.clone());
            ops.subscribed(Ok(installed_state()));
            let context = ctx(&[user("alice", 501)]);
            let vault =
                selected(&context, ops.clone(), &url, Duration::ZERO, Replaces::Any).unwrap();
            assert!(matches!(
                vault.probe(&fake(), &context),
                Probe::Change { .. }
            ));
            let stopped = AtomicBool::new(false);
            std::thread::scope(|scope| {
                let stopped = &stopped;
                scope.spawn(move || {
                    let (stream, _) =
                        aterm_uds::accept(&listener, AcceptWait::Forever, AcceptedMode::Blocking)
                            .unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(60)))
                        .unwrap();
                    stream
                        .set_write_timeout(Some(Duration::from_secs(60)))
                        .unwrap();
                    let mut reader = std::io::BufReader::new(stream);
                    loop {
                        let mut line = String::new();
                        assert!(reader.read_line(&mut line).unwrap() > 0);
                        if line == "\r\n" {
                            break;
                        }
                    }
                    let mut stream = reader.into_inner();
                    stopped.store(withdraw, Ordering::SeqCst);
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        bytes.len()
                    )
                    .unwrap();
                    stream.write_all(&bytes).unwrap();
                });
                let current = || {
                    if stopped.load(Ordering::SeqCst) {
                        Err("setup stopped while the vault response was in flight".into())
                    } else {
                        Ok(())
                    }
                };
                let result = vault.apply_guarded(&fake(), &context, &current);
                assert_eq!(result.is_err(), withdraw, "{result:?}");
            });
            assert_eq!(*ops.syncs.lock().unwrap(), 1);
            assert!(ops.joins().is_empty());
            if withdraw {
                assert_eq!(std::fs::read(&target).unwrap(), b"preserved-local-value");
                assert_eq!(std::fs::read(&state_file).unwrap(), state);
                assert_eq!(std::fs::read(&cache_file).unwrap(), cache);
            } else {
                assert_eq!(std::fs::read(&target).unwrap(), b"fixture-value");
                assert!(aterm_secrets::client::materialization_ready(&paths).unwrap());
            }
        }
    }

    fn legacy_lock_setup() -> (aterm_tempfile::TempDir, Paths, Arc<Scripted>) {
        use std::os::unix::fs::PermissionsExt;
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(&root.path().join("member"));
        let source = root.path().join("team.vault");
        aterm_secrets::client::create(&paths, source.to_str().unwrap(), None, Some("member"))
            .unwrap();
        assert!(aterm_secrets::client::materialization_ready(&paths).unwrap());
        let lock = paths.dir.join(".lock");
        std::fs::write(&lock, b"retained lock inode").unwrap();
        std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o644)).unwrap();
        let status = aterm_secrets::client::status(&paths).unwrap();
        assert!(matches!(status.membership, Membership::In { .. }));
        let ops = Scripted::new(vec![Ok(status)]);
        *ops.local_home.lock().unwrap() = Some(paths.home.clone());
        (root, paths, ops)
    }

    #[test]
    fn an_owned_legacy_lock_and_absent_subscriber_are_repaired_only_by_apply() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let (_root, paths, ops) = legacy_lock_setup();
        let lock = paths.dir.join(".lock");
        let before = std::fs::metadata(&lock).unwrap();
        assert!(aterm_secrets::client::materialization_ready(&paths).is_err());
        assert_eq!(ops.subscriber().unwrap(), State::Absent);
        let planned = run(&fake(), &ops, Mode::Plan);
        assert_eq!(planned[0].outcome, Outcome::Planned, "{planned:?}");
        assert_eq!(ops.installs(), 0);
        assert_eq!(*ops.syncs.lock().unwrap(), 0);
        assert_eq!(std::fs::metadata(&lock).unwrap().mode() & 0o7777, 0o644);
        assert_eq!(std::fs::read(&lock).unwrap(), b"retained lock inode");

        let applied = run(&fake(), &ops, Mode::Apply);
        assert_eq!(applied[0].outcome, Outcome::Changed, "{applied:?}");
        assert_eq!(ops.installs(), 1);
        assert_eq!(*ops.syncs.lock().unwrap(), 1);
        let after = std::fs::metadata(&lock).unwrap();
        assert_eq!(after.ino(), before.ino());
        assert_eq!(after.permissions().mode() & 0o7777, 0o600);
        assert_eq!(std::fs::read(&lock).unwrap(), b"retained lock inode");
        assert!(aterm_secrets::client::materialization_ready(&paths).unwrap());
        assert_eq!(run(&fake(), &ops, Mode::Plan)[0].outcome, Outcome::Ok);
        assert!(ops.joins().is_empty());
    }

    #[test]
    fn legacy_lock_setup_preserves_off_and_refuses_hostile_locks() {
        use std::os::unix::fs::PermissionsExt;
        let (_root, paths, ops) = legacy_lock_setup();
        let lock = paths.dir.join(".lock");
        ops.turn_off();
        let rows = run(&fake(), &ops, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Ok, "{rows:?}");
        assert_eq!(ops.installs(), 0);
        assert_eq!(*ops.syncs.lock().unwrap(), 0);
        assert_eq!(
            std::fs::metadata(&lock).unwrap().permissions().mode() & 0o7777,
            0o644
        );

        for hostile in ["symlink", "hardlink", "read-only"] {
            let (_root, paths, ops) = legacy_lock_setup();
            let lock = paths.dir.join(".lock");
            let other = paths.dir.join("other-lock");
            match hostile {
                "symlink" => {
                    std::fs::rename(&lock, &other).unwrap();
                    std::os::unix::fs::symlink(&other, &lock).unwrap();
                }
                "hardlink" => std::fs::hard_link(&lock, &other).unwrap(),
                _ => {
                    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o444)).unwrap()
                }
            }
            let mode = std::fs::metadata(&lock).unwrap().permissions().mode();
            let rows = run(&fake(), &ops, Mode::Apply);
            assert!(rows[0].outcome.is_failure(), "{hostile}: {rows:?}");
            assert_eq!(ops.installs(), 0, "{hostile}");
            assert_eq!(*ops.syncs.lock().unwrap(), 0, "{hostile}");
            assert_eq!(std::fs::read(&lock).unwrap(), b"retained lock inode");
            assert_eq!(std::fs::metadata(&lock).unwrap().permissions().mode(), mode);
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
            why.starts_with(&format!("waiting to be let in: {ASK_FIRST}, then {LET_IN}")),
            "{why}"
        );
        assert!(!why.contains("admin"), "{why}");
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
            let why = if matches!(
                &probed,
                Ok(ProbeOutcome::Absent | ProbeOutcome::NotThisVault(_))
            ) {
                NO_VAULT_NOW
            } else {
                NOT_HERE
            };
            *ops.probed.lock().unwrap() = Some(probed);
            let rows = run(&sys, &ops, Mode::Apply);
            assert_eq!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
            assert!(
                matches!(&rows[0].stood_aside, Some(StoodAside::Offline(heard)) if heard.starts_with(why))
            );
            assert_eq!(asked(&sys), 0, "{:?}", sys.calls());
            assert_eq!(ops.probes(), 1);
            assert!(ops.joins().is_empty());
            assert_eq!(ops.installs(), 1);
            assert_eq!(*ops.prepared.lock().unwrap(), 1);
        }

        // Control: where the ALab vault answers, it is joined, and nothing is asked either.
        let sys = fake();
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        ops.alab_answers();
        run(&sys, &ops, Mode::Apply);
        assert_eq!(asked(&sys), 0, "{:?}", sys.calls());
        assert_eq!(ops.joins().len(), 1);
    }

    /// The step for `here`, run once in `mode` for alice.
    fn run_at(sys: &Fake, ops: &Arc<Scripted>, mode: Mode, here: Here) -> Vec<ItemReport> {
        let context = ctx(&[user("alice", 501)]);
        let port: Arc<dyn SecretsOps> = ops.clone();
        let step = items_at(sys, &context, port, mode, None, here).expect("items");
        assert_eq!(step.items.len(), 1);
        engine::run(&step.items, mode, sys, &context, &mut NoJournal)
    }

    /// Design §7: "The vault step never asks for the password." Where the open ALab vault
    /// answers, `aterm init` at the console joins it with no question; the join password stays
    /// for `aterm secrets join --password-stdin` and System State's field.
    #[test]
    fn the_open_alab_vault_is_joined_at_the_console_with_no_question() {
        let sys = fake();
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        ops.alab_answers();
        run(&sys, &ops, Mode::Apply);
        assert_eq!(ops.joins().len(), 1, "the open ALab vault is joined");
        assert_eq!(*ops.passwords.lock().unwrap(), [false]);
        assert_eq!(
            asked(&sys),
            0,
            "no question at the console: {:?}",
            sys.calls()
        );
    }

    /// Design §7: off the console the vault's join still runs, as a background row that asks
    /// nothing: in an agent's tab or with no terminal, and in a remote shell.
    #[test]
    fn off_the_console_the_open_alab_vault_is_joined_asking_nothing() {
        for here in [Here::Session, Here::Remote] {
            let sys = fake();
            let ops = Scripted::new(vec![Ok(unconfigured())]);
            let after = configured(Membership::In {
                seq: 8,
                heard: 1_800_000_000,
            });
            let outcome = JoinOutcome::Joined {
                seq: 8,
                names: after.names.clone(),
            };
            ops.joins_as(Some(after), Ok(outcome));
            *ops.terminal.lock().unwrap() = false;
            ops.alab_answers();
            let rows = run_at(&sys, &ops, Mode::Apply, here);
            assert_eq!(rows[0].outcome, Outcome::Changed, "{here:?}: {rows:?}");
            assert_eq!(
                ops.joins(),
                [(aterm_secrets::ALAB_DEFAULT_URL.to_owned(), false)],
                "{here:?}: joined, and the join asks nothing"
            );
            assert_eq!(asked(&sys), 0, "{here:?}: {:?}", sys.calls());
        }

        // A plan off the console looks for nothing and joins nothing.
        let sys = fake();
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        ops.alab_answers();
        let rows = run_at(&sys, &ops, Mode::Plan, Here::Remote);
        assert_eq!(rows[0].why.as_deref(), Some(UNCONFIGURED), "{rows:?}");
        assert_eq!(ops.probes(), 0);
    }

    /// Every LAN act of aterm's on a Mac goes through the root LAN broker (or, before one is
    /// installed, the allowed direct path, which no test has), so without either no look for the
    /// ALab vault runs. The local bootstrap intent and its running subscriber retain the join
    /// for the broker's arrival; with the broker there, the pass looks and joins immediately.
    #[test]
    fn without_the_lan_broker_no_look_runs_and_the_row_waits_for_it() {
        for here in [Here::Person, Here::Session, Here::Remote, Here::Unbidden] {
            let sys = fake();
            let ops = Scripted::new(vec![Ok(unconfigured())]);
            *ops.terminal.lock().unwrap() = false;
            ops.broker_absent();
            ops.alab_answers();
            let rows = run_at(&sys, &ops, Mode::Apply, here);
            assert_eq!(
                *ops.prepared.lock().unwrap(),
                1,
                "{here:?}: bootstrap intent"
            );
            assert_eq!(ops.installs(), 1, "{here:?}: recovery owner");
            assert!(idle(&ops.subscriber().unwrap()).is_none(), "{here:?}");
            assert_eq!(ops.probes(), 0, "{here:?}: no look");
            assert_eq!(
                *ops.native_tries.lock().unwrap(),
                0,
                "{here:?}: no native ask"
            );
            assert_eq!(*ops.reached.lock().unwrap(), 0, "{here:?}: no HTTP");
            assert!(ops.joins().is_empty(), "{here:?}");
            assert_eq!(asked(&sys), 0, "{here:?}: {:?}", sys.calls());
            assert_eq!(rows[0].outcome, Outcome::Blocked, "{here:?}: {rows:?}");
            assert!(waits_for_lan_broker(&rows[0]), "{here:?}");
            assert!(finishes_by_itself(&rows[0]), "{here:?}: {rows:?}");
        }

        // Neither a failed installation nor a failed start promises an autonomous finish.
        for fails_to_start in [false, true] {
            let ops = Scripted::new(vec![Ok(unconfigured())]);
            ops.broker_absent();
            if fails_to_start {
                *ops.dies.lock().unwrap() = Some("subscriber ended during startup".into());
            } else {
                *ops.installed.lock().unwrap() =
                    Some(Err(aterm_secrets::Error::new("subscriber install failed")));
            }
            let rows = run_at(&fake(), &ops, Mode::Apply, Here::Session);
            assert_eq!(rows[0].outcome, Outcome::Failed, "{rows:?}");
            assert!(!finishes_by_itself(&rows[0]), "{rows:?}");
        }

        // A source that cannot be kept, or an owner that ended after the probe, remains offline.
        for fleeting in [false, true] {
            let ops = Scripted::new(vec![Ok(unconfigured())]);
            ops.broker_absent();
            if fleeting {
                *ops.fleeting.lock().unwrap() =
                    Some("/Volumes/aterm/aterm.app/Contents/MacOS/aterm".into());
            } else {
                *ops.readings.lock().unwrap() = [Ok(installed_state()), Ok(State::Absent)].into();
            }
            let rows = run_at(&fake(), &ops, Mode::Apply, Here::Session);
            assert_eq!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
            assert!(matches!(rows[0].stood_aside, Some(StoodAside::Offline(_))));
            assert!(!finishes_by_itself(&rows[0]), "{rows:?}");
        }

        // The installed owner can finish before its start is read back. Its admitted default
        // uses the full ordinary probe; a competing source never becomes this run's success.
        for same_default in [false, true] {
            let ops = Scripted::new(vec![Ok(unconfigured())]);
            ops.broker_absent();
            let mut after = configured_alab(Membership::In { seq: 7, heard: 0 });
            if !same_default {
                let config = after.config.as_mut().unwrap();
                config.name = "other".into();
                config.source = Source::Url("https://other.example/v1/other".into());
            }
            *ops.published_by_install.lock().unwrap() = Some(after);
            let rows = run_at(&fake(), &ops, Mode::Apply, Here::Session);
            assert_eq!(
                rows[0].outcome,
                if same_default {
                    Outcome::Changed
                } else {
                    Outcome::Failed
                },
                "{rows:?}",
            );
            assert_eq!(ops.installs(), 1);
            assert_eq!(ops.probes(), 0);
            assert_eq!(*ops.native_tries.lock().unwrap(), 0);
            assert_eq!(*ops.reached.lock().unwrap(), 0);
            assert!(ops.joins().is_empty());
            assert!(!finishes_by_itself(&rows[0]), "{rows:?}");
        }

        let sys = fake();
        let context = ctx(&[user("alice", 501)]);
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        ops.broker_absent();
        let item = selected(
            &context,
            ops.clone(),
            aterm_secrets::ALAB_DEFAULT_URL,
            Duration::ZERO,
            Replaces::Read(None),
        )
        .unwrap();
        assert!(matches!(item.probe(&sys, &context), Probe::Change { .. }));
        let stopped = item.apply_guarded(&sys, &context, &|| {
            if ops.installs() == 0 {
                Ok(())
            } else {
                Err("late Stop".into())
            }
        });
        assert_eq!(ops.installs(), 1);
        assert!(stopped.is_err());
        assert!(item.stood_aside().is_none());
        assert_eq!(*ops.native_tries.lock().unwrap(), 0);
        assert_eq!(*ops.reached.lock().unwrap(), 0);
        assert!(ops.joins().is_empty());

        let sys = fake();
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        ops.alab_answers();
        let rows = run_at(&sys, &ops, Mode::Apply, Here::Session);
        assert!(!waits_for_lan_broker(&rows[0]), "{rows:?}");
        assert_eq!(ops.probes(), 1);
        assert_eq!(ops.joins().len(), 1);
    }

    /// The vault this Mac is already in may be on its LAN too (the ALab vault's `.local`
    /// address): without the LAN broker a sync that would reach it at its URL does not, wherever
    /// the run is, and the row waits for the broker as the look does. A check reads this Mac's
    /// own records there, as anywhere, and says what a sync would change. NEGATIVE CONTROL: with
    /// the broker there, every run syncs it; with no `holding_for_lan` on the configured vault,
    /// the runs without it sync too.
    #[test]
    fn without_the_lan_broker_the_vault_this_mac_is_in_waits_for_it() {
        for here in [Here::Person, Here::Session, Here::Remote, Here::Unbidden] {
            let sys = fake();
            let ops = Scripted::new(vec![Ok(configured(Membership::In {
                seq: 7,
                heard: 1_800_000_000,
            }))]);
            ops.subscribed(Ok(installed_state()));
            *ops.unwritten.lock().unwrap() = true;
            ops.broker_absent();
            let rows = run_at(&sys, &ops, Mode::Plan, here);
            assert_eq!(*ops.syncs.lock().unwrap(), 0, "{here:?}: {rows:?}");
            assert_ne!(rows[0].outcome, Outcome::Deferred, "{here:?}: {rows:?}");
            assert_ne!(rows[0].why.as_deref(), Some(absent().as_str()), "{here:?}");
            for broker in [false, true] {
                let sys = fake();
                let ops = Scripted::new(vec![Ok(configured(Membership::In {
                    seq: 7,
                    heard: 1_800_000_000,
                }))]);
                ops.subscribed(Ok(installed_state()));
                *ops.unwritten.lock().unwrap() = true;
                if !broker {
                    ops.broker_absent();
                }
                let rows = run_at(&sys, &ops, Mode::Apply, here);
                assert_eq!(
                    *ops.syncs.lock().unwrap(),
                    u32::from(broker),
                    "{here:?}: {rows:?}"
                );
                if broker {
                    assert_eq!(rows[0].outcome, Outcome::Changed, "{here:?}: {rows:?}");
                } else {
                    assert_eq!(rows[0].outcome, Outcome::Deferred, "{here:?}: {rows:?}");
                    assert_eq!(rows[0].why.as_deref(), Some(absent().as_str()), "{here:?}");
                    assert_eq!(*ops.reached.lock().unwrap(), 0, "{here:?}");
                }
            }
        }
    }

    /// Only a reach of a vault on the LAN waits for the LAN broker, never a read of this Mac's
    /// own records: without the broker a vault at a path (a file this Mac reads, no network use)
    /// or at a URL off the LAN is synced as anywhere, and a vault on the LAN already in sync, its
    /// subscriber running, reads in place with nothing reached. NEGATIVE CONTROL: the same LAN
    /// vault with a file to write again waits
    /// ([`without_the_lan_broker_the_vault_this_mac_is_in_waits_for_it`]); with the gate on every
    /// configured URL the public vault is deferred unsynced, and on every source the path vault
    /// is too, and the vault in sync reads deferred.
    #[test]
    fn without_the_lan_broker_a_vault_off_the_lan_syncs_and_one_in_sync_reads_in_place() {
        for here in [Here::Person, Here::Session, Here::Remote, Here::Unbidden] {
            for source in [
                Source::Path("/Volumes/Keys/team.vault".into()),
                Source::Url("https://vault.example/v1/lab".into()),
            ] {
                let mut off_lan = configured(Membership::In {
                    seq: 7,
                    heard: 1_800_000_000,
                });
                if let Some(config) = off_lan.config.as_mut() {
                    config.source = source.clone();
                }
                let sys = fake();
                let ops = Scripted::new(vec![Ok(off_lan)]);
                ops.subscribed(Ok(installed_state()));
                *ops.unwritten.lock().unwrap() = true;
                ops.broker_absent();
                let rows = run_at(&sys, &ops, Mode::Apply, here);
                assert_eq!(
                    rows[0].outcome,
                    Outcome::Changed,
                    "{here:?} {source:?}: {rows:?}"
                );
                assert_eq!(*ops.syncs.lock().unwrap(), 1, "{here:?}: {rows:?}");
                assert!(!waits_for_lan_broker(&rows[0]), "{here:?}: {rows:?}");
            }

            let sys = fake();
            let ops = converged();
            ops.broker_absent();
            let rows = run_at(&sys, &ops, Mode::Apply, here);
            assert_eq!(rows[0].outcome, Outcome::Ok, "{here:?}: {rows:?}");
            assert_ne!(rows[0].why.as_deref(), Some(absent().as_str()), "{here:?}");
            assert_eq!(*ops.syncs.lock().unwrap(), 0, "{here:?}");
            assert_eq!(*ops.reached.lock().unwrap(), 0, "{here:?}: nothing reached");
            assert_eq!(ops.installs(), 0, "{here:?}");
        }
    }

    /// A `--vault` reaches the LAN only where it names a URL there: without the LAN broker a
    /// person's `--vault` at a path or at a public URL is read and joined as anywhere, and one at
    /// a `.local` URL waits for the broker, read by nothing. NEGATIVE CONTROL: a gate on every
    /// unconfigured apply defers the path and the public URL unread.
    #[test]
    fn without_the_lan_broker_only_a_named_vault_on_the_lan_waits_for_it() {
        for (typed, lan) in [
            ("/Volumes/Keys/team.vault", false),
            ("https://vault.example/v1/lab", false),
            ("http://lab-vault.local:7484/v1/lab", true),
        ] {
            let sys = fake();
            let ops = Scripted::new(vec![Ok(unconfigured())]);
            ops.broker_absent();
            let rows = run_with(&sys, &ops, Mode::Apply, Some(typed));
            assert_eq!(waits_for_lan_broker(&rows[0]), lan, "{typed}: {rows:?}");
            let read: Vec<String> = if lan {
                Vec::new()
            } else {
                vec![typed.to_owned()]
            };
            assert_eq!(*ops.checks.lock().unwrap(), read, "{typed}");
            assert_eq!(ops.joins().is_empty(), lan, "{typed}: {rows:?}");
            assert_eq!(ops.probes(), 0, "{typed}: no look for the ALab vault");
        }
    }

    /// A pairing code is said only where a person is at the console, in their own run or one
    /// aterm decided: off it, a Mac waiting to be let into another vault reads that an admin Mac
    /// approves it, without its code.
    #[test]
    fn off_the_console_a_waiting_row_names_no_code() {
        let url = "https://vault.example/v1/lab";
        let on_lab = || {
            let mut status = configured(Membership::Pending);
            if let Some(config) = status.config.as_mut() {
                config.source = Source::Url(url.into());
                config.name = "lab".into();
            }
            status
        };
        for (here, coded) in [
            (Here::Person, true),
            (Here::Unbidden, true),
            (Here::Session, false),
            (Here::Remote, false),
        ] {
            let sys = fake();
            let ops = Scripted::new(vec![Ok(on_lab())]);
            ops.subscribed(Ok(installed_state()));
            let rows = run_at(&sys, &ops, Mode::Apply, here);
            let why = rows[0].why.as_deref().expect("why it waits");
            assert_eq!(why.contains(CODE), coded, "{here:?}: {why}");
            assert!(why.contains("admin Mac"), "{here:?}: {why}");
        }
    }

    /// A look that reaches no vault (no route to the vault's address) is no vault here after
    /// that one look, on a Mac as on Linux: nothing waits for an answer and nothing is asked.
    #[test]
    fn an_unreachable_look_is_no_vault_here_at_once() {
        for linux in [false, true] {
            let mut sys = fake();
            sys.linux = linux;
            let ops = Scripted::new(vec![Ok(unconfigured())]);
            *ops.probed.lock().unwrap() = Some(Err(aterm_secrets::Error::new(format!(
                "fetching http://alab-secrets.local:7484/v1/alab: {}",
                io::Error::from_raw_os_error(libc::EHOSTUNREACH)
            ))));
            let rows = run(&sys, &ops, Mode::Apply);
            assert_eq!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
            assert!(
                matches!(&rows[0].stood_aside, Some(StoodAside::Offline(why)) if why.starts_with(NOT_HERE)),
                "{rows:?}"
            );
            assert_eq!(ops.probes(), 1);
            assert_eq!(ops.installs(), 1, "the subscriber retains the offline join");
            assert_eq!(asked(&sys), 0);
        }
    }

    #[test]
    fn a_look_kept_for_this_network_spares_the_next_and_a_failed_one_is_not_kept() {
        use aterm_secrets::heard::{self, Heard, Verdict};
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(root.path());
        let here = || Some("n00112233445566aa".to_owned());
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
                network: "n00112233445566aa".into(),
            })
        );
        // The next run on this network makes no request.
        let looked = look_here_with(root.path(), here(), 2_000, probe(other()));
        assert_eq!(looked.unwrap(), ProbeOutcome::Absent);
        assert_eq!(asked.get(), 1, "no look on a network a look found empty");
        // Another network is looked at again; a look the LAN broker could not make is not kept.
        let looked = look_here_with(
            root.path(),
            Some("n66778899aabbccdd".into()),
            2_000,
            probe(Err(aterm_secrets::Error::new(
                aterm_uds::lan::Unavailable::NotInstalled.to_string(),
            ))),
        );
        assert!(looked.is_err());
        assert_eq!(asked.get(), 2);
        assert_eq!(heard::read(&paths).unwrap().network, "n00112233445566aa");
        // Nor is a 404, an answer that is no vault, a timeout or a refused connect: none is an
        // answer about the network.
        let elsewhere = || Some("n0123456789abcdef".to_owned());
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
                "n00112233445566aa",
                "{n}"
            );
        }
        assert_eq!(asked.get(), 6, "each was asked, none kept");
    }

    #[test]
    fn the_alab_vault_answering_is_joined_with_no_join_password_asked() {
        let sys = fake();
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        let after = configured(Membership::In {
            seq: 8,
            heard: 1_800_000_000,
        });
        let outcome = JoinOutcome::Joined {
            seq: 8,
            names: after.names.clone(),
        };
        ops.joins_as(Some(after), Ok(outcome));
        ops.alab_answers();
        *ops.requests.lock().unwrap() = true;
        let rows = run(&sys, &ops, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
        assert_report_matches_current(&sys, &ops, &rows[0]);
        assert_eq!(
            ops.joins(),
            [(aterm_secrets::ALAB_DEFAULT_URL.to_owned(), true)]
        );
        assert_eq!(*ops.passwords.lock().unwrap(), [false]);
        assert_eq!(asked(&sys), 0, "nothing asked: {:?}", sys.calls());
        assert_eq!(
            ops.installs(),
            1,
            "installed once, when the request was queued"
        );
        assert_eq!(*ops.installed_by_join.lock().unwrap(), [1]);
        assert_eq!(*ops.hooked.lock().unwrap(), [true]);
    }

    /// A run aterm decided with a person at the console ([`Here::Unbidden`]) shows them the
    /// codes, but its join of the open ALab vault may ask them nothing: no signer to confirm, whatever its source pins. The
    /// person's own run, whose join may ask, is
    /// `the_alab_vault_answering_is_joined_with_no_join_password_asked`.
    /// NEGATIVE CONTROL: `items_at` building the vault with `.asking(says_codes)` makes this
    /// join interactive, and the test fails.
    #[test]
    fn a_run_aterm_decided_joins_the_alab_vault_asking_nothing() {
        let sys = fake();
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        let after = configured(Membership::In {
            seq: 8,
            heard: 1_800_000_000,
        });
        let outcome = JoinOutcome::Joined {
            seq: 8,
            names: after.names.clone(),
        };
        ops.joins_as(Some(after), Ok(outcome));
        ops.alab_answers();
        let rows = run_at(&sys, &ops, Mode::Apply, Here::Unbidden);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
        assert_eq!(
            ops.joins(),
            [(aterm_secrets::ALAB_DEFAULT_URL.to_owned(), false)],
            "joined, and the join asks nothing"
        );
        assert_eq!(*ops.passwords.lock().unwrap(), [false]);
        assert_eq!(asked(&sys), 0, "nothing asked: {:?}", sys.calls());
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
        engine::run(&step.items, Mode::Apply, &sys, &context, &mut NoJournal);
        assert_eq!(
            asked(&sys),
            1,
            "the signer is the one question: no join password, and nothing once the run makes \
             changes"
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

    /// Without a terminal the open ALab vault, which needs nobody, is still joined, asking
    /// nothing; a vault `--vault` names, whose questions need a person at the terminal, is left
    /// as it is, unread.
    #[test]
    fn without_a_terminal_the_open_alab_vault_is_joined_and_a_named_vault_is_left() {
        let sys = fake();
        sys.answer(Some(aterm_secrets::ALAB_DEFAULT_URL));
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        let after = configured(Membership::In {
            seq: 8,
            heard: 1_800_000_000,
        });
        let outcome = JoinOutcome::Joined {
            seq: 8,
            names: after.names.clone(),
        };
        ops.joins_as(Some(after), Ok(outcome));
        ops.alab_answers();
        *ops.terminal.lock().unwrap() = false;
        let rows = run(&sys, &ops, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
        assert_eq!(asked(&sys), 0);
        assert_eq!(ops.passwords(), [false], "no join password");

        let ops = Scripted::new(vec![Ok(unconfigured())]);
        *ops.terminal.lock().unwrap() = false;
        let rows = run_with(
            &sys,
            &ops,
            Mode::Apply,
            Some("https://vault.example/v1/lab"),
        );
        assert_eq!(rows[0].why.as_deref(), Some(UNCONFIGURED));
        assert!(
            ops.checks.lock().unwrap().is_empty(),
            "a named vault is not read"
        );
        assert_eq!(ops.probes(), 0);
        assert!(ops.joins().is_empty());
    }

    /// A vault at a path has no join queue: a Mac waiting on it names the command that lets
    /// its key in under its name, never a code no admin Mac can approve. One at a URL names the
    /// Allow on any member, and no admin.
    #[test]
    fn a_mac_waiting_on_a_vault_at_a_path_names_its_key_not_its_code() {
        let path = "/Users//someone/home.vault";
        let mut status = configured(Membership::Pending);
        if let Some(config) = status.config.as_mut() {
            config.source = Source::Path(path.into());
        }
        let Probe::Unusable { why } =
            judge(&status, path, (None, true, false), &|| JoinPlace::Home)
        else {
            panic!("a watched pending join waits for an admin");
        };
        let key = Recipient([7; 32]).to_bech32();
        assert!(
            why.contains(&format!("aterm secrets approve {key} --name m22")),
            "{why}"
        );
        assert!(!why.contains(CODE), "{why}");
        let Probe::Unusable { why } = judge(
            &configured(Membership::Pending),
            aterm_secrets::ALAB_DEFAULT_URL,
            (None, true, false),
            &|| JoinPlace::Home,
        ) else {
            panic!("a watched pending join waits for an admin");
        };
        assert!(why.contains(LET_IN) && !why.contains("admin"), "{why}");
    }

    /// A Mac waiting on the ALab vault says what lets it in where it is (`aterm init`'s row and
    /// System State's Secrets step alike): the automatic admission's words on the ALab LAN only,
    /// and off it `aterm_secrets::alab_join::let_in`'s, never "joining ALab automatically", whether
    /// or not a member heard it ask.
    /// NEGATIVE CONTROL: `approval` giving `LET_IN` (and `quiet_first`) wherever this Mac is fails
    /// at the first place off the LAN, "Proves: the words for where it is".
    #[test]
    fn an_alab_joins_waiting_words_follow_where_this_mac_is() {
        use aterm_secrets::alab_join::let_in;
        for at in [
            JoinPlace::Home,
            JoinPlace::Proves,
            JoinPlace::Away,
            JoinPlace::Unknown,
        ] {
            let Probe::Unusable { why } = judge(
                &configured(Membership::Pending),
                aterm_secrets::ALAB_DEFAULT_URL,
                (None, true, false),
                &|| at,
            ) else {
                panic!("a watched pending join waits");
            };
            assert!(
                why.contains(let_in(at)),
                "{at:?}: the words for where it is: {why}"
            );
            let asked = approval(
                None,
                (aterm_secrets::ALAB_DEFAULT_URL, true),
                false,
                (true, &|| at),
            );
            assert!(
                asked.contains(let_in(at)),
                "{at:?}: the words for where it is: {asked}"
            );
            assert_eq!(
                asked.contains(&quiet_first()),
                at == JoinPlace::Home,
                "{at:?}: automatic only on the ALab LAN: {asked}"
            );
        }
    }

    /// A Mac waiting on, or removed from, a vault other than ALab is told what lets it in there,
    /// an admin Mac's approve of its code, never to press Allow on a Mac in ALab: that vault's
    /// members show no Allow for it.
    /// NEGATIVE CONTROL: with `alab_words` always true (the old words for every vault at a URL),
    /// both rows send the person to ALab and the test fails.
    #[test]
    fn a_mac_on_another_vault_is_never_sent_to_alab() {
        let url = "https://vault.example/v1/lab";
        let on = |membership| {
            let mut status = configured(membership);
            if let Some(config) = status.config.as_mut() {
                config.source = Source::Url(url.into());
                config.name = "lab".into();
            }
            status
        };
        let Probe::Unusable { why } =
            judge(&on(Membership::Pending), url, (None, true, false), &|| {
                JoinPlace::Home
            })
        else {
            panic!("a watched pending join waits");
        };
        assert!(
            why.contains(&format!("aterm secrets approve {CODE}")),
            "{why}"
        );
        assert!(!why.contains("ALab") && !why.contains(LET_IN), "{why}");
        let Probe::Blocked { why } = judge(
            &on(Membership::Revoked { seq: 9 }),
            url,
            (None, true, false),
            &|| JoinPlace::Home,
        ) else {
            panic!("a removed Mac is blocked");
        };
        assert!(
            why.contains(&format!("aterm secrets approve {CODE}")),
            "{why}"
        );
        assert!(!why.contains("ALab"), "{why}");
    }

    /// `docs/DESIGN-fluid-swarm-2026-10-28.md` F10: the join System State and `aterm init` make
    /// at the ALab vault asks every member it hears itself, as `aterm members join --no-wait`
    /// does, so a row shows on a member before anyone runs a verb on this Mac.
    #[test]
    fn a_join_waiting_on_the_alab_vault_asks_every_member_itself() {
        let signer: [u8; 32] = aterm_secrets::b64::decode_padded(aterm_secrets::ALAB_SIGNER)
            .expect("the ALab signer")
            .try_into()
            .expect("32 bytes");
        let on_alab = |membership| {
            let mut status = configured(membership);
            status.config.as_mut().expect("a config").signer = Some(signer);
            status
        };
        let joining = |status: Status| {
            let ops = Scripted::new(vec![Ok(unconfigured())]);
            ops.alab_answers();
            *ops.requests.lock().unwrap() = true;
            ops.joins_as(
                Some(status),
                Ok(JoinOutcome::Pending {
                    code: CODE.into(),
                    host: "m22".into(),
                }),
            );
            ops
        };
        let waiting = |ops: &Arc<Scripted>| {
            let sys = fake();
            sys.answer(None);
            let rows = run(&sys, ops, Mode::Apply);
            assert_eq!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
            rows[0].why.clone().expect("why it waits")
        };
        let asked = format!("waiting to be let in: {}, {LET_IN}", quiet_first());

        // The join queues, then asks the members once; one heard it, so the row starts at the
        // Allow that member shows.
        let ops = joining(on_alab(Membership::Pending));
        *ops.members_hear.lock().unwrap() = Some(Ok(2));
        let why = waiting(&ops);
        assert_eq!(*ops.member_asks.lock().unwrap(), 1);
        assert!(why.starts_with(&asked), "{why}");
        assert!(!why.contains("admin"), "{why}");

        // No member heard it: the row starts at `aterm members join`, which asks them all.
        let ops = joining(on_alab(Membership::Pending));
        let why = waiting(&ops);
        assert_eq!(*ops.member_asks.lock().unwrap(), 1);
        assert!(
            why.starts_with(&format!("waiting to be let in: {ASK_FIRST}, then {LET_IN}")),
            "{why}"
        );

        // A later pass that finds the subscriber down starts it and asks again: the member's
        // row is one row, and one it lost (a restart, a day gone) shows again.
        let ops = Scripted::new(vec![Ok(on_alab(Membership::Pending))]);
        *ops.members_hear.lock().unwrap() = Some(Ok(1));
        let why = waiting(&ops);
        assert_eq!(*ops.member_asks.lock().unwrap(), 1);
        assert!(why.starts_with(&asked), "{why}");

        // A pass that finds the subscriber running only reads: nobody is asked, and the row
        // names `aterm members join`, which asks again.
        let ops = Scripted::new(vec![Ok(on_alab(Membership::Pending))]);
        ops.subscribed(Ok(installed_state()));
        *ops.members_hear.lock().unwrap() = Some(Ok(1));
        let why = waiting(&ops);
        assert_eq!(*ops.member_asks.lock().unwrap(), 0);
        assert!(
            why.starts_with(&format!("waiting to be let in: {ASK_FIRST}, then {LET_IN}")),
            "{why}"
        );

        // A member already let this Mac in: nobody is asked, and the row says what it waits on,
        // on the pass that starts the subscriber and on one that only reads.
        for running in [false, true] {
            let ops = Scripted::new(vec![Ok(on_alab(Membership::Pending))]);
            if running {
                ops.subscribed(Ok(installed_state()));
            }
            *ops.let_in.lock().unwrap() = true;
            let why = waiting(&ops);
            assert_eq!(*ops.member_asks.lock().unwrap(), 0);
            assert_eq!(why, format!("waiting to be let in: {ALLOWED}"), "{running}");
        }

        // Control: a vault that is not ALab's has no members to ask, and an Allow left from
        // ALab does not speak for it.
        let ops = joining(configured(Membership::Pending));
        *ops.members_hear.lock().unwrap() = Some(Ok(2));
        *ops.let_in.lock().unwrap() = true;
        let why = waiting(&ops);
        assert_eq!(*ops.member_asks.lock().unwrap(), 0);
        assert!(!why.starts_with(&asked), "{why}");
        assert!(!why.contains(ALLOWED), "{why}");
    }

    #[test]
    fn a_join_that_waits_for_an_admin_stands_aside_naming_the_code() {
        let sys = fake();
        sys.answer(None);
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        ops.alab_answers();
        *ops.requests.lock().unwrap() = true;
        ops.joins_as(
            Some(configured(Membership::Pending)),
            Ok(JoinOutcome::Pending {
                code: CODE.into(),
                host: "m22".into(),
            }),
        );
        let rows = run(&sys, &ops, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
        assert!(matches!(rows[0].stood_aside, Some(StoodAside::Waiting(_))));
        let why = rows[0].why.as_deref().unwrap();
        assert!(
            why.starts_with(&format!("waiting to be let in: {ASK_FIRST}, then {LET_IN}")),
            "{why}"
        );
        assert!(!why.contains("admin"), "{why}");
        assert!(!why.contains("mistyped"), "no password was typed: {why}");
        assert_eq!(*ops.passwords.lock().unwrap(), [false]);
        assert_eq!(
            *ops.waits.lock().unwrap(),
            [Duration::ZERO],
            "the join is not awaited"
        );
        assert_eq!(*ops.installed_by_join.lock().unwrap(), [1]);
        assert_eq!(*ops.hooked.lock().unwrap(), [true]);
        assert_eq!(ops.installs(), 1, "the subscriber finishes the join");
    }

    #[test]
    fn a_join_that_gave_up_waiting_on_a_mac_the_subscriber_let_in_is_done() {
        let sys = fake();
        sys.answer(None);
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        ops.alab_answers();
        *ops.requests.lock().unwrap() = true;
        ops.joins_as(
            Some(configured(Membership::In {
                seq: 8,
                heard: 1_800_000_000,
            })),
            Ok(JoinOutcome::Pending {
                code: CODE.into(),
                host: "m22".into(),
            }),
        );
        let rows = run(&sys, &ops, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
        assert_eq!(rows[0].why, None);
        assert_eq!(ops.installs(), 1);
    }

    #[test]
    fn github_waits_for_a_vault_join_that_waits_to_be_let_in() {
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
                .find(|row| row.family == "github" && !row.id.starts_with("init.github.keychain@"))
                .expect("the GitHub rows are still produced");
            assert!(
                auth.why
                    .as_deref()
                    .unwrap()
                    .starts_with("waits for init.secrets@alice: waiting to be let in"),
                "{auth:?}"
            );
            assert!(matches!(auth.stood_aside, Some(StoodAside::Waiting(_))));
        };

        let sys = fake();
        configured_github(&sys);
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        ops.alab_answers();
        ops.joins_as(
            Some(configured(Membership::Pending)),
            Ok(JoinOutcome::Pending {
                code: CODE.into(),
                host: "m22".into(),
            }),
        );
        let port: Arc<dyn SecretsOps> = ops.clone();
        let step = items(&sys, &context, port, Mode::Apply, None).unwrap();
        let mut all = step.items;
        all.extend(crate::init_github::items(&sys, &context, step.waited_on.as_deref()).unwrap());
        let rows = engine::run(&all, Mode::Apply, &sys, &context, &mut NoJournal);
        assert!(
            matches!(rows[0].stood_aside, Some(StoodAside::Waiting(_))),
            "{rows:?}"
        );
        assert_eq!(asked(&sys), 0, "{:?}", sys.calls());
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

    /// alice's Mac with git and her own home, which the GitHub step reads beside the vault's,
    /// and, where `github` gives one, her init.toml with that `[github]` table.
    fn with_git(github: Option<&[u8]>) -> Fake {
        let sys = fake();
        sys.executable(crate::items::github::CLT_GIT, b"git")
            .stat_of(
                "/Users//alice",
                Stat {
                    uid: 501,
                    gid: 20,
                    mode: 0o700,
                    dir: true,
                    symlink: false,
                    len: 0,
                },
            );
        crate::git_credential::script::unconfigured(&sys, &user("alice", 501));
        if let Some(profile) = github {
            const INIT_TOML: &str = "/Users//alice/.config/aterm/init.toml";
            sys.file(INIT_TOML, profile).stat_of(
                INIT_TOML,
                Stat {
                    uid: 501,
                    gid: 20,
                    mode: 0o644,
                    dir: false,
                    symlink: false,
                    len: profile.len() as u64,
                },
            );
            let alice = user("alice", 501);
            crate::git_credential::script::unconfigured(&sys, &alice);
        }
        sys
    }

    /// Without the LAN broker, a Mac waiting to be let into
    /// a vault at a URL whose subscriber is down has that subscriber held (the vault's row
    /// Deferred, the person's step), so nothing lands the join this run: with no init.toml
    /// `[github]`, GitHub reads its own row, no token on this Mac yet, rather than one blocked on
    /// the vault with no wait to name, and the apply exits 0. A running subscriber still finishes
    /// the join, so GitHub waits for it, and the apply exits 0 too. An init.toml `[github]` that
    /// names a `token_file` reads that file's own step, which says the file is not on this Mac
    /// yet and fails (exit 1); one that names only an account reads the vault's token file as
    /// the person's step, which needs them (exit 1).
    /// NEGATIVE CONTROL: GitHub composed after the held vault whatever the subscriber (as it
    /// was) reads Blocked on it with no wait of its own, and the apply exits 1.
    #[test]
    fn a_held_waiting_vault_with_its_subscriber_down_leaves_github_its_own_row() {
        let context = ctx(&[user("alice", 501)]);
        let applied = |subscriber: State, after: Option<Option<&str>>, github: Option<&[u8]>| {
            let sys = with_git(github);
            let ops = Scripted::new(vec![Ok(configured(Membership::Pending))]);
            ops.subscribed(Ok(subscriber));
            ops.broker_absent();
            let port: Arc<dyn SecretsOps> = ops.clone();
            let step = items_at(&sys, &context, port, Mode::Apply, None, Here::Session).unwrap();
            let mut all = step.items;
            let after = after.unwrap_or(step.waited_on.as_deref());
            all.extend(crate::init_github::items(&sys, &context, after).unwrap());
            let rows = engine::run(&all, Mode::Apply, &sys, &context, &mut NoJournal);
            assert_eq!(ops.installs(), 0, "{rows:?}");
            rows
        };
        let github = |rows: &[ItemReport]| {
            rows.iter()
                .find(|row| row.family == "github" && !row.id.starts_with("init.github.keychain@"))
                .cloned()
                .expect("the GitHub row")
        };

        let rows = applied(State::Absent, None, None);
        assert_eq!(rows[0].outcome, Outcome::Deferred, "{rows:?}");
        assert_eq!(rows[0].why.as_deref(), Some(absent().as_str()), "{rows:?}");
        let own = github(&rows);
        assert_eq!(own.outcome, Outcome::NotApplicable, "{own:?}");
        assert!(
            own.why
                .as_deref()
                .is_some_and(|why| why.starts_with("no GitHub token on this Mac yet")),
            "{own:?}"
        );
        assert_eq!(crate::init::apply_status_for_test(&rows), 0, "{rows:?}");

        let rows = applied(
            State::Absent,
            None,
            Some(b"[github]\naccount='octocat'\ntoken_file='~/.secrets/github'\n"),
        );
        assert_eq!(rows[0].outcome, Outcome::Deferred, "{rows:?}");
        let file = github(&rows);
        assert_eq!(file.outcome, Outcome::Blocked, "{file:?}");
        assert_eq!(
            file.why.as_deref(),
            Some("the GitHub token file (/Users//alice/.secrets/github) is not on this Mac yet"),
            "{file:?}"
        );
        assert!(file.stood_aside.is_none(), "{file:?}");
        assert_eq!(crate::init::apply_status_for_test(&rows), 1, "{rows:?}");

        let rows = applied(State::Absent, None, Some(b"[github]\naccount='octocat'\n"));
        assert_eq!(rows[0].outcome, Outcome::Deferred, "{rows:?}");
        let account = github(&rows);
        assert_eq!(account.outcome, Outcome::Person, "{account:?}");
        assert!(account.stood_aside.is_none(), "{account:?}");
        assert_eq!(crate::init::apply_status_for_test(&rows), 1, "{rows:?}");

        let rows = applied(installed_state(), None, None);
        assert!(
            matches!(rows[0].stood_aside, Some(StoodAside::Waiting(_))),
            "{rows:?}"
        );
        let waits = github(&rows);
        assert!(
            waits.why.as_deref().is_some_and(
                |why| why.starts_with("waits for init.secrets@alice: waiting to be let in")
            ),
            "{waits:?}"
        );
        assert!(matches!(waits.stood_aside, Some(StoodAside::Waiting(_))));
        assert_eq!(crate::init::apply_status_for_test(&rows), 0, "{rows:?}");

        let rows = applied(State::Absent, Some(Some("init.secrets@alice")), None);
        let blocked = github(&rows);
        assert_eq!(blocked.outcome, Outcome::Blocked, "{blocked:?}");
        assert_eq!(
            blocked.why.as_deref(),
            Some("waits for init.secrets@alice"),
            "{blocked:?}"
        );
        assert!(blocked.stood_aside.is_none(), "{blocked:?}");
        assert_eq!(crate::init::apply_status_for_test(&rows), 1, "{rows:?}");
    }

    /// Held for the LAN broker, the vault's row and GitHub's wait judge by one reading
    /// of the subscriber, taken as the step is composed, so a subscriber that stops or starts
    /// before the vault's probe (launchd restarting its KeepAlive job) leaves the two agreeing,
    /// whichever way it moved, and the apply exits 0: running at that reading, the vault's row
    /// waits to be let in and GitHub waits for it; idle, the vault's row reads Deferred and
    /// GitHub its own row. The later answer is never read.
    /// NEGATIVE CONTROL (the mutant): the vault's probe reading `subscriber` again, in place of
    /// the held reading, fails both orders: running then idle reads GitHub Blocked beside a
    /// Deferred vault, with no wait of its own (exit 1), and idle then running a vault waiting to
    /// be let in beside a GitHub that owes nothing.
    #[test]
    fn a_held_vaults_row_and_githubs_wait_judge_by_one_reading_of_the_subscriber() {
        let context = ctx(&[user("alice", 501)]);
        for (first, then) in [
            (installed_state(), State::Absent),
            (State::Absent, installed_state()),
        ] {
            let running = idle(&first).is_none();
            let sys = with_git(None);
            let ops = Scripted::new(vec![Ok(configured(Membership::Pending))]);
            *ops.readings.lock().unwrap() = vec![Ok(first), Ok(then)].into();
            ops.broker_absent();
            let port: Arc<dyn SecretsOps> = ops.clone();
            let step = items_at(&sys, &context, port, Mode::Apply, None, Here::Session).unwrap();
            let mut all = step.items;
            all.extend(
                crate::init_github::items(&sys, &context, step.waited_on.as_deref()).unwrap(),
            );
            let rows = engine::run(&all, Mode::Apply, &sys, &context, &mut NoJournal);
            let github = rows
                .iter()
                .find(|row| row.family == "github" && !row.id.starts_with("init.github.keychain@"))
                .expect("the GitHub row");
            let waits = |row: &ItemReport| matches!(row.stood_aside, Some(StoodAside::Waiting(_)));
            assert_eq!(waits(&rows[0]), running, "running {running}: {rows:?}");
            assert_eq!(waits(github), running, "running {running}: {rows:?}");
            if !running {
                assert_eq!(rows[0].outcome, Outcome::Deferred, "{rows:?}");
                assert_eq!(github.outcome, Outcome::NotApplicable, "{rows:?}");
            }
            assert_eq!(
                crate::init::apply_status_for_test(&rows),
                0,
                "running {running}: {rows:?}"
            );
            assert_eq!(
                ops.readings.lock().unwrap().len(),
                1,
                "running {running}: the later answer is never read"
            );
            assert_eq!(ops.installs(), 0, "{rows:?}");
        }
    }

    #[test]
    fn ctrl_c_at_a_prompt_stops_the_run_with_nothing_joined() {
        let context = ctx(&[user("alice", 501)]);
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
        assert!(why.contains(LET_IN), "{why}");
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
            Some(
                "joining the vault: vault alab: the signature does not verify; the subscriber retries setup automatically"
            )
        );
        assert_eq!(ops.installs(), 1);

        // A join that fails after saving the vault (its request refused) leaves this Mac set to
        // it and waiting: the subscriber that asks again goes in, and the row says so.
        let sys = fake();
        sys.answer(None);
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        ops.alab_answers();
        *ops.saved_by_join.lock().unwrap() = Some(configured(Membership::Pending));
        *ops.outcome.lock().unwrap() = Some(Err(aterm_secrets::Error::new(
            "asking to join: the join queue already holds 256 requests",
        )));
        let rows = run(&sys, &ops, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Failed, "{rows:?}");
        assert_eq!(
            rows[0].why.as_deref(),
            Some(
                format!(
                    "joining the vault: asking to join: the join queue already holds 256 requests; this Mac is now set to {} and its subscriber asks to join again shortly",
                    shown(aterm_secrets::ALAB_DEFAULT_URL)
                )
                .as_str()
            )
        );
        assert_eq!(ops.installs(), 1);

        // What another program saved meanwhile, a vault this join did not ask for, is not this
        // join's save: no subscriber goes in for it and the row says only what failed.
        let sys = fake();
        sys.answer(None);
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        ops.alab_answers();
        let mut elsewhere = configured(Membership::Pending);
        if let Some(config) = elsewhere.config.as_mut() {
            config.source = Source::Url("http://vault.example:7484/v1/other".into());
            config.name = "other".into();
        }
        *ops.saved_by_join.lock().unwrap() = Some(elsewhere);
        *ops.outcome.lock().unwrap() = Some(Err(aterm_secrets::Error::new(
            "asking to join: the join queue already holds 256 requests",
        )));
        let rows = run(&sys, &ops, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Failed, "{rows:?}");
        assert_eq!(
            rows[0].why.as_deref(),
            Some("joining the vault: asking to join: the join queue already holds 256 requests")
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
        assert!(live(&two).is_err());
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
        assert!(!fleeting_wait(&image).contains("--program"));

        let ops = Scripted::new(vec![Ok(unconfigured())]);
        ops.joins_as(
            Some(configured(Membership::In { seq: 7, heard: 0 })),
            Ok(JoinOutcome::Joined {
                seq: 7,
                names: Vec::new(),
            }),
        );
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

        let ops = Scripted::new(vec![Ok(unconfigured())]);
        ops.alab_answers();
        ops.joins_as(
            Some(configured(Membership::Pending)),
            Ok(JoinOutcome::Pending {
                code: CODE.into(),
                host: "m22".into(),
            }),
        );
        *ops.requests.lock().unwrap() = true;
        *ops.outcome.lock().unwrap() = Some(Ok(JoinOutcome::Pending {
            code: CODE.into(),
            host: "m22".into(),
        }));
        *ops.fleeting.lock().unwrap() = Some(image.clone());
        let row = setup_pass(&ops, source, &mut None, false);
        assert_eq!(row.outcome, Outcome::Blocked, "{row:?}");
        let why = row.why.as_deref().unwrap();
        assert!(
            why.starts_with(&format!("waiting to be let in: {ASK_FIRST}, then {LET_IN}")),
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
    }

    #[test]
    fn the_setup_stage_journals_into_the_accounts_journal() {
        let sys = fake();
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        ops.alab_answers();
        ops.joins_as(
            Some(configured(Membership::In { seq: 7, heard: 0 })),
            Ok(JoinOutcome::Joined {
                seq: 7,
                names: Vec::new(),
            }),
        );
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
        *ops.unreachable.lock().unwrap() = Some(Look::Unreached(
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
                    "the vault does not answer, so nothing was changed: fetching http://alab-secrets.local:7484/v1/alab: No route to host (os error 65)"
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
            Some(Look::Unreached("No route to host (os error 65)".into()));
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
    fn a_selected_join_waits_for_an_admin_only_with_the_join_password() {
        let source = aterm_secrets::ALAB_DEFAULT_URL;
        for typed in [false, true] {
            let ops = Scripted::new(vec![Ok(unconfigured())]);
            ops.alab_answers();
            ops.joins_as(
                Some(configured(Membership::Pending)),
                Ok(JoinOutcome::Pending {
                    code: CODE.into(),
                    host: "m22".into(),
                }),
            );
            let mut held = typed.then(password);
            let row = setup_pass(&ops, source, &mut held, false);
            let wait = if typed { SETUP_WAIT } else { Duration::ZERO };
            assert_eq!(*ops.waits.lock().unwrap(), [wait], "typed: {typed}");
            assert!(
                row.outcome == Outcome::Blocked
                    && matches!(row.stood_aside, Some(StoodAside::Waiting(_)))
                    && row.why.as_deref().is_some_and(|why| why.contains(LET_IN)),
                "typed: {typed}: {row:?}"
            );
        }
    }

    /// A Mac a rotation dropped from its vault with no `Remove` (one-mesh E2) is never joined
    /// again, which would ask with the key every desk that read the rotation refuses: System
    /// State's Secrets stage and `aterm init` read it blocked with its words (the way back as a new
    /// Mac) and change nothing, and the port is asked only of a Mac its vault let go.
    ///
    /// CONTROL: the same Mac with no dropped words (an act took it out) is planned a join, as
    /// before. NEGATIVE CONTROL: a step that plans a join for every `Membership::Revoked` fails
    /// the selected read; a probe without `Vault::dropped`'s return gives `judge`'s removal words
    /// and fails the configured reads.
    #[test]
    fn a_dropped_mac_is_never_joined_again_and_says_how_it_comes_back() {
        let context = ctx(&[user("alice", 501)]);
        let alab = aterm_secrets::ALAB_DEFAULT_URL;
        let revoked = |name: &str| {
            let mut status = configured(Membership::Revoked { seq: 9 });
            if name != "alab" {
                let config = status.config.as_mut().unwrap();
                config.source = Source::Url(format!("https://vault.example/v1/{name}"));
                config.name = name.to_owned();
            }
            let ops = Scripted::new(vec![Ok(status)]);
            ops.subscribed(Ok(installed_state()));
            ops
        };

        // System State's Secrets stage: the selected vault.
        let ops = revoked("alab");
        ops.drops(DROPPED);
        let vault = selected(&context, ops.clone(), alab, SETUP_WAIT, Replaces::Any).unwrap();
        let probe = vault.probe(&fake(), &context);
        assert_eq!(
            probe,
            Probe::Blocked {
                why: format!("vault alab: {DROPPED}")
            }
        );
        assert_eq!(vault.planned(), Planned::default(), "nothing is planned");
        assert_eq!(ops.dropped_asks(), [9]);

        // `aterm init` on its configured vault, the ALab vault or another.
        for name in ["alab", "lab"] {
            let ops = revoked(name);
            ops.drops(DROPPED);
            let rows = run(&fake(), &ops, Mode::Apply);
            assert_eq!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
            let why = rows[0].why.as_deref().unwrap();
            assert_eq!(why, format!("vault {name}: {DROPPED}"));
            assert!(ops.changes().is_empty(), "{:?}", ops.changes());
            assert!(ops.joins().is_empty());
        }

        // Asked only of a Mac its vault let go.
        let ops = converged();
        ops.drops(DROPPED);
        let rows = run(&fake(), &ops, Mode::Plan);
        assert_ne!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
        assert!(ops.dropped_asks().is_empty());

        // CONTROL: an act took it out, and the stage plans its join as before.
        let ops = revoked("alab");
        let vault = selected(&context, ops.clone(), alab, SETUP_WAIT, Replaces::Any).unwrap();
        assert!(
            matches!(vault.probe(&fake(), &context), Probe::Change { .. }) && vault.planned().join,
            "a removed Mac is planned a join"
        );
        assert_eq!(ops.dropped_asks(), [9]);
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
            mirrors: Vec::new(),
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
            )
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
        ops.alab_answers();
        ops.joins_as(
            Some(configured(Membership::In { seq: 7, heard: 0 })),
            Ok(JoinOutcome::Joined {
                seq: 7,
                names: Vec::new(),
            }),
        );
        *ops.turned_off.lock().unwrap() = true;
        let _ = setup_pass(&ops, source, &mut None, false);
        assert_eq!(ops.joins().len(), 1, "the join is made");
        assert_eq!(ops.installs(), 1, "a new join turns sync on");
        assert!(!ops.turned_off(), "and the record that it was off goes");
    }

    /// The background converge join has no authority after a new subscriber Off. It neither
    /// enrolls nor installs over that choice; the same join without Off remains eligible.
    /// Explicit System State joins keep their separate person's On behavior above.
    #[test]
    fn the_converge_join_keeps_a_turn_off_recorded_since_its_read() {
        for off in [true, false] {
            let ops = Scripted::new(vec![Ok(unconfigured())]);
            ops.alab_answers();
            ops.joins_as(
                Some(configured(Membership::In { seq: 7, heard: 0 })),
                Ok(JoinOutcome::Joined {
                    seq: 7,
                    names: Vec::new(),
                }),
            );
            *ops.turned_off.lock().unwrap() = off;
            let port: Arc<dyn SecretsOps> = ops.clone();
            let rows = crate::init::run_converge_join(
                &fake(),
                &ctx(&[user("alice", 501)]),
                port,
                aterm_secrets::ALAB_DEFAULT_URL,
                Replaces::Read(None),
                &mut crate::engine::NoJournal,
                &|| Ok(()),
            )
            .unwrap();
            assert_eq!(ops.joins().len(), usize::from(!off), "{rows:?}");
            assert_eq!(ops.turned_off(), off, "off {off}: {rows:?}");
            assert_eq!(ops.installs(), u32::from(!off), "off {off}: {rows:?}");
            if off {
                assert_eq!(ops.probes(), 0);
                assert_eq!(rows[0].outcome, Outcome::NotApplicable);
            }
        }
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
            mirrors: Vec::new(),
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
        let ops = Scripted::new(vec![Ok(team())]);
        ops.joins_as(
            Some(configured(Membership::In { seq: 7, heard: 0 })),
            Ok(JoinOutcome::Joined {
                seq: 7,
                names: vec!["gh_access_token".into()],
            }),
        );
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

        let ops = Scripted::new(vec![Ok(unconfigured())]);
        ops.alab_answers();
        ops.joins_as(
            Some(configured(Membership::In { seq: 7, heard: 0 })),
            Ok(JoinOutcome::Joined {
                seq: 7,
                names: vec!["gh_access_token".into()],
            }),
        );
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
            ops.log()[1].contains("this Mac was removed from vault"),
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
        let outcome = wait_for_approval(
            &reader,
            &source,
            Duration::from_secs(60),
            true,
            None,
            &|line| {
                said.lock().unwrap().push(line.to_owned());
            },
            &|| Ok(()),
        )
        .unwrap();
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
        // This test's own server answers each check within CHECK_WAIT plus the one-minute hang
        // detector (aterm_spec::wait::MUST_HAPPEN) in place of the fetch margin's 5 s.
        let _answering = aterm_secrets::client::ServersAnswering::within(Duration::from_secs(60));

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
        let created =
            aterm_secrets::client::test_create_with_password(&admin, &url, None, Some("admin"))
                .unwrap();
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
            wait_for_approval(
                &live.paths(),
                &source,
                Duration::from_secs(60),
                false,
                None,
                &|line| live.say(line),
                &|| Err(aterm_secrets::Error::new("setup stopped"))
            )
            .is_err(),
            "a stopped wait ends at once"
        );
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let approver = {
            let stop = stop.clone();
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                    let _ = aterm_secrets::client::auto_approve(&admin);
                    std::thread::sleep(Duration::from_millis(200));
                }
            })
        };
        // A hang detector, not a budget: the approval lands within the approver's next pass.
        let inside = wait_for_approval(
            &live.paths(),
            &source,
            Duration::from_secs(60),
            false,
            None,
            &|line| live.say(line),
            &|| Ok(()),
        )
        .unwrap();
        stop.store(true, Ordering::SeqCst);
        approver.join().unwrap();
        running.stop();
        assert!(matches!(inside, Some(JoinOutcome::Joined { .. })));
        assert!(
            !heard.lock().unwrap().iter().any(|line| line == REFUSED),
            "{heard:?}"
        );
    }

    /// A join password an admin Mac's approver refuses ends the join window at once, said as
    /// [`REFUSED`], so the wait ends at the verdict instead of the window's end.
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
        let created =
            aterm_secrets::client::test_create_with_password(&admin, &url, None, Some("admin"))
                .unwrap();
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
        let refused = {
            // This test's own server answers the approver's verdict post, which nothing
            // retries, within the one-minute hang detector (aterm_spec::wait::MUST_HAPPEN), not
            // the product's 10 s.
            let _answering =
                aterm_secrets::client::ServersAnswering::within(Duration::from_secs(60));
            aterm_secrets::client::auto_approve(&admin).unwrap()
        };
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
            let _ = ended.send(
                wait_for_approval(
                    &live.paths(),
                    &source,
                    Duration::from_secs(600),
                    false,
                    None,
                    &|line| live.say(line),
                    &|| Ok(()),
                )
                .map(|outcome| outcome.is_some()),
            );
        });
        let inside = waited
            .recv_timeout(Duration::from_secs(60))
            .expect("CONTROL: a window that reads no verdict runs its 600 s");
        running.stop();
        assert!(!inside.unwrap());
        assert!(heard.lock().unwrap().iter().any(|line| line == REFUSED));
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
    fn an_offline_default_join_keeps_bootstrap_and_its_subscriber_for_retry() {
        let sys = fake();
        sys.answer(None);
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        *ops.unreachable.lock().unwrap() =
            Some(Look::Unreached("alab-secrets.local: no such host".into()));
        let rows = run(&sys, &ops, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
        assert_eq!(
            rows[0].stood_aside.as_ref().map(StoodAside::exit_status),
            Some(69)
        );
        assert!(ops.joins().is_empty());
        assert_eq!(*ops.prepared.lock().unwrap(), 1);
        assert_eq!(*ops.native_tries.lock().unwrap(), 1);
        assert_eq!(*ops.reached.lock().unwrap(), 0);
        assert_eq!(ops.changes(), ["install"]);
    }

    #[test]
    fn an_offline_explicit_join_stands_aside_before_anything_is_made() {
        let sys = fake();
        sys.answer(None);
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        *ops.unreachable.lock().unwrap() =
            Some(Look::Unreached("vault.example: no such host".into()));
        let rows = run_with(
            &sys,
            &ops,
            Mode::Apply,
            Some("https://vault.example/v1/team"),
        );
        assert_eq!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
        assert_eq!(
            rows[0].stood_aside.as_ref().map(StoodAside::exit_status),
            Some(69)
        );
        assert!(ops.joins().is_empty());
        assert_eq!(*ops.prepared.lock().unwrap(), 0);
        assert_eq!(*ops.native_tries.lock().unwrap(), 0);
        assert_eq!(*ops.reached.lock().unwrap(), 1);
        assert_eq!(ops.installs(), 0);
        assert!(ops.changes().is_empty());
    }

    /// A join waiting to be let in on a Mac with Sync off says so and is no step and nothing to
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
                    row.to
                        .as_deref()
                        .is_some_and(|to| to.contains(SYNC_OFF)
                            && to.ends_with("the join lands once it is on")),
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

    #[test]
    fn swarm_enabled_false_marks_secrets_step_not_applicable_and_probes_nothing() {
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        ops.alab_answers();
        *ops.swarm.lock().unwrap() = Some(Ok(false));
        for mode in [Mode::Plan, Mode::Apply] {
            let row = run(&fake(), &ops, mode).remove(0);
            assert_eq!(row.outcome, Outcome::NotApplicable, "{mode:?}: {row:?}");
            assert_eq!(
                row.why.as_deref(),
                Some(aterm_secrets::swarm::SWARM_OFF),
                "{mode:?}: {row:?}"
            );
        }
        assert_eq!(
            ops.probes(),
            0,
            "no LAN probe while [swarm] enabled = false"
        );
        assert!(ops.changes().is_empty());
        // Negative control: sharing enabled admits native bootstrap and the answering vault join.
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        ops.joins_as(
            Some(configured(Membership::In { seq: 7, heard: 0 })),
            Ok(JoinOutcome::Joined {
                seq: 7,
                names: vec!["gh_access_token".into()],
            }),
        );
        ops.alab_answers();
        *ops.swarm.lock().unwrap() = Some(Ok(true));
        let sys = fake();
        sys.answer(None);
        let row = run(&sys, &ops, Mode::Apply).remove(0);
        assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
        assert_eq!(ops.probes(), 1);
        assert_eq!(*ops.prepared.lock().unwrap(), 1);
        assert_eq!(*ops.native_tries.lock().unwrap(), 1);
    }
}
