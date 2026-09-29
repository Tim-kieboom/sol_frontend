use crate::{ids::IdGenerator, impl_sol_ids};

impl_sol_ids!(TestId);
type TestIdGenerator = IdGenerator<TestId>;

#[test]
fn first_alloc_in_id_generator_should_be_1() {
    let mut generator = TestIdGenerator::new();
    assert_eq!(generator.alloc(), TestId(1));
}

#[test]
fn alloc_in_id_generator_should_increment_the_id() {
    let mut generator = TestIdGenerator::new();
    assert_eq!(generator.alloc(), TestId(1));
    assert_eq!(generator.alloc(), TestId(2));
}
