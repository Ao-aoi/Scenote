// サーバーの版（PC 版・Cloudflare 版）による機能の違い。
//
// Cloudflare 版は機能を段階的に移しているため、/api/meta の `features` に使える機能だけを返す。
// PC 版は `features` を返さず、すべての機能が使える。

import type { Meta } from "./types";

/** 版によって使えないことがある機能。 */
export type Feature = "ai" | "files" | "history" | "preferences" | "structure" | "devices";

let features: string[] | null = null;
let logoutUrl: string | null = null;

export function setEdition(meta: Meta, status: { logout_url?: string }): void {
  features = meta.features ?? null;
  logoutUrl = status.logout_url ?? null;
}

export function hasFeature(feature: Feature): boolean {
  return features === null || features.includes(feature);
}

/** Cloudflare Access などでログインしている場合の、ログアウト先。 */
export function logoutLocation(): string | null {
  return logoutUrl;
}
