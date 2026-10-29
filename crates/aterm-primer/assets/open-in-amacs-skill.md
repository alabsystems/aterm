---
name: open-in-amacs
description: Open a file for the person to fill in, check or change in amacs, aterm's editor (an Editor tab beside this one, the caret on the line that needs them), wait for them or not, and read back what they wrote, instead of printing its path. Use for "fill in", "edit this file", "open it for me", "let me edit", "let me check it", a CSV or TSV, a config (TOML, YAML, JSON, .env), a draft or template the person completes, or a command that needs an $EDITOR (git commit).
---
<!-- aterm skill v2 — MANAGED FILE, rewritten by `aterm agents` on every install/update; remove this marker line and aterm will leave the file alone (reported as `foreign`) -->

# Open a file for the person in amacs

amacs is aterm's editor: `amacs` is the one aterm binary under another name, and `aterm edit`
is the same command (`aterm help edit`, `amacs --help`). In an aterm tab it opens each file in a
native Editor tab to the right of yours, in your window. When the person has to fill in, check
or change a file (a CSV, a config, a draft, a template you wrote for them), open it for them
with the caret where they start. Printing its path makes them find and open it themselves.

`aterm ctl @self status` says `editor=amacs` where this works and `editor=-` where it does not
(a headless aterm, or a session in no window); there, give the person the path.

## Open it and keep working

```sh
amacs --no-wait +7 accounts.csv              # caret on line 7; returns at once
amacs --no-wait +3:12 config.toml notes.md   # +LINE[:COL] is for the next FILE only
```

- Your tool's shell has no terminal, so a bare `amacs FILE` exits 2 (`no terminal to edit in`):
  pass `--no-wait` or `--wait`. With `-nw`, inside tmux or screen, or once the window refuses
  the file or cannot be reached, amacs edits in the terminal instead, which a tool shell does
  not have, so they exit 2 too, even where `status` says `editor=amacs`. A refusal says
  `the aterm window did not open it (…)` on stderr first: a file under a cloud-storage folder
  is refused (`refused file-provider`), and a sandboxed tool shell that cannot reach the control
  socket gets `no aterm window could be asked`. Give the person the path then.
- Each run opens another Editor tab, even of a file already open: to wait on the one open, use
  `await edit uri=` below rather than running amacs on it again.
- A FILE that does not exist yet opens empty and its first save creates it. To have the person
  fill something in, write the skeleton first (the header row, the keys with empty values) and
  put the caret on the first blank.
- Say in your reply what to fill in, and that closing the tab hands it back (C-c C-c saves and
  closes).

## Wait until they close it

- `amacs --wait +7 accounts.csv` returns once the tab is closed (several FILEs open one after
  another): exit 0 closed, 1 abandoned (C-c C-k) or failed, 2 not opened (above), 130
  interrupted (the tab stays open), 75 the aterm serving it went away. It takes as long as the
  person does and has no timeout, so run it as a background command.
- After `--no-wait`, `aterm ctl @self await edit uri=file:///abs/accounts.csv
  timeout=600000` answers `OK edit done result=<finished|closed|aborted> saved=<0|1> seq=<n>`
  when they are done, or `OK timeout` (exit 124) at its timeout (600 s at most, 30 s without
  one): run it again. `saved=1`: the file holds their last text; `saved=0`: it does not
  (nothing saved, or typing left unsaved when they closed it), so read the file for what they
  did save.
- Which to use:

  ```sh
  # Claude Code: run_in_background, and you are told when it ends
  amacs --wait +7 accounts.csv
  # codex-alab: its shell tool wakes no turn when a command ends, so open, then wait in the
  # foreground, bounded, and run the wait again on exit 124
  amacs --no-wait +7 accounts.csv
  aterm ctl @self await edit uri=file:///abs/accounts.csv timeout=600000
  ```
- While either waits, your session is held: socket typing into it (`send`, `turn`, ...)
  answers `ERR busy editing`, and `status` says `editing=<n>`.

## Read what they wrote

- Once the tab is closed, the file on disk is the answer: read it.
- While it is open, `aterm ctl @self doc ls uri=file:///abs/accounts.csv` prints `doc=<d> uri=…
  seq=… dirty=0|1 disk=same|changed|missing|unknown views=… ticket=… cursor=<view>:<L>:<C> sel=…`
  (`dirty=1`: typing not saved yet; `cursor=`: where their caret is). A bare `doc ls` lists
  every document an Editor tab shows, each with its exact `uri=`. Name a file as a `file://`
  URI, its path absolute and percent-encoded (`%20` for a space): a bare path answers
  `ERR bad doc=`.
- `aterm ctl @self doc read file:///abs/accounts.csv [lines=A-B]` prints that buffer, unsaved
  typing included, as `<n>:<h6>|<text>` lines ending `-- lines=<total> seq=<s>` (256 KiB a
  read; `next=` says where the next read starts). A document no Editor tab shows answers
  `ERR unknown doc`.
- Leave the file alone while it is open: an agent's edits to an open document (`doc apply`,
  `doc save`) answer `ERR unsupported` for now, and a write to the file underneath them shows
  as `disk=changed`. Wait until they close it, or ask.

## As $EDITOR

- For a command you run that opens an editor, use `amacs --wait`: `GIT_EDITOR='amacs --wait' git
  commit` puts the message in an Editor tab and goes on when the person closes it (C-c C-c
  commits, C-c C-k stops the commit).
- An aterm tab's shell already exports `EDITOR=amacs` unless the person set `EDITOR` or
  `VISUAL` (`[amacs] default_editor = false` turns it off), so in their shell, which has a
  terminal, it is the editor of git, `crontab -e` and an agent's editor key (Claude Code's
  Ctrl-G). Your tool shell inherits it but has no terminal, where a bare `amacs` exits 2, so
  keep `--wait` on the commands you run.
