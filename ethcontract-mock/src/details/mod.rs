//! Implementation details of mock node.

use std::collections::HashMap;
use std::convert::TryFrom;
use std::future::ready;
use std::sync::{Arc, Mutex};

use ethcontract::common::abi::{Function, StateMutability, Token};
use ethcontract::common::hash::H32;
use ethcontract::common::{Abi, FunctionExt};
use ethcontract::jsonrpc::serde::Serialize;
use ethcontract::jsonrpc::serde_json::to_value;
use ethcontract::jsonrpc::{Call, MethodCall, Params, Value};
use ethcontract::tokens::Tokenize;
use ethcontract::web3::types::{
    Bytes, CallRequest, TransactionReceipt, TransactionRequest, U256, U64,
};
use ethcontract::web3::{helpers, Error, RequestId, Transport};
use ethcontract::{Address, BlockNumber, H160, H256};
use parse::Parser;
use sign::verify;

use crate::details::transaction::TransactionResult;
use crate::range::TimesRange;
use crate::CallContext;
use std::any::Any;

mod default;
mod parse;
mod sign;
mod transaction;

/// Mock transport.
#[derive(Clone)]
pub struct MockTransport {
    /// Mutable state.
    state: Arc<Mutex<MockTransportState>>,
}

/// Internal transport state, protected by a mutex.
struct MockTransportState {
    /// Chain ID.
    chain_id: u64,

    /// Current gas price.
    gas_price: u64,

    /// This counter is used to keep track of prepared calls.
    request_id: RequestId,

    /// This counter is used to keep track of mined blocks.
    block: u64,

    /// This counter is used to generate contract addresses.
    address: u64,

    /// Nonce for account.
    nonce: HashMap<Address, u64>,

    /// Deployed mocked contracts.
    contracts: HashMap<Address, Contract>,

    /// Receipts for already performed transactions.
    receipts: HashMap<H256, TransactionReceipt>,
}

impl MockTransport {
    /// Creates a new transport.
    pub fn new(chain_id: u64) -> Self {
        MockTransport {
            state: Arc::new(Mutex::new(MockTransportState {
                chain_id,
                gas_price: 1,
                request_id: 0,
                block: 0,
                address: 0,
                nonce: HashMap::new(),
                contracts: HashMap::new(),
                receipts: HashMap::new(),
            })),
        }
    }

    /// Deploys a new contract with the given ABI.
    pub fn deploy(&self, abi: &Abi) -> Address {
        let mut state = self.state.lock().unwrap();

        state.address += 1;
        let address = H160::from_low_u64_be(state.address);

        state.contracts.insert(address, Contract::new(address, abi));

        address
    }

    pub fn update_gas_price(&self, gas_price: u64) {
        let mut state = self.state.lock().unwrap();
        state.gas_price = gas_price;
    }

    pub fn expect<P: Tokenize + Send + 'static, R: Tokenize + Send + 'static>(
        &self,
        address: Address,
        signature: H32,
    ) -> (usize, usize) {
        let mut state = self.state.lock().unwrap();
        let method = state.method(address, signature);
        method.expect::<P, R>()
    }
}

impl MockTransportState {
    /// Returns contract at the given address, panics if contract does not exist.
    fn contract(&mut self, address: Address) -> &mut Contract {
        match self.contracts.get_mut(&address) {
            Some(contract) => contract,
            None => panic!("there is no mocked contract with address {:#x}", address),
        }
    }

    /// Returns contract's method.
    fn method(&mut self, address: Address, signature: H32) -> &mut Method {
        self.contract(address).method(signature)
    }
}

impl Transport for MockTransport {
    type Out = std::future::Ready<Result<Value, Error>>;

    /// Prepares an RPC call for given method with parameters.
    ///
    /// We don't have to deal with network issues, so we are relaxed about
    /// request IDs, idempotency checks and so on.
    fn prepare(&self, method: &str, params: Vec<Value>) -> (RequestId, Call) {
        let mut state = self.state.lock().unwrap();

        let id = state.request_id;
        state.request_id += 1;

        let request = helpers::build_request(id, method, params);

        (id, request)
    }

    /// Executes a prepared RPC call.
    fn send(&self, _: RequestId, request: Call) -> Self::Out {
        let MethodCall { method, params, .. } = match request {
            Call::MethodCall(method_call) => method_call,
            Call::Notification(_) => panic!("rpc notifications are not supported"),
            _ => panic!("unknown or invalid rpc call type"),
        };

        let params = match params {
            Params::None => Vec::new(),
            Params::Array(array) => array,
            Params::Map(_) => panic!("passing arguments by map is not supported"),
        };

        let result = match method.as_str() {
            "eth_blockNumber" => {
                let name = "eth_blockNumber";
                self.block_number(Parser::new(name, params))
            }
            "eth_chainId" => {
                let name = "eth_chainId";
                self.chain_id(Parser::new(name, params))
            }
            "eth_getTransactionCount" => {
                let name = "eth_getTransactionCount";
                self.transaction_count(Parser::new(name, params))
            }
            "eth_gasPrice" => {
                let name = "eth_gasPrice";
                self.gas_price(Parser::new(name, params))
            }
            "eth_estimateGas" => {
                let name = "eth_estimateGas";
                self.estimate_gas(Parser::new(name, params))
            }
            "eth_call" => {
                let name = "eth_call";
                self.call(Parser::new(name, params))
            }
            "eth_sendTransaction" => {
                let name = "eth_sendTransaction";
                self.send_transaction(Parser::new(name, params))
            }
            "eth_sendRawTransaction" => {
                let name = "eth_sendRawTransaction";
                self.send_raw_transaction(Parser::new(name, params))
            }
            "eth_getTransactionReceipt" => {
                let name = "eth_getTransactionReceipt";
                self.get_transaction_receipt(Parser::new(name, params))
            }
            unsupported => panic!("mock node does not support rpc method {:?}", unsupported),
        };

        ready(result)
    }
}

impl MockTransport {
    fn block_number(&self, args: Parser) -> Result<Value, Error> {
        args.done();

        let state = self.state.lock().unwrap();
        Self::ok(&U64::from(state.block))
    }

    fn chain_id(&self, args: Parser) -> Result<Value, Error> {
        args.done();

        let state = self.state.lock().unwrap();
        Self::ok(&U256::from(state.chain_id))
    }

    fn transaction_count(&self, mut args: Parser) -> Result<Value, Error> {
        let address: Address = args.arg();
        let block: Option<BlockNumber> = args.block_number_opt();
        args.done();

        let block = block.unwrap_or(BlockNumber::Pending);
        let state = self.state.lock().unwrap();
        let transaction_count = match block {
            BlockNumber::Earliest => 0,
            BlockNumber::Number(n) if n == 0.into() => 0,
            BlockNumber::Number(n) if n != state.block.into() => {
                panic!("mock node does not support returning transaction count for specific block number");
            }
            _ => state.nonce.get(&address).copied().unwrap_or(0),
        };
        Self::ok(&U256::from(transaction_count))
    }

    fn gas_price(&self, args: Parser) -> Result<Value, Error> {
        args.done();

        let state = self.state.lock().unwrap();
        Self::ok(&U256::from(state.gas_price))
    }

    fn estimate_gas(&self, mut args: Parser) -> Result<Value, Error> {
        let request: CallRequest = args.arg();
        let block: Option<BlockNumber> = args.block_number_opt();
        args.done();

        let state = self.state.lock().unwrap();

        let block = block.unwrap_or(BlockNumber::Pending);
        match block {
            BlockNumber::Earliest => {
                panic!("mock node does not support executing methods on earliest block");
            }
            BlockNumber::Number(n) if n != state.block.into() => {
                panic!("mock node does not support executing methods on non-last block");
            }
            _ => (),
        }

        match request.to {
            None => panic!("call's 'to' field is empty"),
            Some(to) => to,
        };

        // TODO:
        //
        // We could look up contract's method, match an expectation,
        // and see if the expectation defines gas price.
        //
        // So, for example, this code:
        //
        // ```
        // contract
        //     .expect_method(signature)
        //     .with(matcher)
        //     .gas(100);
        // ```
        //
        // Indicates that call to the method with the given signature
        // requires 100 gas.
        //
        // When estimating gas, we'll check all expectation as if we're
        // executing a method, but we won't mark any expectation as fulfilled.

        Self::ok(&U256::from(1))
    }

    fn call(&self, mut args: Parser) -> Result<Value, Error> {
        let request: CallRequest = args.arg();
        let block: Option<BlockNumber> = args.block_number_opt();

        let mut state = self.state.lock().unwrap();

        let block = block.unwrap_or(BlockNumber::Pending);
        match block {
            BlockNumber::Earliest => {
                panic!("mock node does not support executing methods on earliest block");
            }
            BlockNumber::Number(n) if n != state.block.into() => {
                panic!("mock node does not support executing methods on non-last block");
            }
            _ => (),
        }

        let from = request.from.unwrap_or_default();
        let to = match request.to {
            None => panic!("call's 'to' field is empty"),
            Some(to) => to,
        };

        let nonce = state.nonce.get(&from).copied().unwrap_or(0);

        let gas_price = state.gas_price;

        let contract = state.contract(to);

        let context = CallContext {
            is_view_call: true,
            from: request.from.unwrap_or_default(),
            to,
            nonce: U256::from(nonce),
            gas: request.gas.unwrap_or_else(|| U256::from(1)),
            gas_price: request.gas.unwrap_or_else(|| U256::from(gas_price)),
            value: request.value.unwrap_or_default(),
        };

        let data = request.data.unwrap_or_default();

        let result = contract.process_tx(context, &data.0);

        match result.result {
            Ok(data) => Self::ok(Bytes(data)),
            Err(err) => Err(Error::Rpc(ethcontract::jsonrpc::Error {
                code: ethcontract::jsonrpc::ErrorCode::ServerError(0),
                message: format!("execution reverted: {}", err),
                data: None,
            })),
        }
    }

    fn send_transaction(&self, mut args: Parser) -> Result<Value, Error> {
        let _request: TransactionRequest = args.arg();
        args.done();

        // TODO:
        //
        // We could support signing if user adds accounts with their private
        // keys during mock setup.

        panic!("mock node can't sign transactions, use offline signing with private key");
    }

    fn send_raw_transaction(&self, mut args: Parser) -> Result<Value, Error> {
        let raw_tx: Bytes = args.arg();
        args.done();

        let mut state = self.state.lock().unwrap();

        let tx = verify(&raw_tx.0, state.chain_id);

        let nonce = state.nonce.entry(tx.from).or_insert(0);
        if *nonce != tx.nonce.as_u64() {
            panic!(
                "nonce mismatch for account {:#x}: expected {}, actual {}",
                tx.from,
                tx.nonce.as_u64(),
                nonce
            );
        }
        *nonce += 1;

        let contract = state.contract(tx.to);

        let context = CallContext {
            is_view_call: false,
            from: tx.from,
            to: tx.to,
            nonce: tx.nonce,
            gas: tx.gas,
            gas_price: tx.gas_price,
            value: tx.value,
        };

        let result = contract.process_tx(context, &tx.data);

        state.block += 1;

        let receipt = TransactionReceipt {
            transaction_hash: tx.hash,
            transaction_index: U64::from(0),
            block_hash: None,
            block_number: Some(U64::from(state.block)),
            from: tx.from,
            to: Some(tx.to),
            cumulative_gas_used: U256::from(1),
            gas_used: None,
            contract_address: None,
            logs: vec![],
            status: Some(U64::from(result.result.is_ok() as u64)),
            root: None,
            logs_bloom: Default::default(),
            transaction_type: None,
        };

        state.receipts.insert(tx.hash, receipt);

        state.block += result.confirmations;

        Self::ok(tx.hash)
    }

    fn get_transaction_receipt(&self, mut args: Parser) -> Result<Value, Error> {
        let transaction: H256 = args.arg();
        args.done();

        let state = self.state.lock().unwrap();

        Self::ok(state.receipts.get(&transaction).unwrap_or_else(|| {
            panic!("there is no transaction with hash {:#x}", transaction);
        }))
    }

    fn ok<T: Serialize>(t: T) -> Result<Value, Error> {
        Ok(to_value(t).unwrap())
    }
}

impl std::fmt::Debug for MockTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MockTransport")
    }
}

/// A mocked contract instance.
struct Contract {
    address: Address,
    methods: HashMap<H32, Method>,
}

impl Contract {
    fn new(address: Address, abi: &Abi) -> Self {
        let mut methods = HashMap::new();

        for functions in abi.functions.values() {
            for function in functions {
                methods.insert(function.selector(), Method::new(address, function.clone()));
            }
        }

        Contract { address, methods }
    }

    fn method(&mut self, signature: H32) -> &mut Method {
        match self.methods.get_mut(&signature) {
            Some(method) => method,
            None => panic!(
                "contract {:#x} doesn't have method with signature 0x{}",
                self.address,
                hex::encode(signature)
            ),
        }
    }

    fn process_tx(&mut self, tx: CallContext, data: &[u8]) -> TransactionResult {
        // TODO:
        //
        // We could support receive/fallback functions if data is empty.

        if data.len() < 4 {
            panic!("transaction has invalid call data");
        }

        let signature = H32::try_from(&data[0..4]).unwrap();
        let method = self.method(signature);

        method.process_tx(tx, data)
    }
}

struct Method {
    /// Description for this method.
    description: String,

    /// ABI of this method.
    function: Function,

    /// Incremented whenever `expectations` vector is cleared to invalidate
    /// expectations API handle.
    generation: usize,

    /// Expectation for this method.
    expectations: Vec<Box<dyn ExpectationApi>>,
}

impl Method {
    /// Creates new method.
    fn new(address: Address, function: Function) -> Self {
        let description = format!("{:?} on contract {:#x}", function.abi_signature(), address);

        Method {
            description,
            function,
            generation: 0,
            expectations: Vec::new(),
        }
    }

    /// Adds new expectation.
    fn expect<P: Tokenize + Send + 'static, R: Tokenize + Send + 'static>(
        &mut self,
    ) -> (usize, usize) {
        let index = self.expectations.len();
        self.expectations.push(Box::new(Expectation::<P, R>::new()));
        (index, self.generation)
    }

    /// Executes a transaction or a call.
    fn process_tx(&mut self, tx: CallContext, data: &[u8]) -> TransactionResult {
        if !tx.value.is_zero() && self.function.state_mutability != StateMutability::Payable {
            panic!(
                "call to non-payable {} with non-zero value {}",
                self.description, tx.value,
            )
        }

        let params = self
            .function
            .decode_input(&data[4..])
            .unwrap_or_else(|e| panic!("unable to decode input for {}: {:?}", self.description, e));

        for expectation in self.expectations.iter_mut() {
            if expectation.is_active() {
                // We clone `params` for each expectation, which could potentially
                // be inefficient. We assume, however, that in most cases there
                // are only a few expectations for a method, and they are likely
                // to be filtered out by `is_active`.
                if let Some(result) =
                expectation.process_tx(&tx, &self.description, &self.function, params.clone())
                {
                    return result;
                }
            }
        }

        panic!("unexpected call to {}", self.description)
    }
}

trait ExpectationApi: Send {
    /// Convert this expectation to `Any` for downcast.
    fn as_any(&mut self) -> &mut dyn Any;

    /// Checks if this expectation is active, i.e., still can be called.
    fn is_active(&self) -> bool;

    /// Matches and processes this transaction.
    ///
    /// If transaction matches this expectation, processes it and returns
    /// its result. Otherwise, returns `None`.
    fn process_tx(
        &mut self,
        tx: &CallContext,
        description: &str,
        function: &Function,
        params: Vec<Token>,
    ) -> Option<TransactionResult>;
}

struct Expectation<P: Tokenize + Send + 'static, R: Tokenize + Send + 'static> {
    /// How many times should this expectation be called.
    times: TimesRange,

    /// How many times was it actually called.
    used: usize,

    /// Indicates that predicate for this expectation has been called at least
    /// once. Expectations shouldn't be changed after that happened.
    checked: bool,

    /// How many blocks should node skip for confirmation to be successful.
    confirmations: u64,

    /// Only consider this expectation if predicate returns `true`.
    predicate: Predicate<P>,

    /// Should this expectation match view calls?
    allow_calls: bool,

    /// Should this expectation match transactions?
    allow_transactions: bool,

    /// Function to generate method's return value.
    returns: Returns<P, R>,

    /// Handle for when this expectation belongs to a sequence.
    sequence: Option<mockall::SeqHandle>,
}

impl<P: Tokenize + Send + 'static, R: Tokenize + Send + 'static> Expectation<P, R> {
    fn new() -> Self {
        Expectation {
            times: TimesRange::default(),
            used: 0,
            checked: false,
            confirmations: 0,
            predicate: Predicate::None,
            allow_calls: true,
            allow_transactions: true,
            returns: Returns::Default,
            sequence: None,
        }
    }
}

impl<P: Tokenize + Send + 'static, R: Tokenize + Send + 'static> ExpectationApi
for Expectation<P, R>
{
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }

    fn is_active(&self) -> bool {
        self.times.can_call(self.used)
    }

    fn process_tx(
        &mut self,
        tx: &CallContext,
        description: &str,
        function: &Function,
        params: Vec<Token>,
    ) -> Option<TransactionResult> {
        self.checked = true;

        if tx.is_view_call && !self.allow_calls || !tx.is_view_call && !self.allow_transactions {
            return None;
        }

        if !self.times.can_call(self.used) {
            return None;
        }

        let param = P::from_token(Token::Tuple(params))
            .unwrap_or_else(|e| panic!("unable to decode input for {}: {:?}", description, e));

        if !self.predicate.can_call(tx, &param) {
            return None;
        }

        self.used += 1;
        if let Some(sequence) = &self.sequence {
            sequence.verify(description);

            if self.used == self.times.lower_bound() {
                sequence.satisfy();
            }
        }

        let result = self
            .returns
            .process_tx(function, tx, param)
            .map(|result| ethcontract::common::abi::encode(&[result]));

        Some(TransactionResult {
            result,
            confirmations: self.confirmations,
        })
    }
}

enum Predicate<P: Tokenize + Send + 'static> {
    None,
    Predicate(Box<dyn predicates::Predicate<P> + Send>),
    Function(Box<dyn Fn(&P) -> bool + Send>),
    TxFunction(Box<dyn Fn(&CallContext, &P) -> bool + Send>),
}

impl<P: Tokenize + Send + 'static> Predicate<P> {
    fn can_call(&self, tx: &CallContext, param: &P) -> bool {
        match self {
            Predicate::None => true,
            Predicate::Predicate(p) => p.eval(param),
            Predicate::Function(f) => f(param),
            Predicate::TxFunction(f) => f(tx, param),
        }
    }
}

enum Returns<P: Tokenize + Send + 'static, R: Tokenize + Send + 'static> {
    Default,
    Error(String),
    Const(Token),
    Function(Box<dyn Fn(P) -> Result<R, String> + Send>),
    TxFunction(Box<dyn Fn(&CallContext, P) -> Result<R, String> + Send>),
}

impl<P: Tokenize + Send + 'static, R: Tokenize + Send + 'static> Returns<P, R> {
    fn process_tx(&self, function: &Function, tx: &CallContext, param: P) -> Result<Token, String> {
        match self {
            Returns::Default => Ok(default::default_tuple(
                function.inputs.iter().map(|i| &i.kind),
            )),
            Returns::Error(error) => Err(error.clone()),
            Returns::Const(token) => Ok(token.clone()),
            Returns::Function(f) => f(param).map(Tokenize::into_token),
            Returns::TxFunction(f) => f(tx, param).map(Tokenize::into_token),
        }
    }
}
