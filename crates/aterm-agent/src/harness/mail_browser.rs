// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//! Headless browser mailbox transport, using aterm's private CDP pipe and profile.

#[cfg(unix)]
mod owner;

/// A mailbox setup failure, retaining classified read failures across construction.
/// Opaque browser and local setup errors carry no permission to retry.
#[cfg(unix)]
#[derive(Debug)]
pub struct MailboxOpenError {
    pub(crate) message: String,
    pub(crate) proton_failure: Option<aterm_mail::proton_error::Failure>,
}

#[cfg(unix)]
impl From<String> for MailboxOpenError {
    fn from(message: String) -> Self {
        Self {
            message,
            proton_failure: None,
        }
    }
}

#[cfg(unix)]
impl From<&str> for MailboxOpenError {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}

#[cfg(unix)]
impl std::fmt::Display for MailboxOpenError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

#[cfg(unix)]
impl std::error::Error for MailboxOpenError {}

#[cfg(unix)]
mod native {
    use super::super::{browser_window::ProfileLock, headless_fetch, headless_oauth as browser};
    use super::MailboxOpenError;
    use aterm_json::{Value, json};
    use aterm_mail::Secret;
    use std::{
        collections::BTreeMap,
        os::fd::OwnedFd,
        path::Path,
        time::{Duration, Instant},
    };

    /// Hang detector for a required browser command within its original parent.
    const COMMAND_HANG: Duration = Duration::from_secs(60);

    fn command_remaining(end: Option<Instant>, started: Instant) -> Result<Duration, String> {
        let by = started + COMMAND_HANG;
        operation_remaining(Some(end.map_or(by, |end| end.min(by))), COMMAND_HANG)
    }
    /// Hang detector for a required page response within its original operation.
    const PAGE_REQUEST_HANG: Duration = Duration::from_secs(60);

    fn page_request_remaining(end: Option<Instant>, started: Instant) -> Result<Duration, String> {
        let own = started + PAGE_REQUEST_HANG;
        operation_remaining(Some(end.map_or(own, |end| end.min(own))), PAGE_REQUEST_HANG)
    }

    fn mailbox_key_script(local: &str, headers: &str, timeout_ms: u128) -> String {
        format!(
            r#"(async () => {{
                const local={local};
                if(local===undefined) throw new Error('No account route');
                const session=JSON.parse(localStorage.getItem('ps-'+local) || 'null');
                if(!session?.UID || !session.blob) throw new Error('No persisted session');
                const headers={headers};
                if(headers['x-pm-uid'] && session.UID!==headers['x-pm-uid']) throw new Error('Different session');
                headers['x-pm-uid']=session.UID;
                const response=await fetch('/api/auth/v4/sessions/local/key',{{headers,credentials:'include',redirect:'error',signal:AbortSignal.timeout({timeout_ms})}});
                if(!response.ok) throw new Error('Session key unavailable');
                const result=await response.json();
                const bytes=value=>Uint8Array.from(atob(value),c=>c.charCodeAt(0));
                const raw=bytes(result.ClientKey), blob=bytes(session.blob);
                if(raw.length!==32 || blob.length>65536) throw new Error('Invalid session data');
                const version=session.payloadVersion || 1;
                if(![1,2,3].includes(version)) throw new Error('Unknown session version');
                const n=version===3?12:16;
                const key=await crypto.subtle.importKey('raw',raw,'AES-GCM',false,['decrypt']);
                const options={{name:'AES-GCM',iv:blob.slice(0,n)}};
                if(version>=2) options.additionalData=new TextEncoder().encode('session');
                const decrypted=await crypto.subtle.decrypt(options,key,blob.slice(n));
                const payload=JSON.parse(new TextDecoder().decode(decrypted));
                if(typeof payload.keyPassword!=='string' || payload.keyPassword.length>4096) throw new Error('Invalid mailbox key');
                return payload.keyPassword;
            }})()"#
        )
    }

    fn api_request_script(
        url: &str,
        method: &str,
        body: &str,
        headers: &str,
        timeout_ms: u128,
    ) -> String {
        format!(
            r#"(async () => {{
              const signal=AbortSignal.timeout({timeout_ms});
              const target=new URL({url});
              if(target.origin!=='https://mail.proton.me' || !/^\/api\/(core|mail)\/v4\//.test(target.pathname)) throw new Error('Invalid mail API URL');
              const response=await fetch(target.href, {{method:{method},body:{body},headers:{headers},credentials:'include',cache:'no-store',redirect:'error',signal}});
              const reader=response.body?.getReader(), decoder=new TextDecoder();
              let bytes=0,text='';
              if(reader) while(true) {{
                const chunk=await reader.read();
                if(chunk.done) break;
                bytes+=chunk.value.byteLength;
                if(bytes>33554432) {{await reader.cancel();throw new Error('Mail response exceeds limit');}}
                text+=decoder.decode(chunk.value,{{stream:true}});
              }}
              text+=decoder.decode();
              return {{status:response.status,retryAfter:response.headers.get('Retry-After'),text}};
            }})()"#
        )
    }

    const SNAPSHOT: &str = r#"(() => {
      const buttons=Array.from(document.querySelectorAll('button'));
      const profile=document.title.includes('@') ? document.title : buttons.map(b=>b.getAttribute('aria-label') || '').find(s=>s.includes('@')) || '';
      const ready=!!document.querySelector('[data-testid="sidebar:compose"]') || buttons.some(b=>/New message|Compose/.test(b.getAttribute('aria-label') || b.innerText));
      let gate='';
      if(!ready) {
        const text=String(document.body?.innerText || '');
        gate=/incorrect password|invalid credentials/i.test(text)?'credentials':
          /verify.{0,20}(identity|account)|security check|human verification/i.test(text)?'verification':
          Array.from(document.querySelectorAll('iframe[src*="captcha"], [data-testid*="captcha"]')).some(n=>n.getBoundingClientRect().width>100 && n.getBoundingClientRect().height>40)?'captcha':'';
      }
      return {url:location.href,profile,ready,gate};
    })()"#;

    pub struct Credentials<'a> {
        pub email: &'a str,
        pub login: &'a str,
        pub password: &'a str,
        pub mailbox_password: Option<&'a str>,
        pub totp_secret: Option<&'a str>,
    }

    #[derive(Clone)]
    pub(super) struct StoredCredentials {
        email: String,
        login: String,
        password: Secret,
        mailbox_password: Option<Secret>,
        totp_secret: Option<Secret>,
    }

    impl StoredCredentials {
        pub(super) fn borrowed(&self) -> Credentials<'_> {
            Credentials {
                email: &self.email,
                login: &self.login,
                password: self.password.expose(),
                mailbox_password: self.mailbox_password.as_ref().map(Secret::expose),
                totp_secret: self.totp_secret.as_ref().map(Secret::expose),
            }
        }
    }

    impl From<Credentials<'_>> for StoredCredentials {
        fn from(value: Credentials<'_>) -> Self {
            Self {
                email: value.email.into(),
                login: value.login.into(),
                password: Secret::new(value.password.into()),
                mailbox_password: value.mailbox_password.map(|s| Secret::new(s.into())),
                totp_secret: value.totp_secret.map(|s| Secret::new(s.into())),
            }
        }
    }

    /// Native readers share a process-local cache and one cross-process browser owner.
    pub type SharedMailbox = std::sync::Arc<std::sync::Mutex<aterm_mail::proton::Proton>>;
    type Slot = std::sync::Arc<
        std::sync::Mutex<std::sync::Weak<std::sync::Mutex<aterm_mail::proton::Proton>>>,
    >;

    /// Remaining operation budget capped for one bounded step.
    pub fn operation_remaining(end: Option<Instant>, cap: Duration) -> Result<Duration, String> {
        let left = end.map_or(cap, |end| {
            end.saturating_duration_since(Instant::now()).min(cap)
        });
        if left.is_zero() {
            Err("Mail operation deadline expired".into())
        } else {
            Ok(left)
        }
    }

    pub fn shared_mailbox(
        home: &Path,
        credentials: Credentials<'_>,
    ) -> Result<SharedMailbox, MailboxOpenError> {
        shared_mailbox_until(home, credentials, None)
    }

    pub fn shared_mailbox_until(
        home: &Path,
        credentials: Credentials<'_>,
        end: Option<Instant>,
    ) -> Result<SharedMailbox, MailboxOpenError> {
        static SLOTS: std::sync::OnceLock<std::sync::Mutex<BTreeMap<std::path::PathBuf, Slot>>> =
            std::sync::OnceLock::new();
        let profile = HeadlessMail::profile_path(home, credentials.email);
        let slot = {
            let registry = SLOTS.get_or_init(Default::default);
            let mut slots = loop {
                let pause = operation_remaining(end, Duration::from_millis(10))?;
                match registry.try_lock() {
                    Ok(slots) => break slots,
                    Err(std::sync::TryLockError::Poisoned(_)) => {
                        return Err("Mail registry lock failed".into());
                    }
                    Err(std::sync::TryLockError::WouldBlock) => std::thread::sleep(pause),
                }
            };
            slots.retain(|_, slot| {
                std::sync::Arc::strong_count(slot) > 1
                    || slot
                        .try_lock()
                        .map_or(true, |session| session.strong_count() != 0)
            });
            slots.entry(profile).or_default().clone()
        };
        let mut slot = loop {
            let pause = operation_remaining(end, Duration::from_millis(10))?;
            match slot.try_lock() {
                Ok(slot) => break slot,
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    return Err("Mail account lock failed".into());
                }
                Err(std::sync::TryLockError::WouldBlock) => std::thread::sleep(pause),
            }
        };
        if let Some(mailbox) = slot.upgrade() {
            return Ok(mailbox);
        }
        // Unbounded native Mail startup retains its existing browser setup;
        // bounded CLI requests only use a browser already installed.
        if end.is_none() {
            headless_fetch::ensure_headless_browser(
                Some(home),
                &mut headless_fetch::Curl,
                &|| false,
            )?;
        }
        let mailbox = super::owner::mailbox(home, credentials, end)?;
        let mailbox = std::sync::Arc::new(std::sync::Mutex::new(mailbox));
        *slot = std::sync::Arc::downgrade(&mailbox);
        Ok(mailbox)
    }

    pub struct HeadlessMail {
        cdp: browser::CdpClient,
        child: browser::BrowserChild,
        _profile: ProfileLock,
        session: String,
        local_id: String,
        profile_path: std::path::PathBuf,
        home: std::path::PathBuf,
        credentials: Option<StoredCredentials>,
        api_headers: BTreeMap<String, Secret>,
        operation_deadline: Option<Instant>,
    }

    impl HeadlessMail {
        pub fn connect(home: &Path, email: &str) -> Result<Self, String> {
            Self::connect_with_progress(home, email, |_| {})
        }

        pub fn connect_with_progress(
            home: &Path,
            email: &str,
            progress: impl FnMut(&str),
        ) -> Result<Self, String> {
            let stored = Self::stored_credentials(
                browser::discover_mailbox_config(Some(home), None),
                email,
            )?;
            Self::connect_with_credentials(home, stored.borrowed(), progress)
        }

        /// What `email`'s mailbox signs in with, from a discovered configuration: its login,
        /// password, second password and TOTP seed, the lab registry's first
        /// (`MailboxConfig`, one-mesh design step A9), else the mailbox files'.
        pub(super) fn stored_credentials(
            config: Option<browser::MailboxConfig>,
            email: &str,
        ) -> Result<StoredCredentials, String> {
            let config = config.ok_or("No stored mail credentials")?;
            let login = config
                .webmail_user_for(Some(email))
                .ok_or("No mail login")?;
            let password = config
                .password_for(Some(email))
                .ok_or("No stored mail password")?;
            let mailbox = config.mailbox_password_for(Some(email));
            let totp = config.totp_secret_for(Some(email));
            Ok(StoredCredentials::from(Credentials {
                email,
                login: &login,
                password: &password,
                mailbox_password: mailbox.as_deref(),
                totp_secret: totp.as_deref(),
            }))
        }

        pub fn has_session(home: &Path, email: &str) -> bool {
            Self::profile_path(home, email)
                .join("mail-session-ready")
                .is_file()
        }

        fn profile_path(home: &Path, email: &str) -> std::path::PathBuf {
            home.join(".config/aterm/mail/browser")
                .join(digest(email.trim().to_ascii_lowercase().as_bytes()))
        }

        pub fn connect_with_credentials(
            home: &Path,
            credentials: Credentials<'_>,
            progress: impl FnMut(&str),
        ) -> Result<Self, String> {
            Self::connect_until(home, credentials, progress, None)
        }

        fn within(&self, cap: Duration) -> Result<Duration, String> {
            operation_remaining(self.operation_deadline, cap)
        }

        fn command_wait(&self, started: Instant) -> Result<Duration, String> {
            command_remaining(self.operation_deadline, started)
        }

        pub(super) fn connect_until(
            home: &Path,
            credentials: Credentials<'_>,
            progress: impl FnMut(&str),
            end: Option<Instant>,
        ) -> Result<Self, String> {
            if end.is_some_and(|end| Instant::now() >= end) {
                return Err("Mail operation deadline expired".into());
            }
            let profile = Self::profile_path(home, credentials.email);
            let executable = if end.is_some() {
                browser::find_headless_browser(Some(home))
                    .ok_or("No headless browser installed for bounded mail connection")?
            } else {
                headless_fetch::ensure_headless_browser(
                    Some(home),
                    &mut headless_fetch::Curl,
                    &|| false,
                )?
            };
            let mut result = Self::launch(home, &profile, executable, end)?;
            result.credentials = Some(credentials.into());
            result.navigate("https://mail.proton.me/")?;
            result.authenticate(progress)?;
            Ok(result)
        }

        fn authenticate(&mut self, mut progress: impl FnMut(&str)) -> Result<(), String> {
            let credentials = self
                .credentials
                .clone()
                .ok_or("No browser mailbox credentials")?;
            let email = credentials.email.as_str();
            let login = credentials.login.as_str();
            let password = credentials.password.expose();
            let mailbox = credentials.mailbox_password.as_ref().map(Secret::expose);
            let totp_secret = credentials.totp_secret.as_ref().map(Secret::expose);
            let mut submitted = (false, false, false, false);
            let mut last_state = String::new();
            let deadline = Instant::now() + self.within(Duration::from_secs(60))?;
            loop {
                if Instant::now() >= deadline {
                    return Err("Headless Proton sign-in did not reach the inbox".into());
                }
                let snapshot = self.evaluate(SNAPSHOT)?;
                if snapshot.is_null() {
                    std::thread::sleep(self.within(Duration::from_millis(250))?);
                    continue;
                }
                let page_url = snapshot["url"].as_str().unwrap_or("");
                if !page_url.starts_with("https://mail.proton.me/")
                    && !page_url.starts_with("https://account.proton.me/")
                    && page_url != "about:blank"
                {
                    return Err(
                        "Refusing to submit Proton credentials outside Proton's sign-in origins"
                            .into(),
                    );
                }
                if snapshot["ready"].as_bool() == Some(true) {
                    let identity = snapshot["profile"].as_str().unwrap_or("");
                    if !identity_matches(email, identity) && identity.contains('@') {
                        return Err(
                            "Headless browser is signed into a different or unidentified mailbox"
                                .into(),
                        );
                    }
                    if identity_matches(email, identity) {
                        let url = snapshot["url"].as_str().ok_or("No mailbox URL")?;
                        let tail = url
                            .strip_prefix("https://mail.proton.me/u/")
                            .ok_or("Unexpected mailbox origin")?;
                        let account = tail
                            .split('/')
                            .next()
                            .filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
                            .ok_or("Invalid mailbox route")?;
                        self.local_id = account.into();
                        let marker = self.profile_path.join("mail-session-ready");
                        if !marker.is_file() {
                            crate::harness::atomic_write::write_atomic(&marker, "1")
                                .map_err(|_| "Could not save browser session readiness")?;
                        }
                        progress("Headless mailbox identity verified");
                        return Ok(());
                    }
                }
                if snapshot["ready"].as_bool() != Some(true) {
                    match snapshot["gate"].as_str() {
                        Some("credentials") => {
                            return Err("Proton rejected the headless browser credentials".into());
                        }
                        Some("verification" | "captcha") => {
                            return Err(
                                "Proton requested additional verification in the headless browser"
                                    .into(),
                            );
                        }
                        _ => {}
                    }
                }
                let now = aterm_time::unix_secs().ok_or("Invalid system clock")?;
                let totp = totp_secret.and_then(|secret| browser::generate_totp_at(secret, now));
                let script = browser::webmail_login_js(
                    (Some(login), Some(password), mailbox, totp.as_deref()),
                    submitted,
                );
                let state = self.evaluate(&script)?;
                let phase = state["state"].as_str().unwrap_or("loading");
                if phase != last_state {
                    progress(match phase {
                        "submitted_user" => "Headless username submitted",
                        "submitted_login" => "Headless password submitted",
                        "submitted_totp" => "Headless second factor submitted",
                        "submitted_mailbox_password" => "Headless mailbox unlocked",
                        _ => "Waiting for headless mailbox",
                    });
                    last_state = phase.into();
                }
                match state["state"].as_str() {
                    Some("submitted_user") => submitted.0 = true,
                    Some("submitted_login") => submitted.1 = true,
                    Some("submitted_totp") => submitted.2 = true,
                    Some("submitted_mailbox_password") => submitted.3 = true,
                    Some("needs_totp") => {
                        return Err("Headless mailbox requires an unavailable second factor".into());
                    }
                    Some("needs_mailbox_password") => {
                        return Err(
                            "Headless mailbox requires an unavailable second password".into()
                        );
                    }
                    _ => {}
                }
                std::thread::sleep(self.within(Duration::from_millis(250))?);
            }
        }

        fn launch(
            home: &Path,
            profile: &Path,
            executable: browser::HeadlessBrowser,
            end: Option<Instant>,
        ) -> Result<Self, String> {
            aterm_types::fs_restricted::ensure_private_dir(profile)
                .map_err(|_| "Could not create private mail browser profile")?;
            let lock = ProfileLock::claim(profile).map_err(|error| match error {
                super::super::browser_window::Refusal::Held(_) => {
                    "The mail browser profile is already in use"
                }
                _ => "Could not lock the private mail browser profile",
            })?;
            let (command_read, command_write) =
                aterm_uds::pipe().map_err(|_| "Could not create browser command pipe")?;
            let (event_read, event_write) =
                aterm_uds::pipe().map_err(|_| "Could not create browser event pipe")?;
            let command = browser::headless_browser_command(&executable, profile, &|key| {
                std::env::var_os(key)
            });
            let child = aterm_uds::spawnfd::spawn_with_two_fds(
                command,
                OwnedFd::from(command_read),
                OwnedFd::from(event_write),
            )
            .map_err(|_| "Could not start headless mail browser")?;
            let mut result = Self {
                cdp: browser::CdpClient::new(command_write, event_read),
                child: browser::BrowserChild::new(child),
                _profile: lock,
                session: String::new(),
                local_id: String::new(),
                profile_path: profile.to_path_buf(),
                home: home.to_path_buf(),
                credentials: None,
                api_headers: BTreeMap::new(),
                operation_deadline: end,
            };
            let target = result.cdp.call(
                None,
                "Target.createTarget",
                json!({"url":"about:blank"}),
                result.command_wait(Instant::now())?,
            )?;
            let target = target["targetId"].as_str().ok_or("No browser page")?;
            let attached = result.cdp.call(
                None,
                "Target.attachToTarget",
                json!({"targetId":target,"flatten":true}),
                result.command_wait(Instant::now())?,
            )?;
            result.session = attached["sessionId"]
                .as_str()
                .ok_or("No browser session")?
                .into();
            result.cdp.call(
                Some(&result.session),
                "Page.enable",
                json!({}),
                result.command_wait(Instant::now())?,
            )?;
            result.cdp.call(
                Some(&result.session),
                "Runtime.enable",
                json!({}),
                result.command_wait(Instant::now())?,
            )?;
            result.cdp.call(
                Some(&result.session),
                "Network.enable",
                json!({}),
                result.command_wait(Instant::now())?,
            )?;
            result.cdp.call(
                Some(&result.session),
                "Fetch.enable",
                json!({"patterns":[
                    {"urlPattern":"*","resourceType":"Image","requestStage":"Request"},
                    {"urlPattern":"*","resourceType":"Media","requestStage":"Request"}
                ]}),
                result.command_wait(Instant::now())?,
            )?;
            Ok(result)
        }

        fn navigate(&mut self, url: &str) -> Result<(), String> {
            self.cdp
                .call(
                    Some(&self.session),
                    "Page.navigate",
                    json!({"url":url}),
                    self.command_wait(Instant::now())?,
                )
                .map(|_| ())
                .map_err(|error| format!("Opening mail page: {error}"))
        }
        fn evaluate(&mut self, script: &str) -> Result<Value, String> {
            let answer = match self.cdp.call(
                Some(&self.session),
                "Runtime.evaluate",
                json!({"expression":script,"awaitPromise":true,"returnByValue":true}),
                self.command_wait(Instant::now())?,
            ) {
                Ok(answer) => answer,
                // The outer operation polls the new document within its existing
                // deadline. Never immediately replay a possibly submitted form.
                Err("browser execution context changed") => Value::Null,
                Err(error) => return Err(format!("Reading mail page: {error}")),
            };
            if answer.get("exceptionDetails").is_some() {
                return Err("Headless mail page operation failed".into());
            }
            let deadline = Instant::now() + self.command_wait(Instant::now())?;
            loop {
                let requests = self
                    .cdp
                    .take_events("Fetch.requestPaused", Some(&self.session));
                if requests.is_empty() {
                    break;
                }
                for request in requests {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Err("Timed out blocking remote mail images".into());
                    }
                    let id = request["params"]["requestId"]
                        .as_str()
                        .ok_or("Invalid intercepted image request")?;
                    self.cdp
                        .call(
                            Some(&self.session),
                            "Fetch.failRequest",
                            json!({"requestId":id,"errorReason":"BlockedByClient"}),
                            remaining,
                        )
                        .map_err(|error| format!("Blocking remote mail media: {error}"))?;
                }
            }
            for event in self
                .cdp
                .take_events("Network.requestWillBeSent", Some(&self.session))
            {
                let request = &event["params"]["request"];
                if !request["url"]
                    .as_str()
                    .is_some_and(|url| url.starts_with("https://mail.proton.me/api/"))
                {
                    continue;
                }
                if let Some(headers) = request["headers"].as_object() {
                    for (name, value) in headers {
                        let name = name.to_ascii_lowercase();
                        if matches!(
                            name.as_str(),
                            "authorization" | "x-pm-uid" | "x-pm-appversion" | "x-pm-client-uid"
                        ) && let Some(value) = value.as_str()
                        {
                            self.api_headers.insert(name, Secret::new(value.into()));
                        }
                    }
                }
            }
            let _ = self.cdp.take_matching_event_url(&self.session, |_| false);
            Ok(answer["result"]["value"].clone())
        }

        /// Resume the derived mailbox secret belonging to this private browser session.
        pub fn mailbox_key_password(&mut self) -> Result<Secret, String> {
            let started = Instant::now();
            self.authenticate(|_| {})?;
            let headers: BTreeMap<_, _> = self
                .api_headers
                .iter()
                .map(|(key, value)| (key.clone(), value.expose().to_owned()))
                .collect();
            let headers = aterm_json::to_string(&headers).map_err(|_| "Invalid session headers")?;
            let local =
                aterm_json::to_string(&self.local_id).map_err(|_| "Invalid account route")?;
            let timeout_ms = page_request_remaining(self.operation_deadline, started)?
                .as_millis()
                .max(1);
            let value = self.evaluate(&mailbox_key_script(&local, &headers, timeout_ms))?;
            match value {
                Value::String(password) if !password.is_empty() => Ok(Secret::new(password)),
                _ => Err("No derived mailbox key in authenticated session".into()),
            }
        }

        fn http_request(
            &mut self,
            method: aterm_http::Method,
            path: &str,
            body: Option<Value>,
        ) -> Result<aterm_mail::proton::ApiResponse, String> {
            let started = Instant::now();
            if (!path.starts_with("/core/v4/") && !path.starts_with("/mail/v4/"))
                || path.contains("..")
                || path.contains('#')
                || path.contains('\\')
                || path.chars().any(char::is_control)
            {
                return Err("Invalid mail API path".into());
            }
            self.authenticate(|_| {})?;
            let mut headers: BTreeMap<_, _> = self
                .api_headers
                .iter()
                .map(|(key, value)| (key.clone(), value.expose().to_owned()))
                .collect();
            headers.insert("accept".into(), "application/json".into());
            headers.insert("content-type".into(), "application/json".into());
            let headers = aterm_json::to_string(&headers).map_err(|_| "Invalid API headers")?;
            let url = aterm_json::to_string(&format!("https://mail.proton.me/api{path}"))
                .map_err(|_| "Invalid API path")?;
            let method =
                aterm_json::to_string(method.as_str()).map_err(|_| "Invalid API method")?;
            let body = body
                .map(|body| aterm_json::to_string(&body))
                .transpose()
                .map_err(|_| "Invalid API body")?;
            let body = body
                .map(|body| aterm_json::to_string(&body))
                .transpose()
                .map_err(|_| "Invalid API body")?
                .unwrap_or_else(|| "undefined".into());
            let timeout_ms = page_request_remaining(self.operation_deadline, started)?
                .as_millis()
                .max(1);
            let response = self.evaluate(&api_request_script(
                &url, &method, &body, &headers, timeout_ms,
            ))?;
            let status = response["status"]
                .as_u64()
                .and_then(|n| u16::try_from(n).ok())
                .ok_or("Mail API request interrupted; outcome is unknown")?;
            Ok(aterm_mail::proton::ApiResponse {
                status,
                retry_after: response["retryAfter"].as_str().map(str::to_owned),
                body: response["text"]
                    .as_str()
                    .ok_or("No mail API response body")?
                    .as_bytes()
                    .to_vec(),
            })
        }

        /// Keep authentication in this session and all mail operations in the native API client.
        pub fn into_mailbox(mut self) -> Result<aterm_mail::proton::Proton, String> {
            let email = self
                .credentials
                .as_ref()
                .ok_or("No mailbox credentials")?
                .email
                .clone();
            let password = self.mailbox_key_password()?;
            let transport = BrowserTransport::new(self)?;
            let end = transport.operation_deadline;
            let mut mailbox = aterm_mail::proton::Proton::with_transport(Box::new(transport));
            let unlock = |mailbox: &mut aterm_mail::proton::Proton| {
                if !mailbox.has_address(&email)? {
                    return Err("API session belongs to a different mailbox".into());
                }
                mailbox.unlock_mailbox_with_key_password(&password)
            };
            if let Some(end) = end {
                mailbox.run_until(end, unlock)?;
            } else {
                unlock(&mut mailbox)?;
            }
            Ok(mailbox)
        }
    }

    pub(super) struct BrowserTransport {
        home: std::path::PathBuf,
        credentials: StoredCredentials,
        session: Option<HeadlessMail>,
        operation_deadline: Option<Instant>,
    }

    impl BrowserTransport {
        pub(super) fn new(session: HeadlessMail) -> Result<Self, String> {
            Ok(Self {
                home: session.home.clone(),
                credentials: session
                    .credentials
                    .clone()
                    .ok_or("No browser credentials")?,
                operation_deadline: session.operation_deadline,
                session: Some(session),
            })
        }

        pub(super) fn session(&mut self) -> Result<&mut HeadlessMail, String> {
            if self.session.is_none() {
                self.session = Some(HeadlessMail::connect_until(
                    &self.home,
                    self.credentials.borrowed(),
                    |_| {},
                    self.operation_deadline,
                )?);
            }
            if let Some(session) = self.session.as_mut() {
                session.operation_deadline = self.operation_deadline;
            }
            self.session
                .as_mut()
                .ok_or("Mail session unavailable".into())
        }
    }

    fn disconnected(error: &str) -> bool {
        error.contains("timed out waiting for browser DevTools response")
            || error.contains("DevTools pipe")
            || error.contains("DevTools session closed")
            || error.contains("mail API request interrupted")
            || error.contains("Mail API request interrupted")
    }

    pub(super) fn transport_error(message: String) -> aterm_mail::proton::ApiError {
        use aterm_mail::proton_error::Kind;
        let kind = if message.contains("rejected the headless browser credentials") {
            Kind::Credentials
        } else if message.contains("verification") || message.contains("second factor") {
            Kind::Verification
        } else if message == "Mail operation deadline expired"
            || disconnected(&message)
            || message == "Headless mail page operation failed"
            || message.contains("did not reach the inbox")
        {
            Kind::Network
        } else {
            Kind::Refused
        };
        aterm_mail::proton::ApiError { kind, message }
    }

    impl aterm_mail::proton::ApiTransport for BrowserTransport {
        fn set_deadline(&mut self, end: Option<Instant>) {
            self.operation_deadline = end;
            if let Some(session) = self.session.as_mut() {
                session.operation_deadline = end;
            }
        }
        fn request(
            &mut self,
            method: aterm_http::Method,
            path: &str,
            body: Option<Value>,
        ) -> Result<aterm_mail::proton::ApiResponse, aterm_mail::proton::ApiError> {
            let run = || -> Result<aterm_mail::proton::ApiResponse, String> {
                let read = matches!(method, aterm_http::Method::Get | aterm_http::Method::Head);
                let retry_body = if read { body.clone() } else { None };
                let result = self.session()?.http_request(method, path, body);
                if result.as_ref().is_err_and(|error| disconnected(error)) {
                    // Drop the dead process and its profile lock before reconnecting.
                    self.session.take();
                    if read {
                        return self.session()?.http_request(method, path, retry_body);
                    }
                }
                result
            };
            run().map_err(transport_error)
        }

        fn renew(&mut self) -> Result<(), aterm_mail::proton::ApiError> {
            let mut run = || -> Result<(), String> {
                let session = self.session()?;
                session.api_headers.clear();
                session.navigate("https://mail.proton.me/")?;
                session.authenticate(|_| {})
            };
            run().map_err(transport_error)
        }
    }

    impl Drop for HeadlessMail {
        fn drop(&mut self) {
            let _ = self
                .cdp
                .call(None, "Browser.close", json!({}), Duration::from_secs(2));
            let _ = self.child.close();
        }
    }
    pub(super) fn digest(bytes: &[u8]) -> String {
        aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(bytes))
    }

    fn identity_matches(email: &str, profile: &str) -> bool {
        let expected = email.to_ascii_lowercase();
        let profile = profile.to_ascii_lowercase();
        let Some((local, domain)) = expected.split_once('@') else {
            return false;
        };
        let domains = ["proton.me", "protonmail.com", "protonmail.ch", "pm.me"];
        profile
            .split(|c: char| !(c.is_ascii_alphanumeric() || "@._+-".contains(c)))
            .any(|candidate| {
                candidate == expected
                    || domains.contains(&domain)
                        && domains
                            .iter()
                            .any(|domain| candidate == format!("{local}@{domain}"))
            })
    }

    #[cfg(all(test, target_os = "macos"))]
    mod page_wait_tests;

    #[cfg(test)]
    mod tests;
}

#[cfg(unix)]
pub use native::{
    Credentials, HeadlessMail, SharedMailbox, operation_remaining, shared_mailbox,
    shared_mailbox_until,
};

/// Internal detached owner entry, reached only by the aterm executable.
#[cfg(unix)]
pub(crate) fn owner_main(home: &std::path::Path, account: &str) -> Result<(), String> {
    owner::serve(home, account)
}
