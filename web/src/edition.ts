// サーバーの版（PC 版・Cloudflare 版）による機能の違い。
//
// Cloudflare 版は機能を段階的に移しているため、/api/meta の `features` に使える機能だけを返す。
// PC 版は `features` を返さず、すべての機能が使える。

import type { Meta } from "./types";

/** 版によって使えないことがある機能。 */
/** ai: AI チャットと提案、generate: 作品づくり、llm: AI 設定と使用量。 */
export type Feature = "ai" | "files" | "history" | "preferences" | "structure" | "devices" | "mcp" | "generate" | "llm" | "characters";

let features: string[] | null = null;
let logoutUrl: string | null = null;

export function setEdition(meta: Meta, status: { logout_url?: string }): void {
  features = meta.features ?? null;
  logoutUrl = status.logout_url ?? null;
}

export function hasFeature(feature: Feature): boolean {
  // MCP は Cloudflare 版だけの機能（PC 版は features を返さない）。
  if (feature === "mcp") return features?.includes("mcp") ?? false;
  return features === null || features.includes(feature);
}

/** Cloudflare Access などでログインしている場合の、ログアウト先。 */
export function logoutLocation(): string | null {
  return logoutUrl;
}
