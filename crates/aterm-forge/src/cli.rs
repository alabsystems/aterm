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

  targo --unverified forge budget [--update [--allow-regress \"<reason>\"]]
        Compare the live surface (every cell, the lock, the patch table)
        against tools/forge-budget.tsv. --update lowers each ceiling to its
        live value, or seeds the file when it is absent; it never adds a row.
        Raising a ceiling takes --allow-regress and a one-line reason, which
        is written into the file's 4th column.

  targo --unverified forge attest
        Provenance and license obligations over the vendor/ forks and the
        upstream code absorbed under crates/ (a [patch.crates-io] target, or
        a member NOTICE lists): patch, vendor/ and Cargo.lock agreement,
        [workspace] stubs, .cargo_vcs_info.json and Cargo.toml.orig, retained
        licenses, NOTICE, Apache-2.0 §4(b) notices, fork markers, deny.toml's
        license list and .gitignore. The §4(b) notices are judged by a diff
        against the .crate crates.io published, which attest fetches, holds
        to the sha256 the crates.io index records and caches in this user's
        aterm cache folder; offline, with no cached copy, that fork is
        UNVERIFIED. vendor/astream must be a clean checkout
        of its gitlink, not behind the pin origin/main had at the merge-base
        unless a commit on the branch says `astream-pin-rollback: <reason>`.

  targo --unverified forge check [--cell NAME]...
        The `forge` check of `targo --unverified verify`, without compiling:
        attest, plus a review row for every vendored fork, every fork live in
        some cell (or the first cell's build closure) with no unpatched copy
        beside it, carved paths still gone, the budget ratchet (every cell,
        whatever --cell says), no patch redirecting a dev-dependency oracle,
        a wasm cell for every shipped browser root, vendor/forge.toml matching
        the forks, and deny.toml's bans and sources.

  targo --unverified forge serde-attrs [PATH]...
        Every #[serde(...)] key and form in use under crates/ (except
        crates/aterm-serde), as TSV on stdout; the first-party derive accepts
        only the set recorded in
        docs/measured/third-party-2026-10-15/serde-attrs.tsv. PATHs, relative
        to the workspace root, scan those trees instead. Read-only.

  targo --unverified forge abrew-help [--input SNAPSHOT] [--check|--write]
        Check captured rerouted Brew help (the default), or regenerate the
        declared Rust output. Reads an offline snapshot; never runs Brew.
        Only Trust's formatter is executed. Snapshot paths are inside the
        workspace so the generated header carries a portable rerun command.

  targo --unverified forge abrew-help import-legacy --input SEALED_F15 --output SNAPSHOT
        Bootstrap an independent capture from the exact sealed f15 artifact.
        Explicit migration only; refuses an existing snapshot destination.

OPTIONS
  --root PATH      Workspace root (default: the nearest at or above the CWD)
  --cell NAME      One cell, a target triple and its root package (mac-arm,
                   linux, win, ...); repeatable. Default: every cell.
  --top N          Rows per ranked survey table; 0 = all (default 40)
  -h, --help       This text

EXIT CODES
  0 pass   1 fail   2 usage (the command line did not parse)
  3 could not run (also an unknown --cell name or a malformed blame spec)
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
    AbrewHelp {
        options: aterm_forge::abrew_help::Options,
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
    /// One sentence; main prints USAGE after it.
    pub fn message(&self) -> String {
        match self {
            Self::NoVerb => "no verb given — try `targo --unverified forge survey`".into(),
            Self::UnknownVerb(v) => {
                format!(
                    "unknown verb `{v}` — expected survey, blame, budget, attest, check, serde-attrs or abrew-help"
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

    if verb == "abrew-help" {
        return parse_abrew(it, root);
    }

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

    let no_operands = |ops: Vec<String>| -> Result<(), ParseError> {
        if let Some(extra) = ops.into_iter().next() {
            return Err(ParseError::UnexpectedArg(extra));
        }
        Ok(())
    };
    let cmd = match verb.as_str() {
        "survey" => {
            no_operands(operands)?;
            Cmd::Survey { cells, top }
        }
        "blame" => {
            let mut ops = operands.into_iter();
            let pkg = ops
                .next()
                .ok_or(ParseError::MissingOperand("a package name"))?;
            if let Some(extra) = ops.next() {
                return Err(ParseError::UnexpectedArg(extra));
            }
            Cmd::Blame { pkg, cells }
        }
        "budget" => {
            no_operands(operands)?;
            Cmd::Budget {
                update,
                allow_regress,
            }
        }
        "attest" => {
            no_operands(operands)?;
            Cmd::Attest
        }
        "check" => {
            no_operands(operands)?;
            Cmd::Check { cells }
        }
        "serde-attrs" => Cmd::SerdeAttrs {
            paths: operands.into_iter().map(PathBuf::from).collect(),
        },
        other => return Err(ParseError::UnknownVerb(other.to_string())),
    };
    Ok(Invocation { root, cmd })
}

fn parse_abrew<I: Iterator<Item = String>>(
    mut it: I,
    mut root: Option<PathBuf>,
) -> Result<Invocation, ParseError> {
    use aterm_forge::abrew_help::{INPUT, Mode, Options};
    let mut input = None;
    let mut output = None;
    let mut import = false;
    let mut mode = None;
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                return Ok(Invocation {
                    root,
                    cmd: Cmd::Help,
                });
            }
            "--root" => root = Some(PathBuf::from(next(&mut it, "--root")?)),
            "--input" if input.is_none() => input = Some(PathBuf::from(next(&mut it, "--input")?)),
            "--output" if output.is_none() => {
                output = Some(PathBuf::from(next(&mut it, "--output")?))
            }
            "import-legacy" if !import => import = true,
            "--check" if mode.is_none() => mode = Some(Mode::Check),
            "--write" if mode.is_none() => mode = Some(Mode::Write),
            other => return Err(ParseError::UnexpectedArg(other.into())),
        }
    }
    let options = if import {
        if mode.is_some() {
            return Err(ParseError::UnexpectedArg(
                "check/write on legacy import".into(),
            ));
        }
        Options {
            input: input.ok_or(ParseError::MissingOperand("--input SEALED_F15"))?,
            mode: Mode::ImportLegacy {
                output: output.ok_or(ParseError::MissingOperand("--output SNAPSHOT"))?,
            },
        }
    } else {
        if output.is_some() {
            return Err(ParseError::UnexpectedArg(
                "--output without import-legacy".into(),
            ));
        }
        Options {
            input: input.unwrap_or_else(|| INPUT.into()),
            mode: mode.unwrap_or(Mode::Check),
        }
    };
    Ok(Invocation {
        root,
        cmd: Cmd::AbrewHelp { options },
    })
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
        assert_eq!(
            p(&["blame", "pkg1", "pkg2"]),
            Err(ParseError::UnexpectedArg("pkg2".into()))
        );
        for verb in ["survey", "budget", "attest", "check"] {
            assert_eq!(
                p(&[verb, "stray"]),
                Err(ParseError::UnexpectedArg("stray".into()))
            );
        }
    }

    #[test]
    fn abrew_help_defaults_to_check_and_import_is_an_explicit_separate_mode() {
        use aterm_forge::abrew_help::{INPUT, Mode, Options};
        assert_eq!(
            p(&["abrew-help"]).unwrap().cmd,
            Cmd::AbrewHelp {
                options: Options {
                    input: INPUT.into(),
                    mode: Mode::Check
                }
            }
        );
        assert_eq!(
            p(&["abrew-help", "--input", "data/new.json", "--write"])
                .unwrap()
                .cmd,
            Cmd::AbrewHelp {
                options: Options {
                    input: "data/new.json".into(),
                    mode: Mode::Write
                }
            }
        );
        assert_eq!(
            p(&[
                "abrew-help",
                "import-legacy",
                "--input",
                "/tmp/f15.rs",
                "--output",
                "data/capture.json"
            ])
            .unwrap()
            .cmd,
            Cmd::AbrewHelp {
                options: Options {
                    input: "/tmp/f15.rs".into(),
                    mode: Mode::ImportLegacy {
                        output: "data/capture.json".into()
                    }
                }
            }
        );
        for bad in [
            vec!["abrew-help", "--check", "--write"],
            vec!["abrew-help", "--output", "elsewhere"],
            vec!["abrew-help", "import-legacy", "--input", "capture.rs"],
            vec![
                "abrew-help",
                "import-legacy",
                "--input",
                "capture.rs",
                "--output",
                "capture.json",
                "--write",
            ],
        ] {
            assert!(p(&bad).is_err());
        }
    }
}
