// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Swarm survey facts (C16): host label, version, window liveness (`window` / `window_last`),
//! session count, per-row account health/enrollment/usage/last-use across pooled CLIs, pure-Rust
//! SSH host-key `SHA256:…` fingerprint, vault-server subscriber last-heard records, and the
//! machine's look at its own resources ([`MachineFacts`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use aterm_capacity::machine_look::{Limits, LoadScore, MachineLook};
use aterm_sysprobe::MemoryLevel;
use aterm_types::look_words;

use crate::ai::connector::Cli;
use crate::harness::accounts;
use crate::harness::cli::{Env, PoolMoves, SurveyPool};
use crate::harness::repo_work::home::{self as repo_home, HomeRepo, HomeRepos};
use aterm_types::update_standing::UpdateFact;

const WINDOW_LAST_FILE: &str = "window-last";
const SSH_PUBKEY_MAX: usize = 16 << 10;

/// Per-account health, enrollment, usage and last-use observation carried in survey facts (§6.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurveyAccountFact {
    /// Pooled CLI identifier (`claude`, `codex`, `grok`, `kimi`).
    pub pool: String,
    /// Account label in the roster (`accounts.toml` / `accounts-<cli>.toml`): the key `--host any`
    /// and the fleet census match rows by.
    pub id: String,
    /// The account's address, the name a reader shows it by (`email=`; [`SurveyAccount::email`]).
    /// `None` where the row and its place name none, and from an aterm older than the field.
    pub email: Option<String>,
    /// Canonical [`accounts::Condition`] word (`usable`, `limited`, `low`, `not-enrolled`, …).
    pub condition: String,
    /// Whether the login directory is enrolled on this machine.
    pub enrolled: bool,
    /// Active 5-hour window utilization percent, when observed and still running.
    pub usage_5h_pct: Option<u32>,
    /// Active 7-day weekly utilization percent, when observed and still running.
    pub usage_7d_pct: Option<u32>,
    /// Unix timestamp (seconds) when the weekly window resets, when known. Where its read named no
    /// reset, the latest it can: a week from that read.
    pub week_reset: Option<i64>,
    /// Unix timestamp (seconds) until which a standing wall blocks this account, when walled.
    pub walled_until: Option<i64>,
    /// Each model's own weekly wall standing on it, by its bucket, with when it ends, Unix
    /// seconds (`model_walls=<bucket>:<s>,…`,
    /// [`crate::harness::cli::SurveyAccount::model_walls`]): it walls that model's sessions
    /// alone, so it is in neither [`Self::condition`] nor [`Self::walled_until`]. Empty where none
    /// stands, and from an aterm older than the field (no `model_walls=`, or `model_walls=-`).
    pub model_walls: BTreeMap<String, i64>,
    /// Unix timestamp (seconds) when a session here last ran on this account.
    pub last_used: Option<i64>,
    /// Unix timestamp (seconds) when its usage figures were last read, when known. A row whose
    /// reading is old, or no newer than its newest wall, says `unseen`, not `usable`
    /// ([`crate::harness::cli::survey_accounts`]).
    pub read_at: Option<i64>,
    /// Whether a worker with no starter of its kind would start on it here (`launch=1|0`,
    /// [`crate::harness::accounts_pool::unstarted_launch_row`]). `None` from an aterm older than
    /// the field, which says nothing of it (no `launch=`, or `launch=-`).
    pub launch: Option<bool>,
    /// Claude Code's row: its vault token's state here
    /// ([`crate::harness::accounts_tokens::TokenState::word`], `docs/DESIGN-claude-setup-token-2026-11-05.md`
    /// §5.10). `None` for another pool, on a Mac with no vault configured, and from an aterm older
    /// than the field (no `token=`, or `token=-`).
    pub token: Option<String>,
}

/// One subscriber last-heard record from a vault server running on this machine (§7.2, C16).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerHeardFact {
    /// Vault name (`alab`).
    pub vault: String,
    /// Subscriber `age1…` recipient.
    pub subject: String,
    /// Unix timestamp (seconds) of the last envelope GET / long-poll carrying that subscriber.
    pub at: u64,
}

/// Complete C16 survey facts for one machine, serializable onto and parseable from `survey` wire
/// replies (`OK <n>\ninstance …\n…`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwarmSurveyFacts {
    /// Host-checked release process identities, the release pump's fact (`release_pump`) and
    /// bounded local GitHub quota summaries.
    pub operation_lines: Vec<String>,
    /// Fleet node id (`node=` on `instance`), when enrolled in a fleet.
    pub node: Option<String>,
    /// Canonical machine label (`m<N>` or configured `[ai]`/`[fleet] machine` / vault host).
    pub label: Option<String>,
    /// The roster was unread this pass; a missing label is unknown rather than absent.
    pub roster_unread: bool,
    /// Running `aterm` version string.
    pub version: String,
    /// Whether a GUI window instance is live right now (`1` in a window host, `0` headless).
    pub window: bool,
    /// Unix timestamp (seconds) when a window last ran here (`<state>/window-last`).
    pub window_last: Option<u64>,
    /// Live session count in this instance.
    pub sessions: usize,
    /// OpenSSH host public key fingerprint (`SHA256:<unpadded-base64>`), read in pure Rust.
    pub ssh_host_key: Option<String>,
    /// Per-row account facts across pooled CLIs, in `Cli::ALL` × roster order, each as `aterm
    /// harness accounts --json` reads it ([`crate::harness::cli::survey_accounts`]).
    pub accounts: Vec<SurveyAccountFact>,
    /// The pooled CLIs whose account records could not be read (`accounts_unread=`): a reader
    /// takes such a pool's rows as unknown, never as none.
    pub accounts_unread: Vec<String>,
    /// The account each supervised session was last read running on, by pool and session id: its
    /// row's address, else its label. Not on the wire: this instance's own session rows name it.
    pub session_accounts: BTreeMap<(String, String), String>,
    /// What this Mac's ledgers say of rotations (`moves_7d=`, `last_move=`). `None` from an aterm
    /// whose facts predate them or with a pool it could not read, which a reader takes as unknown,
    /// never as none.
    pub moves: Option<MoveFacts>,
    /// Subscriber last-heard records when this machine serves vaults (`~/.config/aterm/secrets/served`).
    pub server_heard: Vec<ServerHeardFact>,
    /// The home this aterm runs under, absolute (`home=`): where a fleet spawn's `cwd=~/…`
    /// resolves. `None` from an aterm that says nothing of it, which a reader takes as unknown.
    pub home: Option<String>,
    /// The home these facts were collected for, absolute, as the path it is: the volume a look
    /// reads ([`Self::take_latest_look`]). Not on the wire, and read from a path [`Self::home`]
    /// cannot spell (one that is not UTF-8) as from any other.
    pub home_dir: Option<PathBuf>,
    /// Free space on the home's volume, in MiB (`home_free_mb=`): read with the facts beside the
    /// volume's size ([`Self::home_total_mb`]), and with a look ([`Self::machine`]) both are the
    /// look's own figures ([`MachineLook::disk_free_mb`], [`MachineLook::disk_total_mb`]), so the
    /// look's limit verdict travels beside the free space it was judged against.
    pub home_free_mb: Option<u64>,
    /// The size of the home's volume, in MiB (`home_total_mb=`, right after `home_free_mb=`): read
    /// in the one volume read that says its free space, and with a look the look's own. `None`
    /// where no volume was read or it read a size of 0, and from an aterm older than the key
    /// (one whose look said it among the look's keys still reads): unknown, never 0. A size, no
    /// path.
    pub home_total_mb: Option<u64>,
    /// The build output aterm sizes as rebuildable on the home's volume (`home_build_mb=`,
    /// `home_build_at_ms=`, `home_build_partial=`, after `home_total_mb=`): the figure the disk
    /// watch's last sizing of that volume kept in its record
    /// ([`crate::harness::disk::budget::Record`]), read and never walked, with when that sizing
    /// ended, so a reader sees how old it is. `None` where no sizing of this volume is recorded
    /// (none ran yet, or the record is another volume's), and from an aterm older than the keys:
    /// unknown, never 0. Sizes and a time, no path.
    pub home_build: Option<HomeBuild>,
    /// The fullest of the stores aterm itself keeps that refuse writes at a bound
    /// (`store_worst=`, `store_worst_pct=`, the header's last keys): which store by its fixed
    /// word, and the share of its bound it holds. The window or node job that answers sets it
    /// ([`StoreWorst`]); collected facts carry none. `None` where no such store was sized, and
    /// from an aterm older than the keys: unknown, never 0. A word and a share, no path.
    pub store_worst: Option<StoreWorst>,
    /// This machine's look at its own resources, with its OS, its target and since when it has
    /// been up (the header's `os=` to `gpu_lim=`, [`MachineFacts::header_tokens`]). An answer
    /// takes it ([`Self::take_latest_look`]); collected facts carry none, so the 3-minute facts
    /// push, which no reader of a look reads, takes none. `None` there and from an aterm whose
    /// facts predate the look (no `m_at_ms=`): every figure is then unknown, never zero.
    pub machine: Option<MachineFacts>,
    /// The checkouts aterm's setup clones into the home, as its last read found them
    /// ([`repo_home::read_home`]: `repos=`, `repos_at=`, `repos_cut=` and one `repo` line
    /// each); `None` before a read finished, which a reader takes as unknown, never as none.
    pub repos: Option<HomeRepos>,
    /// Where this Mac's updater stands, as `aterm update status` decides it (the `update*=`
    /// words, [`UpdateFact::tokens`]): a resource fact, carried at both levels. `None` where the
    /// collector read no updater ([`Self::collect`] reads none; the window and the node job fill
    /// it) and from an aterm older than the words, which a reader takes as unknown.
    pub update: Option<UpdateFact>,
}

/// What one machine says of itself in a survey header beside its sessions and accounts: its
/// look at its own resources ([`MachineLook`]: integers in fixed units, capacity beside use, the
/// four limit verdicts), its OS, the target its packages are built for, and since when it has
/// been up. Every figure is `None` where it was not read or not sent; none is a zero that stands
/// for unread, and there is no default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineFacts {
    /// The look, kept [`aterm_capacity::machine_look::LOOK_KEEP`] per process.
    pub look: MachineLook,
    /// Since when the machine has been up, Unix ms on its own clock (`up_since_ms=`): the later
    /// of its boot and its last wake ([`crate::harness::upgrade_drive::system_awake_since`]),
    /// so a Mac back from sleep says a new one. Never the start of the answering process, which
    /// every update replaces. `None` where the OS says neither.
    pub up_since_ms: Option<u64>,
    /// `macos`, `linux` or `windows` (`os=`); `None` for any other word.
    pub os: Option<&'static str>,
    /// The target triple atpkg selects this machine's builds by (`target=`, one of
    /// `atpkg::TARGETS`); `None` on a machine atpkg has no target for.
    pub target: Option<String>,
}

impl MachineFacts {
    /// This machine now: the process's kept look with the volume holding `home`
    /// ([`aterm_capacity::machine_look::latest`]; `None` reads no volume), and what the OS says
    /// of itself. It reads in process and starts nothing.
    #[must_use]
    pub fn now(home: Option<&Path>) -> Self {
        Self {
            look: aterm_capacity::machine_look::latest(home),
            up_since_ms: up_since_ms(crate::harness::upgrade_drive::system_awake_since()),
            os: look_words::os_of(std::env::consts::OS),
            target: Some(atpkg::cli::current_triple())
                .filter(|triple| atpkg::TARGETS.contains(triple))
                .map(str::to_owned),
        }
    }

    /// Space-prefixed `key=value` tokens for the `instance` header, `os=` to `load=`: each
    /// figure a whole number in the unit its key names, `-` where it is unread. The home
    /// volume's free space and size are not among them: they are the header's `home_free_mb=`
    /// and `home_total_mb=` ([`SwarmSurveyFacts::header_tokens`]), which a header says with no
    /// look too, and which readers find by key. `load=<level>:<over>` is the deck dial's
    /// system-load score in hundredths ([`LoadScore`]), `load=-` where its level is unread and
    /// `load=<level>:-` where its over is.
    #[must_use]
    pub fn header_tokens(&self) -> String {
        let look = &self.look;
        let load = |at: usize| opt_u32_token(look.load_x100.map(|load| load[at]));
        let limit = |at: Option<bool>| at.map_or("-", |at| if at { "1" } else { "0" });
        format!(
            " os={} target={} m_at_ms={} cores={} load1_x100={} load5_x100={} load15_x100={} \
             cpu_busy_x100={} rate_win_ms={} mem_total_mb={} mem_used_mb={} mem_avail_mb={} \
             mem_pressure={} mem_psi60_x100={} swap_used_mb={} swap_total_mb={} gpu_cores={} \
             gpu_busy_pct={} net_link_kbps={} net_rx_kbps={} net_tx_kbps={} net_other_kbps={} \
             up_since_ms={} cpu_lim={} mem_lim={} disk_lim={} gpu_lim={} load={}",
            self.os.unwrap_or("-"),
            word_token(self.target.as_deref()),
            opt_u64_token(look.at_unix_ms),
            opt_u32_token(look.cores),
            load(0),
            load(1),
            load(2),
            opt_u64_token(look.cpu_busy_x100),
            opt_u64_token(look.rate_win_ms),
            opt_u64_token(look.mem_total_mb),
            opt_u64_token(look.mem_used_mb),
            opt_u64_token(look.mem_avail_mb),
            look.mem_pressure.map_or("-", MemoryLevel::as_str),
            opt_u32_token(look.mem_psi60_x100),
            opt_u64_token(look.swap_used_mb),
            opt_u64_token(look.swap_total_mb),
            opt_u32_token(look.gpu_cores),
            opt_u32_token(look.gpu_busy_pct.map(u32::from)),
            opt_u64_token(look.net_link_kbps),
            opt_u64_token(look.net_rx_kbps),
            opt_u64_token(look.net_tx_kbps),
            opt_u64_token(look.net_other_kbps),
            opt_u64_token(self.up_since_ms),
            limit(look.limits.cpu),
            limit(look.limits.mem),
            limit(look.limits.disk),
            limit(look.limits.gpu),
            load_token(look.load),
        )
    }

    /// What an `instance` header says of its machine, with the free space and the size its
    /// `home_free_mb=` and `home_total_mb=` said: `None` where it has no `m_at_ms=` (an aterm
    /// whose facts predate the look). Each key is read by itself and leniently: one that is
    /// absent, `-`, not a whole number in its range or not a word of its closed set is `None`,
    /// and the rest still read. A size or a core count of 0 is no capacity and reads `None`; the
    /// three loads read together or not at all, and so do the load score's two parts
    /// ([`load_of`]).
    fn of_header(
        header: &[(String, String)],
        home_free_mb: Option<u64>,
        home_total_mb: Option<u64>,
    ) -> Option<Self> {
        field_raw(header, "m_at_ms")?;
        let n = |key: &str| lenient_u64(header, key);
        let n32 = |key: &str| n(key).and_then(|n| u32::try_from(n).ok());
        let limit = |key: &str| match field_raw(header, key)? {
            "1" => Some(true),
            "0" => Some(false),
            _ => None,
        };
        Some(Self {
            look: MachineLook {
                at_unix_ms: n("m_at_ms"),
                cores: n32("cores").filter(|cores| *cores > 0),
                load_x100: n32("load1_x100")
                    .zip(n32("load5_x100"))
                    .zip(n32("load15_x100"))
                    .map(|((one, five), fifteen)| [one, five, fifteen]),
                cpu_busy_x100: n("cpu_busy_x100"),
                rate_win_ms: n("rate_win_ms"),
                mem_total_mb: n("mem_total_mb").filter(|total| *total > 0),
                mem_used_mb: n("mem_used_mb"),
                mem_avail_mb: n("mem_avail_mb"),
                mem_pressure: field_raw(header, "mem_pressure").and_then(memory_level),
                mem_psi60_x100: n32("mem_psi60_x100"),
                swap_used_mb: n("swap_used_mb"),
                swap_total_mb: n("swap_total_mb"),
                disk_total_mb: home_total_mb,
                disk_free_mb: home_free_mb,
                gpu_cores: n32("gpu_cores").filter(|cores| *cores > 0),
                gpu_busy_pct: n("gpu_busy_pct")
                    .and_then(|pct| u8::try_from(pct).ok())
                    .filter(|pct| *pct <= 100),
                net_link_kbps: n("net_link_kbps").filter(|rate| *rate > 0),
                net_rx_kbps: n("net_rx_kbps"),
                net_tx_kbps: n("net_tx_kbps"),
                net_other_kbps: n("net_other_kbps"),
                limits: Limits {
                    cpu: limit("cpu_lim"),
                    mem: limit("mem_lim"),
                    disk: limit("disk_lim"),
                    gpu: limit("gpu_lim"),
                },
                load: field_raw(header, "load").and_then(load_of),
            },
            up_since_ms: n("up_since_ms").filter(|ms| *ms > 0),
            os: field_raw(header, "os").and_then(look_words::os_of),
            target: field_raw(header, "target").and_then(look_words::target_of),
        })
    }
}

/// The header's `load=` value: `<level_x100>:<over_x100>`, the over `-` where it is unread;
/// `-` where the level is unread.
fn load_token(load: Option<LoadScore>) -> String {
    load.map_or_else(
        || "-".to_owned(),
        |load| {
            format!(
                "{}:{}",
                load.level_x100,
                opt_u32_token(load.over_x100.map(u32::from))
            )
        },
    )
}

/// The score a `load=` value names: a whole number `0..=100`, one `:`, then another or `-` (its
/// over unread); `-`, one number alone, a third part or anything out of range is unread.
fn load_of(raw: &str) -> Option<LoadScore> {
    let (level, over) = raw.split_once(':')?;
    let hundredths = |raw: &str| crate::parse_uint::<u8>(raw).filter(|n| *n <= 100);
    Some(LoadScore {
        level_x100: hundredths(level)?,
        over_x100: match over {
            "-" => None,
            over => Some(hundredths(over)?),
        },
    })
}

/// `since_s`, the awake clock in Unix seconds, as the header's Unix ms: `None` where the OS says
/// none (0), never an epoch that reads as up since 1970.
fn up_since_ms(since_s: u64) -> Option<u64> {
    since_s.checked_mul(1000).filter(|ms| *ms > 0)
}

/// The memory pressure level `raw` names ([`MemoryLevel::as_str`]).
fn memory_level(raw: &str) -> Option<MemoryLevel> {
    [
        MemoryLevel::Normal,
        MemoryLevel::Warn,
        MemoryLevel::Critical,
    ]
    .into_iter()
    .find(|level| level.as_str() == raw)
}

/// What a Mac's ledgers keep of rotations, across its pools: whether rotation has ever happened
/// here is the one thing a roster and a condition cannot say.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MoveFacts {
    /// Rotations onto another row a session then proved, in the last seven days: it worked.
    pub proven_7d: usize,
    /// Rotations onto another row, in the last seven days, proved or not: each was an attempt.
    pub tried_7d: usize,
    /// The newest proved rotation its ledgers keep, Unix seconds.
    pub last: Option<i64>,
}

impl MoveFacts {
    fn of(moves: &PoolMoves, now: i64) -> Self {
        const WEEK_S: i64 = 7 * 24 * 60 * 60;
        let in_week = |times: &[i64]| {
            times
                .iter()
                .filter(|t| now.saturating_sub(**t) < WEEK_S)
                .count()
        };
        Self {
            proven_7d: in_week(&moves.proven),
            tried_7d: in_week(&moves.tried),
            last: moves.proven.iter().copied().max(),
        }
    }
}

/// The build output on the home's volume as the disk watch last sized it: the bytes its build
/// caches and build folders held, aterm's rebuildable figure (`aterm ctl sys disk`'s `budget`
/// record), and when that sizing ended. A survey reads it from the record every process of the
/// account shares and walks nothing: the watch sizes at most hourly, on a thread of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HomeBuild {
    /// The bytes counted, in whole MiB (`home_build_mb=`). A measured 0 is 0.
    pub mb: u64,
    /// When the sizing ended, Unix ms on the sizing Mac's clock (`home_build_at_ms=`).
    pub at_ms: u64,
    /// The sizing was cut short (a walk met its bound): `mb` is a floor (`home_build_partial=1`).
    pub partial: bool,
}

impl HomeBuild {
    /// The last sizing the record in the harness state directory `state` keeps
    /// ([`crate::harness::disk::budget::Record::load`], one bounded read and no lock), where it
    /// sized the volume spelled `volume` (`aterm_sysprobe::volumes::VolumeKey::spelled`): another
    /// volume's figure says nothing of this one. `None` where there is no record, it is of
    /// another volume, or its time does not read as a time after the epoch.
    fn recorded(state: &Path, volume: &str) -> Option<Self> {
        let last = crate::harness::disk::budget::Record::load(state)
            .last
            .filter(|last| last.volume == volume)?;
        Some(Self {
            mb: last.sized.bytes >> 20,
            at_ms: u64::try_from(last.sized_unix)
                .ok()
                .filter(|at| *at > 0)?
                .checked_mul(1000)?,
            partial: last.sized.partial,
        })
    }

    /// What an `instance` header says of its build output: the three keys read together, each
    /// leniently, or not at all. A key absent, `-`, not a whole number in its range (a time of
    /// 0 among them) or a partial word other than `0` or `1` reads `None` alone, and the rest of
    /// the reply still reads: a figure nothing here acts on must not cost a machine its reply.
    fn of_header(header: &[(String, String)]) -> Option<Self> {
        Some(Self {
            mb: lenient_u64(header, "home_build_mb")?,
            at_ms: lenient_u64(header, "home_build_at_ms").filter(|at| *at > 0)?,
            partial: match field_raw(header, "home_build_partial")? {
                "1" => true,
                "0" => false,
                _ => return None,
            },
        })
    }
}

/// The fullest of the durable stores aterm keeps on a machine that refuse writes once they
/// meet a bound (the vault's records log, the lab journal's outbox): the one nearest to
/// refusing, as `aterm ctl sys disk`'s `store` lines name and judge them. A store that discards
/// its oldest at its bound is never it, however full.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreWorst {
    /// The store's fixed word (`store_worst=`): lowercase letters and hyphens, never an
    /// entry's name, an account or an address.
    pub name: String,
    /// The share of its bound it holds, in whole percent, rounded down (`store_worst_pct=`);
    /// past 100 where it stands past its bound. A measured 0 is 0.
    pub pct: u64,
}

impl StoreWorst {
    /// The most characters a store's word has.
    const NAME_MAX: usize = 32;

    /// The store `name` at `pct` of its bound; `None` where `name` is not a fixed word's shape
    /// (1 to 32 lowercase letters and hyphens), so nothing else is ever said or shown as one.
    #[must_use]
    pub fn new(name: &str, pct: u64) -> Option<Self> {
        let word = !name.is_empty()
            && name.len() <= Self::NAME_MAX
            && name.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')
            && name.bytes().any(|b| b.is_ascii_lowercase());
        word.then(|| Self {
            name: name.to_owned(),
            pct,
        })
    }

    /// What an `instance` header says of its fullest store: the two keys read together,
    /// leniently, or not at all, as [`HomeBuild::of_header`] reads its three.
    fn of_header(header: &[(String, String)]) -> Option<Self> {
        Self::new(
            field_raw(header, "store_worst")?,
            lenient_u64(header, "store_worst_pct")?,
        )
    }

    /// The header's two tokens, each `-` where `worst` is unknown.
    fn tokens(worst: Option<&Self>) -> String {
        format!(
            " store_worst={} store_worst_pct={}",
            worst.map_or("-", |worst| worst.name.as_str()),
            opt_u64_token(worst.map(|worst| worst.pct)),
        )
    }
}

fn word_token(text: Option<&str>) -> String {
    text.filter(|t| !t.is_empty())
        .map_or_else(|| "-".to_owned(), aterm_codec::pct::encode)
}

fn opt_u64_token(value: Option<u64>) -> String {
    value.map_or_else(|| "-".to_owned(), |n| n.to_string())
}

fn opt_u32_token(value: Option<u32>) -> String {
    value.map_or_else(|| "-".to_owned(), |n| n.to_string())
}

fn opt_i64_token(value: Option<i64>) -> String {
    value.map_or_else(|| "-".to_owned(), |n| n.to_string())
}

/// An account row's `model_walls=` token: each model's own weekly wall as `<bucket>:<unix s>`,
/// comma-joined in bucket-id (the map's key) order, or `-` where none stands
/// ([`SurveyAccountFact::model_walls`]; read by [`parse_model_walls`]).
fn model_walls_token(walls: &BTreeMap<String, i64>) -> String {
    if walls.is_empty() {
        return "-".to_owned();
    }
    let joined = walls
        .iter()
        .map(|(bucket, until)| format!("{bucket}:{until}"))
        .collect::<Vec<_>>()
        .join(",");
    aterm_codec::pct::encode(&joined)
}

impl SwarmSurveyFacts {
    /// Collect live survey facts from the current process environment.
    #[must_use]
    pub fn collect(sessions: usize, window: bool, node: Option<String>) -> Self {
        let now = aterm_time::unix_secs()
            .and_then(|s| i64::try_from(s).ok())
            .unwrap_or(0);
        match Env::from_process() {
            Ok(env) => Self::collect_in(&env, sessions, window, node, cached_system_ssh_host_key()),
            Err(_) => Self {
                operation_lines: Vec::new(),
                node,
                label: None,
                roster_unread: false,
                version: aterm_stamp::app_version().to_owned(),
                window,
                window_last: window.then(|| u64::try_from(now.max(0)).unwrap_or(0)),
                sessions,
                ssh_host_key: cached_system_ssh_host_key(),
                accounts: Vec::new(),
                accounts_unread: Cli::ALL.iter().map(|cli| cli.id().to_owned()).collect(),
                session_accounts: BTreeMap::new(),
                moves: None,
                server_heard: Vec::new(),
                home: None,
                home_dir: None,
                home_free_mb: None,
                home_total_mb: None,
                home_build: None,
                store_worst: None,
                machine: None,
                repos: None,
                update: None,
            },
        }
    }

    /// Collect survey facts rooted at an explicit `aterm_state`, `home`, and `config` path
    /// (used by `aterm node serve` and isolated tests without reading process globals).
    #[must_use]
    pub fn collect_at(
        aterm_state: &Path,
        home: Option<&Path>,
        config: Option<&Path>,
        sessions: usize,
        window: bool,
        node: Option<String>,
        now_secs: i64,
    ) -> Self {
        let env = env_at(aterm_state, home, config, now_secs);
        Self::collect_in(&env, sessions, window, node, cached_system_ssh_host_key())
    }

    /// Collect survey facts under an explicit [`Env`] and pre-read `ssh_host_key` (for tests and
    /// deterministic callers).
    #[must_use]
    pub fn collect_in(
        env: &Env,
        sessions: usize,
        window: bool,
        node: Option<String>,
        ssh_host_key: Option<String>,
    ) -> Self {
        let now_u64 = u64::try_from(env.now.max(0)).unwrap_or(0);
        let window_last = read_window_last(&env.state, window, now_u64);
        let resolved_label = resolve_machine_label_checked(env);
        let roster_unread = resolved_label.is_err();
        let label = resolved_label.ok().flatten();
        let (accounts, accounts_unread, session_accounts, moved) = collect_account_facts(env);
        let server_heard = collect_server_heard(env);
        let home = env.home.as_deref().filter(|home| home.is_absolute());
        // The one volume read collected facts make, by the reader that places a path as the
        // disk watch does, so its key is the key the watch's record names.
        let volume = home.and_then(|home| {
            crate::harness::disk::volume_of(home, &aterm_sysprobe::volumes::table()).ok()
        });
        Self {
            operation_lines: crate::release_progress::survey(&env.state, now_u64)
                .into_iter()
                .chain(crate::release_progress::pump_rows(&env.state, now_u64))
                // agh keeps its rate book under aterm's state root, not in the harness folder
                // below it that the release lines read.
                .chain(aterm_ctl::github_usage::survey(
                    env.aterm_state.as_deref(),
                    u64::try_from(env.now).unwrap_or(0),
                ))
                .collect(),
            node,
            label,
            roster_unread,
            version: aterm_stamp::app_version().to_owned(),
            window,
            window_last,
            sessions,
            ssh_host_key,
            accounts,
            accounts_unread,
            session_accounts,
            moves: moved.map(|moves| MoveFacts::of(&moves, env.now)),
            server_heard,
            home: home.and_then(Path::to_str).map(str::to_owned),
            home_dir: home.map(Path::to_path_buf),
            home_free_mb: volume.as_ref().map(|volume| volume.free >> 20),
            home_total_mb: volume
                .as_ref()
                .map(|volume| volume.total >> 20)
                .filter(|total| *total > 0),
            home_build: volume
                .as_ref()
                .and_then(|volume| HomeBuild::recorded(&env.state, &volume.key.spelled())),
            store_worst: None,
            machine: None,
            repos: home.and_then(repo_home::latest),
            update: None,
        }
    }

    /// Space-prefixed `key=value` tokens appended onto `survey`'s `instance …` header line: with
    /// `accounts=` counting the `account` lines that follow, and `accounts_unread=` naming the
    /// pools none of whose rows they list because their records could not be read (`-` for none).
    /// The home volume's free space, its size and its build output with that sizing's time and
    /// whether it was cut short (`home_free_mb=`, `home_total_mb=`, `home_build_mb=`,
    /// `home_build_at_ms=`, `home_build_partial=`, each `-` where unknown) are resource facts,
    /// sizes and a time with no path, and so are the machine's look after them
    /// ([`Self::machine_tokens`]) and where the updater stands (the `update*=` words after
    /// `roster_unread=`, where [`Self::update`] says it). The header ends with the fullest of
    /// aterm's own stores that refuse writes at a bound (`store_worst=`, `store_worst_pct=`,
    /// each `-` where unknown): a fixed word and a share, a resource fact too. A reply at `content` also adds the home
    /// location and its checkouts' count and read (`home=`, `repos=`, `repos_at=`, `repos_cut=`):
    /// work locations remain content, as a session's folder is.
    #[must_use]
    pub fn header_tokens(&self, content: bool) -> String {
        let home = if content {
            let repos = self.repos.as_ref();
            format!(
                " home={} repos={} repos_at={} repos_cut={}",
                word_token(self.home.as_deref()),
                repos.map_or_else(|| "-".to_owned(), |read| read.repos.len().to_string()),
                opt_u64_token(repos.map(|read| read.read_ms)),
                opt_u64_token(repos.and_then(|read| u64::try_from(read.cut).ok())),
            )
        } else {
            String::new()
        };
        let build = self.home_build;
        format!(
            " label={} window={} window_last={} ssh_host_key={} home_free_mb={} home_total_mb={} \
             home_build_mb={} home_build_at_ms={} home_build_partial={}{}{home} accounts={} \
             accounts_unread={} moves_7d={} move_tries_7d={} last_move={} roster_unread={}{}{}",
            word_token(self.label.as_deref()),
            u8::from(self.window),
            opt_u64_token(self.window_last),
            word_token(self.ssh_host_key.as_deref()),
            opt_u64_token(self.home_free_mb),
            opt_u64_token(self.home_total_mb),
            opt_u64_token(build.map(|build| build.mb)),
            opt_u64_token(build.map(|build| build.at_ms)),
            build.map_or("-", |build| if build.partial { "1" } else { "0" }),
            self.machine_tokens(),
            self.accounts.len(),
            word_token(Some(&self.accounts_unread.join(","))),
            opt_u64_token(self.moves.and_then(|m| u64::try_from(m.proven_7d).ok())),
            opt_u64_token(self.moves.and_then(|m| u64::try_from(m.tried_7d).ok())),
            opt_i64_token(self.moves.and_then(|m| m.last)),
            u8::from(self.roster_unread),
            self.update
                .as_ref()
                .map_or_else(String::new, UpdateFact::tokens),
            StoreWorst::tokens(self.store_worst.as_ref()),
        )
    }

    /// Take the home's checkouts as its latest read found them ([`repo_home::latest`]), so facts a
    /// listener keeps a while still say a read that ended since they were collected.
    pub fn take_latest_repos(&mut self) {
        if let Some(home) = self.home.as_deref() {
            self.repos = repo_home::latest(Path::new(home));
        }
    }

    /// The machine's own header tokens ([`MachineFacts::header_tokens`]), or nothing where
    /// these facts carry no look: a header then says none of the keys, as an older aterm's.
    #[must_use]
    pub fn machine_tokens(&self) -> String {
        self.machine
            .as_ref()
            .map_or_else(String::new, MachineFacts::header_tokens)
    }

    /// Take this machine's look as it is now ([`MachineFacts::now`], kept
    /// [`aterm_capacity::machine_look::LOOK_KEEP`]), its volume the one holding
    /// [`Self::home_dir`], with the free space and the size it read: what an answer does, so
    /// facts a listener keeps a minute say a look of the reply they go out in, never one a minute
    /// behind its `at_ms`. Only an answer calls it: a look is taken only inside one. The build
    /// output stays as collected: the record it was read from moves at most hourly.
    pub fn take_latest_look(&mut self) {
        let machine = MachineFacts::now(self.home_dir.as_deref());
        self.home_free_mb = machine.look.disk_free_mb;
        self.home_total_mb = machine.look.disk_total_mb.filter(|total| *total > 0);
        self.machine = Some(machine);
    }

    /// Additional `account …`, `repo …` and `server_heard …` lines emitted in a `survey` reply,
    /// each account named by its label (`id=`) and its address (`email=`, shown whole, as the
    /// account registry shows it to every member); the `repo` lines only at `content`
    /// ([`Self::header_tokens`]).
    #[must_use]
    pub fn extra_lines(&self, content: bool) -> Vec<String> {
        let repos = match &self.repos {
            Some(read) if content => &read.repos[..],
            _ => &[][..],
        };
        let mut lines =
            Vec::with_capacity(self.accounts.len() + repos.len() + self.server_heard.len());
        lines.extend(self.operation_lines.iter().cloned());
        for acc in &self.accounts {
            lines.push(format!(
                "account pool={} id={} email={} condition={} enrolled={} pct5={} pct7={} \
                 week_reset={} walled_until={} model_walls={} last_used={} read_at={} launch={} \
                 token={}",
                aterm_codec::pct::encode(&acc.pool),
                aterm_codec::pct::encode(&acc.id),
                acc.email
                    .as_deref()
                    .map_or_else(|| "-".to_owned(), aterm_codec::pct::encode),
                aterm_codec::pct::encode(&acc.condition),
                u8::from(acc.enrolled),
                opt_u32_token(acc.usage_5h_pct),
                opt_u32_token(acc.usage_7d_pct),
                opt_i64_token(acc.week_reset),
                opt_i64_token(acc.walled_until),
                model_walls_token(&acc.model_walls),
                opt_i64_token(acc.last_used),
                opt_i64_token(acc.read_at),
                acc.launch
                    .map_or("-", |launch| if launch { "1" } else { "0" }),
                acc.token
                    .as_deref()
                    .map_or_else(|| "-".to_owned(), aterm_codec::pct::encode),
            ));
        }
        for repo in repos {
            lines.push(format!(
                "repo name={} path={} head={} branch={} main={} behind={} fetched_at={} dirty={}",
                aterm_codec::pct::encode(&repo.name),
                word_token(repo.path.to_str()),
                word_token(repo.head.as_deref()),
                word_token(repo.branch.as_deref()),
                word_token(repo.main_ref.as_deref()),
                opt_u32_token(repo.behind),
                opt_u64_token(repo.fetched_ms),
                repo.dirty
                    .map_or("-", |dirty| if dirty { "1" } else { "0" }),
            ));
        }
        for heard in &self.server_heard {
            lines.push(format!(
                "server_heard vault={} subject={} at={}",
                aterm_codec::pct::encode(&heard.vault),
                aterm_codec::pct::encode(&heard.subject),
                heard.at,
            ));
        }
        lines
    }

    /// Render a standalone `OK <n>\ninstance …\n` survey-facts wire payload (used by node-job
    /// heartbeats when no window socket is needed and by round-trip tests), at `facts`: it
    /// carries nothing of the home ([`Self::header_tokens`]).
    #[must_use]
    pub fn to_survey_reply(
        &self,
        pid: u32,
        host: Option<&str>,
        at_ms: u64,
        instances: usize,
        (access, drive): (aterm_types::fleet_access::FleetAccess, bool),
    ) -> String {
        self.to_survey_reply_at(false, pid, host, at_ms, instances, (access, drive))
    }

    /// [`Self::to_survey_reply`] at `content` where it is true (`level=content`), with the home's
    /// facts and checkouts. `access` is this machine's fleet access (`access=<word>`, on every
    /// header) and `drive` whether a grant admits the reader (`drive=1`): what the reader may do
    /// here is both.
    #[must_use]
    pub fn to_survey_reply_at(
        &self,
        content: bool,
        pid: u32,
        host: Option<&str>,
        at_ms: u64,
        instances: usize,
        (access, drive): (aterm_types::fleet_access::FleetAccess, bool),
    ) -> String {
        let extra = self.extra_lines(content);
        let count = 1 + extra.len();
        let host = word_token(host);
        let version = aterm_codec::pct::encode(&self.version);
        let node = word_token(self.node.as_deref());
        let drive_tok = if drive { " drive=1" } else { "" };
        let mut out = format!(
            "OK {count}\ninstance pid={pid} host={host} version={version} sessions={} \
             level={} cut=0 at_ms={at_ms} node={node} instances={instances}{} \
             access={}{drive_tok}\n",
            self.sessions,
            if content { "content" } else { "facts" },
            self.header_tokens(content),
            access.word(),
        );
        for line in extra {
            out.push_str(&line);
            out.push('\n');
        }
        out
    }

    /// Parse and validate [`SwarmSurveyFacts`] from a `survey` wire reply.
    ///
    /// # Errors
    /// Returns why the reply is not a valid C16 survey-facts payload (missing `instance` header,
    /// missing required C16 header field, or malformed `account` / `server_heard` record).
    pub fn from_survey_reply(reply: &str) -> Result<Self, String> {
        let mut instance: Option<Vec<(String, String)>> = None;
        let mut accounts = Vec::new();
        let mut repos = Vec::new();
        let mut server_heard = Vec::new();
        let mut operation_lines = Vec::new();

        for raw in reply.lines() {
            let line = raw.trim_end_matches('\r');
            if line.is_empty() || line.starts_with("OK ") || line.starts_with("session ") {
                continue;
            }
            if line.starts_with("instance ") {
                instance = Some(parse_fields(line));
            } else if line.starts_with("account ") {
                accounts.push(parse_account_line(&parse_fields(line))?);
            } else if line.starts_with("repo ") {
                repos.push(parse_repo_line(&parse_fields(line))?);
            } else if line.starts_with("server_heard ") {
                server_heard.push(parse_server_heard_line(&parse_fields(line))?);
            } else if line.starts_with("release_line ")
                || line.starts_with(aterm_ctl::release_pump::ROW)
                || line.starts_with("github_rate ")
            {
                operation_lines.push(line.to_owned());
            }
        }

        let header =
            instance.ok_or_else(|| "missing instance header in survey reply".to_owned())?;
        if let Some(raw_accounts) = field_raw(&header, "accounts") {
            let expected = crate::parse_uint::<usize>(raw_accounts)
                .ok_or_else(|| "invalid accounts field in instance header".to_owned())?;
            if expected != accounts.len() {
                return Err(format!(
                    "survey reply account count mismatch: header says {expected}, body has {}",
                    accounts.len()
                ));
            }
        }
        let repos = parse_repos(&header, repos)?;
        let version = required_nonempty(&header, "version")?;
        let sessions = crate::parse_uint::<usize>(required_raw(&header, "sessions")?)
            .ok_or_else(|| "invalid sessions field in instance header".to_owned())?;
        let window = parse_bool01(required_raw(&header, "window")?, "window")?;
        let window_last = parse_opt_u64(required_raw(&header, "window_last")?, "window_last")?;
        let label = optional_word(required_raw(&header, "label")?);
        let ssh_host_key = optional_word(required_raw(&header, "ssh_host_key")?);
        let node = field_raw(&header, "node").and_then(optional_word);
        // Optional: an aterm whose C16 facts predate it names no unread pool.
        let accounts_unread = field_raw(&header, "accounts_unread")
            .and_then(optional_word)
            .map_or_else(Vec::new, |pools| {
                pools
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect()
            });

        // Optional: an aterm whose C16 facts predate them says nothing of its moves.
        let moves = field_raw(&header, "moves_7d")
            .and_then(optional_word)
            .map(|week| {
                Ok::<_, String>(MoveFacts {
                    proven_7d: crate::parse_uint::<usize>(&week)
                        .ok_or_else(|| "invalid moves_7d field in instance header".to_owned())?,
                    tried_7d: field_raw(&header, "move_tries_7d")
                        .and_then(crate::parse_uint::<usize>)
                        .ok_or_else(|| {
                            "invalid move_tries_7d field in instance header".to_owned()
                        })?,
                    last: field_raw(&header, "last_move")
                        .map_or(Ok(None), |raw| parse_opt_i64(raw, "last_move"))?,
                })
            })
            .transpose()?;

        let home_free_mb = field_raw(&header, "home_free_mb")
            .map_or(Ok(None), |raw| parse_opt_u64(raw, "home_free_mb"))?;
        // Optional and lenient: an aterm older than the key sends none, and one whose look said
        // it among the look's keys is read wherever it stands on the line.
        let home_total_mb = lenient_u64(&header, "home_total_mb").filter(|total| *total > 0);
        Ok(Self {
            operation_lines,
            node,
            label,
            version,
            roster_unread: field_raw(&header, "roster_unread")
                .map_or(Ok(false), |raw| parse_bool01(raw, "roster_unread"))?,
            window,
            window_last,
            sessions,
            ssh_host_key,
            accounts,
            accounts_unread,
            session_accounts: BTreeMap::new(),
            moves,
            server_heard,
            home: field_raw(&header, "home").and_then(optional_word),
            home_dir: None,
            home_free_mb,
            home_total_mb,
            home_build: HomeBuild::of_header(&header),
            store_worst: StoreWorst::of_header(&header),
            machine: MachineFacts::of_header(&header, home_free_mb, home_total_mb),
            repos,
            // Lenient: a word outside its grammar reads as unknown and never fails the facts.
            update: UpdateFact::of_fields(&header),
        })
    }
}

/// The home's checkouts an `instance` header (`header`) and its `repo` lines (`rows`) say: `None`
/// where it says nothing of them (an aterm whose facts predate them) or none was read yet
/// (`repos=-`, with no line).
fn parse_repos(
    header: &[(String, String)],
    rows: Vec<HomeRepo>,
) -> Result<Option<HomeRepos>, String> {
    let Some(count) = field_raw(header, "repos").and_then(optional_word) else {
        return if rows.is_empty() {
            Ok(None)
        } else {
            Err("survey reply has repo lines and no repos count".to_owned())
        };
    };
    let expected = crate::parse_uint::<usize>(&count)
        .ok_or_else(|| "invalid repos field in instance header".to_owned())?;
    if expected != rows.len() {
        return Err(format!(
            "survey reply repo count mismatch: header says {expected}, body has {}",
            rows.len()
        ));
    }
    let read_ms = parse_opt_u64(required_raw(header, "repos_at")?, "repos_at")?
        .ok_or_else(|| "empty `repos_at` field".to_owned())?;
    let cut = parse_opt_u64(required_raw(header, "repos_cut")?, "repos_cut")?
        .and_then(|cut| usize::try_from(cut).ok())
        .ok_or_else(|| "invalid `repos_cut` field".to_owned())?;
    Ok(Some(HomeRepos {
        read_ms,
        repos: rows,
        cut,
    }))
}

fn parse_repo_line(fields: &[(String, String)]) -> Result<HomeRepo, String> {
    Ok(HomeRepo {
        name: required_nonempty(fields, "name")?,
        path: required_nonempty(fields, "path")?.into(),
        head: optional_word(required_raw(fields, "head")?),
        branch: optional_word(required_raw(fields, "branch")?),
        main_ref: optional_word(required_raw(fields, "main")?),
        behind: parse_opt_u32(required_raw(fields, "behind")?, "behind")?,
        fetched_ms: parse_opt_u64(required_raw(fields, "fetched_at")?, "fetched_at")?,
        dirty: match required_raw(fields, "dirty")? {
            "-" => None,
            raw => Some(parse_bool01(raw, "dirty")?),
        },
    })
}

fn parse_fields(line: &str) -> Vec<(String, String)> {
    line.split(' ')
        .skip(1)
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_owned(), aterm_codec::pct::decode(v)))
        .collect()
}

fn field_raw<'a>(fields: &'a [(String, String)], key: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

fn required_raw<'a>(fields: &'a [(String, String)], key: &str) -> Result<&'a str, String> {
    field_raw(fields, key).ok_or_else(|| format!("missing `{key}` field"))
}

fn required_nonempty(fields: &[(String, String)], key: &str) -> Result<String, String> {
    let raw = required_raw(fields, key)?;
    if raw.is_empty() || raw == "-" {
        return Err(format!("empty `{key}` field"));
    }
    Ok(raw.to_owned())
}

/// A header key's whole number, read leniently: `None` where the key is absent, `-` or anything
/// but digits in range, and the reply it came in still parses. A figure nothing here acts on
/// must not cost a machine its whole reply.
fn lenient_u64(fields: &[(String, String)], key: &str) -> Option<u64> {
    field_raw(fields, key).and_then(crate::parse_uint::<u64>)
}

fn optional_word(raw: &str) -> Option<String> {
    (!raw.is_empty() && raw != "-").then(|| raw.to_owned())
}

fn parse_bool01(raw: &str, key: &str) -> Result<bool, String> {
    match raw {
        "1" => Ok(true),
        "0" => Ok(false),
        _ => Err(format!("invalid `{key}` field: expected 0 or 1")),
    }
}

fn parse_opt_u64(raw: &str, key: &str) -> Result<Option<u64>, String> {
    if raw == "-" {
        return Ok(None);
    }
    crate::parse_uint::<u64>(raw)
        .map(Some)
        .ok_or_else(|| format!("invalid `{key}` field"))
}

fn parse_opt_u32(raw: &str, key: &str) -> Result<Option<u32>, String> {
    if raw == "-" {
        return Ok(None);
    }
    crate::parse_uint::<u32>(raw)
        .map(Some)
        .ok_or_else(|| format!("invalid `{key}` field"))
}

fn parse_opt_i64(raw: &str, key: &str) -> Result<Option<i64>, String> {
    if raw == "-" {
        return Ok(None);
    }
    crate::parse_int::<i64>(raw)
        .map(Some)
        .ok_or_else(|| format!("invalid `{key}` field"))
}

/// An account row's `model_walls=` ([`model_walls_token`]), read leniently: a member older than
/// the field sends none (no key, or `-`), which reads as no model wall, and an entry that is not
/// `<bucket>:<unix s>` is dropped while the rest, and the reply, still read. A model's wall walls
/// no account, so one bad entry must not cost a Mac its whole reply.
fn parse_model_walls(fields: &[(String, String)]) -> BTreeMap<String, i64> {
    field_raw(fields, "model_walls")
        .filter(|raw| *raw != "-")
        .into_iter()
        .flat_map(|raw| raw.split(','))
        .filter_map(|entry| {
            let (bucket, until) = entry.rsplit_once(':')?;
            let until = crate::parse_int::<i64>(until)?;
            (!bucket.is_empty()).then(|| (bucket.to_owned(), until))
        })
        .collect()
}

fn parse_account_line(fields: &[(String, String)]) -> Result<SurveyAccountFact, String> {
    Ok(SurveyAccountFact {
        pool: required_nonempty(fields, "pool")?,
        id: required_nonempty(fields, "id")?,
        // A member older than the field sends none.
        email: field_raw(fields, "email")
            .filter(|raw| !raw.is_empty() && *raw != "-")
            .map(str::to_owned),
        condition: required_nonempty(fields, "condition")?,
        enrolled: parse_bool01(required_raw(fields, "enrolled")?, "enrolled")?,
        usage_5h_pct: parse_opt_u32(required_raw(fields, "pct5")?, "pct5")?,
        usage_7d_pct: parse_opt_u32(required_raw(fields, "pct7")?, "pct7")?,
        week_reset: parse_opt_i64(required_raw(fields, "week_reset")?, "week_reset")?,
        walled_until: parse_opt_i64(required_raw(fields, "walled_until")?, "walled_until")?,
        model_walls: parse_model_walls(fields),
        last_used: parse_opt_i64(required_raw(fields, "last_used")?, "last_used")?,
        // A member older than the field sends none.
        read_at: required_raw(fields, "read_at")
            .ok()
            .map_or(Ok(None), |raw| parse_opt_i64(raw, "read_at"))?,
        // A member older than the field sends none.
        launch: field_raw(fields, "launch")
            .filter(|raw| *raw != "-")
            .map(|raw| parse_bool01(raw, "launch"))
            .transpose()?,
        // A member older than the field sends none.
        token: field_raw(fields, "token")
            .filter(|raw| !raw.is_empty() && *raw != "-")
            .map(str::to_owned),
    })
}

fn parse_server_heard_line(fields: &[(String, String)]) -> Result<ServerHeardFact, String> {
    let vault = required_nonempty(fields, "vault")?;
    let subject = required_nonempty(fields, "subject")?;
    let at = crate::parse_uint::<u64>(required_raw(fields, "at")?)
        .ok_or_else(|| "invalid `at` field on server_heard".to_owned())?;
    Ok(ServerHeardFact { vault, subject, at })
}

fn read_window_last(state_dir: &Path, window: bool, now: u64) -> Option<u64> {
    if window {
        return Some(now);
    }
    let path = state_dir.join(WINDOW_LAST_FILE);
    crate::harness::accounts::read_bounded(&path, 256)
        .ok()
        .flatten()
        .and_then(|s| crate::parse_uint::<u64>(s.trim()))
        .filter(|at| *at <= now)
}

/// Record advisory window liveness from the window's background heartbeat, never a survey
/// request. Losing this hint on power loss is acceptable; it grants no authority and a live
/// window reports its current clock directly.
/// # Errors
/// The state directory could not be created, or the atomic publication failed.
pub fn record_window_last(state_dir: &Path, now: u64) -> std::io::Result<()> {
    std::fs::create_dir_all(state_dir)?;
    aterm_uds::atomic_file::write_flushed(
        &state_dir.join(WINDOW_LAST_FILE),
        format!("{now}\n").as_bytes(),
        0o600,
        None,
    )
}

/// The [`Env`] a survey of `aterm_state`, `home` and `config` at `now_secs` reads.
fn env_at(aterm_state: &Path, home: Option<&Path>, config: Option<&Path>, now_secs: i64) -> Env {
    Env {
        state: aterm_state
            .join("harness")
            .join(crate::harness::cli::HARNESS),
        aterm_state: Some(aterm_state.to_path_buf()),
        cwd: aterm_state.to_path_buf(),
        home: home.map(Path::to_path_buf),
        claude_dir: home.map(|h| h.join(".claude")),
        claude_config_unusable: None,
        claude_profile: home.map(|h| h.join(".claude.json")),
        claude_custom_oauth: false,
        claude_session: None,
        claude_pid: None,
        codex_thread: None,
        codex_home: None,
        xdg_config_home: None,
        codex_login: None,
        agy_conversation: None,
        now: now_secs,
        sid: String::new(),
        sock: None,
        config: config.map(Path::to_path_buf),
        claude_tmp: None,
        claude_running: None,
        tz_s: 0,
    }
}

/// How long a survey waits for a vault writer before it reports the roster unread.
const SURVEY_ROSTER_WAIT: std::time::Duration = std::time::Duration::from_millis(250);

/// How long a release lease waits for a vault writer before it says this Mac's roster could
/// not be read. On 2026-11-13 a survey's 250 ms ran out under load 30–50 on 36 cores and ended
/// a release; a vault apply that outlasts this is a fault worth stopping on.
pub const RELEASE_ROSTER_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// Why this Mac has no canonical label it may publish ([`machine_label_within`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoLabel {
    /// The swarm roster denies this Mac's vault recipient.
    Denied,
    /// Neither `aterm.toml` nor this Mac's vault names a label the roster allows.
    Unnamed,
    /// The roster, the recipient's standing in it, or this Mac's vault identity could not be
    /// read within `waited` (a vault writer held the state lock throughout, or a read failed).
    Unread {
        why: String,
        waited: std::time::Duration,
    },
}

impl std::fmt::Display for NoLabel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("no canonical fleet host label: ")?;
        match self {
            Self::Denied => f.write_str("the swarm roster denies this Mac's vault recipient"),
            Self::Unnamed => f.write_str(
                "neither aterm.toml ([swarm] label, or [ai], [fleet] or [harness] machine) nor \
                 this Mac's vault names one the swarm roster allows",
            ),
            Self::Unread { why, waited } => write!(
                f,
                "this Mac's swarm roster could not be read within {} ms: {why}",
                waited.as_millis()
            ),
        }
    }
}

/// The roster's deny labels, whether it denies this Mac's recipient, and the label it (or, with
/// no roster, the vault's configured host) gives this Mac. A vault writer holding the state lock
/// is waited out until `wait` has passed, for the roster and for the recipient's standing alike:
/// a writer met between the two reads is not a denial.
#[cfg(unix)]
fn local_roster_denial(
    home: Option<&Path>,
    wait: std::time::Duration,
) -> Result<(Vec<String>, bool, Option<String>), String> {
    let Some(home) = home else {
        return Ok((Vec::new(), false, None));
    };
    let deadline = std::time::Instant::now() + wait;
    let paths = aterm_secrets::client::Paths::for_home(home);
    let roster = aterm_secrets::client::swarm_roster_until(&paths, deadline)
        .map_err(|why| why.to_string())?;
    let recipient_denied = loop {
        match aterm_secrets::client::local_recipient_denial_observed(&paths) {
            Ok(denied) => break denied,
            Err(why)
                if why.kind() == aterm_secrets::ErrorKind::VaultBusy
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(
                    deadline
                        .saturating_duration_since(std::time::Instant::now())
                        .min(std::time::Duration::from_millis(20)),
                );
            }
            Err(why) => return Err(why.to_string()),
        }
    };
    let label = if let Some(roster) = &roster {
        aterm_secrets::client::read_machine_identity(&paths)
            .map_err(|why| why.to_string())?
            .and_then(|identity| {
                roster
                    .member_by_recipient(&identity.recipient().to_bech32())
                    .filter(|member| !roster.is_member_denied(member))
                    .map(|member| member.label.clone())
            })
    } else {
        aterm_secrets::client::status(&paths)
            .ok()
            .and_then(|status| status.config.map(|config| config.host))
            .filter(|host| !host.trim().is_empty())
    };
    Ok((
        roster.map_or_else(Vec::new, |r| r.deny_labels),
        recipient_denied,
        label,
    ))
}

#[cfg(not(unix))]
fn local_roster_denial(
    _home: Option<&Path>,
    _wait: std::time::Duration,
) -> Result<(Vec<String>, bool, Option<String>), String> {
    Ok((Vec::new(), false, None))
}

/// This machine's canonical label as its survey facts name it, where it has one it may publish.
#[must_use]
pub fn machine_label() -> Option<String> {
    resolve_machine_label(&Env::from_process().ok()?)
}

/// This machine's canonical label for a record that names its holder (a release lease), or why
/// it has none, waiting up to [`RELEASE_ROSTER_WAIT`] for a vault writer.
/// # Errors
/// [`NoLabel`]'s words, or why this process's environment could not be read.
pub fn release_machine_label() -> Result<String, String> {
    let env = Env::from_process().map_err(|why| format!("no canonical fleet host label: {why}"))?;
    machine_label_within(&env, RELEASE_ROSTER_WAIT).map_err(|no| no.to_string())
}

fn resolve_machine_label(env: &Env) -> Option<String> {
    resolve_machine_label_checked(env).ok().flatten()
}

/// The survey's form: a denied or unnamed Mac publishes no label, an unread roster says so.
fn resolve_machine_label_checked(env: &Env) -> Result<Option<String>, String> {
    match machine_label_within(env, SURVEY_ROSTER_WAIT) {
        Ok(label) => Ok(Some(label)),
        Err(NoLabel::Unread { why, .. }) => Err(why),
        Err(NoLabel::Denied | NoLabel::Unnamed) => Ok(None),
    }
}

/// `env`'s canonical label, waiting up to `wait` for a vault writer; a roster that denies this
/// Mac or names nothing answers at once.
/// # Errors
/// [`NoLabel`]: why there is none.
pub fn machine_label_within(env: &Env, wait: std::time::Duration) -> Result<String, NoLabel> {
    let (deny_labels, recipient_denied, vault_label) =
        local_roster_denial(env.home.as_deref(), wait)
            .map_err(|why| NoLabel::Unread { why, waited: wait })?;
    if recipient_denied {
        return Err(NoLabel::Denied);
    }
    let is_denied_label = |candidate: &str| {
        #[cfg(unix)]
        let canon = aterm_secrets::swarm_roster::leading_m_label(candidate)
            .map(|(_, c)| c)
            .unwrap_or_else(|| candidate.trim().to_ascii_lowercase());
        #[cfg(not(unix))]
        let canon = candidate.trim().to_ascii_lowercase();
        deny_labels.iter().any(|d| d.eq_ignore_ascii_case(&canon))
    };
    if let Some(label) = env
        .config
        .as_deref()
        .and_then(|config| config_label(config, &is_denied_label))
    {
        return Ok(label);
    }
    vault_label
        .filter(|h| !is_denied_label(h))
        .ok_or(NoLabel::Unnamed)
}

/// The first of `[swarm] label`, `[ai] machine`, `[fleet] machine` and `[harness] machine` the
/// `aterm.toml` at `config` sets to a name `denied` does not deny, read by the rule every reader
/// of the file holds ([`aterm_types::dirs::read_aterm_config_text`]): a file group or others can
/// write names none.
fn config_label(config: &Path, denied: &dyn Fn(&str) -> bool) -> Option<String> {
    let text = aterm_types::dirs::read_aterm_config_text(config).ok()??;
    let table = aterm_toml::from_str::<aterm_toml::Value>(&text).ok()?;
    [
        ("swarm", "label"),
        ("ai", "machine"),
        ("fleet", "machine"),
        ("harness", "machine"),
    ]
    .into_iter()
    .find_map(|(section, key)| {
        table
            .get(section)
            .and_then(aterm_toml::Value::as_table)
            .and_then(|t| t.get(key))
            .and_then(aterm_toml::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty() && !denied(s))
            .map(str::to_owned)
    })
}

/// Each pooled CLI's rows, the pools that could not be read, the row each session runs on, as
/// `aterm harness accounts --json` reads them beside `env`'s `aterm.toml`, and when its ledgers
/// say it rotated. Claude Code's rows carry their vault token's state ([`claude_token_states`]).
fn collect_account_facts(env: &Env) -> AccountFacts {
    let mut facts = account_facts_of(Cli::ALL.into_iter().map(|cli| {
        let read = env
            .config
            .as_deref()
            .ok_or_else(|| "no aterm.toml path is known".to_owned())
            .and_then(|config| {
                crate::harness::cli::survey_accounts(
                    cli,
                    (config, &env.state, env.home.as_deref()),
                    env.now,
                )
            });
        (cli, read)
    }));
    let states = claude_token_states(env);
    for row in facts
        .0
        .iter_mut()
        .filter(|row| row.pool == Cli::Claude.id())
    {
        row.token = states.get(&row.id).cloned();
    }
    facts
}

/// Each Claude Code row's vault token state word by label (§5.10): one read of this Mac's vault
/// and its token table ([`crate::harness::accounts_token_view::states_by_label`]); none off Unix,
/// where no home is known, or where no vault is configured
/// ([`crate::harness::accounts_tokens::configured_vault`]), so such a Mac's rows say `token=-`
/// as every other surface says nothing of tokens there.
fn claude_token_states(env: &Env) -> BTreeMap<String, String> {
    #[cfg(unix)]
    {
        use crate::harness::accounts_token_view as view;
        let Some(home) = env.home.as_deref() else {
            return BTreeMap::new();
        };
        let store = accounts::Store::at(&env.state);
        let roster = env
            .config
            .as_deref()
            .and_then(|config| accounts::roster_beside_for(config, Cli::Claude));
        let Some(vault) = crate::harness::accounts_tokens::configured_vault(
            aterm_secrets::client::Paths::for_home(home),
        ) else {
            return BTreeMap::new();
        };
        view::states_by_label(
            &view::Places {
                store: &store,
                roster: roster.as_deref(),
                // The state words are keyed by label; no account is named.
                lab: None,
                logins: &[],
                now: env.now,
                offers: view::OFFERED_HERE,
                enrolled: &accounts::enrolled,
            },
            &vault,
        )
    }
    #[cfg(not(unix))]
    {
        let _ = env;
        BTreeMap::new()
    }
}

/// What [`collect_account_facts`] reads: the rows, the unread pools, each session's row, and its
/// rotations, `None` where a pool or its ledger could not be read.
type AccountFacts = (
    Vec<SurveyAccountFact>,
    Vec<String>,
    BTreeMap<(String, String), String>,
    Option<PoolMoves>,
);

/// The account facts each pool's read makes: its rows, or its id among the unread where it could
/// not be read (a pool left out with no word would read as one holding no row).
fn account_facts_of(
    reads: impl IntoIterator<Item = (Cli, Result<SurveyPool, String>)>,
) -> AccountFacts {
    let mut rows = Vec::new();
    let mut unread = Vec::new();
    let mut sessions = BTreeMap::new();
    let mut moved = Some(PoolMoves::default());
    for (cli, read) in reads {
        let pool = cli.id().to_owned();
        let Ok(read) = read else {
            unread.push(pool);
            moved = None;
            continue;
        };
        moved = moved.zip(read.moves).map(|(mut all, more)| {
            all.tried.extend(more.tried);
            all.proven.extend(more.proven);
            all
        });
        let names: BTreeMap<String, String> = read
            .rows
            .iter()
            .filter_map(|row| Some((row.label.clone(), row.email.clone()?)))
            .collect();
        rows.extend(read.rows.into_iter().map(|row| SurveyAccountFact {
            pool: pool.clone(),
            email: row.email,
            walled_until: match row.condition {
                accounts::Condition::Limited { until } => Some(until),
                _ => None,
            },
            model_walls: row.model_walls,
            id: row.label,
            condition: row.condition.as_str().to_owned(),
            enrolled: row.enrolled,
            usage_5h_pct: row.five_hour_pct,
            usage_7d_pct: row.seven_day_pct,
            week_reset: row.week_reset,
            last_used: row.used_at,
            read_at: row.read_at,
            launch: Some(row.launch),
            token: None,
        }));
        sessions.extend(read.sessions.into_iter().map(|(sid, label)| {
            let name = names.get(&label).cloned().unwrap_or(label);
            ((pool.clone(), sid), name)
        }));
    }
    (rows, unread, sessions, moved)
}

#[cfg(unix)]
fn collect_server_heard(env: &Env) -> Vec<ServerHeardFact> {
    let Some(home) = env.home.as_deref() else {
        return Vec::new();
    };
    let served = aterm_secrets::client::Paths::for_home(home).served();
    aterm_secrets::server::read_last_heard(&served)
        .into_iter()
        .map(|(vault, subject, at)| ServerHeardFact { vault, subject, at })
        .collect()
}

#[cfg(not(unix))]
fn collect_server_heard(_env: &Env) -> Vec<ServerHeardFact> {
    Vec::new()
}

fn cached_system_ssh_host_key() -> Option<String> {
    static HOST_KEY: OnceLock<Option<String>> = OnceLock::new();
    HOST_KEY
        .get_or_init(|| {
            #[cfg(unix)]
            {
                ssh_host_key_fingerprint_in(Path::new("/etc/ssh"))
            }
            #[cfg(not(unix))]
            {
                None
            }
        })
        .clone()
}

/// Compute the OpenSSH `SHA256:<unpadded-base64>` host key fingerprint from `dir`
/// (`ssh_host_ed25519_key.pub`, else `ecdsa`, else `rsa`) in pure Rust without spawning
/// `ssh-keygen`.
#[must_use]
pub fn ssh_host_key_fingerprint_in(dir: &Path) -> Option<String> {
    for file in [
        "ssh_host_ed25519_key.pub",
        "ssh_host_ecdsa_key.pub",
        "ssh_host_rsa_key.pub",
    ] {
        let path = dir.join(file);
        let Ok(Some(text)) = crate::harness::accounts::read_bounded(&path, SSH_PUBKEY_MAX as u64)
        else {
            continue;
        };
        if text.is_empty() {
            continue;
        }
        if let Some(fp) = parse_openssh_pubkey_sha256(&text) {
            return Some(fp);
        }
    }
    None
}

fn parse_openssh_pubkey_sha256(text: &str) -> Option<String> {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))?;
    let mut parts = line.split_ascii_whitespace();
    let key_type = parts.next()?;
    let b64_blob = parts.next()?;
    if !key_type.starts_with("ssh-") && !key_type.starts_with("ecdsa-") {
        return None;
    }
    let decoded = aterm_codec::base64::decode(b64_blob).ok()?;
    if decoded.len() < 16 {
        return None;
    }
    let digest = aterm_digest::Sha256::digest(&decoded);
    let encoded = aterm_codec::base64::encode(&digest).ok()?;
    Some(format!("SHA256:{}", encoded.trim_end_matches('=')))
}

#[cfg(test)]
#[path = "survey_facts_tests.rs"]
mod tests;
