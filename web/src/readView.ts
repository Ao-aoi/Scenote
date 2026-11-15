// 作品全体を章・場面の境界を越えて連続スクロールで読む画面（#7）。
//
// - 章・場面は見出しで区切って順に並べる。左の一覧は目次として働き、選ぶとその位置へ移動する。
// - 長い作品でも重くならないよう、場面の本文は画面に近づいたものから読み込む。
// - 編集は従来どおり場面単位で行う（「この場面を編集」で場面の編集画面を開く）。保存・競合検出の仕組みは変えない。

import { api } from "./api";
import { h } from "./dom";
import { icon } from "./icons";
import { kindLabel, type ProjectTree, type ScenarioElement, type SceneDetail } from "./types";

export interface ReadViewDeps {
  /** 場面の編集画面を開く。 */
  editScene: (sceneId: string) => void;
  /** 画面の先頭にある場面が変わったとき（目次の強調用）。 */
  onCurrentChange: (sceneId: string | null) => void;
}

function str(v: unknown): string {
  return typeof v === "string" ? v : "";
}

export class ReadView {
  private tree: ProjectTree | null = null;
  private sections = new Map<string, HTMLElement>();
  private loaded = new Map<string, SceneDetail>();
  private loader: IntersectionObserver | null = null;
  private frame = 0;
  private readonly onScroll = () => {
    if (this.frame) return;
    this.frame = requestAnimationFrame(() => {
      this.frame = 0;
      this.updateCurrent();
    });
  };
  currentSceneId: string | null = null;

  constructor(
    private readonly host: HTMLElement,
    private readonly scroller: HTMLElement,
    private readonly deps: ReadViewDeps,
  ) {}

  /** 作品全体を表示する。`focusSceneId` を指定するとその場面へ移動する。 */
  async open(tree: ProjectTree, focusSceneId: string | null = null): Promise<void> {
    this.tree = tree;
    this.loaded.clear();
    this.render();
    if (focusSceneId) await this.scrollTo(focusSceneId);
    else this.scroller.scrollTop = 0;
  }

  /** 他の端末などで内容が変わったとき。スクロール位置を保ったまま読み直す。 */
  async refresh(tree: ProjectTree): Promise<void> {
    const anchor = this.currentSceneId;
    const offset = anchor ? (this.sections.get(anchor)?.getBoundingClientRect().top ?? 0) : 0;
    this.tree = tree;
    const ids = new Set(this.sceneIds());
    const reload = [...this.loaded.keys()].filter((id) => ids.has(id));
    this.loaded.clear();
    this.render();
    await Promise.all(reload.map((id) => this.load(id)));
    const section = anchor ? this.sections.get(anchor) : null;
    if (section) {
      this.scroller.scrollTop += section.getBoundingClientRect().top - offset;
    }
  }

  close(): void {
    this.loader?.disconnect();
    this.loader = null;
    this.scroller.removeEventListener("scroll", this.onScroll);
  }

  /** 目次から選んだ場面の位置へ移動する（手前の場面を読み込んでから位置を合わせる）。 */
  async scrollTo(sceneId: string): Promise<void> {
    const ids = this.sceneIds();
    const index = ids.indexOf(sceneId);
    if (index < 0) return;
    // 手前の場面の高さが読み込みで変わって位置がずれないよう、先に読み込む。
    await Promise.all(ids.slice(Math.max(0, index - 3), index + 1).map((id) => this.load(id)));
    const section = this.sections.get(sceneId);
    if (!section) return;
    this.scroller.scrollTop += section.getBoundingClientRect().top - this.scroller.getBoundingClientRect().top - 8;
    this.setCurrent(sceneId);
  }

  private sceneIds(): string[] {
    return this.tree?.chapters.flatMap((c) => c.scenes.map((s) => s.id)) ?? [];
  }

  private render(): void {
    this.close();
    this.sections.clear();
    const tree = this.tree;
    if (!tree) {
      this.host.replaceChildren();
      return;
    }
    this.loader = new IntersectionObserver((entries) => {
      for (const e of entries) {
        if (e.isIntersecting) void this.load((e.target as HTMLElement).dataset.sceneId!);
      }
    }, { root: this.scroller, rootMargin: "1200px 0px" });
    this.scroller.addEventListener("scroll", this.onScroll, { passive: true });

    const chapters = tree.chapters.map((chapter) =>
      h(
        "section", { class: "read-chapter" },
        h("h2", { class: "read-chapter-title" }, chapter.title),
        ...(chapter.scenes.length
          ? chapter.scenes.map((scene) => {
            const body = h("div", { class: "read-body" }, h("p", { class: "muted read-loading" }, "読み込み中…"));
            const section = h(
              "section", { class: "read-scene", "data-scene-id": scene.id },
              h(
                "div", { class: "read-scene-head" },
                h("h3", { class: "read-scene-title" }, scene.title),
                h("button", {
                  type: "button",
                  class: "read-edit",
                  title: `「${scene.title}」を編集`,
                  onclick: () => this.deps.editScene(scene.id),
                }, "この場面を編集"),
              ),
              body,
            );
            this.sections.set(scene.id, section);
            this.loader!.observe(section);
            return section;
          })
          : [h("p", { class: "muted" }, "この章にはまだ場面がありません。")]),
      ),
    );
    this.host.replaceChildren(
      h("div", { class: "scene-header" }, h("h1", { class: "view-title" }, "通して読む")),
      h("p", { class: "hint" }, "章・場面をまたいで本文を続けて表示します。左の一覧から場面を選ぶとその位置へ移動します。編集は「この場面を編集」から場面ごとに行います。"),
      ...(chapters.length ? chapters : [h("p", { class: "empty" }, "まだ章がありません。")]),
    );
  }

  /** 画面の上端を越えた最後の場面を「いま読んでいる場面」とする。 */
  private updateCurrent(): void {
    const top = this.scroller.getBoundingClientRect().top + 24;
    let current: string | null = null;
    for (const [id, section] of this.sections) {
      if (section.getBoundingClientRect().top <= top) current = id;
      else break;
    }
    current ??= this.sceneIds()[0] ?? null;
    if (current) this.setCurrent(current);
  }

  private setCurrent(id: string): void {
    if (this.currentSceneId === id) return;
    this.currentSceneId = id;
    this.deps.onCurrentChange(id);
  }

  private async load(sceneId: string): Promise<void> {
    if (this.loaded.has(sceneId)) return;
    const section = this.sections.get(sceneId);
    if (!section) return;
    let scene: SceneDetail;
    try {
      scene = await api.getScene(sceneId);
    } catch {
      section.querySelector(".read-body")?.replaceChildren(h("p", { class: "muted" }, "読み込めませんでした。"));
      return;
    }
    // 読み込み中に画面が作り直された場合は捨てる。
    if (this.sections.get(sceneId) !== section) return;
    this.loaded.set(sceneId, scene);
    const body = section.querySelector(".read-body")!;
    body.replaceChildren(
      ...(scene.elements.length
        ? scene.elements.map((e) => this.renderElement(e))
        : [h("p", { class: "muted" }, scene.summary ? `（本文なし・概要: ${scene.summary}）` : "（本文なし）")]),
    );
  }

  private sceneTitle(id: unknown): string {
    for (const c of this.tree?.chapters ?? []) {
      for (const s of c.scenes) if (s.id === id) return `${c.title} / ${s.title}`;
    }
    return "";
  }

  private renderElement(e: ScenarioElement): HTMLElement {
    const cls = `read-el read-${e.kind} kind-${e.kind}`;
    switch (e.kind) {
      case "Dialogue":
        return h("p", { class: cls }, e.speaker ? h("span", { class: "read-speaker" }, e.speaker) : null, `「${e.body}」`);
      case "Narration":
        return h("p", { class: cls }, e.body);
      case "CharacterAction":
        return h("p", { class: cls }, e.speaker ? h("span", { class: "read-speaker" }, e.speaker) : null, e.body);
      case "Choice": {
        const options = Array.isArray(e.attrs.options) ? (e.attrs.options as Record<string, unknown>[]) : [];
        return h(
          "div", { class: cls },
          e.body ? h("p", {}, e.body) : null,
          options.length
            ? h("ul", { class: "read-options" }, ...options.map((o) => {
              const target = this.sceneTitle(o.target);
              return h("li", {}, str(o.text), target ? h("span", { class: "read-meta" }, " → ", target) : null);
            }))
            : null,
        );
      }
      case "Transition": {
        const target = this.sceneTitle(e.attrs.target);
        const condition = str(e.attrs.condition);
        return h("p", { class: cls }, icon("down"), target ? `${target} へ` : e.body || "遷移", condition ? h("span", { class: "read-meta" }, `（${condition}）`) : null);
      }
      case "VariableChange":
        return h("p", { class: cls }, str(e.attrs.effects) || e.body);
      default:
        return h("p", { class: cls }, h("span", { class: "read-meta" }, `[${kindLabel(e.kind)}] `), e.body);
    }
  }
}
