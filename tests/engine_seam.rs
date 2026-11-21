//! `Engine::store()` is visible only inside `src/engine`, so the compiler
//! already stops other production code from reaching the Qdrant store. The one
//! public escape hatch is `Engine::store_for_tests`; this test keeps production
//! code from using it.
use std::path::Path;

fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn production_code_does_not_use_store_for_tests() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    assert!(files.len() > 20, "found {} source files", files.len());
    let offenders: Vec<_> = files
        .iter()
        .filter(|path| !path.ends_with("src/engine/mod.rs"))
        .filter(|path| {
            std::fs::read_to_string(path)
                .unwrap()
                .contains("store_for_tests")
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "Engine::store_for_tests is for tests; use an Engine method instead: {offenders:?}"
    );
}
