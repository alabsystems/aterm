// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The lost-login decision ([`crate::harness::codex_login_rotation::prepare`]) against a counting
//! provider: what it reads, how often, and what it decides of the records after each read.

use super::session_scope::{Lost, lost};
use super::*;
use crate::harness::codex_app_login::{OtherRefusal, RefusalReason, RefusalStanding};
use crate::harness::codex_login_rotation::{
    self as login, LoginDecision, LostLoginEnd, ReadLedger,
};
use std::cell::{Cell, RefCell};

/// A provider for the lost-login fixture, as the live reader answers: the session's own login
/// (`main`) presents its token where aterm never presented it, and the issuer refuses it for
/// `refuses` (aterm's own refused send writes its journal and the read answers nothing yet); once
/// that refusal settled the login reads signed out; or, where `works`, it serves usage. `work`
/// serves usage of another account. Every read and presentation is counted.
struct Provider {
    source: PathBuf,
    refuses: RefusalReason,
    works: Cell<bool>,
    calls: RefCell<Vec<PathBuf>>,
    presented: Cell<usize>,
    target: RefCell<Option<IdentifiedUsage>>,
}

impl Provider {
    fn new(lost: &Lost) -> Self {
        Self {
            source: lost.source.join("auth.json"),
            refuses: RefusalReason::Expired,
            works: Cell::new(false),
            calls: RefCell::new(Vec::new()),
            presented: Cell::new(0),
            target: RefCell::new(Some(usage("target", false))),
        }
    }

    fn reads_of(&self, path: &Path) -> usize {
        self.calls.borrow().iter().filter(|p| *p == path).count()
    }
}

impl UsageReader for Provider {
    fn read(
        &self,
        path: &Path,
        _: Option<&str>,
        now: i64,
        wait: Duration,
        use_token: crate::harness::provider_read::Use,
    ) -> IdentifiedRead {
        if use_token
            .begin_home(
                crate::harness::provider_read::Read::CodexEndpoint,
                path,
                now,
            )
            .is_err()
        {
            return IdentifiedRead::Unread;
        }
        assert!(!wait.is_zero());
        self.calls.borrow_mut().push(path.to_owned());
        if path != self.source {
            return self
                .target
                .borrow()
                .clone()
                .map_or(IdentifiedRead::Unread, IdentifiedRead::Usage);
        }
        if self.works.get() {
            return IdentifiedRead::Usage(usage("source", false));
        }
        let home = path.parent().unwrap();
        match crate::harness::codex_app_login::refusal_standing(home, None, now) {
            RefusalStanding::Unpresented { .. } => {
                self.presented.set(self.presented.get() + 1);
                crate::harness::codex_app_login::record_refusal_for_test(home, self.refuses, now)
                    .unwrap();
                IdentifiedRead::Unread
            }
            RefusalStanding::Settled { .. } => IdentifiedRead::SignedOut,
            RefusalStanding::Pending { .. } | RefusalStanding::Unknown => IdentifiedRead::Unread,
        }
    }
}

fn lost_session(world: &World) -> Session<'_> {
    let mut session = session(Cli::Codex, Supervisor::Own, Mode::Early, &world.exe);
    session.point = Point::LoginEnded;
    session
}

/// [`login::prepare`] for `lost`'s session at `now`.
fn decide(
    lost: &Lost,
    reader: &dyn UsageReader,
    now: i64,
    ledger: &mut ReadLedger,
) -> Result<LoginDecision, &'static str> {
    let seat = Seat {
        now,
        ..lost.world.seat()
    };
    login::prepare(
        &seat,
        &lost_session(&lost.world),
        &Identity::from_native("source", "user"),
        reader,
        Duration::from_secs(60),
        ledger,
    )
}

fn moved_to(decision: &Result<LoginDecision, &'static str>) -> Option<String> {
    match decision {
        Ok(LoginDecision::Move(next)) => Some(next.destination.label.clone()),
        _ => None,
    }
}

fn not_needed(end: LostLoginEnd) -> Result<LoginDecision, &'static str> {
    Ok(LoginDecision::NotNeeded(end))
}

/// Edits the tokens of the login file at `path`.
fn edit_tokens(path: &Path, edit: impl FnOnce(&mut aterm_json::Map)) {
    let mut login: aterm_json::Value =
        aterm_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    edit(
        login
            .as_object_mut()
            .and_then(|doc| doc.get_mut("tokens"))
            .and_then(aterm_json::Value::as_object_mut)
            .unwrap(),
    );
    std::fs::write(path, login.to_string()).unwrap();
}

/// THE COMMON LOST LOGIN MOVES, AFTER ONE PRESENTATION AND ONE CONFIRMING READ: a token aterm never
/// presented, on a login not yet signed out on record, whose issuer refuses it as expired. The
/// first ask presents it once and, the refusal being aterm's own and not yet settled, waits until
/// it settles, noting nothing of the read; an ask before then reads nothing; the ask once it
/// settled makes one usage-only read, which reads the login signed out, stages that (ending the
/// pin set on the row before the loss) and reads each candidate once: the session moves to `work`.
/// In all, the source is read twice, its token presented once, the destination once, and nothing
/// after the move. The pool's `active` row, rotations, canary and deduplication records are as
/// they were, and its spent hourly breaker neither stops the move nor is charged.
#[test]
fn the_common_lost_login_presents_once_waits_for_its_refusal_to_settle_then_moves() {
    let lost = lost("login-common");
    let spent = [NOW - 300, NOW - 240, NOW - 180, NOW - 120];
    lost.edit(|state| {
        state.active = Some("main".into());
        state.pinned = true;
        state.rotations = spent.to_vec();
    });
    let reader = Provider::new(&lost);
    let roster = lost.world.seat().bound_roster(Cli::Codex).unwrap();
    assert!(accounts::rotations_spent(
        &lost.world.load(Cli::Codex),
        &roster,
        NOW
    ));
    let source = lost.source.join("auth.json");
    let target = lost.target.join("auth.json");
    let mut ledger = ReadLedger::default();

    let first = decide(&lost, &reader, NOW, &mut ledger);
    assert_eq!(
        first,
        Ok(LoginDecision::Wait {
            until: Some(NOW + 60)
        })
    );
    assert_eq!((reader.reads_of(&source), reader.presented.get()), (1, 1));
    let state = lost.world.load(Cli::Codex);
    assert!(
        state
            .accounts
            .get("main")
            .is_none_or(|a| a.unanswered.is_none()),
        "the presenting read is paced by the journal, not noted unanswered"
    );
    assert!(state.pinned, "nothing is recorded yet");

    let early = decide(&lost, &reader, NOW + 30, &mut ledger);
    assert_eq!(
        early,
        Ok(LoginDecision::Wait {
            until: Some(NOW + 60)
        })
    );
    assert_eq!(reader.calls.borrow().len(), 1, "no read before it settles");

    let settled = decide(&lost, &reader, NOW + 60, &mut ledger);
    assert_eq!(moved_to(&settled), Some("work".into()), "{settled:?}");
    let Ok(LoginDecision::Move(next)) = &settled else {
        unreachable!()
    };
    assert_eq!(next.reason, RefusalReason::Expired);
    assert_eq!(next.old_auth, source);
    assert_eq!(next.new_auth, target);
    assert_eq!(
        next.destination_identity,
        Identity::from_native("target", "user")
    );
    assert_eq!(
        (
            reader.reads_of(&source),
            reader.presented.get(),
            reader.reads_of(&target)
        ),
        (2, 1, 1)
    );
    let state = lost.world.load(Cli::Codex);
    assert!(!state.pinned, "staging the sign-out ended the row's pin");
    assert_eq!(state.active.as_deref(), Some("main"));
    assert_eq!(
        state.rotations,
        spent.to_vec(),
        "the breaker is not charged"
    );
    assert!(state.canary.is_none());
    assert!(
        lost.world
            .seat()
            .store(Cli::Codex)
            .load_logins(NOW)
            .unwrap()
            .0
            .native_rotations
            .is_empty()
    );
    assert_eq!(
        crate::harness::codex_app_login::row_refusal(
            roster.row("main").unwrap(),
            None,
            Some(&lost.source),
            NOW + 60
        )
        .settled(),
        Some(RefusalReason::Expired)
    );
    // Asked again, it reads nothing: the decision stands on the records, and this loss's
    // candidates were read.
    let again = decide(&lost, &reader, NOW + 61, &mut ledger);
    assert_eq!(again, not_needed(LostLoginEnd::Spent));
    assert_eq!(reader.calls.borrow().len(), 3, "nothing after the move");
}

/// A SOURCE ALREADY SIGNED OUT ON RECORD AFTER A SETTLED EXPIRY READS ONLY ITS CANDIDATES: no read
/// of its own login, one of `work`, and the move.
#[test]
fn a_source_signed_out_after_a_settled_expiry_reads_only_its_candidates() {
    let lost = lost("login-already-out");
    lost.ended(RefusalReason::Expired);
    let reader = Provider::new(&lost);
    let decision = decide(&lost, &reader, NOW, &mut ReadLedger::default());
    assert_eq!(moved_to(&decision), Some("work".into()), "{decision:?}");
    assert_eq!(*reader.calls.borrow(), vec![lost.target.join("auth.json")]);
}

/// EVERY OTHER LOSS IS DECIDED FROM THE RECORDS WITH NO READ: a settled refusal for any reason but
/// an expiry (reused, invalidated, `invalid_grant`, an unknown code as a deactivation reads, an
/// invalid token), with or without the sign-out on record; a login with no refresh token; one in
/// aterm's custody; an attempt whose answer was lost; another row's pin; a pin on the source set
/// after its sign-out; a row whose login a renewal replaced; no other row to take it; accounts off.
#[test]
fn every_other_loss_stays_a_stop_with_no_provider_read() {
    let check = |lost: &Lost, expected: Result<LoginDecision, &'static str>| {
        let reader = Provider::new(lost);
        assert_eq!(
            decide(lost, &reader, NOW, &mut ReadLedger::default()),
            expected
        );
        assert!(reader.calls.borrow().is_empty(), "{:?}", reader.calls);
    };
    for reason in [
        RefusalReason::Reused,
        RefusalReason::Invalidated,
        RefusalReason::InvalidGrant,
        RefusalReason::of_code("account_deactivated"),
        RefusalReason::Other(OtherRefusal::InvalidToken),
    ] {
        let out = lost("login-refused-out");
        out.ended(reason);
        check(&out, not_needed(LostLoginEnd::Refused(reason)));
        let recorded = lost("login-refused-recorded");
        recorded.refuse(reason, NOW - 600);
        check(&recorded, not_needed(LostLoginEnd::Refused(reason)));
    }

    let tokenless = lost("login-no-refresh-token");
    tokenless.stage("main", accounts::Auth::SignedOut);
    edit_tokens(&tokenless.source.join("auth.json"), |tokens| {
        tokens.remove("refresh_token");
    });
    check(&tokenless, not_needed(LostLoginEnd::Unrecorded));

    let custody = lost("login-custody");
    custody.ended(RefusalReason::Expired);
    std::fs::write(custody.source.join("auth.json.aterm-owner"), "").unwrap();
    check(&custody, not_needed(LostLoginEnd::Unrecorded));

    let attempted = lost("login-attempted");
    let roster = attempted.world.seat().bound_roster(Cli::Codex).unwrap();
    let RefusalStanding::Unpresented { digest } = crate::harness::codex_app_login::row_refusal(
        roster.row("main").unwrap(),
        None,
        Some(&attempted.source),
        NOW,
    ) else {
        panic!("an unpresented token");
    };
    std::fs::write(
        attempted.source.join("auth.json.refreshed"),
        aterm_json::json!({"aterm_codex_refresh": 2, "state": "attempted",
            "refresh_token_digest": digest})
        .to_string(),
    )
    .unwrap();
    check(&attempted, not_needed(LostLoginEnd::Unrecorded));

    let elsewhere = lost("login-pinned-elsewhere");
    elsewhere.ended(RefusalReason::Expired);
    elsewhere.edit(|state| {
        state.active = Some("work".into());
        state.pinned = true;
    });
    check(&elsewhere, not_needed(LostLoginEnd::PinnedElsewhere));

    let repinned = lost("login-repinned");
    repinned.ended(RefusalReason::Expired);
    repinned.edit(|state| {
        state.active = Some("main".into());
        state.pinned = true;
    });
    check(&repinned, not_needed(LostLoginEnd::Pinned));

    let alone = lost("login-no-target");
    alone.ended(RefusalReason::Expired);
    alone.stage("work", accounts::Auth::SignedOut);
    check(&alone, not_needed(LostLoginEnd::NoTarget));

    let off = lost("login-accounts-off");
    off.ended(RefusalReason::Expired);
    std::fs::write(
        off.world.cfg.join("aterm.toml"),
        "[harness]\naccounts = 'off'\n",
    )
    .unwrap();
    let reader = Provider::new(&off);
    let seat = off.world.seat();
    let mut session = lost_session(&off.world);
    session.mode = Mode::Off;
    assert_eq!(
        login::prepare(
            &seat,
            &session,
            &Identity::from_native("source", "user"),
            &reader,
            Duration::from_secs(60),
            &mut ReadLedger::default(),
        ),
        not_needed(LostLoginEnd::Off)
    );
    assert!(reader.calls.borrow().is_empty());
}

/// A TOKEN THE ISSUER REFUSES FOR ANY REASON BUT AN EXPIRY IS PRESENTED ONCE AND NEVER MOVES ITS
/// SESSION: the common loss (a token aterm never presented, a login not signed out on record) whose
/// issuer answers reused, invalidated, `invalid_grant`, a deactivation's unnamed code or an invalid
/// token. The first ask presents the token once and waits for aterm's own refusal to settle; the
/// ask once it settled is decided from the record with no read, and so is a relaunched process's
/// (a new ledger). No candidate is read, no sign-out is staged and the pin on the row stands.
/// CONTROL: the same loss refused as expired moves
/// ([`the_common_lost_login_presents_once_waits_for_its_refusal_to_settle_then_moves`]).
#[test]
fn a_token_refused_for_any_other_reason_is_presented_once_and_never_moves() {
    for reason in [
        RefusalReason::Reused,
        RefusalReason::Invalidated,
        RefusalReason::InvalidGrant,
        RefusalReason::of_code("account_deactivated"),
        RefusalReason::Other(OtherRefusal::InvalidToken),
    ] {
        let lost = lost("login-refused-now");
        lost.edit(|state| {
            state.active = Some("main".into());
            state.pinned = true;
        });
        let mut reader = Provider::new(&lost);
        reader.refuses = reason;
        let mut ledger = ReadLedger::default();
        assert_eq!(
            decide(&lost, &reader, NOW, &mut ledger),
            Ok(LoginDecision::Wait {
                until: Some(NOW + 60)
            }),
            "{reason:?}"
        );
        assert_eq!(
            decide(&lost, &reader, NOW + 60, &mut ledger),
            not_needed(LostLoginEnd::Refused(reason))
        );
        assert_eq!(
            decide(&lost, &reader, NOW + 61, &mut ReadLedger::default()),
            not_needed(LostLoginEnd::Refused(reason)),
            "a relaunched process"
        );
        assert_eq!(
            *reader.calls.borrow(),
            vec![lost.source.join("auth.json")],
            "{reason:?}: the one presentation, and no candidate"
        );
        assert_eq!(reader.presented.get(), 1, "{reason:?}");
        let state = lost.world.load(Cli::Codex);
        assert!(state.pinned, "{reason:?}: no sign-out was staged");
        let roster = lost.world.seat().bound_roster(Cli::Codex).unwrap();
        assert_ne!(
            accounts::condition(roster.row("main").unwrap(), &state, true, NOW + 61),
            accounts::Condition::SignedOut,
            "{reason:?}"
        );
    }
}

/// A ROW HELD FOR ANOTHER MAC, A LOGIN NO ROW MANAGES AND A ROW WHOSE LOGIN NAMES ANOTHER ACCOUNT
/// ARE STOPS WITH NO READ, though the login ended ordinarily and `work` could take the session.
/// CONTROL: the same loss with none of them moves.
#[test]
fn a_held_row_an_unmanaged_login_or_one_naming_another_account_does_not_move() {
    let roster_with = |lost: &Lost, key: &str| {
        let roster = lost.world.cfg.join(Cli::Codex.accounts().roster_file);
        let text = std::fs::read_to_string(&roster).unwrap();
        let main = "label = \"main\"\n";
        assert!(text.contains(main));
        std::fs::write(&roster, text.replacen(main, &format!("{main}{key}\n"), 1)).unwrap();
    };
    let stopped = |lost: &Lost, end: LostLoginEnd| {
        let reader = Provider::new(lost);
        assert_eq!(
            decide(lost, &reader, NOW, &mut ReadLedger::default()),
            not_needed(end)
        );
        assert!(reader.calls.borrow().is_empty(), "{end:?}");
    };

    let held = lost("login-held-row");
    held.ended(RefusalReason::Expired);
    roster_with(&held, "held_by = \"another-mac\"");
    stopped(&held, LostLoginEnd::Held);

    let named = lost("login-names-another");
    roster_with(&named, "email = \"owner@example.com\"");
    let login = named.source.join("auth.json");
    native_auth_naming(&login, "source", Some("owner@example.com"));
    named.ended(RefusalReason::Expired);
    let reader = Provider::new(&named);
    let control = decide(&named, &reader, NOW, &mut ReadLedger::default());
    assert_eq!(
        moved_to(&control),
        Some("work".into()),
        "CONTROL: {control:?}"
    );
    native_auth_naming(&login, "source", Some("z@example.com"));
    named.refuse(RefusalReason::Expired, NOW - 600);
    stopped(&named, LostLoginEnd::NamesAnother);

    let unmanaged = lost("login-unmanaged");
    unmanaged.ended(RefusalReason::Expired);
    let elsewhere = unmanaged.world.root.join("logins/codex/unmanaged");
    std::fs::create_dir_all(&elsewhere).unwrap();
    native_auth(&elsewhere.join("auth.json"), "source");
    let home = elsewhere.to_string_lossy().into_owned();
    let variables = [("CODEX_HOME", home.as_str())];
    let mut session = lost_session(&unmanaged.world);
    session.env = &variables;
    let reader = Provider::new(&unmanaged);
    assert_eq!(
        login::prepare(
            &unmanaged.world.seat(),
            &session,
            &Identity::from_native("source", "user"),
            &reader,
            Duration::from_secs(60),
            &mut ReadLedger::default(),
        ),
        not_needed(LostLoginEnd::NoRow)
    );
    assert!(reader.calls.borrow().is_empty());
}

/// A READ THIS LOSS ALREADY SPENT IS NOT MADE AGAIN, WHATEVER THE RECORDS' OWN PACING SAYS: once the
/// token was presented, a later ask on the same ledger is answered paced with no read, and once
/// the read confirming a settled expiry was made, a later ask is answered unconfirmed with no
/// read, though the provider would now refuse the token or read the login signed out. Each is
/// shown where nothing but the ledger stands in the way: past the wait a read that answered
/// nothing leaves on the row ([`accounts::UNANSWERED_S`]), and after a read that answered usage,
/// which leaves no wait at all. CONTROL: a new ledger (a relaunched process) makes that read.
#[test]
fn a_spent_presentation_or_confirming_read_is_not_made_again() {
    /// The first read of each loss, in place of the provider's own answer.
    #[derive(Clone, Copy, Debug)]
    enum First {
        /// It answers nothing and leaves nothing on record but the wait.
        Silent,
        /// It answers usage: the login works.
        Usage,
    }
    struct Stub<'a>(&'a Provider, First);
    impl UsageReader for Stub<'_> {
        fn read(
            &self,
            path: &Path,
            _: Option<&str>,
            now: i64,
            _: Duration,
            use_token: crate::harness::provider_read::Use,
        ) -> IdentifiedRead {
            let _ = use_token.begin_home(
                crate::harness::provider_read::Read::CodexEndpoint,
                path,
                now,
            );
            self.0.calls.borrow_mut().push(path.to_owned());
            match self.1 {
                First::Silent => IdentifiedRead::Unread,
                First::Usage => IdentifiedRead::Usage(usage("source", false)),
            }
        }
    }
    // Past the first wait a read that answered nothing leaves on its row.
    let later = NOW + accounts::UNANSWERED_S[0] + 1;
    let reads_admitted = |lost: &Lost, now: i64| {
        let roster = lost.world.seat().bound_roster(Cli::Codex).unwrap();
        crate::harness::accounts_pool::handoff_reads(
            roster.row("main").unwrap(),
            &lost.world.load(Cli::Codex),
            now,
        )
    };
    let source_reads =
        |lost: &Lost, reader: &Provider| reader.reads_of(&lost.source.join("auth.json"));

    for (first, answered, again_at) in [
        (First::Silent, LostLoginEnd::Unconfirmed, later),
        (First::Usage, LostLoginEnd::LoginWorks, NOW),
    ] {
        let presented = lost("login-presented-once");
        let reader = Provider::new(&presented);
        let mut ledger = ReadLedger::default();
        assert_eq!(
            decide(&presented, &Stub(&reader, first), NOW, &mut ledger),
            not_needed(answered),
            "{first:?}"
        );
        assert_eq!(reader.calls.borrow().len(), 1, "{first:?}: presented once");
        assert!(
            reads_admitted(&presented, again_at),
            "{first:?}: the records pace no read of the row then, so only the ledger can"
        );
        assert_eq!(
            decide(&presented, &reader, again_at, &mut ledger),
            not_needed(LostLoginEnd::Paced),
            "{first:?}"
        );
        assert_eq!(
            (reader.calls.borrow().len(), reader.presented.get()),
            (1, 0),
            "{first:?}: the second ask neither read nor presented"
        );
        // CONTROL: the same ask on a new ledger presents the token.
        assert_eq!(
            decide(&presented, &reader, again_at, &mut ReadLedger::default()),
            Ok(LoginDecision::Wait {
                until: Some(again_at + 60)
            }),
            "{first:?}"
        );
        assert_eq!(reader.presented.get(), 1, "{first:?}: CONTROL");
    }

    for (first, answered, again_at) in [
        (First::Silent, LostLoginEnd::Unconfirmed, later),
        (First::Usage, LostLoginEnd::LoginWorks, NOW),
    ] {
        let confirmed = lost("login-confirmed-once");
        confirmed.refuse(RefusalReason::Expired, NOW - 600);
        let reader = Provider::new(&confirmed);
        let mut ledger = ReadLedger::default();
        assert_eq!(
            decide(&confirmed, &Stub(&reader, first), NOW, &mut ledger),
            not_needed(answered),
            "{first:?}"
        );
        assert_eq!(
            reader.calls.borrow().len(),
            1,
            "{first:?}: the one confirming read"
        );
        assert!(
            reads_admitted(&confirmed, again_at),
            "{first:?}: the records pace no read of the row then, so only the ledger can"
        );
        assert_eq!(
            decide(&confirmed, &reader, again_at, &mut ledger),
            not_needed(LostLoginEnd::Unconfirmed),
            "{first:?}"
        );
        assert_eq!(
            reader.calls.borrow().len(),
            1,
            "{first:?}: not confirmed again"
        );
        // CONTROL: the same ask on a new ledger makes the confirming read, which reads the
        // login signed out, and the session moves.
        let control = decide(&confirmed, &reader, again_at, &mut ReadLedger::default());
        assert_eq!(
            moved_to(&control),
            Some("work".into()),
            "{first:?}: {control:?}"
        );
        assert_eq!(source_reads(&confirmed, &reader), 2, "{first:?}: CONTROL");
    }
}

/// A ROW WHOSE LOGIN A RENEWAL REPLACED IS THE RENEWAL'S TO MOVE: a session whose process still
/// holds the login its row moved off reads nothing and is answered that its row was renewed.
#[test]
fn a_renewed_rows_session_is_left_to_its_renewal() {
    let lost = lost("login-renewed");
    lost.ended(RefusalReason::Expired);
    let moved = lost.world.root.join("logins/codex/main-renewed");
    std::fs::create_dir_all(&moved).unwrap();
    native_auth(&moved.join("auth.json"), "source");
    lost.world
        .seat()
        .store(Cli::Codex)
        .transact_logins(NOW, &mut None, |_, logins| {
            logins.labels.insert(
                "main".to_string(),
                accounts::Binding {
                    dir: Some(moved.display().to_string()),
                    was: vec![None],
                    ..accounts::Binding::default()
                },
            );
            Ok(Vec::new())
        })
        .unwrap();
    let reader = Provider::new(&lost);
    assert_eq!(
        decide(&lost, &reader, NOW, &mut ReadLedger::default()),
        not_needed(LostLoginEnd::RowRenewed)
    );
    assert!(reader.calls.borrow().is_empty());
}

/// A PLAIN REFUSAL WHILE THE LOGIN STILL SERVES USAGE IS NO LOST LOGIN: the one presenting read
/// answers usage, and the session is told its login works. A presenting read that answers nothing
/// and leaves no refusal on record is noted unanswered and confirms nothing. A confirming read that
/// answers usage once aterm's own refusal settled is a login that works too: two reads in all.
#[test]
fn a_login_that_still_serves_usage_or_answers_nothing_does_not_move() {
    let works = lost("login-works");
    let reader = Provider::new(&works);
    reader.works.set(true);
    assert_eq!(
        decide(&works, &reader, NOW, &mut ReadLedger::default()),
        not_needed(LostLoginEnd::LoginWorks)
    );
    assert_eq!(reader.calls.borrow().len(), 1);

    // A reader that presents nothing: the token stays unpresented and the read says nothing.
    struct Silent<'a>(&'a Provider);
    impl UsageReader for Silent<'_> {
        fn read(
            &self,
            path: &Path,
            _: Option<&str>,
            now: i64,
            _: Duration,
            use_token: crate::harness::provider_read::Use,
        ) -> IdentifiedRead {
            let _ = use_token.begin_home(
                crate::harness::provider_read::Read::CodexEndpoint,
                path,
                now,
            );
            self.0.calls.borrow_mut().push(path.to_owned());
            IdentifiedRead::Unread
        }
    }
    let silent = lost("login-silent");
    let reader = Provider::new(&silent);
    assert_eq!(
        login::prepare(
            &silent.world.seat(),
            &lost_session(&silent.world),
            &Identity::from_native("source", "user"),
            &Silent(&reader),
            Duration::from_secs(60),
            &mut ReadLedger::default(),
        ),
        not_needed(LostLoginEnd::Unconfirmed)
    );
    assert_eq!(reader.calls.borrow().len(), 1);
    assert!(
        silent
            .world
            .load(Cli::Codex)
            .accounts
            .get("main")
            .is_some_and(|account| account.unanswered.is_some()),
        "the read that answered nothing is on record"
    );

    let confirmed = lost("login-confirm-works");
    let reader = Provider::new(&confirmed);
    let mut ledger = ReadLedger::default();
    assert_eq!(
        decide(&confirmed, &reader, NOW, &mut ledger),
        Ok(LoginDecision::Wait {
            until: Some(NOW + 60)
        })
    );
    reader.works.set(true);
    assert_eq!(
        decide(&confirmed, &reader, NOW + 60, &mut ledger),
        not_needed(LostLoginEnd::LoginWorks)
    );
    assert_eq!(
        (reader.calls.borrow().len(), reader.presented.get()),
        (2, 1),
        "the presentation and the one confirming read"
    );
}

/// A CANDIDATE IS READ ONLY WHERE IT COULD TAKE THE SESSION: one of the source's own account is
/// read and passed over; one another Mac's live lease holds, or one whose earlier successor for
/// this source never attached, is not read at all.
#[test]
fn a_candidate_of_the_same_account_leased_elsewhere_or_held_off_is_passed_over() {
    let same = lost("login-same-account");
    same.ended(RefusalReason::Expired);
    let reader = Provider::new(&same);
    *reader.target.borrow_mut() = Some(usage("source", false));
    assert_eq!(
        decide(&same, &reader, NOW, &mut ReadLedger::default()),
        not_needed(LostLoginEnd::NoTarget)
    );
    assert_eq!(reader.calls.borrow().len(), 1, "read, then passed over");

    let leased = lost("login-leased");
    leased.ended(RefusalReason::Expired);
    leased.edit(|state| state.note_lease("work", "m-elsewhere", NOW + 3600, NOW));
    let reader = Provider::new(&leased);
    assert_eq!(
        decide(&leased, &reader, NOW, &mut ReadLedger::default()),
        not_needed(LostLoginEnd::NoTarget)
    );
    assert!(reader.calls.borrow().is_empty());

    let held = lost("login-held-off");
    held.ended(RefusalReason::Expired);
    let mut moved = held.receipt();
    moved.id = "d".repeat(32);
    moved.phase = crate::harness::codex_renewal::Phase::Committing;
    moved.launch_attempted = true;
    crate::harness::codex_renewal::with_receipt_writer(&held.env, |writer| {
        crate::harness::codex_renewal::write_receipt(&held.env, &moved)?;
        writer.cancel(
            &moved,
            crate::harness::codex_renewal::CancellationKind::SuccessorNeverAttached,
            0,
            0,
        )?;
        let ended =
            crate::harness::codex_renewal::read_receipt(&held.env, &moved.id).ok_or("ended")?;
        writer.note_unattached_login(&ended)
    })
    .unwrap()
    .unwrap();
    let reader = Provider::new(&held);
    assert_eq!(
        decide(&held, &reader, NOW, &mut ReadLedger::default()),
        not_needed(LostLoginEnd::NoTarget)
    );
    assert!(reader.calls.borrow().is_empty());
}

/// THE LEDGER BOUNDS ONE LOSS'S READS AND A NEW TOKEN IS A NEW LOSS: a second pass after this
/// token's candidates were read reads nothing, while a login signed in again (another refresh
/// token, refused in its turn) admits each read again.
#[test]
fn the_ledger_spends_each_read_once_per_token_and_a_new_login_resets_it() {
    let lost = lost("login-ledger");
    lost.ended(RefusalReason::Expired);
    let reader = Provider::new(&lost);
    let mut ledger = ReadLedger::default();
    // `work` reads as the source's own account: read once, and passed over.
    *reader.target.borrow_mut() = Some(usage("source", false));
    assert_eq!(
        decide(&lost, &reader, NOW, &mut ledger),
        not_needed(LostLoginEnd::NoTarget)
    );
    assert_eq!(reader.calls.borrow().len(), 1);
    assert_eq!(
        decide(&lost, &reader, NOW, &mut ledger),
        not_needed(LostLoginEnd::Spent)
    );
    assert_eq!(reader.calls.borrow().len(), 1, "spent");

    // A new sign-in replaces the login: another token, refused in its turn.
    edit_tokens(&lost.source.join("auth.json"), |tokens| {
        tokens.insert("refresh_token".into(), "next".into());
    });
    lost.refuse(RefusalReason::Expired, NOW - 600);
    lost.stage("main", accounts::Auth::SignedOut);
    *reader.target.borrow_mut() = Some(usage("target", false));
    let renewed = decide(&lost, &reader, NOW, &mut ledger);
    assert_eq!(moved_to(&renewed), Some("work".into()), "{renewed:?}");
    assert_eq!(reader.calls.borrow().len(), 2);
}

/// A RENEWAL OF THE ROW UNDER WAY IS WAITED FOR, WITH NO READ: whether it brings the login back is
/// that renewal's to say.
#[test]
fn a_renewal_under_way_is_waited_for_with_no_read() {
    let lost = lost("login-renewing");
    lost.ended(RefusalReason::Expired);
    let reader = Provider::new(&lost);
    let _renewing = crate::harness::accounts_sync::hold_login_activity_for_test(
        &lost.world.seat().store(Cli::Codex),
        Cli::Codex,
        "main",
    );
    assert_eq!(
        decide(&lost, &reader, NOW, &mut ReadLedger::default()),
        Ok(LoginDecision::Wait { until: None })
    );
    assert!(reader.calls.borrow().is_empty());
}

/// ANOTHER THREAD'S INSTANT ON THE ACTIVITY'S LOCK IS NO RENEWAL OF THE ROW: the decision waits
/// for the lock and answers what the records say, as it does with the lock free.
#[test]
fn a_busy_activity_lock_is_waited_out_and_changes_no_answer() {
    let lost = lost("login-activity-busy");
    lost.ended(RefusalReason::Expired);
    let reader = Provider::new(&lost);
    let (holds, held) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _busy = crate::harness::accounts_sync::hold_login_observation_for_test();
        holds.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(200));
    });
    held.recv().unwrap();
    let decision = decide(&lost, &reader, NOW, &mut ReadLedger::default());
    holder.join().unwrap();
    assert_eq!(moved_to(&decision), Some("work".into()), "{decision:?}");
    assert_eq!(*reader.calls.borrow(), vec![lost.target.join("auth.json")]);
}
