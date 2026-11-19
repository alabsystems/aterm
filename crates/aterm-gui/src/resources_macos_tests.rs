// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::sync::Arc;

use aterm_types::macos_setup::updates::Key;

pub(crate) const HOME: &str = "/Users//t";

/// `pmset -g custom` and `pmset -g cap` answered from text.
pub(crate) struct Pmset {
    pub(crate) custom: &'static str,
    pub(crate) cap: &'static str,
}

impl power::Reads for Pmset {
    fn pmset(&self, args: &[&str]) -> Result<String, String> {
        match args {
            ["-g", "custom"] => Ok(self.custom.to_owned()),
            ["-g", "cap"] => Ok(self.cap.to_owned()),
            _ => Err(format!("pmset {args:?}")),
        }
    }
}

/// A desktop whose AC profile is the starter's but for system sleep, 10 minutes.
pub(crate) const SLEEPS: &str = "AC Power:
 Sleep On Power Button 1
 sleep                10
 disksleep            0
 displaysleep         15
 powernap             0
 womp                 1
 autorestart          1
";

/// The same desktop at the starter baseline.
pub(crate) const AT_BASELINE: &str = "AC Power:
 sleep                0
 disksleep            0
 displaysleep         15
 powernap             0
 womp                 1
 autorestart          1
";

pub(crate) const CAP: &str = "Capabilities for AC Power:
 displaysleep
 disksleep
 sleep
 womp
 powernap
 autorestart
";

/// Software Update with checking `check` and every key at the starter's value.
pub(crate) fn updates_at(check: bool) -> UpdatesNow {
    let starter = baseline::starter_updates();
    UpdatesNow {
        check: Ok(check),
        keys: updates::KEYS
            .iter()
            .map(|key: &Key| {
                let held = key
                    .field
                    .of(&starter)
                    .map_or(updates::Held::Unset, updates::Held::Switch);
                (*key, Ok(held))
            })
            .collect(),
    }
}

/// A reading of this Mac: `custom` for pmset, checking `check`, and the build cache's standing
/// `cache`, beside any `more` build folders.
pub(crate) fn reading(
    custom: &'static str,
    check: bool,
    cache: Tm,
    more: Vec<(PathBuf, Tm)>,
) -> MacosReading {
    let home = PathBuf::from(HOME);
    let mut build_folders = vec![(aterm_toolchain::build_cache::cache_root(&home), cache)];
    build_folders.extend(more);
    MacosReading {
        power: power::rows(
            &Pmset { custom, cap: CAP },
            power::Source::Ac,
            &baseline::starter_power_ac(),
        ),
        updates: updates_at(check),
        last_restart: Some(Sent {
            pid: 537,
            start_s: 1_763_000_000,
            at_s: 1_763_290_000,
        }),
        build_folders,
        console: true,
        home: Some(home),
        read_s: 1_763_300_000,
    }
}

/// A Mac that differs from Wanted in one item a row: sleep 10 minutes, checking off, the build
/// cache backed up (and the build output found indexed), fseventsd past its bar, a folder the
/// scope keeps out missing from setup's receipt.
pub(crate) fn differing() -> MacosReading {
    reading(SLEEPS, false, Tm::In, Vec::new())
}

/// The same Mac at Wanted.
pub(crate) fn at_wanted() -> MacosReading {
    reading(AT_BASELINE, true, Tm::Out, Vec::new())
}

/// The page's reading of a Mac whose data volume indexes and whose fseventsd holds `footprint`
/// of 64 GiB.
pub(crate) fn snapshot(footprint: u64) -> aterm_vitals::Snapshot {
    let mut snap = crate::resources_spotlight::tests::snapshot(
        crate::resources_spotlight::tests::volumes(Indexing::Enabled),
    );
    snap.services = Some(Arc::new(aterm_process_facts::services::Reading {
        total: 64 << 30,
        services: vec![aterm_process_facts::services::Service {
            coalition: 77,
            pid: 537,
            start_s: 1_763_000_000,
            uid: FSEVENTSD.uid,
            name: FSEVENTSD.name.to_owned(),
            path: Some(FSEVENTSD.path.to_owned()),
            live: 1,
            footprint,
            ..aterm_process_facts::services::Service::default()
        }],
        ..aterm_process_facts::services::Reading::default()
    }));
    snap
}

/// fseventsd past the 8 GiB bar of a 64 GiB Mac, and well under it.
pub(crate) const PAST: u64 = 20 << 30;
pub(crate) const UNDER: u64 = 4 << 20;

/// The scope SD-95's probe read with $HOME/trust missing from the receipt, the build output found
/// indexed (`build_indexed`) or not.
pub(crate) fn scope(differs: bool) -> crate::resources_spotlight::ScopeReading {
    use crate::resources_spotlight::Found;
    if !differs {
        return crate::resources_spotlight::tests::covered();
    }
    let mut scope = crate::resources_spotlight::tests::missing_trust();
    if let Some(cache) = scope.listing.cache.clone() {
        scope.found.insert(cache, Found::Item);
    }
    scope
}

/// THE WINDOW AND `aterm machines` READ ONE POWER AND UPDATES READING: one `pmset` fixture and
/// one `softwareupdate --schedule` fixture, and one `defaults export` of Software Update's
/// domain, behind aterm-machines' fake port, read by its items and by `aterm machines
/// macos-reading`, whose printed text the window parses back, give each setting and each of the
/// four keys the same Now and the same verdict; and the starter tables the window judges by are
/// the ones `aterm machines init` writes.
/// CONTROL: an AC profile whose `pmset -g cap` lists no autorestart reads NotApplicable on both
/// sides, and a setting at its wanted value reads Ok on both. A domain whose export fails (a
/// file `defaults` cannot read) leaves every key unread on both sides, never "not set", and a
/// key that holds no switch, the `1` integer that satisfies one, and an unset key each read
/// alike.
#[cfg(all(target_os = "macos", debug_assertions))]
#[test]
fn the_window_and_aterm_machines_read_one_power_and_updates_reading() {
    use aterm_machines::items::Ctx;
    use aterm_machines::model::{HostFacts, Probe};
    use aterm_machines::port::{Fake, Ran};
    use aterm_types::macos_setup::baseline::PowerProfile;
    use aterm_types::macos_setup::readout::Readout;
    const NO_AUTORESTART: &str = "Capabilities for AC Power:\n displaysleep\n sleep\n";
    let text =
        aterm_machines::baseline::parse(aterm_machines::controller::templates::DEFAULT_BASELINE)
            .expect("the starter parses");
    let ac = text
        .power
        .as_ref()
        .and_then(|table| table.ac.as_ref())
        .expect("[power.ac]");
    assert_eq!(
        PowerProfile::from(ac),
        baseline::starter_power_ac(),
        "the window judges by the [power.ac] `aterm machines init` writes"
    );
    let table = text.updates.as_ref().expect("[updates]");
    assert_eq!(Updates::from(table), baseline::starter_updates());

    let ctx = Ctx {
        facts: HostFacts::default(),
        users: Vec::new(),
        console: None,
        host_name: "m42".into(),
        cache_dir: "/c".into(),
        disrupt: true,
    };
    let mut cases = 0;
    // The window reads what `aterm machines macos-reading` printed over the same port.
    let window_reads = |sys: &Fake| {
        Readout::parse(&aterm_machines::cli::macos_reading(sys)).expect("the reading parses")
    };
    for cap in [CAP, NO_AUTORESTART] {
        let sys = Fake::root();
        sys.on("/usr/bin/pmset -g custom", Ran::ok(SLEEPS))
            .on("/usr/bin/pmset -g cap", Ran::ok(cap));
        let items = aterm_machines::items::power::expand(text.power.as_ref().unwrap(), &ctx)
            .expect("the items");
        let window = power::rows(
            &window_reads(&sys),
            power::Source::Ac,
            &baseline::starter_power_ac(),
        );
        assert_eq!(items.len(), window.len());
        for row in &window {
            let id = format!("power.ac.{}", row.setting.id);
            let item = items.iter().find(|item| item.id() == id).expect(&id);
            let machines = item.probe(&sys, &ctx);
            let render = |v: u64| power::render(row.setting.kind, v);
            match (&row.now, &machines) {
                (Ok(now), Probe::Ok { now: said }) if *now == row.wanted => {
                    assert_eq!(*said, render(*now), "{id}");
                }
                (Ok(now), Probe::Change { from, to }) if *now != row.wanted => {
                    assert_eq!((from, to), (&render(*now), &render(row.wanted)), "{id}");
                    assert!(row.differs(), "{id}");
                }
                (Err(power::Unread::NotApplicable(why)), Probe::NotApplicable { why: said }) => {
                    assert_eq!(why, said, "{id}");
                }
                (window, machines) => {
                    panic!("{id}: the window reads {window:?}, aterm machines {machines:?}")
                }
            }
            cases += 1;
        }
        let restart = window
            .iter()
            .find(|row| row.setting.key == "autorestart")
            .expect("restart after a power failure");
        assert_eq!(
            matches!(restart.now, Err(power::Unread::NotApplicable(_))),
            cap == NO_AUTORESTART,
            "CONTROL: autorestart reads by the capability"
        );
    }
    assert!(cases >= 12, "{cases}");

    // Checking for updates: one parse, one verdict.
    for (text_out, on) in [
        ("Automatic checking for updates is turned off.\n", false),
        ("Automatic checking for updates is turned on.\n", true),
    ] {
        let sys = Fake::root();
        sys.on("/usr/sbin/softwareupdate --schedule", Ran::ok(text_out));
        let items = aterm_machines::items::updates::expand(table, &ctx).expect("the items");
        let check = items
            .iter()
            .find(|item| item.id() == "updates.automatic-check")
            .expect("the check");
        let window = updates::now(&window_reads(&sys));
        assert_eq!(window.check, Ok(on));
        let machines = check.probe(&sys, &ctx);
        assert_eq!(
            matches!(machines, Probe::Change { .. }),
            updates::differs(&window, &baseline::starter_updates()).contains(&"check for updates"),
            "{machines:?}"
        );
        assert_eq!(matches!(machines, Probe::Ok { .. }), on, "{machines:?}");
    }

    // The four keys: one `defaults export`, one judgment. AutomaticDownload off (wanted on),
    // the macOS install key at the integer 1 (wanted off), CriticalUpdateInstall at the
    // integer 1 (wanted on), ConfigDataInstall a string; then the same domain unset, at the
    // starter, and unreadable.
    const EXPORT: &str = "/usr/bin/defaults export /Library/Preferences/com.apple.SoftwareUpdate -";
    let export = |body: &str| {
        Ran::ok(&format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n{body}\n</plist>\n"
        ))
    };
    let mixed = "<dict>\n\t<key>AutomaticDownload</key>\n\t<false/>\n\t<key>AutomaticallyInstallMacOSUpdates</key>\n\t<integer>1</integer>\n\t<key>CriticalUpdateInstall</key>\n\t<integer>1</integer>\n\t<key>ConfigDataInstall</key>\n\t<string>yes</string>\n</dict>";
    let starter = "<dict>\n\t<key>AutomaticDownload</key>\n\t<true/>\n\t<key>AutomaticallyInstallMacOSUpdates</key>\n\t<false/>\n\t<key>CriticalUpdateInstall</key>\n\t<true/>\n\t<key>ConfigDataInstall</key>\n\t<true/>\n</dict>";
    let unreadable = Ran {
        status: Some(1),
        stdout: Vec::new(),
        stderr: b"Could not read domain".to_vec(),
    };
    let mut seen = std::collections::BTreeSet::new();
    for reply in [
        export(mixed),
        export("<dict/>"),
        export(starter),
        unreadable,
    ] {
        let sys = Fake::root();
        sys.on(
            "/usr/sbin/softwareupdate --schedule",
            Ran::ok("Automatic checking for updates is turned on.\n"),
        )
        .on(EXPORT, reply);
        let items = aterm_machines::items::updates::expand(table, &ctx).expect("the items");
        let window = updates::now(&window_reads(&sys));
        let differing = updates::differs(&window, &baseline::starter_updates());
        assert_eq!(window.keys.len(), updates::KEYS.len());
        for (key, held) in &window.keys {
            let item = items.iter().find(|item| item.id() == key.id).expect(key.id);
            let machines = item.probe(&sys, &ctx);
            let now = |said: &str| match held {
                Ok(updates::Held::Switch(on)) => {
                    assert!(
                        said == on.to_string() || said == u8::from(*on).to_string(),
                        "{}: {said}",
                        key.key
                    );
                }
                Ok(updates::Held::Unset) => assert_eq!(said, "absent", "{}", key.key),
                Ok(updates::Held::Other(value)) => assert_eq!(said, value, "{}", key.key),
                Err(_) => panic!(
                    "{}: unread in the window, {said} in aterm machines",
                    key.key
                ),
            };
            match &machines {
                Probe::Ok { now: said } => {
                    now(said);
                    assert!(!differing.contains(&key.what), "{}: {differing:?}", key.key);
                    seen.insert("ok");
                }
                Probe::Change { from, .. } => {
                    now(from);
                    assert!(differing.contains(&key.what), "{}: {differing:?}", key.key);
                    seen.insert(match held {
                        Ok(updates::Held::Other(_)) => "other",
                        Ok(updates::Held::Unset) => "unset",
                        _ => "change",
                    });
                }
                Probe::Unusable { why } => {
                    assert_eq!(held.as_ref().err(), Some(why), "{}", key.key);
                    assert!(!differing.contains(&key.what), "{}", key.key);
                    seen.insert("unread");
                }
                other => panic!(
                    "{}: the window reads {held:?}, aterm machines {other:?}",
                    key.key
                ),
            }
        }
    }
    assert_eq!(
        seen.into_iter().collect::<Vec<_>>(),
        ["change", "ok", "other", "unread", "unset"],
        "CONTROL: every kind of reading was judged"
    );
}

/// A BUILD FOLDER UNDER A FOLDER MACOS GUARDS IS NEVER ASKED AFTER, AND ONE THAT DOES NOT EXIST
/// IS NO BUILD FOLDER YET: the reads behind the slot's live reading panic on any path under
/// ~/Documents, the guarded folder reads Guarded, a missing build cache reads Absent without
/// `tmutil` (which answers `[UNKNOWN]` for it), and the Time Machine row says so at Wanted with
/// no press.
/// CONTROL: an existing folder is asked of `tmutil` and its answer kept, a failed check is
/// unread, and an existing folder Time Machine backs up still differs.
#[test]
fn a_guarded_build_folder_is_never_asked_after_and_a_missing_one_is_no_folder_yet() {
    let home = PathBuf::from(HOME);
    let documents = home.join("Documents/aterm/target");
    let cache = aterm_toolchain::build_cache::cache_root(&home);
    let present = home.join("src/aterm/target");
    let unchecked = home.join("src/locked/target");
    fn guard(path: &Path) {
        assert!(
            !path.starts_with("/Users//t/Documents"),
            "asked after a guarded folder: {}",
            path.display()
        );
    }
    let reads = TmReads {
        exists: |path| {
            guard(path);
            if path.ends_with("locked/target") {
                Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
            } else {
                Ok(path.starts_with("/Users//t/src"))
            }
        },
        excluded: |path| {
            guard(path);
            assert!(
                path.starts_with("/Users//t/src"),
                "tmutil of {}",
                path.display()
            );
            Ok(false)
        },
    };
    let got = standings(
        &home,
        vec![
            documents.clone(),
            cache.clone(),
            present.clone(),
            unchecked.clone(),
        ],
        reads,
    );
    assert_eq!(got[0], (documents, Tm::Guarded("~/Documents".into())));
    assert_eq!(got[1], (cache.clone(), Tm::Absent));
    assert_eq!(got[2], (present, Tm::In), "CONTROL: asked of tmutil");
    assert!(
        matches!(&got[3], (path, Tm::Unread(why)) if *path == unchecked && why.contains("not checked")),
        "CONTROL: {:?}",
        got[3]
    );

    let mut macos = at_wanted();
    macos.build_folders = vec![(cache, Tm::Absent)];
    let row = time_machine_row(Some(&macos));
    assert_eq!(row.now, "no build folder yet");
    assert!(!row.differs && row.action.is_none(), "{row:?}");
    assert!(row.words.contains("does not exist yet"), "{}", row.words);
    let row = time_machine_row(Some(&differing()));
    assert!(row.differs && row.action.is_some(), "CONTROL: {row:?}");
}
