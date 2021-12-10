use core::{
    alloc::Layout,
    marker::Unsize,
    mem::ManuallyDrop,
    ops::{CoerceUnsized, Deref},
    ptr::{DynMetadata, NonNull},
    sync::atomic::{AtomicUsize, Ordering},
};
use std::alloc::{handle_alloc_error, Allocator, Global};

pub struct ProjectArc<T: ?Sized> {
    inner: NonNull<ArcInner>,
    pointer: NonNull<T>,
}

impl<T> ProjectArc<T> {
    pub fn new(thing: T) -> Self {
        let layout = arc_inner_layout(core::mem::size_of::<T>(), core::mem::align_of::<T>());
        let meta = drop_meta::<T>();

        let ptr = match Global.allocate(layout.layout) {
            Ok(ptr) => ptr.as_mut_ptr(),
            Err(_) => handle_alloc_error(layout.layout),
        };

        unsafe {
            ptr.add(layout.strong_offset)
                .cast::<AtomicUsize>()
                .write(AtomicUsize::new(1));
            ptr.add(layout.drop_offset)
                .cast::<DynMetadata<_>>()
                .write(meta);
            ptr.add(layout.payload_offset).cast::<T>().write(thing);

            let inner_ptr = NonNull::new_unchecked(ptr as *mut ArcInner);
            let payload_ptr = NonNull::new_unchecked(ptr.add(layout.payload_offset) as *mut T);

            ProjectArc {
                inner: inner_ptr,
                pointer: payload_ptr,
            }
        }
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

    pub fn project_clone<F, U>(&self, f: F) -> ProjectArc<U>
    where
        F: for<'a> FnOnce(&'a T) -> &'a U,
    {
        self.clone().project(f)
    }

    fn inner(&self) -> &ArcInner {
        unsafe { self.inner.as_ref() }
    }
}

unsafe impl<T> Send for ProjectArc<T> where T: Send + Sync + ?Sized {}

unsafe impl<T> Sync for ProjectArc<T> where T: Send + Sync + ?Sized {}

impl<T: Deref + ?Sized> ProjectArc<T> {
    pub fn project_deref(self) -> ProjectArc<T::Target> {
        self.project(T::deref)
    }
}

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

impl<T, U> CoerceUnsized<ProjectArc<U>> for ProjectArc<T>
where
    T: Unsize<U> + ?Sized,
    U: ?Sized,
{
}

#[repr(C)]
struct ArcInner {
    strong: AtomicUsize,
    drop: DynMetadata<dyn Droppable>,
    // payload: [u8],
}

unsafe fn deallocate(inner: NonNull<ArcInner>) {
    let meta = unsafe { (*inner.as_ptr()).drop };
    let layout = arc_inner_layout(meta.size_of(), meta.align_of());

    unsafe {
        let payload = (inner.as_ptr() as *mut u8).add(layout.payload_offset) as *mut ();
        let payload: *mut dyn Droppable = core::ptr::from_raw_parts_mut(payload, meta);
        core::ptr::drop_in_place(payload);

        Global.deallocate(inner.cast::<u8>(), layout.layout);
    }
}

struct ArcInnerLayout {
    layout: Layout,
    strong_offset: usize,
    drop_offset: usize,
    payload_offset: usize,
}

fn arc_inner_layout(size: usize, align: usize) -> ArcInnerLayout {
    let (layout, strong_offset) = (Layout::new::<AtomicUsize>(), 0);
    let (layout, drop_offset) = layout
        .extend(Layout::new::<DynMetadata<dyn Droppable>>())
        .unwrap();
    let (layout, payload_offset) = layout
        .extend(Layout::from_size_align(size, align).unwrap())
        .unwrap();
    let layout = layout.pad_to_align();

    ArcInnerLayout {
        layout,
        strong_offset,
        drop_offset,
        payload_offset,
    }
}

trait Droppable {}

impl<T> Droppable for T {}

fn drop_meta<'a, T: 'a>() -> DynMetadata<dyn Droppable + 'a> {
    (core::ptr::null::<T>() as *const dyn Droppable)
        .to_raw_parts()
        .1
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
