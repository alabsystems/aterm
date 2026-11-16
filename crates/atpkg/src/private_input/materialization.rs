// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A fresh materialization namespace and its private Cargo cache are one owned
//! resource. Losing the host may release the kernel cache lock, but no later
//! attempt can adopt this namespace or cache. Source/SDK meaning, authenticated
//! submission, original GNU funding and canonical seed validation remain the
//! resident caller's separate obligations; this type enables no Bootstrap route.

use super::{InputUse, MaterializationWorkspace, Producer, Refused};
use std::{io, path::Path, time::Instant};

const MAX_INPUTS: usize = 8;

/// Same original fresh producer, input uses and private cache, retained together
/// before native dispatch. No path, observation or recovered record constructs
/// this value. Native completion does not validate its materialized contents.
#[must_use = "retain the original producer and private cache through native settlement"]
pub struct MaterializationAttempt {
    workspace: MaterializationWorkspace,
    inputs: Vec<InputUse>,
}

/// The SAME attempt recovered from its successful returning native job.
/// A status, path or producer cannot construct this completion capability.
#[must_use = "perform postchecks while retaining the original materialization guards"]
pub struct CompletedMaterialization {
    original: ReturnedMaterialization,
}

// A refused final bookend keeps the actual READY owner; it cannot recreate a
// BUILDING producer. This negative state is permanently quarantined, retains
// every original guard, and can neither retry finishing nor yield an input.
enum ReturnedMaterialization {
    Attempt(Box<MaterializationAttempt>),
    QuarantinedReady(Box<MaterializedInput>),
}

/// A postchecked private lifecycle with its original inputs, cache and endpoint.
/// This is not a canonical archive, validated seed, SDK or source credential.
#[must_use = "retain all original materialization guards beside the private input"]
pub struct MaterializedInput {
    workspace: super::workspace::ReadyWorkspace,
    inputs: Vec<InputUse>,
}

impl CompletedMaterialization {
    pub(crate) fn from_native_return(attempt: MaterializationAttempt) -> Self {
        Self {
            original: ReturnedMaterialization::Attempt(Box::new(attempt)),
        }
    }

    /// The original materialization location, not an independently admitted seed.
    #[must_use]
    pub fn path(&self) -> &Path {
        match &self.original {
            ReturnedMaterialization::Attempt(attempt) => attempt.path(),
            ReturnedMaterialization::QuarantinedReady(input) => input.path(),
        }
    }

    /// The SAME private cache that survived native cohort settlement.
    #[must_use]
    pub fn cargo_home(&self) -> &Path {
        match &self.original {
            ReturnedMaterialization::Attempt(attempt) => attempt.cargo_home(),
            ReturnedMaterialization::QuarantinedReady(input) => input.cargo_home(),
        }
    }

    /// Recheck the original native-returned producer and its input/cache guards.
    /// # Errors
    /// Original drift, revocation or endpoint expiry permanently quarantines
    /// this attempt; restoring a spelling cannot authorize a later finish.
    pub fn check_current(&self) -> io::Result<()> {
        match &self.original {
            ReturnedMaterialization::Attempt(attempt) => attempt.check_current(),
            ReturnedMaterialization::QuarantinedReady(input) => input.check_current(),
        }
    }

    /// Run semantic postchecks between original-guard bookends, then advance
    /// only this producer's private lifecycle. The caller's validator must
    /// establish any archive, source or seed meaning separately; this method
    /// grants none. No original input/cache guard or endpoint is replaced.
    ///
    /// # Errors
    /// Original guard drift, an expired endpoint, a failed postcheck or a
    /// failed durable lifecycle transition quarantines and returns the SAME
    /// completion with every original resource still held.
    pub fn finish_with(
        self,
        postcheck: impl FnOnce(&Self) -> io::Result<()>,
    ) -> Result<MaterializedInput, Refused<Self>> {
        self.finish_with_transition(postcheck, Producer::finish)
    }

    fn finish_with_transition(
        self,
        postcheck: impl FnOnce(&Self) -> io::Result<()>,
        transition: impl FnOnce(Producer) -> Result<super::Owner, Refused<Producer>>,
    ) -> Result<MaterializedInput, Refused<Self>> {
        let validation = (|| {
            self.check_current()?;
            postcheck(&self)?;
            self.check_current()
        })();
        if let Err(error) = validation {
            self.quarantine();
            return Err(Refused::new(error, self));
        }
        let attempt = match self.original {
            ReturnedMaterialization::Attempt(attempt) => attempt,
            ReturnedMaterialization::QuarantinedReady(input) => {
                input.quarantine();
                return Err(Refused::new(
                    io::ErrorKind::PermissionDenied.into(),
                    Self {
                        original: ReturnedMaterialization::QuarantinedReady(input),
                    },
                ));
            }
        };
        let MaterializationAttempt { workspace, inputs } = *attempt;
        match workspace.finish_with_transition(transition) {
            Ok(workspace) => {
                let input = MaterializedInput { workspace, inputs };
                match input.check_current() {
                    Ok(()) => Ok(input),
                    Err(error) => {
                        input.quarantine();
                        Err(Refused::new(
                            error,
                            Self {
                                original: ReturnedMaterialization::QuarantinedReady(Box::new(
                                    input,
                                )),
                            },
                        ))
                    }
                }
            }
            Err(refused) => {
                let (error, workspace) = refused.into_parts();
                let attempt = MaterializationAttempt { workspace, inputs };
                attempt.quarantine();
                Err(Refused::new(
                    error,
                    Self {
                        original: ReturnedMaterialization::Attempt(Box::new(attempt)),
                    },
                ))
            }
        }
    }

    fn quarantine(&self) {
        match &self.original {
            ReturnedMaterialization::Attempt(attempt) => attempt.quarantine(),
            ReturnedMaterialization::QuarantinedReady(input) => input.quarantine(),
        }
    }
}

impl MaterializedInput {
    pub(super) fn quarantine(&self) {
        self.workspace.quarantine();
    }

    /// Original private location, still subject to this held lifecycle.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.workspace.path()
    }

    /// Original cache; retaining this value retains the actual shared guard.
    #[must_use]
    pub fn cargo_home(&self) -> &Path {
        self.workspace.cargo_home()
    }

    /// Recheck this lifecycle and the SAME original inputs/cache/endpoint.
    /// # Errors
    /// A failed original-guard or lifecycle check refuses and quarantines this
    /// private input. Neither restored names nor later postchecks revive it.
    pub fn check_current(&self) -> io::Result<()> {
        let result = (|| {
            if self.inputs.is_empty() || self.inputs.len() > MAX_INPUTS {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "materialization requires 1..=8 original input uses",
                ));
            }
            self.workspace.check_current()?;
            for input in &self.inputs {
                input.check_current()?;
            }
            self.workspace.check_current()
        })();
        if result.is_err() {
            self.quarantine();
        }
        result
    }
}

impl MaterializationAttempt {
    /// Create a new namespace and an empty private Cargo home before dispatch.
    /// The input uses are actual lifetime guards, not source or SDK credentials.
    /// Failed creation retains any partial namespace, quarantines an established
    /// producer and returns the original input uses. Existing namespaces and
    /// caches are never adopted.
    ///
    /// # Errors
    /// Invalid/expired input guards, namespace creation, private-cache locking,
    /// or an original identity recheck fails. Every original input is returned.
    pub fn create(
        parent: &Path,
        name: &str,
        inputs: Vec<InputUse>,
        deadline: Instant,
    ) -> Result<Self, Refused<Vec<InputUse>>> {
        let prepare = || -> io::Result<MaterializationWorkspace> {
            if inputs.is_empty() || inputs.len() > MAX_INPUTS {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "materialization requires 1..=8 original input uses",
                ));
            }
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            for input in &inputs {
                input.check_current()?;
            }
            MaterializationWorkspace::create(parent, name, deadline)
        };
        let workspace = match prepare() {
            Ok(workspace) => workspace,
            Err(error) => return Err(Refused::new(error, inputs)),
        };
        let attempt = Self { workspace, inputs };
        if let Err(error) = attempt.check_current() {
            return Err(Refused::new(error, attempt.inputs));
        }
        Ok(attempt)
    }

    /// Fresh output location only; neither this path nor its bytes are a seed.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.workspace.path()
    }

    /// The cache owned by this same fresh producer, never an inherited cache.
    #[must_use]
    pub fn cargo_home(&self) -> &Path {
        self.workspace.cargo_home()
    }

    /// Check the original materializing namespace, input uses, cache lock and
    /// original endpoint. Failure leaves all resources in this same attempt.
    ///
    /// # Errors
    /// An original namespace/input/cache identity changed or the endpoint expired.
    pub fn check_current(&self) -> io::Result<()> {
        let current = (|| {
            if self.inputs.is_empty() || self.inputs.len() > MAX_INPUTS {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "materialization requires 1..=8 original input uses",
                ));
            }
            self.workspace.check_current()?;
            for input in &self.inputs {
                input.check_current()?;
            }
            self.workspace.check_current()
        })();
        if current.is_err() {
            self.quarantine();
        }
        current
    }

    /// Permanently revoke this same attempt after an input or producer
    /// postcheck fails, retaining every actual input/cache guard in this bundle.
    /// This stops no child and supplies no native settlement or guard release.
    pub fn quarantine(&self) {
        self.workspace.quarantine();
    }
}

#[cfg(test)]
#[path = "materialization_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "materialization_completion_tests.rs"]
mod completion_tests;
