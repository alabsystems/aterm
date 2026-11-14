// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Writing a machine's secrets to disk (docs/DESIGN-secrets-2026-10-20.md §7, Sync).

use crate::entry_class;
use crate::fsx::{self, Kind};
use crate::text::{self, Entry};
use crate::{Error, SecretBytes};
use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// What the vault wrote: entry name → lowercase hex SHA-256 of the bytes written.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Manifest {
    /// Entry name → digest.
    pub files: BTreeMap<String, String>,
}

/// What one materialization did, by entry name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// Written (new or changed).
    pub written: Vec<String>,
    /// Already held the value.
    pub unchanged: Vec<String>,
    /// Moved aside before being overwritten: name and where it went.
    pub moved_aside: Vec<(String, PathBuf)>,
    /// Removed because the vault dropped them or took them back, or because their name is
    /// reserved ([`crate::reserved_name`]) and a build that did not reserve it wrote the file.
    pub removed: Vec<String>,
    /// Left in place: dropped (or taken back) by the vault, or a reserved name's file, but
    /// changed since it wrote them, not looked at, or ` if-absent` entries whose target or
    /// anchor is the machine's own or whose anchor could not be looked at.
    pub left: Vec<String>,
    /// Not written, or not removed: the name and why (never the value).
    pub failed: Vec<(String, String)>,
    /// Never written and never taken back, the name and why: an entry set apart by its name
    /// ([`entry_class::set_apart_refusal`]: an AI CLI's login file, a home path aterm does not
    /// name, a case variant of a held or consumed name), which a vault an older or modified build
    /// wrote may hold, and both names of a folded pair ([`entry_class::folded_pairs`]). The rest
    /// of the vault is applied all the same.
    pub refused: Vec<(String, String)>,
}

/// The directory under `~/.secrets` that moved-aside files go to.
pub const REPLACED_DIR: &str = ".replaced";

const MAX_ASIDE_SUFFIX: u32 = 1000;

const NAME_MAX: usize = 255;

const ASIDE_DIGEST: usize = 16;

/// Lowercase hex SHA-256 of `bytes`: a manifest digest.
pub fn digest_hex(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(bytes))
}

/// Where entry `name` lands under `home`.
pub fn target_path(home: &Path, name: &str) -> crate::Result<PathBuf> {
    if crate::reserved_name(name) {
        return Err(Error::new("a reserved entry has no plaintext file path"));
    }
    // A vault an older or modified build wrote one into still opens; its file is never written,
    // nor taken back once the entry goes ([`Report::refused`]): an AI CLI's own login, or a file
    // of the Mac's own, stands there.
    if let Some(why) = entry_class::set_apart_refusal(name) {
        return Err(Error::new(why));
    }
    written_path(home, name)
}

/// Where entry `name` landed under `home` in a build that did not reserve it: [`target_path`]
/// without the reserved-name refusal, for taking back the file such a build wrote.
fn written_path(home: &Path, name: &str) -> crate::Result<PathBuf> {
    if !text::valid_entry_name(name) {
        return Err(Error::new(
            "an invalid entry name: a file name [A-Za-z0-9][A-Za-z0-9._-]{0,127} or a ~/ home path",
        ));
    }
    Ok(match name.strip_prefix("~/") {
        Some(rel) => rel
            .split('/')
            .fold(home.to_path_buf(), |path, component| path.join(component)),
        None => aterm_types::vault_entry::plain_entry_path(home, name),
    })
}

/// The name a moved-aside copy of entry `name` gets (before any `.1`, `.2` suffix).
pub fn aside_name(name: &str, now: u64) -> String {
    let flat = name.replace('/', "%2F");
    let stamp = format!(".{now}");
    let suffix = format!(".{}", MAX_ASIDE_SUFFIX - 1).len();
    let room = NAME_MAX - suffix - stamp.len();
    if flat.len() <= room {
        return format!("{flat}{stamp}");
    }
    let digest = digest_hex(name.as_bytes());
    let mut keep = room - 1 - ASIDE_DIGEST;
    while !flat.is_char_boundary(keep) {
        keep -= 1;
    }
    format!("{}~{}{stamp}", &flat[..keep], &digest[..ASIDE_DIGEST])
}

/// Make the files under `home` match `entries`, given what the vault wrote last time.
pub fn apply(
    home: &Path,
    entries: &[Entry],
    held: &Manifest,
    now: u64,
) -> crate::Result<(Manifest, Report)> {
    apply_resuming(
        home,
        entries,
        held,
        &Pending::new(),
        now,
        &mut |_, _| Ok(()),
    )
}

/// Entry name → value digest of what a pass is to write, or a pass cut short was writing.
pub type Pending = BTreeMap<String, String>;

/// [`apply`], resuming a pass cut short and saving ahead for this one.
pub fn apply_resuming(
    home: &Path,
    entries: &[Entry],
    held: &Manifest,
    resumed: &Pending,
    now: u64,
    ahead: &mut dyn FnMut(&Manifest, &Pending) -> crate::Result<()>,
) -> crate::Result<(Manifest, Report)> {
    // Control records stay in the encrypted vault cache. Filtering before any
    // path/standing/pending computation also prevents reporting their keys as
    // ordinary materialization failures or treating them as files to restore.
    // A reserved name the held manifest still names is a file a build that did not
    // reserve it wrote (the fleet code before it was reserved): it is taken back below,
    // once this pass's writes are done, as a dropped entry's file is.
    let written_reserved: Vec<(String, String)> = held
        .files
        .iter()
        .filter(|(name, _)| crate::reserved_name(name))
        .map(|(name, digest)| (name.clone(), digest.clone()))
        .collect();
    // Only what the classification writes as files is the vault's (DESIGN-claude-setup-token
    // §3.2): an entry set apart by its name (an AI CLI's login, a home path aterm does not name,
    // a case variant of a held or consumed name) is refused, never written, and a file a build
    // that did not refuse it wrote there is the Mac's own now, neither taken back nor kept in the
    // manifest. So are both names of a folded pair, which a Mac's disk makes one file.
    let folded = entry_class::folded_pairs(entries.iter().map(|entry| entry.name.as_str()));
    let refused: Vec<(String, String)> = entries
        .iter()
        .filter_map(|entry| {
            let why = match folded.get(entry.name.as_str()) {
                Some(other) => Some(format!(
                    "{}, so neither is written",
                    entry_class::folded_pair_refusal(&entry.name, other)
                )),
                None => entry_class::set_apart_refusal(&entry.name),
            };
            why.map(|why| (entry.name.clone(), why))
        })
        .collect();
    let folded: BTreeSet<String> = folded.keys().map(|name| (*name).to_owned()).collect();
    let set_apart = |name: &str| !entry_class::of(name).materialized() || folded.contains(name);
    let ordinary;
    let entries = if entries.iter().any(|entry| set_apart(&entry.name)) {
        ordinary = entries
            .iter()
            .filter(|entry| !set_apart(&entry.name))
            .cloned()
            .collect::<Vec<_>>();
        ordinary.as_slice()
    } else {
        entries
    };
    let ordinary_held = Manifest {
        files: held
            .files
            .iter()
            .filter(|(name, _)| !set_apart(name))
            .map(|(name, digest)| (name.clone(), digest.clone()))
            .collect(),
    };
    let held = &ordinary_held;
    let secrets = home.join(".secrets");
    let places = Places::new(home, &secrets, now);
    let mut resumed_held = held.clone();
    for (name, digest) in resumed {
        let Ok(target) = target_path(home, name) else {
            continue;
        };
        if !places.reachable(name) {
            continue;
        }
        sweep_temps(&target, digest);
        if held.files.get(name) == Some(digest) {
            continue;
        }
        let wrote = fsx::read_private(&target, text::MAX_VALUE)
            .ok()
            .flatten()
            .is_some_and(|bytes| digest_hex(bytes.as_slice()) == *digest);
        if wrote {
            resumed_held.files.insert(name.clone(), digest.clone());
        }
    }
    let held = &resumed_held;
    let names: BTreeSet<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
    let history = history(home, &names, held);
    let by_name: BTreeMap<&str, &Entry> = entries
        .iter()
        .map(|entry| (entry.name.as_str(), entry))
        .collect();
    let standings = standings(&places, entries, held, &history);
    let pending = to_write(&places, entries, held, &history, &standings);
    if written_reserved.is_empty() {
        ahead(held, &pending)?;
    } else {
        // Saved ahead with the reserved names' records, so a pass cut short leaves their files
        // for the next pass to take back.
        let mut kept = held.clone();
        kept.files.extend(written_reserved.iter().cloned());
        ahead(&kept, &pending)?;
    }
    let mut manifest = Manifest::default();
    let mut report = Report {
        refused,
        ..Report::default()
    };
    let mut claimed: BTreeMap<(u64, u64), String> = BTreeMap::new();
    let mut spared: BTreeSet<(u64, u64)> = BTreeSet::new();
    let mut withdrawn: Vec<(String, String)> = Vec::new();
    // Each directory a write renamed a file into: the entries written there, and the digest
    // each held before ([`flush_written`]).
    let mut written_in: BTreeMap<PathBuf, Vec<(String, Option<String>)>> = BTreeMap::new();
    for entry in entries {
        let mut held_digest = held.files.get(&entry.name);
        let placed = match target_path(home, &entry.name) {
            Err(error) => Err(error.0),
            Ok(_) if !text::anchor_received(entry, |name| by_name.get(name).copied()) => {
                Err(format!(
                    "its anchor `{}` is not an if-absent entry this machine receives",
                    entry.anchor.as_deref().unwrap_or_default()
                ))
            }
            Ok(target) => match places.prepare(&entry.name) {
                Err(why) => {
                    spared.extend(fsx::file_id(&target));
                    if entry.if_absent && places.linked(&entry.name) {
                        Ok(if held_digest.is_some() {
                            Placed::Held
                        } else {
                            Placed::Left
                        })
                    } else {
                        Err(why)
                    }
                }
                Ok(()) => {
                    if held_digest.is_none() {
                        held_digest =
                            fsx::file_id(&target).and_then(|id| history.get(&id).copied());
                    }
                    let digest = digest_hex(entry.value.as_slice());
                    let unannounced = !entry.if_absent
                        && !pending.contains_key(&entry.name)
                        && held_digest != Some(&digest)
                        && matches!(
                            fsx::kind(&target),
                            Ok(Kind::Missing | Kind::File | Kind::Symlink)
                        );
                    let standing = standings
                        .get(entry.name.as_str())
                        .copied()
                        .unwrap_or(Standing::Unseen);
                    let placed = if unannounced && adoptable(&target, entry, held_digest) {
                        match fsx::file_id(&target).and_then(|id| claimed.get(&id)) {
                            Some(other) => {
                                Err(format!("lands on the same file as entry `{other}`"))
                            }
                            None => narrow(&target).map(|()| Placed::Adopted),
                        }
                    } else if unannounced {
                        Err(format!(
                            "{}: changed while this pass looked at it; the next pass places it",
                            target.display()
                        ))
                    } else {
                        place(
                            &places,
                            entry,
                            &target,
                            held_digest,
                            standing,
                            &claimed,
                            &mut report,
                        )
                    };
                    if let Some(id) = fsx::file_id(&target) {
                        claimed.entry(id).or_insert_with(|| entry.name.clone());
                    }
                    placed
                }
            },
        };
        match placed {
            Ok(Placed::Written) => {
                manifest
                    .files
                    .insert(entry.name.clone(), digest_hex(entry.value.as_slice()));
                report.written.push(entry.name.clone());
                if let Some(dir) = target_path(home, &entry.name)
                    .ok()
                    .and_then(|target| target.parent().map(Path::to_path_buf))
                {
                    written_in
                        .entry(dir)
                        .or_default()
                        .push((entry.name.clone(), held_digest.cloned()));
                }
            }
            Ok(Placed::Unchanged) => {
                manifest
                    .files
                    .insert(entry.name.clone(), digest_hex(entry.value.as_slice()));
                report.unchanged.push(entry.name.clone());
            }
            Ok(Placed::Adopted) => report.unchanged.push(entry.name.clone()),
            Ok(Placed::Left) => report.left.push(entry.name.clone()),
            Ok(Placed::Withdrawn) => {
                withdrawn.extend(held_digest.map(|digest| (entry.name.clone(), digest.clone())));
            }
            Ok(Placed::Held) => {
                if let Some(digest) = held_digest {
                    manifest.files.insert(entry.name.clone(), digest.clone());
                }
                report.left.push(entry.name.clone());
            }
            Err(why) => {
                if let Some(digest) = held_digest {
                    manifest.files.insert(entry.name.clone(), digest.clone());
                }
                report.failed.push((entry.name.clone(), why));
            }
        }
    }
    flush_written(&written_in, &mut manifest, &mut report);
    let dropped = held
        .files
        .iter()
        .filter(|(name, _)| !names.contains(name.as_str()));
    for (name, digest) in dropped.chain(withdrawn.iter().map(|(name, digest)| (name, digest))) {
        let Ok(target) = target_path(home, name) else {
            continue;
        };
        let id = fsx::file_id(&target);
        if id.is_some_and(|id| claimed.get(&id).is_some_and(|by| by != name)) {
            continue;
        }
        if !places.reachable(name) || id.is_some_and(|id| spared.contains(&id)) {
            manifest.files.insert(name.clone(), digest.clone());
            report.left.push(name.clone());
            continue;
        }
        take_back(home, name, &target, digest).book(name, digest, &mut manifest, &mut report);
    }
    // A reserved name's file is never the vault's to keep: removed while it holds what the
    // vault wrote, left (and out of the manifest) once the person changed it or where this
    // pass does not look, and kept in the manifest only when its removal failed, so the next
    // pass tries again.
    for (name, digest) in &written_reserved {
        let Ok(target) = written_path(home, name) else {
            continue;
        };
        let id = fsx::file_id(&target);
        if id.is_some_and(|id| claimed.contains_key(&id)) {
            continue;
        }
        if !places.reachable(name) || id.is_some_and(|id| spared.contains(&id)) {
            report.left.push(name.clone());
            continue;
        }
        take_back(home, name, &target, digest).book(name, digest, &mut manifest, &mut report);
    }
    Ok((manifest, report))
}

/// What [`take_back`] did with a file the vault wrote and no longer places.
enum TakenBack {
    /// Gone already, or under a file where no file can be.
    Gone,
    /// Held what the vault wrote, and removed.
    Removed,
    /// Changed since the vault wrote it, or not a plain file: left as it is.
    Left,
    /// Could not be looked at or removed: why.
    Failed(String),
}

impl TakenBack {
    /// Book what happened to entry `name`'s file, which the vault wrote with digest `digest`:
    /// a failed removal keeps the record, so the next pass tries again.
    fn book(self, name: &str, digest: &str, manifest: &mut Manifest, report: &mut Report) {
        match self {
            TakenBack::Gone => {}
            TakenBack::Removed => report.removed.push(name.to_owned()),
            TakenBack::Left => report.left.push(name.to_owned()),
            TakenBack::Failed(why) => {
                manifest.files.insert(name.to_owned(), digest.to_owned());
                report.failed.push((name.to_owned(), why));
            }
        }
    }
}

/// Remove `target`, entry `name`'s file under `home`, if it still holds the value of digest
/// `digest`; never through a link.
fn take_back(home: &Path, name: &str, target: &Path, digest: &str) -> TakenBack {
    match fsx::kind(target) {
        Ok(Kind::Missing) => TakenBack::Gone,
        Ok(Kind::File) => {
            match fsx::wipe_if(target, text::MAX_VALUE, |bytes| digest_hex(bytes) == digest) {
                Ok(true) => TakenBack::Removed,
                Ok(false) => TakenBack::Left,
                Err(error) => TakenBack::Failed(error.0),
            }
        }
        Ok(Kind::Symlink | Kind::Dir | Kind::Other) => TakenBack::Left,
        Err(_) if under_a_file(home, name) => TakenBack::Gone,
        Err(error) => TakenBack::Failed(error.0),
    }
}

/// Flush once each directory this pass's writes renamed files into (`written_in`), after their
/// ordered writes ([`fsx::write_ordered`]): N files cost N ordered flushes and one durable flush
/// per directory, not two durable flushes each, and their renames are durable before the state
/// that names them is saved. An entry whose directory's flush failed (other than as one the
/// volume cannot flush) failed, with the digest it held before kept, as a write that failed
/// after its rename always has: the next pass writes it again.
fn flush_written(
    written_in: &BTreeMap<PathBuf, Vec<(String, Option<String>)>>,
    manifest: &mut Manifest,
    report: &mut Report,
) {
    for (dir, written) in written_in {
        let Err(error) = aterm_durable::Flush::Durable.dir_best_effort(dir) else {
            continue;
        };
        for (name, held) in written {
            report.written.retain(|done| done != name);
            match held {
                Some(digest) => manifest.files.insert(name.clone(), digest.clone()),
                None => manifest.files.remove(name),
            };
            report
                .failed
                .push((name.clone(), format!("{}: {error}", dir.display())));
        }
    }
}

fn to_write(
    places: &Places<'_>,
    entries: &[Entry],
    held: &Manifest,
    history: &BTreeMap<(u64, u64), &String>,
    standings: &BTreeMap<&str, Standing>,
) -> Pending {
    entries
        .iter()
        .filter_map(|entry| {
            let target = target_path(places.home, &entry.name).ok()?;
            let (kind, id) = if entry.if_absent {
                if standings.get(entry.name.as_str()) != Some(&Standing::Vaults) {
                    return None;
                }
                (None, None)
            } else {
                if !places.reachable(&entry.name) {
                    return None;
                }
                let (kind, id) = fsx::kind_and_id(&target).ok()?;
                if !matches!(kind, Kind::Missing | Kind::File | Kind::Symlink) {
                    return None;
                }
                (Some(kind), id)
            };
            let digest = digest_hex(entry.value.as_slice());
            let wrote = held.files.get(&entry.name).or_else(|| {
                id.or_else(|| fsx::file_id(&target))
                    .and_then(|id| history.get(&id).copied())
            });
            let adopted = !entry.if_absent
                && wrote.is_none()
                && kind == Some(Kind::File)
                && holds(&target, entry.value.as_slice());
            (wrote != Some(&digest) && !adopted).then(|| (entry.name.clone(), digest))
        })
        .collect()
}

fn history<'m>(
    home: &Path,
    names: &BTreeSet<&str>,
    held: &'m Manifest,
) -> BTreeMap<(u64, u64), &'m String> {
    let mut history = BTreeMap::new();
    for (name, digest) in &held.files {
        if names.contains(name.as_str()) {
            continue;
        }
        if let Some(id) = target_path(home, name)
            .ok()
            .and_then(|target| fsx::file_id(&target))
        {
            history.entry(id).or_insert(digest);
        }
    }
    history
}

struct Places<'a> {
    home: &'a Path,
    secrets: &'a Path,
    /// `~/.secrets` made private, or why it was refused (a symlink, not a directory, another
    /// user's): the reason every entry that lands there, and every move aside, fails with.
    secrets_ok: Result<(), String>,
    /// What moved-aside copies are named by ([`aside_name`]).
    now: u64,
    /// aterm's own key and state directory (`~/.config/aterm/secrets`, links followed) by
    /// (device, inode): a home path that reaches it by another spelling (through the real
    /// directory a symlinked `~/.config` names) is refused as a symlinked directory is, so no
    /// entry lands on the machine key or the manifest.
    own: Option<(u64, u64)>,
}

impl<'a> Places<'a> {
    fn new(home: &'a Path, secrets: &'a Path, now: u64) -> Places<'a> {
        Places {
            home,
            secrets,
            secrets_ok: fsx::ensure_private_dir(secrets).map_err(|error| error.0),
            now,
            own: own_dir(home),
        }
    }

    fn prepare(&self, name: &str) -> Result<(), String> {
        match name.strip_prefix("~/") {
            Some(rel) => make_parents(self.home, rel, self.own),
            None => self.secrets_ok.clone(),
        }
    }

    /// Whether a directory on the way to home path `name`'s file is a symlink (a dotfiles
    /// setup's `~/.config`, say): an ` if-absent` entry there is the machine's own, never
    /// written through the link and not a failure, as a link in the file's own place is.
    fn linked(&self, name: &str) -> bool {
        linked(self.home, name, self.own)
    }

    fn reachable(&self, name: &str) -> bool {
        match name.strip_prefix("~/") {
            Some(rel) => refused_dir(self.home, rel, self.own).is_none(),
            None => self.secrets_ok.is_ok(),
        }
    }
}

enum Placed {
    Written,
    Unchanged,
    /// A plain entry whose target, a file the vault never wrote, already held its value:
    /// narrowed and kept, and still the machine's own (out of the manifest), so dropping the
    /// entry leaves it and a new value moves it aside first.
    Adopted,
    /// An ` if-absent` entry whose target, or anchor, is the machine's own, or whose target
    /// is behind a symlinked directory ([`Places::linked`]).
    Left,
    /// An anchored ` if-absent` entry whose file the vault wrote and whose anchor is the
    /// machine's own: taken back, as a dropped entry is.
    Withdrawn,
    /// An ` if-absent` entry whose file the vault wrote and which could not be looked at (its
    /// anchor, or its own file behind a symlinked directory): left as it is, and still the
    /// vault's.
    Held,
}

/// How an ` if-absent` entry's file stands, with whatever it is anchored to ([`standings`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Standing {
    /// The vault's: a gap, or a file still holding what the vault wrote. As an anchor, it
    /// lets the entries anchored to it fill their gaps and update what the vault wrote.
    Vaults,
    /// The machine's own: a file the vault never wrote or that changed since, a link, a
    /// directory.
    Own,
    /// Not looked at (seen only through a symlinked or refused directory, or an error):
    /// as an anchor, it lets nothing be written and takes nothing back.
    Unseen,
}

enum Judged {
    /// The new value already.
    Same,
    /// What the vault last wrote (or nothing any more): overwrite it.
    Ours,
    /// Something the vault did not write, or edited since, or unreadable: move it aside.
    Foreign,
}

fn place(
    places: &Places<'_>,
    entry: &Entry,
    target: &Path,
    held: Option<&String>,
    standing: Standing,
    claimed: &BTreeMap<(u64, u64), String>,
    report: &mut Report,
) -> Result<Placed, String> {
    let (kind, id) = fsx::kind_and_id(target).map_err(|error| error.0)?;
    if let Some(other) = id.and_then(|id| claimed.get(&id)) {
        return Err(format!("lands on the same file as entry `{other}`"));
    }
    let value = entry.value.as_slice();
    if entry.if_absent {
        return fill(target, kind, value, held, standing);
    }
    let aside = match kind {
        Kind::Missing => false,
        Kind::Dir => return Err(format!("{}: a directory is in the way", target.display())),
        Kind::Other => return Err(format!("{}: not a regular file", target.display())),
        Kind::Symlink => true,
        Kind::File => match judge(target, value, held) {
            Judged::Same => {
                narrow(target)?;
                return Ok(if held.is_some() {
                    Placed::Unchanged
                } else {
                    Placed::Adopted
                });
            }
            Judged::Ours => false,
            Judged::Foreign => true,
        },
    };
    if aside {
        let to = move_aside(places, &entry.name, target)?;
        report.moved_aside.push((entry.name.clone(), to));
    }
    fsx::write_ordered(target, value, 0o600).map_err(|error| error.0)?;
    Ok(Placed::Written)
}

fn fill(
    target: &Path,
    kind: Kind,
    value: &[u8],
    held: Option<&String>,
    standing: Standing,
) -> Result<Placed, String> {
    let changed = || {
        Err(format!(
            "{}: became this machine's own while this pass ran; the next pass judges it again",
            target.display()
        ))
    };
    match kind {
        Kind::Missing if standing == Standing::Vaults => {
            if fsx::write_new_ordered(target, value, 0o600).map_err(|error| error.0)? {
                Ok(Placed::Written)
            } else {
                changed()
            }
        }
        Kind::Missing => Ok(Placed::Left),
        Kind::File if held.is_some() => match (judge(target, value, held), standing) {
            (Judged::Foreign, Standing::Vaults) => changed(),
            (Judged::Foreign, _) => Ok(Placed::Left),
            (Judged::Same, Standing::Vaults) => {
                narrow(target)?;
                Ok(Placed::Unchanged)
            }
            (Judged::Ours, Standing::Vaults) => {
                fsx::write_ordered(target, value, 0o600).map_err(|error| error.0)?;
                Ok(Placed::Written)
            }
            (Judged::Same | Judged::Ours, Standing::Own) => Ok(Placed::Withdrawn),
            (Judged::Same | Judged::Ours, Standing::Unseen) => Ok(Placed::Held),
        },
        _ if standing == Standing::Vaults => changed(),
        Kind::File | Kind::Symlink | Kind::Dir | Kind::Other => Ok(Placed::Left),
    }
}

fn standings<'a>(
    places: &Places<'_>,
    entries: &'a [Entry],
    held: &Manifest,
    history: &BTreeMap<(u64, u64), &String>,
) -> BTreeMap<&'a str, Standing> {
    let mut standings: BTreeMap<&str, Standing> = BTreeMap::new();
    for entry in entries.iter().filter(|entry| entry.if_absent) {
        let standing = match target_path(places.home, &entry.name) {
            Ok(target) if places.reachable(&entry.name) => match fsx::kind_and_id(&target) {
                Ok((Kind::Missing, _)) => Standing::Vaults,
                Ok((Kind::File, id)) => {
                    let wrote = held
                        .files
                        .get(&entry.name)
                        .or_else(|| id.and_then(|id| history.get(&id).copied()));
                    if wrote.is_some_and(|digest| {
                        holds_or_digest(&target, entry.value.as_slice(), digest)
                    }) {
                        Standing::Vaults
                    } else {
                        Standing::Own
                    }
                }
                Ok((Kind::Symlink | Kind::Dir | Kind::Other, _)) => Standing::Own,
                Err(_) => Standing::Unseen,
            },
            _ => Standing::Unseen,
        };
        standings.insert(&entry.name, standing);
    }
    let anchors: Vec<(&str, &str)> = entries
        .iter()
        .filter(|entry| entry.if_absent)
        .filter_map(|entry| Some((entry.name.as_str(), entry.anchor.as_deref()?)))
        .collect();
    loop {
        let fallen: Vec<(&str, Standing)> = anchors
            .iter()
            .filter(|(name, _)| standings.get(name) == Some(&Standing::Vaults))
            .filter_map(|&(name, anchor)| {
                let of = standings.get(anchor).copied().unwrap_or(Standing::Unseen);
                (of != Standing::Vaults).then_some((name, of))
            })
            .collect();
        if fallen.is_empty() {
            return standings;
        }
        for (name, of) in fallen {
            standings.insert(name, of);
        }
    }
}

fn adoptable(target: &Path, entry: &Entry, wrote: Option<&String>) -> bool {
    !entry.if_absent
        && wrote.is_none()
        && matches!(fsx::kind(target), Ok(Kind::File))
        && holds(target, entry.value.as_slice())
}

fn holds(target: &Path, value: &[u8]) -> bool {
    fsx::read_public(target, text::MAX_VALUE)
        .ok()
        .flatten()
        .is_some_and(|bytes| aterm_digest::ct_eq(SecretBytes::new(bytes).as_slice(), value))
}

fn holds_or_digest(target: &Path, value: &[u8], digest: &str) -> bool {
    fsx::read_public(target, text::MAX_VALUE)
        .ok()
        .flatten()
        .is_some_and(|bytes| {
            let bytes = SecretBytes::new(bytes);
            digest_hex(bytes.as_slice()) == digest || aterm_digest::ct_eq(bytes.as_slice(), value)
        })
}

fn judge(target: &Path, value: &[u8], held: Option<&String>) -> Judged {
    match fsx::read_public(target, text::MAX_VALUE) {
        Ok(Some(bytes)) => {
            let current = SecretBytes::new(bytes);
            if aterm_digest::ct_eq(current.as_slice(), value) {
                Judged::Same
            } else if held.is_some_and(|digest| *digest == digest_hex(current.as_slice())) {
                Judged::Ours
            } else {
                Judged::Foreign
            }
        }
        Ok(None) => Judged::Ours,
        Err(_) => Judged::Foreign,
    }
}

fn narrow(target: &Path) -> Result<(), String> {
    let at = |error: std::io::Error| format!("{}: {error}", target.display());
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(target)
        .map_err(at)?;
    let meta = file.metadata().map_err(at)?;
    if meta.file_type().is_file() && meta.uid() == fsx::euid() && meta.mode() & 0o077 != 0 {
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(at)?;
    }
    Ok(())
}

fn parents(home: &Path, rel: &str) -> Vec<PathBuf> {
    let components: Vec<&str> = rel.split('/').collect();
    let mut dir = home.to_path_buf();
    components[..components.len().saturating_sub(1)]
        .iter()
        .map(|component| {
            dir.push(component);
            dir.clone()
        })
        .collect()
}

fn make_parents(home: &Path, rel: &str, own: Option<(u64, u64)>) -> Result<(), String> {
    for dir in parents(home, rel) {
        match fsx::kind_and_id(&dir).map_err(|error| error.0)? {
            (Kind::Dir, id) if own.is_some() && id == own => {
                return Err(format!(
                    "{}: is aterm's own key and state directory; refusing to write in it",
                    dir.display()
                ));
            }
            (Kind::Dir, _) => {}
            (Kind::Missing, _) => match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
                Ok(()) => {}
                Err(error)
                    if error.kind() == std::io::ErrorKind::AlreadyExists
                        && fsx::kind(&dir).is_ok_and(|kind| kind == Kind::Dir) => {}
                Err(error) => return Err(format!("{}: {error}", dir.display())),
            },
            (Kind::Symlink, _) => {
                return Err(format!(
                    "{}: is a symlink; refusing to write through it",
                    dir.display()
                ));
            }
            (Kind::File | Kind::Other, _) => {
                return Err(format!("{}: not a directory", dir.display()));
            }
        }
    }
    Ok(())
}

/// aterm's own key and state directory under `home` by (device, inode) ([`Places`]).
fn own_dir(home: &Path) -> Option<(u64, u64)> {
    std::fs::metadata(crate::client::Paths::for_home(home).dir)
        .ok()
        .map(|meta| (meta.dev(), meta.ino()))
}

/// Whether home path `name`'s file is behind a symlinked directory ([`Places::linked`]).
fn linked(home: &Path, name: &str, own: Option<(u64, u64)>) -> bool {
    name.strip_prefix("~/").is_some_and(|rel| {
        refused_dir(home, rel, own)
            .is_some_and(|dir| fsx::kind(&dir).is_ok_and(|kind| kind == Kind::Symlink))
    })
}

/// Whether entry `name`'s file under `home` is behind a symlinked directory: a pass never
/// writes or looks through the link, and holds a file the vault wrote there as it was
/// ([`Placed::Held`]), so whoever checks the vault's files are intact does not judge it either.
pub fn behind_a_link(home: &Path, name: &str) -> bool {
    linked(home, name, own_dir(home))
}

/// Whether entry `name`'s file under `home` holds the value of manifest digest `digest`.
pub fn holds_digest(home: &Path, name: &str, digest: &str) -> bool {
    target_path(home, name)
        .ok()
        .and_then(|target| fsx::read_private(&target, text::MAX_VALUE).ok().flatten())
        .is_some_and(|bytes| digest_hex(bytes.as_slice()) == digest)
}

/// Whether entry `name`'s file under `home` is a gap a pass writes: missing, with no directory
/// on its way that a pass refuses (a symlink, or one it does not own) ([`Places::reachable`]).
/// A bare name's way is `~/.secrets` ([`secrets_reachable`]).
pub fn is_gap(home: &Path, name: &str) -> bool {
    let reachable = match name.strip_prefix("~/") {
        Some(rel) => refused_dir(home, rel, own_dir(home)).is_none(),
        None => secrets_reachable(home),
    };
    reachable
        && target_path(home, name)
            .is_ok_and(|target| matches!(fsx::kind(&target), Ok(Kind::Missing)))
}

/// Whether a pass writes in `~/.secrets` under `home`, judged without making or narrowing it:
/// it is missing (a pass makes it), or a directory, not a symlink, this user owns (a pass narrows
/// its mode). [`fsx::ensure_private_dir`] refuses anything else, so a pass writes no file there.
fn secrets_reachable(home: &Path) -> bool {
    match std::fs::symlink_metadata(home.join(".secrets")) {
        Ok(meta) => meta.file_type().is_dir() && meta.uid() == fsx::euid(),
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    }
}

fn refused_dir(home: &Path, rel: &str, own: Option<(u64, u64)>) -> Option<PathBuf> {
    for dir in parents(home, rel) {
        match fsx::kind_and_id(&dir) {
            Ok((Kind::Dir, id)) if own.is_none() || id != own => {}
            Ok((Kind::Dir | Kind::Symlink, _)) => return Some(dir),
            Ok((Kind::Missing | Kind::File | Kind::Other, _)) | Err(_) => return None,
        }
    }
    None
}

fn under_a_file(home: &Path, name: &str) -> bool {
    name.strip_prefix("~/").is_some_and(|rel| {
        parents(home, rel)
            .iter()
            .any(|dir| matches!(fsx::kind(dir), Ok(Kind::File | Kind::Other)))
    })
}

fn sweep_temps(target: &Path, digest: &str) {
    let (Some(dir), Some(file)) = (target.parent(), target.file_name()) else {
        return;
    };
    let Ok(listing) = std::fs::read_dir(dir) else {
        return;
    };
    let prefix = format!(".{}.tmp.", file.to_string_lossy());
    for item in listing
        .flatten()
        .filter(|item| item.file_name().to_string_lossy().starts_with(&prefix))
    {
        let temp = item.path();
        let linked = item
            .metadata()
            .is_ok_and(|meta| meta.file_type().is_file() && meta.nlink() > 1);
        if linked {
            let _ = std::fs::remove_file(&temp);
        } else {
            let _ = fsx::wipe_if(&temp, text::MAX_VALUE, |bytes| digest_hex(bytes) == digest);
        }
    }
}

fn move_aside(places: &Places<'_>, name: &str, target: &Path) -> Result<PathBuf, String> {
    places.secrets_ok.clone()?;
    let dir = places.secrets.join(REPLACED_DIR);
    fsx::ensure_private_dir(&dir).map_err(|error| error.0)?;
    let base = aside_name(name, places.now);
    for k in 0..MAX_ASIDE_SUFFIX {
        let to = if k == 0 {
            dir.join(&base)
        } else {
            dir.join(format!("{base}.{k}"))
        };
        if fsx::kind(&to).map_err(|error| error.0)? != Kind::Missing {
            continue;
        }
        let moving = fsx::file_id(target);
        match fsx::rename_noreplace(target, &to) {
            Ok(()) => return Ok(to),
            Err(_) if fsx::file_id(&to).is_some_and(|id| Some(id) != moving) => {}
            Err(error) => return Err(error.0),
        }
    }
    Err(format!(
        "{}: no free name for a moved-aside copy",
        dir.display()
    ))
}

#[cfg(test)]
#[path = "materialize_tests.rs"]
mod tests;
