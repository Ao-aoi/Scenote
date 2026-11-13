// AI 設定（API キー・モデル・使用量の上限）と、今月の使用量の表示。

import { api } from "./api";
import { h, openDialog, toast } from "./dom";
import type { LlmModel } from "./types";

interface FormValue {
  api_key?: string;
  model: string;
  monthly_token_limit: number;
  warn_percent: number;
}

/** 設定ダイアログを開く。保存した場合は true。 */
export async function openAiSettings(): Promise<boolean> {
  const [settings, usage] = await Promise.all([api.getLlmSettings(), api.getUsage()]);
  let models: LlmModel[] = [];
  const value = await openDialog<FormValue>((close) => {
    const key = h("input", {
      type: "password",
      class: "field",
      autocomplete: "off",
      placeholder: settings.has_api_key ? "設定済み（変更する場合のみ入力）" : "Gemini API キーを入力",
    });
    const removeKey = h("input", { type: "checkbox" });
    const model = h("select", { class: "field", required: true });
    const modelStatus = h("p", { class: "hint" });
    const refreshModels = h("button", { type: "button" }, "モデル一覧を更新");
    let loading = false;
    const renderModels = (error?: string) => {
      const current = model.value || settings.model;
      const choices = new Map<string, string>();
      choices.set("gemini-flash-latest", "Gemini Flash Latest（デフォルト）");
      for (const item of models) choices.set(item.id, item.display_name);
      if (!choices.has(settings.model)) choices.set(settings.model, settings.model + "（現在の設定）");
      model.replaceChildren(...[...choices].map(([id, label]) =>
        h("option", { value: id, selected: id === current }, label),
      ));
      modelStatus.textContent = loading
        ? "モデル一覧を取得しています…"
        : error
          ? error
          : models.length
        ? models.length + " 個の generateContent 対応モデルを取得しました。"
        : settings.has_api_key
          ? "モデル一覧を取得できませんでした。APIキー・通信状態を確認して再取得してください。"
          : "APIキーを設定すると、利用可能なモデル一覧を取得できます。";
    };
    // ダイアログはすぐに開き、一覧は裏で取得する（通信が遅くても設定画面を待たせない）。
    const loadModels = async () => {
      if (!settings.has_api_key || loading) return;
      loading = true;
      refreshModels.disabled = true;
      renderModels();
      let error: string | undefined;
      try {
        models = await api.getGeminiModels();
      } catch (e) {
        error = e instanceof Error ? e.message : "モデル一覧を取得できませんでした。";
      } finally {
        loading = false;
        refreshModels.disabled = !settings.has_api_key;
      }
      renderModels(error);
    };
    refreshModels.disabled = !settings.has_api_key;
    refreshModels.addEventListener("click", () => void loadModels());
    renderModels();
    void loadModels();
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
      h("label", { class: "form-row" }, h("span", {}, "Gemini モデル"), model),
      h("div", { class: "form-row" }, refreshModels, modelStatus),
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
