//! Dependency resolution and build planning.
//!
//! The algorithm is intentionally conservative:
//! 1. installed packages satisfy dependencies when their version does;
//! 2. official repositories are preferred over AUR, matching pacman/yay;
//! 3. official dependencies are installed by xpm as one batch;
//! 4. AUR packages are grouped by package base, topologically sorted and built
//!    one base at a time so build dependencies are available.

use std::collections::HashSet;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::catalog::{Candidate, CandidateSource, InstallReason, PackageCatalog};
use crate::dependency::Dependency;
use crate::error::{CoreError, CoreResult};
use crate::plan::{AurBuild, AurPackagePlan, InstallPlan, OfficialAction, RemovalAction};

/// Knobs that change what the resolver is allowed to select.
#[derive(Clone, Debug)]
pub struct ResolveOptions {
    pub include_make_depends: bool,
    pub include_check_depends: bool,
    pub force_aur: bool,
    pub force_repo: bool,
    pub rebuild: bool,
    pub remove_conflicts: bool,
    /// Mark requested targets as dependencies (`--asdeps`).
    pub target_asdeps: bool,
    /// Plan for a clean chroot: an installed foreign build dependency must be
    /// rebuilt so its artifact can be installed in the disposable root.
    pub chroot: bool,
}

impl Default for ResolveOptions {
    fn default() -> Self {
        Self {
            include_make_depends: true,
            include_check_depends: true,
            force_aur: false,
            force_repo: false,
            rebuild: false,
            remove_conflicts: false,
            target_asdeps: false,
            chroot: false,
        }
    }
}

#[derive(Clone, Debug)]
struct Request {
    dependency: Dependency,
    asdeps: bool,
    explicit: bool,
    required_by: String,
    /// True for make/check dependencies only; they never need a host install
    /// when the build happens inside a chroot.
    build_only: bool,
    /// AUR base whose clean chroot needs this dependency.
    for_base: Option<String>,
}

#[derive(Clone, Debug)]
struct AurNode {
    candidate: Candidate,
    asdeps: bool,
    explicit: bool,
}

#[derive(Clone, Debug)]
struct OfficialNode {
    candidate: Candidate,
    asdeps: bool,
    build_only: bool,
}

/// Resolves package names into an [`InstallPlan`].
pub struct Resolver<'a> {
    catalog: &'a dyn PackageCatalog,
    options: ResolveOptions,
}

impl<'a> Resolver<'a> {
    pub fn new(catalog: &'a dyn PackageCatalog, options: ResolveOptions) -> Self {
        Self { catalog, options }
    }

    /// Resolve user-requested targets.
    ///
    /// Targets are package names or versioned specifications (`foo>=1.2`).
    pub fn resolve(&self, targets: &[String]) -> CoreResult<InstallPlan> {
        if targets.is_empty() {
            return Ok(InstallPlan::default());
        }

        let installed = self.catalog.installed()?;
        let mut state = State {
            installed,
            official: BTreeMap::new(),
            aur: BTreeMap::new(),
            removals: BTreeMap::new(),
            base_official_deps: BTreeMap::new(),
            queue: VecDeque::new(),
            seen: HashSet::new(),
        };

        for target in targets {
            let dependency = Dependency::parse(target)?;
            state.queue.push_back(Request {
                dependency,
                asdeps: self.options.target_asdeps,
                explicit: true,
                required_by: "user".to_string(),
                build_only: false,
                for_base: None,
            });
        }

        while let Some(request) = state.queue.pop_front() {
            self.process_request(&mut state, request)?;
        }

        self.check_conflicts(&mut state)?;
        let aur = build_aur_plan(&state.aur, &state.base_official_deps)?;

        Ok(InstallPlan {
            official: state
                .official
                .iter()
                .map(|(name, node)| OfficialAction {
                    name: name.clone(),
                    asdeps: node.asdeps,
                    build_only: node.build_only,
                })
                .collect(),
            aur,
            removals: state
                .removals
                .into_iter()
                .map(|(name, reason)| RemovalAction { name, reason })
                .collect(),
        })
    }

    fn process_request(&self, state: &mut State, request: Request) -> CoreResult<()> {
        let key = format!(
            "{}|{}|{}",
            request.dependency,
            request.for_base.as_deref().unwrap_or(""),
            request.build_only
        );

        // A dependency already satisfied by the installed system does not need
        // a host reinstall. Its name still goes into the base's chroot list
        // when it comes from an official repository. In chroot mode a foreign
        // build dependency must be rebuilt so its artifact is available.
        if !request.explicit && state.installed_satisfies(&request.dependency) {
            let installed = state
                .installed_satisfying(&request.dependency)
                .map(|candidate| candidate.meta.name.clone());
            let mut force_chroot_build = false;
            if let Some(name) = installed {
                if let Some(base) = &request.for_base {
                    if self.catalog.is_official_package(&name)? {
                        state.record_base_dep(base, name);
                    } else if self.options.chroot {
                        force_chroot_build = true;
                    }
                }
            }
            if !force_chroot_build {
                return Ok(());
            }
        }

        if state.planned_satisfies(&request.dependency) {
            if let Some(base) = &request.for_base {
                let planned = state
                    .planned_satisfying(&request.dependency)
                    .map(|candidate| {
                        (
                            candidate.meta.name.clone(),
                            matches!(candidate.source, CandidateSource::Official { .. }),
                        )
                    });
                if let Some((name, official)) = planned {
                    if official {
                        state.record_base_dep(base, name);
                    }
                }
            }
            state.mark_explicit(&request.dependency, request.explicit && !request.asdeps);
            return Ok(());
        }

        // Avoid asking the catalog the same unresolved question repeatedly in
        // a cyclic graph. Cycles are handled again at the build-base level.
        if !state.seen.insert(key) {
            return Ok(());
        }

        let Some(candidate) = self.choose_candidate(&request)? else {
            // An explicitly requested package that is already installed but no
            // longer exists in official/AUR repositories is still satisfied.
            if request.explicit && state.installed_satisfies(&request.dependency) {
                return Ok(());
            }
            return Err(CoreError::UnsatisfiedDependency {
                dependency: request.dependency.to_string(),
                required_by: request.required_by.clone(),
            });
        };

        if let Some(installed) = state.installed_named(&candidate.meta.name) {
            let newer = candidate.meta.version > installed.meta.version;
            let same = candidate.meta.version == installed.meta.version;
            if !newer && !self.options.rebuild {
                // A foreign build dependency still needs an artifact for the
                // clean chroot even when the host already has that version.
                let rebuild_for_chroot = request.for_base.is_some()
                    && self.options.chroot
                    && same
                    && matches!(candidate.source, CandidateSource::Aur);
                if !rebuild_for_chroot {
                    return Ok(());
                }
            }
        }

        match &candidate.source {
            CandidateSource::Official { .. } => {
                if let Some(base) = &request.for_base {
                    state.record_base_dep(base, candidate.meta.name.clone());
                }
                state
                    .official
                    .entry(candidate.meta.name.clone())
                    .and_modify(|node| {
                        if request.explicit && !request.asdeps {
                            node.asdeps = false;
                        }
                        if !request.build_only {
                            node.build_only = false;
                        }
                    })
                    .or_insert_with(|| OfficialNode {
                        candidate: candidate.clone(),
                        asdeps: request.asdeps,
                        build_only: request.build_only,
                    });
            }
            CandidateSource::Aur => {
                let base = candidate.meta.base.clone();
                let already_planned = state.aur.contains_key(&candidate.meta.name);

                let node = state
                    .aur
                    .entry(candidate.meta.name.clone())
                    .or_insert_with(|| AurNode {
                        candidate: candidate.clone(),
                        asdeps: request.asdeps,
                        explicit: request.explicit && !request.asdeps,
                    });
                if request.explicit && !request.asdeps {
                    node.explicit = true;
                    node.asdeps = false;
                }

                if !already_planned {
                    self.enqueue_aur_dependencies(state, &candidate, &base);
                }
            }
            CandidateSource::Installed => {
                return Err(CoreError::Invariant(
                    "catalog returned an installed package as a new candidate".into(),
                ));
            }
        }

        Ok(())
    }

    fn enqueue_aur_dependencies(&self, state: &mut State, candidate: &Candidate, base: &str) {
        let mut requests: Vec<(Dependency, bool)> = candidate
            .meta
            .depends
            .iter()
            .cloned()
            .map(|dependency| (dependency, false))
            .collect();
        if self.options.include_make_depends {
            requests.extend(
                candidate
                    .meta
                    .make_depends
                    .iter()
                    .cloned()
                    .map(|dependency| (dependency, true)),
            );
        }
        if self.options.include_check_depends {
            requests.extend(
                candidate
                    .meta
                    .check_depends
                    .iter()
                    .cloned()
                    .map(|dependency| (dependency, true)),
            );
        }

        for (dependency, build_only) in requests {
            state.queue.push_back(Request {
                dependency,
                asdeps: true,
                explicit: false,
                required_by: base.to_string(),
                build_only,
                for_base: Some(base.to_string()),
            });
        }
    }

    fn choose_candidate(&self, request: &Request) -> CoreResult<Option<Candidate>> {
        let dependency = &request.dependency;
        // --aur/--repo restrict the requested targets; their dependencies may
        // still come from either source. Forcing every transitive dependency
        // to AUR makes normal AUR builds fail on official build tools.
        let force_aur = self.options.force_aur && request.explicit;
        let force_repo = self.options.force_repo && request.explicit;

        if !force_aur {
            let candidates = self.catalog.official_candidates(dependency)?;
            if let Some(candidate) = choose_best(candidates, dependency) {
                return Ok(Some(candidate));
            }
        }

        if !force_repo {
            let candidates = self.catalog.aur_candidates(dependency)?;
            if let Some(candidate) = choose_best(candidates, dependency) {
                return Ok(Some(candidate));
            }
        }

        Ok(None)
    }

    fn check_conflicts(&self, state: &mut State) -> CoreResult<()> {
        let planned: Vec<Candidate> = state
            .official
            .values()
            .map(|node| node.candidate.clone())
            .chain(state.aur.values().map(|node| node.candidate.clone()))
            .collect();

        for candidate in &planned {
            for conflict in &candidate.meta.conflicts {
                if let Some(other) = state.installed_satisfying(conflict) {
                    if other.meta.name == candidate.meta.name {
                        continue;
                    }
                    if state.removals.contains_key(&other.meta.name) {
                        continue;
                    }
                    if !self.options.remove_conflicts {
                        return Err(CoreError::Conflict {
                            package: candidate.meta.name.clone(),
                            other: other.meta.name.clone(),
                        });
                    }
                    state.removals.insert(
                        other.meta.name.clone(),
                        format!("conflicts with {}", candidate.meta.name),
                    );
                }
                if let Some(other) = state.planned_satisfying(conflict) {
                    if other.meta.name != candidate.meta.name {
                        return Err(CoreError::Conflict {
                            package: candidate.meta.name.clone(),
                            other: other.meta.name.clone(),
                        });
                    }
                }
            }

            for replaced in &candidate.meta.replaces {
                if let Some(other) = state.installed_satisfying(replaced) {
                    if other.meta.name == candidate.meta.name {
                        continue;
                    }
                    if state.removals.contains_key(&other.meta.name) {
                        continue;
                    }
                    if !self.options.remove_conflicts {
                        return Err(CoreError::Replaces {
                            package: candidate.meta.name.clone(),
                            other: other.meta.name.clone(),
                        });
                    }
                    state.removals.insert(
                        other.meta.name.clone(),
                        format!("replaced by {}", candidate.meta.name),
                    );
                }
            }
        }

        Ok(())
    }
}

struct State {
    installed: Vec<Candidate>,
    official: BTreeMap<String, OfficialNode>,
    aur: BTreeMap<String, AurNode>,
    removals: BTreeMap<String, String>,
    /// Official package names needed inside each AUR base's clean chroot.
    base_official_deps: BTreeMap<String, BTreeSet<String>>,
    queue: VecDeque<Request>,
    seen: HashSet<String>,
}

impl State {
    fn record_base_dep(&mut self, base: &str, name: String) {
        self.base_official_deps
            .entry(base.to_string())
            .or_default()
            .insert(name);
    }

    fn installed_named(&self, name: &str) -> Option<&Candidate> {
        self.installed
            .iter()
            .find(|candidate| candidate.meta.name == name)
    }

    fn installed_satisfies(&self, dependency: &Dependency) -> bool {
        self.installed_satisfying(dependency).is_some()
    }

    fn installed_satisfying(&self, dependency: &Dependency) -> Option<&Candidate> {
        self.installed
            .iter()
            .filter(|candidate| !self.removals.contains_key(&candidate.meta.name))
            .find(|candidate| candidate.satisfies(dependency))
    }

    fn planned_satisfies(&self, dependency: &Dependency) -> bool {
        self.planned_satisfying(dependency).is_some()
    }

    fn planned_satisfying(&self, dependency: &Dependency) -> Option<&Candidate> {
        self.aur
            .values()
            .map(|node| &node.candidate)
            .find(|candidate| candidate.satisfies(dependency))
            .or_else(|| {
                self.official
                    .values()
                    .map(|node| &node.candidate)
                    .find(|candidate| candidate.satisfies(dependency))
            })
    }

    fn mark_explicit(&mut self, dependency: &Dependency, explicit: bool) {
        if !explicit {
            return;
        }
        if let Some(node) = self
            .aur
            .values_mut()
            .find(|node| node.candidate.satisfies(dependency))
        {
            node.explicit = true;
            node.asdeps = false;
        }
        if let Some(node) = self
            .official
            .values_mut()
            .find(|node| node.candidate.satisfies(dependency))
        {
            node.asdeps = false;
        }
    }
}

/// Prefer an exact name match, then providers; AUR ties break by votes and
/// popularity. Official catalog order already reflects repository priority.
fn choose_best(mut candidates: Vec<Candidate>, dependency: &Dependency) -> Option<Candidate> {
    candidates.retain(|candidate| candidate.satisfies(dependency));
    if candidates.is_empty() {
        return None;
    }

    if let Some(index) = candidates
        .iter()
        .position(|candidate| candidate.meta.name == dependency.name)
    {
        return Some(candidates.swap_remove(index));
    }

    candidates.sort_by(|a, b| {
        b.votes
            .cmp(&a.votes)
            .then_with(|| b.popularity.cmp(&a.popularity))
            .then_with(|| a.meta.name.cmp(&b.meta.name))
    });
    candidates.into_iter().next()
}

/// Group AUR nodes by package base and topologically sort the bases.
fn build_aur_plan(
    nodes: &BTreeMap<String, AurNode>,
    base_official_deps: &BTreeMap<String, BTreeSet<String>>,
) -> CoreResult<Vec<AurBuild>> {
    if nodes.is_empty() {
        return Ok(Vec::new());
    }

    let mut bases: BTreeMap<String, Vec<&AurNode>> = BTreeMap::new();
    for node in nodes.values() {
        bases
            .entry(node.candidate.meta.base.clone())
            .or_default()
            .push(node);
    }

    let mut edges: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut depends_on: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut indegree: BTreeMap<String, usize> =
        bases.keys().cloned().map(|base| (base, 0)).collect();

    for (base, package_nodes) in &bases {
        for node in package_nodes {
            let mut dependencies = node.candidate.meta.depends.clone();
            dependencies.extend(node.candidate.meta.make_depends.clone());
            dependencies.extend(node.candidate.meta.check_depends.clone());

            for dependency in dependencies {
                let Some(provider) = nodes
                    .values()
                    .find(|candidate| candidate.candidate.satisfies(&dependency))
                else {
                    continue;
                };
                let provider_base = &provider.candidate.meta.base;
                if provider_base == base {
                    continue;
                }
                if edges
                    .entry(provider_base.clone())
                    .or_default()
                    .insert(base.clone())
                {
                    depends_on
                        .entry(base.clone())
                        .or_default()
                        .insert(provider_base.clone());
                    *indegree.get_mut(base).expect("known base") += 1;
                }
            }
        }
    }

    let mut ready: BTreeSet<String> = indegree
        .iter()
        .filter(|(_, degree)| **degree == 0)
        .map(|(base, _)| base.clone())
        .collect();
    let mut ordered = Vec::with_capacity(bases.len());

    while let Some(base) = ready.pop_first() {
        ordered.push(base.clone());
        if let Some(dependents) = edges.get(&base) {
            for dependent in dependents {
                let degree = indegree.get_mut(dependent).expect("known base");
                *degree -= 1;
                if *degree == 0 {
                    ready.insert(dependent.clone());
                }
            }
        }
    }

    if ordered.len() != bases.len() {
        let mut remaining: Vec<String> = indegree
            .into_iter()
            .filter(|(_, degree)| *degree > 0)
            .map(|(base, _)| base)
            .collect();
        remaining.sort();
        return Err(CoreError::DependencyCycle(remaining.join(", ")));
    }

    Ok(ordered
        .into_iter()
        .map(|base| {
            let mut packages: Vec<AurPackagePlan> = bases[&base]
                .iter()
                .map(|node| AurPackagePlan {
                    name: node.candidate.meta.name.clone(),
                    version: node.candidate.meta.version.clone(),
                    explicit: node.explicit,
                })
                .collect();
            packages.sort_by(|a, b| a.name.cmp(&b.name));
            AurBuild {
                depends_on: depends_on
                    .remove(&base)
                    .unwrap_or_default()
                    .into_iter()
                    .collect(),
                build_deps: base_official_deps
                    .get(&base)
                    .map(|deps| deps.iter().cloned().collect())
                    .unwrap_or_default(),
                base,
                packages,
            }
        })
        .collect())
}

/// Convenience for callers that only need to know whether a candidate is
/// explicit in a plan.
pub fn plan_explicit(plan: &InstallPlan, name: &str) -> bool {
    plan.aur
        .iter()
        .flat_map(|build| &build.packages)
        .any(|package| package.name == name && package.explicit)
}

/// Helper used by tests and adapters to construct a dependency reason.
pub fn reason_for(asdeps: bool) -> InstallReason {
    if asdeps {
        InstallReason::Dependency
    } else {
        InstallReason::Explicit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::CandidateSource;
    use crate::package::PackageMeta;

    struct FakeCatalog {
        installed: Vec<Candidate>,
        official: Vec<Candidate>,
        aur: Vec<Candidate>,
    }

    impl FakeCatalog {
        fn new() -> Self {
            Self {
                installed: Vec::new(),
                official: Vec::new(),
                aur: Vec::new(),
            }
        }

        fn installed(mut self, name: &str, version: &str, reason: InstallReason) -> Self {
            let mut candidate =
                Candidate::new(PackageMeta::new(name, version), CandidateSource::Installed);
            candidate.reason = Some(reason);
            self.installed.push(candidate);
            self
        }

        fn official(mut self, name: &str, version: &str) -> Self {
            self.official.push(Candidate::new(
                PackageMeta::new(name, version),
                CandidateSource::Official {
                    repo: "extra".into(),
                    filename: format!("{name}-{version}-x86_64.pkg.tar.zst"),
                },
            ));
            self
        }

        fn aur_meta(mut self, meta: PackageMeta) -> Self {
            self.aur.push(Candidate::new(meta, CandidateSource::Aur));
            self
        }
    }

    impl PackageCatalog for FakeCatalog {
        fn installed(&self) -> CoreResult<Vec<Candidate>> {
            Ok(self.installed.clone())
        }

        fn official_candidates(&self, dependency: &Dependency) -> CoreResult<Vec<Candidate>> {
            Ok(self
                .official
                .iter()
                .filter(|candidate| candidate.satisfies(dependency))
                .cloned()
                .collect())
        }

        fn aur_candidates(&self, dependency: &Dependency) -> CoreResult<Vec<Candidate>> {
            Ok(self
                .aur
                .iter()
                .filter(|candidate| candidate.satisfies(dependency))
                .cloned()
                .collect())
        }
    }

    fn meta(name: &str, version: &str) -> PackageMeta {
        PackageMeta::new(name, version)
    }

    fn dep(spec: &str) -> Dependency {
        Dependency::parse(spec).unwrap()
    }

    fn resolve(catalog: &FakeCatalog, targets: &[&str]) -> CoreResult<InstallPlan> {
        Resolver::new(catalog, ResolveOptions::default())
            .resolve(&targets.iter().map(ToString::to_string).collect::<Vec<_>>())
    }

    #[test]
    fn installs_official_and_aur_dependencies() {
        let mut app = meta("app", "1.0-1");
        app.base = "app".into();
        app.depends = vec![dep("libc"), dep("libaur")];

        let mut lib = meta("libaur", "2.0-1");
        lib.depends = vec![dep("libc")];

        let catalog = FakeCatalog::new()
            .official("libc", "1.0-1")
            .aur_meta(app)
            .aur_meta(lib);
        let plan = resolve(&catalog, &["app"]).unwrap();

        assert_eq!(plan.official.len(), 1);
        assert_eq!(plan.official[0].name, "libc");
        assert_eq!(plan.aur.len(), 2);
        assert_eq!(plan.aur[0].base, "libaur");
        assert_eq!(plan.aur[1].base, "app");
    }

    #[test]
    fn records_base_dependency_levels() {
        let mut lib = meta("lib", "1.0-1");
        lib.base = "lib".into();

        let mut app = meta("app", "1.0-1");
        app.base = "app".into();
        app.depends = vec![dep("lib")];

        let mut tool = meta("tool", "1.0-1");
        tool.base = "tool".into();

        let catalog = FakeCatalog::new()
            .aur_meta(lib)
            .aur_meta(app)
            .aur_meta(tool);
        let plan = resolve(&catalog, &["app", "tool"]).unwrap();

        let by_base = |name: &str| plan.aur.iter().find(|build| build.base == name).unwrap();
        assert!(by_base("lib").depends_on.is_empty());
        assert!(by_base("tool").depends_on.is_empty());
        assert_eq!(by_base("app").depends_on, vec!["lib".to_string()]);

        let first_level: Vec<&str> = plan
            .aur
            .iter()
            .filter(|build| build.depends_on.is_empty())
            .map(|build| build.base.as_str())
            .collect();
        assert_eq!(first_level, vec!["lib", "tool"]);
    }

    #[test]
    fn official_build_dependencies_are_marked_and_scoped_to_the_base() {
        let mut app = meta("app", "1.0-1");
        app.base = "app".into();
        app.depends = vec![dep("libc")];
        app.make_depends = vec![dep("makedep")];

        let catalog = FakeCatalog::new()
            .official("libc", "1.0-1")
            .official("makedep", "1.0-1")
            .aur_meta(app);
        let plan = resolve(&catalog, &["app"]).unwrap();

        let by_name = |name: &str| {
            plan.official
                .iter()
                .find(|action| action.name == name)
                .unwrap()
        };
        assert!(!by_name("libc").build_only);
        assert!(by_name("makedep").build_only);

        let app = plan.aur.iter().find(|build| build.base == "app").unwrap();
        assert_eq!(
            app.build_deps,
            vec!["libc".to_string(), "makedep".to_string()]
        );
    }

    #[test]
    fn installed_official_dependency_is_still_added_to_the_chroot() {
        let mut app = meta("app", "1.0-1");
        app.base = "app".into();
        app.depends = vec![dep("libc")];

        let catalog = FakeCatalog::new()
            .installed("libc", "1.0-1", InstallReason::Dependency)
            .official("libc", "1.0-1")
            .aur_meta(app);
        let plan = resolve(&catalog, &["app"]).unwrap();

        assert!(plan.official.is_empty());
        let app = plan.aur.iter().find(|build| build.base == "app").unwrap();
        assert_eq!(app.build_deps, vec!["libc".to_string()]);
    }

    #[test]
    fn chroot_mode_rebuilds_installed_foreign_build_dependencies() {
        let mut app = meta("app", "1.0-1");
        app.base = "app".into();
        app.make_depends = vec![dep("tool")];

        let catalog = FakeCatalog::new()
            .installed("tool", "1.0-1", InstallReason::Explicit)
            .aur_meta(meta("tool", "1.0-1"))
            .aur_meta(app);

        let plan = resolve(&catalog, &["app"]).unwrap();
        assert_eq!(plan.aur.len(), 1, "host-installed tool needs no rebuild");

        let options = ResolveOptions {
            chroot: true,
            ..ResolveOptions::default()
        };
        let plan = Resolver::new(&catalog, options)
            .resolve(&["app".to_string()])
            .unwrap();
        assert_eq!(plan.aur.len(), 2, "tool must be rebuilt for the chroot");
        assert!(plan.aur.iter().any(|build| build.base == "tool"));
    }

    #[test]
    fn installed_dependency_prevents_reinstall() {
        let mut app = meta("app", "1.0-1");
        app.depends = vec![dep("libc")];
        let catalog = FakeCatalog::new()
            .installed("libc", "1.0-1", InstallReason::Dependency)
            .official("libc", "1.0-1")
            .aur_meta(app);
        let plan = resolve(&catalog, &["app"]).unwrap();
        assert!(plan.official.is_empty());
        assert_eq!(plan.aur.len(), 1);
    }

    #[test]
    fn split_packages_share_one_build() {
        let mut base = meta("demo", "1.0-1");
        base.base = "demo".into();
        let mut docs = meta("demo-docs", "1.0-1");
        docs.base = "demo".into();

        let catalog = FakeCatalog::new().aur_meta(base).aur_meta(docs);
        let plan = resolve(&catalog, &["demo", "demo-docs"]).unwrap();
        assert_eq!(plan.aur.len(), 1);
        assert_eq!(plan.aur[0].base, "demo");
        let names: Vec<_> = plan.aur[0]
            .packages
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(names, vec!["demo", "demo-docs"]);
    }

    #[test]
    fn reports_build_cycles() {
        let mut a = meta("a", "1.0-1");
        a.depends = vec![dep("b")];
        let mut b = meta("b", "1.0-1");
        b.depends = vec![dep("a")];
        let catalog = FakeCatalog::new().aur_meta(a).aur_meta(b);
        assert!(matches!(
            resolve(&catalog, &["a"]),
            Err(CoreError::DependencyCycle(_))
        ));
    }

    #[test]
    fn finds_providers_and_constraints() {
        let mut app = meta("app", "1.0-1");
        app.depends = vec![dep("java-runtime>=17")];
        let mut jre = meta("jre-openjdk", "21.0-1");
        jre.provides = vec![dep("java-runtime=21")];
        let catalog = FakeCatalog::new().aur_meta(app).aur_meta(jre);
        let plan = resolve(&catalog, &["app"]).unwrap();
        assert_eq!(plan.aur.len(), 2);
        assert!(plan.aur.iter().any(|b| b.base == "jre-openjdk"));
    }

    #[test]
    fn marks_dependencies_and_targets() {
        let mut app = meta("app", "1.0-1");
        app.depends = vec![dep("lib")];
        let mut lib = meta("lib", "1.0-1");
        lib.base = "lib".into();
        let catalog = FakeCatalog::new().aur_meta(app).aur_meta(lib);
        let plan = resolve(&catalog, &["app"]).unwrap();
        let app = plan.aur.iter().find(|b| b.base == "app").unwrap();
        let lib = plan.aur.iter().find(|b| b.base == "lib").unwrap();
        assert!(app.packages.iter().all(|p| p.explicit));
        assert!(lib.packages.iter().all(|p| !p.explicit));
    }

    #[test]
    fn detects_conflicts_and_can_remove_them() {
        let mut app = meta("app", "1.0-1");
        app.conflicts = vec![dep("old")];
        let catalog = FakeCatalog::new()
            .installed("old", "1.0-1", InstallReason::Explicit)
            .aur_meta(app);

        let error = resolve(&catalog, &["app"]).unwrap_err();
        assert!(matches!(error, CoreError::Conflict { .. }));

        let options = ResolveOptions {
            remove_conflicts: true,
            ..ResolveOptions::default()
        };
        let plan = Resolver::new(&catalog, options)
            .resolve(&["app".to_string()])
            .unwrap();
        assert_eq!(plan.removals[0].name, "old");
    }
}
