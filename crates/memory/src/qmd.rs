use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QmdFile {
    pub path: PathBuf,
    pub frontmatter: QmdFrontmatter,
    pub content: String,
    pub hash: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct QmdFrontmatter {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub archived: bool,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

pub struct QmdManager {
    qmd_dir: PathBuf,
}

impl QmdManager {
    #[must_use]
    pub fn new(qmd_dir: PathBuf) -> Self {
        Self { qmd_dir }
    }

    pub fn list_qmd_files(&self) -> Result<Vec<QmdFile>, std::io::Error> {
        let mut files = Vec::new();

        if !self.qmd_dir.exists() {
            std::fs::create_dir_all(&self.qmd_dir)?;
            return Ok(files);
        }

        for entry in std::fs::read_dir(&self.qmd_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().map(|e| e == "qmd").unwrap_or(false)
                && let Ok(qmd) = self.load_qmd_file(&path)
            {
                files.push(qmd);
            }
        }

        files.sort_by_key(|file| std::cmp::Reverse(file.updated_at));

        Ok(files)
    }

    pub fn load_qmd_file(&self, path: &Path) -> Result<QmdFile, QmdError> {
        let content = std::fs::read_to_string(path)?;
        let hash = Self::hash_content(&content);

        let (frontmatter, body) = parse_frontmatter(&content)?;

        let metadata = std::fs::metadata(path)?;
        let created_at = metadata
            .created()
            .ok()
            .and_then(|t| DateTime::from_timestamp(t.elapsed().ok()?.as_secs() as i64, 0))
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        let updated_at = metadata
            .modified()
            .ok()
            .and_then(|t| DateTime::from_timestamp(t.elapsed().ok()?.as_secs() as i64, 0))
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        Ok(QmdFile {
            path: path.to_path_buf(),
            frontmatter,
            content: body,
            hash,
            created_at,
            updated_at,
        })
    }

    pub fn create_qmd_file(
        &self,
        filename: &str,
        frontmatter: QmdFrontmatter,
        content: &str,
    ) -> Result<QmdFile, QmdError> {
        std::fs::create_dir_all(&self.qmd_dir)?;

        let path = self.qmd_dir.join(format!("{filename}.qmd"));

        let yaml = serde_saphyr::to_string(&frontmatter)
            .map_err(|e| QmdError::ParseError(e.to_string()))?;

        let full_content = format!("---\n{yaml}---\n{content}");

        std::fs::write(&path, &full_content)?;

        self.load_qmd_file(&path)
    }

    pub fn update_qmd_file(
        &self,
        path: &Path,
        frontmatter: Option<QmdFrontmatter>,
        content: Option<&str>,
    ) -> Result<QmdFile, QmdError> {
        let mut qmd = self.load_qmd_file(path)?;

        if let Some(fm) = frontmatter {
            qmd.frontmatter = fm;
        }

        if let Some(c) = content {
            qmd.content = c.to_owned();
        }

        let yaml = serde_saphyr::to_string(&qmd.frontmatter)
            .map_err(|e| QmdError::ParseError(e.to_string()))?;

        let full_content = format!("---\n{}---\n{}", yaml, qmd.content);

        std::fs::write(path, &full_content)?;

        self.load_qmd_file(path)
    }

    pub fn delete_qmd_file(&self, path: &Path) -> Result<(), std::io::Error> {
        std::fs::remove_file(path)
    }

    pub fn search_by_tag(&self, tag: &str) -> Result<Vec<QmdFile>, std::io::Error> {
        let files = self.list_qmd_files()?;
        Ok(files
            .into_iter()
            .filter(|f| f.frontmatter.tags.iter().any(|t| t == tag))
            .collect())
    }

    pub fn search_by_category(&self, category: &str) -> Result<Vec<QmdFile>, std::io::Error> {
        let files = self.list_qmd_files()?;
        Ok(files
            .into_iter()
            .filter(|f| f.frontmatter.category.as_deref() == Some(category))
            .collect())
    }

    pub fn get_pinned(&self) -> Result<Vec<QmdFile>, std::io::Error> {
        let files = self.list_qmd_files()?;
        Ok(files.into_iter().filter(|f| f.frontmatter.pinned).collect())
    }

    pub fn get_archived(&self) -> Result<Vec<QmdFile>, std::io::Error> {
        let files = self.list_qmd_files()?;
        Ok(files
            .into_iter()
            .filter(|f| f.frontmatter.archived)
            .collect())
    }

    fn hash_content(content: &str) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum QmdError {
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Parse error: {0}")]
    ParseError(String),

    #[error("Missing frontmatter")]
    MissingFrontmatter,
}

fn parse_frontmatter(content: &str) -> Result<(QmdFrontmatter, String), QmdError> {
    let is_delimiter = |line: &str| matches!(line, "---\n" | "---\r\n" | "---");
    let mut lines = content.split_inclusive('\n');
    let Some(opening_line) = lines.next().filter(|line| is_delimiter(line)) else {
        return Ok((QmdFrontmatter::default(), content.to_owned()));
    };
    let mut offset = opening_line.len();
    for line in lines {
        if is_delimiter(line) {
            let frontmatter = serde_saphyr::from_str(&content[opening_line.len()..offset])
                .map_err(|e| QmdError::ParseError(e.to_string()))?;
            // Consume exactly the delimiter line, including its LF or CRLF.
            // All subsequent body bytes belong to the document.
            return Ok((frontmatter, content[offset + line.len()..].to_owned()));
        }
        offset += line.len();
    }
    Err(QmdError::MissingFrontmatter)
}

#[must_use]
pub fn format_qmd_for_embedding(qmd: &QmdFile) -> String {
    let mut parts = Vec::new();

    if let Some(ref title) = qmd.frontmatter.title {
        parts.push(format!("# {title}"));
    }

    if !qmd.frontmatter.tags.is_empty() {
        parts.push(format!("Tags: {}", qmd.frontmatter.tags.join(", ")));
    }

    if let Some(ref category) = qmd.frontmatter.category {
        parts.push(format!("Category: {category}"));
    }

    parts.push(String::new());
    parts.push(qmd.content.clone());

    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qmd_delimiters_consume_the_whole_line_and_preserve_body_bytes() {
        let home = tempfile::tempdir().expect("temporary QMD directory");
        let manager = QmdManager::new(home.path().to_path_buf());
        for (index, newline) in ["\n", "\r\n"].into_iter().enumerate() {
            let body = format!("{newline}  中文{newline}---{newline}---word{newline}");
            let document = format!(
                "---{newline}title: example{newline}---word: literal{newline}---{newline}{body}"
            );
            let path = home.path().join(format!("line-endings-{index}.qmd"));
            std::fs::write(&path, document).expect("write QMD fixture");
            let loaded = manager.load_qmd_file(&path).expect("load QMD");
            assert_eq!(loaded.content, body);
            assert_eq!(loaded.frontmatter.extra["---word"], "literal");
            for _ in 0..3 {
                let updated = manager
                    .update_qmd_file(&path, None, None)
                    .expect("rewrite metadata without changing body");
                assert_eq!(updated.content, body);
            }
            let (_, empty_body) =
                parse_frontmatter(&format!("---{newline}title: example{newline}---"))
                    .expect("closing delimiter at end of file");
            assert_eq!(empty_body, "");
        }
    }

    #[test]
    fn qmd_without_an_opening_delimiter_preserves_all_content() {
        for content in ["", "---word\ntext\n", " \n---\ntext\n", "\r\n  text\r\n"] {
            let (frontmatter, body) = parse_frontmatter(content).expect("plain document");
            assert!(frontmatter.title.is_none());
            assert_eq!(body, content);
        }
    }

    #[test]
    fn qmd_requires_a_complete_closing_delimiter_line() {
        for content in ["---", "---\n", "---\r\n", "---\ntitle: example\n---word\n"] {
            assert!(matches!(
                parse_frontmatter(content),
                Err(QmdError::MissingFrontmatter)
            ));
        }
    }

    #[test]
    fn qmd_yaml_round_trip_preserves_strings_extensions_and_markdown_body() {
        let home = tempfile::tempdir().expect("temporary QMD directory");
        let manager = QmdManager::new(home.path().to_path_buf());
        let frontmatter = QmdFrontmatter {
            title: Some("on".to_owned()),
            tags: vec!["yes".to_owned(), "2026-10-30".to_owned(), "中文".to_owned()],
            extra: HashMap::from([
                ("numeric-looking".to_owned(), serde_json::json!("00123")),
                ("boolean-looking".to_owned(), serde_json::json!("off")),
                ("nested".to_owned(), serde_json::json!({"enabled": true})),
            ]),
            ..Default::default()
        };
        let expected = serde_json::to_value(&frontmatter).expect("frontmatter value");
        let body = "第一段\n---\nMarkdown divider stays in the body.\n";
        let created = manager
            .create_qmd_file("round-trip", frontmatter, body)
            .expect("create QMD");
        assert_eq!(
            serde_json::to_value(&created.frontmatter).expect("loaded value"),
            expected
        );
        assert_eq!(created.content, body);

        let mut changed = created.frontmatter;
        changed.pinned = true;
        changed.archived = true;
        manager
            .update_qmd_file(&created.path, Some(changed), Some("Updated 内容\n"))
            .expect("update QMD");
        let reloaded = QmdManager::new(home.path().to_path_buf())
            .load_qmd_file(&created.path)
            .expect("reopen QMD");
        assert!(reloaded.frontmatter.pinned);
        assert!(reloaded.frontmatter.archived);
        assert_eq!(reloaded.frontmatter.title.as_deref(), Some("on"));
        assert_eq!(reloaded.frontmatter.extra["numeric-looking"], "00123");
        assert_eq!(reloaded.frontmatter.extra["boolean-looking"], "off");
        assert_eq!(reloaded.content, "Updated 内容\n");
    }
}
