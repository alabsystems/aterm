// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-nest` — build an N-deep stack of nested aterms and run a command in the
//! DEEPEST one, proving the recursive-stacking feature from the command line.
//!
//! Each level is a real headless `aterm-gui` (engine + control socket, no window).
//! Level 0 is spawned directly; every deeper level is spawned by driving the level
//! above it over ITS OWN control socket — so authority is exercised one hop at a
//! time, by each level's owner, never borrowed transitively (the confused-deputy
//! boundary the proxy enforces and the Trust `authorize_soundness` model proves).
//!
//! The stack's settings are HERMETIC BUT NOT BARE: every level runs against a
//! per-run scratch `XDG_CONFIG_HOME` seeded with a COPY of the caller's
//! `aterm.toml` (see [`seed_config_dir`]), so the nested terminals look like the
//! caller's while anything they write lands in the copy and is discarded on
//! teardown.
//!
//! Usage:
//!   aterm-nest [--depth N] [--keep] [--gui PATH] -- <command> [args...]
//!   aterm-nest --depth 3 -- claude -p "say hi"
//!
//! It prints the deepest terminal's visible output for the command, then tears the
//! stack down (`--keep` leaves it running and prints the per-level sockets).

use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use aterm_uds::CtlStream;

const DONE: &str = "__ATERM_NEST_DONE__";

struct Args {
    depth: usize,
    keep: bool,
    gui: PathBuf,
    cmd: Vec<String>,
}

fn usage() -> ! {
    eprintln!(
        "usage: aterm-nest [--depth N] [--keep] [--gui PATH] -- <command> [args...]\n\
         \n\
         Builds an N-deep stack of nested headless aterms and runs <command> in the\n\
         deepest, driving each level over its own control socket.\n\
         \n\
         --depth N   number of nested aterm levels (default 1, max 8)\n\
         --keep      leave the stack running; print each level's socket\n\
         --gui PATH  path to the aterm-gui binary (default: sibling of this binary,\n\
                     then $ATERM_NEST_GUI, then `aterm-gui` on PATH)"
    );
    std::process::exit(2);
}

fn parse_args() -> Args {
    match parse_args_opt() {
        Some(a) => a,
        None => usage(),
    }
}

/// The fallible core of `parse_args`. `None` means "print usage and exit(2)",
/// hoisted into the single diverging call in the wrapper above so this
/// function itself stays free of `-> !` calls the verifier cannot lower.
fn parse_args_opt() -> Option<Args> {
    let mut depth = 1usize;
    let mut keep = false;
    let mut gui: Option<PathBuf> = None;
    let mut cmd: Vec<String> = Vec::new();
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--depth" => depth = it.next().and_then(|v| v.parse().ok())?,
            "--keep" => keep = true,
            "--gui" => gui = Some(PathBuf::from(it.next()?)),
            "--help" | "-h" => return None,
            "--" => {
                cmd.extend(it.by_ref());
                break;
            }
            other => {
                // First non-flag begins the command (the `--` is optional).
                cmd.push(other.to_string());
                cmd.extend(it.by_ref());
                break;
            }
        }
    }
    if cmd.is_empty() || depth == 0 || depth > 8 {
        return None;
    }
    Some(Args {
        depth,
        keep,
        gui: gui.unwrap_or_else(resolve_gui),
        cmd,
    })
}

/// Locate the `aterm-gui` binary: explicit `--gui` (handled by caller), then a
/// sibling of this binary (the common cargo layout), then `$ATERM_NEST_GUI`, then
/// bare `aterm-gui` (PATH lookup).
fn resolve_gui() -> PathBuf {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sib = dir.join("aterm-gui");
        if sib.is_file() {
            return sib;
        }
    }
    if let Ok(p) = std::env::var("ATERM_NEST_GUI") {
        return PathBuf::from(p);
    }
    PathBuf::from("aterm-gui")
}

// --- minimal control-socket client (mirrors aterm-ctl's framing) ---

/// The capability token sitting beside the socket (`aterm-<pid>.token`).
fn read_token(sock: &str) -> Option<String> {
    let tok = sock.strip_suffix(".sock")?.to_string() + ".token";
    std::fs::read_to_string(tok)
        .ok()
        .map(|s| s.trim().to_string())
}

/// Render `AUTH <tok>\n` without `fmt::Arguments` (which the Trust verifier
/// cannot lower), returning the wire bytes directly.
fn auth_line_bytes(tok: &str) -> Vec<u8> {
    let mut line = String::new();
    line.push_str("AUTH ");
    line.push_str(tok);
    line.push('\n');
    line.into_bytes()
}

/// Render `<verb>\n` as wire bytes (same `fmt`-free rationale as above).
fn verb_line_bytes(verb: &str) -> Vec<u8> {
    let mut line = String::new();
    line.push_str(verb);
    line.push('\n');
    line.into_bytes()
}

/// Send one verb; return the status line (no follow-up payload read).
fn verb_status(sock: &str, verb: &str) -> io::Result<String> {
    let s = CtlStream::connect(sock)?;
    if let Some(tok) = read_token(sock) {
        (&s).write_all(&auth_line_bytes(&tok))?;
    }
    (&s).write_all(&verb_line_bytes(verb))?;
    (&s).flush()?;
    let mut line = String::new();
    BufReader::new(&s).read_line(&mut line)?;
    Ok(line.trim_end().to_string())
}

/// Send a streaming verb (`text`/`screen`): read the `OK <n>` header then the n
/// follow-up lines, returned joined.
fn verb_stream(sock: &str, verb: &str) -> io::Result<String> {
    let s = CtlStream::connect(sock)?;
    if let Some(tok) = read_token(sock) {
        (&s).write_all(&auth_line_bytes(&tok))?;
    }
    (&s).write_all(&verb_line_bytes(verb))?;
    (&s).flush()?;
    let mut r = BufReader::new(&s);
    let mut status = String::new();
    r.read_line(&mut status)?;
    let n: usize = status
        .trim()
        .strip_prefix("OK ")
        .and_then(|x| x.split_whitespace().next())
        .and_then(|x| x.parse().ok())
        .unwrap_or(0);
    let mut out = String::new();
    for _ in 0..n {
        let mut l = String::new();
        if r.read_line(&mut l)? == 0 {
            break;
        }
        out.push_str(&l);
    }
    Ok(out)
}

/// Type `text` into a level's shell and submit it (cmd_send turns a trailing
/// literal `\n` into CR). `text` must not contain a real newline.
fn type_line(sock: &str, text: &str) -> io::Result<()> {
    // `send <text>\n` where the trailing two chars are a literal backslash-n.
    // (Manual rendering; `fmt::Arguments` is unlowerable for the verifier.)
    let mut verb = String::new();
    verb.push_str("send ");
    verb.push_str(text);
    verb.push('\\');
    verb.push('n');
    let _ = verb_status(sock, &verb)?;
    Ok(())
}

/// Parse `aterm-gui: control socket listening at <PATH> (token-gated...)`.
fn socket_from_line(line: &str) -> Option<String> {
    let after = line.split("listening at ").nth(1)?;
    let path = after.split(" (token-gated").next()?.trim();
    if path.is_empty() {
        None
    } else {
        Some(path.to_string())
    }
}

/// Spawn level 0 directly, returning (child, socket-path). Reads the child's stderr
/// until the "listening at" line (then stops, leaving the child running).
///
/// `cfg` is the per-run scratch `XDG_CONFIG_HOME` from [`seed_config_dir`]; passing
/// it is what keeps the stack off the caller's real `aterm.toml` (see that
/// function's doc for why a nested level could otherwise WRITE it).
fn spawn_root(gui: &Path, cfg: &Path) -> io::Result<(std::process::Child, String)> {
    // Headless via the FLAG — the canonical arming ($ATERM_HEADLESS is an exact
    // equivalent). The launch announces the mode on stderr, on the line before
    // the "listening at" line this function scans for.
    let mut child = Command::new(gui)
        .arg("--headless")
        .env("ATERM_LINES", "40")
        .env("ATERM_COLUMNS", "120")
        .env("XDG_CONFIG_HOME", cfg)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    // stderr is always Some here (we set `Stdio::piped()` above and this is the
    // first `take()`); the guard replaces an `expect` so the verifier sees a
    // panic-free path instead of an unprovable `expect` precondition.
    let Some(err) = child.stderr.take() else {
        let _ = child.kill();
        return Err(io::Error::other("aterm-gui (L0): stderr pipe missing"));
    };
    // The announce hunt runs on a helper thread: a blocking `read_line` HERE
    // would only test the 20 s deadline between lines, so a child that starts
    // but never speaks (or wedges mid-line) would hang this parent forever.
    // The thread owns the pipe end-to-end — it hunts for the "listening at"
    // line, reports the outcome once over the channel, then keeps draining
    // stderr so the long-lived child can never block on a full pipe. The
    // drain copies into `io::sink()`, which retains NOTHING: a `read_to_end`
    // Vec here would grow for as long as the gui keeps logging.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut r = BufReader::new(err);
        let mut line = String::new();
        let found = loop {
            line.clear();
            match r.read_line(&mut line) {
                // EOF before the announce line: the child died or closed
                // stderr without ever listening — nothing left to drain.
                Ok(0) => {
                    break Err(io::Error::other(
                        "aterm-gui (L0) did not announce its socket",
                    ));
                }
                Ok(_) => {
                    if let Some(sock) = socket_from_line(&line) {
                        break Ok(sock);
                    }
                }
                Err(e) => break Err(e),
            }
        };
        let announced = found.is_ok();
        // A failed send means the parent already timed out and moved on to
        // kill the child; nothing more to do either way.
        let _ = tx.send(found);
        if announced {
            let _ = io::copy(&mut r.into_inner(), &mut io::sink());
        }
    });
    // `recv_timeout` IS the deadline: it fires even while the reader is
    // parked inside `read_line`. Every failure path kills the child (a
    // timed-out gui would otherwise outlive us as an orphan).
    match rx.recv_timeout(Duration::from_secs(20)) {
        Ok(Ok(sock)) => Ok((child, sock)),
        Ok(Err(e)) => {
            let _ = child.kill();
            Err(e)
        }
        Err(_) => {
            let _ = child.kill();
            Err(io::Error::other(
                "aterm-gui (L0) did not announce its socket",
            ))
        }
    }
}

/// POSIX single-quote a string for safe interpolation into a `sh -c` command
/// line. Wrap in single quotes and rewrite every embedded single quote as the
/// close-escape-reopen sequence `'\''`, so spaces, globs, and other shell
/// metacharacters in the gui/errfile paths are taken literally (no word-splitting).
fn sh_quote(s: &str) -> String {
    // Manual rendering of `format!("'{}'", s.replace('\'', "'\\''"))`:
    // byte-identical output, but with no `fmt::Arguments`/`str::replace`,
    // which the Trust verifier cannot lower (and whose inlined unsafe would
    // otherwise fail the strict gate closed).
    // Capacity is only a hint (never affects the output), so bounding it
    // keeps the allocation budget provable while remaining behavior-identical.
    // The branch shape gives the allocation-budget checker a dominating
    // comparison on the exact count operand.
    let cap = s.len().saturating_add(2);
    let mut out = if cap < 4096 {
        String::with_capacity(cap)
    } else {
        String::with_capacity(4096)
    };
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            // The close-escape-reopen sequence `'\''`.
            out.push('\'');
            out.push('\\');
            out.push('\'');
            out.push('\'');
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Spawn a deeper level by driving `parent_sock`'s shell to exec aterm-gui with its
/// stderr redirected to `errfile`; poll `errfile` for the "listening at" line.
///
/// The typed launch line (`VAR=1 cmd 2>file`, [`sh_quote`]) is POSIX shell
/// syntax: depth >= 2 nesting functionally requires a POSIX-ish shell inside
/// the nested terminal (on Windows too — e.g. Git Bash). A limitation of the
/// nested-spawn feature, not of the control-socket transport.
///
/// `cfg` (the scratch `XDG_CONFIG_HOME`) is re-stated on this line even though the
/// level above already exports it and `aterm-pty` forwards its environment to the
/// shell: the same belt-and-braces `ATERM_LINES`/`ATERM_COLUMNS` get, and here it
/// is load-bearing — if inheritance ever stopped carrying the variable, the
/// SILENT failure is a nested level writing the caller's real settings.
fn spawn_child(parent_sock: &str, gui: &Path, errfile: &str, cfg: &Path) -> io::Result<String> {
    let _ = std::fs::remove_file(errfile);
    type_line(parent_sock, &launch_line(gui, errfile, cfg))?;
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
        if let Ok(body) = std::fs::read_to_string(errfile) {
            for line in body.lines() {
                if let Some(sock) = socket_from_line(line) {
                    return Ok(sock);
                }
            }
        }
    }
    Err(io::Error::other(
        "nested aterm did not announce its socket in time",
    ))
}

/// The line [`spawn_child`] types into the level above. Split out (pure) for
/// testing, because what it carries is a security property, not a formatting
/// detail: drop the `XDG_CONFIG_HOME=` prefix and every level below this one goes
/// back to reading — and writing — the caller's real `aterm.toml`.
///
/// Every interpolation is single-quoted: the parent shell would otherwise
/// word-split a path containing a space (or expand globs/metachars), and a
/// redirect target undergoes field-splitting too — either silently breaking
/// depth>=2 nesting. (Manual concatenation; `fmt::Arguments` is unlowerable for
/// the verifier.)
fn launch_line(gui: &Path, errfile: &str, cfg: &Path) -> String {
    let g = gui.to_string_lossy();
    let c = cfg.to_string_lossy();
    let mut launch = String::new();
    launch.push_str("XDG_CONFIG_HOME=");
    launch.push_str(&sh_quote(&c));
    launch.push_str(" ATERM_LINES=40 ATERM_COLUMNS=120 ");
    launch.push_str(&sh_quote(&g));
    // Headless via the FLAG (see `spawn_root`): the inner instance must never
    // depend on the outer's environment, which CONSUMED `ATERM_HEADLESS` at its
    // own boot precisely so a nested aterm is not a surprise headless engine.
    launch.push_str(" --headless 2>");
    launch.push_str(&sh_quote(errfile));
    launch
}

/// The base directory for the per-run private rundir: `$XDG_RUNTIME_DIR` (a
/// per-user 0700 dir on Linux), then `$TMPDIR`, then `/tmp`. Most-private first.
#[cfg(unix)]
fn rundir_base() -> PathBuf {
    if let Some(v) = std::env::var_os("XDG_RUNTIME_DIR")
        // SAFETY: the unsafe block here is std's own, inlined from
        // `OsStr::is_empty` (the byte-slice view of an `OsStr`, sound because
        // `OsStr` is byte-backed by construction); MIR inlining re-attributes
        // its span to this call site. No local invariant is required.
        && !v.is_empty()
    {
        return PathBuf::from(v);
    }
    if let Some(v) = std::env::var_os("TMPDIR")
        // SAFETY: same as above — std's `OsStr::is_empty` byte-view unsafe,
        // upheld by std and merely re-attributed here by MIR inlining.
        && !v.is_empty()
    {
        return PathBuf::from(v);
    }
    PathBuf::from("/tmp")
}

/// Windows: `%TEMP%` lives under the user profile, whose default per-user ACL
/// (owner + SYSTEM + Administrators) is the isolation boundary — there is no
/// world-writable `/tmp` analog to defend against here.
#[cfg(windows)]
fn rundir_base() -> PathBuf {
    std::env::temp_dir()
}

/// Create a per-run private directory (mode 0700) inside `base`, mkdtemp-style:
/// an unpredictable name created atomically with `O_EXCL`-equivalent semantics
/// (`mkdir` fails if the name exists). The deeper levels' stderr files and the
/// scratch config dir ([`seed_config_dir`]) live inside it.
///
/// The 0700 directory — not merely the random name — is the security boundary:
/// the parent shell launches each level with `... 2>{errfile}`, a redirection
/// that cannot pass `O_EXCL` and follows symlinks, so a random filename alone
/// in shared `/tmp` is still pre-positionable by a different uid. Inside a 0700
/// dir no other uid can create entries or read through to the errfile (which
/// carries the live control-socket path), which defeats the symlink-follow.
/// The scratch config inherits that same boundary: no other uid can plant an
/// `aterm.toml` under it for the nested levels to load.
/// Digit glyphs for the manual decimal/hex renderers below. Both index it
/// through a `& 0xf` mask, so every lookup is provably in-bounds (16 entries).
const DIGITS16: &[u8; 16] = b"0123456789abcdef";

/// Append `v` in decimal (exactly `format!("{v}")`) without routing through
/// `fmt::Arguments`, which the Trust verifier cannot lower. Divisors are
/// literals (provably nonzero) and every digit is masked before the table
/// lookup, so all obligations discharge structurally.
///
/// Only the `#[cfg(unix)]` `make_rundir_in` renders names this way (the Windows
/// twin uses `format!` directly), so on Windows this is exercised solely by the
/// cross-platform `manual_renderers_match_format` test — hence `any(unix, test)`.
#[cfg(any(unix, test))]
fn push_dec_u32(s: &mut String, v: u32) {
    // Leading digits, most-significant first; each is `< 10` by construction
    // (`% 10`, and `v / 1e9 <= 4` for a u32), so the mask is a no-op.
    let leading = [
        (v / 1_000_000_000) % 10,
        (v / 100_000_000) % 10,
        (v / 10_000_000) % 10,
        (v / 1_000_000) % 10,
        (v / 100_000) % 10,
        (v / 10_000) % 10,
        (v / 1_000) % 10,
        (v / 100) % 10,
        (v / 10) % 10,
    ];
    let mut started = false;
    for &d in leading.iter() {
        if started || d != 0 {
            started = true;
            s.push(DIGITS16[(d & 0xf) as usize] as char);
        }
    }
    // The ones digit is always emitted, so `v == 0` renders as "0".
    s.push(DIGITS16[((v % 10) & 0xf) as usize] as char);
}

/// Append `v` as a single decimal digit — exact for `v <= 9` (every caller
/// passes a nesting level, capped at 8 by `parse_args`); the mask keeps the
/// table lookup provably in bounds either way.
fn push_digit(s: &mut String, v: usize) {
    s.push(DIGITS16[v & 0xf] as char);
}

/// Append `x` as exactly 16 lowercase hex digits (the `format!("{x:016x}")`
/// shape) without routing through `fmt::Arguments`.
///
/// Unix-only in production (the Windows `make_rundir_in` uses `format!`); on
/// Windows only the `manual_renderers_match_format` test reaches it, so gate on
/// `any(unix, test)` to keep it out of the dead Windows bin build.
#[cfg(any(unix, test))]
fn push_hex16(s: &mut String, x: u64) {
    let mut i = 16u32;
    while i > 0 {
        i -= 1;
        let d = ((x >> (i * 4)) & 0xf) as usize;
        s.push(DIGITS16[d] as char);
    }
}

#[cfg(unix)]
fn make_rundir_in(base: &Path, pid: u32) -> io::Result<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let mut last = io::Error::other("could not create private rundir");
    for _ in 0..8 {
        // Manual `aterm-nest-{pid}-{:016x}` rendering: byte-identical to the
        // former `format!`, but lowerable by the verifier (no fmt::Arguments).
        let mut name = String::new();
        name.push_str("aterm-nest-");
        push_dec_u32(&mut name, pid);
        name.push('-');
        push_hex16(&mut name, rand_u64());
        let path = base.join(name);
        match std::fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => {
                // mkdir's mode is masked by the process umask, so an unusual
                // umask (e.g. 0777) would yield a 0000 dir the nested shell
                // cannot write its errfile into (startup would then time out).
                // chmod is NOT umask-masked — force exactly 0700. This only ever
                // NARROWS access during the create..chmod window (the dir is
                // 0000..0700, owner-only throughout, created by us in a sticky
                // base), so it cannot widen exposure to another uid.
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
                return Ok(path);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => last = e,
            Err(e) => return Err(e),
        }
    }
    Err(last)
}

/// Windows twin: same unpredictable name + atomic `create_dir` (fails if the
/// name exists). No POSIX 0700 bits — the `%TEMP%` default per-user ACL is
/// the boundary (see [`rundir_base`]); the random name still keeps two
/// concurrent runs from colliding.
#[cfg(windows)]
fn make_rundir_in(base: &Path, pid: u32) -> io::Result<PathBuf> {
    let mut last = io::Error::other("could not create private rundir");
    for _ in 0..8 {
        let path = base.join(format!("aterm-nest-{pid}-{:016x}", rand_u64()));
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => last = e,
            Err(e) => return Err(e),
        }
    }
    Err(last)
}

/// The caller's OWN `aterm.toml`, resolved with the SAME precedence the gui uses
/// (`app_config::config_path`): `$XDG_CONFIG_HOME/aterm/aterm.toml`, else (Windows)
/// `%APPDATA%\aterm\aterm.toml`, else `$HOME/.config/aterm/aterm.toml`. Read only —
/// this is the file the stack must never touch, and the file it copies FROM.
///
/// Resolved from THIS process's environment, which the isolation never mutates
/// (the scratch dir is handed to each child through `Command::env` / the typed
/// launch line), so the source cannot drift to the copy.
fn caller_config_file() -> Option<PathBuf> {
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|x| !x.is_empty()) {
        return Some(PathBuf::from(x).join("aterm").join("aterm.toml"));
    }
    #[cfg(windows)]
    if let Some(appdata) = std::env::var_os("APPDATA").filter(|a| !a.is_empty()) {
        return Some(PathBuf::from(appdata).join("aterm").join("aterm.toml"));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/aterm/aterm.toml"))
}

/// Seed the per-run scratch config dir inside `rundir` and return it (the value
/// every level gets as `XDG_CONFIG_HOME`): `<rundir>/cfg`, holding a COPY of the
/// caller's `aterm.toml` at `<rundir>/cfg/aterm/aterm.toml`.
///
/// WHY AT ALL. Config resolution has no probe marker and no `ATERM_CONFIG`
/// override, so a headless level launched with the caller's environment resolves
/// the DEVELOPER'S OWN `~/.config/aterm/aterm.toml` — and can WRITE it, because
/// the token beside each socket carries owner scope and owner satisfies
/// `ConfigWrite` unconditionally. That is not hypothetical: on 2026-08-10 a probe
/// left `game_font = "minecraft"` in the owner's live settings and changed the
/// font of their real terminal. `tools/visual-judge/*` was fixed the same day
/// (edfdf7d4); this spawner was the one left.
///
/// WHY A COPY rather than the bare scratch dir those harnesses use: `aterm-nest`
/// exists to SHOW the nesting feature, and a stack rendering in stock defaults
/// while the outer terminal wears the user's theme, font and trail is a worse
/// demo — the levels stop looking nested. Copying keeps the picture and still
/// contains every write, because the copy is what the levels open. The nested
/// stack is not a settings editor, so nothing of value is lost when the copy goes.
///
/// Best-effort by design: an unreadable or absent source leaves the scratch dir
/// EMPTY (the levels then run on built-in defaults, exactly like the visual-judge
/// probes). Isolation must not depend on the copy succeeding — only on the dir
/// existing, which is why that half is the one that returns `Err`.
///
/// NOT copied, deliberately: `themes/` (a theme pack can be a whole checkout — a
/// level naming a USER theme falls back to the built-in scheme) and the
/// `kitty-log`/`kitty-collectibles` ledgers (a demo stack starts with an empty
/// collection rather than a clone of the caller's).
fn seed_config_dir(rundir: &Path) -> io::Result<PathBuf> {
    seed_config_dir_from(rundir, caller_config_file().as_deref())
}

/// The seam of [`seed_config_dir`] with the source handed in, so the copy wiring
/// is unit-tested without mutating the process-global environment (the same shape
/// `aterm_pty::build_child_env` uses for its deny-list).
fn seed_config_dir_from(rundir: &Path, src: Option<&Path>) -> io::Result<PathBuf> {
    let cfg = rundir.join("cfg");
    // `<cfg>/aterm/` is where `config_path()` looks; create it up front so a
    // level that WRITES settings lands in the copy instead of failing over to
    // some other path.
    let aterm_dir = cfg.join("aterm");
    std::fs::create_dir_all(&aterm_dir)?;
    if let Some(src) = src {
        // Copy failures are non-fatal (see the doc above): a missing source is
        // the common case on a fresh machine, and an unreadable one still
        // leaves an isolated — merely bare — scratch dir behind.
        let _ = std::fs::copy(src, aterm_dir.join("aterm.toml"));
    }
    Ok(cfg)
}

/// 8 bytes of OS randomness for an unpredictable per-run directory name, via
/// `aterm_uds::rand` — the ONE audited entropy surface (getentropy(2) with a
/// bounded device-read fallback), never a hand-rolled device read — falling
/// back to a nanosecond timestamp if the OS CSPRNG is unavailable. Degrading
/// is acceptable HERE because the name is anti-collision, not a secret: the
/// security boundary is the 0700 mode + atomic `create_dir`, not the name.
#[cfg(unix)]
fn rand_u64() -> u64 {
    let mut buf = [0u8; 8];
    if aterm_uds::rand::fill(&mut buf).is_ok() {
        u64::from_ne_bytes(buf)
    } else {
        match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            // Same value as the former `d.as_nanos() as u64`: truncation mod
            // 2^64 distributes over the (secs * 1e9 + subsec_nanos) sum, so
            // u64 wrapping ops are byte-identical — without the u128 -> u64
            // cast the verifier cannot model.
            Ok(d) => d
                .as_secs()
                .wrapping_mul(1_000_000_000)
                .wrapping_add(u64::from(d.subsec_nanos())),
            Err(_) => 0,
        }
    }
}

/// Windows twin: `BCryptGenRandom` via `aterm_uds::rand`, with the same
/// nanosecond-timestamp fallback.
#[cfg(windows)]
fn rand_u64() -> u64 {
    let mut buf = [0u8; 8];
    if aterm_uds::rand::fill(&mut buf).is_ok() {
        u64::from_ne_bytes(buf)
    } else {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
    }
}

fn main() {
    let args = parse_args();
    if !args.gui.is_file() && args.gui.to_string_lossy().contains('/') {
        // `Path::display()` renders exactly like `to_string_lossy`, so this
        // is byte-identical to the old `eprintln!` with the fmt arg.
        let mut msg = String::new();
        msg.push_str("aterm-nest: aterm-gui not found at ");
        msg.push_str(&args.gui.to_string_lossy());
        eprint_line(&msg);
        std::process::exit(1);
    }
    match run(&args) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            let mut msg = String::new();
            msg.push_str("aterm-nest: ");
            msg.push_str(&e.to_string());
            eprint_line(&msg);
            std::process::exit(1);
        }
    }
}

fn run(args: &Args) -> io::Result<i32> {
    let pid = std::process::id();
    // ONE per-run private directory (mode 0700) holds everything this run
    // creates, and is removed on teardown.
    //
    // 1. The deeper levels' stderr files. They are launched by the parent shell
    //    with `... 2>{errfile}`, a redirection that opens+truncates without
    //    `O_EXCL` and follows symlinks, so a predictable name in world-writable
    //    `/tmp` let a different uid pre-position a symlink and divert the nested
    //    gui's stderr (which carries the live control-socket path) into an
    //    arbitrary victim-writable file. No other uid can create entries in — or
    //    read through — a 0700 dir, which is what defeats the symlink-follow.
    // 2. The scratch `XDG_CONFIG_HOME` every level runs against
    //    ([`seed_config_dir`]), seeded with a copy of the caller's aterm.toml.
    //
    // It is created BEFORE the root spawn (2 needs it, and depth==1 needs 2), and
    // failing to create it FAILS THE RUN rather than degrading: without a private
    // dir there is no scratch config, and a level without a scratch config edits
    // the caller's real settings — the exact bug this closes.
    let rundir = make_rundir_in(&rundir_base(), pid)?;
    let cfg = match seed_config_dir(&rundir) {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&rundir);
            return Err(e);
        }
    };
    let (mut root, root_sock) = match spawn_root(&args.gui, &cfg) {
        Ok(r) => r,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&rundir);
            return Err(e);
        }
    };
    let mut msg = String::new();
    msg.push_str("aterm-nest: L0 socket ");
    msg.push_str(&root_sock);
    eprint_line(&msg);
    let mut socks = vec![root_sock];
    for k in 1..args.depth {
        // `aterm-nest-L{k}.err`, rendered without `fmt::Arguments`; k <= 7
        // (depth is capped at 8), so a single masked digit is exact.
        let mut fname = String::new();
        fname.push_str("aterm-nest-L");
        fname.push(DIGITS16[k & 0xf] as char);
        fname.push_str(".err");
        let errfile = rundir.join(fname);
        let errfile = errfile.to_string_lossy();
        // socks holds exactly k entries here (L0..L{k-1}), so k-1 is always
        // in bounds; the guard replaces the panicking index with a clean
        // error on the impossible path (saturating_sub is exact for k >= 1).
        let Some(parent_sock) = socks.get(k.saturating_sub(1)) else {
            return Err(io::Error::other("internal: parent socket missing"));
        };
        let sock = match spawn_child(parent_sock, &args.gui, &errfile, &cfg) {
            Ok(s) => s,
            Err(e) => {
                let _ = root.kill();
                let _ = std::fs::remove_dir_all(&rundir);
                return Err(e);
            }
        };
        let mut msg = String::new();
        msg.push_str("aterm-nest: L");
        push_digit(&mut msg, k);
        msg.push_str(" socket ");
        msg.push_str(&sock);
        eprint_line(&msg);
        socks.push(sock);
    }
    // socks always holds at least the root socket pushed above; the guard
    // replaces an `unwrap` so the impossible path is a clean error, matching
    // the old panic's "root left running" behavior (a panic never killed it).
    let Some(deepest) = socks.last().cloned() else {
        return Err(io::Error::other("internal: no level sockets recorded"));
    };

    // Run the command in the deepest level, fenced by a sentinel so we know when it
    // finished, then capture that level's visible text.
    let cmd = args.cmd.join(" ");
    // "aterm-nest: running in L{depth-1} (depth {depth}): {cmd}", rendered
    // manually; depth is 1..=8 (enforced by parse_args), so both numbers are
    // single digits and saturating_sub is exact.
    let mut msg = String::new();
    msg.push_str("aterm-nest: running in L");
    push_digit(&mut msg, args.depth.saturating_sub(1));
    msg.push_str(" (depth ");
    push_digit(&mut msg, args.depth);
    msg.push_str("): ");
    msg.push_str(&cmd);
    eprint_line(&msg);
    // `{cmd}; echo {DONE}` rendered without `fmt::Arguments`.
    let mut fenced = String::new();
    fenced.push_str(&cmd);
    fenced.push_str("; echo ");
    fenced.push_str(DONE);
    type_line(&deepest, &fenced)?;

    let deadline = Instant::now() + Duration::from_secs(180);
    let mut last = String::new();
    loop {
        std::thread::sleep(Duration::from_millis(500));
        last = verb_stream(&deepest, "text").unwrap_or(last);
        // The sentinel also appears in the COMMAND ECHO (`...; echo __DONE__`), so
        // wait for it on its OWN line — the echo's output — not just anywhere.
        if last.lines().any(|l| l.trim() == DONE) || Instant::now() > deadline {
            break;
        }
    }

    // Print the command's output: the visible lines AFTER the command echo and
    // BEFORE the sentinel.
    print_output(&last, &cmd);

    if args.keep {
        eprintln!("aterm-nest: --keep set; stack left running:");
        for (i, s) in socks.iter().enumerate() {
            // "  L{i}: {s}" — i < socks.len() <= depth <= 8, a single digit.
            let mut msg = String::new();
            msg.push_str("  L");
            push_digit(&mut msg, i);
            msg.push_str(": ");
            msg.push_str(s);
            eprint_line(&msg);
        }
        // The rundir OUTLIVES us under `--keep` and is named so: the levels still
        // running hold it open as their `XDG_CONFIG_HOME` (they re-read it on
        // every config change and write settings edits into it), so removing it
        // here would silently drop them back onto built-in defaults. Whoever
        // kills the kept stack removes this directory.
        let mut msg = String::new();
        msg.push_str("  scratch config (delete with the stack): ");
        msg.push_str(&cfg.to_string_lossy());
        eprint_line(&msg);
    } else {
        let _ = root.kill();
        let _ = root.wait();
        let _ = std::fs::remove_dir_all(&rundir);
    }
    Ok(0)
}

/// The command's output region: the lines strictly between the (last) command echo
/// and the sentinel's own line. Split out (pure) for testing.
fn output_region<'a>(text: &'a str, cmd: &str) -> Vec<&'a str> {
    let lines: Vec<&str> = text.lines().collect();
    let echo = lines.iter().rposition(|l| l.contains(cmd));
    let done = lines.iter().rposition(|l| l.trim() == DONE);
    match (echo, done) {
        (Some(e), Some(d)) if d > e => {
            // `rposition` returns in-bounds indices, so on this path
            // e < d < lines.len() always holds; clamp for the modular
            // verifier, which cannot carry that invariant across the
            // call boundary. Identical behavior on every real input.
            let d = if d < lines.len() { d } else { lines.len() };
            let start = e.saturating_add(1).min(d);
            // start <= d <= lines.len() by the clamps above, so `get` never
            // returns None; the guard just replaces the panicking index with
            // a total lookup the verifier can discharge.
            match lines.get(start..d) {
                Some(seg) => seg.to_vec(),
                None => Vec::new(),
            }
        }
        _ => lines
            .into_iter()
            .filter(|l| !l.trim().is_empty() && !l.contains(DONE))
            .collect(),
    }
}

/// Behavior-identical stand-in for `eprintln!` with formatted arguments
/// (whose `fmt::Arguments` machinery the Trust verifier cannot lower): write
/// the pre-rendered line plus `'\n'` to stderr. On failure, `eprintln!`
/// panics — which here (panic=unwind, no hooks/catch_unwind, no
/// cleanup-relevant Drops, and a broken stderr that can't show the message)
/// is observably `exit(101)`; we take that exit directly, since a literal
/// `panic!` would be an L0 refutation in our own code.
fn eprint_line(msg: &str) {
    let mut buf = msg.to_string().into_bytes();
    buf.push(b'\n');
    if io::stderr().write_all(&buf).is_err() {
        std::process::exit(101);
    }
}

/// Print the command's output region from the captured screen text: lines strictly
/// between the (last) command echo and the sentinel, with the sentinel/echo removed.
fn print_output(text: &str, cmd: &str) {
    let out = io::stdout();
    let mut w = out.lock();
    for l in output_region(text, cmd) {
        // Same bytes `writeln!(w, "{}", l.trim_end())` produced, with the
        // write error ignored exactly as the old `let _ = writeln!` did —
        // just without the unlowerable `fmt::Arguments` machinery.
        let mut line = l.trim_end().to_string().into_bytes();
        line.push(b'\n');
        let _ = w.write_all(&line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_from_line_extracts_path() {
        let line = "aterm-gui: control socket listening at /tmp/x/aterm-42.sock (token-gated, same-uid only)";
        assert_eq!(
            socket_from_line(line).as_deref(),
            Some("/tmp/x/aterm-42.sock")
        );
        assert_eq!(socket_from_line("aterm-gui: GPU rendering on Metal"), None);
    }

    #[test]
    fn read_token_path_is_sibling() {
        // (pure path derivation; the file need not exist)
        let tok = "/d/aterm-7.sock"
            .strip_suffix(".sock")
            .map(|b| b.to_string() + ".token");
        assert_eq!(tok.as_deref(), Some("/d/aterm-7.token"));
    }

    #[test]
    fn output_region_is_between_echo_and_sentinel() {
        // The command echo also contains the sentinel (we typed `cmd; echo DONE`);
        // the sentinel's OWN line (== DONE) fences the end.
        let cmd = "echo hi";
        let screen = format!("user% {cmd}; echo {DONE}\nhi\n{DONE}\nuser% \n");
        assert_eq!(output_region(&screen, cmd), vec!["hi"]);
    }

    #[test]
    fn output_region_fallback_when_unfenced() {
        // No proper fence -> non-empty, non-sentinel lines (never lose output).
        let got = output_region("alpha\n\nbeta\n", "missing-cmd");
        assert_eq!(got, vec!["alpha", "beta"]);
    }

    #[test]
    fn sh_quote_round_trips_spaces_and_single_quotes() {
        // The deeper-level launch line interpolates the gui/errfile paths into a
        // command run by the parent shell. A path with a space AND an embedded
        // single quote must survive that shell's word-splitting/quote-processing
        // intact, otherwise depth>=2 nesting silently breaks.
        let nasty = "/path with space/it's a 'gui'";
        let script = format!("printf %s {}", sh_quote(nasty));
        let out = Command::new("sh")
            .arg("-c")
            .arg(&script)
            .output()
            .expect("run sh -c");
        assert!(out.status.success(), "sh -c failed: {script}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            nasty,
            "sh_quote did not round-trip through sh -c"
        );
    }

    #[test]
    fn manual_renderers_match_format() {
        // The fmt-free renderers (introduced so the Trust verifier can lower
        // these functions) must be byte-identical to the `format!` shapes
        // they replaced.
        for v in [
            0u32,
            1,
            7,
            9,
            10,
            42,
            99,
            100,
            101,
            12345,
            1_000_000_000,
            u32::MAX,
        ] {
            let mut s = String::new();
            push_dec_u32(&mut s, v);
            assert_eq!(s, format!("{v}"));
        }
        for x in [0u64, 1, 0xdead_beef, 0x0123_4567_89ab_cdef, u64::MAX] {
            let mut s = String::new();
            push_hex16(&mut s, x);
            assert_eq!(s, format!("{x:016x}"));
        }
        for k in 0usize..=9 {
            let mut s = String::new();
            push_digit(&mut s, k);
            assert_eq!(s, format!("{k}"));
        }
        for s in ["", "plain", "it's", "a b'c'd", "'''", "ünïcodé 'q'"] {
            assert_eq!(sh_quote(s), format!("'{}'", s.replace('\'', "'\\''")));
        }
        assert_eq!(
            String::from_utf8(auth_line_bytes("tok123")).unwrap(),
            format!("AUTH {}\n", "tok123")
        );
        assert_eq!(
            String::from_utf8(verb_line_bytes("text")).unwrap(),
            format!("{}\n", "text")
        );
    }

    /// The whole point of the isolation: the levels open a COPY, so a settings
    /// write inside the stack cannot reach the caller's real `aterm.toml`. (A
    /// probe run DID reach it on 2026-08-10 and changed the owner's font.)
    #[test]
    fn scratch_config_is_a_copy_the_stack_can_write_without_touching_the_original() {
        let rundir = make_rundir_in(&std::env::temp_dir(), std::process::id())
            .expect("create private rundir");
        let real = rundir.join("real-aterm.toml");
        std::fs::write(&real, "cursor_trail_style = \"rainbow kitty pet\"\n")
            .expect("write source");

        let cfg = seed_config_dir_from(&rundir, Some(&real)).expect("seed scratch config");

        // 1. It is the path `config_path()` resolves from `XDG_CONFIG_HOME=cfg`,
        //    and it carries the caller's settings verbatim — the nested stack
        //    renders like the caller's terminal, not like stock defaults.
        let copy = cfg.join("aterm").join("aterm.toml");
        assert!(
            copy.is_file(),
            "expected a seeded copy at {}",
            copy.display()
        );
        assert_eq!(
            std::fs::read_to_string(&copy).unwrap(),
            std::fs::read_to_string(&real).unwrap(),
            "the scratch config must be a faithful copy"
        );
        // 2. It is a COPY, not a link/alias: the exact write that escaped last
        //    time lands here and leaves the original byte-identical.
        std::fs::write(&copy, "game_font = \"minecraft\"\n").expect("write copy");
        assert_eq!(
            std::fs::read_to_string(&real).unwrap(),
            "cursor_trail_style = \"rainbow kitty pet\"\n",
            "a write inside the stack must not reach the caller's aterm.toml"
        );
        let _ = std::fs::remove_dir_all(&rundir);
    }

    /// Isolation must not depend on the copy succeeding: with no source (fresh
    /// machine, or an unreadable file) the scratch dir still exists, so the
    /// levels resolve into it and run on built-in defaults.
    #[test]
    fn scratch_config_exists_even_when_there_is_nothing_to_copy() {
        let rundir = make_rundir_in(&std::env::temp_dir(), std::process::id())
            .expect("create private rundir");
        let missing = rundir.join("nope").join("aterm.toml");
        let cfg = seed_config_dir_from(&rundir, Some(&missing)).expect("seed scratch config");
        assert!(cfg.join("aterm").is_dir(), "scratch config dir must exist");
        assert!(
            !cfg.join("aterm").join("aterm.toml").exists(),
            "no source means no copy, not a failure"
        );
        assert_eq!(
            seed_config_dir_from(&rundir, None).ok(),
            Some(cfg.clone()),
            "an unresolvable source is not an error either"
        );
        let _ = std::fs::remove_dir_all(&rundir);
    }

    /// Deeper levels are launched by TYPING at the level above, so the isolation
    /// has to survive as shell text. It is re-stated on the line (not merely
    /// inherited) because the failure mode of losing it is silent.
    #[test]
    fn launch_line_carries_the_scratch_config_quoted() {
        let line = launch_line(
            Path::new("/opt/my gui/aterm-gui"),
            "/run/x/aterm-nest-L1.err",
            Path::new("/run/x/cfg dir"),
        );
        assert_eq!(
            line,
            "XDG_CONFIG_HOME='/run/x/cfg dir' ATERM_LINES=40 ATERM_COLUMNS=120 \
             '/opt/my gui/aterm-gui' --headless 2>'/run/x/aterm-nest-L1.err'"
        );
        // The env assignment must PRECEDE the binary, or `sh` reads it as an
        // argument and the level below inherits the caller's config after all.
        let assign = line.find("XDG_CONFIG_HOME=").expect("assignment present");
        let bin = line.find("aterm-gui").expect("binary present");
        assert!(assign < bin, "the assignment must prefix the command");
    }

    #[test]
    #[cfg(unix)]
    fn rundir_is_private_0700_and_unpredictable() {
        // The errfiles must live in a per-run 0700 directory, NOT under a
        // predictable world-writable /tmp name a different uid can pre-symlink.
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir();
        let pid = std::process::id();
        let dir = make_rundir_in(&base, pid).expect("create private rundir");
        assert!(dir.is_dir(), "rundir should be a directory");
        // mkdir applies 0700 atomically — no window where it is world-writable
        // and no other uid can read the control-socket path through it.
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "rundir must be mode 0700, got {mode:o}");
        // The name is unpredictable, not the old fixed aterm-nest-<pid>-L{k}.err.
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with(&format!("aterm-nest-{pid}-")),
            "got {name}"
        );
        assert_ne!(name, format!("aterm-nest-{pid}-L1.err"));
        // Distinct per run (random component), so two stacks never collide.
        let dir2 = make_rundir_in(&base, pid).expect("create second rundir");
        assert_ne!(dir, dir2, "rundirs must be unique per run");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    /// Windows twin (no POSIX mode bits — the `%TEMP%` per-user ACL is the
    /// boundary): the rundir is created, unpredictably named, and unique.
    #[test]
    #[cfg(windows)]
    fn rundir_is_created_unpredictable_and_unique() {
        let base = std::env::temp_dir();
        let pid = std::process::id();
        let dir = make_rundir_in(&base, pid).expect("create private rundir");
        assert!(dir.is_dir(), "rundir should be a directory");
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with(&format!("aterm-nest-{pid}-")),
            "got {name}"
        );
        assert_ne!(name, format!("aterm-nest-{pid}-L1.err"));
        let dir2 = make_rundir_in(&base, pid).expect("create second rundir");
        assert_ne!(dir, dir2, "rundirs must be unique per run");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }
}
