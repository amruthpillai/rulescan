use proptest::prelude::*;
use rulescan::config::{Case, CompiledRule, MatchSpec, Rule};
use std::path::Path;

fn rule(glob: String) -> CompiledRule {
    CompiledRule::new(Rule {
        id: "property".into(),
        matcher: MatchSpec {
            basename: None,
            glob: Some(glob),
            bundle_marker: None,
            case: Case::Sensitive,
        },
        category: "instructions".into(),
        on_new: "review".into(),
        on_change: "review".into(),
    })
    .unwrap()
}
proptest! {
    #[test]
    fn single_star_never_crosses_a_directory(a in "[a-z]{1,12}", b in "[a-z]{1,12}") {
        let rule=rule("*.md".into());
        prop_assert!(rule.matches(Path::new(&format!("{a}.md"))),"basename must match");
        prop_assert!(!rule.matches(Path::new(&format!("{a}/{b}.md"))),"separator must not match");
    }
    #[test]
    fn globstar_includes_root_and_nested(a in "[a-z]{1,12}") {
        let rule=rule("**/AGENTS.md".into());
        prop_assert!(rule.matches(Path::new("AGENTS.md")));
        prop_assert!(rule.matches(Path::new(&format!("{a}/AGENTS.md"))),"nested path must match");
    }
}
