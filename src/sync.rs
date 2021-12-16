use std::{
    alloc::{alloc, handle_alloc_error, Layout},
    mem::ManuallyDrop,
    ops::Deref,
    ptr::{null_mut, NonNull},
    sync::atomic::{AtomicUsize, Ordering},
};

use crate::metadata::{drop_in_place, metadata_of, TypeMetadata};

pub struct ProjectArc<T: ?Sized> {
    inner: NonNull<ArcInner>,
    pointer: NonNull<T>,
}

impl<T> ProjectArc<T> {
    pub fn new(thing: T) -> Self {
        let meta = metadata_of::<T>();
        let layout = arc_inner_layout(meta);

        let ptr = unsafe { alloc(layout.layout) };

        if ptr == null_mut() {
            handle_alloc_error(layout.layout);
        }

        unsafe {
            // Write 0 as the strong count
            ptr.add(layout.strong_offset)
                .cast::<AtomicUsize>()
                .write(AtomicUsize::new(1));
            // Write the metadata
            ptr.add(layout.drop_offset)
                .cast::<TypeMetadata>()
                .write(meta);
            // Write the actual pointee
            ptr.add(layout.payload_offset).cast::<T>().write(thing);

            let inner_ptr = NonNull::new(ptr as *mut ArcInner).unwrap();
            let payload_ptr = NonNull::new(ptr.add(layout.payload_offset) as *mut T).unwrap();

            ProjectArc {
                inner: inner_ptr,
                pointer: payload_ptr,
            }
        }
    }
}

impl<T: ?Sized> ProjectArc<T> {
    /// SAFETY: As long as self is alive, we will have one reference pointing to
    /// the inner. Therefore it shall be valid.
    fn inner(&self) -> &ArcInner {
        unsafe { self.inner.as_ref() }
    }
}

impl<T: ?Sized> ProjectArc<T> {
    pub fn project<F, U: ?Sized>(self, f: F) -> ProjectArc<U>
    where
        F: for<'a> FnOnce(&'a T) -> &'a U,
    {
        let self_ = ManuallyDrop::new(self);
        let pointer = f(&**self_);

        ProjectArc {
            inner: self_.inner,
            pointer: pointer.into(),
        }
    }

    pub fn clone_project<F, U>(&self, f: F) -> ProjectArc<U>
    where
        F: for<'a> FnOnce(&'a T) -> &'a U,
    {
        self.clone().project(f)
    }
}

unsafe impl<T> Send for ProjectArc<T> where T: Send + Sync + ?Sized {}

unsafe impl<T> Sync for ProjectArc<T> where T: Send + Sync + ?Sized {}

impl<T: ?Sized> Deref for ProjectArc<T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { self.pointer.as_ref() }
    }
}

impl<T: ?Sized> Clone for ProjectArc<T> {
    fn clone(&self) -> Self {
        self.inner().strong.fetch_add(1, Ordering::Release);

        ProjectArc {
            inner: self.inner,
            pointer: self.pointer,
        }
    }
}

impl<T: ?Sized> Drop for ProjectArc<T> {
    fn drop(&mut self) {
        let count = self.inner().strong.fetch_sub(1, Ordering::AcqRel);

        if count == 1 {
            unsafe {
                deallocate(self.inner);
            }
        }
    }
}

common_impls!(ProjectArc);

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
struct ArcInner {
    strong: AtomicUsize,
    drop: TypeMetadata,
    // payload: [u8],
}

unsafe fn deallocate(inner: NonNull<ArcInner>) {
    let meta = unsafe { (*inner.as_ptr()).drop };
    let layout = arc_inner_layout(meta);

    let inner_ptr = inner.as_ptr() as *mut u8;
    let payload = inner_ptr.add(layout.payload_offset) as *mut ();

    unsafe {
        drop_in_place(payload, meta);
        std::alloc::dealloc(inner_ptr, layout.layout);
    }
}

struct ArcInnerLayout {
    layout: Layout,
    strong_offset: usize,
    drop_offset: usize,
    payload_offset: usize,
}

fn arc_inner_layout(meta: TypeMetadata) -> ArcInnerLayout {
    let (layout, strong_offset) = (Layout::new::<AtomicUsize>(), 0);
    let (layout, drop_offset) = layout.extend(Layout::new::<TypeMetadata>()).unwrap();
    let (layout, payload_offset) = layout
        .extend(Layout::from_size_align(meta.size_of(), meta.align_of()).unwrap())
        .unwrap();
    let layout = layout.pad_to_align();

    ArcInnerLayout {
        layout,
        strong_offset,
        drop_offset,
        payload_offset,
    }
}

#[cfg(test)]
mod test {
    use std::sync::atomic::AtomicBool;

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
        let dropped = &AtomicBool::new(false);

        let p1 = ProjectArc::new(SideEffect(12345, |_| {
            dropped.store(true, Ordering::SeqCst);
        }));
        assert!(!dropped.load(Ordering::SeqCst));
        assert_eq!((*p1).0, 12345);

        let p2 = p1.clone();
        assert_eq!((*p1).0, 12345);
        assert!(!dropped.load(Ordering::SeqCst));

        drop(p1);
        assert_eq!((*p2).0, 12345);
        assert!(!dropped.load(Ordering::SeqCst));

        drop(p2);
        // YES dropped
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[test]
    fn projected_ref_counting() {
        let dropped = &AtomicBool::new(false);

        let p1 = ProjectArc::new(SideEffect(12345, |_| {
            dropped.store(true, Ordering::SeqCst);
        }));
        assert!(!dropped.load(Ordering::SeqCst));

        let p2 = p1.project(|self_| &self_.0);
        assert_eq!(*p2, 12345);
        assert!(!dropped.load(Ordering::SeqCst));

        drop(p2);
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[test]
    fn project_slice() {
        let p1: ProjectArc<[i32]> = ProjectArc::new([1, 2, 3]);

        let p2 = p1.clone().project(|self_| &self_[0]);
        let p3 = p1.project(|self_| &self_[1..]);

        assert_eq!(*p2, 1);
        assert_eq!(*p3, [2, 3]);
    }

    #[test]
    fn project_deref() {
        let p1: ProjectArc<Vec<i32>> = ProjectArc::new(vec![1, 2, 3]);
        let p2: ProjectArc<[i32]> = p1.project_deref();

        assert_eq!(*p2, [1, 2, 3]);
    }

    #[test]
    fn subslices() {
        let p1: ProjectArc<&str> = ProjectArc::new(&"Hello, world!");
        let p1 = p1.project(|self_| &self_[0..4]);

        assert_eq!(&*p1, "Hell");
    }
}
