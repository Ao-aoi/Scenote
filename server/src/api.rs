//! HTTP API。画面からの読み書きはすべてここを通る。

use std::path::Path as FsPath;
use std::sync::{Arc, Mutex, MutexGuard};

use axum::extract::{Path, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use tower_http::services::{ServeDir, ServeFile};

use crate::error::{AppError, Result};
use crate::model::*;
use crate::store::{ElementPatch, ProjectPatch, Store};

#[derive(Clone)]
pub struct AppState {
    store: Arc<Mutex<Store>>,
}

impl AppState {
    pub fn new(store: Store) -> Self {
        AppState {
            store: Arc::new(Mutex::new(store)),
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
