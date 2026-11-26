//! 分岐の構造（場面グラフ・キャラクター相関・変数の使用状況・構造チェック）。DB に依存しない計算。

use std::collections::{HashMap, HashSet, VecDeque};

use serde::Serialize;
use serde_json::Value;

use crate::logic::{self, check_condition, check_effects, VarType};
use crate::model::*;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Variable {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub var_type: VarType,
    pub initial: String,
    pub description: String,
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


/// 変数の名前・型・初期値を検証する（重複の確認は呼び出し側で行う）。
pub fn check_variable(name: &str, var_type: &str, initial: &str) -> crate::validate::Checked<(String, VarType, String)> {
    let name = name.trim();
    if !logic::is_valid_name(name) {
        return Err(
            "変数名は文字（英字・日本語）か _ で始まり、文字・数字・_ だけを使った50文字以内にしてください（and・or・not・true・false は使えません）".into(),
        );
    }
    let t = VarType::parse(var_type).ok_or_else(|| "変数の型は number・bool・text のいずれかです".to_string())?;
    let initial = initial.trim();
    if !initial.is_empty() {
        let problems = check_effects(&format!("{name} = {initial}"), &HashMap::from([(name.to_string(), t)]));
        if let Some(p) = problems.first() {
            return Err(format!("初期値が正しくありません: {p}"));
        }
    }
    Ok((name.to_string(), t, initial.to_string()))
}

/// 場面グラフ・キャラクター相関・変数の使用状況・構造上の問題をまとめて生成する。
pub fn build_structure(tree: &ProjectTree, details: &HashMap<String, Vec<Element>>, variables: Vec<Variable>) -> Structure {
    let types: HashMap<String, VarType> = variables.iter().map(|v| (v.name.clone(), v.var_type)).collect();
    let mut issues: Vec<Issue> = Vec::new();
    let issue = |issues: &mut Vec<Issue>, severity: &'static str, message: String, scene: Option<&str>, element: Option<&str>| {
        issues.push(Issue { severity, message, scene_id: scene.map(str::to_string), element_id: element.map(str::to_string) });
    };

    // 場面をプロジェクト全体の順に並べる。
    let mut scenes = Vec::new();
    for node in &tree.chapters {
        for s in &node.scenes {
            scenes.push((node.chapter.clone(), s.clone(), details.get(&s.id).cloned().unwrap_or_default()));
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
    Structure { nodes, edges, characters, relations, variables, variable_usage, issues }
}
