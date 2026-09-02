//! Finding a project's test command without being told it.
//!
//! The run-tests step takes an explicit command, but a step that has to be
//! configured before it does anything cannot ship in the default flow — it
//! would arrive broken, with a required setting nobody filled in. So the
//! command is optional, and this works it out from what is lying in the
//! repository root when it is left empty.
//!
//! Detection is deliberately dull: one marker file, one command, and the node
//! log always names the marker it matched. A guess you can see the reasoning
//! for is worth far more than a cleverer guess you cannot.

use std::path::Path;

#[derive(Clone, PartialEq, Debug)]
pub struct Suite {
    /// What to run, through `sh -c`, in the repository root.
    pub command: String,
    /// The file that says so, quoted back so the choice is never mysterious.
    pub why: String,
}

/// The test command for `repo`, or `None` when nothing recognisable is there.
pub fn detect(repo: &Path) -> Option<Suite> {
    from_files(
        |name| repo.join(name).exists(),
        |name| std::fs::read_to_string(repo.join(name)).ok(),
    )
}

/// The detection itself, with the filesystem passed in so it can be tested
/// without one.
///
/// Ordering is precedence: a `Makefile` target comes first because it is the
/// one entry point a project wrote down on purpose, and everything after it is
/// the conventional command for a language whose marker file is present.
fn from_files(
    exists: impl Fn(&str) -> bool,
    read: impl Fn(&str) -> Option<String>,
) -> Option<Suite> {
    let found = |command: &str, why: &str| {
        Some(Suite {
            command: command.to_string(),
            why: why.to_string(),
        })
    };

    if read("Makefile").is_some_and(|m| has_make_target(&m, "test")) {
        return found("make test", "a `test` target in Makefile");
    }
    if exists("Cargo.toml") {
        return found("cargo test", "Cargo.toml");
    }
    if exists("go.mod") {
        return found("go test ./...", "go.mod");
    }
    if let Some(script) = read("package.json").as_deref().and_then(npm_test_script) {
        return found("npm test", &format!("`{script}` in package.json"));
    }
    for marker in ["pyproject.toml", "pytest.ini", "tox.ini"] {
        if exists(marker) {
            return found("pytest", marker);
        }
    }
    if exists("pom.xml") {
        return found("mvn test", "pom.xml");
    }
    for marker in ["build.gradle", "build.gradle.kts"] {
        if exists(marker) {
            return found("./gradlew test", marker);
        }
    }
    None
}

/// A rule named `target` at the start of a line. `.PHONY: test` names the
/// target without being one, so a plain substring search finds a Makefile that
/// cannot actually run it.
fn has_make_target(makefile: &str, target: &str) -> bool {
    makefile.lines().any(|line| {
        line.strip_prefix(target)
            .map(str::trim_start)
            // `:` starts a rule; `::=` and `:=` are variable assignments.
            .is_some_and(|rest| rest.starts_with(':') && !rest.starts_with(":="))
    })
}

/// The `scripts.test` entry from a package.json, unless it is the placeholder
/// `npm init` writes, which exits 1 and has never run a test.
fn npm_test_script(package: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(package).ok()?;
    let script = value.get("scripts")?.get("test")?.as_str()?.trim();
    if script.is_empty() || script.contains("no test specified") {
        return None;
    }
    Some(script.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A repository with `names` in its root and nothing else.
    fn repo(files: &[(&str, &str)]) -> Option<Suite> {
        let files = files.to_vec();
        from_files(
            |name| files.iter().any(|(n, _)| *n == name),
            |name| {
                files
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, body)| body.to_string())
            },
        )
    }

    #[test]
    fn a_rust_project_runs_cargo_test() {
        let suite = repo(&[("Cargo.toml", "[package]")]).unwrap();
        assert_eq!(suite.command, "cargo test");
        assert_eq!(suite.why, "Cargo.toml");
    }

    #[test]
    fn the_filesystem_adapter_finds_this_very_repository() {
        // `from_files` is where the rules live, but something has to prove the
        // thin layer that reads the disk is wired to it.
        let here = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert_eq!(detect(here).unwrap().command, "cargo test");
    }

    #[test]
    fn a_repository_with_nothing_recognisable_says_so() {
        assert_eq!(repo(&[("README.md", "hi")]), None);
    }

    #[test]
    fn a_makefile_target_wins_over_the_language_default() {
        // Writing `make test` down is a decision; `cargo test` is a guess.
        let suite = repo(&[
            ("Cargo.toml", "[package]"),
            (
                "Makefile",
                "build:\n\tcargo build\ntest:\n\tcargo test --all\n",
            ),
        ])
        .unwrap();
        assert_eq!(suite.command, "make test");
    }

    #[test]
    fn phony_alone_is_not_a_target() {
        // `.PHONY: test` names the target without defining it — a substring
        // search would run `make test` against a Makefile that has no rule.
        let suite = repo(&[
            ("Cargo.toml", "[package]"),
            ("Makefile", ".PHONY: test\nbuild:\n\tcargo build\n"),
        ])
        .unwrap();
        assert_eq!(suite.command, "cargo test");
    }

    #[test]
    fn a_variable_called_test_is_not_a_target() {
        let suite = repo(&[("Cargo.toml", "[package]"), ("Makefile", "test := yes\n")]).unwrap();
        assert_eq!(suite.command, "cargo test");
    }

    #[test]
    fn a_node_project_with_a_real_test_script_runs_npm_test() {
        let suite = repo(&[("package.json", r#"{"scripts":{"test":"jest"}}"#)]).unwrap();
        assert_eq!(suite.command, "npm test");
    }

    #[test]
    fn the_placeholder_npm_init_writes_is_not_a_test_suite() {
        // `npm test` here exits 1, which would fail every run in a repository
        // that has simply never set tests up.
        let package = r#"{"scripts":{"test":"echo \"Error: no test specified\" && exit 1"}}"#;
        assert_eq!(repo(&[("package.json", package)]), None);
    }

    #[test]
    fn a_package_json_with_no_scripts_at_all_is_not_a_test_suite() {
        assert_eq!(repo(&[("package.json", r#"{"name":"x"}"#)]), None);
        assert_eq!(repo(&[("package.json", "not json")]), None);
    }

    #[test]
    fn python_go_maven_and_gradle_are_each_recognised() {
        assert_eq!(
            repo(&[("go.mod", "module x")]).unwrap().command,
            "go test ./..."
        );
        assert_eq!(repo(&[("pyproject.toml", "")]).unwrap().command, "pytest");
        assert_eq!(repo(&[("tox.ini", "")]).unwrap().command, "pytest");
        assert_eq!(repo(&[("pom.xml", "")]).unwrap().command, "mvn test");
        assert_eq!(
            repo(&[("build.gradle.kts", "")]).unwrap().command,
            "./gradlew test"
        );
    }
}
