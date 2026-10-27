// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE AUTOMATIC SIGN-IN LADDER of a Claude Code roster row: every route this machine has to
//! make the row's login by itself, tried in order, each named with what it found, so a row that
//! no route can sign in ends as a recorded error that names them all
//! ([`super::accounts::note_sign_in_attempt`]), never as a silent stop and never as a person
//! asked.
//!
//! The routes, in order ([`Read::route`]): a configured mailbox (a webmail-only one beside a
//! headless browser); the row's own browser profile, driven in a headless browser; and, while
//! neither is ready, the LAN vault, refreshed by a bounded `aterm secrets sync` where this
//! machine is a member, since it can deliver a newly published `ai-mailbox.toml`, after which
//! the two are read again. The line also names what is never a route here: the row's own desk
//! browser profile, which opens a window and so is never used unattended, and
//! `[harness] hosts`, which renew their own logins and none of this machine's. No route opens a
//! window on the desk or the system's default browser, and none copies, stores or relays a
//! login: a route that runs makes a fresh one on this machine.
//!
//! This module holds the ladder's words and its clocks; `accounts_sync` reads the machine and
//! runs the routes.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// What the row's own headless browser profile offers ([`super::headless_oauth::has_browser_profile`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Profile {
    /// It exists and the row's last renewal answered no other account.
    Ready,
    /// It exists, but the row's last renewal signed another account in.
    WrongAccount,
    /// None was made.
    None,
}

/// What the configured mailbox offers the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Mailbox {
    /// A mailbox the sign-in reads over a directory, IMAP or HTTP, with no browser needed.
    Ready,
    /// Only a webmail inbox, which a headless browser must read.
    WebmailOnly,
    /// None is configured for the row.
    None,
}

/// What the LAN vault's refresh found ([`super::accounts_sync`]'s `run_vault_sync`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Vault {
    /// This machine is in no vault (`~/.config/aterm/secrets/source.toml` is missing).
    NotMember,
    /// The sync applied the vault at this sequence.
    Applied(u64),
    /// The sync applied the vault at this sequence but could not write or remove every file it
    /// should (`aterm secrets sync` counts them in its first line and names each one and why).
    AppliedPartly(u64),
    /// The vault had nothing newer than this sequence.
    Unchanged(u64),
    /// The vault does not list this machine (yet).
    NotIn,
    /// The sync could not finish, and why (one bounded word or two).
    Failed(String),
}

impl Vault {
    /// Its words in the ladder's line.
    fn words(&self) -> String {
        match self {
            Self::NotMember => "not a member".to_owned(),
            Self::Applied(seq) => format!("applied seq {seq}"),
            Self::AppliedPartly(seq) => format!("applied seq {seq}, some files failed"),
            Self::Unchanged(seq) => format!("seq {seq}, nothing new"),
            Self::NotIn => "this machine not in it yet".to_owned(),
            Self::Failed(why) => format!("sync failed: {why}"),
        }
    }

    /// Whether the sync may have written files a row reads: it applied the vault, wholly or partly.
    pub(super) fn wrote_files(&self) -> bool {
        matches!(self, Self::Applied(_) | Self::AppliedPartly(_))
    }

    /// What one `aterm secrets sync` printed (its first line) and how it exited. An applied
    /// vault's line ends with how many files failed (`, 1 failed`), which decides; a line without
    /// that count (an older aterm) is read by the exit status, which work after the files can
    /// also make non-zero.
    pub(super) fn of_sync(stdout: &str, code: Option<i32>) -> Self {
        let first = stdout.lines().next().unwrap_or("").trim();
        let seq_after = |marker: &str| -> Option<u64> {
            let (_, tail) = first.split_once(marker)?;
            tail.split(|c: char| !c.is_ascii_digit())
                .next()
                .and_then(|n| n.parse().ok())
        };
        if let Some(seq) = first
            .starts_with("applied seq ")
            .then(|| seq_after("applied seq "))
            .flatten()
        {
            let failed = first
                .rsplit_once(", ")
                .and_then(|(_, tail)| tail.strip_suffix(" failed"))
                .and_then(|n| n.parse::<u64>().ok());
            return match failed {
                Some(0) => Self::Applied(seq),
                Some(_) => Self::AppliedPartly(seq),
                None if code == Some(0) => Self::Applied(seq),
                None => Self::AppliedPartly(seq),
            };
        }
        if let Some(seq) = seq_after("up to date at seq ") {
            return Self::Unchanged(seq);
        }
        if first.contains("not in the vault") {
            return Self::NotIn;
        }
        Self::Failed(match code {
            Some(code) => format!("exit {code}"),
            None => "no exit status".to_owned(),
        })
    }

    /// A sync that did not run to its end, by the error [`super::align::capture_bounded`] gave:
    /// one that could not be started, one ended at its `budget`, one whose output could not be
    /// read. The error's own text, which can name a path, is not kept.
    pub(super) fn of_run_error(error: &str, budget: Duration) -> Self {
        Self::Failed(if error.starts_with("it will not run") {
            "it could not be started".to_owned()
        } else if error.contains("deadline") {
            format!("timed out after {} s", budget.as_secs())
        } else {
            "its output could not be read".to_owned()
        })
    }
}

/// A route the ladder found ready ([`Read::route`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Route {
    /// The mailbox answers the sign-in's email; a headless browser is driven where one is found.
    Mailbox,
    /// The row's own headless browser profile, driven headless.
    Profile,
}

impl Route {
    /// Its word.
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Mailbox => "mailbox",
            Self::Profile => "profile",
        }
    }
}

/// What the ladder read of the machine for one row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Read {
    /// The row's own headless browser profile.
    pub profile: Profile,
    /// The configured mailbox, as the row's sign-in would read it.
    pub mailbox: Mailbox,
    /// The vault's refresh, where it was tried (only while no route is ready).
    pub vault: Option<Vault>,
    /// A headless-capable browser is installed.
    pub browser: bool,
    /// The row names a desk browser profile (`browser =` in its row).
    pub row_browser: bool,
    /// How many `[harness] hosts` are configured.
    pub hosts: usize,
}

impl Read {
    /// The route a sign-in can take now, in the ladder's order: the mailbox (a webmail-only one
    /// only beside a headless browser), then the row's own profile beside a headless browser.
    pub(super) fn route(&self) -> Option<Route> {
        match self.mailbox {
            Mailbox::Ready => return Some(Route::Mailbox),
            Mailbox::WebmailOnly if self.browser => return Some(Route::Mailbox),
            Mailbox::WebmailOnly | Mailbox::None => {}
        }
        (self.profile == Profile::Ready && self.browser).then_some(Route::Profile)
    }

    /// The routes tried and what each found, one line in the ladder's order:
    /// `profile(none) mailbox(none configured) vault(seq 51, nothing new)
    /// headless-browser(none installed) hosts(none configured)`.
    pub(super) fn words(&self) -> String {
        let mut words = vec![
            format!(
                "profile({})",
                match self.profile {
                    Profile::Ready => "ready",
                    Profile::WrongAccount => "last sign-in was another account",
                    Profile::None => "none",
                }
            ),
            format!(
                "mailbox({})",
                match self.mailbox {
                    Mailbox::Ready => "ready",
                    Mailbox::WebmailOnly => "webmail only",
                    Mailbox::None => "none configured",
                }
            ),
        ];
        if let Some(vault) = &self.vault {
            words.push(format!("vault({})", vault.words()));
        }
        words.push(format!(
            "headless-browser({})",
            if self.browser {
                "found"
            } else {
                "none installed"
            }
        ));
        if self.row_browser {
            words.push("desk-browser(opens a window: not used unattended)".to_owned());
        }
        words.push(match self.hosts {
            0 => "hosts(none configured)".to_owned(),
            n => format!("hosts({n}: each signs in only its own logins)"),
        });
        words.join(" ")
    }
}

/// How one run of a route ended, in the words its record says. A run of `claude auth login`
/// that made no login is named by how it exited (`code`, where the run gave one), never read as
/// the vendor's refusal: nothing the run printed says why.
pub(super) fn how_it_ended(renewal: super::cli::Renewal, code: Option<i32>) -> String {
    use super::cli::Renewal;
    match renewal {
        Renewal::Renewed => "signed in".to_owned(),
        Renewal::AuthFailed => match code {
            Some(code) => format!("claude auth login exited {code} with no login made"),
            None => "claude auth login ended with no login made".to_owned(),
        },
        Renewal::WrongAccount => "another account signed in".to_owned(),
        Renewal::Failed => "the sign-in did not finish".to_owned(),
        Renewal::Busy => "another sign-in held the sign-in lock, so nothing ran".to_owned(),
        Renewal::Deferred => {
            "local sign-in state changed or was unavailable; deferred for a fresh attempt"
                .to_owned()
        }
        Renewal::NeedsPerson => "its page waited on a person".to_owned(),
    }
}

/// The record of a run of `route` that did not sign the row in: the route, how it ended
/// ([`how_it_ended`]) and every route the ladder read ([`Read::words`]).
pub(super) fn failed_words(route: Route, how: &str, read: &Read) -> String {
    format!("{}: {how}; {}", route.as_str(), read.words())
}

/// The first wait before a row whose ladder found no route, or whose route failed, is tried
/// again with nothing changed.
pub(super) const BACKOFF_FIRST: Duration = Duration::from_secs(15 * 60);

/// The longest such wait: the wait doubles from [`BACKOFF_FIRST`] up to it.
pub(super) const BACKOFF_MAX: Duration = Duration::from_secs(6 * 60 * 60);

/// The wait after the `step`-th failure in a row (`1` is the first): [`BACKOFF_FIRST`],
/// doubled each step, never past [`BACKOFF_MAX`].
pub(super) fn backoff_after(step: u32) -> Duration {
    let doublings = step.saturating_sub(1).min(16);
    BACKOFF_FIRST
        .saturating_mul(1u32 << doublings)
        .min(BACKOFF_MAX)
}

/// One row's wait before its ladder runs again with the same inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Backoff {
    /// Not before this instant.
    next: Instant,
    /// Failures in a row.
    step: u32,
    /// What the ladder read when it failed ([`Ladder::held`]'s `inputs`).
    inputs: String,
}

/// The vault refresh's own clock: one `aterm secrets sync` at a time, and none sooner than its
/// wait after the last, which doubles until a refresh applies the whole vault.
#[derive(Debug, Clone, PartialEq, Eq)]
struct VaultRun {
    /// When the last refresh started.
    at: Instant,
    /// The wait after it; while it runs, the wait the refresh before it set (zero before the
    /// first).
    wait: Duration,
    /// What it found; `None` while it runs.
    seen: Option<Vault>,
}

/// What the ladder remembers in this process between upkeeps: each failing row's backoff and
/// the vault refresh's clock.
#[derive(Debug, Default)]
pub(super) struct Ladder {
    backoff: BTreeMap<String, Backoff>,
    vault: Option<VaultRun>,
}

/// What [`Ladder::vault_turn`] says of the vault now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum VaultTurn {
    /// Its last refresh stands (still within its wait): what it found.
    Seen(Vault),
    /// A refresh runs now; the ladder decides once it ends.
    Running,
    /// A refresh is due: the caller starts one and reports its end ([`Ladder::vault_done`]).
    Due,
}

impl Ladder {
    /// The existing retry clock, without probing routes or advancing the ladder.
    pub(super) fn retry_at(&self, label: &str, now: Instant) -> Option<Instant> {
        self.backoff
            .get(label)
            .map(|backoff| backoff.next)
            .filter(|next| *next > now)
    }

    /// Whether `label`'s ladder waits out its backoff at `now`: it does while the wait set at its
    /// last failure stands and the machine reads as it did then (`inputs`); a new mailbox, a
    /// browser installed, a vault delivery or a profile made ends the wait at once.
    pub(super) fn held(&self, label: &str, inputs: &str, now: Instant) -> bool {
        self.backoff
            .get(label)
            .is_some_and(|b| b.inputs == inputs && now < b.next)
    }

    /// Notes a failure of `label`'s ladder at `now` over `inputs`, and returns the wait set: the
    /// next step of its backoff, or the first again where the inputs changed.
    pub(super) fn failed(&mut self, label: &str, inputs: &str, now: Instant) -> Duration {
        let step = self
            .backoff
            .get(label)
            .filter(|b| b.inputs == inputs)
            .map_or(1, |b| b.step.saturating_add(1));
        let wait = backoff_after(step);
        self.backoff.insert(
            label.to_owned(),
            Backoff {
                next: now + wait,
                step,
                inputs: inputs.to_owned(),
            },
        );
        wait
    }

    /// Notes that `label` signed in: its backoff ends.
    pub(super) fn signed_in(&mut self, label: &str) {
        self.backoff.remove(label);
    }

    /// The vault at `now`: the last refresh's finding while its wait stands, else a refresh is
    /// due and is marked running (the caller starts it).
    pub(super) fn vault_turn(&mut self, now: Instant) -> VaultTurn {
        match &self.vault {
            Some(VaultRun { seen: None, .. }) => VaultTurn::Running,
            Some(VaultRun {
                at,
                wait,
                seen: Some(seen),
            }) if now.saturating_duration_since(*at) < *wait => VaultTurn::Seen(seen.clone()),
            last => {
                let wait = last.as_ref().map_or(Duration::ZERO, |last| last.wait);
                self.vault = Some(VaultRun {
                    at: now,
                    wait,
                    seen: None,
                });
                VaultTurn::Due
            }
        }
    }

    /// The refresh started by [`Self::vault_turn`] ended with `seen`: the next one waits
    /// [`BACKOFF_FIRST`] after the first refresh or one that applied the whole vault, else twice
    /// the wait before it, never past [`BACKOFF_MAX`].
    pub(super) fn vault_done(&mut self, seen: Vault) {
        let (at, prior) = self
            .vault
            .as_ref()
            .map_or((Instant::now(), Duration::ZERO), |run| (run.at, run.wait));
        let wait = if matches!(seen, Vault::Applied(_)) || prior.is_zero() {
            BACKOFF_FIRST
        } else {
            prior.saturating_mul(2).min(BACKOFF_MAX)
        };
        self.vault = Some(VaultRun {
            at,
            wait,
            seen: Some(seen),
        });
    }
}

/// The `aterm.log` line one attempt of `label`'s ladder writes: how it ended and the routes.
pub(super) fn log_line(label: &str, outcome: &str, words: &str) -> String {
    format!(
        "account {label}: automatic sign-in {outcome}: {}",
        super::one_line(words, 400)
    )
}

#[cfg(test)]
#[path = "sign_in_ladder_tests.rs"]
mod tests;
