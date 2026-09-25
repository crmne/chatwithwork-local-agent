//! The kernel sandbox (Landlock on Linux, Seatbelt on macOS) lets the daemon
//! read its shared folders and nothing else in the home folder, whatever the
//! daemon's own code does. `cww debug sandbox` confines itself exactly as the
//! daemon does, then tries to read files.

#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::process::Command;

use cww::config::{Config, Root};
use cww::paths::Paths;

#[test]
fn only_shared_folders_are_readable() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let shared = base.join("shared");
    let private = base.join("private");
    std::fs::create_dir_all(&shared).unwrap();
    std::fs::create_dir_all(&private).unwrap();
    std::fs::write(shared.join("notes.md"), "shared").unwrap();
    std::fs::write(private.join("id_ed25519"), "secret").unwrap();

    let home = base.join("cww");
    let paths = Paths::under(&home);
    Config {
        roots: vec![Root {
            id: "shared".into(),
            label: "Shared".into(),
            path: shared.clone(),
            follow_symlinks: false,
        }],
        secret_store: Some("file".into()),
        ..Config::default()
    }
    .save(&paths)
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_cww"))
        .env("CWW_HOME", &home)
        .args(["debug", "sandbox"])
        .arg(shared.join("notes.md"))
        .arg(private.join("id_ed25519"))
        .arg(paths.config_file())
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{out}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    if !out.contains(r#""state":"enforced""#) {
        // Landlock is off in this kernel (some CI runners); nothing to check.
        eprintln!("sandbox not enforced here: {out}");
        return;
    }
    assert!(
        out.contains(&format!("read {}", shared.join("notes.md").display())),
        "{out}"
    );
    assert!(
        out.contains(&format!("refused {}", private.join("id_ed25519").display())),
        "{out}"
    );
    assert!(
        out.contains(&format!("read {}", paths.config_file().display())),
        "{out}"
    );
}
