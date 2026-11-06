// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE AUTOMATIC SIGN-IN LADDER of a Claude Code roster row: every route this machine has to
//! make the row's login by itself, tried in order, each named with what it found, so a row that
//! no route can sign in ends as a recorded error that names them all
//! ([`super::accounts::note_sign_in_attempt`]), never as a silent stop and never as the AI
//! asked to solve a challenge.
//!
//! The routes, in order ([`Read::route`]): a configured mailbox read directly; the row's own
//! proven browser profile; and a webmail-only mailbox (the last two beside a headless browser).
//! While none is ready, the LAN vault is refreshed by a bounded `aterm secrets sync` where this
//! machine is a member, since it can deliver a newly published `ai-mailbox.toml`, after which
//! the routes are read again. The line also names what is never a route here: the row's own desk
//! browser profile, which opens a window and so is never used unattended, and
//! `[harness] hosts`, which renew their own logins and none of this machine's. No route opens a
//! window on this desk or the system's default browser, and none copies, stores or relays a
//! login: a route that runs makes a fresh one on this machine. A blocked Claude OAuth page may
//! instead use the fleet issuer's interactive challenge service, which returns only the exact
//! loopback callback to this machine's own waiting sign-in.
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
    /// No reader but a webmail inbox, which a headless browser must read, and a password the
    /// sign-in would use for the row. A password with no inbox named or known for its domain
    /// reads this way too; its sign-in then says no webmail URL is configured.
    WebmailOnly,
    /// Only a webmail inbox, with no password for the row (the file names none, or names a
    /// password file not delivered yet). A headless browser still tries it, since the webmail
    /// asks for a password only where it is signed out, but it is not the password the lab was
    /// asked for ([`Read::holds_mailbox`]).
    WebmailNoPassword,
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
                .and_then(crate::parse_uint::<u64>);
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

    /// Whether the row's mailbox ends the lab's ask for it ([`Read::wants_mailbox`]): one the
    /// sign-in reads without a browser, or a webmail inbox with the row's password, whether or
    /// not a headless browser is here to read the webmail. A webmail inbox with no password
    /// ([`Mailbox::WebmailNoPassword`]) leaves the ask standing.
    pub(super) fn holds_mailbox(&self) -> bool {
        matches!(self.mailbox, Mailbox::Ready | Mailbox::WebmailOnly)
    }

    /// The route a sign-in can take now, in the ladder's order: a mailbox read directly, the
    /// row's own proven profile, then a webmail-only mailbox (the browser routes need one).
    pub(super) fn route(&self) -> Option<Route> {
        if self.mailbox == Mailbox::Ready {
            return Some(Route::Mailbox);
        }
        if self.profile == Profile::Ready && self.browser {
            return Some(Route::Profile);
        }
        match self.mailbox {
            Mailbox::WebmailOnly | Mailbox::WebmailNoPassword if self.browser => {
                Some(Route::Mailbox)
            }
            _ => None,
        }
    }

    /// The routes tried and what each found, one line in the ladder's order:
    /// `profile(none) mailbox(none configured) vault(seq 51, nothing new)
    /// headless-browser(none installed) hosts(none configured)`. The mailbox is named by its
    /// masked login where it has one: `mailbox(ab…@…)`, `mailbox(ab…@…, webmail only)`,
    /// `mailbox(ab…@…, webmail only, no password)`,
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
                    (Mailbox::WebmailNoPassword, Some(login)) => {
                        format!("{login}, webmail only, no password")
                    }
                    (Mailbox::WebmailNoPassword, None) => "webmail only, no password".to_owned(),
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

/// [`how_it_ended`], then why a run that ended [`Renewal::Failed`] could not be started or read
/// (`cause`): the words of a failure the run itself gave no exit or page for.
///
/// [`Renewal::Failed`]: super::cli::Renewal::Failed
pub(super) fn how_it_ended_because(
    renewal: super::cli::Renewal,
    code: Option<i32>,
    met: Option<&super::open_url::Met>,
    cause: Option<&str>,
) -> String {
    let words = how_it_ended(renewal, code, met);
    match cause {
        Some(why) if renewal == super::cli::Renewal::Failed && met.is_none() => {
            format!("{words}: {why}")
        }
        _ => words,
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
    /// A page stopped this failure ([`Ladder::page_stopped`]).
    page_stopped: bool,
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
    /// browser installed, a vault delivery or a profile made ends the wait at once, though a row a
    /// page stopped is held on record before it gets here. Once a different input is observed,
    /// returning to the previous input cannot restore its wait.
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
                page_stopped: false,
            },
        );
        wait
    }

    /// Notes that a page stopped `label`'s last failure, which it waits on until `until`: no move
    /// ends this wait ([`Self::moved`]), and the saved record says when it is over
    /// ([`Self::page_ended`]).
    pub(super) fn page_stopped(&mut self, label: &str, until: Instant) {
        if let Some(backoff) = self.backoff.get_mut(label) {
            backoff.page_stopped = true;
            backoff.next = backoff.next.max(until);
        }
    }

    /// The saved record no longer holds `label`, so a wait [`Self::page_stopped`] set ends: the
    /// record counts wall-clock time and hears of a person's browser window, which this process's
    /// uptime clock does not.
    pub(super) fn page_ended(&mut self, label: &str) {
        if self.backoff.get(label).is_some_and(|b| b.page_stopped) {
            self.backoff.remove(label);
        }
    }

    /// Whether a retry step is retained, even after its deadline has passed.
    pub(super) fn has_backoff(&self, label: &str) -> bool {
        self.backoff.contains_key(label)
    }

    /// Notes that `label` signed in: its backoff ends.
    pub(super) fn signed_in(&mut self, label: &str) {
        self.backoff.remove(label);
    }

    /// A newly checked preparation replaces the authority behind the prior attempt.
    pub(super) fn preparation_changed(&mut self, label: &str) {
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

    /// Retire this uncompleted refresh without recording an authentication failure.
    pub(super) fn vault_cancelled(&mut self, context: VaultContext) {
        if self
            .vault
            .as_ref()
            .is_some_and(|run| run.context == context && run.seen.is_none())
        {
            self.vault = None;
        }
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
    /// result cannot delay the next refresh on the changed network. A row a page stopped waits on
    /// ([`Self::page_stopped`]): most of what counts as a move has no new address behind it (macOS's
    /// Local Network answer soon after most launches, a fleet's peers all day), and ending that wait
    /// on each would load the page back to back.
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
        for backoff in self.backoff.values_mut().filter(|b| !b.page_stopped) {
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

// THE SIGN-IN DOOR (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.15 rule 4). A provider is
// signed in to for an account only on a Mac using that account, only because it is in use, and
// never in a burst. Every sign-in this harness starts takes a [`SignIn`], minted by one of these:
// a row a session here runs on whose login is signed out here (and the row a launch here starts
// on, whose login was refused), a person naming the account, and one due login this Mac uses
// ([`super::accounts::renewal_due`]). A timer holds none of that evidence, so it can mint nothing.
// The token starts its sign-in under the machine-wide sign-in lock, and an automatic one only
// [`SPACING_S`] after another account's: the spacing clock is a file beside the lock, so every
// process on this Mac keeps one spacing.

/// The least time, in seconds, between the starts of two sign-ins of different accounts on this
/// Mac. Logins made together expire together, and a burst of them from one Mac looks like a
/// sweep to the provider.
pub(super) const SPACING_S: i64 = 10 * 60;

/// The file beside [`super::accounts::SIGNIN_LOCK_FILE`] in [`super::accounts::Store::shared_dir`]
/// that keeps the last sign-in this Mac started. It is read for an automatic sign-in and written
/// at every start, each only while the sign-in lock is held.
pub(super) const SIGNIN_SPACING_FILE: &str = "signin-spacing";

/// The first line of [`SIGNIN_SPACING_FILE`].
const SPACING_HEAD: &str = "aterm-signin-spacing-v1";

/// The last sign-in this Mac started: its CLI's id, its label and when (Unix seconds).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Started {
    cli: String,
    label: String,
    at: i64,
}

impl Started {
    /// The record of `cli`'s `label` starting at `at`.
    pub(super) fn new(cli: super::accounts::Cli, label: &str, at: i64) -> Self {
        Self {
            cli: cli.id().to_owned(),
            label: label.to_owned(),
            at,
        }
    }

    /// Reads [`Self::text`]; anything else is no record.
    fn parse(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        if lines.next()? != SPACING_HEAD {
            return None;
        }
        let mut words = lines.next()?.split(' ');
        let started = Self {
            cli: words.next()?.to_owned(),
            label: words.next()?.to_owned(),
            at: words.next()?.parse().ok()?,
        };
        (words.next().is_none()
            && lines.next().is_none()
            && super::accounts::admissible_label(&started.label))
        .then_some(started)
    }

    /// The file's text: the head line, then `<cli> <label> <at>`.
    fn text(&self) -> String {
        format!("{SPACING_HEAD}\n{} {} {}\n", self.cli, self.label, self.at)
    }
}

/// THE SPACING LAW. An automatic sign-in of `cli`'s `label` at `now` waits until the returned
/// time when `last` is another account's start within [`SPACING_S`] of `now`, either way: a clock
/// set back holds it at most [`SPACING_S`] past the record, never longer. The same account's
/// own retries are the ladder's backoff's to space ([`backoff_after`]), not this law's.
#[must_use]
pub(super) fn spaced_until(
    last: Option<&Started>,
    cli: super::accounts::Cli,
    label: &str,
    now: i64,
) -> Option<i64> {
    let last = last.filter(|last| last.cli != cli.id() || last.label != label)?;
    (now.abs_diff(last.at) < SPACING_S.unsigned_abs()).then(|| last.at.saturating_add(SPACING_S))
}

/// Where `store`'s Mac keeps its last sign-in start ([`SIGNIN_SPACING_FILE`]).
fn spacing_path(store: &super::accounts::Store) -> std::path::PathBuf {
    store.shared_dir().join(SIGNIN_SPACING_FILE)
}

/// The last sign-in `store`'s Mac started, where its record reads as one.
///
/// # Errors
///
/// The record exists and could not be read.
pub(super) fn last_started(store: &super::accounts::Store) -> std::io::Result<Option<Started>> {
    Ok(super::accounts::read_bounded(&spacing_path(store), 4096)?
        .as_deref()
        .and_then(Started::parse))
}

/// Why this Mac may start one sign-in. There is no other reason to start one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Why {
    /// A session on this Mac runs on the row, or a launch here starts on it, and its login is
    /// signed out here.
    Session,
    /// A person named the account: `accounts login|renew <label>`, `harness agy login
    /// <account>`, `signin --cli grok` (the default home, whose device code goes to them), or a
    /// selection saved from the window's account menu.
    Asked,
    /// One due login this Mac uses ([`super::accounts::renewal_due`]).
    Due,
    /// An account a root approved for this Mac's first custody (a shared adoption).
    Granted,
}

impl Why {
    /// Whether a sign-in for this reason keeps [`SPACING_S`] from another account's start. A
    /// person's sign-in of the one account they named starts when they ask, and still sets the
    /// clock the automatic ones keep.
    #[must_use]
    pub(super) const fn spaced(self) -> bool {
        !matches!(self, Self::Asked)
    }
}

/// One sign-in this Mac may start for one account. Its fields are private and it is neither
/// `Clone` nor `Copy`: only the doors below mint one, and [`SignIn::begin`] consumes it.
#[derive(Debug)]
#[must_use]
pub(super) struct SignIn {
    cli: super::accounts::Cli,
    label: String,
    why: Why,
}

/// Why [`SignIn::begin`] started nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Refused {
    /// The token names another account.
    Another,
    /// Another account's sign-in started here within [`SPACING_S`]; not before this time.
    Spaced {
        /// Unix seconds.
        until: i64,
    },
    /// The spacing record could not be read or written.
    Record(String),
}

impl Refused {
    /// Its words at `now`, for a person's terminal or a log line.
    #[must_use]
    pub(super) fn words(&self, now: i64) -> String {
        match self {
            Self::Another => "its sign-in token names another account".to_owned(),
            Self::Spaced { until } => format!(
                "another account's sign-in started on this Mac less than {} minutes ago, and two \
                 start at least that far apart, so this one may start in {} minutes",
                SPACING_S / 60,
                until.saturating_sub(now).max(0).saturating_add(59) / 60
            ),
            Self::Record(why) => format!("the sign-in spacing record could not be kept: {why}"),
        }
    }
}

/// Whether a session on this Mac runs on `row`, whose login is signed out here, and no other
/// Mac's warm set holds it: what [`SignIn::session_sign_in`] admits, for a ranking that mints
/// nothing.
#[must_use]
pub(super) fn session_signed_out(
    row: &super::accounts::Row,
    state: &super::accounts::State,
) -> bool {
    row.held_by.is_none()
        && state.sessions.values().any(|s| s.account == row.label)
        && state
            .accounts
            .get(&row.label)
            .is_some_and(|a| a.current_auth(row) == super::accounts::Auth::SignedOut)
}

/// Whether a door the upkeep holds admits `row` (of the bound roster), so aterm signs it in by
/// itself with nobody asking: a session here runs on it and its login is signed out here
/// ([`session_signed_out`]), or this Mac used its login within a week, it has ended or comes due,
/// and it is not one of `others`, the rows whose account may be another Mac's
/// ([`super::accounts::renewal_due`]). What a report reads before it promises a sign-in.
#[must_use]
pub(super) fn upkeep_signs_in(
    row: &super::accounts::Row,
    (logins, state, others): (
        &super::accounts::Logins,
        &super::accounts::State,
        &super::accounts::OtherMacs,
    ),
    now: i64,
) -> bool {
    session_signed_out(row, state) || super::accounts::renewal_due(row, logins, state, others, now)
}

/// Why neither door the upkeep holds admits `row` ([`upkeep_signs_in`] false), in a report's
/// words: the first condition of [`super::accounts::renewal_due`] it fails, `others` being the
/// rows whose account the source gives to other Macs alone or whose source this Mac cannot
/// compare.
#[must_use]
pub(super) fn upkeep_waits_because(
    row: &super::accounts::Row,
    state: &super::accounts::State,
    others: &super::accounts::OtherMacs,
    now: i64,
) -> &'static str {
    if row.held_by.is_some() {
        "this Mac is not in its warm set"
    } else if row.email.is_none() {
        "it names no email"
    } else if others.named.contains_key(&row.label) {
        "its source gives its account to another Mac"
    } else if !super::accounts::ran_here(state, &row.label, now) {
        "no session here runs on it or ran on it within a week"
    } else if !super::accounts::claim_ours_or_none(state, &row.label, now) {
        "another Mac holds its claim"
    } else if others.unread.contains(&row.label) {
        others
            .unread_why
            .unwrap_or(super::accounts::UnreadWhy::Source)
            .words()
    } else {
        "its login is not on record as ended or due here"
    }
}

impl SignIn {
    /// The upkeep's door for a row a session on this Mac runs on whose login is signed out here
    /// (and that no other Mac's warm set holds): [`session_signed_out`].
    #[must_use]
    pub(super) fn session_sign_in(
        cli: super::accounts::Cli,
        row: &super::accounts::Row,
        state: &super::accounts::State,
    ) -> Option<Self> {
        session_signed_out(row, state).then(|| Self {
            cli,
            label: row.label.clone(),
            why: Why::Session,
        })
    }

    /// The renewal door: `row` (of the bound roster) is a login this Mac's upkeep renews by
    /// itself ([`super::accounts::renewal_due`]), so this Mac ran on it within a week and holds its
    /// senior claim, or no Mac claims it, and it is not one of `others`, the rows whose account
    /// the source gives to other Macs alone or whose source this Mac cannot compare.
    #[must_use]
    pub(super) fn renewal_sign_in(
        cli: super::accounts::Cli,
        row: &super::accounts::Row,
        (logins, state, others): (
            &super::accounts::Logins,
            &super::accounts::State,
            &super::accounts::OtherMacs,
        ),
        now: i64,
    ) -> Option<Self> {
        super::accounts::renewal_due(row, logins, state, others, now).then(|| Self {
            cli,
            label: row.label.clone(),
            why: Why::Due,
        })
    }

    /// A person's command naming the account ([`super::cli::AskedSignIn`]).
    pub(super) fn asked_sign_in(asked: super::cli::AskedSignIn) -> Self {
        let (cli, label) = asked.account();
        Self {
            cli,
            label,
            why: Why::Asked,
        }
    }

    /// A person's selection from the window's account menu, saved as `state`'s pending request
    /// and served by the upkeep or the menu's own thread: `request` must be the one `state` holds.
    #[must_use]
    pub(super) fn selected_sign_in(
        cli: super::accounts::Cli,
        request: &super::accounts::PinRequest,
        state: &super::accounts::State,
    ) -> Option<Self> {
        (state.pending_pin.as_ref() == Some(request)).then(|| Self {
            cli,
            label: request.label().to_owned(),
            why: Why::Asked,
        })
    }

    /// The row a launch on this Mac starts on, whose login its own check refused. Only a Unix
    /// launch renews a refused login ([`super::accounts_pool::prepare_google_worker`]).
    #[cfg(unix)]
    pub(super) fn launch_sign_in(
        _door: impl super::provider_read::LaunchDoor,
        cli: super::accounts::Cli,
        label: &str,
    ) -> Self {
        Self {
            cli,
            label: label.to_owned(),
            why: Why::Session,
        }
    }

    /// The first local custody of an account a root approved for this Mac: the grant is a
    /// person's decision naming the account, made elsewhere, so it is spaced as the upkeep's own
    /// sign-ins are.
    pub(super) fn granted_sign_in(
        _grant: &super::cli::shared_renewal::Grant,
        cli: super::accounts::Cli,
        label: &str,
    ) -> Self {
        Self {
            cli,
            label: label.to_owned(),
            why: Why::Granted,
        }
    }

    /// Fixture authority for an isolated account. No release build exposes it.
    #[cfg(test)]
    pub(super) fn fixture(cli: super::accounts::Cli, label: &str, why: Why) -> Self {
        Self {
            cli,
            label: label.to_owned(),
            why,
        }
    }

    /// Whether it names `cli`'s `label`.
    #[must_use]
    pub(super) fn names(&self, cli: super::accounts::Cli, label: &str) -> bool {
        self.cli == cli && self.label == label
    }

    /// When it may start, where not now ([`spaced_until`]), as `store`'s record reads: a check
    /// a caller makes before it claims anything else, so a spaced sign-in costs nothing. A
    /// person's sign-in is never spaced.
    ///
    /// # Errors
    ///
    /// An automatic sign-in's record could not be read: it starts nothing ([`Refused::Record`]).
    pub(super) fn spaced(
        &self,
        store: &super::accounts::Store,
        now: i64,
    ) -> Result<Option<i64>, Refused> {
        if !self.why.spaced() {
            return Ok(None);
        }
        let last = last_started(store).map_err(|e| Refused::Record(e.to_string()))?;
        Ok(spaced_until(last.as_ref(), self.cli, &self.label, now))
    }

    /// Starts this sign-in of `cli`'s `label` under the sign-in lock `signin` took from `store`
    /// ([`super::accounts::Store::signin_now`], [`super::accounts::Store::signin_within`]):
    /// it refuses a token for another account, and an automatic sign-in within [`SPACING_S`] of
    /// another account's start, then records this start for the next one. The lock moves into
    /// the [`Begun`] the provider flow holds until it ends.
    ///
    /// # Errors
    ///
    /// [`Refused`]: nothing was recorded and nothing may start.
    pub(super) fn begin(
        self,
        cli: super::accounts::Cli,
        label: &str,
        signin: super::accounts::Lock,
        store: &super::accounts::Store,
        now: i64,
    ) -> Result<Begun, Refused> {
        if !self.names(cli, label) {
            return Err(Refused::Another);
        }
        if let Some(until) = self.spaced(store, now)? {
            return Err(Refused::Spaced { until });
        }
        super::upgrade_models::write_atomic(
            &spacing_path(store),
            &Started::new(cli, label, now).text(),
        )
        .map_err(|e| Refused::Record(e.to_string()))?;
        Ok(Begun {
            signin,
            cli: self.cli,
            label: self.label,
        })
    }
}

/// A sign-in [`SignIn::begin`] started: it holds the sign-in lock until the provider flow ends.
/// The functions that start a provider's login take one.
#[derive(Debug)]
#[must_use]
pub(super) struct Begun {
    signin: super::accounts::Lock,
    cli: super::accounts::Cli,
    label: String,
}

impl Begun {
    /// The sign-in lock it holds.
    #[must_use]
    pub(super) fn lock(&self) -> &super::accounts::Lock {
        &self.signin
    }

    /// Whether it is `cli`'s `label`'s sign-in.
    #[must_use]
    pub(super) fn names(&self, cli: super::accounts::Cli, label: &str) -> bool {
        self.cli == cli && self.label == label
    }
}

/// A person's sign-in of `cli`'s `label`, for a test that drives one sign-in.
#[cfg(test)]
pub(super) fn asked_fixture(cli: super::accounts::Cli, label: &str) -> SignIn {
    SignIn::fixture(cli, label, Why::Asked)
}

/// A started sign-in of `cli`'s `label` under `signin`, for a test that drives the provider flow
/// below [`SignIn::begin`]; it records no start.
#[cfg(test)]
pub(super) fn begun_fixture(
    signin: super::accounts::Lock,
    cli: super::accounts::Cli,
    label: &str,
) -> Begun {
    Begun {
        signin,
        cli,
        label: label.to_owned(),
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

/// The ladder reads a row's mailbox from the lab registry (one-mesh design step A9).
#[cfg(test)]
mod registry_tests {
    use super::super::{
        accounts::Row,
        accounts_sync::mailbox_offer,
        alab_roster,
        headless_oauth::{discover_mailbox_config_files, registry_tests as fixture},
    };
    use super::*;

    /// With no mailbox file or sheet present, the upkeep's offer for a row whose email the
    /// registry's webmail mailbox receives is that mailbox with the row's password, its login
    /// masked, and the ladder routes the sign-in through it, all without revealing a value; a
    /// registry mailbox with no password held is named as such and never tried.
    /// NEGATIVE CONTROL: with `mailbox_offer` asking `password_for` again instead of
    /// `holds_for`, the `reveals() == 0` assertion reads 1.
    #[test]
    fn the_ladder_routes_a_row_through_its_registry_mailbox_without_a_reveal() {
        let home = aterm_tempfile::tempdir().unwrap();
        let row = Row {
            label: "claude-1".to_owned(),
            email: Some("ai1@example.test".to_owned()),
            ..Row::default()
        };
        let offer = |mail| {
            alab_roster::with_mail(mail, fixture::secrets(), || {
                let cfg = discover_mailbox_config_files(Some(home.path()), None, None);
                (mailbox_offer(false, cfg, &row), alab_roster::reveals())
            })
        };
        let ((mailbox, login), reveals) = offer(vec![fixture::mailbox()]);
        assert_eq!(mailbox, Mailbox::WebmailOnly);
        assert_eq!(login.as_deref(), Some("ow…@…"));
        assert_eq!(reveals, 0, "the upkeep's look reveals nothing");
        let read = Read {
            profile: Profile::None,
            mailbox,
            mailbox_login: login,
            vault: None,
            browser: true,
            row_browser: false,
            hosts: 0,
        };
        assert_eq!(read.route(), Some(Route::Mailbox));
        assert!(read.holds_mailbox());

        let mut unheld = fixture::mailbox();
        unheld.held.clear();
        let ((mailbox, login), _) = offer(vec![unheld]);
        assert_eq!(
            (mailbox, login.as_deref()),
            (Mailbox::SharedNoPassword, Some("ow…@…"))
        );
    }
}
