mod api;
mod cache;
mod cargo;
mod diff;
mod display;
mod downstream;
mod golang;
mod npm;
mod osv;
mod pypi;
mod types;

use crate::display::*;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "rot",
    about = "Check the health of your dependencies across AUR, Cargo, npm, PyPI, and Go",
    long_about = "\
     ┌───┐
     │ W │
  ┌──┤   ├──┐
  │  │   │  │
  │  └───┘  │
  │         │
  └─────────┘
   │       │
   │       │
  ┌┘       └┐

Rot — scan your project's dependencies and see which ones are
healthy, stale, inactive, or completely dead.

Works with: AUR, Cargo.lock, package-lock.json, poetry.lock /
Pipfile.lock, and go.mod.",
    version
)]
struct Cli {
    /// Search AUR packages with health data
    #[arg(short = 'a', long = "aur", value_name = "QUERY")]
    aur: Option<String>,

    /// Scan Cargo.lock dependencies
    #[arg(short = 'c', long = "cargo")]
    cargo: bool,

    /// Scan package-lock.json dependencies
    #[arg(short = 'n', long = "npm")]
    npm: bool,

    /// Scan Python lockfile (poetry.lock / Pipfile.lock)
    #[arg(short = 'p', long = "pypi")]
    pypi: bool,

    /// Scan Go modules (go.mod)
    #[arg(short = 'g', long = "go")]
    go: bool,

    /// Output in JSON format
    #[arg(short = 'j', long = "json")]
    json: bool,

    /// Show only unhealthy/stale packages
    #[arg(short = 's', long = "stale")]
    stale: bool,

    /// CI mode: exit with code 1 if any dependency is dead or has CVEs
    #[arg(long = "ci")]
    ci: bool,

    /// Show license breakdown for scanned packages
    #[arg(long = "licenses")]
    licenses: bool,

    /// Show fetch errors and unknown packages
    #[arg(short = 'v', long = "verbose")]
    verbose: bool,

    /// Show detailed health info for an AUR package
    package: Option<String>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Show crates that depend on a given crate
    #[command(name = "who-depends", aliases = &["wd"])]
    WhoDepends { crate_name: String },

    /// Show dependency changes between git revisions with health info
    #[command(name = "diff")]
    Diff {
        /// Old git ref to compare against (default: HEAD~1)
        old_ref: Option<String>,

        /// Scan Cargo.lock
        #[arg(long = "cargo")]
        cargo: bool,

        /// Scan package-lock.json
        #[arg(long = "npm")]
        npm: bool,

        /// Scan Python lockfile (poetry.lock / Pipfile.lock)
        #[arg(long = "pypi")]
        pypi: bool,

        /// Scan Go modules (go.mod)
        #[arg(long = "go")]
        go: bool,
    },
}

fn main() {
    let cli = Cli::parse();

    // Subcommand: who-depends <crate>
    if let Some(cmd) = cli.command {
        match cmd {
            Commands::WhoDepends { crate_name } => {
                downstream::who_depends_crates(&crate_name);
            }
            Commands::Diff {
                old_ref,
                cargo,
                npm,
                pypi,
                go,
            } => {
                let ref_str = old_ref.unwrap_or_else(|| "HEAD~1".to_string());
                let ecosystem: Option<&str> = if cargo {
                    Some("cargo")
                } else if npm {
                    Some("npm")
                } else if pypi {
                    Some("pypi")
                } else if go {
                    Some("go")
                } else {
                    None
                };
                diff::run_diff(&ref_str, ecosystem);
            }
        }
        return;
    }

    // Ecosystem scan flags
    if cli.cargo {
        cargo::scan_cargo_deps(cli.stale, cli.json, cli.ci, cli.licenses, cli.verbose);
        return;
    }
    if cli.npm {
        npm::scan_npm_deps(cli.stale, cli.json, cli.ci, cli.licenses, cli.verbose);
        return;
    }
    if cli.pypi {
        pypi::scan_pypi_deps(cli.stale, cli.json, cli.ci, cli.licenses, cli.verbose);
        return;
    }
    if cli.go {
        golang::scan_go_deps(cli.stale, cli.json, cli.ci, cli.licenses, cli.verbose);
        return;
    }

    // AUR search
    if let Some(query) = cli.aur {
        search_and_display(&query, cli.json);
        return;
    }

    // --stale with no ecosystem flag → scan AUR (stale only)
    if cli.stale {
        scan_installed(true, cli.json, cli.ci, cli.verbose);
        return;
    }

    // Single AUR package
    if let Some(pkg) = cli.package {
        display::single_package_json(&pkg, cli.json);
        return;
    }

    // Default: scan all AUR packages
    scan_installed(false, cli.json, cli.ci, cli.verbose);
}
