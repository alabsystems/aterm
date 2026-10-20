// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The open recorder: this program under the name `open`, first on the `PATH`
//! of the lab's agents (`runs/<tag>/shim/open`) and their `BROWSER`. It NEVER
//! opens anything: a URL a sign-in would open in the person's browser is
//! logged to `$LAB_OPEN_LOG` (its scheme and host only, never the query, which
//! carries a login's state and challenge) and answered 0. The jail denies
//! `/usr/bin/open` and Launch Services' open doors as well, so a direct call
//! fails rather than reaching the desktop.

use std::io::Write as _;

/// What one argument is logged as: `scheme://authority` of a URL, `<a path or
/// app>` for anything else, nothing for an option.
#[must_use]
pub fn logged(arg: &str) -> Option<String> {
    if arg.contains("://") {
        let (scheme, rest) = arg.split_once("://")?;
        let scheme_ok = scheme
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c));
        if !scheme_ok {
            return Some(arg.to_string());
        }
        let host: String = rest.chars().take_while(|c| !"/?#".contains(*c)).collect();
        return Some(format!("{scheme}://{host}"));
    }
    if arg.starts_with('-') {
        return None;
    }
    Some("<a path or app>".into())
}

/// `open …` as the recorder answers it: 0, having logged each argument.
#[must_use]
pub fn main(args: &[String]) -> i32 {
    let log = std::env::var("LAB_OPEN_LOG").unwrap_or_else(|_| "/dev/null".into());
    let ts = crate::util::hms(crate::util::now_secs());
    for a in args {
        if let Some(what) = logged(a)
            && let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log)
        {
            let _ = f.write_all(format!("{ts} open {what}\n").as_bytes());
        }
    }
    0
}

/// `aterm-lab hold <port> <secs>`: hold a connection to the stub on loopback
/// for `secs` (the selftests' orphan control: what it holds open, the witness sees).
#[must_use]
pub fn hold(args: &[String]) -> i32 {
    let (Some(port), Some(secs)) = (
        args.first().and_then(|p| p.parse::<u16>().ok()),
        args.get(1).and_then(|s| s.parse::<u64>().ok()),
    ) else {
        eprintln!("usage: aterm-lab hold <port> <secs>");
        return 2;
    };
    let Ok(_conn) = std::net::TcpStream::connect(("127.0.0.1", port)) else {
        return 1;
    };
    std::thread::sleep(std::time::Duration::from_secs(secs));
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_is_logged_by_its_scheme_and_host_never_its_query() {
        assert_eq!(
            logged("https://auth.openai.com/oauth/authorize?state=s3cret&code_challenge=x")
                .as_deref(),
            Some("https://auth.openai.com")
        );
        assert_eq!(
            logged("x-lab-control://selftest").as_deref(),
            Some("x-lab-control://selftest")
        );
        assert_eq!(
            logged("/Applications/Safari.app").as_deref(),
            Some("<a path or app>")
        );
        assert_eq!(logged("-a"), None);
    }
}
