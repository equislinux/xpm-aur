//! Parallel build scheduling for independent AUR package bases.
//!
//! Builds run concurrently up to a job limit. Installs and state updates stay
//! on the caller thread, so xpm transactions never overlap and every base is
//! installed only after all of its dependencies are installed.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc;

use xpm_aur_core::plan::AurBuild;

use crate::error::{AppError, AppResult};

/// A finished build waiting to be installed.
#[derive(Debug)]
pub(crate) struct BuiltBase {
    pub commit: String,
    pub artifacts: xpm_aur_build::BuildArtifacts,
}

impl BuiltBase {
    /// Package files from this base that belong to the install plan.
    pub fn files(&self, build: &AurBuild) -> Vec<PathBuf> {
        build
            .packages
            .iter()
            .filter_map(|package| self.artifacts.get(&package.name))
            .map(|artifact| artifact.path.clone())
            .collect()
    }
}

type BuildResult = (usize, AppResult<BuiltBase>);

/// Build all bases, invoking `install` sequentially as builds finish.
///
/// A base becomes ready only after every base in its `depends_on` list has
/// been built and installed. Ready bases are built concurrently up to `jobs`.
pub(crate) fn run<B, I>(builds: &[AurBuild], jobs: usize, build: B, mut install: I) -> AppResult<()>
where
    B: Fn(&AurBuild, &[PathBuf]) -> AppResult<BuiltBase> + Sync,
    I: FnMut(&AurBuild, &BuiltBase) -> AppResult<()>,
{
    if builds.is_empty() {
        return Ok(());
    }

    let jobs = jobs.max(1);
    let indexes: BTreeMap<&str, usize> = builds
        .iter()
        .enumerate()
        .map(|(index, build)| (build.base.as_str(), index))
        .collect();

    let mut dependencies: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); builds.len()];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); builds.len()];
    for (index, build) in builds.iter().enumerate() {
        for dependency in &build.depends_on {
            let Some(&dependency_index) = indexes.get(dependency.as_str()) else {
                continue;
            };
            if dependency_index != index {
                dependencies[index].insert(dependency_index);
            }
        }
    }
    for (index, deps) in dependencies.iter().enumerate() {
        for &dependency in deps {
            dependents[dependency].push(index);
        }
    }
    for dependents in &mut dependents {
        dependents.sort_unstable();
    }

    let mut remaining: Vec<usize> = dependencies.iter().map(BTreeSet::len).collect();
    let mut ready: VecDeque<usize> = remaining
        .iter()
        .enumerate()
        .filter(|(_, count)| **count == 0)
        .map(|(index, _)| index)
        .collect();

    std::thread::scope(|scope| {
        let (sender, receiver) = mpsc::channel::<BuildResult>();
        let mut running = 0usize;
        let mut installed: BTreeSet<usize> = BTreeSet::new();
        let mut artifacts: Vec<Option<Vec<PathBuf>>> = vec![None; builds.len()];
        let mut failure: Option<AppError> = None;

        loop {
            while failure.is_none() && running < jobs {
                let Some(index) = ready.pop_front() else {
                    break;
                };
                let build_ref = &builds[index];
                let preinstall = preinstall_files(index, &dependencies, &artifacts);
                let sender = sender.clone();
                let build = &build;
                running += 1;
                scope.spawn(move || {
                    let result = build(build_ref, &preinstall);
                    let _ = sender.send((index, result));
                });
            }

            if running == 0 {
                break;
            }

            let Ok((index, result)) = receiver.recv() else {
                break;
            };
            running -= 1;

            if failure.is_some() {
                continue;
            }

            match result {
                Ok(built) => match install(&builds[index], &built) {
                    Ok(()) => {
                        artifacts[index] = Some(built.files(&builds[index]));
                        installed.insert(index);
                        for &dependent in &dependents[index] {
                            remaining[dependent] -= 1;
                            if remaining[dependent] == 0 {
                                ready.push_back(dependent);
                            }
                        }
                    }
                    Err(error) => failure = Some(error),
                },
                Err(error) => failure = Some(error),
            }
        }

        if let Some(error) = failure {
            return Err(error);
        }
        if installed.len() != builds.len() {
            return Err(AppError::Core(xpm_aur_core::error::CoreError::Invariant(
                "AUR build scheduler stalled; dependency cycle?".into(),
            )));
        }
        Ok(())
    })
}

/// Artifacts of all transitive dependencies, in plan order.
fn preinstall_files(
    index: usize,
    dependencies: &[BTreeSet<usize>],
    artifacts: &[Option<Vec<PathBuf>>],
) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    let mut stack = vec![index];
    while let Some(current) = stack.pop() {
        for &dependency in &dependencies[current] {
            if seen.insert(dependency) {
                stack.push(dependency);
            }
        }
    }
    seen.into_iter()
        .flat_map(|dependency| artifacts[dependency].clone().unwrap_or_default())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use xpm_aur_build::{BuildArtifacts, BuiltPackage};
    use xpm_aur_core::plan::AurPackagePlan;
    use xpm_aur_core::version::Evr;

    use super::*;

    fn base(name: &str, depends_on: &[&str]) -> AurBuild {
        AurBuild {
            base: name.into(),
            packages: vec![AurPackagePlan {
                name: name.into(),
                version: Evr::parse("1.0-1").unwrap(),
                explicit: true,
            }],
            depends_on: depends_on
                .iter()
                .map(|dependency| (*dependency).into())
                .collect(),
            build_deps: Vec::new(),
        }
    }

    fn built(build: &AurBuild) -> BuiltBase {
        BuiltBase {
            commit: "cafebabe".into(),
            artifacts: BuildArtifacts {
                base: build.base.clone(),
                packages: build
                    .packages
                    .iter()
                    .map(|package| BuiltPackage {
                        name: package.name.clone(),
                        path: PathBuf::from(format!("/artifacts/{}", package.name)),
                    })
                    .collect(),
            },
        }
    }

    #[test]
    fn passes_transitive_dependency_artifacts_to_dependents() {
        let builds = vec![
            base("lib", &[]),
            base("middle", &["lib"]),
            base("app", &["middle"]),
        ];
        let seen = Arc::new(Mutex::new(Vec::new()));

        run(
            &builds,
            2,
            |build, preinstall| {
                seen.lock()
                    .unwrap()
                    .push((build.base.clone(), preinstall.to_vec()));
                Ok(built(build))
            },
            |_, _| Ok(()),
        )
        .unwrap();

        let seen = seen.lock().unwrap();
        assert!(seen[0].1.is_empty());
        assert_eq!(seen[1].1, vec![PathBuf::from("/artifacts/lib")]);
        assert_eq!(
            seen[2].1,
            vec![
                PathBuf::from("/artifacts/lib"),
                PathBuf::from("/artifacts/middle")
            ]
        );
    }

    #[test]
    fn builds_dependency_levels_in_parallel_but_installs_serially() {
        let builds = vec![
            base("root", &[]),
            base("left", &["root"]),
            base("right", &["root"]),
            base("leaf", &["left", "right"]),
        ];
        let installed = Arc::new(Mutex::new(HashSet::new()));
        let active_builds = Arc::new(AtomicUsize::new(0));
        let max_builds = Arc::new(AtomicUsize::new(0));
        let active_installs = Arc::new(AtomicUsize::new(0));
        let max_installs = Arc::new(AtomicUsize::new(0));
        let wrong_order = Arc::new(Mutex::new(Vec::new()));

        run(
            &builds,
            2,
            |build, _| {
                let active = active_builds.fetch_add(1, Ordering::SeqCst) + 1;
                max_builds.fetch_max(active, Ordering::SeqCst);
                if build.base == "leaf" {
                    let installed = installed.lock().unwrap();
                    if !installed.contains("left") || !installed.contains("right") {
                        wrong_order.lock().unwrap().push(build.base.clone());
                    }
                }
                std::thread::sleep(Duration::from_millis(20));
                active_builds.fetch_sub(1, Ordering::SeqCst);
                Ok(built(build))
            },
            |build, _| {
                let active = active_installs.fetch_add(1, Ordering::SeqCst) + 1;
                max_installs.fetch_max(active, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(10));
                installed.lock().unwrap().insert(build.base.clone());
                active_installs.fetch_sub(1, Ordering::SeqCst);
                Ok(())
            },
        )
        .unwrap();

        assert!(
            max_builds.load(Ordering::SeqCst) >= 2,
            "independent bases should build concurrently"
        );
        assert_eq!(
            max_installs.load(Ordering::SeqCst),
            1,
            "xpm installs must stay sequential"
        );
        assert_eq!(installed.lock().unwrap().len(), builds.len());
        assert!(wrong_order.lock().unwrap().is_empty());
    }

    #[test]
    fn honors_the_job_limit() {
        let builds: Vec<_> = (0..5)
            .map(|index| base(&format!("pkg{index}"), &[]))
            .collect();
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));

        run(
            &builds,
            2,
            |build, _| {
                let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                max_active.fetch_max(current, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(15));
                active.fetch_sub(1, Ordering::SeqCst);
                Ok(built(build))
            },
            |_, _| Ok(()),
        )
        .unwrap();

        assert!(
            max_active.load(Ordering::SeqCst) <= 2,
            "build concurrency exceeded the job limit"
        );
    }
}
