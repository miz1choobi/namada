//! All the states of the [`BlockSpaceAllocator`] state machine,
//! over the extent of a Tendermint consensus round
//! block proposal.
//!
//! # States
//!
//! The state machine moves through the following state DAG:
//!
//! 1. [`BuildingDecryptedTxBatch`] - the initial state. In
//!    this state, we populate a block with DKG decrypted txs.
//! 2. [`BuildingProtocolTxBatch`] - the second state. In
//!    this state, we populate a block with protocol txs.
//! 3. [`BuildingEncryptedTxBatch`] - the third state. In
//!    this state, we populate a block with DKG encrypted txs.
//!    This state supports two modes of operation, which you can
//!    think of as two states diverging from [`BuildingProtocolTxBatch`]:
//!   * [`WithoutEncryptedTxs`] - When this mode is active, no encrypted txs are
//!     included in a block proposal.
//!   * [`WithEncryptedTxs`] - When this mode is active, we are able to include
//!     encrypted txs in a block proposal.
//! 4. [`FillingRemainingSpace`] - the fourth and final state.
//!    During this phase, we fill all remaining block space with arbitrary
//!    protocol transactions that haven't been included in a block, yet.

mod decrypted_txs;
mod encrypted_txs;
mod protocol_txs;
mod remaining_txs;
pub mod tracker;

use super::{AllocFailure, BlockSpaceAllocator};

/// A [`BlockSpaceAllocator`] that keeps track of whether
/// any bin space is left or not.
pub struct FusedBlockSpaceAllocator<S> {
    /// The inner [`BlockSpaceAllocator`].
    alloc: BlockSpaceAllocator<S>,
    /// Boolean flag that keeps track of the failure
    /// status of some allocation.
    ///
    /// In turn, this means that the current allocator
    /// state has no more space left for txs.
    ran_out_of_space: bool,
}

impl<S> FusedBlockSpaceAllocator<S> {
    /// Check if this [`FusedBlockSpaceAllocator`]
    /// still has any bin space left.
    #[inline]
    #[allow(dead_code)]
    pub fn has_run_out_of_space(&self) -> bool {
        self.ran_out_of_space
    }
}

impl<S> BlockSpaceAllocator<S> {
    /// Fuse the current [`BlockSpaceAllocator`].
    #[inline]
    #[allow(dead_code)]
    pub fn fuse(self) -> FusedBlockSpaceAllocator<S> {
        FusedBlockSpaceAllocator {
            alloc: self,
            ran_out_of_space: false,
        }
    }
}

impl<S> TryAlloc for FusedBlockSpaceAllocator<S>
where
    BlockSpaceAllocator<S>: TryAlloc,
{
    fn try_alloc(&mut self, tx: &[u8]) -> Result<(), AllocFailure> {
        if self.ran_out_of_space {
            return Err(AllocFailure::Rejected { bin_space_left: 0 });
        }
        self.alloc.try_alloc(tx).map_err(|err| {
            if matches!(err, AllocFailure::Rejected { .. }) {
                self.ran_out_of_space = true;
            }
            err
        })
    }
}

impl<S, T> NextStateImpl<T> for FusedBlockSpaceAllocator<S>
where
    BlockSpaceAllocator<S>: NextStateImpl<T>,
{
    type Next = <BlockSpaceAllocator<S> as NextStateImpl<T>>::Next;

    fn next_state_impl(self) -> Self::Next {
        self.alloc.next_state_impl()
    }
}

/// Convenience wrapper for a [`BlockSpaceAllocator`] state that allocates
/// encrypted transactions.
pub enum EncryptedTxBatchAllocator {
    WithEncryptedTxs(
        BlockSpaceAllocator<BuildingEncryptedTxBatch<WithEncryptedTxs>>,
    ),
    WithoutEncryptedTxs(
        BlockSpaceAllocator<BuildingEncryptedTxBatch<WithoutEncryptedTxs>>,
    ),
}

/// The leader of the current Tendermint round is building
/// a new batch of DKG decrypted transactions.
///
/// For more info, read the module docs of
/// [`crate::node::ledger::shell::prepare_proposal::block_space_alloc::states`].
pub enum BuildingDecryptedTxBatch {}

/// The leader of the current Tendermint round is building
/// a new batch of Namada protocol transactions.
///
/// For more info, read the module docs of
/// [`crate::node::ledger::shell::prepare_proposal::block_space_alloc::states`].
pub enum BuildingProtocolTxBatch {}

/// The leader of the current Tendermint round is building
/// a new batch of DKG encrypted transactions.
///
/// For more info, read the module docs of
/// [`crate::node::ledger::shell::prepare_proposal::block_space_alloc::states`].
pub struct BuildingEncryptedTxBatch<Mode> {
    /// One of [`WithEncryptedTxs`] and [`WithoutEncryptedTxs`].
    _mode: Mode,
}

/// The leader of the current Tendermint round is populating
/// all remaining space in a block proposal with arbitrary
/// protocol transactions that haven't been included in the
/// block, yet.
///
/// For more info, read the module docs of
/// [`crate::node::ledger::shell::prepare_proposal::block_space_alloc::states`].
pub enum FillingRemainingSpace {}

/// Allow block proposals to include encrypted txs.
///
/// For more info, read the module docs of
/// [`crate::node::ledger::shell::prepare_proposal::block_space_alloc::states`].
pub enum WithEncryptedTxs {}

/// Prohibit block proposals from including encrypted txs.
///
/// For more info, read the module docs of
/// [`crate::node::ledger::shell::prepare_proposal::block_space_alloc::states`].
pub enum WithoutEncryptedTxs {}

/// Try to allocate a new transaction on a [`BlockSpaceAllocator`] state.
///
/// For more info, read the module docs of
/// [`crate::node::ledger::shell::prepare_proposal::block_space_alloc::states`].
pub trait TryAlloc {
    /// Try to allocate space for a new transaction.
    fn try_alloc(&mut self, tx: &[u8]) -> Result<(), AllocFailure>;
}

/// Represents a state transition in the [`BlockSpaceAllocator`] state machine.
///
/// This trait should not be used directly. Instead, consider using one of
/// [`NextState`], [`NextStateWithEncryptedTxs`] or
/// [`NextStateWithoutEncryptedTxs`].
///
/// For more info, read the module docs of
/// [`crate::node::ledger::shell::prepare_proposal::block_space_alloc::states`].
pub trait NextStateImpl<Transition = ()> {
    /// The next state in the [`BlockSpaceAllocator`] state machine.
    type Next;

    /// Transition to the next state in the [`BlockSpaceAllocator`] state
    /// machine.
    fn next_state_impl(self) -> Self::Next;
}

/// Convenience extension of [`NextStateImpl`], to transition to a new
/// state with encrypted txs in a block.
///
/// For more info, read the module docs of
/// [`crate::node::ledger::shell::prepare_proposal::block_space_alloc::states`].
pub trait NextStateWithEncryptedTxs: NextStateImpl<WithEncryptedTxs> {
    /// Transition to the next state in the [`BlockSpaceAllocator`] state,
    /// ensuring we include encrypted txs in a block.
    #[inline]
    fn next_state_with_encrypted_txs(self) -> Self::Next
    where
        Self: Sized,
    {
        self.next_state_impl()
    }
}

impl<S> NextStateWithEncryptedTxs for S where S: NextStateImpl<WithEncryptedTxs> {}

/// Convenience extension of [`NextStateImpl`], to transition to a new
/// state without encrypted txs in a block.
///
/// For more info, read the module docs of
/// [`crate::node::ledger::shell::prepare_proposal::block_space_alloc::states`].
pub trait NextStateWithoutEncryptedTxs:
    NextStateImpl<WithoutEncryptedTxs>
{
    /// Transition to the next state in the [`BlockSpaceAllocator`] state,
    /// ensuring we do not include encrypted txs in a block.
    #[inline]
    fn next_state_without_encrypted_txs(self) -> Self::Next
    where
        Self: Sized,
    {
        self.next_state_impl()
    }
}

impl<S> NextStateWithoutEncryptedTxs for S where
    S: NextStateImpl<WithoutEncryptedTxs>
{
}

/// Convenience extension of [`NextStateImpl`], to transition to a new
/// state with a null transition function.
///
/// For more info, read the module docs of
/// [`crate::node::ledger::shell::prepare_proposal::block_space_alloc::states`].
pub trait NextState: NextStateImpl {
    /// Transition to the next state in the [`BlockSpaceAllocator`] state,
    /// using a null transiiton function.
    #[inline]
    fn next_state(self) -> Self::Next
    where
        Self: Sized,
    {
        self.next_state_impl()
    }
}

impl<S> NextState for S where S: NextStateImpl {}
