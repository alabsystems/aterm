// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Why the issuer refused a Codex login's refresh token for good, kept beside the login in its
//! refresh journal ([`RefreshJournal::Rejected`]) as one closed word and shown beside a row that
//! is signed out. A login in aterm's own custody has no such journal: its settled refusal's
//! word is kept on its row in the accounts store
//! ([`crate::harness::accounts::Account::auth_refusal`]). Recording the reason decides nothing:
//! whether a login is signed out, and what a signed-out row may do, stay exactly as they were.
//!
//! One reader answers where the token a login file holds now stands ([`refusal_standing`]), for
//! a row's login too ([`row_refusal`], which also reads a custody login's row record); a
//! report's reason ([`settled_refusal`], [`row_signed_out_reason`]) is its settled answer. One
//! predicate says which settled reason is a token's ordinary end
//! ([`RefusalReason::ends_ordinarily`]).

use super::*;

/// Why a token endpoint refused a refresh token for good ([`refresh_refused`]), as its answer's
/// code named it: one word of a closed set, never the answer's text. Only the first is a token's
/// ordinary end ([`RefusalReason::ends_ordinarily`]); every other reason may be the provider's
/// own action on the account, or another program's spend of the token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalReason {
    /// `refresh_token_expired`: the token outlived its lifetime.
    Expired,
    /// `refresh_token_reused`: the single-use token was spent before this request. Nothing here
    /// says who spent it: a rotation that reached the login file replaces the token, so a settled
    /// refusal of the file's token ([`settled_refusal`]: [`ROTATION_HANG`] has passed since) is
    /// one after which no working login reached it. Before then another writer's rotation may
    /// still be on its way to the file. Settled, it is no ordinary end
    /// ([`RefusalReason::ends_ordinarily`]).
    Reused,
    /// `refresh_token_invalidated`: the issuer revoked the token.
    Invalidated,
    /// `invalid_grant`, OAuth's word for a grant the issuer will not honour, with no more said.
    InvalidGrant,
    /// Any other answer the refresh read as a refusal for good (a bare `401` among them).
    Other(OtherRefusal),
}

/// A refusal's code outside the token's own words: one of OAuth's token-endpoint error codes
/// (RFC 6749 §5.2) or a bearer token's (RFC 6750 §3.1), else [`OtherRefusal::Unknown`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtherRefusal {
    /// `invalid_request`.
    InvalidRequest,
    /// `invalid_client`.
    InvalidClient,
    /// `unauthorized_client`.
    UnauthorizedClient,
    /// `unsupported_grant_type`.
    UnsupportedGrantType,
    /// `invalid_scope`.
    InvalidScope,
    /// `invalid_token`.
    InvalidToken,
    /// No code, a code outside this closed set, or a refusal recorded before reasons were kept.
    Unknown,
}

impl RefusalReason {
    /// The reason nothing names: no code, an unlisted code, or an old record without one.
    pub const UNKNOWN: Self = Self::Other(OtherRefusal::Unknown);

    /// The reason a refused answer's code names (`error`, else `error.code`, else `code`, read as
    /// [`refresh_refusal_code`] reads it for the refusal itself); none of the answer is kept.
    pub(super) fn of_answer(body: &[u8]) -> Self {
        refresh_refusal_code(body).map_or(Self::UNKNOWN, |code| Self::of_code(&code))
    }

    /// The reason the lowercase code word `code` names; any word outside the closed set is
    /// [`RefusalReason::UNKNOWN`].
    #[must_use]
    pub fn of_code(code: &str) -> Self {
        match code {
            "refresh_token_expired" => Self::Expired,
            "refresh_token_reused" => Self::Reused,
            "refresh_token_invalidated" => Self::Invalidated,
            "invalid_grant" => Self::InvalidGrant,
            "invalid_request" => Self::Other(OtherRefusal::InvalidRequest),
            "invalid_client" => Self::Other(OtherRefusal::InvalidClient),
            "unauthorized_client" => Self::Other(OtherRefusal::UnauthorizedClient),
            "unsupported_grant_type" => Self::Other(OtherRefusal::UnsupportedGrantType),
            "invalid_scope" => Self::Other(OtherRefusal::InvalidScope),
            "invalid_token" => Self::Other(OtherRefusal::InvalidToken),
            _ => Self::UNKNOWN,
        }
    }

    /// The closed word kept for it in the journal and in reports' JSON: the issuer's own code,
    /// or `unknown`. [`RefusalReason::of_code`] reads it back.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Expired => "refresh_token_expired",
            Self::Reused => "refresh_token_reused",
            Self::Invalidated => "refresh_token_invalidated",
            Self::InvalidGrant => "invalid_grant",
            Self::Other(OtherRefusal::InvalidRequest) => "invalid_request",
            Self::Other(OtherRefusal::InvalidClient) => "invalid_client",
            Self::Other(OtherRefusal::UnauthorizedClient) => "unauthorized_client",
            Self::Other(OtherRefusal::UnsupportedGrantType) => "unsupported_grant_type",
            Self::Other(OtherRefusal::InvalidScope) => "invalid_scope",
            Self::Other(OtherRefusal::InvalidToken) => "invalid_token",
            Self::Other(OtherRefusal::Unknown) => "unknown",
        }
    }

    /// Its plain words, as a report writes them after `signed-out` (`signed-out (token expired)`).
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            Self::Expired => "token expired",
            Self::Reused => "token reused",
            Self::Invalidated => "token invalidated",
            Self::InvalidGrant => "invalid grant",
            Self::Other(OtherRefusal::InvalidRequest) => "invalid request",
            Self::Other(OtherRefusal::InvalidClient) => "invalid client",
            Self::Other(OtherRefusal::UnauthorizedClient) => "unauthorized client",
            Self::Other(OtherRefusal::UnsupportedGrantType) => "unsupported grant type",
            Self::Other(OtherRefusal::InvalidScope) => "invalid scope",
            Self::Other(OtherRefusal::InvalidToken) => "invalid token",
            Self::Other(OtherRefusal::Unknown) => "reason unknown",
        }
    }

    /// Whether a settled refusal for this reason ([`RefusalStanding::Settled`]) is the token's
    /// ordinary end, the one loss of a login that is no one's decision: only
    /// [`RefusalReason::Expired`], the token outliving its lifetime. It is the only predicate a
    /// move off a lost login reads; every other reason stays a stop for a person.
    ///
    /// [`RefusalReason::Reused`] is no ordinary end, settled or not. A race between cooperating
    /// writers of one login never settles as a lost login: the winner's rotation reaches the
    /// login file, the file then holds another token, and the refusal of the spent one answers
    /// nothing ([`refusal_standing`] reads the token the file holds now). aterm's own journal
    /// fences its own earlier spend. So a settled `reused` is a spend no record here names (a
    /// program that takes no lock, a copy of the login elsewhere, a third party), and stays a
    /// stop until the issuer's refusal names who spent it. [`RefusalReason::Invalidated`],
    /// [`RefusalReason::InvalidGrant`] and every [`RefusalReason::Other`],
    /// [`OtherRefusal::Unknown`] among them (how a deactivation, a ban or a bare `401` reads),
    /// may be the provider's own action on the account.
    #[must_use]
    pub const fn ends_ordinarily(self) -> bool {
        matches!(self, Self::Expired)
    }
}

/// Where the refresh token a Codex login file holds now stands with its issuer, as aterm's
/// journal beside that login records it ([`refusal_standing`]) or, for a login in aterm's own
/// custody, as its row in the accounts store does ([`row_refusal`]). Each `digest` is the
/// journal's digest of that token (`refresh_token_digest`, domain-separated SHA-256 in lowercase
/// hex; a custody grant's spent-token digest is the same one), never the token: what a later
/// read compares to know the file still holds the same token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefusalStanding {
    /// The issuer refused that very token for `reason`, and the refusal is settled
    /// ([`ROTATION_HANG`] has passed since, or it was recorded before refusals were dated): no
    /// other writer's rotation of the refused token can still be on its way to the file.
    Settled {
        /// Why, as the journal keeps it, or the row of a login in aterm's own custody.
        reason: RefusalReason,
        /// The refused token's digest, the one the file holds now.
        digest: String,
    },
    /// The issuer refused that very token for `reason`, but less than [`ROTATION_HANG`] ago:
    /// another writer of the same login may have spent it and be publishing its successor.
    Pending {
        /// Why, as the journal keeps it.
        reason: RefusalReason,
        /// The refused token's digest, the one the file holds now.
        digest: String,
        /// When it is settled (Unix seconds): the refusal's time and [`ROTATION_HANG`].
        settles_at: i64,
    },
    /// aterm never presented that token: no journal stands beside the login, or the one there
    /// is of an earlier token (the login was signed in again since).
    Unpresented {
        /// The token's digest.
        digest: String,
    },
    /// Nothing can be said of it: the login is in aterm's own custody (its markers beside the
    /// file, whose legacy journal is not its record) and its row holds no settled refusal of the
    /// login as it is now, aterm presented it and holds no answer or holds an answer not yet
    /// applied (an attempt, a response, an enrollment's response), the file names no refresh
    /// token or another account than the one expected, or the file or its journal cannot be
    /// read.
    Unknown,
}

impl RefusalStanding {
    /// The reason of a [`RefusalStanding::Settled`] refusal; `None` for every other standing.
    #[must_use]
    pub const fn settled(&self) -> Option<RefusalReason> {
        match self {
            Self::Settled { reason, .. } => Some(*reason),
            Self::Pending { .. } | Self::Unpresented { .. } | Self::Unknown => None,
        }
    }
}

/// Where, at `now`, the refresh token `home`'s login file holds now stands, where that login is
/// the account `expected_email` names ([`is_account`]) and its journal is aterm's legacy record
/// of it ([`legacy_reader_allowed`]). Settled or pending ([`settled`]) where the journal beside
/// it refused this very token ([`RefreshJournal::Rejected`]); unpresented where no journal state
/// names this token (none stands there, or one of an earlier token: its next read discards it,
/// [`recover_journal_held`]); unknown otherwise ([`RefusalStanding::Unknown`]). The record is of
/// the login file in `home`, whoever signed in there, so a login of another account in a row's
/// place (the vendor's own sign-in on the default login,
/// [`crate::harness::accounts::note_displaced`]) is unknown for that row. Read-only: no lock, no
/// write and no request.
#[must_use]
pub fn refusal_standing(home: &Path, expected_email: Option<&str>, now: i64) -> RefusalStanding {
    if !legacy_reader_allowed(home) {
        return RefusalStanding::Unknown;
    }
    let Some(stored) = load_stored_auth(home) else {
        return RefusalStanding::Unknown;
    };
    let Some(digest) = stored.refresh_token.as_deref().map(refresh_token_digest) else {
        return RefusalStanding::Unknown;
    };
    if !is_account(&stored, expected_email) {
        return RefusalStanding::Unknown;
    }
    match RefreshJournal::read(home) {
        Ok(None) => RefusalStanding::Unpresented { digest },
        Ok(Some(RefreshJournal::Rejected { token, at, reason })) if token == digest => match at {
            Some(at) if !settled(Some(at), now) => RefusalStanding::Pending {
                reason,
                digest,
                settles_at: at.saturating_add(hang_secs()),
            },
            // Settled, or refused before refusals were dated: long settled.
            Some(_) | None => RefusalStanding::Settled { reason, digest },
        },
        // A refusal of, or an attempt with, an earlier token: the login was signed in again.
        Ok(Some(RefreshJournal::Rejected { token, .. } | RefreshJournal::Attempted { token }))
            if token != digest =>
        {
            RefusalStanding::Unpresented { digest }
        }
        // A response to an earlier token. One from before attempts were recorded names none, so
        // it is not shown to be of an earlier one.
        Ok(Some(RefreshJournal::Response {
            token: Some(token), ..
        })) if token != digest => RefusalStanding::Unpresented { digest },
        Ok(Some(
            RefreshJournal::Attempted { .. }
            | RefreshJournal::Response { .. }
            | RefreshJournal::EnrollmentResponse { .. }
            | RefreshJournal::Rejected { .. },
        ))
        | Err(_) => RefusalStanding::Unknown,
    }
}

/// The settled refusal, at `now`, of the refresh token `home`'s login holds now, where that login
/// is the account `expected_email` names: [`refusal_standing`]'s settled reason
/// ([`RefusalStanding::settled`]). A row's reason is this answer for the row's own expected
/// address, so a login of another account in the row's place answers nothing for the row.
///
/// `None` where nothing refused it: no journal or another state of one, a refusal of an earlier
/// token (the login signed in again since), a login file naming no refresh token, a login of
/// another account, or a login aterm keeps in its own custody (its markers beside the file).
/// `None` too for a refusal not yet settled, which may still be no refusal of this login at all:
/// another writer of the same login may have spent the single-use token and be publishing its
/// successor ([`refusal_settled`] holds a read of the login to the same rule). Read-only: no
/// lock, no write and no request.
#[must_use]
pub fn settled_refusal(
    home: &Path,
    expected_email: Option<&str>,
    now: i64,
) -> Option<RefusalReason> {
    refusal_standing(home, expected_email, now).settled()
}

/// What the accounts store holds of a row whose login may be in aterm's own custody, for
/// [`row_refusal`] to read that login's settled refusal from
/// ([`crate::harness::accounts::Store::custody_record`]).
#[derive(Debug, Clone, Copy)]
pub struct CustodyRecord<'a> {
    /// The row's record in the store's `state.json`.
    pub account: &'a crate::harness::accounts::Account,
    /// The user home whose Codex configuration routes the login's reads.
    pub user_home: &'a Path,
}

/// Where the refresh token of `row`'s Codex login stands, for the address the row expects
/// ([`crate::harness::accounts::Row::expected_identity_email`]). Its login file is in its own
/// directory, or, for a row with none (the default login), in `default_login`: the directory the
/// accounts store reads that login's lapse in
/// ([`crate::harness::accounts::Store::default_login_home`]) or the place an observer read.
/// [`RefusalStanding::Unknown`] where neither names one.
///
/// A login whose journal is aterm's legacy record answers [`refusal_standing`]. A login in
/// aterm's own custody (its markers beside the file) answers from `custody`, the row's record in
/// the accounts store: settled, for the reason native custody recorded with the settled
/// sign-out ([`crate::harness::accounts::Account::auth_refusal`]), where that sign-out is still
/// the word on record for the very credentials the file holds now
/// ([`crate::harness::accounts::Account::settled_refusal_in`]); unknown otherwise, and with no
/// `custody` given. Native custody records a refusal only once it is settled, so a custody login
/// is never pending. It reads no condition: whether the row is signed out is the caller's to
/// read beside it. Read-only: no lock, no write and no request.
#[must_use]
pub fn row_refusal(
    row: &crate::harness::accounts::Row,
    custody: Option<CustodyRecord<'_>>,
    default_login: Option<&Path>,
    now: i64,
) -> RefusalStanding {
    row.dir
        .as_deref()
        .filter(|dir| !dir.is_empty())
        .map(Path::new)
        .or(default_login)
        .map_or(RefusalStanding::Unknown, |dir| {
            if legacy_reader_allowed(dir) {
                refusal_standing(dir, row.expected_identity_email(), now)
            } else {
                custody.map_or(RefusalStanding::Unknown, |record| {
                    custody_standing(dir, row, record)
                })
            }
        })
}

/// One read of the login file in `home`, a login in aterm's own custody, with reads routed as
/// `user_home`'s Codex configuration says: the login it holds, its refresh token's digest, and
/// its credentials and route as one context ([`current_login_context`]'s). The writer of a
/// settled refusal ([`refused_login_context`]) and its reader ([`custody_standing`]) both take
/// the digest and the context from this one read, so neither is ever of another token than the
/// other. `None` for a file that is unavailable or names no refresh token.
fn custody_login(home: &Path, user_home: &Path) -> Option<(StoredAuth, String, CredentialContext)> {
    if !file_context_available(home) {
        return None;
    }
    let stored = read_stored_auth(home)?;
    let digest = stored
        .refresh_token
        .as_deref()
        .filter(|token| !token.is_empty())
        .map(refresh_token_digest)?;
    let credential = CredentialContext::for_login(
        &stored,
        &resolve_usage_url(home, user_home),
        &format!("{DEFAULT_ISSUER}/oauth/token"),
    );
    Some((stored, digest, credential))
}

/// The context native custody records a settled refusal in
/// ([`crate::harness::accounts::Account::auth_refusal`]): [`current_context`] of the login in
/// `home`, only where the refresh token its file holds is the refused one, whose digest is
/// `refused` (the grant's spent-token digest, [`owned_refresh_token_digest`]'s). The token and
/// the context are of one read ([`custody_login`]), so a login another program put in the
/// file's place since the refusal has no context here, and no reason is ever recorded for a
/// token nobody refused. `None` too for a file that is unavailable or names no refresh token.
pub(crate) fn refused_login_context(
    home: &Path,
    user_home: &Path,
    refused: &str,
) -> Option<CredentialContext> {
    custody_login(home, user_home)
        .and_then(|(_, held, credential)| (held == refused).then_some(credential))
}

/// [`row_refusal`] of a login in aterm's own custody, whose file is in `home`: settled where
/// `record`'s account holds native custody's reason for the login this one read of the file
/// finds there (its credentials and route as one context and its refresh token's digest,
/// [`custody_login`]), that login being the account `row` expects ([`is_account`]). The record
/// was made only in the context of the refused token ([`refused_login_context`]), so the digest
/// answered is the refused token's. Unknown for a file that is unavailable, names no refresh
/// token, names different users or names another account, and for a record of another word, row
/// or credentials.
fn custody_standing(
    home: &Path,
    row: &crate::harness::accounts::Row,
    record: CustodyRecord<'_>,
) -> RefusalStanding {
    let Some((stored, digest, credential)) =
        custody_login(home, record.user_home).filter(|(stored, ..)| !conflicting_users(stored))
    else {
        return RefusalStanding::Unknown;
    };
    if !is_account(&stored, row.expected_identity_email()) {
        return RefusalStanding::Unknown;
    }
    record
        .account
        .settled_refusal_in(row, &credential)
        .map_or(RefusalStanding::Unknown, |reason| {
            RefusalStanding::Settled { reason, digest }
        })
}

/// Why `row`'s Codex login is signed out, as a report says it: where `condition` is
/// [`crate::harness::accounts::Condition::SignedOut`] and its login's refusal is settled at
/// `now` ([`row_refusal`], [`RefusalStanding::settled`]); `None` for another condition, for a
/// login of another account in the row's place, or where nothing records why. A report reads no
/// row record, so a login in aterm's own custody says none here. Read-only, and an observation
/// only: a launch, a pick or a move that reads a refusal reads [`row_refusal`] and
/// [`RefusalReason::ends_ordinarily`].
#[must_use]
pub fn row_signed_out_reason(
    row: &crate::harness::accounts::Row,
    condition: crate::harness::accounts::Condition,
    default_login: Option<&Path>,
    now: i64,
) -> Option<RefusalReason> {
    if condition != crate::harness::accounts::Condition::SignedOut {
        return None;
    }
    row_refusal(row, None, default_login, now).settled()
}

/// Whether the login `stored` holds is the account `expected_email` names: where an address is
/// expected, the login names that address and no other ([`check_stored_identity`]); a login
/// that names no address is not shown to be it. Its tokens already name one user
/// ([`load_stored_auth`]). Where no address is expected (a row that names none), the row is the
/// login in its place, whoever that is.
fn is_account(stored: &StoredAuth, expected_email: Option<&str>) -> bool {
    expected_email.is_none_or(|expected| {
        preferred_email(stored)
            .is_some_and(|email| crate::harness::accounts::email_matches(expected, email))
            && check_stored_identity(stored, Some(expected)).is_ok()
    })
}

/// Whether a refusal the issuer made at `at` (Unix seconds) is settled at `now`: [`ROTATION_HANG`]
/// has passed since, so no other writer's rotation of the refused token can still be on its way
/// to the file. A journal written before refusals were dated (`None`) is long settled.
pub(super) fn settled(at: Option<i64>, now: i64) -> bool {
    at.is_none_or(|at| now.saturating_sub(at) >= hang_secs())
}

/// [`ROTATION_HANG`] in whole seconds.
fn hang_secs() -> i64 {
    i64::try_from(ROTATION_HANG.as_secs()).unwrap_or(i64::MAX)
}

/// Records the refresh token `home`'s login holds refused for `reason` at `at`, as a refresh the
/// issuer refused writes it: how a report's or a snapshot's tests lay out a login that lost its
/// refresh token.
/// # Errors
/// The login names no refresh token, or the journal cannot be written.
#[cfg(any(test, debug_assertions))]
pub fn record_refusal_for_test(home: &Path, reason: RefusalReason, at: i64) -> std::io::Result<()> {
    let token = load_stored_auth(home)
        .and_then(|stored| stored.refresh_token)
        .map(|token| refresh_token_digest(&token))
        .ok_or_else(|| std::io::Error::other("the login names no refresh token"))?;
    RefreshJournal::Rejected {
        token,
        at: Some(at),
        reason,
    }
    .write(home)
}
