use ast_model::{AstTree, fault::AstErrorKind};

use crate::resolve::tests::resolve_source;



fn intrinsic_fault_count(ast: &AstTree) -> usize {
    ast.faults()
        .iter()
        .filter(|fault| {
            matches!(
                fault.kind(),
                AstErrorKind::UnknownIntrinsic { .. } | AstErrorKind::IntrinsicArityMismatch { .. }
            )
        })
        .count()
}

#[test]
fn known_intrinsics_with_correct_arity_resolve_without_faults() {
    let ast = resolve_source(
        r#"
main() {
    a := intrinsic.fieldIndex(int, 0)
    b := intrinsic.fieldCount(int)
    c := intrinsic.typeinfo(int)
    d := intrinsic.ptr.offset(int, 1)
}
"#,
    );

    assert_eq!(intrinsic_fault_count(&ast), 0);
}

#[test]
fn unknown_intrinsic_reports_exactly_one_fault() {
    let ast = resolve_source(
        r#"
main() {
    a := intrinsic.doesNotExist(1)
}
"#,
    );

    assert_eq!(intrinsic_fault_count(&ast), 1);
}

#[test]
fn wrong_arity_reports_exactly_one_fault() {
    let ast = resolve_source(
        r#"
main() {
    a := intrinsic.typeinfo()
}
"#,
    );

    assert_eq!(intrinsic_fault_count(&ast), 1);
}
