// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HOST'S FACTS: what a row's numbers were measured on. A row carries their digest
//! (`facts=`); each log file opens with a `kind=host` line that spells them, so a reader can
//! name the machine and check the digest.

use std::fmt::Write as _;

use crate::row::{Digest, SCHEMA, host_word, seal_in_place, word_char};

/// The machine and the build a row was measured on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostFacts {
    /// The host's name ([`host_word`]).
    pub host: String,
    /// The machine model (`hw.model`: `Mac16,6`); `-` where unread.
    pub model: String,
    /// The CPU's brand (`Apple M4 Max`); `-` where unread.
    pub cpu: String,
    /// The machine's logical cores (`aterm_sysprobe::cpu_layout`), not this process's
    /// parallelism, which an affinity mask or a cgroup quota narrows; 0 where unread.
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
            cores: u32::try_from(aterm_sysprobe::cpu_layout().logical_cores()).unwrap_or(u32::MAX),
            mem_mib: aterm_sysprobe::physical_memory_mib().unwrap_or(0),
            os: words.os.unwrap_or_else(|| "-".to_owned()),
            aterm: aterm_stamp::build_id(),
        }
    }

    /// Calls `f` with the facts as the log spells them without allocating on the heap.
    fn with_fields<R>(&self, f: impl FnOnce(&str) -> R) -> R {
        struct Buf([u8; 512], usize);
        impl std::fmt::Write for Buf {
            fn write_str(&mut self, s: &str) -> std::fmt::Result {
                let bytes = s.as_bytes();
                self.0[self.1..self.1 + bytes.len()].copy_from_slice(bytes);
                self.1 += bytes.len();
                Ok(())
            }
        }
        let push_word = |b: &mut Buf, text: &str, max: usize| {
            for c in text.chars().take(max) {
                b.0[b.1] = if word_char(c) { c as u8 } else { b'_' };
                b.1 += 1;
            }
        };
        let mut b = Buf([0u8; 512], 0);
        let _ = b.write_str("host=");
        let first = self.host.trim().split('.').next().unwrap_or("");
        if first.is_empty() {
            let _ = b.write_str("-");
        } else {
            push_word(&mut b, first, 63);
        }
        let _ = b.write_str(" model=");
        push_word(&mut b, &self.model, 64);
        let _ = b.write_str(" cpu=");
        push_word(&mut b, &self.cpu, 64);
        let _ = write!(b, " cores={} mem_mib={} os=", self.cores, self.mem_mib);
        push_word(&mut b, &self.os, 64);
        let _ = b.write_str(" aterm=");
        push_word(&mut b, &self.aterm, 64);
        f(std::str::from_utf8(&b.0[..b.1]).unwrap_or_default())
    }

    /// Their digest: of the words the log spells, so a reader recomputes it from the line.
    #[must_use]
    pub fn digest(&self) -> Digest {
        self.with_fields(|fields| Digest::of("facts/1", &[fields.as_bytes()]))
    }

    pub(crate) fn write_line_into(&self, at_ms: u64, out: &mut String) {
        self.with_fields(|fields| {
            let digest = Digest::of("facts/1", &[fields.as_bytes()]);
            out.reserve(64 + fields.len());
            let _ = write!(out, "schema={SCHEMA} at={at_ms} kind=host facts=");
            crate::row::push_hex(out, &digest.0);
            out.push(' ');
            out.push_str(fields);
            seal_in_place(out);
        });
    }

    /// The `kind=host` line written at `at_ms`.
    #[must_use]
    pub fn line(&self, at_ms: u64) -> String {
        let mut out = String::new();
        self.write_line_into(at_ms, &mut out);
        out
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
        let mut host = None;
        let mut model = None;
        let mut cpu = None;
        let mut cores = None;
        let mut mem_mib = None;
        let mut os = None;
        let mut aterm = None;
        let mut facts = None;
        for &(k, v) in w {
            match k {
                "host" => {
                    host.get_or_insert(v);
                }
                "model" => {
                    model.get_or_insert(v);
                }
                "cpu" => {
                    cpu.get_or_insert(v);
                }
                "cores" => {
                    cores.get_or_insert(v);
                }
                "mem_mib" => {
                    mem_mib.get_or_insert(v);
                }
                "os" => {
                    os.get_or_insert(v);
                }
                "aterm" => {
                    aterm.get_or_insert(v);
                }
                "facts" => {
                    facts.get_or_insert(v);
                }
                _ => {}
            }
        }
        let facts = Digest::parse(facts?)?;
        let cores = cores?.parse().ok()?;
        let mem_mib = mem_mib?.parse().ok()?;
        let spelled = HostFacts {
            host: host?.to_owned(),
            model: model?.to_owned(),
            cpu: cpu?.to_owned(),
            cores,
            mem_mib,
            os: os?.to_owned(),
            aterm: aterm?.to_owned(),
        };
        // A line whose spelled facts do not hash to the digest it names is no line of ours.
        (spelled.digest() == facts).then_some(Self {
            at_ms,
            facts,
            spelled,
        })
    }
}
