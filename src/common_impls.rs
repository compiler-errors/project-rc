#[macro_export]
macro_rules! common_impls {
    ($ty:ident) => {
        impl<T: Deref + ?Sized> $ty<T> {
            pub fn project_deref(self) -> $ty<T::Target> {
                self.project(T::deref)
            }
        }
    };
}
