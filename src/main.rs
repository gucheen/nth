mod osc8;
mod pty;
mod selector;
mod state;

use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::io::{self, BufRead, BufReader, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> ExitCode {
    match run(
        env::args_os().skip(1).collect(),
        io::stdin(),
        io::stdout(),
        io::stderr(),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(io::stderr(), "nth: {}", error.message);
            ExitCode::from(error.code)
        }
    }
}

#[derive(Debug)]
struct AppError {
    message: String,
    code: u8,
}

impl AppError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: 1,
        }
    }

    fn usage(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: 2,
        }
    }
}

impl From<io::Error> for AppError {
    fn from(error: io::Error) -> Self {
        Self::new(error.to_string())
    }
}

fn run<R: Read, W: Write, E: Write>(
    args: Vec<OsString>,
    stdin: R,
    mut stdout: W,
    mut stderr: E,
) -> Result<(), AppError> {
    let Some(command) = args.first().and_then(|arg| arg.to_str()) else {
        usage(&mut stderr)?;
        return Err(AppError::usage("a command is required"));
    };

    match command {
        "observe" | "run" => run_observe(&args[1..], &mut stdout),
        "capture" | "number" => run_capture(&args[1..], stdin, &mut stdout),
        "git-status" => run_git_status(&args[1..], &mut stdout),
        "git-list" => run_git_list(&args[1..], &mut stdout),
        "exec" | "x" => run_exec(&args[1..]),
        "resolve" => run_resolve(&args[1..], &mut stdout),
        "show" => run_show(&mut stdout),
        "clear" => state::clear().map_err(AppError::new),
        "init" => run_init(&args[1..], &mut stdout),
        "version" | "--version" | "-v" => writeln!(stdout, "{VERSION}").map_err(Into::into),
        "help" | "--help" | "-h" => usage(&mut stdout).map_err(Into::into),
        _ => Err(AppError::usage(format!("unknown command {command:?}"))),
    }
}

fn absolute(path: &Path) -> Result<PathBuf, AppError> {
    if path.is_absolute() {
        return Ok(path.to_owned());
    }
    env::current_dir()
        .map(|directory| directory.join(path))
        .map_err(|error| {
            AppError::new(format!(
                "cannot resolve {}: {error}",
                display(path.as_os_str())
            ))
        })
}

#[derive(Clone, Copy)]
enum Adapter {
    Auto,
    Eza,
    Lsd,
    GnuLs,
    Osc8,
}

struct ObserveOptions {
    adapter: Adapter,
    command: Vec<OsString>,
}

fn run_observe<W: Write>(args: &[OsString], output: &mut W) -> Result<(), AppError> {
    let options = parse_observe_options(args)?;
    let (program, command_args, structured) = prepare_producer(options)?;
    let produced = pty::run(&program, &command_args)
        .map_err(|error| AppError::new(format!("cannot run {}: {error}", display(&program))))?;

    if !structured {
        output.write_all(&produced.stdout)?;
        return command_status(produced.status);
    }

    let (annotated, items) = match osc8::annotate(&produced.stdout) {
        Ok(parsed) => parsed,
        Err(error) => {
            output.write_all(&produced.stdout)?;
            return Err(AppError::new(format!(
                "cannot parse producer output: {error}"
            )));
        }
    };
    output.write_all(&annotated)?;
    command_status(produced.status)?;
    state::save(items.iter().map(OsString::as_os_str)).map_err(AppError::new)
}

fn parse_observe_options(args: &[OsString]) -> Result<ObserveOptions, AppError> {
    let mut adapter = Adapter::Auto;
    let mut command = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            command.extend_from_slice(&args[index + 1..]);
            break;
        }
        if arg == "--adapter" {
            index += 1;
            let Some(value) = args.get(index).and_then(|value| value.to_str()) else {
                return Err(AppError::usage("--adapter needs a value"));
            };
            adapter = parse_adapter(value)?;
        } else if let Some(value) = arg.to_str().and_then(|arg| arg.strip_prefix("--adapter=")) {
            adapter = parse_adapter(value)?;
        } else if os_bytes(arg).starts_with(b"-") {
            return Err(AppError::usage(format!(
                "unknown observe option {:?}; put the producer after --",
                arg.to_string_lossy()
            )));
        } else {
            command.extend_from_slice(&args[index..]);
            break;
        }
        index += 1;
    }
    if command.is_empty() {
        return Err(AppError::usage("observe needs a producer command after --"));
    }
    Ok(ObserveOptions { adapter, command })
}

fn parse_adapter(value: &str) -> Result<Adapter, AppError> {
    match value {
        "auto" => Ok(Adapter::Auto),
        "eza" => Ok(Adapter::Eza),
        "lsd" => Ok(Adapter::Lsd),
        "gnu-ls" => Ok(Adapter::GnuLs),
        "osc8" => Ok(Adapter::Osc8),
        _ => Err(AppError::usage(format!(
            "unknown adapter {value:?} (supported: auto, eza, lsd, gnu-ls, osc8)"
        ))),
    }
}

fn prepare_producer(options: ObserveOptions) -> Result<(OsString, Vec<OsString>, bool), AppError> {
    let mut command = options.command.into_iter();
    let program = command.next().expect("producer command must exist");
    let mut args = command.collect::<Vec<_>>();
    let adapter = match options.adapter {
        Adapter::Auto => adapter_for(&program),
        adapter => Some(adapter),
    };
    match adapter {
        Some(Adapter::Eza) | Some(Adapter::GnuLs) => {
            args.push(OsString::from("--hyperlink=always"));
        }
        Some(Adapter::Lsd) => {
            args.push(OsString::from("--hyperlink"));
            args.push(OsString::from("always"));
        }
        Some(Adapter::Osc8) => {}
        Some(Adapter::Auto) => unreachable!(),
        None => return Ok((program, args, false)),
    };
    Ok((program, args, true))
}

fn adapter_for(program: &OsStr) -> Option<Adapter> {
    let name = Path::new(program).file_name()?.to_str()?;
    match name {
        "eza" | "exa" => Some(Adapter::Eza),
        "lsd" => Some(Adapter::Lsd),
        "gls" => Some(Adapter::GnuLs),
        "ls" if cfg!(target_os = "linux") => Some(Adapter::GnuLs),
        _ => None,
    }
}

fn command_status(status: std::process::ExitStatus) -> Result<(), AppError> {
    if status.success() {
        Ok(())
    } else {
        Err(AppError {
            message: format!("command exited with status {}", status.code().unwrap_or(1)),
            code: status.code().unwrap_or(1).clamp(1, 255) as u8,
        })
    }
}

fn run_git_status<W: Write>(args: &[OsString], output: &mut W) -> Result<(), AppError> {
    let command = if args.first().is_some_and(|arg| arg == "--") {
        &args[1..]
    } else {
        args
    };
    let Some(program) = command.first() else {
        return Err(AppError::usage("git-status needs a git command after --"));
    };
    let prefix = &command[1..];

    let indexed = (|| {
        let porcelain = git_output(
            program,
            prefix,
            ["status", "--porcelain=v2", "-z", "--branch"],
        )?;
        let status = parse_git_porcelain_v2(&porcelain)?;
        let root_output = git_output(program, prefix, ["rev-parse", "--show-toplevel"])?;
        let root = git_root(&root_output)?;
        let absolute = status
            .entries()
            .map(|entry| root.join(Path::new(&entry.path)).into_os_string())
            .collect::<Vec<_>>();
        Ok::<_, AppError>((status, absolute))
    })();
    let (status, absolute) = indexed?;
    print_git_status(output, &status, git_color_enabled())?;
    state::save(absolute.iter().map(OsString::as_os_str)).map_err(AppError::new)
}

#[derive(Clone, Copy)]
enum GitListKind {
    Branch,
    Commit,
    Stash,
    Worktree,
}

impl GitListKind {
    fn parse(value: &OsStr) -> Result<Self, AppError> {
        match value.to_str() {
            Some("branch") => Ok(Self::Branch),
            Some("commit") => Ok(Self::Commit),
            Some("stash") => Ok(Self::Stash),
            Some("worktree") => Ok(Self::Worktree),
            _ => Err(AppError::usage(format!(
                "unknown git list kind {:?} (supported: branch, commit, stash, worktree)",
                value.to_string_lossy()
            ))),
        }
    }
}

struct GitListEntry {
    value: OsString,
    label: OsString,
}

fn run_git_list<W: Write>(args: &[OsString], output: &mut W) -> Result<(), AppError> {
    let Some(kind) = args.first() else {
        return Err(AppError::usage("git-list needs a list kind"));
    };
    let kind = GitListKind::parse(kind)?;
    let command = if args.get(1).is_some_and(|arg| arg == "--") {
        &args[2..]
    } else {
        &args[1..]
    };
    let Some(program) = command.first() else {
        return Err(AppError::usage("git-list needs a git command after --"));
    };
    let prefix = &command[1..];

    let entries = match kind {
        GitListKind::Branch => git_branch_entries(program, prefix),
        GitListKind::Commit => git_commit_entries(program, prefix),
        GitListKind::Stash => git_stash_entries(program, prefix),
        GitListKind::Worktree => git_worktree_entries(program, prefix),
    };
    let entries = entries?;
    print_git_list(output, &entries)?;
    state::save(entries.iter().map(|entry| entry.value.as_os_str())).map_err(AppError::new)
}

fn git_branch_entries(program: &OsStr, prefix: &[OsString]) -> Result<Vec<GitListEntry>, AppError> {
    let raw = git_output(
        program,
        prefix,
        [
            "for-each-ref",
            "--format=%(HEAD)%00%(refname:short)",
            "refs/heads",
        ],
    )?;
    parse_delimited_git_list(&raw, 2, |fields| {
        let mut label = Vec::with_capacity(fields[0].len() + fields[1].len() + 1);
        label.extend_from_slice(fields[0]);
        label.push(b' ');
        label.extend_from_slice(fields[1]);
        Ok(GitListEntry {
            value: os_string(fields[1].to_vec()),
            label: os_string(label),
        })
    })
}

fn git_commit_entries(program: &OsStr, prefix: &[OsString]) -> Result<Vec<GitListEntry>, AppError> {
    let raw = git_output(program, prefix, ["log", "--format=%H%x00%h%x00%D%x00%s"])?;
    parse_delimited_git_list(&raw, 4, |fields| {
        let mut label = Vec::new();
        label.extend_from_slice(fields[1]);
        if !fields[2].is_empty() {
            label.extend_from_slice(b" (");
            label.extend_from_slice(fields[2]);
            label.push(b')');
        }
        label.push(b' ');
        label.extend_from_slice(fields[3]);
        Ok(GitListEntry {
            value: os_string(fields[0].to_vec()),
            label: os_string(label),
        })
    })
}

fn git_stash_entries(program: &OsStr, prefix: &[OsString]) -> Result<Vec<GitListEntry>, AppError> {
    let raw = git_output(program, prefix, ["stash", "list", "--format=%gd%x00%gs"])?;
    parse_delimited_git_list(&raw, 2, |fields| {
        let mut label = Vec::with_capacity(fields[0].len() + fields[1].len() + 2);
        label.extend_from_slice(fields[0]);
        label.extend_from_slice(b": ");
        label.extend_from_slice(fields[1]);
        Ok(GitListEntry {
            value: os_string(fields[0].to_vec()),
            label: os_string(label),
        })
    })
}

#[derive(Default)]
struct WorktreeRecord {
    path: Option<OsString>,
    head: Option<OsString>,
    branch: Option<OsString>,
    detached: bool,
    bare: bool,
    locked: bool,
    prunable: bool,
}

fn git_worktree_entries(
    program: &OsStr,
    prefix: &[OsString],
) -> Result<Vec<GitListEntry>, AppError> {
    let raw = git_output(program, prefix, ["worktree", "list", "--porcelain", "-z"])?;
    let mut entries = Vec::new();
    let mut record = WorktreeRecord::default();
    for field in raw.split(|byte| *byte == 0) {
        if field.is_empty() {
            if record.path.is_some() {
                entries.push(worktree_entry(std::mem::take(&mut record))?);
            }
            continue;
        }
        if let Some(value) = field.strip_prefix(b"worktree ") {
            record.path = Some(os_string(value.to_vec()));
        } else if let Some(value) = field.strip_prefix(b"HEAD ") {
            record.head = Some(os_string(value.to_vec()));
        } else if let Some(value) = field.strip_prefix(b"branch ") {
            record.branch = Some(os_string(value.to_vec()));
        } else if field == b"detached" {
            record.detached = true;
        } else if field == b"bare" {
            record.bare = true;
        } else if field.starts_with(b"locked") {
            record.locked = true;
        } else if field.starts_with(b"prunable") {
            record.prunable = true;
        }
    }
    if record.path.is_some() {
        entries.push(worktree_entry(record)?);
    }
    Ok(entries)
}

fn worktree_entry(record: WorktreeRecord) -> Result<GitListEntry, AppError> {
    let path = record
        .path
        .ok_or_else(|| AppError::new("invalid git worktree record: missing path"))?;
    let mut label = os_bytes(&path).to_vec();
    if let Some(head) = record.head {
        label.push(b' ');
        label.extend_from_slice(&os_bytes(&head)[..os_bytes(&head).len().min(7)]);
    }
    if let Some(branch) = record.branch {
        label.extend_from_slice(b" [");
        label.extend_from_slice(
            os_bytes(&branch)
                .strip_prefix(b"refs/heads/")
                .unwrap_or_else(|| os_bytes(&branch)),
        );
        label.push(b']');
    } else if record.detached {
        label.extend_from_slice(b" (detached HEAD)");
    } else if record.bare {
        label.extend_from_slice(b" (bare)");
    }
    if record.locked {
        label.extend_from_slice(b" locked");
    }
    if record.prunable {
        label.extend_from_slice(b" prunable");
    }
    Ok(GitListEntry {
        value: path,
        label: os_string(label),
    })
}

fn parse_delimited_git_list<F>(
    input: &[u8],
    field_count: usize,
    mut entry: F,
) -> Result<Vec<GitListEntry>, AppError>
where
    F: FnMut(&[&[u8]]) -> Result<GitListEntry, AppError>,
{
    let mut result = Vec::new();
    for line in input.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        let fields = line.split(|byte| *byte == 0).collect::<Vec<_>>();
        if fields.len() != field_count {
            return Err(AppError::new("invalid structured git list output"));
        }
        result.push(entry(&fields)?);
    }
    Ok(result)
}

fn print_git_list<W: Write>(output: &mut W, entries: &[GitListEntry]) -> io::Result<()> {
    let width = decimal_width(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        writeln!(
            output,
            "[{:>width$}] {}",
            index + 1,
            display(&entry.label),
            width = width
        )?;
    }
    Ok(())
}

fn git_output<const N: usize>(
    program: &OsStr,
    prefix: &[OsString],
    args: [&str; N],
) -> Result<Vec<u8>, AppError> {
    let result = Command::new(program)
        .args(prefix)
        .args(args)
        .output()
        .map_err(|error| AppError::new(format!("cannot run {}: {error}", display(program))))?;
    if result.status.success() {
        Ok(result.stdout)
    } else {
        let message = String::from_utf8_lossy(&result.stderr);
        Err(AppError::new(format!(
            "git query failed: {}",
            message.trim()
        )))
    }
}

fn git_root(output: &[u8]) -> Result<PathBuf, AppError> {
    let root = output.strip_suffix(b"\n").unwrap_or(output);
    if root.is_empty() {
        return Err(AppError::new("git returned an empty repository root"));
    }
    Ok(PathBuf::from(os_string(root.to_vec())))
}

#[derive(Debug, Eq, PartialEq)]
struct GitEntry {
    label: &'static str,
    path: OsString,
    original: Option<OsString>,
}

#[derive(Debug, Default, Eq, PartialEq)]
struct GitStatus {
    branch: Option<OsString>,
    oid: Option<OsString>,
    upstream: Option<OsString>,
    ahead: usize,
    behind: usize,
    staged: Vec<GitEntry>,
    unstaged: Vec<GitEntry>,
    unmerged: Vec<GitEntry>,
    untracked: Vec<GitEntry>,
}

impl GitStatus {
    fn entries(&self) -> impl Iterator<Item = &GitEntry> {
        self.staged
            .iter()
            .chain(&self.unstaged)
            .chain(&self.unmerged)
            .chain(&self.untracked)
    }
}

fn parse_git_porcelain_v2(input: &[u8]) -> Result<GitStatus, AppError> {
    let mut records = input.split(|byte| *byte == 0).peekable();
    let mut status = GitStatus::default();
    while let Some(record) = records.next() {
        if record.is_empty() || record.starts_with(b"! ") {
            continue;
        }
        if record.starts_with(b"# ") {
            parse_git_header(&mut status, record)?;
            continue;
        }
        match record.first() {
            Some(b'1') => {
                let xy = git_xy(record)?;
                let path = field_after_spaces(record, 8)
                    .ok_or_else(|| AppError::new("invalid git porcelain v2 status record"))?;
                push_git_xy(&mut status, xy, os_string(path.to_vec()), None)?;
            }
            Some(b'2') => {
                let xy = git_xy(record)?;
                let path = field_after_spaces(record, 9)
                    .ok_or_else(|| AppError::new("invalid git porcelain v2 rename record"))?;
                let original = records
                    .next()
                    .filter(|path| !path.is_empty())
                    .ok_or_else(|| {
                        AppError::new(
                            "invalid git porcelain v2 rename record: missing original path",
                        )
                    })?;
                push_git_xy(
                    &mut status,
                    xy,
                    os_string(path.to_vec()),
                    Some(os_string(original.to_vec())),
                )?;
            }
            Some(b'u') => {
                let xy = git_xy(record)?;
                let path = field_after_spaces(record, 10)
                    .ok_or_else(|| AppError::new("invalid git porcelain v2 unmerged record"))?;
                status.unmerged.push(GitEntry {
                    label: unmerged_label(xy),
                    path: os_string(path.to_vec()),
                    original: None,
                });
            }
            Some(b'?') => {
                let path = record
                    .strip_prefix(b"? ")
                    .filter(|path| !path.is_empty())
                    .ok_or_else(|| AppError::new("invalid git porcelain v2 untracked record"))?;
                status.untracked.push(GitEntry {
                    label: "untracked",
                    path: os_string(path.to_vec()),
                    original: None,
                });
            }
            _ => return Err(AppError::new("invalid git porcelain v2 status record")),
        }
    }
    Ok(status)
}

fn parse_git_header(status: &mut GitStatus, record: &[u8]) -> Result<(), AppError> {
    if let Some(value) = record.strip_prefix(b"# branch.head ") {
        status.branch = Some(os_string(value.to_vec()));
    } else if let Some(value) = record.strip_prefix(b"# branch.oid ") {
        status.oid = Some(os_string(value.to_vec()));
    } else if let Some(value) = record.strip_prefix(b"# branch.upstream ") {
        status.upstream = Some(os_string(value.to_vec()));
    } else if let Some(value) = record.strip_prefix(b"# branch.ab ") {
        let text = std::str::from_utf8(value)
            .map_err(|_| AppError::new("invalid git porcelain v2 branch counts"))?;
        for field in text.split_ascii_whitespace() {
            if let Some(value) = field.strip_prefix('+') {
                status.ahead = value
                    .parse()
                    .map_err(|_| AppError::new("invalid git porcelain v2 ahead count"))?;
            } else if let Some(value) = field.strip_prefix('-') {
                status.behind = value
                    .parse()
                    .map_err(|_| AppError::new("invalid git porcelain v2 behind count"))?;
            }
        }
    }
    Ok(())
}

fn git_xy(record: &[u8]) -> Result<&[u8], AppError> {
    record
        .get(2..4)
        .filter(|xy| xy.len() == 2)
        .ok_or_else(|| AppError::new("invalid git porcelain v2 XY status"))
}

fn push_git_xy(
    status: &mut GitStatus,
    xy: &[u8],
    path: OsString,
    original: Option<OsString>,
) -> Result<(), AppError> {
    if xy[0] != b'.' {
        status.staged.push(GitEntry {
            label: tracked_label(xy[0])?,
            path: path.clone(),
            original,
        });
    }
    if xy[1] != b'.' {
        status.unstaged.push(GitEntry {
            label: tracked_label(xy[1])?,
            path,
            original: None,
        });
    }
    Ok(())
}

fn tracked_label(code: u8) -> Result<&'static str, AppError> {
    match code {
        b'M' => Ok("modified"),
        b'T' => Ok("type changed"),
        b'A' => Ok("new file"),
        b'D' => Ok("deleted"),
        b'R' => Ok("renamed"),
        b'C' => Ok("copied"),
        _ => Err(AppError::new(format!(
            "unsupported git porcelain v2 status code {:?}",
            char::from(code)
        ))),
    }
}

fn unmerged_label(xy: &[u8]) -> &'static str {
    match xy {
        b"DD" => "both deleted",
        b"AU" => "added by us",
        b"UD" => "deleted by them",
        b"UA" => "added by them",
        b"DU" => "deleted by us",
        b"AA" => "both added",
        b"UU" => "both modified",
        _ => "unmerged",
    }
}

fn field_after_spaces(record: &[u8], count: usize) -> Option<&[u8]> {
    let mut found = 0;
    for (index, byte) in record.iter().enumerate() {
        if *byte == b' ' {
            found += 1;
            if found == count {
                return record.get(index + 1..).filter(|field| !field.is_empty());
            }
        }
    }
    None
}

fn git_color_enabled() -> bool {
    if env::var_os("NO_COLOR").is_some() {
        return false;
    }
    if env::var_os("CLICOLOR_FORCE").is_some_and(|value| value != "0") {
        return true;
    }
    io::stdout().is_terminal() && env::var_os("TERM").is_none_or(|value| value != "dumb")
}

fn print_git_status<W: Write>(output: &mut W, status: &GitStatus, color: bool) -> io::Result<()> {
    write!(output, "On branch ")?;
    ansi(output, color, "1;37")?;
    match status.branch.as_deref() {
        Some(branch) if branch != "(detached)" => write!(output, "{}", display(branch))?,
        _ => {
            write!(output, "detached HEAD")?;
            if let Some(oid) = &status.oid {
                write!(output, " at {:.12}", display(oid))?;
            }
        }
    }
    ansi(output, color, "0")?;
    if let Some(upstream) = &status.upstream {
        write!(output, " tracking {}", display(upstream))?;
    }
    if status.ahead > 0 || status.behind > 0 {
        write!(
            output,
            " [ahead {}, behind {}]",
            status.ahead, status.behind
        )?;
    }
    writeln!(output, "\n")?;

    let total = status.entries().count();
    if total == 0 {
        ansi(output, color, "32")?;
        writeln!(output, "nothing to commit, working tree clean")?;
        ansi(output, color, "0")?;
        return Ok(());
    }

    let index_width = decimal_width(total);
    let label_width = status
        .entries()
        .map(|entry| entry.label.len())
        .max()
        .unwrap_or(0);
    let mut index = 1;
    print_git_section(
        output,
        "Changes to be committed",
        &status.staged,
        "1;32",
        color,
        index_width,
        label_width,
        &mut index,
    )?;
    print_git_section(
        output,
        "Changes not staged for commit",
        &status.unstaged,
        "1;33",
        color,
        index_width,
        label_width,
        &mut index,
    )?;
    print_git_section(
        output,
        "Unmerged paths",
        &status.unmerged,
        "1;31",
        color,
        index_width,
        label_width,
        &mut index,
    )?;
    print_git_section(
        output,
        "Untracked files",
        &status.untracked,
        "1;36",
        color,
        index_width,
        label_width,
        &mut index,
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn print_git_section<W: Write>(
    output: &mut W,
    heading: &str,
    entries: &[GitEntry],
    section_color: &str,
    color: bool,
    index_width: usize,
    label_width: usize,
    index: &mut usize,
) -> io::Result<()> {
    if entries.is_empty() {
        return Ok(());
    }
    ansi(output, color, section_color)?;
    write!(output, "➤")?;
    ansi(output, color, "0")?;
    writeln!(output, " {heading}")?;
    writeln!(output)?;
    for entry in entries {
        write!(output, "  ")?;
        ansi(output, color, git_label_color(entry.label))?;
        write!(output, "{:>label_width$}", entry.label)?;
        ansi(output, color, "0")?;
        write!(output, ":  ")?;
        ansi(output, color, "1;37")?;
        write!(output, "[{:>index_width$}]", *index)?;
        ansi(output, color, "0")?;
        write!(output, " ")?;
        ansi(output, color, section_color)?;
        if let Some(original) = &entry.original {
            write!(output, "{} -> ", display(original))?;
        }
        write!(output, "{}", display(&entry.path))?;
        ansi(output, color, "0")?;
        writeln!(output)?;
        *index += 1;
    }
    writeln!(output)
}

fn git_label_color(label: &str) -> &'static str {
    match label {
        "modified" | "new file" => "32",
        "renamed" | "copied" => "34",
        "deleted" | "both deleted" | "deleted by them" | "deleted by us" | "unmerged" => "31",
        "type changed" => "35",
        "untracked" => "36",
        "added by us" | "added by them" | "both added" | "both modified" => "33",
        _ => "0",
    }
}

fn ansi<W: Write>(output: &mut W, enabled: bool, code: &str) -> io::Result<()> {
    if enabled {
        write!(output, "\x1b[{code}m")?;
    }
    Ok(())
}

struct CaptureOptions {
    null: bool,
    absolute: bool,
    command: Vec<OsString>,
}

fn run_capture<R: Read, W: Write>(
    args: &[OsString],
    stdin: R,
    output: &mut W,
) -> Result<(), AppError> {
    let options = parse_capture_options(args)?;
    let records = if options.command.is_empty() {
        read_records(stdin, options.null)?
    } else {
        capture_command(&options.command, options.null)?
    };
    let stored = if options.absolute {
        records
            .iter()
            .map(|record| absolute(Path::new(record)).map(PathBuf::into_os_string))
            .collect::<Result<Vec<_>, _>>()?
    } else {
        records.clone()
    };
    print_records(output, &records)?;
    state::save(stored.iter().map(OsString::as_os_str)).map_err(AppError::new)
}

fn parse_capture_options(args: &[OsString]) -> Result<CaptureOptions, AppError> {
    let mut null = false;
    let mut absolute = false;
    let mut command = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            command.extend_from_slice(&args[index + 1..]);
            break;
        }
        if arg == "-0" || arg == "--null" {
            null = true;
        } else if arg == "--absolute" {
            absolute = true;
        } else if os_bytes(arg).starts_with(b"-") {
            return Err(AppError::usage(format!(
                "unknown capture option {:?}; put the captured command after --",
                arg.to_string_lossy()
            )));
        } else {
            command.extend_from_slice(&args[index..]);
            break;
        }
        index += 1;
    }
    Ok(CaptureOptions {
        null,
        absolute,
        command,
    })
}

fn capture_command(args: &[OsString], null: bool) -> Result<Vec<OsString>, AppError> {
    let Some(program) = args.first() else {
        return Err(AppError::usage("capture needs a command after --"));
    };
    let mut child = Command::new(program)
        .args(&args[1..])
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|error| AppError::new(format!("cannot start {}: {error}", display(program))))?;
    let records = read_records(
        child.stdout.take().expect("captured stdout must exist"),
        null,
    )?;
    let status = child.wait()?;
    if !status.success() {
        return Err(AppError::new(format!(
            "captured command exited with status {}",
            status.code().unwrap_or(1)
        )));
    }
    Ok(records)
}

fn read_records<R: Read>(input: R, null: bool) -> Result<Vec<OsString>, AppError> {
    let delimiter = if null { 0 } else { b'\n' };
    let mut reader = BufReader::new(input);
    let mut records = Vec::new();
    loop {
        let mut record = Vec::new();
        let count = reader.read_until(delimiter, &mut record)?;
        if count == 0 {
            break;
        }
        if record.last() == Some(&delimiter) {
            record.pop();
        }
        if !record.is_empty() {
            records.push(os_string(record));
        }
    }
    Ok(records)
}

fn print_records<W: Write>(output: &mut W, records: &[OsString]) -> io::Result<()> {
    let width = decimal_width(records.len());
    for (index, record) in records.iter().enumerate() {
        writeln!(
            output,
            "[{:>width$}] {}",
            index + 1,
            display(record),
            width = width
        )?;
    }
    Ok(())
}

fn run_exec(args: &[OsString]) -> Result<(), AppError> {
    let args = if args.first().is_some_and(|arg| arg == "--") {
        &args[1..]
    } else {
        args
    };
    let Some(program) = args.first() else {
        return Err(AppError::usage("exec needs a command"));
    };
    let expanded = expand_current(&args[1..])?;
    let status = Command::new(program)
        .args(expanded)
        .status()
        .map_err(|error| AppError::new(format!("cannot run {}: {error}", display(program))))?;
    if status.success() {
        Ok(())
    } else {
        Err(AppError {
            message: format!("command exited with status {}", status.code().unwrap_or(1)),
            code: status.code().unwrap_or(1).clamp(1, 255) as u8,
        })
    }
}

fn run_resolve<W: Write>(args: &[OsString], output: &mut W) -> Result<(), AppError> {
    let args = if args.first().is_some_and(|arg| arg == "--") {
        &args[1..]
    } else {
        args
    };
    let expanded = expand_current(args)?;
    output.write_all(&[0])?;
    for arg in expanded {
        output.write_all(os_bytes(&arg))?;
        output.write_all(&[0])?;
    }
    Ok(())
}

fn expand_current(args: &[OsString]) -> Result<Vec<OsString>, AppError> {
    let items = if selector::contains_selector(args) {
        state::load().map_err(AppError::new)?.items
    } else {
        Vec::new()
    };
    selector::expand(args, &items).map_err(AppError::new)
}

fn run_show<W: Write>(output: &mut W) -> Result<(), AppError> {
    let current = state::load().map_err(AppError::new)?;
    print_records(output, &current.items)?;
    Ok(())
}

fn run_init<W: Write>(args: &[OsString], output: &mut W) -> Result<(), AppError> {
    if args.len() != 1 {
        return Err(AppError::usage(
            "init needs exactly one shell: bash or fish",
        ));
    }
    let binary = env::current_exe()
        .map_err(|error| AppError::new(format!("cannot locate nth executable: {error}")))?;
    match args[0].to_str() {
        Some("bash") => {
            output.write_all(b"# nth shell integration\n_nth_binary=")?;
            write_bash_literal(output, binary.as_os_str())?;
            output.write_all(b"\n")?;
            output
                .write_all(SHELL_INIT_BASH.as_bytes())
                .map_err(Into::into)
        }
        Some("fish") => {
            output.write_all(b"# nth shell integration\nset -g _nth_binary ")?;
            write_fish_literal(output, binary.as_os_str())?;
            output.write_all(b"\n")?;
            output
                .write_all(SHELL_INIT_FISH.as_bytes())
                .map_err(Into::into)
        }
        _ => Err(AppError::usage(format!(
            "unsupported shell {:?} (supported: bash, fish)",
            args[0].to_string_lossy()
        ))),
    }
}

fn write_bash_literal<W: Write>(output: &mut W, value: &OsStr) -> io::Result<()> {
    output.write_all(b"'")?;
    for byte in os_bytes(value) {
        if *byte == b'\'' {
            output.write_all(b"'\\''")?;
        } else {
            output.write_all(&[*byte])?;
        }
    }
    output.write_all(b"'")
}

fn write_fish_literal<W: Write>(output: &mut W, value: &OsStr) -> io::Result<()> {
    output.write_all(b"'")?;
    for byte in os_bytes(value) {
        if matches!(*byte, b'\'' | b'\\') {
            output.write_all(b"\\")?;
        }
        output.write_all(&[*byte])?;
    }
    output.write_all(b"'")
}

const SHELL_INIT_BASH: &str = r#"if [[ -z "${NTH_SESSION:-}" ]]; then
  export NTH_SESSION="$$"
fi

_nth_remember_command() {
  local name="$1"
  local definition alias_definition
  if alias "$name" >/dev/null 2>&1; then
    alias_definition="$(alias "$name")"
    alias_definition="${alias_definition#*=}"
    eval "definition=$alias_definition"
    case "$definition" in
      *'|'*|*';'*|*'&'*|*'<'*|*'>'*|*'('*|*')'*|*'$'*|*'`'*)
        printf 'nth: leaving complex %s alias unchanged; only command-prefix aliases can be wrapped\n' "$name" >&2
        return 1;;
    esac
    unalias "$name"
  elif declare -F "$name" >/dev/null 2>&1; then
    printf 'nth: leaving existing %s function unchanged; function adapters are not supported\n' "$name" >&2
    return 1
  else
    definition="$name"
  fi
  printf -v "_nth_original_$name" '%s' "$definition"
}

_nth_dispatch() {
  local mode="$1" name="$2"
  shift 2
  local variable="_nth_original_$name"
  local definition="${!variable}"
  local -a original
  eval "original=( $definition )" || return 2

  if [[ "$mode" == observe ]]; then
    command "$_nth_binary" observe -- "${original[@]}" "$@"
  else
    command "$_nth_binary" exec -- "${original[@]}" "$@"
  fi
}

_nth_dispatch_git() {
  local variable="_nth_original_git"
  local definition="${!variable}"
  local -a original
  eval "original=( $definition )" || return 2
  local number_output=1
  if [[ $- == *i* && ! -t 1 ]]; then
    number_output=0
  fi

  if [[ "$number_output" -eq 1 && "$#" -eq 1 && "$1" == status ]]; then
    command "$_nth_binary" git-status -- "${original[@]}"
  elif [[ "$number_output" -eq 1 && "$#" -eq 1 && "$1" == branch ]]; then
    command "$_nth_binary" git-list branch -- "${original[@]}"
  elif [[ "$number_output" -eq 1 && "$#" -eq 1 && "$1" == log ]]; then
    command "$_nth_binary" git-list commit -- "${original[@]}"
  elif [[ "$number_output" -eq 1 && "$#" -eq 2 && "$1" == stash && "$2" == list ]]; then
    command "$_nth_binary" git-list stash -- "${original[@]}"
  elif [[ "$number_output" -eq 1 && "$#" -eq 2 && "$1" == worktree && "$2" == list ]]; then
    command "$_nth_binary" git-list worktree -- "${original[@]}"
  elif [[ "$#" -gt 0 && "$1" == status ]]; then
    command "${original[@]}" "$@"
  elif [[ "$#" -gt 0 && ( "$1" == add || "$1" == branch || "$1" == checkout || "$1" == cherry-pick || "$1" == diff || "$1" == log || "$1" == merge || "$1" == rebase || "$1" == reset || "$1" == restore || "$1" == revert || "$1" == rm || "$1" == show || "$1" == stash || "$1" == switch || "$1" == tag || "$1" == worktree ) ]]; then
    command "$_nth_binary" exec -- "${original[@]}" "$@"
  else
    command "${original[@]}" "$@"
  fi
}

_nth_dispatch_cd() {
  local variable="_nth_original_cd"
  local definition="${!variable}"
  local -a original resolved
  local value
  local resolved_ok=0
  eval "original=( $definition )" || return 2

  while IFS= read -r -d '' value; do
    if [[ "$resolved_ok" -eq 0 ]]; then
      resolved_ok=1
    else
      resolved+=("$value")
    fi
  done < <(command "$_nth_binary" resolve -- "$@")
  [[ "$resolved_ok" -eq 1 ]] || return 1

  if [[ "${original[0]}" == cd ]]; then
    builtin cd "${original[@]:1}" "${resolved[@]}"
  else
    "${original[@]}" "${resolved[@]}"
  fi
}

_nth_wrap_command() {
  local name="$1" mode="$2"
  _nth_remember_command "$name" || return
  eval "$name() { _nth_dispatch $mode $name \"\$@\"; }"
}

_nth_wrap_command ls observe
for _nth_name in cp mv rm cat less stat diff chmod chown ln file wc du; do
  _nth_wrap_command "$_nth_name" exec
done
if _nth_remember_command cd; then
  eval 'cd() { _nth_dispatch_cd "$@"; }'
fi
if _nth_remember_command git; then
  git() { _nth_dispatch_git "$@"; }
fi
unset _nth_name
unset -f _nth_wrap_command _nth_remember_command

ncap() { command "$_nth_binary" capture "$@"; }
nx() { command "$_nth_binary" exec -- "$@"; }
nshow() { command "$_nth_binary" show; }
"#;

const SHELL_INIT_FISH: &str = r#"if not set -q NTH_SESSION
  set -gx NTH_SESSION $fish_pid
end

function _nth_remember_command --argument-names name
  set -l words
  if functions -q $name
    set -l definition (functions $name)
    set -l details (functions --details --verbose $name)
    set -l alias_pattern "--description 'alias "$name"[ =](.*)'"
    set -l alias_command (string match -r --groups-only -- $alias_pattern $definition)
    if test -z "$alias_command"
      contains -- autoloaded $details; and return 1
      printf 'nth: leaving existing %s function unchanged; only aliases can be wrapped\n' $name >&2
      return 1
    end
    echo $alias_command | read --tokenize -a words
    for word in $words
      if contains -- $word '|' '||' '&' '&&' ';' '<' '>' '2>'
        printf 'nth: leaving complex %s alias unchanged; only command-prefix aliases can be wrapped\n' $name >&2
        return 1
      end
    end
    functions --erase $name
  else
    set words $name
  end
  set -g "_nth_original_$name" $words
  return 0
end

function _nth_dispatch --argument-names mode name
  set -e argv[1..2]
  set -l variable "_nth_original_$name"
  if test "$mode" = git
    _nth_dispatch_git $argv
  else if test "$mode" = observe
    command $_nth_binary observe -- $$variable $argv
  else
    command $_nth_binary exec -- $$variable $argv
  end
end

function _nth_dispatch_git
  set -l variable _nth_original_git
  set -l number_output 1
  if status is-interactive; and not isatty stdout
    set number_output 0
  end
  if test $number_output -eq 1; and test (count $argv) -eq 1; and test "$argv[1]" = status
    command $_nth_binary git-status -- $$variable
    return
  end
  if test $number_output -eq 1; and test (count $argv) -eq 1; and test "$argv[1]" = branch
    command $_nth_binary git-list branch -- $$variable
    return
  end
  if test $number_output -eq 1; and test (count $argv) -eq 1; and test "$argv[1]" = log
    command $_nth_binary git-list commit -- $$variable
    return
  end
  if test $number_output -eq 1; and test (count $argv) -eq 2; and test "$argv[1]" = stash; and test "$argv[2]" = list
    command $_nth_binary git-list stash -- $$variable
    return
  end
  if test $number_output -eq 1; and test (count $argv) -eq 2; and test "$argv[1]" = worktree; and test "$argv[2]" = list
    command $_nth_binary git-list worktree -- $$variable
    return
  end
  if test (count $argv) -gt 0; and test "$argv[1]" = status
    command $$variable $argv
    return
  end
  switch $argv[1]
    case add branch checkout cherry-pick diff log merge rebase reset restore revert rm show stash switch tag worktree
      command $_nth_binary exec -- $$variable $argv
    case '*'
      command $$variable $argv
  end
end

function _nth_wrap_cd
  functions -q _nth_original_cd
  and return
  if functions -q cd
    set -l definition (functions cd)
    set -l details (functions --details --verbose cd)
    set -l alias_pattern "--description 'alias cd[ =](.*)'"
    set -l alias_command (string match -r --groups-only -- $alias_pattern $definition)
    if test -z "$alias_command"; and not contains -- autoloaded $details
      printf 'nth: leaving existing cd function unchanged; function adapters are not supported\n' >&2
      return 1
    end
    functions --copy cd _nth_original_cd
  else
    function _nth_original_cd
      builtin cd $argv
    end
  end

  function cd --description 'nth shortcut-aware directory change'
    set -l resolved
    set -l resolved_ok 0
    command $_nth_binary resolve -- $argv | while read --null value
      if test $resolved_ok -eq 0
        set resolved_ok 1
      else
        set -a resolved "$value"
      end
    end
    test $resolved_ok -eq 1
    or return 1
    _nth_original_cd $resolved
  end
end

function _nth_wrap_command --argument-names name mode
  _nth_remember_command $name
  or return
  eval "function $name; _nth_dispatch $mode $name \$argv; end"
end

_nth_wrap_command ls observe
for _nth_name in cp mv rm cat less stat diff chmod chown ln file wc du
  _nth_wrap_command $_nth_name exec
end
_nth_wrap_cd
_nth_wrap_command git git
set -e _nth_name
functions --erase _nth_wrap_cd _nth_wrap_command _nth_remember_command

function ncap; command $_nth_binary capture $argv; end
function nx; command $_nth_binary exec -- $argv; end
function nshow; command $_nth_binary show; end
"#;

fn usage<W: Write>(output: &mut W) -> io::Result<()> {
    writeln!(
        output,
        r#"nth - fast numeric shortcuts for command-line output

Usage:
  nth observe [--adapter NAME] -- command [args ...]
                                         proxy and parse the original command
  nth capture [-0] [--absolute] [-- command ...]
                                         number records from stdin or a command
  nth git-status -- git [global args]    preserve git status and append shortcuts
  nth git-list KIND -- git [global args] number branches, commits, stashes, or worktrees
  nth exec -- command [args ...]        expand selectors and run a command
  nth resolve -- [args ...]             write expanded arguments as NUL-delimited records
  nth show                              show the current shortcut table
  nth clear                             discard the current shortcut table
  nth init bash|fish                    print shell integration

Selectors:
  3          one item
  2-5        an inclusive range
  1,4-6      a list containing ranges
  @3         explicit selector form
  =755       literal argument (removes the leading '=')"#
    )
}

fn decimal_width(number: usize) -> usize {
    number.max(1).ilog10() as usize + 1
}

#[cfg(unix)]
fn os_bytes(value: &OsStr) -> &[u8] {
    value.as_bytes()
}

#[cfg(not(unix))]
fn os_bytes(value: &OsStr) -> &[u8] {
    value.to_str().unwrap_or_default().as_bytes()
}

#[cfg(unix)]
fn os_string(value: Vec<u8>) -> OsString {
    use std::os::unix::ffi::OsStringExt;

    OsString::from_vec(value)
}

#[cfg(not(unix))]
fn os_string(value: Vec<u8>) -> OsString {
    String::from_utf8_lossy(&value).into_owned().into()
}

fn display(value: &OsStr) -> String {
    if let Some(value) = value.to_str() {
        let mut result = String::new();
        for character in value.chars() {
            match character {
                '\n' => result.push_str("\\n"),
                '\r' => result.push_str("\\r"),
                '\t' => result.push_str("\\t"),
                character if character.is_control() => {
                    let _ = write!(result, "\\u{{{:x}}}", character as u32);
                }
                character => result.push(character),
            }
        }
        return result;
    }

    let mut result = String::new();
    for &byte in os_bytes(value) {
        match byte {
            b'\n' => result.push_str("\\n"),
            b'\r' => result.push_str("\\r"),
            b'\t' => result.push_str("\\t"),
            0x20..=0x7e => result.push(byte as char),
            _ => {
                let _ = write!(result, "\\x{byte:02x}");
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_numbers_lines() {
        let records = read_records(&b"alpha\ntwo words\n"[..], false).unwrap();
        let mut output = Vec::new();

        print_records(&mut output, &records).unwrap();

        assert_eq!(
            String::from_utf8(output).unwrap(),
            "[1] alpha\n[2] two words\n"
        );
    }

    #[test]
    fn nul_mode_preserves_newlines_and_escapes_only_for_display() {
        let records = read_records(&b"line\nbreak\0normal\0"[..], true).unwrap();
        let mut output = Vec::new();

        print_records(&mut output, &records).unwrap();

        assert_eq!(
            String::from_utf8(output).unwrap(),
            "[1] line\\nbreak\n[2] normal\n"
        );
        assert_eq!(os_bytes(&records[0]), b"line\nbreak");
    }

    #[test]
    fn bash_init_wraps_the_original_commands() {
        let mut output = Vec::new();

        run_init(&[OsString::from("bash")], &mut output).unwrap();

        let output = String::from_utf8(output).unwrap();
        for fragment in [
            "NTH_SESSION",
            "_nth_binary=",
            "_nth_remember_command",
            "_nth_wrap_command ls observe",
            "command \"$_nth_binary\" observe",
            "command \"$_nth_binary\" exec",
            "command \"$_nth_binary\" resolve",
            "_nth_dispatch_cd",
            "number_output",
            "! -t 1",
            "git-list branch",
            "git-list commit",
            "git-list stash",
            "git-list worktree",
        ] {
            assert!(output.contains(fragment));
        }
    }

    #[test]
    fn fish_init_uses_fish_session_and_argument_syntax() {
        let mut output = Vec::new();

        run_init(&[OsString::from("fish")], &mut output).unwrap();

        let output = String::from_utf8(output).unwrap();
        for fragment in [
            "$fish_pid",
            "$argv",
            "set -g _nth_binary",
            "_nth_wrap_command ls observe",
            "command $_nth_binary exec",
            "command $_nth_binary resolve",
            "_nth_wrap_cd",
            "number_output",
            "status is-interactive",
            "not isatty stdout",
            "git-list branch",
            "git-list commit",
            "git-list stash",
            "git-list worktree",
        ] {
            assert!(output.contains(fragment));
        }
    }

    #[test]
    fn parses_and_groups_git_porcelain_v2_status() {
        let input = b"# branch.oid abcdef0123456789\0\
# branch.head main\0\
# branch.upstream origin/main\0\
# branch.ab +2 -1\0\
1 MM N... 100644 100644 100644 aaa bbb src/main.rs\0\
2 R. N... 100644 100644 100644 aaa bbb R100 new name\0old name\0\
u UU N... 100644 100644 100644 100644 aaa bbb ccc conflict\0\
? notes from review.txt\0";

        let status = parse_git_porcelain_v2(input).unwrap();
        let entries = status
            .entries()
            .map(|entry| {
                (
                    entry.label,
                    entry.path.to_string_lossy().into_owned(),
                    entry
                        .original
                        .as_deref()
                        .map(|path| path.to_string_lossy().into_owned()),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(status.branch.as_deref(), Some(OsStr::new("main")));
        assert_eq!(status.upstream.as_deref(), Some(OsStr::new("origin/main")));
        assert_eq!((status.ahead, status.behind), (2, 1));
        assert_eq!(
            entries,
            [
                ("modified", "src/main.rs".to_owned(), None),
                (
                    "renamed",
                    "new name".to_owned(),
                    Some("old name".to_owned())
                ),
                ("modified", "src/main.rs".to_owned(), None),
                ("both modified", "conflict".to_owned(), None),
                ("untracked", "notes from review.txt".to_owned(), None),
            ]
        );
    }

    #[test]
    fn colors_git_labels_and_paths_by_section() {
        let input = b"# branch.head main\0\
1 MM N... 100644 100644 100644 aaa bbb mixed.txt\0\
2 R. N... 100644 100644 100644 aaa bbb R100 renamed.txt\0old.txt\0\
? loose.txt\0";
        let status = parse_git_porcelain_v2(input).unwrap();
        let mut output = Vec::new();

        print_git_status(&mut output, &status, true).unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("\x1b[1;32m➤\x1b[0m Changes to be committed"));
        assert!(output.contains("\x1b[34m  renamed\x1b[0m"));
        assert!(output.contains("\x1b[1;32mmixed.txt\x1b[0m"));
        assert!(output.contains("\x1b[1;33mmixed.txt\x1b[0m"));
        assert!(output.contains("\x1b[36muntracked\x1b[0m"));
        assert!(output.contains("\x1b[1;36mloose.txt\x1b[0m"));
    }
}
