//! 構造化データと Markdown の相互変換。記法の仕様は `docs/markdown-format.md` を参照。
//!
//! 方針:
//! - 空行で区切られた 1 ブロックが 1 要素に対応する。
//! - 行頭マーカーのないブロックは地の文として本文をそのまま保持する
//!   （未対応の Markdown 記法を削除・変換しない）。
//! - 本文中の空行や、マーカーと紛らわしい行は行頭の `\` でエスケープして往復可能にする。

use crate::model::{
    is_valid_kind, is_valid_speaker, ElementContent, KIND_CHARACTER_ACTION, KIND_CHOICE,
    KIND_DIALOGUE, KIND_DIRECTION, KIND_NARRATION,
};

/// 場面内の要素列を Markdown にする。
pub fn serialize_elements(elements: &[ElementContent]) -> String {
    render_elements(elements, false)
}

/// 場面単位の Markdown を要素列にする（見出しは構造として解釈しない）。
pub fn parse_elements(markdown: &str) -> Vec<ElementContent> {
    tokenize(markdown, false)
        .into_iter()
        .filter_map(|b| match b {
            Block::Element(e) => Some(e),
            _ => None,
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneDoc {
    pub title: String,
    pub elements: Vec<ElementContent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChapterDoc {
    pub title: String,
    pub scenes: Vec<SceneDoc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectDoc {
    pub title: String,
    pub chapters: Vec<ChapterDoc>,
}

/// 見出しのない位置に内容があった場合に補う題名。
pub const UNTITLED: &str = "無題";

/// プロジェクト全体を 1 つの Markdown 文書にする。`#` が章、`##` が場面。
pub fn serialize_project(doc: &ProjectDoc) -> String {
    let mut out = String::new();
    out.push_str("---\nscenote: 1\ntitle: ");
    out.push_str(&serde_json::to_string(&doc.title).unwrap_or_else(|_| "\"\"".into()));
    out.push_str("\n---\n");
    for chapter in &doc.chapters {
        out.push_str("\n# ");
        out.push_str(&chapter.title);
        out.push('\n');
        for scene in &chapter.scenes {
            out.push_str("\n## ");
            out.push_str(&scene.title);
            out.push('\n');
            let body = render_elements(&scene.elements, true);
            if !body.is_empty() {
                out.push('\n');
                out.push_str(&body);
            }
        }
    }
    out
}

/// プロジェクト全体の Markdown 文書を解析する。
pub fn parse_project(markdown: &str) -> ProjectDoc {
    let (title, rest) = split_front_matter(markdown);
    let mut doc = ProjectDoc {
        title: title.unwrap_or_else(|| UNTITLED.to_string()),
        chapters: Vec::new(),
    };
    for block in tokenize(rest, true) {
        match block {
            Block::Chapter(title) => doc.chapters.push(ChapterDoc {
                title,
                scenes: Vec::new(),
            }),
            Block::Scene(title) => {
                current_chapter(&mut doc).scenes.push(SceneDoc {
                    title,
                    elements: Vec::new(),
                });
            }
            Block::Element(e) => {
                let chapter = current_chapter(&mut doc);
                if chapter.scenes.is_empty() {
                    chapter.scenes.push(SceneDoc {
                        title: UNTITLED.to_string(),
                        elements: Vec::new(),
                    });
                }
                chapter.scenes.last_mut().unwrap().elements.push(e);
            }
        }
    }
    doc
}

fn current_chapter(doc: &mut ProjectDoc) -> &mut ChapterDoc {
    if doc.chapters.is_empty() {
        doc.chapters.push(ChapterDoc {
            title: UNTITLED.to_string(),
            scenes: Vec::new(),
        });
    }
    doc.chapters.last_mut().unwrap()
}

/// 先頭の front matter から題名を取り出し、残りの本文を返す。
fn split_front_matter(markdown: &str) -> (Option<String>, &str) {
    let Some(after_open) = markdown
        .strip_prefix("---\n")
        .or_else(|| markdown.strip_prefix("---\r\n"))
    else {
        return (None, markdown);
    };
    let mut title = None;
    let mut is_scenote = false;
    let mut offset = 0;
    for line in after_open.split_inclusive('\n') {
        offset += line.len();
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed == "---" {
            // Scenote が出力した front matter のみ取り除く。それ以外は本文として残す。
            return if is_scenote {
                (title, &after_open[offset..])
            } else {
                (None, markdown)
            };
        }
        if trimmed.starts_with("scenote:") {
            is_scenote = true;
        } else if let Some(value) = trimmed.strip_prefix("title:") {
            let value = value.trim();
            title = Some(serde_json::from_str::<String>(value).unwrap_or_else(|_| value.to_string()));
        }
    }
    (None, markdown)
}

enum Block {
    Chapter(String),
    Scene(String),
    Element(ElementContent),
}

fn tokenize(markdown: &str, structural: bool) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for raw in markdown.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line.trim().is_empty() {
            flush(&mut current, &mut blocks, structural);
            continue;
        }
        if structural && current.is_empty() {
            if let Some(heading) = parse_heading(line) {
                blocks.push(heading);
                continue;
            }
        }
        current.push(line);
    }
    flush(&mut current, &mut blocks, structural);
    blocks
}

fn flush(current: &mut Vec<&str>, blocks: &mut Vec<Block>, structural: bool) {
    if current.is_empty() {
        return;
    }
    let first = current[0];
    let (kind, speaker, first_body) = match parse_marker(first) {
        Some(m) if !first.starts_with('\\') => (m.kind, m.speaker, m.rest),
        _ => (KIND_NARRATION.to_string(), String::new(), first),
    };
    let mut lines = vec![unescape_line(first_body, structural)];
    lines.extend(current[1..].iter().map(|l| unescape_line(l, structural)));
    blocks.push(Block::Element(ElementContent {
        kind,
        speaker,
        body: lines.join("\n"),
    }));
    current.clear();
}

fn parse_heading(line: &str) -> Option<Block> {
    if let Some(title) = line.strip_prefix("## ") {
        Some(Block::Scene(title.trim().to_string()))
    } else {
        line.strip_prefix("# ")
            .map(|title| Block::Chapter(title.trim().to_string()))
    }
}

struct Marker<'a> {
    kind: String,
    speaker: String,
    rest: &'a str,
}

fn label_for_kind(kind: &str) -> Option<&'static str> {
    match kind {
        KIND_NARRATION => Some("地の文"),
        KIND_DIALOGUE => Some("台詞"),
        KIND_CHARACTER_ACTION => Some("動作"),
        KIND_DIRECTION => Some("演出"),
        KIND_CHOICE => Some("選択肢"),
        _ => None,
    }
}

fn kind_for_label(label: &str) -> Option<&'static str> {
    match label {
        "地の文" => Some(KIND_NARRATION),
        "台詞" => Some(KIND_DIALOGUE),
        "動作" => Some(KIND_CHARACTER_ACTION),
        "演出" => Some(KIND_DIRECTION),
        "選択肢" => Some(KIND_CHOICE),
        _ => None,
    }
}

/// ブロック先頭行のマーカーを解析する。マーカーでなければ `None`。
fn parse_marker(line: &str) -> Option<Marker<'_>> {
    if let Some(after) = line.strip_prefix("**") {
        // `**話者**: 台詞`
        let end = after.find("**")?;
        let speaker = &after[..end];
        let tail = &after[end + 2..];
        let rest = tail.strip_prefix(':').or_else(|| tail.strip_prefix('：'))?;
        if speaker.is_empty() || !is_valid_speaker(speaker) {
            return None;
        }
        return Some(Marker {
            kind: KIND_DIALOGUE.to_string(),
            speaker: speaker.to_string(),
            rest: rest.strip_prefix(' ').unwrap_or(rest),
        });
    }
    // `[ラベル] 本文` / `[ラベル:話者] 本文` / `[type:種類] 本文` / `[type:種類:話者] 本文`
    let after = line.strip_prefix('[')?;
    let end = after.find(']')?;
    let mut parts = after[..end].split(':');
    let label = parts.next()?;
    let kind = if label == "type" {
        let kind = parts.next()?;
        if !is_valid_kind(kind) {
            return None;
        }
        kind.to_string()
    } else {
        kind_for_label(label)?.to_string()
    };
    let speaker = parts.next().unwrap_or("");
    if parts.next().is_some() || !is_valid_speaker(speaker) {
        return None;
    }
    let rest = &after[end + 1..];
    Some(Marker {
        kind,
        speaker: speaker.to_string(),
        rest: rest.strip_prefix(' ').unwrap_or(rest),
    })
}

/// エスケープなしで書くと構造として解釈されてしまう行か。
fn is_structural_line(line: &str, structural: bool) -> bool {
    line.trim().is_empty()
        || parse_marker(line).is_some()
        || (structural && parse_heading(line).is_some())
}

fn escape_line(line: &str, structural: bool) -> String {
    if is_structural_line(line.trim_start_matches('\\'), structural) {
        format!("\\{line}")
    } else {
        line.to_string()
    }
}

fn unescape_line(line: &str, structural: bool) -> String {
    match line.strip_prefix('\\') {
        Some(rest) if is_structural_line(rest.trim_start_matches('\\'), structural) => {
            rest.to_string()
        }
        _ => line.to_string(),
    }
}

fn marker_for(e: &ElementContent) -> String {
    if e.kind == KIND_NARRATION && e.speaker.is_empty() {
        return String::new();
    }
    if e.kind == KIND_DIALOGUE && !e.speaker.is_empty() {
        return format!("**{}**: ", e.speaker);
    }
    let label = match label_for_kind(&e.kind) {
        Some(label) => label.to_string(),
        None => format!("type:{}", e.kind),
    };
    if e.speaker.is_empty() {
        format!("[{label}] ")
    } else {
        format!("[{label}:{}] ", e.speaker)
    }
}

fn render_elements(elements: &[ElementContent], structural: bool) -> String {
    let mut out = String::new();
    for (i, e) in elements.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&marker_for(e));
        for (n, line) in e.body.split('\n').enumerate() {
            if n > 0 {
                out.push('\n');
            }
            out.push_str(&escape_line(line, structural));
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn el(kind: &str, speaker: &str, body: &str) -> ElementContent {
        ElementContent {
            kind: kind.into(),
            speaker: speaker.into(),
            body: body.into(),
        }
    }

    #[test]
    fn serializes_basic_kinds() {
        let md = serialize_elements(&[
            el("Narration", "", "夜の校舎は静かだった。"),
            el("Dialogue", "アリス", "「誰かいるの？」"),
            el("CharacterAction", "アリス", "扉に手をかける。"),
            el("Direction", "", "SE: 扉のきしむ音"),
            el("Choice", "", "- 中に入る\n- 引き返す"),
        ]);
        assert_eq!(
            md,
            "夜の校舎は静かだった。\n\n**アリス**: 「誰かいるの？」\n\n[動作:アリス] 扉に手をかける。\n\n[演出] SE: 扉のきしむ音\n\n[選択肢] - 中に入る\n- 引き返す\n"
        );
    }

    #[test]
    fn round_trips_awkward_content() {
        let elements = vec![
            el("Narration", "", "一段落目\n\n二段落目\n   \n末尾"),
            el("Narration", "", "[演出] に見えるが地の文"),
            el("Narration", "", "**太字**: これも地の文"),
            el("Narration", "", "\\[演出] 元からバックスラッシュ付き\n\\\n\\*強調ではない\\*"),
            el("Narration", "", ""),
            el("Narration", "語り手", "話者つきの地の文"),
            el("Dialogue", "", "話者なしの台詞"),
            el("Dialogue", "ボブ", ""),
            el("Dialogue", "ボブ", "\n[選択肢] 台詞の中の角括弧\n# 見出し風"),
            el("Foreshadowing", "", "未知の種類"),
            el("Foreshadowing", "アリス", "未知の種類（話者つき）"),
            el("Narration", "", "# 見出し風の行\n## これも"),
            el("Narration", "", "```js\nconst a = 1;\n```\n- [x] チェックリスト\n[リンク](https://example.com)"),
        ];
        assert_eq!(parse_elements(&serialize_elements(&elements)), elements);

        let doc = ProjectDoc {
            title: "題名: \"引用符\"つき".into(),
            chapters: vec![
                ChapterDoc {
                    title: "第1章".into(),
                    scenes: vec![
                        SceneDoc { title: "出会い".into(), elements: elements.clone() },
                        SceneDoc { title: "空の場面".into(), elements: vec![] },
                    ],
                },
                ChapterDoc { title: "空の章".into(), scenes: vec![] },
            ],
        };
        assert_eq!(parse_project(&serialize_project(&doc)), doc);
    }

    #[test]
    fn preserves_unknown_markdown_as_narration() {
        let src = "### 小見出し\n\n> 引用\n> 続き\n\n| a | b |\n|---|---|\n\n[x] ラベルではない\n\n<div>HTML</div>\n";
        let parsed = parse_elements(src);
        assert!(parsed.iter().all(|e| e.kind == "Narration" && e.speaker.is_empty()));
        assert_eq!(serialize_elements(&parsed), src);
    }

    #[test]
    fn parses_hand_written_variants() {
        let parsed = parse_elements("**アリス**：全角コロン\r\n\r\n[台詞:ボブ]空白なし\n\n\n\n[type:Memo] メモ");
        assert_eq!(
            parsed,
            vec![
                el("Dialogue", "アリス", "全角コロン"),
                el("Dialogue", "ボブ", "空白なし"),
                el("Memo", "", "メモ"),
            ]
        );
    }

    #[test]
    fn project_without_headings_gets_default_structure() {
        let doc = parse_project("---\nauthor: someone\n---\n\n本文だけ。\n");
        assert_eq!(doc.title, UNTITLED);
        assert_eq!(doc.chapters.len(), 1);
        assert_eq!(doc.chapters[0].scenes[0].title, UNTITLED);
        // Scenote 以外の front matter は本文として残す。
        let bodies: Vec<_> = doc.chapters[0].scenes[0].elements.iter().map(|e| e.body.as_str()).collect();
        assert_eq!(bodies, vec!["---\nauthor: someone\n---", "本文だけ。"]);
    }
}
