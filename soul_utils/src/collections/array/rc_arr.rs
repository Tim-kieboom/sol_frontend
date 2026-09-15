use core::slice;
use std::{
    ops::{Deref, Index},
    rc::Rc,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct RcArr<T>(Rc<[T]>);
impl<T> RcArr<T> {
    pub fn new() -> Self {
        Self(Rc::new([]))
    }

    pub fn with_size(size: usize, value: T) -> Self
    where
        T: Clone,
    {
        Self(Rc::from(vec![value; size]))
    }

    pub fn from_vec(v: Vec<T>) -> Self {
        Self(v.into_boxed_slice().into())
    }

    pub fn from_array<const N: usize>(v: [T; N]) -> Self {
        Self(Rc::new(v))
    }

    pub fn from_slice(slice: &[T]) -> Self
    where
        T: Clone,
    {
        Self(slice.to_vec().into_boxed_slice().into())
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn first(&self) -> Option<&T> {
        self.0.first()
    }

    pub fn last(&self) -> Option<&T> {
        self.0.last()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<&T> {
        self.0.get(index)
    }

    pub fn as_slice(&self) -> &[T] {
        &self.0
    }

    pub fn iter(&self) -> slice::Iter<'_, T> {
        self.0.iter()
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
