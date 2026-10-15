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

To update, run `gitglance update`. It reinstalls the latest version from GitHub with the same `cargo install --git` command, and does nothing if you already have it. (From a checkout, `git pull && cargo install --path .` works too.) `cargo uninstall gitglance` removes it.

AI summaries and drafted commit messages are optional. They need the [Claude Code](https://claude.com/claude-code) CLI (`claude`) on your `PATH`, or another command set through `GITGLANCE_AI_CMD` (see [AI summaries](#ai-summaries)). Everything else works without it.

## Keys

| List | |
|---|---|
| `?` | every key on one screen (the footer only shows the everyday ones) |
| `↑↓` `j/k` | move |
| `⏎` | details: AI summary, unpushed commits, changed files with +/-, and commit activity (see [Repo details](#repo-details)) |
| `s` / `S` | AI summary for this repo / for every listed repo without one |
| `c` | commit everything and push (see below) |
| `P` | push commits that are already made (asks first) |
| `u` | pull commits you're behind on (see below) |
| `U` (Shift+U) | sync every non-ignored repo: fetch, then pull updates (fast-forward only) or push local commits |
| `d` | everything pending: uncommitted diff, unpushed commits with patches, incoming commits |
| `a` | toggle repos with changes ↔ all repos |
| `tab` | the repos with recent commits and what they are, live (see [Recent commits](#recent-commits)) |
| `/` | filter by name |
| `x` | ignore this repo (adds it to `.gitglanceignore`); press `x` on an ignored repo to un-ignore it |
| `I` | show or hide ignored repos |
| `r` / `f` | rescan / `git fetch --all` everywhere, then rescan |
| `w` | pause or resume the live refresh |
| `t` | open a shell in the repo (your `$SHELL`); `exit` brings you back |
| `A` / `R` | start a Claude Code session in the repo / resume the last one there, in a new terminal window (see [Agent sessions](#agent-sessions)) |
| `o` | open the repo folder in your file manager (Finder on macOS, `xdg-open` on Linux) |

In the detail view, commits you are behind on are listed under **Incoming** (hash, age and subject, up to 50; `d` shows them all). `↑↓` highlights an incoming or unpushed commit or a changed file and `⏎` opens it: a commit as its full `git show`, a file as its diff (or its contents, if it's new). Esc goes back to the same spot. `n`/`p` jumps to the next or previous repo.

A `●` in front of a repo means a background re-check just found it changed (the last minute), and a dim `•` means it changed in the last ten.

Columns: **STATE** `S` staged, `M` modified, `?` untracked, `U` conflicts, `≡` stashes. **SYNC** `↑` ahead, `↓` behind (as of the last fetch), `unpushed` for a branch with no upstream, `local` for no remote.

## Repo details

`⏎` opens a repo. Next to its pending work (or below it, in a terminal narrower than 120 columns) is an **Activity** panel:

- total commits and how long ago the repo started; commits today, this week and last week
- a bar graph of commits per day over the last 6 weeks, and per week over the last 24 weeks
- who made them over those 24 weeks, with their commits and lines added and removed

The numbers cover all branches and leave out merges. Authors are grouped by `.mailmap` names. The panel loads in the background and updates when new commits land while you look at it.

If the repo has changes and no summary yet, the AI summary starts on its own once the repo has been open for half a second. Paging past repos with `n`/`p`, or backing out right away, sends nothing. `--no-auto-summary` turns this off, so only `s` asks.

## Recent commits

`tab` switches from the repo list to a board of the repos that got commits recently, and what those commits are. It's for keeping an eye on agents and teammates committing across several repos: leave it open, and new commits show up within seconds, with a `●` on the commit and its repo (bright for the first minute, dim `•` for ten).

```
 Last 24 hours   1h · today · [24h] · 7d      2 repos · 6 commits

 ▌ ● shop/api   5 commits · 2m–8h · Peter, Ana                          +175  −35
       ●  2m  worktree-auth  Add token refresh to the middleware     3 files   +88  −12
          1h  Fix the rate limit on login                     Ana    2 files   +40  −10
          8h  Log the request id with every error             Ana    2 files   +35  −11
              … 2 more  (⏎ shows them)

     shop/web   1 commit · 3h · Ana                                       +12   −4
          3h  Fix the cart total rounding                     Ana     1 file   +12   −4
```

- Each repo with commits in the period gets a line saying how many, over how long, by whom and how big they are in total. The repo with the newest commit comes first, so a repo an agent just committed to moves to the top.
- Under it are its 3 newest commits, with their size (files and lines added and removed). `⏎` on the repo, or on "… more", shows all of them; `⏎` on the repo again folds them.
- `[` and `]` (or `1` to `4`) pick the period: the last hour, today, the last 24 hours (the default) or the last 7 days.
- `⏎` on a commit shows it as `git show`: message, files changed and the full patch. `n`/`p` there steps to the next or previous commit on the board, and Esc goes back.
- Commits come from local and remote branches, with merges left out, so commits other people push show up after the next fetch (every 5 minutes, or `f`). A commit is prefixed with its branch only when it isn't on the repo's checked-out branch: an agent's worktree branch, a feature branch, or `origin/main` for commits you haven't pulled yet. A commit shared by a repo and its linked worktrees is listed once.
- Commits you haven't pushed yet are included and marked `↑` (cyan), and the repo line says how many, like `↑2 unpushed`. Repos with no remote aren't marked, since there is nowhere to push them.
- Authors are named only on commits that aren't yours (your git `user.name`), so teammates stand out. Narrow terminals leave out the files count, then the author.
- `/` filters by repo, author, branch or message (`/ana`, `/api`). `t`, `A`, `R` and `o` act on the selected row's repo. `tab` or Esc goes back to the repo list.

## Live dashboard

gitglance re-checks every repo every 5 seconds, and runs `git fetch --all` on all of them every 5 minutes (the first fetch starts right after the opening scan). Changes made elsewhere, by you in an editor or by an agent in another session, show up without a key press:

- New, changed or cleaned-up repos appear and disappear from the list, and repos cloned into the folder show up too.
- Repos that changed get a `●` marker; the detail view says how long ago.
- `↓N` appears once a fetch sees new commits upstream, so you know to pull.
- The terminal window title shows the count, like `3 pending · ↓1 · gitglance ~/code`, so you can see it from a tab or taskbar. The title is put back when gitglance quits.

Background re-checks are quiet: no spinners. When one finds new changes in a repo, the AI summarizes them once they have stayed the same for 10 seconds, so an agent that is still editing doesn't cause a call per re-check, and the row says "summarizing once the changes settle…" meanwhile. A summary that comes back for changes that have moved on since is dropped, and the newer changes get their own once they settle. Summaries are cached by content, so a repo that goes back to a state it was in before costs nothing. `--no-auto-summary` turns this off. Repos that already had changes when gitglance started are summarized with `S` or `--summarize`.

```bash
gitglance ~/code --interval 10 --fetch-every 15   # gentler
gitglance ~/code --fetch-every 0                  # never fetch on its own
gitglance ~/code --interval 0                     # start paused; w resumes
```

## Agent sessions

`A` starts `claude` in the selected repo's folder, so the session works on that repo with its `CLAUDE.md`/`AGENTS.md`. `R` resumes the last conversation there (`claude --continue`). Nothing from gitglance is passed in: the session starts empty, as if you had opened it yourself.

It opens in a new terminal window, so gitglance stays live next to it and shows the agent's changes as they happen. The session keeps running after gitglance quits.

- Linux: `xdg-terminal-exec` (Omarchy's default terminal launcher), otherwise `$TERMINAL -e`.
- macOS: a new Terminal.app window.
- No desktop (over SSH, say): it runs in gitglance's own terminal, like `t`, and quitting it brings you back.

`GITGLANCE_AGENT_CMD` starts another command instead (`GITGLANCE_AGENT_CMD='claude --model opus'`); `R` adds `--continue` to it.

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

## Syncing every repo (`U`)

Press `U` (Shift+U) in the list or detail view to sync every non-ignored repository in the scanned folder, including repos hidden by a filter or by the changes-only view. It discovers new repos first and waits for current scans and Git operations to finish.

The batch runs in the background, with progress in the header. Each repo fetches all remotes, then fast-forwards its current branch if it is behind its upstream, or pushes it if it is ahead. Only commits you've already made are pushed; uncommitted changes stay uncommitted. Branches without an upstream aren't published. Your live AI-summary settings still apply when repo status changes.

Repos with diverged branches, conflicts, an unfinished Git operation, a missing upstream, or local edits that would need to be stashed before pulling are left alone. Use `u` for a diverged branch, or commit/stash edits, then sync again. Repos with no remote, no commits, or detached HEAD are quietly skipped. A failure in one repo doesn't stop the rest.

When the batch finishes you stay where you were, and the footer sums it up (e.g. `Synced 14 repos: 2 pulled (5 commits), 1 pushed (1 commit), 1 needs a look (marked !)`). Pulled and pushed repos simply refresh in the list. A repo that failed or couldn't be synced, including a rejected push, gets a red `!` in SYNC and the reason in SUMMARY, and stays listed even if it has no pending changes. The detail page shows the reason too. The marker clears on the next sync or after a successful commit, push or pull in that repo.

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

Each summary has two levels. The list shows a headline of at most about 50 characters that names what the work is (the feature, fix or screen), never its git state, since the other columns already show that. Merge commits are left out of the prompt so they don't crowd out the real work. Opening a repo shows the full summary, with specific bullets grouped under Uncommitted, Not pushed and To pull.

Code only leaves your machine when you ask for it: `s`, `S`, `c`, `--summarize`, by opening a repo with changes for half a second, or when the live refresh finds new changes that then stay the same for 10 seconds (turn both off with `--no-auto-summary`). Commit messages are drafted in the background only for repos you've already summarized. Secret-looking files (`.env*`, `*.pem`, `*.key`, `*.p8`, `*.p12`, SSH keys, anything named `credentials` or `secret`) are listed by name, but their contents are never sent.

- `GITGLANCE_MODEL=sonnet` picks another model.
- `GITGLANCE_AI_CMD='llm -m gpt-4o-mini'` uses any command that reads the prompt on stdin.

`rust-toolchain.toml` pins 1.98.1, because the machine's `stable` toolchain is an old 1.77.
