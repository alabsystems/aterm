// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A fresh materialization namespace and its private Cargo cache are one owned
//! resource. Losing the host may release the kernel cache lock, but no later
//! attempt can adopt this namespace or cache. Source/SDK meaning, authenticated
//! submission, original GNU funding and canonical seed validation remain the
//! resident caller's separate obligations; this type enables no Bootstrap route.

use super::{InputUse, MaterializationWorkspace, Refused};
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
