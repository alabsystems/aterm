// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A fresh materialization namespace and its private Cargo cache are one owned
//! resource. Losing the host may release the kernel cache lock, but no later
//! attempt can adopt this namespace or cache. Source/SDK meaning, authenticated
//! submission, original GNU funding and canonical seed validation remain the
//! resident caller's separate obligations; this type enables no Bootstrap route.

use super::{InputUse, PreparationInputs, Producer, Refused};
use crate::cargo_cache::SourceCacheGuard;
use std::{
    io,
    os::unix::fs::DirBuilderExt as _,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

/// Same original fresh producer, input uses and private cache, retained together
/// before native dispatch. No path, observation or recovered record constructs
/// this value. Native completion does not validate its materialized contents.
#[must_use = "retain the original producer and private cache through native settlement"]
pub struct MaterializationAttempt {
    producer: Producer,
    inputs: PreparationInputs,
    cargo_home: PathBuf,
    revoked: AtomicBool,
}

impl MaterializationAttempt {
    /// Create a new namespace and an empty private Cargo home before dispatch.
    /// The input uses are actual lifetime guards, not source or SDK credentials.
    /// Failed partial creation leaves its namespace quarantined and returns the
    /// original input uses. Existing namespaces and caches are never adopted.
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
        let prepare = || -> io::Result<(Producer, SourceCacheGuard, PathBuf)> {
            if inputs.is_empty() || inputs.len() > 8 {
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
            let producer = Producer::create(parent, name)?;
            let cargo_home = producer.path().join("cargo-home");
            std::fs::DirBuilder::new().mode(0o700).create(&cargo_home)?;
            let cache = SourceCacheGuard::try_acquire(&cargo_home)?;
            producer.check_current()?;
            Ok((producer, cache, cargo_home))
        };
        let (producer, cache, cargo_home) = match prepare() {
            Ok(prepared) => prepared,
            Err(error) => return Err(Refused::new(error, inputs)),
        };
        let inputs = match PreparationInputs::new(inputs, cache, deadline) {
            Ok(inputs) => inputs,
            Err(refused) => {
                let (error, (inputs, _cache)) = refused.into_parts();
                return Err(Refused::new(error, inputs));
            }
        };
        Ok(Self {
            producer,
            inputs,
            cargo_home,
            revoked: AtomicBool::new(false),
        })
    }

    /// Fresh output location only; neither this path nor its bytes are a seed.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.producer.path()
    }

    /// The cache owned by this same fresh producer, never an inherited cache.
    #[must_use]
    pub fn cargo_home(&self) -> &Path {
        &self.cargo_home
    }

    /// Check the original materializing namespace, input uses, cache lock and
    /// original endpoint. Failure leaves all resources in this same attempt.
    ///
    /// # Errors
    /// An original namespace/input/cache identity changed or the endpoint expired.
    pub fn check_current(&self) -> io::Result<()> {
        if self.revoked.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "original materialization attempt is quarantined",
            ));
        }
        let current = (|| {
            self.producer.check_current()?;
            self.inputs.check_current()?;
            self.producer.check_current()
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
        self.revoked.store(true, Ordering::Release);
        self.producer
            .inner
            .as_ref()
            .expect("same original materialization producer")
            .quarantine();
    }
}

#[cfg(test)]
#[path = "materialization_tests.rs"]
mod tests;
