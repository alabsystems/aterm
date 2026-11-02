// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An admin Mac's record of each Mac asking to join the secrets vault that its approver leaves
//! to a person (`aterm_secrets::client::asking`): no proof of the join password, one that did
//! not match, or no home ALab LAN observation. Each one is an Info entry in the digest
//! ([`crate::join_entries`], never an alert on screen) naming the asking Mac and its code, and
//! its Approve lets exactly that Mac in (`aterm_secrets::client::approve_request`). Approve is
//! never a row's primary or default button ([`Intent::never_default`]): a person compares the
//! code with the one on the other Mac first. A key the vault let go (`code-only`) is filed the
//! same way, for an admin who compares its code (the approver's code-only standing).

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use aterm_messages::{Message, Severity, tags};
use aterm_secrets::client::Asking;

/// How often the lane reads the approver's record again: as often as the approver writes it.
const READ_EVERY: std::time::Duration = std::time::Duration::from_secs(2);

/// The record as the lane last read it.
static ASKING: Mutex<Vec<Asking>> = Mutex::new(Vec::new());

/// Set when [`ASKING`] changed; the next park reads it.
static CHANGED: AtomicBool = AtomicBool::new(false);

/// Whether the record changed since the last look ([`spawn_lane`]).
pub(crate) fn take_changed() -> bool {
    CHANGED.swap(false, Ordering::AcqRel)
}

/// The record as the lane last read it.
pub(crate) fn current() -> Vec<Asking> {
    ASKING.lock().map(|held| held.clone()).unwrap_or_default()
}

/// Start reading the approver's record: a windowed macOS instance, off the event loop, every
/// [`READ_EVERY`], each change handed to the digest at its next park ([`take_changed`]). Local
/// files only: the approver (`aterm secrets watch` on an admin Mac) reads the vault's queue.
/// Whether the thread started.
pub(crate) fn spawn_lane(headless: bool) -> bool {
    if headless || !cfg!(target_os = "macos") {
        return false;
    }
    let spawned =
        crate::qos::spawn_lane_thread("aterm-secrets-asking", crate::qos::Role::Background, || {
            let Ok(paths) = aterm_secrets::client::Paths::live() else {
                return;
            };
            loop {
                let now = aterm_secrets::client::asking(&paths);
                if let Ok(mut held) = ASKING.lock()
                    && *held != now
                {
                    *held = now;
                    CHANGED.store(true, Ordering::Release);
                }
                std::thread::sleep(READ_EVERY);
            }
        });
    match spawned {
        Ok(_) => true,
        Err(error) => {
            aterm_log::warn!("secrets vault: the join entries did not start: {error}");
            false
        }
    }
}

/// The record of an Approve that was refused, the refusal whole in its detail (the command it
/// names among it, which one clipped line would cut off): what a person who pressed it reads in
/// the log of messages, never an alert.
pub(crate) fn refused(host: Option<&str>, why: &str) -> Message {
    let title = match host {
        Some(host) => format!("Couldn't approve {}", crate::join_entries::shown_name(host)),
        None => "Couldn't approve that Mac".to_owned(),
    };
    Message::new(tags::PRIVACY, Severity::Warn, title)
        .sentence(format!("Approve did not let it in: {why}"))
        .record()
}

/// Approve exactly `recipient`, off the event loop: an admin Mac's press of an entry's Approve.
/// The entry ends once the approver's record no longer lists it. A refusal is logged and kept
/// as a record ([`refused`]).
pub(crate) fn approve(recipient: String) -> bool {
    crate::qos::spawn_lane_thread(
        "aterm-secrets-approve",
        crate::qos::Role::Responsive,
        move || {
            let paths = aterm_secrets::client::Paths::live();
            let approved = paths
                .as_ref()
                .map_err(ToString::to_string)
                .and_then(|paths| {
                    aterm_secrets::client::approve_request(paths, &recipient)
                        .map_err(|error| error.to_string())
                });
            match approved {
                Ok(host) => aterm_log::info!("secrets vault: approved {host}"),
                Err(error) => {
                    aterm_log::warn!("secrets vault: approving a Mac failed: {error}");
                    let host = paths
                        .map(|paths| aterm_secrets::client::asking(&paths))
                        .unwrap_or_default()
                        .into_iter()
                        .find(|asking| asking.recipient == recipient)
                        .map(|asking| asking.host);
                    crate::message_inbox::queue_message(refused(host.as_deref(), &error));
                }
            }
        },
    )
    .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A refused Approve says why whole, in a record and not on screen: the refusal of a key the
    /// queue lists under two names ends with the command that approves it, past one detail
    /// line's cap.
    /// NEGATIVE CONTROL: the refusal as one detail line is clipped before its command (asserted
    /// below).
    #[test]
    fn a_refused_approve_says_why_whole_in_a_record() {
        let key = "age1qyqszqgpqyqszqgpqyqszqgpqyqszqgpqyqszqgpqyqszqgpqyqs3290gq";
        let request = |host: &str| aterm_secrets::client::Request {
            recipient: key.to_string(),
            host: host.to_string(),
            code: "ABCD-EFGH-JKMN-PQRS".to_string(),
            at: None,
            home: None,
            allowed: None,
            proof: None,
        };
        let pending = [request("mc"), request(&"a-name-a-lan-host-chose".repeat(2))];
        let why = aterm_secrets::client::one_name_asked(&pending, &pending[0])
            .expect_err("a key listed under two names")
            .to_string();
        let command = format!("aterm secrets approve {key} --name <host>");
        assert!(why.ends_with(&command), "{why}");
        let said = refused(Some("mc"), &why).normalized();
        assert_eq!(said.title, "Couldn't approve mc");
        assert!(
            matches!(said.need(), aterm_messages::need::Need::Record),
            "a record, not an alert: {:?}",
            said.need()
        );
        let said = said.detail.join(" ");
        assert!(
            said.contains(&format!("Approve did not let it in: {why}")),
            "{said}"
        );
        let clipped = Message::new(tags::PRIVACY, Severity::Warn, "x")
            .line(&why)
            .normalized()
            .detail
            .join(" ");
        assert!(!clipped.contains(&command), "{clipped}");
    }
}
