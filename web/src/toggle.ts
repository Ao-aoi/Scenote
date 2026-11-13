// MdLite (Ao-aoi/mdlite) の src/toggle.ts を移植したもの。
/**
 * ソース表示での装飾のトグル（`**` / `*` / `~~` など）。
 * テキストと選択範囲だけを見て、追加または削除する変更を返す純粋関数。
 */

export interface Change {
  from: number
  to: number
  insert: string
}

export interface ToggleResult {
  changes: Change[]
  /** 変更適用後の選択範囲 */
  anchor: number
  head: number
}

/** `pos` から左（dir=-1）または右（dir=1）に、文字 `ch` が何個続くか */
function run(doc: string, pos: number, dir: 1 | -1, ch: string): number {
  let n = 0
  let i = dir === 1 ? pos : pos - 1
  while (i >= 0 && i < doc.length && doc[i] === ch) {
    n++
    i += dir
  }
  return n
}

/**
 * 連続する記号の数 `count` の中に、長さ `size` の記号が含まれているか。
 * `*` は `**`（太字）と同じ文字なので、斜体（1文字）は奇数個のときだけ「ある」とみなす。
 * 例: `*a*` → 斜体, `**a**` → 太字のみ, `***a***` → 太字＋斜体
 */
function contains(count: number, size: number, sharesWithDouble: boolean): boolean {
  if (count < size) return false
  if (size === 1 && sharesWithDouble) return count % 2 === 1
  return true
}

/**
 * @param markers 認識する記号。先頭が追加に使う記号で、残りは削除のときだけ認識する
 *                （例: 設定が `**` でも、既存の `__` は外せるようにする）
 */
export function toggleMarker(doc: string, from: number, to: number, markers: string[]): ToggleResult {
  if (from > to) [from, to] = [to, from]
  const primary = markers[0]

  for (const marker of markers) {
    const ch = marker[0]
    const size = marker.length
    // `*` と `_` は 1 文字（斜体）と 2 文字（太字）で同じ文字を使う
    const shares = ch === '*' || ch === '_'

    if (from === to) {
      // 空の記号ペアの間にカーソルがある → ペアを取り除く
      if (doc.slice(from - size, from) === marker && doc.slice(to, to + size) === marker) {
        const l = run(doc, from, -1, ch)
        const r = run(doc, to, 1, ch)
        if (l === r && contains(l, size, shares)) {
          return {
            changes: [{ from: from - size, to: to + size, insert: '' }],
            anchor: from - size,
            head: from - size,
          }
        }
      }
      continue
    }

    // 選択範囲の外側に記号がある: **[text]**
    const outL = run(doc, from, -1, ch)
    const outR = run(doc, to, 1, ch)
    if (contains(Math.min(outL, outR), size, shares) && (!shares || size === 2 || outL === outR)) {
      return {
        changes: [
          { from: from - size, to: from, insert: '' },
          { from: to, to: to + size, insert: '' },
        ],
        anchor: from - size,
        head: to - size,
      }
    }

    // 選択範囲の内側に記号がある: [**text**]
    const sel = doc.slice(from, to)
    const inL = run(sel, 0, 1, ch)
    const inR = run(sel, sel.length, -1, ch)
    if (sel.length >= size * 2 + 1 && contains(Math.min(inL, inR), size, shares) && (!shares || size === 2 || inL === inR)) {
      return {
        changes: [
          { from, to: from + size, insert: '' },
          { from: to - size, to, insert: '' },
        ],
        anchor: from,
        head: to - size * 2,
      }
    }
  }

  if (from === to) {
    return {
      changes: [{ from, to, insert: primary + primary }],
      anchor: from + primary.length,
      head: from + primary.length,
    }
  }
  return {
    changes: [
      { from, to: from, insert: primary },
      { from: to, to, insert: primary },
    ],
    anchor: from + primary.length,
    head: to + primary.length,
  }
}

/** 変更を文字列に適用する（テスト用・単純な実装） */
export function applyChanges(doc: string, changes: Change[]): string {
  return [...changes]
    .sort((a, b) => b.from - a.from)
    .reduce((d, c) => d.slice(0, c.from) + c.insert + d.slice(c.to), doc)
}
