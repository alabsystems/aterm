// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The broker's argument boundaries, shared by execution and tier observation.

use std::ffi::OsStr;

pub(crate) enum ParseError {
    Help,
    Usage(String),
}

/// Borrow the words without making a filename into flags or losing its bytes.
pub(crate) struct BrokerArgs<'a, S> {
    pub secret_file: Option<&'a S>,
    pub durability: Option<&'a S>,
    /// `--lifeline-fd <n>` ([`aterm_uds::lifeline::FLAG`]).
    pub lifeline_fd: Option<&'a S>,
    /// Index of `--lifeline-fd` among the arguments `parse` read (not `words`), so display lines
    /// can omit it.
    pub lifeline_at: Option<usize>,
    pub repair: bool,
    /// `--retire-log`: give up the log the one word names.
    pub retire_log: bool,
    pub words: Vec<&'a S>,
}

impl<'a, S: AsRef<OsStr>> BrokerArgs<'a, S> {
    pub fn from_invocation(argv: &'a [S]) -> Option<Self> {
        let args = match argv {
            [_, link, broker, rest @ ..]
                if link.as_ref() == "link" && broker.as_ref() == "broker" =>
            {
                rest
            }
            [exe, broker, rest @ ..]
                if std::path::Path::new(exe.as_ref()).file_name()
                    == Some(OsStr::new("aterm-link"))
                    && broker.as_ref() == "broker" =>
            {
                rest
            }
            _ => return None,
        };
        Self::parse(args).ok()
    }

    pub fn parse(args: &'a [S]) -> Result<Self, ParseError> {
        let mut out = Self {
            secret_file: None,
            durability: None,
            lifeline_fd: None,
            lifeline_at: None,
            repair: false,
            retire_log: false,
            words: Vec::new(),
        };
        let mut i = 0;
        while let Some(arg) = args.get(i) {
            let word = arg.as_ref();
            let slot = match word.to_str() {
                Some("-h" | "--help") => return Err(ParseError::Help),
                Some("--secret-file") => Some((&mut out.secret_file, "--secret-file")),
                Some("--durability") => Some((&mut out.durability, "--durability")),
                Some(aterm_uds::lifeline::FLAG) => {
                    out.lifeline_at = Some(i);
                    Some((&mut out.lifeline_fd, aterm_uds::lifeline::FLAG))
                }
                Some("--repair") => {
                    out.repair = true;
                    None
                }
                Some("--retire-log") => {
                    out.retire_log = true;
                    None
                }
                _ if word.as_encoded_bytes().starts_with(b"-") => {
                    return Err(ParseError::Usage(format!(
                        "unknown flag `{}`",
                        crate::render::safe(&word.to_string_lossy(), 128)
                    )));
                }
                _ => {
                    out.words.push(arg);
                    None
                }
            };
            if let Some((slot, name)) = slot {
                if slot.is_some() {
                    return Err(ParseError::Usage(format!("{name} given twice")));
                }
                i += 1;
                *slot = Some(
                    args.get(i)
                        .ok_or_else(|| ParseError::Usage(format!("{name} needs a value")))?,
                );
            }
            i += 1;
        }
        Ok(out)
    }

    /// Whether the first positional word, the broker's socket path, names `endpoint`.
    pub fn serves(&self, endpoint: &aterm_uds::holders::SocketPath) -> bool {
        self.words
            .first()
            .is_some_and(|word| endpoint.names(word.as_ref().as_encoded_bytes()))
    }

    /// Pure serving syntax only; the secret's bytes remain the execution lane's check.
    pub fn serving_durability(&self) -> Option<crate::durability::Durability> {
        let guarded = self.secret_file.is_some();
        self.named_durability()
            .map(|named| crate::durability::resolve(named, guarded))
    }

    /// The LOG a recognised serving invocation serves, as the broker verb reads its words.
    pub fn log_path(&self) -> Option<std::path::PathBuf> {
        self.named_durability()?;
        let path = match self.words.as_slice() {
            [_, log] => std::path::PathBuf::from(log.as_ref()),
            [sock] => {
                let mut log = sock.as_ref().to_os_string();
                log.push(".log");
                std::path::PathBuf::from(log)
            }
            _ => return None,
        };
        path.is_absolute().then_some(path)
    }

    /// What a recognised serving invocation NAMES.
    pub fn named_durability(&self) -> Option<Option<crate::durability::Durability>> {
        if self.repair || self.retire_log || !matches!(self.words.len(), 1 | 2) {
            return None;
        }
        match self.durability {
            None => Some(None),
            Some(word) => crate::durability::Durability::parse(word.as_ref().to_str()?)
                .ok()
                .map(Some),
        }
    }
}

#[cfg(test)]
#[path = "broker_args_tests.rs"]
mod tests;
