//! Enumerate helper for `IntoIterator` values.

/// Enumerate any [`IntoIterator`] without first converting it to an iterator
pub trait IterEnumerate {
    /// The item type of the iterator
    type Item;
    /// Iterate with indices
    fn iter_enumerate(self) -> impl Iterator<Item = (usize, Self::Item)>;
}

impl<V, I: IntoIterator<Item = V>> IterEnumerate for I {
    type Item = V;
    fn iter_enumerate(self) -> impl Iterator<Item = (usize, Self::Item)> {
        self.into_iter().enumerate()
    }
}
