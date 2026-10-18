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
    pub tcp: Option<&'a S>,
    pub key_file: Option<&'a S>,
    pub secret_file: Option<&'a S>,
    pub unix: Option<&'a S>,
    pub durability: Option<&'a S>,
    pub allow_remote: bool,
    pub repair: bool,
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
            tcp: None,
            key_file: None,
            secret_file: None,
            unix: None,
            durability: None,
            allow_remote: false,
            repair: false,
            words: Vec::new(),
        };
        let mut i = 0;
        while let Some(arg) = args.get(i) {
            let word = arg.as_ref();
            let slot = match word.to_str() {
                Some("-h" | "--help") => return Err(ParseError::Help),
                Some("--tcp") => Some((&mut out.tcp, "--tcp")),
                Some("--key-file") => Some((&mut out.key_file, "--key-file")),
                Some("--secret-file") => Some((&mut out.secret_file, "--secret-file")),
                Some("--unix") => Some((&mut out.unix, "--unix")),
                Some("--durability") => Some((&mut out.durability, "--durability")),
                Some("--allow-remote") => {
                    out.allow_remote = true;
                    None
                }
                Some("--repair") => {
                    out.repair = true;
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

    /// Confirm a display-discovered candidate against the actual option
    /// boundaries before attributing a tier to this endpoint.
    pub fn serves_endpoint(&self, unix: bool, endpoint: &str) -> bool {
        if unix {
            let socket = if self.tcp.is_some() {
                self.unix
            } else {
                self.words.first().copied()
            };
            socket.is_some_and(|word| word.as_ref() == endpoint)
        } else {
            self.tcp
                .and_then(|word| word.as_ref().to_str())
                .is_some_and(|bind| crate::transport::dial_for_bind(bind) == endpoint)
        }
    }

    /// Pure serving syntax only. Network reachability, key bytes and the
    /// build's sealed transport remain the execution lane's checks. A bare
    /// argv runs the build's default ([`crate::durability::resolve`]).
    pub fn serving_durability(&self) -> Option<crate::durability::Durability> {
        self.named_durability().map(crate::durability::resolve)
    }

    /// What a recognised serving invocation NAMES: `Some(None)` for no
    /// `--durability` (the build's default), `Some(Some(tier))` for a named
    /// one, `None` for an argv that is not a recognised serving one.
    pub fn named_durability(&self) -> Option<Option<crate::durability::Durability>> {
        if self.repair {
            return None;
        }
        match self.tcp {
            None if self.key_file.is_none()
                && self.unix.is_none()
                && !self.allow_remote
                && matches!(self.words.len(), 1 | 2) => {}
            Some(_)
                if self.key_file.is_some()
                    && self.secret_file.is_some()
                    && self.words.len() == 1 => {}
            _ => return None,
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
mod tests {
    use super::*;

    #[test]
    fn execution_and_observation_share_the_option_value_boundaries() {
        let args = ["socket", "--secret-file", "--durability", "relaxed"];
        let parsed = BrokerArgs::parse(&args)
            .ok()
            .expect("accepted broker syntax");
        assert_eq!(parsed.secret_file.copied(), Some("--durability"));
        assert_eq!(parsed.words, vec![&"socket", &"relaxed"]);
        assert_eq!(
            parsed.serving_durability(),
            Some(crate::durability::Durability::Strict)
        );
        let tcp = [
            "--tcp",
            "127.0.0.1:7",
            "--key-file",
            "--durability",
            "--secret-file",
            "secret",
            "--unix",
            "--help",
            "log",
        ];
        let parsed = BrokerArgs::parse(&tcp)
            .ok()
            .expect("values are opaque words");
        assert_eq!(parsed.key_file.copied(), Some("--durability"));
        assert_eq!(parsed.unix.copied(), Some("--help"));
        assert_eq!(parsed.words, vec![&"log"]);
        assert_eq!(
            parsed.serving_durability(),
            Some(crate::durability::Durability::Strict)
        );
        assert!(matches!(
            BrokerArgs::parse(&["sock", "--secret-file"]),
            Err(ParseError::Usage(_))
        ));
        assert!(matches!(
            BrokerArgs::parse(&["sock", "--secret-file", "one", "--secret-file", "two"]),
            Err(ParseError::Usage(_))
        ));
        assert!(matches!(
            BrokerArgs::parse(&["sock", "--help"]),
            Err(ParseError::Help)
        ));
    }
}
