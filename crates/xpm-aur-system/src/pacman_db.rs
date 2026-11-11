//! Read-only access to pacman's local and sync databases.
//!
//! xpm mirrors its transactions into pacman's database, so this is the
//! authoritative view of installed packages for both tools. Sync databases are
//! used only to resolve official packages; transactions always go through xpm.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use flate2::read::GzDecoder;
use tar::Archive;
use thiserror::Error;

use xpm_aur_core::catalog::{Candidate, CandidateSource, InstallReason};
use xpm_aur_core::dependency::Dependency;
use xpm_aur_core::error::CoreError;
use xpm_aur_core::package::PackageMeta;
use xpm_aur_core::version::Evr;

#[derive(Debug, Error)]
pub enum DbError {
    #[error("could not read {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("could not read gzip database {path}: {source}")]
    Gzip {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("could not read tar database {path}: {source}")]
    Tar {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("invalid ALPM metadata in {path}: {message}")]
    Parse { path: String, message: String },

    #[error("could not lock the sync database cache: {0}")]
    Cache(String),
}

impl From<DbError> for CoreError {
    fn from(err: DbError) -> Self {
        CoreError::Catalog(err.to_string())
    }
}

const DEFAULT_XPM_DB_PATH: &str = "/var/lib/xpm";
const MAX_INCLUDE_DEPTH: usize = 32;

/// A pacman installation rooted at `root`.
pub struct PacmanDatabase {
    root: PathBuf,
    dbpath: PathBuf,
    config: PathBuf,
    sync_cache: Mutex<Option<Vec<Candidate>>>,
}

impl Default for PacmanDatabase {
    fn default() -> Self {
        Self::new(Path::new("/"))
    }
}

impl PacmanDatabase {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            dbpath: root.join("var/lib/pacman"),
            config: root.join("etc/pacman.conf"),
            sync_cache: Mutex::new(None),
        }
    }

    pub fn with_paths(root: &Path, dbpath: &Path, config: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            dbpath: dbpath.to_path_buf(),
            config: config.to_path_buf(),
            sync_cache: Mutex::new(None),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn local_path(&self) -> PathBuf {
        self.dbpath.join("local")
    }

    pub fn xpm_config_path(&self) -> PathBuf {
        self.root.join("etc/xpm.conf")
    }

    /// xpm's database root, honouring `options.db_path` from `/etc/xpm.conf`.
    pub fn xpm_db_path(&self) -> PathBuf {
        let configured = std::fs::read_to_string(self.xpm_config_path())
            .ok()
            .and_then(|text| parse_xpm_db_path(&text))
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| PathBuf::from(DEFAULT_XPM_DB_PATH));
        self.path_under_root(&configured)
    }

    pub fn xpm_local_path(&self) -> PathBuf {
        self.xpm_db_path().join("local")
    }

    fn path_under_root(&self, path: &Path) -> PathBuf {
        match path.strip_prefix("/") {
            Ok(relative) => self.root.join(relative),
            Err(_) => self.root.join(path),
        }
    }

    /// Sync databases are read from xpm's own database when present. This is
    /// essential: `xpm sync` refreshes `/var/lib/xpm/sync`, not pacman's.
    pub fn sync_path(&self) -> PathBuf {
        let xpm_sync = self.xpm_db_path().join("sync");
        if contains_sync_db(&xpm_sync) {
            xpm_sync
        } else {
            self.dbpath.join("sync")
        }
    }

    /// Whether an xpm or pacman sync database can be read.
    pub fn has_sync(&self) -> bool {
        contains_sync_db(&self.xpm_db_path().join("sync"))
            || contains_sync_db(&self.dbpath.join("sync"))
    }

    pub fn config_path(&self) -> &Path {
        &self.config
    }

    /// Load all installed packages. There is no cache because xpm transactions
    /// can change this database between commands.
    pub fn installed(&self) -> Result<Vec<Candidate>, DbError> {
        let xpm_local = self.xpm_local_path();
        if contains_package_dirs(&xpm_local) {
            return installed_from_xpm(&xpm_local);
        }

        let local = self.local_path();
        let entries = match std::fs::read_dir(&local) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(DbError::Io {
                    path: local.display().to_string(),
                    source,
                });
            }
        };

        let mut packages = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| DbError::Io {
                path: local.display().to_string(),
                source,
            })?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let desc_path = path.join("desc");
            if !desc_path.is_file() {
                continue;
            }
            let desc = parse_desc(&desc_path)?;
            let mut candidate = candidate_from_desc(&desc, &desc_path, CandidateSource::Installed)?;
            candidate.reason = Some(match desc_first(&desc, "REASON").unwrap_or("0") {
                "1" => InstallReason::Dependency,
                _ => InstallReason::Explicit,
            });
            packages.push(candidate);
        }

        packages.sort_by(|a, b| a.meta.name.cmp(&b.meta.name));
        Ok(packages)
    }

    /// All sync candidates, loaded once per process.
    pub fn sync_packages(&self) -> Result<Vec<Candidate>, DbError> {
        let mut cache = self
            .sync_cache
            .lock()
            .map_err(|err| DbError::Cache(err.to_string()))?;
        if let Some(packages) = cache.as_ref() {
            return Ok(packages.clone());
        }

        let packages = self.load_sync()?;
        *cache = Some(packages.clone());
        Ok(packages)
    }

    /// Official packages matching a dependency, exact names before providers
    /// and repositories in pacman.conf order.
    pub fn sync_candidates(&self, dependency: &Dependency) -> Result<Vec<Candidate>, DbError> {
        let order = self.repo_order();
        let mut candidates: Vec<Candidate> = self
            .sync_packages()?
            .into_iter()
            .filter(|candidate| candidate.satisfies(dependency))
            .collect();

        candidates.sort_by(|a, b| {
            let a_exact = a.meta.name == dependency.name;
            let b_exact = b.meta.name == dependency.name;
            b_exact
                .cmp(&a_exact)
                .then_with(|| repo_rank(a, &order).cmp(&repo_rank(b, &order)))
                .then_with(|| a.meta.name.cmp(&b.meta.name))
        });
        Ok(candidates)
    }

    /// Search official sync packages by name or description.
    pub fn search_sync(&self, query: &str) -> Result<Vec<Candidate>, DbError> {
        let query = query.to_lowercase();
        let mut seen = std::collections::HashSet::new();
        let mut results: Vec<Candidate> = self
            .sync_packages()?
            .into_iter()
            .filter(|candidate| {
                candidate.meta.name.to_lowercase().contains(&query)
                    || candidate
                        .meta
                        .description
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(&query)
            })
            .filter(|candidate| seen.insert(candidate.meta.name.clone()))
            .collect();
        results.sort_by(|a, b| a.meta.name.cmp(&b.meta.name));
        Ok(results)
    }

    /// First exact package in repository priority order.
    pub fn sync_exact(&self, name: &str) -> Result<Option<Candidate>, DbError> {
        Ok(self
            .sync_packages()?
            .into_iter()
            .find(|candidate| candidate.meta.name == name))
    }

    /// All names present in the official sync databases.
    pub fn official_names(&self) -> Result<std::collections::HashSet<String>, DbError> {
        Ok(self
            .sync_packages()?
            .into_iter()
            .map(|candidate| candidate.meta.name)
            .collect())
    }

    fn load_sync(&self) -> Result<Vec<Candidate>, DbError> {
        let sync = self.sync_path();
        let entries = match std::fs::read_dir(&sync) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(DbError::Io {
                    path: sync.display().to_string(),
                    source,
                });
            }
        };

        let mut db_files: Vec<(String, PathBuf)> = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| DbError::Io {
                path: sync.display().to_string(),
                source,
            })?;
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !name.ends_with(".db") || name.ends_with(".files") {
                continue;
            }
            let repo = name.trim_end_matches(".db").to_string();
            db_files.push((repo, path));
        }

        let order = self.repo_order();
        db_files.sort_by_key(|(repo, _)| {
            order
                .iter()
                .position(|candidate| candidate == repo)
                .unwrap_or(usize::MAX)
        });

        let mut packages = Vec::new();
        for (repo, path) in db_files {
            packages.extend(read_sync_db(&path, &repo)?);
        }
        Ok(packages)
    }

    fn repo_order(&self) -> Vec<String> {
        let mut order = Vec::new();
        let mut visited = HashSet::new();
        collect_repo_order(&self.config, 0, &mut visited, &mut order);
        if order.is_empty() {
            order = vec!["core".into(), "extra".into(), "multilib".into(), "x".into()];
        }
        order
    }
}

fn installed_from_xpm(local: &Path) -> Result<Vec<Candidate>, DbError> {
    let entries = match std::fs::read_dir(local) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(DbError::Io {
                path: local.display().to_string(),
                source,
            });
        }
    };

    let mut packages = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| DbError::Io {
            path: local.display().to_string(),
            source,
        })?;
        let package_dir = entry.path();
        if !package_dir.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let version_path = package_dir.join("version");
        let Some(version) = read_optional_line(&version_path)? else {
            continue;
        };
        if version.is_empty() {
            continue;
        }
        let version = Evr::parse(&version).map_err(|err| DbError::Parse {
            path: version_path.display().to_string(),
            message: err.to_string(),
        })?;
        let depends = read_dependency_file(&package_dir.join("depends"))?;
        let provides = read_dependency_file(&package_dir.join("provides"))?;
        let _origin = read_optional_line(&package_dir.join("origin"))?;
        let reason = match read_optional_line(&package_dir.join("reason"))?.as_deref() {
            Some(reason) if reason.eq_ignore_ascii_case("dep") => InstallReason::Dependency,
            _ => InstallReason::Explicit,
        };
        let meta = PackageMeta {
            name: name.clone(),
            base: name,
            version,
            description: None,
            url: None,
            depends,
            make_depends: Vec::new(),
            check_depends: Vec::new(),
            opt_depends: Vec::new(),
            provides,
            conflicts: Vec::new(),
            replaces: Vec::new(),
        };
        let mut candidate = Candidate::new(meta, CandidateSource::Installed);
        candidate.reason = Some(reason);
        packages.push(candidate);
    }

    packages.sort_by(|a, b| a.meta.name.cmp(&b.meta.name));
    Ok(packages)
}

fn read_optional_line(path: &Path) -> Result<Option<String>, DbError> {
    match std::fs::read_to_string(path) {
        Ok(raw) => Ok(Some(raw.trim().to_string())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(DbError::Io {
            path: path.display().to_string(),
            source,
        }),
    }
}

fn read_dependency_file(path: &Path) -> Result<Vec<Dependency>, DbError> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(DbError::Io {
                path: path.display().to_string(),
                source,
            });
        }
    };
    raw.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            Dependency::parse(line).map_err(|err| DbError::Parse {
                path: path.display().to_string(),
                message: err.to_string(),
            })
        })
        .collect()
}

fn contains_package_dirs(path: &Path) -> bool {
    std::fs::read_dir(path)
        .map(|entries| entries.flatten().any(|entry| entry.path().is_dir()))
        .unwrap_or(false)
}

fn collect_repo_order(
    path: &Path,
    depth: usize,
    visited: &mut HashSet<PathBuf>,
    order: &mut Vec<String>,
) {
    if depth > MAX_INCLUDE_DEPTH {
        return;
    }
    let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if !visited.insert(key) {
        return;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            let name = line[1..line.len() - 1].trim();
            if !name.is_empty() && name != "options" {
                order.push(name.to_string());
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if !key.trim().eq_ignore_ascii_case("include") {
            continue;
        }
        let pattern = value.trim().trim_matches(|ch| ch == '"' || ch == '\'');
        if pattern.is_empty() {
            continue;
        }
        let pattern = if Path::new(pattern).is_absolute() {
            PathBuf::from(pattern)
        } else {
            path.parent()
                .unwrap_or_else(|| Path::new("."))
                .join(pattern)
        };
        let mut matches: Vec<PathBuf> = match glob::glob(&pattern.to_string_lossy()) {
            Ok(paths) => paths.filter_map(Result::ok).collect(),
            Err(_) => vec![pattern],
        };
        matches.sort();
        for included in matches {
            collect_repo_order(&included, depth + 1, visited, order);
        }
    }
}

fn parse_xpm_db_path(text: &str) -> Option<PathBuf> {
    let mut in_options = false;
    for raw in text.lines() {
        let line = strip_toml_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if let Some(section) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            in_options = section.trim() == "options";
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !((in_options && key == "db_path") || key == "options.db_path") {
            continue;
        }
        let value = toml_string(value.trim())?;
        return (!value.is_empty()).then(|| PathBuf::from(value));
    }
    None
}

fn strip_toml_comment(line: &str) -> &str {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (index, ch) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match quote {
            Some('"') if ch == '\\' => escaped = true,
            Some(open) if ch == open => quote = None,
            Some(_) => {}
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch == '#' => return &line[..index],
            None => {}
        }
    }
    line
}

fn toml_string(value: &str) -> Option<String> {
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            let inner = &value[1..value.len() - 1];
            return Some(if first == b'\'' {
                inner.to_string()
            } else {
                unescape_toml(inner)
            });
        }
    }
    if value.starts_with('"') || value.starts_with('\'') {
        return None;
    }
    Some(value.to_string())
}

fn unescape_toml(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn contains_sync_db(path: &Path) -> bool {
    std::fs::read_dir(path)
        .map(|entries| {
            entries.flatten().any(|entry| {
                entry
                    .path()
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(".db") && !name.ends_with(".files"))
            })
        })
        .unwrap_or(false)
}

fn decoder<'a>(bytes: &'a [u8], path: &Path) -> Result<Box<dyn Read + 'a>, DbError> {
    if bytes.starts_with(&[0x1f, 0x8b]) {
        return Ok(Box::new(GzDecoder::new(bytes)));
    }
    if bytes.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        return zstd::Decoder::new(bytes)
            .map(|decoder| Box::new(decoder) as Box<dyn Read>)
            .map_err(|source| DbError::Gzip {
                path: path.display().to_string(),
                source,
            });
    }
    if bytes.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0x00]) {
        return Ok(Box::new(xz2::read::XzDecoder::new(bytes)));
    }
    // Plain tar or an unrecognized stream: let tar report a useful error.
    Ok(Box::new(std::io::Cursor::new(bytes)))
}

fn repo_rank(candidate: &Candidate, order: &[String]) -> usize {
    match &candidate.source {
        CandidateSource::Official { repo, .. } => order
            .iter()
            .position(|name| name == repo)
            .unwrap_or(usize::MAX),
        _ => usize::MAX,
    }
}

fn read_sync_db(path: &Path, repo: &str) -> Result<Vec<Candidate>, DbError> {
    let bytes = std::fs::read(path).map_err(|source| DbError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let mut archive = Archive::new(decoder(&bytes, path)?);
    let entries = archive.entries().map_err(|source| DbError::Tar {
        path: path.display().to_string(),
        source,
    })?;

    let mut packages = Vec::new();
    for entry in entries {
        let mut entry = entry.map_err(|source| DbError::Tar {
            path: path.display().to_string(),
            source,
        })?;
        let entry_path = entry.path().map_err(|source| DbError::Tar {
            path: path.display().to_string(),
            source,
        })?;
        if entry_path.file_name().and_then(|name| name.to_str()) != Some("desc") {
            continue;
        }

        let mut text = String::new();
        entry
            .read_to_string(&mut text)
            .map_err(|source| DbError::Tar {
                path: path.display().to_string(),
                source,
            })?;
        let desc = parse_desc_text(&text, path)?;
        let filename = desc_first(&desc, "FILENAME")
            .unwrap_or_default()
            .to_string();
        let source = CandidateSource::Official {
            repo: repo.to_string(),
            filename,
        };
        packages.push(candidate_from_desc(&desc, path, source)?);
    }
    Ok(packages)
}

fn parse_desc(path: &Path) -> Result<HashMap<String, Vec<String>>, DbError> {
    let text = std::fs::read_to_string(path).map_err(|source| DbError::Io {
        path: path.display().to_string(),
        source,
    })?;
    parse_desc_text(&text, path)
}

/// Parse pacman's `%KEY%` / value blocks.
fn parse_desc_text(text: &str, path: &Path) -> Result<HashMap<String, Vec<String>>, DbError> {
    let mut result: HashMap<String, Vec<String>> = HashMap::new();
    let mut key: Option<String> = None;

    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            key = None;
            continue;
        }
        if let Some(name) = line
            .strip_prefix('%')
            .and_then(|line| line.strip_suffix('%'))
        {
            key = Some(name.to_string());
            result.entry(name.to_string()).or_default();
        } else if let Some(key) = &key {
            result
                .get_mut(key)
                .expect("key inserted above")
                .push(line.to_string());
        } else {
            return Err(DbError::Parse {
                path: path.display().to_string(),
                message: "value outside a %KEY% block".into(),
            });
        }
    }

    Ok(result)
}

fn candidate_from_desc(
    desc: &HashMap<String, Vec<String>>,
    path: &Path,
    source: CandidateSource,
) -> Result<Candidate, DbError> {
    let name = required(desc, "NAME", path)?;
    let version = required(desc, "VERSION", path)?;
    let base = desc_first(desc, "BASE")
        .unwrap_or(name.as_str())
        .to_string();
    let meta = PackageMeta {
        name,
        base,
        version: Evr::parse(&version).map_err(|err| DbError::Parse {
            path: path.display().to_string(),
            message: err.to_string(),
        })?,
        description: desc_first(desc, "DESC").map(ToString::to_string),
        url: desc_first(desc, "URL").map(ToString::to_string),
        depends: parse_deps(desc, "DEPENDS", path)?,
        make_depends: parse_deps(desc, "MAKEDEPENDS", path)?,
        check_depends: parse_deps(desc, "CHECKDEPENDS", path)?,
        opt_depends: parse_optional_deps(desc, "OPTDEPENDS", path)?,
        provides: parse_deps(desc, "PROVIDES", path)?,
        conflicts: parse_deps(desc, "CONFLICTS", path)?,
        replaces: parse_deps(desc, "REPLACES", path)?,
    };

    Ok(Candidate::new(meta, source))
}

fn required(
    desc: &HashMap<String, Vec<String>>,
    key: &str,
    path: &Path,
) -> Result<String, DbError> {
    desc_first(desc, key)
        .map(ToString::to_string)
        .ok_or_else(|| DbError::Parse {
            path: path.display().to_string(),
            message: format!("missing %{key}%"),
        })
}

fn desc_first<'a>(desc: &'a HashMap<String, Vec<String>>, key: &str) -> Option<&'a str> {
    desc.get(key)
        .and_then(|values| values.first())
        .map(String::as_str)
}

fn parse_deps(
    desc: &HashMap<String, Vec<String>>,
    key: &str,
    path: &Path,
) -> Result<Vec<Dependency>, DbError> {
    desc.get(key)
        .into_iter()
        .flatten()
        .map(|raw| {
            Dependency::parse(raw).map_err(|err| DbError::Parse {
                path: path.display().to_string(),
                message: err.to_string(),
            })
        })
        .collect()
}

fn parse_optional_deps(
    desc: &HashMap<String, Vec<String>>,
    key: &str,
    path: &Path,
) -> Result<Vec<Dependency>, DbError> {
    desc.get(key)
        .into_iter()
        .flatten()
        .map(|raw| {
            let spec = raw.split_once(": ").map_or(raw.as_str(), |(spec, _)| spec);
            Dependency::parse(spec).map_err(|err| DbError::Parse {
                path: path.display().to_string(),
                message: err.to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;

    use flate2::Compression;
    use flate2::write::GzEncoder;
    use tar::{Builder, Header};

    fn temp_root() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "xpm-aur-db-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn write_file(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn write_pacman_entry(root: &Path, name: &str, version: &str) {
        write_file(
            &root.join(format!("var/lib/pacman/local/{name}-{version}/desc")),
            &format!("%NAME%\n{name}\n\n%VERSION%\n{version}\n\n%REASON%\n0\n"),
        );
    }

    fn write_xpm_entry(db: &Path, name: &str, fields: &[(&str, &str)]) {
        let dir = db.join("local").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        for (file, contents) in fields {
            std::fs::write(dir.join(file), contents).unwrap();
        }
    }

    #[test]
    fn prefers_xpm_local_database_and_parses_its_format() {
        let root = temp_root();
        write_pacman_entry(&root, "pacman-only", "1.0-1");
        write_xpm_entry(
            &root.join("var/lib/xpm"),
            "foo",
            &[
                ("version", "2.0-1\n"),
                ("depends", "glibc>=2\nlibfoo\n"),
                ("provides", "foo-virtual=2\n"),
                ("reason", "dep\n"),
                ("origin", "x\n"),
            ],
        );

        let db = PacmanDatabase::new(&root);
        let installed = db.installed().unwrap();
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].meta.name, "foo");
        assert_eq!(installed[0].meta.version.to_string(), "2.0-1");
        assert_eq!(installed[0].reason, Some(InstallReason::Dependency));
        assert!(
            installed[0]
                .meta
                .depends
                .iter()
                .any(|dependency| dependency.name == "glibc")
        );
        assert!(
            installed[0]
                .meta
                .provides
                .iter()
                .any(|dependency| dependency.name == "foo-virtual")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn falls_back_to_pacman_local_database() {
        let root = temp_root();
        write_pacman_entry(&root, "bar", "3.1-2");

        let db = PacmanDatabase::new(&root);
        let installed = db.installed().unwrap();
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].meta.name, "bar");
        assert_eq!(installed[0].meta.version.to_string(), "3.1-2");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn honours_xpm_db_path_override() {
        let root = temp_root();
        write_file(
            &root.join("etc/xpm.conf"),
            "[options]\nmirror_pacman = true\ndb_path = \"/srv/xpm-db\" # custom\n",
        );
        write_xpm_entry(
            &root.join("srv/xpm-db"),
            "custom",
            &[("version", "1.2-3\n")],
        );
        write_xpm_entry(
            &root.join("var/lib/xpm"),
            "default-db",
            &[("version", "9.9-9\n")],
        );
        write_file(&root.join("srv/xpm-db/sync/core.db"), "");

        let db = PacmanDatabase::new(&root);
        assert_eq!(db.xpm_db_path(), root.join("srv/xpm-db"));
        assert_eq!(db.xpm_local_path(), root.join("srv/xpm-db/local"));
        assert_eq!(db.sync_path(), root.join("srv/xpm-db/sync"));
        assert!(db.has_sync());
        let installed = db.installed().unwrap();
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].meta.name, "custom");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parses_targeted_xpm_conf() {
        assert_eq!(
            parse_xpm_db_path("[options]\ndb_path = \"/srv/xpm\"\n"),
            Some(PathBuf::from("/srv/xpm"))
        );
        assert_eq!(
            parse_xpm_db_path("[options]\ndb_path = '/srv/xpm' # comment\n"),
            Some(PathBuf::from("/srv/xpm"))
        );
        assert_eq!(
            parse_xpm_db_path("options.db_path = \"/srv/xpm\"\n"),
            Some(PathBuf::from("/srv/xpm"))
        );
        assert_eq!(
            parse_xpm_db_path("[options]\nmirror_pacman = false\n"),
            None
        );
        assert_eq!(parse_xpm_db_path("[repo]\ndb_path = \"/ignored\"\n"), None);
        assert_eq!(parse_xpm_db_path(""), None);
    }

    #[test]
    fn expands_nested_and_globbed_pacman_includes() {
        let root = temp_root();
        let conf_dir = root.join("etc/pacman.d");
        write_file(
            &root.join("etc/pacman.conf"),
            &format!(
                "[options]\nHoldPkg = pacman\n\n[core]\nInclude = {}/repos/*.conf\n",
                conf_dir.display()
            ),
        );
        write_file(
            &conf_dir.join("repos/a.conf"),
            "[extra]\nInclude = nested.conf\n",
        );
        write_file(&conf_dir.join("repos/b.conf"), "[x-custom]\n");
        write_file(&conf_dir.join("repos/nested.conf"), "[nested]\n");

        let db = PacmanDatabase::new(&root);
        assert_eq!(db.repo_order(), vec!["core", "extra", "nested", "x-custom"]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn include_cycles_and_missing_files_terminate() {
        let root = temp_root();
        write_file(
            &root.join("etc/pacman.conf"),
            "[core]\nInclude = pacman.conf\nInclude = /does/not/exist.conf\n",
        );
        let db = PacmanDatabase::new(&root);
        assert_eq!(db.repo_order(), vec!["core"]);
        std::fs::remove_dir_all(root).unwrap();

        let root = temp_root();
        let db = PacmanDatabase::new(&root);
        assert_eq!(
            db.repo_order(),
            vec!["core", "extra", "multilib", "x"],
            "missing pacman.conf uses the built-in order"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn has_sync_detects_xpm_and_pacman_databases() {
        let root = temp_root();
        let db = PacmanDatabase::new(&root);
        assert!(!db.has_sync());

        write_file(&root.join("var/lib/pacman/sync/core.db"), "");
        assert!(db.has_sync());
        assert_eq!(db.sync_path(), root.join("var/lib/pacman/sync"));
        std::fs::remove_dir_all(root).unwrap();

        let root = temp_root();
        write_file(&root.join("var/lib/xpm/sync/core.db"), "");
        let db = PacmanDatabase::new(&root);
        assert!(db.has_sync());
        assert_eq!(db.sync_path(), root.join("var/lib/xpm/sync"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reads_local_and_sync_databases() {
        let root = temp_root();
        let local = root.join("var/lib/pacman/local/foo-1.0-1");
        std::fs::create_dir_all(&local).unwrap();
        std::fs::write(
            local.join("desc"),
            "%NAME%\nfoo\n\n%VERSION%\n1.0-1\n\n%REASON%\n0\n\n%DEPENDS%\nbar>=1.0\n",
        )
        .unwrap();

        let sync = root.join("var/lib/pacman/sync");
        std::fs::create_dir_all(&sync).unwrap();
        let db = sync.join("core.db");
        let file = File::create(&db).unwrap();
        let encoder = GzEncoder::new(file, Compression::default());
        let mut builder = Builder::new(encoder);
        let desc = "%NAME%\nbar\n\n%VERSION%\n2.0-1\n\n%FILENAME%\nbar-2.0-1-x86_64.pkg.tar.zst\n\n%PROVIDES%\nbar-virtual=2\n";
        let mut header = Header::new_gnu();
        header.set_size(desc.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "bar-2.0-1/desc", desc.as_bytes())
            .unwrap();
        builder.finish().unwrap();
        let encoder = builder.into_inner().unwrap();
        encoder.finish().unwrap();

        let db = PacmanDatabase::new(&root);
        let installed = db.installed().unwrap();
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].meta.name, "foo");

        let candidates = db
            .sync_candidates(&Dependency::parse("bar-virtual>=2").unwrap())
            .unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].meta.name, "bar");
        assert!(matches!(
            candidates[0].source,
            CandidateSource::Official { ref repo, .. } if repo == "core"
        ));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn prefers_xpm_sync_database_and_reads_zstd() {
        let root = temp_root();
        let sync = root.join("var/lib/xpm/sync");
        std::fs::create_dir_all(&sync).unwrap();

        let desc = "%NAME%\nxpm-only\n\n%VERSION%\n3.0-1\n\n%FILENAME%\nxpm-only-3.0-1-x86_64.pkg.tar.zst\n";
        let mut tar_bytes = Vec::new();
        {
            let mut builder = Builder::new(&mut tar_bytes);
            let mut header = Header::new_gnu();
            header.set_size(desc.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "xpm-only-3.0-1/desc", desc.as_bytes())
                .unwrap();
            builder.finish().unwrap();
        }
        let compressed = zstd::encode_all(std::io::Cursor::new(&tar_bytes), 3).unwrap();
        std::fs::write(sync.join("core.db"), compressed).unwrap();

        let db = PacmanDatabase::new(&root);
        let candidates = db
            .sync_candidates(&Dependency::parse("xpm-only").unwrap())
            .unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].meta.name, "xpm-only");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_databases_are_empty() {
        let root = temp_root();
        let db = PacmanDatabase::new(&root);
        assert!(db.installed().unwrap().is_empty());
        assert!(db.sync_packages().unwrap().is_empty());
        assert!(!db.has_sync());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parses_blocks_until_blank_line() {
        let text = "%NAME%\nfoo\n\n%VERSION%\n1.0-1\n";
        let parsed = parse_desc_text(text, Path::new("desc")).unwrap();
        assert_eq!(parsed["NAME"], vec!["foo"]);
        assert_eq!(parsed["VERSION"], vec!["1.0-1"]);
    }

    #[test]
    fn write_is_kept_for_clippy() {
        let mut sink = Vec::new();
        sink.write_all(b"").unwrap();
    }
}
