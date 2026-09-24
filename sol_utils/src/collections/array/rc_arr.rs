use core::slice;
use std::{
    ops::{Deref, Index, RangeFull, RangeTo},
    range::{Range, RangeFrom, RangeInclusive, RangeToInclusive},
    rc::Rc,
};

/// A fixed-size array with cheap, shared cloning (`Rc<[T]>` under the hood).
///
/// Same 2-word fat-pointer size as [`Arr`](crate::collections::array::Arr),
/// but `.clone()` is an O(1) refcount bump instead of an O(n) deep copy — at
/// the cost of one extra heap allocation (the `Rc` control block) per
/// instance. Use `RcArr` only when the same array value is genuinely
/// cloned/shared repeatedly (e.g. a node looked up by id from a central
/// store and cloned out to many call sites); otherwise prefer
/// [`Arr`](crate::collections::array::Arr), whose refcount-free clone is
/// pure overhead when nothing shares the data. Reach for `Vec` when the list
/// is mutated (`push`/`pop`) after its initial construction.
#[derive(
    Debug, Clone, PartialEq, PartialOrd, Ord, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct RcArr<T>(Rc<[T]>);
impl<T> RcArr<T> {
    /// Creates a new, empty `RcArr` with no allocation.
    pub fn new() -> Self {
        Self(Rc::new([]))
    }

    /// Creates an `RcArr` of `size` elements, each a clone of `value`.
    pub fn with_size(size: usize, value: T) -> Self
    where
        T: Clone,
    {
        Self(Rc::from(vec![value; size]))
    }

    /// Converts a `Vec<T>` into an `RcArr<T>`, dropping the unused capacity.
    pub fn from_vec(v: Vec<T>) -> Self {
        Self(v.into_boxed_slice().into())
    }

    /// Creates an `RcArr` from a fixed-size array, with no intermediate `Vec`.
    pub fn from_array<const N: usize>(v: [T; N]) -> Self {
        Self(Rc::new(v))
    }

    /// Creates an `RcArr` by cloning every element out of `slice`.
    pub fn from_slice(slice: &[T]) -> Self
    where
        T: Clone,
    {
        Self(slice.to_vec().into_boxed_slice().into())
    }

    /// Returns the number of elements in the array.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns the first element, or `None` if the array is empty.
    pub fn first(&self) -> Option<&T> {
        self.0.first()
    }

    /// Returns the last element, or `None` if the array is empty.
    pub fn last(&self) -> Option<&T> {
        self.0.last()
    }

    /// Returns `true` if the array has no elements.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns the element at `index`, or `None` if out of bounds.
    pub fn get(&self, index: usize) -> Option<&T> {
        self.0.get(index)
    }

    /// Borrows the array's contents as an ordinary slice.
    pub fn as_slice(&self) -> &[T] {
        &self.0
    }

    /// Returns an iterator over the array's elements, by reference.
    pub fn iter(&self) -> slice::Iter<'_, T> {
        self.0.iter()
    }

    /// Returns `true` if `a` and `b` share the same backing allocation —
    /// i.e. one was cloned from the other rather than built separately.
    pub fn ptr_eq(a: &Self, b: &Self) -> bool {
        Rc::ptr_eq(&a.0, &b.0)
    }
}
impl<T> Deref for RcArr<T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<T> AsRef<[T]> for RcArr<T> {
    fn as_ref(&self) -> &[T] {
        self.deref()
    }
}

impl<T> Default for RcArr<T> {
    fn default() -> Self {
        Self::new()
    }
}
impl<T> Index<usize> for RcArr<T> {
    type Output = T;

    fn index(&self, index: usize) -> &Self::Output {
        &self.0[index]
    }
}
impl<T> Index<Range<usize>> for RcArr<T> {
    type Output = [T];

    fn index(&self, index: Range<usize>) -> &Self::Output {
        &self.0[index]
    }
}

impl<T> Index<RangeInclusive<usize>> for RcArr<T> {
    type Output = [T];

    fn index(&self, index: RangeInclusive<usize>) -> &Self::Output {
        &self.0[index]
    }
}

impl<T> Index<RangeTo<usize>> for RcArr<T> {
    type Output = [T];

    fn index(&self, index: RangeTo<usize>) -> &Self::Output {
        &self.0[index]
    }
}

impl<T> Index<RangeFrom<usize>> for RcArr<T> {
    type Output = [T];

    fn index(&self, index: RangeFrom<usize>) -> &Self::Output {
        &self.0[index]
    }
}

impl<T> Index<RangeFull> for RcArr<T> {
    type Output = [T];

    fn index(&self, _index: RangeFull) -> &Self::Output {
        &self.0[..]
    }
}

impl<T> Index<RangeToInclusive<usize>> for RcArr<T> {
    type Output = [T];

    fn index(&self, index: RangeToInclusive<usize>) -> &Self::Output {
        &self.0[index]
    }
}

impl<T> From<Vec<T>> for RcArr<T> {
    fn from(v: Vec<T>) -> Self {
        Self::from_vec(v)
    }
}

impl<T> FromIterator<T> for RcArr<T> {
    fn from_iter<I>(iter: I) -> Self
    where
        I: IntoIterator<Item = T>,
    {
        Self::from_vec(iter.into_iter().collect())
    }
}

impl<'a, T> IntoIterator for &'a RcArr<T> {
    type Item = &'a T;
    type IntoIter = slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
