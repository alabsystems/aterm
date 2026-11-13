// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The journal's cache for the harness's account surfaces
//! (`docs/DESIGN-alab-lab-model-2026-11-01.md` §7, §8, step 11): the harness's
//! `aterm_agent::harness::alab_seen::Source`, filled from `aterm_db::alab::journal::seen` and the
//! registry's LabView, which names each journal record's Mac.
//!
//! A read is an observer's: it reads this Mac's Commons (the journal window and the registry) and
//! its heard archive, writes nothing and calls no provider. A surface that paints or answers a
//! keystroke asks without waiting and gets what is held; the first such ask starts a background
//! thread that reads at once and then every five minutes. A verb a person ran asks and waits for
//! one read. As the lab's roster source, it is inert in a test, a debug build and a build tool's
//! run (`alab_view::holds`), and `[alab] roster = false` turns it off.
//! An unavailable local policy hides held data without discarding its history.

use aterm_agent::harness::alab_seen::{
    self, AccountSeen, Figure, HISTORY_MAX, LastUse, Line, SignedIn, WindowSeen,
};
use aterm_db::alab::{
    Inventory, LabRead, Registry,
    journal::{
        Spool,
        seen::{self, Usage},
    },
    view::LabView,
};
use aterm_secrets::client::Paths;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// The least time between two background reads.
const READ_EVERY: Duration = Duration::from_secs(300);

/// How far back the history reaches into the heard archive, in seconds: the ring's two weeks.
const HEARD_BACK_S: u64 = 14 * 86_400;

/// How a journal record's Mac is shown: its machine label where its facts row binds a live
/// machine with one, else `uuid:<first 8 hex>`.
fn mac(view: &LabView, uuid: &str) -> String {
    view.bound
        .get(uuid)
        .and_then(|pk| view.machine_label(pk))
        .map_or_else(
            || format!("uuid:{}", uuid.get(..8).unwrap_or(uuid)),
            str::to_owned,
        )
}

fn secs(t: u64) -> i64 {
    i64::try_from(t).unwrap_or(i64::MAX)
}

/// The closed words one event carries beyond its kind: its window and figure, its reason, the
/// other account of a switch by its lab label. Never free text: every word is the journal's.
fn detail(view: &LabView, event: &aterm_db::alab::journal::Event) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let sep = |out: &mut String| {
        if !out.is_empty() {
            out.push(' ');
        }
    };
    if let Some(window) = event.window {
        out.push_str(window.word());
    }
    if let Some(p) = event.pct_x10 {
        sep(&mut out);
        let _ = write!(out, "{}.{}%", p / 10, p % 10);
    }
    if let Some(why) = &event.why {
        sep(&mut out);
        out.push_str("why=");
        out.push_str(why.as_str());
    }
    if let Some(to) = &event.to {
        sep(&mut out);
        let label = view
            .accounts
            .get(to.as_str())
            .and_then(|a| a.label.as_deref())
            .unwrap_or_else(|| to.as_str());
        out.push_str("to=");
        out.push_str(label);
    }
    out
}

/// What the journal says, as the harness reads it: each account `seen` knows, its Macs named
/// through `view`.
pub(crate) fn harness_seen(view: &LabView, seen: &seen::Seen) -> alab_seen::Seen {
    let mut history_by_pk: std::collections::BTreeMap<&str, Vec<&seen::Logged>> =
        std::collections::BTreeMap::new();
    for logged in seen.events.iter().rev() {
        let a1 = logged.event.account.as_ref().map(|a| a.as_str());
        let a2 = logged
            .event
            .to
            .as_ref()
            .map(|a| a.as_str())
            .filter(|&to| Some(to) != a1);
        for acct in a1.into_iter().chain(a2) {
            let bucket = history_by_pk.entry(acct).or_default();
            if bucket.len() < HISTORY_MAX {
                bucket.push(logged);
            }
        }
    }
    let mut accounts = std::collections::BTreeMap::new();
    for pk in seen.accounts() {
        let last_use = seen.last_use.get(&pk).map(|(uuid, t)| LastUse {
            by: mac(view, uuid),
            t: secs(*t),
        });
        let mut signed_in: Vec<SignedIn> = seen
            .signins
            .get(&pk)
            .into_iter()
            .flatten()
            .map(|(uuid, s)| SignedIn {
                on: mac(view, uuid),
                state: s.state.to_owned(),
                t: secs(s.t),
            })
            .collect();
        signed_in.sort_unstable_by(|a, b| b.t.cmp(&a.t).then_with(|| a.on.cmp(&b.on)));
        let windows = seen
            .quota(&pk)
            .into_iter()
            .map(|q| {
                let (figure, read) = match q.usage {
                    None => (None, None),
                    Some(Usage::Current {
                        pct_x10,
                        reset,
                        read,
                        by,
                        src,
                    }) => (
                        Some(Figure::Used {
                            pct_x10,
                            reset: reset.map(secs),
                        }),
                        Some((secs(read), mac(view, &by), src)),
                    ),
                    Some(Usage::ResetUnread { reset, read, by }) => (
                        Some(Figure::ResetUnread { reset: secs(reset) }),
                        Some((secs(read), mac(view, &by), "reading".to_owned())),
                    ),
                };
                WindowSeen {
                    window: q.window.word().to_owned(),
                    sub: (q.pk != pk).then_some(q.pk),
                    figure,
                    read,
                    wall_until: q.wall.map(|w| w.until.map(secs)),
                }
            })
            .collect();
        let history: Vec<Line> = history_by_pk
            .remove(pk.as_str())
            .into_iter()
            .flatten()
            .rev()
            .map(|logged| Line {
                t: secs(logged.event.t),
                by: mac(view, &logged.uuid),
                kind: logged.event.kind.word().to_owned(),
                cli: logged.event.cli.as_str().to_owned(),
                detail: detail(view, &logged.event),
                heard: logged.heard,
                window: logged.event.window.map(|w| w.word().to_owned()),
                sub: logged.event.sub.is_some(),
                pct_x10: logged.event.pct_x10,
                reset: logged.event.reset.map(secs),
            })
            .collect();
        accounts.insert(
            pk,
            AccountSeen {
                last_use,
                signed_in,
                windows,
                history,
            },
        );
    }
    alab_seen::Seen {
        accounts,
        at: secs(seen.now),
    }
}

/// The view a journal read names Macs by, given what the registry read gave and the `last`
/// projection held: a projection replaces `last`, a partial history (whose rows are not all here
/// yet) and an unmoved inventory read with it, and no lab drops it.
fn journal_view(
    read: Option<LabRead>,
    last: &mut Option<(Arc<LabView>, Inventory)>,
) -> Arc<LabView> {
    match read {
        Some(LabRead::Projected(view, inventory)) => {
            let view = Arc::new(*view);
            *last = Some((Arc::clone(&view), inventory));
            view
        }
        Some(LabRead::Partial | LabRead::Unchanged) => last
            .as_ref()
            .map_or_else(Arc::default, |(view, _)| Arc::clone(view)),
        None => {
            *last = None;
            Arc::default()
        }
    }
}

struct Cache {
    paths: Paths,
    config: PathBuf,
    spool: Option<Spool>,
    held: Mutex<Option<Arc<alab_seen::Seen>>>,
    /// The last view of the registry projected here, and the inventory it was projected from.
    lab: Mutex<Option<(Arc<LabView>, Inventory)>>,
    reader: OnceLock<()>,
}

impl Cache {
    /// One read of the registry and the journal window ([`journal_view`] names the Macs); a
    /// failed or unheld read keeps what was held.
    fn read(&self) {
        match crate::alab_view::roster_policy(&self.config) {
            crate::alab_view::LocalRosterPolicy::Enabled => {}
            crate::alab_view::LocalRosterPolicy::Unavailable => return,
            crate::alab_view::LocalRosterPolicy::Disabled => {
                *self
                    .held
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                *self
                    .lab
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                return;
            }
        }
        let now = aterm_time::unix_millis().unwrap_or(0) / 1000;
        let mut last = self
            .lab
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let held = last.as_ref().map(|(_, inventory)| *inventory);
        let view = match Registry::at(self.paths.clone()).lab(&self.config, held) {
            Ok(read) => journal_view(read, &mut last),
            Err(why) => {
                aterm_log::debug!("journal view: {why}");
                return;
            }
        };
        drop(last);
        let heard = self
            .spool
            .as_ref()
            .map(|spool| (spool, now.saturating_sub(HEARD_BACK_S)));
        match seen::read(&self.paths, heard, now) {
            Ok(Some(seen)) => {
                let fresh = Arc::new(harness_seen(&view, &seen));
                *self
                    .held
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(fresh);
            }
            Ok(None) => {}
            Err(why) => aterm_log::debug!("journal view: {why}"),
        }
    }

    fn start_reader(&'static self) {
        self.reader.get_or_init(|| {
            let _ = std::thread::Builder::new()
                .name("alab-seen".into())
                .spawn(move || {
                    loop {
                        self.read();
                        std::thread::sleep(READ_EVERY);
                    }
                });
        });
    }
}

impl alab_seen::Source for &'static Cache {
    fn seen(&self, wait: bool) -> Option<Arc<alab_seen::Seen>> {
        if !matches!(
            crate::alab_view::roster_policy(&self.config),
            crate::alab_view::LocalRosterPolicy::Enabled
        ) {
            return None;
        }
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if held.is_none() && wait {
            self.read();
            held = self
                .held
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
        }
        self.start_reader();
        held
    }
}

static CACHE: OnceLock<Cache> = OnceLock::new();

/// Install the harness's journal source: inert where this process holds no lab
/// (`alab_view::holds`).
pub(crate) fn install() {
    if !crate::alab_view::holds() {
        return;
    }
    let (Some(home), Some(config)) = (
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|h| h.is_absolute()),
        aterm_types::dirs::aterm_config_path(),
    ) else {
        return;
    };
    let cache = CACHE.get_or_init(|| Cache {
        paths: Paths::for_home(&home),
        config,
        spool: Spool::local(),
        held: Mutex::new(None),
        lab: Mutex::new(None),
        reader: OnceLock::new(),
    });
    let _ = alab_seen::install(Box::new(cache));
}

#[cfg(test)]
#[path = "alab_seen_tests.rs"]
mod tests;
