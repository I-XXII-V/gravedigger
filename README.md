# Watchtower

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-2021-blue.svg)](https://www.rust-lang.org)
[![CI](https://img.shields.io/github/actions/workflow/status/I-XXII-V/Watchtower/rust.yml?branch=main)](https://github.com/I-XXII-V/Watchtower/actions)
[![Last Commit](https://img.shields.io/github/last-commit/I-XXII-V/Watchtower)](https://github.com/I-XXII-V/Watchtower)

> **Watchtower** — a FOSS CLI tool that checks the health of your dependencies across multiple ecosystems.  
> Scan for stale, abandoned, or out-of-date packages before they become a problem.

## Features

- **Multi-ecosystem**: AUR · Cargo (Rust) · npm (JavaScript) · PyPI (Python) · Go
- **Health scoring**: ✅ healthy · ⚠️ stale · 🔴 inactive · 🪦 dead · ❓ unknown
- **Staleness detection**: based on upstream release activity + GitHub commit history
- **Reverse dependency queries**: find out which crates depend on a given crate
- **Parallel scanning**: blazing fast (59 AUR packages in ~12s)
- **JSON output**: machine-readable for CI/CD pipelines and scripting
- **Orphan detection**: instantly spot unmaintained AUR packages

## Installation

### Binary (GitHub Release) — easiest

[Download the latest release](https://github.com/I-XXII-V/Watchtower/releases/latest) for Linux x86_64:

```bash
curl -L https://github.com/I-XXII-V/Watchtower/releases/latest/download/watchtower -o watchtower
chmod +x watchtower
sudo mv watchtower /usr/local/bin/
```

### AUR (Arch Linux)

```bash
yay -S watchtower
# or
paru -S watchtower
```

### From source (cargo)

```bash
git clone https://github.com/I-XXII-V/Watchtower.git
cd Watchtower
cargo build --release
sudo cp target/release/watchtower /usr/local/bin/
```

### Requirements

- Rust 2021 edition (1.85+) — only needed for source builds
- `pacman` (for AUR scanning on Arch Linux)
- Internet connection for upstream API queries

> **Note**: AUR scanning (`watchtower` with no flags, or `--aur`) requires `pacman -Qm`.  
> It is only available on Arch Linux and derivatives.

## Usage

```text
watchtower [OPTIONS] [PACKAGE]

Arguments:
  <PACKAGE>     Show detailed health info for an AUR package

Options:
  -a, --aur <QUERY>    Search AUR packages with health data
  -c, --cargo          Scan Cargo.lock dependencies
  -n, --npm            Scan package-lock.json dependencies
  -p, --pypi           Scan Python lockfile (poetry.lock / Pipfile.lock)
  -g, --go             Scan Go modules (go.mod)
  -j, --json           Output in JSON format
  -s, --stale          Show only unhealthy/stale packages
  -h, --help           Show this help message

Subcommands:
  who-depends, wd <crate>  Show crates that depend on a given crate
```

## Examples

### Scan AUR packages

```bash
# Scan all installed AUR packages
watchtower

# Show only stale/dead packages
watchtower --stale
```

### Scan project dependencies

```bash
# Rust project
cd my-rust-project
watchtower --cargo

# Node.js project
cd my-node-project
watchtower --npm

# Python project (poetry or pipenv)
cd my-python-project
watchtower --pypi

# Go project
cd my-go-project
watchtower --go
```

### JSON output (for scripting)

```bash
# Get machine-readable output
watchtower --cargo --json

# Filter with jq — show only dead packages
watchtower --cargo --json | jq '.packages[] | select(.health == "dead") | .name'

# Get summary stats
watchtower --cargo --json | jq '.summary'

# Combine with --stale
watchtower --cargo --stale --json
```

### Reverse dependency queries

```bash
# Find crates that depend on serde
watchtower who-depends serde

# Shorthand
watchtower wd tokio
```

### Single package health

```bash
watchtower yay
watchtower neovim
```

### Search AUR

```bash
watchtower --aur neovim
watchtower -a rust-analyzer
```

## Health Scoring

| Status | Emoji | Meaning                                    |
|--------|-------|--------------------------------------------|
| ✅     | Green | Active — updated within last 6 months      |
| ⚠️     | Amber | Stale — 6–12 months since last activity    |
| 🔴     | Red   | Inactive — 1–2 years since last activity   |
| 🪦     | Dead  | Abandoned — >2 years since last activity   |
| ❓     | Gray  | Unknown — unable to determine health       |

Health is determined by:
1. **Out-of-date flag** on AUR (immediate ⚠️)
2. **Package registry freshness** — last release on crates.io / npm / PyPI / Go proxy
3. **GitHub activity** — last commit on upstream repository

## Supported Lockfiles

| Ecosystem | File(s)                      |
|-----------|------------------------------|
| Cargo     | `Cargo.lock`                 |
| npm       | `package-lock.json`          |
| PyPI      | `poetry.lock`, `Pipfile.lock`|
| Go        | `go.mod`                     |

## GitHub Token (optional)

To avoid GitHub API rate limits (60 requests/hour without authentication),
set the `GITHUB_TOKEN` environment variable:

```bash
export GITHUB_TOKEN="github_pat_..."
```

This enables more accurate health scores via commit history analysis.

## License

[MIT](LICENSE) © 2026
