use std::path::{Path, PathBuf};

use edgebox_runtime::Runtime;

fn hello_wasm() -> PathBuf {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/wasm32-wasip2/release/hello.wasm");
    assert!(
        path.exists(),
        "{} not found: run `cargo guests` first",
        path.display()
    );
    path
}

#[test]
fn runs_hello_component() {
    let (code, stdout) = Runtime::new()
        .unwrap()
        .run_captured(&hello_wasm(), &[])
        .unwrap();

    assert_eq!(code, 0);
    assert_eq!(String::from_utf8(stdout).unwrap(), "Hello from WASM!\n");
}

#[test]
fn passes_args_to_guest() {
    let args = ["a".to_string(), "b".to_string()];
    let (_, stdout) = Runtime::new()
        .unwrap()
        .run_captured(&hello_wasm(), &args)
        .unwrap();

    assert!(
        String::from_utf8(stdout)
            .unwrap()
            .contains(r#"args: ["a", "b"]"#)
    );
}

#[test]
fn missing_file_is_a_clear_error() {
    let err = Runtime::new()
        .unwrap()
        .run(Path::new("does-not-exist.wasm"), &[])
        .unwrap_err();

    assert!(err.to_string().contains("failed to load component"));
}
