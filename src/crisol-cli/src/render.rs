//! How an answer looks on a terminal. Plain text, no colors, no cleverness: it is read by
//! people and pasted into issues.

use std::fmt::Write as _;

use crisol_protocol::{CheckStatus, Event, Reply, RpcError, VersionInfo};

/// The text of a reply. Ends with a newline, or is empty.
// One flat arm per kind of answer reads better than a function for each.
#[allow(clippy::too_many_lines)]
#[must_use]
pub fn reply(reply: &Reply) -> String {
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

fn flags(v: &VersionInfo) -> String {
    let mut text = String::new();
    if v.recommended {
        text.push_str("  (recommended)");
    }
    if v.end_of_life {
        text.push_str("  (end of life)");
    }
    if v.unverified {
        text.push_str("  (not verified by Crisol)");
    }
    if !v.advisories.is_empty() {
        let _ = write!(text, "  (vulnerable: {})", v.advisories.join(", "));
    }
    text
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
