// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-open-url <url>`: THE OPENER A RENEWAL'S SIGN-IN RUNS AS `BROWSER`
//! (`docs/DESIGN-claude-accounts-v2-2026-10-15.md` §5.3 step 2): the one
//! binary answers to it as an argv0 alias, so the variable names ONE
//! executable, as Claude Code needs (READ in the 2.1.300 bundle: the opener
//! is spawned with no shell, `BROWSER || "open"` and the URL as its one
//! argument).
//!
//! It opens the URL in the browser profile that holds the account's
//! claude.ai session: [`PROFILE_ENV`] names the profile by its DISPLAY name
//! (`Claude m7-a`, what Chrome shows), Chrome's own `Local State` maps that
//! name to the profile's directory ([`profile_dir_of`]), and `open -na
//! "Google Chrome" --args --profile-directory=<dir> <url>` opens it
//! ([`open_command`]).
//!
//! WITH NO PROFILE NAMED — what a renewal over SSH hands it — it opens
//! NOTHING (exit 1): the owner's browser is on the machine they are at, and
//! Claude Code calls its opener under `SSH_CONNECTION` too (MEASURED on
//! 2.1.300; with `BROWSER` unset that is `open`, a browser on this machine's
//! screen).
//!
//! It NEVER LOGS OR PRINTS THE URL (it names the account's address as its
//! `login_hint`), never reads what the page redirects to, and takes nothing
//! but an `https://` URL. ANY failure — no profile named, Chrome's state
//! unreadable, no such profile, a non-macOS machine, `open` refused — is exit
//! 1 with one line on stderr that names no URL, and Claude Code then prints
//! the URL itself (`If the browser didn't open, visit: …`) for the owner to
//! open by hand.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use aterm_json::Value;

/// The argv0 name the one binary answers as the opener, and the name of the
/// link a renewal makes to it (`<accounts store>/bin/aterm-open-url`).
pub const OPENER: &str = "aterm-open-url";

/// The variable a renewal's flow carries the profile's display name in.
pub const PROFILE_ENV: &str = "ATERM_OPEN_URL_PROFILE";

/// The most bytes of Chrome's `Local State` that are read.
pub const LOCAL_STATE_MAX: u64 = 8 * 1024 * 1024;

/// Chrome's `Local State` under `home` (macOS).
#[must_use]
pub fn local_state_path(home: &Path) -> PathBuf {
    home.join("Library")
        .join("Application Support")
        .join("Google")
        .join("Chrome")
        .join("Local State")
}

/// THE PROFILE DIRECTORY whose display name is `name` in Chrome's `Local
/// State` text (`profile.info_cache.<dir>.name`), compared exactly. `None`
/// for text that is not that document, no such profile, two profiles of the
/// one name (which one the owner meant is not known), and a directory name
/// that is not one (`Default`, `Profile 3`: no path separator, no leading
/// `-`, no control character).
#[must_use]
pub fn profile_dir_of(local_state: &str, name: &str) -> Option<String> {
    let doc: Value = aterm_json::from_str(local_state).ok()?;
    let cache = doc.get("profile")?.get("info_cache")?.as_object()?;
    let mut named = cache
        .iter()
        .filter(|(_, profile)| profile.get("name").and_then(Value::as_str) == Some(name))
        .map(|(dir, _)| dir);
    let dir = named.next()?;
    if named.next().is_some() {
        return None;
    }
    let plain = !dir.is_empty()
        && !dir.starts_with('-')
        && !dir.contains(['/', '\\'])
        && !dir.chars().any(char::is_control);
    plain.then(|| dir.clone())
}

/// `open -na "Google Chrome" --args --profile-directory=<dir> <url>`: the
/// URL in that profile, a new window if Chrome runs already. Built by the
/// workspace's own command, which hands the child its stdio and nothing else.
#[must_use]
pub fn open_command(dir: &str, url: &str) -> aterm_uds::spawnfd::Command {
    let mut cmd = aterm_uds::spawnfd::command("open");
    cmd.args(["-na", "Google Chrome", "--args"])
        .arg(format!("--profile-directory={dir}"))
        .arg(url);
    cmd
}

/// Why `url` is not opened, or `None`: only an `https://` URL with no
/// whitespace or control character in it — what Claude Code's sign-in hands
/// over, and nothing an opener should take that is not.
#[must_use]
pub fn url_refusal(url: &str) -> Option<&'static str> {
    if !url.starts_with("https://") {
        Some("it opens https:// URLs only")
    } else if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        Some("the URL holds a space or a control character")
    } else {
        None
    }
}

/// The opener's work, the machine's reads injected: `profile` is
/// [`PROFILE_ENV`]'s value, `local_state` reads Chrome's state, `run` starts
/// the command and answers whether it exited 0. Answers the exit code and,
/// for a failure, the one line said (never the URL).
pub fn open_with(
    args: &[String],
    profile: Option<&str>,
    local_state: &dyn Fn() -> Option<String>,
    run: &mut dyn FnMut(aterm_uds::spawnfd::Command) -> bool,
) -> (u8, Option<String>) {
    let [url] = args else {
        return (2, Some(format!("{OPENER}: takes one URL")));
    };
    if let Some(why) = url_refusal(url) {
        return (1, Some(format!("{OPENER}: not opened: {why}")));
    }
    let Some(profile) = profile.filter(|p| !p.trim().is_empty()) else {
        return (
            1,
            Some(format!("{OPENER}: no profile named ({PROFILE_ENV})")),
        );
    };
    if !cfg!(target_os = "macos") {
        return (
            1,
            Some(format!("{OPENER}: opens a Chrome profile on macOS only")),
        );
    }
    let Some(text) = local_state() else {
        return (
            1,
            Some(format!("{OPENER}: Chrome's profile list cannot be read")),
        );
    };
    let Some(dir) = profile_dir_of(&text, profile) else {
        return (
            1,
            Some(format!(
                "{OPENER}: Chrome has no one profile named {}",
                super::one_line(profile, 64)
            )),
        );
    };
    if run(open_command(&dir, url)) {
        (0, None)
    } else {
        (1, Some(format!("{OPENER}: `open` did not open Chrome")))
    }
}

/// `aterm-open-url <url>` (the argv0 alias, `crates/aterm`'s router): the
/// URL in [`PROFILE_ENV`]'s profile ([`open_with`] over this machine).
#[must_use]
pub fn main_entry(args: Vec<OsString>) -> ExitCode {
    let args: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let profile = std::env::var(PROFILE_ENV).ok();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let local_state = || {
        use std::io::Read as _;
        let path = local_state_path(home.as_deref()?);
        let mut text = String::new();
        std::fs::File::open(path)
            .ok()?
            .take(LOCAL_STATE_MAX)
            .read_to_string(&mut text)
            .ok()?;
        Some(text)
    };
    let mut run = |mut cmd: aterm_uds::spawnfd::Command| cmd.status().is_ok_and(|s| s.success());
    let (code, said) = open_with(&args, profile.as_deref(), &local_state, &mut run);
    if let Some(said) = said {
        eprintln!("{said}");
    }
    ExitCode::from(code)
}

#[cfg(test)]
#[path = "open_url_tests.rs"]
mod tests;
