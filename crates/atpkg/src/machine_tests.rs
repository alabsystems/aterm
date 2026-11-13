// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::cell::Cell;

/// The first launch of the day claims the slot; every later one, whichever lane, runs
/// nothing until a day has passed.
#[test]
fn the_launch_apply_is_claimed_once_a_day() {
    let prefix = crate::testdir::dir("atpkg-machine-daily-");
    let layout = crate::store::Layout {
        prefix: prefix.clone(),
    };
    let now = 1_790_000_000_i64;
    let day = i64::try_from(LAUNCH_APPLY_EVERY_SECS).unwrap();
    let due = |at| launch_apply_decision(&layout, at, false);
    assert_eq!(due(now), LaunchApply::Due);
    assert_eq!(due(now + 60), LaunchApply::NotDue);
    assert_eq!(due(now + day - 1), LaunchApply::NotDue);
    assert_eq!(due(now + day), LaunchApply::Due);
    assert!(layout.machine_apply_stamp().is_file());
    let _ = std::fs::remove_dir_all(&prefix);
}

/// Under a private state root a launch runs and claims nothing, however often it asks,
/// so the owner's first launch of the day still claims the slot; only the skip says a
/// line.
#[test]
fn a_private_state_root_skips_the_launch_apply_before_the_claim() {
    let prefix = crate::testdir::dir("atpkg-machine-private-");
    let layout = crate::store::Layout {
        prefix: prefix.clone(),
    };
    let now = 1_790_000_000_i64;
    for at in [now, now + 60, now + 2 * 86_400] {
        assert_eq!(
            launch_apply_decision(&layout, at, true),
            LaunchApply::PrivateStateRoot
        );
    }
    assert!(
        !layout.machine_apply_stamp().exists(),
        "the skip precedes the claim: no stamp"
    );
    assert_eq!(launch_apply_decision(&layout, now, false), LaunchApply::Due);
    assert!(layout.machine_apply_stamp().is_file());
    assert_eq!(
        launch_apply_decision(&layout, now + 1, true),
        LaunchApply::PrivateStateRoot
    );

    let mut said = Vec::new();
    assert!(!LaunchApply::PrivateStateRoot.runs(|line| said.push(line)));
    assert_eq!(said, [PRIVATE_STATE_ROOT_SKIP]);
    assert!(LaunchApply::Due.runs(|line| said.push(line)));
    assert!(!LaunchApply::NotDue.runs(|line| said.push(line)));
    assert_eq!(said.len(), 1, "only the skip says anything");
    let _ = std::fs::remove_dir_all(&prefix);
}

/// A never-applied table reads changed; recording it makes it unchanged (and a second
/// record writes nothing); an edit, even onto a default, reads changed again.
#[test]
fn the_applied_table_is_recorded_and_an_edit_reads_as_changed() {
    let p = std::env::temp_dir().join(format!("atpkg-machine-applied-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    let layout = crate::store::Layout { prefix: p.clone() };
    let default = crate::config::MachineConfig::default();
    record_applied(&layout, &default);
    assert!(!p.exists(), "no prefix: nothing is created");
    std::fs::create_dir_all(&p).unwrap();
    assert!(changed_since_applied(&layout, &default), "never applied");
    record_applied(&layout, &default);
    assert!(!changed_since_applied(&layout, &default));
    let stamp = std::fs::metadata(applied_stamp_path(&layout))
        .unwrap()
        .modified()
        .unwrap();
    record_applied(&layout, &default);
    assert_eq!(
        std::fs::metadata(applied_stamp_path(&layout))
            .unwrap()
            .modified()
            .unwrap(),
        stamp,
        "unchanged: not rewritten"
    );
    forget_applied(&layout);
    assert!(
        changed_since_applied(&layout, &default),
        "forgotten: retried"
    );
    record_applied(&layout, &default);
    let leave = crate::config::parse_machine("[machine]\nuniversal_control = \"leave\"\n");
    assert!(changed_since_applied(&layout, &leave), "an edit");
    let spelled_default = crate::config::parse_machine("[machine]\nuniversal_control = \"off\"\n");
    assert!(
        changed_since_applied(&layout, &spelled_default),
        "spelling out a default is an edit too"
    );
    let no_rosetta = crate::config::parse_machine("[machine]\nrosetta = false\n");
    assert!(changed_since_applied(&layout, &no_rosetta), "rosetta off");
    record_applied(&layout, &no_rosetta);
    assert!(!changed_since_applied(&layout, &no_rosetta));
    let rosetta_on = crate::config::parse_machine("[machine]\nrosetta = true\n");
    assert!(
        changed_since_applied(&layout, &rosetta_on),
        "rosetta back on"
    );
    std::fs::write(
        applied_stamp_path(&layout),
        "atpkg-machine-applied v2\nuniversal_control=None\nunreadable=false\n",
    )
    .unwrap();
    assert!(changed_since_applied(&layout, &default), "a v2 stamp");
    let _ = std::fs::remove_dir_all(&p);
}

/// A fake domain: the two keys, and a count of writes.
struct Fake {
    disable: Cell<Option<bool>>,
    edges: Cell<Option<bool>>,
    writes: Cell<u32>,
    write_ok: bool,
    /// `defaults` itself could not answer — the read that taught us nothing.
    unusable: bool,
}

impl Fake {
    fn at(disable: Option<bool>, edges: Option<bool>) -> Self {
        Fake {
            disable: Cell::new(disable),
            edges: Cell::new(edges),
            writes: Cell::new(0),
            write_ok: true,
            unusable: false,
        }
    }
}

impl UniversalControlPort for Fake {
    fn state(&self) -> [Read<bool>; 2] {
        let read = |v: Option<bool>| {
            if self.unusable {
                Read::Unusable("defaults timed out".into())
            } else {
                v.map_or(Read::Absent, Read::Value)
            }
        };
        [read(self.disable.get()), read(self.edges.get())]
    }
    fn disable(&self) -> bool {
        self.writes.set(self.writes.get() + 1);
        if self.write_ok {
            self.disable.set(Some(true));
            self.edges.set(Some(true));
        }
        self.write_ok
    }
}

#[test]
fn defaults_bool_parses_the_renderings_defaults_prints_and_nothing_else() {
    assert_eq!(parse_defaults_bool(b"1\n"), Some(true));
    assert_eq!(parse_defaults_bool(b"0\n"), Some(false));
    assert_eq!(parse_defaults_bool(b"  true "), Some(true));
    assert_eq!(parse_defaults_bool(b"YES"), Some(true));
    assert_eq!(parse_defaults_bool(b"No\n"), Some(false));
    assert_eq!(parse_defaults_bool(b""), None);
    assert_eq!(parse_defaults_bool(b"\n"), None);
    assert_eq!(parse_defaults_bool(b"{\n    Configuration = x;\n}\n"), None);
    assert_eq!(parse_defaults_bool(b"2"), None);
    assert_eq!(parse_defaults_bool(b"truee"), None);
}

/// One read of the whole domain answers both switches, as two key reads did. NEGATIVE
/// CONTROL: a key only a nested dictionary holds is absent at the top level, a value that
/// is no boolean is unusable, and a missing domain leaves both absent.
#[test]
fn one_domain_read_answers_both_universal_control_switches() {
    const KEYS: [&str; 2] = ["Disable", "DisableMagicEdges"];
    let read = |ok: bool, out: &str| {
        universal_control_from_domain(Some((ok, out.as_bytes().to_vec())), KEYS)
    };
    assert_eq!(
        read(true, "{\n    Disable = 1;\n    DisableMagicEdges = 1;\n}\n"),
        [Read::Value(true), Read::Value(true)]
    );
    assert_eq!(
        read(true, "{\n    Disable = 0;\n}\n"),
        [Read::Value(false), Read::Absent]
    );
    assert_eq!(
        read(
            true,
            "{\n    Other =     {\n        Disable = 1;\n    };\n    DisableMagicEdges = \
             true;\n}\n"
        ),
        [Read::Absent, Read::Value(true)],
        "a nested key is not the domain's"
    );
    assert!(matches!(
        read(true, "{\n    Disable = \"maybe\";\n}\n"),
        [Read::Unusable(_), Read::Absent]
    ));
    assert_eq!(read(false, ""), [Read::Absent, Read::Absent], "no domain");
    assert!(matches!(
        read(false, "something"),
        [Read::Unusable(_), Read::Unusable(_)]
    ));
    assert!(matches!(
        read(true, "{\n    Disable = 1;\n"),
        [Read::Unusable(_), Read::Unusable(_)]
    ));
    assert!(matches!(
        universal_control_from_domain(None, KEYS),
        [Read::Unusable(_), Read::Unusable(_)]
    ));
}

#[test]
fn the_pass_disables_once_and_never_touches_a_disabled_host_or_a_leave_policy() {
    if !cfg!(target_os = "macos") {
        let fake = Fake::at(None, None);
        assert_eq!(
            apply_universal_control(UniversalControlPolicy::Off, &fake),
            UniversalControlOutcome::NotApplicable
        );
        assert_eq!(fake.writes.get(), 0);
        return;
    }
    let fake = Fake::at(None, None);
    assert_eq!(
        apply_universal_control(UniversalControlPolicy::Off, &fake),
        UniversalControlOutcome::Disabled
    );
    assert_eq!(fake.writes.get(), 1);
    assert!(universal_control_state(&fake).disabled());
    assert_eq!(
        apply_universal_control(UniversalControlPolicy::Off, &fake),
        UniversalControlOutcome::AlreadyDisabled
    );
    assert_eq!(fake.writes.get(), 1);
    let half = Fake::at(Some(true), None);
    assert!(!universal_control_state(&half).disabled());
    assert_eq!(
        apply_universal_control(UniversalControlPolicy::Off, &half),
        UniversalControlOutcome::Disabled
    );
    let leave = Fake::at(None, None);
    assert_eq!(
        apply_universal_control(UniversalControlPolicy::Leave, &leave),
        UniversalControlOutcome::NotApplicable
    );
    assert_eq!(leave.writes.get(), 0);
    let mut broken = Fake::at(Some(false), Some(false));
    broken.write_ok = false;
    assert_eq!(
        apply_universal_control(UniversalControlPolicy::Off, &broken),
        UniversalControlOutcome::WriteFailed
    );
    assert_eq!(broken.writes.get(), 1);
}

/// Every posture round-trips through the wire line, so the window and the CLI cannot
/// disagree about a spelling; an unknown key is ignored (a newer atpkg), a missing
/// one is a refusal, never a guess.
#[test]
fn machine_state_line_round_trips() {
    for uc in [
        UcPosture::Disabled,
        UcPosture::Default,
        UcPosture::Partial,
        UcPosture::Unknown,
    ] {
        for policy in [UniversalControlPolicy::Off, UniversalControlPolicy::Leave] {
            for home in [
                HomePosture::Account,
                HomePosture::Mismatch,
                HomePosture::Unresolved,
            ] {
                for rosetta in [
                    RosettaPosture::Present,
                    RosettaPosture::Missing,
                    RosettaPosture::Unknown,
                    RosettaPosture::NotApplicable,
                ] {
                    for rosetta_wanted in [true, false] {
                        let s = MachineState {
                            universal_control: uc,
                            policy,
                            home,
                            config_unreadable: uc == UcPosture::Unknown,
                            rosetta,
                            rosetta_wanted,
                        };
                        let line = machine_state_line(&s);
                        assert_eq!(parse_machine_state(&line), Some(s.clone()), "{line}");
                    }
                }
            }
        }
    }
    let full = "universal-control=disabled; policy=off; home=account";
    assert!(parse_machine_state(full).is_some());
    assert!(parse_machine_state(&format!("{full}; future-key=whatever")).is_some());
    assert!(parse_machine_state("universal-control=disabled; policy=off").is_none());
    assert!(parse_machine_state(&full.replace("home=account", "home=elsewhere")).is_none());
    assert!(parse_machine_state("").is_none());
    let older = parse_machine_state(full).unwrap();
    assert_eq!(older.rosetta, RosettaPosture::NotApplicable);
    assert!(older.rosetta_wanted);
    assert!(!older.rosetta_next());
    assert!(parse_machine_state(&format!("{full}; rosetta=installed")).is_none());
    assert!(parse_machine_state(&format!("{full}; rosetta-policy=yes")).is_none());
    assert_eq!(
        parse_machine_state(&format!("{full}; rosetta=missing; rosetta-policy=leave"))
            .map(|s| (s.rosetta, s.rosetta_wanted)),
        Some((RosettaPosture::Missing, false))
    );
}

/// The posture derivation: both keys = disabled, one key = partial, neither = the
/// OS default (an absent key is the ordinary state, not an unknown one).
#[test]
fn universal_control_posture_reads_both_keys() {
    let at = |d, e| UniversalControlState {
        measured: true,
        disable: d,
        magic_edges: e,
    };
    assert_eq!(at(Some(true), Some(true)).posture(), UcPosture::Disabled);
    assert_eq!(at(Some(true), None).posture(), UcPosture::Partial);
    assert_eq!(at(None, Some(true)).posture(), UcPosture::Partial);
    assert_eq!(at(Some(false), Some(true)).posture(), UcPosture::Partial);
    assert_eq!(at(None, None).posture(), UcPosture::Default);
    assert_eq!(at(Some(false), Some(false)).posture(), UcPosture::Default);
    let read = UniversalControlState::from_reads([Read::Absent, Read::Absent]);
    assert!(read.measured);
    assert_eq!(read.posture(), UcPosture::Default);
    assert_eq!(
        UniversalControlState::from_reads([Read::Value(true), Read::Absent]).posture(),
        UcPosture::Partial
    );
    let unread = UniversalControlState::default();
    assert!(!unread.measured);
    assert_eq!(unread.posture(), UcPosture::Unknown);
    for other in [Read::Absent, Read::Value(true)] {
        let unusable = Read::Unusable("defaults passed its deadline".into());
        for reads in [[unusable.clone(), other.clone()], [other.clone(), unusable]] {
            let state = UniversalControlState::from_reads(reads);
            assert!(!state.measured);
            assert_eq!(state.posture(), UcPosture::Unknown);
        }
    }
}

/// The verdict the read verb's `next —` line and the window's Apply button both read.
#[test]
fn an_apply_is_next_only_where_it_would_change_universal_control() {
    use UcPosture::*;
    use UniversalControlPolicy::*;
    let at = |universal_control, policy| MachineState {
        universal_control,
        policy,
        home: HomePosture::Account,
        config_unreadable: false,
        rosetta: RosettaPosture::Present,
        rosetta_wanted: true,
    };
    assert!(!at(Disabled, Off).next(), "everything done");
    assert!(at(Default, Off).next());
    assert!(
        at(Partial, Off).next(),
        "a half state is completed, not reported done"
    );
    assert!(
        at(Unknown, Off).next(),
        "an unreadable posture is re-applied, never skipped"
    );
    assert!(!at(Default, Leave).next(), "policy leave");
    let s = at(Default, Off);
    assert!(
        !MachineState {
            home: HomePosture::Mismatch,
            ..s.clone()
        }
        .next()
    );
    assert!(
        !MachineState {
            config_unreadable: true,
            ..s.clone()
        }
        .next(),
        "an unreadable config offers no next step"
    );
    let flagged = MachineState {
        config_unreadable: true,
        ..s.clone()
    };
    let line = machine_state_line(&flagged);
    assert!(line.ends_with("; config=unreadable"), "{line}");
    assert_eq!(parse_machine_state(&line), Some(flagged));
    let missing = MachineState {
        rosetta: RosettaPosture::Missing,
        ..at(Disabled, Off)
    };
    assert!(missing.next() && missing.rosetta_next());
    assert!(!missing.universal_control_next());
    assert!(
        MachineState {
            policy: Leave,
            ..missing.clone()
        }
        .next(),
        "the Universal Control opt-out is not the Rosetta one"
    );
    for (rosetta, rosetta_wanted, why) in [
        (RosettaPosture::Missing, false, "rosetta = false"),
        (
            RosettaPosture::Unknown,
            true,
            "an unread runtime is not missing",
        ),
        (RosettaPosture::NotApplicable, true, "not Apple silicon"),
        (RosettaPosture::Present, true, "installed"),
    ] {
        let state = MachineState {
            rosetta,
            rosetta_wanted,
            ..missing.clone()
        };
        assert!(!state.next(), "{why}");
    }
    assert!(
        !MachineState {
            home: HomePosture::Mismatch,
            ..missing.clone()
        }
        .next()
    );
    assert!(
        !MachineState {
            config_unreadable: true,
            ..missing.clone()
        }
        .next()
    );
    let both = MachineState {
        universal_control: Default,
        ..missing
    };
    assert!(both.universal_control_next() && both.rosetta_next());
    assert_eq!(s.universal_control_next(), s.next());
}

/// A fake Mac for Rosetta 2: whether the runtime is there, whether the probe can say,
/// whether the install succeeds and lands, and how often it ran.
struct FakeRosetta {
    present: Cell<bool>,
    probe_fails: bool,
    install_ok: bool,
    install_lands: bool,
    installs: Cell<u32>,
}

impl FakeRosetta {
    fn missing() -> Self {
        FakeRosetta {
            present: Cell::new(false),
            probe_fails: false,
            install_ok: true,
            install_lands: true,
            installs: Cell::new(0),
        }
    }
}

impl RosettaPort for FakeRosetta {
    fn present(&self) -> Result<bool, String> {
        if self.probe_fails {
            Err("permission denied".into())
        } else {
            Ok(self.present.get())
        }
    }
    fn install(&self) -> Result<(), String> {
        self.installs.set(self.installs.get() + 1);
        if !self.install_ok {
            return Err("softwareupdate exited 1: Package Authoring Error".into());
        }
        if self.install_lands {
            self.present.set(true);
        }
        Ok(())
    }
}

/// The policy over the fake: a missing Rosetta 2 is installed once and read back, an
/// installed one is left alone, `rosetta = false` and a Mac where it does not apply run
/// nothing, and an unread probe, a failed install and one that did not land are
/// failures that install nothing more.
#[test]
fn rosetta_is_installed_once_where_it_applies_and_never_on_an_unproven_read() {
    let mac = FakeRosetta::missing();
    assert_eq!(
        rosetta_posture_on(true, &mac),
        RosettaPosture::Missing,
        "read before the install"
    );
    assert_eq!(
        apply_rosetta_on(true, true, &mac),
        RosettaOutcome::Installed
    );
    assert_eq!(mac.installs.get(), 1);
    assert_eq!(rosetta_posture_on(true, &mac), RosettaPosture::Present);
    assert_eq!(
        apply_rosetta_on(true, true, &mac),
        RosettaOutcome::AlreadyPresent
    );
    assert_eq!(mac.installs.get(), 1);
    for (applies, wanted) in [(true, false), (false, true), (false, false)] {
        let mac = FakeRosetta {
            probe_fails: true,
            ..FakeRosetta::missing()
        };
        assert_eq!(
            apply_rosetta_on(applies, wanted, &mac),
            RosettaOutcome::NotApplicable
        );
        assert_eq!(mac.installs.get(), 0);
    }
    assert_eq!(
        rosetta_posture_on(false, &FakeRosetta::missing()),
        RosettaPosture::NotApplicable
    );
    let unread = FakeRosetta {
        probe_fails: true,
        ..FakeRosetta::missing()
    };
    assert_eq!(rosetta_posture_on(true, &unread), RosettaPosture::Unknown);
    assert!(matches!(
        apply_rosetta_on(true, true, &unread),
        RosettaOutcome::Failed(why) if why.contains("could not be checked")
    ));
    assert_eq!(unread.installs.get(), 0);
    let refused = FakeRosetta {
        install_ok: false,
        ..FakeRosetta::missing()
    };
    assert!(matches!(
        apply_rosetta_on(true, true, &refused),
        RosettaOutcome::Failed(why) if why.contains("exited 1")
    ));
    let hollow = FakeRosetta {
        install_lands: false,
        ..FakeRosetta::missing()
    };
    assert!(matches!(
        apply_rosetta_on(true, true, &hollow),
        RosettaOutcome::Failed(why) if why.contains("still not there")
    ));
    assert_eq!(hollow.installs.get(), 1);
    let mac = FakeRosetta::missing();
    let expected = if ROSETTA_APPLIES {
        RosettaOutcome::Installed
    } else {
        RosettaOutcome::NotApplicable
    };
    assert_eq!(apply_rosetta(true, &mac), expected);
}

/// The doctor's Rosetta rows: none where it does not apply, a warning only when the
/// apply would install it or could not read it, and the opt-out read as a choice.
#[test]
fn the_rosetta_doctor_row_warns_only_where_an_apply_would_act_or_could_not_read() {
    use crate::doctor::Level;
    assert!(RosettaPosture::NotApplicable.doctor_row(true).is_none());
    let row = |p: RosettaPosture, wanted| p.doctor_row(wanted).unwrap();
    assert_eq!(row(RosettaPosture::Present, true).level, Level::Ok);
    let missing = row(RosettaPosture::Missing, true);
    assert_eq!(missing.level, Level::Warn);
    assert!(
        missing.text.starts_with("Rosetta 2 is not installed — "),
        "the fact is the first clause: {}",
        missing.text
    );
    assert!(
        missing.text.contains("[machine] rosetta = false"),
        "{}",
        missing.text
    );
    let left = row(RosettaPosture::Missing, false);
    assert_eq!(left.level, Level::Ok);
    assert!(left.text.contains("rosetta = false"), "{}", left.text);
    assert_eq!(row(RosettaPosture::Unknown, true).level, Level::Warn);
}

#[cfg(unix)]
#[test]
fn record_applied_does_not_follow_temp_symlink() {
    let layout = crate::testdir::layout("atpkg-machine-applied-symlink-");
    let cfg = crate::config::MachineConfig::default();
    let victim = layout.prefix.join("victim.txt");
    std::fs::write(&victim, "untouched").unwrap();
    let tmp = layout
        .prefix
        .join(format!("machine.applied.tmp-{}", std::process::id()));
    std::os::unix::fs::symlink(&victim, &tmp).unwrap();
    record_applied(&layout, &cfg);
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "untouched");
    assert!(!changed_since_applied(&layout, &cfg));
}
