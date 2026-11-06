// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Admission by Allow and a phrase (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.3, F10): a new
//! Mac asks every member it finds; a person presses Allow on any one, which shows a short
//! phrase there; the person types it into the new Mac, which runs CPace over it with that member
//! ([`crate::pake`]). The member then proves the new Mac holds its keys, signs an
//! [`Allowed`], and hands it a [`Bundle`] of its `[everyone]` files under the run's key. An
//! admin's approver admits only a request that carries an `Allowed` its Commons roster binds.
//!
//! [`desk`] is the member's side, [`joiner`] the new Mac's; the exchange runs on the fleet
//! port's `ask` ALPN, the one path a caller without the fleet secret reaches.

pub mod allowed;
mod authority;
pub mod bundle;
pub mod desk;
pub mod join;
pub mod joiner;
pub(crate) mod proof_retry;
mod wire;

pub use crate::pake::PhraseState;
pub use allowed::Allowed;
pub use bundle::Bundle;

#[cfg(test)]
mod tests;

/// Refuse an `ask` exchange this Mac cannot serve (no desk, no window): `NO why=<why>`.
///
/// # Errors
/// The stream failed.
pub fn refuse<S: std::io::Write + ?Sized>(stream: &mut S, why: &str) -> crate::Result<()> {
    wire::write_line(stream, &wire::refusal(why))
}

/// Relay one `ask` exchange between the joining Mac's stream and the member desk's, as the node
/// job does when it holds the fleet port and the window holds the desk: the member speaks first,
/// then each side in turn, the bundle's bytes after the line that counts them, then the joining
/// Mac's acknowledgement of them where its run asked to give one (`ack=1`), until the member's
/// last answer.
///
/// # Errors
/// Either stream failed, or a line broke the exchange's bounds.
pub fn relay<J, M>(joiner: &mut J, member: &mut M) -> crate::Result<()>
where
    J: std::io::Read + std::io::Write + ?Sized,
    M: std::io::Read + std::io::Write + ?Sized,
{
    let mut acks = false;
    loop {
        let line = wire::read_line(member)?;
        wire::write_line(joiner, &line)?;
        let parsed = wire::Line::parse(&line);
        if parsed.verb == "OK"
            && let Ok(size) = parsed.get("authority")
        {
            let size: u64 = Some(size)
                .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|s| s.parse().ok())
                .filter(|size| *size <= crate::membership_log::MAX_RECORDS_STREAM as u64)
                .ok_or_else(|| crate::Error::new("the member's authority size is out of bounds"))?;
            let copied = std::io::copy(&mut std::io::Read::take(&mut *member, size), joiner)?;
            if copied != size {
                return Err(crate::Error::new("the member's authority was cut short"));
            }
            joiner.flush()?;
            continue;
        }
        if let Ok(size) = parsed.get("bundle") {
            let size: u64 = Some(size)
                .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|s| s.parse().ok())
                .filter(|size| *size <= (bundle::MAX_BUNDLE + 64) as u64)
                .ok_or_else(|| crate::Error::new("the member's bundle size is out of bounds"))?;
            let copied = std::io::copy(&mut std::io::Read::take(&mut *member, size), joiner)?;
            if copied != size {
                return Err(crate::Error::new("the member's bundle was cut short"));
            }
            joiner.flush()?;
            if acks {
                let line = wire::read_line(joiner)?;
                wire::write_line(member, &line)?;
            }
            return Ok(());
        }
        if parsed.verb == "NO" || parsed.get("state").is_ok() {
            return Ok(());
        }
        let line = wire::read_line(joiner)?;
        let asked = wire::Line::parse(&line);
        acks |= asked.verb == "run" && asked.get("ack").is_ok_and(|ack| ack == "1");
        wire::write_line(member, &line)?;
    }
}

#[cfg(test)]
mod relay_tests {
    use super::*;
    use std::io::{Cursor, Read, Write};

    struct Duplex {
        incoming: Cursor<&'static [u8]>,
        written: Vec<u8>,
    }

    impl Duplex {
        fn reading(bytes: &'static [u8]) -> Self {
            Self {
                incoming: Cursor::new(bytes),
                written: Vec::new(),
            }
        }
    }

    impl Read for Duplex {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            self.incoming.read(bytes)
        }
    }

    impl Write for Duplex {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.written.write(bytes)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_truncated_authority_frame_forwards_no_possession() {
        let mut member =
            Duplex::reading(b"ALAB member=1 authority=abcdef\nOK authority=4 authoritysig=00\nab");
        let mut joiner = Duplex::reading(b"prove authority=00\npossess response=must-not-send\n");
        assert!(relay(&mut joiner, &mut member).is_err());
        assert_eq!(member.written, b"prove authority=00\n");
        assert_eq!(
            joiner.incoming.position(),
            b"prove authority=00\n".len() as u64
        );
    }
}
