// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Local release-credential materialization after an authenticated vault apply.

use super::*;
use aterm_uds::spawnfd::Stdio;
use std::{io::Write, process::Output};
use zeroize::Zeroizing;

const PROFILE: &str = "notary";
const ENTRY: &str = "notary";

struct Notary {
    apple_id: Zeroizing<String>,
    team_id: Zeroizing<String>,
    password: Zeroizing<String>,
}

fn parse_notary(bytes: &[u8]) -> crate::Result<Notary> {
    let text = std::str::from_utf8(bytes).map_err(|_| Error::new("notary entry is not UTF-8"))?;
    let mut apple_id = None;
    let mut team_id = None;
    let mut password = None;
    let mut use_aterm = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| Error::new("notary entry needs name=value lines"))?;
        let key = key.trim();
        let value = value.trim();
        if value.is_empty() || value.contains(['\0', '"', '\'', '\\']) {
            return Err(Error::new("notary entry has an invalid value"));
        }
        let slot = match key {
            "apple_id" => &mut apple_id,
            "team_id" => &mut team_id,
            "password" => &mut password,
            "use" => {
                if value != "aterm" {
                    return Err(Error::new("notary entry's use must be aterm"));
                }
                use_aterm = true;
                continue;
            }
            _ => return Err(Error::new("notary entry has an unknown field")),
        };
        if slot.replace(value.to_owned()).is_some() {
            return Err(Error::new("notary entry repeats a field"));
        }
    }
    if !use_aterm {
        return Err(Error::new("notary entry needs use=aterm"));
    }
    Ok(Notary {
        apple_id: Zeroizing::new(
            apple_id.ok_or_else(|| Error::new("notary entry needs apple_id"))?,
        ),
        team_id: Zeroizing::new(team_id.ok_or_else(|| Error::new("notary entry needs team_id"))?),
        password: Zeroizing::new(
            password.ok_or_else(|| Error::new("notary entry needs password"))?,
        ),
    })
}

fn history() -> crate::Result<Output> {
    aterm_uds::spawnfd::command("/usr/bin/xcrun")
        .args(["notarytool", "history", "--keychain-profile", PROFILE])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| Error::new(format!("could not run notarytool: {error}")))
}

fn history_is_current() -> crate::Result<bool> {
    let output = history()?;
    if output.status.success() {
        return Ok(true);
    }
    let detail = String::from_utf8_lossy(&output.stderr);
    if detail.contains("No Keychain password item found") {
        return Ok(false);
    }
    Err(Error::new(
        "notarytool history did not authenticate; the existing profile was left unchanged",
    ))
}

fn store(notary: &Notary) -> crate::Result<()> {
    let mut child = aterm_uds::spawnfd::command("/usr/bin/script")
        .args([
            "-q",
            "/dev/null",
            "/usr/bin/xcrun",
            "notarytool",
            "store-credentials",
            PROFILE,
            "--apple-id",
            notary.apple_id.as_str(),
            "--team-id",
            notary.team_id.as_str(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| Error::new(format!("could not start notarytool: {error}")))?;
    let mut input = child
        .stdin
        .take()
        .ok_or_else(|| Error::new("notarytool has no password input"))?;
    input
        .write_all(notary.password.as_bytes())
        .and_then(|()| input.write_all(b"\n"))
        .map_err(|error| Error::new(format!("could not answer notarytool: {error}")))?;
    drop(input);
    let status = child
        .wait()
        .map_err(|error| Error::new(format!("could not wait for notarytool: {error}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::new("notarytool did not store the credential"))
    }
}

fn materialize_with(
    paths: &Paths,
    history: &mut dyn FnMut() -> crate::Result<bool>,
    store: &mut dyn FnMut(&Notary) -> crate::Result<()>,
) -> crate::Result<Option<&'static str>> {
    let expected =
        config(paths)?.ok_or_else(|| Error::new("no vault configured; run aterm secrets join"))?;
    let Some(entry) = read_verified_entry(paths, &expected, ENTRY)? else {
        return Ok(None);
    };
    let notary = parse_notary(entry.value().as_slice())?;
    if history()? {
        return Ok(Some("notary profile already authenticates"));
    }
    store(&notary)?;
    if history()? {
        Ok(Some("notary profile stored and live-checked"))
    } else {
        Err(Error::new(
            "notarytool stored a credential but did not authenticate on the live check",
        ))
    }
}

/// Provision the current signed `notary` entry, preserving an already working
/// profile. An absent entry changes nothing. The password goes only to the
/// PTY's stdin and never to argv, output, logs or errors.
///
/// # Errors
/// The entry is malformed, the existing profile fails for a reason other than
/// absence, or storing and live-checking the credential fails.
pub fn materialize_notary(paths: &Paths) -> crate::Result<Option<&'static str>> {
    materialize_with(paths, &mut history_is_current, &mut store)
}

#[cfg(test)]
mod tests;
