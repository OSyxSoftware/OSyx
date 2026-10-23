fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    println!("cargo:rustc-link-arg-bins=-T{dir}/../../userspace/user.ld");
    println!("cargo:rustc-link-arg-bins=--no-dynamic-linker");
    println!("cargo:rerun-if-changed=../../userspace/user.ld");
}
