use std::fs;

use wt_janitor::sizes::{find_dependency_dirs, walk_tree};

#[test]
fn zero_budget_marks_wide_walks_incomplete() {
    let temp = tempfile::tempdir().unwrap();
    for index in 0..100 {
        fs::write(temp.path().join(format!("{index}.txt")), b"x").unwrap();
    }

    let walk = walk_tree(temp.path(), &[], 0.0);
    assert!(walk.incomplete);
}

#[test]
fn partial_dependency_measurement_marks_the_scan_incomplete() {
    let temp = tempfile::tempdir().unwrap();
    let dependency = temp.path().join("node_modules");
    fs::create_dir(&dependency).unwrap();
    for index in 0..100 {
        fs::write(dependency.join(format!("{index}.js")), b"x").unwrap();
    }

    let scan = find_dependency_dirs(temp.path(), &["node_modules".into()], 0.0);
    assert!(scan.incomplete);
}

#[cfg(unix)]
#[test]
fn dependency_scan_never_follows_symlinks() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("precious"), b"data").unwrap();
    symlink(outside.path(), temp.path().join("node_modules")).unwrap();

    let scan = find_dependency_dirs(temp.path(), &["node_modules".into()], 1.0);
    assert_eq!(scan.targets.len(), 1);
    assert!(scan.targets[0].is_symlink);
    assert_eq!(scan.targets[0].size_bytes, 0);
}
