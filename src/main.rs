mod api;
mod display;

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
    println!("  -s, --stale          Show only unhealthy/stale packages");
    println!("  -h, --help           Show this help message");
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

    if arg == "--aur" || arg == "-a" {
        if args.len() < 3 {
            eprintln!("❌ Usage: watchtower --aur <search-query>");
            process::exit(1);
        }
        search_and_display(&args[2]);
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
