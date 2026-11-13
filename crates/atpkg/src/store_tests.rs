// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn temp_home(label: &str) -> crate::testdir::Dir {
    crate::testdir::dir(&format!("atpkg-store-{label}-"))
}

#[test]
fn record_writers_preserve_occupied_staging_paths_and_their_linked_bytes() {
    for kind in ["ready", "sidecar", "refusal", "environment"] {
        let home = temp_home(&format!("record-staging-{kind}"));
        let build = home.join("store/tool/7");
        std::fs::create_dir_all(&build).unwrap();
        let parent = build.parent().unwrap();
        let stem = match kind {
            "ready" => ".ready",
            "sidecar" => EXPOSES_SUFFIX,
            "refusal" => ".refused",
            "environment" => ".shim-env",
            _ => unreachable!(),
        };
        let occupied = parent.join(format!("{stem}.tmp-{}", std::process::id()));
        let unrelated = home.join("unrelated-record");
        std::fs::write(&unrelated, b"another writer's bytes").unwrap();
        std::fs::hard_link(&unrelated, &occupied).unwrap();
        let destination = match kind {
            "ready" => {
                mark_build_ready_on(&build, None).unwrap();
                assert!(build_is_complete(&build));
                ready_marker_path(&build).unwrap()
            }
            "sidecar" => {
                write_sidecar_durably(&build, EXPOSES_SUFFIX, b"sidecar contents").unwrap();
                sidecar_path(&build, EXPOSES_SUFFIX).unwrap()
            }
            "refusal" => {
                record_refusal_in(
                    MemoLane::Named,
                    RefusalKind::Held,
                    &build,
                    (DigestAlg::Sha256, &"a".repeat(64)),
                    &"b".repeat(64),
                    "fixture refusal",
                    123,
                )
                .unwrap();
                stage_refusal_path(&build).unwrap()
            }
            "environment" => {
                let environment =
                    crate::shim_env::ShimEnv::admit(&["LEAN_CC=/usr/bin/cc".into()]).unwrap();
                crate::shim_env::write_sidecar(&build, &environment).unwrap();
                crate::shim_env::sidecar_path(&build).unwrap()
            }
            _ => unreachable!(),
        };
        assert!(destination.is_file(), "{kind}");
        assert_eq!(
            std::fs::read(&occupied).unwrap(),
            b"another writer's bytes",
            "{kind}"
        );
        assert_eq!(
            std::fs::read(&unrelated).unwrap(),
            b"another writer's bytes",
            "{kind}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
            let control = home.join("prior-creation-policy");
            drop(
                std::fs::OpenOptions::new()
                    .mode(0o644)
                    .create_new(true)
                    .write(true)
                    .open(&control)
                    .unwrap(),
            );
            assert_eq!(
                std::fs::metadata(destination).unwrap().permissions().mode() & 0o777,
                std::fs::metadata(control).unwrap().permissions().mode() & 0o777,
                "{kind}"
            );
        }
    }
}

/// THE VERSION RECORD READS BACK WHAT WAS WRITTEN AND NOTHING ELSE: a one-word version
/// round-trips (the positive control); an empty or multi-word version writes no record; a
/// record with a foreign header, a second line or a space in its word reads as none; a
/// rewrite replaces the word.
#[test]
fn a_version_record_round_trips_one_word_and_refuses_the_rest() {
    let h = temp_home("version-record");
    let build = h.join("store").join("grok-alab").join("17");
    std::fs::create_dir_all(&build).unwrap();
    assert_eq!(recorded_version(&build), None, "no record yet");
    write_version(&build, "0.3.0").unwrap();
    assert_eq!(recorded_version(&build).as_deref(), Some("0.3.0"));
    let record = sidecar_path(&build, VERSION_SUFFIX).unwrap();
    assert_eq!(
        std::fs::read_to_string(&record).unwrap(),
        "version v1\n0.3.0\n"
    );
    write_version(&build, "0.4.0+g1a2b3c4").unwrap();
    assert_eq!(recorded_version(&build).as_deref(), Some("0.4.0+g1a2b3c4"));
    for refused in ["", "0.4 beta", "0.4\n0.5", &"9".repeat(MAX_VERSION_LEN + 1)] {
        write_version(&build, refused).unwrap();
        assert_eq!(
            recorded_version(&build).as_deref(),
            Some("0.4.0+g1a2b3c4"),
            "{refused:?} is not recorded"
        );
    }
    let other = h.join("store").join("grok-alab").join("18");
    std::fs::create_dir_all(&other).unwrap();
    write_version(&other, "").unwrap();
    assert!(!sidecar_path(&other, VERSION_SUFFIX).unwrap().exists());
    for body in [
        "exposes v1\n0.3.0\n",
        "version v1\n0.3.0\nextra\n",
        "version v1\n0.3 0\n",
        "version v1\n",
        "",
    ] {
        std::fs::write(&record, body).unwrap();
        assert_eq!(recorded_version(&build), None, "{body:?}");
    }
    std::fs::write(&record, "version v1\n1.2.3").unwrap();
    assert_eq!(
        recorded_version(&build).as_deref(),
        Some("1.2.3"),
        "a record with no final newline still reads (control)"
    );
}

#[test]
fn ensure_agents_dir_creates_a_real_directory_once_and_refuses_a_link_or_a_file() {
    let h = temp_home("ensure-agents");
    let l = Layout {
        prefix: h.join("pkg"),
    };
    assert!(!l.agents_dir().exists(), "a fresh prefix has no agents/");
    let dir = l.ensure_agents_dir().expect("created");
    assert_eq!(dir, l.agents_dir());
    assert!(dir.is_absolute());
    assert!(
        std::fs::symlink_metadata(&dir).unwrap().is_dir(),
        "a real directory"
    );
    #[cfg(unix)]
    assert_eq!(
        std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
        0o700,
        "a $HOME prefix's directory is private"
    );
    assert_eq!(l.ensure_agents_dir().as_deref(), Ok(dir.as_path()));
    let filed = Layout {
        prefix: h.join("filed"),
    };
    std::fs::create_dir_all(&filed.prefix).unwrap();
    std::fs::write(filed.agents_dir(), b"not a dir").unwrap();
    filed.ensure_agents_dir().expect_err("a file is refused");
    assert!(filed.agents_dir().is_file(), "left alone");
    #[cfg(unix)]
    {
        let linked = Layout {
            prefix: h.join("linked"),
        };
        std::fs::create_dir_all(&linked.prefix).unwrap();
        std::os::unix::fs::symlink(&dir, linked.agents_dir()).unwrap();
        assert!(linked.agents_dir().is_dir(), "the link resolves");
        linked.ensure_agents_dir().expect_err("a link is refused");
        assert!(
            std::fs::symlink_metadata(linked.agents_dir())
                .unwrap()
                .file_type()
                .is_symlink(),
            "left alone"
        );
    }
    let _ = std::fs::remove_dir_all(&h);
}

#[test]
fn the_declared_mode_record_round_trips_and_goes_with_the_build() {
    let h = temp_home("declared-modes");
    let l = Layout { prefix: h.clone() };
    let build = l.build_dir("claude", 2026092201);
    std::fs::create_dir_all(build.join("bin")).unwrap();
    let mut modes = crate::tree::DeclaredModes::new();
    modes.insert(b"bin/claude.exe".to_vec(), 0o755);
    modes.insert(b"lib/data".to_vec(), 0o644);
    write_declared_modes(&build, &modes).unwrap();
    let sidecar = build.with_file_name("2026092201.modes");
    if crate::platform::HAS_POSIX_MODES {
        assert!(
            !sidecar.exists(),
            "Unix reads the inode: no record is written"
        );
        assert!(declared_modes(&build).unwrap().is_empty());
    } else {
        assert!(sidecar.is_file(), "a sibling, outside the tree");
        assert_eq!(
            std::fs::read(&sidecar).unwrap(),
            b"declared-modes v1\nbin/claude.exe\x00755\nlib/data\x00644\n".to_vec(),
            "the line framing is the tree line's, minus its digest"
        );
        assert_eq!(declared_modes(&build).unwrap(), modes);
        clear_declared_modes(&build);
        let err = declared_modes(&build).unwrap_err().to_string();
        assert!(
            err.contains("no declared-mode record") && err.contains("re-stages"),
            "{err}"
        );
        write_declared_modes(&build, &modes).unwrap();
    }
    discard_build(&build);
    assert!(
        !build.exists() && !sidecar.exists(),
        "the discard takes the record"
    );
    let _ = std::fs::remove_dir_all(&h);
}

#[test]
fn the_declared_mode_record_parser_fails_closed() {
    let ok = parse_declared_modes(b"declared-modes v1\nbin/a\x00755\nb\x000\nc\x00644\n").unwrap();
    assert_eq!(ok.get(&b"bin/a"[..]), Some(&0o755));
    assert_eq!(ok.get(&b"b"[..]), Some(&0));
    assert_eq!(ok.get(&b"c"[..]), Some(&0o644));
    assert!(
        parse_declared_modes(b"declared-modes v1\n")
            .unwrap()
            .is_empty()
    );
    for bad in [
        &b"declared-modes v2\nbin/a\x00755\n"[..],
        b"bin/a\x00755\n",
        b"declared-modes v1\nbin/a 755\n",
        b"declared-modes v1\n\x00755\n",
        b"declared-modes v1\nbin/a\0\n",
        b"declared-modes v1\nbin/a\x00758\n",
        b"declared-modes v1\nbin/a\x0007555\n",
    ] {
        assert!(parse_declared_modes(bad).is_none(), "{bad:?}");
    }
    assert_eq!(crate::tree::oct_mode(0o755), "755");
}

fn sha256(hex: &str) -> (DigestAlg, &str) {
    (DigestAlg::Sha256, hex)
}

#[test]
fn a_refusal_memo_names_its_digest_by_algorithm() {
    let h = temp_home("refusal-alg");
    let l = Layout { prefix: h.clone() };
    let build = l.build_dir("agy", 1_000_001_000_003_000_006);
    let marker = build.with_file_name("1000001000003000006.refused");
    let client = aterm_stamp::app_version();
    let earlier = format!(
        "stage-refusal v1\nsha256=aa\ntree_root=bb\nattempts=2\nat=1000\nclient={client}\n\
         why=asset sha256 mismatch: expected aa, got cc\n"
    );
    std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
    std::fs::write(&marker, &earlier).unwrap();
    let m = stage_refusal(&build).unwrap();
    assert_eq!(
        (m.alg, m.digest.as_str(), m.attempts),
        (DigestAlg::Sha256, "aa", 2)
    );
    assert!(m.binds(sha256("aa"), "bb", 1_001));
    record_stage_refusal(
        &build,
        sha256("aa"),
        "bb",
        "asset sha256 mismatch: expected aa, got cc",
        1_000,
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap(),
        earlier.replace("attempts=2", "attempts=3"),
        "a sha256 memo is written as it always was"
    );

    let sha512 = "5".repeat(128);
    record_signer_refusal(
        &build,
        (DigestAlg::Sha512, &sha512),
        "bb",
        "signer refused: x",
        2_000,
    )
    .unwrap();
    let text = std::fs::read_to_string(&marker).unwrap();
    assert!(text.contains(&format!("\nsha512={sha512}\n")), "{text}");
    assert!(!text.contains("sha256="), "{text}");
    let m = stage_refusal(&build).unwrap();
    assert_eq!(
        (m.alg, m.digest.as_str(), m.attempts),
        (DigestAlg::Sha512, sha512.as_str(), 1)
    );
    assert!(m.binds((DigestAlg::Sha512, &sha512), "bb", i64::MAX));
    assert!(
        !m.binds(sha256(&sha512), "bb", i64::MAX),
        "the same hex under another algorithm is another question"
    );
    for bad in [
        text.replace("sha512=", "sha256=x\nsha512="),
        text.replace(&format!("sha512={sha512}\n"), ""),
        text.replace(&format!("sha512={sha512}\n"), "sha512=not-hex\n"),
        text.replace("attempts=1", "attempts=01"),
        text.replace("at=2000", "at=02000"),
        text.replace("at=2000", "at=-0"),
    ] {
        std::fs::write(&marker, &bad).unwrap();
        assert_eq!(stage_refusal(&build), None, "{bad}");
    }
    assert_eq!(
        stage_scratch_of("17.incoming-42"),
        Some((17, Scratch::Incoming))
    );
    assert_eq!(
        stage_scratch_of("17.superseded-42"),
        Some((17, Scratch::Superseded))
    );
    for bad_scratch in [
        "17.incoming-0",
        "17.incoming-042",
        "17.incoming-+42",
        "17.superseded-4294967296",
    ] {
        assert_eq!(stage_scratch_of(bad_scratch), None, "{bad_scratch}");
    }
    let _ = std::fs::remove_dir_all(&h);
}

#[test]
fn a_stage_refusal_binds_only_while_its_digests_and_cooldown_hold() {
    let memo = |attempts| StageRefusal {
        alg: DigestAlg::Sha256,
        digest: "AA".into(),
        tree_root: "bb".into(),
        why: "asset sha256 mismatch: expected aa, got cc".into(),
        at: 1_000,
        attempts,
        signer: false,
        held: false,
        named: false,
        client: aterm_stamp::app_version().into(),
    };
    assert_eq!(memo(1).cooldown_secs(), 0, "one failure holds nothing");
    assert_eq!(memo(2).cooldown_secs(), 12 * 3600);
    assert_eq!(memo(3).cooldown_secs(), 24 * 3600);
    assert_eq!(memo(4).cooldown_secs(), 48 * 3600);
    assert_eq!(
        memo(10).cooldown_secs(),
        7 * 24 * 3600,
        "capped at one week"
    );
    assert_eq!(memo(1_000).cooldown_secs(), 7 * 24 * 3600, "no overflow");
    assert!(
        !memo(1).binds(sha256("aa"), "bb", 1_000),
        "a single failure never holds the next attempt off the wire"
    );

    let m = memo(2);
    assert_eq!(m.retry_after(), 1_000 + 12 * 3600);
    assert!(
        m.binds(sha256("aa"), "BB", 1_000),
        "case-insensitive on both digests"
    );
    assert!(
        !m.binds(sha256("aa"), "BB", m.retry_after()),
        "the cooldown lapses — the next attempt is due"
    );
    assert!(
        !m.binds(sha256("dd"), "bb", 1_000),
        "a re-cut pin's new sha256 binds nothing"
    );
    assert!(
        !m.binds(sha256("aa"), "dd", 1_000),
        "a re-cut pin's new tree_root binds nothing"
    );
    assert!(
        !m.binds(sha256("aa"), "bb", i64::MAX),
        "an unreadable clock (now_unix's fail-closed i64::MAX) reads as lapsed, \
         so it can only cost a download — never block an install"
    );
    assert!(
        !StageRefusal {
            digest: String::new(),
            ..memo(2)
        }
        .binds(sha256(""), "bb", 1_000),
        "a memo with no digest binds nothing"
    );
    let another = format!("{}+another", aterm_stamp::app_version());
    assert!(
        !StageRefusal {
            client: another.clone(),
            ..memo(2)
        }
        .binds(sha256("aa"), "bb", 1_000),
        "a memo another atpkg wrote binds nothing"
    );
    assert!(
        !StageRefusal {
            client: String::new(),
            ..memo(2)
        }
        .binds(sha256("aa"), "bb", 1_000),
        "a pre-field memo binds nothing"
    );
    let held = StageRefusal {
        held: true,
        ..memo(1)
    };
    assert!(
        held.binds(sha256("aa"), "bb", 1_000),
        "held from the first verdict"
    );
    assert!(
        held.binds(sha256("aa"), "bb", i64::MAX),
        "no cooldown to lapse"
    );
    assert!(
        !held.binds(sha256("dd"), "bb", 1_000),
        "a re-cut pin releases it"
    );
    assert!(
        !StageRefusal {
            client: another,
            ..held.clone()
        }
        .binds(sha256("aa"), "bb", 1_000),
        "a new client gets its attempt"
    );
}

#[test]
fn a_signer_refusal_binds_until_the_digests_move() {
    let m = StageRefusal {
        alg: DigestAlg::Sha256,
        digest: "AA".into(),
        tree_root: "bb".into(),
        why: "signer refused: bin/claude is not signed by Developer ID team X".into(),
        at: 1_000,
        attempts: 1,
        signer: true,
        held: false,
        named: false,
        client: aterm_stamp::app_version().into(),
    };
    assert!(
        m.binds(sha256("aa"), "BB", 1_000),
        "the first refusal already binds"
    );
    assert!(
        m.binds(sha256("aa"), "bb", i64::MAX),
        "no cooldown to lapse"
    );
    assert!(
        !m.binds(sha256("dd"), "bb", 1_000),
        "a new sha256 is a new question"
    );
    assert!(
        !m.binds(sha256("aa"), "dd", 1_000),
        "a new tree_root is a new question"
    );
    assert!(
        !StageRefusal {
            digest: String::new(),
            ..m
        }
        .binds(sha256(""), "bb", 1_000)
    );
}

#[test]
fn the_signer_refusal_memo_round_trips() {
    let h = temp_home("signer-refusal");
    let l = Layout { prefix: h.clone() };
    let build = l.build_dir("claude", 1_000_002_000_001_000_280);
    record_signer_refusal(&build, sha256("aa"), "bb", "signer refused: x", 1_000).unwrap();
    let m = stage_refusal(&build).unwrap();
    assert!(m.signer);
    assert!(!m.held, "a signer memo is its own kind, not a hold");
    assert_eq!(m.client, aterm_stamp::app_version());
    assert_eq!((m.attempts, m.why.as_str()), (1, "signer refused: x"));
    assert!(m.binds(sha256("aa"), "bb", i64::MAX));
    record_stage_refusal(&build, sha256("aa"), "bb", "asset sha256 mismatch", 2_000).unwrap();
    assert!(!stage_refusal(&build).unwrap().signer);
    record_signer_refusal(&build, sha256("aa"), "bb", "signer refused: x", 3_000).unwrap();
    clear_stage_refusals(&l, "claude");
    assert_eq!(stage_refusal(&build), None);
    let _ = std::fs::remove_dir_all(&h);
}

/// A NAMED BUILD'S MEMO STANDS FOR ITS OWN BUILD AND BINDS AS ITS KIND DOES (A14): a held one
/// (a resolve verdict) binds from the first verdict for its digests alone and reads back as
/// named, a cooled-down one (a fault of the run) from the second, and neither replaces the
/// program's other memos nor is replaced by them, so two trees locking two failing builds do
/// not wipe each other's memo. NEGATIVE CONTROL: the update lane's memos are still one per
/// program.
#[test]
fn a_named_refusal_binds_as_its_kind_and_stands_for_its_own_build() {
    let h = temp_home("named-refusal");
    let l = Layout { prefix: h.clone() };
    let b = |n: u64| l.build_dir("trust", n);
    let named = |n: u64, kind: RefusalKind, hex: &str, at: i64| {
        record_refusal_in(
            MemoLane::Named,
            kind,
            &b(n),
            sha256(hex),
            "bb",
            "resolve",
            at,
        )
        .unwrap();
    };
    named(17, RefusalKind::Held, "aa", 1_000);
    let m = stage_refusal(&b(17)).unwrap();
    assert!(m.named && m.held && !m.signer, "{m:?}");
    assert_eq!(m.attempts, 1);
    assert!(
        m.binds(sha256("aa"), "bb", i64::MAX),
        "held from the first verdict"
    );
    assert!(
        !m.binds(sha256("dd"), "bb", 1_000),
        "another pack is another question"
    );
    named(16, RefusalKind::Cooldown, "ab", 1_000);
    let fault = stage_refusal(&b(16)).unwrap();
    assert!(fault.named && !fault.held, "{fault:?}");
    assert!(
        !fault.binds(sha256("ab"), "bb", 1_001),
        "a first fault binds nothing"
    );
    named(16, RefusalKind::Cooldown, "ab", 1_002);
    assert!(
        stage_refusal(&b(16))
            .unwrap()
            .binds(sha256("ab"), "bb", 1_003)
    );

    record_stage_refusal(&b(18), sha256("cc"), "bb", "digest", 2_000).unwrap();
    assert!(
        stage_refusal(&b(17)).is_some() && stage_refusal(&b(16)).is_some(),
        "the lane's memo leaves the named ones"
    );
    named(19, RefusalKind::Held, "ee", 3_000);
    assert!(
        stage_refusal(&b(17)).is_some() && stage_refusal(&b(18)).is_some(),
        "a named memo replaces no other"
    );

    record_stage_refusal(&b(20), sha256("ff"), "bb", "digest", 4_000).unwrap();
    assert_eq!(stage_refusal(&b(18)), None, "the lane keeps one memo");
    assert!(stage_refusal(&b(17)).is_some() && stage_refusal(&b(19)).is_some());
    let _ = std::fs::remove_dir_all(&h);
}

#[test]
fn the_refusal_memo_round_trips_advances_and_is_one_per_program() {
    let h = temp_home("stage-refusal");
    let l = Layout { prefix: h.clone() };
    let build = l.build_dir("trust", 4900);
    assert_eq!(stage_refusal(&build), None, "nothing recorded yet");

    record_stage_refusal(
        &build,
        sha256("AA"),
        "bb",
        "asset sha256 mismatch:\nexpected aa",
        1_000,
    )
    .unwrap();
    let marker = build.with_file_name("4900.refused");
    assert!(marker.is_file(), "a sibling, outside the tree");
    assert!(!build.exists(), "the build it refused never came to exist");
    let m = stage_refusal(&build).unwrap();
    assert_eq!((m.digest.as_str(), m.tree_root.as_str()), ("AA", "bb"));
    assert_eq!(m.at, 1_000);
    assert_eq!(m.attempts, 1);
    assert!(!m.held, "a sha256 mismatch cools down; it is not held");
    assert_eq!(
        m.client,
        aterm_stamp::app_version(),
        "the memo names the client that wrote it"
    );
    let text = std::fs::read_to_string(&marker).unwrap();
    assert!(
        !text.contains("hold="),
        "a cooled-down memo carries no hold line: {text}"
    );
    assert_eq!(
        m.why, "asset sha256 mismatch: expected aa",
        "one line: the newline is folded"
    );

    record_stage_refusal(&build, sha256("aa"), "BB", "again", 50_000).unwrap();
    let m = stage_refusal(&build).unwrap();
    assert_eq!((m.attempts, m.at), (2, 50_000));
    record_stage_refusal(&build, sha256("cc"), "bb", "new pin, new verdict", 60_000).unwrap();
    assert_eq!(stage_refusal(&build).unwrap().attempts, 1);
    record_held_refusal(&build, sha256("cc"), "bb", "tree_root mismatch", 70_000).unwrap();
    let m = stage_refusal(&build).unwrap();
    assert!(m.held, "the hold line reads back");
    assert_eq!(m.attempts, 2, "the same digests: the count still advances");
    assert!(
        m.binds(sha256("cc"), "bb", i64::MAX),
        "held: no cooldown to lapse"
    );
    assert!(
        std::fs::read_to_string(&marker)
            .unwrap()
            .contains("\nhold=1\n"),
        "the memo carries the hold"
    );

    let next = l.build_dir("trust", 4901);
    record_stage_refusal(&next, sha256("dd"), "ee", "and again", 70_000).unwrap();
    assert!(
        !marker.exists(),
        "the superseded build's memo is reclaimed, not left to accumulate"
    );
    assert!(stage_refusal(&next).is_some());

    std::fs::write(next.with_file_name("4901.refused"), b"nonsense\nat=1\n").unwrap();
    assert_eq!(
        stage_refusal(&next),
        None,
        "a file that does not open with the schema line is not read"
    );

    record_stage_refusal(&next, sha256("dd"), "ee", "once more", 80_000).unwrap();
    clear_stage_refusal(&next);
    assert_eq!(stage_refusal(&next), None);
    clear_stage_refusal(&next);
    record_stage_refusal(&next, sha256("dd"), "ee", "once more", 90_000).unwrap();
    clear_stage_refusals(&l, "trust");
    assert_eq!(stage_refusal(&next), None, "the explicit door forgets it");
    record_stage_refusal(&next, sha256("dd"), "ee", "once more", 95_000).unwrap();
    std::fs::create_dir_all(next.join("bin")).unwrap();
    for &suffix in STAGE_SIDECAR_SUFFIXES {
        std::fs::write(sidecar_path(&next, suffix).unwrap(), b"x\n").unwrap();
    }
    discard_build(&next);
    assert_eq!(stage_refusal(&next), None, "the discard takes it too");
    for &suffix in STAGE_SIDECAR_SUFFIXES {
        assert!(
            !sidecar_path(&next, suffix).unwrap().exists(),
            "discard_build must remove {suffix}"
        );
    }
    let _ = std::fs::remove_dir_all(&h);
}

#[test]
fn prefix_outside_home_or_with_traversal_falls_back() {
    let home = temp_home("outside");
    assert_eq!(
        vet_prefix(Some(Path::new("/tmp/evil")), &home),
        default_prefix(&home)
    );
    let sneaky = home.join("../somewhere/pkg");
    assert_eq!(vet_prefix(Some(&sneaky), &home), default_prefix(&home));
    assert_eq!(vet_prefix(Some(&home), &home), default_prefix(&home));
    let _ = std::fs::remove_dir_all(&home);
}

#[cfg(unix)]
#[test]
fn root_owned_prefix_outside_home_is_accepted() {
    use std::os::unix::fs::MetadataExt as _;
    let home = temp_home("sysprefix");
    let Ok(meta) = std::fs::symlink_metadata("/usr/lib") else {
        let _ = std::fs::remove_dir_all(&home);
        aterm_uds::host::could_not_run("no /usr/lib here to stand in as a system chain");
    };
    if meta.uid() != 0 || meta.mode() & 0o022 != 0 {
        let _ = std::fs::remove_dir_all(&home);
        aterm_uds::host::could_not_run(
            "/usr/lib here is not root-owned and closed to group and other writes",
        );
    }
    let prefix = Path::new("/usr/lib/aterm-pkg-system-prefix-test");
    assert!(
        system_chain_trusted(prefix),
        "a fully root-owned chain outside $HOME is a trusted system prefix"
    );
    let expected = if caller_can_use_prefix(prefix) {
        prefix.to_path_buf()
    } else {
        default_prefix(&home)
    };
    assert_eq!(vet_prefix(Some(prefix), &home), expected);
    let _ = std::fs::remove_dir_all(&home);
}

/// A configured prefix aterm cannot write is said whole on stderr and logged by its length
/// in a host's aterm.log: the path is the person's aterm.toml (DESIGN §3.6).
#[cfg(unix)]
#[test]
fn an_unwritable_prefix_reaches_a_hosts_log_by_its_length() {
    use std::os::unix::fs::MetadataExt as _;
    let home = temp_home("unwritable-log");
    let Ok(meta) = std::fs::symlink_metadata("/usr/lib") else {
        aterm_uds::host::could_not_run("no /usr/lib here to stand in as a system chain");
    };
    let prefix = Path::new("/usr/lib/QZP7E7J6-prefix");
    if meta.uid() != 0 || meta.mode() & 0o022 != 0 || caller_can_use_prefix(prefix) {
        aterm_uds::host::could_not_run(
            "/usr/lib here is not a root-owned chain this user cannot write",
        );
    }
    let len = prefix.as_os_str().len();
    let host = crate::notice::heard::while_running(crate::notice::Sink::HostLog, || {
        assert_eq!(vet_prefix(Some(prefix), &home), default_prefix(&home));
    });
    assert!(host.stderr.is_empty(), "a host's notice reached stderr");
    assert!(
        host.log
            .iter()
            .any(|line| line.starts_with(&format!("configured prefix {len}B is not writable"))),
        "the line is logged with the path's length: {} lines",
        host.log.len()
    );
    assert!(
        !host.log.iter().any(|line| line.contains("QZP7E7J")),
        "a host's log keeps the configured path"
    );
    // CONTROL: a verb's stderr still says the path whole (said once per process, so a path
    // of its own).
    let other = Path::new("/usr/lib/QZP7E7J7-prefix");
    let verb = crate::notice::heard::while_running(crate::notice::Sink::Stderr, || {
        assert_eq!(vet_prefix(Some(other), &home), default_prefix(&home));
    });
    assert!(
        verb.stderr.contains("QZP7E7J7-prefix"),
        "stderr says the path"
    );
    assert!(verb.log.is_empty(), "a verb's notice is not logged");
}

#[cfg(unix)]
#[test]
fn world_writable_system_ancestor_is_refused() {
    use std::os::unix::fs::MetadataExt as _;
    let home = temp_home("wwancestor");
    let Some((root, meta)) = ["/private/tmp", "/tmp"].into_iter().find_map(|root| {
        std::fs::symlink_metadata(root)
            .ok()
            .filter(std::fs::Metadata::is_dir)
            .map(|meta| (root, meta))
    }) else {
        let _ = std::fs::remove_dir_all(&home);
        aterm_uds::host::could_not_run("no /private/tmp or /tmp directory here");
    };
    if meta.uid() != 0 || meta.mode() & 0o022 == 0 {
        let _ = std::fs::remove_dir_all(&home);
        aterm_uds::host::could_not_run(format!(
            "{root} here is not the root-owned world-writable fixture this test needs"
        ));
    }
    let prefix = Path::new(root).join("aterm-pkg-should-not-be-trusted");
    let prefix = prefix.as_path();
    assert_eq!(
        vet_prefix(Some(prefix), &home),
        default_prefix(&home),
        "a world-writable ancestor must fail closed even when root owns it"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[cfg(unix)]
#[test]
fn group_writable_intermediate_parent_is_rejected() {
    let home = temp_home("gwparent");
    let mid = home.join("Library");
    std::fs::create_dir_all(&mid).unwrap();
    std::fs::set_permissions(&mid, std::fs::Permissions::from_mode(0o700)).unwrap();
    let bad = mid.join("shared");
    std::fs::create_dir_all(&bad).unwrap();
    std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o777)).unwrap();
    let prefix = bad.join("pkg");
    assert_eq!(
        vet_prefix(Some(&prefix), &home),
        default_prefix(&home),
        "a group/other-writable intermediate parent must fail closed to the default"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn safe_under_home_prefix_is_accepted() {
    let home = temp_home("safe");
    let a = home.join("a");
    std::fs::create_dir_all(&a).unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(&a, std::fs::Permissions::from_mode(0o700)).unwrap();
    let prefix = a.join("b").join("pkg");
    assert_eq!(vet_prefix(Some(&prefix), &home), prefix);
    let _ = std::fs::remove_dir_all(&home);
}

#[cfg(unix)]
const OPEN_UMASK_ENV: &str = "ATPKG_STORE_OPEN_UMASK_HOME";
#[cfg(unix)]
const OPEN_UMASK_PROBE: &str =
    "store::tests::a_prefix_made_under_an_open_umask_has_no_shared_ancestor";

#[cfg(unix)]
fn probe_a_prefix_made_under_an_open_umask_has_no_shared_ancestor_if_asked() -> bool {
    let Some(home) = std::env::var_os(OPEN_UMASK_ENV) else {
        return false;
    };
    let home = PathBuf::from(home);
    // SAFETY: `umask(2)` swaps this process's file-creation mask and cannot fail; this
    // process runs this one test (`--exact`), so no other create sees the mask.
    unsafe { libc::umask(0o002) };
    let mode = |rel: &str| {
        let meta = std::fs::symlink_metadata(home.join(rel)).unwrap();
        format!("{:o}", meta.permissions().mode() & 0o7777)
    };
    let default = Layout {
        prefix: home.join(".local/share/aterm/pkg"),
    };
    default.ensure_dir(&default.prefix).unwrap();
    for made in [".local", ".local/share", ".local/share/aterm"] {
        assert_eq!(mode(made), "755", "{made}: made under umask 002");
    }
    assert_eq!(mode(".local/share/aterm/pkg"), "700", "the $HOME prefix");
    let configured = Layout {
        prefix: home.join("work/aterm/pkg"),
    };
    configured.ensure_dir(&configured.prefix).unwrap();
    for made in ["work", "work/aterm"] {
        assert_eq!(mode(made), "755", "{made}: made under umask 002");
    }
    assert_eq!(mode("work/aterm/pkg"), "700", "the configured prefix");
    assert_eq!(
        vet_prefix(Some(&configured.prefix), &home),
        configured.prefix,
        "the prefix this pass made is refused by the next one"
    );
    let shared = Layout {
        prefix: home.join("opt/aterm/pkg"),
    };
    system_shape::force(&shared.prefix);
    shared.ensure_dir(&shared.prefix).unwrap();
    for made in ["opt", "opt/aterm"] {
        assert_eq!(mode(made), "755", "{made}: made under umask 002");
    }
    assert_eq!(mode("opt/aterm/pkg"), "755", "the system prefix");
    true
}

#[cfg(unix)]
#[test]
fn a_prefix_made_under_an_open_umask_has_no_shared_ancestor() {
    if probe_a_prefix_made_under_an_open_umask_has_no_shared_ancestor_if_asked() {
        return;
    }
    let home = temp_home("open-umask");
    let out = aterm_uds::spawnfd::command(std::env::current_exe().unwrap())
        .args(["--exact", "--nocapture", OPEN_UMASK_PROBE])
        .env(OPEN_UMASK_ENV, home.as_os_str())
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("1 passed"),
        "the prefix under umask 002: {}\n{stdout}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}

#[cfg(unix)]
#[test]
fn trusted_system_prefix_that_we_cannot_write_falls_back_to_default() {
    aterm_uds::host::could_not_run_as_root("a prefix this user cannot write");
    let home = temp_home("unwritable-system");
    let prefix = PathBuf::from("/usr/lib/aterm-nux-probe/pkg");
    if !system_chain_trusted(&prefix) {
        let _ = std::fs::remove_dir_all(&home);
        aterm_uds::host::could_not_run(
            "/usr/lib here is not a trusted system chain (root-owned, closed to writes)",
        );
    }
    assert!(
        !caller_can_use_prefix(&prefix),
        "precondition: a non-root user must not be able to write under /usr"
    );
    assert_eq!(
        vet_prefix(Some(&prefix), &home),
        default_prefix(&home),
        "a trusted-but-unwritable system prefix must degrade to the default"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[cfg(unix)]
#[test]
fn an_unwritable_prefix_whose_shims_cannot_be_stat_ed_is_still_usable() {
    use std::os::unix::fs::PermissionsExt as _;
    aterm_uds::host::could_not_run_as_root("a mode-0 prefix");
    let prefix = crate::testdir::dir("atpkg-shared-");
    let holder = prefix.join("store/ay/17/bin");
    std::fs::create_dir_all(&holder).unwrap();
    std::fs::create_dir_all(prefix.join("bin")).unwrap();
    let ay = ToolName::new("ay").unwrap();
    std::fs::write(holder.join(ay.exe_file()), b"#!/bin/true\n").unwrap();
    crate::platform::install_shim(&holder, &ay, &prefix.join("bin").join(ay.shim_file())).unwrap();
    assert!(
        caller_can_use_prefix(&prefix),
        "precondition: a writable prefix serving a real shim is usable"
    );
    std::fs::set_permissions(&holder, std::fs::Permissions::from_mode(0o000)).unwrap();
    std::fs::set_permissions(&prefix, std::fs::Permissions::from_mode(0o555)).unwrap();
    let target = prefix.join("store/ay/17/bin").join(ay.exe_file());
    let armed = std::fs::metadata(&target).is_err();
    let usable = caller_can_use_prefix(&prefix);
    std::fs::set_permissions(&prefix, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::set_permissions(&holder, std::fs::Permissions::from_mode(0o700)).unwrap();
    let _ = std::fs::remove_dir_all(&prefix);
    assert!(
        armed,
        "the fixture must make the stat fail, or it proves nothing"
    );
    assert!(
        usable,
        "a shared prefix whose tools we may run but not stat must not be abandoned"
    );
}

#[test]
fn shim_names_collision_and_shape_policy() {
    for bad in ["sudo", "SSH", "scp", "sh", "env", "rustc", "Codesign"] {
        assert!(!shim_allowed(bad), "{bad} must be refused a shim");
    }
    for shell in SHELLS {
        for name in [
            (*shell).to_string(),
            shell.to_ascii_uppercase(),
            format!("{ALIAS_PREFIX}{shell}"),
            format!("{shell}-preview"),
            format!("{shell}5"),
            format!("{shell}-7.4"),
        ] {
            assert!(
                !shim_allowed(&name),
                "the shell {name} must be refused a shim"
            );
        }
    }
    for named in ["pwsh", "elvish", "xonsh", "mksh", "powershell", "busybox"] {
        assert!(SHELLS.contains(&named), "{named} is on the list");
    }
    for lookalike in [
        "pw\u{17F}h",
        "fi\u{17F}h",
        "\u{FB01}sh",
        "xon\u{17F}h",
        "elvi\u{17F}h",
        "\u{212A}sh",
        "\u{17F}h",
        "ba\u{17F}h",
        "alab-pw\u{17F}h",
        "caf\u{E9}",
    ] {
        assert!(
            !shim_allowed(lookalike),
            "{lookalike:?} is not plain ASCII and must be refused a shim"
        );
    }
    for ok in [
        "nushell-lsp",
        "dashboard",
        "pwsh-fmt",
        "fisher",
        "kshrc-lint",
        "nuget",
        "ashes",
        "rcfile",
        "bash-lsp",
        "zsh-5x",
        "ion-lint",
        "trust2",
    ] {
        assert!(shim_allowed(ok), "{ok} is no shell and should be allowed");
    }
    for bad in ["", ".", "..", "a/b", "x\0y"] {
        assert!(!shim_allowed(bad), "{bad:?} is not a valid shim name");
    }
    for ok in ["ay", "ny", "git", "gh", "trust-mc", "clean-certify"] {
        assert!(shim_allowed(ok), "{ok} should be allowed");
    }
}

#[test]
fn alias_names_admit_on_their_base_and_refuse_sensitive_bases() {
    for ok in ["alab-ay", "alab-git", "alab-trust", "alab-trust-cg"] {
        assert!(shim_allowed(ok), "{ok} should be allowed");
    }
    for bad in [
        "alab-sudo",
        "alab-SSH",
        "alab-scp",
        "alab-cargo",
        "alab-..",
        "alab-alab-ay",
        "alab-a/b",
        "alab-",
    ] {
        assert!(!shim_allowed(bad), "{bad:?} must be refused a shim");
    }
    let trust = ToolName::new("trust").unwrap();
    assert!(!trust.is_alias());
    let alias = trust.alias().expect("a plain admissible name has an alias");
    assert_eq!(alias.as_str(), "alab-trust");
    assert!(alias.is_alias());
    assert_eq!(alias.alias_base(), Some(trust.clone()));
    assert_eq!(trust.alias_base(), None, "a plain name has no base");
    assert_eq!(alias.alias(), None, "an alias never gets an alias");
    let cg = ToolName::new("trust-cg").unwrap();
    let cg_alias = cg.alias().unwrap();
    assert_eq!(cg_alias.as_str(), "alab-trust-cg");
    assert_eq!(cg_alias.alias_base(), Some(cg));
    assert_eq!(
        cg_alias.shim_file(),
        format!("alab-trust-cg{}", crate::platform::SHIM_SUFFIX)
    );
    assert_eq!(ToolName::new("alab-sudo"), None);
    assert_eq!(ToolName::from_shim_file("alab-scp"), None);
}

#[test]
fn the_suffix_rule_round_trips_and_never_doubles_on_a_suffixed_platform() {
    assert_eq!(with_suffix("ay", ".cmd"), "ay.cmd");
    assert_eq!(with_suffix("ay", ".exe"), "ay.exe");
    assert_ne!(with_suffix("ay", ".cmd"), with_suffix("ay", ".exe"));

    assert_eq!(without_suffix(&with_suffix("ay", ".cmd"), ".cmd"), "ay");
    assert_eq!(
        with_suffix(&with_suffix("ay", ".cmd"), ".cmd"),
        "ay.cmd.cmd"
    );

    assert_eq!(without_suffix("ay", ".cmd"), "ay");
    assert_eq!(without_suffix("ay.exe", ".cmd"), "ay.exe");

    assert_eq!(with_suffix("ay", ""), "ay");
    assert_eq!(without_suffix("ay", ""), "ay");
}

#[test]
#[cfg(unix)]
fn runtime_admission_follows_only_the_owned_aside() {
    let home = temp_home("ready-runtime-aside");
    let build = home.join("store/trust/7");
    std::fs::create_dir_all(build.join("bin")).unwrap();
    std::fs::write(build.join("bin/targo"), b"synthetic non-ELF tool").unwrap();
    mark_build_ready(&build).unwrap();
    let host = Some(aterm_toolchain::gnu_runtime::Host {
        machine: 62,
        glibc: aterm_toolchain::gnu_runtime::Version::parse("2.17").unwrap(),
    });
    assert!(build_is_complete_on(&build, host));
    let aside = aterm_toolchain::layout::aside_of(&build).unwrap();
    std::fs::rename(&build, &aside).unwrap();
    std::os::unix::fs::symlink("7.noindex", &build).unwrap();
    assert!(build_is_complete_on(&build, host));

    std::fs::remove_file(&build).unwrap();
    std::os::unix::fs::symlink("elsewhere", &build).unwrap();
    let outside = build.with_file_name("elsewhere");
    std::fs::rename(&aside, &outside).unwrap();
    assert!(!build_is_complete_on(&build, host));

    std::fs::remove_file(&build).unwrap();
    std::os::unix::fs::symlink("7.noindex", &build).unwrap();
    std::os::unix::fs::symlink("elsewhere", &aside).unwrap();
    assert!(!build_is_complete_on(&build, host));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_marker_from_the_other_slice_reads_as_not_installed() {
    let home = temp_home("readyslice");
    let build = home.join("store").join("ay").join("18");
    std::fs::create_dir_all(&build).unwrap();
    mark_build_ready(&build).unwrap();
    assert!(build_is_complete(&build));

    let marker = ready_marker_path(&build).unwrap();
    let text = std::fs::read_to_string(&marker).unwrap();
    assert!(
        text.starts_with("ok\n"),
        "line 1 stays what every earlier version wrote: {text:?}"
    );
    assert_eq!(recorded_platform(&text), Some(running_platform().as_str()));

    let mut foreign = String::from("ok\n");
    foreign.push_str(READY_PLATFORM_KEY);
    foreign.push_str("some-other-arch-macos\n");
    std::fs::write(&marker, foreign).unwrap();
    assert!(
        !build_is_complete(&build),
        "a build installed by the other architecture is not installed for us"
    );

    mark_build_ready(&build).unwrap();
    assert!(
        build_is_complete(&build),
        "re-staging natively must clear the mismatch, not leave the build unusable"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_marker_is_refuted_when_the_bin_it_vouched_for_is_gone() {
    let home = temp_home("readygutted");
    let build = home.join("store").join("trust").join("8595");
    std::fs::create_dir_all(build.join("bin")).unwrap();
    std::fs::create_dir_all(build.join("lib")).unwrap();
    std::fs::write(build.join("bin").join("trustc"), b"#!/bin/true\n").unwrap();
    std::fs::write(build.join("bin").join("targo"), b"#!/bin/true\n").unwrap();
    std::fs::write(build.join("lib").join("libtrust.dylib"), b"payload").unwrap();
    mark_build_ready(&build).unwrap();
    assert!(build_is_complete(&build), "precondition: a whole build");

    let text = std::fs::read_to_string(ready_marker_path(&build).unwrap()).unwrap();
    assert_eq!(
        recorded_contents(&text),
        Some("bin:targo,trustc"),
        "sorted, comma-joined, so the record is a function of the tree: {text:?}"
    );

    std::fs::remove_dir_all(build.join("bin")).unwrap();
    assert!(
        ready_marker_path(&build).unwrap().is_file(),
        "the fixture must leave the marker in place, or it proves nothing"
    );
    assert!(
        build.join("lib").join("libtrust.dylib").is_file(),
        "and the orphaned payload, exactly as on m3"
    );
    assert!(
        !build_is_complete(&build),
        "a marker whose `bin/` is gone vouches for nothing"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[cfg(unix)]
#[test]
fn a_bin_symlink_is_recorded_and_checked_by_its_own_name() {
    let home = temp_home("readylink");
    let build = home.join("store").join("trust").join("8595");
    std::fs::create_dir_all(build.join("bin")).unwrap();
    std::fs::write(build.join("bin").join("tippy"), b"x").unwrap();
    std::os::unix::fs::symlink("tippy", build.join("bin").join("clippy")).unwrap();
    mark_build_ready(&build).unwrap();
    let text = std::fs::read_to_string(ready_marker_path(&build).unwrap()).unwrap();
    assert_eq!(recorded_contents(&text), Some("bin:clippy,tippy"));
    std::fs::remove_file(build.join("bin").join("tippy")).unwrap();
    assert!(
        !build_is_complete(&build),
        "`tippy` was recorded in its own right and is gone"
    );
    std::fs::write(build.join("bin").join("tippy"), b"x").unwrap();
    assert!(build_is_complete(&build));
    std::fs::remove_file(build.join("bin").join("clippy")).unwrap();
    assert!(!build_is_complete(&build));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_build_with_no_bin_records_none_and_stays_complete() {
    let home = temp_home("readynone");
    let build = home.join("store").join("clt").join("3");
    std::fs::create_dir_all(&build).unwrap();
    mark_build_ready(&build).unwrap();
    let text = std::fs::read_to_string(ready_marker_path(&build).unwrap()).unwrap();
    assert_eq!(recorded_contents(&text), Some(READY_CONTENTS_NONE));
    assert!(build_is_complete(&build));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_marker_over_an_absent_build_dir_is_not_complete() {
    let home = temp_home("readyghost");
    let build = home.join("store").join("ay").join("18");
    std::fs::create_dir_all(&build).unwrap();
    mark_build_ready(&build).unwrap();
    std::fs::remove_dir_all(&build).unwrap();
    assert!(
        ready_marker_path(&build).unwrap().is_file(),
        "the fixture keeps the sibling marker, which is the whole point"
    );
    assert!(!build_is_complete(&build));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn contents_still_stand_refutes_only_a_proven_absence() {
    let home = temp_home("readycontents");
    let build = home.join("b");
    std::fs::create_dir_all(build.join("bin")).unwrap();
    std::fs::write(build.join("bin").join("ay"), b"x").unwrap();
    assert!(contents_still_stand(&build, READY_CONTENTS_NONE));
    assert!(contents_still_stand(&build, "bin:ay"));
    assert!(!contents_still_stand(&build, "bin:ay,ny"));
    assert!(contents_still_stand(&build, "tree-root:deadbeef"));
    assert!(contents_still_stand(&build, "bin"));
    let _ = std::fs::remove_dir_all(&home);
}

/// A first-exec hold is on disk before it returns and goes once its program showed it
/// ran, unless another hold of the build stands: another process's mark, or another of
/// this process's starts still running.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_first_exec_hold_goes_when_it_ran_and_leaves_every_other_hold() {
    let home = temp_home("firstexec");
    let build = |n: &str| {
        let build = home.join("store").join("ay").join(n);
        std::fs::create_dir_all(build.join("bin")).unwrap();
        (
            build.join("bin").join("ay"),
            build.join("bin").join("ny"),
            build,
        )
    };
    let mark = |build: &Path| sidecar_path(build, UNPROVEN_SUFFIX).unwrap();
    let starts = |build: &Path| sidecar_path(build, FIRST_EXEC_MARKS_SUFFIX).unwrap();

    let (ay, _, alone) = build("1");
    let hold = hold_first_exec(&ay).unwrap();
    assert!(starts(&alone).is_dir() && held_unproven(&alone));
    hold.ran();
    assert!(!starts(&alone).exists() && !held_unproven(&alone));

    let (ay, _, theirs) = build("2");
    let now = aterm_uds::process::awake_ms().expect("the awake clock");
    write_sidecar_durably(&theirs, UNPROVEN_SUFFIX, now.to_string().as_bytes()).unwrap();
    hold_first_exec(&ay).unwrap().ran();
    assert!(
        mark(&theirs).is_file() && held_unproven(&theirs),
        "another process's hold stands"
    );

    let (ay, ny, both) = build("3");
    let first = hold_first_exec(&ay).unwrap();
    let second = hold_first_exec(&ny).unwrap();
    first.ran();
    assert!(
        starts(&both).is_dir() && held_unproven(&both),
        "a start still running holds the build"
    );
    second.ran();
    assert!(
        !held_unproven(&ay) && !held_unproven(&ny),
        "neither start holds anything of this process now"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// The mark a resolve check leaves holds its build in every process until FIRST_EXEC_HANG of
/// awake time has passed or the Mac restarted; the discard that then removes the build takes
/// the mark with it.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn an_unproven_mark_holds_its_build_in_every_process_until_it_is_spent() {
    let home = temp_home("unproven");
    let prog = home.join("store").join("trust");
    let build = prog.join("8596");
    let mark = |at: u64| {
        std::fs::create_dir_all(build.join("bin")).unwrap();
        std::fs::write(build.join("bin").join("trustc"), b"x").unwrap();
        write_sidecar_durably(&build, UNPROVEN_SUFFIX, at.to_string().as_bytes()).unwrap();
    };
    let now = aterm_uds::process::awake_ms().expect("the awake clock");
    mark(now);
    assert_eq!(held_build_in(&prog), Some(build.clone()));
    discard_build(&build);
    assert!(
        build.join("bin").join("trustc").is_file(),
        "a fresh mark holds the build in place"
    );

    mark(now + 3_600_000);
    assert_eq!(held_build_in(&prog), None, "a restart ended the assessment");
    discard_build(&build);
    assert!(!build.exists(), "a spent hold lets the discard go");
    assert!(
        !sidecar_path(&build, UNPROVEN_SUFFIX).unwrap().exists(),
        "and the mark goes with the build"
    );

    let window = u64::try_from(aterm_uds::deadline::FIRST_EXEC_HANG.as_millis()).unwrap();
    if let Some(aged) = now.checked_sub(window + 1_000) {
        mark(aged);
        assert_eq!(held_build_in(&prog), None, "an aged mark holds nothing");
    }
}

#[cfg(unix)]
#[test]
fn discard_build_unmarks_before_it_removes_so_an_interrupted_discard_leaves_no_lie() {
    let home = temp_home("discardorder");
    let build = home.join("store").join("trust").join("8595");
    std::fs::create_dir_all(build.join("bin")).unwrap();
    std::fs::write(build.join("bin").join("trustc"), b"x").unwrap();
    mark_build_ready(&build).unwrap();
    assert!(build_is_complete(&build), "precondition");
    std::fs::set_permissions(build.join("bin"), std::fs::Permissions::from_mode(0o500)).unwrap();
    discard_build(&build);
    let survived = build.join("bin").join("trustc").is_file();
    std::fs::set_permissions(build.join("bin"), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        survived,
        "the fixture must make the removal fail, or it proves nothing"
    );
    assert!(
        !ready_marker_path(&build).unwrap().exists(),
        "the marker comes down FIRST, so a failed removal cannot leave one behind"
    );
    assert!(
        !build_is_complete(&build),
        "and the surviving tree reads as incomplete — reclaimable debris, not an install"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn unmark_program_builds_unmarks_every_build_and_only_builds() {
    let home = temp_home("unmarkall");
    let prog = home.join("store").join("trust");
    for n in [8590u64, 8595] {
        let b = prog.join(n.to_string());
        std::fs::create_dir_all(b.join("bin")).unwrap();
        std::fs::write(b.join("bin").join("trustc"), b"x").unwrap();
        mark_build_ready(&b).unwrap();
    }
    std::fs::create_dir_all(prog.join("8595.incoming-7")).unwrap();
    std::fs::write(prog.join("8595.provenance"), b"src\n").unwrap();
    let recording = aterm_durable::seam::Recording::start();
    unmark_program_builds(&prog).unwrap();
    assert_eq!(
        recording.take(),
        [aterm_durable::Flush::Ordered],
        "every marker down before any tree goes: one ordered directory flush \
         (aterm-durable's counting seam; on Windows only noted)"
    );
    drop(recording);
    for n in [8590u64, 8595] {
        let b = prog.join(n.to_string());
        assert!(!build_is_complete(&b), "build {n} must read as incomplete");
        assert!(
            b.join("bin").join("trustc").is_file(),
            "and nothing but the marker is touched"
        );
    }
    assert!(
        prog.join("8595.provenance").is_file(),
        "other sidecars stay"
    );
    assert!(prog.join("8595.incoming-7").is_dir());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_failed_marker_removal_keeps_the_build() {
    let home = temp_home("unmark-remove-fails");
    let build = home.join("store").join("trust").join("8595");
    std::fs::create_dir_all(build.join("bin")).unwrap();
    let witness = build.join("bin").join("trustc");
    std::fs::write(&witness, b"keep").unwrap();
    let marker = ready_marker_path(&build).unwrap();
    std::fs::create_dir(&marker).unwrap();
    std::fs::write(marker.join("obstruction"), b"keep marker").unwrap();
    assert!(
        clear_build_ready(&build).is_err(),
        "the unmark really fails"
    );
    assert!(unmark_program_builds(build.parent().unwrap()).is_err());
    assert!(
        unmark_program_builds(&witness).is_err(),
        "a scan failure is returned"
    );
    discard_build(&build);
    assert_eq!(
        std::fs::read(&witness).unwrap(),
        b"keep",
        "a build whose marker could not be removed is retained"
    );
    assert_eq!(
        std::fs::read(marker.join("obstruction")).unwrap(),
        b"keep marker"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// The store steps that remove a tree after its marker stop before the tree when the
/// flush that orders the marker's removal ahead of it fails: an uninstall's unmark
/// returns the failure (its caller removes nothing), and a discard keeps the tree,
/// unmarked, for a later pass. A folder the volume cannot flush stops neither. With the
/// flush's outcome dropped, as before, both went on and removed the tree.
#[test]
fn a_failed_flush_of_a_markers_removal_keeps_the_tree() {
    use aterm_durable::seam::{EIO, ENOTSUP, FailingDirs};
    let home = temp_home("unmark-flush-fails");
    let prog = home.join("store").join("trust");
    let build = prog.join("8595");
    let laid = || {
        std::fs::create_dir_all(build.join("bin")).unwrap();
        std::fs::write(build.join("bin").join("trustc"), b"x").unwrap();
        mark_build_ready(&build).unwrap();
    };
    laid();
    {
        let _eio = FailingDirs::with(EIO);
        assert_eq!(
            unmark_program_builds(&prog).map_err(|e| e.raw_os_error()),
            Err(Some(EIO))
        );
        assert!(!build_is_complete(&build), "the marker is down");
        mark_build_ready(&build).unwrap();
        discard_build(&build);
    }
    assert_eq!(
        std::fs::read(build.join("bin").join("trustc")).unwrap(),
        b"x"
    );
    assert!(!build_is_complete(&build), "unmarked: partial debris");
    {
        let _refused = FailingDirs::with(ENOTSUP);
        mark_build_ready(&build).unwrap();
        unmark_program_builds(&prog).expect("a folder the volume cannot flush stops nothing");
        discard_build(&build);
    }
    assert!(
        std::fs::symlink_metadata(&build).is_err(),
        "the discard went on past a refusal"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn ready_text_accepts_only_an_absent_or_matching_record() {
    assert!(ready_text_accepts("ok\n", "aarch64-macos"));
    assert!(!ready_text_accepts("", "aarch64-macos"));
    assert!(!ready_text_accepts("\0\0\0\0", "aarch64-macos"));
    assert!(!ready_text_accepts("o", "aarch64-macos"));
    assert!(!ready_text_accepts(
        "platform=aarch64-macos\n",
        "aarch64-macos"
    ));
    assert!(ready_text_accepts("ok", "aarch64-macos"));
    assert!(ready_text_accepts(
        "ok\nplatform=aarch64-macos\n",
        "aarch64-macos"
    ));
    assert!(!ready_text_accepts(
        "ok\nplatform=x86_64-macos\n",
        "aarch64-macos"
    ));
    assert!(!ready_text_accepts(
        "ok\nplatform=aarch64-macos\n",
        "x86_64-macos"
    ));
    assert!(ready_text_accepts(
        "ok\r\nplatform=aarch64-macos\r\n",
        "aarch64-macos"
    ));
    assert!(ready_text_accepts("ok\nplatform=\n", "aarch64-macos"));
    assert!(ready_text_accepts(
        "ok\narch=x86_64-macos\n",
        "aarch64-macos"
    ));
}

#[test]
fn a_marker_left_empty_or_torn_by_a_crash_reads_as_incomplete() {
    let home = temp_home("readytorn");
    let build = home.join("store").join("ay").join("18");
    std::fs::create_dir_all(&build).unwrap();
    let marker = ready_marker_path(&build).unwrap();

    std::fs::write(&marker, b"").unwrap();
    assert!(
        !build_is_complete(&build),
        "a zero-length marker must never vouch for a build"
    );
    std::fs::write(&marker, vec![0u8; 4096]).unwrap();
    assert!(
        !build_is_complete(&build),
        "a NUL-filled marker is not `ok`"
    );
    std::fs::write(&marker, [0xff_u8, 0xfe, 0xff]).unwrap();
    assert!(!build_is_complete(&build), "a non-UTF-8 marker is not `ok`");
    std::fs::write(&marker, b"o").unwrap();
    assert!(!build_is_complete(&build), "a torn marker is not `ok`");

    mark_build_ready(&build).unwrap();
    assert!(build_is_complete(&build));
    std::fs::write(&marker, b"ok\n").unwrap();
    assert!(build_is_complete(&build));
    let _ = std::fs::remove_dir_all(&home);
}

/// On Unix each flush is one plain `fsync(2)`, aterm-durable's
/// (`platform::sync_file_contents` through `primitive::fsync`), which astream-sys counts
/// `plain` in `flush_tally`: the seam counts exactly this thread's seven, and the
/// process-wide tally moves by at least them (this binary's other tests flush meanwhile,
/// so only a bound reads there; aterm-durable's `tests/flush_tally.rs` holds the two
/// counts equal). A store flush that called the OS itself, as this one did through
/// `libc::fsync`, moves neither.
#[test]
fn sync_tree_flushes_every_file_and_directory_it_walks() {
    let home = temp_home("synctree");
    let root = home.join("18.incoming-1");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::create_dir_all(root.join("lib").join("nested")).unwrap();
    std::fs::write(root.join("bin").join("ay"), b"#!/bin/true\n").unwrap();
    std::fs::write(root.join("lib").join("a.rlib"), b"payload").unwrap();
    std::fs::write(root.join("lib").join("nested").join("b.rlib"), b"deep").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("../lib/a.rlib", root.join("bin").join("link")).unwrap();

    let recording = aterm_durable::seam::Recording::start();
    let before = astream_sys::flush_tally();
    let synced = sync_tree(&root).unwrap();
    let after = astream_sys::flush_tally();
    assert_eq!(
        synced.files, 3,
        "every regular file, and the symlink is not one"
    );
    let dirs_expected = if cfg!(windows) { 0 } else { 4 };
    assert_eq!(
        synced.dirs, dirs_expected,
        "the root, bin, lib and lib/nested — where a directory can be opened"
    );
    #[cfg(unix)]
    {
        assert_eq!(
            recording.take_plain(),
            7,
            "each file and directory: one plain fsync(2), aterm-durable's"
        );
        assert_eq!(recording.take(), [], "a plain flush is no Flush");
        assert!(
            after.plain >= before.plain + 7,
            "astream-sys counted the store's flushes: {before:?} -> {after:?}"
        );
    }
    #[cfg(not(unix))]
    let _ = (recording, before, after);

    assert!(sync_tree(&home.join("absent")).is_err());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn append_bin_to_path_appends_never_prepends() {
    let bin = Path::new("/p/bin");
    let inherited = std::env::join_paths([Path::new("/usr/bin"), Path::new("/bin")]).unwrap();
    let out = append_bin_to_path(Some(&inherited), bin);
    assert_eq!(
        out,
        std::env::join_paths([Path::new("/usr/bin"), Path::new("/bin"), bin]).unwrap()
    );
    assert_eq!(append_bin_to_path(None, bin), OsString::from("/p/bin"));
    assert_eq!(
        append_bin_to_path(Some(OsStr::new("")), bin),
        OsString::from("/p/bin")
    );
}

#[test]
fn append_bin_to_path_is_idempotent() {
    let bin = Path::new("/p/bin");
    let bin_first = std::env::join_paths([Path::new("/p/bin"), Path::new("/usr/bin")]).unwrap();
    assert_eq!(append_bin_to_path(Some(&bin_first), bin), bin_first);
    let bin_last = std::env::join_paths([Path::new("/usr/bin"), Path::new("/p/bin")]).unwrap();
    assert_eq!(append_bin_to_path(Some(&bin_last), bin), bin_last);
}

#[test]
fn prepend_bin_to_path_moves_to_front_once_and_is_idempotent() {
    let bin = Path::new("/p/bin");
    let inherited = std::env::join_paths([Path::new("/usr/bin"), Path::new("/bin"), bin]).unwrap();
    let expected = std::env::join_paths([bin, Path::new("/usr/bin"), Path::new("/bin")]).unwrap();
    assert_eq!(prepend_bin_to_path(Some(&inherited), bin), expected);
    assert_eq!(prepend_bin_to_path(Some(&expected), bin), expected);
    assert_eq!(prepend_bin_to_path(None, bin), OsString::from("/p/bin"));
    assert_eq!(
        prepend_bin_to_path(Some(OsStr::new("")), bin),
        OsString::from("/p/bin")
    );
}
