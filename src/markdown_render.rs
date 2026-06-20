use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

const INDENT: &str = "  ";

/// `.rs` paths are shown as normal text, not as inline code.
fn is_rs_source_ref(s: &str) -> bool {
    s.trim().ends_with(".rs")
}

fn visible_plain(s: &str) -> String {
    inline_words(s, Style::default())
        .into_iter()
        .map(|w| w.text)
        .collect::<Vec<_>>()
        .join(" ")
}

fn visible_width(s: &str) -> usize {
    display_width(&visible_plain(s))
}

pub fn render_markdown(text: &str, out: &mut Vec<Line>, max_width: u16) {
    let width = max_width.saturating_sub(4) as usize;
    let mut in_code = false;
    let mut code_lang: Option<String> = None;
    let mut code_lines: Vec<String> = Vec::new();
    let mut table_rows: Vec<Vec<String>> = Vec::new();

    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];

        if line.trim_start().starts_with("```") {
            flush_table(&mut table_rows, out, width);
            if in_code {
                render_code_block(&code_lang, &code_lines, out, max_width);
                code_lines.clear();
                code_lang = None;
                in_code = false;
            } else {
                in_code = true;
                code_lang = parse_fence_lang(line);
            }
            i += 1;
            continue;
        }

        if in_code {
            code_lines.push(line.to_string());
            i += 1;
            continue;
        }

        if is_table_row(line) {
            if !is_table_separator(line) {
                table_rows.push(parse_table_row(line));
            }
            i += 1;
            continue;
        }
        flush_table(&mut table_rows, out, width);

        if line.trim().is_empty() {
            out.push(Line::from(""));
            i += 1;
            continue;
        }

        if is_hr(line) {
            render_hr(out, width);
            i += 1;
            continue;
        }

        if let Some((level, title)) = parse_heading(line) {
            render_heading(out, level, &title, width);
            i += 1;
            continue;
        }

        if let Some(quote) = parse_blockquote(line) {
            render_blockquote(out, &quote, width);
            i += 1;
            continue;
        }

        if let Some((ordered, item)) = parse_list_item(line) {
            render_list_item(out, ordered, &item, width);
            i += 1;
            continue;
        }

        out.extend(wrap_inline_line(line, width, Style::default().fg(Color::White)));
        i += 1;
    }

    if in_code {
        render_code_block(&code_lang, &code_lines, out, max_width);
    }
    flush_table(&mut table_rows, out, width);
}

fn flush_table(rows: &mut Vec<Vec<String>>, out: &mut Vec<Line>, width: usize) {
    if rows.is_empty() {
        return;
    }
    render_table(rows, out, width);
    rows.clear();
}

fn parse_fence_lang(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if !trimmed.starts_with("```") {
        return None;
    }
    let lang = trimmed[3..].trim();
    if lang.is_empty() {
        None
    } else {
        Some(lang.to_string())
    }
}

fn is_table_row(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('|') && t.ends_with('|') && t.contains('|')
}

fn is_table_separator(line: &str) -> bool {
    let t = line.trim().trim_matches('|');
    t.chars()
        .all(|c| c == '-' || c == ':' || c == ' ' || c == '|')
        && t.contains('-')
}

fn parse_table_row(line: &str) -> Vec<String> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(|c| c.trim().to_string())
        .collect()
}

fn is_hr(line: &str) -> bool {
    let t = line.trim();
    t.len() >= 3
        && (t.chars().all(|c| c == '-')
            || t.chars().all(|c| c == '*')
            || t.chars().all(|c| c == '_'))
}

fn parse_heading(line: &str) -> Option<(usize, String)> {
    let trimmed = line.trim_start();
    let level = trimmed.chars().take_while(|&c| c == '#').count();
    if level == 0 || level > 6 {
        return None;
    }
    let rest = trimmed[level..].trim_start();
    if rest.is_empty() {
        return None;
    }
    Some((level, rest.to_string()))
}

fn parse_blockquote(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('>') {
        return None;
    }
    Some(trimmed[1..].trim_start().to_string())
}

fn parse_list_item(line: &str) -> Option<(Option<u32>, String)> {
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed.strip_prefix("- ") {
        return Some((None, rest.to_string()));
    }
    if let Some(rest) = trimmed.strip_prefix("* ") {
        return Some((None, rest.to_string()));
    }
    if let Some(rest) = trimmed.strip_prefix("+ ") {
        return Some((None, rest.to_string()));
    }
    if let Some((num, rest)) = trimmed.split_once(". ") {
        if num.chars().all(|c| c.is_ascii_digit()) {
            return num.parse().ok().map(|n| (Some(n), rest.to_string()));
        }
    }
    None
}

fn render_hr(out: &mut Vec<Line>, width: usize) {
    let w = width.min(60);
    out.push(Line::from(Span::styled(
        format!("{}{}", INDENT, "─".repeat(w)),
        Style::default().fg(Color::DarkGray),
    )));
}

fn render_heading(out: &mut Vec<Line>, level: usize, title: &str, width: usize) {
    let style = match level {
        1 => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
        2 => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
        _ => Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    };
    out.extend(wrap_inline_line(title, width, style));
    if level <= 2 {
        let underline_len = display_width(title).min(width);
        out.push(Line::from(Span::styled(
            format!("{}{}", INDENT, "─".repeat(underline_len)),
            Style::default().fg(Color::DarkGray),
        )));
    }
    out.push(Line::from(""));
}

fn render_blockquote(out: &mut Vec<Line>, text: &str, width: usize) {
    let style = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::ITALIC);
    let prefix = "  │ ";
    let quote_width = width.saturating_sub(display_width(prefix));
    let wrapped = wrap_inline_line_prefixed(text, quote_width, style, prefix, prefix);
    out.extend(wrapped);
}

fn render_list_item(out: &mut Vec<Line>, ordered: Option<u32>, text: &str, width: usize) {
    let bullet = match ordered {
        Some(n) => format!("{:<3}", format!("{}.", n)),
        None => "•  ".to_string(),
    };
    let first_prefix = format!("{}{}", INDENT, bullet);
    let cont_prefix = format!("{}{}", INDENT, " ".repeat(display_width(&bullet)));
    let style = Style::default().fg(Color::White);
    let wrapped = wrap_inline_words(
        inline_words(text, style),
        width,
        &first_prefix,
        &cont_prefix,
    );
    for (idx, mut line) in wrapped.into_iter().enumerate() {
        if idx == 0 && !line.spans.is_empty() {
            line.spans[0] = Span::styled(first_prefix.clone(), Style::default().fg(Color::Cyan));
        }
        out.push(line);
    }
}

fn render_code_block(lang: &Option<String>, lines: &[String], out: &mut Vec<Line>, max_width: u16) {
    let label = lang
        .as_deref()
        .filter(|l| !l.is_empty())
        .map(|l| format!(" {l} "))
        .unwrap_or_else(|| " code ".to_string());
    out.push(Line::from(Span::styled(
        format!("  ┌{label}{}", "─".repeat(28)),
        Style::default().fg(Color::Yellow),
    )));
    let w = max_width.saturating_sub(6) as usize;
    for line in lines {
        for chunk in wrap_line_chunks(line, w) {
            out.push(Line::from(vec![
                Span::styled("  │ ", Style::default().fg(Color::Yellow)),
                Span::styled(chunk, Style::default().fg(Color::Gray)),
            ]));
        }
    }
    out.push(Line::from(Span::styled(
        "  └────────────────────────────────────",
        Style::default().fg(Color::Yellow),
    )));
    out.push(Line::from(""));
}

/// Soft-wrap a single line to display width without dropping characters.
fn wrap_line_chunks(line: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![line.to_string()];
    }
    if display_width(line) <= width {
        return vec![line.to_string()];
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < line.len() {
        let mut end = start;
        let mut w = 0usize;
        while end < line.len() {
            let ch = line[end..].chars().next().unwrap();
            let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if w + cw > width {
                break;
            }
            end += ch.len_utf8();
            w += cw;
        }
        if end == start {
            let ch = line[start..].chars().next().unwrap();
            end = start + ch.len_utf8();
        }
        chunks.push(line[start..end].to_string());
        start = end;
    }
    chunks
}

fn render_table(rows: &[Vec<String>], out: &mut Vec<Line>, width: usize) {
    if rows.is_empty() {
        return;
    }
    let col_count = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if col_count == 0 {
        return;
    }

    let mut col_widths = vec![2usize; col_count];
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < col_count {
                col_widths[i] = col_widths[i].max(visible_width(cell).max(2));
            }
        }
    }

    let border_overhead = 2 + col_count * 3 - 1;
    let content: usize = col_widths.iter().sum();
    if border_overhead + content > width {
        shrink_columns(&mut col_widths, width.saturating_sub(border_overhead));
    }

    let border_style = Style::default().fg(Color::Cyan);
    let header_style = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    let cell_style = Style::default().fg(Color::White);

    out.push(Line::from(Span::styled(
        format!("  {}", table_border(&col_widths, '┌', '┬', '┐')),
        border_style,
    )));

    for (ri, row) in rows.iter().enumerate() {
        out.push(table_row_line(row, &col_widths, if ri == 0 { header_style } else { cell_style }));
        if ri == 0 {
            out.push(Line::from(Span::styled(
                format!("  {}", table_border(&col_widths, '├', '┼', '┤')),
                border_style,
            )));
        }
    }

    out.push(Line::from(Span::styled(
        format!("  {}", table_border(&col_widths, '└', '┴', '┘')),
        border_style,
    )));
    out.push(Line::from(""));
}

fn table_border(widths: &[usize], left: char, mid: char, right: char) -> String {
    let mut s = String::from(left);
    for (i, w) in widths.iter().enumerate() {
        s.push_str(&"─".repeat(*w + 2));
        s.push(if i + 1 == widths.len() { right } else { mid });
    }
    s
}

fn table_row_line(row: &[String], widths: &[usize], style: Style) -> Line<'static> {
    let border = Style::default().fg(Color::Cyan);
    let mut spans = vec![Span::raw("  ".to_string())];
    for (i, w) in widths.iter().enumerate() {
        let cell = row.get(i).map(String::as_str).unwrap_or("");
        spans.push(Span::styled("│".to_string(), border));
        spans.push(Span::raw(" ".to_string()));
        let (cell_spans, used) = table_cell_spans(cell, *w, style);
        spans.extend(cell_spans);
        let pad = w.saturating_sub(used);
        if pad > 0 {
            spans.push(Span::raw(" ".repeat(pad)));
        }
        spans.push(Span::raw(" ".to_string()));
        spans.push(Span::styled("│".to_string(), border));
    }
    Line::from(spans)
}

fn table_cell_spans(cell: &str, inner_width: usize, base_style: Style) -> (Vec<Span<'static>>, usize) {
    let words = inline_words(cell, base_style);
    let mut spans = Vec::new();
    let mut used = 0usize;

    for (i, word) in words.iter().enumerate() {
        let sep = usize::from(i > 0);
        let ww = display_width(&word.text);
        if used + sep + ww > inner_width {
            if spans.is_empty() && inner_width > 0 {
                spans.push(Span::styled(
                    truncate_display(&word.text, inner_width),
                    word.style,
                ));
                used = inner_width;
            }
            break;
        }
        if i > 0 {
            spans.push(Span::raw(" ".to_string()));
            used += 1;
        }
        spans.push(Span::styled(word.text.clone(), word.style));
        used += ww;
    }

    (spans, used)
}

fn shrink_columns(widths: &mut [usize], budget: usize) {
    let min_w = 2usize;
    let mut total: usize = widths.iter().sum();
    while total > budget {
        let flex: Vec<usize> = widths
            .iter()
            .enumerate()
            .filter(|(_, w)| **w > min_w)
            .map(|(i, _)| i)
            .collect();
        if flex.is_empty() {
            break;
        }
        let excess = total - budget;
        let per = (excess / flex.len()).max(1);
        for i in flex {
            widths[i] = widths[i].saturating_sub(per).max(min_w);
        }
        total = widths.iter().sum();
    }
}

fn wrap_inline_line(text: &str, width: usize, base_style: Style) -> Vec<Line<'static>> {
    wrap_inline_line_prefixed(text, width, base_style, INDENT, INDENT)
}

fn wrap_inline_line_prefixed(
    text: &str,
    width: usize,
    base_style: Style,
    prefix: &str,
    cont_prefix: &str,
) -> Vec<Line<'static>> {
    wrap_inline_words(inline_words(text, base_style), width, prefix, cont_prefix)
}

fn wrap_inline_words(
    words: Vec<StyledWord>,
    width: usize,
    prefix: &str,
    cont_prefix: &str,
) -> Vec<Line<'static>> {
    if words.is_empty() {
        return vec![Line::from(Span::raw(prefix.to_string()))];
    }

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current_prefix = prefix;
    let mut current: Vec<Span<'static>> = vec![Span::raw(current_prefix.to_string())];
    let mut current_width = display_width(current_prefix);

    for word in words {
        let word_width = display_width(&word.text);
        let needs_space = current.len() > 1;
        let extra = if needs_space { 1 } else { 0 } + word_width;

        if needs_space && current_width + extra > width && current.len() > 1 {
            lines.push(Line::from(current));
            current_prefix = cont_prefix;
            current = vec![
                Span::raw(current_prefix.to_string()),
                Span::styled(word.text, word.style),
            ];
            current_width = display_width(current_prefix) + word_width;
        } else {
            if needs_space {
                current.push(Span::raw(" ".to_string()));
                current_width += 1;
            }
            current.push(Span::styled(word.text, word.style));
            current_width += word_width;
        }
    }

    if current.len() > 1 {
        lines.push(Line::from(current));
    }
    lines
}

struct StyledWord {
    text: String,
    style: Style,
}

fn inline_words(text: &str, base_style: Style) -> Vec<StyledWord> {
    let mut words = Vec::new();
    parse_inline(text, base_style, &mut words);
    words
}

fn parse_inline(text: &str, style: Style, out: &mut Vec<StyledWord>) {
    parse_inline_rest(text, style, out);
}

fn parse_inline_rest(rest: &str, style: Style, out: &mut Vec<StyledWord>) {
    let mut plain = String::new();
    let mut rest = rest;

    fn flush_plain(plain: &mut String, style: Style, out: &mut Vec<StyledWord>) {
        if plain.is_empty() {
            return;
        }
        push_plain_words(plain, style, out);
        plain.clear();
    }

    fn push_plain_words(text: &str, style: Style, out: &mut Vec<StyledWord>) {
        for word in text.split_whitespace() {
            if !word.is_empty() {
                out.push(StyledWord {
                    text: word.to_string(),
                    style,
                });
            }
        }
    }

    fn push_code_words(code: &str, style: Style, out: &mut Vec<StyledWord>) {
        if is_rs_source_ref(code) {
            push_plain_words(code, style, out);
            return;
        }
        for word in code.split_whitespace() {
            out.push(StyledWord {
                text: word.to_string(),
                style: Style::default().fg(Color::Yellow),
            });
        }
    }

    while !rest.is_empty() {
        if let Some(tail) = rest.strip_prefix('`') {
            flush_plain(&mut plain, style, out);
            if let Some(end) = tail.find('`') {
                let code = &tail[..end];
                push_code_words(code, style, out);
                rest = &tail[end + 1..];
                continue;
            }
            plain.push('`');
            break;
        }

        if let Some(tail) = rest.strip_prefix("**") {
            flush_plain(&mut plain, style, out);
            if let Some(end) = tail.find("**") {
                let bold = style.add_modifier(Modifier::BOLD);
                parse_inline_rest(&tail[..end], bold, out);
                rest = &tail[end + 2..];
                continue;
            }
            plain.push_str("**");
            rest = &rest[2..];
            continue;
        }

        if rest.starts_with('*') && !rest[1..].starts_with('*') {
            flush_plain(&mut plain, style, out);
            if let Some(tail) = rest.strip_prefix('*') {
                if let Some(end) = tail.find('*') {
                    parse_inline_rest(&tail[..end], style.add_modifier(Modifier::ITALIC), out);
                    rest = &tail[end + 1..];
                    continue;
                }
            }
            plain.push('*');
            rest = &rest[1..];
            continue;
        }

        if rest.starts_with('[') {
            flush_plain(&mut plain, style, out);
            if let Some((link, consumed)) = parse_link_prefix(rest) {
                out.push(StyledWord {
                    text: link.label,
                    style: style
                        .fg(Color::Blue)
                        .add_modifier(Modifier::UNDERLINED),
                });
                out.push(StyledWord {
                    text: format!("({})", link.url),
                    style: Style::default().fg(Color::DarkGray),
                });
                rest = &rest[consumed..];
                continue;
            }
        }

        if let Some(ch) = rest.chars().next() {
            plain.push(ch);
            rest = &rest[ch.len_utf8()..];
        } else {
            break;
        }
    }
    flush_plain(&mut plain, style, out);
}

struct ParsedLink {
    label: String,
    url: String,
}

fn parse_link_prefix(text: &str) -> Option<(ParsedLink, usize)> {
    let close_label = text.find(']')?;
    let label = text[1..close_label].to_string();
    let after = &text[close_label + 1..];
    if !after.starts_with('(') {
        return None;
    }
    let close_url = after.find(')')?;
    let url = after[1..close_url].to_string();
    let consumed = close_label + 1 + close_url + 1;
    Some((ParsedLink { label, url }, consumed))
}

fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

fn pad_display(s: &str, target: usize) -> String {
    let w = display_width(s);
    if w >= target {
        return truncate_display(s, target);
    }
    format!("{s}{}", " ".repeat(target - w))
}

fn truncate_display(s: &str, max_width: usize) -> String {
    if display_width(s) <= max_width {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw > max_width.saturating_sub(1) {
            out.push('…');
            break;
        }
        out.push(ch);
        w += cw;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_table_row() {
        let cells = parse_table_row("| a | b |");
        assert_eq!(cells, vec!["a", "b"]);
    }

    #[test]
    fn detects_heading() {
        assert_eq!(
            parse_heading("### Summary"),
            Some((3, "Summary".to_string()))
        );
    }

    #[test]
    fn preserves_chinese_text() {
        let mut words = Vec::new();
        parse_inline("中文测试和English", Style::default(), &mut words);
        let text: String = words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
        assert!(text.contains("中文测试和English"));
    }

    #[test]
    fn renders_bold_markdown() {
        let mut words = Vec::new();
        parse_inline("**核心**", Style::default(), &mut words);
        assert_eq!(words.len(), 1);
        assert_eq!(words[0].text, "核心");
        assert!(words[0].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn rs_files_use_plain_style_not_hidden() {
        let mut words = Vec::new();
        parse_inline("`tool.rs`", Style::default().fg(Color::White), &mut words);
        assert_eq!(words.len(), 1);
        assert_eq!(words[0].text, "tool.rs");
        assert_eq!(words[0].style.fg, Some(Color::White));
    }
}
