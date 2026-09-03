fn main() {
    let config = slint_build::CompilerConfiguration::new().with_style("fluent".into());
    if let Err(err) = slint_build::compile_with_config("ui/app.slint", config) {
        panic!("failed to compile Slint UI: {err}");
    }
}
