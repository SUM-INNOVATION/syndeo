//! Tool execution, inside `wasmtime`.
//!
//! A tool is a WebAssembly module. It is loaded with **no imports at all** —
//! no WASI, no filesystem, no clock, no randomness, no sockets — so the set of
//! things it can reach is exactly the set the host passes it, which is one
//! buffer in and one buffer out. That is what "a tool cannot reach anything the
//! host did not explicitly grant" means when the grant is empty.
//!
//! Three other things bound it, because a module with no imports can still spin
//! or allocate: fuel bounds how long it runs, a memory limit bounds how much it
//! takes, and both are checked by the engine rather than by us.
//!
//! # The interface a tool implements
//!
//! ```text
//! memory:                  exported, named "memory"
//! alloc(len: i32) -> i32   the host writes the input here
//! run(ptr: i32, len: i32) -> i64
//! ```
//!
//! `run` returns the output pointer in the high 32 bits and its length in the
//! low 32. Both buffers are UTF-8 JSON by convention; the host does not care.

use anyhow::{bail, Context, Result};

/// wasmtime has its own error type. This is where it becomes ours, so the rest
/// of the module reads like ordinary Rust.
fn wasm<T>(result: std::result::Result<T, wasmtime::Error>) -> Result<T> {
    result.map_err(|err| anyhow::anyhow!("{err:?}"))
}
use std::path::{Path, PathBuf};
use wasmtime::{Config, Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder};

/// How much work one tool invocation may do.
///
/// Fuel is deterministic — the same input costs the same fuel on every machine —
/// which is why it is the bound rather than a wall clock. A million units is
/// enough for text processing and nowhere near enough to sit in a loop.
pub const FUEL: u64 = 200_000_000;

/// How much memory one tool may have. Sixteen megabytes is generous for a text
/// transform and small enough that a dozen of them do not matter.
pub const MEMORY_LIMIT: usize = 16 * 1024 * 1024;

/// Largest input or output buffer. A tool that wants to return more than this is
/// not returning an answer.
pub const MAX_BUFFER: usize = 8 * 1024 * 1024;

/// A tool that has been read but not yet run.
pub struct Tool {
    pub name: String,
    module: Module,
    engine: Engine,
}

struct Host {
    limits: StoreLimits,
}

impl std::fmt::Debug for Tool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Tool({})", self.name)
    }
}

/// Extensions a tool may be written in.
///
/// `.wat` is accepted as well as `.wasm` because a tool runs inside this
/// process, and something that can be reviewed by eye is a better thing to run
/// than something that cannot. The engine treats them identically.
pub const EXTENSIONS: &[&str] = &["wasm", "wat"];

impl Tool {
    /// Compile a tool from a `.wasm` or `.wat` file.
    pub fn load(path: &Path) -> Result<Tool> {
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "tool".to_string());
        let bytes = std::fs::read(path)
            .with_context(|| format!("reading the tool at {}", path.display()))?;
        Tool::from_bytes(name, &bytes)
    }

    pub fn from_bytes(name: impl Into<String>, bytes: &[u8]) -> Result<Tool> {
        let mut config = Config::new();
        // Fuel is what stops a tool that decides not to return.
        config.consume_fuel(true);
        // Nothing here needs threads, and the `threads` feature is not enabled,
        // so shared memory between two tools is not reachable in the first place.

        let engine = wasm(Engine::new(&config)).context("building the wasm engine")?;
        let module = wasm(Module::new(&engine, bytes)).context("compiling the tool")?;

        // The check that makes the sandbox claim true: a module that imports
        // anything is refused, because the host has nothing to give it.
        let imports: Vec<String> = module
            .imports()
            .map(|import| format!("{}::{}", import.module(), import.name()))
            .collect();
        if !imports.is_empty() {
            bail!(
                "this tool asks the host for {}; tools are given nothing but their input",
                imports.join(", ")
            );
        }

        Ok(Tool {
            name: name.into(),
            module,
            engine,
        })
    }

    /// Run the tool over one input.
    ///
    /// Every failure mode is a returned error rather than a panic or a hang: a
    /// trap, running out of fuel, running out of memory, returning a pointer
    /// outside its own memory, or returning more than [`MAX_BUFFER`].
    pub fn run(&self, input: &[u8]) -> Result<Vec<u8>> {
        if input.len() > MAX_BUFFER {
            bail!("the input is {} bytes, past the {MAX_BUFFER} ceiling", input.len());
        }

        let host = Host {
            limits: StoreLimitsBuilder::new()
                .memory_size(MEMORY_LIMIT)
                .instances(1)
                .memories(1)
                .tables(1)
                .build(),
        };
        let mut store = Store::new(&self.engine, host);
        store.limiter(|host| &mut host.limits);
        wasm(store.set_fuel(FUEL)).context("setting the fuel for this run")?;

        // An empty linker. This is the whole security argument: there is nothing
        // in it, so there is nothing for the tool to call.
        let linker: Linker<Host> = Linker::new(&self.engine);
        let instance = wasm(linker.instantiate(&mut store, &self.module))
            .context("instantiating the tool")?;

        let memory = instance
            .get_memory(&mut store, "memory")
            .context("the tool exports no memory")?;
        let alloc = wasm(instance.get_typed_func::<i32, i32>(&mut store, "alloc"))
            .context("the tool exports no `alloc`")?;
        let run = wasm(instance.get_typed_func::<(i32, i32), i64>(&mut store, "run"))
            .context("the tool exports no `run`")?;

        let pointer = wasm(alloc.call(&mut store, input.len() as i32))
            .context("the tool refused to allocate for its input")?;
        write_into(&mut store, &memory, pointer, input)?;

        let packed = wasm(run.call(&mut store, (pointer, input.len() as i32)))
            .context("the tool trapped, ran out of fuel, or ran out of memory")?;

        let out_pointer = (packed >> 32) as u32 as usize;
        let out_len = (packed & 0xffff_ffff) as u32 as usize;
        if out_len > MAX_BUFFER {
            bail!("the tool returned {out_len} bytes, past the {MAX_BUFFER} ceiling");
        }
        read_from(&store, &memory, out_pointer, out_len)
    }

}

fn write_into(
    store: &mut Store<Host>,
    memory: &wasmtime::Memory,
    pointer: i32,
    bytes: &[u8],
) -> Result<()> {
    let start = usize::try_from(pointer).context("the tool returned a negative pointer")?;
    let data = memory.data_mut(store);
    let end = start
        .checked_add(bytes.len())
        .context("the tool's buffer overflows its own address space")?;
    if end > data.len() {
        bail!("the tool asked us to write outside its memory");
    }
    data[start..end].copy_from_slice(bytes);
    Ok(())
}

fn read_from(
    store: &Store<Host>,
    memory: &wasmtime::Memory,
    start: usize,
    len: usize,
) -> Result<Vec<u8>> {
    let data = memory.data(store);
    let end = start
        .checked_add(len)
        .context("the tool returned a buffer that overflows its own address space")?;
    if end > data.len() {
        bail!("the tool returned a pointer outside its memory");
    }
    Ok(data[start..end].to_vec())
}

/// Every tool in a directory. Missing directory means no tools, not an error.
pub fn discover(directory: &Path) -> Vec<(PathBuf, Result<Tool>)> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let extension = path.extension().and_then(|e| e.to_str()).unwrap_or_default();
        if EXTENSIONS.contains(&extension) {
            out.push((path.clone(), Tool::load(&path)));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tool that returns its input reversed. Hand-written WAT so the test does
    /// not need a toolchain to build a fixture.
    const REVERSE: &str = r#"
        (module
          (memory (export "memory") 1)
          (global $next (mut i32) (i32.const 1024))
          (func (export "alloc") (param $len i32) (result i32)
            (local $at i32)
            (local.set $at (global.get $next))
            (global.set $next (i32.add (global.get $next) (local.get $len)))
            (local.get $at))
          (func (export "run") (param $ptr i32) (param $len i32) (result i64)
            (local $out i32) (local $i i32)
            (local.set $out (call 0 (local.get $len)))
            (local.set $i (i32.const 0))
            (block $done
              (loop $next
                (br_if $done (i32.ge_s (local.get $i) (local.get $len)))
                (i32.store8
                  (i32.add (local.get $out) (local.get $i))
                  (i32.load8_u
                    (i32.add (local.get $ptr)
                             (i32.sub (i32.sub (local.get $len) (i32.const 1)) (local.get $i)))))
                (local.set $i (i32.add (local.get $i) (i32.const 1)))
                (br $next)))
            (i64.or
              (i64.shl (i64.extend_i32_u (local.get $out)) (i64.const 32))
              (i64.extend_i32_u (local.get $len)))))
    "#;

    fn tool(wat: &str) -> Result<Tool> {
        Tool::from_bytes("test", wat.as_bytes())
    }

    #[test]
    fn a_tool_transforms_its_input_and_returns_it() {
        let tool = tool(REVERSE).unwrap();
        assert_eq!(tool.run(b"abcdef").unwrap(), b"fedcba".to_vec());
        assert_eq!(tool.run(b"").unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn a_tool_that_asks_the_host_for_anything_is_refused() {
        // The import here is innocuous. The point is that *any* import is
        // refused, so a tool cannot acquire a capability by asking for one.
        let importer = r#"
            (module
              (import "env" "read_file" (func $read (param i32 i32) (result i32)))
              (memory (export "memory") 1)
              (func (export "alloc") (param i32) (result i32) (i32.const 0))
              (func (export "run") (param i32) (param i32) (result i64) (i64.const 0)))
        "#;
        let err = tool(importer).unwrap_err().to_string();
        assert!(
            err.contains("env::read_file"),
            "the refusal should name what was asked for: {err}"
        );
    }

    #[test]
    fn a_tool_that_will_not_stop_runs_out_of_fuel() {
        let spinner = r#"
            (module
              (memory (export "memory") 1)
              (func (export "alloc") (param i32) (result i32) (i32.const 1024))
              (func (export "run") (param i32) (param i32) (result i64)
                (loop $forever (br $forever))
                (i64.const 0)))
        "#;
        let started = std::time::Instant::now();
        let err = tool(spinner).unwrap().run(b"x").unwrap_err();
        assert!(
            started.elapsed() < std::time::Duration::from_secs(30),
            "the fuel bound did not stop it"
        );
        assert!(
            format!("{err:#}").contains("fuel") || format!("{err:#}").contains("trapped"),
            "unexpected failure: {err:#}"
        );
    }

    #[test]
    fn a_tool_that_points_outside_its_memory_gets_nothing() {
        let liar = r#"
            (module
              (memory (export "memory") 1)
              (func (export "alloc") (param i32) (result i32) (i32.const 1024))
              (func (export "run") (param i32) (param i32) (result i64)
                (i64.or
                  (i64.shl (i64.const 0x7fff0000) (i64.const 32))
                  (i64.const 4096))))
        "#;
        let err = tool(liar).unwrap().run(b"x").unwrap_err().to_string();
        assert!(err.contains("outside its memory"), "{err}");
    }

    #[test]
    fn a_tool_that_returns_more_than_the_ceiling_is_refused() {
        let greedy = format!(
            r#"
            (module
              (memory (export "memory") 1)
              (func (export "alloc") (param i32) (result i32) (i32.const 1024))
              (func (export "run") (param i32) (param i32) (result i64)
                (i64.or
                  (i64.shl (i64.const 1024) (i64.const 32))
                  (i64.const {}))))
        "#,
            MAX_BUFFER + 1
        );
        let err = tool(&greedy).unwrap().run(b"x").unwrap_err().to_string();
        assert!(err.contains("ceiling"), "{err}");
    }

    #[test]
    fn a_tool_that_is_not_a_tool_fails_at_load_rather_than_at_run() {
        assert!(Tool::from_bytes("junk", b"not wasm at all").is_err());

        let no_run = r#"(module (memory (export "memory") 1))"#;
        let err = tool(no_run).unwrap().run(b"x").unwrap_err().to_string();
        assert!(err.contains("alloc") || err.contains("run"), "{err}");
    }
}
