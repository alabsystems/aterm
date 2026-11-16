# Copyright 2026 Andrew Yates
# SPDX-License-Identifier: Apache-2.0
#
# get-aterm.ps1: install aterm for Windows, for this user, from a signed release.
#
#   irm https://alab.systems/get-aterm.ps1 | iex
#   & ([scriptblock]::Create((irm https://alab.systems/get-aterm.ps1))) -Dest D:\tools\aterm
#   powershell -ExecutionPolicy Bypass -File get-aterm.ps1 [-Repo owner/name] [-Dest <dir>]
#       [-NoShortcuts] [-NoPath] [-Register] [-AllowDowngrade]
#
# It installs what a release signed and nothing else, the chain tools/install.sh
# holds a Linux install to:
#   1. the latest release: the tag github.com's latest-release pointer of -Repo
#      (alabsystems/aterm unless named) redirects to, from one HEAD with
#      redirects refused;
#   2. the verifier: PowerShell has no Ed25519, so the signatures are checked by
#      aterm-bootstrap-verify, whose Windows build this script pins by SHA-256
#      below, as install.sh pins the Linux one. It is downloaded from the
#      pinned release, held open so it cannot change, held to the pin, and only
#      then run;
#   3. the proofs: the latest release's roster and appcast with their
#      signatures. The verifier holds the roster to the paper master's key it
#      carries, the appcast to a live machine key of that roster, and the
#      appcast to this release (its tag, its repository), and names the Windows
#      zip the appcast signs: its name, SHA-256 and size, and the release's
#      build number;
#   4. a release with no Windows zip: the release pump publishes Mac-only
#      releases, each the latest. When the latest release's signed appcast
#      signs no zip for this machine (the verifier answers "none"), anonymous
#      requests to the GitHub API list the releases a page at a time (as far
#      back as GitHub lists, its first 1000), and the newest by version older
#      than the latest (no draft, no prerelease, a vMAJOR.MINOR.PATCH tag)
#      whose assets name its Windows zip is proposed.
#      Its appcast is downloaded, and the verifier holds it to the latest
#      release's chain: verified under the latest release's roster, not its
#      own, from a machine that roster does not revoke, the release of its tag,
#      signing this machine's zip, and its build not below the latest release's
#      signed minimum build. The listing only proposes it. A release whose
#      build is older than the one -Dest holds (its BUILD.json's build_number)
#      is refused here unless -AllowDowngrade is given: whoever can point
#      "latest" at an older signed release cannot roll an install back with it;
#   5. the zip: downloaded to exactly the signed size, held open, and held to
#      the signed SHA-256 by the verifier and by this script;
#   6. the install: the zip's own install.ps1 (-Zip) lays it per user
#      (%LOCALAPPDATA%\Programs\aterm, a Start Menu and a Desktop shortcut, the
#      user PATH; -Register adds Apps & Features and Explorer's "Open aterm
#      here"). No administrator is asked for.
# A refusal at any step installs nothing. Everything it downloads goes in a
# folder of its own under %TEMP%, removed when it ends.
#
# Until a release publishes the Windows verifier the pin below is empty, and the
# script refuses before it downloads anything. `targo --unverified ship
# pin-verifier --windows --from-release vX.Y.Z` writes the pin's three lines
# together with crates/aterm-bootstrap-verify/pinned-windows.toml, from the
# verifier digest that release's appcast signs (under the paper master), and the
# tests hold each to the other; never edit them by hand.
#
# With ATERM_GET_LIBRARY_ONLY=1 the script defines its functions and returns,
# for the tests that dot-source it.

param(
    # The repository whose newest release for Windows is installed.
    [string]$Repo = "alabsystems/aterm",
    # Where to install (install.ps1's -Dest): %LOCALAPPDATA%\Programs\aterm unless named.
    [string]$Dest,
    [switch]$NoShortcuts,
    [switch]$NoPath,
    [switch]$Register,
    # Install the release even when -Dest holds a newer build.
    [switch]$AllowDowngrade
)

$AtermReleaseHost = "https://github.com"
# The GitHub API, asked only to list releases when the latest carries no
# Windows zip, with no credential: a page of $AtermListed releases is one
# request of its 60 an hour, read to at most $AtermListingMax bytes, and at most
# $AtermPagesMax pages are read (GitHub lists no release past its first 1000).
# The updater's walk back pages the same way (aterm-update's
# windows_channel::WALK_BACK_PAGE and WALK_BACK_PAGES).
$AtermApiHost = "https://api.github.com"
$AtermListed = 100
$AtermPagesMax = 10
$AtermListingMax = 16777216
# The release verifier's pin: the repository and release whose Windows verifier
# this script runs, and that verifier's SHA-256.
$AtermVerifierRepo = ""
$AtermVerifierTag = ""
$AtermVerifierSha256X86_64 = ""
# The largest verifier it downloads (install.sh's bound for the Linux one).
$AtermVerifierMax = 67108864

# A release tag: v<MAJOR>.<MINOR>.<PATCH>, each a decimal without a leading zero.
function Test-AtermTag([string]$Tag) {
    return ($Tag -cmatch '\Av(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\z')
}

# A repository: owner/name, each word of letters, digits, "_", "." and "-",
# neither "." nor "..".
function Test-AtermRepo([string]$Slug) {
    if ($Slug -cnotmatch '\A[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+\z') { return $false }
    foreach ($word in $Slug.Split('/')) {
        if ($word -eq '.' -or $word -eq '..') { return $false }
    }
    return $true
}

# The download URL of asset $Name of release $Tag of $Slug, for a name on the
# list of what this script fetches; anything else is refused, so nothing a
# release or a redirect names can make it fetch another asset.
function Get-AtermAssetUrl([string]$Slug, [string]$Tag, [string]$Name) {
    if (-not (Test-AtermRepo $Slug)) { throw "refusing a lookup in $Slug, which is not owner/name" }
    if (-not (Test-AtermTag $Tag)) { throw "refusing a lookup in release $Tag, which is not vMAJOR.MINOR.PATCH" }
    $version = [regex]::Escape($Tag.Substring(1))
    $proofs = @("aterm-machines.toml", "aterm-machines.toml.sig", "aterm-appcast.toml", "aterm-appcast.toml.sig")
    $known = ($proofs -ccontains $Name) -or
        ($Name -cmatch "\Aaterm-$version-windows-x86_64\.zip\z") -or
        ($Name -cmatch "\Aaterm-bootstrap-verify-$version-windows-x86_64\.exe\z")
    if (-not $known) { throw "refusing a lookup of $Name, which is no asset this script fetches" }
    return "$AtermReleaseHost/$Slug/releases/download/$Tag/$Name"
}

# An HTTP client: TLS 1.2 or later, the system's proxy, never a credential,
# redirects followed (at most 5) or refused.
function New-AtermHttp([bool]$Follow) {
    Add-Type -AssemblyName System.Net.Http
    # Windows PowerShell on an old .NET may name protocols without TLS 1.2
    # (3072); 0 is the system's choice, which has it, and is kept.
    if ($PSVersionTable.PSEdition -ne "Core") {
        $protocols = [int][Net.ServicePointManager]::SecurityProtocol
        if ($protocols -ne 0 -and -not ($protocols -band 3072)) {
            [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]($protocols -bor 3072)
        }
    }
    $handler = New-Object Net.Http.HttpClientHandler
    $handler.AllowAutoRedirect = $Follow
    if ($Follow) { $handler.MaxAutomaticRedirections = 5 }
    $handler.UseDefaultCredentials = $false
    $client = New-Object Net.Http.HttpClient -ArgumentList $handler
    $client.Timeout = [TimeSpan]::FromMinutes(10)
    $client.DefaultRequestHeaders.UserAgent.ParseAdd("get-aterm.ps1")
    return $client
}

# One HEAD of $Url, redirects refused: its status and Location, as sent.
function Invoke-AtermHead([string]$Url) {
    $client = New-AtermHttp $false
    try {
        $request = New-Object Net.Http.HttpRequestMessage -ArgumentList ([Net.Http.HttpMethod]::Head), $Url
        $response = $client.SendAsync($request).GetAwaiter().GetResult()
        try {
            $location = ""
            if ($null -ne $response.Headers.Location) { $location = $response.Headers.Location.OriginalString }
            return [pscustomobject]@{ Status = [int]$response.StatusCode; Location = $location }
        } finally {
            $response.Dispose()
        }
    } finally {
        $client.Dispose()
    }
}

# $Url's body written to $To, a new file: https to the end, at most $Max bytes.
function Save-AtermUrl([string]$Url, [string]$To, [long]$Max) {
    $client = New-AtermHttp $true
    try {
        $response = $client.GetAsync($Url, [Net.Http.HttpCompletionOption]::ResponseHeadersRead).GetAwaiter().GetResult()
        try {
            if (-not $response.IsSuccessStatusCode) { throw ("{0} answered HTTP {1}" -f $Url, [int]$response.StatusCode) }
            if ($response.RequestMessage.RequestUri.Scheme -cne "https") { throw "$Url was redirected off https" }
            $length = $response.Content.Headers.ContentLength
            if ($null -ne $length -and $length -gt $Max) { throw ("{0} is {1} bytes, more than the {2} this script takes" -f $Url, $length, $Max) }
            $body = $response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
            try {
                $file = [IO.File]::Open($To, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
                try {
                    $buffer = New-Object byte[] 65536
                    $total = [long]0
                    while (($n = $body.Read($buffer, 0, $buffer.Length)) -gt 0) {
                        $total += $n
                        if ($total -gt $Max) { throw ("{0} is more than the {1} bytes this script takes" -f $Url, $Max) }
                        $file.Write($buffer, 0, $n)
                    }
                } finally {
                    $file.Dispose()
                }
            } finally {
                $body.Dispose()
            }
        } finally {
            $response.Dispose()
        }
    } finally {
        $client.Dispose()
    }
}

# The tag github.com's latest-release pointer of $Slug names. The Location must
# be exactly <host>/<slug>/releases/download/<tag>/aterm-appcast.toml with
# <tag> one canonical path segment; anything else (another host, repository or
# asset, a query) is refused, never followed. Each lookup has its own cache key.
function Get-AtermHeadTag([string]$Slug) {
    $url = "{0}/{1}/releases/latest/download/aterm-appcast.toml?aterm_lookup={2}" -f $AtermReleaseHost, $Slug, [Guid]::NewGuid().ToString("N")
    $answer = Invoke-AtermHead $url
    $code = [int]$answer.Status
    if ($code -eq 404) { throw "$Slug has no published release (or the repository is private, renamed or missing: github.com answers the same for all three)" }
    if (@(301, 302, 303, 307, 308) -notcontains $code) { throw ("github.com answered HTTP {0} to {1}, which is not a release pointer (a proxy or a filtered host?)" -f $code, $url) }
    $prefix = "$AtermReleaseHost/$Slug/releases/download/"
    $suffix = "/aterm-appcast.toml"
    $location = [string]$answer.Location
    $tag = ""
    if ($location.StartsWith($prefix, [StringComparison]::Ordinal) -and $location.EndsWith($suffix, [StringComparison]::Ordinal) -and $location.Length -gt $prefix.Length + $suffix.Length) {
        $tag = $location.Substring($prefix.Length, $location.Length - $prefix.Length - $suffix.Length)
    }
    if (-not $tag -or $tag.Contains("/")) { throw "the latest-release pointer of $Slug redirected somewhere this script refuses to follow (not the tag-specific aterm-appcast.toml URL of this repository)" }
    if (-not (Test-AtermTag $tag)) { throw ("the latest release of {0} is {1}, which is not an app release (vMAJOR.MINOR.PATCH)" -f $Slug, ($tag -replace '[^A-Za-z0-9._-]', '?')) }
    return $tag
}

# The three numbers of release tag $Tag, or $null when it is not one
# (Test-AtermTag) or a number is beyond the updater's (2^64 - 1).
function Get-AtermTagKey([string]$Tag) {
    if (-not (Test-AtermTag $Tag)) { return $null }
    $key = New-Object decimal[] 3
    $parts = $Tag.Substring(1).Split('.')
    for ($i = 0; $i -lt 3; $i++) {
        if ($parts[$i].Length -gt 20) { return $null }
        $key[$i] = [decimal]::Parse($parts[$i], [Globalization.CultureInfo]::InvariantCulture)
        if ($key[$i] -gt [decimal]::Parse("18446744073709551615", [Globalization.CultureInfo]::InvariantCulture)) { return $null }
    }
    return ,$key
}

# -1, 0 or 1 as tag key $A is older than, the same as or newer than $B.
function Compare-AtermTagKey([decimal[]]$A, [decimal[]]$B) {
    for ($i = 0; $i -lt 3; $i++) {
        if ($A[$i] -lt $B[$i]) { return -1 }
        if ($A[$i] -gt $B[$i]) { return 1 }
    }
    return 0
}

# The release to propose when $Latest, the latest release of $Slug, signs no
# Windows zip for $Arch. Requests to the GitHub API, with no credential, list
# the releases newest first by creation into $Work, $AtermListed a page, until
# the page after the first that holds a release proposed, the listing's last
# page, or $AtermPagesMax pages, whichever comes first: a release created before
# that one, so listed after it, may be of a newer version. Drafts, prereleases,
# tags that are not vMAJOR.MINOR.PATCH and every release not older than $Latest
# are passed over, the rest are weighed newest version first (never by where the
# listing puts them), and the newest whose assets name its Windows zip for $Arch
# is the answer. The listing only proposes: the verifier holds the release to
# $Latest's roster and floors. Throws, naming what it looked at, when none is.
function Find-AtermWindowsRelease([string]$Slug, [string]$Latest, [string]$Arch, [string]$Work) {
    if (-not (Test-AtermRepo $Slug)) { throw "refusing a lookup in $Slug, which is not owner/name" }
    $latestKey = Get-AtermTagKey $Latest
    if ($null -eq $latestKey) { throw "refusing to walk back from release $Latest, which is not vMAJOR.MINOR.PATCH" }
    $lacks = "the latest release {0} carries no Windows zip for {1}" -f $Latest, $Arch
    $earlier = New-Object Collections.Generic.List[object]
    $seen = New-Object 'Collections.Generic.HashSet[string]' -ArgumentList ([StringComparer]::Ordinal)
    $listedCount = 0
    $pages = 0
    $foundOn = 0
    for ($page = 1; $page -le $AtermPagesMax; $page++) {
        $url = "{0}/repos/{1}/releases?per_page={2}&page={3}" -f $AtermApiHost, $Slug, $AtermListed, $page
        $at = Join-Path $Work ("releases-{0}.json" -f $page)
        try {
            Save-AtermUrl $url $at $AtermListingMax
            $text = [IO.File]::ReadAllText($at)
            if (-not $text.TrimStart().StartsWith("[", [StringComparison]::Ordinal)) { throw "the answer is not a list" }
            $parsed = ConvertFrom-Json -InputObject $text
        } catch {
            throw ("{0}, and the releases before it could not be listed (page {1}): {2}" -f $lacks, $page, $_.Exception.Message)
        }
        $listed = @(foreach ($release in $parsed) { $release })
        if ($listed.Count -gt $AtermListed) { throw ("{0}, and the API listed {1} releases for a page of {2}" -f $lacks, $listed.Count, $AtermListed) }
        $pages = $page
        $listedCount += $listed.Count
        foreach ($release in $listed) {
            if ($null -eq $release -or $release.tag_name -isnot [string] -or $release.draft -isnot [bool] -or $release.prerelease -isnot [bool] -or $release.assets -isnot [array]) {
                throw "$lacks, and the API's listing of its releases is not the JSON it documents"
            }
            if ($release.draft -or $release.prerelease) { continue }
            $key = Get-AtermTagKey $release.tag_name
            if ($null -eq $key -or (Compare-AtermTagKey $key $latestKey) -ge 0) { continue }
            # A release created while the pages were read shifts the listing:
            # the one it pushed down a page is listed twice.
            if (-not $seen.Add($release.tag_name)) { continue }
            $zip = "aterm-{0}-windows-{1}.zip" -f $release.tag_name.Substring(1), $Arch
            $carries = $false
            foreach ($asset in $release.assets) {
                if ($null -ne $asset -and $asset.name -is [string] -and $asset.name -ceq $zip) { $carries = $true }
            }
            if ($carries -and -not $foundOn) { $foundOn = $page }
            $earlier.Add([pscustomobject]@{ Tag = $release.tag_name; A = $key[0]; B = $key[1]; C = $key[2]; Carries = $carries })
        }
        if ($listed.Count -lt $AtermListed -or ($foundOn -and $page -gt $foundOn)) { break }
    }
    $sorted = @($earlier | Sort-Object -Property A, B, C -Descending)
    foreach ($release in $sorted) {
        if ($release.Carries) { return $release.Tag }
    }
    $named = 10
    $tags = @($sorted | ForEach-Object { $_.Tag })
    $looked = if ($tags.Count -eq 0) {
        "none of them is a published vMAJOR.MINOR.PATCH release older than it"
    } elseif ($tags.Count -le $named) {
        "looked at " + ($tags -join ", ")
    } else {
        "looked at {0} and {1} older" -f (($tags[0..($named - 1)]) -join ", "), ($tags.Count - $named)
    }
    $pageWord = if ($pages -eq 1) { "1 page" } else { "$pages pages" }
    throw ("{0}, and no release before it does among the {1} the repository lists on {2} of {3} ({4})" -f $lacks, $listedCount, $pageWord, $AtermListed, $looked)
}

# The architecture aterm installs for here, as a release names it.
function Get-AtermArch {
    $arch = $env:PROCESSOR_ARCHITEW6432
    if (-not $arch) { $arch = $env:PROCESSOR_ARCHITECTURE }
    if ($arch -eq "AMD64") { return "x86_64" }
    throw "aterm for Windows is built for x86_64 (AMD64); this Windows is $arch"
}

# The pin, whole and well formed: an empty one refuses before anything is
# downloaded, since nothing else here can check a signature.
function Assert-AtermPin {
    if (-not $AtermVerifierRepo -and -not $AtermVerifierTag -and -not $AtermVerifierSha256X86_64) {
        throw "this get-aterm.ps1 pins no release verifier yet: no release has published aterm-bootstrap-verify for Windows, and nothing else here can check a release's signatures. Nothing was downloaded."
    }
    if (-not (Test-AtermRepo $AtermVerifierRepo) -or -not (Test-AtermTag $AtermVerifierTag) -or $AtermVerifierSha256X86_64 -cnotmatch '\A[0-9a-f]{64}\z') {
        throw "this get-aterm.ps1's verifier pin is malformed. Nothing was downloaded."
    }
}

# -Dest as the full path install.ps1 lays into (%LOCALAPPDATA%\Programs\aterm
# unless named): resolved here, once, from this session's location, so the
# install.ps1 this script starts in another process lays where the downgrade
# check looked.
# It ends in no backslash, as install.ps1's own resolution does: one before the
# closing quote of a path with a space would escape that quote on the command
# line Windows PowerShell builds for the child.
function Resolve-AtermDest([string]$Path) {
    if ($Path) {
        $provider = $null
        $drive = $null
        $full = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Path, [ref]$provider, [ref]$drive)
        if ($provider.Name -ne "FileSystem") { throw "-Dest must be a file system folder: $Path" }
    } elseif ($env:LOCALAPPDATA) {
        $full = Join-Path $env:LOCALAPPDATA "Programs\aterm"
    } else {
        throw "%LOCALAPPDATA% is not set, so there is no per-user folder to install to; name one with -Dest"
    }
    $full = [IO.Path]::GetFullPath($full)
    if ($full.TrimEnd('\') -eq [IO.Path]::GetPathRoot($full).TrimEnd('\')) { throw "-Dest must be a folder, not a drive root: $full" }
    return $full.TrimEnd('\')
}

# The build number of the aterm installed in $Dir: its BUILD.json's
# build_number, or 0 when nothing there names one (no install, a BUILD.json
# without the field, or one that does not read as a build record).
function Get-AtermInstalledBuild([string]$Dir) {
    $json = Join-Path $Dir "BUILD.json"
    try {
        if (-not (Test-Path -LiteralPath $json -PathType Leaf)) { return [decimal]0 }
        if ((Get-Item -LiteralPath $json).Length -gt 65536) { return [decimal]0 }
        $number = "$(([IO.File]::ReadAllText($json) | ConvertFrom-Json).build_number)"
        if ($number -cmatch '\A[1-9][0-9]{0,19}\z') { return [decimal]$number }
    } catch { }
    return [decimal]0
}

# $Path opened to read, and held so: nobody may write, rename or delete it
# while the handle is open, so what was hashed is what runs or installs.
function Open-AtermHeld([string]$Path) {
    return [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
}

# The SHA-256 of all of $Stream, in lowercase hex.
function Get-AtermSha256([IO.Stream]$Stream) {
    [void]$Stream.Seek(0, [IO.SeekOrigin]::Begin)
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        return (($sha.ComputeHash($Stream) | ForEach-Object { $_.ToString("x2") }) -join "")
    } finally {
        $sha.Dispose()
    }
}

# $Word as one argument of a Windows command line, quoted as the C runtime
# splits one.
function ConvertTo-AtermArgument([string]$Word) {
    if ($Word.Length -gt 0 -and $Word -notmatch '[\s"]') { return $Word }
    $text = New-Object Text.StringBuilder
    [void]$text.Append('"')
    $slashes = 0
    foreach ($c in $Word.ToCharArray()) {
        if ($c -eq [char]'\') { $slashes++; continue }
        if ($c -eq [char]'"') { [void]$text.Append([char]'\', 2 * $slashes + 1) } elseif ($slashes) { [void]$text.Append([char]'\', $slashes) }
        $slashes = 0
        [void]$text.Append($c)
    }
    if ($slashes) { [void]$text.Append([char]'\', 2 * $slashes) }
    [void]$text.Append('"')
    return $text.ToString()
}

# The verifier's one line for $Words: exit 0 and exactly one line out, its five
# tab-separated fields, or "none" (the latest release carries no Windows zip
# for this machine); anything else refuses, in the verifier's words.
function Invoke-AtermVerifier([string]$Exe, [string[]]$Words) {
    $info = New-Object Diagnostics.ProcessStartInfo
    $info.FileName = $Exe
    $info.Arguments = (@($Words | ForEach-Object { ConvertTo-AtermArgument $_ }) -join " ")
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $info.StandardOutputEncoding = New-Object Text.UTF8Encoding -ArgumentList $false
    $info.StandardErrorEncoding = New-Object Text.UTF8Encoding -ArgumentList $false
    $process = [Diagnostics.Process]::Start($info)
    try {
        $out = $process.StandardOutput.ReadToEndAsync()
        $err = $process.StandardError.ReadToEndAsync()
        # A verifier that has not answered in five minutes is stopped, by the
        # handle this script started it with.
        if (-not $process.WaitForExit(300000)) {
            $process.Kill()
            throw "the release verifier gave no answer in five minutes"
        }
        $process.WaitForExit()
        $said = $out.GetAwaiter().GetResult()
        $why = $err.GetAwaiter().GetResult().Trim()
        if ($process.ExitCode -ne 0) {
            if (-not $why) { $why = "the release verifier refused (exit {0})" -f $process.ExitCode }
            throw $why
        }
    } finally {
        $process.Dispose()
    }
    $lines = @($said -split "`r?`n" | Where-Object { $_ -ne "" })
    if ($lines.Count -ne 1 -or ($lines[0] -cne "none" -and @($lines[0] -split "`t").Count -ne 5)) { throw "the release verifier's answer is not one line of five fields" }
    return $lines[0]
}

# Release $Tag of $Slug's proofs named $Names (each with its bounds, as
# @(name, least, most)), downloaded into $Dir.
function Save-AtermProofs([string]$Slug, [string]$Tag, [string]$Dir, [object[]]$Names) {
    foreach ($proof in $Names) {
        $at = Join-Path $Dir $proof[0]
        Save-AtermUrl (Get-AtermAssetUrl $Slug $Tag $proof[0]) $at $proof[2]
        $bytes = (Get-Item -LiteralPath $at).Length
        if ($bytes -lt $proof[1]) { throw ("{0} of {1} is {2} bytes, outside {3}..{4}" -f $proof[0], $Tag, $bytes, $proof[1], $proof[2]) }
    }
}

# The zip's own aterm\install.ps1, written to $To from the held zip.
function Expand-AtermInstaller([IO.Stream]$Stream, [string]$To) {
    Add-Type -AssemblyName System.IO.Compression
    [void]$Stream.Seek(0, [IO.SeekOrigin]::Begin)
    $archive = New-Object IO.Compression.ZipArchive -ArgumentList $Stream, ([IO.Compression.ZipArchiveMode]::Read), $true
    try {
        $found = @($archive.Entries | Where-Object { ($_.FullName -replace '\\', '/') -ceq "aterm/install.ps1" })
        if ($found.Count -ne 1) { throw "the signed zip holds no aterm\install.ps1 of its own: it is not a release's build folder" }
        if ($found[0].Length -gt 4MB) { throw "the signed zip's aterm\install.ps1 is larger than an installer" }
        $in = $found[0].Open()
        try {
            $out = [IO.File]::Open($To, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
            try { $in.CopyTo($out) } finally { $out.Dispose() }
        } finally {
            $in.Dispose()
        }
    } finally {
        $archive.Dispose()
    }
}

# The install. Throws, having installed nothing, on any refusal.
function Invoke-AtermGet {
    param(
        [string]$Repo = "alabsystems/aterm",
        [string]$Dest,
        [switch]$NoShortcuts,
        [switch]$NoPath,
        [switch]$Register,
        [switch]$AllowDowngrade
    )
    $ErrorActionPreference = "Stop"
    $work = $null
    $verifierFile = $null
    $zipFile = $null
    try {
        if ($PSVersionTable.PSEdition -eq "Core" -and -not $IsWindows) {
            throw "get-aterm.ps1 installs aterm for Windows; on macOS and Linux run: curl -fsSL https://alab.systems/install.sh | bash"
        }
        $arch = Get-AtermArch
        Assert-AtermPin
        if (-not (Test-AtermRepo $Repo)) { throw "-Repo $Repo is not owner/name" }
        $destDir = Resolve-AtermDest $Dest

        $work = Join-Path ([IO.Path]::GetTempPath()) ("aterm-get-{0}-{1}" -f $PID, [Guid]::NewGuid().ToString("N").Substring(0, 12))
        New-Item -ItemType Directory $work | Out-Null
        $proofs = Join-Path $work "proofs"
        New-Item -ItemType Directory $proofs | Out-Null

        $latest = Get-AtermHeadTag $Repo
        $tag = $latest

        # The verifier, held to the pin before it runs.
        $verifierName = "aterm-bootstrap-verify-{0}-windows-{1}.exe" -f $AtermVerifierTag.Substring(1), $arch
        $verifier = Join-Path $work $verifierName
        Save-AtermUrl (Get-AtermAssetUrl $AtermVerifierRepo $AtermVerifierTag $verifierName) $verifier $AtermVerifierMax
        $verifierFile = Open-AtermHeld $verifier
        if ((Get-AtermSha256 $verifierFile) -cne $AtermVerifierSha256X86_64) {
            throw ("{0} of {1} {2} is not the verifier this script pins (SHA-256 mismatch); nothing was run" -f $verifierName, $AtermVerifierRepo, $AtermVerifierTag)
        }

        # The latest release's proofs, each within its bounds (a signature is
        # exactly 64 bytes).
        $appcasts = @(@("aterm-appcast.toml", 1, 5000000), @("aterm-appcast.toml.sig", 64, 64))
        Save-AtermProofs $Repo $latest $proofs (@(@("aterm-machines.toml", 1, 65536), @("aterm-machines.toml.sig", 64, 64)) + $appcasts)
        $asked = @($proofs, $latest, $tag, $Repo, $arch)
        $line = Invoke-AtermVerifier $verifier $asked
        if ($line -ceq "none") {
            # The latest release, verified, signs no zip for this machine (a
            # Mac-only release): the newest release before it that lists one,
            # held by the verifier to the latest release's roster and floors.
            $tag = Find-AtermWindowsRelease $Repo $latest $arch $work
            Write-Host ("get-aterm.ps1: {0} carries no Windows build; installing {1}, the newest release that does" -f $latest, $tag)
            foreach ($name in @("aterm-appcast.toml", "aterm-appcast.toml.sig")) {
                [IO.File]::Move((Join-Path $proofs $name), (Join-Path $proofs ($name -creplace '\Aaterm-appcast', 'aterm-policy-appcast')))
            }
            Save-AtermProofs $Repo $tag $proofs $appcasts
            $asked = @($proofs, $latest, $tag, $Repo, $arch)
            $line = Invoke-AtermVerifier $verifier $asked
            if ($line -ceq "none") { throw "the release verifier's answer is not one line of five fields" }
        }
        $version = $tag.Substring(1)
        Write-Host ("get-aterm.ps1: installing {0} {1} (windows-{2})" -f $Repo, $tag, $arch)
        $fields = $line -split "`t"
        $name = $fields[0]
        $digest = $fields[1]
        if ($name -cne ("aterm-{0}-windows-{1}.zip" -f $version, $arch)) { throw "the signed release names $name, not its Windows zip" }
        if ($digest -cnotmatch '\A[0-9a-f]{64}\z' -or $fields[2] -cnotmatch '\A[1-9][0-9]{1,9}\z' -or $fields[3] -cnotmatch '\A[0-9]{1,20}\z') { throw "the release verifier's answer is malformed" }
        $size = [long]$fields[2]
        if ($size -lt 64 -or $size -gt 536870912) { throw "the signed size of $name, $size bytes, is outside 64..536870912" }
        $build = [decimal]$fields[3]
        $installed = Get-AtermInstalledBuild $destDir
        if ($installed -gt $build -and -not $AllowDowngrade) {
            throw ("{0} holds aterm build {1}, newer than {2}'s signed build {3}: refusing to downgrade it (-AllowDowngrade installs {2} anyway). Nothing was installed." -f $destDir, $installed, $tag, $build)
        }

        Write-Host ("get-aterm.ps1: downloading authenticated {0} ({1} MB)" -f $name, [math]::Round($size / 1MB))
        $zip = Join-Path $work $name
        Save-AtermUrl (Get-AtermAssetUrl $Repo $tag $name) $zip $size
        $zipFile = Open-AtermHeld $zip
        if ($zipFile.Length -ne $size) { throw ("the download of {0} is {1} bytes, not the signed {2}" -f $name, $zipFile.Length, $size) }
        if ((Get-AtermSha256 $zipFile) -cne $digest) { throw "the download of $name is not the signed zip (SHA-256 mismatch)" }
        $again = Invoke-AtermVerifier $verifier ($asked + @($zip))
        if ($again -cne $line) { throw "the release verifier's answer for the download differs from its answer for the release" }

        # The zip's own installer lays it, from the zip still held.
        $installer = Join-Path $work "install.ps1"
        Expand-AtermInstaller $zipFile $installer
        $shell = if ($PSVersionTable.PSEdition -eq "Core") { Join-Path $PSHOME "pwsh.exe" } else { Join-Path $PSHOME "powershell.exe" }
        $words = @("-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", $installer, "-Zip", $zip)
        $words += @("-Dest", $destDir)
        if ($NoShortcuts) { $words += "-NoShortcuts" }
        if ($NoPath) { $words += "-NoPath" }
        if ($Register) { $words += "-Register" }
        & $shell @words | Out-Host
        if ($LASTEXITCODE -ne 0) { throw ("the release's install.ps1 refused (exit {0}); nothing of {1} was installed" -f $LASTEXITCODE, $tag) }
        Write-Host ("get-aterm.ps1: installed aterm {0}" -f $version) -ForegroundColor Green
    } catch {
        $why = $_.Exception.Message
        if (-not $why.StartsWith("get-aterm.ps1:", [StringComparison]::Ordinal)) { $why = "get-aterm.ps1: $why" }
        throw $why
    } finally {
        if ($zipFile) { $zipFile.Dispose() }
        if ($verifierFile) { $verifierFile.Dispose() }
        if ($work) { Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue }
    }
}

if ($env:ATERM_GET_LIBRARY_ONLY -eq "1") { return }
Invoke-AtermGet @PSBoundParameters
