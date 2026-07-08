fn main() {
    if std::env::var("PROFILE").as_deref() == Ok("release") {
        let manifest_dir = std::path::PathBuf::from(
            std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by Cargo"),
        );
        let frontend_index = manifest_dir.join("../dist/index.html");
        if !frontend_index.exists() {
            panic!(
                "missing frontend build at {}. Run `pnpm build` before `cargo build --release`.",
                frontend_index.display()
            );
        }
    }

    tauri_build::build()
}
