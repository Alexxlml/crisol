use std::fmt::Write as _;
use std::io::{Cursor, Write};

use sha2::{Digest, Sha256};
use tempfile::TempDir;
use zip::write::SimpleFileOptions;

use super::*;
use crate::catalog::{Arch, Os};
use crate::fetch::fake::FakeFetcher;

const WINDOWS: Target = Target {
    os: Os::Windows,
    arch: Arch::X86_64,
};
const LINUX: Target = Target {
    os: Os::Linux,
    arch: Arch::X86_64,
};

fn zip_of(executable: &str) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default();
    writer.start_file(executable, options).unwrap();
    writer.write_all(b"MZ binary").unwrap();
    writer.start_file("ext/extra.dll", options).unwrap();
    writer.write_all(b"dll").unwrap();
    writer.finish().unwrap().into_inner()
}

fn url_of(service: &str, version: &str) -> String {
    format!("https://example.invalid/{service}-{version}.zip")
}

/// A catalog (and a fetcher that serves it) offering the given `(service, version)` builds.
fn world(builds: &[(&str, &str)]) -> (Catalog, FakeFetcher) {
    let fetcher = FakeFetcher::new();
    let mut text = String::from("schema = 1\nserial = 1\n");
    for (service, version) in builds {
        let bytes = zip_of(&format!("{service}.exe"));
        let digest = Sha256::digest(&bytes);
        let hash = digest.iter().fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        });
        let _ = write!(
            text,
            "
[[package]]
service = \"{service}\"
version = \"{version}\"
os = \"windows\"
arch = \"x86_64\"
             url = \"{}\"
sha256 = \"{hash}\"
format = \"zip\"
executable = \"{service}.exe\"
size = {}
",
            url_of(service, version),
            bytes.len()
        );
        fetcher.serve(&url_of(service, version), bytes);
    }
    (Catalog::parse(&text).unwrap(), fetcher)
}

struct Rig {
    _dir: TempDir,
    paths: CrisolPaths,
}

impl Rig {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let paths = CrisolPaths::new(dir.path().join("home")).unwrap();
        Self { _dir: dir, paths }
    }

    fn install(
        &self,
        catalog: &Catalog,
        fetcher: &FakeFetcher,
        service: &str,
        request: &str,
    ) -> Result<Installed, ManageError> {
        let env = Environment {
            paths: &self.paths,
            catalog,
            target: WINDOWS,
            fetcher,
        };
        install_version(
            &env,
            service,
            request,
            &InstallOptions::default(),
            &mut |_| {},
            &crate::paths::fake::FakePrivateDirs::new(),
        )
    }

    fn installed(&self) -> Vec<String> {
        list_installed(&self.paths)
            .unwrap()
            .iter()
            .map(|i| format!("{} {}", i.service, i.version))
            .collect()
    }

    /// Every file and folder under the home, relative, sorted: the whole picture of the disk.
    fn snapshot(&self) -> Vec<String> {
        let home = self.paths.home().to_path_buf();
        let mut out = Vec::new();
        let mut stack = vec![home.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(read) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in read {
                let path = entry.unwrap().path();
                out.push(
                    path.strip_prefix(&home)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
                if path.is_dir() {
                    stack.push(path);
                }
            }
        }
        out.sort();
        out
    }

    fn mkdir(&self, relative: &str) {
        fs::create_dir_all(self.paths.home().join(relative)).unwrap();
    }
}

// ---- listing --------------------------------------------------------------------

#[test]
fn nothing_is_installed_before_anything_is_installed() {
    let rig = Rig::new();
    assert_eq!(rig.installed(), Vec::<String>::new(), "no bin/ at all");
    rig.paths.ensure_layout().unwrap();
    assert_eq!(rig.installed(), Vec::<String>::new(), "an empty bin/");
}

#[test]
fn installed_versions_are_found_on_disk_in_numeric_order() {
    let rig = Rig::new();
    for dir in [
        "bin/php/8.4.10",
        "bin/php/8.4.2",
        "bin/php/8.3.35",
        "bin/nginx/1.30.5",
        "bin/node/22.23.3",
    ] {
        rig.mkdir(dir);
    }
    assert_eq!(
        rig.installed(),
        [
            "nginx 1.30.5",
            "node 22.23.3",
            "php 8.3.35",
            "php 8.4.2",
            "php 8.4.10"
        ]
    );
    let first = &list_installed(&rig.paths).unwrap()[0];
    assert_eq!(first.path, rig.paths.bin_for("nginx", "1.30.5").unwrap());
}

#[test]
fn work_areas_junk_and_odd_folders_are_not_installations() {
    let rig = Rig::new();
    rig.mkdir("bin/php/8.4.26");
    rig.mkdir("bin/.staging-abc123/php/9.9.9"); // an installer's work area
    rig.mkdir("bin/.hidden/php/1.0.0");
    rig.mkdir("bin/php/not-a-version");
    rig.mkdir("bin/php/.cache");
    rig.mkdir("bin/node");
    fs::write(rig.paths.bin_dir().join("readme.txt"), "x").unwrap();
    fs::write(
        rig.paths.bin_dir().join("php").join("8.9.9"),
        "a file named like a version",
    )
    .unwrap();
    assert_eq!(rig.installed(), ["php 8.4.26"]);
}

#[test]
fn the_catalog_listing_marks_what_is_installed_and_ignores_other_platforms() {
    let rig = Rig::new();
    let (catalog, fetcher) = world(&[("php", "8.3.35"), ("php", "8.4.26"), ("nginx", "1.30.5")]);
    rig.install(&catalog, &fetcher, "php", "8.4").unwrap();

    let list = list_available(&rig.paths, &catalog, WINDOWS).unwrap();
    let rows: Vec<(String, bool)> = list
        .iter()
        .map(|a| (a.package.label(), a.installed))
        .collect();
    assert_eq!(
        rows,
        [
            ("nginx 1.30.5 (windows-x86_64)".to_owned(), false),
            ("php 8.3.35 (windows-x86_64)".to_owned(), false),
            ("php 8.4.26 (windows-x86_64)".to_owned(), true),
        ]
    );
    assert_eq!(
        list_available(&rig.paths, &catalog, LINUX).unwrap(),
        Vec::new()
    );
}

// ---- install --------------------------------------------------------------------

#[test]
fn a_request_installs_the_highest_matching_version() {
    let rig = Rig::new();
    let (catalog, fetcher) = world(&[("php", "8.4.2"), ("php", "8.4.10"), ("php", "8.3.35")]);
    let done = rig.install(&catalog, &fetcher, "php", "8.4").unwrap();
    assert_eq!(done.path, rig.paths.bin_for("php", "8.4.10").unwrap());
    assert_eq!(rig.installed(), ["php 8.4.10"]);
    assert_eq!(fetcher.requests(), [url_of("php", "8.4.10")]);
}

#[test]
fn latest_installs_the_highest_stable_version() {
    let rig = Rig::new();
    let (catalog, fetcher) = world(&[("php", "8.4.26"), ("php", "8.5.0-rc1"), ("php", "8.3.35")]);
    rig.install(&catalog, &fetcher, "php", "latest").unwrap();
    assert_eq!(
        rig.installed(),
        ["php 8.4.26"],
        "a release candidate is not the latest"
    );
}

#[test]
fn a_version_the_catalog_lacks_downloads_nothing() {
    let rig = Rig::new();
    let (catalog, fetcher) = world(&[("php", "8.4.26")]);
    for (service, request) in [("php", "7.4"), ("ruby", "3"), ("php", "latestest")] {
        let error = rig
            .install(&catalog, &fetcher, service, request)
            .unwrap_err();
        assert!(
            matches!(error, ManageError::Catalog(_)),
            "{service} {request}: {error}"
        );
    }
    assert_eq!(fetcher.requests(), Vec::<String>::new());
    assert_eq!(rig.installed(), Vec::<String>::new());
}

#[test]
fn installing_what_is_already_installed_changes_nothing() {
    let rig = Rig::new();
    let (catalog, fetcher) = world(&[("php", "8.4.26")]);
    rig.install(&catalog, &fetcher, "php", "8.4").unwrap();
    let before = rig.snapshot();
    let error = rig.install(&catalog, &fetcher, "php", "8.4").unwrap_err();
    assert!(
        matches!(
            error,
            ManageError::Install(InstallError::AlreadyInstalled(_))
        ),
        "{error}"
    );
    assert_eq!(rig.snapshot(), before);
    assert_eq!(
        fetcher.requests().len(),
        1,
        "the second install must not download again"
    );
}

// ---- uninstall ------------------------------------------------------------------

#[test]
fn installing_and_uninstalling_php_leaves_the_disk_exactly_as_it_was() {
    let rig = Rig::new();
    rig.paths.ensure_layout().unwrap();
    let before = rig.snapshot();
    let (catalog, fetcher) = world(&[("php", "8.4.26")]);

    rig.install(&catalog, &fetcher, "php", "8.4").unwrap();
    assert_ne!(
        rig.snapshot(),
        before,
        "the install really changed something"
    );
    assert!(rig
        .snapshot()
        .contains(&"bin/php/8.4.26/php.exe".to_owned()));

    let removed = uninstall(
        &rig.paths,
        "php",
        "8.4.26",
        &crate::paths::fake::FakePrivateDirs::new(),
    )
    .unwrap();
    assert_eq!(removed.version.service, "php");
    assert!(removed.service_folder_removed);
    assert_eq!(rig.snapshot(), before, "bin/ must be exactly as it was");
}

#[test]
fn uninstalling_one_version_keeps_the_others_and_the_service_folder() {
    let rig = Rig::new();
    let (catalog, fetcher) = world(&[("php", "8.3.35"), ("php", "8.4.26"), ("nginx", "1.30.5")]);
    rig.install(&catalog, &fetcher, "php", "8.3").unwrap();
    rig.install(&catalog, &fetcher, "php", "8.4").unwrap();
    rig.install(&catalog, &fetcher, "nginx", "1").unwrap();
    fs::write(rig.paths.bin_dir().join("notes.txt"), "mine").unwrap();

    let removed = uninstall(
        &rig.paths,
        "php",
        "8.3.35",
        &crate::paths::fake::FakePrivateDirs::new(),
    )
    .unwrap();
    assert!(
        !removed.service_folder_removed,
        "php 8.4.26 still lives there"
    );
    assert_eq!(rig.installed(), ["nginx 1.30.5", "php 8.4.26"]);
    assert!(rig.paths.bin_dir().join("notes.txt").is_file());
    assert!(
        rig.paths
            .bin_for("php", "8.4.26")
            .unwrap()
            .join("php.exe")
            .is_file(),
        "untouched"
    );

    let removed = uninstall(
        &rig.paths,
        "php",
        "8.4.26",
        &crate::paths::fake::FakePrivateDirs::new(),
    )
    .unwrap();
    assert!(removed.service_folder_removed);
    assert_eq!(rig.installed(), ["nginx 1.30.5"]);
    assert!(!rig.paths.bin_dir().join("php").exists());
}

#[test]
fn no_work_area_is_left_behind_by_an_uninstall() {
    let rig = Rig::new();
    let (catalog, fetcher) = world(&[("php", "8.4.26")]);
    rig.install(&catalog, &fetcher, "php", "8.4").unwrap();
    uninstall(
        &rig.paths,
        "php",
        "8.4.26",
        &crate::paths::fake::FakePrivateDirs::new(),
    )
    .unwrap();
    let hidden: Vec<String> = fs::read_dir(rig.paths.bin_dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(hidden, Vec::<String>::new());
}

#[test]
fn a_prefix_removes_the_only_version_it_matches() {
    let rig = Rig::new();
    let (catalog, fetcher) = world(&[("php", "8.4.26"), ("php", "8.3.35")]);
    rig.install(&catalog, &fetcher, "php", "8.4").unwrap();
    rig.install(&catalog, &fetcher, "php", "8.3").unwrap();
    uninstall(
        &rig.paths,
        "php",
        "8.4",
        &crate::paths::fake::FakePrivateDirs::new(),
    )
    .unwrap();
    assert_eq!(rig.installed(), ["php 8.3.35"]);
}

#[test]
fn an_ambiguous_request_removes_nothing_but_an_exact_one_works() {
    let rig = Rig::new();
    for dir in ["bin/php/8.4.2", "bin/php/8.4.10"] {
        rig.mkdir(dir);
    }
    let before = rig.snapshot();
    let error = uninstall(
        &rig.paths,
        "php",
        "8.4",
        &crate::paths::fake::FakePrivateDirs::new(),
    )
    .unwrap_err();
    match &error {
        ManageError::Ambiguous { matches, .. } => assert_eq!(matches, &["8.4.2", "8.4.10"]),
        other => panic!("expected Ambiguous, got {other}"),
    }
    assert!(error.to_string().contains("give the full version"));
    assert_eq!(
        rig.snapshot(),
        before,
        "an ambiguous request must delete nothing"
    );

    uninstall(
        &rig.paths,
        "php",
        "8.4.2",
        &crate::paths::fake::FakePrivateDirs::new(),
    )
    .unwrap();
    assert_eq!(
        rig.installed(),
        ["php 8.4.10"],
        "the exact version, even though `8.4` is ambiguous"
    );
}

#[test]
fn a_prefix_is_ambiguous_even_when_one_match_is_x_y_0() {
    // `8.4` equals `8.4.0` as a version, but it is a prefix of `8.4.26` too.
    let rig = Rig::new();
    for dir in ["bin/php/8.4.0", "bin/php/8.4.26"] {
        rig.mkdir(dir);
    }
    let before = rig.snapshot();
    let error = uninstall(
        &rig.paths,
        "php",
        "8.4",
        &crate::paths::fake::FakePrivateDirs::new(),
    )
    .unwrap_err();
    assert!(matches!(error, ManageError::Ambiguous { .. }), "{error}");
    assert_eq!(rig.snapshot(), before);

    uninstall(
        &rig.paths,
        "php",
        "8.4.0",
        &crate::paths::fake::FakePrivateDirs::new(),
    )
    .unwrap();
    assert_eq!(rig.installed(), ["php 8.4.26"]);
}

#[test]
fn removing_what_is_not_installed_says_what_is() {
    let rig = Rig::new();
    rig.mkdir("bin/php/8.4.26");
    rig.mkdir("bin/php/8.3.35");
    let before = rig.snapshot();

    let error = uninstall(
        &rig.paths,
        "php",
        "7.4",
        &crate::paths::fake::FakePrivateDirs::new(),
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "php 7.4 is not installed (installed: 8.3.35, 8.4.26)"
    );
    let error = uninstall(
        &rig.paths,
        "ruby",
        "3",
        &crate::paths::fake::FakePrivateDirs::new(),
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "ruby 3 is not installed (installed: none)"
    );
    assert_eq!(rig.snapshot(), before);

    uninstall(
        &rig.paths,
        "php",
        "8.4.26",
        &crate::paths::fake::FakePrivateDirs::new(),
    )
    .unwrap();
    assert!(
        matches!(
            uninstall(
                &rig.paths,
                "php",
                "8.4.26",
                &crate::paths::fake::FakePrivateDirs::new()
            ),
            Err(ManageError::NotInstalled { .. })
        ),
        "twice"
    );
}

#[test]
fn bad_names_and_versions_touch_nothing() {
    let rig = Rig::new();
    rig.mkdir("bin/php/8.4.26");
    let before = rig.snapshot();
    for service in ["../etc", "a/b", "", "C:"] {
        assert!(
            matches!(
                uninstall(
                    &rig.paths,
                    service,
                    "8.4",
                    &crate::paths::fake::FakePrivateDirs::new()
                ),
                Err(ManageError::Paths(_))
            ),
            "`{service}`"
        );
    }
    for request in ["", "abc", "../8", "8.x"] {
        assert!(
            matches!(
                uninstall(
                    &rig.paths,
                    "php",
                    request,
                    &crate::paths::fake::FakePrivateDirs::new()
                ),
                Err(ManageError::Version(_))
            ),
            "`{request}`"
        );
    }
    assert_eq!(rig.snapshot(), before);
}

#[cfg(windows)]
#[test]
fn a_version_with_a_running_program_cannot_be_removed_and_is_left_intact() {
    use std::process::{Command, Stdio};

    let rig = Rig::new();
    let (catalog, fetcher) = world(&[("php", "8.4.26")]);
    let installed = rig.install(&catalog, &fetcher, "php", "8.4").unwrap();

    // Run a real program from inside the installation (a copy of `ping` named php.exe).
    let ping = Path::new(&std::env::var("SystemRoot").unwrap())
        .join("System32")
        .join("PING.EXE");
    let exe = installed.path.join("php.exe");
    fs::copy(&ping, &exe).unwrap();
    let mut running = Command::new(&exe)
        .args(["-n", "30", "127.0.0.1"])
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let before = rig.snapshot();

    let result = uninstall(
        &rig.paths,
        "php",
        "8.4.26",
        &crate::paths::fake::FakePrivateDirs::new(),
    );
    let after = rig.snapshot();
    running.kill().unwrap();
    running.wait().unwrap();

    let error = result.expect_err("a running program must block the uninstall");
    match &error {
        ManageError::InUse(file) => {
            assert!(file.ends_with("php.exe"), "names the busy file: {file:?}");
        }
        other => panic!("expected InUse, got {other}"),
    }
    assert_eq!(after, before, "a refused uninstall changes nothing");

    // Once the program is gone it can be removed.
    uninstall(
        &rig.paths,
        "php",
        "8.4.26",
        &crate::paths::fake::FakePrivateDirs::new(),
    )
    .unwrap();
    assert_eq!(rig.installed(), Vec::<String>::new());
}

// ---- resolving what is installed ------------------------------------------------

#[test]
fn the_highest_installed_version_that_matches_is_resolved() {
    let rig = Rig::new();
    for dir in [
        "bin/php/8.3.35",
        "bin/php/8.4.2",
        "bin/php/8.4.10",
        "bin/node/22.23.3",
    ] {
        rig.mkdir(dir);
    }
    let found = |service: &str, request: &str| {
        resolve_installed(&rig.paths, service, request)
            .unwrap()
            .map(|i| i.version.to_string())
    };
    assert_eq!(found("php", "8.4").as_deref(), Some("8.4.10"));
    assert_eq!(found("php", "8").as_deref(), Some("8.4.10"));
    assert_eq!(found("php", "8.3").as_deref(), Some("8.3.35"));
    assert_eq!(found("php", "8.4.2").as_deref(), Some("8.4.2"));
    assert_eq!(found("php", "7"), None);
    assert_eq!(found("ruby", "3"), None);
    assert_eq!(found("node", "22").as_deref(), Some("22.23.3"));
    assert!(matches!(
        resolve_installed(&rig.paths, "php", "nope"),
        Err(ManageError::Version(_))
    ));
}

// ---- `install --from` (ADR-0020) -----------------------------------------------------------

fn sha_of(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
}

impl Rig {
    fn install_from(
        &self,
        catalog: &Catalog,
        fetcher: &FakeFetcher,
        (service, version): (&str, &str),
        (archive, sha256): (&Path, &str),
    ) -> Result<Installed, ManageError> {
        let env = Environment {
            paths: &self.paths,
            catalog,
            target: WINDOWS,
            fetcher,
        };
        install_version_from(
            &env,
            service,
            version,
            LocalArchive {
                path: archive,
                sha256,
            },
            &InstallOptions::default(),
            &mut |_| {},
            &crate::paths::fake::FakePrivateDirs::new(),
        )
    }
}

/// A downloaded archive of `php` with the layout of `world`, and its hash.
fn downloaded(dir: &TempDir) -> (PathBuf, String) {
    let bytes = zip_of("php.exe");
    let path = dir.path().join("php-8.4.28.zip");
    fs::write(&path, &bytes).unwrap();
    (path, sha_of(&bytes))
}

#[test]
fn a_version_the_catalog_lacks_installs_from_a_file_and_is_marked_unverified() {
    let rig = Rig::new();
    let (catalog, fetcher) = world(&[("php", "8.4.26")]);
    let dir = TempDir::new().unwrap();
    let (archive, hash) = downloaded(&dir);

    let installed = rig
        .install_from(&catalog, &fetcher, ("php", "8.4.28"), (&archive, &hash))
        .unwrap();
    assert!(installed.unverified);
    assert_eq!(rig.installed(), ["php 8.4.28"]);
    let version = list_installed(&rig.paths).unwrap().remove(0);
    assert!(is_unverified_install(&version));
    // The layout (the executable) comes from the highest version the catalog has.
    assert!(installed.path.join("php.exe").is_file());
    assert_eq!(
        fetcher.requests(),
        Vec::<String>::new(),
        "nothing is downloaded"
    );
}

#[test]
fn the_hash_may_be_written_in_capitals_and_with_spaces_around() {
    let rig = Rig::new();
    let (catalog, fetcher) = world(&[("php", "8.4.26")]);
    let dir = TempDir::new().unwrap();
    let (archive, hash) = downloaded(&dir);
    let typed = format!("  {}\n", hash.to_uppercase());

    rig.install_from(&catalog, &fetcher, ("php", "8.4.28"), (&archive, &typed))
        .unwrap();
    assert_eq!(rig.installed(), ["php 8.4.28"]);
}

#[test]
fn a_file_with_the_catalogs_own_hash_installs_offline_and_verified() {
    let rig = Rig::new();
    let (catalog, fetcher) = world(&[("php", "8.4.26")]);
    let dir = TempDir::new().unwrap();
    let (archive, hash) = downloaded(&dir);

    let installed = rig
        .install_from(&catalog, &fetcher, ("php", "8.4.26"), (&archive, &hash))
        .unwrap();
    assert!(!installed.unverified);
    assert!(!is_unverified_install(
        &list_installed(&rig.paths).unwrap()[0]
    ));
    assert_eq!(fetcher.requests(), Vec::<String>::new());
}

#[test]
fn a_file_cannot_replace_a_version_the_catalog_has_with_another_hash() {
    let rig = Rig::new();
    let (catalog, fetcher) = world(&[("php", "8.4.26")]);
    let dir = TempDir::new().unwrap();
    let (archive, _) = downloaded(&dir);

    let error = rig
        .install_from(
            &catalog,
            &fetcher,
            ("php", "8.4.26"),
            (&archive, &sha_of(b"another file")),
        )
        .unwrap_err();
    assert!(
        matches!(error, ManageError::FromContradictsCatalog { .. }),
        "{error}"
    );
    assert_eq!(rig.installed(), Vec::<String>::new());
}

#[test]
fn from_needs_an_exact_version_and_a_real_hash() {
    let rig = Rig::new();
    let (catalog, fetcher) = world(&[("php", "8.4.26")]);
    let dir = TempDir::new().unwrap();
    let (archive, hash) = downloaded(&dir);

    for version in ["8.4", "latest", "8", "8.4.28-rc1"] {
        let error = rig
            .install_from(&catalog, &fetcher, ("php", version), (&archive, &hash))
            .unwrap_err();
        assert!(
            matches!(
                error,
                ManageError::FromNeedsExactVersion { .. } | ManageError::Version(_)
            ),
            "{version}: {error}"
        );
    }
    for bad in ["", "abc", &hash[1..], &"z".repeat(64)] {
        let error = rig
            .install_from(&catalog, &fetcher, ("php", "8.4.28"), (&archive, bad))
            .unwrap_err();
        assert!(matches!(error, ManageError::FromBadHash), "{bad}: {error}");
    }
    assert_eq!(rig.installed(), Vec::<String>::new());
}

#[test]
fn from_for_a_tool_the_catalog_does_not_know_is_refused() {
    let rig = Rig::new();
    let (catalog, fetcher) = world(&[("php", "8.4.26")]);
    let dir = TempDir::new().unwrap();
    let (archive, hash) = downloaded(&dir);

    let error = rig
        .install_from(&catalog, &fetcher, ("ruby", "3.4.1"), (&archive, &hash))
        .unwrap_err();
    assert!(
        matches!(error, ManageError::FromUnknownService(_)),
        "{error}"
    );
    assert_eq!(rig.installed(), Vec::<String>::new());
}

#[test]
fn a_file_that_is_not_the_one_vouched_for_installs_nothing() {
    let rig = Rig::new();
    let (catalog, fetcher) = world(&[("php", "8.4.26")]);
    let dir = TempDir::new().unwrap();
    let (archive, _) = downloaded(&dir);

    let error = rig
        .install_from(
            &catalog,
            &fetcher,
            ("php", "8.4.28"),
            (&archive, &sha_of(b"another file")),
        )
        .unwrap_err();
    assert!(
        matches!(
            error,
            ManageError::Install(InstallError::FileHashMismatch { .. })
        ),
        "{error}"
    );
    assert_eq!(rig.installed(), Vec::<String>::new());
}

/// A zip with `executable` inside `folder` (or at the top), and a catalog with one build of
/// `service` `version` whose `root` is `root`.
fn rooted(folder: Option<&str>, executable: &str) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default();
    let name = folder.map_or_else(|| executable.to_owned(), |f| format!("{f}/{executable}"));
    writer.start_file(name, options).unwrap();
    writer.write_all(b"MZ binary").unwrap();
    writer.finish().unwrap().into_inner()
}

fn one_build(service: &str, version: &str, root: Option<&str>, executable: &str) -> Catalog {
    let root = root.map_or_else(String::new, |r| format!("root = \"{r}\"\n"));
    Catalog::parse(&format!(
        "schema = 1\nserial = 1\n\n[[package]]\nservice = \"{service}\"\nversion = \"{version}\"\n\
         os = \"windows\"\narch = \"x86_64\"\nurl = \"https://example.invalid/x.zip\"\n\
         sha256 = \"{}\"\nformat = \"zip\"\n{root}executable = \"{executable}\"\n",
        "ab".repeat(32)
    ))
    .unwrap()
}

fn write_zip(dir: &TempDir, bytes: &[u8]) -> (PathBuf, String) {
    let path = dir.path().join("download.zip");
    fs::write(&path, bytes).unwrap();
    (path, sha_of(bytes))
}

#[test]
fn the_version_in_the_template_root_is_replaced_by_the_one_asked_for() {
    let rig = Rig::new();
    let catalog = one_build("node", "24.22.0", Some("node-v24.22.0-win-x64"), "node.exe");
    let dir = TempDir::new().unwrap();
    let (archive, hash) = write_zip(&dir, &rooted(Some("node-v24.23.0-win-x64"), "node.exe"));

    let installed = rig
        .install_from(
            &catalog,
            &FakeFetcher::new(),
            ("node", "24.23.0"),
            (&archive, &hash),
        )
        .unwrap();
    assert!(installed.path.join("node.exe").is_file());
    assert_eq!(rig.installed(), ["node 24.23.0"]);
}

#[test]
fn a_root_without_the_version_is_kept_and_two_part_versions_work() {
    let rig = Rig::new();
    let catalog = one_build("postgresql", "17.11", Some("pgsql"), "bin/postgres.exe");
    let dir = TempDir::new().unwrap();
    let (archive, hash) = write_zip(&dir, &rooted(Some("pgsql"), "bin/postgres.exe"));

    for wrong in ["17", "17.12.1"] {
        let error = rig
            .install_from(
                &catalog,
                &FakeFetcher::new(),
                ("postgresql", wrong),
                (&archive, &hash),
            )
            .unwrap_err();
        assert!(
            matches!(&error, ManageError::FromNeedsExactVersion { example, .. } if example == "17.11"),
            "{wrong}: {error}"
        );
    }
    rig.install_from(
        &catalog,
        &FakeFetcher::new(),
        ("postgresql", "17.12"),
        (&archive, &hash),
    )
    .unwrap();
    assert_eq!(rig.installed(), ["postgresql 17.12"]);
}

#[test]
fn an_unverified_install_is_confirmed_when_the_catalog_lists_it_with_the_same_hash() {
    let rig = Rig::new();
    let (old_catalog, fetcher) = world(&[("php", "8.4.26")]);
    let dir = TempDir::new().unwrap();
    let (archive, hash) = downloaded(&dir);
    rig.install_from(&old_catalog, &fetcher, ("php", "8.4.28"), (&archive, &hash))
        .unwrap();

    // A newer crisol.exe whose catalog lists 8.4.28 with that hash (`world` hashes the same zip).
    let (new_catalog, fetcher) = world(&[("php", "8.4.26"), ("php", "8.4.28")]);
    let error = rig
        .install(&new_catalog, &fetcher, "php", "8.4.28")
        .unwrap_err();
    assert!(matches!(
        error,
        ManageError::Install(InstallError::AlreadyInstalled(_))
    ));
    let version = list_installed(&rig.paths).unwrap().remove(0);
    assert!(!is_unverified_install(&version), "the mark is gone");
    assert_eq!(fetcher.requests(), Vec::<String>::new());
}

#[test]
fn an_unverified_install_with_another_hash_than_the_catalog_keeps_its_mark() {
    let rig = Rig::new();
    let (old_catalog, fetcher) = world(&[("php", "8.4.26")]);
    let dir = TempDir::new().unwrap();
    let bytes = rooted(None, "php.exe"); // not the zip `world` hashes
    let (archive, hash) = write_zip(&dir, &bytes);
    rig.install_from(&old_catalog, &fetcher, ("php", "8.4.28"), (&archive, &hash))
        .unwrap();

    let (new_catalog, fetcher) = world(&[("php", "8.4.28")]);
    rig.install(&new_catalog, &fetcher, "php", "8.4.28")
        .unwrap_err();
    let version = list_installed(&rig.paths).unwrap().remove(0);
    assert!(is_unverified_install(&version));
}
