//! Locale detection. Strings live in `locales/<lang>.yml`; English is the fallback.

/// Picks the UI language from `LC_ALL`/`LC_MESSAGES`/`LANG`, then the system locale
/// (`fr_FR.UTF-8` / `fr-FR` → `fr`), falling back to English.
pub fn init() {
    let lang = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|var| std::env::var(var).ok().filter(|v| !v.is_empty()))
        .or_else(sys_locale::get_locale)
        .and_then(|tag| tag.split(['-', '_', '.']).next().map(str::to_lowercase))
        .filter(|lang| rust_i18n::available_locales!().iter().any(|l| l == lang))
        .unwrap_or_else(|| "en".to_owned());
    rust_i18n::set_locale(&lang);
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    /// Every translated key must exist in the English fallback.
    #[test]
    fn translations_are_subsets_of_english() {
        let keys = |yml: &str| -> BTreeSet<String> {
            yml.lines()
                .filter(|l| !l.starts_with([' ', '_']))
                .filter_map(|l| l.split_once(':').map(|(k, _)| k.to_owned()))
                .collect()
        };
        let en = keys(include_str!("../locales/en.yml"));
        let fr = keys(include_str!("../locales/fr.yml"));
        let missing: Vec<_> = fr.difference(&en).collect();
        assert!(missing.is_empty(), "keys missing in en.yml: {missing:?}");
        assert_eq!(en, fr, "fr.yml and en.yml must define the same keys");
    }
}
