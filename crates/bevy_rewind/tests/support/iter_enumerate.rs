pub trait IterEnumerate {
    type Item;
    fn iter_enumerate(self) -> impl Iterator<Item = (usize, Self::Item)>;
}

impl<V, I: IntoIterator<Item = V>> IterEnumerate for I {
    type Item = V;
    fn iter_enumerate(self) -> impl Iterator<Item = (usize, Self::Item)> {
        self.into_iter().enumerate()
    }
}
