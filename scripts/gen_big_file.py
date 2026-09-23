from pathlib import Path

PROJECT_DIR = Path(__file__).resolve().parent.parent
OUT = Path(str(PROJECT_DIR) + r"\sol_tester\sol\src\big.sol")

N_ARITH = 500
N_CONTROL = 400
N_STRUCT = 350

parts = []

parts.append(
    "/// big.sol\n"
    "///\n"
    "/// Generated stress-test fixture exercising a broad slice of the Sol language\n"
    "/// surface (structs, enums, unions, traits, generics, closures, async, unsafe)\n"
    "/// repeated many times over, purely to give the compiler pipeline a large,\n"
    "/// realistic-shaped input to measure tokenize/parse/resolve/codegen speed on.\n"
    "///\n"
    "/// This file is meant to be used as its own parse root (see benches/pipeline.rs\n"
    "/// `Input::main_path`), not merged into the SolTest program, so it does not\n"
    "/// define `main` and its identifiers are namespaced with numeric suffixes to\n"
    "/// avoid clashing with the other fixture files if it ever is compiled alongside them.\n"
    "main() {}\n"
)

# ---------------------------------------------------------------------------
# Arithmetic functions
# ---------------------------------------------------------------------------
for i in range(N_ARITH):
    parts.append(f"""
arithmeticOps{i}(a: int, b: int): int {{
    CONST{i} :: {i}
    sum := a + b
    diff := a - b
    product := a * b
    quotient := a / (b + 1)
    remainder := a % (b + 1)
    return sum + diff + product + quotient - remainder + CONST{i}
}}

floatOps{i}(a: f64, b: f64): f64 {{
    sum := a + b
    diff := a - b
    product := a * b
    quotient := a / (b + 1.0)
    return sum + diff + product + quotient
}}
""")

# ---------------------------------------------------------------------------
# Control flow functions
# ---------------------------------------------------------------------------
for i in range(N_CONTROL):
    parts.append(f"""
controlFlow{i}(a: int, b: int): int {{
    mut counter := a
    for counter > b {{
        if counter % 2 == 0 {{
            counter -= 1
        }} else {{
            counter -= 2
        }}
    }}

    return counter
}}
""")

# ---------------------------------------------------------------------------
# Structs with constructors and methods
# ---------------------------------------------------------------------------
for i in range(N_STRUCT):
    parts.append(f"""
//struct BigPoint{i} {{
//    x: int
//    y: int
//}}
//
//sum{i}(p: &BigPoint{i}): int {{
//    p.x + p.y
//}}
//
//scale{i}(p: &BigPoint{i}, factor: int): BigPoint{i} {{
//    BigPoint{i}{{x: p.x * factor, y: p.y * factor}}
//}}
//
//testBigPoint{i}(): int {{
//    p: BigPoint{i} = BigPoint{i}.({i}, {i} + 1)
//    scaled := scale{i}(&p, 2)
//    sum{i}(p, scaled)
//}}
""")


with open(OUT, "w", encoding="utf-8", newline="\n") as f:
    f.write("".join(parts))

with open(OUT, encoding="utf-8") as f:
    line_count = sum(1 for _ in f)

print(f"wrote {OUT} with {line_count} lines")
