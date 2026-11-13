// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const MANIFEST: &str = "0123abcd\nschema = 1\n\n[[sessions]]\nlocal_id = 3\nsid = \"s-p\"\nstate = \"running\"\ntitle = \"QZP6SECRET2\"\nattention = \"QZP6SECRET3\"\nattention_owners = [\"- QZP6SECRET3\"]\ntopics = [\"qzp6secret4 since=9\"]\nhold = \"local QZP6SECRET5\"\nlog_ids = \"2 1 0 0\"\n\n[sessions.screen]\nschema = 1\nmeta = '{\"rows\":24,\"title\":{\"window\":\"QZP6SECRET2\",\"icon\":\"QZP6SECRET2\"},\"shell\":{\"output_blocks\":[{\"id\":1,\"span\":{\"commandline\":\"echo QZP6SECRET1 42\"}}]}}'\ngrid_file = \"g\"\nrepaint = false\n\n[[sessions.fabric.posts]]\nid = 1\nto = \"@s-q\"\nkind = \"note\"\nkey = \"QZP6SECRET8w\"\nvia = \"h-qzp6secret9w\"\nbody = \"QZP6SECRET6 waits\"\n";

fn ctl(log: &[&str], archive: &str) -> Vec<u8> {
    let lines = log
        .iter()
        .map(|l| format!("\"{l}\""))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"version\":1,\"turns\":[{{\"id\":1,\"text\":\"sh /private/tmp/x/marked.sh\"}}],\"unheld_below\":0,\"archive\":{{\"rows\":[\"{archive}\"]}},\"log\":[{lines}]}}"
    )
    .into_bytes()
}

const CLEAN_LOG: [&str; 3] = [
    "1 open - session prog=sh",
    "9 mark - topic op=add fp=00ff00ff00ff00ff",
    "12 mark - mail dir=out kind=note to=@s-q via=h-qzp6secret9 idem=QZP6SECRET8",
];

const TYPED: &str = "$ echo QZP6SECRET1 42";

fn file(name: &str, mode: u32, bytes: Vec<u8>) -> KeptFile {
    KeptFile {
        name: name.to_string(),
        mode,
        bytes,
    }
}

fn attempt(log: &[&str]) -> Kept {
    Kept {
        dir: Some((0o700, "/private/tmp/x/run".to_string())),
        files: vec![
            file("seamless-1-ab.toml", 0o600, MANIFEST.as_bytes().to_vec()),
            file(
                "seamless-1-ab.layout.toml",
                0o600,
                b"schema = 1\n[[windows]]\n[[windows.tabs]]\ntitle = \"QZP6SECRET2\"\nholds = [\"local QZP6SECRET5\"]\n".to_vec(),
            ),
            file("seamless-1-ab.s3.ctl", 0o600, ctl(log, TYPED)),
            file(
                "seamless-1-ab.s4.ctl",
                0o600,
                ctl(&["1 open - session prog=sh"], "$ ls"),
            ),
            file(
                "seamless-1-ab.s3.grid",
                0o600,
                format!("\x03\x00\x00\x00$ ran-42\r\n{TYPED}\r\nQZP6SECRET1 42").into_bytes(),
            ),
            file("seamless-1-ab.person", 0o600, b"3 1200\n4 -900\n".to_vec()),
        ],
        skipped: Vec::new(),
    }
}

fn at(kind: Kind, file: &str, place: &str, digits: &[u8]) -> Found {
    Found {
        kind,
        file: file.to_string(),
        at: place.to_string(),
        digits: digits.to_vec(),
    }
}

/// The places `wrong` names as leaks.
fn leaks(wrong: &[Wrong]) -> Vec<Found> {
    wrong
        .iter()
        .filter_map(|w| match w {
            Wrong::Leaks(found) => Some(found.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

#[test]
fn the_sentinels_are_read_in_any_case_by_their_digit() {
    assert_eq!(
        sentinels(b"a QZP6SECRET3 b qzp6secret9 c QzP6sEcReT3"),
        [3, 9]
    );
    assert!(sentinels(b"QZP6SECRET x QZP6SECRETS").is_empty());
}

#[test]
fn a_carry_with_the_session_state_and_a_host_tier_log_passes() {
    let kept = attempt(&CLEAN_LOG);
    let judged = verdict(&kept, "3", &[]);
    assert_eq!(judged.wrong, [], "{:?}", judged.rendered());
    assert!(judged.rendered().is_ok());
    let swept = sweep(&kept, "3");
    assert_eq!(swept.planted_log, BTreeSet::from([8, 9]));
    assert_eq!(swept.planted_log_lines, 3);
    for place in [
        at(Kind::Manifest, ".toml", "sessions[].title", &[2]),
        at(
            Kind::Manifest,
            ".toml",
            "sessions[].attention_owners[]",
            &[3],
        ),
        at(
            Kind::Manifest,
            ".toml",
            "sessions[].screen.meta>title.window",
            &[2],
        ),
        at(
            Kind::Manifest,
            ".toml",
            "sessions[].screen.meta>shell.output_blocks[].span.commandline",
            &[1],
        ),
        at(Kind::Manifest, ".toml", WAITING_BODY, &[6]),
        at(
            Kind::Manifest,
            ".toml",
            "sessions[].fabric.posts[].key",
            &[8],
        ),
        at(
            Kind::Layout,
            ".layout.toml",
            "windows[].tabs[].holds[]",
            &[5],
        ),
        at(Kind::Control, ".s3.ctl", "log[]", &[8, 9]),
        at(Kind::Control, ".s3.ctl", "archive.rows[]", &[1]),
        at(Kind::Screen, ".s3.grid", "-", &[1]),
    ] {
        assert!(swept.found.contains(&place), "{place} in {:?}", swept.found);
    }
}

/// THE NEGATIVE CONTROL the leg names: a Content-tier key planted in the session log's carry
/// (a topic's name, a title, a hold reason) is found, and so is what is never stored (a
/// command's later word, a mail body).
#[test]
fn a_content_tier_key_in_the_log_carry_is_found() {
    for (planted, digit) in [
        (
            "9 mark - topic op=add fp=00ff00ff00ff00ff name=qzp6secret4",
            4,
        ),
        ("10 mark - title title=QZP6SECRET2", 2),
        ("11 mark - hold on reason=QZP6SECRET5", 5),
        ("13 open r1 cmd detail=echo%20QZP6SECRET1", 1),
        ("14 mark - mail dir=out body=QZP6SECRET6", 6),
        ("15 mark - meta field=attention attn_text=QZP6SECRET3", 3),
    ] {
        let mut log = CLEAN_LOG.to_vec();
        log.push(planted);
        let judged = verdict(&attempt(&log), "3", &[]);
        assert_eq!(
            leaks(&judged.wrong),
            [at(Kind::Control, ".s3.ctl", "log[]", &[digit])],
            "{planted}"
        );
        assert!(judged.rendered().is_err(), "{planted}");
    }
}

#[test]
fn a_sentinel_outside_its_field_or_a_loose_mode_or_a_leftover_fails() {
    let mut kept = attempt(&CLEAN_LOG);
    kept.files.push(file(
        "seamless-1-ab.s3.hist",
        0o600,
        b"\x01\x00\x00\x00$ QZP6SECRET1 QZP6SECRET2".to_vec(),
    ));
    assert_eq!(
        verdict(&kept, "3", &[]).wrong,
        [Wrong::Leaks(vec![at(Kind::Screen, ".s3.hist", "-", &[2])])],
        "a title on a screen is not where the plants put it; the echoed line is"
    );
    let mut kept = attempt(&CLEAN_LOG);
    kept.files[2].bytes = ctl(&CLEAN_LOG, "QZP6SECRET5");
    assert_eq!(
        verdict(&kept, "3", &[]).wrong,
        [Wrong::Leaks(vec![at(
            Kind::Control,
            ".s3.ctl",
            "archive.rows[]",
            &[5]
        )])],
        "a hold reason in the archive rows"
    );
    let mut kept = attempt(&CLEAN_LOG);
    kept.files[0].bytes = MANIFEST
        .replace(
            "state = \"running\"",
            "state = \"running\"\ndescription = \"QZP6SECRET3\"",
        )
        .into_bytes();
    assert_eq!(
        verdict(&kept, "3", &[]).wrong,
        [Wrong::Leaks(vec![at(
            Kind::Manifest,
            ".toml",
            "sessions[].description",
            &[3]
        )])],
        "the attention text where only the description is"
    );
    let mut kept = attempt(&CLEAN_LOG);
    kept.files[2].mode = 0o644;
    assert_eq!(
        verdict(&kept, "3", &[]).wrong,
        [Wrong::Loose(vec![Loose::File {
            file: ".s3.ctl".to_string(),
            mode: 0o644
        }])]
    );
    let mut kept = attempt(&CLEAN_LOG);
    kept.dir = Some((0o755, "/private/tmp/x/run".to_string()));
    assert_eq!(
        verdict(&kept, "3", &[]).wrong,
        [Wrong::Loose(vec![Loose::Dir {
            path: "/private/tmp/x/run".to_string(),
            mode: 0o755
        }])]
    );
    let mut kept = attempt(&CLEAN_LOG);
    kept.dir = None;
    assert_eq!(
        verdict(&kept, "3", &[]).wrong,
        [Wrong::Loose(vec![Loose::NoDir])]
    );
    let left = ["seamless-1-ab.s3.ctl".to_string()];
    assert_eq!(
        verdict(&attempt(&CLEAN_LOG), "3", &left).wrong,
        [Wrong::Left(left.to_vec())]
    );
    let mut kept = attempt(&CLEAN_LOG);
    kept.files[0].bytes = b"0123abcd\nnot = [toml QZP6SECRET2".to_vec();
    let judged = verdict(&kept, "3", &[]);
    assert_eq!(
        leaks(&judged.wrong),
        [at(Kind::Manifest, ".toml", "-", &[2])],
        "a manifest that does not parse keeps nothing"
    );
}

/// A name the keep seam listed and did not read (`skip`: no regular file, or past its cap)
/// was never swept, so the leg cannot say every file was read.
#[test]
fn a_name_the_seam_did_not_read_fails() {
    let mut kept = attempt(&CLEAN_LOG);
    kept.skipped = vec!["seamless-1-ab.s4.hist".to_string()];
    let judged = verdict(&kept, "3", &[]);
    assert_eq!(
        judged.wrong,
        [Wrong::Unread(vec!["seamless-1-ab.s4.hist".to_string()])]
    );
    assert!(judged.rendered().is_err());
}

#[test]
fn a_sweep_that_read_no_planted_path_is_vacuous() {
    let vacuous = |w: &Wrong| matches!(w, Wrong::NoPlantedLog { .. });
    let judged = verdict(&attempt(&CLEAN_LOG), "4", &[]);
    assert!(judged.wrong.iter().any(vacuous), "{:?}", judged.wrong);
    let judged = verdict(&attempt(&CLEAN_LOG[..2]), "3", &[]);
    assert_eq!(
        judged.wrong,
        [Wrong::NoPlantedLog {
            lines: 2,
            held: BTreeSet::new()
        }]
    );
    let mut kept = attempt(&CLEAN_LOG);
    kept.files.retain(|f| Kind::of(&f.name) != Kind::Manifest);
    assert_eq!(
        verdict(&kept, "3", &[]).wrong,
        [Wrong::NoManifest, Wrong::NoWaitingPost]
    );
    let mut kept = attempt(&CLEAN_LOG);
    let unwaited = MANIFEST.split("\n[[sessions.fabric.posts]]").next();
    kept.files[0].bytes = unwaited.unwrap_or_default().as_bytes().to_vec();
    assert_eq!(verdict(&kept, "3", &[]).wrong, [Wrong::NoWaitingPost]);
    let mut kept = attempt(&CLEAN_LOG);
    kept.files[4].bytes = b"\x03\x00\x00\x00$ ran-42".to_vec();
    assert_eq!(verdict(&kept, "3", &[]).wrong, [Wrong::NoTypedLine]);
}

#[test]
fn the_index_is_read_with_each_file_beside_it() {
    let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
    let dir = tmp.path();
    std::fs::write(dir.join("seamless-1-ab.toml"), "0123\nschema = 1\n").expect("a file");
    std::fs::write(
        dir.join("index"),
        "dir 700 /private/tmp/x/run\nfile 600 16 seamless-1-ab.toml\nskip seamless-1-ab.sock\n",
    )
    .expect("the index");
    let kept = read_kept(dir).expect("reads");
    assert_eq!(kept.dir, Some((0o700, "/private/tmp/x/run".to_string())));
    assert_eq!(kept.files.len(), 1);
    assert_eq!(kept.files[0].mode, 0o600);
    assert_eq!(kept.skipped, ["seamless-1-ab.sock"]);
    std::fs::write(dir.join("index"), "file 600 99 seamless-1-ab.toml\n").expect("the index");
    assert!(read_kept(dir).is_err(), "a length the copy does not have");
}
