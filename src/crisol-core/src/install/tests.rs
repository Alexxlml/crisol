//! Installer tests: an in-memory fetcher, real zips built on the fly, a temporary `CRISOL_HOME`.

use std::io::{Cursor, Write};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use sha2::{Digest, Sha256};
use tempfile::TempDir;
use zip::write::SimpleFileOptions;
use zip::CompressionMethod;

use super::*;
use crate::catalog::{Arch, Os, Target};
use crate::fetch::fake::FakeFetcher;
use crate::version::Version;

const URL: &str = "https://example.invalid/pkg.zip";

/// Builds a zip. Names are written exactly as given, so hostile ones can be tested.
fn zip_with(entries: &[(&str, &[u8])], method: CompressionMethod) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(method);
    for (name, data) in entries {
        if name.ends_with('/') {
            writer
                .add_directory(*name, options)
                .expect("directory entry");
        } else {
            writer.start_file(*name, options).expect("file entry");
            writer.write_all(data).expect("entry data");
        }
    }
    writer.finish().expect("finish zip").into_inner()
}

fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
    zip_with(entries, CompressionMethod::Deflated)
}

fn digest(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn package(
    service: &str,
    version: &str,
    bytes: &[u8],
    root: Option<&str>,
    executable: &str,
) -> Package {
    Package {
        service: service.to_owned(),
        version: Version::parse(version).unwrap(),
        target: Target {
            os: Os::Windows,
            arch: Arch::X86_64,
        },
        url: URL.to_owned(),
        sha256: digest(bytes),
        format: ArchiveFormat::Zip,
        root: root.map(str::to_owned),
        executable: executable.to_owned(),
        size: Some(u64::try_from(bytes.len()).unwrap()),
    }
}

struct Rig {
    dir: TempDir,
    paths: CrisolPaths,
    fetcher: FakeFetcher,
}

impl Rig {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let paths = CrisolPaths::new(dir.path().join("home")).unwrap();
        Self {
            dir,
            paths,
            fetcher: FakeFetcher::new(),
        }
    }

    fn install(&self, package: &Package) -> Result<Installed, InstallError> {
        self.install_with(package, &InstallOptions::default()).0
    }

    fn install_with(
        &self,
        package: &Package,
        options: &InstallOptions,
    ) -> (Result<Installed, InstallError>, Vec<Progress>) {
        let mut events = Vec::new();
        let result = install(package, &self.paths, &self.fetcher, options, &mut |p| {
            events.push(p);
        });
        (result, events)
    }

    /// Names inside `bin/`: nothing may be left over after a failure.
    fn bin_listing(&self) -> Vec<String> {
        let Ok(read) = fs::read_dir(self.paths.bin_dir()) else {
            return Vec::new();
        };
        let mut names: Vec<String> = read
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// Everything under the temp directory except `bin/`, to prove nothing was written elsewhere.
    fn outside_bin(&self) -> Vec<String> {
        let mut found = Vec::new();
        let mut stack = vec![self.dir.path().to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path == self.paths.bin_dir() {
                    continue;
                }
                found.push(
                    path.strip_prefix(self.dir.path())
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
                if path.is_dir() {
                    stack.push(path);
                }
            }
        }
        found.sort();
        found
    }

    /// `bin/` holds exactly one folder: the finished installation, with no staging leftovers.
    fn assert_only(&self, service: &str) {
        assert_eq!(self.bin_listing(), [service.to_owned()]);
    }

    fn assert_nothing_installed(&self) {
        assert_eq!(
            self.bin_listing(),
            Vec::<String>::new(),
            "bin/ must hold no leftovers"
        );
    }
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

// ---- the happy path -------------------------------------------------------------

#[test]
fn a_zip_is_installed_into_bin_service_version() {
    let rig = Rig::new();
    let bytes = zip_bytes(&[
        ("php.exe", b"MZ php"),
        ("php-cgi.exe", b"MZ cgi"),
        ("ext/", b""),
        ("ext/php_curl.dll", b"dll"),
    ]);
    rig.fetcher.serve(URL, bytes.clone());
    let pkg = package("php", "8.4.26", &bytes, None, "php.exe");

    let installed = rig.install(&pkg).unwrap();
    let dest = rig.paths.bin_for("php", "8.4.26").unwrap();
    assert_eq!(installed.path, dest);
    assert_eq!(installed.files, 3);
    assert_eq!(installed.downloaded, u64::try_from(bytes.len()).unwrap());
    assert_eq!(read(&dest.join("php.exe")), "MZ php");
    assert_eq!(read(&dest.join("ext").join("php_curl.dll")), "dll");
    assert_eq!(rig.bin_listing(), ["php"], "the staging folder is gone");
    assert_eq!(rig.fetcher.requests(), [URL]);
}

#[test]
fn the_archive_root_folder_is_hoisted() {
    let rig = Rig::new();
    let bytes = zip_bytes(&[
        ("nginx-1.30.5/", b""),
        ("nginx-1.30.5/nginx.exe", b"MZ nginx"),
        ("nginx-1.30.5/conf/", b""),
        ("nginx-1.30.5/conf/nginx.conf", b"worker_processes 1;"),
    ]);
    rig.fetcher.serve(URL, bytes.clone());
    let pkg = package("nginx", "1.30.5", &bytes, Some("nginx-1.30.5"), "nginx.exe");

    let installed = rig.install(&pkg).unwrap();
    assert_eq!(read(&installed.path.join("nginx.exe")), "MZ nginx");
    assert_eq!(
        read(&installed.path.join("conf").join("nginx.conf")),
        "worker_processes 1;"
    );
    assert!(
        !installed.path.join("nginx-1.30.5").exists(),
        "the wrapper folder must not remain"
    );
}

#[test]
fn a_single_file_package_is_installed_under_its_executable_name() {
    let rig = Rig::new();
    let bytes = b"#!/usr/bin/env php\n<?php echo 1;".to_vec();
    rig.fetcher.serve(URL, bytes.clone());
    let mut pkg = package("composer", "2.10.3", &bytes, None, "composer.phar");
    pkg.format = ArchiveFormat::File;

    let installed = rig.install(&pkg).unwrap();
    assert_eq!(installed.files, 1);
    assert_eq!(
        fs::read(installed.path.join("composer.phar")).unwrap(),
        bytes
    );
    assert_eq!(rig.bin_listing(), ["composer"]);
}

#[test]
fn versions_install_side_by_side_and_a_failure_leaves_the_other_intact() {
    let rig = Rig::new();
    let v1 = zip_bytes(&[("php.exe", b"one")]);
    rig.fetcher.serve(URL, v1.clone());
    let first = rig
        .install(&package("php", "8.3.35", &v1, None, "php.exe"))
        .unwrap();

    let v2 = zip_bytes(&[("php.exe", b"two")]);
    let mut broken = package("php", "8.4.26", &v2, None, "php.exe");
    broken.sha256 = "0".repeat(64);
    rig.fetcher.serve(URL, v2);
    assert!(matches!(
        rig.install(&broken),
        Err(InstallError::HashMismatch { .. })
    ));

    assert_eq!(read(&first.path.join("php.exe")), "one");
    assert_eq!(rig.bin_listing(), ["php"]);
    assert!(!rig.paths.bin_for("php", "8.4.26").unwrap().exists());
}

#[test]
fn progress_is_reported_in_order() {
    let rig = Rig::new();
    // Stored (uncompressed) and large enough for several progress steps.
    let big = vec![42_u8; 700 * 1024];
    let bytes = zip_with(
        &[("php.exe", &big), ("b.dll", b"b")],
        CompressionMethod::Stored,
    );
    rig.fetcher.serve(URL, bytes.clone());
    let pkg = package("php", "8.4.26", &bytes, None, "php.exe");
    let total = pkg.size;

    let (result, events) = rig.install_with(&pkg, &InstallOptions::default());
    result.unwrap();

    assert_eq!(events.first(), Some(&Progress::Connecting));
    assert_eq!(events.last(), Some(&Progress::Installing));
    let downloads: Vec<u64> = events
        .iter()
        .filter_map(|e| match e {
            Progress::Downloading { done, total: t } => {
                assert_eq!(*t, total);
                Some(*done)
            }
            _ => None,
        })
        .collect();
    assert!(
        downloads.len() >= 4,
        "several steps for a 700 KB download: {downloads:?}"
    );
    assert!(
        downloads.windows(2).all(|w| w[0] <= w[1]),
        "never goes backwards"
    );
    assert_eq!(downloads.first(), Some(&0));
    assert_eq!(downloads.last().copied(), total);

    let position = |wanted: &Progress| events.iter().position(|e| e == wanted).unwrap();
    assert!(
        position(&Progress::Verifying)
            > events
                .iter()
                .rposition(|e| matches!(e, Progress::Downloading { .. }))
                .unwrap()
                - 1
    );
    let extracting: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, Progress::Extracting { .. }))
        .collect();
    assert_eq!(
        extracting.last(),
        Some(&&Progress::Extracting { done: 2, total: 2 })
    );
    assert!(
        position(&Progress::Verifying)
            < events
                .iter()
                .position(|e| matches!(e, Progress::Extracting { .. }))
                .unwrap()
    );
}

// ---- verification comes before unpacking ----------------------------------------

#[test]
fn a_hash_mismatch_stops_before_anything_is_unpacked() {
    let rig = Rig::new();
    // This archive would be refused if it were ever opened (`..`), but its hash is wrong,
    // so the only acceptable error is the hash one: proof that the check comes first.
    let bytes = zip_bytes(&[("../evil.txt", b"x"), ("php.exe", b"MZ")]);
    rig.fetcher.serve(URL, bytes.clone());
    let mut pkg = package("php", "8.4.26", &bytes, None, "php.exe");
    pkg.sha256 = digest(b"something else entirely");

    let (result, events) = rig.install_with(&pkg, &InstallOptions::default());
    match result.unwrap_err() {
        InstallError::HashMismatch { expected, actual } => {
            assert_eq!(expected, pkg.sha256);
            assert_eq!(actual, digest(&bytes));
        }
        other => panic!("expected a hash mismatch, got {other}"),
    }
    assert!(events.contains(&Progress::Verifying));
    assert!(!events
        .iter()
        .any(|e| matches!(e, Progress::Extracting { .. } | Progress::Installing)));
    rig.assert_nothing_installed();
    assert_eq!(
        rig.outside_bin(),
        ["home"],
        "nothing may be written outside bin/"
    );
}

#[test]
fn a_tampered_download_of_the_right_size_is_caught_by_the_hash() {
    let rig = Rig::new();
    let genuine = zip_bytes(&[("php.exe", b"genuine")]);
    let pkg = package("php", "8.4.26", &genuine, None, "php.exe");
    let mut tampered = genuine.clone();
    let last = tampered.len() - 30;
    tampered[last] ^= 0xFF; // same length, one byte different
    rig.fetcher.serve(URL, tampered);

    assert!(matches!(
        rig.install(&pkg),
        Err(InstallError::HashMismatch { .. })
    ));
    rig.assert_nothing_installed();
}

// ---- hostile archives -----------------------------------------------------------

fn hostile(rig: &Rig, entries: &[(&str, &[u8])], root: Option<&str>) -> InstallError {
    let bytes = zip_bytes(entries);
    rig.fetcher.serve(URL, bytes.clone());
    let pkg = package("php", "8.4.26", &bytes, root, "php.exe");
    rig.install(&pkg)
        .expect_err("a hostile archive must not install")
}

#[test]
fn entries_that_try_to_leave_the_install_folder_are_refused() {
    let names = [
        "../evil.txt",
        "a/../../evil.txt",
        "..\\evil.txt",
        "a\\..\\..\\evil.txt",
        "/abs/evil.txt",
        "\\abs\\evil.txt",
        "C:/evil.txt",
        "C:\\evil.txt",
        "d/x:y.txt",
        "file.txt:hidden-stream",
        "a//b.txt",
    ];
    for name in names {
        let rig = Rig::new();
        let error = hostile(&rig, &[("php.exe", b"MZ"), (name, b"evil")], None);
        assert!(
            matches!(error, InstallError::UnsafeEntry { .. }),
            "`{name}` gave {error}"
        );
        rig.assert_nothing_installed();
        assert_eq!(
            rig.outside_bin(),
            ["home"],
            "`{name}` must not write anywhere else"
        );
    }
}

#[test]
fn windows_device_names_and_rewritten_names_are_refused() {
    for name in [
        "nul",
        "NUL.txt",
        "dir/CON",
        "com1.exe",
        "Lpt3",
        "aux.dll",
        "CONIN$",
        "trail.",
        "space ",
        "bad<name>.txt",
        "q?.txt",
        "pipe|.txt",
        "star*.txt",
    ] {
        let rig = Rig::new();
        let error = hostile(&rig, &[("php.exe", b"MZ"), (name, b"x")], None);
        assert!(
            matches!(error, InstallError::UnsafeEntry { .. }),
            "`{name}` gave {error}"
        );
        rig.assert_nothing_installed();
    }
}

#[test]
fn names_that_merely_resemble_reserved_ones_are_fine() {
    let rig = Rig::new();
    let bytes = zip_bytes(&[
        ("php.exe", b"MZ"),
        ("console.txt", b"a"),
        ("nullable.dll", b"b"),
        ("com10.txt", b"c"),
        ("auxiliary/x.txt", b"d"),
    ]);
    rig.fetcher.serve(URL, bytes.clone());
    rig.install(&package("php", "8.4.26", &bytes, None, "php.exe"))
        .unwrap();
}

#[test]
fn symbolic_links_are_refused() {
    let rig = Rig::new();
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default();
    writer.start_file("php.exe", options).unwrap();
    writer.write_all(b"MZ").unwrap();
    writer
        .add_symlink("link", "C:/Windows/System32", options)
        .unwrap();
    let bytes = writer.finish().unwrap().into_inner();
    rig.fetcher.serve(URL, bytes.clone());

    let error = rig
        .install(&package("php", "8.4.26", &bytes, None, "php.exe"))
        .unwrap_err();
    assert!(
        matches!(&error, InstallError::UnsafeEntry { reason, .. } if reason.contains("symbolic")),
        "{error}"
    );
    rig.assert_nothing_installed();
}

#[test]
fn entries_outside_the_declared_root_are_refused() {
    let rig = Rig::new();
    let error = hostile(
        &rig,
        &[("nginx-1.30.5/nginx.exe", b"MZ"), ("other/evil.exe", b"x")],
        Some("nginx-1.30.5"),
    );
    assert!(
        matches!(&error, InstallError::UnsafeEntry { reason, .. } if reason.contains("expected folder")),
        "{error}"
    );
    rig.assert_nothing_installed();

    // A sibling whose name merely starts with the root's name is not inside it.
    let rig = Rig::new();
    let error = hostile(
        &rig,
        &[("nginx-1.30.5-evil/x.exe", b"x")],
        Some("nginx-1.30.5"),
    );
    assert!(matches!(error, InstallError::UnsafeEntry { .. }), "{error}");
}

#[cfg(windows)]
#[test]
fn two_entries_that_differ_only_in_case_do_not_overwrite_each_other() {
    let rig = Rig::new();
    let error = hostile(
        &rig,
        &[
            ("php.exe", b"MZ"),
            ("Readme.txt", b"one"),
            ("README.TXT", b"two"),
        ],
        None,
    );
    assert!(
        matches!(&error, InstallError::UnsafeEntry { reason, .. } if reason.contains("twice")),
        "{error}"
    );
    rig.assert_nothing_installed();
}

#[test]
fn a_zip_bomb_is_stopped_by_the_extraction_limit() {
    let rig = Rig::new();
    // 8 MB of zeros deflate to a few KB.
    let zeros = vec![0_u8; 8 * 1024 * 1024];
    let bytes = zip_bytes(&[("php.exe", b"MZ"), ("bomb.bin", &zeros)]);
    assert!(
        bytes.len() < 100 * 1024,
        "the bomb must be tiny to be a bomb"
    );
    rig.fetcher.serve(URL, bytes.clone());

    let options = InstallOptions {
        max_extracted_bytes: 1024 * 1024,
        ..InstallOptions::default()
    };
    let (result, _) =
        rig.install_with(&package("php", "8.4.26", &bytes, None, "php.exe"), &options);
    assert!(
        matches!(result, Err(InstallError::ExtractedTooLarge { limit }) if limit == 1024 * 1024)
    );
    rig.assert_nothing_installed();
}

#[test]
fn an_archive_with_too_many_entries_is_refused() {
    let rig = Rig::new();
    let bytes = zip_bytes(&[
        ("php.exe", b"MZ"),
        ("a", b"1"),
        ("b", b"2"),
        ("c", b"3"),
        ("d", b"4"),
    ]);
    rig.fetcher.serve(URL, bytes.clone());
    let options = InstallOptions {
        max_entries: 4,
        ..InstallOptions::default()
    };
    let (result, _) =
        rig.install_with(&package("php", "8.4.26", &bytes, None, "php.exe"), &options);
    assert!(matches!(
        result,
        Err(InstallError::TooManyEntries { limit: 4 })
    ));
    rig.assert_nothing_installed();
}

#[test]
fn something_that_is_not_a_zip_is_a_damaged_archive() {
    let rig = Rig::new();
    let bytes = b"this is definitely not a zip file".to_vec();
    rig.fetcher.serve(URL, bytes.clone());
    let error = rig
        .install(&package("php", "8.4.26", &bytes, None, "php.exe"))
        .unwrap_err();
    assert!(matches!(error, InstallError::BadArchive(_)), "{error}");
    rig.assert_nothing_installed();
}

#[test]
fn an_archive_without_the_promised_executable_is_refused() {
    let rig = Rig::new();
    let bytes = zip_bytes(&[("readme.txt", b"hi"), ("bin/", b"")]);
    rig.fetcher.serve(URL, bytes.clone());
    let error = rig
        .install(&package("php", "8.4.26", &bytes, None, "php.exe"))
        .unwrap_err();
    assert!(
        matches!(&error, InstallError::MissingExecutable(name) if name == "php.exe"),
        "{error}"
    );
    rig.assert_nothing_installed();

    // A folder with the executable's name is not the executable.
    let rig = Rig::new();
    let bytes = zip_bytes(&[("php.exe/", b""), ("php.exe/inner", b"x")]);
    rig.fetcher.serve(URL, bytes.clone());
    let error = rig
        .install(&package("php", "8.4.26", &bytes, None, "php.exe"))
        .unwrap_err();
    assert!(
        matches!(error, InstallError::MissingExecutable(_)),
        "{error}"
    );
}

#[test]
fn tar_formats_are_not_supported_yet_and_leave_nothing() {
    for format in [ArchiveFormat::TarGz, ArchiveFormat::TarXz] {
        let rig = Rig::new();
        let bytes = b"tarball".to_vec();
        rig.fetcher.serve(URL, bytes.clone());
        let mut pkg = package("php", "8.4.26", &bytes, None, "php");
        pkg.format = format;
        assert!(
            matches!(rig.install(&pkg), Err(InstallError::UnsupportedFormat(f)) if f == format)
        );
        rig.assert_nothing_installed();
    }
}

#[test]
fn very_deep_paths_unpack_on_windows_too() {
    // About 360 characters below the install folder: beyond the classic 260-character limit.
    let deep = "dir00_abcde/".repeat(30); // 360 characters of folders
    let file = format!("{deep}leaf.txt");
    let rig = Rig::new();
    let bytes = zip_bytes(&[("php.exe", b"MZ"), (file.as_str(), b"deep")]);
    rig.fetcher.serve(URL, bytes.clone());
    let installed = rig
        .install(&package("php", "8.4.26", &bytes, None, "php.exe"))
        .unwrap();
    assert_eq!(read(&installed.path.join(&file)), "deep");
}

// ---- download problems ----------------------------------------------------------

#[test]
fn download_failures_leave_nothing_behind() {
    let bytes = zip_bytes(&[("php.exe", b"MZ")]);
    let pkg = package("php", "8.4.26", &bytes, None, "php.exe");

    let rig = Rig::new();
    assert!(
        matches!(
            rig.install(&pkg),
            Err(InstallError::Fetch(FetchError::Status(404)))
        ),
        "unknown URL"
    );
    rig.assert_nothing_installed();

    rig.fetcher.unreachable(URL, "connection refused");
    assert!(matches!(
        rig.install(&pkg),
        Err(InstallError::Fetch(FetchError::Network(_)))
    ));
    rig.assert_nothing_installed();

    rig.fetcher.status(URL, 503);
    assert!(matches!(
        rig.install(&pkg),
        Err(InstallError::Fetch(FetchError::Status(503)))
    ));
    rig.assert_nothing_installed();
}

#[test]
fn a_connection_that_drops_mid_download_is_reported_and_cleaned_up() {
    let rig = Rig::new();
    let bytes = zip_bytes(&[("php.exe", &[1_u8; 4000])]);
    let pkg = package("php", "8.4.26", &bytes, None, "php.exe");
    rig.fetcher
        .cut_after(URL, bytes[..bytes.len() / 2].to_vec(), pkg.size.unwrap());

    let error = rig.install(&pkg).unwrap_err();
    assert!(matches!(error, InstallError::Interrupted(_)), "{error}");
    rig.assert_nothing_installed();
}

#[test]
fn a_size_that_disagrees_with_the_catalog_is_refused() {
    let bytes = zip_bytes(&[("php.exe", b"MZ")]);
    let pkg = package("php", "8.4.26", &bytes, None, "php.exe");
    let real = pkg.size.unwrap();

    // The server announces a different size: refused before downloading anything.
    let rig = Rig::new();
    rig.fetcher
        .serve_announcing(URL, bytes.clone(), Some(real + 1000));
    assert!(
        matches!(rig.install(&pkg), Err(InstallError::SizeMismatch { expected, actual }) if expected == real && actual == real + 1000)
    );
    rig.assert_nothing_installed();

    // It announces nothing and sends too many bytes: cut at the limit.
    let rig = Rig::new();
    let mut longer = bytes.clone();
    longer.extend_from_slice(&[0; 500]);
    rig.fetcher.serve_announcing(URL, longer, None);
    assert!(matches!(rig.install(&pkg), Err(InstallError::TooLarge { limit }) if limit == real));
    rig.assert_nothing_installed();

    // It announces nothing and sends too few, ending cleanly: also refused.
    let rig = Rig::new();
    rig.fetcher
        .serve_announcing(URL, bytes[..bytes.len() - 10].to_vec(), None);
    assert!(matches!(
        rig.install(&pkg),
        Err(InstallError::SizeMismatch { .. })
    ));
    rig.assert_nothing_installed();
}

#[test]
fn without_a_size_in_the_catalog_a_hard_limit_applies() {
    let rig = Rig::new();
    let bytes = vec![9_u8; 5000];
    let mut pkg = package("php", "8.4.26", &bytes, None, "php.exe");
    pkg.size = None;
    rig.fetcher.serve_announcing(URL, bytes, None);
    let options = InstallOptions {
        max_download_bytes: 1000,
        ..InstallOptions::default()
    };
    let (result, _) = rig.install_with(&pkg, &options);
    assert!(matches!(
        result,
        Err(InstallError::TooLarge { limit: 1000 })
    ));
    rig.assert_nothing_installed();
}

// ---- cancel, existing installs, bad names ---------------------------------------

#[test]
fn a_cancel_before_starting_does_not_even_connect() {
    let rig = Rig::new();
    let bytes = zip_bytes(&[("php.exe", b"MZ")]);
    rig.fetcher.serve(URL, bytes.clone());
    let options = InstallOptions {
        cancel: Some(Arc::new(AtomicBool::new(true))),
        ..InstallOptions::default()
    };
    let (result, _) =
        rig.install_with(&package("php", "8.4.26", &bytes, None, "php.exe"), &options);
    assert!(matches!(result, Err(InstallError::Cancelled)));
    assert_eq!(rig.fetcher.requests(), Vec::<String>::new());
    rig.assert_nothing_installed();
}

#[test]
fn a_cancel_during_the_download_or_the_unpacking_cleans_up() {
    let big = vec![5_u8; 900 * 1024];
    let bytes = zip_with(
        &[("php.exe", b"MZ"), ("big.bin", &big), ("z.txt", b"z")],
        CompressionMethod::Stored,
    );

    for trigger in ["download", "extract"] {
        let rig = Rig::new();
        rig.fetcher.serve(URL, bytes.clone());
        let flag = Arc::new(AtomicBool::new(false));
        let options = InstallOptions {
            cancel: Some(Arc::clone(&flag)),
            ..InstallOptions::default()
        };
        let pkg = package("php", "8.4.26", &bytes, None, "php.exe");
        let mut events = Vec::new();
        let result = install(&pkg, &rig.paths, &rig.fetcher, &options, &mut |p| {
            let hit = match (&p, trigger) {
                (Progress::Downloading { done, .. }, "download") => *done > 300 * 1024,
                (Progress::Extracting { done, .. }, "extract") => *done >= 1,
                _ => false,
            };
            if hit {
                flag.store(true, Ordering::Relaxed);
            }
            events.push(p);
        });
        assert!(
            matches!(result, Err(InstallError::Cancelled)),
            "{trigger}: {result:?}"
        );
        assert!(!events.contains(&Progress::Installing), "{trigger}");
        rig.assert_nothing_installed();
    }
}

#[test]
fn an_existing_installation_is_never_touched() {
    let rig = Rig::new();
    let dest = rig.paths.bin_for("php", "8.4.26").unwrap();
    fs::create_dir_all(&dest).unwrap();
    fs::write(dest.join("my-notes.txt"), "keep me").unwrap();

    let bytes = zip_bytes(&[("php.exe", b"MZ")]);
    rig.fetcher.serve(URL, bytes.clone());
    let error = rig
        .install(&package("php", "8.4.26", &bytes, None, "php.exe"))
        .unwrap_err();
    assert!(
        matches!(&error, InstallError::AlreadyInstalled(p) if *p == dest),
        "{error}"
    );
    assert_eq!(read(&dest.join("my-notes.txt")), "keep me");
    assert_eq!(
        rig.fetcher.requests(),
        Vec::<String>::new(),
        "nothing is downloaded for an installed version"
    );
    assert_eq!(rig.bin_listing(), ["php"]);
}

#[test]
fn a_package_name_that_is_not_a_safe_folder_name_is_refused() {
    let rig = Rig::new();
    let bytes = zip_bytes(&[("php.exe", b"MZ")]);
    rig.fetcher.serve(URL, bytes.clone());
    for service in ["../escape", "a/b", "", "C:"] {
        let pkg = package(service, "8.4.26", &bytes, None, "php.exe");
        assert!(
            matches!(rig.install(&pkg), Err(InstallError::Paths(_))),
            "`{service}`"
        );
    }
    assert_eq!(rig.outside_bin(), Vec::<String>::new());
}

// ---- staging folders ------------------------------------------------------------

#[test]
fn stale_staging_folders_are_swept_but_installations_are_not() {
    let rig = Rig::new();
    let bin = rig.paths.bin_dir();
    fs::create_dir_all(bin.join(".staging-dead").join("x")).unwrap();
    fs::write(bin.join(".staging-dead").join("x").join("half.bin"), "half").unwrap();
    fs::create_dir_all(bin.join("php").join("8.4.26")).unwrap();
    fs::write(bin.join("loose-file.txt"), "not a folder").unwrap();

    assert_eq!(
        remove_stale_staging(&bin, Duration::from_secs(3600)).unwrap(),
        0,
        "too fresh to be stale"
    );
    assert!(bin.join(".staging-dead").exists());

    assert_eq!(remove_stale_staging(&bin, Duration::ZERO).unwrap(), 1);
    assert!(!bin.join(".staging-dead").exists());
    assert!(
        bin.join("php").join("8.4.26").exists(),
        "installations are never swept"
    );
    assert!(bin.join("loose-file.txt").exists());
}

#[test]
fn an_install_does_not_disturb_a_fresh_staging_folder_of_another_install() {
    let rig = Rig::new();
    let other = rig.paths.bin_dir().join(".staging-running-elsewhere");
    fs::create_dir_all(&other).unwrap();

    let bytes = zip_bytes(&[("php.exe", b"MZ")]);
    rig.fetcher.serve(URL, bytes.clone());
    rig.install(&package("php", "8.4.26", &bytes, None, "php.exe"))
        .unwrap();
    assert!(
        other.exists(),
        "a concurrent install's staging folder must survive"
    );
}

// ---- helpers --------------------------------------------------------------------

#[test]
fn retry_tries_again_and_returns_the_last_error() {
    let mut calls = 0;
    let value = retry(5, Duration::ZERO, || {
        calls += 1;
        if calls < 3 {
            Err(io::Error::other("busy"))
        } else {
            Ok(calls)
        }
    });
    assert_eq!(value.unwrap(), 3);

    let mut calls = 0;
    let error = retry(4, Duration::ZERO, || -> io::Result<()> {
        calls += 1;
        Err(io::Error::other(format!("attempt {calls}")))
    })
    .unwrap_err();
    assert_eq!((calls, error.to_string().as_str()), (4, "attempt 4"));

    let mut calls = 0;
    let _ = retry(0, Duration::ZERO, || -> io::Result<()> {
        calls += 1;
        Err(io::Error::other("x"))
    });
    assert_eq!(calls, 1, "zero attempts still means one try");
}

#[test]
fn safe_relative_path_accepts_normal_names_and_strips_the_root() {
    let ok = |name: &str, root: Option<&str>| safe_relative_path(name, root).unwrap();
    assert_eq!(ok("php.exe", None), Some(PathBuf::from("php.exe")));
    assert_eq!(
        ok("ext/php_curl.dll", None),
        Some(PathBuf::from("ext").join("php_curl.dll"))
    );
    assert_eq!(ok("ext/", None), Some(PathBuf::from("ext")));
    assert_eq!(
        ok("a\\b\\c.txt", None),
        Some(PathBuf::from("a").join("b").join("c.txt"))
    );
    assert_eq!(
        ok("nginx-1.30.5/", Some("nginx-1.30.5")),
        None,
        "the root itself"
    );
    assert_eq!(ok("nginx-1.30.5", Some("nginx-1.30.5")), None);
    assert_eq!(
        ok("nginx-1.30.5/conf/x.conf", Some("nginx-1.30.5")),
        Some(PathBuf::from("conf").join("x.conf"))
    );
    assert_eq!(
        ok("app/bin/tool.exe", Some("app/bin")),
        Some(PathBuf::from("tool.exe")),
        "a root can be nested"
    );
    assert_eq!(
        ok(".hidden", None),
        Some(PathBuf::from(".hidden")),
        "dot files are fine"
    );
    assert_eq!(
        ok("a..b/c", None),
        Some(PathBuf::from("a..b").join("c")),
        "`..` inside a name is fine"
    );
}

#[test]
fn hex_is_lowercase_and_zero_padded() {
    assert_eq!(hex(&[0x00, 0x0a, 0xff, 0x10]), "000aff10");
    assert_eq!(hex(&[]), "");
    assert_eq!(
        digest(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

// ---- the real archives ----------------------------------------------------------

/// Installs every package of the built-in catalog from the real, already downloaded files,
/// so the strict checks are proven not to reject what the providers actually ship.
///
/// It needs the files, so it only runs on request:
///
/// ```text
/// CRISOL_TEST_DOWNLOADS=<folder with the downloads> cargo test -p crisol-core real_ -- --ignored
/// ```
///
/// Run it after every catalog change (see `analisis/fuentes-binarios.md`).
#[test]
#[ignore = "needs CRISOL_TEST_DOWNLOADS pointing at the downloaded official archives"]
fn real_provider_archives_install_cleanly() {
    let Some(folder) = std::env::var_os("CRISOL_TEST_DOWNLOADS").map(PathBuf::from) else {
        panic!("set CRISOL_TEST_DOWNLOADS to the folder with the downloaded archives");
    };
    let catalog = crate::catalog::Catalog::embedded().unwrap();
    let mut installed = 0;
    for package in catalog.packages() {
        let last = package.url.rsplit('/').next().unwrap();
        let extension = last.rsplit_once('.').map_or("", |(_, e)| e);
        let candidates = [
            folder.join(last),
            folder.join(format!(
                "{}-{}.{extension}",
                package.service, package.version
            )),
        ];
        let Some(file) = candidates.iter().find(|c| c.is_file()) else {
            eprintln!(
                "skipping {}: no downloaded file in {}",
                package.label(),
                folder.display()
            );
            continue;
        };
        let rig = Rig::new();
        rig.fetcher.serve(&package.url, fs::read(file).unwrap());

        let result = rig.install(package);
        let done = result.unwrap_or_else(|e| panic!("{} failed to install: {e}", package.label()));
        assert!(
            done.path.join(&package.executable).is_file(),
            "{}",
            package.label()
        );
        rig.assert_only(&package.service);
        eprintln!("ok {:<28} {:>6} files", package.label(), done.files);
        installed += 1;
    }
    assert!(
        installed > 0,
        "nothing was installed: is {} the right folder?",
        folder.display()
    );
}

/// The full path over the real network: HTTPS and TLS from the official site, hash check
/// against the catalog, install. Uses the smallest package (Composer, about 3.6 MB).
///
/// ```text
/// cargo test -p crisol-core real_network -- --ignored
/// ```
#[test]
#[ignore = "downloads about 3.6 MB from getcomposer.org"]
fn real_network_install_of_composer() {
    use crate::fetch::HttpFetcher;

    let catalog = crate::catalog::Catalog::embedded().unwrap();
    let package = catalog
        .latest(
            "composer",
            Target {
                os: Os::Windows,
                arch: Arch::X86_64,
            },
        )
        .unwrap();
    let rig = Rig::new();
    let mut steps = 0;
    let installed = install(
        package,
        &rig.paths,
        &HttpFetcher::new(),
        &InstallOptions::default(),
        &mut |p| {
            if matches!(p, Progress::Downloading { .. }) {
                steps += 1;
            }
        },
    )
    .expect("the official download must verify against the catalog");

    assert_eq!(installed.downloaded, package.size.unwrap());
    assert!(installed.path.join("composer.phar").is_file());
    assert!(
        steps > 3,
        "a 3.6 MB download reports several progress steps, got {steps}"
    );
    rig.assert_only("composer");
}
