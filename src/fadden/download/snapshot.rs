// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: Apache-2.0

//! `snapshot_paths`, `rollback_snapshots` and `discard_snapshots` from
//! `fadden/download.py`: the whole-run journal that restores every changed
//! EPUB, sidecar and manifest when a later step fails.
use std::path::Path;

use crate::fadden::Exc;
use crate::pyos::{OsError, Style};

/// Whole-run backup state for rollback on failure.
#[derive(Debug, Clone)]
pub struct Snapshot {
    entries: Vec<(String, String, bool)>,
}

/// Back up a group of paths for whole-run rollback.
pub fn snapshot_paths(paths: &[&str]) -> Result<Snapshot, Exc> {
    let mut entries = Vec::with_capacity(paths.len());
    for &path in paths {
        let backup = format!("{path}.rollback");
        if Path::new(&backup).exists() {
            return Err(Exc::Runtime(format!("unfinished rollback file: {backup}")));
        }
        entries.push((path.to_string(), backup, Path::new(path).exists()));
    }
    for (path, backup, existed) in &entries {
        if *existed && let Err(err) = copy_file(path, backup) {
            for (_, b, _) in &entries {
                if Path::new(b).exists() {
                    let _ = std::fs::remove_file(b);
                }
            }
            return Err(err);
        }
    }
    Ok(Snapshot { entries })
}

fn copy_file(source: &str, destination: &str) -> Result<(), Exc> {
    std::fs::copy(source, destination).map_err(|e| OsError::new(&e, Some(source), Style::Errno))?;
    if let Ok(meta) = std::fs::metadata(source) {
        let _ = std::fs::set_permissions(destination, meta.permissions());
    }
    Ok(())
}

/// Restore every path changed since the prior manifest was read.
pub fn rollback_snapshots(snapshots: &[Snapshot]) -> Result<(), Exc> {
    for snapshot in snapshots.iter().rev() {
        let new_pair_written = snapshot
            .entries
            .iter()
            .all(|(path, _, existed)| !existed && Path::new(path).exists());
        for (path, backup, existed) in snapshot.entries.iter().rev() {
            if *existed {
                if !Path::new(backup).exists() {
                    return Err(Exc::Runtime(format!("missing rollback file: {backup}")));
                }
                crate::pyos::replace(backup, path)?;
            } else if Path::new(path).exists() && !new_pair_written {
                std::fs::remove_file(path)
                    .map_err(|e| OsError::new(&e, Some(path), Style::Errno))?;
            }
        }
        for (path, backup, _) in &snapshot.entries {
            for transient in [backup.as_str(), &format!("{path}.part")] {
                if Path::new(transient).exists() {
                    std::fs::remove_file(transient)
                        .map_err(|e| OsError::new(&e, Some(transient), Style::Errno))?;
                }
            }
        }
    }
    Ok(())
}

/// Remove rollback copies after the new manifest commits; returns any paths that
/// could not be deleted.
pub fn discard_snapshots(snapshots: &[Snapshot]) -> Vec<String> {
    let mut leftovers = Vec::new();
    for snapshot in snapshots {
        for (_, backup, _) in &snapshot.entries {
            if Path::new(backup).exists() && std::fs::remove_file(backup).is_err() {
                leftovers.push(backup.clone());
            }
        }
    }
    leftovers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_cleans_up_on_copy_error_and_discard_reports_leftovers() {
        let dir = tempfile::tempdir().unwrap();
        let f1 = dir.path().join("a.epub");
        let d2 = dir.path().join("sub");
        std::fs::write(&f1, b"ok").unwrap();
        std::fs::create_dir(&d2).unwrap();
        let (p1, p2) = (
            f1.to_string_lossy().into_owned(),
            d2.to_string_lossy().into_owned(),
        );
        assert!(snapshot_paths(&[&p1, &p2]).is_err());
        assert!(!Path::new(&format!("{p1}.rollback")).exists());

        let snap = snapshot_paths(&[&p1]).unwrap();
        let backup = format!("{p1}.rollback");
        std::fs::remove_file(&backup).unwrap();
        std::fs::create_dir(&backup).unwrap();
        std::fs::write(Path::new(&backup).join("keep"), b"x").unwrap();
        assert_eq!(discard_snapshots(&[snap]), vec![backup]);
    }
}
