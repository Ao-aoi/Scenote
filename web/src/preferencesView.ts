// 執筆嗜好プロファイルの画面。
//
// 分析（AI による傾向の抽出）と、プロファイルへの適用を分けて表示する。自動更新が OFF の場合、
// 分析結果は「更新案」として並び、承認したものだけがルールになる。

import { api } from "./api";
import { askConfirm, h, openDialog, toast } from "./dom";
import { formatTime } from "./historyView";
import { errorMessage } from "./sceneEditor";
import {
  CATEGORY_LABELS,
  type PreferenceProfile,
  type PreferenceRule,
  type PreferenceSuggestion,
  type Project,
} from "./types";

const SOURCE_LABELS: Record<string, string> = {
  user: "手動",
  ai: "分析",
  copied: "複製",
};

/** 分析の実行状況（最終実行日時・失敗）を表示する。 */
export function statusLine(profile: PreferenceProfile): HTMLElement {
  const s = profile.status;
  const parts: (string | HTMLElement)[] = [];
  if (s.state === "running") parts.push("分析を実行中です…");
  if (s.last_success_at) parts.push(`最終分析: ${formatTime(s.last_success_at)}${s.last_trigger === "auto" ? "（自動）" : ""}`);
  if (s.state === "error" && s.last_error_at) {
    parts.push(h("span", { class: "error-text" }, `前回の分析に失敗しました（${formatTime(s.last_error_at)}）: ${s.last_error ?? ""}。以前のルールはそのまま残っています。`));
  }
  if (!parts.length) parts.push("まだ分析していません。");
  return h("p", { class: "muted pref-meta analysis-status" }, ...parts.flatMap((p, i) => (i ? [" ・ ", p] : [p])));
}

function confidenceLabel(c: number): string {
  const level = c >= 0.7 ? "強い" : c >= 0.45 ? "中" : "弱い";
  return `確信度 ${level}（${Math.round(c * 100)}%）`;
}

function categorySelect(value: string): HTMLSelectElement {
  return h(
    "select", { class: "field" },
    ...Object.entries(CATEGORY_LABELS).map(([id, label]) => h("option", { value: id, selected: id === value }, label)),
  );
}

interface RuleForm {
  text: string;
  category: string;
  confidence?: number;
}

function askRule(title: string, initial: RuleForm, withConfidence: boolean): Promise<RuleForm | null> {
  return openDialog<RuleForm>((close) => {
    const text = h("textarea", { class: "field", rows: 3, maxLength: 300, value: initial.text, required: true });
    const category = categorySelect(initial.category);
    const confidence = h("input", {
      type: "range",
      min: "0",
      max: "100",
      step: "5",
      value: String(Math.round((initial.confidence ?? 1) * 100)),
    });
    const confidenceText = h("span", { class: "muted" }, `${confidence.value}%`);
    confidence.addEventListener("input", () => (confidenceText.textContent = `${confidence.value}%`));
    setTimeout(() => text.focus());
    return h(
      "form",
      {
        onsubmit: (e: Event) => {
          e.preventDefault();
          if (!text.value.trim()) return;
          close({
            text: text.value.trim(),
            category: category.value,
            confidence: withConfidence ? Number(confidence.value) / 100 : undefined,
          });
        },
      },
      h("h2", {}, title),
      h("label", { class: "form-row" }, h("span", {}, "ルール（AI への具体的な指示になる一文）"), text),
      h("label", { class: "form-row" }, h("span", {}, "分類"), category),
      withConfidence ? h("label", { class: "form-row" }, h("span", {}, "確信度"), confidence, confidenceText) : null,
      h(
        "div", { class: "dialog-actions" },
        h("button", { type: "button", onclick: () => close(null) }, "キャンセル"),
        h("button", { type: "submit", class: "primary" }, "保存"),
      ),
    );
  });
}

export class PreferencesView {
  private projectId: string | null = null;
  private projects: Project[] = [];
  private analyzing = false;
  private lastSummary: { summary: string; warnings: string[] } | null = null;

  constructor(private readonly root: HTMLElement) {}

  async open(projectId: string, projects: Project[]): Promise<void> {
    if (this.projectId !== projectId) this.lastSummary = null;
    this.projectId = projectId;
    this.projects = projects;
    await this.render();
  }

  private async guard(action: () => Promise<void>): Promise<void> {
    try {
      await action();
    } catch (e) {
      toast(errorMessage(e), "error");
    }
  }

  private async render(): Promise<void> {
    if (!this.projectId) return;
    let profile: PreferenceProfile;
    try {
      profile = await api.getPreferences(this.projectId);
    } catch (e) {
      this.root.replaceChildren(h("p", { class: "error-text" }, errorMessage(e)));
      return;
    }
    this.root.replaceChildren(
      h("div", { class: "scene-header" }, h("h1", { class: "view-title" }, "執筆の嗜好")),
      h(
        "p", { class: "hint" },
        "AI の提案を承認・修正・却下した結果から、好みの傾向を学習します。ルールは依頼のたびに AI へ伝えられます（共通 → このプロジェクト → チャットの追加指示・依頼 の順に、後のものが優先されます）。",
      ),
      this.renderAnalysis(profile),
      this.renderSuggestions(profile.suggestions) ?? "",
      this.renderRules("このプロジェクトのルール", profile.project_rules, this.projectId),
      this.renderRules("共通のルール（全プロジェクト）", profile.global_rules, null),
      this.renderImport() ?? "",
    );
  }

  private renderAnalysis(profile: PreferenceProfile): HTMLElement {
    const auto = h("input", { type: "checkbox", checked: profile.settings.auto_update });
    auto.addEventListener("change", () => void this.guard(async () => {
      await api.updatePreferenceSettings({ auto_update: auto.checked });
      toast(auto.checked ? "分析結果を自動でプロファイルに反映します" : "分析結果は承認してから反映します");
    }));
    const direct = h("input", { type: "checkbox", checked: profile.settings.include_direct_edits });
    direct.addEventListener("change", () => void this.guard(async () => {
      await api.updatePreferenceSettings({ include_direct_edits: direct.checked });
    }));
    const autoAnalyze = h("input", { type: "checkbox", checked: profile.settings.auto_analyze });
    autoAnalyze.addEventListener("change", () => void this.guard(async () => {
      await api.updatePreferenceSettings({ auto_analyze: autoAnalyze.checked });
      toast(autoAnalyze.checked ? "条件を満たしたときに分析を自動で実行します" : "分析は手動で実行します");
      await this.render();
    }));
    const running = this.analyzing || profile.status.state === "running";
    const button = h("button", {
      type: "button",
      class: "primary",
      disabled: running || profile.unanalyzed === 0,
    }, running ? "分析中…" : "分析する");
    button.addEventListener("click", () => void this.analyze());
    const summary = this.lastSummary;
    return h(
      "section", { class: "pref-section" },
      h("h2", {}, "分析"),
      h(
        "div", { class: "pref-analyze" },
        button,
        h("span", { class: "muted" }, profile.unanalyzed ? `未分析の操作 ${profile.unanalyzed} 件` : "新しい操作はありません"),
      ),
      statusLine(profile),
      h("label", { class: "check" }, autoAnalyze, "分析を自動で実行する（未分析の操作が 5 件以上たまり、最後の操作から 10 分経ったとき。間隔は 1 時間以上）"),
      h("label", { class: "check" }, auto, "分析結果を自動でプロファイルに反映する（オフの場合は更新案を確認して承認）"),
      h("label", { class: "check" }, direct, "自分で直接書いた文章も分析に含める"),
      summary
        ? h(
            "div", { class: "pref-summary" },
            h("p", {}, summary.summary || "分析が完了しました。"),
            summary.warnings.length ? h("ul", { class: "msg-warnings" }, ...summary.warnings.map((w) => h("li", {}, w))) : null,
          )
        : null,
    );
  }

  /** 分析を実行する（画面のボタンと、スナックバーの案内から呼ばれる）。 */
  async analyze(): Promise<void> {
    if (!this.projectId || this.analyzing) return;
    this.analyzing = true;
    await this.render();
    try {
      const result = await api.analyzePreferences(this.projectId);
      this.lastSummary = { summary: result.summary, warnings: result.warnings };
      if (result.suggestions.length === 0) toast("新しい傾向は見つかりませんでした");
      else if (result.applied) toast(`${result.suggestions.length} 件の更新をプロファイルに反映しました`);
      else toast(`${result.suggestions.length} 件の更新案があります。確認して承認してください`);
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      this.analyzing = false;
      await this.render();
    }
  }

  private renderSuggestions(suggestions: PreferenceSuggestion[]): HTMLElement | null {
    if (suggestions.length === 0) return null;
    const opLabel = { add: "追加", update: "更新", remove: "削除" };
    const item = (s: PreferenceSuggestion) =>
      h(
        "div", { class: "pref-rule pref-suggestion" },
        h(
          "div", { class: "pref-rule-main" },
          h("div", {}, h("span", { class: "actor" }, opLabel[s.op]), " ", s.op === "remove" ? h("del", {}, s.text) : s.text),
          s.op === "update" && s.rule_text ? h("div", { class: "muted pref-meta" }, `現在: ${s.rule_text}`) : null,
          h(
            "div", { class: "muted pref-meta" },
            `${CATEGORY_LABELS[s.category] ?? s.category} ・ ${confidenceLabel(s.confidence)} ・ 根拠 ${s.evidence.length} 件`,
            s.reason ? ` ・ ${s.reason}` : "",
          ),
        ),
        h(
          "div", { class: "pref-rule-actions" },
          h("button", {
            type: "button",
            class: "primary",
            onclick: () => void this.guard(async () => {
              const r = await api.approveSuggestion(s.id);
              if (r.status === "stale") toast("対象のルールが削除されているため、反映できませんでした", "error");
              await this.render();
            }),
          }, "承認"),
          h("button", {
            type: "button",
            onclick: () => void this.guard(async () => {
              await api.rejectSuggestion(s.id);
              await this.render();
            }),
          }, "却下"),
        ),
      );
    return h(
      "section", { class: "pref-section" },
      h("h2", {}, `承認待ちの更新案（${suggestions.length} 件）`),
      ...suggestions.map(item),
    );
  }

  private renderRules(title: string, rules: PreferenceRule[], scope: string | null): HTMLElement {
    const add = h("button", { type: "button", class: "add" }, "＋ ルールを追加");
    add.addEventListener("click", () => void this.guard(async () => {
      const value = await askRule("ルールを追加", { text: "", category: "style" }, false);
      if (!value) return;
      await api.createRule(scope, value.text, value.category);
      await this.render();
    }));
    const item = (r: PreferenceRule) =>
      h(
        "div", { class: "pref-rule" },
        h(
          "div", { class: "pref-rule-main" },
          h("div", {}, r.text),
          h(
            "div", { class: "muted pref-meta" },
            `${CATEGORY_LABELS[r.category] ?? r.category} ・ ${confidenceLabel(r.confidence)} ・ ${SOURCE_LABELS[r.source] ?? r.source}`,
            r.evidence.length ? ` ・ 根拠 ${r.evidence.length} 件` : "",
          ),
        ),
        h(
          "div", { class: "pref-rule-actions" },
          h("button", {
            type: "button",
            onclick: () => void this.guard(async () => {
              const value = await askRule("ルールを編集", r, true);
              if (!value) return;
              await api.updateRule(r.id, value);
              await this.render();
            }),
          }, "編集"),
          scope !== null
            ? h("button", {
                type: "button",
                title: "このルールを全プロジェクト共通のルールとして複製します",
                onclick: () => void this.guard(async () => {
                  const { copied } = await api.copyRule(r.id, null);
                  toast(copied ? "共通のルールに複製しました" : "同じルールが共通にすでにあります");
                  await this.render();
                }),
              }, "共通に複製")
            : null,
          h("button", {
            type: "button",
            class: "danger-text",
            onclick: () => void this.guard(async () => {
              if (!(await askConfirm(`ルール「${r.text}」を削除しますか？`))) return;
              await api.deleteRule(r.id);
              await this.render();
            }),
          }, "削除"),
        ),
      );
    return h(
      "section", { class: "pref-section" },
      h("h2", {}, title),
      ...(rules.length ? rules.map(item) : [h("p", { class: "muted" }, "ルールはまだありません。")]),
      add,
    );
  }

  private renderImport(): HTMLElement | null {
    const others = this.projects.filter((p) => p.id !== this.projectId);
    if (others.length === 0) return null;
    const select = h(
      "select", { class: "field", "aria-label": "取り込み元のプロジェクト" },
      ...others.map((p) => h("option", { value: p.id }, p.name)),
    );
    const button = h("button", { type: "button" }, "ルールを取り込む");
    button.addEventListener("click", () => void this.guard(async () => {
      if (!this.projectId) return;
      const { copied } = await api.importPreferences(this.projectId, select.value);
      toast(copied ? `${copied} 件のルールを取り込みました` : "取り込める新しいルールはありませんでした");
      await this.render();
    }));
    return h(
      "section", { class: "pref-section" },
      h("h2", {}, "他のプロジェクトから取り込む"),
      h("p", { class: "hint" }, "選んだプロジェクトのルールを、このプロジェクトのルールとして複製します。同じ文面のルールは重複させません。"),
      h("div", { class: "pref-import" }, select, button),
    );
  }
}
