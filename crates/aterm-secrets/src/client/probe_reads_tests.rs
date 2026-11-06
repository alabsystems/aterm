// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A join's probe of every candidate at once ([`probes_within`]): answers longer than the peek
//! read whole two at a time, the vault's compiled-in address besides, each read's failure
//! reaching the fold as the read said it, and each answer judged as a probe of that one server
//! judges it.

use super::fan_out::ENVELOPES;
use super::fan_out::held::HeldReads;
use super::*;
use crate::discover::{DiscoveredVault, MAX_CANDIDATES};
use crate::server::{ENVELOPE_PEEK, ENVELOPE_PULLS_AT_ONCE};
use crate::transport::{Answer, Pulled};
use aterm_spec::wait::MUST_HAPPEN;

/// The seed of the key the probed vault is signed with.
const SEED: [u8; 32] = [3; 32];

/// The `alab` vault's version 1, signed with `seed`, carrying one locker of `armor_lines` lines
/// (none for no locker).
fn signed(seed: &[u8; 32], armor_lines: usize) -> Vec<u8> {
    let lockers = if armor_lines == 0 {
        Vec::new()
    } else {
        let mut armored = format!("{}\n", envelope::ARMOR_BEGIN);
        for _ in 0..armor_lines {
            armored.push_str(&"A".repeat(64));
            armored.push('\n');
        }
        armored.push_str(envelope::ARMOR_END);
        armored.push('\n');
        vec![Locker {
            label: "admin".into(),
            armored,
        }]
    };
    let unsigned = Unsigned {
        name: "alab".into(),
        seq: 1,
        time: 0,
        handoff: Vec::new(),
        parents: Vec::new(),
        lockers,
    };
    envelope::sign(&unsigned, seed).expect("signed")
}

fn pin() -> [u8; 32] {
    envelope::signer_public(&SEED).expect("the pin")
}

/// The servers a browse named besides the vault's compiled-in address, filling the candidates
/// [`probe_alab`] probes up to [`MAX_CANDIDATES`].
fn browsed() -> Vec<DiscoveredVault> {
    (0..MAX_CANDIDATES - crate::ALAB_DEFAULT_URLS.len())
        .map(|candidate| DiscoveredVault {
            url: format!("http://candidate-{candidate}.local:7484/v1/alab"),
            txt: None,
        })
        .collect()
}

/// A join's probe ([`probe_alab`], which [`probe_default`] and a bounded join both take) reads at
/// most [`ENVELOPE_PULLS_AT_ONCE`] answers longer than [`ENVELOPE_PEEK`] from the servers a
/// browse named whole at once, however many it probes, and the vault's compiled-in address
/// ([`crate::ALAB_DEFAULT_URLS`]) besides, and still judges each one it read whole, in its place:
/// every answer is read first under the peek, all at once, and only one longer than that is asked
/// again and read whole, under a slot held until it is judged. Before, every candidate was read
/// whole at once on a thread of its own, so a LAN host that advertised [`MAX_CANDIDATES`]
/// instances of itself, each answering with a 16 MiB body, had a Mac joining the vault, which
/// trusts none of them yet, hold 256 MiB of bodies nothing had verified. Here every candidate's
/// answer is longer than the peek and its whole read held until the test lets it go
/// ([`HeldReads`]), and the slots counted are the ones the shipped probe makes. Every candidate
/// serves the vault, so the probe finds it at the first candidate, the default.
///
/// NEGATIVE CONTROL: the slots `fan_out::reads_within` makes for as many reads at once as a
/// browse names (`PullSlots::new(crate::discover::MAX_CANDIDATES)` for `reads.at_once`), or
/// `probes_within` reading under `fan_out::Reads { at_once: crate::discover::MAX_CANDIDATES,
/// ..fan_out::ENVELOPES }`: every candidate read whole at once, `left: 16, right: 3`, "whole reads
/// in flight at once".
#[test]
fn a_probe_reads_at_most_two_answers_whole_at_once_and_the_vaults_own_address() {
    let held = HeldReads::new(ENVELOPES);
    let body = signed(&SEED, 0);
    let look = |_: &str, _: Duration, limit: usize| {
        held.read(limit, Answer::Fetched(Fetched::Body(body.clone())))
    };
    let discovered = browsed();
    let defaults = crate::ALAB_DEFAULT_URLS.len();
    let browsed = discovered.len();
    let counted = held.count_until(
        "the slots' whole reads and the defaults' in flight and the rest waiting, or more in flight",
        |seen| {
            seen.in_flight > ENVELOPE_PULLS_AT_ONCE + defaults
                || (seen.in_flight == ENVELOPE_PULLS_AT_ONCE + defaults
                    && seen.waiting == browsed - ENVELOPE_PULLS_AT_ONCE)
        },
        || probe_alab(&pin(), &discovered, MUST_HAPPEN * 2, look),
    );
    assert_eq!(
        counted.seen.in_flight,
        ENVELOPE_PULLS_AT_ONCE + defaults,
        "whole reads in flight at once"
    );
    assert_eq!(
        counted.seen.waiting,
        browsed - ENVELOPE_PULLS_AT_ONCE,
        "whole reads waiting for a slot"
    );
    assert_eq!(counted.most, ENVELOPE_PULLS_AT_ONCE + defaults);
    assert_eq!(
        held.peeks.load(Ordering::SeqCst),
        MAX_CANDIDATES,
        "every answer was peeked once"
    );
    assert_eq!(
        counted.result,
        Ok(ProbeOutcome::Found(Probed {
            url: crate::ALAB_DEFAULT_URL.to_owned(),
            seq: 1,
            fingerprint: envelope::fingerprint(&pin()),
            envelope: body.clone(),
        })),
        "every answer read whole and verified, the first candidate's found"
    );
}

/// The servers a browse named cannot keep a join's probe from reading the vault's compiled-in
/// address ([`crate::ALAB_DEFAULT_URLS`]) whole: two that answer the peek with a long length and
/// then send nothing (a LAN host's instances, or Macs asleep mid-answer) hold both slots until the
/// probe's deadline, and a vault longer than the peek at the default address is still read whole
/// at once and found. Under the slots alone, the default's whole read would wait for a slot with
/// the rest, wake at the deadline with no time left, and end the probe `Err(TimedOut)` where the
/// vault answered: `aterm init` and System State would take the vault for not on this network
/// once its envelope outgrew the peek. Here the default's
/// first read waits until two of the browse's whole reads hold both slots, and those are let go
/// only once the default's whole read was made or it waits for a slot too.
///
/// NEGATIVE CONTROL: the default under the slots like every other server (no `if default` arm in
/// `fan_out::reads_within`): it waits for a slot, "the vault's own address read whole while the
/// browse's servers held both slots" fails.
#[test]
fn servers_a_browse_named_cannot_keep_the_vaults_own_address_from_being_read() {
    let held = HeldReads::new(ENVELOPES);
    let long = signed(&SEED, ENVELOPE_PEEK / 64);
    assert!(long.len() > ENVELOPE_PEEK);
    let read_whole_while_held = std::sync::Mutex::new(None);
    let look = |url: &str, _: Duration, limit: usize| -> crate::Result<Pulled<Answer>> {
        if !crate::is_alab_default_url(url) {
            return held.read(
                limit,
                Answer::Fetched(Fetched::Body(b"no envelope".to_vec())),
            );
        }
        if limit == ENVELOPE_PEEK {
            aterm_spec::wait::until_within(
                MUST_HAPPEN,
                "two of the browse's whole reads holding both slots",
                || (held.in_flight() >= ENVELOPE_PULLS_AT_ONCE).then_some(()),
            );
            return Ok(Pulled::Larger);
        }
        *read_whole_while_held.lock().expect("the default's read") = Some(!held.let_go());
        Ok(Pulled::Fetched(Answer::Fetched(Fetched::Body(
            long.clone(),
        ))))
    };
    let discovered = browsed();
    let browsed = discovered.len();
    let counted = held.count_until(
        "the vault's own address read whole, or waiting for a slot with the browse's servers",
        |seen| {
            read_whole_while_held
                .lock()
                .expect("the default's read")
                .is_some()
                || seen.waiting > browsed - ENVELOPE_PULLS_AT_ONCE
        },
        || probe_alab(&pin(), &discovered, MUST_HAPPEN * 2, look),
    );
    assert_eq!(
        *read_whole_while_held.lock().expect("the default's read"),
        Some(true),
        "the vault's own address read whole while the browse's servers held both slots"
    );
    assert_eq!(counted.most, ENVELOPE_PULLS_AT_ONCE);
    assert_eq!(
        counted.result,
        Ok(ProbeOutcome::Found(Probed {
            url: crate::ALAB_DEFAULT_URL.to_owned(),
            seq: 1,
            fingerprint: envelope::fingerprint(&pin()),
            envelope: long,
        })),
        "the vault at its own address found"
    );
}

/// Every candidate's failed read reaches the probe's fold as the read said it, kind and words,
/// whether the first read failed or the whole read an answer longer than the peek took, and the
/// first failure of those the fold is given is the one the probe gives. The fan-out the probes
/// read through ([`fan_out::reads_within`]) hands each failure on; the envelope reads that share
/// it ([`fan_out::envelopes_within`]) drop them.
///
/// NEGATIVE CONTROL: in `reads_within`, hand `judge` a failed read as a failure that says nothing
/// of it (`read(url, left, limit).map_err(|_| Error::new("no answer"))` in `read_by`): every
/// outcome is `Err(Error("no answer", Other))`, the kinds and the words are lost, and the first
/// assert fails.
#[test]
fn every_failed_read_reaches_the_probe_fold_as_it_was_said() {
    use ProbeOrigin::{Default, Discovered};
    let refused = Error::new("fetching gone.local: connection refused");
    let timed_out = Error::of_kind(crate::ErrorKind::TimedOut, "fetching slow.local: timed out");
    let reset = Error::new("fetching reset.local: connection reset by peer");
    // Each candidate: its host, whether its answer is longer than the peek (so its whole read
    // is the one that fails), and the failure its read gives.
    let candidates = [
        ("gone", false, &refused),
        ("slow", false, &timed_out),
        ("gone-whole", true, &refused),
        ("reset-whole", true, &reset),
    ];
    let urls: Vec<String> = candidates
        .iter()
        .map(|(host, _, _)| format!("http://{host}.local:7787/v1/alab"))
        .collect();
    let look = |url: &str, _: Duration, limit: usize| -> crate::Result<Pulled<Answer>> {
        let (_, longer, failure) = candidates
            .iter()
            .find(|(host, _, _)| url.contains(&format!("//{host}.local")))
            .expect("a candidate");
        if *longer && limit == ENVELOPE_PEEK {
            return Ok(Pulled::Larger);
        }
        Err((*failure).clone())
    };
    let outcomes = probes_within(&urls, &pin(), MUST_HAPPEN, look);
    let said: Vec<crate::Result<ProbeOutcome>> = candidates
        .iter()
        .map(|(_, _, failure)| Err((*failure).clone()))
        .collect();
    assert_eq!(outcomes, said, "each read's failure, as the read said it");

    let fold = |picked: &[(usize, ProbeOrigin)]| {
        select_probed_outcome(
            picked
                .iter()
                .map(|&(index, origin)| (origin, outcomes[index].clone())),
        )
    };
    assert_eq!(
        fold(&[(1, Default), (0, Default)]),
        Err(timed_out.clone()),
        "the first failure, whatever its kind"
    );
    assert_eq!(
        fold(&[(2, Default), (1, Discovered)]),
        Err(refused),
        "a failure of a whole read, in its words"
    );
    assert_eq!(
        fold(&[(3, Default), (1, Discovered)]),
        Err(reset),
        "the first of two other failures"
    );
}

/// A loopback server that answers every request with `answer` until [`stop`] is sent it, and
/// hands back the request heads it read. A read under the peek hangs up once it sees a declared
/// length past it, so what the server writes after that goes nowhere.
fn serving(answer: Vec<u8>) -> (String, std::thread::JoinHandle<Vec<String>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let url = format!("http://{}/v1/alab", listener.local_addr().expect("addr"));
    let server = std::thread::spawn(move || {
        use std::io::{Read, Write};
        let mut heads = Vec::new();
        loop {
            let (mut stream, _) = listener.accept().expect("accept");
            stream.set_read_timeout(Some(MUST_HAPPEN)).expect("timeout");
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                if stream.read(&mut byte).expect("read") == 0 {
                    break;
                }
                head.push(byte[0]);
            }
            if head.starts_with(STOP) {
                return heads;
            }
            heads.push(String::from_utf8_lossy(&head).into_owned());
            let _ = stream.write_all(&answer);
        }
    });
    (url, server)
}

/// What ends a [`serving`] server.
const STOP: &[u8] = b"STOP\r\n\r\n";

/// The heads the [`serving`] server at `url` read, once it has answered every request made of it
/// before this: each was answered in turn, so the stop is the last connection it takes.
fn stop(url: &str, server: std::thread::JoinHandle<Vec<String>>) -> Vec<String> {
    use std::io::Write;
    let address = url
        .strip_prefix("http://")
        .and_then(|rest| rest.split_once('/'))
        .map(|(address, _)| address)
        .expect("a loopback URL");
    let mut stream = std::net::TcpStream::connect(address).expect("connect");
    stream.write_all(STOP).expect("stop");
    server.join().expect("the server")
}

/// `status` with `body`, declaring its length and closing the connection.
fn answer(status: &str, body: &[u8]) -> Vec<u8> {
    let mut answer = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    answer.extend_from_slice(body);
    answer
}

/// A probe of many servers at once says of each one's answer what a probe of that one server
/// ([`probe`], which reads it whole in one GET) says, failures included, while it reads an
/// answer longer than [`ENVELOPE_PEEK`] twice, the peek and then whole, and one within it once:
/// a vault longer than the peek is still found, another vault's still told apart, an answer
/// longer than any envelope is still one no vault server gives, in the words it had, and each
/// read is still a look that leaves no heard record. [`probe_candidates`] reads so.
///
/// NEGATIVE CONTROL: put back `probe_candidates_with`'s thread for each URL, each running
/// `probe(url, pin, total)`: the long vault's server is asked once by `probe_candidates`, not
/// twice, `left: 4, right: 5`, "the requests the long vault's server answered".
#[test]
fn a_probe_of_many_says_of_each_answer_what_a_probe_of_one_does() {
    let long = signed(&SEED, ENVELOPE_PEEK / 64);
    assert!(long.len() > ENVELOPE_PEEK);
    let other_seed = [5u8; 32];
    // Each server: its answer, how many times the fan-out asks it, and its name in a failure.
    let servers = [
        (answer("200 OK", &long), 2, "a vault longer than the peek"),
        (answer("200 OK", &signed(&SEED, 1)), 1, "a short vault"),
        (
            answer("200 OK", &signed(&other_seed, ENVELOPE_PEEK / 64)),
            2,
            "another vault longer than the peek",
        ),
        (answer("200 OK", &[b'x'; 64]), 1, "no envelope"),
        (
            answer("404 Not Found", b"no such vault"),
            1,
            "no such vault",
        ),
        (answer("302 Found", b""), 1, "a redirect"),
        (
            answer("500 Internal Server Error", b"broken"),
            1,
            "a failing server",
        ),
        (
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                envelope::MAX_ENVELOPE + 1
            )
            .into_bytes(),
            2,
            "an answer longer than any envelope",
        ),
    ];
    let (mut urls, running): (Vec<String>, Vec<_>) = servers
        .iter()
        .map(|(reply, _, _)| serving(reply.clone()))
        .unzip();
    // Nothing listens on the discard port: the connect is refused.
    urls.push("http://127.0.0.1:9/v1/alab".to_owned());
    let pin = pin();
    let one: Vec<crate::Result<ProbeOutcome>> = urls
        .iter()
        .map(|url| probe(url, &pin, MUST_HAPPEN))
        .collect();
    let many = probes_within(&urls, &pin, MUST_HAPPEN, transport::look_within);
    assert_eq!(many, one, "each answer judged as a probe of one judges it");
    assert!(
        matches!(&many[0], Ok(ProbeOutcome::Found(found)) if found.envelope == long),
        "the vault longer than the peek is found, read whole"
    );
    assert!(
        matches!(&many[2], Ok(ProbeOutcome::AnotherVault(_))),
        "{:?}",
        many[2]
    );
    assert!(
        matches!(&many[7], Ok(ProbeOutcome::NotThisVault(why)) if why.contains(&format!("{}-byte limit", envelope::MAX_ENVELOPE))),
        "{:?}",
        many[7]
    );
    assert!(many[6].is_err() && many[8].is_err(), "{:?}", &many[6..]);
    let ProbeOutcome::Found(found) =
        probe_candidates(&urls[..1], &pin, MUST_HAPPEN).expect("the long vault")
    else {
        panic!("the long vault found");
    };
    assert_eq!(found.url, urls[0]);
    for (index, (server, (_, asked, what))) in running.into_iter().zip(&servers).enumerate() {
        let heads = stop(&urls[index], server);
        if index == 0 {
            // One GET for `probe`, two for the fan-out, two for `probe_candidates`.
            assert_eq!(
                heads.len(),
                5,
                "the requests the long vault's server answered"
            );
        } else {
            assert_eq!(heads.len(), 1 + asked, "the requests {what} answered");
        }
        for head in heads {
            assert!(head.starts_with("GET /v1/alab "), "{head}");
            let head = head.to_ascii_lowercase();
            assert!(
                head.contains(&format!(
                    "{}: 1",
                    transport::OBSERVER_HEADER.to_ascii_lowercase()
                )),
                "each read is a look: {head}"
            );
        }
    }
}
