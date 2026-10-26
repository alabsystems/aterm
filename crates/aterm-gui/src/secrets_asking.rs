// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An admin Mac's notice of each Mac asking to join the secrets vault that its approver leaves
//! to a person (`aterm_secrets::client::asking`): no proof of the join password, one that did
//! not match, or a `code-only` key. Each notice names the asking Mac and its code, and its
//! Approve lets exactly that Mac in (`aterm_secrets::client::approve_request`). Approve is never
//! a row's primary or default button ([`Intent::never_default`]): a person compares the code
//! with the one on the other Mac first.

use aterm_messages::need::{Ask, Reason, StandingWhy, Then};
use aterm_messages::{Intent, Message, Severity, tags};
use aterm_secrets::client::Asking;

/// The key every notice of a Mac asking to join starts with.
pub(crate) const KEY_PREFIX: &str = "secrets.asking.";

/// How often the lane reads the approver's record again: as often as the approver writes it.
const READ_EVERY: std::time::Duration = std::time::Duration::from_secs(2);

/// The notice of `asking`'s key.
pub(crate) fn key_of(asking: &Asking) -> String {
    format!("{KEY_PREFIX}{}", asking.code)
}

/// The notice of one Mac asking to join: its name and code in its title, where a band row
/// shows them beside Approve, and Approve for exactly its key.
pub(crate) fn notice(asking: &Asking) -> Message {
    Message::new(
        tags::PRIVACY,
        Severity::Warn,
        format!(
            "{} asks to join the vault: code {}",
            asking.host, asking.code
        ),
    )
    .action(Intent::ApproveJoin {
        recipient: asking.recipient.clone(),
    })
    .now(Ask::standing(
        StandingWhy::OnlyYou(Reason::new(format!(
            "only an admin lets a Mac read the vault, and {} showed code {}",
            asking.host, asking.code
        ))),
        Then::step(format!(
            "Approve it if {} shows code {}",
            asking.host, asking.code
        )),
    ))
    .key(&key_of(asking))
}

/// What the band does when the approver's record moves from `shown` (the live notices' keys) to
/// `asking`: the notices to post, and the keys whose Mac no longer asks.
pub(crate) fn reconcile<'a>(
    shown: &[String],
    asking: &'a [Asking],
) -> (Vec<&'a Asking>, Vec<String>) {
    let post = asking
        .iter()
        .filter(|asking| !shown.contains(&key_of(asking)))
        .collect();
    let gone = shown
        .iter()
        .filter(|key| !asking.iter().any(|asking| key_of(asking) == **key))
        .cloned()
        .collect();
    (post, gone)
}

static CHANGED: std::sync::Mutex<Option<Vec<Asking>>> = std::sync::Mutex::new(None);

/// The record as the lane last read it, once per change ([`spawn_lane`]).
pub(crate) fn take_changed() -> Option<Vec<Asking>> {
    CHANGED.lock().ok()?.take()
}

/// Start reading the approver's record: a windowed macOS instance, off the event loop, every
/// [`READ_EVERY`], each change handed to the band at its next park ([`take_changed`]). Local
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
            let mut before: Vec<Asking> = Vec::new();
            loop {
                let now = aterm_secrets::client::asking(&paths);
                if now != before {
                    if let Ok(mut changed) = CHANGED.lock() {
                        *changed = Some(now.clone());
                    }
                    before = now;
                }
                std::thread::sleep(READ_EVERY);
            }
        });
    match spawned {
        Ok(_) => true,
        Err(error) => {
            aterm_log::warn!("secrets vault: the join notices did not start: {error}");
            false
        }
    }
}

/// Approve exactly `recipient`, off the event loop: an admin Mac's press of a notice's Approve.
/// The notice ends once the approver's record no longer lists it; a refusal is logged and said
/// in a record.
pub(crate) fn approve(recipient: String) -> bool {
    crate::qos::spawn_lane_thread(
        "aterm-secrets-approve",
        crate::qos::Role::Responsive,
        move || {
            let approved = aterm_secrets::client::Paths::live()
                .and_then(|paths| aterm_secrets::client::approve_request(&paths, &recipient));
            match approved {
                Ok(host) => aterm_log::info!("secrets vault: approved {host}"),
                Err(error) => {
                    aterm_log::warn!("secrets vault: approving a Mac failed: {error}");
                    crate::message_inbox::queue_message(
                        Message::new(tags::PRIVACY, Severity::Warn, "Couldn't approve that Mac")
                            .line(error.to_string())
                            .record(),
                    );
                }
            }
        },
    )
    .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asking(host: &str, code: &str, recipient: &str) -> Asking {
        Asking {
            code: code.to_string(),
            recipient: recipient.to_string(),
            host: host.to_string(),
        }
    }

    #[test]
    fn a_notice_approves_exactly_the_mac_that_asks_and_never_by_default() {
        let m31 = asking(
            "m31",
            "ABCD-EFGH-JKMN-PQRS",
            "age1qyqszqgpqyqszqgpqyqszqgpqyqszqgpqyqszqgpqyqszqgpqyqs3290gq",
        );
        let notice = notice(&m31).normalized();
        assert_eq!(
            notice.actions,
            [Intent::ApproveJoin {
                recipient: m31.recipient.clone()
            }]
        );
        assert!(notice.actions.iter().all(Intent::never_default));
        assert!(!notice.actions.iter().any(Intent::is_consequential));
        assert_eq!(
            notice.title,
            "m31 asks to join the vault: code ABCD-EFGH-JKMN-PQRS"
        );
        assert!(
            notice
                .detail
                .iter()
                .any(|line| line.contains("ABCD-EFGH-JKMN-PQRS")),
            "{:?}",
            notice.detail
        );
        assert_eq!(
            notice.key.as_deref(),
            Some("secrets.asking.ABCD-EFGH-JKMN-PQRS")
        );
        assert!(
            matches!(notice.need(), aterm_messages::need::Need::Now(ask) if ask.fault().is_none()),
            "a complete standing ask: {:?}",
            notice.need()
        );
        // CONTROL: another Mac's notice approves another key.
        let m32 = asking("m32", "WXYZ-WXYZ-WXYZ-WXYZ", "age1other");
        assert_ne!(notice.actions, super::notice(&m32).actions);
    }

    #[test]
    fn the_notices_follow_the_record() {
        let a = asking("a", "AAAA", "age1a");
        let b = asking("b", "BBBB", "age1b");
        let (post, gone) = reconcile(&[], std::slice::from_ref(&a));
        assert_eq!((post, gone), (vec![&a], Vec::<String>::new()));
        let shown = vec![key_of(&a)];
        let (post, gone) = reconcile(&shown, std::slice::from_ref(&b));
        assert_eq!((post, gone), (vec![&b], vec![key_of(&a)]));
        let (post, gone) = reconcile(&shown, std::slice::from_ref(&a));
        assert!(post.is_empty() && gone.is_empty());
    }
}
