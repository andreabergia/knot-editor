//! Production extension pool benchmark.
//!
//! Usage: `v8-bench [--samples N] [--workers N] [--stress]`

use std::process::ExitCode;

fn main() -> ExitCode {
    match knot::host::bench::run(std::env::args().skip(1)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("v8-bench failed: {error:#}");
            ExitCode::FAILURE
        }
    }
}
