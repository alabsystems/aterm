// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The release-line holder query a fleet survey carries: `survey holder=<pid>.<boot>.<start>`
//! asks the answering host's kernel about exactly one process identity, and its answer is one
//! `release_holder alive=<0|1|-> pid= boot= start=` row echoing only that identity.

/// The survey argument naming a holder identity for the answering host to judge.
pub const ARG: &str = "holder=";

/// The row that answers [`ARG`].
pub const ROW: &str = "release_holder ";

/// One process identity: its pid, the boot it ran in and its kernel start in that boot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub pid: u32,
    pub boot: String,
    pub start: u64,
}

impl Identity {
    /// `<pid>.<boot>.<start>`, the word [`Identity::parse`] reads (`--holder`'s value).
    #[must_use]
    pub fn word(&self) -> String {
        format!("{}.{}.{}", self.pid, self.boot, self.start)
    }

    /// A decimal pid, a lowercase-hex boot id of at most 64 digits and a decimal start, joined
    /// by dots. `None` for anything else.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        let mut parts = word.split('.');
        let (pid, boot, start) = (parts.next()?, parts.next()?, parts.next()?);
        let decimal = |text: &str| {
            !text.is_empty() && text.len() <= 20 && text.bytes().all(|b| b.is_ascii_digit())
        };
        let hex = !boot.is_empty()
            && boot.len() <= 64
            && boot
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if parts.next().is_some() || !decimal(pid) || !decimal(start) || !hex {
            return None;
        }
        Some(Self {
            pid: pid.parse().ok()?,
            boot: boot.to_owned(),
            start: start.parse().ok()?,
        })
    }

    /// The survey argument asking about this identity.
    #[must_use]
    pub fn arg(&self) -> String {
        format!("{ARG}{}", self.word())
    }

    /// The identity a survey argument names, if it is a well-formed [`ARG`].
    #[must_use]
    pub fn of_arg(word: &str) -> Option<Self> {
        Self::parse(word.strip_prefix(ARG)?)
    }

    /// The answer row: `alive` is `Some(true)` for the very process running, `Some(false)` for
    /// positive kernel evidence that it runs nothing, `None` for a read the kernel did not answer.
    #[must_use]
    pub fn row(&self, alive: Option<bool>) -> String {
        format!(
            "{ROW}alive={} pid={} boot={} start={}",
            match alive {
                Some(true) => "1",
                Some(false) => "0",
                None => "-",
            },
            self.pid,
            self.boot,
            self.start
        )
    }

    /// What `row` says of exactly this identity: `None` when it is no well-formed answer about
    /// it, else its `alive` as [`Identity::row`] wrote it.
    #[must_use]
    pub fn read(&self, row: &str) -> Option<Option<bool>> {
        let mut fields = row.strip_prefix(ROW)?.split_ascii_whitespace();
        let alive = match fields.next()?.strip_prefix("alive=")? {
            "1" => Some(true),
            "0" => Some(false),
            "-" => None,
            _ => return None,
        };
        let expected = [
            format!("pid={}", self.pid),
            format!("boot={}", self.boot),
            format!("start={}", self.start),
        ];
        for want in &expected {
            if fields.next()? != want {
                return None;
            }
        }
        fields.next().is_none().then_some(alive)
    }
}

#[cfg(test)]
#[path = "release_holder_tests.rs"]
mod tests;
