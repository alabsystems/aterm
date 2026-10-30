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
        }),
        got: s["got"] == 1,
        got_authorized: s["pa"] == 1,
        raised_again_on_change: (s["raises"] == 2).then_some(s["okr"] == 1),
        posted: s["posted"] == 1,
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
    let failures = conformance_failures(&model, request_transition);
    assert!(
        failures.is_empty(),
        "{} failures, first: {:?}",
        failures.len(),
        failures.first()
    );
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
    for (want, purpose, host) in [
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
    ] {
        let ask = ask_for(&want, purpose, host).unwrap();
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
        );
        assert!(ask.needs_you.starts_with("NEEDS YOU: ") && !ask.needs_you.contains('\n'));
        assert!(entry.key().starts_with("secrets.need."), "{}", entry.key());
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
    assert!(decided(&open, &unauthorized, &Decision::Offered).is_none());
    assert!(decided(&open, &unauthorized, &Decision::Published { seq: 9 }).is_none());
    assert!(decided(&open, &unauthorized, &Decision::NeedsPerson).is_none());

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
        decided(&mailbox_open, &held_facts, &Decision::NeedsPerson).is_none(),
        "never raises NEEDS YOU when the entry is already held"
    );

    if let Some(sight) = facts.sight.as_mut() {
        sight
            .held
            .insert(catalog::SITE_DEPLOYER_KEY_ENTRY.to_owned());
    }
    let published = decided(&open, &facts, &Decision::Published { seq: SEQ }).unwrap();
    assert_eq!(settle(&published, &facts).state, State::Fulfilled);
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
    declined.refused = Some(3);
    declined.record = Some(vec![1, 2, 3]);
    declined.raised_again_on_change = Some(true);
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    let mut rows = vec![need, declined];
    ledger::save(&paths, &mut rows).unwrap();
    assert_eq!(ledger::load(&paths).unwrap(), rows);
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
