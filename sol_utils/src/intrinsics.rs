use crate::bitflags;

macro_rules! define_intrinsics {
    (
        $(#[$enum_doc:meta])*
        $vis:vis enum $enum_name:ident {
            $( $(#[$attr:meta])* $variant:ident => $name:expr, $tag:expr, arity: $arity:expr),* $(,)?
        }
    ) => {

        $(#[$enum_doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
        $vis enum $enum_name {
            $(
                $(#[$attr])*
                $variant,
            )*
        }

        impl $enum_name {
            /// All enum variants, in declaration order.
            pub const VARIANTS: &[$enum_name] = &[ $( $enum_name::$variant, )* ];
            /// All string values corresponding to enum variants.
            pub const STRING_VALUES: &[&str] = &[ $($name,)* ];

            /// Returns the string representation of the variant (const-time).
            pub const fn as_str(&self) -> &'static str {
                match self {
                    $( $enum_name::$variant => $name, )*
                }
            }

            pub const fn tag(&self) -> Tags {
                match self {
                    $( $enum_name::$variant => $tag, )*
                }
            }

            pub const fn arity(&self) -> usize {
                match self {
                    $( $enum_name::$variant => $arity, )*
                }
            }

            /// Whether this intrinsic may only be called inside an `unsafe` block.
            ///
            /// Not enforced yet — `unsafe` blocks have no dedicated AST representation
            /// in this compiler. Kept as metadata so the check is a one-line addition
            /// once they do.
            pub const fn requires_unsafe(&self) -> bool {
                self.tag().contains(Tags::UNSAFE)
            }

            /// Whether this intrinsic is callable bare (no `intrinsic.` prefix) — see
            /// the resolver's `resolve_function_call`.
            pub const fn callable_bare(&self) -> bool {
                self.tag().contains(Tags::BARE_CALLABLE)
            }
        }

        impl std::fmt::Display for $enum_name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.as_str().fmt(f)
            }
        }

        impl std::str::FromStr for $enum_name {
            type Err = ();

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $( $name => Ok($enum_name::$variant), )*
                    _ => Err(()),
                }
            }
        }
    };
}

bitflags! {
    pub struct Tags: u8 {
        EMPTY = 0,
        UNSAFE = 1 << 0,
        BARE_CALLABLE = 2 << 0,
    }
}

define_intrinsics!(
    /// Compiler-provided `intrinsic.*` functions.
    ///
    /// Callable as `intrinsic.<path>(...)`, e.g. `intrinsic.array.toRaw(arr)`
    /// or `intrinsic.fieldIndex(t, index)`. Namespaced paths use a dotted
    /// string (`"array.toRaw"`); unnamespaced ones use a bare name (`"typeinfo"`).
    pub enum IntrinsicFunction {
        /// `intrinsic.array.toRaw<T>(arr: []T) -> RawPtr<T>` (unsafe). A raw,
        /// non-owning view — `RawPtr<T>`, never `*T`: `*T` is an *owning*
        /// heap pointer (see `new(expr)`), and this doesn't allocate or
        /// transfer ownership of anything, it just reinterprets an existing
        /// array's own storage.
        ArrayToRaw => "array.toRaw", Tags::UNSAFE, arity: 1,

        /// `intrinsic.ptr.toSlice<T>(ptr: RawPtr<T>, len: uint) -> []T`
        /// (unsafe)
        PtrToSlice => "ptr.toSlice", Tags::UNSAFE, arity: 2,

        /// `intrinsic.ptr.offset<T>(ptr: RawPtr<T>, index: int) -> RawPtr<T>`
        /// (unsafe) — pointer arithmetic on an existing raw pointer, not a
        /// new allocation, so `RawPtr<T>` (non-owning), not `*T`.
        PtrOffset => "ptr.offset", Tags::UNSAFE, arity: 2,

        /// `intrinsic.typeinfo(t: typeid) -> TypeInfo`
        TypeInfo => "typeinfo", Tags::EMPTY, arity: 1,

        /// `intrinsic.fieldIndex(t: typeid, index: uint) -> FieldInfo`
        FieldIndex => "fieldIndex", Tags::EMPTY, arity: 2,

        /// `intrinsic.fieldCount(t: typeid) -> uint`
        FieldCount => "fieldCount", Tags::EMPTY, arity: 1,

        /// `assert(cond: bool)` — panics with a default message if `cond` is
        /// `false`. Unlike every other intrinsic, callable *bare* — no
        /// `intrinsic.` prefix — since that's the only way real Sol code
        /// (and every other language's `assert`) ever calls it. See the
        /// resolver's `resolve_function_call` for where that's carved out.
        Assert => "assert", Tags::BARE_CALLABLE, arity: 1,

        /// `panic(msg: str)` — unconditionally aborts the program with `msg`.
        /// Also callable bare, same reasoning as `Assert`.
        Panic => "panic", Tags::BARE_CALLABLE, arity: 1,
    }
);
