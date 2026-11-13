// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Cooperative controller lifetime, independent of nested compiler capacity reservations.

#[cfg(target_os = "macos")]
#[path = "build_admit_controller_until.rs"]
mod until;
#[cfg(target_os = "macos")]
pub use until::{ControllerDeadline, supervise_controller_until};

/// Root status and cancellation after the original native group is conclusively drained.
#[derive(Debug)]
pub struct ControllerOutcome {
    /// The original controller's owning wait status.
    pub root: std::process::ExitStatus,
    /// Last signal observed by the native forwarding front, even during group settlement.
    pub signal: Option<i32>,
}

/// Run a headless cooperative controller in a dedicated native cohort. Return only after
/// the original group is absent in its captured machine/boot/namespace. A background member
/// keeps the caller's borrowed resources live after root exit; Unknown keeps waiting too.
/// Members deliberately leaving that group are outside this contract. This is no additional
/// capacity reservation: compiler fronts inside the controller retain ordinary admission.
///
/// # Errors
/// Unsupported host, forwarding/birth/spawn failure, or the owned root wait. An unavailable
/// native cohort observation is retained rather than converted into completion. No signal
/// targets a numeric pid after its owning wait consumed it.
pub fn supervise_controller(
    command: &mut aterm_uds::spawnfd::Command,
    say: &mut dyn FnMut(&str),
) -> std::io::Result<ControllerOutcome> {
    #[cfg(unix)]
    {
        supervise_unix(command, say)
    }
    #[cfg(not(unix))]
    {
        let _ = (command, say);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "native controller cohort lifetime is unavailable on this host",
        ))
    }
}

#[cfg(unix)]
fn supervise_unix(
    command: &mut aterm_uds::spawnfd::Command,
    say: &mut dyn FnMut(&str),
) -> std::io::Result<ControllerOutcome> {
    use aterm_capacity::cohort::{Cohort, Lifetime};
    let front = super::front::Front::arm_cohort().ok_or_else(|| {
        std::io::Error::other("native controller signal forwarding could not be armed")
    })?;
    let attempt = aterm_uds::spawnfd::BirthAttempt::new()?;
    let (mut spawned, observation_error) = match command.spawn_cohort(attempt) {
        Ok(spawned) => (spawned, None),
        Err(aterm_uds::spawnfd::SpawnFailure::Unbound { spawned, error }) => {
            (*spawned, Some(error))
        }
        Err(aterm_uds::spawnfd::SpawnFailure::NotSpawned(proof)) => {
            front.never();
            return Err(proof.into_error());
        }
        Err(aterm_uds::spawnfd::SpawnFailure::Refused(error)) => {
            say(&format!(
                "controller: retaining resources without an authoritative no-child receipt ({error})"
            ));
            loop {
                pause();
            }
        }
    };
    let mut reported = false;
    let cohort = loop {
        match Cohort::of_spawned(&mut spawned) {
            Ok(cohort) => break cohort,
            Err(error) => {
                if !reported {
                    say(&format!(
                        "controller: retaining resources while native cohort custody is unavailable ({error})"
                    ));
                    reported = true;
                }
                pause();
            }
        }
    };
    let child = spawned.child_mut();
    let status = if child.is_reaped() {
        front.reaped();
        child.wait()
    } else {
        front.aim(child.id(), None);
        front.reap(child)
    };
    // Front::reap disarms its target before the owning reap. Keep handlers installed while
    // settlement waits so a later cancellation is recorded, never aimed at the old pid.
    front.reaped();
    let mut reported = false;
    let mut observed = None;
    loop {
        if let Some(signal) = front.observed()
            && observed != Some(signal)
        {
            say(&format!(
                "controller: cancellation {signal} observed during native group settlement"
            ));
            observed = Some(signal);
        }
        match cohort.lifetime() {
            Lifetime::Drained => break,
            lifetime => {
                if !reported {
                    say(&format!(
                        "controller: root ended; retaining resources until its native group drains ({lifetime:?})"
                    ));
                    reported = true;
                }
                pause();
            }
        }
    }
    let signal = front.disarm_observed();
    match (observation_error, status) {
        (Some(observation), Err(wait)) => Err(std::io::Error::new(
            observation.kind(),
            ControllerObservationAndWaitError { observation, wait },
        )),
        (Some(error), Ok(_)) => Err(error),
        (None, status) => status.map(|root| ControllerOutcome { root, signal }),
    }
}

#[cfg(unix)]
fn pause() {
    // Native group absence has no waitable descriptor. Keep borrowed resources during
    // paced kernel lifetime queries.
    std::thread::sleep(std::time::Duration::from_millis(50));
}

/// Both original native diagnostics, returned only after the captured cohort drained.
#[cfg(unix)]
#[derive(Debug)]
struct ControllerObservationAndWaitError {
    observation: std::io::Error,
    wait: std::io::Error,
}

#[cfg(unix)]
impl std::fmt::Display for ControllerObservationAndWaitError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}; original child wait also failed: {}",
            self.observation, self.wait
        )
    }
}

#[cfg(unix)]
impl std::error::Error for ControllerObservationAndWaitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.observation)
    }
}
