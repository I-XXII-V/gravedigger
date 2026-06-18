mod api;
mod cargo;
mod display;
mod downstream;
mod golang;
mod npm;
mod pypi;
mod types;

use std::process;
use crate::display::*;

fn has_flag(args: &[String], short: &str, long: &str) -> bool {
    args.iter().any(|a| a == short || a == long)
}

fn print_usage() {
    println!("Watchtower — package health checker");
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
    println!("  -j, --json           Output in JSON format");
    println!("  -s, --stale          Show only unhealthy/stale packages");
    println!("  -h, --help           Show this help message");
    println!();
    println!("Subcommands:");
    println!("  who-depends, wd <crate>  Show crates that depend on a given crate");
    println!();
    println!("Examples:");
    println!("  watchtower                   Scan all installed AUR packages");
    println!("  watchtower --stale            Show only unhealthy packages");
    println!("  watchtower -a neovim          Search AUR for neovim packages");
    println!("  watchtower --cargo --json     Scan Cargo.lock, output JSON");
    println!("  watchtower yay                Check health of the 'yay' package");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let json = has_flag(&args, "-j", "--json");

    if has_flag(&args, "-h", "--help") {
        print_usage();
        return;
    }

    // Determine the "primary" arg (first non-flag arg)
    let primary = args.iter().skip(1).find(|a| !a.starts_with('-'));

    // Handle --cargo / -c
    if has_flag(&args, "-c", "--cargo") {
        let stale = has_flag(&args, "-s", "--stale");
        cargo::scan_cargo_deps(stale, json);
        return;
    }

    // Handle --npm / -n
    if has_flag(&args, "-n", "--npm") {
        let stale = has_flag(&args, "-s", "--stale");
        npm::scan_npm_deps(stale, json);
        return;
    }

    // Handle --pypi / -p
    if has_flag(&args, "-p", "--pypi") {
        let stale = has_flag(&args, "-s", "--stale");
        pypi::scan_pypi_deps(stale, json);
        return;
    }

    // Handle --go / -g
    if has_flag(&args, "-g", "--go") {
        let stale = has_flag(&args, "-s", "--stale");
        golang::scan_go_deps(stale, json);
        return;
    }

    // Handle --aur / -a <query>
    if has_flag(&args, "-a", "--aur") {
        let query = args.iter().skip(1)
            .position(|a| a == "-a" || a == "--aur")
            .and_then(|pos| args.get(pos + 2))
            .or_else(|| args.iter().skip(1).find(|a| !a.starts_with('-') && *a != "-a" && *a != "--aur"));
        match query {
            Some(q) => search_and_display(q, json),
            None => {
                eprintln!("❌ Usage: watchtower --aur <search-query>");
                process::exit(1);
            }
        }
        return;
    }

    // Handle --stale / -s with no other flag → scan AUR
    if has_flag(&args, "-s", "--stale") {
        scan_installed(true, json);
        return;
    }

    // Handle who-depends / wd subcommand
    if let Some(sub) = primary {
        if sub == "who-depends" || sub == "wd" {
            let pkg = args.iter().skip(1)
                .position(|a| a == sub)
                .and_then(|pos| args.get(pos + 1));
            match pkg {
                Some(p) => downstream::who_depends_crates(p),
                None => {
                    eprintln!("❌ Usage: watchtower who-depends <crate-name>");
                    process::exit(1);
                }
            }
            return;
        }
    }

    // No flags → scan all AUR packages
    if primary.is_none() {
        scan_installed(false, json);
        return;
    }

    // Single AUR package
    let pkg_name = primary.unwrap();
    display::single_package_json(pkg_name, json);
}
