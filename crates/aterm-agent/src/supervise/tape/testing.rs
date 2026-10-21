// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A run's door recording into memory, for the tests of the loop: what a
//! `--tape FILE` run records, with the file a buffer a test reads back.

use astream_effects::EffectRecord;

use super::codec::{End, Item};
use super::core::TapeCore;
use super::frame::{Hint, Kind, MAGIC, TapeWriter, frame_bytes};
use super::frame_tests::{Memory, Said};
use super::header::{Header, Roots, VERBATIM};
use super::recover::{Recovered, Tail, recover};
use super::replay::{Options, replay};
use super::report::{Report, Verdict};
use crate::supervise::fx::Fx;

/// The header a test's tape carries: the frame layer takes any text, and no
/// test reads a header back from a run's tape.
pub(crate) const HEADER: &str = "atape 1\ntape=test\nentry=watch\n";

/// The id a test's memory tape is recorded under (its recorder seeds the
/// worker's digest with it): a replay's header names the same id.
pub(crate) const TAPE_ID: &str = "test";

/// A tape in memory a test reads back, and its door.
pub(crate) struct MemoryTape {
    pub(crate) fx: Fx,
    pub(crate) sink: Memory,
    pub(crate) said: Said,
}

impl MemoryTape {
    /// `fx`'s clocks, recording into memory, paths written as they are.
    pub(crate) fn over(fx: &Fx) -> Self {
        Self::under(fx, VERBATIM)
    }

    /// `fx`'s clocks, recording into memory, paths under `roots`.
    pub(crate) fn under(fx: &Fx, roots: Roots) -> Self {
        Self::on(fx, roots, Memory::default())
    }

    /// `fx`'s clocks, recording into `sink` (one a test made fail).
    pub(crate) fn on(fx: &Fx, roots: Roots, sink: Memory) -> Self {
        let said = Said::default();
        let writer = TapeWriter::start(
            Box::new(sink.clone()),
            "test.atape",
            HEADER,
            Box::new(said.clone()),
        );
        let (origin, _) = fx.peek();
        Self {
            fx: fx.recording(TapeCore::start(writer, roots, origin, TAPE_ID)),
            sink,
            said,
        }
    }

    /// The recorder.
    pub(crate) fn core(&self) -> &TapeCore {
        self.fx.tape().expect("a recording door")
    }

    /// The run's end said, and the tape read back: it must read whole.
    pub(crate) fn finish(&self, how: &str) -> Recovered {
        self.fx.finish(End::Returned {
            how: how.to_string(),
        });
        let read = recover(&self.sink.bytes());
        assert_eq!(read.tail, Tail::Clean, "the tape reads back whole");
        assert!(
            matches!(read.items.last(), Some(Item::End(_))),
            "the tape ends with its end"
        );
        read
    }

    /// The tape's bytes with `header` as its header frame in place of the
    /// stand-in [`HEADER`] it was started with: a whole-run helper captures
    /// the run's real header only once its setup is done, after the door
    /// (and so the tape) was handed to the transport. The records frames are
    /// the very bytes the recorder wrote — a header frame stands alone, and
    /// the decoder's state starts afresh after it.
    pub(crate) fn reframed(&self, header: &str) -> Vec<u8> {
        let bytes = self.sink.bytes();
        let first = self.sink.taken().writes.first().copied().unwrap_or(0);
        let mut out = MAGIC.to_vec();
        out.extend(frame_bytes(Kind::Header, 0, header.as_bytes()));
        out.extend_from_slice(&bytes[first..]);
        out
    }
}

/// THE EQUIVALENCE SUITE'S CHECK (the replay plan's §10): the run recorded on
/// `tape` — its real `header`, the requests the scripted worker was `sent`
/// (`Mock::order`'s, the claim's included) and the `lines` it printed —
/// replayed strictly from the tape alone: it must be REPRODUCED, ask exactly
/// the requests the worker was sent and print exactly the lines, with no
/// transport at all (`ReplayCtl` holds none, and the replaying door makes no
/// live call); and replayed a second time it must replay exactly as the first
/// (the determinism self-check: a decision that hangs on a hash map's order,
/// or on anything else the tape does not hold, differs between two replays
/// of one tape).
///
/// Every whole-run helper calls this, so each test that runs the loop over a
/// scripted worker is also a test that the loop's every observation crosses
/// a door: a value read past one is a value the replay cannot serve, and the
/// test fails here, naming the first divergence.
///
/// A run whose options carry a window host (`SuperviseOpts::idle_host`) —
/// a hosted run, and a `watch` whose test stages a host (the stall remedies,
/// the surveys, the idle-point acts of `turn_end_loop_tests`, `stall_tests`,
/// `run_engine_tests` and more) — is replayed the same way: the host's every
/// answer crossed the host's door (`tape::host`), and the replay's host gives
/// them back with no host behind it, so a test's host is never asked twice.
pub(crate) fn assert_replays(
    tape: &MemoryTape,
    header: &Header,
    sent: &[String],
    lines: &[String],
) -> Report {
    let bytes = tape.reframed(&header.encode());
    let replayed = || {
        replay(&bytes, &Options::default())
            .unwrap_or_else(|e| panic!("the recorded run's tape does not replay: {e}"))
    };
    let first = replayed();
    assert_eq!(
        first.verdict,
        Verdict::Reproduced,
        "the recorded run replays from its tape alone:\n{}",
        first.render()
    );
    let asked: Vec<String> = first.requests.iter().map(|w| w.join(" ")).collect();
    assert_eq!(asked, sent, "the replay asks what the recorded run asked");
    let printed: Vec<&str> = first.lines.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(
        printed, lines,
        "the replay prints what the recorded run printed"
    );
    assert_eq!(replayed(), first, "a tape replays the same way twice");
    // ACTS SELF-CONSISTENCY (the plan's T9): the same tape run as a
    // counterfactual under its own policy takes every act it holds, asks
    // what the recorded run asked, and follows the tape record for record —
    // nothing dropped, served again, made up or left unmatched — so acts mode
    // over an unchanged run is a strict replay.
    let acts = replay(
        &bytes,
        &Options {
            candidate: Some(super::acts::Candidate::default()),
            ..Options::default()
        },
    )
    .unwrap_or_else(|e| panic!("the recorded run's tape does not replay in acts mode: {e}"));
    assert_eq!(
        acts.verdict,
        Verdict::Reproduced,
        "the recorded run replays in acts mode under its own policy:\n{}",
        acts.render()
    );
    assert_eq!(
        acts.requests, first.requests,
        "acts mode asks what strict asks"
    );
    assert_eq!(
        acts.lines, first.lines,
        "acts mode prints what strict prints"
    );
    let account = &acts.acts.as_ref().expect("an acts report").account;
    assert!(
        account.exact() && account.consumed == first.effects,
        "acts mode over an unchanged run follows the tape exactly: {account:?} of {} effects",
        first.effects
    );
    first
}

/// A tape of `items` under `header`, written by the real writer: what a test
/// that edits a recorded tape's items (a negative control) replays.
pub(crate) fn written(header: &str, items: &[Item]) -> Vec<u8> {
    let sink = Memory::default();
    let mut w = TapeWriter::start(
        Box::new(sink.clone()),
        "edited.atape",
        header,
        Box::new(Said::default()),
    );
    for item in items {
        w.append(item);
        // Frames closed where the recorder's door closes them: after an act,
        // before a wait.
        if let Some(EffectRecord::Exchange { door, request, .. }) = item.record()
            && door == super::doors::CTL
            && let Ok(words) = super::codec::ctl_args(request)
        {
            let words: Vec<&str> = words.iter().map(String::as_str).collect();
            if super::ctl::is_act(&words) || super::ctl::is_wait(&words) {
                w.flush_hint(Hint::Act);
            }
        }
    }
    w.flush_hint(Hint::Act);
    drop(w);
    sink.bytes()
}
