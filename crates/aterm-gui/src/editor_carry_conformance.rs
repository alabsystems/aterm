// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 of `aterm_spec::derive::native_update_editor_carry_model`
//! (`NativeUpdateEditorCarry`, gap #32): an unsaved native editor tab rides a
//! self-update instead of holding it.
//!
//! Each schedule drives two real `App`s over ONE journal directory — the
//! outgoing process and its successor, as two processes of one user share
//! `drafts/` — through the shipping code: the editor's own input, the
//! preflight and the Commit revalidation (`revalidate_native_update_safety`,
//! the one the handoff's Commit admission calls), the successor's restore of
//! the layout the outgoing process captured (`restore_into_window`, which
//! reopens the file and replays the draft journal), and the rollback (the
//! completion reducer's `rollback_overlap`, and the launched lane's
//! stand-down). After every real step the whole machine is projected
//! from real state — the editor's text, the journal store's proved sequence,
//! the image actually on disk, the successor's editor — and must equal the
//! model's state after the named actions; the real Start and Commit verdicts
//! must BE the model's guards; and at each refusal the defect machines must
//! admit what the real code refused, so no pass is vacuous.

use std::path::PathBuf;

use aterm_spec::derive::{Model, native_update_editor_carry_model};
use aterm_spec::interp::{self, State};

use crate::app_documents::DraftCarry;
use crate::app_native::{CARRIED_DRAFTS_CAP_BYTES, DraftCarryTally, NativeUpdateSafetyToken};
use crate::document_store::DocumentId;
use crate::native_app::{AppEvent, AppKind, AppViewState, TextInputEvent};
use crate::native_document_io::{ContentFingerprint, JournalDocumentKey, recover_journal_for};
use crate::{App, WindowId};

/// The model's `Placeholder`: the successor's leaf is a Recovery tab.
const PLACEHOLDER: i64 = 9;
const DISK: &str = "base line\n";

/// One outgoing App with one Editor tab over one file, the successor it hands
/// to, and the bookkeeping that names each buffer version the model counts.
struct Rig {
    dir: PathBuf,
    journals: PathBuf,
    uri: String,
    file: PathBuf,
    parent: App,
    document: DocumentId,
    /// Version → the editor's text; version 0 is the file on disk.
    texts: Vec<String>,
    /// Version → the outgoing document's sequence at that text.
    seqs: Vec<aterm_buffer::Seq>,
    phase: i64,
    successor: Option<App>,
    restored: i64,
    succ: i64,
    model: Model,
}

fn wid() -> WindowId {
    WindowId(0)
}

impl Rig {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "aterm-editor-carry-{}-{name}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("draft.md");
        std::fs::write(&file, DISK).unwrap();
        let uri = crate::native_document_host::path_to_file_uri(&file).unwrap();
        let journals = dir.join("drafts");
        let mut parent = App::headless_for_test();
        parent.use_document_journal_root_for_test(journals.clone());
        parent.carry_drafts_as_windowed_for_test();
        parent.open_document_tab(AppKind::Editor, &uri).unwrap();
        let (instance, _) = parent.active_native_view(wid()).expect("editor tab");
        let document = parent.native_runtime.document_id(instance).unwrap();
        let snapshot = parent.document_store.snapshot(document).unwrap();
        assert_eq!(&*snapshot.text, DISK);
        Self {
            dir,
            journals,
            uri,
            file,
            parent,
            document,
            texts: vec![DISK.to_string()],
            seqs: vec![snapshot.seq],
            phase: 0,
            successor: None,
            restored: 0,
            succ: 0,
            model: native_update_editor_carry_model(),
        }
    }

    fn version_of_text(&self, text: &str) -> i64 {
        let index = self
            .texts
            .iter()
            .position(|known| known == text)
            .unwrap_or_else(|| panic!("a text no version names: {text:?}"));
        i64::try_from(index).unwrap()
    }

    fn version_of_seq(&self, seq: aterm_buffer::Seq) -> i64 {
        let index = self
            .seqs
            .iter()
            .position(|known| *known == seq)
            .unwrap_or_else(|| panic!("a sequence no version names: {seq:?}"));
        i64::try_from(index).unwrap()
    }

    fn key(&self) -> JournalDocumentKey {
        JournalDocumentKey::for_canonical_uri(
            self.parent
                .document_store
                .canonical_uri(self.document)
                .unwrap(),
        )
    }

    fn image(&self) -> Vec<u8> {
        std::fs::read(
            self.parent
                .document_journal_path_for_test(self.document)
                .unwrap(),
        )
        .unwrap()
    }

    /// Every model variable, read from real state.
    fn project(&self) -> State {
        let mut state = self.model.init_state();
        let head = self.parent.document_store.snapshot(self.document).unwrap();
        state.insert("head", self.version_of_text(&head.text));
        let held = self
            .parent
            .held_journal_appends_for_test()
            .iter()
            .find(|held| held.document == self.document);
        state.insert("inflight", i64::from(held.is_some()));
        state.insert(
            "target",
            held.map_or(0, |held| self.version_of_seq(held.plan.target_seq)),
        );
        state.insert(
            "mem",
            self.version_of_seq(
                self.parent
                    .document_journal_durable_seq_for_test(self.document)
                    .unwrap(),
            ),
        );
        let image = self.image();
        let recovered = recover_journal_for(self.key(), &image).expect("the image replays");
        state.insert("journal", self.version_of_text(&recovered.text));
        state.insert(
            "owner",
            i64::from(
                Some(ContentFingerprint::of(&image))
                    != self.parent.document_journal_image_for_test(self.document),
            ),
        );
        state.insert(
            "reseat",
            i64::from(
                self.parent
                    .document_journal_reseat_owed_for_test(self.document),
            ),
        );
        state.insert("phase", self.phase);
        state.insert("restored", self.restored);
        state.insert("succ", self.succ);
        state.insert(
            "late",
            i64::from(self.parent.document_journal_landed_while_parked()),
        );
        state
    }

    /// The real step `op` IS the model's `actions`, fired in order from the
    /// state it started in, and every invariant holds after it.
    fn step(&mut self, label: &str, actions: &[&str], op: impl FnOnce(&mut Self)) {
        let before = self.project();
        op(self);
        let after = self.project();
        let mut expected = before.clone();
        for action in actions {
            assert!(
                self.model.fire(action, &mut expected),
                "{label}: the model cannot fire {action} from {expected:?} (real: {before:?} -> \
                 {after:?})"
            );
        }
        assert_eq!(
            after, expected,
            "{label}: the real step is not {actions:?} (from {before:?})"
        );
        for invariant in &self.model.invariants {
            assert!(
                self.model.check_invariant(invariant.name, &after),
                "{label}: {} broken at {after:?}",
                invariant.name
            );
        }
    }

    /// One keystroke in the outgoing editor, and the version it makes.
    fn type_key(&mut self, label: &str, key: &str, actions: &[&str]) {
        let key = key.to_string();
        self.step(label, actions, |rig| {
            rig.parent
                .dispatch_native_event(
                    wid(),
                    AppEvent::TextInput(TextInputEvent::Commit(key.clone())),
                )
                .unwrap();
            let snapshot = rig.parent.document_store.snapshot(rig.document).unwrap();
            rig.texts.push(snapshot.text.to_string());
            rig.seqs.push(snapshot.seq);
        });
    }

    /// The real preflight must BE the model's Start guard. Admitted, the
    /// attempt parks at once (the fork lane): the attempt record the Commit
    /// check and the journal fences read is installed.
    fn start(&mut self, label: &str) -> bool {
        let before = self.project();
        let real = self.parent.revalidate_native_update_safety().is_ok();
        assert_eq!(
            real,
            self.model.action_enabled("Start", &before),
            "{label}: the real preflight is not the model's Start guard at {before:?}"
        );
        if real {
            self.step(label, &["Start", "Park"], |rig| {
                rig.parent.pending_update_handoff = Some(parked_record(FORKED));
                rig.phase = 2;
            });
        }
        real
    }

    /// The successor boots from the layout the outgoing process captures now
    /// and restores the editor leaf exactly as the update's successor does.
    fn successor_restores(&mut self, label: &str, action: &str) {
        let layout = editor_only_layout(&self.parent.capture_restore_manifest());
        let journals = self.journals.clone();
        self.step(label, &[action], move |rig| {
            let mut successor = App::headless_for_test();
            successor.use_document_journal_root_for_test(journals);
            successor.incoming_handoff_pending = true;
            successor.restore_into_window(wid(), layout);
            let (instance, _) = successor.active_native_view(wid()).expect("restored leaf");
            rig.succ = match successor.native_runtime.app(instance).map(|app| app.kind()) {
                Some(AppKind::Recovery) => PLACEHOLDER,
                Some(AppKind::Editor) => {
                    let document = successor.native_runtime.document_id(instance).unwrap();
                    let text = successor.document_store.snapshot(document).unwrap().text;
                    rig.version_of_text(&text)
                }
                other => panic!("{label}: the successor restored {other:?}"),
            };
            rig.restored = 1;
            rig.successor = Some(successor);
        });
    }

    /// The real Commit revalidation must BE the model's Commit guard.
    fn commit(&mut self, label: &str) -> bool {
        let before = self.project();
        let real = self.parent.revalidate_native_update_safety().is_ok();
        assert_eq!(
            real,
            self.model.action_enabled("Commit", &before),
            "{label}: the real Commit check is not the model's Commit guard at {before:?}"
        );
        if real {
            self.step(label, &["Commit"], |rig| rig.phase = 3);
        }
        real
    }

    /// The candidate is killed and the outgoing process rolls back — through
    /// the real completion reducer, which runs the rollback for a parked
    /// attempt.
    fn rollback(&mut self, label: &str, actions: &[&str]) {
        self.step(label, actions, |rig| {
            rig.successor = None;
            assert!(
                rig.parent
                    .reduce_returned_handoff_completion(rejected(FORKED))
                    .is_some()
            );
            assert!(!rig.parent.update_handoff_in_flight());
            rig.phase = 0;
            rig.restored = 0;
            rig.succ = 0;
        });
    }

    fn run_held(&mut self, label: &str, actions: &[&str]) {
        self.step(label, actions, |rig| {
            assert!(rig.parent.run_held_journal_append_for_test());
        });
    }

    fn successor_editor(&self) -> (String, bool, Vec<crate::native_editor::Selection>) {
        let successor = self.successor.as_ref().expect("a successor");
        let (instance, view) = successor.active_native_view(wid()).unwrap();
        let document = successor.native_runtime.document_id(instance).unwrap();
        let text = successor
            .document_store
            .snapshot(document)
            .unwrap()
            .text
            .to_string();
        let dirty = successor.document_store.dirty(document) == Some(true);
        let Some(AppViewState::Editor(editor)) = successor.native_runtime.view_state(view) else {
            panic!("the successor's leaf is an editor");
        };
        let selections = editor.buffer.as_ref().unwrap().selections.clone();
        (text, dirty, selections)
    }

    fn assert_file_untouched(&self) {
        assert_eq!(
            std::fs::read_to_string(&self.file).unwrap(),
            DISK,
            "the file on disk is never written by a carry"
        );
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The fork lane's attempt: launched and parked at once.
const FORKED: u64 = 1;

/// The attempt record the park installs. The same-image QA seam's authority,
/// so the completion reducer writes no update ledger.
fn parked_record(attempt_id: u64) -> crate::PendingUpdateHandoff {
    let (cancel, _cancelled) = std::sync::mpsc::sync_channel(1);
    crate::PendingUpdateHandoff {
        park_at: std::time::Instant::now(),
        proof_ready_at: None,
        #[cfg(target_os = "macos")]
        activate_at_commit: false,
        attempt_id,
        nonce: None,
        live: Vec::new(),
        adoption: Vec::new(),
        child_pid: None,
        mode: crate::native_updater_service::ApplyMode::Automatic,
        apply_attempt: None,
        same_image: Some(crate::app_update_handoff::SameImageHandoff::DebugSeam),
        target_build: 0,
        target_commit: String::new(),
        layout: crate::restore::RestoreManifest::new(Vec::new()),
        layout_digest: [0; 32],
        screen_digest: [0; 32],
        activity_epoch: 0,
        cancel,
        arbiter: crate::HandoffAttemptArbiter::new(),
        teardown: crate::DeferredHandoffTeardown::None,
        commit_drain_started: None,
        revoked_by_activity: false,
    }
}

/// Window 0 of `manifest` with only its editor tab: what the successor's
/// restore does with an editor leaf, without spawning shells for the rest.
fn editor_only_layout(manifest: &crate::restore::RestoreManifest) -> crate::restore::WindowLayout {
    let window = manifest.windows.first().expect("window 0");
    let editor = window
        .restored_tabs
        .iter()
        .find(|tab| {
            matches!(
                &tab.root,
                crate::restore::RestoredSplitTree::Leaf {
                    view: crate::restore::RestoredView::Native(native),
                } if native.restore_tag == "editor"
            )
        })
        .expect("the capture holds the editor leaf")
        .clone();
    crate::restore::WindowLayout {
        rows: 24,
        cols: 80,
        active_tab: 0,
        outer_x: None,
        outer_y: None,
        maximized: None,
        show: crate::restore::WindowShow::UNKNOWN,
        tabs: Vec::new(),
        native_tabs: Vec::new(),
        tab_order: Vec::new(),
        active_item: Some(0),
        restored_tabs: vec![editor],
    }
}

/// THE ROUND TRIP: a draft typed, journaled and verified rides the update. The
/// successor's editor holds exactly the outgoing text, dirty, with the caret
/// where the person left it and a status that says the draft was carried; the
/// file on disk is untouched.
#[test]
fn a_journaled_draft_is_carried_and_restored_exactly() {
    let mut rig = Rig::new("round-trip");
    rig.type_key("first key", "a", &["Type", "Plan", "Land"]);
    rig.type_key("second key", "b", &["Type", "Plan", "Land"]);
    rig.parent
        .dispatch_native_event(wid(), AppEvent::EditorSetSelection { anchor: 1, head: 4 })
        .unwrap();
    assert!(rig.start("start"), "a durable, verified draft is carried");
    rig.successor_restores("restore", "Restore");
    let (text, dirty, selections) = rig.successor_editor();
    assert_eq!(text, rig.texts[2], "the successor holds the draft exactly");
    assert!(dirty, "and still unsaved");
    assert_eq!(
        selections,
        vec![crate::native_editor::Selection { anchor: 1, head: 4 }],
        "with the selection the person left"
    );
    let successor = rig.successor.as_ref().unwrap();
    let (instance, _) = successor.active_native_view(wid()).unwrap();
    let document = successor.native_runtime.document_id(instance).unwrap();
    assert_eq!(
        successor.document_recovery_status_for_test(document),
        Some(crate::app_documents::CARRIED_DRAFT_NOTICE),
        "the successor says the draft was carried, not recovered from a crash"
    );
    assert!(rig.commit("commit"), "the successor's copy is verified");
    rig.assert_file_untouched();
}

/// Typing into the outgoing editor during the overlap holds the Commit — the
/// successor restored the older text — and the rollback gives the journal
/// back, so the next attempt carries the newer draft.
#[test]
fn typing_during_the_overlap_holds_the_commit_and_the_rollback_reseats() {
    let mut rig = Rig::new("overlap-typing");
    rig.type_key("draft", "a", &["Type", "Plan", "Land"]);
    assert!(rig.start("start"));
    rig.successor_restores("restore", "Restore");
    rig.type_key("typed during the overlap", "b", &["Type", "Plan", "Refuse"]);
    let held = rig.project();
    assert!(
        !rig.commit("commit refused"),
        "the successor holds the older text"
    );
    let blind = interp::with_buggy(&rig.model, 1);
    assert!(
        blind.action_enabled("Commit", &held),
        "NEGATIVE CONTROL: the blind Commit (Buggy = 1) would have committed here"
    );
    rig.rollback("rollback", &["Rollback", "Reseat"]);
    let reseated = rig.project();
    assert_eq!(
        (reseated["owner"], reseated["journal"], reseated["mem"]),
        (0, 2, 2),
        "the journal is the outgoing store's own again and holds the newer draft"
    );
    rig.type_key("the journal writes again", "c", &["Type", "Plan", "Land"]);
    assert!(rig.start("second start"));
    rig.successor_restores("second restore", "Restore");
    assert!(rig.commit("second commit"));
    assert_eq!(rig.successor_editor().0, rig.texts[3]);
    rig.assert_file_untouched();
}

/// A rollback that forgets the successor's image leaves the outgoing journal
/// refusing every append: with the re-seat, the next keystroke is durable.
#[test]
fn a_rollback_without_its_reseat_would_wedge_the_journal() {
    let mut rig = Rig::new("forgotten-image");
    rig.type_key("draft", "a", &["Type", "Plan", "Land"]);
    assert!(rig.start("start"));
    rig.successor_restores("restore", "Restore");
    let restored = rig.project();
    let forgetful = interp::with_buggy(&rig.model, 1);
    let mut forgotten = restored.clone();
    assert!(forgetful.fire("Rollback", &mut forgotten));
    assert!(
        !forgetful.check_invariant("JournalOwnedOutsideHandoff", &forgotten),
        "NEGATIVE CONTROL: the forgetful rollback breaks the invariant here"
    );
    rig.rollback("rollback", &["Rollback", "Reseat"]);
    rig.type_key("next key", "b", &["Type", "Plan", "Land"]);
    assert_eq!(
        rig.parent
            .document_journal_durable_seq_for_test(rig.document),
        Some(rig.seqs[2]),
        "the keystroke after the rollback is durable"
    );
}

/// THE RACE THE MODEL FOUND: an append already on the worker when the update
/// parks lands on the successor's byte-identical republication AFTER the
/// successor read the older image. The image then replays to the outgoing
/// text, but the successor does not hold it — the `late` fact refuses the
/// Commit.
#[test]
fn an_append_landing_after_the_park_holds_the_commit() {
    let mut rig = Rig::new("late-landing");
    rig.parent.hold_journal_appends_for_test();
    assert!(rig.start("clean start"));
    rig.type_key("typed at the park", "a", &["Type", "Plan"]);
    rig.successor_restores("identical restore", "RestoreIdentical");
    rig.run_held("the held append lands", &["Land"]);
    let landed = rig.project();
    assert_eq!(
        (
            landed["journal"],
            landed["mem"],
            landed["head"],
            landed["succ"]
        ),
        (1, 1, 1, 0),
        "the image replays to the outgoing text, which the successor does not hold"
    );
    assert!(!rig.commit("commit refused"), "nothing orders the landing");
    let unordered = interp::with_buggy(&rig.model, 3);
    assert!(
        unordered.action_enabled("Commit", &landed),
        "NEGATIVE CONTROL: without the late check (Buggy = 3) this would commit"
    );
    let mut lost = landed.clone();
    assert!(unordered.fire("Commit", &mut lost));
    assert!(!unordered.check_invariant("NoDraftLost", &lost));
    rig.rollback("rollback", &["Rollback", "Reseat"]);
}

/// A successor whose restore fails leaves a Recovery tab — and the journal it
/// would reopen holds the draft, so the Commit may proceed and nothing is lost.
/// Its Retry replays the draft once the journal is free.
#[test]
fn a_failed_successor_restore_leaves_the_draft_in_the_journal() {
    let mut rig = Rig::new("failed-restore");
    rig.type_key("draft", "a", &["Type", "Plan", "Land"]);
    assert!(rig.start("start"));
    let path = rig
        .parent
        .document_journal_path_for_test(rig.document)
        .unwrap();
    let lock = crate::native_document_journal::hold_journal_lock_for_test(&path);
    rig.successor_restores("restore refused busy", "RestoreFails");
    // Released by LOCK_UN, not by the close alone: a child another test is
    // forking holds this descriptor until it execs, and the successor's open
    // below has the event loop's 25 ms journal-lock budget (the fd-copy sweep of 2026-09-27).
    lock.unlock().expect("release the journal lock");
    drop(lock);
    assert!(
        rig.commit("commit"),
        "the Recovery tab's journal holds the draft"
    );
    let successor = rig.successor.as_mut().unwrap();
    successor
        .open_document_tab(AppKind::Editor, &rig.uri)
        .unwrap();
    let (instance, _) = successor.active_native_view(wid()).unwrap();
    let document = successor.native_runtime.document_id(instance).unwrap();
    assert_eq!(
        &*successor.document_store.snapshot(document).unwrap().text,
        rig.texts[1],
        "reopening the file replays the carried draft"
    );
    rig.assert_file_untouched();
}

/// A CRASH BETWEEN JOURNAL AND COMMIT loses nothing either way: the outgoing
/// process dying after the successor restored leaves the draft in the
/// successor; the successor dying leaves it in the outgoing process — and in
/// the journal, for whichever process opens the file next.
#[test]
fn a_crash_between_the_journal_and_the_commit_keeps_the_draft() {
    let mut rig = Rig::new("crash");
    rig.type_key("draft", "a", &["Type", "Plan", "Land"]);
    assert!(rig.start("start"));
    rig.successor_restores("restore", "Restore");
    // The outgoing process dies here: the successor already holds the draft.
    assert_eq!(rig.successor_editor().0, rig.texts[1]);
    // …and the successor dies instead: the outgoing process still holds it,
    // and a process opening the file next replays it from the journal.
    rig.successor = None;
    assert_eq!(
        &*rig
            .parent
            .document_store
            .snapshot(rig.document)
            .unwrap()
            .text,
        rig.texts[1]
    );
    let mut next = App::headless_for_test();
    next.use_document_journal_root_for_test(rig.journals.clone());
    next.open_document_tab(AppKind::Editor, &rig.uri).unwrap();
    let (instance, _) = next.active_native_view(wid()).unwrap();
    let document = next.native_runtime.document_id(instance).unwrap();
    assert_eq!(
        &*next.document_store.snapshot(document).unwrap().text,
        rig.texts[1],
        "the journal replays the draft to the next process"
    );
    assert_eq!(next.document_store.dirty(document), Some(true));
    rig.assert_file_untouched();
}

/// A DIGEST MISMATCH holds the update and says so: an image that no longer
/// replays to the editor's text (here, the journal of another text written
/// over it), or one that does not decode at all, is never carried; the blocker
/// is the one a person clears by saving.
#[test]
fn a_journal_that_does_not_verify_holds_the_update_with_a_reason() {
    let mut rig = Rig::new("digest");
    rig.type_key("draft", "a", &["Type", "Plan", "Land"]);
    let path = rig
        .parent
        .document_journal_path_for_test(rig.document)
        .unwrap();
    let good = std::fs::read(&path).unwrap();

    // One flipped byte in the draft payload: a record checksum fails.
    let mut flipped = good.clone();
    let last = flipped.len() - 12;
    flipped[last] ^= 0x20;
    std::fs::write(&path, &flipped).unwrap();
    let DraftCarry::Refused(why) = rig.parent.draft_carry(rig.document) else {
        panic!("a corrupt journal is never carried");
    };
    assert!(why.contains("does not replay"), "{why}");
    let Err(reasons) = rig.parent.revalidate_native_update_safety() else {
        panic!("the preflight holds the update");
    };
    assert!(
        reasons
            .iter()
            .any(|reason| reason.starts_with("Checkpoint Drafts:") && reason.contains(&why)),
        "{reasons:?}"
    );
    assert_eq!(
        App::update_blocker_for_person(&reasons),
        Some(App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY),
        "and the row tells the person what clears it"
    );

    // A well-formed image of a DIFFERENT text: it decodes, and still is not
    // this editor's buffer.
    let other = crate::document_store::DocumentSnapshot {
        id: rig.document,
        seq: aterm_buffer::Seq(1),
        text: std::sync::Arc::from("other\n"),
    };
    let key = rig.key();
    let foreign = crate::native_document_io::encode_journal(&[
        crate::native_document_io::JournalRecord::snapshot_for(key, &other),
    ])
    .unwrap();
    std::fs::write(&path, foreign).unwrap();
    let DraftCarry::Refused(why) = rig.parent.draft_carry(rig.document) else {
        panic!("another text's journal is never carried");
    };
    assert!(why.contains("current text"), "{why}");

    // The image restored: carried again.
    std::fs::write(&path, &good).unwrap();
    assert!(matches!(
        rig.parent.draft_carry(rig.document),
        DraftCarry::Carried { .. }
    ));
    rig.assert_file_untouched();
}

/// THE FILE CHANGED UNDER THE UPDATE: the successor finds contents the draft
/// was not made against, so it preserves the journal aside and opens the file
/// as it now is. The Commit check reads the successor's republication — which
/// no longer replays to the outgoing editor's text — and refuses, so the
/// outgoing process keeps the draft in its editor.
#[test]
fn a_file_changed_before_the_successor_read_it_holds_the_commit() {
    let mut rig = Rig::new("disk-changed");
    rig.type_key("draft", "a", &["Type", "Plan", "Land"]);
    assert!(rig.start("start"));
    std::fs::write(&rig.file, "changed by another program\n").unwrap();
    let layout = editor_only_layout(&rig.parent.capture_restore_manifest());
    let mut successor = App::headless_for_test();
    successor.use_document_journal_root_for_test(rig.journals.clone());
    successor.incoming_handoff_pending = true;
    successor.restore_into_window(wid(), layout);
    let (instance, _) = successor.active_native_view(wid()).unwrap();
    let document = successor.native_runtime.document_id(instance).unwrap();
    assert_eq!(
        &*successor.document_store.snapshot(document).unwrap().text,
        "changed by another program\n",
        "the successor opened the file as it now is"
    );
    let Err(reasons) = rig.parent.revalidate_native_update_safety() else {
        panic!("the Commit must not hand the draft to a successor that does not hold it");
    };
    assert!(
        reasons
            .iter()
            .any(|reason| reason.starts_with("Checkpoint Drafts:")),
        "{reasons:?}"
    );
    assert_eq!(
        &*rig
            .parent
            .document_store
            .snapshot(rig.document)
            .unwrap()
            .text,
        rig.texts[1],
        "the outgoing editor still holds the draft"
    );
    let preserved = std::fs::read_dir(&rig.journals)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().contains(".preserved-"))
        .count();
    assert_eq!(
        preserved, 1,
        "and the successor set the draft's journal aside, not over"
    );
}

const PRELAUNCHED: u64 = 7;

fn prelaunch_record() -> crate::HandoffPrelaunch {
    crate::HandoffPrelaunch {
        stand_down_ack: std::sync::mpsc::sync_channel(1).0,
        attempt_id: PRELAUNCHED,
        nonce: "0123456789abcdef0123456789abcdef".to_string(),
        mode: crate::native_updater_service::ApplyMode::Automatic,
        apply_attempt: None,
        same_image: Some(crate::app_update_handoff::SameImageHandoff::DebugSeam),
        target_build: crate::build_info::BUILD_NUMBER.parse().unwrap_or(0),
        target_commit: crate::build_info::GIT_COMMIT.to_string(),
        cancel: std::sync::mpsc::sync_channel(1).0,
        stand_down: std::sync::mpsc::sync_channel(1).0,
        transfer: std::sync::mpsc::sync_channel(1).0,
        arbiter: crate::HandoffAttemptArbiter::new(),
        launched_at: std::time::Instant::now(),
        dialled: None,
        park_retry_at: None,
        park_misses: 0,
        freeze_seed: crate::app_update_handoff::FreezeSeed::Default,
        land_waits: 0,
        last_wait: None,
        stood_down: false,
        teardown: crate::DeferredHandoffTeardown::None,
        revoked_by_activity: false,
        history_export: None,
    }
}

/// The worker's completion for a candidate it killed and reaped.
fn rejected(attempt_id: u64) -> crate::UpdateHandoffCompletion {
    crate::UpdateHandoffCompletion {
        attempt_id,
        nonce: None,
        child_pid: None,
        outcome: crate::UpdateHandoffOutcome::ActivityRevoked,
        commit_fd: None,
        reject: None,
        reconcile: None,
        detail: "rejected before Commit".to_string(),
        input_drain_spins: 0,
        child_death: crate::ChildDeathEvidence::Unobserved,
    }
}

/// THE LAUNCHED LANE (macOS): the successor is launched and held before the
/// park, and the outgoing editor journals normally meanwhile. Stood down after
/// the park, the readers come back while the attempt record still stands, so
/// the re-seat is FENCED — and nothing is appended over the successor's image
/// until it runs — and the completion that retires the record runs it. Stood
/// down before the park, nothing is owed.
#[test]
#[aterm_spec::spec_unmodeled(
    machine = "NativeUpdateEditorCarry",
    action = "Park",
    reason = "The handoff's own step: the park installs the attempt record, which this machine \
              reads only through the fences and the late fact. Tier-1 installs the real record \
              (`PendingUpdateHandoff`) the park installs; the park itself is \
              `NativeUpdateSeamlessHandoffOwnership`'s `ParkOutgoingReaders`."
)]
#[aterm_spec::spec_unmodeled(
    machine = "NativeUpdateEditorCarry",
    action = "StandDown",
    reason = "The handoff's own step: a launched successor stood down before the park leaves \
              nothing restored and nothing owed. Tier-1 retires a real prelaunch record through \
              the real completion reducer (`reduce_returned_handoff_completion`)."
)]
fn the_launched_lane_journals_through_the_hold_and_reseats_after_its_stand_down() {
    let mut rig = Rig::new("launched-lane");
    rig.type_key("draft", "a", &["Type", "Plan", "Land"]);
    let before = rig.project();
    assert!(rig.parent.revalidate_native_update_safety().is_ok());
    assert!(rig.model.action_enabled("Start", &before));
    rig.step("launched", &["Start"], |rig| {
        rig.parent.update_handoff_prelaunch = Some(prelaunch_record());
        rig.phase = 1;
    });
    rig.type_key("typed during the hold", "b", &["Type", "Plan", "Land"]);
    rig.step("parked", &["Park"], |rig| {
        rig.parent.pending_update_handoff = Some(parked_record(PRELAUNCHED));
        rig.phase = 2;
    });
    rig.successor_restores("restore", "Restore");
    rig.step("stood down after the park", &["Rollback"], |rig| {
        rig.successor = None;
        rig.parent.answer_prelaunched_stand_down(PRELAUNCHED);
        rig.phase = 0;
        rig.restored = 0;
        rig.succ = 0;
    });
    assert!(
        rig.parent.update_handoff_in_flight(),
        "PRECONDITION: the attempt record still stands, so the re-seat is fenced"
    );
    rig.type_key("typed while the re-seat is fenced", "c", &["Type"]);
    rig.step("the attempt retires", &["Reseat"], |rig| {
        assert!(
            rig.parent
                .reduce_returned_handoff_completion(rejected(PRELAUNCHED))
                .is_some()
        );
    });
    assert_eq!(
        rig.parent
            .document_journal_durable_seq_for_test(rig.document),
        Some(rig.seqs[3]),
        "the re-seat wrote the edit typed while it was fenced"
    );

    rig.step("launched again", &["Start"], |rig| {
        rig.parent.update_handoff_prelaunch = Some(prelaunch_record());
        rig.phase = 1;
    });
    rig.step("stood down before the park", &["StandDown"], |rig| {
        assert!(
            rig.parent
                .reduce_returned_handoff_completion(rejected(PRELAUNCHED))
                .is_some()
        );
        rig.phase = 0;
    });
    rig.assert_file_untouched();
}

/// A draft whose latest edit is still on the worker is waited out without a
/// word to the person (`Wait:`), never carried and never a Save row.
#[test]
fn a_draft_still_being_journaled_is_waited_out() {
    let mut rig = Rig::new("writing");
    rig.parent.hold_journal_appends_for_test();
    rig.type_key("draft", "a", &["Type", "Plan"]);
    assert_eq!(
        rig.parent.draft_carry(rig.document),
        DraftCarry::Writing,
        "the append is on the worker"
    );
    assert!(!rig.start("start refused"));
    let Err(reasons) = rig.parent.revalidate_native_update_safety() else {
        panic!("held");
    };
    assert!(
        reasons.iter().all(|reason| reason.starts_with("Wait:")),
        "{reasons:?}"
    );
    assert!(
        reasons
            .iter()
            .any(|reason| reason == "Wait: a draft journal write is in flight"),
        "no update starts over a write that could land after the successor reads: {reasons:?}"
    );
    assert_eq!(App::update_blocker_for_person(&reasons), None);
    rig.run_held("the append lands", &["Land"]);
    assert!(rig.start("start"));
}

/// A JOURNAL THAT KEEPS REFUSING IS SAID, NOT WAITED ON. The update retries a
/// failed append itself (`App::drive_owed_document_journals`) the moment before
/// it asks, so with a document worker the retry is always in flight when the
/// preflight reads the draft. A failure that recurs — another writer's image
/// on disk, a full disk — must still reach the person as the row that asks
/// them to save, as every unsaved draft did before the carry, and not read as
/// "still being written" at every probe while the update waits in silence for
/// good. The held-append seam is the worker here; the probe is the real
/// update entry (`apply_debug_seamless_update`: the owed-journal drive, the
/// shutdown barrier, the preflight).
#[test]
fn a_journal_that_keeps_refusing_holds_the_update_out_loud() {
    // A blocked attempt writes the shared apply ledger (`record_apply_outcome_in_ledger`).
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut rig = Rig::new("refusing-journal");
    rig.parent.hold_journal_appends_for_test();
    rig.parent
        .dispatch_native_event(
            wid(),
            AppEvent::TextInput(TextInputEvent::Commit("a".to_string())),
        )
        .unwrap();
    let path = rig
        .parent
        .document_journal_path_for_test(rig.document)
        .unwrap();
    let ours = std::fs::read(&path).unwrap();
    // Another writer republishes the journal: every append this store plans
    // against its own image fails from now on.
    let foreign = crate::native_document_io::encode_journal(&[
        crate::native_document_io::JournalRecord::snapshot_for(
            rig.key(),
            &crate::document_store::DocumentSnapshot {
                id: rig.document,
                seq: aterm_buffer::Seq(1),
                text: std::sync::Arc::from("another writer\n"),
            },
        ),
    ])
    .unwrap();
    std::fs::write(&path, &foreign).unwrap();
    for probe in 0..3 {
        assert!(
            rig.parent.run_held_journal_append_for_test(),
            "probe {probe}: an append was on the worker"
        );
        assert!(
            rig.parent.held_journal_appends_for_test().is_empty(),
            "probe {probe}: and it failed, with nothing behind it"
        );
        let crate::native_app::UpdateOutcome::Blocked { reasons } =
            rig.parent.apply_debug_seamless_update()
        else {
            panic!("probe {probe}: a draft its journal refuses is never carried");
        };
        assert_eq!(
            App::update_blocker_for_person(&reasons),
            Some(App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY),
            "probe {probe}: a journal that keeps refusing must be said, not waited on: \
             {reasons:?}"
        );
        assert!(
            reasons
                .iter()
                .any(|reason| reason.starts_with("Checkpoint Drafts:")
                    && reason.contains("could not be written")),
            "probe {probe}: {reasons:?}"
        );
    }
    // The update's own retry is on the worker. Once the image is this store's
    // again it lands, and the draft is carried.
    std::fs::write(&path, &ours).unwrap();
    assert!(rig.parent.run_held_journal_append_for_test());
    let token = rig
        .parent
        .revalidate_native_update_safety()
        .expect("the retry landed: carried");
    assert_eq!(token.carried_drafts(), 1);
    rig.assert_file_untouched();
}

/// A SAVED DOCUMENT'S FAILING JOURNAL DOES NOT HOLD THE UPDATE IN SILENCE. After
/// a rollback every journal owes a re-seat. When a clean document's keeps
/// failing (here its image is unreadable), the update has nothing of it to
/// carry and nothing to say to the person — but its own retry of that re-seat,
/// driven the moment before the preflight, was a journal write in flight at
/// every probe ("Wait: a draft journal write is in flight"), so the update
/// waited without a word for as long as the failure lasted. Only an unsaved
/// draft's failed write is retried by the update; the clean one is retried at
/// its document's next drive and after any rollback. The held-re-seat seam is
/// the worker here.
#[test]
fn a_clean_documents_failing_reseat_does_not_hold_the_update_in_silence() {
    use std::os::unix::fs::PermissionsExt as _;

    let mut rig = Rig::new("clean-reseat-fails");
    rig.parent.hold_journal_reseats_for_test();
    rig.parent.pending_update_handoff = Some(parked_record(FORKED));
    assert!(
        rig.parent
            .reduce_returned_handoff_completion(rejected(FORKED))
            .is_some()
    );
    assert_eq!(
        rig.parent.held_journal_reseats_for_test().len(),
        1,
        "the rollback owes the clean document's journal a re-seat"
    );
    let path = rig
        .parent
        .document_journal_path_for_test(rig.document)
        .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    let mut verdicts = Vec::new();
    for probe in 0..3 {
        if !rig.parent.held_journal_reseats_for_test().is_empty() {
            assert!(rig.parent.run_held_journal_reseat_for_test());
            assert!(
                rig.parent
                    .document_journal_reseat_owed_for_test(rig.document),
                "probe {probe}: the re-seat failed and is owed again"
            );
        }
        // The automatic lane's order: drive the owed journals, then ask.
        rig.parent.drive_owed_document_journals();
        verdicts.push(rig.parent.revalidate_native_update_safety().map(|_| ()));
    }
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(
        verdicts.iter().all(Result::is_ok),
        "a clean document carries nothing, so its failing journal must not hold the update \
         with no word to anyone: {verdicts:?}"
    );
    assert!(
        rig.parent.held_journal_reseats_for_test().is_empty(),
        "and the update did not put its failing re-seat back on the worker"
    );
    assert_eq!(rig.parent.document_store.dirty(rig.document), Some(false));
    rig.assert_file_untouched();
}

/// A HEADLESS process's successor restores no document tab (`main_entry`
/// takes the handoff layout only with a window), so a headless process never
/// carries a draft: the update waits, and says why, exactly as it did for
/// every draft before.
#[test]
fn a_headless_process_never_carries_a_draft() {
    let mut rig = Rig::new("headless");
    rig.type_key("draft", "a", &["Type", "Plan", "Land"]);
    assert!(matches!(
        rig.parent.draft_carry(rig.document),
        DraftCarry::Carried { .. }
    ));
    let mut headless = App::headless_for_test();
    headless.use_document_journal_root_for_test(rig.dir.join("headless-drafts"));
    headless
        .open_document_tab(AppKind::Editor, &rig.uri)
        .unwrap();
    headless
        .dispatch_native_event(
            wid(),
            AppEvent::TextInput(TextInputEvent::Commit("a".to_string())),
        )
        .unwrap();
    let (instance, _) = headless.active_native_view(wid()).unwrap();
    let document = headless.native_runtime.document_id(instance).unwrap();
    let DraftCarry::Refused(why) = headless.draft_carry(document) else {
        panic!("a headless process carries no draft");
    };
    assert!(why.contains("headless"), "{why}");
    let Err(reasons) = headless.revalidate_native_update_safety() else {
        panic!("the update waits");
    };
    assert_eq!(
        App::update_blocker_for_person(&reasons),
        Some(App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY)
    );
}

/// THE CAP: an update carries at most `CARRIED_DRAFTS_CAP_BYTES` of unsaved
/// text, summed over every draft. At the cap everything rides; one byte over
/// and nothing does, each draft held with the reason.
#[test]
fn the_carry_is_capped_and_says_why_past_it() {
    let at_cap = DraftCarryTally::of(
        [
            DraftCarry::Carried {
                bytes: CARRIED_DRAFTS_CAP_BYTES - 1,
            },
            DraftCarry::Carried { bytes: 1 },
        ],
        CARRIED_DRAFTS_CAP_BYTES,
    );
    assert_eq!(
        (at_cap.carried, at_cap.carried_bytes, at_cap.refused.len()),
        (2, CARRIED_DRAFTS_CAP_BYTES, 0)
    );
    let over = DraftCarryTally::of(
        [
            DraftCarry::Carried {
                bytes: CARRIED_DRAFTS_CAP_BYTES,
            },
            DraftCarry::Carried { bytes: 1 },
            DraftCarry::Writing,
        ],
        CARRIED_DRAFTS_CAP_BYTES,
    );
    assert_eq!((over.carried, over.carried_bytes, over.writing), (0, 0, 1));
    assert_eq!(over.refused.len(), 2, "every draft is held");
    assert!(
        over.refused[0].contains("more than the 64 MiB an update carries"),
        "{:?}",
        over.refused
    );
}

/// A token that carries a draft is refused by the cold lane — no layout is
/// handed over, so the editor tab would close — and the refusal is the one a
/// person clears by saving.
#[test]
fn a_carried_draft_never_rides_the_cold_lane() {
    let mut rig = Rig::new("cold-lane");
    rig.type_key("draft", "a", &["Type", "Plan", "Land"]);
    let token: NativeUpdateSafetyToken = rig
        .parent
        .revalidate_native_update_safety()
        .expect("carried");
    assert_eq!(token.carried_drafts(), 1);
    let refusal =
        crate::app_update_handoff::cold_lane_carried_drafts_refusal(token.carried_drafts());
    assert!(
        refusal.starts_with("Checkpoint Drafts: 1 document(s)"),
        "{refusal}"
    );
    assert_eq!(
        App::update_blocker_for_person(&[refusal]),
        Some(App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY)
    );
}
