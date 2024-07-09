use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::{
    parse_quote, punctuated::Punctuated, token::Comma, Data, DataStruct, DeriveInput, Expr, Field,
    Fields, FieldsNamed, FieldsUnnamed, Lifetime, Stmt,
};

use super::{
    attributes::{parse_child_attributes, parse_container_attributes},
    rename_all,
};

pub fn expand_derive_from_row(input: &DeriveInput) -> syn::Result<TokenStream> {
    match &input.data {
        Data::Struct(DataStruct {
            fields: Fields::Named(FieldsNamed { named, .. }),
            ..
        }) => expand_derive_from_row_struct(input, named),

        Data::Struct(DataStruct {
            fields: Fields::Unnamed(FieldsUnnamed { unnamed, .. }),
            ..
        }) => expand_derive_from_row_struct_unnamed(input, unnamed),

        Data::Struct(DataStruct {
            fields: Fields::Unit,
            ..
        }) => Err(syn::Error::new_spanned(
            input,
            "unit structs are not supported",
        )),

        Data::Enum(_) => Err(syn::Error::new_spanned(input, "enums are not supported")),

        Data::Union(_) => Err(syn::Error::new_spanned(input, "unions are not supported")),
    }
}

fn expand_derive_from_row_struct(
    input: &DeriveInput,
    fields: &Punctuated<Field, Comma>,
) -> syn::Result<TokenStream> {
    let ident = &input.ident;

    let generics = &input.generics;

    let (lifetime, provided) = generics
        .lifetimes()
        .next()
        .map(|def| (def.lifetime.clone(), false))
        .unwrap_or_else(|| (Lifetime::new("'a", Span::call_site()), true));

    let (_, ty_generics, _) = generics.split_for_impl();

    let mut generics = generics.clone();
    generics
        .params
        .insert(0, parse_quote!(R: ::sqlx_oldapi::Row));

    if provided {
        generics.params.insert(0, parse_quote!(#lifetime));
    }

    let predicates = &mut generics.make_where_clause().predicates;

    predicates.push(parse_quote!(&#lifetime ::std::primitive::str: ::sqlx_oldapi::ColumnIndex<R>));

    let container_attributes = parse_container_attributes(&input.attrs)?;

    let reads: Vec<Stmt> = fields
        .iter()
        .filter_map(|field| -> Option<Stmt> {
            let id = &field.ident.as_ref()?;
            let attributes = parse_child_attributes(&field.attrs).unwrap();
            let ty = &field.ty;

            let expr: Expr = match (attributes.flatten, attributes.try_from) {
                (true, None) => {
                    predicates.push(parse_quote!(#ty: ::sqlx_oldapi::FromRow<#lifetime, R>));
                    parse_quote!(<#ty as ::sqlx_oldapi::FromRow<#lifetime, R>>::from_row(row))
                }
                (false, None) => {
                    predicates
                        .push(parse_quote!(#ty: ::sqlx_oldapi::decode::Decode<#lifetime, R::Database>));
                    predicates.push(parse_quote!(#ty: ::sqlx_oldapi::types::Type<R::Database>));

                    // Change from https://github.com/launchbadge/sqlx/issues/2896. They only
                    // mention this one....
                    let id_s = if let Some(s) = attributes.rename {
                        s
                    } else {
                        let s = id.to_string().trim_start_matches("r#").to_owned();
                        match container_attributes.rename_all {
                             Some(pattern) => rename_all(&s, pattern),
                             None => s,
                        }
                    };
                    parse_quote!(row.try_get(#id_s))
                }
                (true,Some(try_from)) => {
                    predicates.push(parse_quote!(#try_from: ::sqlx_oldapi::FromRow<#lifetime, R>));
                    parse_quote!(<#try_from as ::sqlx_oldapi::FromRow<#lifetime, R>>::from_row(row).and_then(|v| <#ty as ::std::convert::TryFrom::<#try_from>>::try_from(v).map_err(|e| ::sqlx_oldapi::Error::ColumnNotFound("FromRow: try_from failed".to_string())))) 
                }
                (false,Some(try_from)) => {
                    let predicate = parse_quote!(#try_from: ::sqlx_oldapi::decode::Decode<#lifetime, R::Database>);
                    predicates.push(predicate);
                    let predicate2 = parse_quote!(#try_from: ::sqlx_oldapi::types::Type<R::Database>);
                    predicates.push(predicate2);

                    // .. But this seems the same, so changed here too.. left original commented
                    // below for when I have a chance to check it out.
                    let id_s = if let Some(s) = attributes.rename {
                        s
                    } else {
                        let s = id.to_string().trim_start_matches("r#").to_owned();
                        match container_attributes.rename_all {
                             Some(pattern) => rename_all(&s, pattern),
                             None => s,
                        }
                    };
                    // let id_s = attributes
                    //     .rename
                    //     .or_else(|| Some(id.to_string().trim_start_matches("r#").to_owned()))
                    //     .map(|s| match container_attributes.rename_all {
                    //         Some(pattern) => rename_all(&s, pattern),
                    //         None => s,
                    //     })
                    //     .unwrap();
                    parse_quote!(row.try_get(#id_s).and_then(|v| <#ty as ::std::convert::TryFrom::<#try_from>>::try_from(v).map_err(|e| ::sqlx_oldapi::Error::ColumnNotFound("FromRow: try_from failed".to_string()))))
                }
            };

            if attributes.default {
                Some(parse_quote!(let #id: #ty = #expr.or_else(|e| match e {
                ::sqlx_oldapi::Error::ColumnNotFound(_) => {
                    ::std::result::Result::Ok(Default::default())
                },
                e => ::std::result::Result::Err(e)
            })?;))
            } else {
                Some(parse_quote!(
                    let #id: #ty = #expr?;
                ))
            }
        })
        .collect();

    let (impl_generics, _, where_clause) = generics.split_for_impl();

    let names = fields.iter().map(|field| &field.ident);

    Ok(quote!(
        #[automatically_derived]
        impl #impl_generics ::sqlx_oldapi::FromRow<#lifetime, R> for #ident #ty_generics #where_clause {
            fn from_row(row: &#lifetime R) -> ::sqlx_oldapi::Result<Self> {
                #(#reads)*

                ::std::result::Result::Ok(#ident {
                    #(#names),*
                })
            }
        }
    ))
}

fn expand_derive_from_row_struct_unnamed(
    input: &DeriveInput,
    fields: &Punctuated<Field, Comma>,
) -> syn::Result<TokenStream> {
    let ident = &input.ident;

    let generics = &input.generics;

    let (lifetime, provided) = generics
        .lifetimes()
        .next()
        .map(|def| (def.lifetime.clone(), false))
        .unwrap_or_else(|| (Lifetime::new("'a", Span::call_site()), true));

    let (_, ty_generics, _) = generics.split_for_impl();

    let mut generics = generics.clone();
    generics
        .params
        .insert(0, parse_quote!(R: ::sqlx_oldapi::Row));

    if provided {
        generics.params.insert(0, parse_quote!(#lifetime));
    }

    let predicates = &mut generics.make_where_clause().predicates;

    predicates.push(parse_quote!(
        ::std::primitive::usize: ::sqlx_oldapi::ColumnIndex<R>
    ));

    for field in fields {
        let ty = &field.ty;

        predicates.push(parse_quote!(#ty: ::sqlx_oldapi::decode::Decode<#lifetime, R::Database>));
        predicates.push(parse_quote!(#ty: ::sqlx_oldapi::types::Type<R::Database>));
    }

    let (impl_generics, _, where_clause) = generics.split_for_impl();

    let gets = fields
        .iter()
        .enumerate()
        .map(|(idx, _)| quote!(row.try_get(#idx)?));

    Ok(quote!(
        #[automatically_derived]
        impl #impl_generics ::sqlx_oldapi::FromRow<#lifetime, R> for #ident #ty_generics #where_clause {
            fn from_row(row: &#lifetime R) -> ::sqlx_oldapi::Result<Self> {
                ::std::result::Result::Ok(#ident (
                    #(#gets),*
                ))
            }
        }
    ))
}
