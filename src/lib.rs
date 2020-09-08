#![feature(extern_types)]
#![feature(unsize)]
#![feature(coerce_unsized)]

use ptr::NonNull;
use std::{
    cell::Cell, marker::PhantomData, marker::Unsize, mem, ops::CoerceUnsized, ops::Deref, ptr,
};

type TypedDropFn<T> = fn(Box<T>);
type ErasedDropFn = fn(Box<()>);
fn do_drop<T: ?Sized>(_: Box<T>) {}

struct ProjectionInfo {
    strong: Cell<usize>,
    erased_base: NonNull<()>,
    erased_drop_fn: ErasedDropFn,
}

pub struct ProjectRc<T: ?Sized> {
    info: NonNull<ProjectionInfo>,
    data: NonNull<T>,
    phantom: PhantomData<T>,
}

impl<T> ProjectRc<T> {
    pub fn new(t: T) -> ProjectRc<T> {
        Self::from_box(Box::new(t))
    }
}

impl<T: ?Sized> ProjectRc<T> {
    pub fn from_box(t: Box<T>) -> ProjectRc<T> {
        let data = Box::leak(t);
        // Erase the type of this data, because we don't want the obligation of
        // carrying this around. This is the point of the ProjectRc.
        let erased_base = unsafe { NonNull::new_unchecked(data as *mut T as *mut ()) };
        let erased_drop_fn =
            unsafe { mem::transmute::<TypedDropFn<T>, ErasedDropFn>(do_drop::<T>) };

        ProjectRc {
            info: Box::leak(Box::new(ProjectionInfo {
                strong: Cell::new(1),
                erased_base,
                erased_drop_fn,
            }))
            .into(),
            data: data.into(),
            phantom: PhantomData,
        }
    }

    pub unsafe fn from_raw(t: *mut T) -> ProjectRc<T> {
        Self::from_box(Box::from_raw(t))
    }
}

impl<T: ?Sized> Drop for ProjectRc<T> {
    fn drop(&mut self) {
        let mut info = self.info;
        let strong_count = unsafe { info.as_ref() }.strong.get();

        if strong_count == 1 {
            unsafe {
                let mut erased_base = info.as_ref().erased_base;
                let erased_drop_fn = info.as_ref().erased_drop_fn;
                ptr::drop_in_place(info.as_mut());
                erased_drop_fn(Box::from_raw(erased_base.as_mut()))
            }
        } else {
            unsafe {
                self.info.as_ref().strong.set(strong_count - 1);
            }
        }
    }
}

impl<T: ?Sized> Clone for ProjectRc<T> {
    fn clone(&self) -> Self {
        unsafe {
            let info = self.info.as_ref();
            info.strong.set(info.strong.get() + 1);
        }

        ProjectRc {
            info: self.info,
            data: self.data,
            phantom: PhantomData,
        }
    }
}

impl<T: ?Sized> Deref for ProjectRc<T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { self.data.as_ref() }
    }
}

impl<T: ?Sized, U: ?Sized> CoerceUnsized<ProjectRc<U>> for ProjectRc<T> where T: Unsize<U> {}

impl<T: ?Sized> ProjectRc<T> {
    pub fn project<S: ?Sized>(self, f: impl for<'a> Fn(&'a T) -> &'a S) -> ProjectRc<S> {
        let info = self.info;
        let data = self.data;
        std::mem::forget(self);

        ProjectRc {
            info,
            data: unsafe { f(data.as_ref()) }.into(),
            phantom: PhantomData,
        }
    }
}

impl<T: ?Sized> ProjectRc<T>
where
    T: Deref,
{
    pub fn project_deref(self) -> ProjectRc<T::Target> {
        self.project(|self_| self_.deref())
    }
}

impl<T: ?Sized> From<Box<T>> for ProjectRc<T> {
    fn from(b: Box<T>) -> Self {
        Self::from_box(b)
    }
}

impl From<&'static str> for ProjectRc<str> {
    fn from(s: &'static str) -> Self {
        Self::from_box(s.into())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    struct SideEffect<T, F: Fn(&mut T)>(T, F);

    impl<T, F: Fn(&mut T)> Drop for SideEffect<T, F> {
        fn drop(&mut self) {
            (self.1)(&mut self.0)
        }
    }

    #[test]
    fn ref_counting() {
        let dropped = &Cell::new(false);

        let p1 = ProjectRc::new(SideEffect(12345, |_| {
            dropped.set(true);
        }));
        assert!(!dropped.get());
        assert_eq!((*p1).0, 12345);

        let p2 = p1.clone();
        assert_eq!((*p1).0, 12345);
        assert!(!dropped.get());

        drop(p1);
        assert_eq!((*p2).0, 12345);
        assert!(!dropped.get());

        drop(p2);
        // YES dropped
        assert!(dropped.get());
    }

    #[test]
    fn projected_ref_counting() {
        let dropped = &Cell::new(false);

        let p1 = ProjectRc::new(SideEffect(12345, |_| {
            dropped.set(true);
        }));
        assert!(!dropped.get());

        let p2 = p1.project(|self_| &self_.0);
        assert_eq!(*p2, 12345);
        assert!(!dropped.get());

        drop(p2);
        assert!(dropped.get());
    }

    #[test]
    fn project_slice() {
        let p1: ProjectRc<[i32]> = ProjectRc::new([1, 2, 3]);

        let p2 = p1.clone().project(|self_| &self_[0]);
        let p3 = p1.project(|self_| &self_[1..]);

        assert_eq!(*p2, 1);
        assert_eq!(*p3, [2, 3]);
    }

    #[test]
    fn project_deref() {
        let p1: ProjectRc<Vec<i32>> = ProjectRc::new(vec![1, 2, 3]);
        let p2: ProjectRc<[i32]> = p1.project_deref();

        assert_eq!(*p2, [1, 2, 3]);
    }

    #[test]
    fn subslices() {
        let p1: ProjectRc<str> = "Hello, world!".into();
        let p1 = p1.project(|self_| &self_[0..4]);

        assert_eq!(&*p1, "Hell");
    }
}
