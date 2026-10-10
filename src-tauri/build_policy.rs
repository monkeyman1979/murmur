pub fn rejects_debug_tooling(debug_feature_enabled: bool, profile: Option<&str>) -> bool {
    debug_feature_enabled && profile != Some("debug")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_tooling_is_allowed_only_for_the_explicit_debug_profile() {
        assert!(!rejects_debug_tooling(true, Some("debug")));
        assert!(rejects_debug_tooling(true, Some("release")));
        assert!(rejects_debug_tooling(true, Some("production")));
        assert!(rejects_debug_tooling(true, None));
        assert!(!rejects_debug_tooling(false, Some("production")));
    }
}
