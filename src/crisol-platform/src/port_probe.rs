//! Windows implementation of [`PortProbe`].
//!
//! Who is listening comes from the TCP tables of the IP Helper API
//! (`GetExtendedTcpTable`, listeners only, with the owning pid), for IPv4 and IPv6; no
//! `netstat` and no text parsing. If nobody is listening, a short-lived bind on the loopback
//! address (never the wildcard: that would raise the Windows Firewall prompt and create a
//! rule for `crisold.exe`) tells the remaining cases apart:
//!
//! - it works: the port is free;
//! - "address in use": something holds it that the tables did not show;
//! - "access denied": either the port belongs to an excluded range that Windows keeps for
//!   itself (Hyper-V, WSL, Docker), or a program holds it exclusively without listening yet.
//!   Only this rare case asks `netsh` for the excluded ranges to tell them apart.
//!
//! [`PortProbe::is_listening`] only reads the tables: it never binds, so it is safe to call
//! while a service is starting up.
//!
//! Neither call needs elevation. Note that "administered" exclusions (`*` in `netsh`
//! output) do not stop programs from binding explicitly, so those ports are reported free.

use std::io::ErrorKind;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crisol_core::ports::{PortError, PortOwner, PortProbe, PortStatus};
use windows::core::PWSTR;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCP6TABLE_OWNER_PID, MIB_TCPROW_OWNER_PID,
    MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
};
use windows::Win32::Networking::WinSock::{AF_INET, AF_INET6};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, CREATE_NO_WINDOW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};

const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const NO_ERROR: u32 = 0;
/// The table can grow between the size query and the read; retry a few times.
const TABLE_ATTEMPTS: usize = 8;
/// PID of the kernel `System` process, which owns `http.sys` listeners (IIS, `WinRM`, and so on).
const SYSTEM_PID: u32 = 4;

/// Probes TCP ports with the Windows networking APIs.
#[derive(Debug, Default, Clone, Copy)]
pub struct WindowsPortProbe;

impl WindowsPortProbe {
    /// A new probe. It holds no state.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl PortProbe for WindowsPortProbe {
    fn probe(&self, port: u16) -> Result<PortStatus, PortError> {
        let mut owners = listeners(AF_INET.0.into(), port)?;
        owners.extend(listeners(AF_INET6.0.into(), port)?);
        if !owners.is_empty() {
            return Ok(PortStatus::InUse(owners));
        }

        let bind = match TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
            Ok(_listener) => BindOutcome::Bound,
            Err(e) if e.kind() == ErrorKind::AddrInUse => BindOutcome::AddrInUse,
            Err(e) if e.kind() == ErrorKind::PermissionDenied => BindOutcome::Denied,
            Err(e) => BindOutcome::Failed(format!("cannot test port {port}: {e}")),
        };
        classify(owners, bind, || is_excluded(port))
    }

    fn is_listening(&self, port: u16) -> Result<bool, PortError> {
        Ok(!listeners(AF_INET.0.into(), port)?.is_empty()
            || !listeners(AF_INET6.0.into(), port)?.is_empty())
    }
}

/// What a trial bind on the loopback address did.
#[derive(Debug, Clone, PartialEq, Eq)]
enum BindOutcome {
    Bound,
    AddrInUse,
    Denied,
    Failed(String),
}

/// Combines what the TCP tables and the trial bind showed. `excluded` is asked only when the
/// bind was denied and nobody was listed; it says whether Windows excludes the port, or
/// `None` if that could not be found out.
fn classify(
    owners: Vec<PortOwner>,
    bind: BindOutcome,
    excluded: impl FnOnce() -> Option<bool>,
) -> Result<PortStatus, PortError> {
    if !owners.is_empty() {
        return Ok(PortStatus::InUse(owners));
    }
    match bind {
        BindOutcome::Bound => Ok(PortStatus::Free),
        // When in doubt a denied port is not usable either way; call it reserved.
        BindOutcome::Denied if excluded().unwrap_or(true) => Ok(PortStatus::Reserved),
        // In use by something the tables do not show (or exclusively held, not yet listening).
        BindOutcome::AddrInUse | BindOutcome::Denied => Ok(PortStatus::InUse(Vec::new())),
        BindOutcome::Failed(message) => Err(PortError::Os(message)),
    }
}

/// Whether Windows excludes `port` for IPv4 or IPv6 (asks `netsh`; the output is parsed by
/// numbers only, so the system language does not matter). `None` if neither answered.
fn is_excluded(port: u16) -> Option<bool> {
    let mut answered = false;
    for family in ["ipv4", "ipv6"] {
        if let Some(ranges) = excluded_ranges(family) {
            answered = true;
            if ranges
                .iter()
                .any(|(start, end)| (*start..=*end).contains(&port))
            {
                return Some(true);
            }
        }
    }
    answered.then_some(false)
}

/// `netsh.exe` from the system folder. A bare name would be looked up in the folder of the
/// running program first, where nothing of ours should ever be trusted to be `netsh`.
fn netsh_path() -> PathBuf {
    let root =
        std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
    root.join("System32").join("netsh.exe")
}

fn excluded_ranges(family: &str) -> Option<Vec<(u16, u16)>> {
    let output = Command::new(netsh_path())
        .args(["int", family, "show", "excludedportrange", "protocol=tcp"])
        .creation_flags(CREATE_NO_WINDOW.0)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(parse_excluded(&String::from_utf8_lossy(&output.stdout)))
}

/// Pairs of numbers in the lines of the `netsh ... excludedportrange` table.
fn parse_excluded(text: &str) -> Vec<(u16, u16)> {
    text.lines()
        .filter_map(|line| {
            let mut numbers = line
                .split_whitespace()
                .filter_map(|w| w.parse::<u16>().ok());
            let (start, end) = (numbers.next()?, numbers.next()?);
            (start <= end).then_some((start, end))
        })
        .collect()
}

/// Reads the listener table for `family` (`AF_INET` or `AF_INET6`) into 4-byte aligned
/// memory.
fn read_table(family: u32) -> Result<Vec<u32>, PortError> {
    let mut size = 0_u32;
    for _ in 0..TABLE_ATTEMPTS {
        let mut buffer = vec![0_u32; (size as usize).div_ceil(4)];
        let target = if size == 0 {
            None
        } else {
            Some(buffer.as_mut_ptr().cast())
        };
        // SAFETY: `target` is either null (size query) or points to `buffer`, which holds at
        // least `size` bytes; `size` is a valid out-parameter.
        let code = unsafe {
            GetExtendedTcpTable(
                target,
                &raw mut size,
                false,
                family,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            )
        };
        match code {
            NO_ERROR => return Ok(buffer),
            ERROR_INSUFFICIENT_BUFFER => {}
            other => {
                return Err(PortError::Os(format!(
                    "GetExtendedTcpTable failed with code {other}"
                )));
            }
        }
    }
    Err(PortError::Os(
        "the TCP table kept changing while it was read".to_owned(),
    ))
}

/// Every listener on `port` for one address family.
fn listeners(family: u32, port: u16) -> Result<Vec<PortOwner>, PortError> {
    let buffer = read_table(family)?;
    let bytes = buffer.len() * size_of::<u32>();
    let mut found = Vec::new();

    if family == u32::from(AF_INET.0) {
        let table = buffer.as_ptr().cast::<MIB_TCPTABLE_OWNER_PID>();
        // `dwNumEntries` is the first `u32` of the table. An empty buffer has no table at all.
        let Some(&declared) = buffer.first() else {
            return Ok(found);
        };
        let count = (declared as usize)
            .min(bytes.saturating_sub(size_of::<u32>()) / size_of::<MIB_TCPROW_OWNER_PID>());
        // SAFETY: the buffer holds at least one `u32` (checked above) and `count` rows fit in
        // it; `addr_of!` only computes the address of the first row, it reads nothing.
        let rows = unsafe { std::ptr::addr_of!((*table).table) }.cast::<MIB_TCPROW_OWNER_PID>();
        for i in 0..count {
            // SAFETY: `i < count`, so the row is inside the buffer.
            let row = unsafe { rows.add(i).read_unaligned() };
            if network_port(row.dwLocalPort) == port {
                let ip = Ipv4Addr::from(row.dwLocalAddr.to_ne_bytes());
                found.push(owner(row.dwOwningPid, SocketAddr::from((ip, port))));
            }
        }
    } else {
        let table = buffer.as_ptr().cast::<MIB_TCP6TABLE_OWNER_PID>();
        let Some(&declared) = buffer.first() else {
            return Ok(found);
        };
        let count = (declared as usize)
            .min(bytes.saturating_sub(size_of::<u32>()) / size_of::<MIB_TCP6ROW_OWNER_PID>());
        // SAFETY: as above.
        let rows = unsafe { std::ptr::addr_of!((*table).table) }.cast::<MIB_TCP6ROW_OWNER_PID>();
        for i in 0..count {
            // SAFETY: `i < count`, so the row is inside the buffer.
            let row = unsafe { rows.add(i).read_unaligned() };
            if network_port(row.dwLocalPort) == port {
                let ip = Ipv6Addr::from(row.ucLocalAddr);
                found.push(owner(row.dwOwningPid, SocketAddr::from((ip, port))));
            }
        }
    }
    Ok(found)
}

/// The table stores the port in network byte order in the low 16 bits.
fn network_port(raw: u32) -> u16 {
    u16::from_be(u16::try_from(raw & 0xFFFF).unwrap_or(0))
}

fn owner(pid: u32, address: SocketAddr) -> PortOwner {
    PortOwner {
        pid,
        name: process_name(pid),
        address,
    }
}

/// The executable name of `pid`, or `None` if Windows will not tell us.
fn process_name(pid: u32) -> Option<String> {
    match pid {
        0 => return None,
        SYSTEM_PID => return Some("System".to_owned()),
        _ => {}
    }
    // SAFETY: plain call; the returned handle is owned right below.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    // SAFETY: `handle` is a valid handle that nobody else owns.
    let handle = unsafe { OwnedHandle::from_raw_handle(handle.0) };

    let mut buffer = vec![0_u16; 1024];
    let mut length = u32::try_from(buffer.len()).ok()?;
    // SAFETY: `buffer` has `length` UTF-16 units and both outlive the call.
    unsafe {
        QueryFullProcessImageNameW(
            HANDLE(handle.as_raw_handle()),
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &raw mut length,
        )
    }
    .ok()?;
    let path = String::from_utf16_lossy(buffer.get(..length as usize)?);
    Path::new(&path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crisol_core::process::{GroupOptions, ProcessSpec, ProcessSupervisor};
    use std::process::Command;
    use std::time::{Duration, Instant};

    use crate::JobObjectSupervisor;

    fn eventually(seconds: u64, mut condition: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(seconds);
        loop {
            if condition() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// First free port at or after `start` inside the test range 19000-19999. Each test uses
    /// its own starting point so parallel tests do not fight over the same port.
    fn free_port(start: u16) -> u16 {
        assert!((19_000..20_000).contains(&start));
        (start..20_000)
            .find(|p| WindowsPortProbe.probe(*p).is_ok_and(|s| s.is_free()))
            .expect("a free port in 19000-19999")
    }

    fn this_executable() -> String {
        std::env::current_exe()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    fn owners_of(status: PortStatus) -> Vec<PortOwner> {
        match status {
            PortStatus::InUse(owners) => owners,
            other => panic!("expected InUse, got {other:?}"),
        }
    }

    #[test]
    fn a_port_nobody_uses_is_free() {
        let port = free_port(19_000);
        assert_eq!(WindowsPortProbe.probe(port).unwrap(), PortStatus::Free);
    }

    #[test]
    fn a_listener_is_reported_with_its_pid_and_executable_name() {
        let port = free_port(19_100);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).unwrap();

        let owners = owners_of(WindowsPortProbe.probe(port).unwrap());
        assert_eq!(owners.len(), 1, "{owners:?}");
        assert_eq!(owners[0].pid, std::process::id());
        assert_eq!(owners[0].name.as_deref(), Some(this_executable().as_str()));
        assert_eq!(
            owners[0].address,
            SocketAddr::from((Ipv4Addr::LOCALHOST, port))
        );

        drop(listener);
        assert!(eventually(5, || WindowsPortProbe
            .probe(port)
            .unwrap()
            .is_free()));
    }

    #[test]
    fn the_wildcard_and_ipv6_listeners_are_found_too() {
        let port = free_port(19_200);
        let wildcard = TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).unwrap();
        let owners = owners_of(WindowsPortProbe.probe(port).unwrap());
        assert_eq!(
            owners[0].address,
            SocketAddr::from((Ipv4Addr::UNSPECIFIED, port))
        );
        drop(wildcard);
        assert!(eventually(5, || WindowsPortProbe
            .probe(port)
            .unwrap()
            .is_free()));

        let port = free_port(19_300);
        let Ok(v6) = TcpListener::bind((Ipv6Addr::LOCALHOST, port)) else {
            eprintln!("IPv6 loopback is not available here; skipping the IPv6 half");
            return;
        };
        let owners = owners_of(WindowsPortProbe.probe(port).unwrap());
        assert_eq!(
            owners[0].pid,
            std::process::id(),
            "an IPv6-only listener must be seen"
        );
        assert_eq!(
            owners[0].address,
            SocketAddr::from((Ipv6Addr::LOCALHOST, port))
        );
        drop(v6);
    }

    #[test]
    fn is_listening_only_looks_and_never_claims_the_port() {
        let port = free_port(19_300);
        assert!(!WindowsPortProbe.is_listening(port).unwrap());
        // A service that starts between two checks can still bind: nothing was held.
        let service = TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).unwrap();
        assert!(WindowsPortProbe.is_listening(port).unwrap());
        drop(service);
        assert!(eventually(5, || !WindowsPortProbe
            .is_listening(port)
            .unwrap()));
    }

    #[test]
    fn probing_does_not_leave_the_port_taken() {
        let port = free_port(19_400);
        for _ in 0..5 {
            assert!(WindowsPortProbe.probe(port).unwrap().is_free());
        }
        // A real service can still bind it afterwards.
        TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).unwrap();
    }

    #[test]
    fn another_process_is_identified_by_its_own_pid_and_name() {
        let port = free_port(19_500);
        let supervisor = JobObjectSupervisor::new();
        let group = supervisor
            .create_group("listener", GroupOptions::default())
            .unwrap();
        let script = format!(
            "$l = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, {port}); \
             $l.Start(); Start-Sleep -Seconds 60"
        );
        let process = supervisor
            .spawn(
                group,
                &ProcessSpec::new("powershell.exe").args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    &script,
                ]),
            )
            .unwrap();
        let pid = supervisor.pid(process).unwrap();

        let mut seen = Vec::new();
        assert!(
            eventually(20, || {
                seen = match WindowsPortProbe.probe(port).unwrap() {
                    PortStatus::InUse(owners) => owners,
                    _ => Vec::new(),
                };
                !seen.is_empty()
            }),
            "the child never started listening"
        );
        assert_eq!(seen[0].pid, pid);
        assert_eq!(
            seen[0].name.as_deref().map(str::to_lowercase).as_deref(),
            Some("powershell.exe")
        );

        supervisor.kill_group(group).unwrap();
        assert!(
            eventually(10, || WindowsPortProbe.probe(port).unwrap().is_free()),
            "the port must be free after the owner is killed"
        );
    }

    #[test]
    fn suggest_free_skips_a_busy_port() {
        let first = free_port(19_600);
        let _held = TcpListener::bind((Ipv4Addr::LOCALHOST, first)).unwrap();
        let suggestion = WindowsPortProbe
            .suggest_free(first, 20)
            .unwrap()
            .expect("a free port nearby");
        assert!(suggestion > first);
        assert!(WindowsPortProbe.probe(suggestion).unwrap().is_free());
    }

    fn owner_stub(pid: u32) -> PortOwner {
        PortOwner {
            pid,
            name: None,
            address: SocketAddr::from((Ipv4Addr::LOCALHOST, 1)),
        }
    }

    #[test]
    fn a_visible_listener_wins_over_whatever_the_bind_said() {
        for bind in [
            BindOutcome::Bound,
            BindOutcome::AddrInUse,
            BindOutcome::Denied,
        ] {
            let status =
                classify(vec![owner_stub(7)], bind, || panic!("must not ask netsh")).unwrap();
            assert_eq!(status, PortStatus::InUse(vec![owner_stub(7)]));
        }
    }

    #[test]
    fn without_listeners_the_bind_decides() {
        let ask = || -> Option<bool> { panic!("netsh is only for denied binds") };
        assert_eq!(
            classify(vec![], BindOutcome::Bound, ask).unwrap(),
            PortStatus::Free
        );
        assert_eq!(
            classify(vec![], BindOutcome::AddrInUse, ask).unwrap(),
            PortStatus::InUse(vec![])
        );
        let err = classify(vec![], BindOutcome::Failed("boom".into()), ask).unwrap_err();
        assert!(matches!(err, PortError::Os(m) if m == "boom"));
    }

    #[test]
    fn a_denied_bind_is_reserved_only_if_windows_excludes_the_port() {
        let denied = || BindOutcome::Denied;
        assert_eq!(
            classify(vec![], denied(), || Some(true)).unwrap(),
            PortStatus::Reserved
        );
        // Held exclusively by a program that has not started listening.
        assert_eq!(
            classify(vec![], denied(), || Some(false)).unwrap(),
            PortStatus::InUse(vec![])
        );
        // netsh unavailable: still unusable, so say reserved.
        assert_eq!(
            classify(vec![], denied(), || None).unwrap(),
            PortStatus::Reserved
        );
    }

    #[test]
    fn netsh_output_is_parsed_by_numbers_in_any_language() {
        let english = "Protocol tcp Port Exclusion Ranges\n\nStart Port    End Port\n----------    --------\n      5002        5002\n     50000       50059     *\n\n* - Administered port exclusions.\n";
        let spanish = "Protocolo tcp Intervalos de exclusi\u{f3}n de puertos\n\nPuerto de inicio    Puerto final\n----------          --------\n      5002        5002\n     50000       50059     *\n\n* - Exclusiones de puertos administrados.\n";
        for text in [english, spanish] {
            assert_eq!(
                parse_excluded(text),
                vec![(5002, 5002), (50000, 50059)],
                "{text}"
            );
        }
        assert_eq!(parse_excluded(""), vec![]);
        assert_eq!(
            parse_excluded("10 5\n"),
            vec![],
            "an inverted range is not a range"
        );
    }

    #[test]
    fn a_port_windows_refuses_to_bind_is_never_reported_free() {
        let out = Command::new("netsh")
            .args(["int", "ipv4", "show", "excludedportrange", "protocol=tcp"])
            .output()
            .expect("netsh is part of Windows");
        let ranges = parse_excluded(&String::from_utf8_lossy(&out.stdout));

        let mut checked = 0;
        for (start, end) in ranges {
            for port in [start, end] {
                let denied = TcpListener::bind((Ipv4Addr::UNSPECIFIED, port))
                    .is_err_and(|e| e.kind() == ErrorKind::PermissionDenied);
                if denied {
                    let status = WindowsPortProbe.probe(port).unwrap();
                    assert!(
                        !status.is_free(),
                        "port {port} cannot be bound yet was reported free"
                    );
                    checked += 1;
                }
            }
        }
        eprintln!("checked {checked} port(s) that Windows refuses to bind");
    }
}
