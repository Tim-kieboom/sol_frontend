use std::{fmt, rc::Rc};

use sol_utils::{
    Ident, Mutable, SharedStr, collections::array::RcArr, impl_sol_ids, sol_names::PrimitiveTypes,
};

use crate::{NodeId, declare_store::DeclareStore};

// TypeId: uniquely identifies a canonical, interned SolType — see
// DeclareStore::intern_type.
impl_sol_ids!(TypeId);

impl TypeId {
    // Comptime `TypeId`s for every context-free (parameterless) `SolType` —
    // interned in this exact order by the first thing
    // `DeclareStore::new`/`DeclareStore::register_well_known_types` does, so
    // `some_id == TypeId::PRIM_INT` is a plain integer comparison instead of
    // `declares.get_type(some_id) == Some(&SolType::Primitive(PrimitiveTypes::Int))`.
    // The order here must match `register_well_known_types`'s `intern_type`
    // calls exactly — enforced there by a `debug_assert_eq!` per entry, not by
    // construction, since these have to be literal constants to be comptime.
    pub const NONE: TypeId = TypeId(1);
    pub const NEVER: TypeId = TypeId(2);
    pub const STRING: TypeId = TypeId(3);
    pub const FORMAT_STRING: TypeId = TypeId(4);
    pub const ANY: TypeId = TypeId(5);
    pub const TYPE: TypeId = TypeId(6);
    /// `SolType::Error` — the language's built-in error-wrapper type.
    /// Distinct from `TypeId::ERROR`, which is the generic "no such id"
    /// sentinel every `impl_sol_ids!` type gets.
    pub const ERROR_TYPE: TypeId = TypeId(7);

    pub const PRIM_CHAR: TypeId = TypeId(8);
    pub const PRIM_CHAR8: TypeId = TypeId(9);
    pub const PRIM_CHAR16: TypeId = TypeId(10);
    pub const PRIM_CHAR32: TypeId = TypeId(11);
    pub const PRIM_CHAR64: TypeId = TypeId(12);
    pub const PRIM_CSTR: TypeId = TypeId(13);
    pub const PRIM_NONE: TypeId = TypeId(14);
    pub const PRIM_BOOLEAN: TypeId = TypeId(15);
    pub const PRIM_CINT: TypeId = TypeId(16);
    pub const PRIM_UNTYPED_INT: TypeId = TypeId(17);
    pub const PRIM_INT: TypeId = TypeId(18);
    pub const PRIM_INT8: TypeId = TypeId(19);
    pub const PRIM_INT16: TypeId = TypeId(20);
    pub const PRIM_INT32: TypeId = TypeId(21);
    pub const PRIM_INT64: TypeId = TypeId(22);
    pub const PRIM_INT128: TypeId = TypeId(23);
    pub const PRIM_CUINT: TypeId = TypeId(24);
    pub const PRIM_UNTYPED_UINT: TypeId = TypeId(25);
    pub const PRIM_UINT: TypeId = TypeId(26);
    pub const PRIM_UINT8: TypeId = TypeId(27);
    pub const PRIM_UINT16: TypeId = TypeId(28);
    pub const PRIM_UINT32: TypeId = TypeId(29);
    pub const PRIM_UINT64: TypeId = TypeId(30);
    pub const PRIM_UINT128: TypeId = TypeId(31);
    pub const PRIM_UNTYPED_FLOAT: TypeId = TypeId(32);
    pub const PRIM_FLOAT16: TypeId = TypeId(33);
    pub const PRIM_FLOAT32: TypeId = TypeId(34);
    pub const PRIM_FLOAT64: TypeId = TypeId(35);
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum SolType {
    /// empty type
    None,
    /// type will never apear example for unreachable controlflows
    Never,
    /// `str`
    String,
    /// `fstr`
    FormatString,
    /// any type
    Any,
    Type,
    /// tuple `(int, str)` and named_tuple `(number: int, text: str)`
    TupleKind(TupleKind),
    /// Primitive types like int, bool, float
    Primitive(PrimitiveTypes),
    /// array type: `[1]int` or `[&]int` or `[&mut]int` or `[]int`
    Array(ArrayType),
    /// Reference type: `&int` or `&mut int`
    Reference(ReferenceType),
    /// Pointer type: `*int`
    Pointer(ReferenceType),
    /// Raw pointer type: `RawPtr` or `RawPtr<int>`. Nullable by default.
    /// When no generic is specified, it's a void pointer (`RawPtr<none>`).
    RawPtr(Option<TypeId>),
    /// result type: `Res` or `Res<int>` or Res<int, str>.
    /// When no generic is specified, it's a void pointer with Error (`Res<none, Error>`).
    Res {
        ok: Option<TypeId>,
        err: Option<TypeId>,
    },
    /// Built-in error wrapper type (like Rust's `anyhow::Error`).
    /// Can wrap any error value — used as the default `E` in `Res<V>`.
    Error,
    /// Optional type: `?int`
    Optional(TypeId),
    /// Anonymous `impl Trait` type: `impl Display`.
    ImplTrait(TypeId),
    /// unknown type
    Stub(Stub),
    /// A specific variant of an enum type: `base::variant`.
    NamedVariant {
        /// The enum type the variant belongs to.
        base: TypeId,
        /// The variant's name.
        variant: Ident,
    },
    /// A lambda/closure value's type.
    Function {
        arity: usize,
        return_type: TypeId,
    },
}
impl SolType {
    pub fn is_primitive_kind(&self, kind: PrimitiveTypes) -> bool {
        matches!(self, SolType::Primitive(actual) if *actual == kind)
    }

    pub fn is_primitive(&self) -> bool {
        matches!(self, SolType::Primitive(_))
    }

    /// If this is a `&T`/`&mut T`/`*T`, returns the `T` it points at; `None`
    /// for any other type (no automatic pass-through — callers that want
    /// "unwrap one reference layer, or use the type as-is" do
    /// `ty.deref_once(declares).unwrap_or_else(|| ty.clone())`). The one
    /// "is this a reference, what's underneath it" rule shared by
    /// `mir_parser`'s place-resolution (`auto_deref`, which additionally
    /// pushes a `PlaceElem::Deref`), the resolver's own `*ptr` expression
    /// typing, and its struct-field auto-deref prelude — previously three
    /// independent copies of the same match, two of them only kept in sync
    /// by a doc comment.
    pub fn deref_once(&self, declares: &DeclareStore) -> Option<SolType> {
        match self {
            SolType::Reference(reference) | SolType::Pointer(reference) => {
                declares.get_type(reference.inner).cloned()
            }
            _ => None,
        }
    }
}

/// Prints a `SolType`, resolving any nested `TypeId` through `declares` —
/// `SolType`'s own internal fields are interned (see `DeclareStore::
/// intern_type`), so unlike a boundary/leaf `TypeId` (a `Parameter.ty`, say),
/// there's no way to recover a human-readable type name from a bare
/// `SolType` value alone anymore. Use this everywhere a `SolType` used to
/// be formatted with `{:?}` (error messages, the AST/MIR dumps, ...).
pub struct PrintType<'a> {
    ty: &'a SolType,
    declares: &'a DeclareStore,
}

/// Builds a [`PrintType`] for `ty` — see its docs.
pub fn print_type<'a>(ty: &'a SolType, declares: &'a DeclareStore) -> PrintType<'a> {
    PrintType { ty, declares }
}

impl<'a> PrintType<'a> {
    fn with(&self, ty: &'a SolType) -> Self {
        Self {
            ty,
            declares: self.declares,
        }
    }

    /// Resolves `id` and prints it, or `<unknown TypeId>` if it was never
    /// interned — never panics, since this is used from `Display`/`Debug`.
    fn write_id(&self, f: &mut fmt::Formatter<'_>, id: TypeId) -> fmt::Result {
        match self.declares.get_type(id) {
            Some(ty) => write!(f, "{}", self.with(ty)),
            None => write!(f, "<unknown TypeId {id:?}>"),
        }
    }
}

impl<'a> fmt::Display for PrintType<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.ty {
            SolType::None => write!(f, "none"),
            SolType::Never => write!(f, "!"),
            SolType::String => write!(f, "str"),
            SolType::FormatString => write!(f, "fstr"),
            SolType::Any => write!(f, "any"),
            SolType::TupleKind(kind) => match kind {
                TupleKind::Tuple(types) => {
                    write!(f, "(")?;
                    for (i, id) in types.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        self.write_id(f, *id)?;
                    }
                    write!(f, ")")
                }
                TupleKind::NamedTuple(items) => {
                    write!(f, "(")?;
                    for (i, (name, id)) in items.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{name}: ")?;
                        self.write_id(f, *id)?;
                    }
                    write!(f, ")")
                }
            },
            SolType::Primitive(primitive) => write!(f, "{}", primitive),
            SolType::Array(array) => {
                match array.kind {
                    ArrayKind::StackArrayWildcard => write!(f, "[_]")?,
                    ArrayKind::StackArray(size) => write!(f, "[{}]", size)?,
                    ArrayKind::HeapArray => write!(f, "[]")?,
                    ArrayKind::MutSlice => write!(f, "[&]")?,
                    ArrayKind::ConstSlice => write!(f, "[&mut]")?,
                }
                self.write_id(f, array.of_type)
            }
            SolType::Reference(reference) => {
                write!(f, "&")?;
                if let Some(lifetime) = &reference.lifetime {
                    write!(f, "'{} ", lifetime)?;
                }
                if reference.mutable.is_mut() {
                    write!(f, "mut ")?;
                }
                self.write_id(f, reference.inner)
            }
            SolType::Pointer(pointer) => {
                write!(f, "*")?;
                if let Some(lifetime) = &pointer.lifetime {
                    write!(f, "'{} ", lifetime)?;
                }
                if pointer.mutable.is_mut() {
                    write!(f, "mut ")?;
                }
                self.write_id(f, pointer.inner)
            }
            SolType::RawPtr(generic) => match generic {
                Some(id) => {
                    write!(f, "RawPtr<")?;
                    self.write_id(f, *id)?;
                    write!(f, ">")
                }
                None => write!(f, "RawPtr"),
            },
            SolType::Res { ok, err } => match (ok, err) {
                (None, None) => write!(f, "Res"),
                (Some(ok), None) => {
                    write!(f, "Res<")?;
                    self.write_id(f, *ok)?;
                    write!(f, ">")
                }
                (None, Some(err)) => {
                    write!(f, "Res<none, ")?;
                    self.write_id(f, *err)?;
                    write!(f, ">")
                }
                (Some(ok), Some(err)) => {
                    write!(f, "Res<")?;
                    self.write_id(f, *ok)?;
                    write!(f, ", ")?;
                    self.write_id(f, *err)?;
                    write!(f, ">")
                }
            },
            SolType::Error => write!(f, "Error"),
            SolType::Optional(inner) => {
                write!(f, "?")?;
                self.write_id(f, *inner)
            }
            SolType::ImplTrait(inner) => {
                write!(f, "impl ")?;
                self.write_id(f, *inner)
            }
            SolType::Stub(stub) => {
                f.write_str(&stub.name)?;
                #[cfg(debug_assertions)]
                f.write_str("/*as Stub*/")?;
                if !stub.generics.is_empty() {
                    write!(f, "<")?;
                    for (i, id) in stub.generics.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        self.write_id(f, *id)?;
                    }
                    write!(f, ">")?;
                }
                Ok(())
            }
            SolType::NamedVariant { base, variant } => {
                self.write_id(f, *base)?;
                write!(f, "::{}", variant)
            }
            SolType::Function { arity, return_type } => {
                write!(f, "fn({arity} args) -> ")?;
                self.write_id(f, *return_type)
            }
            SolType::Type => write!(f, "type"),
        }
    }
}

/// A tuple type, either positional (`(int, str)`) or with named fields
/// (`(number: int, text: str)`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum TupleKind {
    /// A positional tuple: `(int, str)`.
    Tuple(Tuple),
    /// A tuple with named fields: `(number: int, text: str)`.
    NamedTuple(NamedTuple),
}

impl TupleKind {
    /// Returns `true` if the tuple has no elements.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the number of elements in the tuple.
    pub fn len(&self) -> usize {
        match self {
            TupleKind::Tuple(types) => types.len(),
            TupleKind::NamedTuple(items) => items.len(),
        }
    }
}

/// The element types of a positional tuple.
pub type Tuple = RcArr<TypeId>;
/// The name/type pairs of a named tuple.
pub type NamedTuple = RcArr<(Ident, TypeId)>;

/// Array type
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ArrayType {
    /// The element type of the array.
    pub of_type: TypeId,
    /// Compile-time size, or `None` for dynamic arrays.
    pub kind: ArrayKind,
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ArrayKind {
    /// StackArrayWildcard `[_]int` set infered size same as C stackArray
    StackArrayWildcard,
    /// stackArray `[2]int` set size same as C stackArray
    StackArray(u64),
    /// heapArray `[]int` runtime sized array that lifes on the heap
    HeapArray,
    /// MutRefSlice `[&]int` a Mutable Refrence to any Array kind (can also be part of an array like `slice: [&]int = &array[0..1]`)
    MutSlice,
    /// ConstRefSlice `[&mut]int` a Inmutable Refrence to any Array kind (can also be part of an array `slice: [&mut]int = &mut array[0..1]`)
    ConstSlice,
}

impl ArrayKind {
    /// Whether this array kind is one the current pipeline can actually
    /// represent, in both MIR lowering (`DeclareStore::is_lowerable`) and
    /// codegen (`mir_codegen`'s `array_type`) — a fixed-size stack array or
    /// either slice kind, but not yet `StackArrayWildcard`/`HeapArray`.
    pub fn is_lowerable(self) -> bool {
        matches!(
            self,
            ArrayKind::StackArray(_) | ArrayKind::MutSlice | ArrayKind::ConstSlice
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ReferenceType {
    /// The inner type being referenced.
    pub inner: TypeId,
    /// The lifetime identifier.
    pub lifetime: Option<Ident>,
    /// Whether the reference is mutable.
    pub mutable: Mutable,
}

/// An as-yet-unresolved named type reference (e.g. a struct/enum/trait name
/// before it has been linked to its declaration), with any generic arguments.
///
/// `occurrence` is a fresh `NodeId` the parser allocates once per syntactic
/// type-name occurrence (not shared/deduplicated across occurrences), and
/// participates in this struct's own `Eq`/`Hash` — so two textually
/// identical names (`Point` written in two different modules, or even twice
/// in the same function) always intern to two different `TypeId`s. This is
/// deliberate: interning `Stub` purely by `name`/`generics` (as it was
/// before) meant two different modules' same-named-but-different structs
/// collided on one `TypeId`, which made per-`TypeId` caching of struct
/// resolution unsound (see `DeclareStore::resolve_struct`'s own history) and
/// left "does this name resolve to anything" as something every downstream
/// consumer had to reject independently instead of once, at resolve time.
/// `DeclareStore::type_resolves` is keyed on this now-unique `TypeId`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Stub {
    /// The referenced type's name.
    pub name: SharedStr,
    /// The generic type arguments applied to the reference, if any.
    pub generics: RcArr<TypeId>,
    /// This occurrence's own identity — see the struct's own docs.
    pub occurrence: NodeId,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Generic {
    /// The generic parameter's name.
    pub name: Ident,
    /// Optional trait bound: `T: TraitName`
    pub bound: Option<SolType>,
}

impl Stub {
    /// Creates a stub reference to a named type with no generic arguments, at
    /// a real occurrence — use this from production parsing code, where a
    /// fresh `NodeId` is always available.
    pub fn new_at(name: impl Into<Rc<str>>, occurrence: NodeId) -> Self {
        Self {
            generics: RcArr::new(),
            name: SharedStr::new(name),
            occurrence,
        }
    }

    /// Test/fixture convenience: a stub reference with no real occurrence
    /// identity (`NodeId::ERROR`). Never equal to a genuinely parsed `Stub`
    /// (which always carries a real, unique `occurrence`) — tests that need
    /// to assert something about a parsed/resolved `Stub` should use
    /// `matches_ignoring_occurrence` instead of comparing full equality
    /// against this. Production code should use `new_at`, which takes a
    /// real occurrence.
    pub fn new(name: impl Into<Rc<str>>) -> Self {
        Self::new_at(name, NodeId::ERROR)
    }

    /// Test/fixture helper: compares two stubs by `name`/`generics` only,
    /// ignoring `occurrence` — a hand-built expected `Stub` (via `new`/
    /// `new_at`) can never share a real parsed value's occurrence, so a
    /// test asserting "this resolved to a `Stub` named `Foo`" should use
    /// this instead of `==`/`assert_eq!`.
    pub fn matches_ignoring_occurrence(&self, other: &Stub) -> bool {
        self.name == other.name && self.generics == other.generics
    }
}

impl ReferenceType {
    /// Creates a reference type wrapping `inner`, without a lifetime annotation.
    pub fn new(inner: TypeId, mutable: Mutable) -> Self {
        Self {
            inner,
            lifetime: None,
            mutable,
        }
    }

    /// Creates a reference type wrapping `inner`, with a lifetime annotation.
    pub fn with_lifetime(inner: TypeId, lifetime: Ident, mutable: Mutable) -> Self {
        Self {
            mutable,
            inner,
            lifetime: Some(lifetime),
        }
    }
}
