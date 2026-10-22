// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Hand-rolled `std::env::args` parsing with a `USAGE` const and a typed `ParseError`. No
//! third-party argument crate: the tool that shrinks the dependency surface does not add to it.

use std::path::PathBuf;

pub const USAGE: &str = "\
targo --unverified forge — aterm's third-party dependency surface.

USAGE
  targo --unverified forge survey [--cell NAME]... [--top N]
        The inventory per cell: packages, LOC, unsafe tokens, build scripts,
        proc macros, duplicate versions, and packages ranked by dominator
        cost (what removing one would actually remove). Read-only.

  targo --unverified forge blame <name>[@<version>] [--cell NAME]...
        Why a package is here: its shortest paths from the cell root, its
        direct dependants, its dominator cost per cell, and whether a
        [patch.crates-io] fork reaches that version.

  targo --unverified forge budget [--update] [--allow-regress \"<reason>\"]
        Compare the live surface against tools/forge-budget.tsv. --update may
        only LOWER a ceiling; raising one needs a one-line reason, which is
        written into the file's 4th column.

  targo --unverified forge attest
        Provenance and license obligations over the [patch.crates-io] targets
        (vendor/ forks, and upstream code absorbed under crates/): patch,
        vendor/ and Cargo.lock agreement, [workspace] stubs,
        .cargo_vcs_info.json and Cargo.toml.orig, retained licenses, NOTICE,
        Apache-2.0 §4(b) notices, fork markers, deny.toml's license list and
        .gitignore. vendor/astream must be a clean checkout of its gitlink,
        not behind origin/main's pin.

  targo --unverified forge check [--cell NAME]...
        The `forge` check of `targo --unverified verify`, without compiling:
        attest, plus a review row for every vendored fork, every fork live in
        each cell, carved paths still gone, the budget ratchet, no patch
        redirecting a dev-dependency oracle, vendor/forge.toml matching the
        forks, and deny.toml's bans and sources.

  targo --unverified forge serde-attrs [PATH]...
        Every #[serde(...)] key and form under crates/ (except
        crates/aterm-serde), as TSV on stdout: the closed set the first-party
        derive accepts. PATHs, relative to the workspace root, scan those
        trees instead. Read-only.

OPTIONS
  --root PATH      Workspace root (default: the nearest one above the CWD)
  --cell NAME      One cell, a target triple and its root package (mac-arm,
                   linux, win, ...); repeatable. Default: every cell.
  --top N          Rows per ranked survey table; 0 = all (default 40)
  -h, --help       This text

EXIT CODES
  0 pass   1 fail   2 usage   3 could not run
";

#[derive(Debug, PartialEq, Eq)]
pub enum Cmd {
    Survey {
        cells: Vec<String>,
        top: usize,
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
    /// One sentence, naming the fix.
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
            "--update" => update = true,
            "--allow-regress" => allow_regress = Some(next(&mut it, "--allow-regress")?),
            other if other.starts_with('-') => return Err(ParseError::UnexpectedArg(other.into())),
            other => operands.push(other.to_string()),
        }
    }

    let cmd = match verb.as_str() {
        "survey" => Cmd::Survey { cells, top },
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
}
