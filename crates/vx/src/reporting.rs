use owo_colors::OwoColorize;

pub(crate) fn print_fatal_error(error: &eyre::Report) {
    // Cap content width: readable at 60, never wider than terminal
    let term_width = terminal_width().saturating_sub(4).max(40);
    let content_width = term_width.min(90);

    let full_message = format!("{error:#}");
    let wrapped = soft_wrap(&full_message, content_width);
    let lines: Vec<&str> = wrapped.lines().collect();
    let bar = "─".repeat(content_width + 2);

    eprintln!();
    eprintln!("{}", format!("┌{bar}┐").red().bold());
    eprintln!(
        "{}",
        format!("│ {:content_width$} │", "FATAL ERROR").red().bold()
    );
    eprintln!("{}", format!("├{bar}┤").red().bold());
    for line in &lines {
        let visible_len = visible_width(line);
        let padding = content_width.saturating_sub(visible_len);
        eprintln!(
            "{}{}{}{}",
            "│ ".red().bold(),
            line,
            " ".repeat(padding),
            " │".red().bold(),
        );
    }
    eprintln!("{}", format!("└{bar}┘").red().bold());
    eprintln!();
}

/// Count visible display width, ignoring ANSI escape sequences.
fn visible_width(s: &str) -> usize {
    use unicode_width::UnicodeWidthStr;
    let stripped = strip_ansi_escapes::strip(s);
    let plain = String::from_utf8_lossy(&stripped);
    UnicodeWidthStr::width(plain.as_ref())
}

fn terminal_width() -> usize {
    // Try COLUMNS env var first, then ioctl
    if let Ok(cols) = std::env::var("COLUMNS")
        && let Ok(w) = cols.parse::<usize>()
    {
        return w;
    }
    #[cfg(unix)]
    {
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        let ret = unsafe { libc::ioctl(2, libc::TIOCGWINSZ, &mut ws) };
        if ret == 0 && ws.ws_col > 0 {
            return ws.ws_col as usize;
        }
    }
    80
}

fn soft_wrap(text: &str, max_width: usize) -> String {
    let mut out = String::new();
    for line in text.lines() {
        if visible_width(line) <= max_width {
            out.push_str(line);
            out.push('\n');
        } else {
            // Extract the visual indent (leading whitespace after stripping ANSI).
            // We keep the full prefix (ANSI escapes + spaces) so continuations
            // preserve both color and indentation.
            let (indent, indent_width) = extract_indent(line);
            let cont_width = max_width.saturating_sub(indent_width).max(20);

            let mut first = true;
            let mut remaining = line;
            while !remaining.is_empty() {
                // First line uses full width; continuations use reduced width
                // because we prepend the indent prefix.
                let width = if first { max_width } else { cont_width };
                if visible_width(remaining) <= width {
                    out.push_str(remaining);
                    out.push('\n');
                    break;
                }
                let break_pos = find_wrap_point(remaining, width);
                let chunk = &remaining[..break_pos];
                out.push_str(chunk);
                let active_sgr = last_sgr(chunk);
                if active_sgr.is_some() {
                    out.push_str("\x1b[0m");
                }
                out.push('\n');
                remaining = remaining[break_pos..].trim_start();
                first = false;
                // Prepend indent on continuation lines
                if !remaining.is_empty() {
                    out.push_str(indent);
                    if let Some(ref sgr) = active_sgr {
                        out.push_str(sgr);
                    }
                }
            }
        }
    }
    out
}

/// Extract the leading "indent" from a line that may contain ANSI escapes.
///
/// Returns `(prefix, visible_width)` where `prefix` is the byte slice
/// covering all leading ANSI escapes and whitespace, and `visible_width`
/// is how many display columns the whitespace occupies.
fn extract_indent(line: &str) -> (&str, usize) {
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut width = 0;

    while i < bytes.len() {
        // Skip ANSI CSI sequences (ESC [ ... final_byte)
        if bytes[i] == b'\x1b' && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            i += 2;
            while i < bytes.len() && !(0x40..=0x7E).contains(&bytes[i]) {
                i += 1;
            }
            if i < bytes.len() {
                i += 1; // skip final byte
            }
            continue;
        }
        // Count spaces/tabs
        if bytes[i] == b' ' {
            width += 1;
            i += 1;
        } else if bytes[i] == b'\t' {
            width += 4; // approximate
            i += 1;
        } else {
            break;
        }
    }

    (&line[..i], width)
}

/// Extract the last SGR (Select Graphic Rendition) sequence from a string,
/// or None if the string ends with a reset or has no SGR sequences.
fn last_sgr(s: &str) -> Option<String> {
    let mut last: Option<String> = None;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\x1b' && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            let start = i;
            i += 2;
            while i < bytes.len() && !(0x40..=0x7E).contains(&bytes[i]) {
                i += 1;
            }
            if i < bytes.len() {
                i += 1; // skip final byte
                let seq = &s[start..i];
                // Only track SGR sequences (ending with 'm')
                if seq.ends_with('m') {
                    if seq == "\x1b[0m" || seq == "\x1b[m" {
                        last = None; // reset clears active style
                    } else {
                        last = Some(seq.to_string());
                    }
                }
            }
        } else {
            i += 1;
        }
    }
    last
}

/// Find byte offset in `s` where visible width reaches `max_width`,
/// preferring to break at a space. Skips ANSI escape sequences.
fn find_wrap_point(s: &str, max_width: usize) -> usize {
    use unicode_width::UnicodeWidthChar;

    let bytes = s.as_bytes();
    let mut orig_pos = 0;
    let mut col = 0;
    let mut last_space = None;

    while orig_pos < bytes.len() && col < max_width {
        if bytes[orig_pos] == b'\x1b' && orig_pos + 1 < bytes.len() && bytes[orig_pos + 1] == b'[' {
            // Skip CSI sequence: ESC [ ... final_byte
            orig_pos += 2;
            while orig_pos < bytes.len() && !(0x40..=0x7E).contains(&bytes[orig_pos]) {
                orig_pos += 1;
            }
            if orig_pos < bytes.len() {
                orig_pos += 1;
            }
        } else {
            let c = s[orig_pos..].chars().next().unwrap();
            let w = UnicodeWidthChar::width(c).unwrap_or(0);
            if col + w > max_width {
                break;
            }
            if c == ' ' {
                last_space = Some(orig_pos);
            }
            col += w;
            orig_pos += c.len_utf8();
        }
    }

    last_space.unwrap_or(orig_pos)
}
