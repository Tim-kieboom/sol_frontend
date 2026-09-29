use crate::compiler_options::{CompilerOptions, PlatformInfo};

// This compiler has no cross-compilation support — the machine running
// these tests is always the one `PlatformInfo::host()` is selecting for —
// so on the only target this workspace actually builds on (Windows
// x86-64), `host()` must resolve to `new_windows_x86_64()`'s exact values.
// If a second target is ever added to `host()`, this test only needs
// updating if it starts running in that target's own CI, since it always
// asserts against whatever the *current* build's `cfg!`s select.

#[test]
fn host_selects_windows_x86_64_on_this_target() {
    let host = PlatformInfo::host();
    let windows_x86_64 = PlatformInfo::new_windows_x86_64();
    assert_eq!(host.pointer_bits, windows_x86_64.pointer_bits);
    assert_eq!(host.c_int_bits, windows_x86_64.c_int_bits);
}

#[test]
fn compiler_options_default_platform_matches_host() {
    let options = CompilerOptions::const_default();
    let host = PlatformInfo::host();
    assert_eq!(options.platform.pointer_bits, host.pointer_bits);
    assert_eq!(options.platform.c_int_bits, host.c_int_bits);
}

/// C's `int` is 32 bits everywhere `host()` can select — only pointer width
/// (64 on every 64-bit target, 32 on the 32-bit ones) actually varies. These
/// don't depend on the build's own `cfg!`s (unlike the `host()` tests above)
/// since every named constructor is callable regardless of what's actually
/// running the tests.
#[test]
fn every_named_constructor_has_a_32_bit_c_int() {
    let all = [
        PlatformInfo::new_windows_x86_64(),
        PlatformInfo::new_windows_aarch64(),
        PlatformInfo::new_windows_x86(),
        PlatformInfo::new_linux_x86_64(),
        PlatformInfo::new_linux_aarch64(),
        PlatformInfo::new_linux_x86(),
        PlatformInfo::new_macos_x86_64(),
        PlatformInfo::new_macos_aarch64(),
    ];
    for platform in all {
        assert_eq!(platform.c_int_bits, 32, "{platform:?}");
    }
}

#[test]
fn sixty_four_bit_targets_have_64_bit_pointers() {
    for platform in [
        PlatformInfo::new_windows_x86_64(),
        PlatformInfo::new_windows_aarch64(),
        PlatformInfo::new_linux_x86_64(),
        PlatformInfo::new_linux_aarch64(),
        PlatformInfo::new_macos_x86_64(),
        PlatformInfo::new_macos_aarch64(),
    ] {
        assert_eq!(platform.pointer_bits, 64, "{platform:?}");
    }
}

#[test]
fn thirty_two_bit_targets_have_32_bit_pointers() {
    for platform in [
        PlatformInfo::new_windows_x86(),
        PlatformInfo::new_linux_x86(),
    ] {
        assert_eq!(platform.pointer_bits, 32, "{platform:?}");
    }
}
