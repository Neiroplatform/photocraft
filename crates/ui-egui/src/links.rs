//! Product-owned external links.
//!
//! Pixelle deliberately exposes no upstream community or repository links in the product UI.
//! Keep this module as the single policy point for future first-party links.

/// Help-menu external link commands: none until a Pixelle-owned destination is approved.
pub const COMMANDS: &[(&str, &str)] = &[];

pub fn url_for(id: &str) -> Option<&'static str> {
    COMMANDS.iter().find(|command| command.0 == id).map(|command| command.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_upstream_community_links_are_exposed() {
        assert!(COMMANDS.is_empty());
        for id in ["help.discord", "help.website", "help.artcraftWebsite", "help.github", "help.reportIssue"] {
            assert_eq!(url_for(id), None, "legacy command {id} must stay unavailable");
        }
    }

    #[test]
    fn help_menu_contains_only_product_owned_entries() {
        let app = crate::PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        let help: Vec<String> = crate::menus::menu_items(&app).into_iter().filter(|item| item.path == ["Help"]).map(|item| item.id).collect();
        assert_eq!(help, ["help.systemInfo", "help.about"]);
    }
}
