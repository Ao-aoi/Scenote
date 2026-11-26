// AI を使う機能（作品づくり・嗜好の学習・AI 設定と使用量）と、変数・構造チェックの通し確認。
// 模擬 Gemini（tests/mock-gemini.mjs）を起動し、.dev.vars に次を設定した `npm run dev` に対して実行する。
//   SCENOTE_DEV_USER=dev@example.com
//   GEMINI_API_KEY=test-key
//   SCENOTE_GEMINI_BASE_URL=http://127.0.0.1:8790
//   node tests/ai.mjs [http://127.0.0.1:8787]

import assert from "node:assert/strict";

const base = (process.argv[2] ?? "http://127.0.0.1:8787").replace(/\/$/, "");
let failures = 0;
async function api(method, path, body) {
  const res = await fetch(`${base}/api${path}`, {
    method,
    headers: { ...(body !== undefined ? { "content-type": "application/json" } : {}), ...(method !== "GET" ? { "x-scenote": "1" } : {}) },
    body: body !== undefined ? JSON.stringify(body) : undefined,
  });
  const text = await res.text();
  let data; try { data = text ? JSON.parse(text) : undefined; } catch { data = text; }
  return { status: res.status, data };
}
async function test(name, fn) {
  try { await fn(); console.log(`ok   ${name}`); }
  catch (e) { failures++; console.log(`FAIL ${name}\n     ${e.stack ?? e.message}`); }
}

// 使用量は月ごとに積み上がるため、開始時点との差で確かめる。
await api("PUT", "/settings/llm", { monthly_token_limit: 0 });
await api("POST", "/usage/resume");
const usage0 = (await api("GET", "/usage")).data;
const project = (await api("POST", "/projects", { name: "AI の確認", description: "霧の港町で出会う二人の物語" })).data;

await test("AI 設定: API キーは画面から登録できず、モデルと上限は変更できる", async () => {
  const s = (await api("GET", "/settings/llm")).data;
  assert.equal(s.has_api_key, true);
  assert.equal(s.api_key_editable, false);
  assert.equal(JSON.stringify(s).includes("test-key"), false);
  assert.equal((await api("PUT", "/settings/llm", { api_key: "x" })).status, 403);
  const u = await api("PUT", "/settings/llm", { model: "gemini-test", monthly_token_limit: 0 });
  assert.equal(u.data.model, "gemini-test");
  const models = (await api("GET", "/settings/llm/models")).data;
  assert.deepEqual(models.map((m) => m.id), ["gemini-test"]);
});

await test("作品づくり: キャラクター案 → 世界観案 → ストーリー案 → 採用", async () => {
  const chars = await api("POST", `/projects/${project.id}/generate/characters`, { premise: "", instructions: "", count: 2 });
  assert.equal(chars.status, 200);
  assert.equal(chars.data.drafts.length, 1);
  assert.equal(chars.data.warnings.length, 1);
  // 登録前はストーリー案を作れない。
  assert.equal((await api("POST", `/projects/${project.id}/generate/story`, {})).status, 400);
  const c = (await api("POST", `/projects/${project.id}/characters`, { name: "ミナト" })).data;
  await api("POST", `/characters/${c.id}/facts`, { category: "性格", text: "慎重で優しい" });
  const world = await api("POST", `/projects/${project.id}/generate/world`, { characters: chars.data.drafts });
  assert.equal(world.data.drafts.title, "世界観");
  await api("POST", `/projects/${project.id}/files`, { name: "世界観", content: world.data.drafts.content });
  const story = await api("POST", `/projects/${project.id}/generate/story`, {});
  assert.equal(story.data.drafts[0].scenes.length, 2);
  const outline = await api("POST", `/projects/${project.id}/outline`, { chapters: story.data.drafts });
  assert.equal(outline.status, 201);
  const tree = (await api("GET", `/projects/${project.id}`)).data;
  assert.equal(tree.chapters.at(-1).scenes[0].summary, "二人が出会う。");
});

await test("使用量: 記録され、上限に達すると停止し、明示的に再開できる", async () => {
  const usage = (await api("GET", "/usage")).data;
  assert.equal(usage.requests - usage0.requests, 3);
  assert.equal(usage.total_tokens - usage0.total_tokens, 3600);
  await api("PUT", "/settings/llm", { monthly_token_limit: usage.total_tokens + 400 });
  await api("POST", `/projects/${project.id}/generate/world`, {});
  assert.equal((await api("GET", "/usage")).data.state, "stopped");
  const stopped = await api("POST", `/projects/${project.id}/generate/world`, {});
  assert.equal(stopped.status, 429);
  assert.equal(stopped.data.error.code, "llm_stopped");
  assert.equal((await api("POST", "/usage/resume")).data.state, "warning");
  assert.equal((await api("POST", `/projects/${project.id}/generate/world`, {})).status, 200);
  await api("PUT", "/settings/llm", { monthly_token_limit: 0 });
});

await test("変数と構造チェック", async () => {
  const v = await api("POST", `/projects/${project.id}/variables`, { name: "好感度", var_type: "number", initial: "0" });
  assert.equal(v.status, 201);
  assert.equal((await api("POST", `/projects/${project.id}/variables`, { name: "好感度", var_type: "number" })).status, 400);
  assert.equal((await api("POST", `/projects/${project.id}/variables`, { name: "x", var_type: "number", initial: "\"a\"" })).status, 400);
  const tree = (await api("GET", `/projects/${project.id}`)).data;
  const first = tree.chapters[0].scenes[0].id;
  const el = (await api("POST", `/scenes/${first}/elements`, { kind: "VariableChange", speaker: "", body: "" })).data.element;
  await api("PATCH", `/elements/${el.id}`, { base_revision: el.revision, attrs: { effects: "好感度 += 1; 未定義 = 1" } });
  const s = (await api("GET", `/projects/${project.id}/structure`)).data;
  assert.ok(s.nodes.length >= 3);
  assert.ok(s.issues.some((i) => i.message.includes("未定義")));
  assert.ok(s.variable_usage.some((u) => u.name === "好感度" && u.written_in.length === 1));
  assert.equal((await api("PATCH", `/variables/${v.data.id}`, { description: "ミナトへの" })).data.description, "ミナトへの");
});

await test("嗜好: ルールの手動管理と共通への複製", async () => {
  const r = await api("POST", `/projects/${project.id}/preferences/rules`, { text: "台詞は短く", category: "style" });
  assert.equal(r.status, 201);
  const copy = await api("POST", `/preferences/rules/${r.data.id}/copy`, {});
  assert.equal(copy.data.copied, 1);
  const prof = (await api("GET", `/projects/${project.id}/preferences`)).data;
  assert.equal(prof.project_rules.length, 1);
  assert.ok(prof.global_rules.some((g) => g.text === "台詞は短く"));
  await api("DELETE", `/preferences/rules/${prof.global_rules.find((g) => g.text === "台詞は短く").id}`);
  assert.equal((await api("POST", `/projects/${project.id}/preferences/analyze`)).status, 400);
});

console.log(failures === 0 ? "\nすべて成功" : `\n${failures} 件失敗`);
console.log(`PROJECT=${project.id}`);
process.exit(failures === 0 ? 0 : 1);
