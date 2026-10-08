# gitglance

A quick TUI that shows which repos in a folder have pending work: uncommitted files, unpushed commits, or a branch that is behind. Each one can get a short AI summary.

It stays live: leave it open next to the sessions that are working in your repos, and it picks up their changes on its own, so a glance at the window tells you what still needs a commit, a push or a pull (see [Live dashboard](#live-dashboard)).

```bash
cd ~/code && gitglance          # or: gitglance ~/code --depth 2 --summarize
```

It uses the `git` CLI (`status --porcelain=v2`, with `--no-optional-locks`), so `.gitignore`, global excludes and your git config are respected, and it never competes for the index lock with agents that commit in the background. Repos are scanned in parallel.

## Install

You need [Rust](https://rustup.rs) (via `rustup`) and `git`. The repo pins its Rust version in `rust-toolchain.toml`, and rustup fetches that version automatically on the first build.

```bash
# 1. Install Rust, if you don't have it
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 2. Install gitglance straight from GitHub
cargo install --git https://github.com/theill/gitglance
```

This puts the `gitglance` binary in `~/.cargo/bin`. Make sure that folder is on your `PATH`; the rustup installer normally sets this up, but you may need to open a new terminal first.

From a local checkout:

```bash
git clone https://github.com/theill/gitglance && cd gitglance
cargo install --path .
```

Run the same command again to update. `cargo uninstall gitglance` removes it.

AI summaries and drafted commit messages are optional. They need the [Claude Code](https://claude.com/claude-code) CLI (`claude`) on your `PATH`, or another command set through `GITGLANCE_AI_CMD` (see [AI summaries](#ai-summaries)). Everything else works without it.

## Keys

| List | |
|---|---|
| `↑↓` `j/k` | move |
| `⏎` | details (summary, unpushed commits, changed files with +/-) |
| `s` / `S` | AI summary for this repo / for every listed repo without one |
| `c` | commit everything and push (see below) |
| `P` | push commits that are already made (asks first) |
| `u` | pull commits you're behind on (see below) |
| `d` | everything pending: uncommitted diff, unpushed commits with patches, incoming commits |
| `a` | toggle repos with changes ↔ all repos |
| `/` | filter by name |
| `x` | ignore this repo (adds it to `.gitglanceignore`); press `x` on an ignored repo to un-ignore it |
| `I` | show or hide ignored repos |
| `r` / `f` | rescan / `git fetch --all` everywhere, then rescan |
| `w` | pause or resume the live refresh |
| `t` | open a shell in the repo (your `$SHELL`); `exit` brings you back |
| `o` | open the repo folder in your file manager (Finder on macOS, `xdg-open` on Linux) |

In the detail view, `↑↓` highlights an unpushed commit or a changed file and `⏎` opens it: a commit as its full `git show`, a file as its diff (or its contents, if it's new). Esc goes back to the same spot. `n`/`p` jumps to the next or previous repo.

A `●` in front of a repo means a background re-check just found it changed (the last minute), and a dim `•` means it changed in the last ten.

Columns: **STATE** `S` staged, `M` modified, `?` untracked, `U` conflicts, `≡` stashes. **SYNC** `↑` ahead, `↓` behind (as of the last fetch), `unpushed` for a branch with no upstream, `local` for no remote.

## Live dashboard

gitglance re-checks every repo every 5 seconds, and runs `git fetch --all` on all of them every 5 minutes (the first fetch starts right after the opening scan). Changes made elsewhere, by you in an editor or by an agent in another session, show up without a key press:

- New, changed or cleaned-up repos appear and disappear from the list, and repos cloned into the folder show up too.
- Repos that changed get a `●` marker; the detail view says how long ago.
- `↓N` appears once a fetch sees new commits upstream, so you know to pull.
- The terminal window title shows the count, like `3 pending · ↓1 · gitglance ~/code`, so you can see it from a tab or taskbar. The title is put back when gitglance quits.

Background re-checks are quiet: no spinners, and they never call the AI. With `--summarize`, only the scans you start (on launch, `r`, `f`) summarize automatically.

```bash
gitglance ~/code --interval 10 --fetch-every 15   # gentler
gitglance ~/code --fetch-every 0                  # never fetch on its own
gitglance ~/code --interval 0                     # start paused; w resumes
```

## Commit and push

`c` opens a commit box for the selected repo. Claude drafts the message from the diff, matching the style of the repo's last 12 commits (gitmoji, language, casing). You can type over the draft, start typing before it arrives (the draft won't overwrite you), or press ctrl+g for a new one.

- `⏎` runs `git add -A` (`.gitignore` still applies), commits and pushes. Hooks run as normal.
- New secret-looking files that aren't gitignored (`.env*`, keys, `*.p8`, anything named `credentials` or `secret`) are left out of the commit, and the box lists them. If a file like that is *already tracked*, the box shows a red warning, because its changes would be committed.
- `tab` turns the push off and on.
- `alt+⏎` adds a newline.
- A branch with no upstream is pushed to `origin` with `--set-upstream`. A repo with no remote is only committed.

`P` pushes commits that are already made, after a y/n confirmation. Git never prompts for credentials here, so a push that needs a password fails with an error instead of hanging.

## Pulling (`u`)

`↓N` in SYNC means the upstream has N commits you don't have yet, as of the last fetch (`f` fetches every repo).

- **Only behind:** `u` fast-forwards right away (`git pull --ff-only --autostash`), with no question asked, since nothing can be lost.
- **Diverged** (`↑2 ↓1`): `u` asks, then rebases your local commits on top (`git pull --rebase --autostash`). Push afterwards with `P`. If the rebase hits a conflict, it's aborted automatically and nothing changes; the message tells you the command to run by hand.

Uncommitted changes are stashed for the pull and restored afterwards.

## Ignoring repos

Ignored repos are listed in `.gitglanceignore` in the scanned folder (e.g. `~/code/.gitglanceignore`), one per line, as their path relative to that folder. `x` writes to it for you, and you can also edit it by hand (press `r` to reload):

```
apikiss
unops/opportunityplus   # comments are fine
```

A line hides exactly that repo. It never hides repos nested below it, or another repo with the same name in a different folder, and there are no globs. New repos always show up until you choose to ignore them.

Ignored repos aren't scanned, so they also make the scan faster.

## AI summaries

These run `claude -p` with Haiku and no tools, in a neutral directory. The prompt gets the file list, unpushed commits, the diff (lockfiles left out, capped at 60 KB) and the start of new files. Results are cached in `~/.cache/gitglance`, keyed by that content, so a repo that hasn't changed never costs a second call.

Code only leaves your machine when you ask for it: `s`, `S`, `c` or `--summarize`. Commit messages are drafted in the background only for repos you've already summarized. Secret-looking files (`.env*`, `*.pem`, `*.key`, `*.p8`, `*.p12`, SSH keys, anything named `credentials` or `secret`) are listed by name, but their contents are never sent.

- `GITGLANCE_MODEL=sonnet` picks another model.
- `GITGLANCE_AI_CMD='llm -m gpt-4o-mini'` uses any command that reads the prompt on stdin.

`rust-toolchain.toml` pins 1.98.1, because the machine's `stable` toolchain is an old 1.77.
