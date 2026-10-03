# gitglance

A quick TUI that shows which repos in a folder have pending work: uncommitted files, unpushed commits, or a branch that is behind. Each one can get a short AI summary.

```bash
cd ~/code && gitglance          # or: gitglance ~/code --depth 2 --summarize
```

It uses the `git` CLI (`status --porcelain=v2`, with `--no-optional-locks`), so `.gitignore`, global excludes and your git config are respected, and it never competes for the index lock with agents that commit in the background. Repos are scanned in parallel.

## Keys

| List | |
|---|---|
| `↑↓` `j/k` | move |
| `⏎` | details (summary, unpushed commits, changed files with +/-) |
| `s` / `S` | AI summary for this repo / for every listed repo without one |
| `c` | commit everything and push (see below) |
| `P` | push commits that are already made (asks first) |
| `d` | full diff |
| `a` | toggle repos with changes ↔ all repos |
| `/` | filter by name |
| `x` | ignore this repo (adds it to `.gitglanceignore`); press again on an ignored repo to un-ignore |
| `I` | show or hide ignored repos |
| `r` / `f` | rescan / `git fetch --all` everywhere, then rescan |
| `o` | open in Finder |

In the detail view, `n`/`p` jumps to the next or previous repo.

Columns: **STATE** `S` staged, `M` modified, `?` untracked, `U` conflicts, `≡` stashes. **SYNC** `↑` ahead, `↓` behind (as of the last fetch), `unpushed` for a branch with no upstream, `local` for no remote.

## Commit and push

`c` opens a commit box for the selected repo. Claude drafts the message from the diff, matching the style of the repo's last 12 commits (gitmoji, language, casing). You can type over the draft, start typing before it arrives (the draft won't overwrite you), or press ctrl+g for a new one.

- `⏎` runs `git add -A` (`.gitignore` still applies), commits and pushes. Hooks run as normal.
- `tab` turns the push off and on.
- `alt+⏎` adds a newline.
- A branch with no upstream is pushed to `origin` with `--set-upstream`. A repo with no remote is only committed.

`P` pushes commits that are already made, after a y/n confirmation. Git never prompts for credentials here, so a push that needs a password fails with an error instead of hanging.

## Ignoring folders

Ignored folders are listed in `.gitglanceignore` in the scanned folder (e.g. `~/code/.gitglanceignore`), one per line, gitignore style. `x` writes to it for you, and you can also edit it by hand (press `r` to reload):

```
apikiss          # a name without / matches that folder at any depth
*-site           # globs: * and ?
unops            # ignoring a folder ignores every repo below it
unops/*          # a path with / is relative to the scanned folder
```

Ignored repos aren't scanned, so they also make the scan faster.

## AI summaries

These run `claude -p` with Haiku and no tools, in a neutral directory. The prompt gets the file list, unpushed commits, the diff (lockfiles left out, capped at 60 KB) and the start of new files. Results are cached in `~/.cache/gitglance`, keyed by that content, so a repo that hasn't changed never costs a second call.

- `GITGLANCE_MODEL=sonnet` picks another model.
- `GITGLANCE_AI_CMD='llm -m gpt-4o-mini'` uses any command that reads the prompt on stdin.

## Build

```bash
cargo install --path .
```

`rust-toolchain.toml` pins 1.98.1, because the machine's `stable` toolchain is an old 1.77.
