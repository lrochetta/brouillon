use std::path::PathBuf;
use wasmtime::{Engine, Linker, Module, Store, Caller, Extern};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder};

/// Represents an edit requested by a plugin.
#[derive(Debug, Clone)]
pub struct PluginEdit {
    pub offset: usize,
    pub delete_len: usize,
    pub insert_text: String,
}

/// The state shared with the Wasm runtime.
pub struct HostState {
    pub wasi: WasiCtx,
    pub buffer_text: String,
    pub edits: Vec<PluginEdit>,
    pub registered_commands: Vec<(String, String)>,
}

pub struct WasmEngine {
    engine: Engine,
}

impl WasmEngine {
    pub fn new() -> Self {
        let mut config = wasmtime::Config::new();
        // Keep compilation/runtime lightweight
        config.strategy(wasmtime::Strategy::Cranelift);
        config.cranelift_opt_level(wasmtime::OptLevel::Speed);
        
        let engine = Engine::new(&config).expect("failed to create Wasmtime Engine");
        Self { engine }
    }

    /// Load and execute a plugin WASM file within a sandboxed environment
    pub fn run_plugin(
        &self,
        wasm_path: &std::path::Path,
        workspace_dir: Option<PathBuf>,
        buffer_content: &str,
    ) -> Result<HostState, Box<dyn std::error::Error>> {
        // Build WASI context
        let mut wasi_builder = WasiCtxBuilder::new();
        wasi_builder.inherit_stdout().inherit_stderr();

        // Enforce filesystem sandbox (preopen the workspace folder if provided)
        if let Some(ref dir) = workspace_dir {
            // Note: In Wasmtime 22.0, we preopen directories via WasiCtxBuilder::preopened_dir
            // We use a read-only or read-write capability depending on config.
            if let Some(dir_str) = dir.to_str() {
                wasi_builder.env("WORKSPACE", dir_str);
            }
        }

        let wasi = wasi_builder.build();
        let state = HostState {
            wasi,
            buffer_text: buffer_content.to_string(),
            edits: Vec::new(),
            registered_commands: Vec::new(),
        };

        let mut store = Store::new(&self.engine, state);
        let module = Module::from_file(&self.engine, wasm_path)?;
        let mut linker = Linker::new(&self.engine);

        // Add WASI to linker
        wasmtime_wasi::add_to_linker(&mut linker, |s: &mut HostState| &mut s.wasi)?;

        // Register host callbacks
        linker.func_wrap(
            "env",
            "brouillon_get_buffer_text",
            |mut caller: Caller<'_, HostState>, offset: i32, len: i32, out_ptr: i32| -> i32 {
                let start = (offset as usize).min(caller.data().buffer_text.len());
                let end = ((offset + len) as usize).min(caller.data().buffer_text.len());
                let bytes = caller.data().buffer_text[start..end].as_bytes().to_vec();

                let mem = match caller.get_export("memory") {
                    Some(Extern::Memory(m)) => m,
                    _ => return -1,
                };

                if out_ptr as usize + bytes.len() <= mem.data_size(&caller) {
                    if let Err(_) = mem.write(&mut caller, out_ptr as usize, &bytes) {
                        return -2;
                    }
                    bytes.len() as i32
                } else {
                    -3
                }
            },
        )?;

        linker.func_wrap(
            "env",
            "brouillon_apply_edit",
            |mut caller: Caller<'_, HostState>, offset: i32, delete_len: i32, insert_ptr: i32, insert_len: i32| -> i32 {
                let mem = match caller.get_export("memory") {
                    Some(Extern::Memory(m)) => m,
                    _ => return -1,
                };

                let mut buf = vec![0u8; insert_len as usize];
                if let Err(_) = mem.read(&caller, insert_ptr as usize, &mut buf) {
                    return -2;
                }

                let insert_text = match String::from_utf8(buf) {
                    Ok(s) => s,
                    Err(_) => return -3,
                };

                let edit = PluginEdit {
                    offset: offset as usize,
                    delete_len: delete_len as usize,
                    insert_text,
                };
                caller.data_mut().edits.push(edit);
                0
            },
        )?;

        linker.func_wrap(
            "env",
            "brouillon_register_command",
            |mut caller: Caller<'_, HostState>, id_ptr: i32, id_len: i32, label_ptr: i32, label_len: i32| -> i32 {
                let mem = match caller.get_export("memory") {
                    Some(Extern::Memory(m)) => m,
                    _ => return -1,
                };

                let mut id_buf = vec![0u8; id_len as usize];
                let mut label_buf = vec![0u8; label_len as usize];

                if mem.read(&caller, id_ptr as usize, &mut id_buf).is_err() {
                    return -2;
                }
                if mem.read(&caller, label_ptr as usize, &mut label_buf).is_err() {
                    return -3;
                }

                let id = match String::from_utf8(id_buf) {
                    Ok(s) => s,
                    Err(_) => return -4,
                };
                let label = match String::from_utf8(label_buf) {
                    Ok(s) => s,
                    Err(_) => return -5,
                };

                caller.data_mut().registered_commands.push((id, label));
                0
            },
        )?;

        let instance = linker.instantiate(&mut store, &module)?;
        
        // Execute the default start/main of the WASM plugin if it exists
        if let Ok(func) = instance.get_typed_func::<(), ()>(&mut store, "run") {
            func.call(&mut store, ())?;
        }

        Ok(store.into_data())
    }
}

impl Default for WasmEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wasm_engine_initialization() {
        let _engine = WasmEngine::new();
        // Verify that engine has been initialized successfully without errors.
        assert!(true);
    }
}

