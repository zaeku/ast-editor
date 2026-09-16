//! What `session_db.rs` declares, tested against a real store.
//!
//! Its own file rather than a module inside that one: the tests are two fifths
//! of it, and the cut this leaves behind — a store and the ids keyed on it —
//! has to decide where each of them lands. Moving them first makes that one
//! question rather than two.

#![allow(clippy::await_holding_lock)]

use crate::tools::repository::{SessionRepository, SqliteSessionRepository};
use crate::tools::session_db::*;
use crate::tools::TEST_DB_LOCK as DB_LOCK;
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

fn view_lines_old_compat(
    repository: &impl SessionRepository,
    filepath: &str,
    start_line: usize,
    end_line: usize,
    only_ids: Option<bool>,
) -> Result<String> {
    let res = crate::tools::view::view_lines(
        repository,
        filepath,
        Some(start_line),
        Some(end_line),
        only_ids,
        None,
        None,
        None,
    )?;
    let ids_val: serde_json::Value = serde_json::from_str(&res.metadata_json)?;

    let mut lines = Vec::new();
    if let Some(ref text) = res.lines_text {
        for row in text.lines() {
            let (head, content) = row.split_once(": ").unwrap();
            let (id, n) = head.split_once('|').unwrap();
            lines.push(serde_json::json!([
                id.trim(),
                n.trim().parse::<usize>().unwrap(),
                content
            ]));
        }
    } else {
        for id_entry in ids_val["lines"].as_array().unwrap() {
            let id_arr = id_entry.as_array().unwrap();
            let id = id_arr[0].as_str().unwrap();
            let n = id_arr[1].as_u64().unwrap() as usize;
            lines.push(serde_json::json!([id, n]));
        }
    }

    let mut val = ids_val.clone();
    val["lines"] = serde_json::Value::Array(lines);
    let columns = if only_ids.unwrap_or(false) {
        vec!["id", "n"]
    } else {
        vec!["id", "n", "content"]
    };
    val["columns"] = serde_json::json!(columns);
    Ok(serde_json::to_string(&val)?)
}

#[test]
fn test_create_tables_in_memory() -> Result<()> {
    let _lock = DB_LOCK.lock().unwrap_or_else(|err| err.into_inner());
    let conn = Connection::open_in_memory()?;

    // Configure pragmas
    conn.execute("PRAGMA foreign_keys = ON;", [])
        .context("Failed to enable foreign keys in test database")?;

    // Create tables
    create_tables(&conn)?;

    // Verify that tables exist by query
    let mut stmt =
        conn.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='sessions';")?;
    let mut rows = stmt.query([])?;
    assert!(rows.next()?.is_some(), "sessions table should exist");

    let mut stmt =
        conn.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='lines';")?;
    let mut rows = stmt.query([])?;
    assert!(rows.next()?.is_some(), "lines table should exist");

    // The store hands pages back rather than keeping them on a free list,
    // which is the setting it has to be switched into: a store that only
    // ever grows is the cost of keeping ids, paid forever.
    let auto_vacuum: i64 = conn.pragma_query_value(None, "auto_vacuum", |row| row.get(0))?;
    assert_eq!(
        auto_vacuum, 2,
        "the store was left on auto_vacuum {auto_vacuum} rather than incremental"
    );

    Ok(())
}

#[test]
fn test_get_db_path() -> Result<()> {
    let _lock = DB_LOCK.lock().unwrap_or_else(|err| err.into_inner());
    let path = get_db_path()?;
    assert!(path.to_string_lossy().contains("sessions.db"));

    let temp_dir = crate::tools::test_temp_dir("line-editor-test");
    assert!(path.starts_with(&temp_dir));

    if path.exists() {
        fs::remove_file(&path).context("Failed to clean up test DB file")?;
    }
    if temp_dir.exists() {
        fs::remove_dir_all(&temp_dir).context("Failed to clean up test directory")?;
    }

    Ok(())
}

#[test]
fn test_is_binary_file() -> Result<()> {
    let temp_dir = crate::tools::test_temp_dir("line-editor-test-binary");
    fs::create_dir_all(&temp_dir)?;

    let text_path = temp_dir.join("text.txt");
    fs::write(&text_path, "Hello world!")?;
    assert!(!is_binary_file(text_path.to_str().unwrap())?);

    let binary_path = temp_dir.join("binary.bin");
    fs::write(&binary_path, b"Hello\x00world")?;
    assert!(is_binary_file(binary_path.to_str().unwrap())?);

    fs::remove_dir_all(&temp_dir)?;
    Ok(())
}

#[test]
fn test_check_language_supported() {
    assert!(check_language_supported("foo.rs"));
    assert!(check_language_supported("bar.py"));
    assert!(check_language_supported("baz.ts"));
    assert!(check_language_supported("main.cpp"));
    assert!(check_language_supported("index.html"));
    assert!(check_language_supported("config.toml"));
    assert!(check_language_supported("FOO.RS"));
    assert!(check_language_supported("Bar.Py"));
    assert!(check_language_supported("foo.md"));
    assert!(check_language_supported("bar.markdown"));
    assert!(check_language_supported("script.sh"));
    assert!(check_language_supported("script.bash"));
    assert!(check_language_supported("script.zsh"));
    assert!(check_language_supported("script.ksh"));
    assert!(!check_language_supported("foo.txt"));
    assert!(!check_language_supported("foo.pdf"));
    assert!(!check_language_supported("foo"));
}

#[test]
fn test_compute_sha256() -> Result<()> {
    let temp_dir = crate::tools::test_temp_dir("line-editor-test-sha");
    fs::create_dir_all(&temp_dir)?;
    let file_path = temp_dir.join("test.txt");
    fs::write(&file_path, "hello")?;

    let hash = compute_sha256(file_path.to_str().unwrap())?;
    assert_eq!(
        hash,
        "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
    );

    fs::remove_dir_all(&temp_dir)?;
    Ok(())
}

#[test]
fn test_cleanup_stale_sessions() -> Result<()> {
    let _lock = DB_LOCK.lock().unwrap_or_else(|err| err.into_inner());
    let conn = Connection::open_in_memory()?;
    create_tables(&conn)?;

    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
    // Expressed in seconds rather than against the window, because a time
    // derived from the window is stale or fresh whatever the window is, and
    // the test then holds for a window of two minutes or of none. What the
    // window is for decides the two numbers: it has to outlast the task an
    // agent is in the middle of, and a session nothing has touched for a
    // month is not one of those.
    let day = 24 * 60 * 60;
    let stale_time = now - 31 * day;
    let fresh_time = now - day;

    conn.execute(
        "INSERT INTO sessions (filepath, session_id, file_hash, mtime, last_accessed_at) VALUES (?1, ?2, ?3, ?4, ?5);",
        rusqlite::params!["stale.rs", "session_stale", "hash1", 100, stale_time],
    )?;

    conn.execute(
        "INSERT INTO sessions (filepath, session_id, file_hash, mtime, last_accessed_at) VALUES (?1, ?2, ?3, ?4, ?5);",
        rusqlite::params!["fresh.rs", "session_fresh", "hash2", 100, fresh_time],
    )?;

    cleanup_stale_sessions(&conn)?;

    let mut stmt = conn.prepare("SELECT count(*) FROM sessions")?;
    let count: i64 = stmt.query_row([], |row| row.get(0))?;
    assert_eq!(count, 1);

    let mut stmt = conn.prepare("SELECT filepath FROM sessions")?;
    let filepath: String = stmt.query_row([], |row| row.get(0))?;
    assert_eq!(filepath, "fresh.rs");

    Ok(())
}

#[test]
fn test_init_edit_session_nonexistent_file() -> Result<()> {
    let _lock = DB_LOCK.lock().unwrap_or_else(|err| err.into_inner());
    let temp_dir = crate::tools::test_temp_dir("line-editor-test-init-nonexistent");
    if temp_dir.exists() {
        fs::remove_dir_all(&temp_dir)?;
    }
    let file_path = temp_dir.join("missing.rs");
    let filepath_str = file_path.to_str().unwrap();

    let res = init_edit_session(filepath_str, false);
    assert!(res.is_err());

    let metadata = init_edit_session(filepath_str, true)?;
    assert_eq!(metadata.total_lines, 0);
    assert!(metadata.is_supported);
    assert!(file_path.exists());

    fs::remove_dir_all(&temp_dir)?;
    Ok(())
}

#[test]
fn test_init_edit_session_binary_file() -> Result<()> {
    let _lock = DB_LOCK.lock().unwrap_or_else(|err| err.into_inner());
    let temp_dir = crate::tools::test_temp_dir("line-editor-test-init-binary");
    fs::create_dir_all(&temp_dir)?;
    let file_path = temp_dir.join("binary.bin");
    fs::write(&file_path, b"Hello\x00world")?;

    let res = init_edit_session(file_path.to_str().unwrap(), false);
    assert!(res.is_err());
    let err_msg = format!("{:?}", res.err().unwrap());
    assert!(err_msg.contains("BINARY_FILE_ERROR"));

    fs::remove_dir_all(&temp_dir)?;
    Ok(())
}

#[test]
fn test_init_edit_session_unsupported_language() -> Result<()> {
    let _lock = DB_LOCK.lock().unwrap_or_else(|err| err.into_inner());
    let temp_dir = crate::tools::test_temp_dir("line-editor-test-init-unsupported");
    fs::create_dir_all(&temp_dir)?;
    let file_path = temp_dir.join("unsupported.txt");
    fs::write(&file_path, "Hello\nworld")?;

    let metadata = init_edit_session(file_path.to_str().unwrap(), false)?;
    assert_eq!(metadata.total_lines, 2);
    assert!(!metadata.is_supported);
    assert!(metadata.warning_message.is_some());

    fs::remove_dir_all(&temp_dir)?;
    Ok(())
}

#[test]
fn test_init_edit_session_lifecycle() -> Result<()> {
    let _lock = DB_LOCK.lock().unwrap_or_else(|err| err.into_inner());
    let temp_dir = crate::tools::test_temp_dir("line-editor-test-init-lifecycle");
    fs::create_dir_all(&temp_dir)?;
    let file_path = temp_dir.join("code.rs");
    fs::write(&file_path, "fn main() {\n    println!(\"Hello!\");\n}\n")?;
    let filepath_str = file_path.to_str().unwrap();

    let metadata1 = init_edit_session(filepath_str, false)?;
    assert_eq!(metadata1.total_lines, 3);
    assert!(metadata1.is_supported);
    assert!(metadata1.warning_message.is_none());

    let metadata2 = init_edit_session(filepath_str, false)?;
    assert_eq!(metadata1.session_id, metadata2.session_id);
    assert_eq!(metadata1.file_hash, metadata2.file_hash);
    assert_eq!(metadata1.mtime, metadata2.mtime);

    let repository = SqliteSessionRepository;
    let ids_before: Vec<i64> = repository
        .fetch_lines_range(&metadata1.session_id, 1, metadata1.total_lines)?
        .into_iter()
        .map(|(seq, _, _)| seq)
        .collect();

    fs::write(
        &file_path,
        "fn main() {\n    println!(\"Hello!\");\n    // extra line\n}\n",
    )?;
    let metadata3 = init_edit_session(filepath_str, false)?;
    // The session is reconciled rather than rebuilt, so it keeps its
    // identity and the lines that survived keep their sequence numbers.
    assert_eq!(metadata1.session_id, metadata3.session_id);
    assert_ne!(metadata1.file_hash, metadata3.file_hash);
    assert_eq!(metadata3.total_lines, 4);

    let ids_after: Vec<i64> = repository
        .fetch_lines_range(&metadata3.session_id, 1, metadata3.total_lines)?
        .into_iter()
        .map(|(seq, _, _)| seq)
        .collect();
    assert_eq!(
        &ids_after[..2],
        &ids_before[..2],
        "unchanged lines lost their ids"
    );
    assert_eq!(
        ids_after[3], ids_before[2],
        "the moved closing brace lost its id"
    );
    assert!(
        !ids_before.contains(&ids_after[2]),
        "the inserted line reused an id"
    );

    // The store holds no text, so content is checked through the buffer
    // the repository serves from disk.
    let lines: Vec<String> = repository
        .fetch_lines_range(&metadata3.session_id, 1, metadata3.total_lines)?
        .into_iter()
        .map(|(_, _, content)| content)
        .collect();
    assert_eq!(
        lines,
        vec![
            "fn main() {".to_string(),
            "    println!(\"Hello!\");".to_string(),
            "    // extra line".to_string(),
            "}".to_string(),
        ]
    );

    fs::remove_dir_all(&temp_dir)?;
    Ok(())
}

#[test]
fn test_surfaced_hash_is_a_prefix_of_the_stored_hash() {
    // The index keeps the wide hash; ids carry its first four characters,
    // so a short id can be checked against a stored row directly.
    for content in ["", "fn main() {", "    let a = 1;", "}"] {
        let stored = compute_stored_hash(content);
        assert_eq!(stored.len(), 16);
        assert_eq!(compute_line_hash(content), stored[..4]);
    }
}

#[tokio::test]
async fn test_view_lines() -> Result<()> {
    let _lock = DB_LOCK.lock().unwrap_or_else(|err| err.into_inner());
    let temp_dir = crate::tools::test_temp_dir("line-editor-test-view");
    if temp_dir.exists() {
        fs::remove_dir_all(&temp_dir)?;
    }
    fs::create_dir_all(&temp_dir)?;
    let file_path = temp_dir.join("code.rs");
    fs::write(&file_path, "fn main() {\n    println!(\"Hello!\");\n}\n")?;
    let filepath_str = file_path.to_str().unwrap();

    let repository = SqliteSessionRepository;

    // 1. Initialize session
    let metadata = init_edit_session(filepath_str, false)?;
    assert_eq!(metadata.total_lines, 3);

    // 2. View all lines (1 to 3)
    let output = view_lines_old_compat(&repository, filepath_str, 1, 3, None)?;

    // Verify structure
    let val: serde_json::Value = serde_json::from_str(&output)?;
    let lines = val["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 3);

    let line0 = lines[0].as_array().unwrap();
    let line1 = lines[1].as_array().unwrap();
    let line2 = lines[2].as_array().unwrap();

    assert!(line0[0].as_str().unwrap().starts_with("1#"));
    assert!(line1[0].as_str().unwrap().starts_with("2#"));
    assert!(line2[0].as_str().unwrap().starts_with("3#"));
    assert_eq!(line0[2].as_str().unwrap(), "fn main() {");
    assert_eq!(line1[2].as_str().unwrap(), "    println!(\"Hello!\");");
    assert_eq!(line2[2].as_str().unwrap(), "}");

    // 3. View sub-range (2 to 2)
    let sub_output = view_lines_old_compat(&repository, filepath_str, 2, 2, None)?;
    let sub_val: serde_json::Value = serde_json::from_str(&sub_output)?;
    let sub_lines = sub_val["lines"].as_array().unwrap();
    assert_eq!(sub_lines.len(), 1);
    let sub_line0 = sub_lines[0].as_array().unwrap();
    assert!(sub_line0[0].as_str().unwrap().starts_with("2#"));

    // 4. Test bounds validation
    assert!(crate::tools::view::view_lines(
        &repository,
        filepath_str,
        Some(0),
        Some(3),
        None,
        None,
        None,
        None,
    )
    .is_err());
    assert!(crate::tools::view::view_lines(
        &repository,
        filepath_str,
        Some(3),
        Some(1),
        None,
        None,
        None,
        None,
    )
    .is_err());

    fs::remove_dir_all(&temp_dir)?;
    Ok(())
}

#[tokio::test]
async fn test_view_lines_jit_initialization() -> Result<()> {
    let _lock = DB_LOCK.lock().unwrap_or_else(|err| err.into_inner());
    let temp_dir = crate::tools::test_temp_dir("line-editor-test-jit-view");
    if temp_dir.exists() {
        fs::remove_dir_all(&temp_dir)?;
    }
    fs::create_dir_all(&temp_dir)?;
    let file_path = temp_dir.join("code.rs");
    fs::write(&file_path, "fn main() {\n    println!(\"Hello!\");\n}\n")?;
    let filepath_str = file_path.to_str().unwrap();

    let repository = SqliteSessionRepository;

    // Call view_lines directly without calling init_edit_session
    let output = view_lines_old_compat(&repository, filepath_str, 1, 3, None)?;

    let val: serde_json::Value = serde_json::from_str(&output)?;
    let lines = val["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 3);
    assert_eq!(
        lines[0].as_array().unwrap()[2].as_str().unwrap(),
        "fn main() {"
    );

    fs::remove_dir_all(&temp_dir)?;
    Ok(())
}
