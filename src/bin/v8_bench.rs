//! `v8-bench` binary: Step 7 runtime boundary evidence.
//!
//! Usage: `v8-bench [--samples <n>]`

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
