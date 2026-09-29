//! Combat, one stage per file, in the order a tick runs them (see
//! `GameCorePlugin`): timers, starting weapon attacks and abilities,
//! the attack running its course, then hits, projectiles and death.
//! Every system and event is re-exported here, so `systems::combat::X`
//! paths hold; `resolution` is the shared plumbing behind `attacks` and
//! `abilities`.

mod timers;
mod resolution;
mod attacks;
mod abilities;
mod hits;
mod projectiles;

pub use timers::*;
pub use attacks::*;
pub use abilities::*;
pub use hits::*;
pub use projectiles::*;
