use core::{
    alloc::Layout,
    cell::Cell,
    marker::Unsize,
    mem::ManuallyDrop,
    ops::{CoerceUnsized, Deref},
    ptr::{DynMetadata, NonNull},
};
use std::alloc::{handle_alloc_error, Allocator, Global};

pub struct ProjectRc<T: ?Sized> {
    inner: NonNull<ArcInner>,
    pointer: NonNull<T>,
}

impl<T> ProjectRc<T> {
    pub fn new(thing: T) -> Self {
        let layout = arc_inner_layout(core::mem::size_of::<T>(), core::mem::align_of::<T>());
        let meta = drop_meta::<T>();

        let ptr = match Global.allocate(layout.layout) {
            Ok(ptr) => ptr.as_mut_ptr(),
            Err(_) => handle_alloc_error(layout.layout),
        };

        unsafe {
            ptr.add(layout.strong_offset)
                .cast::<Cell<usize>>()
                .write(Cell::new(1));
            ptr.add(layout.drop_offset)
                .cast::<DynMetadata<_>>()
                .write(meta);
            ptr.add(layout.payload_offset).cast::<T>().write(thing);

            let inner_ptr = NonNull::new_unchecked(ptr as *mut ArcInner);
            let payload_ptr = NonNull::new_unchecked(ptr.add(layout.payload_offset) as *mut T);

            ProjectRc {
                inner: inner_ptr,
                pointer: payload_ptr,
            }
        }
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

    pub fn project_clone<F, U>(&self, f: F) -> ProjectRc<U>
    where
        F: for<'a> FnOnce(&'a T) -> &'a U,
    {
        self.clone().project(f)
    }

    fn inner(&self) -> &ArcInner {
        unsafe { self.inner.as_ref() }
    }
}

impl<T: Deref + ?Sized> ProjectRc<T> {
    pub fn project_deref(self) -> ProjectRc<T::Target> {
        self.project(T::deref)
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

impl<T, U> CoerceUnsized<ProjectRc<U>> for ProjectRc<T>
where
    T: Unsize<U> + ?Sized,
    U: ?Sized,
{
}

#[repr(C)]
struct ArcInner {
    strong: Cell<usize>,
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
    let (layout, strong_offset) = (Layout::new::<Cell<usize>>(), 0);
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
