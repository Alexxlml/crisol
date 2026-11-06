//! How an answer looks on a terminal. By default plain text, no colors, no cleverness: it is
//! read by people and pasted into issues, and by programs. When the output goes to a terminal
//! ([`Style`], ADR-0028) the checks and lists come in boxed tables and the messages carry a
//! coloured mark; the plain text is still what a redirect or `--plain` gets.

mod table;

use std::fmt::Write as _;

use crisol_protocol::{
    Check, CheckStatus, Event, Reply, RpcError, ServiceInfo, SiteInfo, VersionInfo, VersionList,
};

use table::{draw, paint, Cell, Tone};

/// Whether and how to dress the output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    /// Boxed tables, marks and colour. Off: the plain text.
    pub formatted: bool,
    /// The width of the window in characters (only read when `formatted`).
    pub width: usize,
}

impl Style {
    /// The plain text, as it has always been.
    #[must_use]
    pub const fn plain() -> Self {
        Self {
            formatted: false,
            width: 80,
        }
    }

    /// The formatted output for a window `width` characters wide.
    #[must_use]
    pub const fn formatted(width: usize) -> Self {
        Self {
            formatted: true,
            width,
        }
    }

    /// Decides from the facts about an output that is a terminal already: formatting needs
    /// that it takes escape codes (`ansi_ok`), no `NO_COLOR` (the caller counts only a
    /// value that is not empty, as the convention says), no `TERM=dumb` and no `--plain`.
    #[must_use]
    pub fn detect(
        ansi_ok: bool,
        no_color: bool,
        term: Option<&str>,
        plain_flag: bool,
        width: usize,
    ) -> Self {
        if ansi_ok && !no_color && term != Some("dumb") && !plain_flag {
            Self::formatted(width)
        } else {
            Self::plain()
        }
    }
}

/// The text of a reply. Ends with a newline, or is empty.
#[must_use]
pub fn reply(reply: &Reply) -> String {
    plain_reply(reply)
}

// One flat arm per kind of answer reads better than a function for each.
#[allow(clippy::too_many_lines)]
fn plain_reply(reply: &Reply) -> String {
    let mut out = String::new();
    match reply {
        Reply::Hello(info) => {
            let _ = writeln!(
                out,
                "crisold {} (protocol {}) in {}",
                info.daemon, info.protocol, info.home
            );
        }
        Reply::Done(info) => {
            let _ = writeln!(out, "{}", info.message);
            warnings(&mut out, &info.warnings);
        }
        Reply::Status(info) => {
            if info.services.is_empty() {
                out.push_str("No services yet. Install nginx with `crisol install nginx`.\n");
            }
            let width = info
                .services
                .iter()
                .map(|s| s.name.len())
                .max()
                .unwrap_or(0);
            for service in &info.services {
                let _ = write!(out, "{:<width$}  {}", service.name, service.state);
                if let Some(detail) = &service.detail {
                    let _ = write!(out, "  ({detail})");
                }
                out.push('\n');
            }
            warnings(&mut out, &info.warnings);
        }
        Reply::Installed(info) => {
            if info.already {
                let _ = writeln!(
                    out,
                    "{} {} was already installed ({})",
                    info.service, info.version, info.path
                );
            } else if info.unverified {
                let _ = writeln!(
                    out,
                    "Installed {} {} in {} ({} copied from the file)",
                    info.service,
                    info.version,
                    info.path,
                    bytes(info.downloaded)
                );
            } else {
                let _ = writeln!(
                    out,
                    "Installed {} {} in {} ({} downloaded)",
                    info.service,
                    info.version,
                    info.path,
                    bytes(info.downloaded)
                );
            }
            if info.unverified {
                let _ = writeln!(
                    out,
                    "warning: {} {} was not verified by Crisol: it was installed with the hash you gave, not one from the catalog",
                    info.service, info.version
                );
            }
            if info.end_of_life {
                let _ = writeln!(
                    out,
                    "warning: {} {} no longer receives security fixes; use it only for an old project",
                    info.service, info.version
                );
            }
            for advisory in &info.advisories {
                let remedy = match &advisory.fixed_in {
                    Some(fixed) => format!(
                        "fixed in {} {fixed}: `crisol install {} {fixed}`",
                        info.service, info.service
                    ),
                    None => "there is no fixed build".to_owned(),
                };
                let _ = writeln!(
                    out,
                    "warning: {} {} has a known vulnerability, {}: {} ({remedy}; {})",
                    info.service, info.version, advisory.id, advisory.summary, advisory.url
                );
            }
        }
        Reply::Removed(info) => {
            let _ = writeln!(out, "Removed {} {}", info.service, info.version);
        }
        Reply::Versions(list) => {
            out.push_str("Installed:\n");
            if list.installed.is_empty() {
                out.push_str("  (nothing yet)\n");
            }
            for v in &list.installed {
                let _ = writeln!(out, "  {} {}{}", v.service, v.version, flags(v));
            }
            if !list.available.is_empty() {
                out.push_str("Available:\n");
                for v in &list.available {
                    let _ = writeln!(
                        out,
                        "  {} {}{}{}",
                        v.service,
                        v.version,
                        if v.installed { "  [installed]" } else { "" },
                        flags(v)
                    );
                }
            }
        }
        Reply::Used(info) => {
            let _ = writeln!(
                out,
                "{} now selects {} (written to {}){}",
                info.request,
                info.selects,
                info.file,
                if info.changed {
                    ""
                } else {
                    "; it already said so"
                }
            );
            warnings(&mut out, &info.warnings);
        }
        Reply::Sites { sites, warnings: w } => {
            if sites.is_empty() {
                out.push_str("No sites. `crisol park` serves every folder in a folder; `crisol link` serves one.\n");
            }
            let width = sites.iter().map(|s| s.domain.len()).max().unwrap_or(0);
            for site in sites {
                let scheme = if site.secure { "https" } else { "http " };
                let _ = writeln!(
                    out,
                    "{scheme}  {:<width$}  {}  [{}]",
                    site.domain, site.docroot, site.kind
                );
            }
            warnings(&mut out, w);
        }
        Reply::Logs(chunk) => {
            for line in &chunk.lines {
                out.push_str(line);
                out.push('\n');
            }
        }
        Reply::Credentials(info) => {
            for line in &info.lines {
                let _ = writeln!(out, "{}={}", line.key, line.value);
            }
            for note in &info.notes {
                let _ = writeln!(out, "# {note}");
            }
        }
        Reply::Doctor { checks } => {
            for check in checks {
                let mark = match check.status {
                    CheckStatus::Ok => "ok     ",
                    CheckStatus::Warning => "warning",
                    CheckStatus::Problem => "PROBLEM",
                };
                let _ = writeln!(out, "{mark}  {}: {}", check.name, check.detail);
                if let Some(fix) = &check.fix {
                    let _ = writeln!(out, "         fix: {fix}");
                }
            }
        }
    }
    out
}

fn flag_list(v: &VersionInfo) -> Vec<String> {
    let mut flags = Vec::new();
    if v.recommended {
        flags.push("recommended".to_owned());
    }
    if v.end_of_life {
        flags.push("end of life".to_owned());
    }
    if v.unverified {
        flags.push("not verified by Crisol".to_owned());
    }
    if !v.advisories.is_empty() {
        flags.push(format!("vulnerable: {}", v.advisories.join(", ")));
    }
    flags
}

fn flags(v: &VersionInfo) -> String {
    flag_list(v).iter().fold(String::new(), |mut text, flag| {
        let _ = write!(text, "  ({flag})");
        text
    })
}

fn warnings(out: &mut String, list: &[String]) {
    for warning in list {
        let _ = writeln!(out, "warning: {warning}");
    }
}

/// `1.5 MB`, as people read sizes.
#[must_use]
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    #[allow(clippy::cast_precision_loss)]
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// An error as the terminal shows it: the sentence, then the command that fixes it.
#[must_use]
pub fn error(error: &RpcError) -> String {
    let mut out = format!("error: {}\n", error.message);
    if let Some(fix) = &error.fix {
        let _ = writeln!(out, "  fix: {fix}");
    }
    out
}

/// A progress line for a download, or `None` when it is not worth a line (an event of another
/// kind, or no news since the last percentage shown).
#[must_use]
pub fn progress(event: &Event, last_percent: &mut Option<u64>) -> Option<String> {
    let Event::Progress {
        what,
        done,
        total,
        from_file,
    } = event
    else {
        return None;
    };
    // Say "copying" for `install --from`: nothing comes from the network then (ADR-0020).
    let doing = if *from_file {
        format!("Copying {what} from the file")
    } else {
        format!("Downloading {what}")
    };
    match total {
        Some(total) if *total > 0 => {
            let percent = done * 100 / total;
            // One line per ten percent is enough to see it move.
            if last_percent.is_some_and(|last| percent / 10 <= last / 10) && percent < 100 {
                return None;
            }
            *last_percent = Some(percent);
            Some(format!("{doing}: {percent}% ({})", bytes(*done)))
        }
        _ => {
            if last_percent.is_some() {
                return None;
            }
            *last_percent = Some(0);
            Some(format!("{doing}..."))
        }
    }
}

/// The text of a reply as `style` asks. Plain, it is exactly [`reply`].
#[must_use]
pub fn reply_styled(reply: &Reply, style: &Style) -> String {
    if !style.formatted {
        return plain_reply(reply);
    }
    match reply {
        Reply::Status(info) if !info.services.is_empty() => {
            match status_table(&info.services, style) {
                Some(mut out) => {
                    warnings_marked(&mut out, &info.warnings);
                    out
                }
                None => plain_reply(reply),
            }
        }
        Reply::Sites { sites, warnings } if !sites.is_empty() => match sites_table(sites, style) {
            Some(mut out) => {
                warnings_marked(&mut out, warnings);
                out
            }
            None => plain_reply(reply),
        },
        Reply::Versions(list) => versions_styled(list, style),
        Reply::Doctor { checks } => doctor_styled(checks, style),
        Reply::Done(_) | Reply::Installed(_) | Reply::Removed(_) | Reply::Used(_) => {
            marked(&plain_reply(reply))
        }
        _ => plain_reply(reply),
    }
}

/// Puts a green tick before the first line of a message and a yellow `!` in place of the
/// `warning:` of the others.
fn marked(text: &str) -> String {
    let mut out = String::new();
    for (n, line) in text.lines().enumerate() {
        if let Some(warning) = line.strip_prefix("warning: ") {
            let _ = writeln!(out, "{}", paint(&format!("! {warning}"), Tone::Warn));
        } else if n == 0 {
            let _ = writeln!(out, "{} {line}", paint("✓", Tone::Good));
        } else {
            let _ = writeln!(out, "{line}");
        }
    }
    out
}

/// How a message that the CLI writes by itself (`shims`, `path`, `autostart`) went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Done, or all is in place: a green tick.
    Good,
    /// Done, but something deserves a look: a yellow `!`.
    Warn,
    /// Part of it failed: a red cross.
    Bad,
    /// Only says how things are, neither good nor bad: no mark.
    Info,
}

/// A message of the CLI's own, ending with a newline. Formatted, its first line carries the
/// mark of `outcome`; the lines under it are kept as they are. Plain, it is the text itself.
#[must_use]
pub fn message_styled(text: &str, outcome: Outcome, style: &Style) -> String {
    let mark = match outcome {
        Outcome::Good => Some(("✓", Tone::Good)),
        Outcome::Warn => Some(("!", Tone::Warn)),
        Outcome::Bad => Some(("✗", Tone::Bad)),
        Outcome::Info => None,
    };
    match mark {
        Some((mark, tone)) if style.formatted => format!("{} {text}\n", paint(mark, tone)),
        _ => format!("{text}\n"),
    }
}

fn warnings_marked(out: &mut String, list: &[String]) {
    for warning in list {
        let _ = writeln!(out, "{}", paint(&format!("! {warning}"), Tone::Warn));
    }
}

fn state_tone(state: &str) -> Tone {
    match state {
        "running" => Tone::Good,
        "failed" => Tone::Bad,
        "stopped" => Tone::Dim,
        _ => Tone::Warn,
    }
}

fn status_table(services: &[ServiceInfo], style: &Style) -> Option<String> {
    let rows: Vec<Vec<Cell>> = services
        .iter()
        .map(|s| {
            vec![
                Cell::plain(&s.name),
                Cell::new(&s.state, state_tone(&s.state)),
                Cell::plain(s.detail.clone().unwrap_or_default()),
            ]
        })
        .collect();
    draw(&["Service", "State", "Detail"], &rows, style.width, true)
}

fn sites_table(sites: &[SiteInfo], style: &Style) -> Option<String> {
    let rows: Vec<Vec<Cell>> = sites
        .iter()
        .map(|s| {
            vec![
                if s.secure {
                    Cell::new("https", Tone::Good)
                } else {
                    Cell::plain("http")
                },
                Cell::plain(&s.domain),
                Cell::plain(s.kind.clone()),
                Cell::plain(&s.docroot),
            ]
        })
        .collect();
    draw(
        &["Scheme", "Site", "Kind", "Folder"],
        &rows,
        style.width,
        true,
    )
}

fn version_rows(list: &[VersionInfo], show_installed: bool) -> Vec<Vec<Cell>> {
    list.iter()
        .map(|v| {
            let mut notes = flag_list(v);
            if show_installed && v.installed {
                notes.insert(0, "installed".to_owned());
            }
            let tone = if !v.advisories.is_empty() {
                Tone::Bad
            } else if v.end_of_life || v.unverified {
                Tone::Warn
            } else if v.recommended {
                Tone::Good
            } else {
                Tone::Plain
            };
            vec![
                Cell::plain(&v.service),
                Cell::plain(&v.version),
                Cell::new(notes.join(", "), tone),
            ]
        })
        .collect()
}

fn versions_styled(list: &VersionList, style: &Style) -> String {
    let mut out = String::new();
    let sections = [
        ("Installed", &list.installed, false),
        ("Available", &list.available, true),
    ];
    for (title, versions, show_installed) in sections {
        if title == "Available" && versions.is_empty() {
            continue;
        }
        let _ = writeln!(out, "{}", paint(title, Tone::Bold));
        if versions.is_empty() {
            out.push_str("  (nothing yet)\n");
            continue;
        }
        let rows = version_rows(versions, show_installed);
        if let Some(table) = draw(&["Tool", "Version", "Notes"], &rows, style.width, true) {
            out.push_str(&table);
        } else {
            for v in versions {
                let installed = if show_installed && v.installed {
                    "  [installed]"
                } else {
                    ""
                };
                let _ = writeln!(out, "  {} {}{installed}{}", v.service, v.version, flags(v));
            }
        }
    }
    out
}

fn check_mark(status: CheckStatus) -> (&'static str, Tone) {
    match status {
        CheckStatus::Ok => ("✓ ok", Tone::Good),
        CheckStatus::Warning => ("! warn", Tone::Warn),
        CheckStatus::Problem => ("✗ fail", Tone::Bad),
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn doctor_styled(checks: &[Check], style: &Style) -> String {
    let count = |status| checks.iter().filter(|c| c.status == status).count();
    let rows: Vec<Vec<Cell>> = checks
        .iter()
        .map(|c| {
            let (mark, tone) = check_mark(c.status);
            let mut detail = c.detail.clone();
            if let Some(fix) = &c.fix {
                let _ = write!(detail, "\nfix: {fix}");
            }
            vec![
                Cell::new(mark, tone),
                Cell::plain(&c.name),
                Cell::plain(detail),
            ]
        })
        .collect();
    let mut out =
        draw(&["Status", "Check", "Detail"], &rows, style.width, true).unwrap_or_else(|| {
            let mut list = String::new();
            for check in checks {
                let (mark, tone) = check_mark(check.status);
                let _ = writeln!(
                    list,
                    "{} {}: {}",
                    paint(mark, tone),
                    check.name,
                    check.detail
                );
                if let Some(fix) = &check.fix {
                    let _ = writeln!(list, "    fix: {fix}");
                }
            }
            list
        });
    let _ = writeln!(
        out,
        "{} · {} · {}",
        paint(&format!("{} ok", count(CheckStatus::Ok)), Tone::Good),
        paint(
            &plural(count(CheckStatus::Warning), "warning", "warnings"),
            Tone::Warn
        ),
        paint(
            &plural(count(CheckStatus::Problem), "problem", "problems"),
            Tone::Bad
        ),
    );
    out
}

/// An error as the terminal shows it under `style`.
#[must_use]
pub fn error_styled(error: &RpcError, style: &Style) -> String {
    if !style.formatted {
        return self::error(error);
    }
    let mut out = format!("{}\n", paint(&format!("✗ {}", error.message), Tone::Bad));
    if let Some(fix) = &error.fix {
        let _ = writeln!(out, "  {}", paint(&format!("fix: {fix}"), Tone::Dim));
    }
    out
}

/// What to write over the line of a download that is shown in place: `None` when there is
/// nothing new (same percentage as before, or an event of another kind). It starts with the
/// code that erases the line, so the caller writes it as it is, without a newline. The text is
/// cut to `width` characters: a line that wrapped could not be erased, and each redraw would
/// leave a new line behind.
#[must_use]
pub fn progress_in_place(
    event: &Event,
    last_percent: &mut Option<u64>,
    width: usize,
) -> Option<String> {
    let Event::Progress {
        what,
        done,
        total,
        from_file,
    } = event
    else {
        return None;
    };
    let doing = if *from_file {
        format!("Copying {what} from the file")
    } else {
        format!("Downloading {what}")
    };
    let text = match total {
        Some(total) if *total > 0 => {
            let percent = (done * 100 / total).min(100);
            if *last_percent == Some(percent) {
                return None;
            }
            *last_percent = Some(percent);
            let filled = usize::try_from(percent / 5).unwrap_or(20);
            format!(
                "{doing}  [{}{}] {percent}% ({})",
                "█".repeat(filled),
                "░".repeat(20 - filled),
                bytes(*done)
            )
        }
        _ => {
            if last_percent.is_some() {
                return None;
            }
            *last_percent = Some(0);
            format!("{doing}...")
        }
    };
    let text: String = text.chars().take(width.max(1)).collect();
    Some(format!("\r\x1b[2K{text}"))
}

#[cfg(test)]
mod styled_tests;
#[cfg(test)]
mod tests;

/// What the terminal asks after printing the plan of `uninstall --all`, answered with `yes`.
/// When the home is deleted, it asks about backups first.
#[must_use]
pub fn teardown_question(plan: &crisol_core::teardown::Plan) -> &'static str {
    if plan.deletes_home() {
        "\nHave you backed up your projects and the databases you need? The folder above is deleted and cannot be recovered.\nType yes to go on:"
    } else {
        "\nType yes to go on:"
    }
}

/// The list `uninstall --all` shows before it asks. Ends with a newline.
#[must_use]
pub fn teardown_plan(plan: &crisol_core::teardown::Plan) -> String {
    let mut out = String::new();
    if plan.is_empty() {
        out.push_str("Nothing of Crisol is left to undo in Windows.\n");
    } else {
        out.push_str("Crisol will undo, in this order:\n");
        for (n, step) in plan.steps.iter().enumerate() {
            let _ = writeln!(out, "  {}. {}", n + 1, step.describe());
        }
    }
    if let Some(notice) = &plan.purge {
        if notice.foreign.is_empty() {
            let _ = writeln!(
                out,
                "\n--purge deletes {} and everything in it, which cannot be recovered:",
                notice.home.display()
            );
            if !notice.databases.is_empty() {
                let _ = writeln!(
                    out,
                    "  - the data of the databases: {}",
                    notice.databases.join(", ")
                );
            }
            if !notice.projects.is_empty() {
                let _ = writeln!(
                    out,
                    "  - what is in its www folder, projects included: {}",
                    notice.projects.join(", ")
                );
            }
            out.push_str(
                "  - the installed versions, the settings, the passwords of the services and the certificates\n",
            );
            out.push_str("Sites linked or parked from folders outside it are not touched.\n");
        }
    }
    let sections = [
        ("\nTo do by hand:", &plan.by_hand),
        ("\nCould not be looked at:", &plan.problems),
        ("\nKept:", &plan.kept),
    ];
    for (title, lines) in sections {
        if !lines.is_empty() {
            let _ = writeln!(out, "{title}");
            for line in lines {
                let _ = writeln!(out, "  - {line}");
            }
        }
    }
    out
}

/// What `uninstall --all` did, one line per step. Ends with a newline.
#[must_use]
pub fn teardown_outcomes(outcomes: &[crisol_core::teardown::Outcome]) -> String {
    let mut out = String::new();
    for outcome in outcomes {
        let _ = writeln!(out, "[{}] {}", outcome.kind.word(), outcome.message);
    }
    out
}

/// What `setup` will do, what is in place already and what stops it. Ends with a newline.
#[must_use]
pub fn setup_plan(plan: &crisol_core::onboarding::Plan) -> String {
    let mut out = String::new();
    if plan.is_blocked() {
        out.push_str("Crisol cannot be set up yet:\n");
        for problem in &plan.problems {
            let _ = writeln!(out, "  - {}", problem.message);
            if let Some(fix) = &problem.fix {
                let _ = writeln!(out, "    fix: {fix}");
            }
        }
        return out;
    }
    if plan.is_empty() {
        out.push_str("Crisol is set up: there is nothing left to do.\n");
    } else {
        out.push_str("Crisol will set up, in this order:\n");
        for (n, step) in plan.steps.iter().enumerate() {
            let _ = writeln!(
                out,
                "  {}. {}{}",
                n + 1,
                step.describe(),
                if step.optional() { " [asks first]" } else { "" }
            );
        }
    }
    for (title, lines) in [
        ("\nAlready in place:", &plan.done),
        ("\nNote:", &plan.notes),
    ] {
        if !lines.is_empty() {
            let _ = writeln!(out, "{title}");
            for line in lines {
                let _ = writeln!(out, "  - {line}");
            }
        }
    }
    out
}

/// The question before `setup` changes anything.
#[must_use]
pub fn setup_question(plan: &crisol_core::onboarding::Plan) -> &'static str {
    if plan
        .steps
        .iter()
        .any(crisol_core::onboarding::Step::optional)
    {
        "Type yes to go on (the steps marked [asks first] ask again):"
    } else {
        "Type yes to go on:"
    }
}
