use super::*;

#[test]
fn name_normalizes_backslash_and_dotseg() {
    assert_eq!(name_of("a\\b/c").as_str(), "a/b/c");
    assert_eq!(name_of("./x").as_str(), "x");
}
