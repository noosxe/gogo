//! `--check-skin` (design 25 §8, design 27 §8): load a skin folder and print what the
//! loader found.

use std::path::Path;

use gogo_skin::{Finding, Report, SkinPackage};

/// One line per error: where, the code and the message.
pub(crate) fn error_lines(report: &Report) -> Vec<String> {
    report
        .errors()
        .map(|f| format!("{}: {} ({})", f.location, f.message, f.code))
        .collect()
}

fn print(finding: &Finding) {
    eprintln!("{:?}", miette::Report::new(finding.clone()));
}

/// Prints the findings to stderr and a summary to stdout. `true` when there are no errors;
/// warnings alone fail nothing.
pub(crate) fn run(dir: &Path) -> bool {
    let (report, loaded) = match SkinPackage::load(dir) {
        Ok(package) => (package.report().clone(), true),
        Err(report) => (report, false),
    };
    for finding in report.findings() {
        print(finding);
    }
    if report.suppressed() > 0 {
        eprintln!("{} more findings are not shown", report.suppressed());
    }
    let ok = loaded && report.error_total() == 0;
    println!(
        "{}: {} errors, {} warnings{}",
        dir.display(),
        report.error_total(),
        report.warning_total(),
        if loaded {
            ""
        } else {
            "; the skin does not load"
        }
    );
    ok
}
