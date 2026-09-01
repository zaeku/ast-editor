//! Upgrade paths, exercised against a store built under an older schema.
//!
//! A suite that always starts from an empty database cannot catch a migration
//! defect: the schema it creates is already the new one. These tests hand the
//! tool a store as an earlier version left it and check what it makes of it.
//! This lives in its own test binary so it can point AST_EDITOR_CACHE_DIR at a
//! purpose-built directory without disturbing the other tests.

use ast_editor::tools::session_db::{self, SessionRepository, SqliteSessionRepository};
use std::fs;
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

/// AST_EDITOR_CACHE_DIR is process-global, so these tests take turns.
fn serialise() -> std::sync::MutexGuard<'static, ()> {
    match ast_editor::tools::TEST_DB_LOCK.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// A store directory with a file and a `sessions.db` written the old way.
struct LegacyStore {
    dir: PathBuf,
}

impl LegacyStore {
    /// `sessions` and `lines` as they stood before the content column was
    /// dropped: line text cached in the database, four-character hashes, and a
    /// session recording a hash and mtime that still match the file.
    fn new(name: &str, content: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("ast-editor-legacy-{}-{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let file = dir.join(name);
        fs::write(&file, content).unwrap();
        let mtime = fs::metadata(&file).unwrap().modified().unwrap()
            .duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
        let file_hash = {
            use sha2::{Digest, Sha256};
            format!("{:x}", Sha256::digest(content.as_bytes()))
        };

        let db = rusqlite::Connection::open(dir.join("sessions.db")).unwrap();
        db.execute_batch(
            "CREATE TABLE sessions (
                filepath TEXT PRIMARY KEY,
                session_id TEXT UNIQUE NOT NULL,
                file_hash TEXT NOT NULL,
                mtime INTEGER NOT NULL,
                last_accessed_at INTEGER NOT NULL,
                line_ending_crlf INTEGER DEFAULT 0
             );
             CREATE TABLE lines (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                sequence_id INTEGER NOT NULL,
                line_hash TEXT,
                content TEXT NOT NULL,
                sort_order REAL NOT NULL,
                parent_context TEXT
             );",
        ).unwrap();
        db.execute(
            "INSERT INTO sessions (filepath, session_id, file_hash, mtime, last_accessed_at) VALUES (?1, 'legacy-session', ?2, ?3, ?3)",
            rusqlite::params![file.to_str().unwrap(), file_hash, mtime],
        ).unwrap();
        for (idx, line) in content.lines().enumerate() {
            db.execute(
                "INSERT INTO lines (session_id, sequence_id, line_hash, content, sort_order) VALUES ('legacy-session', ?1, ?2, ?3, ?4)",
                rusqlite::params![idx as i64 + 1, "abcd", line, (idx as f64 + 1.0) * 1000.0],
            ).unwrap();
        }
        drop(db);

        std::env::set_var("AST_EDITOR_CACHE_DIR", &dir);
        Self { dir }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.dir.join("sessions.db")).unwrap()
    }
}

impl Drop for LegacyStore {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn test_a_file_untouched_since_the_upgrade_still_reads() {
    let _lock = serialise();
    let body = "fn main() {\n    let a = 1;\n}\n";
    let store = LegacyStore::new("demo.rs", body);
    let path = store.file("demo.rs");
    let path = path.to_str().unwrap();

    // The old store's hash and mtime match the file, so nothing marks it as
    // needing attention. If the migration leaves a session behind while
    // clearing its lines, the session reports zero lines and this reads empty.
    let meta = session_db::init_edit_session(path, false).unwrap();
    assert_eq!(meta.total_lines, 3, "the upgraded store lost the file's lines");

    let repository = SqliteSessionRepository;
    let lines: Vec<String> = repository
        .fetch_lines_range(&meta.session_id, 1, meta.total_lines)
        .unwrap()
        .into_iter()
        .map(|(_, _, content)| content)
        .collect();
    assert_eq!(lines, vec!["fn main() {", "    let a = 1;", "}"]);
    assert_eq!(fs::read_to_string(path).unwrap(), body);
}

#[test]
fn test_the_upgrade_drops_cached_content_and_widens_the_hashes() {
    let _lock = serialise();
    let store = LegacyStore::new("widths.rs", "fn main() {\n    let a = 1;\n}\n");
    let path = store.file("widths.rs");
    session_db::init_edit_session(path.to_str().unwrap(), false).unwrap();

    let db = store.db();
    let columns: Vec<String> = db
        .prepare("SELECT name FROM pragma_table_info('lines')").unwrap()
        .query_map([], |row| row.get(0)).unwrap()
        .collect::<rusqlite::Result<_>>().unwrap();
    assert!(!columns.contains(&"content".to_string()), "content survived the upgrade: {:?}", columns);

    // The old rows carried four-character hashes. Nothing may be left at that
    // width, or reconciliation would align on 16 bits.
    let widths: Vec<usize> = db
        .prepare("SELECT length(line_hash) FROM lines").unwrap()
        .query_map([], |row| row.get::<_, usize>(0)).unwrap()
        .collect::<rusqlite::Result<_>>().unwrap();
    assert!(!widths.is_empty());
    assert!(widths.iter().all(|w| *w == 16), "stored hashes were not widened: {:?}", widths);
}
