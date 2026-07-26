//! Static validation for the WGSL this crate ships (#272).
//!
//! The viewport shaders are string constants that wgpu compiles at run time, so a malformed edit
//! passes `cargo test`, clippy and CI, and first appears as a broken viewport when the app runs.
//! Parsing and validating them with naga (the same front end wgpu uses) turns that into a test
//! failure naming the offending line. Test-only: nothing here runs in the application.

/// Parses and validates `src`, returning naga's own diagnostic as the error when it is malformed.
/// The caller names the shader, since several are checked together.
pub(crate) fn validate(src: &str) -> Result<(), String> {
    let module = naga::front::wgsl::parse_str(src)
        .map_err(|err| format!("does not parse\n{}", err.emit_to_string(src)))?;
    // Capabilities::all() is deliberate. The point is catching a shader that is malformed on its
    // own terms, not modelling one adapter's feature set; the device does that when it creates the
    // module for real, and a capability this crate does not use cannot be validated into existence.
    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    );
    validator
        .validate(&module)
        .map(|_| ())
        .map_err(|err| format!("does not validate\n{}", err.emit_to_string(src)))
}
