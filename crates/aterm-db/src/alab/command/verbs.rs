// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The lab verbs of design §8 over LabView and the journal window: the readers (`machines`,
//! `accounts`, `preferences`, `mail`, `who`, `quota`, `log`, `signins`) and the owner's writers
//! (`add machine|account`, `retire`, `relabel`, `edit`). A reader prints keys, labels, counts,
//! times and words, never an address: an account is its key and its label, an address its key
//! and its mailbox's label. A writer writes only the cells that change, as the owner (one record
//! a row, a random request id, `att::Handle::commit_owner`), and touches no Mac's login: nothing
//! here reaches an account directory, so no one is signed out by a retire or a relabel.

use super::{Args, Io, named_row};
use crate::{
    Error, Result,
    alab::{
        journal::{
            self,
            event::{Event, Window},
            seen::{Seen, Usage, WindowQuota},
        },
        mail,
        registry::{self, ACCOUNT, ADDRESS, MACHINE, MAILBOX, MODEL, PHONE, PREFERENCE},
        view::LabView,
    },
    att::{self, Handle},
    machines::Label,
    typed::{
        LIFECYCLE,
        cell::{self, CellKey, CellValue, Projection},
        guard, keys,
    },
};
use aterm_json::{Value, json};
use aterm_secrets::client::Paths;
use std::collections::BTreeMap;

/// How far back `log` reads by default, in days: the journal ring's two-week window.
pub(super) const LOG_DAYS: u64 = 14;

/// The bytes a row's `note` column holds (the registry's `Ty::Text(512)`).
const NOTE_BYTES: usize = 512;

/// The most `log` reads back, in days: the heard archive's 120.
const LOG_DAYS_MAX: u64 = 120;

/// The registry as LabView reads it on this Mac, as an observer.
fn lab(paths: &Paths) -> Result<(att::View, LabView)> {
    let view = Handle::at(paths.clone(), &att::REGISTRY).view()?;
    let empty = crate::typed::owned::OwnedRead::default();
    let owned = |name: &str| view.owned.get(name).unwrap_or(&empty);
    let lab = LabView::read(
        &view.cells,
        owned(registry::MACHINE_FACTS.name),
        owned(registry::MAILBOX_OBS.name),
        None,
    );
    Ok((view, lab))
}

/// The journal window at `now_ms`, with the heard archive back to `since` where `io` names one;
/// an empty reading where this Mac holds no Commons it is a member of.
fn seen(paths: &Paths, io: &Io<'_>, since: u64, now_ms: u64) -> Result<Seen> {
    let now = now_ms / 1000;
    let heard = io.heard.as_ref().map(|spool| (spool, since));
    Ok(journal::seen::read(paths, heard, now)?.unwrap_or_else(|| Seen::empty(now)))
}

/// A Mac as a journal record names it: the machine its facts row binds, by label, and the uuid.
fn mac(lab: &LabView, uuid: &str) -> Value {
    let machine = lab.bound.get(uuid);
    json!({
        "machine": machine.and_then(|pk| lab.machine_label(pk)),
        "mc": machine,
        "uuid": uuid,
    })
}

/// When, and how long before `now`: "about 3 h ago (its clock)" in fields.
fn at(t: u64, now: u64) -> Value {
    if t > now {
        json!({"t": t, "ahead_s": t - now, "clock": "its"})
    } else {
        json!({"t": t, "ago_s": now - t, "clock": "its"})
    }
}

fn label_str<'a>(lab: &'a LabView, pk: &str) -> Option<&'a str> {
    lab.accounts
        .get(pk)
        .and_then(|account| account.label.as_deref())
}

fn label_of(lab: &LabView, pk: &str) -> Option<String> {
    label_str(lab, pk).map(str::to_owned)
}

/// The account `name` names: its key, its label or its legacy label, live or not. Never an
/// address, which never goes on a command line.
fn account_named(lab: &LabView, name: &str) -> Result<String> {
    if name.contains('@') {
        return Err(Error::new(
            "an account is named by its label, legacy label or key, never its address",
        ));
    }
    if lab.accounts.contains_key(name) {
        return Ok(name.to_owned());
    }
    let found: Vec<&String> = lab
        .accounts
        .values()
        .filter(|a| a.label.as_deref() == Some(name) || a.legacy_label.as_deref() == Some(name))
        .map(|a| &a.pk)
        .collect();
    match found.as_slice() {
        [pk] => Ok((*pk).clone()),
        [] => Err(Error::new("no account goes by that name")),
        _ => Err(Error::new(
            "more than one account goes by that name: name it by its key",
        )),
    }
}

/// The machine `name` names: its key, else the one live row its label or an alias names (a
/// retired row under the same label holds no label, as the importer's `live_by_label` and `add
/// machine` read it), else the one retired row it names, for its history.
pub(super) fn machine_named(lab: &LabView, name: &str) -> Option<String> {
    if lab.machines.contains_key(name) {
        return Some(name.to_owned());
    }
    let wanted = Label::parse(name).ok()?.canonical();
    let found: Vec<_> = lab
        .machines
        .values()
        .filter(|m| m.label.as_deref() == Some(&wanted) || m.aliases.contains(&wanted))
        .collect();
    let live: Vec<_> = found.iter().copied().filter(|m| m.live).collect();
    match (live.as_slice(), found.as_slice()) {
        ([m], _) | ([], [m]) => Some(m.pk.clone()),
        _ => None,
    }
}

fn text(row: &cell::Row, column: &str) -> Value {
    row.get(column).cloned().unwrap_or(Value::Null)
}

/// The facts columns `machines <machine>` shows of each facts row bound to the machine: its
/// hardware and OS, never its host names.
const FACTS_SHOWN: [&str; 10] = [
    "hw_model",
    "chip",
    "cores_p",
    "cores_e",
    "gpu_cores",
    "ram_gb",
    "disk_gb",
    "arch",
    "os_major",
    "seen",
];

/// The `sum` columns `machines <machine>` shows: the Mac's own word on its label, build, OS, how
/// many accounts it used this week and whether its journal publishes.
const SUM_SHOWN: [&str; 6] = ["label", "aterm", "os", "used", "journal", "period"];

/// What runs on machine `mc` (§7 "What runs on m2?"): its row's words, each bound facts row's
/// hardware, its live preferences by rank with each account's label, status, plan and quota per
/// window, its live `place` rows (accounts it runs and does not prefer, with why and since), and
/// each bound Mac's latest `sum` row. `facts` is each facts row's value by its uuid; `plans`
/// each account's `plan` cell. Pure, so a fixture can hold it to the query.
pub(super) fn runs_on(
    lab: &LabView,
    row: Option<&cell::Row>,
    facts: &std::collections::BTreeMap<String, Value>,
    plans: &dyn Fn(&str) -> Value,
    seen: &Seen,
    mc: &str,
) -> Value {
    let uuids: Vec<&String> = lab
        .bound
        .iter()
        .filter(|(_, bound)| *bound == mc)
        .map(|(uuid, _)| uuid)
        .collect();
    let pick = |value: &Value, columns: &[&str]| {
        let mut out = aterm_json::Map::new();
        for column in columns {
            if let Some(cell) = value.get(column) {
                out.insert((*column).to_owned(), cell.clone());
            }
        }
        Value::Object(out)
    };
    let bound_facts: Vec<Value> = uuids
        .iter()
        .filter_map(|uuid| {
            let mut out = pick(facts.get(*uuid)?, &FACTS_SHOWN);
            if let Value::Object(o) = &mut out {
                o.insert("uuid".into(), json!(uuid));
            }
            Some(out)
        })
        .collect();
    let (preferred, placed) = lab.runs_on(mc, &seen.places);
    let mut preferred: Vec<&crate::alab::view::Preference> = preferred;
    preferred.sort_by(|a, b| {
        (
            a.rank.is_none(),
            a.rank,
            label_str(lab, &a.account),
            a.account.as_str(),
        )
            .cmp(&(
                b.rank.is_none(),
                b.rank,
                label_str(lab, &b.account),
                b.account.as_str(),
            ))
    });
    let account = |pk: &str| {
        let found = lab.accounts.get(pk);
        json!({
            "ac": pk,
            "label": label_of(lab, pk),
            "status": found.and_then(|a| a.status.clone()),
            "plan": plans(pk),
            "quota": seen.quota(pk).iter().map(|q| usage_json(lab, q, seen.now)).collect::<Vec<_>>(),
        })
    };
    let prefers: Vec<Value> = preferred
        .iter()
        .map(|p| {
            let mut out = account(&p.account);
            if let Value::Object(o) = &mut out {
                o.insert("rank".into(), json!(p.rank));
            }
            out
        })
        .collect();
    let places: Vec<Value> = placed
        .iter()
        .map(|p| {
            let mut out = account(&p.account);
            if let Value::Object(o) = &mut out {
                o.insert("why".into(), json!(p.why.word()));
                o.insert("since".into(), at(p.since, seen.now));
            }
            out
        })
        .collect();
    let sums: Vec<Value> = uuids
        .iter()
        .filter_map(|uuid| {
            let mut out = pick(seen.sums.get(*uuid)?, &SUM_SHOWN);
            if let (Value::Object(o), Some(t)) = (
                &mut out,
                seen.sums
                    .get(*uuid)
                    .and_then(|s| s.get("at"))
                    .and_then(Value::as_u64),
            ) {
                o.insert("at".into(), at(t, seen.now));
            }
            if let Value::Object(o) = &mut out {
                o.insert("uuid".into(), json!(uuid));
            }
            Some(out)
        })
        .collect();
    let machine = lab.machines.get(mc);
    json!({
        "mc": mc,
        "label": machine.and_then(|m| m.label.clone()),
        "aliases": machine.map(|m| m.aliases.clone()).unwrap_or_default(),
        "status": row.map_or(Value::Null, |r| text(r, "status")),
        "role": row.map_or(Value::Null, |r| text(r, "role")),
        "model": row.map_or(Value::Null, |r| text(r, "model")),
        "make": row.map_or(Value::Null, |r| text(r, "make")),
        "arch": row.map_or(Value::Null, |r| text(r, "arch")),
        "os": row.map_or(Value::Null, |r| text(r, "os")),
        "chip": row.map_or(Value::Null, |r| text(r, "chip")),
        "ram_gb": row.map_or(Value::Null, |r| text(r, "ram_gb")),
        "disk_gb": row.map_or(Value::Null, |r| text(r, "disk_gb")),
        "nickname": row.map_or(Value::Null, |r| text(r, "nickname")),
        "nickname_aliases": row.map_or(Value::Null, |r| text(r, "nickname_aliases")),
        "location": row.map_or(Value::Null, |r| text(r, "location")),
        "row": row.and_then(cell::Row::lifecycle),
        "facts": bound_facts,
        "prefers": prefers,
        "places": places,
        "sum": sums,
    })
}

/// `machines <machine>`: [`runs_on`] the machine its label, alias or key names.
fn machine(paths: &Paths, name: &str, io: &Io<'_>, now_ms: u64) -> Result<Value> {
    let (view, lab) = lab(paths)?;
    let mc = machine_named(&lab, name)
        .ok_or_else(|| Error::new("no one machine goes by that name: name it by its key"))?;
    let seen = seen(paths, io, now_ms / 1000, now_ms)?;
    let rows = view.project(&MACHINE);
    let accounts = view.project(&ACCOUNT);
    let facts: std::collections::BTreeMap<String, Value> = view
        .owned
        .get(registry::MACHINE_FACTS.name)
        .map(|read| {
            read.rows
                .iter()
                .map(|(uuid, row)| (uuid.clone(), row.value.clone()))
                .collect()
        })
        .unwrap_or_default();
    let plans = |pk: &str| {
        accounts
            .rows
            .get(pk)
            .map_or(Value::Null, |r| text(r, "plan"))
    };
    let mut out = runs_on(&lab, rows.rows.get(&mc), &facts, &plans, &seen, &mc);
    if let Value::Object(o) = &mut out {
        o.insert("complete".into(), json!(view.complete));
    }
    Ok(out)
}

/// `machines [<machine>]`: every machine row, its labels, words and what the lab maps to it; with
/// a machine named, what runs on it ([`runs_on`]).
pub(super) fn machines(paths: &Paths, args: &Args, io: &Io<'_>, now_ms: u64) -> Result<Value> {
    match args.operands.as_slice() {
        [] => {}
        [name] => return machine(paths, name, io, now_ms),
        _ => return Err(Error::new("name at most one machine")),
    }
    let (view, lab) = lab(paths)?;
    let rows = view.project(&MACHINE);
    let mut bound_by_machine: BTreeMap<&str, Vec<&String>> = BTreeMap::new();
    for (uuid, bound) in &lab.bound {
        bound_by_machine
            .entry(bound.as_str())
            .or_default()
            .push(uuid);
    }
    let mut out = Vec::with_capacity(lab.machines.len());
    for (pk, machine) in &lab.machines {
        let row = rows.rows.get(pk);
        let prefers = lab.preferred_by(pk).count();
        let uuids = bound_by_machine
            .get(pk.as_str())
            .map_or(&[][..], Vec::as_slice);
        out.push(json!({
            "mc": pk,
            "label": machine.label,
            "aliases": machine.aliases,
            "status": row.map_or(Value::Null, |r| text(r, "status")),
            "role": row.map_or(Value::Null, |r| text(r, "role")),
            "model": row.map_or(Value::Null, |r| text(r, "model")),
            "make": row.map_or(Value::Null, |r| text(r, "make")),
            "arch": row.map_or(Value::Null, |r| text(r, "arch")),
            "os": row.map_or(Value::Null, |r| text(r, "os")),
            "chip": row.map_or(Value::Null, |r| text(r, "chip")),
            "ram_gb": row.map_or(Value::Null, |r| text(r, "ram_gb")),
            "disk_gb": row.map_or(Value::Null, |r| text(r, "disk_gb")),
            "nickname": row.map_or(Value::Null, |r| text(r, "nickname")),
            "nickname_aliases": row.map_or(Value::Null, |r| text(r, "nickname_aliases")),
            "location": row.map_or(Value::Null, |r| text(r, "location")),
            "row": row.and_then(cell::Row::lifecycle),
            "prefers": prefers,
            "uuids": uuids,
        }));
    }
    Ok(json!({"complete": view.complete, "machines": out}))
}

/// `accounts`: every account by key and label, its words and which machines prefer it; never its
/// login.
pub(super) fn accounts(paths: &Paths) -> Result<Value> {
    let (view, lab) = lab(paths)?;
    let mut out = Vec::new();
    for (pk, account) in &lab.accounts {
        let preferring: Vec<Value> = lab
            .preferring(pk)
            .into_iter()
            .map(|p| json!({"machine": lab.machine_label(&p.machine), "rank": p.rank}))
            .collect();
        out.push(json!({
            "ac": pk,
            "label": account.label,
            "legacy_label": account.legacy_label,
            "realm": account.realm,
            "status": account.status,
            "row": if account.live { "live" } else { "not live" },
            "preferred_by": preferring,
        }));
    }
    Ok(json!({"complete": view.complete, "accounts": out}))
}

/// `preferences`: every preference row, by machine label and account label, with its rank, its
/// reason and its lifecycle.
pub(super) fn preferences(paths: &Paths) -> Result<Value> {
    let (view, lab) = lab(paths)?;
    let rows = view.project(&PREFERENCE);
    let mut out: Vec<(Value, Value)> = rows
        .rows
        .iter()
        .map(|(pk, row)| {
            let machine = row.get("machine").and_then(Value::as_str).unwrap_or("");
            let account = row.get("account").and_then(Value::as_str).unwrap_or("");
            let entry = json!({
                "pf": pk,
                "machine": lab.machine_label(machine),
                "mc": machine,
                "account": label_of(&lab, account),
                "ac": account,
                "rank": text(row, "rank"),
                "why": text(row, "why"),
                "row": row.lifecycle(),
            });
            (
                json!([lab.machine_label(machine), text(row, "rank"), account]),
                entry,
            )
        })
        .collect();
    out.sort_by_key(|(key, _)| key.to_string());
    Ok(json!({
        "complete": view.complete,
        "preferences": out.into_iter().map(|(_, v)| v).collect::<Vec<_>>(),
    }))
}

/// `mail`: every address by key (never its text), its kind, the mailbox the lab says its codes
/// arrive in (by key and label) and how, its `alias_of` link, and the accounts that sign in with
/// it; then every mailbox by key and label.
pub(super) fn mail(paths: &Paths) -> Result<Value> {
    let (view, lab) = lab(paths)?;
    let addresses = view.project(&ADDRESS);
    let mailboxes = view.project(&MAILBOX);
    let mailbox_label = |pk: &str| mailboxes.rows.get(pk).map(|row| text(row, "label"));
    let accounts = view.project(&ACCOUNT);
    let mut signing_by_address: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
    for (ac, a) in &accounts.rows {
        let login = a.get("login").and_then(Value::as_str);
        let identity = a.get("identity").and_then(Value::as_str);
        if login.is_some() || identity.is_some() {
            let entry = json!({"ac": ac, "label": label_of(&lab, ac)});
            if let Some(addr) = login {
                signing_by_address
                    .entry(addr)
                    .or_default()
                    .push(entry.clone());
            }
            if let Some(addr) = identity
                && Some(addr) != login
            {
                signing_by_address.entry(addr).or_default().push(entry);
            }
        }
    }
    let mut out = Vec::with_capacity(addresses.rows.len());
    for (pk, row) in &addresses.rows {
        let effective = lab.mailbox(pk, &|_| None);
        let signing = signing_by_address
            .get(pk.as_str())
            .map_or(&[][..], Vec::as_slice);
        out.push(json!({
            "ad": pk,
            "kind": text(row, "kind"),
            "provider": text(row, "provider"),
            "status": text(row, "status"),
            "phone": text(row, "phone"),
            "protected": text(row, "protected"),
            "row": row.lifecycle(),
            "alias_of": text(row, "alias_of"),
            "mailbox": effective.as_ref().map(|e| json!({
                "mb": e.mailbox,
                "label": mailbox_label(&e.mailbox),
                "how": match e.how {
                    mail::How::Owner => "owner",
                    mail::How::Observed => "observed",
                    mail::How::Routed => "routed",
                    mail::How::Hint => "hint",
                },
                "via": e.via,
            })),
            "accounts": signing,
        }));
    }
    let boxes: Vec<Value> = mailboxes
        .rows
        .iter()
        .map(|(pk, row)| {
            json!({
                "mb": pk,
                "label": text(row, "label"),
                "service": text(row, "service"),
                "code_path": text(row, "code_path"),
                "row": row.lifecycle(),
            })
        })
        .collect();
    Ok(json!({"complete": view.complete, "addresses": out, "mailboxes": boxes}))
}

/// `phones`: every phone row by key and label, last4, carrier, line type and device.
pub(super) fn phones(paths: &Paths) -> Result<Value> {
    let (view, _) = lab(paths)?;
    let phones = view.project(&PHONE);
    let out: Vec<Value> = phones
        .rows
        .iter()
        .map(|(pk, row)| {
            json!({
                "ph": pk,
                "label": text(row, "label"),
                "last4": text(row, "last4"),
                "carrier": text(row, "carrier"),
                "line_type": text(row, "line_type"),
                "device": text(row, "device"),
                "row": row.lifecycle(),
            })
        })
        .collect();
    Ok(json!({"complete": view.complete, "phones": out}))
}

fn usage_json(lab: &LabView, q: &WindowQuota, now: u64) -> Value {
    let usage = match &q.usage {
        None => Value::Null,
        Some(Usage::Current {
            pct_x10,
            reset,
            read,
            by,
            src,
        }) => json!({
            "state": if reset.is_some() { "current" } else { "reset-unknown" },
            "used_pct_x10": pct_x10,
            "reset": reset,
            "read": at(*read, now),
            "by": mac(lab, by),
            "src": src,
        }),
        Some(Usage::ResetUnread { reset, read, by }) => json!({
            "state": "reset-unread",
            "reset": reset,
            "read": at(*read, now),
            "by": mac(lab, by),
        }),
    };
    json!({
        "pk": q.pk,
        "window": q.window.word(),
        "usage": usage,
        "wall": q.wall.as_ref().map(|w| json!({
            "until": w.until, "seen": at(w.t, now), "by": mac(lab, &w.uuid),
        })),
    })
}

/// `who <account>`: who last used it and on which Mac, from the journal window, its relays and the
/// claims ring's live claims ([`Seen::claim`]), which Macs hold it and which claim it now.
pub(super) fn who(paths: &Paths, args: &Args, io: &Io<'_>, now_ms: u64) -> Result<Value> {
    let [name] = args.operands.as_slice() else {
        return Err(Error::new(
            "name one account: its label, legacy label or key",
        ));
    };
    let (_, lab) = lab(paths)?;
    let pk = account_named(&lab, name)?;
    let seen = seen(paths, io, now_ms / 1000, now_ms)?;
    Ok(json!({
        "ac": pk,
        "label": label_of(&lab, &pk),
        "last_use": seen.last_use.get(&pk).map(|(uuid, t)| {
            let mut out = mac(&lab, uuid);
            if let (Value::Object(o), Value::Object(when)) = (&mut out, at(*t, seen.now)) {
                o.extend(when);
            }
            out
        }),
        "holding": lab.holding(&pk, &seen.places).iter()
            .map(|m| lab.machine_label(m)).collect::<Vec<_>>(),
        "claimed": seen.claims.get(&pk).into_iter().flatten().map(|held| {
            let mut out = mac(&lab, &held.uuid);
            if let Value::Object(o) = &mut out {
                o.insert("kind".into(), json!(held.kind));
                o.insert("since".into(), at(held.since, seen.now));
                o.insert("until".into(), json!(held.until));
            }
            out
        }).collect::<Vec<_>>(),
    }))
}

/// `quota [<account>]`: each window's usage with its age, Mac and source, one account or every
/// account the window has readings of.
pub(super) fn quota(paths: &Paths, args: &Args, io: &Io<'_>, now_ms: u64) -> Result<Value> {
    let (_, lab) = lab(paths)?;
    let seen = seen(paths, io, now_ms / 1000, now_ms)?;
    let wanted: Vec<String> = match args.operands.as_slice() {
        [] => seen
            .accounts()
            .into_iter()
            .filter(|pk| !seen.quota(pk).is_empty())
            .collect(),
        [name] => vec![account_named(&lab, name)?],
        _ => return Err(Error::new("name at most one account")),
    };
    let out: Vec<Value> = wanted
        .iter()
        .map(|pk| {
            json!({
                "ac": pk,
                "label": label_of(&lab, pk),
                "windows": seen.quota(pk).iter().map(|q| usage_json(&lab, q, seen.now)).collect::<Vec<_>>(),
            })
        })
        .collect();
    Ok(json!({"accounts": out}))
}

fn event_json(lab: &LabView, event: &Event) -> Value {
    json!({
        "kind": event.kind.word(),
        "cli": event.cli.as_str(),
        "ac": event.account.map(|a| a.as_str().to_owned()),
        "label": event.account.and_then(|a| label_of(lab, a.as_str())),
        "window": event.window.map(Window::word),
        "used_pct_x10": event.pct_x10,
        "reset": event.reset,
        "until": event.until,
        "src": event.src.word(),
        "why": event.why.map(|w| w.as_str().to_owned()),
        "to": event.to.map(|a| a.as_str().to_owned()),
    })
}

/// `log <account|machine> [--days N]`: the journal's events about an account, or those a machine
/// logged, by (time, Mac, counter), the window's and this Mac's heard archive's ("heard by this
/// Mac"), from `N` days back.
pub(super) fn log(paths: &Paths, args: &Args, io: &Io<'_>, now_ms: u64) -> Result<Value> {
    let [name] = args.operands.as_slice() else {
        return Err(Error::new("name one account or one machine"));
    };
    let days = args.days.unwrap_or(LOG_DAYS).min(LOG_DAYS_MAX);
    let now = now_ms / 1000;
    let since = now.saturating_sub(days.saturating_mul(86_400));
    let (_, lab) = lab(paths)?;
    let account = account_named(&lab, name).ok();
    let machine = machine_named(&lab, name);
    let (account, machine) = match (account, machine) {
        (Some(_), Some(_)) => {
            return Err(Error::new(
                "an account and a machine both go by that name: name it by its key",
            ));
        }
        (None, None) => return Err(Error::new("no account or machine goes by that name")),
        pair => pair,
    };
    let seen = seen(paths, io, since, now_ms)?;
    let events: Vec<Value> = seen
        .events
        .iter()
        .filter(|logged| logged.event.t >= since)
        .filter(|logged| match (&account, &machine) {
            (Some(pk), _) => {
                logged.event.account.is_some_and(|a| a.as_str() == pk)
                    || logged.event.to.is_some_and(|a| a.as_str() == pk)
            }
            (None, Some(mc)) => lab.bound.get(&logged.uuid) == Some(mc),
            (None, None) => false,
        })
        .map(|logged| {
            let mut out = event_json(&lab, &logged.event);
            if let Value::Object(o) = &mut out {
                o.insert("at".into(), at(logged.event.t, now));
                o.insert("by".into(), mac(&lab, &logged.uuid));
                o.insert(
                    "heard".into(),
                    json!(if logged.heard {
                        "heard by this Mac"
                    } else {
                        "window"
                    }),
                );
            }
            out
        })
        .collect();
    Ok(json!({
        "ac": account,
        "mc": machine,
        "days": days,
        "events": events,
    }))
}

/// `signins`: per account, each Mac's latest sign-in word (`in`, `out`, `ended`, `wrong`,
/// `failed`) and when: the matrix of §7.
pub(super) fn signins(paths: &Paths, io: &Io<'_>, now_ms: u64) -> Result<Value> {
    let (_, lab) = lab(paths)?;
    let seen = seen(paths, io, now_ms / 1000, now_ms)?;
    let out: Vec<Value> = seen
        .signins
        .iter()
        .map(|(pk, by)| {
            json!({
                "ac": pk,
                "label": label_of(&lab, pk),
                "machines": by.iter().map(|(uuid, s)| {
                    let mut out = mac(&lab, uuid);
                    if let Value::Object(o) = &mut out {
                        o.insert("state".into(), json!(s.state));
                        o.insert("cli".into(), json!(s.cli));
                        o.insert("at".into(), at(s.t, seen.now));
                    }
                    out
                }).collect::<Vec<_>>(),
            })
        })
        .collect();
    Ok(json!({"accounts": out}))
}

/// One owner change of `collection` at `pk`.
fn put(
    collection: &crate::typed::Collection,
    pk: &str,
    col: &str,
    x: Value,
) -> Result<crate::database::Change> {
    cell::change(
        collection,
        CellKey {
            pk,
            col,
            elem: None,
        },
        &CellValue::owner(collection.version, x),
    )
}

/// The changes that bring `row` (absent: a new row) to `wanted`, skipping every cell that already
/// holds its value.
fn changes(
    collection: &crate::typed::Collection,
    pk: &str,
    row: Option<&cell::Row>,
    wanted: Vec<(&str, Value)>,
) -> Result<Vec<crate::database::Change>> {
    wanted
        .into_iter()
        .filter(|(col, x)| row.and_then(|r| r.get(col)) != Some(x))
        .map(|(col, x)| put(collection, pk, col, x))
        .collect()
}

/// A label `label` live rows of `machines` hold, as a label or an alias, other than `except`.
fn label_held(machines: &Projection, label: &str, except: Option<&str>) -> bool {
    machines.rows.iter().any(|(pk, row)| {
        Some(pk.as_str()) != except
            && row.live()
            && (row.get("label").and_then(Value::as_str) == Some(label)
                || row
                    .fields
                    .get("aliases")
                    .is_some_and(|field| field.has_member(label)))
    })
}

/// `add machine <label> [--model <hw.model>] [--role <word>]`: a planned machine row, keyed from
/// its label (the birth identity a sheet placeholder takes too), until the Mac registers itself
/// and adoption binds it (§6). A label or alias another live machine holds is refused; a label
/// whose birth key a row already holds (a retired Mac) gets the first free `<label>/<n>` birth,
/// so a replacement Mac has a row of its own. A model the registry holds no row of is refused
/// rather than left dangling.
fn add_machine(paths: &Paths, args: &Args) -> Result<Value> {
    let [_, label] = args.operands.as_slice() else {
        return Err(Error::new("add machine takes one label: `add machine m87`"));
    };
    let label = Label::parse(label)
        .map_err(|_| Error::new("a machine label is m<N> (`m87`)"))?
        .canonical();
    let handle = Handle::at(paths.clone(), &att::REGISTRY);
    let (pk, applied) = handle.commit_owner(|view| {
        let machines = view.project(&MACHINE);
        if label_held(&machines, &label, None) {
            return Err(Error::new(
                "a machine already goes by that label: relabel it or retire it first",
            ));
        }
        // The label's birth key, or the first free `<label>/<n>` birth (`planned_machine_pk`).
        let pk = keys::planned_machine_pk(&label, |pk| machines.rows.contains_key(pk))
            .ok_or_else(|| Error::new("no machine key is free"))?;
        let mut wanted = vec![
            ("label", json!(label)),
            ("status", json!("planned")),
            (LIFECYCLE, json!("live")),
        ];
        if let Some(model) = &args.model {
            let model = keys::model_pk(model)?;
            if !view
                .project(&MODEL)
                .rows
                .get(&model)
                .is_some_and(cell::Row::live)
            {
                return Err(Error::new(
                    "the registry holds no such model yet: a Mac of it registers it, or leave out --model",
                ));
            }
            wanted.push(("model", json!(model)));
        }
        if let Some(role) = &args.role {
            wanted.push(("role", json!(role)));
        }
        let rows = vec![changes(&MACHINE, &pk, None, wanted)?];
        Ok((pk, rows))
    })?;
    Ok(json!({"mc": pk, "label": label, "status": "planned", "written": applied.cells}))
}

/// `add account <provider|realm>`: an account row of that realm for the address read from stdin
/// (never argv, §8), born `pending` under the canonical account key, with the address row it
/// signs in with when the registry has none. The address is never printed back. An account the
/// registry already holds is refused.
fn add_account(paths: &Paths, args: &Args, io: &mut Io<'_>) -> Result<Value> {
    let [_, realm] = args.operands.as_slice() else {
        return Err(Error::new(
            "add account takes one provider or realm (`add account claude`); the address comes from stdin",
        ));
    };
    if realm.contains('@') {
        return Err(Error::new(
            "the address comes from stdin, never the command line",
        ));
    }
    let realm = keys::canonical_realm(realm)
        .ok_or_else(|| Error::new("no provider or realm goes by that name"))?;
    let provider =
        keys::provider_of_realm(realm).ok_or_else(|| Error::new("no provider has that realm"))?;
    let line = (io.stdin)()?;
    let addr = keys::address(line.lines().next().unwrap_or_default())?;
    let address = keys::address_pk(&addr)?;
    let account = keys::account_pk(realm, &keys::claims_email(None, Some(&addr))?)?;
    let domain = addr
        .rsplit_once('@')
        .map(|(_, domain)| domain.to_owned())
        .unwrap_or_default();
    let handle = Handle::at(paths.clone(), &att::REGISTRY);
    let (born, applied) = handle.commit_owner(|view| {
        if view.project(&ACCOUNT).rows.contains_key(&account) {
            return Err(Error::new(
                "the registry already holds that account: edit it with `aterm db alab edit accounts`",
            ));
        }
        let addresses = view.project(&ADDRESS);
        let mut rows = Vec::new();
        let born = !addresses.rows.contains_key(&address);
        if born {
            rows.push(changes(
                &ADDRESS,
                &address,
                None,
                vec![
                    ("addr", json!(addr)),
                    ("domain", json!(domain)),
                    ("origin", json!("owner")),
                    (LIFECYCLE, json!("live")),
                ],
            )?);
        } else if !addresses.rows[&address].live() {
            return Err(Error::new(
                "that address's row is retired: bring it back in `aterm db alab edit mail` first",
            ));
        }
        rows.push(changes(
            &ACCOUNT,
            &account,
            None,
            vec![
                ("provider", json!(provider)),
                ("realm", json!(realm)),
                ("login", json!(address)),
                ("status", json!("pending")),
                (LIFECYCLE, json!("live")),
            ],
        )?);
        Ok((born, rows))
    })?;
    let label = keys::default_account_label(realm, &account).ok();
    Ok(json!({
        "ac": account,
        "label": label,
        "realm": realm,
        "ad": address,
        "address_born": born,
        "status": "pending",
        "written": applied.cells,
    }))
}

/// `add machine …` or `add account …`.
pub(super) fn add(paths: &Paths, args: &Args, io: &mut Io<'_>) -> Result<Value> {
    match args.operands.first().map(String::as_str) {
        Some("machine") => add_machine(paths, args),
        Some("account") => {
            if args.model.is_some() || args.role.is_some() {
                return Err(Error::new("--model and --role belong to `add machine`"));
            }
            add_account(paths, args, io)
        }
        _ => Err(Error::new(
            "add takes `machine <label>` or `account <provider>`",
        )),
    }
}

/// `retire account|machine <name> [--why <text>]`: the row's lifecycle to `retired`, the reason
/// added to its note (behind the importer's note guard, the owner's earlier note kept before it,
/// refused before any write where the two would pass the note's bytes). The row
/// stays, its key and history with it; no preference, login or Mac's roster row moves.
pub(super) fn retire(paths: &Paths, args: &Args) -> Result<Value> {
    let [what, name] = args.operands.as_slice() else {
        return Err(Error::new(
            "retire takes `account <name>` or `machine <name>`",
        ));
    };
    let collection = match what.as_str() {
        "account" => &ACCOUNT,
        "machine" => &MACHINE,
        _ => {
            return Err(Error::new(
                "retire takes `account <name>` or `machine <name>`",
            ));
        }
    };
    if name.contains('@') {
        return Err(Error::new(
            "an account is named by its label, legacy label or key, never its address",
        ));
    }
    let handle = Handle::at(paths.clone(), &att::REGISTRY);
    let (pk, applied) = handle.commit_owner(|view| {
        let rows = view.project(collection);
        let pk = if collection.name == "account" {
            named_row("account", &rows, name, false, |pk, row| {
                registry::account_label(pk, row).as_deref() == Some(name.as_str())
                    || row.get("legacy_label").and_then(Value::as_str) == Some(name.as_str())
            })?
        } else {
            let wanted = Label::parse(name).ok().map(|label| label.canonical());
            named_row("machine", &rows, name, false, |_, row| {
                wanted.as_deref().is_some_and(|label| {
                    row.get("label").and_then(Value::as_str) == Some(label)
                        || row
                            .fields
                            .get("aliases")
                            .is_some_and(|field| field.has_member(label))
                })
            })?
        };
        let row = rows.rows.get(&pk);
        let mut wanted = vec![(LIFECYCLE, json!("retired"))];
        if let Some(why) = &args.why {
            // The importer's note guard: no word that names a secret, nothing password-shaped.
            guard::note_cell(why, NOTE_BYTES).map_err(|refusal| {
                Error::new(&format!("--why is not written: {}", refusal.rule()))
            })?;
            let held = row.and_then(|r| r.get("note")).and_then(Value::as_str);
            let note = match held {
                Some(held) if !held.is_empty() => format!("{held}; retired: {why}"),
                _ => format!("retired: {why}"),
            };
            if note.len() > NOTE_BYTES {
                return Err(Error::new(&format!(
                    "the note with the reason would pass its {NOTE_BYTES} bytes: shorten --why, or the note in `aterm db alab edit {}`",
                    if collection.name == "machine" { "machines" } else { "accounts" }
                )));
            }
            wanted.push(("note", json!(note)));
        }
        let rows = vec![changes(collection, &pk, row, wanted)?];
        Ok((pk, rows))
    })?;
    Ok(json!({"pk": pk, "row": "retired", "written": applied.cells}))
}

/// `relabel <machine> <new label>`: the machine's label becomes the new one and the old one an
/// alias, so every name a Mac or a sheet already uses still finds the row (§1). A label or alias
/// another live machine holds is refused.
pub(super) fn relabel(paths: &Paths, args: &Args) -> Result<Value> {
    let [old, new] = args.operands.as_slice() else {
        return Err(Error::new(
            "relabel takes the machine and its new label: `relabel m40 m107`",
        ));
    };
    let new = Label::parse(new)
        .map_err(|_| Error::new("a machine label is m<N> (`m107`)"))?
        .canonical();
    let handle = Handle::at(paths.clone(), &att::REGISTRY);
    let ((pk, from), applied) = handle.commit_owner(|view| {
        let machines = view.project(&MACHINE);
        let wanted = Label::parse(old).ok().map(|label| label.canonical());
        let pk = named_row("machine", &machines, old, true, |_, row| {
            wanted.as_deref().is_some_and(|label| {
                row.get("label").and_then(Value::as_str) == Some(label)
                    || row
                        .fields
                        .get("aliases")
                        .is_some_and(|field| field.has_member(label))
            })
        })?;
        if label_held(&machines, &new, Some(&pk)) {
            return Err(Error::new("another live machine goes by that label"));
        }
        let row = machines.rows.get(&pk);
        let from = row
            .and_then(|r| r.get("label"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                Error::new("that machine's label is conflicted: settle it in `aterm db alab edit machines`")
            })?
            .to_owned();
        let mut record = changes(&MACHINE, &pk, row, vec![("label", json!(new))])?;
        if from != new
            && !row
                .and_then(|r| r.fields.get("aliases"))
                .is_some_and(|field| field.has_member(from.as_str()))
        {
            record.push(cell::change(
                &MACHINE,
                CellKey {
                    pk: &pk,
                    col: "aliases",
                    elem: Some(&from),
                },
                &CellValue::owner(MACHINE.version, json!(true)),
            )?);
        }
        let rows = if record.is_empty() { Vec::new() } else { vec![record] };
        Ok(((pk, from), rows))
    })?;
    Ok(json!({"mc": pk, "from": from, "label": new, "written": applied.cells}))
}

/// The collection `edit <name>` opens.
fn edited(name: &str) -> Result<&'static crate::typed::Collection> {
    match name {
        "accounts" => Ok(&ACCOUNT),
        "preferences" => Ok(&PREFERENCE),
        "machines" => Ok(&MACHINE),
        "mail" => Ok(&ADDRESS),
        _ => Err(Error::new(
            "edit takes accounts, preferences, machines or mail",
        )),
    }
}

/// `edit accounts|preferences|machines|mail [--show-addresses]`: the collection as a grid in
/// amacs, written back through the grid's three-way save (`att::edit`): a cell is written only
/// while its candidates are the exported ones, a cell that changed meanwhile reopens with both
/// values, a deleted row retires (a preference ends), a new row is born under its derived key.
/// Addresses show masked unless `--show-addresses`.
pub(super) fn edit(paths: &Paths, args: &Args, io: &mut Io<'_>) -> Result<Value> {
    let [name] = args.operands.as_slice() else {
        return Err(Error::new(
            "edit takes accounts, preferences, machines or mail",
        ));
    };
    let collection = edited(name)?;
    let handle = Handle::at(paths.clone(), &att::REGISTRY);
    let mut problems = Vec::new();
    let mut report = |line: &str| problems.push(line.to_owned());
    let edited = att::edit_grid(
        &handle,
        collection,
        &[],
        args.reveal,
        None,
        &mut *io.open,
        &mut report,
    )?;
    Ok(json!({
        "collection": collection.name,
        "rounds": edited.rounds,
        "records": edited.applied.iter().map(|a| a.records).sum::<usize>(),
        "cells": edited.applied.iter().map(|a| a.cells).sum::<usize>(),
        "born": edited.applied.iter().map(|a| a.born.len()).sum::<usize>(),
        "retired": edited.applied.iter().map(|a| a.retired.len()).sum::<usize>(),
        "abandoned": edited.abandoned,
        "problems": problems,
    }))
}
