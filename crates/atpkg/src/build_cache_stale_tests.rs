// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DAY: Duration = Duration::from_secs(24 * 60 * 60);
const HOUR: Duration = Duration::from_secs(60 * 60);
const MINUTE: Duration = Duration::from_secs(60);

/// The clock every case reads.
fn now() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_800_000_000)
}

/// An in-memory tree.
#[derive(Default)]
struct Fake {
    nodes: BTreeMap<PathBuf, (Kind, SystemTime, Vec<u8>, Option<PathBuf>)>,
    /// Paths whose metadata cannot be read (a listing shows them with none).
    unreadable: Vec<PathBuf>,
    /// Regular files whose contents cannot be read despite readable metadata.
    unreadable_heads: Vec<PathBuf>,
    /// Folders whose metadata reads but which cannot be listed.
    blind: Vec<PathBuf>,
    looked: std::cell::RefCell<Vec<PathBuf>>,
    devices: BTreeMap<PathBuf, Option<u64>>,
}

impl Fake {
    /// `path` (and every missing folder above it, written `at`) as `kind`.
    fn put(&mut self, path: &str, kind: Kind, at: SystemTime, bytes: &[u8]) -> &mut Self {
        let path = PathBuf::from(path);
        for parent in path.ancestors().skip(1) {
            self.nodes
                .entry(parent.to_path_buf())
                .or_insert((Kind::Dir, at, Vec::new(), None));
        }
        self.nodes.insert(path, (kind, at, bytes.to_vec(), None));
        self
    }

    fn dir(&mut self, path: &str, at: SystemTime) -> &mut Self {
        self.put(path, Kind::Dir, at, b"")
    }

    fn file(&mut self, path: &str, at: SystemTime, bytes: &[u8]) -> &mut Self {
        self.put(path, Kind::File, at, bytes)
    }

    fn link(&mut self, path: &str, at: SystemTime, to: &str) -> &mut Self {
        self.put(path, Kind::Link, at, b"");
        self.nodes.get_mut(Path::new(path)).unwrap().3 = Some(PathBuf::from(to));
        self
    }

    /// Writes `path` again at `at`: its own time only, as a write into a folder moves the folder's.
    fn touch(&mut self, path: &str, at: SystemTime) {
        self.nodes.get_mut(Path::new(path)).unwrap().1 = at;
    }
}

impl Fs for Fake {
    fn meta(&self, path: &Path) -> Option<Meta> {
        self.looked.borrow_mut().push(path.to_path_buf());
        if self.unreadable.iter().any(|p| p == path) {
            return None;
        }
        let (kind, modified, _, _) = self.nodes.get(path)?;
        Some(Meta {
            kind: *kind,
            modified: *modified,
            device: self.devices.get(path).copied().unwrap_or(Some(1)),
        })
    }

    fn list(&self, dir: &Path) -> Option<Vec<(OsString, Option<Meta>)>> {
        self.looked.borrow_mut().push(dir.to_path_buf());
        if self.meta(dir)?.kind != Kind::Dir || self.blind.iter().any(|p| p == dir) {
            return None;
        }
        Some(
            self.nodes
                .keys()
                .filter(|p| p.parent() == Some(dir))
                .map(|p| (p.file_name().unwrap().to_owned(), self.meta(p)))
                .collect(),
        )
    }

    fn head(&self, file: &Path, max: usize) -> Option<Vec<u8>> {
        self.looked.borrow_mut().push(file.to_path_buf());
        if self.unreadable_heads.iter().any(|path| path == file) {
            return None;
        }
        let (kind, _, bytes, _) = self.nodes.get(file)?;
        (*kind == Kind::File).then(|| bytes.iter().take(max).copied().collect())
    }

    fn read_link(&self, link: &Path) -> Option<PathBuf> {
        self.looked.borrow_mut().push(link.to_path_buf());
        self.nodes.get(link)?.3.clone()
    }
}

const LEAF: &str = "/c/Users//me/ws/.cargo-build";
const PROFILE: &str = "/c/Users//me/ws/.cargo-build/debug";
const OLD: &str = "1111111111111111";
const NEW: &str = "2222222222222222";
const FRESH: &str = "3333333333333333";

/// The store [`world`] lays its compilers in, absolute on this platform: a sysroot is read only
/// when it is absolute, and Windows reads a path with no drive as relative. Cases that spell
/// `/store/trust/…` themselves stay Unix only.
const STORE: &str = if cfg!(windows) { "C:/store" } else { "/store" };

/// `.rustc_info.json` as cargo keeps it.
fn rustc_info(sysroots: &[&str]) -> Vec<u8> {
    let mut outputs = String::from(
        r#""1":{"success":true,"status":"","code":0,"stdout":"rustc 1.99.0-dev\nbinary: trustc\nrelease: 1.99.0-dev\n","stderr":""}"#,
    );
    for (i, root) in sysroots.iter().enumerate() {
        outputs.push_str(&format!(
            r#","{}":{{"success":true,"status":"","code":0,"stdout":"___\nlib___.rlib\n{root}\noff\ndebug_assertions\ntarget_os=\"macos\"\n","stderr":""}}"#,
            i + 2
        ));
    }
    format!(r#"{{"rustc_fingerprint":7,"outputs":{{{outputs}}},"successes":{{}}}}"#).into_bytes()
}

fn fingerprint(rustc: u64) -> Vec<u8> {
    format!(r#"{{"rustc":{rustc},"features":"[]","target":1,"profile":2,"path":3,"deps":[]}}"#)
        .into_bytes()
}

/// Metadata only: the action hash differs from its compiler key, and digest
/// strings name no file the survey may read.
fn output_metadata(schema: u64, rustc: u64) -> aterm_json::Value {
    let mut value = aterm_json::json!({
        "schema": schema,
        "fingerprint": 91,
        "outputs": [{
            "path_sha256": "a".repeat(64),
            "size": 17,
            "sha256": "b".repeat(64)
        }]
    });
    if schema == 2 || schema == 3 {
        value.as_object_mut().unwrap().insert(
            "compiler".into(),
            aterm_json::json!({
                "rustc_key": rustc,
                "content_blake3": vec![7u8; 32],
                "workspace": true,
                "mode": "build",
                "build_script": false
            }),
        );
    }
    if schema == 3 {
        value.as_object_mut().unwrap().insert(
            "compiler_outputs".into(),
            aterm_json::json!([{"path_sha256": "a".repeat(64), "emit": "link"}]),
        );
    }
    value
}

/// A build directory the store's current compiler last built.
fn world(moved: SystemTime, old_age: Duration) -> Fake {
    let then = now() - old_age;
    let mut fs = Fake::default();
    fs.dir(&format!("{STORE}/trust/9192"), now() - 60 * DAY)
        .dir(&format!("{STORE}/trust/15698"), moved)
        .link(&format!("{STORE}/trust/current"), moved, "15698")
        .file(
            &format!("{LEAF}/.rustc_info.json"),
            moved,
            &rustc_info(&[&format!("{STORE}/trust/15698")]),
        )
        .file(
            &format!("{PROFILE}/.cargo-build-lock"),
            now() - 90 * DAY,
            b"",
        );
    for (hash, rustc, at) in [(OLD, 1, then), (NEW, 2, moved + DAY), (FRESH, 2, now())] {
        let fp = format!("{PROFILE}/.fingerprint/aterm-grid-{hash}");
        fs.dir(&fp, at)
            .file(
                &format!("{fp}/lib-aterm_grid.json"),
                at,
                &fingerprint(rustc),
            )
            .file(&format!("{fp}/invoked.timestamp"), at, b"")
            .file(
                &format!("{PROFILE}/deps/libaterm_grid-{hash}.rlib"),
                at,
                b"r",
            )
            .file(&format!("{PROFILE}/deps/aterm_grid-{hash}.d"), at, b"d");
    }
    fs.file(
        &format!("{PROFILE}/deps/aterm_grid-{OLD}.aterm_grid.1a2b3c-cgu.0.rcgu.o"),
        then,
        b"o",
    )
    .dir(&format!("{PROFILE}/build/aterm-grid-{OLD}"), then)
    .file(
        &format!("{PROFILE}/build/aterm-grid-{OLD}/output"),
        then,
        b"",
    )
    .file(&format!("{PROFILE}/examples/demo-{OLD}"), then, b"x")
    .file(&format!("{PROFILE}/deps/libaterm_gpu_web.rlib"), then, b"w")
    .file(&format!("{PROFILE}/deps/0b6650033266f1a6.twit"), then, b"t");
    for (folder, at) in [
        ("aterm_grid-1k116trr0a0xf", then),
        ("aterm_grid-3g63pv341xugh", now()),
    ] {
        let crate_dir = format!("{PROFILE}/incremental/{folder}");
        fs.dir(&crate_dir, at)
            .dir(
                &format!("{crate_dir}/s-hneouhc0uf-1tn8o9j-2awpw07x6x4g1"),
                at,
            )
            .file(&format!("{crate_dir}/s-hneouhc0uf-1tn8o9j.lock"), at, b"");
    }
    fs
}

fn surveyed(fs: &Fake) -> Survey {
    survey(
        fs,
        Path::new(LEAF),
        &[PathBuf::from(PROFILE)],
        Some(Path::new(STORE)),
    )
}

fn paths(doomed: &[Doomed]) -> Vec<(Part, Vec<String>)> {
    doomed
        .iter()
        .map(|d| {
            let mut p: Vec<String> = d
                .paths
                .iter()
                .map(|p| {
                    let rel = p.strip_prefix(PROFILE).unwrap().components();
                    let parts: Vec<_> = rel.map(|c| c.as_os_str().to_string_lossy()).collect();
                    parts.join("/")
                })
                .collect();
            p.sort();
            (d.part, p)
        })
        .collect()
}

#[cfg(unix)]
#[test]
fn observations_separate_quiet_units_from_missing_compiler_evidence_without_more_reads() {
    let fs = world(now() - 2 * DAY, 4 * DAY);
    let mut s = surveyed(&fs);
    s.profiles[0].crates.clear();
    s.profiles[0].units.retain(|u| u.hash == NEW);
    fs.looked.borrow_mut().clear();
    let before = decide(&s, now(), RECLAIM_STALE_UNDER_PRESSURE);
    assert!(
        before.is_empty(),
        "a quiet unit built by the current compiler stays"
    );
    let seen = observations(&s, now(), RECLAIM_STALE_UNDER_PRESSURE);
    assert_eq!(
        seen.units,
        Ages {
            quiet: 1,
            not_quiet: 0,
            unknown: 0
        }
    );
    assert_eq!(seen.last_build_current, Some(true));
    assert_eq!(seen.profiles_with_compiler_references, 1);
    assert_eq!(seen.units_without_compiler, 0);
    assert!(!seen.used_record);

    let mut missing = s.clone();
    missing.last_built = None;
    let unavailable = observations(&missing, now(), RECLAIM_STALE_UNDER_PRESSURE);
    assert_eq!(unavailable.units, seen.units);
    assert_eq!(unavailable.last_build_current, None);
    assert_eq!(unavailable.profiles_with_compiler_references, 0);
    assert!(decide(&missing, now(), RECLAIM_STALE_UNDER_PRESSURE).is_empty());

    let mut no_references = s.clone();
    no_references.last_built.as_mut().unwrap().since = now() + DAY;
    let absent = observations(&no_references, now(), RECLAIM_STALE_UNDER_PRESSURE);
    assert_eq!(absent.last_build_current, Some(true));
    assert_eq!(absent.profiles_with_compiler_references, 0);
    assert!(decide(&no_references, now(), RECLAIM_STALE_UNDER_PRESSURE).is_empty());

    no_references.profiles[0].units[0].rustc = None;
    assert_eq!(
        observations(&no_references, now(), RECLAIM_STALE_UNDER_PRESSURE).units_without_compiler,
        1
    );
    let mut future = s.clone();
    let unit = &mut future.profiles[0].units[0];
    unit.fingerprint.newest = Some(now() + DAY);
    for artifact in &mut unit.artifacts {
        artifact.newest = Some(now() + DAY);
    }
    assert_eq!(
        observations(&future, now(), RECLAIM_STALE_UNDER_PRESSURE).units,
        Ages {
            quiet: 0,
            not_quiet: 1,
            unknown: 0
        }
    );
    future.profiles[0].units[0].fingerprint.newest = None;
    assert_eq!(
        observations(&future, now(), RECLAIM_STALE_UNDER_PRESSURE).units,
        Ages {
            quiet: 0,
            not_quiet: 0,
            unknown: 1
        }
    );
    assert!(decide(&future, now(), RECLAIM_STALE_UNDER_PRESSURE).is_empty());
    assert_eq!(decide(&s, now(), RECLAIM_STALE_UNDER_PRESSURE), before);
    assert!(
        fs.looked.borrow().is_empty(),
        "explanation performs no filesystem reads"
    );
}

#[test]
fn observations_distinguish_used_records_and_do_not_count_omitted_layouts() {
    let mut fs = release_clone();
    let s = clone_survey(&fs);
    let seen = observations(&s, now(), RECLAIM_STALE);
    assert!(seen.used_record);
    assert_eq!(seen.profiles_naming_used_units, 1);
    assert_eq!(
        seen.units.quiet + seen.units.not_quiet + seen.units.unknown,
        s.profiles[0].units.len()
    );
    fs.file(&format!("{LEAF}/{USED}"), now(), b"malformed record");
    let missing = observations(&clone_survey(&fs), now(), RECLAIM_STALE);
    assert!(!missing.used_record);
    assert_eq!(missing.profiles_naming_used_units, 0);
    fs.blind.push(PathBuf::from(format!("{PROFILE}/build")));
    let omitted = observations(&clone_survey(&fs), now(), RECLAIM_STALE);
    assert_eq!(omitted.units, Ages::default());
    assert!(
        fs.nodes
            .keys()
            .any(|p| p.starts_with(format!("{PROFILE}/build"))),
        "zero recognized units is not an empty cache"
    );
}

#[test]
fn a_stale_crate_folder_and_an_old_compilers_unit_go_with_their_entries() {
    let fs = world(now() - 2 * DAY, 4 * DAY);
    let s = surveyed(&fs);
    assert_eq!(
        s.last_built,
        Some(LastBuilt {
            current: true,
            since: now() - 2 * DAY
        })
    );
    assert_eq!(
        paths(&decide(&s, now(), RECLAIM_STALE)),
        vec![
            (
                Part::Incremental,
                vec!["incremental/aterm_grid-1k116trr0a0xf".to_owned()]
            ),
            (
                Part::Unit,
                vec![
                    format!(".fingerprint/aterm-grid-{OLD}"),
                    format!("build/aterm-grid-{OLD}"),
                    format!("deps/aterm_grid-{OLD}.aterm_grid.1a2b3c-cgu.0.rcgu.o"),
                    format!("deps/aterm_grid-{OLD}.d"),
                    format!("deps/libaterm_grid-{OLD}.rlib"),
                    format!("examples/demo-{OLD}"),
                ]
            ),
        ]
    );
}

/// A SURVEY SAYS IT IS STILL GOING: it stats each unit of a profile and reads each fingerprint
/// folder's JSON, and a profile holds thousands of each.
#[test]
fn a_survey_ticks_for_each_stamp_and_fingerprint_it_reads() {
    let fs = world(now() - DAY, 4 * DAY);
    let ticks = aterm_uds::progress::ticks_here;
    let before = ticks();
    stamp(&fs, Path::new(LEAF));
    assert_eq!(ticks() - before, 1, "a stamp");
    let folder = format!("{PROFILE}/.fingerprint/aterm-grid-{OLD}");
    let before = ticks();
    assert!(compiler(&fs, Path::new(&folder)).is_some());
    assert_eq!(ticks() - before, 1, "a fingerprint folder");
}

#[test]
fn native_output_metadata_preserves_named_layout_compiler_evidence() {
    let folder = format!("{PROFILE}/.fingerprint/aterm-grid-{OLD}");
    for schema in [1, 2, 3] {
        let mut fs = world(now() - 2 * DAY, 4 * DAY);
        fs.file(
            &format!("{folder}/outputs-lib-aterm_grid.json"),
            now() - 4 * DAY,
            &aterm_json::to_vec(&output_metadata(schema, 1)).unwrap(),
        );
        assert_eq!(compiler(&fs, Path::new(&folder)), Some(1));
        let survey = surveyed(&fs);
        assert_eq!(survey.profiles[0].units[0].rustc, Some(1));
        assert!(
            decide(&survey, now(), RECLAIM_STALE)
                .iter()
                .any(|doomed| doomed.part == Part::Unit)
        );

        // Recognition repairs only the compiler-record classification. It
        // does not admit a private SDK as an installed store generation.
        fs.file(
            &format!("{LEAF}/.rustc_info.json"),
            now(),
            &rustc_info(&["/private/sdk"]),
        );
        let private = surveyed(&fs);
        assert_eq!(private.last_built, None);
        assert!(
            decide(&private, now(), RECLAIM_STALE)
                .iter()
                .all(|doomed| doomed.part != Part::Unit)
        );
    }
}

#[test]
fn native_output_metadata_preserves_folder_layout_compiler_evidence() {
    for schema in [1, 2, 3] {
        for mode in ["build", "check", "check-test", "test"] {
            let mut fs = Fake::default();
            folder_unit(
                &mut fs,
                "aterm-grid",
                OLD,
                "test-lib-aterm_grid",
                7,
                &["libaterm_grid-{h}.rmeta"],
                now() - 4 * DAY,
            );
            let mut metadata = output_metadata(schema, 7);
            if schema == 2 || schema == 3 {
                metadata
                    .as_object_mut()
                    .unwrap()
                    .get_mut("compiler")
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .insert("mode".into(), mode.into());
            }
            let folder = format!("{PROFILE}/build/aterm-grid/{OLD}/fingerprint");
            fs.file(
                &format!("{folder}/outputs-test-lib-aterm_grid.json"),
                now() - 4 * DAY,
                &aterm_json::to_vec(&metadata).unwrap(),
            );
            assert_eq!(compiler(&fs, Path::new(&folder)), Some(7));
            let units = folder_units(&fs, Path::new(PROFILE));
            assert_eq!(units.len(), 1);
            assert_eq!(units[0].rustc, Some(7));
        }
    }
}

#[test]
fn output_metadata_does_not_hide_bad_or_conflicting_compiler_records() {
    let folder = format!("{PROFILE}/.fingerprint/aterm-grid-{OLD}");
    let actual = format!("{folder}/lib-aterm_grid.json");
    let other = format!("{folder}/test-lib-aterm_grid.json");
    for schema in [1, 2, 3] {
        for fault in 0..4 {
            let mut fs = world(now() - 2 * DAY, 4 * DAY);
            fs.file(
                &format!("{folder}/outputs-lib-aterm_grid.json"),
                now(),
                &aterm_json::to_vec(&output_metadata(schema, 1)).unwrap(),
            );
            match fault {
                0 => {
                    fs.file(&other, now(), &fingerprint(2));
                }
                1 => {
                    fs.file(&actual, now(), b"{\"features\":\"[]\"}");
                }
                2 => fs.unreadable_heads.push(actual.clone().into()),
                _ => fs.unreadable.push(actual.clone().into()),
            }
            assert_eq!(compiler(&fs, Path::new(&folder)), None, "{schema}/{fault}");
        }
    }
}

#[test]
fn output_metadata_requires_an_exact_readable_regular_companion_and_receipt() {
    let folder = format!("{PROFILE}/.fingerprint/aterm-grid-{OLD}");
    let receipt = format!("{folder}/outputs-lib-aterm_grid.json");
    let bytes = aterm_json::to_vec(&output_metadata(1, 1)).unwrap();
    for fault in 0..8 {
        let mut fs = world(now() - 2 * DAY, 4 * DAY);
        fs.file(&receipt, now(), &bytes);
        match fault {
            0 => {
                fs.nodes
                    .remove(Path::new(&format!("{folder}/lib-aterm_grid.json")));
            }
            1 => {
                fs.file(&format!("{folder}/unknown.json"), now(), &bytes);
            }
            2 => {
                fs.file(&format!("{folder}/outputs-bin-orphan.json"), now(), &bytes);
            }
            3 => fs.unreadable_heads.push(receipt.clone().into()),
            4 => fs.unreadable.push(receipt.clone().into()),
            5 => {
                fs.link(&receipt, now(), "/outside/metadata.json");
            }
            6 => {
                fs.dir(&receipt, now());
            }
            _ => {
                fs.put(&receipt, Kind::Other, now(), b"");
            }
        }
        assert_eq!(compiler(&fs, Path::new(&folder)), None, "{fault}");
        assert!(
            !fs.looked
                .borrow()
                .contains(&PathBuf::from("/outside/metadata.json"))
        );
    }
}

#[test]
fn malformed_or_unsupported_native_output_metadata_keeps_compiler_unknown() {
    let mut bad = vec![b"{".to_vec()];
    let duplicate = String::from_utf8(aterm_json::to_vec(&output_metadata(1, 1)).unwrap())
        .unwrap()
        .replace("\"schema\":1", "\"schema\":1,\"schema\":1");
    bad.push(duplicate.into_bytes());
    for schema in [0, 4] {
        bad.push(aterm_json::to_vec(&output_metadata(schema, 1)).unwrap());
    }
    for schema in [2, 3] {
        for fault in 0..10 {
            let mut value = output_metadata(schema, 1);
            match fault {
                0 => {
                    value.as_object_mut().unwrap().remove("compiler");
                }
                1 => {
                    value
                        .as_object_mut()
                        .unwrap()
                        .insert("extra".into(), true.into());
                }
                2 => {
                    value
                        .as_object_mut()
                        .unwrap()
                        .insert("outputs".into(), aterm_json::json!([]));
                }
                3 => {
                    value.as_object_mut().unwrap().insert(
                    "outputs".into(),
                    aterm_json::json!([{"path_sha256":"bad", "size":0, "sha256":"b".repeat(64)}]),
                );
                }
                4 => {
                    value.as_object_mut().unwrap().insert(
                    "outputs".into(),
                    aterm_json::json!([{"path_sha256":"a".repeat(64), "size":8_589_934_593u64, "sha256":"b".repeat(64)}]),
                );
                }
                _ => {
                    let compiler = value
                        .as_object_mut()
                        .unwrap()
                        .get_mut("compiler")
                        .unwrap()
                        .as_object_mut()
                        .unwrap();
                    match fault {
                        5 => {
                            compiler.insert("rustc_key".into(), 2u64.into());
                        }
                        6 => {
                            compiler.insert("mode".into(), "doc".into());
                        }
                        7 => {
                            compiler
                                .insert("content_blake3".into(), aterm_json::json!(vec![7u8; 31]));
                        }
                        8 => {
                            compiler.insert(
                                "content_blake3".into(),
                                aterm_json::json!(vec![256u64; 32]),
                            );
                        }
                        _ => {
                            compiler.remove("workspace");
                        }
                    }
                }
            }
            bad.push(aterm_json::to_vec(&value).unwrap());
        }
    }
    let mut trailing = aterm_json::to_vec(&output_metadata(1, 1)).unwrap();
    trailing.extend_from_slice(b"false");
    bad.push(trailing);
    let folder = format!("{PROFILE}/.fingerprint/aterm-grid-{OLD}");
    for (index, bytes) in bad.iter().enumerate() {
        let mut fs = world(now() - 2 * DAY, 4 * DAY);
        fs.file(
            &format!("{folder}/outputs-lib-aterm_grid.json"),
            now(),
            bytes,
        );
        assert_eq!(compiler(&fs, Path::new(&folder)), None, "{index}");
    }
}

#[test]
fn legacy_metadata_cannot_acquire_compiler_coverage_fields() {
    for schema in [1, 2] {
        let legacy = output_metadata(schema, 1);
        assert!(native_output_metadata(
            &aterm_json::to_vec(&legacy).unwrap(),
            1
        ));
        let mut mislabeled = output_metadata(3, 1);
        mislabeled
            .as_object_mut()
            .unwrap()
            .insert("schema".into(), schema.into());
        assert!(!native_output_metadata(
            &aterm_json::to_vec(&mislabeled).unwrap(),
            1
        ));
    }
}

#[test]
fn native_schema_three_coverage_preserves_auxiliary_outputs_without_path_reads() {
    let mut value = output_metadata(3, 1);
    value
        .as_object_mut()
        .unwrap()
        .get_mut("outputs")
        .unwrap()
        .as_array_mut()
        .unwrap()
        .extend([
            aterm_json::json!({"path_sha256": "c".repeat(64), "size": 11, "sha256": "d".repeat(64)}),
            aterm_json::json!({"path_sha256": "e".repeat(64), "size": 13, "sha256": "f".repeat(64)}),
        ]);
    value.as_object_mut().unwrap().insert(
        "compiler_outputs".into(),
        aterm_json::json!([
            {"path_sha256": "a".repeat(64), "emit": "link"},
            {"path_sha256": "c".repeat(64), "emit": "metadata"}
        ]),
    );
    let folder = format!("{PROFILE}/.fingerprint/aterm-grid-{OLD}");
    let mut fs = world(now() - 2 * DAY, 4 * DAY);
    fs.file(
        &format!("{folder}/outputs-lib-aterm_grid.json"),
        now(),
        &aterm_json::to_vec(&value).unwrap(),
    );
    assert_eq!(compiler(&fs, Path::new(&folder)), Some(1));
    assert!(
        fs.looked
            .borrow()
            .iter()
            .all(|path| path.starts_with(&folder))
    );
}

#[test]
fn malformed_schema_three_compiler_coverage_keeps_compiler_unknown() {
    let mut missing = output_metadata(3, 1);
    missing.as_object_mut().unwrap().remove("compiler_outputs");
    let mut bad = vec![aterm_json::to_vec(&missing).unwrap()];
    for coverage in [
        aterm_json::json!(null),
        aterm_json::json!([]),
        aterm_json::json!({}),
        aterm_json::json!([{}]),
        aterm_json::json!([{"path_sha256": "a".repeat(64)}]),
        aterm_json::json!([{"emit": "link"}]),
        aterm_json::json!([{"path_sha256": "a".repeat(64), "emit": "link", "extra": true}]),
        aterm_json::json!([{"path_sha256": "a".repeat(64), "emit": "Link"}]),
        aterm_json::json!([{"path_sha256": "a".repeat(64), "emit": "rmeta"}]),
        aterm_json::json!([{"path_sha256": "a".repeat(64), "emit": null}]),
        aterm_json::json!([{"path_sha256": "a".repeat(64), "emit": 1}]),
        aterm_json::json!([{"path_sha256": "bad", "emit": "link"}]),
        aterm_json::json!([{"path_sha256": "A".repeat(64), "emit": "link"}]),
        aterm_json::json!([{"path_sha256": "g".repeat(64), "emit": "link"}]),
        aterm_json::json!([{"path_sha256": "c".repeat(64), "emit": "metadata"}]),
        aterm_json::json!([
            {"path_sha256": "a".repeat(64), "emit": "link"},
            {"path_sha256": "a".repeat(64), "emit": "metadata"}
        ]),
    ] {
        let mut value = output_metadata(3, 1);
        value
            .as_object_mut()
            .unwrap()
            .insert("compiler_outputs".into(), coverage);
        bad.push(aterm_json::to_vec(&value).unwrap());
    }
    let valid = String::from_utf8(aterm_json::to_vec(&output_metadata(3, 1)).unwrap()).unwrap();
    bad.push(
        valid
            .replacen(
                "\"compiler_outputs\":",
                "\"compiler_outputs\":[],\"compiler_outputs\":",
                1,
            )
            .into_bytes(),
    );
    bad.push(
        valid
            .replace(
                "\"emit\":\"link\"",
                "\"emit\":\"link\",\"emit\":\"metadata\"",
            )
            .into_bytes(),
    );
    let folder = format!("{PROFILE}/.fingerprint/aterm-grid-{OLD}");
    for (index, bytes) in bad.iter().enumerate() {
        assert!(!native_output_metadata(bytes, 1), "{index}");
        let mut fs = world(now() - 2 * DAY, 4 * DAY);
        fs.file(
            &format!("{folder}/outputs-lib-aterm_grid.json"),
            now(),
            bytes,
        );
        assert_eq!(compiler(&fs, Path::new(&folder)), None, "{index}");
        assert!(
            fs.looked
                .borrow()
                .iter()
                .all(|path| path.starts_with(&folder))
        );
    }
}

#[test]
fn native_schema_three_coverage_has_a_bounded_nonempty_inventory() {
    let mut value = output_metadata(3, 1);
    let outputs: Vec<_> = (0..4096)
        .map(|index| {
            aterm_json::json!({
                "path_sha256": format!("{index:064x}"),
                "size": 1,
                "sha256": "b".repeat(64)
            })
        })
        .collect();
    let coverage: Vec<_> = (0..4096)
        .map(
            |index| aterm_json::json!({"path_sha256": format!("{index:064x}"), "emit": "metadata"}),
        )
        .collect();
    let fields = value.as_object_mut().unwrap();
    fields.insert("outputs".into(), outputs.into());
    fields.insert("compiler_outputs".into(), coverage.into());
    // The pure shape recognizer admits the producer's upper bound. The survey
    // still has its existing smaller directory byte budget, checked separately.
    assert!(native_output_metadata(
        &aterm_json::to_vec(&value).unwrap(),
        1
    ));
    value
        .as_object_mut()
        .unwrap()
        .get_mut("compiler_outputs")
        .unwrap()
        .as_array_mut()
        .unwrap()
        .push(aterm_json::json!({"path_sha256": format!("{:064x}", 4096), "emit": "link"}));
    assert!(!native_output_metadata(
        &aterm_json::to_vec(&value).unwrap(),
        1
    ));
}

#[test]
fn native_output_metadata_reads_have_one_conservative_directory_budget() {
    for schema in [1, 2, 3] {
        let folder = format!("{PROFILE}/.fingerprint/aterm-grid-{OLD}");
        let receipt = format!("{folder}/outputs-lib-aterm_grid.json");
        let mut bytes = aterm_json::to_vec(&output_metadata(schema, 1)).unwrap();
        bytes.resize(OUTPUT_METADATA_BUDGET - 1, b' ');
        let mut fs = world(now() - 2 * DAY, 4 * DAY);
        fs.file(&receipt, now(), &bytes);
        assert_eq!(compiler(&fs, Path::new(&folder)), Some(1));
        bytes.push(b' ');
        fs.file(&receipt, now(), &bytes);
        assert_eq!(compiler(&fs, Path::new(&folder)), None);
        bytes.push(b' ');
        fs.file(&receipt, now(), &bytes);
        assert_eq!(compiler(&fs, Path::new(&folder)), None);

        bytes.truncate(OUTPUT_METADATA_BUDGET / 2 + 1);
        fs.file(&receipt, now(), &bytes);
        fs.file(
            &format!("{folder}/test-lib-aterm_grid.json"),
            now(),
            &fingerprint(1),
        );
        fs.file(
            &format!("{folder}/outputs-test-lib-aterm_grid.json"),
            now(),
            &bytes,
        );
        assert_eq!(
            compiler(&fs, Path::new(&folder)),
            None,
            "budget is shared across receipts"
        );
    }
}

/// THE WINDOW IS THE PASS'S: parts written four hours ago stay for a pass with room and go for
/// a pass below the floor, the unit by the same window as the folder. Parts written 59 minutes
/// ago stay for both, as does the folder written just now.
#[test]
fn below_the_floor_the_same_parts_go_after_an_hour() {
    const HOUR: Duration = Duration::from_secs(60 * 60);
    let hours = surveyed(&world(now() - 3 * HOUR, 4 * HOUR));
    assert_eq!(decide(&hours, now(), RECLAIM_STALE), Vec::new());
    let parts: Vec<Part> = decide(&hours, now(), RECLAIM_STALE_UNDER_PRESSURE)
        .iter()
        .map(|doomed| doomed.part)
        .collect();
    assert_eq!(parts, vec![Part::Incremental, Part::Unit]);

    let minutes = surveyed(&world(now() - 3 * HOUR, Duration::from_secs(59 * 60)));
    assert_eq!(
        decide(&minutes, now(), RECLAIM_STALE_UNDER_PRESSURE),
        Vec::new()
    );
}

#[test]
fn nothing_goes_while_any_of_it_was_written_within_the_window() {
    let young = world(now() - DAY, 2 * DAY);
    assert_eq!(decide(&surveyed(&young), now(), RECLAIM_STALE), Vec::new());

    let mut touched = world(now() - 2 * DAY, 4 * DAY);
    touched.touch(
        &format!("{PROFILE}/deps/aterm_grid-{OLD}.aterm_grid.1a2b3c-cgu.0.rcgu.o"),
        now(),
    );
    assert_eq!(
        paths(&decide(&surveyed(&touched), now(), RECLAIM_STALE))
            .into_iter()
            .map(|(part, _)| part)
            .collect::<Vec<_>>(),
        vec![Part::Incremental]
    );

    for second in [fingerprint(2), b"{\"features\":\"[]\"}".to_vec()] {
        let mut unread = world(now() - 2 * DAY, 4 * DAY);
        unread.file(
            &format!("{PROFILE}/.fingerprint/aterm-grid-{OLD}/test-lib-aterm_grid.json"),
            now() - 4 * DAY,
            &second,
        );
        let s = surveyed(&unread);
        assert_eq!(s.profiles[0].units[0].rustc, None);
        assert!(
            decide(&s, now(), RECLAIM_STALE)
                .iter()
                .all(|d| d.part == Part::Incremental)
        );
    }
}

/// A compiler laid aside reports its sysroot canonical, `<build>.noindex`, while `current`
/// names `<build>`, the link to it ([`aterm_toolchain::layout`]): the same build either way.
#[test]
#[cfg(unix)]
fn a_sysroot_laid_aside_is_the_build_its_link_names() {
    let info = format!("{LEAF}/.rustc_info.json");
    let mut fs = world(now() - 2 * DAY, 4 * DAY);
    fs.dir("/store/trust/15698.noindex", now() - 2 * DAY)
        .link("/store/trust/15698", now() - 2 * DAY, "15698.noindex")
        .file(&info, now(), &rustc_info(&["/store/trust/15698.noindex"]));
    assert_eq!(
        surveyed(&fs).last_built,
        Some(LastBuilt {
            current: true,
            since: now() - 2 * DAY
        })
    );
    fs.link("/store/trust/current", now() - DAY, "9192");
    assert_eq!(
        surveyed(&fs).last_built,
        Some(LastBuilt {
            current: false,
            since: now() - DAY
        }),
        "the previous compiler, laid aside, is still named as the previous one"
    );
}

/// A failed probe's stdout is cached too, but is never compiler-generation evidence.
#[cfg(unix)]
#[test]
fn a_failed_probe_cannot_classify_recognized_units_as_old_compiler_outputs() {
    let mut fs = world(now() - 2 * DAY, 4 * DAY);
    fs.file(
        &format!("{LEAF}/.rustc_info.json"),
        now(),
        br#"{"outputs":{"failed":{"success":false,"status":"exit status: 101","code":101,"stdout":"/store/trust/9192\n","stderr":"probe failed"}}}"#,
    );
    fs.looked.borrow_mut().clear();
    let s = surveyed(&fs);
    assert_eq!(
        s.last_built, None,
        "failed stdout cannot name a previous compiler"
    );
    assert_eq!(
        decide(&s, now(), RECLAIM_STALE)
            .iter()
            .map(|d| d.part)
            .collect::<Vec<_>>(),
        vec![Part::Incremental]
    );
    assert!(
        fs.looked.borrow().iter().all(|p| p.starts_with(LEAF)),
        "a failed probe cannot authorize external store reads"
    );
}

#[cfg(unix)]
#[test]
fn only_successful_typed_probe_records_contribute_sysroots_without_hiding_ambiguity() {
    let record = |success: &str, code: &str, status: &str, stdout: &str, stderr: &str| {
        format!(
            r#"{{"success":{success},"code":{code},"status":{status},"stdout":{stdout},"stderr":{stderr}}}"#
        )
    };
    let empty = r#""""#;
    let current = r#""/store/trust/15698\n""#;
    let former = r#""/store/trust/9192\n""#;
    let good = record("true", "0", empty, current, r#""a warning""#);
    let info = |outputs: &str| format!(r#"{{"outputs":{{{outputs}}}}}"#).into_bytes();
    let expected = Some(PathBuf::from("/store/trust/15698"));
    assert_eq!(sysroot_named(&info(&format!(r#""good":{good}"#))), expected);
    for bad in [
        record("false", "101", r#""exit status: 101""#, former, empty),
        record("false", "0", empty, former, empty),
        record(r#""true""#, "0", empty, former, empty),
        record("null", "0", empty, former, empty),
        record("true", "101", empty, former, empty),
        record("true", "null", empty, former, empty),
        record("true", "0.0", empty, former, empty),
        record("true", r#""0""#, empty, former, empty),
        record("true", "0", r#""exit status: 101""#, former, empty),
        record("true", "0", "null", former, empty),
        record("true", "0", empty, "null", empty),
        record("true", "0", empty, former, "null"),
        format!(r#"{{"stdout":{former}}}"#),
        "17".into(),
    ] {
        assert_eq!(
            sysroot_named(&info(&format!(r#""bad":{bad}"#))),
            None,
            "{bad}"
        );
        assert_eq!(
            sysroot_named(&info(&format!(r#""good":{good},"bad":{bad}"#))),
            expected,
            "an ineligible record cannot obscure a successful probe: {bad}"
        );
    }
    let other = record("true", "0", empty, former, empty);
    assert_eq!(
        sysroot_named(&info(&format!(r#""good":{good},"other":{other}"#))),
        None,
        "distinct eligible sysroots remain ambiguous"
    );
    assert_eq!(
        sysroot_named(&info(&format!(r#""good":{good},"same":{good}"#))),
        expected,
        "several successful probes may name the same sysroot"
    );
}

/// Unix only: the fixture's sysroot (`/store/trust/…`) is read only when it is
/// absolute, which a path with no drive is not on Windows.
#[test]
#[cfg(unix)]
fn unix_ambiguous_probe_paths_never_normalize_into_compiler_evidence() {
    let record = |stdout: &str, success: bool| {
        aterm_json::json!({
            "success": success,
            "code": if success { 0 } else { 101 },
            "status": if success { "" } else { "exit status: 101" },
            "stdout": stdout,
            "stderr": ""
        })
    };
    let info = |outputs| aterm_json::to_vec(&aterm_json::json!({"outputs": outputs})).unwrap();
    let clean = record("/store/trust/15698\n", true);
    let expected = Some(PathBuf::from("/store/trust/15698"));
    assert_eq!(
        sysroot_named(&rustc_info(&["/store/trust/15698"])),
        expected
    );
    for control in ['\r', '\t', '\0', '\u{b}', '\u{c}', '\u{7f}'] {
        let stdout = format!("/store/trust/9192{control}\n");
        let ambiguous = record(&stdout, true);
        for outputs in [
            aterm_json::json!({"ambiguous": ambiguous}),
            aterm_json::json!({"clean": clean, "ambiguous": ambiguous}),
            aterm_json::json!({"same": record("/store/trust/9192\n", true), "ambiguous": ambiguous}),
        ] {
            assert_eq!(
                sysroot_named(&info(outputs)),
                None,
                "eligible control-bearing stdout must keep all inference unknown: {stdout:?}"
            );
        }
        assert_eq!(
            sysroot_named(&info(aterm_json::json!({
                "clean": clean, "failed": record(&stdout, false)
            }))),
            expected,
            "failed output stays ineligible before pathname validation"
        );
    }
}

#[test]
#[cfg(unix)]
fn unix_ambiguous_sysroot_cannot_classify_units_or_authorize_store_reads() {
    let mut fs = world(now() - 2 * DAY, 4 * DAY);
    let info = br#"{"outputs":{"successful":{"success":true,"status":"","code":0,"stdout":"/store/trust/9192\r\n","stderr":""}}}"#;
    assert!(aterm_json::from_slice::<aterm_json::Value>(info).is_ok());
    fs.file(&format!("{LEAF}/.rustc_info.json"), now(), info);
    fs.looked.borrow_mut().clear();
    let s = surveyed(&fs);
    assert_eq!(s.last_built, None);
    assert_eq!(
        decide(&s, now(), RECLAIM_STALE)
            .iter()
            .map(|d| d.part)
            .collect::<Vec<_>>(),
        vec![Part::Incremental]
    );
    assert!(fs.looked.borrow().iter().all(|p| p.starts_with(LEAF)));
}

/// Unix only: the fixture's sysroot (`/store/trust/…`) is read only when it is
/// absolute, which a path with no drive is not on Windows.
#[test]
#[cfg(unix)]
fn units_stay_where_the_current_compiler_cannot_be_named() {
    let incremental_only = |fs: &Fake| {
        let s = surveyed(fs);
        let parts: Vec<Part> = decide(&s, now(), RECLAIM_STALE)
            .iter()
            .map(|d| d.part)
            .collect();
        assert_eq!(parts, vec![Part::Incremental]);
        s.last_built
    };
    let info = format!("{LEAF}/.rustc_info.json");
    let stock = "/Users//me/.rustup/toolchains/stable-aarch64-apple-darwin";
    for sysroots in [
        vec![stock],
        vec!["/opt/trust/15698"],
        vec!["/store/trust/15698", "/store/trust/9192"],
        vec![],
    ] {
        let mut fs = world(now() - 2 * DAY, 4 * DAY);
        fs.file(&info, now(), &rustc_info(&sysroots));
        assert_eq!(incremental_only(&fs), None, "{sysroots:?}");
    }
    for foreign in [
        "/Users//me/Documents/store/trust/7",
        "/Volumes/away/store/trust/7",
        "/other/store/trust/7",
        "/store/trust/../../other/7",
    ] {
        let mut fs = world(now() - 2 * DAY, 4 * DAY);
        fs.file(&info, now(), &rustc_info(&[foreign]));
        fs.looked.borrow_mut().clear();
        assert_eq!(incremental_only(&fs), None);
        assert!(fs.looked.borrow().iter().all(|p| p.starts_with(LEAF)));
    }
    let no_store = world(now() - 2 * DAY, 4 * DAY);
    let s = survey(&no_store, Path::new(LEAF), &[PathBuf::from(PROFILE)], None);
    assert_eq!(s.last_built, None);
    assert!(no_store.looked.borrow().iter().all(|p| p.starts_with(LEAF)));
    assert!(
        decide(&s, now(), RECLAIM_STALE)
            .iter()
            .all(|d| d.part == Part::Incremental)
    );
    let mut aliased = world(now() - 2 * DAY, 4 * DAY);
    aliased.link("/store/trust", now(), "/Users//me/Documents/other");
    aliased.looked.borrow_mut().clear();
    assert_eq!(incremental_only(&aliased), None);
    assert!(
        !aliased
            .looked
            .borrow()
            .iter()
            .any(|p| p == Path::new("/store/trust/current"))
    );

    for device in [None, Some(2)] {
        let mut foreign_device = world(now() - 2 * DAY, 4 * DAY);
        foreign_device
            .devices
            .insert(PathBuf::from("/store/trust"), device);
        foreign_device.looked.borrow_mut().clear();
        assert_eq!(incremental_only(&foreign_device), None);
        assert!(
            !foreign_device
                .looked
                .borrow()
                .iter()
                .any(|p| p == Path::new("/store/trust/current"))
        );
    }

    let mut no_link = world(now() - 2 * DAY, 4 * DAY);
    no_link.dir("/store/trust/current", now() - 2 * DAY);
    assert_eq!(incremental_only(&no_link), None);

    let mut unbuilt = world(now(), 4 * DAY);
    for hash in [NEW, FRESH] {
        let fp = format!("{PROFILE}/.fingerprint/aterm-grid-{hash}");
        for p in [
            fp.clone(),
            format!("{fp}/lib-aterm_grid.json"),
            format!("{fp}/invoked.timestamp"),
        ] {
            unbuilt.touch(&p, now() - DAY);
        }
    }
    assert_eq!(
        incremental_only(&unbuilt),
        Some(LastBuilt {
            current: true,
            since: now()
        })
    );
}

/// Sets the unit `hash`'s fingerprint folder, its files and its entries to `at`
/// (for the Unix-only rows below).
#[cfg(unix)]
fn date_unit(fs: &mut Fake, hash: &str, at: SystemTime) {
    let named: Vec<String> = fs
        .nodes
        .keys()
        .filter(|p| p.to_str().is_some_and(|p| p.contains(&format!("-{hash}"))))
        .map(|p| p.display().to_string())
        .collect();
    for p in named {
        fs.touch(&p, at);
    }
}

/// Unix only: the fixture's sysroot (`/store/trust/…`) is read only when it is
/// absolute, which a path with no drive is not on Windows.
#[test]
#[cfg(unix)]
fn a_workspace_left_on_the_previous_compiler_loses_every_unit_once_stale() {
    let mut fs = world(now() - DAY, 4 * DAY);
    for hash in [NEW, FRESH] {
        date_unit(&mut fs, hash, now() - 5 * DAY);
    }
    let info = format!("{LEAF}/.rustc_info.json");
    fs.file(&info, now() - 5 * DAY, &rustc_info(&["/store/trust/9192"]));
    let s = surveyed(&fs);
    assert_eq!(
        s.last_built,
        Some(LastBuilt {
            current: false,
            since: now() - DAY
        })
    );
    let units: Vec<PathBuf> = decide(&s, now(), RECLAIM_STALE)
        .into_iter()
        .filter(|d| d.part == Part::Unit)
        .map(|d| d.witness.path)
        .collect();
    let fingerprint_of =
        |hash: &str| PathBuf::from(format!("{PROFILE}/.fingerprint/aterm-grid-{hash}"));
    assert_eq!(units, [OLD, NEW, FRESH].map(fingerprint_of));

    fs.file(&info, now() - 5 * DAY, &rustc_info(&["/store/trust/15698"]));
    let parts: Vec<Part> = decide(&surveyed(&fs), now(), RECLAIM_STALE)
        .iter()
        .map(|d| d.part)
        .collect();
    assert_eq!(parts, vec![Part::Incremental]);
}

/// Unix only: the fixture's sysroot (`/store/trust/…`) is read only when it is
/// absolute, which a path with no drive is not on Windows.
#[test]
#[cfg(unix)]
fn what_was_written_since_the_survey_is_kept() {
    let mut fs = world(now() - 2 * DAY, 4 * DAY);
    let doomed = decide(&surveyed(&fs), now(), RECLAIM_STALE);
    assert!(doomed.iter().all(|d| unchanged(&fs, d)));
    fs.touch(
        &format!("{PROFILE}/incremental/aterm_grid-1k116trr0a0xf/s-hneouhc0uf-1tn8o9j.lock"),
        now(),
    );
    fs.touch(
        &format!("{PROFILE}/.fingerprint/aterm-grid-{OLD}/invoked.timestamp"),
        now(),
    );
    assert!(doomed.iter().all(|d| !unchanged(&fs, d)));

    let mut artifact = world(now() - 2 * DAY, 4 * DAY);
    let before = decide(&surveyed(&artifact), now(), RECLAIM_STALE);
    assert!(before.iter().any(|d| d.part == Part::Unit));
    artifact.touch(&format!("{PROFILE}/deps/libaterm_grid-{OLD}.rlib"), now());
    assert!(
        decide(&surveyed(&artifact), now(), RECLAIM_STALE)
            .iter()
            .all(|d| d.part == Part::Incremental)
    );

    let mut moved = world(now() - 2 * DAY, 4 * DAY);
    assert!(
        decide(&surveyed(&moved), now(), RECLAIM_STALE)
            .iter()
            .any(|d| d.part == Part::Unit)
    );
    moved.link("/store/trust/current", now() + DAY, "15698");
    assert!(
        decide(&surveyed(&moved), now(), RECLAIM_STALE)
            .iter()
            .all(|d| d.part == Part::Incremental)
    );

    let mut hidden = world(now() - 2 * DAY, 4 * DAY);
    hidden.unreadable.push(PathBuf::from(format!(
        "{PROFILE}/incremental/aterm_grid-1k116trr0a0xf/s-hneouhc0uf-1tn8o9j.lock"
    )));
    let s = surveyed(&hidden);
    assert_eq!(s.profiles[0].crates[0].newest, None);
    assert!(
        decide(&s, now(), RECLAIM_STALE)
            .iter()
            .all(|d| d.part == Part::Unit)
    );
}

#[test]
fn the_unit_hash_is_read_from_every_output_name() {
    let h = "0123456789abcdef";
    for name in [
        format!("libaterm_grid-{h}.rlib"),
        format!("libaterm_grid-{h}.rmeta"),
        format!("aterm_grid-{h}"),
        format!("aterm_grid-{h}.d"),
        format!("aterm_grid-{h}.dSYM"),
        format!("astream_sys-{h}.0asrtn8wzkwablau3ejrbexnp.030s1tl.rcgu.o"),
        format!("my-pkg-{h}"),
    ] {
        assert_eq!(unit_hash(&name), Some(h), "{name}");
    }
    for name in [
        "libaterm_gpu_web.rlib",
        "0b6650033266f1a6.twit",
        "aterm_grid-0123456789ABCDEF",
        "aterm_grid-0123456789abcde",
        "aterm_grid-0123456789abcdef0",
        "aterm_grid-1k116trr0a0xf",
    ] {
        assert_eq!(unit_hash(name), None, "{name}");
    }
}

/// One unit of the layout that keeps each in a folder, `build/<pkg>/<hash>/`: its fingerprint
/// naming `kind` (`lib-aterm_grid`, `test-lib-aterm_grid`) and compiler `rustc`, and its outputs
/// `outputs` (named with the hash where they hold `{h}`), all written `at`.
fn folder_unit(
    fs: &mut Fake,
    package: &str,
    hash: &str,
    kind: &str,
    rustc: u64,
    outputs: &[&str],
    at: SystemTime,
) {
    let unit = format!("{PROFILE}/build/{package}/{hash}");
    fs.dir(&unit, at)
        .file(&format!("{unit}/fingerprint/{kind}"), at, b"h")
        .file(
            &format!("{unit}/fingerprint/{kind}.json"),
            at,
            &fingerprint(rustc),
        )
        .file(&format!("{unit}/fingerprint/dep-{kind}"), at, b"d")
        .file(&format!("{unit}/fingerprint/invoked.timestamp"), at, b"");
    for out in outputs {
        fs.file(
            &format!("{unit}/out/{}", out.replace("{h}", hash)),
            at,
            b"o",
        );
    }
}

/// THE LAYOUT THAT KEEPS EACH UNIT IN A FOLDER IS READ TOO: a unit an old compiler built goes
/// whole, as its one folder (`build/<pkg>/<hash>`), once stale; the current compiler's units
/// stay. Before A4 the survey read `.fingerprint/` alone, which this layout has none of, so it
/// took nothing of it. NEGATIVE CONTROL: the same old unit written within the window stays.
#[test]
#[cfg(unix)]
fn a_folder_layout_unit_an_old_compiler_built_goes_whole() {
    let moved = now() - 2 * DAY;
    let mut fs = Fake::default();
    fs.dir("/store/trust/15698", moved)
        .link("/store/trust/current", moved, "15698")
        .file(
            &format!("{LEAF}/.rustc_info.json"),
            moved,
            &rustc_info(&["/store/trust/15698"]),
        )
        .file(
            &format!("{PROFILE}/.cargo-build-lock"),
            now() - 90 * DAY,
            b"",
        );
    let lib = [
        "libaterm_grid-{h}.rlib",
        "libaterm_grid-{h}.rmeta",
        "aterm_grid-{h}.d",
    ];
    folder_unit(
        &mut fs,
        "aterm-grid",
        OLD,
        "lib-aterm_grid",
        1,
        &lib,
        now() - 4 * DAY,
    );
    folder_unit(
        &mut fs,
        "aterm-grid",
        NEW,
        "lib-aterm_grid",
        2,
        &lib,
        moved + DAY,
    );
    folder_unit(
        &mut fs,
        "aterm-grid",
        FRESH,
        "lib-aterm_grid",
        2,
        &lib,
        now(),
    );
    let s = surveyed(&fs);
    assert_eq!(
        s.profiles[0]
            .units
            .iter()
            .map(|u| (u.package.as_str(), u.hash.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("aterm-grid", OLD),
            ("aterm-grid", NEW),
            ("aterm-grid", FRESH)
        ]
    );
    assert_eq!(
        paths(&decide(&s, now(), RECLAIM_STALE)),
        vec![(Part::Unit, vec![format!("build/aterm-grid/{OLD}")])]
    );
    folder_unit(
        &mut fs,
        "aterm-grid",
        OLD,
        "lib-aterm_grid",
        1,
        &lib,
        now() - DAY,
    );
    assert_eq!(decide(&surveyed(&fs), now(), RECLAIM_STALE), Vec::new());
}

/// The library build the release run's tests used.
const USED_LIB: &str = "6666666666666666";
/// Lint's check of that library, which writes only `.rmeta`.
const CHECK: &str = "4444444444444444";
/// A build of that library another compile of the run made under its own features (spec-check's).
const SPEC: &str = "8888888888888888";
/// The build of `libc` the release run's tests used.
const LIBC: &str = "5555555555555555";
/// Another build of `libc`, under another compile's features (spec-check's).
const LIBC_SPEC: &str = "7777777777777777";

/// `at` in milliseconds since the epoch.
fn millis(at: SystemTime) -> u64 {
    u64::try_from(at.duration_since(UNIX_EPOCH).unwrap().as_millis()).unwrap()
}

/// When the one run [`release_clone`]'s record keeps began: an hour before the record was
/// written, four days ago.
fn began() -> SystemTime {
    now() - 4 * DAY - HOUR
}

/// The scope [`release_clone`]'s record keeps its one run under.
const SCOPE: &str = "verify /c/Users//me/ws test --workspace";

/// The record at the top of `fs`'s build directory.
fn record(fs: &Fake) -> Kept {
    Kept::decode(&fs.nodes[Path::new(&format!("{LEAF}/{USED}"))].2).unwrap()
}

/// `kept` as the record at the top of `fs`'s build directory, written `at`.
fn keep(fs: &mut Fake, kept: &Kept, at: SystemTime) {
    fs.file(&format!("{LEAF}/{USED}"), at, kept.encode().as_bytes());
}

/// A run that began at `began`, built tests of `members` and used each `(package, hash)` unit of
/// the profile, named by an output in its folder.
fn ran(began: SystemTime, members: &[&str], units: &[(&str, &str)]) -> Used {
    let mut used = Used {
        began_ms: millis(began),
        members: members.iter().map(|m| (*m).to_owned()).collect(),
        ..Used::default()
    };
    for (package, hash) in units {
        used.add(
            Path::new(LEAF),
            Path::new(&format!("{PROFILE}/build/{package}/{hash}/out/x-{hash}")),
        );
    }
    used
}

/// The release clone's profile. Of `aterm-grid`, a package of the workspace's own: the test unit of
/// the candidate before (`OLD`), its test unit now (`NEW`), the library build its tests used
/// ([`USED_LIB`], `.rlib` and `.rmeta`) and lint's check of it ([`CHECK`]), each written five days
/// ago; a test unit built a day ago (`FRESH`); and the build of its library another compile of
/// the run made ([`SPEC`]), written after the run began. Of `libc`, a dependency every candidate
/// reuses, unwritten a month: the build the tests used ([`LIBC`]) and another compile's
/// ([`LIBC_SPEC`]). The record, written four days ago, keeps one run ([`SCOPE`], [`began`]), which
/// names `NEW`, [`USED_LIB`] and [`LIBC`], and `aterm-grid` as the workspace's own, and retired
/// nothing.
fn release_clone() -> Fake {
    release_clone_with(
        &["libaterm_grid-{h}.rlib", "libaterm_grid-{h}.rmeta"],
        &["aterm-grid"],
    )
}

/// [`release_clone`], with [`USED_LIB`]'s outputs `lib` and the record's own packages `members`.
fn release_clone_with(lib: &[&str], members: &[&str]) -> Fake {
    let mut fs = Fake::default();
    fs.file(
        &format!("{PROFILE}/.cargo-build-lock"),
        now() - 90 * DAY,
        b"",
    );
    let then = now() - 5 * DAY;
    let test = ["aterm_grid-{h}", "aterm_grid-{h}.d"];
    let grid = |fs: &mut Fake, hash: &str, kind: &str, outputs: &[&str], at: SystemTime| {
        folder_unit(fs, "aterm-grid", hash, kind, 2, outputs, at);
    };
    grid(&mut fs, OLD, "test-lib-aterm_grid", &test, then);
    grid(&mut fs, NEW, "test-lib-aterm_grid", &test, then);
    grid(&mut fs, FRESH, "test-lib-aterm_grid", &test, now() - DAY);
    grid(&mut fs, USED_LIB, "lib-aterm_grid", lib, then);
    grid(
        &mut fs,
        CHECK,
        "lib-aterm_grid",
        &["libaterm_grid-{h}.rmeta"],
        then,
    );
    grid(
        &mut fs,
        SPEC,
        "lib-aterm_grid",
        &["libaterm_grid-{h}.rlib", "libaterm_grid-{h}.rmeta"],
        began() + Duration::from_secs(10 * 60),
    );
    let libc = ["liblibc-{h}.rlib", "liblibc-{h}.rmeta"];
    for hash in [LIBC, LIBC_SPEC] {
        folder_unit(
            &mut fs,
            "libc",
            hash,
            "lib-libc",
            2,
            &libc,
            now() - 30 * DAY,
        );
    }
    let mut kept = Kept::default();
    kept.end(
        SCOPE,
        ran(
            began(),
            members,
            &[
                ("aterm-grid", NEW),
                ("aterm-grid", USED_LIB),
                ("libc", LIBC),
            ],
        ),
    );
    keep(&mut fs, &kept, now() - 4 * DAY);
    fs
}

/// The release clone's profile ([`release_clone`]) as [`survey`] reads it.
fn clone_survey(fs: &Fake) -> Survey {
    survey(fs, Path::new(LEAF), &[PathBuf::from(PROFILE)], None)
}

/// What [`decide`] takes as superseded: each `(package, hash)` unit's folder.
fn superseded(units: &[(&str, &str)]) -> Vec<(Part, Vec<String>)> {
    units
        .iter()
        .map(|(package, hash)| (Part::Superseded, vec![format!("build/{package}/{hash}")]))
        .collect()
}

/// When the first and the second run of [`dev_checkout`]'s scope began.
fn first() -> SystemTime {
    now() - 2 * HOUR
}

fn second() -> SystemTime {
    now() - 10 * MINUTE
}

/// A dev checkout's profile across two runs of one scope. Of `aterm-grid`, a package of the
/// workspace's own: the test unit the first run used (`OLD`, written `old_at`), the test unit the
/// second built (`NEW`, written a minute after it began) and the library build both used
/// ([`USED_LIB`], written three hours ago, before either). Of `libc`, a dependency, unwritten a
/// month: the build both used ([`LIBC`]) and another ([`LIBC_SPEC`]). No record.
fn dev_checkout(old_at: SystemTime) -> Fake {
    let mut fs = Fake::default();
    fs.file(
        &format!("{PROFILE}/.cargo-build-lock"),
        now() - 90 * DAY,
        b"",
    );
    let test = ["aterm_grid-{h}", "aterm_grid-{h}.d"];
    folder_unit(
        &mut fs,
        "aterm-grid",
        OLD,
        "test-lib-aterm_grid",
        2,
        &test,
        old_at,
    );
    folder_unit(
        &mut fs,
        "aterm-grid",
        NEW,
        "test-lib-aterm_grid",
        2,
        &test,
        second() + MINUTE,
    );
    folder_unit(
        &mut fs,
        "aterm-grid",
        USED_LIB,
        "lib-aterm_grid",
        2,
        &["libaterm_grid-{h}.rlib", "libaterm_grid-{h}.rmeta"],
        now() - 3 * HOUR,
    );
    for hash in [LIBC, LIBC_SPEC] {
        folder_unit(
            &mut fs,
            "libc",
            hash,
            "lib-libc",
            2,
            &["liblibc-{h}.rlib", "liblibc-{h}.rmeta"],
            now() - 30 * DAY,
        );
    }
    fs
}

/// A UNIT THE KEPT RUNS STOPPED USING GOES WHEN ITS RUN ENDS ([`Part::Superseded`],
/// [`Kept::retired`]): a scope's first run, two hours ago, used the test unit `OLD`; its second,
/// ten minutes ago, used `NEW` in its place, so its end retired `OLD`, and `OLD`, written an hour
/// ago, goes under the calm window it is nowhere near quiet for. The units the second run used
/// stay, and so does `libc`'s other build. NEGATIVE CONTROLS, each the one thing that took the
/// unit away: a third end of the same run retires nothing (nothing is carried over), and `OLD`
/// waits the window; `OLD` written after the second run began stays; a retired unit of `libc`
/// stays while `libc` is no package the runs built tests of, and goes once it is; `OLD` stays
/// where the second run names no unit of its kind; where its folder cannot be listed; and where
/// the record is the one before this one (schema 1), under either name, which is not read, even
/// below the floor.
#[test]
fn a_unit_the_kept_runs_stopped_using_goes_when_its_run_ends() {
    let grid = "aterm-grid";
    let run1 = ran(
        first(),
        &[grid],
        &[(grid, OLD), (grid, USED_LIB), ("libc", LIBC)],
    );
    let run2 = ran(
        second(),
        &[grid],
        &[(grid, NEW), (grid, USED_LIB), ("libc", LIBC)],
    );
    // Each run ended in turn under the one scope: the record, and what each end retired.
    let ended = |runs: &[&Used]| {
        let mut kept = Kept::default();
        let retired: Vec<usize> = runs
            .iter()
            .map(|run| kept.end("s", (*run).clone()))
            .collect();
        (kept, retired)
    };
    let gone = |fs: &Fake| paths(&decide(&clone_survey(fs), now(), RECLAIM_STALE));
    let written = now() - 5 * MINUTE;

    let (kept, retired) = ended(&[&run1, &run2]);
    assert_eq!(retired, vec![0, 1]);
    assert_eq!(kept.runs.len(), 1, "one scope keeps one run");
    assert_eq!(
        kept.retired.profiles,
        BTreeMap::from([("debug".to_owned(), BTreeSet::from([OLD.to_owned()]))])
    );
    let mut fs = dev_checkout(now() - HOUR);
    keep(&mut fs, &kept, written);
    let s = clone_survey(&fs);
    assert_eq!(
        (s.began, s.retired_before),
        (Some(second()), Some(second()))
    );
    assert_eq!(
        gone(&fs),
        superseded(&[(grid, OLD)]),
        "an hour old, and not waited on"
    );

    let (same, retired) = ended(&[&run1, &run2, &run2]);
    assert_eq!(retired, vec![0, 1, 0]);
    keep(&mut fs, &same, written);
    assert_eq!(
        gone(&fs),
        Vec::new(),
        "the same run ended again retires nothing, and nothing is carried over"
    );

    let mut after = dev_checkout(second() + MINUTE);
    keep(&mut after, &kept, written);
    assert_eq!(
        gone(&after),
        Vec::new(),
        "written after the run that retired it began"
    );

    let with_libc = |members: &[&str]| {
        let (kept, retired) = ended(&[
            &ran(
                first(),
                members,
                &[
                    (grid, NEW),
                    (grid, USED_LIB),
                    ("libc", LIBC),
                    ("libc", LIBC_SPEC),
                ],
            ),
            &ran(
                second(),
                members,
                &[(grid, NEW), (grid, USED_LIB), ("libc", LIBC)],
            ),
        ]);
        assert_eq!(retired, vec![0, 1]);
        let mut fs = dev_checkout(now() - HOUR);
        keep(&mut fs, &kept, written);
        gone(&fs)
    };
    assert_eq!(
        with_libc(&[grid]),
        Vec::new(),
        "a retired unit of a dependency"
    );
    assert_eq!(
        with_libc(&[grid, "libc"]),
        superseded(&[("libc", LIBC_SPEC)]),
        "the same unit, of a package the runs built tests of"
    );

    let (kindless, retired) = ended(&[
        &run1,
        &ran(second(), &[grid], &[(grid, USED_LIB), ("libc", LIBC)]),
    ]);
    assert_eq!(retired, vec![0, 1]);
    let mut fs = dev_checkout(now() - HOUR);
    keep(&mut fs, &kindless, written);
    assert_eq!(gone(&fs), Vec::new(), "no kept run uses a unit of its kind");

    let mut blind = dev_checkout(now() - HOUR);
    keep(&mut blind, &kept, written);
    blind
        .blind
        .push(PathBuf::from(format!("{PROFILE}/build/aterm-grid/{OLD}")));
    assert_eq!(gone(&blind), Vec::new(), "its folder lists nothing");

    let mut before = dev_checkout(now() - HOUR);
    let schema1 = format!(
        r#"{{"schema":1,"began_ms":{},"members":["aterm-grid"],"profiles":{{"debug":["{NEW}","{USED_LIB}"]}}}}"#,
        millis(second())
    );
    for name in [USED, USED_BEFORE] {
        before.file(&format!("{LEAF}/{name}"), written, schema1.as_bytes());
    }
    let s = clone_survey(&before);
    assert_eq!(
        (
            s.began,
            s.retired_before,
            decide(&s, now(), RECLAIM_STALE_UNDER_PRESSURE)
        ),
        (None, None, Vec::new()),
        "the record before this one is not read"
    );
}

/// A SCOPED RUN'S RECORD DOOMS NO UNIT ANOTHER KEPT SCOPE USED: a run of the whole workspace
/// (`W`, two days ago) used the test unit `NEW`, and a scoped run (`P`, ten minutes ago) the test
/// unit `FRESH` of the same package and kind. Both runs are kept, `P`'s end retires nothing, and
/// `NEW`, unwritten four days, stays under both windows: only its kept run keeps it. NEGATIVE
/// CONTROLS: `P`'s run ended under `W`'s key replaces `W`'s and retires `NEW`, which then goes at
/// once, an hour old (and waits the window with the retirement taken out of the record); and a
/// `W` begun more than [`KEPT_FOR`] before `P` is evicted when `P` ends, its unit retired.
#[test]
fn a_scoped_runs_record_dooms_no_unit_another_kept_scope_used() {
    let grid = "aterm-grid";
    let world = |new_at: SystemTime| {
        let test = ["aterm_grid-{h}", "aterm_grid-{h}.d"];
        let mut fs = Fake::default();
        fs.file(
            &format!("{PROFILE}/.cargo-build-lock"),
            now() - 90 * DAY,
            b"",
        );
        folder_unit(&mut fs, grid, NEW, "test-lib-aterm_grid", 2, &test, new_at);
        folder_unit(
            &mut fs,
            grid,
            FRESH,
            "test-lib-aterm_grid",
            2,
            &test,
            now() - 5 * MINUTE,
        );
        fs
    };
    let whole = |began: SystemTime| ran(began, &[grid], &[(grid, NEW)]);
    let scoped = ran(now() - 10 * MINUTE, &[grid], &[(grid, FRESH)]);
    let scopes = |kept: &Kept| kept.runs.keys().cloned().collect::<Vec<_>>();
    let gone = |fs: &Fake, after| paths(&decide(&clone_survey(fs), now(), after));
    let written = now() - 5 * MINUTE;

    let mut kept = Kept::default();
    assert_eq!(kept.end("W", whole(now() - 2 * DAY)), 0);
    assert_eq!(kept.end("P", scoped.clone()), 0);
    assert_eq!(scopes(&kept), ["P", "W"]);
    assert!(kept.retired.profiles.is_empty());
    let mut fs = world(now() - 4 * DAY);
    keep(&mut fs, &kept, written);
    assert_eq!(clone_survey(&fs).began, Some(now() - 2 * DAY));
    for after in [RECLAIM_STALE, RECLAIM_STALE_UNDER_PRESSURE] {
        assert_eq!(gone(&fs, after), Vec::new(), "{after:?}");
    }

    let mut one = Kept::default();
    assert_eq!(one.end("W", whole(now() - 2 * DAY)), 0);
    assert_eq!(
        one.end("W", scoped.clone()),
        1,
        "the same run under the other scope's key retires its unit"
    );
    assert_eq!(scopes(&one), ["W"]);
    let mut fs = world(now() - HOUR);
    keep(&mut fs, &one, written);
    assert_eq!(
        gone(&fs, RECLAIM_STALE),
        superseded(&[(grid, NEW)]),
        "an hour old, at once"
    );
    one.retired = Used::default();
    keep(&mut fs, &one, written);
    assert_eq!(
        gone(&fs, RECLAIM_STALE),
        Vec::new(),
        "not retired, it waits the window"
    );

    let mut aged = Kept::default();
    assert_eq!(aged.end("W", whole(now() - 4 * DAY - 10 * MINUTE)), 0);
    assert_eq!(
        aged.end("P", scoped),
        1,
        "a run begun more than KEPT_FOR before the one ending is evicted"
    );
    assert_eq!(scopes(&aged), ["P"]);
    assert_eq!(
        aged.retired.profiles,
        BTreeMap::from([("debug".to_owned(), BTreeSet::from([NEW.to_owned()]))])
    );
}

/// A UNIT NO KEPT RUN REPORTED KEEPS THE STALE WINDOW ([`Part::Superseded`]): of the release
/// clone's units ([`release_clone`]), whose record keeps one run and retired nothing, the test
/// unit of the candidate before, which that run did not use beside the test unit of the same
/// package it did, goes whole once stale, and below the floor alone too. Kept: the units the run
/// used, however long unwritten (cargo found them fresh); lint's check of the library, which
/// wrote `.rmeta` alone, so another kind than the library build the run used; the test unit built
/// a day ago; the library build another compile of the run made, written after the run began; and
/// `libc`'s other build, a dependency's, which no version bump hashes anew and another compile
/// may read fresh. With a second, later run kept the oldest kept run's beginning still judges:
/// that library build, quiet and written between the two, stays. And the day-old test unit, once
/// the record names it retired by a run begun an hour ago, goes without waiting. NEGATIVE
/// CONTROLS, each the one thing that kept a unit taken away: the old test unit written after the
/// run began stays; the check goes where the library build the run used wrote `.rmeta` alone too;
/// `libc`'s other build goes where the record names `libc` among the workspace's own; the other
/// compile's library build goes where it was written before the run began, and where the later
/// run alone is kept; and without the record, or with a torn one, nothing goes.
#[test]
fn a_unit_no_run_reported_keeps_the_stale_window() {
    let s = clone_survey(&release_clone());
    assert_eq!(s.began, Some(began()));
    assert_eq!(s.members, BTreeSet::from(["aterm-grid".to_owned()]));
    assert!(s.profiles[0].retired.is_empty());
    let gone = |s: &Survey, after| paths(&decide(s, now(), after));
    assert_eq!(gone(&s, RECLAIM_STALE), superseded(&[("aterm-grid", OLD)]));
    assert_eq!(
        gone(&s, RECLAIM_STALE_UNDER_PRESSURE),
        superseded(&[("aterm-grid", OLD)]),
        "below the floor the day-old test unit and the other compile's build are stale too, but \
         written after the run began"
    );

    let mut after = release_clone();
    date_folder(
        &mut after,
        &format!("build/aterm-grid/{OLD}"),
        began() + Duration::from_secs(60),
    );
    assert_eq!(
        gone(&clone_survey(&after), RECLAIM_STALE),
        Vec::new(),
        "written after the run began"
    );

    let checked = release_clone_with(&["libaterm_grid-{h}.rmeta"], &["aterm-grid"]);
    assert_eq!(
        gone(&clone_survey(&checked), RECLAIM_STALE),
        superseded(&[("aterm-grid", OLD), ("aterm-grid", CHECK)]),
        "the library build the run used, of the check's output kind"
    );

    let libc_ours = release_clone_with(
        &["libaterm_grid-{h}.rlib", "libaterm_grid-{h}.rmeta"],
        &["aterm-grid", "libc"],
    );
    assert_eq!(
        gone(&clone_survey(&libc_ours), RECLAIM_STALE),
        superseded(&[("aterm-grid", OLD), ("libc", LIBC_SPEC)]),
        "a dependency the record calls the workspace's own"
    );

    let mut earlier = release_clone();
    date_folder(
        &mut earlier,
        &format!("build/aterm-grid/{SPEC}"),
        now() - 5 * DAY,
    );
    assert_eq!(
        gone(&clone_survey(&earlier), RECLAIM_STALE),
        superseded(&[("aterm-grid", OLD), ("aterm-grid", SPEC)]),
        "the other compile's build, written before the run began"
    );

    let mut two = release_clone();
    let mut kept = record(&two);
    let later = now() - 2 * DAY;
    assert_eq!(
        kept.end(
            "later",
            ran(
                later,
                &["aterm-grid"],
                &[
                    ("aterm-grid", NEW),
                    ("aterm-grid", USED_LIB),
                    ("libc", LIBC)
                ],
            ),
        ),
        0
    );
    assert_eq!(kept.runs.len(), 2);
    keep(&mut two, &kept, later + HOUR);
    let s = clone_survey(&two);
    assert_eq!(s.began, Some(began()), "the older of the two kept runs");
    assert_eq!(
        gone(&s, RECLAIM_STALE),
        superseded(&[("aterm-grid", OLD)]),
        "the other compile's build, written between the two runs, stays"
    );
    kept.runs.remove(SCOPE);
    keep(&mut two, &kept, later + HOUR);
    assert_eq!(
        gone(&clone_survey(&two), RECLAIM_STALE),
        superseded(&[("aterm-grid", OLD), ("aterm-grid", SPEC)]),
        "the later run alone kept, it is written before that run began"
    );

    let mut retired = release_clone();
    let mut kept = record(&retired);
    kept.retired = ran(now() - HOUR, &[], &[("aterm-grid", FRESH)]);
    keep(&mut retired, &kept, now() - HOUR / 2);
    let s = clone_survey(&retired);
    assert_eq!(
        (s.began, s.retired_before),
        (Some(began()), Some(now() - HOUR))
    );
    assert_eq!(
        gone(&s, RECLAIM_STALE),
        superseded(&[("aterm-grid", OLD), ("aterm-grid", FRESH)]),
        "the day-old test unit, retired"
    );

    let mut none = release_clone();
    none.nodes.remove(Path::new(&format!("{LEAF}/{USED}")));
    let s = clone_survey(&none);
    assert_eq!(
        (s.began, s.retired_before, s.profiles[0].used.clone()),
        (None, None, None)
    );
    assert_eq!(decide(&s, now(), RECLAIM_STALE), Vec::new());

    let mut torn = release_clone();
    torn.file(
        &format!("{LEAF}/{USED}"),
        now() - 4 * DAY,
        b"{\"schema\":2,\"runs\":{\"s\":{\"began_ms\":1,\"members\":[\"aterm-grid\"],\"profiles\":{\"debug\":[\"x\"]}}},\"retired\":{\"began_ms\":1,\"members\":[],\"profiles\":{}}}",
    );
    let s = clone_survey(&torn);
    assert_eq!(
        (s.began, decide(&s, now(), RECLAIM_STALE)),
        (None, Vec::new()),
        "a record with a word that is no hash is no record"
    );
}

/// THE RECORD KEEPS THE NEWEST RUN OF EACH SCOPE AND RETIRES WHAT THEY DROPPED ([`Kept::end`]): a
/// scope's run replaces the one before and retires what that used and no kept run uses now, as
/// of when the ending run began; each end computes it again, so nothing an earlier end retired is
/// carried over; a unit another kept scope uses is not retired; past [`KEPT_RUNS`] the run that
/// began first goes, never the one ending, however early that began; and the record reads back
/// as written, with as many runs as an end leaves at most. NEGATIVE CONTROLS: bytes of schema 1
/// (the record before this one, and this one's shape under its number), of schema 3, without its
/// runs or what it retired, with one run more than [`KEPT_RUNS`], with a word that is no hash, or
/// with a profile that is no plain path below the build directory, are no record.
#[test]
fn the_record_keeps_the_newest_run_of_each_scope_and_retires_what_they_dropped() {
    let grid = "aterm-grid";
    let at = |minutes: u64| now() - DAY + Duration::from_secs(60 * minutes);
    let run = |began: SystemTime, hashes: &[&str]| {
        let units: Vec<(&str, &str)> = hashes.iter().map(|h| (grid, *h)).collect();
        ran(began, &[grid], &units)
    };
    let debug = |hashes: &[&str]| {
        BTreeMap::from([(
            "debug".to_owned(),
            hashes
                .iter()
                .map(|h| (*h).to_owned())
                .collect::<BTreeSet<_>>(),
        )])
    };
    let scopes = |kept: &Kept| kept.runs.keys().cloned().collect::<Vec<_>>();

    let mut kept = Kept::default();
    assert_eq!(kept.end("a", run(at(0), &[OLD, USED_LIB])), 0);
    assert_eq!(
        kept.retired,
        Used {
            began_ms: millis(at(0)),
            ..Used::default()
        }
    );
    assert_eq!(kept.end("a", run(at(1), &[NEW, USED_LIB])), 1);
    assert_eq!(kept.retired.profiles, debug(&[OLD]));
    assert_eq!(kept.retired.began_ms, millis(at(1)));
    assert_eq!(kept.end("b", run(at(2), &[NEW])), 0);
    assert_eq!(
        kept.retired,
        Used {
            began_ms: millis(at(2)),
            ..Used::default()
        },
        "what the end before retired is not carried over"
    );
    assert_eq!(
        kept.end("a", run(at(3), &[FRESH, USED_LIB])),
        0,
        "the other kept scope uses what this one dropped"
    );
    assert_eq!(kept.end("b", run(at(4), &[FRESH])), 1);
    assert_eq!(kept.retired.profiles, debug(&[NEW]));
    assert_eq!(scopes(&kept), ["a", "b"]);

    assert_eq!(kept.end("c", run(at(5), &[SPEC])), 0);
    assert_eq!(kept.end("d", run(at(6), &[SPEC])), 0);
    assert_eq!(kept.runs.len(), KEPT_RUNS);
    assert_eq!(
        kept.end("e", run(at(7), &[SPEC])),
        1,
        "past KEPT_RUNS the run that began first goes"
    );
    assert_eq!(scopes(&kept), ["b", "c", "d", "e"]);
    assert_eq!(kept.retired.profiles, debug(&[USED_LIB]));
    assert_eq!(kept.end("z", run(now() - 2 * DAY, &[OLD])), 1);
    assert_eq!(
        scopes(&kept),
        ["c", "d", "e", "z"],
        "never the run ending, though it began first"
    );
    assert_eq!(kept.retired.profiles, debug(&[FRESH]));

    assert_eq!(Kept::decode(kept.encode().as_bytes()), Some(kept));

    let one =
        r#"{"began_ms":1,"members":["aterm-grid"],"profiles":{"debug":["1111111111111111"]}}"#;
    let none = r#"{"began_ms":1,"members":[],"profiles":{}}"#;
    let whole = format!(r#"{{"schema":2,"runs":{{"s":{one}}},"retired":{none}}}"#);
    assert!(Kept::decode(whole.as_bytes()).is_some(), "{whole}");
    let full = format!(
        r#"{{"schema":2,"runs":{{"a":{one},"b":{one},"c":{one},"d":{one}}},"retired":{none}}}"#
    );
    assert_eq!(
        Kept::decode(full.as_bytes()).map(|kept| kept.runs.len()),
        Some(KEPT_RUNS),
        "{full}"
    );
    for torn in [
        full.replace(r#""d":"#, &format!(r#""d":{one},"e":"#)),
        r#"{"schema":1,"began_ms":1,"members":["aterm-grid"],"profiles":{"debug":["1111111111111111"]}}"#.to_owned(),
        whole.replace(r#""schema":2"#, r#""schema":1"#),
        whole.replace(r#""schema":2"#, r#""schema":3"#),
        format!(r#"{{"schema":2,"retired":{none}}}"#),
        format!(r#"{{"schema":2,"runs":{{"s":{one}}}}}"#),
        whole.replace("1111111111111111", "x"),
        whole.replace("debug", "../x"),
        whole.replace("debug", "debug/"),
    ] {
        assert_eq!(Kept::decode(torn.as_bytes()), None, "{torn}");
    }
}

/// THE BEGINNINGS ARE READ NO LATER THAN THE RECORD WAS WRITTEN: a record whose run says it began
/// after the record was written (a clock moved) is read as beginning when it was written, so the
/// old test unit, written after the record, is never taken, while the other compile's library
/// build, written before it, is; and the beginning a retired unit is judged against is read the
/// same way. NEGATIVE CONTROL: a record whose run began before it was written is read as it says.
#[test]
fn a_run_is_read_as_beginning_no_later_than_its_record() {
    let mut fs = release_clone();
    let mut kept = record(&fs);
    kept.runs.get_mut(SCOPE).unwrap().began_ms = millis(now());
    kept.retired.began_ms = millis(now());
    keep(&mut fs, &kept, now() - 4 * DAY);
    date_folder(
        &mut fs,
        &format!("build/aterm-grid/{OLD}"),
        now() - 4 * DAY + Duration::from_secs(60),
    );
    let s = clone_survey(&fs);
    assert_eq!(s.began, Some(now() - 4 * DAY));
    assert_eq!(s.retired_before, Some(now() - 4 * DAY));
    assert_eq!(
        paths(&decide(&s, now(), RECLAIM_STALE)),
        superseded(&[("aterm-grid", SPEC)])
    );
    assert_eq!(clone_survey(&release_clone()).began, Some(began()));
}

/// A UNIT WHOSE FOLDER CANNOT BE LISTED IS NOT READ, so nothing is decided of it: an old
/// compiler's unit whose folder lists nothing stays, as does one whose `out/` lists nothing; in the
/// layout that names entries with the hash, a profile whose `deps/` is there but lists nothing
/// loses none of its units (their entries would go unread), only its stale incremental folder.
/// NEGATIVE CONTROL: the same units, readable, go.
#[test]
#[cfg(unix)]
fn a_unit_that_cannot_be_listed_is_not_read() {
    let moved = now() - 2 * DAY;
    let mut fs = Fake::default();
    fs.dir("/store/trust/15698", moved)
        .link("/store/trust/current", moved, "15698")
        .file(
            &format!("{LEAF}/.rustc_info.json"),
            moved,
            &rustc_info(&["/store/trust/15698"]),
        )
        .file(
            &format!("{PROFILE}/.cargo-build-lock"),
            now() - 90 * DAY,
            b"",
        );
    let lib = ["libaterm_grid-{h}.rlib", "libaterm_grid-{h}.rmeta"];
    folder_unit(
        &mut fs,
        "aterm-grid",
        OLD,
        "lib-aterm_grid",
        1,
        &lib,
        now() - 4 * DAY,
    );
    folder_unit(
        &mut fs,
        "aterm-grid",
        NEW,
        "lib-aterm_grid",
        2,
        &lib,
        moved + DAY,
    );
    let old = format!("build/aterm-grid/{OLD}");
    assert_eq!(
        paths(&decide(&surveyed(&fs), now(), RECLAIM_STALE)),
        vec![(Part::Unit, vec![old.clone()])]
    );
    for blind in [old.clone(), format!("{old}/out")] {
        fs.blind = vec![PathBuf::from(format!("{PROFILE}/{blind}"))];
        assert_eq!(
            decide(&surveyed(&fs), now(), RECLAIM_STALE),
            Vec::new(),
            "{blind} lists nothing"
        );
    }

    let mut named = world(now() - 2 * DAY, 4 * DAY);
    let both = paths(&decide(&surveyed(&named), now(), RECLAIM_STALE));
    assert!(both.iter().any(|(part, _)| *part == Part::Unit), "{both:?}");
    named.blind = vec![PathBuf::from(format!("{PROFILE}/deps"))];
    assert_eq!(
        paths(&decide(&surveyed(&named), now(), RECLAIM_STALE))
            .into_iter()
            .map(|(part, _)| part)
            .collect::<Vec<_>>(),
        vec![Part::Incremental]
    );
}

/// Sets every node at or below `rel` (under the profile) to `at`.
fn date_folder(fs: &mut Fake, rel: &str, at: SystemTime) {
    let root = PathBuf::from(format!("{PROFILE}/{rel}"));
    let named: Vec<PathBuf> = fs
        .nodes
        .keys()
        .filter(|p| p.starts_with(&root))
        .cloned()
        .collect();
    for p in named {
        fs.nodes.get_mut(&p).unwrap().1 = at;
    }
}

/// THE RECORD NAMES THE UNIT EACH OUTPUT IS OF, in either layout and any profile, and its run
/// reads back as written, with when it began and its own packages: `build/<pkg>/<hash>/…`, a
/// `deps/` file and a `build/<pkg>-<hash>` folder named with the hash, a build script's run (its
/// `out/`), a cross target's profile. NEGATIVE CONTROL: an output cargo copied into the target
/// directory, one named with no hash, and a path with no profile name no unit; a record whose run
/// is without when it began, or without its packages, or names an empty one, is none.
#[test]
fn the_used_record_names_each_outputs_unit() {
    let leaf = Path::new(LEAF);
    let mut used = Used {
        began_ms: 1_799_990_000_000,
        members: BTreeSet::from(["aterm-grid".to_owned()]),
        ..Used::default()
    };
    for file in [
        format!("{PROFILE}/build/aterm-grid/{NEW}/out/libaterm_grid-{NEW}.rlib"),
        format!("{PROFILE}/build/aterm-grid/{NEW}/out/aterm_grid-{NEW}.d"),
        format!("{PROFILE}/deps/libaterm_grid-{OLD}.rmeta"),
        format!("{PROFILE}/build/aterm-grid-{FRESH}/out/generated.rs"),
        format!("{PROFILE}/build/libc/{LIBC}/out"),
        format!("{LEAF}/aarch64-apple-darwin/debug/build/libc/{NEW}/out/liblibc-{NEW}.rlib"),
        "/c/Users//me/ws/target/debug/aterm".to_owned(),
        format!("{PROFILE}/deps/libaterm_gpu_web.rlib"),
        format!("{LEAF}/deps/libx-{OLD}.rlib"),
    ] {
        used.add(leaf, Path::new(&file));
    }
    let set = |hashes: &[&str]| {
        hashes
            .iter()
            .map(|h| (*h).to_owned())
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(
        used.profiles,
        BTreeMap::from([
            ("aarch64-apple-darwin/debug".to_owned(), set(&[NEW])),
            ("debug".to_owned(), set(&[OLD, NEW, FRESH, LIBC])),
        ])
    );
    let mut kept = Kept::default();
    kept.end("s", used.clone());
    assert_eq!(
        Kept::decode(kept.encode().as_bytes()).map(|kept| kept.runs["s"].clone()),
        Some(used)
    );
    let none = r#"{"began_ms":1,"members":[],"profiles":{}}"#;
    for torn in [
        r#"{"members":[],"profiles":{}}"#,
        r#"{"began_ms":1,"profiles":{}}"#,
        r#"{"began_ms":1,"members":[""],"profiles":{}}"#,
    ] {
        for record in [
            format!(r#"{{"schema":2,"runs":{{"s":{torn}}},"retired":{none}}}"#),
            format!(r#"{{"schema":2,"runs":{{"s":{none}}},"retired":{torn}}}"#),
        ] {
            assert_eq!(Kept::decode(record.as_bytes()), None, "{record}");
        }
    }
    assert!(
        Kept::decode(
            format!(r#"{{"schema":2,"runs":{{"s":{none}}},"retired":{none}}}"#).as_bytes()
        )
        .is_some()
    );
}

/// `file`'s modification time set to `at`.
#[cfg(unix)]
fn written_at(file: &Path, at: SystemTime) {
    std::fs::File::options()
        .write(true)
        .open(file)
        .unwrap()
        .set_modified(at)
        .unwrap();
}

/// A BIN A TEST NEEDS IS RECORDED BY THE UNIT ITS COPY CAME FROM ([`Used::add_uplifted`]): cargo
/// names it by its copy in the target directory alone, a copy keeping the unit's length and its
/// modification time to the nanosecond; the unit of its package whose `out/` holds that twin under
/// its crate name is recorded, and a hard link to a unit's output is that unit's. NEGATIVE
/// CONTROLS: another build of the same bin, as long but written at another instant, is not
/// recorded; nor is anything for a twin looked for under another package or another profile, or
/// for a package or profile that is no plain name.
#[cfg(unix)]
#[test]
fn a_bin_a_test_needs_is_recorded_by_the_unit_its_copy_came_from() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("stale-uplift-")
        .tempdir()
        .unwrap();
    let leaf = dir.path().join("build");
    let unit = |hash: &str| {
        let out = leaf.join("debug/build/atpkg").join(hash).join("out");
        std::fs::create_dir_all(&out).unwrap();
        let bin = out.join("atpkg_abrew");
        std::fs::write(&bin, b"the bin the tests ran").unwrap();
        bin
    };
    let at = UNIX_EPOCH + Duration::new(1_799_000_000, 123_456_789);
    let ran = unit(NEW);
    written_at(&ran, at);
    let other = unit(OLD);
    written_at(&other, at + Duration::from_nanos(1));
    let target = dir.path().join("target/debug");
    std::fs::create_dir_all(&target).unwrap();
    let copy = target.join("atpkg-abrew");
    std::fs::write(&copy, b"the bin the tests ran").unwrap();
    written_at(&copy, at);

    let mut used = Used::default();
    used.add_uplifted(&leaf, "debug", "atpkg", &copy);
    let debug = |used: &Used| used.profiles.get("debug").cloned().unwrap_or_default();
    assert_eq!(debug(&used), BTreeSet::from([NEW.to_owned()]));

    let linked_unit = unit(FRESH);
    written_at(&linked_unit, at + Duration::from_secs(5));
    let linked = dir.path().join("linked/debug");
    std::fs::create_dir_all(&linked).unwrap();
    std::fs::hard_link(&linked_unit, linked.join("atpkg-abrew")).unwrap();
    used.add_uplifted(&leaf, "debug", "atpkg", &linked.join("atpkg-abrew"));
    assert_eq!(
        debug(&used),
        BTreeSet::from([NEW.to_owned(), FRESH.to_owned()])
    );

    let mut none = Used::default();
    for (profile, package) in [
        ("debug", "aterm"),
        ("aarch64-apple-darwin/debug", "atpkg"),
        ("../debug", "atpkg"),
        ("debug", "../atpkg"),
        ("debug", ""),
    ] {
        none.add_uplifted(&leaf, profile, package, &copy);
    }
    assert_eq!(none, Used::default());
}
