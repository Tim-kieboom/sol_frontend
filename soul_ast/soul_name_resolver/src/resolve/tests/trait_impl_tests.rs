use ast_model::fault::AstErrorKind;

use crate::resolve::tests::{fault_count_matching, resolve_source};



#[test]
fn conforming_impl_resolves_without_faults() {
    let ast = resolve_source(
        "trait Greeter {\n    greet(&this): i64\n}\nstruct Bar {\n    n: i64\n}\nuse Bar {\n    impl Greeter {\n        greet(&this): i64 => this.n\n    }\n}\nmain() {\n    b: Bar = Bar{n: 5}\n    x := b.greet()\n}\n",
    );
    assert_eq!(ast.faults().iter().count(), 0);
}

#[test]
fn impl_missing_a_trait_method_reports_exactly_one_fault() {
    let ast = resolve_source(
        "trait Greeter {\n    greet(&this): i64\n    farewell(&this): i64\n}\nstruct Bar {\n    n: i64\n}\nuse Bar {\n    impl Greeter {\n        greet(&this): i64 => this.n\n    }\n}\n",
    );
    assert_eq!(
        fault_count_matching(&ast, |k| matches!(
            k,
            AstErrorKind::ImplMissingTraitMethod { .. }
        )),
        1
    );
}

#[test]
fn impl_with_a_method_the_trait_does_not_declare_reports_exactly_one_fault() {
    let ast = resolve_source(
        "trait Greeter {\n    greet(&this): i64\n}\nstruct Bar {\n    n: i64\n}\nuse Bar {\n    impl Greeter {\n        greet(&this): i64 => this.n\n        farewell(&this): i64 => this.n\n    }\n}\n",
    );
    assert_eq!(
        fault_count_matching(&ast, |k| matches!(
            k,
            AstErrorKind::ImplHasExtraTraitMethod { .. }
        )),
        1
    );
}

#[test]
fn impl_method_with_mismatched_return_type_reports_exactly_one_fault() {
    let ast = resolve_source(
        "trait Greeter {\n    greet(&this): i64\n}\nstruct Bar {\n    n: i64\n}\nuse Bar {\n    impl Greeter {\n        greet(&this): none => {}\n    }\n}\n",
    );
    assert_eq!(
        fault_count_matching(&ast, |k| matches!(
            k,
            AstErrorKind::ImplTraitMethodSignatureMismatch { .. }
        )),
        1
    );
}

#[test]
fn impl_of_an_undeclared_trait_name_reports_exactly_one_fault() {
    let ast = resolve_source(
        "struct Bar {\n    n: i64\n}\nuse Bar {\n    impl NotATrait {\n        greet(&this): i64 => this.n\n    }\n}\n",
    );
    assert_eq!(
        fault_count_matching(&ast, |k| matches!(
            k,
            AstErrorKind::ImplTargetIsNotATrait { .. }
        )),
        1
    );
}

#[test]
fn calling_a_same_named_method_from_two_trait_impls_is_ambiguous() {
    let ast = resolve_source(
        "trait A {\n    foo(&this): i64\n}\ntrait B {\n    foo(&this): i64\n}\nstruct Bar {\n    n: i64\n}\nuse Bar {\n    impl A {\n        foo(&this): i64 => this.n\n    }\n}\nuse Bar {\n    impl B {\n        foo(&this): i64 => this.n\n    }\n}\nmain() {\n    b: Bar = Bar{n: 1}\n    x := b.foo()\n}\n",
    );
    assert_eq!(
        fault_count_matching(&ast, |k| matches!(
            k,
            AstErrorKind::AmbiguousMethodCall { .. }
        )),
        1
    );
}
