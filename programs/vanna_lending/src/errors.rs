use anchor_lang::prelude::*;

#[error_code]
pub enum VannaError {
    #[msg("Signer is not authorized to perform this action")]
    Unauthorized,
    #[msg("Signer is not the pending admin")]
    InvalidPendingAdmin,
    #[msg("Treasury cannot be the zero address")]
    InvalidTreasury,
    #[msg("Operating mode value is not a recognized mode")]
    InvalidOperatingMode,
    #[msg("This action is paused by the current protocol operating mode")]
    ProtocolActionPaused,
    #[msg("This action is not allowed by the current reserve status")]
    InvalidReserveStatus,
    #[msg("Account did not derive from the expected PDA seeds")]
    InvalidPda,
    #[msg("Account bump does not match the stored canonical bump")]
    InvalidBump,
    #[msg("Mint does not match the expected asset mint")]
    InvalidMint,
    #[msg("Token program must be classic SPL Token or Token-2022")]
    InvalidTokenProgram,
    #[msg("Token account authority does not match the expected PDA")]
    InvalidVaultAuthority,
    #[msg("Share mint authority does not match the expected Reserve PDA")]
    InvalidShareMintAuthority,
    #[msg("Token-2022 mint extension is not supported in this release")]
    UnsupportedTokenExtension,
    #[msg("Asset is not enabled as collateral")]
    AssetNotCollateralEnabled,
    #[msg("Asset is not enabled for borrowing")]
    AssetNotBorrowEnabled,
    #[msg("Reserve supply cap exceeded")]
    SupplyCapExceeded,
    #[msg("Margin account collateral cap exceeded for this asset")]
    CollateralCapExceeded,
    #[msg("Reserve borrow cap exceeded")]
    BorrowCapExceeded,
    #[msg("Reserve does not have enough available liquidity")]
    InsufficientLiquidity,
    #[msg("Margin account does not have enough credited collateral")]
    InsufficientCollateral,
    #[msg("Lender does not have enough shares")]
    InsufficientShares,
    #[msg("Actual result exceeded the caller's slippage bound")]
    SlippageExceeded,
    #[msg("Margin account already holds the maximum number of active assets")]
    TooManyAssets,
    #[msg("Asset index is already present in the canonical active list")]
    DuplicateAssetIndex,
    #[msg("Remaining accounts did not include every active collateral/debt position")]
    IncompletePositionAccounts,
    #[msg("Oracle account is not owned by the expected Pyth receiver program")]
    InvalidOracleOwner,
    #[msg("Oracle account feed ID does not match the registered asset")]
    InvalidPriceFeed,
    #[msg("Oracle price update is older than the configured maximum age")]
    StalePrice,
    #[msg("Oracle price is zero, negative, or otherwise invalid")]
    InvalidPrice,
    #[msg("Oracle price confidence interval is wider than the configured maximum")]
    ConfidenceTooWide,
    #[msg("Projected borrow health factor would fall below the required minimum")]
    HealthFactorTooLow,
    #[msg("Margin account is healthy and is not eligible for liquidation")]
    PositionHealthy,
    #[msg("Liquidation repay amount exceeds the configured close factor")]
    CloseFactorExceeded,
    #[msg("Margin account still has outstanding debt")]
    OutstandingDebt,
    #[msg("Margin account still has open collateral or debt positions")]
    NonEmptyMargin,
    #[msg("Arithmetic overflow")]
    MathOverflow,
    #[msg("Arithmetic underflow")]
    MathUnderflow,
    #[msg("Division by zero")]
    DivisionByZero,
    #[msg("Clock timestamp moved backwards relative to stored state")]
    TimestampRegression,
    #[msg("Reserve vault/share-mint accounting invariant failed")]
    VaultAccountingInvariantFailed,
    #[msg("Amount must be greater than zero")]
    ZeroAmount,
    #[msg("Duplicate account passed where distinct roles are required")]
    DuplicateAccount,
    #[msg("Reserve is already registered for this asset")]
    ReserveAlreadyExists,
    #[msg("Risk parameter configuration is invalid")]
    InvalidRiskParameters,
    #[msg("Interest-rate model configuration is invalid")]
    InvalidRateModel,
    #[msg("Requested amount is below the required initial supply floor")]
    BelowMinimumInitialSupply,
    #[msg("Collateral position is not empty")]
    NonEmptyCollateralPosition,
    #[msg("Debt position is not empty")]
    NonEmptyDebtPosition,
    #[msg("Collateral vault still holds a raw token balance")]
    NonEmptyVault,
    #[msg("Kamino program id is invalid")]
    InvalidKaminoProgram,
    #[msg("Kamino market/reserve accounts do not match the registered strategy")]
    InvalidKaminoAccounts,
    #[msg("Lite strategy is disabled")]
    LiteStrategyDisabled,
    #[msg("A lite position already exists for this margin account")]
    LitePositionExists,
    #[msg("No lite position found for this margin account")]
    NoLitePosition,
    #[msg("Leverage is outside the allowed range")]
    InvalidLeverage,
    #[msg("Jupiter margin swap route or balance delta is invalid")]
    InvalidSwapRoute,
    #[msg("A previous lite_reduce_redeem hasn't been repaid yet by lite_reduce_repay")]
    PendingLiteRedeem,
    #[msg("No pending redeem to repay — call lite_reduce_redeem first")]
    NoPendingLiteRedeem,
}
