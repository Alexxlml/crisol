use std::fs;

use tempfile::TempDir;

use super::*;

struct World {
    _tmp: TempDir,
    paths: CrisolPaths,
    config: Config,
    www: PathBuf,
    elsewhere: PathBuf,
}

fn world() -> World {
    let tmp = TempDir::new().unwrap();
    let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
    let www = paths.home().join("www");
    let elsewhere = tmp.path().join("code");
    fs::create_dir_all(&www).unwrap();
    fs::create_dir_all(&elsewhere).unwrap();
    World {
        _tmp: tmp,
        paths,
        config: Config::default(),
        www,
        elsewhere,
    }
}

fn touch(dir: &Path, relative: &str) {
    let file = dir.join(relative);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, "x").unwrap();
}

fn found(world: &World) -> SiteList {
    let linked = SitesFile::load(&world.paths).unwrap();
    discover(&world.paths, &world.config, &linked)
}

fn names(list: &SiteList) -> Vec<&str> {
    list.sites.iter().map(|s| s.name.as_str()).collect()
}

// ---- document root ----------------------------------------------------------------

#[test]
fn the_document_root_is_detected_from_what_the_folder_holds() {
    let tmp = TempDir::new().unwrap();
    let case = |files: &[&str]| {
        let dir = tempfile::tempdir_in(tmp.path()).unwrap();
        for file in files {
            touch(dir.path(), file);
        }
        let (docroot, rule) = detect_docroot(dir.path());
        (
            docroot.strip_prefix(dir.path()).unwrap().to_path_buf(),
            rule,
        )
    };
    assert_eq!(
        case(&["artisan", "public/index.php", "composer.json"]),
        (PathBuf::from("public"), DocRoot::Laravel)
    );
    assert_eq!(
        case(&["bin/console", "public/index.php"]),
        (PathBuf::from("public"), DocRoot::Symfony)
    );
    assert_eq!(
        case(&["public/index.php"]),
        (PathBuf::from("public"), DocRoot::PublicFolder)
    );
    assert_eq!(
        case(&["public/index.html"]),
        (PathBuf::from("public"), DocRoot::PublicFolder)
    );
    assert_eq!(
        case(&["web/index.php"]),
        (PathBuf::from("web"), DocRoot::WebFolder)
    );
    assert_eq!(
        case(&["wp-config.php", "index.php", "wp-content/index.php"]),
        (PathBuf::new(), DocRoot::WordPress)
    );
    assert_eq!(
        case(&["index.php"]),
        (PathBuf::new(), DocRoot::Root),
        "a plain PHP site is served from its folder"
    );
    assert_eq!(case(&[]), (PathBuf::new(), DocRoot::Root));
}

#[test]
fn wordpress_wins_over_a_public_folder_it_happens_to_have() {
    let tmp = TempDir::new().unwrap();
    touch(tmp.path(), "wp-load.php");
    touch(tmp.path(), "public/index.php");
    assert_eq!(detect_docroot(tmp.path()).1, DocRoot::WordPress);
}

// ---- parked -----------------------------------------------------------------------

#[test]
fn every_subfolder_of_a_parked_directory_is_a_site() {
    let world = world();
    touch(&world.www.join("blog"), "index.php");
    touch(&world.www.join("shop"), "artisan");
    touch(&world.www.join("shop"), "public/index.php");
    fs::write(world.www.join("notes.txt"), "not a site").unwrap();
    fs::create_dir(world.www.join(".git")).unwrap();

    let list = found(&world);
    assert_eq!(names(&list), ["blog", "shop"]);
    assert_eq!(list.warnings, Vec::<String>::new());
    let shop = &list.sites[1];
    assert_eq!(shop.domain, "shop.test");
    assert_eq!(shop.kind, SiteKind::Parked);
    assert_eq!(shop.root, world.www.join("shop"));
    assert_eq!(shop.docroot, world.www.join("shop").join("public"));
    assert_eq!(shop.docroot_rule, DocRoot::Laravel);
}

#[test]
fn the_suffix_comes_from_the_configuration() {
    let mut world = world();
    world.config.sites.tld = "localhost".to_owned();
    fs::create_dir(world.www.join("app")).unwrap();
    assert_eq!(found(&world).sites[0].domain, "app.localhost");
}

#[test]
fn folder_names_are_lowercased_and_unusable_ones_are_reported_not_fatal() {
    let world = world();
    for dir in ["MyApp", "my app", "my_app", "ok-site"] {
        fs::create_dir(world.www.join(dir)).unwrap();
    }
    let list = found(&world);
    assert_eq!(names(&list), ["myapp", "ok-site"]);
    assert_eq!(list.warnings.len(), 2, "{:?}", list.warnings);
    assert!(list.warnings.iter().all(|w| w.contains("not served")));
}

#[test]
fn a_missing_parked_directory_has_no_sites_and_no_warning() {
    let mut world = world();
    world.config.sites.parked = vec![PathBuf::from("nowhere")];
    let list = found(&world);
    assert_eq!(list, SiteList::default());
}

#[test]
fn several_parked_directories_are_read_and_absolute_ones_are_kept_as_they_are() {
    let mut world = world();
    let more = world.elsewhere.join("projects");
    fs::create_dir_all(more.join("api")).unwrap();
    fs::create_dir(world.www.join("blog")).unwrap();
    world.config.sites.parked = vec![PathBuf::from("www"), more.clone()];
    let list = found(&world);
    assert_eq!(names(&list), ["api", "blog"]);
    assert_eq!(list.sites[0].root, more.join("api"));
}

#[test]
fn the_same_name_in_two_parked_directories_serves_the_first_and_says_so() {
    let mut world = world();
    let more = world.elsewhere.join("projects");
    fs::create_dir_all(more.join("blog")).unwrap();
    fs::create_dir(world.www.join("blog")).unwrap();
    world.config.sites.parked = vec![PathBuf::from("www"), more];
    let list = found(&world);
    assert_eq!(list.sites.len(), 1);
    assert_eq!(list.sites[0].root, world.www.join("blog"));
    assert!(
        list.warnings[0].contains("already belongs"),
        "{:?}",
        list.warnings
    );
}

// ---- linked -----------------------------------------------------------------------

#[test]
fn a_linked_site_is_served_from_anywhere_under_its_own_name() {
    let world = world();
    let project = world.elsewhere.join("client-work");
    touch(&project, "wp-config.php");
    assert!(link(&world.paths, "client", &project, None).unwrap());

    let list = found(&world);
    assert_eq!(names(&list), ["client"]);
    let site = &list.sites[0];
    assert_eq!(
        (site.kind, site.domain.as_str()),
        (SiteKind::Linked, "client.test")
    );
    assert_eq!(site.root, project);
    assert_eq!(site.docroot_rule, DocRoot::WordPress);
}

#[test]
fn a_linked_site_can_name_its_document_root() {
    let world = world();
    let project = world.elsewhere.join("odd");
    touch(&project, "public/index.php");
    touch(&project, "dist/index.html");
    link(&world.paths, "odd", &project, Some("dist")).unwrap();
    let site = &found(&world).sites[0];
    assert_eq!(
        (site.docroot.clone(), site.docroot_rule),
        (project.join("dist"), DocRoot::Explicit)
    );

    // Linking again without one goes back to detection.
    link(&world.paths, "odd", &project, None).unwrap();
    assert_eq!(found(&world).sites[0].docroot_rule, DocRoot::PublicFolder);
}

#[test]
fn a_document_root_that_does_not_exist_falls_back_to_detection_with_a_warning() {
    let world = world();
    let project = world.elsewhere.join("odd");
    touch(&project, "index.php");
    link(&world.paths, "odd", &project, Some("dist")).unwrap();
    let list = found(&world);
    assert_eq!(list.sites[0].docroot_rule, DocRoot::Root);
    assert!(list.warnings[0].contains("dist"), "{:?}", list.warnings);
}

#[test]
fn a_linked_site_replaces_a_parked_folder_of_the_same_name() {
    let world = world();
    fs::create_dir(world.www.join("blog")).unwrap();
    let project = world.elsewhere.join("my-blog");
    fs::create_dir(&project).unwrap();
    link(&world.paths, "blog", &project, None).unwrap();
    let list = found(&world);
    assert_eq!(list.sites.len(), 1);
    assert_eq!(list.sites[0].root, project);
    assert_eq!(list.sites[0].kind, SiteKind::Linked);
    assert_eq!(list.warnings.len(), 1);
}

#[test]
fn a_linked_folder_that_is_gone_is_left_out_with_a_warning() {
    let world = world();
    let project = world.elsewhere.join("temp");
    fs::create_dir(&project).unwrap();
    link(&world.paths, "temp", &project, None).unwrap();
    fs::create_dir(world.www.join("other")).unwrap();
    fs::remove_dir(&project).unwrap();
    let list = found(&world);
    assert_eq!(names(&list), ["other"], "the others are not affected");
    assert!(list.warnings[0].contains("temp"), "{:?}", list.warnings);
}

#[test]
fn linking_keeps_comments_and_other_entries_and_unlinking_removes_only_one() {
    let world = world();
    let (a, b) = (world.elsewhere.join("a"), world.elsewhere.join("b"));
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    let original = "# my sites\n[linked.keep]\npath = 'C:/keep' # the old one\n";
    fs::write(world.paths.sites_file(), original).unwrap();

    link(&world.paths, "a", &a, None).unwrap();
    link(&world.paths, "b", &b, None).unwrap();
    let text = fs::read_to_string(world.paths.sites_file()).unwrap();
    assert!(text.starts_with(original), "{text}");

    assert!(unlink(&world.paths, "a").unwrap());
    assert!(!unlink(&world.paths, "a").unwrap(), "already gone");
    let file = SitesFile::load(&world.paths).unwrap();
    assert_eq!(
        file.linked.keys().map(String::as_str).collect::<Vec<_>>(),
        ["b", "keep"]
    );
    assert!(a.is_dir(), "the project folder is never touched");
    assert!(fs::read_to_string(world.paths.sites_file())
        .unwrap()
        .contains("# my sites"));
}

#[test]
fn linking_the_same_thing_twice_changes_nothing() {
    let world = world();
    link(&world.paths, "a", &world.elsewhere, None).unwrap();
    assert!(!link(&world.paths, "a", &world.elsewhere, None).unwrap());
}

#[test]
fn bad_requests_are_refused_and_write_nothing() {
    let world = world();
    let folder = world.elsewhere.clone();
    for bad in [
        "Has Caps",
        "has space",
        "dot.ted",
        "-lead",
        "trail-",
        "",
        "under_score",
    ] {
        assert!(
            matches!(
                link(&world.paths, bad, &folder, None),
                Err(SitesError::Invalid { .. })
            ),
            "{bad}"
        );
    }
    assert!(matches!(
        link(&world.paths, "x", Path::new("relative/dir"), None),
        Err(SitesError::Invalid { .. })
    ));
    assert!(matches!(
        link(&world.paths, "x", &folder.join("missing"), None),
        Err(SitesError::NotAFolder(_))
    ));
    for docroot in ["../up", "/abs", "a/../../b", ""] {
        assert!(
            matches!(
                link(&world.paths, "x", &folder, Some(docroot)),
                Err(SitesError::Invalid { .. })
            ),
            "{docroot}"
        );
    }
    assert!(!world.paths.sites_file().exists());
}

// ---- reading sites.toml -----------------------------------------------------------

#[test]
fn a_missing_file_is_an_empty_list_and_unknown_fields_only_warn() {
    let world = world();
    assert_eq!(SitesFile::load(&world.paths).unwrap(), SitesFile::default());

    let file = SitesFile::parse("[linked.a]\npath = 'C:/a'\ncolour = 'red'\n").unwrap();
    assert_eq!(file.linked.len(), 1);
    assert_eq!(file.warnings.len(), 1);
}

#[test]
fn a_broken_file_is_an_error_that_names_the_entry() {
    for (text, field) in [
        ("[linked.a]\npath = 'relative'\n", "linked.a.path"),
        ("[linked.Bad]\npath = 'C:/a'\n", "linked.Bad"),
        (
            "[linked.a]\npath = 'C:/a'\ndocroot = '../x'\n",
            "linked.a.docroot",
        ),
    ] {
        let error = SitesFile::parse(text).unwrap_err().to_string();
        assert!(error.contains(field), "{error}");
    }
    assert!(
        SitesFile::parse("[linked.a]\n").is_err(),
        "path is required"
    );
}

#[test]
fn the_editor_refuses_to_save_a_sites_file_that_could_not_be_read_back() {
    let world = world();
    let mut editor = TomlEditor::open_sites(&world.paths).unwrap();
    editor.set(&["linked", "a", "path"], "relative").unwrap();
    assert!(matches!(editor.save(), Err(EditError::Rejected(_))));
    assert!(!world.paths.sites_file().exists());
}
