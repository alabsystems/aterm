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

    /// Confirms that the first positional word is the broker's socket path.
    pub fn serves_endpoint(&self, endpoint: &str) -> bool {
        self.words
            .first()
            .is_some_and(|word| word.as_ref() == endpoint)
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
            Some(crate::durability::default_for(true))
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

    /// **A LIFELINE IS ONE MORE VALUED FLAG**, so a broker a test or tool armed
    /// (`--lifeline-fd 0` appended after its words) reads off the process table as
    /// the same serving invocation: its endpoint, its tier and its log. Its value is
    /// opaque like the others', and it is kept once. NEGATIVE CONTROL: the same argv
    /// with the flag's value missing is refused, never a serving argv.
    #[test]
    fn a_lifeline_is_a_valued_flag_and_the_serving_reads_hold() {
        let argv = [
            "aterm",
            "link",
            "broker",
            "/r/bus.sock",
            "/r/bus.log",
            "--secret-file",
            "/r/mint.secret",
            aterm_uds::lifeline::FLAG,
            "0",
        ];
        let parsed = BrokerArgs::from_invocation(&argv).expect("a serving invocation");
        assert_eq!(parsed.lifeline_fd.copied(), Some("0"));
        assert_eq!(parsed.lifeline_at, Some(4), "its place among the words");
        assert_eq!(parsed.words, vec![&"/r/bus.sock", &"/r/bus.log"]);
        assert!(parsed.serves_endpoint("/r/bus.sock"));
        assert_eq!(
            parsed.serving_durability(),
            Some(crate::durability::default_for(true))
        );
        assert_eq!(
            parsed.log_path(),
            Some(std::path::PathBuf::from("/r/bus.log"))
        );
        let words = ["sock", "--lifeline-fd", "--durability"];
        let opaque = BrokerArgs::parse(&words)
            .ok()
            .expect("its value is an opaque word");
        assert_eq!(opaque.lifeline_fd.copied(), Some("--durability"));
        assert!(opaque.durability.is_none());
        assert!(matches!(
            BrokerArgs::parse(&["sock", "--lifeline-fd", "0", "--lifeline-fd", "3"]),
            Err(ParseError::Usage(_))
        ));
        let cut = &argv[..argv.len() - 1];
        assert!(BrokerArgs::from_invocation(cut).is_none(), "no value");
    }

    /// **THE LOG IS READ AS THE BROKER VERB READS IT** — where astream's
    /// `<log>.health` is looked for: the second word, or the socket's
    /// `<socket>.log` without one, whatever flags sit between. NEGATIVE
    /// CONTROLS: a relative log (the broker's working directory's, not the
    /// reader's), a repair, and an argv the verb would refuse name no log it
    /// serves.
    #[test]
    fn the_log_is_the_one_the_broker_verb_serves() {
        let log = |args: &[&str]| BrokerArgs::parse(args).ok().and_then(|a| a.log_path());
        let p = std::path::PathBuf::from;
        assert_eq!(log(&["/r/bus.sock", "/r/bus.log"]), Some(p("/r/bus.log")));
        assert_eq!(log(&["/r/bus.sock"]), Some(p("/r/bus.sock.log")));
        assert_eq!(
            log(&[
                "/r/bus.sock",
                "--secret-file",
                "/r/mint.secret",
                "/r/bus.log",
                "--durability",
                "relaxed"
            ]),
            Some(p("/r/bus.log"))
        );
        assert_eq!(log(&["bus.sock", "bus.log"]), None, "relative");
        assert_eq!(
            log(&["--repair", "/r/bus.log"]),
            None,
            "a repair serves nothing"
        );
        assert_eq!(
            log(&["/r/a", "/r/b", "/r/c"]),
            None,
            "a surplus word is refused"
        );
        assert_eq!(
            log(&["/r/bus.sock", "--durability", "fast"]),
            None,
            "a tier the verb refuses"
        );
    }
}
