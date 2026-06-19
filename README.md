# Watchtower

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![CI](https://img.shields.io/github/actions/workflow/status/I-XXII-V/Watchtower/rust.yml?branch=main)](https://github.com/I-XXII-V/Watchtower/actions)

Check if your dependencies are still alive, or if yet another open-source maintainer has burnt out and left you holding the bag. Also finds CVEs — because apparently nobody reads advisory databases until they're in a breach postmortem.

```bash
# scan AUR packages (default)
watchtower

# scan Rust project
watchtower --cargo

# who depends on serde?
watchtower who-depends serde

# JSON for jq
watchtower --cargo --json | jq '.packages[] | select(.health == "dead")'

# CI mode — exit 1 if anything is dead or has CVEs
watchtower --npm --ci
```

## Install

**Binary (you don't have a choice):**
```bash
curl -L https://github.com/I-XXII-V/Watchtower/releases/latest/download/watchtower -o watchtower
chmod +x watchtower && sudo mv watchtower /usr/local/bin/
```

**From source:**
```bash
cargo install --git https://github.com/I-XXII-V/Watchtower
```

AUR scanning needs `pacman -Qm`, so it's Arch-only. The rest (Cargo, npm, PyPI, Go) work on any Linux distro where you've inevitably accumulated dependencies you don't remember adding.

## Usage

```text
watchtower [OPTIONS] [PACKAGE] [COMMAND]

Commands:
  who-depends  Show crates that depend on a given crate

Arguments:
  <PACKAGE>           Show detailed info for an AUR package

Options:
  -a, --aur <QUERY>   Search AUR packages with health data
  -c, --cargo         Scan Cargo.lock
  -n, --npm           Scan package-lock.json
  -p, --pypi          Scan poetry.lock / Pipfile.lock
  -g, --go            Scan go.mod
  -j, --json          Output JSON
  -s, --stale         Only show packages that should worry you
      --ci            Exit 1 if any dep is dead or has CVEs
      --licenses      Show license breakdown (you probably don't care until legal asks)
  -h, --help          Print help
  -V, --version       Print version
```

## Examples

```bash
# question your life choices
watchtower --cargo
watchtower --npm
watchtower --pypi
watchtower --go

# ignore the healthy ones, focus on the dumpster fire
watchtower --cargo --stale

# make CI fail because someone didn't update their crate since 2021
watchtower --go --ci

# see what licenses you're violating
watchtower --npm --licenses
```

With `--stale`, each package explains why it's rotting:

```
⚠️ paru — maintainer: Morganamilo, popularity: 38.5
   └─ No release on crates.io in 182 days
```

AUR packages get multiple reasons when needed:

```
🪦 pipes.sh — maintainer: StefansMez, popularity: 1.6
   └─ GitHub fetch failed: HTTP 403 (rate limited)
      PKGBUILD not updated in 2916 days — DEAD
```

With `--json`, you can pipe it somewhere that makes you look productive:

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

Shows AUR metadata plus GitHub stars, forks, last commit, and archive status. Basically a digital obituary.

### Reverse dependencies

```bash
watchtower who-depends serde
watchtower wd tokio
```

See who else is living dangerously by depending on the same things you do.

## CVE scanning

Watchtower checks CVEs via [OSV.dev](https://osv.dev) for each dependency. Supported for Cargo, npm, PyPI, and Go. AUR is skipped — OSV doesn't support it.

If there's a CVE, you'll see it:

```
🚨 3 CVEs: CVE-2024-47081, CVE-2024-35195, CVE-2026-25645
```

Use `--ci` to exit with code 1 when CVEs are found. Because deploying known vulnerabilities to production is a bold strategy, Cotton. Let's see if it pays off for 'em.

Results are cached in `~/.cache/watchtower/`. Second scan is faster. First scan is still faster than reading the actual CVE descriptions.

## Health scoring

For registry packages (Cargo, npm, PyPI, Go):

1. **Out-of-date flag** on AUR — immediate ⚠️
2. **Last release date** on the registry (crates.io / npm / PyPI / Go proxy)
3. **Last commit date** on GitHub (if upstream is on GitHub)

For AUR packages:

1. **Out-of-date flag** from AUR RPC — immediate ⚠️
2. **GitHub last commit** — if the upstream is on GitHub
3. **AUR `LastModified` timestamp** — fallback (no rate limits, no extra API calls)

This means even without a `GITHUB_TOKEN`, all your AUR packages get scored from the PKGBUILD modification date instead of showing ❓.

Additional AUR signal: if a PKGBUILD was updated recently (< 90 days) but the package is orphaned with low popularity, Watchtower flags a potential **maintainer takeover / supply-chain hijack** risk (🚩). These show up separately in the summary so they don't get lost in the warning count.

| Status | Meaning |
|--------|---------|
| ✅ | Active — someone pushed code this decade |
| ⚠️ | Stale — 6–12 months of silence. Maintainer might just be busy. Or dead. We don't know. |
| 🚩 | Hijack risk — recently updated but orphaned with low popularity. Someone's been busy. |
| 🔴 | Inactive — 1–2 years. Start writing that migration guide. |
| 🪦 | Dead — over 2 years. It's not coming back. Hold a funeral. |
| ❓ | Unknown — couldn't fetch data. The package exists but that's all we know. Like a Schrödinger's dependency. |

## GITHUB_TOKEN (optional)

GitHub rate-limits unauthenticated requests to 60/hour. Without a token you'll start seeing "GitHub fetch failed" faster than you can say "why is my build broken":

```bash
export GITHUB_TOKEN="github_pat_..."
```

Create one at GitHub Settings → Developer settings → Personal access tokens. No special scopes needed. Just like every other tool that pretends to work without one.

## Supported lockfiles

| Ecosystem | File |
|-----------|------|
| Cargo | `Cargo.lock` |
| npm | `package-lock.json` |
| PyPI | `poetry.lock` / `Pipfile.lock` |
| Go | `go.mod` |

## License

[MIT](LICENSE) — do whatever you want. Like every dependency you've ever used, this one might also be abandoned someday. Use at your own risk. Don't say we didn't warn you.
