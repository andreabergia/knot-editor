//! Typed asynchronous operation adapters and automatically linked declarations.
//!
//! Only adapters erase handler types. Application contexts own their foreground
//! access and must never return a borrowed editor model from that access.

//!
//! A declaration supplies its typed adapter and linked descriptor:
//! ```no_run
//! mod host { pub use knot::declarations; }
//! use host::declarations::*;
//! #[derive(Clone, Debug, PartialEq)]
//! struct Arguments;
//! impl HostArguments for Arguments {
//!     fn decode(_: &mut dyn ArgumentReader) -> Result<Self, DecodeError> { Ok(Self) }
//! }
//! #[derive(HostError)]
//! enum Error { #[error(code = "Failure", message = "Failure")] Failure }
//! #[host_operation("example")]
//! async fn example(context: (), arguments: Arguments) -> Result<(), Error> { Ok(()) }
//! fn main() { Responder::<example>::new().respond(Ok(())); }
//! ```
//!
//! Declarations require owned contexts and asynchronous handlers:
//! ```compile_fail
//! mod host { pub use knot::declarations; }
//! use host::declarations::host_operation;
//! #[host_operation("invalid")]
//! async fn invalid(context: &str, arguments: ()) -> Result<(), ()> { Ok(()) }
//! fn main() {}
//! ```
//!
//! ```compile_fail
//! mod host { pub use knot::declarations; }
//! use host::declarations::host_operation;
//! #[host_operation("invalid")]
//! fn invalid(context: (), arguments: ()) -> Result<(), ()> { Ok(()) }
//! fn main() {}
//! ```
//!
//! Error codes must be declared explicitly:
//! ```compile_fail
//! mod host { pub use knot::declarations; }
//! use host::declarations::HostError;
//! #[derive(HostError)]
//! enum Error { RenamableRustVariant }
//! fn main() {}
//! ```
//!
//! Typed responders cannot accept another success type:
//! ```compile_fail
//! mod host { pub use knot::declarations; }
//! use host::declarations::*;
//! #[derive(Clone, Debug, PartialEq)]
//! struct Arguments;
//! impl HostArguments for Arguments {
//!     fn decode(_: &mut dyn ArgumentReader) -> Result<Self, DecodeError> { Ok(Self) }
//! }
//! #[derive(HostError)]
//! enum Error { #[error(code = "Failure", message = "Failure")] Failure }
//! #[host_operation("example")]
//! async fn example(context: (), arguments: Arguments) -> Result<(), Error> { Ok(()) }
//! fn main() { Responder::<example>::new().respond(Ok(String::from("unrelated response"))); }
//! ```

use std::{any::Any, collections::BTreeMap, fmt, future::Future, pin::Pin, sync::Arc};

use super::protocol::DocumentDestination;

pub use knot_handler_macros::{HostError, host_operation};

pub type HandlerFuture<T> = Pin<Box<dyn Future<Output = T>>>;

/// Boundary decoding reads recognized fields directly, without author JSON hooks.
pub trait ArgumentReader {
    fn optional_integer(&mut self, index: usize) -> Result<Option<u64>, DecodeError>;
    fn command_arguments(
        &mut self,
        index: usize,
    ) -> Result<super::protocol::CommandArgumentValue, DecodeError>;
    fn outcome(&mut self, index: usize) -> Result<super::protocol::CommandOutcome, DecodeError>;
    fn range(&mut self, index: usize) -> Result<Option<super::protocol::ByteRange>, DecodeError>;
    fn edits(&mut self, index: usize) -> Result<Vec<super::protocol::TextEdit>, DecodeError>;
    fn contributions(
        &mut self,
        index: usize,
    ) -> Result<Vec<super::protocol::EditorContribution>, DecodeError>;
    fn optional_text(
        &mut self,
        index: usize,
        message: &'static str,
    ) -> Result<Option<String>, DecodeError>;
    fn text_list(&mut self, index: usize) -> Result<Vec<String>, DecodeError>;
    fn integer(&mut self, index: usize) -> Result<u64, DecodeError>;
    fn text(&mut self, index: usize, message: &'static str) -> Result<String, DecodeError>;
    fn destination(&mut self, index: usize) -> Result<Option<DocumentDestination>, DecodeError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodeError {
    Type(&'static str),
    Range(&'static str),
    Host(TransportError),
}

pub trait HostArguments: Clone + fmt::Debug + PartialEq + Send + Sync + 'static {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError>;
    fn provider_mutation(&self) -> Option<super::protocol::ProviderIdentity> {
        None
    }
}

pub trait HostSuccess: 'static {
    fn encode(self) -> Result<EncodedValue, DecodeError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EncodedValue {
    Undefined,
    Snapshot(super::protocol::TextSnapshot),
    Json(serde_json::Value),
}

impl HostSuccess for () {
    fn encode(self) -> Result<EncodedValue, DecodeError> {
        Ok(EncodedValue::Undefined)
    }
}

impl HostArguments for () {
    fn decode(_: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(())
    }
}

impl<T: serde::Serialize + 'static> HostSuccess for Vec<T> {
    fn encode(self) -> Result<EncodedValue, DecodeError> {
        encode_json(self)
    }
}

impl<T: serde::Serialize + 'static> HostSuccess for Option<T> {
    fn encode(self) -> Result<EncodedValue, DecodeError> {
        encode_json(self)
    }
}

impl HostSuccess for super::protocol::WorkbenchObservation {
    fn encode(self) -> Result<EncodedValue, DecodeError> {
        encode_json(self)
    }
}

impl HostSuccess for super::protocol::WorkspaceInfo {
    fn encode(self) -> Result<EncodedValue, DecodeError> {
        encode_json(self)
    }
}

impl HostSuccess for super::protocol::PresentationCloseOutcome {
    fn encode(self) -> Result<EncodedValue, DecodeError> {
        encode_json(self)
    }
}

impl HostSuccess for u64 {
    fn encode(self) -> Result<EncodedValue, DecodeError> {
        encode_json(self)
    }
}
impl HostSuccess for super::protocol::BufferSubscriptionId {
    fn encode(self) -> Result<EncodedValue, DecodeError> {
        encode_json(self)
    }
}
impl HostSuccess for super::protocol::TextSnapshot {
    fn encode(self) -> Result<EncodedValue, DecodeError> {
        Ok(EncodedValue::Snapshot(self))
    }
}

impl HostSuccess for super::protocol::TreeProviderRegistrationId {
    fn encode(self) -> Result<EncodedValue, DecodeError> {
        encode_json(self)
    }
}
impl HostSuccess for super::protocol::CompletionProviderRegistrationId {
    fn encode(self) -> Result<EncodedValue, DecodeError> {
        encode_json(self)
    }
}

impl HostSuccess for super::protocol::CommandRegistrationId {
    fn encode(self) -> Result<EncodedValue, DecodeError> {
        encode_json(self)
    }
}
impl HostSuccess for super::protocol::CommandInvokeDispatch {
    fn encode(self) -> Result<EncodedValue, DecodeError> {
        encode_json(self)
    }
}

impl HostSuccess for super::protocol::PickerResult {
    fn encode(self) -> Result<EncodedValue, DecodeError> {
        encode_json(self)
    }
}

impl HostSuccess for super::protocol::InputResult {
    fn encode(self) -> Result<EncodedValue, DecodeError> {
        encode_json(self)
    }
}

impl HostSuccess for super::protocol::ConfirmationResult {
    fn encode(self) -> Result<EncodedValue, DecodeError> {
        encode_json(self)
    }
}

impl HostSuccess for super::protocol::FolderPickerResult {
    fn encode(self) -> Result<EncodedValue, DecodeError> {
        encode_json(self)
    }
}

/// Checks native numeric precision before JSON enters V8.
pub fn encode_json(value: impl serde::Serialize) -> Result<EncodedValue, DecodeError> {
    fn exact(value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::Number(number) => {
                const MAX: u64 = (1 << 53) - 1;
                if let Some(value) = number.as_u64() {
                    value <= MAX
                } else if let Some(value) = number.as_i64() {
                    value.unsigned_abs() <= MAX
                } else {
                    number.as_f64().is_some_and(f64::is_finite)
                }
            }
            serde_json::Value::Array(values) => values.iter().all(exact),
            serde_json::Value::Object(values) => values.values().all(exact),
            _ => true,
        }
    }
    let value = serde_json::to_value(value).expect("host payload serialization");
    if !exact(&value) {
        return Err(DecodeError::Host(TransportError::UnsupportedOperation));
    }
    Ok(EncodedValue::Json(value))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostFailure {
    pub code: &'static str,
    pub message: String,
    pub details: serde_json::Value,
}

pub trait HostError: 'static {
    fn code(&self) -> &'static str;
    fn into_failure(self) -> HostFailure;
    fn codes() -> Vec<&'static str>;
}

/// Common failures generated by the runtime transport itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq, HostError)]
pub enum TransportError {
    #[error(code = "Cancelled", message = "Cancelled")]
    Cancelled,
    #[error(code = "UnsupportedOperation", message = "UnsupportedOperation")]
    UnsupportedOperation,
}

/// Shared typed call contract for native host operations and commands.
pub trait AsyncHandler: 'static {
    type Context: 'static;
    type Arguments: 'static;
    type Success: 'static;
    type Error: 'static;
    const NAME: &'static str;
    fn call(
        context: Self::Context,
        arguments: Self::Arguments,
    ) -> HandlerFuture<Result<Self::Success, Self::Error>>;
}

/// Host transport constraints on the shared async handler contract.
pub trait Handler:
    AsyncHandler<Arguments: HostArguments, Success: HostSuccess, Error: HostError>
{
}
impl<H> Handler for H where
    H: AsyncHandler<Arguments: HostArguments, Success: HostSuccess, Error: HostError>
{
}

/// A typed responder accepts only the success/error pair declared by its handler.
pub struct Responder<H: Handler>(std::marker::PhantomData<H>);

impl<H: Handler> Default for Responder<H> {
    fn default() -> Self {
        Self::new()
    }
}

impl<H: Handler> Responder<H> {
    pub fn new() -> Self {
        Self(std::marker::PhantomData)
    }

    pub fn respond(self, result: Result<H::Success, H::Error>) -> DeclaredResponse {
        DeclaredResponse {
            operation: H::NAME,
            result: result
                .map(HostSuccess::encode)
                .map_err(HostError::into_failure),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeclaredResponse {
    pub operation: &'static str,
    pub result: Result<Result<EncodedValue, DecodeError>, HostFailure>,
}

trait ErasedArguments: fmt::Debug + Send + Sync {
    fn as_any(&self) -> &dyn Any;
    fn equals(&self, other: &dyn ErasedArguments) -> bool;
    fn provider_mutation(&self) -> Option<super::protocol::ProviderIdentity>;
}

impl<A: HostArguments> ErasedArguments for A {
    fn provider_mutation(&self) -> Option<super::protocol::ProviderIdentity> {
        HostArguments::provider_mutation(self)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
    fn equals(&self, other: &dyn ErasedArguments) -> bool {
        other.as_any().downcast_ref::<A>() == Some(self)
    }
}

#[derive(Clone, Debug)]
pub struct DeclaredRequest {
    pub descriptor: &'static OperationDescriptor,
    arguments: Arc<dyn ErasedArguments>,
}

impl PartialEq for DeclaredRequest {
    fn eq(&self, other: &Self) -> bool {
        self.descriptor.name == other.descriptor.name
            && self.arguments.equals(other.arguments.as_ref())
    }
}

impl DeclaredRequest {
    pub fn provider_mutation(&self) -> Option<super::protocol::ProviderIdentity> {
        self.arguments.provider_mutation()
    }

    #[cfg(test)]
    pub fn new<H: Handler>(arguments: H::Arguments) -> Self {
        let descriptor = registry().get(H::NAME).expect("registered handler");
        assert_eq!((descriptor.handler_type)(), std::any::TypeId::of::<H>());
        Self {
            descriptor,
            arguments: Arc::new(arguments),
        }
    }

    #[cfg(test)]
    pub fn arguments<H: Handler>(&self) -> Option<&H::Arguments> {
        (self.descriptor.name == H::NAME)
            .then(|| self.arguments.as_any().downcast_ref::<H::Arguments>())
            .flatten()
    }

    pub fn run<C: 'static>(self, context: C) -> HandlerFuture<DeclaredResponse> {
        (self.descriptor.invoke)(Box::new(context), self.arguments)
    }
}

type InvokeAdapter = fn(Box<dyn Any>, Arc<dyn ErasedArguments>) -> HandlerFuture<DeclaredResponse>;

#[derive(Debug)]
pub struct OperationDescriptor {
    pub name: &'static str,
    decode: fn(&'static Self, &mut dyn ArgumentReader) -> Result<DeclaredRequest, DecodeError>,
    invoke: InvokeAdapter,
    pub error_codes: fn() -> Vec<&'static str>,
    context_type: fn() -> std::any::TypeId,
    #[cfg(test)]
    handler_type: fn() -> std::any::TypeId,
    argument_type: fn() -> std::any::TypeId,
}

impl OperationDescriptor {
    pub fn request<A: HostArguments>(&'static self, arguments: A) -> DeclaredRequest {
        assert_eq!(
            (self.argument_type)(),
            std::any::TypeId::of::<A>(),
            "paired native arguments"
        );
        DeclaredRequest {
            descriptor: self,
            arguments: Arc::new(arguments),
        }
    }

    pub const fn new<H: Handler>() -> Self {
        Self {
            name: H::NAME,
            decode: |descriptor, reader| {
                Ok(DeclaredRequest {
                    descriptor,
                    arguments: Arc::new(H::Arguments::decode(reader)?),
                })
            },
            invoke: |context, arguments| {
                let context = *context
                    .downcast::<H::Context>()
                    .expect("validated handler context");
                let arguments = arguments
                    .as_any()
                    .downcast_ref::<H::Arguments>()
                    .expect("paired argument decoder")
                    .clone();
                Box::pin(
                    async move { Responder::<H>::new().respond(H::call(context, arguments).await) },
                )
            },
            error_codes: H::Error::codes,
            context_type: std::any::TypeId::of::<H::Context>,
            #[cfg(test)]
            handler_type: std::any::TypeId::of::<H>,
            argument_type: std::any::TypeId::of::<H::Arguments>,
        }
    }

    pub fn decode(
        &'static self,
        reader: &mut dyn ArgumentReader,
    ) -> Result<DeclaredRequest, DecodeError> {
        (self.decode)(self, reader)
    }
}

inventory::collect!(OperationDescriptor);

pub struct OperationRegistry(BTreeMap<&'static str, &'static OperationDescriptor>);

impl OperationRegistry {
    pub fn discover() -> Result<Self, String> {
        Self::assemble(inventory::iter::<OperationDescriptor>)
    }

    fn assemble(
        descriptors: impl IntoIterator<Item = &'static OperationDescriptor>,
    ) -> Result<Self, String> {
        let mut descriptors = descriptors.into_iter().collect::<Vec<_>>();
        descriptors.sort_by_key(|descriptor| descriptor.name);
        let mut registry = BTreeMap::new();
        for descriptor in descriptors {
            if registry.insert(descriptor.name, descriptor).is_some() {
                return Err(format!("duplicate host operation: {}", descriptor.name));
            }
        }
        Ok(Self(registry))
    }

    pub fn validate_context<C: 'static>(&self) -> Result<(), String> {
        for descriptor in self.0.values() {
            if (descriptor.context_type)() != std::any::TypeId::of::<C>() {
                return Err(format!(
                    "invalid context for host operation: {}",
                    descriptor.name
                ));
            }
        }
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&'static OperationDescriptor> {
        self.0.get(name).copied()
    }

    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.0.keys().copied()
    }

    pub fn error_codes(&self) -> Vec<&'static str> {
        let mut codes = self
            .0
            .values()
            .flat_map(|descriptor| (descriptor.error_codes)())
            .collect::<Vec<_>>();
        codes.sort_unstable();
        codes.dedup();
        codes
    }
}

pub fn registry() -> &'static OperationRegistry {
    static REGISTRY: std::sync::OnceLock<OperationRegistry> = std::sync::OnceLock::new();
    REGISTRY
        .get_or_init(|| OperationRegistry::discover().expect("valid host operation declarations"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    struct Arguments;
    impl HostArguments for Arguments {
        fn decode(_: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
            Ok(Self)
        }
    }

    #[derive(HostError)]
    enum SharedFailure {
        #[error(code = "StablePublicCode", message = "diagnostic")]
        RustVariant { revision: u64 },
        #[error(code = "DiagnosticDetail", message = "invalid value: {message}")]
        Diagnostic { message: String },
    }

    #[derive(HostError)]
    enum LocalFailure {
        #[error(transparent)]
        Shared(SharedFailure),
    }

    struct First;
    impl AsyncHandler for First {
        type Context = ();
        type Arguments = Arguments;
        type Success = ();
        type Error = LocalFailure;
        const NAME: &'static str = "first";
        fn call(_: (), _: Arguments) -> HandlerFuture<Result<(), LocalFailure>> {
            Box::pin(async { Ok(()) })
        }
    }
    struct Last;
    impl AsyncHandler for Last {
        type Context = ();
        type Arguments = Arguments;
        type Success = ();
        type Error = LocalFailure;
        const NAME: &'static str = "last";
        fn call(_: (), _: Arguments) -> HandlerFuture<Result<(), LocalFailure>> {
            Box::pin(async { Ok(()) })
        }
    }
    static FIRST: OperationDescriptor = OperationDescriptor::new::<First>();
    static LAST: OperationDescriptor = OperationDescriptor::new::<Last>();

    #[test]
    fn registry_is_sorted_and_duplicate_diagnostics_are_order_independent() {
        for descriptors in [[&LAST, &FIRST], [&FIRST, &LAST]] {
            let registry = OperationRegistry::assemble(descriptors).unwrap();
            assert_eq!(
                registry.0.keys().copied().collect::<Vec<_>>(),
                ["first", "last"]
            );
            assert_eq!(
                registry.error_codes(),
                ["DiagnosticDetail", "StablePublicCode"]
            );
            assert!(registry.validate_context::<()>().is_ok());
            assert_eq!(
                registry.validate_context::<String>().unwrap_err(),
                "invalid context for host operation: first"
            );
        }
        for descriptors in [[&LAST, &FIRST, &FIRST], [&FIRST, &FIRST, &LAST]] {
            assert_eq!(
                OperationRegistry::assemble(descriptors).err().unwrap(),
                "duplicate host operation: first"
            );
        }
    }

    #[test]
    fn shared_errors_preserve_explicit_codes_and_typed_details() {
        let error = LocalFailure::Shared(SharedFailure::RustVariant { revision: 7 }).into_failure();
        assert_eq!(error.code, "StablePublicCode");
        assert_eq!(error.message, "diagnostic");
        assert_eq!(error.details, serde_json::json!({ "revision": 7 }));
        assert_eq!(
            LocalFailure::codes(),
            ["DiagnosticDetail", "StablePublicCode"]
        );
        let diagnostic = SharedFailure::Diagnostic {
            message: "bad input".into(),
        }
        .into_failure();
        assert_eq!(diagnostic.message, "invalid value: bad input");
        assert_eq!(
            diagnostic.details,
            serde_json::json!({ "message": "bad input" })
        );
    }

    #[test]
    fn result_precision_is_checked_recursively() {
        assert!(encode_json(serde_json::json!({ "nested": [9007199254740991_u64] })).is_ok());
        assert_eq!(
            encode_json(serde_json::json!({ "nested": [9007199254740992_u64] })),
            Err(DecodeError::Host(TransportError::UnsupportedOperation))
        );
    }

    #[test]
    fn typed_adapter_can_suspend_without_holding_foreground_access() {
        use std::{
            cell::Cell,
            rc::Rc,
            task::{Context, Poll, Waker},
        };
        struct Awaiting;
        impl AsyncHandler for Awaiting {
            type Context = (tokio::sync::oneshot::Receiver<()>, Rc<Cell<bool>>);
            type Arguments = Arguments;
            type Success = ();
            type Error = LocalFailure;
            const NAME: &'static str = "awaiting";
            fn call(
                (ready, committed): Self::Context,
                _: Arguments,
            ) -> HandlerFuture<Result<(), LocalFailure>> {
                Box::pin(async move {
                    ready.await.unwrap();
                    committed.set(true);
                    Ok(())
                })
            }
        }
        static DESCRIPTOR: OperationDescriptor = OperationDescriptor::new::<Awaiting>();
        let (send, receive) = tokio::sync::oneshot::channel();
        let committed = Rc::new(Cell::new(false));
        let request = DeclaredRequest {
            descriptor: &DESCRIPTOR,
            arguments: Arc::new(Arguments),
        };
        let mut future = request.run((receive, committed.clone()));
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
        assert!(!committed.get());
        send.send(()).unwrap();
        let Poll::Ready(response) = future.as_mut().poll(&mut context) else {
            panic!("handler did not resume")
        };
        assert_eq!(response.result, Ok(Ok(EncodedValue::Undefined)));
        assert!(committed.get());
    }

    #[test]
    fn linked_product_handlers_are_discovered_in_test_binaries() {
        let registry = registry();
        assert!(registry.get("openDocument").is_some());
        assert!(registry.get("writeClipboardText").is_some());
        assert!(registry.error_codes().contains(&"Cancelled"));
        assert!(registry.error_codes().contains(&"ResourceNotFound"));
    }
}

#[cfg(test)]
mod declaration_contract {
    use super::*;

    #[test]
    fn declaration_codes_match_public_typescript() {
        let mut codes = registry().error_codes();
        codes.extend(TransportError::codes());
        codes.sort_unstable();
        codes.dedup();
        let declaration = include_str!("js/knot.d.ts")
            .split("export type HostErrorCode =")
            .nth(1)
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        let mut declared = declaration
            .split('"')
            .enumerate()
            .filter_map(|(index, value)| (index % 2 == 1).then_some(value))
            .collect::<Vec<_>>();
        declared.sort_unstable();
        assert_eq!(
            codes, declared,
            "update HostErrorCode from handler and transport descriptors"
        );
    }
}
