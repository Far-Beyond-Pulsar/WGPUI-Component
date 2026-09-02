use proc_macro::TokenStream;
use quote::quote;
use syn::parse_macro_input;

mod derive_into_plot;

#[proc_macro_derive(IntoPlot)]
pub fn derive_into_plot(input: TokenStream) -> TokenStream {
    derive_into_plot::derive_into_plot(input)
}

/// Generate an `IconName` enum by scanning a directory of `.svg` files.
///
/// Accepts a path relative to the calling crate's `CARGO_MANIFEST_DIR`.
/// Each `.svg` file becomes a PascalCase variant.
///
/// ```ignore
/// generate_icon_enum!("../../assets/icons");
/// ```
#[proc_macro]
pub fn generate_icon_enum(input: TokenStream) -> TokenStream {
    let lit = parse_macro_input!(input as syn::LitStr);
    let icons_dir = std::path::Path::new(
        &std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set"),
    )
    .join(lit.value());

    let mut entries: Vec<(String, String)> = std::fs::read_dir(&icons_dir)
        .unwrap_or_else(|e| {
            panic!(
                "generate_icon_enum: cannot read '{}': {}",
                icons_dir.display(),
                e
            )
        })
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if name.ends_with(".svg") {
                Some((to_pascal(&name), format!("icons/{name}")))
            } else {
                None
            }
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let variants: Vec<proc_macro2::Ident> = entries
        .iter()
        .map(|(n, _)| proc_macro2::Ident::new(n, proc_macro2::Span::call_site()))
        .collect();
    let paths: Vec<&str> = entries.iter().map(|(_, p)| p.as_str()).collect();

    TokenStream::from(quote! {
        #[derive(IntoElement, Clone, Debug)]
        pub enum IconName { #(#variants,)* }
        impl IconName {
            pub fn path(self) -> SharedString {
                match self { #(Self::#variants => #paths,)* }.into()
            }
        }
    })
}

fn to_pascal(filename: &str) -> String {
    filename
        .strip_suffix(".svg")
        .unwrap_or(filename)
        .to_lowercase()
        .split('-')
        .map(|p| {
            let mut c = p.chars();
            c.next()
                .map(|f| f.to_uppercase().to_string() + c.as_str())
                .unwrap_or_default()
        })
        .collect()
}
