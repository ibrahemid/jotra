//! Workspace file-name index walk and the shared search ignore policy.
//!
//! The bounded walk in [`walk_index`] enumerates every file under the workspace
//! root that survives the union ignore policy (ADR-026): Jotra's default ignores
//! (`node_modules`, `target`, `.git`, and the rest) plus the git ignore sources
//! (`.gitignore`, `.ignore`, global gitignore). Hidden files are included; only
//! `.git/` and the Jotra defaults are pruned. The same [`build_walk`] is the base
//! for the content grep, so name search and content search agree on which files
//! exist.
//!
//! [`is_path_indexed`] answers the single-path question the watcher patch path
//! needs: given one changed path, should it be in the name index? It applies the
//! Jotra defaults, the root `.gitignore`/`.ignore`, and the global gitignore. It
//! does not read `.gitignore`s nested in subdirectories (the `ignore` crate has
//! no single-path query over the full nested stack); a full rebuild is the
//! convergence backstop for that gap. [`walk_index_within`] answers for a set of
//! paths with the full stack, by walking from the root down to them only.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::WalkBuilder;

/// Result of a bounded workspace file walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexWalk {
    /// Workspace-relative file paths (forward-slash separated).
    pub paths: Vec<String>,
    /// `true` when the walk stopped at `cap` and more files exist.
    pub truncated: bool,
}

/// Builds the shared search walker for `root`: hidden files included, symlinks
/// never followed, git ignore sources honored even outside a git repo, and
/// Jotra's default-ignored directories pruned. Used by both the name index walk
/// and the content grep so the two apply one ignore policy.
pub fn build_walk(root: &Path) -> WalkBuilder {
    walk_entering(root, |_| true)
}

/// [`build_walk`], entering only the entries `enters` accepts.
fn walk_entering(
    root: &Path,
    enters: impl Fn(&Path) -> bool + Send + Sync + 'static,
) -> WalkBuilder {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .follow_links(false)
        .require_git(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .ignore(true);

    let ignores = jotra_core::workspace::default_ignored_dirs();
    builder.filter_entry(move |entry| {
        let name = entry.file_name().to_string_lossy();
        !ignores.iter().any(|ig| *ig == name) && enters(entry.path())
    });
    builder
}

/// Normalizes `path` to a workspace-relative, forward-slash string, or `None`
/// when it is not under `root` or is the root itself.
fn relative_path(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    if rel.as_os_str().is_empty() {
        return None;
    }
    let mut s = rel.to_string_lossy().into_owned();
    if std::path::MAIN_SEPARATOR == '\\' {
        s = s.replace('\\', "/");
    }
    Some(s)
}

/// Walks `root` and collects up to `cap` workspace-relative file paths under the
/// union ignore policy. On overflow the walk stops and `truncated` is `true`.
pub fn walk_index(root: &Path, cap: usize) -> IndexWalk {
    collect_walk(build_walk(root), root, cap)
}

/// [`walk_index`] over only what `targets` name: each target, everything under
/// it, and the folders on the way down to it. A file comes back exactly when
/// the whole walk would list it, since the walk reaches it through the same
/// folders and reads the same rule files on the way, nested ones included.
/// A target that is outside `root`, gone, or ignored yields nothing.
pub fn walk_index_within(root: &Path, targets: &[PathBuf], cap: usize) -> IndexWalk {
    let targets: HashSet<PathBuf> = targets
        .iter()
        .filter(|target| target.starts_with(root) && target.as_path() != root)
        .cloned()
        .collect();
    if targets.is_empty() {
        return IndexWalk {
            paths: Vec::new(),
            truncated: false,
        };
    }
    let on_the_way: HashSet<PathBuf> = targets
        .iter()
        .flat_map(|target| target.ancestors().skip(1))
        .filter(|folder| folder.starts_with(root))
        .map(Path::to_path_buf)
        .collect();
    let enters = move |path: &Path| {
        on_the_way.contains(path) || path.ancestors().any(|at| targets.contains(at))
    };
    collect_walk(walk_entering(root, enters), root, cap)
}

/// Collects up to `cap` workspace-relative file paths from `walk`.
fn collect_walk(walk: WalkBuilder, root: &Path, cap: usize) -> IndexWalk {
    let mut paths = Vec::new();
    let mut truncated = false;

    for result in walk.build() {
        let Ok(entry) = result else { continue };
        match entry.file_type() {
            Some(ft) if ft.is_file() => {}
            _ => continue,
        }
        let Some(rel) = relative_path(root, entry.path()) else {
            continue;
        };
        if paths.len() >= cap {
            truncated = true;
            break;
        }
        paths.push(rel);
    }

    IndexWalk { paths, truncated }
}

/// Builds a gitignore matcher from `root`'s own `.gitignore` and `.ignore`.
fn root_gitignore(root: &Path) -> Gitignore {
    let mut builder = GitignoreBuilder::new(root);
    builder.add(root.join(".gitignore"));
    builder.add(root.join(".ignore"));
    builder.build().unwrap_or_else(|_| Gitignore::empty())
}

/// The single-path ignore policy for one workspace root, parsed once.
///
/// The watcher patch path asks it about every changed path, and parsing the
/// root `.gitignore`, `.ignore` and the global excludes file per event is what
/// a `pip install` into the workspace multiplied by tens of thousands (TS-12).
/// It is built when the root is set and built again when one of the root's
/// own rule files changes ([`WorkspaceIgnore::is_rules_file`]); the global
/// excludes file is read when it is built.
#[derive(Debug)]
pub struct WorkspaceIgnore {
    root: PathBuf,
    root_rules: Gitignore,
    global: Gitignore,
}

impl WorkspaceIgnore {
    /// Reads the rules that apply under `root`.
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            root_rules: root_gitignore(root),
            global: Gitignore::global().0,
        }
    }

    /// The root this policy was read for.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether `path` is one of the root's own rule files, a change to which
    /// means the policy has to be read again.
    pub fn is_rules_file(&self, path: &Path) -> bool {
        path.parent() == Some(self.root.as_path()) && is_rules_file_name(path)
    }

    /// Returns `true` when `path` belongs in the name index: it is under the
    /// root, is not a Jotra default-ignored component, and is not matched by
    /// the root gitignore or the global gitignore. Nested subdirectory
    /// `.gitignore`s are not consulted (see the module docs); a full rebuild
    /// reconciles that gap.
    pub fn is_indexed(&self, path: &Path) -> bool {
        let root = self.root.as_path();
        let Ok(rel) = path.strip_prefix(root) else {
            return false;
        };
        if rel.as_os_str().is_empty() {
            return false;
        }
        if jotra_core::workspace::path_has_ignored_component(root, path) {
            return false;
        }

        // Match on the workspace-relative path: an absolute path outside a
        // matcher's own root makes `matched_path_or_any_parents` panic, and the
        // global matcher's root is the home directory, not the workspace.
        let is_dir = path.is_dir();
        if self
            .root_rules
            .matched_path_or_any_parents(rel, is_dir)
            .is_ignore()
        {
            return false;
        }
        !self
            .global
            .matched_path_or_any_parents(rel, is_dir)
            .is_ignore()
    }
}

/// Whether `path` is named like a rule file the walk reads in every folder it
/// enters, which governs every path under that folder.
pub fn is_rules_file_name(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name == ".gitignore" || name == ".ignore")
}

/// Returns `true` when `path` belongs in the name index under `root`, reading
/// the rules for this one question ([`WorkspaceIgnore::is_indexed`]).
pub fn is_path_indexed(root: &Path, path: &Path) -> bool {
    WorkspaceIgnore::new(root).is_indexed(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write(dir: &Path, rel: &str, body: &str) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, body).unwrap();
    }

    #[test]
    fn walk_lists_files_relative_to_root() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), "src/main.rs", "fn main() {}");
        write(dir.path(), "README.md", "# hi");

        let walk = walk_index(dir.path(), 1000);
        assert!(walk.paths.contains(&"src/main.rs".to_string()));
        assert!(walk.paths.contains(&"README.md".to_string()));
        assert!(!walk.truncated);
    }

    #[test]
    fn walk_prunes_jotra_default_ignores() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), "node_modules/pkg/index.js", "x");
        write(dir.path(), "target/debug/app", "x");
        write(dir.path(), ".git/HEAD", "ref: x");
        write(dir.path(), "src/lib.rs", "x");

        let walk = walk_index(dir.path(), 1000);
        assert_eq!(walk.paths, vec!["src/lib.rs".to_string()]);
    }

    #[test]
    fn walk_honors_gitignore() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), ".gitignore", "secret.txt\nbuild-out/\n");
        write(dir.path(), "secret.txt", "shh");
        write(dir.path(), "build-out/artifact.bin", "x");
        write(dir.path(), "keep.rs", "x");

        let walk = walk_index(dir.path(), 1000);
        assert!(walk.paths.contains(&"keep.rs".to_string()));
        // .gitignore itself is a dotfile and is included (dotfiles are searched).
        assert!(walk.paths.contains(&".gitignore".to_string()));
        assert!(!walk.paths.contains(&"secret.txt".to_string()));
        assert!(!walk.paths.iter().any(|p| p.starts_with("build-out/")));
    }

    #[test]
    fn walk_includes_dotfiles_except_git() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), ".env", "TOKEN=1");
        write(dir.path(), ".github/workflows/ci.yml", "on: push");
        write(dir.path(), ".git/config", "[core]");

        let walk = walk_index(dir.path(), 1000);
        assert!(walk.paths.contains(&".env".to_string()));
        assert!(walk.paths.contains(&".github/workflows/ci.yml".to_string()));
        assert!(!walk.paths.iter().any(|p| p.starts_with(".git/")));
    }

    #[test]
    fn walk_marks_truncated_at_cap() {
        let dir = TempDir::new().unwrap();
        for i in 0..10 {
            write(dir.path(), &format!("f{i}.txt"), "x");
        }
        let walk = walk_index(dir.path(), 4);
        assert_eq!(walk.paths.len(), 4);
        assert!(walk.truncated);
    }

    #[test]
    fn is_path_indexed_matches_the_walk_policy() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), ".gitignore", "secret.txt\n");
        write(dir.path(), "keep.rs", "x");
        write(dir.path(), "secret.txt", "x");
        write(dir.path(), "node_modules/pkg/a.js", "x");
        write(dir.path(), ".env", "x");

        let root = dir.path();
        assert!(is_path_indexed(root, &root.join("keep.rs")));
        assert!(is_path_indexed(root, &root.join(".env")));
        assert!(!is_path_indexed(root, &root.join("secret.txt")));
        assert!(!is_path_indexed(root, &root.join("node_modules/pkg/a.js")));
        // A path outside the root is never indexed.
        let other = TempDir::new().unwrap();
        assert!(!is_path_indexed(root, &other.path().join("x.rs")));
    }

    #[test]
    fn the_rules_are_read_once_and_again_only_when_rebuilt() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        write(root, ".gitignore", "a.log\n");
        write(root, "a.log", "x");
        write(root, "b.log", "x");
        let policy = WorkspaceIgnore::new(root);
        assert!(!policy.is_indexed(&root.join("a.log")));
        assert!(policy.is_indexed(&root.join("b.log")));

        write(root, ".gitignore", "b.log\n");
        assert!(
            policy.is_indexed(&root.join("b.log")),
            "a built policy does not reread the file per question"
        );
        let rebuilt = WorkspaceIgnore::new(root);
        assert!(rebuilt.is_indexed(&root.join("a.log")));
        assert!(!rebuilt.is_indexed(&root.join("b.log")));
    }

    fn within(root: &Path, targets: &[&str], cap: usize) -> IndexWalk {
        let targets: Vec<PathBuf> = targets.iter().map(|rel| root.join(rel)).collect();
        let mut walk = walk_index_within(root, &targets, cap);
        walk.paths.sort();
        walk
    }

    #[test]
    fn a_walk_within_targets_lists_what_the_whole_walk_lists_there() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        write(root, ".gitignore", "*.tmp\n");
        write(root, "top.rs", "x");
        write(root, "a/.gitignore", "gen/\n!keep.tmp\n");
        write(root, "a/one.rs", "x");
        write(root, "a/drop.tmp", "x");
        write(root, "a/keep.tmp", "x");
        write(root, "a/gen/out.rs", "x");
        write(root, "a/b/two.rs", "x");
        write(root, "a/b/node_modules/pkg.js", "x");
        write(root, "c/three.rs", "x");

        let walk = within(root, &["a"], 100);
        assert_eq!(
            walk.paths,
            vec![
                "a/.gitignore".to_string(),
                "a/b/two.rs".to_string(),
                "a/keep.tmp".to_string(),
                "a/one.rs".to_string(),
            ]
        );
        let mut whole = walk_index(root, 100).paths;
        whole.retain(|path| path.starts_with("a/"));
        whole.sort();
        assert_eq!(walk.paths, whole);
        assert!(!walk.truncated);
    }

    #[test]
    fn a_walk_within_a_file_target_lists_it_only_when_it_is_indexed() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        write(root, "web/.gitignore", "generated/\n");
        write(root, "web/main.ts", "x");
        write(root, "web/other.ts", "x");
        write(root, "web/generated/app.js", "x");

        let walk = within(root, &["web/main.ts", "web/generated/app.js"], 100);
        assert_eq!(walk.paths, vec!["web/main.ts".to_string()]);
    }

    #[test]
    fn a_walk_within_gone_ignored_or_outside_targets_lists_nothing() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        write(root, "keep.rs", "x");
        write(root, "node_modules/pkg/a.js", "x");
        let other = TempDir::new().unwrap();
        write(other.path(), "x.rs", "x");

        let targets = vec![
            root.join("gone"),
            root.join("node_modules"),
            other.path().join("x.rs"),
            root.to_path_buf(),
        ];
        let walk = walk_index_within(root, &targets, 100);
        assert!(walk.paths.is_empty());
        assert!(!walk.truncated);
    }

    #[test]
    fn a_walk_within_targets_marks_truncated_at_cap() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        for i in 0..5 {
            write(root, &format!("a/f{i}.txt"), "x");
        }
        let walk = within(root, &["a"], 3);
        assert_eq!(walk.paths.len(), 3);
        assert!(walk.truncated);
    }

    #[test]
    fn only_the_roots_own_rule_files_ask_for_a_rebuild() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let policy = WorkspaceIgnore::new(root);
        assert!(policy.is_rules_file(&root.join(".gitignore")));
        assert!(policy.is_rules_file(&root.join(".ignore")));
        assert!(!policy.is_rules_file(&root.join("src/.gitignore")));
        assert!(!policy.is_rules_file(&root.join("gitignore")));
    }

    #[test]
    fn a_rule_file_is_known_by_its_name_at_any_depth() {
        assert!(is_rules_file_name(Path::new("/w/.gitignore")));
        assert!(is_rules_file_name(Path::new("/w/src/deep/.ignore")));
        assert!(!is_rules_file_name(Path::new("/w/src/gitignore")));
        assert!(!is_rules_file_name(Path::new("/w/src/.gitignore.bak")));
    }
}
