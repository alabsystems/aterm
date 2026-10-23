// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance and negative-control tests for `NativeUpdateSpeculativeSidecar`.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::PathBuf;

use aterm_models_update::native_update_speculative_sidecar_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};
use aterm_spec::xref::StepEvidence;

use super::{LayoutSeal, Mismatch, SpeculativeLayout, seal_layout, speculate_layout};
use crate::restore::RestoreManifest;

const PROJECTED: [&str; 5] = ["spec", "moved", "disk", "verified", "read"];

const ANCHORED: [&str; 5] = [
    "Speculate",
    "KeepVerified",
    "DiscardAndRewrite",
    "WriteAtPark",
    "Retire",
];

struct Rig {
    dir: aterm_tempfile::TempDir,
    nonce: String,
    path: PathBuf,
    at_launch: RestoreManifest,
    moved_to: RestoreManifest,
    moved: bool,
    refuse: bool,
    speculative: Option<SpeculativeLayout>,
    held: Option<std::fs::File>,
    verified: bool,
    read: i64,
}

fn wire(layout: &RestoreManifest) -> String {
    layout.to_toml().expect("the fixture layout serializes")
}

impl Rig {
    fn new() -> Self {
        let dir = aterm_tempfile::Builder::new()
            .prefix("aterm-diet")
            .tempdir()
            .expect("scratch dir");
        let nonce = "0123456789abcdef0123456789abcdef".to_string();
        let path = dir.path().join(format!(
            "seamless-{}-{nonce}.layout.toml",
            std::process::id()
        ));
        let at_launch = crate::crash_journal::fixtures::layout(&[("/tmp", "sh")]);
        let moved_to = crate::crash_journal::fixtures::layout(&[("/tmp", "sh"), ("/var", "sh")]);
        for layout in [&at_launch, &moved_to] {
            assert_eq!(
                RestoreManifest::from_toml(&wire(layout)).as_ref(),
                Some(layout),
                "the fixture layouts survive their own round trip, as a park's must"
            );
        }
        assert_ne!(wire(&at_launch), wire(&moved_to));
        Self {
            dir,
            nonce,
            path,
            at_launch,
            moved_to,
            moved: false,
            refuse: false,
            speculative: None,
            held: None,
            verified: false,
            read: 0,
        }
    }

    fn park_layout(&self) -> &RestoreManifest {
        if self.moved {
            &self.moved_to
        } else {
            &self.at_launch
        }
    }

    fn disk(&self) -> i64 {
        let Ok(meta) = std::fs::symlink_metadata(&self.path) else {
            return 0;
        };
        let bytes = std::fs::read(&self.path).unwrap_or_default();
        let speculative_file = self
            .held
            .as_ref()
            .and_then(|file| file.metadata().ok())
            .is_some_and(|held| held.dev() == meta.dev() && held.ino() == meta.ino());
        if speculative_file && bytes == wire(&self.at_launch).as_bytes() {
            1
        } else if !speculative_file && bytes == wire(self.park_layout()).as_bytes() {
            2
        } else {
            3
        }
    }

    fn refuse_writes(&self, refuse: bool) {
        let mode = if refuse { 0o500 } else { 0o700 };
        std::fs::set_permissions(self.dir.path(), std::fs::Permissions::from_mode(mode))
            .expect("chmod the scratch dir");
    }
}

fn project(rig: &Rig, model_state: &State) -> State {
    let mut state = model_state.clone();
    state.insert("spec", i64::from(rig.held.is_some()));
    state.insert("moved", i64::from(rig.moved));
    state.insert("disk", rig.disk());
    state.insert("verified", i64::from(rig.verified));
    state.insert("read", rig.read);
    state
}

fn seal_action(seal: LayoutSeal) -> &'static str {
    match seal {
        LayoutSeal::Kept { .. } => "KeepVerified",
        LayoutSeal::Rewritten(_) => "DiscardAndRewrite",
        LayoutSeal::WrittenAtPark => "WriteAtPark",
    }
}

fn drive(rig: &mut Rig, action: &'static str, ev: &mut StepEvidence) {
    match action {
        "RefuseWrite" => rig.refuse = true,
        "Speculate" => {
            if rig.refuse {
                rig.refuse_writes(true);
            }
            let (path, layout) = (rig.path.clone(), rig.at_launch.clone());
            let written = ev.step(action, || speculate_layout(&path, &layout));
            if rig.refuse {
                rig.refuse_writes(false);
            }
            match written {
                Ok(speculative) => {
                    assert!(!rig.refuse, "a refused write must not speculate");
                    rig.held = Some(std::fs::File::open(&rig.path).expect("the speculative file"));
                    rig.speculative = Some(speculative);
                }
                Err(why) => assert!(rig.refuse, "an unrefused speculation failed: {why}"),
            }
        }
        "LayoutMoves" => rig.moved = true,
        "DiskDisturbed" => {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&rig.path)
                .expect("the speculative file");
            file.write_all(b"schema = 0\n")
                .expect("overwrite the speculative file");
        }
        "Park" | "Grant" => {}
        "KeepVerified" | "DiscardAndRewrite" | "WriteAtPark" => {
            let (path, park_wire) = (rig.path.clone(), wire(rig.park_layout()));
            let speculative = rig.speculative.take();
            let seal = ev
                .step(action, || seal_layout(&path, &park_wire, speculative))
                .expect("the seal writes the sidecar");
            assert_eq!(
                seal_action(seal),
                action,
                "the shipping seal chose {seal:?}"
            );
            if let LayoutSeal::Rewritten(why) = seal {
                let expected = if rig.moved {
                    Mismatch::LayoutMoved
                } else {
                    Mismatch::FileDisturbed
                };
                assert_eq!(why, expected);
            }
            rig.verified = matches!(seal, LayoutSeal::Kept { .. });
        }
        "SuccessorReads" => rig.read = successor_reads(rig),
        "Retire" => {
            let (dir, nonce) = (rig.dir.path().to_path_buf(), rig.nonce.clone());
            ev.step(action, || {
                crate::seamless::discard_outgoing_in(&dir, &nonce)
            });
        }
        other => panic!("no real step for `{other}`"),
    }
}

fn successor_reads(rig: &Rig) -> i64 {
    let digest = crate::seamless::take_regular_capped(
        &rig.path,
        rig.dir.path(),
        crate::seamless::MAX_HANDOFF_LAYOUT_BYTES,
    )
    .and_then(|bytes| String::from_utf8(bytes).ok())
    .and_then(|wire| crate::seamless::layout_wire_digest(&wire));
    let committed = crate::seamless::layout_digest(rig.park_layout());
    if digest.is_some() && digest == committed {
        1
    } else {
        2
    }
}

fn behaviours(model: &Model) -> Vec<Vec<&'static str>> {
    fn walk(
        model: &Model,
        state: &State,
        path: &mut Vec<&'static str>,
        out: &mut Vec<Vec<&'static str>>,
    ) {
        let mut extended = false;
        for action in model.actions.iter().map(|a| a.name) {
            for next in model.successors(action, state) {
                extended = true;
                path.push(action);
                walk(model, &next, path, out);
                path.pop();
            }
        }
        if !extended {
            out.push(path.clone());
        }
    }
    let mut out = Vec::new();
    walk(model, &model.init_state(), &mut Vec::new(), &mut out);
    out
}

fn replay(
    model: &Model,
    schedule: &[&'static str],
    ev: &mut StepEvidence,
    stand_in: &dyn Fn(&mut Rig, &str) -> bool,
) -> (Rig, State) {
    let mut rig = Rig::new();
    let mut state = model.init_state();
    for &action in schedule {
        assert!(
            model.fire(action, &mut state),
            "{action} is enabled after {schedule:?}: {state:?}"
        );
        if !stand_in(&mut rig, action) {
            drive(&mut rig, action, ev);
        }
        let real = project(&rig, &state);
        for var in PROJECTED {
            assert_eq!(
                real[var], state[var],
                "`{var}` after {action} in {schedule:?}: real {real:?}, model {state:?}"
            );
        }
    }
    (rig, state)
}

#[test]
fn the_shipping_writer_seal_reader_and_sweep_conform_to_the_model() {
    let model = interp::with_buggy(&native_update_speculative_sidecar_model(), 0);
    let mut ev = StepEvidence::new("native_update_speculative_sidecar");
    let schedules = behaviours(&model);
    assert!(
        schedules.len() > 20,
        "the walk covers the machine: {} behaviours",
        schedules.len()
    );
    let mut seals: BTreeSet<&str> = BTreeSet::new();
    let mut read_the_parks_bytes = 0;
    for schedule in &schedules {
        let (_rig, state) = replay(&model, schedule, &mut ev, &|_, _| false);
        for invariant in &model.invariants {
            assert!(
                model.check_invariant(invariant.name, &state),
                "{} after {schedule:?}",
                invariant.name
            );
        }
        seals.extend(schedule.iter().copied().filter(|action| {
            ["KeepVerified", "DiscardAndRewrite", "WriteAtPark"].contains(action)
        }));
        read_the_parks_bytes += usize::from(schedule.contains(&"SuccessorReads"));
    }
    assert_eq!(seals.len(), 3, "all three seals are reached: {seals:?}");
    assert!(
        read_the_parks_bytes > 0,
        "the successor reads in some behaviour"
    );

    let audit = ev.audit();
    let actions: BTreeSet<&str> = audit.actions.iter().copied().collect();
    assert_eq!(actions, ANCHORED.into_iter().collect::<BTreeSet<_>>());
    assert!(audit.uninstrumented.is_empty(), "{audit:?}");
    assert!(audit.ambiguous.is_empty(), "{audit:?}");
    assert!(audit.unanchored_steps.is_empty(), "{audit:?}");
    assert!(audit.undriven.is_empty(), "{audit:?}");
    assert!(audit.unwitnessed.is_empty(), "{audit:?}");
    assert!(audit.blind_steps.is_empty(), "{audit:?}");
    assert!(audit.stray.is_empty(), "{audit:?}");
}

fn mutant_stand_in(rig: &mut Rig, action: &str) -> bool {
    match action {
        "SealUnverified" => {
            rig.speculative = None;
            true
        }
        "VerifyTheMemoryCopy" => {
            let speculative = rig.speculative.take().expect("a speculation");
            assert_eq!(speculative.wire, wire(rig.park_layout()));
            rig.verified = true;
            true
        }
        "GrantBeforeSeal" => true,
        "RetireKeepsSpeculation" => true,
        _ => false,
    }
}

#[test]
fn negative_control_each_mutant_breaks_its_invariant_on_the_real_files() {
    let buggy = interp::with_buggy(&native_update_speculative_sidecar_model(), 1);
    let committed = interp::with_buggy(&native_update_speculative_sidecar_model(), 0);
    let mut ev = StepEvidence::new("native_update_speculative_sidecar");
    let cases: [(&[&'static str], &str, &[&'static str]); 5] = [
        (
            &[
                "Speculate",
                "LayoutMoves",
                "Park",
                "GrantBeforeSeal",
                "SuccessorReads",
            ],
            "SuccessorReadsTheParksBytes",
            &[
                "Speculate",
                "LayoutMoves",
                "Park",
                "DiscardAndRewrite",
                "Grant",
                "SuccessorReads",
            ],
        ),
        (
            &["Speculate", "LayoutMoves", "Park", "SealUnverified"],
            "SealHoldsTheParksBytes",
            &["Speculate", "LayoutMoves", "Park", "DiscardAndRewrite"],
        ),
        (
            &["Speculate", "Park", "SealUnverified"],
            "SpeculationVerifiedBeforeSeal",
            &["Speculate", "Park", "KeepVerified"],
        ),
        (
            &["Speculate", "DiskDisturbed", "Park", "VerifyTheMemoryCopy"],
            "SealHoldsTheParksBytes",
            &["Speculate", "DiskDisturbed", "Park", "DiscardAndRewrite"],
        ),
        (
            &["Speculate", "RetireKeepsSpeculation"],
            "NothingSpeculativeOutlivesTheAttempt",
            &["Speculate", "Retire"],
        ),
    ];
    for (mutant, invariant, shipping) in cases {
        let (rig, state) = replay(&buggy, mutant, &mut ev, &mutant_stand_in);
        let real = project(&rig, &state);
        assert!(
            !buggy.check_invariant(invariant, &real),
            "{mutant:?} breaks {invariant} on the real files: {real:?}"
        );
        let (rig, state) = replay(&committed, shipping, &mut ev, &|_, _| false);
        let real = project(&rig, &state);
        assert!(
            committed.check_invariant(invariant, &real),
            "the shipping {shipping:?} keeps {invariant}: {real:?}"
        );
    }
}

#[test]
fn the_seal_keeps_only_the_exact_bytes_at_the_exact_name() {
    let rig = Rig::new();
    let park_wire = wire(&rig.at_launch);
    for tail in [&b"\n"[..], b""] {
        let speculative = speculate_layout(&rig.path, &rig.at_launch).expect("speculate");
        let mut bytes = park_wire.clone().into_bytes();
        if tail.is_empty() {
            bytes.pop();
        } else {
            bytes.extend_from_slice(tail);
        }
        std::fs::write(&rig.path, &bytes).expect("reshape the file");
        let seal = seal_layout(&rig.path, &park_wire, Some(speculative)).expect("seal");
        assert_eq!(seal, LayoutSeal::Rewritten(Mismatch::FileDisturbed));
        assert_eq!(
            std::fs::read(&rig.path).expect("sealed"),
            park_wire.as_bytes()
        );
    }
    let elsewhere = rig.dir.path().join("elsewhere.layout.toml");
    let speculative = speculate_layout(&elsewhere, &rig.at_launch).expect("speculate");
    let seal = seal_layout(&rig.path, &park_wire, Some(speculative)).expect("seal");
    assert_eq!(seal, LayoutSeal::Rewritten(Mismatch::OtherName));
    assert!(
        !elsewhere.exists(),
        "a speculation for another name is discarded"
    );
    assert_eq!(
        std::fs::read(&rig.path).expect("sealed"),
        park_wire.as_bytes()
    );
}
