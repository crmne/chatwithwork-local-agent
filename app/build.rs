//! Embeds the icon and version information Windows shows for the executable.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=assets/cww-app.ico");
        let mut resource = winresource::WindowsResource::new();
        resource
            .set_icon("assets/cww-app.ico")
            .set("ProductName", "Chat with Work")
            .set("FileDescription", "Chat with Work")
            .set("CompanyName", "PlentyLabs UG (haftungsbeschränkt) & Co. KG")
            .set("LegalCopyright", "© 2026 Carmine Paolino. MIT or Apache 2.0");
        if let Err(error) = resource.compile() {
            println!("cargo:warning=Windows resources not embedded: {error}");
        }
    }
}
