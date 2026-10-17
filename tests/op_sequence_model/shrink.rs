//! Minimization of a failing operation sequence into a pinned regression
//! (#684). Interface only: the implementation follows.
use super::generator::Op;
use std::any::Any;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mismatch {
    pub kind: String,
    pub step: usize,
}

pub fn kind(_message: &str) -> String {
    unimplemented!("#684")
}

pub fn kind_of(_payload: &(dyn Any + Send)) -> String {
    unimplemented!("#684")
}

pub fn shrink(
    _operations: &[Op],
    _found: &Mismatch,
    _reproduces: impl FnMut(&[Op]) -> Option<Mismatch>,
) -> Result<Vec<Op>, String> {
    unimplemented!("#684")
}
