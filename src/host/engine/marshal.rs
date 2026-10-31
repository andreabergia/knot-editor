//! Conversion between V8 values and Knot's typed host protocol.

use super::*;

#[derive(Clone, Copy, Debug)]
pub(super) enum BoundaryError {
    Type(&'static str),
    Range(&'static str),
    Host(TransportError),
}

impl From<&'static str> for BoundaryError {
    fn from(message: &'static str) -> Self {
        Self::Type(message)
    }
}

impl fmt::Display for BoundaryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Type(message) | Self::Range(message) => formatter.write_str(message),
            Self::Host(error) => formatter.write_str(error.code()),
        }
    }
}

pub(super) fn boundary_exception<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    error: BoundaryError,
) -> v8::Local<'s, v8::Value> {
    let message = v8::String::new(scope, &error.to_string()).expect("V8 error message");
    match error {
        BoundaryError::Type(_) => v8::Exception::type_error(scope, message),
        BoundaryError::Range(_) => v8::Exception::range_error(scope, message),
        BoundaryError::Host(code) => {
            let exception = v8::Exception::error(scope, message);
            let object = v8::Local::<v8::Object>::try_from(exception).expect("V8 Error object");
            let name = v8::String::new(scope, "KnotHostError").unwrap();
            let code = v8::String::new(scope, code.code()).unwrap();
            set_property(scope, object, "name", name.into()).expect("V8 error name");
            set_property(scope, object, "code", code.into()).expect("V8 error code");
            exception
        }
    }
}

pub(super) fn host_request_callback<'s, 'i>(
    scope: &mut v8::PinnedRef<'s, v8::HandleScope<'i>>,
    arguments: v8::FunctionCallbackArguments<'s>,
    mut result: v8::ReturnValue,
) {
    #[cfg(test)]
    let configured_error = scope
        .get_slot::<Arc<RuntimeLocalState>>()
        .and_then(|state| state.host_error.lock().ok()?.take());
    #[cfg(not(test))]
    let configured_error: Option<HostFailure> = None;
    let resolver = v8::PromiseResolver::new(scope).unwrap();
    if let Some(error) = configured_error {
        let exception = host_failure_exception(scope, &error);
        resolver.reject(scope, exception);
        result.set(resolver.get_promise(scope).into());
        return;
    }
    let Some(state) = scope.get_slot::<Arc<RuntimeLocalState>>().cloned() else {
        let exception = boundary_exception(
            scope,
            BoundaryError::Host(TransportError::UnsupportedOperation),
        );
        resolver.reject(scope, exception);
        result.set(resolver.get_promise(scope).into());
        return;
    };
    if state.status.load(Ordering::Acquire) != ACTIVE {
        let exception = boundary_exception(scope, BoundaryError::Host(TransportError::Cancelled));
        resolver.reject(scope, exception);
        result.set(resolver.get_promise(scope).into());
        return;
    }
    let invocation = match optional_u64(scope, arguments.get(0), "UnsupportedOperation") {
        Ok(value) => value.map(CommandInvocationId::new),
        Err(error) => {
            let exception = boundary_exception(scope, error);
            resolver.reject(scope, exception);
            result.set(resolver.get_promise(scope).into());
            return;
        }
    };
    let parsed = parse_host_operation(scope, &arguments);
    let sender = state.event_sender.lock().unwrap().clone();
    let root = *state.current_root.lock().unwrap();
    let (operation, kind) = match parsed {
        Ok(parsed) => parsed,
        Err(error) => {
            let exception = boundary_exception(scope, error);
            resolver.reject(scope, exception);
            result.set(resolver.get_promise(scope).into());
            return;
        }
    };
    let (Some(sender), Some(root)) = (sender, root) else {
        let exception = boundary_exception(
            scope,
            BoundaryError::Host(TransportError::UnsupportedOperation),
        );
        resolver.reject(scope, exception);
        result.set(resolver.get_promise(scope).into());
        return;
    };
    let Ok(request_value) =
        state
            .next_request
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                next.checked_add(1)
            })
    else {
        let exception = boundary_exception(
            scope,
            BoundaryError::Host(TransportError::UnsupportedOperation),
        );
        resolver.reject(scope, exception);
        result.set(resolver.get_promise(scope).into());
        return;
    };
    let id = RequestId::new(request_value);
    state.pending.lock().unwrap().insert(
        id,
        PendingRequest {
            root,
            invocation,
            operation: kind,
            resolver: Some(v8::Global::new(scope, resolver)),
            response_queued: false,
            provider_mutation: operation.provider_mutation(),
        },
    );
    if sender
        .send(EngineEvent::Request(HostRequest {
            extension: state.key.extension,
            lifecycle: state.key.lifecycle,
            id,
            invocation,
            operation,
        }))
        .is_err()
    {
        state.pending.lock().unwrap().remove(&id);
        let exception = boundary_exception(scope, BoundaryError::Host(TransportError::Cancelled));
        resolver.reject(scope, exception);
    }
    result.set(resolver.get_promise(scope).into());
}

pub(super) fn cancellation_listener_failure_callback<'s, 'i>(
    scope: &mut v8::PinnedRef<'s, v8::HandleScope<'i>>,
    arguments: v8::FunctionCallbackArguments<'s>,
    _result: v8::ReturnValue,
) {
    if arguments.get(0).is_string() {
        eprintln!(
            "[knot] cancellation listener failed: {}",
            arguments.get(0).to_rust_string_lossy(scope)
        );
    }
}

pub(super) fn keybinding_callback<'s, 'i>(
    scope: &mut v8::PinnedRef<'s, v8::HandleScope<'i>>,
    arguments: v8::FunctionCallbackArguments<'s>,
    _result: v8::ReturnValue,
) {
    let Some(state) = scope.get_slot::<Arc<RuntimeLocalState>>().cloned() else {
        return;
    };
    let mutation = parse_keybinding_mutation(scope, &arguments)
        .unwrap_or_else(|message| KeybindingMutation::Invalid(message.to_string()));
    state.keybinding_mutations.lock().unwrap().push(mutation);
}

pub(super) fn parse_keybinding_mutation<'s, 'i>(
    scope: &mut v8::PinnedRef<'s, v8::HandleScope<'i>>,
    arguments: &v8::FunctionCallbackArguments<'s>,
) -> Result<KeybindingMutation, BoundaryError> {
    let operation = required_string(scope, arguments.get(0), "invalid keybinding operation")?;
    let key = required_string(scope, arguments.get(1), "key must be a string")?;
    if operation == "invalid" {
        return Ok(KeybindingMutation::Invalid(key));
    }
    let options = arguments.get(3);
    let view = if options.is_null_or_undefined() {
        None
    } else {
        if options.is_array() || options.is_function() {
            return Err("options must be an object".into());
        }
        let object =
            v8::Local::<v8::Object>::try_from(options).map_err(|_| "options must be an object")?;
        let view = property(scope, object, "view", "invalid view selector")?;
        optional_string(scope, view, "view must be a string")?
    };
    match operation.as_str() {
        "remove" => Ok(KeybindingMutation::Remove { key, view }),
        "set" => {
            let value = arguments.get(2);
            let command = if value.is_null() {
                None
            } else if value.is_string() {
                Some(Command {
                    name: required_string(scope, value, "command must be a string or object")?
                        .into(),
                    arguments: CommandArgumentValue::Null,
                })
            } else {
                if value.is_array() || value.is_function() {
                    return Err("command must be a string or object".into());
                }
                let object = v8::Local::<v8::Object>::try_from(value)
                    .map_err(|_| "command must be a string or object")?;
                let name = property(scope, object, "name", "invalid command name")?;
                let name = required_string(scope, name, "command name must be a string")?;
                let arguments_value =
                    property(scope, object, "arguments", "invalid command arguments")?;
                let command_arguments = if arguments_value.is_undefined() {
                    CommandArgumentValue::Null
                } else {
                    command_argument_from_v8(scope, arguments_value)
                        .map_err(|_| "command arguments must be JSON-compatible")?
                };
                Some(Command {
                    name: name.into(),
                    arguments: command_arguments,
                })
            };
            Ok(KeybindingMutation::Set { key, view, command })
        }
        _ => Err("invalid keybinding operation".into()),
    }
}

pub(super) fn document_destination_from_v8<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    value: v8::Local<'s, v8::Value>,
) -> Result<Option<crate::host::protocol::DocumentDestination>, BoundaryError> {
    use crate::host::protocol::{
        DocumentDestination, PaneTarget, PresentationTarget, WindowTarget,
    };
    if value.is_null_or_undefined() {
        return Ok(None);
    }
    let object = v8::Local::<v8::Object>::try_from(value)
        .map_err(|_| BoundaryError::Type("invalid destination"))?;
    let window = WindowTarget {
        window: string_property(scope, object, "window")?,
    };
    let kind = string_property(scope, object, "kind")?;
    if kind == "window" {
        return Ok(Some(DocumentDestination::Window(window)));
    }
    let pane = PaneTarget {
        window,
        workbench: string_property(scope, object, "workbench")?,
        pane: string_property(scope, object, "pane")?,
    };
    match kind.as_str() {
        "pane" => Ok(Some(DocumentDestination::Pane(pane))),
        "view" => Ok(Some(DocumentDestination::View(PresentationTarget {
            pane,
            tab: string_property(scope, object, "tab")?,
            presentation: string_property(scope, object, "presentation")?,
        }))),
        _ => Err(BoundaryError::Type("invalid destination kind")),
    }
}

pub(super) fn workspace_root_uris<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    value: v8::Local<'s, v8::Value>,
) -> Result<Vec<String>, BoundaryError> {
    let array = v8::Local::<v8::Array>::try_from(value)
        .map_err(|_| BoundaryError::Type("workspace roots must be an array"))?;
    let mut roots = Vec::with_capacity(array.length() as usize);
    for index in 0..array.length() {
        let uri = array
            .get_index(scope, index)
            .ok_or("invalid workspace root")?;
        roots.push(required_string(
            scope,
            uri,
            "workspace root URI must be text",
        )?);
    }
    Ok(roots)
}

pub(super) struct DeclarationReader<'a, 's, 'i> {
    scope: &'a mut v8::PinnedRef<'s, v8::HandleScope<'i>>,
    arguments: &'a v8::FunctionCallbackArguments<'s>,
}

impl crate::host::declarations::ArgumentReader for DeclarationReader<'_, '_, '_> {
    fn optional_integer(
        &mut self,
        index: usize,
    ) -> Result<Option<u64>, crate::host::declarations::DecodeError> {
        optional_u64(
            self.scope,
            self.arguments.get(index as i32),
            "InvalidTarget",
        )
        .map_err(declaration_decode_error)
    }
    fn command_arguments(
        &mut self,
        index: usize,
    ) -> Result<CommandArgumentValue, crate::host::declarations::DecodeError> {
        command_argument_from_v8(self.scope, self.arguments.get(index as i32))
            .map_err(declaration_decode_error)
    }
    fn outcome(
        &mut self,
        index: usize,
    ) -> Result<CommandOutcome, crate::host::declarations::DecodeError> {
        command_outcome_from_v8(self.scope, self.arguments.get(index as i32))
            .map_err(|_| crate::host::declarations::DecodeError::Type("UnsupportedOperation"))
    }

    fn range(
        &mut self,
        index: usize,
    ) -> Result<Option<ByteRange>, crate::host::declarations::DecodeError> {
        optional_byte_range(self.scope, self.arguments.get(index as i32))
            .map_err(declaration_decode_error)
    }
    fn edits(
        &mut self,
        index: usize,
    ) -> Result<Vec<TextEdit>, crate::host::declarations::DecodeError> {
        text_edits(self.scope, self.arguments.get(index as i32)).map_err(declaration_decode_error)
    }
    fn contributions(
        &mut self,
        index: usize,
    ) -> Result<Vec<EditorContribution>, crate::host::declarations::DecodeError> {
        editor_contributions(self.scope, self.arguments.get(index as i32))
            .map_err(declaration_decode_error)
    }

    fn optional_text(
        &mut self,
        index: usize,
        message: &'static str,
    ) -> Result<Option<String>, crate::host::declarations::DecodeError> {
        optional_string(self.scope, self.arguments.get(index as i32), message)
            .map_err(declaration_decode_error)
    }
    fn optional_boolean(
        &mut self,
        index: usize,
        message: &'static str,
    ) -> Result<Option<bool>, crate::host::declarations::DecodeError> {
        let value = self.arguments.get(index as i32);
        if value.is_null_or_undefined() {
            Ok(None)
        } else if value.is_boolean() {
            Ok(Some(value.is_true()))
        } else {
            Err(crate::host::declarations::DecodeError::Type(message))
        }
    }
    fn text_list(
        &mut self,
        index: usize,
    ) -> Result<Vec<String>, crate::host::declarations::DecodeError> {
        workspace_root_uris(self.scope, self.arguments.get(index as i32))
            .map_err(declaration_decode_error)
    }

    fn integer(&mut self, index: usize) -> Result<u64, crate::host::declarations::DecodeError> {
        argument_u64(self.scope, self.arguments, index as i32).map_err(declaration_decode_error)
    }

    fn text(
        &mut self,
        index: usize,
        message: &'static str,
    ) -> Result<String, crate::host::declarations::DecodeError> {
        required_string(self.scope, self.arguments.get(index as i32), message)
            .map_err(declaration_decode_error)
    }
    fn destination(
        &mut self,
        index: usize,
    ) -> Result<
        Option<crate::host::protocol::DocumentDestination>,
        crate::host::declarations::DecodeError,
    > {
        document_destination_from_v8(self.scope, self.arguments.get(index as i32))
            .map_err(declaration_decode_error)
    }
}

pub(super) fn declaration_decode_error(
    error: BoundaryError,
) -> crate::host::declarations::DecodeError {
    match error {
        BoundaryError::Type(message) => crate::host::declarations::DecodeError::Type(message),
        BoundaryError::Range(message) => crate::host::declarations::DecodeError::Range(message),
        BoundaryError::Host(_) => unreachable!("argument decoding returns author errors"),
    }
}

impl From<crate::host::declarations::DecodeError> for BoundaryError {
    fn from(error: crate::host::declarations::DecodeError) -> Self {
        match error {
            crate::host::declarations::DecodeError::Type(message) => Self::Type(message),
            crate::host::declarations::DecodeError::Range(message) => Self::Range(message),
            crate::host::declarations::DecodeError::Host(error) => Self::Host(error),
        }
    }
}

pub(super) fn parse_host_operation<'s, 'i>(
    scope: &mut v8::PinnedRef<'s, v8::HandleScope<'i>>,
    arguments: &v8::FunctionCallbackArguments<'s>,
) -> Result<(crate::host::declarations::DeclaredRequest, &'static str), BoundaryError> {
    let operation = required_string(scope, arguments.get(1), "operation must be text")?;
    if let Some(descriptor) = crate::host::declarations::registry().get(&operation) {
        let request = descriptor.decode(&mut DeclarationReader { scope, arguments })?;
        return Ok((request, descriptor.name));
    }
    Err(BoundaryError::Host(TransportError::UnsupportedOperation))
}

pub(super) fn optional_u64(
    scope: &mut v8::PinnedRef<'_, v8::HandleScope>,
    value: v8::Local<'_, v8::Value>,
    error: &'static str,
) -> Result<Option<u64>, BoundaryError> {
    if value.is_null_or_undefined() {
        Ok(None)
    } else {
        value_u64(scope, value, error).map(Some)
    }
}

pub(super) fn argument_u64(
    scope: &mut v8::PinnedRef<'_, v8::HandleScope>,
    arguments: &v8::FunctionCallbackArguments,
    index: i32,
) -> Result<u64, BoundaryError> {
    value_u64(scope, arguments.get(index), "UnsupportedOperation")
}

pub(super) fn value_u64(
    scope: &mut v8::PinnedRef<'_, v8::HandleScope>,
    value: v8::Local<'_, v8::Value>,
    _error: &'static str,
) -> Result<u64, BoundaryError> {
    if !value.is_number() {
        return Err(BoundaryError::Type("expected a number"));
    }
    let number = value
        .number_value(scope)
        .ok_or(BoundaryError::Type("expected a number"))?;
    if !number.is_finite()
        || number < 0.0
        || number.fract() != 0.0
        || number > 9_007_199_254_740_991.0
    {
        return Err(BoundaryError::Range("expected a nonnegative safe integer"));
    }
    Ok(number as u64)
}

pub(super) fn value_usize(
    scope: &mut v8::PinnedRef<'_, v8::HandleScope>,
    value: v8::Local<'_, v8::Value>,
    error: &'static str,
) -> Result<usize, BoundaryError> {
    usize::try_from(value_u64(scope, value, error)?)
        .map_err(|_| BoundaryError::Range("integer exceeds native coordinates"))
}

pub(super) fn property<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    object: v8::Local<'s, v8::Object>,
    name: &str,
    error: &'static str,
) -> Result<v8::Local<'s, v8::Value>, BoundaryError> {
    let key = v8::String::new(scope, name).ok_or(error)?;
    object.get(scope, key.into()).ok_or(error.into())
}

pub(super) fn required_string(
    scope: &mut v8::PinnedRef<'_, v8::HandleScope>,
    value: v8::Local<'_, v8::Value>,
    error: &'static str,
) -> Result<String, BoundaryError> {
    if !value.is_string() {
        return Err(error.into());
    }
    Ok(value.to_rust_string_lossy(scope))
}

pub(super) fn optional_string(
    scope: &mut v8::PinnedRef<'_, v8::HandleScope>,
    value: v8::Local<'_, v8::Value>,
    error: &'static str,
) -> Result<Option<String>, BoundaryError> {
    if value.is_null_or_undefined() {
        Ok(None)
    } else {
        required_string(scope, value, error).map(Some)
    }
}

pub(super) fn optional_byte_range<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    value: v8::Local<'s, v8::Value>,
) -> Result<Option<ByteRange>, BoundaryError> {
    if value.is_null_or_undefined() {
        return Ok(None);
    }
    byte_range(scope, value).map(Some)
}

pub(super) fn byte_range<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    value: v8::Local<'s, v8::Value>,
) -> Result<ByteRange, BoundaryError> {
    let object = v8::Local::<v8::Object>::try_from(value).map_err(|_| "InvalidRange")?;
    let start = property(scope, object, "startByteOffset", "InvalidRange")?;
    let start_byte_offset = value_usize(scope, start, "InvalidRange")?;
    let end = property(scope, object, "endByteOffset", "InvalidRange")?;
    let end_byte_offset = value_usize(scope, end, "InvalidRange")?;
    if start_byte_offset > end_byte_offset {
        return Err(BoundaryError::Range("range endpoints must be ordered"));
    }
    Ok(ByteRange {
        start_byte_offset,
        end_byte_offset,
    })
}

pub(super) fn text_edits(
    scope: &mut v8::PinnedRef<'_, v8::HandleScope>,
    value: v8::Local<'_, v8::Value>,
) -> Result<Vec<TextEdit>, BoundaryError> {
    let array = v8::Local::<v8::Array>::try_from(value).map_err(|_| "InvalidEditBatch")?;
    let mut edits = Vec::with_capacity(array.length() as usize);
    for index in 0..array.length() {
        let value = array.get_index(scope, index).ok_or("InvalidEditBatch")?;
        let object = v8::Local::<v8::Object>::try_from(value).map_err(|_| "InvalidEditBatch")?;
        let range = property(scope, object, "range", "InvalidEditBatch")?;
        let range = byte_range(scope, range)?;
        let text = property(scope, object, "text", "InvalidEditBatch")?;
        edits.push(TextEdit {
            range,
            text: required_string(scope, text, "InvalidEditBatch")?,
        });
    }
    Ok(edits)
}

pub(super) fn editor_contributions(
    scope: &mut v8::PinnedRef<'_, v8::HandleScope>,
    value: v8::Local<'_, v8::Value>,
) -> Result<Vec<EditorContribution>, BoundaryError> {
    let error = "UnsupportedOperation";
    let array = v8::Local::<v8::Array>::try_from(value).map_err(|_| error)?;
    let mut contributions = Vec::with_capacity(array.length() as usize);
    for index in 0..array.length() {
        let value = array.get_index(scope, index).ok_or(error)?;
        let object = v8::Local::<v8::Object>::try_from(value).map_err(|_| error)?;
        let range_value = property(scope, object, "range", error)?;
        let range = byte_range(scope, range_value)?;
        let decoration_value = property(scope, object, "decoration", error)?;
        let decoration = optional_string(scope, decoration_value, error)?;
        let gutter_value = property(scope, object, "gutter", error)?;
        let gutter = optional_string(scope, gutter_value, error)?;
        let command_value = property(scope, object, "command", error)?;
        let command = optional_string(scope, command_value, error)?;
        contributions.push(EditorContribution {
            range,
            decoration,
            gutter,
            command,
        });
    }
    Ok(contributions)
}

pub(super) enum JsonFrame<'s> {
    Array {
        object: v8::Local<'s, v8::Object>,
        values: Vec<v8::Local<'s, v8::Value>>,
        next: usize,
        result: Vec<CommandArgumentValue>,
    },
    Object {
        object: v8::Local<'s, v8::Object>,
        keys: Vec<String>,
        values: Vec<v8::Local<'s, v8::Value>>,
        next: usize,
        result: std::collections::BTreeMap<String, CommandArgumentValue>,
    },
}

impl JsonFrame<'_> {
    fn object(&self) -> v8::Local<'_, v8::Object> {
        match self {
            Self::Array { object, .. } | Self::Object { object, .. } => *object,
        }
    }
}

pub(super) fn command_argument_from_v8<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    value: v8::Local<'s, v8::Value>,
) -> Result<CommandArgumentValue, BoundaryError> {
    let mut frames: Vec<JsonFrame<'s>> = Vec::new();
    let mut current = value;
    loop {
        let mut decoded = if current.is_null() {
            CommandArgumentValue::Null
        } else if current.is_boolean() {
            CommandArgumentValue::Boolean(current.boolean_value(scope))
        } else if current.is_string() {
            CommandArgumentValue::String(current.to_rust_string_lossy(scope))
        } else if current.is_number() {
            let number = current
                .number_value(scope)
                .ok_or("expected a JSON number")?;
            if !number.is_finite() {
                return Err(BoundaryError::Type("JSON numbers must be finite"));
            }
            CommandArgumentValue::Number(number)
        } else {
            if current.is_function() || !current.is_object() {
                return Err(BoundaryError::Type(
                    "command arguments must be JSON-compatible",
                ));
            }
            let object =
                v8::Local::<v8::Object>::try_from(current).map_err(|_| "expected a JSON object")?;
            if frames
                .iter()
                .any(|frame| frame.object().strict_equals(object.into()))
            {
                return Err(BoundaryError::Type(
                    "command arguments cannot contain cycles",
                ));
            }
            let array = v8::Local::<v8::Array>::try_from(current).ok();
            let prototype = object
                .get_prototype(scope)
                .ok_or("invalid JSON prototype")?;
            let expected = if array.is_some() {
                v8::Array::new(scope, 0).get_prototype(scope)
            } else {
                v8::Object::new(scope).get_prototype(scope)
            }
            .ok_or("invalid JSON prototype")?;
            if !prototype.is_null() && !prototype.strict_equals(expected) {
                return Err(BoundaryError::Type(
                    "command arguments require plain JSON objects",
                ));
            }
            let to_json = property(scope, object, "toJSON", "invalid JSON conversion")?;
            if to_json.is_function() {
                return Err(BoundaryError::Type("custom JSON conversion is unavailable"));
            }
            let all_keys = object
                .get_own_property_names(
                    scope,
                    v8::GetPropertyNamesArgs {
                        property_filter: v8::PropertyFilter::ALL_PROPERTIES,
                        ..Default::default()
                    },
                )
                .ok_or("invalid JSON keys")?;
            for index in 0..all_keys.length() {
                let key = all_keys.get_index(scope, index).ok_or("invalid JSON key")?;
                if key.is_symbol() {
                    return Err(BoundaryError::Type("JSON keys must be strings"));
                }
                if let Some(array) = array {
                    if key.is_number() {
                        if value_u64(scope, key, "JSON index")? >= array.length() as u64 {
                            return Err(BoundaryError::Type("invalid JSON array index"));
                        }
                    } else {
                        let key = required_string(scope, key, "JSON keys must be strings")?;
                        if key != "length" && !(key == "toJSON" && to_json.is_undefined()) {
                            return Err(BoundaryError::Type(
                                "JSON arrays must contain only indexed data",
                            ));
                        }
                    }
                }
            }
            if let Some(array) = array {
                let mut values = Vec::with_capacity(array.length() as usize);
                for index in 0..array.length() {
                    let key =
                        v8::String::new(scope, &index.to_string()).ok_or("invalid JSON index")?;
                    if object.has_own_property(scope, key.into()) != Some(true) {
                        return Err(BoundaryError::Type("command arrays must be dense"));
                    }
                    values.push(
                        array
                            .get_index(scope, index)
                            .ok_or("invalid JSON array item")?,
                    );
                }
                if values.is_empty() {
                    CommandArgumentValue::Array(Vec::new())
                } else {
                    current = values[0];
                    frames.push(JsonFrame::Array {
                        object,
                        result: Vec::with_capacity(values.len()),
                        values,
                        next: 0,
                    });
                    continue;
                }
            } else {
                let names = object
                    .get_own_property_names(
                        scope,
                        v8::GetPropertyNamesArgs {
                            key_conversion: v8::KeyConversionMode::ConvertToString,
                            ..Default::default()
                        },
                    )
                    .ok_or("invalid JSON keys")?;
                let mut keys = Vec::with_capacity(names.length() as usize);
                let mut values = Vec::with_capacity(names.length() as usize);
                for index in 0..names.length() {
                    let key = names.get_index(scope, index).ok_or("invalid JSON key")?;
                    keys.push(required_string(scope, key, "JSON keys must be strings")?);
                    values.push(object.get(scope, key).ok_or("invalid JSON value")?);
                }
                if values.is_empty() {
                    CommandArgumentValue::Object(Default::default())
                } else {
                    current = values[0];
                    frames.push(JsonFrame::Object {
                        object,
                        keys,
                        values,
                        next: 0,
                        result: Default::default(),
                    });
                    continue;
                }
            }
        };
        loop {
            let Some(frame) = frames.last_mut() else {
                return Ok(decoded);
            };
            let next_value = match frame {
                JsonFrame::Array {
                    result,
                    values,
                    next,
                    ..
                } => {
                    result.push(decoded);
                    *next += 1;
                    values.get(*next).copied()
                }
                JsonFrame::Object {
                    result,
                    keys,
                    values,
                    next,
                    ..
                } => {
                    result.insert(keys[*next].clone(), decoded);
                    *next += 1;
                    values.get(*next).copied()
                }
            };
            if let Some(value) = next_value {
                current = value;
                break;
            }
            decoded = match frames.pop().unwrap() {
                JsonFrame::Array { result, .. } => CommandArgumentValue::Array(result),
                JsonFrame::Object { result, .. } => CommandArgumentValue::Object(result),
            };
        }
    }
}

pub(super) fn command_argument_to_v8<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    value: &CommandArgumentValue,
) -> Result<v8::Local<'s, v8::Value>, &'static str> {
    if !command_argument_is_finite(value) {
        return Err("UnsupportedOperation");
    }
    let json = serde_json::to_string(value).map_err(|_| "UnsupportedOperation")?;
    let json = v8::String::new(scope, &json).ok_or("UnsupportedOperation")?;
    v8::json::parse(scope, json).ok_or("UnsupportedOperation")
}

pub(super) fn command_argument_is_finite(value: &CommandArgumentValue) -> bool {
    match value {
        CommandArgumentValue::Number(value) => value.is_finite(),
        CommandArgumentValue::Array(values) => values.iter().all(command_argument_is_finite),
        CommandArgumentValue::Object(values) => values.values().all(command_argument_is_finite),
        CommandArgumentValue::Null
        | CommandArgumentValue::Boolean(_)
        | CommandArgumentValue::String(_) => true,
    }
}

pub(super) fn command_outcome_from_v8<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    value: v8::Local<'s, v8::Value>,
) -> Result<CommandOutcome, RuntimeError> {
    let decoded = (|| -> Result<CommandOutcome, BoundaryError> {
        let object = v8::Local::<v8::Object>::try_from(value)
            .map_err(|_| "command outcome must be an object")?;
        let kind = string_property(scope, object, "kind")?;
        match kind.as_str() {
            "completed" => Ok(CommandOutcome::Completed),
            "unavailable" => Ok(CommandOutcome::Unavailable),
            "invalidTarget" => Ok(CommandOutcome::InvalidTarget),
            "cancelled" => Ok(CommandOutcome::Cancelled),
            "invalidArgument" => Ok(CommandOutcome::InvalidArgument {
                message: string_property(scope, object, "message")?,
            }),
            "handlerFailure" => Ok(CommandOutcome::HandlerFailure {
                message: string_property(scope, object, "message")?,
            }),
            _ => Err(BoundaryError::Type("invalid command outcome kind")),
        }
    })();
    decoded.map_err(|error| {
        RuntimeError::fatal(
            RuntimeErrorKind::Engine,
            format!("invalid command outcome from JavaScript: {error}"),
        )
    })
}

pub(super) fn string_property<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    object: v8::Local<'s, v8::Object>,
    name: &str,
) -> Result<String, BoundaryError> {
    let value = property(scope, object, name, "invalid text property")?;
    required_string(scope, value, "property must be text")
}

pub(super) fn optional_text_property<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    object: v8::Local<'s, v8::Object>,
    name: &str,
) -> Result<Option<String>, BoundaryError> {
    let value = property(scope, object, name, "invalid optional text property")?;
    optional_string(scope, value, "property must be text or null")
}

pub(super) fn provider_items_from_v8<'s, T>(
    scope: &mut v8::PinScope<'s, '_>,
    value: v8::Local<'s, v8::Value>,
    mut convert: impl FnMut(
        &mut v8::PinScope<'s, '_>,
        v8::Local<'s, v8::Value>,
    ) -> Result<T, BoundaryError>,
) -> Result<Vec<T>, String> {
    let object = v8::Local::<v8::Object>::try_from(value)
        .map_err(|_| "semantic provider result must be an object".to_owned())?;
    let error_key =
        v8::String::new(scope, "error").ok_or_else(|| "invalid provider result".to_owned())?;
    if object.has_own_property(scope, error_key.into()) == Some(true)
        && let Some(error) =
            optional_text_property(scope, object, "error").map_err(|error| error.to_string())?
    {
        return Err(error);
    }
    let values = property(scope, object, "items", "provider result is missing items")
        .map_err(|error| error.to_string())?;
    let values = v8::Local::<v8::Array>::try_from(values)
        .map_err(|_| "semantic provider items must be an array".to_owned())?;
    let mut items = Vec::with_capacity(values.length() as usize);
    for index in 0..values.length() {
        let value = values
            .get_index(scope, index)
            .ok_or_else(|| "invalid provider item".to_owned())?;
        items.push(convert(scope, value).map_err(|error| error.to_string())?);
    }
    Ok(items)
}

pub(super) fn tree_item_from_v8<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    value: v8::Local<'s, v8::Value>,
) -> Result<TreeItem, BoundaryError> {
    let object =
        v8::Local::<v8::Object>::try_from(value).map_err(|_| "tree item must be an object")?;
    let collapsible_state = match string_property(scope, object, "collapsibleState")?.as_str() {
        "none" => TreeCollapsibleState::None,
        "collapsed" => TreeCollapsibleState::Collapsed,
        "expanded" => TreeCollapsibleState::Expanded,
        _ => return Err(BoundaryError::Type("invalid tree collapsible state")),
    };
    let kind = match optional_text_property(scope, object, "kind")? {
        None => TreeItemKind::Item,
        Some(kind) => match kind.as_str() {
            "item" => TreeItemKind::Item,
            "group" => TreeItemKind::Group,
            _ => return Err(BoundaryError::Type("invalid tree item kind")),
        },
    };
    let icon = optional_text_property(scope, object, "icon")?
        .map(|icon| match icon.as_str() {
            "file" => Ok(TreeIcon::File),
            "folder" => Ok(TreeIcon::Folder),
            "symbol" => Ok(TreeIcon::Symbol),
            _ => Err(BoundaryError::Type("invalid tree icon")),
        })
        .transpose()?;
    Ok(TreeItem {
        id: string_property(scope, object, "id")?,
        label: string_property(scope, object, "label")?,
        description: optional_text_property(scope, object, "description")?,
        collapsible_state,
        icon,
        command: optional_text_property(scope, object, "command")?,
        kind,
        status_text: optional_text_property(scope, object, "statusText")?,
        badge: optional_text_property(scope, object, "badge")?,
        actions: tree_actions_from_v8(scope, object, "actions")?,
    })
}

pub(super) fn tree_actions_from_v8<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    object: v8::Local<'s, v8::Object>,
    name: &str,
) -> Result<Vec<TreeAction>, BoundaryError> {
    let value = property(scope, object, name, "invalid tree actions")?;
    if value.is_null_or_undefined() {
        return Ok(Vec::new());
    }
    let array = v8::Local::<v8::Array>::try_from(value)
        .map_err(|_| BoundaryError::Type("tree actions must be an array"))?;
    let mut actions = Vec::with_capacity(array.length() as usize);
    for index in 0..array.length() {
        let value = array.get_index(scope, index).ok_or("invalid tree action")?;
        actions.push(tree_action_from_v8(scope, value)?);
    }
    Ok(actions)
}

pub(super) fn tree_action_from_v8<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    value: v8::Local<'s, v8::Value>,
) -> Result<TreeAction, BoundaryError> {
    let object =
        v8::Local::<v8::Object>::try_from(value).map_err(|_| "tree action must be an object")?;
    let arguments = property(scope, object, "arguments", "invalid tree action arguments")?;
    let arguments = if arguments.is_null_or_undefined() {
        CommandArgumentValue::Null
    } else {
        command_argument_from_v8(scope, arguments)?
    };
    Ok(TreeAction {
        label: string_property(scope, object, "label")?,
        command: string_property(scope, object, "command")?,
        arguments,
    })
}

pub(super) fn tree_response_from_v8<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    request: TreeChildrenRequest,
    value: v8::Local<'s, v8::Value>,
) -> TreeChildrenResponse {
    TreeChildrenResponse {
        registration: request.registration,
        window: request.window,
        view: request.view,
        parent_id: request.parent_id,
        generation: request.generation,
        result: provider_items_from_v8(scope, value, tree_item_from_v8)
            .map_err(|message| TreeProviderError { message }),
    }
}

pub(super) fn completion_response_from_v8<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    request: CompletionRequest,
    value: v8::Local<'s, v8::Value>,
) -> CompletionResponse {
    CompletionResponse {
        registration: request.registration,
        revision: request.revision,
        generation: request.generation,
        result: provider_items_from_v8(scope, value, |scope, value| {
            let object = v8::Local::<v8::Object>::try_from(value)
                .map_err(|_| "completion item must be an object")?;
            Ok(CompletionResultItem {
                label: string_property(scope, object, "label")?,
                insert_text: string_property(scope, object, "insertText")?,
            })
        })
        .map_err(|message| CompletionProviderError { message }),
    }
}

pub(super) const MAX_JS_INTEGER: u64 = 9_007_199_254_740_991;

pub(super) fn exact_number<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    value: u64,
) -> Result<v8::Local<'s, v8::Value>, &'static str> {
    if value > MAX_JS_INTEGER {
        return Err("native integer cannot be represented exactly");
    }
    Ok(v8::Number::new(scope, value as f64).into())
}

pub(super) fn application_document_is_exact(info: &crate::host::protocol::DocumentInfo) -> bool {
    info.revision <= MAX_JS_INTEGER && info.buffer.value() <= MAX_JS_INTEGER
}
pub(super) fn application_window_is_exact(info: &crate::host::protocol::WindowInfo) -> bool {
    info.workspace.generation <= MAX_JS_INTEGER
        && info.panes.iter().flat_map(|pane| &pane.tabs).all(|tab| {
            tab.buffer
                .is_none_or(|buffer| buffer.value() <= MAX_JS_INTEGER)
        })
}
pub(super) fn application_event_to_v8<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    event: &ApplicationEvent,
) -> Result<v8::Local<'s, v8::Value>, &'static str> {
    let exact = event.sequence <= MAX_JS_INTEGER
        && match &event.change {
            ApplicationChange::DocumentOpened { document }
            | ApplicationChange::DocumentChanged { document } => {
                application_document_is_exact(document)
            }
            ApplicationChange::WindowOpened { window }
            | ApplicationChange::PresentationsChanged { window } => {
                application_window_is_exact(window)
            }
            ApplicationChange::WorkspaceChanged { workspace } => {
                workspace.generation <= MAX_JS_INTEGER
            }
            _ => true,
        };
    if !exact {
        return Err("native application integer cannot be represented exactly");
    }
    let json = serde_json::to_string(event).map_err(|_| "UnsupportedOperation")?;
    let json = v8::String::new(scope, &json).ok_or("UnsupportedOperation")?;
    v8::json::parse(scope, json).ok_or("UnsupportedOperation")
}

pub(super) fn host_failure_exception<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    error: &crate::host::declarations::HostFailure,
) -> v8::Local<'s, v8::Value> {
    if let Err(error) = crate::host::declarations::encode_json(&error.details) {
        return boundary_exception(scope, error.into());
    }

    let message = v8::String::new(scope, &error.message).unwrap();
    let exception = v8::Exception::error(scope, message);
    let object = v8::Local::<v8::Object>::try_from(exception).unwrap();
    let name = v8::String::new(scope, "KnotHostError").unwrap();
    let code = v8::String::new(scope, error.code).unwrap();
    set_property(scope, object, "name", name.into()).expect("error name");
    set_property(scope, object, "code", code.into()).expect("error code");
    let json = serde_json::to_string(&error.details).unwrap();
    let json = v8::String::new(scope, &json).unwrap();
    let details = v8::json::parse(scope, json).unwrap();
    set_property(scope, object, "details", details).expect("error details");
    exception
}

pub(super) fn declared_response_to_v8<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    response: &crate::host::declarations::DeclaredResponse,
) -> Result<v8::Local<'s, v8::Value>, v8::Local<'s, v8::Value>> {
    use crate::host::declarations::{EncodedValue, encode_json};
    let json_value = |scope: &mut v8::PinScope<'s, '_>, value: &serde_json::Value| {
        encode_json(value).map_err(|error| boundary_exception(scope, error.into()))?;
        let json = serde_json::to_string(value).expect("encoded host data");
        let json = v8::String::new(scope, &json).expect("host data string");
        v8::json::parse(scope, json).ok_or_else(|| {
            boundary_exception(
                scope,
                BoundaryError::Host(TransportError::UnsupportedOperation),
            )
        })
    };
    match &response.result {
        Ok(Ok(EncodedValue::Undefined)) => Ok(v8::undefined(scope).into()),
        Ok(Ok(EncodedValue::Snapshot(snapshot))) => snapshot_to_v8(scope, snapshot.clone())
            .map_err(|_| {
                boundary_exception(
                    scope,
                    BoundaryError::Host(TransportError::UnsupportedOperation),
                )
            }),
        Ok(Ok(EncodedValue::Json(value))) => json_value(scope, value),
        Ok(Err(error)) => Err(boundary_exception(scope, (*error).into())),
        Err(error) => Err(host_failure_exception(scope, error)),
    }
}

pub(super) fn set_number_property<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    object: v8::Local<'s, v8::Object>,
    name: &str,
    number: u64,
) -> Result<(), &'static str> {
    let number = exact_number(scope, number)?;
    set_property(scope, object, name, number)
}

pub(super) fn snapshot_to_v8<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    snapshot: TextSnapshot,
) -> Result<v8::Local<'s, v8::Value>, &'static str> {
    let value = v8::Object::new(scope);
    let text = snapshot_text_to_v8(scope, snapshot.text)?;
    set_property(scope, value, "text", text.into())?;

    let range = v8::Object::new(scope);
    set_number_property(
        scope,
        range,
        "startByteOffset",
        snapshot.range.start_byte_offset as u64,
    )?;
    set_number_property(
        scope,
        range,
        "endByteOffset",
        snapshot.range.end_byte_offset as u64,
    )?;
    set_property(scope, value, "range", range.into())?;
    set_number_property(scope, value, "revision", snapshot.revision)?;
    Ok(value.into())
}

pub(super) fn buffer_change_to_v8<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    change: BufferChange,
) -> Result<v8::Local<'s, v8::Value>, &'static str> {
    let value = v8::Object::new(scope);
    set_number_property(scope, value, "beforeRevision", change.before_revision)?;
    set_number_property(scope, value, "revision", change.revision)?;
    let edits = v8::Array::new(
        scope,
        i32::try_from(change.edits.len()).map_err(|_| "InvalidEditBatch")?,
    );
    for (index, edit) in change.edits.into_iter().enumerate() {
        let item = v8::Object::new(scope);
        let range = v8::Object::new(scope);
        set_number_property(
            scope,
            range,
            "startByteOffset",
            edit.range.start_byte_offset as u64,
        )?;
        set_number_property(
            scope,
            range,
            "endByteOffset",
            edit.range.end_byte_offset as u64,
        )?;
        set_property(scope, item, "range", range.into())?;
        let text = v8::String::new(scope, &edit.text).ok_or("InvalidEditBatch")?;
        set_property(scope, item, "text", text.into())?;
        if edits.set_index(
            scope,
            u32::try_from(index).map_err(|_| "InvalidEditBatch")?,
            item.into(),
        ) != Some(true)
        {
            return Err("InvalidEditBatch");
        }
    }
    set_property(scope, value, "edits", edits.into())?;
    Ok(value.into())
}

pub(super) fn snapshot_text_to_v8<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    text: SnapshotText,
) -> Result<v8::Local<'s, v8::String>, &'static str> {
    match text.0 {
        text if text.is_empty() => Ok(v8::String::empty(scope)),
        text => {
            let length = text.len();
            let raw = Arc::into_raw(text);
            let buffer = raw.cast::<u16>() as *mut u16;
            // SAFETY: `raw` transfers one Arc strong reference to V8. The
            // callback reconstructs that exact slice using V8's retained
            // length. If V8 rejects externalization, we reclaim it below.
            match unsafe {
                v8::String::new_external_twobyte_raw(scope, buffer, length, drop_external_utf16)
            } {
                Some(value) => Ok(value),
                None => {
                    // SAFETY: V8 did not accept ownership, so `raw` is still
                    // the unique transferred Arc reference.
                    drop(unsafe { Arc::<[u16]>::from_raw(raw) });
                    Err("UnsupportedOperation")
                }
            }
        }
    }
}

pub(super) fn set_property<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    object: v8::Local<'s, v8::Object>,
    name: &str,
    value: v8::Local<'s, v8::Value>,
) -> Result<(), &'static str> {
    let key = v8::String::new(scope, name).ok_or("UnsupportedOperation")?;
    match object.set(scope, key.into(), value) {
        Some(true) => Ok(()),
        _ => Err("UnsupportedOperation"),
    }
}
