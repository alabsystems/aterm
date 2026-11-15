// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `commons-lan emails-*`: the owner's emails table end to end across one synthetic lab vault on
//! separate Macs (docs/DESIGN-alab-one-mesh-2026-11-05.md §11 E1).
//!
//! - **`emails-fixture <world> <seed>`** writes, into a prepared profile, what the owner's emails
//!   live in today: a fleet_db replica (`~/.aterm/lab-db` under the profile's home, with a fleet
//!   key of its own) of [`ROWS`] addresses, [`WITH_PASSWORD`] of them with a password of 8 to 24
//!   made-up characters, and the owner's sheet (`alab-emails.csv` in the profile's folder), whose
//!   `Password` column is a has-password flag: `1` on each row with a password and on
//!   [`DECOYS`] rows without one. The shipped import (`commons-lan db … alab import`) reads both;
//!   its rule is that a flag never becomes a password (§4.7). Every Mac makes the same rows from
//!   the same seed, so another Mac compares its registry with them (`import --fleet-db
//!   --fill-only`, a dry run) and no fixture value crosses Macs.
//! - **`emails-set <aterm> <world> <seed> <row> <column> <value-or-tag>`** writes one cell of row
//!   `<row>` (1 to [`ROWS`]) through the shipped `aterm db emails set`: a `password` from
//!   standard input, a value derived from the seed and `<tag>` (never on a command line); any other
//!   column the value given. It reports when the write returned and the table's digest after it.
//!   A password shows as the mask before and after, so its edit moves the digest only where the
//!   cell held none.
//! - **`emails-wait <aterm> <world> <digest>|!<digest> <seconds>`** reads the table's digest
//!   until it equals `<digest>` (or, after `!`, until it no longer does), and reports when it did
//!   and the digest it read: how long an edit took to reach this Mac.
//!
//! The table's digest is computed here from what `aterm db emails` shows
//! ([`crate::util::listing_digest`]): an address as it is, a password as the mask or as the name
//! of its secret-store entry, never a value. Reports name counts, row numbers, times and
//! digests, never an address or a value.

use super::{db_output, fail, write_new};
use crate::instance::World;
use crate::{Fail, stop};
use aterm_json::{Value, json};
use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};

/// The rows of the owner's emails sheet (its shape on m22 and m104).
pub(super) const ROWS: usize = 100;
/// The rows with a password.
pub(super) const WITH_PASSWORD: usize = 39;
/// Rows whose sheet flag says `1` while the replica holds no password: a flag that must stay one.
pub(super) const DECOYS: usize = 3;
/// The sheet's file in the profile's folder.
pub(super) const SHEET: &str = "alab-emails.csv";
/// The owner's emails sheet's header row.
const SHEET_HEAD: &str = "Email Address,Root Account,Password,Status,Provider,Activation Phone Number,Protection Flag,Notes\n";
/// What a made-up password is drawn from.
const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789!#%+-=?@^_~";
/// Between two digest reads of `emails-wait`.
const POLL: Duration = Duration::from_millis(250);
/// The longest `emails-wait`: past Commons repair's two-minute backoff several times over.
const MAX_WAIT_S: u64 = 900;

/// 32 bytes drawn from `seed` for `label` and `index`.
fn draw(seed: &str, label: &str, index: usize) -> [u8; 32] {
    let mut hash = aterm_digest::Sha256::new();
    hash.update(b"aterm-lab/commons-lan/emails/1\0");
    for part in [
        seed.as_bytes(),
        label.as_bytes(),
        &(index as u64).to_le_bytes(),
    ] {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part);
    }
    hash.finalize()
}

/// A made-up password of 8 to 24 characters drawn from `seed`, `label` and `index`.
fn password(seed: &str, label: &str, index: usize) -> String {
    let first = draw(seed, label, index);
    let second = draw(seed, &format!("{label}+"), index);
    let len = 8 + usize::from(first[0] % 17);
    first[1..]
        .iter()
        .chain(second.iter())
        .take(len)
        .map(|byte| char::from(ALPHABET[usize::from(*byte) % ALPHABET.len()]))
        .collect()
}

/// One row of the fixture.
pub(super) struct Row {
    pub email: String,
    pub password: Option<String>,
    /// The sheet's has-password flag.
    pub flag: bool,
}

/// The fixture's rows, the same on every Mac for one seed: each address is the run's
/// (`lab<NNN>.<tag>@proton.example`), [`WITH_PASSWORD`] rows chosen by the seed hold a password,
/// and the next [`DECOYS`] carry the sheet's flag with no password.
pub(super) fn rows(seed: &str) -> Vec<Row> {
    let tag = aterm_codec::hex::encode_lower(&draw(seed, "tag", 0)[..3]);
    let mut order: Vec<usize> = (1..=ROWS).collect();
    order.sort_by_key(|index| draw(seed, "pick", *index));
    let with: BTreeSet<usize> = order[..WITH_PASSWORD].iter().copied().collect();
    let decoys: BTreeSet<usize> = order[WITH_PASSWORD..WITH_PASSWORD + DECOYS]
        .iter()
        .copied()
        .collect();
    (1..=ROWS)
        .map(|index| Row {
            email: format!("lab{index:03}.{tag}@proton.example"),
            password: with
                .contains(&index)
                .then(|| password(seed, "password", index)),
            flag: with.contains(&index) || decoys.contains(&index),
        })
        .collect()
}

/// The sheet the owner keeps beside the replica: every row, its password column a flag.
fn sheet(rows: &[Row]) -> String {
    let mut out = String::from(SHEET_HEAD);
    for row in rows {
        let flag = if row.flag { "1" } else { "" };
        out.push_str(&format!("{},,{flag},active,proton,,,\n", row.email));
    }
    out
}

/// `emails-fixture`: the replica and the sheet in `world`, made once.
///
/// # Errors
/// The profile already holds a replica, or a file could not be written.
pub(super) fn fixture(world: &World, seed: &str) -> Result<Value, Fail> {
    let aterm_dir = world.home.join(".aterm");
    let dir = aterm_dir.join("lab-db");
    if dir.try_exists().map_err(fail)? {
        return Err(Fail::die("this profile already holds a fleet_db replica"));
    }
    if !aterm_dir.try_exists().map_err(fail)? {
        aterm_uds::private_file::create_private_dir(&aterm_dir).map_err(fail)?;
    }
    aterm_uds::private_file::create_private_dir(&dir).map_err(fail)?;
    // The replica's own fleet key: no other replica shares it, so no fleet_db exchange joins
    // this one to another Mac's.
    let key = aterm_codec::hex::encode_lower(&aterm_db::crypto::random::<32>().map_err(fail)?);
    aterm_uds::private_file::write_private(&dir, "fleet.key", key.as_bytes()).map_err(fail)?;
    let rows = rows(seed);
    let mut db = aterm_db::fleet_db::open_at(&dir).map_err(fail)?;
    for row in &rows {
        let value = json!({
            "email": row.email, "provider": "proton", "account_type": "root",
            "root_account": null, "password": row.password,
            "webmail_url": "https://mail.proton.me/u/0/inbox", "status": "active",
            "phone": null, "protected": false, "assigned_computers": [], "services": [],
            "notes": null,
        });
        aterm_db::fleet_db::put_row(&mut db, "emails", &row.email, value).map_err(fail)?;
    }
    drop(db);
    write_new(&world.dir.join(SHEET), sheet(&rows).as_bytes())?;
    Ok(json!({
        "rows": rows.len(),
        "with_password": rows.iter().filter(|row| row.password.is_some()).count(),
        "flags": rows.iter().filter(|row| row.flag).count(),
        "decoys": DECOYS,
        "replica": dir.display().to_string(),
        "sheet": world.dir.join(SHEET).display().to_string(),
    }))
}

/// The digest of what `aterm db emails` shows in `world` ([`listing`]), or why not.
fn digest(aterm: &Path, world: &World) -> Result<Value, Fail> {
    let output = db_output(
        aterm,
        world,
        &["emails".into()],
        aterm_uds::spawnfd::Stdio::null(),
    )?;
    if !output.status.success() {
        return Err(Fail::die(format!(
            "emails failed: {}",
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .next()
                .unwrap_or_default()
        )));
    }
    let listed: Value = aterm_json::from_slice(&output.stdout).map_err(fail)?;
    listing(&listed).map_err(Fail::die)
}

/// What an `aterm db emails` view says of its table, value-free: `digest` (of its rows as
/// shown), `rows`, `with_password` (rows whose password cell shows anything) and `complete`.
///
/// # Errors
/// The view lists no rows, or its registry read refused (`state` `never`: no rows and the
/// reason, which is no table to compare): what it said instead.
fn listing(listed: &Value) -> Result<Value, String> {
    let said = |what: &str| {
        let text: String = aterm_json::to_string(listed).unwrap_or_default();
        format!(
            "the emails view {what}: {}",
            crate::util::one_line(&text, 160)
        )
    };
    let rows = listed
        .get("emails")
        .and_then(Value::as_array)
        .ok_or_else(|| said("lists no rows"))?;
    if listed.get("state").and_then(Value::as_str) == Some("never") {
        return Err(said("did not read the registry"));
    }
    Ok(json!({
        "digest": crate::util::listing_digest(rows),
        "rows": rows.len(),
        "with_password": rows.iter().filter(|row| !row["password"].is_null()).count(),
        "complete": listed.get("complete").and_then(Value::as_bool).unwrap_or(false),
    }))
}

/// `emails-set`: one cell of row `row`, through the shipped `aterm db emails set`.
///
/// # Errors
/// The row is not the fixture's, or the write or the digest after it failed.
pub(super) fn set(
    aterm: &Path,
    world: &World,
    seed: &str,
    row: &str,
    column: &str,
    value: &str,
) -> Result<Value, Fail> {
    let index: usize = row
        .parse()
        .ok()
        .filter(|index| (1..=ROWS).contains(index))
        .ok_or_else(|| Fail::usage(format!("emails-set takes a row from 1 to {ROWS}")))?;
    let email = rows(seed)
        .into_iter()
        .nth(index - 1)
        .map(|row| row.email)
        .ok_or_else(|| Fail::die("the fixture has no such row"))?;
    let started = Instant::now();
    let output = if column == "password" {
        // The value goes through a pipe, as the owner's `--stdin` takes it: never argv.
        let (reader, mut writer) = aterm_uds::pipe().map_err(fail)?;
        writer
            .write_all(password(seed, &format!("edit:{value}"), index).as_bytes())
            .map_err(fail)?;
        drop(writer);
        db_output(
            aterm,
            world,
            &[
                "emails".into(),
                "set".into(),
                email,
                column.into(),
                "--stdin".into(),
            ],
            reader.into(),
        )?
    } else {
        db_output(
            aterm,
            world,
            &[
                "emails".into(),
                "set".into(),
                email,
                column.into(),
                value.into(),
            ],
            aterm_uds::spawnfd::Stdio::null(),
        )?
    };
    let set_ms = started.elapsed().as_millis();
    let at_ms = crate::util::now_ms();
    if !output.status.success() {
        return Err(Fail::die(format!(
            "emails set failed: {}",
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .next()
                .unwrap_or_default()
        )));
    }
    Ok(json!({
        "row": index, "column": column, "at_ms": at_ms, "set_ms": set_ms,
        "digest": digest(aterm, world)?,
    }))
}

/// `emails-wait`: read the digest until it equals `want` (until it differs from what follows a
/// leading `!`), or `seconds` pass; the report and its exit code (1 when it never did). A read
/// the CLI refused counts as a poll that saw nothing (`failed_reads`, and the first refusal).
///
/// # Errors
/// The wait is out of range, or an interrupt stopped it.
pub(super) fn wait(
    aterm: &Path,
    world: &World,
    want: &str,
    seconds: u64,
) -> Result<(Value, i32), Fail> {
    if !(1..=MAX_WAIT_S).contains(&seconds) {
        return Err(Fail::usage(format!(
            "emails-wait takes 1 to {MAX_WAIT_S} seconds"
        )));
    }
    let started = Instant::now();
    let until = started + Duration::from_secs(seconds);
    let mut polls = 0_u64;
    // A read the CLI refused (a pass past its deadline on a loaded Mac, a table a sync advanced
    // mid-read) observed nothing: the poll goes on, and the report counts them.
    let mut failed = 0_u64;
    let mut first_failure: Option<String> = None;
    loop {
        stop::check()?;
        let read = match digest(aterm, world) {
            Ok(read) => read,
            Err(failure) => {
                polls += 1;
                failed += 1;
                first_failure.get_or_insert_with(|| {
                    failure.msg.unwrap_or_default().chars().take(200).collect()
                });
                if Instant::now() >= until {
                    return Ok((
                        json!({"seen": false, "waited_ms": started.elapsed().as_millis(),
                            "polls": polls, "failed_reads": failed,
                            "first_failure": first_failure}),
                        1,
                    ));
                }
                stop::sleep(POLL)?;
                continue;
            }
        };
        polls += 1;
        let held = read["digest"].as_str();
        let reached = match want.strip_prefix('!') {
            Some(old) => held.is_some_and(|held| held != old),
            None => held == Some(want),
        };
        if reached {
            return Ok((
                json!({"seen": true, "at_ms": crate::util::now_ms(),
                    "waited_ms": started.elapsed().as_millis(), "polls": polls,
                    "failed_reads": failed, "first_failure": first_failure,
                    "digest": read}),
                0,
            ));
        }
        if Instant::now() >= until {
            return Ok((
                json!({"seen": false, "waited_ms": started.elapsed().as_millis(),
                    "polls": polls, "failed_reads": failed, "first_failure": first_failure,
                    "last": read}),
                1,
            ));
        }
        stop::sleep(POLL)?;
    }
}

#[cfg(test)]
#[path = "commons_lan_emails_tests.rs"]
mod tests;
