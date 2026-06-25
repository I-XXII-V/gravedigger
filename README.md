# Gravedigger

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![CI](https://img.shields.io/github/actions/workflow/status/I-XXII-V/Gravedigger/rust.yml?branch=main)](https://github.com/I-XXII-V/Gravedigger/actions)


```bash
# scan AUR packages (default)
gravedigger

# scan Rust project
gravedigger --cargo

# who depends on serde?
gravedigger who-depends serde

# what changed since last commit?
gravedigger diff

# compare against a specific ref
gravedigger diff v1.0

# JSON for jq
gravedigger --cargo --json | jq '.packages[] | select(.health == "dead")'

# CI mode — exit 1 if anything is dead or has CVEs
gravedigger --npm --ci
```

## Install

**Binary:**
```bash
cargo install --git https://github.com/I-XXII-V/Gravedigger
```



AUR scanning needs `pacman -Qm`, so it's Arch-only. The rest (Cargo, npm, PyPI, Go) work on any Linux distro where you've inevitably accumulated dependencies you don't remember adding.

## Usage

```text
gravedigger [OPTIONS] [PACKAGE] [COMMAND]

Commands:
  who-depends  Show crates that depend on a given crate
  diff         Show dependency changes between git revisions with health info

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
      --sbom          Output CycloneDX 1.6 SBOM JSON
  -h, --help          Print help
  -V, --version       Print version
```

## Examples

```bash
# question your life choices
gravedigger --cargo
gravedigger --npm
gravedigger --pypi
gravedigger --go

# ignore the healthy ones, focus on the dumpster fire
gravedigger --cargo --stale

# make CI fail because someone didn't update their crate since 2021
gravedigger --go --ci

# see what licenses you're violating
gravedigger --npm --licenses

# generate a CycloneDX 1.6 SBOM with CVE data
gravedigger --cargo --sbom | jq '.vulnerabilities'

# pipe to tools like grype, trivy, or dependency-track
gravedigger --npm --sbom > sbom.json
```

With `--stale`, each package explains why it's rotting:

```
⚠️ tracing v0.1.44 — Application-level tracing for Rust, downloads: 658.4M
   └─ No release on crates.io in 182 days
```

AUR packages get multiple reasons when needed — including the LastModified fallback:

```
🪦 pipes.sh — maintainer: StefansMez, popularity: 1.6
   └─ GitHub fetch failed: HTTP 403 (rate limited)
      PKGBUILD not updated in 2916 days — DEAD
```

With `--json`, you can pipe it somewhere that makes you look productive:

```bash
gravedigger --cargo --json | jq '.summary'
gravedigger --cargo --json | jq '.packages[] | select(.health == "dead") | .name'
gravedigger --cargo --stale --json | jq '.packages[].stale_reason'
gravedigger --json | jq '.summary.hijack'          # AUR: hijack count
```

### Single package info

```bash
gravedigger yay
gravedigger neovim
gravedigger --aur rust-analyzer
```

Shows AUR metadata plus GitHub stars, forks, last commit, and archive status. Basically a digital obituary.

### Reverse dependencies

```bash
gravedigger who-depends serde
gravedigger wd tokio
```

See who else is living dangerously by depending on the same things you do.

### Dependency diff

```bash
# compare current deps against the last commit
gravedigger diff

# pick an ecosystem explicitly
gravedigger diff --cargo

# compare against a specific branch or tag
gravedigger diff main
gravedigger diff v1.0

# use rev-parse style refs too
gravedigger diff --npm HEAD~3
gravedigger diff --go HEAD
```

Shows what was **added**, **upgraded**, and **removed** between two points in git history. Only the changed dependencies get health-scored, so you can see whether that upgrade introduced something worse without scrolling past 300 packages that haven't moved.

Uses `git show` internally — no extra tools, no copy-pasting lockfiles between branches. Ecosystem auto-detection works the same as the main command. When you don't specify `OLD_REF`, it defaults to `HEAD~1` (the last commit).

## CVE scanning

Gravedigger checks CVEs via [OSV.dev](https://osv.dev) for each dependency. Supported for Cargo, npm, PyPI, and Go. AUR is skipped — OSV doesn't support it.

If there's a CVE, you'll see it:

```
🚨 3 CVEs: CVE-2024-47081, CVE-2024-35195, CVE-2026-25645
```

Use `--ci` to exit with code 1 when CVEs are found. Because deploying known vulnerabilities to production is a bold strategy, Cotton. Let's see if it pays off for 'em.

Results are cached in `~/.cache/gravedigger/`. Second scan is faster. First scan is still faster than reading the actual CVE descriptions.

## SBOM output

Generate a [CycloneDX](https://cyclonedx.org) 1.6 JSON SBOM with `--sbom`:

```bash
gravedigger --cargo --sbom
gravedigger --npm --sbom | jq '.vulnerabilities'
gravedigger --go --sbom > sbom.json
```

Each package becomes a `component` with its PURL, health status, and stale reason. Known CVEs from OSV become `vulnerabilities` with severity ratings. Compatible with tools like [Grype](https://github.com/anchore/grype), [Trivy](https://github.com/aquasecurity/trivy), and [Dependency-Track](https://dependencytrack.org/).

Combine with `--stale` to limit the SBOM to only packages that need attention.

AUR scanning is not supported with `--sbom` — there's no lockfile to produce an SBOM from.

## Health scoring

For registry packages (Cargo, npm, PyPI, Go):

1. **Last release date** on the registry (crates.io / npm / PyPI / Go proxy)
2. **Last commit date** on GitHub (if upstream is on GitHub)

For AUR packages:

1. **Out-of-date flag** from AUR RPC — immediate ⚠️
2. **GitHub last commit** — if the upstream is on GitHub
3. **AUR `LastModified` timestamp** — fallback (no rate limits, no extra API calls)

This means even without a `GITHUB_TOKEN`, all your AUR packages get scored from the PKGBUILD modification date instead of showing ❓.

Additional AUR signal: if a PKGBUILD was updated recently (< 90 days) but the package is orphaned with low popularity, Gravedigger flags a potential **maintainer takeover / supply-chain hijack** risk (🚩). These show up separately in the summary so they don't get lost in the warning count:

```
📊 Summary: ✅ 12  ⚠️ 5  🚩 2  🔴 1  🪦 0  ❓ 39
```

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
