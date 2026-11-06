use crisol_protocol::EnvLine;

use super::*;

fn info(service: &str, lines: &[(&str, &str)]) -> CredentialsInfo {
    CredentialsInfo {
        service: service.to_owned(),
        lines: lines
            .iter()
            .map(|(key, value)| EnvLine {
                key: (*key).to_owned(),
                value: (*value).to_owned(),
            })
            .collect(),
        notes: vec!["a note".to_owned()],
    }
}

#[test]
fn every_data_service_is_listed_mysql_first() {
    let services = credential_services();
    assert_eq!(services.len(), 9);
    assert_eq!(services[0], "mysql");
    assert!(services.contains(&"garnet") && services.contains(&"memcached"));
}

#[test]
fn passwords_and_keys_are_secret_and_the_rest_is_not() {
    for key in [
        "DB_PASSWORD",
        "REDIS_PASSWORD",
        "MEILISEARCH_KEY",
        "APP_SECRET",
        "api_token",
    ] {
        assert!(is_secret(key), "{key}");
    }
    for key in [
        "DB_HOST",
        "DB_PORT",
        "DB_USERNAME",
        "DB_DATABASE",
        "MAIL_HOST",
    ] {
        assert!(!is_secret(key), "{key}");
    }
}

#[test]
fn the_view_marks_the_secrets_and_makes_the_env_text() {
    let view = credentials_view(
        info(
            "mysql",
            &[("DB_HOST", "127.0.0.1"), ("DB_PASSWORD", "s3cret")],
        ),
        Some("running"),
        true,
    );
    assert_eq!(
        view.lines.iter().map(|l| l.secret).collect::<Vec<_>>(),
        [false, true]
    );
    assert_eq!(view.env, "DB_HOST=127.0.0.1\nDB_PASSWORD=s3cret\n");
    assert_eq!(view.notes, ["a note"]);
    assert_eq!(view.reset, ResetOffer::Available);
}

#[test]
fn a_service_without_a_password_offers_no_reset() {
    for kind in [DbKind::Mailpit, DbKind::Memcached] {
        assert_eq!(
            reset_offer(kind, Some("running"), true),
            ResetOffer::NoPassword,
            "{kind:?}"
        );
    }
}

#[test]
fn without_a_daemon_nothing_can_be_reset() {
    assert_eq!(
        reset_offer(DbKind::Mysql, Some("running"), false),
        ResetOffer::NeedsDaemon
    );
}

#[test]
fn garnet_and_meilisearch_have_to_be_stopped_first() {
    for kind in [DbKind::Garnet, DbKind::Meilisearch] {
        assert_eq!(
            reset_offer(kind, Some("running"), true),
            ResetOffer::StopFirst,
            "{kind:?}"
        );
        // Stopped, the new password waits in `secrets/` for the next start.
        assert_eq!(
            reset_offer(kind, Some("stopped"), true),
            ResetOffer::Available,
            "{kind:?}"
        );
        assert_eq!(reset_offer(kind, None, true), ResetOffer::Available);
    }
}

#[test]
fn redis_is_asked_while_it_runs_and_changed_in_the_file_when_it_does_not() {
    assert_eq!(
        reset_offer(DbKind::Redis, Some("running"), true),
        ResetOffer::Available
    );
    assert_eq!(
        reset_offer(DbKind::Redis, Some("stopped"), true),
        ResetOffer::Available
    );
}

#[test]
fn the_databases_that_keep_the_password_in_their_data_have_to_run() {
    for kind in [
        DbKind::Mysql,
        DbKind::Mariadb,
        DbKind::Postgres,
        DbKind::Mongodb,
    ] {
        assert_eq!(
            reset_offer(kind, Some("running"), true),
            ResetOffer::Available,
            "{kind:?}"
        );
        for state in ["stopped", "failed"] {
            assert_eq!(
                reset_offer(kind, Some(state), true),
                ResetOffer::StartFirst,
                "{kind:?} {state}"
            );
        }
        assert_eq!(reset_offer(kind, None, true), ResetOffer::StartFirst);
    }
}

#[test]
fn while_a_service_changes_state_it_is_busy() {
    for state in ["starting", "stopping"] {
        assert_eq!(
            reset_offer(DbKind::Mysql, Some(state), true),
            ResetOffer::Busy,
            "{state}"
        );
    }
}
