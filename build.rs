fn main() {
    // Match the target, not the build machine: Linux compile checks can run
    // on Windows without loading or building the WinFsp SDK for a build script.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
        let arch = match arch.as_str() {
            "x86_64" => "x64",
            "aarch64" => "a64",
            _ => panic!("Unsupported Windows architecture"),
        };
        println!("cargo:rustc-link-arg=/DELAYLOAD:winfsp-{arch}.dll");
        println!("cargo:rustc-link-arg=/DEFAULTLIB:delayimp");
    }
}
