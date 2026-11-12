//! `crisol shell`: a terminal where only Crisol decides which `php`, `node` or `mysql` runs
//! (ADR-0019).
//!
//! The terminal is a child of `crisol`, in the same window, with an environment Crisol builds:
//!
//! - the shims and the folders of the database clients go **in front of** the whole `PATH`
//!   (nothing is taken out of it, so Git, VS Code and the rest still work);
//! - the variables other tools leave and that change how PHP, Node or npm behave
//!   ([`is_removed`]) are taken out;
//! - Crisol's own profile runs **after** the user's (so it wins over a profile that puts nvm or
//!   Laragon first), and marks the terminal: `(crisol)` in front of the prompt and the title
//!   «Crisol».
//!
//! Nothing outside that terminal changes: no registry, no `PATH`, no profile of the user's.
//!
//! This module decides everything and touches only `etc/shell/` (the profiles of cmd and Git
//! Bash). Finding the programs of the terminals and starting them is `crisol-platform`.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use base64::Engine as _;

use crate::config::Config;
use crate::dbservice::{plan as db_plan, DbKind};
use crate::i18n::{Localized, Message};
use crate::msg;
use crate::paths::{CrisolPaths, PrivateDirs};
use crate::shims::{shim_file_name, SHIM_COMMANDS};

/// The variable that tells a process it runs inside `crisol shell`.
pub const INSIDE: &str = "CRISOL_SHELL";
/// The variable that tells Crisol's Bash profile to leave the user's out (`--clean`).
const CLEAN: &str = "CRISOL_SHELL_CLEAN";

/// The variables that are taken out, besides every `npm_config_*`: they point PHP at another
/// `php.ini`, Node at other modules or options, or carry the marks of the shims themselves.
const REMOVED: [&str; 6] = [
    "PHPRC",
    "PHP_INI_SCAN_DIR",
    "NODE_PATH",
    "NODE_OPTIONS",
    "CRISOL_PHPRC",
    "CRISOL_NPM_PREFIX",
];

/// The terminals `crisol shell` can open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Terminal {
    /// PowerShell 7 (`pwsh.exe`).
    Pwsh,
    /// Windows PowerShell 5.1 (`powershell.exe`), the one Windows brings.
    WindowsPowerShell,
    /// `cmd.exe`.
    Cmd,
    /// Git Bash (`bash.exe` of Git for Windows).
    Bash,
}

impl Terminal {
    /// The order tried when the program that started `crisol` is not a terminal.
    pub const FALLBACK: [Self; 3] = [Self::Pwsh, Self::WindowsPowerShell, Self::Cmd];

    /// The terminal a program is, from its file name or path (`pwsh.exe`, `C:\…\bash.exe`).
    #[must_use]
    pub fn from_program(program: &str) -> Option<Self> {
        let name = Path::new(program)
            .file_name()?
            .to_str()?
            .to_ascii_lowercase();
        match name.strip_suffix(".exe").unwrap_or(&name) {
            "pwsh" => Some(Self::Pwsh),
            "powershell" => Some(Self::WindowsPowerShell),
            "cmd" => Some(Self::Cmd),
            "bash" => Some(Self::Bash),
            _ => None,
        }
    }

    /// Every terminal, in the order a list shows them.
    pub const ALL: [Self; 4] = [Self::Pwsh, Self::WindowsPowerShell, Self::Cmd, Self::Bash];

    /// The terminal a name is, as [`name`](Self::name) writes it (`[shell] terminal`).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.name() == name)
    }

    /// How it is called in messages and in `--shell`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Pwsh => "pwsh",
            Self::WindowsPowerShell => "powershell",
            Self::Cmd => "cmd",
            Self::Bash => "bash",
        }
    }
}

/// Why `crisol shell` cannot open a terminal.
#[derive(Debug, thiserror::Error)]
pub enum ShellError {
    /// This is already a `crisol shell`.
    AlreadyInside,
    /// The shims are not in place, so the terminal would have nothing of Crisol's.
    NoShims,
    /// The terminal that was asked for is not installed.
    NotFound(&'static str),
    /// None of the terminals was found.
    NoTerminal,
    /// A folder has a character a `PATH` or a profile cannot carry.
    Unusable(PathBuf),
    /// The profile could not be written.
    Io(#[from] io::Error),
}

impl Localized for ShellError {
    fn message(&self) -> Message {
        match self {
            Self::AlreadyInside => msg!("shell.already_inside"),
            Self::NoShims => msg!("shell.no_shims", command = "crisol shims install"),
            Self::NotFound(name) => msg!("shell.not_found", name = name),
            Self::NoTerminal => msg!("shell.no_terminal"),
            Self::Unusable(path) => msg!("shell.unusable", path = path.display()),
            Self::Io(error) => msg!("shell.io", error = error),
        }
    }
}

crate::display_as_message!(ShellError);

/// What PowerShell reads as a single quote: `'` and the typographic ones (U+2018 to U+201B).
const POWERSHELL_QUOTES: [char; 5] = ['\'', '\u{2018}', '\u{2019}', '\u{201A}', '\u{201B}'];

/// `text` as one PowerShell string in single quotes. Every quote PowerShell accepts is doubled,
/// so nothing in it ends the string: `O’Brien` (with the apostrophe of autocorrect) would,
/// with only `'` doubled (review of phase 11, finding 3).
#[must_use]
pub fn powershell_quote(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('\'');
    for c in text.chars() {
        if POWERSHELL_QUOTES.contains(&c) {
            quoted.push(c);
        }
        quoted.push(c);
    }
    quoted.push('\'');
    quoted
}

/// Whether the variable `name` is taken out of a `crisol shell`.
#[must_use]
pub fn is_removed(name: &str) -> bool {
    REMOVED.iter().any(|r| r.eq_ignore_ascii_case(name))
        || name
            .get(..11)
            .is_some_and(|start| start.eq_ignore_ascii_case("npm_config_"))
}

/// The folders of the clients of the databases installed with Crisol (`mysql`, `psql`,
/// `redis-cli`…), the version the configuration would run, the enabled services first (MySQL
/// and MariaDB both have a `mysql.exe`: the one in use wins).
#[must_use]
pub fn client_folders(paths: &CrisolPaths, config: &Config) -> Vec<PathBuf> {
    let mut kinds: Vec<DbKind> = DbKind::ALL
        .into_iter()
        .filter(|k| k.client_folder().is_some())
        .collect();
    kinds.sort_by_key(|k| !k.enabled(config));
    kinds
        .into_iter()
        .filter_map(|kind| {
            let plan = db_plan(paths, config, kind).ok()?;
            let folder = match kind.client_folder()? {
                "" => plan.version.path,
                sub => plan.version.path.join(sub),
            };
            folder.is_dir().then_some(folder)
        })
        .collect()
}

/// What goes in front of the `PATH`: the shims, then the database clients.
#[must_use]
pub fn front_folders(paths: &CrisolPaths, config: &Config) -> Vec<PathBuf> {
    let mut front = vec![paths.shims_dir()];
    front.extend(client_folders(paths, config));
    front
}

/// Whether the shims are in place (all of them).
#[must_use]
pub fn shims_installed(paths: &CrisolPaths) -> bool {
    let dir = paths.shims_dir();
    SHIM_COMMANDS
        .iter()
        .all(|c| dir.join(shim_file_name(c)).is_file())
}

/// What `[shell] terminal` says ([`crate::config::Config::terminal_preference`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preference<'a> {
    /// No key: the automatic choice.
    Automatic,
    /// A terminal Crisol knows.
    Terminal(Terminal),
    /// A name Crisol does not know (a typo, or a terminal of a newer version). The automatic
    /// choice is used and the mistake is said; the file still loads.
    Unknown(&'a str),
}

impl From<Option<Terminal>> for Preference<'_> {
    fn from(terminal: Option<Terminal>) -> Self {
        terminal.map_or(Self::Automatic, Self::Terminal)
    }
}

/// Why a name in `[shell] terminal` is not used.
#[must_use]
pub fn unknown_terminal(name: &str) -> Message {
    msg!(
        "shell.unknown_terminal",
        setting = format!("shell.terminal = \"{name}\""),
        terminals = "pwsh, powershell, cmd, bash"
    )
}

/// The terminal to open: the one asked for, else the one `crisol` was started from (`parent`),
/// else the one the user chose in `[shell] terminal` (`preferred`), else the first of
/// [`Terminal::FALLBACK`] that is installed. `find` says where a terminal's program is.
///
/// The terminal it was typed in wins over the chosen one: `crisol shell` typed in Git Bash
/// gives a Git Bash, not another shell inside it. The choice is for a program that was not
/// started from a terminal, such as the GUI (which passes no `parent`).
///
/// The note, if any, says why another one was opened. A `preferred` terminal that is no longer
/// installed is not an error: it is said, and the automatic choice goes on (nothing is
/// rewritten). Neither is a name Crisol does not know ([`Preference::Unknown`], a typo in
/// `crisol.toml`): it is said whenever the choice is not `--shell`, also when the terminal it
/// was typed in wins, because the file has a mistake to fix.
pub fn choose_terminal<'a>(
    asked: Option<Terminal>,
    preferred: impl Into<Preference<'a>>,
    parent: Option<&str>,
    find: &dyn Fn(Terminal) -> Option<PathBuf>,
) -> Result<(Terminal, PathBuf, Option<Message>), ShellError> {
    let preferred = preferred.into();
    if let Some(terminal) = asked {
        let program = find(terminal).ok_or(ShellError::NotFound(terminal.name()))?;
        return Ok((terminal, program, None));
    }
    let typo = match preferred {
        Preference::Unknown(name) => Some(unknown_terminal(name)),
        Preference::Automatic | Preference::Terminal(_) => None,
    };
    if let Some(terminal) = parent.and_then(Terminal::from_program) {
        let program = find(terminal).ok_or(ShellError::NotFound(terminal.name()))?;
        let note =
            typo.map(|typo| msg!("shell.opening_after", lost => typo, name = terminal.name()));
        return Ok((terminal, program, note));
    }
    let mut lost = typo;
    if let Preference::Terminal(terminal) = preferred {
        match find(terminal) {
            Some(program) => return Ok((terminal, program, None)),
            None => {
                lost = Some(msg!("shell.lost_terminal", name = terminal.name()));
            }
        }
    }
    let (terminal, program) = Terminal::FALLBACK
        .into_iter()
        .find_map(|t| find(t).map(|p| (t, p)))
        .ok_or(ShellError::NoTerminal)?;
    let note = match lost {
        Some(lost) => msg!("shell.opening_after", lost => lost, name = terminal.name()),
        None => msg!("shell.opening_unknown_parent", name = terminal.name()),
    };
    Ok((terminal, program, Some(note)))
}

/// Writes the terminal the user chose to `[shell] terminal` of `crisol.toml`, or takes the key
/// out for `None` (automatic). Only a person's explicit choice calls this; it goes through
/// `toml_edit`, so comments and order stay. Whether the file changed is returned.
pub fn save_terminal_preference(
    paths: &CrisolPaths,
    dirs: &dyn PrivateDirs,
    terminal: Option<Terminal>,
) -> Result<bool, crate::edit::EditError> {
    // It can be the first thing written (the window lets the terminal be chosen before setup).
    paths
        .create_private_home(dirs)
        .map_err(|source| crate::edit::EditError::Write {
            path: paths.config_file(),
            source,
        })?;
    let mut editor = crate::edit::TomlEditor::open_config(paths)?;
    match terminal {
        Some(terminal) => editor.set(&["shell", "terminal"], terminal.name())?,
        None => {
            editor.remove(&["shell", "terminal"])?;
        }
    }
    editor.save()
}

/// What the platform starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellLaunch {
    /// The terminal's program.
    pub program: PathBuf,
    /// Its arguments.
    pub args: Vec<OsString>,
    /// Variables to set (`PATH` among them).
    pub set: Vec<(OsString, OsString)>,
    /// Variables to take out.
    pub remove: Vec<OsString>,
    /// The folder the terminal opens in; `None` keeps the one of the process that starts it.
    pub cwd: Option<PathBuf>,
}

/// Everything [`plan_shell`] needs.
#[derive(Debug)]
pub struct ShellInput<'a> {
    /// The Crisol home.
    pub paths: &'a CrisolPaths,
    /// The terminal and its program.
    pub terminal: Terminal,
    /// The program of the terminal.
    pub program: &'a Path,
    /// Without the user's profile.
    pub clean: bool,
    /// The folders that go first ([`front_folders`]).
    pub front: &'a [PathBuf],
    /// The environment of `crisol` now.
    pub env: &'a [(OsString, OsString)],
    /// The folder the terminal opens in (`None`: where the process that starts it is).
    pub cwd: Option<&'a Path>,
}

/// Plans the terminal and writes its profile (cmd and Git Bash read it from `etc/shell/`;
/// PowerShell gets it on its command line, because its default policy runs no script file).
pub fn plan_shell(input: &ShellInput<'_>) -> Result<ShellLaunch, ShellError> {
    let ShellInput {
        paths,
        terminal,
        program,
        clean,
        front,
        env,
        cwd,
    } = *input;
    let var = |name: &str| {
        env.iter()
            .find(|(n, _)| n.to_string_lossy().eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
    };
    if var(INSIDE).is_some() {
        return Err(ShellError::AlreadyInside);
    }
    if !shims_installed(paths) {
        return Err(ShellError::NoShims);
    }
    if let Some(bad) = front
        .iter()
        .find(|f| f.to_string_lossy().contains([';', '%', '"']))
    {
        return Err(ShellError::Unusable(bad.clone()));
    }

    let mut set = vec![
        (
            "PATH".into(),
            path_with_front(front, &var("PATH").unwrap_or_default()),
        ),
        (INSIDE.into(), "1".into()),
    ];
    let mut remove: Vec<OsString> = env
        .iter()
        .map(|(n, _)| n.clone())
        .filter(|n| is_removed(&n.to_string_lossy()))
        .collect();
    let profile = profile_text(terminal, front);
    let args: Vec<OsString> = match terminal {
        Terminal::Pwsh | Terminal::WindowsPowerShell => {
            if terminal == Terminal::WindowsPowerShell {
                // A PowerShell 7 that started this one left its own module folders, which
                // Windows PowerShell cannot load.
                if let Some(modules) = var("PSModulePath") {
                    match windows_powershell_modules(&modules.to_string_lossy()) {
                        Some(kept) => set.push(("PSModulePath".into(), kept.into())),
                        None => remove.push("PSModulePath".into()),
                    }
                }
            }
            let mut args = vec!["-NoLogo"];
            if clean {
                args.push("-NoProfile");
            }
            args.extend(["-NoExit", "-EncodedCommand"]);
            let mut args: Vec<OsString> = args.into_iter().map(OsString::from).collect();
            args.push(encoded_command(&profile).into());
            args
        }
        Terminal::Cmd => {
            let file = write_profile(paths, "crisol.cmd", &profile)?;
            let mut args: Vec<OsString> = Vec::new();
            if clean {
                args.push("/d".into());
            }
            args.extend(["/k".into(), file.into_os_string()]);
            args
        }
        Terminal::Bash => {
            let file = write_profile(paths, "crisol.bashrc", &profile)?;
            // Git's `/etc/profile` goes to the home folder unless told to stay.
            set.push(("CHERE_INVOKING".into(), "1".into()));
            if clean {
                set.push((CLEAN.into(), "1".into()));
            }
            vec![
                "--rcfile".into(),
                file.to_string_lossy().replace('\\', "/").into(),
                "-i".into(),
            ]
        }
    };
    Ok(ShellLaunch {
        program: program.to_path_buf(),
        args,
        set,
        remove,
        cwd: cwd.map(Path::to_path_buf),
    })
}

/// `front` followed by `current` without the entries that are already in `front`.
fn path_with_front(front: &[PathBuf], current: &OsStr) -> OsString {
    let normal = |p: &Path| {
        p.to_string_lossy()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_lowercase()
    };
    let ahead: Vec<String> = front.iter().map(|f| normal(f)).collect();
    let rest = std::env::split_paths(current).filter(|p| !ahead.contains(&normal(p)));
    // The front folders were checked for `;` and `"`; an entry of the rest that `join_paths`
    // refuses came from the same `PATH`, so this does not fail in practice.
    std::env::join_paths(front.iter().cloned().chain(rest)).unwrap_or_else(|_| current.into())
}

/// The module folders Windows PowerShell can use: without those of PowerShell 7
/// (`…\PowerShell\…`, not `…\WindowsPowerShell\…`). `None` if none is left.
fn windows_powershell_modules(value: &str) -> Option<String> {
    let kept: Vec<&str> = value
        .split(';')
        .filter(|entry| {
            let lower = entry.to_lowercase();
            !entry.is_empty()
                && (lower.contains("\\windowspowershell\\") || !lower.contains("\\powershell\\"))
        })
        .collect();
    (!kept.is_empty()).then(|| kept.join(";"))
}

fn write_profile(paths: &CrisolPaths, name: &str, text: &str) -> io::Result<PathBuf> {
    let dir = paths.etc_dir().join("shell");
    fs::create_dir_all(&dir)?;
    let file = dir.join(name);
    if fs::read_to_string(&file).ok().as_deref() != Some(text) {
        fs::write(&file, text)?;
    }
    Ok(file)
}

/// `script` the way `-EncodedCommand` takes it: UTF-16LE in Base64.
fn encoded_command(script: &str) -> String {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// `D:\a\b` as `/d/a/b`, the way Git Bash writes it.
fn posix(dir: &Path) -> String {
    let text = dir.to_string_lossy().replace('\\', "/");
    match text.as_bytes() {
        [drive, b':', b'/', ..] if drive.is_ascii_alphabetic() => {
            format!("/{}{}", drive.to_ascii_lowercase() as char, &text[2..])
        }
        _ => text,
    }
}

/// The clients of the databases whose names are taken back like the shims'.
const CLIENTS: [&str; 9] = [
    "mysql",
    "mysqldump",
    "mysqladmin",
    "mariadb",
    "mariadb-dump",
    "psql",
    "pg_dump",
    "pg_restore",
    "redis-cli",
];

/// The commands Crisol provides in a `crisol shell`. An alias, function or `doskey` macro of
/// the user's profile with one of these names would win over every folder of the `PATH` (Herd
/// writes `alias php="php.bat"` into `~/.bash_profile`), so Crisol's profile removes them.
#[must_use]
pub fn taken_back() -> Vec<&'static str> {
    SHIM_COMMANDS
        .iter()
        .chain(CLIENTS.iter())
        .copied()
        .collect()
}

/// Crisol's profile for `terminal`: it runs after the user's and puts Crisol first again.
#[must_use]
pub fn profile_text(terminal: Terminal, front: &[PathBuf]) -> String {
    let removed = REMOVED.join(" ");
    let names = taken_back().join(" ");
    match terminal {
        Terminal::Pwsh | Terminal::WindowsPowerShell => {
            let list = front
                .iter()
                .map(|f| powershell_quote(&f.to_string_lossy()))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "# crisol shell (ADR-0019): runs after your profile.\n\
                 $__crisol_front = @({list})\n\
                 $env:Path = (@($__crisol_front) + @($env:Path -split ';' | Where-Object {{ $_ -and $__crisol_front -notcontains $_.TrimEnd('\\') }})) -join ';'\n\
                 Remove-Variable __crisol_front\n\
                 foreach ($__crisol_name in '{removed}' -split ' ') {{ Remove-Item -LiteralPath \"Env:$__crisol_name\" -ErrorAction SilentlyContinue }}\n\
                 Get-ChildItem Env: | Where-Object {{ $_.Name -like 'npm_config_*' }} | ForEach-Object {{ Remove-Item -LiteralPath \"Env:$($_.Name)\" }}\n\
                 foreach ($__crisol_name in '{names}' -split ' ') {{ Remove-Item -LiteralPath \"Alias:$__crisol_name\", \"Function:$__crisol_name\" -Force -ErrorAction SilentlyContinue }}\n\
                 Remove-Variable __crisol_name -ErrorAction SilentlyContinue\n\
                 $Host.UI.RawUI.WindowTitle = 'Crisol'\n\
                 Set-Item -Path Function:\\global:__crisol_prompt -Value ${{function:prompt}}\n\
                 function global:prompt {{ '(crisol) ' + (& ${{function:__crisol_prompt}}) }}\n"
            )
        }
        Terminal::Cmd => {
            let list = front
                .iter()
                .map(|f| f.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(";");
            let unset: String = REMOVED
                .iter()
                .flat_map(|name| ["@set \"", name, "=\"\r\n"])
                .collect();
            // `doskey name=` with nothing after the `=` deletes a macro.
            let macros: String = taken_back()
                .into_iter()
                .flat_map(|name| ["@doskey ", name, "=\r\n"])
                .collect();
            format!(
                "@rem crisol shell (ADR-0019): runs after AutoRun. Crisol writes this file again each time.\r\n\
                 @set \"PATH={list};%PATH%\"\r\n\
                 {unset}\
                 {macros}\
                 @for /f \"delims==\" %%v in ('set npm_config_ 2^>nul') do @set \"%%v=\"\r\n\
                 @if not defined PROMPT set \"PROMPT=$P$G\"\r\n\
                 @set \"PROMPT=(crisol) %PROMPT%\"\r\n\
                 @title Crisol\r\n"
            )
        }
        Terminal::Bash => {
            let list = front
                .iter()
                .map(|f| format!("'{}'", posix(f).replace('\'', "'\\''")))
                .collect::<Vec<_>>()
                .join(":");
            format!(
                "# crisol shell (ADR-0019). Crisol writes this file again each time.\n\
                 [ -f /etc/profile ] && . /etc/profile\n\
                 if [ -z \"${CLEAN}\" ]; then\n\
                 \x20 if [ -f ~/.bash_profile ]; then . ~/.bash_profile; elif [ -f ~/.bashrc ]; then . ~/.bashrc; fi\n\
                 fi\n\
                 unset {CLEAN} {removed}\n\
                 for __crisol_v in $(compgen -e); do case \"${{__crisol_v,,}}\" in npm_config_*) unset \"$__crisol_v\" ;; esac; done\n\
                 for __crisol_v in {names}; do unalias \"$__crisol_v\" 2>/dev/null; unset -f \"$__crisol_v\" 2>/dev/null; done\n\
                 unset __crisol_v\n\
                 export PATH={list}:\"$PATH\"\n\
                 TITLEPREFIX=Crisol\n\
                 # Git's prompt takes two lines: the mark goes on the one with the `$`.\n\
                 case \"$PS1\" in *'$ ') PS1=\"${{PS1%'$ '}}(crisol) \\$ \" ;; *) PS1=\"(crisol) $PS1\" ;; esac\n\
                 printf '\\033]0;Crisol\\007'\n"
            )
        }
    }
}

#[cfg(test)]
mod tests;
