//! Emitters: serialize a [`crate::plan::BuildPlan`] for a CI system.
//!
//! The plan itself is the stable interface; emitters are deliberately thin.
//! Anything not covered here is expected to consume the JSON form directly.

pub mod github;
pub mod gitlab;

use crate::plan::BuildPlan;

/// The canonical JSON form of a plan.
pub fn plan_json(plan: &BuildPlan) -> String {
    let mut out = serde_json::to_string_pretty(plan).expect("plan serializes");
    out.push('\n');
    out
}

/// Full image references (`image:tag`) for one target, in tag order.
pub(crate) fn image_refs(target: &crate::plan::Target) -> Vec<String> {
    target
        .tags
        .iter()
        .map(|tag| format!("{}:{tag}", target.image))
        .collect()
}

/// Quote a string for POSIX shell interpolation.
pub(crate) fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-./=:@".contains(c))
    {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use crate::emit::shell_quote;

    #[test]
    fn quoting() {
        assert_eq!(shell_quote("plain-1.2/x"), "plain-1.2/x");
        assert_eq!(shell_quote("has space"), "'has space'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote(""), "''");
    }
}
