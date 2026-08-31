use soroban_sdk::contracterror;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    NotAuthorized = 1,
    StreamNotFound = 2,
    StreamCancelled = 3,
    StreamNotStarted = 4,
    StreamEnded = 5,
    NothingToWithdraw = 6,
    InsufficientDeposit = 7,
    InvalidTimeRange = 8,
    AlreadyPaused = 9,
    NotPaused = 10,
    ClawbackDisabled = 11,
    ArithmeticOverflow = 12,
    PauseThresholdNotMet = 13,
    AlreadyInitialized = 14,
    InvalidAmount = 15,
    ReentrancyForbidden = 16,
    OperatorAlreadySet = 17,
    /// The recipient is invalid: either the all-zero Stellar account address
    /// (an unspendable sink) or identical to the stream's `sender` (a
    /// self-stream). Mirrors the guard `DripFactory::create_stream` enforces
    /// before deployment so a stream initialized directly (ADR-001) cannot
    /// bypass it.
    InvalidRecipient = 18,
    /// `start_time` is in the past (before the current ledger time), so the
    /// stream would already be "running" at initialization and the recipient
    /// could immediately withdraw a lump sum before the sender can react.
    /// Mirrors `create_stream`'s backdated-start guard.
    BackdatedStream = 19,
    /// The stream has accrued more tokens than are currently funded in the
    /// contract (e.g. an under-deposited bounded stream, or an open-ended
    /// (`end_time == 0`) stream whose accrual has outpaced its `top_up`s).
    /// The recipient may withdraw only the funded portion; this error
    /// distinguishes "accrued but not funded" from `NothingToWithdraw`
    /// ("nothing has accrued yet").
    StreamUnderfunded = 20,
}
