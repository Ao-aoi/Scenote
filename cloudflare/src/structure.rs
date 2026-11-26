//! 変数と構造チェック（Cloudflare 版）。検証と構造の計算は `scenote_core::structure` を共有する。

use std::collections::HashMap;

use serde::Deserialize;

use scenote_core::logic::VarType;
use scenote_core::structure::{build_structure, check_variable, Structure, Variable};

use crate::db::NOW;
use crate::error::{AppError, Result};
use crate::p;
use crate::store::{new_id, Store};

#[derive(Deserialize)]
struct VariableRow {
    id: String,
    project_id: String,
    name: String,
    var_type: String,
    initial: String,
    description: String,
}

impl From<VariableRow> for Variable {
    fn from(r: VariableRow) -> Self {
        Variable {
            id: r.id,
            project_id: r.project_id,
            name: r.name,
            var_type: VarType::parse(&r.var_type).unwrap_or(VarType::Number),
            initial: r.initial,
            description: r.description,
        }
    }
}

fn type_name(t: VarType) -> String {
    serde_json::to_value(t).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_else(|| "number".into())
}

impl Store {
    pub fn list_variables(&self, project_id: &str) -> Result<Vec<Variable>> {
        let rows: Vec<VariableRow> = self.db.all("SELECT * FROM variables WHERE project_id = ? ORDER BY name", p![project_id])?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub fn get_variable(&self, id: &str) -> Result<Variable> {
        self.db
            .opt::<VariableRow>("SELECT * FROM variables WHERE id = ?", p![id])?
            .map(Into::into)
            .ok_or(AppError::NotFound("変数"))
    }

    fn checked_variable(&self, project_id: &str, name: &str, var_type: &str, initial: &str, exclude: Option<&str>) -> Result<(String, VarType, String)> {
        let (name, t, initial) = check_variable(name, var_type, initial).map_err(AppError::Invalid)?;
        #[derive(Deserialize)]
        struct IdRow {
            #[allow(dead_code)]
            id: String,
        }
        let dup: Option<IdRow> = self.db.opt(
            "SELECT id FROM variables WHERE project_id = ? AND name = ? AND id != ?",
            p![project_id, name.as_str(), exclude.unwrap_or("")],
        )?;
        if dup.is_some() {
            return Err(AppError::Invalid(format!("変数「{name}」はすでにあります")));
        }
        Ok((name, t, initial))
    }

    pub fn create_variable(&self, project_id: &str, name: &str, var_type: &str, initial: &str, description: &str) -> Result<Variable> {
        self.get_project(project_id)?;
        let (name, t, initial) = self.checked_variable(project_id, name, var_type, initial, None)?;
        let id = new_id();
        self.db.exec(
            &format!(
                "INSERT INTO variables (id, project_id, name, var_type, initial, description, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, {NOW}, {NOW})"
            ),
            p![id.as_str(), project_id, name, type_name(t), initial, description.trim()],
        )?;
        self.get_variable(&id)
    }

    pub fn update_variable(&self, id: &str, name: Option<&str>, var_type: Option<&str>, initial: Option<&str>, description: Option<&str>) -> Result<Variable> {
        let v = self.get_variable(id)?;
        let current_type = type_name(v.var_type);
        let (name, t, initial) = self.checked_variable(
            &v.project_id,
            name.unwrap_or(&v.name),
            var_type.unwrap_or(&current_type),
            initial.unwrap_or(&v.initial),
            Some(id),
        )?;
        self.db.exec(
            &format!("UPDATE variables SET name = ?, var_type = ?, initial = ?, description = ?, updated_at = {NOW} WHERE id = ?"),
            p![name, type_name(t), initial, description.map(str::trim).unwrap_or(&v.description), id],
        )?;
        self.get_variable(id)
    }

    pub fn delete_variable(&self, id: &str) -> Result<()> {
        self.get_variable(id)?;
        self.db.exec("DELETE FROM variables WHERE id = ?", p![id])
    }

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
