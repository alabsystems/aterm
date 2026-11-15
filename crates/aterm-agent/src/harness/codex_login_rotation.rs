// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whether a native Codex session whose login was lost moves to another row that is already
//! signed in here, and to which (`docs/DESIGN-ai-accounts-multi-cli-2026-10-23.md`, "A Codex
//! session's move at a lost login").
//!
//! The one loss that moves a session is a settled refusal of the token its login file holds now,
//! for a reason that is that token's ordinary end
//! ([`codex_app_login::RefusalReason::ends_ordinarily`]: `refresh_token_expired` alone), with
//! the source signed out on record. Every other loss stays a stop for a person: another reason, a
//! refusal on record for no reason or none at all, a login that still serves usage, a login in
//! aterm's own custody. The decision reads the records first and the provider only where they
//! cannot say, at most once per kind of read for one loss of one token ([`ReadLedger`]): one
//! presentation of an unpresented token, one usage-only read confirming the sign-out once its
//! refusal settled, and one pass over the candidates. It signs nobody in, copies no token and
//! reads nothing on a timer; a move only names the destination row's own login file. Nothing
//! here selects for the pool: its `active` row, its rotations, its canary and its breaker stay
//! as they are, and the receipt's session-scoped writer ([`super::codex_rotation`]) judges the
//! selection again before it is published and committed.

use super::accounts::{self, Cli, Mode, Point, Row, Supervisor};
use super::accounts_pool::{self, FreshAnswer, FreshRead, Seat, Session};
use super::codex_app_login::{self, IdentifiedRead, RefusalReason, RefusalStanding};
use super::codex_renewal::{Identity, Successor};
use super::codex_rotation::{self, RECORD_WITHIN, UsageReader};
use super::provider_read::{self, Account, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(super) struct LaunchUse(());

pub(super) struct WallUse(Account);

impl provider_read::WallDoor for WallUse {
    fn account(self) -> Account {
        self.0
    }
}

/// A move [`prepare`] selected: the session leaves `source`'s lost login for `destination`'s
/// own login file. Only paths, labels and identity digests; no credential bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginMove {
    /// The session's row, whose login was lost.
    pub source: Row,
    /// The row it moves to, already signed in here.
    pub destination: Row,
    /// The source login's identity, as the session's request named it.
    pub source_identity: Identity,
    /// The destination login's identity, as the request that read it carried.
    pub destination_identity: Identity,
    /// The session's own login file.
    pub old_auth: PathBuf,
    /// The destination row's login file.
    pub new_auth: PathBuf,
    /// The destination's sign-in generation, which its writer holds the receipt to.
    pub generation: u64,
    /// Why the source's token was refused: an ordinary end.
    pub reason: RefusalReason,
}

impl LoginMove {
    /// The successor its receipt names and its writer compares.
    #[must_use]
    pub fn successor(&self) -> Successor {
        Successor {
            row: self.destination.clone(),
            old_auth: self.old_auth.clone(),
            new_auth: self.new_auth.clone(),
            generation: self.generation,
            identity: self.destination_identity.clone(),
        }
    }
}

/// Why no move is coming to a session whose login was lost: it stays a stop for a person, on
/// today's path (a renewal where sign-ins renew, else the sign-in attention).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LostLoginEnd {
    /// Accounts are off, the session's mode is not the pool's, or the default login is in the
    /// keyring.
    Off,
    /// The session's login is no row the pool manages.
    NoRow,
    /// Its row's login names another account than the row's.
    NamesAnother,
    /// Its row is held for another Mac.
    Held,
    /// Another row is pinned: every session stays where it is.
    PinnedElsewhere,
    /// Its own row is pinned though signed out: a pin set after the sign-out (which ended the
    /// one before it) is a person's new decision.
    Pinned,
    /// Its row's login was renewed, so the row's login is no longer the session's: that move is
    /// the renewal's own.
    RowRenewed,
    /// Nothing on record says why its token stopped: no journal answer, an answer lost, a login
    /// in aterm's custody, no refresh token.
    Unrecorded,
    /// The refusal on record is settled but no ordinary end: the provider's own action, or a
    /// spend nothing here names.
    Refused(RefusalReason),
    /// Its token was already presented for this loss, or its reads are paced.
    Paced,
    /// Its login still serves usage: a plain refusal of a request is no lost login.
    LoginWorks,
    /// No read confirmed that it is signed out.
    Unconfirmed,
    /// No other row signed in here can take it.
    NoTarget,
    /// This loss's one pass over the candidates was spent.
    Spent,
}

impl std::fmt::Display for LostLoginEnd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let words = match self {
            Self::Off => "accounts do not move this session",
            Self::NoRow => "its login is no row the pool manages",
            Self::NamesAnother => "its row's login names another account",
            Self::Held => "its row is held for another Mac",
            Self::PinnedElsewhere => "another row is pinned",
            Self::Pinned => "its row was pinned after its sign-out",
            Self::RowRenewed => "its row's login was renewed",
            Self::Unrecorded => "nothing records why its login was lost",
            Self::Refused(reason) => {
                return write!(f, "its token was refused for good ({})", reason.code());
            }
            Self::Paced => "its login was already read for this loss",
            Self::LoginWorks => "its login still serves usage",
            Self::Unconfirmed => "no read confirmed its sign-out",
            Self::NoTarget => "no other signed-in row can take it",
            Self::Spent => "this loss's candidates were already read",
        };
        f.write_str(words)
    }
}

/// What [`prepare`] decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoginDecision {
    /// Move the session as this says.
    Move(Box<LoginMove>),
    /// No move is coming, for this reason.
    NotNeeded(LostLoginEnd),
    /// Ask again later: at `until` (Unix seconds), when a refusal on record settles, or after
    /// the control's own retry where it names none (a renewal of the row under way).
    Wait {
        /// When the records can next decide it.
        until: Option<i64>,
    },
}

/// The provider reads one loss of one token still admits, kept by the caller across its asks
/// for one process, thread and wall turn: one presentation of a token aterm never presented,
/// one usage-only read confirming a settled refusal, and one pass over the candidates. A new
/// token in the login file (the login replaced) is a new loss, and admits each again.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReadLedger {
    /// The digest of the token the reads are for ([`RefusalStanding`]).
    pub(super) digest: Option<String>,
    /// A presentation of the token is still admitted.
    pub(super) may_present: bool,
    /// A read confirming the sign-out is still admitted.
    pub(super) may_confirm: bool,
    /// A pass over the candidates is still admitted.
    pub(super) may_read_candidates: bool,
}

impl ReadLedger {
    /// Admits every read again where `digest` names another token than the one spent for.
    fn for_token(&mut self, digest: &str) {
        if self.digest.as_deref() != Some(digest) {
            *self = Self {
                digest: Some(digest.to_owned()),
                may_present: true,
                may_confirm: true,
                may_read_candidates: true,
            };
        }
    }
}

/// Decide whether the native Codex session `session`, stopped at a lost login
/// ([`Point::LoginEnded`]) whose source the session's request names `expected_source`, moves to
/// another row already signed in here, reading the provider only as `ledger` still admits and
/// within `wait`. Every answer is derived again from the records after each read; a sign-out
/// just read never moves a session on its own, only beside a settled ordinary end of the very
/// token its login file holds.
///
/// # Errors
/// The session is not a native Codex session at a lost login, the pool cannot be read, the
/// deadline was spent before a read, or a read named another account than the source.
pub fn prepare<R>(
    seat: &Seat<'_>,
    session: &Session<'_, R>,
    expected_source: &Identity,
    reader: &dyn UsageReader,
    wait: Duration,
    ledger: &mut ReadLedger,
) -> Result<LoginDecision, &'static str> {
    use LoginDecision::{NotNeeded, Wait};
    if session.cli != Cli::Codex || session.supervisor != Supervisor::Own {
        return Err("not-native-codex");
    }
    if session.point != Point::LoginEnded {
        return Err("not-a-lost-login");
    }
    let mode = seat.mode_for(Cli::Codex);
    if mode == Mode::Off || session.mode != mode || seat.keyring_held(Cli::Codex) {
        return Ok(NotNeeded(LostLoginEnd::Off));
    }
    let deadline = Instant::now().checked_add(wait).ok_or("deadline")?;
    let remaining = || deadline.saturating_duration_since(Instant::now());
    let store = seat.store(Cli::Codex);
    let roster = seat.bound_roster(Cli::Codex).map_err(|_| "roster")?;
    let who = accounts::ProcessLogin::of_env_for(Cli::Codex, session.env.iter().copied());
    let me = who.me(&roster);
    let Some(source) = me.label().and_then(|label| roster.row(label)).cloned() else {
        return Ok(NotNeeded(LostLoginEnd::NoRow));
    };
    // Codex never produces a token route: one reads as outside its stored login.
    if !me.default_home || !me.route.is_stored() {
        return Ok(NotNeeded(LostLoginEnd::NoRow));
    }
    if accounts_pool::check_login_identity(Cli::Codex, &source, seat.home).is_err() {
        return Ok(NotNeeded(LostLoginEnd::NamesAnother));
    }
    if accounts_pool::held_source(&source) {
        return Ok(NotNeeded(LostLoginEnd::Held));
    }
    let judged = || {
        store
            .load_judged(seat.now, &roster)
            .map(|(state, _)| state)
            .map_err(|_| "state")
    };
    let mut state = judged()?;
    if let Some(end) = pinned(&state, &source, seat.now) {
        return Ok(NotNeeded(end));
    }
    let old_auth = who
        .login
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| seat.home.join(Cli::Codex.accounts().default_home))
        .join("auth.json");
    // A login a renewal replaced is no longer the row's: that move is the renewal's.
    if me.account.is_none() || codex_rotation::row_auth(seat, &source) != old_auth {
        return Ok(NotNeeded(LostLoginEnd::RowRenewed));
    }
    // A renewal of the row under way. The activity's lock is waited for: another thread's
    // instant on it is no renewal, and answers nothing of this row.
    if super::accounts_sync::login_activity_waited(&store, Cli::Codex, seat.now)
        .iter()
        .any(|row| row.label == source.label && row.active)
    {
        return Ok(Wait { until: None });
    }
    let default_login = seat.home.join(Cli::Codex.accounts().default_home);
    // No custody record is given: a login in aterm's own custody reads Unknown and stays a
    // stop, though its row now records its settled refusal's reason.
    let standing = || codex_app_login::row_refusal(&source, None, Some(&default_login), seat.now);
    let read_own = |left: Duration| {
        let use_token = provider_read::Use::wall_read_use(WallUse(Account::in_home(
            Read::CodexEndpoint,
            &old_auth,
            Some(&source.label),
        )));
        reader.read_on(
            seat,
            &old_auth,
            source.expected_identity_email(),
            seat.now,
            left,
            use_token,
        )
    };
    let note_own = |context, answer| {
        let read = FreshRead {
            label: source.label.clone(),
            context,
            answer,
        };
        let _ = accounts_pool::note_fresh_reads(seat, Cli::Codex, &[read], remaining());
    };
    let budget = || {
        let left = remaining().saturating_sub(RECORD_WITHIN);
        (!left.is_zero()).then_some(left).ok_or("deadline")
    };
    let reason = match standing() {
        RefusalStanding::Unknown => return Ok(NotNeeded(LostLoginEnd::Unrecorded)),
        RefusalStanding::Settled { reason, .. } if !reason.ends_ordinarily() => {
            return Ok(NotNeeded(LostLoginEnd::Refused(reason)));
        }
        RefusalStanding::Pending { settles_at, .. } => {
            return Ok(Wait {
                until: Some(settles_at),
            });
        }
        RefusalStanding::Unpresented { digest } => {
            ledger.for_token(&digest);
            if !ledger.may_present || !accounts_pool::handoff_reads(&source, &state, seat.now) {
                return Ok(NotNeeded(LostLoginEnd::Paced));
            }
            let left = budget()?;
            let context = store.capture_auth_context(&source);
            ledger.may_present = false;
            let answer = read_own(left);
            return Ok(match answer {
                IdentifiedRead::Usage(observed) => {
                    if observed.identity != *expected_source {
                        return Err("source-identity-changed");
                    }
                    note_own(context, FreshAnswer::Usage(observed.usage));
                    NotNeeded(LostLoginEnd::LoginWorks)
                }
                // aterm's own refusal of the token just now is paced by its journal: it settles
                // once another writer's rotation of the same token could no longer be on its way.
                answer => match standing() {
                    RefusalStanding::Pending {
                        digest: refused,
                        settles_at,
                        ..
                    } if refused == digest => Wait {
                        until: Some(settles_at),
                    },
                    _ => {
                        note_own(context, fresh_answer(answer));
                        NotNeeded(LostLoginEnd::Unconfirmed)
                    }
                },
            });
        }
        RefusalStanding::Settled { reason, digest } => {
            ledger.for_token(&digest);
            if accounts::condition(&source, &state, true, seat.now)
                != accounts::Condition::SignedOut
            {
                if !ledger.may_confirm || !accounts_pool::handoff_reads(&source, &state, seat.now) {
                    return Ok(NotNeeded(LostLoginEnd::Unconfirmed));
                }
                let left = budget()?;
                let context = store.capture_auth_context(&source);
                ledger.may_confirm = false;
                match read_own(left) {
                    IdentifiedRead::SignedOut => {
                        // Staging the sign-out ends a pin on the row, as any sign-out does.
                        note_own(context, FreshAnswer::SignedOut);
                    }
                    IdentifiedRead::Usage(observed) => {
                        if observed.identity != *expected_source {
                            return Err("source-identity-changed");
                        }
                        note_own(context, FreshAnswer::Usage(observed.usage));
                        return Ok(NotNeeded(LostLoginEnd::LoginWorks));
                    }
                    IdentifiedRead::Unread => {
                        note_own(context, FreshAnswer::Unanswered);
                        return Ok(NotNeeded(LostLoginEnd::Unconfirmed));
                    }
                }
                state = judged()?;
                let still = matches!(standing(),
                    RefusalStanding::Settled { reason: now, digest: held }
                        if now.ends_ordinarily() && held == digest);
                if !still
                    || accounts::condition(&source, &state, true, seat.now)
                        != accounts::Condition::SignedOut
                {
                    return Ok(NotNeeded(LostLoginEnd::Unconfirmed));
                }
                if let Some(end) = pinned(&state, &source, seat.now) {
                    return Ok(NotNeeded(end));
                }
            }
            reason
        }
    };
    select(
        seat,
        (&store, &roster, &state),
        (&source, expected_source, reason),
        (&old_auth, reader, &remaining),
        ledger,
    )
}

/// What a pin on record says of a move off `source`: none under another row's pin, nor under
/// one on `source` set after its sign-out (staging that sign-out ended any pin before it).
fn pinned(state: &accounts::State, source: &Row, now: i64) -> Option<LostLoginEnd> {
    if !state.pinned {
        return None;
    }
    if state.active.as_deref() != Some(source.label.as_str()) {
        return Some(LostLoginEnd::PinnedElsewhere);
    }
    (accounts::condition(source, state, true, now) == accounts::Condition::SignedOut)
        .then_some(LostLoginEnd::Pinned)
}

/// A read's answer as the records keep it.
fn fresh_answer(answer: IdentifiedRead) -> FreshAnswer {
    match answer {
        IdentifiedRead::Usage(usage) => FreshAnswer::Usage(usage.usage),
        IdentifiedRead::SignedOut => FreshAnswer::SignedOut,
        IdentifiedRead::Unread => FreshAnswer::Unanswered,
    }
}

/// Steps 9 and 10: the rows the records rank for a session at a lost login
/// ([`accounts::candidates`] at [`Point::LoginEnded`], which holds each to what this Mac may
/// use), less any the handoff's writer would refuse (a row without room left) and any whose
/// earlier successor never attached for this source, read once each in rank order under its
/// own login file; the first that reads usable, as another account than the source, is the
/// move.
fn select(
    seat: &Seat<'_>,
    (store, roster, state): (&accounts::Store, &accounts::Roster, &accounts::State),
    (source, expected_source, reason): (&Row, &Identity, RefusalReason),
    (old_auth, reader, remaining): (&Path, &dyn UsageReader, &dyn Fn() -> Duration),
    ledger: &mut ReadLedger,
) -> Result<LoginDecision, &'static str> {
    use LoginDecision::{Move, NotNeeded};
    let (logins, _) = store.load_logins(seat.now).map_err(|_| "bindings")?;
    let generation = |row: &Row| {
        logins
            .get(&row.label)
            .map_or(0, |binding| binding.renewal_generation)
    };
    let enrolled = accounts::enrolled_in(Cli::Codex, roster, seat.home);
    let ranked = accounts::candidates(
        roster,
        state,
        &enrolled,
        Some(&source.label),
        &Point::LoginEnded,
        seat.now,
    );
    let mut open = Vec::new();
    for (label, verdict) in ranked {
        let Some(row) = roster.row(&label).filter(|_| verdict.is_ok()) else {
            continue;
        };
        let new_auth = codex_rotation::row_auth(seat, row);
        let held = super::codex_renewal::unattached_held_until(
            store,
            expected_source,
            &new_auth,
            generation(row),
            seat.now,
        );
        if row.label != source.label
            && accounts_pool::native_target_admitted(row, state, seat.now)
            && matches!(held, Ok(None))
        {
            open.push((row, new_auth));
        }
    }
    if open.is_empty() {
        return Ok(NotNeeded(LostLoginEnd::NoTarget));
    }
    if !ledger.may_read_candidates {
        return Ok(NotNeeded(LostLoginEnd::Spent));
    }
    ledger.may_read_candidates = false;
    let mut launch = provider_read::Launch::launch_use(LaunchUse(()));
    let mut reads = Vec::new();
    let mut selected = None;
    for (row, new_auth) in open {
        if accounts_pool::check_login_identity(Cli::Codex, row, seat.home).is_err()
            || !accounts_pool::handoff_reads(row, state, seat.now)
        {
            continue;
        }
        let left = remaining().saturating_sub(RECORD_WITHIN);
        if left.is_zero() {
            break;
        }
        let context = store.capture_auth_context(row);
        let Some(use_token) = provider_read::Use::launch_read_use(
            &mut launch,
            Account::in_home(Read::CodexEndpoint, &new_auth, Some(&row.label)),
        ) else {
            break;
        };
        let answer = reader.read_on(
            seat,
            &new_auth,
            row.expected_identity_email(),
            seat.now,
            left,
            use_token,
        );
        let IdentifiedRead::Usage(usage) = answer else {
            reads.push(FreshRead {
                label: row.label.clone(),
                context,
                answer: fresh_answer(answer),
            });
            continue;
        };
        reads.push(FreshRead {
            label: row.label.clone(),
            context,
            answer: FreshAnswer::Usage(usage.usage.clone()),
        });
        if codex_rotation::usable(&usage, seat.now)
            && usage.identity.account != expected_source.account
        {
            selected = Some((row.clone(), usage.identity, new_auth));
            break;
        }
    }
    // The report and later writers judge these rows by what was just read. A selection stands
    // whether or not this records: its writer judges the row again.
    let _ = accounts_pool::note_fresh_reads(seat, Cli::Codex, &reads, remaining());
    let Some((destination, destination_identity, new_auth)) = selected else {
        return Ok(NotNeeded(LostLoginEnd::NoTarget));
    };
    let generation = generation(&destination);
    Ok(Move(Box::new(LoginMove {
        source: source.clone(),
        destination,
        source_identity: expected_source.clone(),
        destination_identity,
        old_auth: old_auth.to_owned(),
        new_auth,
        generation,
        reason,
    })))
}
