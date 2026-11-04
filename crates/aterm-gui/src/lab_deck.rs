// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Stats deck's lab account rows (`docs/DESIGN-alab-lab-model-2026-11-01.md` §8, step 11):
//! one cell per account on the Network State band's Shared State line, and one row per account
//! in its lens. Each row is the account's lab label (never an address), who last used it and on
//! which Mac, its quota left with the age of the reading that says so, and where it is signed in.
//!
//! The rows are projected from the same `LabView` ([`alab_roster::current`]) and journal
//! ([`alab_seen::current`]) the accounts panel reads, on a thread of their own ([`Feed`]): the
//! roster source's first look reads the registry where it is called, so neither door is ever
//! asked from the main thread. Painting and typing read the held [`LabDeck`] and never wait; no
//! provider is asked.

use std::sync::{Arc, Mutex};

use aterm_agent::harness::alab_roster::{self, Lab};
use aterm_agent::harness::alab_seen::{self, AccountSeen, Figure, Seen};

/// The most accounts the deck holds rows for; the band draws the ones that fit and says `+N`.
pub(crate) const MAX_ROWS: usize = 48;

/// What the band's cell of one account shows: how much of its binding window is used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cell {
    /// No current figure: drawn hollow (absent, never zero).
    Unknown,
    /// The binding window's most used reading, in tenths of a percent.
    Used(u16),
    /// A wall stands: the over-max cap's ink.
    AtLimit,
}

/// One account's row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DeckAccount {
    /// A stable selection id ([`id_of`] of its account key), never a vector index.
    pub(crate) id: u64,
    /// The lab's label for it; never an address.
    pub(crate) label: String,
    /// This Mac prefers it.
    pub(crate) here: bool,
    pub(crate) cell: Cell,
    /// `(name, value)` lines for the lens, each time an age at the projection's `at`.
    pub(crate) facts: Vec<(String, String)>,
    /// One line for the lens's list of every account.
    pub(crate) summary: String,
}

/// The deck's account rows as of [`Self::at`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LabDeck {
    /// The rows, this Mac's preferred accounts first, at most [`MAX_ROWS`].
    pub(crate) rows: Vec<DeckAccount>,
    /// How many more accounts there were.
    pub(crate) more: usize,
    /// When the journal was read (Unix seconds), `0` with no journal.
    pub(crate) read_at: i64,
    /// How many accounts the rows leave out because the registry holds them from use (paused,
    /// retired or barred): the deck shows capacity, and a held account is none.
    pub(crate) held: usize,
}

impl LabDeck {
    /// Whether there is anything to draw.
    pub(crate) fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The row selected by `id`.
    pub(crate) fn row(&self, id: u64) -> Option<&DeckAccount> {
        self.rows.iter().find(|row| row.id == id)
    }

    /// The lens rows for the account `id`: its own facts, then one line per other account.
    pub(crate) fn details(&self, id: u64) -> Vec<(String, String)> {
        let Some(row) = self.row(id) else {
            return vec![(
                "Account".into(),
                "The selected account is no longer among the lab's rows".into(),
            )];
        };
        let mut out = row.facts.clone();
        for other in self.rows.iter().filter(|other| other.id != id) {
            out.push((other.label.clone(), other.summary.clone()));
        }
        if self.more > 0 {
            out.push((
                "More accounts".into(),
                format!(
                    "{} more; `aterm db alab accounts` lists every one",
                    self.more
                ),
            ));
        }
        out
    }

    /// The overview's one line: how many accounts and how many this Mac prefers.
    pub(crate) fn overview(&self) -> Option<(String, String)> {
        if self.is_empty() {
            return None;
        }
        let total = self.rows.len() + self.more;
        let here = self.rows.iter().filter(|row| row.here).count();
        let from = if self.read_at == 0 {
            "in the lab registry; the lab journal is not read yet"
        } else {
            "in the lab journal and registry"
        };
        let held = if self.held == 0 {
            String::new()
        } else {
            format!("; {} paused, retired or barred not shown", self.held)
        };
        Some((
            "Lab accounts".into(),
            format!("{total} {from}; {here} preferred by this Mac{held}; no provider asked"),
        ))
    }
}

/// A stable id for the account key `pk`: FNV-1a of its bytes, never 0.
pub(crate) fn id_of(pk: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in pk.bytes() {
        h ^= u64::from(byte);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h.max(1)
}

/// What the lens calls the account: its label, or §0's default (`<cli>-<6 hex>`) where a label
/// holds an address's `@`, so no address reaches the glass even from a mistyped label.
fn label_of(account: &alab_roster::LabAccount) -> String {
    if !account.label.contains('@') && !account.label.trim().is_empty() {
        return account.label.clone();
    }
    let hex: String = account
        .pk
        .strip_prefix("ac-")
        .unwrap_or(&account.pk)
        .chars()
        .filter(char::is_ascii_hexdigit)
        .take(6)
        .collect();
    format!("{}-{hex}", account.cli.connector().id)
}

fn percent(x10: u16) -> String {
    if x10.is_multiple_of(10) {
        format!("{}%", x10 / 10)
    } else {
        format!("{}.{}%", x10 / 10, x10 % 10)
    }
}

/// The binding window of `seen`: a standing wall first, else the most used current account-wide
/// figure. A workspace's window is its own and binds nothing here.
fn binding(seen: &AccountSeen) -> Option<&alab_seen::WindowSeen> {
    let own = || seen.windows.iter().filter(|w| w.sub.is_none());
    own().find(|w| w.wall_until.is_some()).or_else(|| {
        own()
            .filter_map(|w| match w.figure {
                Some(Figure::Used { pct_x10, .. }) => Some((pct_x10, w)),
                _ => None,
            })
            .max_by_key(|(used, _)| *used)
            .map(|(_, w)| w)
    })
}

/// "62% of 5h left, read 12 min ago on m3 (statusline)".
fn left_line(w: &alab_seen::WindowSeen, now: i64) -> String {
    let read = w.read.as_ref().map_or_else(String::new, |(t, by, src)| {
        format!(", read {} ago on {by} ({src})", alab_seen::age(now - *t))
    });
    if let Some(until) = w.wall_until {
        let until = until.map_or_else(String::new, |u| {
            format!(" until {}", aterm_agent::harness::usage::rfc3339_utc(u))
        });
        return format!("{} at its limit{until}{read}", w.window);
    }
    match w.figure {
        Some(Figure::Used { pct_x10, .. }) => format!(
            "{} of {} left{read}",
            percent(1000u16.saturating_sub(pct_x10.min(1000))),
            w.window
        ),
        _ => format!("{} unknown{read}", w.window),
    }
}

fn signed_in_words(seen: &AccountSeen) -> Option<String> {
    (!seen.signed_in.is_empty()).then(|| {
        seen.signed_in
            .iter()
            .map(|s| format!("{} ({})", s.on, s.state))
            .collect::<Vec<_>>()
            .join(", ")
    })
}

/// One account's row. `read`: whether the journal has been read at all, so a row with nothing
/// from it says the journal is not read yet rather than that the journal holds nothing.
fn row_of(
    account: &alab_roster::LabAccount,
    here: bool,
    seen: Option<&AccountSeen>,
    read: bool,
    now: i64,
) -> DeckAccount {
    let label = label_of(account);
    let seen = seen.map_or_else(AccountSeen::default, |seen| AccountSeen {
        windows: seen.windows.iter().map(|w| w.as_of(now)).collect(),
        ..seen.clone()
    });
    let seen = &seen;
    let bind = binding(seen);
    let cell = match bind {
        Some(w) if w.wall_until.is_some() => Cell::AtLimit,
        Some(w) => match w.figure {
            Some(Figure::Used { pct_x10, .. }) => Cell::Used(pct_x10.min(1000)),
            _ => Cell::Unknown,
        },
        None => Cell::Unknown,
    };
    let last = seen
        .last_use
        .as_ref()
        .map(|last| alab_seen::when(&last.by, last.t, now));
    let left = bind.map(|w| left_line(w, now));
    let on = signed_in_words(seen);
    let mut facts = vec![(
        "Account".into(),
        if here {
            format!("{label} · preferred by this Mac")
        } else {
            label.clone()
        },
    )];
    let unread = "The lab journal is not read yet";
    facts.push((
        "Last used by".into(),
        last.clone().unwrap_or_else(|| {
            if read {
                "No use in the lab journal"
            } else {
                unread
            }
            .into()
        }),
    ));
    facts.push((
        "Quota left".into(),
        left.clone().unwrap_or_else(|| {
            if read {
                "Unknown; no current reading in the lab journal"
            } else {
                "Unknown; the lab journal is not read yet"
            }
            .into()
        }),
    ));
    for w in &seen.windows {
        facts.push((
            format!("{} window", w.window),
            alab_seen::window_line(w, now),
        ));
    }
    facts.push((
        "Signed in on".into(),
        on.clone().unwrap_or_else(|| {
            if read {
                "No sign-in word in the lab journal"
            } else {
                unread
            }
            .into()
        }),
    ));
    facts.push((
        "Preferred by".into(),
        if account.preferred.is_empty() {
            "No Mac prefers it; any live account may still run anywhere".into()
        } else {
            account
                .preferred
                .iter()
                .map(|(m, _)| m.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        },
    ));
    let mut summary = Vec::new();
    if here {
        summary.push("preferred here".to_string());
    }
    summary.push(left.unwrap_or_else(|| "quota unknown".into()));
    if let Some(last) = &seen.last_use {
        summary.push(format!(
            "last used by {} {} ago",
            last.by,
            alab_seen::age(now - last.t)
        ));
    }
    if let Some(on) = on {
        summary.push(format!("signed in on {on}"));
    }
    DeckAccount {
        id: id_of(&account.pk),
        label,
        here,
        cell,
        facts,
        summary: summary.join(" · "),
    }
}

/// The deck's rows from `lab` and `seen` at `now` (Unix seconds): every account this Mac
/// prefers, by its rank; then every other account the journal says something of, most recently
/// used first. A preference orders the rows and limits nothing (§0). An account the journal
/// holds that the registry does not name has no label, so it has no row; one the registry holds
/// from use (not `joinable`: paused, retired or barred) is no capacity, so it has none either
/// and is counted in [`LabDeck::held`].
pub(crate) fn project(lab: Option<&Lab>, seen: Option<&Seen>, now: i64) -> LabDeck {
    let Some(lab) = lab else {
        return LabDeck::default();
    };
    let read = seen.is_some();
    let found = |pk: &str| seen.and_then(|seen| seen.accounts.get(pk));
    let says =
        |s: &AccountSeen| s.last_use.is_some() || !s.signed_in.is_empty() || !s.windows.is_empty();
    let me = lab.me.as_deref();
    let shown = |a: &&alab_roster::LabAccount| {
        me.and_then(|me| a.preferred_on(me)).is_some() || found(&a.pk).is_some_and(says)
    };
    let held = lab
        .accounts
        .iter()
        .filter(|a| !a.joinable)
        .filter(shown)
        .count();
    let mut mine: Vec<_> = lab
        .accounts
        .iter()
        .filter(|a| a.joinable)
        .filter_map(|a| Some((me.and_then(|me| a.preferred_on(me))?, a)))
        .collect();
    mine.sort_by(|(ra, a), (rb, b)| {
        (ra.is_none(), *ra, &a.label).cmp(&(rb.is_none(), *rb, &b.label))
    });
    let mut others: Vec<_> = lab
        .accounts
        .iter()
        .filter(|a| a.joinable && me.and_then(|me| a.preferred_on(me)).is_none())
        .filter_map(|a| Some((found(&a.pk).filter(|s| says(s))?, a)))
        .collect();
    others.sort_by(|(sa, a), (sb, b)| {
        let t = |s: &AccountSeen| s.last_use.as_ref().map_or(i64::MIN, |l| l.t);
        t(sb).cmp(&t(sa)).then_with(|| a.label.cmp(&b.label))
    });
    let mut rows: Vec<DeckAccount> = mine
        .into_iter()
        .map(|(_, a)| row_of(a, true, found(&a.pk), read, now))
        .chain(
            others
                .into_iter()
                .map(|(s, a)| row_of(a, false, Some(s), read, now)),
        )
        .collect();
    let more = rows.len().saturating_sub(MAX_ROWS);
    rows.truncate(MAX_ROWS);
    LabDeck {
        rows,
        more,
        read_at: seen.map_or(0, |seen| seen.at),
        held,
    }
}

/// Fixed rows for the feed's dev seam (`ATERM_DEV_LAB_DECK_FIXTURE`): three accounts, one preferred here, one at its limit and
/// one with no reading, with labels that name no machine and no address.
fn fixture(now: i64) -> LabDeck {
    use aterm_agent::harness::accounts::Cli;
    use aterm_agent::harness::alab_seen::{LastUse, SignedIn, WindowSeen};
    let account =
        |pk: &str, label: &str, preferred: Vec<(String, Option<u64>)>| alab_roster::LabAccount {
            pk: pk.into(),
            cli: Cli::Claude,
            vendor: aterm_agent::harness::accounts::Vendor::default(),
            label: label.into(),
            legacy_label: None,
            email: String::new(),
            identity: None,
            browser: None,
            joinable: true,
            preferred,
        };
    let lab = Lab {
        me: Some("m1".into()),
        accounts: vec![
            account(
                "ac-0a1b2c3d4e5f60718293a4",
                "claude-0a1b2c",
                vec![("m1".into(), Some(1))],
            ),
            account(
                "ac-1b2c3d4e5f60718293a4b5",
                "claude-1b2c3d",
                vec![("m2".into(), Some(1))],
            ),
            account("ac-2c3d4e5f60718293a4b5c6", "claude-2c3d4e", Vec::new()),
        ],
        ..Lab::default()
    };
    let window = |pct: u16, wall: bool| WindowSeen {
        window: "5h".into(),
        sub: None,
        figure: Some(Figure::Used {
            pct_x10: pct,
            reset: Some(now + 3_600),
        }),
        read: Some((now - 720, "m1".into(), "statusline".into())),
        wall_until: wall.then_some(Some(now + 3_600)),
    };
    let mut seen = Seen {
        at: now,
        ..Seen::default()
    };
    seen.accounts.insert(
        "ac-0a1b2c3d4e5f60718293a4".into(),
        AccountSeen {
            last_use: Some(LastUse {
                by: "m1".into(),
                t: now - 300,
            }),
            signed_in: vec![SignedIn {
                on: "m1".into(),
                state: "in".into(),
                t: now - 900,
            }],
            windows: vec![window(380, false)],
            history: Vec::new(),
        },
    );
    seen.accounts.insert(
        "ac-1b2c3d4e5f60718293a4b5".into(),
        AccountSeen {
            last_use: Some(LastUse {
                by: "m2".into(),
                t: now - 3_600,
            }),
            signed_in: vec![SignedIn {
                on: "m2".into(),
                state: "in".into(),
                t: now - 7_200,
            }],
            windows: vec![window(1000, true)],
            history: Vec::new(),
        },
    );
    seen.accounts.insert(
        "ac-2c3d4e5f60718293a4b5c6".into(),
        AccountSeen {
            last_use: Some(LastUse {
                by: "m3".into(),
                t: now - 86_400,
            }),
            ..AccountSeen::default()
        },
    );
    project(Some(&lab), Some(&seen), now)
}

/// The deck's rows, projected off the main thread: [`Self::latest`] answers at once with what
/// the feed's thread last projected, and starts that thread on its first ask. The thread
/// projects again each [`Self::EVERY`] only while it is asked: a minute with no ask parks it
/// until the next, so a hidden band costs no wake and no read.
pub(crate) struct Feed {
    held: Arc<Mutex<Option<Arc<LabDeck>>>>,
    /// Set by each ask, taken by the thread: whether anyone still draws the rows.
    asked: Arc<std::sync::atomic::AtomicBool>,
    /// The feed's thread, once started, to wake from its park.
    thread: Option<std::thread::Thread>,
    started: bool,
    /// How many changed projections [`Self::latest`] has handed out: what the deck's frame key
    /// and the lens's fingerprint hash, rather than every row on every frame.
    revision: u64,
    /// Whether the feed projects at all: never in a test, which hands it rows ([`Self::hold`]);
    /// a test of the shipped wake sets it.
    pub(crate) live: bool,
}

impl Default for Feed {
    fn default() -> Self {
        Self {
            held: Arc::default(),
            asked: Arc::default(),
            thread: None,
            started: false,
            revision: 0,
            live: !cfg!(test),
        }
    }
}

impl Feed {
    /// How often the feed projects again: the ages its rows say move by the minute, and the
    /// sources under it reread every five minutes.
    pub(crate) const EVERY: std::time::Duration = std::time::Duration::from_secs(60);

    /// Whether the feed's thread has projected once.
    pub(crate) fn held_any(&self) -> bool {
        self.held.try_lock().is_ok_and(|held| held.is_some())
    }

    /// The rows last projected, when they differ from `shown`; never waits for a projection
    /// (a held lock answers nothing this time). Ask only while the rows are drawn: the ask is
    /// what keeps the thread projecting.
    pub(crate) fn latest(&mut self, shown: &Arc<LabDeck>) -> Option<Arc<LabDeck>> {
        self.asked.store(true, std::sync::atomic::Ordering::SeqCst);
        if !self.started {
            self.started = true;
            self.thread = self.start();
        } else if let Some(thread) = &self.thread {
            thread.unpark();
        }
        let held = self.held.try_lock().ok()?.clone()?;
        if held.as_ref() == shown.as_ref() {
            return None;
        }
        self.revision += 1;
        Some(held)
    }

    /// The count of changed projections handed out so far.
    pub(crate) const fn revision(&self) -> u64 {
        self.revision
    }

    /// Tests hand the feed its rows ([`Self::hold`]) and start no thread.
    #[cfg(test)]
    fn start(&self) -> Option<std::thread::Thread> {
        None
    }

    #[cfg(test)]
    pub(crate) fn hold(&self, deck: LabDeck) {
        *self.held.lock().expect("feed") = Some(Arc::new(deck));
    }

    #[cfg(not(test))]
    fn start(&self) -> Option<std::thread::Thread> {
        /// The dev seam that shows the deck [`fixture`]'s rows in place of the lab's: debug
        /// and dev-seams builds only, for `lab instance` and the galleries, which hold no
        /// registry.
        const FIXTURE_SEAM: &str = "ATERM_DEV_LAB_DECK_FIXTURE";
        let fixed = aterm_types::dev_seam!(FIXTURE_SEAM).is_some();
        let held = Arc::clone(&self.held);
        let asked = Arc::clone(&self.asked);
        std::thread::Builder::new()
            .name("lab-deck".into())
            .spawn(move || {
                loop {
                    let now = aterm_time::unix_millis()
                        .map_or(0, |ms| i64::try_from(ms / 1_000).unwrap_or(i64::MAX));
                    // This thread is no painting path, so it waits for the journal's first
                    // read: a projection before it would say the journal holds nothing.
                    let deck = if fixed {
                        fixture(now)
                    } else {
                        project(
                            alab_roster::current().as_deref(),
                            alab_seen::current(true).as_deref(),
                            now,
                        )
                    };
                    *held
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(deck));
                    std::thread::sleep(Self::EVERY);
                    while !asked.swap(false, std::sync::atomic::Ordering::SeqCst) {
                        std::thread::park();
                    }
                }
            })
            .ok()
            .map(|handle| handle.thread().clone())
    }
}

#[cfg(test)]
#[path = "lab_deck_tests.rs"]
mod tests;
