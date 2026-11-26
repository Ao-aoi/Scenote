# Cloudflare 経由で公開する

Scenote は PC 上の Rust サーバー（axum）と SQLite で動き、Gemini の API キーも PC 側で保持する（CLAUDE.md 第11節）。
そのため Cloudflare Pages / Workers へアプリ本体を載せることはできない（SQLite のファイル・常駐プロセス・WebSocket の状態を持てない）。
本書では **Cloudflare Tunnel** で PC のサーバーを外部の HTTPS URL に公開する方法を示す。データとキーは PC から出ない。

## 1. 前提

- Cloudflare アカウントと、Cloudflare で管理しているドメイン（例: `example.com`）
- PC で Scenote をビルド済み（`web/` で `npm run build`、`server/` で `cargo build --release`）

## 2. サーバーの起動

トンネルは PC 内から `127.0.0.1` へ接続するため、`SCENOTE_LAN=1` は不要。

```powershell
cd server
cargo run --release   # 127.0.0.1:8787 で待ち受け
```

## 3. cloudflared の設定

```powershell
winget install --id Cloudflare.cloudflared
cloudflared tunnel login
cloudflared tunnel create scenote
cloudflared tunnel route dns scenote scenote.example.com
```

`%USERPROFILE%\.cloudflared\config.yml`:

```yaml
tunnel: scenote
credentials-file: C:\Users\<ユーザー名>\.cloudflared\<トンネルID>.json

ingress:
  - hostname: scenote.example.com
    service: http://127.0.0.1:8787
  - service: http_status:404
```

起動（常駐させる場合は `cloudflared service install`）:

```powershell
cloudflared tunnel run scenote
```

注意: `originRequest.httpHostHeader` で Host を `localhost` に書き換えないこと。書き換えると外部からの接続が PC 本体として扱われ、認証が無効になる。

## 4. 認証の扱い

トンネル経由の要求は Host が `scenote.example.com` になるため、Scenote は「PC 本体以外の端末」として扱う（`server/src/auth.rs` の `is_local_request`）。

1. PC のブラウザで `http://localhost:8787` を開き、「端末」画面でペアリングコードを発行する。
2. iPad などで `https://scenote.example.com` を開き、コードを入力する。

Origin と Host の照合（CSRF・WebSocket）は、Cloudflare が Host をそのまま転送するため変更不要。WebSocket（`/api/events`）も Tunnel でそのまま通る。

## 5. 推奨: Cloudflare Access を前段に置く

インターネットへ公開すると、ペアリング画面が誰からも見える状態になる。Zero Trust ダッシュボードの
**Access → Applications → Self-hosted** で `scenote.example.com` を登録し、自分のメールアドレスだけを許可するポリシーを設定することを推奨する。
Scenote 自身のペアリング認証と二重になる。

## 6. アイコン

`web/public/` のアイコン（`icon.svg`、`favicon-32.png`、`apple-touch-icon.png`、`icon-192.png`、`icon-512.png`）と
`manifest.webmanifest` はビルド時に `web/dist/` へコピーされ、iPad の「ホーム画面に追加」でも使われる。
