# Cloudflare 版のデプロイ

PC を使わずに iPhone・iPad から使うための版。`cloudflare/` にある。

- **Worker**（Rust、workers-rs）: `/api/` を処理する。画面（`web/dist`）は静的アセットとして配信する。
- **Durable Object**（`ScenoteData`）: 内蔵の SQLite にすべてのデータを保存する。インスタンスは1つ。
- **ログイン**: Cloudflare Access。Worker は Access の署名つきトークンを検証し、通らない要求を拒否する。
- PC 版と同じ処理（データモデル・Markdown 変換・入力検証・スキーマ）は `core/` を共有する。

## 第1段階で使える機能

プロジェクト・章・場面・要素の編集、自動保存と競合検出、Markdown 編集、分岐の設定、
Markdown・JSON の書き出し、端末間の変更通知、MCP での AI の接続（後述）。かんたんモードの入力欄は、AI を使わず
本文の末尾へそのまま追加する（`**名前**: 台詞` の行は台詞になる）。

AI（チャット・提案・作品づくり・嗜好学習）、参照ファイル、編集履歴・スナップショット、
キャラクター・変数・構造チェックは未対応で、画面に表示しない。編集履歴はまだ記録しない。

## 必要なもの

- Cloudflare アカウントと、Cloudflare で管理しているドメイン（例: `example.com`）
- Workers の有料プランは不要（Durable Objects の SQLite は無料プランで使える）
- Rust（`rustup target add wasm32-unknown-unknown`）、`cargo install worker-build`、Node.js

## 1. Cloudflare Access の設定

1. ダッシュボードの **Zero Trust → Access → Applications → Add an application → Self-hosted**。
2. Application domain に `scenote.example.com` を入力する。
3. ポリシーで **Include → Emails** に自分のメールアドレスだけを指定する。
4. 保存後、アプリケーションの **Overview** にある **Application Audience (AUD) Tag** を控える。
5. **Settings → Custom Pages** などに表示されるチームのドメイン（`<チーム名>.cloudflareaccess.com`）を控える。

## 2. 設定ファイル

`cloudflare/wrangler.toml` を編集する。

```toml
routes = [{ pattern = "scenote.example.com", custom_domain = true }]

[vars]
ACCESS_TEAM_DOMAIN = "<チーム名>.cloudflareaccess.com"
ACCESS_AUD = "<AUD タグ>"
ALLOWED_EMAILS = "you@example.com"   # 任意（Access のポリシーに加えて確認）
```

`workers_dev = false` のままにする（Access で保護していない URL を作らないため）。

## 3. デプロイ

```sh
cd cloudflare
npm install
npx wrangler login
npm run deploy      # web/ のビルド → Worker のビルド → デプロイ
```

## 4. MCP の経路を Access の対象から外す

MCP クライアント（Claude・ChatGPT）は Access のログイン画面を通れないため、次の経路だけは Access を
通さない設定にする。これらの経路は Scenote 自身が OAuth のトークンで保護する。

Zero Trust → Access → Applications で、同じドメインにもう1つ **Self-hosted** のアプリケーションを作り、
次のパスを登録して、ポリシーを **Action: Bypass / Include: Everyone** にする。

| パス | 用途 |
|---|---|
| `scenote.example.com/mcp` | MCP の本体 |
| `scenote.example.com/oauth/register` | クライアントの登録 |
| `scenote.example.com/oauth/token` | トークンの発行 |
| `scenote.example.com/.well-known/*` | 認可サーバーの情報 |

`/oauth/authorize`（接続の許可の画面）は Access で保護したままにする。ここで本人を確認してから許可を求める。

## 6. iPhone のホーム画面に追加

Safari で `https://scenote.example.com` を開いて Access でログインし、共有ボタン →
**ホーム画面に追加**。スマートフォンでは自動でかんたんモードになる。

Access のログインの有効期間（Application の **Session Duration**）を長め（例: 1か月）にすると、
ログインし直す回数が減る。

## 5. Claude・ChatGPT から接続する（MCP）

Scenote の **設定 → AI の接続（MCP）** に、登録する URL（`https://scenote.example.com/mcp`）が表示される。

- **Claude**: 設定 → コネクタ → カスタムコネクタを追加 → URL を入力。Claude のアプリ（iPhone を含む）からも使える。
- **ChatGPT**: 設定 → コネクタ（開発者モード）→ 作成 → URL を入力し、認証に OAuth を選ぶ。

追加すると Scenote の許可の画面が開く（未ログインなら先に Cloudflare Access のログイン）。**許可する** を押すと接続される。

接続した AI は、すべてのプロジェクトの読み取りと、承認を経ない直接の書き換えができる（全体の流れの検討・
人物関係の整理などのプロデュースを任せるため）。書き換えた本文の由来は `ai_direct` になる。
第1段階では編集履歴が記録されないため、大きな書き換えを頼む前に **書き出し → JSON** で保存しておくとよい。
接続は同じ画面から解除でき、解除するとそのクライアントのトークンはすべて無効になる。

## ローカルでの確認

```sh
cd cloudflare
echo 'SCENOTE_DEV_USER=dev@example.com' > .dev.vars   # localhost でだけ Access を省く
npm run dev                                         # http://127.0.0.1:8787
node tests/smoke.mjs http://127.0.0.1:8787          # API の通し確認
node tests/mcp.mjs http://127.0.0.1:8787            # OAuth と MCP の通し確認
```

## データのバックアップ

画面の **書き出し → JSON** でプロジェクト全体を保存できる。Durable Object の SQLite は
Cloudflare 側で保持され、30日以内の任意の時点へ戻せる（Point-in-time recovery）。

## 補足: PC 版を外部から使う場合

PC 版のサーバーをそのまま外から使いたい場合は、Cloudflare Tunnel（`cloudflared`）で
`http://127.0.0.1:8787` を公開できる。その場合も Access で保護し、`httpHostHeader` で
Host を `localhost` に書き換えないこと（書き換えると PC 本体として扱われ、認証が無効になる）。
