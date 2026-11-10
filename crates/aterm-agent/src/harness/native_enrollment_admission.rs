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
    _held: Held,
}

#[cfg(all(unix, any(test, debug_assertions)))]
pub(in crate::harness) fn callback_test_turn() -> Result<CallbackTestTurn, String> {
    let directory = std::env::temp_dir().join("aterm-codex-callback-test-custody");
    let held = Held::open(&directory, "registered-callbacks.lock")?;
    aterm_uds::file_lock::lock_exclusive_until(
        &held.file,
        std::time::Instant::now() + std::time::Duration::from_secs(60),
        "registered callback test custody did not retire",
    )
    .map_err(|_| "registered callback test custody unavailable")?;
    held.current()?;
    Ok(CallbackTestTurn { _held: held })
}

#[cfg(all(test, unix))]
pub(super) mod tests {
    use super::*;

    pub(in crate::harness) fn environment(
        root: &Path,
        home: &Path,
        name: &str,
    ) -> super::super::cli::Env {
        let config = root.join(format!("{name}.toml"));
        std::fs::write(&config, "").unwrap();
        super::super::cli::Env {
            state: root.join(name),
            aterm_state: Some(home.join(".aterm")),
            cwd: home.into(),
            home: Some(home.into()),
            claude_dir: None,
            claude_config_unusable: None,
            claude_profile: None,
            claude_custom_oauth: false,
            claude_session: None,
            claude_pid: None,
            codex_thread: None,
            codex_home: None,
            codex_login: None,
            agy_conversation: None,
            now: 1_800_000_000,
            sid: "s-native-admission-fixture".into(),
            sock: None,
            config: Some(config),
            claude_tmp: None,
            claude_running: None,
            tz_s: 0,
        }
    }
    pub(in crate::harness) fn reservation(
        env: &super::super::cli::Env,
        label: &str,
        operation: &str,
    ) -> Arc<NativeBrowserTurn> {
        reservation_using(env, label, operation, false)
    }
    /// Explicit historical phase fixture: no global catalogue authority.
    pub(in crate::harness) fn historical_reservation(
        env: &super::super::cli::Env,
        label: &str,
        operation: &str,
    ) -> Arc<NativeBrowserTurn> {
        reservation_using(env, label, operation, true)
    }
    fn reservation_using(
        env: &super::super::cli::Env,
        label: &str,
        operation: &str,
        historical: bool,
    ) -> Arc<NativeBrowserTurn> {
        let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
        let request = codex_enrollment::Request {
            id: operation.repeat(32),
            caller: "d".repeat(64),
            label: label.into(),
            revision: "e".repeat(64),
        };
        store
            .transact_logins(env.now, &mut None, |_, logins| {
                if historical {
                    logins
                        .enrollment
                        .admit_historical_native_fixture(&request)?;
                } else {
                    logins.enrollment.admit_native(&request)?;
                }
                Ok(Vec::new())
            })
            .unwrap();
        let roster = accounts::Roster::parse_for(
            accounts::Cli::Codex,
            &format!(
                "[[account]]\nlabel='{label}'\nclass='dedicated'\ndir='{}'\n",
                env.state.join("login").display()
            ),
        )
        .unwrap();
        let turn =
            NativeBrowserTurn::acquire_isolated_fixture(&store, &request, &roster.rows[0], env.now)
                .unwrap();
        turn.bind_source_home(env.home.as_deref().unwrap()).unwrap();
        turn
    }
    pub(in crate::harness) fn callback_test_turn() -> super::CallbackTestTurn {
        super::callback_test_turn().unwrap()
    }

    #[test]
    fn native_exchange_is_shared_across_state_roots_in_one_machine_home() {
        let _callback_turn = callback_test_turn();
        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let other_home = root.path().join("other-home");
        platform::ensure_private_dir(&home).unwrap();
        platform::ensure_private_dir(&other_home).unwrap();
        let a = environment(root.path(), &home, "state-a");
        let b = environment(root.path(), &home, "state-b");
        let other = environment(root.path(), &other_home, "state-c");
        assert_ne!(a.config, b.config);
        assert_ne!(
            a.accounts_store_for(accounts::Cli::Codex)
                .unwrap()
                .shared_dir(),
            b.accounts_store_for(accounts::Cli::Codex)
                .unwrap()
                .shared_dir()
        );
        let first = reservation(&a, "first", "a");
        let second = reservation(&b, "second", "b");
        first
            .enter_exchange(
                super::super::wake_deadline::WakeDeadline::after(std::time::Duration::from_secs(1)),
                None,
            )
            .unwrap();
        // An existing-source inspection does not reacquire shared S, even when
        // the existing holder is exclusive. It is metadata, not source authority.
        let other_source =
            super::super::codex_source_gate::PublicationLock::for_home(&other_home).unwrap();
        let isolated = RefreshExchangeLease::for_env(&other).unwrap();
        isolated.current().unwrap();
        assert!(RefreshExchangeLease::for_env(&a).is_err());
        assert!(RefreshExchangeLease::for_env(&b).is_err());
        assert!(
            second
                .enter_exchange(
                    super::super::wake_deadline::WakeDeadline::after(
                        std::time::Duration::from_millis(10)
                    ),
                    None
                )
                .is_err()
        );
        first.check_exchange().unwrap();
        drop(first);
        second
            .enter_exchange(
                super::super::wake_deadline::WakeDeadline::after(std::time::Duration::from_secs(1)),
                None,
            )
            .unwrap();
        assert!(RefreshExchangeLease::for_env(&a).is_err());
        drop(second);
        let shared_source =
            super::super::codex_source_gate::SourceWorkLease::for_home(&home).unwrap();
        let refresh_a = RefreshExchangeLease::for_env(&a).unwrap();
        let refresh_b = RefreshExchangeLease::for_env(&b).unwrap();
        refresh_a.current().unwrap();
        refresh_b.current().unwrap();
        assert!(
            a.accounts_store_for(accounts::Cli::Codex)
                .unwrap()
                .signin_now()
                .unwrap()
                .is_none()
        );
        assert!(
            b.accounts_store_for(accounts::Cli::Codex)
                .unwrap()
                .signin_now()
                .unwrap()
                .is_none()
        );
        let mut missing_home = a.clone();
        missing_home.home = None;
        assert!(RefreshExchangeLease::for_env(&missing_home).is_err());
        drop((refresh_a, refresh_b, shared_source, isolated, other_source));
    }

    #[test]
    fn actual_native_factory_owns_two_registered_callbacks_and_preserves_legacy_exclusion() {
        let _callback_turn = callback_test_turn();
        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        platform::ensure_private_dir(&home).unwrap();
        let store =
            accounts::Store::at_for(&root.path().join("state"), accounts::Cli::Codex, &home);
        let roster = accounts::Roster::parse_for(accounts::Cli::Codex, &format!(
            "[[account]]\nlabel='a'\nclass='dedicated'\ndir='{}'\n[[account]]\nlabel='b'\nclass='dedicated'\ndir='{}'\n[[account]]\nlabel='c'\nclass='dedicated'\ndir='{}'\n",
            root.path().join("login-a").display(), root.path().join("login-b").display(), root.path().join("login-c").display()
        )).unwrap();
        let requests: Vec<_> = ["a", "b", "c"]
            .into_iter()
            .map(|label| codex_enrollment::Request {
                id: label.repeat(32),
                caller: "d".repeat(64),
                label: label.into(),
                revision: "e".repeat(64),
            })
            .collect();
        for request in &requests {
            store
                .transact_logins(1_800_000_000, &mut None, |_, logins| {
                    logins.enrollment.admit_native(request)?;
                    Ok(Vec::new())
                })
                .unwrap();
        }
        // These are the ACTUAL production constructor and the two registered
        // callback endpoints. No browser or provider request is started.
        let a = NativeBrowserTurn::acquire(&store, &requests[0], &roster.rows[0], 1_800_000_000)
            .unwrap();
        let b = NativeBrowserTurn::acquire(&store, &requests[1], &roster.rows[1], 1_800_000_000)
            .unwrap();
        assert_ne!(a.profile, b.profile);
        let callback = a.callback().unwrap();
        assert_ne!(
            callback.local_addr().unwrap(),
            b.callback().unwrap().local_addr().unwrap()
        );
        assert!(store.signin_now().unwrap().is_none());
        assert!(
            NativeBrowserTurn::acquire(&store, &requests[2], &roster.rows[2], 1_800_000_000)
                .is_err()
        );
        drop(a);
        // The callback worker's real duplicated listener outlives its original
        // constructor, so its endpoint remains unavailable until that worker retires.
        assert!(
            NativeBrowserTurn::acquire(&store, &requests[2], &roster.rows[2], 1_800_000_000)
                .is_err()
        );
        drop(callback);
        let c = NativeBrowserTurn::acquire(&store, &requests[2], &roster.rows[2], 1_800_000_000)
            .unwrap();
        assert!(store.signin_now().unwrap().is_none());
        drop((b, c));
        let legacy = store.signin_now().unwrap().unwrap();
        assert!(
            NativeBrowserTurn::acquire(&store, &requests[0], &roster.rows[0], 1_800_000_000)
                .is_err()
        );
        drop(legacy);
        NativeBrowserTurn::acquire(&store, &requests[0], &roster.rows[0], 1_800_000_000).unwrap();
    }

    fn transition(state: &aterm_spec::interp::State, action: &str) -> aterm_spec::interp::State {
        let root = aterm_tempfile::tempdir().unwrap();
        let directory = root.path().join("private");
        let open = |name| Held::open(&directory, name).unwrap();
        let mut legacy =
            (state["legacy"] == 1).then(|| open(accounts::SIGNIN_LOCK_FILE).exclusive().unwrap());
        let mut a =
            (state["browser_a"] == 1).then(|| open(accounts::SIGNIN_LOCK_FILE).shared().unwrap());
        let mut b =
            (state["browser_b"] == 1).then(|| open(accounts::SIGNIN_LOCK_FILE).shared().unwrap());
        let mut post = (state["post"] == 1).then(|| open(EXCHANGE).exclusive().unwrap());
        let mut ra = (state["refresh_a"] == 1).then(|| open(EXCHANGE).shared().unwrap());
        let mut rb = (state["refresh_b"] == 1).then(|| open(EXCHANGE).shared().unwrap());
        match action {
            "BrowserA" => a = open(accounts::SIGNIN_LOCK_FILE).shared().ok(),
            "BrowserB" => b = open(accounts::SIGNIN_LOCK_FILE).shared().ok(),
            "Legacy" => legacy = open(accounts::SIGNIN_LOCK_FILE).exclusive().ok(),
            "Post" => post = open(EXCHANGE).exclusive().ok(),
            "RefreshA" => ra = open(EXCHANGE).shared().ok(),
            "RefreshB" => rb = open(EXCHANGE).shared().ok(),
            "EndBrowserA" => drop(a.take()),
            "EndBrowserB" => drop(b.take()),
            "EndLegacy" => drop(legacy.take()),
            "EndPost" => drop(post.take()),
            "EndRefreshA" => drop(ra.take()),
            "EndRefreshB" => drop(rb.take()),
            _ => unreachable!(),
        }
        let mut next = state.clone();
        for (name, held) in [
            ("browser_a", a.is_some()),
            ("browser_b", b.is_some()),
            ("legacy", legacy.is_some()),
            ("post", post.is_some()),
            ("refresh_a", ra.is_some()),
            ("refresh_b", rb.is_some()),
        ] {
            next.insert(name, i64::from(held));
        }
        next
    }

    #[test]
    fn actual_kernel_roles_refine_the_native_signin_phase_model() {
        let model = aterm_models_harness::native_signin_phase_model();
        assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
        let states = aterm_spec::interp::reachable_states(&model);
        assert!(
            states
                .iter()
                .any(|state| state["browser_a"] == 1 && state["browser_b"] == 1)
        );
        assert!(
            states
                .iter()
                .any(|state| state["refresh_a"] == 1 && state["refresh_b"] == 1)
        );
        for state in states {
            for action in &model.actions {
                if model.action_enabled(action.name, &state) {
                    let next = transition(&state, action.name);
                    assert!(
                        model.successors(action.name, &state).contains(&next),
                        "{}: {next:?}",
                        action.name
                    );
                }
            }
        }
        let mut bad = model.init_state();
        bad.insert("post", 1);
        bad.insert("browser_a", 1);
        bad.insert("refresh_a", 1);
        assert!(!model.check_invariant("UnknownPostExclusive", &bad));
        bad = model.init_state();
        bad.insert("legacy", 1);
        bad.insert("browser_a", 1);
        assert!(!model.check_invariant("LegacyExclusive", &bad));
    }

    #[test]
    fn publication_waits_for_source_without_spending_another_exchange() {
        // Kernel admission only: this fixture grants no credential authority.
        let home = aterm_tempfile::tempdir().unwrap();
        let held_source =
            super::super::codex_source_gate::SourceWorkLease::for_home(home.path()).unwrap();
        let lifetime = super::super::account_work::Lifetime::default();
        let scope = lifetime.enter().unwrap();
        let path = home.path().to_path_buf();
        let (entered, waiting) = std::sync::mpsc::channel();
        let (done, result) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let once = std::sync::atomic::AtomicBool::new(false);
            done.send(PublicationTurn::acquire_within(
                &path,
                &["principal-fixture".into()],
                &scope,
                super::super::wake_deadline::WakeDeadline::after(std::time::Duration::from_secs(
                    60,
                )),
                || {
                    if !once.swap(true, std::sync::atomic::Ordering::SeqCst) {
                        entered.send(()).unwrap();
                    }
                    Ok(())
                },
            ))
            .unwrap();
        });
        waiting
            .recv_timeout(std::time::Duration::from_secs(60))
            .unwrap();
        assert!(matches!(
            result.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        drop(held_source);
        let publication = result
            .recv_timeout(std::time::Duration::from_secs(60))
            .unwrap()
            .unwrap();
        publication._source.current().unwrap();
        worker.join().unwrap();
        drop(publication);
    }

    #[test]
    fn cancelled_publication_releases_partial_resources_and_unavailable_is_not_busy() {
        let home = aterm_tempfile::tempdir().unwrap();
        let source =
            super::super::codex_source_gate::SourceWorkLease::for_home(home.path()).unwrap();
        let directory = source
            .namespace()
            .unwrap()
            .directory()
            .unwrap()
            .to_path_buf();
        drop(source);
        let resources = directory.join("native-publication-resources");
        let blocked = Held::open(&resources, "b").unwrap().exclusive().unwrap();
        let lifetime = super::super::account_work::Lifetime::default();
        let scope = lifetime.enter().unwrap();
        let cancel = scope.cancellation();
        let path = home.path().to_path_buf();
        let (entered, waiting) = std::sync::mpsc::channel();
        let (done, result) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let once = std::sync::atomic::AtomicBool::new(false);
            done.send(PublicationTurn::acquire_within(
                &path,
                &["a".into(), "b".into()],
                &scope,
                super::super::wake_deadline::WakeDeadline::after(std::time::Duration::from_secs(
                    60,
                )),
                || {
                    if !once.swap(true, std::sync::atomic::Ordering::SeqCst) {
                        entered.send(()).unwrap();
                    }
                    Ok(())
                },
            ))
            .unwrap();
        });
        waiting
            .recv_timeout(std::time::Duration::from_secs(60))
            .unwrap();
        cancel.cancel();
        assert!(
            result
                .recv_timeout(std::time::Duration::from_secs(60))
                .unwrap()
                .is_err()
        );
        worker.join().unwrap();
        let source =
            super::super::codex_source_gate::SourceWorkLease::for_home(home.path()).unwrap();
        let exchange = Held::open(&directory, EXCHANGE)
            .unwrap()
            .exclusive()
            .unwrap();
        let first = Held::open(&resources, "a").unwrap().exclusive().unwrap();
        drop((source, exchange, first, blocked));
        let scope = lifetime.enter().unwrap();
        let attempts = std::cell::Cell::new(0);
        assert!(
            PublicationTurn::acquire_within(
                Path::new("relative-native-home"),
                &[],
                &scope,
                super::super::wake_deadline::WakeDeadline::after(std::time::Duration::from_secs(
                    60
                )),
                || {
                    attempts.set(attempts.get() + 1);
                    Ok(())
                },
            )
            .is_err()
        );
        assert_eq!(attempts.get(), 1, "unavailable namespace must not retry");
        assert!(
            PublicationTurn::acquire_within(
                home.path(),
                &[],
                &scope,
                super::super::wake_deadline::WakeDeadline::after(std::time::Duration::ZERO),
                || panic!("expired admission must not try kernel custody"),
            )
            .is_err()
        );
    }

    #[test]
    fn browser_overlap_is_separate_from_legacy_and_unknown_post_exclusion() {
        let root = aterm_tempfile::tempdir().unwrap();
        let directory = root.path().join("private");
        let a = Held::open(&directory, accounts::SIGNIN_LOCK_FILE)
            .unwrap()
            .shared()
            .unwrap();
        let b = Held::open(&directory, accounts::SIGNIN_LOCK_FILE)
            .unwrap()
            .shared()
            .unwrap();
        assert!(
            Held::open(&directory, accounts::SIGNIN_LOCK_FILE)
                .unwrap()
                .exclusive()
                .is_err()
        );
        let post = Held::open(&directory, EXCHANGE)
            .unwrap()
            .exclusive()
            .unwrap();
        assert!(
            Held::open(&directory, EXCHANGE)
                .unwrap()
                .exclusive()
                .is_err()
        );
        assert!(Held::open(&directory, EXCHANGE).unwrap().shared().is_err());
        a.current().unwrap();
        b.current().unwrap();
        drop(post);
        let refresh_a = Held::open(&directory, EXCHANGE).unwrap().shared().unwrap();
        let refresh_b = Held::open(&directory, EXCHANGE).unwrap().shared().unwrap();
        assert!(
            Held::open(&directory, EXCHANGE)
                .unwrap()
                .exclusive()
                .is_err()
        );
        drop((refresh_a, refresh_b, a, b));
        let legacy = Held::open(&directory, accounts::SIGNIN_LOCK_FILE)
            .unwrap()
            .exclusive()
            .unwrap();
        assert!(
            Held::open(&directory, accounts::SIGNIN_LOCK_FILE)
                .unwrap()
                .shared()
                .is_err()
        );
        drop(legacy);
        Held::open(&directory, accounts::SIGNIN_LOCK_FILE)
            .unwrap()
            .shared()
            .unwrap();
    }

    #[test]
    fn actual_profile_and_callback_resources_remain_exclusive_until_retirement() {
        let root = aterm_tempfile::tempdir().unwrap();
        let a = root.path().join("a");
        let b = root.path().join("b");
        platform::ensure_private_dir(&a).unwrap();
        platform::ensure_private_dir(&b).unwrap();
        let profile_a = browser_window::ProfileLock::claim(&a).unwrap();
        let profile_b = browser_window::ProfileLock::claim(&b).unwrap();
        assert!(browser_window::ProfileLock::claim(&a).is_err());
        assert!(profile_a.current(&a));
        assert!(profile_b.current(&b));
        // Kernel ownership and transfer are tested without consuming the real
        // machine's registered callback endpoints. Live acquire uses ONLY them.
        let callback = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = callback.local_addr().unwrap();
        let worker = callback.try_clone().unwrap();
        drop(callback);
        assert!(TcpListener::bind(address).is_err());
        drop(worker);
        TcpListener::bind(address).unwrap();
        drop(profile_a);
        browser_window::ProfileLock::claim(&a).unwrap();
    }

    #[test]
    fn reopened_permission_check_cannot_rebind_a_held_gate_during_rename() {
        let root = aterm_tempfile::tempdir().unwrap();
        let directory = root.path().join("private");
        let original = Held::open(&directory, EXCHANGE).unwrap().shared().unwrap();
        // Exact race cut: the initial held-FD/path check succeeds; then the
        // old single-link file is renamed before the permission reopen.
        assert!(astream_sys::file_matches_path(&original.file, &original.path).unwrap());
        std::fs::rename(&original.path, directory.join("old-gate")).unwrap();
        let replacement = Held::open(&directory, EXCHANGE).unwrap().shared().unwrap();
        let checked = aterm_uds::private_file::open_private_rw(&directory, EXCHANGE).unwrap();
        assert!(astream_sys::file_matches_path(&checked, &original.path).unwrap());
        assert!(
            original.matches_checked(&checked).is_err(),
            "both path checks succeeding cannot substitute a different descriptor"
        );
        assert!(original.current().is_err());
        replacement.matches_checked(&checked).unwrap();
        replacement.current().unwrap();
    }

    #[test]
    fn replacement_and_aliases_invalidate_existing_exchange_admission() {
        let root = aterm_tempfile::tempdir().unwrap();
        let directory = root.path().join("private");
        let held = Held::open(&directory, EXCHANGE).unwrap().shared().unwrap();
        std::fs::rename(directory.join(EXCHANGE), directory.join("old")).unwrap();
        let replacement = Held::open(&directory, EXCHANGE).unwrap().shared().unwrap();
        assert!(held.current().is_err());
        replacement.current().unwrap();
        #[cfg(unix)]
        {
            std::fs::hard_link(directory.join(EXCHANGE), directory.join("alias")).unwrap();
            assert!(replacement.current().is_err());
            assert!(Held::open(&directory, EXCHANGE).is_err());
        }
    }
}

#[cfg(all(test, not(unix)))]
mod unsupported_tests {
    use super::*;

    #[test]
    fn unsupported_native_admission_does_not_create_a_lock_namespace() {
        let world = aterm_tempfile::tempdir().unwrap();
        let directory = world.path().join("native-admission");
        assert!(Held::open(&directory, EXCHANGE).is_err());
        assert!(!directory.exists());
    }
}
