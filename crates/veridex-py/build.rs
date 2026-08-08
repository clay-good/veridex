//! On macOS, a Python extension module links with undefined Python symbols that the interpreter
//! resolves at load time. maturin sets this automatically; when building with plain cargo we add it
//! here. Scoped to this crate's cdylib only, so the rest of the workspace is unaffected.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-cdylib-link-arg=-undefined");
        println!("cargo:rustc-cdylib-link-arg=dynamic_lookup");
    }
}
