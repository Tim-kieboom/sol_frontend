//! MIR data structures. Pure shapes, no lowering logic — see `mir_parser` for the
//! AST-to-MIR construction pass. Mirrors rustc's MIR shape; see
//! `docs/mir-design.md` at the repo root for the rationale behind each piece.
//!
//! Only a subset of these variants is actually *constructed* by the current
//! (smallest-slice) lowering pass; the rest exist so later slices (control flow,
//! calls, structs, borrow checking) are new passes over an already-complete shape
//! rather than a shape migration.

use ast_model::{Literal, SoulType, TypeId, declare_store::DeclareStore, operators::BinaryOperatorKind};
use soul_utils::{
    FunctionId, Mutable, TypeModifier,
    collections::{array::Arr, vec_map::VecMap},
    impl_soul_ids,
    span::{ModuleId, Span},
};

impl_soul_ids!(LocalId, BlockId);

/// The type of a MIR local. An interned handle onto the frontend's resolved
/// type — resolve back to `&SoulType` via `DeclareStore::get_type` wherever
/// the shape (not just the identity) is needed. M1/M2 only ever see concrete
/// types. Once generics (M3) land this needs to grow a `Param(String)`
/// placeholder variant (see `docs/mir-design.md`'s Generics section) —
/// deliberately not added yet since nothing constructs it today.
pub type Type = TypeId;

/// A constant value baked into MIR. A plain alias onto the frontend's literal
/// representation; revisit if MIR ever needs a constant shape the AST doesn't
/// (e.g. a post-monomorphization sized-array constant).
pub type ConstValue = Literal;

pub struct MirProgram {
    pub functions: VecMap<FunctionId, Function>,
    /// `extern "C"` declarations — a signature with no body to lower at all,
    /// not a `Function` missing its blocks. Kept in a separate map rather
    /// than folded into `functions` so codegen can tell "declare only, no
    /// body to emit" apart from "should have a body" at the type level,
    /// instead of via a sentinel-empty `blocks`/`locals`.
    pub externs: VecMap<FunctionId, ExternFunction>,
}
impl MirProgram {
    pub const fn empty() -> Self {
        Self {
            functions: VecMap::const_default(),
            externs: VecMap::const_default(),
        }
    }
}

/// An `extern "C"` function declaration: just enough to declare it to LLVM
/// and lower calls to it — no body, no locals, no blocks. Every param/return
/// type is passed through as whatever `TypeId` the signature declared;
/// unlike `Function`'s body-lowering, there's no lowering logic here that
/// could break on a type it doesn't understand, so nothing is rejected at
/// this stage — codegen is the sole judge of which types it can actually
/// represent (see `mir_codegen`'s `NonPrimitiveType`/`UnsupportedPrimitiveType`).
#[derive(Debug, serde::Serialize)]
pub struct ExternFunction {
    pub id: FunctionId,
    /// Only the FIXED (non-variadic) parameters — the `varargs` marker
    /// parameter, if the AST signature had one, is not a real typed
    /// parameter and is excluded here.
    pub parameters: Arr<Type>,
    /// `None` for a `none`(void)-returning extern function — same convention
    /// as `Function::return_local`.
    pub return_type: Option<Type>,
    /// True when the AST signature's last parameter was the `varargs`
    /// marker — this extern is a genuine C variadic function and its LLVM
    /// `FunctionType` must be declared with `is_var_arg = true`.
    pub is_variadic: bool,
}

#[derive(Debug, serde::Serialize)]
pub struct Function {
    pub id: FunctionId,
    pub locals: VecMap<LocalId, LocalDecl>,
    pub blocks: VecMap<BlockId, BasicBlock>,
    /// `locals[0..arg_count]` are parameters, by convention.
    pub arg_count: usize,
    /// Holds the return value; conventionally `locals[arg_count]`. `None` for a
    /// `none`(void)-returning function — there's no value to hold, so no local
    /// is allocated for one rather than allocating a phantom, never-touched
    /// `none`-typed local just to fill this field.
    pub return_local: Option<LocalId>,
}

#[derive(Debug, serde::Serialize)]
pub struct LocalDecl {
    pub ty: Type,
    pub mutability: TypeModifier,
    pub span: Span,
}

#[derive(Debug, serde::Serialize)]
pub struct BasicBlock {
    pub statements: Arr<Statement>,
    pub terminator: Terminator,
}

/// No control flow of their own — always fall through to the next statement (or
/// the block's terminator, for the last one).
#[derive(Debug, serde::Serialize)]
pub enum Statement {
    Assign(Place, Rvalue),
    /// Marks a local as moved-from without an assignment (e.g. the source operand
    /// of a destructive read). Needed so the borrow checker can flag "use after
    /// move" without inferring move points from `Rvalue` shapes.
    MarkMoved(LocalId),
    /// Explicit drop-flag toggle: false on move-out, true on (re)init. See
    /// `docs/mir-design.md`'s move/drop section for why this is the source of
    /// truth for "is this slot occupied," not the type system.
    SetDropFlag(LocalId, bool),
    /// Lexical marker only — no runtime or CFG effect.
    StorageDead(LocalId),
}

#[derive(Debug, serde::Serialize)]
pub enum Rvalue {
    Use(Operand),
    BinaryOp(BinaryOperatorKind, Operand, Operand),
    /// `Add`/`Sub`/`Mul` that traps on overflow instead of wrapping. Produces
    /// a `(T, bool)` tuple (result, overflowed) — mirrors rustc's own
    /// `CheckedBinaryOp` shape: lowering assigns this into a tuple-typed
    /// temp, then emits a `Terminator::Assert` on the `bool` half (field
    /// `1`) before using the result (field `0`), so the overflow *check* is
    /// an ordinary MIR `Assert` rather than something `mir_codegen` has to
    /// special-case.
    CheckedBinaryOp(BinaryOperatorKind, Operand, Operand),
    UnaryOp(ast_model::operators::UnaryOperatorKind, Operand),
    Ref {
        mutable: bool,
        place: Place,
    },
    Aggregate(AggregateKind, Vec<Operand>),
    Cast(Operand, Type),
    /// The runtime length of a slice-typed place (`[&]T`/`[&mut]T`'s own
    /// `len` field) — always `uint`-typed. Used by bounds-check lowering to
    /// compare against an index before an `Assert`, same as rustc's `Len`.
    Len(Place),
    /// `new(expr)`: allocates a fresh heap slot sized for `Type`, stores
    /// `Operand`'s value into it, evaluates to the resulting pointer — a
    /// `SoulType::Pointer` (`*T`), an *owning* pointer freed by its own
    /// `Drop` (see M2's TODO.md entry), unlike a plain `Ref`. Doesn't fit
    /// `Terminator::Call`, which requires a real Soul-level `FunctionId` —
    /// `malloc` is a codegen-synthesized libc call, the same category as
    /// `abort`/`exit`/`panic`, not a MIR-level one; and unlike those, this
    /// produces a value rather than diverging, so it's an `Rvalue`, not
    /// its own terminator — no OOM check (an unconditional `Assert` on a
    /// null result) is emitted yet, so this stays a plain, non-branching
    /// `Rvalue` rather than needing a terminator's own control-flow shape.
    /// Carries `Type` explicitly (like `Cast`) since `mir_codegen` can't
    /// recover the pointee's size from context alone — the destination
    /// place's own LLVM type is just an opaque `ptr`.
    HeapAlloc(Type, Operand, Mutable),
}

#[derive(Debug, serde::Serialize)]
pub enum AggregateKind {
    Struct,
    Tuple,
    Array,
    Slice,
}

#[derive(Debug, Clone, serde::Serialize)]
pub enum Operand {
    /// `place`'s type is `Copy` or `AutoCopy`; reading it doesn't invalidate the source.
    Copy(Place),
    /// Reading invalidates the source; lowering emits a `MarkMoved` for the
    /// underlying local alongside this.
    Move(Place),
    Constant(ConstValue),
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Place {
    pub local: LocalId,
    pub projection: Vec<PlaceElem>,
}

impl Place {
    /// A bare local with no projection — the common case.
    pub fn local(local: LocalId) -> Self {
        Self {
            local,
            projection: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub enum PlaceElem {
    Field(usize),
    Index(LocalId),
    Deref,
}

impl PlaceElem {
    /// The Soul type reached by stepping through this one projection element
    /// from `ty` — a struct/tuple field's type for `Field`, an array's
    /// element type for `Index` (any array kind; a caller that only accepts
    /// some kinds, e.g. `mir_codegen`'s slice-only restriction, checks that
    /// itself before calling this), a reference/pointer's inner type for
    /// `Deref` (delegates to `SoulType::deref_once`). `None` if `ty` doesn't
    /// support this step at all (an unresolvable struct/field, a non-array
    /// `Index`, a non-reference `Deref`).
    ///
    /// Shared by `mir_parser`'s `place_type` (a pure, side-effect-free
    /// Soul-type re-derivation of an already-built `Place`) and
    /// `mir_codegen`'s `resolve_place` (which additionally walks the
    /// matching LLVM pointer/GEP for each step — that mechanical half stays
    /// separate, since it's inherently codegen-specific).
    pub fn step_type(
        &self,
        ty: &SoulType,
        declares: &DeclareStore,
        module: Option<ModuleId>,
    ) -> Option<SoulType> {
        match self {
            PlaceElem::Field(index) => {
                if let SoulType::TupleKind(ast_model::TupleKind::Tuple(types)) = ty {
                    let id = *types.get(*index)?;
                    declares.get_type(id).cloned()
                } else {
                    let struct_ = declares.resolve_struct(ty, module)?;
                    let field_ty_id = struct_.fields.get(*index)?.value.ty?;
                    declares.get_type(field_ty_id).cloned()
                }
            }
            PlaceElem::Index(_) => {
                let SoulType::Array(array) = ty else {
                    return None;
                };
                declares.get_type(array.of_type).cloned()
            }
            PlaceElem::Deref => ty.deref_once(declares),
        }
    }
}

/// Every block ends in exactly one of these; this is the whole CFG.
#[derive(Debug, serde::Serialize)]
pub enum Terminator {
    Goto(BlockId),
    /// Covers `if`/match-chain/traditional `match` uniformly.
    SwitchInt {
        discriminant: Operand,
        targets: Arr<(ConstValue, BlockId)>,
        otherwise: BlockId,
    },
    Call {
        id: FunctionId,
        arguments: Arr<Operand>,
        /// `None` when the callee returns `none`, or when the caller discards
        /// a non-`none` result (a bare `f(x);` statement) — either way there's
        /// nothing to write the result into.
        destination: Option<Place>,
        /// `None` = diverges (panics, or return type is `!`).
        target: Option<BlockId>,
    },
    /// `Drop`'s scope-exit call, gated at runtime by the local's drop flag.
    Drop {
        place: Place,
        target: BlockId,
    },
    /// `assert(cond)` / `panic(msg)`. Mirrors rustc's `Assert` terminator
    /// (minus `unwind`, since this compiler has no unwinding model): if
    /// `cond == expected`, execution continues at `target`; otherwise it
    /// panics with `msg`. An unconditional `panic(msg)` is `cond:
    /// Constant(Bool(false)), expected: true` — always takes the panic path,
    /// so `target` is allocated (the shape requires a `BlockId`) but never
    /// actually reachable, and lowering doesn't insert a real block for it.
    /// `span` is the source location this panics *at* — `codegen_assert`
    /// turns it into the `"file:line:col"` string printed alongside `msg`.
    Assert {
        cond: Operand,
        expected: bool,
        msg: Operand,
        target: BlockId,
        span: Span,
    },
    Return,
    /// Target for a diverging `Call`; also a bodyless infinite `for {}`.
    Unreachable,
}
