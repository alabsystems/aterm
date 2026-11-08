// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Encoding validates the wire without constructing another owned payload.
#![cfg(unix)]

use aterm_bench::allocator::{Observed, Observer};
use aterm_uds::lan::wire::{self, Advert, Event, Frame, Instance, Request};
use std::alloc::System;
use std::cell::Cell;

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static LIVE: Cell<usize> = const { Cell::new(0) };
    static PEAK: Cell<usize> = const { Cell::new(0) };
    static CALLS: Cell<usize> = const { Cell::new(0) };
}

#[test]
fn an_oversized_body_is_refused_before_allocating_its_output() {
    let frame = Frame::Event(Event::Record {
        rrtype: 16,
        rdata: vec![0x5a; wire::MAX_FRAME],
        ttl: 60,
        interface: 7,
        added: true,
    });
    LIVE.with(|value| value.set(0));
    PEAK.with(|value| value.set(0));
    CALLS.with(|value| value.set(0));
    ACTIVE.with(|value| value.set(true));
    let result = wire::encode(&frame);
    ACTIVE.with(|value| value.set(false));
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
    assert!(
        PEAK.with(Cell::get) < wire::MAX_FRAME,
        "oversized output was allocated before refusal"
    );
}

fn added(bytes: usize, removed: usize, allocation: bool) {
    let _ = ACTIVE.try_with(|active| {
        if active.get() {
            LIVE.with(|live| {
                live.set(live.get().wrapping_sub(removed).wrapping_add(bytes));
                PEAK.with(|peak| peak.set(peak.get().max(live.get())));
            });
            if allocation {
                CALLS.with(|calls| calls.set(calls.get().wrapping_add(1)));
            }
        }
    });
}

struct Counting;

// SAFETY: constant, drop-free thread-locals and wrapping arithmetic neither allocate nor unwind.
unsafe impl Observer for Counting {
    fn retained(&self, old: usize, new: usize) {
        added(new, old, new != 0);
    }
}

#[global_allocator]
static ALLOCATOR: Observed<System, Counting> = Observed::new(System, Counting);

#[test]
fn encoded_output_is_the_only_live_heap_during_validation() {
    let frames = [
        (
            "browse",
            Frame::Event(Event::Added(Instance {
                name: "lab-mac".into(),
                service: "_alab._tcp".into(),
                domain: "local.".into(),
                interface: 7,
            })),
        ),
        (
            "record-1460",
            Frame::Event(Event::Record {
                rrtype: 16,
                rdata: vec![0x5a; 1460],
                ttl: 60,
                interface: 7,
                added: true,
            }),
        ),
        (
            "record-max",
            Frame::Event(Event::Record {
                rrtype: 16,
                rdata: vec![0x5a; 65_535],
                ttl: 60,
                interface: 7,
                added: true,
            }),
        ),
        (
            "registration",
            Frame::Request(Request::Register(Advert {
                name: "lab-service".into(),
                service: "_alab._tcp".into(),
                domain: "local.".into(),
                port: 7484,
                txt: (0..32).map(|_| vec![b'x'; 20]).collect(),
                interface: Some(7),
                host: Some("lab-mac.local.".into()),
                addresses: (1..=32)
                    .map(|last| std::net::Ipv4Addr::new(192, 0, 2, last).into())
                    .collect(),
            })),
        ),
    ];
    let mut observed = Vec::new();
    for (name, frame) in frames {
        LIVE.with(|value| value.set(0));
        PEAK.with(|value| value.set(0));
        CALLS.with(|value| value.set(0));
        ACTIVE.with(|value| value.set(true));
        let output = wire::encode(&frame);
        ACTIVE.with(|value| value.set(false));
        let output = output.unwrap();
        let peak = PEAK.with(Cell::get);
        let calls = CALLS.with(Cell::get);
        println!(
            "broker_encode fixture={name} wire_bytes={} capacity={} calls={calls} peak={peak}",
            output.len(),
            output.capacity()
        );
        assert_eq!(wire::decode(&output[4..]).unwrap(), frame);
        observed.push((name, peak, output.capacity(), output.len(), calls));
    }
    for (name, peak, capacity, len, calls) in observed {
        assert_eq!(
            peak, capacity,
            "validation constructed temporary owned payload for {name}"
        );
        assert_eq!(calls, 1, "encoding grew its output buffer for {name}");
        assert_eq!(
            capacity, len,
            "encoding retained excess output capacity for {name}"
        );
    }
}
