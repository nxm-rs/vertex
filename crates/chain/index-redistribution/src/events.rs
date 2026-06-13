//! `sol!` event bindings for the Redistribution contract.
//!
//! These are the exact event signatures from the contract ABI at
//! `deployments/mainnet/Redistribution.json`. They are the only contract detail
//! this crate hard-codes; the fold in [`crate::projection`] decodes logs through
//! them and the [`Indexer`](vertex_chain_index::Indexer) filter selects on their
//! `topic0` hashes.

use alloy_sol_types::sol;

sol! {
    /// A node committed its obfuscated reveal hash for `roundNumber`.
    #[allow(missing_docs)]
    event Committed(uint256 roundNumber, bytes32 overlay, uint8 height);

    /// A node revealed its reserve commitment for `roundNumber`.
    #[allow(missing_docs)]
    event Revealed(
        uint256 roundNumber,
        bytes32 overlay,
        uint256 stake,
        uint256 stakeDensity,
        bytes32 reserveCommitment,
        uint8 depth
    );

    /// The reveal anchor (the seed the truth selection draws against) for
    /// `roundNumber`.
    #[allow(missing_docs)]
    event CurrentRevealAnchor(uint256 roundNumber, bytes32 anchor);

    /// The truth (the agreed reserve hash and depth) the round settled on.
    #[allow(missing_docs)]
    event TruthSelected(bytes32 hash, uint8 depth);

    /// The winning reveal a round paid out to.
    #[allow(missing_docs)]
    event WinnerSelected(Reveal winner);

    /// The number of commits counted in the current round.
    #[allow(missing_docs)]
    event CountCommits(uint256 _count);

    /// The number of reveals counted in the current round.
    #[allow(missing_docs)]
    event CountReveals(uint256 _count);

    /// The valid chunk count the round priced against.
    #[allow(missing_docs)]
    event ChunkCount(uint256 validChunkCount);

    /// A reveal record, as carried by [`WinnerSelected`].
    #[allow(missing_docs)]
    struct Reveal {
        bytes32 overlay;
        address owner;
        uint8 depth;
        uint256 stake;
        uint256 stakeDensity;
        bytes32 hash;
    }
}
