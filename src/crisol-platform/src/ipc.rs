//! The Windows side of talking to the daemon (F8.2): the named pipe.
//!
//! The daemon creates the pipe (with `tokio`, in `crisol-daemon`) and this module gives it what
//! tokio cannot: a security descriptor that lets in the current user and `SYSTEM` and nobody
//! else ([`PipeSecurity`]). A client reaches it with [`connect_to_daemon`], which opens the pipe
//! like a file and, before sending anything, checks that **the process on the other end runs as
//! the same user**: pipe names are global, so another account could have created one with that
//! name first, and a client that trusted it would be feeding it the user's folders and commands.
//!
//! [`spawn_daemon`] starts `crisold` detached, so that it outlives the terminal that asked.

use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io::{self, BufReader};
use std::mem::size_of;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use crisol_protocol::Client;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{LocalFree, HANDLE, HLOCAL};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::System::Pipes::GetNamedPipeServerProcessId;
use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

use crate::private_dir::{current_user_sid, user_sid_of_process};

/// `ERROR_PIPE_BUSY`: every instance of the pipe is serving somebody; try again in a moment.
const ERROR_PIPE_BUSY: i32 = 231;
/// `FILE_FLAG_OVERLAPPED` is not wanted: the client reads and writes synchronously.
const SECURITY_SQOS_PRESENT: u32 = 0x0010_0000;
/// `SECURITY_IDENTIFICATION`: the server can know who the client is but cannot act as it.
const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;

const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

/// A security descriptor for the pipe: full access for the current user and `SYSTEM`, nothing
/// for anybody else (not even other administrators: it is this user's daemon).
#[derive(Debug)]
pub struct PipeSecurity {
    descriptor: PSECURITY_DESCRIPTOR,
    attributes: SECURITY_ATTRIBUTES,
}

impl PipeSecurity {
    /// The descriptor for the account this process runs as.
    pub fn current_user_only() -> io::Result<Self> {
        let sddl = pipe_sddl(&current_user_sid()?);
        let wide: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `wide` is a NUL-terminated UTF-16 string that outlives the call;
        // `descriptor` is a valid out-parameter. Windows allocates the descriptor, which `Drop`
        // releases.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(wide.as_ptr()),
                SDDL_REVISION_1,
                &raw mut descriptor,
                None,
            )
        }
        .map_err(io::Error::from)?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(u32::MAX),
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: false.into(),
        };
        Ok(Self {
            descriptor,
            attributes,
        })
    }

    /// The `SECURITY_ATTRIBUTES*` to hand to `CreateNamedPipe` (or tokio's
    /// `create_with_security_attributes_raw`). Valid while `self` is alive.
    pub fn as_attributes_ptr(&mut self) -> *mut c_void {
        (&raw mut self.attributes).cast()
    }
}

impl Drop for PipeSecurity {
    fn drop(&mut self) {
        // SAFETY: the descriptor was allocated by Windows for us and is not used afterwards.
        unsafe { LocalFree(Some(HLOCAL(self.descriptor.0))) };
    }
}

/// The SDDL of the pipe: a protected DACL that gives generic-all to `sid` and to `SYSTEM`.
fn pipe_sddl(sid: &str) -> String {
    format!("D:P(A;;GA;;;{sid})(A;;GA;;;SY)")
}

/// A connection to the daemon: the protocol client over the two ends of the pipe.
pub type DaemonConnection = Client<BufReader<File>, File>;

/// Opens the daemon's pipe `name` and checks who is on the other end.
///
/// `NotFound` means no daemon is running. A pipe whose server runs as another user is refused
/// with `PermissionDenied` before a byte is sent.
pub fn connect_to_daemon(name: &str) -> io::Result<DaemonConnection> {
    let file = open_pipe(name)?;
    verify_server_is_this_user(&file)?;
    let reader = file.try_clone()?;
    Ok(Client::new(BufReader::new(reader), file))
}

fn open_pipe(name: &str) -> io::Result<File> {
    let mut attempts = 0;
    loop {
        let opened = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION)
            .open(name);
        match opened {
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && attempts < 40 => {
                attempts += 1;
                std::thread::sleep(Duration::from_millis(50));
            }
            other => return other,
        }
    }
}

fn verify_server_is_this_user(pipe: &File) -> io::Result<()> {
    let mut pid = 0_u32;
    // SAFETY: `pipe` is an open pipe handle; `pid` is a valid out-parameter.
    unsafe { GetNamedPipeServerProcessId(HANDLE(pipe.as_raw_handle()), &raw mut pid) }
        .map_err(io::Error::from)?;
    // SAFETY: plain call; the handle it returns is owned below.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
        .map_err(io::Error::from)?;
    // SAFETY: `process` is a valid handle that nobody else owns.
    let process = unsafe { OwnedHandle::from_raw_handle(process.0) };
    let server = user_sid_of_process(HANDLE(process.as_raw_handle()))?;
    if server == current_user_sid()? {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the pipe of the Crisol daemon belongs to another account; refusing to talk to it",
        ))
    }
}

/// Starts `daemon` (the path of `crisold.exe`) detached from this console and job, with no
/// window and no standard streams. It does not wait for it.
pub fn spawn_daemon(daemon: &Path) -> io::Result<()> {
    let launch = |flags: u32| {
        Command::new(daemon)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(flags)
            .spawn()
            .map(drop)
    };
    let base = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW;
    // Leaving the job of the terminal keeps the daemon alive when the terminal closes; a job
    // that does not allow it refuses, and then the daemon goes without.
    launch(base | CREATE_BREAKAWAY_FROM_JOB).or_else(|_| launch(base))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pipe_is_for_the_user_and_the_system_only() {
        let sddl = pipe_sddl("S-1-5-21-1-2-3-1001");
        assert_eq!(sddl, "D:P(A;;GA;;;S-1-5-21-1-2-3-1001)(A;;GA;;;SY)");
        assert!(!sddl.contains("WD") && !sddl.contains("BA") && !sddl.contains("AU"));
    }

    #[test]
    fn the_descriptor_for_this_user_can_be_made_and_released() {
        let mut security = PipeSecurity::current_user_only().unwrap();
        assert!(!security.as_attributes_ptr().is_null());
        drop(security);
    }

    #[test]
    fn a_pipe_that_does_not_exist_is_not_found() {
        let error = connect_to_daemon(r"\\.\pipe\crisol-test-nobody-listens-here").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound, "{error}");
    }
}
