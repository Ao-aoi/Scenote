//! 参照ファイル（世界観・キャラクター設定・制作方針などの資料）。

use serde::{Deserialize, Serialize};

use crate::validate::Checked;

pub const MAX_FILE_CHARS: usize = 200_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReferenceFile {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub content: String,
    pub pinned: bool,
    pub revision: i64,
    pub created_at: String,
    pub updated_at: String,
}

pub fn clean_file_content(content: String) -> Checked<String> {
    if content.chars().count() > MAX_FILE_CHARS {
        return Err(format!("参照ファイルは{MAX_FILE_CHARS}文字以内にしてください"));
    }
    Ok(content.replace("\r\n", "\n").replace('\r', "\n"))
}
