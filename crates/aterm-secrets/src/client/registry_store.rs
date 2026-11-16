// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The vault as the lab registry's secret store (tracker LT-04B): put, read and remove one
//! `[everyone]` value a registry cell names, by the routes `aterm secrets set` and `unset` take
//! (a member's write where this Mac is seated in the vault's log, else an admin's publish). A
//! value crosses only as [`SecretBytes`]; nothing here prints or logs one, and a read is in place
//! ([`super::read_in_place`]), never from a file.
//!
//! A value goes in only under [`InPlace`], this Mac's finding that every Mac reading the vault
//! runs a build that writes no vault entry to a file ([`fleet_in_place`]): a build that still
//! does would put the value in plain text under `~/.secrets`. The judgment fails closed. Every
//! current Commons roster member must have reported the first such release
//! ([`hearing::NO_SECRET_FILE`]) or later within [`hearing::RECENT`] ([`hearing::in_place`]),
//! this device by its own running build ([`hearing::Version::own`]) and by every other aterm
//! process it runs ([`local_holding`]: an app an update has not relaunched still runs the build
//! that writes files); one unheard, one with no
//! report and one with an older report each hold the put back, however long this device watched,
//! and while no release is named every Mac holds it. Every Mac that reads the vault's
//! `[everyone]` must be such a member: a reader the roster does not account for runs a build
//! nobody heard. The readers are read from where a put writes ([`everyone_readers`]): each
//! current seat of the vault's log and every recipient a newest `[everyone]` epoch is wrapped to,
//! each Mac its historical roster names, and, on an admin, each machine of its published text.
//! A source this Mac should read and cannot refuses the judgment, never shrinks the set, and each
//! put checks again that the readers it reaches now are among those judged
//! ([`InPlace::covers`]). A removal needs no finding: it takes a value out of every Mac's reach.

use super::Paths;
use crate::{
    Error, SecretBytes,
    age::Recipient,
    commons::{
        binding::Binding,
        bootstrap::Bootstrap,
        hearing::{self, Hold, Ledger},
    },
    membership_log::EpochAudience,
    text,
};
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

/// How long a finding stands: one command's puts, never a later run's.
const STANDS: Duration = Duration::from_secs(10 * 60);

/// What holds the registry's puts back, counted by kind and never naming a Mac or a value
/// ([`floor`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Holding {
    /// Roster members heard within [`hearing::RECENT`] on a release below the floor (with no
    /// release named, every member heard on any release).
    pub older: usize,
    /// Members heard within [`hearing::RECENT`] whose rounds reported no version.
    pub silent: usize,
    /// Members not heard within [`hearing::RECENT`].
    pub unheard: usize,
    /// Macs that read the vault from outside the Commons roster.
    pub outside: usize,
    /// Other aterm processes on this Mac whose build is below the floor or cannot be told
    /// ([`local_holding`]): an app not yet relaunched after an update still runs the build that
    /// writes files, whatever this running build is.
    pub here: usize,
    /// The first release that writes no secret file, as judged (`None`: none is named).
    pub release: Option<hearing::Version>,
}

impl Holding {
    /// How many Macs hold the puts back.
    #[must_use]
    pub const fn macs(&self) -> usize {
        self.older + self.silent + self.unheard + self.outside
    }

    /// Whether nothing holds the puts back: no Mac, and no other aterm process on this one.
    #[must_use]
    pub const fn clear(&self) -> bool {
        self.macs() == 0 && self.here == 0
    }

    /// The refusal this holding makes: how many Macs hold the put back and why.
    #[must_use]
    pub fn refusal(&self) -> Error {
        let floor = self.release.map_or_else(
            || "a release that writes no secret file, and none is named yet".to_owned(),
            |release| format!("aterm {release} or later"),
        );
        let mut why = Vec::new();
        for (count, words) in [
            (self.older, "run an older aterm"),
            (self.silent, "reported no version"),
            (self.unheard, "were not heard in the last 7 days"),
            (
                self.outside,
                "read the vault from outside the Commons roster",
            ),
        ] {
            if count > 0 {
                why.push(format!(
                    "{count} {} {words}",
                    if count == 1 { "Mac" } else { "Macs" }
                ));
            }
        }
        if self.here > 0 {
            why.push(format!(
                "{} other aterm {} on this Mac {} an older or unknown build (quit or relaunch it)",
                self.here,
                if self.here == 1 {
                    "process"
                } else {
                    "processes"
                },
                if self.here == 1 { "runs" } else { "run" },
            ));
        }
        Error::new(format!(
            "no registry secret is written until every Mac reading the vault runs {floor}: {}",
            why.join(", ")
        ))
    }
}

/// This Mac's finding that every Mac reading its vault writes no vault entry to a file, judged at
/// one moment for one vault ([`fleet_in_place`]). It has no public constructor outside tests.
#[derive(Debug)]
pub struct InPlace {
    vault: String,
    until: Instant,
    /// Every reader the judgment counted: a put that would reach another is refused.
    readers: BTreeSet<Recipient>,
}

impl InPlace {
    /// Whether the finding is still within its bound: a holder judges again once it is not.
    #[must_use]
    pub fn stands(&self) -> bool {
        Instant::now() <= self.until
    }

    /// Refuse the finding for any vault but `vault`, or once it is older than its bound.
    fn stands_for(&self, vault: &str) -> crate::Result<()> {
        if self.vault != vault {
            return Err(Error::new(
                "the fleet's builds were judged for another vault; judge this one",
            ));
        }
        if !self.stands() {
            return Err(Error::new(
                "the fleet's builds were judged too long ago; judge them again",
            ));
        }
        Ok(())
    }

    /// Refuse a put whose readers, read where it writes now, include one the judgment did not
    /// count: a Mac seated, wrapped to or added to the published text since then runs a build
    /// nobody judged.
    ///
    /// # Errors
    /// A reader of `reached` is not among the judged readers.
    pub fn covers(&self, reached: &BTreeSet<Recipient>) -> crate::Result<()> {
        if reached.is_subset(&self.readers) {
            Ok(())
        } else {
            Err(Error::new(
                "a Mac reads the vault that this Mac's judgment of the fleet did not count; \
                 no registry secret is written until it is judged again",
            ))
        }
    }

    /// A finding for the vault at `paths` that judged no fleet, for tests whose vault has no
    /// Commons ([`fleet_in_place`] refuses one).
    ///
    /// # Errors
    /// No vault is configured there.
    #[cfg(any(test, debug_assertions))]
    pub fn for_test(paths: &Paths) -> crate::Result<Self> {
        Ok(Self {
            vault: vault_name(paths)?,
            until: Instant::now() + STANDS,
            readers: everyone_readers(paths)?,
        })
    }

    /// A finding that counted exactly `readers`, for a test of [`Self::covers`].
    #[cfg(test)]
    fn counting(readers: BTreeSet<Recipient>) -> Self {
        Self {
            vault: String::new(),
            until: Instant::now() + STANDS,
            readers,
        }
    }
}

/// The name of the vault this Mac writes: what a reference to one of its entries names.
///
/// # Errors
/// No vault is configured here.
pub fn vault_name(paths: &Paths) -> crate::Result<String> {
    super::config(paths)?
        .map(|config| config.name)
        .ok_or_else(|| Error::new("no vault configured; run aterm secrets join"))
}

/// Judge, at `now_ms`, whether every Mac that reads the vault at `paths` writes no vault entry to
/// a file (module docs), and so whether a registry secret may be put into it.
///
/// # Errors
/// No vault is configured; this Mac holds no Commons roster, so it cannot tell which builds the
/// fleet runs; it can read no list of the vault's readers; or a Mac holds the put back. The
/// refusal says how many Macs hold it and why, never a value.
pub fn fleet_in_place(paths: &Paths, now_ms: u64) -> crate::Result<InPlace> {
    let vault = vault_name(paths)?;
    let (holding, readers) = judged(paths, now_ms)?;
    floor(&holding)?;
    Ok(InPlace {
        vault,
        until: Instant::now() + STANDS,
        readers,
    })
}

/// What holds the registry's puts back on this Mac's vault at `now_ms` ([`floor`]), for a count
/// that says so without asking for a put.
///
/// # Errors
/// As [`fleet_in_place`], short of the holding itself.
pub fn fleet_holding(paths: &Paths, now_ms: u64) -> crate::Result<Holding> {
    vault_name(paths)?;
    judged(paths, now_ms).map(|(holding, _)| holding)
}

/// The holding [`fleet_in_place`] judges, with the readers it counted.
fn judged(paths: &Paths, now_ms: u64) -> crate::Result<(Holding, BTreeSet<Recipient>)> {
    let refuse = |why: &str| Error::new(format!("no registry secret is written: {why}"));
    let bootstrap = Bootstrap::cached(paths)?
        .ok_or_else(|| refuse("this Mac holds no Commons roster to tell which builds Macs run"))?;
    let access = bootstrap
        .access()?
        .ok_or_else(|| refuse("this Mac has no access to the Commons roster"))?;
    let readers = everyone_readers(paths)?;
    let ledger = Ledger::read(&bootstrap);
    let mut holding = holding(
        &ledger,
        access.members().iter().map(|member| member.binding()),
        &readers,
        now_ms,
        hearing::NO_SECRET_FILE,
    );
    holding.here = local_holding(local_builds().as_deref(), hearing::NO_SECRET_FILE);
    Ok((holding, readers))
}

/// The judgment [`fleet_in_place`] makes, from what it read: `ledger`, this device's record of
/// what each member reported; `roster`, the current Commons members; `readers`, every Mac that
/// reads the vault's `[everyone]`; and `release`, the first release that writes no secret file
/// (`None`: none is named, and every member holds the put back).
#[must_use]
pub fn holding<'a>(
    ledger: &Ledger,
    roster: impl IntoIterator<Item = &'a Binding>,
    readers: &BTreeSet<Recipient>,
    now_ms: u64,
    release: Option<hearing::Version>,
) -> Holding {
    let roster: Vec<&Binding> = roster.into_iter().collect();
    let held = ledger.holding_below(roster.iter().copied(), now_ms, hearing::in_place(release));
    let members: BTreeSet<Recipient> = roster.iter().map(|binding| binding.recipient()).collect();
    let count = |kind: fn(&Hold) -> bool| held.iter().filter(|(_, hold)| kind(hold)).count();
    Holding {
        older: count(|hold| matches!(hold, Hold::Older(Some(_)))),
        silent: count(|hold| matches!(hold, Hold::Older(None))),
        unheard: count(|hold| matches!(hold, Hold::Unheard | Hold::Unwatched)),
        outside: readers.difference(&members).count(),
        here: 0,
        release,
    }
}

/// How many of this Mac's other aterm processes hold the puts back: each whose build is below
/// `release`, or whose build could not be told (`None`), and, with no `release` named, every one.
/// `builds` is `None` where this Mac's process table could not be read: that holds as one.
/// The ledger judges this device by its running build ([`hearing::Version::own`]) alone; an app
/// on the same Mac that an update has not relaunched runs the build that writes files.
#[must_use]
pub fn local_holding(
    builds: Option<&[Option<hearing::Version>]>,
    release: Option<hearing::Version>,
) -> usize {
    let Some(builds) = builds else {
        return 1;
    };
    builds
        .iter()
        .filter(|build| match (build, release) {
            (Some(build), Some(release)) => *build < release,
            _ => true,
        })
        .count()
}

/// The build of each other live aterm process of this user on this Mac: the release its bundle
/// names, or `None` where the image it runs is no longer the file at its path (an update replaced
/// it) or its bundle names no release. `None` where the process table cannot be read, and on a
/// platform with no reader here.
fn local_builds() -> Option<Vec<Option<hearing::Version>>> {
    #[cfg(target_os = "macos")]
    {
        let me = std::process::id();
        let records = aterm_sysprobe::kproc::records()?;
        let uid = records.iter().find(|r| r.pid == me)?.uid;
        Some(
            records
                .into_iter()
                .filter(|r| r.uid == uid && !r.zombie && r.pid != me && r.comm == "aterm")
                .map(|r| image_release(r.pid))
                .collect(),
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// The release of the bundle whose executable `pid` runs, where that image is still the file at
/// its path.
#[cfg(target_os = "macos")]
fn image_release(pid: u32) -> Option<hearing::Version> {
    use std::os::unix::fs::MetadataExt as _;
    let path = aterm_sysprobe::process_path(pid)?;
    let image = aterm_sysprobe::kproc::image_file(pid)?;
    let file = std::fs::metadata(&path).ok()?;
    if file.dev() != image.device || file.ino() != image.inode {
        return None;
    }
    let plist = std::fs::read_to_string(path.parent()?.parent()?.join("Info.plist")).ok()?;
    bundle_release(&plist)
}

/// The `CFBundleShortVersionString` of an XML `Info.plist`, read as a release.
#[cfg(any(target_os = "macos", test))]
fn bundle_release(plist: &str) -> Option<hearing::Version> {
    let (_, rest) = plist.split_once("<key>CFBundleShortVersionString</key>")?;
    let (value, _) = rest
        .trim_start()
        .strip_prefix("<string>")?
        .split_once("</string>")?;
    hearing::Version::parse(value.trim())
}

/// Refuse `holding` where any Mac holds the put back.
///
/// # Errors
/// A Mac holds it; the refusal counts each kind.
pub fn floor(holding: &Holding) -> crate::Result<()> {
    if holding.clear() {
        Ok(())
    } else {
        Err(holding.refusal())
    }
}

/// Every Mac that reads what a put into the vault's `[everyone]` writes: where this Mac holds the
/// vault's log, each current seat and every recipient a newest `[everyone]` epoch is wrapped to
/// ([`log_readers`]), since a member's put is sealed under one of those epochs or a fresh one
/// wrapped to the seats; each Mac its historical roster names; and where this Mac is an admin,
/// each machine of the published text, which an admin's put publishes to and its mirror of the
/// log copies into ([`published_readers`]).
///
/// # Errors
/// A source this Mac should read cannot be read, or it reads neither the log nor a published
/// text, so it can list no reader of what a put writes.
fn everyone_readers(paths: &Paths) -> crate::Result<BTreeSet<Recipient>> {
    let config = super::config(paths)?
        .ok_or_else(|| Error::new("no vault configured; run aterm secrets join"))?;
    let mut readers = BTreeSet::new();
    let log = super::log_entries::trusted_configured_log(paths, &config)?;
    if let Some(configured) = &log {
        readers.extend(log_readers(&configured.log));
    }
    if let Some(roster) = super::historical_swarm_roster(paths)? {
        for member in &roster.members {
            readers.insert(recipient(&member.recipient)?);
        }
    }
    let published = published_readers(super::can_administer(paths), || {
        doc_readers(super::admin_cached(paths)?.published_document())
    })?;
    if log.is_none() && published.is_none() {
        return Err(Error::new(
            "no registry secret is written: this Mac reads neither the vault's log nor its \
             published text, so it cannot list the vault's readers",
        ));
    }
    readers.extend(published.into_iter().flatten());
    Ok(readers)
}

/// The readers of a put a member signs into `log`: each current seat, and every recipient a
/// `Wrap` of a newest `[everyone]` epoch hands its key to, a seat or not (a Mac let in through
/// the envelope alone, an unproved founder), since the put may be sealed under that epoch.
fn log_readers(log: &crate::membership_log::Log) -> BTreeSet<Recipient> {
    let mut readers = log.current_recipients(&EpochAudience::Everyone);
    for epoch in log.newest_epochs(&EpochAudience::Everyone) {
        readers.extend(log.wrapped_to(&epoch));
    }
    readers
}

/// The machines of the published text where this Mac is an admin (`admin`, whether it can
/// administer, [`super::can_administer`]), read by `machines`; `None` where it is no admin.
///
/// # Errors
/// This Mac cannot tell whether it is an admin, or it is one and cannot read the published text
/// now (its state is changing, or its copy does not open): either way a reader may go uncounted,
/// so the judgment refuses rather than leaving them out.
fn published_readers(
    admin: crate::Result<bool>,
    machines: impl FnOnce() -> crate::Result<BTreeSet<Recipient>>,
) -> crate::Result<Option<BTreeSet<Recipient>>> {
    let refuse = |why: &str, error: &Error| {
        Error::new(format!("no registry secret is written: {why}: {error}"))
    };
    match admin {
        Ok(false) => Ok(None),
        Ok(true) => machines().map(Some).map_err(|error| {
            refuse(
                "the published text this admin writes cannot be read",
                &error,
            )
        }),
        Err(error) => Err(refuse(
            "this Mac cannot tell whether it publishes the vault's text",
            &error,
        )),
    }
}

/// Each machine of `doc`, the published text: who reads what it holds.
fn doc_readers(doc: &text::Document) -> crate::Result<BTreeSet<Recipient>> {
    doc.machines()
        .iter()
        .map(|machine| recipient(&machine.recipient))
        .collect()
}

/// A reader's recipient; one that does not parse is a reader nobody can account for.
fn recipient(text: &str) -> crate::Result<Recipient> {
    Recipient::parse(text).map_err(|_| {
        Error::new("a vault reader's recipient does not parse; no registry secret is written")
    })
}

/// What a put or a removal did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryWrite {
    /// In the vault, and this Mac's copy holds it.
    Here,
    /// Signed or waiting on this Mac: not yet in the vault, and this Mac's vault pass carries it.
    Pending,
}

/// Refuse a name that is no plain shared entry: the registry's store reaches no fleet
/// configuration, release material, control record, Claude Code token or set-apart name.
fn shared_name(name: &str) -> crate::Result<()> {
    if text::valid_entry_name(name)
        && !crate::reserved_name(name)
        && matches!(
            crate::entry_class::of(name),
            crate::entry_class::EntryClass::Shared
        )
    {
        Ok(())
    } else {
        Err(Error::new("not a registry secret's store entry name"))
    }
}

/// Put `value` into the vault's `[everyone]` entry `name`, replacing what it holds. `in_place` is
/// this Mac's finding that every Mac reading the vault writes no vault entry to a file
/// ([`fleet_in_place`]); one judged for another vault, or one older than its bound, is refused.
///
/// # Errors
/// The name is no plain shared entry, `in_place` does not stand for this vault now, the value is
/// empty or too large, the vault refuses the write (another write took it first, this Mac is not
/// seated, the vault stayed busy past `by`), or the vault cannot be written.
pub fn put_entry(
    paths: &Paths,
    in_place: &InPlace,
    name: &str,
    value: SecretBytes,
    by: Instant,
) -> crate::Result<EntryWrite> {
    shared_name(name)?;
    in_place.stands_for(&vault_name(paths)?)?;
    if value.as_slice().is_empty() {
        return Err(Error::new("an empty value is a removal, never a put"));
    }
    if value.as_slice().len() > text::MAX_VALUE {
        return Err(Error::new("the value is larger than 1 MiB"));
    }
    if super::write_route(paths, by)? != super::WriteRoute::NoLog {
        // The readers this put reaches now: the log it is signed into and, on an admin, the
        // published text its mirror copies it into.
        let config = super::config(paths)?
            .ok_or_else(|| Error::new("no vault configured; run aterm secrets join"))?;
        let configured = super::log_entries::trusted_configured_log(paths, &config)?
            .ok_or_else(|| Error::new("the vault's log this put is signed into cannot be read"))?;
        in_place.covers(&log_readers(&configured.log))?;
        if let Some(published) = published_readers(super::can_administer(paths), || {
            doc_readers(super::admin_cached(paths)?.published_document())
        })? {
            in_place.covers(&published)?;
        }
        let entry = super::MemberEntry {
            name: name.to_owned(),
            value,
            if_absent: false,
            anchor: super::Anchor::Keep,
            fleet_reads_anchors: false,
            // A put stands over whatever the entry holds: each value a write puts takes an
            // entry of its own, so nothing newer is ever replaced.
            read_as: None,
            settles: None,
        };
        return match super::member_write(
            paths,
            std::slice::from_ref(&entry),
            super::OnBusy::Wait(by),
        )? {
            super::MemberWritten::Posted => {
                applied(paths);
                Ok(EntryWrite::Here)
            }
            super::MemberWritten::SignedHere | super::MemberWritten::Waiting(_) => {
                Ok(EntryWrite::Pending)
            }
            super::MemberWritten::Overtaken(_) => Err(Error::new(
                "another write took this store entry first; put it again to replace it",
            )),
            super::MemberWritten::Refused(why) => Err(Error::new(why)),
            super::MemberWritten::Busy => Err(Error::new("this Mac's vault stayed busy")),
            super::MemberWritten::NotSeated => {
                Err(Error::new("this Mac has no seat in the vault's log"))
            }
            super::MemberWritten::NoLog => Err(Error::new("this Mac's vault has no log")),
        };
    }
    let admin = super::admin_open(paths)?;
    let mut doc = admin.doc.clone();
    in_place.covers(&doc_readers(&doc)?)?;
    doc.set_entry(&text::Audience::Everyone, name, value.as_slice())
        .map_err(|error| Error::new(error.to_string()))?;
    let mut admin = admin;
    admin.resolving(&text::Audience::Everyone, name);
    super::admin_publish(paths, &admin, &doc)?;
    Ok(EntryWrite::Here)
}

/// Remove the vault's `[everyone]` entry `name`; an entry the vault does not hold is removed
/// already.
///
/// # Errors
/// The name is no plain shared entry, the vault refuses the removal, or the vault cannot be
/// written.
pub fn remove_entry(paths: &Paths, name: &str, by: Instant) -> crate::Result<EntryWrite> {
    shared_name(name)?;
    if super::write_route(paths, by)? != super::WriteRoute::NoLog {
        return match super::member_unset_names(paths, &[name], by)? {
            super::MemberUnset::Posted => {
                applied(paths);
                Ok(EntryWrite::Here)
            }
            super::MemberUnset::Absent | super::MemberUnset::TookBack => Ok(EntryWrite::Here),
            super::MemberUnset::SignedHere => Ok(EntryWrite::Pending),
            super::MemberUnset::Overtaken(_) => Err(Error::new(
                "another write put this store entry back after its removal",
            )),
            super::MemberUnset::Refused(why) => Err(Error::new(why)),
            super::MemberUnset::Busy => Err(Error::new("this Mac's vault stayed busy")),
            super::MemberUnset::NotSeated => {
                Err(Error::new("this Mac has no seat in the vault's log"))
            }
            super::MemberUnset::NoLog => Err(Error::new("this Mac's vault has no log")),
        };
    }
    let admin = super::admin_open(paths)?;
    let mut doc = admin.doc.clone();
    if !doc
        .unset_entry(&text::Audience::Everyone, name)
        .map_err(|error| Error::new(error.to_string()))?
    {
        return Ok(EntryWrite::Here);
    }
    super::admin_publish(paths, &admin, &doc)?;
    Ok(EntryWrite::Here)
}

/// Apply a member write a server took to this Mac's copy; a failure leaves it to the next vault
/// pass, as `aterm secrets set` does.
fn applied(paths: &Paths) {
    if super::admin_cached(paths).is_ok() {
        let _ = super::admin_mirror_log(paths, &[]);
    } else {
        let _ = super::sync(paths, None);
    }
}

#[cfg(test)]
#[path = "registry_store_tests.rs"]
mod tests;
