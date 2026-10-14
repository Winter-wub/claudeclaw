//! The lint hook (architecture §4 and §8).

use crate::diagnostic::JobDiagnostic;
use crate::file::JobFile;

/// An extra check on a parsed job file, run by [`JobFile::parse_with`].
///
/// This is how `ts-compat` adds the 1.x divergence lint (#12 §4) without `jobs` knowing about
/// TS: the binary registers it, and it goes away when `main` switches to Rust (architecture §8).
/// A lint sees the whole file, so it can compare its own reading of the text with
/// [`JobFile::fields`]. It usually returns warnings; an error makes the job invalid.
///
/// Kinds must follow `claudeclaw_api::jobs::JobDiagnosticKind`'s grammar (dotted `snake_case`,
/// at most 64 bytes) and should use the lint's own prefix, such as `divergence.schedule`.
pub trait Lint: Send + Sync {
    /// The findings on `job`; empty when there are none.
    fn check(&self, job: &JobFile) -> Vec<JobDiagnostic>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostic::Severity;

    /// Stands in for `ts-compat`'s lint: flags a day-of-month `*/n`, which 1.x reads as modulo.
    struct DomStep;

    impl Lint for DomStep {
        fn check(&self, job: &JobFile) -> Vec<JobDiagnostic> {
            let Some(s) = &job.fields().schedule else {
                return Vec::new();
            };
            match s.as_str().split_whitespace().nth(2) {
                Some(dom) if dom.starts_with("*/") => vec![
                    JobDiagnostic::warning("divergence.schedule", "1.x fires on other days")
                        .at(2, None),
                ],
                _ => Vec::new(),
            }
        }
    }

    #[test]
    fn lints_append_findings() {
        let text = b"---\nschedule: \"0 9 */3 * *\"\nrecurring: true\n---\n";
        let lints: [&dyn Lint; 1] = [&DomStep];
        let job = JobFile::parse_with(text, &lints);
        assert_eq!(job.diagnostics().len(), 1);
        assert_eq!(job.diagnostics()[0].kind, "divergence.schedule");
        assert_eq!(job.diagnostics()[0].severity, Severity::Warning);
        assert!(job.is_valid());
        assert!(JobFile::parse(text).diagnostics().is_empty());
    }
}
