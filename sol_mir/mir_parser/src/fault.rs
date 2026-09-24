use sol_utils::fault::{Fault, UnclassifiedKind};

/// Structured error kinds for AST-to-MIR lowering. `Unclassified` is a migration
/// fallback carrying the raw message from call sites not yet converted to a real
/// variant.
#[derive(Debug, Clone, PartialEq, thiserror::Error, serde::Serialize, serde::Deserialize)]
pub enum MirErrorKind {
    #[error("{0}")]
    Unclassified(Box<str>),

    #[error("extern/signature-only declarations have no body to lower to MIR")]
    SignatureOnlyFunctionHasNoBody,

    #[error(
        "only simple (non-destructuring) variable bindings are supported in this lowering slice"
    )]
    NonSimpleVariablePatternUnsupported,

    #[error("a variable declaration with no initializer isn't supported in this lowering slice")]
    UninitializedVariableUnsupported,

    #[error("variable has no resolved type")]
    VariableHasNoResolvedType,

    #[error(
        "only a `return <expr>` statement is supported as a function's terminal statement in this lowering slice"
    )]
    NonReturnTerminalStatementUnsupported,

    #[error("this statement kind isn't supported in this lowering slice")]
    UnsupportedStatementKind,

    #[error("function has no `return <expr>` as its final reachable statement")]
    MissingReturnStatement,

    #[error("type `{ty}` isn't a primitive scalar, which is all this lowering slice supports")]
    NonPrimitiveType { ty: Box<str> },

    #[error(
        "only arithmetic (+ - * / %), comparison (== != < > <= >=), and logical (&& ||) binary operators are supported in this lowering slice"
    )]
    UnsupportedBinaryOperator,

    #[error("only the `!` (logical not) unary operator is supported in this lowering slice")]
    UnsupportedUnaryOperator,

    #[error("variable has no resolved binding")]
    VariableHasNoResolvedBinding,

    #[error("variable isn't bound to a local in this function's lowered scope")]
    VariableNotBoundToLocal,

    #[error("nested expression has no resolved type")]
    NestedExpressionHasNoResolvedType,

    #[error(
        "only literals, variables, arithmetic/comparison/logical binary expressions, `!`, and calls are supported as operands in this lowering slice"
    )]
    UnsupportedOperandExpression,

    #[error(
        "only a bare `bool` literal/variable, `!<bool>`, or a comparison/logical (`&&`/`||`) expression is supported as an `if`/`while` condition in this lowering slice"
    )]
    UnsupportedConditionExpression,

    #[error(
        "only `while <cond> {{ .. }}` loops are supported in this lowering slice, not bare `for` loops or `foreach`"
    )]
    UnsupportedLoopCondition,

    #[error("`break` outside of a loop")]
    BreakOutsideLoop,

    #[error("`continue` outside of a loop")]
    ContinueOutsideLoop,

    #[error("statement is unreachable: every preceding path already returned, broke, or continued")]
    UnreachableStatement,

    #[error(
        "only a bare (already-declared) variable or a struct field (`variable.field`) is supported as an assignment target in this lowering slice"
    )]
    AssignmentTargetUnsupported,

    #[error("function call has no resolved target")]
    FunctionCallHasNoResolvedTarget,

    #[error(
        "only a plain `name(args...)` free-function call is supported in this lowering slice — no method-call receiver, generics, named arguments, or `defer`"
    )]
    UnsupportedCallShape,

    #[error("expected `varargs` got `{expression_variant}`")]
    ExpectedVarargs { expression_variant: &'static str },

    #[error(
        "missing `varargs` at the and of the functioncall (empty varargs are explicit so `varargs.[]` if you want no arguments)"
    )]
    MissingVarargs,

    #[error("a `none`-returning call's result can't be used as a value")]
    CannotUseNoneValueAsOperand,

    #[error("`return <expr>` isn't valid in a `none`-returning function")]
    UnexpectedReturnValue,

    #[error("intrinsic `{name}` isn't supported in this lowering slice")]
    UnsupportedIntrinsic { name: Box<str> },

    /// The resolver logs a fault on an intrinsic arity mismatch but still
    /// stores the resolution and lets the call through — so a malformed
    /// `assert()`/`panic()` call can genuinely reach MIR lowering with the
    /// wrong argument count. This is the graceful fault for that, not a
    /// defensive/unreachable one: guard the argument index with it instead
    /// of indexing `call.arguments` directly.
    #[error("intrinsic `{name}` expects {expected} argument(s), got {got}")]
    IntrinsicArityMismatch {
        name: Box<str>,
        expected: usize,
        got: usize,
    },

    #[error(
        "only a variable, a field access (`object.field`), or an index (`collection[i]`) — any nesting of those — is supported as a place expression in this lowering slice"
    )]
    UnsupportedPlaceExpression,

    #[error("`..` default-filled struct constructors aren't supported in this lowering slice")]
    StructConstructorDefaultsUnsupported,

    /// The resolver already rejects a struct constructor missing a field
    /// value, or a field-access naming a field the struct doesn't have
    /// (`StructHasNoField`), before this ever runs — reaching this means the
    /// resolver let bad input through, same defensive category as
    /// `UnexpectedReturnValue`.
    #[error("struct `{struct_name}` has no field `{field}`")]
    StructFieldNotFound {
        struct_name: Box<str>,
        field: Box<str>,
    },

    #[error(
        "only indexing into a slice (`[&]T`/`[&mut]T`) is supported in this lowering slice, not `{ty}`"
    )]
    IndexTargetNotASlice { ty: Box<str> },

    #[error(
        "only `&`/`@` on a fixed-size array (`[N]T`) is supported (to produce a slice) in this lowering slice, not `{ty}`"
    )]
    ArrayReferenceUnsupported { ty: Box<str> },

    #[error("`*` can only dereference a reference/pointer type, not `{ty}`")]
    DerefTargetNotAReference { ty: Box<str> },

    /// The move checker's own violation (see `mir_parser::move_check`).
    /// Points at the moved-out local's own declaration span — MIR statements
    /// don't carry their own per-statement spans yet, so the *exact* source
    /// location of the violating read can't be reported, only where the
    /// value in question was introduced. Handles the full concrete (M1) CFG
    /// shape, `for` loops included, via a real fixed-point dataflow — see
    /// `move_check`'s own docs.
    #[error("value may have already been moved out of")]
    UseAfterMove,

    /// A move-only value moved out through a reference, an owning pointer,
    /// or an array index — there is no owner left whose drop could account
    /// for the hole. Points at the root local's own declaration span.
    #[error("cannot move a value out from behind a reference, pointer, or index")]
    MoveOutOfBorrow,

    /// The escape checker's own violation (see `mir_parser::escape_check`):
    /// a returned value, or a value stored through a `&mut` parameter, holds
    /// a reference into storage that doesn't outlive the function. Points at
    /// the referenced local's own declaration span, for the same reason
    /// `UseAfterMove` does — MIR statements don't carry their own
    /// per-statement spans yet.
    #[error("a reference to a local escapes the function it doesn't outlive")]
    DanglingReference,

    /// The overlap checker's own violation (see
    /// `mir_parser::borrow_checker::overlap_check`). Points at the
    /// later-created borrow's own declaration span. Whole-locals only (no
    /// per-field disjointness) and reference-vs-reference only — a live
    /// borrow blocking a *move* of its target is `MoveWhileBorrowed` below,
    /// a separate check.
    #[error("a mutable borrow overlaps another live borrow of the same value")]
    OverlappingBorrows,

    /// `mir_parser::borrow_checker::overlap_check::check_move_while_borrowed`'s
    /// own violation — a move of a local, or of one of its fields, while
    /// some still-needed borrow (mutable or shared) overlapping it is live.
    /// Points at the moved local's own declaration span, same reasoning as
    /// `UseAfterMove`.
    #[error("value is moved while it is still borrowed")]
    MoveWhileBorrowed,
}

impl From<UnclassifiedKind> for MirErrorKind {
    fn from(value: UnclassifiedKind) -> Self {
        MirErrorKind::Unclassified(value.into_box_str())
    }
}
impl From<MirErrorKind> for UnclassifiedKind {
    fn from(value: MirErrorKind) -> Self {
        UnclassifiedKind::new(value.to_string().into_boxed_str())
    }
}

pub type MirFault = Fault<MirErrorKind>;
pub type MirResult<T> = std::result::Result<T, MirFault>;
