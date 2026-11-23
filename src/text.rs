//! The texts of the plugin, in English and French.

pub fn t(french: bool, key: &str) -> &'static str {
    let (en, fr) = match key {
        "local" => ("Local", "Local"),
        "close" => ("Close", "Fermer"),
        "new" => ("New terminal", "Nouveau terminal"),
        "find" => ("Find", "Chercher"),
        "find_hint" => ("Find in the terminal", "Chercher dans le terminal"),
        "none" => ("No terminal: + opens one", "Aucun terminal : + en ouvre un"),
        "failed" => (
            "The terminal could not start",
            "Le terminal n'a pas pu démarrer",
        ),
        "not_found" => ("Not found", "Introuvable"),
        _ => ("", ""),
    };
    if french {
        fr
    } else {
        en
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_text_exists_in_both_languages() {
        for key in [
            "local",
            "close",
            "new",
            "find",
            "find_hint",
            "none",
            "failed",
            "not_found",
        ] {
            assert!(
                !super::t(false, key).is_empty() && !super::t(true, key).is_empty(),
                "{key}"
            );
        }
    }
}
