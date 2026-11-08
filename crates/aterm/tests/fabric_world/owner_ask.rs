// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE OWNER'S ASK, END TO END (session log Phase 6 step 6I-4, ruling U33).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_link::ctl::{Ctl, Reply};
use aterm_uds::spawnfd::{Child as OwnedChild, Stdio as OwnedStdio};

use super::harness::{
    self, DEADLINE, FIXTURE_GUI_CONFIG, FLEET, SECRET, World, cap_line, connect, connect_within,
    kv, until, write_cap,
};

/// One private fabric: the production broker binary (`aterm link broker`, with the default rules
/// its one constructor sets) in a scratch state dir of its own, and the instances on it.
struct Owner {
    tmp: PathBuf,
    sock: String,
    broker: Option<harness::BrokerChild>,
    scratch: Option<aterm_tempfile::TempDir>,
}

impl Drop for Owner {
    fn drop(&mut self) {
        if let Some(mut b) = self.broker.take() {
            aterm_uds::deadline::kill_within(&mut b.child, DEADLINE);
        }
        if let Some(dir) = self.scratch.take()
            && (std::thread::panicking() || std::env::var_os("ATERM_LINK_KEEP").is_some())
        {
            eprintln!("KEPT owner world {}", dir.keep().display());
        }
    }
}

impl Owner {
    fn boot() -> Self {
        Self::boot_at_rest(false)
    }

    /// [`Owner::boot`], its broker's log sealed at rest when `at_rest`: a `rest.key` beside it, what
    /// `aterm fabric on` leaves.
    fn boot_at_rest(at_rest: bool) -> Self {
        let scratch = aterm_tempfile::Builder::new()
            .prefix("oa-")
            .tempdir_in("/tmp")
            .expect("exclusive world scratch");
        let tmp = scratch.path().to_path_buf();
        let secret = tmp.join("mint.secret");
        std::fs::write(&secret, SECRET).expect("the mint secret");
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600))
                .expect("chmod 600");
        }
        let state = tmp.join("broker");
        std::fs::create_dir_all(&state).expect("the broker's state dir");
        if at_rest {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))
                .expect("chmod 700");
            aterm_uds::private_file::write_private(
                &state,
                aterm_link::at_rest::REST_KEY,
                &harness::REST_KEY,
            )
            .expect("the at-rest key");
        }
        let sock = state.join("b.sock").to_string_lossy().into_owned();
        let log = state.join("b.log").to_string_lossy().into_owned();
        let secret_arg = secret.to_string_lossy().into_owned();
        let err = std::fs::File::create(state.join("stderr.log")).expect("broker stderr");
        let mut broker = harness::spawn_broker(
            &state,
            &[&sock, &log, "--secret-file", &secret_arg],
            |cmd| {
                cmd.stdout(OwnedStdio::piped()).stderr(err);
            },
        );
        let first = harness::first_line_within(&mut broker.child, "the broker's readiness line");
        assert_eq!(first.trim_end(), format!("listening {sock}"));
        Self {
            tmp,
            sock,
            broker: Some(broker),
            scratch: Some(scratch),
        }
    }

    fn root_client(&self) -> astream_broker::Client {
        let mut c = harness::broker_client(&self.sock);
        let root = astream_cap::mint(SECRET, &format!("/f/{FLEET}/>")).expect("mint the root");
        c.attach(&root.filter, &root.tag).expect("attach the root");
        c
    }
}

/// What one instance is booted with beyond the default config.
#[derive(Clone, Default)]
struct Opts {
    /// Top-level keys, before any table.
    top: String,
    /// Lines inside the `[fabric]` table after its `command` (`log = "off"`).
    fabric: String,
    /// Tables appended to the fixture config (a `[harness]` table).
    config: String,
    /// Extra environment.
    env: Vec<(String, String)>,
    /// The shell its tabs start.
    shell: Option<String>,
    /// A PATH for its tabs, first.
    path_first: Option<PathBuf>,
}

/// One private instance on an [`Owner`] fabric: its own HOME, `XDG_CONFIG_HOME`,
/// `XDG_RUNTIME_DIR` and `ATERM_STATE_HOME`, a private `--control-sock`, `USER` and `LOGNAME`,
/// the lab desk seam (`ATERM_DEBUG_HEADLESS_RESTORE=1`), and its [`fake_path`] named as the one
/// stand-in agent its harness may type into.
struct Inst {
    name: String,
    root: PathBuf,
    node: String,
    sock: String,
    token: String,
    log: PathBuf,
    opts: Opts,
    gui: Option<OwnedChild>,
    lifeline: Option<aterm_uds::lifeline::Lifeline>,
    launcher: harness::Launcher,
}

impl Drop for Inst {
    fn drop(&mut self) {
        self.kill();
    }
}

impl Inst {
    fn boot(owner: &Owner, name: &str, node: &str, opts: Opts) -> Self {
        let root = owner.tmp.join(name);
        for sub in [
            "home",
            "cfg/aterm",
            "run/aterm",
            "astate",
            "link",
            "tmp",
            "bin",
        ] {
            std::fs::create_dir_all(root.join(sub)).expect("a scratch dir");
        }
        {
            use std::os::unix::fs::PermissionsExt as _;
            // The state root as the deployment makes it (0700), whatever the umask: Ubuntu's 002
            // leaves a made folder group-writable.
            for dir in [root.clone(), root.join("astate")] {
                std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
                    .expect("chmod 700");
            }
        }
        std::fs::write(root.join("link/node"), format!("{node}\n")).expect("the node id");
        let cap = root.join("node.cap");
        let caps: String = astream_cap::GrantCeiling::aterm_node_ring(FLEET, node)
            .expect("a node ring")
            .rules()
            .iter()
            .map(|g| cap_line(g))
            .collect();
        write_cap(&cap, caps);
        // The bridge command `aterm fabric on` writes (`enable::bridge_command`): this node's own
        // sessions' tasks arrive undemoted.
        let fabric_cmd = format!(
            "{} link serve --fleet {FLEET} --broker {} --cap-file {} --state {} --accept-from {node}",
            harness::aterm_to_launch(),
            owner.sock,
            cap.display(),
            root.join("link").display()
        );
        aterm_uds::atomic_file::write(
            &root.join("cfg/aterm/aterm.toml"),
            format!(
                "tab_strip_rows = 1\n{}{FIXTURE_GUI_CONFIG}\n[fabric]\ncommand = \"{fabric_cmd}\"\n{}\n{}",
                opts.top, opts.fabric, opts.config
            )
            .as_bytes(),
            0o600,
        )
        .expect("the config");
        if let Some(bin) = &opts.path_first {
            pin_login_path(&root.join("home"), bin);
        }
        let sock = root
            .join("run/aterm/aterm.sock")
            .to_string_lossy()
            .into_owned();
        let mut me = Self {
            name: name.to_string(),
            log: root.join("gui.log"),
            root,
            node: node.to_string(),
            sock,
            token: String::new(),
            opts,
            gui: None,
            lifeline: None,
            launcher: harness::Launcher::default(),
        };
        me.launch();
        me
    }

    fn launch(&mut self) {
        let out = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.log)
            .expect("the instance log");
        let err = out.try_clone().expect("the log, twice");
        let mut cmd = aterm_uds::spawnfd::command(harness::aterm_to_launch());
        for (name, _) in std::env::vars_os() {
            let atpkg = name.to_string_lossy().starts_with("ATPKG_");
            if aterm_uds::process::stripped_from_child(&name) || atpkg {
                cmd.env_remove(&name);
            }
        }
        let path = self.opts.path_first.as_ref().map_or_else(
            || "/usr/bin:/bin:/usr/sbin:/sbin".to_string(),
            |first| format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", first.display()),
        );
        cmd.arg("--headless")
            .arg("--no-reroute")
            .arg("--control-sock")
            .arg(&self.sock)
            .args(["--lines", "40", "--columns", "200"])
            .env_remove("TERM_PROGRAM")
            .env_remove("BASH_ENV")
            .env_remove("ENV")
            .env_remove("ZDOTDIR")
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("cfg"))
            .env("XDG_RUNTIME_DIR", self.root.join("run"))
            .env("ATERM_STATE_HOME", self.root.join("astate"))
            .env("TMPDIR", self.root.join("tmp"))
            .env("USER", user())
            .env("LOGNAME", user())
            .env("PATH", path)
            .env(
                "SHELL",
                self.opts.shell.clone().unwrap_or_else(|| "/bin/sh".into()),
            )
            .env("ATERM_DEBUG_HEADLESS_RESTORE", "1")
            .env(
                aterm_agent::harness::agent_image::STANDINS_SEAM,
                fake_path_in(&self.root),
            )
            .env("ATERM_DEBUG_SYSTEM_PRESSURE", "none")
            .env(aterm_types::domain::ENV_DEBUG_HOST_SLEEP, "none")
            .env("ATERM_LOG", "info")
            .env(
                harness::STALE_HORIZON.0,
                harness::STALE_HORIZON.1.as_millis().to_string(),
            )
            .env(
                harness::LIVE_WAIT.0,
                harness::LIVE_WAIT.1.as_millis().to_string(),
            )
            .env(harness::FAST_TIMERS.0, harness::FAST_TIMERS.1)
            .stdin(OwnedStdio::null())
            .stdout(out)
            .stderr(err);
        for (k, v) in &self.opts.env {
            cmd.env(k, v);
        }
        self.lifeline =
            Some(aterm_uds::lifeline::Lifeline::arm(&mut cmd, &self.root).expect("the lifeline"));
        let gui = self
            .gui
            .insert(cmd.spawn().expect("launch aterm --headless"));
        self.token = World::wait_for_owned_token_of(&self.sock, gui, &self.log);
        self.launcher = harness::launcher_of(gui.id(), &self.root.join("link"));
    }

    fn pid(&self) -> u32 {
        self.gui.as_ref().expect("a running instance").id()
    }

    fn ctl(&self) -> Ctl {
        connect(&self.sock, &self.token).expect("connect to aterm")
    }

    fn verb(&self, line: &str) -> Reply {
        self.ctl()
            .request(line)
            .unwrap_or_else(|e| panic!("{}: {line}: {e}", self.name))
    }

    fn text(&self, line: &str) -> String {
        let r = self.verb(line);
        format!("{}\n{}", r.header(), r.rows().join("\n"))
    }

    fn wait_ready(&self, owner: &Owner) {
        until("aterm to report fabric=connected", || {
            self.verb("status")
                .header()
                .contains("fabric=connected")
                .then_some(())
        });
        let subject = format!("/f/{FLEET}/pub/{}/node/presence", self.node);
        let mut c = owner.root_client();
        until("the node's presence row to say live", || {
            let page = c.last(&subject, "", 8).ok()?;
            page.0
                .iter()
                .any(|(_, s, b)| *s == subject && String::from_utf8_lossy(b).contains("state=live"))
                .then_some(())
        });
    }

    fn kill(&mut self) {
        if let Some(mut gui) = self.gui.take() {
            let _ = gui.kill();
            let _ = gui.wait();
        }
        self.lifeline.take();
        harness::end_bridge(&self.root.join("link"), self.launcher);
    }
}

/// Pins `bin` first on the PATH the scratch home's login shells start from, after the system
/// profile's (as `launch_isolation::pin_login_path` does for the suite's instances).
fn pin_login_path(home: &Path, bin: &Path) {
    let pin = format!(
        "PATH='{}:/usr/bin:/bin:/usr/sbin:/sbin'\nexport PATH\n",
        bin.display()
    );
    for name in [".zprofile", ".profile", ".bash_profile"] {
        std::fs::write(home.join(name), &pin).expect("a login profile");
    }
}

fn user() -> String {
    std::env::var("USER").unwrap_or_else(|_| "jsmith".into())
}

const NODE_A: &str = "n-00000000000000a1";
const NODE_B: &str = "n-00000000000000b1";

/// Type `line` into `sid` byte for byte (a `feed-bin` frame), then Enter.
fn type_line(inst: &Inst, sid: &str, line: &str) {
    let r = inst
        .ctl()
        .request_with_body(&format!("@{sid} feed-bin {}", line.len()), line.as_bytes())
        .unwrap_or_else(|e| panic!("feed-bin: {e}"));
    assert!(r.ok(), "feed-bin: {}", r.header());
    let r = inst.verb(&format!("@{sid} key enter"));
    assert!(r.ok(), "key enter: {}", r.header());
}

fn sids(inst: &Inst) -> Vec<String> {
    inst.verb("sessions")
        .rows()
        .iter()
        .filter_map(|r| r.split_whitespace().nth(1).map(str::to_string))
        .collect()
}

/// A script `name` under `root` that writes one whole shell-integration block in one write, A to
/// D;0, its command line `line` (OSC 633;E, a `;` spelled `\\x3b` as the integrations escape it,
/// no other backslash) and its output `ran-42`, each mark signed with the tab's nonce when the
/// shell was handed one. Nothing runs `line`: the shell's marks report it, as a shell runs it.
/// The session's shell sources it (`.`), so its block runs no program: a script run by `sh`
/// would have run `sh` (and the shell `sh` becomes) just before its C, which a gather that read
/// that `exec` only while the PTY held the write's C and not yet its D would place in the block.
fn marked_command(root: &Path, name: &str, line: &str) -> PathBuf {
    let script = format!(
        "#!/bin/sh\n\
         n=\"${{ATERM_SHELL_NONCE:+;id=$ATERM_SHELL_NONCE}}\"\n\
         printf '\\033]133;A%s\\007$ \\033]133;B%s\\007\\033]633;E;{line}%s\\007\\r\\n\\033]133;C%s\\007ran-%s\\r\\n\\033]133;D;0%s\\007' \
         \"$n\" \"$n\" \"$n\" \"$n\" \"$((40+2))\" \"$n\"\n"
    );
    let path = root.join(name);
    std::fs::write(&path, script).expect("write the marked command");
    path
}

/// [`marked_command`]'s block, its run real: after C the script `exec`s `/usr/bin/env`, which
/// `exec`s `/bin/sh` to write the output and D;0. The kernel merges a process's `exec` notes, so
/// the block hears whichever image the process holds when the gather reads it, or none once the
/// process is gone: `env`, `sh` (macOS's `/bin/sh` stub, by its own file's name), or the shell
/// `sh` becomes ([`sh_hands_off_to`]), which also runs the script itself, read in the block when
/// its `exec` is read only after C. A detail that names `env` names a program the kernel saw
/// run, while `line`'s own program never runs.
fn ran_command(root: &Path, name: &str, line: &str) -> PathBuf {
    let script = format!(
        "#!/bin/sh\n\
         n=\"${{ATERM_SHELL_NONCE:+;id=$ATERM_SHELL_NONCE}}\"\n\
         printf '\\033]133;A%s\\007$ \\033]133;B%s\\007\\033]633;E;{line}%s\\007\\r\\n\\033]133;C%s\\007' \
         \"$n\" \"$n\" \"$n\" \"$n\"\n\
         exec /usr/bin/env /bin/sh -c 'printf \"ran-%s\\r\\n\\033]133;D;0%s\\007\" \"$((40+2))\" \"$1\"' sh \"$n\"\n"
    );
    let path = root.join(name);
    std::fs::write(&path, script).expect("write the run command");
    path
}

/// The file names of the shell a `/bin/sh` run may become, as a stand-in shows a file of one
/// name (its canonical file's name): on macOS the shell `/private/var/select/sh` links to, which
/// the `/bin/sh` stub re-execs in its own process (bash by default), or, when that link names
/// none of the shells the stub runs, each of them, as sh(1) says the stub then uses one;
/// elsewhere `/bin/sh`'s own file.
fn sh_hands_off_to() -> Vec<String> {
    let name = |path: &str| {
        std::fs::canonicalize(path)
            .ok()
            .and_then(|file| file.file_name().map(|n| n.to_string_lossy().into_owned()))
    };
    if cfg!(target_vendor = "apple") {
        const STUB_RUNS: [&str; 3] = ["bash", "dash", "zsh"];
        return name("/private/var/select/sh")
            .filter(|shell| STUB_RUNS.contains(&shell.as_str()))
            .map_or_else(|| STUB_RUNS.map(String::from).to_vec(), |shell| vec![shell]);
    }
    vec![name("/bin/sh").expect("the file `/bin/sh` runs")]
}

/// [`marked_command`]'s block, the script sourced (`.`) so the session's shell writes its marks and
/// then starts `chain` as the block's job, as a shell's integration writes C before it starts a
/// job; `chain`'s last program is a shell that sleeps a second in a child and writes the output
/// and D;0: the session's process watch hears each of the job's own images, all after the block's
/// C, however late its drain. (A job that wrote C itself would have run the typed `sh`, and the
/// `bash` macOS's `sh` runs, before it: a drain that heard that `exec` after C would place it in
/// the block.)
fn exec_in_block(root: &Path, name: &str, line: &str, chain: &str) -> PathBuf {
    let script = format!(
        "#!/bin/sh\n\
         n=\"${{ATERM_SHELL_NONCE:+;id=$ATERM_SHELL_NONCE}}\"\n\
         printf '\\033]133;A%s\\007$ \\033]133;B%s\\007\\033]633;E;{line}%s\\007\\r\\n\\033]133;C%s\\007' \
         \"$n\" \"$n\" \"$n\" \"$n\"\n\
         {chain} -c '/bin/sleep 1; printf \"ran-%s\\r\\n\\033]133;D;0%s\\007\" \"$((40+2))\" \"$1\"' qz \"$n\"\n"
    );
    let path = root.join(name);
    std::fs::write(&path, script).expect("write the block's chain");
    path
}

/// How a privacy plant's block runs: its marks alone, the session's shell writing them
/// ([`marked_command`]), its `env` chain ([`ran_command`]), or a chain the shell starts as its
/// job ([`exec_in_block`]). A program a child of the job forks is heard by no watch (it hears the
/// shell's children and their own `exec`s), so a plant whose programs ran there could not fail
/// for a detail that confirms by what they ran.
enum PlantRun {
    Marks,
    Env,
    Execs(String),
}

/// Each `cmd` run of a `log kinds=cmd` read, in the order the runs opened: the `detail=` values
/// its open and its amends wrote, in order.
fn cmd_details(cmds: &str) -> Vec<Vec<String>> {
    let mut runs: Vec<(String, Vec<String>)> = Vec::new();
    for line in cmds.lines() {
        let mut words = line.split(' ');
        let (Some(_), Some(op), Some(run), Some("cmd")) =
            (words.next(), words.next(), words.next(), words.next())
        else {
            continue;
        };
        let detail = line
            .split(' ')
            .find_map(|w| w.strip_prefix("detail="))
            .map(str::to_owned);
        match op {
            "open" => runs.push((run.to_owned(), detail.into_iter().collect())),
            "amend" => {
                if let (Some((_, details)), Some(d)) =
                    (runs.iter_mut().find(|(r, _)| r == run), detail)
                {
                    details.push(d);
                }
            }
            _ => {}
        }
    }
    runs.into_iter().map(|(_, details)| details).collect()
}

const BOX_TOUCH: &str =
    include_str!("../../../aterm-phase/src/fixtures/claude-2.1.280-box-bash-touch.txt");
const SPINNER: &str = include_str!("../../../aterm-phase/src/fixtures/spinner-tempering.txt");
const WALL_SHELL: &str = include_str!(
    "../../../aterm-phase/src/fixtures/claude-2.1.283-wall-weekly-goal-paused-shell-measured.txt"
);
const AFTER_TURN_283: &str =
    include_str!("../../../aterm-phase/src/fixtures/claude-2.1.283-footer-after-turn.txt");
const OFFER: &str =
    include_str!("../../../aterm-phase/src/fixtures/hand-built-offer-end-of-turn.txt");
const ASK_ONE: &str = include_str!("../../../aterm-phase/src/fixtures/claude-2.1.281-ask-one.txt");

/// A fixture's screen: the text after its provenance line.
fn body(fixture: &str) -> String {
    fixture
        .split_once('\n')
        .map_or(fixture, |(_, rest)| rest)
        .to_string()
}

/// The stand-in Claude Code (after `host_live`'s): it draws each scene file it is given in turn,
/// each a whole frame one `cat` writes ([`frame`]).
/// After a `*.gate` scene it waits for `$log.<n>` (the scene's ordinal, from 1), which the test
/// makes; after `wall.type` it echoes the resume text and waits for Enter, as the real composer
/// does; after another `*.type` scene ONE bounded read takes what was typed, logged `text:`; after a
/// `*.drain` scene it reads until half a second passes with nothing to read, logged `drained`;
/// after any other scene it reads ONE key into the key log (`$1`), as `od -c` spells it, a space
/// as `space`. It first clears what the shell printed between its fork and this exec, then writes a
/// cell and erases it in place, as a real Claude Code moves the screen as it starts.
const FAKE: &str = "#!/bin/sh\n\
     log=$1; shift\n\
     printf '\\033[2J\\033[H'\n\
     printf 'x\\b \\b'\n\
     old=$(stty -g)\n\
     stty -icanon -echo min 1\n\
     i=0\n\
     for scene in \"$@\"; do\n\
     \x20 i=$((i+1))\n\
     \x20 cat \"$scene\"\n\
     \x20 case \"$scene\" in\n\
     \x20 *.gate) while [ ! -e \"$log.$i\" ]; do sleep 0.1; done; continue ;;\n\
     \x20 */wall.type) t=$(dd bs=4096 count=1 2>/dev/null); printf 'text:%s\\n' \"$t\" >> \"$log\"; printf '\\033[0m\\033[K%s' \"$t\"; dd bs=1 count=1 >/dev/null 2>&1; continue ;;\n\
     \x20 *.type) t=$(dd bs=4096 count=1 2>/dev/null); printf 'text:%s\\n' \"$t\" >> \"$log\"; continue ;;\n\
     \x20 *.drain) stty min 0 time 5; while [ -n \"$(dd bs=65536 count=1 2>/dev/null)\" ]; do :; done; stty min 1 time 0; echo drained >> \"$log\"; continue ;;\n\
     \x20 esac\n\
     \x20 while :; do\n\
     \x20   k=$(dd bs=1 count=1 2>\"$log.dd\" | od -An -c | tr -d ' ')\n\
     \x20   grep -q '^1+0 records in' \"$log.dd\" && break\n\
     \x20   sleep 0.05\n\
     \x20 done\n\
     \x20 [ -z \"$k\" ] && k=space\n\
     \x20 case \"$k\" in 033) k=\"$k$(dd bs=1 count=2 2>/dev/null | od -An -c | tr -d ' ')\" ;; esac\n\
     \x20 printf '%s\\n' \"$k\" >> \"$log\"\n\
     done\n\
     stty \"$old\"\n\
     printf '\\033[2J\\033[H'\n";

/// Where the stand-in lives: the managed store's `agents/claude` under the instance's home, as
/// `host_live`'s does.
fn fake_path(inst: &Inst) -> PathBuf {
    fake_path_in(&inst.root)
}

/// [`fake_path`] for the instance whose scratch is `root`: what every [`Inst::launch`] names
/// through [`aterm_agent::harness::agent_image::STANDINS_SEAM`], since a debug build's harness
/// types into an agent only when it confirms the agent's image, and a script's identity is all
/// that confirms the stand-in (`/bin/sh <path>`); an unnamed one is only observed, its box raised
/// and nothing typed.
fn fake_path_in(root: &Path) -> PathBuf {
    atpkg::store::default_prefix(&root.join("home")).join("agents/claude")
}

/// A scene as the stand-in draws it ([`FAKE`]): the screen cleared, the scene's rows, and last the
/// first two cells of its last row that opens with `❯`, the prompt, reached by moving up from the
/// frame's end (on the prompt's own row in a scene taller than the screen too), then the cursor on
/// row `n` column 3, `n` the prompt's line in the scene: its caret, where Claude Code leaves it,
/// in a scene the screen holds. `hint` draws its composer text dim, as the measured wall's
/// suggestion is: plain text is a person's draft, even with the cursor home. A partial frame
/// lacks the prompt's `❯`; the final bytes place it and its cursor together, so a grey suggestion
/// with the cursor still away does not leave the harness waiting on an unknown composer.
fn frame(scene: &str, hint: bool) -> String {
    let rows: Vec<&str> = scene.split_inclusive('\n').collect();
    let caret = rows.iter().rposition(|row| row.starts_with('❯'));
    let mut out = String::from("\x1b[2J\x1b[H");
    let mut tail = String::new();
    for (i, row) in rows.iter().enumerate() {
        if Some(i) != caret {
            out.push_str(row);
            continue;
        }
        let head: String = row.chars().take_while(|c| *c != '\n').take(2).collect();
        out.push_str(&format!("\x1b[{}C", head.chars().count()));
        if hint {
            out.push_str("\x1b[2m");
        }
        out.push_str(&row[head.len()..]);
        if hint {
            out.push_str("\x1b[22m");
        }
        let up = rows[i..].iter().filter(|r| r.ends_with('\n')).count();
        let up = if up == 0 {
            String::new()
        } else {
            format!("\x1b[{up}A")
        };
        tail = format!("\r{up}{head}\x1b[{};3H", i + 1);
    }
    out + &tail
}

/// Writes the stand-in and its scenes, each as its [`frame`], into `inst`'s scratch (`sc/`,
/// `work/`).
fn write_fake(inst: &Inst, wall: &str) {
    let fake = fake_path(inst);
    let sc = inst.root.join("sc");
    for d in [
        fake.parent().expect("agents/"),
        &sc,
        &inst.root.join("work"),
    ] {
        std::fs::create_dir_all(d).expect("mkdir");
    }
    std::fs::write(&fake, FAKE).expect("the stand-in");
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    aterm_spec::wait::first_exec(&fake, &["/dev/null"]);
    let idle = aterm_agent::supervise::prompt::fixtures::screen(AFTER_TURN_283).join("\n") + "\n";
    let wall_screen = body(WALL_SHELL).replace("resets 4pm (America/Los_Angeles)", wall);
    for (name, text) in [
        ("box.key", body(BOX_TOUCH)),
        ("busy.gate", body(SPINNER)),
        ("busy2.gate", body(SPINNER)),
        ("idle.key", idle.clone()),
        ("idle.gate", idle.clone()),
        ("idle2.gate", idle.clone()),
        ("idle3.gate", idle.clone()),
        ("idle.drain", idle.clone()),
        ("idle.type", idle),
        ("box.gate", body(BOX_TOUCH)),
        ("busy3.gate", body(SPINNER)),
        ("wall.type", wall_screen),
    ] {
        std::fs::write(sc.join(name), frame(&text, name == "wall.type")).expect("a scene");
    }
}

/// Starts the stand-in in `sid` on `scenes`, its key log `keys`.
fn start_fake(inst: &Inst, sid: &str, keys: &str, scenes: &[&str]) {
    let root = inst.root.display();
    let scenes: Vec<String> = scenes.iter().map(|s| format!("{root}/sc/{s}")).collect();
    let line = format!(
        "clear; cd {root}/work; printf '\\033]7;file://localhost%s\\007' \"$PWD\"; '{}' {root}/{keys} {}",
        fake_path(inst).display(),
        scenes.join(" ")
    );
    type_line(inst, sid, &line);
}

/// Lets the stand-in past its `n`th scene (a `*.gate`).
fn release(inst: &Inst, keys: &str, n: usize) {
    std::fs::write(inst.root.join(format!("{keys}.{n}")), b"").expect("release a gate");
}

/// What the stand-in read, one entry a line.
fn keys_read(inst: &Inst, keys: &str) -> Vec<String> {
    std::fs::read_to_string(inst.root.join(keys))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn spawn_tab(inst: &Inst) -> String {
    let r = inst.verb("spawn");
    assert!(r.ok(), "spawn: {}", r.header());
    r.header()
        .split_whitespace()
        .find(|w| w.starts_with("s-"))
        .unwrap_or_else(|| panic!("spawn names its sid: {}", r.header()))
        .to_string()
}

fn status_of(inst: &Inst, sid: &str) -> String {
    inst.verb(&format!("@{sid} status")).header().to_string()
}

fn cursor(inst: &Inst, sid: &str) -> (String, String) {
    let st = status_of(inst, sid);
    (
        kv(&st, "log_seq").expect("log_seq").to_string(),
        kv(&st, "log_ep").expect("log_ep").to_string(),
    )
}

/// `await log <what>` after `at`, a minute's hang detector.
fn await_log(inst: &Inst, sid: &str, what: &str, at: &(String, String)) -> String {
    let r = inst.verb(&format!(
        "@{sid} await log {what} since={} ep={} timeout=60000",
        at.0, at.1
    ));
    assert!(
        r.header().starts_with("OK seq="),
        "{}: await log {what}: {}\nlog:\n{}",
        inst.name,
        r.header(),
        inst.text(&format!("@{sid} log tier=host n=400"))
    );
    format!("{}\n{}", r.header(), r.rows().join("\n"))
}

/// The names in `state` that are a launch's takes of `session.toml`.
fn launch_takes(state: &Path) -> Vec<String> {
    std::fs::read_dir(state)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| {
            name.starts_with(".session.toml.aterm-taken-")
                || name.starts_with(".session.toml.aterm-retaken-")
        })
        .collect()
}

fn quit(inst: &mut Inst) {
    let _ = inst.ctl().request("invoke Quit");
    let gui = inst.gui.as_mut().expect("a running instance");
    let ended = aterm_spec::wait::within(DEADLINE, Duration::from_millis(25), || {
        gui.try_wait().ok().flatten()
    });
    assert!(ended.is_some(), "{} quits", inst.name);
    inst.gui = None;
    inst.lifeline.take();
    harness::end_bridge(&inst.root.join("link"), inst.launcher);
}

const HARNESS_HANDS_OVER: &str = "[harness]\napprove = \"none\"\n";

/// The factor the world's harness runs the time to a wall's reset at ([`reset_soon`]): the
/// wall's `in 1m` is three seconds on its clock.
const RESET_SPAN: f64 = 0.05;

/// The reset's development seams: the time to the wall's reset at [`RESET_SPAN`]
/// ([`aterm_agent::supervise::policy::turn_end::RESET_SPAN_SCALE_SEAM`]) and the grace and jitter
/// past it at a thousandth ([`aterm_agent::supervise::policy::turn_end::RESET_SCALE_SEAM`]), so
/// the harness's carry-on comes three seconds after the wall that says `in 1m`, its grace and
/// jitter a fraction of a second, and the wait for it is that reset and the hang detector.
fn reset_soon() -> Vec<(String, String)> {
    use aterm_agent::supervise::policy::turn_end;
    vec![
        (
            turn_end::RESET_SPAN_SCALE_SEAM.to_string(),
            RESET_SPAN.to_string(),
        ),
        (turn_end::RESET_SCALE_SEAM.to_string(), "0.001".to_string()),
    ]
}

/// `aterm <args>` run as a program on `inst`'s machine would run it ([`cli_command`]); its exit
/// code and both outputs, within the hang detector.
fn cli(inst: &Inst, args: &[&str]) -> (i32, String, String) {
    let mut cmd = cli_command(harness::aterm_to_launch(), &inst.root, args);
    let out = aterm_uds::deadline::output_until_complete_stdout(
        &mut cmd,
        std::time::Instant::now() + DEADLINE,
    )
    .unwrap_or_else(|e| panic!("aterm {args:?} exits within the hang detector: {e}"));
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `<aterm> <args>` as [`cli`] runs it on the machine whose scratch is `root`: its HOME, config,
/// runtime, state and temporary dirs, and nothing of the test's `ATERM_*` but the verify run's
/// mark ([`harness::kept_from_runner`]). Survey probes use their explicit
/// [`aterm_ctl::PROBE_WITHIN_SEAM`]; required fabric replies use the product's minute detector.
pub(crate) fn cli_command(aterm: &str, root: &Path, args: &[&str]) -> aterm_uds::spawnfd::Command {
    let mut cmd = aterm_uds::spawnfd::command(aterm);
    harness::kept_from_runner(&mut cmd)
        .args(args)
        .env(
            aterm_ctl::PROBE_WITHIN_SEAM,
            (DEADLINE / 2).as_millis().to_string(),
        )
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("cfg"))
        .env("XDG_RUNTIME_DIR", root.join("run"))
        .env("ATERM_STATE_HOME", root.join("astate"))
        .env("TMPDIR", root.join("tmp"))
        .stdin(OwnedStdio::null());
    cmd
}

/// The rows of `sid`'s tab menu, selected by identity before reading it headless.
fn menu(inst: &Inst, sid: &str) -> Vec<String> {
    let r = inst.verb(&format!("raise {sid}"));
    assert!(r.ok(), "raise {sid}: {}", r.header());
    let r = inst.verb("open tab-menu");
    assert!(r.ok(), "open tab-menu: {}", r.header());
    let rows = inst
        .verb("controls tab-menu")
        .rows()
        .iter()
        .filter_map(|l| {
            l.split_once(" text=")
                .map(|(_, t)| t.trim_matches('"').to_string())
        })
        .collect();
    let _ = inst.verb("key escape");
    rows
}

/// The stand-in `claude` Summarize starts on B: it runs each backticked `aterm …` command its
/// brief (its argv) names, and prints the lines of what they said that start with `@` or hold
/// `need=`, and, as the brief's fabric sentence says to read `aterm fabric brief --json`, each
/// session object that is not this node's own (`"this":false`) and names a need, as
/// `@<sid>@<node> need=<word>`; then `summary-end`, and stays (a worker's tab keeps its screen).
const SUMMARIZER: &str = "#!/bin/sh\n\
     case \"$1\" in --version) echo '2.1.0 (Claude Code)'; exit 0 ;; esac\n\
     case \" $* \" in *' -p /usage '*|*' -p /model '*) exit 0 ;; esac\n\
     printf '%s\\n' \"$*\" | grep -o '`aterm [^`]*`' | tr -d '`' > \"$HOME/brief.cmds\"\n\
     while read -r c; do $c 2>&1 | tr '{' '\\n' | sed -nE \
     -e 's/^\"node\":\"([^\"]*)\",\"sid\":\"([^\"]*)\",\"this\":false,.*\"need\":\"([^\"]*)\".*/@\\2@\\1 need=\\3/p' \
     -e t -e '/^@|need=/p'; done < \"$HOME/brief.cmds\"\n\
     echo summary-end\n\
     exec sleep 600\n";

/// B's options: a bash login shell whose PATH starts at its own `bin/`, which holds the
/// [`SUMMARIZER`] as `claude` and an `aterm` that is the build under test.
fn summarizing(owner: &Owner, name: &str) -> Opts {
    let bin = owner.tmp.join(name).join("bin");
    std::fs::create_dir_all(&bin).expect("bin/");
    for (word, text) in [
        ("claude", SUMMARIZER.to_string()),
        (
            "aterm",
            format!("#!/bin/sh\nexec '{}' \"$@\"\n", harness::aterm_to_launch()),
        ),
    ] {
        let path = bin.join(word);
        std::fs::write(&path, text).expect("a stand-in");
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        aterm_spec::wait::first_exec(&path, &["--version"]);
    }
    Opts {
        shell: Some("/bin/bash".into()),
        path_first: Some(bin),
        ..Opts::default()
    }
}

/// `invoke SummarizeFleet` on `inst`, then the worker tab's screen once its stand-in said
/// `summary-end`.
fn summarize(inst: &Inst) -> String {
    let before = sids(inst);
    let r = inst.verb("invoke SummarizeFleet");
    assert!(r.ok(), "invoke SummarizeFleet: {}", r.header());
    let worker = until("Summarize's worker tab", || {
        sids(inst).into_iter().find(|s| !before.contains(s))
    });
    until("the worker to finish its brief", || {
        let screen = inst.text(&format!("@{worker} text"));
        screen.contains("summary-end").then_some(screen)
    })
}

/// Every body the broker holds under `face` (a subject or a `>` pattern), read with the root.
fn bus_bodies(owner: &Owner, face: &str) -> Vec<String> {
    let mut c = owner.root_client();
    let mut out = Vec::new();
    let mut from = 0u64;
    loop {
        let (rows, (next, head)) = c
            .fetch(from, face, 256)
            .unwrap_or_else(|e| panic!("read {face}: {e}"));
        out.extend(
            rows.into_iter()
                .map(|(_, _, body)| String::from_utf8_lossy(&body).into_owned()),
        );
        if next >= head || next <= from {
            return out;
        }
        from = next;
    }
}

/// The number after `"<key>":` in a JSON text.
fn json_num(json: &str, key: &str) -> Option<u64> {
    let at = json.find(&format!("\"{key}\":"))? + key.len() + 3;
    json[at..]
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

/// `sid`'s own object in a brief's JSON: from its `"sid"` to the next session's.
fn session_json<'a>(json: &'a str, sid: &str) -> Option<&'a str> {
    let key = format!("\"sid\":\"{sid}\"");
    let at = json.find(&key)?;
    let rest = &json[at + key.len()..];
    let end = rest.find("\"sid\":\"").unwrap_or(rest.len());
    Some(&json[at..at + key.len() + end])
}

/// The `last_seq` a brief's JSON holds for `sid`.
fn bus_last_seq(json: &str, sid: &str) -> Option<u64> {
    json_num(session_json(json, sid)?, "last_seq")
}

/// The number `key` a brief's JSON holds for `sid`, read in `sid`'s own object.
fn bus_num(json: &str, sid: &str, key: &str) -> Option<u64> {
    json_num(session_json(json, sid)?, key)
}

/// The string `key` a brief's JSON holds for `sid`, read in `sid`'s own object.
fn bus_str(json: &str, sid: &str, key: &str) -> Option<String> {
    let obj = session_json(json, sid)?;
    let at = obj.find(&format!("\"{key}\":\""))? + key.len() + 4;
    Some(obj[at..at + obj[at..].find('"')?].to_string())
}

/// The first two parts of a clause (`now: approval · bash · …`), its word and what.
fn clause_head(clause: &str) -> Vec<&str> {
    clause.split(" \u{b7} ").take(2).collect()
}

/// B's `aterm fabric brief --json`, asserted to exit 0.
fn brief_json(b: &Inst) -> String {
    let (code, out, err) = cli(b, &["fabric", "brief", "--json"]);
    assert_eq!(code, 0, "fabric brief --json: {out}\n{err}");
    out
}

/// B's Overview, read headless after `open overview` (a fresh read of the bus).
fn overview(b: &Inst) -> String {
    let _ = b.verb("key escape");
    let r = b.verb("open overview");
    assert!(r.ok(), "open overview: {}", r.header());
    b.text("controls overview")
}

/// The `now:` row of `sid`'s tab menu, if it has one.
fn clause(inst: &Inst, sid: &str) -> Option<String> {
    menu(inst, sid).into_iter().find(|r| r.starts_with("now: "))
}

/// [`until`] on `sid` in `inst` within `budget`: a failure shows the session's log at the host
/// tier, its status and its screen.
fn until_on<T>(
    inst: &Inst,
    sid: &str,
    budget: Duration,
    what: &str,
    probe: impl FnMut() -> Option<T>,
) -> T {
    aterm_spec::wait::within(budget, Duration::from_millis(100), probe).unwrap_or_else(|| {
        panic!(
            "{}: timed out after {budget:?} waiting for: {what}\nstatus: {}\nlog:\n{}\nscreen:\n{}",
            inst.name,
            status_of(inst, sid),
            inst.text(&format!("@{sid} log tier=host n=120")),
            inst.text(&format!("@{sid} text")),
        )
    })
}

/// `@<sid> mark - <mark>` at the pace the mark limiter sets a `ctl` author (one a second after a
/// burst): an `ERR busy retry_ms=<n>` is the limiter's answer, honoured by sending again after
/// `n` ms, within the hang detector.
fn paced_mark(inst: &Inst, sid: &str, mark: &str) {
    let line = format!("@{sid} mark - {mark}");
    let deadline = Instant::now() + DEADLINE;
    loop {
        let r = inst.verb(&line);
        if r.ok() {
            return;
        }
        let wait = r
            .header()
            .strip_prefix("ERR busy retry_ms=")
            .and_then(|n| n.trim().parse::<u64>().ok())
            .unwrap_or_else(|| panic!("{line}: {}", r.header()));
        assert!(
            Instant::now() < deadline,
            "{line}: still busy after {DEADLINE:?}"
        );
        std::thread::sleep(Duration::from_millis(wait));
    }
}

/// A person's (here a program's, `a-<name>`) cap file in `inst`'s scratch.
fn person_cap(inst: &Inst, person: &str) -> PathBuf {
    let path = inst.root.join(format!("{person}.cap"));
    let caps: String = astream_cap::GrantCeiling::aterm_person_ring(FLEET, person)
        .expect("a person ring")
        .rules()
        .iter()
        .map(|g| cap_line(g))
        .collect();
    std::fs::write(&path, caps).expect("the person's cap");
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod 600");
    path
}

/// **LEGS 2-5 (D4, D6, 6E-5, 6E-6, 6E-7, U43): THE STREAM, AND THE DECISIONS MADE FROM IT.** A
/// harness-hosted stand-in S on A reaches an approval box: B, which shares no LAN fleet, no
/// runtime dir and no control socket with A, reads S's need from the bus alone (brief text and
/// JSON, the Overview's fabric chip and its count, Summarize's worker); a program P on B answers
/// with a note that never acts on S; the approval clears B's chip; S then hits a usage wall, and
/// A's tab menu shows what the log says, the harness's carry-on with its author when it comes.
#[test]
fn the_owners_ask_over_the_stream() {
    let owner = Owner::boot();
    let a = Inst::boot(
        &owner,
        "a",
        NODE_A,
        Opts {
            config: HARNESS_HANDS_OVER.into(),
            env: reset_soon(),
            ..Opts::default()
        },
    );
    let b_opts = summarizing(&owner, "b");
    let b = Inst::boot(&owner, "b", NODE_B, b_opts);
    a.wait_ready(&owner);
    b.wait_ready(&owner);

    // Leg 2: the default config mirrors turns; the broker's Hello advertises its four rules.
    let fs = a.verb("fabric status");
    assert!(
        fs.header().contains(" bridge_log=turns mirror=on"),
        "A's fabric status: {}",
        fs.header()
    );
    let mut c = owner.root_client();
    c.hello().expect("hello");
    let rules = c.retention_rules().expect("the broker advertises rules");
    assert_eq!(rules, aterm_link::broker_rules::default_rules());
    assert_eq!(
        rules.rules().len() + 1,
        4,
        "three rules and the default: {rules:?}"
    );
    drop(c);

    write_fake(&a, "resets in 1m");
    let s = spawn_tab(&a);
    let at = cursor(&a, &s);
    start_fake(
        &a,
        &s,
        "k1",
        &[
            "busy.gate",
            "box.key",
            "wall.type",
            "busy2.gate",
            "idle.gate",
        ],
    );
    await_log(&a, &s, "open=turn", &at);
    let title = menu(&a, &s).first().cloned().expect("the menu's title row");
    let at = cursor(&a, &s);
    release(&a, "k1", 1);
    let need = await_log(&a, &s, "need", &at);
    assert!(
        need.contains(" approval ") && need.contains("box=bash"),
        "{need}"
    );

    // Leg 5: A's clause names the box; the tab's label is its identity title.
    let menu_box = menu(&a, &s);
    assert_eq!(menu_box.first(), Some(&title), "{menu_box:?}");
    let head_a = clause(&a, &s).expect("a now: row");
    assert!(head_a.starts_with("now: approval \u{b7} bash"), "{head_a}");
    assert!(!head_a.contains("by the"), "{head_a}");

    // Leg 2: B's brief folds S from the bus.
    let face = format!("@{s}@{NODE_A}");
    let text = until("B's brief to show S's need", || {
        let (code, out, _) = cli(&b, &["fabric", "brief", &face]);
        (code == 0 && out.contains("need=approval:bash")).then_some(out)
    });
    assert!(
        text.lines()
            .any(|l| l.starts_with(&format!("{face} live ep="))),
        "{text}"
    );
    // Leg 5: B's bus need word is A's clause head at the same moment: A's clause read on both
    // sides of B's read, and B's `"need":"<word>:<what>"` for S said as the clause says it.
    let json = brief_json(&b);
    // The clause's word and what: its age (`just now`, `10s ago`) moves with the clock alone.
    let head_after = clause(&a, &s).expect("a now: row");
    assert_eq!(
        clause_head(&head_after),
        clause_head(&head_a),
        "A's clause moved across B's read: {head_after:?}, was {head_a:?}"
    );
    let bus_need = bus_str(&json, &s, "need").unwrap_or_else(|| panic!("S's need: {json}"));
    let (word, what) = bus_need.split_once(':').expect("<word>:<what>");
    assert_eq!(
        clause_head(&head_a),
        [format!("now: {word}").as_str(), what],
        "B's need is not A's clause head: {json}"
    );
    assert!(json.contains(&format!("\"sid\":\"{s}\"")), "{json}");
    assert!(json.contains("\"need\":\"approval:bash\""), "{json}");

    // Leg 3: B's Overview holds the chip, from the bus alone.
    let ov = until("B's Overview to hold S's chip", || {
        let ov = overview(&b);
        ov.contains(&format!(
            "overview fabric chip node={NODE_A} sid={s} need=approval:bash"
        ))
        .then_some(ov)
    });
    let head = ov
        .lines()
        .find(|l| l.starts_with("overview window="))
        .unwrap_or_default();
    assert_eq!(kv(head, "needs"), Some("1"), "{ov}");
    assert_eq!(kv(head, "fabric"), Some("1"), "{ov}");
    // B's own survey answers, and lists B's own session, yet holds none of A's.
    let (code, survey, err) = cli(&b, &["ctl", "survey"]);
    assert_eq!(code, 0, "B's survey: {survey}\n{err}");
    let b_own = sids(&b);
    assert!(
        b_own.iter().any(|sid| survey.contains(sid.as_str())),
        "B's survey lists none of B's sessions {b_own:?}: {survey}"
    );
    assert!(
        !survey.contains(&s),
        "B reaches A's sessions locally: {survey}"
    );

    // Leg 4: Summarize's worker on B shows S's need as another node's, which only
    // `aterm fabric brief --json` says.
    let screen = summarize(&b);
    assert!(
        screen.contains(&face) && screen.contains("need=approval:bash"),
        "{screen}"
    );

    // Leg 4: a program P on B decides from the brief, and answers with a note citing what it read.
    let json = brief_json(&b);
    let read_at = json_num(&json, "head").expect("the brief's head");
    let cap = person_cap(&b, "a-p");
    let (code, out, err) = cli(
        &b,
        &[
            "fabric",
            "post",
            "--cap-file",
            &cap.to_string_lossy(),
            &format!("to={face}"),
            "kind=note",
            &format!("re=@{read_at}"),
            "--",
            "approve it",
        ],
    );
    assert_eq!(code, 0, "P's post: {out}\n{err}");
    let mail = until("S's log to hold P's note", || {
        let rows = a.text(&format!("@{s} log kinds=mail"));
        rows.contains("mail dir=in kind=note").then_some(rows)
    });
    eprintln!("mail: {mail}");
    // Mail crosses only as counts, in a snapshot, and one is due once S's counts move (at most one
    // a second, LOOP-2): B's brief counts P's note with no further record of S's.
    until("B's brief to count P's note", || {
        bus_num(&brief_json(&b), &s, "mail_in").filter(|n| *n == 1)
    });
    assert!(
        keys_read(&a, "k1").is_empty(),
        "the note reached S's PTY: {:?}",
        keys_read(&a, "k1")
    );
    let after_note = clause(&a, &s);
    assert_eq!(
        after_note.as_deref().map(clause_head),
        Some(clause_head(&head_a)),
        "the note moved the clause: {after_note:?}, was {head_a:?}"
    );
    for body in bus_bodies(&owner, &format!("/f/{FLEET}/pub/>")) {
        for leak in [
            " kind=note",
            " from=",
            " re=",
            " off=",
            " dir=",
            "approve it",
            "a-p",
        ] {
            assert!(!body.contains(leak), "{leak} crossed onto pub/: {body}");
        }
    }

    // Leg 5: the approval; B's next read clears the approval chip; the wall's clause, no author.
    // `walled` comes before the approval is sent: the stand-in draws the wall only once it has
    // read that key, so the harness reads the wall, and places its reset, after `walled`.
    let walled = Instant::now();
    let r = a.verb(&format!("@{s} key act=approve 1"));
    assert!(r.ok(), "key act=approve: {}", r.header());
    until("the stand-in to read the approval", || {
        (keys_read(&a, "k1") == ["1"]).then_some(())
    });
    let wall = until_on(&a, &s, DEADLINE, "the wall's clause", || {
        clause(&a, &s).filter(|c| c.starts_with("now: limit"))
    });
    assert!(
        wall.starts_with("now: limit \u{b7} in 1m \u{b7} "),
        "{wall}"
    );
    assert!(!wall.contains("by the"), "{wall}");
    // The harness writes no label of its own while the wall's need is open (U22: it labels only
    // its own moments with no need open, since an open need's row masks any label, §7.3).
    let under_wall = a.text(&format!("@{s} log tier=host n=400"));
    assert!(
        !under_wall
            .lines()
            .any(|l| l.contains(" label ") && l.contains(" by=harness ")),
        "a harness label under the wall: {under_wall}"
    );
    let wall_chip = format!("overview fabric chip node={NODE_A} sid={s} need=wall:usage-weekly");
    until(
        "B's Overview to replace the approval chip with the wall chip",
        || {
            let ov = overview(&b);
            (!ov.contains("need=approval:bash") && ov.contains(&wall_chip)).then_some(ov)
        },
    );
    let json = brief_json(&b);
    assert_eq!(
        bus_str(&json, &s, "detail").as_deref(),
        clause_head(&wall).get(1).copied(),
        "B's wall is not A's clause head: {json}"
    );
    assert!(
        json.contains("\"need\":\"wall:usage-weekly\",\"detail\":\"in 1m\""),
        "B's need is not A's clause head: {json}"
    );
    assert_eq!(menu(&a, &s).first(), Some(&title));

    // Leg 5: the harness waits out the reset (`in 1m`, three seconds on its clock), then its grace
    // and jitter, a fraction of a second ([`reset_soon`]), and types its carry-on: the menu shows
    // its label with its author once the wall closes. The wait is that reset, counted from
    // `walled`, before the approval the wall follows, and the hang detector; the carry-on, as the
    // test sees it, never comes before the reset. The harness places `in 1m` from the whole second
    // it read the wall in, up to a twentieth of a second early at this scale, which its grace
    // covers, and it read the wall after `walled`.
    let scaled_reset = Duration::from_secs(60).mul_f64(RESET_SPAN);
    let reset = (walled + scaled_reset).saturating_duration_since(Instant::now());
    until_on(&a, &s, reset + DEADLINE, "the harness's carry-on", || {
        (keys_read(&a, "k1").len() == 2).then_some(())
    });
    let resumed_after = walled.elapsed();
    assert!(
        resumed_after >= scaled_reset,
        "the carry-on came {resumed_after:?} after the approval the wall follows, before the \
         wall's reset ({scaled_reset:?})"
    );
    assert!(
        keys_read(&a, "k1")[1].starts_with("text:[aterm harness] "),
        "the carry-on writes its own resume message over the suggestion"
    );
    let carry = until_on(
        &a,
        &s,
        DEADLINE,
        "the harness's label with its author",
        || clause(&a, &s).filter(|c| c.starts_with("now: resuming after limit")),
    );
    assert!(carry.contains(" \u{b7} by the harness"), "{carry}");
    let log = a.text(&format!("@{s} log tier=host n=400"));
    assert!(
        log.lines()
            .any(|l| l.contains(" label text=resuming%20after%20limit ttl=30 by=harness ")),
        "{log}"
    );
    assert!(
        log.lines()
            .any(|l| l.contains(" act word=continue rule=usage-resume.v1 by=harness ")),
        "{log}"
    );
    assert_eq!(menu(&a, &s).first(), Some(&title));

    // The turn ends; still no word of the mail crossed; a ctl label never shows.
    let at = cursor(&a, &s);
    release(&a, "k1", 4);
    await_log(&a, &s, "close=turn", &at);
    // Leg 6 (6H-4): the carry-on's turn is the harness's.
    let brief = a.text(&format!("@{s} brief"));
    assert!(
        brief
            .lines()
            .any(|l| l.starts_with("t2 ") && l.contains(" by=harness")),
        "the carry-on's turn: {brief}"
    );
    for body in bus_bodies(&owner, &format!("/f/{FLEET}/pub/>")) {
        for leak in [
            " kind=note",
            " from=",
            " re=",
            " off=",
            " dir=",
            "approve it",
            "a-p",
        ] {
            assert!(!body.contains(leak), "{leak} crossed onto pub/: {body}");
        }
    }
    paced_mark(&a, &s, "label text=ctl%20words ttl=30");
    let shown = clause(&a, &s);
    assert!(
        !shown.as_deref().unwrap_or_default().contains("ctl words"),
        "a ctl label shows: {shown:?}"
    );
    assert!(
        a.text(&format!("@{s} log tier=host n=400"))
            .lines()
            .any(|l| l.contains(" label text=ctl%20words ttl=30 by=ctl ")),
        "the ctl label is in the log"
    );
    eprintln!("resumed after {resumed_after:?}; clause after: {shown:?}");
}

/// **LEG 5 (6F-9): EACH MOVE OF THE PRINTED NEED IS A `shown` MARK.** S's box opens under a
/// harness that answers no box (`approve = "none"`): it escalates the box to the person, and that
/// ask is the need the person is shown (`need=ask sev=decide`), a `shown` mark. The person's
/// approval closes the box and the harness lifts its ask: a later `shown` mark with no need.
/// `brief` counts them. The harness's escalate act is in the log before the person answers, and
/// it and the escalation are the box's, so a fold through the box's close names no need (fix E3).
#[test]
fn each_move_of_the_printed_need_is_a_shown_mark() {
    let (_owner, a) = live_world();
    write_fake(&a, "resets in 1m");
    let s = spawn_tab(&a);
    let at = cursor(&a, &s);
    start_fake(&a, &s, "k1", &["busy.gate", "box.key", "idle3.gate"]);
    await_log(&a, &s, "open=turn", &at);
    let at = cursor(&a, &s);
    release(&a, "k1", 1);
    await_log(&a, &s, "need", &at);
    let shown = || -> Vec<String> {
        let r = a.verb(&format!("@{s} log kinds=shown"));
        assert!(r.ok(), "log kinds=shown: {}", r.header());
        r.rows().to_vec()
    };
    let asked = until_on(&a, &s, DEADLINE, "the escalated box's shown mark", || {
        shown().into_iter().find(|l| kv(l, "need").is_some())
    });
    assert_eq!(
        (kv(&asked, "need"), kv(&asked, "sev")),
        (Some("ask"), Some("decide")),
        "{asked}"
    );
    // Fix E3 (E2E-3): the harness's escalate act is in the log while the box waits for the
    // person, and both it and the escalation are the box's.
    await_log(&a, &s, "kind=act", &at);
    let recs = || -> Vec<aterm_slog::Rec> {
        let r = a.verb(&format!("@{s} log n=2000"));
        assert!(r.ok(), "log: {}", r.header());
        r.rows()
            .iter()
            .filter_map(|l| aterm_slog::decode(l).ok())
            .collect()
    };
    let word = |r: &aterm_slog::Rec| r.f.get_word(aterm_slog::Key::Word);
    let before = until_on(&a, &s, DEADLINE, "the escalate act", || {
        let all = recs();
        all.iter()
            .any(|r| r.k == aterm_slog::Kind::Act && word(r) == Some("escalate"))
            .then_some(all)
    });
    let the_box = before
        .iter()
        .rev()
        .find(|r| r.op == aterm_slog::Op::Open && r.k == aterm_slog::Kind::Approval)
        .map(|r| r.node)
        .expect("the box's step");
    let on = |kind: aterm_slog::Kind, w: &str| {
        before
            .iter()
            .find(|r| r.op == aterm_slog::Op::Mark && r.k == kind && word(r) == Some(w))
            .map(|r| r.node)
    };
    assert_eq!(
        (
            on(aterm_slog::Kind::Act, "escalate"),
            on(aterm_slog::Kind::Need, "escalated")
        ),
        (Some(the_box), Some(the_box)),
        "the act and the escalation are the box's"
    );
    let r = a.verb(&format!("@{s} key act=approve 1"));
    assert!(r.ok(), "key act=approve: {}", r.header());
    until("the stand-in to read the approval", || {
        (keys_read(&a, "k1") == ["1"]).then_some(())
    });
    // The box's close ends its escalation in the same record: a fold of the log through the
    // close names no need, whenever the harness's clear comes after it.
    let through_close = until_on(&a, &s, DEADLINE, "the box's close", || {
        let all = recs();
        let at = all
            .iter()
            .position(|r| r.op == aterm_slog::Op::Close && r.node == the_box)?;
        Some(all[..=at].to_vec())
    });
    let mut fold = aterm_slog::Fold::new(at.1.parse().expect("log_ep"));
    for r in &through_close {
        fold.apply(r);
    }
    assert_eq!(
        fold.state().top_observed_need().map(|n| n.word),
        None,
        "a fold through the box's close names no need"
    );
    let rows = until_on(
        &a,
        &s,
        DEADLINE,
        "the shown mark of the box's close",
        || {
            let rows = shown();
            let at = rows.iter().position(|l| *l == asked)?;
            rows[at + 1..]
                .iter()
                .any(|l| kv(l, "need").is_none())
                .then_some(rows)
        },
    );
    eprintln!("shown: {rows:#?}");
    // `brief` counts them: read between two readings of the marks, its count lies between them.
    let brief = a.text(&format!("@{s} brief"));
    let after = shown().len();
    let counted = brief.lines().find_map(|l| {
        l.split(" [")
            .next()?
            .strip_prefix("shown ")?
            .parse::<usize>()
            .ok()
    });
    assert!(
        counted.is_some_and(|n| (rows.len()..=after).contains(&n)),
        "{} to {after} shown marks: {brief}",
        rows.len()
    );
    release(&a, "k1", 3);
}

/// The permission bits of `path`.
fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .permissions()
        .mode()
        & 0o777
}

/// Every journal file of `inst` (`<state>/logs/session-log/*.slog`) with its bytes.
fn journals(inst: &Inst) -> Vec<(PathBuf, Vec<u8>)> {
    let dir = inst.root.join("astate/logs/session-log");
    let mut out: Vec<(PathBuf, Vec<u8>)> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "slog"))
        .map(|p| {
            let bytes = std::fs::read(&p).expect("a journal");
            (p, bytes)
        })
        .collect();
    out.sort();
    out
}

fn holds(bytes: &[u8], needle: &str) -> bool {
    bytes.windows(needle.len()).any(|w| w == needle.as_bytes())
}

/// Whether `log` (a `@<sid> log` reply) opens a `cmd` run whose fields after its parent begin
/// with `fields`, whatever run id and parent it was given.
fn opens_cmd(log: &str, fields: &str) -> bool {
    let want: Vec<&str> = fields.split_whitespace().collect();
    log.lines().any(|l| {
        let w: Vec<&str> = l.split_whitespace().collect();
        matches!(
            w.as_slice(),
            [_, "open", id, "cmd", par, rest @ ..]
                if id.starts_with('r') && par.starts_with("par=") && rest.starts_with(&want)
        )
    })
}

/// **LEG 1 (D3, 6A-2, 6C-4, U47): THE JOURNAL ACROSS A QUIT AND A KILL.** On the default config
/// a shell session's command (its second word a sentinel) and a harness-hosted stand-in's turn
/// land in a private journal that keeps the command's first word and never its second; a quit
/// and a relaunch answer the earlier process's records by its sids, the restored session's old
/// cursor answers `gap why=reset` and then the relaunch's own open, and the node log says how
/// the process before ended: `clean` after a quit, `killed` after a SIGKILL by PID.
#[test]
fn the_journal_across_a_quit_and_a_kill() {
    let owner = Owner::boot();
    let mut a = Inst::boot(
        &owner,
        "a",
        NODE_A,
        Opts {
            config: HARNESS_HANDS_OVER.into(),
            ..Opts::default()
        },
    );
    a.wait_ready(&owner);
    write_fake(&a, "resets in 1m");
    let shell = sids(&a)[0].clone();
    let script = marked_command(&a.root, "marked.sh", "echo QZP6SECRET1 42");
    type_line(&a, &shell, &format!("sh {}", script.display()));
    until_on(&a, &shell, DEADLINE, "the shell's run in its log", || {
        a.text(&format!("@{shell} log"))
            .contains(" cmd ")
            .then_some(())
    });
    let s = spawn_tab(&a);
    let at = cursor(&a, &s);
    start_fake(&a, &s, "k1", &["busy.gate", "idle.key"]);
    await_log(&a, &s, "open=turn", &at);
    let at = cursor(&a, &s);
    release(&a, "k1", 1);
    await_log(&a, &s, "close=turn", &at);

    let dir = a.root.join("astate/logs/session-log");
    assert_eq!(mode_of(&dir), 0o700, "{}", dir.display());
    // The journal's writer coalesces on its own thread, so the node log's `close=turn` can come
    // before the segment holding the command lands: wait for it (a hang detector).
    let files = until("a journal holding the shell's command", || {
        let files = journals(&a);
        files
            .iter()
            .any(|(_, b)| holds(b, "detail=echo"))
            .then_some(files)
    });
    for (path, bytes) in &files {
        assert_eq!(mode_of(path), 0o600, "{}", path.display());
        assert!(
            !holds(bytes, "QZP6SECRET1"),
            "a command's later word in {}",
            path.display()
        );
    }
    let old = cursor(&a, &shell);
    // The relaunch judges this run by its ledger row, which its census lands before the amend.
    until("the first launch's start amended prev_end=none", || {
        a.text("log n")
            .contains(" amend n start prev_end=none ")
            .then_some(())
    });

    // A quit, then a relaunch on the same roots.
    quit(&mut a);
    a.launch();
    a.wait_ready(&owner);
    let shell_then = a.text(&format!("@{shell} log"));
    // The sweep numbers the runs: under load it names the `sh` group in front before the
    // script prints its OSC 133 C, so a `prog` run can open first and take `r1`.
    assert!(
        opens_cmd(&shell_then, "blk=0 detail=echo")
            && shell_then.contains(" close - spawn out=app-quit "),
        "{shell_then}"
    );
    assert!(!shell_then.contains("QZP6SECRET1"), "{shell_then}");
    let s_then = a.text(&format!("@{s} log"));
    assert!(
        s_then.contains(" open t1 turn ")
            && s_then.contains(" close t1 turn out=done ")
            && s_then.contains(" close - spawn out=app-quit "),
        "{s_then}"
    );
    let restored = sids(&a)
        .into_iter()
        .find(|n| {
            a.text(&format!("@{n} log"))
                .contains(&format!(" prev={shell} "))
        })
        .unwrap_or_else(|| panic!("no session restored from {shell}: {}", a.text("sessions")));
    let since = a.text(&format!("@{restored} log since={} ep={}", old.0, old.1));
    let rows: Vec<&str> = since.lines().skip(1).collect();
    assert!(
        rows.first()
            .is_some_and(|r| r.starts_with("gap why=reset ")),
        "{since}"
    );
    assert!(
        rows.get(1).is_some_and(|r| r.starts_with("1 open - ")),
        "{since}"
    );
    // The recovery census amends the start on a background thread of its own, which can run
    // after the launch reports ready: wait for its amend, at the hang detector.
    until("the relaunch's start amended prev_end=clean", || {
        a.text("log n")
            .contains(" amend n start prev_end=clean ")
            .then_some(())
    });

    // A SIGKILL by PID, then a relaunch.
    let pid = a.pid();
    a.kill();
    assert!(!aterm_uds::process::pid_alive(pid), "{pid} still runs");
    a.launch();
    a.wait_ready(&owner);
    until("the relaunch's start amended prev_end=killed", || {
        a.text("log n")
            .contains(" amend n start prev_end=killed ")
            .then_some(())
    });
    for (path, bytes) in journals(&a) {
        assert_eq!(mode_of(&path), 0o600, "{}", path.display());
        assert!(!holds(&bytes, "QZP6SECRET1"), "{}", path.display());
    }
}

/// **LEGS 3 AND 4's NEGATIVE CONTROL: A's MIRROR OFF.** The same world with A's `[fabric] log =
/// "off"`: S's need is A's alone, and once S's presence row has moved past it on the bus, B's
/// brief holds no S, B's Overview no fabric chip, and Summarize's worker no need of S's.
#[test]
fn with_as_mirror_off_b_learns_nothing_of_s() {
    let owner = Owner::boot();
    let a = Inst::boot(
        &owner,
        "a",
        NODE_A,
        Opts {
            fabric: "log = \"off\"\n".into(),
            config: HARNESS_HANDS_OVER.into(),
            ..Opts::default()
        },
    );
    let b_opts = summarizing(&owner, "b");
    let b = Inst::boot(&owner, "b", NODE_B, b_opts);
    a.wait_ready(&owner);
    b.wait_ready(&owner);
    let fs = a.verb("fabric status");
    assert!(!fs.header().contains("bridge_log="), "{}", fs.header());
    write_fake(&a, "resets in 1m");
    let s = spawn_tab(&a);
    let at = cursor(&a, &s);
    start_fake(&a, &s, "k1", &["busy.gate", "box.key", "idle.gate"]);
    await_log(&a, &s, "open=turn", &at);
    let at = cursor(&a, &s);
    release(&a, "k1", 1);
    await_log(&a, &s, "need", &at);
    let head = clause(&a, &s).expect("A's clause");
    assert!(head.starts_with("now: approval \u{b7} bash"), "{head}");
    assert!(a.verb(&format!("@{s} meta set attention nc-probe")).ok());
    let presence = format!("/f/{FLEET}/pub/{NODE_A}/{s}/presence");
    until("S's presence row past the need", || {
        bus_bodies(&owner, &presence)
            .iter()
            .any(|b| b.contains("attention="))
            .then_some(())
    });
    let ev = bus_bodies(&owner, &format!("/f/{FLEET}/pub/{NODE_A}/*/ev"));
    assert!(ev.is_empty(), "A's mirror is off, yet: {ev:?}");
    let json = brief_json(&b);
    assert!(!json.contains(&s), "{json}");
    let (_, text, _) = cli(&b, &["fabric", "brief", &format!("@{s}@{NODE_A}")]);
    assert!(!text.contains("need="), "{text}");
    let ov = overview(&b);
    assert!(!ov.contains("overview fabric chip"), "{ov}");
    let screen = summarize(&b);
    assert!(
        !screen.contains("need=") && !screen.contains(&s),
        "{screen}"
    );
}

/// The `write-input` token of an edge minted from `src` to `dst` (`connect … kind=push`).
fn edge_token(inst: &Inst, dst: &str, src: &str) -> String {
    let r = inst.verb(&format!("connect dst={dst} src={src} kind=push"));
    assert!(r.ok(), "connect: {}", r.header());
    kv(r.header(), "write-input")
        .unwrap_or_else(|| panic!("a write-input token: {}", r.header()))
        .to_string()
}

/// One request over an edge: a connection of its own whose one line presents the edge's token
/// (`TOKEN <token> <line>`).
fn edge_request(inst: &Inst, token: &str, line: &str) -> Reply {
    let stream = aterm_link::ctl::dial(&inst.sock).expect("connect to aterm");
    stream
        .set_read_timeout(Some(DEADLINE))
        .expect("bound the reply's read");
    let mut c = Ctl::from_stream(stream).expect("a connection");
    c.request(&format!("TOKEN {token} {line}"))
        .unwrap_or_else(|e| panic!("{line} over an edge: {e}"))
}

/// What `by`'s acted-key marks on `dst` account for: one a mark, and each mark's `dropped=`.
fn accounted(inst: &Inst, dst: &str, by: &str) -> (u64, u64) {
    let r = inst.verb(&format!("@{dst} log kinds=input n=1000"));
    assert!(r.ok(), "log kinds=input: {}", r.header());
    let by = format!(" by={by} ");
    r.rows()
        .iter()
        .filter(|l| l.contains(" input verb=key act=approve ") && l.contains(&by))
        .fold((0, 0), |(marks, dropped), l| {
            let d = kv(l, "dropped").map_or(0, |n| n.parse::<u64>().expect("a count"));
            assert!(
                kv(l, "pool").is_none(),
                "an edge counted in the shared pool: {l}"
            );
            (marks + 1, dropped + d)
        })
}

/// LEG 6's world: one instance A whose harness hands a box over to the person.
fn live_world() -> (Owner, Inst) {
    let owner = Owner::boot();
    let a = Inst::boot(
        &owner,
        "a",
        NODE_A,
        Opts {
            config: HARNESS_HANDS_OVER.into(),
            ..Opts::default()
        },
    );
    a.wait_ready(&owner);
    (owner, a)
}

/// **LEG 6, THE LIVE LEGS PHASE 5 COULD NOT RUN (6C-6): TWO EDGES, EACH ITS OWN `dropped=`.** Two
/// minted edges flood acted keys at one tab (the boot shell D); each author's marks and its own
/// `dropped=` account for every key it sent, and neither is counted in the shared pool.
#[test]
fn two_edges_flooding_one_tab_each_say_their_own_dropped() {
    let (_owner, a) = live_world();
    let d = sids(&a)[0].clone();
    let e1 = spawn_tab(&a);
    let e2 = spawn_tab(&a);
    // An edge's token is checked against the active session's edges: D, the window's first
    // tab, is brought to the front.
    let r = a.verb(&format!("@{d} tab 1"));
    assert!(r.header().starts_with("OK 1 "), "tab 1: {}", r.header());
    let floods: Vec<(String, String, u64)> = [(e1, 30u64), (e2, 45)]
        .into_iter()
        .map(|(src, n)| (edge_token(&a, &d, &src), src, n))
        .collect();
    let key = format!("@{d} key act=approve 1");
    std::thread::scope(|scope| {
        for (token, _, n) in &floods {
            let (a, key) = (&a, &key);
            scope.spawn(move || {
                for _ in 0..*n {
                    let r = edge_request(a, token, key);
                    assert!(r.ok(), "an edge's key: {}", r.header());
                }
            });
        }
    });
    for (token, src, n) in &floods {
        let mut sent = *n;
        let (marks, dropped) = until("the edge's marks to say every key it sent", || {
            let got = accounted(&a, &d, src);
            assert!(
                got.0 + got.1 <= sent,
                "{src} said more than it sent: {got:?} of {sent}"
            );
            if got.0 + got.1 == sent {
                return Some(got);
            }
            std::thread::sleep(Duration::from_millis(aterm_slog::MARK_REFILL_MS));
            let r = edge_request(&a, token, &key);
            assert!(r.ok(), "an edge's key: {}", r.header());
            sent += 1;
            None
        });
        assert!(
            dropped > 0,
            "{src}: the limiter never held a key back: {marks} marks"
        );
        assert!(
            marks > u64::from(aterm_slog::MARK_BURST),
            "{src}: {marks} marks"
        );
        eprintln!("edge from {src}: {marks} marks, dropped={dropped}, {sent} keys sent");
    }
}

/// **LEG 6, THE LIVE LEGS PHASE 5 COULD NOT RUN: THE LATE `re=`.** S's box is open; a paste S
/// cannot read yet holds the PTY, and the `act=approve` admitted behind it is written only once
/// the box has closed: its mark lands after the close and says `re=` the box.
#[test]
fn an_approval_written_after_its_box_closed_says_re_the_box() {
    let (_owner, a) = live_world();
    write_fake(&a, "resets in 1m");
    let s = spawn_tab(&a);
    let at = cursor(&a, &s);
    start_fake(
        &a,
        &s,
        "k1",
        &[
            "busy.gate",
            "box.gate",
            "busy2.gate",
            "idle.drain",
            "idle3.gate",
        ],
    );
    await_log(&a, &s, "open=turn", &at);
    let at = cursor(&a, &s);
    release(&a, "k1", 1);
    let need = await_log(&a, &s, "need", &at);
    assert!(need.contains(" approval "), "{need}");
    let log = a.text(&format!("@{s} log tier=host n=400"));
    let step = log
        .lines()
        .find(|l| l.contains(" open ") && l.contains(" approval "))
        .and_then(|l| l.split_whitespace().nth(2))
        .unwrap_or_else(|| panic!("the box's step: {log}"))
        .to_string();
    let filler = vec![b'x'; 16 * 1024];
    let mut closed_at: Option<u64> = None;
    std::thread::scope(|scope| {
        let (sock, token, sid) = (&a.sock, &a.token, &s);
        let paste = scope.spawn(move || {
            let mut c = connect(sock, token).expect("a connection");
            let r = c
                .request_with_body(&format!("@{sid} paste-bin {}", filler.len()), &filler)
                .expect("the paste");
            r.header().to_string()
        });
        until_on(&a, &s, DEADLINE, "the paste to fill S's input", || {
            kv(&status_of(&a, &s), "input_bytes")
                .and_then(|n| n.parse::<u64>().ok())
                .filter(|n| *n > 0)
        });
        let r = a.verb(&format!("@{s} key unread=ok act=approve 1"));
        assert!(r.ok(), "the approval: {}", r.header());
        let at = cursor(&a, &s);
        release(&a, "k1", 2);
        let closed = await_log(&a, &s, "close=step", &at);
        closed_at = kv(&closed, "seq").and_then(|n| n.parse().ok());
        release(&a, "k1", 3);
        until("S to drain its input", || {
            keys_read(&a, "k1")
                .iter()
                .any(|k| k == "drained")
                .then_some(())
        });
        eprintln!("the paste: {}", paste.join().expect("the paste's thread"));
    });
    let late = until_on(&a, &s, DEADLINE, "the approval's input mark", || {
        let r = a.verb(&format!("@{s} log kinds=input n=200"));
        r.rows()
            .iter()
            .find(|l| l.contains(" input verb=key act=approve "))
            .cloned()
    });
    assert_eq!(kv(&late, "re"), Some(step.as_str()), "{late}");
    // Written late: the mark lands after the box's close, and still binds to the box.
    let written: u64 = late
        .split_whitespace()
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("the mark's seq: {late}"));
    let closed_at = closed_at.expect("the close's seq");
    assert!(
        written > closed_at,
        "the approval was written before the box closed: {written} <= {closed_at}"
    );
    release(&a, "k1", 5);
}

/// **LEG 6, THE LIVE LEGS PHASE 5 COULD NOT RUN (6C-3, 6H-1): A MANAGER'S TASK.** A manager
/// session M's `kind=task` to S links S's next turn (its `brief` line ends `mail=@<off>~`), and
/// that turn, a `turn` exchange, is placed by the drive's ledger from the log by default.
#[test]
fn a_managers_task_links_the_next_turn_and_the_ledger_places_it() {
    let (_owner, a) = live_world();
    write_fake(&a, "resets in 1m");
    let s = spawn_tab(&a);
    let at = cursor(&a, &s);
    start_fake(
        &a,
        &s,
        "k1",
        &["busy.gate", "idle.type", "busy3.gate", "idle3.gate"],
    );
    await_log(&a, &s, "open=turn", &at);
    let at = cursor(&a, &s);
    release(&a, "k1", 1);
    await_log(&a, &s, "close=turn", &at);
    let m = spawn_tab(&a);
    let r = a.verb(&format!("@{m} post to=@{s} kind=task carry the work"));
    assert!(r.ok(), "M's task: {}", r.header());
    let off = harness::off_of(r.header());
    until_on(&a, &s, DEADLINE, "S's task mail", || {
        a.text(&format!("@{s} log kinds=mail"))
            .lines()
            .any(|l| {
                l.contains(" mail dir=in kind=task ") && kv(l, "off") == Some(&off.to_string())
            })
            .then_some(())
    });
    let at = cursor(&a, &s);
    std::thread::scope(|scope| {
        let (sock, token, sid) = (&a.sock, &a.token, &s);
        let turn = scope.spawn(move || {
            let mut c = connect_within(sock, token, DEADLINE).expect("a connection");
            let r = c
                .request(&format!(
                    "@{sid} turn idle=200 timeout=60000 settle=submitted next step"
                ))
                .expect("the turn");
            r.header().to_string()
        });
        await_log(&a, &s, "open=turn", &at);
        release(&a, "k1", 3);
        let header = turn.join().expect("the turn's thread");
        assert!(header.starts_with("OK "), "the turn: {header}");
    });
    let brief = until_on(&a, &s, DEADLINE, "the task's turn to close", || {
        let b = a.text(&format!("@{s} brief"));
        b.lines()
            .any(|l| l.starts_with("t2 ") && !l.contains(" open "))
            .then_some(b)
    });
    let t2 = brief
        .lines()
        .find(|l| l.starts_with("t2 "))
        .expect("the task's turn");
    assert!(t2.contains(" by=ctl ledger="), "{brief}");
    // The row's words end with the link; its evidence tag (` [screen+]`) follows them.
    let words = t2.rsplit_once(" [").map_or(t2, |(words, _)| words);
    assert!(words.ends_with(&format!(" mail=@{off}~")), "{brief}");
    let (code, ledger, err) = cli(&a, &["drive", "ledger", &format!("@{s}")]);
    assert_eq!(code, 0, "drive ledger: {ledger}\n{err}");
    assert!(
        ledger.contains(" log class=turn: 1 of 1 turn(s) placed by ledger="),
        "{ledger}"
    );
    release(&a, "k1", 4);
}

/// The conversation the relaunch stand-in keeps.
const CONVERSATION: &str = "0badf00d-1111-2222-3333-444455556666";

/// The relaunch stand-in's idle screen: a Claude Code banner over an empty composer (as
/// `host_live`'s `bypass_start`).
fn idle_composer() -> String {
    let banner: Vec<String> = body(BOX_TOUCH)
        .lines()
        .take(4)
        .map(str::to_string)
        .collect();
    let offer = body(OFFER);
    let composer: Vec<&str> = offer.lines().rev().take(4).collect();
    let mut rows = banner;
    rows.push(String::new());
    rows.extend(composer.into_iter().rev().map(str::to_string));
    rows.join("\n") + "\n"
}

/// The stand-in a relaunch restarts (after `host_live`'s): it logs each launch's pid and
/// arguments, keeps a conversation (`--resume <id>` picks it) with its session file where Claude
/// Code keeps one, draws an idle composer, logs what it reads, kills itself by SIGKILL once the
/// scratch `die` file appears, and exits 0 (a clean `/exit`) once the scratch `quit` file does.
/// While the scratch `hold-register` file exists, a launch that resumes (`--resume`) writes its
/// session file, and draws its composer, only once the scratch `register` file appears: an agent
/// that registers its conversation late. It runs on `interpreter`.
fn write_relaunch_fake(inst: &Inst, interpreter: Interpreter) {
    let fake = fake_path(inst);
    for d in [
        fake.parent().expect("agents/"),
        &inst.root.join("sc"),
        &inst.root.join("work"),
    ] {
        std::fs::create_dir_all(d).expect("mkdir");
    }
    std::fs::write(inst.root.join("sc/idle.txt"), idle_composer()).expect("idle screen");
    let interpreter = match interpreter {
        Interpreter::Own => harness::own_image(&inst.root, "bash"),
        Interpreter::Platform => PathBuf::from("/bin/sh"),
    };
    let text = format!(
        "#!{}\n\
         case \"$1\" in --version) echo '2.1.281 (Claude Code)'; exit 0 ;; esac\n\
         tmp=${{HOME%/home}}\n\
         printf '%s\\n' \"$PPID\" > \"$tmp/relaunch-parent\"\n\
         printf '%s\\000' \"$@\" > \"$tmp/launch-$$.argv\"\n\
         echo \"$$ $*\" >> \"$tmp/launches.log\"\n\
         stty -icanon -echo min 0 time 2 2>/dev/null\n\
         sid={CONVERSATION}; prev=\n\
         for a in \"$@\"; do [ \"$prev\" = --resume ] && sid=$a; prev=$a; done\n\
         start=$(LC_ALL=C TZ=UTC ps -o lstart= -p $$ | awk '{{$1=$1; print}}')\n\
         case \" $* \" in *' --resume '*) [ -e \"$tmp/hold-register\" ] && \
         while [ ! -e \"$tmp/register\" ]; do sleep 0.1; done ;; esac\n\
         mkdir -p \"$HOME/.claude/sessions\" \"$HOME/.claude/projects/-w\"\n\
         printf '{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"hi\"}}}}\\n' \
         >> \"$HOME/.claude/projects/-w/$sid.jsonl\"\n\
         printf '{{\"pid\":%s,\"sessionId\":\"%s\",\"cwd\":\"%s\",\"version\":\"2.1.281\",\
         \"status\":\"idle\",\"statusUpdatedAt\":%s000,\"procStart\":\"%s\",\
         \"kind\":\"interactive\",\"entrypoint\":\"cli\"}}' \
         $$ \"$sid\" \"$PWD\" \"$(date +%s)\" \"$start\" > \"$HOME/.claude/sessions/$$.json\"\n\
         printf '\\033[?1049h\\033[?1004h\\033[2J\\033[H'\n\
         cat \"$tmp/sc/idle.txt\"\n\
         r=$(grep -n '^❯' \"$tmp/sc/idle.txt\" | tail -n 1 | cut -d: -f1); [ -n \"$r\" ] && printf '\\033[%s;3H' \"$r\"\n\
         cr=$(printf '\\r'); nl=$(printf '\\n.'); nl=${{nl%.}}; esc=$(printf '\\033')\n\
         while :; do\n\
         \x20 if [ -e \"$tmp/die\" ]; then rm -f \"$tmp/die\"; kill -KILL $$; fi\n\
         \x20 if [ -e \"$tmp/quit\" ]; then rm -f \"$tmp/quit\"; exit 0; fi\n\
         \x20 got=$(dd bs=4096 count=1 2>/dev/null; echo .); got=${{got%.}}\n\
         \x20 [ -n \"$got\" ] && printf '%s' \"$got\" >> \"$tmp/typed-$$.log\"\n\
         \x20 case \"$got\" in\n\
         \x20 '') ;;\n\
         \x20 *\"$cr\"*|*\"$nl\"*) printf '\\033[2J\\033[H'; cat \"$tmp/sc/idle.txt\"; printf '\\033[%s;3H' \"$r\" ;;\n\
         \x20 \"$esc\"*) ;;\n\
         \x20 *) printf '\\033[%s;3H\\033[K[Pasted text #1]' \"$r\" ;;\n\
         \x20 esac\n\
         done\n",
        interpreter.display()
    );
    std::fs::write(&fake, text).expect("the stand-in");
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    aterm_spec::wait::first_exec(&fake, &["--version"]);
}

/// Each launch the relaunch stand-in logged: its pid and its arguments.
fn launches(inst: &Inst) -> Vec<(u32, String)> {
    std::fs::read_to_string(inst.root.join("launches.log"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (pid, args) = l.split_once(' ').unwrap_or((l, ""));
            Some((pid.parse().ok()?, args.to_string()))
        })
        .collect()
}

/// Whether `inst`'s aterm can name itself in the usage tap's status-line command
/// (`aterm_agent::harness::tap::stable_exe`, judged on the image this world launches with the
/// temporary directory the instance runs with): a lasting path, neither under a temporary
/// directory nor under a `CACHEDIR.TAG` folder, that the command can spell. A build Cargo left in
/// its target directory (which Cargo tags `CACHEDIR.TAG`) is not lasting, so the relaunch rightly
/// carries no tap; an image at a lasting path (`ATERM_TEST_EXECUTABLE`) does.
fn tap_named(inst: &Inst) -> bool {
    let exe = Path::new(harness::aterm());
    let roots = aterm_toolchain::lasting::temp_roots_from(inst.root.join("tmp"));
    !aterm_toolchain::lasting::transient(exe, &roots)
        && aterm_agent::harness::tap::exe_admissible(exe)
}

/// Check the actual argument boundaries: the relaunch resumes the saved model and conversation
/// with no other argument but, exactly where the instance's aterm can name itself in the tap
/// ([`tap_named`]), one intact managed status-line `--settings` JSON document.
fn assert_stored_resume_arguments(inst: &Inst, pid: u32) {
    let encoded = std::fs::read_to_string(inst.root.join(format!("launch-{pid}.argv")))
        .expect("the launch wrote its argv before announcing itself");
    assert!(encoded.ends_with('\0'), "complete argument record");
    let mut args: Vec<&str> = encoded.split_terminator('\0').collect();
    let settings_at = args.iter().position(|arg| *arg == "--settings");
    let named = tap_named(inst);
    eprintln!("the aterm under test names the tap: {named}; the relaunch's argv: {args:?}");
    if !named {
        assert_eq!(
            settings_at, None,
            "an aterm at a transient path names no tap: {args:?}"
        );
        assert_eq!(args, ["--model", "opus", "--resume", CONVERSATION]);
        return;
    }
    let settings_at = settings_at.expect("stored relaunch keeps the managed status line");
    assert_eq!(args.iter().filter(|arg| **arg == "--settings").count(), 1);
    let settings: aterm_json::Value = aterm_json::from_str(args[settings_at + 1])
        .expect("the settings argument is an intact JSON document");
    let settings = settings.as_object().expect("settings object");
    assert_eq!(settings.len(), 1, "only managed status settings were added");
    let status = settings.get("statusLine").expect("managed status line");
    assert_eq!(status["type"].as_str(), Some("command"));
    assert!(
        status["command"]
            .as_str()
            .is_some_and(|command| !command.is_empty())
    );
    args.drain(settings_at..=settings_at + 1);
    assert_eq!(args, ["--model", "opus", "--resume", CONVERSATION]);
}

/// Every `aterm.log` line under `inst`'s scratch (the harness's own log among them).
fn host_log(inst: &Inst) -> String {
    fn walk(dir: &Path, out: &mut String) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.file_name().is_some_and(|n| n == "aterm.log") {
                out.push_str(&std::fs::read_to_string(&p).unwrap_or_default());
            }
        }
    }
    let mut out = String::new();
    walk(&inst.root, &mut out);
    out
}

/// `sid`'s `program=` is `want`, within the hang detector.
fn program_is(inst: &Inst, sid: &str, want: &str) {
    until_on(inst, sid, DEADLINE, &format!("program={want}"), || {
        (kv(&status_of(inst, sid), "program") == Some(want)).then_some(())
    });
}

/// The zsh legs 6 run on ([`aterm_spec::host::need_tool`]): `/bin/zsh` on macOS, its login
/// shell; elsewhere the host's own, and a host with none ends the test COULD NOT RUN.
fn zsh() -> String {
    aterm_spec::host::need_tool("zsh")
        .to_string_lossy()
        .into_owned()
}

/// **LEG 6 (6C-1, 6H-6): THE RELAUNCH**, on `shell` (its rc file `rc`, made empty): a
/// harness-hosted stand-in, started in the shell's own directory or (`elsewhere`) in a
/// subdirectory the shell is not in, dies by SIGKILL unasked; the harness relaunches it on its
/// conversation and types its continuation; the failure it caused is lifted (`seen` by the
/// harness), the brief counts its two acts, and the tab menu says `now: restarting · … · by the
/// harness`. With `late`, the relaunched agent registers its conversation only after the
/// relaunch's own step gave up waiting for it (`step=wait:resume`) and the tab's loop took the
/// agent over (`supervising`), so no retry of that step can see it: the loop's carry-on adopts it.
/// The relaunch's line, as the shell's integration recorded its block, is returned.
fn relaunch_leg(shell: &str, rc: Option<&str>, elsewhere: bool, late: bool) -> String {
    let (_owner, a, s) = hosted_stand_in(shell, rc, elsewhere);
    if late {
        std::fs::write(a.root.join("hold-register"), "").expect("the late registration");
    }
    std::fs::write(a.root.join("die"), "").expect("the crash");
    let got = until_on(&a, &s, DEADLINE, "the relaunch", || {
        let got = launches(&a);
        (got.len() >= 2).then_some(got)
    });
    if late {
        // The relaunch's own row gave up (`relaunch … step=wait:resume`), and only then did the
        // tab's loop take the relaunched agent over, which ends that relaunch's worker.
        let (row, supervising) = (
            format!("harness @{s}: relaunch "),
            format!("harness @{s}: supervising"),
        );
        until_on(
            &a,
            &s,
            DEADLINE,
            "the loop over the relaunched agent, after its relaunch gave up waiting",
            || {
                let log = host_log(&a);
                let mut lines = log.lines();
                lines.find(|l| l.contains(&row) && l.ends_with(" step=wait:resume"))?;
                lines.any(|l| l.contains(&supervising)).then_some(())
            },
        );
        let early = match std::fs::read(a.root.join(format!("typed-{}.log", got[1].0))) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => panic!("the pre-registration input journal: {error}"),
        };
        assert!(
            early.is_empty(),
            "no continuation reaches the agent before it registers: {early:?}"
        );
        std::fs::write(a.root.join("register"), "").expect("the registration");
    }
    assert_eq!(got.len(), 2, "relaunched once: {got:?}");
    assert_eq!(
        got[1].1,
        format!("--model opus --resume {CONVERSATION}"),
        "{got:?}"
    );
    let label = until_on(&a, &s, DEADLINE, "the harness's restarting label", || {
        clause(&a, &s).filter(|c| c.starts_with("now: restarting"))
    });
    assert!(label.contains(" \u{b7} by the harness"), "{label}");
    let new = got[1].0;
    until_on(
        &a,
        &s,
        DEADLINE,
        "the continuation typed into the relaunched agent",
        || {
            std::fs::read_to_string(a.root.join(format!("typed-{new}.log")))
                .ok()
                .filter(|t| t.ends_with(['\r', '\n']))
        },
    );
    let log = a.text(&format!("@{s} log tier=host n=400"));
    assert!(
        log.lines()
            .any(|l| l.contains(" mark - seen ") && l.contains(" by=harness ")),
        "{log}"
    );
    let brief = until_on(&a, &s, DEADLINE, "the brief to count both acts", || {
        let b = a.text(&format!("@{s} brief"));
        b.lines().any(|l| l.starts_with("acts 2 ")).then_some(b)
    });
    assert!(!brief.contains("need=failure"), "{brief}");
    let blocks = a.text(&format!("@{s} blocks 8 --json"));
    let (_, body) = blocks
        .split_once('\n')
        .expect("the blocks reply has a body");
    let blocks: aterm_json::Value = aterm_json::from_str(body.trim()).expect("the blocks JSON");
    let quote = |word: &str| {
        aterm_agent::harness::upgrade::quote(aterm_agent::harness::upgrade::Dialect::Bash, word)
    };
    // The native route's shell block names the owned physical helper (`Image::at`'s canonical
    // path, which `harness::aterm()` is too), not the vendor command it obtains from the
    // issuer. The actual resumed PID/argv were checked above.
    let helper = format!(
        "{} {} {} {}",
        quote(harness::aterm()),
        quote("harness"),
        quote("native-launch"),
        quote("contract=1")
    );
    let session = quote(&format!("session={s}"));
    let phase = quote("phase=forward");
    blocks["blocks"]
        .as_array()
        .expect("the command blocks")
        .iter()
        .filter_map(|block| block["cmdline"].as_str())
        .find(|line| {
            line.contains("--resume")
                || line.trim_start().starts_with("builtin :")
                || (line.contains(&helper) && line.contains(&session) && line.contains(&phase))
        })
        .map(str::to_owned)
        .unwrap_or_else(|| {
            panic!(
                "a block attributes the direct relaunch or its exact owned forward helper \
                 ({helper} … {session} … {phase}): {body}"
            )
        })
}

/// Legs 6's world: on `shell` (its rc file `rc`, made empty), a tab whose shell runs aterm's
/// integration, with the relaunch stand-in typed into it (in the shell's own directory, or
/// `elsewhere`: a subdirectory the shell is not in), the harness hosting it and at its first idle
/// point.
fn hosted_stand_in(shell: &str, rc: Option<&str>, elsewhere: bool) -> (Owner, Inst, String) {
    hosted_stand_in_as(shell, rc, elsewhere, false, Interpreter::Own)
}

/// What the relaunch stand-in runs on ([`write_relaunch_fake`]).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Interpreter {
    /// The owned native Claude fixture on Apple Silicon macOS; elsewhere a script on an
    /// ad-hoc signed copy of `/bin/bash` ([`harness::own_image`]).
    Own,
    /// The system's own `/bin/sh`: on macOS an Apple binary, like `/bin/zsh`, whose environment
    /// macOS shows no other process.
    Platform,
}

/// Whether process `pid`'s exec environment can be read by another process.
fn env_readable(pid: u32) -> bool {
    atpkg::caller_shell::process_args(pid).is_some_and(|args| !args.env.is_empty())
}

/// The relaunch stand-in's first shell: its parent, as it wrote it.
fn relaunch_parent(a: &Inst) -> u32 {
    std::fs::read_to_string(a.root.join("relaunch-parent"))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

/// Give success fixtures a readable shell exec environment with their unmanaged
/// route already present. The stored-login refusal case retains the platform shell.
fn relaunch_fixture_shell(root: &Path, shell: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let name = Path::new(shell).file_name().expect("a shell name");
    let dir = root.join("relaunch-shell");
    let image = dir.join("image").join(name);
    let entry = dir.join("bin").join(name);
    std::fs::create_dir_all(image.parent().unwrap()).expect("the shell image folder");
    std::fs::create_dir_all(entry.parent().unwrap()).expect("the shell entry folder");
    aterm_uds::deadline::ad_hoc_copy(Path::new(shell), &image);
    let quoted = aterm_agent::harness::upgrade::quote(
        aterm_agent::harness::upgrade::Dialect::Bash,
        image.to_str().expect("the shell image path"),
    );
    std::fs::write(
        &entry,
        format!("#!/bin/sh\nexport ANTHROPIC_API_KEY=aterm-fabric-standin\nexec {quoted} \"$@\"\n"),
    )
    .expect("the shell entry");
    std::fs::set_permissions(&entry, std::fs::Permissions::from_mode(0o755))
        .expect("executable shell entry");
    entry
}

fn hosted_stand_in_as(
    shell: &str,
    rc: Option<&str>,
    elsewhere: bool,
    on_own_login: bool,
    interpreter: Interpreter,
) -> (Owner, Inst, String) {
    let owner = Owner::boot();
    // Its tabs' shell runs aterm's shell integration, so a command's end says its exit code, and
    // the relaunch's line opens a command of its own.
    let shell = if on_own_login {
        PathBuf::from(shell)
    } else {
        relaunch_fixture_shell(&owner.tmp, shell)
    };
    let opts = Opts {
        shell: Some(shell.to_string_lossy().into_owned()),
        ..Opts::default()
    };
    let a = Inst::boot(&owner, "a", NODE_A, opts);
    a.wait_ready(&owner);
    if let Some(rc) = rc {
        std::fs::write(a.root.join("home").join(rc), "").expect("an rc file");
    }
    if interpreter == Interpreter::Own {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        crate::native_claude::place(&a.root, &fake_path(&a), &idle_composer());
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        write_relaunch_fake(&a, interpreter);
    } else {
        write_relaunch_fake(&a, interpreter);
    }
    let s = spawn_tab(&a);
    until_on(&a, &s, DEADLINE, "the shell's integration", || {
        (kv(&status_of(&a, &s), "integration") == Some("on")).then_some(())
    });
    let work = a.root.join("work");
    let fake = fake_path(&a);
    let line = if elsewhere {
        std::fs::create_dir_all(work.join("sub")).expect("a subdirectory");
        format!(
            "cd {} && (cd sub && exec '{}' --model opus)",
            work.display(),
            fake.display()
        )
    } else {
        format!("cd {} && '{}' --model opus", work.display(), fake.display())
    };
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    let line = if interpreter == Interpreter::Own {
        let root = aterm_agent::harness::upgrade::quote(
            aterm_agent::harness::upgrade::Dialect::Zsh,
            &a.root.to_string_lossy(),
        );
        format!("export ATERM_CLAUDE_FIXTURE_ROOT={root}; {line}")
    } else {
        line
    };
    // These provider-free fixtures model an explicit unmanaged credential route.
    // Export only inside this private shell, so resumed stand-ins keep that route.
    let line = if on_own_login {
        line
    } else {
        format!("export ANTHROPIC_API_KEY=aterm-fabric-standin; {line}")
    };
    type_line(&a, &s, &line);
    program_is(&a, &s, "claude");
    until_on(&a, &s, DEADLINE, "the host attached", || {
        kv(&status_of(&a, &s), "supervisor")
            .is_some_and(|v| v.starts_with("aterm-harness"))
            .then_some(())
    });
    until_on(
        &a,
        &s,
        DEADLINE,
        "the harness at the agent's first idle point",
        || host_log(&a).contains("EVENT idle").then_some(()),
    );
    let pid = launches(&a).first().expect("the native launch").0;
    let args = atpkg::caller_shell::process_args(pid).expect("the stand-in's exec context");
    let observed = aterm_agent::harness::accounts::ProcessLogin::of_exec_env(&args.env)
        .map(|who| who.route == aterm_agent::harness::accounts::AuthRoute::Stored);
    assert!(
        observed == Some(on_own_login)
            || (interpreter == Interpreter::Platform && observed.is_none()),
        "fixture authentication attribution: {observed:?}"
    );
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    if interpreter == Interpreter::Own {
        until_on(
            &a,
            &s,
            DEADLINE,
            "the live native watch acknowledgement",
            || crate::native_claude::observed(&a.root, &s, pid).then_some(()),
        );
    }
    (owner, a, s)
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
struct ColdNative {
    // Retire the instance and its children before removing its owner's private world.
    a: Inst,
    _owner: Owner,
    sid: String,
    prefix: PathBuf,
    twin: PathBuf,
    build: PathBuf,
    next_build: PathBuf,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn cold_native_settled() -> ColdNative {
    use std::os::unix::fs::PermissionsExt as _;
    let owner = Owner::boot();
    let root = owner.tmp.join("a");
    let prefix = atpkg::store::default_prefix(&root.join("home"));
    let twin = prefix.join("agents/claude");
    let build = prefix.join("store/claude/1000002000001000318/bin/claude");
    let next_build = prefix.join("store/claude/1000002000001000319/bin/claude");
    let foreign = root.join("foreign/claude");
    let opts = Opts {
        shell: Some(
            relaunch_fixture_shell(&owner.tmp, "/bin/zsh")
                .to_string_lossy()
                .into_owned(),
        ),
        env: vec![
            (
                aterm_agent::harness::agent_image::STANDINS_SEAM.to_string(),
                std::env::join_paths([&twin, &build, &next_build, &foreign])
                    .unwrap()
                    .into_string()
                    .unwrap(),
            ),
            ("ATERM_LOG".into(), "debug".into()),
        ],
        ..Opts::default()
    };
    let a = Inst::boot(&owner, "a", NODE_A, opts);
    a.wait_ready(&owner);
    std::fs::write(a.root.join("home/.zshrc"), "").unwrap();
    crate::native_claude::place(&a.root, &foreign, &idle_composer());
    std::fs::create_dir_all(build.parent().unwrap()).unwrap();
    std::fs::create_dir_all(twin.parent().unwrap()).unwrap();
    std::fs::write(build.with_file_name("fixture-version"), "2.1.318").unwrap();
    aterm_uds::deadline::ad_hoc_copy(&aterm_tempfile::bin_exe("aterm-claude-fixture"), &build);
    let mut proof = aterm_uds::spawnfd::command(&build)
        .arg("--version")
        .stdout(OwnedStdio::piped())
        .spawn()
        .expect("run the build in place");
    aterm_uds::deadline::prints(
        &mut proof,
        "2.1.318 (Claude Code)",
        aterm_uds::deadline::ECHO_HANG,
    );
    assert!(proof.wait().unwrap().success());
    std::fs::write(
        &twin,
        format!("#!/bin/sh\nexec '{}' \"$@\"\n", build.display()),
    )
    .unwrap();
    std::fs::set_permissions(&twin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let s = spawn_tab(&a);
    until_on(&a, &s, DEADLINE, "the shell's integration", || {
        (kv(&status_of(&a, &s), "integration") == Some("on")).then_some(())
    });
    let quoted = aterm_agent::harness::upgrade::quote(
        aterm_agent::harness::upgrade::Dialect::Zsh,
        &a.root.to_string_lossy(),
    );
    type_line(
        &a,
        &s,
        &format!(
            "export ANTHROPIC_API_KEY=aterm-fabric-standin; export ATERM_CLAUDE_FIXTURE_ROOT={quoted}; cd {} && '{}'",
            a.root.join("work").display(),
            foreign.display()
        ),
    );
    program_is(&a, &s, "claude");
    until_on(&a, &s, DEADLINE, "the host attached", || {
        kv(&status_of(&a, &s), "supervisor")
            .is_some_and(|v| v.starts_with("aterm-harness"))
            .then_some(())
    });
    until_on(&a, &s, DEADLINE, "the agent's first idle point", || {
        host_log(&a).contains("EVENT idle").then_some(())
    });
    let pid = launches(&a).first().expect("the native launch").0;
    until_on(&a, &s, DEADLINE, "the live native watch", || {
        crate::native_claude::observed(&a.root, &s, pid).then_some(())
    });
    std::fs::write(a.root.join("die"), "").unwrap();
    let resumed = until_on(&a, &s, DEADLINE, "the relaunched agent", || {
        launches(&a).get(1).cloned()
    });
    assert_eq!(resumed.1, format!("--resume {CONVERSATION}"));
    assert_eq!(
        aterm_uds::process::read_process_path(i32::try_from(resumed.0).unwrap()),
        Some(std::fs::canonicalize(&build).unwrap()),
        "the build, not the stand-in it left"
    );
    let row = format!("harness @{s}: relaunch ");
    until_on(&a, &s, DEADLINE, "the relaunch adopted its agent", || {
        host_log(&a)
            .lines()
            .any(|l| l.contains(&row) && l.ends_with(" step=adopted"))
            .then_some(())
    });
    until_on(&a, &s, DEADLINE, "the host back on the agent", || {
        kv(&status_of(&a, &s), "supervisor")
            .is_some_and(|v| v.starts_with("aterm-harness"))
            .then_some(())
    });

    ColdNative {
        a,
        _owner: owner,
        sid: s,
        prefix,
        twin,
        build,
        next_build,
    }
}

/// A crash onto a newer managed build, followed by an asked upgrade of that actual image.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn a_cold_relaunch_onto_a_newer_twin_execs_the_build_and_settles() {
    let fixture = cold_native_settled();
    let a = &fixture.a;
    let s = &fixture.sid;
    let prefix = &fixture.prefix;
    let twin = &fixture.twin;
    let next_build = &fixture.next_build;

    // The second operation is an asked upgrade of the adopted process, not another
    // cold restart or a raw signal exempted from native coordination.
    std::fs::create_dir_all(next_build.parent().unwrap()).unwrap();
    std::fs::write(next_build.with_file_name("fixture-version"), "2.1.319").unwrap();
    aterm_uds::deadline::ad_hoc_copy(&aterm_tempfile::bin_exe("aterm-claude-fixture"), next_build);
    let mut proof = aterm_uds::spawnfd::command(next_build)
        .arg("--version")
        .stdout(OwnedStdio::piped())
        .spawn()
        .expect("run the second build in place");
    aterm_uds::deadline::prints(
        &mut proof,
        "2.1.319 (Claude Code)",
        aterm_uds::deadline::ECHO_HANG,
    );
    assert!(proof.wait().unwrap().success());
    until_on(a, s, DEADLINE, "activation wake is armed", || {
        host_log(a)
            .contains("activation wake is armed")
            .then_some(())
    });
    let staged_twin = twin.with_file_name(".claude-next");
    let mut prelude = atpkg::platform::twin_prelude(
        "claude",
        prefix,
        None,
        atpkg::selfupdate::verbs_of("claude"),
        &atpkg::yolo::policy_of("claude"),
    );
    prelude.push_str(&atpkg::launch_tap::sh_prelude("claude", None));
    let exports = atpkg::shim_env::ShimEnv::admit(&[
        "DISABLE_AUTOUPDATER=1".into(),
        "CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1".into(),
    ])
    .unwrap();
    atpkg::platform::install_twin_to_env(&staged_twin, next_build, &exports, &prelude).unwrap();
    std::fs::rename(&staged_twin, twin).unwrap();
    let notices = prefix.join(atpkg::activation_notice::NOTICE_DIR);
    std::fs::create_dir_all(&notices).unwrap();
    std::fs::write(notices.join(".m.tmp"), "1\n").unwrap();
    std::fs::rename(
        notices.join(".m.tmp"),
        notices.join(atpkg::activation_notice::CLAUDE_MARKER),
    )
    .unwrap();
    let upgraded = until_on(a, s, DEADLINE, "the asked second build", || {
        launches(a).get(2).cloned()
    });
    assert_eq!(upgraded.1, format!("--resume {CONVERSATION}"));
    assert_eq!(
        aterm_uds::process::read_process_path(i32::try_from(upgraded.0).unwrap()),
        Some(std::fs::canonicalize(next_build).unwrap()),
    );
    until_on(a, s, DEADLINE, "the second build adopted", || {
        (kv(&status_of(a, s), "supervisor").is_some_and(|v| v.starts_with("aterm-harness"))
            && host_log(a).lines().any(|line| {
                line.contains("upgrade pid=")
                    && line
                        .split_whitespace()
                        .any(|word| word == format!("tab={s}"))
                    && line
                        .split_whitespace()
                        .any(|word| word == format!("session={CONVERSATION}"))
                    && line.split_whitespace().any(|word| word == "from=2.1.318")
                    && line
                        .split_whitespace()
                        .any(|word| word == "to=2.1.319(managed)")
                    && line.split_whitespace().any(|word| word == "step=adopted")
            }))
        .then_some(())
    });
    assert_eq!(launches(a).len(), 3, "one launch of each actual image");
}

/// The actual host's Memory hook must use a fresh typed relaunch after cold settlement.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn a_memory_restart_after_cold_settlement_resumes_the_same_native_build() {
    use aterm_agent::harness::native_act::{Image, Process};
    let fixture = cold_native_settled();
    let a = &fixture.a;
    let s = &fixture.sid;
    let original = launches(a)
        .get(1)
        .cloned()
        .expect("the adopted cold launch");
    let original_process = Process::observe(original.0).expect("the live adopted source");
    let expected_image = Image::at(&fixture.build).expect("the proven private native image");
    assert_eq!(original_process.image, expected_image);
    // Only this already-running process sees the memory screen. Its resumed successor
    // starts on the normal restored screen, so the control cannot request a restart loop.
    let memory = body(include_str!(
        "../../../aterm-phase/src/fixtures/memory-banner-idle.txt"
    ));
    std::fs::write(a.root.join("sc/restored.txt"), idle_composer()).unwrap();
    std::fs::write(a.root.join("sc/idle.txt"), memory).unwrap();
    // Empty Enter redraws the native fixture; it creates no assistant response/model fact.
    let redrawn = a.verb(&format!("@{s} key enter"));
    assert!(redrawn.ok(), "empty Enter: {}", redrawn.header());
    let resumed = until_on(a, s, DEADLINE, "the checked Memory successor", || {
        launches(a).get(2).cloned()
    });
    assert_ne!(resumed.0, original.0);
    assert_eq!(resumed.1, format!("--resume {CONVERSATION}"));
    let actual = until_on(
        a,
        s,
        DEADLINE,
        "the Memory successor's exact native image",
        || Process::observe(resumed.0).filter(|process| process.image == expected_image),
    );
    assert_ne!(actual.birth, original_process.birth);
    let row = format!("harness @{s}: restart:memory ");
    let session_field = format!("session={CONVERSATION}");
    until_on(a, s, DEADLINE, "the real Memory restart adopted", || {
        let log = host_log(a);
        (log.contains("asked native prepare result=admitted")
            && log.lines().any(|line| {
                line.contains(&row)
                    && line.split_whitespace().any(|word| word == session_field)
                    && line.split_whitespace().any(|word| word == "from=2.1.318")
                    && line
                        .split_whitespace()
                        .any(|word| word == "to=2.1.318(same)")
                    && line.split_whitespace().any(|word| word == "step=adopted")
            }))
        .then_some(())
    });
    assert_eq!(
        launches(a).len(),
        3,
        "initial, cold and same-build Memory launches only"
    );
}

/// A stored route is admitted from the owned helper, without a parent-kernel environment prerequisite.
#[test]
fn a_stored_login_relaunch_uses_the_owned_helpers_context() {
    let (_owner, a, s) =
        hosted_stand_in_as("/bin/zsh", Some(".zshrc"), false, true, Interpreter::Own);
    let first = launches(&a);
    assert_eq!(first.len(), 1, "one initial stored-login agent: {first:?}");
    std::fs::write(a.root.join("die"), "").unwrap();
    let got = until_on(
        &a,
        &s,
        DEADLINE,
        "the admitted stored-login relaunch",
        || {
            let got = launches(&a);
            (got.len() >= 2).then_some(got)
        },
    );
    assert_eq!(got.len(), 2, "relaunched once: {got:?}");
    assert_ne!(got[0].0, got[1].0, "the helper exec produced a new agent");
    assert_eq!(got[0], first[0], "the original launch is retained");
    assert_stored_resume_arguments(&a, got[1].0);
    let resumed = a
        .root
        .join("home/.claude/sessions")
        .join(format!("{}.json", got[1].0));
    until_on(
        &a,
        &s,
        DEADLINE,
        "the resumed body's conversation registration",
        || {
            let text = std::fs::read_to_string(&resumed).ok()?;
            let row: aterm_json::Value = aterm_json::from_str(&text).ok()?;
            (row["pid"].as_u64() == Some(u64::from(got[1].0))
                && row["sessionId"].as_str() == Some(CONVERSATION))
            .then_some(())
        },
    );
    assert_eq!(
        launches(&a).len(),
        2,
        "registration creates no extra launch"
    );
    assert!(
        a.root
            .join("home/.claude/projects/-w")
            .join(format!("{CONVERSATION}.jsonl"))
            .is_file()
    );
}

/// An unobservable platform-script agent retains its conversation without relaunching.
/// The native lane requires crash observation before account admission; the legacy lane
/// requires a readable launch environment.
#[test]
fn a_stored_login_relaunch_keeps_its_wait_when_the_shell_context_is_unavailable() {
    let (_owner, a, s) = hosted_stand_in_as(
        "/bin/zsh",
        Some(".zshrc"),
        false,
        true,
        Interpreter::Platform,
    );
    let agent = launches(&a).first().expect("the native launch").0;
    let evidence = env_readable(relaunch_parent(&a)) || env_readable(agent);
    eprintln!("an environment to admit on: {evidence}");
    std::fs::write(a.root.join("die"), "").unwrap();
    if evidence {
        until_on(
            &a,
            &s,
            DEADLINE,
            "the admitted stored-login relaunch",
            || (launches(&a).len() == 2).then_some(()),
        );
    } else {
        let step = format!("harness @{s}: relaunch ");
        let refusal = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            " step=wait:native-crash-observation"
        } else {
            " step=wait:account"
        };
        until_on(&a, &s, DEADLINE, "two refused admission attempts", || {
            (host_log(&a)
                .lines()
                .filter(|line| line.contains(&step) && line.ends_with(refusal))
                .count()
                >= 2)
                .then_some(())
        });
        assert_eq!(launches(&a).len(), 1, "no execution without evidence");
        assert!(
            a.root
                .join("home/.claude/projects/-w")
                .join(format!("{CONVERSATION}.jsonl"))
                .is_file()
        );
    }
}

/// **LEG 6, THE RELAUNCH ON zsh** (macOS's login shell), the agent in the shell's own directory:
/// the session file spells it as the agent's logical `$PWD` (`/tmp/…`), the kernel as its real
/// path (`/private/tmp/…`), one directory, so the relaunch's line changes no directory.
#[test]
fn a_relaunch_lifts_the_failure_it_caused_and_says_so() {
    let line = relaunch_leg(&zsh(), Some(".zshrc"), false, false);
    eprintln!("zsh relaunch line: {line}");
    assert!(
        !line.contains("cd --"),
        "one directory under two spellings: {line}"
    );
}

/// **LEG 6, THE RELAUNCH ON macOS's /bin/bash (3.2)**, the agent in a subdirectory the shell is
/// not in, so the relaunch's line changes directory in a subshell: it opens with a simple command,
/// which bash's DEBUG trap reports (bash 3.2 has no PS0 hook), so the relaunch's run opens and the
/// failure it follows is lifted.
#[test]
fn on_bash_a_relaunch_into_another_directory_lifts_the_failure_too() {
    let line = relaunch_leg("/bin/bash", None, true, false);
    eprintln!("bash relaunch line: {line}");
    // The block holds the whole line where bash saved it to history, else the simple command
    // the trap saw (`$BASH_COMMAND`).
    assert!(
        line.trim_start().starts_with("builtin :"),
        "the relaunch's line: {line}"
    );
}

/// **LEG 6R (fix E2; E2E-2, E2E-17): AN ASKED RESTART PAGES NOTHING AND SAYS WHO ASKED**, on zsh,
/// in leg 6's world: `@<s> restart` (a `ctl` client's ask) in place of the crash. The act writes
/// `act word=restart by=ctl` before its `SIGTERM`, the command it ended closes `out=signal
/// code=143 by=ctl` with no `failure` (so no `seen` lifts one), the harness relaunches it once
/// on its conversation with its `restarting` label, and a need watch over the bus pages nothing.
/// Then the relaunched agent exits 0 (`/exit`): its run reads `ok`.
#[test]
fn an_asked_restart_pages_nothing_and_says_who_asked() {
    let (owner, a, s) = hosted_stand_in(&zsh(), Some(".zshrc"), false);
    let first = launches(&a).first().map(|l| l.0).expect("the first launch");
    let reply = a.text(&format!("@{s} restart"));
    assert!(
        reply.starts_with(&format!("OK restarting pid {first} cause=asked")),
        "{reply}"
    );
    let got = until_on(&a, &s, DEADLINE, "the relaunch", || {
        let got = launches(&a);
        (got.len() >= 2).then_some(got)
    });
    assert_eq!(got.len(), 2, "relaunched once: {got:?}");
    assert_eq!(
        got[1].1,
        format!("--model opus --resume {CONVERSATION}"),
        "{got:?}"
    );
    let label = until_on(&a, &s, DEADLINE, "the harness's restarting label", || {
        clause(&a, &s).filter(|c| c.starts_with("now: restarting"))
    });
    let ended = |l: &str| l.contains(" close ") && l.contains(" cmd ") && l.contains(" code=143 ");
    let log = until_on(&a, &s, DEADLINE, "the restarted command's close", || {
        let log = a.text(&format!("@{s} log tier=host n=400"));
        log.lines().any(ended).then_some(log)
    });
    let brief = a.text(&format!("@{s} brief"));
    // The need watch over the bus, once the mirror has carried that close.
    let ev = format!("/f/{FLEET}/pub/{NODE_A}/*/ev");
    until_on(&a, &s, DEADLINE, "the mirror to carry the close", || {
        bus_bodies(&owner, &ev)
            .iter()
            .any(|b| b.contains(" op=close ") && b.contains(" k=cmd ") && b.contains(" code=143 "))
            .then_some(())
    });
    let cap = a.root.join("watch.cap");
    write_cap(&cap, cap_line(&format!("ro:/f/{FLEET}/pub/>")));
    let state = a.root.join("watch");
    std::fs::create_dir_all(&state).expect("the watch's state");
    let fired = a.root.join("fired.log");
    let exec = format!(
        "printf '%s %s %s\\n' \"$ATERM_NOTIFY_SID\" \"$ATERM_NOTIFY_NEED\" \"$ATERM_NOTIFY_DETAIL\" >> '{}'",
        fired.display()
    );
    let (code, out, err) = cli(
        &a,
        &[
            "link",
            "notify",
            "--fleet",
            FLEET,
            "--broker",
            &owner.sock,
            "--cap-file",
            &cap.to_string_lossy(),
            "--state",
            &state.to_string_lossy(),
            "--on",
            "need",
            "--exec",
            &exec,
            "--since",
            "start",
            "--once",
        ],
    );
    let pages = std::fs::read_to_string(&fired).unwrap_or_default();
    eprintln!("asked restart: notify exit {code}: {out}{err}pages: {pages:?}\nbrief: {brief}");
    assert_eq!(code, 0, "{out}{err}");
    assert!(pages.is_empty(), "an asked restart pages nothing: {pages}");
    let lines: Vec<&str> = log.lines().collect();
    let act = lines
        .iter()
        .position(|l| l.contains(" act word=restart ") && l.contains(" by=ctl "));
    let close = lines
        .iter()
        .position(|l| ended(l) && l.contains(" cmd out=signal ") && l.contains(" by=ctl "));
    assert!(
        act.is_some() && act < close,
        "the act's mark, then its command's end, by its asker: {log}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains(" agent out=signal ") && l.contains(" by=ctl ")),
        "{log}"
    );
    assert!(!log.contains(" cmd out=fail "), "{log}");
    assert!(
        !lines.iter().any(|l| l.contains(" mark - seen ")),
        "no failure, so no seen: {log}"
    );
    assert!(!brief.contains("need=failure"), "{brief}");
    assert!(label.contains(" \u{b7} by the harness"), "{label}");

    // A clean exit of the relaunched agent: its run takes its command's `ok`.
    let agent = |log: &str| -> Option<String> {
        log.lines()
            .rfind(|l| l.contains(" open ") && l.contains(" agent "))
            .and_then(|l| l.split(' ').nth(2))
            .map(str::to_string)
    };
    let new = until_on(&a, &s, DEADLINE, "the relaunched agent's run", || {
        let log = a.text(&format!("@{s} log tier=host n=400"));
        agent(&log).filter(|n| !log.contains(&format!(" close {n} ")))
    });
    std::fs::write(a.root.join("quit"), "").expect("a clean exit");
    let log = until_on(&a, &s, DEADLINE, "the clean exit's close", || {
        let log = a.text(&format!("@{s} log tier=host n=400"));
        log.lines()
            .any(|l| l.contains(" cmd out=ok ") && l.contains(" code=0 "))
            .then_some(log)
    });
    assert!(
        log.lines().any(|l| {
            l.contains(&format!(" close {new} agent out=ok "))
                || l.contains(&format!(" amend {new} agent out=ok "))
        }),
        "the agent that exited 0 reads ok: {log}"
    );
}

/// **LEG 6, A LATE REGISTRATION**, on zsh: the relaunched agent registers its conversation only
/// after the relaunch's own step gave up waiting for it and the tab's loop took the agent over, so
/// the loop's carry-on is the first to see the new agent; it adopts it as the relaunch would have,
/// so the failure is lifted and the restart labelled all the same.
#[test]
fn a_relaunch_whose_agent_registers_after_its_row_gave_up_still_lifts_the_failure() {
    let line = relaunch_leg(&zsh(), Some(".zshrc"), false, true);
    eprintln!("late zsh relaunch line: {line}");
}

/// The sentinels (`QZP6SECRET<n>`, any case, `<n>` its whole run of digits) `bytes` holds, by
/// their number.
fn sentinels(bytes: &[u8]) -> Vec<u8> {
    let low = bytes.to_ascii_lowercase();
    let needle = b"qzp6secret";
    let mut out: Vec<u8> = Vec::new();
    let mut i = 0;
    while let Some(at) = low[i..].windows(needle.len()).position(|w| w == needle) {
        let digits = &low[i + at + needle.len()..];
        let len = digits.iter().take_while(|b| b.is_ascii_digit()).count();
        if len > 0 {
            let n = digits[..len].iter().fold(0u32, |n, d| {
                n.saturating_mul(10).saturating_add(u32::from(d - b'0'))
            });
            out.push(u8::try_from(n).unwrap_or(u8::MAX));
        }
        i += at + needle.len();
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// What the privacy sweep found: every place a sentinel was, and those the design does not keep.
#[derive(Default)]
struct Sweep {
    found: Vec<String>,
    leaks: Vec<String>,
}

impl Sweep {
    /// `bytes`, read at `place`, may hold only the sentinels `allowed`.
    fn read(&mut self, place: &str, bytes: &[u8], allowed: &[u8]) {
        let held = sentinels(bytes);
        if held.is_empty() {
            return;
        }
        self.found.push(format!("{place}: {held:?}"));
        let out: Vec<u8> = held.into_iter().filter(|n| !allowed.contains(n)).collect();
        if !out.is_empty() {
            self.leaks.push(format!("{place}: {out:?}"));
        }
    }

    /// Every file under `dir`, each held to what `allowed` says of its path relative to `dir`.
    fn files(&mut self, name: &str, dir: &Path, allowed: &dyn Fn(&Path) -> Vec<u8>) {
        fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
            for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let p = e.path();
                match e.file_type() {
                    Ok(t) if t.is_dir() => walk(&p, out),
                    Ok(t) if t.is_file() => out.push(p),
                    _ => {}
                }
            }
        }
        let mut files = Vec::new();
        walk(dir, &mut files);
        files.sort();
        for path in files {
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let rel = path.strip_prefix(dir).unwrap_or(&path);
            self.read(&format!("{name}/{}", rel.display()), &bytes, &allowed(rel));
        }
    }
}

/// The restore store a quit leaves at rest, relative to an instance's root: `session.toml` and
/// the crash journal's files (`journal-<pid>-<nanos>.toml`) beside it in the state root.
fn restore_store(rel: &Path) -> bool {
    let Some(name) = rel.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    // A journal's write in flight is the same store: `crash_journal` creates its temporary
    // (`.journal-<run>.w-<seed>.tmp`, 0600, beside the journal) and renames it over
    // `journal-<run>.toml`, so a sweep that reads the folder while a live instance writes finds
    // the journal's own sentinels in it.
    let in_flight = name
        .strip_prefix(".journal-")
        .and_then(|rest| rest.strip_suffix(".tmp"))
        .is_some_and(|rest| rest.contains(".w-"));
    rel.parent() == Some(Path::new("astate"))
        && (name == "session.toml"
            || (name.starts_with("journal-") && name.ends_with(".toml"))
            || in_flight)
}

/// Where the design keeps each sentinel on disk, by a file's path relative to an instance's root
/// (or the broker's state dir, `b.log`). The journals (0600) keep the post's key and hop as
/// `idem=` and `via=` (H); the bridge's state dir keeps the key it was handed (`link/keys/`, for
/// its dedupe) and each session's topic names (`link/topics/`, which it subscribes by), each file
/// 0600; the Messages store (0600) and stderr keep the config warning the person is shown, which
/// names the font they wrote; the restore store (0600) keeps the title, the person's own
/// attention and the standing hold's reason (B23's ruled reading of A11), and never the
/// supervisor's attention; the broker's log is the bus's own store, so a plaintext one holds the
/// bus's records (a sealed one, none of it readable; [`privacy_sweep`] runs both arms): the
/// presence row's attention (the supervisor's while it stands, the person's), and
/// the mail's own record, its body and hop (never the post's key, which stays on the node); the
/// test's own inputs hold what it wrote. Each is recorded with its reason in DESIGN-session-log
/// §10 (Phase 6, the end to end). The harness keeps the review it acted on, screen text included
/// (a reply's last line, a box's command), in its own store, the drive folder (`astate/drive`:
/// the approval ledger and the loop journal, each 0600 in a 0700 folder, local, never published;
/// DESIGN §3.6 "Outside the session log", §10 unit E4), and aterm.log keeps only each review
/// line's words; LEG 7H's stand-in draws its sentinels from its own scene files (`sc/`). An
/// aterm.toml that stops parsing (LEG 7T, 24, unit E7) is kept where the person wrote it, in the
/// Messages store's lane row and on stderr, which say the line; aterm.log names only its position
/// (`aterm_toml::Error::log_words`), as it names the refused `font_family` (7) by its length.
fn kept_on_disk(rel: &Path) -> Vec<u8> {
    if rel.starts_with("astate/drive") || rel.starts_with("sc") {
        HARNESS_REVIEW.to_vec()
    } else if rel.starts_with("astate/logs/session-log") {
        vec![8, 9]
    } else if restore_store(rel) {
        vec![2, 5, 10]
    } else if rel.starts_with("link/keys") {
        vec![8]
    } else if rel.starts_with("link/topics") {
        vec![4]
    } else if [
        "astate/logs/messages.log",
        "gui.log",
        "cfg/aterm/aterm.toml",
    ]
    .iter()
    .any(|p| rel == Path::new(p))
    {
        vec![7, UNPARSED_TOML]
    } else if rel == Path::new("marked.sh") {
        vec![1]
    } else if rel == Path::new("wrapped.sh") {
        vec![11]
    } else if rel == Path::new("array.sh") {
        vec![12]
    } else if rel == Path::new("comment.sh") {
        vec![13]
    } else if rel == Path::new("autocd.sh") {
        vec![14]
    } else if rel == Path::new("autocd_made.sh") {
        vec![15]
    } else if rel == Path::new("autocd_hook.sh") {
        vec![16]
    } else if rel == Path::new("handed.sh") {
        vec![17]
    } else if rel == Path::new("alias.sh") {
        vec![18]
    } else if rel == Path::new("standin.sh") {
        vec![19]
    } else if rel == Path::new("title.sh") {
        vec![2]
    } else if rel == Path::new("b.log") {
        vec![3, 6, 9, 10]
    } else {
        Vec::new()
    }
}

/// `subscribe @<sid> log since-log=<seq> log-ep=<ep>` on a connection of its own: what it pushes
/// from `from` until a `LOG` frame reaches `upto`, each read within the hang detector.
fn log_frames(inst: &Inst, sid: &str, from: &(String, String), upto: u64) -> String {
    use std::io::{BufRead as _, Write as _};
    let mut stream = aterm_link::ctl::dial(&inst.sock).expect("connect");
    stream
        .set_read_timeout(Some(DEADLINE))
        .expect("a read bound");
    write!(
        stream,
        "AUTH {}\nsubscribe @{sid} log since-log={} log-ep={}\n",
        inst.token, from.0, from.1
    )
    .expect("subscribe");
    let mut reader = std::io::BufReader::new(stream);
    let mut out = String::new();
    loop {
        let mut line = String::new();
        let n = reader
            .read_line(&mut line)
            .unwrap_or_else(|e| panic!("subscribe @{sid} log: {e}; so far:\n{out}"));
        assert!(n > 0, "subscribe @{sid} log closed; so far:\n{out}");
        let seq = line
            .strip_prefix("LOG ")
            .and_then(|r| r.split_whitespace().nth(1))
            .and_then(|s| s.parse::<u64>().ok());
        out.push_str(&line);
        if seq.is_some_and(|s| s >= upto) {
            return out;
        }
    }
}

/// `line` on a connection of its own, on a thread: an `await` sent before what wakes it.
fn parked(inst: &Inst, line: String) -> std::thread::JoinHandle<String> {
    let (sock, token) = (inst.sock.clone(), inst.token.clone());
    std::thread::spawn(move || {
        let r = connect(&sock, &token)
            .expect("connect to aterm")
            .request(&line)
            .unwrap_or_else(|e| panic!("{line}: {e}"));
        format!("{}\n{}", r.header(), r.rows().join("\n"))
    })
}

/// A headless instance's clean quit by its terminating signal, by PID: a standing hold refuses
/// `invoke Quit` (a window verb) across the instance.
fn quit_by_signal(inst: &mut Inst) {
    harness::kill(inst.pid() as i32, 15);
    let gui = inst.gui.as_mut().expect("a running instance");
    let ended = aterm_spec::wait::within(DEADLINE, Duration::from_millis(25), || {
        gui.try_wait().ok().flatten()
    });
    assert!(ended.is_some(), "{} quits on SIGTERM", inst.name);
    inst.gui = None;
    inst.lifeline.take();
    harness::end_bridge(&inst.root.join("link"), inst.launcher);
}

/// **LEG 7 (U22, U23(d), U36, 6F-7, PRIV-4, PLAN2-3): THE PRIVACY SWEEP.** A sentinel is planted
/// in each kind of caller and program text the log meets: a command's later word, a wrapper's
/// flag value (`env -C <dir>`), a title, the supervisor's attention and the person's own, a topic
/// name, a hold reason (the hold left standing across the quit), a mail body, a config value, a
/// post's `key=` and a `via=` hop. Once the mirror has carried past them, every reader's view (at
/// each tier, `survey` and `survey --brief`, an `await log` each plant woke, `subscribe log`'s
/// frames), every bus body, the crash journal while A runs (a clean quit retires it), the
/// `session.toml` the quit leaves at rest, and after the relaunch every file of both instances
/// and the broker hold a sentinel only where the design keeps one.
#[test]
fn the_privacy_sweep_finds_each_sentinel_only_where_the_design_keeps_it() {
    privacy_sweep(true);
}

/// THE NEGATIVE CONTROL of the sweep over the broker's files: the same run with no key beside the
/// log, whose bytes hold what the sealed run's must not.
#[test]
fn the_plaintext_arm_of_the_privacy_sweep_finds_the_bus_records_in_the_log() {
    privacy_sweep(false);
}

/// The sweep, its broker's log sealed at rest when `sealed`, as `aterm fabric on` leaves it: then
/// no file of the broker's holds a sentinel; a plaintext log holds the bus's own records
/// (`kept_on_disk`). This world compacts nothing, so `.state` and `.held` hold none in either arm:
/// `aterm_link::at_rest`'s tests hold those two to the sentinel, with a plaintext control.
fn privacy_sweep(sealed: bool) {
    let owner = Owner::boot_at_rest(sealed);
    let mut a = Inst::boot(
        &owner,
        "a",
        NODE_A,
        Opts {
            top: "font_family = \"QZP6SECRET7\"\nfont_family_italic = \"QZP6SECRET7\"\n".into(),
            config: HARNESS_HANDS_OVER.into(),
            ..Opts::default()
        },
    );
    let b = Inst::boot(&owner, "b", NODE_B, Opts::default());
    a.wait_ready(&owner);
    b.wait_ready(&owner);
    let x = sids(&a)[0].clone();
    let y = sids(&b)[0].clone();
    let start = cursor(&a, &x);

    // The plants: a command's later word (1); a wrapper's flag value (11), which a program-name
    // reader without the wrapper's grammar keeps as the program (PRIV-1), its run real so the
    // kernel can confirm the `env` that ran (its `make` never does), the value an executable
    // `#!/usr/bin/env /bin/sh` script, which such a reader would find confirmed by any image of
    // that run it heard; an array's element (12); a comment's text (13); a lone directory, the
    // line zsh reports when AUTO_CD runs it as `cd` (14: this world's shell has no AUTO_CD, so its
    // marks report the line); a title (2). The session's shell writes 1, 12, 13 and 14's marks
    // ([`marked_command`]), so their blocks run no program. A detail names only a builtin
    // (`echo`, `true`), a program whose file ran, or a program the command's own job ran standing
    // in for a word that is no program file (H2 rule 2). Which image of a chain the kernel's
    // merged `exec` notes leave heard depends on when the gather reads them, so the wrapper's run
    // reads `env`, the `sh` it ran, the shell `sh` becomes ([`sh_hands_off_to`]: bash on a default
    // macOS), or none ([`ran_command`]). That shell is the session's own too, so this plant tells
    // no stand-in from a misplaced image of the session's shell; `standin.sh`, which reads exactly
    // `qzrun`, fails for one. The directory's path names no file, so it reads none, and no
    // stand-in: its block runs no program. Each later plant's job is programs that are their own
    // images, started once the block's C is written, which this world's process watch hears
    // ([`exec_in_block`]), each handed the plant's sentinel path as an argument (`--rcfile`, which
    // a `-c` bash never reads), as a zoxide-shaped hook or `mkdir` and `cat` are: AUTO_CD into a
    // directory the line made (15) or a lone one (16) reads none, never the directory, nor its
    // job's program (no program of the command's own ran); a path handed to a program and then typed (17: `x` a data file) reads
    // none or the job's program `qzcat` standing in, never `x`; an alias whose expansion runs
    // `true` handed the sentinel (18) reads none or the `true` that ran (H5 is not built: only a
    // stand-in names it); one whose expansion is an `env` handed the sentinel (19) that runs
    // `qzrun` reads `qzrun`, the stand-in, every time: the wrapper never stands in, the job's own
    // image is heard, and `qzrun` outlives the read (a stand-in is an image read by its file). A
    // reader confirming a path word by a path an exec was handed (G1's script words, before H1)
    // reads 15, 16 and 17 by their sentinel's name.
    let root = a.root.display().to_string();
    for dir in ["QZP6SECRET15", "QZP6SECRET16", "QZP6SECRET17"] {
        std::fs::create_dir(a.root.join(dir)).expect("a plant's directory");
    }
    std::fs::write(a.root.join("QZP6SECRET17").join("x"), "data\n").expect("a data file");
    let flag_value = a.root.join("QZP6SECRET11");
    std::fs::write(&flag_value, "#!/usr/bin/env /bin/sh\n").expect("the flag value's script");
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&flag_value, std::fs::Permissions::from_mode(0o755))
            .expect("an executable flag value");
    }
    let handed_to = |name: &str, path: &str| {
        format!(
            "{} --rcfile {path}",
            harness::own_image(&a.root, name).display()
        )
    };
    let handed_off = sh_hands_off_to();
    let wrapped: Vec<Option<&str>> = [None, Some("env"), Some("sh")]
        .into_iter()
        .chain(handed_off.iter().map(|shell| Some(shell.as_str())))
        .collect();
    let plants: Vec<(&str, String, PlantRun, &[Option<&str>])> = vec![
        (
            "marked.sh",
            "echo QZP6SECRET1 42".into(),
            PlantRun::Marks,
            &[Some("echo")],
        ),
        (
            "wrapped.sh",
            format!("env -C {} make", flag_value.display()),
            PlantRun::Env,
            wrapped.as_slice(),
        ),
        (
            "array.sh",
            "arr=(a QZP6SECRET12 b)".into(),
            PlantRun::Marks,
            &[None],
        ),
        (
            "comment.sh",
            "true # x\\\\x3b QZP6SECRET13 y".into(),
            PlantRun::Marks,
            &[Some("true")],
        ),
        (
            "autocd.sh",
            "~/QZP6SECRET14".into(),
            PlantRun::Marks,
            &[None],
        ),
        (
            "autocd_made.sh",
            format!("mkdir {root}/QZP6SECRET15 && {root}/QZP6SECRET15"),
            PlantRun::Execs(handed_to("qzmk", &format!("{root}/QZP6SECRET15"))),
            &[None],
        ),
        (
            "autocd_hook.sh",
            format!("{root}/QZP6SECRET16"),
            PlantRun::Execs(handed_to("qzcd", &format!("{root}/QZP6SECRET16"))),
            &[None],
        ),
        (
            "handed.sh",
            format!("cat {root}/QZP6SECRET17/x\\\\x3b {root}/QZP6SECRET17/x"),
            PlantRun::Execs(handed_to("qzcat", &format!("{root}/QZP6SECRET17/x"))),
            &[None, Some("qzcat")],
        ),
        (
            "alias.sh",
            "qza".into(),
            PlantRun::Execs(handed_to("true", &format!("{root}/QZP6SECRET18"))),
            &[None, Some("true")],
        ),
        (
            "standin.sh",
            "qzs".into(),
            PlantRun::Execs(format!(
                "/usr/bin/env QZP6SECRET19=1 {}",
                harness::own_image(&a.root, "qzrun").display()
            )),
            &[Some("qzrun")],
        ),
    ];
    for (i, (name, line, ran, _)) in plants.iter().enumerate() {
        let (run, script) = match ran {
            PlantRun::Marks => (".", marked_command(&a.root, name, line)),
            PlantRun::Env => ("sh", ran_command(&a.root, name, line)),
            PlantRun::Execs(chain) => (".", exec_in_block(&a.root, name, line, chain)),
        };
        type_line(&a, &x, &format!("{run} {}", script.display()));
        until_on(&a, &x, DEADLINE, &format!("the run of {name}"), || {
            let cmds = a.text(&format!("@{x} log tier=host kinds=cmd n=400"));
            let closed = cmds.lines().filter(|l| l.contains(" close ")).count();
            (closed > i).then_some(())
        });
    }
    let cmds = a.text(&format!("@{x} log tier=host kinds=cmd n=400"));
    let runs = cmd_details(&cmds);
    assert_eq!(runs.len(), plants.len(), "one run per plant: {cmds}");
    let mut written: Vec<String> = Vec::new();
    for ((name, .., allowed), details) in plants.iter().zip(&runs) {
        let last = details.last().map(String::as_str);
        assert!(
            details.iter().all(|d| allowed.contains(&Some(d.as_str()))) && allowed.contains(&last),
            "{name}: detail {details:?}, not one of {allowed:?}: {cmds}"
        );
        written.extend(details.iter().map(|d| format!("detail={d}")));
    }
    let title = a.root.join("title.sh");
    std::fs::write(&title, "printf '\\033]0;QZP6SECRET2\\007'\n").expect("the title script");
    type_line(&a, &x, &format!("sh {}", title.display()));
    until_on(&a, &x, DEADLINE, "the title", || {
        a.text(&format!("@{x} log tier=content"))
            .contains("QZP6SECRET2")
            .then_some(())
    });
    // An address naming B routes only while B advertises Y live.
    let y_row = format!("/f/{FLEET}/pub/{NODE_B}/{y}/presence");
    let mut root = owner.root_client();
    until("B to advertise its session live", || {
        let page = root.last(&y_row, "", 8).ok()?;
        page.0
            .iter()
            .any(|(_, s, b)| *s == y_row && String::from_utf8_lossy(b).contains("state=live"))
            .then_some(())
    });
    // The supervisor's attention (3) is screen text no file keeps; the person's own (10) is
    // restored metadata (b9a0903e34 had moved the plant to the supervisor's alone).
    let at = cursor(&a, &x);
    let mail_await = parked(
        &a,
        format!(
            "@{x} await log kind=mail since={} ep={} timeout=60000",
            at.0, at.1
        ),
    );
    for line in [
        format!("@{x} meta set attention owner=supervisor QZP6SECRET3"),
        format!("@{x} meta set attention QZP6SECRET10"),
        format!("@{x} topic add qzp6secret4"),
        format!(
            "@{x} post to=@{y}@{NODE_B} kind=note key=QZP6SECRET8 via=h-qzp6secret9 QZP6SECRET6"
        ),
    ] {
        let r = a.verb(&line);
        assert!(r.ok(), "{line}: {}", r.header());
    }
    until("B's session to hold the mail", || {
        b.text(&format!("@{y} log kinds=mail"))
            .contains(" mail dir=in ")
            .then_some(())
    });
    // The hold (5) stands from here across the quit.
    let at = cursor(&a, &x);
    let hold_await = parked(
        &a,
        format!(
            "@{x} await log kind=hold since={} ep={} timeout=60000",
            at.0, at.1
        ),
    );
    let r = a.verb(&format!("hold {x} on reason=QZP6SECRET5"));
    assert!(r.ok(), "hold: {}", r.header());
    // The mirror has carried every record before a review mark (bus class) once B's view of X
    // reaches the mark.
    paced_mark(&a, &x, "need word=review");
    let barrier: u64 = a
        .text(&format!("@{x} log kinds=need"))
        .lines()
        .rev()
        .find(|l| l.contains(" need word=review "))
        .and_then(|l| l.split_whitespace().next()?.parse().ok())
        .expect("the review's seq");
    until("B's view of X to reach the review", || {
        bus_last_seq(&brief_json(&b), &x).filter(|n| *n >= barrier)
    });

    let mut sweep = Sweep::default();
    // Every reader's view, at each tier.
    for (args, allowed) in [
        ("log n=400", &[8, 9][..]),
        ("log tier=host n=400", &[8, 9][..]),
        ("log tier=content n=400", &[2, 3, 4, 5, 8, 9, 10][..]),
        ("brief", &[][..]),
    ] {
        let line = format!("@{x} {args}");
        sweep.read(&format!("A {line}"), a.text(&line).as_bytes(), allowed);
        let line = format!("@{y} {args}");
        sweep.read(&format!("B {line}"), b.text(&line).as_bytes(), allowed);
    }
    sweep.read("A log n", a.text("log n").as_bytes(), &[]);
    sweep.read("B fabric brief --json", brief_json(&b).as_bytes(), &[]);
    let (_, text, _) = cli(&b, &["fabric", "brief"]);
    sweep.read("B fabric brief", text.as_bytes(), &[]);
    sweep.read("B controls overview", overview(&b).as_bytes(), &[]);
    // The survey Summarize's worker reads, both forms, on each instance. `survey` reads at its
    // local default, `content`, whose excerpts quote the title and the person's attention
    // (`help survey`: `title=`, `attention=`) from RAM, as `log tier=content` does; `--brief`
    // quotes the node log and the briefs.
    for (name, inst) in [("A", &a), ("B", &b)] {
        for (args, allowed) in [
            (&["ctl", "survey"][..], &[2, 10][..]),
            (&["ctl", "survey", "--brief"][..], &[][..]),
        ] {
            let (code, out, err) = cli(inst, args);
            assert_eq!(code, 0, "{name} {args:?}: {out}\n{err}");
            sweep.read(
                &format!("{name} {}", args.join(" ")),
                out.as_bytes(),
                allowed,
            );
        }
    }
    // An `await log` each plant woke, and `subscribe log`'s frames over every plant: the Host
    // tier, whose positive control is the post's key and hop.
    let host_keys = |what: &str, text: &str| {
        let held = sentinels(text.as_bytes());
        assert!(
            held.contains(&8) && held.contains(&9),
            "CONTROL: {what} serves the post's key and hop: {text}"
        );
    };
    let woke = mail_await.join().expect("the mail's await");
    assert!(woke.starts_with("OK seq="), "await log kind=mail: {woke}");
    host_keys("await log kind=mail", &woke);
    sweep.read("A await log kind=mail", woke.as_bytes(), &[8, 9]);
    let woke = hold_await.join().expect("the hold's await");
    assert!(woke.starts_with("OK seq="), "await log kind=hold: {woke}");
    sweep.read("A await log kind=hold", woke.as_bytes(), &[]);
    let frames = log_frames(&a, &x, &start, barrier);
    host_keys("subscribe log", &frames);
    sweep.read("A subscribe log", frames.as_bytes(), &[8, 9]);
    // Every bus body: the attention text is the presence row's (`meta set attention`, DESIGN
    // §8 presence), the mail's own record its body and hop; the post's key stays on the node.
    let mut c = owner.root_client();
    let mut from = 0u64;
    loop {
        let (rows, (next, head)) = c
            .fetch(from, &format!("/f/{FLEET}/>"), 256)
            .expect("the bus");
        for (_, subject, body) in rows {
            let allowed: &[u8] = if subject.ends_with("/presence") {
                &[3, 10]
            } else if subject.starts_with(&format!("/f/{FLEET}/in/")) {
                &[6, 9]
            } else {
                &[]
            };
            sweep.read(&format!("bus {subject}"), &body, allowed);
        }
        if next >= head || next <= from {
            break;
        }
        from = next;
    }
    drop(c);

    // The crash journal is the restore store a crash leaves, and a clean quit retires it, so it
    // is read while A runs: once a write holds the title, the person's attention and the
    // standing hold (its positive control, which needs at least one journal file), each of its
    // files 0600 and held to the restore store's list.
    let state = a.root.join("astate");
    let running = until(
        "A's crash journal to hold the title, the person's attention and the hold",
        || {
            let files: Vec<(PathBuf, Vec<u8>)> = std::fs::read_dir(&state)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| {
                    let rel = Path::new("astate").join(e.file_name());
                    restore_store(&rel) && !rel.ends_with("session.toml")
                })
                .filter_map(|e| Some((e.path(), std::fs::read(e.path()).ok()?)))
                .collect();
            let held: Vec<u8> = files.iter().flat_map(|(_, b)| sentinels(b)).collect();
            [2, 5, 10].iter().all(|n| held.contains(n)).then_some(files)
        },
    );
    for (path, bytes) in &running {
        assert_eq!(mode_of(path), 0o600, "{}", path.display());
        let rel = path.strip_prefix(&a.root).expect("under A's root");
        sweep.read(
            &format!("A running/{}", rel.display()),
            bytes,
            &kept_on_disk(rel),
        );
    }

    // A quits with the hold standing; what it leaves at rest, before any launch takes the restore
    // store aside: `session.toml` holds the title, the person's attention and the hold (its
    // positive control), 0600. A quit writes no carry: the carry is a seamless update's, which
    // `lab seamless`'s privacy leg reads (unit B23).
    quit_by_signal(&mut a);
    let mut store = Vec::new();
    for e in std::fs::read_dir(&state).expect("the state root").flatten() {
        let rel = Path::new("astate").join(e.file_name());
        if restore_store(&rel) {
            store.push(e.path());
        }
    }
    assert!(
        store.iter().any(|p| p.ends_with("session.toml")),
        "the quit left no session.toml: {store:?}"
    );
    let mut held = Vec::new();
    for path in &store {
        assert_eq!(mode_of(path), 0o600, "{}", path.display());
        held.extend(sentinels(&std::fs::read(path).expect("the restore store")));
    }
    for n in [2, 5, 10] {
        assert!(
            held.contains(&n),
            "CONTROL: the restore store at rest {store:?} holds {n}: {held:?}"
        );
    }
    // The folder is the state root, which this harness makes 0700 as macOS's default deployment
    // hardens it (the per-user control socket directory, `aterm_uds::control_socket_dir`; a
    // private `--control-sock` leaves it as made): no one else may write it, and the instance
    // root above it is 0700.
    assert_eq!(mode_of(&state) & 0o022, 0, "{}", state.display());
    assert_eq!(mode_of(&a.root), 0o700, "{}", a.root.display());
    sweep.files("A at rest", &a.root, &kept_on_disk);

    // A comes back on its roots. The launch takes `session.toml` aside
    // (`.session.toml.aterm-taken-<run>`) and settles the take once it restored the windows; the
    // sweep reads the roots it settled.
    a.launch();
    a.wait_ready(&owner);
    let settled = aterm_spec::wait::within(DEADLINE, Duration::from_millis(25), || {
        launch_takes(&state).is_empty().then_some(())
    });
    assert!(
        settled.is_some(),
        "{}'s launch take settles: {:?}",
        a.name,
        launch_takes(&state)
    );
    let x_then = a.text(&format!("@{x} log tier=host n=400"));
    sweep.read(
        "A @x log tier=host after the relaunch",
        x_then.as_bytes(),
        &[8, 9],
    );
    for dir in ["link/keys", "link/topics"] {
        let files: Vec<PathBuf> = std::fs::read_dir(a.root.join(dir))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .collect();
        assert!(!files.is_empty(), "nothing under {dir}");
        for path in files {
            assert_eq!(mode_of(&path), 0o600, "{}", path.display());
        }
    }
    sweep.files("A", &a.root, &kept_on_disk);
    sweep.files("B", &b.root, &kept_on_disk);
    let broker_dir = owner.tmp.join("broker");
    sweep.files("broker", &broker_dir, &|rel| {
        if !sealed && rel == Path::new("b.log") {
            kept_on_disk(rel)
        } else {
            Vec::new()
        }
    });
    let in_log = sentinels(&std::fs::read(broker_dir.join("b.log")).expect("the log"));
    let word = std::fs::read_to_string(broker_dir.join("stderr.log")).expect("the broker's stderr");
    let (_, report, _) = cli(&b, &["fabric"]);
    let (_, doctor, _) = cli(&b, &["fabric", "doctor"]);
    let told = if sealed { "sealed" } else { "plaintext" };
    assert!(
        report
            .lines()
            .any(|l| l.trim_start().starts_with("at-rest") && l.ends_with(told)),
        "`aterm fabric`'s row: {report}"
    );
    assert!(
        doctor.contains(&format!("at-rest={told}")),
        "`aterm fabric doctor`: {doctor}"
    );
    if sealed {
        assert_eq!(in_log, Vec::<u8>::new(), "the sealed log's bytes");
        assert!(word.contains("at-rest=sealed"), "{word}");
    } else {
        // The mail's body and hop, and the attention that stands: the supervisor's before it (3)
        // may never reach a presence row, which publishes only the latest.
        for n in [6, 9, 10] {
            assert!(
                in_log.contains(&n),
                "CONTROL: the plaintext log holds {n}: {in_log:?}"
            );
        }
        assert!(word.contains("at-rest=plaintext"), "{word}");
    }
    eprintln!("found:\n{}", sweep.found.join("\n"));
    let journals = journals(&a);
    for detail in &written {
        assert!(
            journals.iter().any(|(_, bytes)| holds(bytes, detail)),
            "the command's program is kept: {detail}"
        );
    }
    assert!(
        sweep.leaks.is_empty(),
        "sentinels where the design keeps none:\n{}",
        sweep.leaks.join("\n")
    );
}

/// LEG 7T's sentinel: the value on an aterm.toml line that does not parse.
const UNPARSED_TOML: u8 = 24;

/// **LEG 7T (unit E7): AN aterm.toml THAT STOPS PARSING REACHES aterm.log ONLY AS ITS POSITION.**
/// A's aterm.toml is rewritten while it runs with an unterminated string holding 24 (a value the
/// person wrote): the config lane refuses it and says so (`native config: Config observation was
/// not valid TOML: … TOML error at line L, column C`), and after the quit every file under A's
/// root holds 24 only where the design keeps it: the file itself, the Messages store and stderr.
#[test]
fn an_unparsed_aterm_toml_reaches_aterm_log_only_as_its_position() {
    let owner = Owner::boot();
    let mut a = Inst::boot(&owner, "a", NODE_A, Opts::default());
    a.wait_ready(&owner);
    let x = sids(&a)[0].clone();
    let config = a.root.join("cfg/aterm/aterm.toml");
    let text = std::fs::read_to_string(&config).expect("A's config");
    let refused = "native config: Config observation was not valid TOML: ";
    assert!(
        !host_log(&a).contains(refused),
        "PRECONDITION: A's config parsed at launch"
    );
    std::fs::write(
        &config,
        format!("{text}font_family = \"QZP6SECRET{UNPARSED_TOML} Mono\n"),
    )
    .expect("the rewrite");
    let line = until_on(&a, &x, DEADLINE, "the lane refuses the rewrite", || {
        host_log(&a)
            .lines()
            .find(|l| l.contains(refused))
            .map(str::to_string)
    });
    quit(&mut a);
    let mut sweep = Sweep::default();
    sweep.files("A", &a.root, &kept_on_disk);
    eprintln!("found:\n{}", sweep.found.join("\n"));
    assert!(
        sweep.leaks.is_empty(),
        "sentinels where the design keeps none:\n{}",
        sweep.leaks.join("\n")
    );
    assert!(
        line.contains("TOML error at line "),
        "CONTROL: aterm.log names the position: {line}"
    );
    let file = format!("A/cfg/aterm/aterm.toml: [{UNPARSED_TOML}]");
    assert!(
        sweep.found.contains(&file),
        "CONTROL: the file keeps the line ({file}): {:#?}",
        sweep.found
    );
}

/// LEG 7H's sentinels: the reply's last line (21), the box's command (22) and the question and
/// its options (23).
const HARNESS_REVIEW: [u8; 3] = [21, 22, 23];

/// LEG 7H's harness: it answers no box and types no continuation, so each scene stays the screen
/// it reviews, and it answers the question, as the real run's harness did (its `APPROVED` line
/// names the answer it typed). Its Autopilot stays the default: no host supervises an Off session
/// (`HarnessHosting::hosts`), so an Off harness would review nothing.
const HARNESS_REVIEWS_ONLY: &str = "[harness]\napprove = \"none\"\ncontinue = false\n";

/// **LEG 7H (unit E4, E2E-11): THE HARNESS'S REVIEW REACHES aterm.log ONLY AS WORDS.** A
/// harness-hosted stand-in draws three scenes of its own, each with a sentinel where a real
/// Claude Code run had one: an idle screen whose reply's last line holds 21, a question whose
/// words and options hold 23, and a Bash box whose command holds 22. Each scene's review reaches
/// the loop journal in the drive store with its sentinel (the review's own store, by design) and
/// aterm.log as its lines' words (`harness @<s>: EVENT idle seq=<n> text=<n>B`, `APPROVED`,
/// `EVENT prompt`); after the quit, every file under A's root holds a sentinel only where the
/// design keeps one.
#[test]
fn the_harness_review_reaches_aterm_log_only_as_words() {
    let owner = Owner::boot();
    let mut a = Inst::boot(
        &owner,
        "a",
        NODE_A,
        Opts {
            config: HARNESS_REVIEWS_ONLY.into(),
            ..Opts::default()
        },
    );
    a.wait_ready(&owner);
    write_fake(&a, "resets in 1m");
    let idle = aterm_agent::supervise::prompt::fixtures::screen(AFTER_TURN_283);
    let reply: Vec<String> = idle
        .iter()
        .flat_map(|row| {
            let said = row
                .starts_with("✻ Worked for")
                .then(|| "⏺ QZP6SECRET21 is the reply's last line".to_string());
            said.into_iter().chain([row.clone()])
        })
        .collect();
    let ask = body(ASK_ONE)
        .replace("Which tea would you prefer?", "QZP6SECRET23 which tea?")
        .replace("❯ 1. Green", "❯ 1. QZP6SECRET23 green")
        .replace("  2. Black", "  2. QZP6SECRET23 black");
    let box22 = body(BOX_TOUCH)
        .replace("  ⎿  $ touch x\n", "  ⎿  $ touch QZP6SECRET22\n")
        .replace("\n   touch x\n", "\n   touch QZP6SECRET22\n");
    let sc = a.root.join("sc");
    // Each scene holds its sentinel where the screen does: the reply row; the question and its
    // options; the box's tool row and its command row.
    for (name, text, plant, rows) in [
        ("reply.gate", reply.join("\n") + "\n", "QZP6SECRET21", 1),
        ("ask.gate", ask, "QZP6SECRET23", 3),
        ("box22.gate", box22, "QZP6SECRET22", 2),
    ] {
        assert_eq!(text.matches(plant).count(), rows, "{name}: {text}");
        std::fs::write(sc.join(name), frame(&text, false)).expect("a scene");
    }
    let s = spawn_tab(&a);
    start_fake(
        &a,
        &s,
        "k7h",
        &["busy.gate", "reply.gate", "ask.gate", "box22.gate"],
    );
    program_is(&a, &s, "claude");
    until_on(&a, &s, DEADLINE, "the host attached", || {
        kv(&status_of(&a, &s), "supervisor")
            .is_some_and(|v| v.starts_with("aterm-harness"))
            .then_some(())
    });
    // Each scene's review: its sentinel in the journal and one more line of its kind in aterm.log
    // (the question is a box the harness answers: `APPROVED seq=<n> <the answer it typed>`).
    let journal = a.root.join(format!("astate/drive/{s}.journal.jsonl"));
    let logged_as = |head: &str| {
        let line = format!("harness @{s}: {head}");
        host_log(&a).lines().filter(|l| l.contains(&line)).count()
    };
    for (gate, plant, head) in [
        (1, 21, "EVENT idle seq="),
        (2, 23, "APPROVED seq="),
        (3, 22, "EVENT prompt seq="),
    ] {
        let before = logged_as(head);
        release(&a, "k7h", gate);
        until_on(
            &a,
            &s,
            DEADLINE,
            &format!(
                "the review of scene {} in the journal and aterm.log",
                gate + 1
            ),
            || {
                let journaled = sentinels(&std::fs::read(&journal).unwrap_or_default());
                (journaled.contains(&plant) && logged_as(head) > before).then_some(())
            },
        );
    }
    quit(&mut a);
    let mut sweep = Sweep::default();
    sweep.files("A", &a.root, &kept_on_disk);
    eprintln!("found:\n{}", sweep.found.join("\n"));
    assert!(
        sweep.leaks.is_empty(),
        "sentinels where the design keeps none:\n{}",
        sweep.leaks.join("\n")
    );
    let drive = format!("A/astate/drive/{s}.journal.jsonl: {HARNESS_REVIEW:?}");
    assert!(
        sweep.found.contains(&drive),
        "CONTROL: the loop journal keeps the review it acted on ({drive}): {:#?}",
        sweep.found
    );
    let logged = host_log(&a);
    for event in ["EVENT idle seq=", "APPROVED seq=", "EVENT prompt seq="] {
        let words = logged
            .lines()
            .find_map(|l| {
                l.split_once(&format!("harness @{s}: {event}"))
                    .map(|(_, w)| w)
            })
            .unwrap_or_else(|| panic!("CONTROL: aterm.log names {event}"));
        assert!(
            words
                .split_whitespace()
                .any(|w| w.starts_with("text=") && w.ends_with('B')),
            "CONTROL: {event} is logged with its text's length: {words}"
        );
    }
}
