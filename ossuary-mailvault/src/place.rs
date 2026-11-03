//! Where a message was seen: a mailbox and a folder in it, as one value.
//!
//! The two belong together. Split across attributes they would fall
//! apart in the set — a message seen in two folders of two accounts
//! could no longer say which folder was in which account — so the place
//! is one string: the account's name, a colon, the folder as the server
//! names it. Account names never contain a colon (the configuration
//! refuses them), so the first colon always divides the two. A colon
//! rather than a slash, because folder names use slashes for their own
//! hierarchy, and `gmail.com:[Gmail]/All Mail` shows at a glance where
//! the account ends.
//!
//! It is information, nothing more: "this is where the message was when
//! it was fetched", the way `file:path` says where a file sat. An
//! account renamed later is a new name in new sightings; the old ones
//! stay true for their time.

/// The attribute every place stands under.
pub const ATTRIBUTE: &str = "mailbox:place";

/// The marks the mailbox has on a message beside its place: Outlook's
/// categories and the labels of Gmail and Proton Mail, by name. Not in
/// the message's bytes, and not a place: state as of the last sighting.
/// Each fetch of a message records them anew and retracts those no
/// longer there, except a Proton label: only its folder shows it, and
/// that never says which labels a message lacks.
pub const TAG: &str = "mailbox:tag";

/// A folder of an account.
#[must_use]
pub fn folder(account: &str, folder: &str) -> String {
    format!("{account}:{folder}")
}

/// An account whose folder is not known — a vault that recorded the
/// mailbox a message came from, but not where in it.
#[must_use]
pub fn account(account: &str) -> String {
    account.to_string()
}

/// A folder with no account behind it — what a vault's import records:
/// a name it was given, and nobody to ask about it again.
#[must_use]
pub fn orphan(folder: &str) -> String {
    format!(":{folder}")
}

/// A Gmail label as a tag. Gmail marks its own labels with a leading
/// backslash (`\Inbox`, `\Sent`); the tag has a slash there instead
/// (`/Inbox`), because a backslash has to be escaped in JSON, in the
/// shell and in a glob.
#[must_use]
pub fn gmail_label(label: &str) -> String {
    match label.strip_prefix('\\') {
        Some(rest) => format!("/{rest}"),
        None => label.to_string(),
    }
}

/// Where Proton Mail Bridge shows the labels, one folder each.
const PROTON_LABELS: &str = "Labels/";

/// The label a Proton Mail Bridge folder stands for: `Sales` for
/// `Labels/Sales`; `None` for every other folder.
#[must_use]
pub fn proton_label(folder: &str) -> Option<&str> {
    folder
        .strip_prefix(PROTON_LABELS)
        .filter(|label| !label.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_place_is_the_account_then_the_folder() {
        assert_eq!(folder("example.org", "INBOX"), "example.org:INBOX");
        assert_eq!(
            folder("example.org", "[Gmail]/All Mail"),
            "example.org:[Gmail]/All Mail",
            "the folder keeps its own slashes and spaces"
        );
        assert_eq!(account("example.org"), "example.org");
        assert_eq!(orphan("old mail"), ":old mail");
    }

    #[test]
    fn a_gmail_label_of_gmails_own_leads_with_a_slash() {
        assert_eq!(gmail_label("\\Inbox"), "/Inbox");
        assert_eq!(gmail_label("Work/Projects"), "Work/Projects");
        assert_eq!(gmail_label("a\\b"), "a\\b", "only the leading one");
    }

    #[test]
    fn a_proton_label_folder_is_one_under_labels() {
        assert_eq!(proton_label("Labels/Sales"), Some("Sales"));
        assert_eq!(proton_label("Folders/Work"), None);
        assert_eq!(proton_label("Labels/"), None);
        assert_eq!(proton_label("All Mail"), None);
    }
}
