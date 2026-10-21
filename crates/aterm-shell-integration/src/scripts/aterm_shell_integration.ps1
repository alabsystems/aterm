# aterm_shell_integration.ps1 - Shell integration for aTerm
#
# Copyright 2026 Andrew Yates
# Author: Andrew Yates
# Licensed under the Apache License, Version 2.0
#
# Dot-source this file from your PowerShell profile, or let aterm inject it:
#   pwsh -NoExit -Command ". (Join-Path $env:ATERM_SHELL_INTEGRATION_DIR 'aterm_shell_integration.ps1')"
#
# Features enabled:
# - Directory tracking (OSC 7): tab title updates, "Open Terminal Here" support
# - Command tracking (OSC 133): command history indexing, timing, notifications
# - The managed dirs, LIVE: an already-running session shell resolves claude/codex
#   to atpkg's <prefix>/agents twins the moment atpkg lays them - no new tab
#   (owner ask 2026-09-16; see LIVE below)
# - Key bindings: the readline chords (Ctrl+A/E/J/K/U/W/Y, Alt+B/F, ...) layered
#   over PSReadLine's Windows edit mode in an aterm tab; a key the profile bound
#   to anything but its Windows-mode default keeps that binding, the Emacs block
#   an earlier aterm setup wrote into a profile gives way to it, and
#   $env:ATERM_DISABLE_READLINE_KEYS = '1' in $PROFILE turns the layer off
#   (see KEY BINDINGS below)
#
# Compatible with: Windows PowerShell 5.1 and PowerShell 7+ (pwsh) on any OS.
# ASCII only: aterm writes this file without a BOM, and Windows PowerShell 5.1
# decodes BOM-less source as ANSI, so non-ASCII bytes would be misread.

# AN OLDER SCRIPT'S ~/.aterm/bin, taken off (2026-10-19). Every script before
# 613305828c (2026-10-16) set $__aterm_bin to $HOME/.aterm/bin at its first load
# in a pwsh, left it set, and put that folder FIRST on PATH where it existed;
# none since does (the package block below says why). The guard just below ends
# a second load, so such a pwsh kept the entry until it exited, even with this
# file dot-sourced into it again (a profile re-run after an update). The entry
# goes where the pwsh PROVES that script put it there, and nowhere else (a
# v0.106.0 pwsh, say, where a ~/.aterm/bin on PATH is the user's own):
# $__aterm_bin - a variable, which no nested pwsh inherits and nothing since
# 613305828c sets - says that script loaded THIS pwsh and names what it
# prepended, and a folder older than the process (its positive creation time,
# or last write only when creation is unavailable) says it was there. Then
# one entry goes, the first equal to it, compared ordinally, and nothing else
# moves: a copy the user's own profile added keeps the folder on PATH. The
# variable goes too, so this runs once.
if ((Test-Path -LiteralPath Variable:\__aterm_bin) -and ($__aterm_bin -is [string])) {
    $__aterm_old = $__aterm_bin
    Remove-Variable -Name __aterm_bin -ErrorAction SilentlyContinue
    Remove-Variable -Name __aterm_bin -Scope Global -ErrorAction SilentlyContinue
    $__aterm_item = Get-Item -LiteralPath $__aterm_old -Force -ErrorAction SilentlyContinue
    if ($__aterm_item) {
        $__aterm_born = $__aterm_item.CreationTimeUtc
        if ($__aterm_born -le [DateTime]::new(1970, 1, 1, 0, 0, 0, [DateTimeKind]::Utc)) {
            $__aterm_born = $__aterm_item.LastWriteTimeUtc
        }
        $__aterm_start = [System.Diagnostics.Process]::GetCurrentProcess().StartTime
        if ($__aterm_born -lt $__aterm_start.ToUniversalTime()) {
            $__aterm_sep = [string][System.IO.Path]::PathSeparator
            $__aterm_parts = [System.Collections.Generic.List[string]]::new(
                [string[]]($env:PATH -split [regex]::Escape($__aterm_sep)))
            $__aterm_at = $__aterm_parts.IndexOf($__aterm_old)
            if ($__aterm_at -ge 0) {
                $__aterm_parts.RemoveAt($__aterm_at)
                $env:PATH = $__aterm_parts -join $__aterm_sep
            }
        }
    }
}

# Skip if already loaded
if ($env:ATERM_SHELL_INTEGRATION_INSTALLED) { return }
$env:ATERM_SHELL_INTEGRATION_INSTALLED = '1'

# UTF-8 both ways between pwsh and native programs, in an aterm tab on Windows.
# pwsh decodes a native program's piped output with the console output code page
# (437 by default), so the UTF-8 that aterm's own CLI, git and most tools write
# came back as mojibake from `aterm help | Select-String`. The code page is the
# console's, so it stays UTF-8 for the rest of the tab, after pwsh exits too, and
# a legacy program that writes OEM-code-page bytes is the one misread there. The
# decision and the options it rejected: apps/aterm-win/README.md, "Known
# limitations".
if (($env:ATERM_CHILD -or $env:ATERM_SESSION_ID) -and
    ([System.Environment]::OSVersion.Platform -eq 'Win32NT')) {
    try {
        $__aterm_utf8 = New-Object System.Text.UTF8Encoding $false
        $Global:OutputEncoding = $__aterm_utf8
        # Throws where no console is attached; there is no code page to fix.
        [Console]::OutputEncoding = $__aterm_utf8
    }
    catch { }
}

# Source package shell hooks. No `~/.aterm/bin` goes on PATH: nothing creates or
# manages that folder, and an entry there ahead of everything would let whatever
# lands in it shadow every command in every session. atpkg's managed bin/ is
# APPENDED, by its own hook.
if (-not (Test-Path Env:AGY_CLI_FAST_PROMPT_EDITOR)) {
    $env:AGY_CLI_FAST_PROMPT_EDITOR = 'true'
}
if ($HOME) {
    $__aterm_hooks = Join-Path $HOME '.aterm/shell.d'
    if (Test-Path -LiteralPath $__aterm_hooks) {
        foreach ($__aterm_hook in (Get-ChildItem -LiteralPath $__aterm_hooks -Filter '*.ps1' -ErrorAction SilentlyContinue)) {
            . $__aterm_hook.FullName
        }
    }
}

# The agents directory, moved to the front ahead of the reroute block below.
# $env:ATPKG_AGENTS - set by the atpkg shell.d hook dot-sourced just above - names
# <prefix>/agents, which holds ONLY the agent shims aterm keeps current (claude,
# codex, agy, muse; owner decisions 2026-09-10 and 2026-10-15). An earlier PATH
# entry for one of those names wins otherwise (measured 2026-09-10 on macOS zsh:
# behind /opt/homebrew/bin and ~/.local/bin, so codex ran a brew cask that could
# run no command) - an ORDER failure, so move-to-front. aterm dot-sources this
# file AFTER the user's profile (the -Command it passes pwsh), so this runs after a
# profile's own prepends; a profile that dot-sources this file itself is re-fronted
# only at that point.
# Asserted before the reroute block, so PATH reads reroute, agents, ... - the spawn
# seam's own order. Inert when the variable is unset or names no directory.
#
# The reroute directory, FIRST. $env:ATERM_REROUTE_DIR is set by aterm's spawn
# seam: the session-scoped directory of stubs for the upstream Rust names
# (aterm help reroute), which the seam already put first on the PATH it handed
# this shell. The user's own profile runs AFTER that injected environment and
# may prepend over it, and so may the package blocks just above (measured
# 2026-09-07 on macOS: ~/.cargo/bin at position 17, ahead of the managed store
# at 19, so a bare cargo ran upstream Rust silently - an ORDER failure). So this
# is move-to-front: every existing occurrence is removed by equality, then the
# directory is prepended. Inert outside a session (variable unset) and on
# Windows, where no stubs are laid and the directory does not exist.
#
# Both moves live in one function so the LIVE hot path below can repeat them; it
# also records, in $Global:__aterm_managed_want, the dirs it put in front (in
# order), which the hot path compares the head of PATH against.
$Global:__aterm_managed_want = @()
$Global:__aterm_managed_agents_on = $false
$Global:__aterm_managed_reroute_on = $false
function Global:__aterm_reroute_path_front {
    $Global:__aterm_managed_want = @()
    $Global:__aterm_managed_agents_on = $false
    $Global:__aterm_managed_reroute_on = $false
    if ($env:ATPKG_AGENTS -and (Test-Path -LiteralPath $env:ATPKG_AGENTS -PathType Container)) {
        $__aterm_sep = [string][System.IO.Path]::PathSeparator
        $__aterm_rest = @(($env:PATH -split [regex]::Escape($__aterm_sep)) | Where-Object { $_ -ne $env:ATPKG_AGENTS })
        $env:PATH = (@($env:ATPKG_AGENTS) + $__aterm_rest) -join $__aterm_sep
        $Global:__aterm_managed_want = @($env:ATPKG_AGENTS)
        $Global:__aterm_managed_agents_on = $true
    }
    if ($env:ATERM_REROUTE_DIR -and (Test-Path -LiteralPath $env:ATERM_REROUTE_DIR -PathType Container)) {
        $__aterm_sep = [string][System.IO.Path]::PathSeparator
        $__aterm_rest = @(($env:PATH -split [regex]::Escape($__aterm_sep)) | Where-Object { $_ -ne $env:ATERM_REROUTE_DIR })
        $env:PATH = (@($env:ATERM_REROUTE_DIR) + $__aterm_rest) -join $__aterm_sep
        $Global:__aterm_managed_want = @($env:ATERM_REROUTE_DIR) + $Global:__aterm_managed_want
        $Global:__aterm_managed_reroute_on = $true
    }
}
__aterm_reroute_path_front

# --- LIVE: the tab that is ALREADY OPEN picks the managed dirs up the moment atpkg lays them ---
#
# Owner, 2026-09-16: "aterm atpkg DID install the latest but it didn't make them
# available for me. instead, it is telling me to open a new tab. NO! all the
# latest and best MUST WORK IN THE SAME TAB with live update!" Measured on macOS:
# a shell (pid 1784) spawned 10:44:24 by the previous app build and adopted across
# the seamless update (app 10:44:32) never saw the agents/ dir and shell.d hooks
# the new build created at 10:46 - the one assert above runs at load only. The
# same policy as the zsh/bash/fish scripts, best effort here: re-asserted from the
# prompt function and from the PSReadLine submit shim (the only preexec seam), the
# hook ~/.aterm/shell.d/00-atpkg.ps1 (crates/atpkg/src/hooks.rs) dot-sourced when
# $env:ATPKG_AGENTS is unset or its LastWriteTimeUtc moved since it was last
# sourced (a .NET call, no process), $env:ATERM_REROUTE_DIR derived as the
# sibling <dir of ATPKG_AGENTS>/reroute unless __ATERM_REROUTE_PASSTHROUGH is engaged
# (non-empty and not '0'), a dir that was absent when the front was last laid
# re-probed until it appears, and PATH re-fronted only when its head is not
# already reroute, agents. Gated on being inside an aterm session (ATERM_CHILD=1 or
# ATERM_SESSION_ID), never on ATERM_REROUTE_DIR, which the adopted shell lacks.
$Global:__aterm_atpkg_hook = $null
if ($HOME) { $Global:__aterm_atpkg_hook = Join-Path $HOME '.aterm/shell.d/00-atpkg.ps1' }
$Global:__aterm_atpkg_hook_seen = $null
$Global:__aterm_managed_live = [bool]($env:ATERM_CHILD -or $env:ATERM_SESSION_ID)
if ($Global:__aterm_managed_live -and $env:ATPKG_AGENTS -and $Global:__aterm_atpkg_hook -and [System.IO.File]::Exists($Global:__aterm_atpkg_hook)) {
    $Global:__aterm_atpkg_hook_seen = [System.IO.File]::GetLastWriteTimeUtc($Global:__aterm_atpkg_hook).Ticks
}

function Global:__aterm_managed_derive_reroute {
    if ($env:ATERM_REROUTE_DIR -or -not $env:ATPKG_AGENTS) { return }
    if ($env:__ATERM_REROUTE_PASSTHROUGH -and ($env:__ATERM_REROUTE_PASSTHROUGH -ne '0')) { return }
    $__aterm_parent = Split-Path -Parent $env:ATPKG_AGENTS
    if (-not $__aterm_parent) { return }
    $__aterm_dir = Join-Path $__aterm_parent 'reroute'
    if (Test-Path -LiteralPath $__aterm_dir -PathType Container) { $env:ATERM_REROUTE_DIR = $__aterm_dir }
}

function Global:__aterm_managed_path_live {
    if (-not $Global:__aterm_managed_live -or -not $Global:__aterm_atpkg_hook) { return }
    # 1. The hook.
    if ([System.IO.File]::Exists($Global:__aterm_atpkg_hook)) {
        $__aterm_stamp = [System.IO.File]::GetLastWriteTimeUtc($Global:__aterm_atpkg_hook).Ticks
        if ((-not $env:ATPKG_AGENTS) -or ($__aterm_stamp -ne $Global:__aterm_atpkg_hook_seen)) {
            $Global:__aterm_atpkg_hook_seen = $__aterm_stamp
            . $Global:__aterm_atpkg_hook
            __aterm_managed_derive_reroute
            __aterm_reroute_path_front
            return
        }
    }
    # 2. A dir that was absent when the front was last laid: probe it again (only
    #    in that degraded state), and front it the moment it appears.
    $__aterm_refront = $false
    if ((-not $Global:__aterm_managed_agents_on) -and $env:ATPKG_AGENTS -and (Test-Path -LiteralPath $env:ATPKG_AGENTS -PathType Container)) { $__aterm_refront = $true }
    if (-not $Global:__aterm_managed_reroute_on) {
        if ($env:ATERM_REROUTE_DIR) {
            if (Test-Path -LiteralPath $env:ATERM_REROUTE_DIR -PathType Container) { $__aterm_refront = $true }
        } else {
            __aterm_managed_derive_reroute
            if ($env:ATERM_REROUTE_DIR) { $__aterm_refront = $true }
        }
    }
    if ($__aterm_refront) { __aterm_reroute_path_front; return }
    # 3. The order: re-front only on a mismatch.
    $__aterm_n = $Global:__aterm_managed_want.Count
    if ($__aterm_n -eq 0) { return }
    $__aterm_sep = [string][System.IO.Path]::PathSeparator
    $__aterm_head = @(($env:PATH -split [regex]::Escape($__aterm_sep)) | Select-Object -First $__aterm_n)
    if ($__aterm_head.Count -eq $__aterm_n) {
        $__aterm_same = $true
        for ($__aterm_i = 0; $__aterm_i -lt $__aterm_n; $__aterm_i++) {
            if ($__aterm_head[$__aterm_i] -cne $Global:__aterm_managed_want[$__aterm_i]) { $__aterm_same = $false; break }
        }
        if ($__aterm_same) { return }
    }
    __aterm_reroute_path_front
}

# Capture the capability nonce into a PowerShell variable so we can
# immediately drop it from the environment (#8015). Leaving
# ATERM_SHELL_NONCE in the exported env lets every child process (env,
# ssh SendEnv, docker, python subprocess, ...) read the 64-hex secret that
# would be used to bypass the #7960 nonce-enforcement defense. PowerShell
# (non-env) variables are not inherited by child processes, so the
# captured copy stays in this session.
#
# If the env var is missing or empty at source-time, the captured nonce
# stays empty and __aterm_id_suffix falls through to the unnonced form
# (pre-nonce compatibility for hosts that have not yet authorized a
# nonce), exactly like the bash/zsh/fish scripts.
$Global:__aterm_shell_nonce = if ($env:ATERM_SHELL_NONCE) { $env:ATERM_SHELL_NONCE } else { '' }
if (Test-Path Env:ATERM_SHELL_NONCE) { Remove-Item Env:ATERM_SHELL_NONCE }

# Capability-nonce suffix for OSC 133/633 emissions (#7960, #7987, #8015).
# Expands to ";id=<64-hex>" when the captured nonce is non-empty, or to
# the empty string otherwise. Reads from the captured variable - never
# from the environment - so the nonce is not inherited by subprocesses.
function Global:__aterm_id_suffix {
    if ($Global:__aterm_shell_nonce) { ";id=$($Global:__aterm_shell_nonce)" } else { '' }
}

# Percent-encode a filesystem path for a file:// URI (RFC 3986).
# Unreserved chars (A-Z a-z 0-9 - _ . ~ /) pass through; everything else
# is encoded byte-by-byte over UTF-8 as %XX, matching the bash/zsh
# encoders. ':' also passes through (valid pchar; keeps the RFC 8089
# drive-letter convention file://host/C:/...). Backslashes normalize to
# '/' and drive-letter paths gain a leading '/' (C:\Users\x -> /C:/Users//x)
# so the URI path is absolute.
function Global:__aterm_osc7_path([string]$path) {
    $p = $path -replace '\\', '/'
    if (-not $p.StartsWith('/')) { $p = '/' + $p }
    $sb = New-Object System.Text.StringBuilder
    foreach ($b in [System.Text.Encoding]::UTF8.GetBytes($p)) {
        if (($b -ge 0x41 -and $b -le 0x5A) -or ($b -ge 0x61 -and $b -le 0x7A) -or
            ($b -ge 0x30 -and $b -le 0x39) -or
            $b -eq 0x2D -or $b -eq 0x2E -or $b -eq 0x2F -or $b -eq 0x3A -or
            $b -eq 0x5F -or $b -eq 0x7E) {
            [void]$sb.Append([char]$b)
        }
        else {
            [void]$sb.Append('%')
            [void]$sb.Append($b.ToString('X2'))
        }
    }
    $sb.ToString()
}

# Encode a command line for OSC 633;E (VS Code convention): backslash-hex
# encode semicolons, backslashes, and control/space bytes, matching the
# bash/zsh/fish encoders so a raw ESC/BEL in the command line cannot
# break out of the OSC string.
function Global:__aterm_encode_cmd([string]$cmd) {
    $sb = New-Object System.Text.StringBuilder
    foreach ($ch in $cmd.ToCharArray()) {
        $code = [int]$ch
        if ($ch -eq '\') { [void]$sb.Append('\\') }
        elseif ($ch -eq ';') { [void]$sb.Append('\x3b') }
        elseif ($code -le 0x20 -or $code -eq 0x7F) { [void]$sb.Append('\x' + $code.ToString('x2')) }
        else { [void]$sb.Append($ch) }
    }
    $sb.ToString()
}

# Preserve the user's prompt so ours wraps it instead of replacing it.
$Global:__aterm_original_prompt = $function:Prompt
# $null sentinel = "no prompt observed yet"; 0 = "prompt seen, history empty".
$Global:__aterm_last_history_id = $null

# --- The exit code of a line that failed ---
#
# $? says a line failed; which code it failed with has no one home. A native
# program sets $LASTEXITCODE, a cmdlet error and `throw` leave it alone, so
# after `cmd /c exit 7` every later failing cmdlet read as exit 7 too, until a
# native command succeeded (measured 2026-10-17 on pwsh 7.6.7 and Windows
# PowerShell 5.1: `Write-Error boom`, `throw "x"` and `Get-Item C:\nope` all
# reported 7). So the line's start is marked - $LASTEXITCODE and the newest
# $Error record, taken just before the line runs (the readline shim below, and
# the end of the prompt where PSReadLine is not loaded) - and a failed line
# reports, in this order:
#   - a $LASTEXITCODE that moved to a non-zero value: a native program failed;
#   - STATUS_CONTROL_C_EXIT (0xC000013A) for a line Ctrl+C stopped (its
#     history entry is Stopped), the code a console program stopped by Ctrl+C
#     exits with, so `Start-Sleep 60` reads as `ping -t` does;
#   - 0 for a line that holds no statement (a lone comment), which runs
#     nothing and leaves $? as the line before left it (measured: `#J1 alpha`
#     after a failed `Get-Item` read as an earlier ping's -1073741510);
#   - an unmoved non-zero $LASTEXITCODE when the line added no error record,
#     or when its newest record is the native program's own: the same native
#     failure again (`cmd /c exit 7` twice; Windows PowerShell 5.1 records a
#     native program's redirected stderr as NativeCommandError, and pwsh 7.4+
#     under $PSNativeCommandUseErrorActionPreference records the exit itself
#     as ProgramExitedWithNonZeroCode);
#   - else 1, a cmdlet's failure.
# A line that does not parse (a stray `}`) runs nothing and leaves $? as the
# line before left it, true or false, but its history entry is Failed: it is a
# failed line, never a statement-less one (measured 2026-10-17 on pwsh 7.6.7
# and Windows PowerShell 5.1: `}` after a success read 0).
# Nothing here assigns the user's $LASTEXITCODE or $Error.
$Global:__aterm_lec_before = $null
$Global:__aterm_err_before = $null
function Global:__aterm_mark_line_start {
    $Global:__aterm_lec_before = $global:LASTEXITCODE
    $Global:__aterm_err_before = $null
    if ($global:Error.Count -gt 0) { $Global:__aterm_err_before = $global:Error[0] }
}
function Global:__aterm_history_failed($history) {
    ($null -ne $history) -and ([string]$history.ExecutionStatus -eq 'Failed')
}
function Global:__aterm_failed_code($exit, $history) {
    $moved = ($null -ne $exit) -and ($exit -ne 0) -and ($exit -ne $Global:__aterm_lec_before)
    if ($moved) { return $exit }
    if (($null -ne $history) -and ([string]$history.ExecutionStatus -eq 'Stopped')) { return -1073741510 }
    if (($null -ne $history) -and -not (__aterm_history_failed $history)) {
        $ast = [System.Management.Automation.Language.Parser]::ParseInput([string]$history.CommandLine, [ref]$null, [ref]$null)
        if (($null -eq $ast.EndBlock) -or ($ast.EndBlock.Statements.Count -eq 0)) { return 0 }
    }
    $new_error = ($global:Error.Count -gt 0) -and -not ($global:Error[0] -eq $Global:__aterm_err_before)
    if (($null -ne $exit) -and ($exit -ne 0)) {
        if (-not $new_error) { return $exit }
        $newest = $global:Error[0]
        if (($newest -is [System.Management.Automation.ErrorRecord]) -and
            ([string]$newest.FullyQualifiedErrorId -match '^(NativeCommandError|NativeCommandErrorMessage|ProgramExitedWithNonZeroCode)(,|$)')) { return $exit }
    }
    1
}

function Global:Prompt {
    # Capture $? / $LASTEXITCODE first: any statement below would clobber them.
    $__aterm_ok = $global:?
    $__aterm_exit = $global:LASTEXITCODE
    Set-StrictMode -Off
    # The managed dirs, live (see LIVE above).
    __aterm_managed_path_live
    $__aterm_esc = [char]27
    $__aterm_bel = [char]7
    $__aterm_suffix = __aterm_id_suffix
    $__aterm_out = ''

    # Mark command completion (OSC 133;D;exitcode) - only when a command
    # actually ran (a new history entry appeared since the last prompt),
    # so Ctrl+C / Enter-on-empty do not emit a stray D.
    $__aterm_history = Get-History -Count 1
    $__aterm_hid = 0
    if ($null -ne $__aterm_history) { $__aterm_hid = $__aterm_history.Id }
    if (($null -ne $Global:__aterm_last_history_id) -and ($__aterm_hid -ne $Global:__aterm_last_history_id)) {
        $__aterm_code = 0
        # Which code a failed line reports: see "The exit code of a line that
        # failed" above.
        if ((-not $__aterm_ok) -or (__aterm_history_failed $__aterm_history)) { $__aterm_code = __aterm_failed_code $__aterm_exit $__aterm_history }
        $__aterm_out += "$__aterm_esc]133;D;$__aterm_code$__aterm_suffix$__aterm_bel"
    }
    $Global:__aterm_last_history_id = $__aterm_hid

    # Report current working directory (OSC 7) - filesystem paths only
    # (skip registry/cert/etc. provider locations).
    $__aterm_loc = $ExecutionContext.SessionState.Path.CurrentLocation
    if ($__aterm_loc.Provider.Name -eq 'FileSystem') {
        $__aterm_uri = "file://$([System.Environment]::MachineName)$(__aterm_osc7_path $__aterm_loc.ProviderPath)"
        # SIGNED first, then plain (see the zsh script's __aterm_report_cwd).
        if ($__aterm_suffix) {
            $__aterm_out += "$__aterm_esc]633;P;AtermCwd=$__aterm_uri$__aterm_suffix$__aterm_bel"
        }
        $__aterm_out += "$__aterm_esc]7;$__aterm_uri$__aterm_bel"

        # Set tab title to abbreviated CWD (OSC 0). Strip control chars so a
        # crafted directory name cannot smuggle a nested escape sequence.
        if (-not $env:ATERM_DISABLE_PROMPT_TITLES) {
            $__aterm_title = $__aterm_loc.ProviderPath
            if ($HOME -and ($__aterm_title -eq $HOME)) {
                $__aterm_title = '~'
            }
            elseif ($HOME -and $__aterm_title.StartsWith([string]$HOME + [System.IO.Path]::DirectorySeparatorChar)) {
                $__aterm_title = '~' + $__aterm_title.Substring(([string]$HOME).Length)
            }
            $__aterm_title = $__aterm_title -replace '[\x00-\x1f\x7f]', ''
            $__aterm_out += "$__aterm_esc]0;$__aterm_title$__aterm_bel"
        }
    }

    # Bracketed paste (2004) and in-band size reports (2048) off: PowerShell reads
    # neither, and a program killed with them on (a Claude Code, a vim) left
    # every later paste in ESC[200~ brackets and every resize typed as
    # ESC[48;...t at this prompt (measured 2026-10-18 under OpenConsole).
    $__aterm_out += $__aterm_esc + '[?2004l' + $__aterm_esc + '[?2048l'

    # Mark prompt start (OSC 133;A)
    $__aterm_out += "$__aterm_esc]133;A$__aterm_suffix$__aterm_bel"

    # Run the user's original prompt with $LASTEXITCODE / $? restored so
    # error-aware prompts (starship, posh-git, oh-my-posh) render correctly.
    $global:LASTEXITCODE = $__aterm_exit
    if ($null -ne $Global:__aterm_original_prompt) {
        if (-not $__aterm_ok) { Write-Error 'aterm: restore $? for user prompt' -ErrorAction Ignore }
        $__aterm_out += [string]($Global:__aterm_original_prompt.Invoke())
    }
    else {
        $__aterm_out += "PS $__aterm_loc> "
    }

    # Mark command line start (OSC 133;B) - after prompt, before user input
    $__aterm_out += "$__aterm_esc]133;B$__aterm_suffix$__aterm_bel"
    # PSReadLine's own readline in place of the shim: install the shim again
    # (see the readline shim below).
    $__aterm_readline = $function:PSConsoleHostReadLine
    if (($null -ne $__aterm_readline) -and ($null -ne $__aterm_readline.Module) -and ($__aterm_readline.Module.Name -eq 'PSReadLine')) {
        __aterm_install_readline_shim
    }
    # The next line's start, after the user's prompt ran (it may add errors);
    # the readline shim takes it again once the line is read.
    __aterm_mark_line_start
    $__aterm_out
}

# Mark command execution start (OSC 133;C) and report the command text
# (OSC 633;E). PSReadLine's readline shim is the only portable
# submit-time hook; without PSReadLine these two marks are skipped
# (prompt marks and cwd tracking above still work).
#
# The shim wraps whatever PSConsoleHostReadLine is in place when it is installed
# (a profile's own wrapper included). `Import-Module PSReadLine -Force` puts
# PSReadLine's own back over it, which left every later line unmarked, with no C
# and no E, until the shell restarted (measured 2026-10-19 on pwsh 7.6.7 /
# PSReadLine 2.4.5). So the prompt installs the shim again whenever the function
# in place is PSReadLine's own (its script block's module is PSReadLine): the
# line that re-imported it ran through the old shim, so no line goes unmarked,
# and a PSReadLine first imported after this script gets the shim too. A
# function other code put over the shim is left alone: it may call the shim,
# and wrapping it again would loop.
function Global:__aterm_install_readline_shim {
    $Global:__aterm_original_readline = $function:PSConsoleHostReadLine
    function Global:PSConsoleHostReadLine {
        $__aterm_line = [string]($Global:__aterm_original_readline.Invoke())
        # The managed dirs, live - BEFORE this line executes (see LIVE above).
        __aterm_managed_path_live
        # The line's start, as it runs: a completer or predictor that ran while
        # the line was edited may have moved $LASTEXITCODE or $Error.
        __aterm_mark_line_start
        if ($__aterm_line -and $__aterm_line.Trim()) {
            $__aterm_esc = [char]27
            $__aterm_bel = [char]7
            $__aterm_suffix = __aterm_id_suffix
            # Report command text for session memory (OSC 633;E)
            $__aterm_out = "$__aterm_esc]633;E;$(__aterm_encode_cmd $__aterm_line)$__aterm_suffix$__aterm_bel"
            # Set tab title to running command (OSC 0): the line, control characters
            # stripped, at most 256 characters; a longer one is its first 255 (never
            # half a surrogate pair) and an ellipsis, so the tab knows its end is cut
            # off and never shows a fragment of it as the file (the bash script's "The
            # command title").
            if (-not $env:ATERM_DISABLE_PROMPT_TITLES) {
                $__aterm_title = $__aterm_line
                if ($__aterm_title.Length -gt 256) {
                    $__aterm_cut = 255
                    if ([char]::IsHighSurrogate($__aterm_title[$__aterm_cut - 1])) { $__aterm_cut-- }
                    $__aterm_title = $__aterm_title.Substring(0, $__aterm_cut) + [char]0x2026
                }
                $__aterm_title = $__aterm_title -replace '[\x00-\x1f\x7f]', ''
                $__aterm_out += "$__aterm_esc]0;$__aterm_title$__aterm_bel"
            }
            $__aterm_out += "$__aterm_esc]133;C$__aterm_suffix$__aterm_bel"
            [Console]::Write($__aterm_out)
        }
        $__aterm_line
    }
}
if (Get-Module -Name PSReadLine) { __aterm_install_readline_shim }

# --- KEY BINDINGS: the readline chords, layered over PSReadLine's Windows mode ---
#
# A hand that learned the line editor in a macOS or Linux terminal reaches for
# readline's chords. PSReadLine's default edit mode on Windows, Windows, leaves
# Ctrl+E/K/U/B/F/P/N/T/D/_ unbound, makes Ctrl+A SelectAll and Ctrl+Y Redo, and
# types a literal b or f for Alt+B/F (measured 2026-10-16 through ConPTY with
# the bytes aterm's key encoder writes, on pwsh 7.6.7 / PSReadLine 2.4.5 and
# Windows PowerShell 5.1 / PSReadLine 2.0.0). Its Emacs mode was measured and
# rejected: it loses Ctrl+Left/Right word motion, makes Escape and Ctrl+X chord
# prefixes and Tab Complete. So the readline chords go over Windows mode, and
# Tab, Ctrl+Z/V/C/X, Ctrl+Left/Right, Ctrl+Backspace/Delete, Escape and Home/End
# stay as Windows has them. Alt+D is KillWord in Windows mode already.
#
# Ctrl+J is readline's accept-line, as on macOS and Linux. Its legacy byte, LF,
# reached PSReadLine as Ctrl+Enter, InsertLineAbove, so aterm writes it as the J
# key's record (char LF), which arrives as Ctrl+j, unbound in Windows mode. A
# program that reads the character still gets LF: Git Bash runs the line on it
# (measured 2026-10-17 through ConPTY on pwsh 7.6.7 / PSReadLine 2.4.5, Windows
# PowerShell 5.1 / PSReadLine 2.0.0 and Git Bash 5.3.9).
#
# A chord is bound only where it is unbound or still holds its stock Windows-mode
# function (each row's third column), so a key the user's profile bound to
# anything else keeps the profile's binding: the profile runs before this file
# (the -Command aterm passes pwsh). A profile that binds a key to its stock
# function (Ctrl+A SelectAll, Ctrl+Y Redo, Ctrl+W BackwardKillWord) cannot be
# told from stock and is replaced; ATERM_DISABLE_READLINE_KEYS keeps those. The
# first key of a two-key chord counts as bound: -Bound lists only the chord
# ('Ctrl+d,Ctrl+c'), while PSReadLine dispatches its first key to the chord, so
# binding that key alone would leave the profile's chord unreachable (measured
# 2026-10-16 on both hosts with the Ctrl+d,Ctrl+c CaptureScreen chord of
# PSReadLine's own SamplePSReadLineProfile.ps1).
#
# Gated on PSReadLine being loaded, an aterm tab (ATERM_CHILD or ATERM_SESSION_ID,
# as the UTF-8 block above), ATERM_DISABLE_READLINE_KEYS unset (set it in
# $PROFILE to keep stock Windows mode), and EditMode Windows, so a profile that
# chose Emacs or Vi is left alone. Once per shell, by the loaded guard at the top.
#
# One Emacs is aterm's own, not the user's choice: the block an earlier aterm setup
# wrote into a Windows PowerShell 5.1 profile (the four lines of
# $__aterm_legacy_emacs_block below; found 2026-10-18 in a profile.ps1 dated
# 2026-07-04, and written by no code in this repository's history). It was there
# for Ctrl+A/E/K/U, which this layer now gives, and it cost Windows mode: in a 5.1
# tab with it, 18 of 62 key checks failed (Alt+U/L/C typed u/l/c, Ctrl+Z typed ^Z,
# Ctrl+/Alt+Left/Right, Ctrl+Shift+Left/Right, Ctrl+Backspace/Delete, Escape,
# Ctrl+Enter, Ctrl+Shift+Enter), and with Windows mode back all 18 passed
# (measured 2026-10-18 through OpenConsole). So that Emacs goes back to Windows
# mode and gets the layer
# (__aterm_legacy_emacs), but only where nothing else can have chosen it or would
# be lost: setting EditMode rebinds every key to the mode's defaults, dropping any
# binding made since. All of these hold, else Emacs stays exactly as it is:
#   - EditMode is Emacs as the layer starts;
#   - a profile $PROFILE names (aterm starts PowerShell without -NoProfile, so
#     these are the files that ran) holds the four lines exactly, one after
#     another, line endings and trailing blanks aside;
#   - outside those lines no profile names EditMode, KeyHandler or Chord, comments
#     included: a user's own Set-PSReadLineOption -EditMode, a key binding, PSFzf's
#     -PSReadlineChordProvider, or a comment such as "# EditMode Emacs: mine",
#     which is the documented way to keep Emacs with the block in place;
#   - every bound key holds a PSReadLine function, so no module (oh-my-posh,
#     PSFzf) has bound a script block the switch would drop.
# The one binding none of these sees is a PSReadLine function bound to a key by a
# file the profile dot-sources or by a module: the switch resets it with the rest.
# A block with any line changed is the user's. Only the Emacs path reads files:
# $PROFILE's existing files, read whole, once per shell. A start in Windows mode
# took as long as before, and one in an Emacs of the user's 15 ms longer (measured
# 2026-10-18 on 5.1 and pwsh 7, the script's load with no profile 68 ms and 62 ms).
#
# PowerShell 5.1 ships PSReadLine 2.0.0: it has no -Chord on
# Get-PSReadLineKeyHandler, so the -Bound list is read whole, and no
# BackwardKillInput (Ctrl+U falls back to BackwardKillLine) or Upcase/Downcase/
# CapitalizeWord. Skipped, those rows left Windows mode typing a bare u, l or c
# for Alt+U/L/C (measured 2026-10-18), so there each is a script block under the
# same name that does what 2.4.5's function does (__aterm_case_word): from the
# caret to the end of the next word, upper-, lower- or capitalized, the caret
# left after it. Key strings are case-significant (Ctrl+C is Copy, Ctrl+c
# CopyOrCancelLine) and @{} is not, so the bound list is an Ordinal dictionary.
# Alt+Left/Right (CSI 1;3D/1;3C) move by word on 5.1 with its own PSReadLine
# 2.0.0 and on pwsh 7. A 5.1 running PSReadLine 2.4.5 (upgraded by
# Install-Module, or pwsh 7's own, found through a PSModulePath inherited from a
# pwsh parent) drops both keys, as a bare [Console]::ReadKey under 5.1 does
# (measured 2026-10-16 through ConPTY).
#
# Ctrl+E also accepts PSReadLine's inline suggestion, as end-of-line does under
# zsh-autosuggestions in a macOS tab. RightArrow and the layer's Ctrl+F
# (ForwardChar) accept it, while EndOfLine and End leave the caret and the dim
# suggestion where they were (measured 2026-10-16 through ConPTY on pwsh 7.6.7 /
# PSReadLine 2.4.5, PredictionSource History, InlineView). So where PSReadLine
# has AcceptSuggestion, the Ctrl+e row is a script block: with the caret at the
# end of the line it calls AcceptSuggestion, and then it calls EndOfLine. With the
# caret inside the line the first press only moves to the end and a second press
# accepts, which is zsh-autosuggestions' rule (_zsh_autosuggest_accept runs the
# plain widget unless CURSOR is at the end of the buffer) and ForwardChar's.
# AcceptSuggestion itself never reads the caret: a block that called it at every
# press turned a mid-line Ctrl+E, Enter into a run of the suggested history line
# ('#R5 alpha beta' with the caret 5 left of the end submitted '#R5 alpha beta
# gamma delta'), where the caret test submits what was typed. With no inline
# suggestion showing (none matches, or the profile chose PredictionViewStyle
# ListView), AcceptSuggestion does nothing and the key is EndOfLine. PSReadLine
# 2.0.0 has neither AcceptSuggestion nor predictions, so there the row stays
# EndOfLine. The block runs at every press, long after this function has
# returned, so it names the type in full and reads none of this function's
# variables: they are gone by then, and a block that read $__aterm_rl printed
# "You cannot call a method on a null-valued expression" at every press and
# accepted nothing (all measured 2026-10-16 through ConPTY, as above).
$Global:__aterm_legacy_emacs_block = @(
    '# aterm: emacs line-editing so Ctrl+A/Ctrl+E/Ctrl+K/Ctrl+U... move the cursor',
    '# (Windows PowerShell 5.1''s PSReadLine defaults to "Windows" mode, where Ctrl+A =',
    '# SelectAll and Ctrl+E is unbound). Remove this block to revert.',
    'if (Get-Module -ListAvailable PSReadLine) { Set-PSReadLineOption -EditMode Emacs }')
# Whether the Emacs mode PSReadLine is in is aterm's legacy block's alone (see
# above). Called by the layer with its $ErrorActionPreference = 'Stop', so a file
# it cannot read stops the layer and leaves Emacs as it is.
function Global:__aterm_legacy_emacs {
    if ($null -eq $global:PROFILE) { return $false }
    $texts = @()
    foreach ($name in 'AllUsersAllHosts', 'AllUsersCurrentHost', 'CurrentUserAllHosts', 'CurrentUserCurrentHost') {
        $path = $global:PROFILE.PSObject.Properties[$name]
        if ((-not $path) -or (-not $path.Value) -or (-not [System.IO.File]::Exists([string]$path.Value))) { continue }
        $texts += [System.IO.File]::ReadAllText([string]$path.Value)
    }
    # An Emacs profile without the block, the usual one, is answered by one search.
    if (-not ([string]::Join("`n", [string[]]$texts)).Contains($Global:__aterm_legacy_emacs_block[0])) { return $false }
    $block = "`n" + [string]::Join("`n", [string[]]$Global:__aterm_legacy_emacs_block) + "`n"
    $found = $false
    foreach ($text in $texts) {
        # Its lines without line endings or trailing blanks, less every copy of the
        # block; a copy right after another loses its leading line feed to the first.
        $lines = $text.Replace("`r`n", "`n").Replace("`r", "`n").Split("`n")
        for ($i = 0; $i -lt $lines.Length; $i++) { $lines[$i] = $lines[$i].TrimEnd() }
        $text = "`n" + [string]::Join("`n", $lines) + "`n"
        $rest = $text
        do { $length = $rest.Length; $rest = $rest.Replace($block, "`n") } while ($rest.Length -ne $length)
        if ($rest.Length -ne $text.Length) { $found = $true }
        if ($rest -match 'EditMode|KeyHandler|Chord') { return $false }
    }
    if (-not $found) { return $false }
    # A script block's handler lists its BriefDescription, which names no member.
    $rl = [Microsoft.PowerShell.PSConsoleReadLine]
    foreach ($handler in @(Get-PSReadLineKeyHandler -Bound)) {
        if ($rl.GetMember([string]$handler.Function).Length -eq 0) { return $false }
    }
    $true
}
function Global:__aterm_setup_keybindings {
    # Local to this function: any failure stops the layer, caught by the caller,
    # instead of printing at startup.
    $ErrorActionPreference = 'Stop'
    if (-not (Get-Module -Name PSReadLine)) { return }
    $__aterm_mode = [string](Get-PSReadLineOption).EditMode
    if (($__aterm_mode -eq 'Emacs') -and (__aterm_legacy_emacs)) {
        Set-PSReadLineOption -EditMode Windows
        $__aterm_mode = [string](Get-PSReadLineOption).EditMode
    }
    if ($__aterm_mode -ne 'Windows') { return }
    $__aterm_bound = New-Object 'System.Collections.Generic.Dictionary[string,string]' ([System.StringComparer]::Ordinal)
    foreach ($__aterm_h in @(Get-PSReadLineKeyHandler -Bound)) {
        $__aterm_k = [string]$__aterm_h.Key
        $__aterm_bound[$__aterm_k] = [string]$__aterm_h.Function
        # A chord's first key is bound too (see above). The [^+] keeps the comma
        # of a comma key ('Alt+,') from splitting it.
        if ($__aterm_k -match '^(.+?[^+]),.') { $__aterm_bound[$Matches[1]] = 'ChordFirstKey' }
    }
    $__aterm_rl = [Microsoft.PowerShell.PSConsoleReadLine]
    $__aterm_kill_input = if ($__aterm_rl.GetMethod('BackwardKillInput')) { 'BackwardKillInput' } else { 'BackwardKillLine' }
    $__aterm_end_of_line = 'EndOfLine'
    if ($__aterm_rl.GetMethod('AcceptSuggestion')) {
        $__aterm_end_of_line = @{
            BriefDescription = 'AcceptSuggestionAndEndOfLine'
            Description      = 'At the end of the line, accept the inline suggestion if one shows; elsewhere, move the cursor to the end of the line (aterm readline keys)'
            ScriptBlock      = {
                param($key, $arg)
                $line = $null
                $cursor = $null
                [Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState([ref]$line, [ref]$cursor)
                if ($cursor -eq $line.Length) {
                    [Microsoft.PowerShell.PSConsoleReadLine]::AcceptSuggestion($key, $arg)
                }
                [Microsoft.PowerShell.PSConsoleReadLine]::EndOfLine($key, $arg)
            }
        }
    }
    # Alt+U/L/C: PSReadLine's functions, or where this PSReadLine lacks them (2.0.0)
    # script blocks under the same names over __aterm_case_word (see above).
    function Global:__aterm_case_word([string]$how) {
        $line = $null
        $cursor = $null
        [Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState([ref]$line, [ref]$cursor)
        $delimiters = (Get-PSReadLineOption).WordDelimiters
        $end = $cursor
        while ($end -lt $line.Length -and ([char]::IsWhiteSpace($line[$end]) -or $delimiters.IndexOf($line[$end]) -ge 0)) { $end++ }
        while ($end -lt $line.Length -and -not ([char]::IsWhiteSpace($line[$end]) -or $delimiters.IndexOf($line[$end]) -ge 0)) { $end++ }
        if ($end -le $cursor) { return }
        $word = $line.Substring($cursor, $end - $cursor)
        if ($how -eq 'Upcase') { $word = $word.ToUpper() }
        elseif ($how -eq 'Downcase') { $word = $word.ToLower() }
        else {
            $first = 0
            while ($first -lt $word.Length -and -not [char]::IsLetter($word[$first])) { $first++ }
            if ($first -lt $word.Length) {
                $word = $word.Substring(0, $first) + $word.Substring($first, 1).ToUpper() + $word.Substring($first + 1).ToLower()
            }
        }
        [Microsoft.PowerShell.PSConsoleReadLine]::Replace($cursor, $end - $cursor, $word)
        [Microsoft.PowerShell.PSConsoleReadLine]::SetCursorPosition($end)
    }
    $__aterm_upcase = 'UpcaseWord'
    if (-not $__aterm_rl.GetMethod('UpcaseWord')) {
        $__aterm_upcase = @{
            BriefDescription = 'UpcaseWord'
            Description      = 'Upcase from the cursor to the end of the word (aterm readline keys)'
            ScriptBlock      = { param($key, $arg) __aterm_case_word 'Upcase' }
        }
    }
    $__aterm_downcase = 'DowncaseWord'
    if (-not $__aterm_rl.GetMethod('DowncaseWord')) {
        $__aterm_downcase = @{
            BriefDescription = 'DowncaseWord'
            Description      = 'Downcase from the cursor to the end of the word (aterm readline keys)'
            ScriptBlock      = { param($key, $arg) __aterm_case_word 'Downcase' }
        }
    }
    $__aterm_capitalize = 'CapitalizeWord'
    if (-not $__aterm_rl.GetMethod('CapitalizeWord')) {
        $__aterm_capitalize = @{
            BriefDescription = 'CapitalizeWord'
            Description      = 'Capitalize the word from the cursor and move past it (aterm readline keys)'
            ScriptBlock      = { param($key, $arg) __aterm_case_word 'Capitalize' }
        }
    }
    # chord, function (a PSReadLine function's name, or a script block handler's
    # Set-PSReadLineKeyHandler parameters), the stock Windows-mode function it may
    # replace ('' = unbound)
    foreach ($__aterm_row in @(
            @('Ctrl+a', 'BeginningOfLine', 'SelectAll'),
            @('Ctrl+e', $__aterm_end_of_line, ''),
            @('Ctrl+b', 'BackwardChar', ''),
            @('Ctrl+f', 'ForwardChar', ''),
            @('Ctrl+k', 'KillLine', ''),
            @('Ctrl+u', $__aterm_kill_input, ''),
            @('Ctrl+w', 'UnixWordRubout', 'BackwardKillWord'),
            @('Ctrl+p', 'PreviousHistory', ''),
            @('Ctrl+n', 'NextHistory', ''),
            @('Ctrl+d', 'DeleteCharOrExit', ''),
            @('Ctrl+t', 'SwapCharacters', ''),
            @('Ctrl+y', 'Yank', 'Redo'),
            @('Ctrl+_', 'Undo', ''),
            @('Ctrl+j', 'AcceptLine', ''),
            @('Alt+b', 'BackwardWord', ''),
            @('Alt+f', 'ForwardWord', ''),
            @('Alt+Backspace', 'BackwardKillWord', ''),
            @('Alt+LeftArrow', 'BackwardWord', ''),
            @('Alt+RightArrow', 'ForwardWord', ''),
            @('Alt+u', $__aterm_upcase, ''),
            @('Alt+l', $__aterm_downcase, ''),
            @('Alt+c', $__aterm_capitalize, ''))) {
        $__aterm_how = $__aterm_row[1]
        if ($__aterm_how -is [string]) {
            if (-not $__aterm_rl.GetMethod($__aterm_how)) { continue }
            $__aterm_how = @{ Function = $__aterm_how }
        }
        $__aterm_cur = $null
        $null = $__aterm_bound.TryGetValue($__aterm_row[0], [ref]$__aterm_cur)
        if ((-not $__aterm_cur) -or ($__aterm_cur -ceq $__aterm_row[2])) {
            Set-PSReadLineKeyHandler -Chord $__aterm_row[0] @__aterm_how
        }
    }
}
if ((Get-Module -Name PSReadLine) -and ($env:ATERM_CHILD -or $env:ATERM_SESSION_ID) -and
    (-not $env:ATERM_DISABLE_READLINE_KEYS)) {
    try { __aterm_setup_keybindings } catch { }
}
