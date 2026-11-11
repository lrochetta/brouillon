use wasmtime::Linker;

pub struct WasiCtx;

pub struct WasiCtxBuilder;

impl WasiCtxBuilder {
    pub fn new() -> Self {
        Self
    }
    pub fn inherit_stdout(&mut self) -> &mut Self {
        self
    }
    pub fn inherit_stderr(&mut self) -> &mut Self {
        self
    }
    pub fn env(&mut self, _key: &str, _value: &str) -> &mut Self {
        self
    }
    pub fn build(&mut self) -> WasiCtx {
        WasiCtx
    }
}

pub fn add_to_linker<T>(
    _linker: &mut Linker<T>,
    _get_wasi: impl Fn(&mut T) -> &mut WasiCtx,
) -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}
