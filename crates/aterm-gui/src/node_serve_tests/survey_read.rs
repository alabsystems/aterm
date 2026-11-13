// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A window's counted survey answer read through a buffer (`read_counted_through`): the answer
//! the byte loop it replaced gave, whole, cut short or refused, in a read for each buffer of it
//! and not one for each byte. Counted, never timed.

use std::io::Read;
use std::time::{Duration, Instant};

use super::super::{
    SURVEY_LINE_MAX, SURVEY_LINES_MAX, SURVEY_READ_BUFFER, counted_answer, read_counted_through,
    read_line_raw,
};

/// What a [`Held`] stream answers once its bytes are read.
#[derive(Clone, Copy)]
enum Then {
    /// The peer hung up.
    End,
    /// The peer sent nothing more by the read's timeout.
    TimesOut,
}

/// A stream that holds `bytes` and gives each read what it asks of them, at most `segment`,
/// counting the reads made of it.
struct Held<'a> {
    bytes: &'a [u8],
    at: usize,
    segment: usize,
    then: Then,
    /// Whether the next read is interrupted before it takes anything.
    interrupted: bool,
    reads: usize,
}

impl<'a> Held<'a> {
    fn new(bytes: &'a [u8], segment: usize, then: Then) -> Self {
        Self {
            bytes,
            at: 0,
            segment,
            then,
            interrupted: false,
            reads: 0,
        }
    }
}

impl Read for Held<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.reads += 1;
        if std::mem::take(&mut self.interrupted) {
            return Err(std::io::ErrorKind::Interrupted.into());
        }
        let n = buf.len().min(self.segment).min(self.bytes.len() - self.at);
        if n == 0 && matches!(self.then, Then::TimesOut) {
            return Err(std::io::ErrorKind::WouldBlock.into());
        }
        buf[..n].copy_from_slice(&self.bytes[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

/// A deadline no read here reaches: the hang detector's.
fn ahead() -> Instant {
    Instant::now() + aterm_spec::wait::MUST_HAPPEN
}

/// The byte loop `read_counted` was before it read through a buffer: each line a `read` a byte
/// ([`read_line_raw`]), started only while `deadline` is ahead.
fn bytewise(stream: &mut Held<'_>, deadline: Instant) -> Result<String, Option<String>> {
    counted_answer(|| {
        deadline
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())?;
        read_line_raw(stream, SURVEY_LINE_MAX).ok().flatten()
    })
}

/// What the buffered reader and the byte loop each made of a stream.
struct Both {
    answer: Result<String, Option<String>>,
    /// The reads the buffered reader made of a stream that gives each read all it asks.
    reads: usize,
    /// The reads the byte loop made of it.
    bytewise_reads: usize,
}

/// Read `bytes`, then `then`, through the buffer and by the byte loop: one answer, however the
/// bytes arrive (all a read asks, a thousand at a read, one at a read).
fn both(bytes: &[u8], then: Then) -> Both {
    let mut by_byte = Held::new(bytes, usize::MAX, then);
    let answer = bytewise(&mut by_byte, ahead());
    let mut reads = 0;
    for segment in [usize::MAX, 1000, 1] {
        let mut held = Held::new(bytes, segment, then);
        let buffered = read_counted_through(&mut held, ahead(), |_, _| {});
        assert_eq!(buffered, answer, "segments of {segment}");
        if segment == usize::MAX {
            reads = held.reads;
        }
    }
    Both {
        answer,
        reads,
        bytewise_reads: by_byte.reads,
    }
}

/// A line of `len` bytes, its line feed among them.
fn line(len: usize) -> String {
    format!("{}\n", "x".repeat(len - 1))
}

/// An answer the window sent whole is taken in a read for each [`SURVEY_READ_BUFFER`] bytes of
/// it, where the byte loop made one for each byte: a short one, one of many buffers, and one
/// whose lines end exactly at a buffer's edge. Each is the byte loop's answer.
/// NEGATIVE CONTROL: with `read_counted_through` reading each line by `read_line_raw`, the
/// short answer's 34 bytes take 34 reads, not one.
#[test]
fn a_whole_answer_takes_a_read_a_buffer_and_is_the_byte_loops() {
    let short = "OK 2\ninstance pid=1\nsession sid=7\n";
    let read = both(short.as_bytes(), Then::End);
    assert_eq!(read.answer.as_deref(), Ok(short));
    assert_eq!(read.reads, 1);
    assert_eq!(read.bytewise_reads, short.len());

    let long = format!("OK 40\n{}", line(1000).repeat(40));
    let read = both(long.as_bytes(), Then::End);
    assert_eq!(read.answer.as_deref(), Ok(long.as_str()));
    assert_eq!(read.reads, long.len().div_ceil(SURVEY_READ_BUFFER));
    assert_eq!(read.bytewise_reads, long.len());

    // The head and the first line fill one buffer exactly, the second line the next.
    let head = "OK 2\n";
    let edge = format!(
        "{head}{}{}",
        line(SURVEY_READ_BUFFER - head.len()),
        line(SURVEY_READ_BUFFER)
    );
    assert_eq!(edge.len(), 2 * SURVEY_READ_BUFFER);
    let read = both(edge.as_bytes(), Then::End);
    assert_eq!(read.answer.as_deref(), Ok(edge.as_str()));
    assert_eq!(read.reads, 2);
    assert_eq!(read.bytewise_reads, edge.len());

    // A peer that sends its whole answer and stays connected is not read past it.
    let read = both(short.as_bytes(), Then::TimesOut);
    assert_eq!(read.answer.as_deref(), Ok(short));
    assert_eq!(read.reads, 1);

    // Bytes that follow the answer are not part of it, taken into the buffer or not.
    let followed = format!("{short}session sid=8\n");
    let read = both(followed.as_bytes(), Then::End);
    assert_eq!(read.answer.as_deref(), Ok(short));
    assert_eq!(read.reads, 1);

    // CR LF line ends count as the byte loop counted them.
    let read = both(b"OK 1\r\nsession sid=7\r\n", Then::End);
    assert_eq!(read.answer.as_deref(), Ok("OK 1\r\nsession sid=7\r\n"));
}

/// An answer cut short is the byte loop's refusal, in at most one read more than its buffers:
/// the peer gone or silent before the lines it counted, in a line or between two.
#[test]
fn a_cut_answer_is_refused_as_the_byte_loop_refused_it() {
    for then in [Then::End, Then::TimesOut] {
        let between = "OK 5\nsession sid=1\nsession sid=2\nsession sid=3\n";
        let read = both(between.as_bytes(), then);
        assert_eq!(read.answer, Err(None));
        assert_eq!(read.reads, 2, "the answer, then its end");

        let read = both(b"OK 3\nsession sid=1\nsessi", then);
        assert_eq!(read.answer, Err(None), "cut in a line, more counted");

        let read = both(b"", then);
        assert_eq!(read.answer, Err(None), "no answer at all");
        assert_eq!(read.reads, 1);

        let long = format!("OK 41\n{}", line(1000).repeat(40));
        let read = both(long.as_bytes(), then);
        assert_eq!(read.answer, Err(None));
        assert_eq!(read.reads, long.len().div_ceil(SURVEY_READ_BUFFER) + 1);
    }

    // The byte loop took a last line the peer hung up in as that line, and so does the buffer;
    // a peer that went silent in it gave neither a line.
    let cut = "OK 2\nsession sid=1\nsessi";
    assert_eq!(both(cut.as_bytes(), Then::End).answer.as_deref(), Ok(cut));
    assert_eq!(both(cut.as_bytes(), Then::TimesOut).answer, Err(None));
    // So with a head the peer hung up in.
    let head = both(b"ERR busy", Then::End);
    assert_eq!(head.answer, Err(Some("ERR busy".to_owned())));
}

/// An answer past a bound, or not a counted one, is refused as the byte loop refused it: more
/// lines than a survey sends, a line longer than a request line, more bytes than a fleet reader
/// takes, a count that is no number, bytes that are not text, and a window's own one line.
#[test]
fn an_answer_past_its_bounds_is_refused_as_the_byte_loop_refused_it() {
    let lines = format!("OK {}\n", SURVEY_LINES_MAX + 1);
    assert_eq!(both(lines.as_bytes(), Then::End).answer, Err(None));
    let lines = format!("OK {SURVEY_LINES_MAX}\n{}", "s\n".repeat(SURVEY_LINES_MAX));
    assert_eq!(
        both(lines.as_bytes(), Then::End).answer.as_deref(),
        Ok(lines.as_str())
    );

    // A line of the bound's bytes and its line feed is whole; one byte more is refused, at the
    // end of the answer or with a line feed after it.
    let longest = format!("OK 1\n{}", line(SURVEY_LINE_MAX + 1));
    assert_eq!(
        both(longest.as_bytes(), Then::End).answer.as_deref(),
        Ok(longest.as_str())
    );
    let over = format!("OK 1\n{}", "x".repeat(SURVEY_LINE_MAX + 1));
    assert_eq!(both(over.as_bytes(), Then::End).answer, Err(None));
    let over = format!("OK 1\n{}", line(SURVEY_LINE_MAX + 2));
    assert_eq!(both(over.as_bytes(), Then::End).answer, Err(None));
    let unterminated = format!("OK 1\n{}", "x".repeat(SURVEY_LINE_MAX));
    assert_eq!(
        both(unterminated.as_bytes(), Then::End).answer.as_deref(),
        Ok(unterminated.as_str())
    );

    // A reply of the fleet reader's bound is whole; one byte more is refused.
    let head = "OK 5\n";
    let last = aterm_net::fleet::REPLY_MAX - head.len() - 4 * 60_000;
    let most = format!("{head}{}{}", line(60_000).repeat(4), line(last));
    assert_eq!(most.len(), aterm_net::fleet::REPLY_MAX);
    assert_eq!(
        both(most.as_bytes(), Then::End).answer.as_deref(),
        Ok(most.as_str())
    );
    let over = format!("{head}{}{}", line(60_000).repeat(4), line(last + 1));
    assert_eq!(both(over.as_bytes(), Then::End).answer, Err(None));

    for count in [
        "OK 007\n",
        "OK x\n",
        "OK -1\n",
        "OK 99999999999999999999999\n",
    ] {
        assert_eq!(
            both(count.as_bytes(), Then::End).answer,
            Err(None),
            "{count}"
        );
    }
    for own in ["ERR busy\n", "OK\n", "OK \n", "\n"] {
        assert_eq!(
            both(own.as_bytes(), Then::End).answer,
            Err(Some(own.to_owned())),
            "{own:?}"
        );
    }
    assert_eq!(
        both(b"OK 1\nsession \xff\n", Then::End).answer,
        Err(None),
        "a line that is not UTF-8"
    );
    assert_eq!(
        both(b"\xffOK 1\nsession\n", Then::End).answer,
        Err(None),
        "a head that is not UTF-8"
    );
}

/// The deadline and each line's wait are the byte loop's: no line starts once the deadline
/// passed, an interrupted read is made again, and the stream is given a line's wait before
/// that line's first read and never for a line the buffer already held.
#[test]
fn a_line_starts_only_before_the_deadline_and_bounds_its_own_reads() {
    let short = "OK 2\ninstance pid=1\nsession sid=7\n";

    // `Instant::now` is not ahead of itself: nothing is read.
    let passed = Instant::now();
    let mut held = Held::new(short.as_bytes(), usize::MAX, Then::End);
    assert_eq!(
        read_counted_through(&mut held, passed, |_, _| {}),
        Err(None)
    );
    assert_eq!(held.reads, 0);
    let mut held = Held::new(short.as_bytes(), usize::MAX, Then::End);
    assert_eq!(bytewise(&mut held, passed), Err(None));
    assert_eq!(held.reads, 0);

    for segment in [usize::MAX, 1] {
        let mut held = Held::new(short.as_bytes(), segment, Then::End);
        held.interrupted = true;
        let read = read_counted_through(&mut held, ahead(), |_, _| {});
        assert_eq!(read.as_deref(), Ok(short), "segments of {segment}");
    }

    // Each wait is given before a read, and is what is left to the deadline: never more than
    // the whole of it.
    let waits = |segment: usize| {
        let mut given: Vec<(usize, Duration)> = Vec::new();
        let mut held = Held::new(short.as_bytes(), segment, Then::End);
        let read = read_counted_through(&mut held, ahead(), |held, left| {
            given.push((held.reads, left));
        });
        assert_eq!(read.as_deref(), Ok(short));
        assert!(
            given
                .iter()
                .all(|(_, left)| !left.is_zero() && *left <= aterm_spec::wait::MUST_HAPPEN)
        );
        given
            .into_iter()
            .map(|(reads, _)| reads)
            .collect::<Vec<_>>()
    };
    // One read takes all three lines: the head's wait, and none for the two the buffer held.
    assert_eq!(waits(usize::MAX), [0]);
    // A byte a read: each of the three lines is given its wait before its first byte.
    assert_eq!(
        waits(1),
        [0, "OK 2\n".len(), "OK 2\ninstance pid=1\n".len()]
    );
}
