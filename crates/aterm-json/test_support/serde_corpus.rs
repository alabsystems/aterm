// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Recorded answers for the serde corpus: what aterm's models do with a
//! document, holding first-party serde to the reference verdicts.
//!
//! Each crate whose models carry a persistence or security surface includes
//! this file by `#[path]` and records one case per input: an `Ok` as the value
//! written back out (re-serialized bytes, or the value's `Debug` where the
//! model has no `Serialize`), an `Err` as its `Display` text. The answers live
//! in `<crate>/tests/fixtures/serde-corpus/<name>.txt`, one case per line, and
//! are compared whole: a changed, missing or new case is a red that names its
//! id. `ATERM_BLESS_SERDE_CORPUS=1` rewrites the file instead; a re-record
//! belongs in a commit that says which answer moved and why. The variable is
//! this corpus's own, so blessing another crate's goldens (`ATERM_BLESS_GOLDEN`)
//! never re-records an answer the serde switch is held to.

use std::fmt::Write as _;

/// An `Ok` answer longer than this is pinned by digest and length rather than
/// spelled out (a fixture's re-serialized manifest, a checkpoint's meta).
const VERBATIM_MAX: usize = 4096;

pub(crate) struct Corpus {
    name: &'static str,
    path: String,
    cases: Vec<(String, &'static str, String)>,
}

impl Corpus {
    /// The corpus `name` of the crate at `manifest_dir`.
    pub(crate) fn new(manifest_dir: &str, name: &'static str) -> Self {
        Self {
            name,
            path: format!("{manifest_dir}/tests/fixtures/serde-corpus/{name}.txt"),
            cases: Vec::new(),
        }
    }

    /// Record case `id`'s answer.
    pub(crate) fn record<E: std::fmt::Display>(
        &mut self,
        id: impl Into<String>,
        answer: Result<String, E>,
    ) {
        let id = id.into();
        assert!(
            !id.is_empty() && !id.contains(['\t', '\n']),
            "{}: a case id is one non-empty field: {id:?}",
            self.name
        );
        let (verdict, text) = match answer {
            Ok(bytes) if bytes.len() > VERBATIM_MAX => ("ok", digest(&bytes)),
            Ok(bytes) => ("ok", bytes),
            Err(e) => ("err", e.to_string()),
        };
        self.cases.push((id, verdict, escape(&text)));
    }

    /// Hold the recorded answers to the committed file (or rewrite it under
    /// `ATERM_BLESS_SERDE_CORPUS=1`).
    pub(crate) fn finish(self) {
        let got = self.render();
        if std::env::var_os("ATERM_BLESS_SERDE_CORPUS").as_deref()
            == Some(std::ffi::OsStr::new("1"))
        {
            if let Some(dir) = std::path::Path::new(&self.path).parent() {
                std::fs::create_dir_all(dir).expect("serde corpus directory");
            }
            std::fs::write(&self.path, got).expect("bless serde corpus");
            return;
        }
        self.compare(&got);
    }

    /// The answers file the recorded cases make, exactly as [`Self::finish`]
    /// holds it to the committed one (and blesses it): refused when a case id
    /// is recorded twice or every case answers alike.
    pub(crate) fn render(&self) -> String {
        let mut ids: Vec<&str> = self.cases.iter().map(|(id, _, _)| id.as_str()).collect();
        ids.sort_unstable();
        if let Some(pair) = ids.windows(2).find(|pair| pair[0] == pair[1]) {
            panic!("{}: case id {:?} is recorded twice", self.name, pair[0]);
        }
        // NEGATIVE CONTROL: a corpus whose every case answers alike (a harness
        // that records nothing, an input that never reaches the model) cannot
        // tell one serde from another.
        for verdict in ["ok", "err"] {
            assert!(
                self.cases.iter().any(|(_, v, _)| *v == verdict),
                "{}: no case answered {verdict}; the corpus would pass vacuously",
                self.name
            );
        }
        let mut got = format!(
            "# serde corpus `{}`: one case per line, `<id> TAB ok|err TAB <answer>`.\n\
             # ok: the value written back out; err: the error's Display. \\n \\t \\r \\\\ and\n\
             # \\u{{..}} escape control characters; an ok longer than {VERBATIM_MAX} bytes is its\n\
             # FNV-1a digest and length. docs/AUDIT-third-party-2026-10-15.md, SP.3.\n",
            self.name
        );
        for (id, verdict, text) in &self.cases {
            let _ = writeln!(got, "{id}\t{verdict}\t{text}");
        }
        got
    }

    /// Panic, naming each moved answer, unless `got` is the committed file.
    fn compare(&self, got: &str) {
        let want = std::fs::read_to_string(&self.path).unwrap_or_else(|e| {
            panic!(
                "missing serde corpus {}: {e}; record it with ATERM_BLESS_SERDE_CORPUS=1",
                self.path
            )
        });
        // A Windows checkout under `core.autocrlf` ends each line CRLF; an
        // answer's own carriage return is always written escaped, so a raw one
        // is only ever the checkout's.
        let want = want.replace("\r\n", "\n");
        if want == got {
            return;
        }
        let answers = |text: &str| -> Vec<(String, String)> {
            text.lines()
                .filter(|line| !line.starts_with('#'))
                .map(|line| {
                    let (id, answer) = line.split_once('\t').unwrap_or((line, ""));
                    (id.to_string(), answer.to_string())
                })
                .collect()
        };
        let (want, got) = (answers(&want), answers(got));
        let mut report = String::new();
        let mut moved = 0usize;
        for (id, answer) in &got {
            match want.iter().find(|(w, _)| w == id) {
                Some((_, recorded)) if recorded == answer => {}
                Some((_, recorded)) => {
                    moved += 1;
                    let _ = writeln!(
                        report,
                        "  {id}\n    recorded {recorded}\n    now      {answer}"
                    );
                }
                None => {
                    moved += 1;
                    let _ = writeln!(report, "  {id}\n    new case {answer}");
                }
            }
        }
        for (id, recorded) in &want {
            if !got.iter().any(|(g, _)| g == id) {
                moved += 1;
                let _ = writeln!(report, "  {id}\n    no longer recorded (was {recorded})");
            }
        }
        if moved == 0 {
            report.push_str("  (same answers; the header or the case order changed)\n");
        }
        panic!(
            "serde corpus `{}`: {moved} answer(s) moved from {}:\n{report}\
             A model or serde change moved these. Fix the change, or re-record with \
             ATERM_BLESS_SERDE_CORPUS=1 and say in the commit which answer moved and why.",
            self.name, self.path
        );
    }
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{{{:x}}}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out
}

/// `text` pinned by its FNV-1a digest (as `aterm-json`'s `Golden` pins its
/// sweeps) and its length: the answer a long `Ok` is recorded as, and what a
/// sweep over large fixtures records for every `Ok`.
pub(crate) fn digest(text: &str) -> String {
    let hash = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, b| {
        (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("fnv1a64:{hash:016x} len={}", text.len())
}
