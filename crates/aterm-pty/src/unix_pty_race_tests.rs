// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::{Cell, RefCell};
use std::ffi::{CStr, CString};

std::thread_local! {
    static IN_CHILD: Cell<u32> = const { Cell::new(0) };
    static AFTER_LOGIN_TTY: Cell<u32> = const { Cell::new(0) };
    static FRESH: Cell<u32> = const { Cell::new(0) };
    static GONE_BY_NAME: Cell<u32> = const { Cell::new(0) };
    static OPEN_FAILS: Cell<u32> = const { Cell::new(0) };
    static SLAVE_OPENS_GONE: Cell<u32> = const { Cell::new(0) };
    static STRUCK_FRESH: RefCell<Vec<CString>> = const { RefCell::new(Vec::new()) };
    static STRUCK_OPENS: RefCell<Vec<CString>> = const { RefCell::new(Vec::new()) };
    static GRANTS_SPIN: Cell<u32> = const { Cell::new(0) };
    static STRUCK_GRANTS: RefCell<Vec<CString>> = const { RefCell::new(Vec::new()) };
    static REPLACED: RefCell<Vec<Option<CString>>> = const { RefCell::new(Vec::new()) };
    static GRANTS_NAP: Cell<(u32, std::time::Duration)> =
        const { Cell::new((0, std::time::Duration::ZERO)) };
    static NAPPED_GRANTS: RefCell<Vec<CString>> = const { RefCell::new(Vec::new()) };
    static LET_GO_SLOTS: RefCell<Vec<bool>> = const { RefCell::new(Vec::new()) };
}

unsafe extern "C" {
    fn revoke(path: *const libc::c_char) -> libc::c_int;
}

/// Kill the slave in the child, just before its `login_tty`, on each of this thread's next
/// `n` forks.
pub(crate) fn in_children(n: u32) {
    IN_CHILD.with(|c| c.set(n));
}

/// Kill the slave in the child just after its `login_tty` made it the controlling terminal,
/// on each of this thread's next `n` forks.
pub(crate) fn after_login_tty(n: u32) {
    AFTER_LOGIN_TTY.with(|c| c.set(n));
}

/// On each of this thread's next `n` parent checks that reach the slave's name, `stat` a
/// name that names nothing: the pty's node gone while its descriptors still answer.
pub(crate) fn gone_by_name(n: u32) {
    GONE_BY_NAME.with(|c| c.set(n));
}

/// Fail each of this thread's next `n` fresh pty opens with EAGAIN, as a pty open can fail
/// under the churn that kills ptys.
pub(crate) fn fresh_opens_fail(n: u32) {
    OPEN_FAILS.with(|c| c.set(n));
}

/// In the parent, before a fresh kit opens: whether this open is one that fails.
pub(super) fn open_fails() -> bool {
    take_one(&OPEN_FAILS)
}

/// On each of this thread's next `n` opens of a new pty's slave by its name, open a name
/// that names nothing: the pty's node gone as its slave opens (ENOENT).
pub(crate) fn slave_opens_gone(n: u32) {
    SLAVE_OPENS_GONE.with(|c| c.set(n));
}

/// The names of the ptys whose slave's open [`slave_opens_gone`] struck since the last take.
pub(crate) fn take_struck_opens() -> Vec<CString> {
    STRUCK_OPENS.with(|s| std::mem::take(&mut *s.borrow_mut()))
}

/// In the open of a new pty's slave, the name it opens: `name`, or on a struck open `name`
/// with `.gone` after it, which names nothing.
pub(super) fn opening_slave(name: CString) -> CString {
    if !take_one(&SLAVE_OPENS_GONE) {
        return name;
    }
    STRUCK_OPENS.with(|s| s.borrow_mut().push(name.clone()));
    let mut gone = name.into_bytes();
    gone.extend_from_slice(b".gone");
    CString::new(gone).unwrap_or_default()
}

/// On each of this thread's next `n` pty opens, a grant that never returns by itself, as
/// XNU's `grantpt` spins on a pty whose node could not be made (ATERM-KI-1028-4): it asks the
/// master's slot for its name again and again, the same call on the same number, until the
/// slot holds something else.
pub(crate) fn grants_spin(n: u32) {
    GRANTS_SPIN.with(|c| c.set(n));
}

/// In a replacement, once the fresh pair is open and before the replaced pty closes: that
/// pty's name as its master answers it then (`None` where no pty was kept, or it answers
/// none), the proof it was held while the fresh pair opened.
pub(super) fn replaced(gone: Option<libc::c_int>) {
    let name = gone.and_then(|fd| super::pts_name(fd).ok());
    REPLACED.with(|r| r.borrow_mut().push(name));
}

/// What [`replaced`] recorded since the last take, one entry a replacement.
pub(crate) fn take_replaced() -> Vec<Option<CString>> {
    REPLACED.with(|r| std::mem::take(&mut *r.borrow_mut()))
}

/// The names of the ptys whose grant [`grants_spin`] struck since the last take.
pub(crate) fn take_struck_grants() -> Vec<CString> {
    STRUCK_GRANTS.with(|s| std::mem::take(&mut *s.borrow_mut()))
}

/// In a pty's open, before its grant: whether this grant is one that spins (its pty's name
/// recorded).
pub(super) fn grant_spins(master: libc::c_int) -> bool {
    if !take_one(&GRANTS_SPIN) {
        return false;
    }
    let name = super::pts_name(master).unwrap_or_default();
    STRUCK_GRANTS.with(|s| s.borrow_mut().push(name));
    true
}

/// The spinning grant: `TIOCPTYGNAME` on `master`'s slot until it fails, then -1.
pub(super) fn spin_grant(master: libc::c_int) -> libc::c_int {
    let mut name = [0 as libc::c_char; super::PTS_NAME_LEN];
    // SAFETY: `name` is `PTS_NAME_LEN` writable bytes, the size `TIOCPTYGNAME` writes; it takes
    // whatever descriptor the slot holds, and fails once it holds no pty master.
    while unsafe {
        libc::ioctl(
            master,
            libc::TIOCPTYGNAME as libc::c_ulong,
            name.as_mut_ptr(),
        )
    } == 0
    {}
    -1
}

/// On each of this thread's next `n` pty opens, a grant that sleeps `nap` before it runs, as a
/// healthy grant on an opener the scheduler leaves off its core (heavy load, a page-in, a
/// stop): late, burning no CPU, and then granted.
pub(crate) fn grants_nap(n: u32, nap: std::time::Duration) {
    GRANTS_NAP.with(|c| c.set((n, nap)));
}

/// In a pty's open, before its grant: how long this grant sleeps first, if it is one that
/// does (its pty's name recorded).
pub(super) fn grant_naps(master: libc::c_int) -> Option<std::time::Duration> {
    let (left, nap) = GRANTS_NAP.with(Cell::get);
    if left == 0 {
        return None;
    }
    GRANTS_NAP.with(|c| c.set((left - 1, nap)));
    let name = super::pts_name(master).unwrap_or_default();
    NAPPED_GRANTS.with(|s| s.borrow_mut().push(name));
    Some(nap)
}

/// The names of the ptys whose grant [`grants_nap`] put to sleep since the last take.
pub(crate) fn take_napped_grants() -> Vec<CString> {
    NAPPED_GRANTS.with(|s| std::mem::take(&mut *s.borrow_mut()))
}

/// In a let-go grant's open, before the master's slot closes: whether that slot holds a pipe
/// (the watchdog's), so the number stayed reserved through the swap and the close is of a
/// descriptor the open owns.
pub(super) fn let_go_slot(master: libc::c_int) {
    // SAFETY: every field of `stat` is an integer, so all-zero is valid.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `st` is a live, writable `stat`; a closed `master` answers EBADF.
    let pipe = unsafe { libc::fstat(master, &raw mut st) } == 0
        && st.st_mode & libc::S_IFMT == libc::S_IFIFO;
    LET_GO_SLOTS.with(|s| s.borrow_mut().push(pipe));
}

/// What [`let_go_slot`] recorded since the last take, one entry a let-go grant.
pub(crate) fn take_let_go_slots() -> Vec<bool> {
    LET_GO_SLOTS.with(|s| std::mem::take(&mut *s.borrow_mut()))
}

/// This thread's seam, all of it off, and its record of struck fresh ptys cleared.
pub(crate) fn reset() {
    in_children(0);
    after_login_tty(0);
    in_fresh(0);
    gone_by_name(0);
    fresh_opens_fail(0);
    slave_opens_gone(0);
    let _ = take_struck_fresh();
    let _ = take_struck_opens();
    grants_spin(0);
    let _ = take_struck_grants();
    let _ = take_replaced();
    grants_nap(0, std::time::Duration::ZERO);
    let _ = take_napped_grants();
    let _ = take_let_go_slots();
}

/// The names of the fresh ptys [`in_fresh`] killed since the last take.
pub(crate) fn take_struck_fresh() -> Vec<CString> {
    STRUCK_FRESH.with(|s| std::mem::take(&mut *s.borrow_mut()))
}

/// Kill the slave of each of the next `n` fresh ptys this thread's spawns open, as it opens.
pub(crate) fn in_fresh(n: u32) {
    FRESH.with(|c| c.set(n));
}

fn take_one(left: &'static std::thread::LocalKey<Cell<u32>>) -> bool {
    left.with(|c| {
        let n = c.get();
        c.set(n.saturating_sub(1));
        n > 0
    })
}

/// `revoke(2)` the slave named `name`; `false` when it failed.
pub(crate) fn revoke_slave(name: &CStr) -> bool {
    // SAFETY: `name` is a NUL-terminated path `revoke` only reads.
    unsafe { revoke(name.as_ptr()) == 0 }
}

/// In the parent, before the fork onto `master`'s pty: the slave name this fork's child
/// revokes before its `login_tty` and the one it revokes after, each when this fork is one
/// of the struck ones.
pub(super) fn child_strikes(master: libc::c_int) -> (Option<CString>, Option<CString>) {
    let before = take_one(&IN_CHILD);
    let after = take_one(&AFTER_LOGIN_TTY);
    let name = || super::pts_name(master).ok();
    (before.then(name).flatten(), after.then(name).flatten())
}

/// In the child, before or after its `login_tty`: `revoke(2)` by the name its parent made;
/// no allocation, no lock (one syscall on a live C string).
pub(super) fn in_child(name: Option<&CString>) {
    if let Some(name) = name {
        revoke_slave(name);
    }
}

/// In the parent, as a fresh kit of `master`'s pty opens.
pub(super) fn opened_fresh(master: libc::c_int) {
    if take_one(&FRESH)
        && let Ok(name) = super::pts_name(master)
    {
        revoke_slave(&name);
        STRUCK_FRESH.with(|s| s.borrow_mut().push(name));
    }
}

/// In the parent's check, the slave name it `stat`s: `name`, or on a struck check `name`
/// with `.gone` after it, which names nothing.
pub(super) fn by_name(name: CString) -> CString {
    if !take_one(&GONE_BY_NAME) {
        return name;
    }
    let mut gone = name.into_bytes();
    gone.extend_from_slice(b".gone");
    CString::new(gone).unwrap_or_default()
}
