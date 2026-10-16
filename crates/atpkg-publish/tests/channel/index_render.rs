// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The rendered index round-trips through the CLIENT: what the writer decided is what
//! `quad::verify_dir` (the client's chain and parser) reads back, every field, and the
//! writer is deterministic. The head it is built over carries only keys it can render.

use std::collections::BTreeMap;

use atpkg::{Channel, Program};
use atpkg_publish::channel::index::{Base, Plan, render};
use atpkg_publish::quad;

use super::{Doc, Rig};

fn program(repo: &str, group: Option<&str>, system: Option<&str>, hint: Option<&str>) -> Program {
    Program {
        repo: repo.into(),
        policy: "prebuilt-only".into(),
        coherence_group: group.map(Into::into),
        system: system.map(Into::into),
        unavailable_hint: hint.map(Into::into),
        requires: Vec::new(),
    }
}

fn plan(rig: &Rig) -> Plan {
    let mut by_target = BTreeMap::new();
    by_target.insert(
        "x86_64-unknown-linux-gnu".to_string(),
        BTreeMap::from([("trust-ir".to_string(), 1100), ("ay".to_string(), 9001)]),
    );
    by_target.insert(
        "aarch64-unknown-linux-gnu".to_string(),
        BTreeMap::from([("ay".to_string(), 9002)]),
    );
    Plan {
        index_build: 42,
        valid_until: "2098-01-01T00:00:00Z".into(),
        machine_id: rig.keys.machine_id.clone(),
        roster_seq: rig.keys.roster.roster_seq,
        channel: Channel {
            name: "stable".into(),
            channel_build: 42,
            min_build: 3,
            min_build_by_program: BTreeMap::from([("ay".to_string(), 8000)]),
            yanked: vec!["ay@8100".into(), "codex@2.1.279".into()],
            pin: BTreeMap::from([
                ("ty".to_string(), 3007),
                ("ay".to_string(), 8256),
                ("trust-ir".to_string(), 1123),
                ("codex".to_string(), 2_026_100_701),
            ]),
            pin_by_target: by_target,
        },
        programs: vec![
            ("ay".into(), program("ay", None, None, None)),
            ("ty".into(), program("ty", None, None, None)),
            (
                "trust-ir".into(),
                program("trust-ir", Some("rustc"), None, None),
            ),
            (
                "codex".into(),
                program(
                    "aterm",
                    None,
                    Some("codex"),
                    Some("a \"quoted\" hint\\with a backslash"),
                ),
            ),
        ],
        moved: Vec::new(),
        added: Vec::new(),
        carried: Vec::new(),
        retired: Vec::new(),
    }
}

#[test]
fn the_rendered_index_reads_back_through_the_client_field_for_field() {
    let rig = Rig::new("render");
    let p = plan(&rig);
    let text = render(&p);
    assert_eq!(text, render(&p), "one deterministic writer");
    let dir = rig.path("quad");
    rig.keys.write_quad(&dir, &text);
    let q = quad::verify_dir(&rig.ctx, &dir).expect("the client chain accepts the render");
    let index = &q.index;
    assert_eq!(index.schema, 2);
    assert_eq!(index.index_build, 42);
    assert_eq!(index.valid_until, p.valid_until);
    assert_eq!(index.attribution().machine_id, p.machine_id);
    assert_eq!(index.roster_seq(), p.roster_seq);
    assert_eq!(index.channels.len(), 1);
    let c = &index.channels[0];
    assert_eq!(c.name, p.channel.name);
    assert_eq!(c.channel_build, 42);
    assert_eq!(c.min_build, 3);
    assert_eq!(c.min_build_by_program, p.channel.min_build_by_program);
    assert_eq!(c.yanked, p.channel.yanked);
    assert_eq!(c.pin, p.channel.pin);
    assert_eq!(c.pin_by_target, p.channel.pin_by_target);
    assert_eq!(index.programs.len(), p.programs.len());
    for (name, want) in &p.programs {
        let got = &index.programs[name];
        assert_eq!(
            (
                &got.repo,
                &got.policy,
                &got.coherence_group,
                &got.system,
                &got.unavailable_hint
            ),
            (
                &want.repo,
                &want.policy,
                &want.coherence_group,
                &want.system,
                &want.unavailable_hint
            ),
            "{name}"
        );
    }
}

/// The live index's shape, byte for byte where it is fixed: blocks and `pin` in table
/// order, no key for an empty overlay, yank list or per-program floor.
#[test]
fn the_render_is_the_live_shape_and_orders_by_the_table() {
    let rig = Rig::new("render-shape");
    let mut p = plan(&rig);
    p.channel.pin_by_target.clear();
    p.channel.yanked.clear();
    p.channel.min_build_by_program.clear();
    let text = render(&p);
    assert!(text.contains("\nschema = 2\n"));
    assert!(
        text.contains("\npin = { ay = 8256, ty = 3007, trust-ir = 1123, codex = 2026100701 }\n"),
        "{text}"
    );
    for absent in ["pin_by_target", "yanked", "min_build_by_program", "[keys]"] {
        assert!(!text.contains(absent), "{absent}: {text}");
    }
    let ay = text.find("[programs.ay]").expect("ay");
    let ty = text.find("[programs.ty]").expect("ty");
    assert!(ay < ty);
    let full = render(&plan(&rig));
    assert!(
        full.contains(
            "pin_by_target = { \"aarch64-unknown-linux-gnu\" = { ay = 9002 }, \
             \"x86_64-unknown-linux-gnu\" = { ay = 9001, trust-ir = 1100 } }"
        ),
        "sorted by target, then program: {full}"
    );
}

#[test]
fn a_head_this_writer_cannot_carry_is_refused_not_thinned() {
    let rig = Rig::new("render-head");
    let mut doc = Doc::new(7, &[("ay", 1)]);
    doc.extra_channel_line = Some("retired_knob = 1".into());
    let dir = rig.path("head");
    rig.quad_dir(&dir, &doc);
    let q = quad::verify_dir(&rig.ctx, &dir).expect("the client accepts an unknown key");
    let err = Base::of_head(&q).expect_err("refused");
    assert!(
        err.to_string()
            .contains("cannot carry forward: retired_knob"),
        "{err}"
    );

    // A program block's key the writer does not render is refused the same way.
    let text = Doc::new(7, &[("ay", 1)]).render(&rig.keys).replacen(
        "[programs.ay]\n",
        "[programs.ay]\nextra = true\n",
        1,
    );
    assert!(text.contains("extra = true"), "{text}");
    let dir = rig.path("program-key");
    rig.keys.write_quad(&dir, &text);
    let q = quad::verify_dir(&rig.ctx, &dir).expect("the client accepts an unknown key");
    let err = Base::of_head(&q).expect_err("refused");
    assert!(
        err.to_string()
            .contains("cannot carry forward: programs.ay.extra"),
        "{err}"
    );

    let mut floors = Doc::new(7, &[("ay", 1)]);
    floors.extra_channel_line = Some("min_build_by_program = { ay = 3 }".into());
    let dir = rig.path("floors");
    rig.quad_dir(&dir, &floors);
    let q = quad::verify_dir(&rig.ctx, &dir).expect("verifies");
    let b = Base::of_head(&q).expect("a per-program floor is carried");
    assert_eq!(b.channel.min_build_by_program.get("ay"), Some(&3));

    let two = format!(
        "{}\n[[channels]]\nname = \"nightly\"\nchannel_build = 7\nmin_build = 0\npin = {{ ay = 1 }}\n",
        Doc::new(7, &[("ay", 1)]).render(&rig.keys)
    );
    let dir = rig.path("two");
    rig.keys.write_quad(&dir, &two);
    let q = quad::verify_dir(&rig.ctx, &dir).expect("verifies");
    let err = Base::of_head(&q).expect_err("refused");
    assert!(
        err.to_string().contains("carries 2 [[channels]] blocks"),
        "{err}"
    );
}
