use cosmwasm_std::StdError;
use cw_utils::PaymentError;
use thiserror::Error;
use url::ParseError;

#[derive(Error, Debug)]
pub enum ContractError {
    #[error("{0}")]
    Std(#[from] StdError),

    #[error("{0}")]
    Payment(#[from] PaymentError),

    #[error("{0}")]
    Parse(#[from] ParseError),

    #[error("{0}")]
    Base(#[from] cw721_base::ContractError),

    #[error("Unauthorized")]
    Unauthorized {},

    #[error("Unauthorized Owner Does Not Match Sender")]
    UnauthorizedOwner {},

    #[error("InvalidCreationFee")]
    InvalidCreationFee {},

    #[error("token_id already claimed")]
    Claimed {},

    #[error("Cannot set approval that is already expired")]
    Expired {},

    #[error("Approval not found for: {spender}")]
    ApprovalNotFound { spender: String },

    #[error("Invalid Royalties")]
    InvalidRoyalties {},

    #[error("Description too long")]
    DescriptionTooLong {},

    #[error("InvalidStartTradingTime")]
    InvalidStartTradingTime {},

    #[error("CollectionInfoFrozen")]
    CollectionInfoFrozen {},

    #[error("Exceeded royalty share increase")]
    RoyaltyShareIncreasedTooMuch {},

    #[error("Exceeded max royalty share")]
    RoyaltyShareTooHigh {},

    #[error("Updated royalty too soon")]
    RoyaltyUpdateTooSoon {},

    #[error("Royalty Info invalid")]
    RoyaltyInfoInvalid {},

    #[error("MinterNotFound")]
    MinterNotFound {},

    #[error("Ownership Update Error: {error}")]
    OwnershipUpdateError { error: String },

    #[error("Error while migrating: ({0}) ")]
    MigrationError(String),
}
