use serde::Deserialize;
use std::env;
use std::sync::OnceLock;
use std::time::Duration;

// ── Shared reqwest client with timeouts ─────────────────────────────────

/// Returns a lazily-initialised, shared `blocking::Client` with a 30-second
/// timeout.  All modules should use this instead of `Client::new()`.
pub fn http_client() -> &'static reqwest::blocking::Client {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("vigil")
            .build()
            .expect("reqwest client build should never fail")
    })
}

// ── AUR types ───────────────────────────────────────────────────────────

#[derive(Deserialize, Debug)]
pub struct AurResponse {
    pub resultcount: u32,
    pub results: Vec<AurPackage>,
}

#[derive(Deserialize, Debug)]
pub struct AurPackage {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Version")]
    pub version: String,
    #[serde(rename = "Description")]
    pub description: Option<String>,
    #[serde(rename = "URL")]
    pub url: Option<String>,
    #[serde(rename = "Maintainer")]
    pub maintainer: Option<String>,
    #[serde(rename = "NumVotes")]
    pub numvotes: u32,
    #[serde(rename = "Popularity")]
    pub popularity: f64,
    #[serde(rename = "LastModified")]
    pub lastmodified: u64,
    #[serde(rename = "OutOfDate")]
    pub outofdate: Option<u32>,
}

// ── GitHub API ──────────────────────────────────────────────────────────

#[derive(Deserialize, Debug)]
pub struct GitHubRepo {
    #[serde(rename = "stargazers_count")]
    pub stars: u32,
    #[serde(rename = "forks_count")]
    pub forks: u32,
    #[serde(rename = "open_issues_count")]
    pub open_issues: u32,
    pub pushed_at: String,
    pub archived: bool,
    #[serde(rename = "subscribers_count")]
    pub watchers: u32,
}

/// Safely take the first `n` characters from a string, respecting UTF-8
/// boundaries.  Use this instead of `&text[..n.min(text.len())]` which can
/// panic in the middle of a multi-byte character.
pub fn safe_prefix(s: &str, n: usize) -> &str {
    let byte_idx = s
        .char_indices()
        .nth(n)
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    &s[..byte_idx]
}

/// Parse an `owner` / `repo` pair from common GitHub URL formats:
///
/// | Format                                | Result          |
/// |---------------------------------------|-----------------|
/// | `https://github.com/owner/repo`       | `(owner, repo)` |
/// | `https://github.com/owner/repo.git`   | `(owner, repo)` |
/// | `https://github.com/owner/repo/`      | `(owner, repo)` |
/// | `https://github.com/owner/repo/tree/…`| `(owner, repo)` |
/// | `git@github.com:owner/repo.git`       | `(owner, repo)` |
/// | `ssh://git@github.com/owner/repo`     | `(owner, repo)` |
/// | `git+https://github.com/owner/repo`   | `(owner, repo)` |
/// | `git://github.com/owner/repo.git`     | `(owner, repo)` |
pub fn parse_github_repo(url: &str) -> Option<(String, String)> {
    // Find `github.com/` or `github.com:` — this handles every protocol
    // (https://, git@, ssh://, git://, git+) in one pass.
    let after_host = if let Some(pos) = url.find("github.com/") {
        &url[pos + "github.com/".len()..]
    } else if let Some(pos) = url.find("github.com:") {
        &url[pos + "github.com:".len()..]
    } else {
        return None;
    };

    // Strip trailing `.git` and `/`
    let after_host = after_host.trim_end_matches(".git").trim_end_matches('/');

    // Take first two path segments: owner / repo
    let mut parts = after_host.split('/');
    let owner = parts.next()?.to_string();
    let repo = parts.next()?.to_string();

    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((owner, repo))
}

pub fn fetch_github_info(owner: &str, repo: &str) -> Result<GitHubRepo, String> {
    let url = format!("https://api.github.com/repos/{}/{}", owner, repo);
    let token = env::var("GITHUB_TOKEN").unwrap_or_default();

    let mut req = http_client().get(&url);

    if !token.is_empty() {
        req = req.header("Authorization", format!("Bearer {}", token));
    }

    let response = req.send().map_err(|e| format!("Network error: {}", e))?;
    let status = response.status();
    let text = response.text().map_err(|e| format!("Read error: {}", e))?;

    if !status.is_success() {
        return Err(format!("HTTP {} — {}", status, safe_prefix(&text, 200)));
    }

    serde_json::from_str(&text).map_err(|e| {
        format!("Parse error: {} — body: {}", e, safe_prefix(&text, 200))
    })
}

// ── AUR API (simple enough to not need the shared client) ──────────────

pub fn fetch_aur_info(url: &str) -> Result<AurResponse, reqwest::Error> {
    http_client().get(url).send()?.json::<AurResponse>()
}

pub fn search_aur(query: &str) -> Result<AurResponse, reqwest::Error> {
    let url = format!("https://aur.archlinux.org/rpc/v5/search/{}", query);
    http_client().get(&url).send()?.json::<AurResponse>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_github_repo_https() {
        let result = parse_github_repo("https://github.com/owner/repo");
        assert_eq!(result, Some(("owner".into(), "repo".into())));
    }

    #[test]
    fn test_parse_github_repo_with_trailing_slash() {
        let result = parse_github_repo("https://github.com/owner/repo/");
        assert_eq!(result, Some(("owner".into(), "repo".into())));
    }

    #[test]
    fn test_parse_github_repo_with_subdirectory() {
        let result = parse_github_repo("https://github.com/owner/repo/tree/main/src");
        assert_eq!(result, Some(("owner".into(), "repo".into())));
    }

    #[test]
    fn test_parse_github_repo_not_github() {
        let result = parse_github_repo("https://gitlab.com/owner/repo");
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_github_repo_invalid() {
        let result = parse_github_repo("not-a-url");
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_github_repo_empty() {
        let result = parse_github_repo("");
        assert_eq!(result, None);
    }

    // New formats

    #[test]
    fn test_parse_github_repo_dot_git() {
        let result = parse_github_repo("https://github.com/owner/repo.git");
        assert_eq!(result, Some(("owner".into(), "repo".into())));
    }

    #[test]
    fn test_parse_github_repo_git_ssh() {
        let result = parse_github_repo("git@github.com:owner/repo.git");
        assert_eq!(result, Some(("owner".into(), "repo".into())));
    }

    #[test]
    fn test_parse_github_repo_git_https_prefix() {
        let result = parse_github_repo("git+https://github.com/owner/repo.git");
        assert_eq!(result, Some(("owner".into(), "repo".into())));
    }

    #[test]
    fn test_parse_github_repo_ssh_protocol() {
        let result = parse_github_repo("ssh://git@github.com/owner/repo");
        assert_eq!(result, Some(("owner".into(), "repo".into())));
    }
}
