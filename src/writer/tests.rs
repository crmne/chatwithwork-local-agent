//! The writer's rules, one refusal at a time, on real files.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::*;
use crate::config::{Config, Limits, Root};
use crate::paths::Paths;
use crate::status::Changes;

struct Fixture {
    _tmp: tempfile::TempDir,
    base: PathBuf,
    docs: PathBuf,
    outside: PathBuf,
    trash: PathBuf,
    writer: Writer,
}

fn fixture() -> Fixture {
    fixture_with(Limits::default())
}

fn fixture_with(limits: Limits) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let docs = base.join("docs");
    let other = base.join("other");
    let ro = base.join("ro");
    let outside = base.join("outside");
    for dir in [&docs, &docs.join("sub"), &other, &ro, &outside] {
        fs::create_dir_all(dir).unwrap();
    }
    fs::write(docs.join("notes.md"), "# Notes\nOne\nTwo\n").unwrap();
    fs::write(docs.join(".env"), "TOKEN=1\n").unwrap();
    fs::write(ro.join("keep.md"), "keep").unwrap();
    fs::write(outside.join("secret.txt"), "outside").unwrap();
    symlink(outside.join("secret.txt"), docs.join("escape.txt")).unwrap();
    symlink(&outside, docs.join("linked")).unwrap();
    fs::hard_link(outside.join("secret.txt"), docs.join("hardlink.txt")).unwrap();
    let paths = Paths::under(&base.join("cww"));
    let root = |id: &str, path: &Path, writable| Root {
        id: id.into(),
        label: id.to_uppercase(),
        path: path.to_path_buf(),
        follow_symlinks: false,
        writable,
    };
    let mut config = Config {
        roots: vec![
            root("docs", &docs, true),
            root("other", &other, true),
            root("ro", &ro, false),
        ],
        limits,
        ..Config::default()
    };
    config.index.enabled = false;
    let reader = Arc::new(Reader::new(&config, &paths, Changes::default()).unwrap());
    let limiter = Arc::new(Limiter::new(config.limits.clone()));
    let trash = paths.home_trash.clone().unwrap();
    let writer = Writer::new(reader, limiter, Trash::new(Some(trash.clone())));
    Fixture {
        _tmp: tmp,
        base,
        docs,
        outside,
        trash,
        writer,
    }
}

fn code(result: Result<ChangeResult, ToolError>) -> ErrorCode {
    match result {
        Ok(r) => panic!("expected a refusal, got {r:?}"),
        Err(e) => {
            assert!(!e.message.is_empty());
            e.code
        }
    }
}

fn create(f: &Fixture, path: &str, content: &str) -> Result<ChangeResult, ToolError> {
    f.writer.create(&CreateRequest {
        path: path.into(),
        content: content.into(),
        dry_run: false,
    })
}

fn write(
    f: &Fixture,
    path: &str,
    content: &str,
    mode: WriteMode,
) -> Result<ChangeResult, ToolError> {
    f.writer.write(&WriteRequest {
        path: path.into(),
        content: content.into(),
        mode,
        ..WriteRequest::default()
    })
}

/// Where trashed items land: `files/` in a freedesktop trash, the trash
/// itself on macOS.
fn trash_files(trash: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        trash.to_path_buf()
    } else {
        trash.join("files")
    }
}

fn trashed(f: &Fixture) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(trash_files(&f.trash))
        .map(|d| {
            d.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[test]
fn creates_files_with_the_usual_mode() {
    let f = fixture();
    let made = create(&f, "docs:sub/plan.md", "# Plan\n").unwrap();
    assert_eq!(made.effect, Effect::Created);
    assert_eq!(made.path, "docs:sub/plan.md");
    assert_eq!(made.size, Some(7));
    assert_eq!(
        fs::read_to_string(f.docs.join("sub/plan.md")).unwrap(),
        "# Plan\n"
    );
    let mode = fs::metadata(f.docs.join("sub/plan.md"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o111, 0, "never executable");
    // No temporary file is left behind.
    let names: Vec<_> = fs::read_dir(f.docs.join("sub")).unwrap().collect();
    assert_eq!(names.len(), 1);

    assert_eq!(
        code(create(&f, "docs:sub/plan.md", "again")),
        ErrorCode::Exists
    );
    assert_eq!(
        code(create(&f, "docs:missing/plan.md", "x")),
        ErrorCode::NotFound
    );
}

#[test]
fn dry_runs_change_nothing() {
    let f = fixture();
    let checked = f
        .writer
        .create(&CreateRequest {
            path: "docs:new.md".into(),
            content: "x".into(),
            dry_run: true,
        })
        .unwrap();
    assert!(checked.dry_run);
    assert!(!f.docs.join("new.md").exists());
    let diff = f
        .writer
        .write(&WriteRequest {
            path: "docs:notes.md".into(),
            content: "# Notes\nOne\n2\n".into(),
            dry_run: true,
            ..WriteRequest::default()
        })
        .unwrap();
    let text = diff.diff.unwrap();
    assert!(text.contains("-Two\n+2\n"), "{text}");
    assert_eq!(
        fs::read_to_string(f.docs.join("notes.md")).unwrap(),
        "# Notes\nOne\nTwo\n"
    );
    assert!(trashed(&f).is_empty());
}

#[test]
fn refuses_paths_outside_writable_folders() {
    let f = fixture();
    for (path, expected) in [
        ("ro:new.md", ErrorCode::NotWritable),
        ("ro:keep.md", ErrorCode::NotWritable),
        ("nowhere:new.md", ErrorCode::UnknownRoot),
        ("docs:../outside/new.md", ErrorCode::InvalidPath),
        ("/etc/new.md", ErrorCode::InvalidPath),
        ("docs:/etc/new.md", ErrorCode::InvalidPath),
        ("docs:", ErrorCode::InvalidPath),
        ("docs:linked/new.md", ErrorCode::Denied),
        ("docs:escape.txt/new.md", ErrorCode::Denied),
    ] {
        assert_eq!(code(create(&f, path, "x")), expected, "{path}");
    }
    assert!(!f.outside.join("new.md").exists());
    assert_eq!(fs::read_dir(&f.outside).unwrap().count(), 1);
}

#[test]
fn refuses_denied_and_protected_paths() {
    let f = fixture();
    fs::create_dir_all(f.docs.join(".git")).unwrap();
    for path in [
        "docs:.env",
        "docs:.env.local",
        "docs:keys.pem",
        "docs:.ssh/config",
        "docs:.git/config",
        "docs:.vscode/tasks.json",
        "docs:.bashrc",
        // Names a case-insensitive filesystem folds to the ones above.
        "docs:.z\u{017F}hrc",
        "docs:.\u{212A}ube/config",
        "docs:.ENV",
    ] {
        assert_eq!(code(create(&f, path, "x")), ErrorCode::Denied, "{path}");
    }
    assert_eq!(
        code(write(&f, "docs:.env", "TOKEN=2", WriteMode::Replace)),
        ErrorCode::Denied
    );
    assert_eq!(
        fs::read_to_string(f.docs.join(".env")).unwrap(),
        "TOKEN=1\n"
    );
}

#[test]
fn refuses_programs_and_binary_files() {
    let f = fixture();
    for path in [
        "docs:setup.exe",
        "docs:run.bat",
        "docs:x.desktop",
        "docs:a.docm",
    ] {
        assert_eq!(
            code(create(&f, path, "x")),
            ErrorCode::NotChangeable,
            "{path}"
        );
    }
    for path in ["docs:report.pdf", "docs:sheet.xlsx"] {
        assert_eq!(
            code(create(&f, path, "x")),
            ErrorCode::NotChangeable,
            "{path}"
        );
    }
    assert_eq!(
        code(create(&f, "docs:a.txt", "a\0b")),
        ErrorCode::NotChangeable
    );
    assert_eq!(code(create(&f, "docs:x.exe.", "x")), ErrorCode::InvalidPath);
    assert_eq!(
        code(create(&f, "docs:CON.txt", "x")),
        ErrorCode::InvalidPath
    );

    // An executable file is never changed.
    fs::write(f.docs.join("tool.sh"), "echo hi\n").unwrap();
    fs::set_permissions(f.docs.join("tool.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        code(write(&f, "docs:tool.sh", "rm -rf ~\n", WriteMode::Replace)),
        ErrorCode::NotChangeable
    );
    // Nor a file that isn't text.
    fs::write(f.docs.join("blob.dat"), [0u8, 159, 146, 150]).unwrap();
    assert_eq!(
        code(write(&f, "docs:blob.dat", "text", WriteMode::Replace)),
        ErrorCode::NotChangeable
    );
    // Nor one marked read-only.
    fs::write(f.docs.join("frozen.md"), "frozen").unwrap();
    fs::set_permissions(f.docs.join("frozen.md"), fs::Permissions::from_mode(0o444)).unwrap();
    assert_eq!(
        code(write(&f, "docs:frozen.md", "thawed", WriteMode::Replace)),
        ErrorCode::Denied
    );
}

#[test]
fn refuses_links_hard_links_and_special_files() {
    let f = fixture();
    for path in ["docs:escape.txt", "docs:hardlink.txt"] {
        assert_eq!(
            code(write(&f, path, "gotcha", WriteMode::Replace)),
            ErrorCode::Denied,
            "{path}"
        );
    }
    assert_eq!(
        fs::read_to_string(f.outside.join("secret.txt")).unwrap(),
        "outside"
    );
    let status = std::process::Command::new("mkfifo")
        .arg(f.docs.join("pipe"))
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(
        code(write(&f, "docs:pipe", "x", WriteMode::Replace)),
        ErrorCode::Denied
    );
    assert_eq!(
        code(write(&f, "docs:sub", "x", WriteMode::Replace)),
        ErrorCode::NotAFile
    );
    for path in ["docs:escape.txt", "docs:linked", "docs:pipe"] {
        let refused = f.writer.delete(&DeleteRequest {
            path: path.into(),
            dry_run: false,
        });
        assert_eq!(code(refused), ErrorCode::Denied, "{path}");
    }
    assert!(f.outside.join("secret.txt").exists());
}

#[test]
fn replaces_and_appends_keeping_the_old_version_in_the_trash() {
    let f = fixture();
    fs::set_permissions(f.docs.join("notes.md"), fs::Permissions::from_mode(0o640)).unwrap();
    let replaced = write(&f, "docs:notes.md", "new\n", WriteMode::Replace).unwrap();
    assert_eq!(replaced.effect, Effect::Replaced);
    assert!(replaced.previous.as_ref().unwrap().in_trash);
    assert_eq!(
        fs::read_to_string(f.docs.join("notes.md")).unwrap(),
        "new\n"
    );
    let mode = fs::metadata(f.docs.join("notes.md"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o640, "the mode is kept");
    let old = replaced.trashed_to.unwrap();
    assert_eq!(fs::read_to_string(&old).unwrap(), "# Notes\nOne\nTwo\n");
    assert!(old.starts_with(&f.trash));

    let appended = write(&f, "docs:notes.md", "more\n", WriteMode::Append).unwrap();
    assert_eq!(appended.effect, Effect::Appended);
    assert_eq!(
        fs::read_to_string(f.docs.join("notes.md")).unwrap(),
        "new\nmore\n"
    );
    let second = if cfg!(target_os = "macos") {
        "notes 2.md"
    } else {
        "notes.2.md"
    };
    assert_eq!(trashed(&f), [second, "notes.md"]);

    // Writing a file that isn't there creates it.
    let made = write(&f, "docs:fresh.md", "fresh", WriteMode::Append).unwrap();
    assert_eq!(made.effect, Effect::Created);
}

#[test]
fn expected_sha_guards_against_changes_since_a_dry_run() {
    let f = fixture();
    let checked = f
        .writer
        .write(&WriteRequest {
            path: "docs:notes.md".into(),
            content: "v2".into(),
            dry_run: true,
            ..WriteRequest::default()
        })
        .unwrap();
    let sha = checked.previous.unwrap().sha256.unwrap();
    fs::write(f.docs.join("notes.md"), "changed meanwhile").unwrap();
    let refused = f.writer.write(&WriteRequest {
        path: "docs:notes.md".into(),
        content: "v2".into(),
        expected_sha256: Some(sha),
        ..WriteRequest::default()
    });
    assert_eq!(code(refused), ErrorCode::Conflict);
    assert_eq!(
        fs::read_to_string(f.docs.join("notes.md")).unwrap(),
        "changed meanwhile"
    );
}

#[test]
fn edits_one_exact_span() {
    let f = fixture();
    let edit = |old: &str, new: &str| {
        f.writer.edit(&EditRequest {
            path: "docs:notes.md".into(),
            old_text: old.into(),
            new_text: new.into(),
            ..EditRequest::default()
        })
    };
    assert_eq!(code(edit("Three", "3")), ErrorCode::InvalidArgument);
    fs::write(f.docs.join("notes.md"), "a\na\n").unwrap();
    let err = edit("a", "b").unwrap_err();
    assert!(err.message.contains("2 times"), "{}", err.message);
    assert_eq!(code(edit("", "b")), ErrorCode::InvalidArgument);
    let done = edit("a\na", "a\nb").unwrap();
    assert_eq!(done.effect, Effect::Edited);
    assert_eq!(
        fs::read_to_string(f.docs.join("notes.md")).unwrap(),
        "a\nb\n"
    );
    // Windows line endings: the edit's \n matches the file's \r\n.
    fs::write(f.docs.join("crlf.txt"), "one\r\ntwo\r\n").unwrap();
    f.writer
        .edit(&EditRequest {
            path: "docs:crlf.txt".into(),
            old_text: "one\ntwo".into(),
            new_text: "one\n2".into(),
            ..EditRequest::default()
        })
        .unwrap();
    assert_eq!(
        fs::read_to_string(f.docs.join("crlf.txt")).unwrap(),
        "one\r\n2\r\n"
    );
}

#[test]
fn makes_folders() {
    let f = fixture();
    let mkdir = |path: &str, parents| {
        f.writer.mkdir(&MkdirRequest {
            path: path.into(),
            parents,
            dry_run: false,
        })
    };
    assert_eq!(
        mkdir("docs:new", false).unwrap().effect,
        Effect::CreatedFolder
    );
    assert!(f.docs.join("new").is_dir());
    assert_eq!(mkdir("docs:new", false).unwrap().effect, Effect::Unchanged);
    assert_eq!(code(mkdir("docs:a/b/c", false)), ErrorCode::NotFound);
    let deep = mkdir("docs:a/b/c", true).unwrap();
    assert_eq!(deep.entries, Some(3));
    assert!(f.docs.join("a/b/c").is_dir());
    assert_eq!(code(mkdir("docs:notes.md/x", true)), ErrorCode::Exists);
    assert_eq!(code(mkdir("docs:notes.md", false)), ErrorCode::Exists);
    assert_eq!(code(mkdir("docs:linked/x", true)), ErrorCode::Exists);
    assert_eq!(code(mkdir("docs:.ssh", false)), ErrorCode::Denied);
    assert_eq!(code(mkdir("docs:x/.git/hooks", true)), ErrorCode::Denied);
    assert!(!f.docs.join("x").exists(), "nothing made before a refusal");
    assert_eq!(
        code(mkdir("docs:Thing.app", false)),
        ErrorCode::NotChangeable
    );
    assert_eq!(code(mkdir("ro:new", true)), ErrorCode::NotWritable);
}

#[test]
fn moves_and_renames() {
    let f = fixture();
    let mv = |from: &str, to: &str, replace| {
        f.writer.move_entry(&MoveRequest {
            from: from.into(),
            to: to.into(),
            replace,
            dry_run: false,
        })
    };
    let moved = mv("docs:notes.md", "docs:sub/renamed.md", false).unwrap();
    assert_eq!(moved.effect, Effect::Moved);
    assert_eq!(moved.from.as_deref(), Some("docs:notes.md"));
    assert!(f.docs.join("sub/renamed.md").exists() && !f.docs.join("notes.md").exists());
    // Between two folders that allow changes.
    mv("docs:sub/renamed.md", "other:renamed.md", false).unwrap();
    assert!(f.base.join("other/renamed.md").exists());
    // Never into or out of a read-only folder.
    assert_eq!(
        code(mv("other:renamed.md", "ro:x.md", false)),
        ErrorCode::NotWritable
    );
    assert_eq!(
        code(mv("ro:keep.md", "docs:keep.md", false)),
        ErrorCode::NotWritable
    );
    // Never onto something, unless asked; then the old one goes to the trash.
    fs::write(f.docs.join("a.md"), "a").unwrap();
    fs::write(f.docs.join("b.md"), "b").unwrap();
    assert_eq!(code(mv("docs:a.md", "docs:b.md", false)), ErrorCode::Exists);
    let replaced = mv("docs:a.md", "docs:b.md", true).unwrap();
    assert!(replaced.previous.unwrap().in_trash);
    assert_eq!(fs::read_to_string(f.docs.join("b.md")).unwrap(), "a");
    assert_eq!(trashed(&f), ["b.md"]);
    assert_eq!(code(mv("docs:b.md", "docs:sub", true)), ErrorCode::Exists);
    // Folders, but not into themselves.
    mv("docs:sub", "docs:moved", false).unwrap();
    assert!(f.docs.join("moved").is_dir());
    assert_eq!(
        code(mv("docs:moved", "docs:moved/inner", false)),
        ErrorCode::InvalidArgument
    );
    // Not into or out of the deny list, nor out of reach.
    assert_eq!(
        code(mv("docs:.env", "docs:env.txt", false)),
        ErrorCode::Denied
    );
    assert_eq!(
        code(mv("docs:b.md", "docs:.ssh/b.md", false)),
        ErrorCode::Denied
    );
    assert_eq!(
        code(mv("docs:b.md", "docs:id_rsa", false)),
        ErrorCode::Denied
    );
    assert_eq!(
        code(mv("docs:b.md", "docs:b.exe", false)),
        ErrorCode::NotChangeable
    );
    assert_eq!(
        code(mv("docs:escape.txt", "docs:e.txt", false)),
        ErrorCode::Denied
    );
    assert_eq!(code(mv("docs:linked", "docs:l", false)), ErrorCode::Denied);
    assert_eq!(
        code(mv("docs:hardlink.txt", "docs:h.txt", false)),
        ErrorCode::Denied
    );
    assert_eq!(
        code(mv("docs:b.md", "docs:linked/b.md", false)),
        ErrorCode::Denied
    );
    // A folder holding something private stays.
    fs::create_dir_all(f.docs.join("proj")).unwrap();
    fs::write(f.docs.join("proj/.env"), "X=1").unwrap();
    assert_eq!(
        code(mv("docs:proj", "docs:proj2", false)),
        ErrorCode::Denied
    );
    assert!(f.docs.join("proj/.env").exists());
}

#[test]
fn deletes_to_the_trash_only() {
    let f = fixture();
    let delete = |path: &str| {
        f.writer.delete(&DeleteRequest {
            path: path.into(),
            dry_run: false,
        })
    };
    let gone = delete("docs:notes.md").unwrap();
    assert_eq!(gone.effect, Effect::Trashed);
    assert!(!f.docs.join("notes.md").exists());
    let at = gone.trashed_to.unwrap();
    assert_eq!(fs::read_to_string(&at).unwrap(), "# Notes\nOne\nTwo\n");
    #[cfg(not(target_os = "macos"))]
    {
        let record = fs::read_to_string(f.trash.join("info/notes.md.trashinfo")).unwrap();
        assert!(
            record.contains(&format!("Path={}/notes.md", f.docs.display())),
            "{record}"
        );
    }

    fs::write(f.docs.join("sub/a.md"), "a").unwrap();
    let folder = delete("docs:sub").unwrap();
    assert_eq!(folder.entries, Some(1));
    assert!(!f.docs.join("sub").exists());
    assert!(trash_files(&f.trash).join("sub/a.md").exists());

    assert_eq!(code(delete("docs:")), ErrorCode::InvalidPath);
    assert_eq!(code(delete("docs:missing.md")), ErrorCode::NotFound);
    assert_eq!(code(delete("docs:.env")), ErrorCode::Denied);
    assert_eq!(code(delete("ro:keep.md")), ErrorCode::NotWritable);
    // Programs can go to the trash; they just aren't changed.
    fs::write(f.docs.join("tool.sh"), "echo").unwrap();
    fs::set_permissions(f.docs.join("tool.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    delete("docs:tool.sh").unwrap();
}

#[test]
fn refuses_when_the_trash_is_out_of_reach() {
    let f = fixture();
    // Someone made the trash a link to elsewhere.
    fs::create_dir_all(f.base.join("elsewhere")).unwrap();
    fs::create_dir_all(f.trash.parent().unwrap()).unwrap();
    symlink(f.base.join("elsewhere"), &f.trash).unwrap();
    for refused in [
        f.writer.delete(&DeleteRequest {
            path: "docs:notes.md".into(),
            dry_run: false,
        }),
        write(&f, "docs:notes.md", "new", WriteMode::Replace),
    ] {
        assert_eq!(code(refused), ErrorCode::TrashUnavailable);
    }
    assert_eq!(
        fs::read_to_string(f.docs.join("notes.md")).unwrap(),
        "# Notes\nOne\nTwo\n"
    );
    // No temporary files left behind either.
    let names: Vec<String> = fs::read_dir(&f.docs)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".cww-"))
        .collect();
    assert!(names.is_empty(), "{names:?}");
    // Creating needs no trash.
    create(&f, "docs:new.md", "x").unwrap();
}

#[test]
fn refuses_folders_that_hold_a_shared_folder() {
    let f = fixture();
    // `docs` is inside `base`; share `base/outer` holding `docs`-like folder.
    let inner = f.docs.join("inner");
    fs::create_dir_all(&inner).unwrap();
    let paths = Paths::under(&f.base.join("cww2"));
    let root = |id: &str, path: &Path| Root {
        id: id.into(),
        label: id.into(),
        path: path.to_path_buf(),
        follow_symlinks: false,
        writable: true,
    };
    let mut config = Config {
        roots: vec![root("docs", &f.docs), root("inner", &inner)],
        ..Config::default()
    };
    config.index.enabled = false;
    let reader = Arc::new(Reader::new(&config, &paths, Changes::default()).unwrap());
    let writer = Writer::new(
        reader,
        Arc::new(Limiter::new(Limits::default())),
        Trash::new(paths.home_trash.clone()),
    );
    let refused = writer.delete(&DeleteRequest {
        path: "docs:inner".into(),
        dry_run: false,
    });
    assert_eq!(code(refused), ErrorCode::Denied);
    assert!(inner.is_dir());
}

#[test]
fn folders_shared_read_only_stay_read_only_inside_writable_ones() {
    let f = fixture();
    let inner = f.docs.join("private");
    fs::create_dir_all(&inner).unwrap();
    fs::write(inner.join("keep.md"), "keep").unwrap();
    let paths = Paths::under(&f.base.join("cww4"));
    let root = |id: &str, path: &Path, writable| Root {
        id: id.into(),
        label: id.into(),
        path: path.to_path_buf(),
        follow_symlinks: false,
        writable,
    };
    let mut config = Config {
        roots: vec![root("docs", &f.docs, true), root("private", &inner, false)],
        ..Config::default()
    };
    config.index.enabled = false;
    let reader = Arc::new(Reader::new(&config, &paths, Changes::default()).unwrap());
    let writer = Writer::new(
        reader,
        Arc::new(Limiter::new(Limits::default())),
        Trash::new(paths.home_trash.clone()),
    );
    let refused = writer.create(&CreateRequest {
        path: "docs:private/new.md".into(),
        content: "x".into(),
        dry_run: false,
    });
    assert_eq!(code(refused), ErrorCode::NotWritable);
    let refused = writer.mkdir(&MkdirRequest {
        path: "docs:private/a/b".into(),
        parents: true,
        dry_run: false,
    });
    assert_eq!(code(refused), ErrorCode::NotWritable);
    let refused = writer.move_entry(&MoveRequest {
        from: "docs:notes.md".into(),
        to: "docs:private/notes.md".into(),
        replace: false,
        dry_run: false,
    });
    assert_eq!(code(refused), ErrorCode::NotWritable);
    assert!(!inner.join("new.md").exists() && !inner.join("a").exists());
    writer
        .create(&CreateRequest {
            path: "docs:public.md".into(),
            content: "x".into(),
            dry_run: false,
        })
        .unwrap();
}

#[test]
fn counts_changes_against_their_own_budget() {
    let f = fixture_with(Limits {
        changes_per_minute: 2,
        ..Limits::default()
    });
    create(&f, "docs:1.md", "1").unwrap();
    // Dry runs and refusals don't count.
    let _ = create(&f, "docs:1.md", "again");
    f.writer
        .create(&CreateRequest {
            path: "docs:2.md".into(),
            content: "2".into(),
            dry_run: true,
        })
        .unwrap();
    create(&f, "docs:2.md", "2").unwrap();
    assert_eq!(code(create(&f, "docs:3.md", "3")), ErrorCode::RateLimited);
    assert!(!f.docs.join("3.md").exists());
}

#[test]
fn caps_file_sizes() {
    let f = fixture_with(Limits {
        max_change_file_bytes: 10,
        ..Limits::default()
    });
    assert_eq!(
        code(create(&f, "docs:big.md", "0123456789A")),
        ErrorCode::TooLarge
    );
    assert_eq!(
        code(write(&f, "docs:notes.md", "x", WriteMode::Append)),
        ErrorCode::TooLarge
    );
}

/// `/dev/shm` is usually its own filesystem: a move there copies, then puts
/// the original in the trash.
#[cfg(target_os = "linux")]
#[test]
fn moves_across_filesystems_by_copying() {
    use std::os::unix::fs::MetadataExt;
    let Ok(shm) = tempfile::tempdir_in("/dev/shm") else {
        return;
    };
    let f = fixture();
    let far = shm.path().canonicalize().unwrap();
    if fs::metadata(&far).unwrap().dev() == fs::metadata(&f.docs).unwrap().dev() {
        return;
    }
    let paths = Paths::under(&f.base.join("cww3"));
    let root = |id: &str, path: &Path| Root {
        id: id.into(),
        label: id.into(),
        path: path.to_path_buf(),
        follow_symlinks: false,
        writable: true,
    };
    fs::write(f.docs.join("sub/a.md"), "a").unwrap();
    let mut config = Config {
        roots: vec![root("docs", &f.docs), root("far", &far)],
        ..Config::default()
    };
    config.index.enabled = false;
    let reader = Arc::new(Reader::new(&config, &paths, Changes::default()).unwrap());
    let trash = paths.home_trash.clone().unwrap();
    let writer = Writer::new(
        reader,
        Arc::new(Limiter::new(Limits::default())),
        Trash::new(Some(trash.clone())),
    );
    let mv = |from: &str, to: &str| {
        writer.move_entry(&MoveRequest {
            from: from.into(),
            to: to.into(),
            replace: false,
            dry_run: false,
        })
    };
    let moved = mv("docs:notes.md", "far:notes.md").unwrap();
    assert_eq!(moved.written, 16);
    assert_eq!(
        fs::read_to_string(far.join("notes.md")).unwrap(),
        "# Notes\nOne\nTwo\n"
    );
    assert!(!f.docs.join("notes.md").exists());
    assert!(trash.join("files/notes.md").exists());
    mv("docs:sub", "far:sub").unwrap();
    assert_eq!(fs::read_to_string(far.join("sub/a.md")).unwrap(), "a");
    assert!(trash.join("files/sub/a.md").exists());
    // A folder with a link in it can't be copied.
    fs::create_dir_all(f.docs.join("withlink")).unwrap();
    symlink("x", f.docs.join("withlink/l")).unwrap();
    assert_eq!(
        code(mv("docs:withlink", "far:withlink")),
        ErrorCode::NotChangeable
    );
    assert!(!far.join("withlink").exists());
}

/// Windows reports a folder reached by its 8.3 short name under its long
/// name. Stand in for that with a real-path resolver that names `GIT~1` as
/// `.git` and `SSH~1` as `.ssh`: every check must use the real path.
#[test]
fn checks_changes_against_the_real_path_of_each_folder() {
    let f = fixture();
    fs::create_dir_all(f.docs.join("proj/GIT~1")).unwrap();
    fs::write(f.docs.join("proj/GIT~1/config"), "[core]\n").unwrap();
    fs::create_dir_all(f.docs.join("keys/SSH~1")).unwrap();
    fs::write(f.docs.join("keys/SSH~1/notes.md"), "x").unwrap();
    let docs = f.docs.clone();
    let writer = f.writer.with_real_paths(|dir| {
        dir.real_path().map(|p| {
            PathBuf::from(
                p.to_string_lossy()
                    .replace("GIT~1", ".git")
                    .replace("SSH~1", ".ssh"),
            )
        })
    });
    let refused = writer.write(&WriteRequest {
        path: "docs:proj/GIT~1/config".into(),
        content: "[core]\n\tfsmonitor = evil\n".into(),
        ..WriteRequest::default()
    });
    assert_eq!(code(refused), ErrorCode::Denied);
    assert_eq!(
        fs::read_to_string(docs.join("proj/GIT~1/config")).unwrap(),
        "[core]\n"
    );
    let refused = writer.create(&CreateRequest {
        path: "docs:proj/GIT~1/hooks.txt".into(),
        content: "x".into(),
        dry_run: false,
    });
    assert_eq!(code(refused), ErrorCode::Denied);
    let refused = writer.mkdir(&MkdirRequest {
        path: "docs:proj/GIT~1/hooks/x".into(),
        parents: true,
        dry_run: false,
    });
    assert_eq!(code(refused), ErrorCode::Denied);
    let refused = writer.create(&CreateRequest {
        path: "docs:keys/SSH~1/authorized_keys".into(),
        content: "ssh-ed25519 AAAA".into(),
        dry_run: false,
    });
    assert_eq!(code(refused), ErrorCode::Denied);
    // A folder holding one can't be moved or deleted either.
    let refused = writer.delete(&DeleteRequest {
        path: "docs:keys".into(),
        dry_run: false,
    });
    assert_eq!(code(refused), ErrorCode::Denied);
    assert!(docs.join("keys/SSH~1/notes.md").exists());
    // Elsewhere, changes work as usual.
    writer
        .create(&CreateRequest {
            path: "docs:proj/readme.md".into(),
            content: "x".into(),
            dry_run: false,
        })
        .unwrap();
}

/// A folder whose real place can't be told isn't changed.
#[test]
fn refuses_changes_where_the_real_path_is_unknown() {
    let f = fixture();
    let docs = f.docs.clone();
    let writer = f.writer.with_real_paths(|_| None);
    let refused = writer.create(&CreateRequest {
        path: "docs:new.md".into(),
        content: "x".into(),
        dry_run: false,
    });
    assert_eq!(code(refused), ErrorCode::Internal);
    assert!(!docs.join("new.md").exists());
}

/// On a case-insensitive disk `PRIVATE` reaches the folder shared read-only
/// as `private`. Renaming it after it was shared does the same on any disk:
/// its configured path no longer matches, but its identity does.
#[test]
fn folders_shared_read_only_stay_read_only_whatever_name_reaches_them() {
    let f = fixture();
    let inner = f.docs.join("private");
    fs::create_dir_all(&inner).unwrap();
    fs::write(inner.join("keep.md"), "keep").unwrap();
    let paths = Paths::under(&f.base.join("cww5"));
    let root = |id: &str, path: &Path, writable| Root {
        id: id.into(),
        label: id.into(),
        path: path.to_path_buf(),
        follow_symlinks: false,
        writable,
    };
    let mut config = Config {
        roots: vec![root("docs", &f.docs, true), root("private", &inner, false)],
        ..Config::default()
    };
    config.index.enabled = false;
    let reader = Arc::new(Reader::new(&config, &paths, Changes::default()).unwrap());
    let writer = Writer::new(
        reader,
        Arc::new(Limiter::new(Limits::default())),
        Trash::new(paths.home_trash.clone()),
    );
    let other = f.docs.join("PRIVATE");
    fs::rename(&inner, &other).unwrap();

    let refused = writer.create(&CreateRequest {
        path: "docs:PRIVATE/new.md".into(),
        content: "x".into(),
        dry_run: false,
    });
    assert_eq!(code(refused), ErrorCode::NotWritable);
    let refused = writer.write(&WriteRequest {
        path: "docs:PRIVATE/keep.md".into(),
        content: "changed".into(),
        ..WriteRequest::default()
    });
    assert_eq!(code(refused), ErrorCode::NotWritable);
    let refused = writer.mkdir(&MkdirRequest {
        path: "docs:PRIVATE/a/b".into(),
        parents: true,
        dry_run: false,
    });
    assert_eq!(code(refused), ErrorCode::NotWritable);
    let refused = writer.move_entry(&MoveRequest {
        from: "docs:notes.md".into(),
        to: "docs:PRIVATE/notes.md".into(),
        replace: false,
        dry_run: false,
    });
    assert_eq!(code(refused), ErrorCode::NotWritable);
    let refused = writer.move_entry(&MoveRequest {
        from: "docs:PRIVATE".into(),
        to: "docs:p2".into(),
        replace: false,
        dry_run: false,
    });
    assert_eq!(code(refused), ErrorCode::Denied);
    let refused = writer.delete(&DeleteRequest {
        path: "docs:PRIVATE".into(),
        dry_run: false,
    });
    assert_eq!(code(refused), ErrorCode::Denied);
    // Nor a folder that holds it under yet another name.
    fs::create_dir_all(f.docs.join("outer")).unwrap();
    fs::rename(&other, f.docs.join("outer/Private")).unwrap();
    let refused = writer.delete(&DeleteRequest {
        path: "docs:outer".into(),
        dry_run: false,
    });
    assert_eq!(code(refused), ErrorCode::Denied);
    assert_eq!(
        fs::read_to_string(f.docs.join("outer/Private/keep.md")).unwrap(),
        "keep"
    );
    assert!(!f.docs.join("outer/Private/new.md").exists());
}
