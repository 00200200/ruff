/// Look up a Unicode name or alias, ignoring ASCII case but preserving separators.
pub(super) fn character(input: &str) -> Option<char> {
    // No Unicode name starts with a hyphen. The upstream loose matcher assumes
    // that a hyphen has a preceding byte, which panics here in debug builds.
    if input.starts_with('-') {
        return None;
    }

    let character = unicode_names2::character(input)?;
    let matches_name = unicode_names2::name(character).is_some_and(|name| {
        name.flat_map(str::bytes)
            .eq(input.bytes().map(|byte| byte.to_ascii_uppercase()))
    });

    // Aliases can name control characters that have no primary Unicode name.
    (matches_name
        || unicode_names2::alias(input).is_some_and(|(alias_character, spelling)| {
            alias_character == character && spelling.eq_ignore_ascii_case(input)
        }))
    .then_some(character)
}
