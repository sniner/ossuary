//! An IMAP folder: the resume point, the pending list, Gmail's labels.

use super::*;
use crate::fetch::imap_folder::carry_on;

#[test]
fn where_to_carry_on_from() {
    assert_eq!(carry_on(None, 7, None).0, 0, "first fetch: whole");
    assert_eq!(
        carry_on(Some(at(7, 40)), 7, None).0,
        40,
        "the promise holds: above the point"
    );
    assert_eq!(carry_on(Some(at(7, 40)), 8, None).0, 0, "renumbered: whole");
    assert_eq!(
        carry_on(Some(at(7, 40)), 7, Some("--full")),
        (0, "full fetch (--full)".to_string()),
        "--full: whole, whatever the point"
    );
    assert_eq!(
        carry_on(Some(at(7, 40)), 7, Some("--compare")).1,
        "full fetch (--compare)"
    );
    assert!(carry_on(Some(at(7, 40)), 7, None).1.contains("40"));
}

#[test]
fn a_first_fetch_takes_the_folder_whole_and_leaves_the_point_at_the_top() {
    let bench = bench();
    let mut inbox = mailbox(7, &[3, 5, 9]);

    let tally = fetch(&bench, &mut inbox, false, false);

    assert_eq!((tally.stored, tally.known), (3, 0));
    assert_eq!(tally.claims, 9, "place, size, kind — each message");
    assert!(tally.failed.is_empty());
    assert_eq!(
        point(&bench),
        Some(at(7, 9)),
        "handed back newest first, the point still ends at the highest UID"
    );

    let again = fetch(&bench, &mut inbox, false, false);
    assert_eq!(
        (again.stored, again.known, again.claims),
        (0, 0, 0),
        "nothing above the point, nothing fetched, nothing said"
    );
    assert_eq!(point(&bench), Some(at(7, 9)));
}

#[test]
fn an_empty_folder_gets_a_point_too() {
    let bench = bench();
    let mut inbox = mailbox(7, &[]);
    fetch(&bench, &mut inbox, false, false);
    assert_eq!(
        point(&bench),
        Some(at(7, 0)),
        "seen under this promise, with nothing in it — not a folder never fetched"
    );
}

#[test]
fn a_renumbered_folder_is_fetched_whole() {
    let bench = bench();
    fetch(&bench, &mut mailbox(7, &[3, 5, 9]), false, false);

    let mut renumbered = mailbox(8, &[1, 2]);
    let tally = fetch(&bench, &mut renumbered, false, false);

    assert_eq!(tally.stored, 2, "new numbers, new messages here");
    assert_eq!(point(&bench), Some(at(8, 2)), "the point starts over");
}

#[test]
fn full_fetches_everything_again_and_says_the_place_again() {
    let bench = bench();
    let mut inbox = mailbox(7, &[3, 5]);
    fetch(&bench, &mut inbox, false, false);

    let tally = fetch(&bench, &mut inbox, true, false);

    assert_eq!((tally.stored, tally.known), (0, 2), "the bytes are held");
    assert_eq!(
        tally.claims, 4,
        "place and the told kind, each message — the size the log has"
    );
    assert_eq!(point(&bench), Some(at(7, 5)));
}

#[test]
fn a_dry_run_counts_and_writes_nothing() {
    let bench = bench();
    let tally = fetch(&bench, &mut mailbox(7, &[3, 5]), false, true);
    assert_eq!((tally.would, tally.claims), (2, 0));
    assert!(bench.archive.log().head().unwrap().is_empty());
    assert_eq!(point(&bench), None, "a rehearsal leaves no point");
}

#[test]
fn a_server_giving_up_mid_folder_is_named_and_what_landed_stays() {
    let bench = bench();
    let mut inbox = mailbox(7, &[3, 5, 9]);
    inbox.give_up_after = Some(1);

    let tally = fetch(&bench, &mut inbox, false, false);

    assert_eq!(tally.stored, 1);
    assert_eq!(tally.failed.len(), 1);
    assert!(tally.failed[0].contains("example.org:INBOX"));
    assert_eq!(
        point(&bench),
        None,
        "the one that landed was the newest; below it two are missing, so the point stays"
    );

    inbox.give_up_after = None;
    let rest = fetch(&bench, &mut inbox, false, false);
    assert_eq!(
        (rest.stored, rest.known),
        (2, 1),
        "the rest lands, the one already held dedups"
    );
    assert_eq!(point(&bench), Some(at(7, 9)));
}

#[test]
fn a_refused_message_goes_on_the_pending_list_and_the_point_passes_it() {
    let bench = bench();
    let mut inbox = mailbox(7, &[3, 5, 9]);
    inbox.refused.insert(5);

    let tally = fetch(&bench, &mut inbox, false, false);

    assert_eq!(tally.stored, 2);
    assert_eq!(tally.failed, ["example.org:INBOX: UID 5: NO"]);
    assert_eq!(imap_pending(&bench), [("5".to_string(), 1)]);
    assert_eq!(
        point(&bench),
        Some(at(7, 9)),
        "the refused one is on the list, so nothing below 9 is missing"
    );
}

#[test]
fn the_next_run_asks_for_a_pending_message_first_and_takes_it_off() {
    let bench = bench();
    let mut inbox = mailbox(7, &[3, 5, 9]);
    inbox.refused.insert(5);
    fetch(&bench, &mut inbox, false, false);

    inbox.refused.clear();
    inbox
        .messages
        .insert(11, b"Subject: 11\r\n\r\nbody 11".to_vec());
    let tally = fetch(&bench, &mut inbox, false, false);

    assert_eq!(tally.stored, 2, "the pending one and the new one");
    assert!(tally.failed.is_empty(), "{:?}", tally.failed);
    assert!(imap_pending(&bench).is_empty());
    assert_eq!(point(&bench), Some(at(7, 11)));
}

#[test]
fn a_message_refused_in_three_runs_is_given_up_and_full_fetches_it_again() {
    let bench = bench();
    let mut inbox = mailbox(7, &[3, 5, 9]);
    inbox.refused.insert(5);
    fetch(&bench, &mut inbox, false, false);
    fetch(&bench, &mut inbox, false, false);
    assert_eq!(imap_pending(&bench), [("5".to_string(), 2)]);

    let third = fetch(&bench, &mut inbox, false, false);

    assert_eq!(
        third.failed,
        ["example.org:INBOX: UID 5: NO; given up after 3 runs, --full fetches it again"]
    );
    assert!(imap_pending(&bench).is_empty());

    inbox.refused.clear();
    let full = fetch(&bench, &mut inbox, true, false);
    assert_eq!(full.stored, 1, "the given-up one, at last");
}

#[test]
fn a_pending_message_no_longer_in_the_folder_comes_off_the_list() {
    let bench = bench();
    let mut inbox = mailbox(7, &[3, 5, 9]);
    inbox.refused.insert(5);
    fetch(&bench, &mut inbox, false, false);

    inbox.refused.clear();
    inbox.messages.remove(&5);
    let tally = fetch(&bench, &mut inbox, false, false);

    assert!(tally.failed.is_empty(), "{:?}", tally.failed);
    assert!(imap_pending(&bench).is_empty());
}

#[test]
fn ten_refusals_in_a_row_leave_the_rest_of_the_folder_for_the_next_run() {
    let bench = bench();
    let mut inbox = mailbox(7, &[1, 2]);
    fetch(&bench, &mut inbox, false, false);
    for uid in 3..=14 {
        inbox.messages.insert(
            uid,
            format!("Subject: {uid}\r\n\r\nbody {uid}").into_bytes(),
        );
        inbox.refused.insert(uid);
    }

    let tally = fetch(&bench, &mut inbox, false, false);

    assert_eq!(tally.failed.len(), 11, "{:?}", tally.failed);
    assert_eq!(
        tally.failed[10],
        "example.org:INBOX: 10 messages in a row refused; 2 messages left for the next run"
    );
    assert_eq!(imap_pending(&bench).len(), 10);
    assert_eq!(
        point(&bench),
        Some(at(7, 2)),
        "handed back newest first: 3 and 4 were never asked for, so the point stays"
    );

    inbox.refused.clear();
    let rest = fetch(&bench, &mut inbox, false, false);
    assert!(rest.failed.is_empty(), "{:?}", rest.failed);
    assert_eq!(rest.stored, 12);
    assert!(imap_pending(&bench).is_empty());
    assert_eq!(point(&bench), Some(at(7, 14)));
}

#[test]
fn a_renumbered_folder_takes_the_pending_list_with_it() {
    let bench = bench();
    let mut inbox = mailbox(7, &[3, 5, 9]);
    inbox.refused.insert(5);
    fetch(&bench, &mut inbox, false, false);

    let mut renumbered = mailbox(8, &[1, 2]);
    let tally = fetch(&bench, &mut renumbered, false, false);

    assert!(tally.failed.is_empty(), "{:?}", tally.failed);
    assert!(
        imap_pending(&bench).is_empty(),
        "UID 5 meant another message there"
    );
}

#[test]
fn a_full_fetch_stopped_short_keeps_the_pending_list() {
    let bench = bench();
    let mut inbox = mailbox(7, &[3, 5, 9]);
    inbox.refused.insert(3);
    fetch(&bench, &mut inbox, false, false);
    assert_eq!(point(&bench), Some(at(7, 9)));

    // The connection drops before the full fetch reaches UID 3. The
    // point is still at 9, so only the list names UID 3 as missing.
    inbox.refused.clear();
    inbox.give_up_after = Some(0);
    let full = fetch(&bench, &mut inbox, true, false);
    assert_eq!(full.failed.len(), 1);
    assert_eq!(imap_pending(&bench), [("3".to_string(), 1)]);

    inbox.give_up_after = None;
    let next = fetch(&bench, &mut inbox, false, false);
    assert_eq!(next.stored, 1, "the pending one");
    assert!(imap_pending(&bench).is_empty());
}

#[test]
fn a_full_fetch_asks_for_the_pending_messages_first_and_takes_them_off() {
    let bench = bench();
    let mut inbox = mailbox(7, &[3, 5, 9]);
    inbox.refused.insert(3);
    fetch(&bench, &mut inbox, false, false);

    inbox.refused.clear();
    inbox.fetched.clear();
    let full = fetch(&bench, &mut inbox, true, false);

    assert_eq!(inbox.fetched, [3, 5, 9]);
    assert_eq!((full.stored, full.known), (1, 2));
    assert!(imap_pending(&bench).is_empty());
    assert_eq!(point(&bench), Some(at(7, 9)));
}

#[test]
fn a_dry_run_counts_the_pending_messages_too() {
    let bench = bench();
    let mut inbox = mailbox(7, &[3, 5, 9]);
    inbox.refused.insert(5);
    fetch(&bench, &mut inbox, false, false);

    let tally = fetch(&bench, &mut inbox, false, true);

    assert_eq!(tally.would, 1);
    assert_eq!(
        imap_pending(&bench),
        [("5".to_string(), 1)],
        "and touches none"
    );
}

#[test]
fn gmail_labels_are_recorded_and_those_gone_taken_back_when_the_message_comes_again() {
    let bench = bench();
    let one = mail(1, "<one@example.org>");
    let two = mail(2, "<two@example.org>");

    let first = fetch_labeled(
        &bench,
        &mut labeled(
            7,
            &[
                (1, &one, &["/Important", "Pers\u{f6}nlich"]),
                (2, &two, &[]),
            ],
        ),
        false,
    );
    assert_eq!(
        (first.stored, first.claims, first.taken),
        (2, 8, 0),
        "place, size, kind each, and two labels on the first"
    );
    assert_eq!(
        held(&bench, &one, "mailbox:tag"),
        ["/Important", "Pers\u{f6}nlich"]
    );
    assert!(held(&bench, &two, "mailbox:tag").is_empty());

    // Fetched again with one label removed on the server.
    let again = fetch_labeled(
        &bench,
        &mut labeled(7, &[(1, &one, &["Pers\u{f6}nlich"]), (2, &two, &[])]),
        true,
    );
    assert_eq!(again.taken, 1, "/Important is taken back");
    assert_eq!(held(&bench, &one, "mailbox:tag"), ["Pers\u{f6}nlich"]);
    assert_eq!(
        held(&bench, &one, "mailbox:place"),
        ["example.org:INBOX"],
        "the place stays"
    );

    // A label removed from a message that is not fetched again is not
    // seen.
    let later = fetch_labeled(
        &bench,
        &mut labeled(7, &[(1, &one, &[]), (2, &two, &[])]),
        false,
    );
    assert_eq!((later.stored, later.known, later.taken), (0, 0, 0));
    assert_eq!(held(&bench, &one, "mailbox:tag"), ["Pers\u{f6}nlich"]);
}

/// One message under the labels `/Inbox` and `Work`, as Gmail hands it
/// over from each of the two folders: without the folder's own label.
fn in_two_folders(one: &[u8]) -> (Fake, Fake) {
    let mut work = labeled(7, &[(1, one, &["/Inbox"])]);
    work.listed = Labels::Without("Work".to_string());
    let mut inbox = labeled(9, &[(1, one, &["Work"])]);
    inbox.listed = Labels::Without("/Inbox".to_string());
    (work, inbox)
}

#[test]
fn a_gmail_message_fetched_from_two_folders_keeps_the_label_of_each() {
    let bench = bench();
    let one = mail(1, "<one@example.org>");
    let (mut work, mut inbox) = in_two_folders(&one);

    let tally = run_folders(
        &bench,
        &mut [("Work", &mut work), ("INBOX", &mut inbox)],
        &options(false, false),
    );

    assert_eq!(tally.taken, 0, "INBOX does not list Work's own label gone");
    assert_eq!(held(&bench, &one, "mailbox:tag"), ["/Inbox", "Work"]);
}

#[test]
fn a_gmail_folder_still_takes_back_a_label_that_is_not_its_own() {
    let bench = bench();
    let one = mail(1, "<one@example.org>");
    let (mut work, mut inbox) = in_two_folders(&one);
    run_folders(
        &bench,
        &mut [("Work", &mut work), ("INBOX", &mut inbox)],
        &options(false, false),
    );

    // The label Work was removed from the message.
    inbox.labels.insert(1, Vec::new());
    let again = run_folders(&bench, &mut [("INBOX", &mut inbox)], &options(true, false));

    assert_eq!(again.taken, 1);
    assert_eq!(held(&bench, &one, "mailbox:tag"), ["/Inbox"]);
}

#[test]
fn a_gmail_folder_whose_own_label_is_not_known_takes_nothing_back() {
    let bench = bench();
    let one = mail(1, "<one@example.org>");
    let mut all = labeled(7, &[(1, &one, &["/Sent", "Work"])]);
    // Read from the Sent folder the list lacks /Sent, and the folder
    // list names the folder's use, not its label.
    let mut sent = labeled(9, &[(1, &one, &["Work"])]);
    sent.listed = Labels::WithoutOne;
    run_folders(
        &bench,
        &mut [
            ("[Gmail]/All Mail", &mut all),
            ("[Gmail]/Sent Mail", &mut sent),
        ],
        &options(false, false),
    );
    assert_eq!(held(&bench, &one, "mailbox:tag"), ["/Sent", "Work"]);

    sent.labels.insert(1, Vec::new());
    let again = run_folders(
        &bench,
        &mut [("[Gmail]/Sent Mail", &mut sent)],
        &options(true, false),
    );

    assert_eq!(
        again.taken, 0,
        "Work may be gone, but so may /Sent seem to be"
    );
    assert_eq!(held(&bench, &one, "mailbox:tag"), ["/Sent", "Work"]);
}

#[test]
fn a_server_without_labels_records_no_tags() {
    let bench = bench();
    let one = mail(1, "<one@example.org>");
    let tally = fetch(&bench, &mut holding(7, &[(1, &one)]), false, false);
    assert_eq!(tally.claims, 3, "place, size, kind");
    assert!(held(&bench, &one, "mailbox:tag").is_empty());
}
