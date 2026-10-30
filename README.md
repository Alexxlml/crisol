# Crisol

[![License: MIT or Apache-2.0](https://img.shields.io/badge/license-MIT%20%7C%20Apache--2.0-blue)](#license)
![Platform: Windows](https://img.shields.io/badge/platform-Windows%2010%20%7C%2011-0078D4)
![Built with Rust](https://img.shields.io/badge/built%20with-Rust-CE422B)

**English** | [Español](README.es.md)

Local web development suite for PHP and Node, written in Rust.

Crisol installs and manages the tools of a local development environment —PHP, Nginx, Node, databases and supporting services— so that **everything works out of the box, with no paid features and without cluttering your system**.

> **Status: in development, backend done.** Every backend task is done (1,148 automated tests): phases 0 to 11 are reviewed, and phase 12 (the contract for the graphical interface: the documented protocol and a sample client) is waiting for its review. The graphical interface comes next. There are no published releases yet: today it is used from the command line and only on Windows.

## Progress

| | Progress | Details |
|---|---|---|
| **Backend** | ███████████████████ **100%** | The 49 tasks in the [plan](docs/plan.md). Phase 12 is waiting for its review. |
| **First release with a graphical interface** | ███████████▉░░░░░░░ **≈ 55–65%** | An estimate: the graphical interface has not been broken into tasks yet. Still to do: choose the framework (Slint or Tauri 2), build the screens on top of the protocol, the installer and code signing. |

*Updated 2026-10-30.*

## The problem

Local development suites for Windows solve part of the job well, but the same stumbling blocks show up in daily use:

- **Key features behind a paywall**, such as the database manager, which force you to combine several tools.
- **Environment variables and PATH entries that pile up** with every installed version, clash between tools and are not cleaned up on uninstall.
- **Configuration that reverts on its own:** a change you made by hand disappears the next time the app regenerates its files.
- **Manual steps for the basics:** enabling PHP extensions, trusting the HTTPS certificate or editing the `hosts` file.
- **Heavy applications** that load a full runtime and are tied to one operating system.

## What Crisol proposes

1. **The backend is the product.** All the logic lives in a core with no interface. Today's command line and tomorrow's graphical interface are interchangeable shells over the same protocol.
2. **Zero system pollution.** A single PATH entry (the *shims* folder) and no per-version variables. Uninstalling leaves the system as it was.
3. **User configuration is sacred.** What you write in `crisol.toml` is the source of truth; Crisol only regenerates its own derived files and edits yours only through an explicit command, keeping comments and order.
4. **Fast and lightweight.** Small native executables, instant startup and low idle memory. The *shim* that resolves the `php` or `node` version adds only a few milliseconds.
5. **Secure by default.** Downloads verified with SHA-256 and a signed catalog, a private data folder, passwords generated outside the configuration, no telemetry, and no administrator privileges except for one isolated, optional operation.
6. **Cross-platform by design, Windows first.** Everything OS-specific sits behind *traits* in a separate crate.

## What it does today

| Area | Status |
|---|---|
| **Versions** | Signed catalog; installs, uninstalls and lists versions of PHP, Nginx, Node, Composer and data services, with SHA-256 verification and no leftovers if something fails. |
| **PHP** | Several versions side by side. A generated `php.ini` per version with the common extensions enabled, the system root certificates for cURL/OpenSSL and your `overrides/` applied last. A `php-cgi` pool per version with worker restarts. |
| **Per-project version** | The *shim* picks the version from `.crisol.toml`, `.php-version`, `.nvmrc`, `engines` or `composer.json`, without touching the PATH per project. |
| **Sites** | `park` and `link` serve folders as `name.test` with Nginx, each with its own PHP version. Configuration is generated from templates, never by patching files. |
| **Local HTTPS** | Its own CA, limited to special-use domains (`.test`, `.localhost`…), one certificate per site and the CA installed in the user store, without administrator rights. |
| **`.test` domains** | A local DNS responder and a system rule installed only once, with the `hosts` file as a fallback. |
| **Daemon** | `crisold` supervises the services (Job Objects: no orphan processes), watches the configuration and answers over a *named pipe* that only your user can open. Optional start at sign-in. |
| **Databases and services** | MySQL, MariaDB, PostgreSQL, MongoDB, Redis/Garnet, Memcached, Meilisearch and Mailpit, with generated passwords and `crisol db credentials` for your `.env`. |
| **Node** | Node 20, 22 and 24 with *shims* for `node`, `npm`, `npx`, `corepack`, `pnpm` and `yarn`; global packages and cache kept separate per version. |
| **Diagnostics** | `crisol doctor` checks without changing anything: services that went down (with what their exit code means), ports taken by another program, databases slowed down by the antivirus, sites the `hosts` file sends elsewhere, unsupported versions and the Visual C++ runtime that PHP and the databases need. |
| **Certificates** | `crisol trust` makes Windows trust the local CA and prints its thumbprint before the dialog, so you can compare them; `crisol untrust` takes it out. |
| **Clean uninstall** | `crisol uninstall --all` first lists everything Crisol changed in Windows, then reverts it: sign-in entry, PATH, certificates, DNS rule, `hosts` block, Defender exclusions and the helper, with a single administrator prompt. It keeps your databases, their passwords, your settings and your projects; `--purge` deletes those too. Tested in Windows Sandbox: the system ends up as it was. |

## A quick look at the CLI

```bash
crisol install php 8.4          # download and verify PHP 8.4
crisol install nginx
crisol park                     # every folder here is served as <folder>.test
crisol start                    # Nginx and PHP (starts the daemon if needed)
crisol secure shop              # https://shop.test
crisol use php 8.3 --local      # this project uses PHP 8.3
crisol enable mysql && crisol start
crisol db credentials mysql     # lines ready for your .env
crisol doctor                   # checks the installation and tells you what to fix
crisol trust                    # Windows trusts the local CA (no browser warnings)
crisol uninstall --all          # takes Crisol out of Windows; your databases and projects stay
```

To make `php`, `node` or `composer` use the *shims* in a single terminal, without changing anything on the system: `crisol env | Invoke-Expression` (PowerShell). To make it permanent: `crisol path add`.

## Architecture

```
 crisol (CLI)    future GUI     other clients
        └──────────────┼──────────────┘
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
| `crisol-core` | Models, configuration, catalog, config generation, orchestration. Defines the platform *traits* and their test doubles. |
| `crisol-platform` | Windows implementation of those *traits*: processes, ports, permissions, certificates, DNS, PATH. |
| `crisol-protocol` | Versioned IPC messages. This is the contract the graphical interface will use. |
| `crisol-daemon` | `crisold`: keeps the services alive and serves the IPC. |
| `crisol-cli` | `crisol`: only parses arguments and calls the core or the daemon. |
| `crisol-shim` | Minimal executable installed as `php`, `node`, `npm`… that launches the right version. |
| `crisol-elevate` | Helper installed once with administrator rights for the `.test` DNS rule; nothing else runs elevated. |

More detail in [`docs/arquitectura.md`](docs/arquitectura.md) (in Spanish).

## Build

Requirements: Windows 10/11 and stable Rust 1.88 or later.

```bash
cd src
cargo build --release
bash scripts/check.sh   # formatting, clippy and tests
```

The binaries end up in `src/target/release/`. By default Crisol uses `C:\Crisol` as its data folder; the `CRISOL_HOME` variable changes it (useful for testing).

PHP, MySQL and PostgreSQL need the [Visual C++ runtime (x64)](https://aka.ms/vs/17/release/vc_redist.x64.exe); if it is missing or too old, `crisol start` says so before starting them. Crisol's own programs do not need it.

> Crisol is not ready for daily use yet: it has no installer or published releases.

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
| 12 | Documented protocol for the graphical interface (✅) and a sample client | ⏳ in progress |
| Later | Graphical interface, signed installer, Linux and macOS support | Planned |

The detailed plan, with its acceptance criteria, is in [`docs/plan.md`](docs/plan.md) (in Spanish).

## How it is developed

Crisol is a personal project built with AI agents under human direction:

- **Design and decisions:** every architecture decision is recorded as an ADR in [`docs/decisiones/`](docs/decisiones/) (23 so far: shims instead of PATH, no telemetry, secrets outside the config, a restricted CA, an uninstall that keeps the user's data, among others).
- **Implementation:** in phases and one task at a time, with Claude Code. Each task ends with tests, `clippy` with no warnings and a conventional commit. The rules for the agents are in [`CLAUDE.md`](CLAUDE.md).
- **Review:** at the end of each phase, an independent code review and a manual check on Windows before moving on.

## Repository layout

| Folder | Content |
|---|---|
| `src/` | Source code (Cargo workspace) |
| `docs/` | Architecture, phase plan, catalog and decisions (ADR), in Spanish |
| `analisis/` | Research notes: DNS on Windows, binary sources, Node *shims* and open problems, in Spanish |
| `licenses/` | Licenses of the bundled third-party files (see `THIRD-PARTY-NOTICES.md`) |

## Support Crisol

Crisol is free and open source. If you'd like to support its development and maintenance, you can do so through [GitHub Sponsors](https://github.com/sponsors/Alexxlml) or [Ko-fi](https://ko-fi.com/alexxlml). Starring the repository and sharing ideas or reporting bugs helps a lot too.

## License

Crisol is licensed under either of these two licenses, at your option:

- MIT ([`LICENSE-MIT`](LICENSE-MIT))
- Apache 2.0 ([`LICENSE-APACHE`](LICENSE-APACHE))

Unless you explicitly state otherwise, any contribution you intentionally submit for inclusion in Crisol, as defined in the Apache 2.0 license, shall be dual licensed as above, without any additional terms or conditions.

Third-party material and its licenses are listed in [`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md).
