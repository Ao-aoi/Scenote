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
    // 既定ではこの PC からの接続のみ受け付ける。SCENOTE_LAN=1 で同じ LAN の端末（iPad など）にも公開する。
    let lan = std::env::var("SCENOTE_LAN").is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    let default_addr = if lan { "0.0.0.0:8787" } else { "127.0.0.1:8787" };
    let addr: SocketAddr = match env_or("SCENOTE_ADDR", default_addr).parse() {
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
    // 接続先は通常変更不要。検証用の模擬サーバーや中継サーバーを使う場合のみ指定する。
    let provider = match std::env::var("SCENOTE_GEMINI_BASE_URL") {
        Ok(url) if !url.trim().is_empty() => GeminiProvider::with_base_url(url.trim()),
        _ => GeminiProvider::new(),
    };
    let lan_urls = lan_urls(addr);
    let state = AppState::new(store, Arc::new(provider), env_api_key).with_lan_urls(lan_urls.clone());
    let app = api::router(state, Some(&web_dir));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("{addr} で待ち受けできません: {e}");
            std::process::exit(1);
        }
    };
    let local_port = addr.port();
    println!("Scenote を起動しました: http://localhost:{local_port}");
    if lan_urls.is_empty() {
        println!("（この PC からのみ接続できます。iPad から使う場合は SCENOTE_LAN=1 を付けて起動してください）");
    } else {
        for url in &lan_urls {
            println!("同じ LAN の端末からの接続先: {url}");
        }
        println!("端末を接続するには、PC の画面の「設定」→「端末の接続」でペアリングコードを発行してください。");
    }
    println!("データベース: {}", db_path.display());
    let shutdown = async {
        tokio::signal::ctrl_c().await.ok();
    };
    // 接続元のアドレスを認証に使うため、ConnectInfo を有効にする。
    let service = app.into_make_service_with_connect_info::<SocketAddr>();
    if let Err(e) = axum::serve(listener, service).with_graceful_shutdown(shutdown).await {
        eprintln!("サーバーエラー: {e}");
        std::process::exit(1);
    }
}

/// LAN の端末から接続するための URL。ループバックでのみ待ち受ける場合は空。
fn lan_urls(addr: SocketAddr) -> Vec<String> {
    let ip = addr.ip();
    if ip.is_loopback() {
        return Vec::new();
    }
    let host = if ip.is_unspecified() {
        // 既定の経路で使われる自分のアドレスを調べる（UDP の connect はパケットを送らない）。
        let probe = std::net::UdpSocket::bind("0.0.0.0:0").and_then(|s| {
            s.connect("8.8.8.8:80")?;
            s.local_addr()
        });
        match probe {
            Ok(local) if !local.ip().is_unspecified() && !local.ip().is_loopback() => local.ip(),
            _ => return vec![format!("http://<この PC の IP アドレス>:{}", addr.port())],
        }
    } else {
        ip
    };
    match host {
        std::net::IpAddr::V6(v6) => vec![format!("http://[{v6}]:{}", addr.port())],
        v4 => vec![format!("http://{v4}:{}", addr.port())],
    }
}
