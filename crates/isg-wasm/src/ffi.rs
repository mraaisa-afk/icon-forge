//! The module's exported symbols.
//!
//! This is the only place in the crate where the `unsafe_code` lint is relaxed,
//! and only because a symbol with a fixed name *is* an ABI contract: declaring
//! `#[no_mangle] pub extern "C"` tells the linker to publish a name the host
//! looks up, which rustc treats as an unsafe operation. The bodies below are
//! ordinary safe Rust — they forward to [`crate::abi`] and contain no `unsafe`
//! block, no pointer arithmetic and no FFI slice.

#![allow(unsafe_code)]

/// Runs one editor feature. See [`crate::abi::feature`] for the feature table.
///
/// Returns the feature's value, or `0` when it failed; the reason is then
/// readable with `feature::ERROR`.
#[no_mangle]
pub extern "C" fn editor_call(feature: u32, a: u32, b: u32) -> u32 {
    crate::abi::call(feature, a, b)
}

/// The ABI level, for the adapter's compatibility check.
#[no_mangle]
pub extern "C" fn editor_abi_version() -> u32 {
    crate::abi::ABI_VERSION
}

/// Address of the input table (in words) inside the module's memory.
#[no_mangle]
pub extern "C" fn editor_in_ptr() -> u32 {
    crate::abi::input_ptr()
}

/// Capacity of the input table in words.
#[no_mangle]
pub extern "C" fn editor_in_cap() -> u32 {
    crate::abi::input_capacity()
}

/// Address of the output table (in words) inside the module's memory.
#[no_mangle]
pub extern "C" fn editor_out_ptr() -> u32 {
    crate::abi::output_ptr()
}

/// Capacity of the output table in words.
#[no_mangle]
pub extern "C" fn editor_out_cap() -> u32 {
    crate::abi::output_capacity()
}
