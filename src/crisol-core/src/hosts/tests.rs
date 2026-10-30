use super::*;

const HERD: &str = "# Copyright (c) 1993-2009 Microsoft Corp.\r\n\r\n127.0.0.1 localhost\r\n\
                    # <HERD>\r\n127.0.0.1 old.test # herd\r\n# </HERD>\r\n";

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn the_block_is_added_at_the_end_with_the_files_own_line_endings() {
    let update = apply_block(HERD, &["shop.test", "api.shop.test"]).unwrap();
    assert!(
        update.content.starts_with(HERD),
        "nothing before it changes"
    );
    let block = &update.content[HERD.len()..];
    assert_eq!(
        block,
        "# BEGIN CRISOL\r\n\
         # Managed by Crisol: whatever is between these markers is rewritten.\r\n\
         127.0.0.1 api.shop.test\r\n::1 api.shop.test\r\n\
         127.0.0.1 shop.test\r\n::1 shop.test\r\n\
         # END CRISOL\r\n"
    );
    assert_eq!(update.conflicts, Vec::new());
    assert_eq!(
        block_names(&update.content).unwrap(),
        names(&["api.shop.test", "shop.test"])
    );

    let unix = apply_block("127.0.0.1 localhost\n", &["a.test"])
        .unwrap()
        .content;
    assert!(!unix.contains('\r'), "{unix:?}");
}

#[test]
fn removing_the_block_brings_the_file_back_byte_for_byte() {
    for original in [
        HERD,
        "",
        "127.0.0.1 localhost\n",
        "# only a comment\r\n\r\n\r\n",
    ] {
        let with = apply_block(original, &["shop.test"]).unwrap().content;
        assert_ne!(with, original);
        assert_eq!(remove_block(&with).unwrap(), original, "{original:?}");
    }
    // A file with no final newline gets one, and keeps it: the one documented difference.
    let with = apply_block("127.0.0.1 localhost", &["a.test"])
        .unwrap()
        .content;
    assert_eq!(remove_block(&with).unwrap(), "127.0.0.1 localhost\n");
    // Nothing to remove is not a change.
    assert_eq!(remove_block(HERD).unwrap(), HERD);
}

#[test]
fn a_block_that_is_there_is_replaced_and_what_surrounds_it_stays() {
    let before = "a\r\nb\r\n";
    let after = "c\r\n";
    let first = apply_block(&format!("{before}{after}"), &["one.test"])
        .unwrap()
        .content;
    // Put something after the block and replace it.
    let edited = format!("{first}# later\r\n127.0.0.1 later.local\r\n");
    let second = apply_block(&edited, &["two.test", "three.test"])
        .unwrap()
        .content;
    assert!(second.starts_with(&format!("{before}{after}# BEGIN CRISOL")));
    assert!(second.ends_with("# END CRISOL\r\n# later\r\n127.0.0.1 later.local\r\n"));
    assert!(!second.contains("one.test"));
    assert_eq!(
        block_names(&second).unwrap(),
        names(&["three.test", "two.test"])
    );
    assert_eq!(second.matches("# BEGIN CRISOL").count(), 1);
    assert_eq!(
        remove_block(&second).unwrap(),
        format!("{before}{after}# later\r\n127.0.0.1 later.local\r\n")
    );
}

#[test]
fn applying_the_same_names_twice_changes_nothing() {
    let once = apply_block(HERD, &["b.test", "a.test", "A.test"])
        .unwrap()
        .content;
    let twice = apply_block(&once, &["a.test", "b.test"]).unwrap().content;
    assert_eq!(once, twice);
}

#[test]
fn no_names_removes_the_block() {
    let with = apply_block(HERD, &["a.test"]).unwrap().content;
    let none: [&str; 0] = [];
    assert_eq!(apply_block(&with, &none).unwrap().content, HERD);
}

#[test]
fn a_damaged_block_is_refused_not_repaired() {
    let cases = [
        ("a\r\n# BEGIN CRISOL\r\nx\r\n", "never ends"),
        ("a\r\n# END CRISOL\r\n", "no start"),
        ("# END CRISOL\r\n# BEGIN CRISOL\r\n", "no start"),
        (
            "# BEGIN CRISOL\r\n# END CRISOL\r\n# BEGIN CRISOL\r\n# END CRISOL\r\n",
            "second block",
        ),
        (
            "# BEGIN CRISOL\r\n# BEGIN CRISOL\r\n# END CRISOL\r\n",
            "second block",
        ),
    ];
    for (content, why) in cases {
        let error = apply_block(content, &["a.test"]).unwrap_err();
        assert!(
            matches!(error, HostsError::DamagedBlock { .. }),
            "{why}: {error}"
        );
        assert!(remove_block(content).is_err(), "{why}");
        assert!(block_names(content).is_err(), "{why}");
    }
    let error = apply_block("x\n# BEGIN CRISOL\n", &["a.test"]).unwrap_err();
    assert!(error.to_string().contains("line 2"), "{error}");
}

#[test]
fn only_plain_names_under_special_use_suffixes_get_in() {
    for good in ["shop.test", "a-b.shop.test", "x.localhost", "SHOP.TEST"] {
        assert!(apply_block("", &[good]).is_ok(), "{good}");
    }
    for bad in [
        "example.com",
        "shop.dev",
        "shop.test\r\n127.0.0.1 evil.com",
        "shop test",
        "-a.test",
        "a..test",
        "*.test",
        "",
        "tést.test",
        "a.test # x",
    ] {
        assert!(
            matches!(apply_block("", &[bad]), Err(HostsError::InvalidName(_))),
            "{bad:?}"
        );
    }
}

#[test]
fn names_that_another_tool_defines_are_reported_and_left_in_place() {
    let update = apply_block(HERD, &["old.test", "new.test"]).unwrap();
    assert_eq!(
        update.conflicts,
        vec![Conflict {
            line: 5,
            name: "old.test".to_owned(),
            text: "127.0.0.1 old.test # herd".to_owned()
        }]
    );
    assert!(update.content.starts_with(HERD), "their line stays");
    // Our own block does not conflict with itself on the next run.
    let again = apply_block(&update.content, &["old.test", "new.test"]).unwrap();
    assert_eq!(again.conflicts.len(), 1);
    assert_eq!(again.content, update.content);
}

#[test]
fn comments_and_odd_lines_are_not_entries() {
    let content = "# 127.0.0.1 a.test\r\n127.0.0.1\r\nnot-an-ip a.test\r\n";
    assert_eq!(
        apply_block(content, &["a.test"]).unwrap().conflicts,
        Vec::new()
    );
}

#[test]
fn the_manager_writes_only_when_something_changes() {
    let file = MemoryHosts::new(HERD);
    let hosts = DirectHosts::new(&file);
    assert_eq!(hosts.names().unwrap(), Vec::<String>::new());
    hosts.clear().unwrap();
    assert_eq!(file.writes(), 0, "no block, nothing to clear");

    let conflicts = hosts.set(&names(&["shop.test"])).unwrap();
    assert_eq!(conflicts, Vec::new());
    assert_eq!(file.writes(), 1);
    hosts.set(&names(&["shop.test"])).unwrap();
    assert_eq!(file.writes(), 1, "same names: no write");
    assert_eq!(hosts.names().unwrap(), names(&["shop.test"]));

    hosts.clear().unwrap();
    assert_eq!(file.content(), HERD, "the file is back as it was");
    assert_eq!(file.writes(), 2);
}

#[test]
fn a_damaged_file_stays_untouched_through_the_manager() {
    let damaged = "x\r\n# BEGIN CRISOL\r\n";
    let file = MemoryHosts::new(damaged);
    let hosts = DirectHosts::new(&file);
    assert!(hosts.set(&names(&["a.test"])).is_err());
    assert!(hosts.clear().is_err());
    assert_eq!(file.content(), damaged);
    assert_eq!(file.writes(), 0);
}

#[test]
fn error_messages_have_no_double_spaces() {
    for error in [
        HostsError::DamagedBlock {
            line: 3,
            problem: "the block never ends",
        },
        HostsError::InvalidName("x".to_owned()),
    ] {
        assert!(!error.to_string().contains("  "), "{error}");
    }
}

#[test]
fn the_names_of_the_suffix_outside_the_block_are_found_and_the_block_is_not() {
    let content = format!(
        "{HERD}10.0.0.5 API.Shop.TEST shop.example\r\n127.0.0.1 testing.com\r\n\
         # BEGIN CRISOL\r\n127.0.0.1 mine.test\r\n# END CRISOL\r\n"
    );
    let found = foreign_names(&content, "test").unwrap();
    let found: Vec<(usize, &str)> = found.iter().map(|c| (c.line, c.name.as_str())).collect();
    assert_eq!(found, [(5, "old.test"), (7, "api.shop.test")]);
    assert_eq!(foreign_names("127.0.0.1 localhost\n", "test").unwrap(), []);
    assert!(foreign_names("# BEGIN CRISOL\n", "test").is_err());
}
