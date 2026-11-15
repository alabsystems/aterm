// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The landing handoff's encoding, pinned byte for byte, and every refusal of its reader.

use super::*;
use aterm_types::land_trailer::{Checked, Interrupted, Row};

/// The schema-3 bytes of [`judge_sample`]. A change to the encoding bumps [`SCHEMA`] and
/// replaces this literal.
const SCHEMA_3: &str = concat!(
    r#"{"at":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","#,
    r#""base":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","#,
    r#""branch":{"crates":["aterm-verify"]},"composes":0,"#,
    r#""cone":"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee","hop":1,"#,
    r#""image":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","#,
    r#""root":"/scratch/checkout","#,
    r#""run":{"cause":{"tip":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","#,
    r#""why":"it shares x","word":"cut"},"#,
    r#""id":"99999999999999999999999999999999","seq":3,"started":1763000000},"#,
    r#""schema":3,"#,
    r#""stated":"Aterm-Interrupted: dddddddddddddddddddddddddddddddddddddddd "#,
    r#"base=eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee "#,
    r#"tip=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb secs=42\n","#,
    r#""step":"judge","words":["--admit","now","--test-cores","4"]}"#,
    "\n"
);

fn id(c: char, len: usize) -> String {
    std::iter::repeat_n(c, len).collect()
}

/// A judgment's handoff after one interrupted judgment: the re-judge of the merge `a` over
/// the tip `b` that interrupted the judgment of `d` over `e`, the landing's cone.
fn judge_sample() -> Handoff {
    Handoff {
        root: PathBuf::from("/scratch/checkout"),
        image: id('c', 64),
        words: ["--admit", "now", "--test-cores", "4"]
            .map(str::to_owned)
            .to_vec(),
        step: Step::Qualify,
        hop: 1,
        at: id('a', 40),
        base: id('b', 40),
        composes: 0,
        cone: id('e', 40),
        branch: Some(Branch::Crates(["aterm-verify".to_owned()].into())),
        stated: Stated {
            interrupted: vec![Interrupted {
                tree: id('d', 40),
                base: id('e', 40),
                tip: id('b', 40),
                secs: 42,
            }],
            ..Stated::default()
        },
        run: Some(Carried {
            id: id('9', 32),
            started: 1_763_000_000,
            seq: 3,
            cause: Some(crate::land_run::Cause::new(
                "cut",
                &id('b', 40),
                "it shares x",
            )),
        }),
    }
}

/// A recheck's handoff, its second: the merge `a` of the tip `b` after a passed judgment and
/// two compositions.
fn recheck_sample() -> Handoff {
    Handoff {
        step: Step::Recheck,
        hop: 2,
        composes: 2,
        branch: Some(Branch::Everything("a shared input changed".into())),
        stated: Stated {
            checked: vec![Checked {
                tree: id('d', 40),
                rows: vec![Row {
                    check: "test".into(),
                    word: "ok".into(),
                }],
                base: id('e', 40),
                secs: 7,
            }],
            composed_onto: Some(id('f', 40)),
            ..Stated::default()
        },
        ..judge_sample()
    }
}

/// The run object of a handoff's map.
fn run(map: &mut Map) -> &mut Map {
    map.get_mut("run").and_then(Value::as_object_mut).unwrap()
}

/// The cause object of a handoff's run.
fn cause(map: &mut Map) -> &mut Map {
    run(map)
        .get_mut("cause")
        .and_then(Value::as_object_mut)
        .unwrap()
}

/// The sample's encoding with its object edited by `edit`, re-encoded compactly.
fn edited(sample: &Handoff, edit: impl FnOnce(&mut Map)) -> Vec<u8> {
    let mut value: Value = aterm_json::from_slice(&sample.encode().unwrap()).unwrap();
    edit(value.as_object_mut().unwrap());
    format!("{value}\n").into_bytes()
}

/// The pinned bytes are the judgment sample's encoding and decode to it; a recheck's handoff,
/// with its second hop, composition count, a branch reaching every crate and a composed
/// receipt, round-trips too, as does a landing's first judgment (no receipt, no crates, its
/// cone its base, its run's first judgment with no cause). So does a re-judge of a landing
/// resumed from a kept judgment: the earlier run's receipt and no branch crates, which it reads
/// over its cone. And so does a hop past [`super::super::HOPS`], another build's bound (the
/// receiver hands it on no further).
#[test]
fn a_handoff_encodes_to_its_pinned_schema_bytes_and_reads_back() {
    let sample = judge_sample();
    assert_eq!(
        String::from_utf8(sample.encode().unwrap()).unwrap(),
        SCHEMA_3
    );
    assert_eq!(Handoff::decode(SCHEMA_3.as_bytes()), Ok(sample));
    let recheck = recheck_sample();
    assert_eq!(Handoff::decode(&recheck.encode().unwrap()), Ok(recheck));
    let first = Handoff {
        cone: id('b', 40),
        branch: None,
        stated: Stated::default(),
        run: Some(Carried {
            seq: 0,
            cause: None,
            ..judge_sample().run.unwrap()
        }),
        ..judge_sample()
    };
    assert_eq!(Handoff::decode(&first.encode().unwrap()), Ok(first));
    let resumed = Handoff {
        branch: None,
        stated: Stated {
            checked: recheck_sample().stated.checked,
            ..judge_sample().stated
        },
        ..judge_sample()
    };
    assert_eq!(Handoff::decode(&resumed.encode().unwrap()), Ok(resumed));
    let beyond = Handoff {
        hop: super::super::HOPS + 1,
        ..recheck_sample()
    };
    assert_eq!(Handoff::decode(&beyond.encode().unwrap()), Ok(beyond));
}

/// THE TRANSITION: a branch cut before this build reached main hands its landing over in its
/// own schema, at the first merge that brings this build in. Schema-2 bytes, as that sender
/// wrote them (this schema's fields but the run), read as the same handoff with no run, and
/// schema-1 bytes (those fields and a count of re-judges) the same with the count dropped, for
/// a judgment and for a recheck alike: the landing goes on instead of failing once in every
/// open worktree, and its receiver starts a run of its own. NEGATIVE CONTROL: the same bytes
/// under a schema this build does not read are refused by name. Mutant run: a schema-2 handoff
/// held to this schema's run (it lacked its `run` field).
#[test]
fn a_handoff_of_the_schemas_before_reads_as_this_one_without_its_run() {
    for (sample, rejudges) in [(judge_sample(), 1_u64), (recheck_sample(), 0)] {
        let runless = Handoff {
            run: None,
            ..sample.clone()
        };
        let before = edited(&sample, |map| {
            map.insert("schema".into(), Value::from(2_u64));
            map.remove("run");
        });
        assert_eq!(Handoff::decode(&before), Ok(runless.clone()));
        let earlier = edited(&sample, |map| {
            map.insert("schema".into(), Value::from(1_u64));
            map.remove("run");
            map.insert("rejudges".into(), Value::from(rejudges));
        });
        assert_eq!(Handoff::decode(&earlier), Ok(runless));
        let unread = edited(&sample, |map| {
            map.insert("schema".into(), Value::from(0_u64));
            map.remove("run");
            map.insert("rejudges".into(), Value::from(rejudges));
        });
        let refused = Handoff::decode(&unread).unwrap_err();
        assert!(refused.contains("handed over in schema 0"), "{refused}");
    }
}

/// Each refusal names what it refuses: a schema this build does not read, a missing or unknown
/// field (the earliest schema's re-judge count missing from it, no count, or held by this
/// schema; this schema's run missing from it, or held by an earlier one), a run that does not
/// read (an id that is no 32 hex digits, a cause with a tip that is no object id or an extra
/// field, a start that is no count, an ordinal past 32 bits), a commit
/// that is no object id, a relative root, an image that is no digest, an unknown step, a hop
/// that is no count from 1, a composition count that is no count, branch crates before any
/// judgment, a composition count at a
/// judgment, a recheck with no passed judgment, a cone other than the base before any judgment,
/// bytes that are not JSON and bytes that are not the handoff's own encoding. Mutant run: a
/// run's id held only to 32 characters (one of uppercase hex read).
#[test]
fn a_handoff_the_reader_cannot_honour_is_refused_by_name() {
    let judge = judge_sample();
    let recheck = recheck_sample();
    let set =
        |key: &'static str, value: Value| move |map: &mut Map| drop(map.insert(key.into(), value));
    let unjudged_cone = format!(
        "its cone {} is not its base {}, before any judgment",
        id('e', 40),
        id('b', 40)
    );
    let cases: Vec<(Vec<u8>, &str)> = vec![
        (
            edited(&judge, set("schema", Value::from(4_u64))),
            "it was handed over in schema 4; this tree's verify land reads schemas 1 to 3",
        ),
        // The earliest schema's own fields are held to it: its re-judge count is neither
        // missing nor anything but a count, and this schema holds none.
        (
            edited(&judge, |map| {
                map.insert("schema".into(), Value::from(1_u64));
                map.remove("run");
            }),
            "it lacks its `rejudges` field",
        ),
        (
            edited(&judge, |map| {
                map.insert("schema".into(), Value::from(1_u64));
                map.remove("run");
                map.insert("rejudges".into(), Value::from("0"));
            }),
            "its `rejudges` is not a count",
        ),
        // This schema's run is held to it: missing from it, or held by an earlier one.
        (
            edited(&judge, |map| drop(map.remove("run"))),
            "it lacks its `run` field",
        ),
        (
            edited(&judge, set("schema", Value::from(2_u64))),
            "it holds an unknown field `run`",
        ),
        (
            edited(&judge, |map| {
                drop(run(map).insert("id".into(), Value::from(id('A', 32))))
            }),
            "its `run` does not read",
        ),
        (
            edited(&judge, |map| {
                drop(cause(map).insert("tip".into(), Value::from("main")));
            }),
            "its `run` does not read",
        ),
        (
            edited(&judge, |map| {
                drop(cause(map).insert("shared".into(), Value::from("")));
            }),
            "its `run` does not read",
        ),
        (
            edited(&judge, |map| {
                drop(run(map).insert("started".into(), Value::from("1")))
            }),
            "its `run` does not read",
        ),
        (
            edited(&judge, |map| {
                drop(run(map).insert("seq".into(), Value::from(u64::from(u32::MAX) + 1)));
            }),
            "its `run` does not read",
        ),
        (
            edited(&judge, |map| drop(map.remove("schema"))),
            "it names no schema",
        ),
        (
            edited(&judge, |map| drop(map.remove("words"))),
            "it lacks its `words` field",
        ),
        (
            edited(&judge, set("extra", Value::Null)),
            "it holds an unknown field `extra`",
        ),
        (
            edited(&judge, set("at", Value::from("HEAD"))),
            "its `at` `HEAD` is not a git object id",
        ),
        (
            edited(&judge, set("root", Value::from("scratch/checkout"))),
            "its root scratch/checkout is not absolute",
        ),
        (
            edited(&judge, set("image", Value::from(id('C', 64)))),
            "is not a sha256 digest",
        ),
        (
            edited(&judge, set("step", Value::from("push"))),
            "its step `push` is neither judge nor recheck",
        ),
        (
            edited(&judge, set("words", Value::from(vec![Value::from(4_u64)]))),
            "its `words` are not a list of strings",
        ),
        (
            edited(&judge, set("cone", Value::from("main"))),
            "its `cone` `main` is not a git object id",
        ),
        (
            edited(&recheck, set("composes", Value::from("2"))),
            "its `composes` is not a count",
        ),
        (
            edited(&judge, set("rejudges", Value::from(1_u64))),
            "it holds an unknown field `rejudges`",
        ),
        (
            edited(&judge, set("stated", Value::from(""))),
            "it carries branch crates, but its receipt holds no judgment",
        ),
        (
            edited(&judge, |map| {
                drop(map.insert("branch".into(), Value::Null));
                drop(map.insert("stated".into(), Value::from("")));
            }),
            &unjudged_cone,
        ),
        (
            edited(&judge, set("stated", Value::from("Aterm-Checked: tree\n"))),
            "its receipt does not read",
        ),
        (
            edited(&judge, set("composes", Value::from(1_u64))),
            "a judgment's handoff carries a composition count (1)",
        ),
        (
            edited(&judge, set("hop", Value::from(0_u64))),
            "its `hop` is not a count from 1",
        ),
        (
            edited(&judge, set("hop", Value::from("1"))),
            "its `hop` is not a count from 1",
        ),
        (
            edited(
                &recheck,
                set("stated", Value::from(judge.stated.trailers())),
            ),
            "a recheck's receipt holds no passed judgment",
        ),
        (b"not json\n".to_vec(), "it is not JSON"),
        (b"[1]\n".to_vec(), "it is not a JSON object"),
        (
            SCHEMA_3.replacen(",", ", ", 1).into_bytes(),
            "it is not in the handoff's own encoding",
        ),
        (
            SCHEMA_3.trim_end().as_bytes().to_vec(),
            "it is not in the handoff's own encoding",
        ),
    ];
    for (bytes, why) in cases {
        let refused = Handoff::decode(&bytes).expect_err(why);
        assert!(refused.contains(why), "{refused} lacks {why}");
    }
}

/// Taking a handoff removes its file whether or not it decodes; a file that is not there is
/// refused as such.
#[test]
fn a_handoff_is_taken_once_whether_or_not_it_reads() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let file = scratch.path().join(FILE);
    write(&file, &judge_sample()).unwrap();
    assert_eq!(Handoff::take(&file), Ok(judge_sample()));
    assert!(!file.exists());
    std::fs::write(&file, SCHEMA_3.replace(r#""schema":3"#, r#""schema":4"#)).unwrap();
    let refused = Handoff::take(&file).unwrap_err();
    assert!(refused.contains("schema 4"), "{refused}");
    assert!(!file.exists());
    assert_eq!(Handoff::take(&file), Err("it is not there".into()));
}

/// A handoff the receiver refuses still names the run it carried ([`Handoff::run_of`]), so the
/// receiver ends that run: from bytes of a schema it does not read, or with a field it does not
/// know, the run reads as it would from the handoff it decodes. NEGATIVE CONTROLS: bytes that
/// are no JSON object, a handoff of the schema before (no run) and a run that does not read
/// (an id that is not 32 lowercase hex digits) name none.
#[test]
fn a_refused_handoff_still_names_the_run_it_carried() {
    let sample = judge_sample();
    let carried = sample.run.clone();
    let newer = edited(&sample, |map| {
        map.insert("schema".into(), Value::from(9_u64));
    });
    assert!(Handoff::decode(&newer).is_err());
    assert_eq!(Handoff::run_of(&newer), carried);
    let unknown = edited(&sample, |map| {
        map.insert("later".into(), Value::from(true));
    });
    assert!(Handoff::decode(&unknown).is_err());
    assert_eq!(Handoff::run_of(&unknown), carried);
    assert_eq!(Handoff::run_of(&sample.encode().unwrap()), carried);

    assert_eq!(Handoff::run_of(b"not json"), None);
    let before = edited(&sample, |map| {
        map.remove("run");
    });
    assert_eq!(Handoff::run_of(&before), None);
    let empty = edited(&sample, |map| {
        run(map).insert("id".into(), Value::from(""));
    });
    assert_eq!(Handoff::run_of(&empty), None);
}

/// A run always has an id a handoff carries: the system's random digits where they were drawn,
/// else 32 lowercase hex digits of this process, the moment and the host, which a handoff of the
/// run encodes and decodes whole. NEGATIVE CONTROL, run once as a mutant: the id `live` gave
/// before (`unwrap_or_default`, an empty id where none was drawn) did not encode into a handoff
/// its receiver reads.
#[test]
fn a_run_id_that_could_not_be_drawn_still_carries_across_a_handoff() {
    let drawn = super::super::records::run_id(Ok(id('7', 32)), 1, 2, "host");
    assert_eq!(drawn, id('7', 32));
    let fallback = super::super::records::run_id(
        Err(std::io::Error::other("no random bytes")),
        4242,
        1_763_000_000_000_000_000,
        "host",
    );
    assert!(aterm_codec::hex::is_lower(&fallback, 32), "{fallback}");
    let other = super::super::records::run_id(
        Err(std::io::Error::other("no random bytes")),
        4243,
        1_763_000_000_000_000_000,
        "host",
    );
    assert_ne!(fallback, other, "another process draws another id");
    let mut sample = judge_sample();
    if let Some(run) = sample.run.as_mut() {
        run.id.clone_from(&fallback);
    }
    let read = Handoff::decode(&sample.encode().unwrap()).expect("the handoff reads");
    assert_eq!(read.run.map(|run| run.id), Some(fallback));
}
