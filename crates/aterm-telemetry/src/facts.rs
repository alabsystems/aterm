// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HOST'S FACTS: what a row's numbers were measured on. A row carries their digest
//! (`facts=`); each log file opens with a `kind=host` line that spells them, so a reader can
//! name the machine and check the digest.

use std::fmt::Write as _;

use crate::row::{Digest, SCHEMA, find, host_word, push_word_of, seal};

/// The machine and the build a row was measured on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostFacts {
    /// The host's name ([`host_word`]).
    pub host: String,
    /// The machine model (`hw.model`: `Mac16,6`); `-` where unread.
    pub model: String,
    /// The CPU's brand (`Apple M4 Max`); `-` where unread.
    pub cpu: String,
    /// Logical cores.
    pub cores: u32,
    /// Physical memory, MiB (0 where unread).
    pub mem_mib: u64,
    /// The OS and its version (`macOS 27.0.1`); `-` where unread.
    pub os: String,
    /// The aterm build (`0.153.0+g<sha>`).
    pub aterm: String,
}

impl HostFacts {
    /// This machine and this build, now.
    #[must_use]
    pub fn read() -> Self {
        let words = aterm_sysprobe::machine_words();
        Self {
            host: aterm_uds::process::lan_host_name()
                .or_else(aterm_uds::process::host_name)
                .map_or_else(|| "-".to_owned(), |h| host_word(&h)),
            model: aterm_sysprobe::machine_model().unwrap_or_else(|| "-".to_owned()),
            cpu: words.cpu.unwrap_or_else(|| "-".to_owned()),
            cores: std::thread::available_parallelism()
                .ok()
                .and_then(|n| u32::try_from(n.get()).ok())
                .unwrap_or(0),
            mem_mib: aterm_sysprobe::physical_memory_mib().unwrap_or(0),
            os: words.os.unwrap_or_else(|| "-".to_owned()),
            aterm: aterm_types::version::build_id(),
        }
    }

    /// The facts as the log spells them: each text a word ([`crate::row::word_of`]).
    fn fields(&self) -> String {
        let mut s = String::with_capacity(128);
        s.push_str("host=");
        let first = self.host.trim().split('.').next().unwrap_or("");
        if first.is_empty() {
            s.push('-');
        } else {
            push_word_of(&mut s, first, 63);
        }
        s.push_str(" model=");
        push_word_of(&mut s, &self.model, 64);
        s.push_str(" cpu=");
        push_word_of(&mut s, &self.cpu, 64);
        let _ = write!(s, " cores={} mem_mib={} os=", self.cores, self.mem_mib);
        push_word_of(&mut s, &self.os, 64);
        s.push_str(" aterm=");
        push_word_of(&mut s, &self.aterm, 64);
        s
    }

    /// Their digest: of the words the log spells, so a reader recomputes it from the line.
    #[must_use]
    pub fn digest(&self) -> Digest {
        Digest::of("facts/1", &[self.fields().as_bytes()])
    }

    /// The `kind=host` line written at `at_ms`.
    #[must_use]
    pub fn line(&self, at_ms: u64) -> String {
        let fields = self.fields();
        let digest = Digest::of("facts/1", &[fields.as_bytes()]);
        let mut out = String::with_capacity(64 + fields.len());
        let _ = write!(out, "schema={SCHEMA} at={at_ms} kind=host facts=");
        crate::row::push_hex(&mut out, &digest.0);
        out.push(' ');
        out.push_str(&fields);
        seal(out)
    }
}

/// A `kind=host` line read back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostLine {
    /// When it was written, unix ms.
    pub at_ms: u64,
    /// The digest it names.
    pub facts: Digest,
    /// The facts it spells.
    pub spelled: HostFacts,
}

impl HostLine {
    pub(crate) fn parse(at_ms: u64, w: &[(&str, &str)]) -> Option<Self> {
        let spelled = HostFacts {
            host: find(w, "host")?.to_owned(),
            model: find(w, "model")?.to_owned(),
            cpu: find(w, "cpu")?.to_owned(),
            cores: find(w, "cores")?.parse().ok()?,
            mem_mib: find(w, "mem_mib")?.parse().ok()?,
            os: find(w, "os")?.to_owned(),
            aterm: find(w, "aterm")?.to_owned(),
        };
        let facts = Digest::parse(find(w, "facts")?)?;
        // A line whose spelled facts do not hash to the digest it names is no line of ours.
        (spelled.digest() == facts).then_some(Self {
            at_ms,
            facts,
            spelled,
        })
    }
}
