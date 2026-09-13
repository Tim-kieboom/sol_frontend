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
    /// C's `int` is 32 bits on every data model below (LLP64, LP64, ILP32
    /// alike) — only the pointer width actually varies across them.
    const fn with_pointer_bits(pointer_bits: u32) -> Self {
        Self {
            pointer_bits,
            c_int_bits: 32,
        }
    }

    /// Windows x86-64: 64-bit pointers (the LLP64 data model).
    pub const fn new_windows_x86_64() -> Self {
        Self::with_pointer_bits(64)
    }
    /// Windows on Arm64: 64-bit pointers (LLP64, same as x86-64).
    pub const fn new_windows_aarch64() -> Self {
        Self::with_pointer_bits(64)
    }
    /// Windows x86 (32-bit): 32-bit pointers (LLP64 collapses to ILP32 at
    /// 32-bit pointer width).
    pub const fn new_windows_x86() -> Self {
        Self::with_pointer_bits(32)
    }
    /// Linux x86-64: 64-bit pointers (the LP64 data model).
    pub const fn new_linux_x86_64() -> Self {
        Self::with_pointer_bits(64)
    }
    /// Linux on Arm64: 64-bit pointers (LP64, same as x86-64).
    pub const fn new_linux_aarch64() -> Self {
        Self::with_pointer_bits(64)
    }
    /// Linux x86 (32-bit): 32-bit pointers (the ILP32 data model).
    pub const fn new_linux_x86() -> Self {
        Self::with_pointer_bits(32)
    }
    /// macOS on Intel: 64-bit pointers (the LP64 data model).
    pub const fn new_macos_x86_64() -> Self {
        Self::with_pointer_bits(64)
    }
    /// macOS on Apple Silicon: 64-bit pointers (LP64, same as Intel).
    pub const fn new_macos_aarch64() -> Self {
        Self::with_pointer_bits(64)
    }

    /// Selects the `PlatformInfo` for whatever machine this compiler itself
    /// is being built for — there's no cross-compilation support, so "the
    /// build target" and "the machine `mir_codegen` emits code for" are
    /// always the same one. A real selection instead of every caller
    /// reaching for one named constructor by name, so building for an
    /// (os, arch) pair not covered below fails loudly at compile time here,
    /// rather than silently mislabeling that target's int widths as some
    /// other one's — a genuinely new data model (not just another `(os,
    /// arch)` pair that already matches one of the widths above) still
    /// needs its own named constructor added first.
    pub const fn host() -> Self {
        if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
            Self::new_windows_x86_64()
        } else if cfg!(all(target_os = "windows", target_arch = "aarch64")) {
            Self::new_windows_aarch64()
        } else if cfg!(all(target_os = "windows", target_arch = "x86")) {
            Self::new_windows_x86()
        } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            Self::new_linux_x86_64()
        } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
            Self::new_linux_aarch64()
        } else if cfg!(all(target_os = "linux", target_arch = "x86")) {
            Self::new_linux_x86()
        } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
            Self::new_macos_x86_64()
        } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            Self::new_macos_aarch64()
        } else {
            panic!(
                "Soul doesn't have a PlatformInfo for this (target_os, target_arch) combination yet — add one to PlatformInfo::host()"
            );
        }
    }

    pub const fn const_default() -> Self {
        Self::host()
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
