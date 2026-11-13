// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A durable request to bootstrap the pinned default vault, without claiming membership.

use super::*;

pub(super) const FILE: &str = "alab-join-pending";
const INTENT: &[u8] = b"ALAB-JOIN-1\n";
const NATIVE_ONLY: &[u8] = b"ALAB-JOIN-NATIVE-1\n";

fn intent(paths: &Paths) -> crate::Result<Option<bool>> {
    fsx::read_private(&paths.dir.join(FILE), NATIVE_ONLY.len())?
        .map(|bytes| match bytes.as_slice() {
            INTENT => Ok(false),
            NATIVE_ONLY => Ok(true),
            _ => Err(Error::new("unrecognized ALab bootstrap intent")),
        })
        .transpose()
}

pub(super) fn pending(paths: &Paths) -> bool {
    matches!(config(paths), Ok(None)) && intent(paths).is_ok_and(|intent| intent.is_some())
}

/// A native member proof's result. Only a verified log bundle grants membership;
/// an unanswered possession exchange remains ambiguous and must not queue another enrollment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeAdmission {
    /// No member admitted this Mac or may have signed an admission.
    None,
    /// A verified log bundle was committed locally. Materialization is read separately.
    Admitted,
    /// A member may have signed, or another process holds the proof lock.
    Uncertain,
}

/// The executable's native member transport, absent in library-only users. `current` is a
/// read-only authorization check; a proof rechecks it before publication under the vault lock.
pub type NativeBootstrap =
    fn(&Paths, &(dyn Fn() -> crate::Result<()> + Sync)) -> crate::Result<NativeAdmission>;

static NATIVE_BOOTSTRAP: std::sync::OnceLock<NativeBootstrap> = std::sync::OnceLock::new();
/// The executable's local setup-policy read; an unreadable policy holds background work.
pub type BootstrapAllowed = fn(&Paths) -> crate::Result<bool>;
static BOOTSTRAP_ALLOWED: std::sync::OnceLock<BootstrapAllowed> = std::sync::OnceLock::new();

/// Install the executable's read-only setup authority check for background bootstrap.
/// A later Stop, off choice or selected source holds continuation after a restart too.
pub fn on_bootstrap_allowed(allowed: BootstrapAllowed) {
    let _ = BOOTSTRAP_ALLOWED.set(allowed);
}

/// Check the current executable's background setup choices, without acquiring the vault lock.
/// # Errors
/// Setup was stopped, disabled, replaced, or could not be read.
pub fn check_bootstrap_allowed(paths: &Paths) -> crate::Result<()> {
    if crate::service::subscriber_turned_off(&paths.home) {
        return Err(Error::new(
            "the vault subscriber was turned off; bootstrap is held",
        ));
    }
    if BOOTSTRAP_ALLOWED
        .get()
        .is_some_and(|allowed| !allowed(paths).unwrap_or(false))
    {
        return Err(Error::new(
            "automatic vault bootstrap is held by setup's current choices",
        ));
    }
    Ok(())
}

pub(super) fn background_native(paths: &Paths) -> crate::Result<NativeAdmission> {
    native_bootstrap(paths, &|| {
        check_bootstrap_allowed(paths)?;
        if config(paths)?.is_some() {
            return Err(Error::new(
                "the vault source changed while native bootstrap ran",
            ));
        }
        Ok(())
    })
}

/// Install the executable's native-only, compiled-authority bootstrap transport once.
pub fn on_native_bootstrap(bootstrap: NativeBootstrap) {
    let _ = NATIVE_BOOTSTRAP.set(bootstrap);
}

/// Try native admission without vault HTTP or Bonjour discovery. The callback must admit
/// only the compiled default authority and commit only over an exactly unconfigured source.
/// # Errors
/// Sharing or the caller's authorization changed, this identity is isolated, or proof failed.
pub fn native_bootstrap(
    paths: &Paths,
    current: &(dyn Fn() -> crate::Result<()> + Sync),
) -> crate::Result<NativeAdmission> {
    remember_native(paths, native_attempt(paths, current)?)
}

fn native_attempt(
    paths: &Paths,
    current: &(dyn Fn() -> crate::Result<()> + Sync),
) -> crate::Result<NativeAdmission> {
    current()?;
    let Some(bootstrap) = NATIVE_BOOTSTRAP.get() else {
        return Ok(NativeAdmission::None);
    };
    refuse_lab_identity(paths)?;
    if !crate::swarm::enabled_in_home(&paths.home)? {
        return Err(Error::new("sharing is off; native bootstrap was held"));
    }
    bootstrap(paths, current)
}

fn remember_native(paths: &Paths, outcome: NativeAdmission) -> crate::Result<NativeAdmission> {
    let _lock = StateLock::take(paths)?;
    if config(paths)?.is_some() {
        return Ok(outcome);
    }
    let previous = intent(paths)?;
    if outcome == NativeAdmission::Uncertain && previous.is_some() {
        fsx::write_atomic(&paths.dir.join(FILE), NATIVE_ONLY, 0o600)?;
    }
    Ok(
        if (previous == Some(true) || crate::members::join::pending_proof(paths)?)
            && outcome == NativeAdmission::None
        {
            NativeAdmission::Uncertain
        } else {
            outcome
        },
    )
}

#[cfg(test)]
type Attempt = Box<dyn Fn(&Paths, ExpectedConfig<'static>) -> crate::Result<()>>;
#[cfg(test)]
thread_local! {
    static ATTEMPT: std::cell::RefCell<Option<Attempt>> = const { std::cell::RefCell::new(None) };
}

/// Keep the default ALab join retryable before its first authenticated fetch.
/// This writes no source, identity, membership or credentials. An existing source wins.
///
/// # Errors
/// Sharing is disabled, the slot is reserved for creation, or private state cannot be written.
pub fn prepare_alab_join_retry(paths: &Paths) -> crate::Result<()> {
    if !crate::swarm::enabled_in_home(&paths.home)? {
        return Err(Error::new("sharing is off; ALab bootstrap was not started"));
    }
    let _lock = StateLock::take(paths)?;
    if config(paths)?.is_some() {
        return Ok(());
    }
    creation::permit_join(paths)?;
    if intent(paths)?.is_some() {
        return Ok(());
    }
    fsx::write_atomic(&paths.dir.join(FILE), INTENT, 0o600)
}

pub(super) fn resume(paths: &Paths) -> crate::Result<()> {
    #[cfg(test)]
    if let Some(result) = ATTEMPT.with(|attempt| {
        attempt
            .borrow()
            .as_ref()
            .map(|join| resume_with(paths, join))
    }) {
        return result;
    }
    resume_with(paths, |paths, expected| {
        join_live(
            paths,
            &JoinOptions {
                source: String::new(),
                host: None,
                interactive: false,
                wait: Duration::ZERO,
                confirm: &|_| false,
                password: JoinPassword::Given(None),
                fetched: None,
                requested: &|| {},
                say: &|_| {},
            },
            expected,
        )
        .map(drop)
    })
}

fn resume_with(
    paths: &Paths,
    join: impl FnOnce(&Paths, ExpectedConfig<'static>) -> crate::Result<()>,
) -> crate::Result<()> {
    resume_attempt(paths, join, native_attempt)
}

/// Resume a pending bootstrap of the default vault, the first step of every subscriber pass: by
/// native proof (`native`), else by the ordinary join (`join`).
///
/// Only a pending bootstrap reads [`check_bootstrap_allowed`]: an intent, no vault configured and
/// sharing on. The gate holds bootstrap work, never a pass over a vault this Mac already has, so
/// a Mac with no intent (every configured member) returns at once and takes no lock, and a
/// configured Mac's stale intent is wiped under the lock whatever the gate says. Configured Macs
/// the gate holds are ordinary: a founder made with `--no-agent` keeps a subscriber-off record,
/// and setup's choices hold automatic bootstrap where they hold Secrets or plan a vault other
/// than the default.
///
/// The look without the lock ends the pass's part here only on a clean absence. Whatever else it
/// reads, a wipe's zeroed bytes in flight among them, is read again under the lock.
///
/// # Errors
/// The intent cannot be read, the gate holds a pending bootstrap, or the bootstrap failed.
fn resume_attempt(
    paths: &Paths,
    join: impl FnOnce(&Paths, ExpectedConfig<'static>) -> crate::Result<()>,
    native: impl FnOnce(
        &Paths,
        &(dyn Fn() -> crate::Result<()> + Sync),
    ) -> crate::Result<NativeAdmission>,
) -> crate::Result<()> {
    if matches!(intent(paths), Ok(None)) {
        return Ok(());
    }
    {
        let _lock = StateLock::take(paths)?;
        if intent(paths)?.is_none() {
            return Ok(());
        }
        if config(paths)?.is_some() {
            return fsx::wipe_and_remove(&paths.dir.join(FILE));
        }
        if !crate::swarm::enabled_in_home(&paths.home)? {
            return Ok(());
        }
    }
    check_bootstrap_allowed(paths)?;
    let current = || {
        check_bootstrap_allowed(paths)?;
        if config(paths)?.is_some() {
            return Err(Error::new(
                "the vault source changed while native bootstrap ran",
            ));
        }
        if !crate::swarm::enabled_in_home(&paths.home)? {
            return Err(Error::new("sharing is off; native bootstrap was held"));
        }
        Ok(())
    };
    match remember_native(paths, native(paths, &current)?)? {
        NativeAdmission::Admitted => {
            let _lock = StateLock::take(paths)?;
            if config(paths)?.is_some() {
                fsx::wipe_and_remove(&paths.dir.join(FILE))?;
            }
            return Ok(());
        }
        NativeAdmission::Uncertain => {
            return Err(Error::new(
                "native admission is not confirmed; the subscriber retries the same proof",
            ));
        }
        NativeAdmission::None => {}
    }
    current()?;
    // The ordinary join verifies the pinned signature before configuring anything. Its mode
    // preserves another source chosen during the fetch and rechecks sharing before committing
    // the fetched configuration, applying it or posting a request. Work already in flight may
    // finish; a committed configuration remains for the subscriber when sharing resumes.
    let joined = join(paths, ExpectedConfig::WhileSharing(None));
    let _lock = StateLock::take(paths)?;
    if config(paths)?.is_some() {
        fsx::wipe_and_remove(&paths.dir.join(FILE))?;
    }
    joined
}

#[cfg(test)]
#[path = "alab_join_tests.rs"]
mod tests;
