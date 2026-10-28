//! Module-graph compilation, resolution, and rollback inside an isolate.

use super::*;

#[cfg(test)]
pub(super) fn validate_root_module_specifier(specifier: &str) -> Result<String, RuntimeError> {
    let parsed = Url::parse(specifier).map_err(|_| RuntimeError {
        kind: RuntimeErrorKind::InvalidModuleSpecifier,
        message: format!("invalid fixture module specifier: {specifier}").into(),
        source: Some(specifier.into()),
        line: None,
        column: None,
        stack: None,
    })?;
    let canonical = parsed.to_string();
    if canonical == PRIVATE_BOOTSTRAP_SPECIFIER || canonical == PUBLIC_FACADE_SPECIFIER {
        return Err(RuntimeError {
            kind: RuntimeErrorKind::InvalidModuleSpecifier,
            message: format!("fixture module specifier is reserved: {canonical}").into(),
            source: Some(canonical.into()),
            line: None,
            column: None,
            stack: None,
        });
    }
    Ok(canonical)
}

pub(super) fn evaluate_fixture_module(
    scope: &mut v8::PinScope<'_, '_>,
    state: &Arc<RuntimeLocalState>,
    specifier: &str,
    source: Option<Arc<str>>,
    internal: bool,
) -> Result<String, RuntimeError> {
    if !internal && specifier == PRIVATE_BOOTSTRAP_SPECIFIER {
        return Err(module_resolution_error(
            specifier,
            "Knot private bootstrap bindings are not importable by extensions",
        ));
    }
    if let Some(source) = source {
        let mut modules = state.modules.lock().unwrap();
        if let Some(existing) = modules.sources.get(specifier) {
            if existing.as_ref() != source.as_ref() {
                return Err(module_resolution_error(
                    specifier,
                    "fixture module source cannot change after registration",
                ));
            }
        } else {
            modules.sources.insert(specifier.to_owned(), source);
        }
    }

    v8::tc_scope!(let try_catch, scope);
    let (module_count, resolution_count) = {
        let modules = state.modules.lock().unwrap();
        (modules.modules.len(), modules.resolutions.len())
    };
    let compiled = {
        let mut modules = state.modules.lock().unwrap();
        compile_module_graph(try_catch, &mut modules, specifier, None)
    };
    let compiled = match compiled {
        Ok(compiled) => compiled,
        Err(failure) => {
            let error = failure.report(try_catch, specifier);
            rollback_module_graph(state, module_count, resolution_count);
            return Err(error);
        }
    };
    let module = v8::Local::new(try_catch, &compiled);
    match module.instantiate_module(try_catch, resolve_module_callback) {
        Some(true) => {}
        Some(false) | None => {
            let error = runtime_error_from_try_catch(
                try_catch,
                RuntimeErrorKind::ModuleResolution,
                specifier,
            );
            rollback_module_graph(state, module_count, resolution_count);
            return Err(error);
        }
    }
    let Some(value) = module.evaluate(try_catch) else {
        return Err(runtime_error_from_try_catch(
            try_catch,
            RuntimeErrorKind::Exception,
            specifier,
        ));
    };
    let Ok(promise) = v8::Local::<v8::Promise>::try_from(value) else {
        return Err(RuntimeError::fatal(
            RuntimeErrorKind::Engine,
            "V8 module evaluation did not return a promise",
        ));
    };
    promise.mark_as_handled();
    try_catch.perform_microtask_checkpoint();
    match promise.state() {
        v8::PromiseState::Fulfilled => {
            let value = promise.result(try_catch).to_rust_string_lossy(try_catch);
            let mut rejections = state.rejections.lock().unwrap();
            if let Some(rejection) = rejections.first() {
                let error = RuntimeError {
                    kind: RuntimeErrorKind::Rejection,
                    message: rejection.message.clone().into(),
                    source: rejection.source.clone().map(Into::into),
                    line: rejection.line,
                    column: rejection.column,
                    stack: rejection.stack.clone().map(Into::into),
                };
                rejections.clear();
                Err(error)
            } else {
                Ok(value)
            }
        }
        v8::PromiseState::Rejected => {
            let rejection = promise.result(try_catch);
            let message = v8::Exception::create_message(try_catch, rejection);
            let error = RuntimeError {
                kind: RuntimeErrorKind::Rejection,
                message: message
                    .get(try_catch)
                    .to_rust_string_lossy(try_catch)
                    .into(),
                source: message
                    .get_script_resource_name(try_catch)
                    .and_then(|value| value.to_string(try_catch))
                    .map(|value| value.to_rust_string_lossy(try_catch).into())
                    .or_else(|| Some(specifier.into())),
                line: message
                    .get_line_number(try_catch)
                    .and_then(|line| u32::try_from(line).ok()),
                column: one_based_coordinate(message.get_start_column()),
                stack: None,
            };
            state.rejections.lock().unwrap().clear();
            Err(error)
        }
        v8::PromiseState::Pending => Err(RuntimeError {
            kind: RuntimeErrorKind::Rejection,
            message: "fixture module evaluation remained pending without host work".into(),
            source: Some(specifier.into()),
            line: None,
            column: None,
            stack: None,
        }),
    }
}

pub(super) fn rollback_module_graph(state: &RuntimeLocalState, modules: usize, resolutions: usize) {
    let mut registry = state.modules.lock().unwrap();
    registry.resolutions.truncate(resolutions);
    registry.modules.truncate(modules);
}

pub(super) fn clear_module_registry(state: &RuntimeLocalState) {
    let mut registry = state
        .modules
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    registry.resolutions.clear();
    registry.modules.clear();
    registry.sources.clear();
}

/// Check syntax and static imports without evaluating user code. The caller
/// supplies an immutable graph and runs this away from the foreground thread.
pub(crate) fn validate_module_graph_entry(
    graph: &ModuleGraph,
    entry: &str,
) -> Result<(), RuntimeError> {
    let entry = graph.entry_url(entry).ok_or_else(|| {
        RuntimeError::fatal(
            RuntimeErrorKind::InvalidModuleSpecifier,
            format!("module graph entry is unavailable: {entry}"),
        )
    })?;
    initialize();
    let mut isolate = v8::Isolate::new(Default::default());
    let scope = pin!(v8::HandleScope::new(&mut isolate));
    let mut scope = scope.init();
    let context = v8::Context::new(&scope, Default::default());
    let scope = &mut v8::ContextScope::new(&mut scope, context);
    v8::tc_scope!(let try_catch, scope);
    let mut modules = ModuleRegistry::new(Some(graph));
    let blocked = blocked_post_init_import(&modules, entry);
    compile_module_graph(try_catch, &mut modules, entry, blocked.as_deref())
        .map(|_| ())
        .map_err(|failure| failure.report(try_catch, entry))
}

pub(super) fn blocked_post_init_import(registry: &ModuleRegistry, entry: &str) -> Option<Arc<str>> {
    (registry
        .entries
        .get(ConfigPhase::PreInit.entry())
        .map(AsRef::as_ref)
        == Some(entry))
    .then(|| registry.entries.get(ConfigPhase::PostInit.entry()).cloned())
    .flatten()
}

pub(super) fn compile_module_graph(
    scope: &mut v8::PinScope<'_, '_>,
    registry: &mut ModuleRegistry,
    specifier: &str,
    blocked_import: Option<&str>,
) -> Result<v8::Global<v8::Module>, ModuleCompileFailure> {
    if let Some(record) = registry
        .modules
        .iter()
        .find(|record| record.specifier == specifier)
    {
        return Ok(record.module.clone());
    }
    let Some(source) = registry.sources.get(specifier).cloned() else {
        throw_module_error(
            scope,
            &if registry.package_root.is_some() {
                format!("Knot package module not found: {specifier}")
            } else {
                format!("Knot fixture module not found: {specifier}")
            },
        );
        return Err(RuntimeErrorKind::ModuleResolution.into());
    };
    let source_text = v8::String::new(scope, &source).ok_or(RuntimeErrorKind::Engine)?;
    let resource_name = v8::String::new(scope, specifier).ok_or(RuntimeErrorKind::Engine)?;
    let origin = v8::ScriptOrigin::new(
        scope,
        resource_name.into(),
        0,
        0,
        false,
        0,
        None,
        false,
        false,
        true,
        None,
    );
    let mut source = v8::script_compiler::Source::new(source_text, Some(&origin));
    let module = v8::script_compiler::compile_module(scope, &mut source)
        .ok_or(RuntimeErrorKind::Compilation)?;
    let global = v8::Global::new(scope, module);
    registry.modules.push(CompiledModule {
        specifier: specifier.to_owned(),
        module: global.clone(),
    });

    let requests = module.get_module_requests();
    for index in 0..requests.length() {
        let request = requests.get(scope, index).ok_or(RuntimeErrorKind::Engine)?;
        let request = v8::Local::<v8::ModuleRequest>::try_from(request)
            .map_err(|_| RuntimeErrorKind::Engine)?;
        let location = module.source_offset_to_location(request.get_source_offset());
        let request = request.get_specifier().to_rust_string_lossy(scope);
        let resolved =
            match resolve_module_specifier(&request, specifier, registry.package_root.as_deref()) {
                Ok(resolved) => resolved,
                Err(message) => {
                    throw_module_error(scope, &message);
                    return Err(ModuleCompileFailure::at_import(
                        RuntimeErrorKind::ModuleResolution,
                        specifier,
                        location,
                    ));
                }
            };
        if blocked_import == Some(resolved.as_str()) {
            throw_module_error(scope, "pre-init cannot import post-init.js");
            return Err(ModuleCompileFailure::at_import(
                RuntimeErrorKind::ModuleResolution,
                specifier,
                location,
            ));
        }
        if !registry.sources.contains_key(&resolved) {
            let kind = if registry.package_root.is_some() {
                "package"
            } else {
                "fixture"
            };
            throw_module_error(scope, &format!("Knot {kind} module not found: {resolved}"));
            return Err(ModuleCompileFailure::at_import(
                RuntimeErrorKind::ModuleResolution,
                specifier,
                location,
            ));
        }
        registry.resolutions.push(ModuleResolution {
            referrer: global.clone(),
            request,
            resolved: resolved.clone(),
        });
        compile_module_graph(scope, registry, &resolved, blocked_import)?;
    }
    Ok(global)
}

pub(super) fn resolve_module_specifier(
    request: &str,
    referrer: &str,
    package_root: Option<&str>,
) -> Result<String, String> {
    if request == PRIVATE_BOOTSTRAP_SPECIFIER && referrer != PUBLIC_FACADE_SPECIFIER {
        return Err("Knot private bootstrap bindings are not importable by extensions".into());
    }
    if request == PRIVATE_BOOTSTRAP_SPECIFIER || request == PUBLIC_FACADE_SPECIFIER {
        return Ok(request.to_owned());
    }
    if let Some(package_root) = package_root {
        if referrer == PUBLIC_FACADE_SPECIFIER
            || (!request.starts_with("./") && !request.starts_with("../"))
        {
            return Err(format!(
                "extension import must be relative or knot: {request}"
            ));
        }
        let resolved = Url::parse(referrer)
            .and_then(|base| base.join(request))
            .map_err(|_| format!("invalid extension import: {request}"))?;
        if !resolved.as_str().starts_with(package_root)
            || resolved.query().is_some()
            || resolved.fragment().is_some()
        {
            return Err(format!("extension import escapes package: {request}"));
        }
        return Ok(resolved.to_string());
    }
    if let Ok(absolute) = Url::parse(request) {
        return Ok(absolute.to_string());
    }
    if !request.starts_with("./") && !request.starts_with("../") && !request.starts_with('/') {
        return Err(format!("invalid fixture module specifier: {request}"));
    }
    Url::parse(referrer)
        .and_then(|base| base.join(request))
        .map(|url| url.to_string())
        .map_err(|_| format!("invalid fixture module specifier: {request}"))
}

pub(super) fn module_resolution_error(source: &str, message: &str) -> RuntimeError {
    RuntimeError {
        kind: RuntimeErrorKind::ModuleResolution,
        message: message.into(),
        source: Some(source.into()),
        line: None,
        column: None,
        stack: None,
    }
}

pub(super) fn throw_module_error(scope: &mut v8::PinScope<'_, '_>, message: &str) {
    if let Some(message) = v8::String::new(scope, message) {
        let exception = v8::Exception::type_error(scope, message);
        scope.throw_exception(exception);
    }
}

pub(super) fn reject_dynamic_import<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    _options: v8::Local<'s, v8::Data>,
    _resource_name: v8::Local<'s, v8::Value>,
    _specifier: v8::Local<'s, v8::String>,
    _attributes: v8::Local<'s, v8::FixedArray>,
) -> Option<v8::Local<'s, v8::Promise>> {
    let resolver = v8::PromiseResolver::new(scope)?;
    let message = v8::String::new(scope, "dynamic imports are unavailable to extensions")?;
    let error = v8::Exception::type_error(scope, message);
    resolver.reject(scope, error);
    Some(resolver.get_promise(scope))
}

pub(super) fn resolve_module_callback<'s>(
    context: v8::Local<'s, v8::Context>,
    specifier: v8::Local<'s, v8::String>,
    _import_attributes: v8::Local<'s, v8::FixedArray>,
    referrer: v8::Local<'s, v8::Module>,
) -> Option<v8::Local<'s, v8::Module>> {
    v8::callback_scope!(unsafe scope, context);
    let state = scope.get_slot::<Arc<RuntimeLocalState>>()?.clone();
    let request = specifier.to_rust_string_lossy(scope);
    let registry = state.modules.lock().ok()?;
    let resolution = registry.resolutions.iter().find(|resolution| {
        resolution.request == request && v8::Local::new(scope, &resolution.referrer) == referrer
    });
    let Some(resolution) = resolution else {
        throw_module_error(scope, &format!("unresolved module import: {request}"));
        return None;
    };
    registry
        .modules
        .iter()
        .find(|record| record.specifier == resolution.resolved)
        .map(|record| v8::Local::new(scope, &record.module))
}
