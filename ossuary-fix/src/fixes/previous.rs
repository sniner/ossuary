//! `previous` in the segment header as a list.
//!
//! Until 0.10.1 a segment named the one sealed before it as a string.
//! The member is a list now, so that a segment can name several, and
//! the current build refuses the string. The fix changes nothing in a
//! segment itself: the rewriter writes every header in the current
//! form, and a segment whose header was in the old form comes out
//! renamed, with every segment after it following.

use anyhow::Result;
use ossuary_core::Archive;

use crate::rewrite::{Rewrite, Words};

/// Every segment and the open head, rewritten in the current form.
///
/// # Errors
///
/// Whatever [`Rewrite::plan`] can answer.
pub fn plan(archive: &Archive) -> Result<Rewrite> {
    Rewrite::plan(
        archive,
        Words {
            done: "with previous as a list",
            nothing: "every segment header is in the current form",
        },
        |_| Ok(()),
    )
}
