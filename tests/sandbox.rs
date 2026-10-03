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
            writable: false,
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

/// A folder that allows changes is writable under the sandbox, through
/// cww's changes and its system trash; a read-only folder, the rest of the
/// home folder, and the rest of the data directory stay closed to writes,
/// even to code that skips cww's own checks. Linux only: on macOS the trash
/// is the real `~/.Trash`, which a test shouldn't fill.
#[cfg(target_os = "linux")]
#[test]
fn only_folders_that_allow_changes_are_writable() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let docs = base.join("docs");
    let ro = base.join("ro");
    let private = base.join("private");
    let xdg = base.join("xdg");
    for dir in [&docs, &ro, &private, &xdg] {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(docs.join("notes.md"), "v1\n").unwrap();
    std::fs::write(docs.join("old.md"), "old\n").unwrap();
    std::fs::write(ro.join("keep.md"), "keep\n").unwrap();

    let home = base.join("cww");
    let paths = Paths::under(&home);
    let root = |id: &str, path: &std::path::Path, writable| Root {
        id: id.into(),
        label: id.into(),
        path: path.to_path_buf(),
        follow_symlinks: false,
        writable,
    };
    Config {
        roots: vec![root("docs", &docs, true), root("ro", &ro, false)],
        secret_store: Some("file".into()),
        ..Config::default()
    }
    .save(&paths)
    .unwrap();
    // The trash is $XDG_DATA_HOME/Trash, so the test never touches the
    // real one.
    let run = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_cww"))
            .env("CWW_HOME", &home)
            .env("XDG_DATA_HOME", &xdg)
            .args(args)
            .output()
            .unwrap();
        let out = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(
            output.status.success(),
            "{out}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        out
    };
    let probe = |dir: &std::path::Path| dir.join("probe.txt").display().to_string();
    let out = run(&[
        "debug",
        "sandbox",
        "--write",
        &probe(&docs),
        "--write",
        &probe(&ro),
        "--write",
        &probe(&private),
        "--write",
        &probe(&xdg),
    ]);
    if !out.contains(r#""state":"enforced""#) {
        eprintln!("sandbox not enforced here: {out}");
        return;
    }
    assert!(out.contains(&format!("wrote {}", probe(&docs))), "{out}");
    for closed in [&ro, &private, &xdg] {
        assert!(
            out.contains(&format!("refused writing {}", probe(closed))),
            "{out}"
        );
    }

    // cww's own changes work under the sandbox, the trash included.
    let out = run(&[
        "debug",
        "change",
        "write",
        r#"{"path":"docs:notes.md","content":"v2\n"}"#,
    ]);
    assert!(
        out.contains(r#"changed {"path":"docs:notes.md","effect":"replaced""#),
        "{out}"
    );
    assert_eq!(
        std::fs::read_to_string(docs.join("notes.md")).unwrap(),
        "v2\n"
    );
    let out = run(&["debug", "change", "delete", r#"{"path":"docs:old.md"}"#]);
    assert!(out.contains(r#""effect":"trashed""#), "{out}");
    let trash = xdg.join("Trash");
    assert_eq!(
        std::fs::read_to_string(trash.join("files/notes.md")).unwrap(),
        "v1\n"
    );
    assert_eq!(
        std::fs::read_to_string(trash.join("files/old.md")).unwrap(),
        "old\n"
    );
    assert!(trash.join("info/old.md.trashinfo").exists());
    let out = run(&[
        "debug",
        "change",
        "move",
        r#"{"from":"docs:notes.md","to":"docs:moved.md"}"#,
    ]);
    assert!(out.contains(r#""effect":"moved""#), "{out}");
    let out = run(&[
        "debug",
        "change",
        "create",
        r#"{"path":"ro:new.md","content":"x"}"#,
    ]);
    assert!(out.contains("refused not_writable"), "{out}");
    assert!(!ro.join("new.md").exists());
}
