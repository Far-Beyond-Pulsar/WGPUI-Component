pub trait Sealed {}

impl Sealed for f64 {}

#[cfg(all(feature = "decimal", not(target_family = "wasm")))]
impl Sealed for rust_decimal::Decimal {}
