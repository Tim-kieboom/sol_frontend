mod escape_check;
mod move_check;
mod overlap_check;
pub use escape_check::check_escapes;
pub use move_check::{check_moves, elaborate_drops};
pub use overlap_check::check_borrow_overlaps;