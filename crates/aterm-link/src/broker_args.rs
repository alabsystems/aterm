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
            secret_file: None,
            durability: None,
            repair: false,
            words: Vec::new(),
        };
        let mut i = 0;
        while let Some(arg) = args.get(i) {
            let word = arg.as_ref();
            let slot = match word.to_str() {
                Some("-h" | "--help") => return Err(ParseError::Help),
                Some("--secret-file") => Some((&mut out.secret_file, "--secret-file")),
                Some("--durability") => Some((&mut out.durability, "--durability")),
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
    /// boundaries before attributing a tier to this endpoint: the broker's
    /// socket is its first word.
    pub fn serves_endpoint(&self, endpoint: &str) -> bool {
        self.words
            .first()
            .is_some_and(|word| word.as_ref() == endpoint)
    }

    /// Pure serving syntax only; the secret's bytes remain the execution
    /// lane's check. A bare argv runs the build's default
    /// ([`crate::durability::resolve`]).
    pub fn serving_durability(&self) -> Option<crate::durability::Durability> {
        self.named_durability().map(crate::durability::resolve)
    }

    /// What a recognised serving invocation NAMES: `Some(None)` for no
    /// `--durability` (the build's default), `Some(Some(tier))` for a named
    /// one, `None` for an argv that is not a recognised serving one.
    pub fn named_durability(&self) -> Option<Option<crate::durability::Durability>> {
        if self.repair || !matches!(self.words.len(), 1 | 2) {
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
        assert!(parsed.serves_endpoint("socket"));
        assert!(!parsed.serves_endpoint("relaxed"));
        let opaque = [
            "--secret-file",
            "--help",
            "--durability",
            "--repair",
            "sock",
        ];
        let parsed = BrokerArgs::parse(&opaque)
            .ok()
            .expect("values are opaque words");
        assert_eq!(parsed.secret_file.copied(), Some("--help"));
        assert_eq!(parsed.durability.copied(), Some("--repair"));
        assert_eq!(parsed.words, vec![&"sock"]);
        assert!(!parsed.repair);
        // A retired flag (the sealed wire's `--tcp`) is an unknown one.
        assert!(matches!(
            BrokerArgs::parse(&["--tcp", "127.0.0.1:7", "log"]),
            Err(ParseError::Usage(_))
        ));
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
