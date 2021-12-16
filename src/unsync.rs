use std::{
    alloc::{alloc, handle_alloc_error, Layout},
    cell::Cell,
    mem::ManuallyDrop,
    ops::Deref,
    ptr::{null_mut, NonNull},
};

use crate::metadata::{drop_in_place, metadata_of, TypeMetadata};

pub struct ProjectRc<T: ?Sized> {
    inner: NonNull<RcInner>,
    pointer: NonNull<T>,
}

impl<T> ProjectRc<T> {
    pub fn new(thing: T) -> Self {
        let meta = metadata_of::<T>();
        let layout = rc_inner_layout(meta);

        let ptr = unsafe { alloc(layout.layout) };

        if ptr == null_mut() {
            handle_alloc_error(layout.layout);
        }

        unsafe {
            // Write 0 as the strong count
            ptr.add(layout.strong_offset)
                .cast::<Cell<usize>>()
                .write(Cell::new(1));
            // Write the metadata
            ptr.add(layout.drop_offset)
                .cast::<TypeMetadata>()
                .write(meta);
            // Write the actual pointee
            ptr.add(layout.payload_offset).cast::<T>().write(thing);

            let inner_ptr = NonNull::new(ptr as *mut RcInner).unwrap();
            let payload_ptr = NonNull::new(ptr.add(layout.payload_offset) as *mut T).unwrap();

            ProjectRc {
                inner: inner_ptr,
                pointer: payload_ptr,
            }
        }
    }
}

impl<T: ?Sized> ProjectRc<T> {
    /// SAFETY: As long as self is alive, we will have one reference pointing to
    /// the inner. Therefore it shall be valid.
    fn inner(&self) -> &RcInner {
        unsafe { self.inner.as_ref() }
    }
}

impl<T: ?Sized> ProjectRc<T> {
    pub fn project<F, U: ?Sized>(self, f: F) -> ProjectRc<U>
    where
        F: for<'a> FnOnce(&'a T) -> &'a U,
    {
        let self_ = ManuallyDrop::new(self);
        let pointer = f(&**self_);

        ProjectRc {
            inner: self_.inner,
            pointer: pointer.into(),
        }
    }

    pub fn clone_project<F, U>(&self, f: F) -> ProjectRc<U>
    where
        F: for<'a> FnOnce(&'a T) -> &'a U,
    {
        self.clone().project(f)
    }
}

impl<T: ?Sized> Deref for ProjectRc<T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { self.pointer.as_ref() }
    }
}

impl<T: ?Sized> Clone for ProjectRc<T> {
    fn clone(&self) -> Self {
        let strong = self.inner().strong.get();
        self.inner().strong.set(strong + 1);

        ProjectRc {
            inner: self.inner,
            pointer: self.pointer,
        }
    }
}

impl<T: ?Sized> Drop for ProjectRc<T> {
    fn drop(&mut self) {
        let strong = self.inner().strong.get();
        self.inner().strong.set(strong - 1);

        if strong == 1 {
            unsafe {
                deallocate(self.inner);
            }
        }
    }
}

common_impls!(ProjectRc);

#[cfg(feature = "unsize")]
mod unsize_impl {
    use std::marker::Unsize;
    use std::ops::CoerceUnsized;

    impl<T, U> CoerceUnsized<ProjectRc<U>> for ProjectRc<T>
    where
        T: Unsize<U> + ?Sized,
        U: ?Sized,
    {
    }
}

#[repr(C)]
struct RcInner {
    strong: Cell<usize>,
    drop: TypeMetadata,
    // payload: [u8],
}

unsafe fn deallocate(inner: NonNull<RcInner>) {
    let meta = unsafe { (*inner.as_ptr()).drop };
    let layout = rc_inner_layout(meta);

    let inner_ptr = inner.as_ptr() as *mut u8;
    let payload = inner_ptr.add(layout.payload_offset) as *mut ();

    unsafe {
        drop_in_place(payload, meta);
        std::alloc::dealloc(inner_ptr, layout.layout);
    }
}

struct RcInnerLayout {
    layout: Layout,
    strong_offset: usize,
    drop_offset: usize,
    payload_offset: usize,
}

fn rc_inner_layout(meta: TypeMetadata) -> RcInnerLayout {
    let (layout, strong_offset) = (Layout::new::<Cell<usize>>(), 0);
    let (layout, drop_offset) = layout.extend(Layout::new::<TypeMetadata>()).unwrap();
    let (layout, payload_offset) = layout
        .extend(Layout::from_size_align(meta.size_of(), meta.align_of()).unwrap())
        .unwrap();
    let layout = layout.pad_to_align();

    RcInnerLayout {
        layout,
        strong_offset,
        drop_offset,
        payload_offset,
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
        let p1: ProjectRc<&str> = ProjectRc::new("Hello, world!");
        let p1 = p1.project(|self_| &self_[0..4]);

        assert_eq!(&*p1, "Hell");
    }
}
