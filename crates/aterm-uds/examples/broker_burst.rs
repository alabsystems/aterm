// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Manual broker-reader burst measurement; no timing assertion or live broker.
//! `targo --unverified run --profile bench -p aterm-uds --example broker_burst`.

#[cfg(target_os = "macos")]
fn instructions() -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::uninit();
    // SAFETY: the selected flavor writes this complete rusage_info_v4 buffer;
    // getpid names this process and the output is read only on success.
    let result = unsafe {
        libc::proc_pid_rusage(
            libc::getpid(),
            libc::RUSAGE_INFO_V4,
            info.as_mut_ptr().cast(),
        )
    };
    // SAFETY: success initialized the selected rusage buffer.
    (result == 0).then(|| unsafe { info.assume_init() }.ri_instructions)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn instructions() -> Option<u64> {
    None
}

#[cfg(unix)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use aterm_uds::lan::wire::{self, Frame, Reader};
    use std::io::Write as _;
    use std::os::fd::AsRawFd as _;
    use std::time::{Duration, Instant};

    if std::env::args().nth(1).as_deref() == Some("encode") {
        return encode_cost();
    }
    for frames in [1, 8, 64, 512, 2048] {
        let frame = Frame::Socket { reached: None };
        let encoded = wire::encode(&frame)?;
        let burst = encoded.repeat(frames);
        let rounds = 262_144 / frames;
        let (ours, mut theirs) = aterm_uds::kept_out(aterm_uds::CtlStream::pair)?;
        // Queue the complete bounded burst before receiving, independent of the
        // host's default Unix socket watermarks. This changes only this private pair.
        for (fd, option) in [
            (theirs.as_raw_fd(), libc::SO_SNDBUF),
            (ours.as_raw_fd(), libc::SO_RCVBUF),
        ] {
            let capacity: libc::c_int = 128 * 1024;
            // SAFETY: fd is retained by the private stream; capacity is a live int
            // and the supplied size matches SO_SNDBUF/SO_RCVBUF's integer argument.
            if unsafe {
                libc::setsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    option,
                    (&raw const capacity).cast(),
                    std::mem::size_of_val(&capacity) as libc::socklen_t,
                )
            } < 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        // A necessary local write/read gets a one-minute hang detector, not a latency budget.
        theirs.set_write_timeout(Some(Duration::from_secs(60)))?;
        let deadline = Some(Instant::now() + Duration::from_secs(60));
        let mut reader = Reader::new(0);
        // Warm the reader's buffers before counting the steady stream.
        theirs.write_all(&burst)?;
        for _ in 0..frames {
            assert_eq!(reader.frame(&ours, deadline)?, Some(frame.clone()));
        }
        let before = instructions();
        let began = Instant::now();
        for _ in 0..rounds {
            theirs.write_all(&burst)?;
            for _ in 0..frames {
                assert_eq!(
                    std::hint::black_box(reader.frame(&ours, deadline)?),
                    Some(frame.clone())
                );
            }
        }
        let elapsed = began.elapsed().as_nanos();
        let retired = instructions()
            .zip(before)
            .map(|(after, before)| after.saturating_sub(before));
        println!(
            "broker_burst frames={frames} rounds={rounds} wire_bytes={} elapsed_ns={elapsed} instructions={retired:?}",
            burst.len()
        );
    }
    Ok(())
}

#[cfg(unix)]
fn encode_cost() -> Result<(), Box<dyn std::error::Error>> {
    use aterm_uds::lan::wire::{self, Advert, Event, Frame, Instance, Request};
    use std::time::Instant;
    let fixtures = [
        ("socket", Frame::Socket { reached: None }, 262_144),
        (
            "browse",
            Frame::Event(Event::Added(Instance {
                name: "lab-mac".into(),
                service: "_alab._tcp".into(),
                domain: "local.".into(),
                interface: 7,
            })),
            262_144,
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
            65_536,
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
            4_096,
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
            65_536,
        ),
    ];
    for (name, frame, rounds) in fixtures {
        let wire_bytes = wire::encode(&frame)?.len();
        let before = instructions();
        let began = Instant::now();
        for _ in 0..rounds {
            std::hint::black_box(wire::encode(std::hint::black_box(&frame))?);
        }
        let elapsed = began.elapsed().as_nanos();
        let retired = instructions()
            .zip(before)
            .map(|(after, before)| after.saturating_sub(before));
        println!(
            "broker_encode fixture={name} rounds={rounds} wire_bytes={wire_bytes} elapsed_ns={elapsed} instructions={retired:?}"
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn main() {
    eprintln!("broker_burst requires Unix descriptor passing");
}
