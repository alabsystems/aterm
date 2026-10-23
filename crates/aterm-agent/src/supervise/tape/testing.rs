// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! In-memory tape recorder and replay assertion helpers for supervisor loop tests.

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

/// Default header frame used for in-memory test tapes.
pub(crate) const HEADER: &str = "atape 1\ntape=test\nentry=watch\n";

/// Default tape id used for in-memory test tapes.
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

    /// Returns the tape bytes with `header` replacing the initial [`HEADER`] frame.
    pub(crate) fn reframed(&self, header: &str) -> Vec<u8> {
        let bytes = self.sink.bytes();
        let first = self.sink.taken().writes.first().copied().unwrap_or(0);
        let mut out = MAGIC.to_vec();
        out.extend(frame_bytes(Kind::Header, 0, header.as_bytes()));
        out.extend_from_slice(&bytes[first..]);
        out
    }
}

/// Asserts that `tape` replays deterministically in strict and acts modes.
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

/// Writes `items` under `header` into an in-memory tape.
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
