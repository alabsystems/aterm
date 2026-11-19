// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::{
    cell::Cell,
    io::{self, Read, Write},
};

/// What a test does to the bytes this thread's session writes after its credential handoff.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Fault {
    /// They go out as written.
    #[default]
    None,
    /// Once this many went out every write fails, as a session that ends there.
    Cut(usize),
    /// The byte at this offset goes out with its lowest bit changed.
    Garble(usize),
}

thread_local! {
    static FAULT: Cell<Fault> = const { Cell::new(Fault::None) };
    static MOVED: Cell<[usize; 2]> = const { Cell::new([0, 0]) };
}

/// Run `session`, one side of a round on this thread, under `fault`: what it returned, and the
/// bytes that side wrote and read after its credential handoff.
pub(crate) fn observed<T>(fault: Fault, session: impl FnOnce() -> T) -> (T, [usize; 2]) {
    FAULT.with(|slot| slot.set(fault));
    MOVED.with(|slot| slot.set([0, 0]));
    let out = session();
    FAULT.with(|slot| slot.set(Fault::None));
    (out, MOVED.with(Cell::get))
}

/// A session's stream from the end of its credential handoff: it counts what moves through it
/// for [`observed`] and applies that thread's [`Fault`] to what is written.
pub(crate) struct Tail<'a, S> {
    inner: &'a mut S,
    written: usize,
}

impl<'a, S> Tail<'a, S> {
    pub(crate) fn of(inner: &'a mut S) -> Self {
        Self { inner, written: 0 }
    }
}

impl<S: Read> Read for Tail<'_, S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        MOVED.with(|slot| slot.set([slot.get()[0], slot.get()[1] + n]));
        Ok(n)
    }
}

impl<S: Write> Write for Tail<'_, S> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = match FAULT.with(Cell::get) {
            Fault::None => self.inner.write(buf)?,
            Fault::Cut(at) if self.written >= at => {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "the session ended",
                ));
            }
            Fault::Cut(at) => self.inner.write(&buf[..buf.len().min(at - self.written)])?,
            Fault::Garble(at) => {
                let mut bytes = buf.to_vec();
                if let Some(byte) = at
                    .checked_sub(self.written)
                    .and_then(|at| bytes.get_mut(at))
                {
                    *byte ^= 1;
                }
                self.inner.write(&bytes)?
            }
        };
        self.written += n;
        MOVED.with(|slot| slot.set([slot.get()[0] + n, slot.get()[1]]));
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
