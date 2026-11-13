//! テキスト差分。差分計算は LLM に任せず、ここで行う。

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DiffOp {
    Equal,
    Delete,
    Insert,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Segment {
    pub op: DiffOp,
    pub text: String,
}

/// これを超える組み合わせ数では、より粗い単位へ切り替える。
const MAX_CELLS: usize = 4_000_000;

/// `before` から `after` への差分を返す。
///
/// 短い文章は文字単位、長い文章は行単位で比較する。`Delete` と `Equal` を連結すると
/// `before` に、`Insert` と `Equal` を連結すると `after` に一致する。
pub fn diff_text(before: &str, after: &str) -> Vec<Segment> {
    let a: Vec<&str> = split_chars(before);
    let b: Vec<&str> = split_chars(after);
    if let Some(segments) = diff_tokens(&a, &b) {
        return segments;
    }
    let a: Vec<&str> = before.split_inclusive('\n').collect();
    let b: Vec<&str> = after.split_inclusive('\n').collect();
    if let Some(segments) = diff_tokens(&a, &b) {
        return segments;
    }
    let mut out = Vec::new();
    push(&mut out, DiffOp::Delete, before);
    push(&mut out, DiffOp::Insert, after);
    out
}

fn split_chars(s: &str) -> Vec<&str> {
    s.char_indices()
        .map(|(i, c)| &s[i..i + c.len_utf8()])
        .collect()
}

fn push(out: &mut Vec<Segment>, op: DiffOp, text: &str) {
    if text.is_empty() {
        return;
    }
    match out.last_mut() {
        Some(last) if last.op == op => last.text.push_str(text),
        _ => out.push(Segment {
            op,
            text: text.to_string(),
        }),
    }
}

/// 共通部分を除いた残りが大きすぎる場合は `None`。
fn diff_tokens(a: &[&str], b: &[&str]) -> Option<Vec<Segment>> {
    // 共通の先頭・末尾を除いてから最長共通部分列を求める。
    let prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (am, bm) = (&a[prefix..a.len() - suffix], &b[prefix..b.len() - suffix]);
    let (n, m) = (am.len(), bm.len());
    if n.saturating_mul(m) > MAX_CELLS {
        return None;
    }
    let width = m + 1;
    let mut lcs = vec![0u32; (n + 1) * width];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i * width + j] = if am[i] == bm[j] {
                lcs[(i + 1) * width + j + 1] + 1
            } else {
                lcs[(i + 1) * width + j].max(lcs[i * width + j + 1])
            };
        }
    }
    let mut out = Vec::new();
    for t in &a[..prefix] {
        push(&mut out, DiffOp::Equal, t);
    }
    // 連続する変更は「削除 → 挿入」の順にまとめて読みやすくする。
    let (mut i, mut j) = (0, 0);
    let (mut deleted, mut inserted) = (String::new(), String::new());
    let flush = |out: &mut Vec<Segment>, deleted: &mut String, inserted: &mut String| {
        push(out, DiffOp::Delete, deleted);
        push(out, DiffOp::Insert, inserted);
        deleted.clear();
        inserted.clear();
    };
    while i < n || j < m {
        if i < n && j < m && am[i] == bm[j] {
            flush(&mut out, &mut deleted, &mut inserted);
            push(&mut out, DiffOp::Equal, am[i]);
            i += 1;
            j += 1;
        } else if j < m && (i == n || lcs[i * width + j + 1] >= lcs[(i + 1) * width + j]) {
            inserted.push_str(bm[j]);
            j += 1;
        } else {
            deleted.push_str(am[i]);
            i += 1;
        }
    }
    flush(&mut out, &mut deleted, &mut inserted);
    for t in &a[a.len() - suffix..] {
        push(&mut out, DiffOp::Equal, t);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rebuild(segments: &[Segment]) -> (String, String) {
        let mut before = String::new();
        let mut after = String::new();
        for s in segments {
            if s.op != DiffOp::Insert {
                before.push_str(&s.text);
            }
            if s.op != DiffOp::Delete {
                after.push_str(&s.text);
            }
        }
        (before, after)
    }

    fn seg(op: DiffOp, text: &str) -> Segment {
        Segment { op, text: text.into() }
    }

    #[test]
    fn marks_changed_part_only() {
        let d = diff_text("彼は静かに扉を開けた。", "彼は乱暴に扉を開けた。");
        assert_eq!(
            d,
            vec![
                seg(DiffOp::Equal, "彼は"),
                seg(DiffOp::Delete, "静か"),
                seg(DiffOp::Insert, "乱暴"),
                seg(DiffOp::Equal, "に扉を開けた。"),
            ]
        );
    }

    #[test]
    fn reconstructs_both_sides() {
        let cases = [
            ("", ""),
            ("", "新規"),
            ("削除される", ""),
            ("同じ", "同じ"),
            ("abcabc", "bcab"),
            ("一行目\n二行目\n三行目", "一行目\n挿入\n三行目\n追記"),
            ("絵文字😀と結合文字か\u{3099}", "絵文字😎と結合文字が"),
        ];
        for (a, b) in cases {
            let d = diff_text(a, b);
            assert_eq!(rebuild(&d), (a.to_string(), b.to_string()), "{a:?} -> {b:?}");
            assert!(d.windows(2).all(|w| w[0].op != w[1].op));
        }
    }

    #[test]
    fn falls_back_to_lines_for_long_text() {
        let a: String = (0..3000).map(|i| format!("行{i}\n")).collect();
        // 離れた 2 箇所を変えて、文字単位では比較しきれない大きさにする。
        let b = a.replace("行1500\n", "書き換えた行\n").replace("行10\n", "行10改\n");
        let d = diff_text(&a, &b);
        assert_eq!(rebuild(&d), (a.clone(), b.clone()));
        assert!(d.contains(&seg(DiffOp::Delete, "行1500\n")));
        assert!(d.contains(&seg(DiffOp::Insert, "書き換えた行\n")));
    }
}
