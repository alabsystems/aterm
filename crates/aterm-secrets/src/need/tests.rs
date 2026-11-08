// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::client::{self, Paths};
use aterm_spec::interp::State as ModelState;

const SEQ: u64 = 68;
const HOST: &str = "andrews";

fn want_of(kind: i64) -> Want {
    match kind {
        0 => Want::SiteDeployerKey,
        1 => Want::PaperMaster,
        2 => Want::Login("claude/m29".into()),
        _ => Want::Mailbox("m29".into()),
    }
}

fn purpose_of(want: &Want) -> Purpose {
    match want.class() {
        Class::Copyable => Purpose::ReleaseSiteKey,
        Class::VaultOnly => Purpose::HarnessSignIn,
        _ => Purpose::ByHand,
    }
}

const STATES: [State; 7] = [
    State::Local,
    State::Open,
    State::Offered,
    State::Published,
    State::Fulfilled,
    State::Retired,
    State::AwaitingPerson,
];

const WAITS: [Wait; 6] = [
    Wait::Nobody,
    Wait::Holders,
    Wait::Custodian,
    Wait::Person,
    Wait::Requester,
    Wait::Founding,
];

const REASONS: [Reason; 8] = [
    Reason::NeverStored,
    Reason::NeverCopied,
    Reason::NotAuthorized,
    Reason::NotNeeded,
    Reason::NotHeld,
    Reason::Declined,
    Reason::RequesterGone,
    Reason::Lapsed,
];

fn st_of(state: State) -> i64 {
    STATES
        .iter()
        .position(|s| *s == state)
        .map_or(0, |at| at as i64 + 1)
}

fn wait_num(wait: Wait) -> i64 {
    WAITS.iter().position(|w| *w == wait).unwrap() as i64
}

fn why_num(reason: Reason) -> i64 {
    REASONS
        .iter()
        .position(|r| *r == reason)
        .map_or(0, |at| at as i64 + 1)
}

fn kind_num(class: Class) -> i64 {
    match class {
        Class::Copyable => 0,
        Class::Nowhere => 1,
        Class::PerMac => 2,
        Class::VaultOnly | Class::AdminAct => 3,
    }
}

fn facts_of(s: &ModelState) -> Facts {
    let want = want_of(s["kind"]);
    let held = if s["inv"] == 1 {
        want.entry().into_iter().collect()
    } else {
        BTreeSet::new()
    };
    Facts {
        sight: Some(Sight {
            version: None,
            vault: "alab".into(),
            host: HOST.into(),
            seq: SEQ,
            signer: [7; 32],
            alab: true,
            hidden: s["hidden"] == 1,
            held,
        }),
        commons: if s["commons"] == 1 {
            Commons::Writer
        } else {
            Commons::Unavailable(Unavailable::ControlNotVisible)
        },
        policy: Policy {
            generation: u64::try_from(s["gen"]).unwrap(),
            members_may_have: s["auth"] == 1,
        },
        offered: s["held"] == 1,
        now: 1_000_000,
    }
}

fn need_of(s: &ModelState) -> Option<Need> {
    if s["st"] == 0 {
        return None;
    }
    let want = want_of(s["kind"]);
    let purpose = purpose_of(&want);
    let asking = s["you"] == 1;
    Some(Need {
        id: [1; 16],
        ask: asking.then(|| ask_for(&want, purpose, HOST)).flatten(),
        want,
        purpose,
        state: STATES[usize::try_from(s["st"] - 1).unwrap()],
        wait: WAITS[usize::try_from(s["wait"]).unwrap()],
        retired: (s["why"] > 0).then(|| Retirement {
            reason: REASONS[usize::try_from(s["why"] - 1).unwrap()],
            evidence: String::new(),
            note: String::new(),
            generation: Some(u64::try_from(s["atgen"]).unwrap()),
        }),
        raised: 1,
        updated: 1,
        raises: u32::try_from(s["raises"]).unwrap(),
        refused: None,
        needed: s["need"] == 1,
        asking,
        proof: (s["askok"] == 1).then_some(Proof {
            seq: SEQ,
            absent: true,
            every_locker_open: true,
            before_commons: true,
            swept_unheld: false,
            window_ended: false,
            custodian_silent: false,
        }),
        got: s["got"] == 1,
        got_authorized: s["pa"] == 1,
        raised_again_on_change: (s["raises"] == 2).then_some(s["okr"] == 1),
        posted: s["posted"] == 1,
        custodian_silent: false,
        late_offer: false,
        record: None,
        line: String::new(),
        seq: Some(SEQ),
    })
}

/// The model state the real need `real`, reached from `s` by `action`, projects to.
fn project(s: &ModelState, action: &str, before: Option<&Need>, real: &Need) -> ModelState {
    let mut next = s.clone();
    let set = |next: &mut ModelState, var: &'static str, value: i64| {
        next.insert(var, value);
    };
    set(&mut next, "st", st_of(real.state));
    set(
        &mut next,
        "why",
        real.retired.as_ref().map_or(0, |r| why_num(r.reason)),
    );
    set(&mut next, "kind", kind_num(real.want.class()));
    set(&mut next, "wait", wait_num(real.wait));
    set(&mut next, "you", i64::from(real.asking));
    set(&mut next, "raises", i64::from(real.raises.min(2)));
    set(&mut next, "need", i64::from(real.needed));
    set(&mut next, "got", i64::from(real.got));
    set(&mut next, "pa", i64::from(real.got_authorized));
    set(&mut next, "posted", i64::from(real.posted));
    if let Some(proof) = &real.proof {
        set(&mut next, "askok", i64::from(proof.warrants_asking()));
    }
    if let Some(changed) = real.raised_again_on_change {
        set(&mut next, "okr", i64::from(changed));
    }
    if let Some(generation) = real.retired.as_ref().and_then(|r| r.generation)
        && real.retired.as_ref().map(|r| r.reason) == Some(Reason::NotAuthorized)
    {
        set(&mut next, "atgen", i64::try_from(generation).unwrap());
    }
    if before.is_none_or(|before| before.id != real.id) {
        // A new request id: the lab holds no sweep and no Still of it yet.
        set(&mut next, "swept", 0);
        set(&mut next, "quiet", 0);
    }
    if action.starts_with("Raise")
        && real.want.class() == Class::VaultOnly
        && real.want.person_sourced()
    {
        set(&mut next, "person", 1);
    }
    match action {
        "Give" => set(&mut next, "inv", 1),
        "CommonsReady" => set(&mut next, "commons", 1),
        "Flush" => set(&mut next, "swept", 0),
        _ => {}
    }
    next
}

/// The real move the model's `action` stands for, from `s`: `None` when the real guard does not
/// hold. The person's give and Commons becoming ready are the environment's steps, which this
/// Mac observes: the fixture takes them, then runs the move that observes them.
pub(crate) fn request_transition(s: &ModelState, action: &str) -> Option<ModelState> {
    let mut facts = facts_of(s);
    let before = need_of(s);
    let raise = |kind| {
        (s["st"] == 0).then(|| {
            let want = want_of(kind);
            fresh([2; 16], want.clone(), purpose_of(&want), &facts)
        })
    };
    let real = match action {
        "Raise" => raise(0),
        "RaiseNowhere" => raise(1),
        "RaisePerMachine" => raise(2),
        "RaiseVaultOnly" => raise(3),
        "Give" => {
            if let Some(sight) = facts.sight.as_mut() {
                sight.held.extend(want_of(s["kind"]).entry());
            }
            act(before.as_ref()?, &facts, &Act::Give)
        }
        "CommonsReady" => {
            if s["commons"] == 1 {
                return None;
            }
            facts.commons = Commons::Writer;
            match &before {
                Some(need) => {
                    Some(act(need, &facts, &Act::Refresh).unwrap_or_else(|| need.clone()))
                }
                None => {
                    let mut next = s.clone();
                    next.insert("commons", 1);
                    return Some(next);
                }
            }
        }
        _ => {
            let step = match action {
                "Catalog" => Act::Catalog,
                "Absent" => Act::Absent,
                "Unauthorized" => Act::Unauthorized,
                "Apply" => Act::Apply,
                "Unneeded" => Act::Unneeded,
                "Decline" => Act::Decline("not now".into()),
                "NeedAgain" => Act::NeedAgain,
                "Reraise" => Act::Reraise,
                "Flush" => Act::Flush,
                "Post" => Act::Post,
                _ => panic!("{action} is not a ledger move"),
            };
            act(before.as_ref()?, &facts, &step)
        }
    }?;
    Some(project(s, action, before.as_ref(), &real))
}

/// [`request_transition`] with the model's `Unneeded` taken by a person's withdrawal by hand
/// rather than by the consumer's own check: both are the model's one `Unneeded`.
fn hand_transition(s: &ModelState, action: &str) -> Option<ModelState> {
    if action != "Unneeded" {
        return request_transition(s, action);
    }
    let before = need_of(s)?;
    let real = act(&before, &facts_of(s), &Act::Withdraw("not now".into()))?;
    Some(project(s, action, Some(&before), &real))
}

const BOUND: [&str; 16] = [
    "Raise",
    "RaiseNowhere",
    "RaisePerMachine",
    "RaiseVaultOnly",
    "Catalog",
    "Absent",
    "Unauthorized",
    "Give",
    "Apply",
    "Unneeded",
    "Decline",
    "NeedAgain",
    "Reraise",
    "CommonsReady",
    "Flush",
    "Post",
];

/// Every ledger move, from every reachable state of the model, lands on one of the model's
/// successors, and the real guard holds exactly where the model's does (but past the model's
/// box: a third raise, which `raises <= 2` keeps out of the walk).
fn conformance_failures(
    model: &aterm_spec::derive::Model,
    transition: impl Fn(&ModelState, &str) -> Option<ModelState>,
) -> Vec<String> {
    let mut failures = Vec::new();
    for state in aterm_spec::interp::reachable_states(model) {
        for action in BOUND {
            let enabled = model.action_enabled(action, &state);
            match transition(&state, action) {
                Some(next) => {
                    if action == "Reraise" && state["raises"] == 2 {
                        continue;
                    }
                    if !enabled || !model.successors(action, &state).contains(&next) {
                        failures.push(format!("{action} from {state:?} reached {next:?}"));
                    }
                }
                None if enabled => failures.push(format!("{action} refused from {state:?}")),
                None => {}
            }
        }
    }
    failures
}

#[test]
fn the_need_ledger_conforms_to_the_credential_request_model() {
    let model = aterm_models_db::credential_request_model();
    for transition in [request_transition, hand_transition] {
        let failures = conformance_failures(&model, transition);
        assert!(
            failures.is_empty(),
            "{} failures, first: {:?}",
            failures.len(),
            failures.first()
        );
    }
    let fired = aterm_spec::interp::fired_actions(&model);
    for action in BOUND {
        assert!(fired.contains(action), "{action} never fires in the model");
    }
    // A hand mutant of the real code the projection refuses: a decline that leaves the NEEDS
    // YOU up.
    let decline_keeps_asking = |s: &ModelState, action: &str| {
        let next = request_transition(s, action)?;
        if action == "Decline" {
            let mut kept = next;
            kept.insert("you", 1);
            return Some(kept);
        }
        Some(next)
    };
    assert!(!conformance_failures(&model, decline_keeps_asking).is_empty());
    // And one of the withdrawal by hand: it leaves the consumer's word that it still needs it.
    let withdrawal_keeps_need = |s: &ModelState, action: &str| {
        let mut next = hand_transition(s, action)?;
        if action == "Unneeded" {
            next.insert("need", 1);
        }
        Some(next)
    };
    assert!(!conformance_failures(&model, withdrawal_keeps_need).is_empty());
}

#[test]
fn the_catalog_answers_what_no_mac_can_give_and_names_every_want_once() {
    for word in [
        "site-deployer-key",
        "notary",
        "mailbox:m29",
        "mailbox:m29-2",
        "gh:dev",
        "gh:release",
        "entry:~/company-life/config/local/service-account.alab-site-deployer.json",
        "paper-master",
        "login:claude/m29",
        "browser-profile:m29",
        "refresh-token:codex/work",
        "vault-identity",
        "commons-device-key",
        "commons-control",
    ] {
        assert_eq!(Want::parse(word).unwrap().key(), word);
    }
    for bad in [
        "mailbox:",
        "mailbox:M29",
        "gh:other",
        "entry:~/.secrets/x",
        "secret",
        "login:m29",
    ] {
        assert!(Want::parse(bad).is_err(), "{bad}");
    }
    assert_eq!(Want::PaperMaster.class(), Class::Nowhere);
    assert_eq!(Want::Login("claude/m29".into()).class(), Class::PerMac);
    assert_eq!(Want::SiteDeployerKey.class(), Class::Copyable);
    assert_eq!(Want::Mailbox("m29".into()).class(), Class::VaultOnly);
    assert_eq!(Want::CommonsControl.class(), Class::AdminAct);
    assert!(!Want::Entry("notary".into()).posted());
    assert!(!Want::PaperMaster.posted() && !Want::CommonsControl.posted());
    // The deployer key's entry is a valid home-path entry; a path into ~/.secrets is not one.
    assert!(crate::text::valid_entry_name(
        catalog::SITE_DEPLOYER_KEY_ENTRY
    ));
    assert_eq!(catalog::SITE_DEPLOYER_KEY_ENTRY.len(), 67);
    assert!(!crate::text::valid_entry_name("~/.secrets/x"));
    // The ledger's own folder is one no entry can name.
    assert!(!crate::text::valid_entry_name(
        "~/.config/aterm/secrets/needs.toml"
    ));
}

#[test]
fn mailbox_component_wants_are_exact_and_preserve_legacy_requests() {
    use crate::mailbox_component::{Component, Part};
    let component = Component::for_mailbox(
        "https://mail.example",
        "owner@example",
        Part::SecondPassword,
    )
    .unwrap();
    let want = Want::MailboxComponent(component);
    assert_eq!(Want::parse(&want.key()).unwrap(), want);
    assert_eq!(want.entries(), vec![component.entry()]);
    assert_eq!(want.class(), Class::VaultOnly);
    assert!(want.posted() && want.person_sourced());
    assert_eq!(
        Want::Mailbox("m29".into()).entries(),
        vec!["m29_password", "m29_totp"]
    );
    let ask = ask_for(&want, Purpose::HarnessSignIn, HOST).unwrap();
    assert!(!ask.needs_you.contains("Claude"));
    let mut need = fresh([3; 16], want, Purpose::HarnessSignIn, &bare_facts());
    need.state = State::AwaitingPerson;
    need.asking = true;
    need.ask = Some(ask);
    assert!(exception(&need).unwrap().message().is_ok());
}

#[test]
fn the_hold_probe_names_only_the_catalog_path_whatever_the_environment_says() {
    let home = std::path::Path::new("/Users//someone");
    assert_eq!(
        Want::SiteDeployerKey.hold_path(home).unwrap(),
        home.join("company-life/config/local/service-account.alab-site-deployer.json")
    );
    // Nothing in this crate reads FIREBASE_SA_KEY: the probe is the catalog's one path.
    let source = include_str!("catalog.rs");
    assert!(!source.contains(&["FIREBASE", "_SA_KEY"].concat()));
    assert!(Want::Gh(GhProfile::Dev).hold_path(home).is_none());
    assert!(Want::PaperMaster.hold_path(home).is_none());
}

#[test]
fn every_ask_passes_the_exceptions_entrys_own_rules() {
    let named = Unanswered {
        hosts: vec![
            "m3".into(),
            "a-very-long-vault-host-name-for-another-mac".into(),
        ],
        others: 2,
    };
    let unnamed = Unanswered {
        hosts: Vec::new(),
        others: 1,
    };
    let nobody = Unanswered::default();
    let heard = [
        Heard::Nothing,
        Heard::Unanswered(&named),
        Heard::Unanswered(&unnamed),
        Heard::Offered(&nobody),
        Heard::Offered(&named),
    ];
    for ((want, purpose, host), heard) in [
        (Want::Mailbox("m29".into()), Purpose::HarnessSignIn, HOST),
        (
            Want::Mailbox("m29-2".into()),
            Purpose::HarnessSignIn,
            "m100",
        ),
        (Want::Gh(GhProfile::Dev), Purpose::AghDev, HOST),
        (Want::Gh(GhProfile::Release), Purpose::AghRelease, HOST),
        (
            Want::Mailbox("a-very-long-mailbox-label-for-one-account".into()),
            Purpose::ByHand,
            "a-very-long-vault-host-name-for-a-mac",
        ),
        (Want::Notary, Purpose::ReleaseNotary, HOST),
        (Want::SiteDeployerKey, Purpose::ReleaseSiteKey, HOST),
    ]
    .into_iter()
    .flat_map(|case| heard.iter().map(move |heard| (case.clone(), *heard)))
    {
        let ask = ask_on(&want, purpose, host, heard).unwrap();
        let mut need = fresh([3; 16], want, purpose, &bare_facts());
        need.state = State::AwaitingPerson;
        need.asking = true;
        need.ask = Some(ask.clone());
        let entry = exception(&need).unwrap();
        entry
            .message()
            .unwrap_or_else(|fault| panic!("{ask:?}: {fault:?}"));
        assert!(ask.what.chars().count() <= aterm_messages::exceptions::WHAT_CAP);
        assert!(
            aterm_messages::need::Step::new(&ask.step).fault().is_none(),
            "{}",
            ask.step
        );
        assert!(
            ask.step.chars().count() <= aterm_messages::need::STEP_CAP,
            "{}",
            ask.step
        );
        assert!(
            aterm_messages::need::Reason::new(&ask.reason)
                .fault()
                .is_none()
                && ask.reason.chars().count() <= aterm_messages::need::REASON_CAP,
            "{}",
            ask.reason
        );
        assert!(ask.needs_you.starts_with("NEEDS YOU: ") && !ask.needs_you.contains('\n'));
        assert!(entry.key().starts_with("secrets.need."), "{}", entry.key());
        // The fact keeps three detail lines: every line the ask has reaches it, the source last.
        assert!(
            ask.more.len() <= aterm_messages::exceptions::MORE_LINES
                && ask
                    .more
                    .last()
                    .is_some_and(|line| line.starts_with("it comes from:")),
            "{heard:?}: {:?}",
            ask.more
        );
    }
    // The line names the one command that works today, for this Mac's vault host, with the
    // value from a file and never an argument.
    let ask = ask_for(&Want::Mailbox("m29".into()), Purpose::HarnessSignIn, HOST).unwrap();
    assert!(
        ask.needs_you
            .contains("aterm secrets set m29_password --for andrews --file <file>"),
        "{}",
        ask.needs_you
    );
    assert!(ask.needs_you.contains("m29_totp"), "{}", ask.needs_you);
}

/// A want's ask is filed whatever row for it comes first: another purpose's row that asks
/// nothing (retired, or still waiting) never hides it.
#[test]
fn a_wants_ask_is_filed_whatever_row_for_it_comes_first() {
    let facts = facts_of(&model_state(&[("kind", 3)]));
    let want = Want::Mailbox("m29".into());
    let withdrawn = act(
        &fresh([1; 16], want.clone(), Purpose::ByHand, &facts),
        &facts,
        &Act::Unneeded,
    )
    .unwrap();
    let asking = settle(
        &fresh([2; 16], want, Purpose::HarnessSignIn, &facts),
        &facts,
    );
    assert!(!withdrawn.asking && asking.asking);
    let rows = [withdrawn, asking];
    assert_eq!(exceptions(&rows).len(), 1);
    assert_eq!(needs_you(&rows).len(), 1);
}

/// A need no consumer checks for itself (one raised by hand) is fulfilled once the version
/// this Mac applied gives it the entry, so it never stays published for good; a consumer that
/// checks for itself still decides its own.
#[test]
fn a_need_no_consumer_checks_is_fulfilled_by_its_delivery() {
    let mut facts = facts_of(&model_state(&[("kind", 3)]));
    let want = Want::Mailbox("m29".into());
    let asking = settle(
        &fresh([7; 16], want.clone(), Purpose::ByHand, &facts),
        &facts,
    );
    assert_eq!(asking.state, State::AwaitingPerson);
    assert!(asking.asking);
    let published = |purpose, facts: &Facts| Need {
        state: State::Published,
        wait: Wait::Requester,
        got: true,
        got_authorized: true,
        posted: true,
        ..fresh([6; 16], want.clone(), purpose, facts)
    };
    let waiting = settle(&published(Purpose::ByHand, &facts), &facts);
    assert_eq!(waiting.state, State::Published, "not applied yet");
    if let Some(sight) = facts.sight.as_mut() {
        sight.held.insert("m29_password".into());
    }
    let done = settle(&published(Purpose::ByHand, &facts), &facts);
    assert_eq!(done.state, State::Fulfilled, "{}", done.line);
    let from_person = settle(&asking, &facts);
    assert_eq!(from_person.state, State::Fulfilled, "{}", from_person.line);
    assert!(!from_person.asking && from_person.ask.is_none());
    let own = settle(&published(Purpose::HarnessSignIn, &facts), &facts);
    assert_eq!(
        own.state,
        State::Published,
        "the ladder's own check decides"
    );
}

#[test]
fn decided_checks_policy_and_refuses_asking_when_already_held_and_absorb_settles_delivery() {
    let mut facts = facts_of(&model_state(&[("kind", 2)]));
    let want = Want::SiteDeployerKey;
    let open = Need {
        state: State::Open,
        wait: Wait::Holders,
        posted: true,
        ..fresh([8; 16], want.clone(), Purpose::ByHand, &facts)
    };
    let mut unauthorized = facts.clone();
    unauthorized.policy = Policy::compiled(false);
    assert!(
        decided(
            &open,
            &unauthorized,
            &Decision::Offered(Unanswered::default())
        )
        .is_none()
    );
    assert!(
        decided(
            &open,
            &unauthorized,
            &Decision::Published {
                version: RefusalClaim::LegacySequence(9)
            }
        )
        .is_none()
    );
    assert!(
        decided(
            &open,
            &unauthorized,
            &Decision::NeedsPerson(Unanswered::default())
        )
        .is_none()
    );
    for (floor, seq) in [(7, 6), (7, 7), (u64::MAX, u64::MAX)] {
        let refused = Need {
            refused: Some(RefusalClaim::LegacySequence(floor)),
            ..open.clone()
        };
        assert!(
            decided(
                &refused,
                &facts,
                &Decision::Published {
                    version: RefusalClaim::LegacySequence(seq)
                }
            )
            .is_none()
        );
    }
    let refused = Need {
        refused: Some(RefusalClaim::LegacySequence(7)),
        ..open.clone()
    };
    let newer = decided(
        &refused,
        &facts,
        &Decision::Published {
            version: RefusalClaim::LegacySequence(8),
        },
    )
    .unwrap();
    assert_eq!((newer.state, newer.seq), (State::Published, Some(8)));

    let asking = Need {
        state: State::AwaitingPerson,
        refused: Some(RefusalClaim::LegacySequence(SEQ)),
        ..open.clone()
    };
    let mut given = facts.clone();
    given
        .sight
        .as_mut()
        .unwrap()
        .held
        .extend(asking.want.entry());
    for seq in [SEQ - 1, SEQ] {
        given.sight.as_mut().unwrap().seq = seq;
        assert!(act(&asking, &given, &Act::Give).is_none());
    }
    given.sight.as_mut().unwrap().seq = SEQ + 1;
    let newer = act(&asking, &given, &Act::Give).unwrap();
    assert_eq!((newer.state, newer.seq), (State::Published, Some(SEQ + 1)));

    let mut held_facts = facts_of(&model_state(&[("kind", 3)]));
    if let Some(sight) = held_facts.sight.as_mut() {
        sight.held.insert("m29_password".into());
    }
    let mailbox_open = Need {
        state: State::Open,
        wait: Wait::Custodian,
        posted: true,
        ..fresh(
            [10; 16],
            Want::Mailbox("m29".into()),
            Purpose::HarnessSignIn,
            &held_facts,
        )
    };
    assert!(
        decided(
            &mailbox_open,
            &held_facts,
            &Decision::NeedsPerson(Unanswered::default())
        )
        .is_none(),
        "never raises NEEDS YOU when the entry is already held"
    );

    if let Some(sight) = facts.sight.as_mut() {
        sight
            .held
            .insert(catalog::SITE_DEPLOYER_KEY_ENTRY.to_owned());
    }
    let published = decided(
        &open,
        &facts,
        &Decision::Published {
            version: RefusalClaim::LegacySequence(SEQ),
        },
    )
    .unwrap();
    assert_eq!(settle(&published, &facts).state, State::Fulfilled);
}

/// A decision the want window's end made rests on fewer sweeps than every member's, so its proof
/// says so and, for a want a member can hold, the ask names who did not answer; a want no member
/// can hold keeps the words every member's sweep gives. A late offer moves an ask to the holder's
/// copy once, naming who the window's end named, and keeps it up.
/// NEGATIVE CONTROL: a decision every member's sweep made keeps the words it had; a refusing
/// policy takes no late offer; an offer on an open request is the plain offer.
#[test]
fn a_window_decision_and_a_late_offer_say_what_the_lab_heard() {
    let facts = facts_of(&model_state(&[("kind", 0), ("commons", 1)]));
    let open = |want: Want| Need {
        state: State::Open,
        wait: Wait::Holders,
        posted: true,
        ..fresh([14; 16], want, Purpose::ByHand, &facts)
    };
    let window = Decision::NeedsPerson(Unanswered {
        hosts: vec!["m3".into(), "m4".into()],
        others: 2,
    });
    let notary = decided(&open(Want::Notary), &facts, &window).unwrap();
    let proof = notary.proof.unwrap();
    assert!(
        proof.window_ended && !proof.swept_unheld && proof.warrants_asking(),
        "{proof:?}"
    );
    assert!(
        notary
            .line
            .contains("m3, m4 and 2 other Macs did not answer"),
        "{}",
        notary.line
    );
    let gh = Want::Gh(GhProfile::Dev);
    let token = decided(&open(gh.clone()), &facts, &window).unwrap();
    assert!(token.proof.is_some_and(|proof| proof.window_ended));
    assert_eq!(
        token.ask,
        ask_for(&gh, Purpose::ByHand, HOST),
        "no member's sweep can offer a token"
    );
    let every = Decision::NeedsPerson(Unanswered::default());
    let swept = decided(&open(Want::Notary), &facts, &every).unwrap();
    assert!(
        swept
            .proof
            .is_some_and(|proof| proof.swept_unheld && !proof.window_ended)
    );
    assert_eq!(swept.ask, ask_for(&Want::Notary, Purpose::ByHand, HOST));

    let late = Decision::Offered(Unanswered {
        hosts: vec!["m3".into()],
        others: 0,
    });
    let moved = decided(&notary, &facts, &late).unwrap();
    assert_eq!(
        (moved.state, moved.wait, moved.asking, moved.late_offer),
        (State::AwaitingPerson, Wait::Person, true, true)
    );
    assert!(
        moved
            .line
            .contains("offered it (m3 had not answered when the window ended)")
            && moved
                .line
                .contains("the vault custodian's running aterm takes"),
        "the window named who it is, and the runtime takes the copy: {}",
        moved.line
    );
    assert_eq!(moved.proof, notary.proof, "the ask's warrant stands");
    assert!(decided(&moved, &facts, &late).is_none(), "once");
    let mut refused = facts.clone();
    refused.policy = Policy::compiled(false);
    assert!(decided(&notary, &refused, &late).is_none());
    let offered = decided(&open(Want::Notary), &facts, &late).unwrap();
    assert_eq!((offered.state, offered.asking), (State::Offered, false));
}

/// Past the silence with no custodian record, a posted person-sourced vault-only want goes to a
/// person only on this Mac's absence proof (every locker but `admin` open, the entry in none, the
/// policy giving it); any other open request stays open, says the admin's aterm is silent and
/// files the one fact for later. Each pass reads it again: a sight that comes back, or a locker
/// that opens, moves an open one on to the person, and a value that reaches this Mac's own
/// section (the hand route, run on the admin Mac) delivers it, so a need no consumer checks is
/// fulfilled and the fact comes down. A pass that sees what the last one saw moves nothing.
/// NEGATIVE CONTROL: a hidden locker, a missing sight, a refusing policy and a copyable want stay
/// open; a value the consumer refused at the version this Mac holds is not a delivery; a need
/// not open or offered on the table moves nothing.
#[test]
fn a_silent_custodian_hands_over_only_what_the_absence_proof_covers() {
    let facts = facts_of(&model_state(&[("kind", 3), ("commons", 1)]));
    let posted = |want: Want, purpose| Need {
        state: State::Open,
        wait: Wait::Holders,
        posted: true,
        ..fresh([12; 16], want, purpose, &facts)
    };
    let mailbox = posted(Want::Mailbox("m29".into()), Purpose::HarnessSignIn);
    let asked = silent(&mailbox, &facts).unwrap();
    assert_eq!(
        (asked.state, asked.wait),
        (State::AwaitingPerson, Wait::Person)
    );
    assert!(asked.asking && asked.proof.is_some_and(|proof| proof.warrants_asking()));
    assert_eq!(exceptions(std::slice::from_ref(&asked)).len(), 1);
    assert!(silent(&asked, &facts).is_none(), "asked once");

    let mut hidden = facts.clone();
    if let Some(sight) = hidden.sight.as_mut() {
        sight.hidden = true;
    }
    let mut blind = facts.clone();
    blind.sight = None;
    let mut refused = facts.clone();
    refused.policy = Policy::compiled(false);
    let notary = posted(Want::Notary, Purpose::ByHand);
    for (need, facts) in [
        (&mailbox, &hidden),
        (&mailbox, &blind),
        (&mailbox, &refused),
        (&notary, &facts),
    ] {
        let said = silent(need, facts).unwrap();
        assert_eq!((said.state, said.wait), (State::Open, Wait::Custodian));
        assert!(!said.asking && said.proof.is_none(), "{said:?}");
        assert!(said.line.contains("96 hours"), "{}", said.line);
        let filed = exceptions(std::slice::from_ref(&said));
        assert_eq!(filed.len(), 1);
        assert_eq!(filed[0].key(), "secrets.need.custodian.silent");
        filed[0].message().unwrap();
        assert!(silent(&said, facts).is_none(), "said once");
    }
    for unsighted in [&hidden, &blind] {
        let said = silent(&mailbox, unsighted).unwrap();
        let later = silent(&said, &facts).unwrap();
        assert_eq!(
            (later.state, later.wait),
            (State::AwaitingPerson, Wait::Person),
            "a later pass that sees the whole vault asks: {}",
            later.line
        );
    }

    let mut held = facts.clone();
    if let Some(sight) = held.sight.as_mut() {
        sight.held.insert(catalog::NOTARY_ENTRY.to_owned());
    }
    let said = silent(&notary, &facts).unwrap();
    let delivered = settle(&silent(&said, &held).unwrap(), &held);
    assert_eq!(delivered.state, State::Fulfilled, "{}", delivered.line);
    assert!(exceptions(std::slice::from_ref(&delivered)).is_empty());
    let offered = Need {
        state: State::Offered,
        ..notary.clone()
    };
    assert_eq!(
        silent(&offered, &held).map(|need| need.state),
        Some(State::Published)
    );
    assert!(
        silent(&offered, &facts).is_none(),
        "an offer is its own line"
    );
    let at_refused = Need {
        refused: Some(RefusalClaim::LegacySequence(SEQ)),
        ..notary.clone()
    };
    assert_eq!(
        silent(&at_refused, &held).map(|need| need.state),
        Some(State::Open),
        "the refused value is no delivery"
    );

    let local = fresh([13; 16], Want::Notary, Purpose::ByHand, &facts);
    let unposted = Need {
        posted: false,
        ..notary.clone()
    };
    assert!(silent(&local, &facts).is_none());
    assert!(silent(&unposted, &facts).is_none());
}

/// The silent custodian's fact keeps three detail lines: past two requests it names the first
/// and counts the rest, so none is cut without a word.
#[test]
fn the_silent_custodians_fact_counts_what_its_lines_cannot_hold() {
    let mut facts = facts_of(&model_state(&[("kind", 3), ("commons", 1)]));
    if let Some(sight) = facts.sight.as_mut() {
        sight.hidden = true;
    }
    let said = |id: u8, want: Want| {
        let need = Need {
            state: State::Open,
            posted: true,
            ..fresh([id; 16], want, Purpose::ByHand, &facts)
        };
        silent(&need, &facts).unwrap()
    };
    let two = [said(1, Want::Notary), said(2, Want::SiteDeployerKey)];
    let three = [
        said(1, Want::Notary),
        said(2, Want::SiteDeployerKey),
        said(3, Want::Mailbox("m29".into())),
    ];
    let lines = |needs: &[Need]| {
        let filed = exceptions(needs);
        assert_eq!(filed.len(), 1);
        format!("{:?}", filed[0].message().unwrap())
    };
    let shown = lines(&two);
    assert!(
        shown.contains("notary: ") && shown.contains("site-deployer-key: "),
        "{shown}"
    );
    let counted = lines(&three);
    assert!(
        counted.contains("notary: ")
            && counted.contains("2 more: aterm secrets need ls names each"),
        "{counted}"
    );
}

/// Of a want's rows that ask, the person sees the one whose ask rests on the most the lab heard:
/// a copy a Mac offered, then who did not answer, before an ask that says no Mac holds it,
/// whichever row came first.
/// NEGATIVE CONTROL: rows that heard the same keep the oldest, and another want's row is its own.
#[test]
fn a_wants_ask_is_the_one_that_heard_the_most() {
    let facts = facts_of(&model_state(&[("kind", 0), ("commons", 1)]));
    let open = |id: u8, purpose| Need {
        state: State::Open,
        wait: Wait::Holders,
        posted: true,
        ..fresh([id; 16], Want::Notary, purpose, &facts)
    };
    let every = Decision::NeedsPerson(Unanswered::default());
    let window = Decision::NeedsPerson(Unanswered {
        hosts: vec!["m3".into()],
        others: 0,
    });
    let swept = decided(&open(1, Purpose::ReleaseNotary), &facts, &every).unwrap();
    let unheard = decided(&open(2, Purpose::ByHand), &facts, &window).unwrap();
    let late = Decision::Offered(Unanswered::default());
    let copy = decided(&unheard, &facts, &late).unwrap();
    let said = |rows: &[Need]| needs_you(rows).join("\n");
    assert!(said(&[swept.clone(), unheard.clone()]).contains("m3 did not answer"));
    assert!(said(&[swept.clone(), copy.clone()]).contains("offered it"));
    assert!(said(&[unheard.clone(), copy.clone()]).contains("offered it"));
    assert_eq!(exceptions(&[swept.clone(), copy.clone()]).len(), 1);
    let same = Need {
        id: [3; 16],
        purpose: Purpose::AghDev,
        ..swept.clone()
    };
    assert_eq!(
        needs_you(&[swept.clone(), same]),
        needs_you(std::slice::from_ref(&swept))
    );
    let other = Need {
        want: Want::SiteDeployerKey,
        ..swept.clone()
    };
    assert_eq!(needs_you(&[copy, other]).len(), 2);
}

/// An ask that rests on less than the lab now says is restated, once: one this Mac made standing
/// in for a silent custodian takes the custodian's own decision when it comes (with who did not
/// answer), and one the window's end made takes every member's sweep once each has answered.
/// NEGATIVE CONTROL: an ask the custodian made on every member's sweep, and one a late offer
/// moved, keep their words.
#[test]
fn a_custodians_later_decision_restates_an_ask_that_rested_on_less() {
    let facts = facts_of(&model_state(&[("kind", 3), ("commons", 1)]));
    let open = Need {
        state: State::Open,
        wait: Wait::Holders,
        posted: true,
        ..fresh(
            [5; 16],
            Want::Mailbox("m29".into()),
            Purpose::HarnessSignIn,
            &facts,
        )
    };
    let stood_in = silent(&open, &facts).unwrap();
    let window = Decision::NeedsPerson(Unanswered {
        hosts: vec!["m3".into()],
        others: 0,
    });
    let every = Decision::NeedsPerson(Unanswered::default());
    let custodians = decided(&stood_in, &facts, &window).unwrap();
    let proof = custodians.proof.unwrap();
    assert!(proof.window_ended && !proof.custodian_silent, "{proof:?}");
    assert!(
        custodians.line.contains("m3 did not answer"),
        "{}",
        custodians.line
    );
    assert!(decided(&custodians, &facts, &window).is_none(), "once");
    let answered = decided(&custodians, &facts, &every).unwrap();
    assert!(
        answered
            .proof
            .is_some_and(|proof| proof.swept_unheld && !proof.window_ended)
    );
    assert!(
        answered.line.contains("is on no Mac the lab can reach"),
        "{}",
        answered.line
    );
    assert!(decided(&answered, &facts, &every).is_none());
    assert!(decided(&answered, &facts, &window).is_none());
    let late = decided(
        &custodians,
        &facts,
        &Decision::Offered(Unanswered::default()),
    )
    .unwrap();
    assert!(
        decided(&late, &facts, &every).is_none(),
        "the offer's words stand"
    );
}

fn bare_facts() -> Facts {
    Facts {
        sight: None,
        commons: Commons::Unavailable(Unavailable::Unconfigured),
        policy: Policy::compiled(false),
        offered: false,
        now: 5,
    }
}

#[test]
fn policy_decides_before_existence_so_a_refusal_reads_the_same_either_way() {
    let mut facts = facts_of(&model_state(&[("kind", 3)]));
    facts.policy = Policy::compiled(false);
    let need = fresh(
        [4; 16],
        Want::Mailbox("m29".into()),
        Purpose::HarnessSignIn,
        &facts,
    );
    let absent = settle(&need, &facts);
    if let Some(sight) = facts.sight.as_mut() {
        sight.held.insert("m29_password".into());
    }
    let present = settle(&need, &facts);
    assert_eq!(absent.state, State::Retired);
    assert_eq!(
        absent.retired.as_ref().unwrap().reason,
        Reason::NotAuthorized
    );
    assert_eq!(
        (absent.line.clone(), absent.retired.clone()),
        (present.line, present.retired)
    );
}

#[test]
fn an_entry_want_is_never_posted_and_answers_the_same_words_for_elsewhere_and_absent() {
    let facts = facts_of(&model_state(&[("kind", 3)]));
    let want = Want::Entry("notary".into());
    let need = fresh([5; 16], want, Purpose::ByHand, &facts);
    let absent = settle(&need, &facts);
    assert_eq!(
        absent.retired.as_ref().unwrap().reason,
        Reason::NotAuthorized
    );
    assert!(!absent.posted);
    let mut mine = facts.clone();
    if let Some(sight) = mine.sight.as_mut() {
        sight.held.insert("notary".into());
    }
    let held = settle(&need, &mine);
    assert_eq!(
        held.line,
        format!("already yours at seq {SEQ}: run aterm secrets sync")
    );

    let login_entry = fresh(
        [11; 16],
        Want::Entry("~/.codex/auth.json".into()),
        Purpose::ByHand,
        &mine,
    );
    let mut with_stale_login = mine;
    if let Some(sight) = with_stale_login.sight.as_mut() {
        sight.held.insert("~/.codex/auth.json".into());
    }
    let refused = settle(&login_entry, &with_stale_login);
    assert_eq!(
        refused.retired.as_ref().unwrap().reason,
        Reason::NeverCopied
    );
    assert!(
        refused.line.contains("Codex CLI's login"),
        "{}",
        refused.line
    );
}

fn model_state(overrides: &[(&'static str, i64)]) -> ModelState {
    let model = aterm_models_db::credential_request_model();
    let mut state = model.init_state();
    for (var, value) in overrides {
        state.insert(var, *value);
    }
    state
}

// ---- The ledger on scratch HOMEs, over a vault at a path. ----

struct Vault {
    _root: aterm_tempfile::TempDir,
    admin: Paths,
    member: Paths,
}

fn joined(path: &Paths, source: &std::path::Path, signer: &[u8; 32], host: &str) {
    client::join(
        path,
        &client::JoinOptions {
            source: format!("{}#{}", source.display(), crate::b64::encode_padded(signer)),
            host: Some(host.into()),
            interactive: false,
            wait: std::time::Duration::ZERO,
            confirm: &|_| false,
            password: client::JoinPassword::Given(None),
            fetched: None,
            requested: &|| {},
            say: &|_| {},
        },
    )
    .unwrap();
}

/// A vault at a path whose admin is `m1` and whose member's host name is `andrews`, with
/// `[everyone]` holding one entry: the member opens every locker but `admin`.
fn vault() -> Vault {
    let root = aterm_tempfile::tempdir().unwrap();
    let admin = Paths::for_home(&root.path().join("m1"));
    let member = Paths::for_home(&root.path().join("m29"));
    let source = root.path().join("test.vault");
    client::test_create_unbound(&admin, source.to_str().unwrap(), None, Some("m1")).unwrap();
    let session = client::admin_open(&admin).unwrap();
    let mut doc = session.doc.clone();
    let identity = client::machine_identity(&member).unwrap();
    doc.add_machine(HOST, &identity.recipient().to_bech32())
        .unwrap();
    doc.set_entry(&crate::text::Audience::Everyone, "shared", b"value")
        .unwrap();
    client::admin_publish(&admin, &session, &doc).unwrap();
    joined(&member, &source, &session.config.signer.unwrap(), HOST);
    Vault {
        _root: root,
        admin,
        member,
    }
}

fn publish(paths: &Paths, edit: impl FnOnce(&mut crate::text::Document)) {
    let session = client::admin_open(paths).unwrap();
    let mut doc = session.doc.clone();
    edit(&mut doc);
    client::admin_publish(paths, &session, &doc).unwrap();
}

#[test]
fn a_paper_master_need_posts_nothing_and_retires_never_stored() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    let need = raise(&paths, Want::PaperMaster, Purpose::ByHand).unwrap();
    assert_eq!(need.state, State::Retired);
    assert_eq!(need.retired.as_ref().unwrap().reason, Reason::NeverStored);
    assert!(need.line.contains("on paper") && !need.posted && need.record.is_none());
    let ledger = paths.dir.join(ledger::FILE);
    use std::os::unix::fs::PermissionsExt as _;
    assert_eq!(
        std::fs::metadata(&ledger).unwrap().permissions().mode() & 0o777,
        0o600
    );
    // Raised again, the catalog's answer stands.
    let again = raise(&paths, Want::PaperMaster, Purpose::ByHand).unwrap();
    assert_eq!(
        (again.id, again.state, again.raises),
        (need.id, State::Retired, 1)
    );
}

#[test]
fn the_ledger_is_idempotent_by_want_and_purpose_and_keeps_its_id_across_a_restart() {
    let vault = vault();
    crate::need::test_alab_policy(&vault.member);
    let first = raise(
        &vault.member,
        Want::SiteDeployerKey,
        Purpose::ReleaseSiteKey,
    )
    .unwrap();
    let again = raise(
        &vault.member,
        Want::SiteDeployerKey,
        Purpose::ReleaseSiteKey,
    )
    .unwrap();
    assert_eq!(first.id, again.id);
    // A process that ends and starts again reads the same row.
    let read = ledger::load(&vault.member).unwrap();
    assert_eq!(read.len(), 1);
    assert_eq!(read[0].id, first.id);
    assert_eq!(read[0].state, State::Local);
    assert_eq!(read[0].wait, Wait::Founding);
    // The copyable want waits with the one hand route, naming this Mac's vault host name.
    assert!(
        read[0].line.contains(&format!(
            "aterm secrets set '{}' --for {HOST} --file <file> on the vault's admin Mac",
            catalog::SITE_DEPLOYER_KEY_ENTRY
        )),
        "{}",
        read[0].line
    );
    let other = raise(&vault.member, Want::SiteDeployerKey, Purpose::ByHand).unwrap();
    assert_ne!(other.id, first.id);
    assert_eq!(list(&vault.member).unwrap().len(), 2);
    publish(&vault.admin, |doc| {
        doc.set_entry(
            &crate::text::Audience::Everyone,
            "gh_access_token",
            b"fixture-token",
        )
        .unwrap();
    });
    client::sync(&vault.member, None).unwrap();
    let config = client::config(&vault.member).unwrap().unwrap();
    let paired = client::with_materialization_ready_for_entries(
        &vault.member,
        &config,
        &["gh_access_token"],
        || Ok(()),
    )
    .unwrap()
    .unwrap();
    let high = raise_refused(
        &vault.member,
        Want::Gh(GhProfile::Dev),
        Purpose::AghDev,
        u64::MAX,
    )
    .unwrap();
    let unchanged = raise_refused_from_snapshot(
        &vault.member,
        high.want.clone(),
        high.purpose,
        &paired.snapshot,
        || Ok(()),
    )
    .unwrap();
    assert_eq!(unchanged.id, high.id);
    assert_eq!(
        unchanged.refused,
        Some(RefusalClaim::LegacySequence(u64::MAX)),
        "a genuine lower authenticated version cannot discard a known nonzero floor"
    );
    let bytes = std::fs::read(vault.member.dir.join(LEDGER)).unwrap();
    assert!(
        acknowledge_from_snapshot(&vault.member, &unchanged, &paired.snapshot, || Ok(())).is_err()
    );
    assert_eq!(std::fs::read(vault.member.dir.join(LEDGER)).unwrap(), bytes);
}

#[test]
fn absence_needs_every_locker_open() {
    let vault = vault();
    crate::need::test_alab_policy(&vault.member);
    let sight = client::sight(&vault.member).unwrap().unwrap();
    assert!(!sight.hidden, "admin + 1 readable proves absence");
    assert_eq!(sight.host, HOST);
    let asked = raise(
        &vault.member,
        Want::Mailbox("m29".into()),
        Purpose::HarnessSignIn,
    )
    .unwrap();
    assert_eq!(asked.state, State::AwaitingPerson);
    assert!(asked.asking && asked.proof.unwrap().warrants_asking());
    assert!(
        asked
            .ask
            .as_ref()
            .unwrap()
            .needs_you
            .contains("--for andrews")
    );
    assert_eq!(exceptions(&list(&vault.member).unwrap()).len(), 1);
    // A third Mac's section makes a locker the member cannot open: the proof proves nothing.
    let third = Paths::for_home(&vault.member.home.with_file_name("m3"));
    let identity = client::machine_identity(&third).unwrap();
    publish(&vault.admin, |doc| {
        doc.add_machine("m3", &identity.recipient().to_bech32())
            .unwrap();
        doc.set_entry(
            &crate::text::Audience::Hosts(vec!["m3".into()]),
            "notary",
            b"x",
        )
        .unwrap();
    });
    client::sync(&vault.member, None).unwrap();
    let sight = client::sight(&vault.member).unwrap().unwrap();
    assert!(sight.hidden, "admin + 2 with one unreadable proves nothing");
    let waits = raise(
        &vault.member,
        Want::Mailbox("m30".into()),
        Purpose::HarnessSignIn,
    )
    .unwrap();
    assert_eq!((waits.state, waits.wait), (State::Local, Wait::Founding));
    assert!(
        waits
            .line
            .contains("cannot show that no Mac holds m30_password"),
        "{}",
        waits.line
    );
}

#[test]
fn give_then_satisfied_fulfils_and_takes_the_ask_down() {
    let vault = vault();
    crate::need::test_alab_policy(&vault.member);
    let want = Want::Mailbox("m29".into());
    let asked = raise(&vault.member, want.clone(), Purpose::HarnessSignIn).unwrap();
    assert!(asked.asking);
    // The person runs the admin step the ask names.
    publish(&vault.admin, |doc| {
        doc.set_entry(
            &crate::text::Audience::Hosts(vec![HOST.into()]),
            "m29_password",
            b"pw",
        )
        .unwrap();
    });
    client::sync(&vault.member, None).unwrap();
    let published = list(&vault.member).unwrap();
    assert_eq!(published[0].state, State::Published);
    assert!(exceptions(&published).is_empty() && needs_you(&published).is_empty());
    let done = satisfied(&vault.member, &want, Purpose::HarnessSignIn)
        .unwrap()
        .unwrap();
    assert_eq!(done.state, State::Fulfilled);
    assert!(status_line(&list(&vault.member).unwrap()).is_none());
}

#[test]
fn notary_requests_use_only_the_release_entry() {
    let vault = vault();
    crate::need::test_alab_policy(&vault.member);
    let want = Want::parse("notary").unwrap();
    assert_eq!(want.entry().as_deref(), Some("release-notary"));
    assert_eq!(
        want.hold_path(&vault.member.home),
        Some(vault.member.home.join(".secrets/release-notary"))
    );
    let raised = raise(&vault.member, want.clone(), Purpose::ReleaseNotary).unwrap();
    assert!(raised.line.contains("release-notary"), "{}", raised.line);

    publish(&vault.admin, |doc| {
        doc.set_entry(
            &crate::text::Audience::Everyone,
            "notary",
            b"old credential",
        )
        .unwrap();
    });
    client::sync(&vault.member, None).unwrap();
    assert!(
        !list(&vault.member).unwrap()[0].got,
        "the legacy entry cannot fulfill the typed request"
    );

    publish(&vault.admin, |doc| {
        doc.set_entry(
            &crate::text::Audience::Hosts(vec![HOST.into()]),
            "release-notary",
            b"new credential",
        )
        .unwrap();
    });
    client::sync(&vault.member, None).unwrap();
    let sight = client::sight(&vault.member).unwrap().unwrap();
    assert!(sight.held.contains("release-notary"));
}

#[test]
fn a_decline_retires_with_its_note_and_files_no_entry() {
    let vault = vault();
    crate::need::test_alab_policy(&vault.member);
    raise(
        &vault.member,
        Want::Mailbox("m29".into()),
        Purpose::HarnessSignIn,
    )
    .unwrap();
    assert!(retire(&vault.member, "mailbox:m28", true, "").is_err());
    let declined = retire(&vault.member, "mailbox:m29", true, "the account is closed").unwrap();
    let retired = declined[0].retired.as_ref().unwrap();
    assert_eq!(
        (retired.reason, retired.note.as_str()),
        (Reason::Declined, "the account is closed")
    );
    let after = list(&vault.member).unwrap();
    assert!(exceptions(&after).is_empty() && needs_you(&after).is_empty());
    // A decline stands when the consumer raises again: the person raises it by hand.
    let again = raise(
        &vault.member,
        Want::Mailbox("m29".into()),
        Purpose::HarnessSignIn,
    )
    .unwrap();
    assert_eq!(again.retired.unwrap().reason, Reason::Declined);
    // Declining what awaits no person is refused.
    raise(
        &vault.member,
        Want::SiteDeployerKey,
        Purpose::ReleaseSiteKey,
    )
    .unwrap();
    assert!(retire(&vault.member, "site-deployer-key", true, "").is_err());
    let withdrawn = retire(&vault.member, "site-deployer-key", false, "").unwrap();
    assert_eq!(
        withdrawn[0].retired.as_ref().unwrap().reason,
        Reason::NotNeeded
    );
}

/// A withdrawal by hand claims no check: it says a person withdrew it on this Mac, rests on
/// that, and keeps the person's note in a form the table's record takes. Only the consumer's
/// own check reads as one.
#[test]
fn a_withdrawal_by_hand_says_so_and_keeps_its_note() {
    let vault = vault();
    crate::need::test_alab_policy(&vault.member);
    raise(
        &vault.member,
        Want::SiteDeployerKey,
        Purpose::ReleaseSiteKey,
    )
    .unwrap();
    let withdrawn = retire(
        &vault.member,
        "site-deployer-key",
        false,
        "this Mac\ncuts no releases",
    )
    .unwrap();
    let retired = withdrawn[0].retired.as_ref().unwrap();
    assert_eq!(
        (
            retired.reason,
            retired.evidence.as_str(),
            retired.note.as_str()
        ),
        (
            Reason::NotNeeded,
            "withdrawn by hand on andrews",
            "this Mac cuts no releases"
        )
    );
    assert_eq!(
        withdrawn[0].line,
        "not needed: withdrawn by hand on this Mac (andrews); raising the site deployer's service-account key again asks again"
    );
    assert_eq!(ledger::load(&vault.member).unwrap()[0], withdrawn[0]);
    raise(&vault.member, Want::Notary, Purpose::ReleaseNotary).unwrap();
    let checked = satisfied(&vault.member, &Want::Notary, Purpose::ReleaseNotary)
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            checked.retired.as_ref().unwrap().evidence.as_str(),
            checked.line.as_str()
        ),
        (
            "the requester's own check",
            "not needed: release.notary's check no longer lacks the notary credential"
        )
    );
}

/// A lab retirement the ledger takes in is raised again at once when its reason has already
/// changed: a lapse (this Mac is taking it in, so it is back) comes back as a new request under
/// a new id, as its line says, though a need raised by hand has no consumer to raise it. Each
/// move comes back with the id it named.
/// NEGATIVE CONTROL: a decline, a `not-held` (no offer in the ledger's facts) and a
/// `requester-gone` taken in the same way stay retired under the same id.
#[test]
fn a_lapse_the_ledger_takes_in_is_raised_again_at_once_and_a_standing_reason_is_not() {
    let vault = vault();
    crate::need::test_alab_policy(&vault.member);
    let wants = [
        Want::Notary,
        Want::SiteDeployerKey,
        Want::Mailbox("m29".into()),
        Want::parse("gh:dev").unwrap(),
    ];
    for want in &wants {
        let raised = raise(&vault.member, want.clone(), Purpose::ByHand).unwrap();
        assert!(!raised.state.ended(), "{}", raised.line);
    }
    // As a pass leaves them once the table holds each request (a flush needs a Writer).
    let mut rows: Vec<Need> = ledger::load(&vault.member)
        .unwrap()
        .into_iter()
        .map(|need| Need {
            state: State::Open,
            wait: Wait::Holders,
            posted: true,
            ..need
        })
        .collect();
    ledger::save(&vault.member, &mut rows).unwrap();
    let id = |want: &Want| rows.iter().find(|need| need.want == *want).unwrap().id;
    let retired = |want: &Want, reason| {
        let retirement = Retirement {
            reason,
            evidence: "a custodian".into(),
            note: String::new(),
            generation: None,
        };
        (id(want), Absorb::Decided(Decision::Retired(retirement)))
    };
    let moves = [
        retired(&Want::Notary, Reason::Lapsed),
        retired(&Want::SiteDeployerKey, Reason::Declined),
        retired(&Want::Mailbox("m29".into()), Reason::NotHeld),
        retired(&Want::parse("gh:dev").unwrap(), Reason::RequesterGone),
    ];
    let moved = absorb(&vault.member, &moves).unwrap();
    assert_eq!(
        moved.iter().map(|(named, _)| *named).collect::<Vec<_>>(),
        moves.iter().map(|(named, _)| *named).collect::<Vec<_>>(),
        "each move comes back with the id it named"
    );
    let after = ledger::load(&vault.member).unwrap();
    assert_eq!(
        after,
        moved
            .iter()
            .map(|(_, need)| need.clone())
            .collect::<Vec<_>>()
    );
    let lapsed = &after[0];
    assert_eq!(lapsed.state, State::Local, "{}", lapsed.line);
    assert_ne!(lapsed.id, id(&Want::Notary), "a new request id");
    assert_ne!(lapsed.id, [0; 16]);
    assert_eq!(
        (lapsed.raises, lapsed.raised_again_on_change, lapsed.posted),
        (2, Some(true), false)
    );
    assert!(lapsed.retired.is_none() && lapsed.record.is_none());
    for (standing, reason) in
        after[1..]
            .iter()
            .zip([Reason::Declined, Reason::NotHeld, Reason::RequesterGone])
    {
        assert_eq!(standing.state, State::Retired, "{}", standing.line);
        assert_eq!(standing.id, id(&standing.want), "{standing:?}");
        assert_eq!(standing.retired.as_ref().map(|r| r.reason), Some(reason));
        assert_eq!(standing.raises, 1);
    }
}

#[test]
fn a_non_alab_vault_hands_out_nothing_by_request() {
    let vault = vault();
    let need = raise(
        &vault.member,
        Want::Mailbox("m29".into()),
        Purpose::HarnessSignIn,
    )
    .unwrap();
    let retired = need.retired.unwrap();
    assert_eq!(
        (retired.reason, retired.generation),
        (Reason::NotAuthorized, Some(0))
    );
    assert!(!need.asking);
}

#[test]
fn the_ledger_reads_back_every_field_it_writes() {
    let mut facts = facts_of(&model_state(&[("kind", 3)]));
    facts.now = 77;
    let need = settle(
        &fresh(
            [9; 16],
            Want::Mailbox("m29".into()),
            Purpose::HarnessSignIn,
            &facts,
        ),
        &facts,
    );
    assert!(need.asking);
    let mut declined = act(&need, &facts, &Act::Decline("a \"quoted\" note".into())).unwrap();
    declined.refused = Some(RefusalClaim::LegacySequence(3));
    declined.record = Some(vec![1, 2, 3]);
    declined.raised_again_on_change = Some(true);
    let open = Need {
        id: [11; 16],
        state: State::Open,
        posted: true,
        asking: false,
        ask: None,
        proof: None,
        ..need.clone()
    };
    let stood_in = silent(&open, &facts).unwrap();
    assert!(stood_in.custodian_silent && stood_in.proof.is_some_and(|p| p.custodian_silent));
    let window = Decision::NeedsPerson(Unanswered {
        hosts: vec!["m3".into()],
        others: 1,
    });
    let posted = Need {
        id: [12; 16],
        want: Want::Notary,
        purpose: Purpose::ByHand,
        ..open.clone()
    };
    let asked = decided(&posted, &facts, &window).unwrap();
    let offered = decided(&asked, &facts, &Decision::Offered(Unanswered::default())).unwrap();
    assert!(offered.late_offer && offered.proof.is_some_and(|p| p.window_ended));
    let completed = Need {
        id: [13; 16],
        want: Want::Gh(GhProfile::Dev),
        purpose: Purpose::AghDev,
        state: State::Fulfilled,
        wait: Wait::Nobody,
        asking: false,
        ask: None,
        got: true,
        got_authorized: true,
        posted: true,
        record: Some(vec![4, 5, 6]),
        ..need.clone()
    };
    let rejected = Need {
        id: [14; 16],
        refused: Some(RefusalClaim::LegacySequence(3)),
        ..completed.clone()
    };
    // A codec specimen only: a public claim's representation is not provider acceptance.
    let authenticated = Need {
        id: [15; 16],
        refused: Some(RefusalClaim::Authenticated(
            client::version::VersionClaim::new(
                "codec".into(),
                [5; 32],
                Some(client::version::EnvelopeVersion {
                    seq: 4,
                    digest: [6; 32],
                }),
                None,
            )
            .unwrap(),
        )),
        ..completed.clone()
    };
    let held = Need {
        id: [16; 16],
        want: completed.want.clone(),
        purpose: completed.purpose,
        ..declined.clone()
    };
    let forbidden = Need {
        id: [17; 16],
        retired: Some(Retirement {
            reason: Reason::NotAuthorized,
            evidence: "policy".into(),
            note: String::new(),
            generation: Some(9),
        }),
        ..held.clone()
    };
    let other_purpose = Need {
        id: [18; 16],
        purpose: Purpose::AghRelease,
        ..completed.clone()
    };
    let other_want = Need {
        id: [19; 16],
        want: Want::Gh(GhProfile::Release),
        ..completed.clone()
    };
    let awaiting_confirmation = [completed.id, rejected.id, authenticated.id];
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    let mut rows = vec![
        need,
        declined,
        stood_in,
        offered,
        completed,
        rejected,
        held,
        forbidden,
        other_purpose,
        other_want,
        authenticated,
    ];
    ledger::save(&paths, &mut rows).unwrap();
    assert_eq!(ledger::load(&paths).unwrap(), rows);
    assert_eq!(
        peek(&paths).unwrap(),
        rows,
        "format 3 keeps confirmed completions"
    );
    let file = paths.dir.join(LEDGER);
    let encoded = std::fs::read_to_string(&file).unwrap();
    for format in [1, 2] {
        let mut table: aterm_toml::Table = aterm_toml::from_str(&encoded).unwrap();
        let mut container = table.remove("need").unwrap().as_table().unwrap().clone();
        let mut legacy_rows = container
            .remove("rows")
            .unwrap()
            .as_array()
            .unwrap()
            .clone();
        let expected = if format == 1 {
            // v1 cannot encode an authenticated refusal claim.
            assert!(
                legacy_rows.pop().unwrap()["refused_version"]
                    .as_str()
                    .is_some()
            );
            for row in &mut legacy_rows {
                let row = row.as_table_mut().unwrap();
                if let Some(value) = row.remove("refused_legacy") {
                    row.insert(
                        "refused".into(),
                        aterm_toml::Value::Integer(value.as_str().unwrap().parse().unwrap()),
                    );
                }
            }
            table.insert("need".into(), aterm_toml::Value::Array(legacy_rows));
            &rows[..rows.len() - 1]
        } else {
            container.insert("format".into(), aterm_toml::Value::Integer(format));
            container.insert("rows".into(), aterm_toml::Value::Array(legacy_rows));
            table.insert("need".into(), aterm_toml::Value::Table(container));
            rows.as_slice()
        };
        let legacy = aterm_toml::to_string(&table).unwrap();
        crate::fsx::write_atomic(&file, legacy.as_bytes(), 0o600).unwrap();
        let loaded = peek(&paths).unwrap();
        assert_eq!(loaded.len(), expected.len());
        for (actual, before) in loaded.iter().zip(expected) {
            if awaiting_confirmation.contains(&before.id) {
                assert_eq!(
                    (actual.state, actual.wait),
                    (State::Published, Wait::Requester)
                );
                let retained = Need {
                    state: before.state,
                    wait: before.wait,
                    line: before.line.clone(),
                    ..actual.clone()
                };
                assert_eq!(
                    &retained, before,
                    "all identity and evidence survive migration"
                );
            } else {
                assert_eq!(
                    actual, before,
                    "holds and unrelated completions stay unchanged"
                );
            }
        }
        assert_eq!(ledger::load(&paths).unwrap(), loaded);
        with_ledger(&paths, |_, _| Ok(())).unwrap();
        assert_eq!(
            std::fs::read(&file).unwrap(),
            legacy.as_bytes(),
            "reads never publish migration"
        );
        with_ledger(&paths, |saved, _| {
            saved[0].line.push_str(" persisted");
            Ok(())
        })
        .unwrap();
        let mut written = loaded;
        written[0].line.push_str(" persisted");
        assert_eq!(ledger::load(&paths).unwrap(), written);
        let migrated = std::fs::read_to_string(&file).unwrap();
        let document: aterm_toml::Table = aterm_toml::from_str(&migrated).unwrap();
        assert!(
            document["need"].as_array().is_none(),
            "v1 readers fail closed"
        );
        assert_eq!(
            document["need"]["format"].as_integer(),
            Some(3),
            "v2 readers require format 2"
        );
    }
    let migrated = std::fs::read_to_string(&file).unwrap();
    let mut document: aterm_toml::Table = aterm_toml::from_str(&migrated).unwrap();
    let mut container = document.remove("need").unwrap();
    container
        .as_table_mut()
        .unwrap()
        .insert("format".into(), aterm_toml::Value::Integer(99));
    document.insert("need".into(), container);
    let unknown = aterm_toml::to_string(&document).unwrap();
    crate::fsx::write_atomic(&file, unknown.as_bytes(), 0o600).unwrap();
    let mut entered = false;
    assert!(
        with_ledger(&paths, |_, _| {
            entered = true;
            Ok(())
        })
        .is_err()
    );
    assert!(!entered);
    assert_eq!(std::fs::read(&file).unwrap(), unknown.as_bytes());
}

/// The configured vault decides the policy; a Mac in no vault has none against it yet.
#[test]
fn the_policy_is_the_configured_vaults_and_no_vault_refuses_nothing_yet() {
    assert!(policy_for(Some(true)).allows(&Want::Notary));
    assert!(!policy_for(Some(false)).allows(&Want::Notary));
    assert!(
        policy_for(None).allows(&Want::Notary),
        "no vault: the need waits on a join"
    );
    assert!(
        !policy_for(None).allows(&Want::PaperMaster),
        "the catalog still decides first"
    );
}
