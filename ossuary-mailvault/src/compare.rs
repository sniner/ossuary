//! `fetch --compare`: which messages of a folder listing the archive
//! already holds at that place, decided by Message-ID.
//!
//! The archive's side comes from the index: every message with a
//! standing `mailbox:place` of the folder, and the `mail:message-id`
//! that `ossuary extract mail` recorded on it. The server's side is the
//! Message-ID the listing reports. Neither is reliable on its own:
//! Microsoft 365 reports a Message-ID that differs from the one in the
//! message for some messages, and one for messages that have none; two
//! different messages can share one; and a message the extractor has not
//! read has none on record. So the Message-ID settles only the plain
//! case, a Message-ID the archive holds at the place at least as often
//! as the server lists it. Everything else is fetched, and the store
//! decides by the hash whether the bytes are new. Where the comparison
//! is wrong, the cost is an extra download.
//!
//! That download would recur on every `--compare`: two copies of one
//! message on the server stay two copies under one Message-ID. So the
//! memo keeps the subject of every message `--compare` fetched, by
//! server id, and a message the Message-ID leaves to fetch is not
//! fetched again when the memo names a subject the archive holds at the
//! place.
//!
//! The rules follow the Python mailvault's comparison after its issues
//! #13 and #14.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use ossuary_core::{Attribute, Index, Scope, Term};

/// The attribute `ossuary extract mail` records the Message-ID under.
pub const MESSAGE_ID: &str = "mail:message-id";

/// Length above which a server's Message-ID may be a truncated one.
/// Exchange cuts the value in folder listings at about 255 characters;
/// the threshold lies well below, and only keeps the prefix search
/// small.
const TRUNCATED_FROM: usize = 128;

/// A Message-ID as both sides compare it: what stands between the first
/// `<` and the next `>`, or the whole value without brackets, with all
/// whitespace removed and in lower case. Empty for a value that has
/// none, which matches nothing.
///
/// Whitespace goes because unfolding a long header can leave a space
/// inside the value, and case because the Python mailvault ignores it
/// too. Both sides must go through this one function: a second
/// normalisation that disagrees on a single malformed value makes
/// archived messages look missing.
#[must_use]
pub fn key(value: &str) -> String {
    let inner = match value.find('<') {
        Some(open) => {
            let rest = &value[open + 1..];
            rest.find('>').map_or(rest, |close| &rest[..close])
        }
        None => value,
    };
    inner
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_lowercase()
}

/// Whether the server's `value` can stand for the archived `key`: equal,
/// or long enough to have been truncated and a prefix of it.
fn same(value: &str, key: &str) -> bool {
    !value.is_empty() && (key == value || (value.len() > TRUNCATED_FROM && key.starts_with(value)))
}

/// Server messages in listing order, each with its Message-ID as [`key`]
/// spells it.
pub type Keyed<T> = Vec<(T, String)>;

/// The server id an IMAP message is remembered under in the memo: a
/// UID means something only under its UIDVALIDITY.
#[must_use]
pub fn imap_id(uidvalidity: u32, uid: u32) -> String {
    format!("{uidvalidity}/{uid}")
}

/// The archive's side of one folder: each message at the place, by its
/// subject, with its Message-ID as [`key`] spells it.
pub struct Archived {
    keys: HashMap<String, String>,
}

impl Archived {
    /// The messages the index holds at `place` (an `account:folder`
    /// value of `mailbox:place`), with their Message-IDs.
    ///
    /// # Errors
    ///
    /// The index refusing.
    pub fn read(index: &Index, place: &Attribute, name: &str) -> Result<Self> {
        let message_id = Attribute::parse(MESSAGE_ID)?;
        // Quoted: the folder name is meant literally, also where it
        // contains `*` or `..`.
        let term = Term::Attribute(place.clone(), format!("\"{name}\""));
        let mut keys = HashMap::new();
        for subject in index.find(&[term], &[], Scope::Held)? {
            // A message with two Message-ID headers gets the first in
            // sort order; one that does not match costs a download.
            let key = index
                .values(&subject, &message_id, Scope::Held)?
                .iter()
                .find_map(|value| value.as_str().map(self::key))
                .unwrap_or_default();
            keys.insert(subject.as_str().to_string(), key);
        }
        Ok(Self { keys })
    }
}

/// The comparison of one folder, kept over the rounds of a fetch.
pub struct Comparison {
    archived: Archived,
    /// The messages earlier `--compare` runs fetched from the folder:
    /// the subject of each, by server id.
    known: HashMap<String, String>,
    /// Archived copies per Message-ID that still count.
    counts: HashMap<String, usize>,
    /// Archived messages the server listed under another Message-ID:
    /// they no longer count for their own.
    withdrawn: HashSet<String>,
}

/// Server messages as [`Comparison::split`] splits them.
pub struct Split<T> {
    /// To fetch.
    pub wanted: Keyed<T>,
    /// Shown to be archived by their Message-ID.
    pub kept: Keyed<T>,
    /// How many were fetched by an earlier `--compare` and are archived
    /// at the place.
    pub recalled: usize,
}

impl Comparison {
    /// The comparison against the `archived` side of the folder, and
    /// what earlier `--compare` runs fetched from it (`known`: subject
    /// by server id).
    #[must_use]
    pub fn new(archived: Archived, known: HashMap<String, String>) -> Self {
        let mut counts = HashMap::new();
        for key in archived.keys.values().filter(|key| !key.is_empty()) {
            *counts.entry(key.clone()).or_insert(0) += 1;
        }
        Self {
            archived,
            known,
            counts,
            withdrawn: HashSet::new(),
        }
    }

    /// How many messages the archive holds at the place.
    #[must_use]
    pub fn archived(&self) -> usize {
        self.archived.keys.len()
    }

    /// How many of them have no Message-ID on record, because the
    /// extractor has not read them or the message has none, and were
    /// not fetched by an earlier `--compare`: their copies on the server
    /// are fetched.
    #[must_use]
    pub fn without_message_id(&self) -> usize {
        let fetched: HashSet<&str> = self.known.values().map(String::as_str).collect();
        self.archived
            .keys
            .iter()
            .filter(|(subject, key)| key.is_empty() && !fetched.contains(subject.as_str()))
            .count()
    }

    /// Whether an earlier `--compare` fetched the message with this
    /// server id.
    #[must_use]
    pub fn remembers(&self, id: &str) -> bool {
        self.known.contains_key(id)
    }

    /// The server ids of fetched messages that no longer count: not
    /// `listed`, or not a message the archive holds at the place.
    #[must_use]
    pub fn stale(&self, listed: impl Fn(&str) -> bool) -> Vec<&str> {
        self.known
            .iter()
            .filter(|(id, subject)| !listed(id) || !self.archived.keys.contains_key(*subject))
            .map(|(id, _)| id.as_str())
            .collect()
    }

    /// Split server messages, each with its [`key`], into those to fetch
    /// and those shown to be archived. `id` gives a message's server id.
    ///
    /// The Message-ID decides first. A message it leaves to fetch is not
    /// fetched if an earlier `--compare` fetched it and the archive holds
    /// that subject at the place. It counts as landed (see
    /// [`landed`](Self::landed)), and messages reopened by that are
    /// split the same way.
    pub fn split<T>(&mut self, listed: Keyed<T>, id: impl Fn(&T) -> String) -> Split<T> {
        let (mut wanted, mut kept) = self.by_message_id(listed);
        let mut fetch = Vec::new();
        let mut recalled = 0;
        loop {
            let mut reopened = false;
            for (item, key) in wanted {
                let subject = self
                    .known
                    .get(&id(&item))
                    .filter(|subject| self.archived.keys.contains_key(*subject))
                    .cloned();
                if let Some(subject) = subject {
                    recalled += 1;
                    reopened |= self.landed(&key, &subject);
                } else {
                    fetch.push((item, key));
                }
            }
            if !reopened {
                break;
            }
            (wanted, kept) = self.by_message_id(kept);
        }
        Split {
            wanted: fetch,
            kept,
            recalled,
        }
    }

    /// Split server messages into those to fetch and those the
    /// Message-ID shows to be archived. Both keep the listing's order.
    fn by_message_id<T>(&self, listed: Keyed<T>) -> (Keyed<T>, Keyed<T>) {
        let archived = settled(&self.counts, listed.iter().map(|(_, key)| key.as_str()));
        let mut wanted = Vec::new();
        let mut kept = Vec::new();
        for (item, archived) in listed.into_iter().zip(archived) {
            if archived {
                kept.push(item);
            } else {
                wanted.push(item);
            }
        }
        (wanted, kept)
    }

    /// A message fetched under the server's `key` turned out to be the
    /// archived `subject`. If the archive holds it under another
    /// Message-ID, it no longer counts for that one, and the messages
    /// still kept by it have to be split again. Answers whether that is
    /// so.
    pub fn landed(&mut self, key: &str, subject: &str) -> bool {
        let Some(archived) = self.archived.keys.get(subject) else {
            return false;
        };
        if archived.is_empty() || same(key, archived) || !self.withdrawn.insert(subject.into()) {
            return false;
        }
        if let Some(count) = self.counts.get_mut(archived) {
            *count = count.saturating_sub(1);
        }
        true
    }
}

/// For each server key in listing order: whether the Message-ID alone
/// shows the message to be archived.
///
/// Only where the archive holds a Message-ID at least as often as the
/// server lists it. Where the server lists it more often, every message
/// under it is fetched, not only the excess: the Message-ID cannot say
/// which of them is the archived one, and handing the archived copy to
/// whichever comes first leaves a different version listed first
/// unfetched on every run.
fn settled<'a>(counts: &HashMap<String, usize>, keys: impl Iterator<Item = &'a str>) -> Vec<bool> {
    let mut ledger = Ledger::new(counts);
    let claims: Vec<(bool, Vec<String>)> = keys.map(|key| ledger.claim(key)).collect();
    let contested: HashSet<&str> = claims
        .iter()
        .filter(|(taken, _)| !taken)
        .flat_map(|(_, reached)| reached.iter().map(String::as_str))
        .collect();
    claims
        .iter()
        .map(|(taken, reached)| {
            *taken && !reached.iter().any(|key| contested.contains(key.as_str()))
        })
        .collect()
}

/// Archived copies per Message-ID, claimed one by one in listing order.
struct Ledger {
    left: HashMap<String, usize>,
    /// The keys long enough that a truncated server value may stand for
    /// them, sorted for the prefix search.
    long: Vec<String>,
}

impl Ledger {
    fn new(counts: &HashMap<String, usize>) -> Self {
        let left: HashMap<String, usize> = counts
            .iter()
            .filter(|(key, count)| !key.is_empty() && **count > 0)
            .map(|(key, count)| (key.clone(), *count))
            .collect();
        let mut long: Vec<String> = left
            .keys()
            .filter(|key| key.len() > TRUNCATED_FROM)
            .cloned()
            .collect();
        long.sort_unstable();
        Self { left, long }
    }

    /// Claim one archived copy for the server's `value`. Answers whether
    /// a copy was left, and the archived keys involved: the one a copy
    /// was taken from, or every one `value` reached and found used up.
    /// None reached means the archive has no copy under this Message-ID.
    ///
    /// A count run down to zero keeps its entry, which is what tells a
    /// later copy of the same Message-ID from one nobody archived.
    fn claim(&mut self, value: &str) -> (bool, Vec<String>) {
        if value.is_empty() {
            return (false, Vec::new());
        }
        if let Some(left) = self.left.get_mut(value) {
            let taken = *left > 0;
            if taken {
                *left -= 1;
            }
            return (taken, vec![value.to_string()]);
        }
        if value.len() <= TRUNCATED_FROM {
            return (false, Vec::new());
        }
        // In sorted order every key starting with `value` lies at or after
        // the first one not below it. The scan goes on past used-up
        // matches: a later one may still have a copy.
        let start = self.long.partition_point(|key| key.as_str() < value);
        let mut reached = Vec::new();
        for candidate in &self.long[start..] {
            if !candidate.starts_with(value) {
                break;
            }
            let left = self
                .left
                .get_mut(candidate)
                .expect("long holds keys of left");
            if *left > 0 {
                *left -= 1;
                return (true, vec![candidate.clone()]);
            }
            reached.push(candidate.clone());
        }
        (false, reached)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn archived(archived: &[(&str, &str)]) -> Archived {
        Archived {
            keys: archived
                .iter()
                .map(|(subject, value)| ((*subject).to_string(), key(value)))
                .collect(),
        }
    }

    fn comparison(held: &[(&str, &str)]) -> Comparison {
        remembering(held, &[])
    }

    /// A comparison that knows what earlier runs fetched: subject by
    /// server id.
    fn remembering(held: &[(&str, &str)], known: &[(&str, &str)]) -> Comparison {
        let known = known
            .iter()
            .map(|(id, subject)| ((*id).to_string(), (*subject).to_string()))
            .collect();
        Comparison::new(archived(held), known)
    }

    /// Server messages by id, each with the Message-ID listed for it.
    fn listing<'a>(listed: &[(&'a str, &str)]) -> Keyed<&'a str> {
        listed.iter().map(|(id, value)| (*id, key(value))).collect()
    }

    fn ids(keyed: &Keyed<&str>) -> Vec<String> {
        keyed.iter().map(|(id, _)| (*id).to_string()).collect()
    }

    fn split<'a>(comparison: &mut Comparison, listed: &[(&'a str, &str)]) -> Split<&'a str> {
        comparison.split(listing(listed), |id| (*id).to_string())
    }

    /// The server's Message-IDs split: the indexes of those to fetch.
    fn wanted(comparison: &mut Comparison, listed: &[&str]) -> Vec<usize> {
        let listed: Vec<(usize, String)> = listed.iter().map(|v| key(v)).enumerate().collect();
        comparison
            .split(listed, ToString::to_string)
            .wanted
            .into_iter()
            .map(|(index, _)| index)
            .collect()
    }

    #[test]
    fn a_message_id_compares_without_brackets_whitespace_and_case() {
        assert_eq!(key("<Abc@Example.ORG>"), "abc@example.org");
        assert_eq!(key("  <abc@\r\n example.org> (comment)"), "abc@example.org");
        assert_eq!(key("abc@example.org"), "abc@example.org");
        assert_eq!(key("<abc@example.org"), "abc@example.org", "cut off");
        assert_eq!(key(""), "");
        assert_eq!(key("<>"), "");
    }

    #[test]
    fn a_message_id_held_as_often_as_listed_is_settled() {
        let mut comparison = comparison(&[("s1", "<a@x>"), ("s2", "<b@x>")]);
        assert_eq!(wanted(&mut comparison, &["<a@x>", "<b@x>", "<c@x>"]), [2]);
    }

    #[test]
    fn a_message_id_listed_more_often_than_held_fetches_every_copy() {
        let mut comparison = comparison(&[("s1", "<a@x>")]);
        assert_eq!(
            wanted(&mut comparison, &["<a@x>", "<b@x>", "<a@x>"]),
            [0, 1, 2],
            "the archived copy may be either of the two"
        );
    }

    #[test]
    fn a_message_without_a_message_id_matches_nothing() {
        let held = [("s1", ""), ("s2", "<a@x>")];
        assert_eq!(wanted(&mut comparison(&held), &["", "<a@x>"]), [0]);
        assert_eq!(comparison(&held).archived(), 2);
        assert_eq!(comparison(&held).without_message_id(), 1);
        assert_eq!(
            remembering(&held, &[("M1", "s1")]).without_message_id(),
            0,
            "fetched by an earlier --compare"
        );
    }

    #[test]
    fn a_truncated_message_id_matches_its_prefix() {
        let long = format!("<{}@example.org>", "a".repeat(200));
        let cut = &long[..140];
        let mut comparison = comparison(&[("s1", &long)]);
        assert_eq!(wanted(&mut comparison, &[cut]), Vec::<usize>::new());
        assert_eq!(wanted(&mut comparison, &[cut, cut]), [0, 1]);
        assert_eq!(
            wanted(&mut comparison, &[&long[..20]]),
            [0],
            "a short value is no truncation"
        );
    }

    #[test]
    fn an_archived_message_listed_under_another_message_id_reopens_its_own() {
        // A is archived under <a@x> but listed as <other@x>; B, not
        // archived, carries <a@x>.
        let mut comparison = comparison(&[("A", "<a@x>")]);
        let first = split(&mut comparison, &[("A", "<other@x>"), ("B", "<a@x>")]);
        assert_eq!(ids(&first.wanted), ["A"]);
        assert!(comparison.landed(&key("<other@x>"), "A"));
        assert!(!comparison.landed(&key("<other@x>"), "A"), "withdrawn once");
        let again = comparison.split(first.kept, |id| (*id).to_string());
        assert_eq!(ids(&again.wanted), ["B"]);
    }

    #[test]
    fn a_message_an_earlier_compare_fetched_is_not_fetched_again() {
        // Two copies on the server, one archived message.
        let held = [("S", "<a@x>")];
        let known = [("M1", "S"), ("M2", "S")];
        let mut comparison = remembering(&held, &known);
        let sorted = split(&mut comparison, &[("M1", "<a@x>"), ("M2", "<a@x>")]);
        assert!(sorted.wanted.is_empty());
        assert_eq!(sorted.recalled, 2);

        let mut comparison = remembering(&held, &known);
        let sorted = split(
            &mut comparison,
            &[("M1", "<a@x>"), ("M2", "<a@x>"), ("M3", "<a@x>")],
        );
        assert_eq!(ids(&sorted.wanted), ["M3"], "a third copy is new");
    }

    #[test]
    fn a_remembered_message_the_archive_no_longer_holds_is_fetched() {
        let mut comparison = remembering(&[("S", "<a@x>")], &[("M1", "T"), ("M2", "S")]);
        let sorted = split(&mut comparison, &[("M1", "<b@x>")]);
        assert_eq!(ids(&sorted.wanted), ["M1"]);
        assert_eq!(sorted.recalled, 0);
    }

    #[test]
    fn a_remembered_message_listed_under_another_message_id_reopens_its_own() {
        // M1 is the archived A, listed under another Message-ID; M2,
        // not archived, carries A's.
        let mut comparison = remembering(&[("A", "<a@x>")], &[("M1", "A")]);
        let sorted = split(&mut comparison, &[("M1", "<other@x>"), ("M2", "<a@x>")]);
        assert_eq!(ids(&sorted.wanted), ["M2"]);
        assert_eq!(sorted.recalled, 1);
        assert!(sorted.kept.is_empty());
    }

    #[test]
    fn rows_of_messages_no_longer_listed_or_held_are_stale() {
        let comparison = remembering(&[("S", "<a@x>")], &[("M1", "S"), ("M2", "S"), ("M3", "T")]);
        let mut stale = comparison.stale(|id| id != "M2");
        stale.sort_unstable();
        assert_eq!(stale, ["M2", "M3"]);
        assert!(comparison.remembers("M1"));
        assert!(!comparison.remembers("M4"));
    }

    #[test]
    fn a_message_fetched_under_its_own_message_id_changes_nothing() {
        let mut comparison = comparison(&[("A", "<a@x>")]);
        assert!(!comparison.landed(&key("<a@x>"), "A"));
        assert!(
            !comparison.landed(&key("<b@x>"), "elsewhere"),
            "not archived at this place"
        );
    }
}
