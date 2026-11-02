//! AUR RPC v5 client.
//!
//! Only the endpoints needed by xpm-aur are implemented: `info`, `search` and
//! the `provides` search used to satisfy virtual dependencies.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use xpm_aur_core::catalog::{Candidate, CandidateSource};
use xpm_aur_core::dependency::Dependency;
use xpm_aur_core::package::PackageMeta;
use xpm_aur_core::version::Evr;

use crate::error::{AurError, AurResult};

const DEFAULT_RPC: &str = "https://aur.archlinux.org/rpc/v5";
const CHUNK: usize = 100;

/// Search mode exposed by the AUR RPC.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchBy {
    NameDesc,
    Name,
    Provides,
    Depends,
    MakeDepends,
    CheckDepends,
    OptDepends,
    Conflicts,
    Replaces,
    Groups,
    Keywords,
    Maintainer,
    Comaintainers,
}

impl SearchBy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NameDesc => "name-desc",
            Self::Name => "name",
            Self::Provides => "provides",
            Self::Depends => "depends",
            Self::MakeDepends => "makedepends",
            Self::CheckDepends => "checkdepends",
            Self::OptDepends => "optdepends",
            Self::Conflicts => "conflicts",
            Self::Replaces => "replaces",
            Self::Groups => "groups",
            Self::Keywords => "keywords",
            Self::Maintainer => "maintainer",
            Self::Comaintainers => "comaintainers",
        }
    }
}

/// A package record returned by AUR RPC v5.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AurPackage {
    #[serde(rename = "ID", default)]
    pub id: u64,
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "PackageBaseID", default)]
    pub package_base_id: u64,
    #[serde(rename = "PackageBase")]
    pub package_base: String,
    #[serde(rename = "Version")]
    pub version: String,
    #[serde(rename = "Description", default)]
    pub description: Option<String>,
    #[serde(rename = "URL", default)]
    pub url: Option<String>,
    #[serde(rename = "NumVotes", default)]
    pub votes: u64,
    #[serde(rename = "Popularity", default)]
    pub popularity: f64,
    #[serde(rename = "OutOfDate", default)]
    pub out_of_date: Option<i64>,
    #[serde(rename = "Maintainer", default)]
    pub maintainer: Option<String>,
    #[serde(rename = "FirstSubmitted", default)]
    pub first_submitted: i64,
    #[serde(rename = "LastModified", default)]
    pub last_modified: i64,
    #[serde(rename = "URLPath", default)]
    pub url_path: Option<String>,
    #[serde(rename = "Depends", default)]
    pub depends: Vec<String>,
    #[serde(rename = "MakeDepends", default)]
    pub make_depends: Vec<String>,
    #[serde(rename = "CheckDepends", default)]
    pub check_depends: Vec<String>,
    #[serde(rename = "OptDepends", default)]
    pub opt_depends: Vec<String>,
    #[serde(rename = "Provides", default)]
    pub provides: Vec<String>,
    #[serde(rename = "Conflicts", default)]
    pub conflicts: Vec<String>,
    #[serde(rename = "Replaces", default)]
    pub replaces: Vec<String>,
    #[serde(rename = "Groups", default)]
    pub groups: Vec<String>,
    #[serde(rename = "License", default)]
    pub license: Vec<String>,
    #[serde(rename = "Keywords", default)]
    pub keywords: Vec<String>,
}

impl AurPackage {
    /// Convert the RPC record into the source-agnostic resolver metadata.
    pub fn into_candidate(self) -> AurResult<Candidate> {
        let meta = PackageMeta {
            name: self.name,
            base: self.package_base,
            version: Evr::parse(&self.version)
                .map_err(|err| AurError::Api(format!("invalid AUR version: {err}")))?,
            description: self.description,
            url: self.url,
            depends: parse_dependencies(&self.depends)?,
            make_depends: parse_dependencies(&self.make_depends)?,
            check_depends: parse_dependencies(&self.check_depends)?,
            opt_depends: parse_optional_dependencies(&self.opt_depends)?,
            provides: parse_dependencies(&self.provides)?,
            conflicts: parse_dependencies(&self.conflicts)?,
            replaces: parse_dependencies(&self.replaces)?,
        };
        let mut candidate = Candidate::new(meta, CandidateSource::Aur);
        candidate.votes = self.votes;
        candidate.popularity = (self.popularity.max(0.0) * 1000.0) as u64;
        candidate.out_of_date = self.out_of_date.is_some();
        Ok(candidate)
    }
}

#[derive(Debug, Deserialize)]
struct RpcResponse {
    #[serde(rename = "resultcount", default)]
    result_count: usize,
    #[serde(default)]
    results: Vec<AurPackage>,
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// Blocking AUR RPC client.
pub struct AurRpc {
    base_url: String,
    agent: ureq::Agent,
}

impl AurRpc {
    pub fn new(base_url: Option<String>) -> Self {
        let base_url = base_url
            .unwrap_or_else(|| DEFAULT_RPC.to_string())
            .trim_end_matches('/')
            .to_string();
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("xpm-aur/", env!("CARGO_PKG_VERSION")))
            .build();
        Self { base_url, agent }
    }

    /// Fetch metadata for up to any number of package names. Requests are
    /// chunked to keep URLs bounded.
    pub fn info(&self, names: &[String]) -> AurResult<Vec<AurPackage>> {
        let mut out = Vec::new();
        for chunk in names.chunks(CHUNK) {
            let mut url = format!("{}/info?", self.base_url);
            for (index, name) in chunk.iter().enumerate() {
                if index > 0 {
                    url.push('&');
                }
                url.push_str("arg[]=");
                url.push_str(&urlencoding::encode(name));
            }
            let response = self.call(&url)?;
            out.extend(response.results);
        }
        Ok(out)
    }

    pub fn search(&self, query: &str, by: SearchBy) -> AurResult<Vec<AurPackage>> {
        let url = format!(
            "{}/search?by={}&arg={}",
            self.base_url,
            by.as_str(),
            urlencoding::encode(query)
        );
        Ok(self.call(&url)?.results)
    }

    /// Return AUR packages that provide `name`.
    pub fn providers(&self, name: &str) -> AurResult<Vec<AurPackage>> {
        let packages = self.search(name, SearchBy::Provides)?;
        Ok(packages
            .into_iter()
            .filter(|package| {
                package.provides.iter().any(|provide| {
                    provide.split(['<', '>', '=']).next().unwrap_or("").trim() == name
                })
            })
            .collect())
    }

    fn call(&self, url: &str) -> AurResult<RpcResponse> {
        let response = self.agent.get(url).call().map_err(|err| match err {
            ureq::Error::Status(code, response) => {
                AurError::Network(format!("HTTP {code}: {}", response.status_text()))
            }
            ureq::Error::Transport(err) => AurError::Network(err.to_string()),
        })?;
        let response: RpcResponse = response
            .into_json()
            .map_err(|err| AurError::Network(err.to_string()))?;
        if let Some(error) = response.error {
            return Err(AurError::Api(error));
        }
        if response.kind.as_deref() == Some("error") {
            return Err(AurError::Api("AUR returned type=error".into()));
        }
        if response.result_count > 0 && response.results.is_empty() {
            return Err(AurError::Api(
                "AUR response claimed results but returned none".into(),
            ));
        }
        Ok(response)
    }
}

fn parse_dependencies(raw: &[String]) -> AurResult<Vec<Dependency>> {
    raw.iter()
        .map(|dependency| Dependency::parse(dependency))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| AurError::Api(format!("invalid AUR dependency: {err}")))
}

/// Optional dependencies may contain a human-readable suffix after a colon,
/// e.g. `python: for the optional Python bindings`.
fn parse_optional_dependencies(raw: &[String]) -> AurResult<Vec<Dependency>> {
    raw.iter()
        .map(|dependency| {
            let spec = dependency
                .split_once(": ")
                .map_or(dependency.as_str(), |(spec, _)| spec);
            Dependency::parse(spec)
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| AurError::Api(format!("invalid AUR optional dependency: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_rpc_record_to_candidate() {
        let raw = r#"{
            "ID": 1, "Name": "demo", "PackageBaseID": 2, "PackageBase": "demo-base",
            "Version": "1.2.3-1", "Description": "demo", "URL": "https://example.invalid",
            "NumVotes": 42, "Popularity": 1.5, "OutOfDate": null, "Maintainer": "someone",
            "FirstSubmitted": 1, "LastModified": 2, "URLPath": "/cgit/aur.git/snapshot/demo.tar.gz",
            "Depends": ["glibc>=2.39"], "MakeDepends": ["git"],
            "CheckDepends": ["bats"], "OptDepends": ["python: bindings"],
            "Provides": ["demo-virtual=1"], "Conflicts": ["demo-old"],
            "Replaces": [], "Groups": [], "License": ["MIT"], "Keywords": []
        }"#;
        let package: AurPackage = serde_json::from_str(raw).unwrap();
        let candidate = package.into_candidate().unwrap();
        assert_eq!(candidate.meta.name, "demo");
        assert_eq!(candidate.meta.base, "demo-base");
        assert_eq!(candidate.meta.version.to_string(), "1.2.3-1");
        assert_eq!(candidate.votes, 42);
        assert_eq!(candidate.popularity, 1500);
        assert_eq!(candidate.meta.opt_depends[0].name, "python");
        assert!(
            candidate
                .meta
                .satisfies(&Dependency::parse("demo-virtual>=1").unwrap())
        );
    }

    #[test]
    fn handles_aur_error_payloads() {
        let raw = r#"{"resultcount":0,"results":[],"type":"error","error":"Incorrect request type specified."}"#;
        let response: RpcResponse = serde_json::from_str(raw).unwrap();
        assert!(response.error.is_some());
    }
}
