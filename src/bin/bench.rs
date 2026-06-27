//! `bench` binary: drives the renderer harness over a fixture.
//!
//! Usage:
//!   bench --fixture <path.kfx> --backend stub [--duration 5] [--list-backends]

use std::env;
use std::process::ExitCode;

use knot::view::bench;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let cfg = match bench::parse_args(args.into_iter()) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("error: {e:#}");
            eprintln!(
                "usage: bench --fixture <path.kfx> --backend <name> \
                 [--duration <secs>] [--list-backends]"
            );
            return ExitCode::from(2);
        }
    };
    match bench::run(cfg) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("bench failed: {e:#}");
            ExitCode::FAILURE
        }
    }
}
