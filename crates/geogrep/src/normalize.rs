use deunicode::deunicode;

pub fn normalize(s: &str) -> String {
    let ascii = deunicode(s);
    let mut out = String::with_capacity(ascii.len());
    let mut space = true;
    for c in ascii.chars().flat_map(|c| c.to_lowercase()) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            space = false;
        } else if !space {
            out.push(' ');
            space = true;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

pub fn compact(normalized: &str) -> String {
    normalized.chars().filter(|c| !c.is_whitespace()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_folds_norwegian_chars() {
        assert_eq!(normalize("Ogna/Snåsavassdraget"), "ogna snasavassdraget");
        assert_eq!(normalize("Rambergveien 41"), "rambergveien 41");
        assert_eq!(normalize("Kirkegata 5B"), "kirkegata 5b");
    }

    #[test]
    fn collapses_and_trims_whitespace() {
        assert_eq!(normalize("  Ram  Berg Veien  41B  "), "ram berg veien 41b");
    }

    #[test]
    fn strips_punctuation_to_spaces() {
        assert_eq!(normalize("Rambergvn.41"), "rambergvn 41");
        assert_eq!(normalize("St. Hansgate, 12"), "st hansgate 12");
    }

    #[test]
    fn compact_strips_spaces() {
        assert_eq!(compact("ram berg veien 41"), "rambergveien41");
    }
}
