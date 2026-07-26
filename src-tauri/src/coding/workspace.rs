//! Workspace confinement. Every coding-tool path is resolved relative to a
//! canonicalized root and verified to stay inside it — defeating both `..`
//! traversal and symlink escapes (canonicalize resolves symlinks before the
//! prefix check). New (not-yet-existing) files are validated via their nearest
//! existing ancestor so we can still create files without weakening the guard.

use anyhow::{anyhow, Result};
use std::path::{Component, Path, PathBuf};

#[derive(Clone)]
pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    /// Canonicalize the workspace root. Errors if it does not exist or is a file.
    pub fn new(root: &str) -> Result<Self> {
        let root = std::fs::canonicalize(root)
            .map_err(|e| anyhow!("workspace root '{root}' is not accessible: {e}"))?;
        if !root.is_dir() {
            return Err(anyhow!("workspace root '{}' is not a directory", root.display()));
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn root_str(&self) -> String {
        self.root.display().to_string()
    }

    /// Present a resolved absolute path back to the model/UI as a workspace-
    /// relative path (falls back to the absolute path if it is somehow outside).
    pub fn display_relative(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .map(|p| if p.as_os_str().is_empty() { ".".to_string() } else { p.display().to_string() })
            .unwrap_or_else(|_| path.display().to_string())
    }

    /// Accept an absolute path (from a native file picker) and return it as a
    /// workspace-relative path, rejecting anything outside the root. Symlinks
    /// resolve first, so a link inside the workspace pointing out is refused —
    /// same guarantee `resolve` gives for model-supplied paths.
    pub fn relativize(&self, absolute: &str) -> Result<String> {
        let real = std::fs::canonicalize(absolute)
            .map_err(|e| anyhow!("'{absolute}' is not accessible: {e}"))?;
        if !real.starts_with(&self.root) {
            return Err(anyhow!("'{absolute}' is outside the workspace"));
        }
        Ok(self.display_relative(&real))
    }

    /// Resolve a workspace-relative path, rejecting anything that escapes the
    /// root. An empty path resolves to the root itself.
    pub fn resolve(&self, rel: &str) -> Result<PathBuf> {
        let rel = rel.trim();
        if rel.is_empty() || rel == "." {
            return Ok(self.root.clone());
        }
        let joined = self.root.join(normalize_relative(Path::new(rel))?);

        match std::fs::canonicalize(&joined) {
            // Exists: symlinks are resolved; verify containment directly.
            Ok(real) if real.starts_with(&self.root) => Ok(real),
            Ok(_) => Err(anyhow!("path '{rel}' escapes the workspace")),
            // Does not exist yet (e.g. a new file): validate via the parent.
            Err(_) => {
                let parent = joined
                    .parent()
                    .ok_or_else(|| anyhow!("invalid path '{rel}'"))?;
                let real_parent = std::fs::canonicalize(parent).map_err(|_| {
                    anyhow!("parent directory of '{rel}' does not exist")
                })?;
                if !real_parent.starts_with(&self.root) {
                    return Err(anyhow!("path '{rel}' escapes the workspace"));
                }
                let name = joined
                    .file_name()
                    .ok_or_else(|| anyhow!("invalid path '{rel}'"))?;
                Ok(real_parent.join(name))
            }
        }
    }
}

/// Lexically normalize a relative path: drop `.`, reject `..`, and reject any
/// absolute/root/prefix component. This is a fast pre-filter; `resolve` still
/// canonicalizes to catch symlink escapes that survive lexical checks.
fn normalize_relative(p: &Path) -> Result<PathBuf> {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::Normal(c) => out.push(c),
            Component::CurDir => {}
            Component::ParentDir => return Err(anyhow!("'..' is not allowed in workspace paths")),
            Component::RootDir | Component::Prefix(_) => {
                return Err(anyhow!("absolute paths are not allowed"))
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_workspace() -> (tempdir_guard::TempDir, Workspace) {
        let dir = tempdir_guard::TempDir::new();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let ws = Workspace::new(dir.path().to_str().unwrap()).unwrap();
        (dir, ws)
    }

    #[test]
    fn resolves_paths_inside_root() {
        let (_g, ws) = temp_workspace();
        assert!(ws.resolve("a.txt").is_ok());
        assert!(ws.resolve("sub").is_ok());
        assert!(ws.resolve("sub/new.txt").is_ok(), "new file under existing dir");
        assert_eq!(ws.resolve("").unwrap(), *ws.root());
    }

    #[test]
    fn rejects_escapes() {
        let (_g, ws) = temp_workspace();
        assert!(ws.resolve("../secret").is_err());
        assert!(ws.resolve("sub/../../etc/passwd").is_err());
        assert!(ws.resolve("/etc/passwd").is_err());
        assert!(ws.resolve("nonexistent-dir/child.txt").is_err(), "missing parent");
    }

    #[test]
    fn rejects_symlink_escape() {
        let (dir, ws) = temp_workspace();
        // A symlink inside the workspace pointing outside must not be followed.
        #[cfg(unix)]
        {
            let outside = dir.path().parent().unwrap().join("outside.txt");
            std::fs::write(&outside, "secret").ok();
            let link = dir.path().join("link.txt");
            let _ = std::os::unix::fs::symlink(&outside, &link);
            if link.exists() {
                assert!(ws.resolve("link.txt").is_err(), "symlink escape allowed");
            }
        }
    }
}

/// Minimal self-cleaning temp directory for tests (avoids a tempfile dep).
#[cfg(test)]
mod tempdir_guard {
    use std::path::{Path, PathBuf};

    pub struct TempDir(PathBuf);

    impl TempDir {
        pub fn new() -> Self {
            let base = std::env::temp_dir().join(format!(
                "locallmos-ws-test-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir_all(&base).unwrap();
            // canonicalize so macOS /var -> /private/var matches resolve() output.
            Self(std::fs::canonicalize(&base).unwrap())
        }
        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
