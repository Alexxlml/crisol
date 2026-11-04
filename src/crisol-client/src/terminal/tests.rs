use std::fs;
use std::io;
use std::sync::Mutex;

use crisol_core::shell::ShellLaunch;
use crisol_core::shims::{shim_file_name, SHIM_COMMANDS};
use tempfile::TempDir;

use super::*;

/// A desktop with the terminals it is told to have; it records what was started.
struct FakeDesktop {
    has: Vec<Terminal>,
    started: Mutex<Vec<ShellLaunch>>,
    fail_to_start: bool,
}

impl FakeDesktop {
    fn with(has: &[Terminal]) -> Self {
        Self {
            has: has.to_vec(),
            started: Mutex::default(),
            fail_to_start: false,
        }
    }
}

impl Desktop for FakeDesktop {
    fn find_terminal(&self, terminal: Terminal, _parent: Option<&str>) -> Option<PathBuf> {
        self.has
            .contains(&terminal)
            .then(|| PathBuf::from(format!(r"C:\fake\{}.exe", terminal.name())))
    }

    fn run_terminal(&self, _launch: &ShellLaunch) -> io::Result<i32> {
        unreachable!("the GUI never waits for a terminal")
    }

    fn spawn_terminal(&self, launch: &ShellLaunch) -> io::Result<()> {
        if self.fail_to_start {
            return Err(io::Error::other("no way"));
        }
        self.started.lock().unwrap().push(launch.clone());
        Ok(())
    }

    fn open_url(&self, _url: &str) -> io::Result<()> {
        unreachable!()
    }

    fn pick_folder(&self, _owner: Option<isize>) -> io::Result<Option<PathBuf>> {
        unreachable!()
    }
}

/// A home with the shims in place and a `www` folder for the projects.
fn home() -> (TempDir, CrisolPaths) {
    let tmp = TempDir::new().unwrap();
    let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
    paths.ensure_layout().unwrap();
    fs::create_dir_all(paths.shims_dir()).unwrap();
    for command in SHIM_COMMANDS {
        fs::write(paths.shims_dir().join(shim_file_name(command)), b"MZ").unwrap();
    }
    fs::create_dir_all(paths.home().join("www")).unwrap();
    (tmp, paths)
}

fn config(text: &str) -> Config {
    Config::parse(text).unwrap().config
}

#[test]
fn the_list_has_every_terminal_marked_by_whether_the_computer_has_it() {
    let desktop = FakeDesktop::with(&[Terminal::Cmd, Terminal::WindowsPowerShell]);
    let options = terminal_options(&desktop, &config(""));
    let found: Vec<_> = options.options.iter().map(|o| (o.name, o.found)).collect();
    assert_eq!(
        found,
        [
            ("pwsh", false),
            ("powershell", true),
            ("cmd", true),
            ("bash", false)
        ]
    );
    assert_eq!(options.chosen, None);
    // Nothing chosen: the automatic order, and nothing to warn about.
    assert_eq!(options.effective, Some("powershell"));
    assert_eq!(options.warning, None);
}

#[test]
fn a_chosen_terminal_that_is_there_is_the_one_that_opens() {
    let desktop = FakeDesktop::with(&Terminal::ALL);
    let options = terminal_options(&desktop, &config("[shell]\nterminal = \"bash\"\n"));
    assert_eq!(options.chosen, Some("bash"));
    assert_eq!(options.effective, Some("bash"));
    assert_eq!(options.warning, None);
}

#[test]
fn a_chosen_terminal_that_is_gone_is_warned_about_and_the_automatic_one_is_used() {
    let desktop = FakeDesktop::with(&[Terminal::Cmd]);
    let options = terminal_options(&desktop, &config("[shell]\nterminal = \"pwsh\"\n"));
    assert_eq!(options.chosen, Some("pwsh"));
    assert_eq!(options.effective, Some("cmd"));
    assert!(options.warning.unwrap().contains("pwsh"));
}

#[test]
fn with_no_terminal_at_all_nothing_opens_and_it_is_said() {
    let options = terminal_options(&FakeDesktop::with(&[]), &config(""));
    assert_eq!(options.effective, None);
    assert!(options.warning.is_some());
}

#[test]
fn choosing_writes_one_key_and_automatic_takes_it_out() {
    let (_tmp, paths) = home();
    fs::write(paths.config_file(), "# mine\n[sites]\ntld = \"test\"\n").unwrap();
    choose_terminal(&paths, Some("pwsh")).unwrap();
    let text = fs::read_to_string(paths.config_file()).unwrap();
    assert!(text.starts_with("# mine\n"), "{text}");
    assert!(text.contains("terminal = \"pwsh\""), "{text}");
    choose_terminal(&paths, None).unwrap();
    assert!(!fs::read_to_string(paths.config_file())
        .unwrap()
        .contains("terminal"));
}

#[test]
fn a_name_that_is_not_a_terminal_writes_nothing() {
    let (_tmp, paths) = home();
    let error = choose_terminal(&paths, Some("wt")).unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidParams);
    assert!(!paths.config_file().exists());
}

#[test]
fn the_projects_folder_is_the_first_parked_one_that_exists() {
    let (tmp, paths) = home();
    let projects = tmp.path().join("Proyectos");
    fs::create_dir_all(&projects).unwrap();
    let text = format!(
        "[sites]\nparked = [{:?}, \"www\"]\n",
        projects.to_str().unwrap()
    );
    assert_eq!(projects_folder(&paths, &config(&text)), projects);

    // The first one is gone: the next.
    let gone = tmp.path().join("borrada");
    let text = format!(
        "[sites]\nparked = [{:?}, \"www\"]\n",
        gone.to_str().unwrap()
    );
    assert_eq!(
        projects_folder(&paths, &config(&text)),
        paths.home().join("www")
    );
}

#[test]
fn by_default_the_projects_folder_is_www_and_the_home_is_the_last_resort() {
    let (_tmp, paths) = home();
    assert_eq!(
        projects_folder(&paths, &config("")),
        paths.home().join("www")
    );
    fs::remove_dir(paths.home().join("www")).unwrap();
    assert_eq!(projects_folder(&paths, &config("")), paths.home());
}

#[test]
fn a_project_opens_in_its_own_folder() {
    let (tmp, paths) = home();
    let shop = tmp.path().join("shop");
    fs::create_dir_all(&shop).unwrap();
    let sites = vec![("shop".to_owned(), shop.clone())];
    assert_eq!(
        start_folder(&Place::Site("shop".into()), &sites, &paths, &config("")).unwrap(),
        shop
    );
}

#[test]
fn a_project_that_is_gone_is_an_error_not_another_folder() {
    let (tmp, paths) = home();
    let sites = vec![("old".to_owned(), tmp.path().join("deleted"))];
    for name in ["old", "never-was"] {
        let error =
            start_folder(&Place::Site(name.into()), &sites, &paths, &config("")).unwrap_err();
        assert_eq!(error.code, ErrorCode::NotFound, "{name}");
    }
}

#[test]
fn the_mark_of_a_crisol_shell_is_not_carried_to_the_new_window() {
    let env = clean_env([
        ("Path".into(), "C:\\Windows".into()),
        ("crisol_shell".into(), "1".into()),
        ("CRISOL_SHELL".into(), "1".into()),
    ]);
    assert_eq!(
        env,
        [(OsString::from("Path"), OsString::from("C:\\Windows"))]
    );
}

#[test]
fn the_terminal_opens_in_a_new_window_in_the_folder_with_the_shims_in_front() {
    let (tmp, paths) = home();
    let shop = tmp.path().join("shop");
    fs::create_dir_all(&shop).unwrap();
    let desktop = FakeDesktop::with(&Terminal::ALL);
    let env = clean_env([("Path".into(), r"C:\laragon\bin\php;C:\Windows".into())]);

    let opened = launch_terminal(
        &desktop,
        &paths,
        &env,
        &Place::Site("shop".into()),
        &[("shop".to_owned(), shop.clone())],
    )
    .unwrap();

    // No preference: the first of the automatic order, with nothing to explain.
    assert_eq!((opened.terminal, opened.note), ("pwsh", None));
    assert_eq!(opened.folder, shop);
    let started = desktop.started.lock().unwrap();
    assert_eq!(started.len(), 1);
    let launch = &started[0];
    assert_eq!(launch.cwd.as_deref(), Some(shop.as_path()));
    let (_, path) = launch.set.iter().find(|(n, _)| n == "PATH").unwrap();
    let shims = paths.shims_dir().to_string_lossy().into_owned();
    assert!(path.to_string_lossy().starts_with(&shims), "{path:?}");
}

#[test]
fn the_chosen_terminal_opens_and_the_general_button_opens_where_the_projects_are() {
    let (_tmp, paths) = home();
    fs::write(paths.config_file(), "[shell]\nterminal = \"cmd\"\n").unwrap();
    let desktop = FakeDesktop::with(&Terminal::ALL);
    let opened = launch_terminal(&desktop, &paths, &[], &Place::Projects, &[]).unwrap();
    assert_eq!(opened.terminal, "cmd");
    assert_eq!(opened.folder, paths.home().join("www"));
    assert_eq!(
        desktop.started.lock().unwrap()[0].program,
        PathBuf::from(r"C:\fake\cmd.exe")
    );
}

#[test]
fn a_chosen_terminal_that_is_gone_still_opens_one_and_says_so() {
    let (_tmp, paths) = home();
    fs::write(paths.config_file(), "[shell]\nterminal = \"bash\"\n").unwrap();
    let desktop = FakeDesktop::with(&[Terminal::Cmd]);
    let opened = launch_terminal(&desktop, &paths, &[], &Place::Projects, &[]).unwrap();
    assert_eq!(opened.terminal, "cmd");
    assert!(opened.note.unwrap().contains("bash"));
    // Nothing was rewritten.
    assert_eq!(
        fs::read_to_string(paths.config_file()).unwrap(),
        "[shell]\nterminal = \"bash\"\n"
    );
}

#[test]
fn a_name_that_is_not_a_terminal_is_shown_as_automatic_with_the_mistake() {
    let desktop = FakeDesktop::with(&[Terminal::Cmd]);
    let options = terminal_options(&desktop, &config("[shell]\nterminal = \"wt\"\n"));
    assert_eq!(options.chosen, None);
    assert_eq!(options.unknown.as_deref(), Some("wt"));
    assert_eq!(options.effective, Some("cmd"));
    let warning = options.warning.unwrap();
    assert!(
        warning.contains("\"wt\"") && warning.contains("opening cmd"),
        "{warning}"
    );
}

#[test]
fn a_name_that_is_not_a_terminal_still_opens_one_and_says_so() {
    let (_tmp, paths) = home();
    fs::write(paths.config_file(), "[shell]\nterminal = \"wt\"\n").unwrap();
    let desktop = FakeDesktop::with(&[Terminal::Cmd]);
    let opened = launch_terminal(&desktop, &paths, &[], &Place::Projects, &[]).unwrap();
    assert_eq!(opened.terminal, "cmd");
    assert!(opened.note.unwrap().contains("\"wt\""));
    assert_eq!(
        fs::read_to_string(paths.config_file()).unwrap(),
        "[shell]\nterminal = \"wt\"\n",
        "nothing was rewritten"
    );
}

#[test]
fn without_shims_nothing_opens_and_the_fix_is_offered() {
    let (_tmp, paths) = home();
    fs::remove_dir_all(paths.shims_dir()).unwrap();
    let desktop = FakeDesktop::with(&Terminal::ALL);
    let error = launch_terminal(&desktop, &paths, &[], &Place::Projects, &[]).unwrap_err();
    assert_eq!(error.code, ErrorCode::NotAvailable);
    assert_eq!(error.fix.as_deref(), Some("crisol shims install"));
    assert!(desktop.started.lock().unwrap().is_empty());
}

#[test]
fn a_terminal_that_cannot_start_says_which_one() {
    let (_tmp, paths) = home();
    let mut desktop = FakeDesktop::with(&Terminal::ALL);
    desktop.fail_to_start = true;
    let error = launch_terminal(&desktop, &paths, &[], &Place::Projects, &[]).unwrap_err();
    assert_eq!(error.code, ErrorCode::Io);
    assert!(error.message.contains("pwsh"), "{}", error.message);
}

#[test]
fn the_place_is_read_from_the_json_the_page_sends() {
    let site: Place = serde_json::from_str(r#"{"where":"site","site":"shop"}"#).unwrap();
    assert_eq!(site, Place::Site("shop".into()));
    let all: Place = serde_json::from_str(r#"{"where":"projects"}"#).unwrap();
    assert_eq!(all, Place::Projects);
}
