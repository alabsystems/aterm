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

/// What an unconfigured Mac's row says where another vault answered at the ALab vault's address:
/// there is no ALab vault on this network, so nothing is asked.
pub const NOT_HERE: &str = "no secrets vault on this network; aterm secrets join <url> or aterm init --vault <url> joins yours";

/// What an unconfigured Mac's row says where nothing answered at the ALab vault's address (a
/// timeout, a refused connect, no route, a name that did not resolve, no LAN broker yet): that is
/// no proof there is none, so it names no remedy, and nothing is asked.
pub const UNANSWERED: &str = "the ALab vault did not answer on this network, which is no proof \
                              there is none; aterm looks again by itself";

/// How the vault item's stand-aside begins when the vault's address answered with no vault there
/// ([`Look::NoVault`]). The stand-aside's kind, [`StoodAside::Absent`], is what tells that answer
/// from one nobody gave ([`StoodAside::Offline`]); these words only say it.
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

/// The kind of stand-aside a look at the ALab vault's address that found none is
/// ([`alab_unavailable`]), made with the words its row says.
type LookKind = fn(String) -> StoodAside;

/// Why the fallback probe found no ALab vault, after native member admission was attempted, and
/// the kind of stand-aside it is: an answer that none is there ([`StoodAside::Absent`]: a server
/// at its address that holds none, [`NO_VAULT_NOW`], or another vault, [`NOT_HERE`]), or no
/// answer at all ([`StoodAside::Offline`], [`UNANSWERED`]), which proves nothing. Every reader of
/// the look reads that kind, never these words.
fn alab_unavailable(ops: &dyn SecretsOps) -> Option<(LookKind, &'static str)> {
    match ops.probe_alab() {
        Ok(ProbeOutcome::Found(_)) => None,
        Ok(ProbeOutcome::Absent | ProbeOutcome::NotThisVault(_)) => {
            Some((StoodAside::Absent, NO_VAULT_NOW))
        }
        Ok(ProbeOutcome::AnotherVault(_)) => Some((StoodAside::Absent, NOT_HERE)),
        Err(_) => Some((StoodAside::Offline, UNANSWERED)),
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

/// Whether `id` names the vault's own row ([`item_id`]), for a row that records it waits on it
/// ([`crate::model::ItemReport::waits_on`]).
#[must_use]
pub(crate) fn is_vault_row(id: &str) -> bool {
    id.starts_with(ITEM)
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
            if let Some((kind, why)) = alab_unavailable(self.ops.as_ref()) {
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
                *aside = Some(kind(why));
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
            Look::NoVault(why) => Some(StoodAside::Absent(format!("{NO_VAULT_NOW}: {why}"))),
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
#[path = "init_secrets_tests.rs"]
pub(crate) mod tests;
