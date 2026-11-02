// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! One session's catalog and native constructor policy. Production uses the shipping
//! roster and disables constructors; only an explicit debug Rust factory supplies fixtures.

use aterm_agent::harness::contract::{self, Row, Words};
#[cfg(all(unix, any(test, debug_assertions)))]
use aterm_agent::harness::native_act::Process;
use aterm_agent::harness::native_act::{Constructor, Image};
use aterm_agent::supervise::SupervisorConfig;

#[derive(Default)]
pub(crate) struct SessionPolicy {
    #[cfg(all(unix, any(test, debug_assertions)))]
    fixture: Option<FixturePolicy>,
    #[cfg(all(unix, any(test, debug_assertions)))]
    registered: std::sync::atomic::AtomicBool,
}

impl SessionPolicy {
    pub(crate) fn registered(&self) {
        #[cfg(all(unix, any(test, debug_assertions)))]
        self.registered.store(true, Ordering::Release);
    }

    pub(crate) fn rows(&self) -> &'static [Row] {
        #[cfg(all(unix, any(test, debug_assertions)))]
        if let Some(fixture) = &self.fixture {
            return fixture.rows;
        }
        contract::ROSTER
    }

    pub(crate) fn holder(&self, holder: &str) -> Option<(&'static Row, u32)> {
        contract::holder_in(self.rows(), holder)
    }

    pub(crate) fn read<'a>(&self, holder: &str, words: &'a [&'a str]) -> Words<'a> {
        contract::read_in(self.rows(), holder, words)
    }

    pub(crate) fn constructor_enabled(&self, kind: Constructor) -> bool {
        #[cfg(all(unix, any(test, debug_assertions)))]
        if let Some(fixture) = &self.fixture {
            return fixture.constructors.enabled(kind);
        }
        let _ = kind;
        false
    }

    /// A fixture's exact captured file identity, never a pathname-only permission.
    fn captured_image(&self, row: &Row, image: &Image) -> Option<bool> {
        #[cfg(all(unix, any(test, debug_assertions)))]
        if let Some(fixture) = &self.fixture {
            return Some(
                fixture
                    .images
                    .iter()
                    .any(|entry| entry.row == row.id && entry.images.contains(image)),
            );
        }
        let _ = (row, image);
        None
    }

    pub(crate) fn actor_image(&self, row: &Row, pid: u32) -> bool {
        #[cfg(all(unix, any(test, debug_assertions)))]
        if self.fixture.is_some() {
            return Process::observe(pid)
                .is_some_and(|actual| self.captured_image(row, &actual.image) == Some(true));
        }
        crate::harness_claim::runs_harness_build(row, pid)
    }

    pub(crate) fn target_image(&self, row: &Row, target: &Image) -> bool {
        if let Some(captured) = self.captured_image(row, target) {
            return captured;
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let path = std::path::Path::new(std::ffi::OsStr::from_bytes(&target.path));
            atpkg::store::resolve_configured()
                .is_some_and(|layout| crate::harness_claim::image_in(&layout, row, path))
        }
        #[cfg(not(unix))]
        false
    }

    pub(crate) fn config(&self) -> Option<&SupervisorConfig> {
        #[cfg(all(unix, any(test, debug_assertions)))]
        if let Some(fixture) = &self.fixture {
            return Some(&fixture.config);
        }
        None
    }
}

#[cfg(all(unix, any(test, debug_assertions)))]
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

/// Captured allowed images for one fixture catalog row. These identify files, not processes.
#[cfg(all(unix, any(test, debug_assertions)))]
pub struct FixtureImages {
    /// The exact catalog id.
    pub row: &'static str,
    /// Existing images captured by the fixture's host before registration.
    pub images: Vec<Image>,
}

/// Rust-only constructor control for one fixture session. Existing operations retain their
/// normal reconciliation paths; this mask is consulted only by fresh-admission checks.
#[cfg(all(unix, any(test, debug_assertions)))]
#[derive(Clone)]
pub struct FixtureConstructors(Arc<AtomicU8>);

#[cfg(all(unix, any(test, debug_assertions)))]
impl FixtureConstructors {
    fn bit(kind: Constructor) -> u8 {
        match kind {
            Constructor::OwnImage => 1,
            Constructor::PrivateRestart => 2,
        }
    }

    /// Set this fixture session's enabled kinds; there is no environment or wire setter.
    pub fn set_enabled(&self, kinds: &[Constructor]) {
        self.0.store(
            kinds.iter().fold(0, |bits, kind| bits | Self::bit(*kind)),
            Ordering::Release,
        );
    }

    fn enabled(&self, kind: Constructor) -> bool {
        self.0.load(Ordering::Acquire) & Self::bit(kind) != 0
    }
}

/// Explicit policy for a display-free private host fixture. The catalog and captured images
/// remain fixed through reconciliation, including after its constructor mask is disabled.
#[cfg(all(unix, any(test, debug_assertions)))]
pub struct FixturePolicy {
    rows: &'static [Row],
    images: Vec<FixtureImages>,
    config: Box<SupervisorConfig>,
    constructors: FixtureConstructors,
}

#[cfg(all(unix, any(test, debug_assertions)))]
impl FixturePolicy {
    /// Prepare a bounded fixture catalog. This grants no claim, kernel identity or operation.
    /// # Errors
    /// Refuses an empty or oversized catalog, duplicate ids, unmatched image rows, empty image
    /// sets, oversized sets or invalid image paths. No filesystem is changed.
    pub fn new(
        rows: &'static [Row],
        images: Vec<FixtureImages>,
        config: SupervisorConfig,
        kinds: &[Constructor],
    ) -> Result<Self, &'static str> {
        if rows.is_empty()
            || rows.len() > 4
            || images.len() != rows.len()
            || rows.iter().enumerate().any(|(at, row)| {
                row.id.is_empty()
                    || row.id.len() > 64
                    || !row
                        .id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
                    || rows[..at].iter().any(|old| old.id == row.id)
                    || images.iter().filter(|entry| entry.row == row.id).count() != 1
            })
            || images.iter().any(|entry| {
                entry.images.is_empty()
                    || entry.images.len() > 8
                    || entry.images.iter().any(|image| {
                        image.path.first() != Some(&b'/')
                            || image.path.len() > 4096
                            || image.path.contains(&0)
                    })
            })
        {
            return Err("invalid native fixture catalog or captured images");
        }
        let constructors = FixtureConstructors(Arc::new(AtomicU8::new(0)));
        constructors.set_enabled(kinds);
        Ok(Self {
            rows,
            images,
            config: Box::new(config),
            constructors,
        })
    }

    /// A control handle to this session's constructor mask, independent of its fixed catalog.
    #[must_use]
    pub fn constructors(&self) -> FixtureConstructors {
        self.constructors.clone()
    }

    pub(crate) fn enabled_constructors(&self) -> Vec<Constructor> {
        Constructor::ALL
            .into_iter()
            .filter(|kind| self.constructors.enabled(*kind))
            .collect()
    }
}

#[cfg(all(unix, any(test, debug_assertions)))]
impl crate::SessionCtx {
    /// Install only while the factory still has exclusive mutable access, before registration.
    /// # Errors
    /// A policy was already installed or this context was registered before. The refused
    /// policy is returned to its caller, including after exclusive ownership is regained.
    pub(crate) fn install_fixture_policy(
        &mut self,
        policy: FixturePolicy,
    ) -> Result<(), FixturePolicy> {
        if self.native_policy.fixture.is_some()
            || self.native_policy.registered.load(Ordering::Acquire)
        {
            return Err(policy);
        }
        self.native_policy.fixture = Some(policy);
        Ok(())
    }
}

#[cfg(all(test, unix))]
#[path = "native_session_policy_tests.rs"]
mod tests;
