// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! DC-24b end to end: a Mac at Full Access takes the shared dependency units a member its grants
//! admit holds, through `aterm pkg unit-source` and the fleet listener's `built` op, between two
//! `aterm --headless` instances of one fleet (the issuer holds, the member that grants it drive
//! takes).
//!
//! The first build fills targo's store (`targo --unverified build --lib --message-format=json`
//! of a fixture whose library units a, b and c, c on b on a, the store shares, with one
//! `CARGO_HOME`, `PATH`, `HOME`, `LANG` and `TZ`, as two Macs with one home folder would build);
//! it is then moved into the holding instance's own cargo home, the store its listener serves,
//! leaving an empty store at the builds' path.
//!
//! Two lanes judge the taking Mac:
//!
//! - THE EXIT (DC-24b's three named tests): the taker's build is started as its build front
//!   starts targo, and its own targo asks the source (Trust's DC-24a); each unit taken is one the
//!   build did not compile (`fresh`). A toolchain that never asks fails them, so they wait,
//!   ignored, for the trust.lock build that carries DC-24a (DC-18d adopts one).
//! - THE STAND-IN: the source is asked exactly as DC-24a's targo asks it (one JSON line per unit,
//!   a new private staging folder in the store), the staged units placed as its `place` does and
//!   checked again against their record and receipt; the build front itself is run through
//!   `atpkg __build_admit` to show what it names targo. These run on any toolchain. A toolchain
//!   from before DC-24a writes no output receipt for a dependency unit, so the holder's units
//!   get the receipt their compiler would have written where they carry none.
//!
//! The fixture's units come from a vendored registry, since a toolchain without DC-18b shares no
//! workspace member.

#![cfg(target_os = "macos")]

use aterm_uds::spawnfd::Stdio;
use std::io::{BufRead as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use aterm_json::Value;

use crate::fleet_live_headless::{driven_pair, fleet_ok};
use crate::headless_boot::Instance;

const HANG: Duration = aterm_spec::wait::MUST_HAPPEN;

const UNITS: [&str; 3] = ["a", "b", "c"];

/// The store under a cargo home.
fn store_in(cargo_home: &Path) -> PathBuf {
    cargo_home.join("trust/native-dependencies-v1")
}

fn sha256_hex(bytes: &[u8]) -> String {
    aterm_net::built::sha256_hex(bytes)
}

/// The fixture: a vendored registry of a, b and c, a home and a cargo home every build shares.
struct Fixture {
    _root: aterm_tempfile::TempDir,
    root: PathBuf,
    cargo_home: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = aterm_tempfile::tempdir().expect("a fixture folder");
        // The path targo names its outputs by (`/private/var/…`, not `/var/…`).
        let path = std::fs::canonicalize(root.path()).expect("the fixture folder's real path");
        for (name, dep, body) in [
            ("a", None, "pub fn value() -> u32 { 1 }\n"),
            ("b", Some("a"), "pub fn value() -> u32 { a::value() + 1 }\n"),
            ("c", Some("b"), "pub fn value() -> u32 { b::value() + 1 }\n"),
        ] {
            let krate = path.join("vendor").join(name);
            std::fs::create_dir_all(krate.join("src")).unwrap();
            let manifest = format!(
                "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n{}",
                dep.map(|dep| format!("{dep} = \"0.1.0\"\n"))
                    .unwrap_or_default()
            );
            std::fs::write(krate.join("Cargo.toml"), &manifest).unwrap();
            std::fs::write(krate.join("src/lib.rs"), body).unwrap();
            std::fs::write(
                krate.join(".cargo-checksum.json"),
                format!(
                    "{{\"files\":{{\"Cargo.toml\":\"{}\",\"src/lib.rs\":\"{}\"}},\"package\":null}}",
                    sha256_hex(manifest.as_bytes()),
                    sha256_hex(body.as_bytes())
                ),
            )
            .unwrap();
        }
        std::fs::create_dir_all(path.join("home")).unwrap();
        Self {
            cargo_home: path.join("cargo-home"),
            root: path,
            _root: root,
        }
    }

    /// A worktree of the package that uses c, carrying the checkout's trust.lock and a shared
    /// dependency cache.
    fn worktree(&self, name: &str) -> PathBuf {
        let tree = self.root.join(name);
        std::fs::create_dir_all(tree.join("src")).unwrap();
        std::fs::create_dir_all(tree.join(".cargo")).unwrap();
        std::fs::write(
            tree.join("Cargo.toml"),
            "[package]\nname = \"top\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nc = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(
            tree.join("src/lib.rs"),
            "pub fn value() -> u32 { c::value() }\n",
        )
        .unwrap();
        std::fs::write(
            tree.join(".cargo/config.toml"),
            format!(
                "[build]\nshared-dependency-cache = true\n\n[source.crates-io]\nreplace-with = \"vendored\"\n\n[source.vendored]\ndirectory = \"{}\"\n",
                self.root.join("vendor").display()
            ),
        )
        .unwrap();
        let lock = aterm_tempfile::manifest_dir().join("../../trust.lock");
        std::fs::copy(&lock, tree.join("trust.lock")).expect("the checkout's trust.lock");
        tree
    }

    fn store(&self) -> PathBuf {
        store_in(&self.cargo_home)
    }

    /// The environment every build and the source share: one home, path, language and zone,
    /// the taking instance's state, and the seams its fleet runs under.
    fn env(&self, taker: &Instance) -> Vec<(String, std::ffi::OsString)> {
        let hang = HANG.as_millis().to_string();
        vec![
            ("HOME".into(), self.root.join("home").into()),
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("LANG".into(), "en_US.UTF-8".into()),
            ("TZ".into(), "UTC".into()),
            ("TMPDIR".into(), self.root.join("home").into()),
            ("CARGO_HOME".into(), self.cargo_home.clone().into()),
            ("ATERM_LOG".into(), "off".into()),
            ("ATERM_STATE_HOME".into(), taker.tmp.join("state").into()),
            ("XDG_CONFIG_HOME".into(), taker.tmp.join("cfg").into()),
            ("XDG_STATE_HOME".into(), taker.tmp.join("state").into()),
            ("XDG_RUNTIME_DIR".into(), taker.tmp.join("run").into()),
            ("XDG_CACHE_HOME".into(), taker.tmp.join("cache").into()),
            ("XDG_DATA_HOME".into(), taker.tmp.join("data").into()),
            (aterm_net::loopback::SEAM.into(), "1".into()),
            (aterm_net::fleet::HANDSHAKE_SEAM.into(), hang.into()),
        ]
    }

    /// What the build front names to targo for `taker` now: the unit source, at Full Access.
    fn front(taker: &Instance) -> Option<(std::ffi::OsString, std::ffi::OsString)> {
        atpkg::build_cache::unit_source::environment(
            &taker.tmp.join("state"),
            &crate::launch_isolation::aterm_executable(),
        )
    }

    /// Build `tree` with `taker`'s environment and `source` (what its front names targo, if
    /// anything); each library unit's row by name.
    fn build(
        &self,
        tree: &Path,
        taker: &Instance,
        source: Option<(std::ffi::OsString, std::ffi::OsString)>,
    ) -> Vec<(String, bool, Vec<String>)> {
        let cargo = std::env::var_os("CARGO").expect("the targo running this test");
        let mut command = aterm_uds::spawnfd::command(cargo);
        command
            .current_dir(tree)
            .env_clear()
            .envs(self.env(taker))
            .args([
                "--unverified",
                "build",
                "-Zfine-grain-locking",
                "--lib",
                "--offline",
                "--message-format=json",
            ]);
        if let Some((name, value)) = source {
            command.env(name, value);
        }
        let output =
            aterm_uds::deadline::output_while_process_progressing_complete_stdout_with_limit(
                &mut command,
                HANG,
                "a fixture build",
                8 * 1024 * 1024,
            )
            .expect("the build ran");
        assert!(
            output.status.success(),
            "the build failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| aterm_json::from_str::<Value>(line).ok())
            .filter(|row| row.get("reason").and_then(Value::as_str) == Some("compiler-artifact"))
            .filter_map(|row| {
                let name = row.get("target")?.get("name")?.as_str()?.to_owned();
                let fresh = row.get("fresh")?.as_bool()?;
                let files = row
                    .get("filenames")?
                    .as_array()?
                    .iter()
                    .filter_map(|file| Some(file.as_str()?.to_owned()))
                    .collect();
                UNITS
                    .contains(&name.as_str())
                    .then_some((name, fresh, files))
            })
            .collect()
    }
}

/// Each fixture unit's freshness in a build's rows.
fn freshness(rows: &[(String, bool, Vec<String>)]) -> Vec<(String, bool)> {
    let mut seen: Vec<(String, bool)> = rows
        .iter()
        .map(|(name, fresh, _)| (name.clone(), *fresh))
        .collect();
    seen.sort();
    seen
}

/// A unit the first build put in the store: its folder relative to the store and its key, the
/// SHA-256 of its fingerprint record.
#[derive(Clone, Debug)]
struct HeldUnit {
    name: String,
    dir: String,
    key: String,
}

fn held_units(fixture: &Fixture, rows: &[(String, bool, Vec<String>)]) -> Vec<HeldUnit> {
    let store = fixture.store();
    let mut held: Vec<HeldUnit> = rows
        .iter()
        .map(|(name, _, files)| {
            let archive = files
                .iter()
                .find(|file| file.ends_with(".rlib"))
                .map(PathBuf::from)
                .expect("an archive");
            let unit = archive.parent().unwrap().parent().unwrap().to_path_buf();
            let dir = unit
                .strip_prefix(&store)
                .unwrap_or_else(|_| panic!("{} is in the shared store", unit.display()))
                .to_str()
                .unwrap()
                .to_owned();
            let record = std::fs::read(unit.join(format!("fingerprint/lib-{name}.json")))
                .expect("its fingerprint record");
            HeldUnit {
                name: name.clone(),
                dir,
                key: sha256_hex(&record),
            }
        })
        .collect();
    held.sort_by(|a, b| a.name.cmp(&b.name));
    assert_eq!(held.len(), 3, "{held:?}");
    held
}

/// Build in `wt1`, then move the store into the holder's own cargo home, the store its listener
/// serves, leaving an empty store at the builds' path.
fn hold(fixture: &Fixture, holder: &Instance) -> Vec<HeldUnit> {
    let first = fixture.build(&fixture.worktree("wt1"), holder, None);
    assert!(
        freshness(&first).iter().all(|(_, fresh)| !fresh),
        "{first:?}"
    );
    let held = held_units(fixture, &first);
    let served = holder.tmp.join("home/.cargo");
    std::fs::create_dir_all(&served).unwrap();
    std::fs::rename(fixture.cargo_home.join("trust"), served.join("trust"))
        .expect("move the store to the holder");
    held
}

/// The served store's copy of a unit.
fn served_unit(holder: &Instance, unit: &HeldUnit) -> PathBuf {
    store_in(&holder.tmp.join("home/.cargo")).join(&unit.dir)
}

fn node_of(inst: &Instance) -> String {
    std::fs::read_to_string(inst.tmp.join("state/fleet-node"))
        .expect("its node")
        .trim()
        .to_owned()
}

/// The rows of `inst`'s disk journal of `kind`.
fn journal_rows(inst: &Instance, kind: &str) -> Vec<Value> {
    std::fs::read_to_string(inst.tmp.join("state/harness/claude-harness/disk.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| aterm_json::from_str::<Value>(line).ok())
        .filter(|row| row.get("kind").and_then(Value::as_str) == Some(kind))
        .collect()
}

/// The connections `holder`'s listener answered for `node`.
fn served_to(holder: &Instance, node: &str) -> usize {
    journal_rows(holder, "unit-serve")
        .iter()
        .filter(|row| row.get("member").and_then(Value::as_str) == Some(node))
        .count()
}

/// The units `holder`'s listener sent `node`, over every connection it answered for it.
fn units_served_to(holder: &Instance, node: &str) -> u64 {
    journal_rows(holder, "unit-serve")
        .iter()
        .filter(|row| row.get("member").and_then(Value::as_str) == Some(node))
        .filter_map(|row| row.get("units").and_then(Value::as_u64))
        .sum()
}

/// Wait until `holder`'s listener has written the rows of the connections `node` made, which
/// together sent `units` units: it writes each once the source closed it (between a build's
/// asks, or at its end).
fn served(holder: &Instance, node: &str, units: u64) {
    let written = aterm_spec::wait::within(HANG, Duration::from_millis(20), || {
        (units_served_to(holder, node) >= units).then_some(())
    });
    assert!(
        written.is_some(),
        "the holder's rows for {node} name fewer than {units} units"
    );
    assert_eq!(units_served_to(holder, node), units);
}

/// What a source row says each member gave: (member, units, bytes).
fn taken_of(row: &Value) -> Vec<(String, u64, u64)> {
    row.get("taken")
        .and_then(Value::as_array)
        .map(|taken| {
            taken
                .iter()
                .filter_map(|entry| {
                    Some((
                        entry.get("member")?.as_str()?.to_owned(),
                        entry.get("units")?.as_u64()?,
                        entry.get("bytes")?.as_u64()?,
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// What a source row says was refused: (key, member, reason).
fn refused_of(row: &Value) -> Vec<(String, String, String)> {
    row.get("refused")
        .and_then(Value::as_array)
        .map(|refused| {
            refused
                .iter()
                .filter_map(|entry| {
                    Some((
                        entry.get("key")?.as_str()?.to_owned(),
                        entry.get("member")?.as_str()?.to_owned(),
                        entry.get("reason")?.as_str()?.to_owned(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The bytes of a unit's planned files as they stand in a store.
fn unit_bytes(unit: &Path) -> u64 {
    ["fingerprint", "out"]
        .iter()
        .flat_map(|folder| std::fs::read_dir(unit.join(folder)).into_iter().flatten())
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
        .map(|entry| entry.metadata().map_or(0, |meta| meta.len()))
        .sum()
}

/// The units a build compiled and those it took, by name: a taken unit is one it did not compile.
fn took_in_build(rows: &[(String, bool, Vec<String>)]) -> Vec<String> {
    freshness(rows)
        .into_iter()
        .filter_map(|(name, fresh)| fresh.then_some(name))
        .collect()
}

/// THE EXIT LANE: build `tree` as the taker's front starts targo, and the units it took. Its
/// own targo must have asked the source (the build's row is in the taker's journal).
fn build_taking(fixture: &Fixture, tree: &Path, taker: &Instance) -> Vec<String> {
    let rows = fixture.build(tree, taker, Fixture::front(taker));
    assert!(
        !journal_rows(taker, "unit-source").is_empty(),
        "this toolchain's targo never asked the unit source its front named (DC-24a): {rows:?}"
    );
    took_in_build(&rows)
}

/// THE STAND-IN LANE: ask the source the taker's front names for each of `held` as DC-24a's
/// targo asks it, into the empty store at the builds' path; the units placed byte for byte as
/// the holder holds them. None where the front names no source.
fn take_as_targo(
    fixture: &Fixture,
    holder: &Instance,
    taker: &Instance,
    held: &[HeldUnit],
) -> Vec<String> {
    let _ = std::fs::remove_dir_all(fixture.cargo_home.join("trust"));
    let Some((_, program)) = Fixture::front(taker) else {
        return Vec::new();
    };
    let placed = ask_as_targo(fixture, taker, Path::new(&program), held);
    held.iter()
        .zip(placed)
        .filter(|(unit, placed)| {
            *placed && {
                let here = fixture.store().join(&unit.dir);
                let there = served_unit(holder, unit);
                let archive = |unit: &Path| {
                    std::fs::read_dir(unit.join("out"))
                        .unwrap()
                        .flatten()
                        .find(|entry| entry.file_name().to_string_lossy().ends_with(".rlib"))
                        .map(|entry| std::fs::read(entry.path()).unwrap())
                };
                archive(&here).is_some() && archive(&here) == archive(&there)
            }
        })
        .map(|(unit, _)| unit.name.clone())
        .collect()
}

/// Ask the source at `program` for each of `held` as Trust's DC-24a targo asks: started once,
/// one JSON line per unit naming its key and a new private staging folder in the store, `working`
/// lines read past, and a `staged` unit placed ([`place`]). Whether each was placed.
fn ask_as_targo(
    fixture: &Fixture,
    taker: &Instance,
    program: &Path,
    held: &[HeldUnit],
) -> Vec<bool> {
    use std::os::unix::fs::DirBuilderExt as _;
    let store = fixture.store();
    let build = store.join("staging/test-build");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&build)
        .unwrap();
    let mut command = aterm_uds::spawnfd::command(program);
    command
        .env_clear()
        .envs(fixture.env(taker))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn().expect("start the unit source");
    let mut input = child.stdin.take().unwrap();
    let output = child.stdout.take().unwrap();
    let (lines, replies) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in std::io::BufReader::new(output).lines() {
            let Ok(line) = line else { break };
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    let mut placed = Vec::new();
    for (n, unit) in held.iter().enumerate() {
        let staging = build.join((n + 1).to_string());
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&staging)
            .unwrap();
        writeln!(
            input,
            "{{\"key\":\"{}\",\"staging\":\"{}\",\"version\":1}}",
            unit.key,
            staging.display()
        )
        .and_then(|()| input.flush())
        .expect("ask the source");
        let answer = loop {
            let line = replies
                .recv_timeout(HANG)
                .expect("the source answers or says it is working");
            if line != "working" {
                break line;
            }
        };
        assert!(answer == "staged" || answer == "none", "{answer:?}");
        placed.push(answer == "staged" && place(&staging, &store.join(&unit.dir), &unit.key));
    }
    drop(input);
    let ended = aterm_spec::wait::within(HANG, Duration::from_millis(20), || {
        child.try_wait().ok().flatten()
    });
    assert!(ended.is_some(), "the source did not end with its input");
    reader.join().unwrap();
    std::fs::remove_dir_all(&build).unwrap();
    placed
}

/// DC-24a's placement: the staged unit's record must have `key`, every staged file must be a
/// plain file with one link reached through plain folders; they are renamed into `unit` with the
/// store's mode (0644), and the unit is checked again as targo checks a placed unit (its record,
/// and each output its one receipt names, at its path in `store`, by size and SHA-256). A unit
/// that fails that check is removed again. `unit` is in the store at the builds' path, so a
/// receipt names its outputs there.
fn place(staged: &Path, unit: &Path, key: &str) -> bool {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let plain_dir = |dir: &Path| std::fs::symlink_metadata(dir).is_ok_and(|meta| meta.is_dir());
    if !["", "fingerprint", "out"]
        .iter()
        .all(|folder| plain_dir(&staged.join(folder)))
    {
        return false;
    }
    let mut files = Vec::new();
    for folder in ["fingerprint", "out"] {
        for entry in std::fs::read_dir(staged.join(folder)).unwrap().flatten() {
            let meta = std::fs::symlink_metadata(entry.path()).unwrap();
            if !meta.is_file() || meta.nlink() != 1 {
                return false;
            }
            files.push(format!("{folder}/{}", entry.file_name().to_string_lossy()));
        }
    }
    let is_record = |rel: &str| {
        rel.starts_with("fingerprint/")
            && rel.ends_with(".json")
            && !rel.starts_with("fingerprint/outputs-")
    };
    let recorded = |root: &Path| {
        files.iter().any(|rel| {
            is_record(rel) && std::fs::read(root.join(rel)).is_ok_and(|b| sha256_hex(&b) == key)
        })
    };
    if !recorded(staged) {
        return false;
    }
    for rel in &files {
        let to = unit.join(rel);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::rename(staged.join(rel), &to).unwrap();
        std::fs::set_permissions(&to, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    let receipt = files
        .iter()
        .find(|rel| rel.starts_with("fingerprint/outputs-"))
        .and_then(|rel| aterm_json::from_slice::<Value>(&std::fs::read(unit.join(rel)).ok()?).ok());
    let outputs_hold = receipt.is_some_and(|receipt| {
        receipt
            .get("outputs")
            .and_then(Value::as_array)
            .is_some_and(|outputs| {
                !outputs.is_empty()
                    && outputs.iter().all(|output| {
                        files.iter().any(|rel| {
                            let at = unit.join(rel);
                            Some(sha256_hex(at.as_os_str().as_encoded_bytes()).as_str())
                                == output.get("path_sha256").and_then(Value::as_str)
                                && std::fs::read(&at).is_ok_and(|bytes| {
                                    Some(bytes.len() as u64)
                                        == output.get("size").and_then(Value::as_u64)
                                        && Some(sha256_hex(&bytes).as_str())
                                            == output.get("sha256").and_then(Value::as_str)
                                })
                        })
                    })
            })
    });
    if !recorded(unit) || !outputs_hold {
        for rel in &files {
            let _ = std::fs::remove_file(unit.join(rel));
        }
        return false;
    }
    true
}

/// The receipt a receipt-writing compiler leaves for `unit` in the served store, written where
/// the holder's toolchain left none (one from before DC-24a): each `.rlib` and `.rmeta` of its
/// `out/`, by the SHA-256 of its path in the builds' store, its size and its SHA-256.
fn receipt_where_none(fixture: &Fixture, holder: &Instance, unit: &HeldUnit) {
    let served = served_unit(holder, unit);
    let written = std::fs::read_dir(served.join("fingerprint"))
        .unwrap()
        .flatten()
        .any(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name.starts_with("outputs-") && name.ends_with(".json")
        });
    if written {
        return;
    }
    let receipt = served.join(format!("fingerprint/outputs-lib-{}.json", unit.name));
    let mut outputs = Vec::new();
    let mut names: Vec<String> = std::fs::read_dir(served.join("out"))
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".rlib") || name.ends_with(".rmeta"))
        .collect();
    names.sort();
    for name in names {
        let bytes = std::fs::read(served.join("out").join(&name)).unwrap();
        let at = fixture.store().join(&unit.dir).join("out").join(&name);
        outputs.push(format!(
            "{{\"path_sha256\":\"{}\",\"size\":{},\"sha256\":\"{}\"}}",
            sha256_hex(at.as_os_str().as_encoded_bytes()),
            bytes.len(),
            sha256_hex(&bytes)
        ));
    }
    assert!(!outputs.is_empty(), "{} has outputs", unit.name);
    std::fs::write(&receipt, format!("{{\"outputs\":[{}]}}", outputs.join(","))).unwrap();
}

/// [`hold`], each held unit carrying a receipt ([`receipt_where_none`]).
fn hold_with_receipts(fixture: &Fixture, holder: &Instance) -> Vec<HeldUnit> {
    let held = hold(fixture, holder);
    for unit in &held {
        receipt_where_none(fixture, holder, unit);
    }
    held
}

/// The one source row the taker's build left: by targo's own ask, or by [`ask_as_targo`].
fn source_row(taker: &Instance) -> Value {
    let rows = journal_rows(taker, "unit-source");
    assert_eq!(rows.len(), 1, "{rows:?}");
    rows.into_iter().next().unwrap()
}

fn names(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

/// The taker's journal holds one row naming the holder, `units` units and their bytes (no more
/// than the holder holds), and no refusal.
fn took_from(holder: &Instance, taker: &Instance, held: &[HeldUnit], units: u64) {
    let row = source_row(taker);
    let bytes: u64 = held
        .iter()
        .map(|unit| unit_bytes(&served_unit(holder, unit)))
        .sum();
    let taken = taken_of(&row);
    assert_eq!(taken.len(), 1, "{row}");
    assert_eq!(taken[0].0, node_of(holder), "{row}");
    assert_eq!(taken[0].1, units, "{row}");
    assert!(
        taken[0].2 > 0 && taken[0].2 <= bytes,
        "{row}: {bytes} bytes held"
    );
    assert!(refused_of(&row).is_empty(), "{row}");
}

/// The archive of `unit` in the served store damaged after its compiler wrote it: its first 8
/// bytes overwritten, its size and write time kept (as Trust's tests/shared_dependencies.rs
/// damages one).
fn damage_archive(holder: &Instance, unit: &HeldUnit) {
    let served = served_unit(holder, unit);
    let archive = std::fs::read_dir(served.join("out"))
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "rlib"))
        .expect("its archive");
    let size = std::fs::metadata(&archive).unwrap().len();
    let written = std::fs::metadata(&archive).unwrap().modified().unwrap();
    {
        use std::io::{Seek as _, SeekFrom};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&archive)
            .unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(b"BROKEN!!").unwrap();
        file.set_modified(written).unwrap();
    }
    assert_eq!(std::fs::metadata(&archive).unwrap().len(), size);
}

/// The taker's journal names `unit` refused by `holder` for its receipt, and the rest taken.
fn refused_for_receipt(holder: &Instance, taker: &Instance, unit: &HeldUnit, taken: u64) {
    let row = source_row(taker);
    let took = taken_of(&row);
    assert_eq!(took.len(), 1, "{row}");
    assert_eq!(
        (took[0].0.as_str(), took[0].1),
        (node_of(holder).as_str(), taken),
        "{row}"
    );
    assert_eq!(
        refused_of(&row),
        [(unit.key.clone(), node_of(holder), "receipt".to_owned())],
        "{row}"
    );
}

/// The grant line in `taker`'s aterm.toml replaced by one naming only a third node: the holder
/// is then no member this Mac lets drive it.
fn grant_only_a_third_node(taker: &Instance) {
    let config = taker.tmp.join("cfg/aterm/aterm.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    let granted = text
        .lines()
        .find(|line| line.trim_start().starts_with("drive_from"))
        .expect("the member's grant")
        .to_owned();
    let third = format!("drive_from = [\"{}:{}\"]", "7".repeat(32), "8".repeat(64));
    std::fs::write(&config, text.replace(&granted, &third)).unwrap();
}

fn forget_rows(taker: &Instance) {
    let _ = std::fs::remove_file(taker.tmp.join("state/harness/claude-harness/disk.jsonl"));
}

/// DC-24b's exit: a Mac at Full Access whose grants admit the member that holds a, b and c takes
/// all three in its build, and its journal names that member, three units and their bytes.
#[test]
#[ignore = "DC-24b's exit: needs a trust.lock build whose targo asks the unit source (DC-24a)"]
fn a_mac_at_full_access_takes_every_library_unit_a_granted_member_holds() {
    let fixture = Fixture::new();
    let (holder, taker, _, _) = driven_pair(("dcuh", "dcut"));
    let held = hold(&fixture, &holder);
    let took = build_taking(&fixture, &fixture.worktree("wt2"), &taker);
    assert_eq!(took, names(&UNITS));
    took_from(&holder, &taker, &held, 3);
    served(&holder, &node_of(&taker), 3);
}

/// DC-24b's exit: a unit whose archive was damaged on its holder's disk is refused by the
/// receipt it carries and compiled here; the rest are taken.
#[test]
#[ignore = "DC-24b's exit: needs a trust.lock build whose targo asks the unit source (DC-24a)"]
fn a_unit_damaged_on_its_holders_disk_is_compiled_here_and_the_rest_taken() {
    let fixture = Fixture::new();
    let (holder, taker, _, _) = driven_pair(("dcdh", "dcdt"));
    let held = hold(&fixture, &holder);
    let c = held.iter().find(|unit| unit.name == "c").unwrap().clone();
    damage_archive(&holder, &c);
    let took = build_taking(&fixture, &fixture.worktree("wt2"), &taker);
    assert_eq!(took, names(&["a", "b"]));
    refused_for_receipt(&holder, &taker, &c, 2);
}

/// DC-24b's exit: at Read Only the Mac names no source and its build asks no member; at Full
/// Access with a grant naming only a third node it asks none either; with the grant raised to
/// every member its build takes all three. Each phase starts from an emptied store.
#[test]
#[ignore = "DC-24b's exit: needs a trust.lock build whose targo asks the unit source (DC-24a)"]
fn a_mac_asks_no_member_below_full_access_or_outside_its_grants() {
    let fixture = Fixture::new();
    let (holder, taker, _, _) = driven_pair(("dcah", "dcat"));
    let _held = hold(&fixture, &holder);
    let taker_node = node_of(&taker);

    fleet_ok(&taker.tmp, &["access", "read"], None);
    assert!(
        Fixture::front(&taker).is_none(),
        "Read Only names no source"
    );
    let rows = fixture.build(&fixture.worktree("wt2"), &taker, Fixture::front(&taker));
    assert!(took_in_build(&rows).is_empty(), "{rows:?}");
    assert!(journal_rows(&taker, "unit-source").is_empty());
    assert_eq!(served_to(&holder, &taker_node), 0);

    fleet_ok(&taker.tmp, &["access", "full"], None);
    grant_only_a_third_node(&taker);
    let _ = std::fs::remove_dir_all(fixture.cargo_home.join("trust"));
    let outside = build_taking(&fixture, &fixture.worktree("wt3"), &taker);
    assert!(outside.is_empty());
    assert_eq!(served_to(&holder, &taker_node), 0);
    let row = source_row(&taker);
    assert!(
        taken_of(&row).is_empty() && refused_of(&row).is_empty(),
        "{row}"
    );
    forget_rows(&taker);

    fleet_ok(&taker.tmp, &["allow", "drive", "--all"], None);
    let _ = std::fs::remove_dir_all(fixture.cargo_home.join("trust"));
    let all = build_taking(&fixture, &fixture.worktree("wt4"), &taker);
    assert_eq!(all, names(&UNITS));
    assert_eq!(taken_of(&source_row(&taker))[0].1, 3);
    served(&holder, &taker_node, 3);
}

/// The stand-in: the source a Full Access Mac's front names stages every unit a granted member
/// holds, each placed byte for byte, and its journal names that member, three units and their
/// bytes; the holder answered one connection.
#[test]
fn the_source_stages_every_unit_a_granted_member_holds() {
    let fixture = Fixture::new();
    let (holder, taker, _, _) = driven_pair(("dcsh", "dcst"));
    let held = hold_with_receipts(&fixture, &holder);
    let took = take_as_targo(&fixture, &holder, &taker, &held);
    assert_eq!(
        took,
        names(&UNITS),
        "{:?}",
        journal_rows(&taker, "unit-source")
    );
    took_from(&holder, &taker, &held, 3);
    served(&holder, &node_of(&taker), 3);
}

/// The stand-in: a unit whose archive was damaged on its holder's disk is refused by its
/// receipt, and the rest are staged.
#[test]
fn the_source_refuses_a_unit_damaged_on_its_holders_disk_and_stages_the_rest() {
    let fixture = Fixture::new();
    let (holder, taker, _, _) = driven_pair(("dcrh", "dcrt"));
    let held = hold_with_receipts(&fixture, &holder);
    let c = held.iter().find(|unit| unit.name == "c").unwrap().clone();
    damage_archive(&holder, &c);
    let took = take_as_targo(&fixture, &holder, &taker, &held);
    assert_eq!(
        took,
        names(&["a", "b"]),
        "{:?}",
        journal_rows(&taker, "unit-source")
    );
    refused_for_receipt(&holder, &taker, &c, 2);
}

/// What the taker's build front names targo, run as the hop runs it (`atpkg __build_admit
/// <targo>`, the `aterm` image under the name `atpkg`) on a stand-in targo that writes what it
/// was given in `TRUST_TARGO_UNIT_SOURCE`: the program, or none.
fn front_names(fixture: &Fixture, taker: &Instance) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = fixture.root.join("front");
    std::fs::create_dir_all(&dir).unwrap();
    let atpkg = dir.join("atpkg");
    if !atpkg.exists() {
        std::os::unix::fs::symlink(crate::launch_isolation::aterm_executable(), &atpkg).unwrap();
    }
    let targo = dir.join("targo");
    std::fs::write(
        &targo,
        "#!/bin/sh\nprintf 'source=%s\\n' \"${TRUST_TARGO_UNIT_SOURCE-}\" > \"$FRONT_OUT\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&targo, std::fs::Permissions::from_mode(0o755)).unwrap();
    let said = dir.join("said");
    let _ = std::fs::remove_file(&said);
    std::fs::create_dir_all(dir.join("ledger")).unwrap();
    let mut command = aterm_uds::spawnfd::command(&atpkg);
    command
        .args([std::ffi::OsStr::new("__build_admit"), targo.as_os_str()])
        .env_clear()
        .envs(fixture.env(taker))
        .env(atpkg::build_admit::LEDGER_ENV, dir.join("ledger"))
        .env("FRONT_OUT", &said)
        .current_dir(&dir);
    let output = aterm_uds::deadline::output_while_process_progressing_complete_stdout_with_limit(
        &mut command,
        HANG,
        "the build front",
        64 * 1024,
    )
    .expect("the front ran");
    assert!(output.status.success(), "{output:?}");
    let said = std::fs::read_to_string(&said).expect("the stand-in targo ran");
    let named = said.trim_end().strip_prefix("source=").expect("its line");
    (!named.is_empty()).then(|| PathBuf::from(named))
}

/// The stand-in: at Read Only the front names targo no source and the source itself, run anyway,
/// asks no member; at Full Access the front names the source (an `aterm-unit-source` link to the
/// `aterm` it runs as) but with a grant naming only a third node it asks none; with the grant
/// raised to every member it stages all three. Each phase starts from an emptied store.
#[test]
fn the_source_asks_no_member_below_full_access_or_outside_its_grants() {
    let fixture = Fixture::new();
    let (holder, taker, _, _) = driven_pair(("dcoh", "dcot"));
    let held = hold_with_receipts(&fixture, &holder);
    let taker_node = node_of(&taker);

    fleet_ok(&taker.tmp, &["access", "full"], None);
    let named = front_names(&fixture, &taker).expect("Full Access names a source");
    assert_eq!(
        named.file_name().unwrap(),
        atpkg::build_cache::unit_source::LINK
    );
    assert_eq!(
        std::fs::canonicalize(&named).unwrap(),
        crate::launch_isolation::aterm_executable()
    );
    assert_eq!(
        Fixture::front(&taker).map(|(_, link)| PathBuf::from(link)),
        Some(named.clone())
    );

    fleet_ok(&taker.tmp, &["access", "read"], None);
    assert_eq!(
        front_names(&fixture, &taker),
        None,
        "Read Only names no source"
    );
    assert!(take_as_targo(&fixture, &holder, &taker, &held).is_empty());
    // The source itself, started anyway, asks no member below Full Access.
    let _ = std::fs::remove_dir_all(fixture.cargo_home.join("trust"));
    assert!(
        ask_as_targo(&fixture, &taker, &named, &held)
            .iter()
            .all(|placed| !placed)
    );
    assert_eq!(served_to(&holder, &taker_node), 0);
    let row = source_row(&taker);
    assert!(
        taken_of(&row).is_empty() && refused_of(&row).is_empty(),
        "{row}"
    );
    forget_rows(&taker);

    // A grant naming only a third node: the holder is no member this Mac lets drive it.
    fleet_ok(&taker.tmp, &["access", "full"], None);
    grant_only_a_third_node(&taker);
    assert!(take_as_targo(&fixture, &holder, &taker, &held).is_empty());
    assert_eq!(served_to(&holder, &taker_node), 0);
    let row = source_row(&taker);
    assert!(
        taken_of(&row).is_empty() && refused_of(&row).is_empty(),
        "{row}"
    );
    forget_rows(&taker);

    fleet_ok(&taker.tmp, &["allow", "drive", "--all"], None);
    assert_eq!(
        take_as_targo(&fixture, &holder, &taker, &held),
        names(&UNITS),
        "{:?}",
        journal_rows(&taker, "unit-source")
    );
    assert_eq!(taken_of(&source_row(&taker))[0].1, 3);
    served(&holder, &taker_node, 3);
}
