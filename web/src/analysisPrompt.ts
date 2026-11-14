// 執筆嗜好の分析を、作業が進んだタイミングでスナックバーで案内する。
//
// 分析の材料は「AI の提案への操作（承認・修正して採用・却下）」なので、案内の条件も
// 未分析の操作の件数で判定する。案内を閉じた場合は、その時点からさらに操作が
// たまるまで再表示しない。表示の状態はプロジェクト単位で、この端末に保存する。

import { api } from "./api";
import { snackbar } from "./dom";

/** 初めての分析を案内する、未分析の操作の件数。 */
export const FIRST_THRESHOLD = 5;
/** 2 回目以降（すでにルールがある場合）の分析を案内する件数。 */
export const REPEAT_THRESHOLD = 10;

const key = (projectId: string) => `scenote.analysisPrompt.${projectId}`;

/**
 * 案内を出すべきか判定する。
 * `dismissedAt` は、前回案内を閉じたときの未分析の件数（閉じていなければ null）。
 */
export function shouldPrompt(unanalyzed: number, hasRules: boolean, dismissedAt: number | null): boolean {
  const threshold = hasRules ? REPEAT_THRESHOLD : FIRST_THRESHOLD;
  if (unanalyzed < threshold) return false;
  // 分析すると未分析の件数は 0 に戻る。閉じたときより件数が減っていれば、その記録は古い。
  if (dismissedAt === null || unanalyzed < dismissedAt) return true;
  return unanalyzed >= dismissedAt + threshold;
}

let checking = false;

/** 条件を満たしていれば案内を表示する。`analyze` は「分析する」を押したときの処理。 */
export async function maybePromptAnalysis(projectId: string, analyze: () => void): Promise<void> {
  if (checking) return;
  checking = true;
  try {
    const profile = await api.getPreferences(projectId);
    // 自動分析が有効なら、案内は出さない（サーバーが条件を見て実行する）。
    if (profile.settings.auto_analyze) return;
    const stored = localStorage.getItem(key(projectId));
    const dismissedAt = stored === null ? null : Number(stored);
    const hasRules = profile.project_rules.some((r) => r.source === "ai") || profile.suggestions.length > 0;
    if (!shouldPrompt(profile.unanalyzed, hasRules, Number.isFinite(dismissedAt) ? dismissedAt : null)) return;
    const dismiss = () => localStorage.setItem(key(projectId), String(profile.unanalyzed));
    snackbar(
      `AI の提案への操作が ${profile.unanalyzed} 件たまりました。執筆の嗜好を分析しますか？`,
      [
        { label: "分析する", primary: true, action: () => { localStorage.removeItem(key(projectId)); analyze(); } },
        { label: "あとで", action: dismiss },
      ],
      dismiss,
    );
  } catch {
    // 案内は補助的な機能なので、取得に失敗しても何もしない。
  } finally {
    checking = false;
  }
}
