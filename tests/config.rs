use std::fs;
use std::path::Path;

use tempfile::TempDir;
use wt_janitor::config::{
    load_config, prune_min_age_days, select_repos, write_example_config, DEFAULT_DEPENDENCY_DIRS,
    DEFAULT_INACTIVE_DAYS,
};

const VALID: &str = r#"
inactive_days = 7

[cleanup]
dependency_dirs = ["node_modules", ".venv"]
prune_min_age_days = 3

[[repos]]
name = "web"
path = "~/Projects/web"

[[repos]]
name = "api"
path = "/tmp/api"
"#;

fn write_config(temp: &TempDir, text: &str) -> std::path::PathBuf {
    let path = temp.path().join("config.toml");
    fs::write(&path, text).unwrap();
    path
}

#[test]
fn loads_valid_config_and_expands_home() {
    let temp = tempfile::tempdir().unwrap();
    let path = write_config(&temp, VALID);
    let config = load_config(&path).unwrap();

    assert_eq!(config.path, path);
    assert_eq!(config.inactive_days, 7);
    assert_eq!(config.dependency_dirs, ["node_modules", ".venv"]);
    assert_eq!(config.prune_min_age_days, Some(3));
    assert_eq!(
        config
            .repos
            .iter()
            .map(|repo| repo.name.as_str())
            .collect::<Vec<_>>(),
        ["web", "api"]
    );
    assert_eq!(config.repos[0].raw_path, "~/Projects/web");
    assert!(config.repos[0].path.ends_with("Projects/web"));
    assert_eq!(config.repos[1].path, Path::new("/tmp/api"));
}

#[test]
fn applies_conservative_defaults() {
    let temp = tempfile::tempdir().unwrap();
    let path = write_config(&temp, "[[repos]]\nname = \"x\"\npath = \"/tmp\"\n");
    let config = load_config(&path).unwrap();

    assert_eq!(config.inactive_days, DEFAULT_INACTIVE_DAYS);
    assert_eq!(config.dependency_dirs, DEFAULT_DEPENDENCY_DIRS);
    assert_eq!(config.prune_min_age_days, None);
    assert_eq!(prune_min_age_days(&config), DEFAULT_INACTIVE_DAYS);
}

#[test]
fn explicit_prune_age_overrides_inactive_age() {
    let temp = tempfile::tempdir().unwrap();
    let config = load_config(&write_config(
        &temp,
        "inactive_days = 30\n[cleanup]\nprune_min_age_days = 0\n",
    ))
    .unwrap();

    assert_eq!(prune_min_age_days(&config), 0);
}

#[test]
fn rejects_invalid_configuration_values_with_actionable_messages() {
    let cases = [
        ("inactive_days = 0\n", "inactive_days"),
        ("inactive_days = \"7\"\n", "inactive_days"),
        ("inactive_days = true\n", "inactive_days"),
        (
            "[cleanup]\ndependency_dirs = \"node_modules\"\n",
            "dependency_dirs",
        ),
        (
            "[cleanup]\ndependency_dirs = [\"../escape\"]\n",
            "dependency_dirs",
        ),
        (
            "[cleanup]\ndependency_dirs = [\"a/b\"]\n",
            "dependency_dirs",
        ),
        (
            "[cleanup]\ndependency_dirs = [\"a\\\\b\"]\n",
            "dependency_dirs",
        ),
        (
            "[cleanup]\ndependency_dirs = [\" node_modules\"]\n",
            "dependency_dirs",
        ),
        (
            "[cleanup]\nprune_min_age_days = -1\n",
            "prune_min_age_days",
        ),
        ("[[repos]]\nname = \"\"\npath = \"/tmp\"\n", "name"),
        ("[[repos]]\nname = \"a\"\npath = \"\"\n", "path"),
        (
            "[[repos]]\nname = \"a\"\npath = \"/tmp\"\n[[repos]]\nname = \"a\"\npath = \"/other\"\n",
            "duplicate",
        ),
        ("cleanup = 3\n", "cleanup"),
        ("repos = 3\n", "repos"),
    ];

    for (index, (text, fragment)) in cases.into_iter().enumerate() {
        let temp = tempfile::tempdir().unwrap();
        let error = load_config(&write_config(&temp, text))
            .expect_err(&format!("case {index} unexpectedly parsed"));
        assert!(
            error.to_string().contains(fragment),
            "case {index}: expected {fragment:?} in {error}"
        );
    }
}

#[test]
fn reports_invalid_toml_and_missing_files() {
    let temp = tempfile::tempdir().unwrap();
    let invalid = load_config(&write_config(&temp, "not [valid toml =")).unwrap_err();
    assert!(invalid.to_string().contains("invalid TOML"));

    let missing = load_config(&temp.path().join("missing.toml")).unwrap_err();
    assert!(missing.to_string().contains("not found"));
    assert!(missing.to_string().contains("wt-janitor init"));
}

#[test]
fn selects_all_requested_or_reports_every_unknown_repo() {
    let temp = tempfile::tempdir().unwrap();
    let config = load_config(&write_config(&temp, VALID)).unwrap();

    let all = select_repos(&config, &[]).unwrap();
    assert_eq!(all.len(), 2);

    let selected = select_repos(&config, &["api".into()]).unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].name, "api");

    let error = select_repos(&config, &["missing-a".into(), "missing-b".into()]).unwrap_err();
    let message = error.to_string();
    assert!(message.contains("missing-a"));
    assert!(message.contains("missing-b"));
    assert!(message.contains("web"));
}

#[test]
fn example_config_is_created_once_and_is_parseable() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("nested/config.toml");

    assert!(write_example_config(&target).unwrap());
    let before = fs::read_to_string(&target).unwrap();
    let parsed = load_config(&target).unwrap();
    assert!(!parsed.repos.is_empty());

    assert!(!write_example_config(&target).unwrap());
    assert_eq!(fs::read_to_string(target).unwrap(), before);
}
