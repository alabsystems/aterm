// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typed native mutation plans. An operation never grants general terminal input.

use super::contract::Scope;
use std::collections::BTreeMap;

/// The additive native-operation protocol, separate from a supervisor claim's contract.
pub const CONTRACT: u32 = 1;
/// Maximum bytes of one native-operation request.
pub const MAX_REQUEST: usize = 64 * 1024;
/// Maximum bytes of a path retained by a native plan.
pub const MAX_PATH: usize = 4096;
/// Maximum bytes of the exact private host continuation retained by a plan.
pub const MAX_RESUME: usize = 16 * 1024;
/// Maximum arguments or environment entries in one retained native command.
pub const MAX_COMMAND_ITEMS: usize = 256;

/// A constructor's selected command, before shell quoting. Both the legacy line and the
/// one-shot launcher use this exact selection, including line-length/shim/tap fallbacks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestartCommand {
    /// Exact expected executable identity.
    pub image: Image,
    /// The selected spelling used for argv[0] and the legacy line, before canonical identity.
    pub program: String,
    /// Arguments after argv[0], never parsed from shell syntax.
    pub argv: Vec<String>,
    /// The exact selected exports, in their original order.
    pub env: Vec<(String, String)>,
    /// Inherited variables explicitly removed by the selected command.
    pub unset: Option<Vec<String>>,
    /// The selected shell-local directory change.
    pub cwd: Option<String>,
    /// The existing managed shell hook sourced before the command.
    pub heal: Option<String>,
    /// The original shell's quoting/command dialect.
    pub dialect: super::upgrade::Dialect,
}

impl RestartCommand {
    /// Render the exact selected legacy representation, without another store-env fallback.
    #[must_use]
    pub fn legacy_line(&self) -> Option<String> {
        self.line_at(&self.program, &self.argv)
    }

    fn line_at(&self, path: &str, argv: &[String]) -> Option<String> {
        let env = self
            .env
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect::<Vec<_>>();
        let unset = self
            .unset
            .as_ref()
            .map(|names| names.iter().map(String::as_str).collect::<Vec<_>>());
        super::upgrade::relaunch_line_fixed(
            self.dialect,
            self.heal.as_deref().map(std::path::Path::new),
            self.cwd.as_deref(),
            &env,
            unset.as_deref(),
            std::path::Path::new(path),
            argv,
        )
        .ok()
    }

    /// The fixed physical helper command. Its argv carries only the original operation,
    /// phase and endpoint; the helper obtains its captured vendor command from that host.
    #[must_use]
    pub fn launcher_line(
        &self,
        launcher: &Image,
        socket: &str,
        sid: &str,
        binding: &Binding,
        phase: ImagePhase,
    ) -> Option<String> {
        let argv = vec![
            "harness".into(),
            "native-launch".into(),
            "contract=1".into(),
            format!(
                "socket={}",
                aterm_codec::hex::encode_lower(socket.as_bytes())
            ),
            format!("session={sid}"),
            format!("id={}", binding.id),
            format!("cap={}", binding.capability),
            format!("phase={}", phase.word()),
        ];
        self.line_at(std::str::from_utf8(&launcher.path).ok()?, &argv)
    }

    /// Canonical bounded command storage. It contains data, never a replacement line supplied
    /// by a later Resume/Launch request.
    #[must_use]
    pub fn encode(&self) -> Option<String> {
        use std::fmt::Write;
        let bytes = self
            .program
            .len()
            .saturating_add(self.argv.iter().map(String::len).sum::<usize>())
            .saturating_add(
                self.env
                    .iter()
                    .map(|(name, value)| name.len().saturating_add(value.len()))
                    .sum::<usize>(),
            )
            .saturating_add(
                self.unset
                    .as_ref()
                    .map_or(0, |names| names.iter().map(String::len).sum::<usize>()),
            )
            .saturating_add(self.cwd.as_ref().map_or(0, String::len))
            .saturating_add(self.heal.as_ref().map_or(0, String::len));
        if bytes > MAX_RESUME {
            return None;
        }
        let line = self.legacy_line()?;
        if self.argv.len() > MAX_COMMAND_ITEMS
            || self.env.len() > MAX_COMMAND_ITEMS
            || self
                .unset
                .as_ref()
                .is_some_and(|names| names.len() > MAX_COMMAND_ITEMS)
            || line.len() > MAX_RESUME
            || line.contains(['\r', '\n', '\0'])
            || self.image.path.first() != Some(&b'/')
            || self.image.path.len() > MAX_PATH
            || self.image.path.contains(&0)
            || !std::path::Path::new(&self.program).is_absolute()
        {
            return None;
        }
        let mut out = String::new();
        encode_image(&mut out, "exec", &self.image);
        let json = |value: &str| aterm_codec::hex::encode_lower(value.as_bytes());
        let optional = |value: Option<&str>| value.map_or_else(|| "-".into(), json);
        write!(
            out,
            " program={} argv={} env={} unset={} cwd={} heal={} dialect={}",
            json(&self.program),
            json(&aterm_json::to_string(&self.argv).ok()?),
            json(&aterm_json::to_string(&self.env).ok()?),
            json(&aterm_json::to_string(&self.unset).ok()?),
            optional(self.cwd.as_deref()),
            optional(self.heal.as_deref()),
            self.dialect.hook_ext(),
        )
        .ok()?;
        (out.len() <= MAX_REQUEST).then(|| out.trim_start().to_string())
    }

    /// Decode only a canonical captured command; duplicate/unknown fields and alternate JSON
    /// representations refuse instead of changing the retained command's identity.
    #[must_use]
    pub fn parse(encoded: &str) -> Option<Self> {
        if encoded.len() > MAX_REQUEST || encoded.contains(['\n', '\r', '\0']) {
            return None;
        }
        let mut words = BTreeMap::new();
        for word in encoded.split_ascii_whitespace() {
            let (key, value) = word.split_once('=')?;
            if words.insert(key, value).is_some() {
                return None;
            }
        }
        let image = read_image(&mut words, "exec")?;
        let program = text_blob(take(&mut words, "program")?, MAX_PATH)?;
        let argv =
            aterm_json::from_str(&text_blob(take(&mut words, "argv")?, MAX_REQUEST)?).ok()?;
        let env = aterm_json::from_str(&text_blob(take(&mut words, "env")?, MAX_REQUEST)?).ok()?;
        let unset =
            aterm_json::from_str(&text_blob(take(&mut words, "unset")?, MAX_REQUEST)?).ok()?;
        let optional = |value| {
            if value == "-" {
                Some(None)
            } else {
                text_blob(value, MAX_PATH).map(Some)
            }
        };
        let cwd = optional(take(&mut words, "cwd")?)?;
        let heal = optional(take(&mut words, "heal")?)?;
        let dialect = super::upgrade::Dialect::from_exe_name(take(&mut words, "dialect")?)?;
        let command = Self {
            image,
            program,
            argv,
            env,
            unset,
            cwd,
            heal,
            dialect,
        };
        (words.is_empty() && command.encode().as_deref() == Some(encoded)).then_some(command)
    }
}

/// A mapped executable identity. Paths preserve Unix bytes and never become shell syntax.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// The kernel-named or immutable managed image path.
    pub path: Vec<u8>,
    /// Its filesystem device.
    pub device: u64,
    /// Its filesystem inode.
    pub inode: u64,
}

/// A process identity observed independently by the host before admission and completion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Process {
    /// A canonical pid above one.
    pub pid: u32,
    /// Kernel birth: epoch microseconds on Darwin, boot clock ticks on Linux.
    pub birth: u64,
    /// Its actual parent.
    pub parent: u32,
    /// Its process group.
    pub group: u32,
    /// Its kernel session.
    pub session: u32,
    /// Its mapped image.
    pub image: Image,
}

impl Image {
    /// Capture an executable file identity without making it shell syntax.
    #[must_use]
    pub fn at(path: &std::path::Path) -> Option<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            use std::os::unix::fs::MetadataExt;
            let path = std::fs::canonicalize(path).ok()?;
            let metadata = std::fs::metadata(&path).ok()?;
            (metadata.is_file() && metadata.mode() & 0o111 != 0).then(|| Self {
                path: path.as_os_str().as_bytes().to_vec(),
                device: metadata.dev(),
                inode: metadata.ino(),
            })
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            None
        }
    }
}

impl Process {
    /// Birth-bracket kernel process/group/session and actual mapped image facts. An unread
    /// process or a changed mapping refuses; a pathname's replacement never proves the old image.
    #[must_use]
    pub fn observe(pid: u32) -> Option<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            use std::os::unix::fs::MetadataExt;
            let signed = i32::try_from(pid).ok().filter(|p| *p > 1)?;
            let birth = aterm_uds::process::read_birth(signed)?;
            let parent = aterm_uds::process::read_process_parent(signed)?;
            let path = aterm_uds::process::read_process_path(signed)?;
            #[cfg(target_os = "linux")]
            let metadata = std::fs::metadata(format!("/proc/{pid}/exe")).ok()?;
            #[cfg(not(target_os = "linux"))]
            let metadata = std::fs::metadata(&path).ok()?;
            #[cfg(target_os = "macos")]
            if aterm_sysprobe::kproc::image_inode(pid)? != metadata.ino() {
                return None;
            }
            // SAFETY: positive pid, no pointers; syscall errors refuse rather than fabricate facts.
            let group = u32::try_from(unsafe { libc::getpgid(signed) }).ok()?;
            // SAFETY: same positive pid, independently birth-bracketed; errors refuse.
            let session = u32::try_from(unsafe { libc::getsid(signed) }).ok()?;
            if aterm_uds::process::read_birth(signed)? != birth {
                return None;
            }
            Some(Self {
                pid,
                birth: birth.0,
                parent,
                group,
                session,
                image: Image {
                    path: path.as_os_str().as_bytes().to_vec(),
                    device: metadata.dev(),
                    inode: metadata.ino(),
                },
            })
        }
        #[cfg(not(unix))]
        {
            let _ = pid;
            None
        }
    }
}

/// Mint a fresh id before preparing one move. Retries retain it; they never call this again.
/// # Errors
/// The operating system entropy source is unavailable.
pub fn operation_id() -> std::io::Result<String> {
    let mut bytes = [0; 16];
    aterm_uds::rand::fill(&mut bytes)?;
    Ok(aterm_codec::hex::encode_lower(&bytes))
}

/// Actual constructor kinds, discovered separately from protocol grammar and roster rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Constructor {
    /// An external supervisor's own-image constructor.
    OwnImage,
    /// The private built-in's retained signal/launcher/rollback constructor.
    PrivateRestart,
}

impl Constructor {
    /// The canonical discovery order. A capability is not an action ticket.
    pub const ALL: [Self; 2] = [Self::OwnImage, Self::PrivateRestart];

    /// This constructor's strict wire word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::OwnImage => "own-image",
            Self::PrivateRestart => "private-restart",
        }
    }
}

/// The complete permitted mutation, retained by the host before its first side effect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativePlan {
    /// Replace the authenticated supervisor's own image, preserving its child and PTY.
    OwnImage {
        /// The same live vendor child before and after the move.
        child: Process,
        /// The exact immutable supervisor image the real exec constructor will enter.
        target: Image,
        /// The compiled version independently probed by the actual constructor.
        version: String,
    },
    /// A private built-in constructor's fixed signal and subsequent resume submission.
    /// External harnesses cannot submit this plan or obtain its terminal continuation.
    Restart {
        /// The exact process to retire, including its birth and original image.
        process: Process,
        /// A passive retained-death proof for crash entry, never an old mutation ticket.
        observation: Option<super::native_observe::ObservationBinding>,
        /// The foreground shell in which the fixed continuation may run.
        shell: Process,
        /// The exact aterm image that registers and execs the retained vendor command.
        launcher: Image,
        /// The same local host endpoint on which this operation was prepared.
        socket: String,
        /// The exact selected forward command, captured before the first signal.
        forward: Box<RestartCommand>,
        /// At most one old-image rollback, also captured before the first signal.
        rollback: Option<Box<RestartCommand>>,
    },
}

impl NativePlan {
    /// The actual constructor required before this plan can receive new admission.
    #[must_use]
    pub const fn constructor(&self) -> Constructor {
        match self {
            Self::OwnImage { .. } => Constructor::OwnImage,
            Self::Restart { .. } => Constructor::PrivateRestart,
        }
    }
}

/// A preparation's authority expectation. The server independently derives principal,
/// connection and kernel peer, and checks its immutable claim under the metadata lock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prepare {
    /// Caller-generated idempotency key, exactly 32 lowercase hexadecimal digits.
    pub id: String,
    /// Fresh host-process epoch obtained from `native-act info`. An app crash/replacement
    /// cannot silently turn recovery of an old operation into a new admission.
    pub host_epoch: String,
    /// The actual holder; a private built-in always uses its own holder.
    pub holder: String,
    /// The exact claim revision under which the constructor begins.
    pub revision: u64,
    /// Private Restart's sequence from the same fresh status whose person policy passed.
    /// OwnImage carries none and acquires no quiet/person gate.
    pub expected_human_seq: Option<u64>,
    /// One absolute CLOCK_MONOTONIC deadline in microseconds, fixed before prepare.
    /// Polling, exec, recovery and rollback never extend it.
    pub deadline_us: u64,
    /// Explicit native semantic duty, Upgrade or Relaunch only.
    pub duty: Scope,
    /// The complete planned mutation.
    pub plan: NativePlan,
}

/// The exact original preparation and the supervisor image captured BEFORE sending it.
/// The original image is essential after bounded operation history has been reclaimed:
/// a process already in the target image cannot turn table absence into rollback proof.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CancelPrepare {
    /// The complete original request, with its unchanged id/epoch/plan/deadline.
    pub prepare: Prepare,
    /// The original kernel actor; its outer parent is diagnostic across reparenting.
    pub actor: Process,
}

/// Opaque host-issued capability identifying this operation, not a supervisor claim.
#[derive(Clone, PartialEq, Eq)]
pub struct Binding {
    /// The original idempotency key.
    pub id: String,
    /// Host-issued unpredictable capability, exactly 64 lowercase hexadecimal digits.
    pub capability: String,
}

impl std::fmt::Debug for Binding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Binding")
            .field("id", &self.id)
            .field("capability", &"redacted")
            .finish()
    }
}

/// Reproof does not commit a claim rebind or release native custody.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImagePhase {
    /// The actual constructor entered its proposed new image.
    Forward,
    /// The actual constructor returned to the original image.
    Rollback,
}

/// A final request's result. Dispatching commit makes transport ambiguity irreversible:
/// the constructor must query this same operation rather than assume rollback is allowed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// The host observed the planned outcome and completed the native operation. Its separate
    /// claim receipt says whether authority survived; this never revives an invalidated claim.
    Committed,
    /// The host proved no commit occurred, retains the operation and explicitly permits rollback.
    ProvenNotCommittedRollbackSafe,
    /// Outcome is unresolved. Retain all possibly authoritative descriptors; begin no new move.
    Unknown,
    /// The failed plan is terminal: no future vendor effect can be admitted under its cap.
    /// Ungranted helpers may still exit later; this is never a successful image completion.
    Retired(Retirement),
}

/// Why a failed native plan can release custody after its exact input prefix drains.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retirement {
    /// No child exec was granted; terminal closure rejects every delayed Attach/Execute.
    NotGranted,
    /// Every granted child (or OwnImage actor/child) has a retained, retired kernel cohort.
    /// This names those exact admitted processes/groups, never arbitrary detached descendants.
    Exited,
}

impl Retirement {
    /// The exact terminal receipt word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::NotGranted => "no-exec-grant",
            Self::Exited => "cohort-exited",
        }
    }
}

/// The authority observation atomically recorded with native completion, never a new grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// The original scoped claim survived and its image version was rebound at this revision.
    Bound(u64),
    /// Release, expiry or replacement invalidated the claim. A later claim must follow the
    /// normal current-authority rules, after this native operation has completed.
    Lost,
    /// A private built-in continuation completed; it rebinds no external claim.
    Builtin,
}

/// The bounded wire requests the actual constructors may issue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// Read the host-process epoch and its native protocol without claiming or moving anything.
    Info,
    /// Discover enabled actual constructor kinds before preparing or claiming anything.
    Constructors,
    /// Discover atomic initial-prepare cancellation before creating a scoped claim.
    /// This probe leaves the frozen native protocol's Info reply unchanged.
    CancelSupport,
    /// Admit a complete plan, atomically with its actor and revision.
    Prepare(Box<Prepare>),
    /// Seal this exact original OwnImage preparation against future admission. A fresh
    /// connection supplies new kernel proof; this request grants no action or claim.
    CancelPrepare(Box<CancelPrepare>),
    /// Reprove the new image while explicitly retaining rollback custody.
    Precommit(Binding, ImagePhase),
    /// The real constructor has finished all fallible local adoption and sealed mutation.
    Commit(Binding, ImagePhase, String),
    /// Recover the same idempotent operation after any ambiguous acknowledgement.
    Query(Binding),
    /// Seal an admitted operation before its first mutation, explicitly abandoning its plan.
    Abort(Binding),
    /// Reconstruct only this native-operation channel after its previous channel closed.
    /// This never binds a supervisor claim or admits a new operation.
    Recover(Binding),
    /// Execute only the fixed terminating signal of a private Restart plan.
    Signal(Binding),
    /// Submit only the fixed resume bytes retained in a private Restart plan.
    Resume(Binding, ImagePhase, String, u64),
    /// Register the exact one-shot aterm child and retained kernel exit witness before exec.
    Attach(Binding, ImagePhase),
    /// Recheck current guards and the original deadline immediately before the fixed exec.
    Execute(Binding, ImagePhase),
    /// Record only the bound one-shot child's actual exec errno, never arbitrary outcome text.
    ExecFailed(Binding, ImagePhase, i32),
    /// Observe kernel extinction without granting a continuation or re-creating an operation.
    Reconcile(Binding),
}

fn canonical_number<T: std::str::FromStr + ToString>(text: &str) -> Option<T> {
    let value: T = text.parse().ok()?;
    (value.to_string() == text).then_some(value)
}

fn blob(text: &str, max: usize) -> Option<Vec<u8>> {
    if text.len() > max.checked_mul(2)? || !aterm_codec::hex::is_lower(text, text.len()) {
        return None;
    }
    aterm_codec::hex::decode_vec(text.as_bytes())
}

fn text_blob(text: &str, max: usize) -> Option<String> {
    String::from_utf8(blob(text, max)?).ok()
}

fn take<'a>(words: &mut BTreeMap<&str, &'a str>, key: &str) -> Option<&'a str> {
    words.remove(key)
}

fn read_image(words: &mut BTreeMap<&str, &str>, prefix: &str) -> Option<Image> {
    let path = blob(take(words, &format!("{prefix}-path"))?, MAX_PATH)?;
    if path.first() != Some(&b'/') || path.contains(&0) {
        return None;
    }
    let device = canonical_number(take(words, &format!("{prefix}-dev"))?)?;
    let inode = canonical_number(take(words, &format!("{prefix}-ino"))?)?;
    Some(Image {
        path,
        device,
        inode,
    })
}

fn read_process(words: &mut BTreeMap<&str, &str>, prefix: &str) -> Option<Process> {
    let pid = canonical_number::<u32>(take(words, &format!("{prefix}-pid"))?)?;
    let birth = canonical_number(take(words, &format!("{prefix}-birth"))?)?;
    let parent = canonical_number(take(words, &format!("{prefix}-parent"))?)?;
    let group = canonical_number(take(words, &format!("{prefix}-group"))?)?;
    let session = canonical_number(take(words, &format!("{prefix}-session"))?)?;
    (pid > 1).then_some(Process {
        pid,
        birth,
        parent,
        group,
        session,
        image: read_image(words, prefix)?,
    })
}

fn read_binding(words: &mut BTreeMap<&str, &str>) -> Option<Binding> {
    let id = take(words, "id")?;
    let capability = take(words, "cap")?;
    (aterm_codec::hex::is_lower(id, 32) && aterm_codec::hex::is_lower(capability, 64)).then(|| {
        Binding {
            id: id.to_string(),
            capability: capability.to_string(),
        }
    })
}

fn read_phase(words: &mut BTreeMap<&str, &str>) -> Option<ImagePhase> {
    match take(words, "phase")? {
        "forward" => Some(ImagePhase::Forward),
        "rollback" => Some(ImagePhase::Rollback),
        _ => None,
    }
}

impl Request {
    /// Own a complete data-only preparation without putting its retained recipes on every
    /// request's stack. This constructs words; only the host can admit the operation.
    #[must_use]
    pub fn prepare(prepare: Prepare) -> Self {
        Self::Prepare(Box::new(prepare))
    }

    /// Own the retained original words and old kernel actor, without sending or granting
    /// anything. Only a matching positive host receipt proves cancellation.
    #[must_use]
    pub fn cancel_prepare(prepare: Prepare, actor: Process) -> Self {
        Self::CancelPrepare(Box::new(CancelPrepare { prepare, actor }))
    }
    /// Decode one verb's tail. Unknown/duplicate fields, noncanonical numbers, invalid paths
    /// or capabilities, and non-native semantic duties refuse the complete request.
    #[must_use]
    pub fn parse(tail: &str) -> Option<Self> {
        if tail.len() > MAX_REQUEST || tail.contains(['\n', '\r', '\0']) {
            return None;
        }
        let mut split = tail.split_ascii_whitespace();
        let verb = split.next()?;
        let mut words = BTreeMap::new();
        for word in split {
            let (key, value) = word.split_once('=')?;
            if value.is_empty() || words.insert(key, value).is_some() {
                return None;
            }
        }
        let request = match verb {
            "info" => Self::Info,
            "constructors" => Self::Constructors,
            "cancel-support" => Self::CancelSupport,
            "prepare" | "cancel-prepare" => {
                let id = take(&mut words, "id")?.to_string();
                if !aterm_codec::hex::is_lower(&id, 32) {
                    return None;
                }
                let host_epoch = take(&mut words, "epoch")?.to_string();
                if !aterm_codec::hex::is_lower(&host_epoch, 32) {
                    return None;
                }
                let holder = text_blob(take(&mut words, "holder")?, 1024)?;
                let revision = canonical_number(take(&mut words, "revision")?)?;
                let expected_human_seq = match words.remove("human-seq") {
                    None => None,
                    Some(value) => Some(canonical_number(value)?),
                };
                let deadline_us = canonical_number(take(&mut words, "deadline-us")?)?;
                let duty = Scope::from_word(take(&mut words, "duty")?)?;
                if !matches!(duty, Scope::Upgrade | Scope::Relaunch) {
                    return None;
                }
                let plan = match take(&mut words, "plan")? {
                    "own-image" if duty == Scope::Upgrade => NativePlan::OwnImage {
                        child: read_process(&mut words, "child")?,
                        target: read_image(&mut words, "target")?,
                        version: text_blob(take(&mut words, "version")?, 256)?,
                    },
                    "restart" => NativePlan::Restart {
                        process: read_process(&mut words, "process")?,
                        observation: match take(&mut words, "observation")? {
                            "-" => None,
                            encoded => {
                                let super::native_observe::Request::Refresh(binding) =
                                    super::native_observe::Request::parse(&text_blob(
                                        encoded,
                                        MAX_REQUEST,
                                    )?)?
                                else {
                                    return None;
                                };
                                Some(binding)
                            }
                        },
                        shell: read_process(&mut words, "shell")?,
                        launcher: read_image(&mut words, "launcher")?,
                        socket: text_blob(take(&mut words, "socket")?, MAX_PATH)?,
                        forward: Box::new(RestartCommand::parse(&text_blob(
                            take(&mut words, "forward")?,
                            MAX_REQUEST,
                        )?)?),
                        rollback: match take(&mut words, "rollback")? {
                            "-" => None,
                            encoded => Some(Box::new(RestartCommand::parse(&text_blob(
                                encoded,
                                MAX_REQUEST,
                            )?)?)),
                        },
                    },
                    _ => return None,
                };
                if holder.is_empty() {
                    return None;
                }
                if matches!(plan, NativePlan::Restart { .. }) != expected_human_seq.is_some() {
                    return None;
                }
                if let NativePlan::Restart { socket, .. } = &plan
                    && (!std::path::Path::new(socket).is_absolute()
                        || socket.contains(['\r', '\n', '\0']))
                {
                    return None;
                }
                let prepare = Prepare {
                    id,
                    host_epoch,
                    holder,
                    revision,
                    expected_human_seq,
                    deadline_us,
                    duty,
                    plan,
                };
                if verb == "cancel-prepare" {
                    if !matches!(prepare.plan, NativePlan::OwnImage { .. }) {
                        return None;
                    }
                    let actor = read_process(&mut words, "actor")?;
                    Self::cancel_prepare(prepare, actor)
                } else {
                    Self::prepare(prepare)
                }
            }
            "precommit" => Self::Precommit(read_binding(&mut words)?, read_phase(&mut words)?),
            "commit" => Self::Commit(
                read_binding(&mut words)?,
                read_phase(&mut words)?,
                text_blob(take(&mut words, "version")?, 256)?,
            ),
            "query" => Self::Query(read_binding(&mut words)?),
            "abort" => Self::Abort(read_binding(&mut words)?),
            "recover" => Self::Recover(read_binding(&mut words)?),
            "signal" => Self::Signal(read_binding(&mut words)?),
            "attach" => Self::Attach(read_binding(&mut words)?, read_phase(&mut words)?),
            "execute" => Self::Execute(read_binding(&mut words)?, read_phase(&mut words)?),
            "exec-failed" => {
                let binding = read_binding(&mut words)?;
                let phase = read_phase(&mut words)?;
                let errno = canonical_number::<i32>(take(&mut words, "errno")?)?;
                if errno <= 0 {
                    return None;
                }
                Self::ExecFailed(binding, phase, errno)
            }
            "reconcile" => Self::Reconcile(read_binding(&mut words)?),
            "resume" => {
                let binding = read_binding(&mut words)?;
                let phase = read_phase(&mut words)?;
                let generation = take(&mut words, "generation")?;
                let human_seq = canonical_number(take(&mut words, "human-seq")?)?;
                aterm_types::control_verbs::ScreenGen::parse(generation)?;
                Self::Resume(binding, phase, generation.to_string(), human_seq)
            }
            _ => return None,
        };
        words.is_empty().then_some(request)
    }

    /// Encode the complete request canonically. The decoder validates constructed values too;
    /// an invalid request is never partially sent to a control stream.
    #[must_use]
    pub fn encode(&self) -> Option<String> {
        use std::fmt::Write;
        let mut out = String::new();
        match self {
            Self::Info => out.push_str("info"),
            Self::Constructors => out.push_str("constructors"),
            Self::CancelSupport => out.push_str("cancel-support"),
            Self::Prepare(_) | Self::CancelPrepare(_) => {
                let p = match self {
                    Self::Prepare(prepare) => &**prepare,
                    Self::CancelPrepare(cancel) => &cancel.prepare,
                    _ => unreachable!(),
                };
                let verb = if matches!(self, Self::CancelPrepare(_)) {
                    "cancel-prepare"
                } else {
                    "prepare"
                };
                write!(
                    out,
                    "{verb} id={} epoch={} holder={} revision={} deadline-us={} duty={}",
                    p.id,
                    p.host_epoch,
                    aterm_codec::hex::encode_lower(p.holder.as_bytes()),
                    p.revision,
                    p.deadline_us,
                    p.duty.word()
                )
                .ok()?;
                if let Some(seq) = p.expected_human_seq {
                    write!(out, " human-seq={seq}").ok()?;
                }
                match &p.plan {
                    NativePlan::OwnImage {
                        child,
                        target,
                        version,
                    } => {
                        out.push_str(" plan=own-image");
                        encode_process(&mut out, "child", child);
                        encode_image(&mut out, "target", target);
                        write!(
                            out,
                            " version={}",
                            aterm_codec::hex::encode_lower(version.as_bytes())
                        )
                        .ok()?;
                    }
                    NativePlan::Restart {
                        process,
                        observation,
                        shell,
                        launcher,
                        socket,
                        forward,
                        rollback,
                    } => {
                        out.push_str(" plan=restart");
                        encode_process(&mut out, "process", process);
                        encode_process(&mut out, "shell", shell);
                        encode_image(&mut out, "launcher", launcher);
                        write!(
                            out,
                            " observation={} socket={} forward={} rollback={}",
                            match observation {
                                None => "-".into(),
                                Some(binding) => aterm_codec::hex::encode_lower(
                                    super::native_observe::Request::Refresh(binding.clone())
                                        .encode()?
                                        .as_bytes()
                                ),
                            },
                            aterm_codec::hex::encode_lower(socket.as_bytes()),
                            aterm_codec::hex::encode_lower(forward.encode()?.as_bytes()),
                            match rollback {
                                None => "-".into(),
                                Some(command) =>
                                    aterm_codec::hex::encode_lower(command.encode()?.as_bytes()),
                            },
                        )
                        .ok()?;
                    }
                }
                if let Self::CancelPrepare(cancel) = self {
                    encode_process(&mut out, "actor", &cancel.actor);
                }
            }
            Self::Precommit(b, phase) => {
                write!(out, "precommit {} phase={}", b.words(), phase.word()).ok()?
            }
            Self::Commit(b, phase, version) => write!(
                out,
                "commit {} phase={} version={}",
                b.words(),
                phase.word(),
                aterm_codec::hex::encode_lower(version.as_bytes())
            )
            .ok()?,
            Self::Query(b) => write!(out, "query {}", b.words()).ok()?,
            Self::Abort(b) => write!(out, "abort {}", b.words()).ok()?,
            Self::Recover(b) => write!(out, "recover {}", b.words()).ok()?,
            Self::Signal(b) => write!(out, "signal {}", b.words()).ok()?,
            Self::Attach(b, phase) => {
                write!(out, "attach {} phase={}", b.words(), phase.word()).ok()?
            }
            Self::Execute(b, phase) => {
                write!(out, "execute {} phase={}", b.words(), phase.word()).ok()?
            }
            Self::ExecFailed(b, phase, errno) => write!(
                out,
                "exec-failed {} phase={} errno={errno}",
                b.words(),
                phase.word()
            )
            .ok()?,
            Self::Reconcile(b) => write!(out, "reconcile {}", b.words()).ok()?,
            Self::Resume(b, phase, generation, human_seq) => write!(
                out,
                "resume {} phase={} generation={generation} human-seq={human_seq}",
                b.words(),
                phase.word()
            )
            .ok()?,
        }
        (Self::parse(&out).as_ref() == Some(self)).then_some(out)
    }
}

fn encode_image(out: &mut String, prefix: &str, image: &Image) {
    use std::fmt::Write;
    let _ = write!(
        out,
        " {prefix}-path={} {prefix}-dev={} {prefix}-ino={}",
        aterm_codec::hex::encode_lower(&image.path),
        image.device,
        image.inode
    );
}

fn encode_process(out: &mut String, prefix: &str, process: &Process) {
    use std::fmt::Write;
    let _ = write!(
        out,
        " {prefix}-pid={} {prefix}-birth={} {prefix}-parent={} {prefix}-group={} {prefix}-session={}",
        process.pid, process.birth, process.parent, process.group, process.session
    );
    encode_image(out, prefix, &process.image);
}

impl Binding {
    fn words(&self) -> String {
        format!("id={} cap={}", self.id, self.capability)
    }
}

impl ImagePhase {
    /// The exact wire word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Forward => "forward",
            Self::Rollback => "rollback",
        }
    }
}

/// A native-operation reply. No reply grants terminal input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    /// Enabled actual constructors in canonical order. An empty set is explicit disabled
    /// support; this observation grants no plan, mutation, typing duty or supervisor claim.
    Constructors(Vec<Constructor>),
    /// This host can atomically seal an exact initial OwnImage preparation. Check this
    /// separate discovery reply before claiming; native-act=1 alone is insufficient.
    CancelSupport,
    /// No mutation committed and no future original Prepare can admit this exact plan.
    /// The echoed original request binds all fields, including its unchanged deadline.
    Cancelled(Box<CancelPrepare>),
    /// This host supports native-act=1, blocks app handoff through unresolved native work,
    /// and uses absolute CLOCK_MONOTONIC microseconds. It carries no action permission.
    Info {
        /// The fresh process epoch required by Prepare.
        epoch: String,
        /// Exact roster IDs accepted for OwnImage/Upgrade. Check this before any claim;
        /// an absent row must never be probed by creating an ordinary foreign claim.
        own_image: Vec<String>,
    },
    /// The same operation is still waiting for its already-started predecessors.
    Pending(Binding),
    /// The same retained operation may now begin its exact mutation.
    Admitted(Binding),
    /// Image proof succeeded; rollback custody and the original operation remain intact.
    Precommitted,
    /// Final or unresolved outcome of this same operation.
    Decision(Decision),
    /// Idempotent native completion plus the exact claim observation at that completion.
    Committed(ClaimOutcome),
    /// A private builtin completion also retained this positively observed new child for
    /// later crash admission. The frozen external OwnImage receipt is unchanged.
    RestartCommitted(Box<super::native_observe::SavedObservation>),
    /// The retained fixed signal was delivered, or its exact process had already exited.
    Signalled,
    /// The retained fixed continuation reached the kernel in full.
    Resumed(ImagePhase),
    /// The bound launcher's fixed command data, before permission for its first exec.
    Launch {
        command: RestartCommand,
        deadline_us: u64,
    },
    /// Permission for this already registered child's one fixed exec, under the old deadline.
    Execute { deadline_us: u64 },
    /// The bound child reported its fixed exec failure; kernel death still owns retirement.
    ExecFailed,
    /// The sealed attempt has no pending writes and accepted no bytes; the same plan may
    /// be tried at a fresh guarded prompt without minting another native operation.
    Unwritten,
}

impl Reply {
    /// Parse a complete reply. Unknown fields are refused rather than guessed into a grant.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let mut words = line.trim_end_matches(['\r', '\n']).split_ascii_whitespace();
        if words.next()? != "OK" || words.next()? != "native-act" {
            return None;
        }
        let state = words.next()?.strip_prefix("state=")?;
        let reply = match state {
            "constructors" => {
                let plans = words.next()?.strip_prefix("plans=")?;
                let enabled = if plans == "-" {
                    Vec::new()
                } else {
                    let enabled = Constructor::ALL
                        .into_iter()
                        .filter(|kind| plans.split(',').any(|word| word == kind.word()))
                        .collect::<Vec<_>>();
                    if enabled.is_empty()
                        || enabled
                            .iter()
                            .map(|kind| kind.word())
                            .collect::<Vec<_>>()
                            .join(",")
                            != plans
                    {
                        return None;
                    }
                    enabled
                };
                Self::Constructors(enabled)
            }
            "cancel-support" => {
                if words.next()? != "cancel-prepare=1" {
                    return None;
                }
                Self::CancelSupport
            }
            "cancelled" => {
                let encoded = words.next()?.strip_prefix("request=")?;
                let original = text_blob(encoded, MAX_REQUEST)?;
                let Request::CancelPrepare(cancel) = Request::parse(&original)? else {
                    return None;
                };
                if Request::CancelPrepare(cancel.clone()).encode()?.as_str() != original {
                    return None;
                }
                Self::Cancelled(cancel)
            }
            "info" => {
                let epoch = words.next()?.strip_prefix("epoch=")?;
                if !aterm_codec::hex::is_lower(epoch, 32)
                    || words.next()? != "native-act=1"
                    || words.next()? != "handoff=blocked"
                    || words.next()? != "clock=monotonic-us"
                {
                    return None;
                }
                let rows = words.next()?.strip_prefix("own-image=")?;
                let mut own_image = Vec::new();
                if rows != "-" {
                    for row in rows.split(',') {
                        let (id, scope) = row.split_once(':')?;
                        if scope != "upgrade"
                            || id.is_empty()
                            || id.len() > 64
                            || !id
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
                            || own_image.iter().any(|old| old == id)
                        {
                            return None;
                        }
                        own_image.push(id.to_string());
                    }
                }
                Self::Info {
                    epoch: epoch.to_string(),
                    own_image,
                }
            }
            "admitted" | "pending" => {
                let mut fields = BTreeMap::new();
                for word in words.by_ref() {
                    let (key, value) = word.split_once('=')?;
                    if fields.insert(key, value).is_some() {
                        return None;
                    }
                }
                let binding = read_binding(&mut fields)?;
                if !fields.is_empty() {
                    return None;
                }
                if state == "admitted" {
                    Self::Admitted(binding)
                } else {
                    Self::Pending(binding)
                }
            }
            "precommitted" => Self::Precommitted,
            "committed" => {
                let claim = words.next()?.strip_prefix("claim=")?;
                let revision = words.next()?.strip_prefix("revision=")?;
                let outcome = match (claim, revision) {
                    ("bound", revision) => ClaimOutcome::Bound(canonical_number(revision)?),
                    ("lost", "-") => ClaimOutcome::Lost,
                    ("builtin", "-") => ClaimOutcome::Builtin,
                    _ => return None,
                };
                match words.next() {
                    None => Self::Committed(outcome),
                    Some(word) if outcome == ClaimOutcome::Builtin => {
                        let saved = super::native_observe::SavedObservation::parse(&text_blob(
                            word.strip_prefix("observation=")?,
                            MAX_REQUEST,
                        )?)?;
                        Self::RestartCommitted(Box::new(saved))
                    }
                    Some(_) => return None,
                }
            }
            "rollback-safe" => Self::Decision(Decision::ProvenNotCommittedRollbackSafe),
            "unknown" => Self::Decision(Decision::Unknown),
            "signalled" => Self::Signalled,
            "resumed" => Self::Resumed(match words.next()?.strip_prefix("phase=")? {
                "forward" => ImagePhase::Forward,
                "rollback" => ImagePhase::Rollback,
                _ => return None,
            }),
            "launch" => {
                let deadline_us = canonical_number(words.next()?.strip_prefix("deadline-us=")?)?;
                let command = RestartCommand::parse(&text_blob(
                    words.next()?.strip_prefix("command=")?,
                    MAX_REQUEST,
                )?)?;
                Self::Launch {
                    command,
                    deadline_us,
                }
            }
            "execute" => Self::Execute {
                deadline_us: canonical_number(words.next()?.strip_prefix("deadline-us=")?)?,
            },
            "exec-failed" => Self::ExecFailed,
            "retired" => Self::Decision(Decision::Retired(
                match words.next()?.strip_prefix("reason=")? {
                    "no-exec-grant" => Retirement::NotGranted,
                    "cohort-exited" => Retirement::Exited,
                    _ => return None,
                },
            )),
            "unwritten" => Self::Unwritten,
            _ => return None,
        };
        words.next().is_none().then_some(reply)
    }
}

/// Execute one typed request on the constructor's retained authoritative control stream.
/// The caller owns the original deadline and retains the stream on transport ambiguity.
/// # Errors
/// Invalid canonical request/session, a refused or malformed reply, or transport failure.
pub fn exchange<S: std::io::Read + std::io::Write>(
    client: &mut crate::RelayClient<S>,
    sid: &str,
    request: &Request,
) -> std::io::Result<Reply> {
    if sid.is_empty()
        || !sid
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid native session",
        ));
    }
    let tail = request.encode().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid native plan")
    })?;
    let line = client.request_line(&format!("@{sid} native-act {tail}"))?;
    Reply::parse(&line)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::PermissionDenied, line))
}

/// Dispatch final commit once. Every transport/refusal ambiguity after dispatch is Unknown;
/// recovery queries the original binding and never automatically rolls the image back.
pub fn commit<S: std::io::Read + std::io::Write>(
    client: &mut crate::RelayClient<S>,
    sid: &str,
    binding: &Binding,
    phase: ImagePhase,
    version: &str,
) -> Decision {
    match exchange(
        client,
        sid,
        &Request::Commit(binding.clone(), phase, version.to_string()),
    ) {
        Ok(Reply::Committed(_) | Reply::RestartCommitted(_)) => Decision::Committed,
        Ok(Reply::Decision(decision)) => decision,
        Ok(_) | Err(_) => Decision::Unknown,
    }
}

/// The transaction clock shared across exec on one host. This is CLOCK_MONOTONIC, not wall
/// time, an Instant encoding, a process-relative timer or a claim's metadata clock.
#[must_use]
pub fn monotonic_us() -> Option<u64> {
    #[cfg(unix)]
    {
        let mut value = std::mem::MaybeUninit::<libc::timespec>::uninit();
        // SAFETY: clock_gettime initializes the writable timespec on success.
        if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, value.as_mut_ptr()) } != 0 {
            return None;
        }
        // SAFETY: the successful call initialized the complete value.
        let value = unsafe { value.assume_init() };
        u64::try_from(value.tv_sec)
            .ok()?
            .checked_mul(1_000_000)?
            .checked_add(u64::try_from(value.tv_nsec).ok()? / 1000)
    }
    #[cfg(not(unix))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Request {
        let image = Image {
            path: b"/private/image \xff".to_vec(),
            device: 7,
            inode: 19,
        };
        Request::prepare(Prepare {
            id: "12".repeat(16),
            host_epoch: "34".repeat(16),
            holder: "grok-alab@4242".into(),
            revision: 2,
            expected_human_seq: None,
            deadline_us: 91,
            duty: Scope::Upgrade,
            plan: NativePlan::OwnImage {
                child: Process {
                    pid: 100,
                    birth: 2,
                    parent: 4242,
                    group: 100,
                    session: 1,
                    image: image.clone(),
                },
                target: image,
                version: "0.1.0+g123".into(),
            },
        })
    }

    #[test]
    fn native_wire_preserves_unix_bytes_and_one_original_deadline() {
        let request = fixture();
        let wire = request.encode().expect("canonical plan");
        assert_eq!(Request::parse(&wire), Some(request));
        assert!(wire.contains("deadline-us=91"));
        assert!(wire.is_ascii());
        assert_eq!(
            Request::parse(&wire.replace("deadline-us=91", "deadline-us=091")),
            None
        );
        assert_eq!(Request::parse(&(wire.clone() + " deadline-us=92")), None);
        assert_eq!(Request::parse(&(wire + " unrecognized=1")), None);
    }

    #[test]
    fn initial_cancel_is_discovered_separately_and_echoes_the_exact_original_plan() {
        assert_eq!(
            Request::parse("cancel-support"),
            Some(Request::CancelSupport)
        );
        assert_eq!(
            Request::CancelSupport.encode().as_deref(),
            Some("cancel-support")
        );
        assert_eq!(
            Reply::parse("OK native-act state=cancel-support cancel-prepare=1\n"),
            Some(Reply::CancelSupport)
        );
        for malformed in [
            "OK native-act state=cancel-support",
            "OK native-act state=cancel-support cancel-prepare=2",
            "OK native-act state=cancel-support cancel-prepare=1 native-act=1",
        ] {
            assert_eq!(Reply::parse(malformed), None);
        }
        let Request::Prepare(original) = fixture() else {
            unreachable!()
        };
        let actor = Process {
            pid: 4242,
            birth: 17,
            parent: 1,
            group: 4242,
            session: 1,
            image: Image {
                path: b"/original-supervisor".to_vec(),
                device: 7,
                inode: 11,
            },
        };
        let cancel = Request::cancel_prepare((*original).clone(), actor);
        let Request::CancelPrepare(expected) = cancel.clone() else {
            unreachable!()
        };
        let wire = cancel.encode().unwrap();
        assert!(wire.starts_with("cancel-prepare "));
        assert_eq!(Request::parse(&wire), Some(cancel));
        assert_eq!(
            Request::parse(wire.split(" actor-pid=").next().unwrap()),
            None
        );
        let receipt = format!(
            "OK native-act state=cancelled request={}\n",
            aterm_codec::hex::encode_lower(wire.as_bytes())
        );
        assert_eq!(
            Reply::parse(&receipt),
            Some(Reply::Cancelled(expected.clone()))
        );
        assert_eq!(
            Reply::parse(&(receipt.trim_end().to_string() + " extra=1")),
            None
        );
        let changed = wire.replace("deadline-us=91", "deadline-us=092");
        assert_eq!(
            Reply::parse(&format!(
                "OK native-act state=cancelled request={}",
                aterm_codec::hex::encode_lower(changed.as_bytes())
            )),
            None
        );
        assert_eq!(Request::parse(&(wire + " epoch=00")), None);
        let mut different = expected.clone();
        different.prepare.deadline_us += 1;
        assert_ne!(Reply::Cancelled(different), Reply::Cancelled(expected));
    }

    #[test]
    fn constructor_discovery_is_closed_canonical_and_separate_from_info() {
        assert_eq!(
            Request::Constructors.encode().as_deref(),
            Some("constructors")
        );
        assert_eq!(Request::parse("constructors"), Some(Request::Constructors));
        for (words, expected) in [
            ("-", Vec::new()),
            ("own-image", vec![Constructor::OwnImage]),
            ("private-restart", vec![Constructor::PrivateRestart]),
            ("own-image,private-restart", Constructor::ALL.to_vec()),
        ] {
            assert_eq!(
                Reply::parse(&format!("OK native-act state=constructors plans={words}")),
                Some(Reply::Constructors(expected))
            );
        }
        for words in [
            "",
            "own-image,own-image",
            "private-restart,own-image",
            "unknown",
            "own-image,unknown",
            "- extra=1",
        ] {
            assert_eq!(
                Reply::parse(&format!("OK native-act state=constructors plans={words}")),
                None
            );
        }
        assert_eq!(Request::parse("constructors plans=own-image"), None);
        let info = format!(
            "OK native-act state=info epoch={} native-act=1 handoff=blocked clock=monotonic-us own-image=grok-alab:upgrade",
            "34".repeat(16)
        );
        assert!(matches!(Reply::parse(&info), Some(Reply::Info { .. })));
        assert_eq!(Reply::parse(&(info + " plans=own-image")), None);
    }

    #[test]
    fn typing_duties_invalid_capabilities_and_noncanonical_paths_cannot_enter_native_wire() {
        let Request::Prepare(mut request) = fixture() else {
            unreachable!()
        };
        for duty in [
            Scope::TurnEnd,
            Scope::Approvals,
            Scope::Questions,
            Scope::Walls,
        ] {
            request.duty = duty;
            assert!(Request::Prepare(request.clone()).encode().is_none());
        }
        request.duty = Scope::Upgrade;
        let NativePlan::OwnImage { target, .. } = &mut request.plan else {
            unreachable!()
        };
        target.path = b"relative".to_vec();
        assert!(Request::Prepare(request).encode().is_none());
        let binding = Binding {
            id: "ab".repeat(16),
            capability: "cd".repeat(32),
        };
        for request in [
            Request::Query(binding.clone()),
            Request::Recover(binding.clone()),
            Request::Abort(binding.clone()),
            Request::Commit(binding.clone(), ImagePhase::Forward, "0.1.0".into()),
        ] {
            assert_eq!(Request::parse(&request.encode().unwrap()), Some(request));
        }
        assert!(
            Request::Query(Binding {
                capability: "C".repeat(64),
                ..binding
            })
            .encode()
            .is_none()
        );
    }

    #[test]
    fn final_commit_transport_failure_is_unknown_and_never_rollback_safe() {
        struct Closed;
        impl std::io::Read for Closed {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Ok(0)
            }
        }
        impl std::io::Write for Closed {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut client = crate::RelayClient::new(Closed);
        let binding = Binding {
            id: "ab".repeat(16),
            capability: "cd".repeat(32),
        };
        assert_eq!(
            commit(
                &mut client,
                "session",
                &binding,
                ImagePhase::Forward,
                "0.1.0"
            ),
            Decision::Unknown
        );
        assert_eq!(Reply::parse("OK native-act state=committed extra=1"), None);
        assert_eq!(
            Reply::parse("OK native-act state=rollback-safe"),
            Some(Reply::Decision(Decision::ProvenNotCommittedRollbackSafe))
        );
        assert!(!format!("{binding:?}").contains(&binding.capability));
    }
}
