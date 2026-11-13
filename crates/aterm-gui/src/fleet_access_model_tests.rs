// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `FleetAccessLock` against the production decode and admission: every class of stored bytes,
//! every [`DriveKind`] by a line that carries it, and both grant answers by the scope the
//! listener's admission hands [`drive_admission`].

use super::*;
use aterm_spec::interp::State;

/// The bytes a state file may hold for the model's `stored`: for 0, every class that names no
/// level (no file, an empty one, garbage, an unknown word, two `level=` lines, bytes that are
/// no text); for 1 to 3, the level's own file, bare and as the writer writes it.
fn stored_bytes(stored: i64) -> Vec<Option<Vec<u8>>> {
    let file = |text: &str| Some(text.as_bytes().to_vec());
    match stored {
        0 => vec![
            None,
            file(""),
            file("garbage\n"),
            file("level=wide\n"),
            file("level=full\nlevel=full\n"),
            file("level=read\nlevel=write\n"),
            Some(b"level=full\xff\n".to_vec()),
        ],
        1..=3 => {
            let word = FleetAccess::ALL[usize::try_from(stored - 1).unwrap()].word();
            vec![
                file(&format!("level={word}\n")),
                file(&format!("level={word}\nat=1793160000\nby=cli\n")),
            ]
        }
        _ => panic!("unknown stored class"),
    }
}

fn need_of(kind: DriveKind) -> i64 {
    match kind.needs() {
        FleetAccess::ReadOnly => 1,
        FleetAccess::ReadWrite => 2,
        FleetAccess::Full => 3,
    }
}

/// A line that carries an act of `kind`, or `None` where no line does: the mesh's `bus.log`
/// stream is no line, and `vault` is a unix service. The session each names is hosted nowhere:
/// the gate refuses that after the lock has judged the line, which is all this asks.
fn line_of(kind: DriveKind) -> Option<String> {
    use DriveKind as K;
    const W: &str = "s-00000000000000aa";
    const NONCE: &str = "0123456789abcdef0123456789abcdef";
    match kind {
        K::Read => Some(format!("@{W} status")),
        K::ShellRead => Some(format!("@{W} blocks")),
        K::Vault => cfg!(unix).then(|| format!("vault approve abcd-efgh-ijkl-mnop {NONCE}")),
        K::ServiceRepair => Some(
            r#"service-repair {"schema":1,"op":"recover","payload":{"job":"subscriber"}}"#
                .to_owned(),
        ),
        K::AccountsRenew => Some(format!("accounts-renew start={NONCE}")),
        K::Post => Some(format!("@{W} post key=k kind=note -- hi")),
        K::MailSeen => Some(format!("@{W} inbox seen f1")),
        K::Turn => Some(format!("@{W} turn hi")),
        K::Key => Some(format!("@{W} key Enter")),
        K::Send => Some(format!("@{W} send hi")),
        K::Hold => Some(format!("hold {W} on")),
        K::Close => Some(format!("@{W} close")),
        K::BusLog => None,
        K::Spawn => Some("spawn agent=claude len=5".to_owned()),
        K::Claim => Some("request claim path=/tmp/x req=r-20261111-1200-87be5228-01".to_owned()),
    }
}

/// The production verdict for one stored file and one kind, as the model's `admitted`: whether
/// the kind's line, sent on a connection whose scope says what the grants answered, reached the
/// gate ([`drive_admission`]). The test decides neither half: a scope with no drive grant is
/// never asked the lock, and a granted one is refused by it before the gate looks at anything.
///
/// `None` where this harness can run no production verdict. The `bus.log` stream is admitted in
/// the node job (`node_serve`'s `LinkServe::serve`), which asks [`drive_lock_refusal`] only of
/// a peer it admitted as a driver: a granted stream's verdict is that call's, and an ungranted
/// one's is that job's own, held by
/// `a_members_bus_log_stream_is_admitted_as_a_driver_and_served_its_ring`.
fn admitted(bytes: Option<&[u8]>, kind: DriveKind, granted: bool) -> Option<i64> {
    let level = aterm_agent::fleet_access::decode(bytes);
    let Some(line) = line_of(kind) else {
        return (kind == DriveKind::BusLog && granted)
            .then(|| i64::from(drive_lock_refusal(level, kind, kind.word()).is_none()));
    };
    let request = Request::parse(line);
    assert_eq!(drive_kind(&request.head()), Ok(kind), "{}", request.line());
    let was = FLEET_ACCESS.with(|now| now.replace(Some(level)));
    let _driving = crate::fabric::DrivingAs::enter(crate::fabric::Driver {
        node: "05".repeat(16),
        host: "mac-05".to_owned(),
    });
    DRIVE_GATED.with(|gated| gated.set(0));
    let scope = Scope::Fleet {
        content: true,
        drive: granted,
    };
    let answer = drive_admission(request, scope, &crate::session_store::new_store());
    FLEET_ACCESS.with(|now| now.set(was));
    let gated = DRIVE_GATED.with(std::cell::Cell::get);
    if !granted {
        // No drive grant: the line keeps its fleet scope, under which no act is served.
        assert!(
            matches!(answer, Ok((_, Scope::Fleet { drive: false, .. }, None))),
            "{kind:?}: an ungranted line was made an act"
        );
    }
    Some(i64::try_from(gated).unwrap())
}

/// Project the production verdicts for `state`'s setup, one per stored file and kind of its
/// classes: every one must be a successor the model allows.
pub(crate) fn project(state: &State, action: &str) -> Vec<State> {
    let step = |key: &'static str| {
        let mut next = state.clone();
        next.insert(key, state[key] + 1);
        vec![next]
    };
    match action {
        "Stored" => step("stored"),
        "Need" => step("need"),
        "Granted" => step("granted"),
        "Judge" => {
            let mut all = Vec::new();
            for bytes in stored_bytes(state["stored"]) {
                for kind in DriveKind::ALL {
                    if need_of(kind) != state["need"] {
                        continue;
                    }
                    let Some(admitted) = admitted(bytes.as_deref(), kind, state["granted"] == 1)
                    else {
                        continue;
                    };
                    let mut next = state.clone();
                    next.insert("admitted", admitted);
                    next.insert("phase", 1);
                    all.push(next);
                }
            }
            all
        }
        _ => panic!("unknown lock action"),
    }
}

#[test]
fn the_lock_conforms_and_rejects_an_opened_or_ungranted_admission() {
    let model = aterm_models_swarm::fleet_access_lock_model();
    // The global aterm-models non_vacuity test owns the complete fault sweep.
    assert!(aterm_spec::interp::bmc(&aterm_spec::interp::with_buggy(&model, 1)).is_err());
    let (mut judged, mut verdicts) = (0, 0);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if !model.action_enabled(action.name, &state) {
                continue;
            }
            let actual = project(&state, action.name);
            assert!(!actual.is_empty(), "{}: {state:?}", action.name);
            for next in &actual {
                assert!(
                    model.successors(action.name, &state).contains(next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
            if action.name == "Judge" {
                judged += 1;
                verdicts += actual.len();
            }
        }
    }
    assert_eq!(
        judged, 24,
        "every stored class, need and grant answer is judged"
    );
    // Seven unusable files and two per level, over every kind a line carries, granted or not,
    // and the granted `bus.log` stream.
    let lined = DriveKind::ALL
        .into_iter()
        .filter(|kind| line_of(*kind).is_some())
        .count();
    assert_eq!(verdicts, (7 + 3 * 2) * (lined * 2 + 1));

    // NEGATIVE CONTROL: a state the production code never reaches is no successor, and breaks
    // the law it would break. A file naming no level that admitted a spawn:
    let mut unknown = model.init_state();
    unknown.insert("granted", 1);
    unknown.insert("need", 3);
    let mut opened = project(&unknown, "Judge").remove(0);
    assert_eq!(opened["admitted"], 0, "production fails closed");
    opened.insert("admitted", 1);
    assert!(!model.successors("Judge", &unknown).contains(&opened));
    assert!(!model.check_invariant("FailClosed", &opened));
    assert!(!model.check_invariant("NoOverAdmit", &opened));
    // Full Access that admitted a driver no grant names:
    let mut ungranted = model.init_state();
    ungranted.insert("stored", 3);
    let mut admitted = project(&ungranted, "Judge").remove(0);
    assert_eq!(admitted["admitted"], 0, "the lock grants nothing");
    admitted.insert("admitted", 1);
    assert!(!model.successors("Judge", &ungranted).contains(&admitted));
    assert!(!model.check_invariant("Ceiling", &admitted));
    // And a read refused at Read Only:
    let mut reading = model.init_state();
    reading.insert("granted", 1);
    let mut refused = project(&reading, "Judge").remove(0);
    assert_eq!(refused["admitted"], 1, "every level keeps the reads");
    refused.insert("admitted", 0);
    assert!(!model.successors("Judge", &reading).contains(&refused));
    assert!(!model.check_invariant("ReadsStay", &refused));
}
