
pub trait Spliterator: Sized {
    type Item;

    fn len(&self) -> usize;

    fn split(self, at: usize) -> (Self, Self);

    fn next(&mut self) -> Self::Item;


    #[inline]
    fn copied<'a, T>(self) -> Copied<Self>
    where T: 'a, Self: Spliterator<Item = &'a T>
    {
        Copied { inner: self }
    }

    #[inline]
    fn enumerate(self) -> Enumerate<Self> {
        Enumerate { idx: 0, inner: self }
    }

    #[inline]
    fn zip_exact<I2: Spliterator>(self, other: I2) -> Zip<Self, I2> {
        assert_eq!(self.len(), other.len());
        Zip { i1: self, i2: other }
    }
}

pub trait IntoSpliterator {
    type I: Spliterator<Item = Self::Item>;
    type Item;

    fn into_spliterator(self) -> Self::I;
}


pub trait IntoSpliteratorRef<'a> {
    type I: Spliterator + 'a;

    fn spliter(&'a self) -> Self::I;
}

impl<'a, T: ?Sized + 'a> IntoSpliteratorRef<'a> for T  where &'a T: IntoSpliterator {
    type I = <&'a T as IntoSpliterator>::I;

    #[inline]
    fn spliter(&'a self) -> Self::I {
        IntoSpliterator::into_spliterator(self)
    }
}


pub trait IntoSpliteratorRefMut<'a> {
    type I: Spliterator + 'a;

    fn spliter_mut(&'a mut self) -> Self::I;
}

impl<'a, T: ?Sized + 'a> IntoSpliteratorRefMut<'a> for T  where &'a mut T: IntoSpliterator {
    type I = <&'a mut T as IntoSpliterator>::I;

    #[inline]
    fn spliter_mut(&'a mut self) -> Self::I {
        IntoSpliterator::into_spliterator(self)
    }
}


pub mod slice {
    use super::{Spliterator, IntoSpliterator};

    pub struct Spliter<'a, T> {
        pub inner: core::slice::Iter<'a, T>,
    }

    impl<'a, T> Spliterator for Spliter<'a, T> {
        type Item = &'a T;

        #[inline]
        fn len(&self) -> usize {
            self.inner.as_slice().len()
        }

        #[inline]
        fn split(self, at: usize) -> (Self, Self) {
            let slice = self.inner.as_slice();
            let (lhs, rhs) = slice.split_at(at);
            (Spliter { inner: lhs.iter() }, Spliter { inner: rhs.iter() })
        }

        #[inline]
        fn next(&mut self) -> Self::Item {
            Iterator::next(&mut self.inner).expect("unreachable")
        }
    }

    impl<'a, T> IntoSpliterator for &'a [T] {
        type I = Spliter<'a, T>;
        type Item = &'a T;

        #[inline]
        fn into_spliterator(self) -> Self::I {
            Spliter { inner: self.iter() }
        }
    }


    pub struct SpliterMut<'a, T> {
        pub inner: core::slice::IterMut<'a, T>,
    }

    impl<'a, T> Spliterator for SpliterMut<'a, T> {
        type Item = &'a mut T;

        #[inline]
        fn len(&self) -> usize {
            self.inner.as_slice().len()
        }

        #[inline]
        fn split(self, at: usize) -> (Self, Self) {
            let slice = self.inner.into_slice();
            let (lhs, rhs) = slice.split_at_mut(at);
            (SpliterMut { inner: lhs.iter_mut() }, SpliterMut { inner: rhs.iter_mut() })
        }

        #[inline]
        fn next(&mut self) -> Self::Item {
            Iterator::next(&mut self.inner).expect("unreachable")
        }
    }

    impl<'a, T> IntoSpliterator for &'a mut [T] {
        type I = SpliterMut<'a, T>;
        type Item = &'a mut T;

        #[inline]
        fn into_spliterator(self) -> Self::I {
            SpliterMut { inner: self.iter_mut() }
        }
    }
}


pub mod vec {
    use super::IntoSpliterator;
    use sti::alloc::Alloc;
    use sti::vec::Vec;


    impl<'a, T, A: Alloc> IntoSpliterator for &'a Vec<T, A> {
        type I = super::slice::Spliter<'a, T>;
        type Item = &'a T;

        #[inline]
        fn into_spliterator(self) -> Self::I {
            (&**self).into_spliterator()
        }
    }


    impl<'a, T, A: Alloc> IntoSpliterator for &'a mut Vec<T, A> {
        type I = super::slice::SpliterMut<'a, T>;
        type Item = &'a mut T;

        #[inline]
        fn into_spliterator(self) -> Self::I {
            (&mut **self).into_spliterator()
        }
    }


    // @todo: IntoIter.
    //  not sure how we wanna handle dealloc..
}


#[derive(Clone, Copy, Debug)]
pub struct Copied<I> {
    pub inner: I,
}

impl<'a, T: 'a, I: Spliterator<Item = &'a T>> Spliterator for Copied<I> {
    type Item = I::Item;

    #[inline]
    fn len(&self) -> usize {
        self.inner.len()
    }

    #[inline]
    fn split(self, at: usize) -> (Self, Self) {
        let (lhs, rhs) = self.inner.split(at);
        (Copied { inner: lhs }, Copied { inner: rhs })
    }

    #[inline]
    fn next(&mut self) -> Self::Item {
        self.inner.next()
    }
}


#[derive(Clone, Copy, Debug)]
pub struct Enumerate<I> {
    pub idx: usize,
    pub inner: I,
}

impl<'a, T: 'a, I: Spliterator<Item = &'a T>> Spliterator for Enumerate<I> {
    type Item = (usize, I::Item);

    #[inline]
    fn len(&self) -> usize {
        self.inner.len()
    }

    #[inline]
    fn split(self, at: usize) -> (Self, Self) {
        let (lhs, rhs) = self.inner.split(at);
        (Enumerate { idx: self.idx,      inner: lhs },
         Enumerate { idx: self.idx + at, inner: rhs })
    }

    #[inline]
    fn next(&mut self) -> Self::Item {
        (sti::inc!(&mut self.idx), self.inner.next())
    }
}


#[derive(Clone, Copy, Debug)]
pub struct Zip<I1, I2> {
    pub i1: I1,
    pub i2: I2,
}

impl<I1: Spliterator, I2: Spliterator> Spliterator for Zip<I1, I2> {
    type Item = (I1::Item, I2::Item);

    #[inline]
    fn len(&self) -> usize {
        self.i1.len()
    }

    #[inline]
    fn split(self, at: usize) -> (Self, Self) {
        let (i11, i12) = self.i1.split(at);
        let (i21, i22) = self.i2.split(at);
        (Zip { i1: i11, i2: i21 }, Zip { i1: i12, i2: i22 })
    }

    #[inline]
    fn next(&mut self) -> Self::Item {
        (self.i1.next(), self.i2.next())
    }
}


