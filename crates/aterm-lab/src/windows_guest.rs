// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Unattended Windows 11 guest provisioning and SSH/[`desktop_runner`] command wrappers for
//! [`crate::windows`].

use std::fmt::Write as _;

/// STOCK EXCEPTION: no Windows build of the Trust toolchain is published.
/// STOCK EXCEPTION: no Windows build of the Trust toolchain is published (the
pub const RUST: &str = "1.97.1";

/// Rustup-init for aarch64 Windows.
pub const RUSTUP: (&str, &str) = (
    "1.29.1",
    "01aa49cf9574a8bd0ae52005d7de2590e8f27181ded6748236e702c92aef826d",
);

/// Git for Windows' native arm64 installer.
pub const GIT: (&str, &str, &str) = (
    "v2.56.0.windows.2",
    "Git-2.56.0.2-arm64.exe",
    "b9ced78ce371577dc966f50ae571909d6731e36d31065d35c8efa21544c58bf0",
);

/// PowerShell 7's arm64 installer.
pub const PWSH: (&str, &str, &str) = (
    "7.6.7",
    "PowerShell-7.6.7-win-arm64.msi",
    "ddb2f3613a0ad27817b90601a0827f7347e81dfa5bce18d4be8009b8cb0e0a31",
);

/// WSL's arm64 package: its version, file and the digest GitHub publishes for the asset.
pub const WSL: (&str, &str, &str) = (
    "3.0.3",
    "wsl.3.0.3.0.arm64.msi",
    "2937c7ce840bb35ec6e97f052189bdfff27f8c3463bfdd231df9a958212787a3",
);

/// The runner's WSL distro.
pub const WSL_DISTRO: (&str, &str, &str) = (
    "aterm-lane",
    "ubuntu-24.04.5-wsl-arm64.wsl",
    "74478b5024d9047397ba79d17c27f73caf85ada4988f88cffc2892d65e6a806b",
);

/// Where the runner's WSL distro lives, under the runner's own `%LOCALAPPDATA%`.
pub const WSL_DIR: &str = r"aterm-lane\wsl";

/// Virtio-win's display-only driver for the VM's virtio-gpu.
pub const VIOGPUDO_DIR: &str = "viogpudo/w11/ARM64";
pub const VIOGPUDO_FILES: [(&str, &str); 3] = [
    (
        "viogpudo.inf",
        "849cfefa9364b4a3e80dd44ce898e3b4766af5c4a475ea0a95ddb4c9f1cb2e4b",
    ),
    (
        "viogpudo.sys",
        "78bf0afc614a19c365a050e96e0d0274def2ac860e842cc0964795bf431a14b7",
    ),
    (
        "viogpudo.cat",
        "15a9b4664470602ab20efe1d665dc98d5fe0337e32038bc51369ecc0ca4826d2",
    ),
];

/// The service [`VIOGPUDO_FILES`]' inf installs for the device.
pub const VIOGPUDO_SERVICE: &str = "VioGpuDod";

/// Where the host puts [`VIOGPUDO_FILES`] in the guest before provisioning.
pub const VIOGPUDO_GUEST: &str = r"C:\lane\dl\viogpudo";

/// The Windows capability that makes the inbox Japanese IME usable.
pub const JA_CAPABILITY: &str = "Language.Basic~~~ja-JP~0.0.1.0";

/// How long that capability's install may take.
pub const CAPABILITY_WITHIN_SECS: u64 = 3600;

/// The runner's input languages, in order.
pub const LANGUAGES: [&str; 2] = ["en-US", "ja-JP"];

/// Visual Studio 2022's Build Tools bootstrapper.
pub const VS_BOOTSTRAPPER: &str = "https://aka.ms/vs/17/release/vs_BuildTools.exe";

/// What the Build Tools install.
pub const VS_COMPONENTS: [&str; 5] = [
    "Microsoft.VisualStudio.Workload.VCTools",
    "Microsoft.VisualStudio.Component.VC.Tools.ARM64",
    "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
    "Microsoft.VisualStudio.Component.Windows11SDK.26100",
    "Microsoft.VisualStudio.Component.VC.Llvm.Clang",
];

/// Where the Build Tools go.
pub const VS_PATH: &str = r"C:\BuildTools";

/// The Build Tools' arm64-hosted Clang, first on an exec's `PATH` so ring's `clang` is found.
pub const LLVM_BIN: &str = r"C:\BuildTools\VC\Tools\Llvm\ARM64\bin";

/// The guest's administrator, which installs, provisions and shuts down Windows.
pub const USER: &str = "aterm";

/// The guest's standard user, which every `exec` and `smoke` runs as.
pub const RUNNER: &str = "lane";

/// The runner's authorized keys, which `sshd_config` names for it.
pub const RUNNER_KEYS: &str = r"C:\ProgramData\ssh\lane_authorized_keys";

/// Windows Terminal's package name.
pub const TERMINAL_PACKAGE: &str = "Microsoft.WindowsTerminal";

/// Rust's homes (`RUSTUP_HOME`, `CARGO_HOME`), in the lane, which the runner may change.
pub const RUSTUP_HOME: &str = r"C:\lane\rustup";
pub const CARGO_HOME: &str = r"C:\lane\cargo";

/// The guest's lane directory, which Defender skips.
pub const LANE: &str = r"C:\lane";

/// The checkout as the guest holds it.
pub const SRC: &str = r"C:\lane\src";

/// The build output: `CARGO_TARGET_DIR` of every `exec`.
pub const TARGET: &str = r"C:\lane\target";

/// Downloads the provisioning plan keeps (installers, rustup-init).
pub const DOWNLOADS: &str = r"C:\lane\dl";

/// Where a command run in the runner's desktop ([`desktop_runner`]) has its batch file.
pub const TASKS: &str = r"C:\lane\task";

/// Where the guest records the plan it was provisioned with.
pub const STAMP: &str = r"C:\lane\provisioned";

/// What the first-logon script writes when it finished.
pub const FIRST_LOGON_DONE: &str = r"C:\lane\first-logon.done";
pub const FIRST_LOGON_LOG: &str = r"C:\lane\first-logon.log";

/// The answer media's volume name and the script on it the first logon runs.
pub const MEDIA_LABEL: &str = "ATERMLANE";
pub const FIRST_LOGON_FILE: &str = "aterm-lane.ps1";

/// The edition the answer file installs from the multi-edition ISO.
pub const EDITION: &str = "Windows 11 Pro";

/// Microsoft's generic installation key for Windows 11 Pro.
pub const GENERIC_PRO_KEY: &str = "VK7JG-NPHTM-C97JM-9MPGT-3V66T";

/// The guest's name on its network.
pub const COMPUTER_NAME: &str = "ATERM-LANE";

/// A CPU architecture the lane builds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arch {
    /// The guest's own: built and run natively.
    Aarch64,
    /// Cross-built with MSVC's x64 tools and run under Prism.
    X86_64,
}

impl Arch {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Arch::Aarch64 => "aarch64",
            Arch::X86_64 => "x86_64",
        }
    }

    /// The Rust target triple.
    #[must_use]
    pub const fn triple(self) -> &'static str {
        match self {
            Arch::Aarch64 => "aarch64-pc-windows-msvc",
            Arch::X86_64 => "x86_64-pc-windows-msvc",
        }
    }

    /// How its binaries run in the guest.
    #[must_use]
    pub const fn runs(self) -> &'static str {
        match self {
            Arch::Aarch64 => "native",
            Arch::X86_64 => "Prism",
        }
    }

    /// The architecture a command-line word names.
    #[must_use]
    pub fn parse(word: &str) -> Option<Arch> {
        match word {
            "aarch64" | "arm64" => Some(Arch::Aarch64),
            "x86_64" | "amd64" | "x64" => Some(Arch::X86_64),
            _ => None,
        }
    }
}

/// `text` with XML's five special characters escaped.
#[must_use]
pub fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// Registry values Windows Setup reads (`HKLM\SYSTEM\Setup\LabConfig`) to skip hardware checks.
pub const LAB_CONFIG: [&str; 4] = [
    "BypassTPMCheck",
    "BypassSecureBootCheck",
    "BypassCPUCheck",
    "BypassRAMCheck",
];

/// The FirstLogonCommand.
#[must_use]
pub fn first_logon_command() -> String {
    format!(
        "cmd /c for %d in (D E F G H I J K L M N O P) do @if exist %d:\\{FIRST_LOGON_FILE} \
         powershell -NoProfile -ExecutionPolicy Bypass -File %d:\\{FIRST_LOGON_FILE}"
    )
}

fn component(name: &str, body: &str) -> String {
    format!(
        "    <component name=\"{name}\" processorArchitecture=\"arm64\" \
         publicKeyToken=\"31bf3856ad364e35\" language=\"neutral\" versionScope=\"nonSxS\" \
         xmlns:wcm=\"http://schemas.microsoft.com/WMIConfig/2002/State\">\n{body}    </component>\n"
    )
}

/// `autounattend.xml`: Windows 11 Pro installed on the whole first disk (efi, msr, NTFS).
#[must_use]
pub fn autounattend(password: &str) -> String {
    let pw = xml_escape(password);
    let mut lab = String::new();
    for (i, key) in LAB_CONFIG.iter().enumerate() {
        let _ = writeln!(
            lab,
            "          <RunSynchronousCommand wcm:action=\"add\"><Order>{}</Order>\
             <Path>reg add HKLM\\SYSTEM\\Setup\\LabConfig /v {key} /t REG_DWORD /d 1 /f</Path>\
             </RunSynchronousCommand>",
            i + 1
        );
    }
    let locale = "      <InputLocale>en-US</InputLocale>\n      <SystemLocale>en-US</SystemLocale>\n      \
                  <UILanguage>en-US</UILanguage>\n      <UserLocale>en-US</UserLocale>\n";
    let mut pe = String::new();
    pe.push_str(&component(
        "Microsoft-Windows-International-Core-WinPE",
        &format!(
            "      <SetupUILanguage><UILanguage>en-US</UILanguage></SetupUILanguage>\n{locale}"
        ),
    ));
    pe.push_str(&component(
        "Microsoft-Windows-Setup",
        &format!(
            "      <RunSynchronous>\n{lab}      </RunSynchronous>\n\
      <DiskConfiguration>\n\
        <Disk wcm:action=\"add\">\n\
          <DiskID>0</DiskID>\n\
          <WillWipeDisk>true</WillWipeDisk>\n\
          <CreatePartitions>\n\
            <CreatePartition wcm:action=\"add\"><Order>1</Order><Type>EFI</Type><Size>300</Size></CreatePartition>\n\
            <CreatePartition wcm:action=\"add\"><Order>2</Order><Type>MSR</Type><Size>16</Size></CreatePartition>\n\
            <CreatePartition wcm:action=\"add\"><Order>3</Order><Type>Primary</Type><Extend>true</Extend></CreatePartition>\n\
          </CreatePartitions>\n\
          <ModifyPartitions>\n\
            <ModifyPartition wcm:action=\"add\"><Order>1</Order><PartitionID>1</PartitionID><Format>FAT32</Format><Label>System</Label></ModifyPartition>\n\
            <ModifyPartition wcm:action=\"add\"><Order>2</Order><PartitionID>2</PartitionID></ModifyPartition>\n\
            <ModifyPartition wcm:action=\"add\"><Order>3</Order><PartitionID>3</PartitionID><Format>NTFS</Format><Label>Windows</Label><Letter>C</Letter></ModifyPartition>\n\
          </ModifyPartitions>\n\
        </Disk>\n\
      </DiskConfiguration>\n\
      <ImageInstall>\n\
        <OSImage>\n\
          <InstallFrom><MetaData wcm:action=\"add\"><Key>/IMAGE/NAME</Key><Value>{EDITION}</Value></MetaData></InstallFrom>\n\
          <InstallTo><DiskID>0</DiskID><PartitionID>3</PartitionID></InstallTo>\n\
        </OSImage>\n\
      </ImageInstall>\n\
      <UserData>\n\
        <AcceptEula>true</AcceptEula>\n\
        <ProductKey><Key>{GENERIC_PRO_KEY}</Key><WillShowUI>Never</WillShowUI></ProductKey>\n\
      </UserData>\n"
        ),
    ));
    let mut specialize = String::new();
    specialize.push_str(&component(
        "Microsoft-Windows-Shell-Setup",
        &format!(
            "      <ComputerName>{COMPUTER_NAME}</ComputerName>\n      <TimeZone>UTC</TimeZone>\n"
        ),
    ));
    specialize.push_str(&component(
        "Microsoft-Windows-Deployment",
        "      <RunSynchronous>\n\
          <RunSynchronousCommand wcm:action=\"add\"><Order>1</Order>\
<Path>reg add HKLM\\SYSTEM\\CurrentControlSet\\Control\\BitLocker /v PreventDeviceEncryption /t REG_DWORD /d 1 /f</Path>\
</RunSynchronousCommand>\n\
      </RunSynchronous>\n",
    ));
    let mut oobe = String::new();
    oobe.push_str(&component("Microsoft-Windows-International-Core", locale));
    oobe.push_str(&component(
        "Microsoft-Windows-Shell-Setup",
        &format!(
            "      <OOBE>\n\
        <HideEULAPage>true</HideEULAPage>\n\
        <HideOEMRegistrationScreen>true</HideOEMRegistrationScreen>\n\
        <HideOnlineAccountScreens>true</HideOnlineAccountScreens>\n\
        <HideWirelessSetupInOOBE>true</HideWirelessSetupInOOBE>\n\
        <HideLocalAccountScreen>true</HideLocalAccountScreen>\n\
        <ProtectYourPC>3</ProtectYourPC>\n\
      </OOBE>\n\
      <UserAccounts>\n\
        <LocalAccounts>\n\
          <LocalAccount wcm:action=\"add\">\n\
            <Name>{USER}</Name>\n\
            <Group>Administrators</Group>\n\
            <Password><Value>{pw}</Value><PlainText>true</PlainText></Password>\n\
          </LocalAccount>\n\
        </LocalAccounts>\n\
      </UserAccounts>\n\
      <AutoLogon>\n\
        <Enabled>true</Enabled>\n\
        <LogonCount>1</LogonCount>\n\
        <Username>{USER}</Username>\n\
        <Password><Value>{pw}</Value><PlainText>true</PlainText></Password>\n\
      </AutoLogon>\n\
      <FirstLogonCommands>\n\
        <SynchronousCommand wcm:action=\"add\">\n\
          <Order>1</Order>\n\
          <CommandLine>{}</CommandLine>\n\
          <Description>aterm lane first logon</Description>\n\
        </SynchronousCommand>\n\
      </FirstLogonCommands>\n",
            xml_escape(&first_logon_command())
        ),
    ));
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
<unattend xmlns=\"urn:schemas-microsoft-com:unattend\">\n\
  <settings pass=\"windowsPE\">\n{pe}  </settings>\n\
  <settings pass=\"specialize\">\n{specialize}  </settings>\n\
  <settings pass=\"oobeSystem\">\n{oobe}  </settings>\n\
</unattend>\n"
    )
}

/// The script the first logon runs from the answer media (as [`USER`], elevated).
pub const FIRST_LOGON_SCRIPT: &str = r#"$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$media = Split-Path -Parent $PSCommandPath
New-Item -ItemType Directory -Force -Path 'C:\lane' | Out-Null
Start-Transcript -Path 'C:\lane\first-logon.log' -Append
try {
  powercfg /hibernate off
  foreach ($s in 'standby-timeout-ac', 'monitor-timeout-ac', 'hibernate-timeout-ac', 'disk-timeout-ac') {
    powercfg /change $s 0
  }
  Add-MpPreference -ExclusionPath 'C:\lane'
  Set-ItemProperty -Path 'HKLM:\SYSTEM\CurrentControlSet\Control\FileSystem' -Name LongPathsEnabled -Value 1 -Type DWord
  $au = 'HKLM:\SOFTWARE\Policies\Microsoft\Windows\WindowsUpdate\AU'
  New-Item -Force -Path $au | Out-Null
  Set-ItemProperty -Path $au -Name NoAutoUpdate -Value 1 -Type DWord
  pnputil /add-driver (Join-Path $media 'NetKVM\w11\ARM64\netkvm.inf') /install
  if ($LASTEXITCODE -notin 0, 3010) { throw "pnputil exited $LASTEXITCODE" }
  $deadline = (Get-Date).AddMinutes(10)
  while (-not (Get-NetAdapter | Where-Object Status -eq 'Up')) {
    if ((Get-Date) -gt $deadline) { throw 'no network adapter came up' }
    Start-Sleep -Seconds 5
  }
  if ((Get-WindowsCapability -Online -Name 'OpenSSH.Server~~~~0.0.1.0').State -ne 'Installed') {
    Add-WindowsCapability -Online -Name 'OpenSSH.Server~~~~0.0.1.0' | Out-Null
  }
  Set-Service -Name sshd -StartupType Automatic
  Start-Service sshd
  $ssh = 'C:\ProgramData\ssh'
  $keys = Join-Path $ssh 'administrators_authorized_keys'
  Copy-Item -Force (Join-Path $media 'administrators_authorized_keys') $keys
  icacls $keys /inheritance:r /grant '*S-1-5-32-544:F' /grant '*S-1-5-18:F'
  if ($LASTEXITCODE -ne 0) { throw "icacls exited $LASTEXITCODE" }
  $runner = 'lane'
  if (-not (Get-LocalUser -Name $runner -ErrorAction SilentlyContinue)) {
    $chars = [char[]]((48..57) + (65..90) + (97..122))
    $secret = (-join (1..32 | ForEach-Object { $chars | Get-Random })) + 'aA1!'
    $pw = ConvertTo-SecureString $secret -AsPlainText -Force
    New-LocalUser -Name $runner -Password $pw -PasswordNeverExpires -AccountNeverExpires -Description 'aterm lane runner' | Out-Null
    Add-LocalGroupMember -SID 'S-1-5-32-545' -Member $runner
  }
  $runnerKeys = Join-Path $ssh 'lane_authorized_keys'
  Copy-Item -Force (Join-Path $media 'administrators_authorized_keys') $runnerKeys
  icacls $runnerKeys /inheritance:r /grant '*S-1-5-32-544:F' /grant '*S-1-5-18:F' /grant "${runner}:R"
  if ($LASTEXITCODE -ne 0) { throw "icacls exited $LASTEXITCODE" }
  $config = Join-Path $ssh 'sshd_config'
  $text = Get-Content -Raw $config
  if ($text -notmatch '(?m)^PasswordAuthentication no') {
    $text = "PasswordAuthentication no`r`n" + $text
  }
  if ($text -notmatch "(?m)^Match User $runner") {
    $text = $text.TrimEnd() + "`r`n`r`nMatch User $runner`r`n       AuthorizedKeysFile __PROGRAMDATA__/ssh/lane_authorized_keys`r`n"
  }
  Set-Content -Encoding ascii -Path $config -Value $text
  Restart-Service sshd
  if (-not (Get-NetFirewallRule -Name 'aterm-lane-ssh' -ErrorAction SilentlyContinue)) {
    New-NetFirewallRule -Name 'aterm-lane-ssh' -DisplayName 'aterm lane SSH' -Protocol TCP -LocalPort 22 -Direction Inbound -Action Allow -Profile Any | Out-Null
  }
  Set-Content -Path 'C:\lane\first-logon.done' -Value 'ok'
} catch {
  Write-Output "FAILED: $_"
  Set-Content -Path 'C:\lane\first-logon.failed' -Value "$_"
} finally {
  Stop-Transcript
}
"#;

/// The first-logon script ([`FIRST_LOGON_SCRIPT`]), with CRLF line ends.
#[must_use]
pub fn first_logon_script() -> String {
    FIRST_LOGON_SCRIPT.replace('\n', "\r\n")
}

/// `text` as a PowerShell single-quoted string literal (a `'` doubled).
#[must_use]
pub fn ps_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// One provisioning step: a PowerShell script run in the guest, as the administrator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub label: &'static str,
    pub script: String,
    pub runner: bool,
    pub reboot: bool,
}

/// Every step's preamble: stop on a cmdlet's error, no progress records
const PREAMBLE: &str =
    "$ErrorActionPreference = 'Stop'\n$ProgressPreference = 'SilentlyContinue'\n";

/// A download to [`DOWNLOADS`] with Windows' own curl, failing on an HTTP error.
fn download(url: &str, file: &str) -> String {
    format!(
        "New-Item -ItemType Directory -Force -Path {dl} | Out-Null\n\
         $f = Join-Path {dl} {file}\n\
         curl.exe -fsSL --retry 2 -o $f {url}\n\
         if ($LASTEXITCODE -ne 0) {{ throw \"download of {name} failed (curl exited $LASTEXITCODE)\" }}\n",
        dl = ps_quote(DOWNLOADS),
        file = ps_quote(file),
        url = ps_quote(url),
        name = file.replace('"', ""),
    )
}

/// A check that `$f` has the SHA-256 `sha256`.
fn digest(sha256: &str) -> String {
    format!(
        "if ((Get-FileHash -Algorithm SHA256 $f).Hash -ne {}) {{ throw \"$f does not have its published digest\" }}\n",
        ps_quote(sha256)
    )
}

/// The provisioning plan, in order.
#[must_use]
pub fn provision_steps() -> Vec<Step> {
    let (git_tag, git_file, git_sha) = GIT;
    let git = format!(
        "{PREAMBLE}$git = 'C:\\Program Files\\Git\\cmd\\git.exe'\n\
         if (-not (Test-Path $git)) {{\n{}{}\
         $p = Start-Process -FilePath $f -ArgumentList '/VERYSILENT','/NORESTART','/NOCANCEL','/SP-','/SUPPRESSMSGBOXES' -Wait -PassThru\n\
         if ($p.ExitCode -ne 0) {{ throw \"the Git installer exited $($p.ExitCode)\" }}\n}}\n\
         & $git config --system core.longpaths true\n\
         & $git --version\n",
        download(
            &format!(
                "https://github.com/git-for-windows/git/releases/download/{git_tag}/{git_file}"
            ),
            git_file
        ),
        digest(git_sha),
    );
    let (pwsh_version, pwsh_file, pwsh_sha) = PWSH;
    let pwsh = format!(
        "{PREAMBLE}$pwsh = 'C:\\Program Files\\PowerShell\\7\\pwsh.exe'\n\
         function Version {{ if (Test-Path $pwsh) {{ & $pwsh -NoLogo -NoProfile -NonInteractive -Command '$PSVersionTable.PSVersion.ToString()' }} }}\n\
         if ((Version) -ne {want}) {{\n{}{}\
         $p = Start-Process -FilePath msiexec.exe -ArgumentList '/i',$f,'/quiet','/norestart','ADD_PATH=1','DISABLE_TELEMETRY=1','USE_MU=0','ENABLE_MU=0' -Wait -PassThru\n\
         if ($p.ExitCode -notin 0, 3010) {{ throw \"the PowerShell installer exited $($p.ExitCode)\" }}\n\
         if ((Version) -ne {want}) {{ throw \"pwsh.exe is not {pwsh_version} after its install\" }}\n}}\n\
         \"pwsh $(Version)\"\n",
        download(
            &format!(
                "https://github.com/PowerShell/PowerShell/releases/download/v{pwsh_version}/{pwsh_file}"
            ),
            pwsh_file
        ),
        digest(pwsh_sha),
        want = ps_quote(pwsh_version),
    );
    let (wsl_version, wsl_file, wsl_sha) = WSL;
    let wsl = format!(
        "{PREAMBLE}$feature = 'Microsoft-Windows-Subsystem-Linux'\n\
         if ((Get-WindowsOptionalFeature -Online -FeatureName $feature).State -ne 'Enabled') {{ \
         Enable-WindowsOptionalFeature -Online -FeatureName $feature -All -NoRestart -WarningAction SilentlyContinue | Out-Null }}\n\
         $exe = 'C:\\Program Files\\WSL\\wsl.exe'\n\
         function Version {{ if (Test-Path $exe) {{ (Get-Item $exe).VersionInfo.ProductVersion }} }}\n\
         if ((Version) -ne {want}) {{\n{}{}\
         $p = Start-Process -FilePath msiexec.exe -ArgumentList '/i',$f,'/quiet','/norestart' -Wait -PassThru\n\
         if ($p.ExitCode -notin 0, 3010) {{ throw \"the WSL installer exited $($p.ExitCode)\" }}\n\
         if ((Version) -ne {want}) {{ throw \"wsl.exe is not {wsl_version} after its install\" }}\n}}\n\
         \"wsl $(Version), $feature $((Get-WindowsOptionalFeature -Online -FeatureName $feature).State)\"\n",
        download(
            &format!("https://github.com/microsoft/WSL/releases/download/{wsl_version}/{wsl_file}"),
            wsl_file
        ),
        digest(wsl_sha),
        want = ps_quote(&format!("{wsl_version}.0")),
    );
    let adds: Vec<String> = VS_COMPONENTS
        .iter()
        .flat_map(|c| ["'--add'".to_string(), ps_quote(c)])
        .collect();
    let requires: Vec<String> = VS_COMPONENTS[1..].iter().map(|c| ps_quote(c)).collect();
    let vs = format!(
        "{PREAMBLE}$vswhere = Join-Path ${{env:ProgramFiles(x86)}} 'Microsoft Visual Studio\\Installer\\vswhere.exe'\n\
         function Have {{ if (Test-Path $vswhere) {{ & $vswhere -products * -requires {req} -property installationPath }} }}\n\
         if (-not (Have)) {{\n{dl}\
         $sig = Get-AuthenticodeSignature $f\n\
         if ($sig.Status -ne 'Valid' -or $sig.SignerCertificate.Subject -notmatch 'O=Microsoft Corporation') {{ throw \"$f is not signed by Microsoft: $($sig.Status)\" }}\n\
         $verb = if (Test-Path (Join-Path {path} 'Common7\\Tools\\VsDevCmd.bat')) {{ @('modify') }} else {{ @() }}\n\
         $p = Start-Process -FilePath $f -ArgumentList ($verb + @('--quiet','--wait','--norestart','--nocache','--installPath',{path},{adds})) -Wait -PassThru\n\
         if ($p.ExitCode -notin 0, 3010) {{ throw \"the Build Tools installer exited $($p.ExitCode)\" }}\n\
         if (-not (Have)) {{ throw 'the Build Tools installed without every component the plan names' }}\n}}\n\
         if (-not (Test-Path {clang})) {{ throw 'no arm64 clang.exe at {llvm}' }}\n\
         Have\n\
         & {clang} --version\n",
        req = requires.join(" "),
        dl = download(VS_BOOTSTRAPPER, "vs_BuildTools.exe"),
        path = ps_quote(VS_PATH),
        adds = adds.join(","),
        clang = ps_quote(&format!("{LLVM_BIN}\\clang.exe")),
        llvm = LLVM_BIN.replace('\'', "''"),
    );
    let (rustup_version, rustup_sha) = RUSTUP;
    let rustup = format!(
        "{PREAMBLE}$env:RUSTUP_HOME = {rustup_home}\n\
         $env:CARGO_HOME = {cargo_home}\n\
         $rustup = Join-Path $env:CARGO_HOME 'bin\\rustup.exe'\n\
         if (-not (Test-Path $rustup)) {{\n{}{}\
         & $f -y --default-toolchain none --profile minimal --no-modify-path\n\
         if ($LASTEXITCODE -ne 0) {{ throw \"rustup-init exited $LASTEXITCODE\" }}\n}}\n\
         & $rustup set auto-self-update disable\n\
         if ($LASTEXITCODE -ne 0) {{ throw \"rustup set auto-self-update exited $LASTEXITCODE\" }}\n\
         & $rustup toolchain install {RUST} --profile minimal --target {}\n\
         if ($LASTEXITCODE -ne 0) {{ throw \"rustup toolchain install exited $LASTEXITCODE\" }}\n\
         & $rustup run {RUST} rustc --version --verbose\n",
        download(
            &format!(
                "https://static.rust-lang.org/rustup/archive/{rustup_version}/{}/rustup-init.exe",
                Arch::Aarch64.triple()
            ),
            "rustup-init.exe"
        ),
        digest(rustup_sha),
        Arch::X86_64.triple(),
        rustup_home = ps_quote(RUSTUP_HOME),
        cargo_home = ps_quote(CARGO_HOME),
    );
    let dirs = format!(
        "{PREAMBLE}if (-not (Get-LocalUser -Name {runner} -ErrorAction SilentlyContinue) -or \
         -not (Test-Path {keys})) {{ throw 'the first logon made no {RUNNER} account or key' }}\n\
         foreach ($d in {src}, {target}, {dl}, {tasks}, {rustup_home}, {cargo_home}) {{ \
         New-Item -ItemType Directory -Force -Path $d | Out-Null }}\n\
         icacls {lane} /grant '{RUNNER}:(OI)(CI)M' /T /Q\n\
         if ($LASTEXITCODE -ne 0) {{ throw \"icacls exited $LASTEXITCODE\" }}\n\
         Add-MpPreference -ExclusionPath {lane}\n",
        runner = ps_quote(RUNNER),
        keys = ps_quote(RUNNER_KEYS),
        src = ps_quote(SRC),
        target = ps_quote(TARGET),
        dl = ps_quote(DOWNLOADS),
        tasks = ps_quote(TASKS),
        rustup_home = ps_quote(RUSTUP_HOME),
        cargo_home = ps_quote(CARGO_HOME),
        lane = ps_quote(LANE),
    );
    let terminal = format!(
        "{PREAMBLE}if (-not (Get-AppxPackage -Name {name})) {{ \
         Add-AppxPackage -RegisterByFamilyName -MainPackage {family} }}\n\
         (Get-AppxPackage -Name {name}).PackageFullName\n",
        name = ps_quote(TERMINAL_PACKAGE),
        family = ps_quote(&format!("{TERMINAL_PACKAGE}_8wekyb3d8bbwe")),
    );
    vec![
        Step {
            label: "lane directories, the runner's",
            script: dirs,
            runner: false,
            reboot: false,
        },
        Step {
            label: "Git for Windows",
            script: git,
            runner: false,
            reboot: false,
        },
        Step {
            label: "PowerShell 7",
            script: pwsh,
            runner: false,
            reboot: false,
        },
        Step {
            label: "Visual Studio Build Tools (MSVC arm64 and x64, Windows SDK)",
            script: vs,
            runner: false,
            reboot: false,
        },
        Step {
            label: "rust toolchain",
            script: rustup,
            runner: false,
            reboot: false,
        },
        Step {
            label: "Windows Terminal registered for the runner",
            script: terminal,
            runner: true,
            reboot: false,
        },
        Step {
            label: "WSL (its package, and WSL 1's Windows feature)",
            script: wsl,
            runner: false,
            reboot: false,
        },
        japanese_step(),
        languages_step(),
        display_step(),
        desktop_step(),
        wsl_distro_step(),
    ]
}

/// The Japanese IME made usable ([`JA_CAPABILITY`]).
#[must_use]
pub fn japanese_step() -> Step {
    let log = format!("{TASKS}\\capability.log");
    let body = format!(
        "Add-WindowsCapability -Online -Name {cap} *> {log}",
        cap = ps_quote(JA_CAPABILITY),
        log = ps_quote(&log),
    );
    let script = format!(
        "{PREAMBLE}$cap = {cap}\n\
         if ((Get-WindowsCapability -Online -Name $cap).State -ne 'Installed') {{\n\
         $name = 'aterm-lane-capability'\n\
         $action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument {argument}\n\
         $principal = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest\n\
         Register-ScheduledTask -TaskName $name -Action $action -Principal $principal -Force | Out-Null\n\
         try {{\n\
         Start-ScheduledTask -TaskName $name\n\
         $deadline = (Get-Date).AddSeconds({CAPABILITY_WITHIN_SECS})\n\
         do {{\n\
         Start-Sleep -Seconds 5\n\
         $info = Get-ScheduledTaskInfo -TaskName $name\n\
         $state = (Get-ScheduledTask -TaskName $name).State\n\
         if ((Get-Date) -gt $deadline) {{ throw \"$cap was not installed within {CAPABILITY_WITHIN_SECS} s\" }}\n\
         }} while ($state -in 'Running', 'Queued' -or $info.LastTaskResult -eq 0x41303)\n\
         if (Test-Path {qlog}) {{ Get-Content {qlog} }}\n\
         }} finally {{\n\
         Unregister-ScheduledTask -TaskName $name -Confirm:$false\n\
         Remove-Item -Force -ErrorAction SilentlyContinue {qlog}\n}}\n}}\n\
         $state = (Get-WindowsCapability -Online -Name $cap).State\n\
         if ($state -ne 'Installed') {{ throw \"$cap is $state\" }}\n\
         \"${{cap}}: $state\"\n",
        cap = ps_quote(JA_CAPABILITY),
        argument = ps_quote(&format!(
            "-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand {}",
            encoded(&body)
        )),
        qlog = ps_quote(&log),
    );
    Step {
        label: "the Japanese IME's language capability",
        script,
        runner: false,
        reboot: false,
    }
}

/// The runner's input languages ([`LANGUAGES`]), set by the runner (they are its own).
#[must_use]
pub fn languages_step() -> Step {
    let [first, rest @ ..] = LANGUAGES;
    let adds: String = rest
        .iter()
        .map(|l| format!("$l.Add({})\n", ps_quote(l)))
        .collect();
    let script = format!(
        "{PREAMBLE}$l = New-WinUserLanguageList {first}\n{adds}\
         Set-WinUserLanguageList $l -Force\n\
         'input languages: ' + ((Get-WinUserLanguageList).LanguageTag -join ', ')\n",
        first = ps_quote(first),
    );
    Step {
        label: "the runner's input languages (English, Japanese)",
        script,
        runner: true,
        reboot: false,
    }
}

/// Virtio-win's display driver ([`VIOGPUDO_FILES`], which the host copied to [`VIOGPUDO_GUEST`]).
#[must_use]
pub fn display_step() -> Step {
    let files: Vec<String> = VIOGPUDO_FILES
        .iter()
        .map(|(f, sha)| format!("@({}, {})", ps_quote(f), ps_quote(sha)))
        .collect();
    let script = format!(
        "{PREAMBLE}$d = {dir}\n\
         foreach ($p in @({files})) {{\n\
         $f = Join-Path $d $p[0]\n\
         if ((Get-FileHash -Algorithm SHA256 $f).Hash -ne $p[1]) {{ throw \"$f does not have its pinned digest\" }}\n}}\n\
         function Gpu {{ Get-PnpDevice -PresentOnly | Where-Object {{ $_.InstanceId -like 'PCI\\VEN_1AF4&DEV_1050*' }} }}\n\
         if (-not (Gpu)) {{ throw 'the VM has no virtio-gpu (PCI VEN_1AF4 DEV_1050)' }}\n\
         if ((Gpu).Service -ne {service}) {{\n\
         pnputil /add-driver (Join-Path $d 'viogpudo.inf') /install\n\
         if ($LASTEXITCODE -notin 0, 3010) {{ throw \"pnputil exited $LASTEXITCODE\" }}\n}}\n\
         $g = Gpu\n\
         if ($g.Service -ne {service}) {{ throw \"the virtio-gpu runs $($g.Service), not {VIOGPUDO_SERVICE}\" }}\n\
         \"$($g.FriendlyName): $($g.Status) ($($g.Service))\"\n",
        dir = ps_quote(VIOGPUDO_GUEST),
        files = files.join(", "),
        service = ps_quote(VIOGPUDO_SERVICE),
    );
    Step {
        label: "virtio-win's display driver on the virtio-gpu",
        script,
        runner: false,
        reboot: false,
    }
}

/// The runner's WSL distro ([`WSL_DISTRO`]).
#[must_use]
pub fn wsl_distro_step() -> Step {
    let (name, file, sha) = WSL_DISTRO;
    let script = format!(
        "{PREAMBLE}$wsl = 'C:\\Program Files\\WSL\\wsl.exe'\n\
         $env:WSL_UTF8 = '1'\n\
         $dir = Join-Path $env:LOCALAPPDATA {dir}\n\
         $have = Get-ChildItem 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Lxss' -ErrorAction SilentlyContinue | \
         Get-ItemProperty | Where-Object DistributionName -eq {name}\n\
         if ($have -and ($have.BasePath -replace '^\\\\\\\\\\?\\\\', '') -ne $dir) {{\n\
         & $wsl --unregister {name}\n\
         if ($LASTEXITCODE -ne 0) {{ throw \"wsl --unregister exited $LASTEXITCODE\" }}\n\
         $have = $null\n}}\n\
         if (-not $have) {{\n{}{}\
         New-Item -ItemType Directory -Force -Path (Split-Path $dir) | Out-Null\n\
         & $wsl --import {name} $dir $f --version 1\n\
         if ($LASTEXITCODE -ne 0) {{ throw \"wsl --import exited $LASTEXITCODE\" }}\n\
         Remove-Item -LiteralPath $f\n}}\n\
         & $wsl --set-default {name}\n\
         if ($LASTEXITCODE -ne 0) {{ throw \"wsl --set-default exited $LASTEXITCODE\" }}\n\
         & $wsl -d {name} --exec /bin/bash -c 'echo $WSL_DISTRO_NAME: bash $BASH_VERSION, kernel $(uname -r)'\n\
         if ($LASTEXITCODE -ne 0) {{ throw \"the distro's bash exited $LASTEXITCODE\" }}\n",
        download(
            &format!("https://cdimage.ubuntu.com/releases/noble/release/{file}"),
            file
        ),
        digest(sha),
        name = ps_quote(name),
        dir = ps_quote(&format!("{WSL_DIR}\\{name}")),
    );
    Step {
        label: "the runner's WSL distro",
        script,
        runner: true,
        reboot: false,
    }
}

/// The runner logged on at every boot, so a command can run in its interactive session.
#[must_use]
pub fn desktop_step() -> Step {
    let script = format!(
        "{PREAMBLE}$w = 'HKLM:\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\Winlogon'\n\
         $have = Get-ItemProperty -Path $w\n\
         if ($have.AutoAdminLogon -ne '1' -or $have.DefaultUserName -ne {runner} -or -not $have.DefaultPassword) {{\n\
         $chars = [char[]]((48..57) + (65..90) + (97..122))\n\
         $secret = (-join (1..32 | ForEach-Object {{ $chars | Get-Random }})) + 'aA1!'\n\
         Set-LocalUser -Name {runner} -Password (ConvertTo-SecureString $secret -AsPlainText -Force)\n\
         Set-ItemProperty -Path $w -Name DefaultUserName -Value {runner}\n\
         Set-ItemProperty -Path $w -Name DefaultDomainName -Value $env:COMPUTERNAME\n\
         Set-ItemProperty -Path $w -Name DefaultPassword -Value $secret\n\
         Set-ItemProperty -Path $w -Name AutoAdminLogon -Value '1'\n}}\n\
         Remove-ItemProperty -Path $w -Name AutoLogonCount -ErrorAction SilentlyContinue\n\
         Set-ItemProperty -Path 'HKLM:\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Policies\\System' \
         -Name EnableFirstLogonAnimation -Value 0 -Type DWord\n\
         'automatic logon: ' + (Get-ItemProperty -Path $w).DefaultUserName\n",
        runner = ps_quote(RUNNER),
    );
    Step {
        label: "the runner logged on at boot (its desktop session)",
        script,
        runner: false,
        reboot: true,
    }
}

/// A PowerShell condition that holds when the runner has an active console
fn desktop_condition() -> String {
    format!("((cmd /d /c 'query user 2>nul') -match '^\\s*>?{RUNNER}\\s.*\\sActive\\s')")
}

/// A PowerShell script that prints [`DESKTOP_UP`] when the runner's desktop session is up.
#[must_use]
pub fn desktop_check() -> String {
    format!("if {} {{ '{DESKTOP_UP}' }}\n", desktop_condition())
}

/// What [`desktop_check`] prints when the runner's desktop session is up.
pub const DESKTOP_UP: &str = "lane-desktop-up";

/// The plan's fingerprint (fnv-1a over every step), which [`STAMP`] holds once it ran.
#[must_use]
pub fn fingerprint(steps: &[Step]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for s in steps {
        for b in s
            .script
            .bytes()
            .chain([u8::from(s.runner), u8::from(s.reboot), 0x1f])
        {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("{h:016x}")
}

/// `script` as PowerShell's `-EncodedCommand` takes it.
#[must_use]
pub fn encoded(script: &str) -> String {
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    aterm_codec::base64::encode(&utf16).unwrap_or_default()
}

/// The command line that runs `script` in Windows PowerShell 5.1.
#[must_use]
pub fn powershell_command(script: &str) -> String {
    format!(
        "powershell -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -OutputFormat Text -EncodedCommand {}",
        encoded(script)
    )
}

/// The environment `cargo` gets for `arch`.
#[must_use]
pub fn cargo_env(arch: Arch) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = vec![
        ("RUSTUP_TOOLCHAIN".into(), RUST.into()),
        ("RUSTUP_HOME".into(), RUSTUP_HOME.into()),
        ("CARGO_HOME".into(), CARGO_HOME.into()),
        ("CARGO_INCREMENTAL".into(), "0".into()),
        ("CARGO_TARGET_DIR".into(), TARGET.into()),
        ("CARGO_TERM_COLOR".into(), "never".into()),
    ];
    if arch == Arch::X86_64 {
        env.push(("CARGO_BUILD_TARGET".into(), arch.triple().into()));
    }
    env
}

/// The cmd.exe line (sshd runs a command through `cmd.exe /c`) that runs `argv` in `dir`.
#[must_use]
pub fn exec_command(dir: &str, env: &[(String, String)], argv: &[String]) -> String {
    let mut parts: Vec<String> = env
        .iter()
        .map(|(k, v)| format!("set \"{k}={v}\""))
        .collect();
    parts.push(format!(
        "set \"PATH={CARGO_HOME}\\bin;C:\\Program Files\\Git\\cmd;{LLVM_BIN};%PATH%\""
    ));
    parts.push(format!("cd /d {}", cmd_quote(dir)));
    let line: Vec<String> = argv.iter().map(|a| cmd_quote(a)).collect();
    parts.push(format!("{} 2>&1", line.join(" ")));
    parts.join(" && ")
}

/// `arg` quoted for a Windows command line that cmd.exe reads first.
#[must_use]
pub fn cmd_quote(arg: &str) -> String {
    let plain = !arg.is_empty()
        && !arg.chars().any(|c| {
            matches!(
                c,
                ' ' | '\t' | '"' | '&' | '|' | '<' | '>' | '^' | '(' | ')' | '%' | '!'
            )
        });
    if plain {
        return arg.to_string();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            c => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                backslashes = 0;
                out.push(c);
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    if !arg.contains('"') {
        return out;
    }
    let mut escaped = String::with_capacity(out.len() + 8);
    for c in out.chars() {
        if matches!(c, '"' | '&' | '|' | '<' | '>' | '^' | '(' | ')') {
            escaped.push('^');
        }
        escaped.push(c);
    }
    escaped
}

/// The batch file a command runs from in the runner's desktop.
#[must_use]
pub fn desktop_batch(line: &str, name: &str) -> String {
    format!(
        "@echo off\r\nchcp 65001 >nul\r\n{line}\r\nset LANE_CODE=%ERRORLEVEL%\r\n\
         >\"{TASKS}\\{name}.code\" echo %LANE_CODE%\r\nexit /b %LANE_CODE%\r\n"
    )
}

/// How long the runner's desktop session may take to come up, and a task to start.
pub const DESKTOP_WITHIN_SECS: u64 = 300;
pub const TASK_START_WITHIN_SECS: u64 = 60;

/// How a command runs in the runner's desktop ([`desktop_runner`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Desktop {
    /// In a console window of its own (cmd.exe's), waited for.
    Console,
    /// With no window (`conhost.exe --headless`), waited for.
    Hidden,
    /// With no window, started and left running.
    Started,
}

/// The PowerShell script (run as administrator) that runs a command in the runner's desktop.
#[must_use]
pub fn desktop_runner(name: &str, how: Desktop) -> String {
    let bat = format!("{TASKS}\\{name}.cmd");
    let log = format!("{TASKS}\\{name}.log");
    let code = format!("{TASKS}\\{name}.code");
    let action = match how {
        Desktop::Console => {
            "$action = New-ScheduledTaskAction -Execute (Join-Path $env:SystemRoot 'System32\\cmd.exe') \
             -Argument \"/d /c $bat > $log 2>&1\"\n"
        }
        Desktop::Hidden | Desktop::Started => {
            "$action = New-ScheduledTaskAction -Execute (Join-Path $env:SystemRoot 'System32\\conhost.exe') \
             -Argument \"--headless cmd.exe /d /c $bat > $log 2>&1\"\n"
        }
    };
    let mut s = format!(
        "{PREAMBLE}if ($PSCommandPath) {{ Remove-Item -Force -ErrorAction SilentlyContinue $PSCommandPath }}\n\
         $name = {qname}\n\
         $bat = {qbat}\n\
         $log = {qlog}\n\
         $codefile = {qcode}\n\
         $deadline = (Get-Date).AddSeconds({DESKTOP_WITHIN_SECS})\n\
         while ($true) {{\n\
         if {up} {{ break }}\n\
         if ((Get-Date) -gt $deadline) {{ [Console]::Error.WriteLine('{RUNNER} has no desktop session (it is logged on at boot)'); exit 125 }}\n\
         Start-Sleep -Seconds 1\n}}\n\
         {action}\
         $principal = New-ScheduledTaskPrincipal -UserId {runner} -LogonType Interactive -RunLevel Limited\n\
         $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries \
         -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew\n\
         Register-ScheduledTask -TaskName $name -Action $action -Principal $principal -Settings $settings | Out-Null\n\
         $code = 125\n",
        qname = ps_quote(name),
        qbat = ps_quote(&bat),
        qlog = ps_quote(&log),
        qcode = ps_quote(&code),
        up = desktop_condition(),
        runner = ps_quote(RUNNER),
    );
    if how == Desktop::Started {
        s.push_str(&format!(
            "Start-ScheduledTask -TaskName $name\n\
             $started = Get-Date\n\
             while ($true) {{\n\
             $info = Get-ScheduledTaskInfo -TaskName $name\n\
             $state = (Get-ScheduledTask -TaskName $name).State\n\
             if ($state -eq 'Running' -or $info.LastTaskResult -ne 0x41303) {{ $code = 0; break }}\n\
             if (((Get-Date) - $started).TotalSeconds -gt {TASK_START_WITHIN_SECS}) {{\n\
             [Console]::Error.WriteLine(\"the task $name did not start within {TASK_START_WITHIN_SECS} s (state $state)\")\n\
             break\n}}\n\
             Start-Sleep -Milliseconds 250\n}}\n\
             exit $code\n"
        ));
        return s;
    }
    s.push_str(&format!(
        "$stream = $null\n\
         try {{\n\
         Start-ScheduledTask -TaskName $name\n\
         $out = [Console]::OpenStandardOutput()\n\
         $buf = New-Object byte[] 65536\n\
         $started = Get-Date\n\
         while ($true) {{\n\
         $info = Get-ScheduledTaskInfo -TaskName $name\n\
         $state = (Get-ScheduledTask -TaskName $name).State\n\
         if ($null -eq $stream -and (Test-Path $log)) {{ $stream = [IO.File]::Open($log, 'Open', 'Read', 'ReadWrite, Delete') }}\n\
         if ($stream) {{ while (($n = $stream.Read($buf, 0, $buf.Length)) -gt 0) {{ $out.Write($buf, 0, $n) }}; $out.Flush() }}\n\
         $ran = $info.LastTaskResult -ne 0x41303\n\
         if ($ran -and $state -ne 'Running' -and $info.LastTaskResult -ne 0x41301) {{\n\
         if ($stream) {{ while (($n = $stream.Read($buf, 0, $buf.Length)) -gt 0) {{ $out.Write($buf, 0, $n) }}; $out.Flush() }}\n\
         $code = [BitConverter]::ToInt32([BitConverter]::GetBytes([uint32]$info.LastTaskResult), 0)\n\
         if (Test-Path $codefile) {{ $code = [int]((Get-Content -Raw $codefile).Trim()) }}\n\
         break\n}}\n\
         if (-not $ran -and ((Get-Date) - $started).TotalSeconds -gt {TASK_START_WITHIN_SECS}) {{\n\
         [Console]::Error.WriteLine(\"the task $name did not start within {TASK_START_WITHIN_SECS} s (state $state)\")\n\
         break\n}}\n\
         Start-Sleep -Milliseconds {poll}\n}}\n\
         }} finally {{\n\
         if ($stream) {{ $stream.Dispose() }}\n\
         Unregister-ScheduledTask -TaskName $name -Confirm:$false\n\
         Remove-Item -Force -ErrorAction SilentlyContinue $bat, $log, $codefile\n}}\n\
         exit $code\n",
        poll = if how == Desktop::Hidden { 100 } else { 500 },
    ));
    s
}

/// The PowerShell that ends a [`Desktop::Started`] task.
#[must_use]
pub fn desktop_stop(name: &str) -> String {
    format!(
        "$name = {qname}\n\
         if (Get-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue) {{\n\
         Stop-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue\n\
         Unregister-ScheduledTask -TaskName $name -Confirm:$false\n}}\n\
         Remove-Item -Force -ErrorAction SilentlyContinue {bat}, {log}, {code}\n",
        qname = ps_quote(name),
        bat = ps_quote(&format!("{TASKS}\\{name}.cmd")),
        log = ps_quote(&format!("{TASKS}\\{name}.log")),
        code = ps_quote(&format!("{TASKS}\\{name}.code")),
    )
}

/// The PowerShell script (run as administrator) that shuts the guest down.
#[must_use]
pub fn end_leftovers() -> String {
    format!(
        "{PREAMBLE}function Get-LaneLeftovers {{\n\
         @(Get-Process -IncludeUserName -ErrorAction Stop | Where-Object {{ $_.SessionId -eq 0 -and $_.UserName -like {owner} }})\n}}\n\
         $left = @(Get-LaneLeftovers)\n\
         $failed = @()\n\
         foreach ($p in $left) {{\n\
         & taskkill.exe /T /F /PID $p.Id *> $null\n\
         if ($LASTEXITCODE -ne 0) {{ $failed += \"$($p.Id):$LASTEXITCODE\" }}\n}}\n\
         $remaining = @(Get-LaneLeftovers)\n\
         if ($remaining.Count -ne 0) {{\n\
         throw \"guest processes remain: $($remaining.Id -join ', ') (taskkill failures: $($failed -join ', '))\"\n}}\n\
         \"ended $($left.Count)\"\n",
        owner = ps_quote(&format!("*\\{RUNNER}")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_answer_file_is_one_xml_document_with_every_pass() {
        let xml = autounattend("p<w>&'\"");
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<unattend "));
        for pass in ["windowsPE", "specialize", "oobeSystem"] {
            assert_eq!(
                xml.matches(&format!("<settings pass=\"{pass}\">")).count(),
                1,
                "{pass}"
            );
        }
        assert_eq!(
            xml.matches("<settings ").count(),
            xml.matches("</settings>").count()
        );
        assert_eq!(
            xml.matches("<component ").count(),
            xml.matches("</component>").count()
        );
        assert_eq!(xml.matches("p&lt;w&gt;&amp;&apos;&quot;").count(), 2);
        assert!(!xml.contains("p<w>"));
        assert!(xml.contains("<AcceptEula>true</AcceptEula>"));
        assert!(xml.contains(&format!("<Value>{EDITION}</Value>")));
        assert!(xml.contains("PreventDeviceEncryption"));
        for key in LAB_CONFIG {
            assert!(
                xml.contains(&format!("/v {key} /t REG_DWORD /d 1 /f")),
                "{key}"
            );
        }
        assert_eq!(
            xml.matches("processorArchitecture=\"arm64\"").count(),
            xml.matches("<component ").count()
        );
    }

    #[test]
    fn the_first_logon_finds_its_script_on_any_letter() {
        let c = first_logon_command();
        assert!(c.contains("%d:\\aterm-lane.ps1"));
        assert!(c.contains("in (D E F"));
        assert!(autounattend("x").contains(&xml_escape(&c)));
        let script = first_logon_script();
        assert!(!script.contains("\r\r"));
        assert!(script.contains("OpenSSH.Server~~~~0.0.1.0"));
        assert!(script.contains("administrators_authorized_keys"));
        assert!(script.contains(FIRST_LOGON_DONE.rsplit('\\').next().unwrap_or("")));
        assert!(script.contains(&format!("$runner = '{RUNNER}'")));
        let keys = RUNNER_KEYS.rsplit('\\').next().unwrap_or("");
        assert!(script.contains(&format!("Join-Path $ssh '{keys}'")));
        assert!(script.contains(&format!("__PROGRAMDATA__/ssh/{keys}")));
    }

    #[test]
    fn single_quotes_double_inside_a_powershell_literal() {
        assert_eq!(ps_quote("it's"), "'it''s'");
        assert_eq!(ps_quote(""), "''");
    }

    #[test]
    fn windows_command_lines_quote_as_command_line_to_argv_reads_them() {
        assert_eq!(cmd_quote("cargo"), "cargo");
        assert_eq!(cmd_quote("-p"), "-p");
        assert_eq!(cmd_quote(""), "\"\"");
        assert_eq!(cmd_quote("a b"), "\"a b\"");
        assert_eq!(cmd_quote("say \"hi\""), "^\"say \\^\"hi\\^\"^\"");
        assert_eq!(cmd_quote("C:\\dir\\"), "C:\\dir\\");
        assert_eq!(cmd_quote("C:\\a dir\\"), "\"C:\\a dir\\\\\"");
        assert_eq!(cmd_quote("a&b"), "\"a&b\"");
        assert_eq!(cmd_quote("a\"&b"), "^\"a\\^\"^&b^\"");
        assert_eq!(cmd_quote("x \"y\" (z)|w"), "^\"x \\^\"y\\^\" ^(z^)^|w^\"");
        assert_eq!(cmd_quote("a\"|\"b"), "^\"a\\^\"^|\\^\"b^\"");
    }

    /// cmd.exe's reading of a quoted word: quote state toggled at every `"`,
    fn as_cmd_hands_on(word: &str) -> Result<String, char> {
        let mut out = String::new();
        let mut quoted = false;
        let mut chars = word.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => {
                    quoted = !quoted;
                    out.push(c);
                }
                '^' if !quoted => out.extend(chars.next()),
                '&' | '|' | '<' | '>' | '(' | ')' if !quoted => return Err(c),
                c => out.push(c),
            }
        }
        if quoted {
            return Err('"');
        }
        Ok(out)
    }

    /// CommandLineToArgvW's reading of one word (backslashes before a quote
    fn as_argv_reads(word: &str) -> String {
        let mut out = String::new();
        let mut backslashes = 0usize;
        for c in word.chars() {
            match c {
                '\\' => backslashes += 1,
                '"' => {
                    out.extend(std::iter::repeat_n('\\', backslashes / 2));
                    if backslashes % 2 == 1 {
                        out.push('"');
                    }
                    backslashes = 0;
                }
                c => {
                    out.extend(std::iter::repeat_n('\\', backslashes));
                    backslashes = 0;
                    out.push(c);
                }
            }
        }
        out.extend(std::iter::repeat_n('\\', backslashes));
        out
    }

    #[test]
    fn a_quoted_word_reaches_the_program_whole_through_cmd() {
        for word in [
            "a\"&b",
            "x \"y\" (z)|w",
            "a\"|\"b",
            "\"a\" & \"b\"",
            "say \"hi\"",
            "C:\\a dir\\",
            "<in> ^caret^",
            "plain",
        ] {
            let q = cmd_quote(word);
            let handed = as_cmd_hands_on(&q)
                .unwrap_or_else(|c| panic!("{word:?}: cmd acts on {c:?} in {q}"));
            assert_eq!(as_argv_reads(&handed), word, "{q}");
        }
        assert_eq!(as_cmd_hands_on("\"a\\\"&b\""), Err('&'));
        assert_eq!(as_cmd_hands_on("\"a\\\"^&b\""), Err('"'));
    }

    #[test]
    fn an_encoded_command_is_the_scripts_utf16le_in_base64() {
        // "dir" in UTF-16LE is 64 00 69 00 72 00.
        assert!(powershell_command("dir").ends_with(" -EncodedCommand ZABpAHIA"));
    }

    #[test]
    fn the_plan_pins_what_it_downloads_and_its_fingerprint_follows_it() {
        let steps = provision_steps();
        let all: String = steps.iter().map(|s| s.script.as_str()).collect();
        assert!(all.contains(GIT.2));
        assert!(all.contains(PWSH.2));
        assert!(all.contains(RUSTUP.1));
        assert!(all.contains(&format!("toolchain install {RUST}")));
        assert!(all.contains("x86_64-pc-windows-msvc"));
        for c in VS_COMPONENTS {
            assert!(all.contains(c), "{c}");
        }
        let terminal = steps
            .iter()
            .filter(|s| s.runner)
            .map(|s| s.script.as_str())
            .collect::<String>();
        assert!(terminal.contains(&format!("-MainPackage '{TERMINAL_PACKAGE}_8wekyb3d8bbwe'")));
        assert!(
            steps
                .iter()
                .filter(|s| !s.runner)
                .all(|s| !s.script.contains("Add-AppxPackage"))
        );
        let mut changed = steps.clone();
        changed[0].script.push(' ');
        assert_ne!(fingerprint(&steps), fingerprint(&changed));
        let mut moved = steps.clone();
        moved[0].runner = !moved[0].runner;
        assert_ne!(fingerprint(&steps), fingerprint(&moved));
        let mut restarted = steps.clone();
        restarted[0].reboot = !restarted[0].reboot;
        assert_ne!(fingerprint(&steps), fingerprint(&restarted));
        assert_eq!(fingerprint(&steps), fingerprint(&provision_steps()));
        let at = steps
            .iter()
            .position(|s| s == &desktop_step())
            .unwrap_or_else(|| unreachable!());
        let desktop = &steps[at];
        assert!(desktop.reboot && !desktop.runner);
        assert_eq!(steps.iter().filter(|s| s.reboot).count(), 1);
        assert_eq!(&steps[at + 1..], &[wsl_distro_step()]);
        assert!(desktop.script.contains("AutoAdminLogon -Value '1'"));
        assert!(
            desktop
                .script
                .contains(&format!("DefaultUserName -Value '{RUNNER}'"))
        );
        assert!(all.contains(&ps_quote(TASKS)));
        assert!(all.contains(WSL.2) && all.contains(WSL_DISTRO.2));
        let feature = steps
            .iter()
            .position(|s| s.script.contains("Microsoft-Windows-Subsystem-Linux"))
            .unwrap_or_else(|| unreachable!());
        assert!(feature < at && !steps[feature].runner);
        let distro = wsl_distro_step();
        assert!(distro.runner);
        assert!(distro.script.contains(&format!(
            "$dir = Join-Path $env:LOCALAPPDATA '{WSL_DIR}\\{}'",
            WSL_DISTRO.0
        )));
        assert!(
            distro
                .script
                .contains(&format!("--import '{}' $dir $f --version 1", WSL_DISTRO.0))
        );
    }

    #[test]
    fn a_desktop_run_is_a_task_of_the_runners_in_its_session() {
        let line = exec_command(
            SRC,
            &cargo_env(Arch::Aarch64),
            &["cargo".into(), "test".into()],
        );
        let bat = desktop_batch(&line, "aterm-lane-1-2");
        assert!(bat.starts_with("@echo off\r\nchcp 65001 >nul\r\n"));
        assert!(bat.contains(&format!("{line}\r\nset LANE_CODE=%ERRORLEVEL%\r\n")));
        assert!(bat.contains(&format!(
            ">\"{TASKS}\\aterm-lane-1-2.code\" echo %LANE_CODE%\r\n"
        )));
        assert!(bat.ends_with("exit /b %LANE_CODE%\r\n"));
        for how in [Desktop::Console, Desktop::Hidden, Desktop::Started] {
            let ps = desktop_runner("aterm-lane-1-2", how);
            assert!(ps.contains(&format!("$bat = '{TASKS}\\aterm-lane-1-2.cmd'")));
            assert!(ps.contains(&format!("$log = '{TASKS}\\aterm-lane-1-2.log'")));
            assert!(ps.contains(&format!(
                "New-ScheduledTaskPrincipal -UserId '{RUNNER}' -LogonType Interactive -RunLevel Limited"
            )));
            assert!(ps.contains(&format!("if {} {{ break }}", desktop_condition())));
            assert!(ps.contains(&format!("AddSeconds({DESKTOP_WITHIN_SECS})")));
            assert!(ps.contains(&format!("-gt {TASK_START_WITHIN_SECS}")));
            assert!(ps.trim_end().ends_with("exit $code"));
            assert_eq!(
                ps.contains("-Argument \"/d /c $bat > $log 2>&1\""),
                how == Desktop::Console
            );
            assert_eq!(
                ps.contains("'System32\\conhost.exe') -Argument \"--headless cmd.exe /d /c $bat > $log 2>&1\""),
                how != Desktop::Console
            );
            let finally = ps.find("} finally {").unwrap_or(usize::MAX);
            let unregister = ps.find("Unregister-ScheduledTask");
            if how == Desktop::Started {
                assert!(unregister.is_none() && finally == usize::MAX);
            } else {
                assert!(unregister.is_some_and(|u| u > finally));
            }
        }
        assert!(desktop_check().contains(DESKTOP_UP));
    }

    #[test]
    fn the_desk_steps_install_the_ime_the_languages_and_the_display_driver() {
        let steps = provision_steps();
        let at = |s: &Step| steps.iter().position(|x| x == s).unwrap_or(usize::MAX);
        let desktop = at(&desktop_step());
        for s in [japanese_step(), languages_step(), display_step()] {
            assert!(at(&s) < desktop, "{}", s.label);
        }
        let ja = japanese_step();
        assert!(!ja.runner && ja.script.contains(&ps_quote(JA_CAPABILITY)));
        assert!(
            ja.script
                .contains(&format!("AddSeconds({CAPABILITY_WITHIN_SECS})"))
        );
        assert!(languages_step().runner);
        let display = display_step();
        assert!(!display.runner);
        for (file, sha) in VIOGPUDO_FILES {
            assert!(
                display.script.contains(&format!("@('{file}', '{sha}')")),
                "{file}"
            );
        }
        assert!(display.script.contains(&ps_quote(VIOGPUDO_GUEST)));
    }

    #[test]
    fn an_exec_sets_the_environment_and_returns_the_commands_status() {
        let s = exec_command(
            SRC,
            &cargo_env(Arch::X86_64),
            &["cargo".into(), "build".into(), "-p".into(), "aterm".into()],
        );
        assert!(s.starts_with("set \"RUSTUP_TOOLCHAIN="));
        assert!(s.contains("set \"CARGO_BUILD_TARGET=x86_64-pc-windows-msvc\" && "));
        assert!(s.contains(" && cd /d C:\\lane\\src && "));
        assert!(s.ends_with(" && cargo build -p aterm 2>&1"));
        assert!(
            !cargo_env(Arch::Aarch64)
                .iter()
                .any(|(k, _)| k == "CARGO_BUILD_TARGET")
        );
    }
}
