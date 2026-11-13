use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use scenote::api::{self, AppState};
use scenote::llm::GeminiProvider;
use scenote::store::Store;

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

#[tokio::main]
async fn main() {
    let db_path = PathBuf::from(env_or("SCENOTE_DB", "data/scenote.db"));
    let web_dir = PathBuf::from(env_or("SCENOTE_WEB_DIR", "web/dist"));
    // 認証を実装するまで（Phase 4）は、既定でこの PC からの接続のみ受け付ける。
    let addr: SocketAddr = match env_or("SCENOTE_ADDR", "127.0.0.1:8787").parse() {
        Ok(addr) => addr,
        Err(_) => {
            eprintln!("SCENOTE_ADDR の形式が不正です（例: 127.0.0.1:8787）");
            std::process::exit(2);
        }
    };

    let store = match Store::open(&db_path) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("データベースを開けません ({}): {e:?}", db_path.display());
            std::process::exit(1);
        }
    };
    if !web_dir.join("index.html").exists() {
        eprintln!(
            "注意: {} に Web UI がありません。`npm run build`（web ディレクトリ）を実行してください。",
            web_dir.display()
        );
    }

    // API キーは環境変数 GEMINI_API_KEY、または画面の「AI 設定」から登録する（PC 側にのみ保存）。
    let env_api_key = std::env::var("GEMINI_API_KEY").ok();
    let state = AppState::new(store, Arc::new(GeminiProvider::new()), env_api_key);
    let app = api::router(state, Some(&web_dir));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("{addr} で待ち受けできません: {e}");
            std::process::exit(1);
        }
    };
    println!("Scenote を起動しました: http://{addr}");
    println!("データベース: {}", db_path.display());
    let shutdown = async {
        tokio::signal::ctrl_c().await.ok();
    };
    if let Err(e) = axum::serve(listener, app).with_graceful_shutdown(shutdown).await {
        eprintln!("サーバーエラー: {e}");
        std::process::exit(1);
    }
}
