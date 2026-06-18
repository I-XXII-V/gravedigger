# Watchtower

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![CI](https://img.shields.io/github/actions/workflow/status/I-XXII-V/Watchtower/rust.yml?branch=main)](https://github.com/I-XXII-V/Watchtower/actions)

Cek kesehatan dependency project lo dari berbagai ecosystem — AUR, Cargo, npm, PyPI, sama Go. Tinggal scan, liat mana aja yang udah basi atau ditinggal mati sama maintainer.

Ada command `who-depends` juga buat liat siapa aja yang depende ke suatu package (kalo lo penasaran).

```bash
# scan semua AUR package yang terinstall
watchtower

# scan project Rust lo
watchtower --cargo

# liat reverse dependencies serde
watchtower who-depends serde

# output JSON, pipe ke jq
watchtower --cargo --json | jq '.packages[] | select(.health == "dead")'
```

## Install

**Binary langsung (paling gampang):**
```bash
curl -L https://github.com/I-XXII-V/Watchtower/releases/latest/download/watchtower -o watchtower
chmod +x watchtower && sudo mv watchtower /usr/local/bin/
```

**AUR (Arch):**
```bash
yay -S watchtower
```

**Cargo source:**
```bash
cargo install --git https://github.com/I-XXII-V/Watchtower
```

Note: kalo lo pake Arch, AUR scan butuh `pacman -Qm`. Yang pake ecosystem lain (npm, PyPI, dll) gak perlu Arch — tinggal masuk ke folder project dan scan.

## Cara pake

```text
watchtower [OPTIONS] [PACKAGE]

Arguments:
  <PACKAGE>              Detail info suatu AUR package

Options:
  -a, --aur <QUERY>      Cari AUR package
  -c, --cargo            Scan Cargo.lock
  -n, --npm              Scan package-lock.json
  -p, --pypi             Scan poetry.lock / Pipfile.lock
  -g, --go               Scan go.mod
  -j, --json             Output JSON
  -s, --stale            Filter: tampilkan yg bermasalah aja

Subcommands:
  who-depends, wd <crate>  Cari reverse dependencies suatu crate
```

### Scan dependency project

Masuk ke folder project, terus jalanin:

```bash
watchtower --cargo   # Rust — baca Cargo.lock
watchtower --npm     # JS — baca package-lock.json
watchtower --pypi    # Python — baca poetry.lock atau Pipfile.lock
watchtower --go      # Go — baca go.mod
```

Bisa digabung sama `--stale` biar yang keliatan cuma yang bermasalah:

```bash
watchtower --cargo --stale
```

Nanti muncul alesan kenapa dia dianggep stale:
```
⚠️ tracing v0.1.44 — Application-level tracing for Rust, downloads: 658.4M
   └─ No release on crates.io in 182 days
```

### Output JSON

Buat scripting atau CI:

```bash
watchtower --cargo --json | jq '.summary'
watchtower --cargo --json | jq '.packages[] | select(.health == "dead") | .name'
watchtower --cargo --stale --json | jq '.packages[].stale_reason'
```

### Liat detail satu package

```bash
watchtower yay
watchtower neovim
watchtower --aur rust-analyzer
```

Outputnya: info AUR + GitHub stars, forks, last commit, detection kalo di-archive.

### Reverse dependencies

```bash
watchtower who-depends serde
watchtower wd tokio
```

## Health scoring

Watchtower nentuin kesehatan package berdasarkan 3 hal (urut):

1. **Flag out-of-date** di AUR — langsung ⚠️
2. **Kapan terakhir rilis** di registry (crates.io / npm / PyPI / Go proxy)
3. **Kapan terakhir commit** di GitHub upstream (kalo repository-nya GitHub)

| Status | Artinya |
|--------|---------|
| ✅ | Active — ada aktivitas 6 bulan terakhir |
| ⚠️ | Stale — 6-12 bulan gak ada gerak |
| 🔴 | Inactive — 1-2 tahun |
| 🪦 | Dead — >2 tahun, tinggal kenangan |
| ❓ | Unknown — gagal fetch data |

## GITHUB_TOKEN (opsional)

GitHub API punya rate limit 60 request/jam kalo tanpa token. Biar scanning lebih akurat (cek commit history), set env variable:

```bash
export GITHUB_TOKEN="github_pat_..."
```

Bikin token di GitHub Settings → Developer settings → Personal access tokens (gausah kasih scope apa-apa, cukup public access).

## Supported lockfiles

| Ecosystem | File |
|-----------|------|
| Cargo | `Cargo.lock` |
| npm | `package-lock.json` |
| PyPI | `poetry.lock` / `Pipfile.lock` |
| Go | `go.mod` |

## License

[MIT](LICENSE)
