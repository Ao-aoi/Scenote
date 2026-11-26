//! キャラクター（`docs/character-model.md`）の型と入力の検証。PC 版と Cloudflare 版で共有する。

use serde::{Deserialize, Serialize};

use crate::validate::{clean_title, Checked};

/// 情報の由来。
pub const SOURCES: [&str; 3] = ["user", "confirmed", "inferred"];
/// 情報の状態。
pub const STATUSES: [&str; 3] = ["active", "proposed", "rejected"];

const MAX_TEXT_CHARS: usize = 2000;
const MAX_CATEGORY_CHARS: usize = 50;
const MAX_ALIASES: usize = 20;
pub const MAX_EVIDENCE: usize = 50;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Character {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateCharacter {
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateCharacter {
    pub name: Option<String>,
    pub aliases: Option<Vec<String>>,
}

/// 人物についての 1 件の情報（プロフィール・性格・口調など）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CharacterFact {
    pub id: String,
    pub character_id: String,
    pub category: String,
    pub text: String,
    pub source: String,
    pub confidence: Option<f64>,
    /// 根拠となる要素の ID。
    pub evidence: Vec<String>,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

/// 2 人の関係（方向つき）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CharacterRelation {
    pub id: String,
    pub project_id: String,
    pub from_character_id: String,
    pub to_character_id: String,
    pub kind: String,
    pub text: String,
    pub source: String,
    pub confidence: Option<f64>,
    pub evidence: Vec<String>,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

/// 情報・関係の作成と更新で共通の入力。更新では省略した項目を変えない。
#[derive(Debug, Default, Deserialize)]
pub struct InfoInput {
    pub category: Option<String>,
    pub kind: Option<String>,
    pub text: Option<String>,
    pub source: Option<String>,
    /// `null` で確信度を消す。
    #[serde(default, deserialize_with = "double_option")]
    pub confidence: Option<Option<f64>>,
    pub evidence: Option<Vec<String>>,
    pub status: Option<String>,
    /// 関係の作成時のみ。
    pub from_character_id: Option<String>,
    pub to_character_id: Option<String>,
}

/// 「省略」と「null」を区別して受け取る。
pub fn double_option<'de, D, T>(d: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

pub fn json_list(text: String) -> Vec<String> {
    serde_json::from_str(&text).unwrap_or_default()
}


pub fn clean_aliases(aliases: Vec<String>, name: &str) -> Checked<Vec<String>> {
    if aliases.len() > MAX_ALIASES {
        return Err(format!("別名は{MAX_ALIASES}個までです"));
    }
    let mut result = Vec::new();
    for alias in aliases {
        let alias = clean_title("別名", &alias)?;
        if alias != name && !result.contains(&alias) {
            result.push(alias);
        }
    }
    Ok(result)
}

pub fn clean_label(what: &str, value: &str) -> Checked<String> {
    let v = value.trim();
    if v.is_empty() {
        return Err(format!("{what}を入力してください"));
    }
    if v.chars().count() > MAX_CATEGORY_CHARS || v.chars().any(char::is_control) {
        return Err(format!("{what}は改行を含まない{MAX_CATEGORY_CHARS}文字以内にしてください"));
    }
    Ok(v.to_string())
}

pub fn clean_text(value: &str, required: bool) -> Checked<String> {
    let v = value.trim();
    if required && v.is_empty() {
        return Err("内容を入力してください".into());
    }
    if v.chars().count() > MAX_TEXT_CHARS {
        return Err(format!("内容は{MAX_TEXT_CHARS}文字以内にしてください"));
    }
    Ok(v.replace("\r\n", "\n"))
}

pub fn clean_choice(what: &str, value: &str, allowed: &[&str]) -> Checked<String> {
    if allowed.contains(&value) {
        Ok(value.to_string())
    } else {
        Err(format!("{what}は {} のいずれかで指定してください", allowed.join("・")))
    }
}

/// 由来と確信度の組み合わせを検証する。AI の推定（inferred）には確信度が必要。
pub fn check_confidence(source: &str, confidence: Option<f64>) -> Checked<()> {
    if let Some(c) = confidence {
        if !(0.0..=1.0).contains(&c) || c.is_nan() {
            return Err("確信度は 0〜1 で指定してください".into());
        }
    }
    if source == "inferred" && confidence.is_none() {
        return Err("AI の推定（inferred）には確信度を指定してください".into());
    }
    Ok(())
}

