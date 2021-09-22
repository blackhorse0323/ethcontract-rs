//! Implementation of typed gas price estimation.

use crate::errors::ExecutionError;
use crate::GasPrice;
use primitive_types::U256;
use web3::api::Web3;
use web3::types::U64;
use web3::Transport;

/// The gas price setting to use.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TypedGasPrice {
    /// The standard estimated gas price from the node, this is usually the
    /// median gas price from the last few blocks. This is the default gas price
    /// used by transactions.
    Legacy(GasPrice),

    /// Specify a specific gas price (eip1559) to use for the transaction. This will cause
    /// the transaction `SendFuture` to not query the node for a gas price
    /// estimation.
    Eip1559((U256, U256)),
}

impl TypedGasPrice {
    /// Returns `Some(value)` if the gas price is explicitly specified, `None`
    /// otherwise.
    pub fn legacy(&self) -> Option<U256> {
        match self {
            TypedGasPrice::Legacy(x) => x.value(),
            _ => None,
        }
    }

    /// Returns `Some(value)` if the gas price is explicitly specified, `None`
    /// otherwise.
    pub fn eip1559(&self) -> Option<(U256, U256)> {
        match self {
            TypedGasPrice::Eip1559(x) => Some(*x),
            _ => None,
        }
    }

    pub fn transaction_type(&self) -> Option<U64> {
        match self {
            TypedGasPrice::Legacy(_) => None,
            TypedGasPrice::Eip1559(_) => Some(2.into()),
        }
    }

    /// Resolves the gas price into a value. Returns a future that resolves once
    /// the gas price is calculated as this may require contacting the node for
    /// gas price estimates in the case of `GasPrice::Standard` and
    /// `GasPrice::Scaled`.
    pub async fn resolve<T: Transport>(
        self,
        web3: &Web3<T>,
    ) -> Result<TypedGasPriceResolved, ExecutionError> {
        let resolved_gas_price = match self {
            TypedGasPrice::Legacy(x) => TypedGasPriceResolved::Legacy(x.resolve(web3).await?),
            TypedGasPrice::Eip1559(x) => TypedGasPriceResolved::Eip1559(x),
        };

        Ok(resolved_gas_price)
    }

    // Resolves the gas price into an `Option<U256>` intendend to be used by a
    // `TransactionRequest`. Note that `TransactionRequest`s gas price default
    // to the node's estimate (i.e. `GasPrice::Standard`) when omitted, so this
    // allows for a small optimization by foregoing a JSON RPC request.
    // pub async fn resolve_for_transaction_request<T: Transport>(
    //     self,
    //     web3: &Web3<T>,
    // ) -> Result<Option<U256>, ExecutionError> {
    //     let gas_price = match self {
    //         GasPrice::Standard => None,
    //         _ => Some(self.resolve(web3).await?),
    //     };

    //     Ok(gas_price)
    // }
}

impl Default for TypedGasPrice {
    fn default() -> Self {
        TypedGasPrice::Legacy(GasPrice::Standard)
    }
}

pub enum TypedGasPriceResolved {
    Legacy(U256),
    Eip1559((U256, U256)),
}

impl TypedGasPriceResolved {
    pub fn resolve_for_transaction(
        &self,
    ) -> (Option<U256>, Option<U256>, Option<U256>, Option<U64>) {
        match self {
            TypedGasPriceResolved::Legacy(value) => (Some(*value), None, None, None),
            TypedGasPriceResolved::Eip1559(pair) => {
                (None, Some(pair.0), Some(pair.1), Some(2.into()))
            }
        }
    }
}

// impl From<U256> for GasPrice {
//     fn from(value: U256) -> Self {
//         GasPrice::Value(value)
//     }
// }

// macro_rules! impl_gas_price_from_integer {
//     ($($t:ty),* $(,)?) => {
//         $(
//             impl From<$t> for GasPrice {
//                 fn from(value: $t) -> Self {
//                     GasPrice::Value(value.into())
//                 }
//             }
//         )*
//     };
// }

// impl_gas_price_from_integer! {
//     i8, i16, i32, i64, i128, isize,
//     u8, u16, u32, u64, u128, usize,
// }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test::prelude::*;

    #[test]
    fn gas_price_scalling() {
        assert_eq!(scale_gas_price(1_000_000.into(), 2.0), 2_000_000.into());
        assert_eq!(scale_gas_price(1_000_000.into(), 1.5), 1_500_000.into());
        assert_eq!(scale_gas_price(U256::MAX, 2.0), U256::MAX);
    }

    #[test]
    fn resolve_gas_price() {
        let mut transport = TestTransport::new();
        let web3 = Web3::new(transport.clone());

        let gas_price = U256::from(1_000_000);

        transport.add_response(json!(gas_price));
        assert_eq!(
            GasPrice::Standard
                .resolve(&web3)
                .immediate()
                .expect("error resolving gas price"),
            gas_price
        );
        transport.assert_request("eth_gasPrice", &[]);
        transport.assert_no_more_requests();

        transport.add_response(json!(gas_price));
        assert_eq!(
            GasPrice::Scaled(2.0)
                .resolve(&web3)
                .immediate()
                .expect("error resolving gas price"),
            gas_price * 2
        );
        transport.assert_request("eth_gasPrice", &[]);
        transport.assert_no_more_requests();

        assert_eq!(
            GasPrice::Value(gas_price)
                .resolve(&web3)
                .immediate()
                .expect("error resolving gas price"),
            gas_price
        );
        transport.assert_no_more_requests();
    }

    #[test]
    fn resolve_gas_price_for_transaction_request() {
        let mut transport = TestTransport::new();
        let web3 = Web3::new(transport.clone());

        let gas_price = U256::from(1_000_000);

        assert_eq!(
            GasPrice::Standard
                .resolve_for_transaction_request(&web3)
                .immediate()
                .expect("error resolving gas price"),
            None
        );
        transport.assert_no_more_requests();

        transport.add_response(json!(gas_price));
        assert_eq!(
            GasPrice::Scaled(2.0)
                .resolve_for_transaction_request(&web3)
                .immediate()
                .expect("error resolving gas price"),
            Some(gas_price * 2),
        );
        transport.assert_request("eth_gasPrice", &[]);
        transport.assert_no_more_requests();

        assert_eq!(
            GasPrice::Value(gas_price)
                .resolve_for_transaction_request(&web3)
                .immediate()
                .expect("error resolving gas price"),
            Some(gas_price)
        );
        transport.assert_no_more_requests();
    }
}
