//! 執筆嗜好の学習の、DB と通信に依存しない部分（型・検証・分析のプロンプトと結果の解釈）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::llm::extract_json;
use crate::validate::Checked;

pub const CATEGORIES: [&str; 9] = [
    "style", "vocabulary", "length", "avoid", "balance", "tempo", "character", "density", "other",
];

pub const SOURCE_USER: &str = "user";
pub const SOURCE_AI: &str = "ai";
pub const SOURCE_COPIED: &str = "copied";

pub const KEY_AUTO_UPDATE: &str = "preferences.auto_update";
pub const KEY_INCLUDE_DIRECT: &str = "preferences.include_direct_edits";

/// 1 回の分析で扱う操作の最大件数。
pub const MAX_DECISIONS: usize = 40;
/// プロンプトに含める嗜好ルールの最大件数（共通・プロジェクトそれぞれ）。
pub const MAX_RULES_IN_PROMPT: usize = 30;
pub const MAX_RULE_CHARS: usize = 300;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Rule {
    pub id: String,
    /// `None` は全プロジェクト共通。
    pub project_id: Option<String>,
    pub text: String,
    pub category: String,
    pub confidence: f64,
    /// 根拠となった提案の ID。
    pub evidence: Vec<String>,
    pub source: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Suggestion {
    pub id: String,
    pub project_id: String,
    /// add / update / remove
    pub op: String,
    pub rule_id: Option<String>,
    /// update / remove の対象ルールの、提案時点の文面。
    pub rule_text: Option<String>,
    pub text: String,
    pub category: String,
    pub confidence: f64,
    pub evidence: Vec<String>,
    pub reason: String,
    pub status: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PreferenceSettings {
    /// 分析結果（更新案）を承認なしでプロファイルに適用する。
    pub auto_update: bool,
    pub include_direct_edits: bool,
    /// 条件を満たしたら、分析そのものを自動で実行する（#10）。
    pub auto_analyze: bool,
}

/// 分析の実行状況（プロジェクトごと）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AnalysisStatus {
    /// idle / running / error
    pub state: String,
    /// 最後に成功した分析の日時（UTC）。
    pub last_success_at: Option<String>,
    /// 最後に失敗した分析の日時と理由。失敗しても以前のルールは残る。
    pub last_error_at: Option<String>,
    pub last_error: Option<String>,
    /// 最後の実行のきっかけ（manual / auto）。
    pub last_trigger: Option<String>,
}

/// 自動分析を始める、未分析の操作の件数。
pub const AUTO_MIN_DECISIONS: i64 = 5;
/// 最後の操作からこの時間（秒）が経つまでは自動分析しない（操作が続いている間にまとめるため）。
pub const AUTO_IDLE_SECONDS: i64 = 10 * 60;
/// 自動分析どうしの最小間隔（秒）。
pub const AUTO_MIN_INTERVAL_SECONDS: i64 = 60 * 60;
pub const KEY_AUTO_ANALYZE: &str = "preferences.auto_analyze";
pub const STATUS_PREFIX: &str = "preferences.status.";

#[derive(Debug, Clone, Serialize)]
pub struct Profile {
    pub global_rules: Vec<Rule>,
    pub project_rules: Vec<Rule>,
    pub suggestions: Vec<Suggestion>,
    pub settings: PreferenceSettings,
    /// まだ分析していない、承認・却下済みの提案の件数。
    pub unanalyzed: i64,
    pub status: AnalysisStatus,
}

#[derive(Debug, Serialize)]
pub struct AnalysisResult {
    pub summary: String,
    /// 自動更新 ON のときは適用済み、OFF のときは承認待ちの更新案。
    pub suggestions: Vec<Suggestion>,
    pub applied: bool,
    pub warnings: Vec<String>,
    pub profile: Profile,
}

/// 根拠の件数から確信度を決める。1 件では 0.35 にとどめ、件数が増えるほど上がる（上限 0.9）。
pub fn confidence_for(evidence_count: usize) -> f64 {
    let c = 0.2 + 0.15 * evidence_count as f64;
    (c.min(0.9) * 100.0).round() / 100.0
}


pub fn clean_rule_text(text: &str) -> Checked<String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("ルールの内容を入力してください".into());
    }
    if text.chars().count() > MAX_RULE_CHARS {
        return Err(format!("ルールは{MAX_RULE_CHARS}文字以内で入力してください"));
    }
    Ok(text.replace(['\r', '\n'], " "))
}

pub fn clean_category(category: &str) -> Checked<String> {
    if CATEGORIES.contains(&category) {
        Ok(category.to_string())
    } else {
        Err(format!("未知の分類です: {category}"))
    }
}

pub fn clean_confidence(c: f64) -> Checked<f64> {
    if !(0.0..=1.0).contains(&c) || c.is_nan() {
        return Err("確信度は 0〜1 で指定してください".into());
    }
    Ok(c)
}

/// 分析に渡す 1 件の操作。
pub struct Decision {
    /// 根拠として記録する ID（提案の ID、または履歴の `h{seq}`）。なければ件数にだけ数える。
    pub proposal_id: Option<String>,
    pub text: String,
}

pub const ANALYSIS_PROMPT: &str = r#"あなたはゲームシナリオ制作アプリの補助機能です。ユーザーが AI の提案をどう扱ったか（承認・修正して採用・却下）から、ユーザーの執筆上の好みを読み取り、嗜好プロファイルの更新案を作ります。

## 原則
- 少数の操作から普遍的な好みと断定しないでください。1 件だけの却下を「禁止事項」にしないでください。
- 根拠のない推測でルールを作らないでください。各案には根拠となる操作の参照名（d1 など）を必ず挙げます。
- 既存のルールと重複する案は作らず、補強する場合は update を使います。矛盾する根拠が出た場合は update で表現を弱めるか、remove を提案します。
- 修正して採用した操作では、AI の提案と最終的な内容の違いが最も重要な手がかりです。
- ルールは、文章生成への具体的な指示として使える短い一文にします（例: 「台詞の語尾に『〜わ』を多用しない」）。
- 分類（category）は style（文体）, vocabulary（語彙）, length（文章の長さ）, avoid（避けたい表現）, balance（台詞と地の文の配分）, tempo（展開のテンポ）, character（キャラクターの話し方）, density（演出・説明の密度）, other のいずれかです。
- 読み取れる傾向がなければ changes は空にします。

## 出力形式
次の JSON オブジェクトだけを出力します。
{
  "summary": "分析の要約（日本語、2〜3 文）",
  "changes": [
    {"op": "add", "text": "ルール", "category": "style", "evidence": ["d1", "d3"], "reason": "理由"},
    {"op": "update", "target": "r2", "text": "更新後のルール", "category": "tempo", "evidence": ["d4"], "reason": "理由"},
    {"op": "remove", "target": "r3", "evidence": ["d5"], "reason": "理由"}
  ]
}"#;

pub fn describe(value: &Option<Value>) -> String {
    match value {
        None => "（なし）".to_string(),
        Some(v) => {
            let text = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("");
            if v.get("title").is_some() && v.get("body").is_some() {
                format!("[場面「{}」の概要] {}", text("title"), text("body"))
            } else if v.get("kind").is_some() {
                let speaker = text("speaker");
                let head = if speaker.is_empty() { text("kind").to_string() } else { format!("{}・{speaker}", text("kind")) };
                format!("[{head}] {}", text("body"))
            } else {
                format!("[ファイル「{}」] {}", text("name"), text("content"))
            }
        }
    }
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

/// 検証済みの更新案（保存前）。
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedChange {
    pub op: &'static str,
    pub rule_id: Option<String>,
    pub text: String,
    pub category: String,
    pub evidence: Vec<String>,
    pub reason: String,
    pub confidence: f64,
}

/// 分析結果を検証し、更新案にする。不正な案は除外して注意に残す。
pub fn interpret_analysis(
    text: &str,
    decisions: &[Decision],
    rules: &[Rule],
) -> std::result::Result<(String, Vec<PlannedChange>, Vec<String>), String> {
    let value = extract_json(text).ok_or("分析結果を解釈できませんでした（JSON ではありません）")?;
    let summary = value.get("summary").and_then(Value::as_str).unwrap_or("").trim().to_string();
    let mut warnings = Vec::new();
    let mut out = Vec::new();
    let changes = value.get("changes").and_then(Value::as_array).cloned().unwrap_or_default();
    for (i, c) in changes.iter().enumerate() {
        let n = i + 1;
        let get = |k: &str| c.get(k).and_then(Value::as_str).map(str::trim);
        let op = get("op").unwrap_or("");
        // 根拠は提案に由来するもの（d 参照）のみを記録する。直接編集は根拠の件数にだけ数える。
        let mut evidence_count = 0;
        let mut evidence = Vec::new();
        for r in c.get("evidence").and_then(Value::as_array).into_iter().flatten() {
            let Some(index) = r.as_str().and_then(|s| s.strip_prefix('d')).and_then(|s| s.parse::<usize>().ok()) else {
                continue;
            };
            if let Some(d) = index.checked_sub(1).and_then(|i| decisions.get(i)) {
                evidence_count += 1;
                if let Some(id) = &d.proposal_id {
                    if !evidence.contains(id) {
                        evidence.push(id.clone());
                    }
                }
            }
        }
        if evidence_count == 0 {
            warnings.push(format!("案 {n} は根拠となる操作が示されていないため除外しました"));
            continue;
        }
        let target = match get("target") {
            Some(t) => match t.strip_prefix('r').and_then(|s| s.parse::<usize>().ok()).and_then(|i| i.checked_sub(1)).and_then(|i| rules.get(i)) {
                Some(rule) => Some(rule),
                None => {
                    warnings.push(format!("案 {n} は存在しないルールを指しているため除外しました"));
                    continue;
                }
            },
            None => None,
        };
        let reason = get("reason").unwrap_or("").to_string();
        match op {
            "add" | "update" => {
                let Some(text) = get("text").and_then(|t| clean_rule_text(t).ok()) else {
                    warnings.push(format!("案 {n} はルールの文面が不正なため除外しました"));
                    continue;
                };
                let category = get("category").filter(|c| CATEGORIES.contains(c)).unwrap_or("other").to_string();
                if op == "update" {
                    let Some(rule) = target else {
                        warnings.push(format!("案 {n} は更新対象のルールがないため除外しました"));
                        continue;
                    };
                    let total = rule.evidence.len().max(1) + evidence_count;
                    out.push(PlannedChange { op: "update", rule_id: Some(rule.id.clone()), text, category, evidence, reason, confidence: confidence_for(total) });
                } else {
                    if rules.iter().any(|r| r.text == text) {
                        warnings.push(format!("案 {n} は既存のルールと同じため除外しました"));
                        continue;
                    }
                    out.push(PlannedChange { op: "add", rule_id: None, text, category, evidence, reason, confidence: confidence_for(evidence_count) });
                }
            }
            "remove" => {
                let Some(rule) = target else {
                    warnings.push(format!("案 {n} は削除対象のルールがないため除外しました"));
                    continue;
                };
                out.push(PlannedChange { op: "remove", rule_id: Some(rule.id.clone()), text: rule.text.clone(), category: rule.category.clone(), evidence, reason, confidence: rule.confidence });
            }
            other => warnings.push(format!("案 {n} は未対応の操作（{other}）のため除外しました")),
        }
    }
    Ok((summary, out, warnings))
}


/// 依頼のプロンプトに含める嗜好の説明。ルールがなければ空文字。
pub fn preference_prompt_text(project_rules: &[Rule], all_global: Vec<Rule>) -> String {
    let section = |title: &str, rules: &[Rule]| -> String {
        if rules.is_empty() {
            return String::new();
        }
        let mut out = format!("## {title}\n");
        for r in rules.iter().take(MAX_RULES_IN_PROMPT) {
            let strength = if r.confidence >= 0.7 {
                "強い傾向"
            } else if r.confidence >= 0.45 {
                "傾向"
            } else {
                "弱い傾向（参考程度）"
            };
            out.push_str(&format!("- {}（{strength}）\n", r.text));
        }
        out
    };
    // 共通へ複製したルールなど、プロジェクトのルールと同じ文面のものは重ねて伝えない。
    let global_rules: Vec<Rule> = all_global
        .into_iter()
        .filter(|g| !project_rules.iter().any(|p| p.text == g.text))
        .collect();
    let global = section("共通の嗜好", &global_rules);
    let project = section("このプロジェクトでの嗜好", project_rules);
    if global.is_empty() && project.is_empty() {
        return String::new();
    }
    format!(
        "# ユーザーの執筆嗜好\n過去の提案への反応から学習した傾向です。共通の嗜好よりプロジェクトの嗜好を、\
         それよりチャットの追加指示とユーザーの依頼を優先してください。\n{global}{project}"
    )
}

/// 分析の要求の本文。
pub fn analysis_prompt(rules: &[Rule], global_rules: &[Rule], decisions: &[Decision]) -> String {
    let mut prompt = String::from("# 既存のルール（このプロジェクト）\n");
    if rules.is_empty() {
        prompt.push_str("（なし）\n");
    }
    for (i, r) in rules.iter().enumerate() {
        prompt.push_str(&format!("- [r{}] {}（分類: {}、確信度: {:.2}）\n", i + 1, r.text, r.category, r.confidence));
    }
    if !global_rules.is_empty() {
        prompt.push_str("\n# 共通のルール（参考。変更対象にはしない）\n");
        for r in global_rules {
            prompt.push_str(&format!("- {}\n", r.text));
        }
    }
    prompt.push_str("\n# ユーザーの操作\n");
    for (i, d) in decisions.iter().enumerate() {
        prompt.push_str(&format!("## [d{}]\n{}\n\n", i + 1, d.text));
    }
    prompt
}
