//! Hiding what follows a match, for output that will be shared.
//!
//! Best effort, made for the layout breach lists overwhelmingly use: the account first, then the
//! secret (`email:password`, `https://site/login:user:password`). It hides everything after the
//! match and the separator that follows it. A line that keeps the password *before* the account
//! (`password:email`) is not protected by it; `--extract` is the precise tool for that.

/// What stands in for a hidden secret.
pub const MASK: &str = "***";

fn is_separator(c: char) -> bool {
    matches!(c, ':' | ';' | ',' | '|' | '\t' | ' ')
}

/// `line` with everything after `span` (and the separator right after it) replaced by `***`.
/// The span still points at the same bytes. A line with nothing after the match is unchanged.
pub fn mask_after_match(line: &str, span: (usize, usize)) -> String {
    let mut end = span.1.min(line.len());
    while !line.is_char_boundary(end) {
        end += 1;
    }
    let rest = &line[end..];
    let Some(first) = rest.chars().next() else { return line.to_string() };
    if is_separator(first) {
        let after = &rest[first.len_utf8()..];
        if after.is_empty() {
            return line.to_string();
        }
        return format!("{}{first}{MASK}", &line[..end]);
    }
    format!("{}{MASK}", &line[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hides_what_follows_the_match() {
        let l = "alice@example.com:hunter2";
        assert_eq!(mask_after_match(l, (0, 17)), "alice@example.com:***");
        assert_eq!(mask_after_match("https://example.com/login:bob:pw", (8, 19)), "https://example.com***");
        assert_eq!(mask_after_match("bob@x.test;a b c", (0, 10)), "bob@x.test;***");
    }

    #[test]
    fn leaves_lines_with_nothing_after_the_match() {
        assert_eq!(mask_after_match("alice@example.com", (0, 17)), "alice@example.com");
        assert_eq!(mask_after_match("alice@example.com:", (0, 17)), "alice@example.com:");
    }

    #[test]
    fn keeps_the_text_before_and_survives_odd_spans() {
        assert_eq!(mask_after_match("user=alice@example.com pw", (5, 22)), "user=alice@example.com ***");
        assert_eq!(mask_after_match("é:secret", (0, 1)), "é:***", "a span inside a character moves to its end");
        assert_eq!(mask_after_match("short", (0, 99)), "short");
    }
}
