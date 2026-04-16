#[cfg(target_os = "emscripten")]
pub fn require_native(cmd: &str) -> anyhow::Result<()> {
    anyhow::bail!(
        "'path {}' requires a native environment (not available in this WebAssembly build)",
        cmd
    )
}
