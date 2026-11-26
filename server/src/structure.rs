//! 変数・分岐の構造データと、その可視化・検査。
//!
//! - 変数: プロジェクトごとに名前・型・初期値を持つ。
//! - 分岐: 選択肢（`Choice`）の各項目と遷移（`Transition`）が、移動先の場面・条件式・変更式を `attrs` に持つ。
//! - 場面グラフ・キャラクター相関・構造チェックは、保存されたデータからプログラムが生成する（LLM は使わない）。

use std::collections::HashMap;

use rusqlite::{params, OptionalExtension, Row};
use serde_json::Value;

use crate::error::{AppError, Result};
use crate::logic::VarType;
use crate::store::{new_id, Store, NOW};


pub use scenote_core::structure::*;

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
        let (name, t, initial) = scenote_core::structure::check_variable(name, var_type, initial).map_err(AppError::Invalid)?;
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
        Ok((name, t, initial))
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
        let project_id: String = self.conn.query_row(
            "SELECT c.project_id FROM scenes s JOIN chapters c ON c.id = s.chapter_id WHERE s.id = ?1",
            [scene_id],
            |r| r.get(0),
        )?;
        let mut db_error = None;
        let result = scenote_core::validate::clean_attrs(kind, attrs, |target| {
            match self
                .conn
                .query_row(
                    "SELECT 1 FROM scenes s JOIN chapters c ON c.id = s.chapter_id WHERE s.id = ?1 AND c.project_id = ?2",
                    params![target, project_id],
                    |_| Ok(()),
                )
                .optional()
            {
                Ok(found) => found.is_some(),
                Err(e) => {
                    db_error = Some(e);
                    false
                }
            }
        });
        if let Some(e) = db_error {
            return Err(e.into());
        }
        result.map_err(AppError::Invalid)
    }

    // ---- 可視化・構造チェック ------------------------------------------

    /// 場面グラフ・キャラクター相関・変数の使用状況・構造上の問題をまとめて生成する。
    pub fn structure(&self, project_id: &str) -> Result<Structure> {
        let tree = self.project_tree(project_id)?;
        let mut details = HashMap::new();
        for node in &tree.chapters {
            for s in &node.scenes {
                details.insert(s.id.clone(), self.scene_detail(&s.id)?.elements);
            }
        }
        Ok(build_structure(&tree, &details, self.list_variables(project_id)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use crate::model::*;

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
