//! Text normalization for search: case folding, accent stripping and tokenizing.

use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

/// Lowercases, strips accents (NFKD minus combining marks) and splits on anything that is
/// not a letter or digit. "Saint-Jérôme" gives `["saint", "jerome"]`.
pub fn tokens(text: &str) -> Vec<String> {
    let folded: String = text
        .nfkd()
        .filter(|c| !is_combining_mark(*c))
        .flat_map(char::to_lowercase)
        .collect();
    folded
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_accents_and_case() {
        assert_eq!(tokens("Saint-Jérôme"), ["saint", "jerome"]);
        assert_eq!(tokens("  N. High St. "), ["n", "high", "st"]);
        assert_eq!(
            tokens("Côte-des-Neiges–Notre-Dame"),
            ["cote", "des", "neiges", "notre", "dame"]
        );
    }
}
