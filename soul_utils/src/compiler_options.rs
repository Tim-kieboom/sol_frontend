use crate::{bitflags, fault::Severity};

bitflags! {
    pub struct MirOptions: u8 {
        CHECK_ALGORITHMIC_OVERFLOW = 1 << 0,
        CHECK_INDEX_OUT_OF_BOUNDS = 1 << 1,
    }
}
pub struct CompilerOptions {
    pub platform: PlatformInfo,
    pub panic_mode: PanicMode,
    pub fail_level: Severity,
    pub mir: MirOptions,
}
impl CompilerOptions {
    pub const fn const_default() -> Self {
        Self {
            platform: PlatformInfo::const_default(),
            fail_level: Severity::const_default(),
            panic_mode: PanicMode::Abort,
            mir: MirOptions::all(),
        }
    }
}

/// Target-specific integer widths codegen needs to turn a Soul type like
/// `int`/`cint` into a concrete machine width. Every stage upstream of
/// codegen (AST, name resolution, MIR) only ever treats `int`/`cint` as
/// opaque type tags, so nothing else in the pipeline reads this — it's
/// carried on `CompilerOptions` purely so codegen doesn't have to guess
/// the target for itself.
#[derive(Debug, Clone, Copy)]
pub struct PlatformInfo {
    /// Width of Soul's own platform-sized `int`/`uint` — pointer width.
    pub pointer_bits: u32,
    /// Width of C's `int`/`unsigned int` on this target. Fixed at 32 on
    /// every LP64/LLP64 target (i.e. every target this compiler currently
    /// runs on), independent of pointer width.
    pub c_int_bits: u32,
}
impl PlatformInfo {
    /// This compiler currently only targets Windows x86-64: 64-bit
    /// pointers, 32-bit C `int` (the LLP64 data model).
    pub const fn new_windows_x86_64() -> Self {
        Self {
            pointer_bits: 64,
            c_int_bits: 32,
        }
    }

    pub const fn const_default() -> Self {
        Self::new_windows_x86_64()
    }
}
impl Default for PlatformInfo {
    fn default() -> Self {
        Self::const_default()
    }
}

/// How a panic (a failed bounds/overflow check, `assert`, `panic(msg)`)
/// terminates the process after printing its message — see
/// `mir_codegen::terminator::panic_function`, the only place this is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanicMode {
    /// `abort()` — routes through the OS's structured-exception/crash-
    /// reporting machinery (on Windows, measurably slower: ~55ms vs. ~4ms
    /// for a plain process exit, even with the same `printf` output).
    Abort,
    /// `exit(code)` — a plain process exit with no OS-level crash reporting;
    /// faster, at the cost of losing whatever crash-dump/reporting tooling
    /// relies on the abort signal.
    Exit,
}
impl PanicMode {
    pub const fn const_default() -> Self {
        Self::Abort
    }

    /// Parses a `--panic-mode` CLI value — case-insensitive, matching the
    /// variant names (`abort`, `exit`).
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            _ if value.eq_ignore_ascii_case("abort") => Some(Self::Abort),
            _ if value.eq_ignore_ascii_case("exit") => Some(Self::Exit),
            _ => None,
        }
    }
}
impl Default for PanicMode {
    fn default() -> Self {
        Self::const_default()
    }
}
