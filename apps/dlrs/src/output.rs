//! Terminal output helpers shared by the subcommands.

/// `println!` that exits quietly when the reader closes the pipe.
///
/// Several subcommands emit thousands of lines and are routinely piped into `head`.
/// Plain `println!` panics on a closed pipe, and the panic unwinding through a locked
/// stdout can leave the process wedged holding its own binary open.
macro_rules! outln {
    ($($arg:tt)*) => {{
        use std::io::Write;
        let stdout = std::io::stdout();
        let mut lock = stdout.lock();
        if writeln!(lock, $($arg)*).is_err() {
            std::process::exit(0);
        }
    }};
}

/// `writeln!` into a `String`, so a whole frame can be composed and written at once.
macro_rules! bufln {
    ($buf:expr, $($arg:tt)*) => {{
        use std::fmt::Write as _;
        let _ = writeln!($buf, $($arg)*);
    }};
}

/// `Some(v)` as its display form, `None` as `-`.
pub fn opt<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map(|x| x.to_string()).unwrap_or_else(|| "-".into())
}

/// Truncate to `max` chars. Steam names are user-supplied and can be far wider than
/// their column.
///
/// Never returns more than `max` characters. A column with no room for the ellipsis
/// takes the characters that do fit instead: `"..."` in a two-wide column is three
/// characters, which is the one thing a column-fitting helper must not produce.
pub fn trunc(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    if max < 3 {
        return s.chars().take(max).collect();
    }
    s.chars().take(max - 3).collect::<String>() + "..."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opt_renders_a_value_or_a_dash() {
        assert_eq!(opt(Some(7)), "7");
        assert_eq!(opt(Some("x")), "x");
        assert_eq!(opt(None::<u32>), "-");
        // The dash is what an empty column looks like, so an empty string is not it.
        assert_eq!(opt(Some("")), "");
    }

    #[test]
    fn trunc_leaves_anything_that_already_fits() {
        assert_eq!(trunc("abc", 3), "abc");
        assert_eq!(trunc("abc", 20), "abc");
        assert_eq!(trunc("", 0), "");
    }

    /// The bug this pins: `max.saturating_sub(3)` clamped to zero and returned a bare
    /// `"..."`, three characters wide, for every column narrower than three.
    #[test]
    fn trunc_never_exceeds_the_column_it_fits() {
        for max in 0..=8 {
            let out = trunc("abcdefgh", max);
            assert!(
                out.chars().count() <= max,
                "trunc(_, {max}) returned {out:?}, {} chars",
                out.chars().count()
            );
        }
        assert_eq!(trunc("abcdef", 0), "");
        assert_eq!(trunc("abcdef", 1), "a");
        assert_eq!(trunc("abcdef", 2), "ab");
        assert_eq!(trunc("abcdef", 3), "...");
        assert_eq!(trunc("abcdef", 4), "a...");
    }

    /// `chars().count()` measures characters, not bytes, so a name that is well inside
    /// its column in characters must not be cut for being wide in UTF-8.
    #[test]
    fn trunc_counts_characters_not_bytes() {
        let name = "ＡＢＣＤＥＦ"; // 6 chars, 18 bytes
        assert_eq!(name.len(), 18);
        assert_eq!(trunc(name, 6), name);
        assert_eq!(trunc(name, 5), "ＡＢ...");
        assert_eq!(trunc(name, 2), "ＡＢ");
        // Never split a character in half, whatever the width.
        for max in 0..=7 {
            assert!(trunc(name, max).chars().count() <= max);
        }
    }
}
