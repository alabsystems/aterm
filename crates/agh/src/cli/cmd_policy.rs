// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `agh policy check` (design §6.3): agh.toml against L1–L10, every fault at once, and with no
//! agh.toml, the vault's adopted rows (design §6.10).

use std::path::{Path, PathBuf};

use aterm_json::{Map, Value};

use super::args::Invocation;
use super::verbs::Noun;
use super::{Answer, Ctx, Outcome, Receipt};
use crate::config::{self, CONFIG_FILE, ConfigFault, Counts};
use crate::error::{Error, Kind, Result};
use crate::identity::tilde;
use crate::identity::vault::{self, Found};
use crate::redact::Redactor;

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
    let (present, counts, mut faults) = judge(&dir, &name, &home, agh_dir);
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

/// `{"config","counts":{"owners","profiles","targets"},"faults":[{"rule","what"}],"ok",
/// "present"}`, to which `policy_check` adds `adopted`. The path and each `what` pass `redactor`,
/// as the error lines do (design §6.6):
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

/// Whether the file is there, its row counts, and its faults.
type Judged = (bool, Counts, Vec<ConfigFault>);

/// What reading and judging the file found: whether it is there, its row counts, and its
/// faults. No file is a valid state (`present: false`), as it is for `config::load`.
#[cfg(unix)]
fn judge(dir: &Path, name: &str, home: &Path, agh_dir: Option<&Path>) -> Judged {
    match config::faults_of(config::read_config_in(dir, name, home, agh_dir), home) {
        None => (false, Counts::default(), Vec::new()),
        Some(judged) => (
            true,
            judged.counts,
            judged.verdict.err().unwrap_or_default(),
        ),
    }
}

/// Off unix there is no permission check, so agh never reads agh.toml there (design §6.6:
/// every read is anonymous), and `agh policy check` follows `config::load`'s rule: no file.
#[cfg(not(unix))]
fn judge(dir: &Path, name: &str, home: &Path, agh_dir: Option<&Path>) -> Judged {
    let _ = (dir, name, home, agh_dir);
    (false, Counts::default(), Vec::new())
}
