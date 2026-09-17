use crate::{define_str_enum, define_symbols};

define_str_enum!(
    /// Symbol kinds representing all possible symbols/tokens in the Soul language.
    ///
    /// This enum covers operators, punctuation, brackets, and other symbols
    /// that can appear in source code.
    pub enum Symbol {
        /// `+`
        Plus => "+",
        /// `-`
        Minus => "-",
        /// `*`
        Star => "*",
        /// `/`
        Slash => "/",
        /// `</`
        Root => "</",
        /// `%`
        Mod => "%",
        /// `&`
        And => "&",
        /// `@`
        AtSign => "@",
        /// `$`
        Money => "$",
        /// `|`
        Or => "|",
        /// `^`
        Xor => "^",
        /// `||`
        DoubleOr => "||",
        /// `=`
        Assign => "=",
        /// `:=`
        ColonAssign => ":=",
        /// `+=`
        PlusEq => "+=",
        /// `-=`
        MinusEq => "-=",
        /// `*=`
        StarEq => "*=",
        /// `/=`
        SlashEq => "/=",
        /// `%=`
        ModEq => "%=",
        /// `&=`
        AndEq => "&=",
        /// `|=`
        OrEq => "|=",
        /// `^=`
        XorEq => "^=",
        /// `=>`
        LambdaArrow => "=>",
        /// `==`
        Eq => "==",
        /// `!`
        Not => "!",
        /// `#`
        Hash => "#",
        /// `?`
        Question => "?",
        /// `??`
        DoubleQuestion => "??",
        /// `!=`
        NotEq => "!=",
        /// `<`
        LeftArray => "<",
        /// `>`
        RightArray => ">",
        /// `<=`
        Le => "<=",
        /// `>=`
        Ge => ">=",
        /// `->`
        RightArrow => "->",
        /// `:`
        Colon => ":",
        /// `::`
        DoubleColon => "::",
        /// `;`
        SemiColon => ";",
        /// `.`
        Dot => ".",
        /// `,`
        Comma => ",",
        /// `..`
        DoubleDot => "..",
        /// `[]`
        Array => "[]",
        /// `(`
        RoundOpen => "(",
        /// `)`
        RoundClose => ")",
        /// `[`
        SquareOpen => "[",
        /// `]`
        SquareClose => "]",
        /// `{`
        CurlyOpen => "{",
        /// `}`
        CurlyClose => "}",
    }
);

/// Represents the primitive size categories for type inference.
pub enum PrimitiveSize {
    /// Character-sized (platform-specific).
    CharSize = 0,
    /// Integer-sized (platform-specific).
    IntAndPtrSize = 1,
    /// C integer-sized (platform-specific).
    CIntSize = 2,
    /// 8-bit.
    Bit8 = 3,
    /// 16-bit.
    Bit16 = 4,
    /// 32-bit.
    Bit32 = 5,
    /// 64-bit.
    Bit64 = 6,
    /// 128-bit.
    Bit128 = 7,
}

define_str_enum!(
    /// Internal primitive types available in the Soul language.
    ///
    /// These are the built-in numeric, character, and boolean types.
    ///
    /// (!!WARNING!! precedence is used for bit size DO NOT USE FOR PRECEDENCE)
    #[derive(Hash)]
    pub enum PrimitiveTypes {
        /// default-size character type
        Char => "char", PrimitiveSize::CharSize as u8,
        /// 8-bit character type
        Char8 => "char8", 8,
        /// 16-bit character type
        Char16 => "char16", 16,
        /// 32-bit character type
        Char32 => "char32", 32,
        /// 64-bit character type
        Char64 => "char64", 64,

        /// a null terminated char pointer
        CStr => "cstr", PrimitiveSize::IntAndPtrSize as u8,

        /// empty type (also known as `void` in c like languages)
        None => "none", 8,
        /// boolean (`true` or `false`) type
        Boolean => "bool", 8,

        /// c sized interger type
        CInt => "cint", PrimitiveSize::CIntSize as u8,
        /// undecided integer type
        UntypedInt => "untypedInt", PrimitiveSize::IntAndPtrSize as u8,
        /// system-sizes integer type
        Int => "int", PrimitiveSize::IntAndPtrSize as u8,
        /// 8-bit integer type
        Int8 => "i8", 8,
        /// 16-bit integer type
        Int16 => "i16", 16,
        /// 32-bit integer type
        Int32 => "i32", 32,
        /// 64-bit integer type
        Int64 => "i64", 64,
        /// 128-bit integer type
        Int128 => "i128", 128,

        /// c sized interger type
        CUint => "cuint", PrimitiveSize::CIntSize as u8,
        /// undecided unsigned integer type
        UntypedUint => "untypedUint", PrimitiveSize::IntAndPtrSize as u8,
        /// system-sized unsigned integer type
        Uint => "uint", PrimitiveSize::IntAndPtrSize as u8,
        /// 8-bit unsigned integer type
        Uint8 => "u8", 8,
        /// 16-bit unsigned integer type
        Uint16 => "u16", 16,
        /// 32-bit unsigned integer type
        Uint32 => "u32", 32,
        /// 64-bit unsigned integer type
        Uint64 => "u64", 64,
        /// 128-bit unsigned integer type
        Uint128 => "u128", 128,

        /// undecided floating-point type
        UntypedFloat => "untypedFloat", 32,
        /// 16-bit floating-point type
        Float16 => "f16", 16,
        /// 32-bit floating-point type
        Float32 => "f32", 32,
        /// 64-bit floating-point type
        Float64 => "f64", 64,
    }
);

impl PrimitiveTypes {
    /// Whether this primitive is a signed integer type — a pure classification
    /// of the type's own shape, shared by both `mir_parser` (checked-arithmetic/
    /// div lowering) and `mir_codegen` (choosing signed vs. unsigned LLVM
    /// int predicates/ops), which each used to reimplement this identically.
    pub fn is_signed(self) -> bool {
        use PrimitiveTypes::*;
        matches!(
            self,
            CInt | UntypedInt | Int | Int8 | Int16 | Int32 | Int64 | Int128
        )
    }

    /// Whether this primitive is a floating-point type.
    pub fn is_float(self) -> bool {
        use PrimitiveTypes::*;
        matches!(self, Float16 | Float32 | Float64 | UntypedFloat)
    }

    /// The minimum representable value of a signed integer primitive, or
    /// `None` if this isn't a signed integer at all. `Int`/`UntypedInt`/`CInt`
    /// are platform-sized (see `PlatformInfo`'s own doc comment on why nothing
    /// upstream of codegen is normally supposed to read it) — computing the
    /// correct `MIN` bit pattern genuinely needs the concrete width, so this
    /// is the one place that peeks it ahead of codegen.
    pub fn signed_min(self, platform: &crate::compiler_options::PlatformInfo) -> Option<i128> {
        use PrimitiveTypes::{CInt, Int, Int8, Int16, Int32, Int64, Int128, UntypedInt};
        let bits = match self {
            Int8 => 8,
            Int16 => 16,
            Int32 => 32,
            Int64 => 64,
            Int128 => 128,
            Int | UntypedInt => platform.pointer_bits,
            CInt => platform.c_int_bits,
            _ => return None,
        };
        Some(match bits {
            8 => i8::MIN as i128,
            16 => i16::MIN as i128,
            32 => i32::MIN as i128,
            64 => i64::MIN as i128,
            128 => i128::MIN,
            // Only 32/64-bit pointer/C-int widths are produced today; this
            // only exists so the match is exhaustive.
            _ => i64::MIN as i128,
        })
    }
}

define_symbols!(
    /// Binary and unary operators available in the Soul language.
    ///
    /// These operators are used in expressions for arithmetic, logical, bitwise,
    /// and comparison operations.
    pub enum Operator {
        /// logical not `!`
        Not => "!", Symbol::Not, 8,
        /// lvalue(exponent) root rvalue(base) `</`
        Root => "</", Symbol::Root, 7,
        /// multiplication `*`
        Mul => "*", Symbol::Star, 6,
        /// divide `/`
        Div => "/", Symbol::Slash, 6,
        /// modulo `%`
        Mod => "%", Symbol::Mod, 6,
        /// addition `+`
        Add => "+", Symbol::Plus, 5,
        /// subtraction `-`
        Sub => "-", Symbol::Minus, 5,
        /// atSign `@`
        AtSign => "@", Symbol::AtSign, 6,

        /// smaller equals `<=`
        LessEq => "<=", Symbol::Le, 4,
        /// bigger equals `>=`
        GreatEq => ">=", Symbol::Ge, 4,
        // smaller then `<`
        LessThen => "<", Symbol::LeftArray, 4,
        // bigger then `>`
        GreatThen => ">", Symbol::RightArray, 4,
        /// not equals `!=`
        NotEq => "!=", Symbol::NotEq, 3,
        /// equal `==`
        Eq => "==", Symbol::Eq, 3,

        /// range (`begin..end`)
        Range => "..", Symbol::DoubleDot, 1,

        /// bitwise or `|`
        BitOr => "|", Symbol::Or, 1,
        /// bitwise and `&`
        BitAnd => "&", Symbol::And, 1,
        /// bitwise xor `^`
        BitXor => "^", Symbol::Xor, 2,

        /// logical or `||`
        LogOr => "||", Symbol::DoubleOr, 0,
        /// safe-call / chaining `->`
        Arrow => "->", Symbol::RightArrow, 9,
    }
);
