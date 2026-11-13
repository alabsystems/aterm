// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::admission::deadline as admission_deadline;
use crate::{
    Error, Result,
    catalog::{Descriptor, Directory, State},
    database::Command,
    table::{
        Scope, Session,
        store::{Receipt, Store},
    },
};
use aterm_secrets::{client::Paths, commons::bootstrap::Bootstrap};

mod retained;
pub(crate) use retained::{
    ProjectionCustody, RetainedCatalog, with_retained_catalog, with_retained_store,
    with_selected_store,
};

#[cfg(test)]
thread_local! {
    pub(crate) static DROP_PROJECTION_CUSTODY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Recognize only a canonical value-free join, independently of the
/// application's value or authority schema.
pub(crate) fn decode_merge(payload: &[u8]) -> Option<Command> {
    let command = Command::decode(payload).ok()?;
    if matches!(command, Command::Merge { .. }) && command.encode().ok()?.as_slice() == payload {
        Some(command)
    } else {
        None
    }
}

/// Append a typed application record, using value-free commands to join wide
/// causal frontiers. The shared application log recognizes these commands
/// independently of each application's business schema.
/// Callers validate payloads and retries first. A caller with record-count
/// budgets must also admit the additional retained merge records.
pub(crate) fn append(session: &Session<'_>, store: &mut Store, payload: &[u8]) -> Result<Receipt> {
    store.commit_catalog_with_merges(session, payload, || {
        Command::Merge {
            request: crate::crypto::random()?,
        }
        .encode()
    })
}

/// Open one application under a fresh membership observation. Callers keep
/// their own schema and descriptor; this helper grants no application authority.
pub(crate) fn with_store<T>(
    paths: &Paths,
    create: bool,
    descriptor: impl FnOnce(&Bootstrap) -> Result<Descriptor>,
    action: impl FnOnce(&Session<'_>, &mut Store, Scope) -> Result<T>,
) -> Result<T> {
    with_store_until(paths, create, descriptor, admission_deadline(), action)
}

/// Preserve one caller-owned deadline through custody and every table writer
/// acquired by this session.
pub(crate) fn with_store_until<T>(
    paths: &Paths,
    create: bool,
    descriptor: impl FnOnce(&Bootstrap) -> Result<Descriptor>,
    deadline: std::time::Instant,
    action: impl FnOnce(&Session<'_>, &mut Store, Scope) -> Result<T>,
) -> Result<T> {
    with_store_guarded_until(
        paths,
        create,
        descriptor,
        deadline,
        |_, _, _| Ok(()),
        action,
    )
}

/// Check application-specific independent custody before publication can enroll
/// storage. The caller shares the admitted control writer with this guard.
pub(crate) fn with_store_guarded_until<T>(
    paths: &Paths,
    create: bool,
    descriptor: impl FnOnce(&Bootstrap) -> Result<Descriptor>,
    deadline: std::time::Instant,
    before_publication: impl FnOnce(&Session<'_>, &Directory, &Descriptor) -> Result<()>,
    action: impl FnOnce(&Session<'_>, &mut Store, Scope) -> Result<T>,
) -> Result<T> {
    pass_current(deadline)?;
    let paths = crate::application_paths_until(paths, deadline)?;
    let paths = &paths;
    let (bootstrap, journal) =
        Bootstrap::required_with_journal_until(paths, deadline).map_err(crate::table::failure)?;
    let session = Session::from_admitted_journal_until(&bootstrap, journal, Some(deadline))?;
    in_session(
        &bootstrap,
        &session,
        create,
        descriptor,
        deadline,
        before_publication,
        action,
    )
}

/// [`with_store_until`], waiting for another local writer of the control journal until
/// `deadline`: the journal's own bounded wait ([`Session::open_until`]), which still refuses as
/// busy past it. Custody and the writer generation's selection come first, under the ordinary
/// admission ([`admission_deadline`], never past `deadline`), and custody is let go before that
/// wait, so a long wait never keeps `commons.lock` from the observations that expect it within a
/// second. Past `deadline` every wait ends and the open is refused; once the journal's writer is
/// taken, the session's later generation checks each take a fresh admission, as
/// [`Session::open_until`]'s do.
pub(crate) fn with_store_awaiting_journal<T>(
    paths: &Paths,
    create: bool,
    descriptor: impl FnOnce(&Bootstrap) -> Result<Descriptor>,
    deadline: std::time::Instant,
    action: impl FnOnce(&Session<'_>, &mut Store, Scope) -> Result<T>,
) -> Result<T> {
    pass_current(deadline)?;
    let custody = deadline.min(admission_deadline());
    let paths = crate::application_paths_until(paths, custody)?;
    let paths = &paths;
    let bootstrap = Bootstrap::required_until(paths, custody).map_err(crate::table::failure)?;
    let session = Session::open_until(&bootstrap, deadline)?;
    in_session(
        &bootstrap,
        &session,
        create,
        descriptor,
        deadline,
        |_, _, _| Ok(()),
        action,
    )
}

/// The admitted session's half of an open in `bootstrap`'s custody: `before_publication`, then
/// the descriptor's table, published when `create` and checked against the directory, then
/// `action`. Past `deadline` it is refused, checked before the descriptor, the store and `action`.
fn in_session<T>(
    bootstrap: &Bootstrap,
    session: &Session<'_>,
    create: bool,
    descriptor: impl FnOnce(&Bootstrap) -> Result<Descriptor>,
    deadline: std::time::Instant,
    before_publication: impl FnOnce(&Session<'_>, &Directory, &Descriptor) -> Result<()>,
    action: impl FnOnce(&Session<'_>, &mut Store, Scope) -> Result<T>,
) -> Result<T> {
    pass_current(deadline)?;
    let descriptor = descriptor(bootstrap)?;
    let directory = Directory::new(bootstrap.paths(), bootstrap.trust());
    before_publication(session, &directory, &descriptor)?;
    if create {
        directory.publish(session, &descriptor)?;
    }
    let snapshot = if create {
        directory.snapshot(session)?
    } else {
        directory.snapshot_existing(session)?
    };
    let entry = snapshot
        .entries
        .get(&descriptor.scope.digest())
        .ok_or_else(|| Error::new("application table is not initialized"))?;
    if entry.state != State::Subscribed
        || entry
            .declarations
            .iter()
            .any(|declaration| declaration.descriptor != descriptor)
    {
        return Err(Error::new(
            "application table has an incompatible or unavailable descriptor",
        ));
    }
    pass_current(deadline)?;
    let mut store = Store::open(
        session,
        &directory.path(descriptor.scope)?,
        descriptor.scope,
    )?;
    pass_current(deadline)?;
    action(session, &mut store, descriptor.scope)
}

/// Observe an initialized application without initializing custody or acquiring
/// either journal's writer.
/// The immutable store exposes only its verified custody-witnessed prefix.
pub(crate) fn observe_store<T>(
    paths: &Paths,
    descriptor: impl FnOnce(&Bootstrap) -> Result<Descriptor>,
    action: impl FnOnce(&Session<'_>, &Store, Scope) -> Result<T>,
) -> Result<T> {
    observe_store_until(paths, descriptor, admission_deadline(), action)
}

pub(crate) fn observe_store_until<T>(
    paths: &Paths,
    descriptor: impl FnOnce(&Bootstrap) -> Result<Descriptor>,
    deadline: std::time::Instant,
    action: impl FnOnce(&Session<'_>, &Store, Scope) -> Result<T>,
) -> Result<T> {
    observe_store_if_declared_until(paths, descriptor, deadline, action)?
        .ok_or_else(|| Error::new("application table is not initialized"))
}

/// Observe an application when its declaration is retained in the existing catalog.
/// An absent declaration is `None`; invalid custody, catalog or declared table
/// storage remains an error. This creates neither a declaration nor a store.
pub(crate) fn observe_store_if_declared<T>(
    paths: &Paths,
    descriptor: impl FnOnce(&Bootstrap) -> Result<Descriptor>,
    action: impl FnOnce(&Session<'_>, &Store, Scope) -> Result<T>,
) -> Result<Option<T>> {
    observe_store_if_declared_until(paths, descriptor, admission_deadline(), action)
}

pub(crate) fn observe_store_if_declared_until<T>(
    paths: &Paths,
    descriptor: impl FnOnce(&Bootstrap) -> Result<Descriptor>,
    deadline: std::time::Instant,
    action: impl FnOnce(&Session<'_>, &Store, Scope) -> Result<T>,
) -> Result<Option<T>> {
    observe_store_if_declared_within(paths, descriptor, deadline, &|| deadline, action)
}

/// [`observe_store_if_declared_until`] under two bounds, as
/// [`crate::generation::Observation::observe_within`] takes them: `wait` is asked as each stage
/// that waits for a holder of the vault or of custody starts (this home's and each
/// predecessor's), and says when that stage's waits end; `read_by` is when the read says it ran
/// out of time, a gate the selection, the session's generation checks and each table stage pass
/// through. The selected home's custody is opened before its predecessors are observed, and the
/// session keeps that observation ([`Session::observe_selected`]), so a successor home's
/// predecessors are observed once per read, after every wait the read makes for the selected
/// home's custody, not again when the session opens.
pub(crate) fn observe_store_if_declared_within<T>(
    paths: &Paths,
    descriptor: impl FnOnce(&Bootstrap) -> Result<Descriptor>,
    read_by: std::time::Instant,
    wait: &dyn Fn() -> std::time::Instant,
    action: impl FnOnce(&Session<'_>, &Store, Scope) -> Result<T>,
) -> Result<Option<T>> {
    pass_current(read_by)?;
    let paths = crate::generation::observed_generation_until(paths, read_by)?;
    observe_selected_if_declared(&paths, descriptor, read_by, wait, false, action)
}

/// [`observe_store_if_declared_within`] at `selected`, the home
/// [`crate::generation::observed_generation_until`] named: its established custody opened, then
/// its session ([`observed_session`]), then the declared table read ([`observe_declared`], which
/// `held` is for).
fn observe_selected_if_declared<T>(
    selected: &Paths,
    descriptor: impl FnOnce(&Bootstrap) -> Result<Descriptor>,
    read_by: std::time::Instant,
    wait: &dyn Fn() -> std::time::Instant,
    held: bool,
    action: impl FnOnce(&Session<'_>, &Store, Scope) -> Result<T>,
) -> Result<Option<T>> {
    let (bootstrap, journal) = Bootstrap::established_with_journal_until(selected, wait())
        .map_err(crate::table::failure)?;
    let session = observed_session(&bootstrap, journal, read_by, wait)?;
    observe_declared(&session, descriptor(&bootstrap)?, read_by, held, action)
}

/// A read-only session of the selected home whose established custody is `bootstrap`, with its
/// control view `journal`: its predecessors observed now, after that custody was opened, and the
/// observation kept for the session's checks. The caller names the home with
/// [`crate::generation::observed_generation_until`] and opens `bootstrap` (taking any source
/// reads it holds through the read) before this, so each predecessor is observed once per read,
/// after every wait the read makes for the selected home's custody.
pub(crate) fn observed_session<'a>(
    bootstrap: &'a Bootstrap,
    journal: aterm_secrets::commons::journal::Journal,
    read_by: std::time::Instant,
    wait: &dyn Fn() -> std::time::Instant,
) -> Result<Session<'a>> {
    #[cfg(test)]
    seams::opened();
    let selection = crate::generation::observe_generation(bootstrap.paths(), read_by, wait)?;
    #[cfg(test)]
    seams::selected();
    Session::observe_selected(bootstrap, journal, selection, read_by)
}

/// [`observe_store`] under [`observe_store_if_declared_within`]'s two bounds, for a read a person
/// waits on, given longer to read than any holder of the vault or of custody is given.
pub(crate) fn observe_store_within<T>(
    paths: &Paths,
    descriptor: impl FnOnce(&Bootstrap) -> Result<Descriptor>,
    read_by: std::time::Instant,
    wait: &dyn Fn() -> std::time::Instant,
    action: impl FnOnce(&Session<'_>, &Store, Scope) -> Result<T>,
) -> Result<T> {
    observe_store_if_declared_within(paths, descriptor, read_by, wait, action)?
        .ok_or_else(|| Error::new("application table is not initialized"))
}

/// Observe another declared table through a bounded current application session.
/// Historical sessions remain usable elsewhere; this read refuses generation changes.
pub(crate) fn observe_store_in_session<T>(
    session: &Session<'_>,
    descriptor: Descriptor,
    deadline: std::time::Instant,
    action: impl FnOnce(&Session<'_>, &Store, Scope) -> Result<T>,
) -> Result<T> {
    session.require_current_application_generation(deadline)?;
    let result = observe_declared_in_session(session, descriptor, deadline, action)?
        .ok_or_else(|| Error::new("application table is not initialized"))?;
    session.require_current_application_generation(deadline)?;
    Ok(result)
}

pub(crate) fn observe_declared_in_session<T>(
    session: &Session<'_>,
    descriptor: Descriptor,
    deadline: std::time::Instant,
    action: impl FnOnce(&Session<'_>, &Store, Scope) -> Result<T>,
) -> Result<Option<T>> {
    observe_declared(session, descriptor, deadline, false, action)
}

/// The table `descriptor` declares, observed in `session` where its catalog retains that
/// declaration. Where `held`, a catalog or declared table whose directory binding was lost to a
/// relocation is answered from its witnessed prefix if `session`'s home is a fenced original
/// whose selection record holds no completion and that holds no completion mark
/// ([`Directory::observe_held_table`]); otherwise,
/// and for every reader that does not ask, such a table refuses.
fn observe_declared<T>(
    session: &Session<'_>,
    descriptor: Descriptor,
    deadline: std::time::Instant,
    held: bool,
    action: impl FnOnce(&Session<'_>, &Store, Scope) -> Result<T>,
) -> Result<Option<T>> {
    pass_current(deadline)?;
    let directory = Directory::new(session.paths(), session.trust());
    let snapshot = if held {
        directory.snapshot_existing_held(session)?
    } else {
        directory.snapshot_existing(session)?
    };
    pass_current(deadline)?;
    let Some(entry) = snapshot.entries.get(&descriptor.scope.digest()) else {
        return Ok(None);
    };
    if entry.state != State::Subscribed
        || entry
            .declarations
            .iter()
            .any(|declaration| declaration.descriptor != descriptor)
    {
        return Err(Error::new(
            "application table has an incompatible or unavailable descriptor",
        ));
    }
    let store = if held {
        directory.observe_held_table(session, descriptor.scope)?
    } else {
        directory.observe_table(session, descriptor.scope)?
    };
    pass_current(deadline)?;
    action(session, &store, descriptor.scope).map(Some)
}

/// [`observe_store_if_declared`] on a Mac that may hold no Commons: `None` there too, as
/// [`peek_catalog`] reads one, since nothing was declared. `deadline` bounds the pass, else
/// [`admission_deadline`]. It creates no table, catalog or declaration, though asking
/// [`commons_here`] may establish this Mac's custody; invalid custody, catalog or declared table
/// storage remains an error, and a vault writer's moment is busy, never "no Commons here".
///
/// This is the one reader that answers a displaced table (SF-13): where the home it reads is a
/// fenced original whose selection record holds no completion and that holds no completion
/// mark ([`crate::generation::Preserving`]), a catalog or declared table
/// whose directory binding was lost to a relocation is read from the prefix its claim witnesses
/// ([`Directory::observe_held_table`]), each record authenticated as on any observation. That
/// table read writes nothing, backfills no volume witness and repairs no binding. A table whose
/// prefix has no witness still refuses, as does a displaced table of a home that is not fenced;
/// and every other reader here ([`observe_store_if_declared_within`] and those built on it,
/// [`observe_store_in_session`], [`peek_catalog`], [`peek_declaration`]) refuses a displaced
/// table as before.
pub(crate) fn observe_store_if_here<T>(
    paths: &Paths,
    descriptor: impl FnOnce(&Bootstrap) -> Result<Descriptor>,
    deadline: Option<std::time::Instant>,
    action: impl FnOnce(&Session<'_>, &Store, Scope) -> Result<T>,
) -> Result<Option<T>> {
    let deadline = deadline.unwrap_or_else(admission_deadline);
    let selected = crate::generation::observed_generation_until(paths, deadline)?;
    if !commons_here(&selected)? {
        return Ok(None);
    }
    observe_selected_if_declared(&selected, descriptor, deadline, &|| deadline, true, action)
}

/// This Mac's lab and the catalog it holds, read as an observer that creates no table, catalog
/// or declaration (asking [`commons_here`] may establish this Mac's custody): `None` where there
/// is no Commons here, and an empty catalog where none is held yet.
///
/// # Errors
/// Custody, membership or the catalog's storage cannot be read.
pub(crate) fn peek_catalog(
    paths: &Paths,
) -> Result<
    Option<(
        aterm_secrets::commons::journal::Trust,
        crate::catalog::Snapshot,
    )>,
> {
    let deadline = admission_deadline();
    peek_catalog_within(paths, deadline, &|| deadline)
}

/// [`peek_catalog`] under [`observe_store_if_declared_within`]'s two bounds.
///
/// # Errors
/// As [`peek_catalog`], or the read ran out of time at `read_by`.
pub(crate) fn peek_catalog_within(
    paths: &Paths,
    read_by: std::time::Instant,
    wait: &dyn Fn() -> std::time::Instant,
) -> Result<
    Option<(
        aterm_secrets::commons::journal::Trust,
        crate::catalog::Snapshot,
    )>,
> {
    let paths = crate::generation::observed_generation_until(paths, read_by)?;
    if !commons_here(&paths)? {
        return Ok(None);
    }
    let (bootstrap, journal) =
        Bootstrap::established_with_journal_until(&paths, wait()).map_err(crate::table::failure)?;
    let session = observed_session(&bootstrap, journal, read_by, wait)?;
    let directory = Directory::new(&paths, bootstrap.trust());
    Ok(Some((
        bootstrap.trust(),
        directory.snapshot_if_exists(&session)?,
    )))
}

/// What this Mac's catalog says of one table ([`peek_declaration`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Declaration {
    /// The catalog's own table id.
    pub catalog: astream_wire::sync::Digest,
    /// Any Mac declared the table in it.
    pub declared: bool,
    /// A Mac other than this one declared it.
    pub elsewhere: bool,
}

/// What this Mac's catalog says of `descriptor`'s table ([`Declaration`]), read as an observer
/// that creates no table, catalog or declaration (asking [`commons_here`] may establish this
/// Mac's custody). `None` where there is no Commons here.
///
/// # Errors
/// Custody, membership or the catalog's storage cannot be read.
pub(crate) fn peek_declaration(
    paths: &Paths,
    descriptor: impl FnOnce(&Bootstrap) -> Result<Descriptor>,
    deadline: std::time::Instant,
) -> Result<Option<Declaration>> {
    let paths = crate::generation::observed_generation_until(paths, deadline)?;
    if !commons_here(&paths)? {
        return Ok(None);
    }
    let (bootstrap, journal) = Bootstrap::established_with_journal_until(&paths, deadline)
        .map_err(crate::table::failure)?;
    let session = observed_session(&bootstrap, journal, deadline, &|| deadline)?;
    let directory = Directory::new(&paths, bootstrap.trust());
    let table = descriptor(&bootstrap)?.scope.digest();
    let me = session.actor();
    let snapshot = directory.snapshot_if_exists(&session)?;
    let entry = snapshot.entries.get(&table);
    Ok(Some(Declaration {
        catalog: directory.catalog_scope().digest(),
        declared: entry.is_some_and(|entry| !entry.declarations.is_empty()),
        elsewhere: entry.is_some_and(|entry| {
            entry
                .declarations
                .iter()
                .any(|declaration| declaration.actor != me)
        }),
    }))
}

/// Observe a Commons source without equating an unreadable vault with an absent application.
/// A signed vault with no Commons control entry is a known absence; incomplete application,
/// missing custody or a current writer must remain an error and retain the caller's view.
/// Like [`Bootstrap::observe`], an authenticated visible descriptor can establish this Mac's
/// custody. This creates no application table, catalog or declaration.
pub(crate) fn observed_bootstrap(paths: &Paths) -> Result<Option<Box<Bootstrap>>> {
    use aterm_secrets::commons::{Unavailable, bootstrap::Availability};
    match Bootstrap::observe(paths).map_err(crate::table::failure)? {
        Availability::Available(bootstrap) => Ok(Some(bootstrap)),
        Availability::Unavailable(
            Unavailable::Unconfigured
            | Unavailable::Revoked
            | Unavailable::Dropped
            | Unavailable::ControlExcluded
            | Unavailable::ControlNotVisible,
        ) => Ok(None),
        Availability::Unavailable(reason) => Err(aterm_secrets::Error::of_kind(
            if reason == Unavailable::Busy {
                aterm_secrets::ErrorKind::VaultBusy
            } else {
                aterm_secrets::ErrorKind::Other
            },
            format!(
                "application custody unavailable ({}): {}",
                reason.code(),
                reason.explanation()
            ),
        )
        .into()),
    }
}

/// Whether this installation never configured a Commons: no generation or opened custody
/// names one, and the vault observes as unconfigured or shows this Mac no Commons control
/// entry. Its one vault observation is a nonblocking shared read: it waits on no lock, takes
/// no writer and initializes nothing, so an initially busy vault refuses at once, as a cache
/// observation does, instead of waiting out a retained admission. A known vault that is
/// unavailable is an error.
pub(crate) fn never_configured(paths: &Paths, deadline: std::time::Instant) -> Result<bool> {
    use aterm_secrets::commons::{Observation, Unavailable};
    pass_current(deadline)?;
    let selected = crate::generation::observed_generation_until(paths, deadline)?;
    let known = selected != *paths
        || crate::generation::is_candidate(&selected)
        || aterm_secrets::commons::bootstrap::custody_opened(&selected)
            .map_err(crate::table::failure)?;
    match aterm_secrets::commons::observe(&selected).map_err(crate::table::failure)? {
        Observation::Unavailable(Unavailable::Unconfigured | Unavailable::ControlNotVisible)
            if !known =>
        {
            Ok(true)
        }
        Observation::Unavailable(reason) => Err(aterm_secrets::Error::of_kind(
            if reason == Unavailable::Busy {
                aterm_secrets::ErrorKind::VaultBusy
            } else {
                aterm_secrets::ErrorKind::Other
            },
            format!(
                "application custody unavailable ({}): {}",
                reason.code(),
                reason.explanation()
            ),
        )
        .into()),
        Observation::Visible(_) => Ok(false),
    }
}

/// Whether an existing, readable Commons source is available for an application read.
fn commons_here(paths: &Paths) -> Result<bool> {
    observed_bootstrap(paths).map(|bootstrap| bootstrap.is_some())
}

fn pass_current(deadline: std::time::Instant) -> Result<()> {
    if std::time::Instant::now() >= deadline {
        Err(Error::new("application pass deadline expired"))
    } else {
        Ok(())
    }
}

/// Test seams of the observation path.
#[cfg(test)]
#[path = "application_seams_tests.rs"]
pub(crate) mod seams;
