// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Hand-rolled `std::env::args` parsing with a `USAGE` const and a typed
//! `ParseError` — the shape `xtask`, `aterm-verify`, `aterm-release` and
//! `aterm-ctl` all use. No third-party argument crate: a program whose entire
//! purpose is to shrink the dependency surface does not get to add one.

use std::path::PathBuf;

pub const USAGE: &str = "\
cargo forge — aterm's third-party surface: survey, notarize, ratchet.

USAGE
  cargo forge survey [--cell NAME]... [--top N] [--json PATH]
        Emit the inventory: packages, LOC, unsafe tokens, build scripts,
        proc macros, duplicate versions, and dominator cost per package.
        Read-only. Needs no compiler. This is the answer to \"what are all
        the third-party dependencies of aterm?\", per target.

  cargo forge blame <name>[@<version>] [--cell NAME]
        Why is this package here? Prints every path from the root to it and
        its dominator cost in each cell.

  cargo forge budget [--update] [--allow-regress \"<reason>\"]
        Compare the live surface against tools/forge-budget.tsv. --update may
        only LOWER a ceiling; raising one needs a reason of >= 80 characters,
        which is written into the file and reprinted on every run thereafter.

  cargo forge attest
        Provenance and license obligations over vendor/: upstream.lock
        integrity, [workspace] stub, .cargo_vcs_info.json, Cargo.toml.orig,
        retained LICENSE files, NOTICE agreement, Apache-2.0 §4(b)
        modification notices, and the // aterm-trust: marker floor.

  cargo forge check [--cell NAME]...
        THE GATE VERB. attest + patch-liveness + census cross-check, with no
        compilation and no network. Wired as `xtask gate forge`.

OPTIONS
  --root PATH      Workspace root (default: discovered from CWD)
  --cell NAME      Restrict to one cell; repeatable. Default: every cell.
  --top N          Rows in the ranked survey table (default 40)
  --json PATH      Also write the survey as JSON
  -h, --help       This text

EXIT CODES
  0 pass   1 policy failure   2 usage   3 could-not-run
";

#[derive(Debug, PartialEq, Eq)]
pub enum Cmd {
    Survey {
        cells: Vec<String>,
        top: usize,
        json: Option<PathBuf>,
    },
    Blame {
        pkg: String,
        cells: Vec<String>,
    },
    Budget {
        update: bool,
        allow_regress: Option<String>,
    },
    Attest,
    Check {
        cells: Vec<String>,
    },
    Help,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Invocation {
    pub root: Option<PathBuf>,
    pub cmd: Cmd,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    NoVerb,
    UnknownVerb(String),
    MissingValue(&'static str),
    BadNumber(String),
    UnexpectedArg(String),
    MissingOperand(&'static str),
}

impl ParseError {
    /// One sentence, naming the fix. A refusal that does not say what to type
    /// instead is a bug report addressed to nobody.
    pub fn message(&self) -> String {
        match self {
            Self::NoVerb => "no verb given — try `cargo forge survey`".into(),
            Self::UnknownVerb(v) => {
                format!("unknown verb `{v}` — expected survey, blame, budget, attest or check")
            }
            Self::MissingValue(flag) => format!("`{flag}` needs a value"),
            Self::BadNumber(s) => format!("`{s}` is not a number"),
            Self::UnexpectedArg(a) => format!("unexpected argument `{a}`"),
            Self::MissingOperand(what) => format!("missing {what}"),
        }
    }
}

pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Invocation, ParseError> {
    let mut it = args.into_iter().peekable();
    let mut root: Option<PathBuf> = None;
    let mut cells: Vec<String> = Vec::new();
    let mut top: usize = 40;
    let mut json: Option<PathBuf> = None;
    let mut update = false;
    let mut allow_regress: Option<String> = None;
    let mut operand: Option<String> = None;

    let verb = loop {
        let Some(a) = it.next() else {
            return Err(ParseError::NoVerb);
        };
        match a.as_str() {
            "-h" | "--help" | "help" => {
                return Ok(Invocation {
                    root,
                    cmd: Cmd::Help,
                });
            }
            "--root" => root = Some(PathBuf::from(next(&mut it, "--root")?)),
            other if other.starts_with('-') => return Err(ParseError::UnexpectedArg(other.into())),
            other => break other.to_string(),
        }
    };

    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => {
                return Ok(Invocation {
                    root,
                    cmd: Cmd::Help,
                });
            }
            "--root" => root = Some(PathBuf::from(next(&mut it, "--root")?)),
            "--cell" => cells.push(next(&mut it, "--cell")?),
            "--top" => {
                let v = next(&mut it, "--top")?;
                top = v.parse().map_err(|_| ParseError::BadNumber(v))?;
            }
            "--json" => json = Some(PathBuf::from(next(&mut it, "--json")?)),
            "--update" => update = true,
            "--allow-regress" => allow_regress = Some(next(&mut it, "--allow-regress")?),
            other if other.starts_with('-') => return Err(ParseError::UnexpectedArg(other.into())),
            other => operand = Some(other.to_string()),
        }
    }

    let cmd = match verb.as_str() {
        "survey" => Cmd::Survey { cells, top, json },
        "blame" => Cmd::Blame {
            pkg: operand.ok_or(ParseError::MissingOperand("a package name"))?,
            cells,
        },
        "budget" => Cmd::Budget {
            update,
            allow_regress,
        },
        "attest" => Cmd::Attest,
        "check" => Cmd::Check { cells },
        other => return Err(ParseError::UnknownVerb(other.to_string())),
    };
    Ok(Invocation { root, cmd })
}

fn next<I: Iterator<Item = String>>(it: &mut I, flag: &'static str) -> Result<String, ParseError> {
    it.next().ok_or(ParseError::MissingValue(flag))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &[&str]) -> Result<Invocation, ParseError> {
        parse(s.iter().map(|x| x.to_string()))
    }

    #[test]
    fn survey_defaults_to_every_cell_and_forty_rows() {
        let inv = p(&["survey"]).unwrap();
        assert_eq!(
            inv.cmd,
            Cmd::Survey {
                cells: vec![],
                top: 40,
                json: None
            }
        );
    }

    #[test]
    fn cell_flag_repeats() {
        let inv = p(&["survey", "--cell", "mac-arm", "--cell", "linux"]).unwrap();
        let Cmd::Survey { cells, .. } = inv.cmd else {
            panic!("wrong verb")
        };
        assert_eq!(cells, vec!["mac-arm", "linux"]);
    }

    #[test]
    fn blame_requires_a_package() {
        assert_eq!(
            p(&["blame"]).unwrap_err(),
            ParseError::MissingOperand("a package name")
        );
    }

    #[test]
    fn unknown_verb_names_the_alternatives() {
        let e = p(&["carve"]).unwrap_err();
        assert!(e.message().contains("survey"), "{}", e.message());
    }

    #[test]
    fn a_flag_missing_its_value_is_a_typed_error_not_a_panic() {
        assert_eq!(
            p(&["survey", "--top"]).unwrap_err(),
            ParseError::MissingValue("--top")
        );
    }

    #[test]
    fn root_is_accepted_before_or_after_the_verb() {
        let a = p(&["--root", "/tmp/x", "survey"]).unwrap();
        let b = p(&["survey", "--root", "/tmp/x"]).unwrap();
        assert_eq!(a.root, b.root);
    }
}
