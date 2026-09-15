use core::slice;
use std::{
    ops::{Deref, Index},
};

/// A fixed-size, exclusively-owned array (`Box<[T]>` under the hood).
///
/// Same 2-word fat-pointer size as [`RcArr`](crate::collections::array::RcArr),
/// but `.clone()` is an O(n) deep copy rather than an O(1) refcount bump. Use
/// `Arr` for a list that is built once and only ever read by reference
/// afterward — it has no sharing overhead. Reach for
/// [`RcArr`](crate::collections::array::RcArr) instead when the same array
/// value is genuinely cloned/shared across multiple owners; reach for `Vec`
/// when the list is mutated (`push`/`pop`) after its initial construction.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Arr<T>(Box<[T]>);
impl<T> Arr<T> {
    /// Creates a new, empty `Arr` with no allocation.
    pub fn new() -> Self {
        Self(Box::new([]))
    }

    /// Creates an `Arr` of `size` elements, each a clone of `value`.
    pub fn with_size(size: usize, value: T) -> Self
    where
        T: Clone,
    {
        Self(Box::from(vec![value; size]))
    }

    /// Converts a `Vec<T>` into an `Arr<T>`, dropping the unused capacity.
    pub fn from_vec(v: Vec<T>) -> Self {
        Self(v.into_boxed_slice())
    }

    /// Creates an `Arr` from a fixed-size array, with no intermediate `Vec`.
    pub fn from_array<const N: usize>(v: [T; N]) -> Self {
        Self(Box::new(v))
    }

    /// Creates an `Arr` by cloning every element out of `slice`.
    pub fn from_slice(slice: &[T]) -> Self
    where
        T: Clone,
    {
        Self(slice.to_vec().into_boxed_slice())
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
}
impl<T> Deref for Arr<T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<T> AsRef<[T]> for Arr<T> {
    fn as_ref(&self) -> &[T] {
        self.deref()
    }
}

impl<T> Default for Arr<T> {
    fn default() -> Self {
        Self::new()
    }
}
impl<T> Index<usize> for Arr<T> {
    type Output = T;

    fn index(&self, index: usize) -> &Self::Output {
        &self.0[index]
    }
}

impl<T> From<Vec<T>> for Arr<T> {
    fn from(v: Vec<T>) -> Self {
        Self::from_vec(v)
    }
}

impl<T> FromIterator<T> for Arr<T> {
    fn from_iter<I>(iter: I) -> Self
    where
        I: IntoIterator<Item = T>,
    {
        Self::from_vec(iter.into_iter().collect())
    }
}

impl<'a, T> IntoIterator for &'a Arr<T> {
    type Item = &'a T;
    type IntoIter = slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
