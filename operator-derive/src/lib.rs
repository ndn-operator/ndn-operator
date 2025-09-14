use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, DeriveInput, Data, Fields};

#[proc_macro_derive(Conditions)]
pub fn derive_conditions(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = input.ident;

    let mut has_conditions_field = false;
    if let Data::Struct(data_struct) = &input.data {
        if let Fields::Named(fields_named) = &data_struct.fields {
            for field in &fields_named.named {
                if let Some(ident) = &field.ident {
                    if ident == "conditions" {
                        has_conditions_field = true;
                        break;
                    }
                }
            }
        }
    }

    if !has_conditions_field {
        return syn::Error::new_spanned(
            name,
            "#[derive(Conditions)] requires a `conditions` field",
        )
        .to_compile_error()
        .into();
    }

    let expanded = quote! {
        impl crate::conditions::Conditions for #name {
            fn conditions(&self) -> &Option<Vec<k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition>> {
                &self.conditions
            }
            fn conditions_mut(&mut self) -> &mut Option<Vec<k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition>> {
                &mut self.conditions
            }
        }
    };

    TokenStream::from(expanded)
}
