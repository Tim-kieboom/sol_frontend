// expect: 0
// expect_stdout: 1, 1.000000
//
// New feature: `varargs.[...]` at a call site, flattened into separate
// trailing LLVM call operands (never a runtime array/slice). `fnum` is
// declared `f32` but must still print as a `%f`-correct double — proving
// the C default argument promotion (`f32` -> `f64`) actually happened.
extern "C" printf(fmt: cstr, args: varargs): cint

main(): i32 {
    inum: int = 1
    fnum: f32 = 1.0
    printf(c"%d, %f", varargs.[inum, fnum])
    return 0
}
