# nth

`nth` adds numeric shortcuts to existing command output. It is inspired by
[SCM Breeze](https://github.com/ndbroadbent/scm_breeze) and
[scmpuff](https://github.com/mroth/scmpuff), but its central rule is different:
the user's configured command remains the producer. `nth` does not implement a
replacement for `ls`, `eza`, `lsd`, or any other command.

[![asciicast](https://asciinema.org/a/TtlsOG7FkOlVBmua.svg)](https://asciinema.org/a/TtlsOG7FkOlVBmua)

With Bash integration loaded after your aliases:

```bash
alias ls='lsd --icon never --oneline'
alias cat='cat -n'
eval "$(nth init bash)"
```

Normal commands keep their names and configured behavior:

```console
$ ls
[1] README.md
[2] build
[3] notes from review.txt
[4] src

$ cd 2
$ cat 3
     1  review notes
$ cp 1,3 backup/
$ rm 2-3
```

Git uses the same command names. `git status` is rendered as a compact grouped
view with each shortcut next to its path:

```console
$ git status
On branch main

➤ Changes to be committed

     modified:  [1] src/main.rs

➤ Changes not staged for commit

     modified:  [2] src/main.rs

➤ Untracked files

    untracked:  [3] notes.txt

$ git add 1
$ git diff 2
```

Object lists use the same shortcuts. The default branch, log, stash, and
worktree listings are numbered, and the next Git command can select the shown
object directly:

```console
$ git branch
[1] * main
[2]   topic

$ git switch 2
$ git log
[1] d9c42d1 (HEAD -> topic) finish parser
[2] 7610adb add parser skeleton

$ git show 2
$ git stash list
[1] stash@{0}: On topic: work in progress

$ git stash pop 1
$ git worktree list
[1] /work/project d9c42d1 [topic]
[2] /work/review  7610adb [review]

$ git worktree remove 2
```

## How observation works

The Bash and Fish integrations snapshot simple aliases before installing
same-name functions.
When `ls` runs, `nth` starts the original producer in a pseudo-terminal, so its
TTY detection, colors, icons, layout, and sorting continue to work. It then
parses structured metadata from the producer's real output, inserts number
labels, and saves the extracted arguments for that shell session.

The built-in adapters use OSC 8 file hyperlinks because their URI contains the
actual path independently of the visible filename. Current adapters are:

- `eza` and `exa`;
- `lsd`;
- GNU `ls` (`ls` on Linux or `gls` elsewhere);
- `osc8` for an explicitly configured producer that already emits file links.

Unsupported producers are passed through unchanged and the previous shortcut
table is cleared. `nth` never guesses a path by stripping colors, icons, table
columns, or other human-readable formatting. macOS BSD `ls` does not expose the
required structured metadata; use `eza`, `lsd`, or Homebrew GNU `gls` when
reliable shortcuts are required.

Existing shell functions are also left unchanged because serializing and
replaying an arbitrary function in a child pseudo-terminal would change its
semantics. The initializer prints a warning for each such function.

## Install

Rust 1.85 or newer is required.

```sh
cargo install --path .
```

For transparent Bash integration, place this after your alias definitions:

```bash
eval "$(nth init bash)"
```

For Fish:

```fish
alias ls 'lsd --icon never --oneline'
alias cat 'cat -n'
nth init fish | source
```

The first release supports Bash and Fish. Simple command-prefix aliases such as
`alias ls='lsd --icon never --oneline'` are preserved. Existing shell functions
and aliases containing pipelines, redirections, or other shell syntax are
outside the first-release wrapper contract and remain unchanged.

## First-release support contract

`nth` is currently optimized for a small, explicit personal setup rather than
best-effort compatibility with every `ls` implementation.

- Shell: Bash 3.2 or newer, or Fish 3.6 or newer.
- Producer: `eza` or `lsd`, configured as a simple `ls` alias before loading
  the shell integration.
- Identity format: every listed entry must carry an OSC 8 `file://` hyperlink;
  `nth` enables the producer's hyperlink option automatically.
- Icons are optional and are not used for identity. Set `--icon never` in an
  `lsd` alias if icon output is unwanted.
- Layout: one entry per line. Add `--oneline` to the alias; grid, tree,
  recursive, and multi-directory layouts are not part of the supported output
  contract yet.
- Consumers: `cd`, `cp`, `mv`, `rm`, `cat`, `less`, `stat`, `diff`, `chmod`,
  `chown`, `ln`, `file`, `wc`, and `du`, including simple command-prefix
  aliases. `cd` is resolved in the current shell so the directory change
  persists.
- Git producer: exactly `git status` with no status options. Git's stable
  `--porcelain=v2 -z --branch` output remains the source of truth; `nth` groups
  those records into staged, unstaged, unmerged, and untracked sections. It
  does not reproduce Git's status calculation or parse human-readable output.
- Git object producers: exactly `git branch`, `git log`, `git stash list`, and
  `git worktree list`, with no additional options. They use structured ref,
  log, reflog, and worktree output respectively, then render a compact numbered
  view. Branch shortcuts store branch names, commit shortcuts store full object
  IDs, stash shortcuts store `stash@{n}` selectors, and worktree shortcuts
  store paths.
- Git consumers: `git add`, `git branch`, `git checkout`, `git cherry-pick`,
  `git diff`, `git log`, `git merge`, `git rebase`, `git reset`, `git restore`,
  `git revert`, `git rm`, `git show`, `git stash`, `git switch`, `git tag`, and
  `git worktree`. Other Git subcommands pass through unchanged.
- Git paths: a path present in both staged and unstaged sections receives a
  separate shortcut in each section, matching what is visible. A rename shows
  both names and stores its destination path. Stored paths are absolute, so
  they remain valid for the command that immediately consumes them.
- Load order: aliases first, then `nth init`. Existing functions and aliases
  containing shell operators are intentionally not rewritten.
- Numeric literals: use the `=` escape when a number is an argument rather
  than a selector, for example `chmod =755 1`.

Plain BSD `ls` and human-readable output parsing are explicitly outside the
first-release guarantee. Some unsupported producers may already work as
best-effort adapters, but they are not part of the tested compatibility
surface. Unsupported producers remain responsible for their original output;
`nth` does not replace them with its own filesystem implementation.

Colors are enabled when stdout is a terminal, disabled by `NO_COLOR`, and can
be forced with `CLICOLOR_FORCE=1`. `git status` with options, such as
`git status --short`, passes through without aggregation and does not change the
previous shortcut table. Git object listings with additional options likewise
retain Git's normal behavior and are not numbered.

A shortcut table is an immutable snapshot of the last successfully displayed
numbered list. Consumer commands such as `git add 4` and `git reset 4` only read
that snapshot, so both references continue to identify the same path even when
the first command changes repository state. In an interactive shell, Git lists
whose output is redirected or captured are treated as background queries and
do not replace the snapshot; this prevents Git-aware prompts from silently
renumbering it. A later visible numbered list replaces the snapshot atomically,
and `nth clear` discards it explicitly.

## Core commands

The shell integration uses `observe` internally:

```sh
nth observe -- lsd --icon never --oneline -a
nth observe --adapter eza -- eza --icons=always
nth observe --adapter osc8 -- custom-producer
```

The Git list adapters can also be called directly:

```sh
nth git-list branch -- git
nth git-list commit -- git
nth git-list stash -- git
nth git-list worktree -- git
```

`capture` is an explicit parser for newline- or NUL-delimited records. It is
useful for commands that already emit paths directly:

```sh
rg --files | nth capture --absolute
find . -type f -print0 | nth capture -0 --absolute
```

`exec` expands a stored selector and invokes the target program directly:

```sh
nth exec -- printf '%s\n' 1,3-5
```

`resolve` is the NUL-delimited expansion interface used by shell functions
such as `cd`, which must run in the current shell:

```sh
nth resolve -- 1
```

Selectors may be a number (`3`), an inclusive range (`2-5`), a comma-separated
combination (`1,4-6`), or the explicit form (`@3`). Descending ranges such as
`5-2` also work. Prefix a literal numeric argument with `=`, for example
`chmod =755 1-3`.

## Safety and performance

- original commands produce all observed output;
- a PTY preserves terminal-sensitive rendering;
- only structured parser results become shortcuts;
- state is isolated by shell session and written atomically;
- paths containing whitespace, newlines, glob characters, or non-UTF-8 bytes
  are stored without loss;
- expansion creates an argument vector and never evaluates a shell command.

Run verification with:

```sh
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
cargo build --release
```

## License

MIT
