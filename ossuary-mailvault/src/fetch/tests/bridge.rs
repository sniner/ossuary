//! Proton Mail Bridge: the Proton id and the label folders.

use super::*;
use crate::config::Account;

#[test]
fn a_proton_message_on_record_is_not_fetched_again_after_the_bridge_rebuilt() {
    let bench = bench();
    let (one, two) = (built(1, "a"), built(2, "a"));
    let first = run_folders(
        &bench,
        &mut [("All Mail", &mut proton(7, &[(1, &one), (2, &two)]))],
        &options(false, false),
    );
    assert_eq!(
        (first.stored, first.claims),
        (2, 8),
        "place, size, kind, id each"
    );
    assert_eq!(held(&bench, &one, "mailbox:id"), ["proton:id1=="]);

    // The Bridge rebuilt its database: a new UIDVALIDITY, new local
    // ids, and one message more.
    let (again, three) = ([built(1, "b"), built(2, "b")], built(3, "b"));
    let mut rebuilt = proton(8, &[(1, &again[0]), (2, &again[1]), (3, &three)]);
    let tally = run_folders(
        &bench,
        &mut [("All Mail", &mut rebuilt)],
        &options(false, false),
    );
    assert_eq!(rebuilt.fetched, [3], "only the new one is downloaded");
    assert_eq!(
        (tally.stored, tally.known, tally.identified, tally.claims),
        (1, 0, 2, 4),
        "the known ones have their place already"
    );
    assert!(
        held(&bench, &again[0], "mailbox:place").is_empty(),
        "the rebuilt bytes are not stored"
    );
    assert_eq!(
        bench.memo.imap_resume("example.org", "All Mail").unwrap(),
        Some(at(8, 3)),
        "the point passes the known ones as well"
    );
}

#[test]
fn full_and_compare_on_proton_fetch_only_what_has_no_id_on_record() {
    let bench = bench();
    let one = built(1, "a");
    run_folders(
        &bench,
        &mut [("All Mail", &mut proton(7, &[(1, &one)]))],
        &options(false, false),
    );
    let (rebuilt, two) = (built(1, "b"), built(2, "b"));
    let compare = Options {
        full: false,
        compare: true,
        dry_run: false,
    };
    for (options, downloaded) in [(options(true, false), vec![2]), (compare, vec![])] {
        let mut folder = proton(7, &[(1, &rebuilt), (2, &two)]);
        run_folders(&bench, &mut [("All Mail", &mut folder)], &options);
        assert_eq!(folder.fetched, downloaded);
    }
    assert!(held(&bench, &rebuilt, "mailbox:place").is_empty());
}

#[test]
fn a_proton_message_in_two_configured_folders_is_fetched_once_and_placed_twice() {
    let bench = bench();
    let one = built(1, "a");
    let mut all = proton(7, &[(1, &one)]);
    let mut work = proton(9, &[(5, &built(1, "b"))]);
    let tally = run_folders(
        &bench,
        &mut [("All Mail", &mut all), ("Folders/Work", &mut work)],
        &options(false, false),
    );
    assert!(
        work.fetched.is_empty(),
        "the second folder finds what the first stored in this run"
    );
    assert_eq!((tally.stored, tally.identified), (1, 1));
    assert_eq!(
        held(&bench, &one, "mailbox:place"),
        ["example.org:All Mail", "example.org:Folders/Work"]
    );
}

#[test]
fn the_place_of_a_known_proton_message_goes_to_the_files_still_placed() {
    let bench = bench();
    let (kept, dropped) = (built(1, "a"), built(1, "b"));
    for bytes in [&kept, &dropped] {
        claimed(&bench, bytes, "mailbox:id", "proton:id1==", false);
        claimed(
            &bench,
            bytes,
            "mailbox:place",
            "example.org:All Mail",
            false,
        );
    }
    claimed(
        &bench,
        &dropped,
        "mailbox:place",
        "example.org:All Mail",
        true,
    );

    let mut work = proton(9, &[(5, &built(1, "c"))]);
    run_folders(
        &bench,
        &mut [("Folders/Work", &mut work)],
        &options(false, false),
    );
    assert!(work.fetched.is_empty());
    assert_eq!(
        held(&bench, &kept, "mailbox:place"),
        ["example.org:All Mail", "example.org:Folders/Work"]
    );
    assert!(
        held(&bench, &dropped, "mailbox:place").is_empty(),
        "a file whose places were retracted stays out of it"
    );
}

#[test]
fn a_dry_run_on_proton_counts_what_it_would_fetch_and_what_it_found() {
    let bench = bench();
    let one = built(1, "a");
    run_folders(
        &bench,
        &mut [("All Mail", &mut proton(7, &[(1, &one)]))],
        &options(false, false),
    );
    let head = bench.archive.log().head().unwrap().len();
    let mut folder = proton(8, &[(1, &built(1, "b")), (2, &built(2, "b"))]);
    let tally = run_folders(
        &bench,
        &mut [("All Mail", &mut folder)],
        &options(false, true),
    );
    assert_eq!((tally.would, tally.identified), (1, 1));
    assert!(folder.fetched.is_empty());
    assert_eq!(
        bench.archive.log().head().unwrap().len(),
        head,
        "nothing written"
    );
}

#[test]
fn a_proton_label_folder_tags_the_file_from_all_mail_and_stores_nothing() {
    let bench = bench();
    let (one, two, three, four) = (built(1, "a"), built(2, "a"), built(3, "a"), built(4, "a"));
    // The label folder has the first as the Bridge builds it now, and
    // a third that All Mail has not brought yet.
    let mut label = proton(9, &[(1, &built(1, "b")), (2, &two), (3, &three)]);
    let first = with_label(
        &bench,
        &mut proton(7, &[(1, &one), (2, &two)]),
        &mut label,
        &options(false, false),
    );
    assert!(
        label.fetched.is_empty(),
        "nothing downloaded from a label folder"
    );
    assert_eq!(first.stored, 2, "only All Mail stores");
    for bytes in [&one, &two] {
        assert_eq!(held(&bench, bytes, "mailbox:tag"), ["Sales"]);
        assert_eq!(
            held(&bench, bytes, "mailbox:place"),
            ["example.org:All Mail"],
            "a label folder is no place"
        );
    }
    assert_eq!(
        sales(&bench),
        (vec![3], Some(at(9, 3))),
        "the third stays open"
    );

    // The next run: All Mail brings the third and a fourth, and the
    // label folder asks about the open UID and the one above its
    // point, nothing else.
    let mut label = proton(
        9,
        &[(1, &built(1, "b")), (2, &two), (3, &three), (4, &four)],
    );
    with_label(
        &bench,
        &mut proton(7, &[(1, &one), (2, &two), (3, &three), (4, &four)]),
        &mut label,
        &options(false, false),
    );
    assert_eq!(label.headed, [3, 4]);
    for bytes in [&three, &four] {
        assert_eq!(held(&bench, bytes, "mailbox:tag"), ["Sales"]);
    }
    assert_eq!(sales(&bench), (vec![], Some(at(9, 4))));
}

#[test]
fn a_label_folder_writes_its_tags_in_one_bundle() {
    let bench = bench();
    let messages = [built(1, "a"), built(2, "a"), built(3, "a")];
    let all: Vec<(u32, &[u8])> = (1..).zip(messages.iter().map(Vec::as_slice)).collect();
    with_label(
        &bench,
        &mut proton(7, &all),
        &mut proton(9, &[]),
        &options(false, false),
    );
    let bundles = || {
        let open = bench.archive.log().open().unwrap().unwrap();
        open.bundles().len()
    };
    let before = bundles();
    with_label(
        &bench,
        &mut proton(7, &all),
        &mut proton(9, &all),
        &options(false, false),
    );
    assert_eq!(bundles(), before + 1);
    for bytes in &messages {
        assert_eq!(held(&bench, bytes, "mailbox:tag"), ["Sales"]);
    }
}

#[test]
fn an_open_uid_that_left_the_label_folder_is_dropped() {
    let bench = bench();
    let (one, three) = (built(1, "a"), built(3, "a"));
    with_label(
        &bench,
        &mut proton(7, &[(1, &one)]),
        &mut proton(9, &[(1, &one), (3, &three)]),
        &options(false, false),
    );
    assert_eq!(sales(&bench), (vec![3], Some(at(9, 3))));
    let mut label = proton(9, &[(1, &one)]);
    with_label(
        &bench,
        &mut proton(7, &[(1, &one)]),
        &mut label,
        &options(false, false),
    );
    assert_eq!(label.headed, [3], "asked about once more");
    assert_eq!(sales(&bench), (vec![], Some(at(9, 3))));
}

#[test]
fn a_renumbered_label_folder_is_looked_up_whole_and_says_nothing_twice() {
    let bench = bench();
    let one = built(1, "a");
    with_label(
        &bench,
        &mut proton(7, &[(1, &one)]),
        &mut proton(9, &[(1, &one)]),
        &options(false, false),
    );
    let mut renumbered = proton(10, &[(4, &built(1, "b"))]);
    let again = with_label(
        &bench,
        &mut proton(7, &[(1, &one)]),
        &mut renumbered,
        &options(false, false),
    );
    assert_eq!(renumbered.headed, [4]);
    assert_eq!(again.claims, 0, "the label stands already");
    assert_eq!(sales(&bench), (vec![], Some(at(10, 4))));
}

#[test]
fn a_dry_run_of_a_label_folder_writes_nothing() {
    let bench = bench();
    let one = built(1, "a");
    with_label(
        &bench,
        &mut proton(7, &[(1, &one)]),
        &mut proton(9, &[]),
        &options(false, false),
    );
    let (head, point) = (bench.archive.log().head().unwrap().len(), sales(&bench));
    with_label(
        &bench,
        &mut proton(7, &[(1, &one)]),
        &mut proton(9, &[(1, &one), (2, &built(2, "a"))]),
        &options(false, true),
    );
    assert_eq!(bench.archive.log().head().unwrap().len(), head);
    assert_eq!(sales(&bench), point);
}

#[test]
fn full_retracts_a_proton_label_removed_from_a_message_still_in_all_mail() {
    let bench = bench();
    let (one, two) = (built(1, "a"), built(2, "a"));
    let all = || proton(7, &[(1, &one), (2, &two)]);
    with_label(
        &bench,
        &mut all(),
        &mut proton(9, &[(1, &one), (2, &two)]),
        &options(false, false),
    );
    assert_eq!(held(&bench, &two, "mailbox:tag"), ["Sales"]);

    // The label was removed from the second. A normal run does not
    // see it.
    let after = || proton(9, &[(1, &one)]);
    let normal = with_label(&bench, &mut all(), &mut after(), &options(false, false));
    assert_eq!(normal.taken, 0);
    let head = bench.archive.log().head().unwrap().len();
    let dry = with_label(&bench, &mut all(), &mut after(), &options(true, true));
    assert_eq!(
        (dry.taken, bench.archive.log().head().unwrap().len()),
        (0, head),
        "a dry run writes nothing"
    );

    let full = with_label(&bench, &mut all(), &mut after(), &options(true, false));
    assert_eq!(full.taken, 1);
    assert!(held(&bench, &two, "mailbox:tag").is_empty());
    assert_eq!(held(&bench, &one, "mailbox:tag"), ["Sales"]);
}

#[test]
fn a_message_gone_from_the_mailbox_keeps_its_proton_labels_under_full() {
    let bench = bench();
    let (one, two) = (built(1, "a"), built(2, "a"));
    with_label(
        &bench,
        &mut proton(7, &[(1, &one), (2, &two)]),
        &mut proton(9, &[(1, &one), (2, &two)]),
        &options(false, false),
    );
    // Deleted: gone from All Mail and from the label folder.
    let full = with_label(
        &bench,
        &mut proton(7, &[(1, &one)]),
        &mut proton(9, &[(1, &one)]),
        &options(true, false),
    );
    assert_eq!(full.taken, 0);
    assert_eq!(held(&bench, &two, "mailbox:tag"), ["Sales"]);
}

#[test]
fn the_label_folders_wait_when_a_folder_of_the_account_failed() {
    let bench = bench();
    let account: Account = toml::from_str(
        "name = \"example.org\"\nhost = \"127.0.0.1\"\nuser = \"john\"\n\
             folders = [\"All Mail\"]\n",
    )
    .unwrap();
    let folders = ["All Mail".to_string()];
    for (failing, examined) in [
        (false, vec!["All Mail", "Labels/Sales"]),
        (true, vec!["All Mail"]),
    ] {
        let mut bridge = proton(7, &[(1, &built(1, "a"))]);
        bridge.folders = vec!["All Mail".to_string(), "Labels/Sales".to_string()];
        if failing {
            bridge.unopenable.insert("All Mail".to_string());
        }
        let options = options(false, false);
        let fetch = fetcher(&bench, &options);
        let mut state = State::new();
        fetch
            .imap_folders(&account, &mut bridge, &folders, &mut state)
            .unwrap();
        assert_eq!(bridge.examined, examined);
        assert_eq!(state.tally.failed.len(), usize::from(failing));
    }
}
