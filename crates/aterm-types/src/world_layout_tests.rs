// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The scratch world's laws, L1 to L5, each with its negative controls: the shapes the worlds
//! written before the layout took, which each law reports.

use super::*;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// [`LANES_OFF`] as it was written by hand before the lanes became rows. Its bytes do not move:
/// every site that still appends it writes the same tables.
const LANES_OFF_BEFORE: &str = "[update]\nenabled = false\nauto_apply = false\n\
                                [packages]\nenabled = false\n\
                                [machine]\nuniversal_control = \"leave\"\n\
                                [keeper]\nenabled = false\n\
                                [swarm]\nenabled = false\n";

/// Heads of each shape a site writes: none; root keys only; root keys and an own table; one that
/// sets the bell itself; one with a `[harness]` table; one with no final newline.
const HEADS: [&str; 6] = [
    "",
    "agents_auto_prime = false\n",
    "serious_mode = true\ncursor_trail = true\n[sparkle_words]\nenabled = false\n",
    "bell_sound = true\ntemporal_recording = true\n",
    "agents_auto_prime = false\n[harness]\nenabled = false\n",
    "temporal_recording = true",
];

/// `text` as one TOML document, as aterm's reader takes it.
fn document(text: &str) -> Result<aterm_toml::Value, String> {
    aterm_toml::from_str::<aterm_toml::Value>(text).map_err(|error| error.to_string())
}

/// Where `text`, a world's `aterm.toml` for `head` driving `driving`, breaks L1, each as a name:
/// `parse` where it is not one TOML document; each `table.key` of a lane it does not drive that
/// it does not hold at the lane's off value; `[table]` for a driven lane whose off table it
/// writes anyway; each entry of the head (a root key, or one of its own tables whole) that the
/// document's root does not hold with the head's value; and `bell_sound` where the head does
/// not set it at its root and the document's root does not hold it false.
fn short_of_l1(text: &str, head: &str, driving: &[Lane]) -> Vec<String> {
    let Ok(doc) = document(text) else {
        return vec!["parse".to_owned()];
    };
    let own = document(head).expect("a head is one document");
    let mut short = Vec::new();
    for lane in Lane::ALL {
        let header = format!("[{}]", lane.table());
        let headers = |text: &str| text.lines().filter(|line| *line == header).count();
        if driving.contains(&lane) {
            if headers(text) > headers(head) {
                short.push(header);
            }
            continue;
        }
        let off = document(lane.off()).expect("a lane's off table is one document");
        let table = off.get(lane.table()).and_then(aterm_toml::Value::as_table);
        for (key, value) in table.expect("an off table is a table") {
            if doc
                .get(lane.table())
                .and_then(|held| held.get(key.as_str()))
                != Some(value)
            {
                short.push(format!("{}.{key}", lane.table()));
            }
        }
    }
    for (key, value) in own.as_table().expect("a document is a table") {
        if doc.get(key.as_str()) != Some(value) {
            short.push(key.clone());
        }
    }
    if own.get("bell_sound").is_none()
        && doc.get("bell_sound") != Some(&aterm_toml::Value::Boolean(false))
    {
        short.push("bell_sound".to_owned());
    }
    short
}

/// Every driving set L1 is held over: none, each lane alone, all.
fn driving_sets() -> Vec<Vec<Lane>> {
    let mut sets = vec![Vec::new(), Lane::ALL.to_vec()];
    sets.extend(Lane::ALL.map(|lane| vec![lane]));
    sets
}

/// L1: every lane a world does not drive is off. For every head shape and driving set the
/// rendered `aterm.toml` is one document holding each undriven lane's off entries, no off table
/// of a driven lane, the head's root keys at the root with the head's values and its own tables
/// whole, and the bell off at the root unless the head sets it; [`LANES_OFF`] is the lanes' off
/// tables in order, byte for byte the hand-written text it replaced.
#[test]
fn every_lane_a_world_does_not_drive_is_off() {
    for head in HEADS {
        for driving in driving_sets() {
            let text = Config::new(head)
                .driving(&driving)
                .render()
                .unwrap_or_else(|refusal| panic!("{head:?} driving {driving:?}: {refusal}"));
            assert_eq!(
                short_of_l1(&text, head, &driving),
                Vec::<String>::new(),
                "{head:?} driving {driving:?}:\n{text}"
            );
        }
    }
    assert_eq!(LANES_OFF, Lane::ALL.map(Lane::off).concat());
    assert_eq!(LANES_OFF, LANES_OFF_BEFORE);
    assert_eq!(Config::OFF.render(), Ok(format!("{QUIET_ROOT}{LANES_OFF}")));
    assert_eq!(
        Config::new("temporal_recording = true\n").render(),
        Ok(format!(
            "bell_sound = false\ntemporal_recording = true\n{LANES_OFF}"
        )),
        "the order probe's text, its root keys then the lanes"
    );
}

/// L1, negative control (a): a head that names a lane it does not drive, by a table header
/// (`[update]`, `[[keeper]]`, `[machine.extra]`, a quoted `["update"]`) or a root key
/// (`swarm.enabled`, an inline `packages` table), is refused, and its unguarded concatenation
/// with the lanes off, which a site that wrote its own text could make, fails L1's checker. Once
/// the config drives the lane its head configures it and no off table follows. A key inside the
/// site's own table, a lane's name as a value and a comment name no lane.
#[test]
fn a_head_that_names_a_lane_it_does_not_drive_is_refused() {
    for (head, lane) in [
        ("[update]\nenabled = true\n", Lane::Update),
        ("swarm.enabled = true\n", Lane::Swarm),
        ("[[keeper]]\nname = 'x'\n", Lane::Keeper),
        ("[machine.extra]\nx = 1\n", Lane::Machine),
        ("packages = { enabled = true }\n", Lane::Packages),
        ("x = 1\n[ \"update\" ]\nenabled = true\n", Lane::Update),
    ] {
        match Config::new(head).render() {
            Err(ConfigRefusal::NamesLane { lane: named, .. }) => assert_eq!(named, lane, "{head}"),
            other => panic!("{head:?} was not refused: {other:?}"),
        }
        let driven = Config::new(head)
            .driving(&[lane])
            .render()
            .unwrap_or_else(|refusal| panic!("{head:?}: {refusal}"));
        assert_eq!(
            short_of_l1(&driven, head, &[lane]),
            Vec::<String>::new(),
            "{driven}"
        );
        let unguarded = format!("{QUIET_ROOT}{head}{LANES_OFF}");
        assert_ne!(
            short_of_l1(&unguarded, head, &[]),
            Vec::<String>::new(),
            "{unguarded}"
        );
    }
    for head in [
        "[harness]\nupdate = 1\n",
        "accounts = 'swarm'\n",
        "# [update]\n",
    ] {
        assert!(Config::new(head).render().is_ok(), "{head}");
    }
}

/// L1, read by lines: a multi-line array's lines are its value's, so a header-shaped element
/// (`[1]`, `["update"]`) names no lane and ends no root, and a root key after the array still
/// counts; three quotes inside a one-line string or a comment open nothing. Negative control: a
/// head that opens a multi-line string, whose lines would read as headers and keys, is refused,
/// whatever lanes it drives. Read by lines, a `[y]` line in one would end the root, so a root
/// `bell_sound` after it would go unseen and the bell be written twice, which is no document.
#[test]
fn a_head_is_read_by_its_lines_and_never_holds_a_multi_line_string() {
    for head in [
        "x = [\n[1],\n]\nbell_sound = true\n",
        "x = [\n[1],\n[\"update\"],\n]\n",
        "x = [\n  { a = 1 },\n]\n[harness]\nenabled = false\n",
        "x = \"'''\"\ny = '\"\"\"'\nz = \"a\\\"[b\"\n# \"\"\" and ''' and [\nbell_sound = true\n",
    ] {
        let text = Config::new(head)
            .render()
            .unwrap_or_else(|refusal| panic!("{head:?}: {refusal}"));
        assert_eq!(
            short_of_l1(&text, head, &[]),
            Vec::<String>::new(),
            "{text}"
        );
    }
    let bell_after = "x = \"\"\"\n[y]\n\"\"\"\nbell_sound = true\n";
    for head in [
        "x = \"\"\"\n[update]\n\"\"\"\n",
        "x = '''\nupdate = 1\n'''\n",
        bell_after,
        "x = [\n'''a''',\n]\n",
    ] {
        for driving in driving_sets() {
            match Config::new(head).driving(&driving).render() {
                Err(ConfigRefusal::MultiLineString { .. }) => {}
                other => panic!("{head:?} driving {driving:?} was not refused: {other:?}"),
            }
        }
    }
    let bell_unseen = format!("{QUIET_ROOT}{bell_after}{LANES_OFF}");
    assert_eq!(short_of_l1(&bell_unseen, bell_after, &[]), ["parse"]);
}

/// L1, negative control (b): the `aterm.toml` texts the worlds wrote before the layout, held here
/// as fixtures, each fall short of L1 by exactly the entries they left out.
#[test]
fn the_worlds_written_before_the_layout_fall_short_by_their_missing_entries() {
    let lab_head = "agents_auto_prime = false\n[harness]\nenabled = false\n";
    let cases: [(&str, &str, String, &[&str]); 5] = [
        (
            "the lab instance world (CONFIG_OFF after bell_off)",
            lab_head,
            "bell_sound = false\nagents_auto_prime = false\n\
             [update]\nenabled = false\nauto_apply = false\n\
             [packages]\nenabled = false\n\
             [harness]\nenabled = false\n\
             [machine]\nuniversal_control = \"leave\"\n"
                .to_owned(),
            &["keeper.enabled", "swarm.enabled"],
        ),
        (
            "the lab fleet world (CONFIG after bell_off)",
            lab_head,
            "bell_sound = false\nagents_auto_prime = false\n\
             [update]\nenabled = false\nauto_apply = false\n\
             [packages]\nenabled = false\n\
             [machine]\nuniversal_control = \"leave\"\n\
             [harness]\nenabled = false\n"
                .to_owned(),
            &["keeper.enabled", "swarm.enabled"],
        ),
        (
            "the release slice probe's two tables",
            "",
            "[update]\nenabled = false\n[packages]\nenabled = false\n".to_owned(),
            &[
                "update.auto_apply",
                "machine.universal_control",
                "keeper.enabled",
                "swarm.enabled",
                "bell_sound",
            ],
        ),
        (
            "the integration-test rig (agents_auto_prime then CONFIG_OFF)",
            lab_head,
            "agents_auto_prime = false\n\
             [update]\nenabled = false\nauto_apply = false\n\
             [packages]\nenabled = false\n\
             [machine]\nuniversal_control = \"leave\"\n\
             [harness]\nenabled = false\n"
                .to_owned(),
            &["keeper.enabled", "swarm.enabled", "bell_sound"],
        ),
        (
            "lab up's tables (scratch_aterm_toml after bell_off)",
            "[harness]\nprobe_api = false\n",
            "bell_sound = false\n\n[update]\nenabled = false\n[packages]\nenabled = false\n\
             [harness]\nprobe_api = false\n\n"
                .to_owned(),
            &[
                "update.auto_apply",
                "machine.universal_control",
                "keeper.enabled",
                "swarm.enabled",
            ],
        ),
    ];
    for (world, head, text, missing) in cases {
        assert_eq!(short_of_l1(&text, head, &[]), missing, "{world}");
    }
}

/// L1, negative control (c): root keys written after the tables, the order probe's old mistake,
/// are read into the last table, and the root-key clause names them.
#[test]
fn root_keys_after_the_tables_are_not_at_the_root() {
    let head = "bell_sound = false\ntemporal_recording = true\n";
    let tables_first = format!("{LANES_OFF}{head}");
    assert_eq!(
        short_of_l1(&tables_first, head, &[]),
        ["bell_sound", "temporal_recording"]
    );
}

/// A root that is absolute on this host, so the laws hold the same paths on any host.
fn root() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(r"C:\w")
    } else {
        PathBuf::from("/w")
    }
}

/// A Windows caller's environment as [`Layout::env`] asks it: every machine variable set.
fn machine(name: &str) -> Option<OsString> {
    WINDOWS_MACHINE_VARS
        .contains(&name)
        .then(|| OsString::from(format!("machine {name}")))
}

/// `name`'s value in `env`.
fn var<'e>(env: &'e [(&'static str, OsString)], name: &str) -> Option<&'e OsString> {
    env.iter()
        .find(|(held, _)| *held == name)
        .map(|(_, value)| value)
}

/// The state inputs a build on `on` reads from `env`: its HOME (`USERPROFILE` on Windows),
/// `XDG_STATE_HOME` and `LOCALAPPDATA`.
fn state_inputs(on: Platform, env: &[(&'static str, OsString)]) -> crate::dirs::StatePlatform {
    let path = |name: &str| var(env, name).map(PathBuf::from);
    crate::dirs::StatePlatform {
        home: path(match on {
            Platform::Windows => "USERPROFILE",
            Platform::MacOs | Platform::Xdg => "HOME",
        }),
        xdg_state_home: path("XDG_STATE_HOME"),
        local_app_data: path("LOCALAPPDATA"),
    }
}

/// Where a build on `on` in the environment `env` keeps its state in two folders, or in none: a
/// build without development seams resolves its state root from the world's variables, one with
/// them takes [`MARKER`] as its root.
fn state_split(on: Platform, env: &[(&'static str, OsString)]) -> Option<String> {
    let release = crate::dirs::resolve_state_dir_for(on, None, state_inputs(on, env));
    let marker = var(env, MARKER).map(PathBuf::from);
    let debug = crate::dirs::resolve_state_dir_for(on, marker, state_inputs(on, env));
    (release.is_none() || release != debug)
        .then(|| format!("{on:?}: a release build at {release:?}, a debug build at {debug:?}"))
}

/// An environment of `pairs`.
fn env_of(pairs: &[(&'static str, PathBuf)]) -> Vec<(&'static str, OsString)> {
    pairs
        .iter()
        .map(|(name, path)| (*name, path.clone().into_os_string()))
        .collect()
}

/// L2: both builds keep a world's state in one folder. On every platform, from any host, a
/// build without development seams resolves its state root from the world's environment to
/// [`Layout::state`], and the marker names that folder, which a build with them takes as its
/// root.
#[test]
fn both_builds_keep_a_worlds_state_in_one_folder() {
    for on in Platform::ALL {
        let layout = Layout::for_platform(on, &root()).expect("a short root fits");
        let env = layout.env(&machine).expect("every machine variable is set");
        assert_eq!(state_split(on, &env), None);
        assert_eq!(var(&env, MARKER).map(PathBuf::from), Some(layout.state()));
    }
    let host = Layout::under(&root()).expect("the host is a platform a world knows");
    assert_eq!(state_home(&host.home), Some(host.state()));
}

/// L2, negative controls: the fleet world's and the test rig's shape (`XDG_STATE_HOME` and the
/// marker both `root/state`) splits the state on macOS and the other Unixes; lab up's (the marker
/// at `w/state`) splits it on macOS; and a Windows world whose environment leaves out
/// `LOCALAPPDATA` gives a release build no state root at all.
#[test]
fn the_markers_the_worlds_set_before_the_layout_split_the_state() {
    let home = root().join("home");
    let rig = env_of(&[
        ("HOME", home.clone()),
        ("XDG_STATE_HOME", root().join("state")),
        (MARKER, root().join("state")),
    ]);
    for on in [Platform::MacOs, Platform::Xdg] {
        assert!(state_split(on, &rig).is_some(), "{on:?}");
    }
    let lab_up = env_of(&[("HOME", home), (MARKER, root().join("state"))]);
    assert!(state_split(Platform::MacOs, &lab_up).is_some());
    let windows = Layout::for_platform(Platform::Windows, &root())
        .expect("a short root fits")
        .env(&machine)
        .expect("every machine variable is set");
    let without_local: Vec<_> = windows
        .into_iter()
        .filter(|(name, _)| *name != "LOCALAPPDATA")
        .collect();
    assert!(state_split(Platform::Windows, &without_local).is_some());
    assert_eq!(
        crate::dirs::resolve_state_dir_for(
            Platform::Windows,
            None,
            state_inputs(Platform::Windows, &without_local)
        ),
        None
    );
}

/// The `aterm.toml` aterm reads in the environment `env` ([`crate::dirs::aterm_config_path`]).
fn read_config(env: &[(&'static str, OsString)]) -> Option<PathBuf> {
    let value = |name: &str| var(env, name).map(OsString::as_os_str);
    crate::dirs::resolve_aterm_config_path(
        value("XDG_CONFIG_HOME"),
        value("APPDATA"),
        value("HOME"),
    )
}

/// L3: the world's `aterm.toml` is the file aterm reads. On every platform aterm's config reader
/// resolves the world's environment to [`Layout::config_file`], and that is the one
/// `aterm.toml` [`Layout::files`] writes.
#[test]
fn a_worlds_aterm_toml_is_the_file_aterm_reads() {
    for on in Platform::ALL {
        let layout = Layout::for_platform(on, &root()).expect("a short root fits");
        let env = layout.env(&machine).expect("every machine variable is set");
        assert_eq!(read_config(&env), Some(layout.config_file()), "{on:?}");
        let files = layout.files(&Config::OFF).expect("no head names a lane");
        let tomls: Vec<_> = files
            .iter()
            .filter(|(path, _)| path.file_name() == Some("aterm.toml".as_ref()))
            .map(|(path, _)| path.clone())
            .collect();
        assert_eq!(tomls, [layout.config_file()], "{on:?}");
    }
}

/// L3, negative controls: a world that wrote `aterm.toml` straight into its configuration
/// folder (no `aterm/` folder) writes a file aterm never reads, and a Windows world whose
/// environment leaves out `XDG_CONFIG_HOME` is read elsewhere (on Windows, `APPDATA`'s).
#[test]
fn an_aterm_toml_beside_the_one_aterm_reads_is_never_read() {
    let layout = Layout::under(&root()).expect("the host is a platform a world knows");
    let env = layout.env(&machine).expect("every machine variable is set");
    assert_ne!(
        read_config(&env),
        Some(layout.config_home.join("aterm.toml"))
    );
    let windows = Layout::for_platform(Platform::Windows, &root()).expect("a short root fits");
    let without_config: Vec<_> = windows
        .env(&machine)
        .expect("every machine variable is set")
        .into_iter()
        .filter(|(name, _)| *name != "XDG_CONFIG_HOME")
        .collect();
    let read = read_config(&without_config);
    assert_ne!(read, Some(windows.config_file()));
    if cfg!(windows) {
        let appdata = var(&without_config, "APPDATA").map(PathBuf::from);
        assert_eq!(
            read,
            appdata.map(|dir| dir.join("aterm").join("aterm.toml"))
        );
    }
}

/// L4: nothing reaches a world but what it names. A Unix world takes nothing from its caller's
/// environment, a Windows one exactly [`WINDOWS_MACHINE_VARS`]; each variable is set once.
#[test]
fn nothing_reaches_a_world_but_what_it_names() {
    let leak = |_: &str| Some(OsString::from("LEAK"));
    for on in Platform::ALL {
        let layout = Layout::for_platform(on, &root()).expect("a short root fits");
        let env = layout.env(&leak).expect("every machine variable is set");
        let leaked: Vec<&str> = env
            .iter()
            .filter(|(_, value)| value == "LEAK")
            .map(|(name, _)| *name)
            .collect();
        match on {
            Platform::Windows => assert_eq!(leaked, WINDOWS_MACHINE_VARS),
            Platform::MacOs | Platform::Xdg => assert_eq!(leaked, Vec::<&str>::new(), "{on:?}"),
        }
        let mut names: Vec<String> = env
            .iter()
            .map(|(name, _)| name.to_ascii_uppercase())
            .collect();
        names.sort();
        let all = names.len();
        names.dedup();
        assert_eq!(names.len(), all, "{on:?} sets a variable twice: {env:?}");
    }
}

/// L4, negative control: a Windows caller without `SystemRoot`, or with `SystemDrive` empty,
/// gets no environment, since a program started without them writes a literal folder of that
/// name.
#[test]
fn a_windows_world_needs_the_machines_own_folders() {
    let windows = Layout::for_platform(Platform::Windows, &root()).expect("a short root fits");
    let without_root = |name: &str| (name != "SystemRoot").then(|| machine(name)).flatten();
    assert_eq!(
        windows.env(&without_root),
        Err(LayoutRefusal::MissingMachineVar("SystemRoot"))
    );
    let empty_drive = |name: &str| match name {
        "SystemDrive" => Some(OsString::new()),
        _ => machine(name),
    };
    assert_eq!(
        windows.env(&empty_drive),
        Err(LayoutRefusal::MissingMachineVar("SystemDrive"))
    );
}

/// L4: the live environment adds to [`Layout::env`]'s, fed from this process, only the verify
/// run's and the trust check's marks.
#[cfg(not(target_family = "wasm"))]
#[test]
fn the_live_environment_adds_only_the_runs_marks() {
    let host = Layout::under(&root()).expect("the host is a platform a world knows");
    let marks = [
        aterm_uds::process::VERIFY_RUN_VAR,
        aterm_uds::process::TRUST_CHECK_VAR,
    ];
    let mut want = host
        .env(&|name| std::env::var_os(name))
        .expect("this host's environment has its machine variables");
    want.extend(
        marks
            .into_iter()
            .filter_map(|mark| std::env::var_os(mark).map(|value| (mark, value))),
    );
    assert_eq!(host.live_env(), Ok(want));
}

/// A root of exactly `bytes` bytes.
fn root_of(bytes: usize) -> PathBuf {
    let base = root();
    let pad = bytes - base.as_os_str().len() - 1;
    base.join("r".repeat(pad))
}

/// L5: every path of a world is in it, and every socket fits. Its folders come each after its
/// parent and lie under its root; each file sits in one of them, and each path under the root its
/// environment names, but the marker's (aterm's state folder, which aterm makes), is one of its
/// folders or files. A root fits while its longest socket path, with its terminator, fits 104
/// bytes, and that path is a keeper's in the world's temporary folder; negative controls: one
/// byte longer, and an 80-byte root, are refused.
#[test]
fn every_path_of_a_world_is_in_it_and_every_socket_fits() {
    for on in Platform::ALL {
        let layout = Layout::for_platform(on, &root()).expect("a short root fits");
        let dirs = layout.dirs();
        let files: Vec<PathBuf> = layout
            .files(&Config::OFF)
            .expect("no head names a lane")
            .into_iter()
            .map(|(path, _)| path)
            .collect();
        let env = layout.env(&machine).expect("every machine variable is set");
        for (name, value) in &env {
            let path = PathBuf::from(value);
            if *name != MARKER && path.starts_with(&layout.root) {
                assert!(
                    dirs.contains(&path) || files.contains(&path),
                    "{on:?}: {name} names {}, which the world does not make",
                    path.display()
                );
            }
        }
        for (at, dir) in dirs.iter().enumerate() {
            let parent = dir.parent().expect("a folder of a world has a parent");
            assert!(
                parent == layout.root || dirs[..at].iter().any(|made| made == parent),
                "{on:?}: {} comes before its parent",
                dir.display()
            );
            assert!(
                dir.starts_with(&layout.root),
                "{on:?}: {} lies outside the world",
                dir.display()
            );
        }
        for path in &files {
            let folder = path.parent().map(Path::to_path_buf);
            assert!(
                folder.is_some_and(|folder| dirs.contains(&folder)),
                "{on:?}: {} is written in a folder the world does not make",
                path.display()
            );
        }
        let refused = (8..=104)
            .find(|&bytes| Layout::for_platform(on, &root_of(bytes)).is_err())
            .expect("a long root is refused");
        match Layout::for_platform(on, &root_of(refused)) {
            Err(LayoutRefusal::SocketTooLong(socket)) => {
                assert_eq!(
                    socket.as_os_str().len(),
                    104,
                    "{on:?}: the first root refused is the first whose longest socket path, \
                     with its terminator, does not fit 104 bytes: {}",
                    socket.display()
                );
                let tmp = root_of(refused).join("home").join("tmp");
                assert!(socket.starts_with(&tmp), "{on:?}: {}", socket.display());
            }
            other => panic!("{on:?}: a {refused}-byte root was not refused: {other:?}"),
        }
        let long = Layout::for_platform(on, &root_of(80));
        assert!(
            matches!(long, Err(LayoutRefusal::SocketTooLong(_))),
            "{on:?}: {long:?}"
        );
    }
}

/// A scratch parent short enough for a world's sockets: under `/tmp` on Unix, whose `TMPDIR` a
/// test runs with is too long.
fn scratch() -> aterm_tempfile::TempDir {
    let parent = if cfg!(unix) {
        PathBuf::from("/tmp")
    } else {
        std::env::temp_dir()
    };
    aterm_tempfile::Builder::new()
        .prefix("wl")
        .tempdir_in(parent)
        .expect("a scratch folder")
}

/// Every entry under `dir`, links not followed.
fn entries(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).expect("a folder of the world reads") {
        let path = entry.expect("an entry reads").path();
        let meta = std::fs::symlink_metadata(&path).expect("an entry's metadata reads");
        if meta.is_dir() {
            found.extend(entries(&path));
        }
        found.push(path);
    }
    found
}

/// L5: a world is made of exactly its folders and files. [`Layout::make`] on an absent root
/// makes the root, each of [`Layout::dirs`] and each of [`Layout::files`] with its text and
/// nothing else, folders owner-only and files owner-read-write; it refuses the root once it is
/// no longer empty, and a config that names a lane it does not drive makes nothing.
#[test]
fn a_world_is_made_of_exactly_its_folders_and_files() {
    let parent = scratch();
    let root = parent.path().join("w");
    let layout = Layout::under(&root).expect("a /tmp root fits");
    layout.make(&Config::OFF).expect("a world is made");
    let files = layout.files(&Config::OFF).expect("no head names a lane");
    let mut want: Vec<PathBuf> = layout.dirs();
    want.extend(files.iter().map(|(path, _)| path.clone()));
    want.sort();
    let mut found = entries(&root);
    found.sort();
    assert_eq!(found, want);
    for (path, text) in &files {
        assert_eq!(
            std::fs::read_to_string(path).ok().as_deref(),
            Some(text.as_str())
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |path: &Path| {
            std::fs::symlink_metadata(path)
                .expect("a made entry's metadata reads")
                .permissions()
                .mode()
                & 0o7777
        };
        assert_eq!(mode(&root), 0o700);
        for dir in layout.dirs() {
            assert_eq!(mode(&dir), 0o700, "{}", dir.display());
        }
        for (path, _) in &files {
            assert_eq!(mode(path), 0o600, "{}", path.display());
        }
    }
    let again = layout.make(&Config::OFF).expect_err("a world is made once");
    assert_eq!(again.kind(), std::io::ErrorKind::AlreadyExists);

    let refused = parent.path().join("v");
    let error = Layout::under(&refused)
        .expect("a /tmp root fits")
        .make(&Config::new("[swarm]\nenabled = true\n"))
        .expect_err("a head that names an undriven lane is refused");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert!(!refused.exists(), "a refused config makes nothing");
}

/// L5, negative control: a root that already holds an entry is refused before anything is made
/// in it. A root holding an unrelated file `x` is refused ([`std::io::ErrorKind::AlreadyExists`]),
/// and `x`, with its bytes, stays its only entry.
#[test]
fn a_world_is_made_only_in_an_empty_root() {
    let parent = scratch();
    let root = parent.path().join("w");
    std::fs::create_dir(&root).expect("the root is made");
    std::fs::write(root.join("x"), "theirs").expect("an unrelated entry is planted");
    let error = Layout::under(&root)
        .expect("a /tmp root fits")
        .make(&Config::OFF)
        .expect_err("a root that holds an entry is refused");
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(entries(&root), [root.join("x")]);
    assert_eq!(
        std::fs::read_to_string(root.join("x")).ok().as_deref(),
        Some("theirs")
    );
}

/// L5, negative controls: the world's two writes never go through an entry already there. A
/// folder ([`layout::make_dir`]) is not made where its parent is absent, which it does not make;
/// a file ([`layout::write_new`]) is not written where a file already is, which keeps its bytes.
#[test]
fn a_worlds_writes_never_replace_or_reach_past_an_entry() {
    let parent = scratch();
    let orphan = parent.path().join("a").join("b");
    let error = layout::make_dir(&orphan).expect_err("a folder whose parent is absent");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    assert!(!parent.path().join("a").exists(), "no parent is made");

    let file = parent.path().join("f");
    std::fs::write(&file, "theirs").expect("a file is planted");
    let error = layout::write_new(&file, "ours").expect_err("a file already there");
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(
        std::fs::read_to_string(&file).ok().as_deref(),
        Some("theirs")
    );
}

/// L5, negative controls: a link planted where a world goes is never written through. A root
/// whose HOME is a planted link is refused before anything is made in it, so the link stays its
/// only entry; a root that is a link itself is refused; a folder ([`layout::make_dir`]) is not
/// made, nor a file ([`layout::write_new`]) written, where a link is planted. In each case the
/// folder the link names stays empty.
#[cfg(unix)]
#[test]
fn a_world_is_never_made_through_a_planted_link() {
    use std::os::unix::fs::symlink;
    let parent = scratch();
    let target = parent.path().join("target");
    std::fs::create_dir(&target).expect("the link's target is made");
    let empty = |dir: &Path| {
        std::fs::read_dir(dir)
            .expect("the target reads")
            .next()
            .is_none()
    };

    let root = parent.path().join("w");
    std::fs::create_dir(&root).expect("the root is made");
    symlink(&target, root.join("home")).expect("a link is planted at HOME");
    let error = Layout::under(&root)
        .expect("a /tmp root fits")
        .make(&Config::OFF)
        .expect_err("a root that holds a link is refused");
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(entries(&root), [root.join("home")]);
    assert!(empty(&target));

    let linked_root = parent.path().join("l");
    symlink(&target, &linked_root).expect("a link is planted at the root");
    assert!(
        Layout::under(&linked_root)
            .expect("a /tmp root fits")
            .make(&Config::OFF)
            .is_err()
    );
    assert!(empty(&target));

    let folder = parent.path().join("d");
    symlink(&target, &folder).expect("a link is planted where a folder goes");
    assert!(layout::make_dir(&folder).is_err());
    assert!(empty(&target));

    let file = parent.path().join("f");
    symlink(target.join("f"), &file).expect("a link is planted where a file goes");
    assert!(layout::write_new(&file, "ours").is_err());
    assert!(empty(&target));
}
