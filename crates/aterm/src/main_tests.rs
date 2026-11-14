// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

/// The command exits with the package pass's own status whatever the follow-up's, a panic
/// included, and the follow-up runs after the pass, only for a person's pass whose words
/// setup follows ([`atpkg::cli::pass_finishes_setup`] holds the grammar) and that ended
/// settled ([`atpkg::cli::pass_ended_settled`] holds what each exit means): a success, or a
/// whole update's 2 that found nothing installable here, never a usage error's 2.
#[test]
fn a_package_pass_keeps_its_status_and_setup_follows_only_a_persons_settled_pass() {
    use std::cell::RefCell;
    use std::ffi::OsString;
    use std::process::ExitCode;
    let ok = (ExitCode::SUCCESS, true);
    let nothing_here = (ExitCode::from(2), true);
    for (words, person, (package_code, settled), follows) in [
        (&["install", "trust"][..], true, ok, true),
        (&["update"], true, ok, true),
        (&["update"], true, nothing_here, true),
        (
            &["update", "--bogus"],
            true,
            (ExitCode::from(2), false),
            false,
        ),
        (&["update"], true, (ExitCode::from(2), false), false),
        (&["update"], true, (ExitCode::FAILURE, false), false),
        (&["update"], true, (ExitCode::from(75), false), false),
        (&["update"], false, ok, false),
        (&["update"], false, nothing_here, false),
        (
            &["update", "claude", "--wait-lock", "1800"],
            true,
            ok,
            false,
        ),
        (&["install", "--help"], true, ok, false),
        (&["install", "clt"], true, ok, false),
        (
            &["install", "claude", atpkg::cli::NO_INIT_FLAG],
            true,
            ok,
            false,
        ),
    ] {
        for setup_code in [
            Some(ExitCode::SUCCESS),
            Some(ExitCode::from(75)),
            Some(ExitCode::from(77)),
            Some(ExitCode::from(69)),
            Some(ExitCode::FAILURE),
            None,
        ] {
            let events = RefCell::new(Vec::new());
            let prepares = person
                && atpkg::cli::pass_finishes_setup(
                    &words.iter().map(OsString::from).collect::<Vec<_>>(),
                );
            let code = super::package_entry_with(
                words.iter().map(OsString::from).collect(),
                person,
                |forwarded| {
                    assert_eq!(
                        forwarded,
                        std::iter::once(OsString::from("pkg"))
                            .chain(words.iter().map(OsString::from))
                            .collect::<Vec<_>>()
                    );
                    events.borrow_mut().push("prepare");
                    None
                },
                |args| {
                    assert_eq!(args, words.iter().map(OsString::from).collect::<Vec<_>>());
                    events.borrow_mut().push("packages");
                    (package_code, settled)
                },
                || {
                    assert_eq!(*events.borrow(), ["prepare", "packages"]);
                    events.borrow_mut().push("setup");
                    setup_code.unwrap_or_else(|| panic!("setup panicked"))
                },
            );
            let want: &[&str] = if follows {
                &["prepare", "packages", "setup"]
            } else if prepares {
                &["prepare", "packages"]
            } else {
                &["packages"]
            };
            assert_eq!(events.borrow().as_slice(), want, "{words:?}");
            assert_eq!(code, package_code, "{words:?} then {setup_code:?}");
        }
    }
    let events = RefCell::new(Vec::new());
    let words = vec![
        OsString::from("update"),
        OsString::from("--aterm-front-door-routing-probe"),
    ];
    let code = super::package_entry_with(
        words.clone(),
        true,
        |forwarded| {
            assert_eq!(
                forwarded,
                [vec![OsString::from("pkg")], words.clone()].concat()
            );
            events.borrow_mut().push("prepare");
            None
        },
        |args| {
            assert_eq!(*events.borrow(), ["prepare"]);
            events.borrow_mut().push("parser");
            let code = atpkg::cli::main_entry(args);
            (code, atpkg::cli::pass_ended_settled(code))
        },
        || panic!("a rejected package operand cannot enter setup"),
    );
    assert_eq!(code, ExitCode::from(2));
    assert_eq!(*events.borrow(), ["prepare", "parser"]);
    let code = super::package_entry_with(
        words,
        true,
        |_| Some(ExitCode::from(2)),
        |_| panic!("the original must not run a handed-off pass again"),
        || panic!("the original must not enter setup after handoff"),
    );
    assert_eq!(code, ExitCode::from(2));
}

/// The follow-up's person ([`super::a_person_at_their_own_account_from`]): typed at a
/// terminal, not root, in their own login's home, and not under CI, whatever `CI` says.
#[test]
fn setup_follows_no_pass_under_ci_root_or_another_home() {
    use std::ffi::OsString;
    fn env(ci: Option<&'static str>) -> impl Fn(&str) -> Option<OsString> {
        move |name| ci.filter(|_| name == "CI").map(OsString::from)
    }
    for (typed, uid, live, ci, person) in [
        (true, 501, true, None, true),
        (true, 501, true, Some("true"), false),
        (true, 501, true, Some(""), false),
        (false, 501, true, None, false),
        (true, 0, true, None, false),
        (true, 501, false, None, false),
    ] {
        assert_eq!(
            super::a_person_at_their_own_account_from(typed, uid, live, env(ci)),
            person,
            "typed {typed}, uid {uid}, live {live}, CI {ci:?}"
        );
    }
}

mod setup_startup {
    use super::super::{PreparedSetup, SetupAdmission, SetupStartup, prepare_setup};
    use aterm_gui::WorkstationSetupDecision::{self, Begin, Unavailable};
    use aterm_types::macos_setup::{Plan, Selection, Setting};
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::rc::Rc;

    fn plan(selection: Selection) -> Plan {
        Plan {
            selection,
            ..Plan::default()
        }
    }

    struct Claim(Rc<Cell<usize>>);

    impl Drop for Claim {
        fn drop(&mut self) {
            self.0.set(self.0.get() - 1);
        }
    }

    #[derive(Default)]
    struct Host {
        revisions: VecDeque<Result<u64, &'static str>>,
        claims: VecDeque<Result<bool, &'static str>>,
        finished: VecDeque<Result<bool, &'static str>>,
        known_admissions: VecDeque<Result<Option<SetupAdmission>, &'static str>>,
        offers: VecDeque<WorkstationSetupDecision>,
        initial: VecDeque<Result<Plan, &'static str>>,
        authorizations: VecDeque<Result<(), &'static str>>,
        admissions: VecDeque<Option<u64>>,
        offered: Vec<Plan>,
        authorized: Vec<Plan>,
        active: Rc<Cell<usize>>,
        held_during_backoff: Vec<usize>,
        failures: Vec<String>,
        bind_calls: usize,
        claim_calls: usize,
        offer_calls: usize,
        invalid_plan: bool,
        pending_authorization: bool,
        terminal_authorization_error: bool,
        /// Whether a person's choice waits for the claim, look by look.
        choice_awaits: VecDeque<bool>,
        /// Waits before a retried write of the page's answer, each holding
        /// the claim.
        retry_waits: usize,
        /// The review ended unbound for a fresh one.
        bound_afresh: bool,
    }

    impl SetupStartup for Host {
        type Claim = Claim;

        fn choice_awaits_claim(&mut self) -> bool {
            self.choice_awaits.pop_front().unwrap_or(false)
        }

        fn bind_authorization(&mut self) -> Result<u64, String> {
            self.bind_calls += 1;
            assert_eq!(self.bind_calls, 1, "capture the startup review once");
            self.revision()
        }

        fn bind_afresh(&mut self) {
            self.bound_afresh = true;
        }

        fn revision(&mut self) -> Result<u64, String> {
            self.revisions
                .pop_front()
                .unwrap_or(Ok(0))
                .map_err(str::to_owned)
        }

        fn claim(&mut self) -> Result<Option<Claim>, String> {
            self.claim_calls += 1;
            assert_eq!(self.active.get(), 0, "never claim over our existing lease");
            self.claims
                .pop_front()
                .expect("unexpected claim attempt")
                .map(|acquired| {
                    acquired.then(|| {
                        self.active.set(1);
                        Claim(Rc::clone(&self.active))
                    })
                })
                .map_err(str::to_owned)
        }

        fn finished(&mut self) -> Result<bool, String> {
            self.finished
                .pop_front()
                .expect("unexpected marker read")
                .map_err(str::to_owned)
        }

        fn admission(&mut self) -> Result<Option<SetupAdmission>, String> {
            assert_eq!(self.active.get(), 1, "admission is read under our claim");
            self.known_admissions
                .pop_front()
                .unwrap_or(Ok(None))
                .map_err(str::to_owned)
        }

        fn initial(&mut self) -> Result<Plan, String> {
            self.initial
                .pop_front()
                .unwrap_or(Ok(Plan::default()))
                .map_err(str::to_owned)
        }

        fn offer(&mut self, initial: Plan) -> WorkstationSetupDecision {
            self.offer_calls += 1;
            self.offered.push(initial);
            assert_eq!(self.active.get(), 1, "offer requires an exclusive claim");
            self.offers.pop_front().expect("unexpected offer")
        }

        fn authorize(&mut self, plan: &Plan) -> Result<Option<u64>, String> {
            assert_eq!(self.active.get(), 1, "authorization keeps the setup claim");
            self.authorized.push(plan.clone());
            let result = self.authorizations.pop_front().unwrap_or(Ok(()));
            self.pending_authorization = result.is_err() && !self.terminal_authorization_error;
            result.map_err(str::to_owned)?;
            Ok(self.admissions.pop_front().unwrap_or(Some(1)))
        }

        fn authorization_pending(&self) -> bool {
            self.pending_authorization
        }

        fn validate(&mut self, _: &Plan) -> Result<(), String> {
            if self.invalid_plan {
                Err("the selected vault URL is invalid".into())
            } else {
                Ok(())
            }
        }

        fn wait_holding_claim(&mut self) -> bool {
            assert_eq!(self.active.get(), 1, "this wait holds the claim");
            self.retry_waits += 1;
            self.choice_awaits_claim()
        }

        fn backoff(&mut self) {
            self.held_during_backoff.push(self.active.get());
        }

        fn report_failure(&mut self, why: &str) {
            self.failures.push(why.to_owned());
        }
    }

    /// The person closes the page that showed the offer (Cmd-W): the page lets it go
    /// unanswered, and the startup, holding nothing while it waits, offers it again; the
    /// page that shows it next, quietly behind the person's tab, answers it at once, so
    /// setup goes on with nothing asked (design §6.1).
    /// NEGATIVE CONTROL: a startup that ends at the first let-go admits nothing here.
    #[test]
    fn an_offer_its_page_let_go_is_offered_again_and_admitted() {
        let mut host = Host {
            claims: [Ok(true), Ok(true)].into(),
            finished: [Ok(false), Ok(false), Ok(false)].into(),
            offers: [Unavailable, Begin(Plan::default())].into(),
            ..Host::default()
        };
        let result = prepare_setup(&mut host).unwrap();
        assert!(matches!(result, PreparedSetup::Authorized { .. }));
        assert_eq!((host.claim_calls, host.offer_calls), (2, 2));
        assert_eq!(
            host.held_during_backoff,
            [0],
            "no claim held between offers"
        );
        assert_eq!(host.authorized, [Plan::default()]);
        assert!(host.failures.is_empty());
        drop(result);
    }

    #[test]
    fn a_window_retries_initial_claim_errors_before_its_first_offer() {
        let mut host = Host {
            claims: [Err("temporary state I/O"), Ok(true)].into(),
            finished: [Ok(false), Ok(false), Ok(false)].into(),
            offers: [Begin(Plan::default())].into(),
            ..Host::default()
        };
        let result = prepare_setup(&mut host).unwrap();
        assert!(matches!(result, PreparedSetup::Authorized { .. }));
        assert_eq!(host.claim_calls, 2);
        assert_eq!(host.offer_calls, 1);
        assert_eq!(host.held_during_backoff, [0]);
        assert_eq!(host.failures, ["temporary state I/O"]);
        assert_eq!(host.active.get(), 1, "worker inherits the live claim");
        drop(result);
        assert_eq!(host.active.get(), 0);
    }

    #[test]
    fn a_choice_made_while_the_plan_is_read_gets_the_claim_before_any_offer() {
        let mut host = Host {
            claims: [Ok(true), Ok(true)].into(),
            finished: [Ok(false), Ok(false)].into(),
            choice_awaits: [true, false].into(),
            offers: [Begin(Plan::default())].into(),
            ..Host::default()
        };
        let result = prepare_setup(&mut host).unwrap();
        assert!(matches!(result, PreparedSetup::Authorized { .. }));
        // It let the claim go before it waited, and offered only once no
        // choice waited for it.
        assert_eq!(host.held_during_backoff, [0]);
        assert_eq!(host.claim_calls, 2);
        assert_eq!(host.offer_calls, 1);
    }

    fn assert_pre_offer_failure_releases_claim(initial_read: bool) {
        let selected = plan(Selection::from_bits(1).unwrap());
        let mut host = Host {
            claims: [Ok(true), Ok(true)].into(),
            finished: if initial_read {
                [Ok(false), Ok(false), Ok(false)].into()
            } else {
                [
                    Err("pre-offer completion read failed"),
                    Ok(false),
                    Ok(false),
                ]
                .into()
            },
            initial: if initial_read {
                [Err("pre-offer plan read failed"), Ok(selected.clone())].into()
            } else {
                [Ok(selected.clone())].into()
            },
            offers: [Begin(selected.clone())].into(),
            ..Host::default()
        };
        let result = prepare_setup(&mut host).unwrap();
        assert!(matches!(result, PreparedSetup::Authorized { .. }));
        assert_eq!(
            host.held_during_backoff,
            [0],
            "before a decision, manual repair must be able to claim setup"
        );
        assert_eq!(host.claim_calls, 2, "reload only under a fresh claim");
        assert_eq!(host.offered.as_slice(), std::slice::from_ref(&selected));
        assert_eq!(host.authorized, [selected]);
        assert_eq!(host.active.get(), 1, "the admitted worker keeps its claim");
        drop(result);
        assert_eq!(host.active.get(), 0);
    }

    #[test]
    fn pre_offer_failure_releases_claim_after_a_plan_read_error() {
        assert_pre_offer_failure_releases_claim(true);
    }

    #[test]
    fn pre_offer_failure_releases_claim_after_a_completion_read_error() {
        assert_pre_offer_failure_releases_claim(false);
    }

    #[test]
    fn a_new_manual_run_during_startup_backoff_prevents_another_offer() {
        for unavailable_offer in [false, true] {
            let mut host = Host {
                revisions: [Ok(7), Ok(7), Ok(8)].into(),
                claims: [Ok(true), Ok(true)].into(),
                finished: [Ok(false), Ok(false)].into(),
                initial: if unavailable_offer {
                    [Ok(Plan::default())].into()
                } else {
                    [Err("initial source unreadable")].into()
                },
                offers: if unavailable_offer {
                    [Unavailable].into()
                } else {
                    VecDeque::new()
                },
                ..Host::default()
            };
            assert!(matches!(
                prepare_setup(&mut host).unwrap(),
                PreparedSetup::Unavailable
            ));
            assert_eq!(host.held_during_backoff, [0]);
            assert_eq!(host.claim_calls, 2);
            assert_eq!(host.offer_calls, usize::from(unavailable_offer));
            assert!(
                host.authorized.is_empty(),
                "never replace the manual worker's password"
            );
            assert!(host.revisions.is_empty());
            assert_eq!(host.active.get(), 0);
        }
    }

    /// A startup that resumes a run an earlier process admitted (Quit & Reopen's relaunch
    /// mid-setup) offers nothing, for review or otherwise, and writes no admission: the run
    /// goes on under its own revision, with aterm's own permissions in its plan or not.
    #[test]
    fn an_admitted_pending_run_resumes_without_another_offer() {
        let mut selected = Plan {
            selection: Selection::from_bits(1 << 10).unwrap(),
            vault_url: "http://saved.local/v1/team".into(),
        };
        selected.selection.set(Setting::Permissions, true);
        let mut unpermitted = selected.clone();
        unpermitted.selection.set(Setting::Permissions, false);
        for plan in [selected.clone(), unpermitted] {
            let admitted = SetupAdmission {
                plan: plan.clone(),
                revision: 7,
                completed: false,
            };
            let mut host = Host {
                revisions: [Ok(7), Ok(7)].into(),
                claims: [Ok(true)].into(),
                finished: [Ok(false), Ok(false)].into(),
                known_admissions: [Ok(Some(admitted))].into(),
                initial: [Ok(plan.clone())].into(),
                offers: [Begin(plan.clone())].into(),
                ..Host::default()
            };
            let result = prepare_setup(&mut host).unwrap();
            assert_eq!(
                host.offer_calls, 0,
                "an admitted choice needs no second review"
            );
            assert!(
                host.authorized.is_empty(),
                "keep the live worker's admitted revision"
            );
            assert!(matches!(
                result,
                PreparedSetup::Authorized { revision: 7, .. }
            ));
            assert_eq!(host.initial, [Ok(plan)]);
            assert_eq!(host.active.get(), 1);
            drop(result);
            assert_eq!(host.active.get(), 0);
        }
    }

    #[test]
    fn startup_admission_read_errors_never_fall_through_to_an_offer() {
        let mut host = Host {
            revisions: [Ok(7), Ok(7), Ok(7)].into(),
            claims: [Ok(true), Ok(true)].into(),
            finished: [Ok(false), Ok(false)].into(),
            known_admissions: [
                Err("admission snapshot changed"),
                Ok(Some(SetupAdmission {
                    plan: Plan::default(),
                    revision: 7,
                    completed: false,
                })),
            ]
            .into(),
            ..Host::default()
        };
        let result = prepare_setup(&mut host).unwrap();
        assert!(matches!(
            result,
            PreparedSetup::Authorized { revision: 7, .. }
        ));
        assert_eq!(host.claim_calls, 2);
        assert_eq!(host.held_during_backoff, [0]);
        drop(result);
        assert_eq!(host.offer_calls, 0);
        assert!(host.authorized.is_empty());
        assert_eq!(host.active.get(), 0);
    }

    #[test]
    fn startup_admission_confirmation_cannot_adopt_a_newer_run() {
        for (revision, completed) in [(8, false), (8, true), (7, true)] {
            let mut host = Host {
                revisions: [Ok(7), Ok(7)].into(),
                claims: [Ok(true)].into(),
                finished: [Ok(false)].into(),
                known_admissions: [Ok(Some(SetupAdmission {
                    plan: Plan::default(),
                    revision,
                    completed,
                }))]
                .into(),
                ..Host::default()
            };
            let result = prepare_setup(&mut host).unwrap();
            if revision == 7 {
                assert!(matches!(result, PreparedSetup::Finished));
            } else {
                assert!(matches!(result, PreparedSetup::Unavailable));
            }
            assert_eq!(host.offer_calls, 0);
            assert!(host.authorized.is_empty());
            assert_eq!(host.active.get(), 0);
        }
    }

    struct AuthorizationTicket {
        publication: u64,
        target: Option<u64>,
        pending: bool,
    }

    impl AuthorizationTicket {
        fn bind(publication: u64) -> Self {
            Self {
                publication,
                target: None,
                pending: true,
            }
        }
    }

    #[derive(Default)]
    struct ReviewState {
        publication: u64,
        revision: u64,
        finished: bool,
        binds: usize,
        claims: usize,
        offers: usize,
        writes: usize,
        accepted: usize,
        backoffs: usize,
        failures: usize,
    }

    struct ReviewHost {
        state: Rc<RefCell<ReviewState>>,
        active: Rc<Cell<usize>>,
        authorization: Option<AuthorizationTicket>,
        selected: Plan,
        replace_during_offer: bool,
        fail_publication: bool,
        fail_confirmations: usize,
    }

    impl SetupStartup for ReviewHost {
        type Claim = Claim;

        fn bind_authorization(&mut self) -> Result<u64, String> {
            let mut state = self.state.borrow_mut();
            state.binds += 1;
            assert_eq!(state.binds, 1);
            self.authorization = Some(AuthorizationTicket::bind(state.publication));
            Ok(state.revision)
        }

        fn revision(&mut self) -> Result<u64, String> {
            Ok(self.state.borrow().revision)
        }

        fn claim(&mut self) -> Result<Option<Self::Claim>, String> {
            assert_eq!(self.active.get(), 0);
            self.active.set(1);
            self.state.borrow_mut().claims += 1;
            Ok(Some(Claim(Rc::clone(&self.active))))
        }

        fn finished(&mut self) -> Result<bool, String> {
            assert_eq!(
                self.state.borrow().offers,
                0,
                "no Plan-only completion shortcut"
            );
            Ok(self.state.borrow().finished)
        }

        fn admission(&mut self) -> Result<Option<SetupAdmission>, String> {
            assert_eq!(self.state.borrow().offers, 0);
            Ok(None)
        }

        fn initial(&mut self) -> Result<Plan, String> {
            Ok(self.selected.clone())
        }

        fn offer(&mut self, selected: Plan) -> WorkstationSetupDecision {
            let mut state = self.state.borrow_mut();
            state.offers += 1;
            if self.replace_during_offer {
                state.revision += 1;
                state.publication += 1;
            }
            Begin(selected)
        }

        fn authorize(&mut self, selected: &Plan) -> Result<Option<u64>, String> {
            assert_eq!(selected, &self.selected);
            assert_eq!(self.active.get(), 1);
            let ticket = self.authorization.as_mut().expect("carry the bound review");
            let mut state = self.state.borrow_mut();
            if ticket.publication != state.publication {
                ticket.pending = false;
                return Ok(None);
            }
            if ticket.target.is_none() {
                if state.finished {
                    ticket.pending = false;
                    return Ok(None);
                }
                state.revision += 1;
                state.publication += 1;
                state.writes += 1;
                ticket.publication = state.publication;
                ticket.target = Some(state.revision);
                state.finished = selected.selection.is_empty();
                if std::mem::take(&mut self.fail_publication) {
                    return Err("owned admission published before its flush failed".into());
                }
            } else if self.fail_confirmations > 0 {
                self.fail_confirmations -= 1;
                return Err("owned admission observation is not durable yet".into());
            }
            ticket.pending = false;
            state.accepted += 1;
            Ok(ticket.target)
        }

        fn authorization_pending(&self) -> bool {
            self.authorization
                .as_ref()
                .is_some_and(|ticket| ticket.pending)
        }

        fn wait_holding_claim(&mut self) -> bool {
            assert_eq!(self.active.get(), 1);
            let mut state = self.state.borrow_mut();
            assert_eq!(state.accepted, 0);
            state.backoffs += 1;
            false
        }

        fn backoff(&mut self) {
            unreachable!("no choice waits for this claim");
        }

        fn report_failure(&mut self, _: &str) {
            self.state.borrow_mut().failures += 1;
        }
    }

    fn review_host(selected: Plan) -> ReviewHost {
        ReviewHost {
            state: Rc::new(RefCell::new(ReviewState::default())),
            active: Rc::new(Cell::new(0)),
            authorization: None,
            selected,
            replace_during_offer: false,
            fail_publication: false,
            fail_confirmations: 0,
        }
    }

    #[test]
    fn a_changed_offer_is_rejected_without_acknowledging_or_reauthorizing_it() {
        for changed in [false, true] {
            let mut host = review_host(Plan::default());
            host.replace_during_offer = changed;
            let prepared = prepare_setup(&mut host).unwrap();
            assert_eq!(matches!(prepared, PreparedSetup::Unavailable), changed);
            assert_eq!(
                matches!(prepared, PreparedSetup::Authorized { .. }),
                !changed
            );
            let state = host.state.borrow();
            assert_eq!(state.binds, 1);
            assert_eq!(state.claims, 1);
            assert_eq!(state.offers, 1);
            assert_eq!(state.writes, usize::from(!changed));
            assert_eq!(state.accepted, usize::from(!changed));
            assert_eq!(state.backoffs, 0);
            drop(state);
            drop(prepared);
            assert_eq!(host.active.get(), 0);
        }
    }

    #[test]
    fn an_owned_empty_admission_is_confirmed_without_a_later_completion_shortcut() {
        let mut host = review_host(plan(Selection::none()));
        host.fail_publication = true;
        host.fail_confirmations = 1;
        assert!(matches!(
            prepare_setup(&mut host).unwrap(),
            PreparedSetup::Finished
        ));
        let state = host.state.borrow();
        assert_eq!(state.binds, 1);
        assert_eq!(state.claims, 1);
        assert_eq!(state.offers, 1);
        assert_eq!(state.writes, 1);
        assert_eq!(state.revision, 1);
        assert_eq!(state.accepted, 1);
        assert_eq!(state.backoffs, 2);
        assert_eq!(state.failures, 2);
        assert_eq!(host.active.get(), 0);
    }

    #[test]
    fn an_unadmitted_pending_run_remains_reviewable_through_same_revision_recovery() {
        let selected = plan(Selection::from_bits(1).unwrap());
        for recover_read in [false, true] {
            let mut host = Host {
                revisions: [Ok(7), Ok(7), Ok(7)].into(),
                claims: [Ok(true), Ok(true)].into(),
                finished: [Ok(false), Ok(false), Ok(false)].into(),
                initial: if recover_read {
                    [Err("temporary source read"), Ok(selected.clone())].into()
                } else {
                    [Ok(selected.clone())].into()
                },
                offers: [Begin(selected.clone())].into(),
                admissions: [Some(8)].into(),
                ..Host::default()
            };
            let result = prepare_setup(&mut host).unwrap();
            assert!(matches!(
                result,
                PreparedSetup::Authorized { revision: 8, .. }
            ));
            assert_eq!(host.offered.as_slice(), std::slice::from_ref(&selected));
            assert_eq!(host.authorized.as_slice(), std::slice::from_ref(&selected));
            assert_eq!(host.claim_calls, if recover_read { 2 } else { 1 });
            assert_eq!(
                host.held_during_backoff,
                if recover_read { vec![0] } else { Vec::new() }
            );
            drop(result);
            assert_eq!(host.active.get(), 0);
        }
    }

    /// A capture that failed ends this review unbound: it never adopts the revision a manual
    /// repair wrote meanwhile, and after a pause holding nothing hands on to a fresh review.
    #[test]
    fn an_unreadable_startup_identity_cannot_be_adopted_after_a_manual_repair() {
        let mut host = Host {
            revisions: [Err("startup identity unreadable"), Ok(8)].into(),
            ..Host::default()
        };
        assert!(matches!(
            prepare_setup(&mut host),
            Ok(PreparedSetup::Unavailable)
        ));
        assert_eq!(host.revisions, [Ok(8)]);
        assert_eq!(host.claim_calls, 0);
        assert_eq!(host.offer_calls, 0);
        assert_eq!(host.held_during_backoff, [0]);
        assert_eq!(host.failures.len(), 1);
        assert!(host.bound_afresh, "a fresh review binds the records");
    }

    #[test]
    fn authorization_fence_returns_invalid_review_without_retry_or_authorization() {
        let mut host = Host {
            claims: [Ok(true)].into(),
            finished: [Ok(false), Ok(false), Ok(false)].into(),
            offers: [Begin(Plan::default())].into(),
            invalid_plan: true,
            ..Host::default()
        };
        assert!(matches!(
            prepare_setup(&mut host).unwrap(),
            PreparedSetup::Unavailable
        ));
        assert!(host.authorized.is_empty());
        assert!(host.held_during_backoff.is_empty());
        assert_eq!(host.active.get(), 0);
        assert_eq!(host.failures, ["the selected vault URL is invalid"]);
    }

    #[test]
    fn authorization_fence_hands_off_the_committed_revision_before_any_later_read() {
        let mut host = Host {
            claims: [Ok(true)].into(),
            finished: [Ok(false), Err("completion read failed"), Ok(false)].into(),
            offers: [Begin(Plan::default())].into(),
            admissions: [Some(7)].into(),
            ..Host::default()
        };
        let prepared = prepare_setup(&mut host).unwrap();
        assert!(matches!(
            prepared,
            PreparedSetup::Authorized { revision: 7, .. }
        ));
        assert_eq!(host.authorized, [Plan::default()]);
        assert!(host.held_during_backoff.is_empty());
        assert!(host.failures.is_empty());
        assert_eq!(host.finished, [Err("completion read failed"), Ok(false)]);
        drop(prepared);
        assert_eq!(host.active.get(), 0);
    }

    #[test]
    fn a_waiting_window_retries_a_failed_marker_read_then_takes_over() {
        let mut host = Host {
            claims: [Ok(false), Ok(true)].into(),
            finished: [
                Err("temporary marker read"),
                Ok(false),
                Ok(false),
                Ok(false),
                Ok(false),
            ]
            .into(),
            offers: [Begin(Plan::default())].into(),
            ..Host::default()
        };
        assert!(matches!(
            prepare_setup(&mut host).unwrap(),
            PreparedSetup::Authorized { .. }
        ));
        assert_eq!(host.claim_calls, 2);
        assert_eq!(host.offer_calls, 1);
        assert_eq!(host.held_during_backoff, [0, 0]);
        assert_eq!(host.failures, ["temporary marker read"]);
    }

    #[test]
    fn begin_keeps_its_claim_and_decision_while_its_owned_write_recovers() {
        let mut host = Host {
            claims: [Ok(true)].into(),
            finished: [Ok(false)].into(),
            offers: [Begin(Plan::default())].into(),
            authorizations: [Err("owned authorization write failed"), Ok(())].into(),
            ..Host::default()
        };
        assert!(matches!(
            prepare_setup(&mut host).unwrap(),
            PreparedSetup::Authorized { .. }
        ));
        assert_eq!(host.offer_calls, 1, "never repeat an accepted offer");
        assert_eq!(host.claim_calls, 1);
        // It holds the claim through one wait, which a person's choice
        // would end at once (no choice came), and through no other.
        assert_eq!(host.retry_waits, 1);
        assert!(host.held_during_backoff.is_empty());
        assert_eq!(host.failures, ["owned authorization write failed"]);
    }

    #[test]
    fn a_choice_made_while_the_answer_is_retried_ends_the_wait_and_gets_the_claim() {
        // The answer's write failed and waits to retry; a choice made meanwhile ends the
        // wait and holds the claim until the answer takes it again for its next try.
        let mut host = Host {
            claims: [Ok(true), Ok(true)].into(),
            finished: [Ok(false)].into(),
            offers: [Begin(Plan::default())].into(),
            authorizations: [Err("owned authorization write failed"), Ok(())].into(),
            choice_awaits: [false, true].into(),
            ..Host::default()
        };
        let result = prepare_setup(&mut host).unwrap();
        assert!(matches!(result, PreparedSetup::Authorized { .. }));
        assert_eq!(host.retry_waits, 1);
        assert_eq!(host.held_during_backoff, [0]);
        assert_eq!(host.claim_calls, 2);
        assert_eq!(host.authorized.len(), 2);
        assert_eq!(
            host.active.get(),
            1,
            "the run inherits the claim taken again"
        );
        drop(result);
        // The choice's run holds the claim and finishes setup: the
        // answer is not written again.
        let mut host = Host {
            claims: [Ok(true), Ok(false)].into(),
            finished: [Ok(false), Ok(true)].into(),
            offers: [Begin(Plan::default())].into(),
            authorizations: [Err("owned authorization write failed")].into(),
            choice_awaits: [false, true].into(),
            ..Host::default()
        };
        assert!(matches!(
            prepare_setup(&mut host).unwrap(),
            PreparedSetup::Finished
        ));
        assert_eq!(host.authorized.len(), 1);
        assert_eq!(host.active.get(), 0);
    }

    #[test]
    fn an_unbound_authorization_failure_releases_the_choice_without_retry() {
        let mut host = Host {
            claims: [Ok(true)].into(),
            finished: [Ok(false)].into(),
            offers: [Begin(Plan::default())].into(),
            authorizations: [Err("the failed publication could not be pinned")].into(),
            terminal_authorization_error: true,
            ..Host::default()
        };
        assert!(matches!(
            prepare_setup(&mut host).unwrap(),
            PreparedSetup::Unavailable
        ));
        assert_eq!(host.claim_calls, 1);
        assert_eq!(host.offer_calls, 1);
        assert_eq!(host.authorized, [Plan::default()]);
        assert_eq!(
            host.failures,
            ["the failed publication could not be pinned"]
        );
        assert!(host.held_during_backoff.is_empty());
        assert_eq!(host.active.get(), 0);
    }

    #[test]
    fn an_unavailable_window_releases_its_claim_before_waiting_and_reoffering() {
        let mut host = Host {
            claims: [Ok(true), Ok(true)].into(),
            finished: [Ok(false), Ok(false), Ok(false), Ok(false), Ok(false)].into(),
            offers: [Unavailable, Begin(Plan::default())].into(),
            ..Host::default()
        };
        assert!(matches!(
            prepare_setup(&mut host).unwrap(),
            PreparedSetup::Authorized { .. }
        ));
        assert_eq!(host.claim_calls, 2);
        assert_eq!(host.offer_calls, 2);
        assert_eq!(host.held_during_backoff, [0]);
        assert!(host.failures.is_empty());
    }

    #[test]
    fn a_finished_setup_stops_later_offers() {
        for mut host in [
            Host {
                claims: [Ok(false)].into(),
                finished: [Ok(true)].into(),
                ..Host::default()
            },
            Host {
                claims: [Ok(true)].into(),
                finished: [Ok(true)].into(),
                ..Host::default()
            },
            Host {
                claims: [Ok(true), Ok(false)].into(),
                finished: [Ok(false), Ok(true)].into(),
                offers: [Unavailable].into(),
                ..Host::default()
            },
        ] {
            assert!(matches!(
                prepare_setup(&mut host).unwrap(),
                PreparedSetup::Finished
            ));
            assert!(
                host.offers.is_empty(),
                "no duplicate offer after completion"
            );
            assert_eq!(host.active.get(), 0);
        }
    }

    #[test]
    fn checklist_choice_is_loaded_once_and_persisted_before_engine_authorization() {
        let saved = plan(Selection::from_bits(1).unwrap());
        let selected = plan(Selection::from_bits(2).unwrap());
        let mut host = Host {
            claims: [Ok(true), Ok(true)].into(),
            finished: [Ok(false), Ok(false), Ok(false), Ok(false)].into(),
            initial: [Err("selection read failed"), Ok(saved.clone())].into(),
            offers: [Begin(selected.clone())].into(),
            authorizations: [Err("authorization write failed"), Ok(())].into(),
            ..Host::default()
        };
        let result = prepare_setup(&mut host).unwrap();
        let PreparedSetup::Authorized { claim, .. } = &result else {
            panic!("the worker must inherit the authorized run")
        };
        assert!(
            Rc::ptr_eq(&claim.0, &host.active),
            "the worker inherits this setup's live lease"
        );
        assert_eq!(host.offered, [saved]);
        assert_eq!(host.authorized, [selected.clone(), selected]);
        assert_eq!(host.claim_calls, 2);
        assert_eq!(host.held_during_backoff, [0]);
        assert_eq!(host.retry_waits, 1);
        drop(result);
        assert_eq!(host.active.get(), 0);
    }

    #[test]
    fn an_empty_checklist_finishes_without_entering_the_engine() {
        let mut host = Host {
            claims: [Ok(true)].into(),
            finished: [Ok(false), Ok(false), Ok(true)].into(),
            offers: [Begin(plan(Selection::none()))].into(),
            ..Host::default()
        };
        assert!(matches!(
            prepare_setup(&mut host).unwrap(),
            PreparedSetup::Finished
        ));
        assert_eq!(host.authorized, [plan(Selection::none())]);
        assert_eq!(host.active.get(), 0);
    }
}

#[test]
fn setup_windows_keep_scope_ssh_and_command_guards() {
    let window = [OsString::from("--window")];
    assert!(first_run_window_eligible(
        &window,
        DoctorStartupEnv::default()
    ));
    assert!(
        first_run_window_eligible(&[], DoctorStartupEnv::default()),
        "Dock and updater launches have a GUI offer surface"
    );
    for env in [
        DoctorStartupEnv {
            isolated: true,
            ..DoctorStartupEnv::default()
        },
        DoctorStartupEnv {
            outside_person_scope: true,
            ..DoctorStartupEnv::default()
        },
        // A window started over ssh leaves setup to one started at this Mac.
        DoctorStartupEnv {
            remote: true,
            ..DoctorStartupEnv::default()
        },
    ] {
        assert!(!first_run_window_eligible(&window, env));
    }
    for args in [
        vec![
            OsString::from("--control-sock"),
            OsString::from("/tmp/private"),
        ],
        vec![OsString::from("-e"), OsString::from("zsh")],
        vec![OsString::from("--headless")],
    ] {
        assert!(!first_run_window_eligible(
            &args,
            DoctorStartupEnv::default()
        ));
    }
}

#[test]
fn setup_relaunch_hint_preserves_private_flags_before_its_carried_command() {
    let osv = |list: &[&str]| -> Vec<OsString> { list.iter().map(OsString::from).collect() };
    for args in [
        osv(&["--control-sock", "/tmp/test.sock"]),
        osv(&["--control-sock", "/tmp/test.sock", "-e", "zsh"]),
        osv(&["--headless", "-e", "zsh"]),
    ] {
        assert!(
            !first_run_window_eligible(
                first_run_window_args(&args, true),
                DoctorStartupEnv::default(),
            ),
            "a relaunch hint must preserve {args:?}",
        );
    }
    let ordinary = osv(&["--window", "-d", "/tmp"]);
    assert!(first_run_window_eligible(
        first_run_window_args(&ordinary, true),
        DoctorStartupEnv::default(),
    ));
    let carried = osv(&["--window", "-d", "/tmp", "-e", "zsh", "--control-sock"]);
    assert_eq!(first_run_window_args(&carried, true), ordinary.as_slice());
    assert!(!first_run_window_eligible(
        first_run_window_args(&carried, false),
        DoctorStartupEnv::default(),
    ));
}

#[test]
fn ordinary_window_options_keep_the_unified_setup_offer() {
    let osv = |list: &[&str]| -> Vec<OsString> { list.iter().map(OsString::from).collect() };
    for args in [
        osv(&["--gpu"]),
        osv(&["--cpu"]),
        osv(&["--font-px", "24", "--font", "Menlo", "--scale", "2"]),
        osv(&["--columns", "120", "--lines", "40", "-d", "/tmp"]),
        osv(&[
            "--shell",
            "zsh",
            "--shell-integration",
            "--no-shell-integration",
        ]),
    ] {
        assert!(
            first_run_window_eligible(&args, DoctorStartupEnv::default()),
            "ordinary window options must retain the setup surface: {args:?}",
        );
    }
    for args in [
        osv(&["--gpu", "-e", "zsh"]),
        osv(&["--cpu", "--headless"]),
        osv(&["--font", "Menlo", "--control-sock", "/tmp/private"]),
        osv(&["--columns", "120", "--diagnose"]),
        osv(&["--gpu", "--containment", "user"]),
        osv(&["--gpu", "--bogus"]),
        osv(&["--font-px"]),
    ] {
        assert!(
            !first_run_window_eligible(&args, DoctorStartupEnv::default()),
            "ordinary options cannot erase a scope/command refusal: {args:?}",
        );
    }
}

#[test]
fn an_update_successor_of_a_shell_started_window_is_still_a_window() {
    let osv = |list: &[&str]| -> Vec<OsString> { list.iter().map(OsString::from).collect() };
    for argv in [
        osv(&["--window"]),
        osv(&[]),
        osv(&["--window", "--window", "-d", "/tmp"]),
    ] {
        let successor = aterm_update::window_successor_args(argv.clone().into_iter());
        assert!(
            is_windowish(&successor, false, true),
            "{argv:?} -> {successor:?} must route to the window with a TTY on stdin"
        );
    }
    assert!(!is_windowish(
        &aterm_update::reexec_forwarded_args(osv(&["--window"]).into_iter()),
        false,
        true
    ));
    for argv in [
        osv(&["new-window"]),
        osv(&["new-window", "-d", "/tmp"]),
        osv(&["new-tab"]),
        osv(&["split-pane", "-H"]),
    ] {
        let running = aterm_update::running_window_successor_args(argv.clone().into_iter(), None);
        let mode_args = strip_mode_flags(&running);
        assert!(
            is_windowish(&running, false, true)
                && mode_args.first().is_none_or(|first| {
                    aterm_cli::Verb::from_operand(&first.to_string_lossy()).is_none()
                }),
            "{argv:?} -> {running:?}: the window parser gets {mode_args:?}"
        );
        let boot = aterm_update::window_successor_args(argv.clone().into_iter());
        assert!(
            boot.first()
                .is_some_and(
                    |first| aterm_cli::Verb::from_operand(&first.to_string_lossy()).is_some()
                ),
            "{argv:?} -> {boot:?}: the boot swap's re-exec is routed as the verb"
        );
    }
    assert!(is_windowish(&osv(&[]), false, false), "no TTY: the window");
    assert!(!is_windowish(&osv(&[]), false, true), "a TTY: the session");
    assert!(
        !is_windowish(&osv(&["--window"]), true, false),
        "--session wins"
    );
}

use super::*;

/// Windows has no updater, so `aterm update status` and `check` land on this line: the
/// state, then the one lane that updates a Windows copy (the words `aterm help update`
/// prints), never an app bundle.
#[cfg(windows)]
#[test]
fn windows_update_line_names_the_lane_that_updates_it() {
    let line = update_unreadable_line("0.94.0");
    assert!(line.starts_with("aterm 0.94.0 "), "{line}");
    assert!(line.contains(aterm_cli::WINDOWS_UPDATE_LANE), "{line}");
    assert!(!line.contains("aterm.app"), "{line}");
    assert!(!line.contains("macOS"), "{line}");
}

fn flapped_cast() -> String {
    let mut frame = String::from("\\u001b[?1049h\\u001b[2J");
    for r in 0..8 {
        frame.push_str(&format!("\\u001b[{};1HA{r}", r + 1));
    }
    frame.push_str("\\u001b[6;3H");
    format!(
        "{{\"version\": 2, \"width\": 20, \"height\": 8}}\n\
         [0.100000, \"o\", \"{frame}\"]\n\
         [1.000000, \"r\", \"20x7\"]\n\
         [1.000300, \"o\", \"\\u001b[?1000h\"]\n\
         [1.000700, \"r\", \"20x8\"]\n\
         [1.005000, \"o\", \"\\u001b[3;1H\\u001b[KB2\\u001b[6;3H\"]\n"
    )
}

#[test]
fn ctl_cast_drift_computes_the_report_client_side() {
    let cast = flapped_cast();
    let screen: Vec<String> = ["A1", "A2", "B2", "A4", "A5", "A6", "A7", ""]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    let job = aterm_ctl::CastDriftJob {
        cast: &cast,
        screen: Some(&screen),
        cursor: Some((5, 2)),
        alt: Some(true),
        args: "",
        cut: aterm_ctl::DriftCut::Quiescent,
        until: None,
        peer: aterm_ctl::DriftPeer::PreUndo,
    };
    let report = ctl_cast_drift(&job).expect("a report");
    let head = report.lines().next().unwrap();
    for key in [
        "verdict=desync",
        "fidelity=pass",
        "culprit=1.000000",
        "cut=quiescent",
        "dropped=-",
        "undo=off",
        "computed=client",
    ] {
        assert!(head.split_whitespace().any(|t| t == key), "{key} in {head}");
    }
    assert!(report.contains("\nrow 7 live= expected=A7\n"), "{report}");
    let offline = ctl_cast_drift(&aterm_ctl::CastDriftJob {
        screen: None,
        cursor: None,
        alt: None,
        cut: aterm_ctl::DriftCut::File,
        peer: aterm_ctl::DriftPeer::Unknown,
        ..job
    })
    .expect("a report");
    assert!(
        offline.starts_with("OK 9 verdict=desync fidelity=- against=replay"),
        "{offline}"
    );
    let early = ctl_cast_drift(&aterm_ctl::CastDriftJob {
        screen: None,
        cut: aterm_ctl::DriftCut::File,
        until: Some(0.5),
        peer: aterm_ctl::DriftPeer::Unknown,
        ..job
    })
    .expect("a report");
    assert!(early.starts_with("OK 0 verdict=clean"), "{early}");
    let bad = ctl_cast_drift(&aterm_ctl::CastDriftJob {
        args: "count=2",
        ..job
    })
    .expect_err("usage");
    assert!(bad.starts_with("ERR usage: cast drift"), "{bad}");
    let broken = ctl_cast_drift(&aterm_ctl::CastDriftJob {
        cast: "not a cast",
        ..job
    })
    .expect_err("unparseable");
    assert!(broken.contains("line 1"), "{broken}");
}

#[test]
fn a_quiet_flap_on_an_aterm_before_the_undo_is_named_client_side() {
    let mut frame = String::from("\\u001b[?1049h\\u001b[2J");
    for r in 0..8 {
        frame.push_str(&format!("\\u001b[{};1HL{r}", r + 1));
    }
    frame.push_str("\\u001b[6;3H");
    let cast = format!(
        "{{\"version\": 2, \"width\": 20, \"height\": 8}}\n\
         [0.100000, \"o\", \"{frame}\"]\n\
         [1.000000, \"r\", \"20x7\"]\n\
         [1.000700, \"r\", \"20x8\"]\n\
         [1.005000, \"o\", \"\\u001b[3;1H\\u001b[KB2\\u001b[6;3H\"]\n"
    );
    let old: Vec<String> = ["L1", "L2", "B2", "L4", "L5", "L6", "L7", ""]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    let job = aterm_ctl::CastDriftJob {
        cast: &cast,
        screen: Some(&old),
        cursor: Some((5, 2)),
        alt: Some(true),
        args: "",
        cut: aterm_ctl::DriftCut::Quiescent,
        until: None,
        peer: aterm_ctl::DriftPeer::PreUndo,
    };
    for peer in [aterm_ctl::DriftPeer::PreUndo, aterm_ctl::DriftPeer::Unknown] {
        let report = ctl_cast_drift(&aterm_ctl::CastDriftJob { peer, ..job }).expect("a report");
        let head = report.lines().next().unwrap();
        for key in [
            "verdict=desync",
            "fidelity=pass",
            "culprit=1.000000",
            "undo=off",
            "computed=client",
        ] {
            assert!(
                head.split_whitespace().any(|t| t == key),
                "{peer:?}: {key} in {head}"
            );
        }
        assert!(report.contains("\nrow 0 live=L1 expected=L0\n"), "{report}");
    }
    let new: Vec<String> = ["L0", "L1", "B2", "L3", "L4", "L5", "L6", "L7"]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    let report = ctl_cast_drift(&aterm_ctl::CastDriftJob {
        screen: Some(&new),
        cut: aterm_ctl::DriftCut::File,
        peer: aterm_ctl::DriftPeer::Unknown,
        ..job
    })
    .expect("a report");
    let head = report.lines().next().unwrap();
    assert!(head.contains(" verdict=clean fidelity=pass "), "{head}");
    assert!(!head.contains(" undo=off"), "{head}");
}

fn update_status() -> aterm_update::UpdateStatus {
    aterm_update::UpdateStatus {
        linux: None,
        enabled: true,
        installable: true,
        current_build: 100,
        staged_build: None,
        staged_version: None,
        staged_commit: None,
        staged_dmg_sha256: None,
        changelog: None,
        outcome: "up to date (latest release build 100) \u{b7} checks every 30 min".into(),
        updated_at: "2026-09-23T12:00:00Z".into(),
        failing_checks: 0,
        failing_kind: String::new(),
        failing_applies: 0,
        failing_since: String::new(),
        failing_persistent: false,
        failing_checks_kind: String::new(),
        channel_unreadable: false,
        hold: None,
        start_failure: None,
        last: None,
        offered: Some(100),
        behind: false,
    }
}

#[test]
fn a_missing_ledger_fails_check_everywhere_and_status_only_on_a_mac() {
    assert!(
        no_ledger_fails(true),
        "check cannot do its job without a ledger"
    );
    assert_eq!(no_ledger_fails(false), cfg!(target_os = "macos"));
}

#[test]
fn the_update_line_claims_up_to_date_only_over_a_completed_check() {
    let checked = aterm_update_core::pkg_check::rfc3339_to_unix("2026-09-23T12:00:00Z").unwrap();
    let now = checked + 12 * 60;
    let say = |st: &aterm_update::UpdateStatus, lane: Option<&aterm_update::ApplyLaneReport>| {
        update_summary("0.91.0", 100, st, lane, now, true, true, &DevCopy::No, "").0
    };
    let up_to_date = |line: &str| line.contains("is up to date");
    assert!(up_to_date(&say(&update_status(), None)));
    let mut never = update_status();
    never.updated_at = String::new();
    assert!(!up_to_date(&say(&never, None)), "never checked");

    let mut waiting = update_status();
    waiting.linux = Some(aterm_update::LinuxUpdateStatus {
        installed_build: 101,
        installed_version: "0.92.0".into(),
        staged_build: None,
        staged_version: None,
        staged_commit: None,
        trial_phase: Some("Installed".into()),
        trial_starts: 0,
        trial_healthy: false,
    });
    assert!(!up_to_date(&say(&waiting, None)), "an unfinished install");
    waiting.linux.as_mut().expect("linux").trial_healthy = true;
    assert!(up_to_date(&say(&waiting, None)), "a confirmed install");
    waiting.linux.as_mut().expect("linux").trial_phase = None;
    waiting.hold = Some(held_view(checked, now - 60, false));
    assert!(
        !up_to_date(&say(&waiting, None)),
        "a rolled-back newer build"
    );

    let mut staged = update_status();
    staged.staged_build = Some(101);
    staged.staged_version = Some("0.92.0".into());
    staged.failing_applies = 3;
    let tried = |target: u64, failures: u32| aterm_update::ApplyLaneReport {
        last_failure_target_build: target,
        failures_for_target: failures,
        ..Default::default()
    };
    let failed = |line: &str| line.contains("didn\u{2019}t install");
    assert!(
        failed(&say(&staged, Some(&tried(101, 2)))),
        "this build's failures"
    );
    for lane in [tried(99, 3), tried(0, 0)] {
        assert!(!failed(&say(&staged, Some(&lane))), "{lane:?}");
    }
}

/// ONE DECISION (ATERM-KI-1111-23): the line `aterm update status` prints and the standing a
/// fleet survey says of this copy come from one decision (`UpdateStatus::arm`), so they never
/// disagree: over a status for every arm, on a Mac and on Linux, each line carries its standing's
/// words (held ⇔ "is held for", current ⇔ "is up to date", refused ⇔ "install was refused",
/// installed ⇔ "is installed", …), and no other standing's.
/// NEGATIVE CONTROL: an `arm` that answers current for a staged build fails "downloaded ⇔ is
/// downloaded".
#[test]
fn the_update_line_and_the_fleets_standing_are_one_decision() {
    use aterm_types::update_standing::UpdateStanding;
    let checked = aterm_update_core::pkg_check::rfc3339_to_unix("2026-09-23T12:00:00Z").unwrap();
    let now = checked + 600;
    let linux = |staged: Option<u64>, phase: Option<&str>| aterm_update::LinuxUpdateStatus {
        installed_build: 101,
        installed_version: "0.92.0".into(),
        staged_build: staged,
        staged_version: staged.map(|_| "0.93.0".into()),
        staged_commit: None,
        trial_phase: phase.map(str::to_owned),
        trial_starts: 0,
        trial_healthy: false,
    };
    let refusing = aterm_update::ApplyLaneReport {
        last_refusal: "the Mac setup offer is still open".into(),
        ..Default::default()
    };
    let tried = aterm_update::ApplyLaneReport {
        last_failure_target_build: 101,
        failures_for_target: 2,
        ..Default::default()
    };
    let with = |edit: &dyn Fn(&mut aterm_update::UpdateStatus)| {
        let mut st = update_status();
        edit(&mut st);
        st
    };
    let staged = |st: &mut aterm_update::UpdateStatus| {
        st.staged_build = Some(101);
        st.staged_version = Some("0.92.0".into());
    };
    let corpus: Vec<(
        aterm_update::UpdateStatus,
        Option<&aterm_update::ApplyLaneReport>,
    )> = vec![
        (with(&|st| st.enabled = false), None),
        (with(&|st| st.linux = Some(linux(Some(102), None))), None),
        (
            with(&|st| st.linux = Some(linux(None, Some("Installed")))),
            None,
        ),
        (with(&|st| st.installable = false), None),
        (
            with(&|st| {
                staged(st);
                st.hold = Some(held_view(now, now, false));
            }),
            None,
        ),
        (
            with(&|st| {
                staged(st);
                st.start_failure = Some(start_failed_view(now, now));
            }),
            Some(&refusing),
        ),
        (with(&|st| staged(st)), Some(&refusing)),
        (with(&|st| staged(st)), Some(&tried)),
        (with(&|st| staged(st)), None),
        (with(&|st| st.channel_unreadable = true), None),
        (
            with(&|st| {
                st.failing_persistent = true;
                st.failing_checks_kind = "manifest".into();
                st.outcome = "reinstall aterm from the current release".into();
            }),
            None,
        ),
        (
            with(&|st| {
                st.failing_persistent = true;
                st.failing_kind = "network".into();
            }),
            None,
        ),
        (
            with(&|st| {
                st.failing_checks = 1;
                st.failing_checks_kind = "network".into();
            }),
            None,
        ),
        (
            with(&|st| st.hold = Some(held_view(now - 6000, now, false))),
            None,
        ),
        (
            with(&|st| {
                st.behind = true;
                st.offered = Some(101);
            }),
            None,
        ),
        (with(&|_| {}), None),
        (with(&|st| st.updated_at = String::new()), None),
    ];
    let says = |standing: UpdateStanding| -> &'static [&'static str] {
        match standing {
            UpdateStanding::Off => &["doesn\u{2019}t update itself"],
            UpdateStanding::Installed => &["is installed"],
            UpdateStanding::Downloaded => &["is downloaded"],
            UpdateStanding::Cannot => &["can\u{2019}t update itself"],
            UpdateStanding::Held => &["is held for"],
            UpdateStanding::Refused => &["install was refused"],
            UpdateStanding::NotInstalling => &["didn\u{2019}t install ("],
            UpdateStanding::Unreadable => &["can\u{2019}t check for updates"],
            UpdateStanding::TooOld => &["is too old"],
            UpdateStanding::Failing => &["keep failing"],
            UpdateStanding::CheckFailed => &["the last check didn\u{2019}t finish"],
            UpdateStanding::Retrying => &["again"],
            UpdateStanding::Behind => &["a newer release is out"],
            UpdateStanding::Current => &["is up to date"],
            UpdateStanding::Unchecked => &["hasn\u{2019}t checked"],
            UpdateStanding::Unread => &[],
        }
    };
    let mut seen = Vec::new();
    for (st, lane) in &corpus {
        let standing = st.standing(100, *lane);
        let line = update_summary(
            "0.91.0",
            100,
            st,
            *lane,
            now,
            true,
            true,
            &DevCopy::No,
            "hint",
        )
        .0;
        seen.push(standing);
        for other in UpdateStanding::ALL {
            for words in says(other) {
                // A refused or failed install is a download too.
                let shared = *words == "is downloaded"
                    && matches!(
                        standing,
                        UpdateStanding::Refused | UpdateStanding::NotInstalling
                    );
                assert_eq!(
                    line.contains(words),
                    other == standing || shared,
                    "{standing:?} \u{21d4} {words}: {line}"
                );
            }
        }
    }
    for standing in UpdateStanding::ALL {
        assert_eq!(
            seen.contains(&standing),
            standing != UpdateStanding::Unread,
            "{standing:?} has a status in the sweep"
        );
    }
}

/// A hold on build 101 (0.92.0) after it did not start cleanly, at its second try, set at `at`
/// and lapsing an hour and a half later (live as of `now` when `now` is before that).
fn held_view(at: i64, now: i64, peer_lifted: bool) -> aterm_update::HoldView {
    let until = u64::try_from(at).unwrap() + 5400;
    aterm_update::HoldView {
        build: 101,
        version: "0.92.0".into(),
        cause: aterm_update_core::update_record::Cause::CrashLoop,
        witness: aterm_update_core::update_record::Witness::Headless,
        why: "the listener did not answer".into(),
        tries: 2,
        until,
        live: u64::try_from(now).unwrap() < until,
        peer_lifted,
    }
}

/// Build 101's (0.92.0) staged bytes, which a window running 100 saw fail to start at `at`: the
/// first try, lapsing four hours later (live as of `now` when `now` is before that).
fn start_failed_view(at: i64, now: i64) -> aterm_update::HoldView {
    let until = u64::try_from(at).unwrap() + 4 * 3600;
    aterm_update::HoldView {
        build: 101,
        version: "0.92.0".into(),
        cause: aterm_update_core::update_record::Cause::StartFailed,
        witness: aterm_update_core::update_record::Witness::Window,
        why: "stalled (unlaunched)".into(),
        tries: 1,
        until,
        live: u64::try_from(now).unwrap() < until,
        peer_lifted: false,
    }
}

/// A STAGED BUILD A WINDOW SAW FAIL TO START IS NAMED: while the window-start failure of the
/// staged bytes holds, no start without a window installs them, and `aterm update status` says
/// so over the download and its refusal: the build, how long more, why, which try and the verb
/// that ends it, with how the window saw it fail on stderr. Once it lapses, the download's own
/// line is said again.
/// NEGATIVE CONTROL: saying the start-failure arm by `held_update_line`, as a hold of its own,
/// fails `held over its download and its refusal`.
#[test]
fn the_update_line_names_a_staged_build_a_window_saw_fail_to_start() {
    let checked = aterm_update_core::pkg_check::rfc3339_to_unix("2026-09-23T12:00:00Z").unwrap();
    let now = checked + 600;
    let refusing = aterm_update::ApplyLaneReport {
        last_refusal: "the Mac setup offer is still open".into(),
        ..Default::default()
    };
    let mut st = update_status();
    st.staged_build = Some(101);
    st.staged_version = Some("0.92.0".into());
    st.start_failure = Some(start_failed_view(now - 600, now));
    let (line, trouble) = update_summary(
        "0.91.0",
        100,
        &st,
        Some(&refusing),
        now,
        true,
        true,
        &DevCopy::No,
        "",
    );
    assert_eq!(
        line,
        "aterm 0.91.0 \u{b7} 0.92.0 is held for 4 h more from every start with no window (a \
         window here saw it fail to start, try 1) \u{b7} `aterm update retry` ends that now",
        "held over its download and its refusal"
    );
    assert_eq!(trouble.as_deref(), Some("stalled (unlaunched)"));
    st.start_failure = Some(start_failed_view(now - 4 * 3600, now));
    let line = update_summary("0.91.0", 100, &st, None, now, true, true, &DevCopy::No, "").0;
    assert!(line.contains("0.92.0 is downloaded"), "{line}");
}

/// The headline never says "up to date" while a newer build is held or offered, on every
/// system: a live hold names the build, how long it holds, why, which try and the verb that ends
/// it, outranks a download of it and puts the recorded reason on stderr; a lapsed hold says the
/// build is tried again (and that another Mac started it, when one cut it short); an offer
/// newer than the running build says it is not installed yet.
/// NEGATIVE CONTROL: with the live-hold arm removed from `update_summary`, the staged download's
/// line wins (`` held over its download ``).
#[test]
fn the_update_line_names_a_held_or_newer_build_and_never_says_up_to_date() {
    let checked = aterm_update_core::pkg_check::rfc3339_to_unix("2026-09-23T12:00:00Z").unwrap();
    let now = checked + 600;
    let say = |st: &aterm_update::UpdateStatus| {
        update_summary("0.91.0", 100, st, None, now, true, true, &DevCopy::No, "")
    };
    for linux in [false, true] {
        let mut st = update_status();
        if linux {
            st.linux = Some(aterm_update::LinuxUpdateStatus {
                installed_build: 100,
                installed_version: "0.91.0".into(),
                staged_build: None,
                staged_version: None,
                staged_commit: None,
                trial_phase: None,
                trial_starts: 0,
                trial_healthy: true,
            });
        }
        st.hold = Some(held_view(now, now, false));
        let (line, trouble) = say(&st);
        assert_eq!(
            line,
            "aterm 0.91.0 \u{b7} 0.92.0 is held for 90 min more (it did not start cleanly, \
             try 2) \u{b7} `aterm update retry` tries it now"
        );
        assert_eq!(trouble.as_deref(), Some("the listener did not answer"));
        st.staged_build = Some(101);
        st.staged_version = Some("0.92.0".into());
        assert!(say(&st).0.contains("is held for"), "held over its download");
        st.staged_build = None;

        st.hold = Some(held_view(now - 6000, now, false));
        let (line, trouble) = say(&st);
        assert_eq!(
            line,
            "aterm 0.91.0 \u{b7} trying 0.92.0 again \u{b7} checked 10 min ago"
        );
        assert_eq!(trouble, None);
        st.hold = Some(held_view(now - 6000, now, true));
        assert!(
            say(&st)
                .0
                .contains("trying 0.92.0 again: it started on another Mac"),
            "lifted by a peer"
        );

        st.hold = None;
        st.offered = Some(101);
        st.behind = true;
        let line = say(&st).0;
        assert!(
            line.contains("a newer release is out and not installed yet"),
            "{line}"
        );
        assert!(!line.contains("up to date"), "{line}");
        st.behind = false;
        assert!(say(&st).0.contains("is up to date"));
    }
}

#[test]
fn a_staged_updates_standing_refusal_reaches_the_nonverbose_summary() {
    let checked = aterm_update_core::pkg_check::rfc3339_to_unix("2026-09-23T12:00:00Z").unwrap();
    let mut staged = update_status();
    staged.staged_build = Some(101);
    staged.staged_version = Some("0.92.0".into());
    let reason = "the Mac setup offer is still open";
    let mut lane = aterm_update::ApplyLaneReport {
        last_refusal: reason.into(),
        ..Default::default()
    };
    let summary = |st: &aterm_update::UpdateStatus,
                   lane: Option<&aterm_update::ApplyLaneReport>,
                   automatic| {
        update_summary(
            "0.91.0",
            100,
            st,
            lane,
            checked + 60,
            automatic,
            true,
            &DevCopy::No,
            "log detail",
        )
    };
    for automatic in [true, false] {
        assert_eq!(
            summary(&staged, Some(&lane), automatic).1.as_deref(),
            Some(reason),
            "a standing refusal matters with either installation preference"
        );
    }
    lane.last_failure_target_build = 101;
    lane.failures_for_target = 2;
    assert_eq!(
        summary(&staged, Some(&lane), true).1.as_deref(),
        Some(reason),
        "the standing cause remains visible after an earlier failed attempt"
    );
    lane.last_refusal = " \n\t".into();
    assert_eq!(
        summary(&staged, Some(&lane), true).1.as_deref(),
        Some("log detail"),
        "an empty refusal does not replace failure diagnostics"
    );
    lane.failures_for_target = 0;
    assert!(summary(&staged, Some(&lane), true).1.is_none());
    assert!(summary(&staged, None, true).1.is_none());
    lane.last_refusal = reason.into();
    staged.staged_build = Some(100);
    assert!(summary(&staged, Some(&lane), true).1.is_none());
    staged.staged_build = None;
    assert!(summary(&staged, Some(&lane), true).1.is_none());
}

#[test]
fn downloaded_update_summaries_distinguish_candidate_and_running_identity() {
    for linux in [false, true] {
        for next in [Some("0.92.0"), Some("0.91.0"), None] {
            for automatic in [false, true] {
                for lane in [
                    aterm_update::ApplyLaneReport::default(),
                    aterm_update::ApplyLaneReport {
                        last_refusal: "waiting for work to retire".into(),
                        ..Default::default()
                    },
                    aterm_update::ApplyLaneReport {
                        last_failure_target_build: 101,
                        failures_for_target: 1,
                        ..Default::default()
                    },
                ] {
                    let mut status = update_status();
                    status.staged_build = Some(101);
                    status.staged_version = next.map(str::to_owned);
                    if linux {
                        status.linux = Some(aterm_update::LinuxUpdateStatus {
                            installed_build: 100,
                            installed_version: "0.91.0".into(),
                            staged_build: Some(101),
                            staged_version: next.map(str::to_owned),
                            staged_commit: None,
                            trial_phase: None,
                            trial_starts: 0,
                            trial_healthy: true,
                        });
                    }
                    let line = update_summary(
                        "0.91.0",
                        100,
                        &status,
                        Some(&lane),
                        0,
                        automatic,
                        true,
                        &DevCopy::No,
                        "details",
                    )
                    .0;
                    let (candidate, running) = if next == Some("0.91.0") {
                        ("101", "100")
                    } else {
                        (next.unwrap_or("101"), "0.91.0")
                    };
                    assert!(
                        line.contains(candidate),
                        "missing candidate {candidate}: {line}"
                    );
                    assert!(
                        line.contains(running),
                        "missing running identity {running}: {line}"
                    );
                }
            }
        }
    }
}

fn scratch_layout(label: &str) -> (aterm_tempfile::TempDir, atpkg::store::Layout) {
    let dir = aterm_tempfile::Builder::new()
        .prefix(&format!("aterm-session-lane-{label}-"))
        .suffix(".noindex")
        .tempdir()
        .expect("scratch prefix");
    let prefix = dir.path().to_path_buf();
    (dir, atpkg::store::Layout { prefix })
}

#[test]
fn the_session_pass_is_due_on_the_machine_rule_and_claimed_once() {
    let (_dir, layout) = scratch_layout("pass");
    let now = 1_790_000_000_i64;
    assert!(
        session_pass_due(&layout, now),
        "never checked: the first tab"
    );
    assert!(
        !session_pass_due(&layout, now + 1),
        "the second tab, a second later: claimed"
    );
    assert!(
        session_pass_due(&layout, now + 5 * 60),
        "still never checked, five minutes on: the next tab tries again"
    );
    std::fs::write(
        layout.status(),
        "schema = 1\nupdated_at = \"2026-09-21T13:00:00Z\"\n\
         last_success_at = \"2026-09-21T13:00:00Z\"\n",
    )
    .unwrap();
    let success =
        aterm_update_core::pkg_check::rfc3339_to_unix("2026-09-21T13:00:00Z").expect("stamp");
    assert!(!session_pass_due(&layout, success + 3600));
    let walk = i64::try_from(aterm_update_core::pkg_check::FULL_PASS_INTERVAL_SECS).unwrap();
    assert!(!session_pass_due(&layout, success + walk - 1));
    assert!(session_pass_due(&layout, success + walk), "six hours on");
    std::fs::write(
        layout.status(),
        "schema = 1\nupdated_at = \"2026-09-21T14:00:00Z\"\n\
         last_success_at = \"2026-09-21T13:00:00Z\"\n",
    )
    .unwrap();
    let claim = i64::try_from(aterm_update_core::pkg_check::PASS_SPACING_SECS).unwrap();
    assert!(
        session_pass_due(&layout, success + walk + claim),
        "six hours on, past the last claim: owed"
    );
    std::fs::write(
        layout.status(),
        "schema = 1\nupdated_at = \"2026-09-22T13:30:00Z\"\n\
         last_success_at = \"2026-09-21T13:00:00Z\"\nlast_pass = \"failed\"\n\
         last_pass_at = \"2026-09-22T13:30:00Z\"\n",
    )
    .unwrap();
    let attempt =
        aterm_update_core::pkg_check::rfc3339_to_unix("2026-09-22T13:30:00Z").expect("stamp");
    assert!(!session_pass_due(&layout, attempt + 60));
    assert!(!session_pass_due(&layout, attempt + 5 * 60));
    assert!(
        session_pass_due(&layout, attempt + walk),
        "the negative control: six hours after it, it is owed again"
    );
    std::fs::write(
        layout.status(),
        "schema = 1\nupdated_at = \"2026-09-22T13:30:00Z\"\n\
         last_success_at = \"2026-09-21T13:00:00Z\"\nlast_pass = \"ok\"\n\
         last_pass_at = \"2026-09-21T13:00:00Z\"\n",
    )
    .unwrap();
    assert!(session_pass_due(&layout, attempt + 5 * 60));
}

#[test]
fn a_failing_or_running_pass_is_not_respawned_by_every_tab() {
    let (_dir, layout) = scratch_layout("failing");
    let attempt =
        aterm_update_core::pkg_check::rfc3339_to_unix("2026-09-22T13:00:00Z").expect("stamp");
    std::fs::write(
        layout.status(),
        "schema = 1\nupdated_at = \"2026-09-22T13:00:00Z\"\n\
         outcome = \"update failed: index unreachable\"\nlast_pass = \"failed\"\n\
         last_pass_at = \"2026-09-22T13:00:00Z\"\n",
    )
    .unwrap();
    for minutes in [5, 10, 60, 120, 359] {
        assert!(
            !session_pass_due(&layout, attempt + minutes * 60),
            "a tab {minutes} min after the failed pass"
        );
    }
    assert!(
        session_pass_due(&layout, attempt + 6 * 3600),
        "the negative control: six hours on, one tab tries again"
    );
    let (_fresh_dir, fresh) = scratch_layout("running");
    let now = attempt + 7 * 3600;
    std::fs::write(
        fresh.progress_file(),
        format!(
            "{{\"v\":{},\"pid\":{},\"heartbeat_unix\":{now}}}",
            atpkg::progress::PROGRESS_VERSION,
            std::process::id()
        ),
    )
    .unwrap();
    assert!(
        !session_pass_due(&fresh, now),
        "never checked, but installing"
    );
    std::fs::remove_file(fresh.progress_file()).unwrap();
    assert!(
        session_pass_due(&fresh, now),
        "the negative control: nothing running"
    );
}

#[test]
#[cfg(unix)]
fn a_detached_pass_is_neither_our_child_nor_in_our_process_group() {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("aterm-detached-pass-")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch dir");
    let dir = scratch.path();
    let pid_file = dir.join("pid");
    let env_file = dir.join("env");
    let script = format!(
        "echo \"$ATPKG_SPAWNER_PID\" > '{1}' && echo $$ > '{0}.tmp' && mv '{0}.tmp' '{0}' \
         && exec sleep 30",
        pid_file.display(),
        env_file.display()
    );
    let args = [std::ffi::OsString::from("-c"), script.into()];
    atpkg::platform::spawn_detached(
        std::ffi::OsStr::new("/bin/sh"),
        &args,
        Some(("ATPKG_SPAWNER_PID", atpkg::cli::SPAWNER_DETACHED)),
    )
    .expect("spawn");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let pid: libc::pid_t = loop {
        if let Ok(text) = std::fs::read_to_string(&pid_file) {
            break text.trim().parse().expect("a pid");
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the pass never started"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let mut status = 0;
    let reaped = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
    let errno = std::io::Error::last_os_error().raw_os_error();
    let pgid = unsafe { libc::getpgid(pid) };
    let ours = unsafe { libc::getpgid(0) };
    unsafe { libc::kill(pid, libc::SIGKILL) };
    if reaped == 0 {
        unsafe { libc::waitpid(pid, &mut status, 0) };
    }
    let env = std::fs::read_to_string(&env_file).unwrap_or_default();
    assert_eq!(
        env.trim(),
        atpkg::cli::SPAWNER_DETACHED,
        "the pass is told it was detached on purpose"
    );
    assert_eq!(
        (reaped, errno),
        (-1, Some(libc::ECHILD)),
        "the detached pass is still this process's child"
    );
    assert_ne!(
        pgid, ours,
        "the detached pass shares the session's process group"
    );
}

static NO_REROUTE_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn a_plain_launch_carries_its_directory_or_declines_to_route() {
    let osv = |list: &[&str]| -> Vec<OsString> { list.iter().map(OsString::from).collect() };
    assert_eq!(plain_launch_dir(&osv(&["--window"])), Ok(None));
    let cwd = std::env::current_dir().expect("a working directory");
    for spelling in [
        osv(&["--window", "-d", "."]),
        osv(&["--working-directory", "."]),
    ] {
        let found = plain_launch_dir(&spelling).expect("resolvable");
        assert_eq!(
            found.as_deref().map(std::path::Path::new),
            Some(cwd.as_path()),
            "{spelling:?}"
        );
    }
    assert_eq!(
        plain_launch_dir(&osv(&["-d", "definitely-not-here-9f3a"])),
        Err(())
    );
}

#[test]
fn a_child_command_payload_is_never_routed_by_policy() {
    let _env_lock = NO_REROUTE_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    aterm_log::env::unset(atpkg::reroute::PASSTHROUGH_ENV);
    let osv = |list: &[&str]| -> Vec<OsString> { list.iter().map(OsString::from).collect() };
    let env = aterm_cli::LaunchEnv::default();
    for argv in [
        osv(&["-e", "vim"]),
        osv(&["--window", "-e", "vim"]),
        osv(&["--command", "vim"]),
        osv(&["--", "sh", "-c", "echo hi"]),
        osv(&["--window", "-d", ".", "-e", "vim"]),
    ] {
        assert_eq!(
            plain_launch_request(&argv, env),
            None,
            "{argv:?} carries a payload a forwarded tab cannot run"
        );
    }
    let dash_e = osv(&["-e", "vim"]);
    assert_eq!(payload_boundary(&dash_e), 0);
    assert!(
        plain_launch_request(&dash_e[..0], env).is_some(),
        "the pre-boundary scan of `-e vim` is an empty, maximally eligible argv — \
         the gate must never be handed it"
    );
    assert!(plain_launch_request(&osv(&[]), env).is_some());
    assert!(plain_launch_request(&osv(&["--window"]), env).is_some());
}

#[test]
fn the_gui_alias_routes_the_windowing_verbs_and_nothing_else() {
    for verb in aterm_cli::Verb::ALL {
        let expected = if verb.is_windowing() {
            AliasRoute::AliasWindowVerb
        } else {
            AliasRoute::AliasWindow
        };
        assert_eq!(
            alias_route("aterm-gui", verb.name()),
            expected,
            "aterm-gui {}",
            verb.name()
        );
    }
    for operand in ["", "--window", "-d", "--headless", "--diagnose"] {
        assert_eq!(
            alias_route("aterm-gui", operand),
            AliasRoute::AliasWindow,
            "aterm-gui {operand:?}"
        );
    }
    assert_eq!(alias_route("aterm-ctl", "new-tab"), AliasRoute::Ctl);
    assert_eq!(alias_route("atpkg", "new-tab"), AliasRoute::Pkg);
    assert_eq!(alias_route("agh", "release"), AliasRoute::Agh);
    assert_eq!(alias_route("aterm-fleet", ""), AliasRoute::Fleet);
    assert_eq!(alias_route("aterm-drive", ""), AliasRoute::Drive);
    assert_eq!(
        alias_route("aterm-askpass", "Password:"),
        AliasRoute::Askpass
    );
    assert_eq!(
        alias_route("aterm-machines-askpass", "admin@lab-07.local's password: "),
        AliasRoute::MachinesAskpass
    );
    assert_eq!(alias_route("aterm-link", ""), AliasRoute::Link);
    assert_eq!(alias_route("aterm-link", "serve"), AliasRoute::Link);
    assert_eq!(
        alias_route("aterm-open-url", "https://claude.ai/x"),
        AliasRoute::OpenUrl
    );
    for operand in ["", ".git/COMMIT_EDITMSG", "+3", "new-window", "edit"] {
        assert_eq!(
            alias_route("amacs", operand),
            AliasRoute::Amacs,
            "{operand:?}"
        );
    }
    for name in ["aterm", "aterm-cli", "my-renamed-aterm"] {
        assert_eq!(
            alias_route(name, "new-window"),
            AliasRoute::FrontDoor,
            "{name}"
        );
        assert_eq!(alias_route(name, ""), AliasRoute::FrontDoor, "{name}");
    }
}

#[test]
fn the_windowed_target_name_is_the_gui_alias() {
    for verb in aterm_cli::Verb::ALL {
        assert_eq!(
            alias_route("aterm-windowed", verb.name()),
            alias_route("aterm-gui", verb.name()),
            "aterm-windowed {}",
            verb.name()
        );
    }
    assert_eq!(
        alias_route("aterm-windowed", "new-window"),
        AliasRoute::AliasWindowVerb
    );
    for operand in ["", "--window", "-d", "--headless", "--diagnose"] {
        assert_eq!(
            alias_route("aterm-windowed", operand),
            AliasRoute::AliasWindow,
            "aterm-windowed {operand:?}"
        );
    }
}

#[test]
fn a_console_window_launch_goes_to_the_sibling_unless_a_harness_reads_it() {
    let osv = |list: &[&str]| -> Vec<OsString> { list.iter().map(OsString::from).collect() };
    let window = if cfg!(windows) {
        WindowHost::WindowedSibling
    } else {
        WindowHost::ThisProcess
    };
    assert_eq!(window_host_for(&osv(&[])), window, "the no-TTY launch");
    assert_eq!(window_host_for(&osv(&["--window"])), window);
    assert_eq!(
        window_host_for(&osv(&["--window", "-d", ".", "--hold"])),
        window
    );
    for flag in [
        "--help",
        "--version",
        "--list-fonts",
        "--install-context-menu",
    ] {
        assert_eq!(
            window_host_for(&osv(&["--window", flag])),
            window,
            "{flag} is answered by the parser run before the handoff"
        );
    }
    for flag in ["--headless", "--diagnose"] {
        assert_eq!(
            window_host_for(&osv(&["--window", flag])),
            WindowHost::ThisProcess,
            "{flag}: a harness reads this process's output"
        );
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn network_publication_cache_never_renews_stalled_or_busy_evidence() {
    use aterm_db::network_state::Publication;
    use aterm_types::network_state::{NetworkObservation, Visibility};
    use std::{
        sync::{Arc, Mutex},
        time::{Duration, Instant},
    };

    for prefetch in [false, true] {
        let at = Instant::now();
        let publisher = Arc::new(Mutex::new(Some(Publication::new(
            NetworkObservation {
                observed_at_ms: 100_000,
                visibility: Visibility::Active,
                ..NetworkObservation::default()
            },
            at,
        ))));
        let clock = Arc::new(Mutex::new((at, Some(100_000))));
        let reader = publisher.clone();
        let timer = clock.clone();
        let read = super::network_state_source_with_readers(
            move || reader.try_lock().ok().and_then(|state| state.clone()),
            || None,
            None,
            move || *timer.lock().unwrap(),
        );
        if prefetch {
            assert_eq!(
                read(aterm_gui::NetworkRead::WithServices)
                    .unwrap()
                    .network
                    .visibility,
                Visibility::Active
            );
        }
        *clock.lock().unwrap() = (at + Duration::from_secs(20), Some(120_000));
        assert_eq!(
            read(aterm_gui::NetworkRead::WithServices)
                .unwrap()
                .network
                .visibility,
            Visibility::Unavailable
        );
        let mut held = publisher.lock().unwrap();
        *clock.lock().unwrap() = (at + Duration::from_secs(21), Some(113_000));
        let during = read(aterm_gui::NetworkRead::WithServices).unwrap();
        assert_eq!(during.network.visibility, Visibility::Unavailable);
        assert_eq!(during.network.observed_at_ms, 100_000);
        *held = Some(Publication::new(
            NetworkObservation {
                observed_at_ms: 113_000,
                visibility: Visibility::Active,
                ..NetworkObservation::default()
            },
            at + Duration::from_secs(21),
        ));
        drop(held);
        assert_eq!(
            read(aterm_gui::NetworkRead::WithServices)
                .unwrap()
                .network
                .visibility,
            Visibility::Active
        );
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn service_observations_survive_an_absent_network_runtime() {
    use aterm_types::network_state::{
        HistoryState, ServiceAvailability, ServiceStates, Visibility,
    };
    use std::sync::{Arc, Mutex};

    let mut services = ServiceStates::default();
    services.secrets.availability = ServiceAvailability::Cached;
    services.secrets.observed_at_ms = 1000;
    let at = std::time::Instant::now();
    let services = Arc::new(Mutex::new(aterm_db::service_state::Publication::new(
        services, at,
    )));
    let publisher = services.clone();
    let read = super::network_state_source_with_readers(
        || None,
        move || Some(publisher.lock().unwrap().clone()),
        None,
        move || (at, Some(2000)),
    );
    let first = read(aterm_gui::NetworkRead::WithServices).unwrap();
    assert_eq!(first.network.visibility, Visibility::Unavailable);
    assert_eq!(first.network.history, HistoryState::Unavailable);
    assert_eq!(
        first.services.secrets.availability,
        ServiceAvailability::Cached
    );
    assert!(Arc::ptr_eq(
        &first,
        &read(aterm_gui::NetworkRead::WithServices).unwrap()
    ));

    let mut changed = first.services.clone();
    changed.secrets.availability = ServiceAvailability::Unavailable;
    *services.lock().unwrap() = aterm_db::service_state::Publication::new(changed, at);
    let second = read(aterm_gui::NetworkRead::WithServices).unwrap();
    assert!(second.revision > first.revision);
    assert_eq!(
        second.services.secrets.availability,
        ServiceAvailability::Unavailable
    );
    assert_eq!(
        first.services.secrets.availability,
        ServiceAvailability::Cached
    );
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_busy_service_publisher_cannot_stall_the_network_ui_reader() {
    use aterm_types::network_state::{ServiceAvailability, ServiceStates};
    use std::sync::{Arc, Mutex, mpsc};

    for prefetch in [false, true] {
        let mut initial = ServiceStates::default();
        initial.secrets.availability = ServiceAvailability::Cached;
        initial.secrets.observed_at_ms = 1000;
        let at = std::time::Instant::now();
        let services = Arc::new(Mutex::new(aterm_db::service_state::Publication::new(
            initial, at,
        )));
        let reader = services.clone();
        let read = super::network_state_source_with_readers(
            || None,
            move || reader.try_lock().ok().map(|state| state.clone()),
            None,
            move || (at, Some(2000)),
        );
        let first = prefetch.then(|| read(aterm_gui::NetworkRead::WithServices).unwrap());
        let mut publisher = services.lock().unwrap();
        let mut changed = ServiceStates::default();
        changed.secrets.availability = ServiceAvailability::Unavailable;
        changed.secrets.observed_at_ms = 2000;
        *publisher = aterm_db::service_state::Publication::new(changed, at);
        let concurrent = read.clone();
        let (send, receive) = mpsc::channel();
        let task = std::thread::spawn(move || {
            send.send(concurrent(aterm_gui::NetworkRead::WithServices))
                .unwrap()
        });
        let result = receive.recv_timeout(std::time::Duration::from_secs(60));
        drop(publisher);
        task.join().unwrap();
        let during = result
            .expect("a publishing worker must not stall the UI reader")
            .expect("only the producer is busy, not the composition reader");
        if let Some(first) = first {
            assert!(Arc::ptr_eq(&first, &during));
            assert_eq!(during.services.secrets.observed_at_ms, 1000);
        } else {
            assert_eq!(during.services.secrets.observed_at_ms, 0);
            assert_eq!(
                during.services.secrets.availability,
                ServiceAvailability::Unknown
            );
        }
        let after = read(aterm_gui::NetworkRead::WithServices).unwrap();
        assert_eq!(after.services.secrets.observed_at_ms, 2000);
        assert_eq!(
            after.services.secrets.availability,
            ServiceAvailability::Unavailable
        );
        assert!(after.revision > during.revision);
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_busy_composition_reader_returns_no_sample_instead_of_startup_state() {
    use aterm_types::network_state::{
        HistoryState, NetworkObservation, ServiceAvailability, ServiceStates, Visibility,
    };
    use std::{
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
            mpsc,
        },
        time::{Duration, Instant},
    };

    let at = Instant::now();
    let network = aterm_db::network_state::Publication::new(
        NetworkObservation {
            observed_at_ms: 100_000,
            visibility: Visibility::Active,
            history: HistoryState::Ready,
            detail: "authenticated observer".into(),
            ..Default::default()
        },
        at,
    );
    let mut services = ServiceStates::default();
    services.secrets.availability = ServiceAvailability::Cached;
    services.secrets.observed_at_ms = 100_000;
    services.secrets.revision = Some("retained revision".into());
    services.secrets.refresh_failed = true;
    let services = aterm_db::service_state::Publication::new(services, at);
    let clock = Arc::new(Mutex::new((at, Some(100_000))));
    let timer = clock.clone();
    let (entered, entering) = mpsc::channel();
    let (resume, resumed) = mpsc::channel();
    let resumed = Mutex::new(resumed);
    let reads = AtomicUsize::new(0);
    let read = super::network_state_source_with_readers(
        move || {
            if reads.fetch_add(1, Ordering::Relaxed) == 1 {
                entered.send(()).unwrap();
                resumed
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(60))
                    .unwrap();
            }
            Some(network.clone())
        },
        move || Some(services.clone()),
        None,
        move || *timer.lock().unwrap(),
    );
    let first = read(aterm_gui::NetworkRead::WithServices).unwrap();
    assert_eq!(first.network.history, HistoryState::Ready);
    assert_eq!(
        first.services.secrets.availability,
        ServiceAvailability::Cached
    );
    let background = read.clone();
    let background = std::thread::spawn(move || background(aterm_gui::NetworkRead::WithServices));
    entering.recv_timeout(Duration::from_secs(60)).unwrap();
    let foreground = read.clone();
    let (finished, result) = mpsc::channel();
    let foreground = std::thread::spawn(move || {
        finished
            .send(foreground(aterm_gui::NetworkRead::WithServices))
            .unwrap()
    });
    let busy = result.recv_timeout(Duration::from_secs(60));
    resume.send(()).unwrap();
    let resumed = background.join().unwrap().unwrap();
    foreground.join().unwrap();
    // The old WouldBlock => startup fallback policy returns Some here, erasing
    // an already observed history and failed-refresh warning at the consumer.
    assert!(
        busy.expect("the composition reader must not block drawing")
            .is_none()
    );
    assert!(Arc::ptr_eq(&first, &resumed));
    assert!(Arc::ptr_eq(
        &first,
        &read(aterm_gui::NetworkRead::WithServices).unwrap()
    ));

    // Recovery still uses the producer's original monotonic sample: even a
    // wall-clock rollback that remains after observed_at cannot extend it.
    *clock.lock().unwrap() = (at + Duration::from_secs(61), Some(100_001));
    let expired = read(aterm_gui::NetworkRead::WithServices).unwrap();
    assert_eq!(expired.network.visibility, Visibility::Unavailable);
    assert_eq!(expired.network.history, HistoryState::Ready);
    assert_eq!(expired.network.observed_at_ms, 100_000);
    assert_eq!(
        expired.services.secrets.availability,
        ServiceAvailability::Unknown
    );
    assert_eq!(expired.services.secrets.observed_at_ms, 100_000);
    assert_eq!(
        expired.services.secrets.revision,
        first.services.secrets.revision
    );
    assert!(expired.services.secrets.refresh_failed);
    assert!(expired.revision > first.revision);
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn service_publication_cache_expires_without_network_and_never_renews_on_rollback() {
    use aterm_types::network_state::{ServiceAvailability, ServiceStates, Visibility};
    use std::{
        sync::{Arc, Mutex},
        time::{Duration, Instant},
    };
    let at = Instant::now();
    let mut states = ServiceStates::default();
    states.secrets.availability = ServiceAvailability::Cached;
    states.secrets.observed_at_ms = 100_000;
    states.secrets.revision = Some("retained revision".into());
    let publisher = Arc::new(Mutex::new(aterm_db::service_state::Publication::new(
        states.clone(),
        at,
    )));
    let clock = Arc::new(Mutex::new((at, Some(100_000))));
    let source = publisher.clone();
    let timer = clock.clone();
    let read = super::network_state_source_with_readers(
        || None,
        move || source.try_lock().ok().map(|state| state.clone()),
        None,
        move || *timer.lock().unwrap(),
    );
    assert_eq!(
        read(aterm_gui::NetworkRead::WithServices)
            .unwrap()
            .services
            .secrets
            .availability,
        ServiceAvailability::Cached
    );
    *clock.lock().unwrap() = (at + Duration::from_secs(61), Some(161_001));
    assert_eq!(
        read(aterm_gui::NetworkRead::WithServices)
            .unwrap()
            .services
            .secrets
            .availability,
        ServiceAvailability::Unknown
    );
    let mut held = publisher.lock().unwrap();
    *clock.lock().unwrap() = (at + Duration::from_secs(62), Some(130_000));
    let stale = read(aterm_gui::NetworkRead::WithServices).unwrap();
    assert_eq!(stale.network.visibility, Visibility::Unavailable);
    assert_eq!(
        stale.services.secrets.availability,
        ServiceAvailability::Unknown
    );
    assert_eq!(stale.services.secrets.observed_at_ms, 100_000);
    assert_eq!(stale.services.secrets.revision, states.secrets.revision);
    drop(held);
    assert_eq!(
        read(aterm_gui::NetworkRead::WithServices)
            .unwrap()
            .services
            .secrets
            .availability,
        ServiceAvailability::Unknown
    );
    held = publisher.lock().unwrap();
    states.secrets.observed_at_ms = 130_000;
    *held = aterm_db::service_state::Publication::new(states, at + Duration::from_secs(62));
    drop(held);
    let restored = read(aterm_gui::NetworkRead::WithServices).unwrap();
    assert_eq!(
        restored.services.secrets.availability,
        ServiceAvailability::Cached
    );
    assert!(restored.revision > stale.revision);
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn first_service_read_after_a_stall_and_rollback_is_not_fresh() {
    use aterm_types::network_state::{ServiceAvailability, ServiceStates};
    use std::time::{Duration, Instant};
    let at = Instant::now();
    let mut states = ServiceStates::default();
    states.accounts.availability = ServiceAvailability::Current;
    states.accounts.observed_at_ms = 100_000;
    let publication = aterm_db::service_state::Publication::new(states, at);
    let read = super::network_state_source_with_readers(
        || None,
        move || Some(publication.clone()),
        None,
        move || (at + Duration::from_secs(120), Some(110_000)),
    );
    assert_eq!(
        read(aterm_gui::NetworkRead::WithServices)
            .unwrap()
            .services
            .accounts
            .availability,
        ServiceAvailability::Unknown
    );
    assert_eq!(
        read(aterm_gui::NetworkRead::WithServices)
            .unwrap()
            .services
            .accounts
            .observed_at_ms,
        100_000
    );
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_missing_service_observer_does_not_hide_network_observations() {
    use aterm_types::network_state::{ServiceAvailability, Visibility};
    use std::{
        sync::{
            Arc, OnceLock,
            atomic::{AtomicUsize, Ordering},
        },
        time::{Duration, Instant},
    };

    let scratch = aterm_tempfile::tempdir().unwrap();
    let paths = aterm_secrets::client::Paths::for_home(&scratch.path().join("home"));
    let config = scratch.path().join("aterm.toml");
    aterm_uds::atomic_file::write(&config, b"[swarm]\nenabled = false\n", 0o600).unwrap();
    let published = Arc::new(OnceLock::<aterm_db::service_state::Runtime>::new());
    let publisher = published.clone();
    let attempts = Arc::new(AtomicUsize::new(0));
    let tried = attempts.clone();
    let mut startup = super::host_startup::Constructor::new(move || {
        if tried.fetch_add(1, Ordering::SeqCst) == 0 {
            return false;
        }
        let runtime =
            aterm_db::service_state::Runtime::start(paths.clone(), config.clone()).unwrap();
        assert!(publisher.set(runtime).is_ok());
        true
    });
    let now = Instant::now();
    let due = startup.maintain_at(now).unwrap();
    let network = aterm_db::network_state::ObservationHandle::default();
    let services = published.clone();
    let read = super::network_state_source_from(
        move || Some(network.clone()),
        move || {
            services
                .get()
                .map(aterm_db::service_state::Runtime::observation)
        },
        None,
    );
    let snapshot = read(aterm_gui::NetworkRead::WithServices).unwrap();
    assert_eq!(snapshot.network.visibility, Visibility::Checking);
    assert_eq!(
        snapshot.services.accounts.availability,
        ServiceAvailability::Unknown
    );
    assert_eq!(
        snapshot.services.secrets.availability,
        ServiceAvailability::Unknown
    );
    let _ = read(aterm_gui::NetworkRead::Network);
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        1,
        "readers never run constructors"
    );
    startup.maintain_at(due - Duration::from_nanos(1));
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        1,
        "failed starts keep their backoff"
    );
    assert_eq!(startup.maintain_at(due), None);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    // The same held source discovers the real owner, even while sharing is off. Ordinary
    // WithServices demand supplies its first paid metadata sample; painting starts nothing.
    let healthy = aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the recovered service owner's first metadata publication",
        || {
            let snapshot = read(aterm_gui::NetworkRead::WithServices)?;
            (snapshot.services.accounts.observed_at_ms != 0
                && snapshot.services.secrets.observed_at_ms != 0)
                .then_some(snapshot)
        },
    );
    assert_eq!(healthy.network.visibility, Visibility::Checking);
    assert!(healthy.revision > snapshot.revision);
    assert_eq!(startup.maintain_at(due + Duration::from_secs(120)), None);
    let _ = read(aterm_gui::NetworkRead::Network);
    let _ = read(aterm_gui::NetworkRead::WithServices);
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        2,
        "a published owner is not restarted"
    );
    drop(read);
    drop(published);
}

/// Only a reader that draws the services reads them, since a services read asks their
/// observer for its next sample; a network reader gets their last sample, which ages as it
/// would, keeping what it last observed.
/// NEGATIVE CONTROL: a composition that reads the services whatever was asked counts a
/// services read for each network read.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_network_read_reads_no_services_and_ages_their_last_sample() {
    use aterm_gui::NetworkRead::{Network, WithServices};
    use aterm_types::network_state::{ServiceAvailability, ServiceStates};
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Duration;

    let at = std::time::Instant::now();
    let mut states = ServiceStates::default();
    states.secrets.availability = ServiceAvailability::Cached;
    states.secrets.observed_at_ms = 100_000;
    let publication = aterm_db::service_state::Publication::new(states, at);
    let reads = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&reads);
    let clock = Arc::new(Mutex::new((at, Some(100_000))));
    let timer = Arc::clone(&clock);
    let read = super::network_state_source_with_readers(
        || None,
        move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Some(publication.clone())
        },
        None,
        move || *timer.lock().unwrap(),
    );
    let read_with = |kind| read(kind).unwrap();
    assert_eq!(
        read_with(Network).services.secrets.availability,
        ServiceAvailability::Unknown,
        "nothing read the services yet"
    );
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    assert_eq!(
        read_with(WithServices).services.secrets.availability,
        ServiceAvailability::Cached
    );
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    assert_eq!(
        read_with(Network).services.secrets.availability,
        ServiceAvailability::Cached,
        "the last sample, still current"
    );
    *clock.lock().unwrap() = (at + Duration::from_secs(61), Some(161_000));
    let aged = read_with(Network).services.secrets.clone();
    assert_eq!(aged.availability, ServiceAvailability::Unknown);
    assert_eq!(aged.observed_at_ms, 100_000);
    assert_eq!(reads.load(Ordering::SeqCst), 1);
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn an_absent_network_runtime_retains_history_but_never_current_reachability() {
    use aterm_types::network_state::{
        HistoryState, MachineIdentity, NetworkObservation, PeerObservation, Reachability,
        Visibility,
    };
    use std::sync::Arc;

    let retained = NetworkObservation {
        history: HistoryState::Ready,
        visibility: Visibility::Active,
        observer: Some(MachineIdentity {
            machine_id: [3; 16],
            incarnation: [4; 16],
            label: None,
        }),
        observed_at_ms: 2000,
        unverified_endpoints: 3,
        peers: vec![PeerObservation {
            slot: 7,
            identity: MachineIdentity {
                machine_id: [1; 16],
                incarnation: [2; 16],
                label: Some("saved machine".into()),
            },
            first_seen_ms: 1000,
            last_seen_ms: 2000,
            last_checked_ms: Some(2000),
            last_repair: None,
            last_authenticated_endpoint: None,
            reachability: Reachability::Reachable,
            instances: Some(2),
            samples: Vec::new(),
            lease_ms: None,
        }],
        ..Default::default()
    };
    let read = super::network_state_source_from(|| None, || None, Some(retained.clone()));
    let snapshot = read(aterm_gui::NetworkRead::WithServices).unwrap();
    assert_eq!(snapshot.network.visibility, Visibility::Unavailable);
    assert!(snapshot.network.observer.is_none());
    assert_eq!(snapshot.network.history, HistoryState::Ready);
    assert_eq!(snapshot.network.observed_at_ms, 2000);
    assert_eq!(snapshot.network.unverified_endpoints, 0);
    let peer = &snapshot.network.peers[0];
    assert_eq!(peer.slot, 7);
    assert_eq!(peer.identity, retained.peers[0].identity);
    assert_eq!((peer.first_seen_ms, peer.last_seen_ms), (1000, 2000));
    assert_eq!(peer.reachability, Reachability::Unknown);
    assert_eq!(peer.last_checked_ms, None);
    assert!(Arc::ptr_eq(
        &snapshot,
        &read(aterm_gui::NetworkRead::WithServices).unwrap()
    ));
    assert_eq!(retained.peers[0].reachability, Reachability::Reachable);
}

/// EACH LOOK AT THE NETWORK IS OFFERED TO THE FLEET TABLES, in the state it was observed in: a
/// window with no observer and no retained history offers nothing read (held, `no-history`),
/// one with retained history offers that history (held, `unobserved`, dated by it), and neither
/// is ever a live observation read at the epoch.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn each_look_at_the_network_is_offered_to_the_fleet_tables_in_the_state_it_was_observed_in() {
    use aterm_db::fleet::{Folder, HELD_NO_HISTORY, HELD_UNOBSERVED};
    use aterm_types::network_state::{HistoryState, NetworkObservation, Visibility};

    let looked = |retained: Option<NetworkObservation>| {
        let fleet: &'static Folder = Box::leak(Box::new(Folder::new()));
        let read = super::offering_to_fleet(
            super::network_state_source_with_readers(
                || None,
                || None,
                retained,
                || (std::time::Instant::now(), aterm_time::unix_millis()),
            ),
            fleet,
        );
        fleet.fold_now(0);
        assert_eq!(
            fleet.tables().value.sources.network.word(),
            "never",
            "CONTROL: nothing is offered before a look"
        );
        let snapshot = read(aterm_gui::NetworkRead::Network).expect("a look");
        assert_eq!(snapshot.network.visibility, Visibility::Unavailable);
        fleet.fold_now(0);
        let revision = fleet.tables().revision;
        // The same look again offers the same snapshot: nothing new to fold.
        let _ = read(aterm_gui::NetworkRead::Network).expect("a look");
        fleet.fold_now(0);
        assert_eq!(fleet.tables().revision, revision);
        let network = fleet.tables().value.sources.network;
        (network.word(), network.why(), network.at_ms())
    };
    assert_eq!(looked(None), ("held", Some(HELD_NO_HISTORY), None));
    assert_eq!(
        looked(Some(NetworkObservation {
            observed_at_ms: 2_000,
            history: HistoryState::Ready,
            ..NetworkObservation::default()
        })),
        ("held", Some(HELD_UNOBSERVED), Some(2_000))
    );
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn only_an_ordinary_window_autostarts_acaffeine() {
    let osv = |list: &[&str]| -> Vec<OsString> { list.iter().map(OsString::from).collect() };
    for args in [
        osv(&[]),
        osv(&["--window"]),
        osv(&["-d", "."]),
        osv(&["--no-control-sock"]),
        osv(&["--window", "--control-sock", "off"]),
        osv(&["--control-sock", "OFF"]),
        osv(&["--control-sock", "0"]),
        osv(&["--control-sock", ""]),
    ] {
        assert!(ordinary_window(&args), "{args:?}");
    }
    for args in [
        osv(&["--headless"]),
        osv(&["--diagnose"]),
        osv(&["--control-sock", "/tmp/private.sock"]),
        osv(&["--control-sock"]),
        osv(&["--control-sock", "off", "--headless"]),
        osv(&["--control-sock", "--headless"]),
        osv(&["-e", "sh", "--control-sock", "/tmp/private.sock"]),
    ] {
        assert!(!ordinary_window(&args), "{args:?}");
    }
}

#[test]
fn a_spawn_is_handed_to_the_sibling_as_new_window_never_as_a_plain_launch() {
    assert_eq!(
        alias_route("aterm-gui", "new-window"),
        AliasRoute::AliasWindowVerb
    );
    for intent in [
        aterm_cli::LaunchIntent::NewTab,
        aterm_cli::LaunchIntent::NewWindow,
        aterm_cli::LaunchIntent::SplitPane,
        aterm_cli::LaunchIntent::Plain,
    ] {
        for dir in [None, Some(String::from(r"C:\work dir"))] {
            let request = aterm_cli::WindowRequest {
                intent,
                dir: dir.clone(),
                split: aterm_cli::SplitOrientation::Horizontal,
            };
            let argv = spawn_handoff_argv(&request);
            let (verb, rest) = argv.split_first().expect("the verb leads");
            let verb_str = verb.to_string_lossy();
            assert_eq!(verb_str, "new-window", "{intent:?}");
            assert_eq!(
                alias_route("aterm-gui", &verb_str),
                AliasRoute::AliasWindowVerb
            );
            let reparsed =
                aterm_cli::parse_window_request("new-window", rest, |raw| Ok(raw.to_owned()))
                    .expect("the sibling's grammar accepts the handoff");
            assert_eq!(reparsed.intent, aterm_cli::LaunchIntent::NewWindow);
            assert_eq!(reparsed.dir, dir, "{intent:?}: the directory travels");
            assert_eq!(
                aterm_cli::route_launch(
                    reparsed.intent,
                    aterm_cli::WindowingBehavior::Attach,
                    true
                ),
                aterm_cli::WindowRoute::Spawn,
                "attach with an instance up still spawns"
            );
            assert!(
                !aterm_cli::plain_launch_is_policy_eligible(&argv, aterm_cli::LaunchEnv::default()),
                "the handoff is never a plain launch the policy could forward"
            );
        }
    }
}

#[test]
fn a_bare_launch_inside_a_tab_opens_a_tab_and_anything_else_nests() {
    assert!(nests_inside_aterm(true, false, true, true));
    assert!(
        !nests_inside_aterm(false, false, true, true),
        "flags, a -e payload or a typo go to the session parser"
    );
    assert!(
        !nests_inside_aterm(true, true, true, true),
        "aterm-cli nests on purpose"
    );
    assert!(
        !nests_inside_aterm(true, false, false, true),
        "not inside aterm at all"
    );
    assert!(
        !nests_inside_aterm(true, false, true, false),
        "a piped launch is a harness and gets what it asked for"
    );
}

/// Which windowed image the console image hands a window to, against a real folder: the
/// shipped name in an install folder, only as a file; in a build tree (cargo's `.fingerprint`
/// beside the exe) the target name, never the `aterm-gui.exe` there (crate aterm-gui's thin
/// dev bin).
#[cfg(windows)]
#[test]
fn the_windowed_sibling_is_the_shipped_image_or_a_build_trees_own() {
    let scratch = scratch_dir("windowed-sibling");
    let dir = scratch.path();
    let exe = dir.join("aterm.exe");
    let found = || aterm_gui::windowed_front_door_beside(&exe);
    assert_eq!(found(), None);
    std::fs::create_dir_all(dir.join("aterm-gui.exe")).expect("a decoy directory");
    assert_eq!(found(), None, "a directory of that name is not an image");
    std::fs::remove_dir(dir.join("aterm-gui.exe")).expect("drop the decoy");
    std::fs::write(dir.join("aterm-gui.exe"), b"").expect("the shipped name");
    assert_eq!(found(), Some(dir.join("aterm-gui.exe")));
    std::fs::create_dir_all(dir.join(".fingerprint")).expect("a build tree");
    assert_eq!(
        found(),
        None,
        "a build tree's aterm-gui.exe is the thin dev bin, never the sibling"
    );
    std::fs::write(dir.join("aterm-windowed.exe"), b"").expect("the target name");
    assert_eq!(found(), Some(dir.join("aterm-windowed.exe")));
}

/// A build tree's windowed image stands for this build only when it is as new as this one,
/// give or take the two bins' link spread (measured 6 ms, either order): `cargo run -p aterm`
/// relinks the console image alone.
#[cfg(windows)]
#[test]
fn a_build_trees_windowed_image_older_than_this_one_is_not_used() {
    use std::time::{Duration, SystemTime};
    let now = SystemTime::now();
    let before = |secs: u64| now - Duration::from_secs(secs);
    assert!(
        !windowed_sibling::is_stale(now, now),
        "same instant: one build"
    );
    assert!(
        !windowed_sibling::is_stale(now - Duration::from_millis(6), now),
        "the measured link spread of one build"
    );
    assert!(
        !windowed_sibling::is_stale(now + Duration::from_secs(30), now),
        "newer than this image"
    );
    assert!(
        !windowed_sibling::is_stale(before(60), now),
        "within a minute"
    );
    assert!(
        windowed_sibling::is_stale(before(61), now),
        "past the spread"
    );
    assert!(
        windowed_sibling::is_stale(before(4 * 24 * 3600), now),
        "an earlier build's"
    );
}

/// After the call no std handle is inheritable, or a capturing shell's
/// `$o = & aterm --window` would hang until the window closed.
#[cfg(windows)]
#[test]
fn the_std_handles_are_not_inheritable_once_the_sibling_is_about_to_start() {
    windowed_sibling::stop_inheriting_std_handles();
    assert!(
        !windowed_sibling::a_std_handle_is_inheritable(),
        "a std handle would still be carried into the detached window"
    );
}

/// `RegisterApplicationRestart` fires after a Restart-Manager reboot and (`dwFlags = 0`)
/// after a crash or hang, and its command line comes back through this router.
#[cfg(windows)]
#[test]
fn the_os_restart_command_line_is_the_one_verb_policy_never_forwards() {
    let line = aterm_gui::OS_RESTART_COMMAND_LINE;
    let verb = aterm_cli::Verb::from_operand(line)
        .unwrap_or_else(|| panic!("{line:?} must be a routed front-door verb"));
    assert!(verb.is_windowing(), "{line:?}");
    for behavior in [
        aterm_cli::WindowingBehavior::NewWindow,
        aterm_cli::WindowingBehavior::Attach,
    ] {
        for reachable in [true, false] {
            assert_eq!(
                aterm_cli::route_launch(aterm_cli::LaunchIntent::NewWindow, behavior, reachable),
                aterm_cli::WindowRoute::Spawn,
                "an OS relaunch must come back as a WINDOW under {behavior:?}"
            );
        }
    }
    assert_eq!(verb, aterm_cli::Verb::NewWindow);
    assert_eq!(
        alias_route("aterm-gui", line),
        AliasRoute::AliasWindowVerb,
        "the relaunched image is current_exe(), normally aterm-gui.exe"
    );
}

#[test]
fn the_mode_flags_are_stripped_before_the_window_but_never_inside_a_payload() {
    let osv = |list: &[&str]| -> Vec<OsString> { list.iter().map(OsString::from).collect() };
    assert_eq!(strip_mode_flags(&osv(&["--window"])), osv(&[]));
    assert_eq!(
        strip_mode_flags(&osv(&["--window", "-d", "/tmp"])),
        osv(&["-d", "/tmp"])
    );
    assert_eq!(strip_mode_flags(&osv(&["--session"])), osv(&[]));
    assert_eq!(
        strip_mode_flags(&osv(&["--window", "-e", "sh", "--window"])),
        osv(&["-e", "sh", "--window"])
    );
    assert_eq!(payload_boundary(&osv(&["-d", "/tmp"])), 2);
    assert_eq!(payload_boundary(&osv(&["--window", "--", "x"])), 1);
    assert_eq!(
        strip_flags(&osv(&["--window", "--session"]), &["--window"]),
        osv(&["--session"])
    );
}

#[test]
fn no_reroute_is_stripped_before_dispatch_and_sets_the_marker() {
    let _env_lock = NO_REROUTE_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let osv = |list: &[&str]| -> Vec<OsString> { list.iter().map(OsString::from).collect() };
    let env = atpkg::reroute::PASSTHROUGH_ENV;
    let engaged = || atpkg::reroute::engaged(std::env::var(env).ok().as_deref());
    aterm_log::env::unset(env);
    aterm_log::env::set(env, "1");
    assert_eq!(take_no_reroute(&osv(&["--window"])), osv(&["--window"]));
    assert!(
        !engaged(),
        "no flag, no escape — whatever the environment carried"
    );
    assert_eq!(
        take_no_reroute(&osv(&["-e", "sh", "--no-reroute"])),
        osv(&["-e", "sh", "--no-reroute"])
    );
    assert!(!engaged(), "a payload token must not engage the escape");
    assert_eq!(
        take_no_reroute(&osv(&["--window", "--no-reroute", "-d", "/tmp"])),
        osv(&["--window", "-d", "/tmp"])
    );
    assert!(engaged(), "the flag must establish {env}=1");
    assert_eq!(
        plain_launch_request(
            &osv(&["--window", "--no-reroute"]),
            aterm_cli::LaunchEnv::default()
        ),
        None,
        "a --no-reroute launch fails closed to a local spawn"
    );
    let announced = atpkg::reroute::ANNOUNCED_ENV;
    for flags in [&["--window"][..], &["--window", "--no-reroute"][..]] {
        aterm_log::env::set(announced, "1");
        let _ = take_no_reroute(&osv(flags));
        assert!(
            std::env::var_os(announced).is_none(),
            "{flags:?}: an inherited {announced} would silence the session's signposts"
        );
    }
    aterm_log::env::unset(env);
}

fn scratch_dir(tag: &str) -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix(&format!("aterm-front-door-{tag}-"))
        .suffix(".noindex")
        .tempdir()
        .expect("scratch dir")
}

#[test]
fn the_agents_dir_is_handed_on_every_lane_regardless_of_the_reroute_switch() {
    let _env_lock = NO_REROUTE_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let scratch = scratch_dir("agents-handoff");
    let layout = atpkg::store::Layout {
        prefix: scratch.path().join("pkg"),
    };
    assert_eq!(agents_dir_handoff(None), None, "no layout, nothing handed");
    assert!(
        !layout.agents_dir().exists(),
        "a fresh prefix has no agents/"
    );
    let handed = agents_dir_handoff(Some(&layout)).expect("created, hence handed");
    assert_eq!(handed, layout.agents_dir().to_str().unwrap());
    assert!(std::path::Path::new(&handed).is_absolute());
    assert!(
        std::fs::symlink_metadata(&handed).unwrap().is_dir(),
        "a real directory"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&handed).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "a $HOME prefix's directory is private");
    }
    assert_eq!(
        agents_dir_handoff(Some(&layout)).as_deref(),
        Some(handed.as_str()),
        "a second call is a no-op with the same answer"
    );
    let env = atpkg::reroute::PASSTHROUGH_ENV;
    let before = std::env::var_os(env);
    for value in ["1", "yes"] {
        aterm_log::env::set(env, value);
        assert!(atpkg::reroute::engaged(std::env::var(env).ok().as_deref()));
        assert_eq!(
            agents_dir_handoff(Some(&layout)).as_deref(),
            Some(handed.as_str()),
            "{env}={value} must not drop the managed agents dir"
        );
    }
    match before {
        Some(v) => aterm_log::env::set(env, v),
        None => aterm_log::env::unset(env),
    }
    let var = atpkg::reroute::AGENTS_DIR_ENV;
    assert_eq!(var, "ATERM_AGENTS_DIR", "the contract's spelling");
    hand_agents_dir(Some(&layout));
    assert_eq!(std::env::var(var).ok().as_deref(), Some(handed.as_str()));
    hand_agents_dir(None);
    assert_eq!(
        std::env::var_os(var),
        None,
        "no layout ⇒ not set, not empty"
    );
}

/// A prefix that refuses the `mkdir`, or a symlink at `agents/`, hands nothing (said on
/// stderr), creates nothing, and `hand_agents_dir` removes an inherited value.
#[cfg(unix)]
#[test]
fn a_refused_agents_dir_hands_nothing_and_clears_an_inherited_stray() {
    use std::os::unix::fs::PermissionsExt as _;
    let _env_lock = NO_REROUTE_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let scratch_dir = scratch_dir("agents-refused");
    let scratch = scratch_dir.path();
    let var = atpkg::reroute::AGENTS_DIR_ENV;
    let real = scratch.join("real-agents");
    std::fs::create_dir_all(&real).unwrap();
    let linked = atpkg::store::Layout {
        prefix: scratch.join("linked"),
    };
    std::fs::create_dir_all(&linked.prefix).unwrap();
    std::os::unix::fs::symlink(&real, linked.agents_dir()).unwrap();
    assert!(linked.agents_dir().is_dir(), "the link resolves");
    assert_eq!(agents_dir_handoff(Some(&linked)), None);
    assert!(
        std::fs::symlink_metadata(linked.agents_dir())
            .unwrap()
            .file_type()
            .is_symlink(),
        "left alone"
    );
    let filed = atpkg::store::Layout {
        prefix: scratch.join("filed"),
    };
    std::fs::create_dir_all(&filed.prefix).unwrap();
    std::fs::write(filed.agents_dir(), b"not a dir").unwrap();
    assert_eq!(agents_dir_handoff(Some(&filed)), None);
    aterm_log::env::set(var, real.to_str().unwrap());
    hand_agents_dir(Some(&linked));
    assert_eq!(
        std::env::var_os(var),
        None,
        "an inherited stray must not survive a refusal"
    );
    // SAFETY: getuid() takes no arguments and cannot fail.
    if unsafe { libc::getuid() } == 0 {
        aterm_spec::host::could_not_run(
            "running as root, where a read-only prefix refuses nothing: the refusal leg cannot run",
        );
    }
    let layout = atpkg::store::Layout {
        prefix: scratch.join("pkg"),
    };
    std::fs::create_dir_all(&layout.prefix).unwrap();
    struct RestorePerms(std::path::PathBuf);
    impl Drop for RestorePerms {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
        }
    }
    let _restore = RestorePerms(layout.prefix.clone());
    std::fs::set_permissions(&layout.prefix, std::fs::Permissions::from_mode(0o500)).unwrap();
    assert_eq!(agents_dir_handoff(Some(&layout)), None);
    assert!(!layout.agents_dir().exists(), "nothing created");
    aterm_log::env::set(var, real.to_str().unwrap());
    hand_agents_dir(Some(&layout));
    assert_eq!(std::env::var_os(var), None);
    std::fs::set_permissions(&layout.prefix, std::fs::Permissions::from_mode(0o700)).unwrap();
    hand_agents_dir(Some(&layout));
    assert_eq!(
        std::env::var(var).ok().as_deref(),
        layout.agents_dir().to_str()
    );
    assert_eq!(
        std::fs::metadata(layout.agents_dir())
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o700
    );
    aterm_log::env::unset(var);
}

#[test]
fn front_door_verbs_and_completion_flags_have_no_duplicates() {
    let verbs = super::front_door_verbs();
    let mut seen_verbs = std::collections::BTreeSet::new();
    for &verb in &verbs {
        assert!(
            seen_verbs.insert(verb),
            "duplicate front-door completion verb: {verb:?}"
        );
    }
    assert!(seen_verbs.contains("help"));
    for verb in aterm_cli::Verb::ALL {
        assert!(seen_verbs.contains(verb.name()), "missing {}", verb.name());
    }
    for &(name, _) in aterm_cli::DIAG_COMMANDS {
        assert!(seen_verbs.contains(name), "missing diag {name}");
    }

    let mut seen_flags = std::collections::BTreeSet::new();
    for &(flag, desc) in super::COMPLETION_FLAGS {
        assert!(flag.starts_with("--"), "flag {flag:?} must start with --");
        assert!(!desc.is_empty(), "flag {flag:?} must carry a description");
        assert!(
            seen_flags.insert(flag),
            "duplicate front-door completion flag: {flag:?}"
        );
    }

    for (shell, wiring) in [
        ("bash", "complete -F _aterm aterm\n"),
        ("zsh", "#compdef aterm\n"),
        ("fish", "complete -c aterm -f\n"),
    ] {
        let script =
            aterm_ctl::front_door_completion_script(shell, &verbs, super::COMPLETION_FLAGS)
                .unwrap_or_else(|| panic!("{shell} script"));
        if shell == "zsh" {
            assert!(script.starts_with(wiring), "zsh must start with {wiring:?}");
        } else {
            assert!(script.contains(wiring), "{shell} missing {wiring:?}");
        }
        for &verb in &verbs {
            assert!(script.contains(verb), "{shell} missing verb {verb:?}");
        }
        for &(flag, _) in super::COMPLETION_FLAGS {
            let want = if shell == "fish" {
                flag.trim_start_matches('-')
            } else {
                flag
            };
            assert!(script.contains(want), "{shell} missing flag {want:?}");
        }
    }
    assert!(
        aterm_ctl::front_door_completion_script("tcsh", &verbs, super::COMPLETION_FLAGS).is_none()
    );
}
