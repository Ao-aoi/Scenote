# Cloudflare 版のデプロイ手順

PC を使わずに、iPhone・iPad・PC のブラウザから Scenote を使うための版。コードは `cloudflare/` にある。

## 0. 全体像

```
iPhone / iPad / PC のブラウザ ──┐
Claude・ChatGPT（MCP） ────────┤
                                ▼
          https://scenote.example.com（自分のドメイン）
                                │  Cloudflare Access（本人の確認）
                                ▼
          Worker（Rust）── 画面のファイル（web/dist）
                │
                ▼
          Durable Object「ScenoteData」── 内蔵の SQLite（すべてのデータ）
                │
                └── Gemini API（作品づくり・嗜好の分析。キーは Cloudflare の秘密情報）
```

- **データ**: Durable Object の SQLite に、PC 版と同じスキーマで保存する。
- **ログイン**: Cloudflare Access。許可したメールアドレスの本人だけが開ける。Worker 側でも Access の署名を検証する。
- **MCP**: `/mcp`。Claude・ChatGPT は OAuth で接続し、接続の許可は Access でログインした本人が行う。

### 使える機能

| 機能 | Cloudflare 版 |
|---|---|
| プロジェクト・章・場面・本文の編集、自動保存、競合検出、Markdown 編集、分岐の設定 | ○ |
| 書き出し（Markdown・JSON）、端末間の変更通知 | ○ |
| 編集履歴・スナップショット（比較・復元） | ○ |
| 参照ファイル（世界観・設定資料）、キャラクターの管理 | ○ |
| 変数・構造チェック（場面グラフ・相関図） | ○ |
| 作品づくり（AI）、執筆の嗜好（学習・自動分析）、AI 設定と使用量 | ○ |
| MCP での AI の接続（Claude・ChatGPT） | ○（Cloudflare 版のみ） |
| Scenote 内の AI チャットと提案、端末のペアリング | ×（未移植。ログインは Access が担う） |

かんたんモード（スマートフォン）の入力欄は、AI チャットがないため、入力をそのまま本文の末尾に追加する
（`**名前**: 台詞` の行は台詞になる）。

**嗜好の学習の材料**: Cloudflare 版には AI の提案がないため、主に「MCP で接続した AI が書いた本文を、
あなたが後から直した・消した操作」を材料にする（編集履歴から集める）。設定で有効にすれば、直接入力した本文も材料にできる。

### 費用の目安

- Cloudflare: Workers の無料プランで動く（Durable Objects の SQLite と Access も無料枠あり）。個人利用なら通常は無料枠に収まる。
- ドメイン: 年 1,000〜2,000 円程度（Cloudflare Registrar などで取得）。すでに持っていれば不要。
- Gemini API: 無料枠あり。Scenote の「AI 設定・使用量」で月の上限を設定でき、上限に達すると自動で止まる。

料金と無料枠は変わることがあるため、各サービスの公式の料金ページで確認すること。

---

## 1. 準備するもの

1. **Cloudflare のアカウント**（https://dash.cloudflare.com/sign-up）
2. **Cloudflare で管理しているドメイン**（例: `example.com`）
   - Cloudflare で新しく取得する: ダッシュボードの **ドメインの登録（Domain Registration）**
   - 他社のドメインを使う: **サイトを追加** し、表示されるネームサーバーをドメインの登録元で設定する（反映に数時間かかることがある）
   - 以降、Scenote は `scenote.example.com` で公開する例で説明する
3. **Gemini の API キー**（作品づくりと嗜好の分析に使う。使わないなら後回しでよい）
   - Google AI Studio（https://aistudio.google.com/apikey）で作成する
4. **作業する PC**（デプロイのときだけ使う。Windows・Mac・Linux いずれも可）
   - Git
   - Node.js 20 以上（https://nodejs.org/）
   - Rust（https://rustup.rs/）

### 1-1. ツールのインストール

```sh
# Rust の WebAssembly 向けの部品と、Worker のビルドツール
rustup target add wasm32-unknown-unknown
cargo install worker-build

# リポジトリを取得
git clone https://github.com/Ao-aoi/Scenote.git
cd Scenote/cloudflare
npm install            # wrangler（Cloudflare の CLI）が入る
```

Windows では、Rust のインストール時に「Visual Studio Build Tools」を求められたら案内に従って入れる。

---

## 2. Cloudflare Access を設定する（ログインの仕組み）

### 2-1. Zero Trust を有効にする（初回のみ）

1. ダッシュボードの左のメニューから **Zero Trust** を開く。
2. チーム名を決める（例: `myteam`）。チームのドメインは `myteam.cloudflareaccess.com` になる。**控えておく**。
3. プランは **Free** を選ぶ（支払い方法の登録を求められる場合があるが、Free プランは課金されない）。

### 2-2. Scenote を保護するアプリケーションを作る

1. **Zero Trust → Access → Applications → Add an application → Self-hosted** を選ぶ。
2. 次のように入力する。
   - Application name: `Scenote`
   - Session Duration: `1 month`（長めにすると iPhone でログインし直す回数が減る）
   - Public hostname（Domain）: サブドメイン `scenote`、ドメイン `example.com`、パスは空欄
3. ポリシーを追加する。
   - Policy name: `本人のみ`
   - Action: **Allow**
   - Include → Selector: **Emails** → 自分のメールアドレス
4. ログイン方法（Login methods）は **One-time PIN**（メールに届く番号でログイン）のままでよい。Google アカウントなどを追加してもよい。
5. 保存後、アプリケーションの **Overview（または Basic information）** にある **Application Audience (AUD) Tag** を **控えておく**。

### 2-3. MCP 用の経路を Access の対象から外す（MCP を使う場合）

Claude・ChatGPT は Access のログイン画面を通れないため、次の経路だけを Access の対象から外す。
これらの経路は Scenote 自身が OAuth のトークンで保護する。

1. もう一度 **Add an application → Self-hosted** を選び、名前を `Scenote MCP` にする。
2. Public hostname を、パスを変えて **4 つ** 登録する（「+ Add public hostname」で追加）。

   | サブドメイン | ドメイン | パス |
   |---|---|---|
   | `scenote` | `example.com` | `mcp` |
   | `scenote` | `example.com` | `oauth/register` |
   | `scenote` | `example.com` | `oauth/token` |
   | `scenote` | `example.com` | `.well-known/*` |

3. ポリシーを追加する: Action **Bypass**、Include → **Everyone**。
4. 保存する。

`/oauth/authorize`（接続を許可する画面）は 2-2 のアプリケーションで保護したままにする。
ここで本人がログインしてから許可するため、他人が勝手に接続することはできない。

---

## 3. 設定ファイルを書く

`cloudflare/wrangler.toml` を開き、次の 2 か所を自分の値にする。

```toml
# コメントを外し、自分のドメインにする
routes = [{ pattern = "scenote.example.com", custom_domain = true }]

[vars]
ACCESS_TEAM_DOMAIN = "myteam.cloudflareaccess.com"   # 2-1 で控えたチームのドメイン
ACCESS_AUD = "0123456789abcdef..."                   # 2-2 で控えた AUD タグ
ALLOWED_EMAILS = "you@example.com"                   # 任意。Access のポリシーに加えて確認する
```

`workers_dev = false` はそのままにする（Access で保護していない `*.workers.dev` の URL を作らないため）。

---

## 4. デプロイする

```sh
cd Scenote/cloudflare

# Cloudflare にログイン（ブラウザが開くので許可する）
npx wrangler login

# Gemini の API キーを秘密情報として登録（入力を求められたらキーを貼り付ける）
npx wrangler secret put GEMINI_API_KEY

# 画面（web/）と Worker をビルドしてデプロイ
npm run deploy
```

`npm run deploy` は次を順に行う。

1. `web/` の依存関係を入れて画面をビルドする（`web/dist`）
2. Rust の Worker を WebAssembly にビルドする（`worker-build --release`）
3. Worker・画面・Durable Object を Cloudflare にアップロードし、`scenote.example.com` に割り当てる

初回は DNS の設定と証明書の発行に数分かかることがある。

> API キーは `wrangler.toml` や Git に書かないこと。秘密情報として登録したキーは、Cloudflare の外へは出ず、
> Scenote の画面にも表示されない（登録済みかどうかだけが表示される）。

---

## 5. 動作を確認する

1. PC のブラウザで `https://scenote.example.com` を開く。
2. Access のログイン画面が出るので、メールアドレスを入力し、届いた番号を入力する。
3. Scenote の画面が開けば完了。新しいプロジェクトを作って、本文を書いてみる。
4. **設定 → AI 設定・使用量** を開き、「API キーは登録済みです」と表示されることを確認する。
   - モデルは「一覧を取得」から選べる。
   - **月あたりのトークン上限** を必要に応じて設定する（初期値は 200 万トークン。到達すると自動で止まる）。
5. **作品づくり（AI）** で、キャラクター案を作ってみる。

---

## 6. iPhone のホーム画面に追加する

1. iPhone の Safari で `https://scenote.example.com` を開き、Access でログインする。
2. 共有ボタン → **ホーム画面に追加** → 追加。
3. ホーム画面の Scenote のアイコンから開く。スマートフォンでは自動で「かんたんモード」になる
   （メニューから「制作モード」に切り替えると、すべての機能を使える）。

Access のログインの有効期間（2-2 の Session Duration）が切れたら、もう一度メールの番号でログインする。

---

## 7. Claude・ChatGPT から接続する（MCP）

1. Scenote の **設定 → AI の接続（MCP）** を開き、表示される URL（`https://scenote.example.com/mcp`）をコピーする。
2. 接続する AI の設定で、カスタムコネクタとして登録する。
   - **Claude**: 設定 → コネクタ → **カスタムコネクタを追加** → URL を貼り付けて追加 → 「連携」。
     追加したコネクタは iPhone の Claude アプリからも使える。
   - **ChatGPT**: 設定 → アプリとコネクタ → 詳細設定で **開発者モード** を有効にする → コネクタを作成 →
     URL を貼り付け、認証に **OAuth** を選ぶ。
   （画面の名前は各サービスの更新で変わることがある）
3. Scenote の「接続の許可」の画面が開く（未ログインなら先に Access のログイン）。内容を確認して **許可する** を押す。
4. AI に「Scenote のプロジェクト一覧を見て」などと頼み、読めることを確認する。

接続した AI は、すべてのプロジェクトの読み取りと、承認を経ない直接の書き換えができる。書き換えた本文は
編集履歴に「AI（MCP）が直接編集」として残り、**履歴 → スナップショット** から以前の状態へ戻せる。
AI には、大きな書き換えの前にスナップショットを作るよう指示してある。
接続は **設定 → AI の接続（MCP）** から確認・解除できる。

---

## 8. 更新する（新しい版を反映する）

```sh
cd Scenote
git pull
cd cloudflare
npm run deploy
```

データ（Durable Object の SQLite）は更新しても消えない。スキーマが変わる場合は、最初の要求のときに自動で更新される。

---

## 9. バックアップと復元

- **書き出し → JSON**: プロジェクト全体（本文・ID・由来・参照ファイル・キャラクター）を保存できる。定期的に保存しておくとよい。
- **スナップショット**: Scenote の中に、その時点のプロジェクトを保存する。誤った変更は、ここから戻せる。
- **Cloudflare 側の復元**: Durable Object の SQLite は、過去 30 日以内の任意の時点へ戻せる（Point-in-time recovery）。
  通常はスナップショットで足りる。Scenote 全体を戻す必要がある場合の最終手段として使う。

---

## 10. 困ったとき

| 症状 | 原因と対処 |
|---|---|
| 画面に「Cloudflare Access でログインしてから開いてください」 | 2-2 の Access のアプリケーションが設定されていない。ドメインとパスを確認する |
| 画面に「サーバーの設定が不足しています（ACCESS_TEAM_DOMAIN・ACCESS_AUD）」 | 3 の `wrangler.toml` の値が空。設定してから `npm run deploy` し直す |
| 「ログインの確認に失敗しました」 | `ACCESS_TEAM_DOMAIN` または `ACCESS_AUD` の値が違う。2-1・2-2 の値を写し直す |
| 「このアカウントには利用が許可されていません」 | `ALLOWED_EMAILS` にログインしたアドレスが入っていない |
| AI の機能で「Gemini の API キーが設定されていません」 | `npx wrangler secret put GEMINI_API_KEY` を実行する（デプロイし直す必要はない） |
| AI の機能で「利用制限に達しました」「上限に達したため停止」 | Gemini の無料枠か、Scenote の月の上限に達した。「AI 設定・使用量」で上限を変えるか、再開する |
| Claude・ChatGPT から接続できない | 2-3 の Bypass の設定（4 つのパス）を確認する。`/.well-known/*` の漏れが多い |
| 接続の許可の画面で「登録されていないクライアントです」 | AI 側でコネクタを削除して、追加し直す |
| `npm run deploy` で `worker-build` が見つからない | 1-1 の `cargo install worker-build` を実行する |
| ログを見たい | `npx wrangler tail` で、動いている Worker のログを表示できる（本文や認証情報はログに出さない） |

---

## 付録 A. 自分の PC で動かして確認する（開発用）

```sh
cd Scenote/cloudflare
cat > .dev.vars <<'EOF'
SCENOTE_DEV_USER=dev@example.com
EOF
npm run dev        # http://127.0.0.1:8787 （localhost に限り Access を省く）
```

AI の機能も試す場合は、模擬 Gemini を使う。

```sh
node tests/mock-gemini.mjs 8790 &
cat >> .dev.vars <<'EOF'
GEMINI_API_KEY=test-key
SCENOTE_GEMINI_BASE_URL=http://127.0.0.1:8790
EOF
npm run dev
node tests/smoke.mjs http://127.0.0.1:8787   # 編集・履歴・キャラクター・参照ファイル
node tests/mcp.mjs   http://127.0.0.1:8787   # OAuth と MCP
node tests/ai.mjs    http://127.0.0.1:8787   # 作品づくり・嗜好・使用量・変数と構造チェック
```

`.dev.vars` は Git に含めない（`.gitignore` 済み）。

## 付録 B. PC 版を外部から使う場合（Cloudflare Tunnel）

PC 版のサーバーをそのまま外から使いたい場合は、Cloudflare Tunnel（`cloudflared`）で
`http://127.0.0.1:8787` を公開できる。その場合も Access で保護し、`httpHostHeader` で Host を `localhost` に
書き換えないこと（書き換えると PC 本体として扱われ、認証が無効になる）。
