//! Orchestration tests: temporary home, the process and port fakes, a made-up clock.

use std::fs;
use std::net::{Ipv4Addr, SocketAddr};

use tempfile::TempDir;

use super::*;
use crate::ports::fake::FakePortProbe;
use crate::ports::PortOwner;
use crate::process::fake::FakeSupervisor;
use crate::project::TrustAllOwners;
use crate::sites::SitesFile;

const HTTP: u16 = 19080;

struct Rig {
    _tmp: TempDir,
    paths: CrisolPaths,
    config: Config,
    catalog: Catalog,
    supervisor: Arc<FakeSupervisor>,
    probe: Arc<FakePortProbe>,
    stack: WebStack,
    t0: Instant,
}

impl Rig {
    fn new(php: &[&str]) -> Self {
        let tmp = TempDir::new().unwrap();
        let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
        let mut config = Config::default();
        config.services.nginx.http_port = HTTP;
        config.php.base_port = 19100;
        config.php.workers = 2;
        for version in php {
            fs::create_dir_all(paths.bin_for("php", version).unwrap()).unwrap();
        }
        let nginx_dir = paths.bin_for("nginx", "1.28.3").unwrap();
        fs::create_dir_all(&nginx_dir).unwrap();
        let supervisor = Arc::new(FakeSupervisor::new());
        let probe = Arc::new(FakePortProbe::new());
        let stack = WebStack::new(
            paths.clone(),
            &config,
            nginx_dir,
            supervisor.clone(),
            probe.clone(),
        );
        Self {
            _tmp: tmp,
            paths,
            config,
            catalog: Catalog::parse("schema = 1\nserial = 1\n").unwrap(),
            supervisor,
            probe,
            stack,
            t0: Instant::now(),
        }
    }

    fn at(&self, ms: u64) -> Instant {
        self.t0 + Duration::from_millis(ms)
    }

    /// A parked site with an optional `.php-version`.
    fn site(&self, name: &str, php: Option<&str>) {
        let dir = self.paths.www_dir().join(name);
        fs::create_dir_all(&dir).unwrap();
        if let Some(php) = php {
            fs::write(dir.join(".php-version"), format!("{php}\n")).unwrap();
        }
    }

    fn plan(&self) -> WebPlan {
        let none = |_: &str| None;
        plan_web(
            &self.paths,
            &self.config,
            &SitesFile::default(),
            &self.catalog,
            &TrustAllOwners,
            &none,
        )
    }

    /// These ports get a listener (what the real processes would do once they are up).
    fn listen(&self, ports: &[u16]) {
        for &port in ports {
            self.probe.occupy(
                port,
                PortOwner {
                    pid: 1,
                    name: Some("x".to_owned()),
                    address: SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
                },
            );
        }
    }

    fn tick(&mut self, ms: u64) -> Vec<WebEvent> {
        let now = self.at(ms);
        self.stack.tick(now)
    }

    /// Every control command (`-t`, `-s reload`) that was started, with its arguments after
    /// `-c <conf> -p <dir>`. Their groups are closed once they finish, so they are found by
    /// what they ran.
    fn controls(&self) -> Vec<(crate::process::ProcessId, Vec<String>)> {
        self.supervisor
            .spawned()
            .into_iter()
            .filter_map(|r| {
                let args: Vec<String> = r
                    .spec
                    .args
                    .iter()
                    .skip(4)
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect();
                (args == ["-t"] || args == ["-s", "reload"]).then_some((r.process, args))
            })
            .collect()
    }

    fn control_commands(&self) -> Vec<Vec<String>> {
        self.controls().into_iter().map(|(_, args)| args).collect()
    }

    fn last_control(&self) -> crate::process::ProcessId {
        self.controls().pop().unwrap().0
    }

    fn started(&self, group: &str) -> usize {
        self.supervisor
            .spawned()
            .into_iter()
            .filter(|r| self.supervisor.group_name(r.group).as_deref() == Some(group))
            .count()
    }

    /// Starts the stack with the current sites and brings everything up.
    fn run(&mut self) {
        let plan = self.plan();
        self.stack.apply(&plan, self.at(0)).unwrap();
        self.stack.start(self.at(0));
        let mut ports = vec![HTTP];
        for version in plan.php_versions().keys() {
            ports.extend(self.stack.pool_ports(version).unwrap());
        }
        self.listen(&ports);
        self.tick(600);
        assert_eq!(self.stack.nginx_state(), Some(&ServiceState::Running));
    }
}

fn versions(plan: &WebPlan) -> Vec<(String, Option<String>)> {
    plan.sites
        .iter()
        .map(|s| {
            (
                s.site.name.clone(),
                s.php.as_ref().map(|v| v.version.to_string()),
            )
        })
        .collect()
}

// ---- planning ---------------------------------------------------------------------

#[test]
fn each_site_gets_the_php_the_shims_would_run_in_its_folder() {
    let rig = Rig::new(&["8.3.35", "8.4.26"]);
    rig.site("old", Some("8.3"));
    rig.site("new", None);
    rig.site("pinned", Some("8.4"));
    let plan = rig.plan();
    assert_eq!(
        versions(&plan),
        [
            ("new".to_owned(), Some("8.4.26".to_owned())),
            ("old".to_owned(), Some("8.3.35".to_owned())),
            ("pinned".to_owned(), Some("8.4.26".to_owned())),
        ]
    );
    assert_eq!(plan.warnings, Vec::<String>::new());
    assert_eq!(
        plan.php_versions().keys().collect::<Vec<_>>(),
        ["8.3.35", "8.4.26"]
    );
}

#[test]
fn a_site_that_wants_a_php_that_is_not_installed_is_still_served_without_php() {
    let rig = Rig::new(&["8.4.26"]);
    rig.site("legacy", Some("5.6"));
    rig.site("fine", None);
    let plan = rig.plan();
    assert_eq!(
        versions(&plan),
        [
            ("fine".to_owned(), Some("8.4.26".to_owned())),
            ("legacy".to_owned(), None)
        ]
    );
    assert_eq!(plan.warnings.len(), 1, "{:?}", plan.warnings);
    assert!(plan.warnings[0].contains("legacy.test") && plan.warnings[0].contains("5.6"));
}

#[test]
fn with_no_php_installed_every_site_is_static() {
    let rig = Rig::new(&[]);
    rig.site("a", None);
    let plan = rig.plan();
    assert_eq!(versions(&plan), [("a".to_owned(), None)]);
    assert_eq!(plan.warnings.len(), 1);
    assert!(plan.php_versions().is_empty());
}

// ---- applying and starting --------------------------------------------------------

#[test]
fn applying_a_plan_to_a_stopped_stack_writes_the_files_and_runs_nothing() {
    let mut rig = Rig::new(&["8.4.26"]);
    rig.site("shop", None);
    let plan = rig.plan();
    let applied = rig.stack.apply(&plan, rig.at(0)).unwrap();
    assert_eq!(applied.new_pools, ["8.4.26"]);
    assert!(!applied.reload_requested);
    let etc = rig.paths.etc_for("nginx").unwrap();
    assert!(etc.join("nginx.conf").is_file() && etc.join("sites").join("shop.conf").is_file());
    assert_eq!(rig.supervisor.spawned().len(), 0);
    assert!(!rig.stack.is_running());
}

#[test]
fn starting_runs_nginx_and_one_pool_per_php_version_in_use() {
    let mut rig = Rig::new(&["8.3.35", "8.4.26"]);
    rig.site("a", Some("8.3"));
    rig.site("b", Some("8.4"));
    rig.run();
    assert_eq!(rig.started("nginx"), 1);
    assert_eq!(rig.started("php-8.3.35"), 2, "two workers each");
    assert_eq!(rig.started("php-8.4.26"), 2);
    assert_eq!(rig.stack.pool_ports("8.3.35"), Some(&[19100, 19101][..]));
    assert_eq!(rig.stack.pool_ports("8.4.26"), Some(&[19102, 19103][..]));
    assert_eq!(rig.stack.pool_state("8.4.26"), Some(&ServiceState::Running));
}

#[test]
fn the_nginx_process_gets_the_generated_file_and_the_installation_as_prefix() {
    let mut rig = Rig::new(&[]);
    rig.site("a", None);
    rig.run();
    let record = rig
        .supervisor
        .spawned()
        .into_iter()
        .find(|r| rig.supervisor.group_name(r.group).as_deref() == Some("nginx"))
        .unwrap();
    let args: Vec<String> = record
        .spec
        .args
        .iter()
        .map(|a| a.to_string_lossy().replace('\\', "/"))
        .collect();
    assert_eq!(args[0], "-c");
    assert!(args[1].ends_with("etc/nginx/nginx.conf"), "{args:?}");
    assert_eq!(args[2], "-p");
    assert!(args[3].ends_with("bin/nginx/1.28.3"), "{args:?}");
}

// ---- changing sites while running -------------------------------------------------

#[test]
fn a_new_site_with_a_new_php_starts_its_pool_then_tests_then_reloads_in_that_order() {
    let mut rig = Rig::new(&["8.3.35", "8.4.26"]);
    rig.site("shop", Some("8.4"));
    rig.run();

    rig.site("legacy", Some("8.3"));
    let plan = rig.plan();
    let applied = rig.stack.apply(&plan, rig.at(1000)).unwrap();
    assert_eq!(applied.new_pools, ["8.3.35"]);
    assert!(applied.reload_requested);
    assert_eq!(rig.started("php-8.3.35"), 2, "the new pool starts at once");
    assert_eq!(rig.control_commands(), Vec::<Vec<String>>::new());

    // The new pool is not up yet: Nginx is not told about it.
    assert_eq!(rig.tick(1200), vec![]);
    assert_eq!(rig.control_commands(), Vec::<Vec<String>>::new());

    // It comes up: the configuration is tested first.
    rig.listen(&[19102, 19103]);
    rig.tick(1700);
    assert_eq!(rig.control_commands(), [["-t"]]);
    // The test passes: only now the reload is requested.
    rig.supervisor.exit(rig.last_control(), 0).unwrap();
    let events = rig.tick(2000);
    assert_eq!(events, vec![]);
    assert_eq!(rig.control_commands(), [vec!["-t"], vec!["-s", "reload"]]);
    rig.supervisor.exit(rig.last_control(), 0).unwrap();
    let events = rig.tick(2300);
    assert_eq!(events, vec![WebEvent::Reloaded]);
    // Nginx itself was never restarted.
    assert_eq!(rig.started("nginx"), 1);
}

#[test]
fn a_configuration_nginx_rejects_is_reported_and_nothing_is_reloaded() {
    let mut rig = Rig::new(&["8.4.26"]);
    rig.site("shop", None);
    rig.run();
    rig.site("blog", None);
    let plan = rig.plan();
    rig.stack.apply(&plan, rig.at(1000)).unwrap();
    rig.tick(1100);
    assert_eq!(rig.control_commands(), [["-t"]]);

    rig.supervisor.exit(rig.last_control(), 1).unwrap();
    let events = rig.tick(1300);
    assert_eq!(events.len(), 1);
    match &events[0] {
        WebEvent::ReloadFailed { reason } => {
            assert!(
                reason.contains("rejected") && reason.contains("reload.log"),
                "{reason}"
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(rig.control_commands().len(), 1, "no reload was sent");
    assert_eq!(rig.stack.nginx_state(), Some(&ServiceState::Running));
}

#[test]
fn a_reload_that_takes_forever_gives_up() {
    let mut rig = Rig::new(&["8.4.26"]);
    rig.site("shop", None);
    rig.run();
    rig.site("blog", None);
    let plan = rig.plan();
    rig.stack.apply(&plan, rig.at(1000)).unwrap();
    rig.tick(1100);
    let events = rig.tick(1100 + 21_000);
    assert!(
        matches!(events.as_slice(), [WebEvent::ReloadFailed { reason }] if reason.contains("did not finish")),
        "{events:?}"
    );
}

#[test]
fn changes_made_during_a_reload_are_merged_into_one_more_reload() {
    let mut rig = Rig::new(&["8.4.26"]);
    rig.site("a", None);
    rig.run();

    rig.site("b", None);
    let plan = rig.plan();
    rig.stack.apply(&plan, rig.at(1000)).unwrap();
    rig.tick(1100); // -t running
    rig.site("c", None);
    rig.site("d", None);
    let plan = rig.plan();
    rig.stack.apply(&plan, rig.at(1200)).unwrap();
    let again = rig.stack.apply(&plan, rig.at(1250)).unwrap();
    assert!(
        !again.reload_requested,
        "the same plan again changes nothing"
    );

    rig.supervisor.exit(rig.last_control(), 0).unwrap();
    rig.tick(1400); // -s reload
    rig.supervisor.exit(rig.last_control(), 0).unwrap();
    let events = rig.tick(1600);
    assert_eq!(events, vec![WebEvent::Reloaded, WebEvent::ReloadStarted]);

    // The second round: test, reload, done. Three sites changed, one extra round.
    rig.tick(1700);
    rig.supervisor.exit(rig.last_control(), 0).unwrap();
    rig.tick(1900);
    rig.supervisor.exit(rig.last_control(), 0).unwrap();
    let events = rig.tick(2100);
    assert_eq!(events, vec![WebEvent::Reloaded]);
    assert_eq!(
        rig.control_commands().len(),
        4,
        "two rounds of test + reload, no more"
    );
}

#[test]
fn a_pool_nobody_uses_any_more_is_stopped_after_the_reload_not_before() {
    let mut rig = Rig::new(&["8.3.35", "8.4.26"]);
    rig.site("old", Some("8.3"));
    rig.site("new", Some("8.4"));
    rig.run();

    // The only 8.3 site goes away.
    fs::remove_dir_all(rig.paths.www_dir().join("old")).unwrap();
    let plan = rig.plan();
    let applied = rig.stack.apply(&plan, rig.at(1000)).unwrap();
    assert_eq!(applied.retired_pools, ["8.3.35"]);
    assert!(applied.reload_requested);
    assert_eq!(
        rig.stack.pool_state("8.3.35"),
        Some(&ServiceState::Running),
        "still serving until Nginx stops sending it requests"
    );

    rig.tick(1100);
    rig.supervisor.exit(rig.last_control(), 0).unwrap();
    rig.tick(1300);
    rig.supervisor.exit(rig.last_control(), 0).unwrap();
    rig.tick(1500);
    assert_eq!(rig.stack.pool_state("8.3.35"), None, "gone from the stack");
    // Its processes are being stopped.
    rig.tick(1700);
    rig.tick(2500);
    assert_eq!(rig.stack.pool_state("8.4.26"), Some(&ServiceState::Running));
    // Nginx and the two 8.4 workers are what is left.
    assert_eq!(rig.supervisor.alive_pids().len(), 3);
}

#[test]
fn a_pool_added_later_never_moves_the_ports_of_a_running_one() {
    let mut rig = Rig::new(&["8.3.35", "8.4.26"]);
    rig.site("new", Some("8.4"));
    rig.run();
    assert_eq!(rig.stack.pool_ports("8.4.26"), Some(&[19100, 19101][..]));

    // 8.3 sorts before 8.4, but it takes the next free block.
    rig.site("old", Some("8.3"));
    let plan = rig.plan();
    rig.stack.apply(&plan, rig.at(1000)).unwrap();
    assert_eq!(rig.stack.pool_ports("8.4.26"), Some(&[19100, 19101][..]));
    assert_eq!(rig.stack.pool_ports("8.3.35"), Some(&[19102, 19103][..]));
}

#[test]
fn a_change_that_alters_nothing_reloads_nothing() {
    let mut rig = Rig::new(&["8.4.26"]);
    rig.site("shop", None);
    rig.run();
    let plan = rig.plan();
    let applied = rig.stack.apply(&plan, rig.at(1000)).unwrap();
    assert!(!applied.reload_requested);
    assert_eq!(applied.nginx.changed, Vec::<PathBuf>::new());
    assert_eq!(rig.control_commands(), Vec::<Vec<String>>::new());
}

// ---- stopping ---------------------------------------------------------------------

#[test]
fn stopping_stops_everything_and_cancels_a_reload_under_way() {
    let mut rig = Rig::new(&["8.4.26"]);
    rig.site("shop", None);
    rig.run();
    rig.site("blog", None);
    let plan = rig.plan();
    rig.stack.apply(&plan, rig.at(1000)).unwrap();
    rig.tick(1100);
    assert_eq!(rig.control_commands().len(), 1);

    rig.stack.stop(rig.at(1200));
    assert!(!rig.stack.is_running());
    for ms in [1300, 1600, 2000, 6000, 6100, 11_000] {
        rig.tick(ms);
    }
    assert!(rig.stack.is_stopped());
    assert_eq!(
        rig.supervisor.alive_pids().len(),
        0,
        "nothing is left behind"
    );
    assert_eq!(rig.supervisor.group_count(), 0);
    assert_eq!(rig.control_commands().len(), 1, "no reload after the stop");
}
