// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// ─── PowerShell: the readline key layer over PSReadLine's Windows mode ───
//
// `include!`d into lib.rs's `#[cfg(test)] mod tests` after tests_tty.rs; it uses
// tests.rs's `powershell_output` and `Pwsh`.
//
// The script's KEY BINDINGS block says why the layer exists and what it binds.
// The static test pins its shape on every host, since no PowerShell runs on the
// unix lanes. The Windows tests run the script in Windows PowerShell 5.1 (its
// PSReadLine is 2.0.0 on a stock box) and in pwsh 7 where `pwsh.exe` resolves,
// and read back what PSReadLine has bound; the last one presses Ctrl+E at a live
// prompt in a ConPTY and reads back what Enter submitted.
//
// PSReadLine is not loaded under `-NonInteractive` (measured 2026-10-16: `Get-Module
// PSReadLine` is empty at startup in both), while an interactive pwsh has it loaded
// before aterm's `-Command` runs. So every live run imports it first, in the prelude
// that plays the user's profile. `Import-Module PSReadLine` and
// `Set-PSReadLineKeyHandler` both work there, with stdout redirected and
// `CREATE_NO_WINDOW`; these tests are that measurement.

/// The home and per-user state of one test child, never the runner's. The
/// automatic PowerShell $HOME uses USERPROFILE on Windows; PSReadLine's default
/// history uses APPDATA, so redirecting only the HOME environment is insufficient.
#[cfg(windows)]
struct PowerShellWorld {
    root: aterm_tempfile::TempDir,
}

#[cfg(windows)]
impl PowerShellWorld {
    fn new() -> Self {
        let root = aterm_tempfile::tempdir().expect("exclusive PowerShell fixture");
        for child in [
            "home", "roaming", "local", "config", "data", "cache", "state", "tmp",
        ] {
            std::fs::create_dir(root.path().join(child)).expect("private PowerShell state");
        }
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn env(&self) -> Vec<(String, String)> {
        let value = |child: &str| {
            self.root
                .path()
                .join(child)
                .to_str()
                .expect("UTF-8 fixture path")
                .to_owned()
        };
        let mut vars = vec![
            ("HOME".to_owned(), value("home")),
            ("USERPROFILE".to_owned(), value("home")),
            ("ATERM_TEST_HOME".to_owned(), value("home")),
            ("APPDATA".to_owned(), value("roaming")),
            ("LOCALAPPDATA".to_owned(), value("local")),
            ("XDG_CONFIG_HOME".to_owned(), value("config")),
            ("XDG_DATA_HOME".to_owned(), value("data")),
            ("XDG_CACHE_HOME".to_owned(), value("cache")),
            ("XDG_STATE_HOME".to_owned(), value("state")),
            ("TEMP".to_owned(), value("tmp")),
            ("TMP".to_owned(), value("tmp")),
        ];
        // A managed store inherited from a real tab must not be re-fronted by
        // the LIVE hook. The fixture's own hook remains fully exercised.
        vars.extend(
            ["ATPKG_AGENTS", "ATERM_REROUTE_DIR"].map(|key| (key.to_owned(), String::new())),
        );
        vars.push(("__ATERM_REROUTE_PASSTHROUGH".to_owned(), "1".to_owned()));
        vars
    }

    /// The start of every test child's `-Command`. It refuses, before sourcing
    /// any integration or hook, a host that does not honor USERPROFILE as
    /// documented, and never falls back to its home. It points `$PROFILE` into the
    /// private home: `-NoProfile` keeps the real profiles from running, but
    /// `$PROFILE` still names them (Windows PowerShell 5.1 finds them under the
    /// Documents folder, which no environment variable moves), and the key layer
    /// reads the files it names when the shell is in Emacs mode. A test writes the
    /// ones it plays with [`PowerShellWorld::write_profile`].
    fn home_guard() -> &'static str {
        "if ($HOME -cne $env:ATERM_TEST_HOME) { throw 'private PowerShell home not active' }; \
         $__test_profile = [psobject](Join-Path $HOME 'profile\\CurrentUserCurrentHost.ps1'); \
         foreach ($__test_name in 'AllUsersAllHosts', 'AllUsersCurrentHost', 'CurrentUserAllHosts', 'CurrentUserCurrentHost') { \
         Add-Member -InputObject $__test_profile -MemberType NoteProperty -Name $__test_name \
         -Value (Join-Path $HOME ('profile\\' + $__test_name + '.ps1')) }; \
         $global:PROFILE = $__test_profile; "
    }

    /// Write `text` as the file `$PROFILE.<name>` names in this world.
    fn write_profile(&self, name: &str, text: &str) {
        let dir = self.home().join("profile");
        std::fs::create_dir_all(&dir).expect("private profile folder");
        std::fs::write(dir.join(format!("{name}.ps1")), text).expect("private profile");
    }
}

/// Plays the profiles a PowerShell runs at startup, in its order, from the files
/// [`PowerShellWorld::home_guard`] points `$PROFILE` at, where a test wrote them.
#[cfg(windows)]
const PLAY_PROFILES: &str = "foreach ($__test_name in 'AllUsersAllHosts', 'AllUsersCurrentHost', \
     'CurrentUserAllHosts', 'CurrentUserCurrentHost') { \
     $__test_file = $global:PROFILE.$__test_name; \
     if (Test-Path -LiteralPath $__test_file) { . $__test_file } }; ";

/// The block an earlier aterm setup wrote into a Windows PowerShell 5.1 profile:
/// the whole `profile.ps1` it was found in on 2026-10-18 (308 bytes, LF line
/// endings), which kept every 5.1 tab in Emacs mode and out of the key layer
/// until the layer took it back (`$__aterm_legacy_emacs_block`).
const LEGACY_EMACS_BLOCK: &str = "\
# aterm: emacs line-editing so Ctrl+A/Ctrl+E/Ctrl+K/Ctrl+U... move the cursor\n\
# (Windows PowerShell 5.1's PSReadLine defaults to \"Windows\" mode, where Ctrl+A =\n\
# SelectAll and Ctrl+E is unbound). Remove this block to revert.\n\
if (Get-Module -ListAvailable PSReadLine) { Set-PSReadLineOption -EditMode Emacs }\n";

/// The rows the KEY BINDINGS block binds, in its order: chord, function, and the
/// stock Windows-mode function it may replace (`""`: the chord is unbound in
/// Windows mode). `Ctrl+u` is `BackwardKillInput` where PSReadLine has it and
/// `BackwardKillLine` where it does not (2.0.0); `Ctrl+e` is the
/// [`ACCEPT_THEN_END`] script block where PSReadLine has `AcceptSuggestion` and
/// `EndOfLine` where it does not (2.0.0); `Alt+u/l/c` are PSReadLine's
/// [`CASE_WORD_FUNCTIONS`], or where it lacks them (2.0.0) script blocks listed
/// under the same names.
const READLINE_ROWS: &[(&str, &str, &str)] = &[
    ("Ctrl+a", "BeginningOfLine", "SelectAll"),
    ("Ctrl+e", ACCEPT_THEN_END, ""),
    ("Ctrl+b", "BackwardChar", ""),
    ("Ctrl+f", "ForwardChar", ""),
    ("Ctrl+k", "KillLine", ""),
    ("Ctrl+u", "BackwardKillInput", ""),
    ("Ctrl+w", "UnixWordRubout", "BackwardKillWord"),
    ("Ctrl+p", "PreviousHistory", ""),
    ("Ctrl+n", "NextHistory", ""),
    ("Ctrl+d", "DeleteCharOrExit", ""),
    ("Ctrl+t", "SwapCharacters", ""),
    ("Ctrl+y", "Yank", "Redo"),
    ("Ctrl+_", "Undo", ""),
    ("Ctrl+j", "AcceptLine", ""),
    ("Alt+b", "BackwardWord", ""),
    ("Alt+f", "ForwardWord", ""),
    ("Alt+Backspace", "BackwardKillWord", ""),
    ("Alt+LeftArrow", "BackwardWord", ""),
    ("Alt+RightArrow", "ForwardWord", ""),
    ("Alt+u", "UpcaseWord", ""),
    ("Alt+l", "DowncaseWord", ""),
    ("Alt+c", "CapitalizeWord", ""),
];

/// The BriefDescription of the `Ctrl+e` script block (`AcceptSuggestion`, then
/// `EndOfLine`), which `Get-PSReadLineKeyHandler -Bound` lists as its function.
const ACCEPT_THEN_END: &str = "AcceptSuggestionAndEndOfLine";

/// The Alt+u/l/c functions, which PSReadLine 2.0.0 (Windows PowerShell 5.1)
/// lacks, as it lacks `BackwardKillInput` (measured 2026-10-16 with
/// `[Microsoft.PowerShell.PSConsoleReadLine].GetMethod`; 2.4.5 has all four).
/// The layer binds a script block under each name there, so the rows bind on
/// every PSReadLine; without them Windows mode typed a bare u, l or c (measured
/// 2026-10-18).
const CASE_WORD_FUNCTIONS: &[&str] = &["UpcaseWord", "DowncaseWord", "CapitalizeWord"];

/// The chords Windows mode keeps: none of them may be a row.
const WINDOWS_MODE_KEEPS: &[&str] = &[
    "Tab",
    "Ctrl+z",
    "Ctrl+v",
    "Ctrl+c",
    "Ctrl+C",
    "Ctrl+x",
    "Ctrl+LeftArrow",
    "Ctrl+RightArrow",
    "Ctrl+Backspace",
    "Ctrl+Delete",
    "Escape",
    "Home",
    "End",
];

/// The script's code without its comment lines, so a needle in a comment cannot
/// satisfy a pin (SPECS.md §5.7).
fn powershell_code() -> String {
    scripts::POWERSHELL
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The block's shape, on every host: after the readline shim, every gate, the
/// `-Bound` read into an ordinal dictionary, the 2.0.0 fallbacks, and exactly the
/// rows above.
#[test]
fn test_powershell_layers_readline_chords_over_windows_mode() {
    let code = powershell_code();
    let shim = code
        .find("function Global:PSConsoleHostReadLine")
        .expect("the PSReadLine submit shim");
    let start = code
        .find("function Global:__aterm_setup_keybindings {")
        .expect("the key layer is a function, as the other shells' __aterm_setup_keybindings");
    assert!(shim < start, "the key layer comes after the readline shim");
    let call = "    try { __aterm_setup_keybindings } catch { }\n}";
    let end = code[start..].find(call).expect("the call site") + start + call.len();
    let block = &code[start..end];
    assert_eq!(
        code.matches("__aterm_setup_keybindings").count(),
        2,
        "defined once and called once, at load"
    );

    // The gates: PSReadLine loaded, an aterm tab, the opt-out unset, Windows mode.
    let gate = &block[block.rfind("\nif ").expect("the call site's gate")..];
    for needle in [
        "(Get-Module -Name PSReadLine)",
        "($env:ATERM_CHILD -or $env:ATERM_SESSION_ID)",
        "(-not $env:ATERM_DISABLE_READLINE_KEYS)",
    ] {
        assert!(
            gate.contains(needle),
            "the call is gated on {needle}: {gate}"
        );
    }
    for needle in [
        "$ErrorActionPreference = 'Stop'",
        "if (-not (Get-Module -Name PSReadLine)) { return }",
        // Windows mode, or aterm's legacy Emacs block taken back to it.
        "$__aterm_mode = [string](Get-PSReadLineOption).EditMode\n    \
         if (($__aterm_mode -eq 'Emacs') -and (__aterm_legacy_emacs)) {\n        \
         Set-PSReadLineOption -EditMode Windows\n",
        "if ($__aterm_mode -ne 'Windows') { return }",
        // PSReadLine 2.0.0 has no -Chord on Get-PSReadLineKeyHandler.
        "foreach ($__aterm_h in @(Get-PSReadLineKeyHandler -Bound)) {",
        // -Bound lists a chord whole; its first key dispatches to it.
        "if ($__aterm_k -match '^(.+?[^+]),.') { $__aterm_bound[$Matches[1]] = 'ChordFirstKey' }",
        // Key strings are case-significant; @{} is not.
        "New-Object 'System.Collections.Generic.Dictionary[string,string]' ([System.StringComparer]::Ordinal)",
        "$__aterm_kill_input = if ($__aterm_rl.GetMethod('BackwardKillInput')) { 'BackwardKillInput' } else { 'BackwardKillLine' }",
        // Ctrl+e: EndOfLine, or the script block where AcceptSuggestion exists.
        "$__aterm_end_of_line = 'EndOfLine'\n    if ($__aterm_rl.GetMethod('AcceptSuggestion')) {\n        $__aterm_end_of_line = @{",
        // A row's function is a name, gated on this PSReadLine having it, or a
        // script block handler's parameters, splatted.
        "$__aterm_how = $__aterm_row[1]\n        if ($__aterm_how -is [string]) {\n            if (-not $__aterm_rl.GetMethod($__aterm_how)) { continue }\n            $__aterm_how = @{ Function = $__aterm_how }\n        }",
        "if ((-not $__aterm_cur) -or ($__aterm_cur -ceq $__aterm_row[2])) {",
        "Set-PSReadLineKeyHandler -Chord $__aterm_row[0] @__aterm_how",
    ] {
        assert!(block.contains(needle), "the key layer holds {needle:?}");
    }
    assert!(
        !code.contains("Get-PSReadLineKeyHandler -Chord"),
        "PSReadLine 2.0.0 (Windows PowerShell 5.1) has no -Chord on Get-PSReadLineKeyHandler"
    );

    // aterm's legacy Emacs block: the four lines it wrote, each a single-quoted
    // PowerShell string, which __aterm_legacy_emacs looks for in the profiles. The
    // decisions are `test_powershell_readline_layer_takes_back_atermss_legacy_emacs_block`
    // and `test_powershell_readline_layer_keeps_an_emacs_the_user_chose`.
    let quoted: Vec<String> = LEGACY_EMACS_BLOCK
        .lines()
        .map(|line| format!("'{}'", line.replace('\'', "''")))
        .collect();
    let legacy = format!(
        "$Global:__aterm_legacy_emacs_block = @(\n    {})\n",
        quoted.join(",\n    ")
    );
    let legacy_at = code
        .find(&legacy)
        .unwrap_or_else(|| panic!("the legacy block, line for line as aterm wrote it: {legacy}"));
    let check = code
        .find("function Global:__aterm_legacy_emacs {")
        .expect("the legacy check");
    assert!(
        legacy_at < check && check < start,
        "the block, then the check, then the layer that calls it"
    );

    // The Ctrl+e handler: named for -Bound, described, and a block that accepts the
    // suggestion only with the caret at the end of the line, as zsh-autosuggestions
    // does, and then moves to the end. It runs at every press, after the function's
    // variables are gone, so it reads none of them: a block that read $__aterm_rl
    // failed at every press (measured 2026-10-16 through ConPTY). The presses
    // themselves are `test_powershell_ctrl_e_accepts_a_suggestion_only_at_the_end_of_the_line`.
    let handler = &block[block
        .find("$__aterm_end_of_line = @{")
        .expect("the Ctrl+e handler")..];
    let handler = &handler[..handler.find("\n        }\n").expect("the handler closes")];
    assert!(
        handler.contains(&format!("BriefDescription = '{ACCEPT_THEN_END}'")),
        "-Bound lists the handler by its BriefDescription: {handler}"
    );
    let body = &handler[handler
        .find("ScriptBlock      = {")
        .expect("the script block")..];
    assert!(
        !body.contains("$__aterm_"),
        "the block runs at key-press time and reads no variable of the setup function: {body}"
    );
    assert!(
        body.contains(
            "param($key, $arg)\n                \
             $line = $null\n                \
             $cursor = $null\n                \
             [Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState([ref]$line, [ref]$cursor)\n                \
             if ($cursor -eq $line.Length) {\n                    \
             [Microsoft.PowerShell.PSConsoleReadLine]::AcceptSuggestion($key, $arg)\n                \
             }\n                \
             [Microsoft.PowerShell.PSConsoleReadLine]::EndOfLine($key, $arg)\n            }"
        ),
        "AcceptSuggestion only with the caret at the end of the buffer, then EndOfLine: {body}"
    );

    // Alt+u/l/c: PSReadLine's function, or a script block listed under its name
    // that cases the word from the caret. Like the Ctrl+e block it runs at key
    // press time, so it calls the global helper and reads no setup variable.
    // The presses are `test_powershell_alt_u_l_c_case_the_word_on_every_psreadline`.
    assert!(
        block.contains("function Global:__aterm_case_word([string]$how) {"),
        "the case helper"
    );
    for (function, var, how) in CASE_WORD_FUNCTIONS
        .iter()
        .zip([
            "$__aterm_upcase",
            "$__aterm_downcase",
            "$__aterm_capitalize",
        ])
        .zip(["Upcase", "Downcase", "Capitalize"])
        .map(|((function, var), how)| (*function, var, how))
    {
        let fallback = format!(
            "{var} = '{function}'\n    \
             if (-not $__aterm_rl.GetMethod('{function}')) {{\n        \
             {var} = @{{\n            \
             BriefDescription = '{function}'"
        );
        assert!(
            block.contains(&fallback),
            "{function}'s fallback: {fallback:?}"
        );
        let call = format!("ScriptBlock      = {{ param($key, $arg) __aterm_case_word '{how}' }}");
        assert!(block.contains(&call), "{function}'s block: {call:?}");
    }

    // The rows, exactly: `@('chord', function, 'stock')`, one per line.
    let rows: Vec<(String, String, String)> = block
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix("@('"))
        .map(|row| {
            let row = &row[..row.find(')').expect("a row closes")];
            let cells: Vec<&str> = row
                .split(", ")
                .map(|cell| cell.trim_matches('\''))
                .collect();
            assert_eq!(cells.len(), 3, "chord, function, stock: {row}");
            (
                cells[0].to_owned(),
                cells[1].to_owned(),
                cells[2].to_owned(),
            )
        })
        .collect();
    let expected: Vec<(String, String, String)> = READLINE_ROWS
        .iter()
        .map(|&(chord, function, stock)| {
            let function = match chord {
                "Ctrl+u" => "$__aterm_kill_input",
                "Ctrl+e" => "$__aterm_end_of_line",
                "Alt+u" => "$__aterm_upcase",
                "Alt+l" => "$__aterm_downcase",
                "Alt+c" => "$__aterm_capitalize",
                _ => function,
            };
            (chord.to_owned(), function.to_owned(), stock.to_owned())
        })
        .collect();
    assert_eq!(rows, expected);
    for kept in WINDOWS_MODE_KEEPS {
        assert!(
            READLINE_ROWS.iter().all(|&(chord, _, _)| chord != *kept),
            "{kept} stays as Windows mode has it"
        );
    }
}

/// What PSReadLine has bound at the end of a live run.
#[cfg(windows)]
struct Bound {
    /// `Get-PSReadLineKeyHandler -Bound`, in its order: `(Key, Function)`.
    keys: Vec<(String, String)>,
    /// Which of `BackwardKillInput`, the [`CASE_WORD_FUNCTIONS`] and
    /// `AcceptSuggestion` this PSReadLine has.
    has: std::collections::BTreeSet<String>,
    /// `(Get-PSReadLineOption).EditMode`.
    mode: String,
    /// The PSReadLine version, for messages.
    version: String,
    /// Whether the integration script defined the key layer (it ran).
    loaded: bool,
}

#[cfg(windows)]
impl Bound {
    fn get(&self, key: &str) -> Option<&str> {
        self.keys
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, function)| function.as_str())
    }

    /// The bindings as a map; Windows mode lists each key once.
    fn map(&self) -> std::collections::BTreeMap<String, String> {
        let map: std::collections::BTreeMap<String, String> = self.keys.iter().cloned().collect();
        assert_eq!(
            map.len(),
            self.keys.len(),
            "a key listed twice: {:?}",
            self.keys
        );
        map
    }
}

/// The snippet that prints a [`Bound`], one `field=value` per line.
#[cfg(windows)]
const LIST_BOUND: &str = "$l = @('version=' + (Get-Module PSReadLine).Version); \
     $l += 'mode=' + (Get-PSReadLineOption).EditMode; \
     $l += 'loaded=' + [bool](Get-Command __aterm_setup_keybindings -ErrorAction Ignore); \
     foreach ($f in 'BackwardKillInput', 'UpcaseWord', 'DowncaseWord', 'CapitalizeWord', 'AcceptSuggestion') { \
         if ([Microsoft.PowerShell.PSConsoleReadLine].GetMethod($f)) { $l += 'has=' + $f } }; \
     foreach ($h in @(Get-PSReadLineKeyHandler -Bound)) { $l += 'key=' + $h.Key + '=' + $h.Function }; \
     [Console]::Write($l -join [char]10)";

/// Run `exe` with PSReadLine imported and then `prelude` (the user's profile), the
/// integration as `how` says, and `env`; return what PSReadLine has bound. The run
/// must succeed and print nothing on stderr: the layer never errors at startup.
#[cfg(windows)]
fn bound_after(exe: &Path, prelude: &str, how: Pwsh, env: &[(&str, &str)]) -> Bound {
    bound_after_in(&PowerShellWorld::new(), exe, prelude, how, env)
}

/// [`bound_after`] in `world`, whose private home holds the profiles a test wrote.
#[cfg(windows)]
fn bound_after_in(
    world: &PowerShellWorld,
    exe: &Path,
    prelude: &str,
    how: Pwsh,
    env: &[(&str, &str)],
) -> Bound {
    let prelude = format!("Import-Module PSReadLine; {prelude}");
    let output = powershell_output_in(world, exe, &prelude, LIST_BOUND, None, how, env);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stderr.trim().is_empty(),
        "{exe:?} with prelude {prelude:?}: status {:?}; stdout: {stdout:?}; stderr: {stderr:?}",
        output.status
    );
    let mut bound = Bound {
        keys: Vec::new(),
        has: std::collections::BTreeSet::new(),
        mode: String::new(),
        version: String::new(),
        loaded: false,
    };
    for line in stdout.lines().map(|line| line.trim_end_matches('\r')) {
        if let Some(rest) = line.strip_prefix("key=") {
            // A key may hold `=` (`Alt+=`); a function name never does.
            let (key, function) = rest.rsplit_once('=').expect("key=<Key>=<Function>");
            bound.keys.push((key.to_owned(), function.to_owned()));
        } else if let Some(function) = line.strip_prefix("has=") {
            bound.has.insert(function.to_owned());
        } else if let Some(mode) = line.strip_prefix("mode=") {
            mode.clone_into(&mut bound.mode);
        } else if let Some(version) = line.strip_prefix("version=") {
            version.clone_into(&mut bound.version);
        } else if let Some(loaded) = line.strip_prefix("loaded=") {
            bound.loaded = loaded == "True";
        }
    }
    assert!(
        !bound.version.is_empty(),
        "{exe:?} printed no listing: {stdout:?}"
    );
    bound
}

/// The rows a PSReadLine with `has` binds: `Ctrl+u` and `Ctrl+e` take their 2.0.0
/// fallbacks, and every other row binds, the [`CASE_WORD_FUNCTIONS`] as the
/// layer's script blocks of the same name where PSReadLine lacks them.
#[cfg(windows)]
fn expected_layer(
    has: &std::collections::BTreeSet<String>,
) -> Vec<(&'static str, &'static str, &'static str)> {
    READLINE_ROWS
        .iter()
        .map(|&(chord, function, stock)| {
            if chord == "Ctrl+u" && !has.contains("BackwardKillInput") {
                return (chord, "BackwardKillLine", stock);
            }
            if chord == "Ctrl+e" && !has.contains("AcceptSuggestion") {
                return (chord, "EndOfLine", stock);
            }
            (chord, function, stock)
        })
        .collect()
}

/// pwsh 7 for the live tests: `$ATERM_TEST_PWSH`, else the first `pwsh.exe` on
/// PATH. An override that names nothing is an operator error and fails, as
/// `aterm_spec::host::test_shell`'s does on unix. The PATH entry is read without being
/// followed: a Store install's `pwsh.exe` (under `WindowsApps`) is an app
/// execution alias, a reparse point, which `CreateProcess` runs.
#[cfg(windows)]
fn pwsh_exe() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("ATERM_TEST_PWSH") {
        let explicit = PathBuf::from(explicit);
        assert!(
            std::fs::symlink_metadata(&explicit).is_ok(),
            "ATERM_TEST_PWSH points at {explicit:?}, which does not exist"
        );
        return Some(explicit);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("pwsh.exe"))
        .find(|candidate| std::fs::symlink_metadata(candidate).is_ok_and(|meta| !meta.is_dir()))
}

/// Run `check` on Windows PowerShell 5.1, which every supported Windows has, and
/// on pwsh 7 where it resolves. Where it does not, the 5.1 half has run and the
/// test ends COULD NOT RUN for the pwsh half: a pass never claims a pwsh that did
/// not run (a `SKIP` note on stderr beside a pass hid the lane in a green run).
#[cfg(windows)]
fn on_each_powershell(test: &str, mut check: impl FnMut(&Path)) {
    check(Path::new("powershell.exe"));
    match pwsh_exe() {
        Some(pwsh) => check(&pwsh),
        None => aterm_spec::host::could_not_run(format!(
            "{test} on pwsh 7: no pwsh.exe on PATH and no ATERM_TEST_PWSH; only Windows \
             PowerShell 5.1 ran"
        )),
    }
}

/// In an aterm tab every row is bound and nothing else moves: the whole `-Bound`
/// list is the stock one with the rows laid over it. `ATERM_SESSION_ID` alone
/// opens the gate as `ATERM_CHILD` does.
#[cfg(windows)]
#[test]
fn test_powershell_binds_the_readline_chords_in_an_aterm_tab() {
    on_each_powershell("readline chords", |exe| {
        let stock = bound_after(exe, "", Pwsh::Bare, &[]);
        assert_eq!(
            stock.mode, "Windows",
            "{exe:?}: PSReadLine's default on Windows"
        );
        let layer = expected_layer(&stock.has);
        let mut expected = stock.map();
        for &(chord, function, replaces) in &layer {
            // The precondition the layer is written against: every row's chord is
            // unbound in stock Windows mode, or holds the function the row names.
            assert_eq!(
                stock.get(chord).unwrap_or(""),
                replaces,
                "{exe:?} PSReadLine {}: stock {chord}",
                stock.version
            );
            expected.insert(chord.to_owned(), function.to_owned());
        }
        let tab = bound_after(exe, "", Pwsh::InTab, &[]);
        assert!(tab.loaded);
        assert_eq!(
            tab.map(),
            expected,
            "{exe:?} PSReadLine {}: stock Windows mode plus exactly the rows",
            tab.version
        );
        // Ctrl+e accepts an inline suggestion where PSReadLine has
        // AcceptSuggestion, and is EndOfLine where it does not: 5.1's PSReadLine
        // 2.0.0 has neither it nor predictions, pwsh 7.6.7's 2.4.5 has both
        // (measured 2026-10-16).
        let end_of_line = if stock.has.contains("AcceptSuggestion") {
            ACCEPT_THEN_END
        } else {
            "EndOfLine"
        };
        for (chord, function) in [
            ("Ctrl+a", "BeginningOfLine"),
            ("Ctrl+e", end_of_line),
            ("Ctrl+k", "KillLine"),
            ("Ctrl+y", "Yank"),
            ("Alt+b", "BackwardWord"),
            ("Alt+f", "ForwardWord"),
            ("Tab", "TabCompleteNext"),
            ("Ctrl+z", "Undo"),
            ("Ctrl+LeftArrow", "BackwardWord"),
        ] {
            assert_eq!(tab.get(chord), Some(function), "{exe:?}: {chord}");
        }
        let session = bound_after(exe, "", Pwsh::Outside, &[("ATERM_SESSION_ID", "s-test")]);
        assert_eq!(
            session.map(),
            expected,
            "{exe:?}: ATERM_SESSION_ID is an aterm tab too"
        );
    });
}

/// A chord the profile bound (it runs before the integration) keeps the user's
/// function, whether it was unbound in stock Windows mode (`Ctrl+e`), held a stock
/// function the layer would replace (`Ctrl+y`), or is a script block (`Alt+b`).
/// A row key that starts a profile's two-key chord is left to the chord, whether
/// it was unbound in stock (`Ctrl+d`) or held a stock function (`Ctrl+w`):
/// `-Bound` lists only `Ctrl+d,Ctrl+c`, but PSReadLine dispatches `Ctrl+d` to it,
/// and binding `Ctrl+d` alone made the chord unreachable (measured 2026-10-16
/// through ConPTY on both hosts). The other rows still apply.
#[cfg(windows)]
#[test]
fn test_powershell_readline_layer_keeps_a_profiles_own_bindings() {
    on_each_powershell("profile bindings", |exe| {
        let profile = "Set-PSReadLineKeyHandler -Chord 'Ctrl+e' -Function ForwardChar; \
                       Set-PSReadLineKeyHandler -Chord 'Ctrl+y' -Function Undo; \
                       Set-PSReadLineKeyHandler -Chord 'Alt+b' -ScriptBlock { } -BriefDescription mine; \
                       Set-PSReadLineKeyHandler -Chord 'Ctrl+d,Ctrl+c' -Function CaptureScreen; \
                       Set-PSReadLineKeyHandler -Chord 'Ctrl+w,Ctrl+w' -Function CaptureScreen; ";
        let tab = bound_after(exe, profile, Pwsh::InTab, &[]);
        assert!(tab.loaded);
        for (chord, function) in [
            ("Ctrl+e", "ForwardChar"),
            ("Ctrl+y", "Undo"),
            ("Alt+b", "mine"),
            ("Ctrl+d,Ctrl+c", "CaptureScreen"),
            ("Ctrl+w,Ctrl+w", "CaptureScreen"),
            ("Ctrl+a", "BeginningOfLine"),
            ("Ctrl+k", "KillLine"),
            ("Alt+f", "ForwardWord"),
        ] {
            assert_eq!(tab.get(chord), Some(function), "{exe:?}: {chord}");
        }
        for first in ["Ctrl+d", "Ctrl+w"] {
            assert_eq!(
                tab.get(first),
                None,
                "{exe:?}: {first} starts the profile's chord and stays bound to it"
            );
        }
    });
}

/// A profile that chose Emacs or Vi gets exactly what it chose.
#[cfg(windows)]
#[test]
fn test_powershell_readline_layer_leaves_emacs_and_vi_alone() {
    on_each_powershell("emacs and vi", |exe| {
        for mode in ["Emacs", "Vi"] {
            let profile = format!("Set-PSReadLineOption -EditMode {mode}; ");
            let chosen = bound_after(exe, &profile, Pwsh::Bare, &[]);
            let tab = bound_after(exe, &profile, Pwsh::InTab, &[]);
            assert_eq!(tab.mode, mode);
            assert!(tab.loaded, "{exe:?}: the integration ran");
            assert_eq!(tab.keys, chosen.keys, "{exe:?}: EditMode {mode} untouched");
        }
    });
}

/// The Emacs mode of aterm's own legacy block is taken back: in an aterm tab whose
/// profile holds [`LEGACY_EMACS_BLOCK`], as found or saved with CRLF and trailing
/// blanks among other lines, the shell gets stock Windows mode plus exactly the
/// rows, as a tab with no profile does. Without the change the block kept Emacs
/// and no row (measured 2026-10-18 in a live 5.1 tab: 18 of 62 key checks failed,
/// Alt+U/L/C typing u/l/c among them).
#[cfg(windows)]
#[test]
fn test_powershell_readline_layer_takes_back_atermss_legacy_emacs_block() {
    on_each_powershell("legacy Emacs block", |exe| {
        let stock = bound_after(exe, "", Pwsh::Bare, &[]);
        let mut expected = stock.map();
        for (chord, function, _) in expected_layer(&stock.has) {
            expected.insert(chord.to_owned(), function.to_owned());
        }
        let saved_by_an_editor = LEGACY_EMACS_BLOCK.replace('\n', " \r\n");
        for (what, profiles) in [
            (
                "the block as found, in profile.ps1",
                vec![("CurrentUserAllHosts", LEGACY_EMACS_BLOCK.to_owned())],
            ),
            (
                "the block with CRLF and trailing blanks among other lines",
                vec![
                    ("CurrentUserAllHosts", "# my profile\r\n".to_owned()),
                    (
                        "CurrentUserCurrentHost",
                        format!(
                            "$global:__mine = 1\r\n{saved_by_an_editor}\
                             Set-PSReadLineOption -HistorySaveStyle SaveNothing\r\n\
                             function global:prompt {{ 'P> ' }}\r\n"
                        ),
                    ),
                ],
            ),
        ] {
            let world = PowerShellWorld::new();
            for (name, text) in &profiles {
                world.write_profile(name, text);
            }
            let played = bound_after_in(&world, exe, PLAY_PROFILES, Pwsh::Bare, &[]);
            assert_eq!(
                played.mode, "Emacs",
                "{exe:?}, {what}: the block sets Emacs"
            );
            let tab = bound_after_in(&world, exe, PLAY_PROFILES, Pwsh::InTab, &[]);
            assert!(tab.loaded);
            assert_eq!(tab.mode, "Windows", "{exe:?}, {what}");
            assert_eq!(
                tab.map(),
                expected,
                "{exe:?}, {what}: stock Windows mode plus exactly the rows"
            );
        }
    });
}

/// An Emacs mode the user chose stays exactly as the profile left it, block or no
/// block: the user's own line, the block beside a comment naming EditMode (the
/// documented way to keep Emacs with the block in place) or edited, the block with
/// a key binding the switch to Windows mode would reset, in the profile text or
/// bound as a script block by a module, the opt-out, and a shell outside aterm.
#[cfg(windows)]
#[test]
fn test_powershell_readline_layer_keeps_an_emacs_the_user_chose() {
    /// One shell whose Emacs mode must survive the integration.
    struct Chosen {
        what: &'static str,
        /// `(the $PROFILE property naming the file, its text)`.
        profiles: Vec<(&'static str, String)>,
        /// What runs after the profiles and before the integration (a module).
        module: &'static str,
        env: &'static [(&'static str, &'static str)],
        how: Pwsh,
    }
    let block = || ("CurrentUserAllHosts", LEGACY_EMACS_BLOCK.to_owned());
    let in_tab = |what, profiles| Chosen {
        what,
        profiles,
        module: "",
        env: &[],
        how: Pwsh::InTab,
    };
    let cases = [
        in_tab(
            "the user's own Emacs line",
            vec![(
                "CurrentUserAllHosts",
                "Set-PSReadLineOption -EditMode Emacs\n".to_owned(),
            )],
        ),
        in_tab(
            "the block and a comment naming EditMode",
            vec![(
                "CurrentUserAllHosts",
                format!("{LEGACY_EMACS_BLOCK}# EditMode Emacs: mine\n"),
            )],
        ),
        in_tab(
            "an edited block",
            vec![(
                "CurrentUserAllHosts",
                LEGACY_EMACS_BLOCK.replacen(
                    "emacs line-editing so",
                    "emacs line-editing, mine, so",
                    1,
                ),
            )],
        ),
        in_tab(
            "the block and a key binding in the next profile",
            vec![
                block(),
                (
                    "CurrentUserCurrentHost",
                    // The commonest binding, which the switch would reset to
                    // Windows mode's TabCompleteNext.
                    "Set-PSReadLineKeyHandler -Key Tab -Function MenuComplete\n".to_owned(),
                ),
            ],
        ),
        Chosen {
            module: "Set-PSReadLineKeyHandler -Chord 'Ctrl+q' -ScriptBlock { } \
                     -BriefDescription module; ",
            ..in_tab("the block and a module's script block", vec![block()])
        },
        Chosen {
            env: &[("ATERM_DISABLE_READLINE_KEYS", "1")],
            ..in_tab("the block and the opt-out", vec![block()])
        },
        Chosen {
            how: Pwsh::Outside,
            ..in_tab("the block outside aterm", vec![block()])
        },
    ];
    on_each_powershell("an Emacs of the user's", |exe| {
        for case in &cases {
            let what = case.what;
            let world = PowerShellWorld::new();
            for (name, text) in &case.profiles {
                world.write_profile(name, text);
            }
            let prelude = format!("{PLAY_PROFILES}{}", case.module);
            let chosen = bound_after_in(&world, exe, &prelude, Pwsh::Bare, &[]);
            assert_eq!(
                chosen.mode, "Emacs",
                "{exe:?}, {what}: the profile sets Emacs"
            );
            let tab = bound_after_in(&world, exe, &prelude, case.how, case.env);
            assert!(tab.loaded, "{exe:?}, {what}: the integration ran");
            assert_eq!(tab.mode, "Emacs", "{exe:?}, {what}");
            assert_eq!(tab.keys, chosen.keys, "{exe:?}, {what}: Emacs untouched");
        }
    });
}

/// `ATERM_DISABLE_READLINE_KEYS` keeps stock Windows mode in an aterm tab, and a
/// shell outside aterm that sources the script keeps it too.
#[cfg(windows)]
#[test]
fn test_powershell_readline_layer_honours_the_opt_out_and_stays_out_of_other_terminals() {
    on_each_powershell("opt-out and outside", |exe| {
        let stock = bound_after(exe, "", Pwsh::Bare, &[]);
        let opted_out = bound_after(
            exe,
            "",
            Pwsh::InTab,
            &[("ATERM_DISABLE_READLINE_KEYS", "1")],
        );
        assert!(opted_out.loaded);
        assert_eq!(opted_out.get("Ctrl+a"), Some("SelectAll"), "{exe:?}");
        assert_eq!(
            opted_out.keys, stock.keys,
            "{exe:?}: the opt-out binds nothing"
        );
        let outside = bound_after(exe, "", Pwsh::Outside, &[]);
        assert!(outside.loaded);
        assert_eq!(
            outside.keys, stock.keys,
            "{exe:?}: outside aterm binds nothing"
        );
    });
}

// ─── Ctrl+E pressed at a real prompt ───
//
// The tests above read what PSReadLine has BOUND; they run under `-NonInteractive`,
// where no ReadLine runs and no key can be pressed. What Ctrl+E is for, accepting
// the grey inline suggestion, happens only at a live prompt, so this test types at
// one: a PowerShell in a ConPTY from aterm-pty, the seam a tab spawns through, fed
// the bytes aterm writes for the keys, with the history predictor on where
// PSReadLine has it. Its history is the oracle: every line ends with Enter and is a
// comment, so nothing runs, and the profile's dump function writes what history
// holds into the run's own folder. That profile also points the history file into
// that folder and saves nothing, so the user's PSReadLine history is never read or
// written.

/// The lines typed first, so the history predictor has something to suggest.
#[cfg(windows)]
const PRESS_SEEDS: &[&str] = &[
    "#S1 accept me fully",
    "#R5 alpha beta gamma delta",
    "#S6 one two three four",
];

/// Each line typed after the seeds, as the bytes aterm writes (Ctrl+E is 0x05, Left
/// `CSI D`), Enter not included, and the line history holds after Enter: where
/// PSReadLine has `AcceptSuggestion` (pwsh 7's 2.4.5, predictions on), and where it
/// does not (5.1's 2.0.0, where Ctrl+E is EndOfLine). The middle-of-the-line rows are
/// zsh-autosuggestions' rule; the block that accepted at every press failed them
/// (measured 2026-10-16: `#R5 alpha beta` with the caret five left of the end
/// submitted `#R5 alpha beta gamma delta`).
#[cfg(windows)]
const PRESSES: &[(&str, &str, &str)] = &[
    // At the end of the line with a suggestion showing: Ctrl+E accepts it.
    ("#S1 acc\x05X", "#S1 accept me fullyX", "#S1 accX"),
    // Five left of the end: Ctrl+E only moves, so Enter runs what was typed.
    (
        "#R5 alpha beta\x1b[D\x1b[D\x1b[D\x1b[D\x1b[D\x05",
        "#R5 alpha beta",
        "#R5 alpha beta",
    ),
    // Inside the line, a second press, now at the end, accepts.
    (
        "#S6 one tw\x1b[D\x1b[D\x1b[D\x05\x05X",
        "#S6 one two three fourX",
        "#S6 one twX",
    ),
    // One press inside the line, then a key: moved, nothing accepted.
    ("#S6 one t\x1b[D\x1b[D\x05X", "#S6 one tX", "#S6 one tX"),
    // No suggestion matches: EndOfLine.
    (
        "#V4 one two\x1b[D\x1b[D\x1b[D\x05Q",
        "#V4 one twoQ",
        "#V4 one twoQ",
    ),
];

/// A PowerShell at a real prompt in a ConPTY from aterm-pty, with a reader thread
/// passing on what the console draws. Dropping it hangs the shell up and reaps it,
/// so a failed wait leaves nothing running.
#[cfg(windows)]
struct ConptyShell {
    master: i32,
    pid: i32,
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
    /// Everything the console has drawn so far.
    seen: Vec<u8>,
}

// `include!`d into a `#[cfg(test)]` module already; the gate is spelled here too for the
// source censuses, which read this file on its own (the root-authority mint and a thread
// spawn are test-only here).
#[cfg(test)]
#[cfg(windows)]
impl ConptyShell {
    /// A wait for something that must happen: a hang detector, not a latency budget.
    const HANG: std::time::Duration = std::time::Duration::from_secs(60);

    fn spawn(
        world: &PowerShellWorld,
        argv: &[std::ffi::OsString],
        env_add: &[(String, String)],
    ) -> Self {
        let mut env_add = env_add.to_vec();
        env_add.extend(world.env());
        let cwd = world.root.path().to_str().expect("UTF-8 fixture cwd");
        // SAFETY: a test process is its own trusted launcher, as aterm-pty's
        // tests/windows_smoke.rs is; the caps reach only this spawn.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let shell = aterm_pty::spawn_shell_with_pid(
            50,
            200,
            &authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted),
            &authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted),
            &env_add,
            None,
            None,
            None,
            Some(argv),
            Some(cwd),
            None,
            aterm_sandbox::Limits::inherit(),
        )
        .unwrap_or_else(|error| panic!("ConPTY spawn of {argv:?}: {error}"));
        let master = shell.master;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("conpty-reader".to_owned())
            .spawn(move || {
                let mut buf = [0u8; 4096];
                // 0 is EOF (the shell exited) and a negative count an error; the
                // sender drops with the thread, which the waits read as the end.
                while let Ok(n @ 1..) = usize::try_from(aterm_pty::read(master, &mut buf)) {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            })
            .expect("the ConPTY reader thread");
        Self {
            master,
            pid: shell.pid,
            rx,
            seen: Vec::new(),
        }
    }

    fn type_keys(&self, keys: &str) {
        aterm_pty::write_all(self.master, keys.as_bytes());
    }

    /// Wait until the console has drawn `needle`.
    fn wait_for(&mut self, needle: &str, what: &str) {
        let deadline = std::time::Instant::now() + Self::HANG;
        while !self
            .seen
            .windows(needle.len())
            .any(|window| window == needle.as_bytes())
        {
            match self
                .rx
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            {
                Ok(chunk) => self.seen.extend_from_slice(&chunk),
                Err(error) => panic!(
                    "{what}: {error:?} before the console drew {needle:?}; it drew: {}",
                    self.drawn()
                ),
            }
        }
    }

    /// Wait until the shell has exited (the reader saw EOF).
    fn wait_for_exit(&mut self, what: &str) {
        let deadline = std::time::Instant::now() + Self::HANG;
        loop {
            match self
                .rx
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            {
                Ok(chunk) => self.seen.extend_from_slice(&chunk),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => panic!(
                    "{what}: the shell did not exit; the console drew: {}",
                    self.drawn()
                ),
            }
        }
    }

    /// The end of what the console drew, for a failure message.
    fn drawn(&self) -> String {
        let tail = &self.seen[self.seen.len().saturating_sub(3000)..];
        format!("{:?}", String::from_utf8_lossy(tail))
    }
}

#[cfg(windows)]
impl Drop for ConptyShell {
    fn drop(&mut self) {
        aterm_pty::hangup(self.pid);
        aterm_pty::reap(self.pid);
        aterm_pty::close_master(self.master);
    }
}

/// Ctrl+E pressed at a real prompt in an aterm tab: with the caret at the end of the
/// line it accepts the inline suggestion where PSReadLine has `AcceptSuggestion`;
/// from inside the line the first press moves to the end and the second accepts, so
/// Ctrl+E then Enter runs what was typed; with no suggestion, and on 5.1's PSReadLine
/// 2.0.0, it is EndOfLine. Each line is typed only once its prompt has drawn: the
/// profile's prompt counts itself (`K1> `, `K2> ` ...), so the keys of one line are
/// never queued behind the last, and the few keys after a line's last edit are
/// within the ten PSReadLine reads before it skips a render (and the suggestion
/// lookup with it).
#[cfg(windows)]
#[test]
fn test_powershell_ctrl_e_accepts_a_suggestion_only_at_the_end_of_the_line() {
    on_each_powershell("Ctrl+E presses", |exe| {
        let world = PowerShellWorld::new();
        let dir = &world.root;
        // Inside a PowerShell single-quoted string.
        let quoted = |path: &Path| path.display().to_string().replace('\'', "''");
        let result = dir.path().join("result.txt");
        let profile = dir.path().join("profile.ps1");
        let script = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("scripts")
            .join("aterm_shell_integration.ps1");
        // The user's profile, which a tab runs before the integration: history kept
        // in this folder and never saved, the history predictor inline where
        // PSReadLine has one, the counting prompt, and the dump.
        let body = [
            format!(
                "Set-PSReadLineOption -HistorySavePath '{}' -HistorySaveStyle SaveNothing",
                quoted(&dir.path().join("history.txt"))
            ),
            "if ([Microsoft.PowerShell.PSConsoleReadLine].GetMethod('AcceptSuggestion')) { \
             Set-PSReadLineOption -PredictionSource History }"
                .to_owned(),
            "if ((Get-Command Set-PSReadLineOption).Parameters.ContainsKey('PredictionViewStyle')) { \
             Set-PSReadLineOption -PredictionViewStyle InlineView }"
                .to_owned(),
            "$global:__keys_prompts = 0".to_owned(),
            "function global:prompt { $global:__keys_prompts += 1; 'K' + $global:__keys_prompts + '> ' }"
                .to_owned(),
            format!(
                "function global:__keys_dump {{ \
                 $l = @('has=' + [bool][Microsoft.PowerShell.PSConsoleReadLine].GetMethod('AcceptSuggestion')); \
                 $l += 'version=' + (Get-Module PSReadLine).Version; \
                 foreach ($h in @(Get-PSReadLineKeyHandler -Bound)) {{ \
                 if ($h.Key -ceq 'Ctrl+e') {{ $l += 'ctrl_e=' + $h.Function }} }}; \
                 foreach ($h in [Microsoft.PowerShell.PSConsoleReadLine]::GetHistoryItems()) {{ \
                 $l += 'line=' + $h.CommandLine }}; \
                 [System.IO.File]::WriteAllLines('{}', [string[]]$l) }}",
                quoted(&result)
            ),
            format!(". '{}'", quoted(&script)),
        ]
        .join("\r\n");
        std::fs::write(&profile, body).expect("write the profile");
        let command = format!("{}. '{}'", PowerShellWorld::home_guard(), quoted(&profile));
        let argv: Vec<std::ffi::OsString> = [
            exe.as_os_str(),
            "-NoProfile".as_ref(),
            "-NoLogo".as_ref(),
            "-NoExit".as_ref(),
            "-ExecutionPolicy".as_ref(),
            "Bypass".as_ref(),
            "-Command".as_ref(),
            command.as_ref(),
        ]
        .into_iter()
        .map(std::ffi::OsStr::to_os_string)
        .collect();
        let env_add: Vec<(String, String)> = [
            ("ATERM_CHILD", "1"),
            // Inherited by a suite run inside aterm, these would skip the script
            // (the loaded guard) or the layer (the opt-out); empty reads as unset.
            ("ATERM_SHELL_INTEGRATION_INSTALLED", ""),
            ("ATERM_DISABLE_READLINE_KEYS", ""),
            // Empty, each PowerShell builds its own module path, as in
            // `powershell_output`: 5.1 then loads its own PSReadLine 2.0.0.
            ("PSModulePath", ""),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();

        let mut shell = ConptyShell::spawn(&world, &argv, &env_add);
        let lines = PRESS_SEEDS
            .iter()
            .copied()
            .chain(PRESSES.iter().map(|&(keys, _, _)| keys));
        // `K<n>>` without the space: the console need not draw a trailing blank.
        for (n, keys) in lines.enumerate() {
            shell.wait_for(&format!("K{}>", n + 1), &format!("{exe:?} line {n}"));
            shell.type_keys(&format!("{keys}\r"));
        }
        shell.wait_for(
            &format!("K{}>", PRESS_SEEDS.len() + PRESSES.len() + 1),
            &format!("{exe:?} after the presses"),
        );
        shell.type_keys("__keys_dump; exit\r");
        shell.wait_for_exit(&format!("{exe:?} dump"));
        let drawn = shell.drawn();
        drop(shell);

        let dump = std::fs::read_to_string(&result)
            .unwrap_or_else(|error| panic!("{exe:?} wrote no dump ({error}); it drew: {drawn}"));
        let field = |name: &str| -> Vec<&str> {
            dump.lines()
                .filter_map(|line| line.strip_prefix(name))
                .collect()
        };
        let has = field("has=") == ["True"];
        let version = field("version=").concat();
        let history = field("line=");
        // Windows PowerShell 5.1 also puts the -Command it started with into
        // history, ahead of the typed lines; pwsh does not (measured 2026-10-16).
        let history = history
            .strip_prefix(&[command.as_str()][..])
            .unwrap_or(&history);
        let expected: Vec<&str> = PRESS_SEEDS
            .iter()
            .copied()
            .chain(
                PRESSES
                    .iter()
                    .map(|&(_, accepted, plain)| if has { accepted } else { plain }),
            )
            .chain(["__keys_dump; exit"])
            .collect();
        assert_eq!(
            history, expected,
            "{exe:?} PSReadLine {version} (AcceptSuggestion: {has}): the lines Enter \
             submitted; the console drew: {drawn}"
        );
        assert_eq!(
            field("ctrl_e="),
            [if has { ACCEPT_THEN_END } else { "EndOfLine" }],
            "{exe:?} PSReadLine {version}: Ctrl+e's binding"
        );
    });
}

/// Each line typed at a real prompt for the Alt+U/L/C test, as the bytes aterm
/// writes (Home `CSI H`, Right `CSI C`, Alt+u `ESC u`), Enter not included, and
/// the line history holds after Enter. Every line is a comment, so nothing runs.
#[cfg(windows)]
const CASE_PRESSES: &[(&str, &str)] = &[
    // From the start of a word to its end, the caret left after it.
    (
        "# alpha beta gamma\x1b[H\x1b[C\x1b[C\x1buX",
        "# ALPHAX beta gamma",
    ),
    (
        "# ALPHA beta gamma\x1b[H\x1b[C\x1b[C\x1blX",
        "# alphaX beta gamma",
    ),
    (
        "# alpha beta gamma\x1b[H\x1b[C\x1b[C\x1bcX",
        "# AlphaX beta gamma",
    ),
    // From the space before a word: the next word, mixed case capitalized.
    ("# hELLO world\x1b[H\x1b[C\x1bcX", "# HelloX world"),
    // From inside a word, to its end.
    (
        "# alpha beta\x1b[H\x1b[C\x1b[C\x1b[C\x1b[C\x1buX",
        "# alPHAX beta",
    ),
];

/// Alt+U/L/C pressed at a real prompt in an aterm tab case the word from the caret,
/// as readline's do, on every PSReadLine: 2.4.5's own UpcaseWord, DowncaseWord and
/// CapitalizeWord, and the layer's script blocks on 5.1's 2.0.0, where Windows mode
/// typed a bare u, l or c (`# uXalpha beta gamma`, measured 2026-10-18).
#[cfg(windows)]
#[test]
fn test_powershell_alt_u_l_c_case_the_word_on_every_psreadline() {
    on_each_powershell("Alt+U/L/C presses", |exe| {
        let world = PowerShellWorld::new();
        let dir = &world.root;
        let quoted = |path: &Path| path.display().to_string().replace('\'', "''");
        let result = dir.path().join("result.txt");
        let profile = dir.path().join("profile.ps1");
        let script = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("scripts")
            .join("aterm_shell_integration.ps1");
        // History kept in this folder and never saved, no predictor, the counting
        // prompt, and the dump.
        let body = [
            format!(
                "Set-PSReadLineOption -HistorySavePath '{}' -HistorySaveStyle SaveNothing",
                quoted(&dir.path().join("history.txt"))
            ),
            "if ((Get-Command Set-PSReadLineOption).Parameters.ContainsKey('PredictionSource')) { \
             Set-PSReadLineOption -PredictionSource None }"
                .to_owned(),
            "$global:__keys_prompts = 0".to_owned(),
            "function global:prompt { $global:__keys_prompts += 1; 'K' + $global:__keys_prompts + '> ' }"
                .to_owned(),
            format!(
                "function global:__keys_dump {{ \
                 $l = @('version=' + (Get-Module PSReadLine).Version); \
                 foreach ($h in [Microsoft.PowerShell.PSConsoleReadLine]::GetHistoryItems()) {{ \
                 $l += 'line=' + $h.CommandLine }}; \
                 [System.IO.File]::WriteAllLines('{}', [string[]]$l) }}",
                quoted(&result)
            ),
            format!(". '{}'", quoted(&script)),
        ]
        .join("\r\n");
        std::fs::write(&profile, body).expect("write the profile");
        let command = format!("{}. '{}'", PowerShellWorld::home_guard(), quoted(&profile));
        let argv: Vec<std::ffi::OsString> = [
            exe.as_os_str(),
            "-NoProfile".as_ref(),
            "-NoLogo".as_ref(),
            "-NoExit".as_ref(),
            "-ExecutionPolicy".as_ref(),
            "Bypass".as_ref(),
            "-Command".as_ref(),
            command.as_ref(),
        ]
        .into_iter()
        .map(std::ffi::OsStr::to_os_string)
        .collect();
        let env_add: Vec<(String, String)> = [
            ("ATERM_CHILD", "1"),
            ("ATERM_SHELL_INTEGRATION_INSTALLED", ""),
            ("ATERM_DISABLE_READLINE_KEYS", ""),
            ("PSModulePath", ""),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();

        let mut shell = ConptyShell::spawn(&world, &argv, &env_add);
        for (n, &(keys, _)) in CASE_PRESSES.iter().enumerate() {
            shell.wait_for(&format!("K{}>", n + 1), &format!("{exe:?} line {n}"));
            shell.type_keys(&format!("{keys}\r"));
        }
        shell.wait_for(
            &format!("K{}>", CASE_PRESSES.len() + 1),
            &format!("{exe:?} after the presses"),
        );
        shell.type_keys("__keys_dump; exit\r");
        shell.wait_for_exit(&format!("{exe:?} dump"));
        let drawn = shell.drawn();
        drop(shell);

        let dump = std::fs::read_to_string(&result)
            .unwrap_or_else(|error| panic!("{exe:?} wrote no dump ({error}); it drew: {drawn}"));
        let field = |name: &str| -> Vec<&str> {
            dump.lines()
                .filter_map(|line| line.strip_prefix(name))
                .collect()
        };
        let version = field("version=").concat();
        let history = field("line=");
        // 5.1 puts the -Command it started with into history first.
        let history = history
            .strip_prefix(&[command.as_str()][..])
            .unwrap_or(&history);
        let expected: Vec<&str> = CASE_PRESSES
            .iter()
            .map(|&(_, line)| line)
            .chain(["__keys_dump; exit"])
            .collect();
        assert_eq!(
            history, expected,
            "{exe:?} PSReadLine {version}: the lines Enter submitted; the console drew: {drawn}"
        );
    });
}

/// Exercise the actual hook loader against two owned homes. Even an explicit
/// ordinary-env override cannot make the child source the decoy's hook.
#[cfg(windows)]
#[test]
fn test_powershell_children_source_only_their_exclusive_home_hooks() {
    on_each_powershell("private home hooks", |exe| {
        let world = PowerShellWorld::new();
        let decoy = aterm_tempfile::tempdir().expect("owned decoy home");
        let private_marker = world.root.path().join("private-hook-ran");
        let decoy_marker = decoy.path().join("decoy-hook-ran");
        let quoted = |path: &Path| {
            path.to_str()
                .expect("UTF-8 fixture path")
                .replace('\'', "''")
        };
        for (home, name, marker, tail) in [
            (world.home(), "private", &private_marker, ""),
            (
                decoy.path().to_owned(),
                "decoy",
                &decoy_marker,
                "; throw 'decoy hook must not run'",
            ),
        ] {
            let hooks = home.join(".aterm/shell.d");
            std::fs::create_dir_all(&hooks).expect("owned hook directory");
            std::fs::write(
                hooks.join("00-owned.ps1"),
                format!(
                    "[System.IO.File]::WriteAllText('{}', '{}'); $global:__fixture_hook = '{}'{}",
                    quoted(marker),
                    name,
                    name,
                    tail
                ),
            )
            .expect("owned hook");
        }
        let decoy_path = decoy.path().to_str().expect("UTF-8 decoy");
        let output = powershell_output_in(
            &world,
            exe,
            "",
            "[Console]::Write($HOME + '|' + $env:USERPROFILE + '|' + $env:APPDATA + '|' + $global:__fixture_hook)",
            None,
            Pwsh::InTab,
            &[
                ("HOME", decoy_path),
                ("USERPROFILE", decoy_path),
                ("APPDATA", decoy_path),
            ],
        );
        assert!(!decoy_marker.exists(), "the decoy hook was never sourced");
        assert!(
            output.status.success(),
            "{exe:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            std::fs::read(&private_marker).expect("actual private hook ran"),
            b"private"
        );
        assert_eq!(
            String::from_utf8(output.stdout).expect("UTF-8 probe"),
            format!(
                "{}|{}|{}|private",
                world.home().display(),
                world.home().display(),
                world.root.path().join("roaming").display()
            )
        );
    });
}
