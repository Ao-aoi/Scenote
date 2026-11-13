// 端末の接続（ペアリング）に関する画面。
//
// - 未登録の端末: ペアリングコードを入力して登録する画面。
// - PC 本体: ペアリングコードの発行と、登録済み端末の一覧・登録解除。

import { api } from "./api";
import { askConfirm, h, openDialog, toast } from "./dom";
import { formatTime } from "./historyView";
import { errorMessage } from "./sceneEditor";

/** 未登録の端末に表示する、ペアリングの画面。登録できたらページを読み込み直す。 */
export function showPairingScreen(root: HTMLElement, reason?: string): void {
  const code = h("input", {
    type: "text",
    class: "field pair-code",
    inputMode: "numeric",
    autocomplete: "one-time-code",
    maxLength: 12,
    placeholder: "1234 5678",
    required: true,
    "aria-label": "ペアリングコード",
  });
  const name = h("input", {
    type: "text",
    class: "field",
    maxLength: 100,
    value: /iPad|Macintosh/.test(navigator.userAgent) && "ontouchend" in document ? "iPad" : "",
    placeholder: "例: 自分の iPad",
    "aria-label": "この端末の名前",
  });
  const message = h("p", { class: "error-text", role: "alert" }, reason ?? "");
  const submit = h("button", { type: "submit", class: "primary" }, "接続する");
  const form = h(
    "form",
    {
      class: "pair-card",
      onsubmit: async (e: Event) => {
        e.preventDefault();
        submit.disabled = true;
        message.textContent = "";
        try {
          await api.pair(code.value, name.value);
          location.reload();
        } catch (err) {
          message.textContent = errorMessage(err);
          submit.disabled = false;
          code.select();
        }
      },
    },
    h("h1", {}, "Scenote に接続"),
    h(
      "p", {},
      "この端末はまだ接続が許可されていません。PC の Scenote で「設定」→「端末の接続…」を開き、表示されたペアリングコードを入力してください。",
    ),
    h("label", { class: "form-row" }, h("span", {}, "ペアリングコード（8 桁）"), code),
    h("label", { class: "form-row" }, h("span", {}, "この端末の名前（PC の端末一覧に表示されます）"), name),
    message,
    h("div", { class: "dialog-actions" }, submit),
  );
  root.replaceChildren(h("div", { class: "pair-screen" }, form));
  setTimeout(() => code.focus());
}

/** PC 本体で開く、端末の管理ダイアログ。 */
export async function openDeviceManager(): Promise<void> {
  const status = await api.authStatus();
  await openDialog<null>((close) => {
    const list = h("div", { class: "device-list" });
    const pairingBox = h("div", { class: "pairing-box" });
    let timer: number | undefined;

    const renderDevices = async () => {
      try {
        const devices = await api.listDevices();
        list.replaceChildren(
          ...(devices.length
            ? devices.map((d) =>
                h(
                  "div", { class: "device" },
                  h("div", {}, h("strong", {}, d.name), h("div", { class: "muted pref-meta" }, `登録 ${formatTime(d.created_at)} ・ 最終接続 ${formatTime(d.last_seen_at)}`)),
                  h("button", {
                    type: "button",
                    class: "danger-text",
                    onclick: async () => {
                      if (!(await askConfirm(`「${d.name}」の接続を解除しますか？この端末は、もう一度ペアリングするまで使えなくなります。`, "解除する"))) return;
                      try {
                        await api.deleteDevice(d.id);
                        await renderDevices();
                      } catch (e) {
                        toast(errorMessage(e), "error");
                      }
                    },
                  }, "解除"),
                ),
              )
            : [h("p", { class: "muted" }, "接続している端末はありません。")]),
        );
      } catch (e) {
        list.replaceChildren(h("p", { class: "error-text" }, errorMessage(e)));
      }
    };

    const issue = async () => {
      try {
        const p = await api.startPairing();
        const until = Date.now() + p.expires_in_seconds * 1000;
        const remaining = h("span", { class: "muted" });
        const tick = () => {
          const s = Math.max(0, Math.round((until - Date.now()) / 1000));
          remaining.textContent = s > 0 ? `有効期限まで ${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}（一度だけ使えます）` : "期限が切れました。もう一度発行してください。";
          if (s === 0) window.clearInterval(timer);
        };
        window.clearInterval(timer);
        timer = window.setInterval(tick, 1000);
        tick();
        pairingBox.replaceChildren(
          h("p", {}, "iPad などの端末のブラウザ（Safari）で次のアドレスを開き、コードを入力してください。"),
          ...p.urls.map((u) => h("p", { class: "pair-url" }, u)),
          h("p", { class: "pair-code-display", "aria-label": "ペアリングコード" }, `${p.code.slice(0, 4)} ${p.code.slice(4)}`),
          remaining,
        );
      } catch (e) {
        pairingBox.replaceChildren(h("p", { class: "error-text" }, errorMessage(e)));
      }
    };

    void renderDevices();
    // 開いている間は一覧を定期的に読み直す（端末が接続されたらすぐ分かるように）。
    const poll = window.setInterval(() => {
      if (!document.body.contains(list)) {
        window.clearInterval(poll);
        window.clearInterval(timer);
        return;
      }
      void renderDevices();
    }, 3000);
    const dialog = h(
      "div", {},
      h("h2", {}, "端末の接続"),
      status.lan_enabled
        ? h(
            "div", {},
            h("p", { class: "hint" }, "同じ LAN（Wi-Fi）につながった iPad などから、このプロジェクトを操作できます。ペアリングコードは 10 分間・一度だけ有効です。"),
            h("button", { type: "button", class: "primary", onclick: () => void issue() }, "ペアリングコードを発行"),
          )
        : h(
            "p", { class: "warn-text" },
            "LAN への公開が無効です。iPad から使うには、サーバーを SCENOTE_LAN=1 を付けて起動し直してください（README の「iPad から使う」を参照）。",
          ),
      pairingBox,
      h("h3", {}, "接続済みの端末"),
      list,
      h("p", { class: "hint" }, "通信は暗号化されていない HTTP です。自宅など信頼できるネットワークでのみ使ってください。"),
      h("div", { class: "dialog-actions" }, h("button", { type: "button", onclick: () => { window.clearInterval(timer); window.clearInterval(poll); close(null); } }, "閉じる")),
    );
    return dialog;
  });
}

/** 端末側から自分の接続を解除する。 */
export async function disconnectThisDevice(): Promise<void> {
  if (!(await askConfirm("この端末の接続を解除しますか？もう一度使うには、PC でペアリングコードを発行して入力する必要があります。", "解除する"))) return;
  await api.logout();
  location.reload();
}
