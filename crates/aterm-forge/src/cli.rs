// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Hand-rolled `std::env::args` parsing with a `USAGE` const and a typed
//! `ParseError` — the shape `aterm-verify`, `aterm-release` and
//! `aterm-ctl` all use. No third-party argument crate: a program whose entire
//! purpose is to shrink the dependency surface does not get to add one.

use std::path::PathBuf;

pub const USAGE: &str = "\
cargo forge — aterm's third-party surface: survey, notarize, ratchet.

USAGE
  targo --unverified forge survey [--cell NAME]... [--top N] [--json PATH]
        Emit the inventory: packages, LOC, unsafe tokens, build scripts,
        proc macros, duplicate versions, and dominator cost per package.
        Read-only. Needs no compiler. This is the answer to \"what are all
        the third-party dependencies of aterm?\", per target.

  targo --unverified forge blame <name>[@<version>] [--cell NAME]
        Why is this package here? Prints every path from the root to it and
        its dominator cost in each cell.

  targo --unverified forge budget [--update] [--allow-regress \"<reason>\"]
        Compare the live surface against tools/forge-budget.tsv. --update may
        only LOWER a ceiling; raising one needs a reason of >= 80 characters,
        which is written into the file's 4th column.

  targo --unverified forge attest
        Provenance and license obligations over vendor/: patch agreement,
        [workspace] stubs, .cargo_vcs_info.json, Cargo.toml.orig, licenses,
        NOTICE, Apache-only pristine diffs when available, and fork markers.
        The vendor/astream submodule is checked against its gitlink: the
        .gitmodules entry, a clean checkout at exactly the pinned commit, and
        Cargo metadata resolving its crates from there outside the workspace.

  targo --unverified forge check [--cell NAME]...
        THE GATE VERB. attest + patch-liveness + census cross-check, with no
        compilation and no network. Run by `verify forge`.
        What it does not prove: [OB-11] scores path forks forward only (each
        needs a reviewed row; the reverse is attest [OB-1] and the census scan
        set).

  targo --unverified forge serde-attrs [PATH]...
        Every #[serde(...)] key and form under crates/ (crates/aterm-serde,
        the implementation and its suite, left out), as TSV on stdout: the
        closed set the first-party derive accepts (AUDIT-third-party SP.5).
        PATHs, relative to the workspace root, list those trees instead.
        Read-only.

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
    SerdeAttrs {
        paths: Vec<PathBuf>,
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
            Self::NoVerb => "no verb given — try `targo --unverified forge survey`".into(),
            Self::UnknownVerb(v) => {
                format!(
                    "unknown verb `{v}` — expected survey, blame, budget, attest, check or serde-attrs"
                )
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
    let mut operands: Vec<String> = Vec::new();

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
            other => operands.push(other.to_string()),
        }
    }

    let cmd = match verb.as_str() {
        "survey" => Cmd::Survey { cells, top, json },
        "blame" => Cmd::Blame {
            pkg: operands
                .pop()
                .ok_or(ParseError::MissingOperand("a package name"))?,
            cells,
        },
        "budget" => Cmd::Budget {
            update,
            allow_regress,
        },
        "attest" => Cmd::Attest,
        "check" => Cmd::Check { cells },
        "serde-attrs" => Cmd::SerdeAttrs {
            paths: operands.into_iter().map(PathBuf::from).collect(),
        },
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
    fn serde_attrs_takes_any_number_of_paths() {
        assert_eq!(
            p(&["serde-attrs"]).unwrap().cmd,
            Cmd::SerdeAttrs { paths: vec![] }
        );
        assert_eq!(
            p(&["serde-attrs", "crates/aterm-serde", "crates/x"])
                .unwrap()
                .cmd,
            Cmd::SerdeAttrs {
                paths: vec!["crates/aterm-serde".into(), "crates/x".into()]
            }
        );
    }

    #[test]
    fn root_is_accepted_before_or_after_the_verb() {
        let a = p(&["--root", "/tmp/x", "survey"]).unwrap();
        let b = p(&["survey", "--root", "/tmp/x"]).unwrap();
        assert_eq!(a.root, b.root);
    }
}
