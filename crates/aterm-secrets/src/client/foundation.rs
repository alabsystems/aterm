// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Durable Commons foundation for an existing vault. A retained descriptor is
//! retried byte-for-byte; an already published root is finished, never replaced.
//! `aterm secrets commons init` founds it at a person's word ([`admin_commons_init`]); the
//! converge pass of a vault's only admin founds it by itself ([`commons_foundable`],
//! [`admin_commons_found_by_default`]), under the same authority. One founding runs on a Mac
//! at a time ([`FoundationLock`]).

use super::*;
use crate::commons::{
    bootstrap::{Availability, Bootstrap, Descriptor},
    node::Node,
};
use aterm_uds::file_lock::LockFile;

const PREPARED: &str = "commons-foundation";

/// The lock one founding on this Mac holds from its read of the vault and of a kept attempt
/// ([`prepare`]) through the publication that carries it ([`FoundationLock`]).
const LOCK: &str = "commons-foundation.lock";

/// How long `aterm secrets commons init` waits for another founding on this Mac to let
/// [`LOCK`] go: a window's founding by default reads the vault, publishes once and syncs,
/// each exchange under its request's own deadline (a publication's is `transport::EXCHANGE`).
const WAITS_OUT: Duration = Duration::from_secs(600);

/// The first release whose approver shares the Commons root with every machine it lets in, an
/// open approval included (`enrollment::include_approved`), and counts an `Open` decision as
/// admission. An approver before it records `Open` for a machine an open vault lets in but
/// leaves it outside the control entry's audience, which a later aterm reads as an admin's
/// explicit exclusion, so no aterm ever shares the root with it. Since aterm 0.150 an approver
/// lets no Mac in by an open vault's policy at all; this guards against the older ones still
/// running.
const SHARES_ON_OPEN: locker_skew::Version = (0, 145, 0);

/// Why this Mac's approver would leave each Mac it lets in after a founding by default outside
/// Commons, where it would: a program its subscriber runs or starts next
/// ([`locker_skew::approvers`]) is a release before [`SHARES_ON_OPEN`] or one whose release
/// cannot be read, or those programs cannot be named. This program reads as its own release,
/// which is at or after it. `None` where every one shares the root, or no subscriber is installed.
fn approver_strands(paths: &Paths) -> Option<String> {
    let remedy = "so the founding waits: `aterm secrets agent on` moves the approver onto this \
                  aterm, or `agent on --program` onto an aterm 0.145.0 or later, and the next \
                  pass founds it; `aterm secrets commons init` founds it now, by hand, leaving \
                  each Mac this approver lets in afterwards to be approved again by hand \
                  (`aterm secrets approve <its age1… key> --name <host>`)";
    let running = match locker_skew::own()
        .map_err(|error| error.to_string())
        .and_then(|own| locker_skew::approvers(paths, &own))
    {
        Ok(running) => running,
        Err(why) => {
            return Some(format!(
                "whether this Mac's approver shares the Commons root on an open approval is \
                 unknown ({why}), {remedy}"
            ));
        }
    };
    let old = running
        .iter()
        .find(|program| !matches!(program.version, Ok(version) if version >= SHARES_ON_OPEN))?;
    let (release, shares) = match &old.version {
        Ok(version) => (
            format!("aterm {}", locker_skew::dotted(*version)),
            "shares no Commons root",
        ),
        Err(why) => (
            format!("a program whose release is unknown: {why}"),
            "may share no Commons root",
        ),
    };
    Some(format!(
        "this Mac's approver ({}, {release}) {shares} on an open approval, as aterm {} and later \
         do, so each Mac it lets in after a founding would stay out of Commons; {remedy}",
        old.program.display(),
        locker_skew::dotted(SHARES_ON_OPEN),
    ))
}

/// Public outcome of a Commons foundation or its idempotent retry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommonsInit {
    /// The network whose founder history is installed locally.
    pub network: [u8; 16],
    /// The independently retained Commons authority.
    pub authority: [u8; 32],
    /// The signed genesis shared by every replica.
    pub genesis: [u8; 32],
    /// Vault machines eligible to see the control descriptor and request membership.
    pub eligible: usize,
    /// Whether this call published the foundation.
    pub published: bool,
    /// Vault machines, by name, this call approved for Commons: at `commons init`, `Manual` on a
    /// closed vault and the open vault's standing approval (`Open`) on an open one (its admin and
    /// a `code-only` key `Manual`), with the foundation or, on a root an earlier aterm founded, in
    /// a publication of their own; the founder and `Open` at a founding by default.
    pub approved: Vec<String>,
}

/// Whether this Mac founds its vault's Commons by itself ([`commons_foundable`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Foundable {
    /// This Mac opens no admin locker of its vault, or has no signed vault.
    NotAdmin,
    /// The vault's Commons control entry is published: enrollment, not foundation, is left.
    Founded,
    /// It is not this Mac's to found by itself: why, and what founds it where anything does.
    Elsewhere(String),
    /// This Mac, the vault's only admin, founds it: what it finds.
    Due(String),
    /// This Mac, the vault's only admin, founds it once a person gives what the founding waits
    /// on (an approver that shares the root on an open approval, `approver_strands`): why,
    /// and what gives it.
    Waiting(String),
    /// The vault's state is changing (a sync holds its lock), as the Commons observation reads
    /// it busy: the next read finds it settled.
    Busy,
}

/// Why a founding by default did not finish ([`admin_commons_found_by_default`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotFounded {
    /// Asked again it may finish: another founding on this Mac holds its lock, another publish
    /// landed first, or the vault, the network or a lock did not answer. Why.
    Again(String),
    /// Asked again it meets the same answer until a person changes something: why, and what
    /// changes it.
    Stands(String),
}

impl std::fmt::Display for NotFounded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Again(why) | Self::Stands(why) => f.write_str(why),
        }
    }
}

/// A failure on the way, read by its kind ([`next`]): one whose kind says the next try meets
/// the same answer (the vault server's 403 or 507, say) stands; any other may pass by itself.
impl From<Error> for NotFounded {
    fn from(error: Error) -> Self {
        match next(error.kind()) {
            Next::Same => Self::Stands(format!(
                "{error}: asked again, it meets the same answer until someone changes what that \
                 names; `aterm secrets commons init` shows it whole"
            )),
            Next::Passes | Next::Unsaid => Self::Again(error.to_string()),
        }
    }
}

/// What the next try of a step meets after it failed with an error of a kind ([`next`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Next {
    /// The same answer, until someone changes the server, the vault, its path or this Mac.
    Same,
    /// Maybe not: a lock or a writer that lets go, a request past its deadline, a vault that
    /// moved under the read, or a publish that lost a race.
    Passes,
    /// The kind does not say ([`crate::ErrorKind::Other`]): the step that failed tells.
    Unsaid,
}

/// What the next try meets after an error of `kind`, as its kind says. Every kind is read here,
/// so a new one is placed when it is added.
const fn next(kind: crate::ErrorKind) -> Next {
    use crate::ErrorKind as Kind;
    match kind {
        Kind::Refused | Kind::LabIdentity | Kind::Dropped | Kind::MembershipCapacity => Next::Same,
        Kind::JournalBusy
        | Kind::CustodyBusy
        | Kind::VaultBusy
        | Kind::VaultChanged
        | Kind::MembershipHistoryUnavailable
        | Kind::TimedOut
        | Kind::JoinQueueIncomplete
        | Kind::NotStored
        | Kind::MemberBusy => Next::Passes,
        Kind::Other => Next::Unsaid,
    }
}

/// One founding of this vault's Commons on this Mac at a time: a person's `aterm secrets
/// commons init` and a window's founding by default each hold it from their read of the vault
/// and of a kept attempt ([`prepare`]) through the publication that carries it, so neither
/// publishes from a copy of the vault the other's publication made stale, which the server
/// refuses (its 409). Let go by `LOCK_UN` as it drops ([`LockFile`]).
struct FoundationLock {
    _held: LockFile,
}

impl FoundationLock {
    /// The lock, waited for up to [`WAITS_OUT`]: a person's command waits out a window's
    /// founding.
    fn held_waiting(paths: &Paths) -> crate::Result<Self> {
        Self::held_by(paths, Instant::now() + WAITS_OUT)?.ok_or_else(|| {
            Error::new(format!(
                "another founding of this vault's Commons on this Mac held {} for {} minutes: run \
                 it again once it ends",
                paths.dir.join(LOCK).display(),
                WAITS_OUT.as_secs() / 60
            ))
        })
    }

    /// The lock where it is free; `None` while another founding on this Mac holds it.
    fn held_if_free(paths: &Paths) -> crate::Result<Option<Self>> {
        Self::held_by(paths, Instant::now())
    }

    /// The lock, taken by `deadline` ([`LockFile::take`], which tries once at least); `None`
    /// while another holder has it then.
    fn held_by(paths: &Paths, deadline: Instant) -> crate::Result<Option<Self>> {
        fsx::ensure_private_dir(&paths.dir)?;
        let path = paths.dir.join(LOCK);
        match LockFile::take(&path, deadline, "another founding holds it") {
            Ok(held) => Ok(Some(Self { _held: held })),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(Error::new(format!("{}: {error}", path.display()))),
        }
    }
}

fn prepared(paths: &Paths) -> crate::Result<Option<Descriptor>> {
    let Some(bytes) = fsx::read_private(&paths.dir.join(PREPARED), envelope::MAX_ENVELOPE)? else {
        return Ok(None);
    };
    Descriptor::decode(bytes.as_slice()).map(Some)
}

/// The names a founding's own history leaves in `commons/` before the vault describes it: the
/// runtime's lease and the device custody `prepare` writes.
const OWN_HISTORY: [&str; 3] = ["runtime.lock", "device.age", "device.claim"];

/// Whether `name` in `commons` is a founding's own: one of [`OWN_HISTORY`], or a temporary a
/// write of one left behind (`aterm_uds::atomic_file::create_temp_beside`'s
/// `.<name>.tmp.<pid>.<salt>`, which `fsx::write_new` writes each through), which a kill or
/// power loss mid-write leaves for good.
fn own_history(commons: &Path, name: &std::ffi::OsStr) -> bool {
    OWN_HISTORY.iter().any(|own| {
        name.to_str() == Some(own)
            || aterm_uds::atomic_file::temp_writer(&commons.join(own), name).is_some()
    })
}

fn refuse_unrelated_history(paths: &Paths) -> crate::Result<()> {
    let commons = paths.dir.join("commons");
    match fsx::kind(&commons)? {
        fsx::Kind::Missing => return Ok(()),
        fsx::Kind::Dir => {}
        fsx::Kind::Symlink | fsx::Kind::File | fsx::Kind::Other => {
            return Err(Error::new(format!(
                "Commons foundation cannot replace the existing commons path {}: {MOVE_ASIDE}",
                commons.display()
            )));
        }
    }
    let entries = std::fs::read_dir(&commons)
        .map_err(|error| Error::new(format!("{}: {error}", commons.display())))?;
    for entry in entries {
        let entry = entry.map_err(|error| Error::new(format!("{}: {error}", commons.display())))?;
        if !own_history(&commons, &entry.file_name()) {
            return Err(Error::new(format!(
                "Commons foundation cannot replace existing local history: {}: {MOVE_ASIDE}",
                entry.path().display()
            )));
        }
    }
    Ok(())
}

/// What lets a founding go on past local Commons history it does not replace.
const MOVE_ASIDE: &str = "move it out of the way if nothing of it is kept, and the next founding \
                          goes on";

/// What lets a founding go on past an attempt this Mac kept under another authority.
fn kept_elsewhere(paths: &Paths) -> String {
    format!(
        "move {} out of the way, and the next founding founds the vault under its own",
        paths.dir.join(PREPARED).display()
    )
}

fn audience(admin: &Admin) -> Audience {
    let mut hosts = admin
        .doc
        .machines()
        .into_iter()
        .filter(|m| !enrollment::machine_denied_in_doc(&admin.doc, &m.name, &m.recipient))
        .map(|machine| machine.name)
        .collect::<BTreeSet<_>>();
    hosts.insert(admin.config.host.clone());
    Audience::Hosts(hosts.into_iter().collect())
}

/// The machines `doc`'s control entry shares the root with that hold no admission
/// decision, which the admin founding Commons approves in its own publication ([`approve`]).
/// Members let in before decisions were recorded, or by an aterm that recorded none,
/// could otherwise never enroll (owner, 2026-10-27: "BY DEFAULT ON all machines running
/// aterm find each other"). A recorded decision is never replaced, and a machine outside
/// the entry's audience is never approved, since an approval would bring it in.
fn vouched(admin: &Admin, doc: &Document) -> Vec<text::Machine> {
    let Some(Audience::Hosts(hosts)) = doc.commons_audience() else {
        return Vec::new();
    };
    doc.machines()
        .into_iter()
        .filter(|machine| hosts.contains(&machine.name))
        .filter(|m| !enrollment::machine_denied_in_doc(doc, &m.name, &m.recipient))
        .filter(|machine| {
            Recipient::parse(&machine.recipient)
                .is_ok_and(|key| admin.admissions.evidence(&key).is_none())
        })
        .collect()
}

/// Publish `doc` with this admin's approval of `machines`, naming them. Each is recorded as the
/// way it came in can be told: on a closed vault, and for this admin's own machine and a key on a
/// `code-only` line (which an open vault's approver never lets in by itself), the admin's
/// `Manual`; on an open vault every other one as the vault's standing approval (`Open`), since
/// it may have come in with nobody approving it (`aterm secrets password open`, or an approver
/// before aterm 0.150 that let a Mac in by the open policy). A `Manual` there would say a person
/// approved a machine nobody did, and the anchored credentials (identity, roster, agh token,
/// Name) key on that boundary, which a later revocation alone could narrow again; `Open` admits
/// to Commons all the same ([`admission::Ledger::permits`]), and a person's later approval
/// replaces it.
fn approve(
    paths: &Paths,
    admin: &Admin,
    doc: &Document,
    machines: Vec<text::Machine>,
) -> crate::Result<Vec<String>> {
    let open = Approval::of(doc) == Some(Approval::Open);
    let own = machine_identity(paths)?.recipient().to_bech32();
    let code_only = doc.code_only();
    let admissions = machines
        .iter()
        .map(|machine| {
            let by_policy =
                open && machine.recipient != own && !code_only.contains(&machine.recipient);
            let method = if by_policy {
                AdmissionMethod::Open
            } else {
                AdmissionMethod::Manual
            };
            (machine.recipient.clone(), method)
        })
        .collect::<Vec<_>>();
    let _ = publish_as(paths, admin, doc, false, &admissions, false)?;
    Ok(machines.into_iter().map(|machine| machine.name).collect())
}

/// Why an admin's aterm does not found `doc`'s Commons by itself, where it does not, with what
/// changes that where anything does: the control entry is opaque; the Commons authority is
/// unbound (an unused key may still rotate, so no founding could be told current); `doc` names
/// an admin besides `own`, the recipient of this Mac, since two admins founding at once could
/// each publish a first roster under one network (docs/DESIGN-alab-commons-2026-10-24.md); or
/// the vault is not open. A closed vault's members wait for a person's approval, which `aterm
/// secrets commons init` gives as it founds, and a founding by default would leave it none to
/// give: on a root this build founded, that command approves nothing.
fn founds_by_default(doc: &Document, bound: bool, own: &str) -> Result<(), String> {
    if doc.commons_audience().is_some() {
        return Err(format!(
            "its Commons control entry is opaque (no descriptor this aterm reads), and no aterm \
             replaces it: an admin's `aterm secrets edit` that removes its `{}` entry leaves the \
             vault to be founded",
            crate::commons::ENTRY
        ));
    }
    if !bound {
        return Err(
            "its Commons authority is not bound, so no aterm founds Commons on it, and no command \
             binds one to an existing vault"
                .to_owned(),
        );
    }
    let admins = doc.admins();
    let machines = doc.machines();
    let mine = |name: &String| {
        machines
            .iter()
            .any(|machine| &machine.name == name && machine.recipient == own)
    };
    match admins.as_slice() {
        [only] if mine(only) => {}
        [only] => {
            return Err(format!(
                "this Mac is not its admin: `aterm secrets commons init`, run on {only}, founds \
                 its Commons"
            ));
        }
        _ => {
            return Err(format!(
                "it names {} admins ({}): `aterm secrets commons init`, run on one of them, \
                 founds its Commons, since two founding at once could each publish a first roster",
                admins.len(),
                admins.join(", ")
            ));
        }
    }
    if Approval::of(doc) != Some(Approval::Open) {
        return Err(
            "it is not open, so its members wait for a person's approval: `aterm secrets \
                    commons init` on its admin founds its Commons and gives it"
                .to_owned(),
        );
    }
    Ok(())
}

/// The machines of `doc`, whose founding audience names every one ([`audience`]), that the open
/// vault's standing approval admits to Commons as it is founded: each but `own` (the founder)
/// with no recorded decision and no `code-only` line, since an open vault's approver lets no
/// such key in by itself.
fn open_to(doc: &Document, admissions: &admission::Ledger, own: &str) -> Vec<text::Machine> {
    let code_only = doc.code_only();
    doc.machines()
        .into_iter()
        .filter(|machine| machine.recipient != own && !code_only.contains(&machine.recipient))
        .filter(|m| !enrollment::machine_denied_in_doc(doc, &m.name, &m.recipient))
        .filter(|machine| {
            Recipient::parse(&machine.recipient)
                .is_ok_and(|key| admissions.evidence(&key).is_none())
        })
        .collect()
}

fn prepare(paths: &Paths, admin: &Admin) -> crate::Result<Descriptor> {
    refuse_unrelated_history(paths)?;
    if let Some(descriptor) = prepared(paths)? {
        if descriptor.trust().authority != admin.commons_authority()? {
            return Err(Error::new(format!(
                "retained Commons foundation belongs to another authority: {}",
                kept_elsewhere(paths)
            )));
        }
        return Ok(descriptor);
    }
    let descriptor = Descriptor::from_genesis(Node::foundation(admin.commons_authority()?)?)?;
    let mut doc = admin.doc.clone();
    let eligible = audience(admin);
    doc.set_entry(&eligible, crate::commons::ENTRY, &descriptor.bytes())
        .map_err(|error| Error::new(error.to_string()))?;
    let descriptor = creation::enroll_founder(
        paths,
        &doc,
        &admin.config.host,
        &admin.commons_seed,
        descriptor,
    )?;
    aterm_journal::write_private_durable(&paths.dir, PREPARED, &descriptor.bytes())?;
    Ok(descriptor)
}

/// The root `admin`'s vault publishes, held to the attempt this Mac kept and to its authority
/// custody; `None` where no control entry is published.
fn published_root(paths: &Paths, admin: &Admin) -> crate::Result<Option<Descriptor>> {
    let Some(descriptor) = admin_keys::control_descriptor(&admin.doc)? else {
        return Ok(None);
    };
    if prepared(paths)?
        .as_ref()
        .is_some_and(|old| old.trust() != descriptor.trust())
    {
        return Err(Error::new(
            "published Commons foundation differs from the retained attempt",
        ));
    }
    if descriptor.trust().authority != admin.commons_authority()? {
        return Err(Error::new(
            "published Commons foundation has no matching authority custody",
        ));
    }
    Ok(Some(descriptor))
}

/// The Commons root installed here, read once this Mac's subscriber lets the vault's state lock
/// go, by `deadline`: a founding's publication wakes it, and its apply holds the lock for a
/// moment, which a read at once meets as busy ([`Bootstrap::observe_until`]).
///
/// # Errors
/// The local state, the applied copy or the Commons custody cannot be read, or the root is not
/// visible here: the state lock still held at `deadline` says the vault is busy.
fn installed(paths: &Paths, deadline: Instant) -> crate::Result<Bootstrap> {
    match Bootstrap::observe_until(paths, deadline)? {
        Availability::Available(bootstrap) => Ok(*bootstrap),
        Availability::Unavailable(reason) => {
            Err(reason.failure("published Commons control is not visible after sync"))
        }
    }
}

fn installed_after(paths: &Paths, elapsed: Duration) -> crate::Result<Bootstrap> {
    installed(
        paths,
        Instant::now() + crate::commons::SETTLE.saturating_sub(elapsed),
    )
}

fn finish(paths: &Paths, descriptor: &Descriptor, published: bool) -> crate::Result<CommonsInit> {
    let _ = sync(paths, None)?;
    #[cfg(test)]
    tests::synced();
    let bootstrap = installed_after(paths, Duration::ZERO)?;
    if bootstrap.trust() != descriptor.trust() {
        return Err(Error::new(
            "installed Commons history belongs to another foundation",
        ));
    }
    bootstrap.access()?.ok_or_else(|| {
        Error::new("Commons foundation is published but founder membership is not ready")
    })?;
    let eligible = match admin_open(paths)?.doc.commons_audience() {
        Some(Audience::Hosts(hosts)) => hosts.len(),
        _ => 0,
    };
    Ok(CommonsInit {
        network: descriptor.trust().network,
        authority: descriptor.trust().authority,
        genesis: descriptor.trust().genesis,
        eligible,
        published,
        approved: Vec::new(),
    })
}

/// Finish the published root `descriptor`, publishing nothing but, at `commons init`
/// (`vouch`), the approvals an earlier aterm's founding left out ([`vouched`]).
fn finish_published(
    paths: &Paths,
    descriptor: &Descriptor,
    vouch: bool,
) -> crate::Result<CommonsInit> {
    // Readiness first, so a run that fails has changed nothing.
    let done = finish(paths, descriptor, false)?;
    if !vouch {
        return Ok(done);
    }
    // After this build's founding every machine it shares the root with holds a decision,
    // this admin included (`audience` names it). A root whose admin holds none was founded
    // by an earlier aterm, which recorded none; on any other root an undecided machine in
    // the audience came by an edit that recorded no decision, which never admits it.
    let admin = admin_open(paths)?;
    let own = machine_identity(paths)?.recipient();
    if admin.admissions.evidence(&own).is_some() {
        return Ok(done);
    }
    let vouched = vouched(&admin, &admin.doc);
    if vouched.is_empty() {
        return Ok(done);
    }
    let approved = approve(paths, &admin, &admin.doc, vouched)?;
    Ok(CommonsInit { approved, ..done })
}

/// After `commons init`'s publication met another that landed first (the server's 409, which
/// stored nothing of it): reopen the vault once and finish the root that landed so, if one did,
/// as `commons init` finishes a published root ([`finish_published`] with `vouch`); `None`
/// where none did, and the attempt kept in [`PREPARED`] is published again at the next try.
fn landed_first(paths: &Paths) -> crate::Result<Option<CommonsInit>> {
    let admin = admin_open(paths)?;
    published_root(paths, &admin)?
        .map(|descriptor| finish_published(paths, &descriptor, true))
        .transpose()
}

/// [`published_root`] as the founding by default reads it, before it prepares anything and again
/// once another publish landed first: a root that is not the one this Mac kept or holds custody
/// of stands, since `commons init` meets the same refusal until a person moves what is in its
/// way.
fn root_by_default(paths: &Paths, admin: &Admin) -> Result<Option<Descriptor>, NotFounded> {
    published_root(paths, admin).map_err(|error| {
        NotFounded::Stands(format!(
            "{error}: `aterm secrets commons init` shows the same refusal"
        ))
    })
}

/// Initialize Commons on an existing vault, or finish an earlier uncertain attempt.
/// Existing roots and local history are never replaced. All current vault machines
/// receive the control descriptor, and the same publication records an approval of each one
/// with no admission decision (`vouched`): the admin's `Manual` on a closed vault, the open
/// vault's `Open` on an open one ([`approve`]). On a published root an earlier aterm
/// founded, which recorded no decisions, a call that finds the root ready records it for
/// each machine in the root's audience still without one; on a root this build founded it
/// approves nothing. Membership still requires the normal proofs. It waits for another
/// founding on this Mac to end ([`FoundationLock`]), and a publication that another landed
/// before finishes the root that landed so, if one did ([`landed_first`]).
///
/// # Errors
/// The caller is not an admin, the vault is unbound or has an opaque control entry,
/// unrelated local Commons history exists, the retained descriptor is for another
/// authority, another founding on this Mac held its lock past [`WAITS_OUT`], or
/// publication/bootstrap fails (where another publish landed first with no root, the
/// error, of [`crate::ErrorKind::NotStored`], says to run it again).
pub fn admin_commons_init(paths: &Paths) -> crate::Result<CommonsInit> {
    let _founding = FoundationLock::held_waiting(paths)?;
    let admin = admin_open(paths)?;
    if let Some(descriptor) = published_root(paths, &admin)? {
        return finish_published(paths, &descriptor, true);
    }
    if admin.doc.commons_audience().is_some() {
        return Err(Error::new(
            "Commons control entry is opaque; refusing to replace it",
        ));
    }
    if !admin.commons_bound {
        return Err(Error::new(
            "this vault has no bound Commons authority for foundation",
        ));
    }
    let descriptor = prepare(paths, &admin)?;
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &audience(&admin),
        crate::commons::ENTRY,
        &descriptor.bytes(),
    )
    .map_err(|error| Error::new(error.to_string()))?;
    let approved = match approve(paths, &admin, &doc, vouched(&admin, &doc)) {
        Ok(approved) => approved,
        Err(error) if error.kind() == crate::ErrorKind::NotStored => {
            return landed_first(paths)?.ok_or_else(|| {
                Error::of_kind(
                    crate::ErrorKind::NotStored,
                    format!("{error}: another publish landed first: run it again"),
                )
            });
        }
        Err(error) => return Err(error),
    };
    Ok(CommonsInit {
        approved,
        ..finish(paths, &descriptor, true)?
    })
}

/// Whether this Mac founds its vault's Commons by itself ([`admin_commons_found_by_default`]),
/// read from the applied local copy, with nothing fetched or written: it does where it opens
/// the admin locker as the vault's only admin, the vault is open, the Commons authority is
/// bound, no control entry is published (an attempt kept in `commons-foundation` under that
/// authority is finished by that founding), the person has not turned this Mac's vault sync
/// off (`aterm secrets agent off`), no unrelated local Commons history exists, and every program
/// this Mac's subscriber approves with shares the root on an open approval
/// (`approver_strands`; one that does not reads [`Foundable::Waiting`]), read with the state
/// lock let go, since each asks a program its release. Nothing a member sends enters this
/// read, and a held state lock reads [`Foundable::Busy`], so a member's row reads as the
/// observation's own busy read does.
///
/// # Errors
/// The local state or applied copy cannot be read.
pub fn commons_foundable(paths: &Paths) -> crate::Result<Foundable> {
    let Some(config) = config(paths)? else {
        return Ok(Foundable::NotAdmin);
    };
    let Some(lock) = StateLock::observe(paths)? else {
        return Ok(Foundable::Busy);
    };
    let state = load_state(paths)?;
    let Some(opened) = admin_text_here(paths, &config, &state)? else {
        return Ok(Foundable::NotAdmin);
    };
    if admin_keys::control_descriptor(&opened.doc)?.is_some() {
        return Ok(Foundable::Founded);
    }
    // The identity that opened the locker: read, never made here.
    let Some(identity) = read_identity(paths)? else {
        return Ok(Foundable::NotAdmin);
    };
    let own = identity.recipient().to_bech32();
    let name = &config.name;
    if let Err(why) = founds_by_default(&opened.doc, opened.commons_bound, &own) {
        return Ok(Foundable::Elsewhere(format!("vault {name}: {why}")));
    }
    // A founding publishes to the vault and syncs this Mac's copy, which the person stopped:
    // `sync.vault` reads the same record as their choice.
    if crate::service::subscriber_turned_off(&paths.home) {
        return Ok(Foundable::Elsewhere(format!(
            "vault {name}: sync is off on this Mac by your choice, so no pass founds its \
             Commons here: `aterm secrets agent on` lets it, or `aterm secrets commons init` \
             founds it now"
        )));
    }
    if let Err(why) = refuse_unrelated_history(paths) {
        return Ok(Foundable::Elsewhere(format!("vault {name}: {why}")));
    }
    let kept = match prepared(paths)? {
        None => "",
        Some(kept) if kept.trust().authority == envelope::signer_public(&opened.commons_seed)? => {
            " from the attempt it kept"
        }
        Some(_) => {
            return Ok(Foundable::Elsewhere(format!(
                "vault {name}: the Commons foundation this Mac kept belongs to another \
                 authority: {}",
                kept_elsewhere(paths)
            )));
        }
    };
    let others = opened.doc.machines().len().saturating_sub(1);
    let open = open_to(&opened.doc, &opened.admissions, &own).len();
    drop(lock);
    if let Some(why) = approver_strands(paths) {
        return Ok(Foundable::Waiting(format!("vault {name}: {why}")));
    }
    Ok(Foundable::Due(format!(
        "vault {name} is open and has no Commons control, and this Mac is its only admin: it \
         founds it{kept}, approving {open} of its {others} other machines by the open policy"
    )))
}

/// Found Commons on this Mac's vault by itself, as its converge pass does
/// ([`commons_foundable`]): [`admin_commons_init`]'s founding, under the same authority, where
/// this Mac is the vault's only admin, the vault is open and that authority is bound. The
/// publication that carries the descriptor records this admin as its `Founder` and the open
/// vault's standing approval (`Open`) of every other machine with no decision and no
/// `code-only` line ([`open_to`]), so each member enrolls as one the open vault lets in later
/// does. `Open` keeps the anchored credentials (identity, roster, agh token, Name) from them,
/// where a recorded `Manual` would say a person approved them. On a published root it finishes
/// what it finds and records nothing, where `commons init` adds the approvals an earlier aterm
/// left out. It founds nothing while another founding on this Mac holds its lock
/// ([`FoundationLock`]), and a publication that another landed before reopens the vault once, as
/// `commons init`'s does ([`landed_first`]), and finishes the root that landed so, if one did.
///
/// # Errors
/// [`NotFounded::Stands`] where the vault names an admin besides this Mac or is not open, its
/// Commons authority is unbound or its control entry opaque, or this Mac's approver would leave
/// the Macs it lets in later outside Commons (`approver_strands`), each before anything is
/// prepared or published; where the published root, read before anything is prepared or after
/// another publish landed first, is not the one this Mac kept or holds custody of
/// ([`root_by_default`]); where preparing meets unrelated local Commons history, an
/// attempt kept under another authority, or another failure whose kind does not say it passes;
/// and where anything on the way fails with an error whose kind says the next try meets the
/// same answer (the vault server's 403 or 507, [`crate::ErrorKind::Refused`]).
/// [`NotFounded::Again`] where another founding on this Mac holds its lock, before anything is
/// read; where another publish landed first with no root; and where anything else fails (the
/// vault, the network or a lock did not answer).
pub fn admin_commons_found_by_default(paths: &Paths) -> Result<CommonsInit, NotFounded> {
    let Some(_founding) = FoundationLock::held_if_free(paths)? else {
        return Err(NotFounded::Again(
            "another founding of this vault's Commons runs on this Mac now (`aterm secrets \
             commons init`, or another aterm's pass)"
                .to_owned(),
        ));
    };
    let admin = admin_open(paths)?;
    if let Some(descriptor) = root_by_default(paths, &admin)? {
        return finish_published(paths, &descriptor, false).map_err(NotFounded::from);
    }
    let own = machine_identity(paths)?.recipient().to_bech32();
    founds_by_default(&admin.published_doc, admin.commons_bound, &own)
        .map_err(|why| NotFounded::Stands(format!("Commons is not founded by default: {why}")))?;
    if let Some(why) = approver_strands(paths) {
        return Err(NotFounded::Stands(format!(
            "Commons is not founded by default: {why}"
        )));
    }
    let descriptor = prepare(paths, &admin).map_err(|error| match next(error.kind()) {
        Next::Passes => NotFounded::Again(error.to_string()),
        Next::Same | Next::Unsaid => NotFounded::Stands(format!(
            "Commons is not founded by default: {error}; `aterm secrets commons init` shows the \
             same refusal"
        )),
    })?;
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &audience(&admin),
        crate::commons::ENTRY,
        &descriptor.bytes(),
    )
    .map_err(|error| Error::new(error.to_string()))?;
    let admissions = std::iter::once((own.clone(), AdmissionMethod::Founder))
        .chain(
            open_to(&doc, &admin.admissions, &own)
                .into_iter()
                .map(|machine| (machine.recipient, AdmissionMethod::Open)),
        )
        .collect::<Vec<_>>();
    match publish_as(paths, &admin, &doc, false, &admissions, false) {
        Ok(_) => {}
        // Reopened once, as `commons init` does ([`landed_first`]), the root that landed is
        // read as the first read reads one: a root that is not the one this Mac kept stands.
        Err(error) if error.kind() == crate::ErrorKind::NotStored => {
            let admin = admin_open(paths)?;
            return match root_by_default(paths, &admin)? {
                Some(descriptor) => {
                    finish_published(paths, &descriptor, false).map_err(NotFounded::from)
                }
                None => Err(NotFounded::Again(format!(
                    "{error}: another publish landed first"
                ))),
            };
        }
        Err(error) => return Err(error.into()),
    }
    let approved = doc
        .machines()
        .into_iter()
        .filter(|machine| admissions.iter().any(|(key, _)| key == &machine.recipient))
        .map(|machine| machine.name)
        .collect();
    Ok(CommonsInit {
        approved,
        ..finish(paths, &descriptor, true)?
    })
}

#[cfg(test)]
mod tests;
