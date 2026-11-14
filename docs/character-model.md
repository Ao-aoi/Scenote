# キャラクターを独立したデータとして扱うための拡張方針

対象: [Ao-aoi/Scenote#1](https://github.com/Ao-aoi/Scenote/issues/1)

本書は方針を定めるものであり、キャラクター管理の機能そのものは実装していない。
キャラクター管理・性格分析・関係性マップ・エピソード提案を後から追加するときに、既存のシナリオデータと AI 機能を作り直さずに済むよう、データの境界と移行の手順を決めておく。

> **実装状況**：段階 1（第 3 節）は実装済み（[Ao-aoi/Scenote#12](https://github.com/Ao-aoi/Scenote/issues/12)）。API は `/api/projects/{id}/characters`、`/api/characters/{id}`、`/api/characters/{id}/facts`、`/api/character-facts/{id}`、`/api/projects/{id}/character-relations`、`/api/character-relations/{id}`。要素の作成・更新では `character_id` を指定できる（更新で `null` を送ると外す）。

## 1. 現状（段階 1 の実装前）

キャラクターは、次の場所に**名前の文字列**としてだけ存在する。

| 場所 | 内容 |
|---|---|
| `elements.speaker` | 台詞の話者、動作の主体。空文字は「なし」 |
| Markdown | `**名前**: 台詞`、`[動作:名前] …`（`docs/markdown-format.md`） |
| AI への文脈・提案 | 要素の `speaker` をそのまま渡し、提案も `speaker` を文字列で返す |
| 履歴・スナップショット・JSON 書き出し | 要素の内容として `speaker` を保存 |
| キャラクター相関図 | `speaker` が一致する要素を同じ人物として集計（`server/src/structure.rs`） |
| 参照ファイル | 「キャラクター設定」などの自由記述の Markdown。構造は持たない |

この方式の限界は次の 3 点である。

- 表記のゆれ（「アリス」と「ありす」）や改名を、同じ人物として扱えない。
- プロフィール・性格・関係性など、人物に紐づく情報を置く場所がない。
- 情報の由来（ユーザーが決めた設定か、AI の推定か）を区別できない。

## 2. 目標とするデータ構造

キャラクターを、安定した ID を持つ独立したデータとして追加する。**既存の `speaker` 文字列は残し、ID は併設する**。

```text
characters                      人物そのもの
  id, project_id, name, aliases (別名・表記ゆれの JSON 配列), created_at, updated_at

character_facts                 人物についての個々の情報（プロフィール・性格・口調など）
  id, character_id, category, text,
  source      user（ユーザーが明示）/ confirmed（作中の描写から確認しユーザーが承認）/ inferred（AI の推定）
  confidence  inferred のときの確信度（根拠の件数からプログラムが決める）
  evidence    根拠となる要素の ID（JSON 配列）
  status      active / proposed（承認待ち）/ rejected

character_relations             2 人の関係
  id, project_id, from_character_id, to_character_id, kind（信頼・好意・敵意・尊敬・対立・依存・秘密の共有 …）,
  text, source, confidence, evidence, status      （character_facts と同じ考え方）

elements.character_id           要素から人物への参照（NULL 可）。speaker と併存する
```

設計上の決まりごとは次のとおり。

1. **項目を固定しない。** プロフィールの項目は `character_facts` の行として持ち、`category` は文字列とする（要素の `kind` と同じ方針）。項目を増やしてもスキーマを変えずに済む。
2. **由来を必ず持つ。** `source` で、ユーザーの設定・確認済みの情報・AI の推定を区別する。
3. **ユーザーの設定を優先する。** `source = user` の情報を AI が書き換えることはない。矛盾や変化を見つけた場合は、`status = proposed` の行（「矛盾・成長の候補」）として追加し、ユーザーが承認・却下する。既存の「AI の変更は提案として提示し、承認されたものだけ反映する」原則と、嗜好ルールの更新案（`preference_suggestions`）と同じ流れである。
4. **確信度は LLM に決めさせない。** 嗜好ルールと同じく、根拠の件数からプログラムが決める。
5. **関係は方向つきで持つ。** 「A は B を信頼しているが、B は A を警戒している」を表せるようにする。現在の相関図（同じ場面への登場回数）は自動集計として残し、関係の内容はこのテーブルから重ねて表示する。

## 3. `speaker` 文字列との関係と移行

`speaker` は廃止しない。`character_id` を**追加**し、段階的に対応づける。

| 段階 | 内容 | 既存データへの影響 |
|---|---|---|
| 0（現在） | `speaker` のみ | — |
| 1 | `characters` テーブルと `elements.character_id`（NULL 可）を追加。既存の要素は `character_id = NULL` のまま動く | なし。スキーマの追加のみ |
| 2 | 既存の `speaker` を集計して人物の候補を提示し、ユーザーが確認して `characters` を作る。同じ `name` または `aliases` に一致する `speaker` を持つ要素へ `character_id` を設定する | `speaker` は書き換えない |
| 3 | 以後、要素を保存するときに `speaker` を `name`・`aliases` と照合し、一致すれば `character_id` を自動で設定する。一致しなければ NULL のまま（未登録の人物として扱う） | なし |

- **照合はプログラムが行う**（完全一致。前後の空白は除く）。あいまいな一致を LLM に任せて自動で結びつけることはしない。表記ゆれの統合は、候補を提示してユーザーが `aliases` に加える。
- **改名**は `characters.name` の変更として扱う。過去の要素の `speaker` を一括で書き換えるかどうかはユーザーが選ぶ（書き換える場合は履歴に記録する）。書き換えなくても、旧名を `aliases` に残せば `character_id` で同じ人物として扱える。
- **人物の削除**では、要素の `character_id` を NULL に戻すだけとし、`speaker` と本文は残す。
- 集計（相関図・登場場面）は `character_id` を優先し、NULL の要素は従来どおり `speaker` の文字列で集計する。したがって、段階 1〜2 の途中でも表示は壊れない。

## 4. Markdown・JSON との関係

Markdown + JSON のハイブリッド構成は変えない。

- **Markdown は従来どおり名前で書く**（`**アリス**: …`）。ID は Markdown に埋め込まない。読み込み時に、第 3 節の照合で `character_id` を決める。
- **キャラクターの情報は Markdown を唯一の保存先にしない。** 正本は DB の構造化データとし、JSON 書き出し・スナップショットに `characters`・`character_facts`・`character_relations` と、要素の `character_id` を含める。
- 人が読むための Markdown（プロフィールの一覧など）は、構造化データから**生成する表示**として用意する。既存の参照ファイル（自由記述のキャラクター設定）はそのまま使え、必要なら AI の文脈に加える。
- スナップショットの形式には `schema_version` があるため、`characters` などの項目は「なければ空」として読み込めば、古いスナップショットも復元できる。

## 5. AI 機能との関係

Phase 2 の提案・差分承認の仕組みは変更しない。

- **要素への提案**は今後も `speaker` を文字列で受け取り、保存時に第 3 節の照合で `character_id` を決める。提案の形式・検証（`server/src/assistant.rs`）・差分表示はそのまま使える。
- **AI への文脈**には、登場人物の `user`・`confirmed` の情報を「確定した設定」として、`inferred` の情報を「推定（参考）」として分けて渡す。渡す対象は、開いている場面に登場する人物に絞る（参照ファイルの自動選択と同じく、文脈の量を抑えるため）。
- **性格分析・プロフィール生成**は、嗜好学習（`server/src/preferences.rs`）と同じ構成にする。LLM が根拠（要素の参照名）つきの案を返し、プログラムが検証して `status = proposed` として保存し、ユーザーが承認したものだけを有効にする。
- **エピソード提案**は、選んだ人物の情報と関係を文脈に加えた通常のチャットの依頼として実現できる。生成された案は提案であり、自動で正史にはしない。

## 6. 実装するときの順序

1. スキーマの追加（段階 1）と、JSON 書き出し・スナップショットへの追加
2. 人物の一覧・編集の画面と、既存の `speaker` からの取り込み（段階 2・3）
3. 相関図への関係の重ね合わせ
4. 性格分析・関係の推定（提案として提示）
5. [Ao-aoi/Scenote#6](https://github.com/Ao-aoi/Scenote/issues/6) のキャラクター・世界観の生成は、2 までを前提とする

## 7. いま守っておくこと

キャラクター管理を実装するまでの間、次の境界を保つ。

- 人物の同一性の判定（`speaker` の比較）は、サーバーの集計処理に閉じ込める。画面側で `speaker` の文字列を比較して人物を判断する処理を増やさない。
- `speaker` に、名前以外の情報（肩書きや感情など）を詰め込む使い方を機能として作らない。
- AI が返した人物に関する情報を、承認なしに確定した設定として保存しない。
