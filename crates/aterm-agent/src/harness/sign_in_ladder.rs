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
//! the routes are read again. Before a ready route opens any browser or window for a row the
//! lab keys a token entry for, and before a row with none is recorded as such, the vault route
//! ([`Route::Vault`]) reads whether the row's year token is in use here, refreshing the vault
//! first (that same bounded sync, on its own clock) once in the row's sign-in workflow, since
//! another Mac may have minted the token in aterm's window, or verified one, since this one last
//! applied the vault: where it is in use, the row runs on it, the attempt is recorded `on-token`
//! by the `vault` route and no browser opens. A token another Mac minted in aterm's window or
//! headless route serves here from its mint, verified or not; one it minted at a person's
//! terminal, or by a meta that names no route, serves only that Mac until verified, so the row's
//! ready route runs as before. The line also names what is never a route here:
//! the row's own desk browser profile, which opens a window and so is never used unattended, and
//! `[harness] hosts`, which renew their own logins and none of this machine's. No route copies,
//! stores or relays a login, and none sends a sign-in, its URL or a code to another machine:
//! a route that runs makes a fresh login on this machine. One exception stands, and only in
//! the headless route: where a person turned the fleet's mail relay on (`relay = true`), that
//! route's mailbox read asks the relay's Mac for the login code and names the account's
//! address to it. On macOS a Claude row's sign-in that this Mac started for itself, at its
//! own desktop, runs in a window of aterm's own instead of a headless page (`signin_window`,
//! one window at a time under its own lock, a token's window included), where the window
//! reads the row's mailbox by itself on this Mac (never the relay, never webmail) or a
//! person is at the terminal to type the code: it signs in there with that code, and the
//! window's page hands the callback to its own waiting sign-in. Claude's consent page may hold
//! `Authorize` behind a human check, which aterm never operates; where it does, the sign-in
//! ends `blocked` unless a person at the desk completes the check, and the upkeep then opens no
//! window for that row again: a person's own renewal does. Off macOS, under SSH, for another
//! machine's request and in a tab another Mac's fleet drive started or typed into there is no
//! window, and the headless route ends at Cloudflare's page.
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

impl Mailbox {
    /// Whether it ends the lab's ask for the row's mailbox password ([`Read::wants_mailbox`]):
    /// one the sign-in reads without a browser, or a webmail inbox with the row's password,
    /// whether or not a headless browser is here to read the webmail. A webmail inbox with no
    /// password ([`Mailbox::WebmailNoPassword`]) leaves the ask standing.
    pub(super) const fn holds(self) -> bool {
        matches!(self, Self::Ready | Self::WebmailOnly)
    }
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

    /// Whether the sync read the whole vault: it applied every file, or found nothing newer. A
    /// failed, partial or refused sync leaves what the vault holds unknown.
    pub(super) fn read_whole(&self) -> bool {
        matches!(self, Self::Applied(_) | Self::Unchanged(_))
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

    /// A sync that did not run to its end, by the error [`super::align::capture_bounded`] gave
    /// ([`Unfinished::of`]): one that could not be started, one ended at its `budget`, one whose
    /// output could not be read, which every other error of that capture is taken for. The
    /// error's own text, which can name a path, is not kept.
    pub(super) fn of_run_error(error: &str, budget: Duration) -> Self {
        Self::Failed(match Unfinished::of(error) {
            Some(Unfinished::TimedOut) => format!("timed out after {} s", budget.as_secs()),
            Some(why) => why.words().to_owned(),
            None => Unfinished::Unread.words().to_owned(),
        })
    }
}

/// Why a run aterm started did not run to its end, in closed words: what a record keeps of a
/// bounded capture's error ([`super::align::capture_bounded`] and its owned kin), whose own text
/// can name a path and is never kept. One reading for a vault refresh ([`Vault::of_run_error`])
/// and a sign-in's run ([`super::cli::RenewalObservation`], [`how_it_ended`]'s `cause`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Unfinished {
    /// It could not be started: its program did not spawn, or it could not be tied to this
    /// aterm, whose executable an update replaced (`it will not run…`).
    NotStarted,
    /// It ran past its deadline and was ended.
    TimedOut,
    /// Its output could not be read.
    Unread,
    /// A signal ended it: it gave no exit code.
    Signal,
    /// The Mac slept while it ran ([`interrupted_by_sleep`]): its deadline, its page and its
    /// mailbox wait all stood still, so how it ended says nothing of the sign-in.
    Interrupted,
}

impl Unfinished {
    /// The way the capture's `error` says its run did not finish; `None` for an error that says
    /// none of them (a stop its watch asked for, a reap that failed), which then gives no cause.
    pub(super) fn of(error: &str) -> Option<Self> {
        if error.starts_with("it will not run") {
            Some(Self::NotStarted)
        } else if error.contains("deadline") {
            Some(Self::TimedOut)
        } else if error.starts_with("its output") || error.starts_with("its stdout") {
            Some(Self::Unread)
        } else {
            None
        }
    }

    /// Its words, after `the sign-in did not finish: `.
    pub(super) const fn words(self) -> &'static str {
        match self {
            Self::NotStarted => "it could not be started",
            Self::TimedOut => "it timed out",
            Self::Unread => "its output could not be read",
            Self::Signal => "a signal ended it",
            Self::Interrupted => "this Mac slept while it ran",
        }
    }
}

/// The least sleep that interrupts a sign-in's run ([`interrupted_by_sleep`]): shorter than any
/// wait a sign-in page or a mailbox survives, longer than a clock's jitter.
pub(super) const INTERRUPTING_SLEEP: Duration = Duration::from_secs(30);

/// How many of one row's unattended runs in a row a sleep may interrupt and still be tried
/// again at once ([`Ladder::run_ended`]); the next one a sleep spans is counted as it ended,
/// with the backoff every failure sets. A lid shut over a Mac that wakes in the dark every
/// few minutes would otherwise end one run and start the next at every wake, all night.
pub(super) const MOST_INTERRUPTED_RUNS: u32 = 2;

/// Whether a sleep of `slept` across a run is long enough to interrupt it
/// ([`INTERRUPTING_SLEEP`]); no clock that counts sleep (`None`) shows none.
#[must_use]
pub(super) fn slept_across(slept: Option<Duration>) -> bool {
    slept.is_some_and(|slept| slept >= INTERRUPTING_SLEEP)
}

/// AN UNATTENDED ATTEMPT WHOSE RUN SPANNED A SYSTEM SLEEP IS INTERRUPTED, NOT TIMED OUT (plan
/// package FL-174). `slept` is how long the Mac slept across the run
/// ([`super::wake_deadline::SleepWatch::slept`]); the run is interrupted where that reaches
/// [`INTERRUPTING_SLEEP`], it made no login (`published` false) and nothing a person must
/// answer stopped it (`stopped` false: a bot protection, a closed window, a wrong account and a
/// desk with no desktop stay what they are, sleep or no sleep). An interrupted run of the
/// ladder's own is tried again, counted neither failed nor blocked and starting no backoff,
/// for as long as its row's runs in a row a sleep interrupted number no more than
/// [`MOST_INTERRUPTED_RUNS`] ([`Ladder::run_ended`]); it gives its spacing back only where it
/// never reached the provider ([`Begun::refund_spacing`]). A run a person started (a click, a
/// saved choice, a Next selection) is never waived by this, whatever it answers: its caller
/// counts it as it ended and retires its request, since a saved choice left pending is run
/// again by every pass with no cooldown. On 2026-11-15 two unattended runs "timed out" after
/// 718 s and 1016 s against a 240 s bound: the lid was shut over both.
#[must_use]
pub(super) fn interrupted_by_sleep(
    slept: Option<Duration>,
    published: bool,
    stopped: bool,
) -> bool {
    !published && !stopped && slept_across(slept)
}

/// A route the ladder found ready ([`Read::route`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Route {
    /// The mailbox answers the sign-in's email; a headless browser is driven where one is found.
    Mailbox,
    /// The row's own headless browser profile, driven headless.
    Profile,
    /// The row's year token in the vault, in use here: no browser and no login. Never what
    /// [`Read::route`] returns: the ladder takes it before a ready route runs, and before a row
    /// with none is recorded as such ([`on_token_words`]).
    Vault,
}

impl Route {
    /// Its word.
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Mailbox => "mailbox",
            Self::Profile => "profile",
            Self::Vault => "vault",
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

    /// Whether the row's mailbox ends the lab's ask for it ([`Mailbox::holds`]).
    pub(super) const fn holds_mailbox(&self) -> bool {
        self.mailbox.holds()
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

/// How one run of a sign-in ended, in the one wording every place that says it reads: the row's
/// record from the upkeep's ladder, whichever custody ran it, a renewal run by hand
/// ([`super::cli`]'s `note_hand_renewal`) and its terminal lines, and a shared-custody run's
/// diagnostic, which the fact menu's `LoginDiagnostic` shows
/// ([`super::cli::RenewalObservation::ended`]). The run is `the sign-in`, which is true of every
/// CLI's. A run that made no login is named by how it exited (`code`, where the run gave one),
/// never read as the vendor's refusal, and then by what its opener's sign-in met where its
/// outcome file names it (`met`: Cloudflare's bot check or block page, no callback in time, or a
/// failure with the opener's own reason; [`super::open_url::Met::words`]), so a bot check is
/// named on record. A run that did not finish says how it exited where its exit is why (a
/// signal's 128 + n, or a negative code: not 0, which a check after the run failed), then what
/// it met, else why it could not be started or read (`cause`: [`Unfinished::words`] on record),
/// where any is known. A run that ended where only a person goes on (a bot check that did not
/// clear, or another CLI's stop for a person) is what it met alone, since that may be a missing
/// desktop session, the consent page's human check, a window a person closed or a consent page
/// that named another account, and no bot check. A run this Mac deferred for a fresh attempt
/// says the deferral, then how the run itself ended (its exit, what it met, else its cause),
/// since the deferral is this Mac's and the rest is the run's. Each says how the run ended once.
/// What it met never enters the ladder's backoff key, which is what the ladder read before the
/// run ([`Ladder::failed`]).
pub(super) fn how_it_ended(
    renewal: super::cli::Renewal,
    code: Option<i32>,
    met: Option<&super::open_url::Met>,
    cause: Option<&str>,
) -> String {
    use super::cli::Renewal;
    let met = met.map(super::open_url::Met::words);
    match (renewal, met) {
        (Renewal::AuthFailed, None) => match code {
            Some(code) => format!("the sign-in exited {code} with no login made"),
            None => "the sign-in ended with no login made".to_owned(),
        },
        (Renewal::AuthFailed, Some(met)) => match code {
            Some(code) => format!("the sign-in exited {code}; {met}"),
            None => format!("the sign-in ended; {met}"),
        },
        (Renewal::Failed, met) => {
            let code = code.filter(|code| !(0..=127).contains(code));
            match (code, met, cause) {
                (Some(code), Some(met), _) => {
                    format!("the sign-in did not finish: it exited {code}; {met}")
                }
                (Some(code), None, _) => format!("the sign-in did not finish: it exited {code}"),
                (None, Some(met), _) => format!("the sign-in did not finish; {met}"),
                (None, None, Some(why)) => format!("the sign-in did not finish: {why}"),
                (None, None, None) => "the sign-in did not finish".to_owned(),
            }
        }
        (Renewal::BotCheck | Renewal::NeedsPerson, Some(met)) => met,
        (Renewal::BotCheck, None) => {
            "it met Cloudflare's bot check; aterm pressed nothing there".to_owned()
        }
        (Renewal::NeedsPerson, None) => "its page waited on a person".to_owned(),
        (Renewal::Deferred, met) => {
            let deferred = "deferred: local sign-in state changed or was unavailable";
            let ran = match (code, met, cause) {
                (Some(code), Some(met), _) => format!("the sign-in exited {code}; {met}"),
                (Some(code), None, _) => format!("the sign-in exited {code}"),
                (None, Some(met), _) => met,
                (None, None, Some(why)) => format!("the sign-in did not finish: {why}"),
                (None, None, None) => return deferred.to_owned(),
            };
            format!("{deferred}; {ran}")
        }
        (Renewal::Renewed, _) => "signed in".to_owned(),
        (Renewal::OtherRenewed, _) => "another configured account signed in".to_owned(),
        (Renewal::WrongAccount, _) => "another account signed in".to_owned(),
        (Renewal::SigninHeld, _) => "sign-in admission was held, so nothing ran".to_owned(),
    }
}

/// The record of a run of `route` that did not sign the row in: the route, how it ended
/// ([`how_it_ended`]) and every route the ladder read ([`Read::words`]).
pub(super) fn failed_words(route: Route, how: &str, read: &Read) -> String {
    format!("{}: {how}; {}", route.as_str(), read.words())
}

/// What the vault route says where it found the row's year token in use here, ending at `until`
/// (unix seconds), then every route the ladder read, the vault's refresh among them where one
/// ran: `vault: its year token is in use here until 2027-11-12; no browser opened; profile(ready)
/// mailbox(none configured) vault(applied seq 52) …`. The attempt's record is the route's word
/// alone, as a route that signed the row in records its own.
pub(super) fn on_token_words(until: u64, read: &Read) -> String {
    let (y, m, d) =
        aterm_types::rfc3339::civil_from_days(i64::try_from(until / 86_400).unwrap_or(i64::MAX));
    format!(
        "{}: its year token is in use here until {y:04}-{m:02}-{d:02}; no browser opened; {}",
        Route::Vault.as_str(),
        read.words()
    )
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
    /// That page was a bot protection, which only a person goes on from: no clock and no new
    /// input ends this wait, only the saved record ([`Ladder::page_ended`]).
    for_a_person: bool,
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

/// An opaque digest of this home's vault source and this Mac's standing with that vault.
pub(super) type VaultContext = [u8; 32];

/// What the ladder remembers in this process between upkeeps: each failing row's backoff, the
/// vault refresh's clock, and the sign-in workflow whose vault route each row has refreshed for.
#[derive(Debug, Default)]
pub(super) struct Ladder {
    backoff: BTreeMap<String, Backoff>,
    vault: Option<VaultRun>,
    /// Last observed context survives a network move that discards the cached result.
    context: Option<VaultContext>,
    /// The workflow each row's vault route last refreshed the vault for
    /// ([`Self::token_refreshed`]).
    token_refreshed: BTreeMap<String, super::accounts::WorkflowToken>,
    /// How many of each row's unattended runs in a row a sleep interrupted
    /// ([`Self::run_ended`]).
    interrupted: BTreeMap<String, u32>,
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
    /// A row that waits for a person has none ([`Self::page_stopped`]).
    pub(super) fn retry_at(&self, label: &str, now: Instant) -> Option<Instant> {
        self.backoff
            .get(label)
            .filter(|backoff| !backoff.for_a_person)
            .map(|backoff| backoff.next)
            .filter(|next| *next > now)
    }

    /// Whether `label`'s ladder waits out its backoff at `now`: it does while the wait set at its
    /// last failure stands and the machine reads as it did then (`inputs`); a new mailbox, a
    /// browser installed, a vault delivery or a profile made ends the wait at once, though a row a
    /// page stopped is held on record before it gets here. Once a different input is observed,
    /// returning to the previous input cannot restore its wait. A row a bot protection stopped
    /// waits for a person whatever it reads and however long it waits ([`Self::page_stopped`]).
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
        if self.backoff.get(label).is_some_and(|b| b.for_a_person) {
            return true;
        }
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
                for_a_person: false,
            },
        );
        wait
    }

    /// NOTES HOW ONE OF `label`'S UNATTENDED RUNS ENDED AGAINST THE MAC'S SLEEP, and returns
    /// whether the run is waived: tried again at once and counted against nothing. `slept`:
    /// the Mac slept across it ([`slept_across`]); `interrupted`: it is one a sleep interrupted
    /// ([`interrupted_by_sleep`]) that nothing else ended. A run no sleep spanned ends the
    /// row's count, however it ended. An interrupted one adds to it and is waived while the
    /// count is no more than [`MOST_INTERRUPTED_RUNS`]; past that it is not, and its caller
    /// counts it as it ended ([`Self::failed`]), so the row's backoff holds the next. The count
    /// stands until a run of the row ends with no sleep across it.
    pub(super) fn run_ended(&mut self, label: &str, slept: bool, interrupted: bool) -> bool {
        if !slept {
            self.interrupted.remove(label);
            return false;
        }
        if !interrupted {
            return false;
        }
        let in_a_row = self.interrupted.entry(label.to_owned()).or_default();
        *in_a_row = in_a_row.saturating_add(1);
        *in_a_row <= MOST_INTERRUPTED_RUNS
    }

    /// Notes that a page stopped `label`'s last failure, which it waits on until `until`, or, with
    /// `None`, until a person goes on from it: a bot protection's check or block page, which
    /// nothing here tries again on a clock (`docs/DESIGN-claude-sign-in-sitting-2026-11-12.md`
    /// §3). No move ends either wait ([`Self::moved`]), and the saved record says when it is over
    /// ([`Self::page_ended`]).
    pub(super) fn page_stopped(&mut self, label: &str, until: Option<Instant>) {
        if let Some(backoff) = self.backoff.get_mut(label) {
            backoff.page_stopped = true;
            match until {
                Some(until) => backoff.next = backoff.next.max(until),
                None => backoff.for_a_person = true,
            }
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

    /// Whether `label`'s vault route has refreshed the vault in `workflow` already: one refresh
    /// per sign-in workflow, never one per pass ([`Self::note_token_refreshed`]).
    pub(super) fn token_refreshed(
        &self,
        label: &str,
        workflow: super::accounts::WorkflowToken,
    ) -> bool {
        self.token_refreshed.get(label) == Some(&workflow)
    }

    /// Notes that `label`'s vault route had the vault's refresh in `workflow`: one that read the
    /// whole vault ([`Vault::read_whole`]), never one that failed.
    pub(super) fn note_token_refreshed(
        &mut self,
        label: &str,
        workflow: super::accounts::WorkflowToken,
    ) {
        self.token_refreshed.insert(label.to_owned(), workflow);
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
            reason = "Observe a bounded local vault source/membership fingerprint."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "VaultRefreshContext",
            action = "ContextB",
            reason = "Observe a different bounded local vault source/membership fingerprint."
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

    /// What the vault's last refresh found, where it started no earlier than `since`, has
    /// ended, and no observed change has made it stale since: a reading of the vault fresher
    /// than `since` (plan package FL-138's token upkeep reads one before it makes a token).
    #[cfg(unix)] // read by the unix-only token upkeep (`accounts_sync::upkeep_tokens`)
    pub(super) fn vault_seen_since(&self, since: Instant) -> Option<Vault> {
        self.vault
            .as_ref()
            .filter(|run| !run.invalidated && run.at >= since)
            .and_then(|run| run.seen.clone())
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
    /// ([`Self::page_stopped`]): most of what counts as a move has no new address behind it (a fleet's
    /// peers all day), and ending that wait
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
// a row a session here runs on whose login is on record as signed out here (and the row a launch
// here starts on, whose login was refused), a person naming the account, and one due login this Mac uses
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

/// What [`space_token_capture`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(unix)] // read by the unix-only token upkeep (`accounts_sync::upkeep_tokens`)
pub(super) enum TokenSpacing {
    /// The capture may start now: its start is this Mac's last sign-in start.
    Started,
    /// Another account's sign-in started here within [`SPACING_S`]; not before this time (Unix
    /// seconds).
    Until(i64),
    /// The sign-in lock is held (a sign-in runs here), or the record could not be read or
    /// written: nothing starts on this pass.
    Busy,
}

/// SPACES A TOKEN CAPTURE THIS MAC'S UPKEEP MAKES WITH NOBODY THERE (plan package FL-138) as an
/// automatic sign-in of Claude Code's `label` at `now`: its page may sign the account's window
/// store in, which the provider sees as a sign-in, so another account's start within
/// [`SPACING_S`] holds it ([`spaced_until`]), and its own start is recorded as this Mac's last.
/// Both are read and written under the sign-in lock, taken without waiting and let go before
/// the capture runs: a token is made under its account's lock, never `signin.lock`.
#[cfg(unix)] // read by the unix-only token upkeep (`accounts_sync::upkeep_tokens`)
pub(super) fn space_token_capture(
    store: &super::accounts::Store,
    label: &str,
    now: i64,
) -> TokenSpacing {
    let Ok(Some(_turn)) = store.signin_now() else {
        return TokenSpacing::Busy;
    };
    let Ok(last) = last_started(store) else {
        return TokenSpacing::Busy;
    };
    let cli = super::accounts::Cli::Claude;
    if let Some(until) = spaced_until(last.as_ref(), cli, label, now) {
        return TokenSpacing::Until(until);
    }
    match super::atomic_write::write_atomic(
        &spacing_path(store),
        &Started::new(cli, label, now).text(),
    ) {
        Ok(()) => TokenSpacing::Started,
        Err(_) => TokenSpacing::Busy,
    }
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

/// The file beside [`SIGNIN_SPACING_FILE`] that names the sign-in this Mac is running now. It is
/// written at every start, while the sign-in lock is held, by the process that holds it, and
/// removed when the provider flow ends ([`RunningMark`]). It names that process by its identity
/// (its number and birth, and on Apple its pid version), so a record outlives nothing: a
/// process killed mid sign-in drops the lock with it, and the record then names a process that
/// is gone, whose number another may take but not its birth.
pub(super) const SIGNIN_RUNNING_FILE: &str = "signin-running";

/// The first line of [`SIGNIN_RUNNING_FILE`]. A `v1` record named its process by number alone;
/// it reads as no record, so a build that writes one and a build that reads this one each see
/// no sign-in running.
const RUNNING_HEAD: &str = "aterm-signin-running-v2";

/// The sign-in this Mac is running: its CLI's id, its label, when it started (Unix seconds) and
/// the process that runs it, which holds the sign-in lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Running {
    cli: String,
    label: String,
    at: i64,
    holder: aterm_uds::process::Identity,
}

impl Running {
    /// Reads [`Self::text`]; anything else is no record.
    fn parse(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        if lines.next()? != RUNNING_HEAD {
            return None;
        }
        let mut words = lines.next()?.split(' ');
        let running = Self {
            cli: words.next()?.to_owned(),
            label: words.next()?.to_owned(),
            at: words.next()?.parse().ok()?,
            holder: aterm_uds::process::Identity::parse(words.next()?)?,
        };
        (words.next().is_none()
            && lines.next().is_none()
            && super::accounts::admissible_label(&running.label))
        .then_some(running)
    }

    /// The file's text: the head line, then `<cli> <label> <at> <holder>`, the holder as its
    /// identity token ([`aterm_uds::process::Identity::token`]).
    fn text(&self) -> String {
        format!(
            "{RUNNING_HEAD}\n{} {} {} {}\n",
            self.cli,
            self.label,
            self.at,
            self.holder.token()
        )
    }
}

/// The identity of the live process `pid` as a record of the sign-in lock's holder names it:
/// its number, birth and, where the kernel keeps one (Apple), pid version, which an exec moves
/// (the lock is opened close-on-exec, so a holder that exec'd holds it no more). On Apple and
/// Linux it is read of a process of this user that is no zombie
/// ([`aterm_uds::process::read_identity`]); elsewhere of its birth alone
/// ([`aterm_uds::process::read_birth`]). `None` where none can be read.
fn holder(pid: u32) -> Option<aterm_uds::process::Identity> {
    let pid = i32::try_from(pid).ok()?;
    if cfg!(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "android"
    )) {
        aterm_uds::process::read_identity(pid)
    } else {
        Some(aterm_uds::process::Identity {
            pid,
            birth: aterm_uds::process::read_birth(pid)?,
            idversion: None,
        })
    }
}

/// Where `store`'s Mac names its running sign-in ([`SIGNIN_RUNNING_FILE`]).
fn running_path(store: &super::accounts::Store) -> std::path::PathBuf {
    store.shared_dir().join(SIGNIN_RUNNING_FILE)
}

/// A started sign-in's record in [`SIGNIN_RUNNING_FILE`], removed when it drops. A record
/// another start has since replaced is left as that start's.
#[derive(Debug)]
struct RunningMark {
    path: std::path::PathBuf,
    text: String,
}

impl RunningMark {
    /// Names `cli`'s `label` as the sign-in this process runs from `at`, this process being the
    /// sign-in lock's holder. The record is a fact for readers and no admission, so a record
    /// that cannot be written, or whose holder's identity cannot be read, starts the sign-in all
    /// the same, unnamed.
    fn note(
        store: &super::accounts::Store,
        cli: super::accounts::Cli,
        label: &str,
        at: i64,
    ) -> Option<Self> {
        Self::note_by(store, cli, label, at, std::process::id())
    }

    /// [`Self::note`], naming process `pid` as the sign-in lock's holder.
    fn note_by(
        store: &super::accounts::Store,
        cli: super::accounts::Cli,
        label: &str,
        at: i64,
        pid: u32,
    ) -> Option<Self> {
        let path = running_path(store);
        let text = Running {
            cli: cli.id().to_owned(),
            label: label.to_owned(),
            at,
            holder: holder(pid)?,
        }
        .text();
        super::atomic_write::write_atomic(&path, &text).ok()?;
        Some(Self { path, text })
    }
}

impl Drop for RunningMark {
    fn drop(&mut self) {
        if super::accounts::read_bounded(&self.path, 4096)
            .ok()
            .flatten()
            .is_some_and(|text| text == self.text)
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// A started sign-in's record ([`RunningMark::note`]) for a test of a reader in another crate:
/// `cli`'s `label` reads as signing in until what it returns drops.
#[cfg(any(test, debug_assertions))]
pub(super) fn running_fixture(
    store: &super::accounts::Store,
    cli: super::accounts::Cli,
    label: &str,
    at: i64,
) -> Option<Box<dyn std::any::Any>> {
    RunningMark::note(store, cli, label, at).map(|mark| Box::new(mark) as Box<dyn std::any::Any>)
}

/// [`running_fixture`], the record naming the live process `pid` as the holder: `label` reads as
/// signing in while that process lives as itself, and its record stays when it dies, as a
/// sign-in killed mid-run leaves it, until what this returns drops.
#[cfg(any(test, debug_assertions))]
pub(super) fn running_fixture_by(
    store: &super::accounts::Store,
    cli: super::accounts::Cli,
    label: &str,
    at: i64,
    pid: u32,
) -> Option<Box<dyn std::any::Any>> {
    RunningMark::note_by(store, cli, label, at, pid)
        .map(|mark| Box::new(mark) as Box<dyn std::any::Any>)
}

/// Where `store`'s Mac names its running sign-in ([`SIGNIN_RUNNING_FILE`]), for a reader that
/// looks at the file before it reads it.
#[must_use]
pub(super) fn running_file(store: &super::accounts::Store) -> std::path::PathBuf {
    running_path(store)
}

/// The sign-in a record of [`SIGNIN_RUNNING_FILE`] names for one CLI: its row's label and the
/// process that runs it, the sign-in lock's holder, by its identity, whether or not that process
/// still lives ([`Self::lives`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningSignIn {
    label: String,
    holder: aterm_uds::process::Identity,
}

impl RunningSignIn {
    /// The label of the row the sign-in runs for.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Whether the process the record names still lives as that same process ([`holder`] reads
    /// its identity now from the process table, not from the record). A sign-in killed mid-run
    /// leaves its record, which then names nothing, even once another process takes its number.
    #[must_use]
    pub fn lives(&self) -> bool {
        holder(self.holder.pid.unsigned_abs()) == Some(self.holder)
    }

    /// The process the record names, the sign-in lock's holder, by its identity as the record
    /// holds it: what a reader watches for the exit that ends the sign-in, killed or not, while
    /// its record stays.
    #[must_use]
    pub fn holder(&self) -> aterm_uds::process::Identity {
        self.holder
    }
}

/// What [`SIGNIN_RUNNING_FILE`] names for `cli` on `store`'s Mac, alive or not: one read of the
/// file. `None` where there is no record, or it is another CLI's or no record at all.
#[must_use]
pub(super) fn running_record(
    store: &super::accounts::Store,
    cli: super::accounts::Cli,
) -> Option<RunningSignIn> {
    let running = super::accounts::read_bounded(&running_path(store), 4096)
        .ok()
        .flatten()
        .as_deref()
        .and_then(Running::parse)?;
    (running.cli == cli.id()).then_some(RunningSignIn {
        label: running.label,
        holder: running.holder,
    })
}

/// The label of `cli`'s row whose sign-in runs on `store`'s Mac now, in this process or another:
/// what [`SIGNIN_RUNNING_FILE`] names while the process it names, the sign-in lock's holder,
/// still lives as that same process ([`running_record`], [`RunningSignIn::lives`]).
#[must_use]
pub(super) fn signing_in(
    store: &super::accounts::Store,
    cli: super::accounts::Cli,
) -> Option<String> {
    running_record(store, cli)
        .filter(RunningSignIn::lives)
        .map(|running| running.label)
}

/// Why this Mac may start one sign-in. There is no other reason to start one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Why {
    /// A session on this Mac runs on the row and its login is on record as signed out here, or a
    /// launch here starts on it and its own check refused its login.
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
    turn: Option<super::accounts::SigninTurn>,
    selection_owner: Option<super::accounts_sync::ManualSelectionOwner>,
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

/// Whether a session on this Mac runs on `row`, whose login's last word on record here is
/// [`super::accounts::Auth::SignedOut`], and no other Mac's warm set holds it: what
/// [`SignIn::session_sign_in`] admits, for a ranking that mints nothing. A lapse its credential
/// file alone shows ([`super::accounts::lapsed_here`]), which
/// [`super::accounts::signed_out_here`] counts, admits nothing here: the upkeep starts no sign-in
/// on its own for a lapsed file login (one-mesh AC1). Once the session meets its wall, which
/// records the sign-out, this door admits the row.
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
/// itself with nobody asking: a session here runs on it and its login is on record as signed out
/// here ([`session_signed_out`]), or this Mac used its login within a week, its records say it has
/// ended or comes due, and it is not one of `others`, the rows whose account may be another Mac's
/// ([`super::accounts::renewal_due`]). Neither admits a row on its vault token here
/// ([`super::accounts::OtherMacs::on_token`]): renewal is off for it. What a report reads before
/// it promises a sign-in.
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
    !others.on_token.contains_key(&row.label)
        && (session_signed_out(row, state)
            || super::accounts::renewal_due(row, logins, state, others, now))
}

/// Why neither door the upkeep holds admits `row` ([`upkeep_signs_in`] false), in a report's
/// words: the first condition of [`super::accounts::renewal_due`] it fails, `others` being the
/// rows whose account the source gives to other Macs alone or whose source this Mac cannot
/// compare, and the rows on their vault token here. Where it fails only the last (its records say its login has neither ended nor comes
/// due) and its own credential file shows the login lapsed ([`super::accounts::lapsed_here`]),
/// the words name that file.
#[must_use]
pub(super) fn upkeep_waits_because(
    row: &super::accounts::Row,
    state: &super::accounts::State,
    others: &super::accounts::OtherMacs,
    now: i64,
) -> &'static str {
    if row.held_by.is_some() {
        "this Mac is not in its warm set"
    } else if others.on_token.contains_key(&row.label) {
        "it runs on its vault token here, and renewal is off for it"
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
    } else if super::accounts::lapsed_here(row, state) {
        // It reads expired ([`super::accounts::login_state`]), but the renewal door opens only on
        // its records ([`super::accounts::renewal_due`]), never on the file alone.
        "its credential file here shows its login lapsed, and a file alone never starts aterm's \
         own sign-in"
    } else {
        "its login is not on record as ended or due here"
    }
}

impl SignIn {
    /// The upkeep's door for a row a session on this Mac runs on whose login is on record as
    /// signed out here (and that no other Mac's warm set holds): [`session_signed_out`].
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
            turn: None,
            selection_owner: None,
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
            turn: None,
            selection_owner: None,
        })
    }

    /// A person's command naming the account ([`super::cli::AskedSignIn`]).
    pub(super) fn asked_sign_in(asked: super::cli::AskedSignIn) -> Self {
        let (cli, label) = asked.account();
        Self {
            cli,
            label,
            why: Why::Asked,
            turn: None,
            selection_owner: None,
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
        (state
            .pending_pin
            .as_ref()
            .and_then(super::accounts::PendingSelection::account)
            == Some(request))
        .then(|| Self {
            cli,
            label: request.label().to_owned(),
            why: Why::Asked,
            turn: None,
            selection_owner: None,
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
            turn: None,
            selection_owner: None,
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
            turn: None,
            selection_owner: None,
        }
    }

    /// Fixture authority for an isolated account. No release build exposes it.
    #[cfg(test)]
    pub(super) fn fixture(cli: super::accounts::Cli, label: &str, why: Why) -> Self {
        Self {
            cli,
            label: label.to_owned(),
            why,
            turn: None,
            selection_owner: None,
        }
    }

    /// Whether it names `cli`'s `label`.
    #[must_use]
    pub(super) fn names(&self, cli: super::accounts::Cli, label: &str) -> bool {
        self.cli == cli && self.label == label
    }

    /// Retain the already acquired manual turn with this one-use authority token.
    /// An existing admission is never replaced; a refused operation retires both guards.
    pub(super) fn admitted(
        mut self,
        turn: super::accounts::SigninTurn,
        owner: super::accounts_sync::ManualSelectionOwner,
    ) -> Result<Self, Refused> {
        if self.turn.is_some() || self.selection_owner.is_some() {
            return Err(Refused::Record(
                "sign-in admission is already retained".into(),
            ));
        }
        self.turn = Some(turn);
        self.selection_owner = Some(owner);
        Ok(self)
    }

    pub(super) fn has_turn(&self) -> bool {
        self.turn.is_some()
    }

    pub(super) fn selection(&self) -> Option<&super::accounts::PendingSelection> {
        self.selection_owner
            .as_ref()
            .map(super::accounts_sync::ManualSelectionOwner::selection)
    }

    /// Transfer retained admission exactly once into the existing provider-plan guard.
    pub(super) fn take_turn(&mut self) -> Option<super::accounts::SigninTurn> {
        self.turn.take()
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

    pub(super) fn begin_native(
        self,
        label: &str,
        turn: std::sync::Arc<super::native_enrollment_admission::NativeBrowserTurn>,
        store: &super::accounts::Store,
        now: i64,
    ) -> Result<Begun, Refused> {
        super::native_custody_platform::require_available()
            .map_err(|error| Refused::Record(error.into()))?;
        if !self.names(super::accounts::Cli::Codex, label)
            || !turn.names(label)
            || self.turn.is_some()
        {
            return Err(Refused::Another);
        }
        turn.check_preparation(store).map_err(Refused::Record)?;
        #[cfg(unix)]
        let _spacing = aterm_uds::private_file::PrivateLock::take(
            &store.shared_dir(),
            "native-signin-spacing.lock",
            std::time::Duration::from_secs(1),
            "sign-in spacing busy",
        )
        .map_err(|_| Refused::Record("sign-in spacing unavailable".into()))?;
        if let Some(until) = self.spaced(store, now)? {
            return Err(Refused::Spaced { until });
        }
        super::atomic_write::write_atomic(
            &spacing_path(store),
            &Started::new(super::accounts::Cli::Codex, label, now).text(),
        )
        .map_err(|error| Refused::Record(error.to_string()))?;
        Ok(Begun {
            _running: RunningMark::note(store, super::accounts::Cli::Codex, label, now),
            signin: SigninOwner::Native(turn),
            cli: self.cli,
            label: self.label,
            selection_owner: self.selection_owner,
            spacing: None,
        })
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
        signin: super::accounts::SigninTurn,
        store: &super::accounts::Store,
        now: i64,
    ) -> Result<Begun, Refused> {
        if !self.names(cli, label) {
            return Err(Refused::Another);
        }
        if self.turn.is_some() {
            return Err(Refused::Record(
                "retained admission must move into the provider plan".into(),
            ));
        }
        #[cfg(unix)]
        signin
            .check_store(store)
            .map_err(|error| Refused::Record(error.to_string()))?;
        if let Some(until) = self.spaced(store, now)? {
            return Err(Refused::Spaced { until });
        }
        // The record this start replaces, kept so a run that is superseded or interrupted
        // before it reaches the provider gives its spacing back ([`Begun::refund_spacing`]); one
        // that does not read is none.
        let before = last_started(store).ok().flatten();
        let own = Started::new(cli, label, now);
        super::atomic_write::write_atomic(&spacing_path(store), &own.text())
            .map_err(|e| Refused::Record(e.to_string()))?;
        Ok(Begun {
            _running: RunningMark::note(store, cli, label, now),
            signin: SigninOwner::Global(signin),
            cli: self.cli,
            label: self.label,
            selection_owner: self.selection_owner,
            spacing: Some((own, before)),
        })
    }
}

/// A sign-in [`SignIn::begin`] started: it holds the sign-in lock until the provider flow ends,
/// and this Mac's record that the sign-in runs ([`signing_in`]) for as long. The functions that
/// start a provider's login take one.
#[derive(Debug)]
#[must_use]
pub(super) struct Begun {
    /// Dropped before the lock is released, so no record outlives its sign-in's lock. `None`
    /// for a recovery, which starts no provider, and where the record could not be written.
    _running: Option<RunningMark>,
    signin: SigninOwner,
    cli: super::accounts::Cli,
    label: String,
    selection_owner: Option<super::accounts_sync::ManualSelectionOwner>,
    /// This start's own spacing record and the one it replaced ([`Self::refund_spacing`]);
    /// `None` for a start that records its spacing elsewhere or records none.
    spacing: Option<(Started, Option<Started>)>,
}

#[derive(Debug)]
enum SigninOwner {
    Global(super::accounts::SigninTurn),
    Native(std::sync::Arc<super::native_enrollment_admission::NativeBrowserTurn>),
    NativeRecovery(Box<super::native_enrollment_admission::NativeRecoveryTurn>),
}

impl Begun {
    pub(super) fn native_recovery(
        turn: super::native_enrollment_admission::NativeRecoveryTurn,
        store: &super::accounts::Store,
        label: &str,
    ) -> Result<Self, String> {
        if turn.admission().request().label != label {
            return Err("native recovery row changed".into());
        }
        turn.check_store(store)?;
        Ok(Self {
            _running: None,
            signin: SigninOwner::NativeRecovery(Box::new(turn)),
            cli: store.cli(),
            label: label.to_owned(),
            selection_owner: None,
            spacing: None,
        })
    }

    /// GIVES BACK THE SPACING THIS START SPENT, for a run that was superseded or that a sleep
    /// interrupted ([`interrupted_by_sleep`]) before it reached the provider: its caller calls
    /// this only where the run never started its provider's step, so it loaded no sign-in the
    /// provider counts and the next account's sign-in does not wait [`SPACING_S`] on it. A run
    /// that started that step keeps its spacing however it ended: its page may have loaded.
    /// Where the spacing record is still this start's own, the record it replaced is put back,
    /// or the file removed where it replaced none. Called while this sign-in still holds the
    /// sign-in lock, under which alone the record is written, so no other start's record is
    /// undone.
    pub(super) fn refund_spacing(&self, store: &super::accounts::Store) {
        let Some((own, before)) = &self.spacing else {
            return;
        };
        if last_started(store).ok().flatten().as_ref() != Some(own) {
            return;
        }
        let path = spacing_path(store);
        match before {
            Some(before) => {
                let _ = super::atomic_write::write_atomic(&path, &before.text());
            }
            None => {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    pub(super) fn native_recovery_scope(
        &self,
    ) -> Option<&super::native_enrollment_admission::NativeRecoveryTurn> {
        match &self.signin {
            SigninOwner::NativeRecovery(turn) => Some(turn),
            _ => None,
        }
    }

    /// A checked publication-only recovery retains the fresh actual signer but starts no provider.
    pub(super) fn recovery(
        signin: super::accounts::SigninTurn,
        store: &super::accounts::Store,
        label: &str,
    ) -> Result<Self, String> {
        signin
            .check_store(store)
            .map_err(|error| error.to_string())?;
        Ok(Self {
            _running: None,
            signin: SigninOwner::Global(signin),
            cli: store.cli(),
            label: label.to_owned(),
            selection_owner: None,
            spacing: None,
        })
    }
    pub(super) fn check_store(&self, store: &super::accounts::Store) -> Result<(), String> {
        match &self.signin {
            SigninOwner::Global(turn) => turn.check_store(store).map_err(|error| error.to_string()),
            SigninOwner::NativeRecovery(turn) => turn.check_store(store),
            SigninOwner::Native(turn) => {
                turn.check_preparation(store)?;
                turn.check_publication()
            }
        }
    }
    pub(super) fn check_preparation(&self, store: &super::accounts::Store) -> Result<(), String> {
        match &self.signin {
            SigninOwner::Global(turn) => turn.check_store(store).map_err(|error| error.to_string()),
            SigninOwner::Native(turn) => turn.check_preparation(store),
            SigninOwner::NativeRecovery(_) => {
                Err("publication-only recovery cannot prepare a sign-in".into())
            }
        }
    }
    pub(super) fn native_scope(
        &self,
    ) -> Option<&std::sync::Arc<super::native_enrollment_admission::NativeBrowserTurn>> {
        match &self.signin {
            SigninOwner::Global(_) | SigninOwner::NativeRecovery(_) => None,
            SigninOwner::Native(turn) => Some(turn),
        }
    }
    pub(super) fn selection(&self) -> Option<&super::accounts::PendingSelection> {
        self.selection_owner
            .as_ref()
            .map(super::accounts_sync::ManualSelectionOwner::selection)
    }
    /// The sign-in lock it holds.
    #[must_use]
    pub(super) fn lock(&self) -> Option<&super::accounts::Lock> {
        match &self.signin {
            SigninOwner::Global(turn) => Some(turn),
            SigninOwner::Native(_) | SigninOwner::NativeRecovery(_) => None,
        }
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
    signin: super::accounts::SigninTurn,
    cli: super::accounts::Cli,
    label: &str,
) -> Begun {
    Begun {
        _running: None,
        signin: SigninOwner::Global(signin),
        cli,
        label: label.to_owned(),
        selection_owner: None,
        spacing: None,
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
#[path = "sign_in_ladder_registry_tests.rs"]
mod registry_tests;
