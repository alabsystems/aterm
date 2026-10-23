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
    ConsentPolicy, ConsentState, DebuggerRow, DevToolsRow, SlotFacts, session_attribution,
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
    /// The four unix-second facts the record holds.
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
    /// The Developer Tools list (debugger design §9.1): tccd's newest answer
    /// for this bundle id, read from tccd's own log.
    devtools: DevToolsRow,
    /// Per-service folder state, in `folder_names` order, and its source.
    folders: Vec<(&'static str, &'static str)>,
    folder_source: String,
    observer_fda: &'static str,
    observer_responsible: &'static str,
    /// The `observer log=` token: `off` unless `[privacy] observer` asked.
    observer_log: &'static str,
    /// Where the Full Disk Access question stands (`access_card` row).
    access_card: AccessCardReport,
    /// Every bundle on disk claiming this process's `CFBundleIdentifier`
    /// (`aterm_containment::consent`, "The claimant census") and the answer's
    /// age.
    claimants: Option<(Claimants, Duration)>,
    /// No census answer exists yet — `census=pending`, as opposed to `off`.
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

/// Successful access establishes the observing host's scope directly.
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

const REMEDIATE: [&str; 4] = [
    "settings:Privacy_AllFiles",
    "settings:Privacy_FilesAndFolders",
    DEBUGGER_REMEDIATE,
    DEVTOOLS_REMEDIATE,
];

const TASKPORT_WATCH: &str = "off";

const DEBUGGER_REMEDIATE: &str = "aterm-settings:security";

const DEVTOOLS_REMEDIATE: &str = "settings:Privacy_DevTools";

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

    fn remediate(&self) -> [Option<&'static str>; 4] {
        REMEDIATE.map(|place| self.os_consent().then_some(place))
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
        out.push(format!(
            "devtools state={} subject={} requests={} not_allowed={} answer_age_ms={} \
             window_s={} probe={} probe_age_ms={} grant={} list={} list_probe={}",
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
        ));
        out.push(format!(
            "observer fda={} responsible={} log={} taskport={TASKPORT_WATCH}",
            self.observer_fda, self.observer_responsible, self.observer_log,
        ));
        out.push(self.access_card.line());
        let [fda, files, debugger, devtools] = self
            .remediate()
            .map(|place| place.map_or_else(|| "-".to_string(), pct_encode));
        out.push(format!(
            "remediate fda={fda} files={files} debugger={debugger} devtools={devtools}"
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
    let _ = write!(
        body,
        "\"devtools\":{{{},\"subject\":{},\"requests\":{},\"not_allowed\":{},\
         \"answer_age_ms\":{},\"window_s\":{},{},\"probe_age_ms\":{},{},{},{}}},",
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
    let [fda, files, debugger, devtools] = snapshot.remediate().map(json_opt);
    let _ = write!(
        body,
        "\"remediate\":{{\"fda\":{fda},\"files\":{files},\"debugger\":{debugger},\
         \"devtools\":{devtools}}},",
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
    /// The warm-up's completion stamp (design §3.5), read without draining:
    /// an unfolded pass is reported on the next read.
    warmup_last_ms: Option<u128>,
    /// The `observer log=` token.
    observer_log: &'static str,
    access_card: AccessCardInputs,
}

impl PrivacyAppFacts {
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
mod privacy_read_tests {
    use aterm_containment::consent::{DebuggerProbeLabel, DevToolProbeLabel, FdaProbe, ProbeGate};

    use super::*;
    use crate::consent_state::ChangeKind;

    fn slots(probe: FdaProbe) -> SlotFacts {
        SlotFacts {
            fda: (probe, Duration::ZERO),
            census: (None, false),
            debugger: DebuggerRow::refused(DebuggerProbeLabel::RefusedDisabled),
            devtools: DevToolsRow::refused(DevToolProbeLabel::RefusedDisabled, None),
        }
    }

    #[test]
    fn privacy_read_observes_the_real_async_cache_completion() {
        let start = Instant::now();
        let mut state = ConsentState::inert();
        state.probes_for_test().live = true;
        let interval = Duration::from_secs(5);
        let seen = Cell::new(state.publication());
        let ((first, _), ticket) = state.read_at(ProbeGate::on(), interval, start);
        assert_eq!(first.label, ProbeLabel::Pending);
        let ticket = ticket.unwrap();
        let denied = consent::classify_probe(consent::ProbeOutcome::Errno(consent::ERRNO_EPERM));
        let rereads = Cell::new(0);
        let result = finish_privacy_read(
            slots(first),
            || {
                rereads.set(rereads.get() + 1);
                seen.set(state.publication());
                let ((current, _), next) = state.read_at(ProbeGate::on(), interval, start);
                assert!(
                    next.is_none(),
                    "reading a fresh completion does not launch a probe"
                );
                slots(current)
            },
            || start,
            |pending, remaining| {
                assert_eq!(pending.pending_kinds(), [ChangeKind::Fda]);
                assert_eq!(remaining, PRIVACY_READ_WAIT);
                state.complete_fda_for_test(ticket, denied, start);
                state.wait_for_change(
                    seen.get(),
                    &pending.pending_kinds(),
                    Duration::from_secs(60),
                )
            },
        );
        assert!(!result.pending());
        assert_eq!(result.fda.0.state, FdaState::Denied);
        assert_eq!(rereads.get(), 1, "one publication, one reread");
    }

    #[test]
    fn privacy_read_has_one_deadline_and_keeps_stuck_work_pending() {
        let start = Instant::now();
        let clock = Cell::new(start);
        let waits = Cell::new(0);
        let stuck = finish_privacy_read(
            slots(FdaProbe::pending()),
            || panic!("nothing published, so nothing is reread"),
            || clock.get(),
            |_, remaining| {
                waits.set(waits.get() + 1);
                assert_eq!(remaining, PRIVACY_READ_WAIT);
                clock.set(clock.get() + remaining);
                false
            },
        );
        assert!(stuck.pending());
        assert_eq!(stuck.fda.0.state, FdaState::Unknown);
        assert_eq!(clock.get() - start, PRIVACY_READ_WAIT);
        assert_eq!(waits.get(), 1);

        let clock = Cell::new(start);
        let rereads = Cell::new(0u32);
        let step = Duration::from_millis(200);
        let churned = finish_privacy_read(
            slots(FdaProbe::pending()),
            || {
                rereads.set(rereads.get() + 1);
                slots(FdaProbe::pending())
            },
            || clock.get(),
            |_, remaining| {
                assert_eq!(remaining, PRIVACY_READ_WAIT - step * rereads.get());
                let parked = step.min(remaining);
                clock.set(clock.get() + parked);
                parked == step
            },
        );
        assert!(churned.pending());
        assert_eq!(clock.get() - start, PRIVACY_READ_WAIT);
        assert_eq!(rereads.get(), 2, "one reread per publication");
    }

    #[test]
    fn a_completed_privacy_read_neither_waits_nor_rereads() {
        let now = Instant::now();
        let completed = finish_privacy_read(
            slots(consent::classify_probe(consent::ProbeOutcome::Ok)),
            || panic!("a completed observation needs no reread"),
            || now,
            |_, _| panic!("a completed observation needs no wait"),
        );
        assert_eq!(completed.fda.0.state, FdaState::Granted);
    }

    #[test]
    fn a_report_waits_on_exactly_its_pending_slots() {
        let answered = slots(consent::classify_probe(consent::ProbeOutcome::Ok));
        assert!(answered.pending_kinds().is_empty());
        assert!(!answered.pending());
        let mut pending = answered.clone();
        pending.census.1 = true;
        pending.devtools = DevToolsRow::refused(DevToolProbeLabel::Pending, None);
        assert_eq!(
            pending.pending_kinds(),
            [ChangeKind::Census, ChangeKind::Devtools]
        );
        pending.fda.0 = FdaProbe::pending();
        pending.debugger = DebuggerRow::refused(DebuggerProbeLabel::Pending);
        assert_eq!(
            pending.pending_kinds(),
            [
                ChangeKind::Fda,
                ChangeKind::Census,
                ChangeKind::Debugger,
                ChangeKind::Devtools
            ]
        );
    }

    fn app_data_model() -> aterm_spec::derive::Model {
        aterm_spec::ty_model! {
            AppDataFdaAuthority {
                const Buggy = 0;
                var grant = 0;
                var scope = 0;
                var adopted = 0;
                var decided = 0;
                var host_covered = 0;
                var session_covered = 0;
                action Grant when (decided == 0) { grant = 1; }
                action CurrentScope when (decided == 0) { scope = 1; }
                action FutureScope when (decided == 0) { scope = 2; }
                action Adopt when (decided == 0) { adopted = 1; }
                action Classify when (decided == 0) {
                    host_covered = if (grant == 1 && scope > 0) || Buggy == 1 { 1 } else { 0 };
                    session_covered = if Buggy == 1 && grant == 1 { 1 } else { 0 };
                    decided = 1;
                }
                invariant HostNeedsAuthority: host_covered == 0 || (grant == 1 && scope > 0);
                invariant NoInheritedGrant: adopted == 0 || session_covered == 0;
                invariant Bounds: grant <= 1 && scope <= 2 && adopted <= 1 && decided <= 1 && host_covered <= 1 && session_covered <= 1;
            }
        }
    }

    #[test]
    fn app_data_model_and_real_classifier_require_current_authority() {
        let model = app_data_model();
        aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
        for fda in [FdaState::Granted, FdaState::Denied, FdaState::Unknown] {
            for scope in [
                consent::FdaScope::Unknown,
                consent::FdaScope::ThisProcess,
                consent::FdaScope::NewProcesses,
            ] {
                for adoption in [Attribution::Live, Attribution::Adopted] {
                    let mut state = model.init_state();
                    if fda == FdaState::Granted {
                        assert!(model.fire("Grant", &mut state));
                    }
                    match scope {
                        consent::FdaScope::Unknown => {}
                        consent::FdaScope::ThisProcess => {
                            assert!(model.fire("CurrentScope", &mut state))
                        }
                        consent::FdaScope::NewProcesses => {
                            assert!(model.fire("FutureScope", &mut state))
                        }
                    }
                    if adoption == Attribution::Adopted {
                        assert!(model.fire("Adopt", &mut state));
                    }
                    let expected = model.successors("Classify", &state);
                    let mut projected = expected[0].clone();
                    projected.insert("host_covered", i64::from(app_data_covered(fda, scope)));
                    let posture = ConsentPosture::join(PostureInputs {
                        adoption,
                        fda,
                        responsible: Responsible::Unknown,
                        observed_eperm: false,
                        evidence: SpikeEvidence::UNMEASURED,
                    });
                    projected.insert(
                        "session_covered",
                        i64::from(posture.fs_consent == FsConsent::Covered),
                    );
                    assert!(expected.contains(&projected));
                    if adoption == Attribution::Adopted {
                        projected.insert("session_covered", 1);
                        assert!(!model.check_invariant("NoInheritedGrant", &projected));
                    }
                }
            }
            let split = covers_split(fda, SpikeEvidence::UNMEASURED);
            assert_eq!(split.covers.contains(&"app-data"), fda == FdaState::Granted);
            assert_eq!(
                split.unmeasured.contains(&"app-data"),
                fda != FdaState::Granted
            );
            assert_eq!(
                observed_fda_scope(fda, consent::FdaScope::Unknown),
                if fda == FdaState::Granted {
                    consent::FdaScope::ThisProcess
                } else {
                    consent::FdaScope::Unknown
                }
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use aterm_containment::consent::{
        DebuggerMember, DebuggerMode, DebuggerProbe, DebuggerProbeLabel, DevToolList,
        DevToolListLabel, DevToolProbe, DevToolProbeLabel, DrClass, FdaProbe, ProbeGate,
        ProbeOutcome, SelfHardening,
    };
    use aterm_types::MutexExt as _;
    use std::path::PathBuf;
    use std::sync::atomic::Ordering;
    use std::sync::{Mutex, OnceLock};

    use super::*;
    use crate::consent_state::{ChangeKind, ConsentProbes};
    const FIXTURE_DEBUGGER: DebuggerRow = DebuggerRow {
        probe: DebuggerProbe {
            mode: DebuggerMode::Disabled,
            member: DebuggerMember::Admin,
            label: DebuggerProbeLabel::StatusOk,
        },
        self_hardened: SelfHardening::Hardened,
        probe_age_ms: Some(1204),
    };

    const FIXTURE_DEVTOOLS: DevToolsRow = DevToolsRow {
        probe: DevToolProbe {
            state: aterm_containment::DevToolState::Unlisted,
            label: DevToolProbeLabel::Observed,
            requests: Some(233),
            not_allowed: Some(233),
            answered_at_ms: Some(1_791_381_501_820),
        },
        list: DevToolList {
            listing: aterm_containment::DevToolListing::Absent,
            label: DevToolListLabel::Read,
            modified_at_s: None,
            rows: Some(1),
        },
        subject: Some("com.aterm.aterm"),
        probe_age_ms: Some(812),
        answer_age_ms: Some(4100),
    };

    fn snapshot(sessions: usize, fda: FdaState, evidence: SpikeEvidence) -> PrivacySnapshot {
        snapshot_with_warmup(sessions, fda, evidence, &[])
    }

    fn fixture_identity() -> &'static SigningIdentity {
        static IDENTITY: OnceLock<SigningIdentity> = OnceLock::new();
        IDENTITY.get_or_init(|| SigningIdentity {
            bundle_id: Some("com.aterm.aterm".to_string()),
            reset_subject: None,
            display_name: Some("aterm".to_string()),
            signing: "developer-id",
            team: Some("A66A9P66Z7".to_string()),
            dr: DrClass::Identity,
            dev_build: Some(false),
        })
    }

    fn snapshot_with_warmup(
        sessions: usize,
        fda: FdaState,
        evidence: SpikeEvidence,
        warmup: &[(Folder, crate::consent_warmup::WarmupRow)],
    ) -> PrivacySnapshot {
        let split = covers_split(fda, evidence);
        let folders: Vec<(&'static str, &'static str)> = folder_names()
            .into_iter()
            .map(|name| (name, folder_state(name, warmup, &split.covers)))
            .collect();
        let folder_source = folder_source(&folders);
        PrivacySnapshot {
            folders,
            folder_source,
            claimants: None,
            census_pending: false,
            trash_tool: true,
            platform: "macos",
            os: Some("26.6.2"),
            identity: fixture_identity(),
            install: "installed",
            running: Some("/Applications/aterm.app/Contents/MacOS/aterm".to_string()),
            anchor: aterm_containment::ImageAnchor::Live,
            fda,
            probe: ProbeLabel::OpenEperm,
            probe_age_ms: Some(1840),
            evidence,
            split,
            attribution_root: Attribution::Live,
            sessions: (0..sessions)
                .map(|i| SessionRow {
                    sid: format!("s-{i}"),
                    attribution: if i == 1 {
                        Attribution::Adopted
                    } else {
                        Attribution::Live
                    },
                    responsible_pid: Some(4711),
                    responsible: Responsible::SelfProcess,
                    fs_consent: FsConsent::Unknown,
                    cwd: Some("/Users//a b/src".to_string()),
                })
                .collect(),
            containment_mode: "user".to_string(),
            seatbelt: "none",
            protected: vec!["~/one".to_string(), "~/two".to_string()],
            warmup: "on-request",
            warmup_last_ms: None,
            debugger: FIXTURE_DEBUGGER,
            devtools: FIXTURE_DEVTOOLS,
            observer_fda: "ok",
            observer_responsible: "ok",
            observer_log: "off",
            access_card: AccessCardReport {
                state: "quiet",
                record: crate::consent_card::AccessRecord::default(),
                needs: 0,
            },
        }
    }

    #[test]
    fn a_headless_app_wires_the_inert_consent_probe_arm() {
        let app = crate::App::headless_for_test();
        let (lines, pending) = app.privacy_report_for_test(PrivacyForm::Lines, Scope::Owner);
        let row = |prefix: &str| {
            lines
                .iter()
                .find(|l| l.starts_with(prefix))
                .unwrap_or_else(|| panic!("no {prefix} row: {lines:?}"))
                .clone()
        };
        let fda = row("full_disk_access=");
        assert!(
            fda.starts_with("full_disk_access=unknown probe=refused_disabled "),
            "{fda}"
        );
        assert!(row("observer ").contains(" responsible=off "));
        for prefix in ["debugger ", "devtools "] {
            let line = row(prefix);
            assert!(line.contains(" probe=refused_disabled "), "{line}");
        }
        assert!(!pending);
    }

    #[test]
    fn an_edge_sees_only_the_sessions_it_may_read() {
        use aterm_session::{EdgeToken, Op, SessionId};
        let mut app = crate::App::headless_for_test();
        let sibling = app.pool.insert(crate::stub_session(1));
        let grant = |app: &crate::App, op: Op| {
            let ctx = &app.pool.get(sibling).expect("the sibling").ctx;
            let mut table = ctx.edges.lock().unwrap_or_else(|p| p.into_inner());
            table.grant(SessionId::new("s-src"), ctx.self_id.clone(), op, ctx.nonce)
        };
        let read = Scope::Edge(grant(&app, Op::ReadScreen));
        let write = Scope::Edge(grant(&app, Op::WriteInput));
        let sibling_sid = app
            .pool
            .get(sibling)
            .expect("sid")
            .ctx
            .self_id
            .as_str()
            .to_string();
        let rows = |scope: Scope| -> Vec<String> {
            let lines = app.privacy_report_for_test(PrivacyForm::Lines, scope).0;
            assert!(
                lines.iter().any(|l| l.starts_with("full_disk_access=")),
                "the instance-wide posture answers every scope: {lines:?}"
            );
            lines
                .into_iter()
                .filter(|l| l.starts_with("session "))
                .collect()
        };
        assert_eq!(rows(Scope::Owner).len(), 2, "Owner sees every session");
        let seen = rows(read);
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert!(
            seen[0].starts_with(&format!("session sid={sibling_sid} ")),
            "{seen:?}"
        );
        assert!(rows(write).is_empty(), "a write edge reads no session");
        assert!(rows(Scope::Edge(EdgeToken::generate())).is_empty());
        let json = app
            .privacy_report_for_test(PrivacyForm::Json, read)
            .0
            .concat();
        assert_eq!(json.matches("\"sid\":").count(), 1, "{json}");
        assert!(json.contains(&sibling_sid), "{json}");
    }

    #[test]
    fn off_macos_the_report_claims_no_consent_layer_and_keeps_its_shape() {
        let mac = snapshot(1, FdaState::Unknown, SpikeEvidence::UNMEASURED);
        let mut linux = snapshot(1, FdaState::Unknown, SpikeEvidence::UNMEASURED);
        linux.platform = "linux";
        let (mac_lines, linux_lines) = (mac.lines(), linux.lines());
        assert_eq!(mac_lines.len(), linux_lines.len(), "the same rows");
        assert!(
            mac_lines.contains(&"prompt_possible=yes".to_string()),
            "control"
        );
        assert!(
            linux_lines.contains(&"prompt_possible=no".to_string()),
            "{linux_lines:?}"
        );
        assert!(
            linux_lines.contains(&"remediate fda=- files=- debugger=- devtools=-".to_string()),
            "{linux_lines:?}"
        );
        let containment = |lines: &[String]| {
            lines
                .iter()
                .find(|l| l.starts_with("containment "))
                .cloned()
                .unwrap()
        };
        assert!(
            containment(&linux_lines).ends_with(" protected=-"),
            "{linux_lines:?}"
        );
        assert!(
            !containment(&mac_lines).ends_with(" protected=-"),
            "control"
        );
        assert_eq!(
            linux_lines.last().unwrap(),
            &format!("note {}", pct_encode(NOTE_NO_OS_CONSENT))
        );
        let json = cmd_privacy_json(&linux);
        assert!(json.contains("\"prompt_possible\":false"), "{json}");
        assert!(json.contains("\"remediate\":{\"fda\":null"), "{json}");
        assert!(json.contains("\"protected\":[]"), "{json}");
    }

    #[test]
    fn every_live_session_gets_one_row_with_its_cwd_pct_encoded() {
        for count in [0, 1, 40] {
            let lines = snapshot(count, FdaState::Unknown, SpikeEvidence::UNMEASURED).lines();
            let rows: Vec<&String> = lines.iter().filter(|l| l.starts_with("session ")).collect();
            assert_eq!(rows.len(), count);
            assert!(
                lines
                    .iter()
                    .any(|l| l.contains(&format!(" sessions_total={count} "))),
                "{lines:?}"
            );
            if let Some(row) = rows.first() {
                assert!(row.ends_with(" cwd=/Users//a%20b/src"), "{row}");
            }
        }
    }

    #[test]
    fn the_line_body_is_the_documented_shape() {
        let keys = |line: &String| -> String {
            line.split(' ')
                .enumerate()
                .filter_map(|(i, field)| match field.split_once('=') {
                    Some((key, _)) => Some(key),
                    None => (i == 0).then_some(field),
                })
                .collect::<Vec<_>>()
                .join(" ")
        };
        let mut snap = snapshot(2, FdaState::Denied, SpikeEvidence::UNMEASURED);
        let mut got: Vec<String> = snap.lines().iter().map(keys).collect();
        let folder = std::iter::once("folder")
            .chain(folder_names())
            .chain(["source"])
            .collect::<Vec<_>>()
            .join(" ");
        let session = "session sid attribution responsible_pid responsible fs_consent cwd";
        let mut want: Vec<String> = [
            "schema",
            "platform os",
            "bundle_id display_name signing team dr grant_stable dev_build",
            "install running anchor",
            "full_disk_access probe probe_age_ms fda_scope",
            "covers",
            "uncovered",
            "unmeasured",
            &folder,
            "claimants conflicting census",
            "prompt_possible",
            "attribution_root sessions_total sessions_adopted",
            session,
            session,
            "containment mode seatbelt protected",
            "warmup warmup_last_ms",
            "debugger mode member self_hardened right probe probe_age_ms pending pending_right \
             pending_age_ms last watch_ms",
            "devtools state subject requests not_allowed answer_age_ms window_s probe \
             probe_age_ms grant list list_probe",
            "observer fda responsible log taskport",
            "access_card state asked_at opened_at verified_at renew_at needs",
            "remediate fda files debugger devtools",
            "note",
        ]
        .map(String::from)
        .to_vec();
        got.sort_unstable();
        want.sort_unstable();
        assert_eq!(got, want);

        let summary = |snap: &PrivacySnapshot| {
            snap.lines()
                .into_iter()
                .find(|l| l.starts_with("claimants="))
                .expect("a census row")
        };
        assert_eq!(summary(&snap), "claimants=- conflicting=- census=off");
        snap.census_pending = true;
        assert_eq!(summary(&snap), "claimants=- conflicting=- census=pending");
        snap.claimants = Some((
            census_answer(vec![claimant(
                "/Applications/aterm.app",
                DrClass::Identity,
                "developer-id",
                true,
            )]),
            Duration::ZERO,
        ));
        assert_eq!(
            summary(&snap),
            "claimants=1 conflicting=0 census=complete sole=yes age_ms=0"
        );
        let lines = snap.lines();
        assert!(
            !lines.iter().any(|l| l.starts_with("claimant ")),
            "{lines:?}"
        );
        assert_eq!(
            lines.iter().filter(|l| l.starts_with("note ")).count(),
            1,
            "only the closing note: {lines:?}"
        );
    }

    fn census_answer(found: Vec<aterm_containment::consent::Claimant>) -> Claimants {
        Claimants {
            found,
            enumeration: aterm_containment::consent::Enumeration::Complete,
        }
    }

    fn claimant(
        path: &str,
        dr: DrClass,
        signing: &'static str,
        running: bool,
    ) -> aterm_containment::consent::Claimant {
        aterm_containment::consent::Claimant {
            path: std::path::PathBuf::from(path),
            dr,
            dr_text: format!("designated => {} {path}", dr.as_str()),
            signing,
            team: None,
            running,
            links: Vec::new(),
        }
    }

    #[test]
    fn the_census_rows_offer_the_trash_only_for_what_the_move_takes() {
        let mut snap = snapshot(1, FdaState::Granted, SpikeEvidence::UNMEASURED);
        let mut linked = claimant("/Users//a/dev/aterm.app", DrClass::Cdhash, "adhoc", false);
        linked.links = vec![PathBuf::from("/Applications/aterm-dev.app")];
        let census = census_answer(vec![
            claimant(
                "/Applications/aterm.app",
                DrClass::Identity,
                "developer-id",
                true,
            ),
            claimant(
                "/Applications/aterm.app.rollback",
                DrClass::Cdhash,
                "adhoc",
                false,
            ),
            linked,
            claimant(
                "/Users//a/pinned/aterm.app",
                DrClass::Unknown,
                "adhoc",
                false,
            ),
        ]);
        let moved: Vec<PathBuf> = census.retirable().iter().map(|c| c.path.clone()).collect();
        assert_eq!(moved, [PathBuf::from("/Applications/aterm.app.rollback")]);
        snap.claimants = Some((census, Duration::from_millis(1200)));
        let offered = |snap: &PrivacySnapshot| -> Vec<PathBuf> {
            let lines = snap.lines();
            assert!(
                !lines
                    .iter()
                    .any(|l| l.starts_with("claimant path=/Applications/aterm.app ")),
                "the running copy is the reference: {lines:?}"
            );
            let text: Vec<PathBuf> = lines
                .iter()
                .filter_map(|line| line.strip_prefix("claimant path="))
                .filter_map(|row| {
                    let (path, fields) = row.split_once(' ').expect("fields");
                    let trash = fields
                        .split(' ')
                        .find_map(|field| field.strip_prefix("trash="))
                        .expect("every row says whether the Trash takes it");
                    (trash == "offered").then(|| PathBuf::from(path))
                })
                .collect();
            let json: aterm_json::Value =
                aterm_json::from_str(cmd_privacy_json(snap).lines().nth(1).expect("JSON body"))
                    .expect("the JSON parses");
            let json: Vec<PathBuf> = json["census"]["conflicts"]
                .as_array()
                .expect("an answered census lists its conflicts")
                .iter()
                .filter_map(|row| {
                    let trash = row["trash"]
                        .as_str()
                        .expect("every conflict says whether the Trash takes it");
                    (trash == "offered").then(|| PathBuf::from(row["path"].as_str().expect("path")))
                })
                .collect();
            assert_eq!(text, json, "both forms read one rule");
            text
        };
        assert_eq!(offered(&snap), moved, "only what the move takes is offered");
        snap.trash_tool = false;
        assert_eq!(offered(&snap), Vec::<PathBuf>::new(), "no tool, no offer");
    }

    #[test]
    fn the_json_census_parses_in_both_states_and_is_capped() {
        let keys = [
            "state",
            "claimants",
            "conflicting",
            "sole",
            "age_ms",
            "conflicts",
            "truncated",
        ];
        let snap = snapshot(1, FdaState::Granted, SpikeEvidence::UNMEASURED);
        let json: aterm_json::Value =
            aterm_json::from_str(cmd_privacy_json(&snap).lines().nth(1).expect("JSON body"))
                .expect("pending json parses");
        for key in keys {
            assert!(
                json["census"].get(key).is_some(),
                "{key} in the unanswered form"
            );
        }
        assert_eq!(json["census"]["state"].as_str(), Some("off"));

        let mut snap = snapshot(1, FdaState::Granted, SpikeEvidence::UNMEASURED);
        let mut found = vec![claimant(
            "/A/aterm.app",
            DrClass::Identity,
            "developer-id",
            true,
        )];
        for n in 0..(aterm_containment::consent::MAX_CLAIMANT_ROWS + 3) {
            found.push(claimant(
                &format!("/A/copy-{n}.app"),
                DrClass::Cdhash,
                "adhoc",
                false,
            ));
        }
        snap.claimants = Some((census_answer(found), Duration::from_millis(7)));
        let json: aterm_json::Value =
            aterm_json::from_str(cmd_privacy_json(&snap).lines().nth(1).expect("JSON body"))
                .expect("answered json parses");
        for key in keys {
            assert!(
                json["census"].get(key).is_some(),
                "{key} in the answered form"
            );
        }
        assert_eq!(
            json["census"]["conflicts"].as_array().map(Vec::len),
            Some(aterm_containment::consent::MAX_CLAIMANT_ROWS)
        );
        assert_eq!(json["census"]["truncated"].as_u64(), Some(3));
        assert_eq!(json["census"]["conflicting"].as_u64(), Some(11));
    }

    #[test]
    fn no_unmeasured_folder_state_is_ever_inferred_from_the_grant() {
        const STILL_UNMEASURED: &[&str] = &[
            "documents",
            "desktop",
            "downloads",
            "network-volumes",
            "removable-volumes",
        ];
        for fda in [FdaState::Granted, FdaState::Denied, FdaState::Unknown] {
            let lines = snapshot(1, fda, SpikeEvidence::UNMEASURED).lines();
            let folder = lines
                .iter()
                .find(|l| l.starts_with("folder "))
                .expect("a folder row");
            for name in STILL_UNMEASURED {
                assert!(
                    folder.contains(&format!("{name}=unknown")),
                    "{name} has no measurement of its own: {folder}"
                );
            }
            let expected = if fda == FdaState::Granted {
                "app-data=covered-by-fda"
            } else {
                "app-data=unknown"
            };
            assert!(folder.contains(expected), "{fda:?}: {folder}");
            let expected_source = if fda == FdaState::Granted {
                " source=fda"
            } else {
                " source=none"
            };
            assert!(folder.ends_with(expected_source), "{fda:?}: {folder}");
        }
    }

    #[test]
    fn an_observed_warmup_row_outranks_the_coverage_rule_and_reaches_the_wire() {
        use crate::consent_warmup::WarmupRow;
        let warmed = [
            (Folder::Documents, WarmupRow::Denied),
            (Folder::Desktop, WarmupRow::Allowed),
            (Folder::AppData, WarmupRow::Denied),
        ];
        let lines =
            snapshot_with_warmup(1, FdaState::Granted, SpikeEvidence::UNMEASURED, &warmed).lines();
        let folder = lines
            .iter()
            .find(|l| l.starts_with("folder "))
            .expect("a folder row");
        assert!(folder.contains("documents=denied"), "{folder}");
        assert!(folder.contains("desktop=allowed"), "{folder}");
        assert!(folder.contains("app-data=denied"), "{folder}");
        assert!(folder.contains("downloads=unknown"), "{folder}");
        assert!(folder.ends_with(" source=warmup"), "{folder}");

        let asking = [(Folder::Documents, WarmupRow::Asking)];
        let lines =
            snapshot_with_warmup(1, FdaState::Denied, SpikeEvidence::UNMEASURED, &asking).lines();
        let folder = lines
            .iter()
            .find(|l| l.starts_with("folder "))
            .expect("a folder row");
        assert!(folder.contains("documents=asking"), "{folder}");
        assert!(folder.ends_with(" source=warmup"), "{folder}");
    }

    fn fake_debugger(runs: &std::sync::atomic::AtomicUsize) -> DebuggerProbe {
        runs.fetch_add(1, Ordering::AcqRel);
        DebuggerProbe {
            mode: DebuggerMode::Disabled,
            member: DebuggerMember::Admin,
            label: DebuggerProbeLabel::StatusOk,
        }
    }

    fn fake_devtools(runs: &std::sync::atomic::AtomicUsize, at_ms: i64) -> DevToolProbe {
        runs.fetch_add(1, Ordering::AcqRel);
        DevToolProbe {
            state: aterm_containment::DevToolState::Unlisted,
            label: DevToolProbeLabel::Observed,
            requests: Some(3),
            not_allowed: Some(3),
            answered_at_ms: Some(at_ms),
        }
    }

    fn fake_subject() -> Option<&'static str> {
        Some("com.example.subject")
    }

    #[test]
    fn the_privacy_facts_hop_makes_no_os_call() {
        use std::sync::atomic::AtomicUsize;
        static FDA: AtomicUsize = AtomicUsize::new(0);
        static RESPONSIBLE: AtomicUsize = AtomicUsize::new(0);
        static CENSUS: AtomicUsize = AtomicUsize::new(0);
        static DEBUGGER: AtomicUsize = AtomicUsize::new(0);
        static HARDENING: AtomicUsize = AtomicUsize::new(0);
        static DEVTOOLS: AtomicUsize = AtomicUsize::new(0);
        static LIST: AtomicUsize = AtomicUsize::new(0);
        fn fda(_gate: ProbeGate) -> FdaProbe {
            FDA.fetch_add(1, Ordering::AcqRel);
            consent::classify_probe(ProbeOutcome::Ok)
        }
        fn responsible(pids: &[i32]) -> Vec<Result<i32, ResponsibleError>> {
            RESPONSIBLE.fetch_add(1, Ordering::AcqRel);
            pids.iter().map(|&pid| Ok(pid)).collect()
        }
        fn census(_bundle_id: &str) -> Claimants {
            CENSUS.fetch_add(1, Ordering::AcqRel);
            Claimants::default()
        }
        fn debugger(_gate: ProbeGate) -> DebuggerProbe {
            fake_debugger(&DEBUGGER)
        }
        fn hardening() -> SelfHardening {
            HARDENING.fetch_add(1, Ordering::AcqRel);
            SelfHardening::Hardened
        }
        fn devtools(_gate: ProbeGate, _subject: &str) -> DevToolProbe {
            fake_devtools(&DEVTOOLS, 0)
        }
        fn list(_gate: ProbeGate, _subject: &str) -> DevToolList {
            LIST.fetch_add(1, Ordering::AcqRel);
            DevToolList::refused(DevToolListLabel::RefusedDisabled)
        }
        let counts = || {
            [
                &FDA,
                &RESPONSIBLE,
                &CENSUS,
                &DEBUGGER,
                &HARDENING,
                &DEVTOOLS,
                &LIST,
            ]
            .map(|runs| runs.load(Ordering::Acquire))
        };
        let mut app = crate::App::headless_for_test();
        *app.consent.probes_for_test() = ConsentProbes {
            fda,
            responsible,
            census,
            debugger,
            hardening,
            devtools,
            devtools_list: list,
            subject: fake_subject,
            warm_subject: fake_subject,
            live: true,
        };
        assert!(
            app.consent.policy().gate.permits() && app.consent_policy().gate.permits(),
            "the gate is open, so only the hop can keep the arms quiet"
        );

        let facts = app.privacy_facts(Scope::Owner);
        assert_eq!(facts.sessions.len(), 1, "the hop read the pool");
        assert_eq!(counts(), [0; 7], "the hop made an OS call");

        let identity = SigningIdentity {
            bundle_id: Some("com.example.subject".to_string()),
            ..SigningIdentity::unknown().clone()
        };
        let consent = &facts.consent;
        let seen = consent.publication();
        let slots = consent.report_slots(&facts.policy, &identity);
        assert_eq!(
            slots.pending_kinds(),
            [
                ChangeKind::Fda,
                ChangeKind::Census,
                ChangeKind::Debugger,
                ChangeKind::Devtools
            ],
            "every slot dispatched its worker"
        );
        assert_eq!(facts.responsible_answers().len(), 1);
        for kind in slots.pending_kinds() {
            assert!(
                consent.wait_for_change(seen, &[kind], Duration::from_secs(60)),
                "the {kind:?} worker publishes"
            );
        }
        assert!(counts().iter().all(|&runs| runs >= 1), "{:?}", counts());
    }

    #[test]
    fn the_report_asks_the_spi_before_it_reads_a_slot() {
        use std::sync::atomic::AtomicUsize;
        static SLOT_READS: AtomicUsize = AtomicUsize::new(0);
        static READS_AT_THE_SPI: AtomicUsize = AtomicUsize::new(usize::MAX);
        fn responsible(pids: &[i32]) -> Vec<Result<i32, ResponsibleError>> {
            READS_AT_THE_SPI.store(SLOT_READS.load(Ordering::Acquire), Ordering::Release);
            pids.iter().map(|&pid| Ok(pid)).collect()
        }
        fn subject() -> Option<&'static str> {
            SLOT_READS.fetch_add(1, Ordering::AcqRel);
            None
        }
        let mut app = crate::App::headless_for_test();
        app.consent.probes_for_test().live = true;
        app.consent.probes_for_test().responsible = responsible;
        app.consent.probes_for_test().subject = subject;
        let facts = app.privacy_facts(Scope::Owner);
        assert_eq!(facts.sessions.len(), 1, "a session to ask about");
        let snapshot = facts.report(SigningIdentity::unknown());
        assert_eq!(
            READS_AT_THE_SPI.load(Ordering::Acquire),
            0,
            "the SPI was asked after a slot read"
        );
        assert!(
            SLOT_READS.load(Ordering::Acquire) >= 1,
            "the report read the slots"
        );
        assert!(
            snapshot.sessions[0].responsible_pid.is_some(),
            "the SPI's answer is joined"
        );
    }

    #[test]
    fn the_report_asks_the_spi_once_for_every_session() {
        static ASKED: Mutex<Vec<Vec<i32>>> = Mutex::new(Vec::new());
        fn responsible(pids: &[i32]) -> Vec<Result<i32, ResponsibleError>> {
            ASKED.lock_or_recover().push(pids.to_vec());
            pids.iter().map(|&pid| Ok(pid + 1000)).collect()
        }
        let mut app = crate::App::headless_for_test();
        app.consent.probes_for_test().responsible = responsible;
        let with_sessions = |mut facts: PrivacyAppFacts| {
            let first = facts.sessions[0].clone();
            for (sid, pid) in [("s-a", 7001), ("s-b", 7002)] {
                facts.sessions.push(SessionFacts {
                    sid: sid.to_string(),
                    pid,
                    ..first.clone()
                });
            }
            facts
        };

        let facts = with_sessions(app.privacy_facts(Scope::Owner));
        assert!(facts.policy.report_attribution);
        let pids: Vec<i32> = facts.sessions.iter().map(|s| s.pid).collect();
        let snapshot = facts.report(SigningIdentity::unknown());
        assert_eq!(
            *ASKED.lock_or_recover(),
            std::slice::from_ref(&pids),
            "one call, every pid, in session order"
        );
        let joined: Vec<Option<i32>> = snapshot
            .sessions
            .iter()
            .map(|s| s.responsible_pid)
            .collect();
        let want: Vec<Option<i32>> = pids.iter().map(|&pid| Some(pid + 1000)).collect();
        assert_eq!(joined, want, "each answer is joined to its own session");
        assert_eq!(snapshot.observer_responsible, "ok");

        let mut facts = with_sessions(app.privacy_facts(Scope::Owner));
        facts.policy.report_attribution = false;
        let snapshot = facts.report(SigningIdentity::unknown());
        assert_eq!(ASKED.lock_or_recover().len(), 1, "the SPI was asked again");
        assert_eq!(snapshot.observer_responsible, "off");
        assert!(
            snapshot
                .sessions
                .iter()
                .all(|s| s.responsible_pid.is_none())
        );
    }

    #[test]
    fn an_unvalidatable_identity_is_reported_and_does_not_hide_behind_a_granted_probe() {
        use aterm_containment::ImageAnchor;
        for (anchor, notes) in [
            (ImageAnchor::Live, 1),
            (ImageAnchor::NotBundled, 1),
            (ImageAnchor::Unknown, 1),
            (ImageAnchor::Displaced, 2),
            (ImageAnchor::Deleted, 2),
        ] {
            let mut snap = snapshot(1, FdaState::Granted, SpikeEvidence::UNMEASURED);
            snap.anchor = anchor;
            let lines = snap.lines();
            assert!(
                lines
                    .iter()
                    .any(|l| l.ends_with(&format!(" anchor={}", anchor.as_str()))),
                "{anchor:?}: {lines:?}"
            );
            assert_eq!(
                lines.iter().filter(|l| l.starts_with("note ")).count(),
                notes,
                "{anchor:?}: {lines:?}"
            );
        }
    }

    #[test]
    fn the_remediate_row_names_only_places() {
        let lines = snapshot(1, FdaState::Denied, SpikeEvidence::UNMEASURED).lines();
        let remediate = lines
            .iter()
            .find_map(|l| l.strip_prefix("remediate "))
            .expect("a remediate row");
        for field in remediate.split(' ') {
            let (_, place) = field.split_once('=').expect("k=v");
            assert!(
                place.starts_with("settings:") || place.starts_with("aterm-settings:"),
                "{field}"
            );
        }
        assert_eq!(
            lines.last().expect("a note row").split(' ').count(),
            2,
            "one field after `note`"
        );
    }

    #[test]
    fn unavailable_is_distinct_from_off_and_from_a_negative_answer() {
        for (label, want) in [
            (ProbeLabel::RefusedDisabled, "off"),
            (ProbeLabel::RefusedOutOfBundle, "unavailable"),
            (ProbeLabel::RefusedNoHome, "unavailable"),
            (ProbeLabel::UnsupportedPlatform, "unavailable"),
            (ProbeLabel::OpenOk, "ok"),
            (ProbeLabel::OpenEperm, "ok"),
            (ProbeLabel::OpenErrno(13), "ok"),
        ] {
            assert_eq!(observer_fda_value(label), want, "{label:?}");
        }
        for (answers, want) in [
            (&[Err(ResponsibleError::Unsupported)][..], "off"),
            (
                &[Err(ResponsibleError::SymbolUnavailable)][..],
                "unavailable",
            ),
            (&[Err(ResponsibleError::NotOurs), Ok(1)][..], "ok"),
            (&[][..], "off"),
        ] {
            assert_eq!(observer_responsible_value(answers), want, "{answers:?}");
        }
    }

    #[test]
    fn the_access_card_row_reports_where_the_question_stands() {
        use crate::consent_card::{AccessRecord, Marker, RENEW_AFTER, report_state};
        let asked = AccessRecord {
            asked_at: Some(1_000),
            ..AccessRecord::default()
        };
        let week = 1_000 + RENEW_AFTER.as_secs();
        let mut snap = snapshot(0, FdaState::Denied, SpikeEvidence::UNMEASURED);
        snap.access_card = AccessCardReport {
            state: "asked",
            record: asked,
            needs: 3,
        };
        let row = snap
            .lines()
            .into_iter()
            .find(|l| l.starts_with("access_card "))
            .expect("an access_card row");
        let renew = format!("renew_at={week}");
        for field in [renew.as_str(), "verified_at=-", "needs=3"] {
            assert!(row.split(' ').any(|f| f == field), "{field}: {row}");
        }
        for (marker, record, fda, now, want) in [
            (None, AccessRecord::default(), FdaState::Denied, 5, "quiet"),
            (
                Some(Marker::NotNow),
                asked,
                FdaState::Granted,
                5,
                "declined",
            ),
            (None, asked, FdaState::Granted, 5, "granted"),
            (None, asked, FdaState::Denied, 1_500, "asked"),
            (None, asked, FdaState::Denied, week, "quiet"),
        ] {
            assert_eq!(report_state(marker, &record, fda, now), want, "{want}");
        }
    }

    #[test]
    fn the_json_form_carries_the_documented_sub_objects() {
        let parse = |snap: &PrivacySnapshot| -> aterm_json::Value {
            let body = cmd_privacy_json(snap);
            assert!(body.starts_with("OK 1\n"), "{body}");
            assert_eq!(body.lines().count(), 2, "a JSON reply is one body line");
            aterm_json::from_str(body.lines().nth(1).expect("a body line")).expect("it parses")
        };
        let observed = snapshot(2, FdaState::Denied, SpikeEvidence::UNMEASURED);
        let json = parse(&observed);
        for key in ["folders", "debugger", "devtools", "observers", "remediate"] {
            assert!(json[key].is_object(), "{key}: {json:?}");
        }
        assert_eq!(json["sessions"].as_array().map(Vec::len), Some(2));
        assert_eq!(json["sessions"][0]["cwd"].as_str(), Some("/Users//a b/src"));
        assert_eq!(json["debugger"]["probe_age_ms"].as_u64(), Some(1204));
        assert_eq!(json["devtools"]["answer_age_ms"].as_u64(), Some(4100));

        let mut unobserved = observed.clone();
        unobserved.debugger = DebuggerRow::awaiting(SelfHardening::Unknown);
        unobserved.devtools = DevToolsRow::refused(DevToolProbeLabel::Pending, None);
        let pending = parse(&unobserved);
        for row in ["debugger", "devtools"] {
            let keys = |v: &aterm_json::Value| -> Vec<String> {
                v[row]
                    .as_object()
                    .expect("an object")
                    .keys()
                    .cloned()
                    .collect()
            };
            assert_eq!(keys(&pending), keys(&json), "{row}: no key dropped");
            assert!(pending[row]["probe_age_ms"].is_null(), "{row}");
        }
        assert!(pending["devtools"]["subject"].is_null());
    }
}
