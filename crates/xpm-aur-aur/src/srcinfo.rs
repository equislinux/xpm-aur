//! Minimal but strict `.SRCINFO` parser.
//!
//! RPC metadata is used for resolution, but `.SRCINFO` is useful to validate a
//! checkout, enumerate split packages and detect a stale AUR clone.

use std::collections::BTreeMap;
use std::path::Path;

use xpm_aur_core::dependency::Dependency;
use xpm_aur_core::version::Evr;

use crate::error::{AurError, AurResult};

/// Parsed `.SRCINFO` for one AUR package base.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SrcInfo {
    pub pkgbase: String,
    pub packages: BTreeMap<String, SrcPackage>,
}

/// One `pkgname` section.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SrcPackage {
    pub name: String,
    pub version: Option<Evr>,
    pub description: Option<String>,
    pub depends: Vec<Dependency>,
    pub make_depends: Vec<Dependency>,
    pub check_depends: Vec<Dependency>,
    pub opt_depends: Vec<Dependency>,
    pub provides: Vec<Dependency>,
    pub conflicts: Vec<Dependency>,
    pub replaces: Vec<Dependency>,
}

impl SrcInfo {
    pub fn package_names(&self) -> impl Iterator<Item = &str> {
        self.packages.keys().map(String::as_str)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.packages.contains_key(name)
    }
}

/// Read and parse `<dir>/.SRCINFO`.
pub fn read_srcinfo(dir: &Path) -> AurResult<SrcInfo> {
    let path = dir.join(".SRCINFO");
    let text = std::fs::read_to_string(&path).map_err(|source| AurError::Io {
        path: path.display().to_string(),
        source,
    })?;
    parse_srcinfo(&text)
}

/// Parse the text format emitted by `makepkg --printsrcinfo`.
pub fn parse_srcinfo(text: &str) -> AurResult<SrcInfo> {
    parse_srcinfo_for_arch(text, current_arch())
}

/// Parse `.SRCINFO` merging fields for `arch` and ignoring other architectures.
pub fn parse_srcinfo_for_arch(text: &str, arch: &str) -> AurResult<SrcInfo> {
    let mut pkgbase = String::new();
    let mut packages: BTreeMap<String, SrcPackage> = BTreeMap::new();
    let mut current: Option<String> = None;

    let mut global = SrcPackage::default();
    let mut epoch = 0_u64;
    let mut pkgver: Option<String> = None;
    let mut pkgrel: Option<String> = None;

    for (line_number, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(AurError::SrcInfo(format!(
                "line {}: expected key = value",
                line_number + 1
            )));
        };
        let key = key.trim();
        let value = value.trim();
        let (key, skip) = split_arch_suffix(key, arch);
        if skip {
            continue;
        }

        match key {
            "pkgbase" => pkgbase = value.to_string(),
            "pkgname" => {
                let package = SrcPackage {
                    name: value.to_string(),
                    ..SrcPackage::default()
                };
                packages.insert(value.to_string(), package);
                current = Some(value.to_string());
            }
            "epoch" => {
                epoch = value
                    .parse()
                    .map_err(|_| AurError::SrcInfo(format!("invalid epoch: {value}")))?;
            }
            "pkgver" => pkgver = Some(value.to_string()),
            "pkgrel" => pkgrel = Some(value.to_string()),
            "pkgdesc" => {
                target(&mut packages, current.as_deref(), &mut global).description =
                    Some(value.to_string())
            }
            "depends" => push_dep(&mut packages, current.as_deref(), &mut global, key, value)?,
            "makedepends" => push_dep(&mut packages, current.as_deref(), &mut global, key, value)?,
            "checkdepends" => push_dep(&mut packages, current.as_deref(), &mut global, key, value)?,
            "optdepends" => push_dep(&mut packages, current.as_deref(), &mut global, key, value)?,
            "provides" => push_dep(&mut packages, current.as_deref(), &mut global, key, value)?,
            "conflicts" => push_dep(&mut packages, current.as_deref(), &mut global, key, value)?,
            "replaces" => push_dep(&mut packages, current.as_deref(), &mut global, key, value)?,
            _ => {}
        }
    }

    if pkgbase.is_empty() {
        return Err(AurError::SrcInfo("missing pkgbase".into()));
    }
    if packages.is_empty() {
        return Err(AurError::SrcInfo("missing pkgname".into()));
    }

    let version = match pkgver {
        Some(pkgver) => {
            let raw = if epoch == 0 {
                match &pkgrel {
                    Some(pkgrel) => format!("{pkgver}-{pkgrel}"),
                    None => pkgver,
                }
            } else {
                match &pkgrel {
                    Some(pkgrel) => format!("{epoch}:{pkgver}-{pkgrel}"),
                    None => format!("{epoch}:{pkgver}"),
                }
            };
            Some(Evr::parse(&raw).map_err(|err| AurError::SrcInfo(err.to_string()))?)
        }
        None => None,
    };

    for package in packages.values_mut() {
        package.version = version.clone();
        merge_global(package, &global);
    }

    Ok(SrcInfo { pkgbase, packages })
}

const KNOWN_ARCHES: &[&str] = &[
    "any", "x86_64", "i686", "aarch64", "armv7h", "armv6h", "riscv64", "loong64", "ppc64le",
    "s390x", "amd64", "arm64",
];

fn current_arch() -> &'static str {
    match std::env::consts::ARCH {
        "amd64" => "x86_64",
        "arm64" => "aarch64",
        arch => arch,
    }
}

fn strip_arch_suffix<'a>(key: &'a str, arch: &str) -> Option<&'a str> {
    key.strip_suffix(arch)?.strip_suffix('_')
}

fn split_arch_suffix<'a>(key: &'a str, arch: &str) -> (&'a str, bool) {
    if let Some(base) = strip_arch_suffix(key, arch) {
        return (base, false);
    }
    if let Some(base) = strip_arch_suffix(key, "any") {
        return (base, false);
    }
    if KNOWN_ARCHES
        .iter()
        .any(|known| strip_arch_suffix(key, known).is_some())
    {
        return (key, true);
    }
    (key, false)
}

fn target<'a>(
    packages: &'a mut BTreeMap<String, SrcPackage>,
    current: Option<&str>,
    global: &'a mut SrcPackage,
) -> &'a mut SrcPackage {
    match current {
        Some(name) => packages.get_mut(name).expect("current package exists"),
        None => global,
    }
}

fn push_dep(
    packages: &mut BTreeMap<String, SrcPackage>,
    current: Option<&str>,
    global: &mut SrcPackage,
    key: &str,
    value: &str,
) -> AurResult<()> {
    let spec = if key == "optdepends" {
        value.split_once(": ").map_or(value, |(spec, _)| spec)
    } else {
        value
    };
    let dependency = Dependency::parse(spec)
        .map_err(|err| AurError::SrcInfo(format!("invalid {key} entry {value:?}: {err}")))?;
    let target = target(packages, current, global);
    match key {
        "depends" => target.depends.push(dependency),
        "makedepends" => target.make_depends.push(dependency),
        "checkdepends" => target.check_depends.push(dependency),
        "optdepends" => target.opt_depends.push(dependency),
        "provides" => target.provides.push(dependency),
        "conflicts" => target.conflicts.push(dependency),
        "replaces" => target.replaces.push(dependency),
        _ => unreachable!("push_dep only called for dependency keys"),
    }
    Ok(())
}

fn merge_global(package: &mut SrcPackage, global: &SrcPackage) {
    if package.description.is_none() {
        package.description.clone_from(&global.description);
    }
    for (target, source) in [
        (&mut package.depends, &global.depends),
        (&mut package.make_depends, &global.make_depends),
        (&mut package.check_depends, &global.check_depends),
        (&mut package.opt_depends, &global.opt_depends),
        (&mut package.provides, &global.provides),
        (&mut package.conflicts, &global.conflicts),
        (&mut package.replaces, &global.replaces),
    ] {
        for dependency in source {
            if !target.contains(dependency) {
                target.push(dependency.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPLIT: &str = r#"
# Generated by makepkg 7.1.0
pkgbase = demo
    pkgdesc = demo package base
    pkgver = 1.2.3
    pkgrel = 4
    epoch = 1
    makedepends = git
    depends_x86_64 = glibc>=2.39
    depends_aarch64 = ignored

pkgname = demo
    depends = libfoo
    optdepends = python: bindings
    provides = demo-virtual=1.2.3
    conflicts = demo-old

pkgname = demo-docs
    depends = demo
"#;

    #[test]
    fn parses_split_packages_and_arch_specific_fields() {
        let info = parse_srcinfo_for_arch(SPLIT, "x86_64").unwrap();
        assert_eq!(info.pkgbase, "demo");
        assert_eq!(
            info.package_names().collect::<Vec<_>>(),
            vec!["demo", "demo-docs"]
        );
        let demo = &info.packages["demo"];
        assert_eq!(demo.version.as_ref().unwrap().to_string(), "1:1.2.3-4");
        assert!(demo.depends.iter().any(|d| d.name == "glibc"));
        assert!(!demo.depends.iter().any(|d| d.name == "ignored"));
        assert!(demo.depends.iter().any(|d| d.name == "libfoo"));
        assert_eq!(demo.opt_depends[0].name, "python");
        assert!(demo.provides.iter().any(|d| d.name == "demo-virtual"));
        assert!(demo.conflicts.iter().any(|d| d.name == "demo-old"));
        assert!(demo.make_depends.iter().any(|d| d.name == "git"));
        assert!(
            info.packages["demo-docs"]
                .depends
                .iter()
                .any(|d| d.name == "demo")
        );
    }

    #[test]
    fn ignores_other_architectures_and_merges_the_selected_one() {
        let info = parse_srcinfo_for_arch(SPLIT, "aarch64").unwrap();
        let demo = &info.packages["demo"];
        assert!(demo.depends.iter().any(|d| d.name == "ignored"));
        assert!(!demo.depends.iter().any(|d| d.name == "glibc"));
    }

    #[test]
    fn any_architecture_fields_are_merged_everywhere() {
        let text = r#"
pkgbase = demo
    pkgver = 1.0
    pkgrel = 1
    depends_any = universal
    depends_x86_64 = only-x86
    depends_aarch64 = only-arm

pkgname = demo
"#;
        let x86 = parse_srcinfo_for_arch(text, "x86_64").unwrap();
        let names: Vec<&str> = x86.packages["demo"]
            .depends
            .iter()
            .map(|d| d.name.as_str())
            .collect();
        assert_eq!(names, vec!["universal", "only-x86"]);

        let arm = parse_srcinfo_for_arch(text, "aarch64").unwrap();
        let names: Vec<&str> = arm.packages["demo"]
            .depends
            .iter()
            .map(|d| d.name.as_str())
            .collect();
        assert_eq!(names, vec!["universal", "only-arm"]);
    }
}
