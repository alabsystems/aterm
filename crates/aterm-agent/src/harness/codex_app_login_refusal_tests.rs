// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The reason a refused refresh keeps beside the login, as one closed word: what each answer
//! shape records, that it survives a reload, how a journal from before reasons reads, that a
//! sign-in clears it, and that nothing of the answer or the token is kept.

use std::collections::BTreeSet;

use super::*;

const NOW: i64 = 1_790_000_000;

/// When a refusal made at [`NOW`] is settled ([`ROTATION_HANG`] later).
fn settled_at() -> i64 {
    NOW + i64::try_from(ROTATION_HANG.as_secs()).expect("seconds")
}

/// Every reason, each once.
const EVERY: [RefusalReason; 11] = [
    RefusalReason::Expired,
    RefusalReason::Reused,
    RefusalReason::Invalidated,
    RefusalReason::InvalidGrant,
    RefusalReason::Other(OtherRefusal::InvalidRequest),
    RefusalReason::Other(OtherRefusal::InvalidClient),
    RefusalReason::Other(OtherRefusal::UnauthorizedClient),
    RefusalReason::Other(OtherRefusal::UnsupportedGrantType),
    RefusalReason::Other(OtherRefusal::InvalidScope),
    RefusalReason::Other(OtherRefusal::InvalidToken),
    RefusalReason::UNKNOWN,
];

/// One refresh of `home`'s login that the token endpoint answers `status` `body`: what it came
/// to, and the journal it left beside the login.
fn refresh_answered(
    home: &Path,
    status: &'static str,
    body: Value,
) -> (Refreshed, Option<RefreshJournal>) {
    let stored = load_stored_auth(home).expect("a login");
    let (port, server) = provider(move |_, target| match target {
        "/oauth/token" => (status, body.clone()),
        _ => ("404 Not Found", json!({})),
    });
    let token_url = format!("http://127.0.0.1:{port}/oauth/token");
    let outcome = refresh_outcome(
        home,
        &stored,
        &token_url,
        aterm_spec::wait::MUST_HAPPEN,
        NOW,
        None,
    );
    assert_eq!(provider_seen(port, server), ["POST /oauth/token"]);
    (
        outcome,
        RefreshJournal::read(home).expect("a readable journal"),
    )
}

/// EACH SHAPE OF REFUSAL KEEPS ITS CLOSED REASON: the code in `error`, in `error.code` or in
/// `code`, trimmed and in any case; OAuth's other words as themselves; an unlisted code, or none,
/// as `unknown`. The refusal itself is what it was: each is `Rejected`. CONTROL: an answer that
/// refuses nothing for good records no refusal.
#[test]
fn each_refusal_shape_keeps_its_closed_reason() {
    for (status, body, want) in [
        (
            "400 Bad Request",
            json!({"error":"refresh_token_expired"}),
            RefusalReason::Expired,
        ),
        (
            "401 Unauthorized",
            json!({"error":{"code":"refresh_token_reused","message":"spent"}}),
            RefusalReason::Reused,
        ),
        (
            "400 Bad Request",
            json!({"code":"refresh_token_invalidated"}),
            RefusalReason::Invalidated,
        ),
        (
            "400 Bad Request",
            json!({"error":"invalid_grant","error_description":"no"}),
            RefusalReason::InvalidGrant,
        ),
        (
            "401 Unauthorized",
            json!({"error":" Refresh_Token_Expired "}),
            RefusalReason::Expired,
        ),
        (
            "401 Unauthorized",
            json!({"error":"invalid_client"}),
            RefusalReason::Other(OtherRefusal::InvalidClient),
        ),
        (
            "401 Unauthorized",
            json!({"error":"account_paused_by_review"}),
            RefusalReason::UNKNOWN,
        ),
        ("401 Unauthorized", json!({}), RefusalReason::UNKNOWN),
    ] {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("login");
        hour_login(&home, NOW, Some("spent"));
        let (outcome, journal) = refresh_answered(&home, status, body.clone());
        assert!(
            matches!(outcome, Refreshed::Rejected),
            "{status} {body}: {outcome:?}"
        );
        assert!(
            matches!(journal, Some(RefreshJournal::Rejected { reason, .. }) if reason == want),
            "{status} {body}: {journal:?}"
        );
        assert_eq!(
            settled_refusal(&home, None, settled_at()),
            Some(want),
            "{status} {body}"
        );
    }
    assert_eq!(
        RefusalReason::of_answer(b"<html>401 Unauthorized</html>"),
        RefusalReason::UNKNOWN,
        "an answer that is no JSON names nothing"
    );

    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("login");
    hour_login(&home, NOW, Some("kept"));
    let (outcome, journal) = refresh_answered(
        &home,
        "500 Internal Server Error",
        json!({"error":"server_error"}),
    );
    assert!(matches!(outcome, Refreshed::Failed), "CONTROL: {outcome:?}");
    assert!(
        matches!(journal, Some(RefreshJournal::Attempted { .. })),
        "CONTROL: {journal:?}"
    );
    assert_eq!(settled_refusal(&home, None, settled_at()), None, "CONTROL");
}

/// A KEPT REASON SURVIVES A RELOAD: each reason written beside a login reads back as itself, its
/// word is its own and no other's, and the record still holds the refused token back.
#[test]
fn every_reason_reads_back_as_itself_from_the_journal() {
    let words: BTreeSet<&str> = EVERY.iter().map(|reason| reason.code()).collect();
    assert_eq!(words.len(), EVERY.len(), "one word each");
    for reason in EVERY {
        assert_eq!(RefusalReason::of_code(reason.code()), reason);
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("login");
        hour_login(&home, NOW, Some("held"));
        record_refusal_for_test(&home, reason, NOW).expect("write");
        assert_eq!(
            RefreshJournal::read(&home).expect("read"),
            Some(RefreshJournal::Rejected {
                token: refresh_token_digest("held"),
                at: Some(NOW),
                reason,
            }),
            "{reason:?}"
        );
        assert_eq!(settled_refusal(&home, None, settled_at()), Some(reason));
        assert!(matches!(
            recover_journal_held(&home),
            Err(Refreshed::Rejected)
        ));
    }
}

/// A JOURNAL FROM BEFORE REASONS WERE KEPT READS `unknown`, and so does one whose word is outside
/// the closed set or no word at all: each is still a refusal of its token, which stays held back,
/// never a journal read as broken.
#[test]
fn a_refusal_recorded_without_a_known_reason_reads_unknown() {
    for reason in [
        None,
        Some(json!("refresh_token_expired, said a person")),
        Some(json!(7)),
    ] {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("login");
        hour_login(&home, NOW, Some("held"));
        let mut doc = json!({
            "aterm_codex_refresh": 2,
            "state": "rejected",
            "refresh_token_digest": refresh_token_digest("held"),
            "rejected_at": NOW,
        });
        if let (Some(reason), Some(doc)) = (reason.clone(), doc.as_object_mut()) {
            doc.insert("reason".into(), reason);
        }
        std::fs::write(
            beside_auth(&home, JOURNAL_SUFFIX),
            aterm_json::to_string(&doc).unwrap(),
        )
        .unwrap();
        assert_eq!(
            RefreshJournal::read(&home).expect("an old journal reads"),
            Some(RefreshJournal::Rejected {
                token: refresh_token_digest("held"),
                at: Some(NOW),
                reason: RefusalReason::UNKNOWN,
            }),
            "{reason:?}"
        );
        assert_eq!(
            settled_refusal(&home, None, settled_at()),
            Some(RefusalReason::UNKNOWN)
        );
        assert!(matches!(
            recover_journal_held(&home),
            Err(Refreshed::Rejected)
        ));
    }
}

/// A REFUSAL IS ANSWERED ONLY ONCE SETTLED: until [`ROTATION_HANG`] has passed since the issuer
/// refused the token, another writer of the same login may still be publishing the successor of
/// the single-use token it spent, so a reuse just refused is not yet this login's refusal. A
/// journal written before refusals were dated is long settled. CONTROL: the same record answers
/// from the moment it is settled.
#[test]
fn a_refusal_is_answered_only_once_settled() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("login");
    hour_login(&home, NOW, Some("spent"));
    record_refusal_for_test(&home, RefusalReason::Reused, NOW).expect("write");
    assert_eq!(settled_refusal(&home, None, NOW), None, "just refused");
    assert_eq!(
        settled_refusal(&home, None, settled_at() - 1),
        None,
        "a second before it is settled"
    );
    assert_eq!(
        settled_refusal(&home, None, settled_at()),
        Some(RefusalReason::Reused),
        "CONTROL: settled"
    );

    std::fs::write(
        beside_auth(&home, JOURNAL_SUFFIX),
        aterm_json::to_string(&json!({
            "aterm_codex_refresh": 2,
            "state": "rejected",
            "refresh_token_digest": refresh_token_digest("spent"),
            "reason": "refresh_token_reused",
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        settled_refusal(&home, None, NOW),
        Some(RefusalReason::Reused),
        "an undated refusal is long settled"
    );
}

/// A REFUSAL ANSWERS ONLY FOR THE ACCOUNT ITS LOGIN IS: the record is of the login file in a
/// place, so a row's reason is answered only where that login names the address the row expects,
/// in any case. Another account's login there (the vendor's own sign-in on the default login),
/// a login that names no address, or one whose tokens name the row's address and another's
/// answers nothing for the row. A row that expects no address is the login in its place.
/// CONTROL: the login's own address answers.
#[test]
fn a_refusal_answers_only_for_the_account_its_login_is() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("login");
    hour_login(&home, NOW, Some("spent"));
    record_refusal_for_test(&home, RefusalReason::Expired, NOW).expect("write");
    let answer = |expected| settled_refusal(&home, expected, settled_at());
    assert_eq!(
        answer(Some("owner@example.test")),
        Some(RefusalReason::Expired),
        "CONTROL: the login's own account"
    );
    assert_eq!(
        answer(Some("Owner@Example.test")),
        Some(RefusalReason::Expired)
    );
    assert_eq!(answer(Some("other@example.test")), None, "another account");
    assert_eq!(
        answer(None),
        Some(RefusalReason::Expired),
        "a row that expects no address"
    );

    for (id, access, why) in [
        (None, None, "a login that names no address"),
        (
            Some("owner@example.test"),
            Some("other@example.test"),
            "a login that names two addresses",
        ),
    ] {
        let id = make_chatgpt_jwt(id, "pro", NOW + 3600);
        let access = make_chatgpt_jwt(access, "pro", NOW + 3600);
        write_auth_doc(
            &home,
            &json!({"auth_mode":"chatgpt", "tokens":{
                "id_token":id, "access_token":access, "refresh_token":"spent",
                "account_id":"acct_native_test"
            }}),
        )
        .expect("login");
        assert_eq!(answer(Some("owner@example.test")), None, "{why}");
        assert_eq!(
            answer(None),
            Some(RefusalReason::Expired),
            "{why}: a row that expects no address"
        );
    }
}

/// A LOGIN IN ATERM'S OWN CUSTODY ANSWERS NO LEGACY REFUSAL: once an owner or Slot marker sits
/// beside the login file, the legacy journal beside it is not that login's record, even where it
/// still names the file's token, so no reason is answered and its standing is unknown, a
/// refusal on record or none. CONTROL: the same journal, with no marker, answers.
#[test]
fn a_login_in_aterms_custody_answers_no_legacy_refusal() {
    for suffix in [OWNER_MARKER_SUFFIX, ".aterm-slot", ".aterm-slot-state"] {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("login");
        hour_login(&home, NOW, Some("spent"));
        record_refusal_for_test(&home, RefusalReason::Expired, NOW).expect("write");
        assert_eq!(
            settled_refusal(&home, None, settled_at()),
            Some(RefusalReason::Expired),
            "CONTROL: {suffix}"
        );
        assert_eq!(
            refusal_standing(&home, None, settled_at()),
            RefusalStanding::Settled {
                reason: RefusalReason::Expired,
                digest: refresh_token_digest("spent"),
            },
            "CONTROL: {suffix}"
        );
        std::fs::write(beside_auth(&home, suffix), OWNER_MARKER_TEXT).expect("marker");
        assert_eq!(settled_refusal(&home, None, settled_at()), None, "{suffix}");
        assert_eq!(
            refusal_standing(&home, None, settled_at()),
            RefusalStanding::Unknown,
            "{suffix}"
        );
        RefreshJournal::discard(&home).expect("discard");
        assert_eq!(
            refusal_standing(&home, None, settled_at()),
            RefusalStanding::Unknown,
            "{suffix}: no journal, still in custody"
        );
    }
}

/// [`settled_refusal`] as it read before [`refusal_standing`] existed, kept here as the oracle
/// its projection must answer exactly as.
fn settled_refusal_before(
    home: &Path,
    expected_email: Option<&str>,
    now: i64,
) -> Option<RefusalReason> {
    if !legacy_reader_allowed(home) {
        return None;
    }
    let Ok(Some(RefreshJournal::Rejected { token, at, reason })) = RefreshJournal::read(home)
    else {
        return None;
    };
    let stored = load_stored_auth(home)?;
    let held = stored.refresh_token.as_deref().map(refresh_token_digest)?;
    let account = expected_email.is_none_or(|expected| {
        preferred_email(&stored)
            .is_some_and(|email| crate::harness::accounts::email_matches(expected, email))
            && check_stored_identity(&stored, Some(expected)).is_ok()
    });
    (held == token && refusal::settled(at, now) && account).then_some(reason)
}

/// A response journal beside `home`'s login to a refresh of the token `token` digests (`None`:
/// one written before attempts were recorded), over the authentication `base` digests.
fn record_response(home: &Path, base: String, token: Option<String>) {
    RefreshJournal::Response {
        base,
        token,
        tokens: RefreshedTokens {
            access_token: "successor-access".into(),
            id_token: None,
            refresh_token: Some("successor-refresh".into()),
            account_id: None,
            last_refresh: "2026-09-21T12:00:00Z".into(),
        },
    }
    .write(home)
    .expect("a response journal");
}

/// The authentication digest of the login `home` holds now, as a response journal binds it.
fn base_of(home: &Path) -> String {
    authentication_digest(&load_stored_auth(home).expect("a login").doc).expect("a digest")
}

/// THE STANDING OF THE TOKEN A LOGIN FILE HOLDS NOW: settled where the journal beside it refused
/// that very token and [`ROTATION_HANG`] has passed (or the refusal is undated), pending before
/// then (settling at the refusal's time and the hang), unpresented where no journal state names
/// that token (none, or a refusal, an attempt or a response of an earlier token), and unknown for
/// everything else: an attempt or a response with that token, a response that names no token,
/// an enrollment's response, an unreadable journal, a login with no refresh token, a login of
/// another account. Each digest is the journal's digest of the file's token. THE REPORTS' READ
/// IS UNCHANGED: across every state, account and moment, [`settled_refusal`] answers exactly as
/// it did before ([`settled_refusal_before`]), and is the standing's settled reason.
#[test]
fn a_login_files_token_stands_settled_pending_unpresented_or_unknown() {
    type Lay = fn(&Path);
    let held = || refresh_token_digest("held");
    let cases: [(&str, Lay, Option<&str>, i64, RefusalStanding); 17] = [
        (
            "settled Rejected(expired)",
            |home| record_refusal_for_test(home, RefusalReason::Expired, NOW).expect("write"),
            None,
            settled_at(),
            RefusalStanding::Settled {
                reason: RefusalReason::Expired,
                digest: held(),
            },
        ),
        (
            "settled Rejected(expired), the row's own account",
            |home| record_refusal_for_test(home, RefusalReason::Expired, NOW).expect("write"),
            Some("Owner@Example.test"),
            settled_at(),
            RefusalStanding::Settled {
                reason: RefusalReason::Expired,
                digest: held(),
            },
        ),
        (
            "Rejected(expired) under a minute old",
            |home| record_refusal_for_test(home, RefusalReason::Expired, NOW).expect("write"),
            None,
            settled_at() - 1,
            RefusalStanding::Pending {
                reason: RefusalReason::Expired,
                digest: held(),
                settles_at: settled_at(),
            },
        ),
        (
            "Rejected(reused) just made",
            |home| record_refusal_for_test(home, RefusalReason::Reused, NOW).expect("write"),
            None,
            NOW,
            RefusalStanding::Pending {
                reason: RefusalReason::Reused,
                digest: held(),
                settles_at: settled_at(),
            },
        ),
        (
            "an undated refusal is long settled",
            |home| {
                RefreshJournal::Rejected {
                    token: refresh_token_digest("held"),
                    at: None,
                    reason: RefusalReason::Invalidated,
                }
                .write(home)
                .expect("write");
            },
            None,
            NOW,
            RefusalStanding::Settled {
                reason: RefusalReason::Invalidated,
                digest: held(),
            },
        ),
        (
            "Rejected of an earlier token",
            |home| {
                RefreshJournal::Rejected {
                    token: refresh_token_digest("earlier"),
                    at: Some(NOW),
                    reason: RefusalReason::Expired,
                }
                .write(home)
                .expect("write");
            },
            None,
            settled_at(),
            RefusalStanding::Unpresented { digest: held() },
        ),
        (
            "no journal",
            |_| {},
            None,
            settled_at(),
            RefusalStanding::Unpresented { digest: held() },
        ),
        (
            "Attempted for an earlier token",
            |home| {
                RefreshJournal::Attempted {
                    token: refresh_token_digest("earlier"),
                }
                .write(home)
                .expect("write");
            },
            None,
            settled_at(),
            RefusalStanding::Unpresented { digest: held() },
        ),
        (
            "Response of an earlier token",
            |home| {
                record_response(home, "0".repeat(64), Some(refresh_token_digest("earlier")));
            },
            None,
            settled_at(),
            RefusalStanding::Unpresented { digest: held() },
        ),
        (
            "Attempted for the current token",
            |home| {
                RefreshJournal::Attempted {
                    token: refresh_token_digest("held"),
                }
                .write(home)
                .expect("write");
            },
            None,
            settled_at(),
            RefusalStanding::Unknown,
        ),
        (
            "Response for the current token",
            |home| record_response(home, base_of(home), Some(refresh_token_digest("held"))),
            None,
            settled_at(),
            RefusalStanding::Unknown,
        ),
        (
            "a Response that names no token",
            |home| record_response(home, "0".repeat(64), None),
            None,
            settled_at(),
            RefusalStanding::Unknown,
        ),
        (
            "an enrollment's response",
            |home| {
                native_receipt_fixture(home, NOW)
                    .write(home)
                    .expect("write")
            },
            None,
            settled_at(),
            RefusalStanding::Unknown,
        ),
        (
            "an unreadable journal",
            |home| std::fs::write(beside_auth(home, JOURNAL_SUFFIX), "{not").expect("write"),
            None,
            settled_at(),
            RefusalStanding::Unknown,
        ),
        (
            "another account",
            |home| record_refusal_for_test(home, RefusalReason::Expired, NOW).expect("write"),
            Some("other@example.test"),
            settled_at(),
            RefusalStanding::Unknown,
        ),
        (
            "another account, no journal",
            |_| {},
            Some("other@example.test"),
            settled_at(),
            RefusalStanding::Unknown,
        ),
        (
            "no refresh token",
            |home| {
                RefreshJournal::Rejected {
                    token: refresh_token_digest("held"),
                    at: Some(NOW),
                    reason: RefusalReason::Expired,
                }
                .write(home)
                .expect("write");
                hour_login(home, NOW, None);
            },
            None,
            settled_at(),
            RefusalStanding::Unknown,
        ),
    ];
    for (why, lay, expected, now, want) in cases {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("login");
        hour_login(&home, NOW, Some("held"));
        lay(&home);
        assert_eq!(refusal_standing(&home, expected, now), want, "{why}");
        for email in [None, Some("owner@example.test"), Some("other@example.test")] {
            for at in [
                NOW - 1,
                NOW,
                settled_at() - 1,
                settled_at(),
                settled_at() + 86_400,
            ] {
                let standing = refusal_standing(&home, email, at);
                assert_eq!(
                    settled_refusal(&home, email, at),
                    settled_refusal_before(&home, email, at),
                    "{why}, {email:?} at {at}: unchanged"
                );
                assert_eq!(
                    settled_refusal(&home, email, at),
                    standing.settled(),
                    "{why}, {email:?} at {at}: the standing's settled reason"
                );
            }
        }
    }
}

/// ONLY AN EXPIRED TOKEN ENDS ORDINARILY ([`RefusalReason::ends_ordinarily`]): the one reason a
/// move off a lost login may read as no one's decision. Every other reason stays a stop, one
/// negative each: a reused token (whoever spent it), an invalidated one, `invalid_grant`, and
/// every other code, `unknown` among them. A code outside the closed set, `account_deactivated`
/// among them, and a bare `401` with no code read `unknown`, which is no ordinary end.
#[test]
fn only_an_expired_token_ends_ordinarily() {
    assert!(RefusalReason::Expired.ends_ordinarily());
    for reason in [
        RefusalReason::Reused,
        RefusalReason::Invalidated,
        RefusalReason::InvalidGrant,
        RefusalReason::Other(OtherRefusal::InvalidRequest),
        RefusalReason::Other(OtherRefusal::InvalidClient),
        RefusalReason::Other(OtherRefusal::UnauthorizedClient),
        RefusalReason::Other(OtherRefusal::UnsupportedGrantType),
        RefusalReason::Other(OtherRefusal::InvalidScope),
        RefusalReason::Other(OtherRefusal::InvalidToken),
        RefusalReason::Other(OtherRefusal::Unknown),
    ] {
        assert!(!reason.ends_ordinarily(), "{reason:?} is a stop");
    }
    let ordinary: Vec<RefusalReason> = EVERY
        .into_iter()
        .filter(|reason| reason.ends_ordinarily())
        .collect();
    assert_eq!(
        ordinary,
        [RefusalReason::Expired],
        "exactly one, of every reason"
    );
    for word in ["account_deactivated", "account_banned", "unauthorized", ""] {
        assert_eq!(
            RefusalReason::of_code(word),
            RefusalReason::UNKNOWN,
            "{word:?}"
        );
        assert!(!RefusalReason::of_code(word).ends_ordinarily(), "{word:?}");
    }
    for body in [
        br#"{"error":{"code":"account_deactivated"}}"#.as_slice(),
        br#"{"error":"refresh_token_reused"}"#.as_slice(),
        b"".as_slice(),
    ] {
        assert!(
            !RefusalReason::of_answer(body).ends_ordinarily(),
            "{}",
            String::from_utf8_lossy(body)
        );
    }
    assert!(
        RefusalReason::of_answer(br#"{"error":{"code":"refresh_token_expired"}}"#)
            .ends_ordinarily()
    );
}

/// A ROW'S REFUSAL IS ITS LOGIN FILE'S ([`row_refusal`]): a row on a directory of its own reads
/// the standing there, for the address it expects; a row on the default login reads the default
/// login's place, and a row with no place named stands unknown. A report's reason
/// ([`row_signed_out_reason`]) is that standing's settled reason, only for a row signed out
/// here: the same refusal beside a row of any other condition annotates nothing, and a refusal
/// not yet settled says nothing.
#[test]
fn a_rows_refusal_is_its_login_files() {
    use crate::harness::accounts::{Condition, Row};
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let own = tmp.path().join("own");
    let default = tmp.path().join("default");
    // The own directory's refusal is made at NOW; the default login's is settled by then.
    let hang = settled_at() - NOW;
    for (home, at) in [(&own, NOW), (&default, NOW - hang)] {
        hour_login(home, NOW, Some("held"));
        record_refusal_for_test(home, RefusalReason::Expired, at).expect("write");
    }
    let row = |dir: Option<&Path>, email: &str| Row {
        label: "row".into(),
        dir: dir.map(|dir| dir.to_str().expect("UTF-8").to_owned()),
        email: Some(email.into()),
        ..Row::default()
    };
    let settled = RefusalStanding::Settled {
        reason: RefusalReason::Expired,
        digest: refresh_token_digest("held"),
    };
    let on_own = row(Some(&own), "owner@example.test");
    assert_eq!(row_refusal(&on_own, Some(&default), settled_at()), settled);
    assert_eq!(
        row_refusal(&on_own, None, settled_at() - 1),
        RefusalStanding::Pending {
            reason: RefusalReason::Expired,
            digest: refresh_token_digest("held"),
            settles_at: settled_at(),
        },
        "its own directory, not the default login"
    );
    let on_default = row(None, "owner@example.test");
    assert_eq!(row_refusal(&on_default, Some(&default), NOW), settled);
    assert_eq!(
        row_refusal(&on_default, None, NOW),
        RefusalStanding::Unknown,
        "no place named"
    );
    let empty = Row {
        dir: Some(String::new()),
        ..on_default.clone()
    };
    assert_eq!(
        row_refusal(&empty, Some(&default), NOW),
        settled,
        "an empty directory is the default login"
    );
    assert_eq!(
        row_refusal(&row(None, "other@example.test"), Some(&default), NOW),
        RefusalStanding::Unknown,
        "another account's login in the row's place"
    );
    assert_eq!(
        row_signed_out_reason(&on_default, Condition::SignedOut, Some(&default), NOW),
        Some(RefusalReason::Expired)
    );
    for condition in [
        Condition::NotEnrolled,
        Condition::Usable,
        Condition::Unseen,
        Condition::Low,
        Condition::Limited { until: NOW + 60 },
    ] {
        assert_eq!(
            row_signed_out_reason(&on_default, condition, Some(&default), NOW),
            None,
            "{condition:?}"
        );
    }
    assert_eq!(
        row_signed_out_reason(&on_own, Condition::SignedOut, None, settled_at() - 1),
        None,
        "not yet settled"
    );
}

/// A SIGN-IN CLEARS THE REASON: once the login file holds another refresh token (a sign-in by
/// codex-alab or a person), the refusal of the old one is no longer this login's, and the next
/// read of the login discards it. aterm's own sign-in discards it outright
/// (`code_exchange_validates_and_clears_refresh_journal_around_publication`). CONTROL: the
/// same login, not signed in again, keeps it across reads; one with no refresh token has none.
#[test]
fn a_sign_in_clears_the_refusal_on_record() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("login");
    hour_login(&home, NOW, Some("spent"));
    let (outcome, _) = refresh_answered(
        &home,
        "400 Bad Request",
        json!({"error":"refresh_token_expired"}),
    );
    assert!(matches!(outcome, Refreshed::Rejected), "{outcome:?}");
    recover_journal(&home, Instant::now() + aterm_spec::wait::MUST_HAPPEN);
    assert_eq!(
        settled_refusal(&home, None, settled_at()),
        Some(RefusalReason::Expired),
        "CONTROL: a read keeps the refusal of the token the login still holds"
    );

    hour_login(&home, NOW, Some("signed-in-again"));
    assert_eq!(
        settled_refusal(&home, None, settled_at()),
        None,
        "the new login's own"
    );
    assert!(beside_auth(&home, JOURNAL_SUFFIX).exists());
    recover_journal(&home, Instant::now() + aterm_spec::wait::MUST_HAPPEN);
    assert!(
        !beside_auth(&home, JOURNAL_SUFFIX).exists(),
        "the next read discards the old token's refusal"
    );

    hour_login(&home, NOW, None);
    RefreshJournal::Rejected {
        token: refresh_token_digest("signed-in-again"),
        at: Some(NOW),
        reason: RefusalReason::Expired,
    }
    .write(&home)
    .unwrap();
    assert_eq!(
        settled_refusal(&home, None, settled_at()),
        None,
        "CONTROL: no refresh token"
    );
}

/// NOTHING OF THE ANSWER OR THE TOKEN IS KEPT: an answer that names an address, free text and
/// a token beside its code leaves a journal of the closed word, the refused token's digest and
/// the time alone, and a code outside the set is kept as `unknown`, not as itself.
#[test]
fn a_refusal_keeps_no_text_of_the_answer_or_the_token() {
    const TOKEN: &str = concat!("rt-private-", "0f1e2d");
    for (body, word) in [
        (
            json!({"error":{"code":"refresh_token_expired",
                "message":"owner@example.test: session ended (ref 9f8e7d)"},
                "refresh_token":concat!("rt-echoed-", "5a6b")}),
            "refresh_token_expired",
        ),
        (
            json!({"error":"owner@example.test session ended (ref 9f8e7d)"}),
            "unknown",
        ),
    ] {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("login");
        hour_login(&home, NOW, Some(TOKEN));
        let (outcome, _) = refresh_answered(&home, "401 Unauthorized", body);
        assert!(matches!(outcome, Refreshed::Rejected), "{outcome:?}");
        let bytes = std::fs::read(beside_auth(&home, JOURNAL_SUFFIX)).expect("the journal");
        let text = String::from_utf8(bytes).expect("UTF-8");
        for kept in [
            TOKEN,
            concat!("rt-echoed-", "5a6b"),
            "owner@example.test",
            "session ended",
            "9f8e7d",
            "message",
        ] {
            assert!(!text.contains(kept), "{kept:?} kept: {text}");
        }
        let doc: Value = aterm_json::from_str(&text).expect("JSON");
        let keys: BTreeSet<&str> = doc
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            BTreeSet::from([
                "aterm_codex_refresh",
                "state",
                "refresh_token_digest",
                "rejected_at",
                "reason",
            ])
        );
        assert_eq!(doc["reason"], Value::from(word));
        assert_eq!(
            doc["refresh_token_digest"],
            Value::from(refresh_token_digest(TOKEN))
        );
    }
}
