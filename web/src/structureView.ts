// 構造の画面: 場面・分岐グラフ、キャラクター相関図、変数の管理、構造チェック。
//
// グラフはサーバーが生成したデータ（場面・辺）を、ここで SVG に配置して描く。

import { api } from "./api";
import { askConfirm, h, openDialog, toast } from "./dom";
import { errorMessage } from "./sceneEditor";
import { VAR_TYPE_LABELS, type GraphEdge, type GraphNode, type Structure, type Variable } from "./types";

const SVG_NS = "http://www.w3.org/2000/svg";

function svg<K extends keyof SVGElementTagNameMap>(tag: K, attrs: Record<string, string | number> = {}, ...children: (Node | string)[]): SVGElementTagNameMap[K] {
  const el = document.createElementNS(SVG_NS, tag);
  for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, String(v));
  el.append(...children);
  return el;
}

function truncate(s: string, n: number): string {
  return s.length > n ? `${s.slice(0, n - 1)}…` : s;
}

type Tab = "graph" | "characters" | "variables" | "issues";

const NODE_W = 168;
const NODE_H = 54;
const COL_GAP = 96;
const ROW_GAP = 28;
const PAD = 24;

/**
 * 場面を左から右へ並べる。最初の場面からの距離（最短の辺の数）を列にし、
 * 同じ列の中はプロジェクト内の順番で並べる。たどり着けない場面は最後の列に置く。
 */
export function layoutGraph(nodes: GraphNode[], edges: GraphEdge[]): Map<string, { x: number; y: number; col: number }> {
  const depth = new Map<string, number>();
  const start = nodes.find((n) => n.is_start);
  if (start) {
    depth.set(start.id, 0);
    const queue = [start.id];
    while (queue.length) {
      const cur = queue.shift()!;
      for (const e of edges) {
        if (e.from === cur && !depth.has(e.to)) {
          depth.set(e.to, depth.get(cur)! + 1);
          queue.push(e.to);
        }
      }
    }
  }
  const maxDepth = Math.max(0, ...depth.values());
  const columns = new Map<number, GraphNode[]>();
  for (const n of [...nodes].sort((a, b) => a.order - b.order)) {
    const col = depth.get(n.id) ?? maxDepth + 1;
    columns.set(col, [...(columns.get(col) ?? []), n]);
  }
  const pos = new Map<string, { x: number; y: number; col: number }>();
  for (const [col, list] of columns) {
    list.forEach((n, row) => pos.set(n.id, { x: PAD + col * (NODE_W + COL_GAP), y: PAD + 20 + row * (NODE_H + ROW_GAP), col }));
  }
  return pos;
}

export interface StructureViewOptions {
  openScene: (sceneId: string) => void;
}

export class StructureView {
  private projectId: string | null = null;
  private tab: Tab = "graph";
  private data: Structure | null = null;

  constructor(
    private readonly root: HTMLElement,
    private readonly options: StructureViewOptions,
  ) {}

  async open(projectId: string): Promise<void> {
    this.projectId = projectId;
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
    try {
      this.data = await api.getStructure(this.projectId);
    } catch (e) {
      this.root.replaceChildren(h("p", { class: "error-text" }, errorMessage(e)));
      return;
    }
    const d = this.data;
    const errors = d.issues.filter((i) => i.severity === "error").length;
    const warnings = d.issues.filter((i) => i.severity === "warning").length;
    const tab = (id: Tab, label: string) =>
      h("button", {
        type: "button",
        class: this.tab === id ? "seg active" : "seg",
        "aria-pressed": String(this.tab === id),
        onclick: () => void this.guard(async () => {
          this.tab = id;
          await this.render();
        }),
      }, label);
    const body = h("div", { class: "structure-body" });
    this.root.replaceChildren(
      h(
        "div", { class: "scene-header" },
        h("h1", { class: "view-title" }, "構造"),
        h(
          "div", { class: "segmented", role: "group", "aria-label": "表示" },
          tab("graph", "場面と分岐"),
          tab("characters", "キャラクター"),
          tab("variables", "変数"),
          tab("issues", `チェック${errors + warnings ? `（${errors + warnings}）` : ""}`),
        ),
      ),
      body,
    );
    if (this.tab === "graph") this.renderGraph(body, d);
    else if (this.tab === "characters") this.renderCharacters(body, d);
    else if (this.tab === "variables") this.renderVariables(body, d);
    else this.renderIssues(body, d);
  }

  // ---- 場面・分岐グラフ -----------------------------------------------

  private renderGraph(body: HTMLElement, d: Structure): void {
    if (d.nodes.length === 0) {
      body.append(h("p", { class: "empty" }, "場面がありません。"));
      return;
    }
    const pos = layoutGraph(d.nodes, d.edges);
    const maxX = Math.max(...[...pos.values()].map((p) => p.x)) + NODE_W + PAD;
    const maxY = Math.max(...[...pos.values()].map((p) => p.y)) + NODE_H + PAD + 30;
    const root = svg("svg", { width: maxX, height: maxY, viewBox: `0 0 ${maxX} ${maxY}`, class: "graph", role: "img", "aria-label": "場面と分岐のグラフ" });
    const defs = svg("defs");
    for (const kind of ["next", "choice", "transition"]) {
      defs.append(
        svg("marker", { id: `arrow-${kind}`, viewBox: "0 0 10 10", refX: 9, refY: 5, markerWidth: 7, markerHeight: 7, orient: "auto-start-reverse" },
          svg("path", { d: "M0,0 L10,5 L0,10 z", class: `arrow arrow-${kind}` })),
      );
    }
    root.append(defs);

    // 同じ 2 場面間の辺が重ならないよう、少しずつずらす。
    const pairCount = new Map<string, number>();
    const edgeLayer = svg("g");
    const labelLayer = svg("g");
    for (const e of d.edges) {
      const a = pos.get(e.from);
      const b = pos.get(e.to);
      if (!a || !b) continue;
      const key = `${e.from}>${e.to}`;
      const nth = pairCount.get(key) ?? 0;
      pairCount.set(key, nth + 1);
      const offset = nth * 10;
      let path: string;
      let lx: number;
      let ly: number;
      if (b.col > a.col) {
        const x1 = a.x + NODE_W, y1 = a.y + NODE_H / 2 + offset;
        const x2 = b.x, y2 = b.y + NODE_H / 2 + offset;
        const mid = (x1 + x2) / 2;
        path = `M${x1},${y1} C${mid},${y1} ${mid},${y2} ${x2},${y2}`;
        lx = mid;
        ly = (y1 + y2) / 2 - 4;
      } else if (b.col === a.col) {
        // 同じ列（下へ進む・上へ戻る）。右側を回り込む。
        const x1 = a.x + NODE_W, y1 = a.y + NODE_H / 2;
        const x2 = b.x + NODE_W, y2 = b.y + NODE_H / 2;
        const bulge = x1 + 40 + offset;
        path = `M${x1},${y1} C${bulge},${y1} ${bulge},${y2} ${x2},${y2}`;
        lx = bulge - 10;
        ly = (y1 + y2) / 2;
      } else {
        // 前の場面へ戻る辺。下側を回り込む。
        const x1 = a.x + NODE_W / 2, y1 = a.y + NODE_H;
        const x2 = b.x + NODE_W / 2, y2 = b.y + NODE_H;
        const low = Math.max(y1, y2) + 40 + offset;
        path = `M${x1},${y1} C${x1},${low} ${x2},${low} ${x2},${y2}`;
        lx = (x1 + x2) / 2;
        ly = low - 8;
      }
      const title = [e.label, e.condition ? `条件: ${e.condition}` : "", e.effects ? `変更: ${e.effects}` : ""].filter(Boolean).join("\n");
      const line = svg("path", { d: path, class: `edge edge-${e.kind}`, "marker-end": `url(#arrow-${e.kind})` }, svg("title", {}, title || (e.kind === "next" ? "次の場面へ" : "")));
      edgeLayer.append(line);
      const text = e.kind === "choice" ? e.label : e.kind === "transition" ? e.label || "遷移" : "";
      if (text) {
        labelLayer.append(svg("text", { x: lx, y: ly, class: `edge-label edge-label-${e.kind}`, "text-anchor": "middle" }, truncate(text + (e.condition && e.kind === "choice" ? " ※" : ""), 14)));
      }
    }
    root.append(edgeLayer, labelLayer);

    for (const n of d.nodes) {
      const p = pos.get(n.id)!;
      const cls = ["node", n.is_start ? "node-start" : "", n.is_end ? "node-end" : "", n.reachable ? "" : "node-unreachable"].filter(Boolean).join(" ");
      const g = svg("g", { class: cls, transform: `translate(${p.x},${p.y})`, tabindex: 0, role: "button", "aria-label": `${n.chapter_title} / ${n.title} を開く` });
      g.append(
        svg("rect", { width: NODE_W, height: NODE_H, rx: 8 }),
        svg("text", { x: 10, y: 20, class: "node-title" }, truncate(n.title, 12)),
        svg("text", { x: 10, y: 40, class: "node-meta" }, truncate(`${n.chapter_title} ・ ${n.element_count}要素${n.is_start ? " ・ 開始" : ""}${n.is_end ? " ・ 終わり" : ""}`, 22)),
        svg("title", {}, `${n.chapter_title} / ${n.title}${n.reachable ? "" : "\n最初の場面からたどり着けません"}`),
      );
      const open = () => this.options.openScene(n.id);
      g.addEventListener("click", open);
      g.addEventListener("keydown", (ev) => {
        if ((ev as KeyboardEvent).key === "Enter") open();
      });
      root.append(g);
    }

    body.append(
      h(
        "div", { class: "graph-legend" },
        h("span", { class: "legend legend-next" }, "次の場面へ"),
        h("span", { class: "legend legend-choice" }, "選択肢（※ 条件つき）"),
        h("span", { class: "legend legend-transition" }, "遷移"),
        h("span", { class: "legend legend-unreachable" }, "たどり着けない場面"),
      ),
      h("div", { class: "graph-scroll" }, root),
      h("p", { class: "hint" }, "場面を押すと開きます。線にカーソルを合わせると、条件と変数の変更を確認できます。分岐は、選択肢・遷移の要素で移動先を設定すると表示されます。"),
    );
  }

  // ---- キャラクター相関図 ---------------------------------------------

  private renderCharacters(body: HTMLElement, d: Structure): void {
    if (d.characters.length === 0) {
      body.append(h("p", { class: "empty" }, "台詞・動作にキャラクター名が設定されていません。"));
      return;
    }
    const size = Math.max(360, Math.min(720, 140 + d.characters.length * 50));
    const cx = size / 2, cy = size / 2, r = size / 2 - 70;
    const maxLines = Math.max(...d.characters.map((c) => c.lines));
    const maxShared = Math.max(1, ...d.relations.map((x) => x.shared_scenes));
    const at = new Map<string, { x: number; y: number }>();
    d.characters.forEach((c, i) => {
      const a = (2 * Math.PI * i) / d.characters.length - Math.PI / 2;
      at.set(c.name, d.characters.length === 1 ? { x: cx, y: cy } : { x: cx + r * Math.cos(a), y: cy + r * Math.sin(a) });
    });
    const root = svg("svg", { width: size, height: size, viewBox: `0 0 ${size} ${size}`, class: "graph", role: "img", "aria-label": "キャラクター相関図" });
    for (const rel of d.relations) {
      const a = at.get(rel.a)!, b = at.get(rel.b)!;
      root.append(
        svg("line", { x1: a.x, y1: a.y, x2: b.x, y2: b.y, class: "relation", "stroke-width": 1 + (5 * rel.shared_scenes) / maxShared },
          svg("title", {}, `${rel.a} と ${rel.b}: 同じ場面に ${rel.shared_scenes} 回登場`)),
      );
    }
    for (const c of d.characters) {
      const p = at.get(c.name)!;
      const radius = 14 + 18 * Math.sqrt(c.lines / maxLines);
      root.append(
        svg("g", { class: "person" },
          svg("circle", { cx: p.x, cy: p.y, r: radius }),
          svg("text", { x: p.x, y: p.y + radius + 16, "text-anchor": "middle", class: "node-title" }, truncate(c.name, 10)),
          svg("title", {}, `${c.name}: 台詞・動作 ${c.lines} 件、${c.scenes} 場面に登場`)),
      );
    }
    const table = h(
      "table", { class: "usage-table" },
      h("thead", {}, h("tr", {}, h("th", {}, "キャラクター"), h("th", {}, "台詞・動作"), h("th", {}, "登場場面"))),
      h("tbody", {}, ...d.characters.map((c) => h("tr", {}, h("td", {}, c.name), h("td", {}, String(c.lines)), h("td", {}, String(c.scenes))))),
    );
    body.append(
      h("p", { class: "hint" }, "台詞・動作のキャラクター名から自動で作った図です。線の太さは、同じ場面に登場した回数を表します（関係の内容までは表しません）。"),
      h("div", { class: "graph-scroll center" }, root),
      table,
    );
  }

  // ---- 変数 -----------------------------------------------------------

  private renderVariables(body: HTMLElement, d: Structure): void {
    const usage = new Map(d.variable_usage.map((u) => [u.name, u]));
    const add = h("button", { type: "button", class: "primary" }, "＋ 変数を追加");
    add.addEventListener("click", () => void this.guard(() => this.editVariable(null)));
    const rows = d.variables.map((v) => {
      const u = usage.get(v.name);
      return h(
        "tr", {},
        h("td", {}, h("strong", {}, v.name), v.description ? h("div", { class: "muted pref-meta" }, v.description) : ""),
        h("td", {}, VAR_TYPE_LABELS[v.var_type] ?? v.var_type),
        h("td", {}, v.initial || "—"),
        h("td", {}, u?.written_in.join("、") || "—"),
        h("td", {}, u?.read_in.join("、") || "—"),
        h(
          "td", { class: "var-actions" },
          h("button", { type: "button", onclick: () => void this.guard(() => this.editVariable(v)) }, "編集"),
          h("button", {
            type: "button",
            class: "danger-text",
            onclick: () => void this.guard(async () => {
              if (!(await askConfirm(`変数「${v.name}」を削除しますか？条件式・変更式で使っている箇所はエラーとして表示されます。`))) return;
              await api.deleteVariable(v.id);
              await this.render();
            }),
          }, "削除"),
        ),
      );
    });
    body.append(
      h("p", { class: "hint" }, "フラグ・好感度・所持金などを定義し、選択肢や遷移の条件式（例: 好感度 >= 3 and 鍵）と変更式（例: 好感度 += 1）で使います。"),
      add,
      rows.length
        ? h(
            "table", { class: "usage-table var-table" },
            h("thead", {}, h("tr", {}, h("th", {}, "変数"), h("th", {}, "型"), h("th", {}, "初期値"), h("th", {}, "変更する場面"), h("th", {}, "条件で使う場面"), h("th", {}, ""))),
            h("tbody", {}, ...rows),
          )
        : h("p", { class: "empty" }, "変数はまだありません。"),
      h(
        "details", { class: "syntax-help" },
        h("summary", {}, "条件式・変更式の書き方"),
        h("ul", {},
          h("li", {}, "比較: ", h("code", {}, "=="), " ", h("code", {}, "!="), " ", h("code", {}, "<"), " ", h("code", {}, "<="), " ", h("code", {}, ">"), " ", h("code", {}, ">=")),
          h("li", {}, "組み合わせ: ", h("code", {}, "and"), " ", h("code", {}, "or"), " ", h("code", {}, "not"), "、かっこ ", h("code", {}, "( )")),
          h("li", {}, "値: 数値、", h("code", {}, "true"), " / ", h("code", {}, "false"), "、文字列 ", h("code", {}, "\"先輩\"")),
          h("li", {}, "変更: ", h("code", {}, "好感度 += 1"), "、", h("code", {}, "鍵 = true"), "。複数は ", h("code", {}, ";"), " で区切る")),
      ),
    );
  }

  private async editVariable(v: Variable | null): Promise<void> {
    if (!this.projectId) return;
    const value = await openDialog<{ name: string; var_type: string; initial: string; description: string }>((close) => {
      const name = h("input", { type: "text", class: "field", value: v?.name ?? "", maxLength: 50, required: true, placeholder: "例: 好感度" });
      const type = h(
        "select", { class: "field" },
        ...Object.entries(VAR_TYPE_LABELS).map(([id, label]) => h("option", { value: id, selected: id === (v?.var_type ?? "number") }, label)),
      );
      const initial = h("input", { type: "text", class: "field", value: v?.initial ?? "", placeholder: "例: 0、false、\"名無し\"" });
      const description = h("input", { type: "text", class: "field", value: v?.description ?? "", maxLength: 200 });
      setTimeout(() => name.focus());
      return h(
        "form",
        {
          onsubmit: (e: Event) => {
            e.preventDefault();
            close({ name: name.value.trim(), var_type: type.value, initial: initial.value.trim(), description: description.value });
          },
        },
        h("h2", {}, v ? "変数を編集" : "変数を追加"),
        h("label", { class: "form-row" }, h("span", {}, "名前（文字か _ で始まる。空白は使えません）"), name),
        h("label", { class: "form-row" }, h("span", {}, "型"), type),
        h("label", { class: "form-row" }, h("span", {}, "初期値（任意）"), initial),
        h("label", { class: "form-row" }, h("span", {}, "説明（任意）"), description),
        h(
          "div", { class: "dialog-actions" },
          h("button", { type: "button", onclick: () => close(null) }, "キャンセル"),
          h("button", { type: "submit", class: "primary" }, "保存"),
        ),
      );
    });
    if (!value) return;
    if (v) await api.updateVariable(v.id, value);
    else await api.createVariable(this.projectId, value);
    await this.render();
  }

  // ---- 構造チェック ---------------------------------------------------

  private renderIssues(body: HTMLElement, d: Structure): void {
    const titles = new Map(d.nodes.map((n) => [n.id, `${n.chapter_title} / ${n.title}`]));
    const label = { error: "エラー", warning: "注意", info: "情報" };
    if (d.issues.length === 0) {
      body.append(h("p", { class: "empty" }, "問題は見つかりませんでした。"));
      return;
    }
    body.append(
      h("p", { class: "hint" }, "移動先・条件式・変数の参照・場面のつながりを検査した結果です。項目を押すと該当する場面を開きます。"),
      h(
        "div", { class: "issue-list" },
        ...d.issues.map((i) =>
          h(
            "button",
            {
              type: "button",
              class: `issue issue-${i.severity}`,
              disabled: !i.scene_id,
              onclick: () => i.scene_id && this.options.openScene(i.scene_id),
            },
            h("span", { class: `badge badge-${i.severity}` }, label[i.severity]),
            h("span", { class: "issue-text" }, i.message),
            i.scene_id ? h("span", { class: "muted issue-scene" }, titles.get(i.scene_id) ?? "") : "",
          ),
        ),
      ),
    );
  }
}
