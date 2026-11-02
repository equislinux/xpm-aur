//! Read-only access to pacman's local and sync databases.
//!
//! xpm mirrors its transactions into pacman's database, so this is the
//! authoritative view of installed packages for both tools. Sync databases are
//! used only to resolve official packages; transactions always go through xpm.

use std::collections::HashMap;
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

    /// Sync databases are read from xpm's own database when present. This is
    /// essential: `xpm sync` refreshes `/var/lib/xpm/sync`, not pacman's.
    pub fn sync_path(&self) -> PathBuf {
        let xpm_sync = self.root.join("var/lib/xpm/sync");
        if contains_sync_db(&xpm_sync) {
            xpm_sync
        } else {
            self.dbpath.join("sync")
        }
    }

    pub fn config_path(&self) -> &Path {
        &self.config
    }

    /// Load all installed packages. There is no cache because xpm transactions
    /// can change this database between commands.
    pub fn installed(&self) -> Result<Vec<Candidate>, DbError> {
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
        if let Ok(text) = std::fs::read_to_string(&self.config) {
            for line in text.lines() {
                let line = line.split('#').next().unwrap_or("").trim();
                if let Some(name) = line
                    .strip_prefix('[')
                    .and_then(|line| line.strip_suffix(']'))
                {
                    let name = name.trim();
                    if name != "options" && !name.is_empty() {
                        order.push(name.to_string());
                    }
                }
            }
        }
        if order.is_empty() {
            order = vec!["core".into(), "extra".into(), "multilib".into(), "x".into()];
        }
        order
    }
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
