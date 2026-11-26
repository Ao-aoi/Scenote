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
});

await test("未対応の機能は 501、存在しない API は 404", async () => {
  assert.deepEqual((await api("GET", `/projects/${project.id}/files`)).data, []);
  assert.equal((await api("POST", `/projects/${project.id}/chats`, { title: "x" })).status, 501);
  assert.equal((await api("GET", "/usage")).status, 501);
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
