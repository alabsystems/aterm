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
    /// Also covers a steward's `mail-code` fleet relay (`ai-signin.toml` `relay = true`).
    Ready,
    /// Only a webmail inbox, which a headless browser must read.
    WebmailOnly,
    /// The row's email's own `[[mailboxes]]` entry ([`super::mailbox_routes::MailboxRoute::own`])
    /// has no password (or no login) yet: it is never tried, and that is said, never a failed
    /// sign-in. The lab may be asked for it ([`Read::wants_mailbox`]): the answer, kept under
    /// the row's label, is the entry's password
    /// ([`super::headless_oauth::MailboxConfig::completed_route`]).
    NoPassword,
    /// The `[[mailboxes]]` entry the row's email's codes arrive in is shared (other addresses,
    /// a wildcard, or another login's mailbox) and has no password yet: never tried, and never
    /// asked for row by row, since an answer kept under one row's label is not that mailbox's
    /// password; the importer names it once (`aterm harness accounts mailboxes import`).
    SharedNoPassword,
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

    /// A bounded diagnostic and its agreeing exit status, or an older sync's first line.
    /// Legacy applied output uses its failed count when present, because work after the
    /// files can also make the exit non-zero. Malformed versioned output never falls back
    /// to interpreting embedded prose as a successful legacy outcome.
    pub(super) fn of_sync(stdout: &str, code: Option<i32>) -> Self {
        if stdout.trim_start().starts_with("aterm-secrets-sync-") {
            #[cfg(not(unix))]
            return Self::Failed("sync diagnostic unavailable on this platform".to_owned());
            #[cfg(unix)]
            {
                use aterm_secrets::sync_diagnostic::{SyncDiagnostic, failure_reason};
                let Some(outcome) = SyncDiagnostic::parse(stdout) else {
                    return Self::Failed("invalid sync diagnostic".to_owned());
                };
                if code.is_none() || outcome.success() != (code == Some(0)) {
                    return Self::Failed("sync diagnostic disagrees with exit status".to_owned());
                }
                return match outcome {
                    SyncDiagnostic::Applied { seq, failed: 0 } => Self::Applied(seq),
                    SyncDiagnostic::Applied { seq, .. } => Self::AppliedPartly(seq),
                    SyncDiagnostic::Unchanged(seq) | SyncDiagnostic::PutBack(seq) => {
                        Self::Unchanged(seq)
                    }
                    SyncDiagnostic::NotIn => Self::NotIn,
                    SyncDiagnostic::Failed(kind) => Self::Failed(failure_reason(kind).to_owned()),
                };
            }
        }
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
    /// The login of that mailbox, masked (`ab…@…`), where it reads one over IMAP, HTTP or
    /// webmail, or is a `[[mailboxes]]` entry with no password yet.
    pub mailbox_login: Option<String>,
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
    /// Whether this read, with no route, is the case the lab is asked for the row's mailbox
    /// password (docs/DESIGN-credential-requests-2026-10-27.md §12.3): no mailbox is configured
    /// for the row, or its email's own `[[mailboxes]]` entry has no password yet (never a
    /// shared one's: [`Mailbox::SharedNoPassword`]), and a vault refresh ran and gave it none. A
    /// Mac in no vault, or one whose refresh failed, asks nothing.
    pub(super) fn wants_mailbox(&self) -> bool {
        matches!(self.mailbox, Mailbox::None | Mailbox::NoPassword)
            && matches!(
                self.vault,
                Some(Vault::Applied(_) | Vault::AppliedPartly(_) | Vault::Unchanged(_))
            )
    }

    /// The route a sign-in can take now, in the ladder's order: the mailbox (a webmail-only one
    /// only beside a headless browser), then the row's own profile beside a headless browser.
    pub(super) fn route(&self) -> Option<Route> {
        match self.mailbox {
            Mailbox::Ready => return Some(Route::Mailbox),
            Mailbox::WebmailOnly if self.browser => return Some(Route::Mailbox),
            Mailbox::WebmailOnly
            | Mailbox::NoPassword
            | Mailbox::SharedNoPassword
            | Mailbox::None => {}
        }
        (self.profile == Profile::Ready && self.browser).then_some(Route::Profile)
    }

    /// The routes tried and what each found, one line in the ladder's order:
    /// `profile(none) mailbox(none configured) vault(seq 51, nothing new)
    /// headless-browser(none installed) hosts(none configured)`. The mailbox is named by its
    /// masked login where it has one: `mailbox(ab…@…)`, `mailbox(ab…@…, webmail only)`,
    /// `mailbox(ab…@… has no password yet)`. An installed headless browser
    /// that no route drives is named as not run (`headless-browser(installed; not run: no
    /// profile or mailbox to drive)`), never as if a sign-in had been tried in it.
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
                match (self.mailbox, self.mailbox_login.as_deref()) {
                    (Mailbox::Ready, Some(login)) => login.to_owned(),
                    (Mailbox::Ready, None) => "ready".to_owned(),
                    (Mailbox::WebmailOnly, Some(login)) => format!("{login}, webmail only"),
                    (Mailbox::WebmailOnly, None) => "webmail only".to_owned(),
                    (Mailbox::NoPassword | Mailbox::SharedNoPassword, login) => format!(
                        "{} has no password yet",
                        login.unwrap_or("its [[mailboxes]] entry")
                    ),
                    (Mailbox::None, _) => "none configured".to_owned(),
                }
            ),
        ];
        if let Some(vault) = &self.vault {
            words.push(format!("vault({})", vault.words()));
        }
        words.push(format!(
            "headless-browser({})",
            match (self.browser, self.route()) {
                (false, _) => "none installed",
                (true, Some(_)) => "installed",
                (true, None) => "installed; not run: no profile or mailbox to drive",
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
/// that made no login, or did not finish, is named by how it exited (`code`, where the run gave
/// one), never read as the vendor's refusal, and then by what its opener's headless sign-in met
/// where its outcome file names it (`met`: Cloudflare's bot check or block page, or no callback
/// in time; [`super::open_url::Met::words`]), so a bot check is named on record. What it met
/// never enters the ladder's backoff key, which is what the ladder read before the run
/// ([`Ladder::failed`]).
pub(super) fn how_it_ended(
    renewal: super::cli::Renewal,
    code: Option<i32>,
    met: Option<&super::open_url::Met>,
) -> String {
    use super::cli::Renewal;
    match (renewal, met) {
        (Renewal::AuthFailed, Some(met)) => match code {
            Some(code) => format!("claude auth login exited {code}; {}", met.words()),
            None => format!("claude auth login ended; {}", met.words()),
        },
        (Renewal::Failed, Some(met)) => format!("{}; {}", run_words(renewal, code), met.words()),
        _ => run_words(renewal, code),
    }
}

/// [`how_it_ended`] without what the page met.
fn run_words(renewal: super::cli::Renewal, code: Option<i32>) -> String {
    use super::cli::Renewal;
    match renewal {
        Renewal::Renewed => "signed in".to_owned(),
        Renewal::OtherRenewed => "another configured account signed in".to_owned(),
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
        Renewal::BotCheck => "it met Cloudflare's bot check, which did not clear".to_owned(),
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
    /// The bounded local vault inputs observed before this refresh.
    context: VaultContext,
    /// An observed context change or network move forbids reuse, including after completion.
    invalidated: bool,
    /// When the last refresh started.
    at: Instant,
    /// The wait after it; while it runs, the wait the refresh before it set (zero before the
    /// first).
    wait: Duration,
    /// What it found; `None` while it runs.
    seen: Option<Vault>,
}

/// An opaque digest of this home's source and stable applied-vault observations.
pub(super) type VaultContext = [u8; 32];

/// What the ladder remembers in this process between upkeeps: each failing row's backoff and
/// the vault refresh's clock.
#[derive(Debug, Default)]
pub(super) struct Ladder {
    backoff: BTreeMap<String, Backoff>,
    vault: Option<VaultRun>,
    /// Last observed context survives a network move that discards the cached result.
    context: Option<VaultContext>,
}

/// What [`Ladder::vault_turn`] says of the vault now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum VaultTurn {
    /// No readable context is available; no refresh was started or result reused.
    Unavailable,
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
    /// browser installed, a vault delivery or a profile made ends the wait at once. Once a
    /// different input is observed, returning to the previous input cannot restore its wait.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "AccountRetryInputs",
            action = "RowA",
            reason = "The admitted account row changes independently of the retry clock."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "AccountRetryInputs",
            action = "RowB",
            reason = "A different admitted account row is observed under the same label."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "AccountRetryInputs",
            action = "Expire",
            reason = "The local monotonic retry deadline passes."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountRetryInputs",
            action = "Probe",
            project = "aterm_agent::harness::sign_in_ladder::row_context_tests::transition"
        )
    )]
    pub(super) fn held(&mut self, label: &str, inputs: &str, now: Instant) -> bool {
        if self.backoff.get(label).is_some_and(|b| b.inputs != inputs) {
            self.backoff.remove(label);
            return false;
        }
        self.backoff
            .get(label)
            .is_some_and(|b| b.inputs == inputs && now < b.next)
    }

    /// Notes a failure of `label`'s ladder at `now` over `inputs`, and returns the wait set: the
    /// next step of its backoff, or the first again where the inputs changed.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountRetryInputs",
            action = "Fail",
            project = "aterm_agent::harness::sign_in_ladder::row_context_tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountRetryInputs",
            action = "OtherFailure",
            project = "aterm_agent::harness::sign_in_ladder::row_context_tests::transition"
        )
    )]
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

    /// Whether a retry step is retained, even after its deadline has passed.
    pub(super) fn has_backoff(&self, label: &str) -> bool {
        self.backoff.contains_key(label)
    }

    /// Notes that `label` signed in: its backoff ends.
    pub(super) fn signed_in(&mut self, label: &str) {
        self.backoff.remove(label);
    }

    /// The vault at `now`: reuse only a completed result for the same readable context.
    /// A changed context invalidates a running refresh but never starts another beside it.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "VaultRefreshContext",
            action = "ContextA",
            reason = "Observe a bounded local vault source/revision fingerprint."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "VaultRefreshContext",
            action = "ContextB",
            reason = "Observe a different bounded local vault source/revision fingerprint."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "VaultRefreshContext",
            action = "Unreadable",
            reason = "The local vault context becomes absent or unreadable."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "VaultRefreshContext",
            action = "Expire",
            reason = "The local monotonic refresh deadline passes."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "VaultRefreshContext",
            action = "Lookup",
            project = "aterm_agent::harness::sign_in_ladder::vault_context_tests::transition"
        )
    )]
    pub(super) fn vault_turn(&mut self, context: Option<VaultContext>, now: Instant) -> VaultTurn {
        if self.context != context {
            self.backoff.clear();
            self.context = context;
        }
        if let Some(run) = &mut self.vault
            && context != Some(run.context)
        {
            run.invalidated = true;
            self.backoff.clear();
        }
        if self.vault.as_ref().is_some_and(|run| run.seen.is_none()) {
            return VaultTurn::Running;
        }
        let Some(context) = context else {
            self.vault = None;
            return VaultTurn::Unavailable;
        };
        if let Some(seen) = self.vault_result(context, now) {
            return VaultTurn::Seen(seen);
        }
        let wait = self
            .vault
            .as_ref()
            .filter(|run| !run.invalidated)
            .map_or(Duration::ZERO, |run| run.wait);
        self.vault = Some(VaultRun {
            context,
            invalidated: false,
            at: now,
            wait,
            seen: None,
        });
        VaultTurn::Due
    }

    /// Read a completed result without reserving a refresh the caller will not start.
    pub(super) fn vault_result(&self, context: VaultContext, now: Instant) -> Option<Vault> {
        self.vault.as_ref().and_then(|run| {
            (run.context == context
                && !run.invalidated
                && now.saturating_duration_since(run.at) < run.wait)
                .then(|| run.seen.clone())
                .flatten()
        })
    }

    /// Discard obsolete evidence and row waits without freeing a running refresh's slot.
    pub(super) fn invalidate_vault(&mut self) {
        self.backoff.clear();
        self.context = None;
        self.invalidate_vault_result();
    }

    fn invalidate_vault_result(&mut self) {
        match &mut self.vault {
            Some(run) if run.seen.is_none() => run.invalidated = true,
            _ => self.vault = None,
        }
    }

    /// This machine's network moved at `now` (docs/DESIGN-fluid-swarm-2026-10-28.md F3c): the
    /// vault a refresh could not reach may answer now, so the vault's wait ends and its clock
    /// starts again from the first wait, and each row's backoff ends, keeping its step, so a row
    /// that fails again the same way waits longer. A refresh running now runs on, and its
    /// result cannot delay the next refresh on the changed network.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "VaultRefreshContext",
            action = "Move",
            project = "aterm_agent::harness::sign_in_ladder::vault_context_tests::transition"
        )
    )]
    pub(super) fn moved(&mut self, now: Instant) {
        self.invalidate_vault_result();
        for backoff in self.backoff.values_mut() {
            backoff.next = backoff.next.min(now);
        }
    }

    /// The refresh started by [`Self::vault_turn`] ended with `seen`: the next one waits
    /// [`BACKOFF_FIRST`] after the first refresh or one that applied the whole vault, else twice
    /// the wait before it, never past [`BACKOFF_MAX`].
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "VaultRefreshContext",
            action = "Complete",
            project = "aterm_agent::harness::sign_in_ladder::vault_context_tests::transition"
        )
    )]
    pub(super) fn vault_done(&mut self, seen: Vault) {
        let Some(run) = self.vault.as_mut().filter(|run| run.seen.is_none()) else {
            return;
        };
        run.wait = if matches!(seen, Vault::Applied(_)) || run.wait.is_zero() {
            BACKOFF_FIRST
        } else {
            run.wait.saturating_mul(2).min(BACKOFF_MAX)
        };
        run.seen = Some(seen);
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

#[cfg(test)]
#[path = "row_retry_context_tests.rs"]
mod row_context_tests;

#[cfg(test)]
#[path = "vault_refresh_context_tests.rs"]
mod vault_context_tests;
