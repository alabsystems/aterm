// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE NEED LEDGER: `~/.config/aterm/secrets/needs.toml`, 0600, one row per `(want, purpose)`,
//! read and written under `needs.lock`. Its folder is one no vault entry can name
//! (`text::valid_entry_name`), so the vault never overwrites it. Rows carry names, states and
//! the lines `need ls` prints, never a value.

use super::{
    Act, Facts, GhProfile, Need, Purpose, Reason, RefusalClaim, Retirement, State, Wait, Want, act,
    settle,
};
use crate::client::{Paths, toml_str};
use crate::{Error, fsx};

/// The ledger's file name, beside `source.toml`.
pub const FILE: &str = "needs.toml";

/// The lock the ledger's readers and writers take.
const LOCK: &str = "needs.lock";

/// The most rows kept: past it, the oldest ended rows go first.
const MAX_ROWS: usize = 64;

/// The largest ledger read.
const MAX_BYTES: usize = 1 << 20;

/// Format 3 requires agh's development consumer to confirm its own delivery; format 4 holds no
/// person's ask. Older writers reject it rather than misread its rows.
const FORMAT: i64 = 4;

/// The ledger's lock, released on drop.
struct Lock(std::fs::File);

impl Lock {
    fn take(paths: &Paths) -> crate::Result<Self> {
        fsx::ensure_private_dir(&paths.dir)?;
        let path = paths.dir.join(LOCK);
        let ledger_lock_file: std::fs::File = fsx::open_lock(&path)?;
        #[expect(
            clippy::disallowed_methods,
            reason = "the need ledger's lock, whose Drop releases it (File::unlock)"
        )]
        ledger_lock_file
            .lock()
            .map_err(|e| Error::new(format!("{}: {e}", path.display())))?;
        Ok(Self(ledger_lock_file))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn hex(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(bytes)
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    aterm_codec::hex::decode_vec(text.as_bytes())
}

/// Every row, oldest first, interpreting older completion claims without writing them back.
/// A missing ledger holds none.
pub(crate) fn load(paths: &Paths) -> crate::Result<Vec<Need>> {
    let path = paths.dir.join(FILE);
    let Some(bytes) = fsx::read_private(&path, MAX_BYTES)? else {
        return Ok(Vec::new());
    };
    let text = bytes
        .as_str()
        .ok_or_else(|| Error::new(format!("{}: not UTF-8", path.display())))?;
    let damaged = || {
        Error::new(format!(
            "{}: not a need ledger aterm wrote; move it aside and raise the needs again",
            path.display()
        ))
    };
    let Ok(aterm_toml::Value::Table(table)) = aterm_toml::from_str::<aterm_toml::Value>(text)
    else {
        return Err(damaged());
    };
    let Some(rows) = table.get("need") else {
        return Ok(Vec::new());
    };
    let (rows, format): (&[_], i64) = match rows {
        aterm_toml::Value::Array(rows) => (rows, 1),
        aterm_toml::Value::Table(container) => {
            let format = container
                .get("format")
                .and_then(aterm_toml::Value::as_integer)
                .filter(|format| matches!(*format, 2 | 3 | FORMAT))
                .ok_or_else(damaged)?;
            let rows = match container.get("rows") {
                None => &[][..],
                Some(rows) => rows.as_array().ok_or_else(damaged)?.as_slice(),
            };
            (rows, format)
        }
        _ => return Err(damaged()),
    };
    let mut resolved = std::collections::BTreeMap::new();
    rows.iter()
        .map(|row| {
            let table = row.as_table().ok_or_else(damaged)?;
            let mut need = row_of(table).ok_or_else(damaged)?;
            let present = ["refused", "refused_legacy", "refused_version"]
                .iter()
                .filter(|key| table.get(key).is_some())
                .count();
            if present > 1
                || (format == 1
                    && (table.get("refused_legacy").is_some()
                        || table.get("refused_version").is_some()))
            {
                return Err(damaged());
            }
            if table.get("refused").is_some() && need.refused.is_none() {
                return Err(damaged());
            }
            if let Some(value) = table.get("refused_legacy") {
                need.refused = Some(RefusalClaim::LegacySequence(
                    value
                        .as_str()
                        .ok_or_else(damaged)?
                        .parse()
                        .map_err(|_| damaged())?,
                ));
            }
            if let Some(value) = table.get("refused_version") {
                let text = value
                    .as_str()
                    .filter(|text| text.len() <= 2048)
                    .ok_or_else(damaged)?;
                let bytes = unhex(text).ok_or_else(damaged)?;
                if !resolved.contains_key(&bytes) {
                    let claim = crate::client::version::resolve_reference(paths, &bytes)?;
                    resolved.insert(bytes.clone(), claim);
                }
                need.refused = Some(RefusalClaim::Authenticated(resolved[&bytes].clone()));
            }
            // Before the consumer was registered, delivery alone could mark agh.dev
            // fulfilled. No stored field distinguishes that from a real consumer check.
            // Preserve its identity and evidence, but require confirmation once. A peek
            // only interprets the old format; an actual ledger mutation publishes FORMAT.
            if format < 3
                && need.want == Want::Gh(GhProfile::Dev)
                && need.purpose == Purpose::AghDev
                && need.state == State::Fulfilled
            {
                need.state = State::Published;
                need.wait = Wait::Requester;
                need.line = "published: the development credential awaits its consumer's current check after an older completion record".into();
            }
            Ok(need)
        })
        .collect()
}

fn row_of(t: &aterm_toml::Table) -> Option<Need> {
    let s = |key: &str| t.get(key).and_then(aterm_toml::Value::as_str);
    let n = |key: &str| {
        t.get(key)
            .and_then(aterm_toml::Value::as_integer)
            .and_then(|n| u64::try_from(n).ok())
    };
    let b = |key: &str| t.get(key).and_then(aterm_toml::Value::as_bool);
    let id = <[u8; 16]>::try_from(unhex(s("id")?)?.as_slice()).ok()?;
    let retired = match s("reason") {
        Some(word) => Some(Retirement {
            reason: Reason::parse(word)?,
            evidence: s("evidence").unwrap_or_default().to_owned(),
            // An earlier build kept a note as typed, control characters and all, which the
            // table refuses: read back, a note is cleaned as one entering the ledger is.
            note: super::note_of(s("note").unwrap_or_default()),
            generation: n("generation"),
        }),
        None => None,
    };
    // Before format 4 a need no Mac held could await a person. It waits on the lab's Macs now:
    // a posted one is open on the table, and one never posted is still this Mac's to post.
    let asked = s("state")? == "awaiting-person";
    let (state, wait) = match (asked, b("posted")?) {
        (true, true) => (State::Open, Wait::Holders),
        (true, false) => (State::Local, Wait::Requester),
        (false, _) => (State::parse(s("state")?)?, Wait::parse(s("wait")?)?),
    };
    let line = if asked {
        "waits on the lab's Macs".to_owned()
    } else {
        s("line").unwrap_or_default().to_owned()
    };
    Some(Need {
        id,
        want: Want::parse(s("want")?).ok()?,
        purpose: Purpose::parse(s("purpose")?)?,
        state,
        wait,
        retired,
        raised: n("raised")?,
        updated: n("updated")?,
        raises: u32::try_from(n("raises")?).ok()?,
        refused: n("refused").map(RefusalClaim::LegacySequence),
        needed: b("needed")?,
        got: b("got")?,
        got_authorized: b("got_authorized")?,
        raised_again_on_change: b("raised_again_on_change"),
        posted: b("posted")?,
        custodian_silent: b("custodian_silent").unwrap_or(false),
        record: match s("record") {
            Some(text) => Some(unhex(text)?),
            None => None,
        },
        line,
        seq: n("seq"),
        exposed: match t.get("exposed") {
            None => Vec::new(),
            Some(sections) => sections
                .as_array()?
                .iter()
                .map(|section| section.as_str().map(str::to_owned))
                .collect::<Option<_>>()?,
        },
    })
}

fn int(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

fn render(paths: &Paths, rows: &[Need]) -> crate::Result<String> {
    let mut text = format!(
        "# aterm secrets need ledger (docs/DESIGN-credential-requests-2026-10-27.md).\n\
         # Written by aterm; names, states and explanations only, never a value.\n\n[need]\nformat = {FORMAT}\n",
    );
    for need in rows {
        text.push_str("\n[[need.rows]]\n");
        let mut put = |key: &str, value: String| text.push_str(&format!("{key} = {value}\n"));
        put("id", toml_str(&hex(&need.id)));
        put("want", toml_str(&need.want.key()));
        put("purpose", toml_str(need.purpose.id()));
        put("state", toml_str(need.state.word()));
        put("wait", toml_str(need.wait.word()));
        put("raised", int(need.raised).to_string());
        put("updated", int(need.updated).to_string());
        put("raises", need.raises.to_string());
        put("needed", need.needed.to_string());
        put("got", need.got.to_string());
        put("got_authorized", need.got_authorized.to_string());
        put("posted", need.posted.to_string());
        put("line", toml_str(&need.line));
        if let Some(seq) = need.seq {
            put("seq", int(seq).to_string());
        }
        if let Some(refused) = &need.refused {
            match refused {
                RefusalClaim::LegacySequence(seq) => {
                    put("refused_legacy", toml_str(&seq.to_string()))
                }
                RefusalClaim::Authenticated(claim) => {
                    let reference = crate::client::version::retained_reference(paths, claim)?;
                    put("refused_version", toml_str(&hex(&reference)));
                }
            }
        }
        if let Some(changed) = need.raised_again_on_change {
            put("raised_again_on_change", changed.to_string());
        }
        if let Some(record) = &need.record {
            put("record", toml_str(&hex(record)));
        }
        if need.custodian_silent {
            put("custodian_silent", "true".to_owned());
        }
        if !need.exposed.is_empty() {
            let sections: Vec<String> = need.exposed.iter().map(|s| toml_str(s)).collect();
            put("exposed", format!("[{}]", sections.join(", ")));
        }
        if let Some(retired) = &need.retired {
            put("reason", toml_str(retired.reason.word()));
            put("evidence", toml_str(&retired.evidence));
            put("note", toml_str(&retired.note));
            if let Some(generation) = retired.generation {
                put("generation", int(generation).to_string());
            }
        }
    }
    if text.len() > MAX_BYTES {
        return Err(Error::new("the need ledger exceeds its bounded storage"));
    }
    Ok(text)
}

/// Write every row, keeping at most [`MAX_ROWS`] (the oldest ended ones go first). A row with
/// no request id (a random source that failed) is refused.
pub(crate) fn save(paths: &Paths, rows: &mut Vec<Need>) -> crate::Result<()> {
    if rows.iter().any(|need| need.id == [0; 16]) {
        return Err(Error::new(
            "a need has no request id: the random source failed, so nothing was written",
        ));
    }
    while rows.len() > MAX_ROWS {
        let Some(oldest) = rows
            .iter()
            .enumerate()
            .filter(|(_, need)| need.state.ended())
            .min_by_key(|(_, need)| need.updated)
            .map(|(at, _)| at)
        else {
            break;
        };
        rows.remove(oldest);
    }
    fsx::ensure_private_dir(&paths.dir)?;
    fsx::write_atomic(
        &paths.dir.join(FILE),
        render(paths, rows)?.as_bytes(),
        0o600,
    )
}

fn now() -> u64 {
    aterm_time::unix_secs().unwrap_or(0)
}

/// Run `change` on the ledger's rows under its lock and this Mac's current [`Facts`], saving
/// what it leaves.
///
/// # Errors
/// The ledger or the vault state cannot be read, or the ledger cannot be written.
pub fn with_ledger<T>(
    paths: &Paths,
    change: impl FnOnce(&mut Vec<Need>, &Facts) -> crate::Result<T>,
) -> crate::Result<T> {
    let _lock = Lock::take(paths)?;
    let facts = super::facts(paths, now())?;
    with_rows(paths, &facts, change)
}

fn with_rows<T>(
    paths: &Paths,
    facts: &Facts,
    change: impl FnOnce(&mut Vec<Need>, &Facts) -> crate::Result<T>,
) -> crate::Result<T> {
    let mut rows = load(paths)?;
    let before = rows.clone();
    let out = change(&mut rows, facts)?;
    if rows != before {
        save(paths, &mut rows)?;
    }
    Ok(out)
}

fn new_id() -> crate::Result<[u8; 16]> {
    let mut id = [0; 16];
    aterm_uds::rand::fill(&mut id)
        .map_err(|e| Error::new(format!("no request id: the random source failed ({e})")))?;
    Ok(id)
}

/// Raise `want` for `purpose` (§9.1): idempotent by `(want, purpose)`. A need that has not
/// ended moves as this Mac now observes; one retired `not-needed`, `lapsed`, `not-authorized`
/// at an older policy generation or `not-held` that a member now holds is raised again under a
/// new request id; any other retirement stands, with its reason. A fulfilled need raised again
/// means the value vanished or its consumer refused it: a new request, `refused` naming the
/// vault seq it had.
///
/// # Errors
/// The ledger or the vault state cannot be read or written, or no request id can be drawn.
pub fn raise(paths: &Paths, want: Want, purpose: Purpose) -> crate::Result<Need> {
    raise_with(paths, want, purpose, None)
}

/// File each vault entry of `exposures`, `(name, sections)`, for a new value under one lock: a
/// stolen Mac read it in those sections ([`Purpose::StolenRotation`]). Idempotent as [`raise`]:
/// an open filing gains the sections, one a set closed is filed again.
///
/// # Errors
/// As [`raise`].
pub fn raise_rotations(
    paths: &Paths,
    exposures: &[(String, Vec<String>)],
) -> crate::Result<Vec<Need>> {
    with_ledger(paths, |rows, facts| {
        exposures
            .iter()
            .map(|(name, sections)| {
                let want = Want::Entry(name.clone());
                let need = raise_in(rows, facts, want.clone(), Purpose::StolenRotation, None)?;
                let Some(row) = rows
                    .iter_mut()
                    .find(|row| row.want == want && row.purpose == Purpose::StolenRotation)
                else {
                    return Ok(need);
                };
                if row.state.ended() {
                    return Ok(need);
                }
                row.exposed.extend(sections.iter().cloned());
                row.exposed.sort();
                row.exposed.dedup();
                *row = settle(row, facts);
                Ok(row.clone())
            })
            .collect()
    })
}

/// A new value of the entry `name` was published in the vault section `section` (`[everyone]`,
/// `[m3 m22]`): the open rotation filing for it, if any, no longer counts that section, and ends
/// `not-needed` once no section the stolen Mac read it in holds the old value. `None` when none
/// is open; the ledger is read alone, under no lock, to find that out.
///
/// # Errors
/// The ledger or the vault state cannot be read or written.
pub fn rotated(paths: &Paths, name: &str, section: &str) -> crate::Result<Option<Need>> {
    let want = Want::Entry(name.to_owned());
    let open = |row: &Need| {
        row.want == want && row.purpose == Purpose::StolenRotation && !row.state.ended()
    };
    if !load(paths)?.iter().any(open) {
        return Ok(None);
    }
    with_ledger(paths, |rows, facts| {
        let Some(row) = rows.iter_mut().find(|row| open(row)) else {
            return Ok(None);
        };
        row.exposed.retain(|held| held != section);
        if !row.exposed.is_empty() {
            *row = settle(row, facts);
            return Ok(Some(row.clone()));
        }
        let Some(next) = act(row, facts, &Act::Unneeded) else {
            return Ok(None);
        };
        *row = Need {
            line: format!("rotated: a new value of {name} was published"),
            ..next
        };
        Ok(Some(row.clone()))
    })
}

/// [`raise`] for a consumer that refused the value the vault gave it at `seq` (§6.2).
///
/// # Errors
/// As [`raise`].
pub fn raise_refused(paths: &Paths, want: Want, purpose: Purpose, seq: u64) -> crate::Result<Need> {
    raise_with(
        paths,
        want,
        purpose,
        Some(RefusalClaim::LegacySequence(seq)),
    )
}

/// Raise a need only while its consumer's local admission still holds, including after
/// waiting for the ledger lock and immediately before saving. `refused` is the same
/// legacy sequence as [`raise_refused`]. `admitted` must not wait for a vault lock,
/// reenter this ledger, run a subprocess, use the network or perform effects.
///
/// # Errors
/// As [`raise`], or the consumer withdrew its admission.
pub fn raise_with_admission(
    paths: &Paths,
    want: Want,
    purpose: Purpose,
    refused: Option<u64>,
    admitted: impl Fn() -> crate::Result<()>,
) -> crate::Result<Need> {
    admitted()?;
    with_ledger(paths, |rows, facts| {
        admitted()?;
        let raised = raise_in(
            rows,
            facts,
            want,
            purpose,
            refused.map(RefusalClaim::LegacySequence),
        )?;
        admitted()?;
        Ok(raised)
    })
}

/// Record a consumer refusal only while the captured paired materialization's exact
/// source/version and the caller's local admission still hold. The need lock precedes
/// the shared materialization guard; the existing policy settles the same canonical raise.
/// `current` must perform local reads only, without subprocesses, network, ledger reentry
/// or mutation. It runs before custody observation, then under both locks before mutation
/// and before the atomic save.
/// # Errors
/// The ledger, captured source/version or current consumer admission cannot be read safely.
pub fn raise_refused_from_snapshot(
    paths: &Paths,
    want: Want,
    purpose: Purpose,
    snapshot: &crate::client::MaterializationSnapshot,
    current: impl Fn() -> crate::Result<()>,
) -> crate::Result<Need> {
    let entries = want.entries();
    with_snapshot_ledger(
        paths,
        &entries,
        snapshot,
        true,
        true,
        current,
        |rows, facts| {
            let version = facts
                .sight
                .as_ref()
                .and_then(|sight| sight.version.as_ref())
                .ok_or_else(|| Error::new("the refusal has no authenticated source version"))?;
            raise_in(
                rows,
                facts,
                want,
                purpose,
                Some(RefusalClaim::Authenticated(version.claim().clone())),
            )
        },
    )
}

fn with_snapshot_ledger<T>(
    paths: &Paths,
    entries: &[String],
    snapshot: &crate::client::MaterializationSnapshot,
    require_signed: bool,
    retain: bool,
    current: impl Fn() -> crate::Result<()>,
    change: impl FnOnce(&mut Vec<Need>, &Facts) -> crate::Result<T>,
) -> crate::Result<T> {
    let _lock = Lock::take(paths)?;
    current()?;
    let commons = super::observed_commons(paths);
    if retain {
        snapshot.retain_for_refusal(paths)?;
        current()?;
    }
    snapshot.at_current_version(paths, entries, || {
        let facts = super::facts_with_commons(paths, now(), || commons)?;
        with_rows(paths, &facts, |rows, facts| {
            current()?;
            if require_signed
                && facts
                    .sight
                    .as_ref()
                    .is_none_or(|sight| sight.seq != snapshot.sequence())
            {
                return Err(Error::new(
                    "the credential refusal has no matching signed version",
                ));
            }
            let result = change(rows, facts)?;
            current()?;
            Ok(result)
        })
    })
}

fn raise_with(
    paths: &Paths,
    want: Want,
    purpose: Purpose,
    refused: Option<RefusalClaim>,
) -> crate::Result<Need> {
    with_ledger(paths, |rows, facts| {
        raise_in(rows, facts, want, purpose, refused)
    })
}

fn raise_in(
    rows: &mut Vec<Need>,
    facts: &Facts,
    want: Want,
    purpose: Purpose,
    refused: Option<RefusalClaim>,
) -> crate::Result<Need> {
    let at = rows
        .iter()
        .position(|need| need.want == want && need.purpose == purpose);
    let next = match at.map(|at| rows[at].clone()) {
        None => {
            let mut need = super::fresh(new_id()?, want, purpose, facts);
            need.refused = refused;
            settle(&need, facts)
        }
        Some(old)
            if old.state == State::Fulfilled
                || (!old.state.ended()
                    && advances(facts, refused.as_ref(), old.refused.as_ref())) =>
        {
            let mut need = super::fresh(new_id()?, want, purpose, facts);
            need.raises = old.raises.saturating_add(1);
            let incoming = refused.or_else(|| old.seq.map(RefusalClaim::LegacySequence));
            need.refused = later_floor(facts, incoming, old.refused);
            settle(&need, facts)
        }
        Some(old) if old.state.ended() => {
            let again = act(&old, facts, &Act::NeedAgain).unwrap_or(old);
            match reraise(&again, facts)? {
                Some(mut raised) => {
                    raised.refused = later_floor(facts, refused, raised.refused);
                    settle(&raised, facts)
                }
                None => again,
            }
        }
        Some(old) => settle(&old, facts),
    };
    match at {
        Some(at) => rows[at] = next.clone(),
        None => rows.push(next.clone()),
    }
    Ok(next)
}

fn advances(facts: &Facts, next: Option<&RefusalClaim>, before: Option<&RefusalClaim>) -> bool {
    match (next, before) {
        (None, _) => false,
        (Some(_), None) => true,
        (Some(RefusalClaim::LegacySequence(next)), Some(RefusalClaim::LegacySequence(old))) => {
            next > old
        }
        (Some(RefusalClaim::Authenticated(next)), Some(RefusalClaim::LegacySequence(old))) => {
            facts.sight.as_ref().is_some_and(|sight| {
                sight.version.as_ref().is_some_and(|current| {
                    current.claim() == next
                        && (current.envelope_sequence().is_some_and(|seq| seq > *old)
                            || (*old == 0
                                && sight.seq == 0
                                && next.log().is_some()
                                && current.envelope_sequence().is_none()))
                })
            })
        }
        (Some(RefusalClaim::Authenticated(next)), Some(RefusalClaim::Authenticated(old))) => facts
            .sight
            .as_ref()
            .and_then(|sight| sight.version.as_ref())
            .is_some_and(|current| {
                current.relation_between(next, old)
                    == crate::client::version::VersionRelation::Advanced
                    || (current.claim() == next && current.refines_for_recovery(old))
            }),
        _ => false,
    }
}

fn later_floor(
    facts: &Facts,
    next: Option<RefusalClaim>,
    before: Option<RefusalClaim>,
) -> Option<RefusalClaim> {
    if advances(facts, next.as_ref(), before.as_ref()) {
        next
    } else {
        before.or(next)
    }
}

/// `need` raised again under a new request id, when it ended for a reason whose condition has
/// changed while it is still needed ([`Act::Reraise`]); `None` otherwise.
fn reraise(need: &Need, facts: &Facts) -> crate::Result<Option<Need>> {
    let Some(mut raised) = act(need, facts, &Act::Reraise) else {
        return Ok(None);
    };
    if raised.id == [0; 16] {
        raised.id = new_id()?;
    }
    Ok(Some(raised))
}

/// `purpose`'s own check found `want` (§4: the consumer's check is what fulfils a need): a
/// need the vault delivered is fulfilled, any other that has not ended is retired
/// `not-needed`. `None` when no such need is in the ledger.
///
/// # Errors
/// The ledger or the vault state cannot be read or written.
pub fn satisfied(paths: &Paths, want: &Want, purpose: Purpose) -> crate::Result<Option<Need>> {
    with_ledger(paths, |rows, facts| satisfy_in(rows, facts, want, purpose))
}

/// The development consumer found no selected repository work in this exact inventory
/// snapshot. Retire only its unchanged live request as not needed; this neither accepts a
/// credential nor changes its refusal floor. The caller must validate the empty selection
/// and its current account, source and choices through `current`, whose local-read-only
/// contract is the same as [`raise_refused_from_snapshot`].
/// # Errors
/// The inventory snapshot, current admission or expected request ID/floor no longer holds.
pub fn unneeded_from_snapshot(
    paths: &Paths,
    expected: &Need,
    snapshot: &crate::client::MaterializationSnapshot,
    current: impl Fn() -> crate::Result<()>,
) -> crate::Result<()> {
    if expected.want != Want::Gh(GhProfile::Dev) || expected.purpose != Purpose::AghDev {
        return Err(Error::new(
            "the inventory belongs to no development credential request",
        ));
    }
    with_snapshot_ledger(
        paths,
        &[crate::entry_class::GITHUB_REPOSITORIES.to_owned()],
        snapshot,
        false,
        false,
        current,
        |rows, facts| {
            let row = rows
                .iter_mut()
                .find(|row| row.want == expected.want && row.purpose == expected.purpose)
                .ok_or_else(|| Error::new("the development credential request changed"))?;
            if row.id != expected.id || row.refused != expected.refused || row.state.ended() {
                return Err(Error::new(
                    "the development credential request or refusal changed",
                ));
            }
            *row = act(row, facts, &Act::Unneeded).ok_or_else(|| {
                Error::new("the development credential request cannot retire as unneeded")
            })?;
            Ok(())
        },
    )
}

/// The consumer accepted its paired credential. An explicit refusal additionally requires
/// prior provider acceptance at a later signed version or an exact-envelope witness
/// refinement; ordinary local consumption may retire an unneeded request without claiming
/// signed or protocol delivery.
/// A prior request's acceptance cannot settle a concurrent replacement or new refusal.
/// `current` has the same local-read-only contract as [`raise_refused_from_snapshot`].
/// # Errors
/// The captured source/version, request ID/floor, policy or local admission no longer holds.
pub fn acknowledge_from_snapshot(
    paths: &Paths,
    expected: &Need,
    snapshot: &crate::client::MaterializationSnapshot,
    current: impl Fn() -> crate::Result<()>,
) -> crate::Result<Option<Need>> {
    with_snapshot_ledger(
        paths,
        &expected.want.entries(),
        snapshot,
        expected.refused.is_some(),
        false,
        current,
        |rows, facts| {
            let row = rows
                .iter()
                .find(|row| row.want == expected.want && row.purpose == expected.purpose)
                .ok_or_else(|| Error::new("the credential request changed"))?;
            if row.id != expected.id
                || row.refused != expected.refused
                || row.state.ended()
                || match row.refused.as_ref() {
                    Some(RefusalClaim::Authenticated(floor)) => {
                        !facts.after_refusal(row.refused.as_ref())
                            && !facts
                                .sight
                                .as_ref()
                                .and_then(|sight| sight.version.as_ref())
                                .is_some_and(|version| version.refines_for_recovery(floor))
                    }
                    Some(RefusalClaim::LegacySequence(floor))
                        if !facts.after_refusal(row.refused.as_ref()) =>
                    {
                        // No historical log witness exists for old scalar zero. A consumer may
                        // confirm the actual current paired value without inventing that past.
                        *floor != 0
                            || snapshot.sequence() != 0
                            || facts
                                .sight
                                .as_ref()
                                .and_then(|s| s.version.as_ref())
                                .is_none_or(|v| {
                                    v.claim().log().is_none() || v.envelope_sequence().is_some()
                                })
                    }
                    _ => false,
                }
            {
                return Err(Error::new(
                    "the credential request or refusal changed during confirmation",
                ));
            }
            satisfy_in(rows, facts, &expected.want, expected.purpose)
        },
    )
}

fn satisfy_in(
    rows: &mut [Need],
    facts: &Facts,
    want: &Want,
    purpose: Purpose,
) -> crate::Result<Option<Need>> {
    let Some(at) = rows
        .iter()
        .position(|need| need.want == *want && need.purpose == purpose)
    else {
        return Ok(None);
    };
    let settled = settle(&rows[at], facts);
    let next = act(&settled, facts, &Act::Apply)
        .or_else(|| act(&settled, facts, &Act::Unneeded))
        .unwrap_or(settled);
    rows[at] = next.clone();
    Ok(Some(next))
}

/// Withdraw by hand this Mac's needs that `selector` names (a want key or a request id),
/// keeping `note` (§6.2).
///
/// # Errors
/// Nothing in the ledger matches, or the ledger cannot be read or written.
pub fn retire(paths: &Paths, selector: &str, note: &str) -> crate::Result<Vec<Need>> {
    with_ledger(paths, |rows, facts| {
        let matches = |need: &Need| need.named(selector);
        if !rows.iter().any(matches) {
            return Err(Error::new(format!(
                "no need on this Mac is {selector} (aterm secrets need ls names them); a need another Mac raised is withdrawn on that Mac"
            )));
        }
        let mut changed = Vec::new();
        for need in rows.iter_mut().filter(|need| matches(need)) {
            if let Some(next) = act(need, facts, &Act::Withdraw(note.to_owned())) {
                *need = next.clone();
                changed.push(next);
            }
        }
        if changed.is_empty() {
            return Err(Error::new(format!("{selector} has already ended")));
        }
        Ok(changed)
    })
}

/// Take in what reached the lab's table, or was read off it, for this Mac's needs: a flush
/// the table now holds, each decision whose writer and evidence checked, and a silence past
/// [`super::SILENCE_SECS`] with no custodian record. A move that does not apply where the need
/// stands changes nothing. A lab retirement whose reason has already
/// changed (a lapse: this Mac is taking it in, so it is back) raises the need again at once, as
/// the line it ends with says, since a need raised by hand has no consumer to raise it (§6.2).
/// Each need moved comes back with the request id its move named.
///
/// # Errors
/// The ledger or the vault state cannot be read or written, or no request id can be drawn.
pub fn absorb(
    paths: &Paths,
    moves: &[([u8; 16], super::Absorb)],
) -> crate::Result<Vec<([u8; 16], Need)>> {
    use super::Absorb;
    with_ledger(paths, |rows, facts| {
        let mut moved = Vec::new();
        for (id, step) in moves {
            let Some(need) = rows.iter_mut().find(|need| need.id == *id) else {
                continue;
            };
            let next = match step {
                Absorb::Flushed(record) => act(need, facts, &Act::Flush).map(|next| Need {
                    record: Some(record.clone()),
                    ..next
                }),
                Absorb::Reissued { nonce, record } if !need.state.ended() && need.posted => {
                    let fresh = Need {
                        raises: need.raises.saturating_add(1),
                        refused: need.refused.clone(),
                        ..super::fresh(*nonce, need.want.clone(), need.purpose, facts)
                    };
                    act(&fresh, facts, &Act::Flush).map(|next| Need {
                        record: Some(record.clone()),
                        ..next
                    })
                }
                Absorb::Reissued { .. } => None,
                Absorb::Decided(decision) => match super::decided(need, facts, decision) {
                    Some(next) => Some(settle(&reraise(&next, facts)?.unwrap_or(next), facts)),
                    None => None,
                },
                Absorb::CustodianSilent => {
                    super::silent(need, facts).map(|next| settle(&next, facts))
                }
            };
            if let Some(next) = next {
                *need = next.clone();
                moved.push((*id, next));
            }
        }
        Ok(moved)
    })
}

/// The ledger's supported interpretation, writing nothing: what a status line reads. Older
/// agh.dev completion claims await the consumer check they did not previously require.
///
/// # Errors
/// The ledger cannot be read.
pub fn peek(paths: &Paths) -> crate::Result<Vec<Need>> {
    load(paths)
}

/// Every need in the ledger, each moved as this Mac now observes.
///
/// # Errors
/// The ledger or the vault state cannot be read or written.
pub fn list(paths: &Paths) -> crate::Result<Vec<Need>> {
    with_ledger(paths, |rows, facts| {
        for need in rows.iter_mut() {
            *need = settle(need, facts);
        }
        Ok(rows.clone())
    })
}
