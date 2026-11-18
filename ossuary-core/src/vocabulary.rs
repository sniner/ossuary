//! The attributes this crate reads or writes itself.
//!
//! What each of them means is in `docs/vocabulary.md`. Everything else in
//! a log is vocabulary of the programs and extractors that wrote it, and
//! this crate passes it through without reading it.

use crate::claim::Attribute;

/// A place a file was seen at: an absolute path, or a path inside
/// another file when it begins with `@`.
pub(crate) const FILE_PATH: &str = "file:path";
/// A name a file was known by.
pub(crate) const FILE_NAME: &str = "file:name";
/// A file's MIME type, detected or announced.
pub(crate) const FILE_MIME: &str = "file:mime";
/// A file's length in bytes.
pub(crate) const FILE_SIZE: &str = "file:size";
/// A file's modification time as the filesystem reported it.
pub(crate) const FILE_MODIFIED: &str = "file:modified";
/// The file a derived file was made from. The value names a subject.
pub(crate) const PROV_ORIGIN: &str = "prov:origin";
/// The machine a file was seen on.
pub(crate) const PROV_HOST: &str = "prov:host";
/// The receipt attribute: this file was examined by the claim's source,
/// which the value names again.
pub const EXAMINED: &str = "prov:examined";
/// A mailbox folder a message was seen in, `account:folder`.
pub(crate) const MAILBOX_PLACE: &str = "mailbox:place";
/// A tag the user gave a file.
pub(crate) const USER_TAG: &str = "user:tag";
/// A comment the user made on a file.
pub(crate) const USER_COMMENT: &str = "user:comment";

/// One of the attributes above as an [`Attribute`].
///
/// # Panics
///
/// When `name` is not an attribute by the claim grammar; the names in
/// this module are.
pub(crate) fn known_attribute(name: &'static str) -> Attribute {
    Attribute::parse(name).expect("a known attribute")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_name_is_an_attribute() {
        for name in [
            FILE_PATH,
            FILE_NAME,
            FILE_MIME,
            FILE_SIZE,
            FILE_MODIFIED,
            PROV_ORIGIN,
            PROV_HOST,
            EXAMINED,
            MAILBOX_PLACE,
            USER_TAG,
            USER_COMMENT,
        ] {
            assert_eq!(known_attribute(name).as_str(), name);
        }
    }
}
