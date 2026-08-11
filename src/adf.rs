//! Markdown -> Atlassian Document Format (ADF) conversion.
//!
//! Ported from the Python `jira_utils.markdown_to_adf`. Handles: headings, paragraphs,
//! bullet/ordered lists, bold, italic, strikethrough, code spans, code blocks, blockquotes,
//! tables, horizontal rules, and links. Checkboxes are left as text (ADF has no checkbox node).

use serde_json::{Value, json};

/// Convert a markdown string to an ADF document (a `serde_json::Value`).
pub fn markdown_to_adf(markdown: &str) -> Value {
    let lines: Vec<&str> = markdown.split('\n').collect();
    let mut content: Vec<Value> = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];

        // Code block
        if let Some(rest) = line.strip_prefix("```") {
            let lang = rest.trim();
            let lang = if lang.is_empty() {
                None
            } else {
                Some(lang.to_string())
            };
            let mut code_lines = Vec::new();
            i += 1;
            while i < lines.len() && !lines[i].starts_with("```") {
                code_lines.push(lines[i]);
                i += 1;
            }
            i += 1; // skip closing ```
            content.push(adf_code_block(&code_lines.join("\n"), lang.as_deref()));
            continue;
        }

        // Heading
        if let Some(h) = parse_heading(line) {
            content.push(h);
            i += 1;
            continue;
        }

        // Horizontal rule
        if is_horizontal_rule(line) {
            content.push(json!({"type": "rule"}));
            i += 1;
            continue;
        }

        // Bullet list
        if is_bullet_start(line) {
            let mut items = Vec::new();
            while i < lines.len() && is_bullet_start(lines[i]) {
                let text = strip_bullet(lines[i]);
                items.push(parse_inline(&text));
                i += 1;
            }
            content.push(adf_bullet_list(&items));
            continue;
        }

        // Ordered list
        if is_ordered_start(line) {
            let mut items = Vec::new();
            while i < lines.len() && is_ordered_start(lines[i]) {
                let text = strip_ordered(lines[i]);
                items.push(parse_inline(&text));
                i += 1;
            }
            content.push(adf_ordered_list(&items));
            continue;
        }

        // Blockquote
        if line.starts_with("> ") || line == ">" {
            let mut quote_lines = Vec::new();
            while i < lines.len() && (lines[i].starts_with("> ") || lines[i] == ">") {
                let stripped = if lines[i] == ">" { "" } else { &lines[i][2..] };
                quote_lines.push(stripped);
                i += 1;
            }
            let quote_md = quote_lines.join("\n");
            let inner = markdown_to_adf(&quote_md);
            let inner_content = inner
                .get("content")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            // ADF blockquotes can't contain headings; use a panel instead.
            let has_headings = inner_content
                .iter()
                .any(|n| n.get("type").and_then(Value::as_str) == Some("heading"));
            if has_headings {
                content.push(json!({
                    "type": "panel",
                    "attrs": {"panelType": "info"},
                    "content": inner_content,
                }));
            } else {
                content.push(json!({
                    "type": "blockquote",
                    "content": inner_content,
                }));
            }
            continue;
        }

        // Table. Both pipes required, or a lone `| foo` line would match zero rows below and
        // loop forever without advancing i.
        if line.starts_with('|') && line.ends_with('|') {
            let mut table_rows: Vec<Vec<String>> = Vec::new();
            while i < lines.len() && lines[i].starts_with('|') && lines[i].ends_with('|') {
                let row_text = lines[i].trim();
                // Skip separator rows (| --- | --- |)
                if is_table_separator(row_text) {
                    i += 1;
                    continue;
                }
                let cells: Vec<String> = row_text
                    .split('|')
                    .skip(1) // empty before first |
                    .collect::<Vec<_>>()
                    .split_last()
                    .map(|(_, rest)| rest.iter().map(|c| c.to_string()).collect())
                    .unwrap_or_default();
                table_rows.push(cells);
                i += 1;
            }
            if !table_rows.is_empty() {
                content.push(adf_table(&table_rows));
            }
            continue;
        }

        // Empty line
        if line.trim().is_empty() {
            i += 1;
            continue;
        }

        // Paragraph: accumulate consecutive non-empty, non-special lines
        let mut para_lines = Vec::new();
        while i < lines.len()
            && !lines[i].trim().is_empty()
            && !lines[i].starts_with('#')
            && !lines[i].starts_with("```")
            && !is_bullet_start(lines[i])
            && !is_ordered_start(lines[i])
            && !is_horizontal_rule(lines[i])
            && !(lines[i].starts_with('|') && lines[i].ends_with('|'))
        {
            para_lines.push(lines[i]);
            i += 1;
        }
        if !para_lines.is_empty() {
            let text = para_lines.join(" ");
            content.push(adf_paragraph(&parse_inline(&text)));
        } else {
            // Safety net: no branch matched and i was not advanced
            content.push(adf_paragraph(&parse_inline(line)));
            i += 1;
        }
    }

    if content.is_empty() {
        content.push(adf_paragraph(&[adf_text("", None)]));
    }

    json!({"type": "doc", "version": 1, "content": content})
}

fn parse_heading(line: &str) -> Option<Value> {
    let bytes = line.as_bytes();
    let mut level = 0usize;
    while level < bytes.len() && bytes[level] == b'#' && level < 6 {
        level += 1;
    }
    if level == 0 || level >= bytes.len() || bytes[level] != b' ' {
        return None;
    }
    let text = line[level + 1..].trim();
    let nodes = if text.is_empty() {
        vec![adf_text("", None)]
    } else {
        parse_inline(text)
    };
    Some(json!({
        "type": "heading",
        "attrs": {"level": level},
        "content": nodes,
    }))
}

fn is_horizontal_rule(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.len() >= 3 && trimmed.chars().all(|c| c == '-')
}

fn is_bullet_start(line: &str) -> bool {
    line.starts_with("- ") || line.starts_with("* ")
}

fn strip_bullet(line: &str) -> String {
    line[2..].to_string()
}

/// `1. text` — the space after the dot matters, or `1.5 stars` becomes a list.
fn is_ordered_start(line: &str) -> bool {
    let digits = line.chars().take_while(|c| c.is_ascii_digit()).count();
    digits > 0 && line[digits..].starts_with(". ")
}

fn strip_ordered(line: &str) -> String {
    let digits = line.chars().take_while(|c| c.is_ascii_digit()).count();
    line[digits + 2..].to_string()
}

fn is_table_separator(line: &str) -> bool {
    // | --- | --- | or |:---|:---|
    line.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

fn adf_text(text: &str, marks: Option<Vec<Value>>) -> Value {
    let mut node = json!({"type": "text", "text": text});
    if let Some(m) = marks
        && !m.is_empty()
    {
        node["marks"] = Value::Array(m);
    }
    node
}

fn adf_paragraph(nodes: &[Value]) -> Value {
    json!({"type": "paragraph", "content": nodes})
}

fn adf_code_block(text: &str, language: Option<&str>) -> Value {
    let mut node = json!({"type": "codeBlock", "content": [adf_text(text, None)]});
    if let Some(lang) = language
        && !lang.is_empty()
    {
        node["attrs"] = json!({"language": lang});
    }
    node
}

fn adf_bullet_list(items: &[Vec<Value>]) -> Value {
    json!({
        "type": "bulletList",
        "content": items.iter().map(|nodes| json!({
            "type": "listItem",
            "content": [adf_paragraph(nodes)],
        })).collect::<Vec<_>>(),
    })
}

fn adf_ordered_list(items: &[Vec<Value>]) -> Value {
    json!({
        "type": "orderedList",
        "content": items.iter().map(|nodes| json!({
            "type": "listItem",
            "content": [adf_paragraph(nodes)],
        })).collect::<Vec<_>>(),
    })
}

fn adf_table(rows: &[Vec<String>]) -> Value {
    let adf_rows: Vec<Value> = rows
        .iter()
        .enumerate()
        .map(|(row_idx, cells)| {
            let cell_type = if row_idx == 0 {
                "tableHeader"
            } else {
                "tableCell"
            };
            let adf_cells: Vec<Value> = cells
                .iter()
                .map(|cell_text| {
                    json!({
                        "type": cell_type,
                        "content": [adf_paragraph(&parse_inline(cell_text.trim()))],
                    })
                })
                .collect();
            json!({"type": "tableRow", "content": adf_cells})
        })
        .collect();
    json!({"type": "table", "content": adf_rows})
}

/// Parse inline markdown formatting into ADF text nodes with marks.
/// Handles: **bold**, *italic*, ~~strike~~, `code`, [text](url)
fn parse_inline(text: &str) -> Vec<Value> {
    let mut nodes = Vec::new();
    scan_inline(text, &mut nodes);
    if nodes.is_empty() {
        nodes.push(adf_text(text, None));
    }
    nodes
}

fn scan_inline(text: &str, nodes: &mut Vec<Value>) {
    let mut pos = 0;
    let len = text.len();

    while pos < len {
        // Find the next inline pattern
        let mut earliest: Option<(usize, usize, Value)> = None; // (start, end, node)

        // **bold**
        if let Some(m) = find_delimited(text, pos, "**", "**")
            && (earliest.is_none() || m.0 < earliest.as_ref().map(|e| e.0).unwrap_or(usize::MAX))
        {
            let inner = &text[m.0 + 2..m.1 - 2];
            earliest = Some((
                m.0,
                m.1,
                adf_text(inner, Some(vec![json!({"type": "strong"})])),
            ));
        }

        // ~~strike~~
        if let Some(m) = find_delimited(text, pos, "~~", "~~")
            && (earliest.is_none() || m.0 < earliest.as_ref().map(|e| e.0).unwrap_or(usize::MAX))
        {
            let inner = &text[m.0 + 2..m.1 - 2];
            earliest = Some((
                m.0,
                m.1,
                adf_text(inner, Some(vec![json!({"type": "strike"})])),
            ));
        }

        // *italic* (but not **)
        if let Some(m) = find_single_star_italic(text, pos)
            && (earliest.is_none() || m.0 < earliest.as_ref().map(|e| e.0).unwrap_or(usize::MAX))
        {
            let inner = &text[m.0 + 1..m.1 - 1];
            earliest = Some((m.0, m.1, adf_text(inner, Some(vec![json!({"type": "em"})]))));
        }

        // `code`
        if let Some(m) = find_backtick_code(text, pos)
            && (earliest.is_none() || m.0 < earliest.as_ref().map(|e| e.0).unwrap_or(usize::MAX))
        {
            let inner = &text[m.0 + 1..m.1 - 1];
            earliest = Some((
                m.0,
                m.1,
                adf_text(inner, Some(vec![json!({"type": "code"})])),
            ));
        }

        // [text](url)
        if let Some(m) = find_link(text, pos)
            && (earliest.is_none() || m.0 < earliest.as_ref().map(|e| e.0).unwrap_or(usize::MAX))
        {
            earliest = Some((m.0, m.1, m.2));
        }

        match earliest {
            Some((start, end, node)) => {
                if start > pos {
                    nodes.push(adf_text(&text[pos..start], None));
                }
                nodes.push(node);
                pos = end;
            }
            None => {
                // No more patterns; emit the rest as plain text
                if pos < len {
                    nodes.push(adf_text(&text[pos..], None));
                }
                break;
            }
        }
    }
}

/// Find `open...close` delimiters starting from `from`.
fn find_delimited(text: &str, from: usize, open: &str, close: &str) -> Option<(usize, usize)> {
    let start = text[from..].find(open).map(|i| i + from)?;
    let after_open = start + open.len();
    if after_open >= text.len() {
        return None;
    }
    let end_inner = text[after_open..].find(close)?;
    if end_inner == 0 {
        return None; // empty content
    }
    Some((start, after_open + end_inner + close.len()))
}

/// Find *italic* that isn't **bold**.
fn find_single_star_italic(text: &str, from: usize) -> Option<(usize, usize)> {
    let mut search_from = from;
    loop {
        let start = text[search_from..].find('*').map(|i| i + search_from)?;
        // Skip if this is the start of **
        if text[start..].starts_with("**") {
            search_from = start + 2;
            continue;
        }
        // Find closing * that isn't **
        let after = start + 1;
        if after >= text.len() {
            return None;
        }
        let mut end_search = after;
        loop {
            let close = text[end_search..].find('*').map(|i| i + end_search)?;
            // Make sure the close * isn't part of **
            if close + 1 < text.len() && text.as_bytes()[close + 1] == b'*' {
                end_search = close + 2;
                continue;
            }
            if close > start + 1 && (close == 0 || text.as_bytes()[close - 1] != b'*') {
                return Some((start, close + 1));
            }
            end_search = close + 1;
            if end_search >= text.len() {
                return None;
            }
        }
    }
}

fn find_backtick_code(text: &str, from: usize) -> Option<(usize, usize)> {
    let start = text[from..].find('`').map(|i| i + from)?;
    let after = start + 1;
    if after >= text.len() {
        return None;
    }
    let end = text[after..].find('`').map(|i| i + after)?;
    if end == after {
        return None; // empty
    }
    Some((start, end + 1))
}

fn find_link(text: &str, from: usize) -> Option<(usize, usize, Value)> {
    let start = text[from..].find('[').map(|i| i + from)?;
    let close_bracket = text[start + 1..].find(']').map(|i| i + start + 1)?;
    // Must be followed immediately by (
    if close_bracket + 1 >= text.len() || text.as_bytes()[close_bracket + 1] != b'(' {
        return None;
    }
    let close_paren = text[close_bracket + 2..]
        .find(')')
        .map(|i| i + close_bracket + 2)?;
    let link_text = &text[start + 1..close_bracket];
    let url = &text[close_bracket + 2..close_paren];
    let node = adf_text(
        link_text,
        Some(vec![json!({"type": "link", "attrs": {"href": url}})]),
    );
    Some((start, close_paren + 1, node))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_paragraph() {
        let adf = markdown_to_adf("Hello world");
        assert_eq!(adf["type"], "doc");
        assert_eq!(adf["content"][0]["type"], "paragraph");
        assert_eq!(adf["content"][0]["content"][0]["text"], "Hello world");
    }

    #[test]
    fn headings_h1_to_h6() {
        for level in 1..=6 {
            let prefix = "#".repeat(level);
            let adf = markdown_to_adf(&format!("{prefix} Heading {level}"));
            let heading = &adf["content"][0];
            assert_eq!(heading["type"], "heading");
            assert_eq!(heading["attrs"]["level"], level);
            assert_eq!(heading["content"][0]["text"], format!("Heading {level}"));
        }
    }

    #[test]
    fn bullet_list() {
        let adf = markdown_to_adf("- Item one\n- Item two\n- Item three");
        let bl = &adf["content"][0];
        assert_eq!(bl["type"], "bulletList");
        assert_eq!(bl["content"].as_array().map(|a| a.len()), Some(3));
        assert_eq!(bl["content"][0]["type"], "listItem");
    }

    #[test]
    fn ordered_list() {
        let adf = markdown_to_adf("1. First\n2. Second\n3. Third");
        let ol = &adf["content"][0];
        assert_eq!(ol["type"], "orderedList");
        assert_eq!(ol["content"].as_array().map(|a| a.len()), Some(3));
    }

    #[test]
    fn code_block_with_language() {
        let adf = markdown_to_adf("```python\nprint('hello')\n```");
        let cb = &adf["content"][0];
        assert_eq!(cb["type"], "codeBlock");
        assert_eq!(cb["attrs"]["language"], "python");
        assert_eq!(cb["content"][0]["text"], "print('hello')");
    }

    #[test]
    fn code_block_no_language() {
        let adf = markdown_to_adf("```\nsome code\n```");
        let cb = &adf["content"][0];
        assert_eq!(cb["type"], "codeBlock");
        assert_eq!(cb["content"][0]["text"], "some code");
    }

    #[test]
    fn table() {
        let md = "| Name | Value |\n|------|-------|\n| foo  | bar   |";
        let adf = markdown_to_adf(md);
        let table = &adf["content"][0];
        assert_eq!(table["type"], "table");
        let rows = table["content"].as_array().expect("table rows");
        assert_eq!(rows.len(), 2); // header + 1 data row (separator skipped)
        assert_eq!(rows[0]["content"][0]["type"], "tableHeader");
        assert_eq!(rows[1]["content"][0]["type"], "tableCell");
    }

    #[test]
    fn bold_and_italic() {
        let adf = markdown_to_adf("Some **bold** and *italic* text");
        let nodes = adf["content"][0]["content"]
            .as_array()
            .expect("inline nodes");
        // Should have: "Some " -> **bold** -> " and " -> *italic* -> " text"
        assert!(
            nodes.len() >= 5,
            "expected 5+ inline nodes, got {}",
            nodes.len()
        );
        assert_eq!(nodes[1]["marks"][0]["type"], "strong");
        assert_eq!(nodes[1]["text"], "bold");
        assert_eq!(nodes[3]["marks"][0]["type"], "em");
        assert_eq!(nodes[3]["text"], "italic");
    }

    #[test]
    fn inline_code() {
        let adf = markdown_to_adf("Use `foo()` here");
        let nodes = adf["content"][0]["content"].as_array().expect("inline");
        let code_node = nodes
            .iter()
            .find(|n| n["text"] == "foo()")
            .expect("code node");
        assert_eq!(code_node["marks"][0]["type"], "code");
    }

    #[test]
    fn link() {
        let adf = markdown_to_adf("Visit [example](https://example.com) now");
        let nodes = adf["content"][0]["content"].as_array().expect("inline");
        let link_node = nodes
            .iter()
            .find(|n| n["text"] == "example")
            .expect("link node");
        assert_eq!(link_node["marks"][0]["type"], "link");
        assert_eq!(
            link_node["marks"][0]["attrs"]["href"],
            "https://example.com"
        );
    }

    #[test]
    fn horizontal_rule() {
        let adf = markdown_to_adf("Above\n\n---\n\nBelow");
        let rule = adf["content"]
            .as_array()
            .expect("content")
            .iter()
            .find(|n| n["type"] == "rule");
        assert!(rule.is_some(), "expected a rule node");
    }

    #[test]
    fn blockquote() {
        let adf = markdown_to_adf("> Quoted text");
        let bq = &adf["content"][0];
        assert_eq!(bq["type"], "blockquote");
    }

    #[test]
    fn blockquote_with_heading_becomes_panel() {
        let adf = markdown_to_adf("> # Heading inside quote");
        let panel = &adf["content"][0];
        assert_eq!(panel["type"], "panel");
        assert_eq!(panel["attrs"]["panelType"], "info");
    }

    #[test]
    fn strikethrough() {
        let adf = markdown_to_adf("~~deleted~~");
        let nodes = adf["content"][0]["content"].as_array().expect("inline");
        assert_eq!(nodes[0]["text"], "deleted");
        assert_eq!(nodes[0]["marks"][0]["type"], "strike");
    }

    /// A pipe-opened line with no closing pipe must not enter the table branch: it matches zero
    /// rows there and the parser would spin forever without advancing.
    #[test]
    fn lone_pipe_line_is_a_paragraph_not_a_hang() {
        let adf = markdown_to_adf("| foo\nplain text");
        let types: Vec<&str> = adf["content"]
            .as_array()
            .expect("content")
            .iter()
            .map(|n| n["type"].as_str().unwrap_or("?"))
            .collect();
        assert!(!types.contains(&"table"), "types: {types:?}");
        assert!(types.contains(&"paragraph"), "types: {types:?}");
    }

    #[test]
    fn decimal_number_is_not_an_ordered_list() {
        let adf = markdown_to_adf("1.5 stars on average");
        assert_eq!(adf["content"][0]["type"], "paragraph");
    }

    #[test]
    fn empty_input() {
        let adf = markdown_to_adf("");
        assert_eq!(adf["type"], "doc");
        assert!(!adf["content"].as_array().expect("content").is_empty());
    }

    #[test]
    fn complex_document() {
        let md = "\
# Title

Some paragraph with **bold** and *italic*.

## Section

- bullet one
- bullet two

1. ordered one
2. ordered two

```rust
fn main() {}
```

| Col A | Col B |
|-------|-------|
| 1     | 2     |

> A quote

---

End.";
        let adf = markdown_to_adf(md);
        let content = adf["content"].as_array().expect("content array");
        let types: Vec<&str> = content
            .iter()
            .map(|n| n["type"].as_str().unwrap_or("?"))
            .collect();
        assert!(types.contains(&"heading"), "types: {types:?}");
        assert!(types.contains(&"paragraph"), "types: {types:?}");
        assert!(types.contains(&"bulletList"), "types: {types:?}");
        assert!(types.contains(&"orderedList"), "types: {types:?}");
        assert!(types.contains(&"codeBlock"), "types: {types:?}");
        assert!(types.contains(&"table"), "types: {types:?}");
        assert!(types.contains(&"blockquote"), "types: {types:?}");
        assert!(types.contains(&"rule"), "types: {types:?}");
    }
}
