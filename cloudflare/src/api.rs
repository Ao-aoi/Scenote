//! HTTP API。パス・要求・応答の形は PC 版（`server/src/api.rs`）と同じにする。
//!
//! 処理は同期的に行い、結果を `ApiResponse` で返す（書き込みはトランザクションの中で呼ばれる）。

use serde::Deserialize;
use serde_json::{json, Value};

use scenote_core::model::*;

use crate::error::{AppError, Result};
use crate::files::FilePatch;
use crate::store::{ElementPatch, Store};

/// Cloudflare 版で使える機能。画面はこれを見て、使えない機能の入口を隠す。
pub const FEATURES: [&str; 4] = ["export", "mcp", "history", "files"];

pub enum Body {
    Json(Value),
    Download { content_type: &'static str, disposition: String, text: String },
    Empty,
}

pub struct ApiResponse {
    pub status: u16,
    pub body: Body,
}

impl ApiResponse {
    fn json(status: u16, value: impl serde::Serialize) -> Result<Self> {
        let value = serde_json::to_value(value).map_err(|e| AppError::Internal(e.to_string()))?;
        Ok(Self { status, body: Body::Json(value) })
    }

    fn ok(value: impl serde::Serialize) -> Result<Self> {
        Self::json(200, value)
    }

    fn created(value: impl serde::Serialize) -> Result<Self> {
        Self::json(201, value)
    }

    fn no_content() -> Result<Self> {
        Ok(Self { status: 204, body: Body::Empty })
    }

    pub fn error(e: &AppError) -> Self {
        Self { status: e.status(), body: Body::Json(e.body()) }
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// 要求を送ってきた利用者（Cloudflare Access で確認したメールアドレス）。
pub struct Caller {
    pub email: String,
}

pub struct ApiRequest<'a> {
    pub method: &'a str,
    /// `/api/` を除いたパス。
    pub path: &'a str,
    pub body: &'a str,
    pub caller: &'a Caller,
}

fn parse<'de, T: Deserialize<'de>>(body: &'de str) -> Result<T> {
    Ok(serde_json::from_str(if body.trim().is_empty() { "{}" } else { body })?)
}

/// 状態を変える要求か。
pub fn is_mutating(method: &str) -> bool {
    !matches!(method, "GET" | "HEAD" | "OPTIONS")
}

pub fn handle(store: &Store, req: &ApiRequest) -> ApiResponse {
    match route(store, req) {
        Ok(r) => r,
        Err(e) => {
            if let AppError::Internal(detail) = &e {
                // 詳細はログにのみ出す（創作データは含めない）。
                worker::console_error!("internal error: {detail}");
            }
            ApiResponse::error(&e)
        }
    }
}

#[derive(Deserialize)]
struct CreateProject {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    templates: Vec<String>,
}

#[derive(Deserialize)]
struct UpdateProject {
    name: Option<String>,
    description: Option<String>,
    templates: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct TitleBody {
    title: String,
}

#[derive(Deserialize)]
struct OrderBody {
    ids: Vec<String>,
}

#[derive(Deserialize)]
struct CreateSceneBody {
    title: String,
    #[serde(default)]
    summary: String,
    after: Option<String>,
}

#[derive(Deserialize)]
struct UpdateSceneBody {
    title: Option<String>,
    summary: Option<String>,
    base_revision: Option<i64>,
}

#[derive(Deserialize)]
struct CreateElement {
    #[serde(flatten)]
    content: ElementContent,
    position: Option<i64>,
    character_id: Option<String>,
}

#[derive(Deserialize)]
struct UpdateElement {
    base_revision: i64,
    kind: Option<String>,
    speaker: Option<String>,
    body: Option<String>,
    attrs: Option<Value>,
    #[serde(default, deserialize_with = "nullable")]
    character_id: Option<Option<String>>,
}

/// 「省略」と「null」を区別して受け取る。
fn nullable<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(d).map(Some)
}

#[derive(Deserialize)]
struct CreateFile {
    name: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    pinned: bool,
}

#[derive(Deserialize)]
struct UpdateFile {
    base_revision: i64,
    name: Option<String>,
    content: Option<String>,
    pinned: Option<bool>,
}

#[derive(Deserialize)]
struct HistorySettings {
    max_entries: i64,
}

#[derive(Deserialize, Default)]
struct SnapshotBody {
    #[serde(default)]
    note: String,
}

#[derive(Deserialize)]
struct MarkdownBody {
    base_revision: i64,
    markdown: String,
}

/// 要素の変更結果。場面のリビジョンも進むため、併せて返す。
fn element_response(store: &Store, status: u16, element: Element) -> Result<ApiResponse> {
    let scene_revision = store.get_scene(&element.scene_id)?.revision;
    ApiResponse::json(status, json!({ "element": element, "scene_revision": scene_revision }))
}

/// ダウンロード用の応答。ファイル名はプロジェクト名（RFC 5987 形式）。
fn attachment(name: &str, ext: &str, content_type: &'static str, text: String) -> ApiResponse {
    let mut encoded = String::new();
    for b in format!("{name}.{ext}").bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.') {
            encoded.push(b as char);
        } else {
            encoded.push_str(&format!("%{b:02X}"));
        }
    }
    let disposition = format!("attachment; filename=\"scenote-export.{ext}\"; filename*=UTF-8''{encoded}");
    ApiResponse { status: 200, body: Body::Download { content_type, disposition, text } }
}

fn query_param(query: &str, key: &str) -> Option<String> {
    url::form_urlencoded::parse(query.as_bytes()).find(|(k, _)| k == key).map(|(_, v)| v.into_owned())
}

fn route(store: &Store, req: &ApiRequest) -> Result<ApiResponse> {
    let (path, query) = req.path.split_once('?').unwrap_or((req.path, ""));
    let segments: Vec<&str> = path.trim_end_matches('/').split('/').collect();
    let body = req.body;
    match (req.method, segments.as_slice()) {
        ("GET", ["meta"]) => ApiResponse::ok(json!({
            "schema_version": SCHEMA_VERSION,
            "element_kinds": BASIC_KINDS,
            "templates": TEMPLATES,
            "edition": "cloudflare",
            "features": FEATURES,
        })),
        ("GET", ["auth", "status"]) => ApiResponse::ok(json!({
            "authenticated": true,
            "local": false,
            "device_name": req.caller.email,
            "lan_enabled": false,
            "logout_url": "/cdn-cgi/access/logout",
        })),

        // ---- プロジェクト
        ("GET", ["projects"]) => ApiResponse::ok(store.list_projects()?),
        ("POST", ["projects"]) => {
            let b: CreateProject = parse(body)?;
            ApiResponse::created(store.create_project(&b.name, &b.description, &b.templates)?)
        }
        ("GET", ["projects", id]) => ApiResponse::ok(store.project_tree(id)?),
        ("PATCH", ["projects", id]) => {
            let b: UpdateProject = parse(body)?;
            ApiResponse::ok(store.update_project(id, b.name, b.description, b.templates)?)
        }
        ("DELETE", ["projects", id]) => {
            store.delete_project(id)?;
            ApiResponse::no_content()
        }
        ("POST", ["projects", id, "chapters"]) => {
            let b: TitleBody = parse(body)?;
            ApiResponse::created(store.create_chapter(id, &b.title)?)
        }
        ("PUT", ["projects", id, "chapters", "order"]) => {
            let b: OrderBody = parse(body)?;
            store.reorder_chapters(id, &b.ids)?;
            ApiResponse::no_content()
        }
        ("GET", ["projects", id, "export", "markdown"]) => {
            let name = store.get_project(id)?.name;
            Ok(attachment(&name, "md", "text/markdown; charset=utf-8", store.export_markdown(id)?))
        }
        ("GET", ["projects", id, "export", "json"]) => {
            let name = store.get_project(id)?.name;
            let text = serde_json::to_string_pretty(&store.export_json(id)?).map_err(|e| AppError::Internal(e.to_string()))?;
            Ok(attachment(&name, "json", "application/json; charset=utf-8", text))
        }
        // ---- 参照ファイル（世界観・設定資料）
        ("GET", ["projects", id, "files"]) => {
            store.get_project(id)?;
            ApiResponse::ok(store.list_files(id)?)
        }
        ("POST", ["projects", id, "files"]) => {
            let b: CreateFile = parse(body)?;
            ApiResponse::created(store.create_file(id, &b.name, &b.content, b.pinned)?)
        }
        ("GET", ["files", id]) => ApiResponse::ok(store.get_file(id)?),
        ("PATCH", ["files", id]) => {
            let b: UpdateFile = parse(body)?;
            ApiResponse::ok(store.update_file(id, b.base_revision, FilePatch { name: b.name, content: b.content, pinned: b.pinned })?)
        }
        ("DELETE", ["files", id]) => {
            store.delete_file(id)?;
            ApiResponse::no_content()
        }

        // ---- キャラクター・人物の情報・関係
        ("GET", ["projects", id, "characters"]) => ApiResponse::ok(store.list_characters(id)?),
        ("POST", ["projects", id, "characters"]) => ApiResponse::created(store.create_character(id, parse(body)?)?),
        ("GET", ["characters", id]) => ApiResponse::ok(store.get_character(id)?),
        ("PATCH", ["characters", id]) => ApiResponse::ok(store.update_character(id, parse(body)?)?),
        ("DELETE", ["characters", id]) => {
            store.delete_character(id)?;
            ApiResponse::no_content()
        }
        ("GET", ["characters", id, "facts"]) => ApiResponse::ok(store.list_facts(id)?),
        ("POST", ["characters", id, "facts"]) => ApiResponse::created(store.create_fact(id, parse(body)?)?),
        ("PATCH", ["character-facts", id]) => ApiResponse::ok(store.update_fact(id, parse(body)?)?),
        ("DELETE", ["character-facts", id]) => {
            store.delete_fact(id)?;
            ApiResponse::no_content()
        }
        ("GET", ["projects", id, "character-relations"]) => ApiResponse::ok(store.list_relations(id)?),
        ("POST", ["projects", id, "character-relations"]) => ApiResponse::created(store.create_relation(id, parse(body)?)?),
        ("PATCH", ["character-relations", id]) => ApiResponse::ok(store.update_relation(id, parse(body)?)?),
        ("DELETE", ["character-relations", id]) => {
            store.delete_relation(id)?;
            ApiResponse::no_content()
        }

        // ---- 編集履歴・スナップショット
        ("GET", ["projects", id, "history"]) => {
            let scene = query_param(query, "scene_id").filter(|v| !v.is_empty());
            let before = query_param(query, "before").and_then(|v| v.parse().ok());
            let limit = query_param(query, "limit").and_then(|v| v.parse().ok()).unwrap_or(100);
            ApiResponse::ok(store.list_history(id, scene.as_deref(), before, limit)?)
        }
        ("GET", ["settings", "history"]) => ApiResponse::ok(json!({ "max_entries": store.history_limit()? })),
        ("PUT", ["settings", "history"]) => {
            let b: HistorySettings = parse(body)?;
            store.set_history_limit(b.max_entries)?;
            ApiResponse::ok(json!({ "max_entries": store.history_limit()? }))
        }
        ("GET", ["projects", id, "snapshots"]) => ApiResponse::ok(store.list_snapshots(id)?),
        ("POST", ["projects", id, "snapshots"]) => {
            let b: SnapshotBody = parse(body)?;
            ApiResponse::created(store.create_snapshot(id, &b.note, scenote_core::history::SNAPSHOT_MANUAL)?)
        }
        ("DELETE", ["snapshots", id]) => {
            store.delete_snapshot(id)?;
            ApiResponse::no_content()
        }
        ("GET", ["snapshots", id, "compare"]) => ApiResponse::ok(store.compare_snapshot(id)?),
        ("POST", ["snapshots", id, "restore"]) => ApiResponse::ok(store.restore_snapshot(id)?),

        // 未対応の一覧は、画面が空の一覧として扱えるよう空で返す。
        ("GET", ["projects", id, "chats" | "variables"]) => {
            store.get_project(id)?;
            ApiResponse::ok(json!([]))
        }

        // ---- 章
        ("PATCH", ["chapters", id]) => {
            let b: TitleBody = parse(body)?;
            ApiResponse::ok(store.rename_chapter(id, &b.title)?)
        }
        ("DELETE", ["chapters", id]) => {
            store.delete_chapter(id)?;
            ApiResponse::no_content()
        }
        ("POST", ["chapters", id, "scenes"]) => {
            let b: CreateSceneBody = parse(body)?;
            ApiResponse::created(store.create_scene(id, &b.title, &b.summary, b.after.as_deref())?)
        }
        ("PUT", ["chapters", id, "scenes", "order"]) => {
            let b: OrderBody = parse(body)?;
            store.reorder_scenes(id, &b.ids)?;
            ApiResponse::no_content()
        }

        // ---- 場面
        ("GET", ["scenes", id]) => ApiResponse::ok(store.scene_detail(id)?),
        ("PATCH", ["scenes", id]) => {
            let b: UpdateSceneBody = parse(body)?;
            let mut scene = store.get_scene(id)?;
            if let Some(summary) = b.summary {
                let base = b
                    .base_revision
                    .ok_or_else(|| AppError::Invalid("概要を変更するには base_revision を指定してください".into()))?;
                scene = store.update_scene_summary(id, base, &summary)?;
            }
            if let Some(title) = b.title {
                scene = store.rename_scene(id, &title)?;
            }
            ApiResponse::ok(scene)
        }
        ("DELETE", ["scenes", id]) => {
            store.delete_scene(id)?;
            ApiResponse::no_content()
        }
        ("POST", ["scenes", id, "elements"]) => {
            let b: CreateElement = parse(body)?;
            let element = store.create_element(id, b.content, b.position, b.character_id.as_deref())?;
            element_response(store, 201, element)
        }
        ("PUT", ["scenes", id, "elements", "order"]) => {
            let b: OrderBody = parse(body)?;
            store.reorder_elements(id, &b.ids)?;
            ApiResponse::no_content()
        }
        ("GET", ["scenes", id, "markdown"]) => {
            let (scene, markdown) = store.scene_markdown(id)?;
            ApiResponse::ok(json!({ "revision": scene.revision, "markdown": markdown }))
        }
        ("PUT", ["scenes", id, "markdown"]) => {
            let b: MarkdownBody = parse(body)?;
            ApiResponse::ok(store.replace_scene_markdown(id, b.base_revision, &b.markdown)?)
        }

        // ---- 要素
        ("PATCH", ["elements", id]) => {
            let b: UpdateElement = parse(body)?;
            let patch = ElementPatch { kind: b.kind, speaker: b.speaker, body: b.body, attrs: b.attrs, character_id: b.character_id };
            let element = store.update_element(id, b.base_revision, patch)?;
            element_response(store, 200, element)
        }
        ("DELETE", ["elements", id]) => {
            store.delete_element(id)?;
            ApiResponse::no_content()
        }

        // ---- MCP で接続しているクライアント（設定画面から解除する）
        ("GET", ["mcp", "clients"]) => ApiResponse::ok(crate::oauth::list_clients(&store.db)?),
        ("DELETE", ["mcp", "clients", id]) => {
            crate::oauth::revoke_client(&store.db, id)?;
            ApiResponse::no_content()
        }

        // PC 版にあって、第1段階では未対応の機能（AI・参照ファイル・履歴・嗜好・端末管理など）。
        _ if is_known_pc_route(&segments) => Err(AppError::Unavailable),
        _ => Err(AppError::NotFound("API")),
    }
}

/// PC 版に存在する API か（未対応の機能を「見つからない」ではなく「未対応」と伝えるため）。
fn is_known_pc_route(segments: &[&str]) -> bool {
    matches!(
        segments.first().copied(),
        Some(
            "auth" | "devices" | "projects" | "files" | "chats" | "proposals" | "settings" | "snapshots" | "preferences"
                | "characters" | "character-facts" | "character-relations" | "variables" | "usage"
        )
    )
}
