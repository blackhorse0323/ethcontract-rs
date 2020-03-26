use crate::contract::{types, Context};
use crate::util;
use anyhow::{Context as _, Result};
use ethcontract_common::abi::{Function, Param};
use inflector::Inflector;
use proc_macro2::{Literal, TokenStream};
use quote::quote;
use syn::Ident as SynIdent;

pub(crate) fn expand(cx: &Context) -> Result<TokenStream> {
    let contract_name = &cx.contract_name;

    let functions = cx
        .artifact
        .abi
        .functions()
        .map(|function| {
            expand_function(&cx, function)
                .with_context(|| format!("error expanding function '{}'", function.name))
        })
        .collect::<Result<Vec<_>>>()?;

    if functions.is_empty() {
        return Ok(quote! {});
    }

    Ok(quote! {
        #[allow(dead_code)]
        #[allow(clippy::too_many_arguments, clippy::type_complexity)]
        impl #contract_name {
            #( #functions )*
        }
    })
}

fn expand_function(cx: &Context, function: &Function) -> Result<TokenStream> {
    let ethcontract = &cx.runtime_crate;

    let name = util::safe_ident(&function.name.to_snake_case());
    let name_str = Literal::string(&function.name);

    let signature = function_signature(&function);
    let doc_str = cx
        .artifact
        .devdoc
        .methods
        .get(&signature)
        .or_else(|| cx.artifact.userdoc.methods.get(&signature))
        .and_then(|entry| entry.details.as_ref())
        .map(String::as_str)
        .unwrap_or("Generated by `ethcontract`");
    let doc = util::expand_doc(doc_str);

    let input = expand_inputs(cx, &function.inputs)?;
    let outputs = expand_fn_outputs(cx, &function.outputs)?;
    let (method, result_type_name) = if function.constant {
        (quote! { view_method }, quote! { DynViewMethodBuilder })
    } else {
        (quote! { method }, quote! { DynMethodBuilder })
    };
    let result = quote! { #ethcontract::#result_type_name<#outputs> };
    let arg = expand_inputs_call_arg(&function.inputs);

    Ok(quote! {
        #doc
        pub fn #name(&self #input) -> #result {
            self.instance.#method(#name_str, #arg)
                .expect("generated call")
        }
    })
}

fn function_signature(function: &Function) -> String {
    let types = match function.inputs.len() {
        0 => String::new(),
        _ => {
            let mut params = function.inputs.iter().map(|param| &param.kind);
            let first = params.next().expect("at least one param").to_string();
            params.fold(first, |acc, param| format!("{},{}", acc, param))
        }
    };
    format!("{}({})", function.name, types)
}

pub(crate) fn expand_inputs(cx: &Context, inputs: &[Param]) -> Result<TokenStream> {
    let params = inputs
        .iter()
        .enumerate()
        .map(|(i, param)| {
            let name = expand_input_name(i, &param.name);
            let kind = types::expand(cx, &param.kind)?;
            Ok(quote! { #name: #kind })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(quote! { #( , #params )* })
}

fn input_name_to_ident(index: usize, name: &str) -> SynIdent {
    let name_str = match name {
        "" => format!("p{}", index),
        n => n.to_snake_case(),
    };
    util::safe_ident(&name_str)
}

fn expand_input_name(index: usize, name: &str) -> TokenStream {
    let name = input_name_to_ident(index, name);
    quote! { #name }
}

pub(crate) fn expand_inputs_call_arg(inputs: &[Param]) -> TokenStream {
    let names = inputs
        .iter()
        .enumerate()
        .map(|(i, param)| expand_input_name(i, &param.name));
    quote! { ( #( #names ,)* ) }
}

fn expand_fn_outputs(cx: &Context, outputs: &[Param]) -> Result<TokenStream> {
    match outputs.len() {
        0 => Ok(quote! { () }),
        1 => types::expand(cx, &outputs[0].kind),
        _ => {
            let types = outputs
                .iter()
                .map(|param| types::expand(cx, &param.kind))
                .collect::<Result<Vec<_>>>()?;
            Ok(quote! { (#( #types ),*) })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ethcontract_common::abi::ParamType;

    #[test]
    fn function_signature_empty() {
        assert_eq!(
            function_signature(&Function {
                name: String::new(),
                inputs: Vec::new(),
                outputs: Vec::new(),
                constant: false,
            }),
            "()"
        );
    }

    #[test]
    fn function_signature_normal() {
        assert_eq!(
            function_signature(&Function {
                name: "name".to_string(),
                inputs: vec![
                    Param {
                        name: "a".to_string(),
                        kind: ParamType::Address,
                    },
                    Param {
                        name: "b".to_string(),
                        kind: ParamType::Bytes,
                    },
                ],
                outputs: Vec::new(),
                constant: false
            }),
            "name(address,bytes)"
        );
    }

    #[test]
    fn input_name_to_ident_empty() {
        assert_eq!(input_name_to_ident(0, ""), util::ident("p0"));
    }

    #[test]
    fn input_name_to_ident_keyword() {
        assert_eq!(input_name_to_ident(0, "self"), util::ident("self_"));
    }

    #[test]
    fn input_name_to_ident_snake_case() {
        assert_eq!(
            input_name_to_ident(0, "CamelCase1"),
            util::ident("camel_case_1")
        );
    }

    #[test]
    fn expand_inputs_empty() {
        assert_quote!(
            expand_inputs(&Context::default(), &[]).unwrap().to_string(),
            {},
        );
    }

    #[test]
    fn expand_inputs_() {
        assert_quote!(
            expand_inputs(
                &Context::default(),
                &[
                    Param {
                        name: "a".to_string(),
                        kind: ParamType::Bool,
                    },
                    Param {
                        name: "b".to_string(),
                        kind: ParamType::Address,
                    },
                ],
            )
            .unwrap(),
            { , a: bool, b: ethcontract::Address },
        );
    }

    #[test]
    fn expand_fn_outputs_empty() {
        assert_quote!(expand_fn_outputs(&Context::default(), &[],).unwrap(), {
            ()
        });
    }

    #[test]
    fn expand_fn_outputs_single() {
        assert_quote!(
            expand_fn_outputs(
                &Context::default(),
                &[Param {
                    name: "a".to_string(),
                    kind: ParamType::Bool,
                },],
            )
            .unwrap(),
            { bool },
        );
    }

    #[test]
    fn expand_fn_outputs_muliple() {
        assert_quote!(
            expand_fn_outputs(
                &Context::default(),
                &[
                    Param {
                        name: "a".to_string(),
                        kind: ParamType::Bool,
                    },
                    Param {
                        name: "b".to_string(),
                        kind: ParamType::Address,
                    },
                ],
            )
            .unwrap(),
            { (bool, ethcontract::Address) },
        );
    }
}
