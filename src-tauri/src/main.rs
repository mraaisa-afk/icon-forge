//! Icon Forge — Tauri 2 entry point.
//!
//! The Windows subsystem attribute keeps the release build console-free;
//! it is skipped on other targets and in tests so `cargo test` output and
//! Linux CI builds behave normally.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    icon_forge_lib::run();
}
