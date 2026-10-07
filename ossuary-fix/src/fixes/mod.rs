//! The fixes, one module per scar. A fix that adds claims is a
//! function from the log to a [`Plan`](crate::plan::Plan): read what
//! stands, work out what is missing, and hand back the claims that
//! close the gap with the words for saying so. A fix that has to change
//! what is sealed is a function from the archive to a
//! [`Rewrite`](crate::rewrite::Rewrite): the edit of one segment, and
//! the words. A fix writes nothing itself.

pub mod origin;
pub mod packed;
pub mod previous;
