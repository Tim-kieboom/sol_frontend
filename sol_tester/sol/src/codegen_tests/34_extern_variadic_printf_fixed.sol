// expect: 0
// expect_stdout: 1.000000
//
// Prerequisite bug fix: before `is_variadic` was threaded through to
// `declare_all_functions`'s LLVM `FunctionType` (`is_var_arg`), a call to
// the real (genuinely C-variadic) `printf` with a single `f64` argument
// printed `0.000000` on Windows x64 instead of `1.000000` — the callee's
// LLVM type wasn't marked variadic, so the float argument wasn't also
// duplicated into the integer register slot the real variadic `printf`
// reads it from under the Windows x64 calling convention.
extern "C" printf(format: cstr, args: varargs): cint

main(): i32 {
    f: f64 = 1.0
    printf(c"%f", varargs.[f])
    return 0
}
