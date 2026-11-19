// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Spotlight's exclusion of the console account's own new top-level home folders, which the
//! root LAN broker makes with no password ([`super::wire::Request::ScopeHome`],
//! `docs/DESIGN-lan-broker-2026-11-05.md` §2): the answer a client hears ([`ScopeAnswer`]) and
//! the duty a broker serves the request by ([`ScopeDuty`]), which
//! `aterm_machines::init_spotlight::broker_duty` implements and `aterm-lan` runs one at a time,
//! paced.
//!
//! The request names nothing. Everything the duty acts on is what root reads for itself: the
//! peer's uid from the kernel, the console's owner, that account's home from directory services,
//! the home's own listing, and root's receipt.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

use super::wire::Frame;

/// The least time between two `mds` restarts the root LAN broker's applies make: a request
/// inside it waits out the rest at the broker. A person's run that heard [`ScopeAnswer::Busy`]
/// waits one before it asks once more, which is no bound on another request's hold of the
/// broker's one at a time: that holds for its readings (each up to the duty's own deadline), at
/// most one such wait and its apply, which together can outlast one pace, so the second ask can
/// find it held still.
pub const SCOPE_PACE: Duration = Duration::from_secs(60);

/// What the broker answered a [`super::wire::Request::ScopeHome`] ([`super::scope_home`]). Each
/// failure is a [`Frame::Error`] whose code says which ([`Self::of_error`], [`Self::frame`]),
/// and whose words are the broker's fixed sentence for it: none names a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeAnswer {
    /// It excluded this many of the account's top-level home folders: none where none was owed.
    Excluded(u32),
    /// The broker could not serve the request now (`EAGAIN`): another request held its one at
    /// a time, another root edit of Spotlight held root's lock, its reading of the home did not
    /// answer in time, or `mds` rewrote the configuration under each of its edits. Asked again
    /// later, it may be served.
    Busy,
    /// No broker of this build served the request: none answers, one older than the request or
    /// of another protocol, one that refused or did not judge this process's code, an exchange
    /// that failed, or a broker that has no Spotlight duty (`ENOTSUP`: the direct path).
    Unserved(String),
    /// The broker does not scope this account's home (`EPERM`): root, an account other than the
    /// console's, or a home it does not hold to its rules.
    Refused(String),
    /// The broker left Spotlight as it is (`EALREADY`): a receipt that is not root's own or is
    /// past its bound, a home past its listing or receipt bound, an indexing state it could not
    /// read, or the same folders whose exclusion failed the last time it tried them.
    Unchanged(String),
    /// The broker's edit of Spotlight's configuration, or its receipt, failed (`EIO`).
    Failed(String),
}

impl ScopeAnswer {
    /// The answer a broker's [`Frame::Error`] of `code` and `words` stands for. A code no broker
    /// of this build sends reads as [`Self::Unserved`]: a broker that answers so serves this
    /// build nothing it can act on.
    #[must_use]
    pub fn of_error(code: i32, words: String) -> Self {
        match code {
            libc::EPERM => Self::Refused(words),
            libc::EAGAIN => Self::Busy,
            libc::ENOTSUP => Self::Unserved(words),
            libc::EALREADY => Self::Unchanged(words),
            libc::EIO => Self::Failed(words),
            other => Self::Unserved(format!("the LAN broker answered code {other}: {words}")),
        }
    }

    /// The frame a broker answers with: [`Frame::Scoped`], or the [`Frame::Error`] that
    /// [`Self::of_error`] reads back as this answer.
    #[must_use]
    pub fn frame(&self) -> Frame {
        let (code, words) = match self {
            Self::Excluded(added) => return Frame::Scoped { added: *added },
            Self::Busy => (
                libc::EAGAIN,
                "the LAN broker could not serve this request now; ask again later",
            ),
            Self::Unserved(words) => (libc::ENOTSUP, words.as_str()),
            Self::Refused(words) => (libc::EPERM, words.as_str()),
            Self::Unchanged(words) => (libc::EALREADY, words.as_str()),
            Self::Failed(words) => (libc::EIO, words.as_str()),
        };
        Frame::Error {
            code,
            words: words.to_owned(),
        }
    }

    /// The answer's word in the broker's log line: `added=<n>`, `busy`, `unserved`, `refused`,
    /// `unchanged` or `failed`. It never names a path.
    #[must_use]
    pub fn class(&self) -> String {
        match self {
            Self::Excluded(added) => format!("added={added}"),
            Self::Busy => "busy".to_owned(),
            Self::Unserved(_) => "unserved".to_owned(),
            Self::Refused(_) => "refused".to_owned(),
            Self::Unchanged(_) => "unchanged".to_owned(),
            Self::Failed(_) => "failed".to_owned(),
        }
    }
}

/// What one request would exclude for the account `uid` ([`ScopeDuty::plan`]): top-level
/// folders of that account's own home, none of them named by root's receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopePlan {
    /// The account the kernel named as the request's peer.
    pub uid: u32,
    /// The folders, in path order.
    pub folders: BTreeSet<PathBuf>,
}

/// What a broker serves a [`super::wire::Request::ScopeHome`] by. The broker takes its one lock
/// around both calls and waits out the pace between `mds` restarts by [`Self::now`] and
/// [`Self::sleep`], planning again after the wait.
pub trait ScopeDuty: Send + Sync {
    /// What a request of `uid`'s would exclude now, read afresh: the plan (empty where nothing
    /// is owed), or the answer that ends the request with no edit.
    ///
    /// # Errors
    /// The answer: [`ScopeAnswer::Refused`] for an account or home outside the duty's rules, or
    /// [`ScopeAnswer::Unchanged`] where it leaves Spotlight as it is.
    fn plan(&self, uid: u32) -> Result<ScopePlan, ScopeAnswer>;

    /// Exclude `plan`'s folders and note them on root's receipt: [`ScopeAnswer::Excluded`];
    /// [`ScopeAnswer::Busy`] where another root edit of Spotlight holds it off or `mds` rewrote
    /// the configuration under each of its edits, which the client asks again;
    /// [`ScopeAnswer::Unchanged`] where it leaves Spotlight as it is; or [`ScopeAnswer::Failed`].
    /// Only [`ScopeAnswer::Excluded`] of at least one folder and [`ScopeAnswer::Failed`] may have
    /// restarted `mds`: every other answer ends before the configuration's rename, so the broker
    /// starts its pace ([`SCOPE_PACE`]) on those two alone.
    fn apply(&self, plan: &ScopePlan) -> ScopeAnswer;

    /// Unix seconds now, the pace's clock.
    fn now(&self) -> u64;

    /// Wait `period` on the pace's clock.
    fn sleep(&self, period: Duration);
}

#[cfg(test)]
#[path = "scope_tests.rs"]
mod tests;
