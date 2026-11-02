// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `agh policy check` (design §6.3): agh.toml against L1–L11, every fault at once, and with no
//! agh.toml, the vault's adopted rows (design §6.10); and M2's `policy grant` and
//! `policy revoke`, the `[[writers]]` rows.

use std::path::{Path, PathBuf};

use aterm_json::{Map, Value};

use super::args::Invocation;
use super::verbs::Noun;
use super::{Answer, Ctx, Outcome, Receipt};
use crate::config::{self, AghConfig, CONFIG_FILE, ConfigFault, Counts};
use crate::error::{Error, Kind, Result};
use crate::identity::tilde;
use crate::identity::vault::{self, Found};
use crate::redact::Redactor;
use crate::slug::{RepoArg, Slug};

/// `agh policy check [--config PATH]`. Its report's `adopted` lists the vault's tokens when
/// agh.toml is absent or says `vault = true` and no `--config` was given (else it is empty),
/// and their rows' faults, and a `file` fault for each vault token that is unusable, join
/// `faults`.
/// # Errors
/// Usage for a `--config` path with no file name (such as `/` or `..`), or a `~/` one with no
/// home directory; CouldNotRun when the platform names no config directory and no `--config`
/// was given.
pub(crate) fn policy_check(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let home = ctx
        .dirs
        .as_ref()
        .map_or_else(PathBuf::new, |dirs| dirs.home.clone());
    let (dir, name) = match inv.flag("--config") {
        Some(text) => {
            let path = match text.strip_prefix("~/") {
                Some(_) if home.as_os_str().is_empty() => {
                    return Err(Error::usage(format!(
                        "--config {text}: no home directory to expand ~ against"
                    )));
                }
                Some(_) => config::expand(text, &home).unwrap_or_else(|| PathBuf::from(text)),
                None => PathBuf::from(text),
            };
            let Some(name) = path.file_name().and_then(std::ffi::OsStr::to_str) else {
                return Err(Error::usage(format!("--config names no file: {text}")));
            };
            let dir = path
                .parent()
                .filter(|dir| !dir.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            (relative_shown(dir), name.to_string())
        }
        None => match &ctx.dirs {
            Some(dirs) => (dirs.config.clone(), CONFIG_FILE.to_string()),
            None => {
                return Err(Error::could_not_run(
                    "no config directory: the platform names none (pass --config PATH)",
                ));
            }
        },
    };
    let agh_dir = ctx.dirs.as_ref().map(|dirs| dirs.config.as_path());
    let Judged {
        present,
        counts,
        mut faults,
        rows,
    } = judge(&dir, &name, &home, agh_dir);
    let adoption = match (&ctx.dirs, inv.flag("--config")) {
        (Some(dirs), None) => vault::view(dirs, &*ctx.env),
        _ => None,
    };
    let adopted = adoption
        .as_ref()
        .map(|adoption| {
            faults.extend_from_slice(adoption.faults());
            faults.extend(unusable_faults(adoption, &home));
            adopted_rows(adoption, &home, &ctx.redactor)
        })
        .unwrap_or_default();
    let mut value = report(
        &tilde(&dir.join(&name), &home),
        counts,
        &faults,
        present,
        &ctx.redactor,
    );
    if let Some(object) = value.as_object_mut() {
        let text = |s: &str| Value::from(ctx.redactor.apply(&config::masked(s)));
        let mut loose: Vec<&str> = rows
            .as_ref()
            .map(|rows| {
                rows.profiles
                    .iter()
                    .filter(|(_, row)| row.legacy_loose)
                    .map(|(name, _)| name.as_str())
                    .collect()
            })
            .unwrap_or_default();
        loose.sort_unstable();
        object.insert(
            "legacy_loose".into(),
            Value::Array(loose.into_iter().map(text).collect()),
        );
        let writers = rows
            .as_ref()
            .map(|rows| {
                rows.writers
                    .iter()
                    .map(|row| writer_row(&row.caller, &row.repos, &ctx.redactor))
                    .collect()
            })
            .unwrap_or_default();
        object.insert("writers".into(), Value::Array(writers));
    }
    if let Some(object) = value.as_object_mut() {
        object.insert("adopted".into(), Value::Array(adopted));
    }
    let answer = if faults.is_empty() {
        Answer::Object {
            noun: Noun::Policy,
            value,
        }
    } else {
        let what = if present {
            "agh.toml breaks"
        } else {
            "the vault's adopted rows break"
        };
        let next = if present {
            None
        } else {
            adoption.as_ref().and_then(vault::Adoption::next_step)
        };
        let error = Error::new(Kind::Invalid, format!("{what} {} rule(s)", faults.len()))
            .with_next(next.unwrap_or_else(|| "fix the rows named in faults".to_string()));
        Answer::FailedWith { value, error }
    };
    Ok(Outcome {
        answer,
        receipt: Receipt::new(&inv.verb_name()),
    })
}

/// A `file` fault for each vault token adoption cannot use because its file fails the
/// private-file or token rules, with its remedy: a token that would route is never quietly
/// left out (design §6.2).
fn unusable_faults(adoption: &vault::Adoption, home: &Path) -> Vec<ConfigFault> {
    adoption
        .vaulted()
        .iter()
        .filter_map(|vaulted| match &vaulted.found {
            Found::Unusable { slug, why, remedy } => Some(ConfigFault {
                rule: "file",
                what: format!(
                    "vault token {} ({}) is not used: {slug}: {why}{}",
                    vaulted.slot.profile(),
                    tilde(&vaulted.token_file, home),
                    remedy
                        .as_deref()
                        .map_or_else(String::new, |r| format!("; remedy: {r}"))
                ),
            }),
            _ => None,
        })
        .collect()
}

/// One row per vault token found with no agh.toml (design §6.10), as the record and the files
/// stand (`vault::view`: nothing is asked of GitHub): its profile, tier, file, the owner and
/// login it routes as once adopted, and its state, `adopted`, `pending` (its login is learned
/// on the next call that routes), `rotated` (the file holds another token than the one
/// recorded: the next call that routes learns its login again, and refuses it if the login
/// changed), `unusable` (with why and the remedy), `faulted` (its rows break a rule beside the
/// others, with the step that fixes the cause) or `changed`.
fn adopted_rows(adoption: &vault::Adoption, home: &Path, redactor: &Redactor) -> Vec<Value> {
    let text = |s: &str| Value::from(redactor.apply(s));
    adoption
        .vaulted()
        .iter()
        .filter_map(|vaulted| {
            let (state, login, why, remedy) = match &vaulted.found {
                Found::Absent => return None,
                Found::Adopted { login, .. } => ("adopted", Some(login.as_str()), None, None),
                Found::Unlearned { why, rotated, .. } => (
                    if *rotated { "rotated" } else { "pending" },
                    None,
                    Some(why.clone()),
                    vaulted.next(),
                ),
                Found::Unusable { slug, why, remedy } => (
                    "unusable",
                    None,
                    Some(format!("{slug}: {why}")),
                    remedy.clone(),
                ),
                Found::Faulted {
                    login, why, next, ..
                } => (
                    "faulted",
                    login.as_deref(),
                    Some(why.clone()),
                    Some(next.clone()),
                ),
                Found::Changed { adopted, now, .. } => (
                    "changed",
                    Some(adopted.as_str()),
                    Some(format!("adopted as {adopted}, GitHub now says {now}")),
                    None,
                ),
            };
            let owner = match (vaulted.slot, login) {
                (vault::Slot::Release, _) => Some(aterm_types::channel::PUBLIC_OWNER),
                (vault::Slot::Dev, login) => login,
            };
            let mut row = Map::new();
            row.insert("login".into(), login.map_or(Value::Null, text));
            row.insert("owner".into(), owner.map_or(Value::Null, text));
            row.insert("profile".into(), Value::from(vaulted.slot.profile()));
            row.insert("remedy".into(), remedy.as_deref().map_or(Value::Null, text));
            row.insert("state".into(), Value::from(state));
            row.insert("tier".into(), Value::from(vaulted.slot.tier().slug()));
            row.insert("token_file".into(), text(&tilde(&vaulted.token_file, home)));
            row.insert("why".into(), why.as_deref().map_or(Value::Null, text));
            Some(Value::Object(row))
        })
        .collect()
}

/// `dir`, with a relative one that starts with neither `.` nor `..` written `./{dir}`, so that
/// the report's `config` never reads as a `~/` home path or an absolute one.
fn relative_shown(dir: &Path) -> PathBuf {
    use std::path::Component;
    match dir.components().next() {
        Some(Component::Normal(_)) => Path::new(".").join(dir),
        _ => dir.to_path_buf(),
    }
}

/// One `[[writers]]` row as `policy check`, `policy grant` and `policy revoke` print it:
/// `{"caller","repos":[…]}`, each string an agh.toml value: masked as a valid row's values are
/// (`config::masked`, so a legacy token pasted into the row is never printed), then through
/// `redactor`.
fn writer_row(caller: &str, repos: &[String], redactor: &Redactor) -> Value {
    let shown = |s: &str| Value::from(redactor.apply(&config::masked(s)));
    let mut row = Map::new();
    row.insert("caller".into(), shown(caller));
    row.insert(
        "repos".into(),
        Value::Array(repos.iter().map(|repo| shown(repo)).collect()),
    );
    Value::Object(row)
}

/// `{"config","counts":{"owners","profiles","targets","writers"},"faults":[{"rule","what"}],
/// "ok","present"}`, to which `policy_check` adds `adopted`, `legacy_loose` (the profile rows
/// that set it, sorted) and `writers` (the `[[writers]]` rows in file order). The path and each
/// `what` pass `redactor`, as the error lines do (design §6.6):
/// stdout gets no string the Redactor did not see.
fn report(
    shown: &str,
    counts: Counts,
    faults: &[ConfigFault],
    present: bool,
    redactor: &Redactor,
) -> Value {
    let count = |n: usize| Value::from(u64::try_from(n).unwrap_or(u64::MAX));
    let mut counted = Map::new();
    counted.insert("owners".into(), count(counts.owners));
    counted.insert("profiles".into(), count(counts.profiles));
    counted.insert("targets".into(), count(counts.targets));
    counted.insert("writers".into(), count(counts.writers));
    let listed = faults
        .iter()
        .map(|fault| {
            let mut item = Map::new();
            item.insert("rule".into(), Value::from(fault.rule));
            item.insert("what".into(), Value::from(redactor.apply(&fault.what)));
            Value::Object(item)
        })
        .collect();
    let mut object = Map::new();
    object.insert("config".into(), Value::from(redactor.apply(shown)));
    object.insert("counts".into(), Value::Object(counted));
    object.insert("faults".into(), Value::Array(listed));
    object.insert("ok".into(), Value::from(faults.is_empty()));
    object.insert("present".into(), Value::from(present));
    Value::Object(object)
}

/// What reading and judging the file found.
struct Judged {
    /// Whether the file is there.
    present: bool,
    /// Its row counts.
    counts: Counts,
    /// Its faults.
    faults: Vec<ConfigFault>,
    /// Its rows as written, when it parsed.
    rows: Option<AghConfig>,
}

/// What reading and judging the file found: whether it is there, its row counts, its faults
/// and its rows. No file is a valid state (`present: false`), as it is for `config::load`.
#[cfg(unix)]
fn judge(dir: &Path, name: &str, home: &Path, agh_dir: Option<&Path>) -> Judged {
    match config::faults_of(config::read_config_in(dir, name, home, agh_dir), home) {
        None => Judged {
            present: false,
            counts: Counts::default(),
            faults: Vec::new(),
            rows: None,
        },
        Some(judged) => Judged {
            present: true,
            counts: judged.counts,
            faults: judged.verdict.err().unwrap_or_default(),
            rows: judged.parsed,
        },
    }
}

/// Off unix there is no permission check, so agh never reads agh.toml there (design §6.6:
/// every read is anonymous), and `agh policy check` follows `config::load`'s rule: no file.
#[cfg(not(unix))]
fn judge(dir: &Path, name: &str, home: &Path, agh_dir: Option<&Path>) -> Judged {
    let _ = (dir, name, home, agh_dir);
    Judged {
        present: false,
        counts: Counts::default(),
        faults: Vec::new(),
        rows: None,
    }
}

/// `agh policy grant CALLER REPO` (design §6.2): `REPO` added to `CALLER`'s `[[writers]]`
/// row of agh.toml, which is written again whole, atomically and 0600. It runs with or without
/// a terminal (owner ruling 2026-10-26: an agent grants as a person does). Its answer is
/// `{"caller","change","repos"}`: `added` or `unchanged`, and the row after the edit.
///
/// # Errors
///
/// Usage for a caller outside `[a-z][a-z0-9-]{0,31}` or a `REPO` that is no literal
/// `OWNER/NAME` (each quoted with a token-shaped part masked). Refused when there is no
/// agh.toml; when another edit holds the auth edit lock, or the config directory or the lock
/// is not private; when `read_private` refuses agh.toml or it is not UTF-8; and, with nothing
/// written, when the edit would break one of its rules, would make it longer than
/// `config::CONFIG_MAX`, or cannot be made in place (`config::edit_writers`'s Refused).
/// Invalid when agh.toml does not parse or its `writers` cannot be edited
/// (`config::edit_writers`'s Invalid). could_not_run off unix, when no config directory
/// resolves, and when agh.toml or the lock cannot be read or written.
pub(crate) fn policy_grant(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    edit_grant(ctx, inv, true)
}

/// `agh policy revoke CALLER REPO`: `REPO` taken out of `CALLER`'s `[[writers]]` row, and the
/// row with its last repository. As `policy grant`, with `removed` or `unchanged`.
///
/// # Errors
///
/// As `policy grant`.
pub(crate) fn policy_revoke(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    edit_grant(ctx, inv, false)
}

/// `policy grant` (`add`) and `policy revoke`: the arguments, the file, the edit, the write and
/// the answer, in that order.
fn edit_grant(ctx: &mut Ctx, inv: &Invocation, add: bool) -> Result<Outcome> {
    let caller = inv.require(0, "CALLER")?;
    let repo = inv.require(1, "REPO")?;
    if let Some(fault) = config::caller_fault(caller) {
        return Err(inv.usage(fault));
    }
    let slug = match RepoArg::parse(repo) {
        Ok(RepoArg::Literal(slug)) => slug,
        Ok(RepoArg::Target(name)) => {
            return Err(inv.usage(config::masked(&format!(
                "REPO is a literal OWNER/NAME, not @{name}"
            ))));
        }
        // The parser quotes what was typed, and a legacy token pasted as REPO is all hex, which
        // the Redactor cannot know: masked as CALLER's fault is.
        Err(error) => return Err(inv.usage(config::masked(&error.why))),
    };
    let Some(dirs) = ctx.dirs.clone() else {
        return Err(Error::could_not_run(
            "no config directory: agh cannot provision here",
        ));
    };
    let (change, repos) = write_grant(&dirs, caller, &slug, add)?;
    let mut value = writer_row(caller, &repos, &ctx.redactor);
    if let Some(object) = value.as_object_mut() {
        object.insert("change".into(), Value::from(change));
    }
    Ok(Outcome {
        answer: Answer::Object {
            noun: Noun::Policy,
            value,
        },
        receipt: Receipt::new(&inv.verb_name()),
    })
}

/// The why of `AuthEdit::take`'s refusal while another edit holds the lock.
#[cfg(unix)]
const AUTH_EDIT_BUSY: &str = "another agh auth edit is in progress";

/// The edit on unix, under the auth edit lock: agh.toml read with `read_private`, edited by
/// `config::edit_writers`, judged by L1–L11 and held to `config::CONFIG_MAX`, and written with
/// `write_private`. The change (`added`, `removed` or `unchanged`) and the caller's
/// repositories after it.
#[cfg(unix)]
fn write_grant(
    dirs: &config::Dirs,
    caller: &str,
    slug: &Slug,
    add: bool,
) -> Result<(&'static str, Vec<String>)> {
    use crate::config::ConfigText;
    use crate::identity::provision::AuthEdit;
    let verb = if add { "grant" } else { "revoke" };
    let missing = || {
        Error::refused(format!("there is no agh.toml to {verb} in"))
            .with_next("agh auth add NAME --file PATH --tier TIER")
    };
    if std::fs::symlink_metadata(dirs.config_file()).is_err() {
        return Err(missing());
    }
    // The auth verbs' lock, whose busy refusal names the auth command: told again for this verb.
    let _edit = AuthEdit::take(dirs).map_err(|error| {
        if error.kind == Kind::Refused && error.why == AUTH_EDIT_BUSY {
            Error::refused("another agh.toml edit is in progress").with_next(format!(
                "run agh policy {verb} again after that edit finishes"
            ))
        } else {
            error
        }
    })?;
    let text = match config::read_config(&dirs.config, CONFIG_FILE, &dirs.home) {
        ConfigText::Text(text) => text,
        ConfigText::Missing => return Err(missing()),
        ConfigText::Refused { error, remedy } => {
            return Err(config::invalid(
                format!("agh.toml: {error}"),
                remedy.unwrap_or_else(|| config::POLICY_CHECK.to_string()),
            ));
        }
        ConfigText::Io(error) => {
            return Err(Error::could_not_run(format!("agh.toml: {error}")));
        }
        ConfigText::NotUtf8 => {
            return Err(config::invalid(
                "agh.toml is not UTF-8".to_string(),
                config::POLICY_CHECK,
            ));
        }
    };
    let edited = config::edit_writers(&text, caller, slug, add)?;
    let judged = config::judge(&edited, &dirs.home);
    if let Err(faults) = &judged.verdict {
        let first = faults.first().map_or_else(String::new, |fault| {
            format!("{} {}", fault.rule, fault.what)
        });
        return Err(
            Error::refused(format!("the edit would break agh.toml: {first}"))
                .with_next(config::POLICY_CHECK),
        );
    }
    let repos = judged
        .parsed
        .as_ref()
        .and_then(|rows| rows.writers.iter().find(|row| row.caller == caller))
        .map(|row| row.repos.clone())
        .unwrap_or_default();
    if edited == text {
        return Ok(("unchanged", repos));
    }
    // Every load reads at most CONFIG_MAX bytes and refuses a longer file, and with it every
    // verb, `policy revoke` included: such an edit is never written.
    if edited.len() > config::CONFIG_MAX {
        return Err(Error::refused(format!(
            "the {verb} would make agh.toml longer than such a file can be ({} bytes; at most \
             {}; nothing is written)",
            edited.len(),
            config::CONFIG_MAX
        ))
        .with_next(format!(
            "make agh.toml shorter, then run agh policy {verb} again"
        )));
    }
    aterm_uds::private_file::write_private(&dirs.config, CONFIG_FILE, edited.as_bytes()).map_err(
        |e| Error::could_not_run(format!("{}: {e}", tilde(&dirs.config_file(), &dirs.home))),
    )?;
    Ok((if add { "added" } else { "removed" }, repos))
}

/// Off unix agh never reads or writes agh.toml (design §6.6), so it grants nothing there.
#[cfg(not(unix))]
fn write_grant(
    dirs: &config::Dirs,
    caller: &str,
    slug: &Slug,
    add: bool,
) -> Result<(&'static str, Vec<String>)> {
    let _ = (dirs, caller, slug, add);
    Err(Error::could_not_run(
        "agh reads and writes agh.toml only on unix, where its permissions are checked",
    )
    .with_sub(crate::error::Sub::Local))
}
