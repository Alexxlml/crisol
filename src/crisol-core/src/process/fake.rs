//! An in-memory [`ProcessSupervisor`] for tests.
//!
//! It starts nothing. Tests script what happens (a process exits, a spawn fails, a service
//! grows grandchildren) and inspect what was asked of it. Every consumer of the supervisor
//! trait (service lifecycle, watchdog, daemon) is tested against this double.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use super::{
    ExitReason, GroupId, GroupOptions, ProcessId, ProcessSpec, ProcessStatus, ProcessSupervisor,
    SupervisorError,
};

/// First fake pid handed out. Easy to recognise in assertions and logs.
pub const FIRST_PID: u32 = 1000;

#[derive(Debug, PartialEq, Eq)]
enum GroupState {
    Active,
    Killed,
}

#[derive(Debug)]
struct Group {
    name: String,
    options: GroupOptions,
    state: GroupState,
    /// Grandchildren and deeper: alive processes the supervisor did not spawn itself.
    descendants: usize,
}

#[derive(Debug)]
struct Process {
    group: GroupId,
    pid: u32,
    status: ProcessStatus,
}

/// One call to `spawn` that succeeded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnRecord {
    /// Group it was spawned into.
    pub group: GroupId,
    /// Handle returned to the caller.
    pub process: ProcessId,
    /// Fake pid.
    pub pid: u32,
    /// Exactly what was requested.
    pub spec: ProcessSpec,
}

#[derive(Debug, Default)]
struct State {
    next_group: u64,
    next_process: u64,
    next_pid: u32,
    groups: BTreeMap<GroupId, Group>,
    processes: BTreeMap<ProcessId, Process>,
    spawned: Vec<SpawnRecord>,
    /// Pids still running after their group was closed without `kill_on_close`.
    detached: Vec<u32>,
    /// Groups whose processes survive every kill (a stuck service).
    unkillable: Vec<GroupId>,
    /// A spawn failure still to happen: (spawns that succeed first, the error).
    fail_spawn: Option<(usize, SupervisorError)>,
}

/// Scriptable fake of [`ProcessSupervisor`].
#[derive(Debug)]
pub struct FakeSupervisor {
    state: Mutex<State>,
}

impl Default for FakeSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeSupervisor {
    /// A supervisor with no groups.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State {
                next_pid: FIRST_PID,
                ..State::default()
            }),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        // A panic in another test thread must not hide this test's real failure.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Makes the next `spawn` fail with `error`. Only that one call is affected.
    pub fn fail_next_spawn(&self, error: SupervisorError) {
        self.fail_spawn_after(0, error);
    }

    /// Lets `successes` more spawns succeed, then makes the one after them fail with `error`.
    pub fn fail_spawn_after(&self, successes: usize, error: SupervisorError) {
        self.state().fail_spawn = Some((successes, error));
    }

    /// Simulates the process exiting on its own with `code`. Ignored if it already ended.
    pub fn exit(&self, process: ProcessId, code: i32) -> Result<(), SupervisorError> {
        let mut state = self.state();
        let entry = state
            .processes
            .get_mut(&process)
            .ok_or(SupervisorError::UnknownProcess(process))?;
        if entry.status == ProcessStatus::Running {
            entry.status = ProcessStatus::Exited(ExitReason::Code(code));
        }
        Ok(())
    }

    /// Makes the processes of `group` ignore `kill_group` and `close_group`, like a service
    /// stuck in the kernel. Only for testing how callers cope with a stop that fails.
    pub fn make_unkillable(&self, group: GroupId) {
        self.state().unkillable.push(group);
    }

    /// Simulates `count` more alive descendants (children of the service's own children).
    pub fn add_descendants(&self, group: GroupId, count: usize) -> Result<(), SupervisorError> {
        let mut state = self.state();
        let entry = state
            .groups
            .get_mut(&group)
            .ok_or(SupervisorError::UnknownGroup(group))?;
        entry.descendants += count;
        Ok(())
    }

    /// Every successful spawn, in order.
    #[must_use]
    pub fn spawned(&self) -> Vec<SpawnRecord> {
        self.state().spawned.clone()
    }

    /// The name a group was created with.
    #[must_use]
    pub fn group_name(&self, group: GroupId) -> Option<String> {
        self.state().groups.get(&group).map(|g| g.name.clone())
    }

    /// Fake pids that are still running, including those of closed groups that were
    /// allowed to survive (`kill_on_close = false`).
    #[must_use]
    pub fn alive_pids(&self) -> Vec<u32> {
        let state = self.state();
        let mut pids: Vec<u32> = state
            .processes
            .values()
            .filter(|p| p.status == ProcessStatus::Running)
            .map(|p| p.pid)
            .collect();
        pids.extend(&state.detached);
        pids
    }

    /// Number of groups that exist (created and not closed).
    #[must_use]
    pub fn group_count(&self) -> usize {
        self.state().groups.len()
    }
}

fn kill_all(state: &mut State, group: GroupId) {
    if state.unkillable.contains(&group) {
        return;
    }
    for process in state.processes.values_mut().filter(|p| p.group == group) {
        if process.status == ProcessStatus::Running {
            process.status = ProcessStatus::Exited(ExitReason::Killed);
        }
    }
    if let Some(entry) = state.groups.get_mut(&group) {
        entry.descendants = 0;
        entry.state = GroupState::Killed;
    }
}

impl ProcessSupervisor for FakeSupervisor {
    fn create_group(&self, name: &str, options: GroupOptions) -> Result<GroupId, SupervisorError> {
        let mut state = self.state();
        state.next_group += 1;
        let id = GroupId::new(state.next_group);
        state.groups.insert(
            id,
            Group {
                name: name.to_owned(),
                options,
                state: GroupState::Active,
                descendants: 0,
            },
        );
        Ok(id)
    }

    fn spawn(&self, group: GroupId, spec: &ProcessSpec) -> Result<ProcessId, SupervisorError> {
        let mut state = self.state();
        match state.groups.get(&group) {
            None => return Err(SupervisorError::UnknownGroup(group)),
            Some(g) if g.state == GroupState::Killed => {
                return Err(SupervisorError::GroupKilled(group));
            }
            Some(_) => {}
        }
        match state.fail_spawn.take() {
            Some((0, error)) => return Err(error),
            Some((successes, error)) => state.fail_spawn = Some((successes - 1, error)),
            None => {}
        }

        state.next_process += 1;
        let id = ProcessId::new(state.next_process);
        let pid = state.next_pid;
        state.next_pid += 1;
        state.processes.insert(
            id,
            Process {
                group,
                pid,
                status: ProcessStatus::Running,
            },
        );
        state.spawned.push(SpawnRecord {
            group,
            process: id,
            pid,
            spec: spec.clone(),
        });
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
        self.state()
            .processes
            .get(&process)
            .map(|p| p.status)
            .ok_or(SupervisorError::UnknownProcess(process))
    }

    fn active_processes(&self, group: GroupId) -> Result<usize, SupervisorError> {
        let state = self.state();
        let entry = state
            .groups
            .get(&group)
            .ok_or(SupervisorError::UnknownGroup(group))?;
        let own = state
            .processes
            .values()
            .filter(|p| p.group == group && p.status == ProcessStatus::Running)
            .count();
        Ok(own + entry.descendants)
    }

    fn kill_group(&self, group: GroupId) -> Result<(), SupervisorError> {
        let mut state = self.state();
        if !state.groups.contains_key(&group) {
            return Err(SupervisorError::UnknownGroup(group));
        }
        kill_all(&mut state, group);
        Ok(())
    }

    fn close_group(&self, group: GroupId) -> Result<(), SupervisorError> {
        let mut state = self.state();
        let entry = state
            .groups
            .get(&group)
            .ok_or(SupervisorError::UnknownGroup(group))?;
        if entry.options.kill_on_close {
            kill_all(&mut state, group);
        } else {
            let survivors: Vec<u32> = state
                .processes
                .values()
                .filter(|p| p.group == group && p.status == ProcessStatus::Running)
                .map(|p| p.pid)
                .collect();
            state.detached.extend(survivors);
        }
        // Ids of a closed group are invalid either way.
        state.processes.retain(|_, p| p.group != group);
        state.groups.remove(&group);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::sync::Arc;
    use std::thread;

    fn group(s: &FakeSupervisor, name: &str) -> GroupId {
        s.create_group(name, GroupOptions::default()).unwrap()
    }

    fn spawn(s: &FakeSupervisor, g: GroupId, program: &str) -> ProcessId {
        s.spawn(g, &ProcessSpec::new(program)).unwrap()
    }

    #[test]
    fn a_spawned_process_is_running_with_a_fresh_pid() {
        let s = FakeSupervisor::new();
        let g = group(&s, "nginx");
        let a = spawn(&s, g, "nginx.exe");
        let b = spawn(&s, g, "php-cgi.exe");
        assert_eq!(s.status(a).unwrap(), ProcessStatus::Running);
        assert_eq!(s.pid(a).unwrap(), FIRST_PID);
        assert_eq!(s.pid(b).unwrap(), FIRST_PID + 1);
        assert_ne!(a, b);
        assert_eq!(s.active_processes(g).unwrap(), 2);
        assert_eq!(s.group_name(g).as_deref(), Some("nginx"));
    }

    #[test]
    fn the_spec_is_recorded_exactly() {
        let s = FakeSupervisor::new();
        let g = group(&s, "php");
        let spec = ProcessSpec::new("C:/bin/php-cgi.exe")
            .args(["-b", "127.0.0.1:9100"])
            .arg("-c")
            .arg("php.ini")
            .cwd("C:/www")
            .env("PHP_FCGI_MAX_REQUESTS", "500")
            .log_file("C:/logs/php/runtime.log");
        let id = s.spawn(g, &spec).unwrap();
        let records = s.spawned();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].spec, spec);
        assert_eq!(records[0].process, id);
        assert_eq!(records[0].group, g);
        assert_eq!(spec.args.len(), 4);
    }

    #[test]
    fn a_process_that_exits_stops_counting_as_active() {
        let s = FakeSupervisor::new();
        let g = group(&s, "redis");
        let p = spawn(&s, g, "redis-server");
        s.exit(p, 3).unwrap();
        assert_eq!(
            s.status(p).unwrap(),
            ProcessStatus::Exited(ExitReason::Code(3))
        );
        assert_eq!(s.active_processes(g).unwrap(), 0);
        // A second exit does not rewrite history.
        s.exit(p, 9).unwrap();
        assert_eq!(
            s.status(p).unwrap(),
            ProcessStatus::Exited(ExitReason::Code(3))
        );
    }

    #[test]
    fn killing_a_group_kills_its_processes_and_every_descendant() {
        let s = FakeSupervisor::new();
        let g = group(&s, "php");
        let workers: Vec<_> = (0..4).map(|_| spawn(&s, g, "php-cgi.exe")).collect();
        s.add_descendants(g, 3).unwrap();
        assert_eq!(s.active_processes(g).unwrap(), 7);

        s.kill_group(g).unwrap();
        assert_eq!(
            s.active_processes(g).unwrap(),
            0,
            "the whole tree must be gone"
        );
        for w in workers {
            assert_eq!(
                s.status(w).unwrap(),
                ProcessStatus::Exited(ExitReason::Killed)
            );
        }
        assert_eq!(s.alive_pids(), Vec::<u32>::new());
    }

    #[test]
    fn killing_keeps_the_exit_code_of_processes_that_already_ended() {
        let s = FakeSupervisor::new();
        let g = group(&s, "mixed");
        let done = spawn(&s, g, "a");
        let live = spawn(&s, g, "b");
        s.exit(done, 0).unwrap();
        s.kill_group(g).unwrap();
        assert_eq!(
            s.status(done).unwrap(),
            ProcessStatus::Exited(ExitReason::Code(0))
        );
        assert_eq!(
            s.status(live).unwrap(),
            ProcessStatus::Exited(ExitReason::Killed)
        );
    }

    #[test]
    fn killing_twice_is_fine_and_a_killed_group_refuses_new_processes() {
        let s = FakeSupervisor::new();
        let g = group(&s, "nginx");
        spawn(&s, g, "nginx.exe");
        s.kill_group(g).unwrap();
        s.kill_group(g).unwrap();
        let err = s.spawn(g, &ProcessSpec::new("nginx.exe")).unwrap_err();
        assert!(
            matches!(err, SupervisorError::GroupKilled(id) if id == g),
            "{err}"
        );
        assert_eq!(s.spawned().len(), 1, "a refused spawn is not recorded");
    }

    #[test]
    fn unknown_ids_are_reported() {
        let s = FakeSupervisor::new();
        let ghost_group = GroupId::new(99);
        let ghost_process = ProcessId::new(99);
        assert!(matches!(
            s.spawn(ghost_group, &ProcessSpec::new("x")),
            Err(SupervisorError::UnknownGroup(_))
        ));
        assert!(matches!(
            s.kill_group(ghost_group),
            Err(SupervisorError::UnknownGroup(_))
        ));
        assert!(matches!(
            s.close_group(ghost_group),
            Err(SupervisorError::UnknownGroup(_))
        ));
        assert!(matches!(
            s.active_processes(ghost_group),
            Err(SupervisorError::UnknownGroup(_))
        ));
        assert!(matches!(
            s.status(ghost_process),
            Err(SupervisorError::UnknownProcess(_))
        ));
        assert!(matches!(
            s.pid(ghost_process),
            Err(SupervisorError::UnknownProcess(_))
        ));
        assert!(matches!(
            s.exit(ghost_process, 0),
            Err(SupervisorError::UnknownProcess(_))
        ));
    }

    #[test]
    fn closing_a_group_kills_it_by_default_and_invalidates_its_ids() {
        let s = FakeSupervisor::new();
        let g = group(&s, "mysql");
        let p = spawn(&s, g, "mysqld");
        s.close_group(g).unwrap();
        assert!(
            s.alive_pids().is_empty(),
            "kill_on_close must leave no orphans"
        );
        assert_eq!(s.group_count(), 0);
        assert!(matches!(
            s.status(p),
            Err(SupervisorError::UnknownProcess(_))
        ));
        assert!(matches!(
            s.active_processes(g),
            Err(SupervisorError::UnknownGroup(_))
        ));
    }

    #[test]
    fn a_group_without_kill_on_close_lets_its_processes_survive() {
        let s = FakeSupervisor::new();
        let g = s
            .create_group(
                "detached",
                GroupOptions {
                    kill_on_close: false,
                },
            )
            .unwrap();
        let p = spawn(&s, g, "mailpit");
        let pid = s.pid(p).unwrap();
        s.close_group(g).unwrap();
        assert_eq!(s.alive_pids(), vec![pid], "the service outlives its group");
        assert!(matches!(
            s.status(p),
            Err(SupervisorError::UnknownProcess(_))
        ));
    }

    #[test]
    fn a_scripted_spawn_failure_hits_one_call_only() {
        let s = FakeSupervisor::new();
        let g = group(&s, "nginx");
        s.fail_next_spawn(SupervisorError::Spawn {
            program: "nginx.exe".into(),
            source: io::Error::from(io::ErrorKind::NotFound),
        });
        let err = s.spawn(g, &ProcessSpec::new("nginx.exe")).unwrap_err();
        assert!(matches!(err, SupervisorError::Spawn { .. }), "{err}");
        assert!(err.to_string().contains("nginx.exe"));
        assert_eq!(s.active_processes(g).unwrap(), 0);
        assert_eq!(s.spawned(), Vec::new());

        spawn(&s, g, "nginx.exe");
        assert_eq!(s.active_processes(g).unwrap(), 1);
    }

    #[test]
    fn groups_are_independent() {
        let s = FakeSupervisor::new();
        let nginx = group(&s, "nginx");
        let redis = group(&s, "redis");
        let n = spawn(&s, nginx, "nginx.exe");
        let r = spawn(&s, redis, "redis-server");
        s.kill_group(nginx).unwrap();
        assert_eq!(
            s.status(n).unwrap(),
            ProcessStatus::Exited(ExitReason::Killed)
        );
        assert_eq!(s.status(r).unwrap(), ProcessStatus::Running);
        assert_eq!(s.active_processes(redis).unwrap(), 1);
    }

    #[test]
    fn it_is_usable_as_a_shared_trait_object_across_threads() {
        let supervisor: Arc<dyn ProcessSupervisor> = Arc::new(FakeSupervisor::new());
        let g = supervisor
            .create_group("shared", GroupOptions::default())
            .unwrap();
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let supervisor = Arc::clone(&supervisor);
                thread::spawn(move || supervisor.spawn(g, &ProcessSpec::new("w")).unwrap())
            })
            .collect();
        let mut ids: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 8, "every process gets its own id");
        assert_eq!(supervisor.active_processes(g).unwrap(), 8);
        supervisor.kill_group(g).unwrap();
        assert_eq!(supervisor.active_processes(g).unwrap(), 0);
    }
}
