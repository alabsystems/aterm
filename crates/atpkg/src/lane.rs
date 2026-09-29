// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `atpkg lane` — the Rust-lane reader over ONE shell command line.
//!
//! [`aterm_types::rust_lane`] finds a stock Rust tool (`cargo`, `cargo +1.97.1`,
//! `rustc`, `rustfmt`, clippy, rustdoc) at COMMAND POSITION in a shell line — quote-,
//! heredoc-, comment- and case-pattern-aware, through `$(…)`, backticks, `bash -c`,
//! `eval`, `find -exec`, prefix words with their options and `rustup run` — and
//! renders the Trust spelling of the same command. The reroute stubs cannot use it:
//! they answer a NAME looked up on PATH and never see a command line. This verb is
//! where that reading is asked for.
//!
//! WHY A VERB, AND WHAT IT IS NOT. On 2026-09-23 the owner ruled that Rust on this
//! machine means the Trust toolchain everywhere, and an interim `PreToolUse` script in
//! `~/.claude/hooks` began refusing an agent's stock command; it missed shapes the
//! reader here gets right (the review of 2026-09-24 measured 13 of 42 probes). The
//! first home planned for the reader was an `aterm harness install` capability, but
//! decision "B" (2026-09-22, finished 2026-09-23) retired that installer: aterm
//! installs nothing into an agent. So this verb installs nothing, reads nothing but
//! its input, and knows no vendor: it takes a command line — argv, stdin, or one
//! string field of a JSON object on stdin (`--json tool_input.command`) — and answers
//! with an exit code and stderr. Whoever wants a guard wires it themselves.
//!
//! THE ANSWER. Exit 0 and silence: no stock Rust tool at command position. Exit 0 and
//! one stderr line: one stood there, cleared by `ATERM_STOCK_REASON='<why>'` in front
//! of it. Exit [`crate::reroute::REFUSAL_EXIT`] (2) with [`rust_lane::refusal`] on
//! stderr — the Trust spelling of the same command, the directory's stock pin, the
//! escape: one did. Exit 1: the input could not be read (not JSON, or not UTF-8), so
//! nothing is claimed either way.

use std::path::{Path, PathBuf};

use aterm_types::rust_lane;

use crate::reroute::Pin;

/// The most input one call reads. An agent's command line can be large (a heredoc
/// body), but a guard that buffers without bound is its own hazard.
pub const MAX_INPUT_BYTES: u64 = 8 * 1024 * 1024;

/// Where the command line comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// The words after `--`, joined by single spaces.
    Words(Vec<String>),
    /// All of stdin (`-`).
    Stdin,
    /// The string at this dotted path of the JSON object on stdin (`--json`).
    Json(String),
}

/// A parsed `atpkg lane` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    /// The directory whose pin a refusal names (`--cwd`); the process's otherwise.
    pub cwd: Option<PathBuf>,
    /// Where the command line comes from.
    pub source: Source,
}

/// Parse the argv after `lane`. `Err` is the usage error to print.
///
/// # Errors
/// An unknown option, a missing value, no source, or two sources.
pub fn parse_args(rest: &[String]) -> Result<Args, String> {
    let mut cwd = None;
    let mut source: Option<Source> = None;
    let set = |source: &mut Option<Source>, next: Source| -> Result<(), String> {
        if source.is_some() {
            return Err(String::from(
                "name the command line once: `-- <command…>`, `-`, or `--json <field>`",
            ));
        }
        *source = Some(next);
        Ok(())
    };
    let mut i = 0;
    while let Some(arg) = rest.get(i) {
        match arg.as_str() {
            "--" => {
                let words = rest[i + 1..].to_vec();
                if words.is_empty() {
                    return Err(String::from("`--` names no command"));
                }
                set(&mut source, Source::Words(words))?;
                break;
            }
            "-" => set(&mut source, Source::Stdin)?,
            "--cwd" | "--json" => {
                let Some(value) = rest.get(i + 1).filter(|v| !v.is_empty()) else {
                    return Err(format!("{arg} takes a value"));
                };
                if arg == "--cwd" {
                    cwd = Some(PathBuf::from(value));
                } else {
                    set(&mut source, Source::Json(value.clone()))?;
                }
                i += 1;
            }
            other => {
                if let Some(value) = other.strip_prefix("--cwd=").filter(|v| !v.is_empty()) {
                    cwd = Some(PathBuf::from(value));
                } else if let Some(value) = other.strip_prefix("--json=").filter(|v| !v.is_empty())
                {
                    set(&mut source, Source::Json(value.to_string()))?;
                } else {
                    return Err(format!("unknown argument {other:?}"));
                }
            }
        }
        i += 1;
    }
    let Some(source) = source else {
        return Err(String::from(
            "no command line: `-- <command…>`, `-` (stdin), or `--json <field>` (stdin)",
        ));
    };
    Ok(Args { cwd, source })
}

/// The command line `source` names, `input` being stdin's bytes (read only for the
/// stdin sources). `Ok(None)`: the JSON object has no string at that path — nothing to
/// read, so nothing to refuse.
///
/// # Errors
/// Input that is not UTF-8, or not JSON for [`Source::Json`].
pub fn command_line(source: &Source, input: &[u8]) -> Result<Option<String>, String> {
    match source {
        Source::Words(words) => Ok(Some(words.join(" "))),
        Source::Stdin => std::str::from_utf8(input)
            .map(|s| Some(s.to_string()))
            .map_err(|e| format!("stdin is not UTF-8: {e}")),
        Source::Json(path) => {
            let text =
                std::str::from_utf8(input).map_err(|e| format!("stdin is not UTF-8: {e}"))?;
            let doc: aterm_json::Value =
                aterm_json::from_str(text).map_err(|e| format!("stdin is not JSON: {e}"))?;
            let mut at = &doc;
            for key in path.split('.') {
                match at.get(key) {
                    Some(next) => at = next,
                    None => return Ok(None),
                }
            }
            Ok(at.as_str().map(str::to_string))
        }
    }
}

/// What the reader says of one command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// No stock Rust tool at command position.
    Clean,
    /// One, cleared by its `ATERM_STOCK_REASON`: the line to print.
    Escaped(String),
    /// One: the refusal to print.
    Refused(String),
}

/// The reader's answer for `command`, a refusal naming the stock pin of `cwd` (or
/// `$RUSTUP_TOOLCHAIN`, `env_toolchain`, which rustup ranks above a file).
///
/// The reader looks at the directory of a path-form command word
/// ([`rust_lane::verdict_in`], [`rust_lane::file_beside`]): a relative one is under
/// `cwd`, a leading `~/` or `$HOME/` is `home`. So a Trust toolchain's own `rustc` by
/// path — `build/<host>/stage2/bin/rustc -vV`, `~/toolchains/trust-…/bin/rustc`, the
/// compiler under measurement — is not refused, and its `cargo` is refused with that
/// directory's `targo`. With no `cwd`, only an absolute or home-relative directory
/// can be looked at.
#[must_use]
pub fn answer(
    command: &str,
    cwd: Option<&Path>,
    home: Option<&Path>,
    env_toolchain: Option<&str>,
) -> Answer {
    let beside = |dir: &str, name: &str| rust_lane::file_beside(dir, name, cwd, home);
    match rust_lane::verdict_in(command, &beside) {
        rust_lane::Verdict::Clean => Answer::Clean,
        rust_lane::Verdict::Escaped { hit, reason } => Answer::Escaped(format!(
            "aterm: {} `{}` let through — {}={reason:?}; say why in your reply.",
            if hit.toolchain_dir.is_some() {
                "compatibility"
            } else {
                "stock"
            },
            hit.used,
            rust_lane::STOCK_REASON
        )),
        rust_lane::Verdict::Stock(hit) => {
            let pin = Pin::for_dir(cwd, env_toolchain);
            let pin = pin
                .as_ref()
                .map(|p| (Path::new(p.source.as_str()), p.channel.as_str()));
            Answer::Refused(rust_lane::refusal(&hit, pin))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(list: &[&str]) -> Vec<String> {
        list.iter().map(|w| (*w).to_string()).collect()
    }

    #[test]
    fn the_command_line_is_named_exactly_once() {
        assert_eq!(
            parse_args(&s(&["--", "cargo", "+1.97.1", "test", "--help"])),
            Ok(Args {
                cwd: None,
                source: Source::Words(s(&["cargo", "+1.97.1", "test", "--help"])),
            }),
            "everything after `--` is the command, its own flags included"
        );
        assert_eq!(
            parse_args(&s(&["--cwd", "/r", "-"])),
            Ok(Args {
                cwd: Some(PathBuf::from("/r")),
                source: Source::Stdin,
            })
        );
        assert_eq!(
            parse_args(&s(&["--json=tool_input.command", "--cwd=/r"])),
            Ok(Args {
                cwd: Some(PathBuf::from("/r")),
                source: Source::Json("tool_input.command".into()),
            })
        );
        for bad in [
            &[][..],
            &["--"][..],
            &["--json"][..],
            &["--cwd"][..],
            &["-", "--json", "a"][..],
            &["--json", "a", "--", "cargo"][..],
            &["cargo", "build"][..],
            &["--verbose", "-"][..],
        ] {
            assert!(parse_args(&s(bad)).is_err(), "{bad:?}");
        }
    }

    /// The JSON source reads one string field by its dotted path — the shape a hook
    /// payload carries it in — and a payload without it is nothing to read, not an
    /// error; input that is not JSON is an error, never a refusal.
    #[test]
    fn a_json_field_is_read_by_its_dotted_path() {
        let payload = br#"{"session_id":"x","cwd":"/r","tool_name":"Bash","tool_input":{"command":"cargo +1.97.1 test -p x","description":"d"}}"#;
        let field = Source::Json("tool_input.command".into());
        assert_eq!(
            command_line(&field, payload),
            Ok(Some("cargo +1.97.1 test -p x".into()))
        );
        assert_eq!(
            command_line(&Source::Json("tool_input.file_path".into()), payload),
            Ok(None)
        );
        assert_eq!(
            command_line(&Source::Json("tool_input".into()), payload),
            Ok(None),
            "an object is not a command line"
        );
        assert!(command_line(&field, b"cargo build").is_err());
        assert!(command_line(&Source::Stdin, b"\xff\xfe").is_err());
        assert_eq!(
            command_line(&Source::Stdin, b"cargo build\n"),
            Ok(Some("cargo build\n".into()))
        );
    }

    /// The three answers: silence, one escaped line, a refusal with the Trust spelling
    /// and the directory's stock pin.
    #[test]
    fn the_answer_is_silence_an_escape_line_or_the_refusal() {
        assert_eq!(
            answer("targo --unverified test -p x", None, None, None),
            Answer::Clean
        );
        assert_eq!(
            answer("git commit -m 'cargo build'", None, None, None),
            Answer::Clean
        );
        let Answer::Escaped(line) = answer(
            "ATERM_STOCK_REASON='wasm32 cell' cargo +stable build --target wasm32-unknown-unknown",
            None,
            None,
            None,
        ) else {
            panic!("an escaped command is let through");
        };
        assert!(
            line.starts_with("aterm: stock `cargo +stable build` let through"),
            "{line}"
        );
        assert!(
            line.contains("ATERM_STOCK_REASON=\"wasm32 cell\""),
            "{line}"
        );
        let Answer::Refused(text) = answer("stdbuf -oL cargo +1.97.1 test -p x", None, None, None)
        else {
            panic!("a stock command is refused");
        };
        assert!(
            text.starts_with("`cargo +1.97.1 test` is stock Rust."),
            "{text}"
        );
        assert!(text.contains("targo trust test -p x"), "{text}");
        assert!(text.contains("targo --unverified test -p x"), "{text}");
        // The directory's stock pin, and `$RUSTUP_TOOLCHAIN` above it.
        let dir = std::env::temp_dir().join(format!("atpkg-lane-{}", std::process::id()));
        let sub = dir.join("src");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(
            dir.join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"1.97.1\"\n",
        )
        .unwrap();
        let Answer::Refused(pinned) = answer("cargo clippy", Some(&sub), None, None) else {
            panic!("refused");
        };
        assert!(
            pinned.contains("This directory pins stock \"1.97.1\""),
            "{pinned}"
        );
        assert!(pinned.contains("targo tippy"), "{pinned}");
        let Answer::Refused(env) = answer("cargo clippy", Some(&sub), None, Some("stable")) else {
            panic!("refused");
        };
        assert!(
            env.contains("pins stock \"stable\" ($RUSTUP_TOOLCHAIN)"),
            "{env}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
