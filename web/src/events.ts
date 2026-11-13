// サーバーからの変更通知（WebSocket）を受け取り、画面の各部分を読み直す。

import { CLIENT_ID } from "./api";

export interface ChangeMessage {
  type: "change" | "resync" | "revoked";
  scopes: string[];
  client: string | null;
}

export interface EventHandlers {
  /** 他の端末・画面で変更があったとき。`scopes` は影響範囲。 */
  onChange: (scopes: Set<string>) => void;
  /** この端末の登録が解除されたとき。 */
  onRevoked: () => void;
  /** 接続状態が変わったとき。 */
  onConnection: (connected: boolean) => void;
}

export function connectEvents(handlers: EventHandlers): void {
  let retry = 1000;
  let wasConnected = false;
  const open = () => {
    const protocol = location.protocol === "https:" ? "wss:" : "ws:";
    const socket = new WebSocket(`${protocol}//${location.host}/api/events`);
    socket.addEventListener("open", () => {
      retry = 1000;
      handlers.onConnection(true);
      // 切断していた間の変更を取りこぼさないよう、再接続したら全体を読み直す。
      if (wasConnected) handlers.onChange(new Set(["tree", "projects", "scene", "files", "chat", "prefs", "snapshots", "history", "settings"]));
      wasConnected = true;
    });
    socket.addEventListener("message", (e) => {
      let msg: ChangeMessage;
      try {
        msg = JSON.parse(String(e.data)) as ChangeMessage;
      } catch {
        return;
      }
      if (msg.type === "revoked") {
        handlers.onRevoked();
        return;
      }
      if (msg.type === "resync") {
        handlers.onChange(new Set(["tree", "projects", "scene", "files", "chat", "prefs", "snapshots", "history", "settings"]));
        return;
      }
      // 自分が行った変更は、画面に反映済みなので無視する。
      if (msg.client === CLIENT_ID) return;
      handlers.onChange(new Set(msg.scopes));
    });
    socket.addEventListener("close", () => {
      handlers.onConnection(false);
      window.setTimeout(open, retry);
      retry = Math.min(retry * 2, 15000);
    });
  };
  open();
}
