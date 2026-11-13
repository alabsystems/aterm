// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use super::HostDisk;
use crate::linux::Out;

/// One call the lane made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Call {
    /// `shell` (with its stdin) or `shell_streamed` (`merged`).
    Shell {
        /// The argv.
        argv: Vec<String>,
        /// What was written to its stdin.
        stdin: Option<String>,
        /// Whether it was `shell_streamed`.
        merged: bool,
    },
    /// `copy_out`.
    CopyOut(String),
    /// `copy_dir_in`.
    CopyDirIn(String),
    /// `copy_dir_out`.
    CopyDirOut(String),
    /// `end`, with `booted_here` and `keep`.
    End {
        /// Whether the run booted the VM.
        booted_here: bool,
        /// Whether it is kept up.
        keep: bool,
    },
}

/// An answer: an exit code, or one of the agent's other three ends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    /// `Exited(code)`, saying `text`.
    Exited(i32, String),
    /// `Signaled(signal)`.
    Signaled(i32),
    /// `NotStarted(reason)`.
    NotStarted(String),
    /// `Lost(why)`.
    Lost(String),
}

/// A scripted guest: the first rule whose words all appear in a call's argv answers it;
/// anything else exits 0, saying nothing.
#[derive(Clone, Debug, Default)]
pub struct Guest {
    rules: Vec<(Vec<String>, Answer)>,
    /// Every call, in order.
    pub calls: Arc<Mutex<Vec<Call>>>,
    /// What `host_disk` says.
    pub host_disk: Option<HostDisk>,
    /// Files `copy_out` writes, by guest path.
    pub files: BTreeMap<String, Vec<u8>>,
    /// The first `Lost` answered.
    lost: Arc<Mutex<Option<String>>>,
}

impl Guest {
    /// A guest whose every command exits 0.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Answer a call whose argv holds every one of `words` with `answer`.
    #[must_use]
    pub fn answer(mut self, words: &[&str], answer: Answer) -> Self {
        self.rules
            .push((words.iter().map(|w| (*w).to_string()).collect(), answer));
        self
    }

    /// As [`super::Driver::lost`]: the first `Lost` answered.
    #[must_use]
    pub fn lost(&self) -> Option<String> {
        self.lost
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// The calls so far.
    #[must_use]
    pub fn calls(&self) -> Vec<Call> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn record(&self, call: Call) {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(call);
    }

    fn answer_for(&self, argv: &[String]) -> Out {
        let answer = self
            .rules
            .iter()
            .find(|(words, _)| words.iter().all(|w| argv.contains(w)))
            .map_or(Answer::Exited(0, String::new()), |(_, a)| a.clone());
        let mut out = Out {
            code: None,
            text: String::new(),
            secs: 0.0,
            signal: None,
            not_started: None,
            lost: None,
        };
        match answer {
            Answer::Exited(code, text) => {
                out.code = Some(code);
                out.text = text;
            }
            Answer::Signaled(signal) => {
                out.signal = Some(signal);
                out.text = crate::linux_vz_driver::signal_words(signal);
            }
            Answer::NotStarted(why) => out.not_started = Some(why),
            Answer::Lost(why) => {
                self.lost
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get_or_insert_with(|| why.clone());
                out.lost = Some(why);
            }
        }
        out
    }

    /// As [`super::Driver::shell`].
    #[must_use]
    pub fn shell(&self, _workdir: &str, argv: &[String], stdin: Option<&str>) -> Out {
        self.record(Call::Shell {
            argv: argv.to_vec(),
            stdin: stdin.map(str::to_string),
            merged: false,
        });
        self.answer_for(argv)
    }

    /// As [`super::Driver::shell_streamed`].
    #[must_use]
    pub fn shell_streamed(&self, _workdir: &str, argv: &[String], _prefix: &str) -> Out {
        self.record(Call::Shell {
            argv: argv.to_vec(),
            stdin: None,
            merged: true,
        });
        self.answer_for(argv)
    }

    /// As [`super::Driver::copy_out`]: writes the scripted file.
    #[must_use]
    pub fn copy_out(&self, guest_path: &str, host_path: &Path) -> Out {
        self.record(Call::CopyOut(guest_path.into()));
        let mut out = self.answer_for(&[guest_path.to_string()]);
        match self.files.get(guest_path) {
            Some(bytes) => {
                if let Err(e) = crate::util::write_fresh(host_path, bytes) {
                    out.code = Some(1);
                    out.text = e.to_string();
                }
            }
            None => {
                out.code = Some(1);
                out.text = format!("cat: {guest_path}: No such file or directory\n");
            }
        }
        out
    }

    /// As [`super::Driver::copy_dir_in`].
    #[must_use]
    pub fn copy_dir_in(&self, host_dir: &Path, _guest_parent: &str) -> Out {
        self.record(Call::CopyDirIn(host_dir.display().to_string()));
        self.answer_for(&[host_dir.display().to_string()])
    }

    /// As [`super::Driver::copy_dir_out`].
    #[must_use]
    pub fn copy_dir_out(&self, guest_dir: &str, _host_parent: &Path) -> Out {
        self.record(Call::CopyDirOut(guest_dir.into()));
        self.answer_for(&[guest_dir.to_string()])
    }

    /// As [`super::Driver::end`].
    pub fn end(&self, booted_here: bool, keep: bool) {
        self.record(Call::End { booted_here, keep });
    }
}
