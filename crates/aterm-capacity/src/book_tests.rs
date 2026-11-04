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
        r#""checkout":"/ck","crates":["aterm-gui"],"variant":"jobs 8"},"peaks":[{"bytes":"#,
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
    gui.variant = Some("jobs 8".to_owned());
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
/// checkout answers. NEGATIVE CONTROL: a wider selection in another checkout, a narrower one, or
/// the key in another variant or another selection elsewhere is no rule.
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
        (key("targo test", "/other", &["a", "b"]), 9),
        (key("targo test", "/ck", &[]), 3),
        (release, 6),
        (key("targo test", "/one", &["a", "b"]), 6),
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
