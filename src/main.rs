mod api;
mod cargo;
mod display;
mod downstream;
mod golang;
mod npm;
mod pypi;

use std::process;
use crate::display::*;

fn print_usage() {
    println!("Watchtower — AUR package health checker");
    println!();
    println!("Usage: watchtower [OPTIONS] [PACKAGE]");
    println!();
    println!("Arguments:");
    println!("  <PACKAGE>     Show detailed health info for an AUR package");
    println!();
    println!("Options:");
    println!("  -a, --aur <QUERY>    Search AUR packages with health data");
    println!("  -c, --cargo          Scan Cargo.lock dependencies");
    println!("  -n, --npm            Scan package-lock.json dependencies");
    println!("  -p, --pypi           Scan Python lockfile (poetry.lock / Pipfile.lock)");
    println!("  -g, --go             Scan Go modules (go.mod)");
    println!("  -s, --stale          Show only unhealthy/stale packages");
    println!("  -h, --help           Show this help message");
    println!();
    println!("Subcommands:");
    println!("  who-depends, wd <crate>  Show crates that depend on a given crate");
    println!();
    println!("Examples:");
    println!("  watchtower               Scan all installed AUR packages");
    println!("  watchtower --stale        Show only unhealthy packages");
    println!("  watchtower -a neovim      Search AUR for neovim packages");
    println!("  watchtower yay            Check health of the 'yay' package");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() >= 2 && (args[1] == "--help" || args[1] == "-h") {
        print_usage();
        return;
    }

    if args.len() < 2 || args[1] == "--stale" || args[1] == "-s" {
        scan_installed(args.len() >= 2 && (args[1] == "--stale" || args[1] == "-s"));
        return;
    }

    let arg = &args[1];

    if arg == "--cargo" || arg == "-c" {
        let stale = args.len() >= 3 && (args[2] == "--stale" || args[2] == "-s");
        cargo::scan_cargo_deps(stale);
        return;
    }

    if arg == "--npm" || arg == "-n" {
        let stale = args.len() >= 3 && (args[2] == "--stale" || args[2] == "-s");
        npm::scan_npm_deps(stale);
        return;
    }

    if arg == "--pypi" || arg == "-p" {
        let stale = args.len() >= 3 && (args[2] == "--stale" || args[2] == "-s");
        pypi::scan_pypi_deps(stale);
        return;
    }

    if arg == "--go" || arg == "-g" {
        let stale = args.len() >= 3 && (args[2] == "--stale" || args[2] == "-s");
        golang::scan_go_deps(stale);
        return;
    }

    if arg == "--aur" || arg == "-a" {
        if args.len() < 3 {
            eprintln!("❌ Usage: watchtower --aur <search-query>");
            process::exit(1);
        }
        search_and_display(&args[2]);
        return;
    }

    // Downstream mode: who-depends <package>
    if arg == "who-depends" || arg == "wd" {
        if args.len() < 3 {
            eprintln!("❌ Usage: watchtower who-depends <crate-name>");
            process::exit(1);
        }
        downstream::who_depends_crates(&args[2]);
        return;
    }

    println!("🔍 Watchtower: scanning {}", arg);

    let url = format!("https://aur.archlinux.org/rpc/v5/info/{}", arg);
    match api::fetch_aur_info(&url) {
        Ok(response) => {
            if response.resultcount == 0 {
                eprintln!("❌ Package '{}' not found in AUR", arg);
                process::exit(1);
            }
            let pkg = &response.results[0];
            print_package_info(pkg);

            if let Some(ref upstream_url) = pkg.url {
                if let Some((owner, repo)) = api::parse_github_repo(upstream_url) {
                    println!("\n🐙 GitHub: {}/{}", owner, repo);
                    match api::fetch_github_info(&owner, &repo) {
                        Ok(gh) => print_github_info(&gh),
                        Err(e) => eprintln!("   ❌ Fetch failed: {}", e),
                    }
                } else {
                    println!("\n🐙 GitHub: not a GitHub repository");
                }
            }
        }
        Err(e) => {
            eprintln!("❌ Failed to fetch AUR: {}", e);
            process::exit(1);
        }
    }
}
