//! The audit: the archive held against its own record.
//!
//! An archive answers for two things at once — the bytes it holds and the
//! claims it recorded about them — and the audit proves the two still
//! agree. Every entry of both blob stores is read whole and re-hashed: a
//! name must still be true of its bytes. Every sealed segment and the
//! open head must read back claim by claim, and the chain they form must
//! hold: every segment a header names as the one sealed before it must
//! be held, because a sealed segment that is gone is a loss the record
//! itself points at. And every subject the claims speak of must be held
//! by a store: the claims are the record, and a record that names what
//! nothing holds has found a loss, not a policy — nothing is ever
//! deliberately removed from an archive. The other direction is milder:
//! bytes held that no claim mentions are noted as observations, never
//! findings, because a run interrupted between storing and recording
//! leaves such entries legitimately, and the next arrival of the same
//! bytes records them. Not so a second beginning: a segment that names
//! no predecessor, beyond the one the archive begins with, stands where
//! a head was lost and begun anew, and the claims the lost head held
//! went with it — a finding, though the chain cannot say how much.
//!
//! Bytes held by both stores are an observation of their own: a file
//! that was won as a derived file first and taken in as an original
//! later stands in `derived/` and `content/` alike, under the same name,
//! and the log never says which store answers for it. Nothing is wrong
//! with that; the audit notes each such twin with how both copies fared
//! (a [`Twin`]), because the derived copy is the one thing an archive
//! holds that it can let go of without loss, and `weed` does that.
//!
//! A break is closed by a mend, a segment of no claims that names the
//! two ends it joins (`Log::mend`). The audit applies every mend it
//! meets before it counts: a break with a mend in front of it is not a
//! finding but an observation, the record's own word that the loss was
//! seen and the chain joined over it. Nothing is rewritten for that —
//! the segment after the break still names what it named.
//!
//! A record can have more than one first segment: a segment names every
//! segment sealed before it, and after a merge of two archives one
//! segment names the last segment of each. The audit reads the chains
//! so that a segment continues the chain of the first segment it names,
//! and every other segment it names ends its chain there, merged in.
//! Which chain continues is a matter of presentation; whatever the
//! choice, the same segments are reachable from the head. A chain that
//! begins at a first segment and ends merged into another is a whole
//! line of the record, not a break: its beginning is a first segment,
//! not a head lost. A chain whose end nothing follows is where a loss
//! begins, and the chain after the loss is paired with it in the order
//! of first claims, as the breaks have always been paired.
//!
//! The whole pass works from the truth tiers alone: stores, segments,
//! head. The cache is never consulted — an audit is the tool for the day
//! nothing else is trusted, so it leans on nothing the archive could
//! rebuild from what is being audited.

use std::collections::{BTreeMap, BTreeSet};

use immure::{Digest, Store};

use crate::claim::{Claim, Subject, Timestamp, Value};
use crate::error::Result;
use crate::log::{Contents, Log};

/// Attributes whose values name other subjects. A reference in a value
/// is a reference like the claim's own subject, and the audit follows
/// both — a derived file's origin must be held no less than the derived
/// file itself.
const LINKS: [&str; 1] = ["prov:origin"];

/// One blob store's fixity: every entry read whole, its bytes re-hashed
/// against the name they are filed under.
#[derive(Debug)]
pub struct StoreAudit {
    /// Entries the walk met.
    pub checked: usize,
    /// Entries whose bytes are no longer what their names say.
    pub damaged: Vec<String>,
    /// Entries nothing could be established about, with what stood in
    /// the way. Unreadable is not damaged: blaming the entry for a
    /// permission would take a healthy name's word away for a reason
    /// that is not the entry's.
    pub unreadable: Vec<(String, String)>,
    /// Every name the store holds, damaged or not — what the presence
    /// check runs against. A damaged entry is still held; it is already
    /// a finding once, and missing on top would count the same wound
    /// twice. Keyed by the name as the claims spell it, with the digest
    /// it was walked under beside it.
    held: BTreeMap<String, Digest>,
}

impl StoreAudit {
    /// What the walk established about each entry that is not sound —
    /// every name not in the map verified.
    fn troubles(&self) -> BTreeMap<&str, Fixity> {
        let mut troubles = BTreeMap::new();
        for name in &self.damaged {
            troubles.insert(name.as_str(), Fixity::Damaged);
        }
        for (name, error) in &self.unreadable {
            troubles.insert(name.as_str(), Fixity::Unreadable(error.clone()));
        }
        troubles
    }
}

/// What one look at one entry established: the bytes are what the name
/// says, they are not, or nothing could be established at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fixity {
    /// Read whole and re-hashed: the name is true of its bytes.
    Sound,
    /// Read whole and re-hashed: the name is not.
    Damaged,
    /// Not read, with what stood in the way. Not damaged: the entry
    /// answers for its bytes, not for a permission.
    Unreadable(String),
}

/// One content held by both stores: won as a derived file, and taken in
/// as an original as well, in whichever order. The log speaks of the
/// subject, never of a store, so the two copies are one file to it, and
/// `content/` answers for it first. Which is why the derived copy can go
/// — once both are known to be sound, or the one in `content/` is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Twin {
    /// The name both copies are filed under.
    pub digest: Digest,
    /// How the copy in `content/` fared.
    pub content: Fixity,
    /// How the copy in `derived/` fared.
    pub derived: Fixity,
}

impl Twin {
    /// Whether the one in `content/` is the damaged copy and the one in
    /// `derived/` the sound one: the case where `weed` with repair puts
    /// the sound bytes back under the original's name.
    #[must_use]
    pub fn repairable(&self) -> bool {
        self.content == Fixity::Damaged && self.derived == Fixity::Sound
    }
}

/// Audit one blob store: walk it whole, verify every entry.
///
/// # Errors
///
/// [`Error::Store`](crate::Error::Store) when the store cannot be walked
/// at all; what a single entry has to answer for lands in the report
/// instead.
pub fn audit_store(store: &Store) -> Result<StoreAudit> {
    let mut report = StoreAudit {
        checked: 0,
        damaged: Vec::new(),
        unreadable: Vec::new(),
        held: BTreeMap::new(),
    };
    for entry in store.entries() {
        let entry = entry?;
        let name = entry.digest().as_str().to_string();
        report.checked += 1;
        report.held.insert(name.clone(), entry.digest().clone());
        match store.verify(&entry) {
            Ok(true) => {}
            Ok(false) => report.damaged.push(name),
            Err(error) => report.unreadable.push((name, error.to_string())),
        }
    }
    Ok(report)
}

/// The claim log read back whole: every sealed segment, the open head.
#[derive(Debug)]
pub struct LogAudit {
    /// Sealed segments the walk met.
    pub segments: usize,
    /// Claims read back, the open head's included.
    pub claims: usize,
    /// Segments whose bytes are no longer what their names say. A
    /// damaged segment is not parsed: the finding stands, and lines
    /// from bytes that lie would only decorate it.
    pub damaged: Vec<String>,
    /// Segments nothing could be established about, with what stood in
    /// the way.
    pub unreadable: Vec<(String, String)>,
    /// Segments true to their names that will not read back as
    /// segments, with the first thing wrong.
    pub broken: Vec<(String, String)>,
    /// What stands in the open head's way, when something does.
    pub head_broken: Option<String>,
    /// Segments named as the one sealed before another that no store
    /// entry answers to, and no mend stands in for: the segment that
    /// names it, and the name. Each is a sealed segment lost — the
    /// finding the chain exists for.
    pub predecessor_missing: Vec<(String, String)>,
    /// The segment the open head names as the last one sealed, when no
    /// store entry answers to it.
    pub head_predecessor_missing: Option<String>,
    /// The chains the readable segments form, oldest first, mends
    /// counted among their links. A whole record written as one line is
    /// one chain from the first segment to the open head. A record
    /// merged from several archives has one such chain and, for every
    /// line merged in, a chain from that line's first segment to the
    /// segment it was merged into ([`Chain::joined`]). Every other
    /// chain begins at a break.
    pub chains: Vec<Chain>,
    /// The breaks between the chains, oldest first. A break where a
    /// head was lost is a finding; one where a segment is lost is
    /// counted as [`predecessor_missing`](LogAudit::predecessor_missing)
    /// or [`head_predecessor_missing`](LogAudit::head_predecessor_missing)
    /// already; one behind a segment that is held but will not read is
    /// counted for that.
    pub breaks: Vec<Break>,
    /// Breaks that a mend has closed, oldest first. An observation: the
    /// chain holds, and the record says where it was joined.
    pub mended: Vec<Mended>,
    /// Mends whose loss was made good since: the segment each stood in
    /// for is held again, and the chain runs through it, not the mend.
    /// An observation — the mend stays, and says what once was gone.
    pub restored: Vec<Mended>,
    /// Mends that stand in front of a segment needing none, or of one the
    /// store does not hold. An observation.
    pub idle_mends: Vec<String>,
    /// Mends on a chain that runs in a circle: a mend joined two ends
    /// that were not a break's, and the chain no longer has a first
    /// segment. A finding — the record cannot be read in order.
    pub looped: Vec<String>,
    /// Every subject the readable claims speak of: their subjects, and
    /// the subjects link values name. Read from the whole history,
    /// retractions included — a retraction withdraws a statement, never
    /// bytes.
    referenced: BTreeSet<String>,
}

impl LogAudit {
    /// How many heads were lost and begun anew, by the breaks that say
    /// so — what the chain adds to the findings.
    #[must_use]
    pub fn heads_lost(&self) -> usize {
        self.breaks
            .iter()
            .filter(|brk| brk.cause == Cause::HeadLost)
            .count()
    }
}

/// One unbroken run of segments: from a segment that follows nothing
/// held to the last one that anything follows, to the open head, or to
/// where the chain was merged into another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chain {
    /// The sealed segments in chain order, mends among them.
    pub segments: Vec<String>,
    /// Whether the open head continues the chain. Only the last chain
    /// can, and a chain of no segments is the open head alone.
    pub open_head: bool,
    /// Where the chain was merged into another, when it was: the
    /// segment, or the open head, that names the chain's last segment
    /// among its predecessors without continuing the chain.
    pub joined: Option<Joined>,
    /// Claims in the chain, the open head's included when it is part.
    pub claims: usize,
    /// When the chain's first claim was recorded.
    pub from: Option<Timestamp>,
    /// When the chain's last claim was recorded.
    pub to: Option<Timestamp>,
}

/// What a merged chain was merged into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Joined {
    /// A sealed segment, by name.
    Segment(String),
    /// The open head.
    Head,
}

/// Why a chain begins where it does, when it is not the first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cause {
    /// The segment names nothing as sealed before it: the head was lost
    /// and a fresh one begun, its claims with it.
    HeadLost,
    /// The segment names one the store does not hold: that segment is
    /// gone, and this is its name.
    SegmentLost(String),
    /// The segment names one the store holds but cannot read back: this
    /// is its name, and it is a finding of its own.
    SegmentUnreadable(String),
}

/// A break in the chain: where the record stops hanging together, and
/// what is known about why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Break {
    /// The last segment of the chain before the break.
    pub after: String,
    /// The first segment of the chain after it — `None` when that chain
    /// is the open head alone.
    pub before: Option<String>,
    /// What the segment after the break says of it.
    pub cause: Cause,
    /// When the last claim before the break was recorded.
    pub from: Option<Timestamp>,
    /// When the first claim after the break was recorded.
    pub to: Option<Timestamp>,
    /// Whether these are surely the break's two ends. Chains stand in
    /// the order of their first claims, and the one the open head
    /// continues stands last whatever its claims say; two chains whose
    /// first claims share a second could stand either way round, and a
    /// break next to such a pair might join the wrong ends.
    pub sure: bool,
}

impl Break {
    /// Whether a mend can close the break: it can stand in for what is
    /// gone, not for what is held and damaged — that is a finding of
    /// its own, and a mend over it would say the loss is understood
    /// when it is not. And only where the two ends are
    /// [`sure`](Break::sure): a mend between the wrong ends is worse
    /// than the break.
    #[must_use]
    pub fn mendable(&self) -> bool {
        self.sure && !matches!(self.cause, Cause::SegmentUnreadable(_))
    }
}

/// A break a mend has closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mended {
    /// The mend itself.
    pub mend: String,
    /// The segment the mend follows: the end before the break.
    pub previous: String,
    /// The segment the mend stands in front of; `None` when the open
    /// head names it.
    pub before: Option<String>,
    /// The segment the mend stands in for, when its name was known.
    pub replaces: Option<String>,
}

/// One readable segment, as the chain walk needs it.
struct Link {
    previous: Vec<String>,
    mend: Option<(Option<String>, Option<String>)>,
    claims: usize,
    first: Option<Timestamp>,
    last: Option<Timestamp>,
}

/// Audit the claim log: fixity of every sealed segment, every line of
/// every segment that is true to its name, the open head last, and the
/// chain the headers form held against what the store holds.
///
/// # Errors
///
/// [`Error::Store`](crate::Error::Store) when the claims store cannot
/// be walked at all; what a single segment or the head has to answer
/// for lands in the report instead.
pub fn audit_log(log: &Log) -> Result<LogAudit> {
    let mut report = LogAudit {
        segments: 0,
        claims: 0,
        damaged: Vec::new(),
        unreadable: Vec::new(),
        broken: Vec::new(),
        head_broken: None,
        predecessor_missing: Vec::new(),
        head_predecessor_missing: None,
        chains: Vec::new(),
        breaks: Vec::new(),
        mended: Vec::new(),
        restored: Vec::new(),
        idle_mends: Vec::new(),
        looped: Vec::new(),
        referenced: BTreeSet::new(),
    };
    // Every name the store holds, damaged or not — what a predecessor is
    // held against. A damaged segment is still held: it is a finding once
    // already, and gone on top would count the same wound twice.
    let mut held = BTreeSet::new();
    // The readable segments, by name, with what the chain walk needs.
    let mut links: BTreeMap<String, Link> = BTreeMap::new();
    let store = log.store();
    for entry in store.entries() {
        let entry = entry?;
        let name = entry.digest().as_str().to_string();
        report.segments += 1;
        held.insert(name.clone());
        match store.verify(&entry) {
            Ok(true) => {}
            Ok(false) => {
                report.damaged.push(name);
                continue;
            }
            Err(error) => {
                report.unreadable.push((name, error.to_string()));
                continue;
            }
        }
        match log.contents(entry.digest()) {
            Ok(contents) => {
                report.claims += contents.claims().len();
                for claim in contents.claims() {
                    reference(claim, &mut report.referenced);
                }
                links.insert(name, link(&contents));
            }
            Err(error) => report.broken.push((name, error.spelled())),
        }
    }
    let open = match log.head_contents() {
        Ok(contents) => {
            report.claims += contents.claims().len();
            for claim in contents.claims() {
                reference(claim, &mut report.referenced);
            }
            Some(link(&contents))
        }
        Err(error) => {
            report.head_broken = Some(error.spelled());
            None
        }
    };
    chain(&mut report, &held, &links, open.as_ref());
    Ok(report)
}

fn link(contents: &Contents) -> Link {
    let hex = |digest: &Digest| digest.as_str().to_string();
    Link {
        previous: contents.previous().iter().map(hex).collect(),
        mend: contents
            .mend()
            .map(|mend| (mend.before().map(hex), mend.replaces().map(hex))),
        claims: contents.claims().len(),
        first: contents.claims().first().map(|claim| claim.time().clone()),
        last: contents.claims().last().map(|claim| claim.time().clone()),
    }
}

/// Hang the readable segments together: mends applied where a break
/// stands, chains walked from every beginning, the breaks between them
/// named for what the segment after each says.
fn chain(
    report: &mut LogAudit,
    held: &BTreeSet<String>,
    links: &BTreeMap<String, Link>,
    open: Option<&Link>,
) {
    // Why a segment begins a chain, from the first segment it names as
    // sealed before it, or `None` when that is held and readable, so
    // nothing begins.
    let cause = |previous: Option<&String>| -> Option<Cause> {
        match previous {
            None => Some(Cause::HeadLost),
            Some(name) if !held.contains(name) => Some(Cause::SegmentLost(name.clone())),
            Some(name) if !links.contains_key(name) => Some(Cause::SegmentUnreadable(name.clone())),
            Some(_) => None,
        }
    };
    let head_previous = open.and_then(|open| open.previous.first().map(String::as_str));
    let mut pending: BTreeMap<&str, Cause> = links
        .iter()
        .filter_map(|(name, link)| cause(link.previous.first()).map(|cause| (name.as_str(), cause)))
        .collect();

    let (mut successor, joins) = follow(report, held, links, open);
    apply_mends(
        report,
        held,
        links,
        head_previous,
        &mut pending,
        &mut successor,
    );

    // A segment lost in front of a chain, as the chain's first segment
    // names it. A mended loss is no longer pending and is not counted.
    for (start, cause) in &pending {
        if let Cause::SegmentLost(previous) = cause {
            report
                .predecessor_missing
                .push(((*start).to_string(), previous.clone()));
        }
    }

    // Walk each chain from its beginning to where nothing follows. A
    // segment no walk from a beginning reaches follows something held
    // that is on no chain — a fork, which nothing this crate writes
    // leaves behind; it is listed as a chain of its own, not dropped.
    let mut chains: Vec<(Chain, Option<Cause>)> = Vec::new();
    let mut visited: BTreeSet<&str> = BTreeSet::new();
    let idle = |name: &String| {
        report.idle_mends.contains(name)
            || report.restored.iter().any(|mended| &mended.mend == name)
    };
    let starts: Vec<(&str, Option<Cause>)> = pending
        .iter()
        .map(|(start, cause)| (*start, Some(cause.clone())))
        .chain(
            links
                .keys()
                .filter(|name| !pending.contains_key(name.as_str()) && !idle(name))
                .map(|name| (name.as_str(), None)),
        )
        .collect();
    for (start, cause) in starts {
        if visited.contains(start) {
            continue;
        }
        let (chain, looped) = walk(
            start,
            links,
            &successor,
            &joins,
            head_previous,
            &mut visited,
        );
        if looped {
            // A chain that comes round to itself has no first segment
            // and no cause; the mends on it are what closed the circle.
            let mends = chain
                .segments
                .iter()
                .filter(|name| links[name.as_str()].mend.is_some())
                .cloned();
            report.looped.extend(mends);
            continue;
        }
        chains.push((chain, cause));
    }
    // Oldest first, by first claim; the chain the open head continues
    // last whatever its claims say, a chain of no claims at all before
    // it.
    chains.sort_by(|(a, _), (b, _)| {
        let key = |chain: &Chain| (chain.open_head, chain.from.is_none(), chain.from.clone());
        key(a)
            .cmp(&key(b))
            .then_with(|| a.segments.first().cmp(&b.segments.first()))
    });

    if let Some(open) = open {
        let cause = match open.previous.first() {
            None if held.is_empty() => None,
            previous => cause(previous),
        };
        with_head(report, &mut chains, open, cause);
    }
    report.predecessor_missing.sort();
    breaks_between(report, chains);
}

/// The open head into the chains: the end of the chain it continues,
/// or, naming nothing held, a chain of its own, last, with `cause`
/// saying why. In an archive with nothing sealed the head is where the
/// record begins.
fn with_head(
    report: &mut LogAudit,
    chains: &mut Vec<(Chain, Option<Cause>)>,
    open: &Link,
    cause: Option<Cause>,
) {
    if let Some((chain, _)) = chains.iter_mut().find(|(chain, _)| chain.open_head) {
        chain.claims += open.claims;
        if chain.from.is_none() {
            chain.from.clone_from(&open.first);
        }
        if open.last.is_some() {
            chain.to.clone_from(&open.last);
        }
        return;
    }
    if let Some(Cause::SegmentLost(previous)) = &cause {
        report.head_predecessor_missing = Some(previous.clone());
    }
    let chain = Chain {
        segments: Vec::new(),
        open_head: true,
        joined: None,
        claims: open.claims,
        from: open.first.clone(),
        to: open.last.clone(),
    };
    chains.push((chain, cause));
}

/// Who follows whom: a segment continues the chain of the first
/// segment it names, and every further segment it names has its chain
/// end there, merged in. A segment merged in that is not held is a loss
/// no mend can stand in for, counted here. A mend that stands in front
/// of a sealed segment is not in the map yet: it comes in only where it
/// closes a pending break.
fn follow<'a>(
    report: &mut LogAudit,
    held: &BTreeSet<String>,
    links: &'a BTreeMap<String, Link>,
    open: Option<&'a Link>,
) -> (BTreeMap<&'a str, &'a str>, BTreeMap<&'a str, Joined>) {
    let mut successor: BTreeMap<&str, &str> = BTreeMap::new();
    let mut joins: BTreeMap<&str, Joined> = BTreeMap::new();
    for (name, link) in links {
        let stands_before_sealed = matches!(&link.mend, Some((Some(_), _)));
        if let (Some(previous), false) = (link.previous.first(), stands_before_sealed) {
            successor.insert(previous, name);
        }
        for merged in link.previous.iter().skip(1) {
            if held.contains(merged) {
                joins.insert(merged, Joined::Segment(name.clone()));
            } else {
                report
                    .predecessor_missing
                    .push((name.clone(), merged.clone()));
            }
        }
    }
    if let Some(open) = open {
        for merged in open.previous.iter().skip(1) {
            if held.contains(merged) {
                joins.insert(merged, Joined::Head);
            } else {
                report
                    .predecessor_missing
                    .push((HEAD.to_string(), merged.clone()));
            }
        }
    }
    (successor, joins)
}

/// The name the open head goes by where a sealed segment's name is
/// expected: in [`LogAudit::predecessor_missing`], for a segment the
/// head names as merged in that is not held.
pub const HEAD: &str = "head";

/// Apply every mend: one in front of a pending break closes it — the
/// end before the break is then followed by the mend, and the mend by
/// the segment after — and one the open head names, or the segment the
/// head has since become, closed a break in front of the head. A mend
/// that closes nothing stands idle.
fn apply_mends<'a>(
    report: &mut LogAudit,
    held: &BTreeSet<String>,
    links: &'a BTreeMap<String, Link>,
    head_previous: Option<&str>,
    pending: &mut BTreeMap<&'a str, Cause>,
    successor: &mut BTreeMap<&'a str, &'a str>,
) {
    for (name, link) in links {
        let Some((before, replaces)) = &link.mend else {
            continue;
        };
        let Some(previous) = link.previous.first() else {
            report.idle_mends.push(name.clone());
            continue;
        };
        let closes = match before {
            Some(before) => {
                let closes = pending.remove(before.as_str()).is_some();
                if closes {
                    successor.insert(previous, name);
                    successor.insert(name, before);
                }
                closes
            }
            None => successor.contains_key(name.as_str()) || head_previous == Some(name),
        };
        let mended = Mended {
            mend: name.clone(),
            previous: previous.clone(),
            before: before.clone(),
            replaces: replaces.clone(),
        };
        if closes {
            report.mended.push(mended);
        } else if replaces.as_ref().is_some_and(|lost| held.contains(lost)) {
            report.restored.push(mended);
        } else {
            report.idle_mends.push(name.clone());
        }
    }
    report.idle_mends.sort();
}

/// Whether two chains could stand either way round: neither is the
/// one the open head continues, and their first claims share a second
/// — or one of them has no claim to date it by.
fn tie(a: &Chain, b: &Chain) -> bool {
    if a.open_head || b.open_head {
        return false;
    }
    match (&a.from, &b.from) {
        (Some(a), Some(b)) => a == b,
        _ => true,
    }
}

/// The breaks between the chains, into the report with the chains
/// themselves. A chain whose end nothing follows is an open end; the
/// next chain in time order that begins with a cause is paired with
/// it as a break. A chain that begins with a cause while no end is
/// open is a beginning of the record: the archive's first chain, or a
/// line merged in whole. A chain that ends merged into another, or at
/// the open head, leaves no end open.
fn breaks_between(report: &mut LogAudit, chains: Vec<(Chain, Option<Cause>)>) {
    let mut open: Option<usize> = None;
    for index in 0..chains.len() {
        let (later, cause) = &chains[index];
        if let (Some(earlier), Some(cause)) = (open, cause) {
            let (earlier_chain, _) = &chains[earlier];
            if let Some(after) = earlier_chain.segments.last() {
                // The ends are sure when the earlier chain is surely the
                // one right before the later: no tie with its own
                // predecessor either, or it might be the one standing
                // here.
                let sure = !tie(earlier_chain, later)
                    && earlier
                        .checked_sub(1)
                        .is_none_or(|before| !tie(&chains[before].0, earlier_chain));
                report.breaks.push(Break {
                    after: after.clone(),
                    before: later.segments.first().cloned(),
                    cause: cause.clone(),
                    from: earlier_chain.to.clone(),
                    to: later.from.clone(),
                    sure,
                });
            }
        }
        open = if later.open_head || later.joined.is_some() {
            None
        } else {
            Some(index)
        };
    }
    report.chains = chains.into_iter().map(|(chain, _)| chain).collect();
}

/// One chain from `start`: along `successor` until nothing follows,
/// marking each segment visited on the way — and whether the walk came
/// round to a segment already walked, which no chain does. A chain
/// that nothing follows may end merged into another, as `joins` says.
fn walk<'a>(
    start: &'a str,
    links: &'a BTreeMap<String, Link>,
    successor: &BTreeMap<&'a str, &'a str>,
    joins: &BTreeMap<&'a str, Joined>,
    head_previous: Option<&str>,
    visited: &mut BTreeSet<&'a str>,
) -> (Chain, bool) {
    let mut chain = Chain {
        segments: Vec::new(),
        open_head: false,
        joined: None,
        claims: 0,
        from: None,
        to: None,
    };
    let mut at = start;
    loop {
        if !visited.insert(at) {
            // Back on a segment already walked: the chain runs in a
            // circle.
            return (chain, true);
        }
        let link = &links[at];
        chain.segments.push(at.to_string());
        chain.claims += link.claims;
        if chain.from.is_none() {
            chain.from.clone_from(&link.first);
        }
        if link.last.is_some() {
            chain.to.clone_from(&link.last);
        }
        if head_previous == Some(at) {
            chain.open_head = true;
        }
        let Some(next) = successor.get(at) else {
            chain.joined = joins.get(at).cloned();
            return (chain, false);
        };
        at = next;
    }
}

/// What one claim points at: its subject always, and — for an attribute
/// whose values are references — the subject its value names.
fn reference(claim: &Claim, referenced: &mut BTreeSet<String>) {
    referenced.insert(claim.subject().as_str().to_string());
    if LINKS.contains(&claim.attribute().as_str()) {
        if let Some(Value::String(text)) = claim.value() {
            if let Ok(subject) = Subject::parse(text) {
                referenced.insert(subject.as_str().to_string());
            }
        }
    }
}

/// The whole audit: both stores, the log, and the two held against each
/// other.
#[derive(Debug)]
pub struct Audit {
    /// What was taken in, each entry proved against its name.
    pub content: StoreAudit,
    /// What tools made, proved the same way.
    pub derived: StoreAudit,
    /// The record, read back whole.
    pub log: LogAudit,
    /// Subjects the claims speak of that no store holds — each one a
    /// loss, because absence has no innocent reading.
    pub missing: Vec<String>,
    /// Entries of `content/` no claim speaks of. An observation, not a
    /// finding.
    pub unrecorded_content: Vec<String>,
    /// Entries of `derived/` no claim speaks of. An observation, not a
    /// finding.
    pub unrecorded_derived: Vec<String>,
    /// Contents held by both stores, each with how its two copies fared.
    /// An observation, not a finding: what is wrong with either copy
    /// already stands as one on its store.
    pub twins: Vec<Twin>,
}

impl Audit {
    /// Hold the pieces against each other: what is spoken of must be
    /// held — by either store, a subject never says where it lies — and
    /// what is held ought to be spoken of.
    #[must_use]
    pub fn assemble(content: StoreAudit, derived: StoreAudit, log: LogAudit) -> Audit {
        let missing: Vec<String> = log
            .referenced
            .iter()
            .filter(|name| !content.held.contains_key(*name) && !derived.held.contains_key(*name))
            .cloned()
            .collect();
        let unrecorded = |store: &StoreAudit| -> Vec<String> {
            store
                .held
                .keys()
                .filter(|name| !log.referenced.contains(*name))
                .cloned()
                .collect()
        };
        let content_troubles = content.troubles();
        let derived_troubles = derived.troubles();
        let fixity = |troubles: &BTreeMap<&str, Fixity>, name: &str| {
            troubles.get(name).cloned().unwrap_or(Fixity::Sound)
        };
        let twins = content
            .held
            .iter()
            .filter(|(name, _)| derived.held.contains_key(*name))
            .map(|(name, digest)| Twin {
                digest: digest.clone(),
                content: fixity(&content_troubles, name),
                derived: fixity(&derived_troubles, name),
            })
            .collect();
        Audit {
            missing,
            unrecorded_content: unrecorded(&content),
            unrecorded_derived: unrecorded(&derived),
            twins,
            content,
            derived,
            log,
        }
    }

    /// How many findings stand — what the verdict and the exit code
    /// count. Observations are not among them.
    #[must_use]
    pub fn findings(&self) -> usize {
        let store = |report: &StoreAudit| report.damaged.len() + report.unreadable.len();
        store(&self.content)
            + store(&self.derived)
            + self.log.damaged.len()
            + self.log.unreadable.len()
            + self.log.broken.len()
            + usize::from(self.log.head_broken.is_some())
            + self.log.predecessor_missing.len()
            + usize::from(self.log.head_predecessor_missing.is_some())
            + self.log.heads_lost()
            + self.log.looped.len()
            + self.missing.len()
    }

    /// Whether the archive is sound: no finding stands. Observations
    /// may — soundness is about loss and damage, not tidiness.
    #[must_use]
    pub fn is_sound(&self) -> bool {
        self.findings() == 0
    }
}

#[cfg(test)]
mod tests {
    use immure::{Algorithm, Digest};
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::Archive;
    use crate::claim::{Attribute, Source, Timestamp};
    use crate::log::Mend;

    fn archive(dir: &TempDir) -> Archive {
        Archive::create(dir.path().join("archive"), Algorithm::Sha256).unwrap()
    }

    /// Bytes into the content store with one claim on the record — the
    /// smallest thing the audit calls whole.
    fn take(archive: &Archive, bytes: &[u8]) -> Subject {
        let (_, entry) = archive.content().add(bytes).unwrap();
        let subject = Subject::parse(entry.digest().as_str()).unwrap();
        record(archive, &subject, "file:size", json!(bytes.len()));
        subject
    }

    fn record(archive: &Archive, subject: &Subject, attribute: &str, value: Value) {
        record_at(archive, subject, attribute, value, "2026-09-06T12:00:00Z");
    }

    fn record_at(archive: &Archive, subject: &Subject, attribute: &str, value: Value, at: &str) {
        let claim = Claim::assert(
            subject.clone(),
            Attribute::parse(attribute).unwrap(),
            value,
            Timestamp::parse(at).unwrap(),
            Source::parse("test").unwrap(),
            crate::claim::Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
        )
        .unwrap();
        archive.log().append(&claim).unwrap();
    }

    /// As `take`, recorded at a chosen time — chains stand in the order
    /// of their first claims, so a test about their order needs claims
    /// that do not all share one second.
    fn take_at(archive: &Archive, bytes: &[u8], at: &str) -> Subject {
        let (_, entry) = archive.content().add(bytes).unwrap();
        let subject = Subject::parse(entry.digest().as_str()).unwrap();
        record_at(archive, &subject, "file:size", json!(bytes.len()), at);
        subject
    }

    fn run(archive: &Archive) -> Audit {
        Audit::assemble(
            audit_store(archive.content()).unwrap(),
            audit_store(archive.derived()).unwrap(),
            audit_log(archive.log()).unwrap(),
        )
    }

    /// Damage an entry in place — past the read-only mode the store put
    /// on it, which tampering does not politely honour.
    fn tamper(path: &std::path::Path, bytes: &[u8]) {
        let mut permissions = std::fs::metadata(path).unwrap().permissions();
        #[allow(
            clippy::permissions_set_readonly_false,
            reason = "the test plays the corruption"
        )]
        permissions.set_readonly(false);
        std::fs::set_permissions(path, permissions).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn a_sound_archive_has_nothing_to_report() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"kept for good");
        archive.log().seal().unwrap();
        take(&archive, b"and this one too");

        let audit = run(&archive);

        assert!(audit.is_sound());
        assert_eq!(audit.findings(), 0);
        assert_eq!(audit.content.checked, 2);
        assert_eq!(audit.log.segments, 1);
        assert_eq!(audit.log.claims, 2, "one sealed, one still in the head");
        assert!(audit.missing.is_empty());
        assert!(audit.unrecorded_content.is_empty());
        assert!(audit.unrecorded_derived.is_empty());
        assert_eq!(audit.log.chains.len(), 1, "one chain, the record whole");
        assert!(audit.log.chains[0].open_head);
        assert_eq!(audit.log.chains[0].claims, 2);
        assert!(audit.log.breaks.is_empty());
        assert!(audit.log.mended.is_empty());
        assert!(audit.log.predecessor_missing.is_empty());
        assert!(audit.log.head_predecessor_missing.is_none());
    }

    /// Delete a sealed segment's file outright — the loss the chain
    /// exists to show.
    fn lose(archive: &Archive, digest: &Digest) {
        let path = archive.log().store().find(digest).unwrap().unwrap();
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_lost_segment_is_found_by_the_one_sealed_after_it() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first");
        let first = archive.log().seal().unwrap().unwrap();
        take(&archive, b"second");
        let second = archive.log().seal().unwrap().unwrap();
        lose(&archive, first.digest());

        let audit = run(&archive);

        assert_eq!(
            audit.log.predecessor_missing,
            vec![(
                second.digest().as_str().to_string(),
                first.digest().as_str().to_string()
            )]
        );
        assert!(audit.log.head_predecessor_missing.is_none());
        assert_eq!(
            audit.log.chains.len(),
            1,
            "the second segment begins the one chain there is — nothing earlier is left to break from"
        );
        assert!(audit.log.breaks.is_empty());
        assert_eq!(audit.findings(), 1);
        assert!(!audit.is_sound());
        assert_eq!(
            audit.unrecorded_content.len(),
            1,
            "the blob the lost segment spoke of turns unrecorded — an observation, the loss is counted once"
        );
    }

    #[test]
    fn a_lost_last_segment_is_found_by_the_open_head() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"sealed and lost");
        let segment = archive.log().seal().unwrap().unwrap();
        lose(&archive, segment.digest());

        let audit = run(&archive);

        assert_eq!(
            audit.log.head_predecessor_missing.as_deref(),
            Some(segment.digest().as_str())
        );
        assert_eq!(audit.log.segments, 0);
        assert_eq!(audit.findings(), 1);
    }

    #[test]
    fn a_damaged_predecessor_is_held_and_counted_once() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"damaged later");
        let segment = archive.log().seal().unwrap().unwrap();
        let path = archive
            .log()
            .store()
            .find(segment.digest())
            .unwrap()
            .unwrap();
        tamper(&path, b"garbage");

        let audit = run(&archive);

        assert_eq!(audit.log.damaged.len(), 1);
        assert!(
            audit.log.head_predecessor_missing.is_none(),
            "damaged is still held — the head's predecessor answers"
        );
        assert_eq!(audit.findings(), 1);
    }

    /// The open head gone, as a restore from elsewhere or a stray `rm`
    /// leaves it: the next append begins one anew.
    fn lose_head(archive: &Archive) {
        std::fs::remove_file(archive.root().join("head.jsonl")).unwrap();
    }

    fn name(segment: &crate::Segment) -> String {
        segment.digest().as_str().to_string()
    }

    #[test]
    fn a_head_lost_and_sealed_anew_is_a_break_and_a_finding() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take_at(&archive, b"before the loss", "2026-09-01T00:00:00Z");
        let first = archive.log().seal().unwrap().unwrap();
        take_at(&archive, b"lost with the head", "2026-09-02T00:00:00Z");
        lose_head(&archive);
        take_at(&archive, b"after the loss", "2026-09-03T00:00:00Z");
        let anew = archive.log().seal().unwrap().unwrap();
        take_at(&archive, b"and on", "2026-09-04T00:00:00Z");

        let audit = run(&archive);

        assert_eq!(audit.log.chains.len(), 2);
        assert_eq!(audit.log.chains[0].segments, vec![name(&first)]);
        assert!(!audit.log.chains[0].open_head);
        assert_eq!(audit.log.chains[1].segments, vec![name(&anew)]);
        assert!(audit.log.chains[1].open_head);
        assert_eq!(
            audit.log.chains[1].claims, 2,
            "the head's claim counts to its chain"
        );
        assert_eq!(
            audit.log.breaks,
            vec![Break {
                after: name(&first),
                before: Some(name(&anew)),
                cause: Cause::HeadLost,
                from: Some(Timestamp::parse("2026-09-01T00:00:00Z").unwrap()),
                to: Some(Timestamp::parse("2026-09-03T00:00:00Z").unwrap()),
                sure: true,
            }]
        );
        assert!(audit.log.breaks[0].mendable());
        assert_eq!(audit.log.heads_lost(), 1);
        assert_eq!(audit.findings(), 1, "the head lost, once");
        assert_eq!(
            audit.unrecorded_content.len(),
            1,
            "the blob whose claim went with the head is held, spoken of by nothing — an observation"
        );
    }

    #[test]
    fn a_head_lost_and_not_yet_sealed_is_a_break_in_front_of_the_head() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"before the loss");
        let first = archive.log().seal().unwrap().unwrap();
        lose_head(&archive);
        take(&archive, b"after the loss");

        let audit = run(&archive);

        assert_eq!(audit.log.chains.len(), 2);
        assert!(
            audit.log.chains[1].segments.is_empty(),
            "the open head alone"
        );
        assert!(audit.log.chains[1].open_head);
        assert_eq!(audit.log.breaks.len(), 1);
        assert_eq!(audit.log.breaks[0].after, name(&first));
        assert_eq!(audit.log.breaks[0].before, None);
        assert_eq!(audit.log.breaks[0].cause, Cause::HeadLost);
        assert_eq!(audit.findings(), 1);
    }

    #[test]
    fn a_mend_in_front_of_a_sealed_segment_closes_the_break() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"before the loss");
        let first = archive.log().seal().unwrap().unwrap();
        lose_head(&archive);
        take(&archive, b"after the loss");
        let anew = archive.log().seal().unwrap().unwrap();
        let mend = archive
            .log()
            .mend(
                first.digest(),
                &Mend::new(Some(anew.digest().clone()), None),
            )
            .unwrap();

        let audit = run(&archive);

        assert!(audit.is_sound());
        assert_eq!(audit.log.chains.len(), 1);
        assert_eq!(
            audit.log.chains[0].segments,
            vec![name(&first), name(&mend), name(&anew)],
            "the mend is a link of the chain"
        );
        assert!(audit.log.chains[0].open_head);
        assert!(audit.log.breaks.is_empty());
        assert_eq!(
            audit.log.mended,
            vec![Mended {
                mend: name(&mend),
                previous: name(&first),
                before: Some(name(&anew)),
                replaces: None,
            }]
        );
        assert!(audit.log.idle_mends.is_empty());
    }

    #[test]
    fn a_mend_the_head_names_closes_the_break_in_front_of_it() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"before the loss");
        let first = archive.log().seal().unwrap().unwrap();
        lose_head(&archive);
        take(&archive, b"after the loss");
        let mend = archive
            .log()
            .mend(first.digest(), &Mend::default())
            .unwrap();
        archive
            .log()
            .head_follows(std::slice::from_ref(mend.digest()))
            .unwrap();

        let audit = run(&archive);

        assert!(audit.is_sound());
        assert_eq!(audit.log.chains.len(), 1);
        assert_eq!(
            audit.log.chains[0].segments,
            vec![name(&first), name(&mend)]
        );
        assert!(audit.log.chains[0].open_head);
        assert_eq!(audit.log.mended.len(), 1);
        assert_eq!(audit.log.mended[0].before, None);

        // Sealed later, the head's segment names the mend as any
        // segment names its predecessor, and the mend stays applied.
        archive.log().seal().unwrap().unwrap();
        let audit = run(&archive);
        assert!(audit.is_sound());
        assert_eq!(audit.log.chains.len(), 1);
        assert_eq!(audit.log.mended.len(), 1);
    }

    #[test]
    fn a_mend_stands_in_for_a_lost_segment_and_the_record_keeps_its_name() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first");
        let first = archive.log().seal().unwrap().unwrap();
        take(&archive, b"second, lost");
        let second = archive.log().seal().unwrap().unwrap();
        take(&archive, b"third");
        let third = archive.log().seal().unwrap().unwrap();
        lose(&archive, second.digest());
        let mend = archive
            .log()
            .mend(
                first.digest(),
                &Mend::new(Some(third.digest().clone()), Some(second.digest().clone())),
            )
            .unwrap();

        let audit = run(&archive);

        assert!(
            audit.log.predecessor_missing.is_empty(),
            "the mend stands in for what the third names"
        );
        assert_eq!(audit.log.chains.len(), 1);
        assert_eq!(
            audit.log.chains[0].segments,
            vec![name(&first), name(&mend), name(&third)]
        );
        assert_eq!(
            audit.log.mended[0].replaces.as_deref(),
            Some(second.digest().as_str())
        );
        assert_eq!(
            archive.log().contents(third.digest()).unwrap().previous(),
            std::slice::from_ref(second.digest()),
            "nothing sealed was rewritten"
        );
        assert_eq!(
            audit.findings(),
            0,
            "the lost segment's claim spoke of a blob still held — unrecorded now, an observation"
        );
        assert!(audit.is_sound());
    }

    #[test]
    fn a_mend_in_front_of_a_segment_needing_none_stands_idle() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first");
        let first = archive.log().seal().unwrap().unwrap();
        take(&archive, b"second");
        let second = archive.log().seal().unwrap().unwrap();
        let idle = archive
            .log()
            .mend(
                first.digest(),
                &Mend::new(Some(second.digest().clone()), None),
            )
            .unwrap();

        let audit = run(&archive);

        assert!(audit.is_sound());
        assert_eq!(audit.log.chains.len(), 1);
        assert_eq!(
            audit.log.chains[0].segments,
            vec![name(&first), name(&second)],
            "an idle mend is on no chain"
        );
        assert_eq!(audit.log.idle_mends, vec![name(&idle)]);
        assert!(audit.log.mended.is_empty());
    }

    #[test]
    fn a_break_behind_a_damaged_segment_cannot_be_mended() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take_at(&archive, b"first", "2026-09-01T00:00:00Z");
        let first = archive.log().seal().unwrap().unwrap();
        take_at(&archive, b"second", "2026-09-02T00:00:00Z");
        let second = archive.log().seal().unwrap().unwrap();
        take_at(&archive, b"third", "2026-09-03T00:00:00Z");
        let third = archive.log().seal().unwrap().unwrap();
        let path = archive
            .log()
            .store()
            .find(second.digest())
            .unwrap()
            .unwrap();
        tamper(&path, b"garbage");

        let audit = run(&archive);

        assert_eq!(audit.log.chains.len(), 2);
        assert_eq!(audit.log.breaks.len(), 1);
        assert_eq!(audit.log.breaks[0].after, name(&first));
        assert_eq!(
            audit.log.breaks[0].before.as_deref(),
            Some(third.digest().as_str())
        );
        assert_eq!(
            audit.log.breaks[0].cause,
            Cause::SegmentUnreadable(name(&second))
        );
        assert!(!audit.log.breaks[0].mendable());
        assert_eq!(audit.findings(), 1, "the damage, and nothing on top of it");
    }

    #[test]
    fn the_chain_the_head_continues_stands_last_whatever_the_second_says() {
        // Every claim in one second, as a fast ingest leaves them: the
        // order of the chains must come from the head, not the digests.
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first");
        let first = archive.log().seal().unwrap().unwrap();
        take(&archive, b"second, lost");
        let second = archive.log().seal().unwrap().unwrap();
        take(&archive, b"third");
        let third = archive.log().seal().unwrap().unwrap();
        lose(&archive, second.digest());

        let audit = run(&archive);

        assert_eq!(audit.log.chains.len(), 2);
        assert_eq!(audit.log.chains[0].segments, vec![name(&first)]);
        assert_eq!(audit.log.chains[1].segments, vec![name(&third)]);
        assert!(audit.log.chains[1].open_head);
        assert_eq!(audit.log.breaks.len(), 1);
        assert_eq!(audit.log.breaks[0].after, name(&first));
        assert_eq!(
            audit.log.breaks[0].before.as_deref(),
            Some(third.digest().as_str())
        );
        assert_eq!(audit.log.breaks[0].cause, Cause::SegmentLost(name(&second)));
        assert!(
            audit.log.breaks[0].sure,
            "two chains, and the head says which is last"
        );
    }

    #[test]
    fn chains_that_share_a_second_leave_their_breaks_unsure() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first");
        archive.log().seal().unwrap().unwrap();
        lose_head(&archive);
        take(&archive, b"second beginning, same second");
        archive.log().seal().unwrap().unwrap();
        lose_head(&archive);
        take_at(&archive, b"third beginning, later", "2026-09-07T00:00:00Z");

        let audit = run(&archive);

        assert_eq!(audit.log.chains.len(), 3);
        assert_eq!(audit.log.breaks.len(), 2);
        assert!(
            !audit.log.breaks[0].sure,
            "the first two chains could stand either way round"
        );
        assert!(
            !audit.log.breaks[1].sure,
            "so which of them stands before the head is not sure either"
        );
        assert!(!audit.log.breaks[1].mendable());
        assert_eq!(
            audit.log.heads_lost(),
            2,
            "unsure or not, two heads were lost"
        );
    }

    #[test]
    fn a_lost_segment_restored_leaves_its_mend_standing_as_a_record() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take_at(&archive, b"first", "2026-09-01T00:00:00Z");
        let first = archive.log().seal().unwrap().unwrap();
        take_at(&archive, b"second", "2026-09-02T00:00:00Z");
        let second = archive.log().seal().unwrap().unwrap();
        take_at(&archive, b"third", "2026-09-03T00:00:00Z");
        let third = archive.log().seal().unwrap().unwrap();
        let path = archive
            .log()
            .store()
            .find(second.digest())
            .unwrap()
            .unwrap();
        let backup = dir.path().join("backup");
        std::fs::rename(&path, &backup).unwrap();
        let mend = archive
            .log()
            .mend(
                first.digest(),
                &Mend::new(Some(third.digest().clone()), Some(second.digest().clone())),
            )
            .unwrap();
        assert_eq!(run(&archive).log.mended.len(), 1);
        std::fs::rename(&backup, &path).unwrap();

        let audit = run(&archive);

        assert!(audit.is_sound());
        assert_eq!(audit.log.chains.len(), 1);
        assert_eq!(
            audit.log.chains[0].segments,
            vec![name(&first), name(&second), name(&third)],
            "the chain runs through the segment again, not the mend"
        );
        assert!(audit.log.mended.is_empty());
        assert_eq!(audit.log.restored.len(), 1);
        assert_eq!(audit.log.restored[0].mend, name(&mend));
        assert!(audit.log.idle_mends.is_empty());
    }

    #[test]
    fn a_mend_that_closes_the_chain_into_a_circle_is_a_finding() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take_at(&archive, b"first", "2026-09-01T00:00:00Z");
        let first = archive.log().seal().unwrap().unwrap();
        take_at(&archive, b"second", "2026-09-02T00:00:00Z");
        let second = archive.log().seal().unwrap().unwrap();
        // A mend from the last segment back to the first: the first names
        // no predecessor, so the mend closes that "break" — into a ring.
        let mend = archive
            .log()
            .mend(
                second.digest(),
                &Mend::new(Some(first.digest().clone()), None),
            )
            .unwrap();

        let audit = run(&archive);

        assert_eq!(audit.log.looped, vec![name(&mend)]);
        assert_eq!(
            audit.log.chains.len(),
            1,
            "a ring is no chain — the open head stands alone"
        );
        assert!(audit.log.chains[0].segments.is_empty());
        assert!(audit.log.breaks.is_empty());
        assert!(!audit.is_sound());
        assert_eq!(audit.findings(), 1);
    }

    #[test]
    fn bytes_no_longer_true_to_their_name_are_damaged() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let subject = take(&archive, b"original bytes");
        let digest = Digest::parse(subject.as_str()).unwrap();
        let path = archive.content().find(&digest).unwrap().unwrap();
        tamper(&path, b"tampered");

        let audit = run(&archive);

        assert_eq!(audit.content.damaged, vec![subject.as_str().to_string()]);
        assert!(!audit.is_sound());
        assert_eq!(
            audit.findings(),
            1,
            "damaged is still held — one finding, not damaged and missing both"
        );
        assert!(audit.missing.is_empty());
    }

    #[test]
    fn what_the_claims_speak_of_must_be_held() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let held = take(&archive, b"held");
        let ghost = Subject::parse(&"ab".repeat(32)).unwrap();
        record(&archive, &ghost, "user:tag", json!("gone"));
        let linked = Subject::parse(&"cd".repeat(32)).unwrap();
        record(&archive, &held, "prov:origin", json!(linked.as_str()));
        // A string value outside the link attributes is words, not a name.
        record(&archive, &held, "user:comment", json!(&"ef".repeat(32)));

        let audit = run(&archive);

        assert_eq!(
            audit.missing,
            vec![ghost.as_str().to_string(), linked.as_str().to_string()],
            "a claim's subject and a link's value are both spoken of"
        );
        assert_eq!(audit.findings(), 2);
    }

    #[test]
    fn held_bytes_no_claim_speaks_of_are_noted_not_found() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"recorded");
        let (_, stray) = archive.content().add(b"stray").unwrap();

        let audit = run(&archive);

        assert!(audit.is_sound(), "an observation is not a finding");
        assert_eq!(
            audit.unrecorded_content,
            vec![stray.digest().as_str().to_string()]
        );
    }

    #[test]
    fn a_subject_held_by_either_store_is_held() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let (_, entry) = archive.derived().add(b"a derived file").unwrap();
        let subject = Subject::parse(entry.digest().as_str()).unwrap();
        record(&archive, &subject, "file:mime", json!("text/plain"));

        let audit = run(&archive);

        assert!(
            audit.missing.is_empty(),
            "a subject never says where it lies"
        );
        assert!(audit.is_sound());
        assert!(audit.unrecorded_derived.is_empty());
        assert!(audit.twins.is_empty());
    }

    #[test]
    fn a_content_held_by_both_stores_is_a_twin_and_not_a_finding() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let subject = take(&archive, b"an attachment, later taken in");
        archive
            .derived()
            .add(b"an attachment, later taken in")
            .unwrap();

        let audit = run(&archive);

        assert!(audit.is_sound());
        assert_eq!(
            audit.twins,
            vec![Twin {
                digest: Digest::parse(subject.as_str()).unwrap(),
                content: Fixity::Sound,
                derived: Fixity::Sound,
            }]
        );
        assert!(!audit.twins[0].repairable());
    }

    #[test]
    fn a_twin_carries_how_each_copy_fared() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"sound where it was won");
        let (_, derived) = archive.derived().add(b"sound where it was won").unwrap();
        let content = archive
            .content()
            .find(derived.digest())
            .unwrap()
            .expect("taken in");
        tamper(&content, b"sound where it was won?");

        let audit = run(&archive);

        assert_eq!(audit.findings(), 1, "the damage is the content store's");
        assert_eq!(audit.twins.len(), 1);
        assert_eq!(audit.twins[0].content, Fixity::Damaged);
        assert_eq!(audit.twins[0].derived, Fixity::Sound);
        assert!(audit.twins[0].repairable());
    }

    #[test]
    fn a_segment_that_will_not_read_back_is_named() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let (_, entry) = archive.log().store().add(b"not a header\n").unwrap();
        std::fs::write(
            archive.root().join("head.jsonl"),
            "{\"ossuary-segment\":1}\nnot a claim\n",
        )
        .unwrap();

        let audit = run(&archive);

        assert_eq!(audit.log.broken.len(), 1);
        assert_eq!(audit.log.broken[0].0, entry.digest().as_str());
        assert!(audit.log.head_broken.is_some());
        assert_eq!(audit.findings(), 2);
    }

    #[test]
    fn a_damaged_segment_is_one_finding_not_two() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"sealed away");
        let segment = archive.log().seal().unwrap().unwrap();
        let path = archive
            .log()
            .store()
            .find(segment.digest())
            .unwrap()
            .unwrap();
        tamper(&path, b"garbage");

        let audit = run(&archive);

        assert_eq!(
            audit.log.damaged,
            vec![segment.digest().as_str().to_string()]
        );
        assert!(
            audit.log.broken.is_empty(),
            "a damaged segment is not parsed"
        );
        assert_eq!(audit.log.claims, 0, "its claims are lost with it");
        assert_eq!(
            audit.findings(),
            1,
            "the blob it spoke of turns unrecorded, an observation, not a second finding"
        );
    }

    /// A sealed segment written straight into the claims store, as a
    /// second archive's segments arrive in a merge: the header names
    /// `previous`, the claims follow.
    fn sealed(archive: &Archive, previous: &[&Digest], claims: &[Claim]) -> Digest {
        let mut text = if previous.is_empty() {
            "{\"ossuary-segment\":1}\n".to_string()
        } else {
            let names: Vec<String> = previous.iter().map(|d| format!("\"{d}\"")).collect();
            format!(
                "{{\"ossuary-segment\":1,\"previous\":[{}]}}\n",
                names.join(",")
            )
        };
        for claim in claims {
            text.push_str(&claim.to_line());
            text.push('\n');
        }
        let (_, entry) = archive.log().store().add(text.as_bytes()).unwrap();
        entry.digest().clone()
    }

    /// One claim about bytes held by the content store, recorded at
    /// `at` without touching the open head.
    fn held_claim(archive: &Archive, bytes: &[u8], at: &str) -> Claim {
        let (_, entry) = archive.content().add(bytes).unwrap();
        Claim::assert(
            Subject::parse(entry.digest().as_str()).unwrap(),
            Attribute::parse("file:size").unwrap(),
            json!(bytes.len()),
            Timestamp::parse(at).unwrap(),
            Source::parse("test").unwrap(),
            crate::claim::Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn a_line_merged_in_is_a_whole_chain_and_no_break() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take_at(&archive, b"own first", "2026-09-05T00:00:00Z");
        let own = archive.log().seal().unwrap().unwrap();
        // The other archive's line, older than this one's.
        let b1 = sealed(
            &archive,
            &[],
            &[held_claim(
                &archive,
                b"merged first",
                "2026-09-01T00:00:00Z",
            )],
        );
        let b2 = sealed(
            &archive,
            &[&b1],
            &[held_claim(
                &archive,
                b"merged second",
                "2026-09-02T00:00:00Z",
            )],
        );
        archive
            .log()
            .head_follows(&[own.digest().clone(), b2.clone()])
            .unwrap();
        take_at(&archive, b"after the merge", "2026-09-06T00:00:00Z");

        let audit = run(&archive);

        assert!(audit.is_sound());
        assert_eq!(audit.log.chains.len(), 2);
        assert_eq!(
            audit.log.chains[0].segments,
            vec![b1.as_str().to_string(), b2.as_str().to_string()],
            "the merged line, oldest, stands first"
        );
        assert_eq!(audit.log.chains[0].joined, Some(Joined::Head));
        assert!(!audit.log.chains[0].open_head);
        assert_eq!(audit.log.chains[1].segments, vec![name(&own)]);
        assert!(audit.log.chains[1].open_head);
        assert_eq!(audit.log.chains[1].joined, None);
        assert!(audit.log.breaks.is_empty());
        assert_eq!(audit.log.heads_lost(), 0);
        assert_eq!(audit.log.claims, 4);

        // Sealed, the head's segment names both, and the merged line
        // ends in that segment.
        let sealed = archive.log().seal().unwrap().unwrap();
        let audit = run(&archive);
        assert!(audit.is_sound());
        assert_eq!(audit.log.chains.len(), 2);
        assert_eq!(
            audit.log.chains[0].joined,
            Some(Joined::Segment(name(&sealed)))
        );
        assert_eq!(
            audit.log.chains[1].segments,
            vec![name(&own), name(&sealed)],
            "the own line continues through the segment that merged"
        );
        assert!(audit.log.breaks.is_empty());
    }

    #[test]
    fn a_segment_merged_in_that_is_not_held_is_a_loss() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"own");
        let own = archive.log().seal().unwrap().unwrap();
        let ghost = Digest::parse(&"cd".repeat(32)).unwrap();
        archive
            .log()
            .head_follows(&[own.digest().clone(), ghost.clone()])
            .unwrap();

        let audit = run(&archive);

        assert_eq!(
            audit.log.predecessor_missing,
            vec![(HEAD.to_string(), ghost.as_str().to_string())]
        );
        assert!(audit.log.head_predecessor_missing.is_none());
        assert_eq!(audit.findings(), 1);
        assert_eq!(audit.log.chains.len(), 1, "the own line, whole");
        assert!(audit.log.breaks.is_empty());
    }

    #[test]
    fn a_loss_inside_a_merged_line_is_a_break_of_that_line() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take_at(&archive, b"own", "2026-09-05T00:00:00Z");
        let own = archive.log().seal().unwrap().unwrap();
        let b1 = sealed(
            &archive,
            &[],
            &[held_claim(&archive, b"b1", "2026-09-01T00:00:00Z")],
        );
        let b2 = sealed(
            &archive,
            &[&b1],
            &[held_claim(&archive, b"b2", "2026-09-02T00:00:00Z")],
        );
        let b3 = sealed(
            &archive,
            &[&b2],
            &[held_claim(&archive, b"b3", "2026-09-03T00:00:00Z")],
        );
        lose(&archive, &b2);
        archive
            .log()
            .head_follows(&[own.digest().clone(), b3.clone()])
            .unwrap();

        let audit = run(&archive);

        assert_eq!(audit.log.chains.len(), 3);
        assert_eq!(audit.log.chains[0].segments, vec![b1.as_str().to_string()]);
        assert_eq!(audit.log.chains[1].segments, vec![b3.as_str().to_string()]);
        assert_eq!(audit.log.chains[1].joined, Some(Joined::Head));
        assert!(audit.log.chains[2].open_head);
        assert_eq!(
            audit.log.breaks,
            vec![Break {
                after: b1.as_str().to_string(),
                before: Some(b3.as_str().to_string()),
                cause: Cause::SegmentLost(b2.as_str().to_string()),
                from: Some(Timestamp::parse("2026-09-01T00:00:00Z").unwrap()),
                to: Some(Timestamp::parse("2026-09-03T00:00:00Z").unwrap()),
                sure: true,
            }],
            "the break is within the merged line; the own line's first segment is no break"
        );
        assert_eq!(
            audit.log.predecessor_missing,
            vec![(b3.as_str().to_string(), b2.as_str().to_string())]
        );
        assert_eq!(audit.log.heads_lost(), 0);
        assert_eq!(audit.findings(), 1);

        let mend = crate::mend::mend(archive.log(), &audit.log.breaks[0])
            .unwrap()
            .expect("mendable");
        let audit = run(&archive);
        assert!(audit.is_sound());
        assert_eq!(audit.log.chains.len(), 2);
        assert_eq!(
            audit.log.chains[0].segments,
            vec![
                b1.as_str().to_string(),
                name(&mend),
                b3.as_str().to_string()
            ]
        );
        assert_eq!(audit.log.chains[0].joined, Some(Joined::Head));
    }
}
