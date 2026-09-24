// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Settings ▸ Packages read projection — the toolchain-package analogue of
//! [`crate::update_screen`]. ONE structured snapshot drives the native
//! `/packages` route: the host-owned [`PackagesService`] reduces off-thread
//! observations of the CO-LOCATED `atpkg`'s durable state (its `status.toml`,
//! dev-link markers, and inert/enabled posture) plus in-flight verb state, and
//! every Settings view renders the same [`PackagesProjection`] via revision
//! fan-out. Collection ([`collect_packages_status`]) always runs OFF the event
//! loop; the service itself is memory-only, mirroring the updater's
//! "no ledger reads on the UI thread" doctrine.

/// Which co-located `atpkg` verb is currently running. The busy gate
/// serializes UI-INITIATED verbs only (one queue keeps feedback honest); the
/// 6-hour background loop's `atpkg update` is a separate process that can run
/// concurrently — atpkg's own sha256/tree_root verify gates keep an interleaved
/// pass fail-closed (a torn stage fails loudly; nothing corrupt activates).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PackagesBusy {
    /// `atpkg update` — check/update every installed managed program.
    Check,
    /// `atpkg install --default-set` — bootstrap-install the signed default set.
    Install,
    /// `atpkg uninstall --all` — remove the whole managed toolset, reclaiming its disk.
    Uninstall,
    /// `atpkg install <name> --elevate=never` — one EXTRA, opted in by the Install
    /// control on its Packages row. atpkg records the opt-in marker itself
    /// (`<prefix>/optin/<name>`) before it installs, so the GUI never writes into the
    /// store; `--elevate=never` keeps a windowed child from ever waiting on a sudo
    /// prompt nobody can see (an extra needs no elevation anyway).
    InstallExtra,
    /// `atpkg install <name> --elevate=osascript`, one process per program in
    /// dependency order — the GUI door for the `needs admin` rows (Apple's Command
    /// Line Tools through `softwareupdate`, Homebrew's signed `.pkg`). macOS shows
    /// its own administrator dialog; the GUI never sees a password.
    InstallAdmin,
    /// `atpkg machine apply` — the `[machine]` host settings (Universal Control off,
    /// build output hidden from Spotlight), applied now from the Security page's
    /// "Apply now". A LOCAL verb: no store lock, no network, and it works with the
    /// package manager switched off. macOS only.
    MachineApply,
}

impl PackagesBusy {
    /// Whether the verb's atpkg may apply the `[machine]` settings — `machine apply`
    /// always, and `update` and every form of `install` at their dispatch edge when the
    /// `[machine]` table changed since the last apply (Phase 3, 2026-09-22) — mirroring
    /// atpkg's `verb_applies_machine_settings` (NOT `uninstall --all`), pinned on the
    /// atpkg side by its source-scan test. The host re-reads the machine record when such
    /// a verb finishes, so the card confirms rather than assumes.
    pub(crate) fn applies_machine_settings(self) -> bool {
        matches!(
            self,
            Self::Check
                | Self::Install
                | Self::InstallExtra
                | Self::InstallAdmin
                | Self::MachineApply
        )
    }

    fn completed_headline(self) -> &'static str {
        match self {
            Self::Check => "Package check completed",
            Self::Install => "ALab tools install completed",
            Self::Uninstall => "ALab tools removed",
            Self::InstallExtra => "Extra install completed",
            Self::InstallAdmin => "Admin install completed",
            Self::MachineApply => "Machine settings applied",
        }
    }

    fn failed_headline(self) -> &'static str {
        match self {
            Self::Check => "Package check failed",
            Self::Install => "ALab tools install failed",
            Self::Uninstall => "ALab tools removal failed",
            Self::InstallExtra => "Extra install failed",
            Self::InstallAdmin => "Admin install failed",
            Self::MachineApply => "Machine settings not applied",
        }
    }
}

/// The final result of the co-located `atpkg` process, distinct from the
/// synchronous UI-thread admission result. Keeping this typed result beside
/// the refreshed status report prevents an old successful `status.toml` from
/// masquerading as the outcome of a process that failed to launch or exited
/// non-zero.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PackagesCommandOutcome {
    Succeeded {
        operation: PackagesBusy,
    },
    Failed {
        operation: PackagesBusy,
        message: String,
    },
}

impl PackagesCommandOutcome {
    fn operation(&self) -> PackagesBusy {
        match self {
            Self::Succeeded { operation } | Self::Failed { operation, .. } => *operation,
        }
    }

    fn headline(&self) -> &'static str {
        match self {
            Self::Succeeded { operation } => operation.completed_headline(),
            Self::Failed { operation, .. } => operation.failed_headline(),
        }
    }

    fn feedback(&self) -> String {
        match self {
            Self::Succeeded { operation } => operation.completed_headline().to_string(),
            Self::Failed { operation, message } => {
                format!("{}: {message}", operation.failed_headline())
            }
        }
    }
}

/// One off-thread worker completion. A silent refresh carries no command
/// outcome; a user verb must carry one matching the operation reserved by
/// [`PackagesService::begin`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PackagesWorkerCompletion {
    pub(crate) report: PackagesStatusReport,
    pub(crate) command: Option<PackagesCommandOutcome>,
    /// The `atpkg machine: …` verdict sentence a [`PackagesBusy::MachineApply`]
    /// worker read off the child's stdout (`applied — …` / `nothing changed — …`),
    /// so the card and the feedback line can quote what the pass said rather than a
    /// generic headline. `None` for every other worker.
    pub(crate) machine_verdict: Option<String>,
    /// The `machine-state:` record a [`PackagesBusy::MachineApply`] worker read off the
    /// child's stdout (2026-09-16): the apply's own measurement, printed last by a
    /// short child, so it confirms the card without a read. `None` for every other
    /// worker — the collected lanes' record was measured at their pass's top, minutes
    /// before it reaches the host, and is never taken as the newest state.
    pub(crate) machine_state: Option<atpkg::machine::MachineState>,
}

impl PackagesWorkerCompletion {
    pub(crate) fn refresh(report: PackagesStatusReport) -> Self {
        Self {
            report,
            command: None,
            machine_verdict: None,
            machine_state: None,
        }
    }

    /// Attach the machine-apply verdict sentence (see `machine_verdict`).
    pub(crate) fn with_machine_verdict(mut self, verdict: Option<String>) -> Self {
        self.machine_verdict = verdict;
        self
    }

    /// Attach the machine-apply record (see `machine_state`).
    pub(crate) fn with_machine_state(
        mut self,
        state: Option<atpkg::machine::MachineState>,
    ) -> Self {
        self.machine_state = state;
        self
    }

    pub(crate) fn command(report: PackagesStatusReport, command: PackagesCommandOutcome) -> Self {
        Self {
            machine_verdict: None,
            machine_state: None,
            report,
            command: Some(command),
        }
    }
}

/// One managed program's last-known state, projected read-only for the page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PackagesProgramRow {
    pub(crate) name: String,
    /// The active build number, if one is installed.
    pub(crate) installed_build: Option<u64>,
    /// atpkg's state line, VERBATIM. For an index-listed program it is one of the
    /// canonical spellings of `atpkg::state` (`managed <build> — pinned by index <N>`,
    /// for a vendor program `managed <version> — <Vendor> latest` or `managed <version> —
    /// <why>`, `system: <path> — not managed by aterm`, `managed <build> — SHADOWED by
    /// <path>`,
    /// `extra — not installed (opt in: …)`, `installed via <protocol>: <path>`,
    /// `needs admin — run: aterm pkg install <name>`, `unavailable on <target>: <hint>`,
    /// `blocked by <dep>: <dep state>`); faults keep their prefixed free text. The row
    /// paints this string as-is — the Packages page says exactly what the pass log,
    /// `doctor` and `which` say (`docs/TOOLCHAIN-PACKAGE-MANAGER.md` §17.2).
    pub(crate) state: String,
    /// The same line, parsed through `atpkg::state`'s own readers — never a second
    /// spelling. Drives the grouping and the controls; the text above drives the paint.
    pub(crate) kind: ProgramStateKind,
    /// Which Packages group the row belongs to.
    pub(crate) group: RowGroup,
    /// Vendor / license / size for an extra, from atpkg's authored roster line
    /// (`atpkg::stub::describe`). `None` for a default-set member or an extra this
    /// binary was published before.
    pub(crate) facts: Option<ExtraFacts>,
    /// Source annotation: `Some("dev-link → /path")` for a dev-linked checkout.
    pub(crate) annotation: Option<String>,
    /// Version, source, last updated and latest known (Phase 4) — read from the row and,
    /// on the worker, the store ([`PackagesStatusReport::attach_store_facts`]).
    pub(crate) update: ProgramUpdate,
    /// The row's words as the page paints them, derived at projection time
    /// ([`ProgramUpdate::words`]); `None` in a report, and for a row with no installed
    /// build to describe (it paints its state verbatim, as before).
    pub(crate) words: Option<ProgramWords>,
}

/// SETTINGS ▸ PACKAGES' FOUR FACTS PER PROGRAM (Phase 4): the installed VERSION (a vendor
/// build's recorded version, an ALab build's number), its SOURCE (`Anthropic latest`,
/// `OpenAI latest`, `ALab index N` — [`atpkg::state::source_words`]), when it was LAST
/// UPDATED, and the LATEST KNOWN build (a vendor's head as its stamp last saw it, an ALab
/// program's pin in the last verified index).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProgramUpdate {
    /// `2.1.280` / `build 1971`; `None` with no installed build.
    pub(crate) version: Option<String>,
    /// Where its builds come from.
    pub(crate) source: String,
    /// When the active build went live (Unix seconds): a vendor build's verification, an
    /// ALab build's `current` flip.
    pub(crate) updated_at: Option<i64>,
    /// The newest build known: a vendor's head, an ALab index pin.
    pub(crate) latest_known: Option<String>,
    /// When a vendor's head was last checked (Unix seconds).
    pub(crate) checked_at: Option<i64>,
}

/// A program row's words: the summary beside its name (`2.1.280  ·  latest from
/// Anthropic`) and the detail under it (`Updated 3 h ago  ·  Checked 4 min ago`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProgramWords {
    pub(crate) summary: String,
    pub(crate) detail: Option<String>,
}

impl ProgramUpdate {
    /// The latest known build when it is not the installed one.
    fn newer_known(&self) -> Option<&str> {
        let version = self.version.as_deref()?;
        self.latest_known
            .as_deref()
            .filter(|latest| *latest != version)
    }

    /// Whether a row whose state says it is current (at its index's pin, at its vendor's
    /// head) knows a newer build than the one installed: then neither the row nor the
    /// page's headline may say "up to date".
    pub(crate) fn behind(&self, kind: &ProgramStateKind) -> bool {
        matches!(
            kind,
            ProgramStateKind::Managed { .. } | ProgramStateKind::VendorLatest { .. }
        ) && self.newer_known().is_some()
    }

    /// The words for a row whose state is `kind`/`state`, times relative to `now` on the
    /// local `clock`. `None` for a row with no installed version: an extra, a system
    /// binary, a row waiting on an administrator paints its [`plain_state`] instead.
    /// The summary is the version and what the state says in words — `up to date`,
    /// `latest from Anthropic`, `you pinned it` — never a claim to be current while a
    /// newer build is known (the detail names it); a state with no plain words (a FAULT)
    /// says where the build comes from, and its own words stay on the reason lines under
    /// the row, whole, fix first ([`reason_lines`]).
    pub(crate) fn words(
        &self,
        kind: &ProgramStateKind,
        state: &str,
        now: i64,
        clock: LocalClock,
    ) -> Option<ProgramWords> {
        let version = self.version.as_deref()?;
        let newer = self.newer_known();
        let said = plain_state(kind, state)
            .filter(|_| !self.behind(kind))
            .or_else(|| source_plain(&self.source));
        // `kept at build 108 — 112 isn't available …` already names the version it keeps
        // and the newer one it cannot take.
        let mut summary = String::from(version);
        let mut newer = newer;
        match said {
            Some(said) if said.starts_with(&format!("kept at {version} ")) => {
                summary = said;
                newer = None;
            }
            Some(said) => {
                summary.push_str("  \u{b7}  ");
                summary.push_str(&said);
            }
            None => {}
        }
        let mut detail: Vec<String> = Vec::new();
        if let Some(at) = self.updated_at {
            detail.push(format!("Updated {}", when_words(at, now, clock)));
        }
        match newer {
            Some(latest) => {
                let mut known = format!("Latest known {latest}");
                if let Some(at) = self.checked_at {
                    known.push_str(", checked ");
                    known.push_str(&when_words(at, now, clock));
                }
                detail.push(known);
            }
            None => {
                if let Some(at) = self.checked_at {
                    detail.push(format!("Checked {}", when_words(at, now, clock)));
                }
            }
        }
        Some(ProgramWords {
            summary,
            detail: (!detail.is_empty()).then(|| detail.join("  \u{b7}  ")),
        })
    }
}

/// A row's state in a person's words (2026-09-23 audit, SB-18): `up to date` (at its
/// index's pin), `latest from Anthropic` (its vendor's head), `you pinned it` (a local
/// hold), `kept at build 8 — 9 isn't available for this Mac` (a group pin with no build
/// for this Mac), `not installed`, `needs an administrator`, `waiting for clt`. `None` for
/// a state these words do not cover — a fault, anything this page does not parse — which
/// rides the row verbatim, fix first ([`reason_lines`]). `status.toml`, `doctor` and the
/// log keep atpkg's own spelling; the page carries it as the row's accessible value.
pub(crate) fn plain_state(kind: &ProgramStateKind, state: &str) -> Option<String> {
    Some(match kind {
        ProgramStateKind::Managed { .. } => "up to date".to_string(),
        ProgramStateKind::VendorLatest { vendor, .. } => format!("latest from {vendor}"),
        ProgramStateKind::VendorKept { why, .. } if why.starts_with("held by local pin") => {
            "you pinned it".to_string()
        }
        // atpkg's reason is already a phrase (`updates from Anthropic`, `rolled back from
        // 2.1.281`, `2.1.281 is yanked`).
        ProgramStateKind::VendorKept { why, .. } => why.clone(),
        ProgramStateKind::Shadowed { path, .. } => format!("another copy runs first: {path}"),
        ProgramStateKind::System { path, .. } => format!("uses the copy at {path}"),
        ProgramStateKind::ExtraNotInstalled => "not installed".to_string(),
        ProgramStateKind::AgentInstalling => "installing\u{2026}".to_string(),
        ProgramStateKind::InstalledVia { protocol, .. } => format!("installed via {protocol}"),
        ProgramStateKind::NeedsAdmin => "needs an administrator to install".to_string(),
        ProgramStateKind::Unavailable => "not available for this Mac".to_string(),
        ProgramStateKind::BlockedBy { dep, .. } => format!("waiting for {dep}"),
        ProgramStateKind::Other => return held_words(state),
    })
}

/// `held: [<owner>'s ]pinned build <N> is not published for <target>; staying on build
/// <C>` ([`atpkg::state::held_unpublished`]) → `kept at build C — N isn't available for
/// this Mac` (`— <owner>'s build N …` on a sibling's row).
fn held_words(state: &str) -> Option<String> {
    let rest = state.strip_prefix(atpkg::state::HELD_PREFIX)?;
    let (owner, rest) = match rest.split_once("'s pinned build ") {
        Some((owner, rest)) => (Some(owner), rest),
        None => (None, rest.strip_prefix("pinned build ")?),
    };
    let (pinned, rest) = rest.split_once(" is not published for ")?;
    let (_, current) = rest.split_once("; staying on build ")?;
    let whose = owner.map_or_else(String::new, |owner| format!("{owner}'s build "));
    Some(format!(
        "kept at build {current} \u{2014} {whose}{pinned} isn\u{2019}t available for this Mac"
    ))
}

/// Whether a pass's recorded outcome is the plain healthy sentence — `up to date (index
/// build 45)`, `up to date (ay build 8256)` — with nothing after it. A qualified one (`… —
/// but not what this machine runs: …`) says more than the headline, and the page shows it.
fn outcome_is_plainly_current(outcome: &str) -> bool {
    let Some(rest) = outcome.strip_prefix("up to date") else {
        return false;
    };
    let rest = rest.trim();
    rest.is_empty() || (rest.starts_with('(') && rest.find(')') == Some(rest.len() - 1))
}

/// Where a build comes from, as a row says it when its state has no plain words:
/// `Anthropic latest` → `from Anthropic`, `ALab index 44` → `from ALab`.
fn source_plain(source: &str) -> Option<String> {
    let vendor = source
        .strip_suffix(" latest")
        .or_else(|| source.split_once(" index").map(|(vendor, _)| vendor))
        .unwrap_or(source)
        .trim();
    (!vendor.is_empty()).then(|| format!("from {vendor}"))
}

/// `checked 3 min ago` → `Checked 3 min ago`.
fn sentence_case(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// WHEN, AS A PERSON READS IT (Phase 4: "local relative time, never raw UTC"): `just now`,
/// `4 min ago`, `3 h ago` inside a day; then the LOCAL calendar — `yesterday 14:05`, a
/// weekday within the week (`Mon 14:05`), `Sep 14` within the year, `Sep 14 2025` beyond —
/// each instant on the offset IT had ([`LocalClock`]), so a time from before a
/// daylight-saving switch keeps its own hour. A time AHEAD of `now` (a clock set back since
/// it was stamped) never reads as "ago": it is its local day and time — `today 16:40`,
/// `Sep 22 09:00`. With no offset known, the calendar is UTC's and says so (`… UTC`).
/// Pure for the test.
pub(crate) fn when_words(at: i64, now: i64, clock: LocalClock) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    let age = now.saturating_sub(at);
    if (0..60).contains(&age) {
        return "just now".to_string();
    }
    if (60..3600).contains(&age) {
        return format!("{} min ago", age / 60);
    }
    if (3600..86_400).contains(&age) {
        return format!("{} h ago", age / 3600);
    }
    let local = at.saturating_add(clock.offset_at(at));
    let day = local.div_euclid(86_400);
    let today = now.saturating_add(clock.offset_at(now)).div_euclid(86_400);
    let tod = local.rem_euclid(86_400);
    let time = format!("{:02}:{:02}", tod / 3600, (tod % 3600) / 60);
    let (year, month, date) = aterm_types::rfc3339::civil_from_days(day);
    let (this_year, _, _) = aterm_types::rfc3339::civil_from_days(today);
    let month = MONTHS[usize::try_from(month - 1).unwrap_or(0).min(11)];
    let mut words = if day == today {
        format!("today {time}")
    } else if day > today {
        format!("{month} {date} {time}")
    } else if day == today - 1 {
        format!("yesterday {time}")
    } else if day > today - 7 && day < today {
        let weekday = WEEKDAYS[usize::try_from(day.rem_euclid(7)).unwrap_or(0)];
        format!("{weekday} {time}")
    } else if year == this_year {
        format!("{month} {date}")
    } else {
        format!("{month} {date} {year}")
    };
    if !clock.known() {
        words.push_str(" UTC");
    }
    words
}

/// How far back the calendar shows a clock time (`yesterday 14:05`, `Mon 09:30`): a week.
/// Older times are dates alone, where a daylight-saving hour moves nothing a person reads.
const CLOCK_WINDOW_S: i64 = 7 * 86_400;

/// THE LOCAL CLOCK the Packages page reads its times on (Phase 4), per instant (review of
/// Phase 4, 2026-09-23): the offset east of UTC NOW, and — when the zone's offset changed
/// inside [`CLOCK_WINDOW_S`] (a daylight-saving switch) — when it changed and the offset
/// before it, so `yesterday 14:05` is on the clock it was stamped under. Read on the worker
/// at EACH collection ([`Self::read`]): it was read once per process, and a window that
/// lived across a switch showed every clock time an hour off. No offset known ⇒ the page
/// says `UTC` beside its times ([`when_words`]) rather than passing UTC off as local.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LocalClock {
    offset_s: Option<i64>,
    /// `(changed_at, offset_before)` — an instant before `changed_at` was on the offset
    /// before.
    changed: Option<(i64, i64)>,
}

impl Default for LocalClock {
    fn default() -> Self {
        Self::UNKNOWN
    }
}

impl LocalClock {
    /// No offset could be read: times are UTC's, and say so.
    pub(crate) const UNKNOWN: Self = Self {
        offset_s: None,
        changed: None,
    };

    /// A clock `offset_s` east of UTC with no switch in the window.
    pub(crate) const fn fixed(offset_s: i64) -> Self {
        Self {
            offset_s: Some(offset_s),
            changed: None,
        }
    }

    /// The clock as `date` reads it — a subprocess per read, so a WORKER's only: the offset
    /// now and a window ago, and only when they differ (the week after a switch) the
    /// switch between them, bisected to the minute (about fourteen reads).
    pub(crate) fn read(now: i64) -> Self {
        Self::probe(now, crate::presence::local_offset_at)
    }

    /// [`Self::read`] over an injected offset reader. Pure for the test.
    fn probe(now: i64, offset_at: impl Fn(i64) -> Option<i64>) -> Self {
        let Some(offset) = offset_at(now) else {
            return Self::UNKNOWN;
        };
        let (mut before_at, mut after_at) = (now - CLOCK_WINDOW_S, now);
        let before = match offset_at(before_at) {
            Some(before) if before != offset => before,
            _ => return Self::fixed(offset),
        };
        // `before_at` reads `before`, `after_at` reads `offset`: the switch is between.
        while after_at - before_at > 60 {
            let mid = before_at + (after_at - before_at) / 2;
            match offset_at(mid) {
                Some(o) if o == before => before_at = mid,
                Some(_) => after_at = mid,
                // A read that failed half-way: every time on today's offset, as before.
                None => return Self::fixed(offset),
            }
        }
        Self {
            offset_s: Some(offset),
            changed: Some((after_at, before)),
        }
    }

    /// The offset `at` was on (UTC's, `0`, when none is known).
    pub(crate) fn offset_at(self, at: i64) -> i64 {
        match self.changed {
            Some((since, before)) if at < since => before,
            _ => self.offset_s.unwrap_or(0),
        }
    }

    pub(crate) fn known(self) -> bool {
        self.offset_s.is_some()
    }
}

impl PackagesProgramRow {
    fn from_status(name: &str, program: &atpkg::ProgramStatus, last_index_build: u64) -> Self {
        let kind = ProgramStateKind::parse(&program.state);
        let group = RowGroup::of(name, &kind);
        // A vendor row names its version; any other installed build is named by its number
        // (a vendor-direct build id reads as its version there too).
        let version = match &kind {
            ProgramStateKind::VendorLatest { version, .. }
            | ProgramStateKind::VendorKept { version, .. } => Some(version.clone()),
            _ => program
                .installed_build
                .map(atpkg::vendor_direct::build_words),
        };
        // What the row itself vouches for: a row pinned by its index IS at the pin, and a
        // vendor's latest IS the head the lane last took. The store refines both.
        let latest_known = match &kind {
            ProgramStateKind::Managed { build, .. } => {
                Some(atpkg::vendor_direct::build_words(*build))
            }
            ProgramStateKind::VendorLatest { version, .. } => Some(version.clone()),
            _ => None,
        };
        Self {
            name: name.to_string(),
            installed_build: program.installed_build,
            state: program.state.clone(),
            // The authored vendor · license · size line rides on every extra AND on the
            // agent programs (default-set, but still a vendor's proprietary tool).
            facts: (group == RowGroup::Extras || atpkg::stub::is_agent_program(name))
                .then(|| atpkg::stub::describe(name).map(ExtraFacts::parse))
                .flatten(),
            update: ProgramUpdate {
                version,
                source: atpkg::state::source_words(name, &program.state, last_index_build),
                updated_at: None,
                latest_known,
                checked_at: None,
            },
            words: None,
            kind,
            group,
            annotation: None,
        }
    }

    /// The extras Install control belongs on an extra that is waiting for consent
    /// (`extra — not installed (opt in: aterm pkg install <name>)`) — never on one
    /// already installed, system-satisfied or unavailable here.
    pub(crate) fn offers_extra_install(&self) -> bool {
        self.group == RowGroup::Extras && matches!(self.kind, ProgramStateKind::ExtraNotInstalled)
    }
}

/// The three groups the Packages page renders (`docs/DESIGN-which-copy-runs-2026-08-27.md`
/// S9: "Packages lists extras separately with vendor, license, size, and an Install
/// control"; §17.8: the `needs admin` rows and the GUI door).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RowGroup {
    /// A default-set member (or any row the index does not mark as an extra).
    Default,
    /// An opt-in extra: listed and pinned, installed only after consent.
    Extras,
    /// A row waiting on an administrator: its own `needs admin` state, or `blocked by`
    /// a dependency whose chain ends in one (Homebrew behind the Command Line Tools).
    NeedsAdmin,
}

impl RowGroup {
    fn of(name: &str, kind: &ProgramStateKind) -> Self {
        // The agent programs (`atpkg::stub::AGENT_PROGRAMS`: claude, codex) are
        // default-set members since the 2026-09-10 owner decision — installed by the
        // pass unasked, never behind an Install control — whatever a stale
        // `extra — not installed` row from an older atpkg still says.
        let agent = atpkg::stub::is_agent_program(name);
        if !agent
            && (matches!(kind, ProgramStateKind::ExtraNotInstalled)
                || atpkg::stub::compiled_extra(name))
        {
            // Needs-admin membership is decided over the WHOLE row set (a blocked
            // chain reaches other rows), in `PackagesStatusReport::from_parts`.
            Self::Extras
        } else if matches!(kind, ProgramStateKind::NeedsAdmin) {
            Self::NeedsAdmin
        } else {
            Self::Default
        }
    }

    pub(crate) fn heading(self) -> &'static str {
        match self {
            Self::Default => "DEFAULT SET",
            Self::Extras => "EXTRAS",
            Self::NeedsAdmin => "NEEDS ADMIN",
        }
    }
}

/// A `status.toml` state line read through `atpkg::state`'s parsers — the SAME
/// readers `doctor` and `which` use, so this page cannot drift from what the pass
/// wrote. `Other` keeps every fault/legacy line (`active`, `error: …`, `linked`) as
/// a plain default-set row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProgramStateKind {
    Managed {
        build: u64,
        index: u64,
    },
    /// A vendor-direct program at its vendor's head: `managed 2.1.280 — Anthropic latest`.
    VendorLatest {
        version: String,
        vendor: String,
    },
    /// A vendor-direct program kept on `version` while the head was not taken, and why
    /// (a hold, a yank, a rollback): `managed 2.1.280 — held by local pin`.
    VendorKept {
        version: String,
        why: String,
    },
    Shadowed {
        build: u64,
        path: String,
    },
    System {
        path: String,
        retired: Option<String>,
    },
    ExtraNotInstalled,
    /// An agent program the pass has not installed YET (`agent program — installing`,
    /// `atpkg::state::AGENT_INSTALLING`): a default-set row on its way, never an
    /// extra waiting for consent.
    AgentInstalling,
    InstalledVia {
        protocol: String,
        path: String,
    },
    NeedsAdmin,
    Unavailable,
    BlockedBy {
        dep: String,
        dep_state: String,
    },
    Other,
}

impl ProgramStateKind {
    pub(crate) fn parse(state: &str) -> Self {
        if let Some((build, index)) = atpkg::state::managed_pin(state) {
            return Self::Managed { build, index };
        }
        if let Some((version, vendor)) = atpkg::state::vendor_latest(state) {
            return Self::VendorLatest {
                version: version.to_string(),
                vendor: vendor.to_string(),
            };
        }
        if let Some((version, why)) = atpkg::state::vendor_row(state) {
            return Self::VendorKept {
                version: version.to_string(),
                why: why.to_string(),
            };
        }
        if let Some((build, path)) = atpkg::state::shadowed_by(state) {
            return Self::Shadowed {
                build,
                path: path.to_string(),
            };
        }
        if let Some(path) = atpkg::state::system_path(state) {
            return Self::System {
                path: path.to_string(),
                retired: atpkg::state::system_retired(state).map(str::to_string),
            };
        }
        if state.starts_with(atpkg::state::AGENT_INSTALLING) {
            return Self::AgentInstalling;
        }
        if state.starts_with(atpkg::state::EXTRA_PREFIX) {
            return Self::ExtraNotInstalled;
        }
        if let Some((protocol, path)) = atpkg::state::installed_via_path(state) {
            return Self::InstalledVia {
                protocol: protocol.to_string(),
                path: path.to_string(),
            };
        }
        if state.starts_with(atpkg::state::NEEDS_ADMIN_PREFIX) {
            return Self::NeedsAdmin;
        }
        if state.starts_with(atpkg::state::UNAVAILABLE_PREFIX) {
            return Self::Unavailable;
        }
        if let Some((dep, dep_state)) = atpkg::state::blocked_by(state) {
            return Self::BlockedBy {
                dep: dep.to_string(),
                dep_state: dep_state.to_string(),
            };
        }
        Self::Other
    }
}

/// What an extra's row says beside its state: the vendor, the license and the size,
/// parsed from atpkg's one authored line per extra (`atpkg::stub::EXTRAS_STUB_NAMES`:
/// `"<vendor product> — <license>, <size>, downloaded from <host>"`). A line that does
/// not follow that grammar is kept whole as the vendor so nothing authored is lost.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExtraFacts {
    pub(crate) vendor: String,
    pub(crate) license: Option<String>,
    pub(crate) size: Option<String>,
}

impl ExtraFacts {
    pub(crate) fn parse(line: &str) -> Self {
        let line = line.trim();
        let Some((vendor, rest)) = line.split_once(" \u{2014} ") else {
            return Self {
                vendor: line.to_string(),
                license: None,
                size: None,
            };
        };
        let mut parts = rest.split(',').map(str::trim).filter(|p| !p.is_empty());
        let license = parts.next().map(str::to_string);
        let size = parts
            .next()
            .filter(|p| p.contains("MB") || p.contains("GB") || p.contains("KB"))
            .map(str::to_string);
        Self {
            vendor: vendor.trim().to_string(),
            license,
            size,
        }
    }

    /// `"<vendor>  ·  <license>  ·  <size>"`, whichever facts the line carried.
    pub(crate) fn line(&self) -> String {
        let mut out = self.vendor.clone();
        for extra in [self.license.as_deref(), self.size.as_deref()]
            .into_iter()
            .flatten()
        {
            out.push_str("  \u{b7}  ");
            out.push_str(extra);
        }
        out
    }
}

/// The programs waiting on an administrator, in DEPENDENCY ORDER — the order the GUI
/// door installs them in (§17.10: `clt` before `brew`, which `requires` it).
///
/// A program waits on admin when its own row is `needs admin — run: aterm pkg install
/// <name>`, or when it is `blocked by <dep>: <dep state>` and following the `blocked by`
/// chain (each tail is the dependency's own row, quoted verbatim) ends at a `needs
/// admin` row. Dependencies come first (chain depth, then name); a cycle or a dangling
/// chain is simply not admin-waiting. Pure: the notice thread, the Packages page and
/// the tests all read the same rule.
pub(crate) fn needs_admin_order(rows: &[(String, String)]) -> Vec<String> {
    let states: std::collections::BTreeMap<&str, &str> = rows
        .iter()
        .map(|(name, state)| (name.as_str(), state.as_str()))
        .collect();
    let mut waiting: Vec<(usize, &str)> = Vec::new();
    for (name, state) in &states {
        // Walk the chain by NAME through the recorded rows, so the depth counts the
        // programs the door has to install first; the quoted tail is the fallback
        // when the dependency has no row of its own.
        let mut depth = 0usize;
        let mut current: String = (*state).to_string();
        let mut seen: Vec<String> = vec![(*name).to_string()];
        let admin = loop {
            if current.starts_with(atpkg::state::NEEDS_ADMIN_PREFIX) {
                break true;
            }
            let Some((dep, dep_state)) = atpkg::state::blocked_by(&current) else {
                break false;
            };
            if seen.iter().any(|s| s == dep) || depth >= rows.len() {
                break false;
            }
            seen.push(dep.to_string());
            depth += 1;
            current = states
                .get(dep)
                .map_or_else(|| dep_state.to_string(), |s| (*s).to_string());
        };
        if admin {
            waiting.push((depth, name));
        }
    }
    waiting.sort();
    waiting
        .into_iter()
        .map(|(_, name)| name.to_string())
        .collect()
}

/// The first-launch admin card's dismissal rule: "Not now" records the set of names it
/// was shown for, and the card is not raised again until that set CHANGES. `marker` is
/// the recorded text (`None` = never dismissed); the card shows for a non-empty set
/// whose recorded form differs from the marker.
pub(crate) fn admin_step_should_show(marker: Option<&str>, names: &[String]) -> bool {
    !names.is_empty() && marker.map(str::trim) != Some(admin_step_marker_text(names).as_str())
}

/// The recorded form of a needs-admin set: one name per line, dependency order kept
/// (the door's order is part of what the user declined).
pub(crate) fn admin_step_marker_text(names: &[String]) -> String {
    names.join("\n")
}

/// The dismissal marker beside `aterm.toml` (the config-dir latch idiom of
/// `connections::first_use_notice_should_show`; never inside atpkg's store, which is
/// atpkg's to write).
pub(crate) const ADMIN_STEP_MARKER: &str = "packages-admin-step-dismissed";

/// The most a marker may hold and still be a record: one program name per line, and
/// atpkg refuses names past 64 bytes, so a legitimate set is a few hundred bytes. Same
/// ceiling as atpkg's own prefix markers (`store::Layout::retired_date`); a bigger file
/// is something else wearing the name and reads as "never dismissed" — the card shows,
/// which errs toward disclosure.
pub(crate) const MAX_ADMIN_STEP_MARKER_BYTES: usize = 4096;

pub(crate) fn admin_step_marker_path(
    config_path: Option<&std::path::Path>,
) -> Option<std::path::PathBuf> {
    config_path
        .and_then(std::path::Path::parent)
        .map(|dir| dir.join(ADMIN_STEP_MARKER))
}

/// The marker's text, when a REGULAR, non-symlink file of at most
/// [`MAX_ADMIN_STEP_MARKER_BYTES`] sits at `path` — the symlink-refusing, size-capped
/// rule every other prefix marker follows (`atpkg::store::Layout::retired_date`,
/// `optin_exists`), spelled once in [`crate::config_marker`]. A planted link, a
/// directory, an oversized or non-UTF-8 file is not a dismissal this machine recorded:
/// `None`, and the card shows.
pub(crate) fn read_admin_step_marker(path: &std::path::Path) -> Option<String> {
    crate::config_marker::read_marker(path, MAX_ADMIN_STEP_MARKER_BYTES)
}

/// Record "Not now" for `names`. Best-effort: an unwritable config dir means the card
/// comes back next pass, which errs toward disclosure.
///
/// Never writes THROUGH anything already at the marker path — the exclusive-sibling
/// plus rename rule and the occupied-path refusal are [`crate::config_marker`]'s,
/// shared with the macOS access card's answer marker; a set too large for a marker
/// is refused there, never truncated.
pub(crate) fn record_admin_step_dismissal(
    config_path: Option<&std::path::Path>,
    names: &[String],
) -> std::io::Result<()> {
    let Some(path) = admin_step_marker_path(config_path) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no config directory to record the dismissal in",
        ));
    };
    crate::config_marker::write_marker(
        &path,
        &admin_step_marker_text(names),
        MAX_ADMIN_STEP_MARKER_BYTES,
    )
}

/// Whether the admin card should be raised NOW for the rows `status.toml` carries —
/// the off-thread half of the first-launch admin step. Reads the marker file (never on
/// the UI thread: callers are the atpkg-update worker). Returns the names, in door
/// order, when the card is due.
pub(crate) fn admin_step_due(
    status: &atpkg::Status,
    config_path: Option<&std::path::Path>,
) -> Option<Vec<String>> {
    let rows: Vec<(String, String)> = status
        .programs
        .iter()
        .map(|(name, program)| (name.clone(), program.state.clone()))
        .collect();
    let names = needs_admin_order(&rows);
    let marker = admin_step_marker_path(config_path).and_then(|p| read_admin_step_marker(&p));
    admin_step_should_show(marker.as_deref(), &names).then_some(names)
}

/// Who installs `name` and how — the honest sentence the admin card and the Needs-admin
/// rows use. Spelled from the §17.8 protocols for the two shipped rows; anything newer
/// says the generic truth (its own installer, through atpkg's door).
pub(crate) fn admin_vendor_line(name: &str) -> String {
    match name {
        "clt" => "Apple Command Line Tools (Apple's installer, via softwareupdate)".to_string(),
        "brew" => "Homebrew (its signed installer package)".to_string(),
        other => format!("{other} (its own installer)"),
    }
}

/// How many lines one program's reason may take on the Packages page: enough for a
/// real diagnosis, bounded so a pathological ledger string cannot stretch the programs
/// card without limit (the posture of the Update page's outcome bound). A `fix:`
/// clause is exempt — it is shown whole, and the bound yields to it.
pub(crate) const MAX_REASON_LINES: usize = 4;

/// The narrowest row [`reason_lines`] wraps to: the bare `…(NNN more)` marker (11
/// characters) must fit on a line of its own.
const MIN_REASON_WIDTH: usize = 12;

/// atpkg's remedy marker, as its own text spells it (`provenance::REMEDY`,
/// `doctor::provenance_line`).
const FIX_MARKER: &str = "fix:";

/// The 2026-09-14 incident's state line, word for word as the install pass recorded it
/// (`atpkg::lay::tracked_refusal` under an `error: stage: ` head) — ~700 characters, the
/// fix LAST. Frozen here: atpkg clears the tag itself now and no longer says this, but a
/// long reason with its fix at the end is still the shape the wrap must handle.
#[cfg(test)]
pub(crate) const INCIDENT_2026_09_14_REASON: &str = "error: stage: this process is \
    provenance-tracked (a probe file it wrote came back carrying com.apple.provenance) and \
    the untracked launchd lane could not run the helper (launchd job exited 78) — refusing \
    rather than write files that would all carry the tag, because the refuse policy is in \
    force (ATPKG_REFUSE_TRACKED_INSTALL set in this environment, or `[packages] \
    tracked_install = \"refuse\"` in aterm.toml): the tag follows the executable and the \
    parent process: every file a tagged trustc/targo (or a cutter descended from a tagged \
    process) writes inherits it, `xattr -d` exits 0 and removes nothing, and \
    tools/proof_snapshot.py refuses a tagged proof snapshot AFTER the ledger claim — a \
    burned build number (v0.83.0, 2026-09-12). fix: unset ATPKG_REFUSE_TRACKED_INSTALL and \
    set tracked_install = \"record\" (or drop the key) and the files are written in-process \
    and recorded beside the build as <build>.tracked-install (`aterm pkg doctor` names it \
    and every tagged shim), or clear what stopped launchd from running the helper — named \
    above — and retry";

/// The lines a program's `state` line paints on the Packages page, each at most
/// `width` characters.
///
/// 2026-09-14 incident: atpkg's install pass recorded a ~700-character `error: stage:
/// this process is provenance-tracked … fix: unset ATPKG_REFUSE_TRACKED_INSTALL …`
/// state whose `fix:` clause came LAST, and the row painted it as one ellipsized line
/// — the part the user needed was exactly the part cut. atpkg spells every remedy
/// `fix: …`, so the marker is a contract this reader relies on:
///
/// * a reason that fits `width` comes back unchanged, one line — the common case, so
///   the canonical `atpkg::state` spellings paint exactly as they always did;
/// * a long reason with a `fix:` clause puts the fix FIRST and whole (every wrapped
///   line of it, past [`MAX_REASON_LINES`] if it must), then the diagnosis,
///   word-wrapped and bounded;
/// * a long reason without one is word-wrapped and bounded.
///
/// When the bound hides diagnosis lines, `…(N more)` is folded into the last visible
/// line in place of its trailing words (the Update page's `bound_outcome_lines`
/// posture), N counting the wrapped lines hidden entirely — and only ever after the
/// fix is complete; a fix that alone fills the bound is followed by the marker on a
/// line of its own, one past the bound. A token longer than `width` (a path) is
/// broken at `width`, so no line exceeds it: the painter's ellipsis is never the
/// layout. Pure — character counts, no font measurement — so the compact and wide
/// pages agree on the row count they budget for.
pub(crate) fn reason_lines(state: &str, width: usize) -> Vec<String> {
    let width = width.max(MIN_REASON_WIDTH);
    if fits(state, width) {
        return vec![state.to_string()];
    }
    let (diagnosis, fix) = match fix_marker(state) {
        Some(at) => {
            let fix = state[at + FIX_MARKER.len()..].trim();
            // The sentence that led into the marker ends with its own punctuation
            // (`…: <what it breaks>. fix: …`, `…; fix: …`); the diagnosis keeps none
            // of it, since the fix no longer follows.
            let diagnosis = state[..at]
                .trim_end()
                .trim_end_matches(['.', ';', ',', ':', '—', '–', '-'])
                .trim_end();
            if fix.is_empty() {
                (state, None)
            } else {
                (diagnosis, Some(fix))
            }
        }
        None => (state, None),
    };
    let mut lines = match fix {
        Some(fix) => wrap_words(&format!("{FIX_MARKER} {fix}"), width),
        None => Vec::new(),
    };
    let rest = wrap_words(diagnosis, width);
    let room = MAX_REASON_LINES.saturating_sub(lines.len());
    if rest.len() <= room {
        lines.extend(rest);
    } else if room == 0 {
        lines.push(more_marker(rest.len()));
    } else {
        lines.extend(bound_with_marker(rest, room, width));
    }
    lines
}

fn more_marker(hidden: usize) -> String {
    format!("\u{2026}({hidden} more)")
}

/// The first `room` of `lines`, the last of them giving up trailing words to the
/// `…(N more)` marker until it fits `width`. N counts the lines hidden entirely; a
/// boundary line that loses ALL its words to the marker counts as hidden too, so the
/// number never understates.
fn bound_with_marker(lines: Vec<String>, room: usize, width: usize) -> Vec<String> {
    debug_assert!(room >= 1 && lines.len() > room);
    let hidden = lines.len() - room;
    let mut kept: Vec<String> = lines.into_iter().take(room).collect();
    let Some(last) = kept.pop() else {
        return kept;
    };
    let mut words: Vec<&str> = last.split_whitespace().collect();
    loop {
        let candidate = if words.is_empty() {
            more_marker(hidden + 1)
        } else {
            format!("{} {}", words.join(" "), more_marker(hidden))
        };
        if fits(&candidate, width) || words.is_empty() {
            kept.push(candidate);
            return kept;
        }
        words.pop();
    }
}

/// The byte offset of the `fix:` marker in `state`, when it carries one: the word
/// `fix:` at a word boundary and followed by whitespace or the end — `prefix:` and
/// `suffix:` are not remedies, and `fix:` glued to a path segment is not one either.
fn fix_marker(state: &str) -> Option<usize> {
    state.match_indices(FIX_MARKER).find_map(|(at, _)| {
        let boundary_before = state[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
        let boundary_after = state[at + FIX_MARKER.len()..]
            .chars()
            .next()
            .is_none_or(char::is_whitespace);
        (boundary_before && boundary_after).then_some(at)
    })
}

/// Greedy word wrap to `width` characters. Whitespace of every kind is a break (a
/// recorded error may carry a newline); a token longer than a whole line is broken
/// at `width` so every line fits. Character counts (Unicode scalars): the reasons are
/// atpkg's ASCII-and-em-dash prose, and a measured wrap would need the font this
/// pure projection does not have.
fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut used = 0usize;
    let budget = width * ADVANCE_UNIT;
    for word in text.split_whitespace() {
        let len = advance(word);
        if used > 0 && used + ADVANCE_UNIT + len <= budget {
            current.push(' ');
            current.push_str(word);
            used += ADVANCE_UNIT + len;
            continue;
        }
        if used > 0 {
            lines.push(std::mem::take(&mut current));
        }
        if len <= budget {
            current.push_str(word);
            used = len;
            continue;
        }
        // A token wider than the row is broken where its advance fills it.
        let mut chunk = String::new();
        let mut chunk_used = 0usize;
        for c in word.chars() {
            let a = glyph_advance(c);
            if chunk_used + a > budget && !chunk.is_empty() {
                lines.push(std::mem::take(&mut chunk));
                chunk_used = 0;
            }
            chunk.push(c);
            chunk_used += a;
        }
        current = chunk;
        used = chunk_used;
    }
    if used > 0 {
        lines.push(current);
    }
    lines
}

/// The wrap's unit: one average lowercase glyph, in the sub-unit [`glyph_advance`]
/// counts in. The row budget `width` is in these units.
const ADVANCE_UNIT: usize = 20;

/// An approximate advance for `c` in a proportional UI face, in twentieths of an
/// average lowercase glyph (2026-09-15): the wrap used to count CHARACTERS, and a
/// line of 64 characters holding `ATPKG_REFUSE_TRACKED_INSTALL` — 28 capitals and
/// underscores — measured 405pt against a 402pt row at the compact width and
/// overflowed into the painter's ellipsis, the very thing the wrap exists to prevent.
/// The weights were fitted against the painter's own measurements of four incident
/// lines (844, 873, 149 and 969pt): capitals, `_`, `—` and `@#%&` 1.2 glyphs, `m`/`w`
/// 1.5, the narrow glyphs and most punctuation 0.65, digits and the rest one — which
/// holds the spread of pt-per-unit across those lines to 7% (0.325–0.35), and the row
/// budgets ([`super::native_settings`]' `packages_reason_wrap_chars`) are set from
/// the widest of them. Coarse on purpose — the paint audit in `native_settings`' tests
/// measures the result with the real face, and this table only has to keep a row
/// under its budget.
fn glyph_advance(c: char) -> usize {
    match c {
        'A'..='Z' | '_' | '\u{2014}' | '\u{2013}' | '@' | '#' | '%' | '&' => 24,
        'm' | 'w' => 30,
        'i' | 'j' | 'l' | 't' | 'f' | 'r' | '\'' | '"' | '`' | '.' | ',' | ':' | ';' | '!'
        | '|' | '(' | ')' | '[' | ']' | '{' | '}' | '/' | '\\' | '-' | ' ' => 13,
        _ => ADVANCE_UNIT,
    }
}

/// The advance of `word`, summed over its glyphs ([`glyph_advance`]).
fn advance(word: &str) -> usize {
    word.chars().map(glyph_advance).sum()
}

/// Whether `text` fits a row of `width` units — the one predicate the short-state
/// pass-through, the wrap and the marker's tail share.
fn fits(text: &str, width: usize) -> bool {
    advance(text) <= width * ADVANCE_UNIT
}

/// Whether `text` fits one row line at `width`'s budget, measured the way the reason wrap
/// measures ([`reason_lines`]) — so a program row's detail rides inline exactly when it
/// fits (Phase 4).
pub(crate) fn fits_one_line(text: &str, width: usize) -> bool {
    fits(text, width.max(MIN_REASON_WIDTH))
}

/// How many lines one Activity event may take on the Packages page: a long refusal says
/// enough to be read there, and Open Log has the rest — the card's height stays bounded.
pub(crate) const MAX_ACTIVITY_LINES: usize = 3;

/// The lines one Activity event paints, each at most `width` characters: the sentence
/// whole when it fits (the common case), else word-wrapped in order — a token wider than a
/// line broken at it — and bounded at [`MAX_ACTIVITY_LINES`], `…(N more)` folded into the
/// last. The ~700-character refusal the program rows were reworked to show (2026-09-14)
/// was cut mid-sentence by the painter's ellipsis in Activity until review of Phase 4
/// (2026-09-23). Pure, counted the way [`reason_lines`] counts, so both pages budget the
/// same lines.
pub(crate) fn activity_text_lines(text: &str, width: usize) -> Vec<String> {
    let width = width.max(MIN_REASON_WIDTH);
    if fits(text, width) {
        return vec![text.to_string()];
    }
    let lines = wrap_words(text, width);
    if lines.len() <= MAX_ACTIVITY_LINES {
        lines
    } else {
        bound_with_marker(lines, MAX_ACTIVITY_LINES, width)
    }
}

/// The lines one of the page's own sentences paints at `width`: whole when it fits, else
/// broken first between its ` · ` parts (a part that fits a line is never split, and the
/// separator a break falls on is not painted) and word-wrapped within a part that does not,
/// in order, EVERY line kept — the Background service line and the retired `auto_update`
/// note, whose last words are what to do (Update Now, the last full check, how to clear the
/// key), so no bound may drop them. Only for sentences of bounded length the page or atpkg
/// writes; a recorded reason is [`reason_lines`]' and an event [`activity_text_lines`]'.
/// Counted the way both count, so a page budgets what it paints.
pub(crate) fn note_lines(text: &str, width: usize) -> Vec<String> {
    const SEP: &str = " \u{b7} ";
    let width = width.max(MIN_REASON_WIDTH);
    if fits(text, width) {
        return vec![text.to_string()];
    }
    let mut lines: Vec<String> = Vec::new();
    for part in text.split(SEP) {
        match lines.last_mut() {
            Some(line) if fits(&format!("{line}{SEP}{part}"), width) => {
                line.push_str(SEP);
                line.push_str(part);
            }
            _ => lines.extend(wrap_words(part, width)),
        }
    }
    lines
}

/// atpkg's status ledger names its whole-toolset pseudo-program `*toolset*` —
/// terminal-flavoured emphasis that a native page paints as literal asterisks
/// (2026-08 settings audit). The meta-row gets its product name; any other
/// emphasis-wrapped ledger name unwraps the same way. Real program names pass
/// through untouched, and node KEYS keep the raw name for stable identity. One
/// spelling for the rows and the attention headline ([`attention_items`]).
pub(crate) fn package_program_display_name(name: &str) -> String {
    if name == "*toolset*" {
        return "ALab tools".to_string();
    }
    name.strip_prefix('*')
        .and_then(|inner| inner.strip_suffix('*'))
        .filter(|inner| !inner.is_empty())
        .unwrap_or(name)
        .to_string()
}

/// The words of atpkg's row for a vendor build whose head check has not reached
/// its vendor for a day: `managed <version> — <Vendor>'s release channel not
/// reached since <date>` (`record_vendor_status` in `crates/atpkg/src/cli.rs`).
const VENDOR_UNREACHED: &str = "'s release channel not reached since ";

/// One recorded row the Packages badge counts, as the headline names it, or
/// `None`: a fault `atpkg doctor` lists ([`atpkg::doctor::is_recorded_problem`] —
/// `error:`, `aborted:`, `tombstoned:`, `unavailable:`, `blocked:` on a real program;
/// never a stray `-` row) other than the machine-wide no-build-for-this-architecture
/// verdict, which nobody on the machine can act on ([`atpkg::state::is_unserved_toolset`],
/// said by [`UNSERVED_HEADLINE`] instead); or a vendor build whose release channel has
/// not been reached for a day ([`VENDOR_UNREACHED`]). The row itself, verbatim, is on
/// the page beneath the headline.
pub(crate) fn attention_item(name: &str, state: &str) -> Option<String> {
    if atpkg::state::is_unserved_toolset(state) {
        return None;
    }
    let display = package_program_display_name(name);
    if atpkg::doctor::is_recorded_problem(name, state) {
        let what = match state.split_once(':').map_or("", |(head, _)| head) {
            "aborted" => "aborted",
            "tombstoned" => "disabled",
            "unavailable" | "blocked" => "unavailable",
            // A refusal (a digest or signature that did not check out) is named as one:
            // "failed" read as a flaky network (Phase 4).
            _ if atpkg::state::not_current(state).is_some_and(|(r, _)| r == "refused") => "refused",
            _ => "failed",
        };
        return Some(format!("{display} {what}"));
    }
    let (_, why) = state
        .strip_prefix(atpkg::state::MANAGED_PREFIX)?
        .split_once(" \u{2014} ")?;
    let (vendor, since) = why.split_once(VENDOR_UNREACHED)?;
    Some(format!("{vendor} not reached since {since}"))
}

/// Everything the Packages badge counts: each recorded row that needs attention
/// ([`attention_item`]), in the page's order — none on a DECLINED store, whose rows
/// describe a toolset the user removed on purpose (`atpkg doctor` lists nothing there
/// either) — then the remembered pass trouble ([`PackagesService::note_pass_trouble`]).
/// Empty ⇒ no badge. The badge clears when the condition does: a later pass rewrites
/// the row, a clean or later-completed pass forgets the trouble.
pub(crate) fn attention_items(
    programs: &[PackagesProgramRow],
    declined: bool,
    pass_trouble: Option<&PassTrouble>,
) -> Vec<String> {
    programs
        .iter()
        .filter(|_| !declined)
        .filter_map(|row| attention_item(&row.name, &row.state))
        .chain(pass_trouble.map(PassTrouble::item))
        .collect()
}

/// The longest cause a headline item quotes, in characters.
const HEADLINE_CAUSE_CHARS: usize = 48;

/// The prefix every attention headline opens with ([`attention_headline`]). It names
/// no command's result, which is how the conformance projection tells it from one
/// ([`presented_result_of`]).
pub(crate) const ATTENTION_PREFIX: &str = "Needs attention: ";

/// The headline that names what failed (`Needs attention: codex failed · …`),
/// at most [`ATTENTION_NAMED`] items and a count for the rest; `None` for none.
/// The badge's accessible value reads the same words.
pub(crate) fn attention_headline(items: &[String]) -> Option<String> {
    if items.is_empty() {
        return None;
    }
    let mut named = items
        .iter()
        .take(ATTENTION_NAMED)
        .cloned()
        .collect::<Vec<_>>()
        .join(" \u{b7} ");
    if items.len() > ATTENTION_NAMED {
        named.push_str(&format!(
            " \u{b7} and {} more",
            items.len() - ATTENTION_NAMED
        ));
    }
    Some(format!("{ATTENTION_PREFIX}{named}"))
}

/// The headline for a machine the signed index serves nothing for
/// ([`atpkg::state::is_unserved_toolset`]): a fact, stated without the badge.
pub(crate) const UNSERVED_HEADLINE: &str = "ALab tools aren\u{2019}t available for this Mac yet";

/// The headline while a `[packages] prefix` this build ignores stops unattended installs
/// (`atpkg::config::PackagesConfig::installs_unattended`); the detail carries atpkg's own
/// sentence, naming the prefix and the fix. Presents no command result.
pub(crate) const IGNORED_PREFIX_HEADLINE: &str =
    "Automatic installs are paused — [packages] prefix is set";

/// Which command result a Packages headline PRESENTS, as the `NativePackagesWorker`
/// model counts it (`presented_result`: 0 none, 1 success, 2 failure): an attention
/// headline presents none, whatever words it ends on.
#[cfg(test)]
pub(crate) fn presented_result_of(headline: &str) -> u8 {
    if headline.starts_with(ATTENTION_PREFIX) {
        0
    } else if headline.ends_with("completed") {
        1
    } else if headline.ends_with("failed") {
        2
    } else {
        0
    }
}

/// What kind of trouble a background pass left the Packages badge
/// ([`PackagesService::note_pass_trouble`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PassTroubleKind {
    /// A failure marker, a refused whole pass, a child that died mid-pass.
    Failed,
    /// The window STOOD DOWN for this launch behind a pass that never finished, and
    /// will not retry; the cause names the command to run.
    StoodDown,
}

impl PassTroubleKind {
    /// The badge's item for it ([`attention_items`]).
    fn item(self) -> &'static str {
        match self {
            Self::Failed => "the last package check failed",
            Self::StoodDown => "the package check didn\u{2019}t run",
        }
    }
}

/// A background pass's trouble the Packages badge remembers: what kind, atpkg's own
/// cause, and when the window learned it (Unix seconds) — a record whose
/// `last_success_at` is later proves a pass completed since ([`PackagesService::finish`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PassTrouble {
    pub(crate) kind: PassTroubleKind,
    pub(crate) cause: String,
    pub(crate) noted_unix: i64,
}

impl PassTrouble {
    /// The badge's item, NAMING THE CAUSE (Phase 4): `the last package check failed: prefix
    /// is not writable` — atpkg's own words, its `atpkg: ` prefix dropped, the first clause
    /// only and at most [`HEADLINE_CAUSE_CHARS`]; the whole cause leads the detail.
    fn item(&self) -> String {
        let cause = self.cause.trim();
        let cause = cause.strip_prefix("atpkg: ").unwrap_or(cause);
        let clause = cause
            .split([';', '\u{2014}'])
            .next()
            .unwrap_or(cause)
            .trim()
            .trim_end_matches(['.', ',', ':']);
        let mut short: String = clause.chars().take(HEADLINE_CAUSE_CHARS).collect();
        if clause.chars().count() > HEADLINE_CAUSE_CHARS {
            short = short
                .rsplit_once(' ')
                .map_or(short.clone(), |(head, _)| head.to_string());
            short.push('\u{2026}');
        }
        if short.is_empty() {
            self.kind.item().to_string()
        } else {
            format!("{}: {short}", self.kind.item())
        }
    }
}

/// How many failures the headline names before it counts the rest.
const ATTENTION_NAMED: usize = 3;

/// Facts about the co-located package manager, collected entirely OFF the
/// event loop by one worker pass. Bounded and owned, suitable for a typed
/// event-loop wake.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PackagesStatusReport {
    /// The co-located `atpkg` binary exists beside our executable. Release
    /// bundles ship it in Contents/MacOS; a WORKSPACE dev build has
    /// `target/debug/atpkg` beside the dev binary too (atpkg is a workspace
    /// bin), so dev runs exercise the same co-location seam — inert without a
    /// pinned root key like any unpinned build. Only a truly solitary binary
    /// (e.g. a copied-out executable) has none.
    pub(crate) available: bool,
    /// The co-located CLI's OWN posture, mirrored: the compiled root anchor is
    /// present. False ⇒ inert by construction (an unpinned build — the only cause
    /// left: the `ATPKG_DISABLE` kill switch is gone, 2026-09-23).
    pub(crate) manager_enabled: bool,
    /// The pinned root key's operator-facing fingerprint (the doctor line).
    pub(crate) root_fingerprint: String,
    /// `status.toml` existed and parsed (atpkg has run at least once).
    pub(crate) recorded: bool,
    /// `status.toml` fields (empty when `recorded` is false).
    pub(crate) updated_at: String,
    pub(crate) outcome: String,
    pub(crate) index_source: String,
    pub(crate) programs: Vec<PackagesProgramRow>,
    /// Bounded, operator-readable diagnostic when some private package
    /// metadata could not be admitted or parsed. A worker still publishes the
    /// rest of the snapshot, so Settings never remains permanently “Reading”.
    pub(crate) collection_error: Option<String>,
    /// The store carries atpkg's DECLINED marker (`aterm pkg uninstall --all`): its
    /// fault rows describe a toolset removed on purpose, and count for no badge.
    pub(crate) declined: bool,
    /// `status.toml`'s `last_success_at` (RFC 3339, empty when never): the last pass
    /// that resolved the index and ran to its end, whoever ran it.
    pub(crate) last_success_at: String,
    /// The package log's newest events, newest first (at most
    /// [`atpkg::packages_log::ACTIVITY_EVENTS`]) — Settings ▸ Packages' Activity.
    pub(crate) activity: Vec<atpkg::packages_log::Entry>,
    /// Where the package log is (`<logs dir>/packages.log`), when a log directory resolves:
    /// what "Open Log" opens.
    pub(crate) log_path: Option<std::path::PathBuf>,
    /// The local clock, read on the worker at each collection ([`LocalClock::read`]): every
    /// time the page shows is relative and local, never raw UTC.
    pub(crate) clock: LocalClock,
    /// The sentence for a `[packages] prefix` this build does not use
    /// (`atpkg::config::PackagesConfig::ignored_prefix`, in
    /// `atpkg::config::ignored_prefix_note`'s words): while it stands, atpkg installs
    /// nothing unattended, and this page is where a person learns why.
    pub(crate) ignored_prefix: Option<String>,
}

impl PackagesStatusReport {
    /// The honest zero state before any worker observation: nothing is claimed
    /// available or enabled until a collection pass has actually looked.
    fn unobserved() -> Self {
        Self {
            activity: Vec::new(),
            log_path: None,
            clock: LocalClock::UNKNOWN,
            available: false,
            manager_enabled: false,
            root_fingerprint: String::new(),
            recorded: false,
            updated_at: String::new(),
            outcome: String::new(),
            index_source: String::new(),
            programs: Vec::new(),
            collection_error: None,
            declined: false,
            last_success_at: String::new(),
            ignored_prefix: None,
        }
    }

    /// Pure assembly from atpkg's own record types — the testable core of
    /// [`collect_packages_status`]. `links` maps program → dev-link target.
    pub(crate) fn from_parts(
        available: bool,
        manager_enabled: bool,
        root_fingerprint: String,
        status: Option<&atpkg::Status>,
        links: &[(String, Option<std::path::PathBuf>)],
    ) -> Self {
        let mut programs: Vec<PackagesProgramRow> = status
            .map(|status| {
                status
                    .programs
                    .iter()
                    .map(|(name, program)| {
                        PackagesProgramRow::from_status(name, program, status.last_index_build)
                    })
                    .collect()
            })
            .unwrap_or_default();
        // The Needs-admin group is a property of the WHOLE row set: Homebrew's row
        // reads `blocked by clt: needs admin — …`, and it waits on the same door.
        let admin_rows: Vec<(String, String)> = programs
            .iter()
            .map(|row| (row.name.clone(), row.state.clone()))
            .collect();
        for name in needs_admin_order(&admin_rows) {
            if let Some(row) = programs.iter_mut().find(|row| row.name == name) {
                row.group = RowGroup::NeedsAdmin;
            }
        }
        // Dev-linked programs are managed OUTSIDE the registry (update
        // hard-skips them); surface them in the same list, annotated, so the
        // page never claims a linked tool is registry-current.
        for (name, target) in links {
            let annotation = Some(match target {
                Some(path) => {
                    let mut label = String::from("dev-link → ");
                    label.push_str(&path.display().to_string());
                    label
                }
                None => "dev-link".to_string(),
            });
            if let Some(row) = programs.iter_mut().find(|row| row.name == *name) {
                row.annotation = annotation;
            } else {
                programs.push(PackagesProgramRow {
                    name: name.clone(),
                    installed_build: None,
                    state: "linked".to_string(),
                    kind: ProgramStateKind::Other,
                    group: RowGroup::Default,
                    facts: None,
                    annotation,
                    update: ProgramUpdate::default(),
                    words: None,
                });
            }
        }
        // A rustup seam the last pass REFUSED to re-assert (`refused:rustup:<name>: <why>`
        // in the record's `seams`, written by `atpkg::seam::reassert`) is a row of its
        // own: the toolchain is installed, and `cargo +trust` still runs something else
        // — the record carried it, `doctor` printed it, and this page never read it
        // (audit 2026-09-14).
        if let Some(status) = status {
            for key in &status.seams {
                let Some(rest) = key.strip_prefix("refused:rustup:") else {
                    continue;
                };
                let (name, why) = rest.split_once(": ").unwrap_or((rest, ""));
                programs.push(PackagesProgramRow {
                    name: format!("rustup:{name}"),
                    installed_build: None,
                    state: if why.is_empty() {
                        "rustup seam refused".to_string()
                    } else {
                        format!("rustup seam refused: {why}")
                    },
                    kind: ProgramStateKind::Other,
                    group: RowGroup::Default,
                    facts: None,
                    annotation: Some("cargo +trust does not run the managed toolchain".to_string()),
                    update: ProgramUpdate::default(),
                    words: None,
                });
            }
        }
        programs.sort_by(|a, b| a.name.cmp(&b.name));
        Self {
            available,
            manager_enabled,
            root_fingerprint,
            recorded: status.is_some(),
            updated_at: status.map(|s| s.updated_at.clone()).unwrap_or_default(),
            outcome: status.map(|s| s.outcome.clone()).unwrap_or_default(),
            index_source: status.map(|s| s.index_source.clone()).unwrap_or_default(),
            programs,
            collection_error: None,
            declined: false,
            last_success_at: status
                .map(|s| s.last_success_at.clone())
                .unwrap_or_default(),
            activity: Vec::new(),
            log_path: None,
            clock: LocalClock::UNKNOWN,
            ignored_prefix: None,
        }
    }

    /// What the STORE says about each installed row (Phase 4), read on the worker: when
    /// its build went live — a vendor build's verification (its `.vendor` record, which
    /// also names its version), else the moment its `current` link was flipped — and the
    /// latest build known — a vendor program's head as its stamp last saw it (and when),
    /// an ALab program's pin in the last verified index (`pins`,
    /// [`atpkg::cli::latest_known_pins`]).
    pub(crate) fn attach_store_facts(
        &mut self,
        layout: &atpkg::Layout,
        pins: Option<&std::collections::BTreeMap<String, u64>>,
    ) {
        for row in &mut self.programs {
            let Some(build) = row.installed_build else {
                continue;
            };
            if atpkg::vendor_direct::is_vendor(&row.name) {
                if let Some(record) =
                    atpkg::vendor_direct::complete_record(&layout.build_dir(&row.name, build))
                {
                    row.update.version = Some(record.version.to_string());
                    row.update.updated_at = Some(record.verified_at);
                }
                if let Some(stamp) = atpkg::vendor_direct::ProgramStamp::read(layout, &row.name) {
                    if let Some(head) = stamp.last_head {
                        row.update.latest_known = Some(head.to_string());
                    }
                    row.update.checked_at =
                        (stamp.last_checked_at > 0).then_some(stamp.last_checked_at);
                }
            } else if let Some(pin) = pins.and_then(|p| p.get(&row.name)) {
                row.update.latest_known = Some(atpkg::vendor_direct::build_words(*pin));
            }
            if row.update.updated_at.is_none() {
                row.update.updated_at =
                    std::fs::symlink_metadata(layout.program_current(&row.name))
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .and_then(|d| i64::try_from(d.as_secs()).ok());
            }
        }
    }
}

impl PackagesStatusReport {
    /// Regroup rows whose names carry an opt-in marker as extras (an installed extra
    /// otherwise reads exactly like a default-set member). A needs-admin row keeps its
    /// group: the door outranks the roster.
    pub(crate) fn mark_extras<S: AsRef<str>>(&mut self, optins: impl IntoIterator<Item = S>) {
        for name in optins {
            if let Some(row) = self
                .programs
                .iter_mut()
                .find(|row| row.name == name.as_ref() && row.group == RowGroup::Default)
            {
                row.group = RowGroup::Extras;
                if row.facts.is_none() {
                    row.facts = atpkg::stub::describe(&row.name).map(ExtraFacts::parse);
                }
            }
        }
    }

    /// The needs-admin names in door order (see [`needs_admin_order`]).
    pub(crate) fn needs_admin(&self) -> Vec<String> {
        let rows: Vec<(String, String)> = self
            .programs
            .iter()
            .map(|row| (row.name.clone(), row.state.clone()))
            .collect();
        needs_admin_order(&rows)
    }
}

const MAX_PACKAGE_COLLECTION_ERRORS: usize = 4;

fn collection_error_summary(errors: Vec<String>, total: usize) -> Option<String> {
    if errors.is_empty() {
        return None;
    }
    let mut summary = errors.join(" · ");
    if total > errors.len() {
        summary.push_str(" · … and ");
        summary.push_str(&(total - errors.len()).to_string());
        summary.push_str(" more metadata errors");
    }
    Some(summary)
}

/// Collect the packages report from the real machine — filesystem reads, no network, and
/// one subprocess kind: `date`, for the local clock ([`LocalClock::read`]). MUST run off
/// the event loop (worker threads only): it stats the co-located binary, parses
/// `status.toml` and runs `date`.
pub(crate) fn collect_packages_status(available: bool) -> PackagesStatusReport {
    // The CONFIGURED prefix, not the default: this page is what every seed notice
    // points at, and reading the default store made a relocated lab store report
    // "No package activity yet" forever (2026-08-20 round-8 audit).
    // ONE read of the table serves both the layout and the ignored-prefix line.
    let config = atpkg::config::load();
    let layout = atpkg::store::resolve_from(&config);
    let login_path = std::ffi::OsString::from(crate::spawn::atpkg_child_path_once());
    let mut report =
        collect_packages_status_from_layout(available, layout.as_ref(), Some(&login_path));
    report.ignored_prefix = config
        .ignored_prefix
        .as_deref()
        .map(atpkg::config::ignored_prefix_note);
    // The package log's tail (Phase 4) — atpkg's own file, beside `aterm.log` — and the
    // local clock every time on the page is shown in.
    report.log_path = atpkg::packages_log::log_path();
    report.activity = atpkg::packages_log::tail(atpkg::packages_log::ACTIVITY_EVENTS);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    report.clock = LocalClock::read(now);
    report
}

/// The page's report over `layout`, its SHADOWED rows computed for `path_var` — the login
/// shell's PATH, which is what the user's terminals run with. atpkg records no SHADOWED
/// row (Phase 3: it is per-shell truth), so the page reads it the way `which` and
/// `doctor` do ([`atpkg::status::with_shadows`]); a row an older atpkg recorded as
/// SHADOWED still parses as one.
fn collect_packages_status_from_layout(
    available: bool,
    layout: Option<&atpkg::Layout>,
    path_var: Option<&std::ffi::OsStr>,
) -> PackagesStatusReport {
    let mut errors = Vec::new();
    let mut error_count = 0usize;
    let mut record_error = |message: String| {
        error_count = error_count.saturating_add(1);
        if errors.len() < MAX_PACKAGE_COLLECTION_ERRORS {
            errors.push(message);
        }
    };
    let status = layout.and_then(|layout| match atpkg::status::read_checked(layout) {
        Ok(status) => status.map(|s| atpkg::status::with_shadows(layout, s, path_var)),
        Err(error) => {
            record_error(format!("Could not read status.toml: {error}"));
            None
        }
    });
    let links: Vec<(String, Option<std::path::PathBuf>)> = layout
        .map(|layout| match atpkg::linked_programs_checked(layout) {
            Ok(names) => names
                .into_iter()
                .filter_map(|name| match atpkg::linked_checkout_checked(layout, &name) {
                    Ok(Some(target)) => Some((name, Some(target))),
                    Ok(None) => {
                        record_error(format!("Link marker {name:?} disappeared while reading"));
                        None
                    }
                    Err(error) => {
                        record_error(format!("Could not read link marker {name:?}: {error}"));
                        None
                    }
                })
                .collect(),
            Err(error) => {
                record_error(format!("Could not enumerate package links: {error}"));
                Vec::new()
            }
        })
        .unwrap_or_default();
    // Mirror the CO-LOCATED CLI's own posture: the compiled root anchor, and nothing
    // else — no root-key override and no environment kill switch, so the pin's
    // fingerprint is always the live one and an inert manager is an unpinned build.
    let manager_enabled = atpkg::manager_enabled();
    let mut report = PackagesStatusReport::from_parts(
        available,
        manager_enabled,
        atpkg::root_key_fingerprint(),
        status.as_ref(),
        &links,
    );
    // An extra the user opted in to and that is now installed reads `managed …`, the
    // same words as a default-set member; the opt-in marker is what still says it is
    // an extra. Read here (worker thread), applied to the grouping only.
    if let Some(layout) = layout {
        report.mark_extras(layout.optins());
        report.declined = layout.declined().is_file();
        // What the store says about each row: when it went live, the latest build known
        // (the config re-read: a window lives for days).
        let pins = atpkg::cli::latest_known_pins(layout, &atpkg::config::load());
        report.attach_store_facts(layout, pins.as_ref().map(|(_, pins)| pins));
    }
    report.collection_error = collection_error_summary(errors, error_count);
    report
}

/// Host-owned reducer for the packages surface: one worker at a time, stale
/// completions inert, every observable change bumps `revision` for fan-out.
/// Construction is filesystem-free (the updater-service doctrine): nothing is
/// claimed until a worker reports.
pub(crate) struct PackagesService {
    revision: u64,
    /// Worker generation: a completion must present the exact sequence its
    /// `begin` minted, so a superseded worker cannot import stale facts.
    sequence: u64,
    inflight: bool,
    busy: Option<PackagesBusy>,
    report: Option<PackagesStatusReport>,
    last_command: Option<PackagesCommandOutcome>,
    /// What bare `atpkg machine` last measured, plus this launch's memory of what
    /// changed. Rides the same revision fan-out as the rest of the surface.
    machine: MachinePosture,
    /// The last background toolchain pass's TROUBLE — a failure marker, a whole
    /// pass refused before it could write a record, a stand-down that will not
    /// retry — held until a pass is known to have completed since
    /// ([`Self::note_pass_trouble`], cleared by [`Self::note_pass_clean`] and
    /// [`Self::finish`]). One half of the Packages badge; the recorded rows are the
    /// other ([`attention_items`]).
    pass_trouble: Option<PassTrouble>,
}

/// The `[machine]` host settings as the window CONFIRMED them: the record bare
/// `atpkg machine` printed (`machine-state:`), read by the host's machine worker —
/// never re-derived in-process from `defaults` or a home walk — plus this launch's
/// memory of the last `machine-settings:` change and the last apply verdict.
/// Nothing is durable: a fresh launch starts unobserved and reads again.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct MachinePosture {
    /// A read has completed at least once (a parsed state OR a read error).
    pub(crate) observed: bool,
    /// The parsed record, when the last read produced one.
    pub(crate) state: Option<atpkg::machine::MachineState>,
    /// Why the last read produced no record (the child could not be spawned, or
    /// printed no `machine-state:` line — which is never "nothing to apply").
    pub(crate) read_error: Option<String>,
    /// The last `machine-settings:` body this process saw and when it arrived.
    pub(crate) last_change: Option<(String, std::time::SystemTime)>,
    /// The last `atpkg machine apply` verdict sentence this process saw.
    pub(crate) last_verdict: Option<String>,
    /// The machine read worker is running.
    pub(crate) refreshing: bool,
    /// A read was asked for while one was running: the running read may have
    /// started before an apply and would land as a pre-apply record, so it is
    /// followed by another read rather than joined.
    pub(crate) rerun: bool,
    /// A pass printed a `machine-settings:` change and its own `machine-state:`
    /// record is expected to follow on the same stream (2026-09-16): the card reads
    /// as refreshing meanwhile, and NO read is spawned — the pass measured the
    /// machine already. Cleared by the record, or by the stream ending without one
    /// ([`Self::take_record_expectation`] then spawns the read as the fallback).
    pub(crate) awaiting_record: bool,
    /// A worker read is running and a NEWER record — a pass's own, measured after
    /// its apply — landed while it ran. The worker's answer is older than what the
    /// card shows and is dropped when it arrives, so a read that started before an
    /// apply can never overwrite the state the apply reported.
    pub(crate) superseded: bool,
    /// Reads started so far — bumped when one is spawned (a request admitted, or a
    /// queued rerun handed back to be spawned). One read is in flight at a time, so
    /// the read that completes is the one this counted last.
    pub(crate) reads_started: u64,
    /// `reads_started` when the open expectation was opened: a read that STARTED
    /// before the change cannot answer for it (it measured the machine the change
    /// then changed), so only a read counted after this closes the expectation.
    pub(crate) expectation_read_gen: u64,
}

/// Scalar projection used only by Tier-1 conformance. Every field is read from
/// the genuine reducer; `presented_result` additionally reads the shipping
/// projection headline so the formal trace is bound to what Settings renders.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PackagesModelState {
    pub(crate) sequence: u64,
    pub(crate) inflight: bool,
    pub(crate) operation: u8,
    pub(crate) observed: bool,
    pub(crate) last_operation: u8,
    pub(crate) last_result: u8,
    pub(crate) presented_result: u8,
}

impl PackagesService {
    pub(crate) fn new() -> Self {
        Self {
            revision: 1,
            sequence: 0,
            inflight: false,
            busy: None,
            report: None,
            last_command: None,
            machine: MachinePosture::default(),
            pass_trouble: None,
        }
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn busy(&self) -> Option<PackagesBusy> {
        self.busy
    }

    /// The machine posture as last reduced. Test-only: the host reaches the
    /// posture through [`Self::request_machine_read`] / [`Self::replace_machine_state`]
    /// and the projection, never by inspecting it.
    #[cfg(test)]
    pub(crate) fn machine(&self) -> &MachinePosture {
        &self.machine
    }

    /// Mark a read as running without asking for one — test-only, to stage the
    /// "read in flight" posture; the host uses [`Self::request_machine_read`].
    #[cfg(test)]
    pub(crate) fn set_machine_refreshing(&mut self, refreshing: bool) {
        if self.machine.refreshing == refreshing {
            return;
        }
        self.machine.refreshing = refreshing;
        self.revision = self.revision.saturating_add(1);
    }

    /// Ask for a machine read. `true` ⇒ the caller spawns one (the posture is now
    /// refreshing); `false` ⇒ one is already running and a rerun is queued behind
    /// it — never joined, because the running read may predate an apply and would
    /// land as the final "observed" record.
    pub(crate) fn request_machine_read(&mut self) -> bool {
        if self.machine.refreshing {
            self.machine.rerun = true;
            return false;
        }
        self.machine.refreshing = true;
        self.machine.reads_started = self.machine.reads_started.saturating_add(1);
        self.revision = self.revision.saturating_add(1);
        true
    }

    /// Reduce one machine read: a parsed `machine-state:` record, or the reason
    /// there was none. Either way the posture is now OBSERVED — a read error is a
    /// fact the card states, not a reason to keep saying "reading". Returns `true`
    /// when a rerun was queued while this read ran: the posture then STAYS
    /// refreshing (Apply disabled, the card still "Reading…") and the caller must
    /// spawn the already-admitted read directly, without requesting admission again.
    #[must_use = "a queued rerun must be spawned by the caller"]
    pub(crate) fn replace_machine_state(
        &mut self,
        result: Result<atpkg::machine::MachineState, String>,
    ) -> bool {
        self.machine.observed = true;
        let rerun = std::mem::take(&mut self.machine.rerun);
        self.machine.refreshing = rerun;
        // A completed read answers an open expectation — when it STARTED after the
        // change that opened it. One that was already running measured the machine
        // the change then changed, and must not stand under "Last change" as if it
        // confirmed it; the expectation stays for the record, the fallback, or the
        // next read (2026-09-16 review).
        let completed_gen = self.machine.reads_started;
        if self.machine.awaiting_record && completed_gen > self.machine.expectation_read_gen {
            self.machine.awaiting_record = false;
        }
        if rerun {
            // The caller spawns the queued read now: count it as started.
            self.machine.reads_started = self.machine.reads_started.saturating_add(1);
        }
        // Superseded: a pass's own record landed while this read ran, so the card
        // already shows a newer state than this read measured. The answer is dropped
        // — a read error included, since the newer record is not in error.
        if std::mem::take(&mut self.machine.superseded) {
            self.revision = self.revision.saturating_add(1);
            return rerun;
        }
        match result {
            Ok(state) => {
                self.machine.state = Some(state);
                self.machine.read_error = None;
            }
            Err(error) => {
                // Keep the last good record beside the error: a transient read
                // failure must not erase what was measured a moment ago.
                self.machine.read_error = Some(error);
            }
        }
        self.revision = self.revision.saturating_add(1);
        rerun
    }

    /// Remember a `machine-settings:` change (the pull-down row's body) and when
    /// this process saw it.
    pub(crate) fn note_machine_change(&mut self, body: String, at: std::time::SystemTime) {
        self.machine.last_change = Some((body, at));
        self.revision = self.revision.saturating_add(1);
    }

    /// A pass printed a change; its own `machine-state:` record follows on the same
    /// stream. The card reads as refreshing (Apply disabled) until it lands, and no
    /// read is spawned for it.
    pub(crate) fn expect_machine_record(&mut self) {
        if self.machine.awaiting_record {
            return;
        }
        self.machine.awaiting_record = true;
        self.machine.expectation_read_gen = self.machine.reads_started;
        self.revision = self.revision.saturating_add(1);
    }

    /// Reduce a record a PASS printed after its apply: the newest measurement there
    /// is. It answers the expectation a change opened, satisfies any rerun queued
    /// meanwhile (the rerun asked for a state newer than a running read; this is
    /// one), and marks a read still running as superseded so its older answer is
    /// dropped on arrival.
    pub(crate) fn note_machine_record(&mut self, state: atpkg::machine::MachineState) {
        self.machine.observed = true;
        self.machine.state = Some(state);
        self.machine.read_error = None;
        self.machine.awaiting_record = false;
        self.machine.rerun = false;
        if self.machine.refreshing {
            self.machine.superseded = true;
        }
        self.revision = self.revision.saturating_add(1);
    }

    /// The pass's stream ended (or its record did not parse) while a record was
    /// still expected: `true` ⇒ the caller must spawn the read after all — the
    /// fallback that keeps the card from sitting under a change it predates.
    #[must_use = "an open expectation must be answered by a spawned read"]
    pub(crate) fn take_record_expectation(&mut self) -> bool {
        if !self.machine.awaiting_record {
            return false;
        }
        self.machine.awaiting_record = false;
        self.revision = self.revision.saturating_add(1);
        true
    }

    /// A background toolchain pass left TROUBLE of `kind` with atpkg's `cause`,
    /// learned at `noted` (2026-09-22: its only surface is the Packages badge and
    /// headline, never a status-bar row). `true` when the badge's words changed.
    pub(crate) fn note_pass_trouble(
        &mut self,
        kind: PassTroubleKind,
        cause: String,
        noted: std::time::SystemTime,
    ) -> bool {
        if self
            .pass_trouble
            .as_ref()
            .is_some_and(|t| t.kind == kind && t.cause == cause)
        {
            return false;
        }
        let noted_unix = noted
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
        self.pass_trouble = Some(PassTrouble {
            kind,
            cause,
            noted_unix,
        });
        self.revision = self.revision.saturating_add(1);
        true
    }

    /// A later whole pass ran clean: the condition the remembered trouble named
    /// is gone, and so is that half of the badge. `true` when one was cleared.
    pub(crate) fn note_pass_clean(&mut self) -> bool {
        if self.pass_trouble.take().is_none() {
            return false;
        }
        self.revision = self.revision.saturating_add(1);
        true
    }

    /// Whether `completion` proves a pass COMPLETED after the remembered trouble:
    /// a whole-pass verb from Settings (Check, Install) that succeeded, or a record
    /// whose `last_success_at` — stamped by whoever ran the pass, a terminal's
    /// `aterm pkg update` included — is later than the moment the trouble was noted.
    fn completion_clears_trouble(&self, completion: &PackagesWorkerCompletion) -> bool {
        let Some(trouble) = self.pass_trouble.as_ref() else {
            return false;
        };
        if matches!(
            completion.command,
            Some(PackagesCommandOutcome::Succeeded {
                operation: PackagesBusy::Check | PackagesBusy::Install,
            })
        ) {
            return true;
        }
        aterm_update_core::pkg_check::rfc3339_to_unix(&completion.report.last_success_at)
            .is_some_and(|success| success > trouble.noted_unix)
    }

    /// Remember that a pass REFUSED to apply the settings, or FAILED to.
    ///
    /// The launch lanes stream a pass's stdout through the marker reader, which until
    /// now kept only the change marker — so a launch that printed
    /// `machine settings not applied — …` or `machine settings failed — …` left the
    /// card showing the last successful record with nothing to say that the most recent
    /// attempt had come to nothing. This is the same slot an explicit Apply now writes,
    /// so the card has one place to look and the newest answer wins.
    pub(crate) fn note_machine_verdict(&mut self, verdict: String) {
        self.machine.last_verdict = Some(verdict);
        self.revision = self.revision.saturating_add(1);
    }

    #[cfg(test)]
    pub(crate) fn model_state(&self) -> PackagesModelState {
        let operation = match (self.inflight, self.busy) {
            (false, _) => 0,
            (true, None) => 1,
            (true, Some(PackagesBusy::Check)) => 2,
            (true, Some(PackagesBusy::Install)) => 3,
            (true, Some(PackagesBusy::Uninstall)) => 4,
            (true, Some(PackagesBusy::InstallExtra)) => 5,
            (true, Some(PackagesBusy::InstallAdmin)) => 6,
            (true, Some(PackagesBusy::MachineApply)) => 7,
        };
        let (last_operation, last_result) = match self.last_command.as_ref() {
            None => (0, 0),
            Some(PackagesCommandOutcome::Succeeded { operation }) => (
                match operation {
                    PackagesBusy::Check => 2,
                    PackagesBusy::Install => 3,
                    PackagesBusy::Uninstall => 4,
                    PackagesBusy::InstallExtra => 5,
                    PackagesBusy::InstallAdmin => 6,
                    PackagesBusy::MachineApply => 7,
                },
                1,
            ),
            Some(PackagesCommandOutcome::Failed { operation, .. }) => (
                match operation {
                    PackagesBusy::Check => 2,
                    PackagesBusy::Install => 3,
                    PackagesBusy::Uninstall => 4,
                    PackagesBusy::InstallExtra => 5,
                    PackagesBusy::InstallAdmin => 6,
                    PackagesBusy::MachineApply => 7,
                },
                2,
            ),
        };
        let headline = self.state(true, true, false).projection().headline;
        let presented_result = presented_result_of(&headline);
        PackagesModelState {
            sequence: self.sequence,
            inflight: self.inflight,
            operation,
            observed: self.report.is_some(),
            last_operation,
            last_result,
            presented_result,
        }
    }

    /// Begin one worker pass (`busy = None` ⇒ a silent status refresh).
    /// Returns the minted sequence, or `None` while a pass is already running —
    /// callers treat that as "join" (the running pass ends with a fresh
    /// collection anyway).
    pub(crate) fn begin(&mut self, busy: Option<PackagesBusy>) -> Option<u64> {
        if self.inflight {
            return None;
        }
        self.sequence = self.sequence.saturating_add(1);
        self.inflight = true;
        self.busy = busy;
        if busy.is_some() {
            // The in-flight headline is now authoritative. Never leave the
            // previous verb's result attached to a new attempt.
            self.last_command = None;
        }
        self.revision = self.revision.saturating_add(1);
        Some(self.sequence)
    }

    /// Reduce one worker completion. `false` ⇒ stale (reducer-inert means
    /// presentation-inert too: the caller must not publish). A current but
    /// protocol-mismatched result is inert as well: a refresh cannot settle a
    /// verb, and a command result cannot settle a refresh reservation.
    pub(crate) fn finish(&mut self, sequence: u64, completion: PackagesWorkerCompletion) -> bool {
        if !self.inflight || sequence != self.sequence {
            return false;
        }
        let command_operation = completion
            .command
            .as_ref()
            .map(PackagesCommandOutcome::operation);
        if self.busy != command_operation {
            return false;
        }
        self.inflight = false;
        self.busy = None;
        if self.completion_clears_trouble(&completion) {
            self.pass_trouble = None;
        }
        self.report = Some(completion.report);
        if completion.command.is_some() {
            self.last_command = completion.command;
        }
        if let Some(state) = completion.machine_state {
            // The apply's own record: the newest measurement, as a streamed pass's
            // record is — it closes the expectation its change line opened.
            self.note_machine_record(state);
        }
        if let Some(verdict) = completion.machine_verdict {
            // A FAILED APPLY IS NOT REMEMBERED AS A SUCCESS. The child's verdict is
            // whatever it printed before it fell over — for a pass whose Spotlight half
            // landed and whose Universal Control write did not, that is literally
            // `applied — spotlight-noindex 1 dir(s) migrated`. Rendered alone beside a
            // failure message it reads as a contradiction; carried WITH it, it is the
            // useful half of the truth.
            self.machine.last_verdict = Some(match self.last_command.as_ref() {
                Some(PackagesCommandOutcome::Failed { operation, message })
                    if operation.applies_machine_settings() =>
                {
                    format!("failed — {message}; earlier in the pass: {verdict}")
                }
                _ => verdict,
            });
        }
        self.revision = self.revision.saturating_add(1);
        true
    }

    /// Release a reservation whose worker never started (thread-spawn failure):
    /// clears inflight/busy WITHOUT storing a report, so a previously-observed
    /// snapshot keeps its real facts and a never-observed surface honestly
    /// stays unobserved (no fabricated `manager_enabled`/fingerprint). `false`
    /// ⇒ stale.
    pub(crate) fn abort(&mut self, sequence: u64) -> bool {
        if !self.inflight || sequence != self.sequence {
            return false;
        }
        self.inflight = false;
        self.busy = None;
        self.revision = self.revision.saturating_add(1);
        true
    }

    /// The state a Settings controller renders, resolved against the CURRENT
    /// config consent flags (the config is host-owned; the service holds only
    /// worker facts).
    pub(crate) fn state(
        &self,
        master_enabled: bool,
        auto_install: bool,
        loop_running: bool,
    ) -> PackagesState {
        PackagesState {
            observed: self.report.is_some(),
            report: self
                .report
                .clone()
                .unwrap_or_else(PackagesStatusReport::unobserved),
            busy: self.busy,
            inflight: self.inflight,
            last_command: self.last_command.clone(),
            master_enabled,
            auto_install,
            loop_running,
            machine: self.machine.clone(),
            saved_universal_control: atpkg::config::UniversalControlPolicy::Off,
            saved_spotlight_noindex: true,
            pass_trouble: self.pass_trouble.clone(),
            retired_switch_note: None,
        }
    }
}

/// The snapshot a [`crate::native_settings::SettingsApp`] holds (the
/// [`crate::update_screen::UpdateState`] analogue). Pure data; `projection()`
/// derives the presentation strings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PackagesState {
    /// A worker pass has completed at least once (before that the page says
    /// "Reading package status…" instead of claiming anything).
    observed: bool,
    report: PackagesStatusReport,
    busy: Option<PackagesBusy>,
    /// ANY worker is inflight — including the silent status refresh, whose
    /// `busy` label is `None`. Actions gate on THIS (not on `busy`): a click
    /// admitted during a silent refresh would only be dropped by the host's
    /// one-worker rule, so the buttons disable for the (brief) window instead.
    inflight: bool,
    /// Final result of the most recent UI-initiated verb. Silent refreshes keep
    /// it; starting a new verb clears it.
    last_command: Option<PackagesCommandOutcome>,
    /// Resolved `[packages]` flags (`enabled`, with the retired `auto_update` folded
    /// in, and `auto_install`) — display only; the switches edit the config keys.
    master_enabled: bool,
    auto_install: bool,
    /// Immutable fact: the background updater thread actually started for this
    /// process. What it RUNS follows the saved switches live (Phase 4: it re-reads
    /// `[packages]` before every pass), so only its absence needs saying.
    loop_running: bool,
    /// The `[machine]` host settings as confirmed by the machine read worker.
    machine: MachinePosture,
    /// The SAVED `[machine]` switches, resolved from the host's config — display
    /// only, so the card can say when a saved switch has not been read yet.
    saved_universal_control: atpkg::config::UniversalControlPolicy,
    saved_spotlight_noindex: bool,
    /// The last background pass's trouble, until a pass completes since
    /// ([`PackagesService::note_pass_trouble`]).
    pass_trouble: Option<PassTrouble>,
    /// atpkg's own sentence for a retired `[packages] auto_update = false` that is what
    /// holds Automatic updates off (`atpkg::config::auto_update_note`, the words doctor
    /// and the config editor say) — the WHY of an off [`Self::master_enabled`], never a
    /// second switch: the effective state is `master_enabled` alone.
    retired_switch_note: Option<String>,
}

impl PackagesState {
    /// Attach the host's saved `[machine]` switches (the config is host-owned; the
    /// service holds only worker facts).
    pub(crate) fn with_machine_config(
        mut self,
        universal_control: atpkg::config::UniversalControlPolicy,
        spotlight_noindex: bool,
    ) -> Self {
        self.saved_universal_control = universal_control;
        self.saved_spotlight_noindex = spotlight_noindex;
        self
    }

    /// Attach the host's word on a retired `[packages] auto_update = false` holding
    /// Automatic updates off (`Config::packages_retired_switch_note`) — config-owned, like
    /// the switches.
    pub(crate) fn with_retired_switch_note(mut self, note: Option<String>) -> Self {
        self.retired_switch_note = note;
        self
    }
    /// The pre-observation default a freshly-created Settings controller holds
    /// until the host publishes a real snapshot.
    pub(crate) fn unobserved() -> Self {
        Self {
            observed: false,
            report: PackagesStatusReport::unobserved(),
            busy: None,
            inflight: false,
            last_command: None,
            master_enabled: true,
            auto_install: true,
            loop_running: false,
            machine: MachinePosture::default(),
            saved_universal_control: atpkg::config::UniversalControlPolicy::Off,
            saved_spotlight_noindex: true,
            pass_trouble: None,
            retired_switch_note: None,
        }
    }

    /// Snapshot the exact state the Packages page paints.
    pub(crate) fn projection(&self) -> PackagesProjection {
        self.projection_at(std::time::SystemTime::now())
    }

    /// [`Self::projection`] with the clock injected — every time on the page is said
    /// relative to it, on the local clock (the "Last change … ago" line, each row's
    /// "Updated …", the Activity's times, the last full check), and tests pin them.
    pub(crate) fn projection_at(&self, now: std::time::SystemTime) -> PackagesProjection {
        let report = &self.report;
        let actions_enabled = self.observed
            && report.available
            && report.manager_enabled
            && report.collection_error.is_none()
            && !self.inflight;
        // WHAT FAILED, NAMED — the Settings rail's badge (2026-09-22: a failure's
        // one surface), and the headline whenever no verb's result holds it (a verb
        // in flight or finished, a manager that cannot act). A finished verb keeps
        // its own headline — the model's `FinalResultIsPresented` — and the line
        // rides its detail instead.
        let attention = attention_items(
            &report.programs,
            report.declined,
            self.pass_trouble.as_ref(),
        );
        let attention_line = attention_headline(&attention);
        let unserved = !report.declined
            && report
                .programs
                .iter()
                .any(|row| atpkg::state::is_unserved_toolset(&row.state));
        let headline = if !self.observed {
            "Reading package status…".to_string()
        } else if let Some(busy) = self.busy {
            match busy {
                PackagesBusy::Check => "Checking for package updates…".to_string(),
                PackagesBusy::Install => "Installing ALab tools…".to_string(),
                PackagesBusy::Uninstall => "Removing ALab tools…".to_string(),
                PackagesBusy::InstallExtra => "Installing the extra…".to_string(),
                PackagesBusy::InstallAdmin => {
                    "Installing through macOS — the administrator dialog is open…".to_string()
                }
                PackagesBusy::MachineApply => "Applying the machine settings…".to_string(),
            }
        } else if let Some(command) = self.last_command.as_ref() {
            command.headline().to_string()
        } else if !report.available {
            "Package tool missing".to_string()
        } else if !report.manager_enabled {
            "Package updates are off in this build".to_string()
        } else if let Some(line) = attention_line.clone() {
            line
        } else if report.ignored_prefix.is_some() {
            IGNORED_PREFIX_HEADLINE.to_string()
        } else if unserved {
            UNSERVED_HEADLINE.to_string()
        } else if report.collection_error.is_some() {
            "Some package details couldn\u{2019}t be read".to_string()
        } else if report.declined {
            // Removed on purpose (`aterm pkg uninstall --all`, Remove ALab Tools): what is
            // left in the record is stale, and "up to date" would be about nothing.
            "ALab tools aren\u{2019}t installed".to_string()
        } else if report.recorded
            && report
                .programs
                .iter()
                .any(|row| row.update.behind(&row.kind))
        {
            // Never "up to date" over a row that names a newer build (the row itself
            // stops saying so too, [`ProgramUpdate::words`]).
            "Updates are available".to_string()
        } else if report.recorded {
            "ALab tools are up to date".to_string()
        } else {
            "Not checked yet".to_string()
        };
        let now_unix = now
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
        let offset = report.clock;
        // THE RECORD IN WORDS (2026-09-23 audit, SB-20): when the last full check ran,
        // relative and local — never raw UTC — and the pass's own sentence only where it
        // says more than the headline does (a healthy "up to date (index build N)" does
        // not; the index build stays in the log and `aterm pkg status`). `say_current`
        // keeps "up to date" where no headline says it: under a verb's own result.
        let recorded_detail = |say_current: bool| {
            let mut parts: Vec<String> = Vec::new();
            let outcome = report.outcome.trim();
            if outcome_is_plainly_current(outcome) {
                if say_current {
                    parts.push("up to date".to_string());
                }
            } else if !outcome.is_empty() {
                parts.push(outcome.to_string());
            }
            let stamp = if report.last_success_at.is_empty() {
                &report.updated_at
            } else {
                &report.last_success_at
            };
            if !stamp.is_empty() {
                // A stamp that does not parse is quoted as it stands.
                parts.push(match aterm_update_core::pkg_check::rfc3339_to_unix(stamp) {
                    Some(at) => format!("checked {}", when_words(at, now_unix, offset)),
                    None => format!("checked {stamp}"),
                });
            }
            (!parts.is_empty()).then(|| parts.join("  \u{b7}  "))
        };
        // A machine apply that succeeded quotes the pass's own verdict sentence
        // (`applied — …` / `nothing changed — …`) instead of a bare headline.
        let command_feedback = match (
            self.last_command.as_ref(),
            self.machine.last_verdict.as_deref(),
        ) {
            (
                Some(PackagesCommandOutcome::Succeeded {
                    operation: PackagesBusy::MachineApply,
                }),
                Some(verdict),
            ) => Some(format!("Machine settings: {verdict}")),
            (Some(command), _) => Some(command.feedback()),
            (None, _) => None,
        };
        let mut detail = if !self.observed {
            None
        } else if let Some(command) = self.last_command.as_ref() {
            let mut detail = command_feedback
                .clone()
                .unwrap_or_else(|| command.feedback());
            // What still needs attention follows the verb's own result, never
            // replaces it.
            if let Some(line) = attention_line.as_deref() {
                detail.push_str("  \u{b7}  ");
                detail.push_str(line);
            }
            match command {
                PackagesCommandOutcome::Succeeded { .. } => {
                    if let Some(recorded) = recorded_detail(false) {
                        detail.push_str("  \u{b7}  ");
                        detail.push_str(&recorded);
                    }
                }
                // What was true BEFORE this attempt, said as such: the record is not
                // this attempt's result.
                PackagesCommandOutcome::Failed { .. } => {
                    if let Some(recorded) = recorded_detail(true) {
                        detail.push_str("  \u{b7}  Before this attempt: ");
                        detail.push_str(&recorded);
                    }
                }
            }
            Some(detail)
        } else if !report.available {
            Some("The package tool is missing from this copy of aterm.".to_string())
        } else if !report.manager_enabled {
            // The one cause an inert manager has — an unpinned build; `aterm pkg doctor`
            // names the key.
            Some("This build can\u{2019}t install packages.".to_string())
        } else if report.recorded {
            recorded_detail(false).map(|recorded| sentence_case(&recorded))
        } else {
            Some("Click Check & Update Now to check.".to_string())
        };
        // The troubled pass's own cause leads the detail — the headline names it
        // only as "the last package check failed" / "didn't run".
        if self.observed
            && self.last_command.is_none()
            && let Some(trouble) = self.pass_trouble.as_ref()
        {
            let cause = trouble.cause.trim();
            let last = format!(
                "Last check: {}",
                cause.strip_prefix("atpkg: ").unwrap_or(cause)
            );
            detail = Some(match detail {
                Some(rest) => format!("{last}  \u{b7}  {rest}"),
                None => last,
            });
        }
        // A `[packages] prefix` this build ignores stops every unattended install: its
        // sentence rides the detail whatever else it says, until the line is removed.
        if self.observed
            && let Some(note) = report.ignored_prefix.as_deref()
        {
            match detail.as_mut() {
                Some(detail) => {
                    detail.push_str("  ·  ");
                    detail.push_str(note);
                }
                None => detail = Some(note.to_string()),
            }
        }
        // A retired `auto_update = false` holding Automatic updates off does NOT ride this
        // line: the hero paints the detail on one line, and atpkg's sentence for it — the
        // remedy last — lost exactly the remedy to the painter's ellipsis at every width
        // (review of the Phase 4 merge, 2026-09-23). It is its own wrapped note under the
        // switch ([`PackagesProjection::retired_switch_note`]).
        if self.observed
            && let Some(error) = report.collection_error.as_deref()
        {
            let warning = format!("Package metadata warning: {error}");
            match detail.as_mut() {
                Some(detail) => {
                    detail.push_str("  ·  ");
                    detail.push_str(&warning);
                }
                None => detail = Some(warning),
            }
        }
        // THE SWITCH IS LIVE (Phase 4): the window's package loop re-reads `[packages]`
        // before every pass and every few seconds of a park, so what is saved is what runs
        // — no "next launch". ONE switch (2026-09-23): `[packages] enabled`, Automatic
        // updates, with the retired `auto_update` folded in ([`Self::master_enabled`]); when
        // that retired key is what holds it off, the line names it
        // ([`Self::retired_switch_note`]) — the one control that clears it is the switch.
        // The line says what runs; when the last full check completed is the headline's
        // detail ("Checked 3 min ago").
        let loop_status = if !self.loop_running {
            "Not running in this window — Update Now still works".to_string()
        } else if !self.master_enabled && self.retired_switch_note.is_some() {
            "Off — the retired [packages] auto_update = false holds it off (turning Automatic \
             updates on removes it); Update Now still works"
                .to_string()
        } else if !self.master_enabled {
            "Off — nothing updates by itself; Update Now still works".to_string()
        } else {
            "On — updates arrive in the background, without interrupting".to_string()
        };
        let programs = report
            .programs
            .iter()
            .map(|row| {
                let mut row = row.clone();
                row.words = row.update.words(&row.kind, &row.state, now_unix, offset);
                row
            })
            .collect();
        let activity = activity_lines(&report.activity, now_unix, offset);
        // The machine verb is NOT manager-gated: `atpkg machine apply` is a local
        // verb that works with the manager switched off, so its slice gets the
        // page's idleness alone — observed, atpkg present, no worker inflight.
        let machine_idle = self.observed && report.available && !self.inflight;
        let machine = self.machine_projection(machine_idle, now);
        PackagesProjection {
            machine,
            observed: self.observed,
            available: report.available,
            manager_enabled: report.manager_enabled,
            // The compiled pin IS the live anchor now, so its fingerprint is
            // always the honest one to show.
            root_fingerprint: report.root_fingerprint.clone(),
            recorded: report.recorded,
            updated_at: report.updated_at.clone(),
            outcome: report.outcome.clone(),
            index_source: report.index_source.clone(),
            programs,
            activity,
            log_path: report.log_path.clone(),
            needs_admin: report.needs_admin(),
            admin_door: cfg!(target_os = "macos"),
            collection_error: report.collection_error.clone(),
            busy: self.busy,
            refreshing: self.inflight && self.busy.is_none(),
            master_enabled: self.master_enabled,
            auto_install: self.auto_install,
            loop_running: self.loop_running,
            loop_status,
            retired_switch_note: self.retired_switch_note.clone(),
            actions_enabled,
            headline,
            detail,
            command_feedback,
            attention,
        }
    }

    /// The "This Mac" card's words, derived HERE so pixels, accessibility and
    /// `aterm ctl` introspection read one truth. `idle` is the page's idleness
    /// (observed, atpkg present, no worker inflight) and deliberately NOT the
    /// manager-gated `actions_enabled`: `atpkg machine apply` is a local verb that
    /// works with the package manager switched off. The button additionally needs
    /// a parsed record, nothing in the way, and a verdict — measured posture ×
    /// the SAVED switches, which is what the apply child will read — that says
    /// something is left to do.
    fn machine_projection(&self, idle: bool, now: std::time::SystemTime) -> MachineProjection {
        use atpkg::config::UniversalControlPolicy;
        use atpkg::machine::{HomePosture, UcPosture};
        let posture = &self.machine;
        let supported = cfg!(target_os = "macos");
        let state = posture.state.as_ref();
        // A read that failed AFTER a good record keeps the record (a transient
        // failure must not erase a measurement) but every measured sentence says
        // it is prior — a stale record never reads as a fresh one.
        let prior = if posture.read_error.is_some() && state.is_some() {
            " (from the last successful read)"
        } else {
            ""
        };
        let universal_control = match state {
            None => "Universal Control: not read yet".to_string(),
            Some(s) => {
                let word = match s.universal_control {
                    UcPosture::Disabled => "disabled on this Mac",
                    UcPosture::Default => {
                        "at the OS default — the cursor roams to other Macs and iPads"
                    }
                    UcPosture::Partial => "partly disabled",
                    // NOT "unknown" bare: the reader needs to know the machine was not
                    // measured, and why that is not the same as "off".
                    UcPosture::Unknown => "could not be read on this Mac",
                };
                let policy = match (s.policy, s.universal_control) {
                    (UniversalControlPolicy::Leave, UcPosture::Disabled)
                    | (UniversalControlPolicy::Off, _) => "",
                    // NOT `= "leave"`: any spelling the parser does not know resolves
                    // to Leave, so quoting the file back at the user could quote a word
                    // they never wrote.
                    (UniversalControlPolicy::Leave, _) => {
                        " · left alone ([machine] universal_control is not \"off\")"
                    }
                };
                format!("Universal Control: {word}{policy}{prior}")
            }
        };
        let spotlight = match state {
            None => "Build output: not read yet".to_string(),
            Some(s) => {
                let at_least = if s.scan_complete {
                    ""
                } else {
                    " (at least — the scan hit its budget)"
                };
                let policy = if s.spotlight_noindex {
                    ""
                } else {
                    " · switched off ([machine] spotlight_noindex = false)"
                };
                // What an apply CAN hide — the CLI's `can_hide`: nothing with the
                // switch off, whatever the scan counted as migratable.
                let can_hide = if s.spotlight_noindex {
                    s.would_migrate
                } else {
                    0
                };
                format!(
                    "Build output: {} target dirs hidden, {} open to Spotlight — {can_hide} a pass would hide{at_least}{policy}{prior}",
                    s.hidden, s.exposed
                )
            }
        };
        let last_change = match posture.last_change.as_ref() {
            None => "No change recorded this launch".to_string(),
            Some((items, at)) => {
                let age_ms = now
                    .duration_since(*at)
                    .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
                    .unwrap_or(0);
                format!(
                    "Last change: {items} · {}",
                    crate::session_chrome::relative_age(age_ms)
                )
            }
        };
        let reason = if let Some(error) = posture.read_error.as_deref() {
            Some(format!("Could not read the machine state: {error}"))
        } else if state.is_some_and(|s| s.config_unreadable) {
            // The refusal the owner can fix, and the one that must not read as a
            // measurement: both `[machine]` defaults ACT, so a file that does not parse
            // cannot be treated as "no opt-outs were set".
            Some(
                "Not applied here: aterm.toml does not parse, so the [machine] switches \
                 could not be read. Fix the file and the next pass applies them."
                    .to_string(),
            )
        } else {
            match state.map(|s| s.home) {
                Some(HomePosture::Mismatch) => Some(
                    "Not applied here: this window's HOME is not the account's home, and \
                     `defaults` writes the account's per-host settings regardless."
                        .to_string(),
                ),
                Some(HomePosture::Unresolved) => Some(
                    "Not applied here: the account home could not be resolved, so HOME \
                     cannot be proven to be it."
                        .to_string(),
                ),
                Some(HomePosture::Account) | None => None,
            }
        };
        // A switch saved in Settings after the last read: the record's `policy=` /
        // `noindex=` came from the file as it was THEN, so say what the next read,
        // pass or Apply now will use — the card must never look as if a flipped
        // switch had already been measured.
        let saved = state.and_then(|s| {
            let mut changed: Vec<String> = Vec::new();
            if s.policy != self.saved_universal_control {
                changed.push(format!(
                    "universal_control = \"{}\"",
                    match self.saved_universal_control {
                        UniversalControlPolicy::Off => "off",
                        UniversalControlPolicy::Leave => "leave",
                    }
                ));
            }
            if s.spotlight_noindex != self.saved_spotlight_noindex {
                changed.push(format!(
                    "spotlight_noindex = {}",
                    self.saved_spotlight_noindex
                ));
            }
            (!changed.is_empty()).then(|| {
                format!(
                    "Saved since the last read: {} — the next package pass or Apply now uses it",
                    changed.join(", ")
                )
            })
        });
        // THE verdict — the same `machine_next` table the CLI uses, over the
        // measured posture and the SAVED switches (the apply child reads the file
        // fresh, so a switch flipped after the read decides what it will do; the
        // record's own `policy=`/`noindex=` only feed the measured sentences and
        // the "Saved since the last read" line). None on any home but the
        // account's, exactly as `MachineState::next`.
        let next = state.and_then(|s| {
            (s.home == HomePosture::Account && !s.config_unreadable)
                .then(|| {
                    atpkg::machine::machine_next(
                        s.universal_control,
                        self.saved_universal_control,
                        self.saved_spotlight_noindex,
                        s.would_migrate,
                    )
                })
                .flatten()
        });
        let apply_enabled = idle
            && supported
            && posture.observed
            && !posture.refreshing
            && !posture.awaiting_record
            && reason.is_none()
            && next.is_some();
        // `next()` is already None on a home mismatch, and the reason line says
        // why — so "nothing to apply" is claimed only when nothing is in the way.
        let nothing_to_apply = posture.observed && reason.is_none() && next.is_none();
        let next = match next {
            Some(n) if reason.is_none() => {
                let mut what: Vec<String> = Vec::new();
                if n.universal_control {
                    what.push("Universal Control off for this host".to_string());
                }
                if n.spotlight > 0 {
                    what.push(format!(
                        "{} target dir(s) hidden from Spotlight",
                        n.spotlight
                    ));
                }
                format!("Apply now would set: {}", what.join("; "))
            }
            // A SCAN THAT DID NOT FINISH CANNOT SAY SPOTLIGHT IS SETTLED — the same rule
            // the CLI verdict follows, so the two surfaces never disagree.
            // Exposed but unreachable — the CLI's `nothing an apply can do` arm. The
            // card must not say the machine is where [machine] wants it while the line
            // above it counts directories that are still open.
            _ if nothing_to_apply
                && state.is_some_and(|s| {
                    s.scan_complete && s.spotlight_noindex && s.exposed > s.would_migrate
                }) =>
            {
                let stuck = state.map_or(0, |s| s.exposed - s.would_migrate);
                format!(
                    "Nothing an apply can do — {stuck} target dir(s) stay open to Spotlight \
                     (free-standing, or refused for now)"
                )
            }
            _ if nothing_to_apply && state.is_some_and(|s| !s.scan_complete) => {
                "Nothing to apply from what was seen — Universal Control is where [machine] \
                 wants it, and the build-output scan did not finish"
                    .to_string()
            }
            _ if nothing_to_apply => {
                "Nothing to apply — Universal Control and Spotlight are where [machine] wants them"
                    .to_string()
            }
            _ => String::new(),
        };
        MachineProjection {
            supported,
            observed: posture.observed,
            refreshing: posture.refreshing || posture.awaiting_record,
            universal_control,
            spotlight,
            last_change,
            last_verdict: posture.last_verdict.clone(),
            reason,
            saved,
            next,
            apply_enabled,
            nothing_to_apply,
        }
    }
}

/// The "This Mac" card on Settings ▸ Security, as words (the
/// [`PackagesProjection`] slice for the `[machine]` host settings).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MachineProjection {
    /// These are macOS host settings; elsewhere the card is absent.
    pub(crate) supported: bool,
    /// A machine read has completed at least once.
    pub(crate) observed: bool,
    /// The machine read worker is running.
    pub(crate) refreshing: bool,
    /// `Universal Control: …` — the measured posture.
    pub(crate) universal_control: String,
    /// `Build output: …` — the Spotlight counts.
    pub(crate) spotlight: String,
    /// `Last change: … · <age>` or `No change recorded this launch`.
    pub(crate) last_change: String,
    /// The last `atpkg machine apply` verdict this launch, when there is one.
    pub(crate) last_verdict: Option<String>,
    /// Why an apply would do nothing here (home mismatch/unresolved) or why the
    /// state could not be read. `None` when there is nothing in the way.
    pub(crate) reason: Option<String>,
    /// A `[machine]` switch saved after the last read, and what will use it.
    pub(crate) saved: Option<String>,
    /// What Apply now would do (`Apply now would set: …`), or the "nothing to
    /// apply" sentence; empty while unobserved or when a reason is in the way.
    pub(crate) next: String,
    /// The Apply now button: page idle, macOS, observed, not refreshing, and the
    /// verdict says something is left to do.
    pub(crate) apply_enabled: bool,
    /// Observed, readable, and the verdict is "nothing to apply" — the card says so
    /// in place of an enabled button.
    pub(crate) nothing_to_apply: bool,
}

/// Owned, structured read projection shared with the native Settings
/// `/packages` route (the [`crate::update_screen::UpdateProjection`] analogue).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PackagesProjection {
    /// The `[machine]` host settings slice (Settings ▸ Security's "This Mac" card).
    pub(crate) machine: MachineProjection,
    pub(crate) observed: bool,
    pub(crate) available: bool,
    pub(crate) manager_enabled: bool,
    pub(crate) root_fingerprint: String,
    pub(crate) recorded: bool,
    pub(crate) updated_at: String,
    pub(crate) outcome: String,
    pub(crate) index_source: String,
    pub(crate) programs: Vec<PackagesProgramRow>,
    /// The programs waiting on an administrator, in the order the GUI door installs
    /// them ([`needs_admin_order`]). The Install control on any of them runs the door
    /// for that program AND everything before it in this list.
    pub(crate) needs_admin: Vec<String>,
    /// The osascript door exists on this platform (macOS). Elsewhere the Needs-admin
    /// rows name the terminal command instead of offering a control that could only
    /// record `needs admin` again.
    pub(crate) admin_door: bool,
    pub(crate) collection_error: Option<String>,
    pub(crate) busy: Option<PackagesBusy>,
    /// A SILENT status-refresh worker is inflight (no `busy` label). Action
    /// admission must refuse during this brief window too — the host runs one
    /// worker at a time, and a raced click would otherwise be dropped.
    pub(crate) refreshing: bool,
    /// `[packages] enabled` — Automatic updates, resolved (the retired `auto_update`
    /// folded in).
    pub(crate) master_enabled: bool,
    /// `[packages] auto_install` — the one install consent, resolved.
    pub(crate) auto_install: bool,
    pub(crate) loop_running: bool,
    /// What is actually running now, plus any saved-vs-live mismatch. Derived
    /// here so pixels, accessibility, and introspection use one truth source.
    /// The page wraps it ([`note_lines`]): no state of it is ever left to the ellipsis.
    pub(crate) loop_status: String,
    /// atpkg's own sentence for a retired `[packages] auto_update = false` that holds
    /// Automatic updates off (`atpkg::config::auto_update_note`, doctor's and the config
    /// editor's words), which the page paints WRAPPED under the switch — never on a
    /// one-line row, where the remedy it ends with was the part cut.
    pub(crate) retired_switch_note: Option<String>,
    /// Both action buttons: available AND enabled AND idle AND observed.
    pub(crate) actions_enabled: bool,
    pub(crate) headline: String,
    pub(crate) detail: Option<String>,
    /// Final result text for replacing the initiating view's temporary
    /// synchronous “request accepted” feedback.
    pub(crate) command_feedback: Option<String>,
    /// What failed, named ([`attention_items`]): the Settings rail's Packages
    /// badge is up while this is non-empty, and [`attention_headline`] of it is
    /// the headline whenever nothing more urgent holds that.
    pub(crate) attention: Vec<String>,
    /// The package log's newest events as the Activity card reads them, newest first
    /// ([`activity_lines`]).
    pub(crate) activity: Vec<ActivityLine>,
    /// The package log, for "Open Log" (`None`: no log directory resolves).
    pub(crate) log_path: Option<std::path::PathBuf>,
}

/// One line of Settings ▸ Packages' Activity (Phase 4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActivityLine {
    /// When, relative and local ([`when_words`]).
    pub(crate) when: String,
    /// What happened.
    pub(crate) text: String,
    /// A failure or a refusal — the kind of line a person scans for.
    pub(crate) trouble: bool,
}

/// THE ACTIVITY LIST: the package log's events ([`atpkg::packages_log`]), newest first, as
/// sentences — a program's move (`claude 2.1.278 → 2.1.280 · updated · Anthropic latest`),
/// why a row stopped being current (`codex held: held by local pin`), a pass's end
/// (`Update check · aterm window · finished: up to date · 7 s`) — and a pass that started
/// with no end after it (still running, or killed). A pass's START is otherwise left out:
/// its end says everything. Pure for the test.
pub(crate) fn activity_lines(
    entries: &[atpkg::packages_log::Entry],
    now: i64,
    clock: LocalClock,
) -> Vec<ActivityLine> {
    activity_sentences(entries)
        .into_iter()
        .map(|(entry, text, trouble)| ActivityLine {
            when: entry.at.map_or_else(
                || "at an unknown time".to_string(),
                |at| when_words(at, now, clock),
            ),
            text,
            trouble,
        })
        .collect()
}

/// [`activity_lines`]' sentences with the event each says, before any time is put to
/// them. Newest first in, newest first out.
pub(crate) fn activity_sentences(
    entries: &[atpkg::packages_log::Entry],
) -> Vec<(&atpkg::packages_log::Entry, String, bool)> {
    use atpkg::packages_log::kind;
    let mut ended: Vec<u32> = Vec::new();
    let mut out = Vec::new();
    for entry in entries {
        let get = |key: &str| entry.get(key).unwrap_or("");
        let (text, trouble) = match entry.kind.as_str() {
            kind::PROGRAM => program_activity(
                get("program"),
                entry.get("from"),
                entry.get("to"),
                get("source"),
                get("result"),
                get("reason"),
            ),
            kind::PASS_END => {
                ended.push(entry.pid);
                let exit: u8 = get("exit").parse().unwrap_or(1);
                let mut text = format!(
                    "{} \u{b7} {}",
                    pass_words(get("verb")),
                    lane_words(get("lane"))
                );
                let said = match exit {
                    0 => "finished",
                    69 => "offline — tried again soon",
                    75 => "did not run",
                    1 => "failed",
                    _ => "ended",
                };
                text.push_str(" \u{b7} ");
                text.push_str(said);
                let outcome = get("outcome");
                if !outcome.is_empty() && !(exit == 75 && outcome.starts_with("did not run")) {
                    text.push_str(": ");
                    text.push_str(outcome);
                } else if !matches!(exit, 0 | 1 | 69 | 75) {
                    text.push_str(&format!(" (exit {exit})"));
                }
                if let Ok(secs) = get("secs").parse::<u64>()
                    && secs > 0
                {
                    text.push_str(&format!(" \u{b7} {secs} s"));
                }
                (text, !matches!(exit, 0 | 69 | 75))
            }
            kind::PASS_START if !ended.contains(&entry.pid) => (
                format!(
                    "{} \u{b7} {} \u{b7} started, no end recorded yet",
                    pass_words(get("verb")),
                    lane_words(get("lane"))
                ),
                false,
            ),
            _ => continue,
        };
        out.push((entry, text, trouble));
    }
    out
}

/// A program transition as the Activity says it.
fn program_activity(
    program: &str,
    from: Option<&str>,
    to: Option<&str>,
    source: &str,
    result: &str,
    reason: &str,
) -> (String, bool) {
    let name = package_program_display_name(program);
    let with_source = |mut text: String| {
        if !source.is_empty() {
            text.push_str(" \u{b7} ");
            text.push_str(source);
        }
        text
    };
    match (result, from, to) {
        ("installed", _, Some(to)) => (with_source(format!("{name} {to} installed")), false),
        ("updated" | "rolled back", Some(from), Some(to)) => (
            with_source(format!("{name} {from} \u{2192} {to} \u{b7} {result}")),
            false,
        ),
        ("removed", Some(from), _) => (format!("{name} {from} removed"), false),
        ("current", _, _) => (with_source(format!("{name} is current again")), false),
        _ => {
            // The row's own words, less the prefix the result already names — and said
            // once: a reason that names its program (`codex 0.157.0 refused: …`) or opens
            // with the result (`held by local pin`) is the sentence itself.
            let why = [
                "error: ",
                "aborted: ",
                "tombstoned: ",
                "rejected: ",
                "deferred: ",
                "held: ",
                "unavailable: ",
                "blocked: ",
            ]
            .iter()
            .find_map(|p| reason.strip_prefix(p))
            .unwrap_or(reason)
            .trim();
            let text = if why.is_empty() {
                format!("{name} {result}")
            } else if why
                .strip_prefix(program)
                .is_some_and(|rest| rest.starts_with(' '))
            {
                why.to_string()
            } else if why.starts_with(result) {
                format!("{name} {why}")
            } else {
                format!("{name} {result}: {why}")
            };
            (text, matches!(result, "failed" | "refused" | "disabled"))
        }
    }
}

/// A pass's verb as the Activity names it.
fn pass_words(verb: &str) -> String {
    let mut words = verb.split(' ');
    match (words.next().unwrap_or(""), words.next()) {
        ("update", None) => "Update check".to_string(),
        ("update", Some(program)) => format!("{} update", package_program_display_name(program)),
        ("seed", _) => "Launch check".to_string(),
        ("install", Some("--default-set")) => "ALab tools install".to_string(),
        ("install", Some(program)) => format!("Install {program}"),
        ("uninstall", Some("--all")) => "ALab tools removal".to_string(),
        (other, rest) => {
            let mut text = sentence_case(other);
            if let Some(rest) = rest {
                text.push(' ');
                text.push_str(rest);
            }
            text
        }
    }
}

/// Which lane ran a pass, as the Activity names it (atpkg's lane words: `window`,
/// `session`, `head watch`, `agent update`, `typed`).
fn lane_words(lane: &str) -> &str {
    match lane {
        "window" => "aterm window",
        "session" => "terminal session",
        "head watch" => "release watch",
        "agent update" => "you asked",
        "typed" => "command line",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refresh(report: PackagesStatusReport) -> PackagesWorkerCompletion {
        PackagesWorkerCompletion::refresh(report)
    }

    fn succeeded(
        report: PackagesStatusReport,
        operation: PackagesBusy,
    ) -> PackagesWorkerCompletion {
        PackagesWorkerCompletion::command(report, PackagesCommandOutcome::Succeeded { operation })
    }

    fn status(outcome: &str) -> atpkg::Status {
        let mut programs = std::collections::BTreeMap::new();
        programs.insert(
            "ay".to_string(),
            atpkg::ProgramStatus {
                installed_build: Some(1971),
                state: "active".to_string(),
                tree_root: String::new(),
            },
        );
        atpkg::Status {
            schema: 1,
            updated_at: "2026-07-21T00:00:00Z".to_string(),
            enabled: true,
            index_source: "alabsystems/aterm".to_string(),
            outcome: outcome.to_string(),
            seams: Vec::new(),
            last_success_at: String::new(),
            last_index_reached_at: String::new(),
            last_index_build: 0,
            index_build_changed_at: String::new(),
            last_pass: String::new(),
            last_pass_at: String::new(),
            last_pass_attempted_index_build: 0,
            last_pass_attempted_at: String::new(),
            metered_hold_until: String::new(),
            programs,
            extra: Default::default(),
        }
    }

    /// One worker at a time; a stale sequence is reducer-inert; a current
    /// completion clears busy, stores the report, and bumps the revision.
    #[test]
    fn service_serializes_workers_and_rejects_stale_completions() {
        let mut service = PackagesService::new();
        let base = service.revision();
        let seq = service
            .begin(Some(PackagesBusy::Check))
            .expect("idle → begin");
        assert!(service.revision() > base, "busy flip fans out");
        assert_eq!(service.busy(), Some(PackagesBusy::Check));
        assert!(
            service.begin(Some(PackagesBusy::Install)).is_none(),
            "second verb joins, never overlaps"
        );

        let report =
            PackagesStatusReport::from_parts(true, true, "fp".into(), Some(&status("ok")), &[]);
        assert!(
            !service.finish(seq + 1, succeeded(report.clone(), PackagesBusy::Check)),
            "a superseded worker cannot import facts"
        );
        assert_eq!(
            service.busy(),
            Some(PackagesBusy::Check),
            "stale finish is inert"
        );
        let before = service.revision();
        assert!(service.finish(seq, succeeded(report, PackagesBusy::Check)));
        assert_eq!(service.busy(), None);
        assert!(service.revision() > before);
        let state = service.state(true, true, true);
        let projection = state.projection();
        assert!(projection.recorded);
        assert_eq!(projection.programs.len(), 1);
        assert_eq!(projection.programs[0].installed_build, Some(1971));
        assert!(projection.actions_enabled);
        assert_eq!(projection.headline, "Package check completed");
        assert_eq!(
            projection.command_feedback.as_deref(),
            Some("Package check completed")
        );
    }

    /// A spawn-failure abort releases the reservation WITHOUT importing facts:
    /// a never-observed surface stays unobserved (no fabricated inert claim),
    /// a previously-observed one keeps its real report; stale aborts are inert.
    #[test]
    fn abort_releases_the_reservation_without_fabricating_a_report() {
        let mut service = PackagesService::new();
        let seq = service.begin(None).expect("idle → begin");
        let before = service.revision();
        assert!(service.abort(seq), "current abort releases");
        assert!(service.revision() > before, "the un-busy flip fans out");
        assert_eq!(service.busy(), None);
        let state = service.state(true, true, true);
        assert!(
            state.projection().headline.contains("Reading"),
            "never-observed stays honestly unobserved after an abort"
        );

        // Observed facts survive a later abort untouched.
        let seq = service.begin(Some(PackagesBusy::Check)).unwrap();
        let report =
            PackagesStatusReport::from_parts(true, true, "fp".into(), Some(&status("ok")), &[]);
        assert!(service.finish(seq, succeeded(report, PackagesBusy::Check)));
        let seq = service.begin(Some(PackagesBusy::Install)).unwrap();
        assert!(!service.abort(seq + 1), "stale abort is inert");
        assert_eq!(service.busy(), Some(PackagesBusy::Install));
        assert!(service.abort(seq));
        let projection = service.state(true, true, true).projection();
        assert!(projection.recorded, "the prior report's facts survive");
        assert_eq!(projection.programs.len(), 1);
        assert!(projection.actions_enabled);
    }

    /// The honest-state ladder: unobserved → unavailable → inert → recorded,
    /// each with a distinct headline and action gating that never lies.
    #[test]
    fn projection_headlines_track_the_honest_manager_posture() {
        let unobserved = PackagesState::unobserved().projection();
        assert!(unobserved.headline.contains("Reading"));
        assert!(
            !unobserved.actions_enabled,
            "nothing is claimed before observation"
        );

        let mut service = PackagesService::new();
        let seq = service.begin(None).unwrap();
        assert!(service.finish(
            seq,
            refresh(PackagesStatusReport::from_parts(
                false,
                false,
                String::new(),
                None,
                &[],
            )),
        ));
        let unavailable = service.state(true, true, true).projection();
        assert_eq!(unavailable.headline, "Package tool missing");
        assert_eq!(
            unavailable.detail.as_deref(),
            Some("The package tool is missing from this copy of aterm.")
        );
        assert!(!unavailable.actions_enabled);

        let seq = service.begin(None).unwrap();
        assert!(service.finish(
            seq,
            refresh(PackagesStatusReport::from_parts(
                true,
                false,
                "0000".into(),
                None,
                &[],
            )),
        ));
        let inert = service.state(true, true, true).projection();
        assert_eq!(inert.headline, "Package updates are off in this build");
        assert_eq!(
            inert.detail.as_deref(),
            Some("This build can\u{2019}t install packages.")
        );
        assert!(!inert.actions_enabled);

        let seq = service.begin(None).unwrap();
        assert!(service.finish(
            seq,
            refresh(PackagesStatusReport::from_parts(
                true,
                true,
                "fp".into(),
                Some(&status("up to date")),
                &[]
            )),
        ));
        let live = service.state(true, true, true).projection();
        assert!(live.actions_enabled);
        assert_eq!(live.headline, "ALab tools are up to date");
        // The headline says it; the detail says when, never "up to date (index build N)".
        let detail = live.detail.as_deref().unwrap();
        assert!(detail.starts_with("Checked "), "{detail}");
        assert!(!detail.contains("index build"), "{detail}");

        // Busy while a verb runs: actions gate off and the headline says which.
        let _ = service.begin(Some(PackagesBusy::Install)).unwrap();
        let busy = service.state(true, true, true).projection();
        assert!(busy.headline.contains("Installing"));
        assert!(!busy.actions_enabled);
    }

    /// No environment variable switches the manager off (2026-09-23): an inert manager
    /// is an unpinned build, and the page says exactly that.
    #[test]
    fn an_inert_manager_is_named_as_an_unpinned_build() {
        let report = PackagesStatusReport::from_parts(
            true,
            false,
            "compiled-root-present".into(),
            None,
            &[],
        );
        let mut service = PackagesService::new();
        let seq = service.begin(None).unwrap();
        assert!(service.finish(seq, refresh(report)));
        let projection = service.state(true, true, true).projection();
        assert!(!projection.manager_enabled);
        let detail = projection.detail.as_deref().unwrap();
        assert_eq!(detail, "This build can\u{2019}t install packages.");
    }

    /// THE SWITCH IS LIVE (Phase 4): the service line says what the loop runs NOW — no
    /// "next launch" — and, when the retired `auto_update = false` is what holds it off
    /// (ONE switch, 2026-09-23: it folds into `enabled`), names that key and the control
    /// that clears it, with atpkg's own sentence for it in the detail. When the last full
    /// check completed is the headline's detail, relative. (Replaces the saved-vs-this-launch
    /// wording the launch-time gates needed.)
    #[test]
    fn the_service_line_says_what_the_live_switch_runs() {
        let service = PackagesService::new();
        let on = service.state(true, true, true).projection();
        assert!(on.loop_running);
        assert!(
            on.loop_status.starts_with("On \u{2014}"),
            "{}",
            on.loop_status
        );
        let off = service.state(false, true, true).projection();
        assert!(!off.master_enabled);
        assert!(
            off.loop_status
                .starts_with("Off \u{2014} nothing updates by itself"),
            "{}",
            off.loop_status
        );
        let note = atpkg::config::auto_update_note(false, Some(true));
        let retired = service
            .state(false, true, true)
            .with_retired_switch_note(Some(note.clone()))
            .projection();
        assert!(
            retired
                .loop_status
                .contains("[packages] auto_update = false")
                && retired.loop_status.contains("Automatic updates on"),
            "the key that holds it off is named, and the control that clears it: {}",
            retired.loop_status
        );
        let absent = service.state(true, true, false).projection();
        assert!(absent.loop_status.starts_with("Not running in this window"));
        for p in [&on, &off, &retired, &absent] {
            assert!(!p.loop_status.contains("launch"), "{}", p.loop_status);
        }
        // A completed pass on record: when, relative — and the retired key's own sentence,
        // in atpkg's words, is the projection's note for the page to wrap, never a part of
        // the one-line hero detail whose ellipsis ate its remedy (review of the merge).
        let mut report = report_with(&[("ay", atpkg::state::managed(1971, 44))]);
        report.last_success_at = "2026-09-23T09:00:00Z".to_string();
        let mut service = PackagesService::new();
        observe(&mut service, report);
        let at = aterm_update_core::pkg_check::rfc3339_to_unix("2026-09-23T12:00:00Z").unwrap();
        let p = service
            .state(true, true, true)
            .projection_at(std::time::UNIX_EPOCH + std::time::Duration::from_secs(at as u64));
        assert_eq!(p.detail.as_deref(), Some("Checked 3 h ago"));
        assert!(!p.loop_status.contains("check"), "{}", p.loop_status);
        let held = service
            .state(false, true, true)
            .with_retired_switch_note(Some(note.clone()))
            .projection();
        assert_eq!(held.retired_switch_note.as_deref(), Some(note.as_str()));
        assert!(
            !held.detail.as_deref().unwrap_or_default().contains(&note),
            "{:?}",
            held.detail
        );
        assert_eq!(p.retired_switch_note, None);
    }

    /// The page's own sentences wrap WHOLE ([`note_lines`]): at the narrowest measure the
    /// retired key's note and the longest service line keep every word, in order, each line
    /// within the measure — no `…(N more)` bound eats the remedy they end with — and a
    /// ` · ` part that fits a line is kept whole on one.
    #[test]
    fn note_lines_keep_every_word_within_the_measure() {
        let words = |text: &str| -> Vec<String> {
            text.split_whitespace()
                .filter(|word| *word != "\u{b7}")
                .map(str::to_string)
                .collect()
        };
        let note = atpkg::config::auto_update_note(false, Some(true));
        let service = "Off \u{2014} the retired [packages] auto_update = false holds it off \
                       (turning Automatic updates on removes it); Update Now still works \
                       \u{b7} last full check 3 h ago";
        for text in [note.as_str(), service] {
            for width in [MIN_REASON_WIDTH, 36, 56] {
                let lines = note_lines(text, width);
                assert!(lines.len() > 1, "{width}: wrapped");
                assert!(
                    lines.iter().all(|line| fits(line, width)),
                    "{width}: {lines:?}"
                );
                assert_eq!(
                    words(&lines.join(" ")),
                    words(text),
                    "{width}: every word, in order"
                );
            }
        }
        assert!(
            note_lines(service, 36)
                .last()
                .is_some_and(|line| line.ends_with("last full check 3 h ago")),
            "the last full check is one part, whole on its line"
        );
        assert_eq!(note_lines("On", 36), vec!["On".to_string()]);
    }

    /// A failed process must outrank a stale successful status report, update
    /// the initiating Settings feedback, and survive a later silent refresh.
    #[test]
    fn failed_command_cannot_redisplay_an_earlier_success_as_this_attempt() {
        let mut service = PackagesService::new();
        let old_report = PackagesStatusReport::from_parts(
            true,
            true,
            "fp".into(),
            Some(&status("up to date")),
            &[],
        );
        let first = service.begin(None).unwrap();
        assert!(service.finish(first, refresh(old_report.clone())));

        let sequence = service.begin(Some(PackagesBusy::Check)).unwrap();
        let failed = PackagesWorkerCompletion::command(
            old_report.clone(),
            PackagesCommandOutcome::Failed {
                operation: PackagesBusy::Check,
                message: "atpkg update exited with status 7".to_string(),
            },
        );
        assert!(service.finish(sequence, failed));
        let projection = service.state(true, true, true).projection();
        assert_eq!(projection.headline, "Package check failed");
        assert!(
            projection
                .detail
                .as_deref()
                .unwrap()
                .contains("exited with status 7")
        );
        assert!(
            projection
                .detail
                .as_deref()
                .unwrap()
                .contains("Before this attempt: up to date"),
            "the old success is retained only as explicitly historical context"
        );
        assert!(
            projection
                .command_feedback
                .as_deref()
                .unwrap()
                .starts_with("Package check failed")
        );

        let refresh_sequence = service.begin(None).unwrap();
        assert!(service.finish(refresh_sequence, refresh(old_report)));
        assert_eq!(
            service.state(true, true, true).projection().headline,
            "Package check failed",
            "a background status read cannot erase the last verb result"
        );
    }

    /// Current sequence alone is insufficient: the completion kind/operation
    /// must match the reservation before the reducer may clear busy.
    #[test]
    fn completion_kind_and_operation_must_match_the_reservation() {
        let mut service = PackagesService::new();
        let report = PackagesStatusReport::from_parts(true, true, "fp".into(), None, &[]);
        let sequence = service.begin(Some(PackagesBusy::Install)).unwrap();
        assert!(!service.finish(sequence, refresh(report.clone())));
        assert_eq!(service.busy(), Some(PackagesBusy::Install));
        assert!(!service.finish(sequence, succeeded(report.clone(), PackagesBusy::Check)));
        assert_eq!(service.busy(), Some(PackagesBusy::Install));
        assert!(service.finish(sequence, succeeded(report, PackagesBusy::Install)));
        assert_eq!(service.busy(), None);
    }

    /// A report whose program rows are `rows` (name, state), as a pass wrote them.
    fn report_with(rows: &[(&str, String)]) -> PackagesStatusReport {
        let mut status = status("up to date (index build 44)");
        status.programs = rows
            .iter()
            .map(|(name, state)| {
                (
                    (*name).to_string(),
                    atpkg::ProgramStatus {
                        // A row at its index's pin runs that build, as atpkg records it.
                        installed_build: Some(
                            atpkg::state::managed_pin(state).map_or(1, |(build, _)| build),
                        ),
                        state: state.clone(),
                        tree_root: String::new(),
                    },
                )
            })
            .collect();
        PackagesStatusReport::from_parts(true, true, "fp".into(), Some(&status), &[])
    }

    /// One silent refresh that observes `report`.
    fn observe(service: &mut PackagesService, report: PackagesStatusReport) {
        let sequence = service.begin(None).expect("idle");
        assert!(service.finish(sequence, refresh(report)));
    }

    fn projection(service: &PackagesService) -> PackagesProjection {
        service.state(true, true, true).projection()
    }

    /// A REFUSED VENDOR PROGRAM IS THE BADGE AND THE HEADLINE, UNTIL ITS ROW HEALS
    /// (2026-09-22). A head-watch pass that refuses codex prints no marker and
    /// raises no row (it never did: a targeted refusal is a log line); the refusal
    /// is the `error:` row the pass recorded, and that row is what lights the
    /// Settings ▸ Packages badge and names itself in the page's headline. The
    /// condition clears when the condition does: the next pass rewrote the row.
    #[test]
    fn a_refused_vendor_program_is_the_badge_and_the_headline_until_its_row_heals() {
        let refused =
            "error: codex 0.157.0 refused: SHA256SUMS disagrees with release.json".to_string();
        let claude = atpkg::state::vendor_managed("2.1.280", "Anthropic");
        let mut service = PackagesService::new();
        observe(
            &mut service,
            report_with(&[("claude", claude.clone()), ("codex", refused.clone())]),
        );
        let p = projection(&service);
        // Named as what it is (Phase 4): a refusal, not a flaky "failed".
        assert_eq!(p.attention, ["codex refused"], "the badge is up");
        assert_eq!(p.headline, "Needs attention: codex refused");
        assert!(
            p.programs
                .iter()
                .any(|row| row.name == "codex" && row.state == refused),
            "the row itself is on the page, verbatim"
        );
        observe(
            &mut service,
            report_with(&[
                ("claude", claude),
                ("codex", atpkg::state::vendor_managed("0.157.0", "OpenAI")),
            ]),
        );
        let p = projection(&service);
        assert!(p.attention.is_empty(), "cleared: the badge is off");
        assert_eq!(p.headline, "ALab tools are up to date");
    }

    /// WHAT THE BADGE COUNTS, AND NOTHING ELSE: the faults `atpkg doctor` lists (its
    /// own predicate, one spelling) and a vendor build whose release channel has not
    /// been reached for a day. A held, shadowed, kept, deferred or waiting row is not
    /// a failure; nor is a stray `-` row (doctor skips it, `b867fb7bf`'s `--help`
    /// row), nor the machine-wide no-build-for-this-architecture verdict — a fact
    /// nobody on the machine can act on, so it lit a badge nobody could clear.
    #[test]
    fn attention_is_the_doctors_faults_and_an_unreached_vendor() {
        let item = |name: &str, state: &str| attention_item(name, state);
        assert_eq!(
            item("codex", "error: codex 0.157.0 refused: x").as_deref(),
            Some("codex refused"),
            "a refusal is named as one (Phase 4)"
        );
        assert_eq!(
            item("codex", "error: stage: disk full").as_deref(),
            Some("codex failed")
        );
        assert_eq!(
            item("trust", "aborted: stage").as_deref(),
            Some("trust aborted")
        );
        assert_eq!(
            item("claude", "tombstoned: claude 2.1.279 was yanked").as_deref(),
            Some("claude disabled")
        );
        assert_eq!(
            item(
                "*toolset*",
                "unavailable: every published build was refused"
            )
            .as_deref(),
            Some("ALab tools unavailable")
        );
        assert_eq!(
            item("*index*", "error: index unreachable").as_deref(),
            Some("index failed")
        );
        let unreached = atpkg::state::vendor_kept(
            "2.1.280",
            "Anthropic's release channel not reached since 2026-09-20",
        );
        assert_eq!(
            item("claude", &unreached).as_deref(),
            Some("Anthropic not reached since 2026-09-20")
        );
        let p = std::path::Path::new;
        for benign in [
            atpkg::state::managed(6808, 41),
            atpkg::state::vendor_managed("2.1.280", "Anthropic"),
            atpkg::state::vendor_kept("2.1.280", "held by local pin"),
            atpkg::state::vendor_source("2.1.280", "Anthropic"),
            atpkg::state::shadowed(1971, p("/Users//dev/.local/bin/ay")),
            atpkg::state::system(p("/opt/homebrew/bin/gh"), None),
            atpkg::state::AGENT_INSTALLING.to_string(),
            "extra — not installed (opt in: aterm pkg install gemini)".to_string(),
            "needs admin — run: aterm pkg install clt".to_string(),
            "unavailable on x86_64-apple-darwin: no build is published for this target".to_string(),
            "blocked by clt: needs admin — run: aterm pkg install clt".to_string(),
            "held: pinned build 9 is not published for x86_64-apple-darwin; staying on build 8"
                .to_string(),
            "active".to_string(),
            "linked".to_string(),
        ] {
            assert_eq!(item("x", &benign), None, "{benign}");
        }
        // Doctor's filters, and the verdict nobody can act on.
        assert_eq!(item("--help", "error: not a program"), None, "a stray row");
        for unserved in [
            atpkg::state::TOOLSET_UNSERVED,
            atpkg::state::TOOLSET_UNSERVED_BLOCKED,
        ] {
            assert_eq!(item("*toolset*", unserved), None, "{unserved}");
        }
    }

    /// THE MACHINE NOBODY SERVES, AND THE STORE REMOVED ON PURPOSE, WEAR NO BADGE.
    /// An Intel Mac before x86_64 lands reads its `*toolset*` verdict as a fact in
    /// the headline, badge off; a declined store (`aterm pkg uninstall --all`) keeps
    /// stale fault rows the doctor lists nothing for — badge off too, while a
    /// remembered pass trouble still counts.
    #[test]
    fn an_unserved_machine_and_a_declined_store_wear_no_badge() {
        let mut service = PackagesService::new();
        observe(
            &mut service,
            report_with(&[(
                "*toolset*",
                atpkg::state::TOOLSET_UNSERVED_BLOCKED.to_string(),
            )]),
        );
        let p = projection(&service);
        assert!(p.attention.is_empty(), "no badge: {:?}", p.attention);
        assert_eq!(p.headline, UNSERVED_HEADLINE, "said, as a fact");
        let mut declined = report_with(&[
            (
                "ay",
                "error: ay stage failed: no space left on device".to_string(),
            ),
            ("--help", "error: stray".to_string()),
        ]);
        declined.declined = true;
        let mut service = PackagesService::new();
        observe(&mut service, declined.clone());
        let p = projection(&service);
        assert!(
            p.attention.is_empty(),
            "declined: no badge: {:?}",
            p.attention
        );
        assert_eq!(
            p.headline, "ALab tools aren\u{2019}t installed",
            "removed on purpose: never \"up to date\" about a record of nothing"
        );
        assert!(service.note_pass_trouble(
            PassTroubleKind::Failed,
            "atpkg: prefix is not writable".into(),
            at(100),
        ));
        assert_eq!(
            projection(&service).attention,
            ["the last package check failed: prefix is not writable"],
            "the pass's own trouble is not a declined row"
        );
    }

    /// `secs` past the epoch, as the clock a trouble is noted at.
    fn at(secs: u64) -> std::time::SystemTime {
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs)
    }

    /// A report whose record says a pass completed at `stamp` (RFC 3339).
    fn completed_at(stamp: &str) -> PackagesStatusReport {
        let mut report = report_with(&[("ay", atpkg::state::managed(1971, 44))]);
        report.last_success_at = stamp.to_string();
        report
    }

    /// A `[packages] prefix` THIS BUILD IGNORES IS SAID HERE (2026-09-23 review): it
    /// stops every unattended install until the line is removed, so the page — where every
    /// seed notice points — names it in the headline and carries atpkg's own sentence in
    /// the detail; without it the page read "Toolchain packages managed" over a machine
    /// that would install nothing new. Real trouble still outranks it.
    #[test]
    fn an_ignored_prefix_is_named_on_the_page() {
        let mut service = PackagesService::new();
        let note = atpkg::config::ignored_prefix_note(std::path::Path::new("/opt/aterm/pkg"));
        let mut report = report_with(&[("ay", atpkg::state::managed(1971, 44))]);
        report.ignored_prefix = Some(note.clone());
        observe(&mut service, report);
        let shown = projection(&service);
        assert_eq!(shown.headline, IGNORED_PREFIX_HEADLINE);
        assert_eq!(presented_result_of(&shown.headline), 0);
        assert!(
            shown.detail.as_deref().is_some_and(|d| d.contains(&note)),
            "{:?}",
            shown.detail
        );
        let mut troubled = report_with(&[("ay", "error: verify failed".to_string())]);
        troubled.ignored_prefix = Some(note.clone());
        observe(&mut service, troubled);
        let shown = projection(&service);
        assert_ne!(
            shown.headline, IGNORED_PREFIX_HEADLINE,
            "a failure outranks it"
        );
        assert!(shown.detail.as_deref().is_some_and(|d| d.contains(&note)));
    }

    /// A PASS TROUBLE WITH NO ROW OF ITS OWN — a refusal at atpkg's dispatch edge
    /// (`prefix is not writable`), a child that died after announcing — is the
    /// badge too: its cause leads the detail, the headline names it, and a later
    /// CLEAN whole pass clears it. Same words twice fan nothing out. A stand-down is
    /// the same badge in its own words: the pass did not run.
    #[test]
    fn a_failed_pass_is_the_badge_until_a_clean_pass_clears_it() {
        let mut service = PackagesService::new();
        observe(
            &mut service,
            report_with(&[("ay", atpkg::state::managed(1971, 44))]),
        );
        assert!(projection(&service).attention.is_empty());
        let before = service.revision();
        assert!(service.note_pass_trouble(
            PassTroubleKind::Failed,
            "atpkg: prefix is not writable".into(),
            at(100),
        ));
        assert!(service.revision() > before, "the badge fans out");
        let settled = service.revision();
        assert!(!service.note_pass_trouble(
            PassTroubleKind::Failed,
            "atpkg: prefix is not writable".into(),
            at(200),
        ));
        assert_eq!(service.revision(), settled, "same words: nothing new");
        let p = projection(&service);
        // The headline NAMES THE CAUSE (Phase 4): atpkg's words, its prefix dropped.
        assert_eq!(
            p.attention,
            ["the last package check failed: prefix is not writable"]
        );
        assert_eq!(
            p.headline,
            "Needs attention: the last package check failed: prefix is not writable"
        );
        let detail = p.detail.expect("a detail");
        assert!(
            detail.starts_with("Last check: prefix is not writable"),
            "{detail}"
        );
        assert!(service.note_pass_clean());
        assert!(!service.note_pass_clean(), "idempotent");
        let p = projection(&service);
        assert!(p.attention.is_empty(), "cleared: the badge is off");
        assert_eq!(p.headline, "ALab tools are up to date");
        assert!(!p.detail.unwrap_or_default().starts_with("Last check"));
        assert!(
            service.note_pass_trouble(
                PassTroubleKind::StoodDown,
                "an earlier toolchain pass was still running after 30 min — automatic updates \
             are off; run: aterm pkg seed"
                    .into(),
                at(100),
            )
        );
        let p = projection(&service);
        // The first clause of a long cause, capped at a word.
        assert_eq!(
            p.headline,
            "Needs attention: the package check didn\u{2019}t run: an earlier toolchain pass \
             was still running\u{2026}"
        );
        assert!(
            p.detail
                .as_deref()
                .is_some_and(|d| d.contains("run: aterm pkg seed")),
            "{:?}",
            p.detail
        );
    }

    /// THE TROUBLE CLEARS WHEN A PASS IS KNOWN TO HAVE COMPLETED SINCE — not only
    /// when the window's own lane runs one clean. With the lane stopped (then spelled
    /// `auto_update = false`, which stopped it after the launch seed; today Automatic
    /// updates off, which never starts it), a seed that hit a full disk left the badge
    /// and its headline over the user's own successful Install until relaunch. A
    /// Settings Check or Install that SUCCEEDS clears it (a failed one, or a verb that
    /// is not a whole pass, does not), and so does any refresh whose record's
    /// `last_success_at` is LATER than the moment the trouble was noted — a
    /// terminal's `aterm pkg update` included; an earlier or unparseable stamp does not.
    #[test]
    fn a_pass_completed_since_the_trouble_clears_it() {
        let noted = at(1_790_000_000);
        let troubled = || {
            let mut service = PackagesService::new();
            observe(&mut service, completed_at(""));
            assert!(service.note_pass_trouble(
                PassTroubleKind::Failed,
                "ay stage failed: no space left on device".into(),
                noted,
            ));
            service
        };
        let badged = |service: &PackagesService| !projection(service).attention.is_empty();
        for operation in [PackagesBusy::Check, PackagesBusy::Install] {
            let mut service = troubled();
            let sequence = service.begin(Some(operation)).unwrap();
            assert!(service.finish(sequence, succeeded(completed_at(""), operation)));
            assert!(!badged(&service), "{operation:?} succeeded: cleared");
            assert_eq!(
                projection(&service).headline,
                operation.completed_headline(),
                "the verb's own words, and nothing over them"
            );
        }
        let mut service = troubled();
        let sequence = service.begin(Some(PackagesBusy::Check)).unwrap();
        assert!(service.finish(
            sequence,
            PackagesWorkerCompletion::command(
                completed_at(""),
                PackagesCommandOutcome::Failed {
                    operation: PackagesBusy::Check,
                    message: "exit 1".into(),
                },
            ),
        ));
        assert!(badged(&service), "a failed Check proves nothing");
        let mut service = troubled();
        let sequence = service.begin(Some(PackagesBusy::InstallExtra)).unwrap();
        assert!(service.finish(
            sequence,
            succeeded(completed_at(""), PackagesBusy::InstallExtra)
        ));
        assert!(badged(&service), "one extra is not a whole pass");
        // 1_790_000_000 is 2026-09-21T14:13:20Z.
        for (stamp, clears) in [
            ("2026-09-21T14:13:21Z", true),
            ("2026-09-21T14:13:20Z", false),
            ("2026-09-20T00:00:00Z", false),
            ("not a time", false),
        ] {
            let mut service = troubled();
            observe(&mut service, completed_at(stamp));
            assert_eq!(!badged(&service), clears, "{stamp}");
        }
    }

    /// WHAT HOLDS THE HEADLINE: a verb in flight, a verb's own result — completed
    /// or failed — a manager that cannot act; then what failed. A verb that
    /// COMPLETED keeps its headline (the `NativePackagesWorker` model's
    /// `FinalResultIsPresented`; the attention line once replaced it, and the
    /// conformance bind now catches that) and carries the attention line in its
    /// detail; the badge is up whatever holds the headline; more than three
    /// failures are counted.
    #[test]
    fn the_attention_headline_yields_to_any_verb_result() {
        let failing = report_with(&[("codex", "error: x".to_string())]);
        let mut service = PackagesService::new();
        observe(&mut service, failing.clone());
        assert_eq!(
            projection(&service).headline,
            "Needs attention: codex failed"
        );
        let sequence = service.begin(Some(PackagesBusy::Check)).unwrap();
        assert_eq!(
            projection(&service).headline,
            "Checking for package updates…"
        );
        assert!(service.finish(sequence, succeeded(failing.clone(), PackagesBusy::Check)));
        let p = projection(&service);
        assert_eq!(p.headline, "Package check completed");
        assert_eq!(p.attention, ["codex failed"], "the badge stays up");
        assert!(
            p.detail
                .as_deref()
                .is_some_and(|d| d
                    .starts_with("Package check completed  \u{b7}  Needs attention: codex failed")),
            "{:?}",
            p.detail
        );
        let sequence = service.begin(Some(PackagesBusy::Check)).unwrap();
        assert!(service.finish(
            sequence,
            PackagesWorkerCompletion::command(
                failing.clone(),
                PackagesCommandOutcome::Failed {
                    operation: PackagesBusy::Check,
                    message: "exit 1".into(),
                },
            ),
        ));
        let p = projection(&service);
        assert_eq!(p.headline, "Package check failed", "the verb's own failure");
        assert_eq!(p.attention, ["codex failed"], "the badge stays up");
        let mut inert = PackagesService::new();
        let mut status = status("up to date");
        status.programs.insert(
            "codex".into(),
            atpkg::ProgramStatus {
                installed_build: None,
                state: "error: x".into(),
                tree_root: String::new(),
            },
        );
        observe(
            &mut inert,
            PackagesStatusReport::from_parts(true, false, "0000".into(), Some(&status), &[]),
        );
        let p = projection(&inert);
        assert_eq!(p.headline, "Package updates are off in this build");
        assert!(!p.attention.is_empty());
        let many = report_with(&[
            ("a", "error: 1".to_string()),
            ("b", "error: 2".to_string()),
            ("c", "error: 3".to_string()),
            ("d", "error: 4".to_string()),
        ]);
        let trouble = PassTrouble {
            kind: PassTroubleKind::Failed,
            cause: "x".into(),
            noted_unix: 0,
        };
        assert_eq!(
            attention_headline(&attention_items(&many.programs, false, Some(&trouble))).as_deref(),
            Some("Needs attention: a failed \u{b7} b failed \u{b7} c failed \u{b7} and 2 more")
        );
        assert_eq!(attention_headline(&[]), None);
        // The projection's classifier: an attention headline presents no verb's
        // result, whatever it ends on.
        assert_eq!(presented_result_of("Needs attention: codex failed"), 0);
        assert_eq!(presented_result_of("Package check completed"), 1);
        assert_eq!(presented_result_of("Package check failed"), 2);
    }

    /// A `status.toml` fixture carrying every canonical §17.2 state: the rows parse
    /// through `atpkg::state`'s own readers (never a second spelling), the text is
    /// kept verbatim, and the grouping falls out — Default set, Extras (with the
    /// authored vendor / license / size), and Needs admin, where Homebrew's
    /// `blocked by clt: needs admin …` row waits on the same door as `clt`.
    #[test]
    fn rows_parse_every_canonical_state_and_group_by_it() {
        let p = std::path::Path::new;
        let claude = atpkg::vendor_direct::Version::parse("2.1.280")
            .unwrap()
            .build_id();
        let fixture: Vec<(&str, String, Option<u64>)> = vec![
            ("trust", atpkg::state::managed(6808, 41), Some(6808)),
            (
                "ay",
                atpkg::state::shadowed(1971, p("/Users//dev/.local/bin/ay")),
                Some(1971),
            ),
            (
                "gh",
                atpkg::state::system(p("/opt/homebrew/bin/gh"), Some("2026-08-27")),
                None,
            ),
            ("codex", atpkg::state::agent_installing(), None),
            (
                "claude",
                atpkg::state::vendor_managed("2.1.280", "Anthropic"),
                Some(claude),
            ),
            (
                "vendorx",
                atpkg::state::extra_not_installed("vendorx"),
                None,
            ),
            ("clt", atpkg::state::needs_admin("clt"), None),
            (
                "brew",
                atpkg::state::blocked("clt", &atpkg::state::needs_admin("clt")),
                None,
            ),
            (
                "emacs",
                atpkg::state::unavailable("aarch64-pc-windows-msvc", "no Windows-on-ARM build"),
                None,
            ),
            (
                "xc",
                atpkg::state::installed_via("pkg", p("/opt/homebrew/bin/xc")),
                None,
            ),
            ("ny", "error: x".to_string(), None),
        ];
        let mut programs = std::collections::BTreeMap::new();
        for (name, state, build) in &fixture {
            programs.insert(
                (*name).to_string(),
                atpkg::ProgramStatus {
                    installed_build: *build,
                    state: state.clone(),
                    tree_root: String::new(),
                },
            );
        }
        let status = atpkg::Status {
            schema: 1,
            updated_at: "2026-08-27T00:00:00Z".to_string(),
            enabled: true,
            index_source: "alabsystems/aterm".to_string(),
            outcome: "up to date".to_string(),
            seams: Vec::new(),
            last_success_at: String::new(),
            last_index_reached_at: String::new(),
            last_index_build: 0,
            index_build_changed_at: String::new(),
            last_pass: String::new(),
            last_pass_at: String::new(),
            last_pass_attempted_index_build: 0,
            last_pass_attempted_at: String::new(),
            metered_hold_until: String::new(),
            programs,
            extra: Default::default(),
        };
        let text = status.to_toml().unwrap();
        let round: atpkg::Status = aterm_toml::from_str(&text).expect("the fixture is status.toml");
        let report = PackagesStatusReport::from_parts(true, true, "fp".into(), Some(&round), &[]);
        let row = |name: &str| {
            report
                .programs
                .iter()
                .find(|row| row.name == name)
                .unwrap_or_else(|| panic!("{name} row"))
        };
        // Verbatim text, every row.
        for (name, state, _) in &fixture {
            assert_eq!(
                &row(name).state,
                state,
                "{name} keeps the canonical spelling"
            );
        }
        assert_eq!(
            row("trust").kind,
            ProgramStateKind::Managed {
                build: 6808,
                index: 41
            }
        );
        assert_eq!(
            row("ay").kind,
            ProgramStateKind::Shadowed {
                build: 1971,
                path: "/Users//dev/.local/bin/ay".into()
            }
        );
        assert_eq!(
            row("gh").kind,
            ProgramStateKind::System {
                path: "/opt/homebrew/bin/gh".into(),
                retired: Some("2026-08-27".into())
            }
        );
        assert_eq!(row("codex").kind, ProgramStateKind::AgentInstalling);
        assert_eq!(
            row("claude").kind,
            ProgramStateKind::VendorLatest {
                version: "2.1.280".into(),
                vendor: "Anthropic".into()
            }
        );
        // The vendor rows a pass writes besides the latest, and a vendor build's shadow
        // row — which names the version and reads back as its store id.
        assert_eq!(
            ProgramStateKind::parse(&atpkg::state::vendor_kept("2.1.279", "held by local pin")),
            ProgramStateKind::VendorKept {
                version: "2.1.279".into(),
                why: "held by local pin".into()
            }
        );
        assert_eq!(
            ProgramStateKind::parse(&atpkg::state::shadowed(
                claude,
                p("/Users//dev/.local/bin/claude")
            )),
            ProgramStateKind::Shadowed {
                build: claude,
                path: "/Users//dev/.local/bin/claude".into()
            }
        );
        assert_eq!(row("vendorx").kind, ProgramStateKind::ExtraNotInstalled);
        assert_eq!(row("clt").kind, ProgramStateKind::NeedsAdmin);
        assert_eq!(
            row("brew").kind,
            ProgramStateKind::BlockedBy {
                dep: "clt".into(),
                dep_state: "needs admin — run: aterm pkg install clt".into()
            }
        );
        assert_eq!(row("emacs").kind, ProgramStateKind::Unavailable);
        assert_eq!(
            row("xc").kind,
            ProgramStateKind::InstalledVia {
                protocol: "pkg".into(),
                path: "/opt/homebrew/bin/xc".into()
            }
        );
        assert_eq!(row("ny").kind, ProgramStateKind::Other);

        // Grouping.
        for name in ["trust", "ay", "gh", "emacs", "xc", "ny"] {
            assert_eq!(row(name).group, RowGroup::Default, "{name}");
            assert!(row(name).facts.is_none(), "{name} carries no extra facts");
            assert!(!row(name).offers_extra_install(), "{name}");
        }
        // The agent programs are DEFAULT-SET (owner decision 2026-09-10): the pass
        // installs them unasked, so neither the one still installing nor the one
        // already managed is an extra, and neither offers an Install control.
        assert_eq!(
            row("codex").group,
            RowGroup::Default,
            "an agent program installing"
        );
        assert!(!row("codex").offers_extra_install());
        assert_eq!(
            row("claude").group,
            RowGroup::Default,
            "an agent program installed"
        );
        assert!(!row("claude").offers_extra_install());
        // A real opt-in extra still waits for consent behind the Install control.
        assert_eq!(row("vendorx").group, RowGroup::Extras);
        assert!(row("vendorx").offers_extra_install());
        assert!(
            row("vendorx").facts.is_none(),
            "an unauthored extra carries no facts"
        );
        assert_eq!(row("clt").group, RowGroup::NeedsAdmin);
        assert_eq!(
            row("brew").group,
            RowGroup::NeedsAdmin,
            "blocked behind a needs-admin dependency waits on the same door"
        );
        assert_eq!(
            report.needs_admin(),
            vec!["clt".to_string(), "brew".to_string()],
            "door order: the dependency first"
        );

        // The authored extras facts.
        let codex = row("codex").facts.clone().expect("codex facts");
        assert_eq!(codex.vendor, "OpenAI Codex CLI");
        assert_eq!(codex.license.as_deref(), Some("Apache-2.0"));
        // The authored fact moved to "~110 MB (~290 MB on disk)" in atpkg's
        // stub table (1f6d9332d) without this pin following it.
        assert_eq!(codex.size.as_deref(), Some("~110 MB (~290 MB on disk)"));
        assert_eq!(
            codex.line(),
            "OpenAI Codex CLI  ·  Apache-2.0  ·  ~110 MB (~290 MB on disk)"
        );
        let claude = row("claude").facts.clone().expect("claude facts");
        assert_eq!(claude.vendor, "Anthropic Claude Code");
        assert_eq!(claude.license.as_deref(), Some("proprietary"));
        // Same drift for Claude Code: the stub table says "~200 MB" now.
        assert_eq!(claude.size.as_deref(), Some("~200 MB"));
        let odd = ExtraFacts::parse("Some Tool without the grammar");
        assert_eq!(odd.vendor, "Some Tool without the grammar");
        assert_eq!(odd.license, None);
        assert_eq!(odd.size, None);

        // The projection carries the door order and the platform's door.
        let mut service = PackagesService::new();
        let seq = service.begin(None).unwrap();
        assert!(service.finish(seq, refresh(report)));
        let projection = service.state(true, true, true).projection();
        assert_eq!(
            projection.needs_admin,
            vec!["clt".to_string(), "brew".to_string()]
        );
        assert_eq!(projection.admin_door, cfg!(target_os = "macos"));
    }

    /// An opt-in marker regroups an installed extra whose name the compiled roster
    /// does not carry (a newer index) — and never touches a needs-admin row.
    #[test]
    fn opt_in_markers_regroup_installed_extras() {
        let mut programs = std::collections::BTreeMap::new();
        for (name, state) in [
            ("newtool", atpkg::state::managed(5, 41)),
            ("clt", atpkg::state::needs_admin("clt")),
        ] {
            programs.insert(
                name.to_string(),
                atpkg::ProgramStatus {
                    installed_build: None,
                    state,
                    tree_root: String::new(),
                },
            );
        }
        let status = atpkg::Status {
            schema: 1,
            programs,
            ..Default::default()
        };
        let mut report =
            PackagesStatusReport::from_parts(true, true, "fp".into(), Some(&status), &[]);
        assert_eq!(report.programs[1].group, RowGroup::Default);
        report.mark_extras(["newtool", "clt", "absent"]);
        let newtool = report
            .programs
            .iter()
            .find(|r| r.name == "newtool")
            .unwrap();
        assert_eq!(newtool.group, RowGroup::Extras);
        assert!(
            newtool.facts.is_none(),
            "no authored line for a name the binary predates"
        );
        let clt = report.programs.iter().find(|r| r.name == "clt").unwrap();
        assert_eq!(
            clt.group,
            RowGroup::NeedsAdmin,
            "the door outranks the roster"
        );
    }

    /// The needs-admin order over chains: depth first, then name; a cycle, a dangling
    /// chain and a chain that ends anywhere but `needs admin` are not admin-waiting.
    #[test]
    fn needs_admin_order_walks_blocked_chains_dependency_first() {
        let s = |n: &str, st: String| (n.to_string(), st);
        let na = atpkg::state::needs_admin;
        let rows = vec![
            s("brew", atpkg::state::blocked("clt", &na("clt"))),
            s("clt", na("clt")),
            s(
                "cask",
                atpkg::state::blocked("brew", &atpkg::state::blocked("clt", &na("clt"))),
            ),
            s(
                "ay",
                atpkg::state::blocked("codex", &atpkg::state::extra_not_installed("codex")),
            ),
            s("codex", atpkg::state::extra_not_installed("codex")),
            s(
                "loop-a",
                atpkg::state::blocked("loop-b", "blocked by loop-a: x"),
            ),
            s(
                "loop-b",
                atpkg::state::blocked("loop-a", "blocked by loop-b: x"),
            ),
            s("dangling", atpkg::state::blocked("ghost", "not installed")),
            s("apt-thing", na("apt-thing")),
            s("trust", atpkg::state::managed(1, 1)),
        ];
        assert_eq!(
            needs_admin_order(&rows),
            vec![
                "apt-thing".to_string(),
                "clt".to_string(),
                "brew".to_string(),
                "cask".to_string()
            ]
        );
        // A quoted tail stands in when the dependency has no row of its own.
        let orphan = vec![s("brew", atpkg::state::blocked("clt", &na("clt")))];
        assert_eq!(needs_admin_order(&orphan), vec!["brew".to_string()]);
        assert!(needs_admin_order(&[]).is_empty());
    }

    /// The dismissal rule: never dismissed ⇒ show; the recorded set ⇒ silent; a
    /// changed set (one more name, one fewer, a different order) ⇒ show again; an
    /// empty set is never a card. The marker round-trips through the file helpers.
    #[test]
    fn the_admin_step_is_raised_once_per_distinct_set() {
        let both = vec!["clt".to_string(), "brew".to_string()];
        let only_clt = vec!["clt".to_string()];
        assert!(admin_step_should_show(None, &both));
        assert!(!admin_step_should_show(None, &[]));
        let recorded = admin_step_marker_text(&both);
        assert_eq!(recorded, "clt\nbrew");
        assert!(!admin_step_should_show(Some(&recorded), &both));
        assert!(
            !admin_step_should_show(Some("clt\nbrew\n"), &both),
            "a trailing newline is the same record"
        );
        assert!(admin_step_should_show(Some(&recorded), &only_clt));
        assert!(admin_step_should_show(
            Some(&admin_step_marker_text(&only_clt)),
            &both
        ));
        assert!(
            admin_step_should_show(Some("brew\nclt"), &both),
            "order is part of the set"
        );
        assert!(!admin_step_should_show(Some(&recorded), &[]));

        let dir = std::env::temp_dir().join(format!(
            "aterm-admin-step-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let config = dir.join("aterm.toml");
        assert_eq!(
            admin_step_marker_path(Some(&config)),
            Some(dir.join(ADMIN_STEP_MARKER))
        );
        assert_eq!(admin_step_marker_path(None), None);
        let mut programs = std::collections::BTreeMap::new();
        for (name, state) in [
            ("clt", atpkg::state::needs_admin("clt")),
            (
                "brew",
                atpkg::state::blocked("clt", &atpkg::state::needs_admin("clt")),
            ),
            ("trust", atpkg::state::managed(1, 1)),
        ] {
            programs.insert(
                name.to_string(),
                atpkg::ProgramStatus {
                    installed_build: None,
                    state,
                    tree_root: String::new(),
                },
            );
        }
        let status = atpkg::Status {
            schema: 1,
            programs,
            ..Default::default()
        };
        assert_eq!(admin_step_due(&status, Some(&config)), Some(both.clone()));
        record_admin_step_dismissal(Some(&config), &both).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join(ADMIN_STEP_MARKER)).unwrap(),
            "clt\nbrew"
        );
        assert_eq!(
            admin_step_due(&status, Some(&config)),
            None,
            "declined: silent"
        );
        // clt installs; only brew still waits — a NEW set, so the card is due again.
        let mut moved = status.clone();
        moved.programs.get_mut("clt").unwrap().state =
            atpkg::state::installed_via("softwareupdate", std::path::Path::new("/L/git"));
        moved.programs.get_mut("brew").unwrap().state = atpkg::state::needs_admin("brew");
        assert_eq!(
            admin_step_due(&moved, Some(&config)),
            Some(vec!["brew".to_string()])
        );
        // Nothing waits ⇒ nothing is due, marker or not.
        let mut done = moved.clone();
        done.programs.get_mut("brew").unwrap().state =
            atpkg::state::installed_via("pkg", std::path::Path::new("/opt/homebrew/bin/brew"));
        assert_eq!(admin_step_due(&done, Some(&config)), None);
        assert!(record_admin_step_dismissal(None, &both).is_err());

        // THE MARKER IS A PREFIX MARKER: a re-record replaces the regular file in place
        // (no temp file left behind); an oversized file is not a record; a planted
        // link is neither read nor written through and is left exactly as planted; a
        // directory at the path fails the write closed.
        let marker = dir.join(ADMIN_STEP_MARKER);
        record_admin_step_dismissal(Some(&config), &only_clt).unwrap();
        assert_eq!(read_admin_step_marker(&marker).as_deref(), Some("clt"));
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "the exclusive sibling is renamed away, never left beside the marker"
        );
        assert_eq!(admin_step_due(&status, Some(&config)), Some(both.clone()));
        std::fs::write(&marker, "x".repeat(MAX_ADMIN_STEP_MARKER_BYTES + 1)).unwrap();
        assert_eq!(
            read_admin_step_marker(&marker),
            None,
            "oversized: not a record"
        );
        assert_eq!(admin_step_due(&status, Some(&config)), Some(both.clone()));
        assert!(
            record_admin_step_dismissal(Some(&config), &vec!["x".repeat(65); 80]).is_err(),
            "a set too large for a marker is refused, not truncated"
        );
        std::fs::remove_file(&marker).unwrap();
        #[cfg(unix)]
        {
            let target = dir.join("planted-target");
            std::fs::write(&target, admin_step_marker_text(&both)).unwrap();
            std::os::unix::fs::symlink(&target, &marker).unwrap();
            assert_eq!(
                read_admin_step_marker(&marker),
                None,
                "a link whose target spells the set is still not a record"
            );
            assert_eq!(admin_step_due(&status, Some(&config)), Some(both.clone()));
            let refused = record_admin_step_dismissal(Some(&config), &both).unwrap_err();
            assert_eq!(refused.kind(), std::io::ErrorKind::AlreadyExists);
            assert!(
                std::fs::symlink_metadata(&marker)
                    .unwrap()
                    .file_type()
                    .is_symlink(),
                "the planted link is left alone"
            );
            assert_eq!(
                std::fs::read_to_string(&target).unwrap(),
                admin_step_marker_text(&both),
                "nothing was written through the link"
            );
            std::fs::remove_file(&marker).unwrap();
            std::fs::remove_file(&target).unwrap();
        }
        std::fs::create_dir(&marker).unwrap();
        assert_eq!(read_admin_step_marker(&marker), None);
        assert!(record_admin_step_dismissal(Some(&config), &both).is_err());
        assert!(std::fs::symlink_metadata(&marker).unwrap().is_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Each vendor's line names who installs it, honestly (the admin-step row's
    /// detail lines, `message_reporters::admin_step`).
    #[test]
    fn the_admin_vendor_lines_name_the_installers() {
        assert_eq!(
            admin_vendor_line("clt"),
            "Apple Command Line Tools (Apple's installer, via softwareupdate)"
        );
        assert_eq!(
            admin_vendor_line("brew"),
            "Homebrew (its signed installer package)"
        );
        assert_eq!(admin_vendor_line("newpkg"), "newpkg (its own installer)");
    }

    /// Dev-linked programs surface annotated — merged into an existing status
    /// row when atpkg recorded one, appended as `linked` when it never did —
    /// and the row list stays name-sorted for stable paint.
    #[test]
    fn report_merges_dev_links_into_the_program_rows() {
        let links = vec![
            (
                "ay".to_string(),
                Some(std::path::PathBuf::from("/Users//x/ay")),
            ),
            ("orc".to_string(), None),
        ];
        let report =
            PackagesStatusReport::from_parts(true, true, "fp".into(), Some(&status("ok")), &links);
        assert_eq!(report.programs.len(), 2);
        let ay = report.programs.iter().find(|row| row.name == "ay").unwrap();
        assert_eq!(ay.installed_build, Some(1971), "status row kept");
        assert_eq!(ay.annotation.as_deref(), Some("dev-link → /Users//x/ay"));
        let orc = report
            .programs
            .iter()
            .find(|row| row.name == "orc")
            .unwrap();
        assert_eq!(orc.state, "linked");
        assert_eq!(orc.installed_build, None);
        assert!(report.programs.windows(2).all(|w| w[0].name <= w[1].name));
    }

    /// A hostile/corrupt metadata source settles the worker into an explicit,
    /// non-busy error projection; it can never strand Packages on “Reading”.
    #[test]
    fn metadata_admission_error_completes_into_visible_nonreading_state() {
        let mut service = PackagesService::new();
        let sequence = service.begin(None).unwrap();
        let mut report = PackagesStatusReport::from_parts(true, true, "fp".into(), None, &[]);
        report.collection_error = Some(
            "Could not read status.toml: package metadata is not a regular non-link file"
                .to_string(),
        );
        assert!(service.finish(sequence, refresh(report)));

        let projection = service.state(true, true, true).projection();
        assert_eq!(
            projection.headline,
            "Some package details couldn\u{2019}t be read"
        );
        assert!(!projection.refreshing);
        assert!(!projection.actions_enabled);
        assert!(
            projection
                .detail
                .as_deref()
                .unwrap_or("")
                .contains("regular non-link"),
            "the checked-read diagnostic reaches the native Settings projection"
        );
    }

    /// SHADOWED IS READ, NOT RECORDED (Phase 3): atpkg records the managed row, and the
    /// page reads a program as shadowed against the login shell's PATH it is handed — the
    /// same row a record from an older atpkg carried, parsed the same way.
    #[cfg(unix)]
    #[test]
    fn the_collector_reads_shadowing_against_the_path_it_is_handed() {
        use std::os::unix::fs::PermissionsExt as _;
        let root =
            std::env::temp_dir().join(format!("aterm-packages-shadow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let layout = atpkg::Layout {
            prefix: root.join("pkg"),
        };
        let build = layout.build_dir("ay", 18);
        std::fs::create_dir_all(build.join("bin")).unwrap();
        std::fs::write(build.join("bin/ay"), b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(build.join("bin/ay"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        atpkg::activate::install_shims(
            &layout,
            &build,
            &["ay".to_string()],
            atpkg::activate::Aliases::Off,
        )
        .unwrap();
        atpkg::store::mark_build_ready(&build).unwrap();
        let mut programs = std::collections::BTreeMap::new();
        programs.insert(
            "ay".to_string(),
            atpkg::ProgramStatus {
                installed_build: Some(18),
                state: atpkg::state::managed(18, 41),
                tree_root: String::new(),
            },
        );
        atpkg::status::write(
            &layout,
            &atpkg::Status {
                schema: 1,
                programs,
                ..Default::default()
            },
        )
        .unwrap();
        let foreign = root.join("foreign");
        std::fs::create_dir_all(&foreign).unwrap();
        std::fs::write(foreign.join("ay"), b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(foreign.join("ay"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let row = |path: &std::ffi::OsStr| {
            collect_packages_status_from_layout(true, Some(&layout), Some(path))
                .programs
                .into_iter()
                .find(|r| r.name == "ay")
                .unwrap()
        };
        let ahead = std::env::join_paths([foreign.clone(), layout.bin_dir()]).unwrap();
        assert!(
            matches!(
                row(&ahead).kind,
                ProgramStateKind::Shadowed { build: 18, .. }
            ),
            "{:?}",
            row(&ahead).state
        );
        let behind = std::env::join_paths([layout.bin_dir(), foreign]).unwrap();
        assert!(matches!(
            row(&behind).kind,
            ProgramStateKind::Managed {
                build: 18,
                index: 41
            }
        ));
        assert_eq!(
            atpkg::status::read(&layout).unwrap().programs["ay"].state,
            atpkg::state::managed(18, 41),
            "the record itself is never touched by a read"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn real_collector_turns_status_fifo_into_a_completed_error_report() {
        use std::os::unix::ffi::OsStrExt as _;

        let prefix =
            std::env::temp_dir().join(format!("aterm-packages-fifo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&prefix);
        std::fs::create_dir_all(&prefix).unwrap();
        let layout = atpkg::Layout {
            prefix: prefix.clone(),
        };
        let status = layout.status();
        let status_c = std::ffi::CString::new(status.as_os_str().as_bytes()).unwrap();
        // SAFETY: `status_c` is a live NUL-terminated path in our private fixture.
        assert_eq!(unsafe { libc::mkfifo(status_c.as_ptr(), 0o600) }, 0);

        let report = collect_packages_status_from_layout(true, Some(&layout), None);
        assert!(
            report
                .collection_error
                .as_deref()
                .unwrap_or("")
                .contains("regular non-link"),
            "the production collector surfaces the FIFO refusal"
        );
        let mut service = PackagesService::new();
        let sequence = service.begin(None).unwrap();
        assert!(service.finish(sequence, refresh(report)));
        let projection = service.state(true, true, true).projection();
        assert!(!projection.refreshing);
        assert!(!projection.headline.contains("Reading"));
        let _ = std::fs::remove_dir_all(prefix);
    }

    fn machine_state(
        uc: atpkg::machine::UcPosture,
        would_migrate: usize,
        home: atpkg::machine::HomePosture,
    ) -> atpkg::machine::MachineState {
        atpkg::machine::MachineState {
            universal_control: uc,
            policy: atpkg::config::UniversalControlPolicy::Off,
            spotlight_noindex: true,
            exposed: would_migrate + 1,
            hidden: 8,
            would_migrate,
            scan_complete: true,
            home,
            config_unreadable: false,
        }
    }

    /// THE PASS'S OWN RECORD ANSWERS THE CHANGE (2026-09-16). A `machine-settings:`
    /// line opens an expectation — the card reads as refreshing, Apply disabled — and
    /// NO read is spawned; the `machine-state:` record the pass prints behind it is
    /// the newest measurement and closes the expectation. A read that was running
    /// meanwhile is superseded: its older answer is dropped on arrival, so a walk that
    /// began before the apply can never overwrite the state the apply reported. A
    /// stream that ends without a record hands the read back to the caller.
    #[test]
    fn a_passes_record_answers_its_change_without_a_read() {
        use atpkg::machine::{HomePosture, UcPosture};
        let now = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let mut service = PackagesService::new();
        // Unobserved, a change arrives with its record behind it.
        service.note_machine_change("universal-control disabled".into(), now);
        service.expect_machine_record();
        let waiting = service.state(true, true, true).projection();
        assert!(waiting.machine.refreshing, "the card reads as refreshing");
        assert!(!waiting.machine.apply_enabled);
        assert!(service.machine().awaiting_record && !service.machine().refreshing);
        service.note_machine_record(machine_state(UcPosture::Disabled, 0, HomePosture::Account));
        let m = service.machine();
        assert!(m.observed && !m.awaiting_record && !m.refreshing);
        assert_eq!(
            m.state.as_ref().map(|s| s.universal_control),
            Some(UcPosture::Disabled)
        );
        assert!(!service.take_record_expectation(), "nothing left to answer");

        // A read running when the record lands is superseded: its answer is dropped.
        assert!(service.request_machine_read(), "a read is spawned");
        assert!(
            !service.request_machine_read(),
            "…and a second queues a rerun"
        );
        service.note_machine_record(machine_state(UcPosture::Disabled, 2, HomePosture::Account));
        assert!(service.machine().superseded);
        assert!(
            !service.machine().rerun,
            "the record satisfies the queued rerun"
        );
        let rerun = service.replace_machine_state(Ok(machine_state(
            UcPosture::Default,
            0,
            HomePosture::Account,
        )));
        assert!(!rerun);
        let m = service.machine();
        assert!(!m.refreshing && !m.superseded);
        assert_eq!(
            m.state
                .as_ref()
                .map(|s| (s.universal_control, s.would_migrate)),
            Some((UcPosture::Disabled, 2)),
            "the older read must not overwrite the pass's record"
        );

        // A change whose record never comes: the expectation is handed back exactly
        // once, and the caller spawns the read.
        service.expect_machine_record();
        assert!(service.take_record_expectation());
        assert!(!service.take_record_expectation());
        assert!(!service.machine().awaiting_record);

        // A read that STARTED before the change cannot answer for it: its completion
        // leaves the expectation open; a read started after the change closes it.
        assert!(service.request_machine_read(), "a read is running…");
        service.expect_machine_record();
        assert!(!service.replace_machine_state(Ok(machine_state(
            UcPosture::Default,
            1,
            HomePosture::Account,
        ))));
        assert!(
            service.machine().awaiting_record,
            "a pre-change read measured the machine the change then changed"
        );
        assert!(
            service.request_machine_read(),
            "…and one started afterwards"
        );
        assert!(!service.replace_machine_state(Ok(machine_state(
            UcPosture::Disabled,
            0,
            HomePosture::Account,
        ))));
        assert!(!service.machine().awaiting_record, "answers it");
        // The same through a queued rerun: the rerun is spawned after the change, so
        // its answer closes the expectation the first read could not.
        assert!(service.request_machine_read());
        assert!(!service.request_machine_read(), "queued behind it");
        service.expect_machine_record();
        assert!(
            service.replace_machine_state(Err("first".into())),
            "the rerun is handed back"
        );
        assert!(
            service.machine().awaiting_record,
            "the first read predates the change"
        );
        assert!(!service.replace_machine_state(Ok(machine_state(
            UcPosture::Disabled,
            0,
            HomePosture::Account,
        ))));
        assert!(!service.machine().awaiting_record, "the rerun answers it");

        // A `machine apply` completion carrying the apply's own record confirms the
        // card the way a streamed record does.
        let seq = service.begin(Some(PackagesBusy::MachineApply)).unwrap();
        service.expect_machine_record();
        let done = succeeded(
            PackagesStatusReport::unobserved(),
            PackagesBusy::MachineApply,
        )
        .with_machine_state(Some(machine_state(
            UcPosture::Disabled,
            3,
            HomePosture::Account,
        )));
        assert!(
            !service.finish(1_000_000, done.clone()),
            "a stale sequence is inert"
        );
        assert!(service.finish(seq, done));
        let m = service.machine();
        assert!(!m.awaiting_record);
        assert_eq!(m.state.as_ref().map(|s| s.would_migrate), Some(3));
    }

    /// The "This Mac" card reads the machine posture through the packages
    /// projection: unobserved says nothing and offers no button; a read bumps the
    /// revision and renders the measured words; `Apply now` follows
    /// `MachineState::next` (a home mismatch or nothing-left disables it); a
    /// recorded change and an apply verdict are quoted with their age.
    #[test]
    fn projection_carries_the_machine_posture() {
        use atpkg::machine::{HomePosture, UcPosture};
        let mut service = PackagesService::new();
        let base = service.revision();
        let unobserved = service.state(true, true, true).projection();
        assert!(!unobserved.machine.observed);
        assert!(!unobserved.machine.apply_enabled);
        assert!(!unobserved.machine.nothing_to_apply);
        assert_eq!(
            unobserved.machine.universal_control,
            "Universal Control: not read yet"
        );
        assert_eq!(
            unobserved.machine.last_change,
            "No change recorded this launch"
        );
        assert_eq!(unobserved.machine.supported, cfg!(target_os = "macos"));

        // The page must be observed and idle before any verb — the machine card
        // shares that gate, so seed a live report first.
        let report =
            PackagesStatusReport::from_parts(true, true, "fp".into(), Some(&status("ok")), &[]);
        let seq = service.begin(None).unwrap();
        assert!(service.finish(seq, refresh(report)));

        service.set_machine_refreshing(true);
        let refreshing = service.state(true, true, true).projection();
        assert!(refreshing.machine.refreshing);
        assert!(!refreshing.machine.apply_enabled);

        let before = service.revision();
        let _ = service.replace_machine_state(Ok(machine_state(
            UcPosture::Default,
            2,
            HomePosture::Account,
        )));
        assert!(service.revision() > before, "a read fans out");
        assert!(!service.machine().refreshing, "a read ends the refresh");
        let now = std::time::SystemTime::now();
        let live = service.state(true, true, true).projection_at(now);
        assert!(live.machine.observed);
        assert_eq!(
            live.machine.universal_control,
            "Universal Control: at the OS default — the cursor roams to other Macs and iPads"
        );
        assert_eq!(
            live.machine.spotlight,
            "Build output: 8 target dirs hidden, 3 open to Spotlight — 2 a pass would hide"
        );
        assert!(live.machine.reason.is_none());
        assert_eq!(live.machine.apply_enabled, cfg!(target_os = "macos"));
        assert!(!live.machine.nothing_to_apply);
        assert_eq!(
            live.machine.next,
            "Apply now would set: Universal Control off for this host; 2 target dir(s) hidden from Spotlight"
        );
        assert!(live.machine.saved.is_none(), "saved == measured ⇒ no note");
        // A switch flipped in Settings after the read is named, never shown as
        // already measured.
        let flipped = service
            .state(true, true, true)
            .with_machine_config(atpkg::config::UniversalControlPolicy::Leave, false)
            .projection_at(now);
        assert_eq!(
            flipped.machine.saved.as_deref(),
            Some(
                "Saved since the last read: universal_control = \"leave\", spotlight_noindex = false — the next package pass or Apply now uses it"
            )
        );

        // A recorded change carries its age; a verdict is quoted verbatim.
        service.note_machine_change(
            "universal-control disabled".to_string(),
            now - std::time::Duration::from_secs(120),
        );
        let seq = service.begin(Some(PackagesBusy::MachineApply)).unwrap();
        assert!(
            service.finish(
                seq,
                succeeded(
                    PackagesStatusReport::from_parts(
                        true,
                        true,
                        "fp".into(),
                        Some(&status("ok")),
                        &[]
                    ),
                    PackagesBusy::MachineApply,
                )
                .with_machine_verdict(Some("applied — universal-control disabled".to_string())),
            )
        );
        let changed = service.state(true, true, true).projection_at(now);
        assert_eq!(
            changed.machine.last_change,
            "Last change: universal-control disabled · 2m ago"
        );
        assert_eq!(
            changed.machine.last_verdict.as_deref(),
            Some("applied — universal-control disabled")
        );
        assert_eq!(changed.headline, "Machine settings applied");
        assert_eq!(
            changed.command_feedback.as_deref(),
            Some("Machine settings: applied — universal-control disabled")
        );

        // Nothing left to do: the button is off and the card says so.
        let _ = service.replace_machine_state(Ok(machine_state(
            UcPosture::Disabled,
            0,
            HomePosture::Account,
        )));
        let done = service.state(true, true, true).projection_at(now);
        assert_eq!(
            done.machine.universal_control,
            "Universal Control: disabled on this Mac"
        );
        assert!(!done.machine.apply_enabled);
        assert!(done.machine.nothing_to_apply);
        // The fixture leaves one exposed dir no pass can reach (`exposed =
        // would_migrate + 1`), so the honest sentence is the unreachable one: nothing
        // an APPLY can do, which is not the same as the machine being settled.
        assert_eq!(
            done.machine.next,
            "Nothing an apply can do — 1 target dir(s) stay open to Spotlight \
             (free-standing, or refused for now)",
        );

        // A home mismatch disables the button and names the reason.
        let _ = service.replace_machine_state(Ok(machine_state(
            UcPosture::Default,
            2,
            HomePosture::Mismatch,
        )));
        let mismatch = service.state(true, true, true).projection_at(now);
        assert!(!mismatch.machine.apply_enabled);
        assert!(
            !mismatch.machine.nothing_to_apply,
            "a mismatch is not \"nothing to apply\""
        );
        assert!(mismatch.machine.next.is_empty());
        assert!(
            mismatch
                .machine
                .reason
                .as_deref()
                .is_some_and(|r| r.starts_with("Not applied here")),
            "{:?}",
            mismatch.machine.reason
        );

        // With the switch OFF a pass hides nothing, whatever the scan counted: the
        // sentence prints what an apply CAN hide (the CLI's `can_hide`), and with
        // the saved switch off too there is nothing left to apply.
        let _ = service.replace_machine_state(Ok(atpkg::machine::MachineState {
            spotlight_noindex: false,
            ..machine_state(UcPosture::Disabled, 2, HomePosture::Account)
        }));
        let switched_off = service
            .state(true, true, true)
            .with_machine_config(atpkg::config::UniversalControlPolicy::Off, false)
            .projection_at(now);
        assert_eq!(
            switched_off.machine.spotlight,
            "Build output: 8 target dirs hidden, 3 open to Spotlight — 0 a pass would hide · switched off ([machine] spotlight_noindex = false)"
        );
        assert!(switched_off.machine.nothing_to_apply);
        assert!(
            switched_off.machine.next.starts_with("Nothing to apply"),
            "{}",
            switched_off.machine.next
        );
        assert!(switched_off.machine.saved.is_none());

        // A read error AFTER a good record: observed, the last record is kept but
        // every measured sentence says it is prior, the verdict line is silent and
        // the button is off — a stale record never stands in for a measured one.
        let _ = service.replace_machine_state(Ok(machine_state(
            UcPosture::Default,
            2,
            HomePosture::Account,
        )));
        let _ = service.replace_machine_state(Err("atpkg machine printed no state".to_string()));
        let errored = service.state(true, true, true).projection_at(now);
        assert!(errored.machine.observed);
        assert!(!errored.machine.apply_enabled);
        assert!(!errored.machine.nothing_to_apply);
        assert!(errored.machine.next.is_empty(), "{}", errored.machine.next);
        assert_eq!(
            errored.machine.reason.as_deref(),
            Some("Could not read the machine state: atpkg machine printed no state")
        );
        assert!(
            errored
                .machine
                .universal_control
                .ends_with("(from the last successful read)"),
            "{}",
            errored.machine.universal_control
        );
        assert!(
            errored
                .machine
                .spotlight
                .ends_with("(from the last successful read)"),
            "{}",
            errored.machine.spotlight
        );
        assert!(
            service.machine().state.is_some(),
            "a failed read never erases the last measurement"
        );
        let _ = base;
    }

    /// `atpkg machine apply` is a LOCAL verb: the card's Apply now must not inherit
    /// the package page's manager gate. With the manager inert (no pinned root key)
    /// or the status collection torn, the
    /// package verbs are off but the machine verdict and its button stay live.
    #[test]
    fn machine_apply_ignores_the_manager_gate() {
        use atpkg::machine::{HomePosture, UcPosture};
        let manager_off =
            PackagesStatusReport::from_parts(true, false, "fp".into(), Some(&status("ok")), &[]);
        let mut torn =
            PackagesStatusReport::from_parts(true, true, "fp".into(), Some(&status("ok")), &[]);
        torn.collection_error = Some("status.toml: torn record".to_string());
        for (report, why) in [(manager_off, "manager off"), (torn, "collection error")] {
            let mut service = PackagesService::new();
            let seq = service.begin(None).unwrap();
            assert!(service.finish(seq, refresh(report)));
            let _ = service.replace_machine_state(Ok(machine_state(
                UcPosture::Default,
                2,
                HomePosture::Account,
            )));
            let p = service.state(true, true, true).projection();
            assert!(!p.actions_enabled, "{why}: the package verbs are gated");
            assert_eq!(
                p.machine.apply_enabled,
                cfg!(target_os = "macos"),
                "{why}: the machine verb is not"
            );
            assert!(!p.machine.nothing_to_apply, "{why}");
            assert!(
                p.machine.next.starts_with("Apply now would set"),
                "{why}: {}",
                p.machine.next
            );
        }
    }

    /// The apply child reads the SAVED `[machine]` switches, so the verdict and the
    /// button follow measured posture × saved switches — never the record's stale
    /// `policy=`/`noindex=` — in both directions.
    #[test]
    fn flipped_machine_switch_drives_the_apply_verdict_not_the_stale_record() {
        use atpkg::config::UniversalControlPolicy;
        use atpkg::machine::{HomePosture, UcPosture};
        let mut service = PackagesService::new();
        let seq = service.begin(None).unwrap();
        assert!(service.finish(
            seq,
            refresh(PackagesStatusReport::from_parts(
                true,
                true,
                "fp".into(),
                Some(&status("ok")),
                &[]
            )),
        ));
        // Direction B: the record says the pass would act; the user then saved
        // "leave" and switched Spotlight off — an apply would now do nothing.
        let _ = service.replace_machine_state(Ok(machine_state(
            UcPosture::Default,
            2,
            HomePosture::Account,
        )));
        let b = service
            .state(true, true, true)
            .with_machine_config(UniversalControlPolicy::Leave, false)
            .projection();
        assert!(!b.machine.apply_enabled);
        assert!(b.machine.nothing_to_apply);
        assert!(
            b.machine.next.starts_with("Nothing an apply can do"),
            "{}",
            b.machine.next
        );
        assert!(
            b.machine
                .saved
                .as_deref()
                .is_some_and(|s| s.contains("universal_control = \"leave\"")
                    && s.contains("spotlight_noindex = false")),
            "{:?}",
            b.machine.saved
        );
        // Direction A: the record was read under leave/off; the user then saved
        // "off" and switched Spotlight on — an apply WOULD act, so say so.
        let _ = service.replace_machine_state(Ok(atpkg::machine::MachineState {
            policy: UniversalControlPolicy::Leave,
            spotlight_noindex: false,
            ..machine_state(UcPosture::Default, 2, HomePosture::Account)
        }));
        let a = service
            .state(true, true, true)
            .with_machine_config(UniversalControlPolicy::Off, true)
            .projection();
        assert_eq!(a.machine.apply_enabled, cfg!(target_os = "macos"));
        assert!(!a.machine.nothing_to_apply);
        assert_eq!(
            a.machine.next,
            "Apply now would set: Universal Control off for this host; 2 target dir(s) hidden from Spotlight"
        );
        assert!(
            a.machine
                .saved
                .as_deref()
                .is_some_and(|s| s.contains("universal_control = \"off\"")
                    && s.contains("spotlight_noindex = true")),
            "{:?}",
            a.machine.saved
        );
    }

    /// A read asked for while one is running is QUEUED behind it, never joined:
    /// the running read may have started before an apply and would land as a
    /// pre-apply record. The posture stays refreshing (Apply off, "Reading…")
    /// until the rerun lands.
    #[test]
    fn a_read_in_flight_when_the_apply_finishes_is_rerun_not_joined() {
        use atpkg::machine::{HomePosture, UcPosture};
        let report =
            || PackagesStatusReport::from_parts(true, true, "fp".into(), Some(&status("ok")), &[]);
        let mut service = PackagesService::new();
        let seq = service.begin(None).unwrap();
        assert!(service.finish(seq, refresh(report())));
        assert!(service.request_machine_read(), "idle ⇒ the caller spawns");
        assert!(service.machine().refreshing);
        assert!(!service.machine().rerun);

        // An apply runs and finishes while that read is still out.
        let seq = service.begin(Some(PackagesBusy::MachineApply)).unwrap();
        assert!(
            service.finish(
                seq,
                succeeded(report(), PackagesBusy::MachineApply)
                    .with_machine_verdict(Some("applied — universal-control disabled".to_string())),
            )
        );
        assert!(
            !service.request_machine_read(),
            "a read in flight is queued, not joined"
        );
        assert!(service.machine().refreshing);
        assert!(service.machine().rerun);

        // The STALE read lands: the rerun is handed back to the caller, and the
        // card keeps reading — the stale record never enables Apply.
        assert!(service.replace_machine_state(Ok(machine_state(
            UcPosture::Default,
            2,
            HomePosture::Account,
        ))));
        assert!(!service.machine().rerun, "handed back exactly once");
        let stale = service.state(true, true, true).projection();
        assert!(stale.machine.observed);
        assert!(stale.machine.refreshing);
        assert!(!stale.machine.apply_enabled);

        // The rerun lands: fresh record, nothing pending, the verdict is final.
        assert!(!service.replace_machine_state(Ok(machine_state(
            UcPosture::Disabled,
            0,
            HomePosture::Account,
        ))));
        let fresh = service.state(true, true, true).projection();
        assert!(!fresh.machine.refreshing);
        assert!(fresh.machine.nothing_to_apply);
        assert!(!fresh.machine.apply_enabled);
    }

    /// Every posture word the card can say, pinned from the record: the two
    /// half/unknown Universal Control states, the `leave` suffix (and its absence
    /// once disabled), the scan-budget suffix, the switched-off Spotlight sentence
    /// and the unresolved-home reason — with the verdict each one yields.
    ///
    /// THIS TEST'S BODY WAS DELETED AND ITS `#[test]` LEFT BEHIND, which landed the
    /// attribute on the next test's doc; `duplicated attribute` named it 2026-09-16.
    /// The law above is NOT withdrawn — the posture words it pins are still the ones
    /// the card says — but restoring it means writing a body against the card's render,
    /// not moving a doc, so it is recorded here and owed rather than faked. Its sibling
    /// in `native_config_language.rs`, lost the same way, WAS restorable and is back.
    ///
    /// A CHANGE A PASS REPORTED REACHES THE CARD, and asks for a fresh read.
    ///
    /// The launch one-shot and every pass stream `machine-settings:` to the window, and
    /// the wake arm answers by noting the change and starting a re-read. Both halves
    /// were untested: a rework that dropped the note would leave the card's "Last
    /// change" empty forever, and one that dropped the re-read would leave the measured
    /// lines describing a machine the change had already moved.
    #[test]
    fn a_reported_change_lands_on_the_card_and_asks_for_a_read() {
        use atpkg::machine::{HomePosture, UcPosture};
        let mut service = PackagesService::new();
        let seq = service.begin(None).unwrap();
        assert!(service.finish(
            seq,
            refresh(PackagesStatusReport::from_parts(
                true,
                true,
                "fp".into(),
                Some(&status("ok")),
                &[]
            )),
        ));
        let _ = service.replace_machine_state(Ok(machine_state(
            UcPosture::Default,
            1,
            HomePosture::Account,
        )));
        let before = service.state(true, true, true).projection().machine;
        assert_eq!(before.last_change, "No change recorded this launch");

        let at = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000);
        service.note_machine_change("universal-control disabled".to_string(), at);
        let after = service
            .state(true, true, true)
            .projection_at(at + std::time::Duration::from_secs(24))
            .machine;
        assert!(
            after
                .last_change
                .starts_with("Last change: universal-control disabled · "),
            "{}",
            after.last_change
        );
        // And a re-read can be asked for: the arm's other half.
        assert!(
            service.request_machine_read(),
            "a read starts when none is in flight"
        );
        assert!(
            !service.request_machine_read(),
            "a second request queues behind it rather than starting a second worker"
        );
    }

    /// A CONFIG THAT DOES NOT PARSE IS A REFUSAL, NOT A MEASUREMENT. Both `[machine]`
    /// defaults act, so the card must not offer Apply — and must say which file to fix —
    /// when the record reports that the switches could not be read.
    #[test]
    fn an_unreadable_config_is_said_on_the_card_and_disables_apply() {
        use atpkg::config::UniversalControlPolicy::Off;
        use atpkg::machine::{HomePosture, MachineState, UcPosture};
        let mut service = PackagesService::new();
        let seq = service.begin(None).unwrap();
        assert!(service.finish(
            seq,
            refresh(PackagesStatusReport::from_parts(
                true,
                true,
                "fp".into(),
                Some(&status("ok")),
                &[]
            )),
        ));
        let _ = service.replace_machine_state(Ok(MachineState {
            config_unreadable: true,
            ..machine_state(UcPosture::Default, 3, HomePosture::Account)
        }));
        let projection = service
            .state(true, true, true)
            .with_machine_config(Off, true)
            .projection()
            .machine;
        let reason = projection.reason.as_deref().expect("a refusal is said");
        assert!(reason.contains("aterm.toml does not parse"), "{reason}");
        assert!(
            projection.next.is_empty(),
            "nothing is next when nothing may be applied: {projection:?}"
        );
        assert!(
            !projection.apply_enabled,
            "Apply must be off: {projection:?}"
        );
        assert!(
            !projection.nothing_to_apply,
            "a refusal is not the same as being already applied: {projection:?}"
        );
    }

    #[test]
    fn machine_projection_words_cover_every_posture() {
        use atpkg::config::UniversalControlPolicy::{self, Leave, Off};
        use atpkg::machine::{HomePosture, MachineState, UcPosture};
        struct Row {
            name: &'static str,
            state: MachineState,
            saved: (UniversalControlPolicy, bool),
            universal_control: &'static str,
            spotlight: &'static str,
            next: &'static str,
            reason_starts: Option<&'static str>,
            nothing_to_apply: bool,
            apply_on_macos: bool,
        }
        let rows = [
            Row {
                name: "partial",
                state: machine_state(UcPosture::Partial, 2, HomePosture::Account),
                saved: (Off, true),
                universal_control: "Universal Control: partly disabled",
                spotlight: "Build output: 8 target dirs hidden, 3 open to Spotlight — 2 a pass would hide",
                next: "Apply now would set: Universal Control off for this host; 2 target dir(s) hidden from Spotlight",
                reason_starts: None,
                nothing_to_apply: false,
                apply_on_macos: true,
            },
            Row {
                name: "unknown",
                state: machine_state(UcPosture::Unknown, 0, HomePosture::Account),
                saved: (Off, true),
                universal_control: "Universal Control: could not be read on this Mac",
                spotlight: "Build output: 8 target dirs hidden, 1 open to Spotlight — 0 a pass would hide",
                next: "Apply now would set: Universal Control off for this host",
                reason_starts: None,
                nothing_to_apply: false,
                apply_on_macos: true,
            },
            Row {
                name: "default, left alone",
                state: MachineState {
                    policy: Leave,
                    ..machine_state(UcPosture::Default, 0, HomePosture::Account)
                },
                saved: (Leave, true),
                universal_control: "Universal Control: at the OS default — the cursor roams to other Macs and iPads · left alone ([machine] universal_control is not \"off\")",
                spotlight: "Build output: 8 target dirs hidden, 1 open to Spotlight — 0 a pass would hide",
                // `spotlight_noindex` is ON and one dir is exposed that no pass can
                // reach, so the machine is NOT where [machine] wants it — the verdict
                // says what an apply can (not) do instead of calling it settled.
                next: "Nothing an apply can do — 1 target dir(s) stay open to Spotlight (free-standing, or refused for now)",
                reason_starts: None,
                nothing_to_apply: true,
                apply_on_macos: false,
            },
            Row {
                name: "disabled, leave says nothing extra",
                state: MachineState {
                    policy: Leave,
                    ..machine_state(UcPosture::Disabled, 0, HomePosture::Account)
                },
                saved: (Leave, true),
                universal_control: "Universal Control: disabled on this Mac",
                spotlight: "Build output: 8 target dirs hidden, 1 open to Spotlight — 0 a pass would hide",
                next: "Nothing an apply can do — 1 target dir(s) stay open to Spotlight (free-standing, or refused for now)",
                reason_starts: None,
                nothing_to_apply: true,
                apply_on_macos: false,
            },
            Row {
                name: "scan hit its budget",
                state: MachineState {
                    scan_complete: false,
                    ..machine_state(UcPosture::Disabled, 1, HomePosture::Account)
                },
                saved: (Off, true),
                universal_control: "Universal Control: disabled on this Mac",
                spotlight: "Build output: 8 target dirs hidden, 2 open to Spotlight — 1 a pass would hide (at least — the scan hit its budget)",
                next: "Apply now would set: 1 target dir(s) hidden from Spotlight",
                reason_starts: None,
                nothing_to_apply: false,
                apply_on_macos: true,
            },
            Row {
                name: "spotlight switched off",
                state: MachineState {
                    spotlight_noindex: false,
                    ..machine_state(UcPosture::Disabled, 2, HomePosture::Account)
                },
                saved: (Off, false),
                universal_control: "Universal Control: disabled on this Mac",
                spotlight: "Build output: 8 target dirs hidden, 3 open to Spotlight — 0 a pass would hide · switched off ([machine] spotlight_noindex = false)",
                next: "Nothing to apply — Universal Control and Spotlight are where [machine] wants them",
                reason_starts: None,
                nothing_to_apply: true,
                apply_on_macos: false,
            },
            Row {
                name: "home unresolved",
                state: machine_state(UcPosture::Default, 2, HomePosture::Unresolved),
                saved: (Off, true),
                universal_control: "Universal Control: at the OS default — the cursor roams to other Macs and iPads",
                spotlight: "Build output: 8 target dirs hidden, 3 open to Spotlight — 2 a pass would hide",
                next: "",
                reason_starts: Some("Not applied here: the account home could not be resolved"),
                nothing_to_apply: false,
                apply_on_macos: false,
            },
        ];
        for row in rows {
            let mut service = PackagesService::new();
            let seq = service.begin(None).unwrap();
            assert!(service.finish(
                seq,
                refresh(PackagesStatusReport::from_parts(
                    true,
                    true,
                    "fp".into(),
                    Some(&status("ok")),
                    &[]
                )),
            ));
            let _ = service.replace_machine_state(Ok(row.state));
            let p = service
                .state(true, true, true)
                .with_machine_config(row.saved.0, row.saved.1)
                .projection();
            assert_eq!(
                p.machine.universal_control, row.universal_control,
                "{}",
                row.name
            );
            assert_eq!(p.machine.spotlight, row.spotlight, "{}", row.name);
            assert_eq!(p.machine.next, row.next, "{}", row.name);
            match row.reason_starts {
                Some(prefix) => assert!(
                    p.machine
                        .reason
                        .as_deref()
                        .is_some_and(|r| r.starts_with(prefix)),
                    "{}: {:?}",
                    row.name,
                    p.machine.reason
                ),
                None => assert!(
                    p.machine.reason.is_none(),
                    "{}: {:?}",
                    row.name,
                    p.machine.reason
                ),
            }
            assert_eq!(
                p.machine.nothing_to_apply, row.nothing_to_apply,
                "{}",
                row.name
            );
            assert_eq!(
                p.machine.apply_enabled,
                cfg!(target_os = "macos") && row.apply_on_macos,
                "{}",
                row.name
            );
            assert!(p.machine.saved.is_none(), "{}: saved == record", row.name);
        }
    }

    /// The 2026-09-14 incident's state line ([`INCIDENT_2026_09_14_REASON`]).
    fn incident_reason() -> String {
        INCIDENT_2026_09_14_REASON.to_string()
    }

    /// The canonical `atpkg::state` spellings the page paints every day fit one row at
    /// the narrowest budget and come back byte-for-byte: the layout of the common case
    /// is unchanged by the wrap.
    #[test]
    fn reason_lines_keeps_a_short_state_unchanged() {
        for state in [
            "managed 1971 — pinned by index 41",
            "active",
            "extra — not installed (opt in: aterm pkg install vendorx)",
            "needs admin — run: aterm pkg install clt",
            "blocked by clt: needs admin — run: aterm pkg install clt",
            "linked",
            "",
            // A remedy that fits stays one line — the fix is already fully visible.
            "error: stale link. fix: `aterm pkg repair`",
        ] {
            assert_eq!(
                reason_lines(state, 64),
                vec![state.to_string()],
                "{state:?}"
            );
        }
        // Exactly at the budget is still one line; one past it wraps.
        let edge = "x".repeat(64);
        assert_eq!(reason_lines(&edge, 64), vec![edge.clone()]);
        assert_eq!(reason_lines(&format!("{edge} y"), 64).len(), 2);
    }

    /// The incident: the fix leads, whole, at every budget the page uses — nothing of
    /// it is cut, no line exceeds its budget, and the diagnosis is bounded after it.
    #[test]
    fn reason_lines_puts_the_fix_first_and_whole() {
        let incident = incident_reason();
        assert!(
            incident.chars().count() > 600,
            "{}",
            incident.chars().count()
        );
        let fix_at = incident
            .find(" fix: ")
            .expect("atpkg spells the remedy ` fix: `");
        let fix_clause = &incident[fix_at + " fix: ".len()..];
        for width in [64usize, 104, 144] {
            let lines = reason_lines(&incident, width);
            assert!(
                lines[0].starts_with("fix: unset ATPKG_REFUSE_TRACKED_INSTALL"),
                "{width}: {:?}",
                lines[0]
            );
            for line in &lines {
                assert!(fits(line, width), "{width}: {line:?}");
            }
            // The fix's lines are the leading ones; the diagnosis starts with the
            // recorded head, and a fix that fills the bound is followed by the marker.
            let fix_lines: Vec<&str> = lines
                .iter()
                .map(String::as_str)
                .take_while(|line| {
                    !line.starts_with("error: stage:") && !line.starts_with('\u{2026}')
                })
                .collect();
            assert_eq!(
                fix_lines.join(" "),
                format!("fix: {fix_clause}"),
                "{width}: nothing of the fix is cut"
            );
            let last = lines.last().unwrap();
            assert!(last.ends_with(" more)"), "{width}: {last:?}");
            let expected_len = if fix_lines.len() >= MAX_REASON_LINES {
                fix_lines.len() + 1
            } else {
                MAX_REASON_LINES
            };
            assert_eq!(lines.len(), expected_len, "{width}: {lines:#?}");
            if fix_lines.len() < MAX_REASON_LINES {
                assert!(
                    lines[fix_lines.len()]
                        .starts_with("error: stage: this process is provenance-tracked"),
                    "{width}: the diagnosis head follows the fix: {:?}",
                    lines[fix_lines.len()]
                );
            }
        }
        // The wide budget: the fix's lines first (however many the remedy's current
        // wording needs — it grew by a line when the config key arrived, 2026-09-15),
        // then the diagnosis in what room is left — the first line whole, the last
        // giving its tail to the marker, which counts what stays hidden.
        let wide = reason_lines(&incident, 144);
        assert_eq!(wide.len(), MAX_REASON_LINES);
        let fix_n = wrap_words(&format!("fix: {fix_clause}"), 144).len();
        assert!(
            fix_n < MAX_REASON_LINES,
            "the wide budget holds the whole fix with room to spare: {fix_n}"
        );
        assert!(
            wide[fix_n].starts_with("error: stage:"),
            "{:?}",
            wide[fix_n]
        );
        assert!(
            wide.iter().all(|line| !line.contains(" fix:")),
            "the diagnosis no longer carries the fix: {wide:#?}"
        );
        let diagnosis = incident[..fix_at].trim_end_matches('.');
        let room = MAX_REASON_LINES - fix_n;
        let hidden = wrap_words(diagnosis, 144).len() - room;
        assert!(hidden >= 1, "{hidden}");
        let last = &wide[MAX_REASON_LINES - 1];
        assert!(
            last.ends_with(&format!(" \u{2026}({hidden} more)")),
            "{last:?}"
        );
        assert!(
            wrap_words(diagnosis, 144)[room - 1]
                .starts_with(last.split(" \u{2026}").next().unwrap()),
            "the boundary line is the diagnosis's own line, shortened: {last:?}"
        );
    }

    /// No remedy to lead with: the reason wraps whole while it fits the bound, and past
    /// it the marker replaces the last visible line's tail, counting the hidden lines.
    #[test]
    fn reason_lines_wraps_and_bounds_a_reason_without_a_fix() {
        let within = "error: stage: the signed index names build 1971 for ay but the store holds \
                      no such tree and the download step was refused by the policy";
        let wrapped = reason_lines(within, 48);
        assert!(
            wrapped.len() > 1 && wrapped.len() <= MAX_REASON_LINES,
            "{wrapped:#?}"
        );
        assert_eq!(
            wrapped.join(" "),
            within.split_whitespace().collect::<Vec<_>>().join(" ")
        );
        for line in &wrapped {
            assert!(fits(line, 48), "{line:?}");
        }

        let long: String = (0..60)
            .map(|n| format!("word{n:02}"))
            .collect::<Vec<_>>()
            .join(" ");
        let all = wrap_words(&long, 40);
        assert!(all.len() > MAX_REASON_LINES);
        let bounded = reason_lines(&long, 40);
        assert_eq!(bounded.len(), MAX_REASON_LINES);
        assert_eq!(bounded[..MAX_REASON_LINES - 1], all[..MAX_REASON_LINES - 1]);
        let hidden = all.len() - MAX_REASON_LINES;
        let last = &bounded[MAX_REASON_LINES - 1];
        assert!(
            last.ends_with(&format!(" \u{2026}({hidden} more)")),
            "{last:?}"
        );
        assert!(fits(last, 40), "{last:?}");
        assert!(all[MAX_REASON_LINES - 1].starts_with(last.split(" \u{2026}").next().unwrap()));
    }

    /// The marker is the WORD `fix:` — a `prefix:` in a path sentence is not a remedy,
    /// a bundle line's parenthesised `fix:` is, and one glued to a path segment is not.
    #[test]
    fn reason_lines_marker_is_the_word_fix() {
        let prefix = format!(
            "error: stage: the prefix: /Users//example/Library/Application Support/aterm/pkg is {}",
            "not writable and the untracked lane could not be reached for the install pass"
        );
        let lines = reason_lines(&prefix, 48);
        assert!(
            lines[0].starts_with("error: stage: the prefix:"),
            "{lines:#?}"
        );
        assert!(
            lines.iter().all(|line| !line.starts_with("fix:")),
            "{lines:#?}"
        );

        let bundle = format!(
            "error: the bundle at /Applications/aterm.app carries com.apple.provenance on {} (the bundle's fix: {})",
            "targo",
            atpkg::provenance::REMEDY
        );
        let lines = reason_lines(&bundle, 64);
        assert!(
            lines[0].starts_with("fix: `aterm pkg repair` clears the tag"),
            "{lines:#?}"
        );

        let glued = format!(
            "error: stage: /tmp/fix:abc is not a store path {}",
            "x".repeat(60)
        );
        let lines = reason_lines(&glued, 48);
        assert!(
            lines[0].starts_with("error: stage: /tmp/fix:abc"),
            "{lines:#?}"
        );
    }

    /// A token longer than the row (a path) is broken at the budget rather than left
    /// for the painter to ellipsize; the pieces reassemble to the token.
    #[test]
    fn reason_lines_breaks_a_token_longer_than_the_row() {
        let path = format!(
            "/Users//example/Library/Application-Support/aterm/pkg/store/{}",
            "a".repeat(70)
        );
        let state = format!("error: stage: cannot lay {path}");
        let lines = reason_lines(&state, 40);
        for line in &lines {
            assert!(fits(line, 40), "{line:?}");
        }
        let joined: String = lines
            .iter()
            .map(|line| line.trim_end_matches(" \u{2026}(1 more)"))
            .collect::<Vec<_>>()
            .concat();
        assert!(
            joined.starts_with("error: stage: cannot lay/Users"),
            "{joined}"
        );
        assert_eq!(reason_lines(&state, 200), vec![state.clone()]);
        let whole = wrap_words(&path, 40);
        assert_eq!(whole.concat(), path);
        assert!(whole.iter().all(|line| fits(line, 40)));
    }

    // ---- Phase 4: the four facts per program, the Activity, the local clock ----

    /// 2026-09-21T14:13:20Z, a Monday.
    const NOW: i64 = 1_790_000_000;
    /// UTC-7 (a Pacific summer clock).
    const PDT: i64 = -7 * 3600;

    /// WHEN, AS A PERSON READS IT: relative inside a day, then the LOCAL calendar — the
    /// offset moves the day and the clock, never a raw UTC stamp — and a time ahead of now
    /// (a clock set back) is never "ago".
    #[test]
    fn when_words_are_relative_then_the_local_calendar() {
        let w = |ago: i64, offset: i64| when_words(NOW - ago, NOW, LocalClock::fixed(offset));
        assert_eq!(w(0, PDT), "just now");
        assert_eq!(w(59, PDT), "just now");
        assert_eq!(w(4 * 60, PDT), "4 min ago");
        assert_eq!(w(3 * 3600 + 5, PDT), "3 h ago");
        assert_eq!(w(23 * 3600, PDT), "23 h ago");
        assert_eq!(w(86_400, PDT), "yesterday 07:13");
        assert_eq!(w(3 * 86_400, PDT), "Fri 07:13");
        assert_eq!(w(30 * 86_400, PDT), "Aug 22");
        assert_eq!(w(400 * 86_400, PDT), "Aug 17 2025");
        // The same instant, 25 h ago, on three clocks: the LOCAL day decides.
        assert_eq!(w(25 * 3600, 0), "yesterday 13:13");
        assert_eq!(w(25 * 3600, 12 * 3600), "yesterday 01:13");
        assert_eq!(w(25 * 3600, -14 * 3600), "Sat 23:13");
        // Ahead of now: its local day and time.
        let utc = LocalClock::fixed(0);
        assert_eq!(when_words(NOW + 7200, NOW, utc), "today 16:13");
        assert_eq!(when_words(NOW + 86_400, NOW, utc), "Sep 22 14:13");
        for words in [
            w(86_400, PDT),
            w(400 * 86_400, 0),
            when_words(NOW + 60, NOW, utc),
        ] {
            assert!(!words.contains('T') && !words.contains('Z'), "{words}");
        }
    }

    /// THE CLOCK PER INSTANT (review of Phase 4, 2026-09-23). A window that lives across a
    /// daylight-saving switch reads the switch on its next collection, and a clock time
    /// from before it keeps the hour it was stamped under: Pacific summer time ended at
    /// `SWITCH`, so a change made at 14:05 PDT the Saturday before reads `14:05`, never
    /// `13:05`, and one after the switch reads on PST. The switch is found by bisection over the
    /// reader. No offset known: the calendar is UTC's and SAYS so.
    #[test]
    fn the_local_clock_reads_each_instant_on_its_own_offset() {
        const PST: i64 = -8 * 3600;
        // 2026-11-01T09:00:00Z — 02:00 PDT, when the clocks fell back.
        const SWITCH: i64 = 1_793_523_600;
        let now = SWITCH + 36 * 3600;
        let zone = |at: i64| Some(if at < SWITCH { PDT } else { PST });
        let clock = LocalClock::probe(now, zone);
        let (since, before) = clock.changed.expect("the switch is inside the week");
        assert!((SWITCH..SWITCH + 60).contains(&since), "{since}");
        assert_eq!(before, PDT);
        // 2026-10-31T21:05:00Z is 14:05 PDT on Saturday; 2026-11-01T22:05:00Z 14:05 PST.
        let saturday = 1_793_480_700;
        assert_eq!(when_words(saturday, now, clock), "Sat 14:05");
        assert_eq!(
            when_words(saturday, now, LocalClock::fixed(PST)),
            "Sat 13:05",
            "one offset for every instant is the hour off this replaces"
        );
        // Sunday 17:05Z, after the switch: 09:05 PST.
        assert_eq!(
            when_words(saturday + 20 * 3600, now, clock),
            "yesterday 09:05"
        );
        // No switch in the week: two reads, one offset.
        let reads = std::cell::Cell::new(0);
        let steady = LocalClock::probe(now + 30 * 86_400, |at| {
            reads.set(reads.get() + 1);
            zone(at)
        });
        assert_eq!((steady, reads.get()), (LocalClock::fixed(PST), 2));
        // A reader that cannot say: UTC, marked as UTC.
        assert_eq!(LocalClock::probe(now, |_| None), LocalClock::UNKNOWN);
        assert_eq!(
            when_words(saturday, now, LocalClock::UNKNOWN),
            "Sat 21:05 UTC"
        );
        assert_eq!(
            when_words(now - 120, now, LocalClock::UNKNOWN),
            "2 min ago",
            "a relative time needs no zone"
        );
        // The live reader (`date -r`), wherever it answers: the offset now, and a switch
        // found exactly when this machine's zone has one in that week (a Pacific clock
        // bisects the real one).
        if let (Some(then), Some(at_now)) = (
            crate::presence::local_offset_at(now - CLOCK_WINDOW_S),
            crate::presence::local_offset_at(now),
        ) {
            let live = LocalClock::read(now);
            assert_eq!(live.offset_s, Some(at_now));
            assert_eq!(live.changed.is_some(), then != at_now, "{live:?}");
        }
    }

    fn vendor_id(version: &str) -> u64 {
        atpkg::vendor_direct::Version::parse(version)
            .unwrap()
            .build_id()
    }

    /// A report over `rows` (name, installed build, state), index build 44.
    fn report_of(rows: &[(&str, Option<u64>, String)]) -> PackagesStatusReport {
        let mut status = status("up to date (index build 44)");
        status.last_index_build = 44;
        status.programs = rows
            .iter()
            .map(|(name, build, state)| {
                (
                    (*name).to_string(),
                    atpkg::ProgramStatus {
                        installed_build: *build,
                        state: state.clone(),
                        tree_root: String::new(),
                    },
                )
            })
            .collect();
        PackagesStatusReport::from_parts(true, true, "fp".into(), Some(&status), &[])
    }

    fn row<'a>(p: &'a PackagesProjection, name: &str) -> &'a PackagesProgramRow {
        p.programs.iter().find(|r| r.name == name).unwrap()
    }

    /// THE PAGE'S FACTS PER PROGRAM (Phase 4), IN PLAIN WORDS (2026-09-23 audit, SB-18),
    /// for the four kinds of row: a vendor program at its vendor's latest (`latest from
    /// Anthropic`), an ALab program behind its index's pin (no claim to be current while
    /// a newer build is known — where it comes from, and the newer build), a vendor
    /// program HELD (`you pinned it`), and one whose newer build was REFUSED (its fault
    /// stays on the reason lines; the badge names a refusal). Last updated relative and
    /// local, when its vendor was checked, any newer build known.
    #[test]
    fn a_rows_facts_project_version_source_updated_and_latest_known() {
        let mut report = report_of(&[
            (
                "claude",
                Some(vendor_id("2.1.280")),
                atpkg::state::vendor_managed("2.1.280", "Anthropic"),
            ),
            ("ay", Some(1971), atpkg::state::managed(1971, 44)),
            (
                "codex",
                Some(vendor_id("0.156.0")),
                atpkg::state::vendor_kept("0.156.0", "held by local pin"),
            ),
        ]);
        report.clock = LocalClock::fixed(PDT);
        for r in &mut report.programs {
            match r.name.as_str() {
                "claude" => {
                    r.update.updated_at = Some(NOW - 3 * 3600);
                    r.update.latest_known = Some("2.1.280".into());
                    r.update.checked_at = Some(NOW - 4 * 60);
                }
                "ay" => {
                    r.update.updated_at = Some(NOW - 86_400);
                    r.update.latest_known = Some("build 1980".into());
                }
                _ => r.update.latest_known = Some("0.157.0".into()),
            }
        }
        let mut service = PackagesService::new();
        observe(&mut service, report);
        let p = service
            .state(true, true, true)
            .projection_at(std::time::UNIX_EPOCH + std::time::Duration::from_secs(NOW as u64));
        let words = |name: &str| row(&p, name).words.clone().expect(name);

        let claude = words("claude");
        assert_eq!(claude.summary, "2.1.280  \u{b7}  latest from Anthropic");
        assert_eq!(
            claude.detail.as_deref(),
            Some("Updated 3 h ago  \u{b7}  Checked 4 min ago")
        );
        let ay = words("ay");
        assert_eq!(ay.summary, "build 1971  \u{b7}  from ALab");
        assert_eq!(
            ay.detail.as_deref(),
            Some("Updated yesterday 07:13  \u{b7}  Latest known build 1980")
        );
        let held = words("codex");
        assert_eq!(
            held.summary, "0.156.0  \u{b7}  you pinned it",
            "a held row says why it is not current"
        );
        assert_eq!(held.detail.as_deref(), Some("Latest known 0.157.0"));
        assert!(
            p.attention.is_empty(),
            "a hold is no fault: {:?}",
            p.attention
        );
        // A REFUSED newer build: the row names the version it keeps and its vendor's
        // channel; the fault is NOT in the detail — the page keeps the fault's own words
        // whole on the reason lines — and the badge names a refusal as one.
        let mut refused = report_of(&[(
            "codex",
            Some(vendor_id("0.156.0")),
            "error: codex 0.157.0 refused: digest mismatch \u{2014} keeping 0.156.0".to_string(),
        )]);
        refused.clock = LocalClock::fixed(PDT);
        refused.programs[0].update.updated_at = Some(NOW - 3 * 86_400);
        refused.programs[0].update.latest_known = Some("0.157.0".into());
        let mut service = PackagesService::new();
        observe(&mut service, refused);
        let p = service
            .state(true, true, true)
            .projection_at(std::time::UNIX_EPOCH + std::time::Duration::from_secs(NOW as u64));
        let refused = row(&p, "codex").words.clone().expect("codex");
        assert_eq!(refused.summary, "0.156.0  \u{b7}  from OpenAI");
        assert_eq!(
            refused.detail.as_deref(),
            Some("Updated Fri 07:13  \u{b7}  Latest known 0.157.0")
        );
        assert_eq!(p.attention, ["codex refused"]);
        // A row with no installed build paints its state as before: no words.
        let mut none = report_of(&[(
            "vendorx",
            None,
            atpkg::state::extra_not_installed("vendorx"),
        )]);
        none.clock = LocalClock::fixed(0);
        let mut service = PackagesService::new();
        observe(&mut service, none);
        assert_eq!(row(&projection(&service), "vendorx").words, None);
    }

    /// EVERY STATE atpkg WRITES, IN A PERSON'S WORDS — or none, for a fault, which rides
    /// its row verbatim (2026-09-23 audit, SB-18).
    #[test]
    fn plain_state_says_each_canonical_state_in_words() {
        let say = |state: String| plain_state(&ProgramStateKind::parse(&state), &state);
        assert_eq!(
            say(atpkg::state::managed(8256, 45)).as_deref(),
            Some("up to date")
        );
        assert_eq!(
            say(atpkg::state::vendor_managed("2.1.280", "Anthropic")).as_deref(),
            Some("latest from Anthropic")
        );
        assert_eq!(
            say(atpkg::state::vendor_kept("2.1.280", "held by local pin")).as_deref(),
            Some("you pinned it")
        );
        assert_eq!(
            say(atpkg::state::held_unpublished(
                None,
                9,
                "aarch64-apple-darwin",
                8
            ))
            .as_deref(),
            Some("kept at build 8 \u{2014} 9 isn\u{2019}t available for this Mac")
        );
        assert_eq!(
            say(atpkg::state::held_unpublished(
                Some("ay"),
                9,
                "aarch64-apple-darwin",
                8
            ))
            .as_deref(),
            Some("kept at build 8 \u{2014} ay's build 9 isn\u{2019}t available for this Mac")
        );
        assert_eq!(
            say(atpkg::state::extra_not_installed("gh")).as_deref(),
            Some("not installed")
        );
        assert_eq!(
            say(atpkg::state::needs_admin("clt")).as_deref(),
            Some("needs an administrator to install")
        );
        assert_eq!(
            say(atpkg::state::blocked(
                "clt",
                &atpkg::state::needs_admin("clt")
            ))
            .as_deref(),
            Some("waiting for clt")
        );
        for fault in [
            "error: codex 0.157.0 refused: digest mismatch",
            "aborted: ay stage failed",
            "tombstoned: pin yanked/below floor",
        ] {
            assert_eq!(say(fault.to_string()), None, "{fault} rides verbatim");
        }
    }

    /// THE HEADLINE SAYS "UP TO DATE" ONLY WHEN EVERY ROW THAT CLAIMS IT IS: a row at its
    /// index's pin that already knows a newer build stops saying "up to date" itself, and
    /// the headline above it must not say it either. A row the person pinned is their
    /// choice, not news — the negative control.
    #[test]
    fn the_headline_says_up_to_date_only_when_every_current_row_is() {
        let headline = |ay_latest: &str| {
            let mut report = report_of(&[
                ("ay", Some(1971), atpkg::state::managed(1971, 44)),
                (
                    "codex",
                    Some(vendor_id("0.156.0")),
                    atpkg::state::vendor_kept("0.156.0", "held by local pin"),
                ),
            ]);
            for r in &mut report.programs {
                r.update.latest_known = Some(if r.name == "ay" {
                    ay_latest.to_string()
                } else {
                    "0.157.0".to_string()
                });
            }
            let mut service = PackagesService::new();
            observe(&mut service, report);
            projection(&service).headline
        };
        assert_eq!(headline("build 1980"), "Updates are available");
        assert_eq!(
            headline("build 1971"),
            "ALab tools are up to date",
            "a pinned row knowing a newer build is not the page's news"
        );
    }

    /// A PASS'S OWN SENTENCE IS DROPPED ONLY WHEN THE HEADLINE SAYS ALL OF IT: `up to date
    /// (index build 45)` is folded into "Checked 3 min ago"; `up to date (ay build N) — but
    /// not what this machine runs: …` (atpkg records that qualification on purpose) stays.
    #[test]
    fn a_qualified_up_to_date_outcome_is_shown_not_folded() {
        assert!(outcome_is_plainly_current("up to date"));
        assert!(outcome_is_plainly_current("up to date (index build 45)"));
        assert!(outcome_is_plainly_current("up to date (ay build 8256)"));
        let qualified = "up to date (ay build 1971) \u{2014} but not what this machine runs: \
                         dev-linked to ~/src/ay";
        assert!(!outcome_is_plainly_current(qualified));
        assert!(!outcome_is_plainly_current("installed ay build 1972"));
        let mut service = PackagesService::new();
        observe(
            &mut service,
            PackagesStatusReport::from_parts(
                true,
                true,
                "fp".into(),
                Some(&status(qualified)),
                &[],
            ),
        );
        let detail = projection(&service).detail.unwrap_or_default();
        assert!(
            detail.contains("but not what this machine runs: dev-linked"),
            "{detail}"
        );
        let mut service = PackagesService::new();
        observe(
            &mut service,
            PackagesStatusReport::from_parts(
                true,
                true,
                "fp".into(),
                Some(&status("up to date (index build 45)")),
                &[],
            ),
        );
        let detail = projection(&service).detail.unwrap_or_default();
        assert!(detail.starts_with("Checked "), "{detail}");
    }

    /// WHAT THE STORE SAYS, read on the worker: a vendor build's `.vendor` record names its
    /// version and when it was verified (its "updated"), its stamp the head last seen and
    /// when; an ALab build's latest known is the verified index's pin, and its "updated"
    /// is when its `current` link was flipped.
    #[cfg(unix)]
    #[test]
    fn the_store_supplies_when_a_build_went_live_and_the_latest_known() {
        let root =
            std::env::temp_dir().join(format!("aterm-packages-facts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let layout = atpkg::Layout {
            prefix: root.join("pkg"),
        };
        // claude 2.1.280, verified at NOW - 2 h, beside a complete build.
        let version = atpkg::vendor_direct::Version::parse("2.1.280").unwrap();
        let build = layout.build_dir("claude", version.build_id());
        std::fs::create_dir_all(build.join("bin")).unwrap();
        let spec = atpkg::vendor_direct::spec("claude").unwrap();
        let record = atpkg::vendor_direct::VendorRecord {
            schema: 1,
            program: "claude".into(),
            version,
            vendor: spec.vendor.to_string(),
            source_url: format!("{}2.1.280/darwin-arm64/claude", spec.url_prefixes[0]),
            sha256: "a".repeat(64),
            size: 1,
            tree_root: "0".repeat(64),
            apple_team: cfg!(target_os = "macos").then(|| spec.apple_team.to_string()),
            anchor: spec.anchor,
            build_date: None,
            verified_at: NOW - 7200,
        };
        std::fs::write(
            atpkg::vendor_direct::record_path(&build).unwrap(),
            aterm_toml::to_string(&record).unwrap(),
        )
        .unwrap();
        atpkg::store::mark_build_ready(&build).unwrap();
        atpkg::vendor_direct::ProgramStamp::record_check(
            &layout,
            "claude",
            NOW - 240,
            atpkg::vendor_direct::Version::parse("2.1.281"),
            None,
        )
        .unwrap();
        // ay build 1971, its `current` flipped just now.
        let ay = layout.build_dir("ay", 1971);
        std::fs::create_dir_all(&ay).unwrap();
        std::os::unix::fs::symlink(&ay, layout.program_current("ay")).unwrap();

        let mut report = report_of(&[
            (
                "claude",
                Some(version.build_id()),
                atpkg::state::vendor_managed("2.1.280", "Anthropic"),
            ),
            ("ay", Some(1971), atpkg::state::managed(1971, 44)),
        ]);
        let pins: std::collections::BTreeMap<String, u64> =
            [("ay".to_string(), 1980)].into_iter().collect();
        report.attach_store_facts(&layout, Some(&pins));
        let fact = |name: &str| {
            report
                .programs
                .iter()
                .find(|r| r.name == name)
                .unwrap()
                .update
                .clone()
        };
        let claude = fact("claude");
        assert_eq!(claude.version.as_deref(), Some("2.1.280"));
        assert_eq!(claude.source, "Anthropic latest");
        assert_eq!(claude.updated_at, Some(NOW - 7200), "its verification");
        assert_eq!(
            claude.latest_known.as_deref(),
            Some("2.1.281"),
            "the stamp's head"
        );
        assert_eq!(claude.checked_at, Some(NOW - 240));
        let ay = fact("ay");
        assert_eq!(ay.version.as_deref(), Some("build 1971"));
        assert_eq!(ay.source, "ALab index 44");
        assert_eq!(
            ay.latest_known.as_deref(),
            Some("build 1980"),
            "the index pin"
        );
        let flipped = ay.updated_at.expect("the current link's time");
        let wall = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert!(
            (wall - 120..=wall + 5).contains(&flipped),
            "{flipped} vs {wall}"
        );
        // No pins: the row's own pin is the latest known.
        let mut unpinned = report_of(&[("ay", Some(1971), atpkg::state::managed(1971, 44))]);
        unpinned.attach_store_facts(&layout, None);
        assert_eq!(
            unpinned.programs[0].update.latest_known.as_deref(),
            Some("build 1971")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    fn entry(at: i64, kind: &str, pid: u32, fields: &[(&str, &str)]) -> atpkg::packages_log::Entry {
        atpkg::packages_log::Entry {
            at: Some(at),
            kind: kind.to_string(),
            pid,
            fields: fields
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        }
    }

    /// THE ACTIVITY: the package log's events as sentences, newest first, with relative
    /// local times — a program's move, why a row stopped being current (a refusal is
    /// trouble, a hold is not), a pass's end in its lane's words (offline and "did not
    /// run" are no trouble; a failure is), and a start with no end after it; a start whose
    /// end is listed is left out.
    #[test]
    fn the_activity_reads_the_log_as_sentences_newest_first() {
        use atpkg::packages_log::kind;
        let entries = vec![
            entry(
                NOW - 30,
                kind::PASS_START,
                9,
                &[("lane", "session"), ("verb", "update")],
            ),
            entry(
                NOW - 60,
                kind::PASS_END,
                7,
                &[
                    ("lane", "window"),
                    ("verb", "update"),
                    ("exit", "0"),
                    ("secs", "7"),
                    ("outcome", "up to date (index build 44)"),
                ],
            ),
            entry(
                NOW - 70,
                kind::PROGRAM,
                7,
                &[
                    ("program", "claude"),
                    ("from", "2.1.278"),
                    ("to", "2.1.280"),
                    ("source", "Anthropic latest"),
                    ("result", "updated"),
                ],
            ),
            entry(
                NOW - 80,
                kind::PROGRAM,
                7,
                &[
                    ("program", "codex"),
                    ("from", "0.156.0"),
                    ("source", "OpenAI latest"),
                    ("result", "refused"),
                    ("reason", "error: codex 0.157.0 refused: digest mismatch"),
                ],
            ),
            entry(
                NOW - 90,
                kind::PROGRAM,
                7,
                &[
                    ("program", "ay"),
                    ("from", "build 1971"),
                    ("source", "ALab index 44"),
                    ("result", "held"),
                    ("reason", "held: build 1980 is not published for this Mac"),
                ],
            ),
            entry(
                NOW - 100,
                kind::PASS_START,
                7,
                &[("lane", "window"), ("verb", "update")],
            ),
            entry(
                NOW - 3 * 3600,
                kind::PASS_END,
                5,
                &[
                    ("lane", "head watch"),
                    ("verb", "update claude"),
                    ("exit", "69"),
                ],
            ),
            entry(
                NOW - 86_400,
                kind::PASS_END,
                4,
                &[
                    ("lane", "typed"),
                    ("verb", "seed"),
                    ("exit", "75"),
                    ("outcome", "did not run: another pass holds the store"),
                ],
            ),
            entry(
                NOW - 2 * 86_400,
                kind::PASS_END,
                3,
                &[
                    ("lane", "agent update"),
                    ("verb", "update codex"),
                    ("exit", "1"),
                    ("secs", "12"),
                    ("outcome", "codex: stage failed"),
                ],
            ),
        ];
        let lines = activity_lines(&entries, NOW, LocalClock::fixed(PDT));
        let got: Vec<(&str, &str, bool)> = lines
            .iter()
            .map(|l| (l.when.as_str(), l.text.as_str(), l.trouble))
            .collect();
        assert_eq!(
            got,
            vec![
                (
                    "just now",
                    "Update check \u{b7} terminal session \u{b7} started, no end recorded yet",
                    false
                ),
                (
                    "1 min ago",
                    "Update check \u{b7} aterm window \u{b7} finished: up to date (index \
                     build 44) \u{b7} 7 s",
                    false
                ),
                (
                    "1 min ago",
                    "claude 2.1.278 \u{2192} 2.1.280 \u{b7} updated \u{b7} Anthropic latest",
                    false
                ),
                ("1 min ago", "codex 0.157.0 refused: digest mismatch", true),
                (
                    "1 min ago",
                    "ay held: build 1980 is not published for this Mac",
                    false
                ),
                (
                    "3 h ago",
                    "claude update \u{b7} release watch \u{b7} offline \u{2014} tried again \
                     soon",
                    false
                ),
                (
                    "yesterday 07:13",
                    "Launch check \u{b7} command line \u{b7} did not run",
                    false
                ),
                (
                    "Sat 07:13",
                    "codex update \u{b7} you asked \u{b7} failed: codex: stage failed \u{b7} 12 s",
                    true
                ),
            ]
        );
        assert!(activity_lines(&[], NOW, LocalClock::fixed(0)).is_empty());
        // A reason that opens with its result is the sentence itself.
        assert_eq!(
            program_activity(
                "codex",
                Some("0.156.0"),
                None,
                "OpenAI latest",
                "held",
                "held by local pin"
            ),
            ("codex held by local pin".to_string(), false)
        );
        assert_eq!(
            program_activity(
                "ty",
                Some("build 5"),
                None,
                "ALab index 44",
                "failed",
                "error: stage: disk full"
            ),
            ("ty failed: stage: disk full".to_string(), true)
        );
    }
}
