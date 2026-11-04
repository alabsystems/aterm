// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::io::{BufRead as _, Write as _};

fn finish(stream: TcpStream, ended: Receiver<bool>, worker: JoinHandle<()>) -> bool {
    drop(stream);
    let served = ended.recv_timeout(HANG).unwrap();
    worker.join().unwrap();
    served
}

#[test]
fn offhome_done_cannot_close_a_valid_pending_request() {
    let f = Fixture::home();
    let now = f.fake.now();
    *f.fake.frozen.lock_or_recover() = Some(now);
    f.ask("queued", false);
    let desk = f.ready_on("home-a");
    let before = desk.lock_or_recover().rows(f.fake.now());
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].state, desk::RowState::Asking);
    let device = device(&f.fake.root, "queued");
    let joining = Joining {
        records_bound: aterm_secrets::crossing::RecordsBound::uncrossed(),
        device: &device,
        node: [7; 32],
        name: "queued".to_owned(),
        model: "Mac".to_owned(),
    };

    let (mut stream, ended, worker) = f.connection_from("172.20.10.40");
    let greeting = joiner::greeting(&mut stream).unwrap();
    let away = joiner::done(&mut stream, &greeting, &joining);
    let away_served = finish(stream, ended, worker);
    let after_away = desk.lock_or_recover().rows(f.fake.now());

    // The identical signed request is valid: on home it closes the existing row.
    let (mut stream, ended, worker) = f.connection();
    let greeting = joiner::greeting(&mut stream).unwrap();
    let home = joiner::done(&mut stream, &greeting, &joining);
    let home_served = finish(stream, ended, worker);
    let after_home = desk.lock_or_recover().rows(f.fake.now());

    assert_eq!(away.unwrap_err().to_string(), UNAVAILABLE);
    assert!(!away_served);
    assert_eq!(after_away, before);
    home.unwrap();
    assert!(home_served);
    assert_eq!(after_home.len(), 1);
    assert_eq!(after_home[0].id, before[0].id);
    assert_eq!(after_home[0].state, desk::RowState::Closed);
}

#[test]
fn offhome_run_is_refused_by_policy_before_dispatch() {
    let f = Fixture::home();
    let now = f.fake.now();
    *f.fake.frozen.lock_or_recover() = Some(now);
    let mut policy = Vec::new();
    aterm_secrets::members::refuse(&mut policy, UNAVAILABLE).unwrap();
    let policy = String::from_utf8(policy).unwrap();
    let responses: Vec<_> = ["172.20.10.40", "192.168.1.40"]
        .into_iter()
        .map(|peer| {
            let (mut stream, ended, worker) = f.connection_from(peer);
            joiner::greeting(&mut stream).unwrap();
            stream.write_all(b"run\n").unwrap();
            stream.flush().unwrap();
            let mut response = String::new();
            std::io::BufReader::new(&mut stream)
                .read_line(&mut response)
                .unwrap();
            let served = finish(stream, ended, worker);
            (response, served)
        })
        .collect();

    assert_eq!(responses[0], (policy.clone(), false));
    assert!(responses[1].0.starts_with("NO "));
    assert_ne!(responses[1].0, policy);
    assert!(!responses[1].1);
}
