# Watchtower

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![CI](https://img.shields.io/github/actions/workflow/status/I-XXII-V/Watchtower/rust.yml?branch=main)](https://github.com/I-XXII-V/Watchtower/actions)

Check if your dependencies are still alive across AUR, Cargo, npm, PyPI, and Go. Spot stale, abandoned, or out-of-date packages before they become a headache.

```bash
# scan all installed AUR packages
watchtower

# scan your Rust project
watchtower --cargo

# find out who depends on serde
watchtower who-depends serde

# JSON output, pipe to jq
watchtower --cargo --json | jq '.packages[] | select(.health == "dead")'
```

## Install

**Pre-built binary (easiest):**
```bash
curl -L https://github.com/I-XXII-V/Watchtower/releases/latest/download/watchtower -o watchtower
chmod +x watchtower && sudo mv watchtower /usr/local/bin/
```

**AUR (Arch Linux):**
```bash
yay -S watchtower
```

**From source:**
```bash
cargo install --git https://github.com/I-XXII-V/Watchtower
```

AUR scanning requires `pacman -Qm`, so that only works on Arch. Everything else (Cargo, npm, PyPI, Go) works on any Linux distro.

## Usage

```text
watchtower [OPTIONS] [PACKAGE]

Arguments:
  <PACKAGE>              Show detailed info for an AUR package

Options:
  -a, --aur <QUERY>      Search AUR packages
  -c, --cargo            Scan Cargo.lock dependencies
  -n, --npm              Scan package-lock.json dependencies
  -p, --pypi             Scan poetry.lock / Pipfile.lock
  -g, --go               Scan go.mod dependencies
  -j, --json             Output in JSON format
  -s, --stale            Only show unhealthy/stale packages

Subcommands:
  who-depends, wd <crate>  Show crates that depend on a given crate
```

### Scan your project dependencies

```bash
cd my-rust-project
watchtower --cargo

cd my-node-project
watchtower --npm

cd my-python-project
watchtower --pypi

cd my-go-project
watchtower --go
```

Add `--stale` to see only the ones that need attention:

```bash
watchtower --cargo --stale
```

Each stale package shows why:
```
⚠️ tracing v0.1.44 — Application-level tracing for Rust, downloads: 658.4M
   └─ No release on crates.io in 182 days
```

### JSON output

For scripting or CI pipelines:

```bash
watchtower --cargo --json | jq '.summary'
watchtower --cargo --json | jq '.packages[] | select(.health == "dead") | .name'
watchtower --cargo --stale --json | jq '.packages[].stale_reason'
```

### Single package info

```bash
watchtower yay
watchtower neovim
watchtower --aur rust-analyzer
```

Shows AUR metadata plus GitHub stars, forks, last commit, and archive status.

### Reverse dependencies

```bash
watchtower who-depends serde
watchtower wd tokio
```

## Health scoring

Packages get scored based on three things (in order):

1. **Out-of-date flag** on AUR — immediate ⚠️
2. **Last release date** on the registry (crates.io / npm / PyPI / Go proxy)
3. **Last commit date** on GitHub (if the upstream is on GitHub)

| Status | Meaning |
|--------|---------|
| ✅ | Active — something happened in the last 6 months |
| ⚠️ | Stale — 6 to 12 months of silence |
| 🔴 | Inactive — 1 to 2 years |
| 🪦 | Dead — over 2 years, buried |
| ❓ | Unknown — couldn't fetch data |

## GITHUB_TOKEN (optional)

GitHub rate-limits unauthenticated requests to 60/hour. Without a token you'll start seeing "GitHub fetch failed" after a while. Set this to avoid that:

```bash
export GITHUB_TOKEN="github_pat_..."
```

Create one at GitHub Settings → Developer settings → Personal access tokens. No special scopes needed.

## Supported lockfiles

| Ecosystem | File |
|-----------|------|
| Cargo | `Cargo.lock` |
| npm | `package-lock.json` |
| PyPI | `poetry.lock` / `Pipfile.lock` |
| Go | `go.mod` |

## License

[MIT](LICENSE)
