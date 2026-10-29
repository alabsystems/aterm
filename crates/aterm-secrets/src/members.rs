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
pub mod bundle;
pub mod desk;
pub mod join;
pub mod joiner;
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
/// then each side in turn, the bundle's bytes after the line that counts them, until the
/// member's last answer.
///
/// # Errors
/// Either stream failed, or a line broke the exchange's bounds.
pub fn relay<J, M>(joiner: &mut J, member: &mut M) -> crate::Result<()>
where
    J: std::io::Read + std::io::Write + ?Sized,
    M: std::io::Read + std::io::Write + ?Sized,
{
    loop {
        let line = wire::read_line(member)?;
        wire::write_line(joiner, &line)?;
        let parsed = wire::Line::parse(&line);
        if let Ok(size) = parsed.get("bundle") {
            let size: u64 = size
                .parse()
                .ok()
                .filter(|size| *size <= (bundle::MAX_BUNDLE + 64) as u64)
                .ok_or_else(|| crate::Error::new("the member's bundle size is out of bounds"))?;
            let copied = std::io::copy(&mut std::io::Read::take(&mut *member, size), joiner)?;
            if copied != size {
                return Err(crate::Error::new("the member's bundle was cut short"));
            }
            joiner.flush()?;
            return Ok(());
        }
        if parsed.verb == "NO" || parsed.get("state").is_ok() {
            return Ok(());
        }
        let line = wire::read_line(joiner)?;
        wire::write_line(member, &line)?;
    }
}
