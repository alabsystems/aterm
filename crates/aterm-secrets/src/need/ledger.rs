// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE NEED LEDGER: `~/.config/aterm/secrets/needs.toml`, 0600, one row per `(want, purpose)`,
//! read and written under `needs.lock`. Its folder is one no vault entry can name
//! (`text::valid_entry_name`), so the vault never overwrites it. Rows carry names, states and
//! the words a person reads, never a value.

use super::{
    Act, Ask, Facts, Need, Proof, Purpose, Reason, RefusalClaim, Retirement, State, Wait, Want,
    act, settle,
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

/// Every row, oldest first. A missing ledger holds none.
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
    let (rows, versioned): (&[_], bool) = match rows {
        aterm_toml::Value::Array(rows) => (rows, false),
        aterm_toml::Value::Table(container)
            if container
                .get("format")
                .and_then(aterm_toml::Value::as_integer)
                == Some(2) =>
        {
            let rows = match container.get("rows") {
                None => &[][..],
                Some(rows) => rows.as_array().ok_or_else(damaged)?.as_slice(),
            };
            (rows, true)
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
                || (!versioned
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
    let proof = match n("proof_seq") {
        Some(seq) => Some(Proof {
            seq,
            absent: b("proof_absent")?,
            every_locker_open: b("proof_every_locker_open")?,
            before_commons: b("proof_before_commons")?,
            swept_unheld: b("proof_swept_unheld")?,
            window_ended: b("proof_window_ended").unwrap_or(false),
            custodian_silent: b("proof_custodian_silent").unwrap_or(false),
        }),
        None => None,
    };
    let ask = match s("ask_what") {
        Some(what) => Some(Ask {
            what: what.to_owned(),
            step: s("ask_step")?.to_owned(),
            reason: s("ask_reason")?.to_owned(),
            more: t
                .get("ask_more")
                .and_then(aterm_toml::Value::as_array)
                .map(|lines| {
                    lines
                        .iter()
                        .filter_map(aterm_toml::Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            needs_you: s("needs_you")?.to_owned(),
        }),
        None => None,
    };
    Some(Need {
        id,
        want: Want::parse(s("want")?).ok()?,
        purpose: Purpose::parse(s("purpose")?)?,
        state: State::parse(s("state")?)?,
        wait: Wait::parse(s("wait")?)?,
        retired,
        raised: n("raised")?,
        updated: n("updated")?,
        raises: u32::try_from(n("raises")?).ok()?,
        refused: n("refused").map(RefusalClaim::LegacySequence),
        needed: b("needed")?,
        asking: b("asking")?,
        proof,
        ask,
        got: b("got")?,
        got_authorized: b("got_authorized")?,
        raised_again_on_change: b("raised_again_on_change"),
        posted: b("posted")?,
        custodian_silent: b("custodian_silent").unwrap_or(false),
        late_offer: b("late_offer").unwrap_or(false),
        record: match s("record") {
            Some(text) => Some(unhex(text)?),
            None => None,
        },
        line: s("line").unwrap_or_default().to_owned(),
        seq: n("seq"),
    })
}

fn int(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

fn render(paths: &Paths, rows: &[Need]) -> crate::Result<String> {
    let mut text = String::from(
        "# aterm secrets need ledger (docs/DESIGN-credential-requests-2026-10-27.md).\n\
         # Written by aterm; names, states and explanations only, never a value.\n\n[need]\nformat = 2\n",
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
        put("asking", need.asking.to_string());
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
        if need.late_offer {
            put("late_offer", "true".to_owned());
        }
        if let Some(retired) = &need.retired {
            put("reason", toml_str(retired.reason.word()));
            put("evidence", toml_str(&retired.evidence));
            put("note", toml_str(&retired.note));
            if let Some(generation) = retired.generation {
                put("generation", int(generation).to_string());
            }
        }
        if let Some(proof) = &need.proof {
            put("proof_seq", int(proof.seq).to_string());
            put("proof_absent", proof.absent.to_string());
            put(
                "proof_every_locker_open",
                proof.every_locker_open.to_string(),
            );
            put("proof_before_commons", proof.before_commons.to_string());
            put("proof_swept_unheld", proof.swept_unheld.to_string());
            if proof.window_ended {
                put("proof_window_ended", "true".to_owned());
            }
            if proof.custodian_silent {
                put("proof_custodian_silent", "true".to_owned());
            }
        }
        if let Some(ask) = &need.ask {
            put("ask_what", toml_str(&ask.what));
            put("ask_step", toml_str(&ask.step));
            put("ask_reason", toml_str(&ask.reason));
            let more: Vec<String> = ask.more.iter().map(|line| toml_str(line)).collect();
            put("ask_more", format!("[{}]", more.join(", ")));
            put("needs_you", toml_str(&ask.needs_you));
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

/// Withdraw by hand this Mac's needs that `selector` names (a want key or a request id), or,
/// with `declined`, decline as a person each one awaiting a person; either keeps `note` (§6.2).
///
/// # Errors
/// Nothing in the ledger matches, a decline names no need awaiting a person, or the ledger
/// cannot be read or written.
pub fn retire(
    paths: &Paths,
    selector: &str,
    declined: bool,
    note: &str,
) -> crate::Result<Vec<Need>> {
    with_ledger(paths, |rows, facts| {
        let matches = |need: &Need| need.named(selector);
        if !rows.iter().any(matches) {
            return Err(Error::new(format!(
                "no need on this Mac is {selector} (aterm secrets need ls names them); a need another Mac raised is withdrawn on that Mac, and one awaiting a person is declined there, or on the vault's admin Mac by the id aterm secrets need ls --all prints"
            )));
        }
        let mut changed = Vec::new();
        for need in rows.iter_mut().filter(|need| matches(need)) {
            let step = if declined {
                Act::Decline(note.to_owned())
            } else {
                Act::Withdraw(note.to_owned())
            };
            if let Some(next) = act(need, facts, &step) {
                *need = next.clone();
                changed.push(next);
            }
        }
        if changed.is_empty() {
            return Err(Error::new(if declined {
                format!("{selector} awaits no person here, so there is nothing to decline")
            } else {
                format!("{selector} has already ended")
            }));
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
                Absorb::Posted(record) => act(need, facts, &Act::Post).map(|next| Need {
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

/// The ledger as it stands, moving nothing and writing nothing: what a status line reads.
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
