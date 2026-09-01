//! How answers read on a terminal.

use ossuary_core::Claim;

/// One claim as one line: when it was recorded, what it says, who says so.
///
/// Values read as JSON — a string keeps its quotes, a number stands bare —
/// so what the reader sees is what the log holds, type and all.
pub fn line(claim: &Claim) -> String {
    let time = claim.time().as_str();
    let attribute = claim.attribute().as_str();
    let source = claim.source().as_str();
    match (claim.value(), claim.is_retraction()) {
        (Some(value), false) => format!("{time}  {attribute} = {value}  [{source}]"),
        (Some(value), true) => format!("{time}  retracted: {attribute} = {value}  [{source}]"),
        (None, _) => format!("{time}  retracted: {attribute}, every value  [{source}]"),
    }
}
