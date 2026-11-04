//! Proton Mail Bridge: a message is known by the id Proton gives it,
//! not by its bytes.
//!
//! The Bridge builds each message when a client fetches it, and writes
//! two ids into its header. `X-Pm-Internal-Id` is the message's id at
//! Proton: the same in every folder, and after the Bridge has rebuilt
//! its local database. `X-Pm-Gluon-Id` is the id of the Bridge's local
//! database and new after every rebuild, and a multipart message gets a
//! new MIME boundary each time it is built. A message fetched again
//! after a rebuild is other bytes, and the store would hold it twice.
//!
//! So whether the archive holds a message from a Proton account is
//! decided by `mailbox:id`, which the fetch and the import record from
//! the first of those headers. A message whose id is on record is not
//! downloaded again, and its place goes to the file that has the id.

use anyhow::Result;
use ossuary_core::{Attribute, Index, Scope, Subject, Term, Value};

use crate::remote::header_value;

/// The header the Bridge writes Proton's id of the message into.
pub const HEADER: &str = "X-Pm-Internal-Id";

/// The attribute the id is recorded under. The value names the service
/// first, `proton:…`, so one attribute holds the ids of every service
/// that has them.
pub const ATTRIBUTE: &str = "mailbox:id";

/// The `mailbox:id` value for the header's value; `None` for an empty
/// one.
#[must_use]
pub fn id(header: &str) -> Option<String> {
    let id = header.trim();
    (!id.is_empty()).then(|| format!("proton:{id}"))
}

/// The `mailbox:id` value a whole message carries in its header, if
/// any.
///
/// The Bridge always writes its own value into this header, and an id
/// is too long to come about by chance, so a message that carries one
/// is the Proton message of that id, also where it was later moved to
/// another server.
#[must_use]
pub fn id_of(message: &[u8]) -> Option<String> {
    id(&header_value(message, HEADER))
}

/// The listed UIDs of a folder, by what the archive holds of them.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Sorted {
    /// To download, with the id to record: an id not on record, or none
    /// listed.
    pub fetch: Vec<(u32, Option<String>)>,
    /// On record: each UID with the files that carry its id.
    pub known: Vec<(u32, Vec<Subject>)>,
}

/// Sort `asked` by what the archive holds of each UID. `listed` is the
/// header's value as the server listed it; a UID it leaves out is
/// downloaded, and the download finds out what became of it.
/// `holders` answers which files carry an id.
///
/// # Errors
///
/// Whatever `holders` answers.
pub fn sort(
    asked: &[u32],
    listed: &[(u32, String)],
    mut holders: impl FnMut(&str) -> Result<Vec<Subject>>,
) -> Result<Sorted> {
    let listed: std::collections::HashMap<u32, &str> = listed
        .iter()
        .map(|(uid, value)| (*uid, value.as_str()))
        .collect();
    let mut sorted = Sorted::default();
    for &uid in asked {
        let Some(id) = listed.get(&uid).and_then(|value| id(value)) else {
            sorted.fetch.push((uid, None));
            continue;
        };
        let held = holders(&id)?;
        if held.is_empty() {
            sorted.fetch.push((uid, Some(id)));
        } else {
            sorted.known.push((uid, held));
        }
    }
    Ok(sorted)
}

/// Which of the files that carry one id a fact goes to: each of those
/// present that lacks it, or, when none is present, each that lacks it.
///
/// Several files carry one id when the Bridge handed the message over
/// as other bytes before; a file whose places were retracted stays out
/// of the way as long as another is present.
///
/// # Errors
///
/// Whatever `present` or `has` answers.
pub fn receivers(
    holders: &[Subject],
    present: impl Fn(&Subject) -> Result<bool>,
    has: impl Fn(&Subject) -> Result<bool>,
) -> Result<Vec<Subject>> {
    let mut placed = Vec::new();
    for subject in holders {
        if present(subject)? {
            placed.push(subject.clone());
        }
    }
    let candidates = if placed.is_empty() {
        holders.to_vec()
    } else {
        placed
    };
    let mut lacking = Vec::new();
    for subject in candidates {
        if !has(&subject)? {
            lacking.push(subject);
        }
    }
    Ok(lacking)
}

/// The record's side of [`sort`] and [`receivers`]: the files that
/// carry an id, whether a file has a place, whether it has a value.
pub struct Record<'a> {
    pub index: &'a Index,
    pub id: &'a Attribute,
    pub place: &'a Attribute,
}

impl Record<'_> {
    /// The files that carry `id`, whether placed or not.
    ///
    /// # Errors
    ///
    /// The index refusing.
    pub fn holders(&self, id: &str) -> Result<Vec<Subject>> {
        // Quoted: the id is meant literally.
        let term = Term::Attribute(self.id.clone(), format!("\"{id}\""));
        Ok(self.index.find(&[term], &[], Scope::Held)?)
    }

    /// The files among `holders` that `attribute` = `value` goes to,
    /// after [`receivers`]. A message is present here when it has a
    /// standing place.
    ///
    /// # Errors
    ///
    /// The index refusing.
    pub fn receivers(
        &self,
        holders: &[Subject],
        attribute: &Attribute,
        value: &Value,
    ) -> Result<Vec<Subject>> {
        receivers(
            holders,
            |subject| {
                Ok(!self
                    .index
                    .values(subject, self.place, Scope::Held)?
                    .is_empty())
            },
            |subject| {
                Ok(self
                    .index
                    .values(subject, attribute, Scope::Held)?
                    .contains(value))
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subject(n: u8) -> Subject {
        Subject::parse(&format!("{n:02x}").repeat(32)).unwrap()
    }

    #[test]
    fn the_id_names_the_service_and_an_empty_header_is_none() {
        assert_eq!(id(" abc== "), Some("proton:abc==".to_string()));
        assert_eq!(id(""), None);
        assert_eq!(
            id_of(b"Subject: x\r\nX-Pm-Internal-Id: abc==\r\n\r\nbody"),
            Some("proton:abc==".to_string())
        );
        assert_eq!(
            id_of(b"Subject: x\r\n\r\nX-Pm-Internal-Id: abc==\r\n"),
            None
        );
    }

    #[test]
    fn listed_uids_sort_into_known_and_to_fetch() {
        let one = subject(1);
        let held = |id: &str| {
            Ok(if id == "proton:known" {
                vec![one.clone()]
            } else {
                Vec::new()
            })
        };
        let listed = [
            (1, "known".to_string()),
            (2, "new".to_string()),
            (3, String::new()),
        ];
        let sorted = sort(&[1, 2, 3, 4], &listed, held).unwrap();
        assert_eq!(sorted.known, [(1, vec![one])]);
        assert_eq!(
            sorted.fetch,
            [(2, Some("proton:new".to_string())), (3, None), (4, None)],
            "a new id, no header, not listed"
        );
    }

    #[test]
    fn a_fact_goes_to_the_present_files_that_lack_it() {
        let (placed, retracted, tagged) = (subject(1), subject(2), subject(3));
        let present = |s: &Subject| Ok(*s != retracted);
        let has = |s: &Subject| Ok(*s == tagged);
        let both = [placed.clone(), retracted.clone()];
        assert_eq!(
            receivers(&both, present, has).unwrap(),
            vec![placed],
            "not the one whose places were retracted"
        );
        assert!(
            receivers(std::slice::from_ref(&tagged), present, has)
                .unwrap()
                .is_empty(),
            "nothing to say again"
        );
        assert_eq!(
            receivers(std::slice::from_ref(&retracted), |_| Ok(false), has).unwrap(),
            vec![retracted.clone()],
            "none present: all of them"
        );
    }
}
