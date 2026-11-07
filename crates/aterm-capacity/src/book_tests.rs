// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The book's file and the plain build's reading of it ([`super::chain`], [`super::need`]), on
//! fixed records: no test reads this user's own.

use super::*;

const GIB: u64 = 1 << 30;

fn key(stage: &str, checkout: &str, crates: &[&str]) -> Key {
    Key {
        stage: stage.to_owned(),
        checkout: checkout.to_owned(),
        crates: Some(crates.iter().map(|c| (*c).to_owned()).collect()),
        variant: None,
    }
}

fn with(rows: &[(Key, u64)]) -> Records {
    let mut r = Records::default();
    for (at, (k, gib)) in rows.iter().enumerate() {
        r.push(k.clone(), gib * GIB, at as u64);
    }
    r
}

/// Verify's file, as its serde derive wrote it before the book left verify (fields in its
/// struct order, `crates` and `variant` left out when unset), reads whole, and reads back the
/// same once the book writes it. NEGATIVE CONTROL: schema 1's file, and one whose row lacks its
/// checkout, read as none.
#[test]
fn verifys_schema_2_file_reads_as_its_serde_derive_wrote_it() {
    let text = concat!(
        r#"{"schema":2,"rows":[{"key":{"stage":"TestCompile","checkout":"/ck","crates":["#,
        r#""aterm-gui"]},"peaks":[{"bytes":16106127360,"at_ms":1793278342337},"#,
        r#"{"bytes":18897856102,"at_ms":1793279342337}]},{"key":{"stage":"Test","#,
        r#""checkout":"/ck","crates":["aterm-gui"],"variant":"cells linux-arm"},"peaks":[{"bytes":"#,
        r#"1610612736,"at_ms":1793278342408}]},{"key":{"stage":"Export","checkout":"/ck"},"#,
        r#""peaks":[]}]}"#
    );
    let r = Records::decode(text);
    let mut gui = key("TestCompile", "/ck", &["aterm-gui"]);
    assert_eq!(r.rows().len(), 3, "{r:?}");
    assert_eq!(r.rows()[0].key, gui);
    assert_eq!(
        r.rows()[0].peaks,
        vec![
            Peak {
                bytes: 16_106_127_360,
                at_ms: 1_793_278_342_337
            },
            Peak {
                bytes: 18_897_856_102,
                at_ms: 1_793_279_342_337
            }
        ]
    );
    gui.stage = "Test".to_owned();
    gui.variant = Some("cells linux-arm".to_owned());
    assert_eq!(r.rows()[1].key, gui);
    assert_eq!(r.rows()[2].key.crates, None);
    assert_eq!(r.rows()[2].key.variant, None);
    assert_eq!(Records::decode(&r.encode()), r);
    assert_eq!(
        Records::decode(&text.replace("\"schema\":2", "\"schema\":1")),
        Records::default()
    );
    assert_eq!(Records::decode("not json"), Records::default());
    assert_eq!(
        Records::decode(&text.replace(
            "\"checkout\":\"/ck\",\"crates\":[\"aterm-gui\"]}",
            "\"crates\":[]}"
        )),
        Records::default()
    );
}

/// A file written by [`Records::keep`] is read back whole, and a second keep adds to it.
/// NEGATIVE CONTROL: a path whose folder is a file cannot be kept, and says so.
#[test]
fn a_kept_peak_is_read_back_from_its_file() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let path = dir.path().join("state/verify/targo-memory.json");
    let k = key("targo test", "/ck", &["a"]);
    Records::keep(&path, k.clone(), 3 * GIB, 1).expect("kept");
    Records::keep(&path, k.clone(), 5 * GIB, 2).expect("kept again");
    let read = Records::load(&path).learned(&k).map(|l| (l.peak, l.runs));
    assert_eq!(read, Some((5 * GIB, 2)));
    let blocked = dir.path().join("file");
    std::fs::write(&blocked, b"x").expect("a file");
    assert!(Records::keep(&blocked.join("targo-memory.json"), k.clone(), GIB, 3).is_err());
    #[cfg(unix)]
    {
        let link = dir.path().join("record-link.json");
        std::os::unix::fs::symlink(&path, &link).expect("record symlink");
        assert_eq!(Records::load(&link), Records::default());
        let lock_link = lock_path(&dir.path().join("guarded.json"));
        std::os::unix::fs::symlink(&blocked, &lock_link).expect("lock symlink");
        assert!(Records::keep(&dir.path().join("guarded.json"), k, GIB, 4).is_err());
    }
}

fn book_past_the_read_bound() -> Records {
    let mut records = Records::default();
    let long = "feature".repeat(4_682);
    for i in 0..ROWS_KEPT {
        let mut k = key("targo test", "/ck", &["a"]);
        k.variant = Some(format!("features {i}-{long}"));
        records.push(k, 64 * GIB, i as u64);
    }
    assert!(
        records.encode().len() > 16 * 1024 * 1024,
        "oversized fixture"
    );
    records
}

#[test]
fn an_oversized_book_save_preserves_the_previous_readable_peaks() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let path = dir.path().join("book.json");
    let k = key("targo test", "/ck", &["a"]);
    let previous = with(&[(k.clone(), 64)]);
    previous.save(&path).expect("ordinary readable book");
    let bytes = std::fs::read(&path).expect("previous bytes");
    assert_eq!(Records::load(&path), previous, "ordinary round trip");
    assert_eq!(
        std::fs::read_dir(dir.path()).expect("listed").count(),
        1,
        "no stage folder is left beside it"
    );
    assert_eq!(
        Records::load(&dir.path().join("absent")),
        Records::default()
    );
    let result = book_past_the_read_bound().save(&path);
    assert_eq!(
        result
            .expect_err("a successful save must remain readable")
            .kind(),
        std::io::ErrorKind::FileTooLarge
    );
    assert_eq!(std::fs::read(&path).expect("preserved bytes"), bytes);
    assert_eq!(Records::load(&path).learned(&k).unwrap().peak, 64 * GIB);
}

#[test]
fn a_read_refused_book_is_not_relearned_as_empty_by_an_update() {
    for kind in ["oversized", "non-text", "directory", "symlink"] {
        if kind == "symlink" && !cfg!(unix) {
            continue;
        }
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let path = dir.path().join("book.json");
        let target = dir.path().join("target.json");
        let before = match kind {
            "oversized" => Some(book_past_the_read_bound().encode().into_bytes()),
            "non-text" => Some(vec![0xff, 0xfe]),
            "symlink" => {
                #[cfg(unix)]
                {
                    std::fs::write(&target, b"retained target").expect("target");
                    std::os::unix::fs::symlink(&target, &path).expect("book symlink");
                }
                None
            }
            "directory" => {
                std::fs::create_dir(&path).expect("directory book");
                None
            }
            _ => unreachable!(),
        };
        if let Some(bytes) = &before {
            std::fs::write(&path, bytes).expect("existing refused book");
        }
        assert_eq!(
            Records::load(&path),
            Records::default(),
            "soft lookup: {kind}"
        );
        let mut changed = false;
        let result = Records::update(&path, |records| {
            changed = true;
            records.push(key("targo test", "/ck", &["a"]), GIB, 3);
            true
        });
        assert!(
            result.is_err(),
            "{kind}: update must preserve refused evidence"
        );
        assert!(!changed, "{kind}: no callback sees a fabricated empty book");
        if let Some(bytes) = before {
            assert_eq!(
                std::fs::read(&path).expect("preserved bytes"),
                bytes,
                "{kind}"
            );
        } else if kind == "directory" {
            assert!(path.is_dir());
            assert_eq!(std::fs::read_dir(&path).unwrap().count(), 0);
        } else {
            assert!(
                std::fs::symlink_metadata(&path)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert_eq!(std::fs::read(&target).unwrap(), b"retained target");
        }
    }
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let path = dir.path().join("new/book.json");
    let k = key("targo test", "/ck", &["a"]);
    Records::keep(&path, k.clone(), 3 * GIB, 1).expect("missing book can be created");
    Records::keep(&path, k.clone(), 5 * GIB, 2).expect("readable book can be updated");
    assert_eq!(Records::load(&path).learned(&k).unwrap().peak, 5 * GIB);
}

const MIB: u64 = 1 << 20;

/// The chain reads every record of the same work together, the key's own, the key's in another
/// checkout and verify's analogue, and the most of them answers, whatever order they were kept
/// in; a tie is the build's own. NEGATIVE CONTROL: with the heavier ones gone, the lighter
/// answers, and the wider selection's record is never read beside them.
#[test]
fn the_same_works_records_are_read_together_and_the_most_answers() {
    let own = key("targo test", "/ck", &["a"]);
    let like = key("TestCompile", "/ck", &["a"]);
    let verify = with(&[(like.clone(), 20)]);
    let records = with(&[
        (own.clone(), 4),
        (own.clone(), 2),
        (key("targo test", "/ck", &["a", "b"]), 30),
        (key("targo test", "/other", &["a"]), 11),
    ]);
    assert_eq!(
        chain(&records, &own, Some((&verify, &like))),
        Some(Found {
            peak: 20 * GIB,
            runs: 1,
            via: Via::Analogue(like.clone())
        })
    );
    assert_eq!(
        chain(&records, &own, None),
        Some(Found {
            peak: 11 * GIB,
            runs: 1,
            via: Via::Elsewhere(key("targo test", "/other", &["a"]))
        })
    );
    let here = with(&[
        (own.clone(), 4),
        (own.clone(), 2),
        (key("targo test", "/ck", &["a", "b"]), 30),
    ]);
    assert_eq!(
        chain(&here, &own, None),
        Some(Found {
            peak: 4 * GIB,
            runs: 2,
            via: Via::Own
        })
    );
    let tie = with(&[(own.clone(), 20)]);
    assert_eq!(
        chain(&tie, &own, Some((&verify, &like))).map(|f| f.via),
        Some(Via::Own)
    );
}

/// A light run of the same work, a fresh build that compiled nothing, never bounds the build
/// below a heavy record of that work: neither its own light row nor a light row of the whole
/// workspace in its checkout hides verify's heavy analogue. NEGATIVE CONTROL: with the heavy
/// analogue gone, each light figure answers.
#[test]
fn a_light_record_never_hides_a_heavy_one_of_the_same_work() {
    let own = key("targo test", "/ck", &["aterm-gui"]);
    let like = key("TestCompile", "/ck", &["aterm-gui"]);
    let verify = with(&[(like.clone(), 17)]);
    let mut light = Records::default();
    light.push(own.clone(), 3 * MIB, 1);
    assert_eq!(
        chain(&light, &own, Some((&verify, &like))).map(|f| (f.peak, f.via)),
        Some((17 * GIB, Via::Analogue(like.clone())))
    );
    assert_eq!(
        chain(&light, &own, None).map(|f| (f.peak, f.via)),
        Some((3 * MIB, Via::Own))
    );
    let workspace = Key {
        crates: None,
        ..own.clone()
    };
    let mut wide = Records::default();
    wide.push(workspace.clone(), 3 * MIB, 1);
    assert_eq!(
        chain(&wide, &own, Some((&verify, &like))).map(|f| (f.peak, f.via)),
        Some((17 * GIB, Via::Analogue(like)))
    );
    assert_eq!(
        chain(&wide, &own, None).map(|f| (f.peak, f.via)),
        Some((3 * MIB, Via::Wider(workspace)))
    );
}

/// Where nothing of the same work is kept, the most of a wider selection's records in the same
/// checkout answers. NEGATIVE CONTROL: a narrower selection, or the key in another variant or
/// another selection elsewhere, is no rule (a wider selection in another checkout answers after
/// it: [`a_wider_selection_elsewhere_answers_last_and_claims_at_most_the_cold_figure`]).
#[test]
fn a_wider_selection_in_the_checkout_answers_only_for_work_never_kept() {
    let own = key("targo test", "/ck", &["a"]);
    let records = with(&[
        (key("targo test", "/ck", &["a", "b"]), 9),
        (key("targo test", "/ck", &["a", "c"]), 7),
    ]);
    assert_eq!(
        chain(&records, &own, None),
        Some(Found {
            peak: 9 * GIB,
            runs: 1,
            via: Via::Wider(key("targo test", "/ck", &["a", "b"]))
        })
    );
    let release = Key {
        variant: Some("release".to_owned()),
        ..key("targo test", "/one", &["a"])
    };
    let apart = with(&[
        (key("targo test", "/ck", &[]), 3),
        (release, 6),
        (key("targo test", "/one", &["b"]), 6),
    ]);
    assert_eq!(chain(&apart, &own, None), None);
}

/// A NEW WORKTREE'S BUILD READS A WIDER SELECTION ELSEWHERE: where nothing of the same work is
/// kept, nor a wider selection's in its checkout, the most of a wider selection's records of the
/// same work and variant in another checkout answers ([`Via::WiderElsewhere`]). On this 48 GiB
/// Mac, `targo test -p aterm-sysprobe` in /vwider, with `-p aterm-release -p aterm-sysprobe` kept
/// at 3 and then 4.55 GiB in /sw-trust and `-p aterm-sysprobe -p aterm-time` at 2 GiB in /far,
/// claims 4.55 GiB and a fifth more, and says so: the most of the two wider selections, the
/// heavy one over the light one. A wider selection's in its own checkout wins over it, however
/// light. A wider peak elsewhere whose claim passes the cold figure claims the cold figure, and
/// says so; another variant's, another work's, a narrower selection's or the workspace's record
/// elsewhere (another repository's root build, perhaps) is no rule. NEGATIVE CONTROLS, by hand:
/// with the rule taken out of [`chain`], it found nothing for the first build (`None`), which
/// claims the cold figure; with the workspace let in ([`Key::wider_elsewhere`] without its
/// `crates.is_some()`), the last `chain` found the workspace's 9 GiB.
#[test]
fn a_wider_selection_elsewhere_answers_last_and_claims_at_most_the_cold_figure() {
    let own = key("targo test", "/vwider", &["aterm-sysprobe"]);
    let pair = key(
        "targo test",
        "/sw-trust",
        &["aterm-release", "aterm-sysprobe"],
    );
    let measured = 4_885_525_299; // 4.55 GiB.
    let mut records = Records::default();
    records.push(pair.clone(), 3 * GIB, 1);
    records.push(pair.clone(), measured, 2);
    records.push(
        key("targo test", "/far", &["aterm-sysprobe", "aterm-time"]),
        2 * GIB,
        3,
    );
    assert_eq!(
        chain(&records, &own, None),
        Some(Found {
            peak: measured,
            runs: 2,
            via: Via::WiderElsewhere(pair.clone())
        })
    );
    let n = need(&records, &own, None, Some(48 * GIB));
    assert_eq!(n.bytes, crate::peak_claim(measured), "{n}");
    assert!(
        n.to_string().contains(&format!(
            "the most a build of a wider selection (-p aterm-release -p aterm-sysprobe) in \
             /sw-trust used ({}) and a fifth more, at least 1 GiB",
            crate::size(measured)
        )),
        "{n}"
    );
    let mut here = records.clone();
    let near = key("targo test", "/vwider", &["aterm-sysprobe", "aterm-time"]);
    here.push(near.clone(), GIB, 4);
    assert_eq!(
        chain(&here, &own, None).map(|f| (f.peak, f.via)),
        Some((GIB, Via::Wider(near))),
        "a wider selection's in its own checkout"
    );
    let heavy = with(&[(pair.clone(), 20)]);
    let n = need(&heavy, &own, None, Some(48 * GIB));
    assert_eq!(n.bytes, crate::cold_claim(Some(48 * GIB)), "{n}");
    assert!(n.to_string().contains("at most the cold figure"), "{n}");
    let n = need(&heavy, &own, None, Some(24 * GIB));
    assert_eq!(n.bytes, 12 * GIB, "half of physical memory");
    let apart = with(&[
        (
            Key {
                variant: Some("release".to_owned()),
                ..pair.clone()
            },
            9,
        ),
        (
            key(
                "targo check",
                "/sw-trust",
                &["aterm-release", "aterm-sysprobe"],
            ),
            9,
        ),
        (key("targo test", "/sw-trust", &[]), 9),
        (
            Key {
                crates: None,
                ..key("targo test", "/astream", &[])
            },
            9,
        ),
    ]);
    assert_eq!(chain(&apart, &own, None), None);
}

/// Verify's analogue is read as verify reads it, then elsewhere: its own record, a wider
/// selection's in its checkout, then the same in another checkout. NEGATIVE CONTROL: with no
/// analogue named, nothing is found.
#[test]
fn verifys_analogue_is_read_by_verifys_rules_then_elsewhere() {
    let own = key("targo test", "/new", &["aterm-gui"]);
    let none = Records::default();
    let like = key("TestCompile", "/new", &["aterm-gui"]);
    for (verify, from, gib) in [
        (with(&[(like.clone(), 15)]), like.clone(), 15),
        (
            with(&[(
                key("TestCompile", "/new", &["aterm-effects", "aterm-gui"]),
                21,
            )]),
            key("TestCompile", "/new", &["aterm-effects", "aterm-gui"]),
            21,
        ),
        (
            with(&[(key("TestCompile", "/vmem", &["aterm-gui"]), 17)]),
            key("TestCompile", "/vmem", &["aterm-gui"]),
            17,
        ),
    ] {
        assert_eq!(
            chain(&none, &own, Some((&verify, &like))),
            Some(Found {
                peak: gib * GIB,
                runs: 1,
                via: Via::Analogue(from)
            })
        );
        assert_eq!(chain(&none, &own, None), None);
    }
}

/// The last rule: with nothing found the cold figure, 16 GiB and at most half of physical
/// memory; with a peak found, it and a fifth more, at least 1 GiB more. Each says why.
/// NEGATIVE CONTROL: the found figure without its margin would be the peak itself.
#[test]
fn a_build_with_nothing_kept_claims_the_cold_figure() {
    let own = key("targo test", "/ck", &["a"]);
    let cold = need(&Records::default(), &own, None, Some(24 * GIB));
    assert_eq!(cold.bytes, 12 * GIB);
    assert!(cold.to_string().contains("cold"), "{cold}");
    let kept = with(&[(own.clone(), 10)]);
    let learned = need(&kept, &own, None, Some(24 * GIB));
    assert_eq!(learned.bytes, 12 * GIB, "10 GiB and a fifth");
    assert_ne!(learned.bytes, 10 * GIB);
    assert!(
        learned.to_string().contains("its last 1 build(s)"),
        "{learned}"
    );
    let verify = with(&[(key("Tippy", "/vmem", &["a"]), 15)]);
    let like = key("Tippy", "/ck", &["a"]);
    let via = need(
        &Records::default(),
        &key("targo tippy", "/ck", &["a"]),
        Some((&verify, &like)),
        None,
    );
    assert_eq!(via.bytes, 18 * GIB);
    assert!(
        via.to_string().contains("verify's Tippy of -p a in /vmem"),
        "{via}"
    );
}

/// A plain build's light runs, fresh builds that compiled nothing, never push its heavy peak
/// out of its record while that peak is young: eight runs of 50 MiB after a 10 GiB one keep
/// nothing, and the record still reads 10 GiB; a run of a quarter of it or more is kept, and once
/// the heavy peak is older than [`HEAVY_STANDS_MS`] a light run is kept again. NEGATIVE CONTROL:
/// the same light runs kept as verify keeps its own ([`Records::keep`]) read 50 MiB.
#[test]
fn a_plain_builds_light_runs_never_push_its_heavy_peak_out() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let path = dir.path().join("targo-memory.json");
    let plain = dir.path().join("verify-like.json");
    let k = key("targo check", "/ck", &["aterm-gui"]);
    assert!(Records::keep_unless_light(&path, k.clone(), 10 * GIB, 1).expect("kept"));
    Records::keep(&plain, k.clone(), 10 * GIB, 1).expect("kept");
    for at in 2..=9 {
        assert!(
            !Records::keep_unless_light(&path, k.clone(), 50 * MIB, at).expect("read"),
            "a light run at {at}"
        );
        Records::keep(&plain, k.clone(), 50 * MIB, at).expect("kept");
    }
    let read = |p: &Path| Records::load(p).learned(&k).map(|l| (l.peak, l.runs));
    assert_eq!(read(&path), Some((10 * GIB, 1)));
    assert_eq!(read(&plain), Some((50 * MIB, KEPT)));
    assert!(Records::keep_unless_light(&path, k.clone(), 3 * GIB, 10).expect("kept"));
    assert_eq!(read(&path), Some((10 * GIB, 2)));
    let later = 1 + HEAVY_STANDS_MS;
    assert!(Records::keep_unless_light(&path, k.clone(), 50 * MIB, later).expect("kept"));
    assert_eq!(read(&path), Some((10 * GIB, 3)));
    assert!(
        !Records::default().light(&k, 1, 1),
        "a key with no record has no heavy peak"
    );
}

/// A key reads back whole from the one line of JSON it writes, with or without its selection and
/// variant. NEGATIVE CONTROL: text that holds no key (no stage, a number for the checkout, not
/// JSON) reads as none.
#[test]
fn a_key_reads_back_from_its_json() {
    let mut k = key("targo test", "/w/ck", &["a", "b"]);
    k.variant = Some("--tests".to_owned());
    assert_eq!(Key::from_json(&k.to_json()), Some(k.clone()));
    k.crates = None;
    k.variant = None;
    assert_eq!(Key::from_json(&k.to_json()), Some(k));
    for text in [
        r#"{"checkout":"/w"}"#,
        r#"{"stage":"s","checkout":7}"#,
        "key",
    ] {
        assert_eq!(Key::from_json(text), None, "{text}");
    }
}

/// A WRITER HOLDS THE RECORD'S LOCK FROM ITS READ TO ITS WRITE: while `update` changes the
/// records, another handle of the lock beside the file cannot take it, so a second writer, in this
/// process or another, waits for the first and then reads what it wrote; once `update` returns,
/// the lock is free, and a second keep adds its row beside the first. NEGATIVE CONTROL, by hand:
/// with the lock not taken, the other handle took it inside the change.
#[test]
fn a_writer_holds_the_records_lock_from_its_read_to_its_write() {
    use aterm_toolchain::record_lock::RecordLock;
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let path = dir.path().join("verify/stage-memory.json");
    let other = || {
        std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(lock_path(&path))
            .expect("the lock file")
    };
    let wrote = Records::update(&path, |records| {
        let taken = RecordLock::try_lock(other(), 0);
        assert!(
            matches!(taken, Err(std::fs::TryLockError::WouldBlock)),
            "{taken:?}"
        );
        records.push(key("TestCompile", "/a", &["x"]), GIB, 1);
        true
    })
    .expect("written");
    assert!(wrote);
    let free = RecordLock::try_lock(other(), 0);
    assert!(free.is_ok(), "{free:?}");
    drop(free);
    Records::keep(&path, key("TestCompile", "/b", &["x"]), 2 * GIB, 2).expect("kept");
    assert_eq!(Records::load(&path).rows().len(), 2);
}

/// A NEW WORKTREE'S BUILD READS VERIFY'S WIDER SELECTION ELSEWHERE: `targo test -p
/// aterm-sysprobe` in /vwider, with no plain build of a selection that holds it kept anywhere,
/// and verify's test compile of `-p aterm-release -p aterm-sysprobe` kept at 4.55 GiB in
/// /sw-trust (the measured case), claims that and a fifth more, and says so
/// ([`Via::AnalogueWiderElsewhere`]). Beside a plain build's wider record elsewhere, the more of
/// the two answers, a tie the build's own; a heavy one claims at most the cold figure. Verify's
/// wider record in its own checkout is read before it, as verify reads it. NEGATIVE CONTROL, by
/// hand: with the analogue's wider record elsewhere unread, `chain` found nothing (`None`), the
/// cold figure.
#[test]
fn a_new_worktrees_build_reads_verifys_wider_selection_elsewhere() {
    let own = key("targo test", "/vwider", &["aterm-sysprobe"]);
    let like = key("TestCompile", "/vwider", &["aterm-sysprobe"]);
    let pair = key(
        "TestCompile",
        "/sw-trust",
        &["aterm-release", "aterm-sysprobe"],
    );
    let measured = 4_885_525_299; // 4.55 GiB.
    let mut verify = Records::default();
    verify.push(pair.clone(), measured, 1);
    let none = Records::default();
    assert_eq!(
        chain(&none, &own, Some((&verify, &like))),
        Some(Found {
            peak: measured,
            runs: 1,
            via: Via::AnalogueWiderElsewhere(pair.clone())
        })
    );
    let n = need(&none, &own, Some((&verify, &like)), Some(48 * GIB));
    assert_eq!(n.bytes, crate::peak_claim(measured), "{n}");
    assert!(
        n.to_string().contains(&format!(
            "the most verify's TestCompile of a wider selection (-p aterm-release -p \
             aterm-sysprobe) in /sw-trust used ({}) and a fifth more, at least 1 GiB",
            crate::size(measured)
        )),
        "{n}"
    );
    let plain_pair = key("targo test", "/far", &["aterm-sysprobe", "aterm-time"]);
    for (gib, want) in [
        (6, Via::WiderElsewhere(plain_pair.clone())),
        (2, Via::AnalogueWiderElsewhere(pair.clone())),
    ] {
        let plain = with(&[(plain_pair.clone(), gib)]);
        assert_eq!(
            chain(&plain, &own, Some((&verify, &like))).map(|f| f.via),
            Some(want),
            "{gib} GiB"
        );
    }
    let mut tie = Records::default();
    tie.push(plain_pair.clone(), measured, 2);
    assert_eq!(
        chain(&tie, &own, Some((&verify, &like))).map(|f| f.via),
        Some(Via::WiderElsewhere(plain_pair)),
        "a tie is the build's own"
    );
    let heavy = with(&[(pair.clone(), 20)]);
    let n = need(&none, &own, Some((&heavy, &like)), Some(48 * GIB));
    assert_eq!(n.bytes, crate::cold_claim(Some(48 * GIB)), "{n}");
    let mut near = verify.clone();
    let here = key("TestCompile", "/vwider", &["aterm-sysprobe", "aterm-time"]);
    near.push(here.clone(), GIB, 2);
    assert_eq!(
        chain(&none, &own, Some((&near, &like))).map(|f| (f.peak, f.via)),
        Some((GIB, Via::Analogue(here))),
        "verify's wider record in its own checkout"
    );
}

/// A PLAIN BUILD REFUSES THE WIDER RECORD ELSEWHERE VERIFY REFUSES: verify keeps its test compile
/// of `-p a` at 10 GiB in /one and of `-p a -p b -p c` at 0.5 GiB in /far. A `targo test -p a -p
/// b` in /new, with nothing of its own kept, reads the wider record by verify's one rule
/// ([`Records::wider_elsewhere`]): `-p a`, inside its selection, kept more, so that wider run was
/// warm, and the build claims the cold figure, as verify's own test compile there would. The
/// same holds for its own book's records of the same work, and for its own book's wider record
/// where only verify kept the narrower one: a wider `targo test` of no variant built all that
/// verify's test compile of `-p a` did. NEGATIVE CONTROL, by hand: with the narrower records'
/// refusal taken out of [`Records::wider_elsewhere`] (the unfiltered read this rule had before),
/// the first two builds claimed 1.5 GiB; with [`chain`]'s own floor left at the unit floor (verify's
/// narrower records not read for the build's own wider record), the third claimed 2 GiB.
#[test]
fn a_plain_build_refuses_the_wider_record_elsewhere_verify_refuses() {
    let own = key("targo test", "/new", &["a", "b"]);
    let like = key("TestCompile", "/new", &["a", "b"]);
    let mut verify = with(&[(key("TestCompile", "/one", &["a"]), 10)]);
    verify.push(key("TestCompile", "/far", &["a", "b", "c"]), GIB / 2, 2);
    assert!(key("TestCompile", "/far", &["a", "b", "c"]).wider_elsewhere(&like));
    let none = Records::default();
    let cold = crate::cold_claim(Some(48 * GIB));
    let n = need(&none, &own, Some((&verify, &like)), Some(48 * GIB));
    assert_eq!((n.bytes, n.found), (cold, None), "verify's records");
    let mut plain = with(&[(key("targo test", "/one", &["a"]), 10)]);
    plain.push(key("targo test", "/far", &["a", "b", "c"]), GIB / 2, 2);
    let n = need(&plain, &own, None, Some(48 * GIB));
    assert_eq!((n.bytes, n.found), (cold, None), "its own book's records");
    let narrow = with(&[(key("TestCompile", "/one", &["a"]), 10)]);
    let wide = with(&[(key("targo test", "/far", &["a", "b", "c"]), 1)]);
    let n = need(&wide, &own, Some((&narrow, &like)), Some(48 * GIB));
    assert_eq!(
        (n.bytes, n.found),
        (cold, None),
        "verify's narrower, its own wider"
    );
    let lib = |k: Key| Key {
        variant: Some("--lib".to_owned()),
        ..k
    };
    let wide_lib = with(&[(lib(key("targo test", "/far", &["a", "b", "c"])), 1)]);
    let n = need(&wide_lib, &lib(own), Some((&narrow, &like)), Some(48 * GIB));
    assert_eq!(
        n.bytes,
        crate::peak_claim(GIB),
        "a --lib build's wider --lib record: {n}"
    );
}

/// A TEST BUILD REFUSES A WIDER RECORD ELSEWHERE LIGHTER THAN A UNIT OF ITS CRATES: verify keeps
/// its test compile of `-p a -p b` at 2 GiB in /far and the lib-test unit of `a` at 5 GiB in /old
/// ([`UNIT_STAGE`]). A cold test compile of `-p a` compiles that unit, so the wider run was warm:
/// `targo test -p a` in /new claims the cold figure, whether the wider record is verify's
/// analogue's or its own book's `targo test` of no variant. Where the unit floor does not hold,
/// the wider record still answers: a `targo test --lib` reads its own book's wider `--lib` record
/// (its run built no test target), and a lint reads verify's lint, which keeps no unit's peak.
/// NEGATIVE CONTROL, by hand: with [`Records::unit_floor`] answering 0, the first build claimed
/// 3 GiB, the wider record's 2 GiB and 1 GiB more.
#[test]
fn a_test_build_refuses_a_wider_record_elsewhere_lighter_than_a_unit_of_its_crates() {
    let unit = Key {
        variant: Some("a lib".to_owned()),
        ..key(UNIT_STAGE, "/old", &["a"])
    };
    let mut verify = with(&[(unit, 5)]);
    verify.push(key(TEST_COMPILE, "/far", &["a", "b"]), 2 * GIB, 2);
    verify.push(key("Tippy", "/far", &["a", "b"]), 2 * GIB, 3);
    let none = Records::default();
    let own = key("targo test", "/new", &["a"]);
    let like = key(TEST_COMPILE, "/new", &["a"]);
    let cold = crate::cold_claim(Some(48 * GIB));
    assert_eq!(verify.unit_floor(&like), 5 * GIB);
    let n = need(&none, &own, Some((&verify, &like)), Some(48 * GIB));
    assert_eq!((n.bytes, &n.found), (cold, &None), "verify's analogue: {n}");
    let plain = with(&[(key("targo test", "/far", &["a", "b"]), 2)]);
    let n = need(&plain, &own, Some((&verify, &like)), Some(48 * GIB));
    assert_eq!((n.bytes, &n.found), (cold, &None), "its own book's: {n}");
    let lib = |k: Key| Key {
        variant: Some("--lib".to_owned()),
        ..k
    };
    let plain_lib = with(&[(lib(key("targo test", "/far", &["a", "b"])), 2)]);
    let n = need(
        &plain_lib,
        &lib(own),
        Some((&verify, &like)),
        Some(48 * GIB),
    );
    assert_eq!(n.bytes, crate::peak_claim(2 * GIB), "a --lib build: {n}");
    let lint = key("Tippy", "/new", &["a"]);
    let n = need(
        &none,
        &key("targo tippy", "/new", &["a"]),
        Some((&verify, &lint)),
        Some(48 * GIB),
    );
    assert_eq!(n.bytes, crate::peak_claim(2 * GIB), "a lint: {n}");
}

/// EACH RULE SAYS WHERE ITS FIGURE WAS READ: a wider selection in the build's own checkout names
/// the selection and no checkout; its key in another checkout names that checkout; and verify's
/// analogue's wider record elsewhere, past the cold figure, names it and says its claim is at most
/// the cold figure. NEGATIVE CONTROL: the found figure's own rule's text is not another's (the
/// wider selection's text names no checkout, ` in /`).
#[test]
fn each_rule_says_where_its_figure_was_read() {
    let own = key("targo test", "/ck", &["a"]);
    let wider = need(
        &with(&[(key("targo test", "/ck", &["a", "b"]), 3)]),
        &own,
        None,
        Some(48 * GIB),
    );
    let text = wider.to_string();
    assert!(
        text.contains("the most a build of a wider selection (-p a -p b) used (3 GiB)"),
        "{text}"
    );
    assert!(!text.contains(" in /"), "{text}");
    let far = need(
        &with(&[(key("targo test", "/other", &["a"]), 3)]),
        &own,
        None,
        Some(48 * GIB),
    );
    assert!(
        far.to_string()
            .contains("the most a build of it in /other used (3 GiB) and a fifth more"),
        "{far}"
    );
    let verify = with(&[(key(TEST_COMPILE, "/far", &["a", "b"]), 20)]);
    let like = key(TEST_COMPILE, "/ck", &["a"]);
    let capped = need(
        &Records::default(),
        &own,
        Some((&verify, &like)),
        Some(48 * GIB),
    );
    assert_eq!(capped.bytes, crate::cold_claim(Some(48 * GIB)), "{capped}");
    assert!(
        capped.to_string().contains(
            "the most verify's TestCompile of a wider selection (-p a -p b) in /far used (20 GiB) \
             and a fifth more, at most the cold figure (16 GiB, at most half of physical memory)"
        ),
        "{capped}"
    );
}
