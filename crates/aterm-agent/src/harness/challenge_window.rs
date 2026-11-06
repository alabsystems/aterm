// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fleet challenge service's visible Claude OAuth browser. It automates navigation and
//! loopback or hosted-code callback capture only: every sign-in, consent and human-verification
//! control is the person's.

use super::headless_oauth::{BrowserChild, CdpClient};
use aterm_json::json;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const TICK: Duration = Duration::from_millis(250);
const TIDY_WITHIN: Duration = Duration::from_secs(30);
const CALLBACK_URL_JS: &str = r#"(() => {
  const raw = String(window.location && window.location.href || "");
  const failed = raw.startsWith("chrome-error://")
    ? String((window.loadTimeData && window.loadTimeData.data_ && window.loadTimeData.data_.summary && window.loadTimeData.data_.summary.failedUrl) || "")
    : "";
  return failed || raw;
})()"#;

pub(super) fn work_current(scope: Option<&super::account_work::Scope>) -> Result<(), String> {
    if scope.is_some_and(super::account_work::Scope::cancelled) {
        Err("account work admissions are paused".into())
    } else {
        Ok(())
    }
}

fn callback(
    cdp: &mut CdpClient,
    session: &str,
    flow: &super::open_url::MailboxFlow,
    timeout: Duration,
) -> Result<Option<String>, String> {
    let mut intercepted = None;
    for request in cdp.take_events("Fetch.requestPaused", Some(session)) {
        let id = request["params"]["requestId"]
            .as_str()
            .ok_or("the central challenge interception had no request ID")?;
        let url = request["params"]["request"]["url"]
            .as_str()
            .ok_or("the central challenge interception had no URL")?;
        if flow.is_callback(url) {
            intercepted = Some(url.to_owned());
        }
        cdp.call(
            Some(session),
            "Fetch.failRequest",
            json!({"requestId":id,"errorReason":"BlockedByClient"}),
            timeout,
        )
        .map_err(|_| "the central challenge callback could not be blocked".to_owned())?;
    }
    if let Some(callback) = intercepted {
        return Ok(Some(callback));
    }
    let answer = cdp
        .call(
            Some(session),
            "Runtime.evaluate",
            json!({"expression":CALLBACK_URL_JS,"returnByValue":true}),
            timeout,
        )
        .map_err(|_| "the central challenge browser stopped answering".to_owned())?;
    let url = answer["result"]["value"]
        .as_str()
        .ok_or("the central challenge page could not be read")?;
    if flow.is_callback(url) {
        return Ok(Some(url.to_owned()));
    }
    if url.starts_with("http://127.0.0.1:") || url.starts_with("http://localhost:") {
        return Err("the central challenge reached an invalid local callback".into());
    }
    if url.starts_with("https://") && !flow.admits_provider_url(url) {
        return Err(format!(
            "the central challenge left its pinned Claude OAuth flow at {}: {}",
            super::headless_oauth::url_place(url),
            flow.provider_refusal(url)
                .unwrap_or_else(|| "unvalidated destination".into())
        ));
    }
    Ok(cdp.take_matching_event_url(session, |url| flow.is_callback(url)))
}

fn drive(
    cdp: &mut CdpClient,
    authorize_url: &str,
    flow: &super::open_url::MailboxFlow,
    timeout: Duration,
    scope: Option<&super::account_work::Scope>,
) -> Result<String, String> {
    work_current(scope)?;
    let end = Instant::now()
        .checked_add(timeout)
        .ok_or("the central challenge deadline is invalid")?;
    let left = || end.saturating_duration_since(Instant::now());
    let target = cdp
        .call(
            None,
            "Target.createTarget",
            json!({"url":"about:blank"}),
            left(),
        )
        .map_err(|_| "the central challenge browser created no page".to_owned())?;
    let target_id = target["targetId"]
        .as_str()
        .ok_or("the central challenge browser created no target")?;
    work_current(scope)?;
    let attached = cdp
        .call(
            None,
            "Target.attachToTarget",
            json!({"targetId":target_id,"flatten":true}),
            left(),
        )
        .map_err(|_| "the central challenge browser attached no page".to_owned())?;
    let session = attached["sessionId"]
        .as_str()
        .ok_or("the central challenge browser attached no session")?;
    for method in ["Page.enable", "Runtime.enable", "Network.enable"] {
        work_current(scope)?;
        cdp.call(Some(session), method, json!({}), left())
            .map_err(|_| "the central challenge browser could not observe its page".to_owned())?;
    }
    work_current(scope)?;
    cdp.call(
        Some(session),
        "Fetch.enable",
        json!({"patterns":[
            {"urlPattern":"http://127.0.0.1/*","resourceType":"Document","requestStage":"Request"},
            {"urlPattern":"http://localhost/*","resourceType":"Document","requestStage":"Request"}
        ]}),
        left(),
    )
    .map_err(|_| "the central challenge callback could not be contained".to_owned())?;
    work_current(scope)?;
    cdp.call(
        Some(session),
        "Page.navigate",
        json!({"url":authorize_url}),
        left(),
    )
    .map_err(|_| "the central challenge page did not open".to_owned())?;
    loop {
        work_current(scope)?;
        if left().is_zero() {
            return Err("the central challenge was not completed in time".into());
        }
        if let Some(callback) = callback(cdp, session, flow, left())? {
            return Ok(callback);
        }
        std::thread::sleep(left().min(TICK));
    }
}

fn launch(
    browser: &Path,
    profile: &Path,
    scope: &super::account_work::Scope,
) -> Result<(BrowserChild, CdpClient), String> {
    use aterm_uds::spawnfd::{ChildStdio, LaneCommand, spawn_with_two_fds};
    use std::os::fd::OwnedFd;

    let (command_read, command_write) = std::io::pipe()
        .map_err(|_| "no challenge browser command pipe could be made".to_owned())?;
    let (event_read, event_write) =
        std::io::pipe().map_err(|_| "no challenge browser event pipe could be made".to_owned())?;
    let mut args = super::headless_oauth::profile_args(profile);
    args.push("--remote-debugging-pipe".into());
    args.push("about:blank".into());
    let mut command = LaneCommand::new(browser);
    command
        .args(args)
        .env_exactly(super::open_url::child_env(&|name| std::env::var_os(name)))
        .stdin(ChildStdio::Null)
        .stdout(ChildStdio::Null)
        .stderr(ChildStdio::Null);
    work_current(Some(scope))?;
    let child = spawn_with_two_fds(
        command,
        OwnedFd::from(command_read),
        OwnedFd::from(event_write),
    )
    .map_err(|_| "the central challenge browser did not start".to_owned())?;
    let child = BrowserChild::new(child);
    let cdp = CdpClient::new_scoped(command_write, event_read, Some(scope.clone()))?;
    Ok((child, cdp))
}

/// Open `authorize_url` in the central service's visible browser and return its exact callback.
/// No OAuth form, CAPTCHA, anti-bot widget or consent button is clicked by software.
pub(super) fn authorize(
    authorize_url: &str,
    email: Option<&str>,
    timeout: Duration,
    scope: &super::account_work::Scope,
) -> Result<String, String> {
    work_current(Some(scope))?;
    if super::browser_window::no_window_desk().is_some() {
        return Err("the central challenge has no interactive desktop".into());
    }
    let flow = super::open_url::MailboxFlow::new(authorize_url)?;
    #[cfg(target_os = "macos")]
    if super::safari_window::available() {
        return super::safari_window::authorize(authorize_url, &flow, timeout, Some(scope));
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("the central challenge has no home directory")?;
    let browser = super::browser_window::find_window_browser(Some(&home))
        .ok_or("the central challenge has no interactive Chromium installed")?;
    let state =
        aterm_types::dirs::state_dir().ok_or("the central challenge has no state directory")?;
    let profile =
        super::headless_oauth::resolve_browser_profile_dir(&state.join("accounts"), None, email);
    for dir in [profile.parent(), Some(profile.as_path())]
        .into_iter()
        .flatten()
    {
        aterm_types::fs_restricted::ensure_private_dir(dir)
            .map_err(|_| "the central challenge profile could not be prepared".to_owned())?;
    }
    work_current(Some(scope))?;
    let _claimed = super::browser_window::ProfileLock::claim(&profile)
        .map_err(|_| "the central challenge profile is already in use".to_owned())?;
    super::browser_window::quiet_password_manager(&profile)
        .map_err(|_| "the central challenge password manager could not be disabled".to_owned())?;
    super::browser_window::mark_untidied(&profile)
        .map_err(|_| "the central challenge profile could not be marked".to_owned())?;

    let result = (|| {
        let (mut child, mut cdp) = launch(&browser, &profile, scope)?;
        let result = drive(&mut cdp, authorize_url, &flow, timeout, Some(scope));
        let _ = cdp.call(None, "Browser.close", json!({}), Duration::from_secs(2));
        let _ = child.close();
        result
    })();

    work_current(Some(scope))?;

    let headless = super::headless_oauth::find_headless_browser(Some(&home))
        .filter(|found| found.is_headless_shell)
        .unwrap_or_else(|| super::headless_oauth::HeadlessBrowser {
            path: browser.clone(),
            is_headless_shell: false,
        });
    let origin = super::browser_window::origin_of(authorize_url);
    let session = super::accounts::Cli::Claude
        .accounts()
        .web_session_key
        .zip(origin.as_deref())
        .map(|(key, origin)| (origin, key));
    let tidied = super::browser_window::tidy_profile_scoped(
        &headless,
        &profile,
        session,
        TIDY_WITHIN,
        Some(scope.clone()),
    );
    work_current(Some(scope))?;
    if tidied.is_ok() {
        let _ = super::browser_window::clear_untidied(&profile);
    }
    match (result, tidied) {
        (Ok(callback), Ok(_)) => {
            super::headless_oauth::adopt_browser_profile(&profile)
                .map_err(|_| "the central challenge profile could not be retained".to_owned())?;
            Ok(callback)
        }
        (Err(error), _) => Err(error),
        (Ok(_), Err(_)) => Err("the central challenge profile could not be cleaned".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read as _, Write as _};

    const AUTHORIZE: &str = "https://claude.ai/oauth/authorize?client_id=claude&redirect_uri=http%3A%2F%2F127.0.0.1%3A4321%2Fcallback&state=state";
    const CALLBACK: &str = "http://127.0.0.1:4321/callback?code=secret&state=state";
    const HOSTED_AUTHORIZE: &str = "https://claude.com/cai/oauth/authorize?client_id=claude&redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback&state=state";
    const HOSTED_CALLBACK: &str =
        "https://platform.claude.com/oauth/code/callback?code=secret&state=state";

    fn browser(url: &'static str) -> (CdpClient, std::thread::JoinHandle<()>) {
        let (mut commands, to_browser) = std::io::pipe().unwrap();
        let (from_browser, mut events) = std::io::pipe().unwrap();
        let cdp = CdpClient::new(to_browser, from_browser);
        let peer = std::thread::spawn(move || {
            let mut pending = Vec::new();
            let mut chunk = [0; 4096];
            loop {
                let count = commands.read(&mut chunk).unwrap();
                if count == 0 {
                    break;
                }
                pending.extend_from_slice(&chunk[..count]);
                for message in super::super::headless_oauth::split_nul_messages(&mut pending) {
                    let id = message["id"].as_u64().unwrap();
                    let method = message["method"].as_str().unwrap();
                    if method == "Fetch.enable" && url == CALLBACK {
                        let mut paused = aterm_json::to_vec(&json!({
                            "method":"Fetch.requestPaused","sessionId":"s1",
                            "params":{"requestId":"r1","request":{"url":url}}
                        }))
                        .unwrap();
                        paused.push(0);
                        events.write_all(&paused).unwrap();
                    }
                    let result = match method {
                        "Target.createTarget" => json!({"targetId":"t1"}),
                        "Target.attachToTarget" => json!({"sessionId":"s1"}),
                        "Runtime.evaluate" if url == CALLBACK => {
                            json!({"result":{"value":"https://example.invalid/not-the-callback"}})
                        }
                        "Runtime.evaluate" => json!({"result":{"value":url}}),
                        _ => json!({}),
                    };
                    let mut reply = aterm_json::to_vec(&json!({"id":id,"result":result})).unwrap();
                    reply.push(0);
                    events.write_all(&reply).unwrap();
                }
            }
        });
        (cdp, peer)
    }

    #[test]
    fn the_window_drive_returns_only_the_exact_callback() {
        let flow = super::super::open_url::MailboxFlow::new(AUTHORIZE).unwrap();
        let (mut cdp, peer) = browser(CALLBACK);
        assert_eq!(
            drive(&mut cdp, AUTHORIZE, &flow, Duration::from_secs(60), None),
            Ok(CALLBACK.to_owned())
        );
        drop(cdp);
        peer.join().unwrap();
    }

    #[test]
    fn the_window_drive_returns_the_exact_hosted_code_callback() {
        let flow = super::super::open_url::MailboxFlow::new(HOSTED_AUTHORIZE).unwrap();
        let (mut cdp, peer) = browser(HOSTED_CALLBACK);
        assert_eq!(
            drive(
                &mut cdp,
                HOSTED_AUTHORIZE,
                &flow,
                Duration::from_secs(60),
                None
            ),
            Ok(HOSTED_CALLBACK.to_owned())
        );
        drop(cdp);
        peer.join().unwrap();
    }

    #[test]
    fn the_window_drive_refuses_another_callback_or_origin() {
        let flow = super::super::open_url::MailboxFlow::new(AUTHORIZE).unwrap();
        for url in [
            "http://127.0.0.1:9999/callback?code=secret&state=state",
            "https://example.invalid/callback",
        ] {
            let (mut cdp, peer) = browser(url);
            assert!(drive(&mut cdp, AUTHORIZE, &flow, Duration::from_secs(60), None).is_err());
            drop(cdp);
            peer.join().unwrap();
        }
    }
}
