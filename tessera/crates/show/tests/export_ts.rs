use tessera_show::*;
use ts_rs::{Config, TS};

#[test]
fn export_ts() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../app/src/lib/bindings");
    let cfg = Config::default().with_out_dir(&dir);
    Show::export_all(&cfg).unwrap();
    ShowOp::export_all(&cfg).unwrap();
    ToEngine::export_all(&cfg).unwrap();
    FromEngine::export_all(&cfg).unwrap();
    assert!(dir.join("Show.ts").exists());
    assert!(dir.join("ToEngine.ts").exists());
}
