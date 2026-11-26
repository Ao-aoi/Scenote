// MCP と OAuth の通し確認。`npm run dev`（.dev.vars に SCENOTE_DEV_USER）を起動した状態で
//   node tests/mcp.mjs [http://127.0.0.1:8787]
// を実行する。クライアントの登録 → 認可（許可の画面）→ トークン → MCP のツール呼び出しまでを確かめる。

import assert from "node:assert/strict";
import { createHash, randomBytes } from "node:crypto";

const base = (process.argv[2] ?? "http://127.0.0.1:8787").replace(/\/$/, "");
const b64url = (buf) => buf.toString("base64url");
let failures = 0;
async function test(name, fn) {
  try { await fn(); console.log(`ok   ${name}`); }
  catch (e) { failures++; console.log(`FAIL ${name}\n     ${e.stack ?? e.message}`); }
}

const redirect = "https://client.example/callback";
let clientId, accessToken, refreshToken, projectId, sceneId;

await test("MCP は未認証なら 401 と認可サーバーの案内を返す", async () => {
  const r = await fetch(`${base}/mcp`, { method: "POST", headers: { "content-type": "application/json" }, body: "{}" });
  assert.equal(r.status, 401);
  assert.match(r.headers.get("www-authenticate"), /resource_metadata="http.*\/\.well-known\/oauth-protected-resource"/);
  const meta = await (await fetch(`${base}/.well-known/oauth-protected-resource/mcp`)).json();
  assert.equal(meta.resource, `${base}/mcp`);
  const as = await (await fetch(`${base}/.well-known/oauth-authorization-server`)).json();
  assert.deepEqual(as.code_challenge_methods_supported, ["S256"]);
  assert.equal(as.registration_endpoint, `${base}/oauth/register`);
});

await test("クライアントの動的登録（http の外部 URL は拒否）", async () => {
  const bad = await fetch(`${base}/oauth/register`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ redirect_uris: ["http://evil.example/cb"] }) });
  assert.equal(bad.status, 400);
  const r = await fetch(`${base}/oauth/register`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ client_name: "テスト<script>", redirect_uris: [redirect] }) });
  assert.equal(r.status, 201);
  clientId = (await r.json()).client_id;
  assert.ok(clientId.length >= 40);
});

const verifier = b64url(randomBytes(32));
const challenge = b64url(createHash("sha256").update(verifier).digest());
const params = new URLSearchParams({ response_type: "code", client_id: "", redirect_uri: redirect, state: "st<1>", code_challenge: challenge, code_challenge_method: "S256" });
let code;

await test("許可の画面を表示し、許可するとコードつきで転送する", async () => {
  params.set("client_id", clientId);
  const page = await fetch(`${base}/oauth/authorize?${params}`);
  assert.equal(page.status, 200);
  const html = await page.text();
  assert.match(html, /テスト&lt;script&gt;/);
  assert.doesNotMatch(html, /<script>/);
  const wrong = new URLSearchParams(params); wrong.set("redirect_uri", "https://other.example/cb");
  assert.equal((await fetch(`${base}/oauth/authorize?${wrong}`)).status, 400);
  const noPkce = new URLSearchParams(params); noPkce.delete("code_challenge");
  assert.equal((await fetch(`${base}/oauth/authorize?${noPkce}`)).status, 400);
  const form = new URLSearchParams(params); form.set("decision", "allow");
  const r = await fetch(`${base}/oauth/authorize`, { method: "POST", redirect: "manual", headers: { "content-type": "application/x-www-form-urlencoded", origin: base }, body: form });
  assert.equal(r.status, 302);
  const to = new URL(r.headers.get("location"));
  assert.equal(`${to.origin}${to.pathname}`, redirect);
  assert.equal(to.searchParams.get("state"), "st<1>");
  code = to.searchParams.get("code");
  assert.ok(code);
});

await test("拒否すると access_denied で戻る", async () => {
  const form = new URLSearchParams(params); form.set("decision", "deny");
  const r = await fetch(`${base}/oauth/authorize`, { method: "POST", redirect: "manual", headers: { "content-type": "application/x-www-form-urlencoded", origin: base }, body: form });
  assert.equal(new URL(r.headers.get("location")).searchParams.get("error"), "access_denied");
});

async function token(form) {
  const r = await fetch(`${base}/oauth/token`, { method: "POST", headers: { "content-type": "application/x-www-form-urlencoded" }, body: new URLSearchParams(form) });
  return { status: r.status, data: await r.json() };
}

await test("コードの交換（PKCE の検証、コードは一度だけ）", async () => {
  const bad = await token({ grant_type: "authorization_code", code, client_id: clientId, redirect_uri: redirect, code_verifier: "wrong" });
  assert.equal(bad.status, 400);
  // 失敗した交換でもコードは無効になる。新しいコードを取り直す。
  const form = new URLSearchParams(params); form.set("decision", "allow");
  const r = await fetch(`${base}/oauth/authorize`, { method: "POST", redirect: "manual", headers: { "content-type": "application/x-www-form-urlencoded", origin: base }, body: form });
  code = new URL(r.headers.get("location")).searchParams.get("code");
  const ok = await token({ grant_type: "authorization_code", code, client_id: clientId, redirect_uri: redirect, code_verifier: verifier });
  assert.equal(ok.status, 200);
  assert.equal(ok.data.token_type, "Bearer");
  accessToken = ok.data.access_token;
  refreshToken = ok.data.refresh_token;
  const again = await token({ grant_type: "authorization_code", code, client_id: clientId, redirect_uri: redirect, code_verifier: verifier });
  assert.equal(again.status, 400);
});

let rpcId = 0;
async function rpc(method, params, tokenValue = accessToken) {
  const r = await fetch(`${base}/mcp`, {
    method: "POST",
    headers: { "content-type": "application/json", accept: "application/json, text/event-stream", authorization: `Bearer ${tokenValue}` },
    body: JSON.stringify({ jsonrpc: "2.0", id: ++rpcId, method, params }),
  });
  return { status: r.status, data: r.status === 202 ? null : await r.json() };
}
async function call(name, args) {
  const r = await rpc("tools/call", { name, arguments: args });
  const result = r.data.result;
  const text = result.content[0].text;
  let data; try { data = JSON.parse(text); } catch { data = text; }
  return { isError: result.isError, data };
}

await test("initialize と tools/list", async () => {
  const init = await rpc("initialize", { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "test", version: "1" } });
  assert.equal(init.data.result.protocolVersion, "2025-06-18");
  assert.equal(init.data.result.serverInfo.name, "scenote");
  assert.ok(init.data.result.instructions.includes("プロデューサー"));
  const note = await fetch(`${base}/mcp`, { method: "POST", headers: { "content-type": "application/json", authorization: `Bearer ${accessToken}` }, body: JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" }) });
  assert.equal(note.status, 202);
  const list = await rpc("tools/list", {});
  const names = list.data.result.tools.map((t) => t.name);
  for (const n of ["list_projects", "get_outline", "read_scene", "write_scene", "append_to_scene", "update_element", "create_scene"]) assert.ok(names.includes(n), n);
  assert.equal((await rpc("nope", {})).data.error.code, -32601);
});

await test("ツールで作成・追記・読み取り（由来は ai_direct）", async () => {
  const p = await call("create_project", { name: "MCP の作品" });
  assert.equal(p.isError, false);
  projectId = p.data.id;
  const outline = await call("get_outline", { project_id: projectId });
  sceneId = outline.data.chapters[0].scenes[0].id;
  const a = await call("append_to_scene", { scene_id: sceneId, markdown: "雨が降り出した。\n\n**アオ**: 傘、ある？" });
  assert.equal(a.isError, false);
  const scene = await call("read_scene", { scene_id: sceneId });
  assert.deepEqual(scene.data.elements.map((e) => [e.kind, e.speaker, e.body, e.origin]), [
    ["Narration", "", "雨が降り出した。", "ai_direct"],
    ["Dialogue", "アオ", "傘、ある？", "ai_direct"],
  ]);
  const text = await call("read_project_text", { project_id: projectId });
  assert.match(text.data, /\*\*アオ\*\*: 傘、ある？/);
});

await test("競合と入力の誤りはツールの失敗として返す", async () => {
  const scene = await call("read_scene", { scene_id: sceneId });
  const el = scene.data.elements[0];
  const ok = await call("update_element", { element_id: el.id, base_revision: el.revision, body: "雨が強くなった。" });
  assert.equal(ok.isError, false);
  const stale = await call("update_element", { element_id: el.id, base_revision: el.revision, body: "古い" });
  assert.equal(stale.isError, true);
  assert.equal(stale.data.error.code, "conflict");
  assert.equal((await call("read_scene", { scene_id: "../projects" })).isError, true);
  assert.equal((await call("unknown_tool", {})).isError, true);
});

await test("キャラクター・世界観・履歴のツール", async () => {
  const c = await call("create_character", { project_id: projectId, name: "アオ" });
  assert.equal(c.isError, false);
  const k = await call("create_character", { project_id: projectId, name: "カイ" });
  assert.equal((await call("add_character_fact", { character_id: c.data.id, category: "性格", text: "強がり", source: "inferred", confidence: 0.6 })).isError, false);
  assert.equal((await call("add_relation", { project_id: projectId, from_character_id: c.data.id, to_character_id: k.data.id, kind: "幼なじみ" })).isError, false);
  const cast = await call("get_characters", { project_id: projectId });
  assert.equal(cast.data.characters.length, 2);
  assert.equal(cast.data.characters.find((x) => x.name === "アオ").facts[0].text, "強がり");
  assert.equal(cast.data.relations[0].kind, "幼なじみ");
  const f = await call("create_file", { project_id: projectId, name: "世界観", content: "雨の多い港町" });
  assert.equal(f.isError, false);
  const files = await call("list_files", { project_id: projectId });
  assert.equal(files.data[0].name, "世界観");
  assert.equal(files.data[0].content, undefined);
  const read = await call("read_file", { file_id: f.data.id });
  assert.equal(read.data.content, "雨の多い港町");
  assert.equal((await call("update_file", { file_id: f.data.id, base_revision: read.data.revision, content: "霧の港町" })).isError, false);
  const snap = await call("create_snapshot", { project_id: projectId, note: "MCP での保存" });
  assert.equal(snap.isError, false);
  const hist = await call("list_history", { project_id: projectId, limit: 5 });
  assert.equal(hist.data[0].actor, "ai_direct");
  assert.equal(hist.data[0].target_type, "file");
  assert.equal((await call("compare_snapshot", { snapshot_id: snap.data.id })).isError, false);
});

await test("画面での編集は人間の由来、AI の要素を人間が直すと mixed", async () => {
  const scene = await call("read_scene", { scene_id: sceneId });
  const el = scene.data.elements[1];
  const r = await fetch(`${base}/api/elements/${el.id}`, { method: "PATCH", headers: { "content-type": "application/json", "x-scenote": "1" }, body: JSON.stringify({ base_revision: el.revision, body: "傘ある？" }) });
  assert.equal((await r.json()).element.origin, "mixed");
});

await test("リフレッシュトークンで更新し、古いものは使えない", async () => {
  const r = await token({ grant_type: "refresh_token", refresh_token: refreshToken, client_id: clientId });
  assert.equal(r.status, 200);
  assert.notEqual(r.data.access_token, accessToken);
  assert.equal((await token({ grant_type: "refresh_token", refresh_token: refreshToken })).status, 400);
  accessToken = r.data.access_token;
  assert.equal((await rpc("ping", {})).status, 200);
});

await test("設定画面から接続を解除するとトークンが無効になる", async () => {
  const clients = await (await fetch(`${base}/api/mcp/clients`)).json();
  assert.ok(clients.some((c) => c.id === clientId && c.active > 0));
  const del = await fetch(`${base}/api/mcp/clients/${clientId}`, { method: "DELETE", headers: { "x-scenote": "1" } });
  assert.equal(del.status, 204);
  assert.equal((await rpc("ping", {})).status, 401);
});

await fetch(`${base}/api/projects/${projectId}`, { method: "DELETE", headers: { "x-scenote": "1" } });
console.log(failures === 0 ? "\nすべて成功" : `\n${failures} 件失敗`);
process.exit(failures === 0 ? 0 : 1);
