// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Manual public admission-reader comparison; private files and synthetic facts only.

#[cfg(all(unix, debug_assertions, target_os = "macos"))]
fn instructions() -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::uninit();
    // SAFETY: this flavor initializes the complete selected buffer on success.
    let result = unsafe {
        libc::proc_pid_rusage(
            libc::getpid(),
            libc::RUSAGE_INFO_V4,
            info.as_mut_ptr().cast(),
        )
    };
    // SAFETY: success initialized the buffer above.
    (result == 0).then(|| unsafe { info.assume_init() }.ri_instructions)
}

#[cfg(all(unix, debug_assertions, not(target_os = "macos")))]
fn instructions() -> Option<u64> {
    None
}

#[cfg(all(unix, debug_assertions))]
fn main() {
    use aterm_netplace::{
        Answer, Evidence, Facts, Lane, Record, heard, proof, record, sample, step,
    };
    let place = sample::home(1, 4);
    let facts = Facts {
        route: place.route,
        tunnel: place.tunnel,
        path: place.path,
    };
    let now = 100_000;
    for (count, notes) in [
        (0, false),
        (heard::MAX_LINES, false),
        (0, true),
        (416, true),
    ] {
        for moved in [false, true] {
            let state = aterm_tempfile::tempdir().unwrap();
            let dir = record::dir(state.path());
            let written = record::Written {
                record: step(
                    &Record::default(),
                    &facts,
                    &Evidence::default(),
                    None,
                    now - 1000,
                    None,
                )
                .0,
                ..Default::default()
            };
            record::write(&dir, &written).unwrap();
            let route = facts.route.as_ref().unwrap();
            let evidence = Evidence {
                answers: (0..count)
                    .map(|index| Answer {
                        lane: if index % 2 == 0 {
                            Lane::Fleet
                        } else {
                            Lane::Commons
                        },
                        member: format!("{:016x}", index * 73 % count.max(1)),
                        addr: route.gateway.map(std::net::IpAddr::V4),
                        via: Some(route.address),
                        iface: Some(route.interface.clone()),
                        network: aterm_netplace::NetworkId::of(route),
                        logged: true,
                        began_ms: now - 500,
                        at_ms: now,
                    })
                    .collect(),
                ..Default::default()
            };
            let mut text = heard::format(&evidence);
            if notes {
                for index in 0..64 {
                    text.push_str(&format!("r fleet 1 {} {} 0\n", now - 500, now + index));
                }
                for index in 0..32 {
                    text.push_str(&format!(
                        "i ok path {} https://example.com/probe\n",
                        now + index
                    ));
                }
            }
            aterm_uds::private_file::write_private(&dir, heard::FILE, text.as_bytes()).unwrap();
            let (evidence, passed) = heard::read_counted(&dir);
            assert_eq!(passed, 0);
            assert_eq!(evidence.answers.len(), count);
            assert_eq!(evidence.rounds.len(), if notes { 64 } else { 0 });
            assert_eq!(evidence.internet.len(), if notes { 32 } else { 0 });
            let mut here = aterm_netplace::facts::Here {
                facts: facts.clone(),
                own: vec![std::net::IpAddr::V4(route.address)],
                point_to_point: false,
            };
            if moved {
                here.facts
                    .route
                    .as_mut()
                    .unwrap()
                    .gateway_hw
                    .as_mut()
                    .unwrap()[5] ^= 1;
            }
            let expected_record = step(&written.record, &here.facts, &evidence, None, now, None).0;
            let expected = proof::Now {
                stay_since_ms: expected_record.current().map(|stay| stay.start_ms),
                stay_network: expected_record
                    .current()
                    .and_then(|stay| stay.network.clone()),
                facts: here.facts.clone(),
                own: here.own.clone(),
                point_to_point: here.point_to_point,
                answers: evidence.answers,
                at_ms: now,
            };
            let saved = std::fs::read(dir.join(record::FILE)).unwrap();
            let inputs = vec![here; 64];
            let before = instructions();
            let started = std::time::Instant::now();
            let (_, allocations) = aterm_heap_count::allocations_in(|| {
                for here in inputs {
                    let got = proof::read_with(std::hint::black_box(state.path()), here, now);
                    assert_eq!(got, expected);
                    std::hint::black_box(got);
                }
            });
            let elapsed = started.elapsed();
            let retired = before.zip(instructions()).map(|(a, b)| b - a);
            println!(
                "answers={count} notes={notes} moved={moved} rounds=64 allocations={allocations} instructions={retired:?} elapsed_ns={}",
                elapsed.as_nanos()
            );
            assert_eq!(std::fs::read(dir.join(record::FILE)).unwrap(), saved);
            assert_eq!(
                std::fs::read(dir.join(heard::FILE)).unwrap(),
                text.as_bytes()
            );
        }
    }
}

#[cfg(not(all(unix, debug_assertions)))]
fn main() {
    eprintln!("proof_reader_cost requires Unix and --profile opt-test");
}
