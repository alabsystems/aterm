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

  cargo forge mirror emit --out DIR
        The Lane 1 generator: walk Cargo.lock's registry-sourced entries,
        verify each cached .crate against the lock checksum (refusing by name
        on mismatch), and emit a cargo `local-registry` — index/ JSON rows
        plus .crate copies. Missing cache files come back as a fetch list and
        a RED exit, never a silent skip.

  cargo forge mirror verify --dir DIR
        Re-hash every .crate in an emitted mirror against its index row AND
        Cargo.lock, both directions (missing + stray). Any drift is named and
        the exit is nonzero. Also judges each row's CONTENT, which no checksum
        covers: byte-for-byte against cargo's own sparse-index cache where one
        exists, and against Cargo.lock's resolved dependency edges everywhere.
        The verdict prints how many rows it could anchor and says plainly what
        an unanchored row does not prove.

  cargo forge mirror bundle --dir DIR --out FILE
        Pack a VERIFIED mirror into one deterministic, uncompressed bundle: a
        manifest holding every package name/version/cksum, each entry's
        sha256, the payload digest and the lock digest it was emitted from,
        then the bytes. Byte-identical on a second run from the same input.
        Refuses to bundle a mirror that does not verify. Never signs.

  cargo forge mirror check-bundle --file FILE
        Verify a bundle WITHOUT unpacking: header, manifest digest, structural
        rules, payload digest, every entry digest, every package cksum, every
        index row, and (when run in a workspace) whether it was built for THIS
        Cargo.lock. Proves INTEGRITY and SHAPE, not PROVENANCE: every digest a
        bundle carries is inside it, so an attacker who edits it re-seals them.
        Only a signature over the printed bundle-sha256 closes that, and
        signing is the owner's ceremony.

  cargo forge mirror unbundle --file FILE --out DIR [--force]
        check-bundle, then extract — re-hashing every entry as it is written
        and stat'ing every path afterwards, so the count it reports is the
        filesystem's. Judges row CONTENT with the same two anchors check-bundle
        uses and PRINTS how far each got: a bundle for a different lock is
        still extractable, but the edges THIS lock resolved are required of
        every row they cover, because on a delivery target they are the only
        anchor left. NOTHING is created until the bundle verifies AND the
        output tree is judged: a bundle may only name mirror paths, and --out
        must be absent or empty unless --force is given. A filesystem that
        fails part-way can still leave a partial tree; the failure says how
        many files landed.

  cargo forge mirror config [--write]
        Print the shippable `[source]` fragment for this lock; --write puts it
        at tools/cargo-mirror-config.toml. Flips NO default: cargo does not
        read that path. `cargo forge check` [OB-16] fails if the file and
        Cargo.lock disagree about what is mirrored.

OPTIONS
  --root PATH      Workspace root (default: discovered from CWD)
  --cell NAME      Restrict to one cell; repeatable. Default: every cell.
  --top N          Rows in the ranked survey table (default 40)
  --json PATH      Also write the survey as JSON
  --file PATH      The bundle file (mirror check-bundle / unbundle)
  --write          mirror config: write the fragment instead of printing it
  --force          mirror unbundle: extract into a directory that is not empty
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
    MirrorEmit {
        out: PathBuf,
    },
    MirrorVerify {
        dir: PathBuf,
    },
    MirrorBundle {
        dir: PathBuf,
        out: PathBuf,
    },
    MirrorUnbundle {
        file: PathBuf,
        out: PathBuf,
        force: bool,
    },
    MirrorCheckBundle {
        file: PathBuf,
    },
    MirrorConfig {
        write: bool,
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
    UnknownMirrorAction(String),
}

impl ParseError {
    /// One sentence, naming the fix. A refusal that does not say what to type
    /// instead is a bug report addressed to nobody.
    pub fn message(&self) -> String {
        match self {
            Self::NoVerb => "no verb given — try `cargo forge survey`".into(),
            Self::UnknownVerb(v) => {
                format!(
                    "unknown verb `{v}` — expected survey, blame, budget, attest, check or mirror"
                )
            }
            Self::UnknownMirrorAction(a) => format!(
                "unknown mirror action `{a}` — expected emit, verify, bundle, unbundle, \
                 check-bundle or config"
            ),
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
    let mut out: Option<PathBuf> = None;
    let mut dir: Option<PathBuf> = None;
    let mut file: Option<PathBuf> = None;
    let mut write = false;
    let mut force = false;
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
            "--out" => out = Some(PathBuf::from(next(&mut it, "--out")?)),
            "--dir" => dir = Some(PathBuf::from(next(&mut it, "--dir")?)),
            "--file" => file = Some(PathBuf::from(next(&mut it, "--file")?)),
            "--write" => write = true,
            "--force" => force = true,
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
        "mirror" => match operand.as_deref() {
            Some("emit") => Cmd::MirrorEmit {
                out: out.ok_or(ParseError::MissingOperand(
                    "`--out DIR` (where mirror emit writes the local registry)",
                ))?,
            },
            Some("verify") => Cmd::MirrorVerify {
                dir: dir.ok_or(ParseError::MissingOperand(
                    "`--dir DIR` (the emitted local registry to re-hash)",
                ))?,
            },
            Some("bundle") => Cmd::MirrorBundle {
                dir: dir.ok_or(ParseError::MissingOperand(
                    "`--dir DIR` (the emitted local registry to pack)",
                ))?,
                out: out.ok_or(ParseError::MissingOperand(
                    "`--out FILE` (where mirror bundle writes the bundle)",
                ))?,
            },
            Some("unbundle") => Cmd::MirrorUnbundle {
                file: file.ok_or(ParseError::MissingOperand(
                    "`--file FILE` (the bundle to verify and extract)",
                ))?,
                out: out.ok_or(ParseError::MissingOperand(
                    "`--out DIR` (where mirror unbundle extracts the mirror)",
                ))?,
                force,
            },
            Some("check-bundle") => Cmd::MirrorCheckBundle {
                file: file.ok_or(ParseError::MissingOperand(
                    "`--file FILE` (the bundle to verify without unpacking)",
                ))?,
            },
            Some("config") => Cmd::MirrorConfig { write },
            Some(a) => return Err(ParseError::UnknownMirrorAction(a.to_string())),
            None => {
                return Err(ParseError::MissingOperand(
                    "a mirror action: emit --out DIR, verify --dir DIR, \
                     bundle --dir DIR --out FILE, unbundle --file FILE --out DIR, \
                     check-bundle --file FILE, or config [--write]",
                ));
            }
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
    fn mirror_emit_takes_out_and_verify_takes_dir() {
        let a = p(&["mirror", "emit", "--out", "/tmp/m"]).unwrap();
        assert_eq!(
            a.cmd,
            Cmd::MirrorEmit {
                out: PathBuf::from("/tmp/m")
            }
        );
        let b = p(&["mirror", "verify", "--dir", "/tmp/m"]).unwrap();
        assert_eq!(
            b.cmd,
            Cmd::MirrorVerify {
                dir: PathBuf::from("/tmp/m")
            }
        );
    }

    #[test]
    fn mirror_without_an_action_or_flag_names_what_to_type() {
        let e = p(&["mirror"]).unwrap_err();
        assert!(e.message().contains("emit --out DIR"), "{}", e.message());
        let e = p(&["mirror", "emit"]).unwrap_err();
        assert!(e.message().contains("--out DIR"), "{}", e.message());
        let e = p(&["mirror", "shred"]).unwrap_err();
        assert!(e.message().contains("`shred`"), "{}", e.message());
    }

    #[test]
    fn the_delivery_verbs_take_their_own_operands() {
        assert_eq!(
            p(&["mirror", "bundle", "--dir", "/tmp/m", "--out", "/tmp/b"])
                .unwrap()
                .cmd,
            Cmd::MirrorBundle {
                dir: PathBuf::from("/tmp/m"),
                out: PathBuf::from("/tmp/b")
            }
        );
        assert_eq!(
            p(&["mirror", "unbundle", "--file", "/tmp/b", "--out", "/tmp/m"])
                .unwrap()
                .cmd,
            Cmd::MirrorUnbundle {
                file: PathBuf::from("/tmp/b"),
                out: PathBuf::from("/tmp/m"),
                force: false
            }
        );
        // The escape hatch is OFF unless it is typed, and it is the only way
        // to extract into a populated tree.
        assert_eq!(
            p(&[
                "mirror", "unbundle", "--file", "/tmp/b", "--out", "/tmp/m", "--force"
            ])
            .unwrap()
            .cmd,
            Cmd::MirrorUnbundle {
                file: PathBuf::from("/tmp/b"),
                out: PathBuf::from("/tmp/m"),
                force: true
            }
        );
        assert_eq!(
            p(&["mirror", "check-bundle", "--file", "/tmp/b"])
                .unwrap()
                .cmd,
            Cmd::MirrorCheckBundle {
                file: PathBuf::from("/tmp/b")
            }
        );
        assert_eq!(
            p(&["mirror", "config"]).unwrap().cmd,
            Cmd::MirrorConfig { write: false }
        );
        assert_eq!(
            p(&["mirror", "config", "--write"]).unwrap().cmd,
            Cmd::MirrorConfig { write: true }
        );
    }

    /// `bundle` needs BOTH operands and says which one is missing — the shape
    /// that costs a run when the message only says "missing operand".
    #[test]
    fn each_delivery_verb_names_the_operand_it_lacks() {
        for (args, want) in [
            (vec!["mirror", "bundle", "--out", "/tmp/b"], "--dir DIR"),
            (vec!["mirror", "bundle", "--dir", "/tmp/m"], "--out FILE"),
            (vec!["mirror", "unbundle", "--out", "/tmp/m"], "--file FILE"),
            (vec!["mirror", "unbundle", "--file", "/tmp/b"], "--out DIR"),
            (vec!["mirror", "check-bundle"], "--file FILE"),
        ] {
            let e = p(&args).unwrap_err();
            assert!(e.message().contains(want), "{args:?}: {}", e.message());
        }
    }

    #[test]
    fn root_is_accepted_before_or_after_the_verb() {
        let a = p(&["--root", "/tmp/x", "survey"]).unwrap();
        let b = p(&["survey", "--root", "/tmp/x"]).unwrap();
        assert_eq!(a.root, b.root);
    }
}
