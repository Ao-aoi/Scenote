// 開発用の模擬 Gemini API。`node tests/mock-gemini.mjs [port]` で起動し、.dev.vars の
// SCENOTE_GEMINI_BASE_URL=http://127.0.0.1:<port> で Worker の接続先を差し替える。
// 依頼の種類（システムの指示）に応じて、決まった形の応答を返す。
import { createServer } from "node:http";

const port = Number(process.argv[2] ?? 8790);

function reply(system) {
  if (system.includes("登場人物のプロフィール案")) {
    return { characters: [
      { name: "ミナト", facts: [{ category: "性格", text: "慎重で優しい" }, { category: "口調", text: "丁寧語" }] },
      { name: "", facts: [] },
    ] };
  }
  if (system.includes("世界観・設定の案")) return { title: "世界観", content: "## 舞台\n霧の港町。" };
  if (system.includes("ストーリーの構成案")) {
    return { chapters: [{ title: "第一章 霧", scenes: [{ title: "出会い", summary: "二人が出会う。" }, { title: "約束", summary: "再会を約束する。" }] }] };
  }
  if (system.includes("嗜好プロファイルの更新案")) {
    return { summary: "説明を短くする傾向がある。", changes: [
      { op: "add", text: "地の文は短く切る", category: "length", evidence: ["d1"], reason: "AI の長い文を短く直した" },
      { op: "add", text: "根拠のない案", category: "style", evidence: [], reason: "x" },
    ] };
  }
  return { ok: true };
}

createServer((req, res) => {
  let body = "";
  req.on("data", (c) => (body += c));
  req.on("end", () => {
    res.setHeader("content-type", "application/json");
    if (req.headers["x-goog-api-key"] !== "test-key") {
      res.statusCode = 403;
      res.end(JSON.stringify({ error: { message: "API key not valid" } }));
      return;
    }
    if (req.method === "GET" && req.url.startsWith("/models")) {
      res.end(JSON.stringify({ models: [
        { name: "models/gemini-test", displayName: "Gemini Test", supportedGenerationMethods: ["generateContent"] },
        { name: "models/gemini-image", displayName: "Image", supportedGenerationMethods: ["generateContent"] },
      ] }));
      return;
    }
    const request = JSON.parse(body || "{}");
    const system = request.systemInstruction?.parts?.[0]?.text ?? "";
    res.end(JSON.stringify({
      candidates: [{ content: { parts: [{ text: JSON.stringify(reply(system)) }] } }],
      usageMetadata: { promptTokenCount: 1000, candidatesTokenCount: 200 },
    }));
  });
}).listen(port, "127.0.0.1", () => console.log(`mock gemini on ${port}`));
