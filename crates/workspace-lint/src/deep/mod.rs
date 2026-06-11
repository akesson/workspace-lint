//! Deep `--fix` verification: run `rust-analyzer scip`, ingest its index, and
//! use it as a one-directional oracle over the reference-evidence findings
//! (`unused-deps`, `unused-pub`) before `--fix` acts on them.
//!
//! The doctrine (`DESIGN-ir-pipeline.md` §10): SCIP is ground truth for "is
//! crate X referenced" (it sees through method calls and macro expansion that
//! the syn-based resolver can't), so it can only ever **disprove** one of our
//! findings — confirm the resolver and rust-analyzer agree (apply the
//! structural fix) or catch a resolver false positive (write a suppression
//! directive instead, gated on the clean tree the `--fix` entry requires).
//! It never creates a finding and never upgrades a `MaybeIncorrect` suggestion.
//!
//! Submodules:
//! - [`normalize`] — project a SCIP symbol onto canonical segments (§8).
//! - [`index`] — load + flatten a `rust-analyzer scip` index.

pub(crate) mod index;
pub(crate) mod normalize;
