// Embedded UniFFI bindgen CLI. Pinning it to the crate's uniffi version avoids
// runtime/bindgen contract mismatches. Run via: cargo run --features cli --bin uniffi-bindgen -- ...
fn main() {
    uniffi::uniffi_bindgen_main()
}
