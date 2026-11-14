// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whether another Mac is behind the aterm session this process runs in: asked of this Mac's
//! aterm instances, before a sign-in opens anything on this Mac's desk.
//!
//! A sign-in opens aterm's sign-in window, or a person's browser, only for a sign-in this Mac
//! started for itself. A renewal another Mac's request starts carries a mark in its
//! environment, and so does every tab another Mac's fleet drive starts
//! ([`super::remote_renew::FOR_PEER_ENV`]). A tab this Mac started that a fleet driver then
//! types into carries none: the instance that holds the tab knows, and says so in its `reveal`
//! answer (`fleet=yes`), which names the session by the asking process's lineage and never by
//! its environment.
//!
//! This keeps a window from opening on a desk for another Mac's driver. It is no boundary
//! against a process that sets out to open one, and an instance that cannot be asked, or one
//! from before the answer had the word, refuses nothing: the sign-in itself runs on this Mac
//! either way.

use std::time::Duration;

/// How long one instance has to answer.
const ASK_WITHIN: Duration = Duration::from_secs(5);

/// Whether any aterm instance on this Mac says another Mac is behind the session this process
/// descends from. The instance that is this process is not asked: its own work is in no tab.
pub(super) fn an_instance_says_another_mac_drives() -> bool {
    let Ok(instances) = aterm_ctl::local_instances() else {
        return false;
    };
    let own = std::process::id();
    instances
        .iter()
        .filter(|(pid, _)| *pid != own)
        .any(|(_, sock)| asked(sock).is_some_and(|lines| says_fleet(&lines)))
}

/// The `reveal` answer of the instance listening at `sock`; none where it could not be asked.
fn asked(sock: &str) -> Option<Vec<String>> {
    let (stream, token) = aterm_ctl::connect_then_read_token(sock).ok()?;
    aterm_ctl::exchange_lines(&stream, &token.ok()?, "reveal", ASK_WITHIN).ok()
}

/// Whether a `reveal` answer's `lines` say another Mac is behind the session (`fleet=yes`).
/// An answer without the word, an older instance's, says no.
pub(super) fn says_fleet(lines: &[String]) -> bool {
    lines.iter().any(|line| {
        line.split_ascii_whitespace()
            .any(|field| field == "fleet=yes")
    })
}

#[cfg(test)]
#[path = "fleet_session_tests.rs"]
mod tests;
