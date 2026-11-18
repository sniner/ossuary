use immure::Store;
use serde_json::json;
use tempfile::TempDir;

use super::*;
use crate::claim::{Attribute, Claim, Run, Source, Subject, Timestamp, Value};
use crate::error::Error;
use crate::log::Log;
use crate::part::Part;
use rusqlite::Connection;

fn log_in(dir: &TempDir) -> Log {
    let store = Store::builder(dir.path().join("claims"))
        .suffix(".seg")
        .depth(1)
        .create()
        .unwrap();
    crate::log::test_log(store, dir.path())
}

fn index_in(dir: &TempDir) -> Index {
    let cache = dir.path().join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    Index::open(cache.join("index.sqlite")).unwrap()
}

fn subject() -> Subject {
    Subject::parse("9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e").unwrap()
}

fn tag(tag: &str, time: &str) -> Claim {
    tag_about(subject(), tag, time)
}

fn tag_about(subject: Subject, tag: &str, time: &str) -> Claim {
    Claim::assert(
        subject,
        Attribute::parse("user:tag").unwrap(),
        json!(tag),
        Timestamp::parse(time).unwrap(),
        Source::parse("user").unwrap(),
        Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
    )
    .unwrap()
}

#[test]
fn a_fold_is_incremental_because_segments_never_change() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);

    log.append(&tag("holiday", "2026-09-01T21:14:03Z")).unwrap();
    log.seal().unwrap().unwrap();
    log.append(&tag("beach", "2026-09-01T21:14:04Z")).unwrap();

    let first = index.fold(&log).unwrap();
    assert_eq!(
        first,
        Folded {
            segments: 1,
            claims: 1,
            head: 1,
            rebuilt: false
        }
    );

    let again = index.fold(&log).unwrap();
    assert_eq!(
        again,
        Folded::default(),
        "nothing appended or sealed, nothing read"
    );
}

fn rows(index: &Index) -> i64 {
    index
        .connection
        .query_row("SELECT COUNT(*) FROM claims", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn a_fold_reads_only_what_was_appended_to_the_head() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    log.append(&tag("holiday", "2026-09-01T00:00:00Z")).unwrap();
    index.fold(&log).unwrap();

    log.append(&tag("beach", "2026-09-02T00:00:00Z")).unwrap();
    let folded = index.fold(&log).unwrap();

    assert_eq!(folded.head, 1, "only the claim appended since");
    assert_eq!(rows(&index), 2, "no claim held twice");
    assert_eq!(tagged(&index, "holiday"), [subject()]);
    assert_eq!(tagged(&index, "beach"), [subject()]);
}

#[test]
fn a_sealed_head_keeps_its_rows_and_brings_only_what_came_after_them() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    log.append(&tag("one", "2026-09-01T00:00:00Z")).unwrap();
    index.fold(&log).unwrap();
    log.append(&tag("two", "2026-09-02T00:00:00Z")).unwrap();
    log.seal().unwrap().unwrap();
    log.append(&tag("three", "2026-09-03T00:00:00Z")).unwrap();
    log.seal().unwrap().unwrap();
    log.append(&tag("four", "2026-09-04T00:00:00Z")).unwrap();

    let folded = index.fold(&log).unwrap();

    assert_eq!(
        folded,
        Folded {
            segments: 2,
            claims: 2,
            head: 1,
            rebuilt: false
        },
        "the first claim was in the index already"
    );
    assert_eq!(rows(&index), 4);
    let order: Vec<_> = index
        .about(&subject())
        .unwrap()
        .iter()
        .map(|claim| claim.value().cloned())
        .collect();
    assert_eq!(
        order,
        [
            Some(json!("one")),
            Some(json!("two")),
            Some(json!("three")),
            Some(json!("four"))
        ],
        "log order"
    );
    let fresh_dir = TempDir::new().unwrap();
    let mut fresh = index_in(&fresh_dir);
    fresh.fold(&log).unwrap();
    let positions = |index: &Index| -> Vec<(i64, i64)> {
        index
            .connection
            .prepare("SELECT seq, position FROM v_claims ORDER BY seq, position")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(std::result::Result::unwrap)
            .collect()
    };
    assert_eq!(
        positions(&index),
        positions(&fresh),
        "as a fresh fold has them"
    );
}

/// Claims in the index but no record of what it read of the head, as
/// in an index that another build wrote: it is built anew.
#[test]
fn an_index_without_a_record_of_the_head_is_built_anew() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    log.append(&tag("holiday", "2026-09-01T00:00:00Z")).unwrap();
    index.fold(&log).unwrap();
    index
        .connection
        .execute("DELETE FROM open_read", [])
        .unwrap();
    log.append(&tag("beach", "2026-09-02T00:00:00Z")).unwrap();

    let folded = index.fold(&log).unwrap();

    assert!(folded.rebuilt);
    assert_eq!(folded.head, 2, "the whole head, read anew");
    assert_eq!(rows(&index), 2);
}

fn untag(log: &Log, tag: &str, time: &str) -> Claim {
    unsay(
        log,
        &subject(),
        "user:tag",
        Some(&json!(tag)),
        time,
        "user",
        Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
    )
}

/// A retraction of what stands in `log` for `attribute` on `subject`:
/// of `value`, or of every value, named by id the way a writer does
/// after looking the claims up.
fn unsay(
    log: &Log,
    subject: &Subject,
    attribute: &str,
    value: Option<&serde_json::Value>,
    time: &str,
    source: &str,
    run: Run,
) -> Claim {
    let mut claims = Vec::new();
    for segment in log.segments().unwrap() {
        claims.extend(log.read(segment.digest()).unwrap());
    }
    claims.extend(log.head().unwrap());
    let attribute = Attribute::parse(attribute).unwrap();
    let ids = crate::claim::standing_ids_in(&claims, subject, &attribute, value);
    Claim::retract(
        subject.clone(),
        attribute,
        ids,
        Timestamp::parse(time).unwrap(),
        Source::parse(source).unwrap(),
        run,
    )
    .unwrap()
}

fn tagged(index: &Index, tag: &str) -> Vec<Subject> {
    index
        .find(&[term("user:tag", tag)], &[], Scope::Held)
        .unwrap()
}

/// Review 0.14.0, B1: the fold went by the time of each segment's
/// first claim, so a segment of older timestamps sealed later, as a
/// mail import writes one, sorted in front of segments already folded,
/// and its retraction was undone. The order is the log's.
#[test]
fn a_segment_of_older_timestamps_counts_after_what_came_before_it() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    log.append(&tag("holiday", "2026-09-05T00:00:00Z")).unwrap();
    log.seal().unwrap().unwrap();
    index.fold(&log).unwrap();
    assert_eq!(tagged(&index, "holiday"), [subject()]);

    log.append(&untag(&log, "holiday", "2026-09-01T00:00:00Z"))
        .unwrap();
    log.seal().unwrap().unwrap();
    index.fold(&log).unwrap();

    assert_eq!(
        tagged(&index, "holiday"),
        [],
        "the retraction stands later in the log, whatever its timestamp"
    );
    let fresh_dir = TempDir::new().unwrap();
    let mut fresh = index_in(&fresh_dir);
    fresh.fold(&log).unwrap();
    assert_eq!(tagged(&fresh, "holiday"), [], "a fresh fold agrees");
    assert_eq!(
        fresh
            .about(&subject())
            .unwrap()
            .iter()
            .map(Claim::is_retraction)
            .collect::<Vec<_>>(),
        [false, true],
        "about answers in log order too"
    );
}

#[test]
fn a_fold_reads_only_the_segments_sealed_since() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    log.append(&tag("one", "2026-09-01T00:00:00Z")).unwrap();
    log.seal().unwrap().unwrap();
    log.append(&tag("two", "2026-09-02T00:00:00Z")).unwrap();
    log.seal().unwrap().unwrap();
    assert_eq!(index.fold(&log).unwrap().segments, 2);

    log.append(&tag("three", "2026-09-03T00:00:00Z")).unwrap();
    log.seal().unwrap().unwrap();

    assert_eq!(index.fold(&log).unwrap().segments, 1);
    assert_eq!(tagged(&index, "one"), [subject()]);
    assert_eq!(tagged(&index, "three"), [subject()]);
}

#[test]
fn an_index_folded_from_another_chain_is_folded_anew() {
    let one_dir = TempDir::new().unwrap();
    let one = log_in(&one_dir);
    one.append(&tag("ours", "2026-09-01T00:00:00Z")).unwrap();
    one.seal().unwrap().unwrap();
    let other_dir = TempDir::new().unwrap();
    let other = log_in(&other_dir);
    other
        .append(&tag("theirs", "2026-09-01T00:00:00Z"))
        .unwrap();
    other.seal().unwrap().unwrap();
    let mut index = index_in(&one_dir);
    index.fold(&one).unwrap();

    index.fold(&other).unwrap();

    assert_eq!(
        tagged(&index, "ours"),
        [],
        "nothing of the other chain stays"
    );
    assert_eq!(tagged(&index, "theirs"), [subject()]);
}

#[test]
fn an_index_that_read_a_bundle_the_open_segment_no_longer_has_is_folded_anew() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    log.append(&tag("stays", "2026-09-01T00:00:00Z")).unwrap();
    log.append(&tag("goes", "2026-09-02T00:00:00Z")).unwrap();
    index.fold(&log).unwrap();
    assert_eq!(tagged(&index, "goes"), [subject()]);
    // The open segment of a copy that is one append behind: the
    // same segment before it, one bundle fewer.
    let open = log.open().unwrap().unwrap();
    let gone = open
        .bundles()
        .iter()
        .find(|bundle| {
            let claims = log.bundle(&open, bundle).unwrap().unwrap();
            claims[0].value() == Some(&json!("goes"))
        })
        .unwrap();
    std::fs::remove_file(
        dir.path()
            .join("open")
            .join("first")
            .join(format!("{gone}.jsonl")),
    )
    .unwrap();

    let folded = index.fold(&log).unwrap();

    assert!(folded.rebuilt);
    assert_eq!(
        tagged(&index, "goes"),
        [],
        "nothing of the other copy stays"
    );
    assert_eq!(tagged(&index, "stays"), [subject()]);
}

#[test]
fn an_index_outside_the_archive_is_used_only_with_a_segment_to_tell_by() {
    let one_dir = TempDir::new().unwrap();
    let one = log_in(&one_dir);
    one.append(&tag("ours", "2026-09-01T00:00:00Z")).unwrap();
    let other_dir = TempDir::new().unwrap();
    let other = log_in(&other_dir);
    other
        .append(&tag("theirs", "2026-09-01T00:00:00Z"))
        .unwrap();
    let outside = TempDir::new().unwrap();
    let path = outside.path().join("index.sqlite");

    // Neither log has a sealed segment: nothing shows whose claims
    // the file holds, so each first fold begins anew.
    let mut index = Index::open_outside(&path).unwrap();
    index.fold(&one).unwrap();
    assert!(
        !index.fold(&one).unwrap().rebuilt,
        "one instance folds on from what it folded itself"
    );
    drop(index);
    let mut index = Index::open_outside(&path).unwrap();
    let folded = index.fold(&other).unwrap();
    assert!(folded.rebuilt);
    assert_eq!(tagged(&index, "ours"), [], "nothing of the other log stays");
    assert_eq!(tagged(&index, "theirs"), [subject()]);
    drop(index);

    // With a sealed segment the index follows it, and the next
    // instance folds on.
    other.seal().unwrap().unwrap();
    let mut index = Index::open_outside(&path).unwrap();
    assert!(
        index.fold(&other).unwrap().rebuilt,
        "still nothing to tell by"
    );
    drop(index);
    other.append(&tag("later", "2026-09-02T00:00:00Z")).unwrap();
    let mut index = Index::open_outside(&path).unwrap();
    let folded = index.fold(&other).unwrap();
    assert!(!folded.rebuilt);
    assert_eq!(folded.head, 1, "only what was appended since");
}

#[test]
fn about_answers_in_log_order_with_the_head_last() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);

    log.append(&tag("holiday", "2026-09-01T21:14:03Z")).unwrap();
    log.seal().unwrap().unwrap();
    log.append(&tag("beach", "2026-09-01T21:14:03Z")).unwrap();
    index.fold(&log).unwrap();

    let claims = index.about(&subject()).unwrap();
    assert_eq!(
        claims
            .iter()
            .map(|claim| claim.value().unwrap().as_str().unwrap())
            .collect::<Vec<_>>(),
        ["holiday", "beach"],
        "same second, and the sealed segment still comes before the head"
    );
}

#[test]
fn retractions_come_back_as_the_claims_they_are() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);

    log.append(&tag("holiday", "2026-09-01T21:14:03Z")).unwrap();
    log.append(&unsay(
        &log,
        &subject(),
        "user:tag",
        Some(&json!("holiday")),
        "2030-04-01T10:00:00Z",
        "user",
        Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
    ))
    .unwrap();
    index.fold(&log).unwrap();

    let claims = index.about(&subject()).unwrap();
    assert_eq!(claims.len(), 2, "nothing is interpreted away");
    assert!(claims[1].is_retraction());
    assert_eq!(claims[1].value(), None);
    assert_eq!(
        claims[1].retracts(),
        &[claims[0].id().unwrap().clone()],
        "and it names the claim it retracts"
    );
}

#[test]
fn a_beginning_names_the_subjects_it_begins() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    let near =
        Subject::parse("9f2ac41edd00000000000000000000000000000000000000000000000000ffff").unwrap();
    log.append(&tag("holiday", "2026-09-01T21:14:03Z")).unwrap();
    log.seal().unwrap().unwrap();
    log.append(&tag_about(near.clone(), "beach", "2026-09-01T21:14:04Z"))
        .unwrap();
    index.fold(&log).unwrap();

    assert_eq!(
        index.matching("9f2ac41e").unwrap(),
        [subject(), near.clone()],
        "sealed or still in the head, each once, sorted"
    );
    assert_eq!(
        index.matching("9F2AC41ED").unwrap(),
        [near],
        "case falls away, the way subjects themselves are spelled"
    );
    assert_eq!(index.matching("ffff").unwrap(), Vec::new());
    assert_eq!(
        index.matching("9f2ac41e%").unwrap(),
        Vec::new(),
        "a wildcard is a character, and no digest contains one"
    );
}

fn say(subject: &Subject, attribute: &str, value: serde_json::Value, time: &str) -> Claim {
    Claim::assert(
        subject.clone(),
        Attribute::parse(attribute).unwrap(),
        value,
        Timestamp::parse(time).unwrap(),
        Source::parse("user").unwrap(),
        Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
    )
    .unwrap()
}

fn term(attribute: &str, pattern: &str) -> Term {
    Term::Attribute(Attribute::parse(attribute).unwrap(), pattern.to_string())
}

/// A run id per letter, so a test can tell its runs apart.
fn run_x(letter: char) -> Run {
    Run::parse(&format!("315e360b-020e-48be-8f2d-f2002a2ea9b{letter}")).unwrap()
}

/// One claim with its run chosen, the way a writer of this version
/// writes one.
fn said_in(subject: &Subject, attribute: &str, value: Value, time: &str, run: &Run) -> Claim {
    Claim::assert(
        subject.clone(),
        Attribute::parse(attribute).unwrap(),
        value,
        Timestamp::parse(time).unwrap(),
        Source::parse("ingest").unwrap(),
        run.clone(),
    )
    .unwrap()
}

fn field(field: Field, pattern: &str) -> Term {
    Term::Field(field, pattern.to_string())
}

#[test]
#[allow(clippy::too_many_lines, reason = "one scene, asked seven ways")]
fn a_field_term_asks_about_the_claim_behind_a_value() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    let (a, b) = (subject(), other());
    // Run a names file a; run b names file b and tags file a.
    log.append(&said_in(
        &a,
        "file:name",
        json!("abc"),
        "2026-09-01T10:00:00Z",
        &run_x('a'),
    ))
    .unwrap();
    log.append(&said_in(
        &b,
        "file:name",
        json!("abc"),
        "2026-09-02T10:00:00Z",
        &run_x('b'),
    ))
    .unwrap();
    log.append(&said_in(
        &a,
        "user:tag",
        json!("beach"),
        "2026-09-02T10:00:00Z",
        &run_x('b'),
    ))
    .unwrap();
    index.fold(&log).unwrap();

    let name = || term("file:name", "abc");
    assert_eq!(
        index
            .find(
                &[field(Field::Run, run_x('a').as_str()), name()],
                &[],
                Scope::Held
            )
            .unwrap(),
        std::slice::from_ref(&a),
        "the name claim itself must come from the run"
    );
    assert_eq!(
        index
            .find(
                &[field(Field::Run, run_x('b').as_str()), name()],
                &[],
                Scope::Held
            )
            .unwrap(),
        std::slice::from_ref(&b),
        "file a was touched by run b, but its name was not said there"
    );
    assert_eq!(
        index
            .find(&[field(Field::Run, run_x('b').as_str())], &[], Scope::Held)
            .unwrap(),
        [a.clone(), b.clone()],
        "a field term alone: every file with a standing value from that run"
    );
    assert_eq!(
        index
            .find(&[field(Field::Time, "2026-09-02..")], &[], Scope::Held)
            .unwrap(),
        [a.clone(), b.clone()],
        "time ranges read like attribute ranges, in claim time's spelling"
    );
    assert_eq!(
        index
            .find(
                &[field(Field::Time, "..2026-09-01T23:59:59Z"), name()],
                &[],
                Scope::Held
            )
            .unwrap(),
        std::slice::from_ref(&a)
    );
    assert_eq!(
        index
            .find(&[field(Field::Source, "ing*")], &[], Scope::Held)
            .unwrap(),
        [a.clone(), b.clone()]
    );
    assert_eq!(
        index
            .find(
                &[field(Field::Time, "..2026-09-01"), name()],
                &[],
                Scope::Held
            )
            .unwrap(),
        std::slice::from_ref(&a),
        "a range up to a date closes with the day, as --as-of does"
    );
    assert_eq!(
        index
            .find(&[field(Field::Time, "2026-09-02")], &[], Scope::Held)
            .unwrap(),
        [a.clone(), b.clone()],
        "a date alone is the whole day"
    );
    assert_eq!(
        index
            .find(
                &[field(Field::Time, "2026-09-01T10:00:00")],
                &[],
                Scope::Held
            )
            .unwrap(),
        std::slice::from_ref(&a),
        "a moment without its Z is the moment"
    );
    assert!(
        matches!(
            index.find(&[field(Field::Time, "..yesterday")], &[], Scope::Held),
            Err(Error::Timestamp(given)) if given == "yesterday"
        ),
        "a bound that names no moment is refused, not compared as text"
    );
    assert_eq!(
        index
            .find(&[field(Field::Subject, "aa*"), name()], &[], Scope::Held)
            .unwrap(),
        std::slice::from_ref(&b),
        "subject is a field like any other, a beginning globbed"
    );
    assert_eq!(
        index.field_values(&a, Field::Run, Scope::Held).unwrap(),
        [json!(run_x('a').as_str()), json!(run_x('b').as_str())],
        "a bare field shows every run that stands behind a value"
    );
    assert!(matches!(
        index.find(&[field(Field::Retract, "maybe")], &[], Scope::Record),
        Err(Error::Retract(_))
    ));
}

#[test]
fn a_value_said_again_answers_for_the_run_that_said_it_last() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    log.append(&said_in(
        &subject(),
        "file:name",
        json!("abc"),
        "2026-09-01T10:00:00Z",
        &run_x('a'),
    ))
    .unwrap();
    log.append(&said_in(
        &subject(),
        "file:name",
        json!("abc"),
        "2026-09-02T10:00:00Z",
        &run_x('b'),
    ))
    .unwrap();
    index.fold(&log).unwrap();

    let name = term("file:name", "abc");
    assert_eq!(
        index
            .find(
                &[field(Field::Run, run_x('a').as_str()), name.clone()],
                &[],
                Scope::Held
            )
            .unwrap(),
        [],
        "the standing row points at the newest sayer"
    );
    assert_eq!(
        index
            .find(
                &[field(Field::Run, run_x('a').as_str()), name],
                &[],
                Scope::Record
            )
            .unwrap(),
        [subject()],
        "the record still knows who said it first"
    );
}

/// Of two assertions in one second the one with the larger id
/// answers, whichever the index read first, so an index folded on
/// and one built anew name the same claim.
#[test]
fn two_assertions_in_one_second_answer_alike_however_the_index_read_them() {
    let moment = "2026-09-01T10:00:00Z";
    let name = Attribute::parse("file:name").unwrap();
    for (first, second) in [('a', 'b'), ('b', 'a')] {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        for run in [first, second] {
            log.append(&said_in(
                &subject(),
                "file:name",
                json!("abc"),
                moment,
                &run_x(run),
            ))
            .unwrap();
            index.fold(&log).unwrap();
        }
        log.seal().unwrap().unwrap();
        index.fold(&log).unwrap();
        let fresh_dir = TempDir::new().unwrap();
        let mut fresh = index_in(&fresh_dir);
        fresh.fold(&log).unwrap();

        let answering = |index: &Index| {
            index.standing_as_of("file:name", None).unwrap()[0]
                .claim
                .clone()
        };
        let largest = index
            .standing_ids(&subject(), &name, None)
            .unwrap()
            .into_iter()
            .max()
            .unwrap();
        assert_eq!(answering(&index), largest, "run {first} read first");
        assert_eq!(answering(&fresh), largest, "and in an index built anew");
        assert_eq!(
            index.standing_as_of("file:name", Some(moment)).unwrap()[0].claim,
            largest,
            "and in a replay"
        );
    }
}

#[test]
fn a_retraction_with_another_subject_or_attribute_retracts_nothing() {
    let later = "2026-09-03T00:00:00Z";
    let assertion = tag("holiday", "2026-09-01T00:00:00Z").identified(immure::Algorithm::Sha256);
    let naming = |subject: Subject, attribute: &str| {
        Claim::retract(
            subject,
            Attribute::parse(attribute).unwrap(),
            vec![assertion.id().unwrap().clone()],
            Timestamp::parse("2026-09-02T00:00:00Z").unwrap(),
            Source::parse("user").unwrap(),
            Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
        )
        .unwrap()
    };
    let strangers = [naming(other(), "user:tag"), naming(subject(), "file:name")];
    // The index may read a retraction before or after the claim it
    // names; it retracts nothing either way.
    for retractions_first in [false, true] {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        if retractions_first {
            log.append_all(&strangers).unwrap();
            index.fold(&log).unwrap();
            log.append(&assertion).unwrap();
        } else {
            log.append(&assertion).unwrap();
            index.fold(&log).unwrap();
            log.append_all(&strangers).unwrap();
        }
        index.fold(&log).unwrap();

        assert_eq!(tagged(&index, "holiday"), [subject()]);
        assert_eq!(
            index.standing_as_of("user:tag", Some(later)).unwrap().len(),
            1,
            "in a replay too"
        );

        log.append(&naming(subject(), "user:tag")).unwrap();
        index.fold(&log).unwrap();
        assert_eq!(tagged(&index, "holiday"), [], "its own retraction does");
        assert_eq!(index.standing_as_of("user:tag", Some(later)).unwrap(), []);
    }
}

#[test]
fn the_record_answers_for_what_was_taken_back() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    log.append(&said_in(
        &subject(),
        "file:path",
        json!("/home/s/a.txt"),
        "2026-09-01T10:00:00Z",
        &run_x('a'),
    ))
    .unwrap();
    log.append(&unsay(
        &log,
        &subject(),
        "file:path",
        Some(&json!("/home/s/a.txt")),
        "2026-09-03T10:00:00Z",
        "ingest",
        run_x('c'),
    ))
    .unwrap();
    index.fold(&log).unwrap();

    let path = term("file:path", "*");
    assert_eq!(
        index
            .find(std::slice::from_ref(&path), &[], Scope::Present)
            .unwrap(),
        []
    );
    assert_eq!(
        index
            .find(std::slice::from_ref(&path), &[], Scope::Held)
            .unwrap(),
        []
    );
    assert_eq!(
        index
            .find(std::slice::from_ref(&path), &[], Scope::Record)
            .unwrap(),
        [subject()],
        "the record keeps the path, standing or not"
    );
    assert_eq!(
        index
            .find(&[field(Field::Retract, "true")], &[], Scope::Record)
            .unwrap(),
        [subject()],
        "the retraction claim itself is on the record"
    );
    assert_eq!(
        index
            .find(
                &[field(Field::Retract, "true"), path.clone()],
                &[],
                Scope::Record
            )
            .unwrap(),
        [],
        "a retraction carries no value to match; it names claims"
    );
    assert_eq!(
        index
            .find(&[field(Field::Retract, "true"), path], &[], Scope::Held)
            .unwrap(),
        [],
        "a retraction never stands"
    );
    assert_eq!(
        index
            .values(
                &subject(),
                &Attribute::parse("file:path").unwrap(),
                Scope::Record
            )
            .unwrap(),
        [json!("/home/s/a.txt")],
        "under the record a value said and taken back shows once"
    );
    assert_eq!(index.subjects(Scope::Record).unwrap(), [subject()]);
    assert_eq!(index.subjects(Scope::Held).unwrap(), []);
}

#[test]
fn history_orders_runs_by_their_first_claim_across_a_seal() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    // Three runs begun c, b, a, a second apart — and a goes on past
    // a seal, so its lowest position lies in a later segment.
    for (letter, value, when) in [
        ('c', "c", "2026-09-01T10:00:00Z"),
        ('b', "b", "2026-09-01T10:00:01Z"),
        ('a', "a", "2026-09-01T10:00:02Z"),
    ] {
        log.append(&said_in(
            &subject(),
            "user:tag",
            json!(value),
            when,
            &run_x(letter),
        ))
        .unwrap();
    }
    log.seal().unwrap().unwrap();
    log.append(&said_in(
        &subject(),
        "user:tag",
        json!("a again"),
        "2026-09-01T10:00:03Z",
        &run_x('a'),
    ))
    .unwrap();
    index.fold(&log).unwrap();

    let runs: Vec<Run> = index
        .history()
        .unwrap()
        .into_iter()
        .map(|episode| episode.run)
        .collect();
    assert_eq!(
        runs,
        [run_x('c'), run_x('b'), run_x('a')],
        "log order of the first claim, not the lowest segment and position apart"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one record, three runs and a runless claim"
)]
fn history_tells_each_run_from_its_first_claim_to_its_last() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    log.append(&said_in(
        &subject(),
        "file:name",
        json!("a.txt"),
        "2026-09-01T10:00:00Z",
        &run_x('a'),
    ))
    .unwrap();
    log.append(&said_in(
        &other(),
        "file:name",
        json!("b.txt"),
        "2026-09-01T10:00:07Z",
        &run_x('a'),
    ))
    .unwrap();
    log.append(
        &Claim::assert(
            subject(),
            Attribute::parse("user:tag").unwrap(),
            json!("beach"),
            Timestamp::parse("2026-09-01T10:00:07Z").unwrap(),
            Source::parse("user").unwrap(),
            run_x('a'),
        )
        .unwrap(),
    )
    .unwrap();
    log.append(&unsay(
        &log,
        &other(),
        "file:name",
        Some(&json!("b.txt")),
        "2026-09-02T10:00:00Z",
        "ingest",
        run_x('b'),
    ))
    .unwrap();
    // A claim from before runs were written belongs to no run.
    log.append(
        &Claim::recorded(
            subject(),
            Attribute::parse("user:tag").unwrap(),
            Some(json!("old")),
            Timestamp::parse("2026-08-01T10:00:00Z").unwrap(),
            Source::parse("user").unwrap(),
            None,
            Vec::new(),
        )
        .unwrap(),
    )
    .unwrap();
    index.fold(&log).unwrap();

    assert_eq!(
        index.history().unwrap(),
        [
            Episode {
                run: run_x('a'),
                first: "2026-09-01T10:00:00Z".to_string(),
                last: "2026-09-01T10:00:07Z".to_string(),
                sources: vec![
                    Source::parse("ingest").unwrap(),
                    Source::parse("user").unwrap()
                ],
                files: 2,
                claims: 3,
                retractions: 0,
            },
            Episode {
                run: run_x('b'),
                first: "2026-09-02T10:00:00Z".to_string(),
                last: "2026-09-02T10:00:00Z".to_string(),
                sources: vec![Source::parse("ingest").unwrap()],
                files: 1,
                claims: 1,
                retractions: 1,
            },
        ],
        "in log order, the runless claim in no episode"
    );
    assert_eq!(
        index
            .find(&[field(Field::Run, "*")], &[], Scope::Held)
            .unwrap(),
        [subject()],
        "every file with a standing value from a run; file b's name was taken back, and the runless tag counts for nothing"
    );
    assert_eq!(
        index
            .field_values(&subject(), Field::Run, Scope::Held)
            .unwrap(),
        [json!(run_x('a').as_str())],
        "the runless tag contributes nothing to run"
    );
    assert_eq!(
        index
            .field_values(&subject(), Field::Attribute, Scope::Held)
            .unwrap(),
        [json!("file:name"), json!("user:tag")]
    );
}

#[test]
fn as_of_a_run_closes_after_its_last_claim() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    // Two runs a second apart, and a third within a's last second:
    // within one second the record has no order, so a run's last
    // second is taken as its own.
    let when = "2026-09-01T10:00:00Z";
    let later = "2026-09-01T10:00:01Z";
    log.append(&said_in(
        &subject(),
        "user:tag",
        json!("first"),
        when,
        &run_x('a'),
    ))
    .unwrap();
    log.append(&said_in(
        &subject(),
        "user:tag",
        json!("second"),
        later,
        &run_x('b'),
    ))
    .unwrap();
    log.append(&said_in(
        &subject(),
        "user:tag",
        json!("alongside"),
        when,
        &run_x('d'),
    ))
    .unwrap();
    index.fold(&log).unwrap();

    let tag = Attribute::parse("user:tag").unwrap();
    let after_a = index.as_of_run(&run_x('a')).unwrap().unwrap();
    assert_eq!(
        after_a.values(&subject(), &tag, Scope::Held).unwrap(),
        [json!("first")],
        "the view closes after run a's last claim, run b is not yet"
    );
    let after_b = index.as_of_run(&run_x('b')).unwrap().unwrap();
    assert_eq!(
        after_b.values(&subject(), &tag, Scope::Held).unwrap(),
        [json!("alongside"), json!("first"), json!("second")]
    );
    let after_d = index.as_of_run(&run_x('d')).unwrap().unwrap();
    assert_eq!(
        after_d.values(&subject(), &tag, Scope::Held).unwrap(),
        [json!("alongside")],
        "the last second of a run is its own: a's claim from it is not in d's view, and the other way round"
    );
    assert_eq!(
        index
            .as_of(later)
            .unwrap()
            .values(&subject(), &tag, Scope::Held)
            .unwrap(),
        [json!("alongside"), json!("first"), json!("second")],
        "the later second holds all three"
    );
    assert!(
        index.as_of_run(&run_x('c')).unwrap().is_none(),
        "a run no claim carries is no cutoff"
    );

    // The one door: a run id or a moment, and the moment it closes at.
    let (view, closed) = index.at(run_x('a').as_str()).unwrap().unwrap();
    assert_eq!(
        view.values(&subject(), &tag, Scope::Held).unwrap(),
        [json!("first")]
    );
    assert_eq!(closed.as_str(), when, "a run closes at its last claim");
    let (view, closed) = index.at("2026-09-01").unwrap().unwrap();
    assert_eq!(
        view.values(&subject(), &tag, Scope::Held).unwrap(),
        [json!("alongside"), json!("first"), json!("second")]
    );
    assert_eq!(
        closed.as_str(),
        "2026-09-01T23:59:59Z",
        "a date alone closes at the day's end"
    );
    assert!(index.at(run_x('c').as_str()).unwrap().is_none());
    assert!(matches!(
        index.at("last week"),
        Err(Error::Timestamp(given)) if given == "last week"
    ));
}

#[test]
fn a_value_said_twice_stands_once() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    log.append(&tag("holiday", "2026-09-01T21:14:03Z")).unwrap();
    log.append(&tag("holiday", "2026-09-02T09:00:00Z")).unwrap();
    index.fold(&log).unwrap();

    assert_eq!(
        index
            .find(&[term("user:tag", "holiday")], &[], Scope::Held)
            .unwrap(),
        [subject()],
        "the set holds it once, however often it was said"
    );
    assert_eq!(
        index.about(&subject()).unwrap().len(),
        2,
        "while the history keeps every word"
    );
}

#[test]
fn a_retracted_value_no_longer_answers() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    log.append(&tag("holiday", "2026-09-01T21:14:03Z")).unwrap();
    log.append(&tag("crete", "2026-09-01T21:14:04Z")).unwrap();
    log.append(&unsay(
        &log,
        &subject(),
        "user:tag",
        Some(&json!("holiday")),
        "2026-09-02T10:00:00Z",
        "user",
        Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
    ))
    .unwrap();
    index.fold(&log).unwrap();

    assert_eq!(
        index
            .find(&[term("user:tag", "holiday")], &[], Scope::Held)
            .unwrap(),
        []
    );
    assert_eq!(
        index
            .find(&[term("user:tag", "crete")], &[], Scope::Held)
            .unwrap(),
        [subject()],
        "the neighbour value stands untouched"
    );
}

#[test]
fn a_valueless_retraction_empties_the_attribute() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    log.append(&tag("holiday", "2026-09-01T21:14:03Z")).unwrap();
    log.append(&tag("crete", "2026-09-01T21:14:04Z")).unwrap();
    log.append(&unsay(
        &log,
        &subject(),
        "user:tag",
        None,
        "2026-09-02T10:00:00Z",
        "user",
        Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
    ))
    .unwrap();
    index.fold(&log).unwrap();

    assert_eq!(
        index
            .find(&[term("user:tag", "crete")], &[], Scope::Held)
            .unwrap(),
        []
    );
}

#[test]
fn a_retraction_seen_once_holds_across_refolds() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    // The assertion seals; the retraction stays in the open head,
    // which every fold applies afresh.
    log.append(&tag("holiday", "2026-09-01T21:14:03Z")).unwrap();
    log.seal().unwrap().unwrap();
    log.append(&unsay(
        &log,
        &subject(),
        "user:tag",
        Some(&json!("holiday")),
        "2026-09-02T10:00:00Z",
        "user",
        Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
    ))
    .unwrap();

    index.fold(&log).unwrap();
    index.fold(&log).unwrap();
    assert_eq!(
        index
            .find(&[term("user:tag", "holiday")], &[], Scope::Held)
            .unwrap(),
        [],
        "the sealed assertion is folded once and must not resurface"
    );
}

#[test]
fn values_answers_the_standing_set_and_nothing_more() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    log.append(&tag("holiday", "2026-09-01T21:14:03Z")).unwrap();
    log.append(&tag("crete", "2026-09-01T21:14:04Z")).unwrap();
    log.append(&tag("crete", "2026-09-02T08:00:00Z")).unwrap();
    log.append(&tag("beach", "2026-09-02T08:00:01Z")).unwrap();
    log.append(&unsay(
        &log,
        &subject(),
        "user:tag",
        Some(&json!("holiday")),
        "2026-09-03T10:00:00Z",
        "user",
        Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
    ))
    .unwrap();
    index.fold(&log).unwrap();

    let tags = Attribute::parse("user:tag").unwrap();
    assert_eq!(
        index.values(&subject(), &tags, Scope::Held).unwrap(),
        [json!("beach"), json!("crete")],
        "said twice stands once, retracted stands not at all; sorted by spelling"
    );
    assert_eq!(
        index
            .values(
                &subject(),
                &Attribute::parse("exif:model").unwrap(),
                Scope::Held
            )
            .unwrap(),
        Vec::<Value>::new(),
        "an attribute never claimed has nothing standing"
    );
}

#[test]
fn a_file_with_two_matching_values_answers_once() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    for (name, time) in [
        ("rebar1.jpg", "2026-09-01T10:00:00Z"),
        ("rebar4.jpg", "2026-09-01T10:00:01Z"),
    ] {
        log.append(&say(&subject(), "file:name", json!(name), time))
            .unwrap();
    }
    index.fold(&log).unwrap();

    assert_eq!(
        index
            .find(&[term("file:name", "*.jpg")], &[], Scope::Held)
            .unwrap(),
        [subject()],
        "one term, two values matching it, one file: the answer is a set of files"
    );
}

#[test]
fn the_views_show_the_tables_with_names_in_place_of_ids() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    log.append(&tag("holiday", "2026-09-01T21:14:03Z")).unwrap();
    log.seal().unwrap().unwrap();
    log.append(&tag("beach", "2026-09-01T21:14:04Z")).unwrap();
    index.fold(&log).unwrap();

    let rows: Vec<(String, String, String, String, String)> = index
        .connection
        .prepare("SELECT subject, attribute, value, source, segment FROM v_claims ORDER BY id")
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, subject().as_str());
    assert_eq!(rows[0].1, "user:tag");
    assert_eq!(rows[0].2, "\"holiday\"", "the value as stored: JSON text");
    assert_eq!(rows[0].3, "user");
    assert_eq!(rows[0].4.len(), 64, "the sealed segment by its digest");
    assert_eq!(rows[1].4, "head");

    let standing: Vec<(String, String, String)> = index
        .connection
        .prepare("SELECT subject, attribute, value FROM v_standing ORDER BY value")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert_eq!(
        standing,
        [
            (
                subject().as_str().to_string(),
                "user:tag".to_string(),
                "\"beach\"".to_string()
            ),
            (
                subject().as_str().to_string(),
                "user:tag".to_string(),
                "\"holiday\"".to_string()
            ),
        ]
    );
}

#[test]
fn places_and_runs_read_as_tables() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    for (attribute, value) in [
        ("file:path", json!("/home/john/a.txt")),
        ("file:name", json!("a.txt")),
    ] {
        log.append(&say(&subject(), attribute, value, "2026-09-01T10:00:00Z"))
            .unwrap();
    }
    index.fold(&log).unwrap();

    let path: String = index
        .connection
        .query_row("SELECT path FROM v_places", [], |row| row.get(0))
        .unwrap();
    assert_eq!(path, "/home/john/a.txt", "the path bare, not as JSON text");

    let run: (String, i64, i64) = index
        .connection
        .query_row("SELECT run, files, claims FROM v_runs", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap();
    assert_eq!(
        run,
        ("315e360b-020e-48be-8f2d-f2002a2ea9b4".to_string(), 1, 2)
    );
}

#[test]
fn two_sources_saying_one_value_stand_as_one() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    let mime = Attribute::parse("file:mime").unwrap();
    for (source, time) in [
        ("ingest", "2026-09-01T10:00:00Z"),
        ("extractor:mail/1", "2026-09-01T10:00:05Z"),
    ] {
        log.append(
            &Claim::assert(
                subject(),
                mime.clone(),
                json!("message/rfc822"),
                Timestamp::parse(time).unwrap(),
                Source::parse(source).unwrap(),
                Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    }
    index.fold(&log).unwrap();

    assert_eq!(
        index.values(&subject(), &mime, Scope::Held).unwrap(),
        [json!("message/rfc822")],
        "who says it is the claim's business; the standing set holds the value once"
    );
    assert_eq!(
        index
            .find(&[term("file:mime", "message/rfc822")], &[], Scope::Held)
            .unwrap(),
        [subject()],
        "and a search finds the file once"
    );
}

#[test]
fn as_of_answers_with_the_knowledge_of_that_day() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    log.append(&tag("holiday", "2026-09-01T10:00:00Z")).unwrap();
    log.seal().unwrap().unwrap();
    log.append(&unsay(
        &log,
        &subject(),
        "user:tag",
        Some(&json!("holiday")),
        "2026-09-03T10:00:00Z",
        "user",
        Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
    ))
    .unwrap();
    log.append(&tag("beach", "2026-09-04T10:00:00Z")).unwrap();
    index.fold(&log).unwrap();

    let tags = Attribute::parse("user:tag").unwrap();
    let early = index.as_of("2026-09-02T00:00:00Z").unwrap();
    assert_eq!(
        early.values(&subject(), &tags, Scope::Held).unwrap(),
        [json!("holiday")],
        "on the second, holiday stood and nothing had been taken back"
    );
    assert_eq!(
        early.about(&subject()).unwrap().len(),
        1,
        "the story as well ends at the cutoff"
    );

    let late = index.as_of("2026-09-03T12:00:00Z").unwrap();
    assert_eq!(
        late.values(&subject(), &tags, Scope::Held).unwrap(),
        Vec::<Value>::new(),
        "after the retraction nothing stands, and beach has not arrived yet"
    );
    assert_eq!(
        index.values(&subject(), &tags, Scope::Held).unwrap(),
        [json!("beach")],
        "the index itself keeps answering for today"
    );
}

#[test]
fn standing_answers_the_whole_outcome_of_one_subject() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    log.append(&tag("crete", "2026-09-01T21:14:03Z")).unwrap();
    log.append(&tag("beach", "2026-09-01T21:14:04Z")).unwrap();
    log.append(&say(
        &subject(),
        "file:name",
        json!("dscn0042.jpg"),
        "2026-09-01T21:14:05Z",
    ))
    .unwrap();
    log.append(&tag("holiday", "2026-09-01T21:14:06Z")).unwrap();
    log.append(&unsay(
        &log,
        &subject(),
        "user:tag",
        Some(&json!("holiday")),
        "2026-09-02T10:00:00Z",
        "user",
        Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
    ))
    .unwrap();
    let other =
        Subject::parse("1111111111111111111111111111111111111111111111111111111111111111").unwrap();
    log.append(&tag_about(other, "noise", "2026-09-02T10:00:01Z"))
        .unwrap();
    index.fold(&log).unwrap();

    let pairs = index.standing(&subject()).unwrap();
    assert_eq!(
        pairs,
        [
            (
                Attribute::parse("file:name").unwrap(),
                json!("dscn0042.jpg")
            ),
            (Attribute::parse("user:tag").unwrap(), json!("beach")),
            (Attribute::parse("user:tag").unwrap(), json!("crete")),
        ],
        "every attribute of this subject and no other's, retracted values gone, ordered by attribute then value"
    );
}

#[test]
fn find_needs_every_term_to_stand() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    let other =
        Subject::parse("1111111111111111111111111111111111111111111111111111111111111111").unwrap();
    log.append(&say(
        &subject(),
        "file:mime",
        json!("image/jpeg"),
        "2026-09-01T21:14:03Z",
    ))
    .unwrap();
    log.append(&tag("holiday", "2026-09-01T21:14:04Z")).unwrap();
    log.append(&say(
        &other,
        "file:mime",
        json!("image/jpeg"),
        "2026-09-01T21:14:05Z",
    ))
    .unwrap();
    index.fold(&log).unwrap();

    assert_eq!(
        index
            .find(
                &[term("file:mime", "image/jpeg"), term("user:tag", "holiday")],
                &[],
                Scope::Held
            )
            .unwrap(),
        [subject()],
        "both terms, one subject; the untagged jpeg is not it"
    );
}

#[test]
fn a_pattern_matches_within_string_values_only() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    log.append(&say(
        &subject(),
        "file:path",
        json!("/photos/2019/crete/beach.jpg"),
        "2026-09-01T21:14:03Z",
    ))
    .unwrap();
    log.append(&say(
        &subject(),
        "file:size",
        json!(2019),
        "2026-09-01T21:14:03Z",
    ))
    .unwrap();
    index.fold(&log).unwrap();

    assert_eq!(
        index
            .find(&[term("file:path", "*crete*")], &[], Scope::Held)
            .unwrap(),
        [subject()]
    );
    assert_eq!(
        index
            .find(&[term("file:size", "20*")], &[], Scope::Held)
            .unwrap(),
        [],
        "numbers were promised no wildcards"
    );
    assert_eq!(
        index
            .find(&[term("file:size", "2019")], &[], Scope::Held)
            .unwrap(),
        [subject()],
        "while the exact number answers"
    );
}

#[test]
fn a_namespace_answers_whole_and_the_record_names_its_subjects() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    let when = "2026-09-01T21:14:03Z";
    log.append(&say(&subject(), "exif:model", json!("Pixel 7"), when))
        .unwrap();
    log.append(&say(&subject(), "exif:make", json!("Google"), when))
        .unwrap();
    log.append(&say(&subject(), "file:size", json!(2019), when))
        .unwrap();
    index.fold(&log).unwrap();

    let spelled: Vec<(String, Value)> = index
        .values_in(&subject(), "exif", Scope::Held)
        .unwrap()
        .into_iter()
        .map(|(attribute, value)| (attribute.as_str().to_string(), value))
        .collect();
    assert_eq!(
        spelled,
        [
            ("exif:make".to_string(), json!("Google")),
            ("exif:model".to_string(), json!("Pixel 7")),
        ],
        "the namespace whole, ordered by attribute"
    );
    assert_eq!(
        index.values_in(&subject(), "user", Scope::Held).unwrap(),
        [],
        "a namespace nothing stands in answers empty"
    );
    assert_eq!(
        index.subjects(Scope::Held).unwrap(),
        [subject()],
        "the record names every subject it speaks about"
    );
}

#[test]
fn attributes_names_every_attribute_and_counts_its_files() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    let other =
        Subject::parse("1111111111111111111111111111111111111111111111111111111111111111").unwrap();
    let when = "2026-09-01T21:14:03Z";
    log.append(&say(&subject(), "file:mime", json!("image/jpeg"), when))
        .unwrap();
    log.append(&say(&other, "file:mime", json!("image/jpeg"), when))
        .unwrap();
    log.append(&say(&subject(), "user:tag", json!("holiday"), when))
        .unwrap();
    log.append(&say(&subject(), "user:tag", json!("beach"), when))
        .unwrap();
    log.append(&say(&other, "exif:make", json!("Google"), when))
        .unwrap();
    log.append(&unsay(
        &log,
        &other.clone(),
        "exif:make",
        Some(&json!("Google")),
        "2026-09-02T10:00:00Z",
        "user",
        Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
    ))
    .unwrap();
    index.fold(&log).unwrap();

    let names: Vec<String> = index
        .attributes()
        .unwrap()
        .into_iter()
        .map(|attribute| attribute.as_str().to_string())
        .collect();
    assert_eq!(
        names,
        ["exif:make", "file:mime", "user:tag"],
        "sorted, and an attribute retracted everywhere is listed"
    );
    let counted: Vec<(String, u64)> = index
        .attribute_counts()
        .unwrap()
        .into_iter()
        .map(|(attribute, files)| (attribute.as_str().to_string(), files))
        .collect();
    assert_eq!(
        counted,
        [
            ("exif:make".to_string(), 0),
            ("file:mime".to_string(), 2),
            ("user:tag".to_string(), 1)
        ],
        "files are counted, not values: two tags on one file are one file"
    );
}

#[test]
fn missing_names_what_a_subject_lacks() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    let bare =
        Subject::parse("1111111111111111111111111111111111111111111111111111111111111111").unwrap();
    log.append(&say(
        &subject(),
        "file:mime",
        json!("image/jpeg"),
        "2026-09-01T21:14:03Z",
    ))
    .unwrap();
    log.append(&say(
        &subject(),
        "exif:make",
        json!("Google"),
        "2026-09-01T21:14:03Z",
    ))
    .unwrap();
    log.append(&say(
        &bare,
        "file:mime",
        json!("image/jpeg"),
        "2026-09-01T21:14:04Z",
    ))
    .unwrap();
    index.fold(&log).unwrap();

    assert_eq!(
        index
            .find(
                &[term("file:mime", "image/jpeg")],
                &["exif:".to_string()],
                Scope::Held
            )
            .unwrap(),
        std::slice::from_ref(&bare),
        "the namespace prefix names the lack"
    );
    assert_eq!(
        index
            .find(&[], &["exif:make".to_string()], Scope::Held)
            .unwrap(),
        [bare],
        "with no terms, missing is asked of every subject"
    );
}

#[test]
fn a_bad_namespace_in_missing_is_named_as_given() {
    let dir = TempDir::new().unwrap();
    let index = index_in(&dir);

    let error = index
        .find(&[], &["Bad!:".to_string()], Scope::Held)
        .unwrap_err();

    assert_eq!(error.to_string(), r#"not an attribute: "Bad!:""#);
}

#[test]
fn a_range_speaks_about_one_standing_value() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    let august =
        Subject::parse("1111111111111111111111111111111111111111111111111111111111111111").unwrap();
    let december =
        Subject::parse("2222222222222222222222222222222222222222222222222222222222222222").unwrap();
    let both =
        Subject::parse("3333333333333333333333333333333333333333333333333333333333333333").unwrap();
    let when = "2026-09-01T00:00:00Z";
    log.append(&say(
        &august,
        "file:modified",
        json!("2026-08-15T10:00:00Z"),
        when,
    ))
    .unwrap();
    log.append(&say(
        &december,
        "file:modified",
        json!("2026-12-24T10:00:00Z"),
        when,
    ))
    .unwrap();
    log.append(&say(
        &both,
        "file:modified",
        json!("2026-08-15T10:00:00Z"),
        when,
    ))
    .unwrap();
    log.append(&say(
        &both,
        "file:modified",
        json!("2026-12-24T10:00:00Z"),
        when,
    ))
    .unwrap();
    index.fold(&log).unwrap();

    assert_eq!(
        index
            .find(&[term("file:modified", "2026-09-01..")], &[], Scope::Held)
            .unwrap(),
        [december.clone(), both.clone()],
        "since: one standing value past the bound suffices"
    );
    assert_eq!(
        index
            .find(
                &[term("file:modified", "2026-09-01..2026-10-01")],
                &[],
                Scope::Held
            )
            .unwrap(),
        [],
        "one range term wants a single value inside — nobody has one"
    );
    assert_eq!(
        index
            .find(
                &[
                    term("file:modified", "2026-09-01.."),
                    term("file:modified", "..2026-10-01"),
                ],
                &[],
                Scope::Held
            )
            .unwrap(),
        [both],
        "two terms may be satisfied by two different values"
    );
}

#[test]
fn a_numeric_range_reads_numbers_only() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    let worded =
        Subject::parse("1111111111111111111111111111111111111111111111111111111111111111").unwrap();
    log.append(&say(
        &subject(),
        "user:rating",
        json!(7),
        "2026-09-01T00:00:00Z",
    ))
    .unwrap();
    log.append(&say(
        &worded,
        "user:rating",
        json!("7ish"),
        "2026-09-01T00:00:00Z",
    ))
    .unwrap();
    index.fold(&log).unwrap();

    assert_eq!(
        index
            .find(&[term("user:rating", "1..10")], &[], Scope::Held)
            .unwrap(),
        [subject()],
        "numeric bounds speak about numbers; the worded rating is not seven"
    );
}

#[test]
fn a_verbatim_spelling_ranges_in_its_own_order() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    log.append(&say(
        &subject(),
        "exif:date-time-original",
        json!("2026:07:25 23:09:09"),
        "2026-09-01T00:00:00Z",
    ))
    .unwrap();
    index.fold(&log).unwrap();

    assert_eq!(
        index
            .find(
                &[term("exif:date-time-original", "2026:07:01..2026:08:01")],
                &[],
                Scope::Held
            )
            .unwrap(),
        [subject()],
        "EXIF's own spelling is fixed-width and sorts chronologically"
    );
    assert_eq!(
        index
            .find(
                &[term("exif:date-time-original", "2026:08:01..")],
                &[],
                Scope::Held
            )
            .unwrap(),
        []
    );
}

#[test]
fn quotes_take_a_value_literally() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    log.append(&say(
        &subject(),
        "user:note",
        json!("see 3..4"),
        "2026-09-01T00:00:00Z",
    ))
    .unwrap();
    index.fold(&log).unwrap();

    assert_eq!(
        index
            .find(&[term("user:note", "\"see 3..4\"")], &[], Scope::Held)
            .unwrap(),
        [subject()],
        "quoted, the dots are just dots"
    );
    assert_eq!(
        index
            .find(&[term("user:note", "\"see 3\"")], &[], Scope::Held)
            .unwrap(),
        [],
        "and quoted means whole, not prefix"
    );
}

#[test]
fn a_bare_range_asks_only_that_the_attribute_stands() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    let bare =
        Subject::parse("1111111111111111111111111111111111111111111111111111111111111111").unwrap();
    log.append(&tag("holiday", "2026-09-01T21:14:03Z")).unwrap();
    log.append(&say(
        &bare,
        "file:mime",
        json!("image/jpeg"),
        "2026-09-01T21:14:04Z",
    ))
    .unwrap();
    index.fold(&log).unwrap();

    assert_eq!(
        index
            .find(&[term("user:tag", "..")], &[], Scope::Held)
            .unwrap(),
        [subject()],
        "the presence question, --missing turned around"
    );
}

fn part(part: &str, pattern: &str) -> Term {
    Term::Part(Part::parse(part).unwrap(), pattern.to_string())
}

#[test]
fn part_terms_on_one_attribute_match_one_value() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    // Munich's latitude with Berlin's longitude, and the other way
    // round: each half lies in a box around Munich, no position does.
    let crossed =
        Subject::parse("1111111111111111111111111111111111111111111111111111111111111111").unwrap();
    let when = "2026-09-01T00:00:00Z";
    let position =
        |latitude: f64, longitude: f64| json!({"latitude": latitude, "longitude": longitude});
    log.append(&say(
        &subject(),
        "geo:position",
        position(48.137_154, 11.576_124),
        when,
    ))
    .unwrap();
    log.append(&say(
        &crossed,
        "geo:position",
        position(48.137_154, 13.404_954),
        when,
    ))
    .unwrap();
    log.append(&say(
        &crossed,
        "geo:position",
        position(52.520_008, 11.576_124),
        when,
    ))
    .unwrap();
    index.fold(&log).unwrap();

    assert_eq!(
        index
            .find(
                &[
                    part("geo:position.latitude", "48.1..48.2"),
                    part("geo:position.longitude", "11.5..11.6"),
                ],
                &[],
                Scope::Held
            )
            .unwrap(),
        [subject()],
        "both ranges must hold on the same position"
    );
    assert_eq!(
        index
            .find(
                &[part("geo:position.latitude", "48.1..48.2")],
                &[],
                Scope::Held
            )
            .unwrap(),
        [crossed.clone(), subject()],
        "one part term: any position with that latitude"
    );
    assert_eq!(
        index
            .find(
                &[
                    part("geo:position.latitude", "48.1..48.2"),
                    term("geo:position", ".."),
                ],
                &[],
                Scope::Held
            )
            .unwrap(),
        [crossed, subject()],
        "a term on the whole value is asked on its own"
    );
}

#[test]
fn a_part_term_reads_its_pattern_like_a_value_term() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    let unranked =
        Subject::parse("1111111111111111111111111111111111111111111111111111111111111111").unwrap();
    let when = "2026-09-01T00:00:00Z";
    log.append(&say(
        &subject(),
        "test:city",
        json!({"name": "Zürich", "rank": 7, "codes": ["ZH", "8001"]}),
        when,
    ))
    .unwrap();
    log.append(&say(&unranked, "test:city", json!({"name": "Bern"}), when))
        .unwrap();
    index.fold(&log).unwrap();
    let found = |name: &str, pattern: &str| {
        index
            .find(&[part(name, pattern)], &[], Scope::Held)
            .unwrap()
    };

    assert_eq!(found("test:city.name", "Z*"), [subject()], "glob");
    assert_eq!(
        found("test:city.name", "\"Zürich\""),
        [subject()],
        "literal"
    );
    assert_eq!(
        found("test:city.name", "Bern"),
        std::slice::from_ref(&unranked),
        "exact text"
    );
    assert_eq!(found("test:city.name", "A..C"), [unranked], "text range");
    assert_eq!(found("test:city.rank", "7"), [subject()], "exact number");
    assert_eq!(
        found("test:city.rank", "1..10"),
        [subject()],
        "numeric range"
    );
    assert_eq!(
        found("test:city.rank", ".."),
        [subject()],
        "the part is there"
    );
    assert_eq!(
        found("test:city.codes.0", "ZH"),
        [subject()],
        "a list position"
    );
    assert_eq!(
        found("test:city.codes.2", ".."),
        [],
        "past the end of the list"
    );
    assert_eq!(
        found("test:city.name.0", ".."),
        [],
        "a position asked of text"
    );
}

#[test]
fn a_missing_part_is_one_no_value_has() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    let flat =
        Subject::parse("1111111111111111111111111111111111111111111111111111111111111111").unwrap();
    let unplaced =
        Subject::parse("2222222222222222222222222222222222222222222222222222222222222222").unwrap();
    let when = "2026-09-01T00:00:00Z";
    log.append(&say(
        &subject(),
        "geo:position",
        json!({"latitude": 48.1, "longitude": 11.5, "altitude": 519}),
        when,
    ))
    .unwrap();
    log.append(&say(
        &subject(),
        "geo:position",
        json!({"latitude": 48.2, "longitude": 11.6}),
        when,
    ))
    .unwrap();
    log.append(&say(
        &flat,
        "geo:position",
        json!({"latitude": 48.1, "longitude": 11.5}),
        when,
    ))
    .unwrap();
    log.append(&say(&unplaced, "file:mime", json!("image/jpeg"), when))
        .unwrap();
    index.fold(&log).unwrap();

    assert_eq!(
        index
            .find(&[], &["geo:position.altitude".to_string()], Scope::Held)
            .unwrap(),
        [flat.clone(), unplaced],
        "one position with an altitude is enough to have it"
    );
    assert_eq!(
        index
            .find(
                &[term("geo:position", "..")],
                &["geo:position.altitude".to_string()],
                Scope::Held
            )
            .unwrap(),
        [flat]
    );
    let error = index
        .find(&[], &["geo:position.Altitude".to_string()], Scope::Held)
        .unwrap_err();
    assert!(matches!(error, Error::Part(given) if given == "geo:position.Altitude"));
}

#[test]
fn a_field_term_asks_about_the_claim_of_the_value_the_parts_match() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    let when = "2026-09-01T00:00:00Z";
    log.append(&said_in(
        &subject(),
        "geo:position",
        json!({"latitude": 48.1, "longitude": 11.5}),
        when,
        &run_x('a'),
    ))
    .unwrap();
    log.append(&said_in(
        &subject(),
        "geo:position",
        json!({"latitude": 52.5, "longitude": 13.4}),
        when,
        &run_x('b'),
    ))
    .unwrap();
    index.fold(&log).unwrap();
    let found = |run: char| {
        index
            .find(
                &[
                    part("geo:position.latitude", "..50"),
                    field(Field::Run, run_x(run).as_str()),
                ],
                &[],
                Scope::Held,
            )
            .unwrap()
    };

    assert_eq!(found('a'), [subject()]);
    assert_eq!(
        found('b'),
        [],
        "run b recorded only the position further north"
    );
}

#[test]
fn a_retracted_value_has_its_parts_found_in_the_record_only() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    let position = json!({"latitude": 48.1, "longitude": 11.5});
    log.append(&say(
        &subject(),
        "geo:position",
        position.clone(),
        "2026-09-01T00:00:00Z",
    ))
    .unwrap();
    log.append(&unsay(
        &log,
        &subject(),
        "geo:position",
        Some(&position),
        "2026-09-02T00:00:00Z",
        "user",
        run_x('a'),
    ))
    .unwrap();
    index.fold(&log).unwrap();
    let terms = [part("geo:position.latitude", "48.1")];

    assert_eq!(index.find(&terms, &[], Scope::Held).unwrap(), []);
    assert_eq!(index.find(&terms, &[], Scope::Record).unwrap(), [subject()]);
}

/// `docs/format.md` promises that the order of an object's members
/// does not matter. The index keeps a value as its JSON text, and
/// `serde_json` writes the members sorted only while no crate in the
/// build enables its `preserve_order` feature.
#[test]
fn the_order_of_an_objects_members_makes_no_second_value() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    for (text, time) in [
        (
            r#"{"latitude": 48.1, "longitude": 11.5}"#,
            "2026-09-01T00:00:00Z",
        ),
        (
            r#"{"longitude": 11.5, "latitude": 48.1}"#,
            "2026-09-02T00:00:00Z",
        ),
    ] {
        let value: Value = serde_json::from_str(text).unwrap();
        log.append(&say(&subject(), "geo:position", value, time))
            .unwrap();
    }
    index.fold(&log).unwrap();

    let attribute = Attribute::parse("geo:position").unwrap();
    assert_eq!(
        index
            .values(&subject(), &attribute, Scope::Held)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn under_answers_component_wise() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    let neighbour =
        Subject::parse("1111111111111111111111111111111111111111111111111111111111111111").unwrap();
    let deeper =
        Subject::parse("2222222222222222222222222222222222222222222222222222222222222222").unwrap();
    let when = "2026-09-01T21:14:03Z";
    log.append(&say(
        &subject(),
        "file:path",
        json!("/home/john/a.txt"),
        when,
    ))
    .unwrap();
    log.append(&say(
        &neighbour,
        "file:path",
        json!("/home/johnny/b.txt"),
        when,
    ))
    .unwrap();
    log.append(&say(
        &deeper,
        "file:path",
        json!("/home/john/photos/c.jpg"),
        when,
    ))
    .unwrap();
    index.fold(&log).unwrap();

    assert_eq!(
        index.under("/home/john").unwrap(),
        [
            ("/home/john/a.txt".to_string(), subject()),
            ("/home/john/photos/c.jpg".to_string(), deeper.clone()),
        ],
        "a place bounds by component: johnny is a neighbour, not a child"
    );
    assert_eq!(
        index.under("/home/john/a.txt").unwrap(),
        [("/home/john/a.txt".to_string(), subject())],
        "a place that is a file answers with itself"
    );
    assert_eq!(
        index.under("/").unwrap().len(),
        3,
        "the root is over everything"
    );
    assert_eq!(index.under("/mnt").unwrap(), []);
}

#[test]
fn under_reads_standing_string_places_only() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    let when = "2026-09-01T21:14:03Z";
    log.append(&say(
        &subject(),
        "file:path",
        json!("/home/john/a.txt"),
        when,
    ))
    .unwrap();
    log.append(&say(&subject(), "file:path", json!(42), when))
        .unwrap();
    log.append(&unsay(
        &log,
        &subject(),
        "file:path",
        Some(&json!("/home/john/a.txt")),
        "2026-09-02T10:00:00Z",
        "user",
        Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
    ))
    .unwrap();
    index.fold(&log).unwrap();

    assert_eq!(
        index.under("/").unwrap(),
        [],
        "the retracted place no longer stands, and a number names none"
    );
}

#[test]
fn standing_as_of_replays_to_a_closing_time() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    let place = json!("/home/john/a.txt");
    log.append(&say(
        &subject(),
        "file:path",
        place.clone(),
        "2026-01-01T00:00:00Z",
    ))
    .unwrap();
    log.append(&unsay(
        &log,
        &subject(),
        "file:path",
        Some(&place),
        "2026-02-01T00:00:00Z",
        "user",
        Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
    ))
    .unwrap();
    log.append(&say(
        &subject(),
        "file:path",
        place.clone(),
        "2026-03-01T00:00:00Z",
    ))
    .unwrap();
    index.fold(&log).unwrap();

    let at = |cutoff| index.standing_as_of("file:path", cutoff).unwrap();
    assert_eq!(
        (
            at(Some("2026-01-15T00:00:00Z"))[0].value.clone(),
            at(Some("2026-01-15T00:00:00Z"))[0].asserted.clone()
        ),
        (place.clone(), "2026-01-01T00:00:00Z".to_string()),
        "before the retraction the first assertion stands"
    );
    assert_eq!(
        at(Some("2026-02-15T00:00:00Z")),
        [],
        "at a cutoff behind the retraction nothing stands"
    );
    let today = at(None);
    let surviving = index
        .standing_ids(&subject(), &Attribute::parse("file:path").unwrap(), None)
        .unwrap();
    assert_eq!(surviving.len(), 1);
    assert_eq!(
        (
            today[0].value.clone(),
            today[0].asserted.clone(),
            today[0].claim.clone()
        ),
        (
            place,
            "2026-03-01T00:00:00Z".to_string(),
            surviving[0].clone()
        ),
        "no cutoff answers for today, and the surviving assertion names its moment and its id"
    );
}

#[test]
fn standing_as_of_renews_the_moment_and_empties_whole_attributes() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    log.append(&tag("holiday", "2026-01-01T00:00:00Z")).unwrap();
    log.append(&tag("beach", "2026-01-02T00:00:00Z")).unwrap();
    log.append(&tag("holiday", "2026-01-03T00:00:00Z")).unwrap();
    log.append(&unsay(
        &log,
        &subject(),
        "user:tag",
        None,
        "2026-01-04T00:00:00Z",
        "user",
        Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
    ))
    .unwrap();
    index.fold(&log).unwrap();

    let before = index
        .standing_as_of("user:tag", Some("2026-01-03T00:00:00Z"))
        .unwrap();
    assert_eq!(
        before
            .iter()
            .map(|standing| (standing.value.clone(), standing.asserted.as_str()))
            .collect::<Vec<_>>(),
        [
            (json!("beach"), "2026-01-02T00:00:00Z"),
            (json!("holiday"), "2026-01-03T00:00:00Z"),
        ],
        "a repeated assertion collapses in the set but renews its moment"
    );
    assert_eq!(
        index.standing_as_of("user:tag", None).unwrap(),
        [],
        "a valueless retraction empties the attribute in the replay too"
    );
}

/// One sighting the way ingest writes one: place, name and run
/// with one moment and one source.
fn sight(log: &Log, subject: &Subject, run: &Run, path: &str, time: &str) {
    let time = Timestamp::parse(time).unwrap();
    let source = Source::parse("ingest").unwrap();
    let name = path.rsplit('/').next().unwrap();
    for (attribute, value) in [("file:path", json!(path)), ("file:name", json!(name))] {
        log.append(
            &Claim::assert(
                subject.clone(),
                Attribute::parse(attribute).unwrap(),
                value,
                time.clone(),
                source.clone(),
                run.clone(),
            )
            .unwrap(),
        )
        .unwrap();
    }
}

#[test]
fn run_sightings_answer_with_the_runs_own_places() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    let moved =
        Subject::parse("1111111111111111111111111111111111111111111111111111111111111111").unwrap();
    sight(
        &log,
        &subject(),
        &run_x('a'),
        "/home/s/a.txt",
        "2026-09-01T10:00:00Z",
    );
    sight(
        &log,
        &moved,
        &run_x('a'),
        "/home/s/b.txt",
        "2026-09-01T10:00:01Z",
    );
    // The same content, met again by a later run somewhere else: its
    // path belongs to that run, not to run-a.
    sight(
        &log,
        &subject(),
        &run_x('b'),
        "/mnt/nas/a.txt",
        "2026-09-02T10:00:00Z",
    );
    index.fold(&log).unwrap();

    assert_eq!(
        index.run_sightings(&run_x('a')).unwrap(),
        [
            (moved, Placement::Path("/home/s/b.txt".to_string())),
            (subject(), Placement::Path("/home/s/a.txt".to_string())),
        ],
        "each run answers with the places it recorded itself, by subject"
    );
    assert_eq!(
        index.run_sightings(&run_x('b')).unwrap(),
        [(subject(), Placement::Path("/mnt/nas/a.txt".to_string()))]
    );
    assert_eq!(index.run_sightings(&run_x('c')).unwrap(), []);

    // A run that only took something back is on the record and names
    // no file; only has_run tells it from a run that never was.
    let run = run_x('d');
    log.append(&unsay(
        &log,
        &subject(),
        "file:path",
        Some(&json!("/home/s/a.txt")),
        "2026-09-03T10:00:00Z",
        "ingest",
        run.clone(),
    ))
    .unwrap();
    index.fold(&log).unwrap();
    assert_eq!(index.run_sightings(&run).unwrap(), []);
    assert!(index.has_run(&run).unwrap());
    assert!(!index.has_run(&run_x('c')).unwrap());
}

#[test]
fn one_run_seeing_two_places_answers_twice() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    sight(
        &log,
        &subject(),
        &run_x('a'),
        "/home/s/a.txt",
        "2026-09-01T10:00:00Z",
    );
    sight(
        &log,
        &subject(),
        &run_x('a'),
        "/home/s/copy/a.txt",
        "2026-09-01T10:00:02Z",
    );
    index.fold(&log).unwrap();

    assert_eq!(
        index.run_sightings(&run_x('a')).unwrap(),
        [
            (subject(), Placement::Path("/home/s/a.txt".to_string())),
            (subject(), Placement::Path("/home/s/copy/a.txt".to_string())),
        ],
        "the run's reality had two, and the record says so"
    );
}

#[test]
fn a_sighting_without_a_path_answers_with_its_names() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    let time = Timestamp::parse("2026-09-01T10:00:00Z").unwrap();
    let source = Source::parse("extractor:mail/0.1.0").unwrap();
    log.append(
        &Claim::assert(
            subject(),
            Attribute::parse("file:name").unwrap(),
            json!("invoice.pdf"),
            time.clone(),
            source.clone(),
            run_x('e'),
        )
        .unwrap(),
    )
    .unwrap();
    index.fold(&log).unwrap();

    assert_eq!(
        index.run_sightings(&run_x('e')).unwrap(),
        [(subject(), Placement::Name("invoice.pdf".to_string()))],
        "a derived file never sat anywhere, so its name answers"
    );
}

#[test]
fn a_fetched_message_answers_with_its_mailbox_places() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    let time = Timestamp::parse("2026-09-01T10:00:00Z").unwrap();
    let source = Source::parse("mailvault").unwrap();
    for (attribute, value) in [
        ("mailbox:place", "example.org:INBOX"),
        ("mailbox:place", "example.org:Archive"),
        ("file:name", "message.eml"),
    ] {
        log.append(
            &Claim::assert(
                subject(),
                Attribute::parse(attribute).unwrap(),
                json!(value),
                time.clone(),
                source.clone(),
                run_x('f'),
            )
            .unwrap(),
        )
        .unwrap();
    }
    index.fold(&log).unwrap();

    assert_eq!(
        index.run_sightings(&run_x('f')).unwrap(),
        [
            (
                subject(),
                Placement::Mailbox("example.org:Archive".to_string())
            ),
            (
                subject(),
                Placement::Mailbox("example.org:INBOX".to_string())
            ),
        ],
        "a message answers with every folder the run saw it in, and its places win over a name"
    );
}

#[test]
fn a_file_is_placed_by_the_first_kind_of_place_that_stands() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    let time = Timestamp::parse("2026-09-01T10:00:00Z").unwrap();
    let source = Source::parse("ingest").unwrap();
    let said = |attribute: &str, value: &str| {
        Claim::assert(
            subject(),
            Attribute::parse(attribute).unwrap(),
            json!(value),
            time.clone(),
            source.clone(),
            run_x('a'),
        )
        .unwrap()
        .identified(immure::Algorithm::Sha256)
    };
    assert_eq!(index.placements(&subject()).unwrap(), []);

    let path = said("file:path", "/home/s/a.txt");
    log.append_all(&[
        path.clone(),
        said("mailbox:place", "example.org:INBOX"),
        said("mailbox:place", "example.org:Archive"),
        said("file:name", "a.txt"),
    ])
    .unwrap();
    index.fold(&log).unwrap();
    assert_eq!(
        index.placements(&subject()).unwrap(),
        [Placement::Path("/home/s/a.txt".to_string())]
    );

    log.append(
        &Claim::retract(
            subject(),
            Attribute::parse("file:path").unwrap(),
            vec![path.id().unwrap().clone()],
            Timestamp::parse("2026-09-02T10:00:00Z").unwrap(),
            source.clone(),
            run_x('b'),
        )
        .unwrap(),
    )
    .unwrap();
    index.fold(&log).unwrap();
    assert_eq!(
        index.placements(&subject()).unwrap(),
        [
            Placement::Mailbox("example.org:Archive".to_string()),
            Placement::Mailbox("example.org:INBOX".to_string()),
        ],
        "the path was retracted, and the mailbox places win over the name"
    );
}

#[test]
fn an_older_cache_is_emptied_and_refolds() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let path = {
        let cache = dir.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        cache.join("index.sqlite")
    };
    log.append(&tag("holiday", "2026-09-01T21:14:03Z")).unwrap();
    log.seal().unwrap().unwrap();
    {
        let mut index = Index::open(&path).unwrap();
        index.fold(&log).unwrap();
    }
    // An index file from before this schema announces an older
    // generation; opening it starts over instead of guessing.
    Connection::open(&path)
        .unwrap()
        .execute_batch("PRAGMA user_version = 0")
        .unwrap();

    let mut index = Index::open(&path).unwrap();
    let folded = index.fold(&log).unwrap();
    assert_eq!(folded.segments, 1, "the emptied cache folds from scratch");
    assert_eq!(
        index
            .find(&[term("user:tag", "holiday")], &[], Scope::Held)
            .unwrap(),
        [subject()]
    );
}

#[test]
fn a_subject_the_log_never_mentioned_has_nothing_to_say() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    index.fold(&log).unwrap();

    let unknown =
        Subject::parse("00000000000000000000000000000000000000000000000000000000000000ff").unwrap();
    assert_eq!(index.about(&unknown).unwrap(), Vec::new());
}

fn other() -> Subject {
    Subject::parse("aa2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e").unwrap()
}

fn said(subject: Subject, attribute: &str, value: Value, time: &str) -> Claim {
    Claim::assert(
        subject,
        Attribute::parse(attribute).unwrap(),
        value,
        Timestamp::parse(time).unwrap(),
        Source::parse("test").unwrap(),
        Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
    )
    .unwrap()
}

fn taken_back(log: &Log, subject: &Subject, attribute: &str, value: &Value, time: &str) -> Claim {
    unsay(
        log,
        subject,
        attribute,
        Some(value),
        time,
        "test",
        Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
    )
}

#[test]
fn a_file_answers_only_while_a_place_stands_on_it() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    let placed = subject();
    let placeless = other();
    log.append(&tag_about(
        placed.clone(),
        "holiday",
        "2026-09-01T10:00:00Z",
    ))
    .unwrap();
    log.append(&said(
        placed.clone(),
        "file:path",
        json!("/x/a"),
        "2026-09-01T10:00:00Z",
    ))
    .unwrap();
    log.append(&tag_about(
        placeless.clone(),
        "holiday",
        "2026-09-01T10:00:00Z",
    ))
    .unwrap();
    index.fold(&log).unwrap();

    let holiday = [term("user:tag", "holiday")];
    assert_eq!(
        index.find(&holiday, &[], Scope::Present).unwrap(),
        vec![placed.clone()],
        "no place stands on the other"
    );
    assert_eq!(
        index.find(&holiday, &[], Scope::Held).unwrap(),
        vec![placed.clone(), placeless.clone()]
    );
    assert_eq!(
        index.subjects(Scope::Present).unwrap(),
        vec![placed.clone()]
    );

    log.append(&taken_back(
        &log,
        &placed,
        "file:path",
        &json!("/x/a"),
        "2026-09-02T10:00:00Z",
    ))
    .unwrap();
    index.fold(&log).unwrap();

    assert!(
        index
            .find(&holiday, &[], Scope::Present)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        index.find(&holiday, &[], Scope::Held).unwrap(),
        vec![placed.clone(), placeless],
        "held all the same"
    );
    let before = index.as_of("2026-09-01T23:59:59Z").unwrap();
    assert_eq!(
        before.find(&holiday, &[], Scope::Present).unwrap(),
        vec![placed],
        "as of the day it lay there, it did"
    );
}

#[test]
fn what_was_derived_answers_one_step_down() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    let mail = subject();
    let pdf = other();
    let text =
        Subject::parse("bb2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e").unwrap();
    let image =
        Subject::parse("cc2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e").unwrap();
    for (child, origin) in [(&pdf, &mail), (&image, &mail), (&text, &pdf)] {
        log.append(&said(
            child.clone(),
            "prov:origin",
            json!(origin.as_str()),
            "2026-09-01T10:00:00Z",
        ))
        .unwrap();
    }
    index.fold(&log).unwrap();

    assert_eq!(
        index.derived(&mail, Scope::Held).unwrap(),
        vec![pdf.clone(), image.clone()],
        "both children, in digest order, and not the grandchild"
    );
    assert_eq!(
        index.derived(&pdf, Scope::Held).unwrap(),
        vec![text.clone()]
    );
    assert_eq!(
        index.derived(&text, Scope::Held).unwrap(),
        Vec::new(),
        "a leaf has nothing below it"
    );

    log.append(&taken_back(
        &log,
        &image,
        "prov:origin",
        &json!(mail.as_str()),
        "2026-09-02T10:00:00Z",
    ))
    .unwrap();
    index.fold(&log).unwrap();
    assert_eq!(
        index.derived(&mail, Scope::Held).unwrap(),
        vec![pdf.clone()],
        "an origin taken back no longer stands"
    );
    assert_eq!(
        index.derived(&mail, Scope::Record).unwrap(),
        vec![pdf, image],
        "but the record remembers it"
    );
}

#[test]
fn where_a_file_came_from_answers_one_step_up() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    let mail = subject();
    let other_mail = other();
    let pdf =
        Subject::parse("bb2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e").unwrap();
    let text =
        Subject::parse("cc2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e").unwrap();
    // The same attachment in two mails: one file, two origins.
    for (child, origin) in [(&pdf, &other_mail), (&pdf, &mail), (&text, &pdf)] {
        log.append(&said(
            child.clone(),
            "prov:origin",
            json!(origin.as_str()),
            "2026-09-01T10:00:00Z",
        ))
        .unwrap();
    }
    log.append(&said(
        text.clone(),
        "prov:origin",
        json!("not a name"),
        "2026-09-01T10:00:00Z",
    ))
    .unwrap();
    index.fold(&log).unwrap();

    assert_eq!(
        index.origins(&pdf, Scope::Held).unwrap(),
        vec![mail.clone(), other_mail.clone()],
        "both origins, in digest order, and not the grandparent's"
    );
    assert_eq!(
        index.origins(&text, Scope::Held).unwrap(),
        vec![pdf.clone()],
        "a value that spells no name is no step"
    );
    assert_eq!(
        index.origins(&mail, Scope::Held).unwrap(),
        Vec::new(),
        "a root came from nowhere"
    );

    log.append(&taken_back(
        &log,
        &pdf,
        "prov:origin",
        &json!(other_mail.as_str()),
        "2026-09-02T10:00:00Z",
    ))
    .unwrap();
    index.fold(&log).unwrap();
    assert_eq!(
        index.origins(&pdf, Scope::Held).unwrap(),
        vec![mail.clone()],
        "an origin taken back no longer stands"
    );
    assert_eq!(
        index.origins(&pdf, Scope::Record).unwrap(),
        vec![mail, other_mail],
        "but the record remembers it"
    );
}

#[test]
fn a_derived_file_is_placed_while_its_origin_is() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    let origin = subject();
    let derived = other();
    log.append(&said(
        origin.clone(),
        "file:path",
        json!("/x/mail.eml"),
        "2026-09-01T10:00:00Z",
    ))
    .unwrap();
    log.append(&said(
        derived.clone(),
        "prov:origin",
        json!(origin.as_str()),
        "2026-09-01T10:00:00Z",
    ))
    .unwrap();
    log.append(&tag_about(
        derived.clone(),
        "invoice",
        "2026-09-01T10:00:00Z",
    ))
    .unwrap();
    index.fold(&log).unwrap();

    let invoice = [term("user:tag", "invoice")];
    assert_eq!(
        index.find(&invoice, &[], Scope::Present).unwrap(),
        vec![derived.clone()],
        "an attachment lies where its mail lies"
    );

    log.append(&taken_back(
        &log,
        &origin,
        "file:path",
        &json!("/x/mail.eml"),
        "2026-09-02T10:00:00Z",
    ))
    .unwrap();
    index.fold(&log).unwrap();
    assert!(
        index
            .find(&invoice, &[], Scope::Present)
            .unwrap()
            .is_empty(),
        "and goes where its mail goes"
    );
}

#[test]
fn an_inner_place_places_nothing_by_itself() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    let archive = subject();
    let entry = other();
    log.append(&said(
        entry.clone(),
        "file:path",
        json!("@dir/a.txt"),
        "2026-09-01T10:00:00Z",
    ))
    .unwrap();
    log.append(&tag_about(entry.clone(), "inner", "2026-09-01T10:00:00Z"))
        .unwrap();
    index.fold(&log).unwrap();

    let inner = [term("user:tag", "inner")];
    assert!(
        index.find(&inner, &[], Scope::Present).unwrap().is_empty(),
        "an entry with only a place inside its archive is held, not placed"
    );

    log.append(&said(
        archive.clone(),
        "file:path",
        json!("/x/bundle.zip"),
        "2026-09-01T10:00:01Z",
    ))
    .unwrap();
    log.append(&said(
        entry.clone(),
        "prov:origin",
        json!(archive.as_str()),
        "2026-09-01T10:00:01Z",
    ))
    .unwrap();
    index.fold(&log).unwrap();
    assert_eq!(
        index.find(&inner, &[], Scope::Present).unwrap(),
        vec![entry.clone()],
        "and present through its archive"
    );

    log.append(&taken_back(
        &log,
        &archive,
        "file:path",
        &json!("/x/bundle.zip"),
        "2026-09-02T10:00:00Z",
    ))
    .unwrap();
    index.fold(&log).unwrap();
    assert!(
        index.find(&inner, &[], Scope::Present).unwrap().is_empty(),
        "the walk takes back the archive's place, and the entry goes with it"
    );
    assert_eq!(
        index
            .values(&entry, &Attribute::parse("file:path").unwrap(), Scope::Held)
            .unwrap(),
        vec![json!("@dir/a.txt")],
        "its inner place still stands on the record"
    );
}

#[test]
fn a_message_at_a_mailbox_place_is_placed() {
    let dir = TempDir::new().unwrap();
    let log = log_in(&dir);
    let mut index = index_in(&dir);
    let message = subject();
    log.append(&said(
        message.clone(),
        "mailbox:place",
        json!("example.org:INBOX"),
        "2026-09-01T10:00:00Z",
    ))
    .unwrap();
    log.append(&tag_about(
        message.clone(),
        "holiday",
        "2026-09-01T10:00:00Z",
    ))
    .unwrap();
    index.fold(&log).unwrap();

    assert_eq!(
        index
            .find(&[term("user:tag", "holiday")], &[], Scope::Present)
            .unwrap(),
        vec![message]
    );
}
