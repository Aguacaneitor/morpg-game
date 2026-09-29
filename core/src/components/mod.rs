//! Pure data. No behavior lives here — behavior lives in `systems/`.
//! This is the ECS discipline: components are just structs of numbers.

mod body;
mod input;
mod combat;
mod abilities;
mod character;
mod items;
mod ai;

pub use body::*;
pub use input::*;
pub use combat::*;
pub use abilities::*;
pub use character::*;
pub use items::*;
pub use ai::*;
