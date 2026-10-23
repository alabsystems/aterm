// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The signed chain: the paper master's roster, a live machine's appcast, the release's
//! identity and Linux triples under the durable floors ([`verify_manifest`]), the
//! channel head's policy over an older candidate, and the command line ([`main`]).

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::Read as _;

use aterm_toml::{Table, Value};
use ring::signature::{ED25519, UnparsedPublicKey};

use crate::py::{PyPath, bare_os_error, equals_str, int_tuple, os_error, truthy, uint, uint_or};
use crate::store::{Policy, preflight_store, read_install_policy};
use crate::{MAX_BINARY, Refusal, Verdict, require};

const U32_MAX: u64 = 0xffff_ffff;

/// The two appcast proofs a verification may read.
const MANIFEST_NAMES: [&str; 2] = ["aterm-appcast.toml", "aterm-policy-appcast.toml"];

/// `base64.b64decode(text, validate=True)` as Python 3.11+ reads it (`binascii`'s strict
/// mode): the standard alphabet only, `=` only to complete the last group of four, and
/// nothing after it.
pub(crate) fn b64decode_strict(text: &str) -> Option<Vec<u8>> {
    let body = text.trim_end_matches('=');
    let shaped = text.len().is_multiple_of(4)
        && text.len() - body.len() <= 2
        && body
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/');
    shaped.then(|| aterm_codec::base64::decode(text).ok())?
}

/// The 32 raw bytes of `key` when it is their one canonical padded base64 spelling.
fn canonical_key(key: &str) -> Option<Vec<u8>> {
    let raw = b64decode_strict(key)?;
    let spelt = aterm_codec::base64::encode(&raw).ok()?;
    (raw.len() == 32 && spelt == key).then_some(raw)
}

/// Whether `signature` is `key`'s Ed25519 signature over `data`: a canonical 32-byte key,
/// a 64-byte signature, and ring's verification (the updater's).
#[must_use]
pub fn verify_ed25519(key: &str, data: &[u8], signature: &[u8]) -> bool {
    let Some(raw) = canonical_key(key) else {
        return false;
    };
    signature.len() == 64
        && UnparsedPublicKey::new(&ED25519, raw)
            .verify(data, signature)
            .is_ok()
}

/// The Unix second a roster's `valid_until` or a machine's `not_after` names, an exclusive
/// deadline: `YYYY-MM-DDTHH:MM:SSZ`, a real UTC date and time, else `invalid roster
/// expiry`.
///
/// # Errors
/// Anything but that spelling of a date and time that exists.
pub fn deadline(value: Option<&Value>) -> Verdict<i64> {
    let invalid = || Refusal::new("invalid roster expiry");
    let text = value.and_then(Value::as_str).ok_or_else(invalid)?;
    let b = text.as_bytes();
    let shape = b.len() == 20
        && b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            10 => *c == b'T',
            13 | 16 => *c == b':',
            19 => *c == b'Z',
            _ => c.is_ascii_digit(),
        });
    if !shape {
        return Err(invalid());
    }
    let field = |from: usize, to: usize| -> i64 {
        b[from..to]
            .iter()
            .fold(0, |n, c| n * 10 + i64::from(c - b'0'))
    };
    let (year, month, day) = (field(0, 4), field(5, 7), field(8, 10));
    let (hour, minute, second) = (field(11, 13), field(14, 16), field(17, 19));
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => 0,
    };
    if year < 1 || day < 1 || day > days_in_month || hour > 23 || minute > 59 || second > 59 {
        return Err(invalid());
    }
    Ok(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Days from 1970-01-01 to a proleptic Gregorian date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The apply floor once every revoked machine's part is withdrawn: the updater's
/// `FloorSources::step` with nothing raised.
///
/// # Errors
/// Sources that are not a table of a `base` and a `machines` table of u64s.
pub fn withdrawn_floor(
    recorded: u64,
    sources: Option<&Value>,
    revoked: &BTreeSet<String>,
) -> Verdict<u64> {
    if !truthy(sources) {
        return Ok(recorded);
    }
    let Some(Value::Table(sources)) = sources else {
        return Err(Refusal::new("invalid floor sources"));
    };
    let base = uint_or(sources.get("base"), 0, "floor base", u64::MAX)?;
    let empty = Table::new();
    let machines = match sources.get("machines") {
        None => &empty,
        Some(Value::Table(machines)) => machines,
        Some(_) => return Err(Refusal::new("invalid floor sources")),
    };
    require(
        machines.keys().all(|key| !key.is_empty()),
        "invalid floor sources",
    )?;
    let machines = machines
        .iter()
        .map(|(key, value)| Ok((key.as_str(), uint(Some(value), "floor source", u64::MAX)?)))
        .collect::<Verdict<Vec<(&str, u64)>>>()?;
    if machines.iter().map(|(_, v)| *v).chain([base]).max() < Some(recorded) {
        return Ok(recorded);
    }
    let kept = machines
        .iter()
        .filter(|(key, _)| !revoked.contains(*key))
        .map(|(_, value)| (*value).min(recorded));
    Ok(kept.chain([base.min(recorded)]).max().unwrap_or(recorded))
}

/// `read_bounded`: at most `maximum` bytes of `path`, else `oversized <path>`.
pub(crate) fn read_bounded(path: &PyPath, maximum: u64) -> Verdict<Vec<u8>> {
    let real = path.to_path();
    let file = open_file(&real)?;
    let mut data = Vec::new();
    file.take(maximum + 1)
        .read_to_end(&mut data)
        .map_err(|e| bare_os_error(&e))?;
    require(
        u64::try_from(data.len()).is_ok_and(|len| len <= maximum),
        &format!("oversized {path}"),
    )?;
    Ok(data)
}

/// `open(path, "rb")`: a directory is refused as Python's `open` refuses it.
pub(crate) fn open_file(path: &std::path::Path) -> Verdict<std::fs::File> {
    let file = std::fs::File::open(path).map_err(|e| os_error(&e, path))?;
    let meta = file.metadata().map_err(|e| os_error(&e, path))?;
    if meta.is_dir() {
        return Err(os_error(
            &std::io::Error::from_raw_os_error(libc::EISDIR),
            path,
        ));
    }
    Ok(file)
}

/// `tomllib.loads(bytes.decode("utf-8"))`.
pub(crate) fn parse_toml(bytes: &[u8], path: &PyPath) -> Verdict<Table> {
    let text = std::str::from_utf8(bytes)
        .map_err(|e| Refusal::new(format!("{path} is not UTF-8: {e}")))?;
    if text.starts_with('\u{feff}') {
        return Err(Refusal::new(format!(
            "{path} is not TOML: Invalid statement (at line 1, column 1)"
        )));
    }
    let table = text.parse::<Table>().map_err(|e| {
        let message = e.message_text().replace(['\r', '\n'], " ");
        let at = e
            .span()
            .and_then(|span| text.get(..span.start))
            .map(|head| {
                let line = 1 + head.matches('\n').count();
                let column = 1 + head
                    .rsplit('\n')
                    .next()
                    .map_or(0, |last| last.chars().count());
                format!(" (at line {line}, column {column})")
            });
        Refusal::new(format!(
            "{path} is not TOML: {message}{}",
            at.unwrap_or_default()
        ))
    })?;
    tomllib_reads(&table).map_err(|value| {
        Refusal::new(format!(
            "{path} is not TOML: Invalid date or datetime {value}"
        ))
    })?;
    Ok(table)
}

/// The first date-time in `table` that tomllib cannot read, though TOML 1.0 admits it: a
/// date in year 0 (Python's `datetime.date` starts at year 1) or a leap second (tomllib's
/// seconds run 00 to 59).
fn tomllib_reads(table: &Table) -> Result<(), aterm_toml::Datetime> {
    fn read(value: &Value) -> Result<(), aterm_toml::Datetime> {
        match value {
            Value::Datetime(at)
                if at.date.is_some_and(|date| date.year == 0)
                    || at.time.is_some_and(|time| time.second == 60) =>
            {
                Err(*at)
            }
            Value::Array(items) => items.iter().try_for_each(read),
            Value::Table(table) => tomllib_reads(table),
            _ => Ok(()),
        }
    }
    table.values().try_for_each(read)
}

/// One verification's question: whose proofs, which release, for which architecture, at
/// what time, under which trust anchor.
pub struct Ask<'a> {
    /// The directory holding `aterm-machines.toml(.sig)` and the appcast pair.
    pub directory: &'a std::ffi::OsStr,
    /// The release tag, `v<version>`.
    pub tag: &'a str,
    /// The repository, `owner/repo`.
    pub slug: &'a str,
    /// The Linux architecture whose artifact is asked for; none asks only for the appcast.
    pub arch: Option<&'a str>,
    /// `aterm-appcast.toml`, or the channel head's `aterm-policy-appcast.toml`.
    pub manifest_name: &'a str,
    /// Unix seconds.
    pub now: i64,
    /// The keys the roster must be signed under.
    pub pins: &'a [String],
}

/// What a verified appcast says, beside its signed table.
#[derive(Debug)]
pub struct Signed {
    /// The appcast as signed.
    pub manifest: Table,
    /// The build it names.
    pub build: u64,
    /// Its own minimum build (`min_build`, 0 when absent).
    pub minimum: u64,
    /// The roster generation that admitted its signer.
    pub roster_seq: u64,
    /// The machines that roster revokes.
    pub roster_revoked: Vec<String>,
    /// The asked architecture's artifact: its name, SHA-256 and size.
    pub artifact: Option<(String, String, u64)>,
}

/// `re.fullmatch("[0-9a-f]{n}", text)`.
fn lower_hex(text: &str, n: usize) -> bool {
    text.len() == n
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)`.
fn canonical_tag(tag: &str) -> bool {
    let Some(version) = tag.strip_prefix('v') else {
        return false;
    };
    let parts: Vec<&str> = version.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.bytes().all(|b| b.is_ascii_digit())
                && (*part == "0" || !part.starts_with('0'))
        })
}

/// `[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+`.
fn repository(slug: &str) -> bool {
    let word = |w: &str| {
        !w.is_empty()
            && w.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
    };
    slug.split_once('/')
        .is_some_and(|(owner, repo)| word(owner) && word(repo))
}

/// The machine table's string field `key`, which the roster pass already checked.
fn text<'a>(machine: &'a Table, key: &str) -> &'a str {
    machine.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// The chain the updater checks before it installs a release, under the durable `policy`:
/// the roster signed by a pinned master, admitted at the durable roster floor and before
/// its deadline, its machines well-formed; the appcast signed by a live machine neither it
/// nor the durable record revokes, attributed to it, of a roster it admits; the tag, the
/// signed origin and the source commit; the build above every floor; and every Linux
/// triple whole and canonical, the asked architecture's present.
///
/// # Errors
/// The first rule the proofs break, in the embedded verifier's words.
pub fn verify_manifest(ask: &Ask<'_>, policy: &Policy) -> Verdict<Signed> {
    let tag = ask.tag;
    require(
        ask.arch
            .is_none_or(|arch| arch == "x86_64" || arch == "aarch64"),
        "unsupported Linux architecture",
    )?;
    require(canonical_tag(tag), "noncanonical release tag")?;
    let version = &tag[1..];
    require(
        version.split('.').all(|part| part.parse::<u64>().is_ok()),
        "release tag overflow",
    )?;
    require(repository(ask.slug), "invalid repository")?;
    let root = PyPath::new(ask.directory);
    let roster_path = root.join("aterm-machines.toml");
    let roster_bytes = read_bounded(&roster_path, 65_536)?;
    let roster_sig = read_bounded(&root.join("aterm-machines.toml.sig"), 64)?;
    require(
        ask.pins
            .iter()
            .any(|key| verify_ed25519(key, &roster_bytes, &roster_sig)),
        "roster is not signed by the pinned paper master",
    )?;
    let roster = parse_toml(&roster_bytes, &roster_path)?;
    require(
        uint(roster.get("schema"), "roster schema", U32_MAX)? <= 1,
        "newer roster schema",
    )?;
    require(
        roster.keys().all(|key| {
            ["schema", "roster_seq", "valid_until", "revoked", "machine"].contains(&key.as_str())
        }),
        "unknown roster key",
    )?;
    let seq = uint(roster.get("roster_seq"), "roster sequence", u64::MAX)?;
    require(seq >= policy.floors[2], "roster rollback")?;
    require(
        ask.now < deadline(roster.get("valid_until"))?,
        "roster expired",
    )?;
    let empty = Vec::new();
    let machines = match roster.get("machine") {
        None => Some(&empty),
        Some(Value::Array(machines)) => Some(machines),
        Some(_) => None,
    };
    let machines = machines
        .filter(|machines| machines.len() <= 16)
        .ok_or_else(|| Refusal::new("invalid machine roster"))?;
    let revoked = strings(roster.get("revoked"), "invalid revoked machines")?;
    let mut ids = BTreeSet::new();
    let mut keys = BTreeSet::new();
    let mut rows = Vec::with_capacity(machines.len());
    for machine in machines {
        let Value::Table(machine) = machine else {
            return Err(Refusal::new("invalid machine"));
        };
        require(
            machine
                .keys()
                .all(|key| ["id", "pubkey", "added_at", "not_after"].contains(&key.as_str())),
            "unknown machine key",
        )?;
        require(
            machine.get("added_at").is_none_or(|v| v.as_str().is_some()),
            "invalid machine added_at",
        )?;
        let identity = machine.get("id").and_then(Value::as_str);
        require(
            identity.is_some_and(|id| !id.is_empty() && !ids.contains(id)),
            "empty or duplicate machine identity",
        )?;
        let key = machine.get("pubkey").and_then(Value::as_str);
        require(
            key.is_some_and(|key| !key.is_empty() && !keys.contains(key)),
            "empty or duplicate machine key",
        )?;
        let key = key.unwrap_or_default();
        let decoded =
            b64decode_strict(key).ok_or_else(|| Refusal::new("invalid roster machine key"))?;
        require(
            decoded.len() == 32
                && aterm_codec::base64::encode(&decoded).is_ok_and(|spelt| spelt == key),
            "noncanonical roster machine key",
        )?;
        ids.insert(identity.unwrap_or_default().to_string());
        keys.insert(key.to_string());
        rows.push(machine);
    }
    require(
        MANIFEST_NAMES.contains(&ask.manifest_name),
        "invalid manifest proof name",
    )?;
    let manifest_path = root.join(ask.manifest_name);
    let manifest_bytes = read_bounded(&manifest_path, 5_000_000)?;
    let manifest_sig = read_bounded(&root.join(&format!("{}.sig", ask.manifest_name)), 64)?;
    let mut signer = None;
    for machine in &rows {
        let id = text(machine, "id");
        if revoked.iter().any(|r| r == id) || policy.revoked_machines.iter().any(|r| r == id) {
            continue;
        }
        if machine.get("not_after").is_some() && ask.now >= deadline(machine.get("not_after"))? {
            continue;
        }
        if verify_ed25519(text(machine, "pubkey"), &manifest_bytes, &manifest_sig) {
            signer = Some(id.to_string());
            break;
        }
    }
    let signer =
        signer.ok_or_else(|| Refusal::new("appcast has no authorized live machine signature"))?;
    let manifest = parse_toml(&manifest_bytes, &manifest_path)?;
    require(
        uint(manifest.get("schema"), "manifest schema", U32_MAX)? <= 1,
        "newer manifest schema",
    )?;
    require(
        equals_str(manifest.get("machine_id"), &signer),
        "appcast signer attribution mismatch",
    )?;
    require(
        uint(
            manifest.get("roster_seq"),
            "appcast roster sequence",
            u64::MAX,
        )? <= seq,
        "appcast requires newer roster",
    )?;
    require(
        equals_str(manifest.get("version"), version),
        "release tag/appcast version mismatch",
    )?;
    let commit = match manifest.get("commit") {
        None => Some(""),
        Some(value) => value.as_str(),
    };
    require(
        commit.is_some_and(|commit| lower_hex(commit, 40)),
        "invalid source commit",
    )?;
    require(
        manifest.get("sha256").and_then(Value::as_str).is_some(),
        "appcast has no dmg digest",
    )?;
    let dmg = format!("aterm-{version}.dmg");
    let url = format!(
        "https://github.com/{}/releases/download/{tag}/{dmg}",
        ask.slug
    );
    require(
        equals_str(manifest.get("dmg"), &dmg) && equals_str(manifest.get("url"), &url),
        "signed release origin mismatch",
    )?;
    let build = uint(manifest.get("build_number"), "build number", u64::MAX)?;
    require(
        build > 0 && build > policy.rejected_build,
        "zero or previously rejected release build",
    )?;
    let minimum = uint_or(manifest.get("min_build"), 0, "minimum build", u64::MAX)?;
    let mut withdrawn: BTreeSet<String> = revoked.iter().cloned().collect();
    withdrawn.extend(policy.revoked_machines.iter().cloned());
    let floor = withdrawn_floor(
        policy.floors[1],
        policy.min_build_sources.as_ref(),
        &withdrawn,
    )?;
    require(
        build >= policy.floors[0].max(floor).max(minimum),
        "release build rollback or below apply floor",
    )?;
    let mut artifacts = Vec::new();
    for target in ["x86_64", "aarch64"] {
        let field = format!("linux_{target}");
        let present = ["", "_sha256", "_size"]
            .map(|suffix| manifest.get(&format!("{field}{suffix}")).is_some());
        require(
            !present.iter().any(|p| *p) || present.iter().all(|p| *p),
            "incomplete Linux artifact triple",
        )?;
        if !present[0] {
            continue;
        }
        let name = format!("aterm-{version}-linux-{target}");
        require(
            equals_str(manifest.get(&field), &name),
            "noncanonical Linux asset",
        )?;
        let digest = manifest
            .get(&format!("{field}_sha256"))
            .and_then(Value::as_str)
            .filter(|digest| lower_hex(digest, 64))
            .ok_or_else(|| Refusal::new("invalid Linux digest"))?;
        let size = uint(
            manifest.get(&format!("{field}_size")),
            "Linux size",
            MAX_BINARY,
        )?;
        require(size >= 64, "Linux artifact too small")?;
        artifacts.push((target, name, digest.to_string(), size));
    }
    let artifact = match ask.arch {
        None => None,
        Some(arch) => {
            let found = artifacts.into_iter().find(|(target, ..)| *target == arch);
            let (_, name, digest, size) = found.ok_or_else(|| {
                Refusal::new(format!("signed release has no Linux artifact for {arch}"))
            })?;
            Some((name, digest, size))
        }
    };
    Ok(Signed {
        manifest,
        build,
        minimum,
        roster_seq: seq,
        roster_revoked: revoked,
        artifact,
    })
}

/// A list of non-empty strings (`[]` when absent), else `why`.
pub(crate) fn strings(value: Option<&Value>, why: &str) -> Verdict<Vec<String>> {
    match value {
        None => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item {
                Value::String(s) if !s.is_empty() => Ok(s.clone()),
                _ => Err(Refusal::new(why)),
            })
            .collect(),
        Some(_) => Err(Refusal::new(why)),
    }
}

/// Python's `os.path.exists`-by-`Path.exists()`: a missing entry, a missing directory on
/// the way, a bad descriptor or a symbolic-link loop is "no"; any other error is raised.
fn exists(path: &PyPath) -> Verdict<bool> {
    let real = path.to_path();
    match std::fs::metadata(&real) {
        Ok(_) => Ok(true),
        Err(e)
            if matches!(
                e.raw_os_error(),
                Some(libc::ENOENT | libc::ENOTDIR | libc::EBADF | libc::ELOOP)
            ) =>
        {
            Ok(false)
        }
        Err(e) => Err(os_error(&e, &real)),
    }
}

/// The command line ([`crate::run`]) under the trust anchor `pins`.
pub(crate) fn main(args: &[OsString], now: i64, pins: &[String]) -> Verdict<Option<String>> {
    if args.first().is_some_and(|a| a == "--store-path") {
        require(args.len() == 2, "invalid Linux verifier invocation")?;
        preflight_store(&args[1])?;
        return Ok(None);
    }
    require(
        args.len() == 5 || args.len() == 6,
        "invalid Linux verifier invocation",
    )?;
    let lossy = |n: usize| args[n].to_string_lossy().into_owned();
    let (tag, slug, arch) = (lossy(1), lossy(2), lossy(3));
    let mut policy = read_install_policy(&args[4])?;
    let directory = PyPath::new(&args[0]);
    let head_path = directory.join("aterm-policy-appcast.toml");
    let head_sig = directory.join("aterm-policy-appcast.toml.sig");
    if exists(&head_path)? || exists(&head_sig)? {
        require(
            exists(&head_path)? && exists(&head_sig)?,
            "incomplete latest-policy proof pair",
        )?;
        let candidate = parse_toml(&read_bounded(&head_path, 5_000_000)?, &head_path)?;
        let head_tag = match candidate.get("version") {
            None => "v".to_string(),
            Some(Value::String(version)) => format!("v{version}"),
            Some(_) => "v?".to_string(),
        };
        let head = verify_manifest(
            &Ask {
                directory: &args[0],
                tag: &head_tag,
                slug: &slug,
                arch: None,
                manifest_name: "aterm-policy-appcast.toml",
                now,
                pins,
            },
            &policy,
        )?;
        let candidate_version: String = tag.chars().skip(1).collect();
        require(
            int_tuple(&head_tag[1..])? >= int_tuple(&candidate_version)?,
            "native candidate is newer than authenticated current APP head",
        )?;
        let mut revoked: BTreeSet<String> = policy.revoked_machines.iter().cloned().collect();
        revoked.extend(head.roster_revoked.iter().cloned());
        let floor = withdrawn_floor(
            policy.floors[1],
            policy.min_build_sources.as_ref(),
            &revoked,
        )?;
        policy.revoked_machines = revoked.into_iter().collect();
        policy.floors = [
            policy.floors[0],
            floor.max(head.minimum),
            policy.floors[2].max(head.roster_seq),
        ];
        policy.min_build_sources = None;
    }
    let signed = verify_manifest(
        &Ask {
            directory: &args[0],
            tag: &tag,
            slug: &slug,
            arch: Some(&arch),
            manifest_name: "aterm-appcast.toml",
            now,
            pins,
        },
        &policy,
    )?;
    let (name, digest, size) = signed
        .artifact
        .ok_or_else(|| Refusal::new(format!("signed release has no Linux artifact for {arch}")))?;
    if let Some(candidate) = args.get(5) {
        crate::binary::verify_binary(candidate, &arch, &digest, size)?;
    }
    Ok(Some(format!(
        "{name}\t{digest}\t{size}\t{}\t{}",
        signed.build, signed.roster_seq
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_deadline_is_one_utc_spelling_of_a_real_second() {
        let at = |text: &str| deadline(Some(&Value::String(text.into())));
        assert_eq!(at("1970-01-01T00:00:00Z"), Ok(0));
        assert_eq!(at("2030-01-01T00:00:00Z"), Ok(1_893_456_000));
        assert_eq!(at("2024-02-29T23:59:59Z"), Ok(1_709_251_199));
        for refused in [
            "2023-02-29T00:00:00Z",
            "2030-13-01T00:00:00Z",
            "2030-01-01T24:00:00Z",
            "2030-01-01T00:00:60Z",
            "0000-01-01T00:00:00Z",
            "2030-01-01T00:00:00+00:00",
            "2030-01-01",
            "9999-1-1T0:0:0Z",
        ] {
            assert_eq!(
                at(refused).expect_err(refused).0,
                "invalid roster expiry",
                "{refused}"
            );
        }
        assert!(deadline(Some(&Value::Integer(5))).is_err());
        assert!(deadline(None).is_err());
    }

    #[test]
    fn a_tag_is_canonical_and_a_slug_two_words() {
        assert!(canonical_tag("v0.100.0") && canonical_tag("v18446744073709551616.0.0"));
        for refused in [
            "0.100.0",
            "v00.100.0",
            "v0.100",
            "v0.100.0.0",
            "v0.1a.0",
            "v0.100.00",
        ] {
            assert!(!canonical_tag(refused), "{refused}");
        }
        assert!(repository("example/aterm") && repository("a.b_c-d/e"));
        assert!(!repository("example/aterm/x") && !repository("/aterm") && !repository("a b/c"));
    }

    /// A parse error is one line, saying where, as tomllib's is: install.sh reads one.
    #[test]
    fn a_toml_error_is_one_line_saying_where() {
        let path = PyPath::new(std::ffi::OsStr::new("/r/aterm-appcast.toml"));
        let why = parse_toml(b"a = 1\nb = 2\na = 3\n", &path)
            .expect_err("a duplicate key")
            .0;
        assert!(
            why.starts_with("/r/aterm-appcast.toml is not TOML: ")
                && why.ends_with(" (at line 3, column 1)")
                && !why.contains('\n'),
            "{why}"
        );
        assert!(parse_toml(b"a = 1\n", &path).is_ok());
    }

    /// What TOML 1.0 admits and tomllib does not is no document: a byte order mark, a date
    /// in year 0 and a leap second, anywhere in the tree; the nearest values it reads are.
    #[test]
    fn a_document_tomllib_refuses_is_refused() {
        let path = PyPath::new(std::ffi::OsStr::new("/r/aterm-appcast.toml"));
        let why = |text: &str| {
            parse_toml(text.as_bytes(), &path)
                .map(|_| ())
                .map_err(|e| e.0)
        };
        assert_eq!(
            why("\u{feff}a = 1\n"),
            Err(
                "/r/aterm-appcast.toml is not TOML: Invalid statement (at line 1, column 1)".into()
            )
        );
        for (text, value) in [
            ("a = 2026-06-30T23:59:60Z\n", "2026-06-30T23:59:60Z"),
            ("a = 0000-01-01\n", "0000-01-01"),
            ("a = 23:59:60\n", "23:59:60"),
            (
                "[t]\nb = [1, [0000-12-31T00:00:00]]\n",
                "0000-12-31T00:00:00",
            ),
        ] {
            assert_eq!(
                why(text),
                Err(format!(
                    "/r/aterm-appcast.toml is not TOML: Invalid date or datetime {value}"
                )),
                "{text}"
            );
        }
        for text in [
            "a = \"\u{feff}\"\n",
            "a = 2026-06-30T23:59:59Z\n",
            "a = 0001-01-01\n",
            "a = 9999-12-31T23:59:59.999999+23:59\n",
        ] {
            assert_eq!(why(text), Ok(()), "{text}");
        }
    }

    /// A key is decoded as Python 3.11+'s `b64decode(validate=True)` decodes it: a spelling
    /// it refuses is no key, and one it reads is then held to its canonical spelling.
    #[test]
    fn a_key_is_decoded_as_pythons_strict_base64() {
        let key = "oJql9HpnWYAv+VX43C0qFKXJnSO+l/hkEn/5ODRVpPA="; // gitleaks:allow
        assert_eq!(b64decode_strict(key).map(|raw| raw.len()), Some(32));
        assert!(canonical_key(key).is_some());
        for refused in [
            "AAA",
            "oJql9HpnWYAv+VX43C0qFKXJnSO+l/hkEn/5ODRVpPA",
            "AAAA=",
            "AAA==",
            "A===",
            "====",
            "AB=C",
            "AA==AA==",
            "AAAA AAAA",
            "AAAA\n",
            "AA-_",
        ] {
            assert_eq!(b64decode_strict(refused), None, "{refused}");
            assert!(canonical_key(refused).is_none(), "{refused}");
        }
        assert_eq!(b64decode_strict("AB=="), Some(vec![0]));
        assert_eq!(b64decode_strict(""), Some(Vec::new()));
        let alias = "oJql9HpnWYAv+VX43C0qFKXJnSO+l/hkEn/5ODRVpPB=";
        assert_eq!(b64decode_strict(alias), b64decode_strict(key));
        assert!(canonical_key(alias).is_none());
    }
}
