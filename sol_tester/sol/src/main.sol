extern "C" (
    printf(format: cstr, args: varargs): cint
)

main() {
    printf(c"hello world\n", varargs.[]);
}