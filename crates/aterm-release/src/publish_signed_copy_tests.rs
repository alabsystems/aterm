// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const VERSION: &str = "0.5.0";
const BUILD: u64 = 500;

fn key() -> SignedKey {
    SignedKey {
        artifact: "k".repeat(64),
        bundle_digest: "d".repeat(64),
        macho_sha256: "m".repeat(64),
        version: VERSION.into(),
        build: BUILD,
        commit: "c".repeat(40),
        team: "TEAMID1234".into(),
        identity: Some("F".repeat(40)),
        signature_required: true,
        update_pin: Some("p".repeat(64)),
    }
}

/// A `dist/` holding a signed copy of build [`BUILD`]: its staged bundle, both containers,
/// their twins and sidecars, and its provenance; and the record `cut --sign` keeps of it.
fn signed_dist() -> (aterm_tempfile::TempDir, PathBuf, SignedRecord) {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-signed-copy-")
        .tempdir()
        .unwrap();
    let dist = dir.path().join("dist");
    let app = bundle::staged_app_path(&dist, BUILD);
    fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
    fs::write(app.join("Contents/MacOS/aterm"), b"signed executable").unwrap();
    fs::write(app.join("Contents/Info.plist"), b"<plist/>").unwrap();
    for (name, bytes) in [
        (channel::dmg_asset_name(VERSION), &b"the notarized dmg"[..]),
        (channel::zip_asset_name(VERSION), b"the stapled zip"),
        (channel::provenance_asset_name(VERSION), b"version=0.5.0\n"),
        (channel::stable_dmg_asset_name(), b"the notarized dmg"),
        (channel::stable_zip_asset_name(), b"the stapled zip"),
        (
            channel::sha256_sidecar_name(&channel::dmg_asset_name(VERSION)),
            b"sum",
        ),
    ] {
        fs::write(dist.join(name), bytes).unwrap();
    }
    let record = SignedRecord {
        format: SIGNED_FORMAT,
        key: key(),
        outputs: signed_outputs(&dist, VERSION, BUILD).unwrap(),
    };
    write_signed_record(&dist, &record).unwrap();
    (dir, dist, record)
}

#[test]
fn a_record_adopts_only_its_own_bytes_under_its_own_key() {
    let (_dir, dist, record) = signed_dist();
    assert_eq!(
        read_signed_record(&dist, BUILD).unwrap().as_ref(),
        Some(&record),
        "the record reads back as written"
    );
    assert_eq!(signed_copy_stands(&record, &key(), &dist), Ok(()));

    // NEGATIVE CONTROLS: one changed byte of any output, or any field of the key, signs again.
    let dmg = dist.join(channel::dmg_asset_name(VERSION));
    fs::write(&dmg, b"the notarized dmG").unwrap();
    let why = signed_copy_stands(&record, &key(), &dist).unwrap_err();
    assert!(why.starts_with("the DMG is "), "{why}");
    fs::write(&dmg, b"the notarized dmg").unwrap();
    assert_eq!(signed_copy_stands(&record, &key(), &dist), Ok(()));

    let exe = bundle::staged_app_path(&dist, BUILD).join("Contents/MacOS/aterm");
    fs::write(&exe, b"signed executablE").unwrap();
    let why = signed_copy_stands(&record, &key(), &dist).unwrap_err();
    assert!(why.starts_with("the signed bundle is "), "{why}");
    fs::write(&exe, b"signed executable").unwrap();

    let zip = dist.join(channel::zip_asset_name(VERSION));
    fs::write(&zip, b"the stapled ziP").unwrap();
    let why = signed_copy_stands(&record, &key(), &dist).unwrap_err();
    assert!(why.starts_with("the zip is "), "{why}");
    fs::write(&zip, b"the stapled zip").unwrap();
    let provenance = dist.join(channel::provenance_asset_name(VERSION));
    fs::write(&provenance, b"version=0.5.1\n").unwrap();
    let why = signed_copy_stands(&record, &key(), &dist).unwrap_err();
    assert!(why.starts_with("the provenance is "), "{why}");
    fs::write(&provenance, b"version=0.5.0\n").unwrap();
    assert_eq!(signed_copy_stands(&record, &key(), &dist), Ok(()));
    type Size = fn(&mut SignedOutputs);
    let sizes: [(&str, Size); 2] = [
        ("the DMG's size", |o| o.dmg_size += 1),
        ("the zip's size", |o| o.zip_size += 1),
    ];
    for (what, change) in sizes {
        let mut other = record.clone();
        change(&mut other.outputs);
        let why = signed_copy_stands(&other, &key(), &dist).unwrap_err();
        assert!(why.starts_with(&format!("{what} is ")), "{what}: {why}");
    }

    type Change = fn(&mut SignedKey);
    let changes: [(&str, Change); 10] = [
        ("bundle", |k| k.artifact = "a".repeat(64)),
        ("bundle digest", |k| k.bundle_digest = "e".repeat(64)),
        ("executable", |k| k.macho_sha256 = "n".repeat(64)),
        ("version", |k| k.version = "0.5.1".into()),
        ("build", |k| k.build += 1),
        ("release commit", |k| k.commit = "b".repeat(40)),
        ("team", |k| k.team = "OTHERTEAM1".into()),
        ("Developer ID", |k| k.identity = None),
        ("manifest signature requirement", |k| {
            k.signature_required = false
        }),
        ("update pin", |k| k.update_pin = None),
    ];
    for (field, change) in changes {
        let mut other = key();
        change(&mut other);
        let why = signed_copy_stands(&record, &other, &dist).unwrap_err();
        assert!(
            why.contains(&format!("signed as {field} ")),
            "{field}: {why}"
        );
    }
    let mut old = record.clone();
    old.format = SIGNED_FORMAT + 1;
    assert!(signed_copy_stands(&old, &key(), &dist).is_err());
}

/// NEGATIVE CONTROLS: writing signed.toml during preparation makes it adoptable before
/// READY; executing the callback then overlaps the bundle's windowed checks.
#[test]
fn early_packaging_cannot_be_adopted_until_its_local_check_passes() {
    let (_dir, dist, record) = signed_dist();
    forget_signed_record(&dist, BUILD).unwrap();
    finish_private_copy(&dist, &record, true, || {
        panic!("windowed check before READY")
    })
    .unwrap();
    assert!(read_signed_record(&dist, BUILD).unwrap().is_none());
    assert_eq!(
        prepared_record(&dist, &record.key).unwrap(),
        Some(record.clone())
    );

    // An interrupted self-check creates no adoptable record. The same prepared bytes
    // survive and can be checked on resume without another signing operation.
    assert!(finish_private_copy(&dist, &record, false, || Err(Error::new("interrupted"))).is_err());
    assert!(read_signed_record(&dist, BUILD).unwrap().is_none());
    assert_eq!(
        prepared_record(&dist, &record.key).unwrap(),
        Some(record.clone())
    );
    finish_private_copy(&dist, &record, false, || Ok(())).unwrap();
    assert_eq!(read_signed_record(&dist, BUILD).unwrap(), Some(record));
    assert!(!prepared_record_path(&dist, BUILD).exists());
}

/// NEGATIVE CONTROL: skipping the prepared key or output comparison admits different
/// bytes after interruption. A changed executable key must refuse, never sign anew.
#[test]
fn prepared_bytes_refuse_a_changed_executable_and_discard_on_bundle_red() {
    let (_dir, dist, record) = signed_dist();
    forget_signed_record(&dist, BUILD).unwrap();
    finish_private_copy(&dist, &record, true, || panic!("no self-check")).unwrap();
    let mut changed = record.key.clone();
    changed.macho_sha256 = "other executable".into();
    assert!(
        prepared_record(&dist, &changed)
            .unwrap_err()
            .0
            .contains("executable")
    );
    fs::write(
        bundle::staged_app_path(&dist, BUILD).join("Contents/MacOS/aterm"),
        b"changed",
    )
    .unwrap();
    assert!(prepared_record(&dist, &record.key).is_err());
    assert!(read_signed_record(&dist, BUILD).unwrap().is_none());
    assert!(
        discard_signed(
            &dist,
            VERSION,
            BUILD,
            crate::release::SignedDiscard::CancelPreparation
        )
        .complete()
    );
    assert!(prepared_record(&dist, &record.key).unwrap().is_none());
    assert!(read_signed_record(&dist, BUILD).unwrap().is_none());
}

#[test]
fn selfcheck_cannot_certify_outputs_that_changed_while_it_ran() {
    let (_dir, dist, record) = signed_dist();
    forget_signed_record(&dist, BUILD).unwrap();
    assert!(
        finish_private_copy(&dist, &record, false, || {
            fs::write(
                dist.join(channel::dmg_asset_name(VERSION)),
                b"different dmg",
            )
            .unwrap();
            Ok(())
        })
        .is_err()
    );
    assert!(read_signed_record(&dist, BUILD).unwrap().is_none());
}

/// A RECORD THAT CANNOT BE READ IS NO RECORD OF THESE BYTES: the claim removes it and signs
/// again, rather than refusing on every resume. NEGATIVE CONTROL: passing the read's error on
/// leaves the cut stuck behind the file.
#[test]
fn an_unreadable_record_is_removed_and_the_copy_signed_again() {
    let (_dir, dist, record) = signed_dist();
    let read = |dist: &Path| read_signed_record(dist, BUILD).transpose().unwrap();
    assert_eq!(
        standing_record(read(&dist), &key(), &dist).unwrap(),
        Some(record)
    );
    fs::write(signed_record_path(&dist, BUILD), b"format = \"two\"\n").unwrap();
    assert_eq!(standing_record(read(&dist), &key(), &dist).unwrap(), None);
    assert_eq!(
        read_signed_record(&dist, BUILD),
        Ok(None),
        "its record is removed"
    );
}

/// `CUT --SIGN` RECORDS ONLY THE OUTPUTS IT MADE: a DMG or zip in `dist/` other than the one
/// it signed and packaged is never recorded for a claim to adopt. NEGATIVE CONTROL: recording
/// what `dist/` holds at the end records the other zip.
#[test]
fn cut_sign_records_only_the_outputs_it_made() {
    let (_dir, _dist, record) = signed_dist();
    let made = SignedCopy {
        app: PathBuf::new(),
        dmg: PathBuf::new(),
        dmg_sha256: record.outputs.dmg_sha256.clone(),
        zip_sha256: record.outputs.zip_sha256.clone(),
    };
    assert!(outputs_as_signed(&record.outputs, &made).is_ok());
    let other = SignedCopy {
        zip_sha256: "0".repeat(64),
        ..made
    };
    let why = outputs_as_signed(&record.outputs, &other)
        .unwrap_err()
        .to_string();
    assert!(why.contains("the zip in dist/"), "{why}");
}

/// `CUT --SIGN` STANDS ASIDE ONLY FROM AN UNFINISHED CUT: an earlier release's finished cut
/// journal is the next fresh cut's to clear, so a clone signs its next release beside it.
/// NEGATIVE CONTROL: refusing beside any journal stops every release after a clone's first.
#[test]
fn cut_sign_stands_aside_only_from_an_unfinished_cut() {
    let header = |done: &[&str]| JournalHeader {
        format: 1,
        version: VERSION.into(),
        build_number: BUILD,
        commit: "c".repeat(40),
        done: done.iter().map(|s| (*s).to_string()).collect(),
    };
    let path = Path::new("dist/cut-state.toml");
    assert!(sign_beside_journal(None, path).is_ok());
    assert!(
        sign_beside_journal(Some(&header(&["claim", "unlock"])), path).is_ok(),
        "an earlier release's finished cut stands aside"
    );
    let why = sign_beside_journal(Some(&header(&["claim"])), path)
        .unwrap_err()
        .to_string();
    assert!(
        why.contains("--resume") && why.contains("Nothing was signed"),
        "{why}"
    );
}

#[test]
fn a_discarded_copy_leaves_nothing_of_itself_in_dist() {
    let (_dir, dist, _) = signed_dist();
    let kept = dist.join("notes-0.5.0.md");
    fs::write(&kept, b"notes").unwrap();
    assert_eq!(
        discard_signed(
            &dist,
            VERSION,
            BUILD,
            crate::release::SignedDiscard::LostReservation
        ),
        crate::release::Discarded::Except(Vec::new())
    );
    assert_eq!(read_signed_record(&dist, BUILD), Ok(None));
    assert!(!dist.join(bundle::staging_dir_name(BUILD)).exists());
    for name in [
        channel::dmg_asset_name(VERSION),
        channel::zip_asset_name(VERSION),
        channel::provenance_asset_name(VERSION),
        channel::stable_dmg_asset_name(),
        channel::stable_zip_asset_name(),
        channel::sha256_sidecar_name(&channel::dmg_asset_name(VERSION)),
    ] {
        assert!(!dist.join(&name).exists(), "{name} is left");
    }
    assert!(kept.exists(), "what is not the copy's stays");
    assert_eq!(
        discard_signed(
            &dist,
            VERSION,
            BUILD,
            crate::release::SignedDiscard::LostReservation
        ),
        crate::release::Discarded::Except(Vec::new()),
        "a second discard finds nothing"
    );
}

#[test]
fn only_a_tail_at_or_past_the_reservation_lost_the_race() {
    let ledger = "1791500000 0.5.0\n";
    assert!(lost_the_race(ledger, 1_791_500_000));
    assert!(lost_the_race(ledger, 1_791_499_999));
    assert!(!lost_the_race(ledger, 1_791_500_001));
    assert!(
        !lost_the_race("not a ledger", 1),
        "an unread ledger is no lost race"
    );
}

/// Origin's ledger, as `git show origin/main:RELEASES.ledger` answers it.
struct Origin(&'static str);

impl GitRunner for Origin {
    fn git(&self, args: &[&str]) -> Result<RunOut> {
        assert_eq!(args, ["show", "origin/main:RELEASES.ledger"]);
        Ok(RunOut {
            status: 0,
            stdout: self.0.as_bytes().to_vec(),
            stderr: Vec::new(),
        })
    }
}

/// A claim refused because origin's tail reached the reservation discards the copy and
/// says so; a claim refused for any other cause keeps it for the next run to adopt.
#[test]
fn only_a_claim_that_lost_the_race_discards_the_copy() {
    let (_dir, dist, record) = signed_dist();
    let kept = discard_on_lost_race(
        &Origin("499 0.4.0\n"),
        &dist,
        VERSION,
        BUILD,
        Error::new("the claim's push was refused"),
    );
    assert_eq!(kept.to_string(), "the claim's push was refused");
    assert_eq!(read_signed_record(&dist, BUILD), Ok(Some(record)));
    assert!(dist.join(channel::dmg_asset_name(VERSION)).exists());

    let lost = discard_on_lost_race(
        &Origin("500 0.5.0\n"),
        &dist,
        VERSION,
        BUILD,
        Error::new("origin's ledger reached build 500"),
    )
    .to_string();
    assert!(
        lost.starts_with(
            "origin's ledger reached build 500. The copy signed, notarized and \
                          packaged privately as build 500 was discarded; nothing of it was \
                          made public"
        ),
        "{lost}"
    );
    assert_eq!(read_signed_record(&dist, BUILD), Ok(None));
    assert!(!dist.join(channel::dmg_asset_name(VERSION)).exists());
    assert!(!dist.join(bundle::staging_dir_name(BUILD)).exists());
}
