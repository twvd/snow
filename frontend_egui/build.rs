use std::env;

fn main() {
    println!("cargo:rerun-if-changed=../.git/HEAD");

    if env::var_os("CARGO_CFG_WINDOWS").is_some() {
        winresource::WindowsResource::new()
            .set_icon("../docs/images/snow.ico")
            .compile()
            .expect("Failed to embed icon (Windows)");
    }
}
