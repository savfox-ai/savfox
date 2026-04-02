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
    pub extra: HashMap<String, serde_yaml::Value>,
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

        files.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

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

        let yaml =
            serde_yaml::to_string(&frontmatter).map_err(|e| QmdError::ParseError(e.to_string()))?;

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

        let yaml = serde_yaml::to_string(&qmd.frontmatter)
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
    let content = content.trim_start();

    if !content.starts_with("---") {
        return Ok((QmdFrontmatter::default(), content.to_owned()));
    }

    let end_marker = content[3..]
        .find("\n---")
        .ok_or(QmdError::MissingFrontmatter)?;

    let frontmatter_str = &content[4..end_marker + 3];
    let body = &content[end_marker + 7..];

    let frontmatter: QmdFrontmatter =
        serde_yaml::from_str(frontmatter_str).map_err(|e| QmdError::ParseError(e.to_string()))?;

    Ok((frontmatter, body.to_owned()))
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
