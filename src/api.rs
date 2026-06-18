use serde::Deserialize;
use std::env;

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

pub fn fetch_aur_info(url: &str) -> Result<AurResponse, reqwest::Error> {
    let response = reqwest::blocking::get(url)?.json::<AurResponse>()?;
    Ok(response)
}

pub fn search_aur(query: &str) -> Result<AurResponse, reqwest::Error> {
    let url = format!("https://aur.archlinux.org/rpc/v5/search/{}", query);
    let response = reqwest::blocking::get(&url)?.json::<AurResponse>()?;
    Ok(response)
}

pub fn parse_github_repo(url: &str) -> Option<(String, String)> {
    let url = url.trim_end_matches('/');
    if !url.starts_with("https://github.com/") {
        return None;
    }
    let parts: Vec<&str> = url.split('/').collect();
    if parts.len() < 5 {
        return None;
    }
    Some((parts[3].to_string(), parts[4].to_string()))
}

pub fn fetch_github_info(owner: &str, repo: &str) -> Result<GitHubRepo, String> {
    let url = format!("https://api.github.com/repos/{}/{}", owner, repo);
    let token = env::var("GITHUB_TOKEN").unwrap_or_default();

    let client = reqwest::blocking::Client::new();
    let mut req = client.get(&url).header("User-Agent", "watchtower");

    if !token.is_empty() {
        req = req.header("Authorization", format!("Bearer {}", token));
    }

    let response = req.send().map_err(|e| format!("Network error: {}", e))?;
    let status = response.status();
    let text = response.text().map_err(|e| format!("Read error: {}", e))?;

    if !status.is_success() {
        return Err(format!("HTTP {} — {}", status, text));
    }

    serde_json::from_str(&text)
        .map_err(|e| format!("Parse error: {} — body: {}", e, &text[..200.min(text.len())]))
}
