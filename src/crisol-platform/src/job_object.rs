//! Windows implementation of [`ProcessSupervisor`] with Job Objects.
//!
//! One Job Object per group:
//!
//! - **No orphans.** The job is created with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, so when
//!   the last handle to it closes (the group is closed, the supervisor is dropped, or the
//!   daemon dies) Windows kills every process in it. The flag can be turned off per group.
//! - **The whole tree.** Children of a process in a job join the job, so killing the job
//!   kills grandchildren too.
//! - **No race.** A process is created *suspended*, put in the job, and only then resumed,
//!   so it cannot spawn anything before it is contained.
//! - **No console window** (`CREATE_NO_WINDOW`), and stdout/stderr always go to the log
//!   file or to nothing; they are never left attached to a console.
//!
//! Services run as the current user. Nothing here asks for elevation.

use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::mem::size_of;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::thread;

use crisol_core::log::{pump, RotatingLog};
use crisol_core::process::{
    ExitReason, GroupId, GroupOptions, ProcessId, ProcessSpec, ProcessStatus, ProcessSupervisor,
    SupervisorError,
};
use windows::core::PCWSTR;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicAccountingInformation,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows::Win32::System::Threading::{
    OpenThread, ResumeThread, CREATE_NO_WINDOW, CREATE_SUSPENDED, THREAD_SUSPEND_RESUME,
};

/// Exit code given to processes killed through the job.
const KILLED_EXIT_CODE: u32 = 1;

/// A Job Object handle; closed on drop.
#[derive(Debug)]
struct Job(OwnedHandle);

impl Job {
    fn raw(&self) -> HANDLE {
        HANDLE(self.0.as_raw_handle())
    }
}

#[derive(Debug)]
struct Group {
    options: GroupOptions,
    job: Job,
    killed: bool,
    processes: Vec<ProcessId>,
}

#[derive(Debug)]
struct Process {
    pid: u32,
    child: Child,
    /// Final status, remembered once seen so it never changes afterwards.
    finished: Option<ProcessStatus>,
}

#[derive(Debug, Default)]
struct State {
    next_group: u64,
    next_process: u64,
    groups: BTreeMap<GroupId, Group>,
    processes: BTreeMap<ProcessId, Process>,
    /// Open log files by path. Weak, so a file closes once its last pump ends.
    logs: HashMap<PathBuf, Weak<Mutex<RotatingLog>>>,
}

/// Supervises processes with Windows Job Objects.
#[derive(Debug, Default)]
pub struct JobObjectSupervisor {
    state: Mutex<State>,
}

impl JobObjectSupervisor {
    /// A supervisor with no groups.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn os_error(what: &str, error: &windows::core::Error) -> SupervisorError {
    SupervisorError::Os(format!("{what}: {error}"))
}

fn size_of_u32<T>() -> u32 {
    u32::try_from(size_of::<T>()).unwrap_or(u32::MAX)
}

/// Creates a Job Object, with kill-on-close if asked.
fn create_job(options: GroupOptions) -> Result<Job, SupervisorError> {
    // SAFETY: no security attributes and no name; the call has no pointer arguments.
    let handle = unsafe { CreateJobObjectW(None, PCWSTR::null()) }
        .map_err(|e| os_error("CreateJobObject", &e))?;
    // SAFETY: `handle` is a valid, newly created handle that nobody else owns.
    let job = Job(unsafe { OwnedHandle::from_raw_handle(handle.0) });

    if options.kill_on_close {
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `info` is a valid, initialized struct of the size passed alongside it and
        // lives for the whole call.
        unsafe {
            SetInformationJobObject(
                job.raw(),
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&info).cast(),
                size_of_u32::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>(),
            )
        }
        .map_err(|e| os_error("SetInformationJobObject", &e))?;
    }
    Ok(job)
}

/// Resumes the threads of a process created suspended.
fn resume_process(pid: u32) -> Result<(), SupervisorError> {
    // SAFETY: plain snapshot request; the returned handle is owned right below.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }
        .map_err(|e| os_error("CreateToolhelp32Snapshot", &e))?;
    // SAFETY: `snapshot` is a valid handle that nobody else owns.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot.0) };
    let snapshot_handle = HANDLE(snapshot.as_raw_handle());

    let mut entry = THREADENTRY32 {
        dwSize: size_of_u32::<THREADENTRY32>(),
        ..Default::default()
    };
    let mut resumed = 0_u32;
    // SAFETY: `entry.dwSize` is set and `entry` outlives the call.
    let mut more = unsafe { Thread32First(snapshot_handle, &mut entry) }.is_ok();
    while more {
        if entry.th32OwnerProcessID == pid {
            // SAFETY: plain call; the handle is owned right below.
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, false, entry.th32ThreadID) }
                .map_err(|e| os_error("OpenThread", &e))?;
            // SAFETY: `thread` is a valid handle that nobody else owns.
            let thread = unsafe { OwnedHandle::from_raw_handle(thread.0) };
            // SAFETY: `thread` is a valid thread handle with suspend/resume access.
            if unsafe { ResumeThread(HANDLE(thread.as_raw_handle())) } == u32::MAX {
                return Err(SupervisorError::Os(format!(
                    "ResumeThread failed for thread {}",
                    entry.th32ThreadID
                )));
            }
            resumed += 1;
        }
        // SAFETY: same as for `Thread32First`.
        more = unsafe { Thread32Next(snapshot_handle, &mut entry) }.is_ok();
    }
    if resumed == 0 {
        return Err(SupervisorError::Os(format!(
            "no thread found to resume for pid {pid}"
        )));
    }
    Ok(())
}

type SharedLog = Arc<Mutex<RotatingLog>>;

/// The writer for the spec's log file, shared with every other process that logs to the same
/// path so that they never rotate the file under each other. `None` if there is no log.
fn shared_log(
    logs: &mut HashMap<PathBuf, Weak<Mutex<RotatingLog>>>,
    spec: &ProcessSpec,
) -> Result<Option<SharedLog>, SupervisorError> {
    let Some(path) = &spec.log_file else {
        return Ok(None);
    };
    if let Some(existing) = logs.get(path).and_then(Weak::upgrade) {
        return Ok(Some(existing));
    }
    let log =
        RotatingLog::open(path, spec.log_rotation).map_err(|source| SupervisorError::Spawn {
            program: spec.program.clone(),
            source,
        })?;
    let log = Arc::new(Mutex::new(log));
    logs.retain(|_, weak| weak.strong_count() > 0);
    logs.insert(path.clone(), Arc::downgrade(&log));
    Ok(Some(log))
}

/// Starts a thread that copies `reader` into the log until the pipe closes. The thread
/// always drains the pipe, even if the log cannot be written, so the process never blocks.
fn start_pump(
    name: &str,
    reader: impl Read + Send + 'static,
    log: SharedLog,
) -> Result<(), SupervisorError> {
    thread::Builder::new()
        .name(format!("crisol-log-{name}"))
        .spawn(move || {
            pump(reader, |bytes| {
                log.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .write(bytes)
            });
        })
        .map(|_| ())
        .map_err(|e| SupervisorError::Os(format!("cannot start the log thread: {e}")))
}

/// Starts the threads that move the child's stdout and stderr into the shared log.
fn start_pumps(
    child: &mut Child,
    log: Option<&SharedLog>,
    spec: &ProcessSpec,
) -> Result<(), SupervisorError> {
    let Some(log) = log else {
        return Ok(());
    };
    let name = spec
        .program
        .file_stem()
        .map_or_else(|| "process".into(), |n| n.to_string_lossy().into_owned());
    if let Some(stdout) = child.stdout.take() {
        start_pump(&name, stdout, Arc::clone(log))?;
    }
    if let Some(stderr) = child.stderr.take() {
        start_pump(&name, stderr, Arc::clone(log))?;
    }
    Ok(())
}

/// Refreshes and returns the status of a process.
fn poll(process: &mut Process) -> ProcessStatus {
    if let Some(status) = process.finished {
        return status;
    }
    match process.child.try_wait() {
        Ok(Some(exit)) => {
            let status = ProcessStatus::Exited(ExitReason::Code(exit.code().unwrap_or(-1)));
            process.finished = Some(status);
            status
        }
        // Still running, or the OS could not tell: treat as running.
        Ok(None) | Err(_) => ProcessStatus::Running,
    }
}

impl ProcessSupervisor for JobObjectSupervisor {
    fn create_group(&self, _name: &str, options: GroupOptions) -> Result<GroupId, SupervisorError> {
        let job = create_job(options)?;
        let mut state = self.state();
        state.next_group += 1;
        let id = GroupId::new(state.next_group);
        state.groups.insert(
            id,
            Group {
                options,
                job,
                killed: false,
                processes: Vec::new(),
            },
        );
        Ok(id)
    }

    fn spawn(&self, group: GroupId, spec: &ProcessSpec) -> Result<ProcessId, SupervisorError> {
        let mut state = self.state();
        let entry = state
            .groups
            .get(&group)
            .ok_or(SupervisorError::UnknownGroup(group))?;
        if entry.killed {
            return Err(SupervisorError::GroupKilled(group));
        }
        let job = entry.job.raw();

        let log = shared_log(&mut state.logs, spec)?;
        let piped = || {
            if log.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            }
        };
        let mut command = Command::new(&spec.program);
        command
            .args(&spec.args)
            .envs(spec.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null())
            .stdout(piped())
            .stderr(piped())
            .creation_flags((CREATE_NO_WINDOW | CREATE_SUSPENDED).0);
        if let Some(cwd) = &spec.cwd {
            command.current_dir(cwd);
        }
        let mut child = command.spawn().map_err(|source| SupervisorError::Spawn {
            program: spec.program.clone(),
            source,
        })?;

        // Contain it before it runs a single instruction, wire its output, and only then let
        // it run.
        let contained = {
            let process = HANDLE(child.as_raw_handle());
            // SAFETY: both handles are valid; `child` owns the process handle for the call.
            unsafe { AssignProcessToJobObject(job, process) }
                .map_err(|e| os_error("AssignProcessToJobObject", &e))
                .and_then(|()| start_pumps(&mut child, log.as_ref(), spec))
                .and_then(|()| resume_process(child.id()))
        };
        if let Err(error) = contained {
            // Never leave a suspended or uncontained process behind.
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }

        state.next_process += 1;
        let id = ProcessId::new(state.next_process);
        let pid = child.id();
        state.processes.insert(
            id,
            Process {
                pid,
                child,
                finished: None,
            },
        );
        if let Some(entry) = state.groups.get_mut(&group) {
            entry.processes.push(id);
        }
        Ok(id)
    }

    fn pid(&self, process: ProcessId) -> Result<u32, SupervisorError> {
        self.state()
            .processes
            .get(&process)
            .map(|p| p.pid)
            .ok_or(SupervisorError::UnknownProcess(process))
    }

    fn status(&self, process: ProcessId) -> Result<ProcessStatus, SupervisorError> {
        let mut state = self.state();
        let entry = state
            .processes
            .get_mut(&process)
            .ok_or(SupervisorError::UnknownProcess(process))?;
        Ok(poll(entry))
    }

    fn active_processes(&self, group: GroupId) -> Result<usize, SupervisorError> {
        let state = self.state();
        let entry = state
            .groups
            .get(&group)
            .ok_or(SupervisorError::UnknownGroup(group))?;
        let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: `info` is a valid struct of the size passed alongside it and outlives the
        // call; the job handle is valid while the group exists.
        unsafe {
            QueryInformationJobObject(
                Some(entry.job.raw()),
                JobObjectBasicAccountingInformation,
                std::ptr::from_mut(&mut info).cast(),
                size_of_u32::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>(),
                None,
            )
        }
        .map_err(|e| os_error("QueryInformationJobObject", &e))?;
        Ok(info.ActiveProcesses as usize)
    }

    fn kill_group(&self, group: GroupId) -> Result<(), SupervisorError> {
        let mut state = self.state();
        kill(&mut state, group)
    }

    fn close_group(&self, group: GroupId) -> Result<(), SupervisorError> {
        let mut state = self.state();
        let entry = state
            .groups
            .get(&group)
            .ok_or(SupervisorError::UnknownGroup(group))?;
        if entry.options.kill_on_close {
            kill(&mut state, group)?;
        }
        // Dropping the group closes the job handle: with kill-on-close Windows now kills
        // anything left; without it the processes keep running, detached from Crisol.
        if let Some(removed) = state.groups.remove(&group) {
            for id in removed.processes {
                state.processes.remove(&id);
            }
        }
        Ok(())
    }
}

/// Kills the job of `group` and records how each process ended.
fn kill(state: &mut State, group: GroupId) -> Result<(), SupervisorError> {
    let entry = state
        .groups
        .get_mut(&group)
        .ok_or(SupervisorError::UnknownGroup(group))?;
    if entry.killed {
        return Ok(());
    }

    // Remember natural exits first, so they keep their own exit code.
    let ids = entry.processes.clone();
    let job = entry.job.raw();
    for id in &ids {
        if let Some(process) = state.processes.get_mut(id) {
            poll(process);
        }
    }

    // SAFETY: the job handle is valid while the group exists.
    unsafe { TerminateJobObject(job, KILLED_EXIT_CODE) }
        .map_err(|e| os_error("TerminateJobObject", &e))?;

    for id in &ids {
        if let Some(process) = state.processes.get_mut(id) {
            if process.finished.is_none() {
                process.finished = Some(ProcessStatus::Exited(ExitReason::Killed));
            }
        }
    }
    if let Some(entry) = state.groups.get_mut(&group) {
        entry.killed = true;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crisol_core::log::LogRotation;
    use std::fs;
    use std::path::Path;
    use std::time::{Duration, Instant};
    use tempfile::TempDir;

    /// Polls until `condition` holds or `seconds` pass. Returns whether it held.
    fn eventually(seconds: u64, mut condition: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(seconds);
        loop {
            if condition() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Counts live `cmd`/`ping` processes whose command line contains `marker`, asking
    /// Windows directly (not the Job Object API) so the check is independent.
    fn live_with_marker(marker: &str) -> usize {
        let script = format!(
            "(Get-CimInstance Win32_Process | Where-Object {{ $_.CommandLine -like '*{marker}*' \
             -and $_.Name -in @('ping.exe','cmd.exe') }} | Measure-Object).Count"
        );
        let output = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .output()
            .expect("powershell is available on Windows");
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .unwrap_or(usize::MAX)
    }

    /// `ping` to a unique loopback address: a harmless process that lives about a minute.
    fn sleeper(marker: &str) -> ProcessSpec {
        ProcessSpec::new("ping.exe").args(["-n", "60", marker])
    }

    /// `cmd` that starts a background grandchild and then waits on a second child.
    fn tree(marker: &str) -> ProcessSpec {
        let line = format!("start /b ping -n 60 {marker} >nul & ping -n 60 {marker} >nul");
        ProcessSpec::new("cmd.exe").args(["/c".to_owned(), line])
    }

    fn group(s: &JobObjectSupervisor) -> GroupId {
        s.create_group("test", GroupOptions::default()).unwrap()
    }

    /// Waits for the process to end and for every `needle` to reach the log (a pump
    /// thread copies the output, so it lands a moment after the process exits).
    fn log_after_exit(
        s: &JobObjectSupervisor,
        id: ProcessId,
        file: &Path,
        needles: &[&str],
    ) -> String {
        assert!(
            eventually(10, || s.status(id).unwrap() != ProcessStatus::Running),
            "the process should have finished"
        );
        let mut text = String::new();
        eventually(10, || {
            text = fs::read_to_string(file).unwrap_or_default();
            needles.iter().all(|n| text.contains(n))
        });
        text
    }

    #[test]
    fn killing_the_group_leaves_no_process_of_the_tree_alive() {
        let marker = "127.0.0.71";
        let s = JobObjectSupervisor::new();
        let g = group(&s);
        let parent = s.spawn(g, &tree(marker)).unwrap();

        assert!(
            eventually(10, || s.active_processes(g).unwrap() >= 3),
            "the parent and its two children should be in the job"
        );
        assert!(
            live_with_marker(marker) >= 3,
            "Windows should see the whole tree"
        );

        s.kill_group(g).unwrap();
        assert!(
            eventually(10, || s.active_processes(g).unwrap() == 0),
            "the job must empty out"
        );
        assert_eq!(
            live_with_marker(marker),
            0,
            "no process of the tree may survive"
        );
        assert_eq!(
            s.status(parent).unwrap(),
            ProcessStatus::Exited(ExitReason::Killed)
        );
        s.kill_group(g).unwrap();
    }

    #[test]
    fn children_started_immediately_are_still_contained() {
        // The parent forks at once; this fails if it can run before joining the job.
        for round in 0..5 {
            let marker = format!("127.0.0.{}", 80 + round);
            let s = JobObjectSupervisor::new();
            let g = group(&s);
            s.spawn(g, &tree(&marker)).unwrap();
            s.kill_group(g).unwrap();
            assert!(
                eventually(10, || s.active_processes(g).unwrap() == 0),
                "round {round}"
            );
            assert_eq!(
                live_with_marker(&marker),
                0,
                "round {round}: a child escaped"
            );
        }
    }

    #[test]
    fn closing_a_group_kills_the_tree_by_default() {
        let marker = "127.0.0.72";
        let s = JobObjectSupervisor::new();
        let g = group(&s);
        s.spawn(g, &tree(marker)).unwrap();
        assert!(eventually(10, || s.active_processes(g).unwrap() >= 3));

        s.close_group(g).unwrap();
        assert!(
            eventually(10, || live_with_marker(marker) == 0),
            "close must kill the tree"
        );
        assert!(matches!(
            s.active_processes(g),
            Err(SupervisorError::UnknownGroup(_))
        ));
    }

    #[test]
    fn dropping_the_supervisor_kills_everything_it_started() {
        let marker = "127.0.0.73";
        let s = JobObjectSupervisor::new();
        let g = group(&s);
        s.spawn(g, &tree(marker)).unwrap();
        assert!(eventually(10, || s.active_processes(g).unwrap() >= 3));

        drop(s); // the same thing that happens when the daemon dies
        assert!(
            eventually(10, || live_with_marker(marker) == 0),
            "kill-on-close must leave no orphans"
        );
    }

    #[test]
    fn a_group_without_kill_on_close_lets_its_process_survive() {
        let marker = "127.0.0.74";
        let s = JobObjectSupervisor::new();
        let g = s
            .create_group(
                "detached",
                GroupOptions {
                    kill_on_close: false,
                },
            )
            .unwrap();
        let p = s.spawn(g, &sleeper(marker)).unwrap();
        let pid = s.pid(p).unwrap();

        s.close_group(g).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        let alive = live_with_marker(marker);

        // Clean up before asserting.
        let _ = Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .output();
        assert_eq!(alive, 1, "the service should have outlived its group");
        assert!(eventually(10, || live_with_marker(marker) == 0));
    }

    #[test]
    fn a_killed_group_refuses_new_processes() {
        let s = JobObjectSupervisor::new();
        let g = group(&s);
        s.kill_group(g).unwrap();
        let err = s.spawn(g, &ProcessSpec::new("cmd.exe")).unwrap_err();
        assert!(
            matches!(err, SupervisorError::GroupKilled(id) if id == g),
            "{err}"
        );
    }

    #[test]
    fn a_process_that_exits_reports_its_code_and_keeps_it_after_a_kill() {
        let s = JobObjectSupervisor::new();
        let g = group(&s);
        let p = s
            .spawn(g, &ProcessSpec::new("cmd.exe").args(["/c", "exit", "7"]))
            .unwrap();
        assert!(eventually(10, || s.status(p).unwrap() != ProcessStatus::Running));
        assert_eq!(
            s.status(p).unwrap(),
            ProcessStatus::Exited(ExitReason::Code(7))
        );
        s.kill_group(g).unwrap();
        assert_eq!(
            s.status(p).unwrap(),
            ProcessStatus::Exited(ExitReason::Code(7))
        );
    }

    #[test]
    fn a_missing_program_is_a_spawn_error_and_leaves_the_group_usable() {
        let s = JobObjectSupervisor::new();
        let g = group(&s);
        let err = s
            .spawn(g, &ProcessSpec::new("C:/definitely/not/here.exe"))
            .unwrap_err();
        match err {
            SupervisorError::Spawn { source, .. } => {
                assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
            }
            other => panic!("unexpected error: {other}"),
        }
        assert_eq!(s.active_processes(g).unwrap(), 0);
        let p = s
            .spawn(g, &ProcessSpec::new("cmd.exe").args(["/c", "exit", "0"]))
            .unwrap();
        assert!(eventually(10, || s.status(p).unwrap() != ProcessStatus::Running));
    }

    #[test]
    fn output_goes_to_the_log_file_and_is_appended() {
        let dir = TempDir::new().unwrap();
        let log = dir.path().join("logs").join("svc").join("runtime.log");
        let s = JobObjectSupervisor::new();
        let g = group(&s);

        let first = ProcessSpec::new("cmd.exe")
            .args(["/c", "echo hello-out& echo hello-err 1>&2"])
            .log_file(&log);
        let p = s.spawn(g, &first).unwrap();
        let text = log_after_exit(&s, p, &log, &["hello-out", "hello-err"]);
        assert!(text.contains("hello-out"), "{text}");
        assert!(
            text.contains("hello-err"),
            "stderr must be captured too: {text}"
        );

        let second = ProcessSpec::new("cmd.exe")
            .args(["/c", "echo second-run"])
            .log_file(&log);
        let p = s.spawn(g, &second).unwrap();
        let text = log_after_exit(&s, p, &log, &["hello-out", "second-run"]);
        assert!(
            text.contains("hello-out") && text.contains("second-run"),
            "{text}"
        );
    }

    #[test]
    fn environment_and_working_directory_are_applied() {
        let dir = TempDir::new().unwrap();
        let work = dir.path().join("work");
        fs::create_dir(&work).unwrap();
        let log = dir.path().join("out.log");
        let s = JobObjectSupervisor::new();
        let g = group(&s);

        let spec = ProcessSpec::new("cmd.exe")
            .args(["/c", "echo %CRISOL_TEST_VAR%& cd"])
            .env("CRISOL_TEST_VAR", "bar-value")
            .cwd(&work)
            .log_file(&log);
        let p = s.spawn(g, &spec).unwrap();
        let text = log_after_exit(&s, p, &log, &["bar-value", "work"]);
        assert!(text.contains("bar-value"), "{text}");
        assert!(text.contains("work"), "{text}");
    }

    #[test]
    fn a_chatty_service_rotates_its_log_and_never_blocks() {
        let dir = TempDir::new().unwrap();
        let log = dir.path().join("logs").join("chatty").join("runtime.log");
        let s = JobObjectSupervisor::new();
        let g = group(&s);

        // About 300 lines of ~60 bytes (~18 KB) into a log that rotates at 2 KB, keeping 2.
        let line =
            "for /l %i in (1,1,300) do @echo line-%i-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";
        let spec = ProcessSpec::new("cmd.exe")
            .args(["/c", line])
            .log_file(&log)
            .log_rotation(LogRotation {
                max_bytes: 2000,
                keep: 2,
            });
        let p = s.spawn(g, &spec).unwrap();
        let text = log_after_exit(&s, p, &log, &["line-300-"]);

        let rotated = |n| log.with_file_name(format!("runtime.log.{n}"));
        assert!(
            rotated(1).is_file() && rotated(2).is_file(),
            "history should exist"
        );
        assert!(!rotated(3).exists(), "only `keep` rotated files may remain");
        assert!(
            text.contains("line-300-"),
            "the newest line is in the current file"
        );
        for file in [log.clone(), rotated(1), rotated(2)] {
            let size = fs::metadata(&file).unwrap().len();
            assert!(
                size <= 2000 + 16 * 1024,
                "{} is too large: {size}",
                file.display()
            );
        }
        // 300 lines would be ~18 KB in one file; the cap really bounds the disk use.
        let total: u64 = [log.clone(), rotated(1), rotated(2)]
            .iter()
            .map(|f| fs::metadata(f).unwrap().len())
            .sum();
        assert!(
            total < 8000,
            "the three files should hold far less than the full output: {total}"
        );
    }

    #[test]
    fn processes_sharing_a_log_file_share_one_writer() {
        let dir = TempDir::new().unwrap();
        let log = dir.path().join("runtime.log");
        let s = JobObjectSupervisor::new();
        let g = group(&s);

        let a = s
            .spawn(
                g,
                &ProcessSpec::new("cmd.exe")
                    .args(["/c", "echo from-a"])
                    .log_file(&log),
            )
            .unwrap();
        let b = s
            .spawn(
                g,
                &ProcessSpec::new("cmd.exe")
                    .args(["/c", "echo from-b"])
                    .log_file(&log),
            )
            .unwrap();
        let text = log_after_exit(&s, a, &log, &["from-a", "from-b"]);
        assert!(eventually(10, || s.status(b).unwrap() != ProcessStatus::Running));
        assert!(text.contains("from-a") && text.contains("from-b"), "{text}");
    }

    #[test]
    fn a_log_that_cannot_be_opened_is_a_spawn_error() {
        let dir = TempDir::new().unwrap();
        // A directory where the log file should be.
        let log = dir.path().join("runtime.log");
        fs::create_dir(&log).unwrap();
        let s = JobObjectSupervisor::new();
        let g = group(&s);
        let err = s
            .spawn(
                g,
                &ProcessSpec::new("cmd.exe")
                    .args(["/c", "echo x"])
                    .log_file(&log),
            )
            .unwrap_err();
        assert!(matches!(err, SupervisorError::Spawn { .. }), "{err}");
        assert_eq!(s.active_processes(g).unwrap(), 0, "nothing may be started");
    }

    #[test]
    fn groups_are_independent_and_unknown_ids_fail() {
        let s = JobObjectSupervisor::new();
        let a = group(&s);
        let b = group(&s);
        let in_a = s.spawn(a, &sleeper("127.0.0.75")).unwrap();
        let in_b = s.spawn(b, &sleeper("127.0.0.76")).unwrap();

        s.kill_group(a).unwrap();
        assert_eq!(
            s.status(in_a).unwrap(),
            ProcessStatus::Exited(ExitReason::Killed)
        );
        assert_eq!(s.status(in_b).unwrap(), ProcessStatus::Running);
        assert_eq!(s.active_processes(b).unwrap(), 1);

        assert!(matches!(
            s.spawn(GroupId::new(999), &ProcessSpec::new("x")),
            Err(SupervisorError::UnknownGroup(_))
        ));
        assert!(matches!(
            s.status(ProcessId::new(999)),
            Err(SupervisorError::UnknownProcess(_))
        ));
        s.close_group(b).unwrap();
        assert!(eventually(10, || live_with_marker("127.0.0.76") == 0));
    }
}
