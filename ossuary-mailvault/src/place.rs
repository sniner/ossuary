//! Where a message was seen: a mailbox and a folder in it, as one value.
//!
//! The two belong together. Split across attributes they would fall
//! apart in the set — a message seen in two folders of two accounts
//! could no longer say which folder was in which account — so the place
//! is one string: the account's name, a slash, the folder as the server
//! spells it. Account names never contain a slash (the configuration
//! refuses them), so the first slash always divides the two, and a
//! folder may carry as many as its hierarchy needs.
//!
//! It is information, nothing more: "this is where the message was when
//! it was fetched", the way `file:path` says where a file sat. An
//! account renamed later is a new name in new sightings; the old ones
//! stay true for their time.

/// The attribute every place stands under.
pub const ATTRIBUTE: &str = "mailbox:place";

/// When a place was seen, where that is not the claim's own time: a
/// takeover repeats what a mailvault archive's log saw years earlier,
/// and says so in the log's own words.
pub const SEEN: &str = "mailbox:seen";

/// A folder of an account.
#[must_use]
pub fn folder(account: &str, folder: &str) -> String {
    format!("{account}/{folder}")
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
    format!("/{folder}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_place_is_the_account_then_the_folder() {
        assert_eq!(folder("example.org", "INBOX"), "example.org/INBOX");
        assert_eq!(
            folder("example.org", "[Gmail]/All Mail"),
            "example.org/[Gmail]/All Mail",
            "the folder keeps its own slashes and spaces"
        );
        assert_eq!(account("example.org"), "example.org");
        assert_eq!(orphan("old mail"), "/old mail");
    }
}
