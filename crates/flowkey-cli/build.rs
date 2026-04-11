fn main() {
    println!("cargo:rerun-if-changed=resources/app.manifest");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let manifest_dir =
            std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR should be set");
        let manifest = std::path::Path::new(&manifest_dir)
            .join("resources")
            .join("app.manifest");

        println!("cargo:rustc-link-arg-bin=flky=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bin=flky=/MANIFESTUAC:NO");
        println!(
            "cargo:rustc-link-arg-bin=flky=/MANIFESTINPUT:{}",
            manifest.display()
        );
    }
}
