//! Declared application capabilities and their foreground access context.

use super::*;
use crate::host::declarations::{
    ArgumentReader, DecodeError, EncodedValue, HostArguments, HostError, HostSuccess, encode_json,
    host_operation,
};
use gpui::WeakEntity;

#[derive(Clone)]
pub(super) struct HostContext {
    host: WeakEntity<ProductExtensionHost>,
    application: gpui::AsyncApp,
    request: HostRequest,
}

impl HostContext {
    pub(super) fn new(request: HostRequest, cx: &mut Context<ProductExtensionHost>) -> Self {
        Self {
            host: cx.entity().downgrade(),
            application: cx.to_async(),
            request,
        }
    }

    fn foreground<R: 'static>(
        &self,
        access: impl FnOnce(&mut ProductExtensionHost, &mut Context<ProductExtensionHost>) -> R,
    ) -> Result<R, AccessError> {
        self.host
            .update(&mut self.application.clone(), |host, cx| {
                if !host.operation_is_live(
                    ExtensionKey::new(self.request.extension, self.request.lifecycle),
                    self.request.invocation,
                ) {
                    return Err(AccessError::Cancelled);
                }
                Ok(access(host, cx))
            })
            .map_err(|_| AccessError::Cancelled)?
    }
}

#[derive(Debug, HostError)]
pub(super) enum AccessError {
    #[error(code = "Cancelled", message = "Cancelled")]
    Cancelled,
    #[error(code = "InvalidTarget", message = "InvalidTarget")]
    InvalidTarget,
    #[error(code = "ContextUnavailable", message = "ContextUnavailable")]
    ContextUnavailable,
}

#[derive(Debug, HostError)]
pub(super) enum OpenError {
    #[error(transparent)]
    Access(AccessError),
    #[error(code = "InvalidUri", message = "InvalidUri")]
    InvalidUri,
    #[error(code = "InvalidUtf8", message = "InvalidUtf8")]
    InvalidUtf8,
    #[error(code = "ResourceNotFound", message = "ResourceNotFound")]
    ResourceNotFound,
    #[error(code = "ResourceWrongKind", message = "ResourceWrongKind")]
    ResourceWrongKind,
    #[error(code = "ProviderFailure", message = "ProviderFailure")]
    ProviderFailure,
    #[error(code = "BufferClosed", message = "BufferClosed")]
    BufferClosed,
}

impl From<AccessError> for OpenError {
    fn from(error: AccessError) -> Self {
        Self::Access(error)
    }
}

fn open_failure(error: crate::host::protocol::HostRequestError) -> OpenError {
    use crate::host::protocol::HostRequestError as Error;
    match error {
        Error::Cancelled => AccessError::Cancelled.into(),
        Error::ContextUnavailable => AccessError::ContextUnavailable.into(),
        Error::BufferClosed => OpenError::BufferClosed,
        Error::InvalidTarget => AccessError::InvalidTarget.into(),
        Error::InvalidUri => OpenError::InvalidUri,
        Error::InvalidUtf8 => OpenError::InvalidUtf8,
        Error::ResourceNotFound => OpenError::ResourceNotFound,
        Error::ResourceWrongKind => OpenError::ResourceWrongKind,
        Error::ProviderFailure => OpenError::ProviderFailure,
        _ => unreachable!("document Open returned an unrelated failure: {error:?}"),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct ClipboardArguments {
    text: String,
}

impl HostArguments for ClipboardArguments {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            text: reader.text(2, "clipboard text must be text")?,
        })
    }
}

#[host_operation("writeClipboardText")]
async fn write_clipboard(
    context: HostContext,
    arguments: ClipboardArguments,
) -> Result<(), AccessError> {
    context
        .foreground(|_, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(arguments.text)))
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct OpenArguments {
    uri: String,
    target: Option<crate::host::protocol::DocumentDestination>,
}

impl HostArguments for OpenArguments {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            uri: reader.text(2, "resource URI must be text")?,
            target: reader.destination(3)?,
        })
    }
}

impl HostSuccess for crate::host::protocol::DocumentPresentation {
    fn encode(self) -> Result<EncodedValue, DecodeError> {
        encode_json(self)
    }
}

#[host_operation("openDocument")]
async fn open_document(
    context: HostContext,
    arguments: OpenArguments,
) -> Result<crate::host::protocol::DocumentPresentation, OpenError> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let host = context.host.clone();
    let request = context.request.clone();
    context
        .foreground(move |state, cx| {
            let destination = state.document_destination(&request, arguments.target, cx)?;
            product::operations::open_document(
                arguments.uri,
                destination,
                product::operations::OperationCaller {
                    host,
                    key: ExtensionKey::new(request.extension, request.lifecycle),
                    invocation: request.invocation,
                },
                move |result, _| {
                    let _ = sender.send(result);
                },
                cx,
            )
        })?
        .map_err(open_failure)?;
    let (document, target) = receiver
        .await
        .map_err(|_| AccessError::Cancelled)?
        .map_err(open_failure)?;
    context
        .foreground(move |host, cx| {
            product::operations::validate_presentation(&target, cx)?;
            host.document_presentation(document, target, cx)
        })?
        .map_err(open_failure)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::protocol::{HostOperation, RequestId};
    use gpui::TestAppContext;

    #[gpui::test]
    async fn immediate_handler_commits_and_rejects_a_disposed_context(cx: &mut TestAppContext) {
        let key = ExtensionKey::new(ExtensionId::new(702), ExtensionLifecycleId::new(1));
        let (host, context) = cx.update(|cx| {
            let commands =
                cx.new(super::super::super::product_commands::ProductCommandDispatcher::new);
            cx.set_global(ApplicationProductCommands(commands));
            let host = install(cx);
            let context = host.update(cx, |host, cx| {
                host.load(key, ExtensionConfig::default()).unwrap();
                HostContext::new(
                    HostRequest {
                        extension: key.extension,
                        lifecycle: key.lifecycle,
                        id: RequestId::new(1),
                        invocation: None,
                        operation: HostOperation::ActiveBuffer { target: None },
                    },
                    cx,
                )
            });
            (host, context)
        });
        write_clipboard(
            context.clone(),
            ClipboardArguments {
                text: "application".into(),
            },
        )
        .await
        .unwrap();
        cx.update(|cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                "application"
            );
            host.update(cx, |host, cx| host.unload(key, cx).unwrap());
        });
        let result = write_clipboard(
            context,
            ClipboardArguments {
                text: "late".into(),
            },
        )
        .await;
        assert!(matches!(result, Err(AccessError::Cancelled)));
        cx.update(|cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                "application"
            )
        });
    }
}
