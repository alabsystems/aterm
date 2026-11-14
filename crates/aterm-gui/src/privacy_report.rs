// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Control-socket `privacy` report joining session consent, probe slots, and process facts.

use std::cell::Cell;
use std::time::{Duration, Instant};

use aterm_containment::consent::{
    self, Attribution, Claimants, ConsentPosture, FdaState, Folder, FsConsent, PostureInputs,
    ProbeLabel, Responsible, ResponsibleError, SpikeEvidence, TrashOffer,
};
use aterm_control::wire::{json_ok, json_str_field, pct_encode};

use crate::App;
use crate::consent_state::{
    AxProbe, ConsentPolicy, ConsentState, DebuggerRow, DevToolsRow, SlotFacts, session_attribution,
    session_posture,
};
use crate::control::{Scope, cross_session_authorized};
use crate::signing_identity::{SigningIdentity, install_posture_once, os_version};

const SERVICES: &[&str] = &[
    "documents",
    "desktop",
    "downloads",
    "network-volumes",
    "removable-volumes",
    "app-data",
    "file-provider-domains",
    "media-library",
    "photos",
    "app-bundles",
    "contacts",
];

const NEVER_COVERED: &[&str] = &[
    "file-provider-domains",
    "media-library",
    "photos",
    "app-bundles",
    "contacts",
];

const VOLUME_ROWS: &[&str] = &["network-volumes", "removable-volumes"];

const PRIVACY_READ_WAIT: Duration = Duration::from_millis(500);

/// One live session's consent row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionRow {
    sid: String,
    attribution: Attribution,
    /// The raw pid the responsibility SPI named, when it named one.
    responsible_pid: Option<i32>,
    responsible: Responsible,
    fs_consent: FsConsent,
    /// The shell-integration cwd, unencoded. `None` when unreported.
    cwd: Option<String>,
}

/// Where the Full Disk Access question stands, for the `access_card` line of the report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AccessCardReport {
    /// `quiet`, `asked`, `opened`, `granted` or `declined`.
    pub(crate) state: &'static str,
    /// The record: three unix-second stamps and the build that last verified the grant.
    pub(crate) record: crate::consent_card::AccessRecord,
    /// Need events counted in this process.
    pub(crate) needs: u32,
}

/// What the card host holds for the `access_card` row, read on the event loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AccessCardInputs {
    pub(crate) marker: Option<crate::consent_card::Marker>,
    pub(crate) record: crate::consent_card::AccessRecord,
    pub(crate) needs: u32,
}

impl AccessCardReport {
    fn new(inputs: AccessCardInputs, fda: FdaState, now_secs: u64) -> Self {
        Self {
            state: crate::consent_card::report_state(inputs.marker, &inputs.record, fda, now_secs),
            record: inputs.record,
            needs: inputs.needs,
        }
    }

    fn renew_at(&self) -> Option<u64> {
        self.record
            .asked_at
            .map(|at| at.saturating_add(crate::consent_card::RENEW_AFTER.as_secs()))
    }

    fn opt(value: Option<u64>) -> String {
        value.map_or_else(|| "-".to_string(), |v| v.to_string())
    }

    fn line(&self) -> String {
        format!(
            "access_card state={} asked_at={} opened_at={} verified_at={} renew_at={} needs={}",
            self.state,
            Self::opt(self.record.asked_at),
            Self::opt(self.record.opened_at),
            Self::opt(self.record.verified_at),
            Self::opt(self.renew_at()),
            self.needs,
        )
    }

    fn json(&self) -> String {
        let num = |value: Option<u64>| value.map_or_else(|| "null".to_string(), |v| v.to_string());
        format!(
            "{{{},\"asked_at\":{},\"opened_at\":{},\"verified_at\":{},\"renew_at\":{},\"needs\":{}}}",
            json_str_field("state", self.state),
            num(self.record.asked_at),
            num(self.record.opened_at),
            num(self.record.verified_at),
            num(self.renew_at()),
            self.needs,
        )
    }
}

/// The whole posture, assembled once and rendered two ways.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PrivacySnapshot {
    platform: &'static str,
    os: Option<&'static str>,
    /// The process-wide identity, read once
    /// ([`crate::signing_identity::signing_identity`]).
    identity: &'static SigningIdentity,
    /// The macOS install posture the first-open doctor classifies
    /// (`aterm_update::which_copy::posture_from`), as a token.
    install: &'static str,
    /// The canonical path this process runs from, when it can be read.
    running: Option<String>,
    /// Whether macOS can still find the code this process's grants are keyed
    /// to.
    anchor: aterm_containment::ImageAnchor,
    fda: FdaState,
    probe: ProbeLabel,
    probe_age_ms: Option<u128>,
    evidence: SpikeEvidence,
    /// [`covers_split`] of `fda` and `evidence`, computed once for the folder
    /// row and both renderers.
    split: CoverageSplit,
    attribution_root: Attribution,
    sessions: Vec<SessionRow>,
    containment_mode: String,
    seatbelt: &'static str,
    protected: Vec<String>,
    warmup: &'static str,
    warmup_last_ms: Option<u128>,
    /// The debugger rule (`docs/DESIGN-macos-debugger-access-2026-09-29.md`
    /// §4.1): an Authorization Services right, not a TCC grant, so it joins no
    /// coverage bucket and never feeds `prompt_possible`.
    debugger: DebuggerRow,
    /// The Developer Tools row (debugger design §9.1): tccd's newest answer
    /// for this bundle id from tccd's log, the switch's row in the system
    /// TCC.db, and the two joined (`grant=`).
    devtools: DevToolsRow,
    /// aterm's own Accessibility grant (`accessibility` row), read once for this report by the
    /// consent arm that wraps the permission walk's live read ([`ConsentState::accessibility`]).
    /// It joins no coverage bucket and never feeds `prompt_possible`.
    accessibility: AxProbe,
    /// This user's programs macOS's program check holds now: how many, the longest held to now,
    /// ms, and how many are stranded ([`crate::sys_host::Shared::program_check_now`]); `None` off
    /// macOS or with no view that speaks for now.
    program_check: Option<crate::vitals_jobs::HeldCount>,
    /// Per-service folder state, in `folder_names` order, and its source.
    folders: Vec<(&'static str, &'static str)>,
    folder_source: String,
    observer_fda: &'static str,
    observer_responsible: &'static str,
    /// The `observer log=` token: `ok` while a log stream runs (the default
    /// sentinel or `observer = true`), `unavailable` when one was wanted and
    /// cannot run, `off` otherwise.
    observer_log: &'static str,
    /// Where the Full Disk Access question stands (`access_card` row).
    access_card: AccessCardReport,
    /// Every bundle on disk claiming this process's `CFBundleIdentifier`, and
    /// the answer's age.
    claimants: Option<(Claimants, Duration)>,
    /// No census answer exists yet: `census=pending`, as opposed to `off`.
    census_pending: bool,
    /// Whether `/usr/bin/trash` is on this Mac: without it the Security panel
    /// offers no move, and no row reads `trash=offered`.
    trash_tool: bool,
}

/// The `covers=` / `uncovered=` / `unmeasured=` split.
pub(crate) fn covers_split(fda: FdaState, evidence: SpikeEvidence) -> CoverageSplit {
    let never = |s: &&&str| NEVER_COVERED.contains(s);
    let uncovered: Vec<&'static str> = SERVICES.iter().filter(never).copied().collect();
    let rest: Vec<&'static str> = SERVICES.iter().filter(|s| !never(s)).copied().collect();
    let mut split = if evidence.fda_coverage_measured {
        CoverageSplit {
            covers: rest,
            uncovered,
            unmeasured: Vec::new(),
        }
    } else {
        CoverageSplit {
            covers: Vec::new(),
            uncovered,
            unmeasured: rest,
        }
    };
    split.covers.retain(|service| *service != "app-data");
    split.unmeasured.retain(|service| *service != "app-data");
    if app_data_covered(fda, observed_fda_scope(fda, evidence.fda_scope)) {
        split.covers.push("app-data");
    } else {
        split.unmeasured.push("app-data");
    }
    split
}

/// The FDA scope the report uses: a grant this process observed, with no scope measured, is this
/// process's.
pub(crate) const fn observed_fda_scope(
    fda: FdaState,
    measured: consent::FdaScope,
) -> consent::FdaScope {
    if matches!(fda, FdaState::Granted) && matches!(measured, consent::FdaScope::Unknown) {
        consent::FdaScope::ThisProcess
    } else {
        measured
    }
}

const fn app_data_covered(fda: FdaState, scope: consent::FdaScope) -> bool {
    matches!(fda, FdaState::Granted)
        && matches!(
            scope,
            consent::FdaScope::ThisProcess | consent::FdaScope::NewProcesses
        )
}

/// The three coverage buckets of [`covers_split`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CoverageSplit {
    pub(crate) covers: Vec<&'static str>,
    pub(crate) uncovered: Vec<&'static str>,
    pub(crate) unmeasured: Vec<&'static str>,
}

/// The `observer fda=` value.
pub(crate) const fn observer_fda_value(probe: ProbeLabel) -> &'static str {
    match probe {
        ProbeLabel::RefusedDisabled => "off",
        ProbeLabel::Pending => "pending",
        label if label.refused() => "unavailable",
        _ => "ok",
    }
}

pub(crate) fn completed_probe_age_ms(label: ProbeLabel, age: Duration) -> Option<u128> {
    (!label.refused() && label != ProbeLabel::Pending).then_some(age.as_millis())
}

fn observer_responsible_value(answers: &[Result<i32, ResponsibleError>]) -> &'static str {
    if answers
        .iter()
        .any(|a| matches!(a, Err(ResponsibleError::Unsupported)))
    {
        return "off";
    }
    if answers
        .iter()
        .any(|a| matches!(a, Err(ResponsibleError::SymbolUnavailable)))
    {
        return "unavailable";
    }
    if answers.is_empty() { "off" } else { "ok" }
}

fn responsible_token(responsible: Responsible) -> String {
    responsible.token().map_or_else(
        || {
            responsible
                .pid()
                .map_or_else(|| "unknown".to_string(), |pid| pid.to_string())
        },
        str::to_owned,
    )
}

fn opt(value: Option<&str>) -> String {
    value.map_or_else(|| "-".to_string(), pct_encode)
}

const NOTE: &str =
    "unknown means not observed, not denied: reading a folder is what raises the prompt";

const NOTE_NO_OS_CONSENT: &str = "this platform has no OS consent layer: nothing prompts, and an \
     Operation not permitted is an ordinary file permission";

const REMEDIATE: [&str; 5] = [
    "settings:Privacy_AllFiles",
    "settings:Privacy_FilesAndFolders",
    DEBUGGER_REMEDIATE,
    DEVTOOLS_REMEDIATE,
    ACCESSIBILITY_REMEDIATE,
];

const TASKPORT_WATCH: &str = "off";

const DEBUGGER_REMEDIATE: &str = "aterm-settings:security";

const DEVTOOLS_REMEDIATE: &str = "settings:Privacy_DevTools";

/// The pane the walk's Accessibility page opens (`system_state_access::live_open_access_pane`).
const ACCESSIBILITY_REMEDIATE: &str = "settings:Privacy_Accessibility";

impl PrivacySnapshot {
    fn claimant_lines(&self) -> Vec<String> {
        let Some((census, age)) = &self.claimants else {
            let state = if self.census_pending {
                "pending"
            } else {
                "off"
            };
            return vec![format!("claimants=- conflicting=- census={state}")];
        };
        let conflicting = census.conflicting();
        let mut out = vec![format!(
            "claimants={} conflicting={} census={} sole={} age_ms={}",
            census.found.len(),
            conflicting.len(),
            census.enumeration.as_str(),
            yes_no(census.sole_claimant()),
            age.as_millis(),
        )];
        let listed = census.listed(self.trash_tool);
        for (claimant, offer) in &listed {
            out.push(format!(
                "claimant path={} signing={} dr={} team={} conflicts=yes trash={}",
                pct_encode(&claimant.path.to_string_lossy()),
                claimant.signing,
                claimant.dr.as_str(),
                opt(claimant.team.as_deref()),
                offer.as_str(),
            ));
        }
        if conflicting.len() > consent::MAX_CLAIMANT_ROWS {
            out.push(format!(
                "claimant truncated={} of={}",
                conflicting.len() - consent::MAX_CLAIMANT_ROWS,
                conflicting.len()
            ));
        }
        if !conflicting.is_empty() {
            let n = conflicting.len();
            let (copies, them, marked) = if n == 1 {
                ("1 other copy of aterm is".to_string(), "it", "it")
            } else {
                (
                    format!("{n} other copies of aterm are"),
                    "them",
                    "the ones marked trash=offered",
                )
            };
            let remedy = if listed
                .iter()
                .any(|(_, offer)| *offer == TrashOffer::Offered)
            {
                format!(
                    "Settings \u{25b8} Security lists {them} and offers to move {marked} to the \
                     Trash"
                )
            } else {
                format!("Settings \u{25b8} Security lists {them}")
            };
            out.push(format!(
                "note {}",
                pct_encode(&format!(
                    "{copies} signed differently; whichever asks macOS last resets access for \
                     every copy; {remedy}"
                ))
            ));
        }
        out
    }

    fn os_consent(&self) -> bool {
        self.platform == "macos"
    }

    fn remediate(&self) -> [Option<&'static str>; 5] {
        REMEDIATE.map(|place| self.os_consent().then_some(place))
    }

    /// `program_check=`, `program_check_oldest_ms=` and `program_check_stranded=`: the count, the
    /// longest held and how many are stranded, or none with none read; no oldest with none held,
    /// and no stranded count while none is named and not every held row was judged.
    fn program_check_fields(&self) -> (Option<u64>, Option<u64>, Option<u64>) {
        self.program_check.map_or((None, None, None), |c| {
            (
                Some(u64::from(c.held)),
                (c.held > 0).then_some(c.oldest_ms),
                c.stranded.map(u64::from),
            )
        })
    }

    fn protected(&self) -> &[String] {
        if self.os_consent() {
            &self.protected
        } else {
            &[]
        }
    }

    fn note(&self) -> &'static str {
        if self.os_consent() {
            NOTE
        } else {
            NOTE_NO_OS_CONSENT
        }
    }

    fn prompt_possible(&self) -> bool {
        self.os_consent()
            && ConsentPosture::join(PostureInputs {
                adoption: Attribution::Live,
                fda: self.fda,
                responsible: Responsible::Unknown,
                observed_eperm: false,
                evidence: self.evidence,
            })
            .prompt_possible
    }

    fn sessions_adopted(&self) -> usize {
        self.sessions
            .iter()
            .filter(|s| matches!(s.attribution, Attribution::Adopted))
            .count()
    }

    /// The line body.
    pub(crate) fn lines(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(self.sessions.len() + 14);
        out.push("schema=1".to_string());
        out.push(format!("platform={} os={}", self.platform, opt(self.os)));
        out.push(format!(
            "bundle_id={} display_name={} signing={} team={} dr={} grant_stable={} dev_build={}",
            opt(self.identity.bundle_id.as_deref()),
            opt(self.identity.display_name.as_deref()),
            self.identity.signing,
            opt(self.identity.team.as_deref()),
            self.identity.dr.as_str(),
            yes_no(self.identity.dr.grant_stable()),
            self.identity
                .dev_build
                .map_or_else(|| "-".to_string(), |d| d.to_string()),
        ));
        out.push(format!(
            "install={} running={} anchor={}",
            self.install,
            opt(self.running.as_deref()),
            self.anchor.as_str()
        ));
        out.push(format!(
            "full_disk_access={} probe={} probe_age_ms={} fda_scope={}",
            self.fda.as_str(),
            self.probe.as_str(),
            self.probe_age_ms
                .map_or_else(|| "-".to_string(), |ms| ms.to_string()),
            observed_fda_scope(self.fda, self.evidence.fda_scope).as_str(),
        ));
        out.push(format!("covers={}", list_or_dash(&self.split.covers)));
        out.push(format!("uncovered={}", list_or_dash(&self.split.uncovered)));
        out.push(format!(
            "unmeasured={}",
            list_or_dash(&self.split.unmeasured)
        ));
        let mut folder = String::from("folder");
        for (name, state) in &self.folders {
            folder.push(' ');
            folder.push_str(name);
            folder.push('=');
            folder.push_str(state);
        }
        folder.push_str(" source=");
        folder.push_str(&self.folder_source);
        out.push(folder);
        out.extend(self.claimant_lines());
        out.push(format!(
            "prompt_possible={}",
            yes_no(self.prompt_possible())
        ));
        out.push(format!(
            "attribution_root={} sessions_total={} sessions_adopted={}",
            self.attribution_root.as_str(),
            self.sessions.len(),
            self.sessions_adopted(),
        ));
        for s in &self.sessions {
            out.push(format!(
                "session sid={} attribution={} responsible_pid={} responsible={} fs_consent={} \
                 cwd={}",
                pct_encode(&s.sid),
                s.attribution.as_str(),
                s.responsible_pid
                    .map_or_else(|| "-".to_string(), |pid| pid.to_string()),
                responsible_token(s.responsible),
                s.fs_consent.as_str(),
                opt(s.cwd.as_deref()),
            ));
        }
        out.push(format!(
            "containment mode={} seatbelt={} protected={}",
            self.containment_mode,
            self.seatbelt,
            if self.protected().is_empty() {
                "-".to_string()
            } else {
                self.protected()
                    .iter()
                    .map(|p| pct_encode(p))
                    .collect::<Vec<_>>()
                    .join(",")
            },
        ));
        out.push(format!(
            "warmup={} warmup_last_ms={}",
            self.warmup,
            self.warmup_last_ms
                .map_or_else(|| "-".to_string(), |ms| ms.to_string()),
        ));
        out.push(format!(
            "debugger mode={} member={} self_hardened={} right={} probe={} probe_age_ms={} \
             pending={TASKPORT_WATCH} pending_right=- pending_age_ms=- last=- watch_ms=-",
            self.debugger.probe.mode.as_str(),
            self.debugger.probe.member.as_str(),
            self.debugger.self_hardened.as_str(),
            consent::TASKPORT_DEBUG_RIGHT,
            self.debugger.probe.label.as_str(),
            self.debugger
                .probe_age_ms
                .map_or_else(|| "-".to_string(), |ms| ms.to_string()),
        ));
        let num = |value: Option<u64>| value.map_or_else(|| "-".to_string(), |n| n.to_string());
        let devtools = self.devtools.probe;
        let (held, oldest, stranded) = self.program_check_fields();
        out.push(format!(
            "devtools state={} subject={} requests={} not_allowed={} answer_age_ms={} \
             window_s={} probe={} probe_age_ms={} grant={} list={} list_probe={} \
             program_check={} program_check_oldest_ms={} program_check_stranded={}",
            devtools.state.as_str(),
            opt(self.devtools.subject),
            num(devtools.requests.map(u64::from)),
            num(devtools.not_allowed.map(u64::from)),
            num(self.devtools.answer_age_ms),
            consent::DEVTOOL_WINDOW_S,
            devtools.label.as_str(),
            self.devtools
                .probe_age_ms
                .map_or_else(|| "-".to_string(), |ms| ms.to_string()),
            self.devtools.grant().as_str(),
            self.devtools.list.listing.as_str(),
            self.devtools.list.label.as_str(),
            num(held),
            num(oldest),
            num(stranded),
        ));
        out.push(format!(
            "accessibility grant={} probe={}",
            self.accessibility.grant(),
            self.accessibility.label(),
        ));
        out.push(format!(
            "observer fda={} responsible={} log={} taskport={TASKPORT_WATCH}",
            self.observer_fda, self.observer_responsible, self.observer_log,
        ));
        out.push(self.access_card.line());
        let [fda, files, debugger, devtools, accessibility] = self
            .remediate()
            .map(|place| place.map_or_else(|| "-".to_string(), pct_encode));
        out.push(format!(
            "remediate fda={fda} files={files} debugger={debugger} devtools={devtools} \
             accessibility={accessibility}"
        ));
        if self.anchor.grant_unverifiable() {
            let gone = if self.anchor == aterm_containment::ImageAnchor::Deleted {
                "deleted"
            } else {
                "renamed"
            };
            out.push(format!(
                "note {}",
                pct_encode(&format!(
                    "the bundle this process started from was {gone}, so no grant can match \
                     it: macOS asks again for everything, whatever full_disk_access= says, and \
                     its requests clear access for every copy of aterm"
                ))
            ));
        }
        out.push(format!("note {}", pct_encode(self.note())));
        out
    }
}

fn folder_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = Folder::ALL.iter().map(|f| f.as_str()).collect();
    names.extend_from_slice(VOLUME_ROWS);
    names
}

fn folder_state(
    name: &str,
    warmup: &[(Folder, crate::consent_warmup::WarmupRow)],
    covers: &[&'static str],
) -> &'static str {
    if let Some((_, row)) = warmup
        .iter()
        .find(|(folder, _)| folder.as_str() == name)
        .filter(|(_, row)| *row != crate::consent_warmup::WarmupRow::Unknown)
    {
        return row.as_str();
    }
    if covers.contains(&name) {
        return "covered-by-fda";
    }
    "unknown"
}

fn folder_source(states: &[(&'static str, &'static str)]) -> String {
    let mut sources: Vec<&'static str> = Vec::new();
    if states
        .iter()
        .any(|(_, state)| matches!(*state, "allowed" | "denied" | "asking" | "error"))
    {
        sources.push("warmup");
    }
    if states.iter().any(|(_, state)| *state == "covered-by-fda") {
        sources.push("fda");
    }
    if sources.is_empty() {
        "none".to_string()
    } else {
        sources.join(",")
    }
}

const fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn list_or_dash(items: &[&str]) -> String {
    if items.is_empty() {
        "-".to_string()
    } else {
        items.join(",")
    }
}

/// Render the `--json` privacy report object.
pub(crate) fn cmd_privacy_json(snapshot: &PrivacySnapshot) -> String {
    use std::fmt::Write as _;

    let split = &snapshot.split;
    let mut body = String::from("{\"schema\":1,");
    let _ = write!(body, "{},", json_str_field("platform", snapshot.platform));
    let _ = write!(body, "\"os\":{},", json_opt(snapshot.os));
    let _ = write!(
        body,
        "\"identity\":{{\"bundle_id\":{},\"display_name\":{},{},\"team\":{},{},\
         \"grant_stable\":{},\"dev_build\":{}}},",
        json_opt(snapshot.identity.bundle_id.as_deref()),
        json_opt(snapshot.identity.display_name.as_deref()),
        json_str_field("signing", snapshot.identity.signing),
        json_opt(snapshot.identity.team.as_deref()),
        json_str_field("dr", snapshot.identity.dr.as_str()),
        snapshot.identity.dr.grant_stable(),
        snapshot
            .identity
            .dev_build
            .map_or_else(|| "null".to_string(), |d| d.to_string()),
    );
    let _ = write!(
        body,
        "{},\"running\":{},{},\"grant_unverifiable\":{},",
        json_str_field("install", snapshot.install),
        json_opt(snapshot.running.as_deref()),
        json_str_field("anchor", snapshot.anchor.as_str()),
        snapshot.anchor.grant_unverifiable(),
    );
    let _ = write!(
        body,
        "{},{},\"probe_age_ms\":{},{},",
        json_str_field("full_disk_access", snapshot.fda.as_str()),
        json_str_field("probe", snapshot.probe.as_str()),
        snapshot
            .probe_age_ms
            .map_or_else(|| "null".to_string(), |ms| ms.to_string()),
        json_str_field(
            "fda_scope",
            observed_fda_scope(snapshot.fda, snapshot.evidence.fda_scope).as_str()
        ),
    );
    let _ = write!(
        body,
        "\"covers\":{},\"uncovered\":{},\"unmeasured\":{},",
        json_str_array(&split.covers),
        json_str_array(&split.uncovered),
        json_str_array(&split.unmeasured),
    );
    let _ = write!(
        body,
        "\"folders\":{{{}",
        json_str_field("source", &snapshot.folder_source)
    );
    for (name, state) in &snapshot.folders {
        let _ = write!(body, ",{}", json_str_field(name, state));
    }
    let _ = write!(body, "}},");
    let _ = match &snapshot.claimants {
        None => write!(
            body,
            "\"census\":{{{},\"claimants\":null,\"conflicting\":null,\"sole\":null,\
             \"age_ms\":null,\"conflicts\":null,\"truncated\":0}},",
            json_str_field(
                "state",
                if snapshot.census_pending {
                    "pending"
                } else {
                    "off"
                }
            )
        ),
        Some((census, age)) => {
            let conflicting = census.conflicting();
            let conflicts: Vec<String> = census
                .listed(snapshot.trash_tool)
                .into_iter()
                .map(|(claimant, offer)| {
                    format!(
                        "{{{},{},{},\"team\":{},{}}}",
                        json_str_field("path", &claimant.path.to_string_lossy()),
                        json_str_field("signing", claimant.signing),
                        json_str_field("dr", claimant.dr.as_str()),
                        json_opt(claimant.team.as_deref()),
                        json_str_field("trash", offer.as_str()),
                    )
                })
                .collect();
            write!(
                body,
                "\"census\":{{{},\"claimants\":{},\"conflicting\":{},\"sole\":{},\
                 \"age_ms\":{},\"conflicts\":[{}],\"truncated\":{}}},",
                json_str_field("state", census.enumeration.as_str()),
                census.found.len(),
                conflicting.len(),
                census.sole_claimant(),
                age.as_millis(),
                conflicts.join(","),
                conflicting.len().saturating_sub(consent::MAX_CLAIMANT_ROWS),
            )
        }
    };
    let _ = write!(body, "\"prompt_possible\":{},", snapshot.prompt_possible());
    let _ = write!(
        body,
        "{},\"sessions_total\":{},\"sessions_adopted\":{},",
        json_str_field("attribution_root", snapshot.attribution_root.as_str()),
        snapshot.sessions.len(),
        snapshot.sessions_adopted(),
    );
    body.push_str("\"sessions\":[");
    for (i, s) in snapshot.sessions.iter().enumerate() {
        if i > 0 {
            body.push(',');
        }
        let _ = write!(
            body,
            "{{{},{},\"responsible_pid\":{},{},{},\"cwd\":{}}}",
            json_str_field("sid", &s.sid),
            json_str_field("attribution", s.attribution.as_str()),
            s.responsible_pid
                .map_or_else(|| "null".to_string(), |pid| pid.to_string()),
            json_str_field("responsible", &responsible_token(s.responsible)),
            json_str_field("fs_consent", s.fs_consent.as_str()),
            json_opt(s.cwd.as_deref()),
        );
    }
    body.push_str("],");
    let _ = write!(
        body,
        "\"containment\":{{{},{},\"protected\":{}}},",
        json_str_field("mode", &snapshot.containment_mode),
        json_str_field("seatbelt", snapshot.seatbelt),
        json_str_array(
            &snapshot
                .protected()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
        ),
    );
    let _ = write!(
        body,
        "\"warmup\":{{{},\"last_ms\":{}}},",
        json_str_field("mode", snapshot.warmup),
        snapshot
            .warmup_last_ms
            .map_or_else(|| "null".to_string(), |ms| ms.to_string()),
    );
    let _ = write!(
        body,
        "\"debugger\":{{{},{},{},{},{},\"probe_age_ms\":{},{},\"pending_right\":null,\
         \"pending_age_ms\":null,\"last\":null,\"watch_ms\":null}},",
        json_str_field("mode", snapshot.debugger.probe.mode.as_str()),
        json_str_field("member", snapshot.debugger.probe.member.as_str()),
        json_str_field("self_hardened", snapshot.debugger.self_hardened.as_str()),
        json_str_field("right", consent::TASKPORT_DEBUG_RIGHT),
        json_str_field("probe", snapshot.debugger.probe.label.as_str()),
        snapshot
            .debugger
            .probe_age_ms
            .map_or_else(|| "null".to_string(), |ms| ms.to_string()),
        json_str_field("pending", TASKPORT_WATCH),
    );
    let num = |value: Option<u64>| value.map_or_else(|| "null".to_string(), |n| n.to_string());
    let devtools = snapshot.devtools.probe;
    let (held, oldest, stranded) = snapshot.program_check_fields();
    let _ = write!(
        body,
        "\"devtools\":{{{},\"subject\":{},\"requests\":{},\"not_allowed\":{},\
         \"answer_age_ms\":{},\"window_s\":{},{},\"probe_age_ms\":{},{},{},{},\
         \"program_check\":{},\"program_check_oldest_ms\":{},\"program_check_stranded\":{}}},",
        json_str_field("state", devtools.state.as_str()),
        json_opt(snapshot.devtools.subject),
        num(devtools.requests.map(u64::from)),
        num(devtools.not_allowed.map(u64::from)),
        num(snapshot.devtools.answer_age_ms),
        consent::DEVTOOL_WINDOW_S,
        json_str_field("probe", devtools.label.as_str()),
        snapshot
            .devtools
            .probe_age_ms
            .map_or_else(|| "null".to_string(), |ms| ms.to_string()),
        json_str_field("grant", snapshot.devtools.grant().as_str()),
        json_str_field("list", snapshot.devtools.list.listing.as_str()),
        json_str_field("list_probe", snapshot.devtools.list.label.as_str()),
        num(held),
        num(oldest),
        num(stranded),
    );
    let _ = write!(
        body,
        "\"accessibility\":{{{},{}}},",
        json_str_field("grant", snapshot.accessibility.grant()),
        json_str_field("probe", snapshot.accessibility.label()),
    );
    let _ = write!(body, "\"access_card\":{},", snapshot.access_card.json());
    let _ = write!(
        body,
        "\"observers\":{{{},{},{},{}}},",
        json_str_field("fda", snapshot.observer_fda),
        json_str_field("responsible", snapshot.observer_responsible),
        json_str_field("log", snapshot.observer_log),
        json_str_field("taskport", TASKPORT_WATCH),
    );
    let [fda, files, debugger, devtools, accessibility] = snapshot.remediate().map(json_opt);
    let _ = write!(
        body,
        "\"remediate\":{{\"fda\":{fda},\"files\":{files},\"debugger\":{debugger},\
         \"devtools\":{devtools},\"accessibility\":{accessibility}}},",
    );
    let _ = write!(body, "{}}}", json_str_field("note", snapshot.note()));
    json_ok(&body)
}

fn json_opt(value: Option<&str>) -> String {
    value.map_or_else(
        || "null".to_string(),
        |v| format!("\"{}\"", aterm_control::wire::json_escape(v)),
    )
}

fn json_str_array(items: &[&str]) -> String {
    let mut out = String::from("[");
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(&aterm_control::wire::json_escape(item));
        out.push('"');
    }
    out.push(']');
    out
}

/// Which shape of the posture the verb renders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrivacyForm {
    /// The full posture, as lines.
    Lines,
    /// The full posture, as one JSON object.
    Json,
}

/// One session the caller may read, as the pool holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SessionFacts {
    sid: String,
    pid: i32,
    /// aterm's adoption record, before the master switch.
    observed: Attribution,
    /// The shell-integration cwd, unencoded. `None` when unreported or when
    /// the terminal was contended.
    cwd: Option<String>,
}

/// What `privacy`'s one main-thread hop brings back ([`App::privacy_facts`]):
/// the facts only `App` holds.
#[derive(Debug)]
pub(crate) struct PrivacyAppFacts {
    /// The instance's consent state; a clone shares the slots.
    consent: ConsentState,
    /// `[privacy]` as the config held it at the hop.
    policy: ConsentPolicy,
    /// The instance's own adoption record, before the master switch.
    attribution_root: Attribution,
    /// The sessions the caller may read, by id.
    sessions: Vec<SessionFacts>,
    /// The configured warm-up mode's token.
    warmup: &'static str,
    /// The warm-up's rows, read without draining: a pass the Security panel
    /// has not folded yet must not be consumed.
    warmup_rows: Vec<(Folder, crate::consent_warmup::WarmupRow)>,
    /// The warm-up's completion stamp
    /// (`docs/DESIGN-macos-tcc-prompts-2026-08-30.md` §3.5), read without
    /// draining: an unfolded pass is reported on the next read.
    warmup_last_ms: Option<u128>,
    /// The `observer log=` token.
    observer_log: &'static str,
    access_card: AccessCardInputs,
    /// This user's programs macOS's program check holds now: the control worker's read
    /// ([`Self::with_program_check`]), `None` from the main hop.
    program_check: Option<crate::vitals_jobs::HeldCount>,
}

impl PrivacyAppFacts {
    /// These facts with `held`: this user's programs macOS's program check holds now, the longest
    /// held, ms, and how many are stranded ([`crate::sys_host::Shared::program_check_now`], read on
    /// the control worker, where it may pull a reading).
    pub(crate) fn with_program_check(
        mut self,
        held: Option<crate::vitals_jobs::HeldCount>,
    ) -> Self {
        self.program_check = held;
        self
    }

    fn responsible_answers(&self) -> Vec<Result<i32, ResponsibleError>> {
        if !self.policy.report_attribution {
            return vec![Err(ResponsibleError::Unsupported); self.sessions.len()];
        }
        let pids: Vec<i32> = self.sessions.iter().map(|session| session.pid).collect();
        self.consent.responsible_answers(&pids)
    }

    /// The report, off the event loop: the SPI asked; the slots read and,
    /// while one is pending, waited on for up to [`PRIVACY_READ_WAIT`]; then
    /// everything joined.
    pub(crate) fn report(self, identity: &'static SigningIdentity) -> PrivacySnapshot {
        let answers = self.responsible_answers();
        let consent = &self.consent;
        let seen = Cell::new(consent.publication());
        let first = consent.report_slots(&self.policy, identity);
        let slots = finish_privacy_read(
            first,
            || {
                seen.set(consent.publication());
                consent.report_slots(&self.policy, identity)
            },
            Instant::now,
            |slots, remaining| {
                consent.wait_for_change(seen.get(), &slots.pending_kinds(), remaining)
            },
        );
        PrivacySnapshot::assemble(self, slots, identity, &answers)
    }
}

impl PrivacySnapshot {
    fn assemble(
        facts: PrivacyAppFacts,
        slots: SlotFacts,
        identity: &'static SigningIdentity,
        responsible: &[Result<i32, ResponsibleError>],
    ) -> Self {
        debug_assert_eq!(facts.sessions.len(), responsible.len());
        let policy = &facts.policy;
        let accessibility = facts.consent.accessibility(policy.gate);
        let (probe, age) = slots.fda;
        let self_pid = i32::try_from(std::process::id()).unwrap_or(-1);
        let sessions = facts
            .sessions
            .into_iter()
            .zip(responsible)
            .map(|(session, &answer)| {
                let posture = session_posture(policy, session.observed, probe.state);
                SessionRow {
                    sid: session.sid,
                    attribution: posture.attribution,
                    responsible_pid: answer.ok(),
                    responsible: consent::classify_responsible(self_pid, answer),
                    fs_consent: posture.fs_consent,
                    cwd: session.cwd,
                }
            })
            .collect();
        let mode = aterm_containment::mode_or_containment();
        let (install, running) = install_posture_once().clone();
        let evidence = SpikeEvidence::MEASURED;
        let split = covers_split(probe.state, evidence);
        let folders: Vec<(&'static str, &'static str)> = folder_names()
            .into_iter()
            .map(|name| (name, folder_state(name, &facts.warmup_rows, &split.covers)))
            .collect();
        let folder_source = folder_source(&folders);
        Self {
            platform: std::env::consts::OS,
            os: os_version(),
            identity,
            install,
            running,
            anchor: aterm_containment::image_anchor(),
            fda: probe.state,
            probe: probe.label,
            probe_age_ms: completed_probe_age_ms(probe.label, age),
            evidence,
            split,
            attribution_root: policy.adoption(facts.attribution_root),
            sessions,
            containment_mode: mode.to_string().to_ascii_lowercase(),
            seatbelt: if aterm_containment::actuator::network_sandbox_actuated(mode) {
                "applied"
            } else {
                "none"
            },
            protected: policy
                .protected_roots()
                .iter()
                .map(|p| crate::app_tabs::home_abbreviated(&p.to_string_lossy()))
                .collect(),
            warmup: facts.warmup,
            warmup_last_ms: facts.warmup_last_ms,
            debugger: slots.debugger,
            devtools: slots.devtools,
            accessibility,
            program_check: facts.program_check,
            folders,
            folder_source,
            claimants: slots.census.0,
            census_pending: slots.census.1,
            trash_tool: crate::consent_retire::trash_tool_present(),
            observer_fda: observer_fda_value(probe.label),
            observer_responsible: observer_responsible_value(responsible),
            observer_log: facts.observer_log,
            access_card: AccessCardReport::new(
                facts.access_card,
                probe.state,
                crate::consent_card::now_unix_secs(),
            ),
        }
    }
}

impl App {
    /// `privacy`'s one main-thread hop (`Wake::ReadPrivacy`): the facts only
    /// `App` holds ([`PrivacyAppFacts`]).
    pub(crate) fn privacy_facts(&self, scope: Scope) -> PrivacyAppFacts {
        let mut ids: Vec<u64> = self.pool.iter().map(|s| s.id).collect();
        ids.sort_unstable();
        let sessions = ids
            .into_iter()
            .filter_map(|id| {
                let session = self.pool.get(id)?;
                cross_session_authorized(scope, "privacy", &session.ctx).then(|| SessionFacts {
                    sid: session.ctx.self_id.as_str().to_string(),
                    pid: session.pid,
                    observed: session_attribution(session),
                    cwd: self.session_reported_cwd(id),
                })
            })
            .collect();
        PrivacyAppFacts {
            consent: self.consent.clone(),
            policy: self.consent_policy(),
            attribution_root: self.instance_attribution(),
            sessions,
            warmup: self.config.privacy_warmup().as_str(),
            warmup_rows: self.consent_warmup.rows().to_vec(),
            warmup_last_ms: self.consent_warmup.last_pass_ms(),
            observer_log: self.privacy_observer_log(),
            access_card: self.access_card_inputs(),
            program_check: None,
        }
    }

    /// The report as `privacy` renders it from this App's state, with no
    /// wait: the hop's facts, the SPI's answers and one read of the slots,
    /// joined.
    #[cfg(test)]
    pub(crate) fn privacy_report_for_test(
        &self,
        form: PrivacyForm,
        scope: Scope,
    ) -> (Vec<String>, bool) {
        let facts = self.privacy_facts(scope);
        let identity = crate::signing_identity::signing_identity_if_warm();
        let answers = facts.responsible_answers();
        let slots = facts.consent.report_slots(&facts.policy, identity);
        let pending = slots.pending();
        let snapshot = PrivacySnapshot::assemble(facts, slots, identity, &answers);
        let rows = match form {
            PrivacyForm::Lines => snapshot.lines(),
            PrivacyForm::Json => vec![cmd_privacy_json(&snapshot)],
        };
        (rows, pending)
    }

    const fn instance_attribution(&self) -> Attribution {
        if self.handoff_successor {
            Attribution::Adopted
        } else {
            Attribution::Live
        }
    }

    /// The shell-integration cwd for one session, without ever waiting on the
    /// terminal lock: a contended terminal reports nothing rather than
    /// blocking the event loop under a poll.
    pub(crate) fn session_reported_cwd(&self, session: u64) -> Option<String> {
        use crate::cwd_native::ReportedCwd as _;
        let pooled = self.pool.get(session)?;
        let term = pooled.term.try_lock().ok()?;
        term.native_working_directory()
            .map(std::borrow::Cow::into_owned)
    }
}

fn finish_privacy_read(
    mut latest: SlotFacts,
    mut reread: impl FnMut() -> SlotFacts,
    now: impl Fn() -> Instant,
    mut wait: impl FnMut(&SlotFacts, Duration) -> bool,
) -> SlotFacts {
    let deadline = now() + PRIVACY_READ_WAIT;
    loop {
        let remaining = deadline.saturating_duration_since(now());
        if !privacy_read_should_retry(latest.pending(), remaining) || !wait(&latest, remaining) {
            return latest;
        }
        latest = reread();
    }
}

const fn privacy_read_should_retry(pending: bool, remaining: Duration) -> bool {
    pending && !remaining.is_zero()
}

#[cfg(test)]
#[path = "privacy_report_privacy_read_tests.rs"]
mod privacy_read_tests;

#[cfg(test)]
#[path = "privacy_report_tests.rs"]
mod tests;
