/// Voting -- simple yes/no ballot with tally and close.
///
/// Demonstrates multi-field struct resources, conditional state
/// transitions, and read-only view functions.
/// Inspired by Aptos on-chain governance patterns.
module voting_addr::voting {
    use std::signer;

    /// Error codes.
    const E_ALREADY_EXISTS: u64 = 1;
    const E_NOT_FOUND: u64 = 2;
    const E_NOT_AUTHORIZED: u64 = 3;
    const E_ALREADY_CLOSED: u64 = 4;
    const E_ALREADY_VOTED: u64 = 5;

    /// Status constants.
    const STATUS_OPEN: u8 = 0;
    const STATUS_CLOSED: u8 = 1;

    /// A ballot created by a proposer.
    struct Ballot has key, store {
        proposer: address,
        yes_votes: u64,
        no_votes: u64,
        status: u8,
    }

    /// Tracks whether an account has voted on a given proposer's ballot.
    struct VoteReceipt has key, store {
        voted_on: address,
        vote: bool,
    }

    /// Create a new ballot.  One ballot per proposer.
    public entry fun create_ballot(proposer: &signer) {
        let addr = signer::address_of(proposer);
        assert!(!exists<Ballot>(addr), E_ALREADY_EXISTS);
        move_to(proposer, Ballot {
            proposer: addr,
            yes_votes: 0,
            no_votes: 0,
            status: STATUS_OPEN,
        });
    }

    /// Cast a vote on `proposer`'s ballot.
    /// `vote_yes` = true for yes, false for no.
    /// Each voter can only vote once (tracked via VoteReceipt).
    public entry fun cast_vote(
        voter: &signer,
        proposer: address,
        vote_yes: bool,
    ) acquires Ballot {
        let voter_addr = signer::address_of(voter);

        // Ballot must exist and be open.
        assert!(exists<Ballot>(proposer), E_NOT_FOUND);
        let ballot = borrow_global_mut<Ballot>(proposer);
        assert!(ballot.status == STATUS_OPEN, E_ALREADY_CLOSED);

        // Voter must not have voted already.
        assert!(!exists<VoteReceipt>(voter_addr), E_ALREADY_VOTED);

        // Record the vote.
        if (vote_yes) {
            ballot.yes_votes = ballot.yes_votes + 1;
        } else {
            ballot.no_votes = ballot.no_votes + 1;
        };

        // Give voter a receipt so they cannot vote again.
        move_to(voter, VoteReceipt {
            voted_on: proposer,
            vote: vote_yes,
        });
    }

    /// Close a ballot.  Only the proposer can close it.
    public entry fun close_ballot(proposer: &signer) acquires Ballot {
        let addr = signer::address_of(proposer);
        assert!(exists<Ballot>(addr), E_NOT_FOUND);
        let ballot = borrow_global_mut<Ballot>(addr);
        assert!(ballot.proposer == addr, E_NOT_AUTHORIZED);
        assert!(ballot.status == STATUS_OPEN, E_ALREADY_CLOSED);
        ballot.status = STATUS_CLOSED;
    }

    #[view]
    /// Read ballot results: (yes_votes, no_votes, status).
    public fun get_results(proposer: address): (u64, u64, u8) acquires Ballot {
        assert!(exists<Ballot>(proposer), E_NOT_FOUND);
        let b = borrow_global<Ballot>(proposer);
        (b.yes_votes, b.no_votes, b.status)
    }

    #[view]
    /// Check if the ballot is still open.
    public fun is_open(proposer: address): bool acquires Ballot {
        assert!(exists<Ballot>(proposer), E_NOT_FOUND);
        borrow_global<Ballot>(proposer).status == STATUS_OPEN
    }
}
