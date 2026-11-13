// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Native browser work and unknown token spending have different lock roles.
//! The legacy sign-in lock is shared here; no shared-to-exclusive upgrade occurs.
//! Refresh conservatively shares that cross-CLI gate before the exchange gate,
//! so a supported legacy signer excludes native refresh as well as browsers.
//! Source work precedes both gates; publication must never upgrade shared source
//! custody while holding exchange admission. Unmanaged OS consumers are outside
//! this cooperative contract. Legacy compatibility is confined to the actual
//! account-state root; native E and resource keys share the machine-home namespace.

use super::native_custody_platform as platform;
use super::{accounts, browser_window, codex_app_login, codex_enrollment, headless_oauth};
use std::fs::File;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[path = "native_enrollment_refresh_resources.rs"]
mod refresh_resources;
pub(super) use refresh_resources::RefreshResourceTurn;

const EXCHANGE: &str = "native-code-exchange.lock";

/// Managed refresh enters before account/owner/AuthLock custody, and retains
/// this guard until the response is durably settled. Unmanaged consumers are
/// deliberately outside this native cooperative exclusion contract.
pub(super) struct RefreshExchangeLease {
    legacy: Held,
    exchange: Held,
}
impl RefreshExchangeLease {
    pub(super) fn for_env(env: &super::cli::Env) -> Result<Self, String> {
        platform::require_available()?;
        let store = env
            .accounts_store_for(accounts::Cli::Codex)
            .ok_or("native exchange store unavailable")?;
        let namespace = super::codex_source_gate::existing_namespace_for_home(
            env.home
                .as_deref()
                .ok_or("native machine home unavailable")?,
        )
        .map_err(str::to_owned)?;
        let legacy = Held::open(&store.shared_dir(), accounts::SIGNIN_LOCK_FILE)?.shared()?;
        let exchange =
            Held::open(namespace.directory().map_err(str::to_owned)?, EXCHANGE)?.shared()?;
        Ok(Self { legacy, exchange })
    }
    pub(super) fn current(&self) -> Result<(), String> {
        self.legacy.current()?;
        self.exchange.current()
    }
}
impl std::fmt::Debug for RefreshExchangeLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeRefreshExchangeLease(redacted)")
    }
}

struct Held {
    file: File,
    path: PathBuf,
}
impl Held {
    #[cfg(unix)]
    fn open(directory: &Path, name: &str) -> Result<Self, String> {
        use aterm_uds::private_file;
        platform::ensure_private_dir(directory)
            .map_err(|_| "native admission directory unavailable")?;
        private_file::check_private_dir(directory)
            .map_err(|_| "native admission directory is not private")?;
        let file = match private_file::open_private_rw(directory, name) {
            Ok(file) => file,
            Err(private_file::PrivateFileError::Missing) => {
                // A concurrent creator is benign only if the normal private
                // opener accepts its exact file; every other failure refuses.
                let _ = private_file::write_private_new(directory, name, b"", |_| Ok(()));
                private_file::open_private_rw(directory, name)
                    .map_err(|_| "native admission file unavailable")?
            }
            Err(_) => return Err("native admission file unavailable".into()),
        };
        let held = Self {
            file,
            path: directory.join(name),
        };
        held.current()?;
        Ok(held)
    }
    #[cfg(not(unix))]
    fn open(_: &Path, _: &str) -> Result<Self, String> {
        Err("native credential custody is unsupported on this platform".into())
    }
    fn current(&self) -> Result<(), String> {
        if !astream_sys::file_matches_path(&self.file, &self.path)
            .map_err(|_| "native admission file unavailable")?
        {
            return Err("native admission file changed".into());
        }
        // Reopen through the same owner/mode/no-follow verifier: inode
        // equality alone must not overlook a permission change while held.
        let parent = self
            .path
            .parent()
            .ok_or("native admission parent missing")?;
        let name = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("native admission name unavailable")?;
        let checked = platform::open_private_rw(parent, name)
            .map_err(|_| "native admission permissions changed")?;
        if !astream_sys::file_matches_path(&checked, &self.path)
            .map_err(|_| "native admission file unavailable")?
        {
            return Err("native admission file changed".into());
        }
        self.matches_checked(&checked)?;
        platform::check_private_dir(
            self.path
                .parent()
                .ok_or("native admission parent missing")?,
        )
        .map_err(|_| "native admission parent changed".into())
    }
    fn matches_checked(&self, checked: &File) -> Result<(), String> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let original = self
                .file
                .metadata()
                .map_err(|_| "native admission metadata unavailable")?;
            let reopened = checked
                .metadata()
                .map_err(|_| "native admission metadata unavailable")?;
            if (original.dev(), original.ino()) != (reopened.dev(), reopened.ino()) {
                return Err("native admission file changed".into());
            }
            if original.nlink() != 1 || reopened.nlink() != 1 {
                return Err("native admission file has aliases".into());
            }
            Ok(())
        }
        #[cfg(not(unix))]
        {
            let _ = checked;
            Err("native descriptor identity is unavailable on this platform".into())
        }
    }
    fn shared(self) -> Result<Self, String> {
        self.try_shared()?
            .ok_or_else(|| "native admission is busy".into())
    }
    fn try_shared(self) -> Result<Option<Self>, String> {
        #[expect(
            clippy::disallowed_methods,
            reason = "a native phase retains shared kernel admission without upgrading this file"
        )]
        match self.file.try_lock_shared() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
            Err(std::fs::TryLockError::Error(_)) => {
                return Err("native admission unavailable".into());
            }
        }
        self.current()?;
        Ok(Some(self))
    }
    #[cfg(all(test, unix))]
    fn exclusive(self) -> Result<Self, String> {
        self.try_exclusive()?
            .ok_or_else(|| "native admission is busy".into())
    }
    fn try_exclusive(self) -> Result<Option<Self>, String> {
        #[expect(
            clippy::disallowed_methods,
            reason = "unknown native exchanges use a separate exclusive kernel gate"
        )]
        match self.file.try_lock() {
            Ok(()) => {
                self.current()?;
                Ok(Some(self))
            }
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(_)) => Err("native admission unavailable".into()),
        }
    }
}
impl Drop for Held {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// This owns physical browser resources, not an independent account identity.
/// Its exchange role is separate and is acquired only by the actual producer.
pub struct NativeBrowserTurn {
    request: codex_enrollment::Request,
    shared_dir: PathBuf,
    legacy: Held,
    profile: PathBuf,
    directory: File,
    claim: browser_window::ProfileLock,
    callback: TcpListener,
    callback_port: u16,
    source_home: std::sync::OnceLock<PathBuf>,
    exchange: Mutex<Option<CodeExchangeLease>>,
    publication: Mutex<Option<PublicationTurn>>,
}

impl std::fmt::Debug for NativeBrowserTurn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeBrowserTurn(redacted)")
    }
}

#[path = "native_enrollment_recovery.rs"]
mod recovery;
pub(super) use recovery::{NativeRecoveryResources, NativeRecoveryTurn};
#[path = "native_enrollment_retirement.rs"]
mod retirement;
#[path = "native_enrollment_retirement_gate.rs"]
mod retirement_gate;
pub(super) use retirement::NativeRetirementResources;

struct CodeExchangeLease {
    _source: super::codex_source_gate::SourceWorkLease,
    exchange: Held,
}
struct PublicationTurn {
    _source: super::codex_source_gate::PublicationLock,
    exchange: Held,
    resources: Vec<Held>,
    names: Vec<String>,
}
impl PublicationTurn {
    fn try_acquire(home: &Path, names: &[String]) -> Result<Option<Self>, String> {
        let Some(source) =
            super::codex_source_gate::PublicationLock::try_for_home(home).map_err(str::to_owned)?
        else {
            return Ok(None);
        };
        let namespace = source.namespace().map_err(str::to_owned)?;
        let directory = namespace.directory().map_err(str::to_owned)?;
        let Some(exchange) = Held::open(directory, EXCHANGE)?.try_shared()? else {
            return Ok(None);
        };
        let directory = directory.join("native-publication-resources");
        let mut resources = Vec::new();
        for name in names {
            let Some(resource) = Held::open(&directory, name)?.try_exclusive()? else {
                return Ok(None);
            };
            resources.push(resource);
        }
        Ok(Some(Self {
            _source: source,
            exchange,
            resources,
            names: names.to_vec(),
        }))
    }

    fn acquire_within(
        home: &Path,
        names: &[String],
        scope: &super::account_work::Scope,
        deadline: super::wake_deadline::WakeDeadline,
        current: impl Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        loop {
            if scope.cancelled() || deadline.passed() {
                return Err("native publication admission stopped".into());
            }
            current()?;
            if let Some(held) = Self::try_acquire(home, names)? {
                if scope.cancelled() || deadline.passed() {
                    return Err("native publication admission stopped".into());
                }
                return Ok(held);
            }
            // Every partial acquisition was dropped before this contention wait.
            std::thread::sleep(
                deadline
                    .remaining()
                    .min(std::time::Duration::from_millis(20)),
            );
        }
    }
}
impl NativeBrowserTurn {
    pub(super) fn bind_source_home(&self, home: &Path) -> Result<(), String> {
        let home = home
            .canonicalize()
            .map_err(|_| "native source home unavailable")?;
        if let Some(existing) = self.source_home.get() {
            if *existing != home {
                return Err("native source home changed".into());
            }
        } else if self.source_home.set(home.clone()).is_err()
            && self.source_home.get() != Some(&home)
        {
            return Err("native source home changed".into());
        }
        Ok(())
    }
    pub(super) fn response_durable(
        &self,
        proof: &super::codex_grant_owner::DurableExchangeSettlement,
    ) {
        if proof.operation() == self.request.id
            && let Ok(mut exchange) = self.exchange.lock()
        {
            *exchange = None;
        }
    }

    pub(super) fn acquire(
        store: &accounts::Store,
        request: &codex_enrollment::Request,
        row: &accounts::Row,
        now: i64,
    ) -> Result<Arc<Self>, String> {
        Self::acquire_on(store, request, row, now, &codex_app_login::LOOPBACK_PORTS)
    }

    /// Private-world fixtures must not reserve a person's real sign-in ports.
    #[cfg(all(unix, any(test, debug_assertions)))]
    pub(super) fn acquire_isolated_fixture(
        store: &accounts::Store,
        request: &codex_enrollment::Request,
        row: &accounts::Row,
        now: i64,
    ) -> Result<Arc<Self>, String> {
        Self::acquire_on(store, request, row, now, &[0])
    }

    fn acquire_on(
        store: &accounts::Store,
        request: &codex_enrollment::Request,
        row: &accounts::Row,
        now: i64,
        ports: &[u16],
    ) -> Result<Arc<Self>, String> {
        platform::require_available()?;
        let (logins, warning) = store
            .load_logins(now)
            .map_err(|_| "native admission records unavailable")?;
        if warning.is_some()
            || row.label != request.label
            || !logins.enrollment.requires_native_grant(request)?
        {
            return Err("native enrollment admission changed".into());
        }
        let shared_dir = store.shared_dir();
        let legacy = Held::open(&shared_dir, accounts::SIGNIN_LOCK_FILE)?.shared()?;
        let profile = headless_oauth::vendor_profile_dir(
            &shared_dir,
            &headless_oauth::vendor_label_key(accounts::Cli::Codex, &row.label),
            row.email.as_deref(),
        );
        platform::ensure_private_dir(&profile).map_err(|_| "native browser profile unavailable")?;
        let profile = profile
            .canonicalize()
            .map_err(|_| "native browser profile unavailable")?;
        let directory = astream_sys::open_nonblocking(&profile, astream_sys::Links::Refuse)
            .map_err(|_| "native browser profile unavailable")?;
        astream_sys::owner_private_dir(&directory)
            .map_err(|_| "native browser profile unavailable")?
            .map_err(|_| "native browser profile is not private")?;
        let claim = browser_window::ProfileLock::claim(&profile)
            .map_err(|_| "native browser profile is in use")?;
        let callback = codex_app_login::bind_callback_listener(
            ports,
            super::wake_deadline::WakeDeadline::after(super::login_drive::HANG),
        )
        .map_err(|_| "registered native callback ports are in use")?;
        let callback_port = callback
            .local_addr()
            .map_err(|_| "native callback unavailable")?
            .port();
        let result = Arc::new(Self {
            request: request.clone(),
            shared_dir,
            legacy,
            profile,
            directory,
            claim,
            callback,
            callback_port,
            source_home: std::sync::OnceLock::new(),
            exchange: Mutex::new(None),
            publication: Mutex::new(None),
        });
        result.check_preparation(store)?;
        Ok(result)
    }

    #[cfg(unix)]
    pub(super) fn check_profile(&self, profile: &Path) -> Result<(), String> {
        if profile.canonicalize().ok().as_deref() != Some(self.profile.as_path())
            || !astream_sys::file_matches_path(&self.directory, &self.profile).unwrap_or(false)
            || !self.claim.current(&self.profile)
        {
            return Err("native browser profile changed".into());
        }
        self.legacy.current()
    }

    pub(super) fn matches_request(&self, request: &codex_enrollment::Request) -> bool {
        self.request == *request
    }
    pub(super) fn names(&self, label: &str) -> bool {
        self.request.label == label
    }

    pub(super) fn check_preparation(&self, store: &accounts::Store) -> Result<(), String> {
        if self.shared_dir != store.shared_dir()
            || !astream_sys::file_matches_path(&self.directory, &self.profile)
                .map_err(|_| "native browser profile unavailable")?
            || !self.claim.current(&self.profile)
        {
            return Err("native enrollment resources changed".into());
        }
        self.legacy.current()?;
        self.callback_port()?;
        Ok(())
    }

    fn callback_port(&self) -> Result<u16, String> {
        let address = self
            .callback
            .local_addr()
            .map_err(|_| "native callback unavailable")?;
        if address.ip() != std::net::Ipv4Addr::LOCALHOST || address.port() != self.callback_port {
            return Err("native callback changed".into());
        }
        Ok(address.port())
    }

    pub(super) fn callback(&self) -> Result<TcpListener, String> {
        self.legacy.current()?;
        self.callback_port()?;
        self.callback
            .try_clone()
            .map_err(|_| "native callback transfer unavailable".into())
    }

    pub(super) fn enter_exchange(
        &self,
        deadline: super::wake_deadline::WakeDeadline,
        work: Option<&super::account_work::Scope>,
    ) -> Result<(), String> {
        loop {
            if deadline.passed() || work.is_some_and(super::account_work::Scope::cancelled) {
                return Err("native exchange admission stopped".into());
            }
            self.legacy.current()?;
            if self
                .exchange
                .lock()
                .map_err(|_| "native exchange state unavailable")?
                .is_some()
            {
                return self.check_exchange();
            }
            // Never wait for source work while holding exclusive exchange.
            let source = super::codex_source_gate::SourceWorkLease::for_home(
                self.source_home
                    .get()
                    .ok_or("native source home unavailable")?,
            )
            .map_err(str::to_owned)?;
            let namespace = source.namespace().map_err(str::to_owned)?;
            if let Some(exchange) =
                Held::open(namespace.directory().map_err(str::to_owned)?, EXCHANGE)?
                    .try_exclusive()?
            {
                let mut current = self
                    .exchange
                    .lock()
                    .map_err(|_| "native exchange state unavailable")?;
                if current.is_none() {
                    *current = Some(CodeExchangeLease {
                        _source: source,
                        exchange,
                    });
                }
                drop(current);
                return self.check_exchange();
            }
            drop(source);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    pub(super) fn admit_publication_within(
        &self,
        proof: &super::codex_grant_owner::PublicationIdentity,
        scope: &super::account_work::Scope,
        deadline: super::wake_deadline::WakeDeadline,
    ) -> Result<(), String> {
        if scope.cancelled() || deadline.passed() {
            return Err("native publication admission stopped".into());
        }
        if proof.operation() != self.request.id {
            return Err("native publication request changed".into());
        }
        if self
            .exchange
            .lock()
            .map_err(|_| "native exchange state unavailable")?
            .is_some()
        {
            return Err("native exchange response is not settled".into());
        }
        {
            let current = self
                .publication
                .lock()
                .map_err(|_| "native publication state unavailable")?;
            if let Some(held) = current.as_ref() {
                if held.names.as_slice() != proof.resources() {
                    return Err("native publication identity changed".into());
                }
                drop(current);
                return self.check_publication();
            }
        }
        let held = PublicationTurn::acquire_within(
            self.source_home
                .get()
                .ok_or("native source home unavailable")?,
            proof.resources(),
            scope,
            deadline,
            || {
                self.legacy.current()?;
                if self
                    .exchange
                    .lock()
                    .map_err(|_| "native exchange state unavailable")?
                    .is_some()
                {
                    return Err("native exchange response is not settled".into());
                }
                if self
                    .publication
                    .lock()
                    .map_err(|_| "native publication state unavailable")?
                    .is_some()
                {
                    return Err("native publication admission changed".into());
                }
                Ok(())
            },
        )?;
        let mut current = self
            .publication
            .lock()
            .map_err(|_| "native publication state unavailable")?;
        if scope.cancelled() || deadline.passed() {
            return Err("native publication admission stopped".into());
        }
        if current.is_some() {
            return Err("native publication admission changed".into());
        }
        *current = Some(held);
        drop(current);
        self.check_publication()
    }
    pub(super) fn check_publication(&self) -> Result<(), String> {
        self.legacy.current()?;
        let held = self
            .publication
            .lock()
            .map_err(|_| "native publication state unavailable")?;
        let held = held
            .as_ref()
            .ok_or("native publication has not been admitted")?;
        held._source.current().map_err(str::to_owned)?;
        held.exchange.current()?;
        for resource in &held.resources {
            resource.current()?;
        }
        Ok(())
    }
    pub(super) fn reject_publication(&self) {
        if let Ok(mut held) = self.publication.lock() {
            *held = None;
        }
    }
    pub(super) fn check_exchange(&self) -> Result<(), String> {
        self.legacy.current()?;
        let held = self
            .exchange
            .lock()
            .map_err(|_| "native exchange state unavailable")?;
        let held = held
            .as_ref()
            .ok_or("native exchange has not been admitted")?;
        held._source.current().map_err(str::to_owned)?;
        held.exchange.current()
    }
}

/// Registered callback endpoints are machine resources shared by all fixture
/// processes. Queue before acquiring browser resources; this grants no producer
/// or enrollment authority.
#[cfg(all(unix, any(test, debug_assertions)))]
pub(in crate::harness) struct CallbackTestTurn {
    _held: aterm_uds::private_file::PrivateLock,
}

#[cfg(all(unix, any(test, debug_assertions)))]
pub(in crate::harness) fn callback_test_turn() -> Result<CallbackTestTurn, String> {
    let directory = std::env::temp_dir().join("aterm-codex-callback-test-custody");
    callback_test_turn_at(&directory, || {})
}

#[cfg(all(unix, any(test, debug_assertions)))]
fn callback_test_turn_at(
    directory: &Path,
    contended: impl FnOnce(),
) -> Result<CallbackTestTurn, String> {
    platform::ensure_private_dir(directory)
        .map_err(|_| "registered callback test directory unavailable")?;
    // This queues test processes before they own browser resources. A busy
    // suite is not a failed enrollment: sleep until the previous fixture drops
    // its lock, without polling or imposing a deadline on another test.
    let held = aterm_uds::private_file::PrivateLock::wait_for(
        directory,
        "registered-callbacks.lock",
        contended,
    )
    .map_err(|_| "registered callback test custody unavailable")?;
    Ok(CallbackTestTurn { _held: held })
}

#[cfg(all(test, unix))]
#[path = "native_enrollment_admission_tests.rs"]
pub(super) mod tests;

#[cfg(all(test, not(unix)))]
#[path = "native_enrollment_admission_unsupported_tests.rs"]
mod unsupported_tests;
