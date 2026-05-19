//! Compile-fail tests for `#[derive(clapfig::Schema)]`.
//!
//! Each `tests/ui/derive/*.rs` file is a complete program the macro must
//! reject. `*.stderr` captures the expected diagnostic (regenerate with
//! `TRYBUILD=overwrite cargo test --test ui_derive_schema`).
//!
//! **Why these tests matter:** the macro is the layer most likely to ship
//! latent breakage — a typo in a derive attribute, an unrecognized field
//! type, a malformed default expression. Behavioral tests can only catch
//! cases that compile; these lock down the diagnostics on cases that
//! must not.

#![cfg(feature = "derive")]

#[test]
fn ui_compile_fail_cases() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/derive/*.rs");
}
