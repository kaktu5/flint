mod analyze;
mod flake;
mod output;
mod updates;

use pound::Parse;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use flake::lock::FlakeLock;
use flake::nix::FlakeNix;
use output::Options;

/// Analyze flake inputs for duplicates and check updates.
#[derive(Parse)]
#[pound(name = "flint")]
struct Args {
    /// Path to the directory containing a flake.
    #[pound(short = 'f', long, default = ".")]
    flake: PathBuf,

    /// Enable verbose output.
    #[pound(short, long)]
    verbose: bool,

    /// Exit with error if multiple versions found.
    #[pound(long)]
    fail_if_multiple_versions: bool,

    /// Output format: plain, pretty, or json.
    #[pound(short = 'o', long, default = "pretty")]
    output: String,

    /// Merge all dependants into one list for each input.
    #[pound(short, long)]
    merge: bool,

    /// Suppress all non-error output.
    #[pound(short, long)]
    quiet: bool,

    /// Check for available updates for flake inputs.
    #[pound(short = 'u', long)]
    check_updates: bool,

    /// Print version information.
    #[pound(long)]
    version: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();

    if args.version {
        println!("flint version {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }

    if output::no_color() {
        yansi::disable();
    }

    if !args.flake.is_dir() {
        eprintln!("Error: `{}` is not a directory", args.flake.display());
        return ExitCode::FAILURE;
    }

    let (flake_nix_src, flake_lock_src) = match read_flake_files(&args.flake) {
        Ok(files) => files,
        Err(err) => {
            eprintln!("{err}");
            return ExitCode::FAILURE;
        },
    };

    let nix: FlakeNix = match FlakeNix::from_str(&flake_nix_src) {
        Ok(nix) => nix,
        Err(err) => {
            eprintln!("error parsing flake.nix: {err}");
            return ExitCode::FAILURE;
        }
    };

    let lock: FlakeLock = match serde_json::from_str(&flake_lock_src) {
        Ok(lock) => lock,
        Err(err) => {
            eprintln!("error decoding flake.lock: {err}");
            return ExitCode::FAILURE;
        }
    };

    let options = Options {
        output_format: args.output,
        verbose: args.verbose,
        merge: args.merge,
        fail_if_multiple_versions: args.fail_if_multiple_versions,
        quiet: args.quiet,
    };

    if args.check_updates {
        let results = match updates::check_updates(&lock, options.verbose) {
            Ok(results) => results,
            Err(err) => {
                eprintln!("error checking updates: {err}");
                return ExitCode::FAILURE;
            }
        };
        if let Err(err) = output::print_updates(&results, &options) {
            eprintln!("Error: {err}");
            return ExitCode::FAILURE;
        }
        return ExitCode::SUCCESS;
    }

    let relations = match analyze::analyze_flake(&nix, &lock) {
        Ok(relations) => relations,
        Err(err) => {
            eprintln!("Error: {err}");
            return ExitCode::FAILURE;
        },
    };
    if let Err(err) = output::print_dependencies(
        &relations.deps,
        &relations.reverse_deps,
        &relations.warnings,
        &options
    )
    {
        eprintln!("Error: {err}");
        return ExitCode::FAILURE;
    }

    if output::should_fail_on_duplicates(&options, &relations.deps) {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn read_flake_files(dir: &Path) -> Result<(String, String), String> {
    let nix_path = dir.join("flake.nix");
    let lock_path = dir.join("flake.lock");

    let nix = fs::read_to_string(&nix_path)
        .map_err(|e| format!("error reading {}: {e}", nix_path.display()))?;
    let lock = fs::read_to_string(&lock_path)
        .map_err(|e| format!("error reading {}: {e}", lock_path.display()))?;

    Ok((nix, lock))
}
