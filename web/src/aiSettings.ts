// AI 設定（API キー・モデル・使用量の上限）と、今月の使用量の表示。

import { api } from "./api";
import { h, openDialog, toast } from "./dom";

interface FormValue {
  api_key?: string;
  model: string;
  monthly_token_limit: number;
  warn_percent: number;
}

/** 設定ダイアログを開く。保存した場合は true。 */
export async function openAiSettings(): Promise<boolean> {
  const [settings, usage] = await Promise.all([api.getLlmSettings(), api.getUsage()]);
  const value = await openDialog<FormValue>((close) => {
    const key = h("input", {
      type: "password",
      class: "field",
      autocomplete: "off",
      placeholder: settings.has_api_key ? "設定済み（変更する場合のみ入力）" : "Gemini API キーを入力",
    });
    const removeKey = h("input", { type: "checkbox" });
    const model = h("input", { type: "text", class: "field", value: settings.model, required: true, maxLength: 100 });
    const limit = h("input", {
      type: "number",
      class: "field",
      min: "0",
      step: "1000",
      value: String(settings.monthly_token_limit),
      required: true,
    });
    const warn = h("input", {
      type: "number",
      class: "field",
      min: "1",
      max: "100",
      value: String(settings.warn_percent),
      required: true,
    });
    const usageRows = usage.by_model.map((m) =>
      h(
        "tr", {},
        h("td", {}, `${m.provider} / ${m.model}`),
        h("td", {}, m.requests.toLocaleString()),
        h("td", {}, m.input_tokens.toLocaleString()),
        h("td", {}, m.output_tokens.toLocaleString()),
      ),
    );
    const limitText = usage.monthly_token_limit > 0 ? usage.monthly_token_limit.toLocaleString() : "無制限";
    return h(
      "form",
      {
        onsubmit: (e: Event) => {
          e.preventDefault();
          const result: FormValue = {
            model: model.value.trim(),
            monthly_token_limit: Math.max(0, Math.floor(Number(limit.value) || 0)),
            warn_percent: Math.floor(Number(warn.value) || 0),
          };
          if (removeKey.checked) result.api_key = "";
          else if (key.value.trim()) result.api_key = key.value.trim();
          close(result);
        },
      },
      h("h2", {}, "AI 設定・使用量"),
      h(
        "label", { class: "form-row" },
        h("span", {}, "Gemini API キー（この PC にのみ保存され、画面には表示されません）"),
        key,
      ),
      settings.has_api_key ? h("label", { class: "check" }, removeKey, "保存済みの API キーを削除する") : null,
      h("label", { class: "form-row" }, h("span", {}, "モデル名"), model),
      h("label", { class: "form-row" }, h("span", {}, "月あたりのトークン上限（0 で無制限。到達すると AI の利用を自動停止）"), limit),
      h("label", { class: "form-row" }, h("span", {}, "警告を出す使用率（%）"), warn),
      h("h3", {}, `今月の使用量（${usage.month}、UTC）`),
      h("p", {}, `合計 ${usage.total_tokens.toLocaleString()} / ${limitText} トークン・${usage.requests.toLocaleString()} リクエスト`),
      usageRows.length
        ? h(
            "table", { class: "usage-table" },
            h("thead", {}, h("tr", {}, h("th", {}, "モデル"), h("th", {}, "回数"), h("th", {}, "入力"), h("th", {}, "出力"))),
            h("tbody", {}, ...usageRows),
          )
        : null,
      h("p", { class: "hint" }, "推定コストは表示していません。料金と無料枠の条件は Google の公式ページで確認してください。"),
      h(
        "div", { class: "dialog-actions" },
        h("button", { type: "button", onclick: () => close(null) }, "閉じる"),
        h("button", { type: "submit", class: "primary" }, "保存"),
      ),
    );
  });
  if (!value) return false;
  await api.updateLlmSettings(value);
  toast("AI 設定を保存しました");
  return true;
}
