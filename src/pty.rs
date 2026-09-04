use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd};
use std::process::{Command, ExitStatus, Stdio};

pub struct Output {
    pub stdout: Vec<u8>,
    pub status: ExitStatus,
}

pub fn run(program: &OsString, args: &[OsString]) -> io::Result<Output> {
    let (mut master, slave) = open()?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::from(slave))
        .stderr(Stdio::inherit())
        .spawn()?;

    let mut stdout = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        match master.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => stdout.extend_from_slice(&buffer[..count]),
            Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
            Err(error) => return Err(error),
        }
    }
    let status = child.wait()?;
    Ok(Output { stdout, status })
}

fn open() -> io::Result<(File, File)> {
    let mut master_fd = -1;
    let mut slave_fd = -1;
    let mut window: libc::winsize = unsafe { std::mem::zeroed() };

    // SAFETY: window points to initialized writable storage and STDOUT_FILENO is
    // only queried; failure simply leaves the PTY with its default dimensions.
    unsafe {
        libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut window);
    }
    if window.ws_col == 0 || window.ws_row == 0 {
        window.ws_col = 80;
        window.ws_row = 24;
    }
    // SAFETY: openpty initializes both descriptors on success. Null name and
    // termios pointers request system defaults; ownership is transferred below.
    let result = unsafe {
        libc::openpty(
            &mut master_fd,
            &mut slave_fd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut window,
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: successful openpty returned two new owned file descriptors.
    let master = unsafe { File::from_raw_fd(master_fd) };
    let slave = unsafe { File::from_raw_fd(slave_fd) };
    set_raw(slave.as_raw_fd())?;
    Ok((master, slave))
}

fn set_raw(fd: libc::c_int) -> io::Result<()> {
    let mut terminal: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: fd is an open PTY slave and terminal points to writable storage.
    if unsafe { libc::tcgetattr(fd, &mut terminal) } == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: terminal is initialized by tcgetattr and remains valid here.
    unsafe {
        libc::cfmakeraw(&mut terminal);
    }
    // SAFETY: fd and terminal remain valid for the duration of the call.
    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &terminal) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
