use proc_macro::TokenStream;
use quote::quote;
use syn::{DeriveInput, ItemFn, LitStr, parse_macro_input};

#[proc_macro_attribute]
pub fn host_operation(attribute: TokenStream, item: TokenStream) -> TokenStream {
    let name = parse_macro_input!(attribute as LitStr);
    let function = parse_macro_input!(item as ItemFn);
    expand_operation(name, function)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn expand_operation(name: LitStr, function: ItemFn) -> syn::Result<proc_macro2::TokenStream> {
    expand_handler(name, function, None)
}

#[proc_macro_attribute]
pub fn command(attribute: TokenStream, item: TokenStream) -> TokenStream {
    let attribute = proc_macro2::TokenStream::from(attribute);
    let mut function = parse_macro_input!(item as ItemFn);
    let mut declarations = vec![attribute];
    function.attrs.retain(|attr| {
        if attr.path().is_ident("command") {
            if let syn::Meta::List(meta) = &attr.meta {
                declarations.push(meta.tokens.clone());
            }
            false
        } else {
            true
        }
    });
    let result = (|| {
        let mut output = quote! { #function };
        for declaration in declarations {
            let options = syn::parse2::<CommandOptions>(declaration)?;
            output.extend(expand_handler(
                options.name.clone(),
                function.clone(),
                Some(options),
            )?);
        }
        Ok::<_, syn::Error>(output)
    })();
    result.unwrap_or_else(syn::Error::into_compile_error).into()
}

struct CommandOptions {
    name: LitStr,
    title: LitStr,
    id: syn::Ident,
    participant: syn::Ident,
    claim: Option<syn::Path>,
}

impl syn::parse::Parse for CommandOptions {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let name = input.parse()?;
        let mut title = None;
        let mut id = None;
        let mut participant = None;
        let mut claim = None;
        while !input.is_empty() {
            input.parse::<syn::Token![,]>()?;
            if input.is_empty() {
                break;
            }
            let key: syn::Ident = input.parse()?;
            input.parse::<syn::Token![=]>()?;
            match key.to_string().as_str() {
                "title" => title = Some(input.parse()?),
                "id" => id = Some(input.parse()?),
                "participant" => participant = Some(input.parse()?),
                "claim" => claim = Some(input.parse()?),
                _ => {
                    return Err(syn::Error::new_spanned(
                        key,
                        "expected title, id, participant, or claim",
                    ));
                }
            }
        }
        Ok(Self {
            name,
            title: title.ok_or_else(|| input.error("expected title"))?,
            id: id.ok_or_else(|| input.error("expected id"))?,
            participant: participant.ok_or_else(|| input.error("expected participant"))?,
            claim,
        })
    }
}

fn expand_handler(
    name: LitStr,
    function: ItemFn,
    command: Option<CommandOptions>,
) -> syn::Result<proc_macro2::TokenStream> {
    let signature = &function.sig;
    if signature.asyncness.is_none()
        || signature.inputs.len() != 2
        || !signature.generics.params.is_empty()
        || signature.unsafety.is_some()
    {
        return Err(syn::Error::new_spanned(
            signature,
            "handlers must be safe async functions with owned context and arguments, without generics",
        ));
    }
    let types = signature
        .inputs
        .iter()
        .map(|argument| match argument {
            syn::FnArg::Typed(argument) if !matches!(*argument.ty, syn::Type::Reference(_)) => {
                Ok(argument.ty.as_ref())
            }
            _ => Err(syn::Error::new_spanned(
                argument,
                "expected an owned argument",
            )),
        })
        .collect::<syn::Result<Vec<_>>>()?;
    let context = types[0];
    let arguments = types[1];
    let syn::ReturnType::Type(_, result) = &signature.output else {
        return Err(syn::Error::new_spanned(
            signature,
            "expected Result<Success, Error>",
        ));
    };
    let syn::Type::Path(result) = result.as_ref() else {
        return Err(syn::Error::new_spanned(
            result,
            "expected Result<Success, Error>",
        ));
    };
    let segment = result.path.segments.last().unwrap();
    let syn::PathArguments::AngleBracketed(parameters) = &segment.arguments else {
        return Err(syn::Error::new_spanned(
            result,
            "expected Result<Success, Error>",
        ));
    };
    if segment.ident != "Result" || parameters.args.len() != 2 {
        return Err(syn::Error::new_spanned(
            result,
            "expected Result<Success, Error>",
        ));
    }
    let success = &parameters.args[0];
    let error = &parameters.args[1];
    let ident = &signature.ident;
    let conditions = function
        .attrs
        .iter()
        .filter(|attribute| {
            attribute.path().is_ident("cfg") || attribute.path().is_ident("cfg_attr")
        })
        .collect::<Vec<_>>();
    let visibility = &function.vis;
    let (marker, definition, registration, handler_name) = if let Some(options) = command {
        let id = options.id;
        let title = options.title;
        let participant = options.participant;
        let marker = quote::format_ident!("__command_{}", id);
        let claim = if let Some(claim) = options.claim {
            quote! { #claim }
        } else {
            quote! { crate::app::command_declarations::always_claim }
        };
        (
            marker.clone(),
            quote! { #(#conditions)* #visibility const #id: &str = #name; },
            quote! {
                crate::app::command_declarations::CommandDescriptor::new::<#marker>(
                    #title, crate::app::command_declarations::Participant::#participant, #claim)
            },
            quote! { #id },
        )
    } else {
        (
            ident.clone(),
            quote! { #function },
            quote! {
                crate::host::declarations::OperationDescriptor::new::<#ident>()
            },
            quote! { #name },
        )
    };
    Ok(quote! {
        #definition
        #(#conditions)*
        #[allow(non_camel_case_types)]
        #visibility enum #marker {}
        #(#conditions)*
        impl crate::host::declarations::AsyncHandler for #marker {
            type Context = #context;
            type Arguments = #arguments;
            type Success = #success;
            type Error = #error;
            const NAME: &'static str = #handler_name;
            fn call(context: Self::Context, arguments: Self::Arguments)
                -> crate::host::declarations::HandlerFuture<Result<Self::Success, Self::Error>>
            {
                Box::pin(#ident(context, arguments))
            }
        }
        #(#conditions)*
        inventory::submit! {
            #registration
        }
    })
}

#[proc_macro_derive(HostError, attributes(error))]
pub fn host_error(item: TokenStream) -> TokenStream {
    expand_error(parse_macro_input!(item as DeriveInput))
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn expand_error(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            input.generics,
            "host errors cannot be generic",
        ));
    }
    let syn::Data::Enum(data) = input.data else {
        return Err(syn::Error::new_spanned(
            input.ident,
            "HostError requires an enum",
        ));
    };
    let mut arms = Vec::new();
    let mut codes = Vec::new();
    let mut code_arms = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for variant in data.variants {
        let mut code = None;
        let mut message = None;
        let mut transparent = false;
        for attribute in &variant.attrs {
            if attribute.path().is_ident("error") {
                attribute.parse_nested_meta(|meta| {
                    if meta.path.is_ident("code") {
                        code = Some(meta.value()?.parse::<LitStr>()?);
                    } else if meta.path.is_ident("message") {
                        message = Some(meta.value()?.parse::<LitStr>()?);
                    } else if meta.path.is_ident("transparent") {
                        transparent = true;
                    } else {
                        return Err(meta.error("expected code, message, or transparent"));
                    }
                    Ok(())
                })?;
            }
        }
        let ident = &variant.ident;
        if transparent {
            let syn::Fields::Unnamed(fields) = &variant.fields else {
                return Err(syn::Error::new_spanned(
                    &variant,
                    "transparent errors require one unnamed field",
                ));
            };
            if fields.unnamed.len() != 1 || code.is_some() || message.is_some() {
                return Err(syn::Error::new_spanned(
                    &variant,
                    "transparent errors require one field and no code or message",
                ));
            }
            let ty = &fields.unnamed[0].ty;
            code_arms.push(
                quote! { Self::#ident(error) => crate::host::declarations::HostError::code(error) },
            );
            arms.push(quote! { Self::#ident(error) => crate::host::declarations::HostError::into_failure(error) });
            codes.push(
                quote! { result.extend(<#ty as crate::host::declarations::HostError>::codes()); },
            );
            continue;
        }
        let code = code.ok_or_else(|| {
            syn::Error::new_spanned(&variant, "an explicit stable error code is required")
        })?;
        let message = message.ok_or_else(|| {
            syn::Error::new_spanned(&variant, "an explicit diagnostic message is required")
        })?;
        if code.value().is_empty() || !seen.insert(code.value()) {
            return Err(syn::Error::new_spanned(
                code,
                "error codes must be nonempty and unique",
            ));
        }
        let (pattern, details) = match &variant.fields {
            syn::Fields::Unit => (quote! { Self::#ident }, quote! { serde_json::Value::Null }),
            syn::Fields::Named(fields) => {
                let names = fields
                    .named
                    .iter()
                    .map(|field| field.ident.as_ref().unwrap())
                    .collect::<Vec<_>>();
                let keys = names
                    .iter()
                    .map(|name| name.to_string())
                    .collect::<Vec<_>>();
                (
                    quote! { Self::#ident { #(#names),* } },
                    quote! { serde_json::json!({ #(#keys: #names),* }) },
                )
            }
            _ => {
                return Err(syn::Error::new_spanned(
                    &variant,
                    "use named details or a transparent composed error",
                ));
            }
        };
        let code_pattern = match &variant.fields {
            syn::Fields::Unit => quote! { Self::#ident },
            _ => quote! { Self::#ident { .. } },
        };
        code_arms.push(quote! { #code_pattern => #code });
        let diagnostic = if message.value().contains('{') {
            quote! { format!(#message) }
        } else {
            quote! { #message.into() }
        };
        arms.push(
            quote! { #pattern => crate::host::declarations::HostFailure {
                code: #code, message: #diagnostic, details: #details,
            } },
        );
        codes.push(quote! { result.push(#code); });
    }
    let ident = input.ident;
    Ok(quote! {
        impl crate::host::declarations::HostError for #ident {
            fn code(&self) -> &'static str {
                match self { #(#code_arms),* }
            }
            fn into_failure(self) -> crate::host::declarations::HostFailure {
                match self { #(#arms),* }
            }
            fn codes() -> Vec<&'static str> {
                let mut result = Vec::new();
                #(#codes)*
                result.sort_unstable();
                result.dedup();
                result
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_handler_shapes() {
        for function in [
            syn::parse_quote!(
                fn handler(context: Context, args: Args) -> Result<(), Error> {}
            ),
            syn::parse_quote!(
                async fn handler(context: &Context, args: Args) -> Result<(), Error> {}
            ),
            syn::parse_quote!(
                async fn handler<T>(context: Context, args: T) -> Result<(), Error> {}
            ),
            syn::parse_quote!(
                async fn handler(context: Context) -> Result<(), Error> {}
            ),
            syn::parse_quote!(
                async fn handler(context: Context, args: Args) -> () {}
            ),
        ] {
            assert!(expand_operation(syn::parse_quote!("example"), function).is_err());
        }
    }

    #[test]
    fn command_declarations_require_metadata_and_share_signature_validation() {
        for options in [
            quote! { "example", id = EXAMPLE, participant = Editor },
            quote! { "example", title = "Example", participant = Editor },
            quote! { "example", title = "Example", id = EXAMPLE },
            quote! { "example", title = "Example", id = EXAMPLE, participant = Editor, unknown = value },
        ] {
            assert!(syn::parse2::<CommandOptions>(options).is_err());
        }
        let options = syn::parse2::<CommandOptions>(quote! {
            "example", title = "Example", id = EXAMPLE, participant = Editor
        })
        .unwrap();
        assert!(
            expand_handler(
                options.name.clone(),
                syn::parse_quote!(
                    fn example(context: Context, args: Args) -> Result<(), Error> {}
                ),
                Some(options)
            )
            .is_err()
        );
        let options = syn::parse2::<CommandOptions>(quote! {
            "example", title = "Example", id = EXAMPLE, participant = Editor, claim = claim_example
        })
        .unwrap();
        let expanded = expand_handler(
            options.name.clone(),
            syn::parse_quote!(
                async fn example(context: Context, args: Args) -> Result<(), Error> {}
            ),
            Some(options),
        )
        .unwrap()
        .to_string();
        assert!(expanded.contains("inventory :: submit !"));
        assert!(expanded.contains("AsyncHandler"));
        assert!(expanded.contains("CommandDescriptor :: new"));
        assert!(expanded.contains("claim_example"));
        assert!(expanded.contains("const EXAMPLE"));
    }

    #[test]
    fn rejects_errors_without_stable_codes_and_ambiguous_details() {
        for declaration in [
            syn::parse_quote!(
                enum Error {
                    Missing,
                }
            ),
            syn::parse_quote!(
                enum Error {
                    #[error(code = "Code")]
                    MissingMessage,
                }
            ),
            syn::parse_quote!(
                enum Error {
                    #[error(code = "Code", message = "message")]
                    Tuple(String),
                }
            ),
            syn::parse_quote!(
                enum Error {
                    #[error(transparent)]
                    Empty,
                }
            ),
            syn::parse_quote!(
                enum Error {
                    #[error(code = "Same", message = "first")]
                    First,
                    #[error(code = "Same", message = "second")]
                    Second,
                }
            ),
        ] {
            assert!(expand_error(declaration).is_err());
        }
    }
}
