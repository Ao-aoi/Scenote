// アプリ共通のアイコン。外部素材は使わず、同じ線幅・サイズの線画として自前で定義している。
// アイコンを追加する場合は、24×24 の枠・線幅 1.8・角丸の線端にそろえること。

import { h } from "./dom";

const PATHS = {
  menu: ["M4 7h16", "M4 12h16", "M4 17h16"],
  settings: ["M4 7h9", "M17 7h3", "M4 17h3", "M11 17h9", "M15 4.8a2.2 2.2 0 1 0 0 4.4a2.2 2.2 0 0 0 0-4.4z", "M9 14.8a2.2 2.2 0 1 0 0 4.4a2.2 2.2 0 0 0 0-4.4z"],
  history: ["M4.5 12a7.5 7.5 0 1 0 2.4-5.5", "M4 4.5v3.2h3.2", "M12 8v4.2l2.8 1.8"],
  export: ["M12 4v10", "M8 10.5l4 4l4-4", "M5 15.5V19h14v-3.5"],
  ai: ["M5 6.5h14v9.5h-7.5L8 19.5V16H5z", "M9 10h6", "M9 12.8h4"],
} as const;

export type IconName = keyof typeof PATHS;

export function icon(name: IconName): SVGSVGElement {
  const ns = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(ns, "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("class", "ico");
  svg.setAttribute("aria-hidden", "true");
  for (const d of PATHS[name]) {
    const path = document.createElementNS(ns, "path");
    path.setAttribute("d", d);
    svg.append(path);
  }
  return svg;
}

/**
 * アイコンとラベルを持つボタン。幅が狭い画面ではラベルを隠してアイコンだけにする（CSS で切り替え）。
 * ラベルを隠しても意味が分かるよう、title と aria-label を常に付ける。
 */
export function iconButton(name: IconName, label: string, description = label): HTMLButtonElement {
  return h(
    "button",
    { type: "button", class: "icon-button", title: description, "aria-label": description },
    icon(name),
    h("span", { class: "icon-label" }, label),
  );
}
