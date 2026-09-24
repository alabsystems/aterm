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
#
# Compatible with: Windows PowerShell 5.1 and PowerShell 7+ (pwsh) on any OS.
# ASCII only: aterm writes this file without a BOM, and Windows PowerShell 5.1
# decodes BOM-less source as ANSI, so non-ASCII bytes would be misread.

# Skip if already loaded
if ($env:ATERM_SHELL_INTEGRATION_INSTALLED) { return }
$env:ATERM_SHELL_INTEGRATION_INSTALLED = '1'

# Package bin directory
if ($HOME) {
    $__aterm_bin = Join-Path $HOME '.aterm/bin'
    if (Test-Path -LiteralPath $__aterm_bin) {
        $env:PATH = $__aterm_bin + [System.IO.Path]::PathSeparator + $env:PATH
    }

    # Source package shell hooks
    $__aterm_hooks = Join-Path $HOME '.aterm/shell.d'
    if (Test-Path -LiteralPath $__aterm_hooks) {
        foreach ($__aterm_hook in (Get-ChildItem -LiteralPath $__aterm_hooks -Filter '*.ps1' -ErrorAction SilentlyContinue)) {
            . $__aterm_hook.FullName
        }
    }
}

# The agents directory, moved to the front ahead of the reroute block below.
# $env:ATPKG_AGENTS - set by the atpkg shell.d hook dot-sourced just above - names
# <prefix>/agents, which holds ONLY the claude and codex shims aterm keeps current
# (owner decision 2026-09-10). An earlier PATH entry for claude or codex wins
# otherwise (measured 2026-09-10 on macOS zsh: behind /opt/homebrew/bin and
# ~/.local/bin, so codex ran a brew cask that could run no command) - an ORDER
# failure, so move-to-front. aterm dot-sources this file AFTER the user's profile
# (the -Command it passes pwsh), so this runs after a profile's own prepends; a
# profile that dot-sources this file itself is re-fronted only at that point.
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
        if (-not $__aterm_ok) {
            # A native command's failure lands in $LASTEXITCODE; a cmdlet
            # error leaves it untouched (possibly 0), so fall back to 1.
            if (($null -ne $__aterm_exit) -and ($__aterm_exit -ne 0)) { $__aterm_code = $__aterm_exit } else { $__aterm_code = 1 }
        }
        $__aterm_out += "$__aterm_esc]133;D;$__aterm_code$__aterm_suffix$__aterm_bel"
    }
    $Global:__aterm_last_history_id = $__aterm_hid

    # Report current working directory (OSC 7) - filesystem paths only
    # (skip registry/cert/etc. provider locations).
    $__aterm_loc = $ExecutionContext.SessionState.Path.CurrentLocation
    if ($__aterm_loc.Provider.Name -eq 'FileSystem') {
        $__aterm_uri = __aterm_osc7_path $__aterm_loc.ProviderPath
        $__aterm_out += "$__aterm_esc]7;file://$([System.Environment]::MachineName)$__aterm_uri$__aterm_bel"

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
    $__aterm_out
}

# Mark command execution start (OSC 133;C) and report the command text
# (OSC 633;E). PSReadLine's readline shim is the only portable
# submit-time hook; without PSReadLine these two marks are skipped
# (prompt marks and cwd tracking above still work).
if (Get-Module -Name PSReadLine) {
    $Global:__aterm_original_readline = $function:PSConsoleHostReadLine
    function Global:PSConsoleHostReadLine {
        $__aterm_line = [string]($Global:__aterm_original_readline.Invoke())
        # The managed dirs, live - BEFORE this line executes (see LIVE above).
        __aterm_managed_path_live
        if ($__aterm_line -and $__aterm_line.Trim()) {
            $__aterm_esc = [char]27
            $__aterm_bel = [char]7
            $__aterm_suffix = __aterm_id_suffix
            # Report command text for session memory (OSC 633;E)
            $__aterm_out = "$__aterm_esc]633;E;$(__aterm_encode_cmd $__aterm_line)$__aterm_suffix$__aterm_bel"
            # Set tab title to running command (OSC 0).
            # Truncate to first 64 chars and strip control characters.
            if (-not $env:ATERM_DISABLE_PROMPT_TITLES) {
                $__aterm_title = $__aterm_line
                if ($__aterm_title.Length -gt 64) { $__aterm_title = $__aterm_title.Substring(0, 64) }
                $__aterm_title = $__aterm_title -replace '[\x00-\x1f\x7f]', ''
                $__aterm_out += "$__aterm_esc]0;$__aterm_title$__aterm_bel"
            }
            $__aterm_out += "$__aterm_esc]133;C$__aterm_suffix$__aterm_bel"
            [Console]::Write($__aterm_out)
        }
        $__aterm_line
    }
}
