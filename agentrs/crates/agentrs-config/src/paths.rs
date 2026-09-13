// Cross-platform path containment.
//
// Lives here, below every crate that needs it, because the platform quirks it
// papers over are the same wherever the question is asked: memory-directory
// membership, sub-agent write confinement, and anything else that has to
// decide whether one path sits under another.

use std::fs;
use std::path::{Component, Path, PathBuf};

/// Whether `path` resolves to a location inside `root`.
///
/// Both sides are canonicalized so `..` segments and symlinks cannot be used
/// to step outside `root` while still looking like a child of it.
///
/// A path that does not exist yet resolves through its nearest existing
/// ancestor. That case is the common one — containment is usually checked
/// *before* a file is written — and leaving it purely lexical would break the
/// comparison outright: on Windows canonicalization yields a verbatim
/// `\\?\C:\...` prefix and on macOS it rewrites `/var` to `/private/var`, so a
/// lexical child never appears to live under its own canonical parent.
///
/// Returns `false` when either side cannot be resolved, and when `path`
/// contains `..` segments that no filesystem state can safely settle.
pub fn is_within(path: &Path, root: &Path) -> bool {
    let (Some(path), Some(root)) = (resolve(path), resolve(root)) else {
        return false;
    };
    path.starts_with(&root)
}

/// Resolve `path` as far as the filesystem allows.
///
/// Returns `None` when the path does not exist and contains `..`, since
/// lexical normalization cannot settle a parent reference without knowing what
/// the intervening directories actually are.
pub fn resolve(path: &Path) -> Option<PathBuf> {
    if let Ok(canonical) = fs::canonicalize(path) {
        return Some(canonical);
    }
    if contains_traversal(path) {
        return None;
    }
    let normalized = normalize_lexical(path);
    if normalized.as_os_str().is_empty() {
        return None;
    }
    Some(canonicalize_existing_ancestor(&normalized).unwrap_or(normalized))
}

/// Canonicalize the nearest ancestor that exists, then re-attach the tail.
fn canonicalize_existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut tail = Vec::new();
    let mut cursor = path;
    while let Some(parent) = cursor.parent() {
        tail.push(cursor.file_name()?);
        if let Ok(canonical) = fs::canonicalize(parent) {
            let mut resolved = canonical;
            resolved.extend(tail.iter().rev());
            return Some(resolved);
        }
        cursor = parent;
    }
    None
}

/// Collapse `.` and redundant separators without touching `..`.
///
/// Purely textual: no filesystem access, no symlink resolution. Callers that
/// have already rejected `..` use this to tidy a path without committing to
/// what is currently on disk.
pub fn normalize_lexical(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            _ => result.push(component),
        }
    }
    result
}

fn contains_traversal(path: &Path) -> bool {
    path.components().any(|component| component == Component::ParentDir)
}

#[cfg(test)]
#[path = "paths_test.rs"]
mod paths_test;
