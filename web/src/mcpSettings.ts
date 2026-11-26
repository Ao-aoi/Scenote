// MCP での接続（Cloudflare 版）: 登録する URL の案内と、接続中の AI の一覧・解除。

import { api, type McpClient } from "./api";
import { askConfirm, h, openDialog, toast } from "./dom";

export async function openMcpSettings(): Promise<void> {
  const url = `${location.origin}/mcp`;
  const list = h("div", { class: "mcp-clients" });

  const render = (clients: McpClient[]) => {
    if (clients.length === 0) {
      list.replaceChildren(h("p", { class: "muted" }, "接続している AI はありません。"));
      return;
    }
    list.replaceChildren(
      ...clients.map((c) =>
        h(
          "div", { class: "mcp-client" },
          h("div", {},
            h("strong", {}, c.name),
            h("div", { class: "muted" }, `${new Date(c.created_at * 1000).toLocaleString()} に登録・${c.active > 0 ? "接続中" : "未接続"}`),
          ),
          h("button", {
            type: "button",
            class: "danger",
            onclick: async () => {
              if (!(await askConfirm(`「${c.name}」の接続を解除しますか？この AI は、もう一度許可するまで Scenote を読み書きできなくなります。`, "解除する"))) return;
              try {
                await api.revokeMcpClient(c.id);
                render(await api.listMcpClients());
              } catch (e) {
                toast(e instanceof Error ? e.message : "解除できませんでした", "error");
              }
            },
          }, "解除"),
        ),
      ),
    );
  };
  render(await api.listMcpClients());

  await openDialog<void>((close) =>
    h(
      "div", { class: "mcp-settings" },
      h("h2", {}, "AI の接続（MCP）"),
      h("p", {}, "Claude や ChatGPT のコネクタ（カスタムコネクタ）に、次の URL を登録してください。登録すると、この画面と同じ Cloudflare のログインで本人を確認し、接続の許可を求めます。"),
      h("div", { class: "mcp-url" },
        h("input", { type: "text", readonly: true, value: url, "aria-label": "MCP の URL", onclick: (e: Event) => (e.target as HTMLInputElement).select() }),
        h("button", {
          type: "button",
          onclick: async () => {
            try {
              await navigator.clipboard.writeText(url);
              toast("コピーしました");
            } catch {
              toast("コピーできませんでした。URL を選択してコピーしてください", "error");
            }
          },
        }, "コピー"),
      ),
      h("p", { class: "muted" }, "接続した AI は、すべてのプロジェクトを読み取り、承認を経ずに直接書き換えられます。書き換えた本文には由来「AI（MCP）」が記録されます。"),
      h("h3", {}, "接続している AI"),
      list,
      h("div", { class: "dialog-actions" }, h("button", { type: "button", onclick: () => close(null) }, "閉じる")),
    ),
  );
}
