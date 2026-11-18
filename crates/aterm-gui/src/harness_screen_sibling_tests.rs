// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const A: &str = "tqn5r6e0148";
const B: &str = "tqn5r6e0149";
const N: &str = "11111111111111111111111111111111";
const OTHER: &str = "22222222222222222222222222222222";

fn roster(local: u64, sid: &str, nonce: &str) -> String {
    format!("{local} {sid} - Running shell nonce={nonce}")
}

fn part(local: u64, sid: &str, nonce: &str, bytes: u64) -> aterm_json::Value {
    json(&format!(
        r#"{{"name":"tab","local":{local},"sid":"{sid}","nonce":"{nonce}","footprint_bytes":{bytes},"footprint_unread":0}}"#
    ))
}

fn json(s: &str) -> aterm_json::Value {
    aterm_json::from_str(s).unwrap()
}

fn set(object: &mut aterm_json::Value, key: &str, value: aterm_json::Value) {
    object
        .as_object_mut()
        .expect("fixture object")
        .insert(key.to_owned(), value);
}

fn reading(parts: Vec<aterm_json::Value>) -> aterm_json::Value {
    let mut r =
        json(r#"{"schema":1,"kind":"sys.aterm","as_of":{"state":"fresh","age_ms":20},"parts":[]}"#);
    set(&mut r, "parts", aterm_json::Value::Array(parts));
    r
}

fn rows() -> Vec<Session> {
    [roster(9, B, N), roster(2, A, N)]
        .iter()
        .map(|line| session("mac", 99, line).unwrap())
        .collect()
}

#[test]
fn per_pane_parts_join_by_identity_in_any_order_and_never_charge_the_instance() {
    let mut rows = rows();
    let third = roster(11, "tqn5r6e014a", N);
    rows.push(session("mac", 99, &third).unwrap());
    let after = [roster(2, A, N), roster(9, B, N), third];
    let mut instance = part(99, "tqn5r6e014a", N, 900 << 20);
    set(
        &mut instance,
        "name",
        aterm_json::Value::String("shared".to_owned()),
    );
    let reading = reading(vec![
        part(2, A, N, 200 << 20),
        instance,
        part(9, B, N, 700 << 20),
    ]);
    apply(&mut rows, Some(&after), Some(&reading));
    assert_eq!(rows[0].row.vitals, Memory::Complete(700 << 20).words());
    assert_eq!(rows[1].row.vitals, Memory::Complete(200 << 20).words());
    assert_eq!(
        rows[2].row.vitals,
        Memory::Unavailable.words(),
        "no part never falls back to shared/whole-instance memory"
    );
}

#[test]
fn generation_churn_missing_proof_and_duplicate_ids_refuse_credit() {
    let valid = part(2, A, N, 200 << 20);
    for (after, parts) in [
        (vec![roster(2, A, OTHER)], vec![valid.clone()]),
        (vec![roster(2, A, N)], vec![part(2, A, OTHER, 200 << 20)]),
        (vec![roster(2, B, N)], vec![valid.clone()]),
        (vec![], vec![valid.clone()]),
        (vec![roster(2, A, N), roster(2, A, N)], vec![valid.clone()]),
        (vec![roster(2, A, N)], vec![valid.clone(), valid.clone()]),
        (
            vec![roster(2, A, N)],
            vec![valid.clone(), part(2, B, N, 999 << 20)],
        ),
        (
            vec![roster(2, A, N)],
            vec![valid.clone(), part(9, A, N, 999 << 20)],
        ),
        (vec![roster(2, A, N), roster(9, A, N)], vec![valid.clone()]),
        (
            vec![roster(2, A, N)],
            vec![json(
                r#"{"name":"tab","local":2,"sid":"tqn5r6e0148","footprint_bytes":100,"footprint_unread":0}"#,
            )],
        ),
    ] {
        let mut rows = vec![session("mac", 99, &roster(2, A, N)).unwrap()];
        apply(&mut rows, Some(&after), Some(&reading(parts)));
        assert_eq!(rows[0].row.vitals, Memory::Unavailable.words());
    }
    let mut duplicate = vec![
        session("mac", 99, &roster(2, A, N)).unwrap(),
        session("mac", 99, &roster(2, A, "bad")).unwrap(),
    ];
    apply(
        &mut duplicate,
        Some(&[roster(2, A, N)]),
        Some(&reading(vec![valid])),
    );
    assert!(
        duplicate
            .iter()
            .all(|s| s.row.vitals == Memory::Unavailable.words())
    );
}

#[test]
fn an_unproved_pane_preserves_other_proved_panes_but_still_counts_as_ambiguous() {
    let missing = json(
        r#"{"name":"tab","local":9,"sid":"tqn5r6e0149","footprint_bytes":999,"footprint_unread":0}"#,
    );
    let valid = part(2, A, N, 200 << 20);
    let after = [roster(2, A, N), roster(9, B, N)];
    let mut rows = rows();
    apply(
        &mut rows,
        Some(&after),
        Some(&reading(vec![valid.clone(), missing.clone()])),
    );
    assert_eq!(rows[1].row.vitals, Memory::Complete(200 << 20).words());
    assert_eq!(rows[0].row.vitals, Memory::Unavailable.words());
    let mut duplicate = missing;
    set(&mut duplicate, "local", json("2"));
    apply(
        &mut rows,
        Some(&after),
        Some(&reading(vec![valid, duplicate])),
    );
    assert!(
        rows.iter()
            .all(|row| row.row.vitals == Memory::Unavailable.words())
    );
}

#[test]
fn zero_missing_and_lower_bounds_keep_their_meaning_and_stale_frames_are_unavailable() {
    let mut lower = part(2, A, N, 200 << 20);
    set(&mut lower, "footprint_unread", json("2"));
    set(
        &mut lower,
        "footprint_bytes",
        json(r#"{"v":209715200,"bound":"at_least","cause":"refused","unread":2}"#),
    );
    let mut missing = lower.clone();
    set(
        &mut missing,
        "footprint_bytes",
        json(r#"{"why":"refused"}"#),
    );
    assert_eq!(memory(&part(2, A, N, 0)), Memory::Complete(0));
    assert_eq!(memory(&lower), Memory::AtLeast(200 << 20));
    assert_eq!(memory(&missing), Memory::Unavailable);
    let mut contradictory = lower.clone();
    set(&mut contradictory, "footprint_unread", json("0"));
    assert_eq!(memory(&contradictory), Memory::Unavailable);
    for as_of in [
        r#"{"state":"stale","age_ms":1}"#,
        r#"{"state":"warming","age_ms":1}"#,
        r#"{"state":"fresh","age_ms":60001}"#,
        r#"{"state":"fresh","age_ms":{"why":"warming"}}"#,
    ] {
        let mut r = reading(vec![lower.clone()]);
        set(&mut r, "as_of", json(as_of));
        let mut rows = rows();
        apply(
            &mut rows,
            Some(&[roster(2, A, N), roster(9, B, N)]),
            Some(&r),
        );
        assert!(
            rows.iter()
                .all(|s| s.row.vitals == Memory::Unavailable.words())
        );
    }
    let mut r = reading(vec![lower]);
    set(
        r.as_object_mut().unwrap().get_mut("as_of").unwrap(),
        "state",
        json(r#""partial""#),
    );
    set(&mut r, "lanes", json(r#"[{"figures":["net"]}]"#));
    assert!(
        fresh(&r),
        "an unrelated stalled lane leaves the footprint readable"
    );
    set(
        &mut r,
        "lanes",
        json(r#"[{"figures":["aterm","tab","procs"]}]"#),
    );
    assert!(
        !fresh(&r),
        "machine age cannot freshen a stalled process observation"
    );
}

#[test]
fn one_authenticated_engine_reply_supplies_both_panes_and_all_reads_are_framed() {
    if !crate::test_alone::alone() {
        return;
    }
    use std::io::{BufRead as _, Write as _};
    use std::os::unix::fs::OpenOptionsExt as _;
    let scratch = crate::test_scratch::short_dir("peer-vitals");
    let socket = scratch.path().join("aterm-9999999.sock");
    let token = scratch.path().join("aterm-9999999.token");
    std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(token)
        .unwrap()
        .write_all(b"original")
        .unwrap();
    let listener = aterm_uds::CtlListener::bind(&socket).unwrap();
    let server = std::thread::spawn(move || {
        let data = reading(vec![part(2, A, N, 200 << 20), part(9, B, N, 700 << 20)]);
        let responses = [
            (
                vec!["sessions".to_owned()],
                format!("OK 2\n{}\n{}\n", roster(9, B, N), roster(2, A, N)),
            ),
            (
                vec![format!("@{B} sys aterm --json fresh=60000")],
                format!(
                    "OK 1 sys v=1 view=aterm\n{}\n",
                    aterm_json::to_string(&data).unwrap()
                ),
            ),
            (
                vec!["sessions bridge".to_owned()],
                format!("OK 2\n2 {A} nonce={N}\n9 {B} nonce={N}\n"),
            ),
            (
                vec![
                    format!("@{B} dims"),
                    format!("@{B} cwd"),
                    format!("@{A} dims"),
                    format!("@{A} cwd"),
                ],
                "OK 24 80\nOK %2Ftmp\nOK 40 120\nOK -\n".into(),
            ),
        ];
        for (requests, response) in responses {
            let (mut conn, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Until(std::time::Instant::now() + Duration::from_secs(60)),
                aterm_uds::AcceptedMode::Blocking,
            )
            .unwrap();
            conn.set_read_timeout(Some(Duration::from_secs(60)))
                .unwrap();
            let mut reader = std::io::BufReader::new(&conn);
            let mut auth = String::new();
            reader.read_line(&mut auth).unwrap();
            assert_eq!(auth, "AUTH original\n");
            for expected in requests {
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                assert_eq!(request.trim_end(), expected);
            }
            conn.write_all(response.as_bytes()).unwrap();
        }
    });
    let rows = read_within(
        "mac",
        9999999,
        socket.to_str().unwrap(),
        Duration::from_secs(60),
    );
    server.join().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].vitals, Memory::Complete(700 << 20).words());
    assert_eq!(rows[1].vitals, Memory::Complete(200 << 20).words());
    assert!(rows[0].place.contains("80×24"));
    assert!(rows[1].place.contains("120×40"));
}
