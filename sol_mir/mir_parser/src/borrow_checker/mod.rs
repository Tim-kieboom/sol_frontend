mod escape_check;
mod move_check;
mod overlap_check;
mod points_to;
pub use escape_check::check_escapes;
pub use move_check::{check_and_elaborate_moves, check_moves, elaborate_drops};
pub use overlap_check::{
    check_borrow_overlaps, check_move_while_borrowed, check_overlaps_and_moves_while_borrowed,
};

use mir_model::{self as mir, BlockId};
use sol_utils::collections::vec_set::VecSet;

/// Every block in `function` reachable from `entry`, ordered so that a
/// block appears only after every block reachable from it.
fn postorder(function: &mir::Function, entry: BlockId) -> Vec<BlockId> {
    fn visit(
        function: &mir::Function,
        block_id: BlockId,
        visited: &mut VecSet<BlockId>,
        order: &mut Vec<BlockId>,
    ) {
        if visited.insert(block_id).is_some() {
            return;
        }

        let Some(block) = function.blocks.get(block_id) else {
            return;
        };

        for successor in successors(&block.terminator) {
            visit(function, successor, visited, order);
        }

        order.push(block_id);
    }

    let mut visited = VecSet::new();
    let mut order = Vec::new();
    visit(function, entry, &mut visited, &mut order);
    order
}

/// Every block `terminator` can transfer control to.
fn successors(terminator: &mir::Terminator) -> Vec<BlockId> {
    match terminator {
        mir::Terminator::Goto(target) | mir::Terminator::Drop { target, .. } => vec![*target],
        mir::Terminator::SwitchInt {
            targets, otherwise, ..
        } => {
            let mut result: Vec<BlockId> = targets.iter().map(|(_, id)| *id).collect();
            result.push(*otherwise);
            result
        }
        mir::Terminator::Call { target, .. } => target.iter().copied().collect(),
        mir::Terminator::Assert { target, .. } => vec![*target],
        mir::Terminator::Return | mir::Terminator::Unreachable => Vec::new(),
    }
}
