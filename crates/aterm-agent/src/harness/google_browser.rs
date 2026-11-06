// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Isolated memory-only browser contexts for initial Google federation.
//! Native CLI credentials persist independently. Explicit snapshots encrypt only Google
//! cookies to the machine key; the browser's default disk profile remains unauthenticated.

use std::os::fd::OwnedFd;
use std::path::Path;
use std::time::{Duration, Instant};

use aterm_json::{Value, json};

use super::google_identity::{Identity, Session};
use super::headless_oauth::{BrowserChild, CdpClient, HeadlessBrowser};

/// A bounded browser bootstrap failure; none contains a URL, token or password.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Deadline,
    IdentityBusy,
    Prepare,
    Launch,
    Protocol,
    WrongContext,
    Script,
    Session,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Deadline => "Google browser bootstrap exceeded its deadline",
            Self::IdentityBusy => "Google identity browser is unavailable or already in use",
            Self::Prepare => "Google browser could not be prepared",
            Self::Launch => "Google browser could not start",
            Self::Protocol => "Google browser control is unavailable",
            Self::WrongContext => "Google browser page belongs to another context",
            Self::Script => "Google browser operation did not complete",
            Self::Session => "Protected Google browser session is unavailable",
        })
    }
}

impl std::error::Error for Error {}

/// A page opened or admitted by one memory-only context.
pub struct Page {
    target: String,
    session: String,
    context: String,
    main_frame: String,
}

impl Page {
    /// The opaque target identifier, for checking a popup's opener.
    #[must_use]
    pub fn target(&self) -> &str {
        &self.target
    }
}

/// A context-bound target observation, without a printable representation of its URL.
pub struct PageInfo {
    target: String,
    opener: Option<String>,
    url: String,
    context: String,
}

impl PageInfo {
    /// The observed URL; callers must not log its OAuth query.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The opaque target identifier.
    #[must_use]
    pub fn target(&self) -> &str {
        &self.target
    }

    /// The opener, when the target was created as a popup.
    #[must_use]
    pub fn opener(&self) -> Option<&str> {
        self.opener.as_deref()
    }
}

fn page_info(value: &Value, context: &str) -> Option<PageInfo> {
    if value.get("type").and_then(Value::as_str) != Some("page")
        || value.get("browserContextId").and_then(Value::as_str) != Some(context)
    {
        return None;
    }
    Some(PageInfo {
        target: value.get("targetId")?.as_str()?.to_owned(),
        opener: value
            .get("openerId")
            .and_then(Value::as_str)
            .map(str::to_owned),
        url: value.get("url")?.as_str()?.to_owned(),
        context: context.to_owned(),
    })
}

/// Owns the browser until it exits, then removes its scratch profile and releases the identity.
/// Authentication pages can be created only in its off-the-record context.
pub struct MemoryBrowser {
    // Field order matters: reap the browser before removing files or releasing ownership.
    _child: BrowserChild,
    cdp: CdpClient,
    context: String,
    deadline: Instant,
    _profile: aterm_tempfile::TempDir,
    _identity: Option<Session>,
    identity: Identity,
    previous_snapshot: Option<[u8; 32]>,
    _work: Option<super::account_work::Scope>,
}

/// Unpublished Google cookies, wiped on drop and protected by the original identity claim.
/// Only a canonically verified native grant can commit them to encrypted storage.
pub(super) struct PendingSession {
    cookies: aterm_secrets::SecretBytes,
    identity: Identity,
    claim: Session,
    home: std::path::PathBuf,
    previous: Option<[u8; 32]>,
    _work: Option<super::account_work::Scope>,
}

impl PendingSession {
    pub(super) fn retain_work(&mut self, work: Option<super::account_work::Scope>) {
        if self._work.is_none() {
            self._work = work;
        }
    }
    pub(super) fn recovery_cookies(
        &self,
        identity: &Identity,
        home: &Path,
    ) -> Result<(&[u8], Option<[u8; 32]>), Error> {
        if &self.identity != identity
            || std::fs::canonicalize(&self.home).map_err(|_| Error::Session)?
                != std::fs::canonicalize(home).map_err(|_| Error::Session)?
        {
            return Err(Error::Session);
        }
        Ok((self.cookies.as_slice(), self.previous))
    }

    /// Quarantined cookies remain unpublished and cannot be restored into a browser.
    pub(super) fn recover(
        identity: &Identity,
        state: &Path,
        home: &Path,
        cookies: aterm_secrets::SecretBytes,
        previous: Option<[u8; 32]>,
    ) -> Result<Self, Error> {
        Self::recover_scoped(identity, state, home, cookies, previous, None)
    }

    pub(super) fn recover_scoped(
        identity: &Identity,
        state: &Path,
        home: &Path,
        cookies: aterm_secrets::SecretBytes,
        previous: Option<[u8; 32]>,
        work: Option<super::account_work::Scope>,
    ) -> Result<Self, Error> {
        if cookies.is_empty() || cookies.len() > super::google_browser_store::MAX_BYTES {
            return Err(Error::Session);
        }
        Ok(Self {
            cookies,
            identity: identity.clone(),
            claim: identity.claim(state).map_err(|_| Error::IdentityBusy)?,
            home: home.into(),
            previous,
            _work: work,
        })
    }

    pub(super) fn commit(
        self,
        proof: &super::agy_signin::VerifiedGoogleGrant,
    ) -> Result<(), Error> {
        self.commit_using(|identity| {
            proof
                .check_browser_identity(identity)
                .map_err(|_| Error::Session)?;
            if !identity
                .email()
                .eq_ignore_ascii_case(&proof.profile().email)
            {
                return Err(Error::Session);
            }
            Ok(())
        })
    }

    pub(super) fn commit_kimi(
        self,
        proof: &super::google_federation::VerifiedKimiGoogle<'_>,
    ) -> Result<(), Error> {
        self.commit_using(|identity| {
            proof
                .check_browser_identity(identity)
                .map_err(|_| Error::Session)
        })
    }

    fn commit_using(self, check: impl Fn(&Identity) -> Result<(), Error>) -> Result<(), Error> {
        if self
            ._work
            .as_ref()
            .is_some_and(super::account_work::Scope::cancelled)
        {
            return Err(Error::Session);
        }
        check(&self.identity)?;
        if super::google_browser_store::fingerprint(&self.claim).map_err(|_| Error::Session)?
            != self.previous
        {
            // A publication made while this candidate was stopped wins. The native
            // proof can still be used without rolling its newer browser session back.
            if self
                ._work
                .as_ref()
                .is_some_and(super::account_work::Scope::cancelled)
            {
                return Err(Error::Session);
            }
            return check(&self.identity);
        }
        if self
            ._work
            .as_ref()
            .is_some_and(super::account_work::Scope::cancelled)
        {
            return Err(Error::Session);
        }
        super::google_browser_store::save(
            &self.home,
            &self.identity,
            &self.claim,
            self.cookies.as_slice(),
        )
        .map_err(|_| Error::Session)?;
        check(&self.identity)
    }
}

#[cfg(test)]
pub(super) fn pending_fixture(
    identity: &Identity,
    state: &Path,
    home: &Path,
    cookies: &[u8],
) -> PendingSession {
    let claim = identity.claim(state).unwrap();
    PendingSession {
        cookies: aterm_secrets::SecretBytes::from(cookies),
        identity: identity.clone(),
        previous: super::google_browser_store::fingerprint(&claim).unwrap(),
        claim,
        home: home.into(),
        _work: None,
    }
}

impl MemoryBrowser {
    /// Fetch the pinned full browser, launch it without a window, and create one incognito context.
    /// The scratch default profile receives no authentication navigation.
    ///
    /// # Errors
    /// The deadline expired, the identity is held, or browser preparation or control failed.
    pub fn start(
        identity: &Identity,
        machine_state: &Path,
        home: Option<&Path>,
        deadline: Instant,
    ) -> Result<Self, Error> {
        Self::start_scoped(identity, machine_state, home, deadline, None)
    }

    /// Start with the caller's automatic lifetime retained by actual CDP workers.
    pub fn start_scoped(
        identity: &Identity,
        machine_state: &Path,
        home: Option<&Path>,
        deadline: Instant,
        work: Option<super::account_work::Scope>,
    ) -> Result<Self, Error> {
        if work
            .as_ref()
            .is_some_and(super::account_work::Scope::cancelled)
        {
            return Err(Error::Prepare);
        }
        remaining(deadline)?;
        let claim = identity
            .claim(machine_state)
            .map_err(|_| Error::IdentityBusy)?;
        let previous_snapshot =
            super::google_browser_store::fingerprint(&claim).map_err(|_| Error::Session)?;
        let browser = super::browser_window::fetch_window_browser(
            home,
            &mut super::headless_fetch::Curl,
            &|| {
                Instant::now() >= deadline
                    || work
                        .as_ref()
                        .is_some_and(super::account_work::Scope::cancelled)
            },
        )
        .map_err(|_| Error::Prepare)?;
        remaining(deadline)?;
        let profile = aterm_tempfile::tempdir().map_err(|_| Error::Prepare)?;
        let browser = HeadlessBrowser {
            path: browser,
            is_headless_shell: false,
        };
        let (commands, to_browser) = std::io::pipe().map_err(|_| Error::Launch)?;
        let (from_browser, replies) = std::io::pipe().map_err(|_| Error::Launch)?;
        let command =
            super::headless_oauth::headless_browser_command(&browser, profile.path(), &|name| {
                std::env::var_os(name)
            });
        if work
            .as_ref()
            .is_some_and(super::account_work::Scope::cancelled)
        {
            return Err(Error::Prepare);
        }
        let child = aterm_uds::spawnfd::spawn_with_two_fds(
            command,
            OwnedFd::from(commands),
            OwnedFd::from(replies),
        )
        .map_err(|_| Error::Launch)?;
        let child = BrowserChild::new(child);
        let mut cdp = CdpClient::new_scoped(to_browser, from_browser, work.clone())
            .map_err(|_| Error::Launch)?;
        let context = cdp
            .call(
                None,
                "Target.createBrowserContext",
                json!({"disposeOnDetach":true}),
                remaining(deadline)?,
            )
            .map_err(|_| Error::Protocol)?
            .get("browserContextId")
            .and_then(Value::as_str)
            .ok_or(Error::Protocol)?
            .to_owned();
        if context.is_empty() {
            return Err(Error::Protocol);
        }
        Ok(Self {
            _child: child,
            _work: work,
            cdp,
            context,
            deadline,
            _profile: profile,
            _identity: Some(claim),
            identity: identity.clone(),
            previous_snapshot,
        })
    }

    /// Restore this identity's machine-encrypted Google cookies into its memory context.
    /// Malformed or foreign snapshots are refused before any cookie is imported.
    ///
    /// # Errors
    /// Protected storage, cookie validation or browser control failed.
    pub fn restore_session(&mut self, home: &Path) -> Result<bool, Error> {
        let Some(bytes) = super::google_browser_store::load(
            home,
            &self.identity,
            self._identity.as_ref().ok_or(Error::IdentityBusy)?,
        )
        .map_err(|_| Error::Session)?
        else {
            return Ok(false);
        };
        let value =
            SecretValue(aterm_json::from_slice(bytes.as_slice()).map_err(|_| Error::Session)?);
        let cookies = google_cookies(&value.0)?;
        self.call(
            None,
            "Storage.setCookies",
            json!({"browserContextId":self.context,"cookies":cookies.0}),
        )?;
        Ok(true)
    }

    /// Save only Google cookies, encrypted to this machine and bound to this identity.
    /// The caller must first prove the expected account on a Google-origin authenticated page.
    /// The plaintext default browser profile is never used for authentication.
    ///
    /// # Errors
    /// Cookie validation, protected storage or browser control failed.
    pub fn save_session(&mut self, home: &Path) -> Result<(), Error> {
        let bytes = self.session_bytes()?;
        if super::google_browser_store::fingerprint(
            self._identity.as_ref().ok_or(Error::IdentityBusy)?,
        )
        .map_err(|_| Error::Session)?
            != self.previous_snapshot
        {
            return Err(Error::Session);
        }
        super::google_browser_store::save(
            home,
            &self.identity,
            self._identity.as_ref().ok_or(Error::IdentityBusy)?,
            bytes.as_slice(),
        )
        .map_err(|_| Error::Session)?;
        self.previous_snapshot = super::google_browser_store::fingerprint(
            self._identity.as_ref().ok_or(Error::IdentityBusy)?,
        )
        .map_err(|_| Error::Session)?;
        Ok(())
    }

    /// Capture cookies in memory and retain this identity's claim after closing the browser.
    /// No snapshot is written until the native grant proves the expected Google account.
    ///
    /// # Errors
    /// Cookie capture or validation failed, or the identity claim is unavailable.
    pub(super) fn into_pending_session(mut self, home: &Path) -> Result<PendingSession, Error> {
        let cookies = self.session_bytes()?;
        let claim = self._identity.take().ok_or(Error::IdentityBusy)?;
        Ok(PendingSession {
            cookies,
            identity: self.identity.clone(),
            claim,
            home: home.into(),
            previous: self.previous_snapshot,
            _work: self._work.clone(),
        })
    }

    fn session_bytes(&mut self) -> Result<aterm_secrets::SecretBytes, Error> {
        let value = SecretValue(self.call(
            None,
            "Storage.getCookies",
            json!({"browserContextId":self.context}),
        )?);
        let cookies = google_cookies(&value.0["cookies"])?;
        if cookies.0.as_array().is_none_or(Vec::is_empty) {
            return Err(Error::Session);
        }
        Ok(aterm_secrets::SecretBytes::from(
            aterm_json::to_vec(&cookies.0).map_err(|_| Error::Session)?,
        ))
    }

    /// Open a page inside this identity's memory-only context.
    ///
    /// # Errors
    /// The deadline expired, or target creation, admission or navigation failed.
    pub fn open(&mut self, url: &str) -> Result<Page, Error> {
        let target = self
            .call(
                None,
                "Target.createTarget",
                json!({"url":"about:blank","browserContextId":self.context}),
            )?
            .get("targetId")
            .and_then(Value::as_str)
            .ok_or(Error::Protocol)?
            .to_owned();
        let info = self.call(None, "Target.getTargetInfo", json!({"targetId":target}))?;
        let info = page_info(
            info.get("targetInfo").ok_or(Error::Protocol)?,
            &self.context,
        )
        .ok_or(Error::WrongContext)?;
        let page = self.attach(&info)?;
        self.navigate(&page, url)?;
        Ok(page)
    }

    /// Observe only this context's page targets, including its popups.
    ///
    /// # Errors
    /// The deadline expired or the browser could not enumerate targets.
    pub fn pages(&mut self) -> Result<Vec<PageInfo>, Error> {
        let result = self.call(None, "Target.getTargets", json!({}))?;
        let targets = result
            .get("targetInfos")
            .and_then(Value::as_array)
            .ok_or(Error::Protocol)?;
        Ok(targets
            .iter()
            .filter_map(|info| page_info(info, &self.context))
            .collect())
    }

    /// Drain URLs of windows opened by this admitted page. The event captures the requested
    /// URL before redirects; callers must separately admit and bind the resulting target.
    ///
    /// # Errors
    /// The page belongs to another context.
    pub fn window_opens(&mut self, page: &Page) -> Result<Vec<String>, Error> {
        self.check_page(page)?;
        Ok(self
            .cdp
            .take_events("Page.windowOpen", Some(&page.session))
            .into_iter()
            .filter_map(|event| {
                event
                    .pointer("/params/url")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .collect())
    }

    /// Drain same-tab navigation requests made by this page's main frame, before redirects
    /// can hide the original OAuth URL. Child-frame requests and requests opening another
    /// target are excluded; popups are observed separately by [`Self::window_opens`].
    ///
    /// # Errors
    /// The page belongs to another context.
    pub fn navigation_requests(&mut self, page: &Page) -> Result<Vec<String>, Error> {
        self.check_page(page)?;
        Ok(self
            .cdp
            .take_events("Page.frameRequestedNavigation", Some(&page.session))
            .into_iter()
            .filter_map(|event| navigation_request(&event, &page.main_frame))
            .collect())
    }

    /// Original committed main-frame URLs, before page scripts can clear OAuth queries.
    /// Only this owned page/session supplies observations; callers still validate the flow.
    pub fn navigation_commits(&mut self, page: &Page) -> Result<Vec<String>, Error> {
        self.check_page(page)?;
        Ok(self
            .cdp
            .take_events("Page.frameNavigated", Some(&page.session))
            .into_iter()
            .filter_map(|event| {
                let frame = event.pointer("/params/frame")?;
                if frame.get("id")?.as_str()? != page.main_frame {
                    return None;
                }
                frame.get("url")?.as_str().map(str::to_owned)
            })
            .collect())
    }

    /// Revalidate and attach a target observed in this context.
    ///
    /// # Errors
    /// The target changed context, disappeared, or browser control failed.
    pub fn attach(&mut self, info: &PageInfo) -> Result<Page, Error> {
        if info.context != self.context {
            return Err(Error::WrongContext);
        }
        let current = self.call(
            None,
            "Target.getTargetInfo",
            json!({"targetId":info.target}),
        )?;
        let current = page_info(
            current.get("targetInfo").ok_or(Error::Protocol)?,
            &self.context,
        )
        .ok_or(Error::WrongContext)?;
        let session = self
            .call(
                None,
                "Target.attachToTarget",
                json!({"targetId":current.target,"flatten":true}),
            )?
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or(Error::Protocol)?
            .to_owned();
        for method in ["Page.enable", "Runtime.enable"] {
            self.call(Some(&session), method, json!({}))?;
        }
        let main_frame = self
            .call(Some(&session), "Page.getFrameTree", json!({}))?
            .pointer("/frameTree/frame/id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or(Error::Protocol)?
            .to_owned();
        Ok(Page {
            target: current.target,
            session,
            context: self.context.clone(),
            main_frame,
        })
    }

    /// Navigate an admitted page. Origin and OAuth flow policy belong to the caller.
    ///
    /// # Errors
    /// The page belongs to another context or browser control failed.
    pub fn navigate(&mut self, page: &Page, url: &str) -> Result<(), Error> {
        self.check_page(page)?;
        self.call(Some(&page.session), "Page.navigate", json!({"url":url}))?;
        Ok(())
    }

    /// Evaluate in an admitted page and return its JSON value without logging the expression.
    /// Security-sensitive scripts must recheck their origin inside the expression.
    ///
    /// # Errors
    /// The page is not admitted, the deadline expired, or evaluation failed.
    pub fn evaluate(
        &mut self,
        page: &Page,
        expression: &str,
        gesture: bool,
    ) -> Result<Value, Error> {
        self.check_page(page)?;
        let value = self.call(
            Some(&page.session),
            "Runtime.evaluate",
            json!({"expression":expression,"returnByValue":true,"userGesture":gesture}),
        )?;
        if value.get("exceptionDetails").is_some() {
            return Err(Error::Script);
        }
        value.pointer("/result/value").cloned().ok_or(Error::Script)
    }

    /// Send one primary-button pointer click inside an admitted page.
    /// Coordinates are viewport CSS pixels read from that page, never screen coordinates.
    ///
    /// # Errors
    /// The coordinates are invalid, the page is not admitted, or browser control failed.
    pub fn click(&mut self, page: &Page, x: f64, y: f64) -> Result<(), Error> {
        self.check_page(page)?;
        if !x.is_finite()
            || !y.is_finite()
            || !(0.0..=16384.0).contains(&x)
            || !(0.0..=16384.0).contains(&y)
        {
            return Err(Error::Script);
        }
        for (kind, buttons) in [("mouseMoved", 0), ("mousePressed", 1), ("mouseReleased", 0)] {
            self.call(
                Some(&page.session),
                "Input.dispatchMouseEvent",
                json!({"type":kind,"x":x,"y":y,"button":"left","buttons":buttons,"clickCount":1}),
            )?;
        }
        Ok(())
    }

    fn check_page(&self, page: &Page) -> Result<(), Error> {
        if page.context != self.context || page.target.is_empty() || page.session.is_empty() {
            return Err(Error::WrongContext);
        }
        Ok(())
    }

    fn call(&mut self, session: Option<&str>, method: &str, params: Value) -> Result<Value, Error> {
        self.cdp
            .call(session, method, params, remaining(self.deadline)?)
            .map_err(|_| Error::Protocol)
    }
}

fn navigation_request(event: &Value, main_frame: &str) -> Option<String> {
    let params = event.get("params")?;
    if main_frame.is_empty()
        || params.get("frameId")?.as_str()? != main_frame
        || params.get("disposition")?.as_str()? != "currentTab"
    {
        return None;
    }
    params.get("url")?.as_str().map(str::to_owned)
}

/// CDP cookie responses and parameter copies contain bearer credentials. Wipe their strings
/// even when validation or browser control returns early.
struct SecretValue(Value);

impl Drop for SecretValue {
    fn drop(&mut self) {
        let mut pending = vec![std::mem::replace(&mut self.0, Value::Null)];
        while let Some(value) = pending.pop() {
            match value {
                Value::String(text) => drop(aterm_secrets::SecretBytes::from(text)),
                Value::Array(values) => pending.extend(values),
                Value::Object(values) => {
                    for (key, value) in values {
                        drop(aterm_secrets::SecretBytes::from(key));
                        pending.push(value);
                    }
                }
                _ => {}
            }
        }
    }
}

fn google_cookies(value: &Value) -> Result<SecretValue, Error> {
    let values = value
        .as_array()
        .filter(|values| values.len() <= 256)
        .ok_or(Error::Session)?;
    let mut admitted = SecretValue(json!([]));
    for cookie in values {
        let domain = cookie
            .get("domain")
            .and_then(Value::as_str)
            .ok_or(Error::Session)?;
        if !matches!(
            domain,
            ".google.com" | "google.com" | "accounts.google.com" | ".accounts.google.com"
        ) {
            continue;
        }
        // A vendor's partitioned third-party cookie is not the first-party Google session.
        if cookie.get("partitionKey").is_some()
            || cookie.get("partitionKeyOpaque").and_then(Value::as_bool) == Some(true)
        {
            continue;
        }
        for (field, limit) in [("name", 1024), ("value", 8192), ("path", 4096)] {
            let text = cookie
                .get(field)
                .and_then(Value::as_str)
                .ok_or(Error::Session)?;
            if text.len() > limit
                || text.chars().any(char::is_control)
                || (field != "value" && text.is_empty())
            {
                return Err(Error::Session);
            }
        }
        let mut selected = SecretValue(json!({}));
        for field in ["name", "value", "domain", "path"] {
            selected
                .0
                .as_object_mut()
                .ok_or(Error::Session)?
                .insert(field.into(), cookie[field].clone());
        }
        for field in ["secure", "httpOnly"] {
            if let Some(value) = cookie.get(field) {
                if value.as_bool().is_none() {
                    return Err(Error::Session);
                }
                selected
                    .0
                    .as_object_mut()
                    .ok_or(Error::Session)?
                    .insert(field.into(), value.clone());
            }
        }
        if let Some(value) = cookie.get("sameSite") {
            if !matches!(value.as_str(), Some("Strict" | "Lax" | "None")) {
                return Err(Error::Session);
            }
            selected
                .0
                .as_object_mut()
                .ok_or(Error::Session)?
                .insert("sameSite".into(), value.clone());
        }
        if let Some(value) = cookie.get("expires") {
            let expires = value.as_f64().ok_or(Error::Session)?;
            if !expires.is_finite() || (expires < 0.0 && expires != -1.0) {
                return Err(Error::Session);
            }
            if expires >= 0.0 {
                let now = aterm_time::unix_secs().ok_or(Error::Session)?;
                if expires <= now as f64 {
                    continue;
                }
                selected
                    .0
                    .as_object_mut()
                    .ok_or(Error::Session)?
                    .insert("expires".into(), cookie["expires"].clone());
            }
        }
        admitted
            .0
            .as_array_mut()
            .ok_or(Error::Session)?
            .push(std::mem::replace(&mut selected.0, Value::Null));
    }
    Ok(admitted)
}

impl Drop for MemoryBrowser {
    fn drop(&mut self) {
        let _ = self.cdp.call(
            None,
            "Target.disposeBrowserContext",
            json!({"browserContextId":self.context}),
            Duration::from_secs(2),
        );
        let _ = self
            .cdp
            .call(None, "Browser.close", json!({}), Duration::from_secs(2));
    }
}

fn remaining(deadline: Instant) -> Result<Duration, Error> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        Err(Error::Deadline)
    } else {
        Ok(left.min(Duration::from_secs(10)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_google_cookies_retain_work_until_the_identity_claim_retires() {
        let root = aterm_tempfile::tempdir().unwrap();
        let registry = super::super::google_identity::Registry::parse(
            "[[identities]]\nid='scope-fixture'\nemail='fixture@example.invalid'\n[[bindings]]\nprovider='fixture'\naccount='one'\nidentity='scope-fixture'\nexpected_email='fixture@example.invalid'\n",
        ).unwrap();
        let identity = registry.resolve("fixture", "one").unwrap().0;
        let lifetime = super::super::account_work::Lifetime::default();
        let mut pending = pending_fixture(identity, root.path(), root.path(), b"synthetic cookies");
        pending._work = lifetime.enter();
        let pause = lifetime.pause().unwrap().try_wait().unwrap_err();
        assert!(identity.claim(root.path()).is_err());
        drop(pending);
        let drained = pause.try_wait().unwrap();
        let claim = identity.claim(root.path()).unwrap();
        assert!(drained.resume().is_ok());
        drop(claim);
    }

    #[test]
    fn pending_google_cookies_do_not_recheck_a_proof_after_work_closes() {
        let root = aterm_tempfile::tempdir().unwrap();
        let registry = super::super::google_identity::Registry::parse(
            "[[identities]]\nid='scope-fixture'\nemail='fixture@example.invalid'\n[[bindings]]\nprovider='fixture'\naccount='one'\nidentity='scope-fixture'\nexpected_email='fixture@example.invalid'\n",
        ).unwrap();
        let identity = registry.resolve("fixture", "one").unwrap().0;
        let lifetime = super::super::account_work::Lifetime::default();
        let mut pending = pending_fixture(identity, root.path(), root.path(), b"synthetic cookies");
        pending._work = lifetime.enter();
        assert_eq!(
            super::super::google_browser_store::fingerprint(&pending.claim).unwrap(),
            None
        );
        pending.previous = Some([42; 32]);
        let checks = std::cell::Cell::new(0);
        let pause = std::cell::RefCell::new(None);
        let result = pending.commit_using(|_| {
            checks.set(checks.get() + 1);
            if checks.get() == 1 {
                *pause.borrow_mut() = lifetime.pause();
            }
            Ok(())
        });
        assert_eq!(result, Err(Error::Session));
        assert_eq!(checks.get(), 1);
        assert!(pause.into_inner().unwrap().try_wait().is_ok());
    }

    #[test]
    fn recovered_google_cookies_keep_work_and_refuse_a_closed_commit() {
        let root = aterm_tempfile::tempdir().unwrap();
        let registry = super::super::google_identity::Registry::parse(
            "[[identities]]\nid='scope-fixture'\nemail='fixture@example.invalid'\n[[bindings]]\nprovider='fixture'\naccount='one'\nidentity='scope-fixture'\nexpected_email='fixture@example.invalid'\n",
        ).unwrap();
        let identity = registry.resolve("fixture", "one").unwrap().0;
        let lifetime = super::super::account_work::Lifetime::default();
        let pending = PendingSession::recover_scoped(
            identity,
            root.path(),
            root.path(),
            aterm_secrets::SecretBytes::from(b"synthetic cookies".as_slice()),
            None,
            lifetime.enter(),
        )
        .unwrap();
        let pause = lifetime.pause().unwrap().try_wait().unwrap_err();
        let checks = std::cell::Cell::new(0);
        assert_eq!(
            pending.commit_using(|_| {
                checks.set(checks.get() + 1);
                Err(Error::Session)
            }),
            Err(Error::Session)
        );
        assert_eq!(checks.get(), 0);
        assert!(pause.try_wait().is_ok());
        assert!(identity.claim(root.path()).is_ok());
    }

    #[test]
    fn only_the_owned_main_frame_can_supply_a_same_tab_handoff() {
        let event = |frame: &str, disposition: &str| json!({"params":{"frameId":frame,"disposition":disposition,"url":"https://accounts.google.com/o/oauth2/v2/auth?client_id=fixture"}});
        assert!(navigation_request(&event("main", "currentTab"), "main").is_some());
        for (frame, disposition) in [
            ("child", "currentTab"),
            ("main", "newTab"),
            ("main", "newWindow"),
            ("main", "download"),
        ] {
            assert!(navigation_request(&event(frame, disposition), "main").is_none());
        }
        assert!(navigation_request(&event("", "currentTab"), "").is_none());
        assert!(navigation_request(&json!({"params":{"frameId":"main"}}), "main").is_none());
    }

    #[test]
    #[ignore = "uses the pinned headless browser and a local callback that clears its URL"]
    fn native_committed_callback_survives_immediate_query_cleanup() {
        use std::io::{BufRead as _, Write as _};
        use std::os::fd::AsFd as _;
        let root = aterm_tempfile::tempdir().unwrap();
        let registry=super::super::google_identity::Registry::parse("[[identities]]\nid='callback-fixture'\nemail='fixture@example.invalid'\n[[bindings]]\nprovider='fixture'\naccount='one'\nidentity='callback-fixture'\nexpected_email='fixture@example.invalid'\n").unwrap();
        let identity = registry.resolve("fixture", "one").unwrap().0;
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut browser =
            MemoryBrowser::start(identity, root.path(), home.as_deref(), deadline).unwrap();
        let listener = aterm_uds::kept_out(|| std::net::TcpListener::bind("127.0.0.1:0")).unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = std::thread::spawn(move || {
            aterm_uds::ready::until(listener.as_fd(), aterm_uds::ready::Interest::Read, deadline)
                .unwrap();
            let (mut stream, _) = aterm_uds::accept(&listener).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut reader = std::io::BufReader::new(&stream);
            for _ in 0..32 {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
            }
            drop(reader);
            let body = "<html><script>history.replaceState({},'', '/oauth-callback');document.title='cleared';</script></html>";
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
        });
        let original =
            format!("http://{address}/oauth-callback?code=fixture-code&state=fixture-state");
        let page = browser.open(&original).unwrap();
        let mut observed = false;
        loop {
            assert!(
                Instant::now() < deadline,
                "callback did not clear its query"
            );
            observed |= browser
                .navigation_commits(&page)
                .unwrap()
                .iter()
                .any(|url| url == &original);
            if browser
                .evaluate(
                    &page,
                    "document.title==='cleared'&&location.search===''",
                    false,
                )
                .ok()
                == Some(json!(true))
                && observed
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        server.join().unwrap();
        assert!(browser.navigation_commits(&page).unwrap().is_empty());
    }

    #[test]
    #[ignore = "runs a same-tab navigation in the pinned full headless browser"]
    fn native_main_frame_navigation_is_observed_before_the_destination() {
        let root = aterm_tempfile::tempdir().unwrap();
        let registry = super::super::google_identity::Registry::parse("[[identities]]\nid='navigation'\nemail='fixture@example.invalid'\n[[bindings]]\nprovider='fixture'\naccount='one'\nidentity='navigation'\nexpected_email='fixture@example.invalid'\n").unwrap();
        let identity = registry.resolve("fixture", "one").unwrap().0;
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut browser =
            MemoryBrowser::start(identity, root.path(), home.as_deref(), deadline).unwrap();
        let page = browser.open("about:blank").unwrap();
        browser
            .evaluate(
                &page,
                "setTimeout(()=>location.assign('about:blank?same-tab'),0);true",
                true,
            )
            .unwrap();
        loop {
            assert!(
                Instant::now() < deadline,
                "same-tab navigation event did not arrive"
            );
            browser.pages().unwrap();
            let requests = browser.navigation_requests(&page).unwrap();
            if !requests.is_empty() {
                assert_eq!(requests, ["about:blank?same-tab"]);
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(browser.window_opens(&page).unwrap().is_empty());
        assert!(browser.navigation_requests(&page).unwrap().is_empty());
    }

    #[test]
    fn target_admission_excludes_default_and_other_identity_contexts() {
        let own = json!({"type":"page","targetId":"popup","openerId":"parent","url":"https://accounts.google.com/signin","browserContextId":"own"});
        let info = page_info(&own, "own").unwrap();
        assert_eq!(info.target(), "popup");
        assert_eq!(info.url(), "https://accounts.google.com/signin");
        assert_eq!(info.opener(), Some("parent"));
        assert!(page_info(&own, "other").is_none());
        assert!(page_info(&json!({"type":"page","targetId":"default","url":"https://accounts.google.com/signin"}), "own").is_none());
        assert!(page_info(&json!({"type":"iframe","targetId":"frame","url":"https://accounts.google.com/","browserContextId":"own"}), "own").is_none());
    }

    #[test]
    fn session_cookie_admission_excludes_other_domains_partitions_and_expired_values() {
        let cookie = |domain: &str| json!({"name":"fixture","value":"secret-fixture","domain":domain,"path":"/","secure":true,"httpOnly":true,"expires":-1});
        let mut expired = cookie(".google.com");
        expired
            .as_object_mut()
            .unwrap()
            .insert("expires".into(), json!(1));
        let mut partitioned = cookie(".google.com");
        partitioned.as_object_mut().unwrap().insert(
            "partitionKey".into(),
            json!({"topLevelSite":"https://vendor.example"}),
        );
        let input = json!([
            cookie(".google.com"),
            cookie("accounts.google.com"),
            cookie(".google.com.evil.test"),
            cookie("vendor.example"),
            expired,
            partitioned
        ]);
        let accepted = google_cookies(&input).unwrap();
        assert_eq!(accepted.0.as_array().unwrap().len(), 2);
        let mut malformed = cookie(".google.com");
        malformed
            .as_object_mut()
            .unwrap()
            .insert("secure".into(), json!("yes"));
        assert!(google_cookies(&json!([malformed])).is_err());
        assert!(google_cookies(&json!({})).is_err());
    }

    #[test]
    #[ignore = "verifies encrypted cookie restart through the pinned full browser"]
    fn native_encrypted_session_survives_restart_without_plaintext_profile_cookies() {
        let root = aterm_tempfile::tempdir().unwrap();
        let store_home = aterm_tempfile::tempdir().unwrap();
        let registry = super::super::google_identity::Registry::parse("[[identities]]\nid='encrypted-fixture'\nemail='fixture@example.invalid'\n[[bindings]]\nprovider='fixture'\naccount='one'\nidentity='encrypted-fixture'\nexpected_email='fixture@example.invalid'\n").unwrap();
        let identity = registry.resolve("fixture", "one").unwrap().0;
        let browser_home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut browser =
            MemoryBrowser::start(identity, root.path(), browser_home.as_deref(), deadline).unwrap();
        assert!(!browser.restore_session(store_home.path()).unwrap());
        browser.call(None,"Storage.setCookies",json!({"browserContextId":browser.context,"cookies":[{"name":"fixture","value":"encrypted-cookie-fixture","domain":".google.com","path":"/","secure":true,"httpOnly":true},{"name":"vendor","value":"excluded-fixture","domain":"example.invalid","path":"/"}]})).unwrap();
        browser.save_session(store_home.path()).unwrap();
        let scratch = browser._profile.path().to_owned();
        drop(browser);
        assert!(!scratch.exists());
        let mut restarted =
            MemoryBrowser::start(identity, root.path(), browser_home.as_deref(), deadline).unwrap();
        assert!(restarted.restore_session(store_home.path()).unwrap());
        let cookies = SecretValue(
            restarted
                .call(
                    None,
                    "Storage.getCookies",
                    json!({"browserContextId":restarted.context}),
                )
                .unwrap(),
        );
        let values = cookies.0["cookies"].as_array().unwrap();
        assert_eq!(values.len(), 1);
        assert_eq!(values[0]["name"].as_str(), Some("fixture"));
        assert_eq!(
            values[0]["value"].as_str(),
            Some("encrypted-cookie-fixture")
        );
        let default = SecretValue(
            restarted
                .call(None, "Storage.getCookies", json!({}))
                .unwrap(),
        );
        assert!(default.0["cookies"].as_array().unwrap().is_empty());
    }

    #[test]
    #[ignore = "verifies deferred cookie commit and restart through the pinned full browser; account proof is synthetic"]
    fn native_pending_session_survives_browser_exit_and_commits_after_proof() {
        let state = aterm_tempfile::tempdir().unwrap();
        let home = aterm_tempfile::tempdir().unwrap();
        let profile = aterm_tempfile::tempdir().unwrap();
        let text = "[[identities]]\nid='pending'\nemail='fixture@example.invalid'\n[[bindings]]\nprovider='agy'\naccount='one'\nidentity='pending'\nexpected_email='fixture@example.invalid'\n[[bindings]]\nprovider='kimi-global'\naccount='one'\nidentity='pending'\nexpected_email='fixture@example.invalid'\n";
        aterm_secrets::fsx::write_atomic(
            &state.path().join("google-identities.toml"),
            text.as_bytes(),
            0o600,
        )
        .unwrap();
        let registry = super::super::google_identity::Registry::parse(text).unwrap();
        let identity = registry.resolve("agy", "one").unwrap().0;
        let browser_home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut browser =
            MemoryBrowser::start(identity, state.path(), browser_home.as_deref(), deadline)
                .unwrap();
        browser.call(None,"Storage.setCookies",json!({"browserContextId":browser.context,"cookies":[{"name":"fixture","value":"pending-cookie","domain":".google.com","path":"/","secure":true,"httpOnly":true},{"name":"vendor","value":"excluded-cookie","domain":"example.invalid","path":"/"}]})).unwrap();
        let scratch = browser._profile.path().to_owned();
        let pending = browser.into_pending_session(home.path()).unwrap();
        assert!(
            !scratch.exists(),
            "cookie candidate retained the native browser"
        );
        assert!(
            identity.claim(state.path()).is_err(),
            "browser exit released the pending identity"
        );
        let path = pending.claim.profile().join("session.age");
        assert!(
            !path.exists(),
            "cookies were persisted before account proof"
        );
        let proof = super::super::agy_signin::publication_fixture(
            state.path(),
            profile.path(),
            "one",
            "fixture@example.invalid",
        );
        pending.commit(&proof).unwrap();
        let saved = std::fs::read(&path).unwrap();
        assert!(
            !saved
                .windows(b"pending-cookie".len())
                .any(|part| part == b"pending-cookie")
        );
        let kimi_store = super::super::accounts::Store::at_for(
            state.path(),
            crate::ai::connector::Cli::Kimi,
            home.path(),
        );
        let approver = super::super::headless_oauth::VendorApprover::with_mailbox(
            crate::ai::connector::Cli::Kimi,
            "one",
            &kimi_store.shared_dir(),
            home.path(),
            super::super::headless_oauth::MailboxConfig::default(),
        );
        assert_eq!(identity, registry.resolve("kimi-global", "one").unwrap().0);
        let mut restarted = MemoryBrowser::start(
            identity,
            &approver.google_state,
            browser_home.as_deref(),
            deadline,
        )
        .unwrap();
        assert!(restarted.restore_session(home.path()).unwrap());
        let cookies = SecretValue(
            restarted
                .call(
                    None,
                    "Storage.getCookies",
                    json!({"browserContextId":restarted.context}),
                )
                .unwrap(),
        );
        assert_eq!(cookies.0["cookies"].as_array().unwrap().len(), 1);
        assert_eq!(
            cookies.0["cookies"][0]["value"].as_str(),
            Some("pending-cookie")
        );
        restarted.call(None,"Storage.setCookies",json!({"browserContextId":restarted.context,"cookies":[{"name":"fixture","value":"discarded-cookie","domain":".google.com","path":"/","secure":true,"httpOnly":true}]})).unwrap();
        let discarded = restarted.into_pending_session(home.path()).unwrap();
        drop(discarded);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            saved,
            "discarding a candidate replaced the committed session"
        );
        assert!(identity.claim(state.path()).is_ok());
        let mut active =
            MemoryBrowser::start(identity, state.path(), browser_home.as_deref(), deadline)
                .unwrap();
        assert!(active.restore_session(home.path()).unwrap());
        active.call(None,"Storage.setCookies",json!({"browserContextId":active.context,"cookies":[{"name":"fixture","value":"older-live-context","domain":".google.com","path":"/","secure":true,"httpOnly":true}]})).unwrap();
        let newer = br#"[{"name":"fixture","value":"newer-publication","domain":".google.com","path":"/","secure":true}]"#;
        super::super::google_browser_store::save(
            home.path(),
            identity,
            active._identity.as_ref().unwrap(),
            newer,
        )
        .unwrap();
        let ciphertext = std::fs::read(&path).unwrap();
        let older = active.into_pending_session(home.path()).unwrap();
        older.commit(&proof).unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            ciphertext,
            "a live context rolled a later snapshot back during completion"
        );
        let claim = identity.claim(state.path()).unwrap();
        assert_eq!(
            super::super::google_browser_store::load(home.path(), identity, &claim)
                .unwrap()
                .unwrap()
                .as_slice(),
            newer
        );
    }

    #[test]
    #[ignore = "provisions the pinned full browser and verifies native context isolation"]
    fn native_bootstrap_keeps_cookies_only_in_its_memory_context() {
        let root = aterm_tempfile::tempdir().unwrap();
        let registry = super::super::google_identity::Registry::parse("[[identities]]\nid='fixture'\nemail='fixture@example.invalid'\n[[bindings]]\nprovider='fixture'\naccount='one'\nidentity='fixture'\nexpected_email='fixture@example.invalid'\n").unwrap();
        let identity = registry.resolve("fixture", "one").unwrap().0;
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        let mut browser = MemoryBrowser::start(
            identity,
            root.path(),
            home.as_deref(),
            Instant::now() + Duration::from_secs(300),
        )
        .unwrap();
        let page = browser.open("about:blank").unwrap();
        assert_eq!(browser.evaluate(&page, "1+1", false).unwrap(), json!(2));
        browser.call(None, "Storage.setCookies", json!({"browserContextId":browser.context,"cookies":[{"name":"fixture","value":"memory-only","domain":"example.invalid","path":"/"}]})).unwrap();
        let cookies = browser
            .call(
                None,
                "Storage.getCookies",
                json!({"browserContextId":browser.context}),
            )
            .unwrap();
        assert_eq!(cookies["cookies"].as_array().unwrap().len(), 1);
        let default_cookies = browser.call(None, "Storage.getCookies", json!({})).unwrap();
        assert!(default_cookies["cookies"].as_array().unwrap().is_empty());
        let targets = browser.pages().unwrap();
        assert!(targets.iter().any(|target| target.target() == page.target));
        assert!(
            targets
                .iter()
                .all(|target| target.context == browser.context)
        );
        let scratch = browser._profile.path().to_owned();
        drop(browser);
        assert!(!scratch.exists());
        let mut restarted = MemoryBrowser::start(
            identity,
            root.path(),
            home.as_deref(),
            Instant::now() + Duration::from_secs(60),
        )
        .unwrap();
        let cookies = restarted
            .call(
                None,
                "Storage.getCookies",
                json!({"browserContextId":restarted.context}),
            )
            .unwrap();
        assert!(cookies["cookies"].as_array().unwrap().is_empty());
        let other = PageInfo {
            target: "forged".into(),
            opener: None,
            url: "https://accounts.google.com/".into(),
            context: "other".into(),
        };
        assert!(matches!(restarted.attach(&other), Err(Error::WrongContext)));
    }
}
