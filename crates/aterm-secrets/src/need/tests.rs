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
        2 => Want::Login("claude/m29".into()),
        3 => Want::Mailbox("m29".into()),
        _ => Want::parse("claude-token:ac-00000000000000a1").unwrap(),
    }
}

fn purpose_of(want: &Want) -> Purpose {
    match want.class() {
        Class::Copyable => Purpose::ReleaseSiteKey,
        Class::VaultOnly => Purpose::HarnessSignIn,
        _ => Purpose::ByHand,
    }
}

const STATES: [State; 6] = [
    State::Local,
    State::Open,
    State::Offered,
    State::Published,
    State::Fulfilled,
    State::Retired,
];

/// The model's `wait` numbers (3, the person, is gone).
const WAITS: [(Wait, i64); 5] = [
    (Wait::Nobody, 0),
    (Wait::Holders, 1),
    (Wait::Custodian, 2),
    (Wait::Requester, 4),
    (Wait::Founding, 5),
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
    WAITS.iter().find(|(w, _)| *w == wait).unwrap().1
}

fn wait_of(n: i64) -> Wait {
    WAITS.iter().find(|(_, at)| *at == n).unwrap().0
}

fn why_num(reason: Reason) -> i64 {
    REASONS
        .iter()
        .position(|r| *r == reason)
        .map_or(0, |at| at as i64 + 1)
}

fn kind_num(want: &Want) -> i64 {
    match want.class() {
        Class::Copyable => 0,
        Class::PerMac => 2,
        Class::VaultOnly if want.producer().is_some() => 4,
        Class::VaultOnly | Class::AdminAct => 3,
    }
}

fn facts_of(s: &ModelState) -> Facts {
    let want = want_of(s["kind"]);
    let held = if s["inv"] == 1 || s["inuse"] == 1 {
        want.entry().into_iter().collect()
    } else {
        BTreeSet::new()
    };
    let in_use = if s["inuse"] == 1 {
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
            held,
        }),
        in_use,
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
    Some(Need {
        id: [1; 16],
        want,
        purpose,
        state: STATES[usize::try_from(s["st"] - 1).unwrap()],
        wait: wait_of(s["wait"]),
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
        got: s["got"] == 1,
        got_authorized: s["pa"] == 1,
        raised_again_on_change: (s["raises"] == 2).then_some(s["okr"] == 1),
        posted: s["posted"] == 1,
        custodian_silent: false,
        record: None,
        line: String::new(),
        seq: Some(SEQ),
        exposed: Vec::new(),
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
    set(&mut next, "kind", kind_num(&real.want));
    set(&mut next, "wait", wait_num(real.wait));
    set(&mut next, "raises", i64::from(real.raises.min(2)));
    set(&mut next, "need", i64::from(real.needed));
    set(&mut next, "got", i64::from(real.got));
    set(&mut next, "pa", i64::from(real.got_authorized));
    set(&mut next, "posted", i64::from(real.posted));
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
    match action {
        "CommonsReady" => set(&mut next, "commons", 1),
        "Flush" => set(&mut next, "swept", 0),
        _ => {}
    }
    next
}

/// The real move the model's `action` stands for, from `s`: `None` when the real guard does not
/// hold. Commons becoming ready is the environment's step, which this Mac observes: the fixture
/// takes it, then runs the move that observes it.
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
        "RaisePerMachine" => raise(2),
        "RaiseVaultOnly" => raise(3),
        "RaiseProducer" => raise(4),
        "Arrived" => arrival(before.as_ref()?, &facts),
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
                "Unauthorized" => Act::Unauthorized,
                "Apply" => Act::Apply,
                "Unneeded" => Act::Unneeded,
                "NeedAgain" => Act::NeedAgain,
                "Reraise" => Act::Reraise,
                "Flush" => Act::Flush,
                _ => panic!("{action} is not a ledger move"),
            };
            act(before.as_ref()?, &facts, &step)
        }
    }?;
    Some(project(s, action, before.as_ref(), &real))
}

/// [`request_transition`] with the model's `Unneeded` taken by a withdrawal by hand
/// rather than by the consumer's own check: both are the model's one `Unneeded`.
fn hand_transition(s: &ModelState, action: &str) -> Option<ModelState> {
    if action != "Unneeded" {
        return request_transition(s, action);
    }
    let before = need_of(s)?;
    let real = act(&before, &facts_of(s), &Act::Withdraw("not now".into()))?;
    Some(project(s, action, Some(&before), &real))
}

const BOUND: [&str; 13] = [
    "Raise",
    "RaisePerMachine",
    "RaiseVaultOnly",
    "RaiseProducer",
    "Arrived",
    "Catalog",
    "Unauthorized",
    "Apply",
    "Unneeded",
    "NeedAgain",
    "Reraise",
    "CommonsReady",
    "Flush",
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
    // A hand mutant of the real code the projection refuses: a withdrawal by hand that leaves
    // the consumer's word that it still needs it.
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
        "developer-id:m22",
        "mailbox:m29",
        "mailbox:m29-2",
        "gh:dev",
        "gh:release",
        "gh:fleet",
        "machines-admin",
        "entry:~/company-life/config/local/service-account.alab-site-deployer.json",
        "paper-master",
        "login:claude/m29",
        "browser-profile:m29",
        "refresh-token:codex/work",
        "vault-identity",
        "commons-device-key",
        "commons-control",
        "azure-signing-principal",
        "fleet-code",
    ] {
        assert_eq!(Want::parse(word).unwrap().key(), word);
    }
    for bad in [
        "mailbox:",
        "mailbox:M29",
        "gh:other",
        "developer-id:",
        "developer-id:../m22",
        "entry:~/.secrets/x",
        "secret",
        "login:m29",
    ] {
        assert!(Want::parse(bad).is_err(), "{bad}");
    }
    assert_eq!(Want::PaperMaster.class(), Class::VaultOnly);
    assert!(Want::PaperMaster.posted() && !Want::PaperMaster.if_absent());
    assert_eq!(Want::PaperMaster.entries(), [catalog::PAPER_MASTER_ENTRY]);
    assert_eq!(
        crate::entry_class::of(catalog::PAPER_MASTER_ENTRY),
        crate::entry_class::EntryClass::Held(crate::entry_class::Held::Release)
    );
    assert_eq!(Want::Login("claude/m29".into()).class(), Class::PerMac);
    assert_eq!(Want::SiteDeployerKey.class(), Class::Copyable);
    assert_eq!(Want::Mailbox("m29".into()).class(), Class::VaultOnly);
    let devid = Want::DeveloperId("m22".into());
    assert_eq!(devid.class(), Class::VaultOnly);
    assert!(devid.posted() && devid.if_absent());
    assert_eq!(
        devid.entries(),
        [
            "~/.aterm/apple/devid-m22.cer",
            "~/.aterm/apple/devid-m22.key"
        ]
    );
    assert!(
        devid
            .entries()
            .iter()
            .all(|name| crate::entry_class::signing_material(name))
    );
    assert_eq!(Want::CommonsControl.class(), Class::AdminAct);
    assert!(!Want::Entry("notary".into()).posted());
    assert!(!Want::VaultIdentity.posted() && !Want::CommonsControl.posted());
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
    assert!(want.posted());
    assert_eq!(
        Want::Mailbox("m29".into()).entries(),
        vec!["m29_password", "m29_totp"]
    );
}

#[test]
fn the_hold_probe_is_the_catalogs_one_entry() {
    assert_eq!(
        Want::SiteDeployerKey.hold_entry().as_deref(),
        Some(crate::entry_class::SITE_DEPLOYER_KEY)
    );
    assert_eq!(
        Want::Mailbox("m29".into()).hold_entry().as_deref(),
        Some("m29_password")
    );
    assert!(Want::Gh(GhProfile::Dev).hold_entry().is_none());
    assert!(Want::PaperMaster.hold_entry().is_none());
}

/// A need no consumer checks for itself (one raised by hand) is fulfilled once the version
/// this Mac applied gives it the entry, so it never stays published for good; a consumer that
/// checks for itself still decides its own.
#[test]
fn a_need_no_consumer_checks_is_fulfilled_by_its_delivery() {
    let mut facts = facts_of(&model_state(&[("kind", 3)]));
    let want = Want::Mailbox("m29".into());
    // Before Commons nothing holds it that this Mac sees, and nobody is asked: it waits to post.
    let local = settle(
        &fresh([7; 16], want.clone(), Purpose::ByHand, &facts),
        &facts,
    );
    assert_eq!((local.state, local.wait), (State::Local, Wait::Founding));
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
    let own = settle(&published(Purpose::HarnessSignIn, &facts), &facts);
    assert_eq!(
        own.state,
        State::Published,
        "the ladder's own check decides"
    );
}

#[test]
fn decided_checks_policy_and_absorb_settles_delivery() {
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

#[test]
fn the_fleet_code_is_never_posted_and_is_fulfilled_when_the_envelope_holds_it() {
    let want = Want::FleetCode;
    assert_eq!(want.class(), Class::VaultOnly);
    assert!(!want.posted());
    assert_eq!(want.entries(), [client::FLEET_CODE_ENTRY]);
    assert_eq!(Purpose::parse("fleet.join"), Some(Purpose::FleetJoin));
    // A Commons Writer whose policy lets members have a want by request posts nothing for it.
    let mut facts = facts_of(&model_state(&[("kind", 3), ("commons", 1), ("auth", 1)]));
    let need = fresh([6; 16], want, Purpose::FleetJoin, &facts);
    let waiting = settle(&need, &facts);
    assert_eq!(
        (waiting.state, waiting.wait, waiting.posted),
        (State::Local, Wait::Custodian, false)
    );
    assert!(
        waiting.line.contains("aterm fleet publish"),
        "{}",
        waiting.line
    );
    assert_eq!(
        settle(&waiting, &facts),
        waiting,
        "a settled need stays as it is"
    );
    if let Some(sight) = facts.sight.as_mut() {
        sight.alab = false;
    }
    assert_eq!(settle(&need, &facts).wait, Wait::Requester);
    if let Some(sight) = facts.sight.as_mut() {
        sight.alab = true;
        sight.held.insert(client::FLEET_CODE_ENTRY.into());
    }
    let held = settle(&waiting, &facts);
    assert_eq!((held.state, held.wait), (State::Fulfilled, Wait::Nobody));
    facts.sight = None;
    assert_eq!(settle(&need, &facts).wait, Wait::Requester);
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

#[cfg(unix)]
#[test]
fn a_per_mac_need_posts_nothing_and_retires_never_copied() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    let need = raise(&paths, Want::VaultIdentity, Purpose::ByHand).unwrap();
    assert_eq!(need.state, State::Retired);
    assert_eq!(need.retired.as_ref().unwrap().reason, Reason::NeverCopied);
    assert!(need.line.contains("own key") && !need.posted && need.record.is_none());
    let ledger = paths.dir.join(ledger::FILE);
    use std::os::unix::fs::PermissionsExt as _;
    assert_eq!(
        std::fs::metadata(&ledger).unwrap().permissions().mode() & 0o777,
        0o600
    );
    // Raised again, the catalog's answer stands.
    let again = raise(&paths, Want::VaultIdentity, Purpose::ByHand).unwrap();
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

/// A need no Mac holds before Commons waits to post and asks nobody; once the vault gives this
/// Mac the entry, its consumer's check ends it.
#[test]
fn a_need_before_commons_asks_nobody_and_the_consumers_check_ends_it() {
    let vault = vault();
    crate::need::test_alab_policy(&vault.member);
    let want = Want::Mailbox("m29".into());
    let raised = raise(&vault.member, want.clone(), Purpose::HarnessSignIn).unwrap();
    assert_eq!((raised.state, raised.wait), (State::Local, Wait::Founding));
    publish(&vault.admin, |doc| {
        doc.set_entry(
            &crate::text::Audience::Hosts(vec![HOST.into()]),
            "m29_password",
            b"pw",
        )
        .unwrap();
    });
    client::sync(&vault.member, None).unwrap();
    assert_eq!(list(&vault.member).unwrap()[0].state, State::Local);
    let done = satisfied(&vault.member, &want, Purpose::HarnessSignIn)
        .unwrap()
        .unwrap();
    assert!(done.state.ended(), "{}", done.line);
    assert!(status_line(&list(&vault.member).unwrap()).is_none());
}

#[test]
fn notary_requests_use_only_the_release_entry() {
    let vault = vault();
    crate::need::test_alab_policy(&vault.member);
    let want = Want::parse("notary").unwrap();
    assert_eq!(want.entry().as_deref(), Some("release-notary"));
    assert_eq!(want.hold_entry().as_deref(), Some("release-notary"));
    raise(&vault.member, want.clone(), Purpose::ReleaseNotary).unwrap();

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

/// A withdrawal by hand claims no check: it says it was withdrawn by hand on this Mac, rests on
/// that, and keeps its note in a form the table's record takes. Only the consumer's
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
/// NEGATIVE CONTROL: an earlier build's decline, a `not-held` (no offer in the ledger's facts) and a
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
}

/// Past the silence with no custodian record, a posted open request waits on the custodian and
/// says the admin's aterm is silent, once: a pass that sees what the last one saw moves nothing.
/// A value that reaches this Mac's own section delivers it, so a need no consumer checks is
/// fulfilled. NEGATIVE CONTROL: a missing sight delivers nothing; a value the consumer refused at
/// the version this Mac holds is no delivery; an offer is its own line; a need not posted, or
/// not open or offered on the table, moves nothing.
#[test]
fn a_silent_custodian_is_said_once_and_a_value_in_this_macs_section_delivers() {
    let facts = facts_of(&model_state(&[("kind", 3), ("commons", 1)]));
    let posted = |want: Want, purpose| Need {
        state: State::Open,
        wait: Wait::Holders,
        posted: true,
        ..fresh([12; 16], want, purpose, &facts)
    };
    let mut blind = facts.clone();
    blind.sight = None;
    let mailbox = posted(Want::Mailbox("m29".into()), Purpose::HarnessSignIn);
    let notary = posted(Want::Notary, Purpose::ByHand);
    for (need, facts) in [(&mailbox, &facts), (&mailbox, &blind), (&notary, &facts)] {
        let said = silent(need, facts).unwrap();
        assert_eq!((said.state, said.wait), (State::Open, Wait::Custodian));
        assert!(said.custodian_silent, "{said:?}");
        assert!(said.line.contains("96 hours"), "{}", said.line);
        assert!(silent(&said, facts).is_none(), "said once");
    }

    let mut held = facts.clone();
    if let Some(sight) = held.sight.as_mut() {
        sight.held.insert(catalog::NOTARY_ENTRY.to_owned());
    }
    let said = silent(&notary, &facts).unwrap();
    let delivered = settle(&silent(&said, &held).unwrap(), &held);
    assert_eq!(delivered.state, State::Fulfilled, "{}", delivered.line);
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
        silent(&at_refused, &held).map(|need| (need.state, need.wait)),
        Some((State::Open, Wait::Custodian)),
        "the refused value is no delivery"
    );

    let local = fresh([13; 16], Want::Notary, Purpose::ByHand, &facts);
    let unposted = Need {
        posted: false,
        ..notary.clone()
    };
    let retired = Need {
        state: State::Retired,
        ..notary.clone()
    };
    assert!(silent(&local, &facts).is_none());
    assert!(silent(&unposted, &facts).is_none());
    assert!(silent(&retired, &held).is_none());
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
    assert_eq!(need.state, State::Local);
    let mut withdrawn = act(&need, &facts, &Act::Withdraw("a \"quoted\" note".into())).unwrap();
    withdrawn.refused = Some(RefusalClaim::LegacySequence(3));
    withdrawn.record = Some(vec![1, 2, 3]);
    withdrawn.raised_again_on_change = Some(true);
    let open = Need {
        id: [11; 16],
        state: State::Open,
        posted: true,
        ..need.clone()
    };
    let silent_on = silent(&open, &facts).unwrap();
    assert!(silent_on.custodian_silent && silent_on.wait == Wait::Custodian);
    let posted = Need {
        id: [12; 16],
        want: Want::Notary,
        purpose: Purpose::ByHand,
        ..open.clone()
    };
    let offered = decided(&posted, &facts, &Decision::Offered).unwrap();
    assert_eq!(offered.state, State::Offered);
    let completed = Need {
        id: [13; 16],
        want: Want::Gh(GhProfile::Dev),
        purpose: Purpose::AghDev,
        state: State::Fulfilled,
        wait: Wait::Nobody,
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
        ..withdrawn.clone()
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
        withdrawn,
        silent_on,
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
        "format 4 keeps confirmed completions"
    );
    let file = paths.dir.join(LEDGER);
    let encoded = std::fs::read_to_string(&file).unwrap();
    for format in [1, 2, 3] {
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
            if format < 3 && awaiting_confirmation.contains(&before.id) {
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
            Some(4),
            "older readers refuse it"
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

/// A row an earlier build left awaiting a person reads as a need the lab's Macs answer: a
/// posted one is open on the table, one never posted is this Mac's to post.
#[test]
fn an_earlier_builds_ask_reads_as_a_need_the_lab_answers() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    let row = |id: &str, posted: bool| {
        format!(
            "[[need.rows]]\nid = \"{id}\"\nwant = \"notary\"\npurpose = \"by-hand\"\nstate = \"awaiting-person\"\nwait = \"person\"\nraised = 1\nupdated = 1\nraises = 1\nneeded = true\nasking = true\ngot = false\ngot_authorized = false\nposted = {posted}\nline = \"NEEDS YOU: the notary credential\"\nproof_seq = 4\nproof_absent = true\nproof_every_locker_open = true\nproof_before_commons = true\nproof_swept_unheld = false\n"
        )
    };
    let text = format!(
        "[need]\nformat = 3\n\n{}\n{}",
        row(&"11".repeat(16), true),
        row(&"22".repeat(16), false)
    );
    crate::fsx::ensure_private_dir(&paths.dir).unwrap();
    crate::fsx::write_atomic(&paths.dir.join(LEDGER), text.as_bytes(), 0o600).unwrap();
    let rows = peek(&paths).unwrap();
    assert_eq!(
        rows.iter()
            .map(|need| (need.state, need.wait, need.line.as_str()))
            .collect::<Vec<_>>(),
        [
            (State::Open, Wait::Holders, "waits on the lab's Macs"),
            (State::Local, Wait::Requester, "waits on the lab's Macs"),
        ]
    );
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
        !policy_for(None).allows(&Want::VaultIdentity),
        "the catalog still decides first"
    );
}

/// A stolen Mac's rotation is never posted and never answered by a copy: it stays on this Mac,
/// waiting on its own `set`, while the vault holds the entry and this Mac may post. An
/// `entry:<name>` that another purpose raises is answered from the envelope at once.
///
/// NEGATIVE CONTROL: the same want under a purpose that asks the lab flushes, and the same
/// entry by hand retires `not-needed`.
#[test]
fn a_stolen_macs_rotation_stays_on_this_mac_and_is_never_posted() {
    let mut facts = facts_of(&model_state(&[("kind", 3), ("commons", 1), ("auth", 1)]));
    if let Some(sight) = facts.sight.as_mut() {
        sight.held.insert("gh_access_token".to_owned());
        sight.held.insert("m29_password".to_owned());
    }
    let entry = Want::Entry("gh_access_token".into());
    let filed = settle(
        &fresh([21; 16], entry.clone(), Purpose::StolenRotation, &facts),
        &facts,
    );
    assert_eq!((filed.state, filed.wait), (State::Local, Wait::Requester));
    assert!(filed.line.contains("gh_access_token"), "{}", filed.line);
    let mailbox = fresh(
        [22; 16],
        Want::Mailbox("m29".into()),
        Purpose::StolenRotation,
        &facts,
    );
    assert_eq!(settle(&mailbox, &facts).state, State::Local);
    assert!(act(&mailbox, &facts, &Act::Flush).is_none());

    let asks = fresh(
        [23; 16],
        Want::Mailbox("m29".into()),
        Purpose::ByHand,
        &facts,
    );
    assert_eq!(
        act(&asks, &facts, &Act::Flush).map(|need| need.state),
        Some(State::Open)
    );
    let by_hand = settle(&fresh([24; 16], entry, Purpose::ByHand, &facts), &facts);
    assert_eq!(by_hand.state, State::Retired, "{}", by_hand.line);
    assert!(!Purpose::StolenRotation.asks_the_lab());
    assert_eq!(
        Purpose::parse("revoke.stolen"),
        Some(Purpose::StolenRotation)
    );
}

/// A Claude Code token is answered once the vault's log gives this Mac a token in use here: the
/// need ends `not-needed` as the model's `Unneeded` does, resting on the vault seq this Mac reads
/// it at, from each state it waits in, whatever its purpose (no consumer in this build checks a
/// token for itself). Until then it waits on this Mac's own reading, posted with its one producer
/// named, and no silence turns it to the custodian. NEGATIVE CONTROLS: the token's name alone (a
/// token unreadable here, ended, mismatched, refused here, or unverified and minted elsewhere)
/// answers nothing; a mailbox the vault gives this Mac is not answered so; and the policy decides
/// before the arrival is read.
#[test]
fn a_claude_token_need_ends_when_a_token_in_use_here_reaches_the_vaults_log() {
    let facts = facts_of(&model_state(&[("kind", 3), ("commons", 1)]));
    let want = Want::parse("claude-token:ac-00000000000000a1").unwrap();
    let entry = want.entry().unwrap();
    let mut named = facts.clone();
    if let Some(sight) = named.sight.as_mut() {
        sight.held.insert(entry.clone());
        sight.held.insert("m29_password".into());
    }
    let mut held = named.clone();
    held.in_use.insert(entry);

    let local = settle(
        &fresh([31; 16], want.clone(), Purpose::ByHand, &facts),
        &facts,
    );
    assert_eq!((local.state, local.wait), (State::Local, Wait::Requester));
    let open = act(&local, &facts, &Act::Flush).unwrap();
    assert_eq!(
        (open.state, open.wait),
        (State::Open, Wait::Requester),
        "it waits on this Mac's own reading, never on holders"
    );
    assert!(
        open.line.contains(catalog::CLAUDE_TOKEN_PRODUCER),
        "{}",
        open.line
    );
    assert_eq!(
        settle(&open, &facts),
        open,
        "nothing moves it before it arrives"
    );
    assert_eq!(
        settle(&local, &named),
        local,
        "its name alone answers nothing"
    );
    assert_eq!(
        settle(&open, &named),
        open,
        "its name alone answers nothing"
    );
    assert!(
        silent(&open, &facts).is_none(),
        "it never waits on the custodian"
    );
    assert!(
        silent(&open, &held).is_none(),
        "nor reads as a custodian's delivery"
    );

    let offered = Need {
        state: State::Offered,
        wait: Wait::Custodian,
        ..open.clone()
    };
    for waiting in [&local, &open, &offered] {
        let answered = settle(waiting, &held);
        let unneeded = act(waiting, &held, &Act::Unneeded).unwrap();
        assert_eq!(
            Need {
                retired: unneeded.retired.clone(),
                line: unneeded.line.clone(),
                seq: unneeded.seq,
                ..answered.clone()
            },
            unneeded,
            "the arrival is the model's Unneeded from {:?}",
            waiting.state
        );
        assert_eq!(arrival(waiting, &held), Some(answered.clone()));
        let retired = answered.retired.as_ref().unwrap();
        assert_eq!(
            (retired.reason, retired.evidence.as_str()),
            (Reason::NotNeeded, format!("vault seq {SEQ}").as_str())
        );
        assert_eq!(answered.seq, Some(SEQ));
    }

    let mailbox = settle(
        &fresh(
            [32; 16],
            Want::Mailbox("m29".into()),
            Purpose::ByHand,
            &held,
        ),
        &held,
    );
    assert_eq!(mailbox.state, State::Local, "{}", mailbox.line);
    assert!(arrival(&mailbox, &held).is_none());
    let mailbox = act(&mailbox, &facts, &Act::Flush).unwrap();
    assert_eq!(mailbox.wait, Wait::Holders, "{}", mailbox.line);
    let mut refused = held.clone();
    refused.policy = Policy::compiled(false);
    let unauthorized = settle(&local, &refused);
    assert_eq!(
        unauthorized.retired.map(|retired| retired.reason),
        Some(Reason::NotAuthorized)
    );
    let own = settle(&fresh([33; 16], want, Purpose::HarnessSignIn, &held), &held);
    assert_eq!(
        own.retired.map(|retired| retired.reason),
        Some(Reason::NotNeeded),
        "a purpose whose consumer checks other wants for itself is answered too: {}",
        own.line
    );
}

/// A WITHDRAWAL BY HAND OUTLASTS EVERY OTHER ENDED ROW: past the ledger's bound the oldest ended
/// rows go first, but a withdrawal, which [`ask_for_tokens`] reads as a person's word, goes only
/// once no other ended row is left. CONTROL: the oldest row that is not a withdrawal goes, and an
/// open row is never pruned.
///
/// NEGATIVE CONTROL (2026-11-14, restored with cp from a backup plus touch, confirmed with cmp):
/// with `save` pruning by `updated` alone, `the withdrawal is kept` fails.
#[test]
fn a_withdrawal_by_hand_is_pruned_after_every_other_ended_row() {
    let facts = facts_of(&model_state(&[("kind", 3), ("commons", 1)]));
    let want = Want::parse("claude-token:ac-00000000000000a1").unwrap();
    let open = fresh([1; 16], want.clone(), Purpose::HarnessSignIn, &facts);
    let withdrawn = Need {
        updated: 1,
        ..act(&open, &facts, &Act::Withdraw("by hand".into())).unwrap()
    };
    assert!(withdrawn.withdrawn_by_hand(), "{}", withdrawn.line);
    let answered = act(&open, &facts, &Act::Unneeded).unwrap();
    assert!(!answered.withdrawn_by_hand(), "{}", answered.line);
    let mut rows = vec![withdrawn.clone()];
    for at in 2..u64::try_from(ledger::MAX_ROWS).unwrap() + 2 {
        let mut id = [0; 16];
        id[..8].copy_from_slice(&at.to_le_bytes());
        rows.push(Need {
            id,
            want: Want::Mailbox(format!("m{at}")),
            updated: at,
            ..answered.clone()
        });
    }
    let oldest_other = rows[1].id;
    rows.push(Need {
        id: [99; 16],
        updated: 0,
        ..open
    });
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    ledger::save(&paths, &mut rows).unwrap();
    let kept = peek(&paths).unwrap();
    assert_eq!(kept.len(), ledger::MAX_ROWS);
    assert!(
        kept.iter().any(|need| need.id == withdrawn.id),
        "the withdrawal is kept"
    );
    assert!(kept.iter().all(|need| need.id != oldest_other), "CONTROL");
    assert!(kept.iter().any(|need| need.id == [99; 16]), "CONTROL: open");
}

/// OUTSIDE THE LAB'S POLICY THE HARNESS'S ASK ENDS AT ONCE AND IS NOT REPEATED: in a vault whose
/// policy gives nothing by request, the raise is retired `not-authorized` as it is made; asked
/// again under the same policy, it changes and writes nothing.
#[test]
fn outside_the_labs_policy_the_harness_ask_ends_not_authorized_and_writes_nothing_more() {
    let vault = vault();
    let key = crate::claude_token::Key::parse("ac-00000000000000a1").unwrap();
    let first = ask_for_tokens(&vault.member, std::slice::from_ref(&key)).unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(
        first[0].retired.as_ref().map(|r| r.reason),
        Some(Reason::NotAuthorized),
        "{}",
        first[0].line
    );
    let ledger = vault.member.dir.join(LEDGER);
    let bytes = std::fs::read(&ledger).unwrap();
    assert!(
        ask_for_tokens(&vault.member, std::slice::from_ref(&key))
            .unwrap()
            .is_empty()
    );
    assert_eq!(std::fs::read(&ledger).unwrap(), bytes, "nothing written");
}

/// THE HARNESS'S ASK FILES A TOKEN'S REQUEST ONCE, AND AGAIN ONLY AFTER A TOKEN ANSWERED IT
/// (DESIGN-claude-setup-token §5.9): with none on file, [`ask_for_tokens`] raises
/// `claude-token:<key>` under the sign-in's purpose; asked again while it is under way it raises
/// nothing and writes nothing; once a token in use here answered it (the ledger's own settle,
/// retiring it `not-needed` on the vault seq) and has left use, the next ask raises a new request;
/// a withdrawal by hand stands, writing nothing, until a person raises the want by hand again.
/// CONTROL: another key with nothing on file is still asked beside the withdrawn one.
///
/// NEGATIVE CONTROL (2026-11-14, restored with cp from a backup plus touch, confirmed with cmp):
/// with `ask_for_tokens` raising only where every need on file is `fulfilled` (the rule a review
/// found), `after its token answered it` reads `left: 0`, `right: 1`.
#[test]
fn the_harness_asks_for_a_token_once_and_again_after_a_token_answered_it() {
    let vault = vault();
    crate::need::test_alab_policy(&vault.member);
    let key = crate::claude_token::Key::parse("ac-00000000000000a1").unwrap();
    let want = Want::ClaudeToken(key.clone());
    let entry = want.entry().unwrap();
    let ask = |keys: &[crate::claude_token::Key]| ask_for_tokens(&vault.member, keys).unwrap();
    let first = ask(std::slice::from_ref(&key));
    assert_eq!(first.len(), 1);
    assert_eq!(
        (&first[0].want, first[0].purpose),
        (&want, Purpose::HarnessSignIn)
    );
    assert!(!first[0].state.ended(), "{}", first[0].line);
    let ledger = vault.member.dir.join(LEDGER);
    let bytes = std::fs::read(&ledger).unwrap();
    assert_eq!(ask(std::slice::from_ref(&key)).len(), 0, "asked again");
    assert_eq!(std::fs::read(&ledger).unwrap(), bytes, "nothing written");

    // A token in use here arrives: the ledger's settle answers the request.
    with_ledger(&vault.member, |rows, facts| {
        let mut arrived = facts.clone();
        arrived.sight.as_mut().unwrap().held.insert(entry.clone());
        arrived.in_use.insert(entry.clone());
        for need in rows.iter_mut() {
            *need = settle(need, &arrived);
        }
        Ok(())
    })
    .unwrap();
    let answered = &peek(&vault.member).unwrap()[0];
    assert_eq!(
        answered.retired.as_ref().map(|r| r.reason),
        Some(Reason::NotNeeded),
        "{}",
        answered.line
    );
    assert!(!answered.withdrawn_by_hand(), "{}", answered.line);

    // That token has left use (the scratch vault holds none): the harness asks again.
    let again = ask(std::slice::from_ref(&key));
    assert_eq!(again.len(), 1, "after its token answered it");
    assert_ne!(again[0].id, first[0].id, "a new request");
    assert!(!again[0].state.ended(), "{}", again[0].line);

    retire(&vault.member, &want.key(), "by hand").unwrap();
    let other = crate::claude_token::Key::parse("ac-00000000000000b2").unwrap();
    let both = ask(&[key.clone(), other.clone()]);
    assert_eq!(
        both.iter().map(|need| &need.want).collect::<Vec<_>>(),
        vec![&Want::ClaudeToken(other)],
        "a withdrawal stands; CONTROL: the other key is asked"
    );
    let bytes = std::fs::read(&ledger).unwrap();
    assert!(
        ask(std::slice::from_ref(&key)).is_empty(),
        "it still stands"
    );
    assert_eq!(std::fs::read(&ledger).unwrap(), bytes, "nothing written");

    // A person raises it by hand again: that request is under way, so the harness adds none.
    let by_hand = raise(&vault.member, want.clone(), Purpose::ByHand).unwrap();
    assert!(!by_hand.state.ended(), "{}", by_hand.line);
    assert!(
        ask(std::slice::from_ref(&key)).is_empty(),
        "one is under way"
    );
}
