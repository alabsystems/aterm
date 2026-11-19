// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! macOS's file-change service as `aterm init --only init.fseventsd`
//! (`aterm_machines::init_fseventsd`) and the System Resources page's macOS configuration section
//! both judge it: the record of the last restart sent ([`Sent`] under [`stamp_path`]), what it
//! holds against the services bar in words ([`holds`]), and the line past which a restart is
//! offered ([`restart_offered`], the window's services watch's line) or followed
//! ([`followed`], `aterm init`'s).

use std::path::PathBuf;
use std::time::Duration;

use aterm_vitals::services::{Size, followed_from, large_bar, size};
use aterm_vitals::words::mem;

/// How long after a restart was sent no run restarts fseventsd again.
pub const RECENT: Duration = Duration::from_mins(5);

/// The file under the account's state root that records the last restart sent ([`Sent`]).
pub const STAMP: &str = "fseventsd-restart";

/// One restart sent: the process signalled, by pid and start second, and when, unix seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sent {
    /// The pid signalled.
    pub pid: u32,
    /// When that process started, unix seconds.
    pub start_s: u64,
    /// When the signal was sent, unix seconds.
    pub at_s: u64,
}

impl Sent {
    /// The record's one line: `<pid> <start second> <sent second>`.
    #[must_use]
    pub fn line(&self) -> String {
        format!("{} {} {}\n", self.pid, self.start_s, self.at_s)
    }

    /// The record `bytes` hold; `None` for anything but one such line.
    #[must_use]
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(bytes).ok()?;
        let mut words = text.split_whitespace();
        let sent = Self {
            pid: parse_uint(words.next()?)?,
            start_s: parse_uint(words.next()?)?,
            at_s: parse_uint(words.next()?)?,
        };
        words.next().is_none().then_some(sent)
    }
}

fn parse_uint<T: std::str::FromStr>(s: &str) -> Option<T> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

/// Where restarts are recorded: [`STAMP`] under this account's state root.
#[must_use]
pub fn stamp_path() -> Option<PathBuf> {
    crate::dirs::account_state_dir().map(|root| root.join(STAMP))
}

/// What a fseventsd of `footprint` bytes holds on a Mac of `total`, against the bar.
#[must_use]
pub fn holds(footprint: u64, total: u64) -> String {
    let bar = mem(large_bar(total));
    match size(footprint, total) {
        Size::Usual if footprint >= followed_from(total) => format!(
            "fseventsd holds {}, within an eighth under the {bar} bar",
            mem(footprint)
        ),
        Size::Usual => format!("fseventsd holds {}, under the {bar} bar", mem(footprint)),
        Size::Large | Size::Huge => {
            format!("fseventsd holds {}, past the {bar} bar", mem(footprint))
        }
    }
}

/// Whether the window offers fseventsd's restart: it is alone in its coalition (`live` 1) and
/// holds past the large bar, the line the window's services watch keeps a service large by and
/// so the one its restart press accepts.
#[must_use]
pub fn restart_offered(footprint: u64, live: u32, total: u64) -> bool {
    live == 1 && size(footprint, total) != Size::Usual
}

/// Whether `aterm init --only init.fseventsd` restarts it: alone in its coalition and holding
/// from the margin under the large bar ([`followed_from`]).
#[must_use]
pub fn followed(footprint: u64, live: u32, total: u64) -> bool {
    live == 1 && footprint >= followed_from(total)
}
