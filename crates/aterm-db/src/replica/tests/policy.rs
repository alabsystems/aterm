// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::RefCell;

type Hook = (u8, Box<dyn FnOnce()>);
thread_local! {
    static SESSION_BOUNDARY: RefCell<Option<Hook>> = RefCell::new(None);
}

pub(in crate::replica) fn session_boundary(at: u8) {
    let callback = SESSION_BOUNDARY.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.as_ref().is_some_and(|(point, _)| *point == at) {
            slot.take()
        } else {
            None
        }
    });
    if let Some((_, callback)) = callback {
        callback();
    }
}

struct Change;
impl Change {
    fn install(at: u8, callback: impl FnOnce() + 'static) -> Self {
        SESSION_BOUNDARY.with(|slot| {
            assert!(
                slot.borrow_mut()
                    .replace((at, Box::new(callback)))
                    .is_none()
            );
        });
        Self
    }
}
impl Drop for Change {
    fn drop(&mut self) {
        SESSION_BOUNDARY.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}

#[test]
fn a_policy_change_fires_once_at_only_its_selected_session_boundary() {
    for point in [0, 1] {
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let invoked = calls.clone();
        let _change = Change::install(point, move || invoked.set(invoked.get() + 1));
        session_boundary(1 - point);
        assert_eq!(
            calls.get(),
            0,
            "the other boundary must not consume the change"
        );
        session_boundary(point);
        assert_eq!(
            calls.get(),
            1,
            "the selected boundary must apply the change"
        );
        session_boundary(point);
        assert_eq!(calls.get(), 1, "a boundary change must not run twice");
    }
}

#[test]
fn policy_changes_before_session_open_refuse_the_old_round() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let config = lab.paths[0].home.join("config.toml");
    let replica = Replica::commons(lab.paths[0].clone(), config.clone()).unwrap();
    let peer = lab.bootstrap(1).request().unwrap().binding().actor();
    let round = replica.round(peer).unwrap();
    let _change = Change::install(0, move || {
        aterm_secrets::fsx::write_atomic(&config, b"[commons]\nenabled=false\n", 0o600).unwrap();
    });
    let called = std::cell::Cell::new(false);
    let result = round.local(peer, |_| {
        called.set(true);
        Ok(())
    });
    assert!(result.is_err(), "late disable must refuse the local batch");
    assert!(!called.get(), "a refused batch must never run");
}

#[derive(Default)]
struct Stream {
    input: Cursor<Vec<u8>>,
    output: Vec<u8>,
}
impl Read for Stream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.input.read(bytes)
    }
}
impl Write for Stream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.output.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct Fixture {
    lab: Lab,
    config: PathBuf,
    directory: crate::catalog::Directory,
    scope: Scope,
    peer: Actor,
    original: Vec<u8>,
    incoming: Vec<u8>,
    observation: [u8; 64],
}
impl Fixture {
    fn new() -> Self {
        let lab = Lab::new(&[Role::Writer, Role::Writer]);
        let config = lab.paths[0].home.join("config.toml");
        let a = lab.bootstrap(0);
        let directory = crate::catalog::Directory::new(&lab.paths[0], a.trust());
        let session = Session::open(&a).unwrap();
        let scope = directory
            .open_application(&session, "accounts", "proposals", 1)
            .unwrap();
        let mut store = directory.enroll(&session, scope).unwrap();
        let receipt = store
            .commit_catalog(&session, b"retained account proposal")
            .unwrap();
        let original = store.wire(&session, &receipt.id).unwrap().unwrap().to_vec();
        let observation = session.observation().unwrap();
        drop((store, session));
        let b = lab.bootstrap(1);
        let peer = b.request().unwrap().binding().actor();
        let other = crate::catalog::Directory::new(&lab.paths[1], b.trust());
        let session = Session::open(&b).unwrap();
        assert_eq!(
            other
                .open_application(&session, "accounts", "proposals", 1)
                .unwrap(),
            scope
        );
        let mut store = other.enroll(&session, scope).unwrap();
        let receipt = store
            .commit_catalog(&session, b"incoming account proposal")
            .unwrap();
        let incoming = store.wire(&session, &receipt.id).unwrap().unwrap().to_vec();
        drop((store, session));
        Self {
            lab,
            config,
            directory,
            scope,
            peer,
            original,
            incoming,
            observation,
        }
    }
    fn config(&self, policy: i64) -> Vec<u8> {
        let excluded = match policy {
            2 => [77; 32],
            4 => self.scope.digest(),
            5 => self.directory.catalog_scope().digest(),
            _ => [0; 32],
        };
        match policy {
            0 => b"[commons]\n".to_vec(),
            1 => b"# semantically unchanged\n[commons]\nexclude_tables=[]\nenabled=true\n".to_vec(),
            2 | 4 | 5 => format!(
                "[commons]\nexclude_tables=['{}']\n",
                aterm_codec::hex::encode_lower(&excluded)
            )
            .into_bytes(),
            3 => b"[commons]\nenabled=false\n".to_vec(),
            6 => b"[commons]\nenabled='false'\n".to_vec(),
            7 => b"[commons]\nexclude_table=[]\n".to_vec(),
            _ => panic!("unknown policy scenario"),
        }
    }
    fn batch(&self, round: &Round<'_>, receiving: bool) -> (bool, bool) {
        round.active_table.set(Some(self.scope.digest()));
        let mut stream = Stream::default();
        let mut report = Report::default();
        let result = if receiving {
            let mut wire = 1u32.to_le_bytes().to_vec();
            control::send_frame(&mut wire, &self.incoming).unwrap();
            stream.input = Cursor::new(wire);
            round.receive_batch(
                &mut stream,
                self.peer,
                self.scope.digest(),
                BatchKind::Operation,
                &mut report,
            )
        } else {
            let mut receipt = Sha256::digest(&self.original).to_vec();
            receipt.push(1);
            stream.input = Cursor::new(receipt);
            round.send_batch(
                &mut stream,
                self.peer,
                std::slice::from_ref(&self.original),
                BatchKind::Operation,
                &mut report,
            )
        };
        if result.is_ok() {
            if receiving {
                let mut receipt = Sha256::digest(&self.incoming).to_vec();
                receipt.push(1);
                assert_eq!(stream.output, receipt);
                assert_eq!(report.received, 1);
            } else {
                let mut expected = 1u32.to_le_bytes().to_vec();
                control::send_frame(&mut expected, &self.original).unwrap();
                assert_eq!(stream.output, expected);
                assert_eq!(report.sent, 1);
            }
        } else {
            assert!(
                stream.output.is_empty(),
                "refusal emitted a frame or receipt"
            );
            assert_eq!(report, Report::default());
        }
        (result.is_ok(), !stream.output.is_empty())
    }
    fn observe(&self, accepted: bool, receipts: bool) -> (i64, i64, i64) {
        let bootstrap = self.lab.bootstrap(0);
        let session = Session::open(&bootstrap).unwrap();
        assert_eq!(session.observation().unwrap(), self.observation);
        let store = self.directory.enroll(&session, self.scope).unwrap();
        assert_eq!(
            store
                .wire(&session, &Sha256::digest(&self.original))
                .unwrap()
                .unwrap(),
            self.original
        );
        (
            i64::from(accepted),
            i64::from(receipts),
            store.inventory(&session).unwrap().count() as i64,
        )
    }
    fn round<'a>(&self, replica: &'a Replica) -> Result<Round<'a>> {
        let mut round = replica.round(self.peer)?;
        round.tables.insert(
            self.scope.digest(),
            (self.scope, self.directory.path(self.scope).unwrap()),
        );
        Ok(round)
    }
}

fn scenario(policy: i64, receiving: bool, point: u8) -> [(i64, i64, i64); 3] {
    let fixture = Fixture::new();
    let replica = Replica::commons(fixture.lab.paths[0].clone(), fixture.config.clone()).unwrap();
    let round = fixture.round(&replica).unwrap();
    let config = fixture.config.clone();
    let bytes = fixture.config(policy);
    let changed = std::rc::Rc::new(std::cell::Cell::new(false));
    let invoked = changed.clone();
    let change = Change::install(point, move || {
        aterm_secrets::fsx::write_atomic(&config, &bytes, 0o600).unwrap();
        invoked.set(true);
    });
    let (accepted, receipts) = fixture.batch(&round, receiving);
    assert!(
        changed.get(),
        "the policy fault must execute at session admission"
    );
    drop(change);
    let first = fixture.observe(accepted, receipts);
    let (accepted, receipts) = fixture
        .round(&replica)
        .map(|round| fixture.batch(&round, receiving))
        .unwrap_or((false, false));
    let retry = fixture.observe(accepted, receipts);
    aterm_secrets::fsx::write_atomic(&fixture.config, &fixture.config(0), 0o600).unwrap();
    let (accepted, receipts) = fixture.batch(&fixture.round(&replica).unwrap(), receiving);
    let restored = fixture.observe(accepted, receipts);
    [first, retry, restored]
}

/// [`scenario`] once per fixture selection: Attempt, Retry and Restore read the three
/// phases of one real run, so a selection runs once, not once per phase.
fn scenario_once(policy: i64, receiving: bool, point: u8) -> [(i64, i64, i64); 3] {
    type Runs = BTreeMap<(i64, bool, u8), [(i64, i64, i64); 3]>;
    static RUNS: std::sync::Mutex<Runs> = std::sync::Mutex::new(BTreeMap::new());
    let key = (policy, receiving, point);
    if let Some(run) = RUNS.lock().unwrap().get(&key) {
        return *run;
    }
    let run = scenario(policy, receiving, point);
    *RUNS.lock().unwrap().entry(key).or_insert(run)
}

fn policy_transition(state: &aterm_spec::interp::State, action: &str) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "NextPolicy" => {
            next.insert("policy", state["policy"] + 1);
        }
        "Receiving" => {
            next.insert("receiving", 1);
        }
        "AfterOpen" => {
            next.insert("point", 1);
        }
        "Attempt" | "Retry" | "Restore" => {
            let index = match action {
                "Attempt" => 0,
                "Retry" => 1,
                _ => 2,
            };
            let (accepted, receipts, retained) = scenario_once(
                state["policy"],
                state["receiving"] == 1,
                state["point"] as u8,
            )[index];
            next.insert("phase", index as i64 + 1);
            next.insert("accepted", accepted);
            next.insert("receipts", receipts);
            next.insert("retained", retained);
        }
        _ => panic!("unknown policy action"),
    }
    next
}

#[test]
fn policy_admission_conforms_and_stale_policy_and_false_receipts_are_detected() {
    let model = aterm_models_db::repair_policy_model();
    assert_eq!(aterm_spec::interp::prove_and_catch(&model), 128);
    assert_eq!(
        aterm_spec::interp::fired_actions(&model).len(),
        model.actions.len()
    );
    let states = aterm_spec::interp::reachable_states(&model);
    // Every selection's real run, each on its own Lab, on every core.
    let selections: Vec<_> = states
        .iter()
        .map(|state| {
            (
                state["policy"],
                state["receiving"] == 1,
                state["point"] as u8,
            )
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    aterm_spec::check::each_on(
        aterm_spec::fs_lane::PLACES,
        &selections,
        |&(policy, receiving, point)| scenario_once(policy, receiving, point),
    );
    for state in states {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = policy_transition(&state, action.name);
                let successors = model.successors(action.name, &state);
                assert!(
                    successors.contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
                if matches!(action.name, "NextPolicy" | "Receiving" | "AfterOpen") {
                    assert!(
                        !successors.contains(&state),
                        "{}: ignoring a fixture selection must fail conformance",
                        action.name
                    );
                }
            }
        }
    }
    let mut changed = model.init_state();
    changed.insert("policy", 3);
    let mut forged = policy_transition(&changed, "Attempt");
    forged.insert("accepted", 1);
    assert!(!model.successors("Attempt", &changed).contains(&forged));
    assert!(!model.check_invariant("LateChangeRefuses", &forged));
    forged.insert("accepted", 0);
    forged.insert("receipts", 1);
    assert!(!model.check_invariant("NoFalseReceipt", &forged));
}
