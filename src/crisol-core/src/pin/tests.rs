use std::fs;

use tempfile::TempDir;

use super::*;
use crate::project::TrustAllOwners;
use crate::resolve::{resolve, ResolveContext};

struct World {
    _tmp: TempDir,
    paths: CrisolPaths,
    project: PathBuf,
}

fn world(php: &[&str], node: &[&str]) -> World {
    let tmp = TempDir::new().unwrap();
    let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
    for (service, versions) in [("php", php), ("node", node)] {
        for version in versions {
            fs::create_dir_all(paths.bin_for(service, version).unwrap()).unwrap();
        }
    }
    let project = tmp.path().join("work").join("app");
    fs::create_dir_all(&project).unwrap();
    World {
        _tmp: tmp,
        paths,
        project,
    }
}

fn no_env(_: &str) -> Option<String> {
    None
}

/// What the shim would pick in `dir`.
fn picked(world: &World, tool: Tool, dir: &Path) -> String {
    let ceiling = dir.parent().unwrap().to_path_buf();
    let catalog = crate::catalog::Catalog::embedded().unwrap();
    resolve(
        tool,
        &ResolveContext {
            paths: &world.paths,
            cwd: dir,
            ceiling: Some(&ceiling),
            owners: &TrustAllOwners,
            env: &no_env,
            catalog: &catalog,
        },
    )
    .unwrap()
    .resolved
    .version
    .version
    .to_string()
}

const PHPS: [&str; 3] = ["8.3.35", "8.4.20", "8.4.26"];

#[test]
fn use_global_creates_crisol_toml_and_the_shim_follows() {
    let world = world(&PHPS, &[]);
    assert!(!world.paths.config_file().exists());

    let used = use_global(&world.paths, Tool::Php, "8.3", &no_env).unwrap();
    assert_eq!(used.file, world.paths.config_file());
    assert!(used.changed);
    assert_eq!(used.previous, None);
    assert_eq!(used.selects.version.to_string(), "8.3.35");
    assert_eq!(
        fs::read_to_string(world.paths.config_file()).unwrap(),
        "[defaults]\nphp = \"8.3\"\n"
    );
    assert_eq!(picked(&world, Tool::Php, &world.project), "8.3.35");
}

#[test]
fn use_global_changes_one_value_and_keeps_the_rest_of_the_users_file() {
    let world = world(&PHPS, &["22.5.0"]);
    let original = "# My setup\n[defaults]\nphp  = \"8.3\"   # for the client\nnode = \"22\"\n\n[sites]\ntld = \"test\" # keep\n";
    fs::write(world.paths.config_file(), original).unwrap();

    let used = use_global(&world.paths, Tool::Php, "8.4", &no_env).unwrap();
    assert_eq!(used.previous.as_deref(), Some("8.3"));
    assert_eq!(
        fs::read_to_string(world.paths.config_file()).unwrap(),
        original.replacen("\"8.3\"", "\"8.4\"", 1),
        "only the value changed, comments and spacing included"
    );
    assert_eq!(picked(&world, Tool::Php, &world.project), "8.4.26");
}

#[test]
fn choosing_what_is_already_chosen_does_not_touch_the_file() {
    let world = world(&PHPS, &[]);
    use_global(&world.paths, Tool::Php, "8.4", &no_env).unwrap();
    let before = fs::metadata(world.paths.config_file())
        .unwrap()
        .modified()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));

    let again = use_global(&world.paths, Tool::Php, "8.4", &no_env).unwrap();
    assert!(!again.changed);
    assert_eq!(again.previous.as_deref(), Some("8.4"));
    let after = fs::metadata(world.paths.config_file())
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(before, after);
}

#[test]
fn use_local_writes_the_project_file_and_beats_the_global_default() {
    let world = world(&PHPS, &[]);
    use_global(&world.paths, Tool::Php, "8.3", &no_env).unwrap();

    let used = use_local(&world.paths, &world.project, Tool::Php, "8.4.20", &no_env).unwrap();
    assert_eq!(used.file, world.project.join(".crisol.toml"));
    assert_eq!(
        fs::read_to_string(&used.file).unwrap(),
        "php = \"8.4.20\"\n"
    );
    assert_eq!(picked(&world, Tool::Php, &world.project), "8.4.20");
    // Another folder still follows the global default.
    let other = world.project.parent().unwrap().join("other");
    fs::create_dir(&other).unwrap();
    assert_eq!(picked(&world, Tool::Php, &other), "8.3.35");
}

#[test]
fn use_local_keeps_the_other_tools_pin_and_the_comments() {
    let world = world(&PHPS, &["22.5.0", "20.11.0"]);
    let file = world.project.join(".crisol.toml");
    fs::write(&file, "# pinned for the client\nnode = \"22\" # LTS\n").unwrap();

    use_local(&world.paths, &world.project, Tool::Php, "8.4", &no_env).unwrap();
    let text = fs::read_to_string(&file).unwrap();
    assert!(
        text.starts_with("# pinned for the client\nnode = \"22\" # LTS\n"),
        "{text}"
    );
    assert!(text.contains("php = \"8.4\""), "{text}");
    assert_eq!(picked(&world, Tool::Node, &world.project), "22.5.0");
    assert_eq!(picked(&world, Tool::Php, &world.project), "8.4.26");

    use_local(&world.paths, &world.project, Tool::Node, "20", &no_env).unwrap();
    assert_eq!(picked(&world, Tool::Node, &world.project), "20.11.0");
}

#[test]
fn a_version_that_is_not_installed_is_refused_and_nothing_is_written() {
    let world = world(&PHPS, &[]);
    let error = use_local(&world.paths, &world.project, Tool::Php, "7.4", &no_env).unwrap_err();
    assert!(
        matches!(&error, UseError::NotInstalled { request, .. } if request == "7.4"),
        "{error}"
    );
    assert!(
        error.to_string().contains("8.3.35, 8.4.20, 8.4.26"),
        "{error}"
    );
    assert!(!world.project.join(".crisol.toml").exists());

    let error = use_global(&world.paths, Tool::Node, "22", &no_env).unwrap_err();
    assert!(error.to_string().contains("installed: none"), "{error}");
    assert!(!world.paths.config_file().exists());
}

#[test]
fn something_that_is_not_a_version_is_refused() {
    let world = world(&PHPS, &[]);
    for bad in ["latest", "", "8..4", "../8", "php8"] {
        let error = use_global(&world.paths, Tool::Php, bad, &no_env).unwrap_err();
        assert!(matches!(error, UseError::BadVersion(_)), "{bad}: {error}");
    }
    assert!(!world.paths.config_file().exists());
}

#[test]
fn a_folder_that_does_not_exist_is_refused() {
    let world = world(&PHPS, &[]);
    let missing = world.project.join("nope");
    let error = use_local(&world.paths, &missing, Tool::Php, "8.4", &no_env).unwrap_err();
    assert!(matches!(error, UseError::NotAFolder(_)), "{error}");
}

#[test]
fn a_broken_file_is_an_error_and_is_not_overwritten() {
    let world = world(&PHPS, &[]);
    let file = world.project.join(".crisol.toml");
    fs::write(&file, "php = = broken").unwrap();
    let error = use_local(&world.paths, &world.project, Tool::Php, "8.4", &no_env).unwrap_err();
    assert!(matches!(error, UseError::Edit(_)), "{error}");
    assert_eq!(fs::read_to_string(&file).unwrap(), "php = = broken");
}

#[test]
fn a_variable_of_this_terminal_is_mentioned_because_it_wins() {
    let world = world(&PHPS, &[]);
    let env = |name: &str| (name == "CRISOL_PHP").then(|| " 8.3 ".to_owned());
    let used = use_global(&world.paths, Tool::Php, "8.4", &env).unwrap();
    assert_eq!(used.warnings.len(), 1);
    assert!(used.warnings[0].contains("CRISOL_PHP") && used.warnings[0].contains("8.3"));

    let quiet = use_local(&world.paths, &world.project, Tool::Php, "8.4", &no_env).unwrap();
    assert_eq!(quiet.warnings, Vec::<String>::new());
}
