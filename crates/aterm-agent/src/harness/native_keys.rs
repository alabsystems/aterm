// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Closed native API-key inputs and current credential-access evidence. These
//! types grant no account identity or permission to execute a model request.

use aterm_http::{Client, Method, ProxyMode, Trust};
use aterm_json::Value;
use aterm_uds::secret::Secret;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const RESPONSE_MAX: usize = 256 * 1024;
/// Hang detector for a required native preparation and its credential-access checks.
pub const CHECK_BUDGET: Duration = Duration::from_secs(60);

pub mod policy;
mod schedule;
pub use schedule::PreparationQueue;

/// Public scheduling identity. It grants no authority or credential access.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparationId {
    pub key: String,
    pub revision: [u8; 32],
}

/// Work bound to the exact public identity admitted by the scheduler. Its
/// closure reopens current authority before credential or provider access.
pub struct PreparationTask {
    pub preparation: PreparationId,
    pub run: Box<dyn FnOnce() -> Result<(), String> + Send>,
}

type Discover = dyn Fn(
        &Path,
        &Path,
        &Path,
        &mut dyn FnMut(&PreparationId) -> bool,
    ) -> Result<Option<PreparationTask>, String>
    + Send
    + Sync;
type Launcher = dyn Fn(Provider, &Path) -> Result<Option<PathBuf>, String> + Send + Sync;
static DISCOVER: OnceLock<Box<Discover>> = OnceLock::new();
static LAUNCHER: OnceLock<Box<Launcher>> = OnceLock::new();

/// Installed only by the executable which can validate the root application.
pub fn install(
    discover: impl Fn(
        &Path,
        &Path,
        &Path,
        &mut dyn FnMut(&PreparationId) -> bool,
    ) -> Result<Option<PreparationTask>, String>
    + Send
    + Sync
    + 'static,
    launcher: impl Fn(Provider, &Path) -> Result<Option<PathBuf>, String> + Send + Sync + 'static,
) {
    let _ = DISCOVER.set(Box::new(discover));
    let _ = LAUNCHER.set(Box::new(launcher));
}

pub(crate) fn preparation(
    home: &Path,
    state: &Path,
    config: &Path,
    admit: &mut dyn FnMut(&PreparationId) -> bool,
) -> Result<Option<PreparationTask>, String> {
    if !preparation_enabled(config)? {
        return Ok(None);
    }
    #[cfg(test)]
    if let Some(result) = TEST_DISCOVERY.with(|test| {
        test.borrow()
            .as_ref()
            .map(|discover| discover(home, state, config, admit))
    }) {
        return result;
    }
    DISCOVER
        .get()
        .map_or(Ok(None), |discover| discover(home, state, config, admit))
}

#[cfg(test)]
thread_local! {
    static TEST_DISCOVERY: std::cell::RefCell<Option<Box<Discover>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn with_test_discovery<T>(discover: Box<Discover>, run: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_DISCOVERY.with(|test| test.borrow_mut().take());
        }
    }
    TEST_DISCOVERY.with(|test| assert!(test.borrow_mut().replace(discover).is_none()));
    let _reset = Reset;
    run()
}

/// Observe the local switches for native credential preparation. A missing
/// config retains the ordinary defaults; a failed or invalid read is unknown.
///
/// # Errors
/// Returns an error when the selected configuration cannot be read or parsed
/// without diagnostics. This observation grants no credential or root authority.
pub fn preparation_enabled(config: &Path) -> Result<bool, String> {
    let (settings, issues) = crate::supervise::config::SupervisorConfig::from_path(Some(config));
    if !issues.is_empty() {
        return Err("native preparation configuration is unavailable or invalid".into());
    }
    Ok(settings.enabled && settings.accounts != super::accounts::Mode::Off && settings.logins)
}

/// A path-only wrapper. Keys never enter a shell line or a launch record.
pub struct Launch {
    pub program: PathBuf,
    pub args: Vec<String>,
}

pub fn launch(
    provider: Provider,
    program: &Path,
    args: &[String],
    env: &[(&str, &str)],
    config: Option<&Path>,
) -> Result<Option<Launch>, String> {
    let Some(config) = config else {
        return Ok(None);
    };
    if !config.is_absolute() {
        return Err("native policy path must be absolute".into());
    }
    if !default_route(provider, args) || env.iter().any(|(name, _)| explicit_env(provider, name)) {
        return Ok(None);
    }
    if !preparation_enabled(config)? {
        return Ok(None);
    }
    let Some(wrapper) = selected_wrapper(provider, config)? else {
        return Ok(None);
    };
    let program = program.to_str().ok_or("native program path is not UTF-8")?;
    let config = config
        .to_str()
        .filter(|_| config.is_absolute())
        .ok_or("native policy path must be absolute UTF-8")?;
    let mut wrapped = vec![
        "harness".into(),
        "accounts".into(),
        "native-run".into(),
        provider.name().into(),
        program.into(),
        "--config".into(),
        config.into(),
        "--".into(),
    ];
    wrapped.extend_from_slice(args);
    Ok(Some(Launch {
        program: wrapper,
        args: wrapped,
    }))
}

fn selected_wrapper(provider: Provider, config: &Path) -> Result<Option<PathBuf>, String> {
    #[cfg(test)]
    if let Some(result) = TEST_LAUNCHER.with(|test| {
        test.borrow()
            .as_ref()
            .map(|launcher| launcher(provider, config))
    }) {
        return result;
    }
    LAUNCHER
        .get()
        .map_or(Ok(None), |launcher| launcher(provider, config))
}

#[cfg(test)]
thread_local! {
    static TEST_LAUNCHER: std::cell::RefCell<Option<Box<Launcher>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn with_test_launcher<T>(
    launcher: impl Fn(Provider, &Path) -> Result<Option<PathBuf>, String> + Send + Sync + 'static,
    run: impl FnOnce() -> T,
) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_LAUNCHER.with(|test| test.borrow_mut().take());
        }
    }
    TEST_LAUNCHER.with(|test| assert!(test.borrow_mut().replace(Box::new(launcher)).is_none()));
    let _reset = Reset;
    run()
}

pub fn default_route(provider: Provider, args: &[String]) -> bool {
    match provider {
        Provider::Google => super::upgrade_agy::shared_key_defaults(args),
        Provider::Meta => super::upgrade_muse::shared_key_defaults(args),
    }
}

pub fn explicit_env(provider: Provider, name: &str) -> bool {
    match provider {
        Provider::Google => {
            name.starts_with("GOOGLE_")
                || name.starts_with("GEMINI_")
                || name.starts_with("AGY_")
                || name.starts_with("ANTIGRAVITY_")
        }
        Provider::Meta => {
            name.starts_with("META_")
                || name.starts_with("TBH_")
                || name.starts_with("MUSE_")
                || matches!(name, "XDG_DATA_HOME" | "XDG_CONFIG_HOME")
        }
    }
}

/// Capability only. Missing input and an unsupported login method remain
/// separate from credential access or permission to run a model.
pub fn capability(provider: &str, method: Option<&str>, has_reference: bool) -> Value {
    match Provider::of(provider) {
        Some(target) => aterm_json::json!({
            "target":target.connector(),"api_key_adapter":true,"oauth_adapter":false,
            "state":match method { None=>"auth-method-missing",Some("api-key") if !has_reference=>"credential-reference-missing",Some("api-key")=>"preparation-supported",_=>"method-unsupported" },
            "credential_access":"not-proved","execution":"not-granted"
        }),
        None => Value::Null,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    Google,
    Meta,
}

impl Provider {
    pub fn of(value: &str) -> Option<Self> {
        match value {
            "google" => Some(Self::Google),
            "meta" => Some(Self::Meta),
            _ => None,
        }
    }

    pub fn for_connector(value: &str) -> Option<Self> {
        match value {
            "agy" => Some(Self::Google),
            "muse" => Some(Self::Meta),
            _ => None,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Google => "google",
            Self::Meta => "meta",
        }
    }

    pub const fn connector(self) -> &'static str {
        match self {
            Self::Google => "agy",
            Self::Meta => "muse",
        }
    }

    pub const fn key_env(self) -> &'static str {
        match self {
            Self::Google => "GEMINI_API_KEY",
            Self::Meta => "META_API_KEY",
        }
    }

    fn endpoint(self) -> &'static str {
        match self {
            Self::Google => "https://generativelanguage.googleapis.com/v1beta/models",
            Self::Meta => "https://api.meta.ai/v1/models",
        }
    }
}

/// Parsing establishes format only. The caller must obtain these exact bytes
/// from the current root-bound preparation before asking for access evidence.
pub struct Inputs {
    provider: Provider,
    bytes: Secret,
    key: Secret,
}

impl Inputs {
    /// Wrap a supplied raw key in the one supported format. The provider is
    /// taken from the checked account; no caller-authored JSON is required.
    pub fn encode_key(provider: Provider, key: &[u8]) -> Result<Secret, String> {
        aterm_secrets::api_key::encode(provider.name(), key)
    }
    pub fn decode(bytes: &[u8], expected: Provider) -> Result<Self, String> {
        Ok(Self {
            provider: expected,
            bytes: Secret::new(bytes),
            key: aterm_secrets::api_key::decode(bytes, expected.name())?,
        })
    }

    pub const fn provider(&self) -> Provider {
        self.provider
    }

    pub fn same_bytes(&self, other: &Self) -> bool {
        self.provider == other.provider && self.bytes.expose() == other.bytes.expose()
    }
}

fn wipe(value: Value) {
    match value {
        Value::String(text) => drop(Secret::from_vec(text.into_bytes())),
        Value::Array(values) => values.into_iter().for_each(wipe),
        Value::Object(values) => values.into_values().for_each(wipe),
        _ => {}
    }
}

/// A bounded catalog response; its body is private even if a faulty endpoint
/// echoes input. Neither body nor credential is included in diagnostics.
pub struct Reply {
    pub status: u16,
    pub body: Secret,
}

pub trait Probe {
    fn get(
        &mut self,
        provider: Provider,
        key: Option<&[u8]>,
        deadline: Instant,
    ) -> Result<Reply, String>;
}

pub struct LiveProbe;

impl Probe for LiveProbe {
    fn get(
        &mut self,
        provider: Provider,
        key: Option<&[u8]>,
        deadline: Instant,
    ) -> Result<Reply, String> {
        let wait = deadline.saturating_duration_since(Instant::now());
        if wait.is_zero() {
            return Err("native credential access check expired".into());
        }
        let client = Client::new(Trust::PlatformVerifier, ProxyMode::Environment, wait);
        let mut request = client
            .request(Method::Get, provider.endpoint())
            .header("Cache-Control", "no-cache")
            .limit(RESPONSE_MAX)
            .backstop(wait);
        if let Some(key) = key {
            request = match provider {
                Provider::Google => request.secret_header("x-goog-api-key", Secret::new(key)),
                Provider::Meta => {
                    let mut header = b"Bearer ".to_vec();
                    header.extend_from_slice(key);
                    request.secret_header("Authorization", Secret::from_vec(header))
                }
            };
        }
        let response = request
            .exchange(aterm_http::Body::Bytes(&[]))
            .map_err(|_| "native credential access check did not complete")?;
        let status = response.status();
        let mut bytes = Vec::with_capacity(RESPONSE_MAX + 1);
        let read = response
            .into_body()
            .take((RESPONSE_MAX + 1) as u64)
            .read_to_end(&mut bytes);
        let body = Secret::from_vec(bytes);
        read.map_err(|_| "native credential response did not complete")?;
        if body.len() > RESPONSE_MAX {
            return Err("native credential response exceeds its bound".into());
        }
        Ok(Reply { status, body })
    }
}

/// Current key-dependent catalog access. It makes no email, principal,
/// subscription, spending-right, or future-availability claim.
pub struct Access {
    inputs: Inputs,
}

impl Access {
    /// The same canonical endpoint must reject an unauthenticated request and
    /// accept the exact key. A public catalog can never produce this evidence.
    /// `current` must revalidate root authorization and local policy.
    pub fn check(
        inputs: Inputs,
        probe: &mut dyn Probe,
        deadline: Instant,
        mut current: impl FnMut() -> Result<(), String>,
    ) -> Result<Self, String> {
        current()?;
        if Instant::now() >= deadline {
            return Err("native credential access check expired".into());
        }
        let absent = probe.get(inputs.provider, None, deadline)?;
        if !matches!(absent.status, 401 | 403) {
            return Err("native endpoint did not establish a credential requirement".into());
        }
        current()?;
        let accepted = probe.get(inputs.provider, Some(inputs.key.expose()), deadline)?;
        if accepted.status != 200 || !catalog(inputs.provider, accepted.body.expose()) {
            return Err("native provider did not accept the exact API key".into());
        }
        if Instant::now() >= deadline {
            return Err("native credential access check expired".into());
        }
        current()?;
        Ok(Self { inputs })
    }

    pub const fn provider(&self) -> Provider {
        self.inputs.provider
    }

    /// Compare the exact current root-bound input, not a public key digest.
    pub fn matches(&self, current: &Inputs) -> bool {
        self.inputs.provider == current.provider
            && self.inputs.bytes.expose() == current.bytes.expose()
    }

    /// Bind only this child after independent execution authorization and
    /// exact current input checks. The caller still owns the actual spawn.
    pub fn bind_child(
        &self,
        current: &Inputs,
        command: &mut aterm_uds::spawnfd::Command,
        authorize_execution: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        if !self.matches(current) {
            return Err("native credential changed after its access check".into());
        }
        authorize_execution()?;
        let key = std::str::from_utf8(self.inputs.key.expose())
            .map_err(|_| "native API key is not text")?;
        command.env(self.provider().key_env(), key);
        Ok(())
    }
}

fn catalog(provider: Provider, bytes: &[u8]) -> bool {
    if bytes.len() > RESPONSE_MAX {
        return false;
    }
    let Ok(value) = aterm_json::from_slice::<Value>(bytes) else {
        return false;
    };
    let valid = match provider {
        Provider::Google => value["models"].as_array().is_some_and(|models| {
            !models.is_empty()
                && models.iter().all(|model| {
                    model["name"].as_str().is_some_and(|name| {
                        name.starts_with("models/") && !name.chars().any(char::is_control)
                    })
                })
        }),
        Provider::Meta => {
            value["object"].as_str() == Some("list")
                && value["data"].as_array().is_some_and(|models| {
                    !models.is_empty()
                        && models.iter().all(|model| {
                            model["object"].as_str() == Some("model")
                                && model["id"].as_str().is_some_and(|id| {
                                    !id.is_empty() && !id.chars().any(char::is_control)
                                })
                        })
                })
        }
    };
    wipe(value);
    valid
}

#[cfg(test)]
mod tests;
