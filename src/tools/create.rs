//! Writing a file that does not exist yet, and answering with its lines.
//!
//! `create` refuses a path something is already at, so an existing file is
//! changed with `edit` and never overwritten by accident. What comes back is
//! the same `[id, line number]` pairs every other answer carries, so the caller
//! can edit inside what it just wrote without reading it again.

use crate::tools::formatter;
use crate::tools::repository::FileStore;
use anyhow::Result;

pub(crate) fn create_lines(
    repository: &impl FileStore,
    filepath: &str,
    content: &str,
    return_ids: Option<bool>,
) -> Result<String> {
    let path = std::path::Path::new(filepath);
    if path.exists() {
        anyhow::bail!("FILE_ALREADY_EXISTS: File already exists at: {}", filepath);
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    std::fs::write(filepath, content)?;

    let return_ids_bool = return_ids.unwrap_or(false);

    let setup_db_and_fetch = || -> Result<(Option<String>, Option<String>, usize, u64)> {
        let meta = repository.init_session(filepath, false)?;
        let total_bytes = std::path::Path::new(filepath).metadata()?.len();
        if return_ids_bool {
            let config = crate::tools::metadata::get_config();
            let formatted_res = crate::tools::formatter::retrieve_and_format_lines(
                repository,
                &meta.file_key,
                1,
                meta.total_lines,
                true,
                config.only_ids_wrap_trigger_length,
                formatter::NO_LINE_CAP,
            )?;
            Ok((
                Some(formatted_res.ids_json),
                formatted_res.warning_msg,
                meta.total_lines,
                total_bytes,
            ))
        } else {
            Ok((None, None, meta.total_lines, total_bytes))
        }
    };

    match setup_db_and_fetch() {
        Ok((items_opt, capacity_warning, total_lines, total_bytes)) => {
            let config = crate::tools::metadata::get_config();
            // A create answers for every line it wrote, so it has no line count
            // to report as capped. The byte cap is what is left, and it arrives
            // as capacity_warning.
            let message = capacity_warning.unwrap_or_default();

            let output = if return_ids_bool {
                let ids: Vec<(String, usize)> = items_opt
                    .and_then(|json_str| serde_json::from_str::<serde_json::Value>(&json_str).ok())
                    .and_then(|val| val.as_array().cloned())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|val| {
                                let item = val.as_array()?;
                                let id = item.first()?.as_str()?.to_string();
                                let line = item.get(1)?.as_u64()? as usize;
                                Some((id, line))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let formatted_ids = crate::tools::formatter::format_lines(
                    &ids,
                    config.only_ids_wrap_trigger_length,
                );
                let indented_ids = formatted_ids.replace("\n", "\n  ");
                format!(
                    "{{\n  \"lines\": {},{}\n  \"total_bytes\": {},\n  \"total_lines\": {}\n}}",
                    indented_ids,
                    warning_field(&message)?,
                    total_bytes,
                    total_lines
                )
            } else {
                format!(
                    "{{{}\n  \"total_bytes\": {},\n  \"total_lines\": {}\n}}",
                    warning_field(&message)?,
                    total_bytes,
                    total_lines
                )
            };
            Ok(output)
        }
        Err(err) => {
            let _ = std::fs::remove_file(filepath);
            Err(err)
        }
    }
}
/// A response carries a message when there is something to say about it —
/// what a call did on its way to succeeding is not news.
fn warning_field(message: &str) -> Result<String> {
    if message.is_empty() {
        return Ok(String::new());
    }
    Ok(format!(
        "\n  \"message\": {},",
        serde_json::to_string(message)?
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::repository::SqliteFileStore;
    use crate::tools::TEST_DB_LOCK as DB_LOCK;
    use std::fs;

    #[test]
    fn test_create_lines_success() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let temp_dir = crate::tools::test_temp_dir("line-editor-test-create-success");
        if temp_dir.exists() {
            fs::remove_dir_all(&temp_dir)?;
        }
        fs::create_dir_all(&temp_dir)?;
        let file_path = temp_dir.join("new_file.txt");
        let filepath_str = file_path.to_str().unwrap();

        let content = "line 1\nline 2\nline 3";
        let repository = SqliteFileStore;
        let output = create_lines(&repository, filepath_str, content, Some(true))?;

        let val: serde_json::Value = serde_json::from_str(&output)?;
        assert!(val["status"].is_null(), "success is the exit code: {}", val);
        assert!(
            val["message"].is_null(),
            "nothing was wrong, so nothing is said"
        );
        assert!(val["columns"].is_null());

        // Each entry is a line as [id, number]: no text comes back.
        let ids = val["lines"].as_array().unwrap();
        assert_eq!(ids.len(), 3);
        for (index, entry) in ids.iter().enumerate() {
            assert!(entry[0]
                .as_str()
                .unwrap()
                .starts_with(&format!("{}#", index + 1)));
            assert_eq!(entry[1].as_u64().unwrap(), index as u64 + 1);
        }
        assert_eq!(val["total_lines"].as_u64().unwrap(), 3);
        assert_eq!(val["total_bytes"].as_u64().unwrap(), content.len() as u64);

        // Verify that the file was indeed written
        let read_content = fs::read_to_string(filepath_str)?;
        assert_eq!(read_content, content);

        fs::remove_dir_all(&temp_dir)?;
        Ok(())
    }

    #[test]
    fn test_create_lines_return_ids_false() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let temp_dir = crate::tools::test_temp_dir("line-editor-test-create-return-ids-false");
        if temp_dir.exists() {
            fs::remove_dir_all(&temp_dir)?;
        }
        fs::create_dir_all(&temp_dir)?;
        let file_path = temp_dir.join("new_file.txt");
        let filepath_str = file_path.to_str().unwrap();

        let content = "line 1\nline 2\nline 3";
        let repository = SqliteFileStore;
        let output = create_lines(&repository, filepath_str, content, Some(false))?;

        let val: serde_json::Value = serde_json::from_str(&output)?;
        assert!(val["status"].is_null(), "success is the exit code: {}", val);
        assert!(
            val["message"].is_null(),
            "nothing was wrong, so nothing is said"
        );
        assert!(val["lines"].is_null());
        assert_eq!(val["total_lines"].as_u64().unwrap(), 3);
        assert_eq!(val["total_bytes"].as_u64().unwrap(), content.len() as u64);

        // Verify that the file was indeed written
        let read_content = fs::read_to_string(filepath_str)?;
        assert_eq!(read_content, content);

        fs::remove_dir_all(&temp_dir)?;
        Ok(())
    }

    #[test]
    fn test_create_lines_already_exists() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let temp_dir = crate::tools::test_temp_dir("line-editor-test-create-exists");
        if temp_dir.exists() {
            fs::remove_dir_all(&temp_dir)?;
        }
        fs::create_dir_all(&temp_dir)?;
        let file_path = temp_dir.join("existing_file.txt");
        let filepath_str = file_path.to_str().unwrap();

        // Write initial content
        let initial_content = "existing content";
        fs::write(filepath_str, initial_content)?;

        // Try to create again
        let repository = SqliteFileStore;
        let res = create_lines(&repository, filepath_str, "new content", None);
        assert!(res.is_err());
        let err_msg = res.err().unwrap().to_string();
        assert!(err_msg.contains("FILE_ALREADY_EXISTS"));
        assert!(err_msg.contains("File already exists at"));

        // Verify that content was NOT overwritten
        let read_content = fs::read_to_string(filepath_str)?;
        assert_eq!(read_content, initial_content);

        fs::remove_dir_all(&temp_dir)?;
        Ok(())
    }

    #[test]
    fn test_create_lines_db_failure_rollback() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let temp_dir = crate::tools::test_temp_dir("line-editor-test-create-rollback");
        if temp_dir.exists() {
            fs::remove_dir_all(&temp_dir)?;
        }
        fs::create_dir_all(&temp_dir)?;
        let file_path = temp_dir.join("binary_file.bin");
        let filepath_str = file_path.to_str().unwrap();

        // Write content containing a null byte to trigger BINARY_FILE_ERROR during DB initialization
        let content = "hello \x00 world";
        let repository = SqliteFileStore;
        let res = create_lines(&repository, filepath_str, content, None);

        assert!(res.is_err());
        let err_msg = res.err().unwrap().to_string();
        assert!(err_msg.contains("BINARY_FILE_ERROR"));

        // Verify that the file was deleted/rolled back
        assert!(!file_path.exists());

        fs::remove_dir_all(&temp_dir)?;
        Ok(())
    }
}
