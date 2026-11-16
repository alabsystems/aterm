// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::path::PathBuf;
use std::time::Duration;

use super::*;

const COMMIT: &str = "1a37de31a3401234567890abcdef1234567890ab";
const TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
const ASSET: &str = "aterm-0.200.0-windows-x86_64.zip";

/// An edit that makes a good record a bad one.
type Change<T> = Box<dyn Fn(&mut T)>;
/// An edit that makes a laid-out handoff a bad one.
type Spoil = Box<dyn Fn(&Path)>;

fn request() -> Request {
    Request {
        version: "0.200.0".to_string(),
        build: 1_794_421_328,
        commit: COMMIT.to_string(),
        tree: TREE.to_string(),
        reserved_at: "2026-11-15T03:04:05Z".to_string(),
    }
}

#[test]
fn the_refs_and_names_are_the_wire_both_ends_use() {
    assert_eq!(REQUEST_REF, "refs/aterm-release/windows/request");
    assert_eq!(ALIVE_REF, "refs/aterm-release/windows/alive");
    assert_eq!(
        handoff_ref("0.200.0", 1_794_421_328, COMMIT),
        "refs/aterm-release/windows/handoff/0.200.0-1794421328-1a37de31a340"
    );
    assert_eq!(
        request().handoff_ref(),
        handoff_ref("0.200.0", 1_794_421_328, COMMIT)
    );
    assert_eq!(attestation_name(ASSET), format!("{ASSET}.handoff.sig"));
    assert_eq!(part_name(ASSET, 0), format!("{ASSET}.part000"));
    assert_eq!(part_name(ASSET, 11), format!("{ASSET}.part011"));
    assert_eq!(PART_MAX_BYTES, 45 * 1024 * 1024);
    assert_eq!(BUILDERS_FILE, "apps/aterm-win/release-builders.toml");
    assert_eq!(ATTESTATION_DOMAIN, b"aterm-windows-handoff-attestation/1\n");
    assert_eq!(max_parts(), 12);
}

#[test]
fn times_and_dates_are_read_as_they_are_written() {
    for secs in [0, 951_782_400, 1_794_424_462, 4_102_444_799] {
        let text = crate::pointer::rfc3339(secs);
        assert_eq!(rfc3339_secs(&text), Some(secs), "{text}");
        assert_eq!(date_days(&date_of(secs)), Some(secs / 86_400), "{text}");
    }
    assert_eq!(date_of(1_794_424_462), "2026-11-11");
    assert_eq!(date_days("2000-02-29"), Some(11_016));
    for bad in [
        "2026-02-29",
        "2100-02-29",
        "2026-04-31",
        "2026-00-10",
        "2026-13-01",
        "1969-12-31",
        "2026-1-01",
        "2026/01/01",
        "",
    ] {
        assert_eq!(date_days(bad), None, "{bad:?}");
    }
    for bad in [
        "2026-11-15T24:00:00Z",
        "2026-11-15T03:60:00Z",
        "2026-11-15T03:04:05",
        "2026-11-15 03:04:05Z",
        "2026-11-15T03:04:05+00:00",
        "2026-02-30T03:04:05Z",
    ] {
        assert_eq!(rfc3339_secs(bad), None, "{bad:?}");
    }
}

#[test]
fn a_request_round_trips_and_refuses_what_names_no_release() {
    let good = request();
    let text = good.to_toml().expect("toml");
    assert_eq!(Request::parse(&text), Ok(good.clone()));
    assert_eq!(good.key(), "0.200.0-1794421328-1a37de31a340");
    assert_eq!(good.reserved_secs(), rfc3339_secs("2026-11-15T03:04:05Z"));
    let cases: [(&str, Change<Request>); 7] = [
        ("version", Box::new(|r| r.version = "v0.200.0".to_string())),
        ("build", Box::new(|r| r.build = 0)),
        (
            "short commit",
            Box::new(|r| r.commit = COMMIT[..12].to_string()),
        ),
        (
            "upper commit",
            Box::new(|r| r.commit = COMMIT.to_uppercase()),
        ),
        ("tree", Box::new(|r| r.tree = "xyz".to_string())),
        (
            "time",
            Box::new(|r| r.reserved_at = "2026-11-15".to_string()),
        ),
        (
            "zone",
            Box::new(|r| r.reserved_at = "2026-11-15T03:04:05+01:00".to_string()),
        ),
    ];
    for (what, change) in cases {
        let mut bad = good.clone();
        change(&mut bad);
        assert!(bad.check().is_err(), "{what}");
        assert!(
            Request::parse(&bad.to_toml().expect("toml")).is_err(),
            "{what}"
        );
    }
    assert!(Request::parse(&format!("{text}extra = 1\n")).is_err());
    assert!(Request::parse(&" ".repeat(RECORD_MAX_BYTES as usize + 1)).is_err());
    // A 64-digit tree (a SHA-256 repository's) is a tree.
    let mut sha256 = good;
    sha256.tree = "a".repeat(64);
    assert_eq!(sha256.check(), Ok(()));
}

#[test]
fn a_heartbeat_is_fresh_within_its_bound_either_way() {
    let alive = Alive {
        builder: "m6".to_string(),
        at: "2026-11-15T03:04:05Z".to_string(),
        request: None,
        passed_over: None,
    };
    assert_eq!(
        Alive::parse(&alive.to_toml().expect("toml")),
        Ok(alive.clone())
    );
    let at = alive.at_secs().expect("at");
    assert!(alive.fresh(at, 0));
    assert!(alive.fresh(at + 1800, 1800));
    assert!(!alive.fresh(at + 1801, 1800));
    // A clock running ahead does not keep it fresh.
    assert!(alive.fresh(at - 1800, 1800));
    assert!(!alive.fresh(at - 1801, 1800));
    assert!(Alive::parse("builder = \"m6\"\nat = \"yesterday\"\n").is_err());
    assert!(Alive::parse("builder = \"m6\"\nat = \"2026-11-15T03:04:05Z\"\nx = 1\n").is_err());
}

/// A HEARTBEAT'S BUILDER IS A BUILDER ID AND ITS REQUEST A COMMIT: anyone who can write origin's
/// refs writes the heartbeat and the cut prints its builder, so an id that carries a terminal
/// escape (or anything but an id) refuses the record; a request it names is a git object id,
/// and what it says it passed over is said only of that request.
/// NEGATIVE CONTROL: a heartbeat read by its time alone put a forged builder's escape sequences
/// into the release operator's terminal.
#[test]
fn a_heartbeats_builder_is_an_id_and_its_request_a_commit() {
    let request = "c".repeat(40);
    let alive = Alive {
        builder: "m6".to_string(),
        at: "2026-11-15T03:04:05Z".to_string(),
        request: Some(request.clone()),
        passed_over: Some(PassedOver::Spent),
    };
    let text = alive.to_toml().expect("toml");
    assert!(text.contains("passed_over = \"spent\""), "{text}");
    assert_eq!(Alive::parse(&text), Ok(alive.clone()));
    assert_eq!(alive.passed_over(&request), Some(PassedOver::Spent));
    assert_eq!(alive.passed_over(&"d".repeat(40)), None);
    let building = Alive {
        passed_over: None,
        ..alive.clone()
    };
    assert_eq!(building.passed_over(&request), None);
    for builder in [
        "m6\u{1b}]52;c;aGk=\u{7}",
        "m6\u{1b}[2J",
        "M6",
        "",
        "m6 builder",
    ] {
        let forged = Alive {
            builder: builder.to_string(),
            ..alive.clone()
        };
        assert!(
            Alive::parse(&forged.to_toml().expect("toml")).is_err(),
            "{builder:?}"
        );
    }
    for request in ["C".repeat(40), "c".repeat(39), "main".to_string()] {
        let forged = Alive {
            request: Some(request.clone()),
            ..alive.clone()
        };
        assert!(
            Alive::parse(&forged.to_toml().expect("toml")).is_err(),
            "{request}"
        );
    }
    assert!(
        Alive::parse(&text.replace("\"spent\"", "\"\\u001b[2J\"")).is_err(),
        "a reason is one of the fixed words"
    );
}

fn table(size: u64) -> Parts {
    let mut parts = Vec::new();
    let mut left = size;
    while left > 0 {
        let take = left.min(PART_MAX_BYTES);
        parts.push(Part {
            name: part_name(ASSET, parts.len()),
            size: take,
        });
        left -= take;
    }
    Parts {
        schema: PARTS_SCHEMA,
        zip: ASSET.to_string(),
        size,
        sha256: "a".repeat(64),
        part: parts,
    }
}

#[test]
fn a_parts_table_is_held_to_its_shape() {
    for size in [
        64,
        PART_MAX_BYTES,
        PART_MAX_BYTES + 1,
        3 * PART_MAX_BYTES + 7,
    ] {
        let good = table(size);
        assert_eq!(good.check(), Ok(()), "{size}");
        assert_eq!(Parts::parse(&good.to_toml().expect("toml")), Ok(good));
    }
    let base = table(2 * PART_MAX_BYTES + 5);
    let cases: [(&str, Change<Parts>); 10] = [
        ("schema", Box::new(|p| p.schema = 2)),
        (
            "zip name",
            Box::new(|p| p.zip = "aterm-0.200.0-x64.zip".to_string()),
        ),
        (
            "zip version",
            Box::new(|p| p.zip = "aterm-v1-windows-x86_64.zip".to_string()),
        ),
        ("digest", Box::new(|p| p.sha256 = "A".repeat(64))),
        ("no parts", Box::new(|p| p.part.clear())),
        (
            "a part's name",
            Box::new(|p| p.part[1].name = part_name(ASSET, 2)),
        ),
        (
            "a short middle part",
            Box::new(|p| {
                p.part[0].size -= 1;
                p.part[2].size += 1;
            }),
        ),
        (
            "an empty part",
            Box::new(|p| {
                p.part.push(Part {
                    name: part_name(ASSET, 3),
                    size: 0,
                })
            }),
        ),
        ("the sum", Box::new(|p| p.size += 1)),
        (
            "too small",
            Box::new(|p| {
                *p = table(63);
            }),
        ),
    ];
    for (what, change) in cases {
        let mut bad = base.clone();
        change(&mut bad);
        assert!(bad.check().is_err(), "{what}");
    }
    // Every name a handoff holds, in order.
    assert_eq!(
        table(64).names(),
        [
            part_name(ASSET, 0),
            PARTS_FILE.to_string(),
            format!("{ASSET}.handoff.toml"),
            format!("{ASSET}.handoff.sig"),
        ]
    );
}

/// `size` bytes no two parts share a run of.
fn bytes(size: usize) -> Vec<u8> {
    (0..size)
        .map(|i| (i % 251) as u8 ^ (i / 251 % 256) as u8)
        .collect()
}

/// A handoff of `zip` laid out in a new directory, with a receipt and an attestation.
fn laid_out(zip: &[u8]) -> (aterm_tempfile::TempDir, PathBuf, Parts) {
    let scratch = aterm_tempfile::tempdir().expect("tempdir");
    let from = scratch.path().join("from");
    let parts_dir = scratch.path().join("parts");
    for dir in [&from, &parts_dir] {
        std::fs::create_dir(dir).expect("dir");
    }
    std::fs::write(from.join(ASSET), zip).expect("zip");
    let parts = split(&from.join(ASSET), &parts_dir, PART_MAX_BYTES).expect("split");
    std::fs::write(
        parts_dir.join(format!("{ASSET}.handoff.toml")),
        b"receipt\r\n",
    )
    .expect("r");
    std::fs::write(parts_dir.join(attestation_name(ASSET)), b"sig\n").expect("s");
    (scratch, parts_dir, parts)
}

#[test]
fn a_zip_split_into_parts_is_put_back_together_byte_for_byte() {
    // A whole part and one byte: the split at its edge.
    let zip = bytes(PART_MAX_BYTES as usize + 1);
    let (scratch, parts_dir, parts) = laid_out(&zip);
    assert_eq!(parts.part.len(), 2);
    assert_eq!(parts.part[0].size, PART_MAX_BYTES);
    assert_eq!(parts.part[1].size, 1);
    assert_eq!(parts.sha256, crate::handoff::fixture::sha256(&zip));
    assert_eq!(parts.size, zip.len() as u64);
    let carry = scratch.path().join("carry");
    std::fs::create_dir(&carry).expect("dir");
    assert_eq!(assemble(&parts_dir, &carry), Ok(parts));
    assert!(std::fs::read(carry.join(ASSET)).expect("zip") == zip);
    // The receipt and the attestation are copied as they are, line endings and all.
    assert_eq!(
        std::fs::read(carry.join(format!("{ASSET}.handoff.toml"))).expect("receipt"),
        b"receipt\r\n"
    );
    let mut names: Vec<String> = std::fs::read_dir(&carry)
        .expect("carry")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            ASSET.to_string(),
            format!("{ASSET}.handoff.sig"),
            format!("{ASSET}.handoff.toml")
        ]
    );
}

#[test]
fn parts_that_are_not_the_tables_are_refused() {
    let zip = bytes(4096);
    let cases: [(&str, Spoil); 5] = [
        (
            "a changed byte",
            Box::new(|dir| {
                let path = dir.join(part_name(ASSET, 0));
                let mut part = std::fs::read(&path).expect("part");
                part[100] ^= 1;
                std::fs::write(&path, part).expect("write");
            }),
        ),
        (
            "a short part",
            Box::new(|dir| {
                let path = dir.join(part_name(ASSET, 0));
                let part = std::fs::read(&path).expect("part");
                std::fs::write(&path, &part[1..]).expect("write");
            }),
        ),
        (
            "a missing part",
            Box::new(|dir| std::fs::remove_file(dir.join(part_name(ASSET, 0))).expect("rm")),
        ),
        (
            "another file",
            Box::new(|dir| std::fs::write(dir.join("notes.txt"), b"x").expect("write")),
        ),
        (
            "no attestation",
            Box::new(|dir| std::fs::remove_file(dir.join(attestation_name(ASSET))).expect("rm")),
        ),
    ];
    for (what, change) in cases {
        let (scratch, parts_dir, _) = laid_out(&zip);
        change(&parts_dir);
        let carry = scratch.path().join("carry");
        std::fs::create_dir(&carry).expect("dir");
        assert!(assemble(&parts_dir, &carry).is_err(), "{what} was taken");
    }
    // Control: untouched, it is taken.
    let (scratch, parts_dir, _) = laid_out(&zip);
    let carry = scratch.path().join("carry");
    std::fs::create_dir(&carry).expect("dir");
    assert!(assemble(&parts_dir, &carry).is_ok());
}

/// Runs git in `dir`, isolated from this machine's configuration; its standard output.
fn fixture_git(dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = aterm_uds::deadline::output_within(
        aterm_uds::fixture_git::git().arg("-C").arg(dir).args(args),
        Duration::from_secs(60),
    )
    .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

/// A repository holding a commit of `files` (path, bytes, executable) with `parent`.
fn commit_of(dir: &Path, files: &[(&str, &[u8], bool)], parent: Option<&str>) -> String {
    let index = dir.join("fixture.index");
    let _ = std::fs::remove_file(&index);
    for (name, data, exec) in files {
        let blob = dir.join("blob.tmp");
        std::fs::write(&blob, data).expect("blob");
        let oid = String::from_utf8(
            fixture_git(
                dir,
                &["hash-object", "-w", "--no-filters", &blob.to_string_lossy()],
            )
            .expect("hash"),
        )
        .expect("utf8");
        let mode = if *exec { "100755" } else { "100644" };
        let out = aterm_uds::deadline::output_within(
            aterm_uds::fixture_git::git()
                .arg("-C")
                .arg(dir)
                .args([
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    &format!("{mode},{},{name}", oid.trim()),
                ])
                .env("GIT_INDEX_FILE", &index),
            Duration::from_secs(60),
        )
        .expect("git");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let out = aterm_uds::deadline::output_within(
        aterm_uds::fixture_git::git()
            .arg("-C")
            .arg(dir)
            .arg("write-tree")
            .env("GIT_INDEX_FILE", &index),
        Duration::from_secs(60),
    )
    .expect("git");
    let tree = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let mut args = vec![
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t.invalid",
        "commit-tree",
        &tree,
        "-m",
        "t",
    ];
    if let Some(parent) = parent {
        args.extend(["-p", parent]);
    }
    String::from_utf8(fixture_git(dir, &args).expect("commit"))
        .expect("utf8")
        .trim()
        .to_string()
}

#[test]
fn a_handoff_commit_is_written_out_as_its_exact_bytes() {
    let scratch = aterm_tempfile::tempdir().expect("tempdir");
    let repo = scratch.path().join("repo");
    std::fs::create_dir(&repo).expect("dir");
    fixture_git(&repo, &["init", "--quiet"]).expect("init");
    let receipt: &[u8] = b"a = 1\r\nb = 2\n";
    let good = commit_of(
        &repo,
        &[
            (PARTS_FILE, b"schema = 1\n", false),
            (
                "aterm-0.200.0-windows-x86_64.zip.part000",
                &[0, 1, 2, 255],
                false,
            ),
            (
                "aterm-0.200.0-windows-x86_64.zip.handoff.toml",
                receipt,
                false,
            ),
        ],
        None,
    );
    let mut git = |args: &[&str]| fixture_git(&repo, args);
    let into = scratch.path().join("into");
    std::fs::create_dir(&into).expect("dir");
    let mut names = extract(&mut git, &good, &into).expect("extracted");
    names.sort();
    assert_eq!(names.len(), 3);
    assert_eq!(
        std::fs::read(into.join("aterm-0.200.0-windows-x86_64.zip.handoff.toml")).expect("r"),
        receipt
    );
    assert_eq!(
        std::fs::read(into.join("aterm-0.200.0-windows-x86_64.zip.part000")).expect("p"),
        [0, 1, 2, 255]
    );
    // Negative controls: a commit with a parent, an executable, a folder.
    let child = commit_of(&repo, &[(PARTS_FILE, b"x", false)], Some(&good));
    let exec = commit_of(&repo, &[(PARTS_FILE, b"x", true)], None);
    let nested = commit_of(&repo, &[("dir/parts.toml", b"x", false)], None);
    for (n, (what, commit)) in [
        ("a parent", child),
        ("an executable", exec),
        ("a folder", nested),
    ]
    .into_iter()
    .enumerate()
    {
        let into = scratch.path().join(format!("into-{n}"));
        std::fs::create_dir(&into).expect("dir");
        let mut git = |args: &[&str]| fixture_git(&repo, args);
        assert!(
            extract(&mut git, &commit, &into).is_err(),
            "{what} was taken"
        );
    }
}
