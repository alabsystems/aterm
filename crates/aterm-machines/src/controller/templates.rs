// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The files `aterm machines init` writes once: the inventory and the lab baseline the owner
//! asked for (design ruling M1). Both are the owner's to edit afterwards; `init` never
//! overwrites an existing file.
//!
//! Every pin below was downloaded and checked on 2026-10-16: the SHA-256 of the bytes at the
//! URL, and the Developer ID Team ID on the app or binary inside
//! (`codesign --verify --deep --strict -R`).

/// `machines.toml` as `init` writes it.
pub const INVENTORY: &str = r#"# aterm machines: the Macs this controller manages (docs/DESIGN-machines-2026-10-16.md).
# `aterm machines enroll` appends a [[host]] block for each Mac it enrolls; edit the
# blocks freely (baseline, users), but leave `uuid` alone: it is the Mac's identity.
schema = 1

[controller]
# Hosts reached at once by plan, apply, run and rekey.
parallel = 8

# The GitHub machine user the hosts' keys are registered with (design §7). Create the
# account, `gh auth login` as it on this Mac, then uncomment.
# [github]
# account = "your-lab-bot"
"#;

/// `baselines/default.toml` as `init` writes it: the owner's lab, from the request that
/// started this design.
pub const DEFAULT_BASELINE: &str = r#"# The baseline every host gets unless its [[host]] block names another.
# `aterm machines plan --all` shows what it would change; `apply --all` changes it.
# {name}, {serial} and {uuid} are filled per host; {user} per account.
schema = 1

# Caps Lock acts as Control, for every keyboard model the Mac has seen (takes effect at
# the next login). Add external keyboards that are not plugged in now as "<vendor>-<product>".
[keyboard]
caps_lock = "control"
models = []

# Consumer features off: `false` turns each one off.
[consumer]
siri = false
siri_data_sharing = false
analytics = false
personalized_ads = false
airdrop = false
widgets = false
ds_store_on_network = false
# Desktops with no keyboard attached: stop the Bluetooth setup assistant at login.
bluetooth_setup_assistant = false

# No Apple Account, Siri, Screen Time or what's-new panes at login.
[setup]
skip_assistant = true

[names]
computer_name = "{name}"
local_host_name = "{name}"

[sharing]
remote_apple_events = false

[firewall]
enabled = true
stealth = true

[time]
network_time = true

# Always on while plugged in; laptops still sleep on battery as macOS chooses.
[power.ac]
sleep = 0
disk_sleep = 0
display_sleep = 15
power_nap = false
wake_on_lan = true
restart_after_power_loss = true

[updates]
automatic_check = true
automatic_download = true
automatic_macos_install = false
critical = true

[developer]
command_line_tools = true

# aterm from its release, once per Mac, plus the `aterm` command for each user. The
# toolset is the ALab compilers and provers (about 3.2 GB per user, from github.com).
[aterm]
toolset = true

[homebrew]
pkg = { url = "https://github.com/Homebrew/brew/releases/download/7.0.9/Homebrew.pkg", sha256 = "e021295947a4a9b7ca1a8eff310c9ea1db14d923c5acf7d79e325b60a81f1a15", team_id = "927JGANW46" }
formulae = ["git", "ripgrep", "fd", "jq", "tmux", "fzf", "wget", "htop", "coreutils", "git-lfs"]
casks = []

[[app]]
name = "Emacs"
kind = "dmg-app"
url = "https://emacsformacosx.com/emacs-builds/Emacs-31.1-universal.dmg"
sha256 = "f0383bcbf0104947d3a46266aae43e3dc9e38fd68eeb35f11ee6b9444277b8ec"
team_id = "5BRAQAFB8B"
bundle = "Emacs.app"
version = "31.1"

[[app]]
name = "Caffeine"
kind = "dmg-app"
url = "https://github.com/IntelliScape/caffeine/releases/download/1.1.4/Caffeine.dmg"
sha256 = "1ad34c3299a0c866873c4feb432c3e11186375adb6f43c5f481ae9d46d06d723"
team_id = "YD6LEYT6WZ"
bundle = "Caffeine.app"
version = "1.1.4"

# gh's installer package carries no installer signature; its binary is checked against
# GitHub's Team ID instead (design §8).
[[app]]
name = "gh"
kind = "pkg"
url = "https://github.com/cli/cli/releases/download/v2.102.0/gh_2.102.0_macOS_universal.pkg"
sha256 = "1787e65f36626245a6b3cee353a458f6ea4372008548ceb8fb5d5dfb55b006d0"
team_id = "VEKTX9H2N7"
unsigned = true
pkg_id = "com.github.cli"
binary = "/usr/local/bin/gh"
version = "2.102.0"

# A per-host GitHub key for each user, registered with the [github] machine user.
[github]
key = true
ssh_urls = true

# The switches only a person can flip; `aterm machines person` lists the Macs that still
# need them, and `screen` takes you there.
[person]
remote_login = true
remote_full_disk_access = true
screen_sharing = true
full_disk_access = ["aterm"]
developer_tools = ["aterm"]
"#;
