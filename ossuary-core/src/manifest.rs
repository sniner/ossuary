//! Per-segment manifests: what a sealed segment holds, without opening it.
//!
//! A manifest is `cache/`'s memo about one sealed segment: how many claims
//! it holds, when its first claim was recorded, the range of claim times,
//! the namespaces that occur, and a bloom filter over the subjects spoken
//! of. Everything in it is a fold over the segment's own claims —
//! recomputable by this crate alone, for good — which is what makes it
//! cache and never truth. And because a sealed segment is immutable, its
//! manifest never goes stale: written once at sealing time, it can only be
//! absent, never outdated.
//!
//! Reading is lenient to match. A manifest that is missing, will not parse,
//! carries another version, or does not answer for the segment it was asked
//! about comes back as absent, and the asker falls back to the segment
//! itself — usually filing a fresh manifest in passing. Nothing under
//! `cache/` is ever load-bearing, so nothing here refuses; the worst a
//! broken drawer can cost is the walk that fills it again.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use immure::Digest;
use serde::{Deserialize, Serialize};

use crate::claim::{Claim, Subject, Timestamp};
use crate::error::{Error, Result};

/// The manifest form this build writes. Any other form read is treated as
/// absent and rebuilt — a cache is never refused, only failed to be helped
/// by, so unlike the segment generation this number guards nothing.
const VERSION: u32 = 1;

/// The one hash the bloom filter is built on, named in the file so the
/// bits cannot be probed with the wrong questions.
const HASH: &str = "fnv1a64-splitmix64";

/// Bloom sizing: ten bits per distinct subject and seven probes put the
/// false-positive rate around one percent; sixty-four bits is the floor,
/// so even a filter over nothing has a shape.
const BITS_PER_SUBJECT: usize = 10;
const PROBES: u32 = 7;
const FEWEST_BITS: usize = 64;

/// FNV-1a, 64 bit — Fowler, Noll and Vo's hash, begun 1991 as review
/// comments to the POSIX committee. The 1a variant, these constants and
/// the test vectors the tests below assert are specified in the IETF
/// draft `draft-eastlake-fnv`. Boring, documented everywhere, and
/// dependency-free.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Tweaks the offset basis so the second hash answers independently of the
/// first; the two together stride the double-hashing probes.
const SECOND_BASIS: u64 = 0x9e37_79b9_7f4a_7c15;

fn fnv1a(bytes: &[u8], basis: u64) -> u64 {
    let mut hash = basis;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// splitmix64's finisher, run over each FNV hash before it is masked.
///
/// FNV's low-order bits disperse poorly — subjects whose bytes differ in
/// compensating ways collide modulo a small power of two, and the probe
/// mask keeps exactly those bits. The mixer avalanches every input bit
/// across the whole word, so the mask sees all of the hash, not its
/// weakest corner. Found by this module's own tests, whose fixture
/// subjects differ in just such a compensating way.
///
/// Borrowed whole, shifts and multipliers alike: `MurmurHash3`'s 64-bit
/// finalizer as improved by David Stafford ("Better Bit Mixing", 2011 —
/// these are his Mix13 constants), adopted by splitmix64 and Java's
/// `SplittableRandom` (Steele, Lea and Flood, "Fast Splittable
/// Pseudorandom Number Generators", 2014).
fn mixed(mut hash: u64) -> u64 {
    hash ^= hash >> 30;
    hash = hash.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    hash ^= hash >> 27;
    hash = hash.wrapping_mul(0x94d0_49bb_1331_11eb);
    hash ^= hash >> 31;
    hash
}

/// The probe positions for one subject: double hashing, stride forced odd
/// so every probe sequence walks the whole power-of-two table.
///
/// That two hashes may simulate all seven — probe `i` at `h1 + i·h2` —
/// without hurting the false-positive rate is Kirsch and Mitzenmacher,
/// "Less Hashing, Same Performance: Building a Better Bloom Filter"
/// (2006); the trick is textbook, not this module's invention.
fn positions(probes: u32, bits: usize, subject: &str) -> impl Iterator<Item = usize> {
    let one = mixed(fnv1a(subject.as_bytes(), FNV_OFFSET));
    let two = mixed(fnv1a(subject.as_bytes(), FNV_OFFSET ^ SECOND_BASIS)) | 1;
    let mask = u64::try_from(bits).expect("a filter fits in memory") - 1;
    (0..u64::from(probes)).map(move |probe| {
        let place = one.wrapping_add(probe.wrapping_mul(two)) & mask;
        usize::try_from(place).expect("masked to the filter's length")
    })
}

/// The subjects of a segment, folded to bits.
///
/// A plain bloom filter: `holds` answers `false` with certainty and `true`
/// with roughly 99% confidence at the sizing above.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Bloom {
    probes: u32,
    bits: Vec<u8>,
}

impl Bloom {
    fn over(subjects: &BTreeSet<&str>) -> Bloom {
        let bits = (subjects.len() * BITS_PER_SUBJECT)
            .next_power_of_two()
            .max(FEWEST_BITS);
        let mut bloom = Bloom {
            probes: PROBES,
            bits: vec![0; bits / 8],
        };
        for subject in subjects {
            for position in positions(bloom.probes, bloom.bits.len() * 8, subject) {
                bloom.bits[position / 8] |= 1 << (position % 8);
            }
        }
        bloom
    }

    fn holds(&self, subject: &str) -> bool {
        positions(self.probes, self.bits.len() * 8, subject)
            .all(|position| self.bits[position / 8] & (1 << (position % 8)) != 0)
    }
}

/// The manifest as it stands in its file — strings throughout, validated
/// into a [`Manifest`] on the way in, the way a claim line becomes a
/// [`Claim`]. Unknown members are tolerated, not refused: this is cache,
/// and the version member already says whose form the file is.
#[derive(Debug, Serialize, Deserialize)]
struct RawManifest {
    #[serde(rename = "ossuary-manifest")]
    version: u32,
    segment: String,
    claims: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    first: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    earliest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    latest: Option<String>,
    namespaces: Vec<String>,
    subjects: usize,
    bloom: RawBloom,
}

#[derive(Debug, Serialize, Deserialize)]
struct RawBloom {
    hash: String,
    probes: u32,
    bits: String,
}

/// What one sealed segment holds, folded small: counts, times, namespaces,
/// and a bloom filter over the subjects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    digest: Digest,
    claims: usize,
    first: Option<Timestamp>,
    earliest: Option<Timestamp>,
    latest: Option<Timestamp>,
    namespaces: BTreeSet<String>,
    subjects: usize,
    bloom: Bloom,
}

impl Manifest {
    /// Fold the claims of the segment under `digest` into its manifest.
    #[must_use]
    pub fn of(digest: &Digest, claims: &[Claim]) -> Manifest {
        let subjects: BTreeSet<&str> = claims
            .iter()
            .map(|claim| claim.subject().as_str())
            .collect();
        let namespaces = claims
            .iter()
            .map(|claim| claim.attribute().namespace().to_string())
            .collect();
        Manifest {
            digest: digest.clone(),
            claims: claims.len(),
            first: claims.first().map(|claim| claim.time().clone()),
            earliest: claims.iter().map(Claim::time).min().cloned(),
            latest: claims.iter().map(Claim::time).max().cloned(),
            namespaces,
            subjects: subjects.len(),
            bloom: Bloom::over(&subjects),
        }
    }

    /// The segment this manifest answers for.
    #[must_use]
    pub fn digest(&self) -> &Digest {
        &self.digest
    }

    /// How many claims the segment holds, the header not counted.
    #[must_use]
    pub fn claim_count(&self) -> usize {
        self.claims
    }

    /// When the segment's first claim was recorded — its place in the log.
    #[must_use]
    pub fn first_claim_at(&self) -> Option<&Timestamp> {
        self.first.as_ref()
    }

    /// The earliest claim time in the segment.
    ///
    /// Usually the first claim's — but claim times follow a wall clock,
    /// and a wall clock is allowed to have stepped backwards.
    #[must_use]
    pub fn earliest_claim_at(&self) -> Option<&Timestamp> {
        self.earliest.as_ref()
    }

    /// The latest claim time in the segment.
    #[must_use]
    pub fn latest_claim_at(&self) -> Option<&Timestamp> {
        self.latest.as_ref()
    }

    /// Every namespace the segment's attributes speak in.
    #[must_use]
    pub fn namespaces(&self) -> &BTreeSet<String> {
        &self.namespaces
    }

    /// How many distinct subjects the segment speaks of.
    #[must_use]
    pub fn subject_count(&self) -> usize {
        self.subjects
    }

    /// Whether the segment might mention `subject`.
    ///
    /// `false` is certain; `true` is roughly 99% confident — a bloom
    /// filter's word. The caller who needs certainty reads the segment,
    /// which this answer exists to make rare.
    #[must_use]
    pub fn might_mention(&self, subject: &Subject) -> bool {
        self.bloom.holds(subject.as_str())
    }

    /// The manifest as its file holds it: one line of JSON.
    fn to_line(&self) -> String {
        let time = |time: &Option<Timestamp>| time.as_ref().map(|time| time.as_str().to_string());
        let raw = RawManifest {
            version: VERSION,
            segment: self.digest.to_string(),
            claims: self.claims,
            first: time(&self.first),
            earliest: time(&self.earliest),
            latest: time(&self.latest),
            namespaces: self.namespaces.iter().cloned().collect(),
            subjects: self.subjects,
            bloom: RawBloom {
                hash: HASH.to_string(),
                probes: self.bloom.probes,
                bits: hex(&self.bloom.bits),
            },
        };
        // Strings and numbers serialise; see `Claim::to_line`.
        let mut line = serde_json::to_string(&raw).expect("a manifest serialises");
        line.push('\n');
        line
    }

    /// Read a manifest back, answering only for `digest` — anything that
    /// does not read, does not match, or was built in a form this build
    /// does not write is `None`, never an error: cache.
    fn parse(text: &str, digest: &Digest) -> Option<Manifest> {
        let raw: RawManifest = serde_json::from_str(text).ok()?;
        if raw.version != VERSION
            || raw.segment != digest.to_string()
            || raw.bloom.hash != HASH
            || raw.bloom.probes == 0
        {
            return None;
        }
        let bits = unhex(&raw.bloom.bits)?;
        // The probe mask relies on the table being a power of two at least
        // the floor wide; bits that are not were never this crate's.
        if bits.len() * 8 < FEWEST_BITS || !(bits.len() * 8).is_power_of_two() {
            return None;
        }
        let time = |time: Option<String>| match time {
            None => Some(None),
            Some(text) => Timestamp::parse(&text).ok().map(Some),
        };
        Some(Manifest {
            digest: digest.clone(),
            claims: raw.claims,
            first: time(raw.first)?,
            earliest: time(raw.earliest)?,
            latest: time(raw.latest)?,
            namespaces: raw.namespaces.into_iter().collect(),
            subjects: raw.subjects,
            bloom: Bloom {
                probes: raw.bloom.probes,
                bits,
            },
        })
    }
}

/// The manifest drawer in `cache/`: one file per sealed segment, named by
/// the segment's digest.
#[derive(Debug)]
pub struct Manifests {
    dir: PathBuf,
}

impl Manifests {
    /// The drawer at `dir`, made when first filed into.
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Manifests { dir: dir.into() }
    }

    /// The manifest for `digest`, when one stands and answers for it.
    ///
    /// Lenient by design — missing, unreadable, another form's, or
    /// answering for a different segment all come back as `None`, and the
    /// caller reads the segment instead, usually filing a fresh manifest
    /// in passing.
    #[must_use]
    pub fn load(&self, digest: &Digest) -> Option<Manifest> {
        let text = fs::read_to_string(self.path(digest)).ok()?;
        Manifest::parse(&text, digest)
    }

    /// File a manifest, making the drawer as needed.
    ///
    /// The write is plain, not atomic: a manifest torn by a crash fails
    /// [`load`](Manifests::load) and is rebuilt, which is all the
    /// durability a cache is owed.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] making the drawer or writing the file — which callers
    /// on a sealing or reading path deliberately drop: cache trouble never
    /// outranks the truth operation it decorates.
    pub fn store(&self, manifest: &Manifest) -> Result<()> {
        let io = |context: &str| {
            let context = format!("{}: {context}", self.dir.display());
            move |source| Error::Io { context, source }
        };
        fs::create_dir_all(&self.dir).map_err(io("creating the manifest drawer"))?;
        fs::write(self.path(manifest.digest()), manifest.to_line())
            .map_err(io("writing a manifest"))
    }

    fn path(&self, digest: &Digest) -> PathBuf {
        self.dir.join(format!("{digest}.json"))
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(text, "{byte:02x}").expect("writing to a string");
    }
    text
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use immure::Digest;
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::claim::{Attribute, Source};

    fn digest() -> Digest {
        Digest::parse(&"ab".repeat(32)).unwrap()
    }

    fn subject(fill: &str) -> Subject {
        Subject::parse(&format!("sha256:{}", fill.repeat(32))).unwrap()
    }

    fn claim(fill: &str, attribute: &str, time: &str) -> Claim {
        Claim::assert(
            subject(fill),
            Attribute::parse(attribute).unwrap(),
            json!("held"),
            Timestamp::parse(time).unwrap(),
            Source::parse("test").unwrap(),
        )
        .unwrap()
    }

    fn manifest() -> Manifest {
        Manifest::of(
            &digest(),
            &[
                claim("1f", "user:tag", "2026-09-04T12:00:00Z"),
                claim("1f", "file:size", "2026-09-04T11:00:00Z"),
                claim("2e", "file:size", "2026-09-04T13:00:00Z"),
            ],
        )
    }

    #[test]
    fn a_manifest_answers_for_its_claims() {
        let manifest = manifest();

        assert_eq!(manifest.claim_count(), 3);
        assert_eq!(manifest.subject_count(), 2);
        assert_eq!(
            manifest.first_claim_at().unwrap().as_str(),
            "2026-09-04T12:00:00Z",
            "first is log order, not clock order"
        );
        assert_eq!(
            manifest.earliest_claim_at().unwrap().as_str(),
            "2026-09-04T11:00:00Z",
            "and earliest is the clock's answer, stepped-back clocks included"
        );
        assert_eq!(
            manifest.latest_claim_at().unwrap().as_str(),
            "2026-09-04T13:00:00Z"
        );
        assert_eq!(
            manifest.namespaces().iter().collect::<Vec<_>>(),
            ["file", "user"]
        );
    }

    #[test]
    fn the_bloom_filter_never_denies_a_subject_it_saw() {
        // The fixture fills are deliberately treacherous: "1f", "2e" and
        // "3d" are byte pairs of equal sum, which collide in unmixed
        // FNV's low bits — exactly the corner that made `mixed` necessary.
        let manifest = manifest();

        assert!(manifest.might_mention(&subject("1f")));
        assert!(manifest.might_mention(&subject("2e")));
        assert!(
            !manifest.might_mention(&subject("3d")),
            "a subject never spoken of probes clean — deterministically, so this stays green"
        );
    }

    #[test]
    fn a_segment_of_no_claims_still_has_a_shape() {
        let manifest = Manifest::of(&digest(), &[]);

        assert_eq!(manifest.claim_count(), 0);
        assert_eq!(manifest.first_claim_at(), None);
        assert_eq!(manifest.earliest_claim_at(), None);
        assert!(manifest.namespaces().is_empty());
        assert!(!manifest.might_mention(&subject("1f")));
    }

    #[test]
    fn the_borrowed_algorithms_match_their_published_vectors() {
        // FNV-1a 64, values from the test vectors of the IETF draft
        // `draft-eastlake-fnv`.
        assert_eq!(fnv1a(b"", FNV_OFFSET), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a", FNV_OFFSET), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a(b"foobar", FNV_OFFSET), 0x8594_4171_f739_67e8);
        // splitmix64, seed 0: the reference implementation's first output
        // is the mixer run over the golden gamma.
        assert_eq!(mixed(0x9e37_79b9_7f4a_7c15), 0xe220_a839_7b1d_cdaf);
    }

    #[test]
    fn the_filter_bits_are_frozen() {
        // Manifests persist, so the whole probe pipeline — bases, mixer,
        // stride, sizing — must stay byte-identical forever: bits already
        // on disk are probed by future builds, and a drifted pipeline
        // would mis-probe every manifest already filed, silently. This
        // hex was computed by an independent implementation; change it
        // only together with VERSION.
        let line = manifest().to_line();
        assert!(
            line.contains("\"bits\":\"1000880044082384\""),
            "the probe pipeline drifted: {line}"
        );
    }

    #[test]
    fn a_manifest_reads_back_from_its_line() {
        let manifest = manifest();

        let line = manifest.to_line();
        assert!(
            line.starts_with("{\"ossuary-manifest\":1,"),
            "the file names its own form before anything else"
        );
        assert_eq!(
            Manifest::parse(&line, &digest()).unwrap(),
            manifest,
            "reading and writing agree, bits included"
        );
    }

    #[test]
    fn what_does_not_answer_for_the_segment_is_absent() {
        let line = manifest().to_line();
        let other = Digest::parse(&"cd".repeat(32)).unwrap();

        assert_eq!(
            Manifest::parse(&line, &other),
            None,
            "a manifest answers for its own segment or not at all"
        );
        assert_eq!(Manifest::parse("not a manifest\n", &digest()), None);
        assert_eq!(
            Manifest::parse(
                &line.replace("\"ossuary-manifest\":1", "\"ossuary-manifest\":2"),
                &digest()
            ),
            None,
            "another form is absence, not an error — cache is rebuilt, never refused"
        );
    }

    #[test]
    fn the_drawer_hands_back_what_it_kept() {
        let dir = TempDir::new().unwrap();
        let drawer = Manifests::new(dir.path().join("manifests"));
        let manifest = manifest();

        assert_eq!(
            drawer.load(&digest()),
            None,
            "an unfiled manifest is absent"
        );
        drawer.store(&manifest).unwrap();
        assert_eq!(drawer.load(&digest()).unwrap(), manifest);
    }
}
