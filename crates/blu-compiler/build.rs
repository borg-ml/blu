#![forbid(unsafe_code)]

#[cfg(not(feature = "legacy-luau"))]
fn main() {}

#[cfg(feature = "legacy-luau")]
fn main() {
    let mut shim = cc::Build::new();
    shim.cpp(true).std("c++17").file("native/compiler_shim.cpp");
    if std::env::var("TARGET").is_ok_and(|target| target.ends_with("emscripten")) {
        shim.flag_if_supported("-fexceptions");
        shim.flag_if_supported("-fwasm-exceptions");
    }
    shim.compile("blu_compiler_shim");

    println!("cargo:rerun-if-changed=native/compiler_shim.cpp");
    luau0_src::Build::new().build().print_cargo_metadata();
}
