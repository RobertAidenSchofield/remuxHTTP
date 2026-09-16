use regex::{Regex, RegexBuilder};

/// Pre-validates whether a regex pattern string is syntactically valid.
pub fn validate_regex_pattern(pattern: &str) -> Result<(), regex::Error> {
    RegexBuilder::new(pattern).build().map(|_| ())
}

/// Safely compiles a single regex pattern using `RegexBuilder`.
/// Returns `Some(Regex)` if valid, or `None` if invalid.
pub fn compile_safe_regex(pattern: &str) -> Option<Regex> {
    RegexBuilder::new(pattern).build().ok()
}

/// Safely compiles a list of pattern strings, ignoring invalid syntax.
/// Returns a tuple containing:
/// - `Vec<Regex>`: all successfully compiled regexes
/// - `Vec<String>`: any pattern strings that failed compilation
pub fn compile_safe_regexes<I, S>(patterns: I) -> (Vec<Regex>, Vec<String>)
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut compiled = Vec::new();
    let mut invalid = Vec::new();

    for p in patterns {
        let pattern_str = p.as_ref();
        match RegexBuilder::new(pattern_str).build() {
            Ok(re) => compiled.push(re),
            Err(_) => invalid.push(pattern_str.to_string()),
        }
    }

    (compiled, invalid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_regex_pattern() {
        assert!(validate_regex_pattern(r"^hello.*world$").is_ok());
        assert!(validate_regex_pattern(r"[unclosed-bracket").is_err());
    }

    #[test]
    fn test_compile_safe_regex() {
        assert!(compile_safe_regex(r"^valid[0-9]+").is_some());
        assert!(compile_safe_regex(r"*(invalid").is_none());
    }

    #[test]
    fn test_compile_safe_regexes_filters_invalid() {
        let patterns = vec![
            "valid_one.*".to_string(),
            "[unclosed".to_string(),
            "^valid_two$".to_string(),
            "?+bad".to_string(),
        ];

        let (compiled, invalid) = compile_safe_regexes(&patterns);
        assert_eq!(compiled.len(), 2);
        assert_eq!(invalid.len(), 2);
        assert_eq!(invalid, vec!["[unclosed", "?+bad"]);
        assert!(compiled[0].is_match("valid_one_test"));
        assert!(compiled[1].is_match("valid_two"));
    }
}

