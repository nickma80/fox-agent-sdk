//! Task assertions: verify agent task completion via objective world-state checks.
//!
//! Unlike GoldenTranscript (which checks the call path), `TaskAssertions` checks
//! the **outcome** — file contents, command exit codes, directory existence.
//! This is the "physical evidence" layer of the evaluation system.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Assertions about the world state after an agent completes a task.
///
/// Agent correctness is determined by objective facts, not by the specific
/// sequence of tool calls it made. This struct encodes those facts.
#[derive(Debug, Clone)]
pub struct TaskAssertions {
    /// Files that must exist (relative to working_dir, or absolute).
    pub file_exists: Vec<PathBuf>,

    /// File → substring that must appear in the file.
    pub file_contains: Vec<(PathBuf, String)>,

    /// File → substring that must NOT appear in the file.
    pub file_not_contains: Vec<(PathBuf, String)>,

    /// Directories that must exist.
    pub dir_exists: Vec<PathBuf>,

    /// Commands to run and their expected outcomes.
    pub commands: Vec<CommandAssertion>,

    /// Maximum allowed wall-clock duration for the task.
    /// Checked by the harness, not by this struct.
    pub max_duration_secs: Option<u64>,
}

/// A command to execute and its expected outcome.
#[derive(Debug, Clone)]
pub struct CommandAssertion {
    /// Working directory for the command.
    pub working_dir: PathBuf,

    /// The command to run (passed to the platform shell).
    pub command: String,

    /// Expected exit code (typically 0 for success).
    pub expected_exit_code: i32,

    /// Optional substring that must appear in stdout.
    pub stdout_contains: Option<String>,

    /// Optional substring that must NOT appear in stderr.
    pub stderr_not_contains: Option<String>,
}

/// Result of running all task assertions.
#[derive(Debug, Clone)]
pub struct AssertionReport {
    /// Overall pass/fail.
    pub passed: bool,

    /// Total assertions checked.
    pub total: usize,

    /// Number passed.
    pub passed_count: usize,

    /// Individual failure messages.
    pub failures: Vec<String>,
}

impl AssertionReport {
    pub fn new() -> Self {
        Self { passed: true, total: 0, passed_count: 0, failures: Vec::new() }
    }

    fn record(&mut self, passed: bool, msg: impl Into<String>) {
        self.total += 1;
        if passed {
            self.passed_count += 1;
        } else {
            self.passed = false;
            self.failures.push(msg.into());
        }
    }
}

impl Default for AssertionReport {
    fn default() -> Self { Self::new() }
}

impl std::fmt::Display for AssertionReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let status = if self.passed { "PASSED" } else { "FAILED" };
        writeln!(f, "╔══ Task Assertions: {} ({}/{})", status, self.passed_count, self.total)?;
        if !self.failures.is_empty() {
            writeln!(f, "╠══ Failures:")?;
            for fail in &self.failures {
                writeln!(f, "║   ✗ {}", fail)?;
            }
        }
        writeln!(f, "╚══")?;
        Ok(())
    }
}

/// Execute all [`TaskAssertions`] against a working directory.
///
/// Returns a detailed [`AssertionReport`].
pub fn run_task_assertions(assertions: &TaskAssertions, working_dir: &Path) -> AssertionReport {
    let mut report = AssertionReport::new();

    // ── file_exists ──
    for path in &assertions.file_exists {
        let resolved = resolve(path, working_dir);
        let exists = resolved.exists();
        report.record(exists, format!("file_exists: {} — {}", path.display(), if exists { "OK" } else { "MISSING" }));
    }

    // ── dir_exists ──
    for path in &assertions.dir_exists {
        let resolved = resolve(path, working_dir);
        let exists = resolved.is_dir();
        report.record(exists, format!("dir_exists: {} — {}", path.display(), if exists { "OK" } else { "MISSING" }));
    }

    // ── file_contains ──
    for (path, expected) in &assertions.file_contains {
        let resolved = resolve(path, working_dir);
        match std::fs::read_to_string(&resolved) {
            Ok(content) => {
                let found = content.contains(expected.as_str());
                report.record(found, format!(
                    "file_contains: {} should contain \"{}\" — {}",
                    path.display(), expected, if found { "OK" } else { "NOT FOUND" }
                ));
            }
            Err(e) => {
                report.record(false, format!("file_contains: {} — failed to read: {}", path.display(), e));
            }
        }
    }

    // ── file_not_contains ──
    for (path, forbidden) in &assertions.file_not_contains {
        let resolved = resolve(path, working_dir);
        match std::fs::read_to_string(&resolved) {
            Ok(content) => {
                let found = content.contains(forbidden.as_str());
                report.record(!found, format!(
                    "file_not_contains: {} should NOT contain \"{}\" — {}",
                    path.display(), forbidden, if !found { "OK" } else { "FOUND (violation)" }
                ));
            }
            Err(e) => {
                report.record(false, format!("file_not_contains: {} — failed to read: {}", path.display(), e));
            }
        }
    }

    // ── commands ──
    for cmd in &assertions.commands {
        let resolved_dir = resolve(&cmd.working_dir, working_dir);
        let output = run_command(&cmd.command, &resolved_dir);

        // Exit code check
        let exit_ok = output.exit_code == Some(cmd.expected_exit_code);
        report.record(exit_ok, format!(
            "command exit_code: \"{}\" — expected {}, got {:?} — {}",
            cmd.command, cmd.expected_exit_code, output.exit_code,
            if exit_ok { "OK" } else { "MISMATCH" }
        ));

        // stdout_contains
        if let Some(ref expected) = cmd.stdout_contains {
            let found = output.stdout.contains(expected.as_str());
            report.record(found, format!(
                "command stdout_contains: \"{}\" — \"{}\" — {}",
                cmd.command, expected, if found { "OK" } else { "NOT FOUND" }
            ));
        }

        // stderr_not_contains
        if let Some(ref forbidden) = cmd.stderr_not_contains {
            let found = output.stderr.contains(forbidden.as_str());
            report.record(!found, format!(
                "command stderr_not_contains: \"{}\" — \"{}\" — {}",
                cmd.command, forbidden, if !found { "OK" } else { "FOUND (violation)" }
            ));
        }
    }

    report
}

/// Result of running a shell command.
#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

fn run_command(cmd_str: &str, working_dir: &Path) -> CommandOutput {
    #[cfg(windows)]
    let output = Command::new("cmd.exe")
        .args(["/C", cmd_str])
        .current_dir(working_dir)
        .output();
    #[cfg(not(windows))]
    let output = Command::new("sh")
        .args(["-c", cmd_str])
        .current_dir(working_dir)
        .output();

    match output {
        Ok(o) => CommandOutput {
            exit_code: o.status.code(),
            stdout: String::from_utf8_lossy(&o.stdout).to_string(),
            stderr: String::from_utf8_lossy(&o.stderr).to_string(),
        },
        Err(e) => CommandOutput {
            exit_code: None,
            stdout: String::new(),
            stderr: format!("failed to execute: {}", e),
        },
    }
}

/// Resolve a path: if it's relative, join with working_dir.
fn resolve(path: &Path, working_dir: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        working_dir.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_exists_pass() {
        let dir = std::env::temp_dir().join("fox-tmp-task-assertions");
        let _ = std::fs::create_dir_all(&dir);
        let f = dir.join("hello.txt");
        std::fs::write(&f, "world").unwrap();

        let assertions = TaskAssertions {
            file_exists: vec![PathBuf::from("hello.txt")],
            file_contains: vec![(PathBuf::from("hello.txt"), "world".into())],
            file_not_contains: vec![(PathBuf::from("hello.txt"), "nope".into())],
            dir_exists: vec![],
            commands: vec![],
            max_duration_secs: None,
        };

        let report = run_task_assertions(&assertions, &dir);
        println!("{report}");
        assert!(report.passed);
    }

    #[test]
    fn test_file_missing_fails() {
        let dir = std::env::temp_dir().join("fox-tmp-missing");
        let _ = std::fs::create_dir_all(&dir);

        let assertions = TaskAssertions {
            file_exists: vec![PathBuf::from("nope.txt")],
            file_contains: vec![],
            file_not_contains: vec![],
            dir_exists: vec![],
            commands: vec![],
            max_duration_secs: None,
        };

        let report = run_task_assertions(&assertions, &dir);
        println!("{report}");
        assert!(!report.passed);
        assert!(!report.failures.is_empty());
    }
}
