#![warn(clippy::std_instead_of_core)]

use core::mem;
use std::time::Instant;

mod parallel;

pub mod addr;
pub mod ext;
pub mod flags;
pub mod graft;
pub mod heap;
pub mod port;
pub mod stats;
pub mod wire;
pub mod word;

pub(crate) mod allocator;
mod interact;

use crate::runtime::{
  allocator::Allocator, ext::Extrinsics, flags::Flags, port::Port, stats::Stats,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YieldReason {
  /// No active pairs remain.
  Quiescent,

  /// Work remains, but the supplied reduction budget was consumed.
  BudgetExhausted,
}

pub struct Runtime<'ivm, 'ext> {
  /// Execution statistics of this runtime.
  pub stats: Stats,

  /// Error flags set during interactions.
  pub flags: Flags,

  pub(crate) extrinsics: &'ext Extrinsics<'ivm>,

  pub(crate) alloc: Allocator<'ivm>,
  pub(crate) alloc_pool: Vec<Allocator<'ivm>>,

  /// Active pairs that should be "fast" to process (generally, those which do
  /// not allocate memory).
  pub(crate) active_fast: Vec<(Port<'ivm>, Port<'ivm>)>,
  /// Active pairs that may be "slow" to process (generally, those which may
  /// allocate memory).
  pub(crate) active_slow: Vec<(Port<'ivm>, Port<'ivm>)>,

  /// Used by [`IVM::execute`].
  pub(crate) registers: Vec<Option<Port<'ivm>>>,
}

impl<'ivm, 'ext> Runtime<'ivm, 'ext> {
  pub(crate) fn new(alloc: Allocator<'ivm>, extrinsics: &'ext Extrinsics<'ivm>) -> Self {
    Self {
      alloc,
      extrinsics,
      alloc_pool: Vec::new(),
      registers: Vec::new(),
      active_fast: Vec::new(),
      active_slow: Vec::new(),
      stats: Stats::default(),
      flags: Flags::default(),
    }
  }

  /// Normalize all nets in this IVM.
  pub fn normalize(&mut self, mut hooks: impl Hooks) {
    let start = hooks.now();
    while !self.is_quiescent() {
      hooks.tick(&start, &mut self.stats);

      let reduced = self.reduce_one();
      debug_assert!(reduced);
    }

    hooks.end(&start, &mut self.stats);
  }

  /// Reduce all "fast" active pairs, returning the number of interactions.
  pub(crate) fn do_fast(&mut self) {
    while let Some((a, b)) = self.active_fast.pop() {
      self.interact(a, b);
    }
  }

  /// Normalize all nets in breadth-first traversal.
  ///
  /// This is useful to get the depth (longest critical path) of the computation
  /// to understand the parallelism of the program.
  pub fn normalize_breadth_first(&mut self, mut hooks: impl Hooks) {
    let start = hooks.now();
    let mut work = vec![];
    loop {
      hooks.tick(&start, &mut self.stats);

      mem::swap(&mut work, &mut self.active_fast);
      work.append(&mut self.active_slow);
      if work.is_empty() {
        break;
      }
      for (a, b) in work.drain(..) {
        self.interact(a, b);
      }
      self.stats.depth += 1;
    }

    hooks.end(&start, &mut self.stats);
  }

  /// Reduces up to `budget` queued active pairs.
  ///
  /// Any remaining active pairs stay in the runtime and may be resumed by a
  /// subsequent call.
  pub fn normalize_for(&mut self, budget: u64, mut hooks: impl Hooks) -> YieldReason {
    let start = hooks.now();
    let mut remaining = budget;

    while remaining > 0 && !self.is_quiescent() {
      hooks.tick(&start, &mut self.stats);

      let reduced = self.reduce_one();
      debug_assert!(reduced);

      remaining -= 1;
    }

    let reason =
      if self.is_quiescent() { YieldReason::Quiescent } else { YieldReason::BudgetExhausted };

    hooks.end(&start, &mut self.stats);
    reason
  }

  /// Reduces one queued active pair.
  ///
  /// Fast active pairs currently retain priority over slow active pairs.
  /// Returns `false` if the runtime is quiescent.
  pub fn reduce_one(&mut self) -> bool {
    let pair = self.active_fast.pop().or_else(|| self.active_slow.pop());

    let Some((a, b)) = pair else {
      return false;
    };

    self.interact(a, b);
    true
  }

  pub fn is_quiescent(&self) -> bool {
    self.active_fast.is_empty() && self.active_slow.is_empty()
  }
}

pub trait Hooks {
  type Instant;

  fn now(&mut self) -> Self::Instant;
  fn tick(&mut self, _start: &Self::Instant, _stats: &mut Stats);
  fn end(&mut self, _start: &Self::Instant, _stats: &mut Stats);
}

impl Hooks for () {
  type Instant = Instant;

  fn now(&mut self) -> Self::Instant {
    Instant::now()
  }

  fn tick(&mut self, _start: &Self::Instant, _stats: &mut Stats) {}

  fn end(&mut self, start: &Self::Instant, stats: &mut Stats) {
    stats.time_clock += start.elapsed();
  }
}

impl<H: Hooks + ?Sized> Hooks for &mut H {
  type Instant = H::Instant;

  fn now(&mut self) -> Self::Instant {
    (**self).now()
  }

  fn tick(&mut self, start: &Self::Instant, stats: &mut Stats) {
    (**self).tick(start, stats);
  }

  fn end(&mut self, start: &Self::Instant, stats: &mut Stats) {
    (**self).end(start, stats);
  }
}

#[cfg(test)]
mod tests {
  use crate::{
    host::{Host, IVM},
    runtime::{YieldReason, heap::Heap, port::Tag},
  };

  #[test]
  fn normalization_can_stop_and_resume() {
    let mut heap = Heap::new();
    let mut ivm = IVM::new();
    let host = Host::new(&mut ivm);
    let mut runtime = host.init(&mut heap);

    // Queue two independent annihilations.
    for _ in 0..2 {
      let a = unsafe { runtime.new_node(Tag::Comb, 0) };

      let b = unsafe { runtime.new_node(Tag::Comb, 0) };

      runtime.link(a.0, b.0);
    }

    assert!(!runtime.is_quiescent());
    assert_eq!(runtime.stats.annihilate, 0);

    let reason = runtime.normalize_for(1, ());

    assert_eq!(reason, YieldReason::BudgetExhausted);
    assert_eq!(runtime.stats.annihilate, 1);
    assert!(!runtime.is_quiescent());

    let reason = runtime.normalize_for(1, ());

    assert_eq!(reason, YieldReason::Quiescent);
    assert_eq!(runtime.stats.annihilate, 2);
    assert!(runtime.is_quiescent());
  }

  #[test]
  fn zero_budget_does_not_reduce_pending_work() {
    let mut heap = Heap::new();
    let mut ivm = IVM::new();
    let host = Host::new(&mut ivm);
    let mut runtime = host.init(&mut heap);

    let a = unsafe { runtime.new_node(Tag::Comb, 0) };

    let b = unsafe { runtime.new_node(Tag::Comb, 0) };

    runtime.link(a.0, b.0);

    let reason = runtime.normalize_for(0, ());

    assert_eq!(reason, YieldReason::BudgetExhausted);
    assert_eq!(runtime.stats.annihilate, 0);
    assert!(!runtime.is_quiescent());

    runtime.normalize(());

    assert_eq!(runtime.stats.annihilate, 1);
    assert!(runtime.is_quiescent());
  }
}
