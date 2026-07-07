use alloy_primitives::Bytes;
use arb_storage_errors::StorageError;
use core::error::Error;
use revm::precompile::{PrecompileError, PrecompileHalt, PrecompileOutput, PrecompileResult};

/// Errors raised by Arbitrum precompiles.
///
/// `Revert` and `OutOfGas` are user-visible: the transaction reverts and the
/// surrounding block continues. `Fatal` indicates an infrastructure failure
/// (database error, broken storage invariant) and must abort the block.
#[derive(thiserror::Error, Debug)]
pub enum ArbPrecompileError {
    /// User-visible revert. The block continues; the tx reverts.
    #[error("revert: {data:?}")]
    Revert {
        /// Optional Solidity custom-error selector that prefixes `data`.
        selector: Option<[u8; 4]>,
        /// ABI-encoded revert payload returned to the caller.
        data: Bytes,
        /// Precompile gas accounted up to the point of the revert.
        gas_used: u64,
    },

    /// Out of gas during precompile execution. User-visible.
    #[error("out of gas")]
    OutOfGas,

    /// Infrastructure failure. Maps to `PrecompileError::Fatal` so the block
    /// aborts instead of producing a user-visible revert.
    #[error("fatal precompile error: {0}")]
    Fatal(#[source] Box<dyn Error + Send + Sync>),
}

impl ArbPrecompileError {
    /// Wraps any [`Error`] as a [`ArbPrecompileError::Fatal`].
    pub fn fatal<E>(err: E) -> Self
    where
        E: Error + Send + Sync + 'static,
    {
        Self::Fatal(Box::new(err))
    }

    /// Builds a [`Revert`](Self::Revert) carrying empty data and the
    /// current precompile-gas accumulator.
    pub fn empty_revert(gas_used: u64) -> Self {
        Self::Revert {
            selector: None,
            data: Bytes::new(),
            gas_used,
        }
    }

    /// Converts this error into a [`PrecompileResult`], capped by `gas_limit`.
    ///
    /// `Revert` produces a revert-status output carrying the configured
    /// selector and payload. `OutOfGas` halts consuming all gas; `Fatal`
    /// becomes an `Err`-variant `PrecompileError` aborting the block.
    pub fn into_precompile_result(self, gas_limit: u64) -> PrecompileResult {
        match self {
            Self::Revert {
                selector,
                data,
                gas_used,
            } => {
                let payload = match selector {
                    Some(sel) => {
                        let mut bytes = Vec::with_capacity(4 + data.len());
                        bytes.extend_from_slice(&sel);
                        bytes.extend_from_slice(&data);
                        Bytes::from(bytes)
                    }
                    None => data,
                };
                Ok(PrecompileOutput::revert(
                    gas_used.min(gas_limit),
                    payload,
                    0,
                ))
            }
            Self::OutOfGas => Ok(PrecompileOutput::halt(PrecompileHalt::OutOfGas, 0)),
            Self::Fatal(source) => Err(PrecompileError::Fatal(source.to_string())),
        }
    }
}

/// Result type carried by precompile method handlers until the dispatch
/// boundary converts it into revm's [`PrecompileResult`].
pub type ArbPrecompileResult = Result<PrecompileOutput, ArbPrecompileError>;

impl ArbPrecompileError {
    /// Converts an error escaping a handler without boundary post-processing:
    /// user-visible variants halt the frame consuming all gas; `Fatal` aborts
    /// the block.
    pub fn into_halt_result(self) -> PrecompileResult {
        match self {
            Self::OutOfGas => Ok(PrecompileOutput::halt(PrecompileHalt::OutOfGas, 0)),
            Self::Revert { .. } => Ok(PrecompileOutput::halt(PrecompileHalt::other("revert"), 0)),
            Self::Fatal(source) => Err(PrecompileError::Fatal(source.to_string())),
        }
    }
}

/// Unwraps a `Result<_, ArbPrecompileError>` inside a handler returning
/// [`PrecompileResult`], escaping via [`ArbPrecompileError::into_halt_result`].
macro_rules! try_or_halt {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(err) => return crate::ArbPrecompileError::from(err).into_halt_result(),
        }
    };
}
pub(crate) use try_or_halt;

impl From<StorageError> for ArbPrecompileError {
    fn from(err: StorageError) -> Self {
        Self::Fatal(Box::new(err))
    }
}
