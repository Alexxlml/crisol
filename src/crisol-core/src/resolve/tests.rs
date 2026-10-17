//! Resolution tests: temporary folders for `CRISOL_HOME` and for the project, a map for the
//! environment and doubles for file owners.

use std::collections::{BTreeSet, HashMap};
use std::io;

use tempfile::TempDir;

use super::*;
use crate::catalog::Catalog;
use crate::project::{FileOwner, TrustAllOwners};

/// A `CRISOL_HOME` with some versions "installed" (empty folders are enough) and a project
/// tree `root/app/src` to run from.
struct World {
    _tmp: TempDir,
    paths: CrisolPaths,
    root: PathBuf,
    app: PathBuf,
    src: PathBuf,
    env: HashMap<String, String>,
    catalog: Catalog,
}

impl World {
    fn new(php: &[&str], node: &[&str]) -> Self {
        let tmp = TempDir::new().unwrap();
        let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
        for (service, versions) in [("php", php), ("node", node)] {
            for version in versions {
                fs::create_dir_all(paths.bin_for(service, version).unwrap()).unwrap();
            }
        }
        let root = tmp.path().join("work");
        let app = root.join("app");
        let src = app.join("src");
        fs::create_dir_all(&src).unwrap();
        Self {
            _tmp: tmp,
            paths,
            root,
            app,
            src,
            env: HashMap::new(),
            // No recommendation and no unsupported versions, unless a test says otherwise.
            catalog: Catalog::parse(
                "schema = 1
serial = 1
",
            )
            .unwrap(),
        }
    }

    /// Uses a catalog with a `[recommended]` section and/or `end_of_life` entries.
    fn catalog(&mut self, text: &str) {
        self.catalog = Catalog::parse(&format!(
            "schema = 1
serial = 1
{text}"
        ))
        .unwrap();
    }

    fn config(&self, text: &str) {
        fs::write(self.paths.config_file(), text).unwrap();
    }

    fn resolve_with(
        &self,
        tool: Tool,
        owners: &dyn FileOwners,
    ) -> Result<Resolution, ResolveError> {
        let lookup = |name: &str| self.env.get(name).cloned();
        resolve(
            tool,
            &ResolveContext {
                paths: &self.paths,
                cwd: &self.src,
                ceiling: Some(&self.root),
                owners,
                env: &lookup,
                catalog: &self.catalog,
            },
        )
    }

    fn php(&self) -> Result<Resolution, ResolveError> {
        self.resolve_with(Tool::Php, &TrustAllOwners)
    }

    fn node(&self) -> Result<Resolution, ResolveError> {
        self.resolve_with(Tool::Node, &TrustAllOwners)
    }
}

fn picked(result: Result<Resolution, ResolveError>) -> (String, Source) {
    let resolution = result.unwrap_or_else(|e| panic!("{e}"));
    (
        resolution.resolved.version.version.to_string(),
        resolution.resolved.source,
    )
}

/// Writes a file into `dir` and returns its path.
fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
    let file = dir.join(name);
    fs::write(&file, text).unwrap();
    file
}

const PHPS: [&str; 4] = ["8.2.28", "8.3.35", "8.4.20", "8.4.26"];

#[test]
fn composer_is_the_highest_installed_and_does_not_depend_on_the_project() {
    let world = World::new(&PHPS, &[]);
    for version in ["2.7.0", "2.10.3", "2.9.1"] {
        fs::create_dir_all(world.paths.bin_for(COMPOSER_SERVICE, version).unwrap()).unwrap();
    }
    write(&world.app, ".crisol.toml", "php = \"8.2\"\n");
    let composer = resolve_composer(&world.paths).unwrap();
    assert_eq!(composer.version.to_string(), "2.10.3");
    assert!(composer.path.ends_with("2.10.3"));
}

#[test]
fn composer_prefers_a_stable_version_to_a_higher_prerelease() {
    let world = World::new(&PHPS, &[]);
    for version in ["2.9.1", "2.10.0-RC1"] {
        fs::create_dir_all(world.paths.bin_for(COMPOSER_SERVICE, version).unwrap()).unwrap();
    }
    assert_eq!(
        resolve_composer(&world.paths).unwrap().version.to_string(),
        "2.9.1"
    );
    fs::remove_dir_all(world.paths.bin_for(COMPOSER_SERVICE, "2.9.1").unwrap()).unwrap();
    assert_eq!(
        resolve_composer(&world.paths).unwrap().version.to_string(),
        "2.10.0-RC1",
        "all there is"
    );
}

#[test]
fn without_composer_installed_that_is_what_is_reported() {
    let world = World::new(&PHPS, &[]);
    let error = resolve_composer(&world.paths).unwrap_err();
    assert!(
        matches!(error, ResolveError::NothingInstalled("composer")),
        "{error}"
    );
}

#[test]
fn commands_know_which_tool_they_follow() {
    for (name, command, tool) in [
        ("php", Command::Php, Tool::Php),
        ("PHP", Command::Php, Tool::Php),
        ("composer", Command::Composer, Tool::Php),
        ("node", Command::Node, Tool::Node),
        ("npm", Command::Npm, Tool::Node),
        ("npx", Command::Npx, Tool::Node),
        ("corepack", Command::Corepack, Tool::Node),
        ("pnpm", Command::Pnpm, Tool::Node),
        ("pnpx", Command::Pnpx, Tool::Node),
        ("Yarn", Command::Yarn, Tool::Node),
        ("yarnpkg", Command::Yarnpkg, Tool::Node),
    ] {
        assert_eq!(Command::from_name(name), Some(command), "{name}");
        assert_eq!(command.tool(), tool, "{name}");
    }
    assert_eq!(Command::from_name("python"), None);
    assert_eq!(Command::from_name(""), None);
}

#[test]
fn with_no_hint_the_highest_installed_version_is_used() {
    let world = World::new(&PHPS, &[]);
    let (version, source) = picked(world.php());
    assert_eq!(version, "8.4.26");
    assert_eq!(source, Source::HighestInstalled);
}

#[test]
fn a_prerelease_is_not_the_highest_unless_it_is_all_there_is() {
    let world = World::new(&["8.4.26", "8.5.0-rc1"], &["22.1.0-nightly1"]);
    assert_eq!(picked(world.php()).0, "8.4.26");
    assert_eq!(picked(world.node()).0, "22.1.0-nightly1");
}

#[test]
fn nothing_installed_is_said_plainly() {
    let world = World::new(&[], &[]);
    let error = world.php().unwrap_err();
    assert!(
        matches!(error, ResolveError::NothingInstalled("php")),
        "{error}"
    );
    assert_eq!(error.to_string(), "no php version is installed");
}

#[test]
fn the_environment_variable_wins_over_everything() {
    let mut world = World::new(&PHPS, &[]);
    write(&world.app, ".crisol.toml", "php = \"8.2\"\n");
    write(&world.app, ".php-version", "8.3\n");
    world.env.insert("CRISOL_PHP".into(), " 8.4.20 ".into());

    let (version, source) = picked(world.php());
    assert_eq!(version, "8.4.20");
    assert_eq!(source, Source::Env("CRISOL_PHP"));
}

#[test]
fn a_bad_or_missing_environment_version_is_an_error_not_a_fallback() {
    let mut world = World::new(&PHPS, &[]);
    world.env.insert("CRISOL_PHP".into(), "latest".into());
    assert!(matches!(
        world.php().unwrap_err(),
        ResolveError::BadEnvironment {
            var: "CRISOL_PHP",
            ..
        }
    ));

    world.env.insert("CRISOL_PHP".into(), "7.4".into());
    let error = world.php().unwrap_err();
    assert!(
        matches!(error, ResolveError::NotInstalled { .. }),
        "{error}"
    );
    let text = error.to_string();
    assert!(
        text.contains("7.4") && text.contains("CRISOL_PHP"),
        "{text}"
    );
    assert!(
        text.contains("8.2.28, 8.3.35, 8.4.20, 8.4.26"),
        "lists what is installed: {text}"
    );

    world.env.insert("CRISOL_PHP".into(), "   ".into());
    assert_eq!(
        picked(world.php()).0,
        "8.4.26",
        "a blank variable counts as unset"
    );
}

#[test]
fn crisol_toml_beats_version_files_and_composer() {
    let world = World::new(&PHPS, &[]);
    write(&world.app, ".php-version", "8.2\n");
    write(&world.app, "composer.json", r#"{"require":{"php":"^8.2"}}"#);
    let toml = write(&world.app, ".crisol.toml", "php = \"8.3\"\n");

    let (version, source) = picked(world.php());
    assert_eq!(version, "8.3.35");
    assert_eq!(source, Source::ProjectFile(toml));
}

#[test]
fn a_crisol_toml_without_this_tool_does_not_stop_the_search() {
    let world = World::new(&PHPS, &["22.5.0"]);
    write(&world.app, ".crisol.toml", "node = \"22\"\n");
    write(&world.app, ".php-version", "8.3\n");
    assert_eq!(picked(world.php()).0, "8.3.35");
    assert_eq!(picked(world.node()).0, "22.5.0");
}

#[test]
fn a_prefix_picks_the_highest_installed_patch() {
    let world = World::new(&PHPS, &[]);
    write(&world.app, ".crisol.toml", "php = \"8.4\"\n");
    assert_eq!(picked(world.php()).0, "8.4.26");
}

#[test]
fn a_pin_that_is_not_installed_is_an_error_even_if_another_source_would_work() {
    let world = World::new(&PHPS, &[]);
    write(&world.app, ".crisol.toml", "php = \"7.4\"\n");
    write(&world.app, ".php-version", "8.3\n");
    let error = world.php().unwrap_err();
    assert!(
        matches!(&error, ResolveError::NotInstalled { request, .. } if request == "7.4"),
        "{error}"
    );
}

#[test]
fn php_version_files_are_read_in_their_usual_spellings() {
    for (text, expected) in [
        ("8.3\n", "8.3.35"),
        ("8.3.35\r\n", "8.3.35"),
        ("v8.3\n", "8.3.35"),
        ("php-8.2\n", "8.2.28"),
        ("# comment\n\n  8.2  \n", "8.2.28"),
        ("\u{feff}8.3\n", "8.3.35"),
    ] {
        let world = World::new(&PHPS, &[]);
        write(&world.app, ".php-version", text);
        assert_eq!(picked(world.php()).0, expected, "{text:?}");
    }
}

#[test]
fn the_nearest_version_file_wins() {
    let world = World::new(&PHPS, &[]);
    write(&world.app, ".php-version", "8.2\n");
    write(&world.src, ".php-version", "8.3\n");
    assert_eq!(picked(world.php()).0, "8.3.35");
}

#[test]
fn a_file_that_cannot_be_understood_is_skipped_with_a_warning() {
    let world = World::new(&PHPS, &["22.5.0"]);
    let bad = write(&world.app, ".php-version", "stable\n");
    write(&world.app, "composer.json", r#"{"require":{"php":"^8.3"}}"#);

    let resolution = world.php().unwrap();
    assert_eq!(
        resolution.resolved.version.version.to_string(),
        "8.4.26",
        "falls to composer.json"
    );
    assert!(
        resolution
            .warnings
            .iter()
            .any(|w| w.contains(&bad.display().to_string())),
        "{:?}",
        resolution.warnings
    );

    // Aliases of nvm are not guessed at.
    write(&world.app, ".nvmrc", "lts/*\n");
    let node = world.node().unwrap();
    assert_eq!(node.resolved.source, Source::HighestInstalled);
    assert!(
        node.warnings.iter().any(|w| w.contains("lts/*")),
        "{:?}",
        node.warnings
    );
}

#[test]
fn composer_json_chooses_the_highest_installed_version_that_satisfies_it() {
    let world = World::new(&PHPS, &[]);
    let json = write(
        &world.app,
        "composer.json",
        r#"{"name":"a/b","require":{"php":"^8.1 <8.4"}}"#,
    );
    let (version, source) = picked(world.php());
    assert_eq!(version, "8.3.35");
    assert_eq!(source, Source::ComposerJson(json));

    write(
        &world.app,
        "composer.json",
        r#"{"require":{"php":">=8.5"}}"#,
    );
    let error = world.php().unwrap_err();
    assert!(
        matches!(error, ResolveError::NotInstalled { .. }),
        "{error}"
    );
    assert!(error.to_string().contains(">=8.5"));
}

#[test]
fn a_composer_json_without_php_is_not_about_php() {
    let world = World::new(&PHPS, &[]);
    write(
        &world.app,
        "composer.json",
        r#"{"require":{"laravel/framework":"^11"}}"#,
    );
    assert_eq!(picked(world.php()).1, Source::HighestInstalled);

    write(&world.app, "composer.json", "{ not json");
    let resolution = world.php().unwrap();
    assert!(resolution
        .warnings
        .iter()
        .any(|w| w.contains("composer.json")));
}

#[test]
fn a_version_file_beats_composer_json_as_the_architecture_orders() {
    let world = World::new(&PHPS, &[]);
    write(&world.app, "composer.json", r#"{"require":{"php":"^8.4"}}"#);
    write(&world.root, ".php-version", "8.2\n");
    assert_eq!(picked(world.php()).0, "8.2.28");
}

#[test]
fn node_reads_nvmrc_then_node_version_then_engines() {
    let world = World::new(&[], &["18.20.4", "20.11.0", "22.5.0"]);
    write(
        &world.app,
        "package.json",
        r#"{"engines":{"node":">=18 <20"}}"#,
    );
    assert_eq!(picked(world.node()).0, "18.20.4", "engines");

    write(&world.app, ".node-version", "v20\n");
    assert_eq!(picked(world.node()).0, "20.11.0", ".node-version");

    write(&world.app, ".nvmrc", "22.5.0\n");
    let (version, source) = picked(world.node());
    assert_eq!(version, "22.5.0", ".nvmrc");
    assert!(matches!(source, Source::VersionFile(p) if p.ends_with(".nvmrc")));
}

#[test]
fn a_package_json_without_engines_is_passed_over() {
    let world = World::new(&[], &["18.20.4", "22.5.0"]);
    write(&world.src, "package.json", r#"{"name":"inner"}"#);
    let outer = write(&world.app, "package.json", r#"{"engines":{"node":"18.x"}}"#);
    let (version, source) = picked(world.node());
    assert_eq!(version, "18.20.4");
    assert_eq!(source, Source::PackageJson(outer));
}

#[test]
fn npm_engines_use_npm_rules() {
    let world = World::new(&[], &["18.4.0", "18.9.0"]);
    // `~18.4` means 18.4.x in npm (it would mean 18.x in composer).
    write(
        &world.app,
        "package.json",
        r#"{"engines":{"node":"~18.4"}}"#,
    );
    assert_eq!(picked(world.node()).0, "18.4.0");
}

#[test]
fn the_global_default_comes_before_the_highest_version() {
    let world = World::new(&PHPS, &["20.11.0", "22.5.0"]);
    world.config("[defaults]\nphp = \"8.3\"\nnode = \"20\"\n");
    let (version, source) = picked(world.php());
    assert_eq!(
        (version.as_str(), source),
        ("8.3.35", Source::GlobalDefault)
    );
    assert_eq!(picked(world.node()).0, "20.11.0");
}

#[test]
fn a_broken_crisol_toml_is_an_error() {
    let world = World::new(&PHPS, &[]);
    world.config("[defaults\nphp = ");
    assert!(matches!(world.php().unwrap_err(), ResolveError::Config(_)));
}

#[test]
fn a_broken_project_file_is_an_error_not_a_fallback() {
    let world = World::new(&PHPS, &[]);
    write(&world.app, ".crisol.toml", "php = = broken");
    assert!(matches!(world.php().unwrap_err(), ResolveError::Config(_)));
}

// ---- who owns the files (ADR-0010) -----------------------------------------------

/// Every file is trusted except the ones listed.
struct Strangers(BTreeSet<PathBuf>);

impl FileOwners for Strangers {
    fn owner_of(&self, path: &Path) -> io::Result<FileOwner> {
        let stranger = self.0.contains(path);
        Ok(FileOwner {
            trusted: !stranger,
            name: if stranger { "S-1-5-21-9-9-9-1" } else { "me" }.to_owned(),
        })
    }
}

#[test]
fn files_of_strangers_are_skipped_with_a_warning_for_every_kind_of_file() {
    let world = World::new(&PHPS, &[]);
    let toml = write(&world.app, ".crisol.toml", "php = \"8.2\"\n");
    let version = write(&world.app, ".php-version", "8.2\n");
    let composer = write(
        &world.app,
        "composer.json",
        r#"{"require":{"php":"8.2.*"}}"#,
    );
    let owners = Strangers([toml.clone(), version.clone(), composer.clone()].into());

    let resolution = world.resolve_with(Tool::Php, &owners).unwrap();
    assert_eq!(resolution.resolved.source, Source::HighestInstalled);
    for file in [toml, version, composer] {
        let name = file.display().to_string();
        assert!(
            resolution
                .warnings
                .iter()
                .any(|w| w.contains(&name) && w.contains("S-1-5-21-9-9-9-1")),
            "{name} in {:?}",
            resolution.warnings
        );
    }
    assert_eq!(
        resolution.warnings.len(),
        3,
        "each is reported once: {:?}",
        resolution.warnings
    );
}

#[test]
fn a_stranger_near_the_project_does_not_hide_the_owners_file_above() {
    let world = World::new(&PHPS, &[]);
    let near = write(&world.src, ".php-version", "8.2\n");
    write(&world.root, ".php-version", "8.3\n");
    let owners = Strangers([near].into());
    let resolution = world.resolve_with(Tool::Php, &owners).unwrap();
    assert_eq!(resolution.resolved.version.version.to_string(), "8.3.35");
    assert_eq!(resolution.warnings.len(), 1);
}

#[test]
fn the_walk_stops_at_the_first_git_folder() {
    let world = World::new(&PHPS, &[]);
    write(&world.root, ".php-version", "8.2\n"); // outside the repository
    fs::create_dir(world.app.join(".git")).unwrap();
    assert_eq!(picked(world.php()).1, Source::HighestInstalled);
}

// ---- the default version (ADR-0012) -----------------------------------------------------

const RECOMMEND_84: &str = "
[recommended]
php = \"8.4\"
";

/// A catalog entry marked as unsupported; only its service and version matter here.
fn eol_entry(service: &str, version: &str) -> String {
    format!(
        "
[[package]]
service = \"{service}\"
version = \"{version}\"
os = \"windows\"
arch = \"x86_64\"
         url = \"https://example.invalid/x.zip\"
sha256 = \"{}\"
format = \"zip\"
executable = \"{service}.exe\"
end_of_life = true
",
        "ab".repeat(32)
    )
}

fn require_php(world: &World, constraint: &str) {
    write(
        &world.app,
        "composer.json",
        &format!(r#"{{"require":{{"php":"{constraint}"}}}}"#),
    );
}

#[test]
fn with_no_hint_the_recommended_line_wins_over_a_higher_version() {
    let mut world = World::new(&["8.3.35", "8.4.20", "8.4.26", "8.5.11"], &[]);
    world.catalog(RECOMMEND_84);
    let (version, source) = picked(world.php());
    assert_eq!(version, "8.4.26", "the highest patch of the line");
    assert_eq!(source, Source::CatalogRecommendation);
}

#[test]
fn if_the_recommended_line_is_not_installed_the_highest_one_is_used() {
    let mut world = World::new(&["8.3.35", "8.5.11"], &[]);
    world.catalog(RECOMMEND_84);
    let (version, source) = picked(world.php());
    assert_eq!(
        (version.as_str(), source),
        ("8.5.11", Source::HighestInstalled)
    );
}

#[test]
fn the_users_default_beats_the_recommendation() {
    let mut world = World::new(&["8.3.35", "8.4.26", "8.5.11"], &[]);
    world.catalog(RECOMMEND_84);
    world.config(
        "[defaults]
php = \"8.5\"
",
    );
    let (version, source) = picked(world.php());
    assert_eq!(
        (version.as_str(), source),
        ("8.5.11", Source::GlobalDefault)
    );
}

#[test]
fn an_open_constraint_gets_the_default_when_it_satisfies_it() {
    let mut world = World::new(&["7.4.33", "8.4.26", "8.5.11"], &[]);
    world.catalog(&format!("{RECOMMEND_84}{}", eol_entry("php", "7.4.33")));
    require_php(&world, ">=7.4");
    assert_eq!(
        picked(world.php()).0,
        "8.4.26",
        "not the newest, the default"
    );

    // The user's default is the one that counts when there is one.
    world.config(
        "[defaults]
php = \"8.5\"
",
    );
    assert_eq!(picked(world.php()).0, "8.5.11");
}

#[test]
fn an_open_constraint_the_default_does_not_satisfy_gets_the_highest_that_does() {
    let mut world = World::new(&["7.4.33", "8.4.26", "8.5.11"], &[]);
    world.catalog(&format!("{RECOMMEND_84}{}", eol_entry("php", "7.4.33")));

    require_php(&world, ">=7.4 <8.0");
    assert_eq!(picked(world.php()).0, "7.4.33");

    require_php(&world, "^8.5");
    assert_eq!(picked(world.php()).0, "8.5.11");

    // And a default that is not installed does not get in the way.
    let mut world = World::new(&["8.3.35", "8.5.11"], &[]);
    world.catalog(RECOMMEND_84);
    require_php(&world, ">=7.4");
    assert_eq!(picked(world.php()).0, "8.5.11");
}

#[test]
fn without_a_recommendation_an_open_constraint_still_takes_the_highest() {
    let world = World::new(&["7.4.33", "8.4.26", "8.5.11"], &[]);
    require_php(&world, ">=7.4");
    assert_eq!(picked(world.php()).0, "8.5.11");
}

#[test]
fn an_unsupported_version_is_never_chosen_by_default_while_another_is_installed() {
    let mut world = World::new(&["7.4.33", "8.3.35"], &[]);
    world.catalog(&eol_entry("php", "7.4.33"));
    let (version, source) = picked(world.php());
    assert_eq!(
        (version.as_str(), source),
        ("8.3.35", Source::HighestInstalled)
    );

    // Even if it were the highest number (a made-up case: 9.x unsupported).
    let mut world = World::new(&["8.3.35", "9.0.1"], &[]);
    world.catalog(&eol_entry("php", "9.0.1"));
    assert_eq!(picked(world.php()).0, "8.3.35");

    // But it is what there is when nothing else is installed.
    let mut world = World::new(&["7.4.33"], &[]);
    world.catalog(&eol_entry("php", "7.4.33"));
    assert_eq!(picked(world.php()).0, "7.4.33");
}

#[test]
fn a_version_pinned_by_hand_is_respected_even_if_unsupported() {
    let mut world = World::new(&["7.4.33", "8.4.26"], &[]);
    world.catalog(&format!("{RECOMMEND_84}{}", eol_entry("php", "7.4.33")));
    write(
        &world.app,
        ".php-version",
        "7.4
",
    );
    assert_eq!(picked(world.php()).0, "7.4.33");
}

const RECOMMEND_NODE_24: &str = "
[recommended]
node = \"24\"
";

#[test]
fn node_follows_the_recommended_line_and_skips_the_unsupported_one() {
    let mut world = World::new(&[], &["20.20.2", "22.23.3", "24.22.0"]);
    world.catalog(&format!(
        "{RECOMMEND_NODE_24}{}",
        eol_entry("node", "20.20.2")
    ));
    let (version, source) = picked(world.node());
    assert_eq!(
        (version.as_str(), source),
        ("24.22.0", Source::CatalogRecommendation)
    );
    // An open `engines.node` also gets the recommended line when it satisfies it.
    write(&world.app, "package.json", r#"{"engines":{"node":">=18"}}"#);
    assert_eq!(picked(world.node()).0, "24.22.0");
    write(
        &world.app, ".nvmrc", "20
",
    );
    assert_eq!(picked(world.node()).0, "20.20.2");

    // Without a recommendation the highest supported one wins, never the unsupported 20.
    let mut world = World::new(&[], &["20.20.2", "22.23.3"]);
    world.catalog(&eol_entry("node", "20.20.2"));
    assert_eq!(picked(world.node()).0, "22.23.3");
}
