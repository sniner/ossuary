//! The fixes, one module per scar. Each is a function from the log to
//! a [`Plan`](crate::plan::Plan): read what stands, work out what is
//! missing, and hand back the claims that close the gap with the words
//! for saying so. A fix writes nothing itself.

pub mod origin;
pub mod packed;
