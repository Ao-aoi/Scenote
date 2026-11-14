// AI 設定（API キー・モデル・使用量の上限）と、今月の使用量の表示。

import { api } from "./api";
import { h, openDialog, toast } from "./dom";
import { describeModel, ModelPicker } from "./modelPicker";
import type { LlmModel } from "./types";

interface FormValue {
  api_key?: string;
  model: string;
  monthly_token_limit: number;
  warn_percent: number;
}

/** 設定ダイアログを開く。保存した場合は true。 */
/** `local` が false（iPad などの端末）の場合、API キーの欄は表示しない（キーを LAN に流さないため）。 */
export async function openAiSettings(local = true): Promise<boolean> {
  const [settings, usage, prefs] = await Promise.all([
    api.getLlmSettings(),
    api.getUsage(),
    api.updatePreferenceSettings({}),
  ]);
  let models: LlmModel[] = [];
  const value = await openDialog<FormValue>((close) => {
    const key = h("input", {
      type: "password",
      class: "field",
      autocomplete: "off",
      placeholder: settings.has_api_key ? "設定済み（変更する場合のみ入力）" : "Gemini API キーを入力",
    });
    const removeKey = h("input", { type: "checkbox" });
    const model = new ModelPicker(settings.model, "Gemini モデル");
    const modelStatus = h("p", { class: "hint" });
    const refreshModels = h("button", { type: "button" }, "モデル一覧を更新");
    let loading = false;
    const renderModels = (error?: string) => {
      const choices = new Map<string, { label: string; meta: string }>();
      choices.set("gemini-flash-latest", { label: "Gemini Flash Latest", meta: "デフォルト · 最新の Flash" });
      for (const item of models) {
        if (item.id !== "gemini-flash-latest") choices.set(item.id, { label: item.display_name, meta: describeModel(item) });
      }
      if (!choices.has(settings.model)) choices.set(settings.model, { label: settings.model, meta: "現在の設定" });
      model.setChoices([...choices].map(([id, c]) => ({ id, ...c })));
      modelStatus.textContent = loading
        ? "モデル一覧を取得しています…"
        : error
          ? error
          : models.length
        ? `文章生成に使える ${models.length} 個のモデルを取得しました（画像・音声生成などの専用モデルは除外）。リリース日は Gemini API から取得できないため、バージョンと入出力の上限を表示しています。`
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
    // 執筆嗜好の分析（#10）。設定はその場で保存する。
    const autoAnalyze = h("input", { type: "checkbox", checked: prefs.auto_analyze });
    const autoApply = h("input", { type: "checkbox", checked: prefs.auto_update });
    autoAnalyze.addEventListener("change", () => void api.updatePreferenceSettings({ auto_analyze: autoAnalyze.checked }).catch(() => undefined));
    autoApply.addEventListener("change", () => void api.updatePreferenceSettings({ auto_update: autoApply.checked }).catch(() => undefined));
    const prefSection = h(
      "div", {},
      h("h3", {}, "執筆嗜好の分析"),
      h("label", { class: "check" }, autoAnalyze, "分析を自動で実行する"),
      h("p", { class: "hint" }, "未分析の操作が 5 件以上たまり、最後の操作から 10 分経ったときに実行します（間隔は 1 時間以上）。API の利用上限に達している間は実行しません。実行状況は「執筆の嗜好」画面で確認できます。"),
      h("label", { class: "check" }, autoApply, "分析結果を承認なしでプロファイルに反映する"),
    );
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
            model: model.value,
            monthly_token_limit: Math.max(0, Math.floor(Number(limit.value) || 0)),
            warn_percent: Math.floor(Number(warn.value) || 0),
          };
          if (local && removeKey.checked) result.api_key = "";
          else if (local && key.value.trim()) result.api_key = key.value.trim();
          close(result);
        },
      },
      h("h2", {}, "AI 設定・使用量"),
      local
        ? h(
            "label", { class: "form-row" },
            h("span", {}, "Gemini API キー（この PC にのみ保存され、画面には表示されません）"),
            key,
          )
        : h(
            "p", { class: "hint" },
            settings.has_api_key ? "API キーは PC に登録済みです。" : "API キーが未登録です。",
            "API キーの登録・変更は、安全のため PC 本体の画面からのみ行えます。",
          ),
      local && settings.has_api_key ? h("label", { class: "check" }, removeKey, "保存済みの API キーを削除する") : null,
      h("div", { class: "form-row" }, h("span", {}, "Gemini モデル"), model.element),
      h("div", { class: "form-row" }, refreshModels, modelStatus),
      h("label", { class: "form-row" }, h("span", {}, "月あたりのトークン上限（0 で無制限。到達すると AI の利用を自動停止）"), limit),
      h("label", { class: "form-row" }, h("span", {}, "警告を出す使用率（%）"), warn),
      prefSection,
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
