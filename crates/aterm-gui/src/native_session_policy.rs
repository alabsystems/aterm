// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! One session's catalog and native constructor policy. Production uses the shipping
//! roster and disables generic constructors. Sealed cold relaunch, qualified asked upgrade and Codex renewal are
//! limited to Apple Silicon macOS; only an explicit debug Rust factory supplies fixtures.

use aterm_agent::harness::contract::{self, Row, Words};
#[cfg(all(unix, any(test, debug_assertions)))]
use aterm_agent::harness::native_act::Process;
use aterm_agent::harness::native_act::{Constructor, Image};
use aterm_agent::supervise::SupervisorConfig;

pub(crate) const fn cold_restart_qualified() -> bool {
    cfg!(all(target_os = "macos", target_arch = "aarch64"))
}

#[derive(Default)]
pub(crate) struct SessionPolicy {
    #[cfg(all(unix, any(test, debug_assertions)))]
    fixture: Option<FixturePolicy>,
    #[cfg(all(unix, any(test, debug_assertions)))]
    registered: std::sync::atomic::AtomicBool,
}

impl SessionPolicy {
    /// This call site follows the actual successful native SIGTERM syscall;
    /// recorded/replayed RPC acknowledgements never emit another effect.
    #[cfg(all(unix, any(test, debug_assertions)))]
    pub(crate) fn renewal_signal_effect(&self, process: &Process) {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let Some(root) = self
            .fixture
            .as_ref()
            .and_then(|fixture| fixture.renewal_fault_root.as_ref())
        else {
            return;
        };
        let event = aterm_json::json!({"schema":1,"kind":"native-signal-effect",
            "pid":process.pid,"birth":process.birth,"group":process.group,"signal":libc::SIGTERM,
            "monotonicUs":aterm_agent::harness::native_act::monotonic_us()});
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(root.join("native-signal-effects.jsonl"))
            .expect("fixture actual signal effect trace");
        let mut bytes = aterm_json::to_vec(&event).expect("fixture actual signal effect encoding");
        bytes.push(b'\n');
        file.write_all(&bytes)
            .expect("fixture actual signal effect append");
    }

    /// Completed fixture observations contain only explicitly constructed fields.
    #[cfg(all(unix, any(test, debug_assertions)))]
    pub(crate) fn renewal_control_observed(&self, event: &aterm_json::Value) {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let Some(root) = self
            .fixture
            .as_ref()
            .and_then(|fixture| fixture.renewal_fault_root.as_ref())
        else {
            return;
        };
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(root.join("renewal-control-trace.jsonl"))
            .expect("fixture completed control trace");
        let mut bytes = aterm_json::to_vec(event).expect("fixture completed trace encoding");
        bytes.push(b'\n');
        file.write_all(&bytes)
            .expect("fixture completed trace append");
    }

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
        // ColdRestart requires built-in Relaunch authority and owned-helper resolution.
        // Asked constructors independently require settlement and current duty authority;
        // AskedRelaunch additionally needs the checked producer's scoped authorization.
        // Discovery supplies none of those admission proofs.
        matches!(
            kind,
            Constructor::ColdRestart | Constructor::AskedUpgrade | Constructor::AskedRelaunch
        ) && cold_restart_qualified()
    }

    /// Qualification gate for the sealed same-account service. Generic native
    /// constructor availability remains independent and unchanged.
    pub(crate) fn renewal_constructor_enabled(&self) -> bool {
        #[cfg(all(unix, any(test, debug_assertions)))]
        if let Some(fixture) = &self.fixture {
            return fixture.constructors.enabled(Constructor::PrivateRestart);
        }
        // This sealed service's native host/provider pair is qualified on Apple Silicon macOS.
        // Generic constructors remain independently disabled above.
        cfg!(all(target_os = "macos", target_arch = "aarch64"))
    }

    pub(crate) fn quota_rotation_enabled(&self) -> bool {
        #[cfg(all(unix, any(test, debug_assertions)))]
        if let Some(fixture) = &self.fixture {
            return fixture.quota_rotation
                && fixture.constructors.enabled(Constructor::PrivateRestart);
        }
        // Account selection still requires fresh, identity-bound weekly usage.
        aterm_agent::harness::codex_rotation::QUOTA_ROTATION_QUALIFIED
    }

    pub(crate) fn quota_fixture_roots(&self) -> Option<Vec<Vec<u8>>> {
        #[cfg(all(unix, debug_assertions))]
        if let Some(fixture) = &self.fixture {
            return fixture.quota_roots.clone();
        }
        None
    }

    pub(crate) fn renewal_before_signal(&self) {
        #[cfg(all(unix, any(test, debug_assertions)))]
        if let Some(root) = self
            .fixture
            .as_ref()
            .and_then(|fixture| fixture.renewal_fault_root.as_ref())
        {
            if root.join("pause-before-signal").exists() {
                aterm_uds::atomic_file::write(
                    &root.join("before-signal.reached"),
                    b"prepared\n",
                    0o600,
                )
                .expect("fixture before Signal checkpoint");
                let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
                while !root.join("before-signal.continue").exists() {
                    assert!(
                        std::time::Instant::now() < until,
                        "fixture before Signal hang detector"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
            if std::fs::rename(
                root.join("panic-before-signal"),
                root.join("panic-before-signal-fired"),
            )
            .is_ok()
            {
                panic!("native renewal fixture before Signal");
            }
        }
    }

    /// An explicit fixture records the actual coordinator exit, independently
    /// of a later observation-triggered native terminal reconciliation.
    pub(crate) fn renewal_coordinator_stopped(&self, id: &str, deadline_us: u64) {
        #[cfg(all(unix, any(test, debug_assertions)))]
        if let Some(root) = self
            .fixture
            .as_ref()
            .and_then(|fixture| fixture.renewal_fault_root.as_ref())
        {
            let event = aterm_json::json!({"operation":id,"deadlineUs":deadline_us,
                "monotonicUs":aterm_agent::harness::native_act::monotonic_us()});
            let bytes = aterm_json::to_vec(&event).expect("fixture coordinator exit encoding");
            aterm_uds::atomic_file::write(
                &root.join(format!("renewal-worker-{id}-stopped.json")),
                &bytes,
                0o600,
            )
            .expect("fixture coordinator exit record");
        }
        let _ = (id, deadline_us);
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
            Constructor::ColdRestart => 4,
            Constructor::AskedUpgrade => 8,
            Constructor::AskedRelaunch => 16,
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
    renewal_fault_root: Option<std::path::PathBuf>,
    quota_rotation: bool,
    quota_roots: Option<Vec<Vec<u8>>>,
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
            renewal_fault_root: None,
            quota_rotation: false,
            quota_roots: None,
        })
    }

    /// An explicit Rust fixture owns this fault directory; installed policy
    /// never reads a path or environment switch for fault injection.
    #[must_use]
    pub fn with_renewal_fault_root(mut self, root: std::path::PathBuf) -> Self {
        self.renewal_fault_root = Some(root);
        self
    }

    /// Enable quota discovery in this isolated Rust fixture. Its private restart
    /// constructor must also be enabled before the fixture can rotate.
    #[must_use]
    pub fn with_quota_rotation(mut self) -> Self {
        self.quota_rotation = true;
        self
    }

    /// Pin the isolated HTTPS usage server's roots for this Rust-only fixture.
    #[must_use]
    pub fn with_quota_roots(mut self, roots: Vec<Vec<u8>>) -> Self {
        self.quota_roots = Some(roots);
        self
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
