// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The files `aterm machines init` writes once: the inventory and the lab baseline the owner
//! asked for.

/// `machines.toml` as `init` writes it.
pub const INVENTORY: &str = r#"# aterm machines: the Macs this controller manages (docs/DESIGN-machines-2026-10-16.md).
# `aterm machines enroll` appends a [[host]] block for each Mac it enrolls; edit the
# blocks freely (baseline, users), but leave `uuid` alone: it is the Mac's identity.
schema = 1

[controller]
# Hosts reached at once by plan, apply, run and rekey.
parallel = 8

# The GitHub machine user the hosts' keys are registered with (design §7). Create the
# account, uncomment this, then `aterm machines github login` keeps its token (one with
# the admin:public_key scope) in this Mac's login keychain.
# [github]
# account = "your-lab-bot"
"#;

/// `baselines/default.toml` as `init` writes it: the owner's lab, from the request that
/// started this design.
pub const DEFAULT_BASELINE: &str = r#"# The baseline every host gets unless its [[host]] block names another.
# `aterm machines plan --all` shows what it would change; `apply --all` changes it.
# {name}, {tag}, {serial} and {uuid} are filled per host ({tag} is the name's leading
# letters and digits: m10 of m10-MacBook-M4-Max-36GB-1TB); {hardware} by each Mac from what
# it reads of itself; {user} per account.
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

# Each Mac named for what it is, read from the Mac: "m10 MacBook Pro M4 Max 36GB 1TB",
# m10-MacBook-Pro-M4-Max-36GB-1TB.local. For the inventory name alone, write "{name}" in both.
# host_name "" unsets a HostName set by hand, so `hostname`, shell prompts and ssh show the
# new Local hostname rather than the old name; a dotted name sets one instead.
[names]
computer_name = "{tag} {hardware}"
local_host_name = "{tag}-{hardware}"
host_name = ""

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

# Other software is yours to add: a [homebrew] table installs Homebrew and its formulae,
# an [[app]] row one signed app (docs/DESIGN-machines-2026-10-16.md §8). For keeping a Mac
# awake and editing a file, aterm has its own `aterm acaffiene` and `aterm edit`.

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

/// The SHA-256 of every earlier [`DEFAULT_BASELINE`], as `init` wrote it.
pub const EARLIER_DEFAULT_BASELINES: &[&str] = &[
    "cde13089544c52e4e8746fe671b083d4483482637c84221d9539303b3fd74363",
    "74dcb0817721f664b3a2b1d4c349e07cd110b6b76e5963f75a0fddc5586323e8",
    "e38407721722b11a0faac79954f4906f57eb92d4760bae3f092a00737380a11a",
    "80dbe78ab51b8dcf737a52074e4c1122c16bcdfa1310077001f2ca6a470ecb52",
    "afbb8d95fe3570f560676006cc83f9276a6a21fb4d7c5c652ff6e28458d296d1",
];
