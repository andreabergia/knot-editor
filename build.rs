//! Compiles the bundled TypeScript extension packages under `extensions/@knot`
//! with the lockfile-pinned compiler in `scripts/extensions`, and generates the
//! table that `src/app/bundled.rs` embeds into the binary.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let tools = root.join("scripts/extensions");
    let bundled = root.join("extensions/@knot");

    println!("cargo::rerun-if-changed=extensions/@knot");
    println!("cargo::rerun-if-changed=scripts/extensions/package-lock.json");
    println!("cargo::rerun-if-changed=scripts/extensions/tsconfig.base.json");
    println!("cargo::rerun-if-changed=src/host/js/knot.d.ts");

    install_tools(&tools);

    let mut table = String::from("const PACKAGES: &[BundledSource] = &[\n");
    for package in sorted_entries(&bundled)
        .into_iter()
        .filter(|path| path.is_dir())
    {
        let name = package.file_name().unwrap().to_str().unwrap();
        let dist = out.join("extensions").join(name).join("dist");
        if dist.exists() {
            fs::remove_dir_all(&dist).unwrap();
        }
        compile(&tools, &package, &dist);

        let mut modules = Vec::new();
        collect_modules(&dist, &dist, &mut modules);
        writeln!(
            table,
            "    BundledSource {{\n        manifest: include_str!({:?}),\n        sources: &[",
            package.join("knot.jsonc").to_str().unwrap()
        )
        .unwrap();
        for (relative, path) in modules {
            writeln!(
                table,
                "            ({:?}, include_str!({:?})),",
                format!("dist/{relative}"),
                path.to_str().unwrap()
            )
            .unwrap();
        }
        table.push_str("        ],\n    },\n");
    }
    table.push_str("];\n");
    fs::write(out.join("bundled_packages.rs"), table).unwrap();
}

/// Installs the pinned toolchain when it is missing or older than the lockfile.
fn install_tools(tools: &Path) {
    let lockfile = tools.join("package-lock.json");
    let installed = tools.join("node_modules/.package-lock.json");
    let modified = |path: &Path| {
        fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .ok()
    };
    if modified(&installed).is_some_and(|time| Some(time) >= modified(&lockfile)) {
        return;
    }
    let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
    run(
        Command::new(npm)
            .args(["ci", "--ignore-scripts", "--no-audit", "--no-fund"])
            .current_dir(tools),
        "npm ci",
    );
}

fn compile(tools: &Path, package: &Path, dist: &Path) {
    run(
        Command::new("node")
            .arg(tools.join("node_modules/typescript/bin/tsc"))
            .arg("-p")
            .arg(package.join("tsconfig.json"))
            .arg("--outDir")
            .arg(dist),
        &format!("tsc for {}", package.display()),
    );
}

fn run(command: &mut Command, description: &str) {
    let output = command.output().unwrap_or_else(|error| {
        panic!("cannot run {description}: {error}; building Knot requires Node.js and npm")
    });
    if !output.status.success() {
        panic!(
            "{description} failed:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn collect_modules(root: &Path, directory: &Path, modules: &mut Vec<(String, PathBuf)>) {
    for path in sorted_entries(directory) {
        if path.is_dir() {
            collect_modules(root, &path, modules);
        } else if path.extension().is_some_and(|extension| extension == "js") {
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .components()
                .map(|part| part.as_os_str().to_str().unwrap())
                .collect::<Vec<_>>()
                .join("/");
            modules.push((relative, path));
        }
    }
}

fn sorted_entries(directory: &Path) -> Vec<PathBuf> {
    let mut entries = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    entries.sort();
    entries
}
