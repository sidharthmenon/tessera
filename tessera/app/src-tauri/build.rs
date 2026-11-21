use std::path::PathBuf;

fn main() {
    // tauri-build refuses to build without the sidecar, which scripts/build-engine.sh makes.
    // On a fresh checkout put a stub in its place that says so, so `cargo build --workspace`
    // works; the stub exits 1 and the controller's watchdog logs its message.
    let target = std::env::var("TARGET").expect("cargo sets TARGET");
    let exe = if target.contains("windows") { ".exe" } else { "" };
    let sidecar = PathBuf::from(format!("binaries/tessera-render-{target}{exe}"));
    if !sidecar.exists() {
        std::fs::create_dir_all("binaries").expect("create binaries/");
        let stub = "#!/bin/sh\necho 'tessera-render is a placeholder: run scripts/build-engine.sh' >&2\nexit 1\n";
        std::fs::write(&sidecar, stub).expect("write the sidecar placeholder");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&sidecar, std::fs::Permissions::from_mode(0o755)).expect("chmod the placeholder");
        }
    }
    tauri_build::build()
}
