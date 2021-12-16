pub(crate) trait Droppable {}

impl<T> Droppable for T {}

#[repr(C)]
struct VTable {
    drop_in_place: unsafe fn(*mut ()),
    size_of: usize,
    align_of: usize,
}

#[derive(Copy, Clone)]
pub(crate) struct TypeMetadata {
    vtable: &'static VTable,
}

impl TypeMetadata {
    pub(crate) fn size_of(&self) -> usize {
        self.vtable.size_of
    }

    pub(crate) fn align_of(&self) -> usize {
        self.vtable.align_of
    }
}

pub(crate) fn metadata_of<T>() -> TypeMetadata {
    let ptr = std::ptr::null::<T>() as *const dyn Droppable;
    let fat = unsafe { std::mem::transmute::<_, [usize; 2]>(ptr) };
    let vtable = unsafe { &*(fat[1] as *const VTable) };

    TypeMetadata { vtable }
}

pub(crate) unsafe fn drop_in_place(ptr: *mut (), meta: TypeMetadata) {
    // SAFETY:
    // 1. ptr is non-null
    // 2. TypeMetadata is the vtable corresponding to ptr's `Droppable` impl
    // 3. ptr will not be accessed after this
    (meta.vtable.drop_in_place)(ptr)
}
