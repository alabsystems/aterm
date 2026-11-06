// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A human Claude sign-in in ordinary Safari. Apple Events read only the front tab's URL;
//! software does not drive page content, verification, consent, cookies or stored credentials.

use std::time::{Duration, Instant};

const TICK: Duration = Duration::from_millis(500);
const READ_WITHIN: Duration = Duration::from_secs(10);
const MAX_OUTPUT: usize = 16 * 1024;
const SAFARI: &str = "/Applications/Safari.app/Contents/MacOS/Safari";
const OPEN: &str = aterm_uds::os_path!("open");
const OSASCRIPT: &str = aterm_uds::os_path!("osascript");
const CURRENT_URL: &str = "tell application id \"com.apple.Safari\" to get URL of front document";

pub(super) fn available() -> bool {
    std::path::Path::new(SAFARI).is_file()
}

fn run(program: &str, args: &[&str], error: &str) -> Result<std::process::Output, String> {
    let mut command = aterm_uds::spawnfd::command(program);
    command
        .args(args)
        .env_clear()
        .envs(super::open_url::child_env(&|name| std::env::var_os(name)));
    let deadline = Instant::now()
        .checked_add(READ_WITHIN)
        .ok_or_else(|| error.to_owned())?;
    let output = aterm_uds::deadline::output_with_input_until(
        &mut command,
        aterm_uds::secret::Secret::new(&[]),
        deadline,
        MAX_OUTPUT,
    )
    .map_err(|_| error.to_owned())?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(error.to_owned())
    }
}

fn open(url: &str) -> Result<(), String> {
    run(
        OPEN,
        &["-a", "Safari", url],
        "the regular Safari challenge did not open",
    )?;
    Ok(())
}

fn current_url() -> Result<Option<String>, String> {
    let output = run(
        OSASCRIPT,
        &["-e", CURRENT_URL],
        "macOS did not allow aterm to read Safari's current URL",
    )?;
    let url = String::from_utf8(output.stdout)
        .map_err(|_| "Safari returned an unreadable current URL".to_owned())?;
    Ok(Some(url.trim().to_owned()))
}

fn drive_with(
    authorize_url: &str,
    flow: &super::open_url::MailboxFlow,
    timeout: Duration,
    scope: Option<&super::account_work::Scope>,
    mut open: impl FnMut(&str) -> Result<(), String>,
    mut current_url: impl FnMut() -> Result<Option<String>, String>,
    mut pause: impl FnMut(Duration),
) -> Result<String, String> {
    let end = Instant::now()
        .checked_add(timeout)
        .ok_or("the central challenge deadline is invalid")?;
    open(authorize_url)?;
    loop {
        super::challenge_window::work_current(scope)?;
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err("the central challenge was not completed in time".into());
        }
        if let Some(url) = current_url()? {
            if flow.is_callback(&url) {
                return Ok(url);
            }
            if url.starts_with("http://127.0.0.1:") || url.starts_with("http://localhost:") {
                return Err("the central challenge reached an invalid local callback".into());
            }
        }
        pause(left.min(TICK));
    }
}

pub(super) fn authorize(
    authorize_url: &str,
    flow: &super::open_url::MailboxFlow,
    timeout: Duration,
    scope: Option<&super::account_work::Scope>,
) -> Result<String, String> {
    drive_with(
        authorize_url,
        flow,
        timeout,
        scope,
        open,
        current_url,
        std::thread::sleep,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOSTED_AUTHORIZE: &str = "https://claude.com/cai/oauth/authorize?client_id=claude&redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback&state=state";
    const HOSTED_CALLBACK: &str =
        "https://platform.claude.com/oauth/code/callback?code=secret&state=state";

    #[test]
    fn safari_ignores_unrelated_tabs_and_returns_the_exact_hosted_callback() {
        let flow = super::super::open_url::MailboxFlow::new(HOSTED_AUTHORIZE).unwrap();
        let urls = [
            Some(HOSTED_AUTHORIZE),
            None,
            Some("https://example.invalid/"),
            Some(HOSTED_CALLBACK),
        ];
        let mut next = 0;
        let mut opened = Vec::new();
        let result = drive_with(
            HOSTED_AUTHORIZE,
            &flow,
            Duration::from_secs(60),
            None,
            |url| {
                opened.push(url.to_owned());
                Ok(())
            },
            || {
                let url = urls[next];
                next += 1;
                Ok(url.map(str::to_owned))
            },
            |_| {},
        );
        assert_eq!(result, Ok(HOSTED_CALLBACK.to_owned()));
        assert_eq!(opened, [HOSTED_AUTHORIZE]);
        assert_eq!(next, urls.len());
    }

    #[test]
    fn safari_refuses_an_invalid_loopback_callback() {
        let flow = super::super::open_url::MailboxFlow::new(HOSTED_AUTHORIZE).unwrap();
        let result = drive_with(
            HOSTED_AUTHORIZE,
            &flow,
            Duration::from_secs(60),
            None,
            |_| Ok(()),
            || {
                Ok(Some(
                    "http://127.0.0.1:9999/callback?code=secret&state=state".into(),
                ))
            },
            |_| {},
        );
        assert_eq!(
            result,
            Err("the central challenge reached an invalid local callback".into())
        );
    }
}
