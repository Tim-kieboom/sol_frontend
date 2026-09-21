// expect: 0
// expect_stdout: Hello from Sol!
extern "C" puts(message: cstr): cint

main(): i32 {
    puts(c"Hello from Sol!")
    return 0
}
