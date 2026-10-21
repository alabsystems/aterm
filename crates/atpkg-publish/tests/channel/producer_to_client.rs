// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The producer and the client verifier in one process. The producer self-checks with the
//! client's own chain (`quad::verify_dir`), so a producer that emits what clients refuse
//! cannot finish: the real indexer's quad verifies, every plausible mutation is refused, and
//! the pre-checks refuse the common mistakes before anything is signed, naming the fix.

use atpkg_publish::api::{Baseline, Delta, IndexRequest};
use atpkg_publish::channel::index::{parse_delta, publish_with, render};
use atpkg_publish::quad::{self, QUAD_FILES};
use atpkg_publish::testkit::Keys;

use super::{Doc, Rig};

fn dry(rig: &Rig, base: &str, out: &str, delta: Delta) -> IndexRequest {
    let dir = rig.path(base);
    rig.quad_dir(&dir, &Doc::new(6, &[("ay", 5)]));
    IndexRequest {
        baseline: Baseline::Dir(dir),
        delta,
        out: rig.path(out),
        upload: false,
        pack_dir: None,
    }
}

#[test]
fn the_real_producer_emits_a_quad_the_real_client_accepts() {
    let rig = Rig::new("p2c-happy");
    let req = dry(&rig, "base", "out", Delta::default());
    let mut log = Rig::log();
    let o =
        publish_with(&rig.ctx, &req, &mut log).unwrap_or_else(|e| panic!("{e}\n{}", log.text()));
    for f in QUAD_FILES {
        assert!(req.out.join(f).is_file(), "the publish set carries {f}");
    }
    assert_eq!(
        std::fs::read(req.out.join("aterm-machines.toml")).expect("roster"),
        rig.keys.roster_bytes,
        "the roster travels unmodified"
    );
    let index = std::fs::read_to_string(req.out.join("index.toml")).expect("index");
    assert!(index.contains("\nschema = 2\n"), "{index}");
    assert!(index.contains("\nmachine_id = \"m3\"\n"), "{index}");
    assert!(
        index.contains(&format!("\nroster_seq = {}\n", rig.keys.roster.roster_seq)),
        "{index}"
    );
    assert!(
        !index.contains("[keys]") && !index.contains("release_key"),
        "{index}"
    );
    // The client agrees, run independently of the producer.
    let q = quad::verify_dir(&rig.ctx, &req.out).expect("the client accepts it");
    assert_eq!(q.index.attribution().machine_id, "m3");
    assert_eq!(q.index.index_build, o.published.index_build);
    assert_eq!(
        index,
        render(&o.plan),
        "what was verified is what was planned"
    );
}

/// The mutations: each is the producer's own output with one plausible edit, signed as
/// the producer signs, and each is refused by the verifier the producer self-checks with.
#[test]
fn every_retired_or_broken_shape_is_refused_by_the_self_check() {
    let rig = Rig::new("p2c-mutants");
    let req = dry(&rig, "base", "out", Delta::default());
    let mut log = Rig::log();
    let o = publish_with(&rig.ctx, &req, &mut log).expect("publishes");
    let good = render(&o.plan);
    let id_line = "machine_id = \"m3\"\n";
    let seq_line = format!("roster_seq = {}\n", rig.keys.roster.roster_seq);
    assert!(good.contains(id_line) && good.contains(&seq_line));

    // (a) the retired schema-1 shape: a [keys] table, no attribution. The schema number alone
    // stops nothing (SUPPORTED_SCHEMA rejects only newer); the attribution bind does.
    let retired = good
        .replace("schema = 2\n", "schema = 1\n")
        .replace(id_line, "")
        .replace(&seq_line, "")
        .replace(
            "\n[programs.",
            "\n[keys]\nrelease_key_id = \"rk-7\"\n\n[programs.",
        );
    // (b) no machine_id: an index nobody can be held to.
    let unattributed = good.replace(id_line, "");
    for (label, text) in [("retired shape", retired), ("no machine_id", unattributed)] {
        let dir = rig.path(&format!("mutant-{}", label.replace(' ', "-")));
        rig.keys.write_quad(&dir, &text);
        assert!(
            quad::verify_dir(&rig.ctx, &dir).is_err(),
            "{label} must be refused"
        );
    }
    // (c) signed by a key nobody rostered: a flawless body, a foreign signature.
    let (outsider, _) = atpkg_keys::generate().expect("keypair");
    let dir = rig.path("mutant-outsider");
    rig.keys.write_quad(&dir, &good);
    std::fs::write(
        dir.join("index.toml.sig"),
        atpkg_keys::sign(&outsider, good.as_bytes()).expect("sign"),
    )
    .expect("sig");
    assert!(
        quad::verify_dir(&rig.ctx, &dir).is_err(),
        "an unrostered key's index"
    );
    // Non-vacuity: the unmutated bytes verify.
    let dir = rig.path("control");
    rig.keys.write_quad(&dir, &good);
    quad::verify_dir(&rig.ctx, &dir).expect("the control verifies");
}

#[test]
fn a_machine_the_roster_does_not_name_is_refused_before_anything_is_signed() {
    let rig = Rig::new("p2c-stranger");
    let req = dry(&rig, "base", "out", Delta::default());
    let (key, public) = atpkg_keys::generate().expect("keypair");
    std::fs::write(&rig.ctx.identity.key, &key).expect("key");
    std::fs::write(
        &rig.ctx.identity.record,
        format!("id = \"m99\"\npubkey = \"{public}\"\n"),
    )
    .expect("record");
    let e = publish_with(&rig.ctx, &req, &mut Rig::log()).expect_err("refused");
    assert!(e.to_string().contains("NOT on the roster"), "{e}");
    assert!(!req.out.join("index.toml").exists(), "nothing signed");
}

#[test]
fn an_unarmed_anchor_stops_the_publish_rather_than_signing_into_the_void() {
    let mut rig = Rig::new("p2c-unarmed");
    let req = dry(&rig, "base", "out", Delta::default());
    rig.ctx.anchor.clear();
    let e = publish_with(&rig.ctx, &req, &mut Rig::log()).expect_err("refused");
    assert!(
        e.to_string().contains("PAPER_MASTER_PUBKEYS is EMPTY"),
        "{e}"
    );
    assert!(!req.out.join("index.toml").exists());
}

/// The index's own horizon is the client's freshness gate, which `atpkg verify-index` does not
/// check: a lapsed or unreadable stamp is refused before a signature, because the upload
/// consumes a tag.
#[test]
fn a_lapsed_or_malformed_horizon_never_reaches_a_signature() {
    let rig = Rig::new("p2c-horizon");
    for (stamp, why) in [
        ("2020-01-01T00:00:00Z", "LAPSED"),
        ("next tuesday", "not UTC RFC3339"),
        ("2099-01-01T00:00:00+00:00", "not UTC RFC3339"),
    ] {
        let delta = parse_delta(&["--valid-until".to_string(), stamp.to_string()]).expect("delta");
        let req = dry(&rig, "base", "out", delta);
        let e = publish_with(&rig.ctx, &req, &mut Rig::log()).expect_err(stamp);
        assert!(e.to_string().contains(why), "{stamp}: {e}");
        assert!(!req.out.join("index.toml").exists(), "{stamp}");
    }
}

/// The roster generation is a no-downgrade floor too: an older pair on this machine never
/// replaces the generation already published beside the head.
#[test]
fn the_roster_shipped_is_the_newest_and_never_below_the_heads() {
    let rig = Rig::new("p2c-roster");
    let now = u64::try_from(atpkg_publish::testkit::NOW).expect("clock");
    let (_, m4) = atpkg_keys::generate().expect("keypair");
    let newer = atpkg_keys::roster_ops::add(rig.keys.roster.clone(), "m4", &m4, now).expect("add");
    let newer_bytes = newer.to_toml().expect("emit").into_bytes();
    let newer_sig = rig.keys.master_sign(&newer_bytes);
    // The head ships the newer generation; this machine still has the older pair installed.
    let head = rig.path("head");
    rig.keys
        .write_quad(&head, &Doc::new(6, &[("ay", 5)]).render(&rig.keys));
    std::fs::write(head.join("aterm-machines.toml"), &newer_bytes).expect("roster");
    std::fs::write(head.join("aterm-machines.toml.sig"), &newer_sig).expect("sig");
    let installed = &rig.ctx.identity.roster;
    std::fs::create_dir_all(installed.parent().expect("dir")).expect("mkdir");
    std::fs::write(installed, &rig.keys.roster_bytes).expect("older");
    std::fs::write(format!("{}.sig", installed.display()), &rig.keys.roster_sig).expect("sig");
    let req = IndexRequest {
        baseline: Baseline::Dir(head),
        delta: Delta::default(),
        out: rig.path("out"),
        upload: false,
        pack_dir: None,
    };
    let mut log = Rig::log();
    let o =
        publish_with(&rig.ctx, &req, &mut log).unwrap_or_else(|e| panic!("{e}\n{}", log.text()));
    assert_eq!(o.plan.roster_seq, newer.roster_seq);
    assert_eq!(
        std::fs::read(req.out.join("aterm-machines.toml")).expect("staged"),
        newer_bytes
    );
}

/// A baseline from before the roster scheme carries no `roster_seq`: the client chain
/// refuses it, so nothing is built over it.
#[test]
fn a_pre_roster_baseline_is_refused() {
    let rig = Rig::new("p2c-preroster");
    let text = Doc::new(6, &[("ay", 5)])
        .render(&rig.keys)
        .lines()
        .filter(|l| !l.starts_with("roster_seq"))
        .collect::<Vec<_>>()
        .join("\n");
    let base = rig.path("base");
    rig.keys.write_quad(&base, &text);
    let req = IndexRequest {
        baseline: Baseline::Dir(base),
        delta: Delta::default(),
        out: rig.path("out"),
        upload: false,
        pack_dir: None,
    };
    assert!(publish_with(&rig.ctx, &req, &mut Rig::log()).is_err());
    assert!(!req.out.join("index.toml").exists());
}

#[test]
fn a_revoked_machine_is_diagnosed_as_revoked_and_not_sent_to_rejoin() {
    let keys = Keys::mint("m3", &["m3"]);
    let rig = Rig::with_keys("p2c-revoked", keys);
    let installed = &rig.ctx.identity.roster;
    std::fs::create_dir_all(installed.parent().expect("dir")).expect("mkdir");
    std::fs::write(installed, &rig.keys.roster_bytes).expect("roster");
    std::fs::write(format!("{}.sig", installed.display()), &rig.keys.roster_sig).expect("sig");
    let req = IndexRequest {
        baseline: Baseline::First {
            channel: "stable".into(),
            index_build: 1,
        },
        delta: parse_delta(&[
            "--valid-until".to_string(),
            "2098-01-01T00:00:00Z".to_string(),
        ])
        .expect("delta"),
        out: rig.path("out"),
        upload: false,
        pack_dir: None,
    };
    let e = publish_with(&rig.ctx, &req, &mut Rig::log())
        .expect_err("revoked")
        .to_string();
    assert!(e.contains("is REVOKED on this roster"), "{e}");
    assert!(
        !e.contains("is NOT on the roster"),
        "never diagnosed as never added: {e}"
    );
    assert!(!req.out.join("index.toml").exists());
}

/// No `$HOME` in anything an operator will paste beside a public release.
#[test]
fn a_successful_publish_prints_no_absolute_home_path() {
    let rig = Rig::new("p2c-home");
    rig.publish_head(&Doc::new(21, &[("ay", 5)]));
    rig.lay_pack("alabsystems", "ay", 5, &["aarch64-apple-darwin"]);
    let req = IndexRequest {
        baseline: Baseline::ChannelHead,
        delta: Delta::default(),
        out: rig.ctx.home.join("dist"),
        upload: true,
        pack_dir: None,
    };
    let mut log = Rig::log();
    publish_with(&rig.ctx, &req, &mut log).unwrap_or_else(|e| panic!("{e}\n{}", log.text()));
    let home = rig.ctx.home.display().to_string();
    assert!(!log.text().contains(&home), "{}", log.text());
}
