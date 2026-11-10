//! The Graph side of the fetch, against a stub on loopback.

use serde_json::json;

use super::*;
use crate::graph::stub::{Reply, Stub};

const USER: &str = "john@example.com";

fn m365() -> Account {
    toml::from_str(&format!(
        "name = \"m365\"\nbackend = \"msgraph\"\ntenant_id = \"tenant\"\n\
                 client_id = \"client\"\nclient_secret = \"s3cret\"\nuser = \"{USER}\"\n"
    ))
    .unwrap()
}

fn graph(account: &Account) -> config::Graph {
    match account.reach().unwrap() {
        Reach::Graph(graph) => graph,
        Reach::Imap(_) => unreachable!(),
    }
}

/// The bytes the stub hands out for a message id, and their
/// subject.
fn message(id: &str) -> (Vec<u8>, Subject) {
    let bytes = format!("Subject: {id}\r\n\r\nbody {id}").into_bytes();
    let subject = Subject::parse(Algorithm::Sha256.hash(&bytes).as_str()).unwrap();
    (bytes, subject)
}

/// A tenant issuing tokens and a mailbox of one folder,
/// `Inbox` as `F1`, with these messages in it.
fn stub(ids: &[&str]) -> Stub {
    let stub = Stub::start();
    stub.script(
        "POST /tenant/oauth2/v2.0/token",
        vec![Reply::json(200, &json!({ "access_token": "tok" }))],
    );
    stub.script(
        &format!(
            "GET /v1.0/users/{USER}/mailFolders?$select=id,displayName,childFolderCount&$top=100"
        ),
        vec![Reply::json(
            200,
            &json!({ "value": [{ "id": "F1", "displayName": "Inbox", "childFolderCount": 0 }] }),
        )],
    );
    for id in ids {
        stub.script(
            &format!("GET /v1.0/users/{USER}/messages/{id}/$value"),
            vec![Reply::bytes(200, &message(id).0)],
        );
    }
    stub
}

/// A message as a round lists it, with these marks on it.
fn marked(id: &str, tags: &[&str]) -> serde_json::Value {
    json!({ "id": id, "categories": tags })
}

/// A round over the folder, from scratch when `from` is `None`
/// and from that link otherwise, offers `items` and ends on
/// `link`.
fn offer(stub: &Stub, from: Option<&str>, items: &[serde_json::Value], link: &str) {
    let key = match from {
        Some(from) => format!("GET /v1.0/delta/{from}"),
        None => {
            format!("GET /v1.0/users/{USER}/mailFolders/F1/messages/delta?$select=id,categories")
        }
    };
    stub.script(
                &key,
                vec![Reply::json(
                    200,
                    &json!({ "value": items, "@odata.deltaLink": format!("{}/v1.0/delta/{link}", stub.url()) }),
                )],
            );
}

/// The first round, from scratch, offers `ids` and ends on
/// `link`.
fn first_round(stub: &Stub, ids: &[&str], link: &str) {
    let items: Vec<_> = ids.iter().map(|id| json!({ "id": id })).collect();
    offer(stub, None, &items, link);
}

/// A round from `link` on offers `ids` and ends on `next`.
fn next_round(stub: &Stub, link: &str, ids: &[&str], next: &str) {
    let items: Vec<_> = ids.iter().map(|id| json!({ "id": id })).collect();
    offer(stub, Some(link), &items, next);
}

fn fetch(bench: &Bench, stub: &Stub, account: &Account, full: bool, dry_run: bool) -> Tally {
    let options = options(full, dry_run);
    let fetch = fetcher(bench, &options);
    let graph = graph(account);
    let mut client = Graph::reach(
        &account.name,
        &graph,
        "s3cret".to_string(),
        stub.url(),
        &format!("{}/v1.0", stub.url()),
        Say::new(true),
    )
    .unwrap();
    let mut state = caught(bench);
    fetch
        .graph_folder(account, &mut client, "Inbox", &mut state)
        .unwrap();
    state.tally
}

/// `fetch --compare` of the folder.
fn compare(bench: &Bench, stub: &Stub, account: &Account, dry_run: bool) -> Tally {
    let options = Options {
        full: false,
        compare: true,
        dry_run,
    };
    let fetch = fetcher(bench, &options);
    let graph = graph(account);
    let mut client = Graph::reach(
        &account.name,
        &graph,
        "s3cret".to_string(),
        stub.url(),
        &format!("{}/v1.0", stub.url()),
        Say::new(true),
    )
    .unwrap();
    let mut state = caught(bench);
    fetch
        .graph_folder(account, &mut client, "Inbox", &mut state)
        .unwrap();
    state.tally
}

/// The round `--compare` asks for offers these messages, each
/// with its Message-ID, and ends on `link`.
fn listing(stub: &Stub, items: &[(&str, &str)], link: &str) {
    let items: Vec<_> = items
        .iter()
        .map(|(id, message_id)| json!({ "id": id, "internetMessageId": message_id }))
        .collect();
    stub.script(
                &format!(
                    "GET /v1.0/users/{USER}/mailFolders/F1/messages/delta?$select=id,categories,internetMessageId"
                ),
                vec![Reply::json(
                    200,
                    &json!({ "value": items, "@odata.deltaLink": format!("{}/v1.0/delta/{link}", stub.url()) }),
                )],
            );
}

#[test]
fn compare_lists_the_folder_and_fetches_what_the_archive_lacks() {
    let bench = bench();
    let account = m365();
    let stub = stub(&["M1", "M2"]);
    first_round(&stub, &["M1"], "one");
    fetch(&bench, &stub, &account, false, false);
    super::extracted(&bench, &message("M1").0, "<m1@example.org>");
    listing(
        &stub,
        &[("M1", "<M1@example.org>"), ("M2", "<m2@example.org>")],
        "two",
    );

    let tally = compare(&bench, &stub, &account, false);

    assert_eq!((tally.stored, tally.known, tally.matched), (1, 0, 1));
    assert!(tally.failed.is_empty(), "{:?}", tally.failed);
    assert!(link(&bench).unwrap().ends_with("/delta/two"));
    assert_eq!(
        stub.seen()
            .iter()
            .filter(|seen| seen.target.contains("/M1/"))
            .count(),
        1,
        "M1 was fetched by the first run only"
    );
}

#[test]
fn compare_does_not_fetch_again_a_message_listed_under_another_message_id() {
    let bench = bench();
    let account = m365();
    let stub = stub(&["M1"]);
    first_round(&stub, &["M1"], "one");
    fetch(&bench, &stub, &account, false, false);
    super::extracted(&bench, &message("M1").0, "<m1@example.org>");
    listing(&stub, &[("M1", "<other@example.org>")], "two");
    let fetches = || {
        stub.seen()
            .iter()
            .filter(|seen| seen.target.contains("/M1/"))
            .count()
    };

    let first = compare(&bench, &stub, &account, false);
    let second = compare(&bench, &stub, &account, false);

    assert_eq!((first.known, first.recalled), (1, 0));
    assert_eq!((second.known, second.recalled), (0, 1));
    assert_eq!(fetches(), 2, "by the first fetch and the first compare");
}

fn link(bench: &Bench) -> Option<String> {
    bench
        .memo
        .graph_resume("m365", "Inbox")
        .unwrap()
        .map(|point| point.link)
}

/// The marks standing on a message, as the record has them.
fn standing_tags(bench: &Bench, subject: &Subject) -> Vec<String> {
    let index = caught_up(&bench.archive, Say::new(true)).unwrap();
    index
        .values(subject, &Attribute::parse(place::TAG).unwrap(), Scope::Held)
        .unwrap()
        .into_iter()
        .map(|value| value.as_str().unwrap().to_string())
        .collect()
}

#[test]
fn a_first_round_takes_the_folder_whole_and_the_next_carries_on_from_its_link() {
    let bench = bench();
    let account = m365();
    let stub = stub(&["M1", "M2", "M3"]);
    first_round(&stub, &["M1", "M2"], "one");
    next_round(&stub, "one", &["M3"], "two");
    next_round(&stub, "two", &[], "three");

    let tally = fetch(&bench, &stub, &account, false, false);
    assert_eq!((tally.stored, tally.known, tally.claims), (2, 0, 6));
    assert!(tally.failed.is_empty(), "{:?}", tally.failed);
    assert!(link(&bench).unwrap().ends_with("/delta/one"));

    let again = fetch(&bench, &stub, &account, false, false);
    assert_eq!((again.stored, again.claims), (1, 3), "only what changed");
    assert!(link(&bench).unwrap().ends_with("/delta/two"));

    let quiet = fetch(&bench, &stub, &account, false, false);
    assert_eq!((quiet.stored, quiet.claims), (0, 0));
    assert!(
        link(&bench).unwrap().ends_with("/delta/three"),
        "an unchanged folder still records the server's word"
    );
}

#[test]
fn the_marks_are_said_as_seen_and_taken_back_when_gone() {
    let bench = bench();
    let account = m365();
    let stub = stub(&["M1", "M2"]);
    let (_, m1) = message("M1");
    let (_, m2) = message("M2");
    offer(
        &stub,
        None,
        &[marked("M1", &["Red", "Later"]), marked("M2", &[])],
        "one",
    );

    let tally = fetch(&bench, &stub, &account, false, false);

    assert_eq!(
        (tally.stored, tally.claims, tally.taken),
        (2, 8, 0),
        "place, size, kind each — and two marks on the first"
    );
    assert_eq!(standing_tags(&bench, &m1), ["Later", "Red"]);
    assert!(standing_tags(&bench, &m2).is_empty());

    // Red comes off M1 and goes onto M2: both come round again.
    offer(
        &stub,
        Some("one"),
        &[marked("M1", &["Later"]), marked("M2", &["Red"])],
        "two",
    );
    let again = fetch(&bench, &stub, &account, false, false);

    assert_eq!((again.stored, again.known), (0, 2), "the bytes are held");
    assert_eq!(again.taken, 1, "Red is taken back from M1");
    assert_eq!(
        again.claims, 7,
        "M1: place, kind, Later, and Red taken back; M2: place, kind, Red"
    );
    assert_eq!(standing_tags(&bench, &m1), ["Later"]);
    assert_eq!(standing_tags(&bench, &m2), ["Red"]);

    // Said again unchanged, by a round from scratch: nothing is
    // taken back.
    offer(
        &stub,
        None,
        &[marked("M1", &["Later"]), marked("M2", &["Red"])],
        "three",
    );
    let full = fetch(&bench, &stub, &account, true, false);
    assert_eq!(full.taken, 0);
    assert_eq!(standing_tags(&bench, &m1), ["Later"]);
}

#[test]
fn a_message_met_twice_in_one_run_keeps_what_the_run_said() {
    // The same bytes under two ids — a message in two folders —
    // and the record does not see the run's own claims yet.
    let bench = bench();
    let account = m365();
    let stub = stub(&["M1"]);
    let (bytes, m1) = message("M1");
    stub.script(
        &format!("GET /v1.0/users/{USER}/messages/M1-again/$value"),
        vec![Reply::bytes(200, &bytes)],
    );
    offer(
        &stub,
        None,
        &[marked("M1", &["Red"]), marked("M1-again", &["Red"])],
        "one",
    );

    let tally = fetch(&bench, &stub, &account, false, false);

    assert_eq!(
        tally.taken, 0,
        "the second sighting says Red again, takes nothing back"
    );
    assert_eq!(standing_tags(&bench, &m1), ["Red"]);

    offer(
        &stub,
        Some("one"),
        &[marked("M1", &["Red"]), marked("M1-again", &[])],
        "two",
    );
    let again = fetch(&bench, &stub, &account, false, false);
    assert_eq!(
        again.taken, 1,
        "the second sighting in a run sees what the first just said, and takes it back"
    );
    assert!(standing_tags(&bench, &m1).is_empty());
}

#[test]
fn a_first_round_that_offers_nothing_starts_no_point() {
    let bench = bench();
    let account = m365();
    let stub = stub(&[]);
    first_round(&stub, &[], "one");

    let tally = fetch(&bench, &stub, &account, false, false);

    assert_eq!(tally.stored, 0);
    assert_eq!(
        link(&bench),
        None,
        "an empty folder and a mailbox not answering yet look alike"
    );
}

#[test]
fn a_link_the_server_no_longer_honours_costs_one_whole_round() {
    let bench = bench();
    let account = m365();
    let stub = stub(&["M1", "M2"]);
    first_round(&stub, &["M1"], "one");
    let tally = fetch(&bench, &stub, &account, false, false);
    assert_eq!(tally.stored, 1);

    stub.script(
        "GET /v1.0/delta/one",
        vec![Reply::json(
            410,
            &json!({ "error": { "code": "syncStateNotFound" } }),
        )],
    );
    first_round(&stub, &["M1", "M2"], "fresh");

    let again = fetch(&bench, &stub, &account, false, false);

    assert_eq!(
        (again.stored, again.known),
        (1, 1),
        "the folder whole: the held one dedups, the new one lands"
    );
    assert!(again.failed.is_empty(), "{:?}", again.failed);
    assert!(link(&bench).unwrap().ends_with("/delta/fresh"));
}

#[test]
fn a_whole_round_that_fails_after_an_expired_link_is_named_with_its_cause() {
    let bench = bench();
    let account = m365();
    let stub = stub(&["M1"]);
    first_round(&stub, &["M1"], "one");
    fetch(&bench, &stub, &account, false, false);

    stub.script(
        "GET /v1.0/delta/one",
        vec![Reply::json(
            410,
            &json!({ "error": { "code": "syncStateNotFound" } }),
        )],
    );
    stub.script(
        &format!("GET /v1.0/users/{USER}/mailFolders/F1/messages/delta?$select=id,categories"),
        vec![Reply::json(
            403,
            &json!({ "error": { "code": "ErrorAccessDenied", "message": "denied" } }),
        )],
    );

    let again = fetch(&bench, &stub, &account, false, false);

    assert_eq!(again.failed.len(), 1, "{:?}", again.failed);
    assert!(
        again.failed[0].contains("HTTP 403 ErrorAccessDenied: denied"),
        "{:?}",
        again.failed
    );
    assert!(link(&bench).unwrap().ends_with("/delta/one"));
}

/// The list pending for `Inbox`, in listing order.
fn pending_list(bench: &Bench) -> Vec<Pending<Vec<String>>> {
    bench.memo.pending("m365", "Inbox").unwrap()
}

/// The ids pending for `Inbox`, in listing order.
fn pending(bench: &Bench) -> Vec<String> {
    pending_list(bench)
        .into_iter()
        .map(|message| message.id)
        .collect()
}

/// A pending entry as the memo hands it back.
fn entry(id: &str, tags: &[&str], attempts: u32) -> Pending<Vec<String>> {
    Pending {
        id: id.to_string(),
        detail: tags.iter().map(ToString::to_string).collect(),
        attempts,
    }
}

/// The server refuses `id` with a status that is not retried.
fn refuse(stub: &Stub, id: &str) {
    stub.script(
        &format!("GET /v1.0/users/{USER}/messages/{id}/$value"),
        vec![Reply::json(
            403,
            &json!({ "error": { "code": "ErrorAccessDenied", "message": "denied" } }),
        )],
    );
}

#[test]
fn a_message_the_server_keeps_stays_pending_under_the_saved_link() {
    let bench = bench();
    let account = m365();
    let stub = stub(&["M1", "M3"]);
    offer(
        &stub,
        None,
        &[marked("M1", &[]), marked("M2", &["Red"]), marked("M3", &[])],
        "one",
    );
    refuse(&stub, "M2");

    let tally = fetch(&bench, &stub, &account, false, false);

    assert_eq!(tally.stored, 2, "the two handed over are in");
    assert_eq!(tally.failed.len(), 1);
    assert!(
        tally.failed[0].starts_with("m365:Inbox: message M2: HTTP 403"),
        "{:?}",
        tally.failed
    );
    assert!(
        link(&bench).unwrap().ends_with("/delta/one"),
        "the listing was complete, so the link is saved"
    );
    assert_eq!(pending(&bench), ["M2"]);

    // Next run: the server hands M2 over now, the round from
    // the link offers nothing new.
    let (_, m2) = message("M2");
    stub.script(
        &format!("GET /v1.0/users/{USER}/messages/M2/$value"),
        vec![Reply::bytes(200, &message("M2").0)],
    );
    next_round(&stub, "one", &[], "two");
    let again = fetch(&bench, &stub, &account, false, false);

    assert_eq!((again.stored, again.known), (1, 0), "only M2 is fetched");
    assert!(again.failed.is_empty(), "{:?}", again.failed);
    assert_eq!(
        standing_tags(&bench, &m2),
        ["Red"],
        "with the marks the listing reported"
    );
    assert!(pending(&bench).is_empty());
    assert!(link(&bench).unwrap().ends_with("/delta/two"));
    let asked = stub
        .seen()
        .iter()
        .filter(|seen| seen.target.ends_with("/$value"))
        .count();
    assert_eq!(asked, 4, "M1, M2, M3, then M2 again; nothing else");
}

#[test]
fn a_message_gone_since_the_listing_is_skipped_not_kept_pending() {
    let bench = bench();
    let account = m365();
    let stub = stub(&["M1"]);
    first_round(&stub, &["M1", "M2"], "one");
    stub.script(
        &format!("GET /v1.0/users/{USER}/messages/M2/$value"),
        vec![Reply::json(
            404,
            &json!({ "error": { "code": "ErrorItemNotFound", "message": "gone" } }),
        )],
    );

    let tally = fetch(&bench, &stub, &account, false, false);

    assert_eq!(tally.stored, 1);
    assert!(tally.failed.is_empty(), "{:?}", tally.failed);
    assert!(pending(&bench).is_empty());
    assert!(link(&bench).unwrap().ends_with("/delta/one"));
}

#[test]
fn a_404_without_the_gone_code_keeps_the_message_pending() {
    let bench = bench();
    let account = m365();
    let stub = stub(&["M1"]);
    first_round(&stub, &["M1", "M2"], "one");
    stub.script(
                &format!("GET /v1.0/users/{USER}/messages/M2/$value"),
                vec![Reply::json(
                    404,
                    &json!({ "error": { "code": "ResourceNotFound", "message": "Resource could not be discovered." } }),
                )],
            );

    let tally = fetch(&bench, &stub, &account, false, false);

    assert_eq!(tally.stored, 1);
    assert_eq!(tally.failed.len(), 1);
    assert!(
        tally.failed[0].starts_with("m365:Inbox: message M2: HTTP 404 ResourceNotFound"),
        "{:?}",
        tally.failed
    );
    assert_eq!(
        pending(&bench),
        ["M2"],
        "not gone, so not taken off the list"
    );
    assert!(link(&bench).unwrap().ends_with("/delta/one"));
}

#[test]
fn a_round_that_lists_a_pending_message_again_renews_its_marks() {
    let bench = bench();
    let account = m365();
    let stub = stub(&["M1"]);
    offer(
        &stub,
        None,
        &[marked("M1", &[]), marked("M2", &["Red"])],
        "one",
    );
    refuse(&stub, "M2");
    fetch(&bench, &stub, &account, false, false);
    assert_eq!(pending(&bench), ["M2"]);

    offer(&stub, Some("one"), &[marked("M2", &["Blue"])], "two");
    fetch(&bench, &stub, &account, false, false);

    assert_eq!(
        pending_list(&bench),
        [entry("M2", &["Blue"], 2)],
        "listed once, with the newer marks, and the refusals of both runs counted"
    );
}

#[test]
fn a_message_refused_in_three_runs_is_given_up() {
    let bench = bench();
    let account = m365();
    let stub = stub(&["M1"]);
    first_round(&stub, &["M1", "M2"], "one");
    refuse(&stub, "M2");
    next_round(&stub, "one", &[], "one");

    fetch(&bench, &stub, &account, false, false);
    assert_eq!(pending_list(&bench), [entry("M2", &[], 1)]);
    let second = fetch(&bench, &stub, &account, false, false);
    assert_eq!(pending_list(&bench), [entry("M2", &[], 2)]);
    assert_eq!(
        second.failed,
        ["m365:Inbox: message M2: HTTP 403 ErrorAccessDenied: denied"]
    );

    let third = fetch(&bench, &stub, &account, false, false);

    assert!(pending(&bench).is_empty(), "given up: off the list");
    assert_eq!(
        third.failed,
        [
            "m365:Inbox: message M2: HTTP 403 ErrorAccessDenied: denied; given up after 3 runs, --full fetches it again"
        ]
    );

    // --full lists it again, and it gets a fresh count.
    first_round(&stub, &["M1", "M2"], "fresh");
    let full = fetch(&bench, &stub, &account, true, false);
    assert_eq!(full.failed.len(), 1);
    assert_eq!(pending_list(&bench), [entry("M2", &[], 1)]);
}

#[test]
fn ten_refusals_in_a_row_leave_the_rest_of_the_folder_for_the_next_run() {
    let bench = bench();
    let account = m365();
    let ids: Vec<String> = (1..=12).map(|n| format!("M{n}")).collect();
    let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
    let stub = stub(&[]);
    first_round(&stub, &ids, "one");
    for id in &ids {
        refuse(&stub, id);
    }

    let tally = fetch(&bench, &stub, &account, false, false);

    assert_eq!(
        tally.failed.len(),
        11,
        "ten refusals and the stop: {:?}",
        tally.failed
    );
    assert_eq!(
        tally.failed[10],
        "m365:Inbox: 10 messages in a row refused; 2 messages left for the next run"
    );
    let list = pending_list(&bench);
    assert_eq!(list.len(), 12, "all still pending");
    assert_eq!(
        list.iter().filter(|message| message.attempts == 1).count(),
        10,
        "the ten refused count a run, the two not asked do not"
    );
    let asked = stub
        .seen()
        .iter()
        .filter(|seen| seen.target.ends_with("/$value"))
        .count();
    assert_eq!(asked, 10, "M11 and M12 are not asked for");
}

#[test]
fn a_server_that_stops_answering_leaves_the_rest_pending_uncounted() {
    let bench = bench();
    let account = m365();
    let stub = stub(&["M1", "M3"]);
    first_round(&stub, &["M1", "M2", "M3"], "one");
    // M2 answers 401, and the tenant will not renew the token:
    // nothing after M2 can be asked for.
    stub.script(
        &format!("GET /v1.0/users/{USER}/messages/M2/$value"),
        vec![Reply::bytes(401, b"")],
    );
    stub.script(
        "POST /tenant/oauth2/v2.0/token",
        vec![
            Reply::json(200, &json!({ "access_token": "tok" })),
            Reply::json(401, &json!({ "error": "invalid_client" })),
        ],
    );

    let tally = fetch(&bench, &stub, &account, false, false);

    assert_eq!(tally.stored, 1, "M1 is in");
    assert_eq!(tally.failed.len(), 1, "{:?}", tally.failed);
    assert!(
        tally.failed[0].ends_with("; 2 messages left for the next run"),
        "{:?}",
        tally.failed
    );
    assert_eq!(
        pending_list(&bench),
        [entry("M2", &[], 0), entry("M3", &[], 0)],
        "neither counts a failed run: the server was gone, not the message"
    );
    assert!(link(&bench).unwrap().ends_with("/delta/one"));
    assert!(
        stub.seen().iter().all(|seen| !seen.target.contains("/M3/")),
        "M3 is not asked for"
    );
}

#[test]
fn a_round_from_scratch_replaces_the_pending_list() {
    let bench = bench();
    let account = m365();
    let stub = stub(&["M1"]);
    first_round(&stub, &["M1", "M2"], "one");
    refuse(&stub, "M2");
    fetch(&bench, &stub, &account, false, false);
    assert_eq!(pending(&bench), ["M2"]);

    // M2 is gone from the mailbox; --full lists the folder
    // whole and M2 is not asked for again.
    first_round(&stub, &["M1"], "fresh");
    let full = fetch(&bench, &stub, &account, true, false);

    assert_eq!((full.stored, full.known), (0, 1));
    assert!(full.failed.is_empty(), "{:?}", full.failed);
    assert!(pending(&bench).is_empty());
    let asked = stub
        .seen()
        .iter()
        .filter(|seen| seen.target.ends_with("/M2/$value"))
        .count();
    assert_eq!(asked, 1, "only in the first run");
}

#[test]
fn a_dry_run_counts_what_is_pending_too() {
    let bench = bench();
    let account = m365();
    let stub = stub(&["M1"]);
    first_round(&stub, &["M1", "M2"], "one");
    refuse(&stub, "M2");
    fetch(&bench, &stub, &account, false, false);
    next_round(&stub, "one", &["M2", "M3"], "two");

    let rehearsed = fetch(&bench, &stub, &account, false, true);

    assert_eq!(
        (rehearsed.would, rehearsed.claims),
        (2, 0),
        "M2 once, though pending and listed again, and M3"
    );
    assert_eq!(pending(&bench), ["M2"], "a rehearsal moves nothing");
    assert!(link(&bench).unwrap().ends_with("/delta/one"));
}

#[test]
fn full_rounds_the_folder_whole_and_a_dry_run_counts() {
    let bench = bench();
    let account = m365();
    let stub = stub(&["M1", "M2"]);
    first_round(&stub, &["M1", "M2"], "one");
    next_round(&stub, "one", &[], "two");
    fetch(&bench, &stub, &account, false, false);

    let rehearsed = fetch(&bench, &stub, &account, true, true);
    assert_eq!((rehearsed.would, rehearsed.claims), (2, 0));
    assert!(
        link(&bench).unwrap().ends_with("/delta/one"),
        "a rehearsal moves nothing"
    );

    let full = fetch(&bench, &stub, &account, true, false);
    assert_eq!(
        (full.stored, full.known),
        (0, 2),
        "the bytes are held, the place is said again"
    );
    assert!(
        link(&bench).unwrap().ends_with("/delta/one"),
        "the round's own link"
    );
}

#[test]
fn a_folder_the_mailbox_does_not_have_is_named_with_what_it_has() {
    let bench = bench();
    let account = m365();
    let stub = stub(&[]);
    let options = options(false, false);
    let fetch = fetcher(&bench, &options);
    let graph = graph(&account);
    let mut client = Graph::reach(
        &account.name,
        &graph,
        "s3cret".to_string(),
        stub.url(),
        &format!("{}/v1.0", stub.url()),
        Say::new(true),
    )
    .unwrap();
    let mut state = caught(&bench);
    fetch
        .graph_folder(&account, &mut client, "Drafts", &mut state)
        .unwrap();
    assert_eq!(
        state.tally.failed,
        ["m365:Drafts: no such folder; the mailbox has Inbox"]
    );
}

#[test]
fn a_point_naming_another_host_is_worth_as_much_as_none() {
    let bench = bench();
    let account = m365();
    let stub = stub(&["M1"]);
    first_round(&stub, &["M1"], "one");
    bench
        .memo
        .advance_graph("m365", "Inbox", "https://evil.example.com/delta")
        .unwrap();

    let tally = fetch(&bench, &stub, &account, false, false);

    assert_eq!(
        tally.stored, 1,
        "the folder whole, the foreign link never followed"
    );
    assert!(
        stub.seen().iter().all(|seen| !seen.target.contains("evil")),
        "nothing went anywhere else"
    );
    assert!(link(&bench).unwrap().ends_with("/delta/one"));
}
