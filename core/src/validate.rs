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

