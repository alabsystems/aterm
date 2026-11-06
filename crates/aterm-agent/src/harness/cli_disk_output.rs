// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Emit the completed disk report and apply result without discovery or cleanup.

use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;

use aterm_json::{Map, Value};

use super::DiskWatchStatus;
use crate::harness::disk;

/// An immutable view of decisions already made before output begins.
pub(super) struct DiskOutput<'a> {
    pub(super) report: &'a disk::Report,
    pub(super) watch: &'a DiskWatchStatus,
    pub(super) applied: Option<&'a disk::Applied>,
    pub(super) json: bool,
    pub(super) config_path: Option<&'a Path>,
    pub(super) off_by_fault: bool,
}

impl DiskOutput<'_> {
    pub(super) fn print(&self, out: &mut dyn Write, err: &mut dyn Write) -> ExitCode {
        if let Err(error) = self.write(out) {
            let _ = writeln!(err, "aterm harness disk: could not write results: {error}");
            return ExitCode::from(1);
        }
        ExitCode::SUCCESS
    }

    fn write(&self, out: &mut dyn Write) -> io::Result<()> {
        let rep = self.report;
        let watch = self.watch;
        let done = self.applied;
        let json = self.json;
        let off_by_fault = self.off_by_fault;
        if json {
            writeln!(out, "{}", rep.to_json())?;
            writeln!(
                out,
                "{}",
                aterm_json::to_string(&watch.json).map_err(io::Error::other)?
            )?;
            if let Some(done) = &done {
                let mut o = Map::new();
                o.insert("schema".to_owned(), Value::from(1u64));
                o.insert("kind".to_owned(), Value::from("disk-apply".to_owned()));
                o.insert(
                    "removed".to_owned(),
                    Value::from(u64::try_from(done.removed.len()).unwrap_or(u64::MAX)),
                );
                o.insert("freed_bytes".to_owned(), Value::from(done.freed_bytes));
                let texts = |items: Vec<String>| {
                    Value::Array(items.into_iter().map(Value::from).collect::<Vec<_>>())
                };
                o.insert(
                    "rows".to_owned(),
                    Value::Array(
                        done.removed
                            .iter()
                            .map(|(row, removed)| {
                                let mut r = Map::new();
                                r.insert(
                                    "path".to_owned(),
                                    Value::from(row.path.display().to_string()),
                                );
                                r.insert("freed_bytes".to_owned(), Value::from(removed.bytes));
                                r.insert(
                                    "units".to_owned(),
                                    texts(
                                        removed
                                            .units
                                            .iter()
                                            .map(|p| p.display().to_string())
                                            .collect(),
                                    ),
                                );
                                r.insert(
                                    "skipped".to_owned(),
                                    texts(
                                        removed
                                            .skipped
                                            .iter()
                                            .map(|(p, why)| format!("{}: {why}", p.display()))
                                            .collect(),
                                    ),
                                );
                                r.insert("trouble".to_owned(), texts(removed.trouble.clone()));
                                Value::Object(r)
                            })
                            .collect(),
                    ),
                );
                o.insert(
                    "denials".to_owned(),
                    Value::Array(
                        done.denials
                            .iter()
                            .map(|d| Value::from(d.describe()))
                            .collect(),
                    ),
                );
                o.insert(
                    "stopped".to_owned(),
                    done.stopped
                        .as_ref()
                        .map_or(Value::Null, |s| Value::from(s.describe())),
                );
                writeln!(
                    out,
                    "{}",
                    aterm_json::to_string(&Value::Object(o)).map_err(io::Error::other)?
                )?;
            }
            return out.flush();
        }

        writeln!(out, "{}", rep.headline())?;
        writeln!(out, "  {}", watch.line)?;
        for row in &rep.rows {
            writeln!(out, "  {}", row.line())?;
        }
        if rep
            .rows
            .iter()
            .any(|r| matches!(r.witness, disk::Witness::LeastRecentlyUsed { .. }))
        {
            writeln!(
                out,
                "  under pressure: the least-recently-used rows go in the order listed, until free space is back or what they freed covers the shortfall; under_pressure_up_to= is the most they would free"
            )?;
        }
        for note in &rep.notes {
            writeln!(out, "  note: {}", disk::shown(note))?;
        }
        if rep.rows.is_empty() {
            writeln!(out, "  nothing to reclaim")?;
        }
        if !rep.hogs.is_empty() {
            writeln!(out, "  the largest it cannot reclaim:")?;
            for hog in &rep.hogs {
                writeln!(out, "    {}", hog.line(rep.home.as_deref()))?;
            }
        }
        let config_file = self
            .config_path
            .map_or_else(|| "aterm.toml".to_string(), |p| p.display().to_string());
        let remedy = if off_by_fault {
            format!("fix what the stderr note names in {config_file}")
        } else {
            format!("set `[disk] apply = true` in {config_file}")
        };
        match &done {
            None => {
                let classes: Vec<&str> = disk::Class::ALL
                    .iter()
                    .filter(|c| rep.rows.iter().any(|r| r.class == **c && r.removable))
                    .map(|c| c.as_str())
                    .collect();
                if !classes.is_empty() {
                    let config = if rep.config.apply {
                        String::new()
                    } else {
                        format!("; also {remedy}")
                    };
                    writeln!(
                        out,
                        "report only: to remove, add --apply <class> ({}){config}",
                        classes.join(", ")
                    )?;
                }
            }
            Some(done) => {
                writeln!(out, "{}", done.headline())?;
                for (row, removed) in &done.removed {
                    let line = format!(
                        "  removed {} — {}; counted {} of file allocation",
                        row.path.display(),
                        row.witness.describe(),
                        disk::human_bytes(removed.bytes)
                    );
                    writeln!(out, "{}", disk::shown(&line))?;
                    for unit in &removed.units {
                        writeln!(
                            out,
                            "    went: {}",
                            disk::shown(&unit.display().to_string())
                        )?;
                    }
                    for (unit, why) in &removed.skipped {
                        let line = format!("    kept: {} — {why}", unit.display());
                        writeln!(out, "{}", disk::shown(&line))?;
                    }
                    for trouble in &removed.trouble {
                        writeln!(out, "    trouble: {}", disk::shown(trouble))?;
                    }
                }
                for refusal in &done.denials {
                    let fix = if matches!(refusal, disk::Refusal::ApplyOff(_)) {
                        format!("; {remedy}")
                    } else {
                        String::new()
                    };
                    writeln!(out, "  denied: {}{fix}", disk::shown(&refusal.describe()))?;
                }
                if let Some(stop) = &done.stopped {
                    writeln!(out, "  stopped: {}", disk::shown(&stop.describe()))?;
                }
            }
        }
        out.flush()
    }
}
