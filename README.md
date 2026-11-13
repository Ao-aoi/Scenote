# Scenote

LLM とユーザーが共同でゲームシナリオを構成・執筆するための、個人利用向けシナリオ制作アプリ。
仕様と開発方針は [CLAUDE.md](CLAUDE.md) を参照。

現在の実装範囲は **Phase 1（最小のシナリオエディタ）** である。

- プロジェクトの作成・選択・設定変更・削除（テンプレートの複数選択）
- 章・場面の作成、名前変更、並べ替え、削除
- シナリオ要素（地の文・台詞・動作・演出・選択肢）の作成、編集、並べ替え、削除
- 場面単位の Markdown ソース編集（[記法](docs/markdown-format.md)）
- 自動保存（SQLite）と、同時編集時の競合検出
- プロジェクト全体の Markdown / JSON 書き出し

AI チャット・提案レビュー（Phase 2）以降は未実装。

## 構成

| ディレクトリ | 内容 |
|---|---|
| `server/` | Rust（axum + SQLite）のローカルサーバー。データ管理・Markdown 変換・HTTP API |
| `web/` | TypeScript + Vite の Web UI（実行時の依存ライブラリなし） |
| `docs/` | 仕様書（Markdown 記法など） |

## 必要なもの

- Rust（stable）
- Node.js 20 以降

## 起動方法

リポジトリのルートで実行する。

```bash
cd web
npm install
npm run build
cd ..
cargo run --release --manifest-path server/Cargo.toml
```

ブラウザで <http://127.0.0.1:8787> を開く。データは `data/scenote.db` に保存される。

| 環境変数 | 既定値 | 説明 |
|---|---|---|
| `SCENOTE_ADDR` | `127.0.0.1:8787` | 待ち受けアドレス |
| `SCENOTE_DB` | `data/scenote.db` | SQLite データベースのパス |
| `SCENOTE_WEB_DIR` | `web/dist` | ビルド済み Web UI のディレクトリ |

> **注意**: 認証は Phase 4 で実装する。それまでは `SCENOTE_ADDR` を既定値（この PC からのみ接続可能）のまま使うこと。
> `0.0.0.0` などに変更すると、同じネットワーク上の誰でもシナリオを読み書きできる状態になる。

## 開発

```bash
# サーバーのテスト
cargo test --manifest-path server/Cargo.toml

# Web UI の開発サーバー（API は 127.0.0.1:8787 へ中継される。別ターミナルでサーバーを起動しておく）
cd web
npm run dev
```
