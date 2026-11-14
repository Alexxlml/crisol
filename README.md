# Crisol

[![License: MIT or Apache-2.0](https://img.shields.io/badge/license-MIT%20%7C%20Apache--2.0-blue)](#license)
![Platform: Windows](https://img.shields.io/badge/platform-Windows%2010%20%7C%2011-0078D4)
![Built with Rust](https://img.shields.io/badge/built%20with-Rust-CE422B)

**English** | [Español](README.es.md)

Local web development suite for PHP and Node, written in Rust.

Crisol installs and manages the tools of a local development environment —PHP, Nginx, Node, databases and supporting services— so that **everything works out of the box, with no paid features and without cluttering your system**.

> **Status: in development, graphical interface nearly done.** The backend is finished and reviewed (phases 0 to 12), and the graphical interface (phase 13) already has every screen: services and sites, versions, logs, credentials, diagnostics with actions, first run and uninstall, packaged with the rest and tested in Windows Sandbox. What is left before the first release is translating the last core messages, the final manual checks and publishing on winget. There are no published releases yet, and it runs only on Windows. 1,488 automated tests.

## Progress

| | Progress | Details |
|---|---|---|
| **Backend** | ███████████████████ **100%** | The 49 tasks of phases 0 to 12 in the [plan](docs/plan.md), all reviewed. |
| **Graphical interface** | █████████████████░░ **≈ 90%** | F13.1 to F13.10b done and reviewed. Left: F13.10c (the rest of the CLI in the user's language) and the pending manual checks. |
| **First release** | ████████████████░░░ **≈ 85%** | Packaging and the release workflow are ready and the winget install passed in Windows Sandbox. Left: the GUI's last task, the first tag and the pull request to `winget-pkgs`. Code signing comes later. |

*Updated 2026-11-14.*

## The problem

Local development suites for Windows solve part of the job well, but the same stumbling blocks show up in daily use:

- **Key features behind a paywall**, such as the database manager, which force you to combine several tools.
- **Environment variables and PATH entries that pile up** with every installed version, clash between tools and are not cleaned up on uninstall.
- **Configuration that reverts on its own:** a change you made by hand disappears the next time the app regenerates its files.
- **Manual steps for the basics:** enabling PHP extensions, trusting the HTTPS certificate or editing the `hosts` file.
- **Heavy applications** that load a full runtime and are tied to one operating system.

## What Crisol proposes

1. **The backend is the product.** All the logic lives in a core with no interface. The command line and the graphical interface are interchangeable shells over the same protocol.
2. **Zero system pollution.** A single PATH entry (the *shims* folder) and no per-version variables. Uninstalling leaves the system as it was.
3. **User configuration is sacred.** What you write in `crisol.toml` is the source of truth; Crisol only regenerates its own derived files and edits yours only through an explicit command, keeping comments and order.
4. **Fast and lightweight.** Small native executables, instant startup and low idle memory. The *shim* that resolves the `php` or `node` version adds only a few milliseconds.
5. **Secure by default.** Downloads verified with SHA-256 and a signed catalog, a private data folder, passwords generated outside the configuration, no telemetry, and no administrator privileges except for one isolated, optional operation.
6. **Cross-platform by design, Windows first.** Everything OS-specific sits behind *traits* in a separate crate.

## What it does today

| Area | Status |
|---|---|
| **Graphical interface** | A tray icon and a window (Tauri 2, no Node needed) on top of the daemon: services and sites live, versions with download progress, logs that follow themselves, `.env` credentials with password reset, diagnostics with buttons for the fixes Crisol knows how to make, a first-run wizard and the uninstall screen. Closing the window hides it in the tray. Dark and light themes following Windows. |
| **Versions** | Signed catalog; installs, uninstalls and lists versions of PHP, Nginx, Node, Composer and data services, with SHA-256 verification and no leftovers if something fails. Security advisories and end-of-support warnings on install and list. |
| **PHP** | Several versions side by side. A generated `php.ini` per version with the common extensions enabled, the system root certificates for cURL/OpenSSL and your `overrides/` applied last. A `php-cgi` pool per version with worker restarts. |
| **Per-project version** | The *shim* picks the version from `.crisol.toml`, `.php-version`, `.nvmrc`, `engines` or `composer.json`, without touching the PATH per project. |
| **Sites** | `park` and `link` serve folders as `name.test` with Nginx, each with its own PHP version. Configuration is generated from templates, never by patching files. |
| **Local HTTPS** | Its own CA, limited to special-use domains (`.test`, `.localhost`…), one certificate per site and the CA installed in the user store, without administrator rights. |
| **`.test` domains** | A local DNS responder and a system rule installed only once, with the `hosts` file as a fallback. |
| **Daemon** | `crisold` supervises the services (Job Objects: no orphan processes), watches the configuration and answers over a *named pipe* that only your user can open, with numbered events for live clients. Optional start at sign-in. |
| **Databases and services** | MySQL, MariaDB, PostgreSQL, MongoDB, Redis/Garnet, Memcached, Meilisearch and Mailpit, with generated passwords and `crisol db credentials` for your `.env`. |
| **Node** | Node 20, 22, 24 and 26 with *shims* for `node`, `npm`, `npx`, `corepack`, `pnpm` and `yarn`; global packages and cache kept separate per version. |
| **Clean terminal** | `crisol shell` (and a button in the window, in the project's folder) opens a terminal with the *shims* first and other suites' variables removed: no global PHP, Node or npm needed. Works with cmd, PowerShell 5.1 and 7, Git Bash and Windows Terminal; the preferred one is chosen in the settings. |
| **Diagnostics** | `crisol doctor` checks without changing anything: services that went down (with what their exit code means), ports taken by another program, databases slowed down by the antivirus, sites the `hosts` file sends elsewhere, unsupported versions, a daemon of another version and the Visual C++ runtime that PHP and the databases need. |
| **First run** | `crisol setup` (or the window's wizard) does once what winget cannot: the commands in your PATH, the Start menu entry and, asking each, the CA, the `.test` names and start at sign-in. It only does what is missing. |
| **Certificates** | `crisol trust` makes Windows trust the local CA and prints its thumbprint before the dialog, so you can compare them; `crisol untrust` takes it out. |
| **Languages** | English and Spanish, following the Windows language; `CRISOL_LANG` forces one. Translations are JSON files, one per language. `--json` and `--help` stay in English. |
| **Terminal output** | Tables, marks and colour when the output is a terminal; plain text when redirected, with `--plain` or `NO_COLOR`. |
| **Clean uninstall** | `crisol uninstall --all` (or the window's uninstall screen) first lists everything Crisol changed in Windows, then reverts it: sign-in entry, PATH, Start menu entry, certificates, DNS rule, `hosts` block, Defender exclusions and the helper, with a single administrator prompt. It keeps your databases, their passwords, your settings and your projects; `--purge` deletes those too. Tested in Windows Sandbox: the system ends up as it was. |

## A quick look at the CLI

```bash
crisol setup                    # first run: the commands in your PATH, the Start menu entry and (asking each) the CA, .test names and start at sign-in
crisol install php 8.4          # download and verify PHP 8.4
crisol install nginx
crisol park                     # every folder here is served as <folder>.test
crisol start                    # Nginx and PHP (starts the daemon if needed)
crisol secure shop              # https://shop.test
crisol use php 8.3 --local      # this project uses PHP 8.3
crisol enable mysql && crisol start
crisol db credentials mysql     # lines ready for your .env
crisol shell                    # a terminal with the shims first and a clean environment
crisol doctor                   # checks the installation and tells you what to fix
crisol trust                    # Windows trusts the local CA (no browser warnings)
crisol uninstall --all          # takes Crisol out of Windows; your databases and projects stay
```

To make `php`, `node` or `composer` use the *shims* in a single terminal, without changing anything on the system: `crisol env | Invoke-Expression` (PowerShell). To make it permanent: `crisol path add`.

## Install

When the first release is out, it will be installed with winget:

```powershell
winget install Alexxlml.Crisol
crisol setup
```

The window then opens from the Start menu. Updating, uninstalling and installing without winget are explained in [`docs/instalacion.md`](docs/instalacion.md) (in Spanish).

## Architecture

```
 crisol (CLI)    crisol-gui     other clients
        └─────── crisol-client ──────┘
                 IPC (named pipe)
               ┌───────▼───────┐
               │    crisold    │  daemon: owns the services
               └───────┬───────┘
               ┌───────▼───────┐
               │  crisol-core  │  pure logic, no UI or OS APIs
               └───────┬───────┘
               ┌───────▼────────┐
               │crisol-platform │  Windows today; Linux and macOS later
               └────────────────┘
  crisol-shim: resolves the php/node/composer version per project
```

| Crate | Responsibility |
|---|---|
| `crisol-core` | Models, configuration, catalog, config generation, orchestration and the translatable messages. Defines the platform *traits* and their test doubles. |
| `crisol-platform` | Windows implementation of those *traits*: processes, ports, permissions, certificates, DNS, PATH, shortcuts, terminals. |
| `crisol-protocol` | Versioned IPC messages and events. This is the contract every client uses ([`docs/protocolo.md`](docs/protocolo.md), in Spanish). |
| `crisol-daemon` | `crisold`: keeps the services alive and serves the IPC. |
| `crisol-client` | Shared by the CLI and the GUI: the connection to the daemon, the live view of the state and which actions are offered at each moment, all tested without an interface. |
| `crisol-cli` | `crisol`: only parses arguments and calls the core or the daemon. |
| `crisol-gui` | `crisol-gui`: the tray and the window. It only draws and turns clicks into calls. |
| `crisol-shim` | Minimal executable installed as `php`, `node`, `npm`… that launches the right version. |
| `crisol-elevate` | Helper installed once with administrator rights for the `.test` DNS rule; nothing else runs elevated. |

More detail in [`docs/arquitectura.md`](docs/arquitectura.md) (in Spanish).

## Build

Requirements: Windows 10/11 and stable Rust 1.88 or later. The GUI needs WebView2, which comes with Windows 11 and an up-to-date Windows 10; it does not need Node or npm.

```bash
cd src
cargo build --release
bash scripts/check.sh   # formatting, clippy and tests
```

The binaries end up in `src/target/release/`. By default Crisol uses `C:\Crisol` as its data folder; the `CRISOL_HOME` variable changes it (useful for testing).

PHP, MySQL and PostgreSQL need the [Visual C++ runtime (x64)](https://aka.ms/vs/17/release/vc_redist.x64.exe); if it is missing or too old, `crisol start` says so before starting them. Crisol's own programs do not need it.

## Roadmap

| Phase | Content | Status |
|---|---|---|
| 0–1 | Skeleton, paths and configuration that is never destroyed | ✅ |
| 2 | Process supervision with no orphans | ✅ |
| 3 | Signed catalog and verified downloads | ✅ |
| 4–5 | PHP, *shims* and sites with Nginx | ✅ |
| 6–7 | Local HTTPS and `.test` domains | ✅ |
| 8 | Daemon, IPC and full CLI | ✅ |
| 9 | Databases and extra services | ✅ |
| 10 | Node | ✅ |
| 11 | Full `crisol doctor` and an uninstall that reverts everything | ✅ |
| 12 | Documented protocol for the graphical interface and a sample client | ✅ |
| 13 | Graphical interface: every screen, packaging and the first part of the translations (✅); the rest of the CLI translated (⏳) | ⏳ in progress |
| Packaging | Release workflow and `crisol setup` (✅); publishing on winget (⏳) | ⏳ in progress |
| Later | Code signing, automatic updates, Linux and macOS support | Planned |

The detailed plan, with its acceptance criteria, is in [`docs/plan.md`](docs/plan.md) (in Spanish).

## How it is developed

Crisol is a personal project built with AI agents under human direction:

- **Design and decisions:** every architecture decision is recorded as an ADR in [`docs/decisiones/`](docs/decisiones/) (30 so far: shims instead of PATH, no telemetry, secrets outside the config, a restricted CA, an uninstall that keeps the user's data, the GUI with no logic of its own, translatable messages, among others).
- **Implementation:** in phases and one task at a time, with Claude Code. Each task ends with tests, `clippy` with no warnings and a conventional commit. The rules for the agents are in [`CLAUDE.md`](CLAUDE.md).
- **Review:** at the end of each phase, an independent code review and a manual check on Windows (or Windows Sandbox) before moving on.

## Repository layout

| Folder | Content |
|---|---|
| `src/` | Source code (Cargo workspace) |
| `docs/` | Architecture, phase plan, protocol, catalog, install and publishing guides, and decisions (ADR), in Spanish |
| `analisis/` | Research notes: DNS on Windows, binary sources, Node *shims*, GUI prototypes and open problems, in Spanish |
| `assets/` | Visual identity: symbol, icons and design tokens |
| `packaging/` | winget manifest templates |
| `licenses/` | Licenses of the bundled third-party files (see `THIRD-PARTY-NOTICES.md`) |

## Support Crisol

Crisol is free and open source. If you'd like to support its development and maintenance, you can do so through [GitHub Sponsors](https://github.com/sponsors/Alexxlml) or [Ko-fi](https://ko-fi.com/alexxlml). Starring the repository and sharing ideas or reporting bugs helps a lot too.

## License

Crisol is licensed under either of these two licenses, at your option:

- MIT ([`LICENSE-MIT`](LICENSE-MIT))
- Apache 2.0 ([`LICENSE-APACHE`](LICENSE-APACHE))

Unless you explicitly state otherwise, any contribution you intentionally submit for inclusion in Crisol, as defined in the Apache 2.0 license, shall be dual licensed as above, without any additional terms or conditions.

Third-party material and its licenses are listed in [`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md).
