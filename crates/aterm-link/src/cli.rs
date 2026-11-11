// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-link` fabric bridge CLI (`ls`, `serve`, `notify`, `broker`, `mint`).

use std::process::ExitCode;

use aterm_uds::lifeline::OnWake;

use crate::bridge::{Bridge, Config};
use crate::transport;

const USAGE: &str = "\
aterm link — the fabric bridge (also runs as `aterm-link`)

  aterm link serve  --fleet <F> --broker <socket> [--cap-file <path>...] [options]
  aterm link ls     [--fleet <F> --broker <socket> --cap-file <path>...] [--attention]
  aterm link notify --fleet <F> --broker <socket> [--cap-file <path>...] --on <sel>,...
                    --exec <cmd> [options]        (alone, it lists its options)
  aterm link broker <socket> [log] [options]      (`aterm link broker --help`)
  aterm link broker --repair <log>
  aterm link mint   <grant> --secret-file <path>
  aterm link fabric ...                           (`aterm fabric --help`)

`ls` takes --fleet and --broker from the rendezvous file `fabric.toml` that `aterm fabric
on` writes, and --cap-file and --state too when --broker is not given; a flag given wins.
`serve` reads `[fabric] presence`, `receipts` and `log` in aterm.toml for a flag not
given. It is the bridge an aterm instance starts from `[fabric] command`, on the lanes it
hands down; run by hand, it needs --sock and --token-file and serves as an observer.

  --fleet <F>            fleet name (the `/f/<F>/` subtree)
  --broker <socket>      the broker's Unix socket
  --cap-file <path>      minted capability file (`<grant> <tag-hex>` lines); repeatable. A
                         regular file of yours at 0600 or tighter, in any folder: a wider
                         one, a symlink or a FIFO is refused at once, including at supervised
                         startup; the file is left unchanged
  --state <dir>          state dir: node id, incarnation, sequences, watermarks (default
                         `$XDG_STATE_HOME/aterm-link`, else `~/.local/state/aterm-link`)
  --accept-from <p>,...  principals whose task/control arrive undemoted (an `h-` human's
                         always do)
  --sock <path>          `serve` only: attach to an instance's control socket as a
                         read/presence observer
  --token-file <path>    the instance token --sock needs
  --presence <mode>      `serve` only: `meta` (default: role=, detail=, phase=, title=
                         beside attention=; no screen text) or `minimal` (attention= only)
  --receipts             `serve` only, on by default: send `kind=ack` when `inbox seen
                         <id> handled|refused|deferred` marks an ask or task
                         (`--no-receipts` turns it off for one run)
  --log <auto|off|turns> `serve` only, default `auto`: mirror session-log turns, runs,
                         needs and steps onto the session's `ev` face while the broker
                         retains them; `turns` mirrors over any broker, `off` not at all
  --attention            `ls` only: keep only rows whose `attention=` is set (not `-`)

`ls` prints each presence row's latest record, one pct-encoded token per column (<sid>
is `node` on a node's own row):
  <node> <host> <sid> state= inc= role= detail= driving= holder= hold= fabric=
  attention= gen= observer= epoch= phase= context= title= proto= aterm=
(`driving=`, `holder=` and `context=` stay `-`: no bridge of this build writes them)
";

/// Library entry point for the `aterm-link` CLI.
#[must_use = "handle or propagate the command's exit status"]
pub fn dispatch(args: &[String]) -> ExitCode {
    dispatch_with_inherited_lanes(args, || {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "inherited bridge lanes require explicit ownership admission at executable startup",
        ))
    })
}

/// Dispatch with the composing executable's one-shot lane admission.
#[must_use = "handle or propagate the command's exit status"]
pub fn dispatch_with_inherited_lanes(
    args: &[String],
    take_lanes: impl FnOnce() -> std::io::Result<crate::bridge::InheritedLanes>,
) -> ExitCode {
    match args.first().map(String::as_str) {
        Some("serve") => run(&args[1..], true, take_lanes),
        Some("ls") => {
            let given = |flag: &str| args[1..].iter().any(|a| a == flag);
            if !given("--fleet")
                && !given("--broker")
                && matches!(crate::enable::Rendezvous::read(), Ok(None))
            {
                eprintln!(
                    "aterm link ls: no --fleet/--broker and no rendezvous file — \
                     `aterm fabric on` writes one"
                );
                return ExitCode::from(2);
            }
            run(
                &crate::enable::with_rendezvous_defaults(&args[1..]),
                false,
                take_lanes,
            )
        }
        Some("notify") => crate::notify::main(&args[1..]),
        Some("broker") => broker(&args[1..]),
        Some("mint") => mint(&args[1..]),
        Some("fabric") => crate::fabric::main(&args[1..]),
        Some("hook") => retired_hook(),
        Some("-h" | "--help" | "help") => aterm_uds::stdout::answer_exit("aterm link", USAGE, 0),
        Some(other) => {
            eprintln!("aterm link: unknown subcommand `{other}`");
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
        None => {
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// Whether `args` holds `-h` or `--help`.
pub(crate) fn asks_help(args: &[String]) -> bool {
    args.iter().any(|a| matches!(a.as_str(), "-h" | "--help"))
}

/// The one line `aterm link hook …` prints, on stderr ([`retired_hook`]).
pub const RETIRED_HOOK_LINE: &str =
    "aterm-link: `hook` is retired and does nothing — delete this entry from the settings file \
     that runs it";

fn retired_hook() -> ExitCode {
    use std::io::IsTerminal as _;
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        let _ = std::io::copy(&mut stdin.lock(), &mut std::io::sink());
    }
    eprintln!("{RETIRED_HOOK_LINE}");
    ExitCode::SUCCESS
}

fn run(
    args: &[String],
    serve: bool,
    take_lanes: impl FnOnce() -> std::io::Result<crate::bridge::InheritedLanes>,
) -> ExitCode {
    let parsed = match parse(args) {
        Ok(parsed) => parsed,
        Err(e) => {
            eprintln!("aterm link: {e}");
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    if serve {
        // `--attention` is an `ls` filter; `serve` refuses it by name rather
        // than parsing it and doing nothing.
        if parsed.attention {
            eprintln!("aterm link: --attention is an `ls` filter; `serve` has no roster");
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
        let mut cfg = parsed.cfg;
        if !parsed.presence_given {
            cfg.presence = crate::fabric::presence_from_config();
        }
        if !parsed.receipts_given {
            cfg.receipts = crate::fabric::receipts_from_config();
        }
        if !parsed.log_given {
            cfg.log = crate::fabric::log_from_config();
        }
        let bridge = if cfg.sock.is_some() && cfg.token.is_some() {
            Bridge::new(cfg)
        } else {
            take_lanes().and_then(|lanes| Bridge::with_inherited_lanes(cfg, lanes))
        };
        match bridge.and_then(Bridge::run) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("aterm-link serve: {e}");
                ExitCode::FAILURE
            }
        }
    } else {
        match roster(&parsed.cfg, parsed.attention) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("aterm link ls: {e}");
                ExitCode::FAILURE
            }
        }
    }
}

/// Parsed bridge config and CLI-only flags.
pub(crate) struct Parsed {
    pub(crate) cfg: Config,
    /// `ls --attention` (§9.3): keep only the rows carrying an escalation.
    pub(crate) attention: bool,
    /// Whether `--presence` was explicitly passed on the command line.
    pub(crate) presence_given: bool,
    /// Whether `--receipts` or `--no-receipts` was explicitly passed on the command line.
    pub(crate) receipts_given: bool,
    /// Whether `--log` was explicitly passed on the command line.
    pub(crate) log_given: bool,
}

fn column(raw: &str) -> String {
    let mut safe = crate::pct::encode(&crate::pct::decode(raw));
    if safe.len() > crate::glance::ATTENTION_CAP {
        safe = crate::presence::clamp_pct(&safe, crate::glance::ATTENTION_CAP - ELLIPSIS.len());
        safe.push_str(ELLIPSIS);
    }
    if safe.is_empty() {
        "-".to_string()
    } else {
        safe
    }
}

const ELLIPSIS: &str = "%E2%80%A6";

fn roster(cfg: &Config, attention_only: bool) -> std::io::Result<()> {
    let (mut client, _closer) = transport::connect(&cfg.broker, crate::fabric::io_timeout())
        .map_err(|e| {
            std::io::Error::new(
                e.kind(),
                format!("cannot connect to the broker at {}: {e}", cfg.broker),
            )
        })?;
    transport::attach_cap_files(&mut client, &cfg.cap_files)?;
    let filter = format!("/f/{}/pub/*/*/presence", cfg.fleet);
    transport::walk_last(&mut client, &filter, |(_, subject, raw)| {
        let (body, _) = crate::body::Body::decode(raw);
        let (line, escalating) = row(subject, &body);
        if !attention_only || escalating {
            println!("{line}");
        }
        Ok(())
    })
}

fn row(subject: &str, body: &crate::body::Body) -> (String, bool) {
    let segs: Vec<&str> = subject.split('/').collect();
    let node = column(segs.get(4).unwrap_or(&"-"));
    let owner = column(segs.get(5).unwrap_or(&"-"));
    let field = |k: &str| {
        body.unknown
            .get(k)
            .map_or_else(|| "-".into(), |v| column(v))
    };
    let attention = field("attention");
    let escalating = attention != "-";
    let line = format!(
        "{node} {} {owner} state={} inc={} role={} detail={} driving={} \
         holder={} hold={} fabric={} attention={attention} gen={} observer={} epoch={} \
         phase={} context={} title={} proto={} aterm={}",
        field("host"),
        field("state"),
        field("inc"),
        field("role"),
        field("detail"),
        field("driving"),
        field("holder"),
        field("hold"),
        field("fabric"),
        body.gen.as_deref().map_or_else(|| "-".into(), column),
        body.unknown
            .get("observer")
            .map_or_else(|| "0".to_string(), |v| column(v)),
        body.epoch.as_deref().map_or_else(|| "-".into(), column),
        field("phase"),
        field("context"),
        field("title"),
        field("proto"),
        field("aterm"),
    );
    (line, escalating)
}

/// Parse `aterm-link` command-line arguments.
pub(crate) fn parse(args: &[String]) -> Result<Parsed, String> {
    let mut attention = false;
    let mut presence_given = false;
    let mut receipts_given = false;
    let mut log_given = false;
    let mut cfg = Config {
        fleet: String::new(),
        broker: String::new(),
        cap_files: Vec::new(),
        state_dir: String::new(),
        accept_from: Vec::new(),
        sock: None,
        token: None,
        presence: crate::presence::Mode::Meta,
        receipts: true,
        log: crate::mirror::LogMode::Auto,
        mesh: None,
    };
    let mut token_file: Option<String> = None;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match flag.as_str() {
            "--fleet" => cfg.fleet = value()?,
            "--broker" => cfg.broker = value()?,
            "--cap-file" => cfg.cap_files.push(value()?),
            "--state" => cfg.state_dir = value()?,
            "--accept-from" => {
                cfg.accept_from
                    .extend(value()?.split(',').map(str::to_string));
            }
            "--attention" => attention = true,
            "--presence" => {
                let v = value()?;
                cfg.presence = crate::presence::Mode::parse(&v)
                    .ok_or_else(|| format!("--presence {v}: `meta` or `minimal`"))?;
                presence_given = true;
            }
            "--receipts" => {
                cfg.receipts = true;
                receipts_given = true;
            }
            "--no-receipts" => {
                cfg.receipts = false;
                receipts_given = true;
            }
            "--log" => {
                let v = value()?;
                cfg.log = crate::mirror::LogMode::parse(&v)
                    .ok_or_else(|| format!("--log {v}: `auto`, `off` or `turns`"))?;
                log_given = true;
            }
            "--sock" => cfg.sock = Some(value()?),
            "--token-file" => token_file = Some(value()?),
            other => return Err(format!("unknown flag {other}")),
        }
    }
    if cfg.fleet.is_empty() || cfg.broker.is_empty() {
        return Err("--fleet and --broker are required".to_string());
    }
    if !crate::subject::is_fleet(&cfg.fleet) {
        return Err(format!(
            "--fleet {} is not a subject segment ([a-z0-9-]{{1,32}})",
            cfg.fleet
        ));
    }
    if cfg.state_dir.is_empty() {
        cfg.state_dir = default_state_dir();
    }
    // With the mesh on (§11 C3) the node key names this bridge's node, in the one fleet.
    cfg.mesh = crate::federate::MeshPaths::of(aterm_types::mesh_switch::MESH, &cfg.broker);
    if let Some(path) = token_file {
        cfg.token = Some(
            aterm_ctl::read_control_token_file(std::path::Path::new(&path))
                .map_err(|e| format!("--token-file {path}: {e}"))?,
        );
    }
    if cfg.sock.is_some() && cfg.token.is_none() {
        return Err("--sock needs --token-file".to_string());
    }
    for p in &cfg.accept_from {
        if !crate::subject::is_principal(p) {
            return Err(format!("--accept-from {p} is not a principal"));
        }
    }
    Ok(Parsed {
        cfg,
        attention,
        presence_given,
        receipts_given,
        log_given,
    })
}

/// Resolve the default `aterm-link` state directory.
pub(crate) fn default_state_dir() -> String {
    if let Ok(x) = std::env::var("XDG_STATE_HOME") {
        if !x.is_empty() {
            return format!("{x}/aterm-link");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return format!("{home}/.local/state/aterm-link");
        }
    }
    "./aterm-link-state".to_string()
}

fn broker(args: &[String]) -> ExitCode {
    let watch = ExeWatch::capture();
    const USAGE: &str = "\
usage: aterm link broker <socket> [log] [--secret-file <path>] [--durability <tier>]
                         [--lifeline-fd <n>]
       aterm link broker --repair <log>
       aterm link broker --retire-log <log>

  <socket>             the Unix socket the bridges' `--broker` names (keep it in a 0700
                       dir: nothing checks)
  [log]                the durable record log (default `<socket>.log`)
  --secret-file <path> the mint secret (32+ raw bytes, 0600, in a 0700 dir of yours,
                       held to `aterm link mint --help`'s rule) that `aterm link mint`
                       and `aterm fabric mint-for` mint under. Without
                       the flag a `mint.secret` beside <socket> is used; with neither, the
                       broker is unguarded and checks no capability
  --durability <tier>  what an ack promises; default `ordered` with --secret-file, else
                       `strict` (a `mint.secret` found beside <socket> keeps `strict`):
                       - `strict`: acks after a full flush (`F_FULLFSYNC` on macOS,
                         `fdatasync` on Linux).
                       - `ordered`: acks after an ordering barrier (`F_BARRIERFSYNC`) and
                         flushes in full within 100 ms; served `strict` off macOS, for a
                         log off the boot volume, or where the device refuses the barrier.
                       - `relaxed`: acks after the page-cache write; survives a broker
                         restart, not a power loss or panic.
                       Halts, a human's words, mail to a person and a person's read marks
                       always flush in full before their ack. A non-strict broker flushes
                       and marks its log closed on SIGTERM, SIGINT or SIGHUP; an unclean
                       stop across a reboot advances the log's generation.
  --lifeline-fd <n>    descriptor <n> is the read end of a pipe or FIFO its launcher holds
                       and never writes, opened by the launcher before it starts the
                       broker (on macOS a FIFO opened just as its writer closes can miss
                       its end): the broker exits cleanly within seconds of its
                       end-of-file. For a test's or tool's throwaway broker; a supervised
                       one takes none
  --repair <log>       cut <log> at its first corrupt record and exit (`nothing to repair`
                       when clean; refused while a running broker holds it, or for a log a
                       newer aterm wrote)
  --retire-log <log>   give up a log that will not open (a sealed log whose key is gone):
                       move it, its sidecars and its key to a `retired-<ms>` folder beside
                       it, nothing deleted, and mint a new key, so the next start begins a
                       new log with a new identity; refused while it opens or a broker
                       holds it, while a seal's switch or a compaction a crash stopped
                       waits for a start to finish it, and for a folder or a file that is
                       no log. A retire a crash stopped is run again: until then a start
                       refuses and mints nothing

A `rest.key` beside <log> (`aterm fabric on` mints one; 32 bytes, 0600, in a 0700 dir)
seals the log and its sidecars at rest: a plaintext log is sealed once at this start, under its
lock, and a sealed log whose key is missing or damaged is refused, naming the way out. On macOS
the key carries the mark Time Machine honours. No other system has a mark, so a backup tool
there has to be told to skip it. It protects the log in backups that leave the key out and in
copies made without the key, not against a process of this user or a copy of the whole folder.

Prints `listening <socket>` once bound and keeps `<log>.health` (0600) beside the log.
Exits 0 when its executable is replaced, its lifeline ends, or its socket path is removed
or taken by another socket; exits 75 when its log is poisoned, so its supervisor restarts
it. Retains `ev` records up to 7 days / 256 MiB, unread mail up to 7 days / 64 MiB, each
presence row's latest value, and other records up to 7 days / 32 MiB (`<log>.held` keeps
what compaction must not drop). Unread mail given up past its limit is logged here (its
subject and count) and told to its node's bridge (the range and, where counted, how many),
so every session that node serves names it in `inbox` (`given_up=`) and marks it in its log.
";
    let parsed = match crate::broker_args::BrokerArgs::parse(args) {
        Ok(parsed) => parsed,
        Err(crate::broker_args::ParseError::Help) => {
            return aterm_uds::stdout::answer_exit("aterm link", USAGE, 0);
        }
        Err(crate::broker_args::ParseError::Usage(error)) => {
            eprintln!("aterm link broker: {error}");
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let secret_file = parsed.secret_file.cloned();
    let durability = parsed.durability.cloned();
    let lifeline_fd = parsed.lifeline_fd.cloned();
    let lifeline_at = parsed.lifeline_at;
    let repair = parsed.repair;
    let retire = parsed.retire_log;
    let words: Vec<&str> = parsed.words.iter().map(|word| word.as_str()).collect();
    if repair && retire {
        eprintln!("aterm link broker: --repair and --retire-log are two ways out: name one");
        eprint!("{USAGE}");
        return ExitCode::from(2);
    }
    if repair || retire {
        let flag = if repair { "--repair" } else { "--retire-log" };
        if secret_file.is_some() || durability.is_some() || lifeline_fd.is_some() {
            eprintln!("aterm link broker: {flag} takes <log> alone: it serves nothing");
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
        let [log] = words.as_slice() else {
            eprintln!("aterm link broker: {flag} takes exactly one <log>");
            eprint!("{USAGE}");
            return ExitCode::from(2);
        };
        return if repair {
            repair_log(log)
        } else {
            retire_log(log)
        };
    }
    let usage_error = |why: &str| {
        eprintln!("aterm link broker: {why}");
        eprint!("{USAGE}");
        ExitCode::from(2)
    };
    let durability = match durability
        .as_deref()
        .map(crate::durability::Durability::parse)
        .transpose()
    {
        Ok(d) => crate::durability::resolve(d, secret_file.is_some()),
        Err(e) => return usage_error(&e),
    };
    let (endpoint, log) = match words.as_slice() {
        [sock] => ((*sock).to_string(), format!("{sock}.log")),
        [sock, log] => ((*sock).to_string(), (*log).to_string()),
        [] => {
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
        [_, _, extra, ..] => {
            return usage_error(&format!("unexpected argument `{extra}`"));
        }
    };
    let mut lifeline = match lifeline_fd.as_deref().map(adopt_lifeline).transpose() {
        Ok(lifeline) => lifeline,
        Err(why) => return usage_error(&why),
    };
    let secret_file = secret_file.or_else(|| {
        let beside = std::path::Path::new(&endpoint).with_file_name(crate::enable::MINT_SECRET);
        std::fs::symlink_metadata(&beside)
            .is_ok()
            .then(|| beside.to_string_lossy().into_owned())
    });
    let secret = match secret_file
        .as_deref()
        .map(|p| (p, transport::read_mint_secret(p)))
    {
        None => None,
        Some((_, Ok(s))) => Some(s),
        Some((path, Err(e))) => {
            eprintln!(
                "aterm link broker: --secret-file {}",
                transport::describe(path, &e)
            );
            return ExitCode::FAILURE;
        }
    };
    let log_created_here = !std::path::Path::new(&log).exists();
    let guarded = secret.is_some();
    let boot = crate::durability::this_boot();
    let stop = (durability != crate::durability::Durability::Strict)
        .then(|| take_stop_signals(std::thread::current()))
        .flatten();
    let seam = crate::broker_health::Seam::from_env(std::path::Path::new(&log));
    let at_rest = match crate::at_rest::prepare(
        std::path::Path::new(&log),
        aterm_uds::backup::Backups::SYSTEM,
    ) {
        Ok(at_rest) => at_rest,
        Err(refused) => {
            eprintln!("aterm link broker: {}", refused.why);
            eprintln!("aterm link broker: {}", refused.remedy);
            return ExitCode::FAILURE;
        }
    };
    for note in &at_rest.notes {
        eprintln!("aterm link broker: {note}");
    }
    let opened = crate::durability::open_escalating(
        std::path::Path::new(&log),
        secret,
        durability,
        boot,
        seam.escalation(),
        at_rest.key.as_ref().map(crate::at_rest::Key::bytes),
    );
    drop(at_rest);
    let broker = match opened {
        Ok(opened) => {
            use std::io::Write as _;
            let mut err = std::io::stderr();
            if let Some(why) = &opened.renewed {
                let _ = writeln!(
                    err,
                    "aterm link broker: {log}: {} — its generation moves, and every bridge and \
                     notifier resumes where it continues",
                    why.describe()
                );
            }
            if let Some(kept) = &opened.mark_kept {
                let _ = writeln!(
                    err,
                    "aterm link broker: could not remove the legacy relaxed mark {kept}; the \
                     log's generation moves once more after the next reboot"
                );
            }
            opened.broker
        }
        Err(e) => {
            eprintln!("aterm link broker: open {log}: {e}");
            if let Some(remedy) = crate::at_rest::open_remedy(std::path::Path::new(&log), e.kind())
                .or_else(|| log_refusal_remedy(e.kind(), &log))
            {
                eprintln!("aterm link broker: {remedy}");
            }
            return ExitCode::FAILURE;
        }
    };
    seam.arm(&broker, std::path::Path::new(&log));
    let retained = match crate::broker_rules::retain(&broker) {
        Ok(()) => format!(
            "retained by aterm's default rules ({})",
            crate::broker_rules::summary()
        ),
        Err(e) => {
            eprintln!(
                "aterm link broker: {log}: its retention rules could not be set ({e}); it serves \
                 unretained, and no bridge mirrors the session logs onto it by default"
            );
            "unretained".to_string()
        }
    };
    if let Err(e) = crate::broker_rules::limit(&broker) {
        eprintln!(
            "aterm link broker: {log}: its rate limit could not be set ({e}); it serves with no \
             limit on any producer"
        );
    }
    // The limit the broker serves under, read back from it: the start line says what it does.
    let limited = crate::broker_rules::rate_word(broker.rate_limit());
    let handle = match broker.serve(&endpoint) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("aterm link broker: serve {endpoint}: {e}");
            if log_created_here
                && std::fs::symlink_metadata(&log).is_ok_and(|m| m.is_file() && m.len() == 0)
            {
                drop(broker);
                let _ = std::fs::remove_file(&log);
            }
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = broker.keep_health(crate::broker_health::HEALTH_EVERY) {
        eprintln!(
            "aterm link broker: {}: could not write its health ({e}); astream tries again every \
             {} s",
            astream_broker::health_path(std::path::Path::new(&log)).display(),
            crate::broker_health::HEALTH_EVERY.as_secs()
        );
    }
    let guard_note = match secret_file.as_deref() {
        Some(path) => format!(" (guarded by {path})"),
        None => " (unguarded: it checks no capability)".to_string(),
    };
    {
        use std::io::Write as _;
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "listening {endpoint}");
        let _ = out.flush();
        let _ = writeln!(
            std::io::stderr(),
            "aterm link broker: unix socket on {endpoint}{guard_note}, durability {}: {}; \
             {retained}; {limited}; {}; at-rest={}; log {}",
            durability.name(),
            crate::durability::describe_served(
                durability,
                Some(broker.served_durability()),
                Some(guarded)
            ),
            crate::broker_health::residency_of(&broker),
            crate::at_rest::word(std::path::Path::new(&log)),
            broker.log_identity()
        );
    }
    let keep = handle;
    let mut watch = watch;
    let mut exe_looked = std::time::Instant::now();
    let mut poison_said = false;
    loop {
        std::thread::park_timeout(crate::broker_health::poll());
        let poisoned = match broker.poisoned() {
            Some(reason) if seam.on_poison == crate::broker_health::OnPoison::Stay => {
                if !poison_said {
                    eprintln!(
                        "aterm link broker: {log}: {} ({reason}); NOT exiting: {}=stay keeps it \
                         serving",
                        crate::broker_health::POISONED_WORDS,
                        crate::broker_health::FAULT_SEAM
                    );
                    poison_said = true;
                }
                None
            }
            poisoned => poisoned,
        };
        // How to start this broker again by hand, when no `aterm fabric on` job runs it: what
        // an exit that leaves it to its supervisor says instead (the poisoned log, a replaced
        // build; fix unit E5).
        let restart = || {
            (!crate::broker_health::supervised()).then(|| {
                let argv: Vec<String> = std::env::args_os()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect();
                crate::fabric::shown_argv(&by_hand(argv, args.len(), lifeline_at))
            })
        };
        let why = if let Some(sig) = stop.as_ref().and_then(StopSignal::taken) {
            format!("stopped by {sig}")
        } else if let Some(reason) = poisoned {
            use std::io::Write as _;
            let restart = restart();
            drop(keep);
            drop(broker);
            let _ = writeln!(
                std::io::stderr(),
                "{}",
                crate::broker_health::poisoned_line(&log, &reason, restart.as_deref())
            );
            return ExitCode::from(crate::broker_health::POISONED_EXIT);
        } else if let Some(end) = ends_itself(
            lifeline.as_mut().and_then(OnWake::cut),
            path_named(&endpoint, keep.names_listener()),
        ) {
            end.words(&endpoint)
        } else {
            let Some(w) = watch.as_mut() else {
                continue;
            };
            if exe_looked.elapsed() < EXE_WATCH {
                continue;
            }
            exe_looked = std::time::Instant::now();
            if !w.poll() {
                continue;
            }
            crate::broker_health::replaced_line(&w.path.display().to_string(), restart().as_deref())
        };
        use std::io::Write as _;
        let mut err = std::io::stderr();
        let _ = writeln!(err, "aterm link broker: {why}");
        drop(keep);
        if let Some(reason) = broker.poisoned() {
            let _ = writeln!(
                err,
                "aterm link broker: {log}: not closed cleanly: its log is {} ({reason}), which \
                 astream does not close, so the next open treats this run as an unclean stop",
                crate::broker_health::POISONED_WORDS
            );
        } else if durability != crate::durability::Durability::Strict {
            let _ = writeln!(
                err,
                "aterm link broker: {log}: closed by astream's clean close — flushed in full and \
                 marked closed, so a reboot keeps its identity, unless astream-broker said \
                 otherwise just above"
            );
        }
        return ExitCode::SUCCESS;
    }
}

/// The words a broker's exit line opens with when its lifeline was cut.
pub const LIFELINE_CUT_WORDS: &str = "lifeline cut";

/// The words a broker's exit line carries, after its socket path, when no socket is there any more.
pub const PATH_GONE_WORDS: &str = "is gone, so no client can reach this broker";

fn adopt_lifeline(number: &str) -> Result<OnWake, String> {
    let flag = aterm_uds::lifeline::FLAG;
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("{flag} {number} is not a descriptor number"));
    }
    let fd: i32 = number
        .parse()
        .map_err(|_| format!("{flag} {number} is not a descriptor number"))?;
    let lifeline = aterm_uds::lifeline::adopt(fd).map_err(|e| e.to_string())?;
    if let Err(e) = aterm_uds::lifeline::close_inherited_copies(&lifeline) {
        eprintln!(
            "aterm link broker: could not check for inherited copies of its lifeline ({e}); \
             one held here would keep it from ending"
        );
    }
    OnWake::new(lifeline).map_err(|e| format!("{flag} {number}: {e}"))
}

/// Why a serving broker ends itself at one wake of its loop ([`ends_itself`]).
#[derive(Debug)]
enum SelfEnd {
    /// Its lifeline read end-of-file, or could no longer be read: the process that
    /// started it is gone (or can no longer be told to live).
    LauncherGone(aterm_uds::lifeline::Cut),
    /// No socket is at its path any more: no client can reach it.
    PathGone,
    /// Another socket was bound or renamed over its path: clients reach that one.
    PathTaken,
}

impl SelfEnd {
    fn words(&self, endpoint: &str) -> String {
        match self {
            Self::LauncherGone(aterm_uds::lifeline::Cut::Closed) => format!(
                "{LIFELINE_CUT_WORDS} (end-of-file): the process that started it is gone; exiting"
            ),
            Self::LauncherGone(aterm_uds::lifeline::Cut::Failed(e)) => format!(
                "{LIFELINE_CUT_WORDS} (it could no longer be read: {e}): whether the process \
                 that started it lives cannot be told; exiting"
            ),
            Self::PathGone => format!("{endpoint} {PATH_GONE_WORDS}; exiting"),
            Self::PathTaken => format!(
                "{endpoint} now names another socket, which its clients reach instead; exiting"
            ),
        }
    }
}

fn ends_itself(
    cut: Option<aterm_uds::lifeline::Cut>,
    path: Option<aterm_uds::listener::Named>,
) -> Option<SelfEnd> {
    use aterm_uds::listener::Named;
    if let Some(cut) = cut {
        return Some(SelfEnd::LauncherGone(cut));
    }
    match path? {
        Named::Same | Named::Unread => None,
        Named::Nothing => Some(SelfEnd::PathGone),
        Named::Another => Some(SelfEnd::PathTaken),
    }
}

/// What `endpoint` names now, given `ours`: whether it names the broker's listener by the
/// identity astream recorded for it ([`astream_broker::BrokerHandle::names_listener`]), read
/// before the path named it unless `serve` had to bind the path itself, never by a read of
/// the path after `serve` returned. `None` when `ours` cannot tell.
fn path_named(endpoint: &str, ours: Option<bool>) -> Option<aterm_uds::listener::Named> {
    use aterm_uds::listener::Named;
    if ours? {
        return Some(Named::Same);
    }
    match aterm_uds::listener::listener_read(endpoint) {
        Ok(Some(_)) => Some(Named::Another),
        Ok(None) => Some(Named::Nothing),
        Err(_) => Some(Named::Unread),
    }
}

fn by_hand(mut argv: Vec<String>, args: usize, lifeline: Option<usize>) -> Vec<String> {
    let at = lifeline.map(|at| argv.len().saturating_sub(args) + at);
    if let Some(at) = at.filter(|&at| {
        argv.get(at).map(String::as_str) == Some(aterm_uds::lifeline::FLAG) && at + 1 < argv.len()
    }) {
        argv.drain(at..=at + 1);
    }
    argv
}

struct StopSignal(std::sync::Arc<std::sync::atomic::AtomicI32>);

impl StopSignal {
    fn taken(&self) -> Option<&'static str> {
        match self.0.load(std::sync::atomic::Ordering::Acquire) {
            0 => None,
            sig => Some(signal_name(sig)),
        }
    }
}

const fn signal_name(sig: i32) -> &'static str {
    match sig {
        libc::SIGTERM => "SIGTERM",
        libc::SIGINT => "SIGINT",
        libc::SIGHUP => "SIGHUP",
        _ => "a signal",
    }
}

/// Block `SIGTERM`, `SIGINT` and `SIGHUP` on the main thread (before worker threads spawn)
/// and unpark `main` when one arrives so astream flushes and marks the log `Closed`.
fn take_stop_signals(main: std::thread::Thread) -> Option<StopSignal> {
    let taken = std::sync::Arc::new(std::sync::atomic::AtomicI32::new(0));
    let flag = std::sync::Arc::clone(&taken);
    aterm_uds::process::take_stop_signals("broker-stop", move |sig| {
        flag.store(sig, std::sync::atomic::Ordering::Release);
        main.unpark();
    })
    .then_some(StopSignal(taken))
}

/// Remedy prefix printed when astream refuses to open a corrupt broker log.
pub const CORRUPT_LOG_REMEDY: &str = "the log has a corrupt record and is not opened: run `aterm \
                                      link broker --repair";

/// Remedy prefix printed when astream refuses a log written by a newer format.
pub const NEWER_LOG_REMEDY: &str = "the log was written by a newer aterm and is not opened";

/// One-line remedy for a broker log open refusal.
#[must_use]
pub fn log_refusal_remedy(kind: std::io::ErrorKind, log: &str) -> Option<String> {
    match kind {
        std::io::ErrorKind::InvalidData => Some(format!(
            "{CORRUPT_LOG_REMEDY} '{}'` to cut it there (the records from it on are lost)",
            log.replace('\'', "'\\''")
        )),
        std::io::ErrorKind::Unsupported => Some(format!(
            "{NEWER_LOG_REMEDY}: run that aterm release or a later one — no repair applies, and \
             {log} is untouched"
        )),
        _ => None,
    }
}

fn repair_log(log: &str) -> ExitCode {
    if let Some(refused) = crate::at_rest::unfinished_retire(std::path::Path::new(log)) {
        return refuse_repair(&refused);
    }
    match std::fs::symlink_metadata(log) {
        Ok(m) if m.is_file() => {}
        Ok(_) => {
            eprintln!("aterm link broker --repair: {log} is not a file");
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!("aterm link broker --repair: {log}: {e}");
            return ExitCode::FAILURE;
        }
    }
    // By the seal, not by the key: a key beside a plaintext log the seal could not take (a damaged
    // one, say) repairs keyless, and a sealed log never does.
    let at = std::path::Path::new(log);
    let opened = if crate::at_rest::is_sealed(at) {
        match crate::at_rest::read_key(at) {
            crate::at_rest::KeyFile::Held(key) => astream_broker::BrokerLog::open_encrypted_repair(
                log,
                astream_broker::Durability::Strict,
                key.bytes(),
            ),
            crate::at_rest::KeyFile::Damaged(problem) => {
                return refuse_repair(&crate::at_rest::refusal(at, &problem));
            }
            crate::at_rest::KeyFile::Absent => {
                return refuse_repair(&crate::at_rest::refusal(
                    at,
                    &format!("{}: no such file", crate::at_rest::key_path(at).display()),
                ));
            }
        }
    } else {
        astream_broker::BrokerLog::open_repair(log, astream_broker::Durability::Strict)
    };
    let said = match opened {
        Ok((_, 0)) => format!("{log}: nothing to repair"),
        Ok((_, dropped)) => {
            format!("{log}: repaired, dropped {dropped} byte(s) from the corrupt record on")
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            eprintln!(
                "aterm link broker --repair: {log} is held by a running broker ({e}); repair it \
                 while no broker has it open"
            );
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!("aterm link broker --repair: {log}: {e}");
            if e.kind() == std::io::ErrorKind::Unsupported {
                if let Some(remedy) = log_refusal_remedy(e.kind(), log) {
                    eprintln!("aterm link broker --repair: {remedy}");
                }
            } else if let Some(remedy) = crate::at_rest::repair_remedy(at, e.kind()) {
                eprintln!("aterm link broker --repair: {remedy}");
            }
            return ExitCode::FAILURE;
        }
    };
    use std::io::Write as _;
    let _ = writeln!(std::io::stdout().lock(), "{said}");
    ExitCode::SUCCESS
}

/// A repair that does not run: the log untouched, and the way out.
fn refuse_repair(refused: &crate::at_rest::Refused) -> ExitCode {
    eprintln!("aterm link broker --repair: {}", refused.why);
    eprintln!("aterm link broker --repair: {}", refused.remedy);
    ExitCode::FAILURE
}

fn retire_log(log: &str) -> ExitCode {
    match crate::at_rest::retire(
        std::path::Path::new(log),
        aterm_uds::backup::Backups::SYSTEM,
    ) {
        Ok(retired) => {
            use std::io::Write as _;
            let _ = writeln!(
                std::io::stdout().lock(),
                "{log}: retired to {} ({}), nothing deleted; {}the next start begins a new log \
                 with a new identity, and a key found later still reads the retired one",
                retired.to.display(),
                retired.moved.join(" "),
                if retired.minted {
                    "a new key is minted; "
                } else {
                    ""
                }
            );
            ExitCode::SUCCESS
        }
        Err(why) => {
            eprintln!("aterm link broker --retire-log: {why}");
            ExitCode::FAILURE
        }
    }
}

const EXE_WATCH: std::time::Duration = std::time::Duration::from_secs(30);

/// A file's identity (device, inode, length, mtime), so a replaced executable (a new inode
/// renamed over the path, or the same inode rewritten) is told from the one a process started
/// from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileId {
    dev: u64,
    ino: u64,
    len: u64,
    mtime: i64,
    mtime_nsec: i64,
}

impl FileId {
    fn of(path: &std::path::Path) -> std::io::Result<Self> {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::metadata(path)?;
        if !m.is_file() {
            return Err(std::io::Error::other(format!(
                "{} is not a regular file",
                path.display()
            )));
        }
        Ok(Self {
            dev: m.dev(),
            ino: m.ino(),
            len: m.len(),
            mtime: m.mtime(),
            mtime_nsec: m.mtime_nsec(),
        })
    }
}

enum RunningImage {
    File(FileId),
    #[cfg(not(target_os = "linux"))]
    Inode(u64),
}

impl RunningImage {
    fn capture() -> Option<Self> {
        #[cfg(target_os = "linux")]
        {
            FileId::of(std::path::Path::new("/proc/self/exe"))
                .ok()
                .map(Self::File)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let inode = crate::fabric::running_image_inode(std::process::id());
            let file = std::env::current_exe()
                .ok()
                .and_then(|p| FileId::of(&p).ok());
            match (file, inode) {
                (Some(file), Some(inode)) if file.ino == inode => Some(Self::File(file)),
                (_, Some(inode)) => Some(Self::Inode(inode)),
                (Some(file), None) => Some(Self::File(file)),
                (None, None) => None,
            }
        }
    }

    fn matches(&self, now: &FileId) -> bool {
        match self {
            Self::File(start) => start == now,
            #[cfg(not(target_os = "linux"))]
            Self::Inode(inode) => *inode == now.ino,
        }
    }
}

fn invocation_path(
    argv0: &std::ffi::OsStr,
    path: Option<&std::ffi::OsStr>,
) -> Option<std::path::PathBuf> {
    let invocation = std::path::Path::new(argv0);
    let file = if invocation.components().count() == 1 && !invocation.is_absolute() {
        crate::enable::which_in(argv0, path?)?
    } else {
        invocation.to_path_buf()
    };
    std::path::absolute(file).ok()
}

struct ExeWatch {
    path: std::path::PathBuf,
    start: RunningImage,
    seen: Option<FileId>,
}

impl ExeWatch {
    fn capture() -> Option<Self> {
        let start = RunningImage::capture()?;
        let argv0 = std::env::args_os().next()?;
        let path = invocation_path(&argv0, std::env::var_os("PATH").as_deref())?;
        Some(Self {
            path,
            start,
            seen: None,
        })
    }

    fn poll(&mut self) -> bool {
        let now = FileId::of(&self.path);
        let changed = replaced(&self.start, self.seen.as_ref(), &now);
        self.seen = now.ok();
        changed
    }
}

fn replaced(start: &RunningImage, seen: Option<&FileId>, now: &std::io::Result<FileId>) -> bool {
    matches!(now, Ok(now) if !start.matches(now) && seen == Some(now))
}

fn mint(args: &[String]) -> ExitCode {
    const USAGE: &str = "\
usage: aterm link mint <grant> --secret-file <path>

  <grant>              e.g. `rw,p=<node>:/f/<fleet>/pub/<node>/>` or `ro:/f/<fleet>/pub/>`
  --secret-file <path> the mint key, used byte for byte: 32+ raw bytes, 0600, in a 0700
                       dir of yours (a looser mode, an access-list allow entry or a
                       symlink on the file or its dir, a second link to the file, or
                       fewer bytes is refused; a deny entry passes)

Prints one `<grant> <tag-hex>` line: append it to the file `serve --cap-file` reads, which
must be 0600 or tighter (a shell redirect makes a new file 0644: `chmod 600` it).
";
    let mut grant: Option<&str> = None;
    let mut secret: Option<&str> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--secret-file" => {
                // Once: a repeated flag never wins last.
                if secret.is_some() {
                    eprintln!("aterm link mint: --secret-file given twice");
                    return ExitCode::from(2);
                }
                i += 1;
                match args.get(i) {
                    Some(v) => secret = Some(v),
                    None => {
                        eprintln!("aterm link mint: --secret-file needs a path");
                        return ExitCode::from(2);
                    }
                }
            }
            "-h" | "--help" => return aterm_uds::stdout::answer_exit("aterm link", USAGE, 0),
            other if grant.is_none() && !other.starts_with('-') => grant = Some(other),
            other => {
                eprintln!("aterm link mint: unknown argument `{other}`");
                eprint!("{USAGE}");
                return ExitCode::from(2);
            }
        }
        i += 1;
    }
    let (Some(grant), Some(secret_path)) = (grant, secret) else {
        eprint!("{USAGE}");
        return ExitCode::from(2);
    };
    let key = match transport::read_mint_secret(secret_path) {
        Ok(k) => k,
        Err(e) => {
            eprintln!(
                "aterm link mint: --secret-file {}",
                transport::describe(secret_path, &e)
            );
            return ExitCode::FAILURE;
        }
    };
    match astream_cap::mint(&key, grant) {
        Ok(cap) => {
            // astream cap-file line format.
            minted_cap_to(
                &mut std::io::stdout().lock(),
                &astream_cap::capfile::format_line(&cap.filter, &cap.tag),
            )
        }
        Err(e) => {
            eprintln!("aterm link mint: {e}");
            ExitCode::FAILURE
        }
    }
}

fn minted_cap_to(out: &mut dyn std::io::Write, line: &str) -> ExitCode {
    match crate::person::write_capability_to(out, &format!("{line}\n")) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!(
                "aterm link mint: the capability was not written whole ({error}); mint again"
            );
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
