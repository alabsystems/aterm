// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Approval-policy probe boundary ([`PolicyProbe`], [`LiveProbe`], [`doors`]).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use astream_effects::{EffectRecord, KeyedIndex};
use aterm_types::MutexExt;
use serde::{Deserialize, Serialize};

use super::approval::ApprovalCtx;
use super::claude_settings::{AskKind, OwnerAsk};
use super::git_config::{GitView, PROJECTION, VALUE_SET, WorkerEnv};
use super::rm_breaker::{ScratchRoot, abs_components};
use super::shell_startup::ShellStartup;
use crate::supervise::fx::Fx;
use crate::supervise::tape::codec::CodecError;
use crate::supervise::tape::core::TapeValue;
use crate::supervise::tape::doors;
use crate::supervise::tape::header::Roots;

/// How the approval policy looks at the machine ([module docs](self)).
pub trait PolicyProbe: Send + Sync + std::fmt::Debug {
    /// A name in a folder on the way to the absolute path `comps` that differs from the drawn one.
    /// # Errors
    /// A folder on the way cannot be listed, or holds too many entries.
    fn hidden_twin(&self, comps: &[String]) -> Result<Option<String>, String> {
        self.hidden_twin_in(comps, None)
    }

    /// [`Self::hidden_twin`] with `home` supplied to avoid TCC-protected macOS folders.
    /// # Errors
    /// A folder on the way cannot be listed, or holds too many entries.
    fn hidden_twin_in(
        &self,
        comps: &[String],
        home: Option<&Path>,
    ) -> Result<Option<String>, String>;

    /// `comps` with every symbolic link resolved (the Read rule's resolver).
    fn real_components(&self, comps: &[String]) -> Option<Vec<String>>;

    /// Where the absolute path `comps` leads on the disk as the kernel walks it for `rm`.
    /// # Errors
    /// A link climbs out of a missing directory, the links do not end.
    fn rm_physical(&self, comps: &[String]) -> Result<OnDisk, String>;

    /// The worker: the process leading the foreground group `group` of session `sid`.
    /// # Errors
    /// The process cannot be read, or names another session or none.
    fn worker_proc(&self, group: u32, sid: &str) -> Result<WorkerEnv, String>;

    /// What a git command the worker runs in `dir` would load.
    /// # Errors
    /// No git can be named, or its configuration cannot be read.
    fn git_view(
        &self,
        worker: &WorkerEnv,
        dir: &Path,
        writes: &WorkerWrites<'_>,
    ) -> Result<GitView, String>;

    /// Check Claude Code settings for `launch` for hazardous `env` variables.
    /// # Errors
    /// Such a variable, or a settings file that cannot be read whole.
    fn settings_env(&self, worker: &WorkerEnv, launch: &Path) -> Result<(), String>;

    /// What the worker's Bash tool's shell starts with ([`super::shell_startup::read`]).
    /// # Errors
    /// What fails every line, or why the startup could not be read.
    fn shell_startup(&self, worker: &WorkerEnv, launch: &Path) -> Result<ShellStartup, String>;

    /// The ask rules and PreToolUse hooks of the agent's own Claude Code settings.
    /// # Errors
    /// Why they could not be read.
    fn owner_asks(&self, worker: &WorkerEnv, launch: &Path) -> Result<Vec<OwnerAsk>, String>;

    /// Where the worker's Bash tool may stand beyond `cwd`.
    fn shell_cwds(&self, worker: &WorkerEnv, cwd: &Path) -> Vec<PathBuf>;
}

/// An absolute path as the disk resolves it for `rm` ([`PolicyProbe::rm_physical`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnDisk {
    /// Where it leads.
    pub comps: Vec<String>,
    /// Whether a symbolic link on the way was followed.
    pub foreign_link: bool,
}

/// Where the worker may write unapproved.
#[derive(Debug, Clone, Copy)]
pub struct WorkerWrites<'a> {
    /// The session's working directory.
    pub cwd: &'a Path,
    /// Where its Bash tool may stand.
    pub shell_cwds: &'a [PathBuf],
    /// The roots an `rm` may point into.
    pub scratch_roots: &'a [ScratchRoot],
}

impl<'a> WorkerWrites<'a> {
    /// What `ctx` says the worker may write.
    #[must_use]
    pub fn of(ctx: &'a ApprovalCtx) -> Self {
        Self {
            cwd: &ctx.cwd,
            shell_cwds: &ctx.shell_cwds,
            scratch_roots: &ctx.scratch_roots,
        }
    }

    /// Whether the worker may write under `dir`.
    pub fn holds(&self, dir: &Path, resolved: &dyn Fn(&Path) -> PathBuf) -> bool {
        let forms = [dir.to_path_buf(), resolved(dir)];
        let bases: Vec<PathBuf> = std::iter::once(self.cwd)
            .chain(self.shell_cwds.iter().map(PathBuf::as_path))
            .flat_map(|b| [b.to_path_buf(), resolved(b)])
            .collect();
        forms.iter().any(|d| {
            bases.iter().any(|b| d.starts_with(b))
                || abs_components(&d.to_string_lossy())
                    .is_some_and(|comps| self.scratch_roots.iter().any(|r| r.holds(&comps)))
        })
    }
}

/// The shipped probe: every look made on this machine, as the policy always made it.
#[derive(Debug, Clone, Copy)]
pub struct LiveProbe;

/// A run recorded to a harness tape.
#[derive(Clone)]
pub(crate) struct TapedProbe<P: PolicyProbe = LiveProbe> {
    fx: Fx,
    live: P,
    /// The tape's seed for the worker's digest ([`WorkerEnv::digest`]).
    seed: u64,
}

/// A replay's probe ([`TapedProbe`] over a replaying door).
pub(crate) type ReplayProbe = TapedProbe<NoLook>;

impl<P: PolicyProbe> std::fmt::Debug for TapedProbe<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TapedProbe({:?})", self.live)
    }
}

impl TapedProbe {
    /// A probe recording through `fx`, the worker digested with `seed`.
    pub(crate) const fn new(fx: Fx, seed: u64) -> Self {
        Self {
            fx,
            live: LiveProbe,
            seed,
        }
    }
}

impl ReplayProbe {
    /// A probe answering from the tape `fx` replays, the worker digested with `seed`.
    /// # Panics
    /// `fx` does not replay: [`NoLook`] has no look to make.
    pub(crate) fn replaying(fx: Fx, seed: u64) -> Self {
        assert!(
            fx.replaying(),
            "a replay's probe over a door that does not replay"
        );
        Self {
            fx,
            live: NoLook,
            seed,
        }
    }
}

/// A replay's live arm: no look at the machine at all.
#[derive(Debug, Clone, Copy)]
pub(crate) struct NoLook;

/// What a [`NoLook`] says if it is ever asked.
const NO_LOOK: &str =
    "a replay never looks at the machine: its door answers every probe from the tape";

impl PolicyProbe for NoLook {
    fn hidden_twin_in(&self, _: &[String], _: Option<&Path>) -> Result<Option<String>, String> {
        unreachable!("{NO_LOOK}")
    }

    fn real_components(&self, _: &[String]) -> Option<Vec<String>> {
        unreachable!("{NO_LOOK}")
    }

    fn rm_physical(&self, _: &[String]) -> Result<OnDisk, String> {
        unreachable!("{NO_LOOK}")
    }

    fn worker_proc(&self, _: u32, _: &str) -> Result<WorkerEnv, String> {
        unreachable!("{NO_LOOK}")
    }

    fn git_view(&self, _: &WorkerEnv, _: &Path, _: &WorkerWrites<'_>) -> Result<GitView, String> {
        unreachable!("{NO_LOOK}")
    }

    fn settings_env(&self, _: &WorkerEnv, _: &Path) -> Result<(), String> {
        unreachable!("{NO_LOOK}")
    }

    fn shell_startup(&self, _: &WorkerEnv, _: &Path) -> Result<ShellStartup, String> {
        unreachable!("{NO_LOOK}")
    }

    fn owner_asks(&self, _: &WorkerEnv, _: &Path) -> Result<Vec<OwnerAsk>, String> {
        unreachable!("{NO_LOOK}")
    }

    fn shell_cwds(&self, _: &WorkerEnv, _: &Path) -> Vec<PathBuf> {
        unreachable!("{NO_LOOK}")
    }
}

/// How a probe's request is written ([module docs](self)).
struct Requests<'a> {
    path: &'a dyn Fn(&Path) -> Vec<u8>,
    seed: u64,
}

impl Requests<'_> {
    /// The absolute path `comps` as the run's roots write it.
    fn comps(&self, comps: &[String]) -> Vec<u8> {
        (self.path)(Path::new(&format!("/{}", comps.join("/"))))
    }

    /// `key=value` lines, each path as the run's roots write it.
    fn lines(&self, fields: &[(&str, Field<'_>)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (key, value) in fields {
            out.extend_from_slice(key.as_bytes());
            out.push(b'=');
            match value {
                Field::Text(t) => out.extend_from_slice(t.as_bytes()),
                Field::Path(p) => out.extend((self.path)(p)),
            }
            out.push(b'\n');
        }
        out
    }

    /// The worker's digest as a request names it.
    fn worker(&self, worker: &WorkerEnv) -> String {
        format!("{:016x}", worker.digest(self.seed))
    }

    /// A request about the worker launched in `at` (`launch=`, `cwd=`).
    fn worker_at(&self, worker: &WorkerEnv, key: &str, at: &Path) -> Vec<u8> {
        self.lines(&[
            ("worker", Field::Text(&self.worker(worker))),
            (key, Field::Path(at)),
        ])
    }

    /// The worker asked for by its foreground group and session.
    fn worker_proc(&self, group: u32, sid: &str) -> Vec<u8> {
        self.lines(&[
            ("group", Field::Text(&group.to_string())),
            ("sid", Field::Text(sid)),
        ])
    }

    /// A git view asked for: the worker, the directory, and where the worker may write.
    fn git_view(&self, worker: &WorkerEnv, dir: &Path, writes: &WorkerWrites<'_>) -> Vec<u8> {
        let worker = self.worker(worker);
        let mut fields = vec![
            ("worker", Field::Text(&worker)),
            ("dir", Field::Path(dir)),
            ("cwd", Field::Path(writes.cwd)),
        ];
        fields.extend(
            writes
                .shell_cwds
                .iter()
                .map(|d| ("shell_cwd", Field::Path(d))),
        );
        fields.extend(
            writes
                .scratch_roots
                .iter()
                .map(|r| ("root", Field::Text(r.label()))),
        );
        self.lines(&fields)
    }
}

impl<P: PolicyProbe> TapedProbe<P> {
    /// A request written by [`Requests`] through this probe's door.
    fn request(&self, write: impl FnOnce(&Requests<'_>) -> Vec<u8>) -> Vec<u8> {
        let path = |p: &Path| self.fx.path_bytes(p);
        write(&Requests {
            path: &path,
            seed: self.seed,
        })
    }
}

/// One value of a probe's request.
enum Field<'a> {
    Text(&'a str),
    Path(&'a Path),
}

impl<P: PolicyProbe> PolicyProbe for TapedProbe<P> {
    fn hidden_twin_in(
        &self,
        comps: &[String],
        home: Option<&Path>,
    ) -> Result<Option<String>, String> {
        self.fx.exchange(
            doors::PROBE_HIDDEN_TWIN,
            || self.request(|r| r.comps(comps)),
            || self.live.hidden_twin_in(comps, home),
        )
    }

    fn real_components(&self, comps: &[String]) -> Option<Vec<String>> {
        self.fx.exchange(
            doors::PROBE_REAL_COMPONENTS,
            || self.request(|r| r.comps(comps)),
            || self.live.real_components(comps),
        )
    }

    fn rm_physical(&self, comps: &[String]) -> Result<OnDisk, String> {
        self.fx.exchange(
            doors::PROBE_RM_PHYSICAL,
            || self.request(|r| r.comps(comps)),
            || self.live.rm_physical(comps),
        )
    }

    fn worker_proc(&self, group: u32, sid: &str) -> Result<WorkerEnv, String> {
        self.fx.exchange(
            doors::PROBE_WORKER_PROC,
            || self.request(|r| r.worker_proc(group, sid)),
            || {
                self.live
                    .worker_proc(group, sid)
                    .map(|w| w.digested(self.seed))
            },
        )
    }

    fn git_view(
        &self,
        worker: &WorkerEnv,
        dir: &Path,
        writes: &WorkerWrites<'_>,
    ) -> Result<GitView, String> {
        self.fx.exchange(
            doors::PROBE_GIT_VIEW,
            || self.request(|r| r.git_view(worker, dir, writes)),
            || self.live.git_view(worker, dir, writes),
        )
    }

    fn settings_env(&self, worker: &WorkerEnv, launch: &Path) -> Result<(), String> {
        self.fx.exchange(
            doors::PROBE_SETTINGS_ENV,
            || self.request(|r| r.worker_at(worker, "launch", launch)),
            || self.live.settings_env(worker, launch),
        )
    }

    fn shell_startup(&self, worker: &WorkerEnv, launch: &Path) -> Result<ShellStartup, String> {
        self.fx.exchange(
            doors::PROBE_SHELL_STARTUP,
            || self.request(|r| r.worker_at(worker, "launch", launch)),
            || self.live.shell_startup(worker, launch),
        )
    }

    fn owner_asks(&self, worker: &WorkerEnv, launch: &Path) -> Result<Vec<OwnerAsk>, String> {
        self.fx.exchange(
            doors::PROBE_OWNER_ASKS,
            || self.request(|r| r.worker_at(worker, "launch", launch)),
            || self.live.owner_asks(worker, launch),
        )
    }

    fn shell_cwds(&self, worker: &WorkerEnv, cwd: &Path) -> Vec<PathBuf> {
        self.fx.exchange(
            doors::PROBE_SHELL_CWDS,
            || self.request(|r| r.worker_at(worker, "cwd", cwd)),
            || self.live.shell_cwds(worker, cwd),
        )
    }
}

/// The tape's effects, by ordinal, and astream-effects' keyed index of their round trips.
pub(crate) type TapeIndex = KeyedIndex<Arc<[EffectRecord]>>;

/// A probe that answers from a tape, asked out of order.
#[derive(Debug)]
pub(crate) struct KeyedProbe {
    /// The tape's records, by ordinal, and their keyed index, shared.
    index: Arc<TapeIndex>,
    /// How many of them the recorded loop had read at the point.
    seen: usize,
    /// The tape's roots as recorded.
    roots: Roots,
    /// The tape's seed for a worker's digest ([`WorkerEnv::digest`]).
    seed: u64,
    misses: Mutex<Vec<String>>,
}

impl KeyedProbe {
    /// A probe standing after the first `seen` records of `index` under `roots` and `seed`.
    pub(crate) const fn at(index: Arc<TapeIndex>, seen: usize, roots: Roots, seed: u64) -> Self {
        Self {
            index,
            seen,
            roots,
            seed,
            misses: Mutex::new(Vec::new()),
        }
    }

    /// Every look asked that the tape did not answer before the point.
    pub(crate) fn misses(&self) -> Vec<String> {
        self.misses.lock_or_recover().clone()
    }

    /// The answer to the request `write` writes at `door`, or `unrecorded` with why.
    fn answer<A: TapeValue>(
        &self,
        door: &str,
        write: impl FnOnce(&Requests<'_>) -> Vec<u8>,
        unrecorded: impl FnOnce(String) -> A,
    ) -> A {
        let path = |p: &Path| {
            String::from_utf8_lossy(&self.roots.encode(p))
                .into_owned()
                .into_bytes()
        };
        let request = write(&Requests {
            path: &path,
            seed: self.seed,
        });
        let found = self
            .index
            .lookup(door, &request, self.seen)
            .map_err(|_| "not on the tape before this point")
            .and_then(|hit| {
                A::from_tape(hit.reply).map_err(|_| "on the tape in a form this build cannot read")
            });
        found.unwrap_or_else(|why| {
            self.misses.lock_or_recover().push(format!(
                "{door} {}",
                String::from_utf8_lossy(&request)
                    .trim_end()
                    .replace('\n', " ")
            ));
            unrecorded(format!("{door}: {why}"))
        })
    }
}

impl PolicyProbe for KeyedProbe {
    fn hidden_twin_in(&self, comps: &[String], _: Option<&Path>) -> Result<Option<String>, String> {
        self.answer(doors::PROBE_HIDDEN_TWIN, |r| r.comps(comps), Err)
    }

    fn real_components(&self, comps: &[String]) -> Option<Vec<String>> {
        self.answer(doors::PROBE_REAL_COMPONENTS, |r| r.comps(comps), |_| None)
    }

    fn rm_physical(&self, comps: &[String]) -> Result<OnDisk, String> {
        self.answer(doors::PROBE_RM_PHYSICAL, |r| r.comps(comps), Err)
    }

    fn worker_proc(&self, group: u32, sid: &str) -> Result<WorkerEnv, String> {
        self.answer(doors::PROBE_WORKER_PROC, |r| r.worker_proc(group, sid), Err)
    }

    fn git_view(
        &self,
        worker: &WorkerEnv,
        dir: &Path,
        writes: &WorkerWrites<'_>,
    ) -> Result<GitView, String> {
        self.answer(
            doors::PROBE_GIT_VIEW,
            |r| r.git_view(worker, dir, writes),
            Err,
        )
    }

    fn settings_env(&self, worker: &WorkerEnv, launch: &Path) -> Result<(), String> {
        self.answer(
            doors::PROBE_SETTINGS_ENV,
            |r| r.worker_at(worker, "launch", launch),
            Err,
        )
    }

    fn shell_startup(&self, worker: &WorkerEnv, launch: &Path) -> Result<ShellStartup, String> {
        self.answer(
            doors::PROBE_SHELL_STARTUP,
            |r| r.worker_at(worker, "launch", launch),
            Err,
        )
    }

    fn owner_asks(&self, worker: &WorkerEnv, launch: &Path) -> Result<Vec<OwnerAsk>, String> {
        self.answer(
            doors::PROBE_OWNER_ASKS,
            |r| r.worker_at(worker, "launch", launch),
            Err,
        )
    }

    fn shell_cwds(&self, worker: &WorkerEnv, cwd: &Path) -> Vec<PathBuf> {
        self.answer(
            doors::PROBE_SHELL_CWDS,
            |r| r.worker_at(worker, "cwd", cwd),
            |_| Vec::new(),
        )
    }
}

/// A probe's answer, or why it has none, as a tape holds it.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Answer<T> {
    Ok(T),
    Err(String),
}

/// `value` as JSON bytes.
fn json<T: Serialize>(value: &T) -> Vec<u8> {
    aterm_json::to_vec(value).unwrap_or_default()
}

/// JSON bytes read back as `T`.
fn of_json<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, CodecError> {
    aterm_json::from_slice(bytes).map_err(|_| CodecError {
        at: 0,
        why: "a probe's answer this build did not write",
    })
}

/// `answer` as a tape holds it, its value written through `wire`.
fn answer<U, T: Serialize>(answer: &Result<U, String>, wire: impl FnOnce(&U) -> T) -> Vec<u8> {
    json(&match answer {
        Ok(value) => Answer::Ok(wire(value)),
        Err(why) => Answer::Err(why.clone()),
    })
}

/// An answer read back, its value made from its mirror by `value`.
fn answer_of<U, T: for<'de> Deserialize<'de>>(
    bytes: &[u8],
    value: impl FnOnce(T) -> Result<U, CodecError>,
) -> Result<Result<U, String>, CodecError> {
    match of_json::<Answer<T>>(bytes)? {
        Answer::Ok(wire) => value(wire).map(Ok),
        Answer::Err(why) => Ok(Err(why)),
    }
}

/// A path as a probe's answer writes it.
fn path_text(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

impl TapeValue for Result<Option<String>, String> {
    fn to_tape(&self) -> Vec<u8> {
        answer(self, Clone::clone)
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        answer_of(bytes, Ok)
    }
}

impl TapeValue for Option<Vec<String>> {
    fn to_tape(&self) -> Vec<u8> {
        json(self)
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        of_json(bytes)
    }
}

impl TapeValue for Result<OnDisk, String> {
    fn to_tape(&self) -> Vec<u8> {
        answer(self, |d| (d.comps.clone(), d.foreign_link))
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        answer_of(bytes, |(comps, foreign_link): (Vec<String>, bool)| {
            Ok(OnDisk {
                comps,
                foreign_link,
            })
        })
    }
}

/// The worker as a tape holds it.
impl TapeValue for Result<WorkerEnv, String> {
    fn to_tape(&self) -> Vec<u8> {
        answer(self, |w| (format!("{:016x}", w.digest(0)), w.pid()))
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        answer_of(bytes, |(digest, pid): (String, Option<u32>)| {
            u64::from_str_radix(&digest, 16)
                .map(|d| WorkerEnv::recorded(d, pid))
                .map_err(|_| CodecError {
                    at: 0,
                    why: "a worker's digest that is not hex",
                })
        })
    }
}

/// A projected git view as a tape holds it ([`GitView::project`]).
#[derive(Serialize, Deserialize)]
struct ViewWire {
    /// [`PROJECTION`], the rule it was projected under.
    projection: u32,
    config: Vec<(String, String, Option<String>)>,
    index_hook: Option<String>,
    filters_selected: Option<Vec<String>>,
    submodules: Vec<(String, ViewWire)>,
    /// Whether the worker's `GIT_EXTERNAL_DIFF` is set.
    external_diff: bool,
}

impl ViewWire {
    /// `view` projected — again, whatever handed it here.
    fn of(view: &GitView) -> Self {
        let v = view.project();
        Self {
            projection: PROJECTION,
            config: v.config,
            index_hook: v.index_hook.as_deref().map(path_text),
            filters_selected: v.filters_selected,
            submodules: v
                .submodules
                .iter()
                .map(|(p, sub)| (path_text(p), Self::of(sub)))
                .collect(),
            external_diff: v.external_diff.is_some(),
        }
    }

    fn view(self) -> Result<GitView, CodecError> {
        if self.projection != PROJECTION {
            return Err(CodecError {
                at: 0,
                why: "a git view projected by a rule this build does not have",
            });
        }
        Ok(GitView {
            config: self.config,
            index_hook: self.index_hook.map(PathBuf::from),
            filters_selected: self.filters_selected,
            submodules: self
                .submodules
                .into_iter()
                .map(|(p, sub)| Ok((PathBuf::from(p), sub.view()?)))
                .collect::<Result<_, CodecError>>()?,
            external_diff: self.external_diff.then(|| VALUE_SET.to_string()),
        })
    }
}

impl TapeValue for Result<GitView, String> {
    fn to_tape(&self) -> Vec<u8> {
        answer(self, ViewWire::of)
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        answer_of(bytes, ViewWire::view)
    }
}

/// A shell's startup as a tape holds it.
pub(crate) type ShellWire = (Vec<(String, String, bool)>, bool);

impl TapeValue for Result<ShellStartup, String> {
    fn to_tape(&self) -> Vec<u8> {
        answer(self, |s| (s.names().to_vec(), s.may_run_zsh()))
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        answer_of(bytes, |(names, zsh): ShellWire| {
            Ok(ShellStartup::of_parts(names, zsh))
        })
    }
}

/// An ask rule or hook as a tape holds it ([`OwnerAsk`]).
#[derive(Serialize, Deserialize)]
pub(crate) struct AskWire {
    /// `rule` or `hook`.
    kind: String,
    /// A rule's tool; a hook's matcher, if it has one.
    tool: Option<String>,
    /// A rule with no specifier.
    bare: bool,
    text: String,
    source: String,
}

impl AskWire {
    pub(crate) fn of(ask: &OwnerAsk) -> Self {
        let (kind, tool, bare) = match &ask.kind {
            AskKind::Rule { tool, bare } => ("rule", Some(tool.clone()), *bare),
            AskKind::Hook { matcher } => ("hook", matcher.clone(), false),
        };
        Self {
            kind: kind.to_string(),
            tool,
            bare,
            text: ask.text.clone(),
            source: ask.source.clone(),
        }
    }

    pub(crate) fn ask(self) -> Result<OwnerAsk, CodecError> {
        let kind = match (self.kind.as_str(), self.tool) {
            ("rule", Some(tool)) => AskKind::Rule {
                tool,
                bare: self.bare,
            },
            ("hook", matcher) => AskKind::Hook { matcher },
            _ => {
                return Err(CodecError {
                    at: 0,
                    why: "an ask this build does not know",
                });
            }
        };
        Ok(OwnerAsk {
            kind,
            text: self.text,
            source: self.source,
        })
    }
}

impl TapeValue for Result<Vec<OwnerAsk>, String> {
    fn to_tape(&self) -> Vec<u8> {
        answer(self, |asks| {
            asks.iter().map(AskWire::of).collect::<Vec<_>>()
        })
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        answer_of(bytes, |asks: Vec<AskWire>| {
            asks.into_iter().map(AskWire::ask).collect()
        })
    }
}

impl TapeValue for Vec<PathBuf> {
    fn to_tape(&self) -> Vec<u8> {
        json(&self.iter().map(|p| path_text(p)).collect::<Vec<_>>())
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        let wire: Vec<String> = of_json(bytes)?;
        Ok(wire.into_iter().map(PathBuf::from).collect())
    }
}

#[cfg(test)]
#[path = "probe_tests.rs"]
pub(crate) mod tests;
