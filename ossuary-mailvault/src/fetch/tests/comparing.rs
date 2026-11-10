//! `fetch --compare` over IMAP.

use super::*;

#[test]
fn compare_fetches_only_what_the_archive_lacks_under_its_message_id() {
    let bench = bench();
    let one = mail(1, "<one@example.org>");
    let two = mail(2, "<two@example.org>");
    let three = mail(3, "<three@example.org>");
    fetch(
        &bench,
        &mut holding(7, &[(1, &one), (2, &two)]),
        false,
        false,
    );
    extracted(&bench, &one, "<one@example.org>");
    extracted(&bench, &two, "<Two@Example.org>");
    // Renumbered, where a plain fetch takes every message again.
    let mut inbox = holding(8, &[(1, &one), (2, &two), (3, &three)]);

    let tally = compare(&bench, &mut inbox, false);

    assert_eq!((tally.stored, tally.known, tally.matched), (1, 0, 2));
    assert!(tally.failed.is_empty(), "{:?}", tally.failed);
    assert_eq!(point(&bench), Some(at(8, 3)));
    assert!(imap_pending(&bench).is_empty());
    let again = fetch(&bench, &mut inbox, false, false);
    assert_eq!(
        (again.stored, again.known),
        (0, 0),
        "the next run carries on from the top"
    );
}

#[test]
fn compare_fetches_messages_without_a_message_id_on_record_again() {
    let bench = bench();
    let one = mail(1, "<one@example.org>");
    let mut inbox = holding(7, &[(1, &one)]);
    fetch(&bench, &mut inbox, false, false);

    let tally = compare(&bench, &mut inbox, false);

    assert_eq!((tally.stored, tally.known, tally.matched), (0, 1, 0));
    let again = compare(&bench, &mut inbox, false);
    assert_eq!(
        (again.known, again.recalled),
        (0, 1),
        "once: the first compare found it archived"
    );
}

#[test]
fn compare_does_not_fetch_again_what_an_earlier_compare_found_archived() {
    // Two copies of one message on the server, one in the archive.
    let bench = bench();
    let one = mail(1, "<one@example.org>");
    fetch(&bench, &mut holding(7, &[(1, &one)]), false, false);
    extracted(&bench, &one, "<one@example.org>");
    let mut inbox = holding(7, &[(1, &one), (2, &one)]);

    let first = compare(&bench, &mut inbox, false);
    let dry = compare(&bench, &mut inbox, true);
    let second = compare(&bench, &mut inbox, false);

    assert_eq!(
        (first.known, first.recalled),
        (2, 0),
        "listed twice, held once: both copies fetched"
    );
    assert_eq!(compared(&bench), ["7/1", "7/2"]);
    assert_eq!((dry.would, dry.matched, dry.recalled), (0, 0, 2));
    assert_eq!(
        (second.stored, second.known, second.recalled, second.claims),
        (0, 0, 2, 0)
    );
    assert_eq!(compared(&bench), ["7/1", "7/2"], "still needed");

    // Renumbered, the same UIDs mean nothing.
    let renumbered = compare(&bench, &mut holding(8, &[(1, &one), (2, &one)]), false);
    assert_eq!((renumbered.known, renumbered.recalled), (2, 0));
    assert_eq!(compared(&bench), ["8/1", "8/2"]);
}

#[test]
fn compare_forgets_what_the_message_id_settles_and_what_is_gone() {
    let bench = bench();
    let one = mail(1, "<one@example.org>");
    let two = mail(2, "<two@example.org>");
    let mut inbox = holding(7, &[(1, &one), (2, &two)]);
    let first = compare(&bench, &mut inbox, false);
    assert_eq!(first.stored, 2);
    assert_eq!(compared(&bench), ["7/1", "7/2"]);
    extracted(&bench, &one, "<one@example.org>");
    inbox.messages.remove(&2);

    let again = compare(&bench, &mut inbox, false);

    assert_eq!((again.known, again.matched, again.recalled), (0, 1, 0));
    assert!(
        compared(&bench).is_empty(),
        "7/1 is settled by its Message-ID, 7/2 is no longer listed"
    );
}

#[test]
fn compare_fetches_every_copy_of_a_message_id_listed_more_often_than_held() {
    // Two versions under one Message-ID, one of them archived.
    let bench = bench();
    let archived = mail(1, "<same@example.org>");
    let missing = mail(2, "<same@example.org>");
    fetch(&bench, &mut holding(7, &[(1, &archived)]), false, false);
    extracted(&bench, &archived, "<same@example.org>");
    // The missing version listed first.
    let mut inbox = holding(8, &[(1, &missing), (2, &archived)]);

    let tally = compare(&bench, &mut inbox, false);

    assert_eq!((tally.stored, tally.known, tally.matched), (1, 1, 0));
}

#[test]
fn compare_looks_again_when_an_archived_message_is_listed_under_another_message_id() {
    // The archived message is listed under another Message-ID, and
    // a missing one carries the archived one's.
    let bench = bench();
    let archived = mail(1, "<a@example.org>");
    let missing = mail(2, "<a@example.org>");
    fetch(&bench, &mut holding(7, &[(1, &archived)]), false, false);
    extracted(&bench, &archived, "<a@example.org>");
    let mut inbox = holding(8, &[(1, &archived), (2, &missing)]);
    inbox.listed_as.insert(1, "<other@example.org>".to_string());

    let tally = compare(&bench, &mut inbox, false);

    assert_eq!((tally.stored, tally.known, tally.matched), (1, 1, 0));
    assert!(imap_pending(&bench).is_empty());
}

#[test]
fn a_dry_run_of_compare_counts_and_writes_nothing() {
    let bench = bench();
    let one = mail(1, "<one@example.org>");
    fetch(&bench, &mut holding(7, &[(1, &one)]), false, false);
    extracted(&bench, &one, "<one@example.org>");
    let two = mail(2, "<two@example.org>");
    let mut inbox = holding(7, &[(1, &one), (2, &two)]);

    let tally = compare(&bench, &mut inbox, true);

    assert_eq!((tally.would, tally.matched, tally.claims), (1, 1, 0));
    assert_eq!(point(&bench), Some(at(7, 1)), "the point stays");
    assert!(imap_pending(&bench).is_empty());
}
