// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Kernel lifetime of an admitted child's process group, independently of guard descriptors.
//!
//! A host and worker can both die while a compiler that closed inherited descriptors keeps
//! running. A free file lock then proves no holder remains, not that the admitted work ended.
//! [`Cohort`] captures the actual owned child's self-led group before work is acknowledged.
//! Its lifetime check never sends a signal and never infers group drain from a dead leader,
//! a missing descriptor, or a process-directory enumeration. It conservatively retains a
//! reservation on an unreadable boot, another pid namespace, or any failed kernel check.

use aterm_uds::process;
use aterm_uds::spawnfd::Child;
use std::io;
use std::time::{Duration, Instant};

/// How long a spawned child that has exited has to become reapable. A live read refuses a process
/// before `wait` can reap it, for about 300 µs when it is a few milliseconds old, as a launch's
/// capture finds it; a retried bind can find an older child, for which this was not measured
/// (`docs/measured/cohort-capture-at-exit-2026-11-04.md`). A test's wait for what must happen is
/// a hang detector, so there it is a minute.
const LEAVES_WITHIN: Duration = if cfg!(test) {
    Duration::from_secs(60)
} else {
    Duration::from_secs(1)
};

/// The pause between looks at a child on its way out.
const LEAVING_LOOK: Duration = Duration::from_micros(100);

/// What one look at a just-spawned child found.
enum Look {
    Live(Cohort),
    Ended,
    /// Exited, and not yet reapable: another look can say more.
    Leaving,
    Refused,
}

fn look(child: &mut Child, scope: &Scope) -> io::Result<Look> {
    Ok(match Cohort::of_child(child) {
        Ok(cohort) if cohort.scope == *scope => Look::Live(cohort),
        _ if ended(child)? => Look::Ended,
        _ if exited(child) => Look::Leaving,
        _ => Look::Refused,
    })
}

/// Whether the owned `child` has ended, reaping it if so, with the resource accounting that reap
/// collects where this platform's kernel gives it ([`Child::try_wait_with_rusage`]), so the
/// child's owner still reads its own resident peak ([`Child::usage`]: a supervised build's floor)
/// once a capture reaped it. A child reaped already has ended.
///
/// # Errors
/// The reap's.
fn ended(child: &mut Child) -> io::Result<bool> {
    if child.is_reaped() {
        return Ok(true);
    }
    match child.try_wait_with_rusage() {
        Ok(usage) => Ok(usage.is_some()),
        Err(error) if error.kind() == io::ErrorKind::Unsupported => Ok(child.try_wait()?.is_some()),
        Err(error) => Err(error),
    }
}

fn exited(child: &Child) -> bool {
    i32::try_from(child.id()).is_ok_and(|pid| {
        matches!(
            process::process_presence(pid),
            process::ProcessPresence::Exited
        )
    })
}

/// Looks until a child on its way out has gone, or `deadline` passes with it still leaving.
fn settled(deadline: Instant, mut look: impl FnMut() -> io::Result<Look>) -> io::Result<Look> {
    loop {
        match look()? {
            Look::Leaving if Instant::now() < deadline => std::thread::sleep(LEAVING_LOOK),
            found => return Ok(found),
        }
    }
}

/// Two reads of one self-led child of this process. `uniqueid` names it through an `exec`, which
/// moves `identity`'s pid version, and the child is unreaped, so its pid is not another's.
fn same_leader(first: &process::Lineage, second: &process::Lineage, child: u32) -> bool {
    first.parent == std::process::id()
        && first.pgid == child
        && first.uniqueid == second.uniqueid
        && first.parent == second.parent
        && first.pgid == second.pgid
}

#[path = "cohort_launch.rs"]
pub mod launch;

#[path = "cohort_terminal_environment.rs"]
mod terminal_environment;
#[doc(hidden)]
pub use terminal_environment::capacity_terminal_environment;

/// What the kernel proves about an admitted process group's lifetime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lifetime {
    /// At least one process can be addressed in the group; retain the full reservation.
    Present,
    /// This boot has ended, or the same namespace's kernel reports no such process group.
    Drained,
    /// The scope or group cannot be read conclusively; retain the full reservation.
    Unknown,
}

impl Lifetime {
    /// Whether retaining the complete admitted budget is still necessary.
    #[must_use]
    pub const fn retains(self) -> bool {
        !matches!(self, Self::Drained)
    }
}

/// OS machine identity, boot and pid namespace in which a process group was captured.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Scope {
    host: String,
    boot: String,
    namespace: Option<String>,
}

impl Scope {
    pub(crate) fn of_key(key: &aterm_uds::spawnfd::BirthKey) -> Self {
        Self {
            host: key.host().to_owned(),
            boot: key.boot().to_owned(),
            namespace: key.namespace().map(str::to_owned),
        }
    }

    /// A changed boot is meaningful only on the same OS host; scope uncertainty retains.
    pub(crate) fn ended_or_unknown(&self) -> Option<Lifetime> {
        let Some(now) = Self::read() else {
            return Some(Lifetime::Unknown);
        };
        if now.host != self.host {
            return Some(Lifetime::Unknown);
        }
        if now.boot != self.boot {
            return Some(Lifetime::Drained);
        }
        if now.namespace != self.namespace {
            return Some(Lifetime::Unknown);
        }
        None
    }

    pub(crate) fn versioned_pid(&self) -> bool {
        self.host.starts_with("darwin:")
    }
    pub(crate) fn read() -> Option<Self> {
        #[cfg(any(target_os = "linux", target_vendor = "apple"))]
        {
            let host = process::host_id()?;
            let boot = process::boot_id().filter(|boot| !boot.is_empty())?;
            #[cfg(target_os = "linux")]
            let namespace = Some(process::pid_namespace()?);
            #[cfg(target_vendor = "apple")]
            let namespace = None;
            Some(Self {
                host,
                boot,
                namespace,
            })
        }
        #[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
        {
            None
        }
    }

    pub(crate) fn encode(&self) -> aterm_json::Value {
        aterm_json::json!({
            "host": self.host,
            "boot": self.boot,
            "namespace": self.namespace,
        })
    }

    pub(crate) fn decode(value: &aterm_json::Value) -> Option<Self> {
        let text = |key| {
            value
                .get(key)?
                .as_str()
                .filter(|v| !v.is_empty() && v.len() <= 128)
                .map(str::to_owned)
        };
        let namespace = match value.get("namespace")? {
            aterm_json::Value::Null => None,
            aterm_json::Value::String(text) if !text.is_empty() && text.len() <= 128 => {
                Some(text.clone())
            }
            _ => return None,
        };
        let host = text("host")?;
        let boot = text("boot")?;
        let hex_id = |value: &str| {
            value.len() == 32
                && value
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                && value.bytes().any(|b| b != b'0')
        };
        let (platform, machine) = host.split_once(':')?;
        if !hex_id(machine) || !hex_id(&boot) {
            return None;
        }
        match (platform, namespace.as_deref()) {
            ("darwin", None) => {}
            ("linux", Some(namespace)) if namespace.parse::<u64>().ok().is_some_and(|n| n > 0) => {}
            _ => return None,
        }
        Some(Self {
            host,
            boot,
            namespace,
        })
    }
}

/// An actual owned child's self-led process group in one boot and pid namespace.
///
/// A group number can later be reused. A nonempty reused group therefore retains this token's
/// reservation conservatively; a changed leader serial is never proof of drain. Members that
/// deliberately leave this group are outside its scope. A caller must separately prohibit or
/// account for such escape before treating this token as custody of an entire build tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cohort {
    scope: Scope,
    group: i32,
    leader_serial: u64,
}

/// A persisted observation of a previously captured cohort, with no owned-child authority.
/// Decoding never creates a [`Cohort`] that a caller can present as a new actual child.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedCohort(Cohort);

impl RecordedCohort {
    pub(crate) fn membership(&self, ancestry: &process::Ancestry) -> crate::FundedMembership {
        use crate::FundedMembership::{Inside, Outside, Unknown};
        match self.0.scope.ended_or_unknown() {
            Some(Lifetime::Drained) => return Outside,
            Some(_) => return Unknown,
            None => {}
        }
        // Group equality is a conservative overlap even if a number was
        // reused. Outside requires the original group's live native leader.
        if ancestry
            .nodes()
            .iter()
            .any(|node| node.pgid == self.0.group as u32)
        {
            return Inside;
        }
        let Some(leader) = process::read_lineage(self.0.group) else {
            return if self.lifetime() == Lifetime::Drained {
                Outside
            } else {
                Unknown
            };
        };
        if leader.uniqueid != self.0.leader_serial {
            return if self.lifetime() == Lifetime::Drained {
                Outside
            } else {
                Unknown
            };
        }
        let inside = ancestry
            .nodes()
            .iter()
            .any(|node| node.identity == leader.identity);
        if process::read_lineage(self.0.group) != Some(leader) {
            Unknown
        } else if inside {
            Inside
        } else if leader.pgid != self.0.group as u32 {
            // A still-live original birth that left its captured group is
            // not evidence of absence. This observation does not repair or
            // broaden that group's separate custody/settlement contract.
            Unknown
        } else {
            Outside
        }
    }
    pub(crate) fn in_scope(&self, scope: &Scope) -> bool {
        self.0.scope == *scope
    }
    /// Encode the complete, versioned machine/boot/namespace/group observation.
    #[must_use]
    pub fn encode(&self) -> aterm_json::Value {
        aterm_json::json!({"v": 1, "captured": self.0.encode()})
    }

    /// Read a complete supported observation. Missing scope or an invalid group is refused.
    /// This is an untrusted observation, never permission to launch, bind or signal a process.
    #[must_use]
    pub fn decode(value: &aterm_json::Value) -> Option<Self> {
        (value.get("v")?.as_u64()? == 1)
            .then(|| Cohort::decode(value.get("captured")?).map(Self))?
    }

    /// Read lifetime only; another machine or unreadable scope conservatively remains unknown.
    #[must_use]
    pub fn lifetime(&self) -> Lifetime {
        self.0.lifetime()
    }
}

impl Cohort {
    /// Capture the dedicated group of a controlled actual spawn, including a child that
    /// exited before the parent could read its live identity. The sealed spawn outcome proves
    /// the initial group birth; a decoded record or an arbitrary numeric pid cannot do so. A child
    /// that has exited and is not yet reapable is waited for.
    ///
    /// # Errors
    /// The attempt's actual owner/scope is stale or its original child was replaced, a still-live
    /// child cannot be captured as this parent's self-led group, an exited child did not become
    /// reapable within a second or its pid is not a group, or its reap fails (an ended child is
    /// reaped with its resource accounting where the kernel gives it, which its owner reads,
    /// [`Child::usage`]). Members leaving the group still require separate custody.
    pub fn of_spawned(spawned: &mut aterm_uds::spawnfd::Spawned) -> io::Result<Self> {
        if !spawned.key().is_current() || !spawned.is_original_child() {
            return Err(io::Error::other(
                "spawn owner, scope or original child changed before cohort capture",
            ));
        }
        let scope = Scope::of_key(spawned.key());
        let child = spawned.child_mut();
        match settled(Instant::now() + LEAVES_WITHIN, || look(child, &scope))? {
            Look::Live(cohort) => Ok(cohort),
            Look::Ended if scope.ended_or_unknown().is_none() => {
                let group = i32::try_from(child.id())
                    .ok()
                    .filter(|p| *p > 1)
                    .ok_or_else(|| io::Error::other("invalid actual child group"))?;
                Ok(Self {
                    scope,
                    group,
                    leader_serial: 0,
                })
            }
            Look::Ended | Look::Leaving | Look::Refused => Err(io::Error::other(
                "live actual child cohort is unknown or changed",
            )),
        }
    }
    /// The durable observation of this actual owned-child capture, with no transferable
    /// owned-child authority. Readers may query its lifetime but cannot bind it as a new child.
    #[must_use]
    pub fn record(&self) -> RecordedCohort {
        RecordedCohort(self.clone())
    }

    fn encode(&self) -> aterm_json::Value {
        aterm_json::json!({
            "scope": self.scope.encode(),
            "group": self.group,
            "leader_serial": self.leader_serial,
        })
    }

    fn decode(value: &aterm_json::Value) -> Option<Self> {
        Some(Self {
            scope: Scope::decode(value.get("scope")?)?,
            group: i32::try_from(value.get("group")?.as_u64()?)
                .ok()
                .filter(|group| *group > 1)?,
            leader_serial: value.get("leader_serial")?.as_u64()?,
        })
    }
    /// Capture `child` before acknowledging its work, while it is a live self-led group.
    ///
    /// The owned, unreaped child keeps its pid from reuse while the group and serial are read.
    /// The parent, group and serial are reread before returning. No process is launched or
    /// signalled here, and this alone publishes no reservation or admission acknowledgement.
    ///
    /// # Errors
    /// The child has ended, is not this process's child and its own group leader, a required
    /// boot/namespace/identity read is unavailable or changes, or the reap of an ended child
    /// fails (one that ended is reaped, with its resource accounting where the kernel gives it).
    pub fn of_child(child: &mut Child) -> io::Result<Self> {
        let refused = || {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "capacity cohort needs a live owned self-led child and a stable kernel scope",
            )
        };
        if ended(child)? {
            return Err(refused());
        }
        let group = i32::try_from(child.id())
            .ok()
            .filter(|pid| *pid > 1)
            .ok_or_else(refused)?;
        let scope = Scope::read().ok_or_else(refused)?;
        let first = process::read_lineage(group).ok_or_else(refused)?;
        let second = process::read_lineage(group).ok_or_else(refused)?;
        if !same_leader(&first, &second, child.id())
            || Scope::read().as_ref() != Some(&scope)
            || ended(child)?
        {
            return Err(refused());
        }
        Ok(Self {
            scope,
            group,
            leader_serial: first.uniqueid,
        })
    }

    /// The captured group number; never an authority to signal it after its leader is reaped.
    #[must_use]
    pub const fn group(&self) -> i32 {
        self.group
    }

    /// The captured leader's kernel serial, retained for binding and diagnostics.
    #[must_use]
    pub const fn leader_serial(&self) -> u64 {
        self.leader_serial
    }

    /// What this boot and namespace's kernel proves about the group's lifetime now.
    ///
    /// Only `ESRCH` from the group check proves drain in the same scope. `EPERM`, a dead or
    /// changed leader, and an unavailable read do not. A different readable boot proves the
    /// old processes ended only on the same OS machine identity; a foreign host or another
    /// pid namespace in the same boot proves nothing about them.
    #[must_use]
    pub fn lifetime(&self) -> Lifetime {
        self.lifetime_over(Scope::read(), kernel_group)
    }

    fn lifetime_over(
        &self,
        now: Option<Scope>,
        read_group: impl FnOnce(i32) -> Lifetime,
    ) -> Lifetime {
        let Some(now) = now else {
            return Lifetime::Unknown;
        };
        if now.host != self.scope.host {
            return Lifetime::Unknown;
        }
        if now.boot != self.scope.boot {
            return Lifetime::Drained;
        }
        if now.namespace != self.scope.namespace {
            return Lifetime::Unknown;
        }
        read_group(self.group)
    }
}

/// An atomic kernel group-existence query, with no signal delivered.
fn kernel_group(group: i32) -> Lifetime {
    match u32::try_from(group).ok().map(process::group_presence) {
        Some(process::ProcessPresence::Present) => Lifetime::Present,
        Some(process::ProcessPresence::Absent) => Lifetime::Drained,
        _ => Lifetime::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::{Cohort, Lifetime, Look, RecordedCohort, Scope, look, same_leader, settled};
    use aterm_uds::spawnfd;
    use std::time::{Duration, Instant};

    struct Running(spawnfd::Child);

    impl Drop for Running {
        fn drop(&mut self) {
            if matches!(self.0.try_wait(), Ok(None)) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }

    fn token() -> Cohort {
        Cohort {
            scope: Scope {
                host: "linux:11111111111111111111111111111111".into(),
                boot: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
                namespace: Some("1234".into()),
            },
            group: 42,
            leader_serial: 17,
        }
    }

    /// An unknown scope or foreign namespace cannot release a reservation even when a local
    /// lookup of the same number would find nothing. A proven reboot can.
    #[test]
    fn a_group_check_is_only_authoritative_in_its_captured_scope() {
        let cohort = token();
        assert_eq!(
            cohort.lifetime_over(None, |_| panic!("no scope")),
            Lifetime::Unknown
        );
        let mut foreign = cohort.scope.clone();
        foreign.host = "host-b".into();
        foreign.boot = "boot-b".into();
        assert_eq!(
            cohort.lifetime_over(Some(foreign), |_| panic!("foreign host")),
            Lifetime::Unknown
        );
        let mut other = cohort.scope.clone();
        other.namespace = Some("ns-b".into());
        assert_eq!(
            cohort.lifetime_over(Some(other), |_| panic!("foreign namespace")),
            Lifetime::Unknown
        );
        let mut reboot = cohort.scope.clone();
        reboot.boot = "boot-b".into();
        assert_eq!(
            cohort.lifetime_over(Some(reboot), |_| panic!("new boot")),
            Lifetime::Drained
        );
        for found in [Lifetime::Present, Lifetime::Unknown, Lifetime::Drained] {
            assert_eq!(
                cohort.lifetime_over(Some(cohort.scope.clone()), |group| {
                    assert_eq!(group, 42);
                    found
                }),
                found
            );
            assert_eq!(found.retains(), found != Lifetime::Drained);
        }
    }

    #[test]
    fn durable_tokens_keep_scope_and_reject_incomplete_or_special_group_selectors() {
        let token = token();
        assert_eq!(Cohort::decode(&token.encode()), Some(token.clone()));
        for invalid in [0u64, 1, u64::MAX] {
            let mut value = token.encode();
            value
                .as_object_mut()
                .unwrap()
                .insert("group".into(), aterm_json::Value::from(invalid));
            assert!(Cohort::decode(&value).is_none());
        }
        for field in ["host", "boot", "namespace"] {
            let mut value = token.encode();
            value
                .as_object_mut()
                .unwrap()
                .get_mut("scope")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert!(Cohort::decode(&value).is_none());
        }
        for (field, invalid) in [
            ("host", "host-a"),
            ("boot", "torn"),
            ("namespace", "unknown"),
        ] {
            let mut value = token.encode();
            value
                .as_object_mut()
                .unwrap()
                .get_mut("scope")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert(field.into(), aterm_json::Value::from(invalid));
            assert!(Cohort::decode(&value).is_none());
        }
        let record = token.record();
        assert_eq!(
            RecordedCohort::decode(&record.encode()),
            Some(record.clone())
        );
        let mut unsupported = record.encode();
        unsupported
            .as_object_mut()
            .unwrap()
            .insert("v".into(), aterm_json::Value::from(2u64));
        assert!(RecordedCohort::decode(&unsupported).is_none());
    }

    /// The actual self-led child has to run, and only its completed kernel group drains the
    /// token. A child in the caller's group is not eligible, nor one already reaped.
    #[cfg(any(target_vendor = "apple", target_os = "linux"))]
    #[test]
    fn only_an_actual_self_led_child_can_be_captured() {
        let mut child = Running(
            spawnfd::command("/bin/cat")
                .stdin(spawnfd::Stdio::piped())
                .stdout(spawnfd::Stdio::piped())
                .process_group(0)
                .spawn()
                .expect("self-led cat"),
        );
        let input = aterm_uds::deadline::cat_echoes(&mut child.0);
        let cohort = Cohort::of_child(&mut child.0).expect("actual child cohort");
        let recorded =
            RecordedCohort::decode(&cohort.record().encode()).expect("complete actual record");
        assert_eq!(cohort.group(), i32::try_from(child.0.id()).unwrap());
        assert_eq!(cohort.lifetime(), Lifetime::Present);
        assert_eq!(recorded.lifetime(), Lifetime::Present);
        drop(input);
        child.0.wait().expect("cat ended");
        assert_eq!(cohort.lifetime(), Lifetime::Drained);
        assert_eq!(recorded.lifetime(), Lifetime::Drained);
        assert!(
            Cohort::of_child(&mut child.0).is_err(),
            "an ended child is not a new cohort"
        );

        let mut inherited = Running(
            spawnfd::command("/bin/cat")
                .stdin(spawnfd::Stdio::piped())
                .stdout(spawnfd::Stdio::piped())
                .spawn()
                .expect("cat in caller's group"),
        );
        let input = aterm_uds::deadline::cat_echoes(&mut inherited.0);
        assert!(
            Cohort::of_child(&mut inherited.0).is_err(),
            "a shared group cannot be admitted as this child"
        );
        drop(input);
        inherited.0.wait().expect("inherited cat ended");
    }

    /// A child that can no longer be read is looked at again until it can be reaped.
    #[test]
    fn a_child_on_its_way_out_is_looked_at_until_it_ends() {
        let mut looks = vec![Look::Ended, Look::Leaving, Look::Leaving];
        let deadline = Instant::now() + Duration::from_secs(60);
        let found = settled(deadline, || Ok(looks.pop().expect("a look too many"))).unwrap();
        assert!(matches!(found, Look::Ended));
        assert!(looks.is_empty());
    }

    /// A refused child has settled: its refusal does not wait on the kernel.
    #[test]
    fn a_refused_child_is_looked_at_once() {
        let mut looks = 0;
        let deadline = Instant::now() + Duration::from_secs(60);
        let found = settled(deadline, || {
            looks += 1;
            Ok(Look::Refused)
        });
        assert!(matches!(found.unwrap(), Look::Refused));
        assert_eq!(looks, 1);
    }

    /// A CHILD THAT ENDED BEFORE ITS CAPTURE IS REAPED WITH ITS ACCOUNTING: the capture's reap is
    /// the child's last, so its owner, a supervised build's front, reads the root's own resident
    /// peak from it ([`spawnfd::Child::usage`]). NEGATIVE CONTROL (by hand: the capture reaping
    /// with a plain `try_wait`): no accounting kept.
    #[cfg(all(
        target_pointer_width = "64",
        any(target_vendor = "apple", target_os = "linux")
    ))]
    #[test]
    fn a_child_that_ended_before_its_capture_keeps_its_accounting() {
        let mut child = spawnfd::command("/bin/sh")
            .args(["-c", "exit 3"])
            .process_group(0)
            .spawn()
            .expect("self-led sh");
        child.wait_exited().expect("its exit, unreaped");
        let found = look(&mut child, &token().scope).unwrap();
        assert!(matches!(found, Look::Ended));
        let usage = child
            .usage()
            .expect("the capture's reap kept its accounting");
        assert_eq!(usage.status.code(), Some(3));
        assert_eq!(child.wait().expect("its status").code(), Some(3));
    }

    /// A live child refused is `Refused`, not one to wait for: it is captured in another scope.
    #[cfg(any(target_vendor = "apple", target_os = "linux"))]
    #[test]
    fn a_live_child_that_will_not_do_is_refused_not_waited_for() {
        let mut child = Running(
            spawnfd::command("/bin/cat")
                .stdin(spawnfd::Stdio::piped())
                .stdout(spawnfd::Stdio::piped())
                .process_group(0)
                .spawn()
                .expect("self-led cat"),
        );
        let input = aterm_uds::deadline::cat_echoes(&mut child.0);
        let found = look(&mut child.0, &token().scope).unwrap();
        assert!(matches!(found, Look::Refused));
        drop(input);
        child.0.wait().expect("cat ended");
    }

    /// The wait has a bound: past it, a child still leaving is the answer.
    #[test]
    fn a_child_still_leaving_at_the_deadline_ends_the_wait() {
        let (done, ended) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut looks = 0;
            let found = settled(Instant::now(), || {
                looks += 1;
                Ok(Look::Leaving)
            });
            let _ = done.send((found.map(|look| matches!(look, Look::Leaving)), looks));
        });
        let (leaving, looks) = ended
            .recv_timeout(Duration::from_secs(60))
            .expect("the deadline ends the wait");
        assert!(leaving.unwrap());
        assert_eq!(looks, 1);
    }

    /// A child that `exec`s between two reads is still the leader they read, though the kernel
    /// moved its pid version: `cat` echoing proves the `exec` came between them.
    #[cfg(any(target_vendor = "apple", target_os = "linux"))]
    #[test]
    fn two_reads_either_side_of_an_exec_are_the_same_leader() {
        use std::io::Write as _;
        let mut child = Running(
            spawnfd::command("/bin/sh")
                .args(["-c", "read go; exec /bin/cat"])
                .stdin(spawnfd::Stdio::piped())
                .stdout(spawnfd::Stdio::piped())
                .process_group(0)
                .spawn()
                .expect("self-led sh"),
        );
        let pid = i32::try_from(child.0.id()).unwrap();
        let before = aterm_uds::process::read_lineage(pid).expect("sh reads");
        let mut input = child.0.stdin.take().expect("sh's input");
        input.write_all(b"go\nran\n").unwrap();
        aterm_uds::deadline::prints(&mut child.0, "ran", aterm_uds::deadline::ECHO_HANG);
        let after = aterm_uds::process::read_lineage(pid).expect("cat reads");
        if aterm_uds::process::KEEPS_PID_VERSIONS {
            assert_ne!(
                before.identity, after.identity,
                "the exec moved its version"
            );
        }
        assert!(same_leader(&before, &after, child.0.id()));
        let mut stranger = after;
        stranger.uniqueid += 1;
        assert!(!same_leader(&before, &stranger, child.0.id()));
        drop(input);
        child.0.wait().expect("cat ended");
    }
}
