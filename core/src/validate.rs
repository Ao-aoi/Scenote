//! 入力の検証・正規化。誤りは利用者に見せる文言（日本語）で返す。

use crate::model::{is_valid_kind, is_valid_speaker, Element, ElementContent, ORIGIN_HUMAN, ORIGIN_MIXED, TEMPLATES};

pub const MAX_TITLE_CHARS: usize = 200;
pub const MAX_SUMMARY_CHARS: usize = 2000;

pub type Checked<T> = std::result::Result<T, String>;

pub fn clean_title(what: &str, title: &str) -> Checked<String> {
    let title = title.trim();
    if title.is_empty() {
        return Err(format!("{what}を入力してください"));
    }
    if title.chars().count() > MAX_TITLE_CHARS {
        return Err(format!("{what}は{MAX_TITLE_CHARS}文字以内で入力してください"));
    }
    if title.chars().any(|c| c.is_control()) {
        return Err(format!("{what}に改行などの制御文字は使えません"));
    }
    Ok(title.to_string())
}

pub fn clean_templates(templates: &[String]) -> Checked<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for t in templates {
        if !TEMPLATES.contains(&t.as_str()) {
            return Err(format!("未知のテンプレートです: {t}"));
        }
        if !out.contains(t) {
            out.push(t.clone());
        }
    }
    Ok(out)
}

pub fn clean_content(content: ElementContent) -> Checked<ElementContent> {
    if !is_valid_kind(&content.kind) {
        return Err("要素の種類は英字で始まる英数字で指定してください".into());
    }
    if !is_valid_speaker(&content.speaker) {
        return Err("キャラクター名に [ ] : * と改行は使えません（100文字以内）".into());
    }
    Ok(ElementContent {
        body: content.body.replace("\r\n", "\n").replace('\r', "\n"),
        ..content
    })
}

pub fn clean_summary(summary: &str) -> Checked<String> {
    let s = summary.trim().replace("\r\n", "\n");
    if s.chars().count() > MAX_SUMMARY_CHARS {
        return Err(format!("場面の概要は{MAX_SUMMARY_CHARS}文字以内にしてください"));
    }
    Ok(s)
}

/// 人間が直接編集した後の由来。AI 由来の要素は「混在」になる。
pub fn origin_after_human_edit(current: &str) -> &'static str {
    if current == ORIGIN_HUMAN {
        ORIGIN_HUMAN
    } else {
        ORIGIN_MIXED
    }
}

pub fn content_of(e: &Element) -> ElementContent {
    ElementContent {
        kind: e.kind.clone(),
        speaker: e.speaker.clone(),
        body: e.body.clone(),
    }
}

/// 並べ替えの指定が、現在の子の ID の並べ替えになっているか。
pub fn is_permutation(existing: &[String], requested: &[String]) -> bool {
    let set: std::collections::HashSet<&String> = requested.iter().collect();
    requested.len() == existing.len() && set.len() == requested.len() && existing.iter().all(|id| set.contains(id))
}

pub const ORDER_MISMATCH: &str = "並べ替えの対象が最新の状態と一致しません。再読み込みしてからやり直してください";

/// 新しい要素列の各要素を、既存要素のどれに対応させるか決める。
///
/// 内容が完全に一致する要素を最長共通部分列で対応づけ、その間に挟まれた区間では
/// 先頭から順に「同じ要素の編集」とみなして対応づける。対応のない新要素は `None`（新規作成）。
pub fn align(old: &[ElementContent], new: &[ElementContent]) -> Vec<Option<usize>> {
    let (n, m) = (old.len(), new.len());
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if old[i] == new[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut plan = vec![None; m];
    let (mut i, mut j) = (0, 0);
    let (mut gap_i, mut gap_j) = (0, 0);
    let pair_gap = |plan: &mut Vec<Option<usize>>, from_i: usize, to_i: usize, from_j: usize, to_j: usize| {
        for (oi, nj) in (from_i..to_i).zip(from_j..to_j) {
            plan[nj] = Some(oi);
        }
    };
    while i < n && j < m {
        if old[i] == new[j] {
            pair_gap(&mut plan, gap_i, i, gap_j, j);
            plan[j] = Some(i);
            i += 1;
            j += 1;
            gap_i = i;
            gap_j = j;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    pair_gap(&mut plan, gap_i, n, gap_j, m);
    plan
}


pub const MAX_OPTIONS: usize = 20;
pub const MAX_EXPR_CHARS: usize = 500;
pub const MAX_ATTRS_BYTES: usize = 20_000;

fn clean_expr(v: Option<&serde_json::Value>, what: &str) -> Checked<String> {
    use serde_json::Value;
    let s = match v {
        None | Some(Value::Null) => "",
        Some(Value::String(s)) => s.as_str(),
        _ => return Err(format!("{what}は文字列で指定してください")),
    };
    if s.chars().count() > MAX_EXPR_CHARS {
        return Err(format!("{what}は{MAX_EXPR_CHARS}文字以内にしてください"));
    }
    Ok(s.trim().to_string())
}

/// 要素の追加情報（分岐の設定）を検証して正規化する。式の誤りは保存を妨げない（構造チェックで知らせる）。
///
/// `scene_exists` は、移動先の場面 ID が同じプロジェクトに存在するかを返す。
pub fn clean_attrs(
    kind: &str,
    attrs: serde_json::Value,
    mut scene_exists: impl FnMut(&str) -> bool,
) -> Checked<serde_json::Value> {
    use crate::model::{KIND_CHOICE, KIND_TRANSITION, KIND_VARIABLE_CHANGE};
    use serde_json::{json, Value};
    if !attrs.is_object() {
        return Err("要素の追加情報はオブジェクトで指定してください".into());
    }
    if attrs.to_string().len() > MAX_ATTRS_BYTES {
        return Err("要素の追加情報が大きすぎます".into());
    }
    let mut target = |v: Option<&Value>| -> Checked<Option<String>> {
        match v {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) if s.is_empty() => Ok(None),
            Some(Value::String(s)) => {
                if scene_exists(s) {
                    Ok(Some(s.clone()))
                } else {
                    Err("移動先の場面が見つかりません".into())
                }
            }
            _ => Err("移動先は場面の ID で指定してください".into()),
        }
    };
    Ok(match kind {
        KIND_CHOICE => {
            let options = attrs.get("options").and_then(Value::as_array).cloned().unwrap_or_default();
            if options.len() > MAX_OPTIONS {
                return Err(format!("選択肢は{MAX_OPTIONS}個までです"));
            }
            let mut out = Vec::new();
            for o in &options {
                let text = clean_expr(o.get("text"), "選択肢の文")?;
                out.push(json!({
                    "text": text,
                    "target": target(o.get("target"))?,
                    "condition": clean_expr(o.get("condition"), "条件式")?,
                    "effects": clean_expr(o.get("effects"), "変更式")?,
                }));
            }
            json!({ "options": out })
        }
        KIND_TRANSITION => json!({
            "target": target(attrs.get("target"))?,
            "condition": clean_expr(attrs.get("condition"), "条件式")?,
        }),
        KIND_VARIABLE_CHANGE => json!({ "effects": clean_expr(attrs.get("effects"), "変更式")? }),
        _ => attrs,
    })
}
