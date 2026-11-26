// Cloudflare 版の通し確認。`npm run dev`（wrangler dev、.dev.vars に SCENOTE_DEV_USER）を起動した状態で
//   node tests/smoke.mjs [http://127.0.0.1:8787]
// を実行する。データは新しいプロジェクトを作って確認し、最後に削除する。

import assert from "node:assert/strict";

const base = (process.argv[2] ?? "http://127.0.0.1:8787").replace(/\/$/, "");
let failures = 0;

async function api(method, path, body, headers = {}) {
  const res = await fetch(`${base}/api${path}`, {
    method,
    headers: {
      ...(body !== undefined ? { "content-type": "application/json" } : {}),
      ...(method !== "GET" ? { "x-scenote": "1" } : {}),
      ...headers,
    },
    body: body !== undefined ? JSON.stringify(body) : undefined,
  });
  const text = await res.text();
  let data;
  try { data = text ? JSON.parse(text) : undefined; } catch { data = text; }
  return { status: res.status, data, headers: res.headers };
}

async function test(name, fn) {
  try {
    await fn();
    console.log(`ok   ${name}`);
  } catch (e) {
    failures++;
    console.log(`FAIL ${name}\n     ${e.message}`);
  }
}

const meta = await api("GET", "/meta");
const project = (await api("POST", "/projects", { name: "通し確認", templates: ["choice_adv"] })).data;
const tree = (await api("GET", `/projects/${project.id}`)).data;
const chapterId = tree.chapters[0].id;
const sceneId = tree.chapters[0].scenes[0].id;

await test("meta は Cloudflare 版であることと機能を返す", async () => {
  assert.equal(meta.data.edition, "cloudflare");
  assert.ok(Array.isArray(meta.data.features));
});

await test("新規プロジェクトには最初の章と場面がある", async () => {
  assert.equal(tree.chapters.length, 1);
  assert.equal(tree.chapters[0].scenes.length, 1);
});

await test("状態を変える要求は独自ヘッダーがないと拒否する（CSRF 対策）", async () => {
  const r2 = await fetch(`${base}/api/projects`, { method: "POST", headers: { "content-type": "application/json" }, body: '{"name":"x"}' });
  assert.equal(r2.status, 403);
});

await test("別の配信元からの要求は拒否する", async () => {
  const r = await api("POST", "/projects", { name: "x" }, { origin: "https://evil.example" });
  assert.equal(r.status, 403);
});

let element;
await test("要素の追加・更新・競合検出", async () => {
  const a = await api("POST", `/scenes/${sceneId}/elements`, { kind: "Narration", speaker: "", body: "夜。" });
  assert.equal(a.status, 201);
  element = a.data.element;
  const b = await api("POST", `/scenes/${sceneId}/elements`, { kind: "Dialogue", speaker: "アオ", body: "行こう", position: 0 });
  assert.equal(b.status, 201);
  const scene = (await api("GET", `/scenes/${sceneId}`)).data;
  assert.deepEqual(scene.elements.map((e) => e.body), ["行こう", "夜。"]);
  const u = await api("PATCH", `/elements/${element.id}`, { base_revision: element.revision, body: "夜が明ける。" });
  assert.equal(u.status, 200);
  assert.equal(u.data.element.revision, element.revision + 1);
  assert.equal(typeof u.data.scene_revision, "number");
  const stale = await api("PATCH", `/elements/${element.id}`, { base_revision: element.revision, body: "古い画面から" });
  assert.equal(stale.status, 409);
  assert.equal(stale.data.error.code, "conflict");
  assert.equal(stale.data.error.current_revision, element.revision + 1);
  element = u.data.element;
});

await test("入力の検証", async () => {
  const r = await api("POST", `/scenes/${sceneId}/elements`, { kind: "1bad", speaker: "", body: "" });
  assert.equal(r.status, 400);
  const t = await api("POST", `/projects/${project.id}/chapters`, { title: "  " });
  assert.equal(t.status, 400);
});

await test("並べ替え（現在の一覧と一致しない指定は拒否）", async () => {
  const scene = (await api("GET", `/scenes/${sceneId}`)).data;
  const ids = scene.elements.map((e) => e.id).reverse();
  assert.equal((await api("PUT", `/scenes/${sceneId}/elements/order`, { ids })).status, 204);
  const after = (await api("GET", `/scenes/${sceneId}`)).data;
  assert.deepEqual(after.elements.map((e) => e.id), ids);
  assert.equal((await api("PUT", `/scenes/${sceneId}/elements/order`, { ids: ids.slice(1) })).status, 400);
});

await test("Markdown の保存は変わっていない要素の ID を保つ", async () => {
  const md = (await api("GET", `/scenes/${sceneId}/markdown`)).data;
  const source = md.markdown + "\n\n**アオ**: 朝だ\n";
  const r = await api("PUT", `/scenes/${sceneId}/markdown`, { base_revision: md.revision, markdown: source });
  assert.equal(r.status, 200);
  assert.equal(r.data.elements.length, 3);
  assert.ok(r.data.elements.some((e) => e.id === element.id));
  const stale = await api("PUT", `/scenes/${sceneId}/markdown`, { base_revision: md.revision, markdown: "" });
  assert.equal(stale.status, 409);
});

await test("章・場面の作成・名前の変更・概要・並べ替え・削除", async () => {
  const s2 = (await api("POST", `/chapters/${chapterId}/scenes`, { title: "場面2", summary: "出発する" })).data;
  const s0 = (await api("POST", `/chapters/${chapterId}/scenes`, { title: "前日", after: undefined })).data;
  const renamed = await api("PATCH", `/scenes/${s2.id}`, { title: "旅立ち" });
  assert.equal(renamed.data.title, "旅立ち");
  const sum = await api("PATCH", `/scenes/${s2.id}`, { summary: "新しい概要", base_revision: renamed.data.revision });
  assert.equal(sum.data.summary, "新しい概要");
  assert.equal((await api("PATCH", `/scenes/${s2.id}`, { summary: "x", base_revision: 1 })).status, 409);
  const order = [s0.id, sceneId, s2.id];
  assert.equal((await api("PUT", `/chapters/${chapterId}/scenes/order`, { ids: order })).status, 204);
  const t = (await api("GET", `/projects/${project.id}`)).data;
  assert.deepEqual(t.chapters[0].scenes.map((s) => s.id), order);
  assert.equal((await api("DELETE", `/scenes/${s0.id}`)).status, 204);
  const t2 = (await api("GET", `/projects/${project.id}`)).data;
  assert.deepEqual(t2.chapters[0].scenes.map((s) => s.position), [0, 1]);
  const c2 = (await api("POST", `/projects/${project.id}/chapters`, { title: "第2章" })).data;
  assert.equal((await api("PATCH", `/chapters/${c2.id}`, { title: "終章" })).data.title, "終章");
  assert.equal((await api("DELETE", `/chapters/${c2.id}`)).status, 204);
});

await test("分岐の設定（移動先は同じプロジェクトの場面に限る）", async () => {
  const t = (await api("GET", `/projects/${project.id}`)).data;
  const target = t.chapters[0].scenes[1].id;
  const tr = (await api("POST", `/scenes/${sceneId}/elements`, { kind: "Transition", speaker: "", body: "" })).data.element;
  const ok = await api("PATCH", `/elements/${tr.id}`, { base_revision: tr.revision, attrs: { target, condition: " 鍵 " } });
  assert.equal(ok.status, 200);
  assert.deepEqual(ok.data.element.attrs, { target, condition: "鍵" });
  const bad = await api("PATCH", `/elements/${tr.id}`, { base_revision: ok.data.element.revision, attrs: { target: "nope" } });
  assert.equal(bad.status, 400);
  // 失敗した要求は変更を残さない（リビジョンが進んでいない）。
  const again = (await api("GET", `/scenes/${sceneId}`)).data.elements.find((e) => e.id === tr.id);
  assert.equal(again.revision, ok.data.element.revision);
});

await test("書き出し（Markdown・JSON）", async () => {
  const md = await api("GET", `/projects/${project.id}/export/markdown`);
  assert.equal(md.status, 200);
  assert.match(md.headers.get("content-disposition"), /attachment/);
  assert.match(md.data, /title: "通し確認"/);
  const js = await api("GET", `/projects/${project.id}/export/json`);
  assert.equal(js.data.format, "scenote-project");
  assert.equal(js.data.schema_version, meta.data.schema_version);
  assert.ok(Array.isArray(js.data.reference_files));
  assert.ok(Array.isArray(js.data.characters));
});

await test("参照ファイル（世界観）の作成・更新・競合・削除", async () => {
  const f = await api("POST", `/projects/${project.id}/files`, { name: "世界観", content: "魔法のない世界" });
  assert.equal(f.status, 201);
  const u = await api("PATCH", `/files/${f.data.id}`, { base_revision: f.data.revision, content: "魔法が失われた世界", pinned: true });
  assert.equal(u.data.content, "魔法が失われた世界");
  assert.equal(u.data.pinned, true);
  assert.equal((await api("PATCH", `/files/${f.data.id}`, { base_revision: f.data.revision, content: "x" })).status, 409);
  const list = (await api("GET", `/projects/${project.id}/files`)).data;
  assert.equal(list.length, 1);
  const tmp = (await api("POST", `/projects/${project.id}/files`, { name: "メモ" })).data;
  assert.equal((await api("DELETE", `/files/${tmp.id}`)).status, 204);
});

let aoi, kai;
await test("キャラクター・情報・関係", async () => {
  aoi = (await api("POST", `/projects/${project.id}/characters`, { name: "アオ", aliases: ["あおちゃん"] })).data;
  kai = (await api("POST", `/projects/${project.id}/characters`, { name: "カイ" })).data;
  assert.equal((await api("POST", `/projects/${project.id}/characters`, { name: "アオ" })).status, 400);
  const fact = await api("POST", `/characters/${aoi.id}/facts`, { category: "性格", text: "強がり" });
  assert.equal(fact.status, 201);
  assert.equal(fact.data.source, "user");
  assert.equal((await api("POST", `/characters/${aoi.id}/facts`, { category: "性格", text: "x", source: "inferred" })).status, 400);
  const inferred = await api("POST", `/characters/${aoi.id}/facts`, { category: "過去", text: "兄がいる", source: "inferred", confidence: 0.4 });
  assert.equal(inferred.status, 201);
  const rel = await api("POST", `/projects/${project.id}/character-relations`, { from_character_id: aoi.id, to_character_id: kai.id, kind: "幼なじみ" });
  assert.equal(rel.status, 201);
  assert.equal((await api("PATCH", `/character-relations/${rel.data.id}`, { text: "家が隣" })).data.text, "家が隣");
  assert.equal((await api("GET", `/characters/${aoi.id}/facts`)).data.length, 2);
  // 要素にキャラクターを結びつける。
  const el = (await api("POST", `/scenes/${sceneId}/elements`, { kind: "Dialogue", speaker: "カイ", body: "よう", character_id: kai.id })).data.element;
  assert.equal(el.character_id, kai.id);
  // 削除すると情報・関係も消え、要素の結びつきは外れる（話者名は残る）。
  assert.equal((await api("DELETE", `/characters/${kai.id}`)).status, 204);
  assert.equal((await api("GET", `/projects/${project.id}/character-relations`)).data.length, 0);
  const after = (await api("GET", `/scenes/${sceneId}`)).data.elements.find((e) => e.id === el.id);
  assert.equal(after.character_id, null);
  assert.equal(after.speaker, "カイ");
});

await test("編集履歴（連続した更新はまとめる）", async () => {
  const scene = (await api("GET", `/scenes/${sceneId}`)).data;
  let el = scene.elements[0];
  for (const body of ["一", "一二", "一二三"]) {
    el = (await api("PATCH", `/elements/${el.id}`, { base_revision: el.revision, body })).data.element;
  }
  const hist = (await api("GET", `/projects/${project.id}/history?limit=5`)).data;
  const latest = hist[0];
  assert.equal(latest.target_id, el.id);
  assert.equal(latest.actor, "human");
  assert.equal(latest.after.body, "一二三");
  assert.ok(latest.diff.length > 0);
  assert.equal(hist.filter((h) => h.target_id === el.id && h.action === "update").length, 1);
  const filtered = (await api("GET", `/projects/${project.id}/history?scene_id=${sceneId}`)).data;
  assert.ok(filtered.every((h) => h.scene_id === sceneId));
  const settings = await api("PUT", "/settings/history", { max_entries: 200 });
  assert.equal(settings.data.max_entries, 200);
  assert.equal((await api("PUT", "/settings/history", { max_entries: 1 })).status, 400);
});

await test("スナップショットの作成・比較・復元", async () => {
  const snap = await api("POST", `/projects/${project.id}/snapshots`, { note: "書き直し前" });
  assert.equal(snap.status, 201);
  const scene = (await api("GET", `/scenes/${sceneId}`)).data;
  const el = scene.elements[0];
  await api("PATCH", `/elements/${el.id}`, { base_revision: el.revision, body: "すっかり書き換えた" });
  await api("POST", `/characters/${aoi.id}/facts`, { category: "口調", text: "早口" });
  const cmp = (await api("GET", `/snapshots/${snap.data.id}/compare`)).data;
  assert.ok(cmp.some((c) => c.item_type === "scene" && c.status === "changed"));
  const restored = await api("POST", `/snapshots/${snap.data.id}/restore`);
  assert.equal(restored.status, 200);
  assert.equal(restored.data.kind, "before_restore");
  const back = (await api("GET", `/scenes/${sceneId}`)).data;
  assert.equal(back.elements[0].body, el.body);
  assert.ok(back.elements[0].revision > el.revision);
  assert.equal((await api("GET", `/characters/${aoi.id}/facts`)).data.length, 2);
  const snaps = (await api("GET", `/projects/${project.id}/snapshots`)).data;
  assert.equal(snaps.length, 2);
  const hist = (await api("GET", `/projects/${project.id}/history?limit=1`)).data;
  assert.equal(hist[0].action, "restore");
  assert.equal((await api("DELETE", `/snapshots/${restored.data.id}`)).status, 204);
});

await test("未対応の機能は 501、存在しない API は 404", async () => {
  assert.deepEqual((await api("GET", `/projects/${project.id}/chats`)).data, []);
  assert.equal((await api("POST", `/projects/${project.id}/chats`, { title: "x" })).status, 501);
  assert.equal((await api("GET", "/devices")).status, 501);
  assert.equal((await api("GET", "/nothing")).status, 404);
});

await test("変更は WebSocket で通知される", async () => {
  const ws = new WebSocket(`${base.replace(/^http/, "ws")}/api/events`);
  await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = reject; });
  const got = new Promise((resolve, reject) => {
    ws.onmessage = (e) => resolve(JSON.parse(e.data));
    setTimeout(() => reject(new Error("通知が届かない")), 5000);
  });
  await api("PATCH", `/scenes/${sceneId}`, { title: "改題" }, { "x-scenote-client": "smoke" });
  const msg = await got;
  ws.close();
  assert.equal(msg.type, "change");
  assert.deepEqual(msg.scopes, ["tree", "scene"]);
  assert.equal(msg.client, "smoke");
});

await test("プロジェクトの削除で章・場面・要素も消える", async () => {
  assert.equal((await api("DELETE", `/projects/${project.id}`)).status, 204);
  assert.equal((await api("GET", `/projects/${project.id}`)).status, 404);
  assert.equal((await api("GET", `/scenes/${sceneId}`)).status, 404);
});

console.log(failures === 0 ? "\nすべて成功" : `\n${failures} 件失敗`);
process.exit(failures === 0 ? 0 : 1);
