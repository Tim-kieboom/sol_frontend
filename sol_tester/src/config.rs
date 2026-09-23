use std::{
    path::{Path, PathBuf},
    sync::LazyLock,
};

use sol_displayer::PrintConfigs;
use sol_utils::{
    compiler_options::{CompilerOptions, MirOptions, PanicMode, PlatformInfo},
    fault::Severity,
};

pub static CONFIG: LazyLock<Configs> = LazyLock::new(parse_config);

const MIR_OPTIONS: MirOptions = MirOptions::empty()
    .add(MirOptions::CHECK_ALGORITHMIC_OVERFLOW)
    .add(MirOptions::CHECK_INDEX_OUT_OF_BOUNDS);

/// A `LazyLock` rather than a plain `const` — `panic_mode` can be overridden
/// by a `--panic-mode=<abort|exit>` CLI flag (see `parse_panic_mode_arg`),
/// which needs `std::env::args()` at runtime.
pub static COMPILER_OPTIONS: LazyLock<CompilerOptions> = LazyLock::new(|| CompilerOptions {
    mir: MIR_OPTIONS,
    fail_level: Severity::Error,
    panic_mode: parse_panic_mode_arg().unwrap_or(PanicMode::Exit),
    platform: PlatformInfo::host(),
});

/// Looks for a `--panic-mode=<abort|exit>` flag among the process's own CLI
/// arguments. `None` if the flag wasn't passed at all (the caller then falls
/// back to the default); a present-but-unparseable value is a hard error
/// with a clear message rather than a silently-ignored typo. A hand-rolled
/// parse rather than a CLI-args crate, since this is the one optional flag
/// this binary has.
fn parse_panic_mode_arg() -> Option<PanicMode> {
    let arg = std::env::args().find(|arg| arg.starts_with("--panic-mode="))?;
    let value = arg
        .strip_prefix("--panic-mode=")
        .expect("checked by starts_with above");
    Some(PanicMode::parse(value).unwrap_or_else(|| {
        panic!("invalid --panic-mode value {value:?} (expected \"abort\" or \"exit\")")
    }))
}

pub const PRINT_CONFIGS: PrintConfigs = PrintConfigs {
    #[cfg(feature = "error_backtrace")]
    backtrace: true,
    color: true,
};

/// `config.json`'s own location, resolved relative to the running exe
/// rather than baked in at compile time (`include_str!`) — needed so
/// `scripts/run_codegen_tests.py` can build `sol_tester` once and then
/// invoke `target/debug/sol_tester.exe` directly for every test file,
/// rewriting `config.json` between runs without forcing a recompile each
/// time. Assumes the exe lives three directories under the repo root
/// (`target/<profile>/sol_tester.exe`, `cargo`'s own default layout) —
/// doesn't hold under a custom `--target-dir`, but this binary is only ever
/// invoked via `cargo run`/`cargo build` or the test script, never shipped.
fn config_path() -> PathBuf {
    let exe = std::env::current_exe().expect("failed to resolve the running exe's own path");
    exe.parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .unwrap_or_else(|| {
            panic!(
                "expected {} to live three directories under the repo root (target/<profile>/sol_tester.exe)",
                exe.display()
            )
        })
        .join("sol_tester")
        .join("config.json")
}

fn parse_config() -> Configs {
    let path = config_path();
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read config at {}: {e}", path.display()));
    let json: JsonConfigs = serde_json::from_str(&raw).expect("should have not parse error");
    Configs::new(json)
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonConfigs {
    print_faults: bool,
    print_debug_output: bool,
    main_path: String,
    source_path: String,
    output_path: String,
    project_path: String,
}

#[derive(Debug, Clone)]
pub struct Configs {
    print_faults: bool,
    print_debug_output: bool,
    source_path: PathBuf,
    output_path: PathBuf,
    main_file_name: String,
}

impl Configs {
    pub fn new(json: JsonConfigs) -> Self {
        Self {
            print_faults: json.print_faults,
            print_debug_output: json.print_debug_output,
            main_file_name: json.main_path,
            source_path: Path::new(&json.project_path).join(json.source_path),
            output_path: Path::new(&json.project_path).join(json.output_path),
        }
    }

    pub fn print_debug_output(&self) -> bool {
        self.print_debug_output
    }

    pub fn should_print_faults(&self) -> bool {
        self.print_faults
    }

    pub fn to_main_path(&self) -> PathBuf {
        self.source_path.join(&self.main_file_name)
    }

    pub fn source_path(&self) -> &Path {
        &self.source_path
    }

    pub fn output_path(&self) -> &Path {
        &self.output_path
    }
}
