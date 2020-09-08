#![feature(extern_types)]
#![feature(unsize)]
#![feature(coerce_unsized)]
#![warn(clippy::pedantic)]

use std::{
    cell::Cell,
    marker::{PhantomData, Unsize},
    mem,
    ops::{CoerceUnsized, Deref},
    ptr,
    ptr::NonNull,
};

// The "typed" flavor of a function which drops Box<T>
type TypedDropFn<T> = fn(Box<T>);
// The "untyped"/type-erased flavor of a function which drops Box<...>
type ErasedDropFn = fn(Box<()>);

/// The "information" struct, which contains the strong pointer count,
// the base of the memory allocation, and the drop function which is invoked
// when this memory is finally dropped.
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
        // Leak the data out of the box. We'll be taking the pointer to this
        // data which is the base of our future projections.
        let data = Box::leak(t);
        // Erase the type of this data, because we don't want the obligation of
        // carrying this around. This is the point of the ProjectRc.
        let erased_base = unsafe { NonNull::new_unchecked(data as *mut T as *mut ()) };
        // Also, we need to erase a "drop" fn for this type, which we'll pass
        // `erased_base` back into when the time is right to get rid of this data.
        let erased_drop_fn =
            unsafe { mem::transmute::<TypedDropFn<T>, ErasedDropFn>(mem::drop::<Box<T>>) };

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

    /// Make a ProjectRc from an (exclusively-owned) pointer to T.
    ///
    /// Safety:
    ///
    /// Must own the data pointed to by the pointer `t`.
    pub unsafe fn from_raw(t: *mut T) -> ProjectRc<T> {
        Self::from_box(Box::from_raw(t))
    }
}

impl<T: ?Sized> Drop for ProjectRc<T> {
    fn drop(&mut self) {
        let mut info = self.info;
        let strong_count = unsafe { info.as_ref() }.strong.get();

        // If we exclusively own this pointer,
        if strong_count == 1 {
            unsafe {
                // Get the erased base and drop fn back from the info struct.
                let mut erased_base = info.as_ref().erased_base;
                let erased_drop_fn = info.as_ref().erased_drop_fn;
                // Drop the info struct.
                ptr::drop_in_place(info.as_mut());
                // Then invoke the erased drop fn. This will both do any
                // drop-time functionality and also deallocate the data, since
                // we've boxed it back up.
                erased_drop_fn(Box::from_raw(erased_base.as_mut()))
            }
        } else {
            unsafe {
                // Otherwise decrement the strong-count.
                self.info.as_ref().strong.set(strong_count - 1);
            }
        }
    }
}

impl<T: ?Sized> Clone for ProjectRc<T> {
    fn clone(&self) -> Self {
        // Increment the strong-count and clone the info in the ProjectRc.
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
        // Deref'ing the ProjectRc is just deref'ing the data pointer.
        unsafe { self.data.as_ref() }
    }
}

// Spicy
impl<T: ?Sized, U: ?Sized> CoerceUnsized<ProjectRc<U>> for ProjectRc<T> where T: Unsize<U> {}

impl<T: ?Sized> ProjectRc<T> {
    // Given a projection fn (&T -> &S), project this ProjectRc<T> into
    // a ProjectRc<S>. The HRTB is here to ensure that we're as flexible as
    // possible with the projection function.
    pub fn project<S: ?Sized>(self, f: impl for<'a> Fn(&'a T) -> &'a S) -> ProjectRc<S> {
        let info = self.info;
        let data = self.data;

        // Forget self so we don't free the data if the projection function panicks.
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
    // Convenience function for projecting across a Deref::deref
    pub fn project_deref(self) -> ProjectRc<T::Target> {
        self.project(T::deref)
    }
}

/// Convenience from/into for taking ownership of a boxed type
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

impl From<String> for ProjectRc<str> {
    fn from(s: String) -> Self {
        Self::from_box(s.into())
    }
}

impl<T> From<Vec<T>> for ProjectRc<[T]> {
    fn from(s: Vec<T>) -> Self {
        Self::from_box(s.into())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    /// Convenient little struct that'll call the side-effect function F when
    // the struct is dropped.
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
