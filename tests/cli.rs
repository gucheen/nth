#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use std::os::unix::fs::PermissionsExt;

struct Sandbox {
    root: PathBuf,
}

static NEXT_SANDBOX: AtomicU64 = AtomicU64::new(0);

impl Sandbox {
    fn new() -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT_SANDBOX.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "nth-test-{}-{unique}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_nth"));
        command
            .env("NTH_STATE_DIR", self.root.join("state"))
            .env("NTH_SESSION", "integration-test");
        command
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn git(directory: &std::path::Path, args: &[&str]) {
    let output = Command::new("git")
        .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgSign=false"])
        .args(args)
        .current_dir(directory)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?}: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn commit(directory: &std::path::Path, message: &str) {
    git(directory, &["add", "."]);
    git(
        directory,
        &[
            "-c",
            "user.name=nth test",
            "-c",
            "user.email=nth@example.invalid",
            "commit",
            "-qm",
            message,
        ],
    );
}

fn nth_git_list(sandbox: &Sandbox, repository: &std::path::Path, kind: &str) -> String {
    let output = sandbox
        .command()
        .args(["git-list", kind, "--", "git", "-C"])
        .arg(repository)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn observe_extracts_paths_from_the_original_osc8_output() {
    let sandbox = Sandbox::new();
    let output = sandbox
        .command()
        .args([
            "observe",
            "--adapter",
            "osc8",
            "--",
            "/usr/bin/printf",
            "\x1b]8;;file:///tmp/a%%20b\x07a b\x1b]8;;\x07\n",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "status={:?}, stdout={:?}, stderr={}",
        output.status,
        output.stdout,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.starts_with(b"[1] \x1b]8;;file:///tmp/a%20b"));

    let state = sandbox.command().arg("show").output().unwrap();
    assert!(state.status.success());
    assert_eq!(state.stdout, b"[1] /tmp/a b\n");
}

#[test]
fn git_status_groups_porcelain_records_with_visible_shortcuts() {
    let sandbox = Sandbox::new();
    let repository = sandbox.root.join("repo");
    fs::create_dir(&repository).unwrap();
    git(&repository, &["init", "-q"]);
    fs::write(repository.join("tracked.txt"), "initial\n").unwrap();
    git(&repository, &["add", "tracked.txt"]);
    git(
        &repository,
        &[
            "-c",
            "user.name=nth test",
            "-c",
            "user.email=nth@example.invalid",
            "commit",
            "-qm",
            "initial",
        ],
    );
    fs::write(repository.join("tracked.txt"), "staged\n").unwrap();
    git(&repository, &["add", "tracked.txt"]);
    fs::write(repository.join("tracked.txt"), "unstaged\n").unwrap();
    fs::write(repository.join("new file.txt"), "new\n").unwrap();

    let output = sandbox
        .command()
        .args(["git-status", "--", "git"])
        .current_dir(&repository)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("➤ Changes to be committed"));
    assert!(stdout.contains("➤ Changes not staged for commit"));
    assert!(stdout.contains("➤ Untracked files"));
    assert!(stdout.contains("[1] tracked.txt"));
    assert!(stdout.contains("[2] tracked.txt"));
    assert!(stdout.contains("[3] new file.txt"));

    let state = sandbox.command().arg("show").output().unwrap();
    let state = String::from_utf8(state.stdout).unwrap();
    assert_eq!(state.matches("tracked.txt").count(), 2);
    assert!(state.contains(&repository.join("tracked.txt").display().to_string()));
    assert!(state.contains(&repository.join("new file.txt").display().to_string()));
}

#[test]
fn git_lists_number_branches_commits_stashes_and_worktrees() {
    let sandbox = Sandbox::new();
    let repository = sandbox.root.join("repo");
    let linked = sandbox.root.join("linked");
    fs::create_dir(&repository).unwrap();
    git(&repository, &["init", "-q", "-b", "main"]);
    fs::write(repository.join("tracked.txt"), "initial\n").unwrap();
    commit(&repository, "initial");
    fs::write(repository.join("tracked.txt"), "second\n").unwrap();
    commit(&repository, "second");
    git(&repository, &["branch", "topic"]);
    git(&repository, &["branch", "linked"]);

    let branches = nth_git_list(&sandbox, &repository, "branch");
    assert!(branches.contains("[1]   linked"));
    assert!(branches.contains("[2] * main"));
    assert!(branches.contains("[3]   topic"));
    let selected = sandbox
        .command()
        .args(["exec", "--", "/usr/bin/printf", "%s", "3"])
        .output()
        .unwrap();
    assert!(selected.status.success());
    assert_eq!(selected.stdout, b"topic");

    let commits = nth_git_list(&sandbox, &repository, "commit");
    assert!(commits.contains("[1]"));
    assert!(commits.contains("second"));
    assert!(commits.contains("[2]"));
    assert!(commits.contains("initial"));
    let selected = sandbox
        .command()
        .args(["exec", "--", "git", "-C"])
        .arg(&repository)
        .args(["show", "--format=%s", "--no-patch", "2"])
        .output()
        .unwrap();
    assert!(selected.status.success());
    assert_eq!(selected.stdout, b"initial\n");

    fs::write(repository.join("tracked.txt"), "stashed\n").unwrap();
    git(&repository, &["stash", "push", "-qm", "saved change"]);
    let stashes = nth_git_list(&sandbox, &repository, "stash");
    assert!(stashes.contains("[1] stash@{0}: On main: saved change"));
    let selected = sandbox
        .command()
        .args(["exec", "--", "/usr/bin/printf", "%s", "1"])
        .output()
        .unwrap();
    assert!(selected.status.success());
    assert_eq!(selected.stdout, b"stash@{0}");

    git(
        &repository,
        &["worktree", "add", "-q", linked.to_str().unwrap(), "linked"],
    );
    let worktrees = nth_git_list(&sandbox, &repository, "worktree");
    let repository_path = fs::canonicalize(&repository).unwrap();
    let linked_path = fs::canonicalize(&linked).unwrap();
    assert!(worktrees.contains(&format!("[1] {}", repository_path.display())));
    assert!(worktrees.contains(&format!("[2] {}", linked_path.display())));
    let selected = sandbox
        .command()
        .args(["exec", "--", "/usr/bin/printf", "%s", "2"])
        .output()
        .unwrap();
    assert!(selected.status.success());
    assert_eq!(selected.stdout, linked_path.as_os_str().as_encoded_bytes());
}

fn shell_git_status_then_add(shell: &str, shell_args: &[&str], init: &str) {
    let sandbox = Sandbox::new();
    let repository = sandbox.root.join("repo");
    fs::create_dir(&repository).unwrap();
    git(&repository, &["init", "-q"]);
    fs::write(repository.join("new file.txt"), "new\n").unwrap();

    let binary = PathBuf::from(env!("CARGO_BIN_EXE_nth"));
    let script = init.replace("{nth}", &binary.display().to_string());
    let output = Command::new(shell)
        .args(shell_args)
        .arg(script)
        .current_dir(&repository)
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("NTH_STATE_DIR", sandbox.root.join("state"))
        .env("NTH_SESSION", format!("{shell}-git-test"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("[1] new file.txt"));

    let staged = Command::new("git")
        .args(["diff", "--cached", "--name-only"])
        .current_dir(&repository)
        .output()
        .unwrap();
    assert_eq!(staged.stdout, b"new file.txt\n");
}

#[test]
fn bash_git_wrapper_expands_status_shortcuts() {
    shell_git_status_then_add(
        "bash",
        &["--noprofile", "--norc", "-c"],
        "eval \"$('{nth}' init bash)\"; git status; git add 1",
    );
}

#[test]
fn fish_git_wrapper_expands_status_shortcuts() {
    if Command::new("fish").arg("--version").output().is_err() {
        return;
    }
    shell_git_status_then_add(
        "fish",
        &["--no-config", "-c"],
        "'{nth}' init fish | source; git status; git add 1",
    );
}

#[test]
fn bash_git_wrapper_selects_numbered_git_objects() {
    let sandbox = Sandbox::new();
    let repository = sandbox.root.join("repo");
    let linked = sandbox.root.join("linked");
    fs::create_dir(&repository).unwrap();
    git(&repository, &["init", "-q", "-b", "main"]);
    fs::write(repository.join("tracked.txt"), "initial\n").unwrap();
    commit(&repository, "initial");
    fs::write(repository.join("tracked.txt"), "second\n").unwrap();
    commit(&repository, "second");
    git(&repository, &["branch", "topic"]);
    git(&repository, &["branch", "linked"]);
    fs::write(repository.join("tracked.txt"), "stashed\n").unwrap();
    git(&repository, &["stash", "push", "-qm", "saved change"]);
    git(
        &repository,
        &["worktree", "add", "-q", linked.to_str().unwrap(), "linked"],
    );

    let binary = PathBuf::from(env!("CARGO_BIN_EXE_nth"));
    let script = format!(
        "eval \"$('{}' init bash)\"; git branch; git switch 3; git log; test \"$(git show --format=%s --no-patch 2)\" = initial; git stash list; git stash apply 1; git worktree list; git worktree lock 2",
        binary.display()
    );
    let output = Command::new("bash")
        .args(["--noprofile", "--norc", "-c", &script])
        .current_dir(&repository)
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("NTH_STATE_DIR", sandbox.root.join("state"))
        .env("NTH_SESSION", "bash-git-objects-test")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("[3]   topic"));
    assert_eq!(
        fs::read(repository.join("tracked.txt")).unwrap(),
        b"stashed\n"
    );

    let branch = Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(&repository)
        .output()
        .unwrap();
    assert_eq!(branch.stdout, b"topic\n");
    let worktrees = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(&repository)
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&worktrees.stdout).contains("locked"));
}

#[test]
fn git_file_numbers_survive_consumers_and_hidden_status_queries() {
    let sandbox = Sandbox::new();
    let repository = sandbox.root.join("repo");
    fs::create_dir(&repository).unwrap();
    git(&repository, &["init", "-q", "-b", "main"]);
    for name in ["a.txt", "b.txt", "c.txt", "d.txt"] {
        fs::write(repository.join(name), format!("{name}\n")).unwrap();
    }

    let binary = PathBuf::from(env!("CARGO_BIN_EXE_nth"));
    let script = format!(
        "eval \"$('{}' init bash)\"; command '{}' git-status -- git >/dev/null; git add 4; git status >/dev/null; git status --porcelain >/dev/null; git reset 4",
        binary.display(),
        binary.display()
    );
    let output = Command::new("bash")
        .args(["--noprofile", "--norc", "-ic", &script])
        .current_dir(&repository)
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("NTH_STATE_DIR", sandbox.root.join("state"))
        .env("NTH_SESSION", "integration-test")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let staged = Command::new("git")
        .args(["diff", "--cached", "--name-only"])
        .current_dir(&repository)
        .output()
        .unwrap();
    assert!(staged.status.success());
    assert!(staged.stdout.is_empty());

    let state = sandbox.command().arg("show").output().unwrap();
    assert!(state.status.success());
    assert!(String::from_utf8_lossy(&state.stdout).contains("[4]"));
    assert!(String::from_utf8_lossy(&state.stdout).contains("d.txt"));
}

#[test]
fn failed_numbered_producer_keeps_the_previous_snapshot() {
    let sandbox = Sandbox::new();
    let mut capture = sandbox
        .command()
        .arg("capture")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    capture
        .stdin
        .take()
        .unwrap()
        .write_all(b"keep-me\n")
        .unwrap();
    assert!(capture.wait().unwrap().success());

    let failed = sandbox
        .command()
        .args(["git-list", "branch", "--", "/usr/bin/false"])
        .output()
        .unwrap();
    assert!(!failed.status.success());

    let state = sandbox.command().arg("show").output().unwrap();
    assert!(state.status.success());
    assert_eq!(state.stdout, b"[1] keep-me\n");
}

#[test]
fn bash_keeps_the_original_producer_and_consumer_aliases() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }
    let sandbox = Sandbox::new();
    let item = sandbox.root.join("item");
    fs::write(&item, "hello\n").unwrap();
    let producer = sandbox.root.join("lsd");
    fs::write(
        &producer,
        format!(
            "#!/bin/sh\nprintf '\\033]8;;file://{}\\007item\\033]8;;\\007\\n'\n",
            item.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&producer, fs::Permissions::from_mode(0o755)).unwrap();

    let binary = PathBuf::from(env!("CARGO_BIN_EXE_nth"));
    let script = format!(
        "shopt -s expand_aliases; alias ls='{}'; alias cat='cat -n'; eval \"$('{}' init bash)\"; ls; cat 1",
        producer.display(),
        binary.display()
    );
    let output = Command::new("bash")
        .args(["--noprofile", "--norc", "-c", &script])
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("NTH_STATE_DIR", sandbox.root.join("state"))
        .env("NTH_SESSION", "bash-alias-test")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[1]"));
    assert!(stdout.contains("1\thello"));
}

#[test]
fn fish_keeps_the_original_producer_and_consumer_aliases() {
    if Command::new("fish").arg("--version").output().is_err() {
        return;
    }
    let sandbox = Sandbox::new();
    let item = sandbox.root.join("item");
    fs::write(&item, "hello\n").unwrap();
    let producer = sandbox.root.join("lsd");
    fs::write(
        &producer,
        format!(
            "#!/bin/sh\nprintf '\\033]8;;file://{}\\007item\\033]8;;\\007\\n'\n",
            item.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&producer, fs::Permissions::from_mode(0o755)).unwrap();

    let binary = PathBuf::from(env!("CARGO_BIN_EXE_nth"));
    let script = format!(
        "alias ls '{}'; alias cat 'cat -n'; '{}' init fish | source; ls; cat 1",
        producer.display(),
        binary.display()
    );
    let output = Command::new("fish")
        .args(["--no-config", "-c", &script])
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("NTH_STATE_DIR", sandbox.root.join("state"))
        .env("NTH_SESSION", "fish-alias-test")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[1]"));
    assert!(stdout.contains("1\thello"));
}

#[test]
fn captured_output_then_expand_preserves_literal_file_names() {
    let sandbox = Sandbox::new();
    let files = sandbox.root.join("files");
    fs::create_dir(&files).unwrap();
    for name in ["alpha", "two words", "*.literal"] {
        fs::write(files.join(name), name).unwrap();
    }

    let mut capture = sandbox
        .command()
        .arg("capture")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let records = ["*.literal", "alpha", "two words"]
        .map(|name| files.join(name))
        .map(|path| format!("{}\n", path.display()))
        .concat();
    capture
        .stdin
        .take()
        .unwrap()
        .write_all(records.as_bytes())
        .unwrap();
    assert!(capture.wait().unwrap().success());

    let expanded = sandbox
        .command()
        .args(["exec", "--", "/usr/bin/printf", "<%s>\n", "1-3"])
        .output()
        .unwrap();
    assert!(expanded.status.success());
    let expanded = String::from_utf8(expanded.stdout).unwrap();
    assert!(expanded.contains("/*.literal>"));
    assert!(expanded.contains("/alpha>"));
    assert!(expanded.contains("/two words>"));
}

#[test]
fn nul_capture_round_trips_non_utf8_and_newline_bytes() {
    use std::os::unix::ffi::OsStringExt;

    let sandbox = Sandbox::new();
    let mut child = sandbox
        .command()
        .args(["capture", "-0"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"line\nbreak\0bad-\xff-name\0")
        .unwrap();
    assert!(child.wait().unwrap().success());

    let output = sandbox
        .command()
        .args([
            OsString::from("exec"),
            OsString::from("--"),
            OsString::from("/usr/bin/printf"),
            OsString::from("%s\n"),
            OsString::from("1-2"),
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"line\nbreak\nbad-\xff-name\n");

    assert!(OsString::from_vec(vec![0xff]).to_str().is_none());
}

#[test]
fn returns_the_child_exit_status() {
    let sandbox = Sandbox::new();
    let mut capture = sandbox
        .command()
        .arg("capture")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    capture.stdin.take().unwrap().write_all(b"one\n").unwrap();
    assert!(capture.wait().unwrap().success());

    let status = sandbox
        .command()
        .args(["exec", "--", "/bin/sh", "-c", "exit 7"])
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(7));
}

#[test]
fn exec_without_selectors_does_not_require_existing_state() {
    let sandbox = Sandbox::new();
    let output = sandbox
        .command()
        .args(["exec", "--", "/usr/bin/printf", "%s", "ordinary"])
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(output.stdout, b"ordinary");
}
