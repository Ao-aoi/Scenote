//! HTTP API。画面からの読み書きはすべてここを通る。

use std::path::Path as FsPath;
use std::sync::{Arc, Mutex, MutexGuard};

use axum::extract::{Path, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use tower_http::services::{ServeDir, ServeFile};

use crate::error::{AppError, Result};
use crate::model::*;
use crate::assistant::{self, Exchange, SendRequest};
use crate::history::{CompareItem, HistoryEntry, SnapshotInfo, SNAPSHOT_MANUAL};
use crate::llm::LlmProvider;
use crate::preferences::{self, AnalysisResult, Profile, Rule, Suggestion};
use crate::store::{ElementPatch, ProjectPatch, Store};
use crate::store_ai::{
    Chat, ChatDetail, ChatPatch, FilePatch, LlmSettings, Proposal, ReferenceFile, UsageStatus,
};

#[derive(Clone)]
pub struct AppState {
    store: Arc<Mutex<Store>>,
    provider: Arc<dyn LlmProvider>,
    /// 環境変数で与えられた API キー（設定画面での登録より優先する）。
    env_api_key: Option<Arc<str>>,
}

impl AppState {
    pub fn new(store: Store, provider: Arc<dyn LlmProvider>, env_api_key: Option<String>) -> Self {
        AppState {
            store: Arc::new(Mutex::new(store)),
            provider,
            env_api_key: env_api_key.filter(|k| !k.trim().is_empty()).map(Arc::from),
        }
    }

    fn store(&self) -> MutexGuard<'_, Store> {
        // 他のリクエスト処理中のパニックで毒化していても、DB 自体はトランザクションで保護されている。
        self.store.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            AppError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            AppError::Invalid(_) => (StatusCode::BAD_REQUEST, "invalid"),
            AppError::Conflict { .. } => (StatusCode::CONFLICT, "conflict"),
            AppError::Llm(_) => (StatusCode::BAD_GATEWAY, "llm"),
            AppError::LlmStopped => (StatusCode::TOO_MANY_REQUESTS, "llm_stopped"),
            AppError::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        };
        if let AppError::Internal(detail) = &self {
            // 詳細はサーバー側のログにのみ出す（創作データや認証情報は含めない）。
            eprintln!("internal error: {detail}");
        }
        let mut error = json!({ "code": code, "message": self.to_string() });
        if let AppError::Conflict { current } = &self {
            error["current_revision"] = json!(current);
        }
        (status, Json(json!({ "error": error }))).into_response()
    }
}

/// API ルーターを組み立てる。`web_dir` を渡すとビルド済みの Web UI も配信する。
pub fn router(state: AppState, web_dir: Option<&FsPath>) -> Router {
    let api = Router::new()
        .route("/meta", get(meta))
        .route("/projects", get(list_projects).post(create_project))
        .route(
            "/projects/{id}",
            get(project_tree).patch(update_project).delete(delete_project),
        )
        .route("/projects/{id}/chapters", post(create_chapter))
        .route("/projects/{id}/chapters/order", put(reorder_chapters))
        .route("/projects/{id}/export/markdown", get(export_markdown))
        .route("/projects/{id}/export/json", get(export_json))
        .route("/chapters/{id}", patch(rename_chapter).delete(delete_chapter))
        .route("/chapters/{id}/scenes", post(create_scene))
        .route("/chapters/{id}/scenes/order", put(reorder_scenes))
        .route(
            "/scenes/{id}",
            get(scene_detail).patch(rename_scene).delete(delete_scene),
        )
        .route("/scenes/{id}/elements", post(create_element))
        .route("/scenes/{id}/elements/order", put(reorder_elements))
        .route("/scenes/{id}/markdown", get(scene_markdown).put(replace_scene_markdown))
        .route("/elements/{id}", patch(update_element).delete(delete_element))
        .route("/projects/{id}/files", get(list_files).post(create_file))
        .route("/files/{id}", get(get_file).patch(update_file).delete(delete_file))
        .route("/projects/{id}/chats", get(list_chats).post(create_chat))
        .route("/chats/{id}", get(chat_detail).patch(update_chat).delete(delete_chat))
        .route("/chats/{id}/messages", post(send_message))
        .route("/proposals/{id}/approve", post(approve_proposal))
        .route("/proposals/{id}/reject", post(reject_proposal))
        .route("/settings/llm", get(llm_settings).put(update_llm_settings))
        .route("/settings/llm/models", get(llm_models))
        .route("/proposals/{id}/approve-edited", post(approve_proposal_edited))
        .route("/projects/{id}/history", get(project_history))
        .route("/settings/history", get(history_settings).put(update_history_settings))
        .route("/projects/{id}/snapshots", get(list_snapshots).post(create_snapshot))
        .route("/snapshots/{id}", delete(delete_snapshot))
        .route("/snapshots/{id}/compare", get(compare_snapshot))
        .route("/snapshots/{id}/restore", post(restore_snapshot))
        .route("/projects/{id}/preferences", get(preference_profile))
        .route("/projects/{id}/preferences/analyze", post(analyze_preferences))
        .route("/projects/{id}/preferences/import", post(import_preferences))
        .route("/projects/{id}/preferences/rules", post(create_project_rule))
        .route("/preferences/global/rules", post(create_global_rule))
        .route("/preferences/rules/{id}", patch(update_rule).delete(delete_rule))
        .route("/preferences/rules/{id}/copy", post(copy_rule))
        .route("/preferences/suggestions/{id}/approve", post(approve_suggestion))
        .route("/preferences/suggestions/{id}/reject", post(reject_suggestion))
        .route("/settings/preferences", put(update_preference_settings))
        .route("/usage", get(usage))
        .route("/usage/resume", post(resume_usage))
        .fallback(|| async { AppError::NotFound("API") })
        .with_state(state);

    let app = Router::new().nest("/api", api);
    match web_dir {
        Some(dir) => app.fallback_service(
            ServeDir::new(dir).fallback(ServeFile::new(dir.join("index.html"))),
        ),
        None => app,
    }
}

async fn meta() -> Json<Value> {
    Json(json!({
        "schema_version": SCHEMA_VERSION,
        "element_kinds": BASIC_KINDS,
        "templates": TEMPLATES,
    }))
}

// ---- プロジェクト -----------------------------------------------------

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

async fn list_projects(State(s): State<AppState>) -> Result<Json<Vec<Project>>> {
    Ok(Json(s.store().list_projects()?))
}

async fn create_project(
    State(s): State<AppState>,
    Json(req): Json<CreateProject>,
) -> Result<(StatusCode, Json<Project>)> {
    let project = s
        .store()
        .create_project(&req.name, &req.description, &req.templates)?;
    Ok((StatusCode::CREATED, Json(project)))
}

async fn project_tree(State(s): State<AppState>, Path(id): Path<String>) -> Result<Json<ProjectTree>> {
    Ok(Json(s.store().project_tree(&id)?))
}

async fn update_project(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateProject>,
) -> Result<Json<Project>> {
    let patch = ProjectPatch {
        name: req.name,
        description: req.description,
        templates: req.templates,
    };
    Ok(Json(s.store().update_project(&id, patch)?))
}

async fn delete_project(State(s): State<AppState>, Path(id): Path<String>) -> Result<StatusCode> {
    s.store().delete_project(&id)?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- 章・場面 ---------------------------------------------------------

#[derive(Deserialize)]
struct TitleBody {
    title: String,
}

#[derive(Deserialize)]
struct OrderBody {
    ids: Vec<String>,
}

async fn create_chapter(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<TitleBody>,
) -> Result<(StatusCode, Json<Chapter>)> {
    Ok((StatusCode::CREATED, Json(s.store().create_chapter(&id, &req.title)?)))
}

async fn rename_chapter(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<TitleBody>,
) -> Result<Json<Chapter>> {
    Ok(Json(s.store().rename_chapter(&id, &req.title)?))
}

async fn delete_chapter(State(s): State<AppState>, Path(id): Path<String>) -> Result<StatusCode> {
    s.store().delete_chapter(&id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn reorder_chapters(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<OrderBody>,
) -> Result<StatusCode> {
    s.store().reorder_chapters(&id, &req.ids)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn create_scene(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<TitleBody>,
) -> Result<(StatusCode, Json<Scene>)> {
    Ok((StatusCode::CREATED, Json(s.store().create_scene(&id, &req.title)?)))
}

async fn scene_detail(State(s): State<AppState>, Path(id): Path<String>) -> Result<Json<SceneDetail>> {
    Ok(Json(s.store().scene_detail(&id)?))
}

async fn rename_scene(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<TitleBody>,
) -> Result<Json<Scene>> {
    Ok(Json(s.store().rename_scene(&id, &req.title)?))
}

async fn delete_scene(State(s): State<AppState>, Path(id): Path<String>) -> Result<StatusCode> {
    s.store().delete_scene(&id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn reorder_scenes(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<OrderBody>,
) -> Result<StatusCode> {
    s.store().reorder_scenes(&id, &req.ids)?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- 要素 -------------------------------------------------------------

#[derive(Deserialize)]
struct CreateElement {
    #[serde(flatten)]
    content: ElementContent,
    position: Option<i64>,
}

#[derive(Deserialize)]
struct UpdateElement {
    base_revision: i64,
    kind: Option<String>,
    speaker: Option<String>,
    body: Option<String>,
}

/// 要素の変更結果。場面のリビジョンも進むため、併せて返す。
fn element_response(store: &Store, element: Element) -> Result<Json<Value>> {
    let scene_revision = store.get_scene(&element.scene_id)?.revision;
    Ok(Json(json!({ "element": element, "scene_revision": scene_revision })))
}

async fn create_element(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<CreateElement>,
) -> Result<(StatusCode, Json<Value>)> {
    let mut store = s.store();
    let element = store.create_element(&id, req.content, req.position)?;
    Ok((StatusCode::CREATED, element_response(&store, element)?))
}

async fn update_element(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateElement>,
) -> Result<Json<Value>> {
    let patch = ElementPatch {
        kind: req.kind,
        speaker: req.speaker,
        body: req.body,
    };
    let mut store = s.store();
    let element = store.update_element(&id, req.base_revision, patch)?;
    element_response(&store, element)
}

async fn delete_element(State(s): State<AppState>, Path(id): Path<String>) -> Result<StatusCode> {
    s.store().delete_element(&id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn reorder_elements(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<OrderBody>,
) -> Result<StatusCode> {
    s.store().reorder_elements(&id, &req.ids)?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- Markdown / エクスポート ------------------------------------------

#[derive(Deserialize)]
struct MarkdownBody {
    base_revision: i64,
    markdown: String,
}

async fn scene_markdown(State(s): State<AppState>, Path(id): Path<String>) -> Result<Json<Value>> {
    let (scene, markdown) = s.store().scene_markdown(&id)?;
    Ok(Json(json!({ "revision": scene.revision, "markdown": markdown })))
}

async fn replace_scene_markdown(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<MarkdownBody>,
) -> Result<Json<SceneDetail>> {
    Ok(Json(s.store().replace_scene_markdown(
        &id,
        req.base_revision,
        &req.markdown,
    )?))
}

/// ダウンロード用のヘッダーを付ける。ファイル名はプロジェクト名（RFC 5987 形式）。
fn attachment(name: &str, ext: &str, content_type: &'static str, body: String) -> Response {
    let mut encoded = String::new();
    for b in format!("{name}.{ext}").bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.') {
            encoded.push(b as char);
        } else {
            encoded.push_str(&format!("%{b:02X}"));
        }
    }
    let disposition = format!("attachment; filename=\"scenote-export.{ext}\"; filename*=UTF-8''{encoded}");
    let mut response = body.into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    if let Ok(value) = HeaderValue::from_str(&disposition) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    response
}

async fn export_markdown(State(s): State<AppState>, Path(id): Path<String>) -> Result<Response> {
    let store = s.store();
    let name = store.get_project(&id)?.name;
    let body = store.export_markdown(&id)?;
    Ok(attachment(&name, "md", "text/markdown; charset=utf-8", body))
}

async fn export_json(State(s): State<AppState>, Path(id): Path<String>) -> Result<Response> {
    let store = s.store();
    let name = store.get_project(&id)?.name;
    let body = serde_json::to_string_pretty(&store.export_json(&id)?)?;
    Ok(attachment(&name, "json", "application/json; charset=utf-8", body))
}

// ---- 参照ファイル -----------------------------------------------------

#[derive(Deserialize)]
struct CreateFile {
    name: String,
    #[serde(default)]
    content: String,
}

#[derive(Deserialize)]
struct UpdateFile {
    base_revision: i64,
    name: Option<String>,
    content: Option<String>,
    pinned: Option<bool>,
}

async fn list_files(State(s): State<AppState>, Path(id): Path<String>) -> Result<Json<Vec<ReferenceFile>>> {
    let store = s.store();
    store.get_project(&id)?;
    Ok(Json(store.list_files(&id)?))
}

async fn create_file(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<CreateFile>,
) -> Result<(StatusCode, Json<ReferenceFile>)> {
    let file = s.store().create_file(&id, &req.name, &req.content)?;
    Ok((StatusCode::CREATED, Json(file)))
}

async fn get_file(State(s): State<AppState>, Path(id): Path<String>) -> Result<Json<ReferenceFile>> {
    Ok(Json(s.store().get_file(&id)?))
}

async fn update_file(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateFile>,
) -> Result<Json<ReferenceFile>> {
    let patch = FilePatch {
        name: req.name,
        content: req.content,
        pinned: req.pinned,
    };
    Ok(Json(s.store().update_file(&id, req.base_revision, patch)?))
}

async fn delete_file(State(s): State<AppState>, Path(id): Path<String>) -> Result<StatusCode> {
    s.store().delete_file(&id)?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- チャット・提案 ---------------------------------------------------

#[derive(Deserialize)]
struct UpdateChat {
    title: Option<String>,
    instructions: Option<String>,
    auto_select: Option<bool>,
    file_ids: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct SendMessage {
    content: String,
    scene_id: Option<String>,
    #[serde(default)]
    include_file_ids: Vec<String>,
    #[serde(default)]
    exclude_file_ids: Vec<String>,
}

#[derive(Deserialize, Default)]
struct RejectBody {
    #[serde(default)]
    reason: String,
}

async fn list_chats(State(s): State<AppState>, Path(id): Path<String>) -> Result<Json<Vec<Chat>>> {
    let store = s.store();
    store.get_project(&id)?;
    Ok(Json(store.list_chats(&id)?))
}

async fn create_chat(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<TitleBody>,
) -> Result<(StatusCode, Json<Chat>)> {
    Ok((StatusCode::CREATED, Json(s.store().create_chat(&id, &req.title)?)))
}

async fn chat_detail(State(s): State<AppState>, Path(id): Path<String>) -> Result<Json<ChatDetail>> {
    Ok(Json(s.store().chat_detail(&id)?))
}

async fn update_chat(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateChat>,
) -> Result<Json<Chat>> {
    let patch = ChatPatch {
        title: req.title,
        instructions: req.instructions,
        auto_select: req.auto_select,
        file_ids: req.file_ids,
    };
    Ok(Json(s.store().update_chat(&id, patch)?))
}

async fn delete_chat(State(s): State<AppState>, Path(id): Path<String>) -> Result<StatusCode> {
    s.store().delete_chat(&id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn send_message(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SendMessage>,
) -> Result<Json<Exchange>> {
    let request = SendRequest {
        content: req.content,
        scene_id: req.scene_id,
        include_file_ids: req.include_file_ids,
        exclude_file_ids: req.exclude_file_ids,
    };
    let exchange = assistant::send_message(
        &s.store,
        s.provider.as_ref(),
        s.env_api_key.as_deref(),
        &id,
        request,
    )
    .await?;
    Ok(Json(exchange))
}

async fn approve_proposal(State(s): State<AppState>, Path(id): Path<String>) -> Result<Json<Proposal>> {
    Ok(Json(s.store().approve_proposal(&id)?))
}

async fn reject_proposal(
    State(s): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<RejectBody>>,
) -> Result<Json<Proposal>> {
    let reason = body.map(|Json(b)| b.reason).unwrap_or_default();
    Ok(Json(s.store().reject_proposal(&id, &reason)?))
}

// ---- AI 設定・使用量 --------------------------------------------------

#[derive(Deserialize)]
struct UpdateLlmSettings {
    /// 省略時は変更なし、空文字で削除。応答には決して含めない。
    api_key: Option<String>,
    model: Option<String>,
    monthly_token_limit: Option<i64>,
    warn_percent: Option<i64>,
}

async fn llm_models(State(s): State<AppState>) -> Result<Json<Vec<crate::llm::LlmModel>>> {
    let api_key = {
        let store = s.store();
        match &s.env_api_key {
            Some(key) => key.to_string(),
            None => store.stored_api_key()?.ok_or_else(|| {
                AppError::Invalid("モデル一覧を取得するには Gemini API キーを設定してください".into())
            })?,
        }
    };
    let models = s.provider.list_models(&api_key).await.map_err(|e| AppError::Llm(e.0))?;
    Ok(Json(models))
}

async fn llm_settings(State(s): State<AppState>) -> Result<Json<LlmSettings>> {
    Ok(Json(s.store().llm_settings(s.env_api_key.is_some())?))
}

async fn update_llm_settings(
    State(s): State<AppState>,
    Json(req): Json<UpdateLlmSettings>,
) -> Result<Json<LlmSettings>> {
    let mut store = s.store();
    store.update_llm_settings(
        req.api_key.as_deref(),
        req.model.as_deref(),
        req.monthly_token_limit,
        req.warn_percent,
    )?;
    Ok(Json(store.llm_settings(s.env_api_key.is_some())?))
}

async fn usage(State(s): State<AppState>) -> Result<Json<UsageStatus>> {
    Ok(Json(s.store().usage_status()?))
}

async fn resume_usage(State(s): State<AppState>) -> Result<Json<UsageStatus>> {
    let mut store = s.store();
    store.resume_llm()?;
    Ok(Json(store.usage_status()?))
}

// ---- 編集履歴・スナップショット（Phase 3） ------------------------------

#[derive(Deserialize)]
struct ApproveEdited {
    content: Value,
}

async fn approve_proposal_edited(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<ApproveEdited>,
) -> Result<Json<Proposal>> {
    Ok(Json(s.store().approve_proposal_with(&id, Some(req.content))?))
}

#[derive(Deserialize)]
struct HistoryQuery {
    scene_id: Option<String>,
    before: Option<i64>,
    limit: Option<i64>,
}

async fn project_history(
    State(s): State<AppState>,
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<HistoryQuery>,
) -> Result<Json<Vec<HistoryEntry>>> {
    Ok(Json(s.store().list_history(&id, q.scene_id.as_deref(), q.before, q.limit.unwrap_or(100))?))
}

#[derive(Deserialize)]
struct HistorySettings {
    max_entries: i64,
}

async fn history_settings(State(s): State<AppState>) -> Result<Json<Value>> {
    Ok(Json(json!({ "max_entries": s.store().history_limit()? })))
}

async fn update_history_settings(State(s): State<AppState>, Json(req): Json<HistorySettings>) -> Result<Json<Value>> {
    let mut store = s.store();
    store.set_history_limit(req.max_entries)?;
    Ok(Json(json!({ "max_entries": store.history_limit()? })))
}

#[derive(Deserialize, Default)]
struct SnapshotBody {
    #[serde(default)]
    note: String,
}

async fn list_snapshots(State(s): State<AppState>, Path(id): Path<String>) -> Result<Json<Vec<SnapshotInfo>>> {
    Ok(Json(s.store().list_snapshots(&id)?))
}

async fn create_snapshot(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SnapshotBody>,
) -> Result<(StatusCode, Json<SnapshotInfo>)> {
    Ok((StatusCode::CREATED, Json(s.store().create_snapshot(&id, &req.note, SNAPSHOT_MANUAL)?)))
}

async fn delete_snapshot(State(s): State<AppState>, Path(id): Path<String>) -> Result<StatusCode> {
    s.store().delete_snapshot(&id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn compare_snapshot(State(s): State<AppState>, Path(id): Path<String>) -> Result<Json<Vec<CompareItem>>> {
    Ok(Json(s.store().compare_snapshot(&id)?))
}

/// 復元する。応答は、復元の直前に自動保存したスナップショット。
async fn restore_snapshot(State(s): State<AppState>, Path(id): Path<String>) -> Result<Json<SnapshotInfo>> {
    Ok(Json(s.store().restore_snapshot(&id)?))
}

// ---- 執筆嗜好（Phase 3） ------------------------------------------------

#[derive(Deserialize)]
struct RuleBody {
    text: String,
    #[serde(default = "default_category")]
    category: String,
}

fn default_category() -> String {
    "other".into()
}

#[derive(Deserialize)]
struct UpdateRuleBody {
    text: Option<String>,
    category: Option<String>,
    confidence: Option<f64>,
}

#[derive(Deserialize)]
struct CopyRuleBody {
    /// 複製先のプロジェクト。省略時は共通プロファイル。
    project_id: Option<String>,
}

#[derive(Deserialize)]
struct ImportBody {
    source_project_id: String,
}

#[derive(Deserialize)]
struct PreferenceSettingsBody {
    auto_update: Option<bool>,
    include_direct_edits: Option<bool>,
}

async fn preference_profile(State(s): State<AppState>, Path(id): Path<String>) -> Result<Json<Profile>> {
    Ok(Json(s.store().profile(&id)?))
}

async fn analyze_preferences(State(s): State<AppState>, Path(id): Path<String>) -> Result<Json<AnalysisResult>> {
    let result =
        preferences::analyze(&s.store, s.provider.as_ref(), s.env_api_key.as_deref(), &id).await?;
    Ok(Json(result))
}

async fn import_preferences(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<ImportBody>,
) -> Result<Json<Value>> {
    let copied = s.store().import_project_rules(&req.source_project_id, &id)?;
    Ok(Json(json!({ "copied": copied })))
}

async fn create_project_rule(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<RuleBody>,
) -> Result<(StatusCode, Json<Rule>)> {
    Ok((StatusCode::CREATED, Json(s.store().create_rule(Some(&id), &req.text, &req.category)?)))
}

async fn create_global_rule(State(s): State<AppState>, Json(req): Json<RuleBody>) -> Result<(StatusCode, Json<Rule>)> {
    Ok((StatusCode::CREATED, Json(s.store().create_rule(None, &req.text, &req.category)?)))
}

async fn update_rule(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateRuleBody>,
) -> Result<Json<Rule>> {
    Ok(Json(s.store().update_rule(&id, req.text.as_deref(), req.category.as_deref(), req.confidence)?))
}

async fn delete_rule(State(s): State<AppState>, Path(id): Path<String>) -> Result<StatusCode> {
    s.store().delete_rule(&id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn copy_rule(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<CopyRuleBody>,
) -> Result<Json<Value>> {
    let copied = s.store().copy_rules(&[id], req.project_id.as_deref())?;
    Ok(Json(json!({ "copied": copied })))
}

async fn approve_suggestion(State(s): State<AppState>, Path(id): Path<String>) -> Result<Json<Suggestion>> {
    Ok(Json(s.store().approve_suggestion(&id)?))
}

async fn reject_suggestion(State(s): State<AppState>, Path(id): Path<String>) -> Result<Json<Suggestion>> {
    Ok(Json(s.store().reject_suggestion(&id)?))
}

async fn update_preference_settings(
    State(s): State<AppState>,
    Json(req): Json<PreferenceSettingsBody>,
) -> Result<Json<preferences::PreferenceSettings>> {
    let mut store = s.store();
    store.update_preference_settings(req.auto_update, req.include_direct_edits)?;
    Ok(Json(store.preference_settings()?))
}
