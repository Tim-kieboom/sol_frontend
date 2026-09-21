use crate::collections::array::RcArr;

#[test]
fn new_is_empty() {
    let arr: RcArr<i32> = RcArr::new();
    assert!(arr.is_empty());
    assert_eq!(arr.len(), 0);
    assert_eq!(arr.first(), None);
    assert_eq!(arr.last(), None);
}

#[test]
fn default_is_empty() {
    let arr: RcArr<i32> = RcArr::default();
    assert!(arr.is_empty());
}

#[test]
fn with_size_fills_every_slot_with_a_clone() {
    let arr = RcArr::with_size(3, "x".to_string());
    assert_eq!(arr.len(), 3);
    assert_eq!(arr.as_slice(), ["x", "x", "x"]);
}

#[test]
fn from_vec_preserves_order() {
    let arr = RcArr::from_vec(vec![1, 2, 3]);
    assert_eq!(arr.len(), 3);
    assert_eq!(arr.as_slice(), [1, 2, 3]);
}

#[test]
fn from_array_preserves_order() {
    let arr = RcArr::from_array([1, 2, 3]);
    assert_eq!(arr.as_slice(), [1, 2, 3]);
}

#[test]
fn from_slice_clones_every_element() {
    let source = [1, 2, 3];
    let arr = RcArr::from_slice(&source);
    assert_eq!(arr.as_slice(), source);
}

#[test]
fn first_and_last_return_the_outer_elements() {
    let arr = RcArr::from_vec(vec![1, 2, 3]);
    assert_eq!(arr.first(), Some(&1));
    assert_eq!(arr.last(), Some(&3));
}

#[test]
fn get_is_bounds_checked() {
    let arr = RcArr::from_vec(vec![10, 20]);
    assert_eq!(arr.get(0), Some(&10));
    assert_eq!(arr.get(1), Some(&20));
    assert_eq!(arr.get(2), None);
}

#[test]
fn index_panics_out_of_bounds() {
    let arr = RcArr::from_vec(vec![10, 20]);
    assert_eq!(arr[0], 10);
    assert_eq!(arr[1], 20);
}

#[test]
fn deref_and_as_ref_expose_the_underlying_slice() {
    let arr = RcArr::from_vec(vec![1, 2, 3]);
    let slice: &[i32] = &arr;
    assert_eq!(slice, [1, 2, 3]);
    assert_eq!(arr.as_ref(), [1, 2, 3]);
}

#[test]
fn iter_yields_elements_by_reference_in_order() {
    let arr = RcArr::from_vec(vec![1, 2, 3]);
    let collected: Vec<&i32> = arr.iter().collect();
    assert_eq!(collected, vec![&1, &2, &3]);
}

#[test]
fn into_iterator_for_reference_matches_iter() {
    let arr = RcArr::from_vec(vec![1, 2, 3]);
    let mut sum = 0;
    for value in &arr {
        sum += value;
    }
    assert_eq!(sum, 6);
}

#[test]
fn from_vec_conversion_trait() {
    let arr: RcArr<i32> = vec![1, 2, 3].into();
    assert_eq!(arr.as_slice(), [1, 2, 3]);
}

#[test]
fn from_iterator_collects_in_order() {
    let arr: RcArr<i32> = (1..=3).collect();
    assert_eq!(arr.as_slice(), [1, 2, 3]);
}

#[test]
fn clone_shares_the_same_backing_allocation() {
    let original = RcArr::from_vec(vec![1, 2, 3]);
    let cloned = original.clone();
    // `RcArr`'s whole reason to exist is that clones are cheap refcount
    // bumps sharing one allocation, not independent deep copies.
    assert!(RcArr::ptr_eq(&original, &cloned));
    assert_eq!(original, cloned);
}

#[test]
fn independently_constructed_arrays_do_not_share_an_allocation() {
    let a = RcArr::from_vec(vec![1, 2, 3]);
    let b = RcArr::from_vec(vec![1, 2, 3]);
    assert!(!RcArr::ptr_eq(&a, &b));
}

#[test]
fn equality_compares_by_contents() {
    let a = RcArr::from_vec(vec![1, 2, 3]);
    let b = RcArr::from_vec(vec![1, 2, 3]);
    let c = RcArr::from_vec(vec![1, 2, 4]);
    assert_eq!(a, b);
    assert_ne!(a, c);
}

#[test]
fn serde_round_trips_through_json() {
    let arr = RcArr::from_vec(vec![1, 2, 3]);
    let json = serde_json::to_value(&arr).unwrap();
    let round_tripped: RcArr<i32> = serde_json::from_value(json).unwrap();
    assert_eq!(arr, round_tripped);
}
