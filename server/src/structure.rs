//! 変数・分岐の構造データと、その可視化・検査。
//!
//! - 変数: プロジェクトごとに名前・型・初期値を持つ。
//! - 分岐: 選択肢（`Choice`）の各項目と遷移（`Transition`）が、移動先の場面・条件式・変更式を `attrs` に持つ。
//! - 場面グラフ・キャラクター相関・構造チェックは、保存されたデータからプログラムが生成する（LLM は使わない）。

use std::collections::{HashMap, HashSet, VecDeque};

use rusqlite::{params, OptionalExtension, Row};
use serde::Serialize;
use serde_json::{json, Value};

use crate::error::{AppError, Result};
use crate::logic::{self, check_condition, check_effects, VarType};
use crate::model::*;
use crate::store::{new_id, Store, NOW};

const MAX_OPTIONS: usize = 20;
const MAX_EXPR_CHARS: usize = 500;
const MAX_ATTRS_BYTES: usize = 20_000;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Variable {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub var_type: VarType,
    pub initial: String,
    pub description: String,
}

fn variable_from_row(row: &Row) -> rusqlite::Result<Variable> {
    let t: String = row.get("var_type")?;
    Ok(Variable {
        id: row.get("id")?,
        project_id: row.get("project_id")?,
        name: row.get("name")?,
        var_type: VarType::parse(&t).unwrap_or(VarType::Number),
        initial: row.get("initial")?,
        description: row.get("description")?,
    })
}

/// 場面グラフの頂点。
#[derive(Debug, Clone, Serialize)]
pub struct GraphNode {
    pub id: String,
    pub title: String,
    pub chapter_id: String,
    pub chapter_title: String,
    /// プロジェクト全体での順番（0 から）。
    pub order: usize,
    pub is_start: bool,
    /// 出ていく辺がない（物語の終わり）。
    pub is_end: bool,
    pub reachable: bool,
    pub element_count: usize,
    pub choice_count: usize,
}

/// 場面グラフの辺。
#[derive(Debug, Clone, Serialize)]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
    /// next（場面の順に進む） / choice（選択肢） / transition（遷移）
    pub kind: &'static str,
    pub label: String,
    pub condition: String,
    pub effects: String,
    /// 辺の元になった要素。
    pub element_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CharacterNode {
    pub name: String,
    /// 台詞・動作の件数。
    pub lines: usize,
    /// 登場する場面の数。
    pub scenes: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct CharacterEdge {
    pub a: String,
    pub b: String,
    /// 同じ場面に登場する回数。
    pub shared_scenes: usize,
}

/// 構造チェックで見つかった問題。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Issue {
    /// error / warning / info
    pub severity: &'static str,
    pub message: String,
    pub scene_id: Option<String>,
    pub element_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Structure {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub characters: Vec<CharacterNode>,
    pub relations: Vec<CharacterEdge>,
    pub variables: Vec<Variable>,
    /// 変数ごとの、参照・変更している場面の数。
    pub variable_usage: Vec<VariableUsage>,
    pub issues: Vec<Issue>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VariableUsage {
    pub name: String,
    pub read_in: Vec<String>,
    pub written_in: Vec<String>,
}

fn text_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn clean_expr(v: Option<&Value>, what: &str) -> Result<String> {
    let s = match v {
        None | Some(Value::Null) => "",
        Some(Value::String(s)) => s.as_str(),
        _ => return Err(AppError::Invalid(format!("{what}は文字列で指定してください"))),
    };
    if s.chars().count() > MAX_EXPR_CHARS {
        return Err(AppError::Invalid(format!("{what}は{MAX_EXPR_CHARS}文字以内にしてください")));
    }
    Ok(s.trim().to_string())
}

impl Store {
    // ---- 変数 ---------------------------------------------------------

    pub fn list_variables(&self, project_id: &str) -> Result<Vec<Variable>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM variables WHERE project_id = ?1 ORDER BY name")?;
        let rows = stmt.query_map([project_id], variable_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get_variable(&self, id: &str) -> Result<Variable> {
        self.conn
            .query_row("SELECT * FROM variables WHERE id = ?1", [id], variable_from_row)
            .optional()?
            .ok_or(AppError::NotFound("変数"))
    }

    fn check_variable(&self, project_id: &str, name: &str, var_type: &str, initial: &str, exclude: Option<&str>) -> Result<(String, VarType, String)> {
        let name = name.trim();
        if !logic::is_valid_name(name) {
            return Err(AppError::Invalid(
                "変数名は文字（英字・日本語）か _ で始まり、文字・数字・_ だけを使った50文字以内にしてください（and・or・not・true・false は使えません）".into(),
            ));
        }
        let t = VarType::parse(var_type).ok_or_else(|| AppError::Invalid("変数の型は number・bool・text のいずれかです".into()))?;
        let initial = initial.trim();
        if !initial.is_empty() {
            let problems = check_effects(&format!("{name} = {initial}"), &HashMap::from([(name.to_string(), t)]));
            if let Some(p) = problems.first() {
                return Err(AppError::Invalid(format!("初期値が正しくありません: {p}")));
            }
        }
        let dup: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM variables WHERE project_id = ?1 AND name = ?2 AND id IS NOT ?3",
                params![project_id, name, exclude],
                |r| r.get(0),
            )
            .optional()?;
        if dup.is_some() {
            return Err(AppError::Invalid(format!("変数「{name}」はすでにあります")));
        }
        Ok((name.to_string(), t, initial.to_string()))
    }

    pub fn create_variable(&mut self, project_id: &str, name: &str, var_type: &str, initial: &str, description: &str) -> Result<Variable> {
        self.get_project(project_id)?;
        let (name, t, initial) = self.check_variable(project_id, name, var_type, initial, None)?;
        let id = new_id();
        self.conn.execute(
            &format!(
                "INSERT INTO variables (id, project_id, name, var_type, initial, description, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, {NOW}, {NOW})"
            ),
            params![id, project_id, name, serde_json::to_value(t)?.as_str(), initial, description.trim()],
        )?;
        self.get_variable(&id)
    }

    pub fn update_variable(&mut self, id: &str, name: Option<&str>, var_type: Option<&str>, initial: Option<&str>, description: Option<&str>) -> Result<Variable> {
        let v = self.get_variable(id)?;
        let current_type = serde_json::to_value(v.var_type)?;
        let (name, t, initial) = self.check_variable(
            &v.project_id,
            name.unwrap_or(&v.name),
            var_type.unwrap_or(current_type.as_str().unwrap_or("number")),
            initial.unwrap_or(&v.initial),
            Some(id),
        )?;
        self.conn.execute(
            &format!(
                "UPDATE variables SET name = ?2, var_type = ?3, initial = ?4, description = ?5, updated_at = {NOW} WHERE id = ?1"
            ),
            params![id, name, serde_json::to_value(t)?.as_str(), initial, description.map(str::trim).unwrap_or(&v.description)],
        )?;
        self.get_variable(id)
    }

    pub fn delete_variable(&mut self, id: &str) -> Result<()> {
        let n = self.conn.execute("DELETE FROM variables WHERE id = ?1", [id])?;
        if n == 0 {
            return Err(AppError::NotFound("変数"));
        }
        Ok(())
    }

    // ---- 分岐の構造データ ---------------------------------------------

    /// 要素の種類に応じて `attrs` の形を検証・正規化する。式の中身の誤りは保存を妨げず、構造チェックで知らせる。
    pub(crate) fn clean_attrs(&self, scene_id: &str, kind: &str, attrs: Value) -> Result<Value> {
        if !attrs.is_object() {
            return Err(AppError::Invalid("要素の追加情報はオブジェクトで指定してください".into()));
        }
        if attrs.to_string().len() > MAX_ATTRS_BYTES {
            return Err(AppError::Invalid("要素の追加情報が大きすぎます".into()));
        }
        let project_id: String = self.conn.query_row(
            "SELECT c.project_id FROM scenes s JOIN chapters c ON c.id = s.chapter_id WHERE s.id = ?1",
            [scene_id],
            |r| r.get(0),
        )?;
        let target = |v: Option<&Value>| -> Result<Option<String>> {
            match v {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(s)) if s.is_empty() => Ok(None),
                Some(Value::String(s)) => {
                    let ok: Option<String> = self
                        .conn
                        .query_row(
                            "SELECT s.id FROM scenes s JOIN chapters c ON c.id = s.chapter_id WHERE s.id = ?1 AND c.project_id = ?2",
                            params![s, project_id],
                            |r| r.get(0),
                        )
                        .optional()?;
                    ok.map(Some).ok_or_else(|| AppError::Invalid("移動先の場面が見つかりません".into()))
                }
                _ => Err(AppError::Invalid("移動先は場面の ID で指定してください".into())),
            }
        };
        Ok(match kind {
            KIND_CHOICE => {
                let options = attrs.get("options").and_then(Value::as_array).cloned().unwrap_or_default();
                if options.len() > MAX_OPTIONS {
                    return Err(AppError::Invalid(format!("選択肢は{MAX_OPTIONS}個までです")));
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

    // ---- 可視化・構造チェック ------------------------------------------

    /// 場面グラフ・キャラクター相関・変数の使用状況・構造上の問題をまとめて生成する。
    pub fn structure(&self, project_id: &str) -> Result<Structure> {
        let tree = self.project_tree(project_id)?;
        let variables = self.list_variables(project_id)?;
        let types: HashMap<String, VarType> = variables.iter().map(|v| (v.name.clone(), v.var_type)).collect();
        let mut issues: Vec<Issue> = Vec::new();
        let issue = |issues: &mut Vec<Issue>, severity: &'static str, message: String, scene: Option<&str>, element: Option<&str>| {
            issues.push(Issue { severity, message, scene_id: scene.map(str::to_string), element_id: element.map(str::to_string) });
        };

        // 場面をプロジェクト全体の順に並べる。
        let mut scenes = Vec::new();
        for node in &tree.chapters {
            for s in &node.scenes {
                scenes.push((node.chapter.clone(), s.clone(), self.scene_detail(&s.id)?.elements));
            }
        }
        let scene_ids: HashSet<&str> = scenes.iter().map(|(_, s, _)| s.id.as_str()).collect();
        let title_of: HashMap<&str, &str> = scenes.iter().map(|(_, s, _)| (s.id.as_str(), s.title.as_str())).collect();

        let mut edges: Vec<GraphEdge> = Vec::new();
        let mut reads: HashMap<String, Vec<String>> = HashMap::new();
        let mut writes: HashMap<String, Vec<String>> = HashMap::new();
        let note = |map: &mut HashMap<String, Vec<String>>, var: String, scene: &str| {
            let list = map.entry(var).or_default();
            if !list.iter().any(|s| s == scene) {
                list.push(scene.to_string());
            }
        };
        let mut check_cond = |issues: &mut Vec<Issue>, src: &str, scene: &str, element: &str, what: &str| {
            for p in check_condition(src, &types) {
                issue(issues, "error", format!("{what}: {p}"), Some(scene), Some(element));
            }
            if let Ok(Some(e)) = logic::parse_condition(src) {
                let mut names = Vec::new();
                logic::variables_in(&e, &mut names);
                for n in names {
                    note(&mut reads, n, scene);
                }
            }
        };
        let mut effects_of: Vec<(String, String, String, String)> = Vec::new();

        let mut ends_flow: HashSet<String> = HashSet::new();
        for (_, scene, elements) in &scenes {
            let sid = scene.id.as_str();
            for (i, e) in elements.iter().enumerate() {
                match e.kind.as_str() {
                    KIND_CHOICE => {
                        let options = e.attrs.get("options").and_then(Value::as_array).cloned().unwrap_or_default();
                        if options.is_empty() {
                            issue(&mut issues, "info", "選択肢の項目（移動先・条件）がまだ設定されていません".into(), Some(sid), Some(&e.id));
                        }
                        let mut all_targeted = !options.is_empty();
                        for (n, o) in options.iter().enumerate() {
                            let label = text_of(o, "text");
                            let what = format!("選択肢{}「{}」", n + 1, if label.is_empty() { "（文なし）" } else { label });
                            if label.is_empty() {
                                issue(&mut issues, "warning", format!("{what}: 選択肢の文が空です"), Some(sid), Some(&e.id));
                            }
                            check_cond(&mut issues, text_of(o, "condition"), sid, &e.id, &what);
                            effects_of.push((text_of(o, "effects").to_string(), sid.to_string(), e.id.clone(), what.clone()));
                            match o.get("target").and_then(Value::as_str) {
                                Some(t) if scene_ids.contains(t) => edges.push(GraphEdge {
                                    from: sid.into(),
                                    to: t.into(),
                                    kind: "choice",
                                    label: label.to_string(),
                                    condition: text_of(o, "condition").into(),
                                    effects: text_of(o, "effects").into(),
                                    element_id: Some(e.id.clone()),
                                }),
                                Some(_) => {
                                    all_targeted = false;
                                    issue(&mut issues, "error", format!("{what}: 移動先の場面が削除されています"), Some(sid), Some(&e.id));
                                }
                                None => all_targeted = false,
                            }
                        }
                        // すべての項目に移動先がある選択肢は、場面の流れをそこで分ける。
                        if all_targeted {
                            ends_flow.insert(sid.to_string());
                        }
                    }
                    KIND_TRANSITION => {
                        let condition = text_of(&e.attrs, "condition");
                        check_cond(&mut issues, condition, sid, &e.id, "遷移の条件");
                        match e.attrs.get("target").and_then(Value::as_str) {
                            Some(t) if scene_ids.contains(t) => {
                                edges.push(GraphEdge {
                                    from: sid.into(),
                                    to: t.into(),
                                    kind: "transition",
                                    label: if condition.is_empty() { String::new() } else { format!("{condition} のとき") },
                                    condition: condition.into(),
                                    effects: String::new(),
                                    element_id: Some(e.id.clone()),
                                });
                                // 条件のない遷移が場面の最後にあれば、次の場面へは進まない。
                                if condition.is_empty() && i == elements.len() - 1 {
                                    ends_flow.insert(sid.to_string());
                                }
                            }
                            Some(_) => issue(&mut issues, "error", "遷移先の場面が削除されています".into(), Some(sid), Some(&e.id)),
                            None => issue(&mut issues, "warning", "遷移先の場面が設定されていません".into(), Some(sid), Some(&e.id)),
                        }
                    }
                    KIND_VARIABLE_CHANGE => {
                        let effects = text_of(&e.attrs, "effects");
                        if effects.is_empty() {
                            issue(&mut issues, "warning", "変数の変更内容が設定されていません".into(), Some(sid), Some(&e.id));
                        }
                        effects_of.push((effects.to_string(), sid.to_string(), e.id.clone(), "変数の変更".into()));
                    }
                    _ => {}
                }
            }
        }
        for (src, sid, eid, what) in &effects_of {
            for p in check_effects(src, &types) {
                issue(&mut issues, "error", format!("{what}: {p}"), Some(sid), Some(eid));
            }
            if let Ok(effects) = logic::parse_effects(src) {
                for eff in effects {
                    note(&mut writes, eff.variable.clone(), sid);
                    let mut names = Vec::new();
                    logic::variables_in(&eff.value, &mut names);
                    for n in names {
                        note(&mut reads, n, sid);
                    }
                }
            }
        }

        // 場面の順に進む辺（流れが分岐・遷移で終わっていない場面のみ）。
        for w in scenes.windows(2) {
            let (from, to) = (&w[0].1, &w[1].1);
            if !ends_flow.contains(&from.id) {
                edges.push(GraphEdge {
                    from: from.id.clone(),
                    to: to.id.clone(),
                    kind: "next",
                    label: String::new(),
                    condition: String::new(),
                    effects: String::new(),
                    element_id: None,
                });
            }
        }

        // 最初の場面から到達できる場面。
        let start = scenes.first().map(|(_, s, _)| s.id.clone());
        let mut reachable: HashSet<String> = HashSet::new();
        if let Some(start) = &start {
            let mut queue = VecDeque::from([start.clone()]);
            reachable.insert(start.clone());
            while let Some(cur) = queue.pop_front() {
                for e in edges.iter().filter(|e| e.from == cur) {
                    if reachable.insert(e.to.clone()) {
                        queue.push_back(e.to.clone());
                    }
                }
            }
        }
        let outgoing: HashSet<&str> = edges.iter().map(|e| e.from.as_str()).collect();
        let nodes: Vec<GraphNode> = scenes
            .iter()
            .enumerate()
            .map(|(order, (chapter, s, elements))| GraphNode {
                id: s.id.clone(),
                title: s.title.clone(),
                chapter_id: chapter.id.clone(),
                chapter_title: chapter.title.clone(),
                order,
                is_start: Some(&s.id) == start.as_ref(),
                is_end: !outgoing.contains(s.id.as_str()),
                reachable: reachable.contains(&s.id),
                element_count: elements.len(),
                choice_count: elements.iter().filter(|e| e.kind == KIND_CHOICE).count(),
            })
            .collect();
        for n in nodes.iter().filter(|n| !n.reachable) {
            issue(&mut issues, "warning", format!("場面「{}」には最初の場面からたどり着けません", n.title), Some(&n.id), None);
        }
        let mut endings: Vec<&str> = nodes.iter().filter(|n| n.is_end && n.reachable).map(|n| n.title.as_str()).collect();
        endings.dedup();
        if nodes.len() > 1 && !endings.is_empty() {
            issue(&mut issues, "info", format!("終わりの場面（エンディング）: {}", endings.join("、")), None, None);
        }

        // 変数の使用状況。
        for v in &variables {
            if !reads.contains_key(&v.name) && !writes.contains_key(&v.name) {
                issue(&mut issues, "info", format!("変数「{}」はどこでも使われていません", v.name), None, None);
            } else if !reads.contains_key(&v.name) {
                issue(&mut issues, "info", format!("変数「{}」は変更されていますが、条件では使われていません", v.name), None, None);
            } else if !writes.contains_key(&v.name) {
                issue(&mut issues, "info", format!("変数「{}」は条件で使われていますが、どこでも変更されていません（常に初期値のままです）", v.name), None, None);
            }
        }
        let names_of = |ids: Option<&Vec<String>>| -> Vec<String> {
            ids.map(|ids| ids.iter().filter_map(|id| title_of.get(id.as_str()).map(|t| t.to_string())).collect())
                .unwrap_or_default()
        };
        let variable_usage = variables
            .iter()
            .map(|v| VariableUsage {
                name: v.name.clone(),
                read_in: names_of(reads.get(&v.name)),
                written_in: names_of(writes.get(&v.name)),
            })
            .collect();

        // キャラクター相関: 台詞・動作の話者が同じ場面に登場する回数。
        let mut lines: HashMap<String, usize> = HashMap::new();
        let mut appearances: HashMap<String, HashSet<String>> = HashMap::new();
        let mut pair_counts: HashMap<(String, String), usize> = HashMap::new();
        for (_, scene, elements) in &scenes {
            let mut present: Vec<String> = Vec::new();
            for e in elements {
                if (e.kind == KIND_DIALOGUE || e.kind == KIND_CHARACTER_ACTION) && !e.speaker.is_empty() {
                    *lines.entry(e.speaker.clone()).or_default() += 1;
                    appearances.entry(e.speaker.clone()).or_default().insert(scene.id.clone());
                    if !present.contains(&e.speaker) {
                        present.push(e.speaker.clone());
                    }
                }
            }
            present.sort();
            for i in 0..present.len() {
                for j in i + 1..present.len() {
                    *pair_counts.entry((present[i].clone(), present[j].clone())).or_default() += 1;
                }
            }
        }
        let mut characters: Vec<CharacterNode> = lines
            .iter()
            .map(|(name, n)| CharacterNode { name: name.clone(), lines: *n, scenes: appearances.get(name).map_or(0, HashSet::len) })
            .collect();
        characters.sort_by(|a, b| b.lines.cmp(&a.lines).then(a.name.cmp(&b.name)));
        let mut relations: Vec<CharacterEdge> = pair_counts
            .into_iter()
            .map(|((a, b), n)| CharacterEdge { a, b, shared_scenes: n })
            .collect();
        relations.sort_by(|x, y| y.shared_scenes.cmp(&x.shared_scenes).then(x.a.cmp(&y.a)).then(x.b.cmp(&y.b)));

        let rank = |s: &str| match s {
            "error" => 0,
            "warning" => 1,
            _ => 2,
        };
        issues.sort_by_key(|i| rank(i.severity));
        Ok(Structure { nodes, edges, characters, relations, variables, variable_usage, issues })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content(kind: &str, speaker: &str, body: &str) -> ElementContent {
        ElementContent { kind: kind.into(), speaker: speaker.into(), body: body.into() }
    }

    #[test]
    fn builds_branch_graph_and_reports_issues() {
        let mut store = Store::open_in_memory().unwrap();
        let p = store.create_project("分岐", "", &["choice_adv".into()]).unwrap();
        let tree = store.project_tree(&p.id).unwrap();
        let ch = tree.chapters[0].chapter.id.clone();
        let s1 = tree.chapters[0].scenes[0].id.clone();
        let s2 = store.create_scene(&ch, "森").unwrap().id;
        let s3 = store.create_scene(&ch, "城").unwrap().id;
        let s4 = store.create_scene(&ch, "隠し部屋").unwrap().id;
        let s5 = store.create_scene(&ch, "結末").unwrap().id;
        store.create_variable(&p.id, "好感度", "number", "0", "").unwrap();
        store.create_variable(&p.id, "鍵", "bool", "false", "").unwrap();
        store.create_variable(&p.id, "未使用", "text", "\"\"", "").unwrap();

        store.create_element(&s1, content(KIND_DIALOGUE, "アリス", "どっちへ行く？"), None).unwrap();
        store.create_element(&s1, content(KIND_DIALOGUE, "ボブ", "森かな"), None).unwrap();
        let choice = store.create_element(&s1, content(KIND_CHOICE, "", "- 森\n- 城"), None).unwrap();
        let attrs = store.clean_attrs(&s1, KIND_CHOICE, json!({ "options": [
            { "text": "森へ", "target": s2, "effects": "好感度 += 1" },
            { "text": "城へ", "target": s3, "condition": "好感度 >= 2 and 鍵", "effects": "所持金 -= 1" }
        ] })).unwrap();
        store.set_element_attrs(&choice.id, attrs).unwrap();
        // 森 → 結末（条件なしの遷移で終わる）
        store.create_element(&s2, content(KIND_DIALOGUE, "アリス", "暗いね"), None).unwrap();
        let t = store.create_element(&s2, content(KIND_TRANSITION, "", ""), None).unwrap();
        store.set_element_attrs(&t.id, store.clean_attrs(&s2, KIND_TRANSITION, json!({ "target": s5 })).unwrap()).unwrap();
        // 城 → 次の場面（隠し部屋）へ流れるが、条件式に誤り
        let t2 = store.create_element(&s3, content(KIND_TRANSITION, "", ""), None).unwrap();
        store.set_element_attrs(&t2.id, store.clean_attrs(&s3, KIND_TRANSITION, json!({ "target": s5, "condition": "好感度 = 3" })).unwrap()).unwrap();
        let vc = store.create_element(&s3, content(KIND_VARIABLE_CHANGE, "", ""), None).unwrap();
        store.set_element_attrs(&vc.id, store.clean_attrs(&s3, KIND_VARIABLE_CHANGE, json!({ "effects": "鍵 = true" })).unwrap()).unwrap();

        let st = store.structure(&p.id).unwrap();
        let edge = |from: &str, to: &str, kind: &str| st.edges.iter().any(|e| e.from == from && e.to == to && e.kind == kind);
        assert!(edge(&s1, &s2, "choice") && edge(&s1, &s3, "choice"));
        assert!(!edge(&s1, &s2, "next"), "すべての項目に移動先がある選択肢の後は、順に進まない");
        assert!(edge(&s2, &s5, "transition"));
        assert!(!edge(&s2, &s3, "next"), "条件のない遷移で終わる場面は、順に進まない");
        assert!(edge(&s3, &s5, "transition") && edge(&s3, &s4, "next"));
        assert!(edge(&s4, &s5, "next"));
        let node = |id: &str| st.nodes.iter().find(|n| n.id == id).unwrap();
        assert!(node(&s1).is_start && node(&s5).is_end && node(&s4).reachable);
        assert_eq!(node(&s1).choice_count, 1);

        let messages: Vec<&str> = st.issues.iter().map(|i| i.message.as_str()).collect();
        assert!(messages.iter().any(|m| m.contains("「所持金」は定義されていません")), "{messages:?}");
        assert!(messages.iter().any(|m| m.contains("==")), "{messages:?}");
        assert!(messages.iter().any(|m| m.contains("「未使用」はどこでも使われていません")));
        assert!(!messages.iter().any(|m| m.contains("「鍵」は条件で使われていますが")), "鍵は城で変更している");
        assert_eq!(st.issues[0].severity, "error");

        let usage = st.variable_usage.iter().find(|u| u.name == "鍵").unwrap();
        assert_eq!(usage.read_in, ["場面1"]);
        assert_eq!(usage.written_in, ["城"]);

        // キャラクター相関。
        assert_eq!(st.characters[0].name, "アリス");
        assert_eq!(st.characters[0].lines, 2);
        assert_eq!(st.relations.len(), 1);
        assert_eq!((st.relations[0].a.as_str(), st.relations[0].b.as_str(), st.relations[0].shared_scenes), ("アリス", "ボブ", 1));

        // 移動先の場面を削除すると、到達できない場面とあわせて報告する。
        store.delete_scene(&s2).unwrap();
        let st = store.structure(&p.id).unwrap();
        assert!(st.issues.iter().any(|i| i.message.contains("移動先の場面が削除されています")));
    }

    #[test]
    fn unreachable_scenes_and_attr_validation() {
        let mut store = Store::open_in_memory().unwrap();
        let p = store.create_project("到達", "", &[]).unwrap();
        let tree = store.project_tree(&p.id).unwrap();
        let ch = tree.chapters[0].chapter.id.clone();
        let s1 = tree.chapters[0].scenes[0].id.clone();
        let s2 = store.create_scene(&ch, "孤立").unwrap().id;
        let s3 = store.create_scene(&ch, "到着").unwrap().id;
        let t = store.create_element(&s1, content(KIND_TRANSITION, "", ""), None).unwrap();
        store.set_element_attrs(&t.id, store.clean_attrs(&s1, KIND_TRANSITION, json!({ "target": s3 })).unwrap()).unwrap();
        let st = store.structure(&p.id).unwrap();
        assert!(!st.nodes.iter().find(|n| n.id == s2).unwrap().reachable);
        assert!(st.issues.iter().any(|i| i.severity == "warning" && i.scene_id.as_deref() == Some(s2.as_str())));

        // 別プロジェクトの場面や存在しない場面は移動先にできない。
        let other = store.create_project("別", "", &[]).unwrap();
        let other_scene = store.project_tree(&other.id).unwrap().chapters[0].scenes[0].id.clone();
        assert!(store.clean_attrs(&s1, KIND_TRANSITION, json!({ "target": other_scene })).is_err());
        assert!(store.clean_attrs(&s1, KIND_TRANSITION, json!({ "target": 3 })).is_err());
        assert!(store.clean_attrs(&s1, KIND_CHOICE, json!([])).is_err());
        let many: Vec<Value> = (0..21).map(|i| json!({ "text": i.to_string() })).collect();
        assert!(store.clean_attrs(&s1, KIND_CHOICE, json!({ "options": many })).is_err());
        // 余計な項目は取り除いて保存する。
        let cleaned = store.clean_attrs(&s1, KIND_TRANSITION, json!({ "target": "", "condition": " a ", "x": 1 })).unwrap();
        assert_eq!(cleaned, json!({ "target": null, "condition": "a" }));
    }

    #[test]
    fn variables_are_validated() {
        let mut store = Store::open_in_memory().unwrap();
        let p = store.create_project("変数", "", &[]).unwrap();
        let v = store.create_variable(&p.id, "好感度", "number", "3", "ヒロインからの").unwrap();
        assert!(store.create_variable(&p.id, "好感度", "number", "", "").is_err());
        assert!(store.create_variable(&p.id, "2nd", "number", "", "").is_err());
        assert!(store.create_variable(&p.id, "x", "float", "", "").is_err());
        assert!(store.create_variable(&p.id, "x", "number", "\"文字\"", "").is_err());
        assert!(store.create_variable(&p.id, "flag", "bool", "true", "").is_ok());
        let v = store.update_variable(&v.id, Some("信頼"), None, Some("5"), None).unwrap();
        assert_eq!((v.name.as_str(), v.initial.as_str(), v.description.as_str()), ("信頼", "5", "ヒロインからの"));
        assert!(store.update_variable(&v.id, Some("flag"), None, None, None).is_err());
        store.delete_variable(&v.id).unwrap();
        assert_eq!(store.list_variables(&p.id).unwrap().len(), 1);
    }
}
