//! Allowlisted external URL actions consumed from tray flags.

use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) const UPDATES_URL: &str = "https://github.com/mudrii/compme/releases/latest";
pub(crate) const WEBSITE_URL: &str = "https://github.com/mudrii/compme";
pub(crate) const SUPPORT_URL: &str = "https://github.com/mudrii/compme/issues/new";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UrlAction {
    CheckUpdates,
    VisitWebsite,
    ContactSupport,
}

impl UrlAction {
    pub(crate) fn url(self) -> &'static str {
        match self {
            Self::CheckUpdates => UPDATES_URL,
            Self::VisitWebsite => WEBSITE_URL,
            Self::ContactSupport => SUPPORT_URL,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::CheckUpdates => "updates",
            Self::VisitWebsite => "website",
            Self::ContactSupport => "support",
        }
    }
}

pub(crate) struct UrlActionFlags<'a> {
    pub(crate) check_updates: &'a AtomicBool,
    pub(crate) visit_website: &'a AtomicBool,
    pub(crate) contact_support: &'a AtomicBool,
}

/// Consume every armed external-link flag once, in stable menu order.
pub(crate) fn take_url_actions(flags: UrlActionFlags<'_>) -> Vec<UrlAction> {
    [
        (flags.check_updates, UrlAction::CheckUpdates),
        (flags.visit_website, UrlAction::VisitWebsite),
        (flags.contact_support, UrlAction::ContactSupport),
    ]
    .into_iter()
    .filter_map(|(flag, action)| flag.swap(false, Ordering::Relaxed).then_some(action))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_use_exact_allowlisted_urls() {
        assert_eq!(
            [
                UrlAction::CheckUpdates.url(),
                UrlAction::VisitWebsite.url(),
                UrlAction::ContactSupport.url(),
            ],
            [UPDATES_URL, WEBSITE_URL, SUPPORT_URL]
        );
    }

    #[test]
    fn armed_actions_are_consumed_once_in_menu_order() {
        let updates = AtomicBool::new(true);
        let website = AtomicBool::new(false);
        let support = AtomicBool::new(true);
        let flags = || UrlActionFlags {
            check_updates: &updates,
            visit_website: &website,
            contact_support: &support,
        };

        assert_eq!(
            take_url_actions(flags()),
            [UrlAction::CheckUpdates, UrlAction::ContactSupport]
        );
        assert!(take_url_actions(flags()).is_empty());
        assert!(!updates.load(Ordering::Relaxed));
        assert!(!support.load(Ordering::Relaxed));
    }
}
