//! MCP サーバー（Streamable HTTP、JSON 応答）。
//!
//! 外部の AI（Claude・ChatGPT など）が、シナリオ全体の流れ・人物関係・面白さを検討する
//! 「プロデューサー」として、読み取りと書き込みを行えるようにする。
//! ツールはすべて HTTP API（`api.rs`）の呼び出しに置き換えて処理するため、検証・競合検出・
//! 変更通知は画面からの操作と同じになる。書き込んだ本文の由来は `ai_direct` になる。

use serde_json::{json, Value};

/// 対応する MCP の版（新しい順）。クライアントが求めた版に対応していればそれを返す。
const PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

pub const INSTRUCTIONS: &str = "\
Scenote はゲームシナリオ（章 → 場面 → 要素）を管理するアプリです。あなたは作品のプロデューサー兼共同執筆者として、\
全体の流れの整合性、人物の関係と動機、伏線と回収、テンポ、面白さを検討し、必要に応じて本文や構成を直接書き換えられます。

進め方の目安:
1. list_projects → get_outline で章・場面・概要（プロット）をつかむ。全体を読むときは read_project_text。
2. 個別の場面は read_scene で読む（要素の ID・リビジョン・分岐の設定つき）。
3. 書き換える前に、方針を利用者に説明し、合意を得てから書き込む。利用者の文体と既存の設定を尊重する。
4. 場面全体の書き直しは write_scene（Markdown）、一部の変更は update_element、追記は append_to_scene。
5. 人物は get_characters で一覧（情報・関係つき）を確認し、create_character・add_character_fact・add_relation などで整理する。
   世界観・設定資料は参照ファイル（list_files・read_file・create_file・update_file）にまとめる。
6. 大きな書き換えの前には create_snapshot で保存する。誤った変更は list_history で確認し、restore_snapshot で戻せる
   （復元の直前の状態も自動で保存される）。

人物の情報（fact）の由来 source: user（利用者が決めた設定）・confirmed（確認済み）・inferred（推定。確信度 confidence 0〜1 が必要）。
本文から読み取っただけの情報は inferred とし、利用者の設定（user）を勝手に書き換えないでください。

場面の Markdown 記法（空行で区切った1かたまりが1要素）:
- 地の文: そのまま書く
- 台詞: **話者**: 本文
- 動作: [動作:名前] 本文　/ 演出: [演出] 本文　/ 選択肢: [選択肢] 本文　/ 遷移: [遷移] 本文　/ 変数の変更: [変数] 本文
- 本文中の空行は、行頭に \\ だけを書いた行で表す。

競合: 書き込みには base_revision（read_scene で得た revision）が必要なものがあります。他の端末で先に変更されていると\
 conflict になるので、読み直してから書き直してください。分岐（選択肢の移動先・条件など）は update_element の attrs で設定します。\
 削除は取り消せないため、削除の前には必ず利用者に確認してください。";

fn tool(name: &str, description: &str, properties: Value, required: &[&str], read_only: bool) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": { "type": "object", "properties": properties, "required": required, "additionalProperties": false },
        "annotations": { "readOnlyHint": read_only, "destructiveHint": !read_only && name.starts_with("delete") },
    })
}

pub fn tools() -> Value {
    let id = |what: &str| json!({ "type": "string", "description": format!("{what}の ID") });
    let rev = json!({ "type": "integer", "description": "読み取ったときのリビジョン（競合検出用）" });
    let ids = json!({ "type": "array", "items": { "type": "string" }, "description": "新しい順序で並べた ID（現在の一覧と同じ要素をすべて含める）" });
    json!([
        tool("list_projects", "プロジェクトの一覧を返す。", json!({}), &[], true),
        tool("get_outline", "プロジェクトの構成（章・場面の題名・概要・リビジョン）を返す。",
            json!({ "project_id": id("プロジェクト") }), &["project_id"], true),
        tool("read_project_text", "プロジェクト全体の本文を Markdown（# 章、## 場面）で返す。全体の流れを読むときに使う。",
            json!({ "project_id": id("プロジェクト") }), &["project_id"], true),
        tool("read_scene", "場面の本文を返す。Markdown と、要素ごとの ID・種類・話者・本文・リビジョン・分岐の設定（attrs）を含む。",
            json!({ "scene_id": id("場面") }), &["scene_id"], true),
        tool("export_project_json", "ID・由来情報を含むプロジェクト全体の構造を JSON で返す。",
            json!({ "project_id": id("プロジェクト") }), &["project_id"], true),

        tool("create_project", "プロジェクトを作成する（最初の章と場面も作られる）。",
            json!({ "name": { "type": "string" }, "description": { "type": "string" } }), &["name"], false),
        tool("update_project", "プロジェクトの名前・説明を変更する。",
            json!({ "project_id": id("プロジェクト"), "name": { "type": "string" }, "description": { "type": "string" } }), &["project_id"], false),

        tool("create_chapter", "章を末尾に追加する。", json!({ "project_id": id("プロジェクト"), "title": { "type": "string" } }), &["project_id", "title"], false),
        tool("rename_chapter", "章の題名を変更する。", json!({ "chapter_id": id("章"), "title": { "type": "string" } }), &["chapter_id", "title"], false),
        tool("reorder_chapters", "章の順序を変更する。", json!({ "project_id": id("プロジェクト"), "ids": ids }), &["project_id", "ids"], false),
        tool("delete_chapter", "章を、含まれる場面・本文ごと削除する（取り消せない。事前に利用者へ確認すること）。",
            json!({ "chapter_id": id("章") }), &["chapter_id"], false),

        tool("create_scene", "場面を追加する。after を指定するとその場面の直後、省略すると章の末尾に追加する。",
            json!({ "chapter_id": id("章"), "title": { "type": "string" }, "summary": { "type": "string", "description": "場面の概要（プロット）" }, "after": id("直前の場面") }),
            &["chapter_id", "title"], false),
        tool("update_scene", "場面の題名・概要（プロット）を変更する。概要を変更するときは base_revision が必要。",
            json!({ "scene_id": id("場面"), "title": { "type": "string" }, "summary": { "type": "string" }, "base_revision": rev }), &["scene_id"], false),
        tool("reorder_scenes", "章の中の場面の順序を変更する。", json!({ "chapter_id": id("章"), "ids": ids }), &["chapter_id", "ids"], false),
        tool("delete_scene", "場面を本文ごと削除する（取り消せない。事前に利用者へ確認すること）。", json!({ "scene_id": id("場面") }), &["scene_id"], false),

        tool("write_scene", "場面の本文全体を Markdown で置き換える。内容が同じ要素は ID を保つ。base_revision は read_scene の revision。",
            json!({ "scene_id": id("場面"), "base_revision": rev, "markdown": { "type": "string" } }), &["scene_id", "base_revision", "markdown"], false),
        tool("append_to_scene", "場面の末尾に Markdown の本文を追加する。",
            json!({ "scene_id": id("場面"), "markdown": { "type": "string" } }), &["scene_id", "markdown"], false),
        tool("add_element", "場面に要素を1つ追加する。position を省略すると末尾。",
            json!({ "scene_id": id("場面"),
                    "kind": { "type": "string", "enum": ["Narration", "Dialogue", "CharacterAction", "Direction", "Choice", "Transition", "VariableChange"] },
                    "speaker": { "type": "string" }, "body": { "type": "string" }, "position": { "type": "integer" } }),
            &["scene_id", "kind", "body"], false),
        tool("update_element", "要素の種類・話者・本文・分岐の設定を変更する。attrs は選択肢なら {options:[{text,target,condition,effects}]}、遷移なら {target,condition}、変数の変更なら {effects}（target は場面の ID）。",
            json!({ "element_id": id("要素"), "base_revision": rev, "kind": { "type": "string" }, "speaker": { "type": "string" }, "body": { "type": "string" }, "attrs": { "type": "object" } }),
            &["element_id", "base_revision"], false),
        tool("delete_element", "要素を削除する。", json!({ "element_id": id("要素") }), &["element_id"], false),

        tool("get_characters", "プロジェクトのキャラクターの一覧を、人物の情報（facts）と関係（relations）つきで返す。",
            json!({ "project_id": id("プロジェクト") }), &["project_id"], true),
        tool("create_character", "キャラクターを登録する。", json!({ "project_id": id("プロジェクト"), "name": { "type": "string" }, "aliases": { "type": "array", "items": { "type": "string" } } }), &["project_id", "name"], false),
        tool("update_character", "キャラクターの名前・別名を変更する。", json!({ "character_id": id("キャラクター"), "name": { "type": "string" }, "aliases": { "type": "array", "items": { "type": "string" } } }), &["character_id"], false),
        tool("delete_character", "キャラクターを、情報・関係ごと削除する（本文の話者名は残る。事前に利用者へ確認すること）。", json!({ "character_id": id("キャラクター") }), &["character_id"], false),
        tool("add_character_fact", "人物の情報（プロフィール・性格・口調・目的・秘密など）を追加する。",
            json!({ "character_id": id("キャラクター"), "category": { "type": "string", "description": "分類（例: 性格・口調・過去・目的・秘密）" }, "text": { "type": "string" },
                    "source": { "type": "string", "enum": ["user", "confirmed", "inferred"] }, "confidence": { "type": "number" },
                    "evidence": { "type": "array", "items": { "type": "string" }, "description": "根拠となる要素の ID" } }),
            &["character_id", "category", "text"], false),
        tool("update_character_fact", "人物の情報を変更する。status を rejected にすると無効にできる。",
            json!({ "fact_id": id("情報"), "category": { "type": "string" }, "text": { "type": "string" }, "source": { "type": "string", "enum": ["user", "confirmed", "inferred"] },
                    "confidence": { "type": "number" }, "status": { "type": "string", "enum": ["active", "proposed", "rejected"] } }),
            &["fact_id"], false),
        tool("delete_character_fact", "人物の情報を削除する。", json!({ "fact_id": id("情報") }), &["fact_id"], false),
        tool("add_relation", "2 人の関係（from から to への見方・関係）を追加する。",
            json!({ "project_id": id("プロジェクト"), "from_character_id": id("キャラクター"), "to_character_id": id("キャラクター"), "kind": { "type": "string", "description": "関係の種類（例: 幼なじみ・敵対・片思い）" },
                    "text": { "type": "string" }, "source": { "type": "string", "enum": ["user", "confirmed", "inferred"] }, "confidence": { "type": "number" } }),
            &["project_id", "from_character_id", "to_character_id", "kind"], false),
        tool("update_relation", "関係の種類・説明などを変更する（2 人の組み合わせは変えられない）。",
            json!({ "relation_id": id("関係"), "kind": { "type": "string" }, "text": { "type": "string" }, "source": { "type": "string", "enum": ["user", "confirmed", "inferred"] },
                    "confidence": { "type": "number" }, "status": { "type": "string", "enum": ["active", "proposed", "rejected"] } }),
            &["relation_id"], false),
        tool("delete_relation", "関係を削除する。", json!({ "relation_id": id("関係") }), &["relation_id"], false),

        tool("list_files", "参照ファイル（世界観・設定資料・制作方針など）の一覧を返す（本文は含まない）。", json!({ "project_id": id("プロジェクト") }), &["project_id"], true),
        tool("read_file", "参照ファイルの本文（Markdown）とリビジョンを返す。", json!({ "file_id": id("参照ファイル") }), &["file_id"], true),
        tool("create_file", "参照ファイルを作成する。pinned を true にすると常に参照する資料になる。",
            json!({ "project_id": id("プロジェクト"), "name": { "type": "string" }, "content": { "type": "string" }, "pinned": { "type": "boolean" } }), &["project_id", "name"], false),
        tool("update_file", "参照ファイルの名前・本文を変更する。base_revision は read_file の revision。",
            json!({ "file_id": id("参照ファイル"), "base_revision": rev, "name": { "type": "string" }, "content": { "type": "string" }, "pinned": { "type": "boolean" } }),
            &["file_id", "base_revision"], false),
        tool("delete_file", "参照ファイルを削除する（事前に利用者へ確認すること）。", json!({ "file_id": id("参照ファイル") }), &["file_id"], false),

        tool("list_history", "編集履歴を新しい順に返す（変更前後の内容と、誰が変更したか）。",
            json!({ "project_id": id("プロジェクト"), "scene_id": id("場面（絞り込み）"), "limit": { "type": "integer" } }), &["project_id"], true),
        tool("list_snapshots", "スナップショット（保存した時点）の一覧を返す。", json!({ "project_id": id("プロジェクト") }), &["project_id"], true),
        tool("create_snapshot", "現在のプロジェクト全体を保存する。大きな書き換えの前に使う。",
            json!({ "project_id": id("プロジェクト"), "note": { "type": "string", "description": "メモ（例: 第2章の書き直し前）" } }), &["project_id"], false),
        tool("compare_snapshot", "スナップショットと現在の違いを、場面・参照ファイルごとに返す。", json!({ "snapshot_id": id("スナップショット") }), &["snapshot_id"], true),
        tool("restore_snapshot", "スナップショットの状態へ戻す（直前の状態は自動で保存される。事前に利用者へ確認すること）。",
            json!({ "snapshot_id": id("スナップショット") }), &["snapshot_id"], false),
    ])
}

/// ツールの呼び出しを、API の要求（メソッド・パス・本文）に置き換える。
pub enum Call {
    Api { method: &'static str, path: String, body: Value },
    /// 場面の末尾への追記（読み取りと書き込みを1つのトランザクションで行う）。
    Append { scene_id: String, markdown: String },
    /// キャラクターの一覧（情報・関係つき）。
    Characters { project_id: String },
    /// 参照ファイルの一覧（本文を除く）。
    FileList { project_id: String },
}

fn s(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("引数 {key} が必要です"))
}

/// ID を URL のパスに入れても安全か。
fn safe(id: String) -> Result<String, String> {
    if id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        Ok(id)
    } else {
        Err("ID の形式が正しくありません".into())
    }
}

fn pick(args: &Value, keys: &[&str]) -> Value {
    let mut out = serde_json::Map::new();
    for k in keys {
        if let Some(v) = args.get(*k) {
            if !v.is_null() {
                out.insert((*k).to_string(), v.clone());
            }
        }
    }
    Value::Object(out)
}

pub fn to_call(name: &str, args: &Value) -> Result<Call, String> {
    use Call::Api;
    let id = |k: &str| s(args, k).and_then(safe);
    Ok(match name {
        "list_projects" => Api { method: "GET", path: "projects".into(), body: Value::Null },
        "get_outline" => Api { method: "GET", path: format!("projects/{}", id("project_id")?), body: Value::Null },
        "read_project_text" => Api { method: "GET", path: format!("projects/{}/export/markdown", id("project_id")?), body: Value::Null },
        "export_project_json" => Api { method: "GET", path: format!("projects/{}/export/json", id("project_id")?), body: Value::Null },
        "read_scene" => Api { method: "GET", path: format!("scenes/{}", id("scene_id")?), body: Value::Null },
        "create_project" => Api { method: "POST", path: "projects".into(), body: pick(args, &["name", "description"]) },
        "update_project" => Api { method: "PATCH", path: format!("projects/{}", id("project_id")?), body: pick(args, &["name", "description"]) },
        "create_chapter" => Api { method: "POST", path: format!("projects/{}/chapters", id("project_id")?), body: pick(args, &["title"]) },
        "rename_chapter" => Api { method: "PATCH", path: format!("chapters/{}", id("chapter_id")?), body: pick(args, &["title"]) },
        "reorder_chapters" => Api { method: "PUT", path: format!("projects/{}/chapters/order", id("project_id")?), body: pick(args, &["ids"]) },
        "delete_chapter" => Api { method: "DELETE", path: format!("chapters/{}", id("chapter_id")?), body: Value::Null },
        "create_scene" => Api { method: "POST", path: format!("chapters/{}/scenes", id("chapter_id")?), body: pick(args, &["title", "summary", "after"]) },
        "update_scene" => Api { method: "PATCH", path: format!("scenes/{}", id("scene_id")?), body: pick(args, &["title", "summary", "base_revision"]) },
        "reorder_scenes" => Api { method: "PUT", path: format!("chapters/{}/scenes/order", id("chapter_id")?), body: pick(args, &["ids"]) },
        "delete_scene" => Api { method: "DELETE", path: format!("scenes/{}", id("scene_id")?), body: Value::Null },
        "write_scene" => Api { method: "PUT", path: format!("scenes/{}/markdown", id("scene_id")?), body: pick(args, &["base_revision", "markdown"]) },
        "append_to_scene" => Call::Append { scene_id: id("scene_id")?, markdown: s(args, "markdown")? },
        "add_element" => {
            let mut body = pick(args, &["kind", "speaker", "body", "position"]);
            if body.get("speaker").is_none() {
                body["speaker"] = json!("");
            }
            Api { method: "POST", path: format!("scenes/{}/elements", id("scene_id")?), body }
        }
        "update_element" => Api { method: "PATCH", path: format!("elements/{}", id("element_id")?), body: pick(args, &["base_revision", "kind", "speaker", "body", "attrs"]) },
        "delete_element" => Api { method: "DELETE", path: format!("elements/{}", id("element_id")?), body: Value::Null },
        "get_characters" => Call::Characters { project_id: id("project_id")? },
        "create_character" => Api { method: "POST", path: format!("projects/{}/characters", id("project_id")?), body: pick(args, &["name", "aliases"]) },
        "update_character" => Api { method: "PATCH", path: format!("characters/{}", id("character_id")?), body: pick(args, &["name", "aliases"]) },
        "delete_character" => Api { method: "DELETE", path: format!("characters/{}", id("character_id")?), body: Value::Null },
        "add_character_fact" => Api { method: "POST", path: format!("characters/{}/facts", id("character_id")?), body: pick(args, &["category", "text", "source", "confidence", "evidence"]) },
        "update_character_fact" => Api { method: "PATCH", path: format!("character-facts/{}", id("fact_id")?), body: pick(args, &["category", "text", "source", "confidence", "status"]) },
        "delete_character_fact" => Api { method: "DELETE", path: format!("character-facts/{}", id("fact_id")?), body: Value::Null },
        "add_relation" => Api {
            method: "POST",
            path: format!("projects/{}/character-relations", id("project_id")?),
            body: pick(args, &["from_character_id", "to_character_id", "kind", "text", "source", "confidence"]),
        },
        "update_relation" => Api { method: "PATCH", path: format!("character-relations/{}", id("relation_id")?), body: pick(args, &["kind", "text", "source", "confidence", "status"]) },
        "delete_relation" => Api { method: "DELETE", path: format!("character-relations/{}", id("relation_id")?), body: Value::Null },
        "list_files" => Call::FileList { project_id: id("project_id")? },
        "read_file" => Api { method: "GET", path: format!("files/{}", id("file_id")?), body: Value::Null },
        "create_file" => Api { method: "POST", path: format!("projects/{}/files", id("project_id")?), body: pick(args, &["name", "content", "pinned"]) },
        "update_file" => Api { method: "PATCH", path: format!("files/{}", id("file_id")?), body: pick(args, &["base_revision", "name", "content", "pinned"]) },
        "delete_file" => Api { method: "DELETE", path: format!("files/{}", id("file_id")?), body: Value::Null },
        "list_history" => {
            let mut q = url::form_urlencoded::Serializer::new(String::new());
            if let Ok(scene) = id("scene_id") {
                q.append_pair("scene_id", &scene);
            }
            let limit = args.get("limit").and_then(Value::as_i64).unwrap_or(30).clamp(1, 100);
            q.append_pair("limit", &limit.to_string());
            Api { method: "GET", path: format!("projects/{}/history?{}", id("project_id")?, q.finish()), body: Value::Null }
        }
        "list_snapshots" => Api { method: "GET", path: format!("projects/{}/snapshots", id("project_id")?), body: Value::Null },
        "create_snapshot" => Api { method: "POST", path: format!("projects/{}/snapshots", id("project_id")?), body: pick(args, &["note"]) },
        "compare_snapshot" => Api { method: "GET", path: format!("snapshots/{}/compare", id("snapshot_id")?), body: Value::Null },
        "restore_snapshot" => Api { method: "POST", path: format!("snapshots/{}/restore", id("snapshot_id")?), body: Value::Null },
        _ => return Err(format!("未知のツールです: {name}")),
    })
}

pub fn negotiate_version(requested: Option<&str>) -> &'static str {
    requested
        .and_then(|r| PROTOCOL_VERSIONS.iter().find(|v| **v == r).copied())
        .unwrap_or(PROTOCOL_VERSIONS[0])
}

pub fn initialize_result(requested: Option<&str>) -> Value {
    json!({
        "protocolVersion": negotiate_version(requested),
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": "scenote", "title": "Scenote", "version": env!("CARGO_PKG_VERSION") },
        "instructions": INSTRUCTIONS,
    })
}

pub fn rpc_result(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

pub fn rpc_error(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// ツールの結果（文字列）を MCP の形にする。
pub fn tool_result(text: String, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}
